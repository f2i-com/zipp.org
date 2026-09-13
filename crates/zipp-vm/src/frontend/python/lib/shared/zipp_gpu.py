"""Bounded float32 compute graphs, executed by the host's GPU runtime.

Arithmetic on Tensor objects records nodes; nothing runs until a program is
submitted. A program is plain data (`Graph.program`), executed by the host
through WebGPU, WebGL2, compiled WebAssembly or a JavaScript reference
implementation, and only the named outputs come back.

This module is bundled with the Zipp Python frontend (import it as
`zipp_gpu`) and also runs under CPython, where `submit` evaluates the graph
with the float32 reference implementation below. It is a graph builder, not
a NumPy replacement: shapes are rank 0..2, values are float32, and the
operation set is fixed (see `Tensor`).
"""
import math
import struct

try:
    import _zipp_gpu
except ImportError:
    _zipp_gpu = None

__all__ = ["Graph", "Tensor", "GraphError", "ComputeError", "execute_locally"]


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
    if not isinstance(shape, (tuple, list)) or len(shape) > 2:
        raise GraphError("Use a scalar (), vector (N,), or matrix (M, N)")
    if any(type(d) is not int or not 0 < d <= 4096 for d in shape):
        raise GraphError("Dimensions must be integers in 1..4096")
    result = tuple(shape)
    if _size(result) > 1048576:
        raise GraphError("Tensor exceeds 1,048,576 elements")
    return result


def _size(shape):
    result = 1
    for d in shape:
        result *= d
    return result


def _flatten(data):
    if not isinstance(data, (tuple, list)):
        return [_number(data)], ()
    if not data:
        raise GraphError("Empty tensors are not supported in v1")
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

    def __matmul__(self, other):
        return self._graph.matmul(self, other)

    def relu(self):
        return self._graph._unary("relu", self, self.shape)

    def sum(self):
        """Reduce all elements to one float32 scalar."""
        return self._graph._unary("sum", self, ())

    def transpose(self):
        if len(self.shape) != 2:
            raise GraphError("transpose requires a matrix")
        return self._graph._unary("transpose", self, (self.shape[1], self.shape[0]))

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

    def _append(self, op, tensor_shape, **fields):
        if len(self._nodes) >= 512:
            raise GraphError("Graph exceeds 512 nodes; split work into bounded batches")
        shape = _shape(tensor_shape)
        node_id = len(self._nodes)
        node = {"id": node_id, "op": op}
        node.update(fields)
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

    def tensor(self, data, shape=None):
        flat, inferred = _flatten(data)
        shape = _shape(inferred if shape is None else shape)
        if len(flat) != _size(shape):
            raise GraphError("Input length does not match shape")
        return self._append("input", shape, shape=list(shape), data=flat)

    def full(self, shape, value):
        shape = _shape(shape)
        return self._append("full", shape, shape=list(shape), value=_number(value))

    def zeros(self, shape):
        return self.full(shape, 0)

    def _coerce(self, value):
        return self._owned(value) if isinstance(value, Tensor) else self.tensor(value)

    def _binary(self, op, left, right):
        # Check existing handles before recording a new scalar.
        if isinstance(left, Tensor):
            self._owned(left)
        if isinstance(right, Tensor):
            self._owned(right)
        a, b = self._coerce(left), self._coerce(right)
        if a.shape and b.shape and a.shape != b.shape:
            raise GraphError("Only matching shapes or scalar broadcasting are supported")
        return self._append(op, a.shape or b.shape, a=a._id, b=b._id)

    def _unary(self, op, tensor, shape):
        a = self._owned(tensor)
        return self._append(op, shape, a=a._id)

    def matmul(self, left, right):
        a, b = self._owned(left), self._owned(right)
        if len(a.shape) != 2 or len(b.shape) != 2 or a.shape[1] != b.shape[0]:
            raise GraphError("matmul requires [M,K] @ [K,N]")
        return self._append("matmul", (a.shape[0], b.shape[1]), a=a._id, b=b._id)

    def program(self, **outputs):
        """The plain-data program: version, nodes, and the named outputs."""
        if not 1 <= len(outputs) <= 16:
            raise GraphError("Request between 1 and 16 named outputs")
        names = []
        for name, value in outputs.items():
            if (not name or len(name) > 64 or not name[0].isascii() or not name[0].isalpha()
                    or any(not c.isascii() or not (c.isalnum() or c == "_") for c in name)
                    or name in ("constructor", "prototype", "__proto__")):
                raise GraphError("Invalid output name")
            names.append({"name": name, "id": self._owned(value)._id})
        nodes = []
        for n in self._nodes:
            nodes.append({key: list(value) if isinstance(value, list) else value for key, value in n.items()})
        return {"version": 1, "nodes": nodes, "outputs": names}

    def submit(self, callback, on_error=None, **outputs):
        """Execute the program for `outputs` and pass the result to `callback`.

        The result is a plain dict: `result["backend"]` names what ran the
        graph (`webgpu`, `webgl2`, `wasm`, `cpu-js`, or `cpu-python` for the
        reference implementation in this module), `result["outputs"][name]`
        has `shape`, `dtype` and `data` (a flat list), and `result["stats"]`
        carries the host's counters and timings.

        In the Zipp playground the host runs the graph on the GPU after the
        current call returns, so `callback` runs later (from a frame or the
        program's end); the program keeps going meanwhile. Without a host,
        the graph is evaluated here and `callback` runs before `submit`
        returns. A host failure calls `on_error(ComputeError)` when given and
        otherwise raises it.
        """
        if not callable(callback):
            raise TypeError("submit() needs a callable to receive the result")
        program = self.program(**outputs)
        if _zipp_gpu is not None and _zipp_gpu.hosted():
            def deliver(reply):
                if reply.get("ok"):
                    value = reply["value"]
                    # Host numbers arrive as ints when integral; outputs are float32.
                    for out in value["outputs"].values():
                        out["data"] = [float(v) for v in out["data"]]
                    callback(value)
                    return
                error = reply.get("error") or {}
                exc = ComputeError(error.get("code", "GPU"), error.get("message", "GPU request failed"))
                if on_error is None:
                    raise exc
                on_error(exc)
            _zipp_gpu.post(program, deliver)
            return None
        callback(execute_locally(program))
        return None

    def to_json(self, **outputs):
        import json
        return json.dumps(self.program(**outputs), allow_nan=False, separators=(",", ":"))


# ---- float32 reference implementation -----------------------------------------------
# The same semantics as the host's JavaScript reference backend: every
# intermediate rounds to float32, sums reduce pairwise, and matmul
# accumulates in float32.

def _pairwise_sum(values):
    work = list(values)
    while len(work) > 1:
        nxt = []
        for i in range(0, len(work), 2):
            nxt.append(_f32(work[i] + (work[i + 1] if i + 1 < len(work) else 0.0)))
        work = nxt
    return work[0]


def execute_locally(program):
    """Run a program with the float32 reference implementation in Python."""
    if not isinstance(program, dict) or program.get("version") != 1:
        raise ComputeError("PROTOCOL", "Only graph protocol version 1 is supported")
    values = []
    shapes = []
    for node in program["nodes"]:
        op = node["op"]
        if op == "input":
            shape = tuple(node["shape"])
            out = [_f32(v) for v in node["data"]]
        elif op == "full":
            shape = tuple(node["shape"])
            out = [_f32(node["value"])] * _size(shape)
        elif op in ("add", "sub", "mul"):
            a, b = values[node["a"]], values[node["b"]]
            sa, sb = shapes[node["a"]], shapes[node["b"]]
            shape = sb if not sa else sa
            n = _size(shape)
            out = []
            for i in range(n):
                x = a[0] if not sa else a[i]
                y = b[0] if not sb else b[i]
                out.append(_f32(x + y if op == "add" else x - y if op == "sub" else x * y))
        elif op == "relu":
            shape = shapes[node["a"]]
            out = [v if v > 0 else 0.0 for v in values[node["a"]]]
        elif op == "positive":
            shape = shapes[node["a"]]
            out = [1.0 if v > 0 else 0.0 for v in values[node["a"]]]
        elif op == "transpose":
            h, w = shapes[node["a"]]
            shape = (w, h)
            a = values[node["a"]]
            out = [a[r * w + c] for c in range(w) for r in range(h)]
        elif op == "sum":
            shape = ()
            out = [_pairwise_sum(values[node["a"]])]
        elif op == "matmul":
            a, b = values[node["a"]], values[node["b"]]
            (m, k), (_, n) = shapes[node["a"]], shapes[node["b"]]
            shape = (m, n)
            out = []
            for r in range(m):
                for c in range(n):
                    s = 0.0
                    for j in range(k):
                        s = _f32(s + _f32(a[r * k + j] * b[j * n + c]))
                    out.append(s)
        elif op == "life":
            shape = shapes[node["a"]]
            h, w = shape
            a = values[node["a"]]
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
        shapes.append(shape)
    outputs = {}
    for o in program["outputs"]:
        data = values[o["id"]]
        if any(not math.isfinite(v) for v in data):
            raise ComputeError("NUMBER", "Output contains non-finite values")
        outputs[o["name"]] = {"shape": list(shapes[o["id"]]), "dtype": "float32", "data": list(data)}
    return {"version": 1, "backend": "cpu-python", "outputs": outputs,
            "stats": {"nodes": len(values), "readbackElements": sum(len(v["data"]) for v in outputs.values())}}
