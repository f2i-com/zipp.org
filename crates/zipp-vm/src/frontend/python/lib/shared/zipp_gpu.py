"""Bounded float32 compute graphs, executed by the host's GPU runtime.

Arithmetic on Tensor objects records nodes; nothing runs until a program is
submitted. A program is plain data (`Graph.program`), executed by the host
through WebGPU, WebGL2, compiled WebAssembly or a JavaScript reference
implementation, and only the named outputs come back.

This module is bundled with the Zipp Python frontend (import it as
`zipp_gpu`) and also runs under CPython, where `submit` evaluates the graph
with the float32 reference implementation below. It is a graph builder, not
a NumPy replacement: shapes are rank 0..4, values are float32, and the
operation set is fixed (see `Tensor` and the optimizer steps on `Graph`).

Elementwise arithmetic broadcasts like NumPy. Beyond it: exp, log, sqrt,
tanh, sigmoid, relu and GELU (the exact-erf form, with one shared erf
approximation on every backend); sum/mean over an axis or the whole tensor;
softmax and log_softmax over the last axis; matmul for matrices and batches;
reshape/permute; fused cross-entropy with its gradient; and SGD, momentum and
Adam update steps, so a whole training step can run in one graph.
"""
import math
import struct

try:
    import _zipp_gpu
except ImportError:
    _zipp_gpu = None
try:
    # Zipp's tensor kernels: float32 storage moves through a graph (and to
    # and from the host) as typed arrays, and a graph evaluated without a
    # host runs on them. CPython uses the lists and the reference below.
    import _zipp_tensor as _k
except ImportError:
    _k = None

__all__ = ["Graph", "Tensor", "Session", "GraphError", "ComputeError", "execute_locally"]


class GraphError(ValueError):
    """A graph was built or requested incorrectly."""


class ComputeError(RuntimeError):
    """The host refused or failed to execute a program.

    `code` is the host's short reason (`SHAPE`, `LIMIT`, `DENIED`,
    `UNAVAILABLE`, `DEVICE_LOST`, `SHADER`, ...).
    """

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code

    def __str__(self):
        return "%s: %s" % (self.code, self.args[0])


def _f32(value):
    try:
        return struct.unpack("<f", struct.pack("<f", value))[0]
    except (OverflowError, struct.error):
        return math.inf if value > 0 else -math.inf


def _number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise GraphError("Tensor values must be numbers, not bools or objects")
    value = _f32(value)
    if not math.isfinite(value):
        raise GraphError("Only finite float32 values are supported")
    return value


def _shape(shape):
    if isinstance(shape, int) and not isinstance(shape, bool):
        shape = (shape,)
    if not isinstance(shape, (tuple, list)) or len(shape) > 4:
        raise GraphError("Shapes have at most four dimensions")
    if any(type(d) is not int or not 0 < d <= 65536 for d in shape):
        raise GraphError("Dimensions must be integers in 1..65536")
    result = tuple(shape)
    if _size(result) > 4194304:
        raise GraphError("Tensor exceeds 4,194,304 elements")
    return result


def _size(shape):
    result = 1
    for d in shape:
        result *= d
    return result


# The ten operations the first protocol version defined; see `Graph.program`.
_VERSION_ONE_OPS = frozenset((
    "input", "full", "add", "sub", "mul", "relu", "positive", "transpose", "matmul", "sum", "life"))


def _axis(axis, rank):
    if type(axis) is not int or not -max(rank, 1) <= axis < max(rank, 1):
        raise GraphError("Axis %r is out of range for rank %d" % (axis, rank))
    return axis + rank if axis < 0 else axis


def _broadcast(a, b):
    """NumPy broadcasting of two shapes, right-aligned."""
    rank = max(len(a), len(b))
    pa = (1,) * (rank - len(a)) + tuple(a)
    pb = (1,) * (rank - len(b)) + tuple(b)
    if any(x != y and x != 1 and y != 1 for x, y in zip(pa, pb)):
        raise GraphError("Shapes %r and %r cannot be broadcast" % (tuple(a), tuple(b)))
    return tuple(max(x, y) for x, y in zip(pa, pb))


def _flatten(data):
    if not isinstance(data, (tuple, list)):
        return [_number(data)], ()
    if not data:
        raise GraphError("Empty tensors are not supported")
    if isinstance(data[0], (tuple, list)):
        width = len(data[0])
        if not width or any(not isinstance(row, (tuple, list)) or len(row) != width for row in data):
            raise GraphError("Ragged matrices are not supported")
        return [_number(v) for row in data for v in row], (len(data), width)
    return [_number(v) for v in data], (len(data),)


class Tensor:
    """A symbolic handle owned by exactly one Graph, not a GPU allocation."""

    def __init__(self, graph, node_id, shape):
        self._graph = graph
        self._id = node_id
        self.shape = tuple(shape)
        self.dtype = "float32"

    def __add__(self, other):
        return self._graph._binary("add", self, other)

    def __radd__(self, other):
        return self._graph._binary("add", other, self)

    def __sub__(self, other):
        return self._graph._binary("sub", self, other)

    def __rsub__(self, other):
        return self._graph._binary("sub", other, self)

    def __mul__(self, other):
        return self._graph._binary("mul", self, other)

    def __rmul__(self, other):
        return self._graph._binary("mul", other, self)

    def __truediv__(self, other):
        return self._graph._binary("div", self, other)

    def __rtruediv__(self, other):
        return self._graph._binary("div", other, self)

    def __neg__(self):
        return self._graph._unary("neg", self, self.shape)

    def __matmul__(self, other):
        return self._graph.matmul(self, other)

    def relu(self):
        return self._graph._unary("relu", self, self.shape)

    def exp(self):
        return self._graph._unary("exp", self, self.shape)

    def log(self):
        return self._graph._unary("log", self, self.shape)

    def sqrt(self):
        return self._graph._unary("sqrt", self, self.shape)

    def tanh(self):
        return self._graph._unary("tanh", self, self.shape)

    def sigmoid(self):
        return self._graph._unary("sigmoid", self, self.shape)

    def gelu(self):
        """0.5 * x * (1 + erf(x / sqrt(2))), the exact-erf GELU."""
        return self._graph._unary("gelu", self, self.shape)

    def gelu_grad(self):
        """The derivative of `gelu` at each element."""
        return self._graph._unary("gelu_grad", self, self.shape)

    def sum(self, axis=None, keepdim=False):
        """Sum all elements (pairwise), or over one axis in index order."""
        return self._graph._reduce("sum", self, axis, keepdim)

    def mean(self, axis=None, keepdim=False):
        return self._graph._reduce("mean", self, axis, keepdim)

    def softmax(self, axis=-1):
        return self._graph._softmax("softmax", self, axis)

    def log_softmax(self, axis=-1):
        return self._graph._softmax("log_softmax", self, axis)

    def reshape(self, *shape):
        return self._graph.reshape(self, shape[0] if len(shape) == 1 and isinstance(shape[0], (tuple, list)) else shape)

    def permute(self, *dims):
        return self._graph.permute(self, dims[0] if len(dims) == 1 and isinstance(dims[0], (tuple, list)) else dims)

    def transpose(self, dim0=None, dim1=None):
        """Swap two axes; with no arguments, the two axes of a matrix."""
        if dim0 is None and dim1 is None:
            if len(self.shape) != 2:
                raise GraphError("transpose requires a matrix")
            return self._graph._unary("transpose", self, (self.shape[1], self.shape[0]))
        rank = len(self.shape)
        dims = list(range(rank))
        i, j = _axis(dim0, rank), _axis(dim1, rank)
        dims[i], dims[j] = dims[j], dims[i]
        return self._graph.permute(self, dims)

    @property
    def T(self):
        return self.transpose()

    def cross_entropy(self, targets):
        """Mean cross-entropy of logits [N, C] against integer class targets [N]."""
        return self._graph._cross_entropy("cross_entropy", self, targets)

    def cross_entropy_grad(self, targets):
        """d(mean cross-entropy)/d(logits): (softmax(logits) - onehot(targets)) / N."""
        return self._graph._cross_entropy("cross_entropy_grad", self, targets)

    def positive(self):
        return self._graph._unary("positive", self, self.shape)

    def life(self):
        """One toroidal Conway-style life step. Not a trained neural model."""
        if len(self.shape) != 2:
            raise GraphError("life requires a matrix")
        return self._graph._unary("life", self, self.shape)

    def __bool__(self):
        raise TypeError("A symbolic Tensor has no Python truth value; execute and read an output")

    def __repr__(self):
        return "Tensor(id=%r, shape=%r, dtype='float32')" % (self._id, self.shape)


class Graph:
    def __init__(self):
        self._nodes = []
        self._tensors = []
        # The lowest protocol version that still means this graph (see `program`).
        self._version = 1

    def _version_one(self, op, shape, fields):
        """Whether one node also means exactly the same thing under version 1.

        Version 1 has ten operations, rank 0..2, no broadcasting beyond a scalar
        operand, whole-tensor `sum` and `matmul` on matrices only.
        """
        if op not in _VERSION_ONE_OPS or len(shape) > 2 or "axis" in fields or "keepdim" in fields:
            return False
        operands = [self._tensors[fields[key]].shape for key in ("a", "b") if key in fields]
        if any(len(s) > 2 for s in operands):
            return False
        if op in ("add", "sub", "mul"):
            return operands[0] == operands[1] or operands[0] == () or operands[1] == ()
        return True

    def _append(self, op, tensor_shape, **fields):
        if len(self._nodes) >= 512:
            raise GraphError("Graph exceeds 512 nodes; split work into bounded batches")
        return self._record(op, _shape(tensor_shape), fields)

    def _record(self, op, shape, fields):
        # `shape` is one `_shape` accepted, or derived from such shapes
        # without growing (elementwise, transpose, sum): recording a
        # compiled step validates each shape once, not once per node.
        if len(self._nodes) >= 512:
            raise GraphError("Graph exceeds 512 nodes; split work into bounded batches")
        node_id = len(self._nodes)
        node = {"id": node_id, "op": op}
        node.update(fields)
        if self._version == 1 and not self._version_one(op, shape, fields):
            self._version = 2
        self._nodes.append(node)
        tensor = Tensor(self, node_id, shape)
        self._tensors.append(tensor)
        return tensor

    def _owned(self, tensor):
        if (not isinstance(tensor, Tensor) or tensor._graph is not self
                or not 0 <= tensor._id < len(self._tensors)
                or self._tensors[tensor._id] is not tensor):
            raise GraphError("Tensor belongs to another graph or is not a valid handle")
        return tensor

    def _coerce(self, value):
        return self._owned(value) if isinstance(value, Tensor) else self.tensor(value)

    def tensor(self, data, shape=None):
        if _k is not None and isinstance(data, _k.Storage):
            # A float32 tensor storage (the bundled torch's, under Zipp):
            # copied, so the graph owns its snapshot, and checked in one
            # pass instead of value by value.
            if _k.dtype(data) != "float32":
                raise GraphError("Tensor storage must be float32")
            shape = _shape((_k.size(data),) if shape is None else shape)
            if _k.size(data) != _size(shape):
                raise GraphError("Input length does not match shape")
            if not _k.all_finite(data):
                raise GraphError("Only finite float32 values are supported")
            return self._record("input", shape, {"shape": list(shape), "data": _k.copy(data)})
        flat, inferred = _flatten(data)
        shape = _shape(inferred if shape is None else shape)
        if len(flat) != _size(shape):
            raise GraphError("Input length does not match shape")
        return self._record("input", shape, {"shape": list(shape), "data": flat})

    def full(self, shape, value):
        shape = _shape(shape)
        return self._record("full", shape, {"shape": list(shape), "value": _number(value)})

    def zeros(self, shape):
        return self.full(shape, 0)

    def _binary(self, op, left, right):
        # Check existing handles before recording a new scalar.
        left_tensor, right_tensor = isinstance(left, Tensor), isinstance(right, Tensor)
        if left_tensor:
            self._owned(left)
        if right_tensor:
            self._owned(right)
        a, b = self._coerce(left), self._coerce(right)
        return self._append(op, _broadcast(a.shape, b.shape), a=a._id, b=b._id)

    def _unary(self, op, tensor, shape):
        # `shape` is the operand's own, reversed, or () (see `Tensor`).
        a = self._owned(tensor)
        return self._record(op, shape, {"a": a._id})

    def _reduce(self, op, tensor, axis, keepdim):
        a = self._owned(tensor)
        if not isinstance(keepdim, bool):
            raise GraphError("keepdim must be a bool")
        fields = {"keepdim": True} if keepdim else {}
        if axis is None:
            return self._append(op, tuple(1 for _ in a.shape) if keepdim else (), a=a._id, **fields)
        axis = _axis(axis, len(a.shape))
        shape = tuple(1 if i == axis else d for i, d in enumerate(a.shape)) if keepdim else a.shape[:axis] + a.shape[axis + 1:]
        return self._append(op, shape, a=a._id, axis=axis, **fields)

    def _softmax(self, op, tensor, axis):
        a = self._owned(tensor)
        if not a.shape or _axis(axis, len(a.shape)) != len(a.shape) - 1:
            raise GraphError("%s supports the last axis of a tensor with at least one dimension" % op)
        return self._append(op, a.shape, a=a._id)

    def _cross_entropy(self, op, logits, targets):
        a = self._owned(logits)
        t = self._coerce(targets)
        node = self._nodes[t._id]
        if len(a.shape) != 2 or t.shape != (a.shape[0],):
            raise GraphError("%s requires logits [N, C] and targets [N]" % op)
        # Targets are input data (under Zipp, tensor storage), checked here so
        # every backend may index with them.
        data = node.get("data")
        if _k is not None and isinstance(data, _k.Storage):
            data = _k.to_list(data)
        if node["op"] != "input" or any(v != int(v) or not 0 <= v < a.shape[1] for v in data):
            raise GraphError("%s targets must be a tensor of integer class indices in [0, C)" % op)
        return self._append(op, () if op == "cross_entropy" else a.shape, a=a._id, b=t._id)

    def matmul(self, left, right):
        """[M,K] @ [K,N], or batched [B,M,K] @ [B,K,N] (a batch of 1 or a matrix broadcasts)."""
        a, b = self._owned(left), self._owned(right)
        ra, rb = len(a.shape), len(b.shape)
        if ra not in (2, 3) or rb not in (2, 3) or a.shape[-1] != b.shape[-2]:
            raise GraphError("matmul requires [M,K] @ [K,N] or batched [B,M,K] @ [B,K,N]")
        ba, bb = (a.shape[0] if ra == 3 else 1), (b.shape[0] if rb == 3 else 1)
        if ba != bb and ba != 1 and bb != 1:
            raise GraphError("matmul batch dimensions must match or be 1")
        shape = (a.shape[-2], b.shape[-1])
        return self._append("matmul", (max(ba, bb),) + shape if 3 in (ra, rb) else shape, a=a._id, b=b._id)

    def reshape(self, tensor, shape):
        a = self._owned(tensor)
        shape = list(shape)
        if shape.count(-1) == 1:
            known = _size([d for d in shape if d != -1])
            if not known or _size(a.shape) % known:
                raise GraphError("reshape cannot infer -1 for %r" % (tuple(shape),))
            shape[shape.index(-1)] = _size(a.shape) // known
        shape = _shape(shape)
        if _size(shape) != _size(a.shape):
            raise GraphError("reshape must keep the element count")
        return self._append("reshape", shape, a=a._id, shape=list(shape))

    def permute(self, tensor, dims):
        a = self._owned(tensor)
        dims = [_axis(d, len(a.shape)) for d in dims]
        if sorted(dims) != list(range(len(a.shape))):
            raise GraphError("dims must be a permutation of the tensor's axes")
        return self._append("permute", tuple(a.shape[d] for d in dims), a=a._id, dims=dims)

    # ---- optimizer steps: each returns the next value of a tensor ------------------------
    # They follow torch.optim's single-tensor update order, so a training step
    # (forward, backward and update) can run on the device as one graph.

    def _step(self, op, tensors, **scalars):
        first = self._owned(tensors[0])
        for t in tensors[1:]:
            if self._owned(t).shape != first.shape:
                raise GraphError("%s requires tensors of one shape" % op)
        for name, value in scalars.items():
            if name != "step":
                _number(value)
        refs = dict(zip(["a", "b", "c"], [t._id for t in tensors]))
        refs.update(scalars)
        return self._append(op, first.shape, **refs)

    def sgd_update(self, param, direction, lr):
        """param - lr * direction."""
        return self._step("sgd_update", (param, direction), lr=float(lr))

    def momentum_update(self, buf, grad, momentum, dampening=0.0):
        """momentum * buf + (1 - dampening) * grad (PyTorch's SGD momentum buffer)."""
        return self._step("momentum_update", (buf, grad), momentum=float(momentum), dampening=float(dampening))

    def adam(self, param, grad, m, v, lr=0.001, betas=(0.9, 0.999), eps=1e-8, step=1):
        """One Adam step; returns (next param, next first moment, next second moment)."""
        beta1, beta2 = float(betas[0]), float(betas[1])
        if not (0.0 <= beta1 < 1.0 and 0.0 <= beta2 < 1.0) or not eps >= 0 or type(step) is not int or not 1 <= step <= 2 ** 31:
            raise GraphError("Adam needs betas in [0, 1), eps >= 0 and an integer step >= 1")
        m1 = self._step("adam_m", (m, grad), beta1=beta1)
        v1 = self._step("adam_v", (v, grad), beta2=beta2)
        p1 = self._step("adam_update", (param, m1, v1), lr=float(lr), beta1=beta1, beta2=beta2, eps=float(eps), step=step)
        return p1, m1, v1

    def program(self, **outputs):
        """The plain-data program: version, nodes, and the named outputs.

        `version` is 2 as soon as the graph uses anything the first protocol
        version did not define (a new operation, rank above two, broadcasting
        beyond a scalar operand, an axis reduction or a batched matmul), and
        stays 1 otherwise, so a graph that a version-1 host understands is still
        labelled the way that host expects.
        """
        program = self._program(outputs)
        for node in program["nodes"]:
            if "data" in node and not isinstance(node["data"], list):
                node["data"] = _k.to_list(node["data"])
        return program

    def _program(self, outputs):
        # Input data recorded from tensor storage stays storage here: it
        # leaves for the host as a Float32Array. `program` lists it.
        if not 1 <= len(outputs) <= 64:
            raise GraphError("Request between 1 and 64 named outputs")
        names = []
        for name, value in outputs.items():
            if (not name or len(name) > 64 or not name[0].isascii() or not name[0].isalpha()
                    or any(not c.isascii() or not (c.isalnum() or c == "_") for c in name)
                    or name in ("constructor", "prototype", "__proto__")):
                raise GraphError("Invalid output name")
            names.append({"name": name, "id": self._owned(value)._id})
        nodes = []
        for n in self._nodes:
            # Copy every list field (a shape, a permutation, a list input's
            # data); tensor storage is left as storage for the binary transport.
            nodes.append({key: list(value) if isinstance(value, list) else value for key, value in n.items()})
        return {"version": self._version, "nodes": nodes, "outputs": names}

    def submit(self, callback, on_error=None, **outputs):
        """Execute the program for `outputs` and pass the result to `callback`.

        The result is a plain dict: `result["backend"]` names what ran the
        graph (`webgpu`, `webgl2`, `wasm`, `cpu-js`, or `cpu-python` for the
        float32 reference implementation in this module), `result["outputs"][name]`
        has `shape`, `dtype` and `data` (a flat list), and `result["stats"]`
        carries the host's counters and timings.

        In the Zipp playground the host runs the graph on the GPU after the
        current call returns, so `callback` runs later (from a frame or the
        program's end); the program keeps going meanwhile. Without a host,
        the graph is evaluated here and `callback` runs before `submit`
        returns (under Zipp the reference runs on the engine's tensor
        kernels, with the same results bit for bit). A host failure calls
        `on_error(ComputeError)` when given and otherwise raises it.
        """
        return self._submit(callback, on_error, outputs, False)

    def _submit(self, callback, on_error, outputs, storage, program=None):
        # `storage` (Zipp only): each output's `data` is float32 tensor
        # storage instead of a list, which is how torch.compile takes a
        # result without a Python float per element. `program`: this
        # graph's `_program(outputs)`, when the caller already built it.
        if not callable(callback):
            raise TypeError("submit() needs a callable to receive the result")
        if program is None:
            program = self._program(outputs)
        if _zipp_gpu is not None and _zipp_gpu.hosted():
            def deliver(reply):
                if reply.get("ok"):
                    value = reply["value"]
                    for out in value["outputs"].values():
                        out["data"] = _host_data(out["data"], storage)
                    callback(value)
                    return
                error = reply.get("error") or {}
                exc = ComputeError(error.get("code", "GPU"), error.get("message", "GPU request failed"))
                if on_error is None:
                    raise exc
                on_error(exc)
            _zipp_gpu.post(program, deliver)
            return None
        callback(execute_locally(program) if _k is None else _execute_kernels(program, storage))
        return None

    def to_json(self, **outputs):
        import json
        return json.dumps(self.program(**outputs), allow_nan=False, separators=(",", ":"))

    def prepare(self, feeds=None, carry=None, resident=(), backend=None, on_ready=None, on_error=None, **outputs):
        """Validate the program for `outputs` once and keep its tensors on the device.

        Returns a `Session`. `feeds` names the input tensors each run supplies
        (`{"x": x, "y": y}`, then `session.run(cb, x=..., y=...)`); every other
        input keeps the data it was recorded with. `carry` maps an input tensor
        to the output (a tensor passed in `outputs`, or its name) whose value
        it takes for the next step, so parameters and optimizer state never
        leave the device; `resident` lists outputs kept on the device instead
        of being read back (fetch them with `session.download`). `backend`, if
        given, is the backend the host must be running (`"webgpu"`, ...);
        otherwise whatever the host has serves.

        An `adam_update` step number advances by one per executed step, so one
        prepared step trains a whole run. In the Zipp playground the host
        creates the session after the current call returns (`on_ready(session)`
        runs later; runs requested meanwhile are queued); without a host the
        float32 reference implementation keeps the tensors as lists and the
        session is ready at once.
        """
        program = self._program(outputs)
        nodes = program["nodes"]
        names_of = {}
        for out in program["outputs"]:
            names_of.setdefault(out["id"], []).append(out["name"])
        feed_ids = {}
        for name, tensor in (feeds or {}).items():
            if not isinstance(name, str) or not name or not name.isidentifier():
                raise GraphError("Feed names are identifiers")
            node = nodes[self._owned(tensor)._id]
            if node["op"] != "input":
                raise GraphError("Only input tensors can be fed")
            feed_ids[name] = node["id"]
        for node_id in set(feed_ids.values()):
            nodes[node_id].pop("data", None)
        classes, targets = {}, set()
        for node in nodes:
            if node["op"] in ("cross_entropy", "cross_entropy_grad"):
                targets.add(node["b"])
                if "data" not in nodes[node["b"]]:
                    width = self._tensors[node["a"]].shape[1]
                    if classes.get(node["b"], width) != width:
                        raise GraphError("cross_entropy targets are shared by logits of different widths")
                    classes[node["b"]] = width

        def output_name(value, what):
            if isinstance(value, Tensor):
                names = names_of.get(self._owned(value)._id)
                if not names:
                    raise GraphError("%s must be one of the outputs" % what)
                return names[0]
            if isinstance(value, str) and any(o["name"] == value for o in program["outputs"]):
                return value
            raise GraphError("%s must be one of the outputs" % what)

        for tensor, target in (carry or {}).items():
            node = nodes[self._owned(tensor)._id]
            if node["op"] != "input":
                raise GraphError("Only input tensors can carry a value")
            name = output_name(target, "A carry target")
            out_id = next(o["id"] for o in program["outputs"] if o["name"] == name)
            root = out_id
            while nodes[root]["op"] == "reshape":
                root = nodes[root]["a"]
            if nodes[root]["op"] == "input":
                raise GraphError("A carry target must be a computed output")
            if self._tensors[out_id].shape != tensor.shape:
                raise GraphError("carry %s does not match the input shape" % name)
            if node["id"] in targets:
                raise GraphError("cross_entropy targets cannot carry a value")
            node["carry"] = name
        resident_names = []
        for value in resident:
            name = output_name(value, "A resident tensor")
            if name not in resident_names:
                resident_names.append(name)
        if backend is not None and backend not in ("webgpu", "webgl2", "wasm", "cpu-js", "cpu-python"):
            raise GraphError("Unknown backend %r" % (backend,))
        return Session(self, program, feed_ids, classes, resident_names, backend, on_ready, on_error)


# ---- float32 reference implementation -----------------------------------------------
# The same semantics as the host's JavaScript reference backend: every
# intermediate rounds to float32, whole-tensor sums reduce pairwise, axis sums
# and matmul accumulate in index order, and IEEE non-finite values propagate
# (to be rejected at readback) instead of raising.

def _pairwise_sum(values):
    work = list(values)
    while len(work) > 1:
        nxt = []
        for i in range(0, len(work), 2):
            nxt.append(_f32(work[i] + (work[i + 1] if i + 1 < len(work) else 0.0)))
        work = nxt
    return work[0]


def _exp(x):
    if x != x:
        return x
    try:
        return math.exp(x)
    except OverflowError:
        return math.inf


def _tanh(x):
    return x if x != x else math.tanh(x)


def _log(x):
    if x > 0:
        return math.log(x)
    return -math.inf if x == 0 else math.nan


def _sqrt(x):
    return math.sqrt(x) if x >= 0 else math.nan


def _div(x, y):
    if y == 0:
        if x == 0 or x != x:
            return math.nan
        return math.copysign(math.inf, x) * math.copysign(1.0, y)
    return x / y


def _erf_series(z):
    t = z * z
    return z * (1.1283791670955126 + t * (-0.37612638903183754 + t * (0.11283791670955126 + t * (-0.026866170645131252 +
        t * (0.005223977625442188 + t * (-0.0008548327023450852 + t * 0.00012055332981789664))))))


def _erfc_fit(a):
    t = 1.0 / (1.0 + 0.5 * a)
    return t * _exp(-a * a - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 +
        t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))))


def _cdf(x):
    # The shared erf definition (series below 0.5, Numerical Recipes' erfc fit above);
    # the lower tail comes from erfc directly, without cancelling in 1 + erf.
    z = x * 0.7071067811865476
    if abs(z) < 0.5:
        return 0.5 + 0.5 * _erf_series(z)
    if z >= 10:
        return 1.0
    if z <= -10:
        return 0.0
    c = 0.5 * _erfc_fit(abs(z))
    return 1.0 - c if z > 0 else c


def _sigmoid(x):
    if x >= 0:
        return 1.0 / (1.0 + _exp(-x))
    e = _exp(x)
    return e / (1.0 + e)


_UNARY = {
    "relu": lambda x: x if x > 0 or x != x else 0.0,
    "positive": lambda x: 1.0 if x > 0 else 0.0,
    "neg": lambda x: -x,
    "exp": _exp, "log": _log, "sqrt": _sqrt, "tanh": _tanh, "sigmoid": _sigmoid,
    "gelu": lambda x: x * _cdf(x),
    "gelu_grad": lambda x: _cdf(x) + x * 0.3989422804014327 * _exp(-0.5 * min(x * x, 1e300)),
}
_BINARY = {"add": lambda x, y: x + y, "sub": lambda x, y: x - y, "mul": lambda x, y: x * y, "div": _div}


def _strides(shape):
    out, step = [], 1
    for d in reversed(shape):
        out.append(step)
        step *= d
    return out[::-1]


def _indices(shape):
    """Every multi-index of `shape` in row-major order."""
    result = [()]
    for d in shape:
        result = [index + (i,) for index in result for i in range(d)]
    return result


def _row_stats(row):
    m = row[0]
    for v in row[1:]:
        if v > m or v != v:
            m = v
    s = 0.0
    for v in row:
        s = _f32(s + _f32(_exp(_f32(v - m))))
    return m, s


def _optimizer_step(op, node, a, b, c):
    if op == "sgd_update":
        lr = _f32(node["lr"])
        return [_f32(p - _f32(lr * d)) for p, d in zip(a, b)]
    if op == "momentum_update":
        mu, w = _f32(node["momentum"]), _f32(1 - node["dampening"])
        return [_f32(_f32(mu * x) + _f32(w * g)) for x, g in zip(a, b)]
    if op == "adam_m":
        # torch.lerp(m, grad, 1 - beta1), in the branch PyTorch takes for that weight.
        w = _f32(1 - node["beta1"])
        if w < 0.5:
            return [_f32(x + _f32(w * _f32(g - x))) for x, g in zip(a, b)]
        v1 = _f32(1 - w)
        return [_f32(g - _f32(_f32(g - x) * v1)) for x, g in zip(a, b)]
    if op == "adam_v":
        beta, w = _f32(node["beta2"]), _f32(1 - node["beta2"])
        return [_f32(_f32(x * beta) + _f32(_f32(w * g) * g)) for x, g in zip(a, b)]
    # adam_update: p - lr / (1 - beta1^t) * m / (sqrt(v) / sqrt(1 - beta2^t) + eps)
    step = node["step"]
    size = _f32(node["lr"] / (1 - node["beta1"] ** step))
    bc = _f32(math.sqrt(1 - node["beta2"] ** step))
    eps = _f32(node["eps"])
    return [_f32(p - _f32(size * _f32(_div(m1, _f32(_f32(_div(_f32(_sqrt(v1)), bc)) + eps)))))
            for p, m1, v1 in zip(a, b, c)]


def execute_locally(program, check_finite=True):
    """Run a program with the float32 reference implementation in Python."""
    if not isinstance(program, dict) or program.get("version") not in (1, 2):
        raise ComputeError("PROTOCOL", "Only graph protocol versions 1 and 2 are supported")
    if not isinstance(program.get("nodes"), list) or not isinstance(program.get("outputs"), list):
        raise ComputeError("PROTOCOL", "A program needs node and output lists")
    values = []
    shapes = []
    for node in program["nodes"]:
        op = node["op"]
        a = values[node["a"]] if "a" in node else None
        sa = shapes[node["a"]] if "a" in node else None
        if op == "input":
            shape = tuple(node["shape"])
            out = [_f32(v) for v in node["data"]]
        elif op == "full":
            shape = tuple(node["shape"])
            out = [_f32(node["value"])] * _size(shape)
        elif op in _BINARY:
            b, sb = values[node["b"]], shapes[node["b"]]
            shape = _broadcast(sa, sb)
            fn = _BINARY[op]
            rank = len(shape)
            pa, pb = (1,) * (rank - len(sa)) + sa, (1,) * (rank - len(sb)) + sb
            ta = [0 if pa[i] == 1 else s for i, s in enumerate(_strides(pa))]
            tb = [0 if pb[i] == 1 else s for i, s in enumerate(_strides(pb))]
            out = []
            for index in _indices(shape):
                ia = sum(i * s for i, s in zip(index, ta))
                ib = sum(i * s for i, s in zip(index, tb))
                out.append(_f32(fn(a[ia], b[ib])))
        elif op in _UNARY:
            shape = sa
            fn = _UNARY[op]
            out = [_f32(fn(v)) for v in a]
        elif op in ("sum", "mean"):
            if "axis" not in node:
                shape = tuple(1 for _ in sa) if node.get("keepdim") else ()
                total = _pairwise_sum(a)
                out = [_f32(total / len(a)) if op == "mean" else total]
            else:
                axis = node["axis"]
                dims = sa or (1,)
                outer, length, inner = _size(dims[:axis]), dims[axis], _size(dims[axis + 1:])
                shape = tuple(1 if i == axis else d for i, d in enumerate(sa)) if node.get("keepdim") else sa[:axis] + sa[axis + 1:]
                out = []
                for o in range(outer):
                    for i in range(inner):
                        s = 0.0
                        for j in range(length):
                            s = _f32(s + a[(o * length + j) * inner + i])
                        out.append(_f32(s / length) if op == "mean" else s)
        elif op in ("softmax", "log_softmax"):
            shape = sa
            cols = sa[-1]
            out = []
            for r in range(0, len(a), cols):
                row = a[r:r + cols]
                m, s = _row_stats(row)
                ls = _f32(_log(s))
                for v in row:
                    d = _f32(v - m)
                    out.append(_f32(_div(_f32(_exp(d)), s)) if op == "softmax" else _f32(d - ls))
        elif op in ("cross_entropy", "cross_entropy_grad"):
            targets = values[node["b"]]
            rows, cols = sa
            losses, out = [], []
            for r in range(rows):
                row = a[r * cols:(r + 1) * cols]
                m, s = _row_stats(row)
                t = int(targets[r])
                if op == "cross_entropy":
                    losses.append(_f32(_f32(_log(s)) - _f32(row[t] - m)))
                else:
                    for j, v in enumerate(row):
                        p = _f32(_div(_f32(_exp(_f32(v - m))), s))
                        out.append(_f32(_f32(p - (1.0 if j == t else 0.0)) / rows))
            shape = () if op == "cross_entropy" else sa
            if op == "cross_entropy":
                out = [_f32(_pairwise_sum(losses) / rows)]
        elif op in ("transpose", "permute"):
            dims = node["dims"] if op == "permute" else [1, 0]
            shape = tuple(sa[d] for d in dims)
            st = _strides(sa)
            src = [st[d] for d in dims]
            out = [a[sum(i * s for i, s in zip(index, src))] for index in _indices(shape)]
        elif op == "reshape":
            shape = tuple(node["shape"])
            out = a
        elif op == "matmul":
            b, sb = values[node["b"]], shapes[node["b"]]
            m, k, n = sa[-2], sa[-1], sb[-1]
            ba, bb = (sa[0] if len(sa) == 3 else 1), (sb[0] if len(sb) == 3 else 1)
            batch = max(ba, bb)
            shape = (batch, m, n) if 3 in (len(sa), len(sb)) else (m, n)
            out = []
            for t in range(batch):
                ao, bo = (t * m * k if ba > 1 else 0), (t * k * n if bb > 1 else 0)
                for r in range(m):
                    for c in range(n):
                        s = 0.0
                        for j in range(k):
                            s = _f32(s + _f32(a[ao + r * k + j] * b[bo + j * n + c]))
                        out.append(s)
        elif op in ("sgd_update", "momentum_update", "adam_m", "adam_v", "adam_update"):
            shape = sa
            out = _optimizer_step(op, node, a, values[node["b"]], values[node["c"]] if "c" in node else None)
        elif op == "life":
            shape = sa
            h, w = shape
            out = []
            for y in range(h):
                for x in range(w):
                    count = 0
                    for dy in (-1, 0, 1):
                        for dx in (-1, 0, 1):
                            if (dx or dy) and a[((y + dy) % h) * w + (x + dx) % w] > 0.5:
                                count += 1
                    alive = a[y * w + x] > 0.5
                    out.append(1.0 if count == 3 or (alive and count == 2) else 0.0)
        else:
            raise ComputeError("OP", "Unsupported operation: %s" % op)
        values.append(out)
        shapes.append(tuple(shape))
    outputs = {}
    for o in program["outputs"]:
        data = values[o["id"]]
        if check_finite and any(not math.isfinite(v) for v in data):
            raise ComputeError("NUMBER", "Output contains non-finite values")
        outputs[o["name"]] = {"shape": list(shapes[o["id"]]), "dtype": "float32", "data": list(data)}
    return {"version": 1, "backend": "cpu-python", "outputs": outputs,
            "stats": {"nodes": len(values), "readbackElements": sum(len(v["data"]) for v in outputs.values())}}


def _execute_reference(program, storage, check_finite=True):
    """The reference implementation over the binary transport: input storage
    becomes lists for the run, and each output goes back to storage when the
    caller asked for it (`_k` is present, or this would not be reachable)."""
    nodes = []
    for node in program.get("nodes", ()):
        data = node.get("data")
        nodes.append(dict(node, data=_k.to_list(data)) if isinstance(data, _k.Storage) else node)
    result = execute_locally(dict(program, nodes=nodes), check_finite)
    if storage:
        for out in result["outputs"].values():
            out["data"] = _k.from_flat("float32", out["data"])
    return result


def _host_data(data, storage):
    # A host answers with a Float32Array (tensor storage here) or, from an
    # older host, a list of numbers that arrive as ints when integral.
    if _k is not None and isinstance(data, _k.Storage):
        return data if storage else _k.to_list(data)
    return _k.from_flat("float32", data) if storage else [float(v) for v in data]


# The operations the tensor kernels compute float32-identically to the
# reference below. Everything the second protocol version added — broadcasting
# past a scalar, axis reductions, softmax, cross-entropy, the optimizer steps,
# batched matmul — takes the reference path instead, so a graph produces the
# same numbers whichever path runs it.
_KERNEL_OPS = frozenset((
    "input", "full", "add", "sub", "mul", "relu", "positive", "transpose",
    "sum", "matmul", "life",
))


def _execute_kernels(program, storage, check_finite=True):
    """`execute_locally` on Zipp's tensor kernels: the same float32 numbers."""
    if (program.get("version") != 1
            or any(node.get("op") not in _KERNEL_OPS for node in program.get("nodes", ()))):
        return _execute_reference(program, storage, check_finite)
    values = []
    shapes = []
    zero = None
    for node in program["nodes"]:
        op = node["op"]
        if op == "input":
            shape = tuple(node["shape"])
            data = node["data"]
            out = data if isinstance(data, _k.Storage) else _k.from_flat("float32", data)
        elif op == "full":
            shape = tuple(node["shape"])
            out = _k.full("float32", _size(shape), node["value"])
        elif op in ("add", "sub", "mul"):
            a, b = node["a"], node["b"]
            shape = shapes[b] if not shapes[a] else shapes[a]
            out = _k.binary(op, values[a], shapes[a], values[b], shapes[b])[0]
        elif op == "relu":
            shape = shapes[node["a"]]
            out = _k.unary("relu", values[node["a"]])
        elif op == "positive":
            shape = shapes[node["a"]]
            if zero is None:
                zero = _k.zeros("float32", 1)
            out = _k.astype(_k.binary("gt", values[node["a"]], shape, zero, ())[0], "float32")
        elif op == "transpose":
            h, w = shapes[node["a"]]
            shape = (w, h)
            out = _k.permute(values[node["a"]], (h, w), (1, 0))[0]
        elif op == "sum":
            shape = ()
            out = _k.pair_sum(values[node["a"]])
        elif op == "matmul":
            (m, k), (_, n) = shapes[node["a"]], shapes[node["b"]]
            shape = (m, n)
            out = _k.graph_matmul(values[node["a"]], values[node["b"]], m, k, n)
        elif op == "life":
            shape = shapes[node["a"]]
            out = _k.life(values[node["a"]], shape[0], shape[1])
        else:
            raise ComputeError("OP", "Unsupported operation: %s" % op)
        values.append(out)
        shapes.append(shape)
    outputs = {}
    delivered = set()
    readback = 0
    for o in program["outputs"]:
        data = values[o["id"]]
        if check_finite and not _k.all_finite(data):
            raise ComputeError("NUMBER", "Output contains non-finite values")
        readback += _k.size(data)
        if not storage:
            data = _k.to_list(data)
        elif o["id"] in delivered or data is program["nodes"][o["id"]].get("data"):
            # Each output owns its storage, as a host's copies do: never
            # another output's, nor the graph's recorded input.
            data = _k.copy(data)
        delivered.add(o["id"])
        outputs[o["name"]] = {"shape": list(shapes[o["id"]]), "dtype": "float32", "data": data}
    return {"version": 1, "backend": "cpu-python", "outputs": outputs,
            "stats": {"nodes": len(values), "readbackElements": readback}}


# ---- sessions ---------------------------------------------------------------------------
# The protocol behind `Graph.prepare`: `gpu.session.create` validates once and
# uploads the inputs that have data; `gpu.session.run` feeds the inputs each
# step names, carries outputs into their inputs on the device, and reads back
# only the outputs asked for; `gpu.session.download` fetches resident outputs;
# `gpu.session.dispose` frees them. Without a host the same steps run here on
# the float32 reference, so CPython and Zipp agree on every number.

def _finite(values):
    if _k is not None and isinstance(values, _k.Storage):
        return _k.all_finite(values)
    return all(math.isfinite(v) for v in values)


class Session:
    """A prepared program whose tensors stay on the device between runs (see `Graph.prepare`)."""

    def __init__(self, graph, program, feeds, classes, resident, backend, on_ready, on_error):
        self._graph = graph
        self._program = program
        self._feeds = dict(feeds)
        self._classes = classes
        self._resident = list(resident)
        self._outputs = [o["name"] for o in program["outputs"]]
        self._sizes = {node["id"]: _size(node["shape"]) for node in program["nodes"] if node["op"] == "input"}
        self.backend = None
        self.step = 1
        self._id = None
        self._failed = None
        self._disposed = False
        self._queue = []
        self._hosted = _zipp_gpu is not None and _zipp_gpu.hosted()
        if self._hosted:
            payload = {"program": program, "resident": self._resident}
            if backend is not None:
                payload["backend"] = backend

            def created(reply):
                if reply.get("ok"):
                    value = reply["value"]
                    self._id = value["session"]
                    self.backend = value["backend"]
                    self.step = int(value.get("step", 1))
                    queued, self._queue = self._queue, []
                    for kind, body, deliver in queued:
                        body["session"] = self._id
                        _zipp_gpu.request(kind, body, deliver)
                    if on_ready is not None:
                        on_ready(self)
                    return
                error = reply.get("error") or {}
                exc = ComputeError(error.get("code", "GPU"), error.get("message", "GPU request failed"))
                self._failed = exc
                queued, self._queue = self._queue, []
                for kind, body, deliver in queued:
                    deliver({"ok": False, "error": {"code": exc.code, "message": exc.args[0]}})
                if on_error is None:
                    raise exc
                on_error(exc)

            _zipp_gpu.request("gpu.session.create", payload, created)
            return
        if backend not in (None, "cpu-python"):
            exc = ComputeError("BACKEND", "Session requires backend %s; the reference is cpu-python" % backend)
            if on_error is None:
                raise exc
            on_error(exc)
            self._failed = exc
            return
        self.backend = "cpu-python"
        # Static and initial carried values, by input node id; resident outputs by name.
        self._values = {node["id"]: node["data"] for node in program["nodes"] if node["op"] == "input" and "data" in node}
        self._residents = {}
        if on_ready is not None:
            on_ready(self)

    @property
    def feeds(self):
        """The input names each run may supply."""
        return tuple(self._feeds)

    @property
    def outputs(self):
        return tuple(self._outputs)

    @property
    def resident(self):
        return tuple(self._resident)

    def _live(self):
        if self._disposed:
            raise ComputeError("DISPOSED", "Session has been disposed")
        if self._failed is not None:
            raise self._failed

    def _request(self, kind, body, deliver):
        if self._id is not None:
            body["session"] = self._id
            _zipp_gpu.request(kind, body, deliver)
        else:
            self._queue.append((kind, body, deliver))

    def _reply(self, reply, callback, on_error, convert):
        if reply.get("ok"):
            callback(convert(reply["value"]))
            return
        error = reply.get("error") or {}
        exc = ComputeError(error.get("code", "GPU"), error.get("message", "GPU request failed"))
        if on_error is None:
            raise exc
        on_error(exc)

    def _output_names(self, names, what):
        result = []
        for value in names:
            if isinstance(value, Tensor):
                node_id = self._graph._owned(value)._id
                name = next((o["name"] for o in self._program["outputs"] if o["id"] == node_id), None)
            else:
                name = value if isinstance(value, str) else None
            if name is None or name not in self._outputs:
                raise GraphError("%s must name an output of the prepared program" % what)
            if name not in result:
                result.append(name)
        return result

    def _step_inputs(self, inputs, index):
        given = {}
        for name, value in inputs.items():
            if name not in self._feeds:
                raise GraphError("%r is not a feed of this session" % (name,))
            node_id = self._feeds[name]
            if _k is not None and isinstance(value, _k.Storage):
                if _k.dtype(value) != "float32":
                    raise GraphError("Tensor storage must be float32")
                if not _k.all_finite(value):
                    raise GraphError("Only finite float32 values are supported")
                flat, count = value, _k.size(value)
            else:
                flat, _ = _flatten(value)
                count = len(flat)
            if count != self._sizes[node_id]:
                raise GraphError("Input %s length does not match shape in step %d" % (name, index))
            if node_id in self._classes:
                values = _k.to_list(flat) if _k is not None and isinstance(flat, _k.Storage) else flat
                if any(v != int(v) or not 0 <= v < self._classes[node_id] for v in values):
                    raise GraphError("%s must hold integer class indices in [0, %d)" % (name, self._classes[node_id]))
            given[str(node_id)] = flat
        return given

    def run(self, callback, on_error=None, readback=None, step=None, **inputs):
        """Run one step with `inputs` (by feed name); see `run_steps`."""
        return self.run_steps(callback, [inputs], on_error, readback, step)

    def run_steps(self, callback, steps, on_error=None, readback=None, step=None):
        """Run `steps` (a list of `{feed name: data}` dicts) back to back and pass the result to `callback`.

        The result is a dict: `result["steps"][i]["outputs"][name]` has
        `shape`, `dtype` and `data` (a flat list) for each output in
        `readback` (default: every output not resident), `result["outputs"]`
        is the last step's, `result["backend"]` names what ran it and
        `result["step"]` the next step number. `step` restarts the step count
        for this run (Adam's bias correction follows it). A host failure calls
        `on_error(ComputeError)` when given and otherwise raises it.
        """
        self._live()
        if not callable(callback):
            raise TypeError("run() needs a callable to receive the result")
        if not isinstance(steps, (list, tuple)) or not steps or any(not isinstance(s, dict) for s in steps):
            raise GraphError("steps is a non-empty list of {feed name: data} dicts")
        if step is not None and (type(step) is not int or step < 1):
            raise GraphError("step is a positive integer")
        names = self._output_names(readback, "readback") if readback is not None else [n for n in self._outputs if n not in self._resident]
        payload_steps = [{"inputs": self._step_inputs(s, i)} for i, s in enumerate(steps)]
        if not self._hosted:
            # As the host does: every input has a value at every step, or the whole run is refused before any work.
            covered = set(self._values)
            carries = [node["id"] for node in self._program["nodes"] if node.get("carry") is not None]
            for index, entry in enumerate(payload_steps):
                for node in self._program["nodes"]:
                    if node["op"] == "input" and str(node["id"]) not in entry["inputs"] and node["id"] not in covered:
                        raise ComputeError("REFERENCE", "Input %d has no value: feed it in step %d" % (node["id"], index))
                covered.update(carries)
        if self._hosted:
            body = {"steps": payload_steps, "readback": names}
            if step is not None:
                body["step"] = step

            def convert(value):
                for entry in value["steps"]:
                    for out in entry["outputs"].values():
                        out["data"] = _host_data(out["data"], False)
                value["outputs"] = value["steps"][-1]["outputs"]
                self.step = int(value.get("step", self.step + len(steps)))
                return value

            self._request("gpu.session.run", body, lambda reply: self._reply(reply, callback, on_error, convert))
            return None
        callback(self._run_locally(payload_steps, names, step))
        return None

    def _run_locally(self, payload_steps, names, step):
        first = self.step if step is None else step
        base = self._program
        carries = {node["carry"]: node["id"] for node in base["nodes"] if node.get("carry") is not None}
        results = []
        for index, entry in enumerate(payload_steps):
            step_no = first + index
            nodes = []
            for node in base["nodes"]:
                node = dict(node)
                if node["op"] == "input":
                    node.pop("carry", None)
                    key = str(node["id"])
                    if key in entry["inputs"]:
                        node["data"] = entry["inputs"][key]
                    elif node["id"] in self._values:
                        node["data"] = self._values[node["id"]]
                    else:
                        raise ComputeError("REFERENCE", "Input %d has no value: feed it in step %d" % (node["id"], index))
                elif node["op"] == "adam_update" and step_no != 1:
                    node["step"] = node["step"] + step_no - 1
                nodes.append(node)
            program = dict(base, nodes=nodes)
            result = execute_locally(program, False) if _k is None else _execute_kernels(program, False, False)
            outputs = result["outputs"]
            for name, node_id in carries.items():
                self._values[node_id] = outputs[name]["data"]
            for name in self._resident:
                self._residents[name] = outputs[name]
            read = {}
            for name in names:
                if not _finite(outputs[name]["data"]):
                    raise ComputeError("NUMBER", "Output contains non-finite values")
                read[name] = outputs[name]
            results.append({"step": step_no, "outputs": read})
        self.step = first + len(payload_steps)
        return {"version": 1, "backend": "cpu-python", "steps": results, "outputs": results[-1]["outputs"], "step": self.step,
                "stats": {"steps": len(results), "nodes": len(base["nodes"]),
                          "readbackElements": sum(len(o["data"]) for r in results for o in r["outputs"].values())}}

    def download(self, callback, *names, on_error=None):
        """Read resident outputs (tensors or names) into `callback(result)`, `result["outputs"][name]["data"]` a flat list."""
        self._live()
        if not callable(callback):
            raise TypeError("download() needs a callable to receive the result")
        wanted = self._output_names(names, "download")
        if not wanted:
            raise GraphError("download() names at least one resident output")
        for name in wanted:
            if name not in self._resident:
                raise GraphError("%s is not a resident output" % name)
        if self._hosted:
            def convert(value):
                for out in value["outputs"].values():
                    out["data"] = _host_data(out["data"], False)
                return value

            self._request("gpu.session.download", {"names": wanted}, lambda reply: self._reply(reply, callback, on_error, convert))
            return None
        outputs = {}
        for name in wanted:
            if name not in self._residents:
                exc = ComputeError("REFERENCE", "%s has not been computed yet" % name)
                if on_error is None:
                    raise exc
                on_error(exc)
                return None
            out = self._residents[name]
            data = out["data"]
            outputs[name] = {"shape": list(out["shape"]), "dtype": "float32",
                             "data": _k.to_list(data) if _k is not None and isinstance(data, _k.Storage) else list(data)}
        callback({"version": 1, "backend": "cpu-python", "outputs": outputs})
        return None

    def dispose(self):
        """Release the device tensors; further runs fail with DISPOSED."""
        if self._disposed:
            return
        self._disposed = True
        if self._hosted:
            if self._failed is None:
                self._request("gpu.session.dispose", {}, lambda reply: None)
            return
        self._values = {}
        self._residents = {}
