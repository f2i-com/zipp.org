"""Bounded asynchronous inference and first-order SGD training graphs.

The host executes float32 forward, backward and update nodes. Parameters live
on the CPU between submissions; this is not TorchInductor or a CUDA device.
"""
import torch
import _zipp_tensor as _k
from zipp_gpu import Graph

_active = None


def active_capture():
    return _active


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


def _sgd_configuration(optimizer):
    # Keep values and parameter identities, never aliases to mutable groups/lists.
    options = ("lr", "weight_decay", "maximize", "momentum", "dampening", "nesterov")
    groups = []
    for group in optimizer.param_groups:
        values = tuple(group[name] for name in options)
        if any(type(value) not in (int, float, bool) for value in values):
            raise NotImplementedError("GPU SGD options must be numeric or boolean scalars")
        groups.append((tuple(id(p) for p in group["params"]), values))
    return (id(optimizer), tuple(groups))


class _Capture:
    def __init__(self, training=False):
        self.graph = Graph()
        self.training = training
        self.inputs = {}
        self.tape = []
        self.previous_grads = {}
        self.input_metadata = {}
        self.grads = {}
        self.updates = []
        self.optimizer = None
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
                raise TypeError("GPU compilation requires float32 tensors")
            key = id(value)
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

    def sgd(self, optimizer, closure):
        if closure is not None:
            raise NotImplementedError("GPU SGD does not support closures")
        if optimizer is not self.optimizer or not self.did_backward or self.did_step:
            raise RuntimeError("GPU training requires one zero_grad/backward/step with the same SGD optimizer")
        self.optimizer_snapshot = _sgd_configuration(optimizer)
        self.optimizer_params = tuple(p for group in optimizer.param_groups for p in group["params"])
        seen = set()
        for group in optimizer.param_groups:
            if group["momentum"] or group["dampening"] or group["nesterov"]:
                raise NotImplementedError("GPU SGD currently supports no momentum, dampening or Nesterov")
            for parameter in group["params"]:
                if id(parameter) in seen:
                    raise ValueError("GPU SGD rejects duplicate parameters")
                seen.add(id(parameter))
                if not parameter.requires_grad:
                    continue
                entry = self.inputs.get(id(parameter))
                if entry is None or id(entry[1]) not in self.grads:
                    raise NotImplementedError("Every trainable SGD parameter must participate in the captured loss")
                leaf = entry[1]
                gradient = self.grads[id(leaf)]
                if group["maximize"]:
                    gradient = gradient * -1.0
                if group["weight_decay"]:
                    gradient = gradient + leaf._value * group["weight_decay"]
                self.updates.append((parameter, leaf._value - gradient * group["lr"]))
        if not self.updates:
            raise RuntimeError("GPU training requires at least one trainable parameter")
        self.did_step = True


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
            return (ga.sum() if not a.shape and result.shape else ga, gb.sum() if not b.shape and result.shape else gb)
        return _Tensor(self._capture, result, (a, b), backward)

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
        return _Tensor(self._capture, self._value.transpose(), (self,), lambda g: (g.transpose(),))
    @property
    def T(self): return self.transpose(0, 1)
    def linear(self, weight, bias):
        result = self @ self._capture.tensor(weight).T
        return result if bias is None else result + bias
    def relu(self):
        return _Tensor(self._capture, self._value.relu(), (self,), lambda g: (g * self._value.positive(),))
    def sum(self, dim=None, keepdim=False, dtype=None):
        if dim is not None or keepdim or dtype is not None:
            raise NotImplementedError("GPU sum currently reduces all elements")
        return _Tensor(self._capture, self._value.sum(), (self,), lambda g: (self._capture.graph.full(tuple(self.shape), 1.0) * g,))
    def mean(self, dim=None, keepdim=False, dtype=None):
        return self.sum(dim, keepdim, dtype) / self.shape.numel()
    def backward(self, gradient=None, retain_graph=False, create_graph=False):
        if gradient is not None or retain_graph or create_graph:
            raise NotImplementedError("GPU backward supports first-order scalar losses only")
        self._capture.backward(self)
    def __bool__(self):
        raise TypeError("Compiled GPU tensors cannot control Python branches")


class GPUResult:
    """A recorded call. Training commits gradients/weights after successful readback."""
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
                        optimizer_changed = _sgd_configuration(capture.optimizer) != capture.optimizer_snapshot
                    except Exception:
                        optimizer_changed = True
                    if optimizer_changed:
                        raise RuntimeError("GPU training result is stale; optimizer changed before completion")
                value = read("result", tuple(self.shape))
                gradients = [(entry[0], read("grad" + str(i), tuple(entry[0].shape))) for i, entry in enumerate(leaves)]
                updates = [(entry[0], read("weight" + str(i), tuple(entry[0].shape))) for i, entry in enumerate(capture.updates)]
                if capture.training:
                    for original, symbolic, snapshot in capture.inputs.values():
                        if original.dtype != torch.float32 or original._s is not snapshot[0] or _k.version(original._s) != snapshot[1]:
                            raise RuntimeError("GPU training result is stale; a captured tensor changed before completion")
                        if _changed(original.grad, capture.previous_grads[id(original)]):
                            raise RuntimeError("GPU training result is stale; a captured gradient changed before completion")
                        if (tuple(original.shape), original.requires_grad) != capture.input_metadata[id(original)]:
                            raise RuntimeError("GPU training result is stale; a captured tensor changed before completion")
                    for parameter, new_value in updates:
                        parameter.data = new_value
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


def compile(model=None, *, backend="zipp_gpu", training=False):
    if backend != "zipp_gpu":
        raise ValueError("The Zipp compile extension supports only backend='zipp_gpu'")
    if model is None:
        return lambda fn: compile(fn, backend=backend, training=training)
    if not callable(model):
        raise TypeError("torch.compile requires a callable or nn.Module")
    def invoke(*args, **kwargs):
        global _active
        if _active is not None:
            raise RuntimeError("Nested compiled training calls are unsupported")
        capture = _Capture(training)
        convert = lambda value: capture.tensor(value) if isinstance(value, torch.Tensor) else value
        # Eager ops look for graph tensors only while a call records.
        torch._recording(1)
        try:
            _active = capture if training else None
            with torch.enable_grad() if training else torch.no_grad():
                output = model(*[convert(value) for value in args], **{key: convert(value) for key, value in kwargs.items()})
            if not isinstance(output, _Tensor):
                raise TypeError("Compiled GPU calls must return one supported graph tensor")
            if training and not capture.did_step:
                raise RuntimeError("Compiled GPU training must call zero_grad(), backward() and SGD.step()")
            return GPUResult(output)
        finally:
            _active = None
            torch._recording(-1)
    return invoke
