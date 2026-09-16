"""A PyTorch-compatible tensor library for Zipp: contiguous float32/float64/
int64/bool tensors with broadcasting, reverse-mode autograd, and the modules,
functional ops, optimizers and checkpoint format the bundled `torch.nn`,
`torch.nn.functional`, `torch.optim` and `torch.autograd` provide.

Eager operations run on the engine's CPU kernels (`_zipp_tensor`, JavaScript
typed arrays); there is no CUDA device. The experimental compile extension
records supported inference and SGD training graphs for asynchronous host execution. Semantics follow PyTorch for the
supported subset; random streams use MT19937 with PyTorch's CPU transforms
but bit-for-bit agreement with a given PyTorch build is not guaranteed.
"""
import builtins as _b
import math as _math
import _zipp_tensor as _k

__version__ = "2.10.0+zipp"
_int, _float, _bool, _isinstance, _len, _range, _tuple, _list = _b.int, _b.float, _b.bool, _b.isinstance, _b.len, _b.range, _b.tuple, _b.list


class dtype:
    def __init__(self, name, is_floating):
        self.name = name
        self.is_floating_point = is_floating
        self.itemsize = 8 if name in ("float64", "int64") else 4 if name in ("float32", "int32") else 1

    def __repr__(self):
        return "torch." + self.name

    def __eq__(self, other):
        return _isinstance(other, dtype) and other.name == self.name

    def __hash__(self):
        return hash(self.name)


float32 = dtype("float32", True)
float64 = dtype("float64", True)
int64 = dtype("int64", False)
int32 = dtype("int32", False)
uint8 = dtype("uint8", False)
_bool_dtype = dtype("bool", False)
_DTYPES = {"float32": float32, "float64": float64, "int64": int64, "int32": int32, "uint8": uint8, "bool": _bool_dtype}
_default_dtype = float32


class device:
    def __init__(self, kind="cpu"):
        if not (_isinstance(kind, device) or kind in ("cpu", "cpu:0")):
            raise RuntimeError("Eager Zipp torch supports CPU only; use torch.compile(..., backend='zipp_gpu') for GPU inference or training")
        self.type = "cpu"

    def __repr__(self):
        return "device(type='cpu')"

    def __eq__(self, other):
        return _isinstance(other, device) or other == "cpu"

    def __hash__(self):
        return 0


_cpu = device("cpu")


def _check_cpu_device(value):
    if value is not None:
        device(value)


def compile(model=None, *, backend="zipp_gpu", training=False):
    """Record float32 inference or SGD training; submit(callback) executes on the host."""
    from torch._gpu import compile as compile_gpu
    return compile_gpu(model, backend=backend, training=training)


# How many compiled calls are recording. Only then can an operand be a graph
# tensor, so eager ops test this before probing for one: the probe's miss
# raised internally and cost more than the kernel on small tensors.
_graph_recording = 0


def _recording(delta):
    global _graph_recording
    _graph_recording += delta


class _TensorIter:
    __slots__ = ("_t", "_i", "_n")

    def __init__(self, t):
        self._t = t
        self._i = 0
        self._n = t.shape[0]

    def __iter__(self):
        return self

    def __next__(self):
        i = self._i
        if i >= self._n:
            raise StopIteration
        self._i = i + 1
        return self._t[i]


class Size(_tuple):
    def numel(self):
        n = 1
        for d in self:
            n *= d
        return n

    def __repr__(self):
        return "torch.Size(%s)" % _list(self).__repr__()


def _numel(shape):
    n = 1
    for d in shape:
        n *= d
    return n


def _dtype_of(v):
    if v is None:
        return None
    if _isinstance(v, dtype):
        return v
    if v is _b.float:
        return float32
    if v is _b.int:
        return int64
    if v is _b.bool:
        return _bool_dtype
    raise TypeError("invalid dtype %r" % (v,))


def _norm_dim(dim, rank):
    if dim < -rank or dim >= rank:
        raise IndexError("Dimension out of range (expected to be in range of [%d, %d], but got %d)" % (-rank, rank - 1, dim))
    return dim + rank if dim < 0 else dim


# ---- autograd ---------------------------------------------------------------------------
_grad_enabled = True


class no_grad:
    """Context manager and decorator: operations inside record no gradient."""

    def __enter__(self):
        global _grad_enabled
        self._prev = _grad_enabled
        _grad_enabled = False
        return self

    def __exit__(self, *exc):
        global _grad_enabled
        _grad_enabled = self._prev
        return False

    def __call__(self, fn):
        def wrapper(*a, **kw):
            with no_grad():
                return fn(*a, **kw)
        wrapper.__name__ = getattr(fn, "__name__", "wrapper")
        wrapper.__wrapped__ = fn
        return wrapper


class enable_grad(no_grad):
    def __enter__(self):
        global _grad_enabled
        self._prev = _grad_enabled
        _grad_enabled = True
        return self


def is_grad_enabled():
    return _grad_enabled


def set_grad_enabled(mode):
    global _grad_enabled
    _grad_enabled = _bool(mode)


class _Node:
    __slots__ = ("backward", "parents", "name", "pstate")

    def __init__(self, backward, parents, name):
        self.backward = backward
        self.parents = parents
        self.name = name
        # Each parent's node, storage and requires_grad as this node consumed
        # them. An in-place op rebinds the tensor object afterwards; like
        # PyTorch's edges, backward still reaches the history it had here.
        self.pstate = [None if p is None else (p._node, p._s, p.requires_grad) for p in parents]


def _frozen(t, s):
    """`t` with the storage `s` a node saw: an in-place op may have rebound it since."""
    return t if t._s is s else Tensor(s, t.shape, _DTYPES[_k.dtype(s)])


def _needs_grad(*tensors):
    if not _grad_enabled:
        return False
    for t in tensors:
        if _isinstance(t, Tensor) and t.requires_grad:
            return True
    return False


def _unbroadcast(grad, shape):
    """Sum `grad` down to `shape` (the reverse of broadcasting)."""
    if _tuple(grad.shape) == _tuple(shape):
        return grad
    extra = _len(grad.shape) - _len(shape)
    dims = _list(_range(extra))
    for i, d in enumerate(shape):
        if d == 1 and grad.shape[extra + i] != 1:
            dims.append(extra + i)
    if dims:
        grad = grad.sum(dims, keepdim=True)
    return grad.reshape(shape)


# ---- the tensor -------------------------------------------------------------------------
class Tensor:
    def __init__(self, storage, shape, dt, requires_grad=False, node=None):
        self._s = storage
        # A Size is immutable, so tensors of one shape share it: elementwise
        # kernels hand an operand's own Size back as the result's shape.
        self.shape = shape if type(shape) is Size else Size(shape)
        self.dtype = dt
        self.requires_grad = requires_grad
        self.grad = None
        self._node = node
        self._retain = False

    # -- construction helpers
    @staticmethod
    def _wrap(pair, dt, node=None, requires_grad=False):
        storage, shape = pair
        return Tensor(storage, shape, dt, requires_grad, node)

    def _result(self, storage, shape, dt=None):
        return Tensor(storage, shape, self.dtype if dt is None else dt)

    # -- basic properties
    @property
    def ndim(self):
        return _len(self.shape)

    def dim(self):
        return _len(self.shape)

    def size(self, dim=None):
        return self.shape if dim is None else self.shape[_norm_dim(dim, _len(self.shape))]

    def numel(self):
        return _numel(self.shape)

    def nelement(self):
        return self.numel()

    @property
    def device(self):
        return _cpu

    @property
    def is_leaf(self):
        return self._node is None

    @property
    def data(self):
        return Tensor(self._s, self.shape, self.dtype)

    @data.setter
    def data(self, value):
        self._s = value._s
        self.shape = Size(value.shape)
        self.dtype = value.dtype

    @property
    def T(self):
        return self.transpose(0, 1) if _len(self.shape) == 2 else self.permute(*reversed(_range(_len(self.shape))))

    @property
    def mT(self):
        return self.transpose(-2, -1)

    def is_floating_point(self):
        return self.dtype.is_floating_point

    def is_contiguous(self):
        return True

    def contiguous(self):
        return self

    def element_size(self):
        return self.dtype.itemsize

    def get_device(self):
        return -1

    def stride(self):
        s, acc = [], 1
        for d in reversed(self.shape):
            s.append(acc)
            acc *= d
        return _tuple(reversed(s))

    def __len__(self):
        if _len(self.shape) == 0:
            raise TypeError("len() of a 0-d tensor")
        return self.shape[0]

    def __iter__(self):
        if _len(self.shape) == 0:
            raise TypeError("iteration over a 0-d tensor")
        # An iterator object rather than a generator: a generator resumed by a
        # builtin runs in a nested interpreter loop, and the hardened wasm
        # build caps that nesting, so a plain __next__ keeps `for row in t`
        # inside comprehensions and generators cheap.
        return _TensorIter(self)

    def __hash__(self):
        return id(self)

    # -- conversions
    def item(self):
        if _numel(self.shape) != 1:
            raise RuntimeError("a Tensor with %d elements cannot be converted to Scalar" % _numel(self.shape))
        return _k.item(self._s, 0)

    def tolist(self):
        flat = _k.to_list(self._s)
        if _len(self.shape) == 0:
            return flat[0]

        def build(offset, dims):
            if _len(dims) == 1:
                return flat[offset:offset + dims[0]]
            step = _numel(dims[1:])
            return [build(offset + i * step, dims[1:]) for i in _range(dims[0])]
        return build(0, _list(self.shape))

    def __float__(self):
        return _float(self.item())

    def __int__(self):
        return _int(self.item())

    def __bool__(self):
        n = _numel(self.shape)
        if n != 1:
            raise RuntimeError("Boolean value of Tensor with more than one value is ambiguous" if n > 1 else "Boolean value of Tensor with no values is ambiguous")
        return _bool(self.item())

    def __index__(self):
        if self.dtype.is_floating_point or _numel(self.shape) != 1:
            raise TypeError("only integer tensors of a single element can be converted to an index")
        return _int(self.item())

    def numpy(self):
        return _NdArray(self)

    def __repr__(self):
        return _repr(self)

    __str__ = __repr__

    # -- autograd
    def requires_grad_(self, flag=True):
        self.requires_grad = _bool(flag)
        return self

    def detach(self):
        return Tensor(self._s, self.shape, self.dtype)

    def detach_(self):
        self._node = None
        self.requires_grad = False
        return self

    def clone(self):
        out = Tensor(_k.copy(self._s), self.shape, self.dtype)
        if _needs_grad(self):
            out.requires_grad = True
            out._node = _Node(lambda g: (g,), (self,), "clone")
        return out

    def retain_grad(self):
        self._retain = True

    def backward(self, gradient=None):
        if not self.requires_grad:
            raise RuntimeError("element 0 of tensors does not require grad and does not have a grad_fn")
        if gradient is None:
            if _numel(self.shape) != 1:
                raise RuntimeError("grad can be implicitly created only for scalar outputs")
            gradient = ones(*self.shape, dtype=self.dtype)
        _backward(self, gradient, accumulate=True)

    # -- arithmetic
    def __add__(self, other):
        return add(self, other)

    __radd__ = __add__

    def __sub__(self, other):
        return sub(self, other)

    def __rsub__(self, other):
        return sub(other, self)

    def __mul__(self, other):
        return mul(self, other)

    __rmul__ = __mul__

    def __truediv__(self, other):
        return div(self, other)

    def __rtruediv__(self, other):
        return div(other, self)

    def __floordiv__(self, other):
        return _binary_nograd("floordiv", self, other)

    def __mod__(self, other):
        return _binary_nograd("mod", self, other)

    def __pow__(self, other):
        return pow(self, other)

    def __rpow__(self, other):
        return pow(other, self)

    def __matmul__(self, other):
        return matmul(self, other)

    def __rmatmul__(self, other):
        return matmul(other, self)

    def __neg__(self):
        return neg(self)

    def __pos__(self):
        return self

    def __abs__(self):
        return abs(self)

    def __iadd__(self, other):
        return self._inplace(add(self, other))

    def __isub__(self, other):
        return self._inplace(sub(self, other))

    def __imul__(self, other):
        return self._inplace(mul(self, other))

    def __itruediv__(self, other):
        return self._inplace(div(self, other))

    def _inplace(self, result):
        # In-place arithmetic rebinds this tensor to the result's storage and
        # history. `result` was computed from this tensor, and every node
        # recorded its parents' history (`_Node.pstate`), so the graph keeps
        # the version before the change. A leaf that requires grad is
        # refused, as in PyTorch; under no_grad only the values change.
        if self.requires_grad and self._node is None and _grad_enabled:
            raise RuntimeError("a leaf Variable that requires grad is being used in an in-place operation.")
        if _tuple(result.shape) != _tuple(self.shape):
            raise RuntimeError("output with shape %s doesn't match the broadcast shape %s" % (_tuple(self.shape), _tuple(result.shape)))
        self._s = result._s
        self.dtype = result.dtype
        if _grad_enabled:
            self._node = result._node
            self.requires_grad = result.requires_grad
        return self

    def add_(self, other, alpha=1):
        return self._inplace(add(self, mul(other, alpha) if alpha != 1 else other))

    def sub_(self, other, alpha=1):
        return self._inplace(sub(self, mul(other, alpha) if alpha != 1 else other))

    def mul_(self, other):
        return self._inplace(mul(self, other))

    def div_(self, other):
        return self._inplace(div(self, other))

    def zero_(self):
        _k.fill(self._s, 0)
        return self

    def fill_(self, value):
        _k.fill(self._s, _float(value))
        return self

    def copy_(self, other):
        other = _as_tensor(other)
        if _tuple(other.shape) != _tuple(self.shape):
            other = other.expand(*self.shape)
        _k.copy_into(self._s, other._s)
        return self

    def clamp_(self, min=None, max=None):
        return self._inplace(clamp(self, min, max))

    def uniform_(self, a=0.0, b=1.0, generator=None):
        _k.copy_into(self._s, (rand(*self.shape, generator=generator) * (b - a) + a)._s)
        return self

    def normal_(self, mean=0.0, std=1.0, generator=None):
        _k.copy_into(self._s, (randn(*self.shape, generator=generator, dtype=self.dtype) * std + mean)._s)
        return self

    # -- comparisons
    def __eq__(self, other):
        return _binary_nograd("eq", self, other)

    def __ne__(self, other):
        return _binary_nograd("ne", self, other)

    def __lt__(self, other):
        return _binary_nograd("lt", self, other)

    def __le__(self, other):
        return _binary_nograd("le", self, other)

    def __gt__(self, other):
        return _binary_nograd("gt", self, other)

    def __ge__(self, other):
        return _binary_nograd("ge", self, other)

    def __and__(self, other):
        return _binary_nograd("and", self, other)

    def __or__(self, other):
        return _binary_nograd("or", self, other)

    def __xor__(self, other):
        return _binary_nograd("xor", self, other)

    def __invert__(self):
        return logical_not(self)

    eq = __eq__
    ne = __ne__
    lt = __lt__
    le = __le__
    gt = __gt__
    ge = __ge__
    logical_and = __and__
    logical_or = __or__

    # -- elementwise
    def exp(self):
        return exp(self)

    def log(self):
        return log(self)

    def tanh(self):
        return tanh(self)

    def sigmoid(self):
        return sigmoid(self)

    def relu(self):
        return relu(self)

    def sqrt(self):
        return sqrt(self)

    def square(self):
        return square(self)

    def abs(self):
        return abs(self)

    def neg(self):
        return neg(self)

    def sign(self):
        return _unary_nograd("sign", self)

    def floor(self):
        return _unary_nograd("floor", self)

    def ceil(self):
        return _unary_nograd("ceil", self)

    def round(self):
        return _unary_nograd("round", self)

    def isfinite(self):
        return _unary_nograd("isfinite", self)

    def isnan(self):
        return _unary_nograd("isnan", self)

    def logical_not(self):
        return logical_not(self)

    def clamp(self, min=None, max=None):
        return clamp(self, min, max)

    clip = clamp

    def pow(self, exponent):
        return pow(self, exponent)

    def reciprocal(self):
        return div(1.0, self)

    def softmax(self, dim=-1):
        return softmax(self, dim)

    def log_softmax(self, dim=-1):
        return log_softmax(self, dim)

    # -- reductions
    def sum(self, dim=None, keepdim=False, dtype=None):
        return sum(self, dim, keepdim, dtype)

    def mean(self, dim=None, keepdim=False, dtype=None):
        return mean(self, dim, keepdim, dtype)

    def prod(self, dim=None, keepdim=False):
        return prod(self, dim, keepdim)

    def count_nonzero(self, dim=None):
        return count_nonzero(self, dim)

    def max(self, dim=None, keepdim=False):
        return max(self, dim, keepdim)

    def min(self, dim=None, keepdim=False):
        return min(self, dim, keepdim)

    def argmax(self, dim=None, keepdim=False):
        return argmax(self, dim, keepdim)

    def argmin(self, dim=None, keepdim=False):
        return argmin(self, dim, keepdim)

    def all(self, dim=None, keepdim=False):
        return _reduce_nograd("all", self, dim, keepdim)

    def any(self, dim=None, keepdim=False):
        return _reduce_nograd("any", self, dim, keepdim)

    def norm(self, p=2, dim=None, keepdim=False):
        return norm(self, p, dim, keepdim)

    def var(self, dim=None, keepdim=False, unbiased=True, correction=None):
        return var(self, dim, keepdim, unbiased if correction is None else correction)

    def std(self, dim=None, keepdim=False, unbiased=True):
        return sqrt(var(self, dim, keepdim, unbiased))

    def argsort(self, dim=-1, descending=False):
        return argsort(self, dim, descending)

    def sort(self, dim=-1, descending=False):
        idx = argsort(self, dim, descending)
        return gather(self, dim, idx), idx

    def topk(self, k, dim=-1, largest=True):
        idx = argsort(self, dim, largest)
        idx = idx.narrow(dim, 0, k)
        return gather(self, dim, idx), idx

    def cumsum(self, dim):
        return cumsum(self, dim)

    # -- shape
    def reshape(self, *shape):
        return reshape(self, *shape)

    view = reshape

    def reshape_as(self, other):
        return reshape(self, *other.shape)

    view_as = reshape_as

    def flatten(self, start_dim=0, end_dim=-1):
        return flatten(self, start_dim, end_dim)

    def unsqueeze(self, dim):
        return unsqueeze(self, dim)

    def squeeze(self, dim=None):
        return squeeze(self, dim)

    def transpose(self, d0, d1):
        return transpose(self, d0, d1)

    def permute(self, *dims):
        return permute(self, *dims)

    def expand(self, *sizes):
        return expand(self, *sizes)

    def expand_as(self, other):
        return expand(self, *other.shape)

    def repeat(self, *sizes):
        return repeat(self, *sizes)

    def narrow(self, dim, start, length):
        return narrow(self, dim, start, length)

    def unbind(self, dim=0):
        return _tuple(self.select(dim, i) for i in _range(self.shape[_norm_dim(dim, _len(self.shape))]))

    def select(self, dim, index):
        d = _norm_dim(dim, _len(self.shape))
        spec = [slice(None)] * d + [index]
        return self[_tuple(spec)]

    def split(self, size, dim=0):
        return split(self, size, dim)

    def chunk(self, chunks, dim=0):
        return chunk(self, chunks, dim)

    def roll(self, shifts, dims):
        return roll(self, shifts, dims)

    def diag(self):
        return diag(self)

    def type(self, dt=None):
        if dt is None:
            return "torch." + {"float32": "FloatTensor", "float64": "DoubleTensor", "int64": "LongTensor", "int32": "IntTensor", "bool": "BoolTensor", "uint8": "ByteTensor"}[self.dtype.name]
        return self.to(dt)

    def to(self, *args, **kwargs):
        _check_cpu_device(kwargs.get("device"))
        target = kwargs.get("dtype")
        for a in args:
            if _isinstance(a, (_b.str, device)):
                _check_cpu_device(a)
            if _isinstance(a, dtype):
                target = a
            elif _isinstance(a, Tensor):
                target = a.dtype
        if target is None or target == self.dtype:
            return self
        return _cast(self, target)

    def float(self):
        return self.to(float32)

    def double(self):
        return self.to(float64)

    def long(self):
        return self.to(int64)

    def int(self):
        return self.to(int32)

    def bool(self):
        return self.to(_bool_dtype)

    def byte(self):
        return self.to(uint8)

    def half(self):
        return self.to(float32)

    def cpu(self):
        return self

    def cuda(self):
        raise RuntimeError("CUDA is not available on Zipp")

    def new_zeros(self, *shape, dtype=None, requires_grad=False):
        return zeros(*shape, dtype=self.dtype if dtype is None else dtype, requires_grad=requires_grad)

    def new_ones(self, *shape, dtype=None, requires_grad=False):
        return ones(*shape, dtype=self.dtype if dtype is None else dtype, requires_grad=requires_grad)

    def new_full(self, shape, value, dtype=None):
        return full(shape, value, dtype=self.dtype if dtype is None else dtype)

    def new_tensor(self, data, dtype=None):
        return tensor(data, dtype=self.dtype if dtype is None else dtype)

    def new_empty(self, *shape, dtype=None):
        return self.new_zeros(*shape, dtype=dtype)

    # -- indexing
    def __getitem__(self, key):
        return _getitem(self, key)

    def __setitem__(self, key, value):
        _setitem(self, key, value)

    def index_select(self, dim, index):
        return index_select(self, dim, index)

    def gather(self, dim, index):
        return gather(self, dim, index)

    def masked_fill(self, mask, value):
        return where(mask, full_like(self, value), self)

    def nonzero(self):
        return nonzero(self)


class _NdArray:
    """Enough of a numpy view for `.numpy().tobytes()` and `.tolist()`."""

    def __init__(self, t):
        self._t = t
        self.shape = _tuple(t.shape)
        self.dtype = t.dtype.name

    def tobytes(self):
        return _k.tobytes(self._t._s)

    def tolist(self):
        return self._t.tolist()

    def __array__(self):
        return self


def _repr(t):
    flat = _k.to_list(t._s)
    floating = t.dtype.is_floating_point
    # PyTorch picks one layout for the whole tensor: "1." only when every
    # finite element is a whole number, otherwise "%.4f" everywhere. Deciding
    # per element would print tensor([0., 0.5000, 0.5000]).
    int_mode = True
    if floating:
        for v in flat:
            if v != v or v in (_float("inf"), _float("-inf")):
                continue
            if v != _int(v) or _b.abs(v) >= 1e15:
                int_mode = False
                break

    def fmt(v):
        if floating:
            if v != v:
                return "nan"
            if v in (_float("inf"), _float("-inf")):
                return "inf" if v > 0 else "-inf"
            if int_mode:
                return "%d." % _int(v)
            return "%.4f" % v
        if t.dtype is _bool_dtype:
            return "True" if v else "False"
        return "%d" % v

    if _len(t.shape) == 0:
        body = fmt(flat[0])
    else:
        def build(offset, dims, depth):
            if _len(dims) == 1:
                return "[" + ", ".join(fmt(v) for v in flat[offset:offset + dims[0]]) + "]"
            step = _numel(dims[1:])
            sep = ",\n" + " " * (8 + depth)
            return "[" + sep.join(build(offset + i * step, dims[1:], depth + 1) for i in _range(dims[0])) + "]"
        body = build(0, _list(t.shape), 0)
    extra = ""
    if t.dtype not in (float32, int64, _bool_dtype):
        extra += ", dtype=%r" % t.dtype
    if t.requires_grad:
        extra += ", grad_fn=<%s>" % (t._node.name + "Backward") if t._node is not None else ", requires_grad=True"
    return "tensor(%s%s)" % (body, extra)


# ---- creation ---------------------------------------------------------------------------
def _flatten_data(data):
    """Nested lists/tuples (or scalars) to a flat list and a shape."""
    if _isinstance(data, Tensor):
        return _k.to_list(data._s), _tuple(data.shape), data.dtype
    if _isinstance(data, (_list, _tuple)):
        if _len(data) == 0:
            return [], (0,), None
        first = data[0]
        if _isinstance(first, (_list, _tuple, Tensor)):
            flat, shape, dt = [], None, None
            for row in data:
                f, s, d = _flatten_data(row)
                if shape is None:
                    shape = s
                elif s != shape:
                    raise ValueError("expected sequence of length %d at dim 1 (got %d)" % (shape[0], s[0]))
                flat.extend(f)
                if d is not None and (dt is None or (d.is_floating_point and not dt.is_floating_point)):
                    dt = d
            return flat, (_len(data),) + _tuple(shape), dt
        return _list(data), (_len(data),), None
    return [data], (), None


def _infer_dtype(flat, hint):
    if hint is not None:
        return hint
    has_float = False
    all_bool = _len(flat) > 0
    for v in flat:
        if _isinstance(v, _b.bool):
            continue
        all_bool = False
        if _isinstance(v, _float):
            has_float = True
        elif not _isinstance(v, _int):
            raise TypeError("Could not infer dtype of %s" % type(v).__name__)
    if all_bool:
        return _bool_dtype
    return _default_dtype if has_float else int64


def tensor(data, dtype=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    flat, shape, hint = _flatten_data(data)
    if _isinstance(data, Tensor) and dtype is None:
        dt = data.dtype
    else:
        dt = _dtype_of(dtype) or _infer_dtype(flat, hint)
    flat = [_float(v) if dt.is_floating_point else (_int(v) if dt is not _bool_dtype else (1 if v else 0)) for v in flat]
    return Tensor(_k.from_flat(dt.name, flat), shape, dt, requires_grad)


def as_tensor(data, dtype=None, device=None):
    _check_cpu_device(device)
    if _isinstance(data, Tensor) and (dtype is None or _dtype_of(dtype) == data.dtype):
        return data
    return tensor(data, dtype=dtype)


from_numpy = as_tensor


def _as_tensor(v, like=None):
    if _isinstance(v, Tensor):
        return v
    if like is not None and like.dtype.is_floating_point and _isinstance(v, (_int, _float, _b.bool)):
        return Tensor(_k.full(like.dtype.name, 1, _float(v)), (), like.dtype)
    return tensor(v)


def _shape_args(shape):
    if _len(shape) == 1 and _isinstance(shape[0], (_list, _tuple, Size)):
        return _tuple(_int(d) for d in shape[0])
    return _tuple(_int(d) for d in shape)


def zeros(*shape, dtype=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args(shape)
    dt = _dtype_of(dtype) or _default_dtype
    return Tensor(_k.zeros(dt.name, _numel(shape)), shape, dt, requires_grad)


def ones(*shape, dtype=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args(shape)
    dt = _dtype_of(dtype) or _default_dtype
    return Tensor(_k.full(dt.name, _numel(shape), 1), shape, dt, requires_grad)


def full(shape, fill_value, dtype=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args((shape,))
    dt = _dtype_of(dtype) or (_default_dtype if _isinstance(fill_value, _float) else (_bool_dtype if _isinstance(fill_value, _b.bool) else int64))
    return Tensor(_k.full(dt.name, _numel(shape), fill_value), shape, dt, requires_grad)


def empty(*shape, dtype=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    return zeros(*shape, dtype=dtype, requires_grad=requires_grad)


def zeros_like(t, dtype=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    return zeros(*t.shape, dtype=dtype or t.dtype, requires_grad=requires_grad)


def ones_like(t, dtype=None, device=None):
    _check_cpu_device(device)
    return ones(*t.shape, dtype=dtype or t.dtype)


def full_like(t, value, dtype=None, device=None):
    _check_cpu_device(device)
    return full(t.shape, value, dtype=dtype or t.dtype)


def empty_like(t, dtype=None, device=None):
    _check_cpu_device(device)
    return zeros_like(t, dtype=dtype)


def rand_like(t, generator=None):
    return rand(*t.shape, generator=generator, dtype=t.dtype)


def randn_like(t, generator=None):
    return randn(*t.shape, generator=generator, dtype=t.dtype)


def arange(start, end=None, step=1, dtype=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    if end is None:
        start, end = 0, start
    values = []
    v = start
    if step > 0:
        while v < end:
            values.append(v)
            v += step
    else:
        while v > end:
            values.append(v)
            v += step
    dt = _dtype_of(dtype) or (float32 if _isinstance(start, _float) or _isinstance(end, _float) or _isinstance(step, _float) else int64)
    return Tensor(_k.from_flat(dt.name, values), (_len(values),), dt, requires_grad)


def linspace(start, end, steps, dtype=None):
    values = [start + (end - start) * i / (steps - 1) for i in _range(steps)] if steps > 1 else [start]
    dt = _dtype_of(dtype) or float32
    return Tensor(_k.from_flat(dt.name, values), (_len(values),), dt)


def eye(n, m=None, dtype=None):
    m = n if m is None else m
    dt = _dtype_of(dtype) or float32
    k = _b.min(n, m)
    if k == 0:
        return zeros(n, m, dtype=dt)
    # The first min(n, m) rows are one-hot; a taller matrix ends in zero rows.
    out = one_hot_(arange(k), m).to(dt)
    return out if k == n else cat([out, zeros(n - k, m, dtype=dt)], 0)


def one_hot_(idx, n):
    return Tensor(_k.one_hot(idx._s, n), _tuple(idx.shape) + (n,), int64)


# ---- random ------------------------------------------------------------------------------
class Generator:
    def __init__(self, device="cpu"):
        _check_cpu_device(device)
        self._g = _k.gen(0)
        self.device = _cpu

    def manual_seed(self, seed):
        _k.gen_seed(self._g, _int(seed))
        return self

    def seed(self):
        return self.initial_seed()

    def initial_seed(self):
        return _k.gen_initial_seed(self._g)

    def get_state(self):
        return tensor([_k.gen_initial_seed(self._g)])

    def set_state(self, state):
        self.manual_seed(_int(state[0].item()))


default_generator = Generator()


def manual_seed(seed):
    default_generator.manual_seed(seed)
    return default_generator


def seed():
    return default_generator.initial_seed()


def initial_seed():
    return default_generator.initial_seed()


def _gen(generator):
    return (generator or default_generator)._g


def rand(*shape, generator=None, dtype=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args(shape)
    dt = _dtype_of(dtype) or float32
    storage = _k.rand(_gen(generator), _numel(shape)) if dt is float32 else _k.rand_double(_gen(generator), _numel(shape))
    return Tensor(storage, shape, dt, requires_grad)


def randn(*shape, generator=None, dtype=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args(shape)
    dt = _dtype_of(dtype) or float32
    return Tensor(_k.randn(_gen(generator), _numel(shape), dt.name), shape, dt, requires_grad)


def randint(low, high=None, size=None, generator=None, dtype=None, device=None):
    _check_cpu_device(device)
    if size is None:
        # randint(high, size) form
        low, high, size = 0, low, high
    if high is None:
        raise TypeError("randint() missing the size argument")
    shape = _shape_args((size,))
    dt = _dtype_of(dtype) or int64
    out = Tensor(_k.randint(_gen(generator), _int(low), _int(high), _numel(shape)), shape, int64)
    return out if dt is int64 else out.to(dt)


def randperm(n, generator=None, dtype=None):
    return Tensor(_k.randperm(_gen(generator), _int(n)), (n,), int64)


def multinomial(probs, num_samples, replacement=False, generator=None):
    if _len(probs.shape) not in (1, 2):
        raise RuntimeError("prob_dist must be 1 or 2 dim")
    storage = _k.multinomial(_gen(generator), probs._s, probs.shape, _int(num_samples), _bool(replacement))
    shape = (num_samples,) if _len(probs.shape) == 1 else (probs.shape[0], num_samples)
    return Tensor(storage, shape, int64)


def bernoulli(p, generator=None):
    return (rand(*p.shape, generator=generator) < p).to(p.dtype)


# ---- elementwise ops with autograd ------------------------------------------------------
def _binary_nograd(op, a, b):
    a_tensor, b_tensor = _isinstance(a, Tensor), _isinstance(b, Tensor)
    if not a_tensor:
        a = _as_tensor(a, b if b_tensor else None)
    if not b_tensor:
        b = _as_tensor(b, a if a_tensor else None)
    storage, shape = _k.binary(op, a._s, a.shape, b._s, b.shape)
    return Tensor(storage, shape, _DTYPES[_k.dtype(storage)])


def _unary_nograd(op, a, p1=None, p2=None):
    storage = _k.unary(op, a._s, p1, p2)
    return Tensor(storage, a.shape, _DTYPES[_k.dtype(storage)])


def _binary(op, a, b, name, backward):
    # The per-op path: one type test per operand, and `_needs_grad` inline
    # (both operands are tensors here).
    a_tensor, b_tensor = _isinstance(a, Tensor), _isinstance(b, Tensor)
    ta = a if a_tensor else _as_tensor(a, b if b_tensor else None)
    tb = b if b_tensor else _as_tensor(b, a if a_tensor else None)
    storage, shape = _k.binary(op, ta._s, ta.shape, tb._s, tb.shape)
    out = Tensor(storage, shape, _DTYPES[_k.dtype(storage)])
    if _grad_enabled and (ta.requires_grad or tb.requires_grad):
        out.requires_grad = True
        sa, sb = ta._s, tb._s
        out._node = _Node(lambda g: backward(g, _frozen(ta, sa), _frozen(tb, sb), _frozen(out, storage)), (ta, tb), name)
    return out


def add(a, b, alpha=1):
    if _graph_recording:
        if hasattr(a, "_zipp_graph"):
            return a.__add__((b * alpha))
        if hasattr(b, "_zipp_graph"):
            return (b * alpha).__radd__(a)
    if alpha != 1:
        b = mul(b, alpha)
    return _binary("add", a, b, "Add", lambda g, x, y, o: (_unbroadcast(g, x.shape), _unbroadcast(g, y.shape)))


def sub(a, b, alpha=1):
    if _graph_recording:
        if hasattr(a, "_zipp_graph"):
            return a.__sub__((b * alpha))
        if hasattr(b, "_zipp_graph"):
            return (b * alpha).__rsub__(a)
    if alpha != 1:
        b = mul(b, alpha)
    return _binary("sub", a, b, "Sub", lambda g, x, y, o: (_unbroadcast(g, x.shape), _unbroadcast(neg(g), y.shape)))


def mul(a, b):
    if _graph_recording:
        if hasattr(a, "_zipp_graph"):
            return a.__mul__(b)
        if hasattr(b, "_zipp_graph"):
            return b.__rmul__(a)
    return _binary("mul", a, b, "Mul", lambda g, x, y, o: (_unbroadcast(mul(g, y), x.shape), _unbroadcast(mul(g, x), y.shape)))


def div(a, b):
    return _binary("div", a, b, "Div", lambda g, x, y, o: (_unbroadcast(div(g, y), x.shape), _unbroadcast(neg(mul(g, div(o, y))), y.shape)))


def pow(a, b):
    if _isinstance(b, (_int, _float)) and not _isinstance(a, (_int, _float)):
        def backward(g, x, y, o):
            return (mul(g, mul(pow(x, b - 1), b)), None)
        return _binary("pow", a, b, "Pow", backward)
    return _binary("pow", a, b, "Pow", lambda g, x, y, o: (_unbroadcast(mul(g, mul(y, pow(x, sub(y, 1)))), x.shape), _unbroadcast(mul(g, mul(o, log(x))), y.shape)))


def maximum(a, b):
    return _binary("max", a, b, "Maximum", lambda g, x, y, o: (_unbroadcast(mul(g, (x >= y).to(g.dtype)), x.shape), _unbroadcast(mul(g, (y > x).to(g.dtype)), y.shape)))


def minimum(a, b):
    return _binary("min", a, b, "Minimum", lambda g, x, y, o: (_unbroadcast(mul(g, (x <= y).to(g.dtype)), x.shape), _unbroadcast(mul(g, (y < x).to(g.dtype)), y.shape)))


def _unary(op, a, name, backward, p1=None, p2=None):
    storage = _k.unary(op, a._s, p1, p2)
    out = Tensor(storage, a.shape, _DTYPES[_k.dtype(storage)])
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        sa = a._s
        out._node = _Node(lambda g: (backward(g, _frozen(a, sa), _frozen(out, storage)),), (a,), name)
    return out


def neg(a):
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return -a
    return _unary("neg", a, "Neg", lambda g, x, o: neg(g))


def exp(a):
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.exp()
    return _unary("exp", a, "Exp", lambda g, x, o: mul(g, o))


def log(a):
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.log()
    return _unary("log", a, "Log", lambda g, x, o: div(g, x))


def tanh(a):
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.tanh()
    return _unary("tanh", a, "Tanh", lambda g, x, o: mul(g, sub(1, square(o))))


def sigmoid(a):
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.sigmoid()
    return _unary("sigmoid", a, "Sigmoid", lambda g, x, o: mul(g, mul(o, sub(1, o))))


def silu(a):
    return _unary("silu", a, "Silu", lambda g, x, o: mul(g, _silu_grad(x)))


def _gelu(a):
    """The exact-erf GELU, F.gelu's default: x * cdf(x), with the derivative
    cdf(x) + x * pdf(x) for autograd. Under a compiled graph it records the
    protocol's gelu, whose gradient is the same closed form."""
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.gelu()
    return _unary("gelu", a, "Gelu", lambda g, x, o: mul(g, _unary_nograd("gelu_grad", x)))


def _silu_grad(x):
    s = sigmoid(x.detach())
    return mul(s, add(1, mul(x.detach(), sub(1, s))))


def relu(a):
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.relu()
    return _unary("relu", a, "Relu", lambda g, x, o: mul(g, (x > 0).to(g.dtype)))


def sqrt(a):
    return _unary("sqrt", a, "Sqrt", lambda g, x, o: div(g, mul(o, 2)))


def square(a):
    return _unary("square", a, "Pow", lambda g, x, o: mul(g, mul(x, 2)))


def abs(a):
    return _unary("abs", a, "Abs", lambda g, x, o: mul(g, _unary_nograd("sign", x)))


def clamp(a, min=None, max=None):
    lo = None if min is None else _float(min)
    hi = None if max is None else _float(max)

    def backward(g, x, o):
        mask = ones_like(g)
        if lo is not None:
            mask = mul(mask, (x >= lo).to(g.dtype))
        if hi is not None:
            mask = mul(mask, (x <= hi).to(g.dtype))
        return mul(g, mask)
    return _unary("clamp", a, "Clamp", backward, lo, hi)


clip = clamp


def logical_not(a):
    return _unary_nograd("not", a)


def isfinite(a):
    return _unary_nograd("isfinite", a)


def isnan(a):
    return _unary_nograd("isnan", a)


def sign(a):
    return _unary_nograd("sign", a)


def floor(a):
    return _unary_nograd("floor", a)


def ceil(a):
    return _unary_nograd("ceil", a)


def round(a):
    return _unary_nograd("round", a)


def where(condition, a, b):
    ta, tb = _as_tensor(a), _as_tensor(b)
    storage, shape = _k.where(condition._s, condition.shape, ta._s, ta.shape, tb._s, tb.shape)
    out = Tensor(storage, shape, _DTYPES[_k.dtype(storage)])
    if _needs_grad(ta, tb):
        out.requires_grad = True
        cond = condition.to(out.dtype)
        out._node = _Node(lambda g: (_unbroadcast(mul(g, cond), ta.shape), _unbroadcast(mul(g, sub(1, cond)), tb.shape)), (ta, tb), "Where")
    return out


def _cast(a, dt):
    out = Tensor(_k.astype(a._s, dt.name), a.shape, dt)
    if _needs_grad(a) and dt.is_floating_point and a.dtype.is_floating_point:
        out.requires_grad = True
        out._node = _Node(lambda g: (g.to(a.dtype),), (a,), "ToCopy")
    return out


# ---- reductions --------------------------------------------------------------------------
def _dims_arg(dim, rank):
    if dim is None:
        return None
    if _isinstance(dim, (_list, _tuple)):
        return [_norm_dim(_int(d), rank) for d in dim]
    return [_norm_dim(_int(dim), rank)]


def _reduce_nograd(op, a, dim, keepdim):
    dims = _dims_arg(dim, _len(a.shape))
    storage, shape = _k.reduce(op, a._s, a.shape, dims, keepdim)
    return Tensor(storage, shape, _DTYPES[_k.dtype(storage)])


def _expand_back(g, a_shape, dims, keepdim):
    """Broadcast a reduced gradient back over the reduced dims."""
    if dims is None:
        return expand(g.reshape(*([1] * _len(a_shape))), *a_shape)
    if not keepdim:
        for d in sorted(dims):
            g = g.unsqueeze(d)
    return expand(g, *a_shape)


def sum(a, dim=None, keepdim=False, dtype=None):
    if _graph_recording and hasattr(a, "_zipp_graph"):
        return a.sum(dim, keepdim, dtype)
    if dtype is not None:
        a = a.to(dtype)
    else:
        # `(pred == target).sum()` counts; a uint8 sum does not wrap.
        a = _int64_acc(a)
    dims = _dims_arg(dim, _len(a.shape))
    storage, shape = _k.reduce("sum", a._s, a.shape, dims, keepdim)
    out = Tensor(storage, shape, a.dtype)
    if _needs_grad(a):
        out.requires_grad = True
        out._node = _Node(lambda g: (_expand_back(g, a.shape, dims, keepdim),), (a,), "Sum")
    return out


def mean(a, dim=None, keepdim=False, dtype=None):
    if _graph_recording and hasattr(a, "_zipp_graph"):
        return a.mean(dim, keepdim, dtype)
    if dtype is not None:
        a = a.to(dtype)
    if not a.dtype.is_floating_point:
        raise RuntimeError("mean(): could not infer output dtype. Input dtype must be either a floating point or complex dtype. Got: %s" % a.dtype.name.capitalize())
    dims = _dims_arg(dim, _len(a.shape))
    storage, shape = _k.reduce("mean", a._s, a.shape, dims, keepdim)
    out = Tensor(storage, shape, a.dtype)
    if _needs_grad(a):
        count = _numel(a.shape) / _b.max(1, _numel(shape))
        out.requires_grad = True
        out._node = _Node(lambda g: (div(_expand_back(g, a.shape, dims, keepdim), count),), (a,), "Mean")
    return out


class _ReturnTypes(_tuple):
    @property
    def values(self):
        return self[0]

    @property
    def indices(self):
        return self[1]


def max(a, dim=None, keepdim=False):
    if _isinstance(dim, Tensor):
        return maximum(a, dim)
    if dim is None:
        out = _reduce_nograd("max", a, None, False)
        if _needs_grad(a):
            out.requires_grad = True
            out._node = _Node(_ties_backward(a, out), (a,), "Max")
        return out
    values = _reduce_nograd("max", a, dim, keepdim)
    indices = _reduce_nograd("argmax", a, dim, keepdim)
    if _needs_grad(a):
        values.requires_grad = True
        d = _norm_dim(dim, _len(a.shape))
        values._node = _Node(lambda g: (_scatter_grad(a, d, indices, g, keepdim),), (a,), "Max")
    return _ReturnTypes((values, indices))


def min(a, dim=None, keepdim=False):
    if _isinstance(dim, Tensor):
        return minimum(a, dim)
    if dim is None:
        out = _reduce_nograd("min", a, None, False)
        if _needs_grad(a):
            out.requires_grad = True
            out._node = _Node(_ties_backward(a, out), (a,), "Min")
        return out
    values = _reduce_nograd("min", a, dim, keepdim)
    indices = _reduce_nograd("argmin", a, dim, keepdim)
    if _needs_grad(a):
        values.requires_grad = True
        d = _norm_dim(dim, _len(a.shape))
        values._node = _Node(lambda g: (_scatter_grad(a, d, indices, g, keepdim),), (a,), "Min")
    return _ReturnTypes((values, indices))


def _ties_backward(a, out):
    # A full max/min splits the gradient evenly between tied extremes, as PyTorch does.
    sa, so = a._s, out._s

    def backward(g):
        mask = (_frozen(a, sa) == _frozen(out, so)).to(g.dtype)
        return (mul(div(mask, sum(mask)), g),)
    return backward


def _scatter_grad(a, d, indices, g, keepdim):
    if not keepdim:
        indices = indices.unsqueeze(d)
        g = g.unsqueeze(d)
    return _scatter_dim(zeros_like(a), d, indices, g)


def argmax(a, dim=None, keepdim=False):
    return _reduce_nograd("argmax", a, dim, keepdim)


def argmin(a, dim=None, keepdim=False):
    return _reduce_nograd("argmin", a, dim, keepdim)


def all(a, dim=None, keepdim=False):
    return _reduce_nograd("all", a, dim, keepdim)


def any(a, dim=None, keepdim=False):
    return _reduce_nograd("any", a, dim, keepdim)


def _int64_acc(a):
    # bool and integer reductions accumulate in int64, as in PyTorch.
    return a if a.dtype.is_floating_point or a.dtype is int64 else a.to(int64)


def prod(a, dim=None, keepdim=False):
    return _reduce_nograd("prod", _int64_acc(a), dim, keepdim)


def count_nonzero(a, dim=None):
    return sum(_binary_nograd("ne", a, 0), dim)


def var(a, dim=None, keepdim=False, unbiased=True):
    dims = _dims_arg(dim, _len(a.shape))
    m = mean(a, dims, True)
    sq = square(sub(a, m))
    n = _numel(a.shape) / _b.max(1, _numel(sum(sq, dims, True).shape))
    correction = 1 if unbiased is True else (0 if unbiased is False else unbiased)
    return div(sum(sq, dims, keepdim), _b.max(n - correction, 1))


def std(a, dim=None, keepdim=False, unbiased=True):
    return sqrt(var(a, dim, keepdim, unbiased))


def norm(a, p=2, dim=None, keepdim=False):
    if p == 2 or p == "fro":
        return sqrt(sum(square(a), dim, keepdim))
    if p == 1:
        return sum(abs(a), dim, keepdim)
    if p == _float("inf"):
        return max(abs(a)) if dim is None else max(abs(a), dim, keepdim)[0]
    return pow(sum(pow(abs(a), p), dim, keepdim), 1.0 / p)


def cumsum(a, dim):
    a = _int64_acc(a)
    d = _norm_dim(dim, _len(a.shape))
    parts, acc = [], None
    for i in _range(a.shape[d]):
        piece = narrow(a, d, i, 1)
        acc = piece if acc is None else add(acc, piece)
        parts.append(acc)
    return cat(parts, d)


# ---- shape ops ---------------------------------------------------------------------------
def reshape(a, *shape):
    shape = _shape_args(shape)
    n = _numel(a.shape)
    if -1 in shape:
        known = 1
        for d in shape:
            if d != -1:
                known *= d
        shape = _tuple(n // known if d == -1 else d for d in shape)
    if _numel(shape) != n:
        raise RuntimeError("shape '%s' is invalid for input of size %d" % (_list(shape), n))
    out = Tensor(a._s, shape, a.dtype)
    if _needs_grad(a):
        out.requires_grad = True
        out._node = _Node(lambda g: (g.reshape(*a.shape),), (a,), "View")
    return out


def flatten(a, start_dim=0, end_dim=-1):
    rank = _len(a.shape)
    if rank == 0:
        return reshape(a, 1)
    s, e = _norm_dim(start_dim, rank), _norm_dim(end_dim, rank)
    shape = _tuple(a.shape[:s]) + (_numel(a.shape[s:e + 1]),) + _tuple(a.shape[e + 1:])
    return reshape(a, *shape)


def unsqueeze(a, dim):
    d = _norm_dim(dim, _len(a.shape) + 1)
    return reshape(a, *(_tuple(a.shape[:d]) + (1,) + _tuple(a.shape[d:])))


def squeeze(a, dim=None):
    if dim is None:
        shape = _tuple(d for d in a.shape if d != 1)
    else:
        d = _norm_dim(dim, _len(a.shape))
        if a.shape[d] != 1:
            return a
        shape = _tuple(a.shape[:d]) + _tuple(a.shape[d + 1:])
    return reshape(a, *shape)


def permute(a, *dims):
    dims = _shape_args(dims)
    rank = _len(a.shape)
    dims = _tuple(_norm_dim(d, rank) for d in dims)
    if sorted(dims) != _list(_range(rank)):
        raise RuntimeError("permute(): dims must be a permutation of the tensor's dimensions")
    storage, shape = _k.permute(a._s, a.shape, _list(dims))
    out = Tensor(storage, shape, a.dtype)
    if _needs_grad(a):
        inverse = [0] * rank
        for i, d in enumerate(dims):
            inverse[d] = i
        out.requires_grad = True
        out._node = _Node(lambda g: (permute(g, *inverse),), (a,), "Permute")
    return out


def transpose(a, d0, d1):
    rank = _len(a.shape)
    d0, d1 = _norm_dim(d0, rank), _norm_dim(d1, rank)
    dims = _list(_range(rank))
    dims[d0], dims[d1] = dims[d1], dims[d0]
    return permute(a, *dims)


swapaxes = transpose


def movedim(a, source, destination):
    rank = _len(a.shape)
    s, d = _norm_dim(source, rank), _norm_dim(destination, rank)
    dims = [i for i in _range(rank) if i != s]
    dims.insert(d, s)
    return permute(a, *dims)


def expand(a, *sizes):
    sizes = _shape_args(sizes)
    rank = _len(a.shape)
    if _len(sizes) < rank:
        raise RuntimeError("expand: the number of sizes provided (%d) must be greater or equal to the number of dimensions in the tensor (%d)" % (_len(sizes), rank))
    off = _len(sizes) - rank
    target = []
    for i, s in enumerate(sizes):
        if s == -1:
            if i < off:
                raise RuntimeError("expand: -1 is not allowed in a leading, non-existing dimension")
            target.append(a.shape[i - off])
        else:
            target.append(s)
    target = _tuple(target)
    if target == _tuple(a.shape):
        return a
    out = Tensor(_k.expand(a._s, a.shape, _list(target)), target, a.dtype)
    if _needs_grad(a):
        out.requires_grad = True
        out._node = _Node(lambda g: (_unbroadcast(g, a.shape),), (a,), "Expand")
    return out


broadcast_to = expand


def repeat(a, *sizes):
    sizes = _shape_args(sizes)
    off = _len(sizes) - _len(a.shape)
    x = a.reshape(*([1] * off + _list(a.shape)))
    inter = []
    for i, s in enumerate(sizes):
        inter += [s, x.shape[i]]
    y = x.reshape(*[1 if j % 2 == 0 else x.shape[j // 2] for j in _range(2 * _len(sizes))])
    y = expand(y, *inter)
    return y.reshape(*[sizes[i] * x.shape[i] for i in _range(_len(sizes))])


def narrow(a, dim, start, length):
    d = _norm_dim(dim, _len(a.shape))
    spec = [slice(None)] * d + [slice(start, start + length)]
    return a[_tuple(spec)]


def cat(tensors, dim=0):
    tensors = [_as_tensor(t) for t in tensors]
    if not tensors:
        raise RuntimeError("torch.cat(): expected a non-empty list of Tensors")
    rank = _len(tensors[0].shape)
    d = _norm_dim(dim, rank)
    storage, shape = _k.cat([(t._s, t.shape) for t in tensors], d)
    out = Tensor(storage, shape, _DTYPES[_k.dtype(storage)])
    if _needs_grad(*tensors):
        sizes = [t.shape[d] for t in tensors]

        def backward(g):
            grads, start = [], 0
            for t, s in zip(tensors, sizes):
                grads.append(narrow(g, d, start, s) if t.requires_grad else None)
                start += s
            return _tuple(grads)
        out.requires_grad = True
        out._node = _Node(backward, _tuple(tensors), "Cat")
    return out


concat = cat
concatenate = cat


def stack(tensors, dim=0):
    tensors = _list(tensors)
    if not tensors:
        raise RuntimeError("stack expects a non-empty TensorList")
    d = _norm_dim(dim, _len(tensors[0].shape) + 1)
    return cat([unsqueeze(t, d) for t in tensors], d)


def split(a, size, dim=0):
    d = _norm_dim(dim, _len(a.shape))
    n = a.shape[d]
    if _isinstance(size, _int):
        sizes = [size] * (n // size) + ([n % size] if n % size else [])
    else:
        sizes = _list(size)
    out, start = [], 0
    for s in sizes:
        out.append(narrow(a, d, start, s))
        start += s
    return _tuple(out)


def chunk(a, chunks, dim=0):
    d = _norm_dim(dim, _len(a.shape))
    size = -(-a.shape[d] // chunks)
    return split(a, size, d)


def roll(a, shifts, dims=None):
    if dims is None:
        return roll(a.flatten(), shifts, 0).reshape(*a.shape)
    if _isinstance(shifts, (_list, _tuple)):
        out = a
        for s, d in zip(shifts, dims):
            out = roll(out, s, d)
        return out
    d = _norm_dim(dims, _len(a.shape))
    out = Tensor(_k.roll(a._s, a.shape, _int(shifts), d), a.shape, a.dtype)
    if _needs_grad(a):
        out.requires_grad = True
        out._node = _Node(lambda g: (roll(g, -_int(shifts), d),), (a,), "Roll")
    return out


def flip(a, dims):
    out = a
    for d in ([dims] if _isinstance(dims, _int) else dims):
        d = _norm_dim(d, _len(a.shape))
        idx = arange(a.shape[d] - 1, -1, -1)
        out = index_select(out, d, idx)
    return out


def diag(a):
    if _len(a.shape) == 1:
        return diag_embed(a)
    if _len(a.shape) == 2:
        n = _b.min(a.shape[0], a.shape[1])
        idx = arange(n)
        return a[idx, idx]
    raise RuntimeError("diag(): Supports 1D or 2D tensors")


def diag_embed(a):
    n = a.shape[-1]
    return mul(unsqueeze(a, -1), eye(n, dtype=a.dtype))


def tril(a, diagonal=0):
    r, c = a.shape[-2], a.shape[-1]
    mask = (unsqueeze(arange(c), 0) <= unsqueeze(arange(r), 1) + diagonal)
    return where(mask, a, zeros_like(a))


def triu(a, diagonal=0):
    r, c = a.shape[-2], a.shape[-1]
    mask = (unsqueeze(arange(c), 0) >= unsqueeze(arange(r), 1) + diagonal)
    return where(mask, a, zeros_like(a))


# ---- indexing ----------------------------------------------------------------------------
def _basic_spec(a, key):
    """Split an index into the basic part (ints, slices, None, Ellipsis) and
    the advanced tensor indices with the dims they apply to."""
    if not _isinstance(key, _tuple):
        key = (key,)
    if _b.any(_isinstance(k, Tensor) and k.dtype is _bool_dtype for k in key):
        expanded = []
        for k in key:
            if _isinstance(k, Tensor) and k.dtype is _bool_dtype:
                expanded.extend(nonzero(k).unbind(1))
            else:
                expanded.append(k)
        key = _tuple(expanded)
    rank = _len(a.shape)
    consumed = 0
    for k in key:
        if k is not None and k is not Ellipsis:
            consumed += 1
    if consumed > rank:
        raise IndexError("too many indices for tensor of dimension %d" % rank)
    items = []
    for k in key:
        if k is Ellipsis:
            items.extend([slice(None)] * (rank - consumed))
        else:
            items.append(k)
    return items


def _getitem(a, key):
    items = _basic_spec(a, key)
    spec, advanced, new_axes = [], [], []
    d = 0
    for k in items:
        if k is None:
            new_axes.append(_len(spec) - _b.sum(1 for s in spec if _isinstance(s, _int)) + _len(new_axes))
            continue
        if _isinstance(k, Tensor):
            if k.dtype.is_floating_point:
                raise IndexError("tensors used as indices must be long, int, byte or bool tensors")
            advanced.append((d, k.long()))
            spec.append(slice(None))
        elif _isinstance(k, (_list, _tuple)):
            advanced.append((d, tensor(k, dtype=int64)))
            spec.append(slice(None))
        elif _isinstance(k, _b.bool):
            raise IndexError("bool indices are not supported")
        elif _isinstance(k, _int):
            spec.append(k)
        elif _isinstance(k, slice):
            spec.append((k.start, k.stop, k.step))
        elif hasattr(k, "__index__"):
            spec.append(_int(k))
        else:
            raise IndexError("unsupported index %r" % (k,))
        d += 1
    while _len(spec) < _len(a.shape):
        spec.append(slice(None))
    spec = [(None, None, None) if _isinstance(s, slice) else s for s in spec]
    out = _slice(a, spec)
    if advanced:
        # Positions of the advanced dims after the basic step (ints drop dims).
        kept = []
        for i, s in enumerate(spec):
            if not _isinstance(s, _int):
                kept.append(i)
        adv_dims = [kept.index(pos) for pos, _ in advanced]
        out = _advanced_get(out, adv_dims, [t for _, t in advanced])
    for ax in new_axes:
        out = unsqueeze(out, ax)
    return out


def _slice(a, spec):
    storage, shape = _k.slice(a._s, a.shape, spec)
    out = Tensor(storage, shape, a.dtype)
    if _needs_grad(a):
        def backward(g):
            base = zeros_like(a)
            _k.setslice(base._s, base.shape, spec, g._s, g.shape)
            return (base,)
        out.requires_grad = True
        out._node = _Node(backward, (a,), "Slice")
    return out


def _broadcast_indices(indices):
    shape = ()
    for t in indices:
        shape = _broadcast_shapes(shape, _tuple(t.shape))
    return [expand(t, *shape) if _tuple(t.shape) != shape else t for t in indices], shape


def _broadcast_shapes(a, b):
    n = _b.max(_len(a), _len(b))
    a = (1,) * (n - _len(a)) + _tuple(a)
    b = (1,) * (n - _len(b)) + _tuple(b)
    out = []
    for x, y in zip(a, b):
        if x == y or y == 1:
            out.append(x)
        elif x == 1:
            out.append(y)
        else:
            raise IndexError("shape mismatch: indexing tensors could not be broadcast together with shapes %s, %s" % (a, b))
    return _tuple(out)


def _advanced_get(a, dims, indices):
    indices, ishape = _broadcast_indices(indices)
    rank = _len(a.shape)
    k = _len(dims)
    adjacent = dims == _list(_range(dims[0], dims[0] + k))
    order = dims + [i for i in _range(rank) if i not in dims]
    x = permute(a, *order) if order != _list(_range(rank)) else a
    storage, shape = _k.gather(x._s, x.shape, [t._s for t in indices], ishape)
    out = Tensor(storage, shape, a.dtype)
    if _needs_grad(a):
        def backward(g):
            base = zeros(*x.shape, dtype=g.dtype)
            _scatter_add(base, [t for t in indices], ishape, g)
            if order != _list(_range(rank)):
                inverse = [0] * rank
                for i, dd in enumerate(order):
                    inverse[dd] = i
                base = permute(base, *inverse)
            return (base,)
        out.requires_grad = True
        out._node = _Node(backward, (a,), "Index")
    if adjacent and dims[0] > 0:
        # Index dims take the place of the first advanced dim.
        r = _len(out.shape)
        ni = _len(ishape)
        move = _list(_range(ni, ni + dims[0])) + _list(_range(ni)) + _list(_range(ni + dims[0], r))
        out = permute(out, *move)
    return out


def _scatter_add(base, indices, ishape, values):
    # base is fresh; accumulate values at the indexed positions.
    _k.scatter_add(base._s, base.shape, [t._s for t in indices], ishape, values._s, values.shape)


def _scatter_dim(base, dim, index, values):
    """Scatter `values` into `base` along `dim` at `index` (same shape as values)."""
    idx = [index if i == dim else _dim_index(base.shape, i, index.shape) for i in _range(_len(base.shape))]
    idx, ishape = _broadcast_indices(idx)
    _k.scatter(base._s, base.shape, [t._s for t in idx], ishape, values._s, values.shape)
    return base


def _dim_index(shape, i, target):
    view = [1] * _len(target)
    view[i] = shape[i]
    return expand(arange(shape[i]).reshape(*view), *target)


def _setitem(a, key, value):
    value = _as_tensor(value, a)
    if value.dtype != a.dtype:
        value = value.to(a.dtype)
    if not _needs_grad(a, value):
        _setitem_into(a._s, a, key, value)
        return
    # Under autograd the assignment is an index_put: the overwritten
    # positions pass no gradient back to `a`, and `value` receives the
    # gradient of the positions it filled.
    if a.requires_grad and a._node is None:
        raise RuntimeError("a leaf Variable that requires grad is being used in an in-place operation.")
    storage = _k.copy(a._s)
    _setitem_into(storage, a, key, value)
    shape, dt = a.shape, a.dtype

    def backward(g):
        keep = ones(*shape, dtype=g.dtype)
        _setitem_into(keep._s, keep, key, tensor(0, dtype=g.dtype))
        return (mul(g, keep), _getitem(g, key))
    out = Tensor(storage, shape, dt, True, None)
    out._node = _Node(backward, (a, value), "IndexPut")
    a._inplace(out)


def _setitem_into(dst, a, key, value):
    """Assign `value` into the storage `dst` laid out like `a` at `key`."""
    items = _basic_spec(a, key)
    spec, advanced = [], []
    d = 0
    for k in items:
        if k is None:
            raise IndexError("None is not supported in assignment targets")
        if _isinstance(k, Tensor) or _isinstance(k, (_list, _tuple)):
            advanced.append((d, k.long() if _isinstance(k, Tensor) else tensor(k, dtype=int64)))
            spec.append((None, None, None))
        elif _isinstance(k, _int):
            spec.append(k)
        elif _isinstance(k, slice):
            spec.append((k.start, k.stop, k.step))
        else:
            spec.append(_int(k))
        d += 1
    while _len(spec) < _len(a.shape):
        spec.append((None, None, None))
    if not advanced:
        _k.setslice(dst, a.shape, spec, value._s, value.shape)
        return
    if _b.any(_isinstance(s, _int) or s != (None, None, None) for s in spec):
        raise IndexError("mixed basic and advanced assignment is not supported")
    dims = [pos for pos, _ in advanced]
    if dims != _list(_range(_len(dims))):
        raise IndexError("advanced assignment indices must be the leading dimensions")
    indices, ishape = _broadcast_indices([t for _, t in advanced])
    _k.scatter(dst, a.shape, [t._s for t in indices], ishape, value._s, value.shape)


def index_select(a, dim, index):
    d = _norm_dim(dim, _len(a.shape))
    storage, shape = _k.index_select(a._s, a.shape, d, index._s)
    out = Tensor(storage, shape, a.dtype)
    if _needs_grad(a):
        def backward(g):
            x = movedim(g, d, 0) if d != 0 else g
            base = zeros(*([a.shape[d]] + [s for i, s in enumerate(a.shape) if i != d]), dtype=g.dtype)
            _k.scatter_add(base._s, base.shape, [index._s], index.shape, x._s, x.shape)
            return (movedim(base, 0, d) if d != 0 else base,)
        out.requires_grad = True
        out._node = _Node(backward, (a,), "IndexSelect")
    return out


def gather(a, dim, index):
    d = _norm_dim(dim, _len(a.shape))
    idx = [_dim_index(index.shape, i, index.shape) for i in _range(_len(index.shape))]
    idx[d] = index
    return _advanced_get(a, _list(_range(_len(idx))), idx)


def nonzero(a):
    flat = _k.to_list(a._s)
    coords = [[] for _ in a.shape]
    strides = a.stride()
    for i, v in enumerate(flat):
        if v:
            rest = i
            for dd, s in enumerate(strides):
                coords[dd].append(rest // s)
                rest %= s
    if not a.shape:
        return tensor([[]] if flat[0] else [], dtype=int64).reshape(1 if flat[0] else 0, 0)
    return stack([tensor(c, dtype=int64) for c in coords], 1) if coords else zeros(0, 0, dtype=int64)


# ---- linear algebra ----------------------------------------------------------------------
def matmul(a, b):
    if _graph_recording:
        if getattr(a, "_zipp_graph", False):
            return a @ b
        if getattr(b, "_zipp_graph", False):
            return b.__rmatmul__(a)
    ta, tb = _as_tensor(a), _as_tensor(b)
    storage, shape = _k.matmul(ta._s, ta.shape, tb._s, tb.shape)
    out = Tensor(storage, shape, _DTYPES[_k.dtype(storage)])
    if _needs_grad(ta, tb):
        sa, sb, ra, rb = ta._s, tb._s, ta.requires_grad, tb.requires_grad

        def backward(g):
            x, y = _frozen(ta, sa), _frozen(tb, sb)
            gx = gy = None
            xs, ys = x, y
            if _len(x.shape) == 1:
                xs = unsqueeze(x, 0)
            if _len(y.shape) == 1:
                ys = unsqueeze(y, 1)
            gg = g
            if _len(x.shape) == 1:
                gg = unsqueeze(gg, -2)
            if _len(y.shape) == 1:
                gg = unsqueeze(gg, -1)
            if ra:
                gx = _unbroadcast(matmul(gg, transpose(ys, -1, -2)), xs.shape)
                if _len(x.shape) == 1:
                    gx = gx.reshape(*x.shape)
            if rb:
                gy = _unbroadcast(matmul(transpose(xs, -1, -2), gg), ys.shape)
                if _len(y.shape) == 1:
                    gy = gy.reshape(*y.shape)
            return (gx, gy)
        out.requires_grad = True
        out._node = _Node(backward, (ta, tb), "Mm")
    return out


mm = matmul
bmm = matmul


def dot(a, b):
    return sum(mul(a, b))


def outer(a, b):
    return mul(unsqueeze(a, 1), unsqueeze(b, 0))


def einsum(spec, *operands):
    """einsum by broadcasting: align every operand's labels, multiply, then
    sum the labels absent from the output. Fine for the small tensors this
    runtime targets; every step keeps autograd."""
    if _len(operands) == 1 and _isinstance(operands[0], (_list, _tuple)):
        operands = _tuple(operands[0])
    spec = spec.replace(" ", "")
    if "->" in spec:
        lhs, rhs = spec.split("->")
    else:
        lhs = spec
        counts = {}
        for c in lhs.replace(",", ""):
            counts[c] = counts.get(c, 0) + 1
        rhs = "".join(sorted(c for c in counts if counts[c] == 1))
    ins = lhs.split(",")
    if _len(ins) != _len(operands):
        raise ValueError("einsum(): more operands were provided than specified in the equation" if _len(ins) < _len(operands) else "einsum(): fewer operands were provided than specified in the equation")
    labels = []
    for term in ins:
        for c in term:
            if c not in labels:
                labels.append(c)
    for c in rhs:
        if c not in labels:
            raise ValueError("einsum(): output subscript %s does not appear in any input" % c)
    sizes = {}
    aligned = []
    for term, op in zip(ins, operands):
        if _len(term) != _len(op.shape):
            raise ValueError("einsum(): the number of subscripts in the equation (%d) does not match the number of dimensions (%d) for operand" % (_len(term), _len(op.shape)))
        for c, s in zip(term, op.shape):
            if c in sizes and sizes[c] != s and s != 1 and sizes[c] != 1:
                raise ValueError("einsum(): operands do not broadcast with remapped shapes")
            sizes[c] = _b.max(sizes.get(c, 1), s)
        order = [term.index(c) for c in labels if c in term]
        x = permute(op, *order) if order != _list(_range(_len(order))) else op
        shape = [op.shape[term.index(c)] if c in term else 1 for c in labels]
        aligned.append(x.reshape(*shape))
    prod = aligned[0]
    for x in aligned[1:]:
        prod = mul(prod, x)
    if _len(aligned) == 1:
        prod = expand(prod, *[sizes[c] for c in labels])
    reduce_dims = [i for i, c in enumerate(labels) if c not in rhs]
    out = sum(prod, reduce_dims) if reduce_dims else prod
    remaining = [c for c in labels if c in rhs]
    order = [remaining.index(c) for c in rhs]
    if order != _list(_range(_len(order))):
        out = permute(out, *order)
    return out


def equal(a, b):
    return _tuple(a.shape) == _tuple(b.shape) and _bool(_k.equal(a._s, b._s))


def allclose(a, b, rtol=1e-05, atol=1e-08, equal_nan=False):
    a, b = _as_tensor(a), _as_tensor(b)
    if _tuple(a.shape) != _tuple(b.shape):
        b = expand(b, *_broadcast_shapes(a.shape, b.shape)) if _numel(b.shape) == 1 else b
        a = expand(a, *_broadcast_shapes(a.shape, b.shape)) if _numel(a.shape) == 1 else a
        if _tuple(a.shape) != _tuple(b.shape):
            return False
    return _bool(_k.allclose(a._s, b._s, _float(rtol), _float(atol)))


def isclose(a, b, rtol=1e-05, atol=1e-08):
    return abs(sub(a, b)) <= add(atol, mul(rtol, abs(b)))


def softmax(a, dim=-1, dtype=None):
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.softmax(dim, dtype)
    if dtype is not None:
        a = a.to(dtype)
    d = _norm_dim(dim, _len(a.shape))
    out = Tensor(_k.softmax(a._s, a.shape, d, False), a.shape, float32 if not a.dtype.is_floating_point else a.dtype)
    if _needs_grad(a):
        out.requires_grad = True
        so = out._s

        def backward(g):
            o = _frozen(out, so)
            return (mul(o, sub(g, sum(mul(g, o), d, True))),)
        out._node = _Node(backward, (a,), "Softmax")
    return out


def log_softmax(a, dim=-1, dtype=None):
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.log_softmax(dim, dtype)
    if dtype is not None:
        a = a.to(dtype)
    d = _norm_dim(dim, _len(a.shape))
    out = Tensor(_k.softmax(a._s, a.shape, d, True), a.shape, float32 if not a.dtype.is_floating_point else a.dtype)
    if _needs_grad(a):
        out.requires_grad = True
        so = out._s
        out._node = _Node(lambda g: (sub(g, mul(exp(_frozen(out, so)), sum(g, d, True))),), (a,), "LogSoftmax")
    return out


def argsort(a, dim=-1, descending=False):
    d = _norm_dim(dim, _len(a.shape))
    return Tensor(_k.argsort(a._s, a.shape, d, _bool(descending)), a.shape, int64)


def sort(a, dim=-1, descending=False):
    return a.sort(dim, descending)


def topk(a, k, dim=-1, largest=True):
    return a.topk(k, dim, largest)


def clip_grad_norm_(parameters, max_norm, norm_type=2.0):
    if _isinstance(parameters, Tensor):
        parameters = [parameters]
    grads = [p.grad for p in parameters if p.grad is not None]
    if not grads:
        return tensor(0.0)
    total = _math.sqrt(_b.sum(_float(_k.dot_sum(g._s, g._s)) for g in grads))
    coef = _float(max_norm) / (total + 1e-6)
    if coef < 1.0:
        for g in grads:
            _k.copy_into(g._s, mul(g, coef)._s)
    return tensor(total)


# ---- autograd engine ---------------------------------------------------------------------
def _backward(root, grad, accumulate=True, inputs=None, retain=False):
    order, seen = [], set()
    parents_of, aliases = {}, {}

    def resolve(node):
        # A parent rebound by an in-place op since this node consumed it
        # stands in as an alias carrying the history it had then (one alias
        # per earlier version, shared by every node that consumed it).
        out = []
        for p, st in zip(node.parents, node.pstate):
            if p is not None and p._node is not st[0]:
                key = (id(p), id(st[1]))
                a = aliases.get(key)
                if a is None:
                    a = aliases[key] = Tensor(st[1], p.shape, p.dtype, st[2], st[0])
                p = a
            out.append(p)
        return out

    def visit(t):
        if id(t) in seen:
            return
        seen.add(id(t))
        if t._node is not None:
            ps = parents_of[id(t)] = resolve(t._node)
            for p in ps:
                if p is not None and p.requires_grad:
                    visit(p)
        order.append(t)
    visit(root)
    grads = {id(root): grad}
    captured = {} if inputs is not None else None
    wanted = None if inputs is None else set(id(t) for t in inputs)
    with no_grad():
        for t in reversed(order):
            g = grads.pop(id(t), None)
            if g is None:
                continue
            if wanted is not None and id(t) in wanted:
                captured[id(t)] = g if id(t) not in captured else add(captured[id(t)], g)
            if t._node is None:
                if accumulate and t.requires_grad and (wanted is None):
                    t.grad = g.detach() if t.grad is None else add(t.grad, g).detach()
                continue
            if t._retain and accumulate and wanted is None:
                t.grad = g.detach() if t.grad is None else add(t.grad, g).detach()
            parent_grads = t._node.backward(g)
            for p, pg in zip(parents_of[id(t)], parent_grads):
                if p is None or pg is None or not p.requires_grad:
                    continue
                if _tuple(pg.shape) != _tuple(p.shape):
                    pg = _unbroadcast(pg, p.shape) if _numel(pg.shape) >= _numel(p.shape) else expand(pg, *p.shape)
                if pg.dtype != p.dtype and p.dtype.is_floating_point:
                    pg = pg.to(p.dtype)
                grads[id(p)] = pg if id(p) not in grads else add(grads[id(p)], pg)
    return captured


def _autograd_grad(outputs, inputs, grad_outputs=None, retain_graph=None, create_graph=False, allow_unused=False):
    if True:
        outputs = [outputs] if _isinstance(outputs, Tensor) else _list(outputs)
        single = _isinstance(inputs, Tensor)
        inputs = [inputs] if single else _list(inputs)
        result = {}
        for i, out in enumerate(outputs):
            if grad_outputs is None:
                if _numel(out.shape) != 1:
                    raise RuntimeError("grad can be implicitly created only for scalar outputs")
                g = ones(*out.shape, dtype=out.dtype)
            else:
                g = grad_outputs[i] if _isinstance(grad_outputs, (_list, _tuple)) else grad_outputs
            captured = _backward(out, g, accumulate=False, inputs=inputs)
            for k, v in captured.items():
                result[k] = v if k not in result else add(result[k], v)
        grads = []
        for t in inputs:
            g = result.get(id(t))
            if g is None and not allow_unused:
                raise RuntimeError("One of the differentiated Tensors appears to not have been used in the graph. Set allow_unused=True if this is the desired behavior.")
            grads.append(g)
        return _tuple(grads)


class _Autograd:
    grad = staticmethod(_autograd_grad)
    no_grad = no_grad
    enable_grad = enable_grad
    set_grad_enabled = staticmethod(set_grad_enabled)


autograd = _Autograd()


# ---- environment knobs -------------------------------------------------------------------
_threads = 1


def set_num_threads(n):
    global _threads
    _threads = _int(n)


def get_num_threads():
    return _threads


def use_deterministic_algorithms(mode, warn_only=False):
    return None


def set_default_dtype(dt):
    global _default_dtype
    _default_dtype = _dtype_of(dt)


def get_default_dtype():
    return _default_dtype


def is_tensor(v):
    return _isinstance(v, Tensor)


def numel(t):
    return t.numel()


class _Cuda:
    @staticmethod
    def is_available():
        return False

    @staticmethod
    def manual_seed(seed):
        return None

    @staticmethod
    def manual_seed_all(seed):
        return None

    @staticmethod
    def device_count():
        return 0


cuda = _Cuda()


class _Backends:
    class _Zipp:
        available = True
        description = "Zipp CPU kernels (JavaScript typed arrays on the engine)"

    zipp = _Zipp()

    class cudnn:
        deterministic = False
        benchmark = False


backends = _Backends()


# ---- checkpoints -------------------------------------------------------------------------
class _Storage:
    """A typed storage as pickled by PyTorch; the class name is what a
    checkpoint refers to."""
    dtype = float32

    def __init__(self, storage=None):
        self._s = storage


class FloatStorage(_Storage):
    dtype = float32


class DoubleStorage(_Storage):
    dtype = float64


class LongStorage(_Storage):
    dtype = int64


class IntStorage(_Storage):
    dtype = int32


class BoolStorage(_Storage):
    dtype = _bool_dtype


class ByteStorage(_Storage):
    dtype = uint8


_STORAGE_TYPES = {"float32": FloatStorage, "float64": DoubleStorage, "int64": LongStorage, "int32": IntStorage, "bool": BoolStorage, "uint8": ByteStorage}


def _rebuild_tensor_v2(storage, storage_offset, size, stride, requires_grad=False, backward_hooks=None, metadata=None):
    shape = _tuple(_int(d) for d in size)
    n = _numel(shape)
    st = storage._s
    if _int(storage_offset) != 0 or _k.size(st) != n:
        flat = _k.to_list(st)[_int(storage_offset):_int(storage_offset) + n]
        st = _k.from_flat(_k.dtype(st), flat)
    t = Tensor(st, shape, _DTYPES[_k.dtype(st)])
    t.requires_grad = _bool(requires_grad)
    return t


def save(obj, f, pickle_protocol=2):
    import pickle
    import zipfile
    import os
    name = os.path.basename(str(f)) if not hasattr(f, "write") else "archive"
    name = name.rsplit(".", 1)[0] if "." in name else name
    records = []

    def persistent_id(value):
        if _isinstance(value, Tensor):
            return None
        if _isinstance(value, _Storage):
            key = str(_len(records))
            records.append(_k.tobytes(value._s))
            return ("storage", type(value), key, "cpu", _k.size(value._s))
        return None

    def reduce_tensor(t):
        st = _STORAGE_TYPES[t.dtype.name](t._s)
        return (_rebuild_tensor_v2, (st, 0, _tuple(t.shape), t.stride(), _bool(t.requires_grad), _OrderedDict()))

    data = pickle.dumps(obj, protocol=2, persistent_id=persistent_id, reducers={Tensor: reduce_tensor})
    with zipfile.ZipFile(f, "w") as z:
        z.writestr(name + "/data.pkl", data)
        for i, blob in enumerate(records):
            z.writestr(name + "/data/" + str(i), blob)
        z.writestr(name + "/version", "3\n")
        z.writestr(name + "/byteorder", "little")


def load(f, map_location=None, weights_only=True, **kwargs):
    import pickle
    import zipfile
    with zipfile.ZipFile(f, "r") as z:
        names = z.namelist()
        pkl = [n for n in names if n.endswith("/data.pkl") or n == "data.pkl"]
        if not pkl:
            raise RuntimeError("not a torch checkpoint: no data.pkl record")
        prefix = pkl[0][:-_len("data.pkl")]
        blobs = {}

        def persistent_load(pid):
            kind, storage_type, key, location, numel = pid[0], pid[1], pid[2], pid[3], pid[4]
            if kind != "storage":
                raise pickle.UnpicklingError("unknown persistent id")
            if key not in blobs:
                raw = z.read(prefix + "data/" + str(key))
                dt = storage_type.dtype if _isinstance(storage_type, type) else _DTYPES[str(storage_type)]
                blobs[key] = storage_type(_k.frombytes(dt.name, raw, _int(numel)))
            return blobs[key]

        def find_class(module, name):
            if name == "_rebuild_tensor_v2" and module in ("torch._utils", "torch"):
                return _rebuild_tensor_v2
            if module == "torch" and name in _STORAGE_NAMES:
                return _STORAGE_NAMES[name]
            if module == "collections" and name == "OrderedDict":
                return _OrderedDict
            if name == "_rebuild_parameter" and module in ("torch._utils", "torch"):
                return lambda data, requires_grad, hooks: data.requires_grad_(requires_grad)
            if module == "torch" and name in ("float32", "float64", "int64", "int32", "bool", "uint8"):
                return _DTYPES[name]
            raise pickle.UnpicklingError("Weights only load failed: global %s.%s is not allowed" % (module, name))
        return pickle.loads(z.read(pkl[0]), persistent_load=persistent_load, find_class=find_class)


_STORAGE_NAMES = {c.__name__: c for c in (FloatStorage, DoubleStorage, LongStorage, IntStorage, BoolStorage, ByteStorage)}
from collections import OrderedDict as _OrderedDict

# The dtype aliases that shadow builtins go last, after every use of the
# real `int`, `float` and `bool` above (module functions use the `_int`
# aliases, never the bare names).
float = float32
double = float64
long = int64
int = int32
bool = _bool_dtype
half = float32
bfloat16 = float32
FloatTensor = Tensor
LongTensor = Tensor
inf = _math.inf
nan = _math.nan
pi = _math.pi
e = _math.e

import torch.nn as nn
import torch.optim as optim
import torch.nn.functional as _F
