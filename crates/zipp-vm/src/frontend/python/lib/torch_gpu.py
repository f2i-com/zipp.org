"""Bounded asynchronous inference and first-order training graphs.

The host executes float32 forward, backward and update nodes. A compiled call
(`compiled(x, y).submit(cb)`) records a graph per call and keeps parameters
and optimizer state on the CPU between submissions; a prepared step
(`compiled.prepare(x, y)`) records once and keeps them on the device between
steps until `sync()` copies them back. This is not TorchInductor or a CUDA
device.
"""
import math
import torch
import torch.nn.functional as _F
import _zipp_tensor as _k
from zipp_gpu import Graph, ComputeError, _f32, _host_data, _native

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
    # (Plain loops: a prepared session compares this every step.)
    groups = []
    for group in optimizer.param_groups:
        values = []
        for name in _OPTIONS[kind]:
            value = group[name]
            items = tuple(value) if name == "betas" and type(value) in (tuple, list) else (value,)
            for item in items:
                if type(item) not in _SCALAR_TYPES:
                    raise NotImplementedError("GPU optimizer options must be numeric or boolean scalars")
            values.append(items if name == "betas" else value)
        params = group["params"]
        steps = []
        ids = []
        for p in params:
            steps.append(_step_count(optimizer, p))
            ids.append(id(p))
        groups.append((tuple(ids), tuple(values), tuple(steps)))
    return (id(optimizer), kind, bool(getattr(optimizer, "_decoupled", False)), tuple(groups))


_SCALAR_TYPES = (int, float, bool)


def _optimizer_unchanged(optimizer, kind, snapshot):
    """Whether `_optimizer_configuration(optimizer, kind) == snapshot`, compared
    in place, as a prepared step checks it every step. False where the
    snapshot's own types or form differ (the caller's full comparison then
    decides, or raises), so True only when the full one would say equal."""
    groups = snapshot[3]
    param_groups = optimizer.param_groups
    count = len(groups)
    if id(optimizer) != snapshot[0] or len(param_groups) != count or bool(getattr(optimizer, "_decoupled", False)) != snapshot[2]:
        return False
    names = _OPTIONS[kind]
    state = optimizer.state
    # torch.optim's own state map, read by id as its `get` does.
    entries = state._entries if type(state).__name__ == "_ParamState" and type(getattr(state, "_entries", None)) is dict else None
    for g in range(count):
        group = param_groups[g]
        ids, values, steps = groups[g]
        for k in range(len(names)):
            value = group[names[k]]
            recorded = values[k]
            if type(value) is not type(recorded) or value != recorded:
                return False
            if type(recorded) is tuple:
                # betas: the snapshot holds scalars; so must these.
                for j in range(len(recorded)):
                    if type(value[j]) is not type(recorded[j]):
                        return False
        params = group["params"]
        if len(params) != len(ids):
            return False
        for j in range(len(ids)):
            parameter = params[j]
            if id(parameter) != ids[j]:
                return False
            if entries is not None:
                entry = entries.get(ids[j])
                entry = None if entry is None else entry[1]
            else:
                entry = state.get(parameter)
            step = 0 if entry is None else entry.get("step", 0)
            if type(step) is not int or step != steps[j]:
                return False
    return True


def _step_count(optimizer, parameter):
    state = optimizer.state.get(parameter)
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
        # Eager tensors computed from a parameter inside the step (`W.T`,
        # `(p ** 2).sum()`), re-recorded as graph operations on the
        # parameter, by id; each entry keeps its tensor alive.
        self.derived = {}
        # Input storage already in the graph, by id: another tensor over the
        # same storage (`p.detach()`, `.data`) reads the same input node, so a
        # prepared session carries it with the parameter.
        self.storages = {}
        # Whether the step drew random numbers on the CPU (see `_record`).
        self.used_random = False
        # torch's one random entry, as it was before `_record` watched it: the
        # seeds of the device's own draws (dropout masks) come from it, so
        # they follow torch.manual_seed and are not CPU draws to refuse.
        self.draw = getattr(torch, "_gen", None)
        # Eager bool tensors read as graph masks (see `mask_input`), by id.
        self.masks = {}
        # Prepared only: every tensor-kernel call made while the step recorded,
        # as (storages read, storages written), in order (see `_record`).
        self.trace = []
        # Scalar constants the version-3 operations record, one node each.
        self.constants = {}
        # Integer index tensors read as graph index inputs (see `index_input`), by id.
        self.indices = {}
        # Prepared only: the CPU random draws the step made while it recorded,
        # in order (see `_draw`). Each is drawn again on the host for every
        # step, from torch's generator, and fed.
        self.draws = []
        # While a recorded draw runs: {id(real generator): (real, stand-in)}.
        # The draw consumes a copy of the generator's state, so recording
        # leaves torch's stream where it was.
        self.redirect = None
        # Storages of the step's tensor arguments (set by `_record`).
        self.argument_storages = set()

    def derived_input(self, value):
        """Whether `value` is an eager result with a gradient path to a leaf."""
        return self.training and isinstance(value, torch.Tensor) and value.requires_grad and value._node is not None

    def seed(self, generator=None):
        """A 32-bit seed for a device `uniform` draw: one word of torch's
        generator, so compiled calls are reproducible under manual_seed and
        each call (or prepared session) draws its own masks."""
        return int(_k.to_list(_k.randint(self.draw(generator), 0, 1 << 32, 1))[0])

    def scalar(self, value):
        """The graph node of a finite scalar constant, recorded once per capture
        (keyed with the sign, so 0.0 and -0.0 stay distinct)."""
        key = (value, math.copysign(1.0, value))
        node = self.constants.get(key)
        if node is None:
            node = self.constants[key] = self.graph.tensor(value)
        return node

    def mask_input(self, value):
        """An eager bool tensor as a graph mask (0.0/1.0, converted exactly).

        Recorded once per storage and shape, checked for staleness like any
        input, and fed each step when it is a prepared step's argument."""
        if len(value.shape) > 4:
            raise NotImplementedError("Compiled GPU masks have at most four dimensions")
        entry = self.masks.get(id(value))
        if entry is None:
            for other in self.masks.values():
                if other[2][0] is value._s and tuple(other[0].shape) == tuple(value.shape):
                    return other[1]
            node = self.graph.tensor(_k.astype(value._s, "float32"), tuple(value.shape))
            entry = (value, _Tensor(self, node, requires_grad=False, mask=True), (value._s, _k.version(value._s)))
            self.masks[id(value)] = entry
        return entry[1]

    def operand(self, value):
        """A graph operand: a graph tensor, a float32 or bool eager tensor, or a finite number."""
        if isinstance(value, _Tensor):
            if value._capture is not self:
                raise ValueError("Cannot mix separate compiled GPU calls")
            return value
        if isinstance(value, torch.Tensor):
            return self.mask_input(value) if value.dtype is torch.bool else self.tensor(value)
        if isinstance(value, bool):
            value = float(value)
        if isinstance(value, (int, float)):
            if not math.isfinite(value):
                raise NotImplementedError(
                    "torch.compile records finite float32 constants only; %r cannot be a graph value "
                    "(for a masked_fill before softmax use a large finite value such as -1e9)" % (value,))
            return _Tensor(self, self.scalar(float(value)))
        raise TypeError("Expected a tensor or a number, got %s" % type(value).__name__)

    def condition(self, value, what):
        """A bool condition: a comparison result or an eager bool tensor, as in PyTorch."""
        if isinstance(value, _Tensor) and value.dtype is torch.bool:
            return self.operand(value)
        if isinstance(value, torch.Tensor) and value.dtype is torch.bool:
            return self.mask_input(value)
        if isinstance(value, (_Tensor, torch.Tensor)):
            if what == "masked_fill":
                raise RuntimeError("masked_fill_ only supports boolean masks, but got mask with dtype %s" % _dtype_name(value.dtype))
            raise RuntimeError("where expected condition to be a boolean tensor, but got a tensor with dtype %s" % _dtype_name(value.dtype).capitalize())
        raise TypeError("%s(): argument 'condition' must be Tensor, not %s" % (what, type(value).__name__))

    def tensor(self, value, row=False):
        if isinstance(value, _Tensor):
            if value._capture is not self:
                raise ValueError("Cannot mix separate compiled GPU calls")
            return value
        if isinstance(value, torch.Tensor):
            if self.derived_input(value):
                # Never a fresh leaf: its gradient would stop here instead
                # of reaching the parameter it was computed from.
                symbolic = self._replay(value)
                return symbolic.reshape(1, value.shape[0]) if row else symbolic
            if len(value.shape) > 2:
                raise NotImplementedError("Compiled GPU calls support scalar, vector and matrix tensors only")
            if value.dtype is torch.bool:
                return self.mask_input(value)
            if value.dtype != torch.float32:
                raise TypeError("GPU compilation requires float32 tensors; integer tensors are accepted only as cross_entropy class targets")
            key = id(value)
            if _resident and key in _resident:
                raise RuntimeError("Parameter is resident in a prepared GPU session; sync() and dispose() it before recording another step")
            shape = (1, value.shape[0]) if row else tuple(value.shape)
            if key not in self.inputs:
                # The graph copies the storage (one pass, no Python floats);
                # staleness is checked against storage identity and version.
                memo = self.storages.get(id(value._s))
                if memo is not None and memo[2] == shape:
                    node = memo[1]
                elif memo is not None and not value.requires_grad and torch._numel(memo[2]) == torch._numel(shape):
                    node = self.graph.reshape(memo[1], shape)
                else:
                    node = self.graph.tensor(value._s, shape)
                    if memo is None:
                        self.storages[id(value._s)] = (value._s, node, shape)
                symbolic = _Tensor(self, node, requires_grad=value.requires_grad)
                self.inputs[key] = (value, symbolic, (value._s, _k.version(value._s)))
                grad = value.grad
                # The data too: a gradient that is not the optimizer's own
                # is accumulated into on the host.
                self.previous_grads[key] = None if grad is None else _snapshot(grad) + (_k.copy(grad._s),)
                self.input_metadata[key] = (tuple(value.shape), value.requires_grad)
            original, symbolic, snapshot = self.inputs[key]
            if tuple(symbolic.shape) != shape:
                # A bias read both as a row and as a vector: one leaf, reshaped.
                return symbolic.reshape(shape)
            return symbolic
        return _Tensor(self, self.graph.tensor(value))

    # ---- eager results computed from parameters -------------------------------------------
    # A parameter the step touches with an ordinary tensor operation (`W.T`,
    # `W * 2`, `(p ** 2).sum()`, `torch.add(h, b)`'s `b * alpha`) yields an
    # eager tensor whose autograd node leads back to the parameter. That node
    # is re-recorded here as graph operations on the parameter's own input,
    # so the gradient reaches it (and a prepared session recomputes the value
    # from the resident weights each step). A node that cannot be recorded
    # exactly raises; it never becomes a separate leaf.

    def _replay(self, value):
        entry = self.derived.get(id(value))
        if entry is not None:
            return entry[1]
        node = value._node
        name = node.name
        parents = node.parents
        for i, parent in enumerate(parents):
            # Each parent's history as the node consumed it comes first in
            # its `pstate`; an in-place operation since gave it another.
            if parent is not None and node.pstate[i][0] is not parent._node:
                raise NotImplementedError("torch.compile cannot record %s on a parameter whose operand was modified in place afterwards" % name)
        operands = [None if parent is None else self.tensor(parent) for parent in parents]
        shape = tuple(value.shape)
        if name in _REPLAY_BINARY and len(operands) == 2 and None not in operands:
            a, b = operands
            result = a + b if name == "Add" else a - b if name == "Sub" else a * b if name == "Mul" else a / b
        elif name == "Pow":
            if len(parents) == 1:
                result = operands[0].square()
            else:
                exponent = parents[1]
                if exponent.requires_grad or exponent.shape.numel() != 1:
                    raise NotImplementedError("torch.compile records a parameter raised to a constant power only")
                result = operands[0] ** exponent.item()
        elif name in _REPLAY_UNARY:
            result = getattr(operands[0], _REPLAY_UNARY[name])()
        elif name == "View":
            result = operands[0].reshape(shape)
        elif name in ("clone", "ToCopy"):
            if value.dtype != torch.float32:
                raise TypeError("GPU compilation requires float32 tensors")
            result = operands[0]
        elif name in ("Sum", "Mean"):
            result = self._replay_reduction(node, parents[0], operands[0], shape, name)
        elif name == "Permute":
            result = operands[0].permute(self._replay_permutation(node, parents[0], shape))
        elif name == "Mm":
            result = operands[0] @ operands[1]
        elif name in ("Maximum", "Minimum") and len(operands) == 2 and None not in operands:
            result = _maximum(self, "maximum" if name == "Maximum" else "minimum", operands[0], operands[1])
        elif name == "Slice":
            result = self._replay_slice(node, parents[0], operands[0], shape)
        elif name == "Cat" and None not in operands:
            dims = [d for d in range(len(shape)) if all(len(p.shape) == len(shape) for p in parents)
                    and shape[d] == sum(p.shape[d] for p in parents)
                    and all(p.shape[i] == shape[i] for p in parents for i in range(len(shape)) if i != d)]
            if len(dims) != 1:
                raise NotImplementedError("torch.compile could not tell which dimension an eager cat of a parameter joined")
            result = _cat(self, operands, dims[0])
        elif name == "Index":
            raise NotImplementedError(
                "torch.compile cannot record indexing a parameter by a tensor on the CPU (W[idx]); use torch.index_select(W, 0, idx), "
                "torch.gather or F.embedding, which record on the device with the parameter's gradient")
        elif name in ("Softmax", "LogSoftmax"):
            method = "softmax" if name == "Softmax" else "log_softmax"
            with torch.no_grad():
                dims = [d for d in range(len(shape)) if torch.equal(getattr(torch, method)(parents[0], d), value)]
            if len(dims) != 1:
                raise NotImplementedError("torch.compile cannot tell which dimension an eager %s of a parameter used" % method)
            result = getattr(operands[0], method)(dims[0])
        else:
            raise NotImplementedError(
                "torch.compile cannot record the eager operation %s applied to a parameter inside a compiled training step "
                "(its gradient would not reach the parameter); apply it to graph tensors or outside the step" % name)
        if tuple(result.shape) != shape:
            raise NotImplementedError("torch.compile could not record the eager operation %s on a parameter" % name)
        self.derived[id(value)] = (value, result)
        return result

    @staticmethod
    def _probe(node, shape):
        # A linear node's backward applied to distinct values reveals which
        # elements it moved or reduced (the dims live in its closure only).
        count = torch._numel(shape)
        if count > 1 << 24:
            raise NotImplementedError("torch.compile cannot record an eager operation this large on a parameter")
        with torch.no_grad():
            probe = torch.arange(1, count + 1, dtype=torch.float32).reshape(shape)
            return probe, node.backward(probe)[0]

    def _replay_reduction(self, node, parent, operand, shape, name):
        probe, spread = self._probe(node, shape)
        dims = []
        with torch.no_grad():
            for d, size in enumerate(parent.shape):
                if size > 1 and torch.equal(spread, spread.narrow(d, 0, 1).expand(*parent.shape)):
                    dims.append(d)
        if not dims:
            return operand.reshape(shape)
        if not shape and len(dims) == sum(1 for size in parent.shape if size > 1):
            # The whole tensor: the pairwise (protocol version 1) form.
            return operand.sum() if name == "Sum" else operand.mean()
        reduced = operand.sum(dims, True) if name == "Sum" else operand.mean(dims, True)
        return reduced.reshape(shape)

    def _replay_slice(self, node, parent, operand, shape):
        """An eager basic slice of a parameter (`pos[:T]`, `W[0]`, `W[:, ::2]`):
        its backward writes a probe into zeros, which shows the parent position
        of every element; those positions must form one strided box."""
        probe, spread = self._probe(node, shape)
        count = torch._numel(shape)
        where = [None] * count
        for position, v in enumerate(_k.to_list(spread._s)):
            if v:
                where[int(v) - 1] = position
        pshape = tuple(parent.shape)
        if None in where or not pshape:
            raise NotImplementedError("torch.compile could not record an eager slice of a parameter")
        coords = [_unravel(position, pshape) for position in where]
        begin, stride, box = [], [], []
        for d in range(len(pshape)):
            seen = sorted(set(c[d] for c in coords))
            step = seen[1] - seen[0] if len(seen) > 1 else 1
            if any(b - a != step for a, b in zip(seen, seen[1:])):
                raise NotImplementedError("torch.compile could not record an eager slice of a parameter")
            begin.append(seen[0])
            stride.append(step)
            box.append(len(seen))
        if torch._numel(box) != count or any(
                tuple(b + i * t for b, i, t in zip(begin, _unravel(k, box), stride)) != tuple(c) for k, c in enumerate(coords)):
            raise NotImplementedError("torch.compile could not record an eager slice of a parameter")
        return (operand if _whole(pshape, begin, stride, box) else _slice(operand, begin, stride, box)).reshape(shape)

    def _replay_permutation(self, node, parent, shape):
        probe, moved = self._probe(node, shape)
        found = []
        with torch.no_grad():
            for dims in _permutations(len(shape)):
                if tuple(parent.shape[d] for d in dims) == shape and torch.equal(moved.permute(*dims), probe):
                    found.append(dims)
        if len(found) != 1:
            raise NotImplementedError("torch.compile could not record an eager permutation of a parameter")
        return found[0]

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
            for original, symbolic, snapshot, dtype in self.targets.values():
                if snapshot[0] is value._s and original.shape == value.shape:
                    # Another view of the same targets (`y.view(-1)` twice): one input.
                    return symbolic
            symbolic = self.graph.tensor(_k.astype(value._s, "float32"), tuple(value.shape))
            self.targets[key] = (value, symbolic, (value._s, _k.version(value._s)), value.dtype)
        return self.targets[key][1]

    def index_input(self, value, bound, what):
        """An integer index tensor as a graph index input, recorded once per
        storage and shape. The graph's only dtype is float32, so the values are
        converted exactly (indices stay below 65536) and checked here to lie in
        [0, bound); like class targets, an index read from a step argument (or
        a view of one) is fed again every prepared step."""
        if isinstance(value, _Tensor):
            raise NotImplementedError(
                "torch.compile takes %s indices from integer tensors on the CPU; an index computed on the device "
                "(an argmax, a comparison, arithmetic on graph tensors) is not supported" % what)
        if not isinstance(value, torch.Tensor):
            value = torch.tensor(value, dtype=torch.int64)
        if value.dtype.is_floating_point or value.dtype is torch.bool:
            raise IndexError("tensors used as indices must be long, int, byte or bool tensors")
        shape = tuple(value.shape)
        if len(shape) > 4 or not torch._numel(shape):
            raise NotImplementedError("torch.compile records %s indices of at least one element and at most four dimensions" % what)
        values = _k.to_list(value._s)
        low, high = min(values), max(values)
        if low < 0:
            raise NotImplementedError("torch.compile records non-negative %s indices only (got %d); add the dimension size to a negative index" % (what, low))
        if high >= bound:
            raise IndexError("index %d is out of bounds for dimension with size %d" % (high, bound))
        entry = self.indices.get(id(value))
        if entry is not None and tuple(entry[0].shape) == shape:
            return entry[1]
        for original, node, snapshot, dtype in self.indices.values():
            if snapshot[0] is value._s and tuple(original.shape) == shape:
                return node
        node = self.graph.tensor(_k.astype(value._s, "float32"), shape)
        self.indices[id(value)] = (value, node, (value._s, _k.version(value._s)), value.dtype)
        return node

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
            grad = _settle(self.grads.get(id(node)))
            if grad is None or node.pullback is None:
                continue
            for parent, value in zip(node.parents, node.pullback(grad)):
                if parent.requires_grad and value is not None:
                    old = self.grads.get(id(parent))
                    if isinstance(value, _Piece):
                        # The members of one split are consecutive on the tape,
                        # so their pieces arrive together and merge as a cat.
                        if isinstance(old, _Group) and old.group is value.group:
                            old.add(value)
                        else:
                            self.grads[id(parent)] = _Group(self.graph, value, _settle(old))
                        continue
                    old = _settle(old)
                    self.grads[id(parent)] = value if old is None else old + value
        for key, value in list(self.grads.items()):
            if isinstance(value, _Group):
                self.grads[key] = value.value()

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
        state = optimizer.state.get(parameter)
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
            had_buffer = (optimizer.state.get(parameter) or {}).get("momentum_buffer") is not None
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


_REPLAY_BINARY = ("Add", "Sub", "Mul", "Div")
# Eager autograd node names and the graph tensor method recording each.
_REPLAY_UNARY = {"Neg": "__neg__", "Exp": "exp", "Log": "log", "Tanh": "tanh", "Sigmoid": "sigmoid", "Relu": "relu",
                 "Sqrt": "sqrt", "Rsqrt": "rsqrt", "Gelu": "gelu", "Abs": "abs", "Silu": "silu"}


def _permutations(rank):
    if rank == 0:
        return [()]
    return [rest[:i] + (rank - 1,) + rest[i:] for rest in _permutations(rank - 1) for i in range(rank)]


class CompileUnsupportedError(NotImplementedError, AttributeError):
    """An operation torch.compile cannot record. Also an AttributeError, so
    `hasattr`/`getattr(x, name, default)` on a graph tensor keep working."""


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


def _dim(dim, rank):
    if isinstance(dim, bool) or not isinstance(dim, int) or not -max(rank, 1) <= dim < max(rank, 1):
        raise IndexError("Dimension out of range (expected to be in range of [%d, %d], but got %r)" % (-max(rank, 1), max(rank, 1) - 1, dim))
    return dim + rank if dim < 0 else dim


def _dims(dim, rank):
    """A reduction's dimensions, sorted and distinct; None for the whole tensor."""
    if dim is None:
        return None
    dims = tuple(dim) if isinstance(dim, (tuple, list)) else (dim,)
    if not dims:
        return None
    result = sorted(set(_dim(d, rank) for d in dims))
    if len(result) != len(dims):
        raise RuntimeError("dim appears multiple times in the list of dims")
    return result


def _shape_args(shape):
    if len(shape) == 1 and isinstance(shape[0], (tuple, list, torch.Size)):
        shape = shape[0]
    return tuple(int(d) for d in shape)


class _Tensor:
    def __init__(self, capture, value, parents=(), pullback=None, requires_grad=None, mask=False):
        self._capture = capture
        self._value = value
        self._zipp_graph = True
        self.shape = torch.Size(value.shape)
        # A comparison's result is a bool mask, as in PyTorch; on the device
        # it is float32 0.0/1.0, so arithmetic with it is exact.
        self.dtype = torch.bool if mask else torch.float32
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

    def __getattr__(self, name):
        # Only reached for what the class does not define.
        if name.startswith("__"):
            raise AttributeError(name)
        if name == "_s":
            # An eager torch function asked a graph tensor for its storage.
            raise CompileUnsupportedError("This torch function is not supported by torch.compile: it cannot record it on a compiled graph tensor "
                               "(the supported operations are listed in docs/TORCH_COMPATIBILITY.md)")
        raise CompileUnsupportedError("Tensor.%s is not supported by torch.compile" % name)

    def _binary(self, operation, other, reverse=False):
        capture = self._capture
        if (isinstance(other, torch.Tensor) and other.dtype == torch.float32 and len(self.shape) == 2
                and tuple(other.shape) == (self.shape[1],) and not capture.derived_input(other)):
            # A bias row broadcast by a ones-matmul keeps dense layers within
            # protocol version 1; other broadcasting uses the graph's own.
            other = capture.tensor(other, row=True)
            ones = _Tensor(capture, capture.graph.full((self.shape[0], 1), 1.0))
            other = ones @ other
        rhs = capture.tensor(other)
        a, b = (rhs, self) if reverse else (self, rhs)
        av, bv = a._value, b._value
        # Only an operand that takes a gradient gets a pullback node: a
        # constant or a mask operand would otherwise add nodes nothing reads.
        if operation == "div":
            result = av / bv
            def backward(g):
                return (_unbroadcast(g / bv, a) if a.requires_grad else None,
                        _unbroadcast(-(g * (result / bv)), b) if b.requires_grad else None)
            return _Tensor(capture, result, (a, b), backward)
        result = av + bv if operation == "add" else av - bv if operation == "sub" else av * bv
        def backward(g):
            ga = (g * bv if operation == "mul" else g) if a.requires_grad else None
            gb = (g * av if operation == "mul" else g * -1.0 if operation == "sub" else g) if b.requires_grad else None
            return (None if ga is None else _unbroadcast(ga, a), None if gb is None else _unbroadcast(gb, b))
        return _Tensor(capture, result, (a, b), backward)

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
        if isinstance(other, bool):
            raise TypeError("GPU division needs a number or a tensor divisor")
        if isinstance(other, (int, float)):
            # A scalar divisor stays a multiplication (protocol version 1).
            return self * (1.0 / other)
        return self._binary("div", other)
    def __rtruediv__(self, other):
        if isinstance(other, bool):
            raise TypeError("GPU division needs a number or a tensor dividend")
        return self._binary("div", other, True)
    def div(self, other): return self / other
    def __pow__(self, exponent):
        if isinstance(exponent, bool) or not isinstance(exponent, (int, float)):
            raise NotImplementedError("GPU power supports a constant numeric exponent")
        if exponent == 2:
            return self * self
        if exponent == 1:
            return self
        if exponent == 0.5:
            return self.sqrt()
        if exponent == -1:
            return 1.0 / self
        if exponent == -0.5:
            return self.rsqrt()
        raise NotImplementedError("GPU power supports the exponents 2, 1, 0.5, -1 and -0.5")
    def pow(self, exponent): return self ** exponent
    def square(self): return self * self
    # ---- comparisons, masks and selection (protocol version 3) --------------------------
    def __lt__(self, other): return _compare(self._capture, "lt", self, other)
    def __le__(self, other): return _compare(self._capture, "le", self, other)
    def __gt__(self, other): return _compare(self._capture, "gt", self, other)
    def __ge__(self, other): return _compare(self._capture, "ge", self, other)
    def __eq__(self, other):
        return False if other is None else _compare(self._capture, "eq", self, other)
    def __ne__(self, other):
        return True if other is None else _compare(self._capture, "ne", self, other)
    def __hash__(self):
        return id(self)
    def eq(self, other): return _compare(self._capture, "eq", self, other)
    def ne(self, other): return _compare(self._capture, "ne", self, other)
    def lt(self, other): return _compare(self._capture, "lt", self, other)
    def le(self, other): return _compare(self._capture, "le", self, other)
    def gt(self, other): return _compare(self._capture, "gt", self, other)
    def ge(self, other): return _compare(self._capture, "ge", self, other)
    greater, greater_equal, less, less_equal, not_equal = gt, ge, lt, le, ne
    def where(self, condition, other): return _where(self._capture, condition, self, other, "where")
    def masked_fill(self, mask, value): return _masked_fill(self._capture, self, mask, value)
    def maximum(self, other): return _maximum(self._capture, "maximum", self, other)
    def minimum(self, other): return _maximum(self._capture, "minimum", self, other)
    def clamp(self, min=None, max=None): return _clamp(self._capture, self, min, max)
    clip = clamp
    def clamp_min(self, min): return _clamp(self._capture, self, min, None)
    def clamp_max(self, max): return _clamp(self._capture, self, None, max)
    def logical_not(self): return _logical(self._capture, "not", self, None)
    def logical_and(self, other): return _logical(self._capture, "and", self, other)
    def logical_or(self, other): return _logical(self._capture, "or", self, other)
    def logical_xor(self, other): return _logical(self._capture, "xor", self, other)
    def __invert__(self):
        if self.dtype is not torch.bool:
            raise TypeError("~ (bitwise not) of a float compiled graph tensor is not supported; it applies to comparison masks")
        return _logical(self._capture, "not", self, None)
    def _bitwise(self, op, other):
        if self.dtype is not torch.bool or not (isinstance(other, bool) or getattr(other, "dtype", None) is torch.bool):
            raise TypeError("&, | and ^ of compiled graph tensors apply to comparison masks (bool) only")
        return _logical(self._capture, op, self, other)
    def __and__(self, other): return self._bitwise("and", other)
    def __rand__(self, other): return self._bitwise("and", other)
    def __or__(self, other): return self._bitwise("or", other)
    def __ror__(self, other): return self._bitwise("or", other)
    def __xor__(self, other): return self._bitwise("xor", other)
    def __rxor__(self, other): return self._bitwise("xor", other)
    def bool(self):
        # A mask already is one; anything else becomes `!= 0`, as in PyTorch.
        return self if self.dtype is torch.bool else _compare(self._capture, "ne", self, 0.0)

    def __matmul__(self, other):
        b = self._capture.tensor(other)
        ra, rb = len(self.shape), len(b.shape)
        if ra == 1 or rb == 1:
            # A vector is a one-row (left) or one-column (right) matrix.
            if ra not in (1, 2) or rb not in (1, 2):
                raise NotImplementedError("GPU matmul supports vectors and matrices")
            left = self.reshape(1, self.shape[0]) if ra == 1 else self
            right = b.reshape(b.shape[0], 1) if rb == 1 else b
            out = left @ right
            return out.reshape(tuple(out.shape[:1] if ra == 2 else ()) + tuple(out.shape[1:] if rb == 2 else ()))
        if ra != 2 or rb != 2:
            raise NotImplementedError("GPU matmul supports vectors and matrices")
        a = self._value
        return _Tensor(self._capture, a @ b._value, (self, b), lambda g: (g @ b._value.transpose(), a.transpose() @ g))
    def __rmatmul__(self, other): return self._capture.tensor(other) @ self
    def matmul(self, other): return self @ other
    def mm(self, other): return self @ other
    def linear(self, weight, bias):
        result = self @ self._capture.tensor(weight).T
        return result if bias is None else result + bias

    # ---- shape: views, permutations and transposes ----------------------------------
    def size(self, dim=None):
        return self.shape if dim is None else self.shape[_dim(dim, len(self.shape))]
    def dim(self): return len(self.shape)
    @property
    def ndim(self): return len(self.shape)
    def numel(self): return self.shape.numel()
    def __len__(self):
        if not self.shape:
            raise TypeError("len() of a 0-d tensor")
        return self.shape[0]
    def detach(self):
        # A stop-gradient: the same value, no path back to its sources.
        return _Tensor(self._capture, self._value, requires_grad=False)
    def float(self):
        # A mask read as float32 is the same 0.0/1.0 values, without a copy.
        return self if self.dtype is torch.float32 else _Tensor(self._capture, self._value, requires_grad=False)
    def contiguous(self): return self
    def clone(self): return self
    def to(self, *args, **kwargs):
        target = None
        for value in list(args) + list(kwargs.values()):
            if value is torch.float32 or value is torch.bool:
                target = value
            elif value != "cpu" and not (isinstance(value, bool) or value is None):
                raise NotImplementedError("A compiled graph tensor is float32 (bool for comparison masks) on the host GPU; .to(%r) is not supported" % (value,))
        if target is torch.bool:
            return self.bool()
        return self.float() if target is torch.float32 else self
    def type_as(self, other):
        return self.to(other.dtype)

    def reshape(self, *shape):
        shape = _shape_args(shape)
        before = tuple(self.shape)
        value = self._capture.graph.reshape(self._value, shape)
        if tuple(value.shape) == before:
            return self
        return self._unary(value, lambda g: (g.reshape(before),))
    view = reshape
    def view_as(self, other): return self.reshape(tuple(other.shape))
    reshape_as = view_as
    def flatten(self, start_dim=0, end_dim=-1):
        rank = len(self.shape)
        if rank == 0:
            return self.reshape(1)
        s, e = _dim(start_dim, rank), _dim(end_dim, rank)
        if s > e:
            raise RuntimeError("flatten() has invalid args: start_dim cannot come after end_dim")
        return self.reshape(tuple(self.shape[:s]) + (torch._numel(self.shape[s:e + 1]),) + tuple(self.shape[e + 1:]))
    def unsqueeze(self, dim):
        d = _dim(dim, len(self.shape) + 1)
        return self.reshape(tuple(self.shape[:d]) + (1,) + tuple(self.shape[d:]))
    def squeeze(self, dim=None):
        rank = len(self.shape)
        dims = range(rank) if dim is None else [_dim(d, rank) for d in (dim if isinstance(dim, (tuple, list)) else (dim,))]
        drop = set(d for d in dims if self.shape[d] == 1)
        return self.reshape(tuple(size for d, size in enumerate(self.shape) if d not in drop))
    def permute(self, *dims):
        dims = _shape_args(dims)
        rank = len(self.shape)
        dims = tuple(_dim(d, rank) for d in dims)
        if sorted(dims) != list(range(rank)):
            raise RuntimeError("permute(): dims must be a permutation of the tensor's dimensions")
        if dims == tuple(range(rank)):
            return self
        if rank == 2:
            return self._unary(self._value.transpose(), lambda g: (g.transpose(),))
        inverse = [0] * rank
        for i, d in enumerate(dims):
            inverse[d] = i
        return self._unary(self._value.permute(dims), lambda g: (g.permute(inverse),))
    def transpose(self, dim0, dim1):
        rank = len(self.shape)
        dims = list(range(rank))
        i, j = _dim(dim0, rank), _dim(dim1, rank)
        dims[i], dims[j] = dims[j], dims[i]
        return self.permute(dims)
    swapaxes = transpose
    def t(self):
        if len(self.shape) > 2:
            raise RuntimeError("t() expects a tensor with <= 2 dimensions")
        return self.transpose(0, 1) if len(self.shape) == 2 else self
    @property
    def T(self): return self.permute(tuple(reversed(range(len(self.shape)))))

    # ---- element selection (protocol version 4) ---------------------------------------
    def __getitem__(self, key):
        return _getitem(self, key)
    def select(self, dim, index):
        d = _dim(dim, len(self.shape))
        return self[(slice(None),) * d + (_index_of(index),)]
    def narrow(self, dim, start, length):
        d = _dim(dim, len(self.shape))
        size = self.shape[d]
        start, length = _index_of(start), _index_of(length)
        if start < 0:
            start += size
        if start < 0 or length < 0 or start + length > size:
            raise RuntimeError("start (%d) + length (%d) exceeds dimension size (%d)." % (start, length, size))
        return self[(slice(None),) * d + (slice(start, start + length),)]
    def split(self, split_size_or_sections, dim=0):
        d = _dim(dim, len(self.shape))
        n = self.shape[d]
        if isinstance(split_size_or_sections, int):
            size = split_size_or_sections
            if size <= 0:
                raise RuntimeError("split_size can only be 0 if dimension size is 0, but got dimension size of %d" % n)
            sizes = [size] * (n // size) + ([n % size] if n % size else [])
        else:
            sizes = [int(v) for v in split_size_or_sections]
            if sum(sizes) != n:
                raise RuntimeError("split_with_sizes expects split_sizes to sum exactly to %d (input tensor's size at dimension %d), but got split_sizes=%s" % (n, d, sizes))
        return _split(self, sizes, d)
    def split_with_sizes(self, split_sizes, dim=0):
        return self.split(list(split_sizes), dim)
    def chunk(self, chunks, dim=0):
        if chunks <= 0:
            raise RuntimeError("chunk expects `chunks` to be greater than 0, got: %d" % chunks)
        d = _dim(dim, len(self.shape))
        return self.split(-(-self.shape[d] // chunks), d)
    def unbind(self, dim=0):
        d = _dim(dim, len(self.shape))
        kept = tuple(size for i, size in enumerate(self.shape) if i != d)
        return tuple(piece.reshape(kept) for piece in _split(self, [1] * self.shape[d], d))
    def index_select(self, dim, index):
        return _index_select(self, dim, index)
    def gather(self, dim, index, sparse_grad=False):
        return _gather(self, dim, index)
    def take_along_dim(self, indices, dim=None):
        if dim is None:
            return self.reshape(-1).gather(0, indices.reshape(-1))
        return self.gather(dim, indices)
    def flip(self, *dims):
        return _flip(self, _shape_args(dims))
    def fliplr(self): return _flip(self, (1,))
    def flipud(self): return _flip(self, (0,))

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
    def sqrt(self):
        s = self._value.sqrt()
        return self._unary(s, lambda g: (g / (s * 2.0),))
    def rsqrt(self):
        r = 1.0 / self._value.sqrt()
        return self._unary(r, lambda g: (g * ((r * (r * r)) * -0.5),))
    def reciprocal(self): return 1.0 / self
    def abs(self):
        # relu(x) + relu(-x) is |x| exactly; the gradient is sign(x), 0 at 0.
        x = self._value
        return self._unary(x.relu() + (-x).relu(), lambda g: (g * (x.positive() - (-x).positive()),))
    def silu(self):
        return self * self.sigmoid()
    def softmax(self, dim=-1, dtype=None):
        return self._softmax("softmax", dim, dtype)
    def log_softmax(self, dim=-1, dtype=None):
        return self._softmax("log_softmax", dim, dtype)
    def _softmax(self, name, dim, dtype):
        if dtype is not None and dtype != torch.float32:
            raise NotImplementedError("GPU %s computes float32 only" % name)
        rank = len(self.shape)
        if not rank:
            raise NotImplementedError("GPU %s needs a tensor with at least one dimension" % name)
        axis = _dim(dim, rank)
        if axis != rank - 1:
            # The protocol normalizes the last axis: move `dim` there and back.
            return getattr(self.transpose(axis, -1), name)(-1).transpose(axis, -1)
        if name == "softmax":
            s = self._value.softmax()
            return self._unary(s, lambda g: (s * (g - (g * s).sum(-1, keepdim=True)),))
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
        dims = _dims(dim, len(shape))
        if dims is None:
            if not keepdim and op == "sum":
                # Whole-tensor sum, the protocol version 1 form (mean is sum / n).
                return self._unary(self._value.sum(), lambda g: (graph.full(shape, 1.0) * g,))
            if not keepdim:
                return self.sum() / self.shape.numel()
            dims = list(range(len(shape)))
        kept = tuple(1 if i in dims else d for i, d in enumerate(shape))
        count = torch._numel([shape[d] for d in dims])
        if len(dims) == 1 and not (dim is None and keepdim):
            value = self._value.sum(dims[0], keepdim) if op == "sum" else self._value.mean(dims[0], keepdim)
        elif dim is None:
            value = self._value.sum(None, True) if op == "sum" else self._value.mean(None, True)
        else:
            # Several dimensions: one axis at a time, then the mean's one division.
            value = self._value
            for d in reversed(dims):
                value = value.sum(d, True)
            if op == "mean":
                value = value / float(count)
            if not keepdim:
                value = value.reshape(tuple(d for i, d in enumerate(shape) if i not in dims))
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
                    for original, symbolic, snapshot, dtype in capture.indices.values():
                        if original.dtype is not dtype or original._s is not snapshot[0] or _k.version(original._s) != snapshot[1]:
                            raise RuntimeError("GPU training result is stale; a captured index changed before completion")
                    for original, symbolic, snapshot in capture.masks.values():
                        if original.dtype is not torch.bool or original._s is not snapshot[0] or _k.version(original._s) != snapshot[1]:
                            raise RuntimeError("GPU training result is stale; a captured mask changed before completion")
                    for parameter, key, snapshot in capture.state_snapshots:
                        state = capture.optimizer.state.get(parameter)
                        if _changed(None if state is None else state.get(key), snapshot):
                            raise RuntimeError("GPU training result is stale; optimizer state changed before completion")
                    for parameter, new_value in updates:
                        parameter.data = new_value
                    for parameter, key, new_value in states:
                        capture.optimizer.state[parameter][key] = new_value
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


# ---- comparisons, selection, clamping and dropout on graph tensors (protocol version 3) ----
# Each records graph operations and, where PyTorch defines one, the same
# gradient: `where` routes it to the branch taken (zero to the other), clamp
# passes it where lo <= x <= hi (hardtanh and relu6 strictly inside), maximum
# and minimum split a tie evenly, comparisons have none, and dropout scales it
# by the forward's own mask.

def _dtype_name(dtype):
    return "float" if dtype is torch.float32 else "bool" if dtype is torch.bool else str(dtype).replace("torch.", "")


def _capture_of(*values):
    for value in values:
        if isinstance(value, _Tensor):
            return value._capture
    return None


def _mask(capture, value):
    return _Tensor(capture, value, requires_grad=False, mask=True)


def _compare(capture, op, a, b):
    ta, tb = capture.operand(a), capture.operand(b)
    return _mask(capture, capture.graph._binary(op, ta._value, tb._value))


def _truth(capture, value):
    """`value != 0` as a 0/1 graph value (a mask is already one)."""
    t = capture.operand(value)
    return t._value if t.dtype is torch.bool else capture.graph._binary("ne", t._value, capture.scalar(0.0))


def _logical(capture, op, a, b):
    graph = capture.graph
    x = _truth(capture, a)
    if op == "not":
        return _mask(capture, graph._binary("eq", x, capture.scalar(0.0)))
    y = _truth(capture, b)
    # On 0/1 values: and is a product, or a maximum, xor an inequality; all exact.
    return _mask(capture, graph._binary("mul" if op == "and" else "maximum" if op == "or" else "ne", x, y))


def _where(capture, condition, a, b, what):
    c = capture.condition(condition, what)
    ta, tb = capture.operand(a), capture.operand(b)
    graph, cv, zero = capture.graph, c._value, capture.scalar(0.0)

    def backward(grad):
        # PyTorch: where(condition, grad, 0) and where(condition, 0, grad).
        return (_unbroadcast(graph.where(cv, grad, zero), ta) if ta.requires_grad else None,
                _unbroadcast(graph.where(cv, zero, grad), tb) if tb.requires_grad else None)
    return _Tensor(capture, graph.where(cv, ta._value, tb._value), (ta, tb), backward,
                   mask=ta.dtype is torch.bool and tb.dtype is torch.bool)


def _masked_fill(capture, x, mask, value):
    t = capture.operand(x)
    if isinstance(value, (torch.Tensor, _Tensor)) and len(value.shape):
        raise RuntimeError("masked_fill_ only supports a 0-dimensional value tensor, but got tensor with %d dimension(s)." % len(value.shape))
    out = _where(capture, mask, value, t, "masked_fill")
    if tuple(out.shape) != tuple(t.shape):
        raise RuntimeError("output with shape %s doesn't match the broadcast shape %s" % (list(t.shape), list(out.shape)))
    return out


def _maximum(capture, op, a, b):
    ta, tb = capture.operand(a), capture.operand(b)
    graph, av, bv, zero = capture.graph, ta._value, tb._value, capture.scalar(0.0)

    def backward(grad):
        # PyTorch: where(a == b, grad / 2, grad), zero where the other side wins.
        tie = graph.where(graph._binary("eq", av, bv), grad * capture.scalar(0.5), grad)
        ga = gb = None
        if ta.requires_grad:
            ga = _unbroadcast(graph.where(graph._binary("lt" if op == "maximum" else "gt", av, bv), zero, tie), ta)
        if tb.requires_grad:
            gb = _unbroadcast(graph.where(graph._binary("gt" if op == "maximum" else "lt", av, bv), zero, tie), tb)
        return ga, gb
    return _Tensor(capture, graph._binary(op, av, bv), (ta, tb), backward,
                   mask=ta.dtype is torch.bool and tb.dtype is torch.bool)


def _clamp(capture, x, lo, hi, exclusive=False):
    """min(max(x, lo), hi) with scalar or tensor bounds.

    The gradient reaches x where lo <= x <= hi (clamp) or lo < x < hi
    (`exclusive`: hardtanh, relu6); a tensor bound gets it where x lies beyond
    it, the lower one only while lo < hi -- PyTorch's clamp_backward_min_max."""
    if lo is None and hi is None:
        raise RuntimeError("torch.clamp: At least one of 'min' or 'max' must not be None")
    t = capture.operand(x)
    graph, xv, zero = capture.graph, t._value, capture.scalar(0.0)
    low = None if lo is None else capture.operand(lo)
    high = None if hi is None else capture.operand(hi)
    value = xv
    if low is not None:
        value = graph._binary("maximum", value, low._value)
    if high is not None:
        value = graph._binary("minimum", value, high._value)
    parents = (t,) + tuple(bound for bound in (low, high) if bound is not None)

    def backward(grad):
        above_low = None if low is None else graph._binary("gt" if exclusive else "ge", xv, low._value)
        below_high = None if high is None else graph._binary("lt" if exclusive else "le", xv, high._value)
        inside = below_high if above_low is None else above_low if below_high is None else above_low * below_high
        result = [_unbroadcast(graph.where(inside, grad, zero), t) if t.requires_grad else None]
        if low is not None:
            if low.requires_grad:
                taken = graph._binary("lt", xv, low._value)
                if high is not None:
                    taken = taken * graph._binary("lt", low._value, high._value)
                result.append(_unbroadcast(graph.where(taken, grad, zero), low))
            else:
                result.append(None)
        if high is not None:
            if high.requires_grad:
                taken = graph._binary("gt", xv, high._value)
                if low is not None:
                    taken = graph._binary("maximum", taken, graph._binary("lt", high._value, low._value))
                result.append(_unbroadcast(graph.where(taken, grad, zero), high))
            else:
                result.append(None)
        return tuple(result)
    return _Tensor(capture, value, parents, backward)


def _broadcast_shapes(a, b):
    rank = max(len(a), len(b))
    a, b = (1,) * (rank - len(a)) + tuple(a), (1,) * (rank - len(b)) + tuple(b)
    if any(x != y and x != 1 and y != 1 for x, y in zip(a, b)):
        raise RuntimeError("The size of tensor a (%s) must match the size of tensor b (%s)" % (list(a), list(b)))
    return tuple(max(x, y) for x, y in zip(a, b))


def _hardtanh(capture, x, lo, hi):
    if lo > hi:
        raise ValueError("min_val cannot be greater than max_val")
    return _clamp(capture, x, float(lo), float(hi), exclusive=True)


def _leaky_relu(capture, x, slope):
    t = capture.operand(x)
    graph, xv = capture.graph, t._value
    positive = graph._binary("gt", xv, capture.scalar(0.0))
    # PyTorch: x > 0 ? x : x * slope, and grad > 0 ? grad : grad * slope by the same test.
    return _Tensor(capture, graph.where(positive, xv, xv * slope), (t,),
                   lambda grad: (graph.where(positive, grad, grad * slope),))


def _dropout(capture, x, p, training, feature_rank=None, name="dropout"):
    """Dropout with its mask drawn on the device: `uniform(shape, seed, step) >= p`.

    The seed is one word of torch's generator; a prepared session advances the
    step every step, so each step has a fresh mask. Kept elements scale by
    float32(1 / float32(1 - p)) and the gradient by the same mask and scale,
    as PyTorch's CPU dropout multiplies by bernoulli(1 - p) / (1 - p). The
    mask is not PyTorch's random stream (nor eager Zipp's)."""
    if p < 0.0 or p > 1.0:
        raise ValueError("dropout probability has to be between 0 and 1, but got %s" % p)
    if not training or p == 0:
        return x
    t = capture.operand(x)
    if p == 1:
        return t * 0.0
    shape = tuple(t.shape)
    if feature_rank is not None:
        rank = len(shape)
        if rank not in (feature_rank - 1, feature_rank):
            raise RuntimeError("%s: Expected %dD or %dD input, but received a %dD input." % (name, feature_rank - 1, feature_rank, rank))
        lead = 2 if rank == feature_rank else 1
        shape = shape[:lead] + (1,) * (rank - lead)
    graph = capture.graph
    keep = graph.uniform(shape, capture.seed(), 1) >= capture.scalar(_f32(float(p)))
    noise = keep * _f32(1.0 / _f32(1.0 - p))
    return _Tensor(capture, t._value * noise, (t,), lambda grad: (grad * noise,))


def _uniform_like(capture, x, generator=None, dtype=None):
    if dtype is not None and dtype is not torch.float32:
        raise NotImplementedError("A compiled rand_like draws float32 only")
    return _Tensor(capture, capture.graph.uniform(tuple(x.shape), capture.seed(generator), 1), requires_grad=False)


def _bernoulli(capture, x, p=None, generator=None):
    t = capture.operand(x)
    draw = capture.graph.uniform(tuple(t.shape), capture.seed(generator), 1)
    # 1.0 with probability p (the input, or `p`), as float32: PyTorch keeps the input's dtype.
    return _Tensor(capture, draw < (t._value if p is None else float(p)), requires_grad=False)


# ---- element selection on graph tensors (protocol version 4) ----------------------------------
# Slicing records `slice` and its gradient writes the box into zeros
# (`slice_scatter`); a tensor index records `index_select` or `gather` with the
# index as a graph input, and their gradients accumulate with `index_add` or
# `scatter_add` in ascending index position, PyTorch's CPU order. A split's
# members' gradients are written into one another (PyTorch's split backward is
# a cat) rather than added as zero-padded copies, so signed zeros survive as
# they do in PyTorch; separately taken slices add, as they do there.

class _Piece:
    """One split member's gradient: `grad` belongs in the box (begin, stride)
    of zeros shaped like the source."""
    def __init__(self, group, grad, begin, stride, shape):
        self.group = group
        self.grad = grad
        self.begin = begin
        self.stride = stride
        self.shape = shape


class _Group:
    """A split's pieces received so far, written into zeros, plus whatever
    gradient the source had before them."""
    def __init__(self, graph, piece, prior):
        self.graph = graph
        self.group = piece.group
        self.prior = prior
        self.merged = graph.slice_scatter(graph.zeros(piece.shape), piece.grad, piece.begin, piece.stride)

    def add(self, piece):
        self.merged = self.graph.slice_scatter(self.merged, piece.grad, piece.begin, piece.stride)

    def value(self):
        return self.merged if self.prior is None else self.prior + self.merged


def _settle(value):
    return value.value() if isinstance(value, _Group) else value


def _unravel(position, shape):
    out = []
    for size in reversed(shape):
        out.append(position % size)
        position //= size
    return tuple(reversed(out))


def _index_of(value):
    if isinstance(value, bool) or not (isinstance(value, int) or hasattr(value, "__index__")):
        raise TypeError("expected an integer, got %s" % type(value).__name__)
    return value if isinstance(value, int) else value.__index__()


def _slice(t, begin, stride, box, group=None):
    """The strided box of a graph tensor; its gradient is the box written into zeros."""
    capture = t._capture
    graph = capture.graph
    shape = tuple(t.shape)
    begin, stride, box = list(begin), list(stride), list(box)
    if group is None:
        pullback = lambda g: (graph.slice_scatter(graph.zeros(shape), g, begin, stride),)
    else:
        pullback = lambda g: (_Piece(group, g, begin, stride, shape),)
    return _Tensor(capture, graph.slice(t._value, begin, stride, box), (t,), pullback, mask=t.dtype is torch.bool)


def _whole(shape, begin, stride, box):
    return all(b == 0 for b in begin) and all(t == 1 for t in stride) and tuple(box) == tuple(shape)


def _split(t, sizes, d):
    rank = len(t.shape)
    if any(size <= 0 for size in sizes):
        raise NotImplementedError("torch.compile cannot record an empty split piece (graph dimensions are positive)")
    if len(sizes) == 1:
        return (t,)
    group, out, at = object(), [], 0
    for size in sizes:
        out.append(_slice(t, [at if i == d else 0 for i in range(rank)], [1] * rank,
                          [size if i == d else n for i, n in enumerate(t.shape)], group))
        at += size
    return tuple(out)


def _slice_bound(value):
    return value if value is None else _index_of(value)


def _getitem(t, key):
    """Basic indexing (integers, slices with positive steps, None, ...) records
    one `slice` and a reshape; one integer tensor (or list) index records
    `index_select` along its dimension, and x[torch.arange(n), idx] of a
    matrix records `gather`. Masks and device-computed indices are refused."""
    key = key if isinstance(key, tuple) else (key,)
    rank = len(t.shape)
    if sum(1 for k in key if k is Ellipsis) > 1:
        raise IndexError("an index can only have a single ellipsis ('...')")
    consumed = sum(1 for k in key if k is not None and k is not Ellipsis and not isinstance(k, bool))
    if consumed > rank:
        raise IndexError("too many indices for tensor of dimension %d" % rank)
    items = []
    for k in key:
        if k is Ellipsis:
            items.extend([slice(None)] * (rank - consumed))
        else:
            items.append(k)
    items.extend([slice(None)] * (rank - sum(1 for k in items if k is not None and not isinstance(k, bool))))
    begin, stride, box, plan, advanced, integer, axis = [], [], [], [], [], False, 0
    for k in items:
        if k is None or k is True:
            plan.append(None)
            continue
        if k is False:
            raise NotImplementedError("torch.compile cannot index with False: it selects no elements, and graph dimensions are positive")
        size = t.shape[axis]
        if isinstance(k, slice):
            start, stop, step = _slice_bound(k.start), _slice_bound(k.stop), _slice_bound(k.step)
            if step is not None and step <= 0:
                raise ValueError("step must be greater than zero")
            start, stop, step = slice(start, stop, step).indices(size)
            count = len(range(start, stop, step))
            if not count:
                raise NotImplementedError("torch.compile cannot record a slice that selects no elements (graph dimensions are positive)")
            begin.append(start)
            stride.append(step)
            box.append(count)
            plan.append(axis)
        elif isinstance(k, _Tensor):
            raise NotImplementedError(
                "torch.compile cannot index with a boolean mask: it selects a data-dependent number of elements, which a graph shape cannot hold"
                if k.dtype is torch.bool else
                "torch.compile takes tensor indices from integer tensors on the CPU; an index computed on the device is not supported")
        elif isinstance(k, (torch.Tensor, list, tuple)):
            if (k.dtype is torch.bool) if isinstance(k, torch.Tensor) else (len(k) > 0 and all(isinstance(v, bool) for v in k)):
                raise NotImplementedError("torch.compile cannot index with a boolean mask: it selects a data-dependent number of elements, which a graph shape cannot hold")
            index = k if isinstance(k, torch.Tensor) else torch.tensor(k, dtype=torch.int64)
            begin.append(0)
            stride.append(1)
            box.append(size)
            plan.append(("index", axis, index))
            advanced.append((axis, index))
        else:
            k = _index_of(k)
            if not -size <= k < size:
                raise IndexError("index %d is out of bounds for dimension %d with size %d" % (k, axis, size))
            begin.append(k % size)
            stride.append(1)
            box.append(1)
            integer = True
        axis += 1
    out = t if _whole(t.shape, begin, stride, box) else _slice(t, begin, stride, box)
    if len(advanced) == 2 and rank == 2 and not integer and plan == [("index", 0, advanced[0][1]), ("index", 1, advanced[1][1])]:
        # x[torch.arange(n), idx] on an [n, C] matrix: one element per row.
        rows, cols = advanced[0][1], advanced[1][1]
        n = t.shape[0]
        if (tuple(rows.shape) == (n,) and tuple(cols.shape) == (n,) and not rows.dtype.is_floating_point
                and id(rows._s) not in t._capture.argument_storages and _k.to_list(rows._s) == list(range(n))):
            return _gather(out, 1, cols.reshape(n, 1)).reshape(n)
    if len(advanced) > 1:
        raise NotImplementedError("torch.compile records one tensor index per indexing (or x[torch.arange(n), idx] of a matrix); "
                                  "index in separate steps, or use gather")
    shape = []
    if advanced:
        if integer:
            raise NotImplementedError("torch.compile does not combine integer and tensor indices in one indexing; index in two steps (x[i][idx])")
        axis, index = advanced[0]
        out = _index_select(out, axis, index)
    for entry in plan:
        if entry is None:
            shape.append(1)
        elif isinstance(entry, tuple):
            shape.extend(entry[2].shape)
        else:
            shape.append(out.shape[entry])
    return out if tuple(shape) == tuple(out.shape) else out.reshape(tuple(shape))


def _index_select(t, dim, index):
    capture = t._capture
    graph = capture.graph
    rank = len(t.shape)
    if not rank:
        raise NotImplementedError("torch.compile records index_select of a tensor with at least one dimension")
    d = _dim(dim, rank)
    node = capture.index_input(index, t.shape[d], "index_select")
    flat = node if len(node.shape) == 1 else graph.reshape(node, (torch._numel(node.shape),))
    shape = tuple(t.shape)
    return _Tensor(capture, graph.index_select(t._value, d, flat), (t,),
                   lambda g: (graph.index_add(graph.zeros(shape), d, flat, g),), mask=t.dtype is torch.bool)


def _gather(t, dim, index):
    capture = t._capture
    graph = capture.graph
    rank = len(t.shape)
    if not rank:
        raise NotImplementedError("torch.compile records gather of a tensor with at least one dimension")
    d = _dim(dim, rank)
    if isinstance(index, torch.Tensor) and index.dtype is not torch.int64:
        raise RuntimeError("gather(): Expected dtype int64 for index")
    if len(getattr(index, "shape", ())) != rank:
        raise RuntimeError("Index tensor must have the same number of dimensions as input tensor")
    for i in range(rank):
        if i != d and index.shape[i] > t.shape[i]:
            raise RuntimeError("Size does not match at dimension %d expected index %s to be smaller than self %s apart from dimension %d"
                               % (i, list(index.shape), list(t.shape), d))
    node = capture.index_input(index, t.shape[d], "gather")
    shape = tuple(t.shape)
    return _Tensor(capture, graph.gather(t._value, d, node), (t,),
                   lambda g: (graph.scatter_add(graph.zeros(shape), d, node, g),), mask=t.dtype is torch.bool)


def _flip(t, dims):
    rank = len(t.shape)
    flipped = [_dim(d, rank) for d in dims]
    for d in flipped:
        if flipped.count(d) > 1:
            raise RuntimeError("dim %d appears multiple times in the list of dims" % d)
    begin = [n - 1 if i in flipped else 0 for i, n in enumerate(t.shape)]
    stride = [-1 if i in flipped else 1 for i in range(rank)]
    if all(t.shape[i] == 1 for i in flipped):
        return t
    capture = t._capture
    graph = capture.graph
    box = list(t.shape)
    return _Tensor(capture, graph.slice(t._value, begin, stride, box), (t,),
                   lambda g: (graph.slice(g, begin, stride, box),), mask=t.dtype is torch.bool)


def _cat(capture, tensors, dim):
    """Concatenation: each piece written into zeros (`slice_scatter`); the
    gradient of each piece is its slice of the result's."""
    items = []
    for i, value in enumerate(tensors):
        if not isinstance(value, (_Tensor, torch.Tensor)):
            raise TypeError("expected Tensor as element %d in argument 0, but got %s" % (i, type(value).__name__))
        items.append(capture.operand(value))
    if not items:
        raise RuntimeError("torch.cat(): expected a non-empty list of Tensors")
    rank = len(items[0].shape)
    if not rank:
        raise RuntimeError("zero-dimensional tensor (at position 0) cannot be concatenated")
    d = _dim(dim, rank)
    for i, item in enumerate(items):
        if len(item.shape) != rank:
            raise RuntimeError("Tensors must have same number of dimensions: got %d and %d" % (rank, len(item.shape)))
        for j in range(rank):
            if j != d and item.shape[j] != items[0].shape[j]:
                raise RuntimeError("Sizes of tensors must match except in dimension %d. Expected size %d but got size %d for tensor number %d in the list."
                                   % (d, items[0].shape[j], item.shape[j], i))
    mask = all(item.dtype is torch.bool for item in items)
    if len(items) == 1:
        return items[0]
    graph = capture.graph
    shape = [sum(item.shape[d] for item in items) if j == d else n for j, n in enumerate(items[0].shape)]
    out, starts, at = graph.zeros(tuple(shape)), [], 0
    for item in items:
        begin = [at if j == d else 0 for j in range(rank)]
        out = graph.slice_scatter(out, item._value, begin, [1] * rank)
        starts.append(begin)
        at += item.shape[d]

    def backward(g):
        return tuple(graph.slice(g, begin, [1] * rank, list(item.shape)) if item.requires_grad else None
                     for item, begin in zip(items, starts))
    return _Tensor(capture, out, tuple(items), backward, mask=mask)


def _stack(capture, tensors, dim):
    items = [capture.operand(value) for value in tensors]
    if not items:
        raise RuntimeError("stack expects a non-empty TensorList")
    for i, item in enumerate(items):
        if tuple(item.shape) != tuple(items[0].shape):
            raise RuntimeError("stack expects each tensor to be equal size, but got %s at entry 0 and %s at entry %d"
                               % (list(items[0].shape), list(item.shape), i))
    d = _dim(dim, len(items[0].shape) + 1)
    return _cat(capture, [item.unsqueeze(d) for item in items], d)


def _graph_select(capture, input, index):
    """Whether an index_select/gather call records on the graph: a graph
    tensor is involved, the source is a trainable tensor of a training step
    (its gradient must reach it), or the index is read from a step argument or
    a random draw (a prepared session feeds it every step)."""
    if capture is None:
        return False
    if isinstance(input, _Tensor) or isinstance(index, _Tensor):
        return True
    if not isinstance(input, torch.Tensor) or input.dtype is not torch.float32:
        return False
    if capture.training and torch.is_grad_enabled() and input.requires_grad:
        return True
    return isinstance(index, torch.Tensor) and (id(index._s) in capture.argument_storages
                                                or any(entry["storage"] is index._s for entry in capture.draws))


# ---- CPU random draws inside a prepared step ------------------------------------------------------
# A draw records as a feed: while the step records, it draws from a copy of
# the generator's state (torch's stream does not move), and every executed
# step draws again on the host -- the same call, in the same order, from the
# real generator -- and feeds the values. A float32 draw becomes a graph input
# (so arithmetic on it records on the device); an integer draw stays a CPU
# tensor, fed wherever the step reads it as class targets or an index.

def _draw(capture, redraw):
    capture.redirect = {}
    try:
        return redraw()
    finally:
        capture.redirect = None


def _host_draw(capture, redraw, what):
    value = _draw(capture, redraw)
    if not isinstance(value, torch.Tensor):
        return value
    if value.requires_grad:
        raise NotImplementedError("prepare() cannot record a random draw that requires grad (%s)" % what)
    if value.dtype is torch.float32 and len(value.shape) <= 4 and torch._numel(value.shape):
        node = capture.graph.tensor(value._s, tuple(value.shape))
        capture.draws.append({"what": what, "redraw": redraw, "node": node, "storage": None})
        return _Tensor(capture, node, requires_grad=False)
    capture.draws.append({"what": what, "redraw": redraw, "node": None, "storage": value._s, "value": value})
    return value


def _argument(args, kwargs, index, name, default=None):
    if len(args) > index:
        return args[index]
    return kwargs.get(name, default)


def _no_inplace(kwargs, args, index, name):
    if _argument(args, kwargs, index, "inplace", False):
        raise NotImplementedError("torch.compile does not record in-place %s on a graph tensor; use inplace=False" % name)


def _patch_functions(patched, capture=None):
    """Graph-aware versions of torch's comparison, selection, clamping,
    dropout, concatenation and indexing functions, installed while a call
    records. Each falls through to the original unless a graph tensor is among
    its tensor arguments (index_select/gather also record for a trainable
    source or an index a prepared session feeds). While a prepared step
    records, torch's CPU random draws become per-step feeds (see `_draw`).
    Every replaced attribute is appended to `patched` as it is installed."""
    prepared = capture is not None and capture.prepared

    def patch(module, name, make):
        original = getattr(module, name, None)
        if original is not None:
            setattr(module, name, make(original))
            patched.append((module, name, original))

    def where(original):
        def call(condition, *args, **kwargs):
            a, b = _argument(args, kwargs, 0, "input"), _argument(args, kwargs, 1, "other")
            capture = None if a is None and b is None else _capture_of(condition, a, b)
            return original(condition, *args, **kwargs) if capture is None else _where(capture, condition, a, b, "where")
        return call

    def clamp(original, lo_default=None, hi_default=None, one=None):
        def call(input, *args, **kwargs):
            if one == "min":
                lo, hi = _argument(args, kwargs, 0, "min"), None
            elif one == "max":
                lo, hi = None, _argument(args, kwargs, 0, "max")
            else:
                lo, hi = _argument(args, kwargs, 0, "min"), _argument(args, kwargs, 1, "max")
            capture = _capture_of(input, lo, hi)
            return original(input, *args, **kwargs) if capture is None else _clamp(capture, input, lo, hi)
        return call

    def maximum(op):
        def make(original):
            def call(input, *args, **kwargs):
                other = _argument(args, kwargs, 0, "other")
                capture = _capture_of(input, other)
                return original(input, *args, **kwargs) if capture is None else _maximum(capture, op, input, other)
            return call
        return make

    def binary_max(op):
        # torch.max(a, b) / torch.min(a, b): the elementwise form only.
        def make(original):
            def call(input, *args, **kwargs):
                other = _argument(args, kwargs, 0, "dim", kwargs.get("other"))
                capture = _capture_of(input, other) if isinstance(other, (torch.Tensor, _Tensor)) else None
                return original(input, *args, **kwargs) if capture is None else _maximum(capture, op, input, other)
            return call
        return make

    def masked_fill(original):
        def call(input, mask, value):
            capture = _capture_of(input, mask, value)
            return original(input, mask, value) if capture is None else _masked_fill(capture, input, mask, value)
        return call

    def binary_nograd(original):
        def call(op, a, b):
            capture = _capture_of(a, b)
            if capture is not None and op in ("eq", "ne", "lt", "le", "gt", "ge"):
                return _compare(capture, op, a, b)
            if capture is not None and op in ("and", "or", "xor"):
                return _logical(capture, op, a, b)
            return original(op, a, b)
        return call

    def logical_not(original):
        def call(input):
            capture = _capture_of(input)
            return original(input) if capture is None else _logical(capture, "not", input, None)
        return call

    def rand_like(original):
        def call(input, *args, **kwargs):
            owner = _capture_of(input)
            if owner is None:
                if prepared and capture.redirect is None:
                    return _host_draw(capture, lambda: original(input, *args, **kwargs), "torch.rand_like")
                return original(input, *args, **kwargs)
            return _uniform_like(owner, input, _argument(args, kwargs, 0, "generator"), kwargs.get("dtype"))
        return call

    def bernoulli(original, rand):
        def call(input, *args, **kwargs):
            owner = _capture_of(input)
            p, generator = _argument(args, kwargs, 0, "p"), _argument(args, kwargs, 1, "generator")
            if owner is None:
                if not prepared or capture.redirect is not None:
                    return original(input, *args, **kwargs)
                if not isinstance(input, torch.Tensor) or input.dtype is not torch.float32:
                    return _host_draw(capture, lambda: original(input, *args, **kwargs), "torch.bernoulli")
                # PyTorch's (and eager Zipp's) bernoulli: rand(shape) < p, the
                # draw fed and the comparison recorded against p on the device.
                shape = tuple(input.shape)
                u = _host_draw(capture, lambda: rand(*shape, generator=generator, dtype=torch.float32), "torch.bernoulli")
                return _compare(capture, "lt", u, input if p is None else float(p)).float()
            return _bernoulli(owner, input, p, generator)
        return call

    def drawn(what):
        # A CPU draw of a prepared step: the same call, drawn again every step.
        def make(original):
            def call(*args, **kwargs):
                if capture.redirect is not None:
                    return original(*args, **kwargs)
                return _host_draw(capture, lambda: original(*args, **kwargs), what)
            return call
        return make

    def multinomial(original):
        def call(input, *args, **kwargs):
            if isinstance(input, _Tensor):
                raise NotImplementedError("torch.compile cannot record torch.multinomial of a graph tensor: it samples from values "
                                          "the host does not have until the step has run")
            if not prepared or capture.redirect is not None:
                return original(input, *args, **kwargs)
            value = _host_draw(capture, lambda: original(input, *args, **kwargs), "torch.multinomial")
            capture.draws[-1]["probs"] = input
            return value
        return call

    def normal(original, randn):
        # normal(mean, std) with a tensor mean or std: randn of the broadcast
        # shape, times std, plus mean (eager Zipp's order), without gradient.
        # With a graph operand that arithmetic records on the device, and in a
        # prepared step so does it for any tensor operand (a parameter's
        # value lives on the device); the randn is the draw.
        def call(mean=0.0, std=1.0, *args, **kwargs):
            owner = _capture_of(mean, std)
            tensors = isinstance(mean, (torch.Tensor, _Tensor)) or isinstance(std, (torch.Tensor, _Tensor))
            if owner is None and not (prepared and capture.redirect is None):
                return original(mean, std, *args, **kwargs)
            owner = owner or capture
            if not tensors:
                return _host_draw(owner, lambda: original(mean, std, *args, **kwargs), "torch.normal")
            if args or set(kwargs) - {"generator"}:
                raise NotImplementedError("torch.compile records torch.normal(mean, std, generator=None) with a tensor mean or std")
            generator = kwargs.get("generator")
            like = mean if isinstance(mean, (torch.Tensor, _Tensor)) else std
            shape = _broadcast_shapes(tuple(getattr(mean, "shape", ())), tuple(getattr(std, "shape", ())))
            dtype = like.dtype
            draw = lambda: randn(*shape, generator=generator, dtype=dtype)
            z = _host_draw(owner, draw, "torch.normal") if prepared else draw()
            out = owner.operand(z) * owner.operand(std) + owner.operand(mean)
            return out.detach()
        return call

    def cat(original):
        def call(tensors, dim=0):
            owner = _capture_of(*tensors) if isinstance(tensors, (list, tuple)) else None
            return original(tensors, dim) if owner is None else _cat(owner, tensors, dim)
        return call

    def stack(original):
        def call(tensors, dim=0):
            owner = _capture_of(*tensors) if isinstance(tensors, (list, tuple)) else None
            return original(tensors, dim) if owner is None else _stack(owner, tensors, dim)
        return call

    def selecting(kind):
        def make(original):
            def call(input, dim, index, *args, **kwargs):
                if not _graph_select(capture, input, index):
                    return original(input, dim, index, *args, **kwargs)
                t = input if isinstance(input, _Tensor) else capture.operand(input)
                return _index_select(t, dim, index) if kind == "index_select" else _gather(t, dim, index)
            return call
        return make

    def method(name):
        # torch.split(t, ...) and friends of a graph tensor: its own method.
        def make(original):
            def call(input, *args, **kwargs):
                return getattr(input, name)(*args, **kwargs) if isinstance(input, _Tensor) else original(input, *args, **kwargs)
            return call
        return make

    def dropout(feature_rank=None, name="dropout"):
        def make(original):
            def call(input, *args, **kwargs):
                capture = _capture_of(input)
                if capture is None:
                    return original(input, *args, **kwargs)
                p, training = _argument(args, kwargs, 0, "p", 0.5), _argument(args, kwargs, 1, "training", True)
                if training and p:
                    _no_inplace(kwargs, args, 2, name)
                return _dropout(capture, input, p, training, feature_rank, name)
            return call
        return make

    def hardtanh(original):
        def call(input, *args, **kwargs):
            capture = _capture_of(input)
            if capture is None:
                return original(input, *args, **kwargs)
            _no_inplace(kwargs, args, 2, "hardtanh")
            return _hardtanh(capture, input, _argument(args, kwargs, 0, "min_val", -1.0), _argument(args, kwargs, 1, "max_val", 1.0))
        return call

    def relu6(original):
        def call(input, *args, **kwargs):
            capture = _capture_of(input)
            if capture is None:
                return original(input, *args, **kwargs)
            _no_inplace(kwargs, args, 0, "relu6")
            return _hardtanh(capture, input, 0.0, 6.0)
        return call

    def leaky_relu(original):
        def call(input, *args, **kwargs):
            capture = _capture_of(input)
            if capture is None:
                return original(input, *args, **kwargs)
            _no_inplace(kwargs, args, 1, "leaky_relu")
            return _leaky_relu(capture, input, _argument(args, kwargs, 0, "negative_slope", 0.01))
        return call

    patch(torch, "where", where)
    for name in ("clamp", "clip"):
        patch(torch, name, clamp)
    patch(torch, "clamp_min", lambda original: clamp(original, one="min"))
    patch(torch, "clamp_max", lambda original: clamp(original, one="max"))
    patch(torch, "maximum", maximum("maximum"))
    patch(torch, "minimum", maximum("minimum"))
    patch(torch, "max", binary_max("maximum"))
    patch(torch, "min", binary_max("minimum"))
    patch(torch, "masked_fill", masked_fill)
    # Every comparison and logical and/or/xor goes through it, including an
    # eager tensor on the left of a graph tensor (`bound < x`).
    patch(torch, "_binary_nograd", binary_nograd)
    patch(torch, "logical_not", logical_not)
    patch(torch, "rand_like", rand_like)
    patch(torch, "bernoulli", lambda original: bernoulli(original, torch.rand))
    patch(torch, "normal", lambda original: normal(original, torch.randn))
    patch(torch, "multinomial", multinomial)
    if prepared:
        for name in ("randn", "rand", "randint", "randperm", "randn_like", "randint_like"):
            patch(torch, name, drawn("torch." + name))

        def in_place(name):
            def make(original):
                def call(self, *args, **kwargs):
                    raise NotImplementedError(
                        "prepare() cannot record Tensor.%s inside the step: an in-place draw would keep its prepare() values at "
                        "every step. torch.rand/randn/randint/randperm/normal/multinomial, their *_like forms and torch.bernoulli "
                        "are drawn afresh on the host every step; use one of those, per-call torch.compile, or draw outside the "
                        "step and pass the tensor as an argument" % name)
                return call
            return make
        for name in ("uniform_", "normal_", "bernoulli_", "exponential_", "geometric_", "log_normal_", "cauchy_", "random_"):
            patch(torch.Tensor, name, in_place(name))
    for name in ("cat", "concat", "concatenate"):
        patch(torch, name, cat)
    patch(torch, "stack", stack)
    patch(torch, "index_select", selecting("index_select"))
    patch(torch, "gather", selecting("gather"))
    for name in ("split", "chunk", "unbind", "narrow", "select", "flip", "fliplr", "flipud", "take_along_dim"):
        patch(torch, name, method(name))
    patch(_F, "dropout", dropout())
    patch(_F, "dropout1d", dropout(3, "dropout1d"))
    patch(_F, "dropout2d", dropout(4, "dropout2d"))
    # Rank 4 at most on the device, so only unbatched (C, D, H, W) input.
    patch(_F, "dropout3d", dropout(5, "dropout3d"))
    patch(_F, "hardtanh", hardtanh)
    patch(_F, "relu6", relu6)
    patch(_F, "leaky_relu", leaky_relu)


# ---- prepared constants: which tensors a step's history ties to a parameter or argument ------
# Query functions read a storage without deriving one; they are left alone.
_TRACE_SKIP = frozenset(("version", "aversion", "size", "dtype", "all_finite", "to_list", "item", "Storage", "Generator"))


def _storages(value, into, depth=0):
    if isinstance(value, _k.Storage):
        into.append(value)
    elif depth < 2 and isinstance(value, (tuple, list)):
        for item in value:
            _storages(item, into, depth + 1)


def _trace_kernels(trace, traced):
    """Wrap every tensor kernel so each call that reads storage appends
    (storages read, storages it returned or wrote in place) to `trace`; what
    was replaced is appended to `traced` as it is, for `_record` to restore."""
    version = _k.version

    def make(fn):
        def call(*args, **kwargs):
            read = []
            for value in args:
                _storages(value, read)
            for value in kwargs.values():
                _storages(value, read)
            if not read:
                return fn(*args, **kwargs)
            before = [version(x) for x in read]
            out = fn(*args, **kwargs)
            written = []
            _storages(out, written)
            for x, v in zip(read, before):
                if version(x) != v:
                    written.append(x)
            if written:
                trace.append((read, written))
            return out
        return call

    for name in dir(_k):
        if name.startswith("_") or name in _TRACE_SKIP:
            continue
        fn = getattr(_k, name)
        if isinstance(fn, type) or not callable(fn):
            continue
        setattr(_k, name, make(fn))
        traced.append((name, fn))


def _tainted(trace, sources):
    """Storage id -> "parameter" or "argument" for every storage whose history
    reaches one of `sources` through the traced kernel calls, in call order."""
    tainted = dict(sources)
    for read, written in trace:
        reason = None
        for x in read:
            found = tainted.get(id(x))
            if found == "argument":
                reason = found
                break
            if found is not None:
                reason = found
        if reason is not None:
            for x in written:
                tainted.setdefault(id(x), reason)
    return tainted


def _record(model, training, args, kwargs, prepared=False):
    """Run `model` once with graph tensors in place of its float tensor arguments."""
    global _active
    if _active is not None:
        raise RuntimeError("Nested compiled training calls are unsupported")
    capture = _Capture(training, prepared)
    for value in list(args) + list(kwargs.values()):
        if isinstance(value, torch.Tensor):
            capture.argument_storages.add(id(value._s))
    # Float tensors become graph leaves; integer tensors stay on the CPU
    # (cross_entropy records its class targets from there).
    convert = lambda value: capture.tensor(value) if isinstance(value, torch.Tensor) and value.dtype.is_floating_point else value
    # Eager ops look for graph tensors only while a call records.
    torch._recording(1)
    # A prepared step is one recorded program run many times: a random draw
    # on the CPU inside it (torch.randn noise) must be drawn again every step.
    # The draws torch_gpu knows (`_patch_functions`) record as per-step feeds
    # and, while they record, draw from a copy of the generator (`redirect`);
    # every other draw still goes through torch._gen and is refused. Dropout,
    # rand_like and bernoulli of graph tensors draw on the device instead
    # (`uniform`, fresh every step) with seeds from `capture.draw`.
    draw = getattr(torch, "_gen", None) if prepared else None
    if draw is not None:
        def watched(generator):
            real = draw(generator)
            if capture.redirect is None:
                capture.used_random = True
                return real
            entry = capture.redirect.get(id(real))
            if entry is None:
                copy = _k.gen(0)
                _k.gen_set_state(copy, _k.gen_get_state(real))
                entry = capture.redirect[id(real)] = (real, copy)
            return entry[1]
        torch._gen = watched
    # Every tensor kernel call is traced while a prepared step records, so
    # prepare() can tell a constant built inside the step (uploaded once)
    # from a value computed from a parameter or an argument (which a session
    # replaying one program would freeze): see `_tainted`.
    traced, patched = [], []
    try:
        if prepared:
            _trace_kernels(capture.trace, traced)
        # Comparisons, where, clamp, maximum/minimum, dropout and the
        # activations built from them record graph operations while a call
        # records; outside one (and for eager tensors) they are PyTorch's own.
        _patch_functions(patched, capture)
        _active = capture if training else None
        with torch.enable_grad() if training else torch.no_grad():
            output = model(*[convert(value) for value in args], **{key: convert(value) for key, value in kwargs.items()})
        if capture.used_random:
            raise NotImplementedError(
                "prepare() cannot record this CPU random draw: torch.rand/randn/randint/randperm/normal/multinomial, "
                "torch.rand_like/randn_like/randint_like and torch.bernoulli are drawn afresh on the host every step, but an "
                "in-place draw (normal_, uniform_, random_, bernoulli_, exponential_...), torch.poisson or nn.init inside the "
                "step would replay its prepare() values. Use one of those functions, per-call torch.compile, or draw outside "
                "the step and pass the tensor as an argument")
        if not isinstance(output, _Tensor):
            raise TypeError("Compiled GPU calls must return one supported graph tensor")
        if training and not capture.did_step:
            raise RuntimeError("Compiled GPU training must call zero_grad(), backward() and optimizer.step()")
        return capture, output
    finally:
        _active = None
        if draw is not None:
            torch._gen = draw
        for module, name, original in reversed(patched):
            setattr(module, name, original)
        for name, original in traced:
            setattr(_k, name, original)
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
        # `_thin_run`'s plan, once a step has run on the native GPU.
        self._thin = None
        # The feeds: each float tensor argument and each integer class-target
        # argument that the recorded step reads, by position or keyword.
        self._feeds = []
        self._constants = []
        feeds = {}
        optimizer_ids = set(id(p) for p in capture.optimizer_params)
        arguments = list(enumerate(args)) + list(kwargs.items())
        for position, value in arguments:
            if not isinstance(value, torch.Tensor):
                self._constants.append((position, value))
                continue
            key = id(value)
            if value.dtype.is_floating_point:
                entry = capture.inputs.get(key)
                nodes = [] if entry is None else [("f", entry[1]._value)]
                if key in optimizer_ids:
                    raise ValueError("A step argument cannot also be an optimizer parameter")
            else:
                # The class targets read from this argument's storage: the
                # argument itself, or a view of it (`y.view(-1)`, `y.squeeze(1)`).
                nodes = [("t", entry[1]) for entry in capture.targets.values() if entry[2][0] is value._s]
                # A bool argument read as a mask (or through a view of it) is fed the same way.
                nodes += [("m", entry[1]._value) for entry in capture.masks.values() if entry[2][0] is value._s]
                # And an integer argument read as an index (embedding tokens, gather positions).
                nodes += [("i", entry[1]) for entry in capture.indices.values() if entry[2][0] is value._s]
            if not nodes:
                # The step did not read this tensor as a graph input: it
                # ignored it, or used a copy derived from it (a slice, a
                # dtype cast), which one recorded program would freeze.
                raise NotImplementedError(
                    "prepare(): tensor argument %r is not read by the recorded step as a feed; the step ignores it or derives a copy "
                    "from it (a slice, a dtype cast, arithmetic), which the session would freeze at its prepare() value. "
                    "Pass the derived tensor as the argument instead" % (position,))
            for prefix, node in nodes:
                name = "%s%d" % (prefix, len(feeds))
                feeds[name] = node
                self._feeds.append((position, name, tuple(value.shape), value.dtype))
        # A tensor over a parameter's storage captured as its own input
        # (`p.detach().view(...)`) would keep the prepare-time weights.
        argument_ids = set(id(value) for position, value in arguments)
        parameter_nodes = dict((id(p._s), capture.inputs[id(p)][1]._value) for p in capture.optimizer_params if id(p) in capture.inputs)
        for original, symbolic, snapshot in capture.inputs.values():
            node = parameter_nodes.get(id(original._s))
            if node is None or id(original) in optimizer_ids or id(original) in argument_ids:
                continue
            recorded = capture.graph._nodes[symbolic._value._id]
            if symbolic._value is not node and not (recorded["op"] == "reshape" and recorded["a"] == node._id):
                raise NotImplementedError("prepare(): the step reads a parameter through another tensor over its storage in a different "
                                          "layout; that input would keep the prepare-time weights")
        # Constants: a tensor the step reads as a graph input is uploaded once,
        # which is right exactly when no step could give it another value --
        # when nothing in its history is a parameter or a step argument.
        # Parameters and arguments are read through their own (carried or
        # fed) inputs; anything computed from them by eager kernels would be
        # frozen at its prepare() value and is refused.
        sources = {}
        for parameter in capture.optimizer_params:
            sources[id(parameter._s)] = "parameter"
        for position, value in arguments:
            if isinstance(value, torch.Tensor):
                sources[id(value._s)] = "argument"
        drawn = dict((id(entry["storage"]), entry) for entry in capture.draws if entry["storage"] is not None)
        for key in drawn:
            sources[key] = "random"
        tainted = _tainted(capture.trace, sources)
        read = [(entry[0], entry[1]) for entry in capture.inputs.values()] + [(entry[0], entry[1]) for entry in capture.masks.values()]
        read += [(entry[0], entry[1]) for entry in capture.targets.values()] + [(entry[0], entry[1]) for entry in capture.indices.values()]
        for original, symbolic in read:
            if (id(original) in argument_ids or id(original) in optimizer_ids or id(original._s) in parameter_nodes
                    or id(original._s) in capture.argument_storages or id(original._s) in drawn):
                continue
            reason = tainted.get(id(original._s))
            if reason == "random":
                raise NotImplementedError(
                    "prepare(): the step reads a tensor that an eager operation computed from a CPU random draw of integers (a cast, "
                    "a one-hot, arithmetic) as a graph input; the session would keep its prepare() value at every step. Read the "
                    "draw itself as class targets or an index, draw a float32 tensor (arithmetic on it records on the device), "
                    "or draw outside the step and pass the tensor as an argument")
            if reason == "parameter":
                raise NotImplementedError(
                    "prepare(): the step reads a tensor computed from a parameter outside autograd (under no_grad, or from "
                    ".detach()/.data) as a graph input; the session would keep its prepare() value at every step. Compute it from "
                    "the parameter with gradients enabled (it is then recorded on the device from the resident weights) or outside the step")
            if reason == "argument":
                raise NotImplementedError(
                    "prepare(): the step reads a tensor that an eager operation derived from a step argument (a one-hot, a cast, "
                    "arithmetic on class targets) as a graph input; the session would keep its prepare() value at every step. "
                    "Pass the derived tensor as the argument instead")
        # Random draws: each drawn again on the host for every step, in recorded
        # order, and fed -- a float32 draw into its own graph input, an integer
        # one into every class-target, index or mask input read from it.
        self._draws = []
        for i, entry in enumerate(capture.draws):
            names = []
            if entry["node"] is not None:
                names.append(("r%d" % i, True))
                feeds["r%d" % i] = entry["node"]
            else:
                storage = entry["storage"]
                nodes = [entry2[1] for entry2 in capture.targets.values() if entry2[2][0] is storage]
                nodes += [entry2[1] for entry2 in capture.indices.values() if entry2[2][0] is storage]
                nodes += [entry2[1]._value for entry2 in capture.masks.values() if entry2[2][0] is storage]
                if not nodes:
                    raise NotImplementedError(
                        "prepare(): the step draws %s on the CPU and does not read the draw as class targets, an index or a "
                        "mask of the graph (it uses it in Python or in eager arithmetic); the session would keep its prepare() "
                        "value at every step. Use per-call torch.compile, or draw outside the step and pass it as an argument" % entry["what"])
                for j, node in enumerate(nodes):
                    names.append(("r%d_%d" % (i, j), False))
                    feeds["r%d_%d" % (i, j)] = node
            probs = entry.get("probs")
            if probs is not None and (probs.requires_grad or id(probs._s) in capture.argument_storages or id(probs._s) in tainted):
                raise NotImplementedError(
                    "prepare(): torch.multinomial inside the step samples from probabilities computed from a parameter, an argument "
                    "or another draw; the host would sample from their prepare() values. Pass the samples as an argument, or use "
                    "per-call torch.compile")
            self._draws.append((entry["redraw"], names))
        capture.trace = []
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
        # The in-place comparison first (every step pays it); the full one decides otherwise.
        if capture.optimizer is not None and not _optimizer_unchanged(capture.optimizer, capture.optimizer_kind, self._snapshot):
            try:
                changed = _optimizer_configuration(capture.optimizer, capture.optimizer_kind) != self._snapshot
            except Exception:
                changed = True
            if changed:
                raise RuntimeError("Optimizer changed since prepare(); sync(), dispose() and prepare again")

    def _thin_plan(self):
        """What a step on the native GPU takes from its arguments, found once:
        (session, read-back names, [(position, feed key, shape, dtype, float)]).
        None where a step needs more than that (a keyword feed, a recorded
        constant, a CPU random draw, a hosted or CPU session)."""
        session = self._session
        if (self._draws or self._constants or session is None or session._hosted or session._native_id is None
                or not session._native_ran or any(type(position) is not int for position, *_ in self._feeds)):
            return None
        feeds = [(position, str(session._feeds[name]), shape, dtype, dtype == torch.float32)
                 for position, name, shape, dtype in self._feeds]
        return (session, [n for n in session._outputs if n not in session._resident], feeds)

    def _thin_run(self, callback, arguments, on_error, many):
        """`_run` on the native GPU without re-deriving what every step shares.

        Cheap checks here (the arguments' types, shapes and dtypes, the
        optimizer against its snapshot, the session's state), then the request;
        the host checks every fed value as `_run` would (finite, class targets,
        indices) and refuses a bad one before any device work. A refusal is
        answered as `_run` answers it: its own checks run and raise their error,
        or, when they pass, the host's error fails the session as in `_run`.
        False: this call needs `_run` (which then raises or runs it)."""
        plan = self._thin
        session = plan[0]
        if (self._disposed or self._failed is not None or session._disposed or session._failed is not None
                or session._native_id is None or not callable(callback) or (on_error is not None and not callable(on_error))
                or not 1 <= len(arguments) <= 64):
            return False
        capture = self._capture
        if capture.optimizer is not None and not _optimizer_unchanged(capture.optimizer, capture.optimizer_kind, self._snapshot):
            return False
        steps = []
        for args in arguments:
            inputs = {}
            for position, key, shape, dtype, is_float in plan[2]:
                if position >= len(args):
                    return False
                value = args[position]
                if not isinstance(value, torch.Tensor) or value.dtype != dtype or value.shape != shape:
                    return False
                inputs[key] = value._s if is_float else _k.astype(value._s, "float32")
            steps.append({"inputs": inputs})
        count = len(steps)
        reply = _native()("gpu.session.run", {"session": session._native_id, "steps": steps, "readback": plan[1]})
        if not (isinstance(reply, dict) and reply.get("ok")):
            # What `_run` checks before it sends raises here as it would there.
            for i, args in enumerate(arguments):
                session._step_inputs(self._step_feeds(args, {}, i), i)
            error = (reply.get("error") if isinstance(reply, dict) else None) or {}
            self._fail(session._native_error(error), on_error)
            return True
        value = reply["value"]
        session.step = int(value.get("step", session.step + count))
        self.executed += count
        self._since_sync += count
        self.backend = value["backend"]
        self.stats = value.get("stats")
        try:
            results = []
            for i, entry in enumerate(value["steps"]):
                out = entry["outputs"]["result"]
                out["data"] = _host_data(out["data"], True)
                results.append(self._read(entry, i))
        except Exception as error:
            self._fail(error, on_error)
            return True
        callback(results if many else results[0])
        return True

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

    def _redraw(self, feeds):
        # The step's CPU draws, from torch's generator, in the order the step
        # made them: what the same eager step would draw at this point.
        for redraw, names in self._draws:
            value = redraw()
            for name, is_float in names:
                feeds[name] = value._s if is_float else _k.astype(value._s, "float32")
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
        # Drawn only once every step's arguments are accepted, step by step.
        feeds = [self._redraw(step) for step in feeds]
        count = len(batches)

        def arrived(result):
            self.executed += count
            self._since_sync += count
            self.backend = result["backend"]
            self.stats = result.get("stats")
            if self._thin is None:
                self._thin = self._thin_plan()
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
        if self._thin is not None and not kwargs and self._thin_run(callback, (args,), on_error, False):
            return None
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
        if self._thin is not None and all(not kw for _, kw in normalized) and \
                self._thin_run(callback, [args for args, _ in normalized], on_error, True):
            return None
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
                    capture.optimizer.state[parameter][key] = read("state" + str(i), parameter.shape)
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
