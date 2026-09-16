"""Bounded asynchronous inference and first-order training graphs.

The host executes float32 forward, backward and update nodes. A compiled call
(`compiled(x, y).submit(cb)`) records a graph per call and keeps parameters
and optimizer state on the CPU between submissions; a prepared step
(`compiled.prepare(x, y)`) records once and keeps them on the device between
steps until `sync()` copies them back. This is not TorchInductor or a CUDA
device.
"""
import torch
import _zipp_tensor as _k
from zipp_gpu import Graph, ComputeError

_active = None
# Parameters held by a live prepared session, by id: while one holds a
# parameter, compiled calls and eager optimizer steps on it are refused.
_resident = {}


def active_capture():
    return _active


def eager_step(optimizer):
    """Refuse an eager `optimizer.step()` on parameters a prepared session holds."""
    if _resident:
        for group in optimizer.param_groups:
            for parameter in group["params"]:
                if id(parameter) in _resident:
                    raise RuntimeError("Parameter is resident in a prepared GPU session; sync() and dispose() it before an eager optimizer step")


def _snapshot(tensor):
    # What a result's staleness is judged against: the storage object and
    # its version. Rebinding a tensor's storage (`.data =`, in-place
    # arithmetic) replaces the object; writing into it bumps the version.
    return None if tensor is None else (tensor, tensor._s, _k.version(tensor._s))


def _changed(tensor, snapshot):
    if snapshot is None:
        return tensor is not None
    return tensor is not snapshot[0] or tensor._s is not snapshot[1] or _k.version(tensor._s) != snapshot[2]


def _any_requires_grad(parents):
    # A loop, not any() over a generator: every recorded node asks this.
    for parent in parents:
        if parent.requires_grad:
            return True
    return False


# The options each captured optimizer reads, all snapshotted with `step()`.
_OPTIONS = {
    "sgd": ("lr", "weight_decay", "maximize", "momentum", "dampening", "nesterov"),
    "adam": ("lr", "betas", "eps", "weight_decay", "amsgrad", "maximize"),
}


def _optimizer_configuration(optimizer, kind):
    # Keep values and parameter identities, never aliases to mutable groups/lists.
    # The step counts come along: an eager step between capture and readback
    # is a change of optimizer state, not a continuation of this one.
    groups = []
    for group in optimizer.param_groups:
        values = []
        for name in _OPTIONS[kind]:
            value = group[name]
            items = tuple(value) if name == "betas" and type(value) in (tuple, list) else (value,)
            if any(type(item) not in (int, float, bool) for item in items):
                raise NotImplementedError("GPU optimizer options must be numeric or boolean scalars")
            values.append(items if name == "betas" else value)
        params = group["params"]
        steps = tuple(_step_count(optimizer, p) for p in params)
        groups.append((tuple(id(p) for p in params), tuple(values), steps))
    return (id(optimizer), kind, bool(getattr(optimizer, "_decoupled", False)), tuple(groups))


def _step_count(optimizer, parameter):
    state = optimizer.state.get(id(parameter))
    step = 0 if state is None else state.get("step", 0)
    if type(step) is not int or step < 0:
        raise NotImplementedError("GPU optimizers need an integer step count in their state")
    return step


class _Capture:
    def __init__(self, training=False, prepared=False):
        self.graph = Graph()
        self.training = training
        # A prepared capture records one step that runs many times: optimizer
        # state that does not exist yet is recorded as zero inputs (so it can
        # carry), and every state buffer's input is kept for the carry map.
        self.prepared = prepared
        self.inputs = {}
        self.targets = {}
        self.tape = []
        self.previous_grads = {}
        self.input_metadata = {}
        self.grads = {}
        self.updates = []
        self.state_updates = []
        self.state_inputs = []
        self.state_snapshots = []
        self.step_counts = {}
        self.optimizer = None
        self.optimizer_kind = None
        self.optimizer_snapshot = None
        self.optimizer_params = ()
        self.did_backward = False
        self.did_step = False

    def tensor(self, value, row=False):
        if isinstance(value, _Tensor):
            if value._capture is not self:
                raise ValueError("Cannot mix separate compiled GPU calls")
            return value
        if isinstance(value, torch.Tensor):
            if len(value.shape) > 2:
                raise NotImplementedError("Compiled GPU calls support scalar, vector and matrix tensors only")
            if value.dtype != torch.float32:
                raise TypeError("GPU compilation requires float32 tensors; integer tensors are accepted only as cross_entropy class targets")
            key = id(value)
            if _resident and key in _resident:
                raise RuntimeError("Parameter is resident in a prepared GPU session; sync() and dispose() it before recording another step")
            shape = (1, value.shape[0]) if row else tuple(value.shape)
            if key not in self.inputs:
                # The graph copies the storage (one pass, no Python floats);
                # staleness is checked against storage identity and version.
                symbolic = _Tensor(self, self.graph.tensor(value._s, shape), requires_grad=value.requires_grad)
                self.inputs[key] = (value, symbolic, (value._s, _k.version(value._s)))
                grad = value.grad
                # The data too: a gradient that is not the optimizer's own
                # is accumulated into on the host.
                self.previous_grads[key] = None if grad is None else _snapshot(grad) + (_k.copy(grad._s),)
                self.input_metadata[key] = (tuple(value.shape), value.requires_grad)
            original, symbolic, snapshot = self.inputs[key]
            if tuple(symbolic.shape) != shape:
                raise NotImplementedError("A captured tensor cannot have two different layouts")
            return symbolic
        return _Tensor(self, self.graph.tensor(value))

    def class_targets(self, value):
        """Integer class indices for the fused cross-entropy, recorded once.

        The graph's only dtype is float32, so the int64 (or int32) storage
        is converted exactly (class indices are small integers) and the
        graph checks every value is an integer in [0, C). Targets are data,
        not leaves: no gradient, but the same staleness rules as inputs.
        """
        if isinstance(value, _Tensor):
            raise NotImplementedError("GPU cross_entropy needs integer class targets from the CPU, not a computed graph tensor")
        if not isinstance(value, torch.Tensor):
            raise TypeError("cross_entropy targets must be a tensor")
        if value.dtype.is_floating_point or value.dtype is torch.bool:
            raise TypeError("GPU cross_entropy class targets must be an integer tensor")
        if len(value.shape) != 1:
            raise NotImplementedError("GPU cross_entropy supports class targets of shape [N] for logits [N, C]")
        key = id(value)
        if key not in self.targets:
            symbolic = self.graph.tensor(_k.astype(value._s, "float32"), tuple(value.shape))
            self.targets[key] = (value, symbolic, (value._s, _k.version(value._s)), value.dtype)
        return self.targets[key][1]

    def zero_grad(self, optimizer, set_to_none):
        if not set_to_none:
            raise NotImplementedError("GPU training requires zero_grad(set_to_none=True)")
        if self.optimizer is not None or self.did_backward:
            raise RuntimeError("GPU training supports one zero_grad/backward/step per call")
        self.optimizer = optimizer

    def backward(self, loss):
        if not self.training:
            raise NotImplementedError("Use torch.compile(training=True) for GPU backward")
        if self.did_backward or self.optimizer is None or not loss.requires_grad or loss.shape.numel() != 1:
            raise RuntimeError("GPU backward requires zero_grad(), one scalar loss and one backward call")
        self.did_backward = True
        self.grads[id(loss)] = self.graph.full(tuple(loss.shape), 1.0)
        for node in reversed(self.tape):
            grad = self.grads.get(id(node))
            if grad is None or node.pullback is None:
                continue
            for parent, value in zip(node.parents, node.pullback(grad)):
                if parent.requires_grad:
                    old = self.grads.get(id(parent))
                    self.grads[id(parent)] = value if old is None else old + value

    # ---- optimizer steps ----------------------------------------------------------------
    # Each mirrors torch.optim's single-tensor loop, in its order: negate for
    # maximize, add weight decay, update the state buffers, then the parameter.

    def _begin_step(self, optimizer, closure, kind, name):
        if closure is not None:
            raise NotImplementedError("GPU %s does not support closures" % name)
        if optimizer is not self.optimizer or not self.did_backward or self.did_step:
            raise RuntimeError("GPU training requires one zero_grad/backward/step with the same optimizer")
        self.optimizer_kind = kind
        self.optimizer_snapshot = _optimizer_configuration(optimizer, kind)
        self.optimizer_params = tuple(p for group in optimizer.param_groups for p in group["params"])

    def _trainable(self, optimizer, name):
        seen = set()
        for group in optimizer.param_groups:
            for parameter in group["params"]:
                if id(parameter) in seen:
                    raise ValueError("GPU %s rejects duplicate parameters" % name)
                seen.add(id(parameter))
                if not parameter.requires_grad:
                    continue
                entry = self.inputs.get(id(parameter))
                if entry is None or id(entry[1]) not in self.grads:
                    raise NotImplementedError("Every trainable %s parameter must participate in the captured loss" % name)
                leaf = entry[1]
                gradient = self.grads[id(leaf)]
                if group["maximize"]:
                    gradient = gradient * -1.0
                yield group, parameter, leaf, gradient

    def _state_tensor(self, optimizer, parameter, leaf, key):
        """The optimizer's `key` buffer for `parameter` as a graph input, or None.

        The buffer takes the leaf's captured layout (a bias is a row), so the
        update ops see one shape; it is read back in the parameter's shape.
        """
        state = optimizer.state.get(id(parameter))
        buffer = None if state is None else state.get(key)
        if buffer is not None and (not isinstance(buffer, torch.Tensor) or buffer.dtype != torch.float32
                                   or tuple(buffer.shape) != tuple(parameter.shape)):
            raise NotImplementedError("GPU optimizer state %r must be a float32 tensor shaped like its parameter" % key)
        self.state_snapshots.append((parameter, key, _snapshot(buffer)))
        if buffer is None and self.prepared:
            # A prepared step is one program for every step: a buffer that
            # does not exist yet starts as zeros, recorded as an input so the
            # step's output can carry into it.
            symbolic = self.graph.tensor(_k.full("float32", parameter.shape.numel(), 0.0), tuple(leaf.shape))
        elif buffer is None:
            return None
        else:
            symbolic = self.graph.tensor(buffer._s, tuple(leaf.shape))
        self.state_inputs.append((parameter, key, symbolic))
        return symbolic

    def _end_step(self):
        if not self.updates:
            raise RuntimeError("GPU training requires at least one trainable parameter")
        self.did_step = True

    def sgd(self, optimizer, closure):
        self._begin_step(optimizer, closure, "sgd", "SGD")
        for group, parameter, leaf, gradient in self._trainable(optimizer, "SGD"):
            if group["weight_decay"]:
                gradient = gradient + leaf._value * group["weight_decay"]
            if not group["momentum"]:
                # Plain SGD stays within protocol version 1 (see zipp_gpu).
                self.updates.append((parameter, leaf._value - gradient * group["lr"]))
                continue
            had_buffer = (optimizer.state.get(id(parameter)) or {}).get("momentum_buffer") is not None
            if self.prepared and not had_buffer and group["dampening"]:
                # A zero buffer reproduces PyTorch's first step (buffer = grad)
                # only without dampening: momentum * 0 + (1 - 0) * grad is grad.
                raise NotImplementedError("A prepared SGD step with dampening needs an existing momentum buffer; run one eager or compiled step first")
            buffer = self._state_tensor(optimizer, parameter, leaf, "momentum_buffer")
            # PyTorch clones the first gradient into the buffer; the momentum
            # and dampening weights apply from the second step on.
            if buffer is None:
                buffer = gradient
            else:
                buffer = self.graph.momentum_update(buffer, gradient, group["momentum"], group["dampening"])
            self.state_updates.append((parameter, "momentum_buffer", buffer))
            direction = gradient + buffer * group["momentum"] if group["nesterov"] else buffer
            self.updates.append((parameter, self.graph.sgd_update(leaf._value, direction, group["lr"])))
        self._end_step()

    def adam(self, optimizer, closure):
        self._begin_step(optimizer, closure, "adam", "Adam")
        decoupled = bool(getattr(optimizer, "_decoupled", False))
        for group, parameter, leaf, gradient in self._trainable(optimizer, "Adam"):
            if group["amsgrad"]:
                raise NotImplementedError("GPU Adam does not support amsgrad")
            value = leaf._value
            lr, weight_decay = group["lr"], group["weight_decay"]
            if weight_decay:
                if decoupled:
                    value = value * (1.0 - lr * weight_decay)
                else:
                    gradient = gradient + value * weight_decay
            shape = tuple(leaf.shape)
            m = self._state_tensor(optimizer, parameter, leaf, "exp_avg")
            v = self._state_tensor(optimizer, parameter, leaf, "exp_avg_sq")
            step = _step_count(optimizer, parameter) + 1
            new_value, new_m, new_v = self.graph.adam(
                value, gradient,
                self.graph.zeros(shape) if m is None else m,
                self.graph.zeros(shape) if v is None else v,
                lr, group["betas"], group["eps"], step)
            self.updates.append((parameter, new_value))
            self.state_updates.append((parameter, "exp_avg", new_m))
            self.state_updates.append((parameter, "exp_avg_sq", new_v))
            self.step_counts[id(parameter)] = step
        self._end_step()


def _unbroadcast(g, tensor):
    """Reduce a broadcast gradient back to the operand's shape."""
    shape = tuple(tensor.shape)
    if tuple(g.shape) == shape:
        return g
    if not shape:
        return g.sum()
    for _ in range(len(g.shape) - len(shape)):
        g = g.sum(0)
    for axis, size in enumerate(shape):
        if size == 1 and g.shape[axis] != 1:
            g = g.sum(axis, keepdim=True)
    return g


def _one_dim(dim, rank, name):
    if isinstance(dim, (tuple, list)):
        if len(dim) != 1:
            raise NotImplementedError("GPU %s reduces all elements or one dimension" % name)
        dim = dim[0]
    if isinstance(dim, bool) or not isinstance(dim, int) or not -rank <= dim < rank:
        raise IndexError("Dimension out of range (expected to be in range of [%d, %d], but got %r)" % (-rank, rank - 1, dim))
    return dim + rank if dim < 0 else dim


class _Tensor:
    def __init__(self, capture, value, parents=(), pullback=None, requires_grad=None):
        self._capture = capture
        self._value = value
        self._zipp_graph = True
        self.shape = torch.Size(value.shape)
        self.dtype = torch.float32
        self.parents = parents
        self.pullback = pullback
        # Explicit requires_grad describes a source leaf. no_grad suppresses
        # operation recording, but must not change a cached leaf's intrinsic flag.
        self.requires_grad = capture.training and (
            requires_grad if requires_grad is not None
            else torch.is_grad_enabled() and _any_requires_grad(parents)
        )
        if self.requires_grad:
            capture.tape.append(self)

    def _binary(self, operation, other, reverse=False):
        if isinstance(other, torch.Tensor) and len(self.shape) == 2 and tuple(other.shape) == (self.shape[1],):
            # A bias row broadcast by a ones-matmul keeps dense layers within
            # protocol version 1; other broadcasting uses the graph's own.
            other = self._capture.tensor(other, row=True)
            ones = _Tensor(self._capture, self._capture.graph.full((self.shape[0], 1), 1.0))
            other = ones @ other
        rhs = self._capture.tensor(other)
        a, b = (rhs, self) if reverse else (self, rhs)
        av, bv = a._value, b._value
        result = av + bv if operation == "add" else av - bv if operation == "sub" else av * bv
        def backward(g):
            ga = g * bv if operation == "mul" else g
            gb = g * av if operation == "mul" else g * -1.0 if operation == "sub" else g
            return (_unbroadcast(ga, a), _unbroadcast(gb, b))
        return _Tensor(self._capture, result, (a, b), backward)

    def _unary(self, value, pullback):
        return _Tensor(self._capture, value, (self,), pullback)

    def __add__(self, other): return self._binary("add", other)
    def __radd__(self, other): return self._binary("add", other, True)
    def __sub__(self, other): return self._binary("sub", other)
    def __rsub__(self, other): return self._binary("sub", other, True)
    def __mul__(self, other): return self._binary("mul", other)
    def __rmul__(self, other): return self._binary("mul", other, True)
    def __neg__(self): return self * -1.0
    def __truediv__(self, other):
        if not isinstance(other, (int, float)) or isinstance(other, bool):
            raise NotImplementedError("GPU division supports a numeric scalar divisor only")
        return self * (1.0 / other)
    def __pow__(self, exponent):
        if exponent != 2:
            raise NotImplementedError("GPU power currently supports exponent 2 only")
        return self * self
    def __matmul__(self, other):
        b = self._capture.tensor(other)
        a = self._value
        return _Tensor(self._capture, a @ b._value, (self, b), lambda g: (g @ b._value.transpose(), a.transpose() @ g))
    def __rmatmul__(self, other): return self._capture.tensor(other) @ self
    def transpose(self, dim0, dim1):
        if len(self.shape) != 2 or (dim0 % 2, dim1 % 2) not in ((0, 1), (1, 0)) or not -2 <= dim0 < 2 or not -2 <= dim1 < 2:
            raise NotImplementedError("GPU transpose supports swapping the two matrix dimensions")
        return self._unary(self._value.transpose(), lambda g: (g.transpose(),))
    @property
    def T(self): return self.transpose(0, 1)
    def linear(self, weight, bias):
        result = self @ self._capture.tensor(weight).T
        return result if bias is None else result + bias

    # ---- activations: each pullback is composed from graph operations -------------
    def relu(self):
        return self._unary(self._value.relu(), lambda g: (g * self._value.positive(),))
    def gelu(self, approximate="none"):
        if approximate != "none":
            raise NotImplementedError("GPU gelu supports approximate='none' (the exact erf form)")
        x = self._value
        return self._unary(x.gelu(), lambda g: (g * x.gelu_grad(),))
    def sigmoid(self):
        s = self._value.sigmoid()
        return self._unary(s, lambda g: (g * (s * (1.0 - s)),))
    def tanh(self):
        t = self._value.tanh()
        return self._unary(t, lambda g: (g * (1.0 - t * t),))
    def exp(self):
        e = self._value.exp()
        return self._unary(e, lambda g: (g * e,))
    def log(self):
        x = self._value
        return self._unary(x.log(), lambda g: (g / x,))
    def _last_axis(self, dim, name):
        if not self.shape or _one_dim(dim, len(self.shape), name) != len(self.shape) - 1:
            raise NotImplementedError("GPU %s supports the last dimension of a tensor with at least one dimension" % name)
    def softmax(self, dim=-1, dtype=None):
        self._last_axis(dim, "softmax")
        if dtype is not None and dtype != torch.float32:
            raise NotImplementedError("GPU softmax computes float32 only")
        s = self._value.softmax()
        return self._unary(s, lambda g: (s * (g - (g * s).sum(-1, keepdim=True)),))
    def log_softmax(self, dim=-1, dtype=None):
        self._last_axis(dim, "log_softmax")
        if dtype is not None and dtype != torch.float32:
            raise NotImplementedError("GPU log_softmax computes float32 only")
        ls = self._value.log_softmax()
        return self._unary(ls, lambda g: (g - ls.exp() * g.sum(-1, keepdim=True),))

    # ---- reductions ------------------------------------------------------------------
    def _reduce(self, op, dim, keepdim, dtype):
        if dtype is not None and dtype != torch.float32:
            raise NotImplementedError("GPU %s computes float32 only" % op)
        if not isinstance(keepdim, bool):
            raise TypeError("keepdim must be a bool")
        shape = tuple(self.shape)
        graph = self._capture.graph
        if dim is None:
            if not keepdim and op == "sum":
                # Whole-tensor sum, the protocol version 1 form (mean is sum / n).
                return self._unary(self._value.sum(), lambda g: (graph.full(shape, 1.0) * g,))
            if not keepdim:
                return self.sum() / self.shape.numel()
            axis, count, kept = None, self.shape.numel(), tuple(1 for _ in shape)
        else:
            axis = _one_dim(dim, len(shape), op)
            count = shape[axis]
            kept = tuple(1 if i == axis else d for i, d in enumerate(shape))
        value = self._value.sum(axis, keepdim) if op == "sum" else self._value.mean(axis, keepdim)
        scale = 1.0 if op == "sum" else 1.0 / count
        def backward(g):
            spread = g if keepdim else g.reshape(kept)
            return (graph.full(shape, scale) * spread,)
        return self._unary(value, backward)
    def sum(self, dim=None, keepdim=False, dtype=None):
        return self._reduce("sum", dim, keepdim, dtype)
    def mean(self, dim=None, keepdim=False, dtype=None):
        return self._reduce("mean", dim, keepdim, dtype)

    # ---- losses ------------------------------------------------------------------------
    def cross_entropy(self, target, weight=None, reduction="mean", label_smoothing=0.0):
        """The fused mean cross-entropy of logits [N, C] against class indices [N]."""
        if weight is not None or label_smoothing:
            raise NotImplementedError("GPU cross_entropy supports no class weights or label smoothing")
        if reduction not in ("mean", "sum"):
            raise NotImplementedError("GPU cross_entropy supports reduction='mean' or 'sum'")
        if len(self.shape) != 2:
            raise NotImplementedError("GPU cross_entropy needs logits of shape [N, C]")
        targets = self._capture.class_targets(target)
        logits = self._value
        loss = self._unary(logits.cross_entropy(targets), lambda g: (g * logits.cross_entropy_grad(targets),))
        return loss if reduction == "mean" else loss * float(self.shape[0])

    def backward(self, gradient=None, retain_graph=False, create_graph=False):
        if gradient is not None or retain_graph or create_graph:
            raise NotImplementedError("GPU backward supports first-order scalar losses only")
        self._capture.backward(self)
    def __bool__(self):
        raise TypeError("Compiled GPU tensors cannot control Python branches")


class GPUResult:
    """A recorded call. Training commits gradients, weights and optimizer state after successful readback."""
    def __init__(self, value):
        self.shape = value.shape
        self._value = value
        self.backend = None
        self.stats = None
        self._submitted = False

    def submit(self, callback, on_error=None):
        if not callable(callback) or (on_error is not None and not callable(on_error)):
            raise TypeError("submit requires a callback and optional error callback")
        capture = self._value._capture
        if capture.training and self._submitted:
            raise RuntimeError("A GPU training result can only be submitted once")
        outputs = {"result": self._value._value}
        leaves = [entry for entry in capture.inputs.values() if id(entry[1]) in capture.grads]
        optimizer_ids = set(id(p) for p in capture.optimizer_params)
        for i, entry in enumerate(leaves):
            gradient = capture.grads[id(entry[1])]
            previous = capture.previous_grads[id(entry[0])]
            if id(entry[0]) not in optimizer_ids and previous is not None:
                gradient = gradient + capture.graph.tensor(previous[3], tuple(entry[1].shape))
            outputs["grad" + str(i)] = gradient
        for i, entry in enumerate(capture.updates):
            outputs["weight" + str(i)] = entry[1]
        for i, entry in enumerate(capture.state_updates):
            outputs["state" + str(i)] = entry[2]
        # Validate output/node budgets before claiming the single-use submission.
        program = capture.graph._program(outputs)
        self._submitted = True
        def arrived(result):
            def read(name, shape):
                # Outputs arrive as float32 storage the result owns: the
                # tensor wraps it, with no per-element conversion.
                data = result["outputs"][name]["data"]
                if _k.size(data) != torch._numel(shape):
                    raise RuntimeError("GPU output %s has %d elements, expected shape %s" % (name, _k.size(data), shape))
                if capture.training and not _k.all_finite(data):
                    raise RuntimeError("GPU training produced non-finite values; parameters were not updated")
                return torch.Tensor(data, shape, torch.float32)
            try:
                if capture.training:
                    try:
                        optimizer_changed = _optimizer_configuration(capture.optimizer, capture.optimizer_kind) != capture.optimizer_snapshot
                    except Exception:
                        optimizer_changed = True
                    if optimizer_changed:
                        raise RuntimeError("GPU training result is stale; optimizer changed before completion")
                value = read("result", tuple(self.shape))
                gradients = [(entry[0], read("grad" + str(i), tuple(entry[0].shape))) for i, entry in enumerate(leaves)]
                updates = [(entry[0], read("weight" + str(i), tuple(entry[0].shape))) for i, entry in enumerate(capture.updates)]
                states = [(entry[0], entry[1], read("state" + str(i), tuple(entry[0].shape))) for i, entry in enumerate(capture.state_updates)]
                if capture.training:
                    for original, symbolic, snapshot in capture.inputs.values():
                        if original.dtype != torch.float32 or original._s is not snapshot[0] or _k.version(original._s) != snapshot[1]:
                            raise RuntimeError("GPU training result is stale; a captured tensor changed before completion")
                        if _changed(original.grad, capture.previous_grads[id(original)]):
                            raise RuntimeError("GPU training result is stale; a captured gradient changed before completion")
                        if (tuple(original.shape), original.requires_grad) != capture.input_metadata[id(original)]:
                            raise RuntimeError("GPU training result is stale; a captured tensor changed before completion")
                    for original, symbolic, snapshot, dtype in capture.targets.values():
                        if original.dtype is not dtype or original._s is not snapshot[0] or _k.version(original._s) != snapshot[1]:
                            raise RuntimeError("GPU training result is stale; captured class targets changed before completion")
                    for parameter, key, snapshot in capture.state_snapshots:
                        state = capture.optimizer.state.get(id(parameter))
                        if _changed(None if state is None else state.get(key), snapshot):
                            raise RuntimeError("GPU training result is stale; optimizer state changed before completion")
                    for parameter, new_value in updates:
                        parameter.data = new_value
                    for parameter, key, new_value in states:
                        capture.optimizer.state.setdefault(id(parameter), {})[key] = new_value
                    for parameter, step in capture.step_counts.items():
                        capture.optimizer.state[parameter]["step"] = step
                    for parameter in capture.optimizer_params:
                        parameter.grad = None
                    for parameter, gradient in gradients:
                        parameter.grad = gradient
                self.backend = result["backend"]
                self.stats = result.get("stats")
            except Exception as error:
                if on_error is None:
                    raise
                on_error(error)
                return
            callback(value)
        capture.graph._submit(arrived, on_error, outputs, True, program)

    def backward(self, *args, **kwargs):
        raise NotImplementedError("Call loss.backward() inside a torch.compile(training=True) function")


def _record(model, training, args, kwargs, prepared=False):
    """Run `model` once with graph tensors in place of its float tensor arguments."""
    global _active
    if _active is not None:
        raise RuntimeError("Nested compiled training calls are unsupported")
    capture = _Capture(training, prepared)
    # Float tensors become graph leaves; integer tensors stay on the CPU
    # (cross_entropy records its class targets from there).
    convert = lambda value: capture.tensor(value) if isinstance(value, torch.Tensor) and value.dtype.is_floating_point else value
    # Eager ops look for graph tensors only while a call records.
    torch._recording(1)
    try:
        _active = capture if training else None
        with torch.enable_grad() if training else torch.no_grad():
            output = model(*[convert(value) for value in args], **{key: convert(value) for key, value in kwargs.items()})
        if not isinstance(output, _Tensor):
            raise TypeError("Compiled GPU calls must return one supported graph tensor")
        if training and not capture.did_step:
            raise RuntimeError("Compiled GPU training must call zero_grad(), backward() and optimizer.step()")
        return capture, output
    finally:
        _active = None
        torch._recording(-1)


class Compiled:
    """What `torch.compile` returns: call it to record and submit one step, or `prepare` it."""

    def __init__(self, model, training):
        self._model = model
        self.training = training

    def __call__(self, *args, **kwargs):
        capture, output = _record(self._model, self.training, args, kwargs)
        return GPUResult(output)

    def prepare(self, *args, backend=None, on_ready=None, on_error=None, **kwargs):
        """Record the step once from this example call and keep it on the device.

        Returns a `Prepared` session. The float tensor arguments and the
        integer class targets among the arguments are the feeds each step
        supplies (in the recorded shapes and dtypes); every other tensor the
        call reads (parameters, constants) is uploaded now. For a training
        step the parameters, momentum buffers and Adam moments then stay on
        the device and carry from step to step, and the optimizer's step
        count advances per executed step; `sync()` copies them back into the
        model and `optimizer.state`. Non-tensor arguments are recorded as
        constants and must be passed unchanged to every step.

        `backend` names the backend the host must be running (`"webgpu"`,
        `"webgl2"`, `"wasm"`, `"cpu-js"`; `"cpu-python"` is the reference
        without a host), otherwise whatever it has serves. In the playground
        the host creates the session after the current call returns
        (`on_ready(prepared)` runs then; steps requested meanwhile are queued
        behind it); without a host it is ready at once.
        """
        return Prepared(self._model, self.training, args, kwargs, backend, on_ready, on_error)


class Prepared:
    """A training (or inference) step recorded once, with its tensors resident on the device.

    `step(callback, *args)` runs one step, `steps(callback, [args, ...])`
    several in one submission; each callback receives the returned tensor(s)
    read back. `sync(callback)` downloads weights, optimizer state and the last
    gradients into the eager model; `dispose()` frees the device tensors.
    While a session is live its parameters are refused to compiled calls and
    eager `optimizer.step()`: sync and dispose first. A step that fails on
    the device poisons the session (only `dispose` remains), since the
    resident state may have advanced partially.
    """

    def __init__(self, model, training, args, kwargs, backend, on_ready, on_error):
        capture, output = _record(model, training, args, kwargs, True)
        self._capture = capture
        self.training = training
        self.shape = output.shape
        self.backend = None
        self.stats = None
        self.executed = 0
        self._since_sync = 0
        self._failed = None
        self._disposed = False
        self._session = None
        # The feeds: each float tensor argument and each integer class-target
        # argument that the recorded step reads, by position or keyword.
        self._feeds = []
        self._constants = []
        feeds = {}
        optimizer_ids = set(id(p) for p in capture.optimizer_params)
        for position, value in list(enumerate(args)) + list(kwargs.items()):
            if not isinstance(value, torch.Tensor):
                self._constants.append((position, value))
                continue
            key = id(value)
            if value.dtype.is_floating_point:
                entry = capture.inputs.get(key)
                if entry is None:
                    continue
                if key in optimizer_ids:
                    raise ValueError("A step argument cannot also be an optimizer parameter")
                name, node = "f%d" % len(feeds), entry[1]._value
            else:
                entry = capture.targets.get(key)
                if entry is None:
                    continue
                name, node = "t%d" % len(feeds), entry[1]
            feeds[name] = node
            self._feeds.append((position, name, tuple(value.shape), value.dtype))
        # Outputs: the result read back each step; per parameter its weight,
        # optimizer buffers and gradient resident, carried into their inputs.
        self._leaves = [entry for entry in capture.inputs.values() if id(entry[1]) in capture.grads]
        for entry in self._leaves:
            if id(entry[0]) not in optimizer_ids:
                raise NotImplementedError("A prepared step computes gradients for the optimizer's parameters only; a captured tensor outside the optimizer requires grad")
        outputs = {"result": output._value}
        carry = {}
        for i, entry in enumerate(self._leaves):
            outputs["grad" + str(i)] = capture.grads[id(entry[1])]
        for i, (parameter, new_value) in enumerate(capture.updates):
            outputs["weight" + str(i)] = new_value
            carry[capture.inputs[id(parameter)][1]._value] = "weight" + str(i)
        state_inputs = dict(((id(p), key), symbolic) for p, key, symbolic in capture.state_inputs)
        for i, (parameter, key, new_value) in enumerate(capture.state_updates):
            outputs["state" + str(i)] = new_value
            carry[state_inputs[(id(parameter), key)]] = "state" + str(i)
        if len(outputs) > 64:
            per_parameter = 2 + len(capture.state_updates) // max(len(capture.updates), 1)
            raise NotImplementedError(
                "A prepared %s step needs %d graph outputs (the result, then the weight, gradient and %d optimizer buffer(s) of each of %d parameter tensors); "
                "the graph protocol allows 64, so prepare() holds at most %d parameter tensors with this optimizer"
                % (capture.optimizer_kind, len(outputs), per_parameter - 2, len(capture.updates), 63 // per_parameter))
        self._resident = [name for name in outputs if name != "result"]
        self._snapshot = None if capture.optimizer is None else _optimizer_configuration(capture.optimizer, capture.optimizer_kind)
        for parameter in capture.optimizer_params:
            _resident[id(parameter)] = self

        def ready(session):
            # Without a host this runs inside `Graph.prepare`, before it returns.
            self._session = session
            self.backend = session.backend
            if on_ready is not None:
                on_ready(self)

        def failed(error):
            self._failed = error
            self._release()
            if on_error is None:
                raise error
            on_error(error)

        try:
            self._session = capture.graph.prepare(feeds=feeds, carry=carry, resident=self._resident, backend=backend,
                                                  on_ready=ready, on_error=failed, **outputs)
        except BaseException:
            self._release()
            raise

    @property
    def feeds(self):
        """The recorded (shape, dtype) of each fed argument, by position or keyword."""
        return tuple((position, shape, dtype) for position, name, shape, dtype in self._feeds)

    @property
    def session(self):
        return self._session

    def _release(self):
        for parameter in self._capture.optimizer_params:
            if _resident.get(id(parameter)) is self:
                del _resident[id(parameter)]

    def _live(self):
        if self._disposed:
            raise RuntimeError("Prepared GPU session has been disposed")
        if self._failed is not None:
            raise RuntimeError("Prepared GPU session failed (%s); dispose() it and prepare again" % self._failed)
        capture = self._capture
        if capture.optimizer is not None:
            try:
                changed = _optimizer_configuration(capture.optimizer, capture.optimizer_kind) != self._snapshot
            except Exception:
                changed = True
            if changed:
                raise RuntimeError("Optimizer changed since prepare(); sync(), dispose() and prepare again")

    def _step_feeds(self, args, kwargs, index):
        # A step passes the recorded arguments: tensors of the recorded
        # shape and dtype where the example had them, constants unchanged.
        given = {}
        for position, value in list(enumerate(args)) + list(kwargs.items()):
            given[position] = value
        for position, value in self._constants:
            if position not in given or given[position] != value:
                raise ValueError("Step %d must pass the recorded non-tensor argument %r unchanged" % (index, position))
        feeds = {}
        for position, name, shape, dtype in self._feeds:
            value = given.get(position)
            if not isinstance(value, torch.Tensor) or tuple(value.shape) != shape or value.dtype != dtype:
                raise ValueError("Step %d argument %r must be a %s tensor of shape %s" % (index, position, dtype, list(shape)))
            feeds[name] = value._s if dtype == torch.float32 else _k.astype(value._s, "float32")
        return feeds

    def _fail(self, error, on_error):
        self._failed = error
        self._release()
        if on_error is None:
            raise error
        on_error(error)

    def _read(self, result_step, index):
        data = result_step["outputs"]["result"]["data"]
        if _k.size(data) != torch._numel(self.shape):
            raise RuntimeError("GPU output of step %d has %d elements, expected shape %s" % (index, _k.size(data), self.shape))
        return torch.Tensor(data, self.shape, torch.float32)

    def _run(self, callback, batches, on_error, many):
        if not callable(callback) or (on_error is not None and not callable(on_error)):
            raise TypeError("step requires a callback and optional error callback")
        self._live()
        if not 1 <= len(batches) <= 64:
            raise ValueError("steps() submits between 1 and 64 steps in one run")
        feeds = [self._step_feeds(args, kwargs, i) for i, (args, kwargs) in enumerate(batches)]
        count = len(batches)

        def arrived(result):
            self.executed += count
            self._since_sync += count
            self.backend = result["backend"]
            self.stats = result.get("stats")
            try:
                values = [self._read(entry, i) for i, entry in enumerate(result["steps"])]
            except Exception as error:
                self._fail(error, on_error)
                return
            callback(values if many else values[0])

        try:
            self._session._run_steps(arrived, feeds, lambda error: self._fail(error, on_error), None, None, True)
        except ComputeError as error:
            self._fail(error, on_error)

    def step(self, callback, *args, on_error=None, **kwargs):
        """Run one step with these arguments; `callback(result)` receives the returned tensor read back."""
        return self._run(callback, [(args, kwargs)], on_error, False)

    def steps(self, callback, batches, on_error=None):
        """Run several steps in one submission: `batches` is a list of argument tuples
        (or `(args, kwargs)` pairs); `callback(results)` receives one tensor per step."""
        if not isinstance(batches, (list, tuple)) or not batches:
            raise TypeError("steps() takes a non-empty list of argument tuples")
        normalized = []
        for batch in batches:
            if isinstance(batch, tuple) and len(batch) == 2 and isinstance(batch[0], tuple) and isinstance(batch[1], dict):
                normalized.append(batch)
            elif isinstance(batch, (tuple, list)):
                normalized.append((tuple(batch), {}))
            else:
                normalized.append(((batch,), {}))
        return self._run(callback, normalized, on_error, True)

    def sync(self, callback, on_error=None):
        """Download the resident weights, optimizer state and last gradients into the eager tensors.

        Afterwards the model and `optimizer.state` are what the same number of
        eager `optimizer.step()` calls would have left; the session stays
        live and keeps training from the same values.
        """
        if not callable(callback) or (on_error is not None and not callable(on_error)):
            raise TypeError("sync requires a callback and optional error callback")
        self._live()
        capture = self._capture
        if capture.optimizer is None or not self._since_sync:
            callback(self)
            return None

        def arrived(result):
            outputs = result["outputs"]
            try:
                for name in self._resident:
                    if not _k.all_finite(outputs[name]["data"]):
                        raise RuntimeError("GPU training produced non-finite values; parameters were not updated")
                read = lambda name, shape: torch.Tensor(outputs[name]["data"], shape, torch.float32)
                for i, (parameter, new_value) in enumerate(capture.updates):
                    parameter.data = read("weight" + str(i), parameter.shape)
                for i, (parameter, key, new_value) in enumerate(capture.state_updates):
                    capture.optimizer.state.setdefault(id(parameter), {})[key] = read("state" + str(i), parameter.shape)
                for parameter, step in capture.step_counts.items():
                    # Recorded as the count after one step; the device advanced it per executed step.
                    capture.optimizer.state[parameter]["step"] = step + self.executed - 1
                for parameter in capture.optimizer_params:
                    parameter.grad = None
                for i, entry in enumerate(self._leaves):
                    entry[0].grad = read("grad" + str(i), entry[0].shape)
            except Exception as error:
                self._fail(error, on_error)
                return
            self._since_sync = 0
            self._snapshot = _optimizer_configuration(capture.optimizer, capture.optimizer_kind)
            callback(self)

        try:
            self._session._download(arrived, self._resident, lambda error: self._fail(error, on_error), True)
        except ComputeError as error:
            self._fail(error, on_error)
        return None

    def dispose(self):
        """Free the device tensors and release the parameters; what was not synced is lost."""
        if self._disposed:
            return
        self._disposed = True
        self._release()
        if self._session is not None:
            self._session.dispose()


def compile(model=None, *, backend="zipp_gpu", training=False):
    if backend != "zipp_gpu":
        raise ValueError("The Zipp compile extension supports only backend='zipp_gpu'")
    if model is None:
        return lambda fn: compile(fn, backend=backend, training=training)
    if not callable(model):
        raise TypeError("torch.compile requires a callable or nn.Module")
    return Compiled(model, training)
