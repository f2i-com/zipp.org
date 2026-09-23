"""A PyTorch-compatible tensor library for Zipp: contiguous float16/bfloat16/
float32/float64, complex64/complex128, int8/int16/int32/int64, uint8 and bool tensors with broadcasting, reverse-mode autograd, and the modules,
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
_issubclass = _b.issubclass


_ITEMSIZE = {"float64": 8, "int64": 8, "float32": 4, "int32": 4, "float16": 2, "bfloat16": 2, "int16": 2,
             "complex64": 8, "complex128": 16, "complex32": 4}


class dtype:
    def __init__(self, name, is_floating):
        self.name = name
        self.is_floating_point = is_floating
        self.itemsize = _ITEMSIZE.get(name, 1)
        self.is_signed = name not in ("uint8", "bool")
        # complex64/complex128: interleaved (real, imaginary) pairs of
        # float32/float64 (`_zipp_tensor`'s pair storages). Not floating
        # point, as in PyTorch; `_inexact` is "floating or complex".
        self.is_complex = name.startswith("complex")
        self._inexact = is_floating or self.is_complex
        # float16/bfloat16: two bytes per element (a Float16Array, or the
        # upper float32 halves in a Uint16Array), every result rounded to
        # the format; arithmetic follows PyTorch's float `opmath`.
        self._reduced = name in ("float16", "bfloat16")

    def to_real(self):
        return _TO_REAL.get(self.name, self)

    def to_complex(self):
        return _TO_COMPLEX.get(self.name, self)

    def __repr__(self):
        return "torch." + self.name

    def __eq__(self, other):
        return _isinstance(other, dtype) and other.name == self.name

    def __hash__(self):
        return hash(self.name)

    def __reduce__(self):
        # Pickled as the global torch.<name>, as PyTorch writes a dtype.
        return self.name


float32 = dtype("float32", True)
float64 = dtype("float64", True)
int64 = dtype("int64", False)
int32 = dtype("int32", False)
uint8 = dtype("uint8", False)
_bool_dtype = dtype("bool", False)
float16 = dtype("float16", True)
bfloat16 = dtype("bfloat16", True)
int8 = dtype("int8", False)
int16 = dtype("int16", False)
complex64 = dtype("complex64", False)
complex128 = dtype("complex128", False)
# complex32 (ComplexHalf) is named, but no tensor of it can be made here.
complex32 = dtype("complex32", False)
half = float16
short = int16
cfloat = complex64
cdouble = complex128
chalf = complex32
_DTYPES = {"float32": float32, "float64": float64, "int64": int64, "int32": int32, "uint8": uint8, "bool": _bool_dtype,
           "float16": float16, "bfloat16": bfloat16, "int8": int8, "int16": int16, "complex64": complex64, "complex128": complex128,
           "complex32": complex32}
_CPP_NAME = {"complex64": "c10::complex<float>", "complex128": "c10::complex<double>", "float32": "float", "float64": "double",
             "float16": "c10::Half", "bfloat16": "c10::BFloat16", "int64": "int64_t", "int32": "int", "int16": "int16_t", "int8": "int8_t",
             "uint8": "uint8_t", "bool": "bool"}
_TO_REAL = {"complex64": float32, "complex128": float64, "complex32": float16}
_TO_COMPLEX = {"float32": complex64, "float64": complex128, "float16": complex32}
_default_dtype = float32


class device:
    def __init__(self, kind="cpu"):
        if not (_isinstance(kind, device) or kind in ("cpu", "cpu:0")):
            raise RuntimeError("Eager Zipp torch supports CPU only; use torch.compile(..., backend='zipp_gpu') for GPU inference or training")
        self.type = "cpu"

    def __repr__(self):
        return "device(type='cpu')"

    def __str__(self):
        return "cpu"

    @property
    def index(self):
        return None

    def __eq__(self, other):
        # As PyTorch: a device equals a device, not the string naming it.
        return _isinstance(other, device)

    def __reduce__(self):
        return (device, ("cpu",))

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


# The kernels hand result shapes back as Size, so a Tensor takes them as they
# are instead of copying each into a new Size.
_k._set_size_type(Size)
# The shape of every 0-d tensor (a Size is immutable, so one is shared).
_SCALAR_SHAPE = Size(())
# Tuple equality of two shapes, without a tuple subclass's comparison dispatch.
_shape_eq = _k._shape_eq


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

# ---- CPU autocast -------------------------------------------------------------------------
# The lower-precision dtype while a CPU autocast region is enabled, else None
# (torch.amp keeps it). An op on one of PyTorch's CPU autocast lists tests it
# on entry -- one global load when autocast is off -- and runs through
# `_autocast_run`, which casts the op's floating inputs by its policy and runs
# it with autocast off, as PyTorch's autocast kernels call the op below the
# Autocast dispatch key: "lower" casts to the autocast dtype, "fp32" to
# float32, "promote" to the widest floating input, None casts nothing. float64
# and non-floating tensors are never cast.
_autocast_cpu = None
# While the region's cache is enabled, the casts of float32 leaf tensors that
# require grad (weights): id -> (tensor, cast), cleared as the outermost
# region exits (torch.amp).
_autocast_cache = None


def _autocast_cast(v, dt):
    if _isinstance(v, Tensor):
        vd = v.dtype
        if vd is dt or vd is float64 or not vd.is_floating_point:
            return v
        cache = _autocast_cache
        if cache is not None and vd is float32 and dt is _autocast_cpu and v.requires_grad and v._node is None:
            hit = cache.get(id(v))
            if hit is not None and hit[0] is v:
                return hit[1]
            out = v.to(dt)
            cache[id(v)] = (v, out)
            return out
        return v.to(dt)
    if type(v) is _list:
        return [_autocast_cast(x, dt) for x in v]
    if type(v) is _tuple:
        return _tuple([_autocast_cast(x, dt) for x in v])
    return v


def _autocast_widest(values, fast):
    """at::autocast::prioritize over the floating tensors in `values`
    (one list level deep), starting from the autocast dtype."""
    cur = fast
    for v in values:
        for t_ in (v if type(v) is _list or type(v) is _tuple else (v,)):
            if _isinstance(t_, Tensor) and t_.dtype.is_floating_point:
                d = t_.dtype
                if d is float64:
                    continue
                if cur is float32 or d is float32:
                    cur = float32
                elif not (cur is fast and d is fast):
                    raise RuntimeError("Unexpected floating ScalarType in at::autocast::prioritize")
    return cur


def _autocast_run(fn, policy, args, kwargs=None):
    """fn(*args, **kwargs) under the CPU autocast policy `policy`."""
    global _autocast_cpu
    fast = _autocast_cpu
    if policy is not None:
        dt = fast if policy == "lower" else float32 if policy == "fp32" else _autocast_widest(_list(args) + _list((kwargs or {}).values()), fast)
        args = _autocast_cast(_tuple(args), dt)
        if kwargs:
            kwargs = {k: _autocast_cast(v, dt) for k, v in kwargs.items()}
    _autocast_cpu = None
    try:
        return fn(*args, **(kwargs or {}))
    finally:
        _autocast_cpu = fast


class autocast:
    """Mixed precision, as a no-op: every tensor here is float32 already.

    Model code wraps the parts that must stay in full precision -- rotary
    tables, normalisation -- in `torch.autocast(..., enabled=False)`. That is
    exactly what this engine does everywhere, so honouring the request means
    doing nothing, and refusing it would fail code that is asking for the
    behaviour it already has.
    """

    def __init__(self, device_type=None, dtype=None, enabled=True, cache_enabled=None):
        self.device_type = device_type
        self.dtype = dtype
        self.enabled = enabled

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def __call__(self, function):
        def wrapper(*args, **kwargs):
            with self:
                return function(*args, **kwargs)
        return wrapper


def _decorate(make, fn):
    """`fn` running inside the context `make()` returns."""
    def wrapper(*a, **kw):
        with make():
            return fn(*a, **kw)
    wrapper.__name__ = getattr(fn, "__name__", "wrapper")
    wrapper.__doc__ = getattr(fn, "__doc__", None)
    wrapper.__wrapped__ = fn
    return wrapper


class _GradMode:
    """A context manager and decorator that sets the grad mode on entry and
    restores it on exit. Used bare as a decorator (`@torch.no_grad`), the
    class wraps the function directly."""
    _mode = False

    def __new__(cls, orig_func=None):
        if orig_func is not None and callable(orig_func):
            return _decorate(cls, orig_func)
        return object.__new__(cls)

    def __init__(self, orig_func=None):
        # PyTorch's initial `prev`: an __exit__ without __enter__ restores it.
        self._prev = False

    def __enter__(self):
        global _grad_enabled
        self._prev = _grad_enabled
        _grad_enabled = self._mode
        return self

    def __exit__(self, exc_type=None, exc=None, tb=None):
        global _grad_enabled
        _grad_enabled = self._prev
        return False

    def __call__(self, fn):
        return _decorate(type(self), fn)

    def clone(self):
        return type(self)()


class no_grad(_GradMode):
    """Operations inside record no gradient."""
    _mode = False


class enable_grad(_GradMode):
    """Operations inside record gradients, even within no_grad."""
    _mode = True


class inference_mode(_GradMode):
    """`inference_mode(True)` behaves as no_grad; `inference_mode(False)`
    leaves the grad mode as it is. There are no inference tensors: results
    are ordinary tensors that simply carry no history."""

    def __new__(cls, mode=True):
        if not _isinstance(mode, _b.bool) and callable(mode):
            return _decorate(cls, mode)
        return object.__new__(cls)

    def __init__(self, mode=True):
        self.mode = _bool(mode)
        self._prev = _grad_enabled

    def __enter__(self):
        global _grad_enabled
        self._prev = _grad_enabled
        if self.mode:
            _grad_enabled = False
        return self

    def __call__(self, fn):
        mode = self.mode
        return _decorate(lambda: inference_mode(mode), fn)

    def clone(self):
        return inference_mode(self.mode)


class set_grad_enabled(_GradMode):
    """Sets the grad mode when called, as a function or a context manager;
    leaving the `with` block restores the mode from before the call."""

    def __new__(cls, mode):
        return object.__new__(cls)

    def __init__(self, mode):
        global _grad_enabled
        self._prev = _grad_enabled
        self.mode = _bool(mode)
        _grad_enabled = self.mode

    def __enter__(self):
        return self

    def __call__(self, fn):
        # As a decorator it only wraps: the mode set on construction is undone.
        global _grad_enabled
        _grad_enabled = self._prev
        mode = self.mode
        return _decorate(lambda: set_grad_enabled(mode), fn)

    def clone(self):
        return set_grad_enabled(self.mode)

    def __repr__(self):
        return "torch.autograd.grad_mode.set_grad_enabled(mode=%s)" % self.mode


def is_grad_enabled():
    return _grad_enabled


def is_inference_mode_enabled():
    return False


def _autograd_version(s):
    return _k.aversion(s)


class _Node:
    # Whether backward is itself differentiable (create_graph=True); also
    # granted by name through `_DIFFERENTIABLE`. `fn`: the cached grad_fn.
    # Class defaults, and no __slots__: on this runtime a slot store costs
    # more than an instance-dict store, and a node is made per operation.
    diff = False
    fn = None
    saved = None

    def __init__(self, backward, parents, name, saved=None):
        self.backward = backward
        self.parents = parents
        self.name = name
        # Each parent's history, requires_grad and shape as this node
        # consumed it. An in-place op gives the tensor object a new history
        # afterwards; like PyTorch's edges, backward still reaches the
        # history it had here. (One and two parents are spelled out: on this
        # runtime a comprehension costs more than the tuples it builds.)
        n = _len(parents)
        if n == 1:
            p = parents[0]
            self.pstate = [None if p is None else (p._node, p.requires_grad, p.shape)]
        elif n == 2:
            p, q = parents
            self.pstate = [None if p is None else (p._node, p.requires_grad, p.shape), None if q is None else (q._node, q.requires_grad, q.shape)]
        else:
            self.pstate = [None if p is None else (p._node, p.requires_grad, p.shape) for p in parents]
        # The tensors whose values backward reads, each with its storage's
        # version now: writing one in place before backward is an error, as
        # in PyTorch (see `_check_saved`).
        if saved:
            if _len(saved) == 1:
                t = saved[0]
                s = t._s
                self.saved = [(s, _k.aversion(s), t)]
            else:
                self.saved = [(t._s, _k.aversion(t._s), t) for t in saved]


def _check_saved(node):
    for s, version, t in node.saved:
        now = _k.aversion(s)
        if now != version:
            shape = _list(t.shape)
            owner = "" if t._node is None else ", which is output 0 of %s," % _grad_fn_name(t._node.name)
            raise RuntimeError("one of the variables needed for gradient computation has been modified by an inplace operation: [torch.%s %s]%s is at version %d; expected version %d instead. Hint: the backtrace further above can show the operation that failed to compute its gradient." % (Tensor.type(t)[6:], shape, owner, now, version))


def _grad_fn_name(name):
    return name if name.endswith("Backward") or name == "CopySlices" or name.endswith("Backward1") else name + "Backward0"


class _GradFn:
    """What `Tensor.grad_fn` returns: the recorded operation, named as
    PyTorch names it (`MulBackward0`), with its `next_functions`."""

    def __init__(self, node):
        self._node = node

    def name(self):
        return type(self).__name__

    @property
    def next_functions(self):
        out = []
        for p, st in zip(self._node.parents, self._node.pstate):
            if p is None or not st[1]:
                out.append((None, 0))
            elif st[0] is None:
                out.append((AccumulateGrad(p), 0))
            else:
                out.append((_node_fn(st[0]), 0))
        return _tuple(out)

    def __call__(self, *grads):
        return self._node.backward(*grads)

    def __repr__(self):
        return "<%s object>" % type(self).__name__


class AccumulateGrad:
    def __init__(self, variable):
        self.variable = variable
        self.next_functions = ()

    def name(self):
        return "torch::autograd::AccumulateGrad"

    def __repr__(self):
        return "<AccumulateGrad object>"


_GRAD_FN_TYPES = {}


def _node_fn(node):
    fn = node.fn
    if fn is None:
        name = _grad_fn_name(node.name)
        cls = _GRAD_FN_TYPES.get(name)
        if cls is None:
            cls = _GRAD_FN_TYPES[name] = type(name, (_GradFn,), {})
        fn = node.fn = cls(node)
    return fn


class _HookHandle:
    def __init__(self, hooks, hook):
        self._hooks = hooks
        self._hook = hook

    def remove(self):
        if self._hook in self._hooks:
            self._hooks.remove(self._hook)


def _frozen(t, s):
    """`t` with the storage `s` a node saw: `.data =` may have rebound it since."""
    return t if t._s is s else Tensor(s, t.shape, _DTYPES[_k.dtype(s)])


# ---- type promotion ----------------------------------------------------------------------
# PyTorch's result_type: bool < integer < floating categories; within the
# winning category a dimensioned tensor's dtype outranks a 0-d tensor's,
# which outranks a Python scalar's (int -> int64, float -> the default dtype).
# Not a total order: uint8 with int8 promotes to int16 and float16 with
# bfloat16 to float32 (the pairs of equal rank).
_RANK = {"bool": 0, "uint8": 1, "int8": 1, "int16": 2, "int32": 3, "int64": 4, "float16": 5, "bfloat16": 5, "float32": 6, "float64": 7,
         "complex32": 8, "complex64": 9, "complex128": 10}
_CAST_NAME = {"float32": "Float", "float64": "Double", "int64": "Long", "int32": "Int", "bool": "Bool", "uint8": "Byte",
              "float16": "Half", "bfloat16": "BFloat16", "int8": "Char", "int16": "Short", "complex64": "ComplexFloat",
              "complex128": "ComplexDouble", "complex32": "ComplexHalf"}


def _promote_types(x, y):
    if x is None:
        return y
    if y is None or x is y:
        return x
    rx, ry = _RANK[x.name], _RANK[y.name]
    if rx >= 8 or ry >= 8:
        # A complex dtype wins, wide enough for the other's real values
        # (complex64 with float64 is complex128).
        if x is complex128 or y is complex128 or x is float64 or y is float64:
            return complex128
        return complex64
    if rx == ry:
        return int16 if rx == 1 else float32
    return x if rx > ry else y


promote_types = _promote_types


def _combine_categories(higher, lower):
    if higher is None:
        return lower
    if lower is None:
        return higher
    if higher.is_complex:
        return higher
    if lower.is_complex:
        # A higher-priority floating dtype keeps its precision, made complex
        # (c10's combine_categories).
        return _TO_COMPLEX.get(higher.name, complex64) if higher.is_floating_point else lower
    if higher.is_floating_point:
        return higher
    if higher is _bool_dtype or lower.is_floating_point:
        return _promote_types(higher, lower)
    return higher


def _scalar_dtype(v):
    if _isinstance(v, _b.bool):
        return _bool_dtype
    if _isinstance(v, _int):
        return int64
    if _isinstance(v, _float):
        return _default_dtype
    if _cx_parts(v) is not None:
        return _default_complex()
    raise TypeError("unsupported operand type for a tensor operation: '%s'" % type(v).__name__)


def _result_type(*operands):
    dim_t = zero_t = wrapped = None
    for v in operands:
        if _isinstance(v, Tensor):
            if v.shape:
                dim_t = _promote_types(dim_t, v.dtype)
            else:
                zero_t = _promote_types(zero_t, v.dtype)
        elif v is not None:
            wrapped = _promote_types(wrapped, _scalar_dtype(v))
    return _combine_categories(dim_t, _combine_categories(zero_t, wrapped))


def result_type(tensor1, tensor2):
    return _result_type(tensor1, tensor2)


def can_cast(from_, to):
    return _CATEGORY[from_.name] <= _CATEGORY[to.name]


_CATEGORY = {"bool": 0, "uint8": 1, "int8": 1, "int16": 1, "int32": 1, "int64": 1, "float16": 2, "bfloat16": 2, "float32": 2, "float64": 2,
             "complex32": 3, "complex64": 3, "complex128": 3}


def _cast_0d(t_, dt):
    """A 0-d operand converted to the result dtype: differentiably when it
    requires grad (the gradient flows back through the cast)."""
    if _grad_enabled and t_.requires_grad:
        return t_.to(dt)
    return Tensor(_k.astype(t_._s, dt.name), (), dt)


def _operands(a, b, opmath=False):
    """Both operands as tensors plus the promoted result dtype (None when the
    kernel's own promotion of the two storages already gives it). A Python
    scalar becomes a 0-d tensor of the result dtype, as PyTorch converts it.
    `opmath` (mul, div): a float16/bfloat16 result keeps a scalar second
    operand in float32 instead, as PyTorch's kernels read it."""
    if _isinstance(a, Tensor):
        if _isinstance(b, Tensor):
            if a.dtype is b.dtype:
                return a, b, None
            dt = _result_type(a, b)
            if opmath and dt._reduced and not b.shape and a.dtype is dt:
                return a, (b if b.dtype is float32 else _cast_0d(b, float32)), dt
            # Only a 0-d operand can lose to the other's dtype; cast it, so
            # the kernel computes in the result dtype as PyTorch does.
            if b.dtype is not dt and not b.shape:
                b = _cast_0d(b, dt)
            elif a.dtype is not dt and not a.shape:
                a = _cast_0d(a, dt)
            return a, b, dt
        tb = type(b)
        if (tb is _float or tb is _int) and not _graph_recording:
            # `_scalar_result` and `_as_tensor` for a plain float or int
            # (while torch.compile records, `_as_tensor` is watched: below).
            dt = a.dtype
            if not dt._inexact:
                dt = _default_dtype if tb is _float else (int64 if dt is _bool_dtype else dt)
            if opmath and dt._reduced:
                return a, Tensor(_k.full("float32", 1, b), _SCALAR_SHAPE, float32), dt
            return a, Tensor(_k.full(dt.name, 1, b), _SCALAR_SHAPE, dt), dt
        dt = _scalar_result(a.dtype, b)
        return a, _as_tensor(b, None, dt), dt
    if _isinstance(b, Tensor):
        ta = type(a)
        if (ta is _float or ta is _int) and not _graph_recording:
            dt = b.dtype
            if not dt._inexact:
                dt = _default_dtype if ta is _float else (int64 if dt is _bool_dtype else dt)
            return Tensor(_k.full(dt.name, 1, a), _SCALAR_SHAPE, dt), b, dt
        dt = _scalar_result(b.dtype, a)
        return _as_tensor(a, None, dt), b, dt
    return tensor(a), tensor(b), None


def _scalar_result(dt, v):
    if dt.is_complex or ((dt.is_floating_point or _isinstance(v, _b.bool)) and _cx_parts(v) is None):
        return dt
    if _isinstance(v, _int):
        return int64 if dt is _bool_dtype else dt
    if _isinstance(v, _float):
        return _default_dtype
    if _isinstance(v, (_list, _tuple)):
        return _result_type(Tensor(_k.zeros(dt.name, 1), (1,), dt), tensor(v))
    if _cx_parts(v) is not None:
        return _combine_categories(dt, _default_complex())
    return _scalar_dtype(v)


def _lazy(v):
    """Whether v is an uninitialized (Lazy) parameter or buffer; a plain
    Tensor answers from its class alone."""
    return v.__class__ is not Tensor and getattr(v, "_uninitialized", False)


def _lazy_error(v, name):
    # What PyTorch's __torch_function__ raises for one (its message names
    # the builtin, here without the object's address).
    raise ValueError("Attempted to use an uninitialized parameter in <built-in method %s of type object>. This error happens when you are using a `LazyModule` or explicitly manipulating `torch.nn.parameter.%s` objects. When using LazyModules Call `forward` with a dummy batch to initialize the parameters before calling torch functions" % (name, type(v).__name__))


def _check_lazy(name, *values):
    for v in values:
        if _isinstance(v, Tensor) and _lazy(v):
            _lazy_error(v, name)


def _lazy_reraise(e, name, *values):
    """A RuntimeError from reading an operand's shape: PyTorch's ValueError
    when the operand is an uninitialized parameter (its `.shape` raised),
    the error itself otherwise. The hot paths only pay for a `try`."""
    _check_lazy(name, *values)
    raise e


def _needs_grad(*tensors):
    if not _grad_enabled:
        return False
    for t in tensors:
        if _isinstance(t, Tensor) and t.requires_grad:
            return True
    return False


def _unbroadcast(grad, shape):
    """Sum `grad` down to `shape` (the reverse of broadcasting)."""
    gshape = grad.shape
    if gshape is shape or _shape_eq(gshape, shape) or _tuple(gshape) == _tuple(shape):
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
    # Set on the few tensors that need them (class defaults keep every
    # other tensor's construction as cheap as before): `_base`, the tensor a
    # reshape view shares its storage with; `_untracked`, a `.data` alias,
    # whose in-place writes autograd's version checks do not see (PyTorch
    # gives `.data` a version counter of its own); `_hooks`, register_hook.
    _base = None
    _untracked = False
    _hooks = None
    # Defaults every new tensor starts with, read from the class until set:
    # a tensor is made per operation and each attribute store costs.
    grad = None
    _node = None
    _retain = False
    requires_grad = False
    # Backward's traversal marks (see `_backward`).
    _bwe = None
    _pg = None
    _bps = None
    # A strided tensor (the sparse layouts are `_SparseTensor`).
    is_sparse = False
    is_sparse_csr = False

    def __init__(self, storage=None, shape=None, dt=None, requires_grad=False, node=None):
        if dt.__class__ is not dtype:
            # The legacy constructor: torch.Tensor(data) or torch.Tensor(*sizes).
            _legacy_init(self, _default_dtype, (storage, shape, dt) if _isinstance(dt, _int) else ((storage, shape) if shape is not None else (() if storage is None else (storage,))))
            return
        self._s = storage
        # A Size is immutable, so tensors of one shape share it: elementwise
        # kernels hand an operand's own Size back as the result's shape.
        cls = shape.__class__
        self.shape = shape if cls is Size else (_k._size(shape) if cls is _tuple else Size(shape))
        self.dtype = dt
        if requires_grad is not False:
            self.requires_grad = requires_grad
        if node is not None:
            self._node = node

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
        out = Tensor(self._s, self.shape, self.dtype)
        out._untracked = True
        return out

    @property
    def grad_fn(self):
        return None if self._node is None else _node_fn(self._node)

    def register_hook(self, hook):
        """`hook(grad)` runs when this tensor's gradient is computed; a
        returned tensor replaces the gradient."""
        if not self.requires_grad:
            raise RuntimeError("cannot register a hook on a tensor that doesn't require gradient")
        if self._hooks is None:
            self._hooks = []
        self._hooks.append(hook)
        return _HookHandle(self._hooks, hook)

    @property
    def is_cuda(self):
        return False

    @property
    def layout(self):
        return strided

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

    # -- complex parts. `real`/`imag` are copies (writes go through their
    # setters, `view_as_real` or indexing), not the views PyTorch returns.
    @property
    def real(self):
        return real(self)

    @real.setter
    def real(self, value):
        if not self.dtype.is_complex:
            self.copy_(value)
            return
        view_as_real(self)[..., 0] = value

    @property
    def imag(self):
        return imag(self)

    @imag.setter
    def imag(self, value):
        if not self.dtype.is_complex:
            raise RuntimeError("imag is not implemented for tensors with non-complex dtypes.")
        view_as_real(self)[..., 1] = value

    def conj(self):
        return conj(self)

    def conj_physical(self):
        return conj_physical(self)

    def conj_physical_(self):
        return self._inplace_op(conj_physical)

    def resolve_conj(self):
        return self

    def resolve_neg(self):
        return self

    def is_conj(self):
        return False

    def is_neg(self):
        return False

    def angle(self):
        return angle(self)

    def cfloat(self):
        return self.to(complex64)

    def cdouble(self):
        return self.to(complex128)

    def adjoint(self):
        return adjoint(self)

    @property
    def H(self):
        return adjoint(self) if _len(self.shape) == 2 else conj(self)

    @property
    def mH(self):
        return adjoint(self)

    def is_contiguous(self):
        return True

    def contiguous(self):
        return self

    def element_size(self):
        return self.dtype.itemsize

    @property
    def nbytes(self):
        return self.numel() * self.dtype.itemsize

    @property
    def itemsize(self):
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
        if self.dtype.is_complex:
            re, im = _k.to_list(_k.as_real(self._s))
            return _cx_scalar(re, im)
        return _k.item(self._s, 0)

    def tolist(self):
        flat = _cx_scalars(self._s) if self.dtype.is_complex else _k.to_list(self._s)
        if _len(self.shape) == 0:
            return flat[0]

        def build(offset, dims):
            if _len(dims) == 1:
                return flat[offset:offset + dims[0]]
            step = _numel(dims[1:])
            return [build(offset + i * step, dims[1:]) for i in _range(dims[0])]
        return build(0, _list(self.shape))

    def __float__(self):
        if self.dtype.is_complex:
            return _cx_real_scalar(self.item(), "double")
        return _float(self.item())

    def __int__(self):
        if self.dtype.is_complex:
            return _int(_cx_real_scalar(self.item(), "int64_t"))
        return _int(self.item())

    def __complex__(self):
        if _numel(self.shape) != 1:
            raise ValueError("only one element tensors can be converted to Python scalars")
        return _PyComplex(self.item())

    def __bool__(self):
        n = _numel(self.shape)
        if n != 1:
            raise RuntimeError("Boolean value of Tensor with more than one value is ambiguous" if n > 1 else "Boolean value of Tensor with no values is ambiguous")
        if self.dtype.is_complex:
            re, im = _k.to_list(_k.as_real(self._s))
            return re != 0 or im != 0
        return _bool(self.item())

    def __index__(self):
        if self.dtype._inexact or _numel(self.shape) != 1:
            raise TypeError("only integer tensors of a single element can be converted to an index")
        return _int(self.item())

    def numpy(self):
        return _NdArray(self)

    def __repr__(self):
        return _repr(self)

    __str__ = __repr__

    def __format__(self, spec):
        # As PyTorch: a 0-d tensor formats its value; any other tensor only
        # takes an empty format spec.
        if not self.shape:
            return format(self.item(), spec)
        if spec:
            raise TypeError("unsupported format string passed to Tensor.__format__")
        return _repr(self)

    # -- autograd
    def requires_grad_(self, flag=True):
        self.requires_grad = _bool(flag)
        return self

    def detach(self):
        out = Tensor(self._s, self.shape, self.dtype)
        if self._untracked:
            out._untracked = True
        return out

    def detach_(self):
        self._node = None
        self.requires_grad = False
        return self

    def clone(self, memory_format=None):
        out = Tensor(_k.copy(self._s), self.shape, self.dtype)
        if _grad_enabled and self.requires_grad:
            out.requires_grad = True
            out._node = _Node(lambda g: (g,), (self,), "Clone")
        return out

    def retain_grad(self):
        self._retain = True

    @property
    def retains_grad(self):
        return self._retain and self._node is not None

    def backward(self, gradient=None, retain_graph=None, create_graph=False, inputs=None):
        _run_backward([self], [gradient], create_graph, inputs)

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
        if self.dtype._reduced and not _isinstance(other, Tensor):
            # PyTorch's Tensor.__rdiv__: the reciprocal times the scalar.
            return mul(reciprocal(self), other)
        return div(other, self)

    def __floordiv__(self, other):
        return floor_divide(self, other)

    def __rfloordiv__(self, other):
        return floor_divide(other, self)

    def __mod__(self, other):
        return remainder(self, other)

    def __rmod__(self, other):
        return remainder(other, self)

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

    # -- in place
    # An in-place op writes into this tensor's storage, so every alias
    # (`.data`, detach(), reshape views) sees the new values, and bumps the
    # storage's version. Under autograd the result's history becomes this
    # tensor's; a node that saved the old values for backward raises there.
    def _inplace_op(self, fn, *args):
        """Compute `fn(self, *args)` out of place and write it in. When
        autograd records, `fn` reads a copy of the old values carrying the
        old history (PyTorch saves the original self the same way)."""
        if _grad_enabled and (self.requires_grad or _any_requires_grad(args)):
            self._check_inplace()
            src = Tensor(_k.copy(self._s), self.shape, self.dtype, self.requires_grad, self._node)
            result = fn(src, *args)
            self._write(result)
            self._rebase(result)
        else:
            self._write(fn(self, *args))
        return self

    def _check_inplace(self):
        if self.requires_grad and self._node is None:
            raise RuntimeError("a leaf Variable that requires grad is being used in an in-place operation.")
        base = self._base
        if base is not None and base.requires_grad and base._node is None:
            raise RuntimeError("a view of a leaf Variable that requires grad is being used in an in-place operation.")

    def _write(self, result):
        if result.shape is not self.shape and result.shape != self.shape:
            raise RuntimeError("output with shape %s doesn't match the broadcast shape %s" % (_list(self.shape), _list(result.shape)))
        if result.dtype is not self.dtype and _CATEGORY[result.dtype.name] > _CATEGORY[self.dtype.name]:
            raise RuntimeError("result type %s can't be cast to the desired output type %s" % (_CAST_NAME[result.dtype.name], _CAST_NAME[self.dtype.name]))
        _k.copy_into(self._s, result._s)
        if self._untracked:
            _k.untrack(self._s)

    def _wrote(self):
        # A kernel wrote into this storage directly: a `.data` alias's
        # write stays invisible to autograd's version checks.
        if self._untracked:
            _k.untrack(self._s)

    def _rebase(self, result):
        self._node = result._node
        self.requires_grad = result.requires_grad
        base = self._base
        if base is not None and result.requires_grad:
            # The base of a reshape view now holds the view's values: its
            # history continues from the view (PyTorch's CopySlices).
            view, shape = self, self.shape
            base._node = _Node(lambda g: (g.reshape(shape),), (view,), "CopySlices")
            base.requires_grad = True

    def _overwrite(self, values):
        """Write new values that do not depend on the old ones (a random
        fill): under autograd the old values get no gradient."""
        if _grad_enabled and self.requires_grad:
            return self._inplace_op(_replaced, values)
        _k.copy_into(self._s, values._s)
        self._wrote()
        return self

    def __iadd__(self, other):
        return self._inplace_op(add, other)

    def __isub__(self, other):
        return self._inplace_op(sub, other)

    def __imul__(self, other):
        return self._inplace_op(mul, other)

    def __itruediv__(self, other):
        return self._inplace_op(div, other)

    def __ifloordiv__(self, other):
        return self._inplace_op(floor_divide, other)

    def __imod__(self, other):
        return self._inplace_op(remainder, other)

    def __ipow__(self, other):
        return self._inplace_op(pow, other)

    def __iand__(self, other):
        return self._inplace_op(bitwise_and, other)

    def __ior__(self, other):
        return self._inplace_op(bitwise_or, other)

    def __ixor__(self, other):
        return self._inplace_op(bitwise_xor, other)

    def __ilshift__(self, other):
        return self._inplace_op(bitwise_left_shift, other)

    def __irshift__(self, other):
        return self._inplace_op(bitwise_right_shift, other)

    def add_(self, other, alpha=1):
        if other.__class__ is _SparseTensor:
            return _sp._dense_add_(self, other, alpha)
        return self._inplace_op(add, other, alpha)

    def sub_(self, other, alpha=1):
        if other.__class__ is _SparseTensor:
            return _sp._dense_add_(self, other, -alpha)
        return self._inplace_op(sub, other, alpha)

    subtract_ = sub_

    def mul_(self, other):
        return self._inplace_op(mul, other)

    multiply_ = mul_

    def div_(self, other, rounding_mode=None):
        return self._inplace_op(div, other, rounding_mode)

    divide_ = div_
    true_divide_ = div_

    def floor_divide_(self, other):
        return self._inplace_op(floor_divide, other)

    def remainder_(self, other):
        return self._inplace_op(remainder, other)

    def fmod_(self, other):
        return self._inplace_op(fmod, other)

    def pow_(self, exponent):
        return self._inplace_op(pow, exponent)

    def zero_(self):
        if _grad_enabled and self.requires_grad:
            return self._inplace_op(_zeroed)
        _k.fill(self._s, 0)
        self._wrote()
        return self

    def fill_(self, value):
        if _grad_enabled and (self.requires_grad or (_isinstance(value, Tensor) and value.requires_grad)):
            return self._inplace_op(_filled, value)
        value = value.item() if _isinstance(value, Tensor) else value
        if _cx_parts(value) is not None:
            if self.dtype.is_complex:
                _k.copy_into(self._s, _cx_from_values(self.dtype, [value] * _numel(self.shape)))
                self._wrote()
                return self
            value = _cx_real_scalar(value, _CPP_NAME.get(self.dtype.name, self.dtype.name))
        if self.dtype.name in _NARROW_INT:
            _check_narrow(self.dtype, [value], "_t")
        _k.fill(self._s, value)
        self._wrote()
        return self

    def copy_(self, src, non_blocking=False):
        src = _as_tensor(src)
        if _grad_enabled and (self.requires_grad or src.requires_grad):
            return self._inplace_op(_copied, src)
        if src.shape != self.shape:
            src = src.expand(*self.shape)
        _k.copy_into(self._s, src._s)
        self._wrote()
        return self

    def clamp_(self, min=None, max=None):
        return self._inplace_op(clamp, min, max)

    clip_ = clamp_

    def clamp_min_(self, min):
        return self._inplace_op(clamp, min, None)

    def clamp_max_(self, max):
        return self._inplace_op(clamp, None, max)

    def masked_fill_(self, mask, value):
        return self._inplace_op(masked_fill, mask, value)

    def index_fill_(self, dim, index, value):
        return self._inplace_op(index_fill, dim, index, value)

    def index_add_(self, dim, index, source, alpha=1):
        return self._inplace_op(index_add, dim, index, source, alpha)

    def index_copy_(self, dim, index, source):
        return self._inplace_op(index_copy, dim, index, source)

    def scatter_(self, dim, index, src=None, value=None, reduce=None):
        return self._inplace_op(scatter, dim, index, src, value, reduce)

    def scatter_add_(self, dim, index, src):
        return self._inplace_op(scatter_add, dim, index, src)

    def masked_scatter_(self, mask, source):
        return self._inplace_op(masked_scatter, mask, source)

    def addcmul_(self, tensor1, tensor2, value=1):
        return self._inplace_op(addcmul, tensor1, tensor2, value)

    def addcdiv_(self, tensor1, tensor2, value=1):
        return self._inplace_op(addcdiv, tensor1, tensor2, value)

    def lerp_(self, end, weight):
        return self._inplace_op(lerp, end, weight)

    def uniform_(self, a=0.0, b=1.0, generator=None):
        return self._overwrite(rand(*self.shape, generator=generator, dtype=self.dtype if self.dtype.is_floating_point else None) * (b - a) + a)

    def normal_(self, mean=0.0, std=1.0, generator=None):
        return self._overwrite(randn(*self.shape, generator=generator, dtype=self.dtype if self.dtype._inexact else None) * std + mean)

    def bernoulli_(self, p=0.5, generator=None):
        probs = p if _isinstance(p, Tensor) else full(self.shape, _float(p), dtype=float64)
        return self._overwrite(bernoulli(expand(probs.to(float64), *self.shape), generator=generator))

    def exponential_(self, lambd=1.0, generator=None):
        u = rand(*self.shape, generator=generator, dtype=float64)
        return self._overwrite(div(neg(log1p(neg(u))), _float(lambd)))

    def geometric_(self, p, generator=None):
        u = rand(*self.shape, generator=generator, dtype=float64)
        return self._overwrite(ceil(div(log1p(neg(u)), _math.log1p(-_float(p)))))

    def log_normal_(self, mean=1.0, std=2.0, generator=None):
        return self._overwrite(exp(randn(*self.shape, generator=generator, dtype=float64) * std + mean))

    def cauchy_(self, median=0.0, sigma=1.0, generator=None):
        u = rand(*self.shape, generator=generator, dtype=float64)
        return self._overwrite(tan(mul(sub(u, 0.5), _math.pi)) * sigma + median)

    def random_(self, from_=0, to=None, generator=None):
        if to is None:
            if from_ == 0 or from_ is None:
                to = _RANDOM_TO.get(self.dtype.name, 2 ** 53)
            else:
                from_, to = 0, from_
        return self._overwrite(randint(_int(from_), _int(to), self.shape, generator=generator))

    def unsqueeze_(self, dim):
        return self._reshape_(unsqueeze(self, dim).shape)

    def squeeze_(self, dim=None):
        return self._reshape_(squeeze(self, dim).shape)

    def t_(self):
        if _len(self.shape) < 2:
            return self
        return self._inplace_perm(t)

    def transpose_(self, dim0, dim1):
        return self._inplace_perm(lambda a: transpose(a, dim0, dim1))

    swapaxes_ = transpose_
    swapdims_ = transpose_

    def resize_(self, *sizes):
        shape = _shape_args(sizes)
        if _numel(shape) != _numel(self.shape):
            # A resize to another element count keeps the leading values.
            flat = _k.to_list(self._s)[:_numel(shape)]
            flat += [0] * (_numel(shape) - _len(flat))
            self._s = _k.from_flat(self.dtype.name, flat)
        self.shape = Size(shape)
        return self

    def _reshape_(self, shape):
        # A shape change in place: the storage keeps its values and layout.
        if _grad_enabled and self.requires_grad:
            self._check_inplace()
            old = Tensor(self._s, self.shape, self.dtype, True, self._node)
            view = reshape(old, *shape)
            self._node = view._node
        self.shape = Size(shape)
        return self

    def _inplace_perm(self, fn):
        # transpose_/t_: the permuted values are written into this tensor's
        # storage (slices and permutations copy here; see the docs).
        if _grad_enabled and self.requires_grad:
            self._check_inplace()
        src = Tensor(_k.copy(self._s), self.shape, self.dtype, self.requires_grad, self._node)
        out = fn(src)
        self.shape = out.shape
        _k.copy_into(self._s, out._s)
        self._wrote()
        if _grad_enabled and out.requires_grad:
            self._node = out._node
        return self

    # -- comparisons
    def __eq__(self, other):
        if other is None:
            return False
        return _binary_nograd("eq", self, other)

    def __ne__(self, other):
        if other is None:
            return True
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
        return bitwise_and(self, other)

    def __rand__(self, other):
        return bitwise_and(other, self)

    def __or__(self, other):
        return bitwise_or(self, other)

    def __ror__(self, other):
        return bitwise_or(other, self)

    def __xor__(self, other):
        return bitwise_xor(self, other)

    def __rxor__(self, other):
        return bitwise_xor(other, self)

    def __lshift__(self, other):
        return bitwise_left_shift(self, other)

    def __rshift__(self, other):
        return bitwise_right_shift(self, other)

    def __invert__(self):
        return bitwise_not(self)

    eq = __eq__
    ne = __ne__
    lt = __lt__
    le = __le__
    gt = __gt__
    ge = __ge__
    greater = __gt__
    greater_equal = __ge__
    less = __lt__
    less_equal = __le__
    not_equal = __ne__

    def logical_and(self, other):
        return logical_and(self, other)

    def logical_or(self, other):
        return logical_or(self, other)

    def logical_xor(self, other):
        return logical_xor(self, other)

    def bitwise_and(self, other):
        return bitwise_and(self, other)

    def bitwise_or(self, other):
        return bitwise_or(self, other)

    def bitwise_xor(self, other):
        return bitwise_xor(self, other)

    def bitwise_not(self):
        return bitwise_not(self)

    # -- elementwise (the module functions below carry the autograd)
    def exp(self):
        return exp(self)

    def exp2(self):
        return exp2(self)

    def expm1(self):
        return expm1(self)

    def log(self):
        return log(self)

    def log2(self):
        return log2(self)

    def log10(self):
        return log10(self)

    def log1p(self):
        return log1p(self)

    def tanh(self):
        return tanh(self)

    def sigmoid(self):
        return sigmoid(self)

    def relu(self):
        return relu(self)

    def sqrt(self):
        return sqrt(self)

    def rsqrt(self):
        return rsqrt(self)

    def sin(self):
        return sin(self)

    def cos(self):
        return cos(self)

    def tan(self):
        return tan(self)

    def asin(self):
        return asin(self)

    def acos(self):
        return acos(self)

    def atan(self):
        return atan(self)

    def atan2(self, other):
        return atan2(self, other)

    def sinh(self):
        return sinh(self)

    def cosh(self):
        return cosh(self)

    def asinh(self):
        return asinh(self)

    def acosh(self):
        return acosh(self)

    def atanh(self):
        return atanh(self)

    arcsin = asin
    arccos = acos
    arctan = atan
    arctan2 = atan2
    arcsinh = asinh
    arccosh = acosh
    arctanh = atanh

    def erf(self):
        return erf(self)

    def erfc(self):
        return erfc(self)

    def erfinv(self):
        return erfinv(self)

    def lgamma(self):
        return lgamma(self)

    def digamma(self):
        return digamma(self)

    def polygamma(self, n):
        return polygamma(n, self)

    def polygamma_(self, n):
        return self._inplace_op(lambda src: polygamma(n, src))

    def mvlgamma(self, p):
        return mvlgamma(self, p)

    def mvlgamma_(self, p):
        return self._inplace_op(mvlgamma, p)

    def i0(self):
        return i0(self)

    def sinc(self):
        return sinc(self)

    def logit(self, eps=None):
        return logit(self, eps)

    def logit_(self, eps=None):
        return self._inplace_op(logit, eps)

    def xlogy(self, other):
        return xlogy(self, other)

    def igamma(self, other):
        return igamma(self, other)

    def igammac(self, other):
        return igammac(self, other)

    def repeat_interleave(self, repeats, dim=None):
        return repeat_interleave(self, repeats, dim)

    def square(self):
        return square(self)

    def abs(self):
        return abs(self)

    absolute = abs

    def neg(self):
        return neg(self)

    negative = neg

    def sign(self):
        return sign(self)

    def sgn(self):
        return sgn(self)

    def signbit(self):
        return signbit(self)

    def floor(self):
        return floor(self)

    def ceil(self):
        return ceil(self)

    def round(self, decimals=0):
        return round(self, decimals=decimals)

    def trunc(self):
        return trunc(self)

    fix = trunc

    def frac(self):
        return frac(self)

    def isfinite(self):
        return isfinite(self)

    def isnan(self):
        return isnan(self)

    def isinf(self):
        return isinf(self)

    def isposinf(self):
        return isposinf(self)

    def isneginf(self):
        return isneginf(self)

    def isreal(self):
        return isreal(self)

    def nan_to_num(self, nan=0.0, posinf=None, neginf=None):
        return nan_to_num(self, nan, posinf, neginf)

    def nan_to_num_(self, nan=0.0, posinf=None, neginf=None):
        return self._inplace_op(nan_to_num, nan, posinf, neginf)

    def logical_not(self):
        return logical_not(self)

    def clamp(self, min=None, max=None):
        return clamp(self, min, max)

    clip = clamp

    def clamp_min(self, min):
        return clamp(self, min, None)

    def clamp_max(self, max):
        return clamp(self, None, max)

    def pow(self, exponent):
        return pow(self, exponent)

    def float_power(self, exponent):
        return float_power(self, exponent)

    def reciprocal(self):
        return reciprocal(self)

    def softmax(self, dim=-1, dtype=None):
        return softmax(self, dim, dtype)

    def log_softmax(self, dim=-1, dtype=None):
        return log_softmax(self, dim, dtype)

    def add(self, other, alpha=1):
        return add(self, other, alpha=alpha)

    def sub(self, other, alpha=1):
        return sub(self, other, alpha=alpha)

    subtract = sub

    def mul(self, other):
        return mul(self, other)

    multiply = mul

    def div(self, other, rounding_mode=None):
        return div(self, other, rounding_mode=rounding_mode)

    divide = div

    def true_divide(self, other):
        return div(self, other)

    def floor_divide(self, other):
        return floor_divide(self, other)

    def remainder(self, other):
        return remainder(self, other)

    def fmod(self, other):
        return fmod(self, other)

    def fmax(self, other):
        return fmax(self, other)

    def fmin(self, other):
        return fmin(self, other)

    def maximum(self, other):
        return maximum(self, other)

    def minimum(self, other):
        return minimum(self, other)

    def lerp(self, end, weight):
        return lerp(self, end, weight)

    def addcmul(self, tensor1, tensor2, value=1):
        return addcmul(self, tensor1, tensor2, value=value)

    def addcdiv(self, tensor1, tensor2, value=1):
        return addcdiv(self, tensor1, tensor2, value=value)

    def matmul(self, other):
        return matmul(self, other)

    def mm(self, mat2):
        return mm(self, mat2)

    def bmm(self, mat2):
        return bmm(self, mat2)

    def dot(self, other):
        return dot(self, other)

    def mv(self, vec):
        return mv(self, vec)

    def outer(self, other):
        return outer(self, other)

    ger = outer

    def addmm(self, mat1, mat2, beta=1, alpha=1):
        return addmm(self, mat1, mat2, beta=beta, alpha=alpha)

    def baddbmm(self, batch1, batch2, beta=1, alpha=1):
        return baddbmm(self, batch1, batch2, beta=beta, alpha=alpha)

    def kron(self, other):
        return kron(self, other)

    def cross(self, other, dim=None):
        return cross(self, other, dim)

    def trace(self):
        return trace(self)

    def where(self, condition, other):
        return where(condition, self, other)

    # -- reductions
    def sum(self, dim=None, keepdim=False, dtype=None):
        return sum(self, dim, keepdim, dtype=dtype)

    def nansum(self, dim=None, keepdim=False, dtype=None):
        return nansum(self, dim, keepdim, dtype=dtype)

    def mean(self, dim=None, keepdim=False, dtype=None):
        return mean(self, dim, keepdim, dtype=dtype)

    def nanmean(self, dim=None, keepdim=False, dtype=None):
        return nanmean(self, dim, keepdim, dtype=dtype)

    def prod(self, dim=None, keepdim=False, dtype=None):
        return prod(self, dim, keepdim, dtype=dtype)

    def count_nonzero(self, dim=None):
        return count_nonzero(self, dim)

    def max(self, dim=None, keepdim=False):
        return max(self, dim, keepdim)

    def min(self, dim=None, keepdim=False):
        return min(self, dim, keepdim)

    def amax(self, dim=(), keepdim=False):
        return amax(self, dim, keepdim)

    def amin(self, dim=(), keepdim=False):
        return amin(self, dim, keepdim)

    def aminmax(self, dim=None, keepdim=False):
        return aminmax(self, dim=dim, keepdim=keepdim)

    def argmax(self, dim=None, keepdim=False):
        return argmax(self, dim, keepdim)

    def argmin(self, dim=None, keepdim=False):
        return argmin(self, dim, keepdim)

    def all(self, dim=None, keepdim=False):
        return all(self, dim, keepdim)

    def any(self, dim=None, keepdim=False):
        return any(self, dim, keepdim)

    def norm(self, p="fro", dim=None, keepdim=False, dtype=None):
        return norm(self, p, dim, keepdim, dtype=dtype)

    def var(self, dim=None, unbiased=None, keepdim=False, *, correction=None):
        return var(self, dim, unbiased, keepdim, correction=correction)

    def std(self, dim=None, unbiased=None, keepdim=False, *, correction=None):
        return std(self, dim, unbiased, keepdim, correction=correction)

    def logsumexp(self, dim, keepdim=False):
        return logsumexp(self, dim, keepdim)

    def median(self, dim=None, keepdim=False):
        return median(self, dim, keepdim)

    def nanmedian(self, dim=None, keepdim=False):
        return nanmedian(self, dim, keepdim)

    def mode(self, dim=-1, keepdim=False):
        return mode(self, dim, keepdim)

    def kthvalue(self, k, dim=-1, keepdim=False):
        return kthvalue(self, k, dim, keepdim)

    def quantile(self, q, dim=None, keepdim=False, interpolation="linear"):
        return quantile(self, q, dim, keepdim, interpolation=interpolation)

    def argsort(self, dim=-1, descending=False, stable=False):
        return argsort(self, dim, descending, stable=stable)

    def sort(self, dim=-1, descending=False, stable=False):
        return sort(self, dim, descending, stable=stable)

    def msort(self):
        return sort(self, 0)[0]

    def topk(self, k, dim=-1, largest=True, sorted=True):
        return topk(self, k, dim, largest, sorted)

    def cumsum(self, dim, dtype=None):
        return cumsum(self, dim, dtype=dtype)

    def cumprod(self, dim, dtype=None):
        return cumprod(self, dim, dtype=dtype)

    def cummax(self, dim):
        return cummax(self, dim)

    def cummin(self, dim):
        return cummin(self, dim)

    def logcumsumexp(self, dim):
        return logcumsumexp(self, dim)

    def diff(self, n=1, dim=-1, prepend=None, append=None):
        return diff(self, n, dim, prepend, append)

    def unique(self, sorted=True, return_inverse=False, return_counts=False, dim=None):
        return unique(self, sorted, return_inverse, return_counts, dim)

    def unique_consecutive(self, return_inverse=False, return_counts=False, dim=None):
        return unique_consecutive(self, return_inverse, return_counts, dim)

    def bincount(self, weights=None, minlength=0):
        return bincount(self, weights, minlength)

    # -- shape
    def reshape(self, *shape):
        return reshape(self, *shape)

    def view(self, *shape):
        if _len(shape) == 1 and _isinstance(shape[0], dtype):
            return _view_dtype(self, shape[0])
        return reshape(self, *shape)

    def reshape_as(self, other):
        return reshape(self, *other.shape)

    view_as = reshape_as

    def flatten(self, start_dim=0, end_dim=-1):
        return flatten(self, start_dim, end_dim)

    def ravel(self):
        return reshape(self, -1)

    def unflatten(self, dim, sizes):
        return unflatten(self, dim, sizes)

    def unsqueeze(self, dim):
        return unsqueeze(self, dim)

    def squeeze(self, dim=None):
        return squeeze(self, dim)

    def transpose(self, dim0, dim1):
        return transpose(self, dim0, dim1)

    swapaxes = transpose
    swapdims = transpose

    def t(self):
        return t(self)

    def permute(self, *dims):
        return permute(self, *dims)

    def movedim(self, source, destination):
        return movedim(self, source, destination)

    moveaxis = movedim

    def expand(self, *sizes):
        return expand(self, *sizes)

    def expand_as(self, other):
        return expand(self, *other.shape)

    def broadcast_to(self, size):
        return broadcast_to(self, size)

    def repeat(self, *sizes):
        return repeat(self, *sizes)

    def tile(self, *dims):
        return tile(self, *dims)

    def narrow(self, dim, start, length):
        return narrow(self, dim, start, length)

    def unbind(self, dim=0):
        return unbind(self, dim)

    def select(self, dim, index):
        rank = _len(self.shape)
        d = _norm_dim(dim, rank)
        if type(index) is _int and type(self).__getitem__ is _tensor_getitem:
            # The spec `self[(:, ..., index)]` builds, the kernel checking
            # the index: every other dim whole.
            spec = [(None, None, None)] * rank
            spec[d] = index
            return _slice(self, spec)
        spec = [slice(None)] * d + [index]
        return self[_tuple(spec)]

    def split(self, split_size, dim=0):
        return split(self, split_size, dim)

    def tensor_split(self, indices_or_sections, dim=0):
        return tensor_split(self, indices_or_sections, dim)

    def chunk(self, chunks, dim=0):
        return chunk(self, chunks, dim)

    def roll(self, shifts, dims=None):
        return roll(self, shifts, dims)

    def flip(self, *dims):
        return flip(self, _shape_args(dims) if dims else ())

    def fliplr(self):
        return fliplr(self)

    def flipud(self):
        return flipud(self)

    def rot90(self, k=1, dims=(0, 1)):
        return rot90(self, k, dims)

    def diag(self, diagonal=0):
        return diag(self, diagonal)

    def diagonal(self, offset=0, dim1=0, dim2=1):
        return diagonal(self, offset, dim1, dim2)

    def diag_embed(self, offset=0, dim1=-2, dim2=-1):
        return diag_embed(self, offset, dim1, dim2)

    def tril(self, diagonal=0):
        return tril(self, diagonal)

    def triu(self, diagonal=0):
        return triu(self, diagonal)

    def tril_(self, diagonal=0):
        return self._inplace_op(tril, diagonal)

    def triu_(self, diagonal=0):
        return self._inplace_op(triu, diagonal)

    def type(self, dtype=None, non_blocking=False):
        if dtype is None:
            return "torch." + _CAST_NAME[self.dtype.name] + "Tensor"
        if _isinstance(dtype, _b.str):
            name = dtype.rsplit(".", 1)[-1]
            if name not in _LEGACY_TYPES:
                raise RuntimeError("invalid type: '%s'" % dtype)
            dtype = _LEGACY_TYPES[name]
        elif _isinstance(dtype, type) and _issubclass(dtype, Tensor) and dtype in _LEGACY_CLASSES:
            dtype = _LEGACY_CLASSES[dtype]
        return self.to(dtype)

    def type_as(self, other):
        return self.to(other.dtype)

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
        copy = kwargs.get("copy", False)
        if target is None or target == self.dtype:
            return self.clone() if copy else self
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
        return self.to(float16)

    def bfloat16(self):
        return self.to(bfloat16)

    def char(self):
        return self.to(int8)

    def short(self):
        return self.to(int16)

    def cpu(self):
        return self

    def cuda(self, *args, **kwargs):
        raise RuntimeError("CUDA is not available on Zipp")

    def pin_memory(self):
        return self

    def share_memory_(self):
        return self

    def data_ptr(self):
        return id(self._s)

    def untyped_storage(self):
        return _STORAGE_TYPES[self.dtype.name](self._s)

    def new_zeros(self, *size, dtype=None, device=None, requires_grad=False):
        return zeros(*size, dtype=self.dtype if dtype is None else dtype, device=device, requires_grad=requires_grad)

    def new_ones(self, *size, dtype=None, device=None, requires_grad=False):
        return ones(*size, dtype=self.dtype if dtype is None else dtype, device=device, requires_grad=requires_grad)

    def new_full(self, size, fill_value, dtype=None, device=None, requires_grad=False):
        return full(size, fill_value, dtype=self.dtype if dtype is None else dtype, device=device, requires_grad=requires_grad)

    def new_empty(self, *size, dtype=None, device=None, requires_grad=False):
        return self.new_zeros(*size, dtype=dtype, device=device, requires_grad=requires_grad)

    def new_tensor(self, data, dtype=None, device=None, requires_grad=False):
        if _isinstance(data, Tensor):
            out = Tensor(_k.copy(data._s), data.shape, data.dtype)
            return out.to(dtype if dtype is not None else data.dtype).requires_grad_(requires_grad)
        return tensor(data, dtype=self.dtype if dtype is None else dtype, device=device, requires_grad=requires_grad)

    # -- indexing
    def __getitem__(self, key):
        return _getitem(self, key)

    def __setitem__(self, key, value):
        _setitem(self, key, value)

    def index_select(self, dim, index):
        return index_select(self, dim, index)

    def gather(self, dim, index):
        return gather(self, dim, index)

    def take(self, index):
        return take(self, index)

    def take_along_dim(self, indices, dim=None):
        return take_along_dim(self, indices, dim)

    def scatter(self, dim, index, src=None, value=None, reduce=None):
        return scatter(self, dim, index, src, value, reduce)

    def scatter_add(self, dim, index, src):
        return scatter_add(self, dim, index, src)

    def index_add(self, dim, index, source, alpha=1):
        return index_add(self, dim, index, source, alpha)

    def index_fill(self, dim, index, value):
        return index_fill(self, dim, index, value)

    def index_copy(self, dim, index, source):
        return index_copy(self, dim, index, source)

    def masked_fill(self, mask, value):
        return masked_fill(self, mask, value)

    def masked_select(self, mask):
        return masked_select(self, mask)

    def masked_scatter(self, mask, source):
        return masked_scatter(self, mask, source)

    def nonzero(self, as_tuple=False):
        return nonzero(self, as_tuple=as_tuple)

    def argwhere(self):
        return argwhere(self)

    def bernoulli(self, generator=None):
        return bernoulli(self, generator=generator)

    def multinomial(self, num_samples, replacement=False, generator=None):
        return multinomial(self, num_samples, replacement, generator=generator)

    def apply_(self, fn):
        values = [fn(v) for v in _k.to_list(self._s)]
        _k.copy_into(self._s, _k.from_flat(self.dtype.name, values))
        self._wrote()
        return self

    def map_(self, other, fn):
        other = expand(other, *self.shape)
        values = [fn(a, b) for a, b in zip(_k.to_list(self._s), _k.to_list(other._s))]
        _k.copy_into(self._s, _k.from_flat(self.dtype.name, values))
        self._wrote()
        return self

    def is_set_to(self, other):
        return self._s is other._s and self.shape == other.shape

    def is_nonzero(self):
        return _bool(self)

    def is_complex(self):
        return self.dtype.is_complex

    def is_signed(self):
        return self.dtype not in (uint8, _bool_dtype)

    def equal(self, other):
        return equal(self, other)

    def allclose(self, other, rtol=1e-05, atol=1e-08, equal_nan=False):
        return allclose(self, other, rtol, atol, equal_nan)

    def isclose(self, other, rtol=1e-05, atol=1e-08, equal_nan=False):
        return isclose(self, other, rtol, atol, equal_nan)


# `narrow` takes the fast route only for tensors indexed by this method.
_tensor_getitem = Tensor.__getitem__
_tensor_init = Tensor.__init__


# ---- sparse layouts ---------------------------------------------------------------------
# A sparse COO or CSR tensor is an instance of this subclass, also named
# `Tensor` (so `type(t)` prints as torch.Tensor and isinstance holds; `type(t)
# is torch.Tensor` is False), holding index and value tensors instead of a
# storage. torch.sparse (torch_sparse.py) builds them and installs their
# methods; the dense ops that accept one check for it (`_sp`). Reading `_s`
# raises PyTorch's error for an operator without a sparse kernel, so a dense
# kernel is never handed one by mistake.
_DenseTensor = Tensor


class Tensor(_DenseTensor):
    @property
    def _s(self):
        raise NotImplementedError("Could not run this operator with arguments from the '%s' backend: it needs a strided (dense) tensor. This could be because the operator doesn't exist for this backend. Use Tensor.to_dense() first." % ("SparseCPU" if self.is_sparse else "SparseCsrCPU"))


_SparseTensor = Tensor
Tensor = _DenseTensor
# torch.sparse, set once it is imported (the end of this module).
_sp = None


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


class _PrintOptions:
    precision = 4
    threshold = 1000
    edgeitems = 3
    linewidth = 80
    sci_mode = None


_PRINT = _PrintOptions()


def set_printoptions(precision=None, threshold=None, edgeitems=None, linewidth=None, profile=None, sci_mode=None):
    if profile is not None:
        presets = {"default": (4, 1000, 3, 80), "short": (2, 1000, 2, 80), "full": (4, _math.inf, 3, 80)}
        if profile not in presets:
            raise ValueError("unknown print profile %r" % (profile,))
        _PRINT.precision, _PRINT.threshold, _PRINT.edgeitems, _PRINT.linewidth = presets[profile]
    if precision is not None:
        _PRINT.precision = _int(precision)
    if threshold is not None:
        _PRINT.threshold = threshold
    if edgeitems is not None:
        _PRINT.edgeitems = _int(edgeitems)
    if linewidth is not None:
        _PRINT.linewidth = _int(linewidth)
    _PRINT.sci_mode = sci_mode


def get_printoptions():
    return {"precision": _PRINT.precision, "threshold": _PRINT.threshold, "edgeitems": _PRINT.edgeitems, "linewidth": _PRINT.linewidth, "sci_mode": _PRINT.sci_mode}


class _Formatter:
    """PyTorch's element formatter (torch/_tensor_str.py): one layout for the
    whole tensor, chosen from its nonzero finite values, every element padded
    to the widest."""

    def __init__(self, values, dt):
        self.floating = dt.is_floating_point
        self.int_mode = True
        self.sci_mode = False
        self.max_width = 1
        p = _PRINT.precision
        if not self.floating:
            for v in values:
                self.max_width = _b.max(self.max_width, _len(self._plain(v, dt)))
        else:
            finite = [v for v in values if v == v and v not in (_math.inf, -_math.inf) and v != 0]
            if finite:
                absolute = [_b.abs(v) for v in finite]
                lo, hi = _b.min(absolute), _b.max(absolute)
                for v in finite:
                    if v != _math.ceil(v):
                        self.int_mode = False
                        break
                if self.int_mode:
                    self.sci_mode = hi / lo > 1000.0 or hi > 1.0e8
                else:
                    self.sci_mode = hi / lo > 1000.0 or hi > 1.0e8 or lo < 1.0e-4
                if _PRINT.sci_mode is not None:
                    self.sci_mode = _PRINT.sci_mode
                for v in finite:
                    if self.sci_mode:
                        width = _len(("{:.%de}" % p).format(v))
                    elif self.int_mode:
                        width = _len("%.0f" % v) + 1
                    else:
                        width = _len(("{:.%df}" % p).format(v))
                    self.max_width = _b.max(self.max_width, width)
        if _PRINT.sci_mode is not None:
            self.sci_mode = _PRINT.sci_mode
        self.dt = dt

    @staticmethod
    def _plain(v, dt):
        if dt is _bool_dtype:
            return "True" if v else "False"
        return "%d" % v

    def format(self, v):
        p = _PRINT.precision
        if self.floating:
            if self.sci_mode:
                out = ("{:%d.%de}" % (self.max_width, p)).format(v)
            elif self.int_mode:
                out = "%.0f" % v
                if v == v and v not in (_math.inf, -_math.inf):
                    out += "."
            else:
                out = ("{:.%df}" % p).format(v)
        else:
            out = self._plain(v, self.dt)
        return " " * (self.max_width - _len(out)) + out


class _ComplexFormatter:
    """PyTorch's complex layout: the real and imaginary parts each through
    their own formatter, joined as `re+imj`."""

    def __init__(self, re, im):
        self.re = re
        self.im = im
        self.max_width = re.max_width + im.max_width + 1

    def format(self, v):
        real_str = self.re.format(v[0])
        imag_str = (self.im.format(v[1]) + "j").lstrip()
        if imag_str[0] == "+" or imag_str[0] == "-":
            return real_str + imag_str
        return real_str + "+" + imag_str


def _nested(flat, shape, offset):
    """The nested lists of `shape` elements starting at `flat[offset]`."""
    if _len(shape) == 1:
        return flat[offset:offset + shape[0]]
    step = _numel(shape[1:])
    return [_nested(flat, shape[1:], offset + i * step) for i in _range(shape[0])]


def _summarized(rows, rank, edge):
    """PyTorch's get_summarized_data on nested lists: only the edge rows."""
    if rank == 0:
        return rows
    if rank == 1:
        return rows[:edge] + rows[-edge:] if _len(rows) > 2 * edge else rows
    if _len(rows) > 2 * edge:
        rows = rows[:edge] + rows[-edge:]
    return [_summarized(r, rank - 1, edge) for r in rows]


def _leaves(rows, rank, out):
    if rank == 0:
        out.append(rows)
    elif rank == 1:
        out.extend(rows)
    else:
        for r in rows:
            _leaves(r, rank - 1, out)
    return out


def _tensor_body(rows, rank, indent, summarize, fmt):
    edge = _PRINT.edgeitems
    if rank == 0:
        return fmt.format(rows)
    if rank == 1:
        width = fmt.max_width + 2
        per_line = _b.max(1, _int(_math.floor((_PRINT.linewidth - indent) / width)))
        if summarize and not edge:
            data = ["..."]
        elif summarize and _len(rows) > 2 * edge:
            data = [fmt.format(v) for v in rows[:edge]] + [" ..."] + [fmt.format(v) for v in rows[-edge:]]
        else:
            data = [fmt.format(v) for v in rows]
        lines = [", ".join(data[i:i + per_line]) for i in _range(0, _len(data), per_line)]
        return "[" + ("," + "\n" + " " * (indent + 1)).join(lines) + "]"
    if summarize and _len(rows) > 2 * edge:
        parts = [_tensor_body(r, rank - 1, indent + 1, summarize, fmt) for r in rows[:edge]] + ["..."] + [_tensor_body(r, rank - 1, indent + 1, summarize, fmt) for r in rows[-edge:]]
    else:
        parts = [_tensor_body(r, rank - 1, indent + 1, summarize, fmt) for r in rows]
    return "[" + ("," + "\n" * (rank - 1) + " " * (indent + 1)).join(parts) + "]"


def _tensor_str(t, indent):
    """PyTorch's _tensor_str: the bracketed elements of a non-empty dense
    tensor, continuation lines indented by `indent`."""
    rank = _len(t.shape)
    if t.dtype.is_complex:
        parts = _k.to_list(_k.as_real(t._s))
        flat = [(parts[i], parts[i + 1]) for i in _range(0, _len(parts), 2)]
    else:
        flat = _k.to_list(t._s)
    rows = flat[0] if rank == 0 else _nested(flat, _list(t.shape), 0)
    summarize = _numel(t.shape) > _PRINT.threshold
    shown = _summarized(rows, rank, _PRINT.edgeitems) if summarize else rows
    if t.dtype.is_complex:
        leaves = _leaves(shown, rank, [])
        rdt = t.dtype.to_real()
        fmt = _ComplexFormatter(_Formatter([v[0] for v in leaves], rdt), _Formatter([v[1] for v in leaves], rdt))
    else:
        fmt = _Formatter(_leaves(shown, rank, []), t.dtype)
    return _tensor_body(rows, rank, indent, summarize, fmt)


def _add_suffixes(text, suffixes, indent, force_newline=False):
    """PyTorch's _add_suffixes: `, suffix` after the body, each on a new
    line once the line would pass `linewidth` (the first always, for a
    sparse tensor)."""
    out = [text]
    last = _len(text) - text.rfind("\n") + 1
    for suffix in suffixes:
        if force_newline or last + _len(suffix) + 2 > _PRINT.linewidth:
            out.append(",\n" + " " * indent + suffix)
            last = indent + _len(suffix)
            force_newline = False
        else:
            out.append(", " + suffix)
            last += _len(suffix) + 2
    out.append(")")
    return "".join(out)


def _repr(t):
    """PyTorch's tensor repr: layout and padding from torch/_tensor_str.py,
    summarised with '...' beyond `threshold` elements (set_printoptions)."""
    prefix = "tensor("
    indent = _len(prefix)
    suffixes = []
    rank = _len(t.shape)
    n = _numel(t.shape)
    default = t.dtype in (_default_dtype, int64, _bool_dtype) or t.dtype is _default_complex()
    if n == 0:
        if rank != 1:
            suffixes.append("size=" + str(_tuple(t.shape)))
        if t.dtype != _default_dtype:
            suffixes.append("dtype=" + repr(t.dtype))
        body = "[]"
    else:
        if not default:
            suffixes.append("dtype=" + repr(t.dtype))
        body = _tensor_str(t, indent)
    if t._node is not None:
        suffixes.append("grad_fn=<%s>" % _grad_fn_name(t._node.name))
    elif t.requires_grad:
        suffixes.append("requires_grad=True")
    return _add_suffixes(prefix + body, suffixes, indent)


# ---- creation ---------------------------------------------------------------------------
def _flatten_data(data):
    """Nested lists/tuples (or scalars) to a flat list and a shape."""
    if _isinstance(data, Tensor):
        return (_cx_scalars(data._s) if data.dtype.is_complex else _k.to_list(data._s)), _tuple(data.shape), data.dtype
    if _isinstance(data, (_list, _tuple)):
        if _len(data) == 0:
            return [], (0,), None
        first = data[0]
        if _isinstance(first, (_list, _tuple)) or (_isinstance(first, Tensor) and first.shape):
            flat, shape, dt = [], None, None
            for row in data:
                f, s, d = _flatten_data(row)
                if shape is None:
                    shape = s
                elif s != shape:
                    raise ValueError("expected sequence of length %d at dim 1 (got %d)" % (shape[0], s[0]))
                flat.extend(f)
                dt = _promote_types(dt, d)
            return flat, (_len(data),) + _tuple(shape), dt
        flat, dt = [], None
        for v in data:
            if _isinstance(v, Tensor):
                if v.shape:
                    raise ValueError("only one element tensors can be converted to Python scalars")
                dt = _promote_types(dt, v.dtype)
                v = v.item()
            elif _isinstance(v, (_list, _tuple)):
                raise ValueError("expected a sequence of numbers, got a nested sequence")
            flat.append(v)
        if dt is not None:
            for v in flat:
                if not _isinstance(v, Tensor):
                    dt = _promote_types(dt, _infer_dtype([v], None))
        return flat, (_len(data),), dt
    if _isinstance(data, _NdArray):
        return _flatten_data(data._t)
    return [data], (), None


def _infer_dtype(flat, hint):
    if hint is not None:
        return hint
    if not flat:
        # `torch.tensor([])` takes the default floating dtype.
        return _default_dtype
    has_float = False
    has_complex = False
    all_bool = _len(flat) > 0
    for v in flat:
        if _isinstance(v, _b.bool):
            continue
        all_bool = False
        if _isinstance(v, _float):
            has_float = True
        elif _cx_parts(v) is not None:
            has_complex = True
        elif not _isinstance(v, _int):
            raise TypeError("Could not infer dtype of %s" % type(v).__name__)
    if all_bool:
        return _bool_dtype
    if has_complex:
        return _default_complex()
    return _default_dtype if has_float else int64


def tensor(data, dtype=None, device=None, requires_grad=False, pin_memory=False):
    _check_cpu_device(device)
    if _isinstance(data, Tensor) and data.dtype.is_complex:
        # A complex tensor's copy (its elements have no Python form yet).
        dt = data.dtype if dtype is None else _dtype_of(dtype)
        return Tensor(_k.astype(data._s, dt.name), data.shape, dt, requires_grad)
    flat, shape, hint = _flatten_data(data)
    if _isinstance(data, Tensor) and dtype is None:
        dt = data.dtype
    else:
        dt = _dtype_of(dtype) or _infer_dtype(flat, hint)
    if dt.name in _NARROW_INT and not _isinstance(data, Tensor):
        _check_narrow(dt, flat, "")
    if dt.is_complex:
        return Tensor(_cx_from_values(dt, flat), shape, dt, requires_grad)
    flat = [_float(v) if dt.is_floating_point else (_int(v) if dt is not _bool_dtype else (1 if v else 0)) for v in flat]
    return Tensor(_k.from_flat(dt.name, flat), shape, dt, requires_grad)


# int8/int16 refuse a Python value outside their range when a tensor is
# made or filled from it (arithmetic wraps instead), as PyTorch's checked
# scalar conversion does.
_NARROW_INT = {"int8": (-128, 127), "int16": (-32768, 32767)}


def _check_narrow(dt, values, suffix):
    lo, hi = _NARROW_INT[dt.name]
    for v in values:
        if v < lo or v > hi:
            raise RuntimeError("value cannot be converted to type %s%s without overflow" % (dt.name, suffix))


def as_tensor(data, dtype=None, device=None):
    _check_cpu_device(device)
    if _isinstance(data, Tensor) and (dtype is None or _dtype_of(dtype) == data.dtype):
        return data
    return tensor(data, dtype=dtype)


from_numpy = as_tensor
asarray = as_tensor


def _as_tensor(v, like=None, dtype=None):
    """A tensor for an operand: tensors pass through; a Python scalar becomes
    a 0-d tensor (of `dtype`, or of `like`'s floating dtype). Every scalar
    an op wraps comes through here: torch.compile's prepare() watches it."""
    if _isinstance(v, Tensor):
        return v
    if dtype is not None and _isinstance(v, (_int, _float, _b.bool)):
        return Tensor(_k.full(dtype.name, 1, v), _SCALAR_SHAPE, dtype)
    if like is not None and like.dtype._inexact and _isinstance(v, (_int, _float, _b.bool)):
        return Tensor(_k.full(like.dtype.name, 1, _float(v)), _SCALAR_SHAPE, like.dtype)
    return tensor(v)


def _shape_args(shape):
    if _len(shape) == 1:
        s0 = shape[0]
        if type(s0) is not _int and _isinstance(s0, (_list, _tuple, Size)):
            shape = s0
    # A tuple of plain ints is returned as it is; anything else (a bool, a
    # 0-d tensor, an int subclass) goes through int().
    for d in shape:
        if type(d) is not _int:
            return _tuple([_int(v) for v in shape])
    return shape if type(shape) is _tuple else _tuple(shape)


# Tensors the kernels return are built without the class call: with
# `Tensor.__init__` as defined here, `_new3(storage, shape, dt)` is exactly
# `Tensor(storage, shape, dt)` for a Size `shape` (a kernel result's; any
# other shape takes the class call).
# A replaced `__init__` (a wrapper installed on the class) is still called.
_onew = object.__new__


def _view_dtype(t, dt):
    """`t.view(dt)`: t's bytes read as `dt`. float16/bfloat16/int16 (and
    uint8/int8) views share t's memory, so a write through either shows in
    the other; other pairs copy the bytes (int32/int64 are not stored at
    their width). A different element size rescales the last dimension."""
    if dt is t.dtype:
        return t
    old, new = t.dtype.itemsize, dt.itemsize
    if old == new:
        st = _k.view_dtype(t._s, dt.name)
        if st is None:
            st = _k.frombytes(dt.name, _k.tobytes(t._s))
        return Tensor(st, t.shape, dt)
    what = "view %s as %s (different element sizes)" % (_CAST_NAME[t.dtype.name], _CAST_NAME[dt.name])
    if not t.shape:
        raise RuntimeError("self.dim() cannot be 0 to " + what)
    last = t.shape[-1]
    if old < new and (last * old) % new:
        raise RuntimeError("self.size(-1) must be divisible by %d to %s, but got %d" % (new // old, what, last))
    shape = _tuple(t.shape[:-1]) + (last * old // new,)
    return Tensor(_k.frombytes(dt.name, _k.tobytes(t._s)), shape, dt)


def _new3(storage, shape, dt):
    if shape.__class__ is Size and Tensor.__init__ is _tensor_init:
        t = _onew(Tensor)
        t._s = storage
        t.shape = shape
        t.dtype = dt
        return t
    return Tensor(storage, shape, dt)


def _legacy_init(self, dt, args):
    """torch.Tensor(data) / torch.Tensor(*sizes) (and FloatTensor & co.)."""
    if _len(args) == 1 and not _isinstance(args[0], (_int, Size)):
        data = args[0]
        if _isinstance(data, Tensor):
            src = data if data.dtype is dt else data.to(dt)
        else:
            src = tensor(data, dtype=dt)
        Tensor.__init__(self, src._s, src.shape, dt)
        return
    shape = _shape_args(args) if args else (0,)
    Tensor.__init__(self, _k.zeros(dt.name, _numel(shape)), shape, dt)


def _legacy_class(name, dt):
    def __new__(cls, *args, **kwargs):
        out = Tensor.__new__(Tensor)
        _legacy_init(out, dt, args)
        return out
    return type(name, (Tensor,), {"__new__": __new__})


def zeros(*size, dtype=None, layout=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args(size)
    dt = _dtype_of(dtype) or _default_dtype
    return Tensor(_k.zeros(dt.name, _numel(shape)), shape, dt, requires_grad)


def ones(*size, dtype=None, layout=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args(size)
    dt = _dtype_of(dtype) or _default_dtype
    return Tensor(_k.full(dt.name, _numel(shape), 1), shape, dt, requires_grad)


def full(size, fill_value, dtype=None, layout=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args((size,))
    if _isinstance(fill_value, Tensor):
        fill_value = fill_value.item()
    if _cx_parts(fill_value) is not None:
        dt = _dtype_of(dtype) or _default_complex()
        if dt.is_complex:
            return Tensor(_cx_from_values(dt, [fill_value] * _numel(shape)), shape, dt, requires_grad)
        fill_value = _cx_real_scalar(fill_value, _CPP_NAME.get(dt.name, dt.name))
    dt = _dtype_of(dtype) or (_default_dtype if _isinstance(fill_value, _float) else (_bool_dtype if _isinstance(fill_value, _b.bool) else int64))
    if dt.name in _NARROW_INT:
        _check_narrow(dt, [fill_value], "_t")
    return Tensor(_k.full(dt.name, _numel(shape), fill_value), shape, dt, requires_grad)


def empty(*size, dtype=None, layout=None, device=None, requires_grad=False, pin_memory=False, memory_format=None):
    _check_cpu_device(device)
    return zeros(*size, dtype=dtype, requires_grad=requires_grad)


def zeros_like(input, dtype=None, layout=None, device=None, requires_grad=False, memory_format=None):
    _check_cpu_device(device)
    if input.__class__ is _SparseTensor:
        return _sp._zeros_like(input, dtype, requires_grad)
    return zeros(*input.shape, dtype=dtype or input.dtype, requires_grad=requires_grad)


def ones_like(input, dtype=None, layout=None, device=None, requires_grad=False, memory_format=None):
    _check_cpu_device(device)
    return ones(*input.shape, dtype=dtype or input.dtype, requires_grad=requires_grad)


def full_like(input, fill_value, dtype=None, layout=None, device=None, requires_grad=False, memory_format=None):
    _check_cpu_device(device)
    return full(input.shape, fill_value, dtype=dtype or input.dtype, requires_grad=requires_grad)


def empty_like(input, dtype=None, layout=None, device=None, requires_grad=False, memory_format=None):
    _check_cpu_device(device)
    return zeros_like(input, dtype=dtype, requires_grad=requires_grad)


def rand_like(input, generator=None, dtype=None, layout=None, device=None, requires_grad=False, memory_format=None):
    return rand(*input.shape, generator=generator, dtype=dtype or input.dtype, requires_grad=requires_grad)


def randn_like(input, generator=None, dtype=None, layout=None, device=None, requires_grad=False, memory_format=None):
    return randn(*input.shape, generator=generator, dtype=dtype or input.dtype, requires_grad=requires_grad)


def randint_like(input, low=0, high=None, generator=None, dtype=None, layout=None, device=None, requires_grad=False, memory_format=None):
    if high is None:
        low, high = 0, low
    return randint(low, high, input.shape, generator=generator, dtype=dtype or input.dtype, requires_grad=requires_grad)


def _item(v):
    return v.item() if _isinstance(v, Tensor) else v


def arange(start=0, end=None, step=1, dtype=None, layout=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    start, end, step = _item(start), _item(end), _item(step)
    if end is None:
        start, end = 0, start
    if step == 0:
        raise RuntimeError("step must be nonzero")
    floating = _isinstance(start, _float) or _isinstance(end, _float) or _isinstance(step, _float)
    dt = _dtype_of(dtype) or (_default_dtype if floating else int64)
    # PyTorch's length and values: ceil((end - start) / step) elements,
    # element i = start + i * step (no accumulated rounding).
    n = _math.ceil((end - start) / step) if floating or dt.is_floating_point else -((start - end) // step)
    if n < 0 or (step > 0 and start > end) or (step < 0 and start < end):
        raise RuntimeError("upper bound and larger bound inconsistent with step sign")
    n = _int(n)
    if _isinstance(start, (_int, _float)) and _isinstance(step, (_int, _float)) and _b.max(_b.abs(start), _b.abs(n * step), _b.abs(start + n * step)) < 9007199254740992:
        # Every element start + i * step is then exact in double precision
        # for int operands, and computed as Python computes it for floats:
        # the kernel's loop gives the list's values.
        return Tensor(_k.arange(dt.name, start, step, n), (n,), dt, requires_grad)
    values = [start + i * step for i in _range(n)]
    return Tensor(_k.from_flat(dt.name, values), (n,), dt, requires_grad)


def linspace(start, end, steps, dtype=None, layout=None, device=None, requires_grad=False):
    start, end, steps = _item(start), _item(end), _int(steps)
    if steps < 0:
        raise RuntimeError("number of steps must be non-negative")
    # PyTorch's kernel: the first half steps up from start, the second
    # half down from end, so both ends are exact.
    if steps == 1:
        values = [start]
    else:
        step = (end - start) / (steps - 1) if steps > 1 else 0
        half = steps // 2
        values = [start + step * i if i < half else end - step * (steps - i - 1) for i in _range(steps)]
    dt = _dtype_of(dtype) or _default_dtype
    if dt._reduced and steps > 1:
        # PyTorch's kernel steps in the format itself: the step and every
        # product and sum round to float16/bfloat16.
        r = lambda v: _k.item(_k.full(dt.name, 1, v), 0)
        s, e = r(start), r(end)
        step = r(r(e - s) / r(steps - 1))
        half = steps // 2
        values = [r(s + r(step * r(i))) if i < half else r(e - r(step * r(steps - i - 1))) for i in _range(steps)]
    return Tensor(_k.from_flat(dt.name, values), (steps,), dt, requires_grad)


def logspace(start, end, steps, base=10.0, dtype=None, layout=None, device=None, requires_grad=False):
    exps = linspace(start, end, steps, dtype=float64)
    dt = _dtype_of(dtype) or _default_dtype
    values = [_float(base) ** v for v in _k.to_list(exps._s)]
    return Tensor(_k.from_flat(dt.name, values), (_int(steps),), dt, requires_grad)


def eye(n, m=None, dtype=None, layout=None, device=None, requires_grad=False):
    m = n if m is None else m
    dt = _dtype_of(dtype) or _default_dtype
    if dt.is_complex:
        return eye(n, m, dtype=dt.to_real()).to(dt).requires_grad_(requires_grad)
    out = zeros(n, m, dtype=dt)
    for i in _range(_b.min(n, m)):
        _k.setitem(out._s, i * m + i, 1)
    return out.requires_grad_(requires_grad)


def one_hot_(idx, n):
    return Tensor(_k.one_hot(idx._s, n), _tuple(idx.shape) + (n,), int64)


# random_() without bounds: [0, 2**mantissa digits] for a float, [0, max]
# for an integer type, as PyTorch draws them.
_RANDOM_TO = {"float32": 2 ** 24, "float64": 2 ** 53, "float16": 2 ** 11 + 1, "bfloat16": 2 ** 8 + 1, "bool": 2, "uint8": 256,
              "int8": 128, "int16": 32768, "int32": 2 ** 31, "int64": 2 ** 53}


class finfo:
    _INFO = {
        "float32": (32, 1.1920928955078125e-07, 3.4028234663852886e+38, 1.1754943508222875e-38, 1.401298464324817e-45, 1e-06, 6),
        "float64": (64, 2.220446049250313e-16, 1.7976931348623157e+308, 2.2250738585072014e-308, 5e-324, 1e-15, 15),
        "float16": (16, 0.0009765625, 65504.0, 6.103515625e-05, 5.960464477539063e-08, 0.001, 3),
        "bfloat16": (16, 0.0078125, 3.3895313892515355e+38, 1.1754943508222875e-38, 9.183549615799121e-41, 0.01, 2),
    }

    def __init__(self, type=None):
        dt = _default_dtype if type is None else _dtype_of(type)
        if dt.is_complex:
            # A complex dtype's parts: finfo of its real dtype.
            dt = dt.to_real()
        if not dt.is_floating_point:
            raise TypeError("torch.finfo() requires a floating point input type. Use torch.iinfo to handle 'torch.finfo'")
        self.bits, self.eps, self.max, self.tiny, self.smallest_subnormal, self.resolution, self.precision = self._INFO[dt.name]
        self.min = -self.max
        self.smallest_normal = self.tiny
        self.dtype = dt.name

    def __repr__(self):
        return "finfo(resolution=%g, min=%g, max=%g, eps=%g, smallest_normal=%g, tiny=%g, dtype=%s)" % (self.resolution, self.min, self.max, self.eps, self.tiny, self.tiny, self.dtype)


class iinfo:
    _INFO = {"uint8": (8, 0, 255), "int8": (8, -128, 127), "int16": (16, -32768, 32767), "int32": (32, -2 ** 31, 2 ** 31 - 1),
             "int64": (64, -2 ** 63, 2 ** 63 - 1), "bool": (8, 0, 1)}

    def __init__(self, type):
        dt = _dtype_of(type)
        if dt._inexact:
            raise TypeError("torch.iinfo() requires an integer input type. Use torch.finfo to handle 'torch.float'")
        self.bits, self.min, self.max = self._INFO[dt.name]
        self.dtype = dt.name

    def __repr__(self):
        return "iinfo(min=%d, max=%d, dtype=%s)" % (self.min, self.max, self.dtype)


class _Layout:
    def __init__(self, name="strided"):
        self._name = name

    def __repr__(self):
        return "torch." + self._name

    # A layout is a singleton: copies are the layout itself.
    def __copy__(self):
        return self

    def __deepcopy__(self, memo):
        return self


def _get_layout(name):
    """How a checkpoint names a layout (torch.serialization._get_layout)."""
    for layout in (strided, sparse_coo, sparse_csr, sparse_csc, sparse_bsr, sparse_bsc):
        if repr(layout) == name:
            return layout
    raise RuntimeError("unknown layout " + str(name))


_get_layout.__module__ = "torch.serialization"
strided = _Layout()
sparse_coo = _Layout("sparse_coo")
sparse_csr = _Layout("sparse_csr")
sparse_csc = _Layout("sparse_csc")
sparse_bsr = _Layout("sparse_bsr")
sparse_bsc = _Layout("sparse_bsc")


class memory_format:
    def __init__(self, name):
        self.name = name

    def __repr__(self):
        return "torch." + self.name


contiguous_format = memory_format("contiguous_format")
preserve_format = memory_format("preserve_format")
channels_last = memory_format("channels_last")


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
        """Reseed from a non-deterministic source and return the seed."""
        import os
        s = _int.from_bytes(os.urandom(8), "little") & ((1 << 63) - 1)
        self.manual_seed(s)
        return s

    def initial_seed(self):
        return _k.gen_initial_seed(self._g)

    def get_state(self):
        """The whole generator state (a uint8 tensor): restoring it with
        set_state continues the stream from this point."""
        state = _k.gen_get_state(self._g)
        return Tensor(state, (_k.size(state),), uint8)

    def set_state(self, new_state):
        if not _isinstance(new_state, Tensor) or new_state.dtype is not uint8:
            raise TypeError("expected a torch.ByteTensor, but got %s" % type(new_state).__name__)
        _k.gen_set_state(self._g, new_state._s)
        return self

    def clone_state(self):
        g = Generator()
        g.set_state(self.get_state())
        return g


default_generator = Generator()


def manual_seed(seed):
    default_generator.manual_seed(seed)
    return default_generator


def seed():
    return default_generator.seed()


def initial_seed():
    return default_generator.initial_seed()


def get_rng_state():
    return default_generator.get_state()


def set_rng_state(new_state):
    default_generator.set_state(new_state)


def _gen(generator):
    return (generator or default_generator)._g


def rand(*size, generator=None, dtype=None, layout=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args(size)
    dt = _dtype_of(dtype) or _default_dtype
    if dt.is_complex:
        # Uniform real and imaginary parts, drawn as 2n reals.
        return Tensor(_k.as_complex(rand(_numel(shape) * 2, generator=generator, dtype=dt.to_real())._s), shape, dt, requires_grad)
    storage = _k.rand(_gen(generator), _numel(shape), dt.name) if dt is float32 or dt._reduced else _k.rand_double(_gen(generator), _numel(shape))
    return Tensor(storage, shape, dt, requires_grad)


def randn(*size, generator=None, dtype=None, layout=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    shape = _shape_args(size)
    dt = _dtype_of(dtype) or _default_dtype
    if dt.is_complex:
        # Real and imaginary parts each N(0, 1/2), as PyTorch's complex
        # normal: 2n standard normals scaled by sqrt(1/2).
        rdt = dt.to_real()
        parts = _k.binary_scalar("mul", _k.randn(_gen(generator), _numel(shape) * 2, rdt.name), (_numel(shape) * 2,), 0.7071067811865476, rdt.name, rdt.name, False)[0]
        return Tensor(_k.as_complex(parts), shape, dt, requires_grad)
    return Tensor(_k.randn(_gen(generator), _numel(shape), dt.name), shape, dt, requires_grad)


def randint(low=0, high=None, size=None, generator=None, dtype=None, layout=None, device=None, requires_grad=False):
    _check_cpu_device(device)
    if size is None:
        # randint(high, size) form
        low, high, size = 0, low, high
    elif high is None:
        # randint(high, size=...) form
        low, high = 0, low
    if size is None:
        raise TypeError("randint() missing 1 required positional argument: 'size'")
    shape = _shape_args((size,))
    dt = _dtype_of(dtype) or int64
    out = Tensor(_k.randint(_gen(generator), _int(low), _int(high), _numel(shape)), shape, int64)
    out = out if dt is int64 else out.to(dt)
    return out.requires_grad_(requires_grad) if requires_grad else out


def _random64(generator=None):
    """One draw of PyTorch's CPU random64() (two MT19937 words, the first
    high) as a Python int: what `torch.empty((), dtype=torch.int64).random_()`
    reduces modulo 2**63."""
    return _k.random64(_gen(generator), 1)[0]


def randperm(n, generator=None, dtype=None, layout=None, device=None, requires_grad=False):
    out = Tensor(_k.randperm(_gen(generator), _int(n)), (_int(n),), int64)
    dt = _dtype_of(dtype)
    return out if dt is None or dt is int64 else out.to(dt)


def multinomial(input, num_samples, replacement=False, generator=None):
    probs = input
    if _len(probs.shape) not in (1, 2):
        raise RuntimeError("prob_dist must be 1 or 2 dim")
    storage = _k.multinomial(_gen(generator), probs._s, probs.shape, _int(num_samples), _bool(replacement))
    shape = (num_samples,) if _len(probs.shape) == 1 else (probs.shape[0], num_samples)
    return Tensor(storage, shape, int64)


def bernoulli(input, p=None, generator=None):
    if p is not None:
        input = full(input.shape, _float(p), dtype=input.dtype)
    return (rand(*input.shape, generator=generator, dtype=input.dtype if input.dtype.is_floating_point else None) < input).to(input.dtype)


def normal(mean=0.0, std=1.0, size=None, generator=None, dtype=None, layout=None, device=None, requires_grad=False):
    """normal(mean, std) with tensor mean and/or std (their broadcast shape),
    or float mean and std with `size`."""
    if _isinstance(mean, Tensor) or _isinstance(std, Tensor):
        like = mean if _isinstance(mean, Tensor) else std
        shape = _broadcast_shapes(mean.shape if _isinstance(mean, Tensor) else (), std.shape if _isinstance(std, Tensor) else ())
        z = randn(*shape, generator=generator, dtype=like.dtype)
        return add(mul(z, std), mean).detach()
    if size is None:
        raise TypeError("normal() missing the size argument for float mean and std")
    out = randn(*_shape_args((size,)), generator=generator, dtype=dtype)
    out = mul(out, _float(std)) if std != 1 else out
    out = add(out, _float(mean)) if mean != 0 else out
    return out.requires_grad_(requires_grad) if requires_grad else out


def poisson(input, generator=None):
    """Poisson draws with torch.distributions.Poisson's sampler (inversion
    below rate 10, Hormann's PTRS above it, so any rate is cheap) on the
    generator's stream; no gradient, as in PyTorch."""
    import torch.distributions as dist
    # Reading the generator through `_gen` first lets a prepared
    # torch.compile step see (and refuse) a draw it would replay.
    _gen(generator)
    if generator is None or generator is default_generator:
        return dist._poisson_sample(input)
    # The sampler draws from the default generator: lend it this one's state.
    saved = default_generator._g
    default_generator._g = generator._g
    try:
        return dist._poisson_sample(input)
    finally:
        default_generator._g = saved


class _RandomModule:
    """`torch.random`: the default generator's seeding and state."""
    manual_seed = staticmethod(manual_seed)
    seed = staticmethod(seed)
    initial_seed = staticmethod(initial_seed)
    get_rng_state = staticmethod(get_rng_state)
    set_rng_state = staticmethod(set_rng_state)
    default_generator = default_generator

    class fork_rng:
        def __init__(self, devices=None, enabled=True, **kwargs):
            self.enabled = enabled

        def __enter__(self):
            if self.enabled:
                self._state = get_rng_state()
            return self

        def __exit__(self, *exc):
            if self.enabled:
                set_rng_state(self._state)
            return False


random = _RandomModule()


# ---- elementwise ops with autograd ------------------------------------------------------
def _any_requires_grad(args):
    for a in args:
        if _isinstance(a, Tensor) and a.requires_grad:
            return True
    return False


def _binary_nograd(op, a, b):
    if a.__class__ is Tensor and not _graph_recording:
        cb = b.__class__
        if cb is _float or cb is _int:
            # `_operands`' plain-number case, the number going to the kernel
            # as it is (`binary_scalar` stores it as `full` would).
            dt = a.dtype
            if not dt._inexact:
                dt = _default_dtype if cb is _float else (int64 if dt is _bool_dtype else dt)
            storage, shape = _k.binary_scalar(op, a._s, a.shape, b, dt.name, dt.name, False)
            return _new3(storage, shape, _DTYPES[_k.dtype(storage)])
    if a.__class__ is _SparseTensor or b.__class__ is _SparseTensor:
        return _sp._binary(op, a, b)
    try:
        ta, tb, dt = _operands(a, b)
        ta.shape, tb.shape
    except RuntimeError as e:
        _lazy_reraise(e, op, a, b)
    storage, shape = _k.binary(op, ta._s, ta.shape, tb._s, tb.shape, None if dt is None else dt.name)
    return _new3(storage, shape, _DTYPES[_k.dtype(storage)])


def _unary_nograd(op, a, p1=None, p2=None):
    if a.__class__ is _SparseTensor:
        return _sp._unary_nograd(op, a, p1, p2)
    storage = _k.unary(op, a._s, p1, p2)
    try:
        shape = a.shape
    except RuntimeError as e:
        _lazy_reraise(e, op, a)
    return Tensor(storage, shape, _DTYPES[_k.dtype(storage)])


_NOSAVE = ("", "")


def _binary(op, a, b, name, backward, saves=("xy", "xy"), want=None, opmath=False):
    """An elementwise binary op. `saves` names the values backward reads
    when the first/second operand requires grad ("x", "y", "o" for the
    output), so writing one of them in place before backward raises.

    Never a comparison: the result dtype is the kernel's `want` (or the
    operands' shared dtype) whenever that is floating, and only otherwise
    asked of the kernel."""
    ca = a.__class__
    cb = b.__class__
    if ca is Tensor and cb is Tensor and a.dtype is b.dtype:
        ta, tb, dt = a, b, want
    elif ca is Tensor and (cb is _float or cb is _int) and not _graph_recording:
        # `_operands`' plain-number case: the number becomes a 0-d tensor of
        # the result dtype (float32 for a float16/bfloat16 `opmath` op).
        dt = a.dtype
        if not dt._inexact:
            dt = _default_dtype if cb is _float else (int64 if dt is _bool_dtype else dt)
        sdt = float32 if opmath and dt._reduced else dt
        if want is not None:
            dt = want
        if not (_grad_enabled and a.requires_grad):
            # Nothing records: the kernel takes the number itself.
            storage, shape = _k.binary_scalar(op, a._s, a.shape, b, sdt.name, dt.name, False)
            return _new3(storage, shape, dt if dt.is_floating_point else _DTYPES[_k.dtype(storage)])
        ta, tb = a, _scalar_operand(b, sdt)
    elif cb is Tensor and (ca is _float or ca is _int) and not _graph_recording:
        dt = b.dtype
        if not dt._inexact:
            dt = _default_dtype if ca is _float else (int64 if dt is _bool_dtype else dt)
        sdt = dt
        if want is not None:
            dt = want
        if not (_grad_enabled and b.requires_grad):
            storage, shape = _k.binary_scalar(op, b._s, b.shape, a, sdt.name, dt.name, True)
            return _new3(storage, shape, dt if dt.is_floating_point else _DTYPES[_k.dtype(storage)])
        ta, tb = _scalar_operand(a, sdt), b
    else:
        if ca is _SparseTensor or cb is _SparseTensor:
            return _sp._binary(op, a, b)
        try:
            ta, tb, dt = _operands(a, b, opmath)
            # (an uninitialized parameter's shape raises here, not below)
            ta.shape, tb.shape
        except RuntimeError as e:
            _lazy_reraise(e, name.lower(), a, b)
        if want is not None:
            dt = want
    if dt is None:
        storage, shape = _k.binary(op, ta._s, ta.shape, tb._s, tb.shape, None)
        rdt = ta.dtype
        if not (rdt.is_floating_point and rdt is tb.dtype):
            rdt = _DTYPES[_k.dtype(storage)]
    else:
        storage, shape = _k.binary(op, ta._s, ta.shape, tb._s, tb.shape, dt.name)
        rdt = dt if dt.is_floating_point else _DTYPES[_k.dtype(storage)]
    out = _new3(storage, shape, rdt)
    ra = ta.requires_grad
    rb = tb.requires_grad
    if _grad_enabled and (ra or rb):
        if not rdt.is_floating_point and rdt.is_complex:
            backward = _COMPLEX_BACKWARD.get(backward, backward)
        out.requires_grad = True
        need = (saves[0] if ra else "") + (saves[1] if rb else "")
        saved = None
        if need:
            if "x" in need:
                saved = [ta]
                if "y" in need:
                    saved.append(tb)
            elif "y" in need:
                saved = [tb]
            if "o" in need:
                if saved is None:
                    saved = [out]
                else:
                    saved.append(out)
        if saves is _NOSAVE:
            # add/sub: backward reads only the operands' shapes, which
            # `_frozen` would take from the tensors themselves anyway.
            out._node = _Node(lambda g: backward(g, ta, tb, None), (ta, tb), name, saved)
        else:
            sa = ta._s
            sb = tb._s
            out._node = _Node(lambda g: backward(g, _frozen(ta, sa), _frozen(tb, sb), _frozen(out, storage)), (ta, tb), name, saved)
    return out


def _add_backward(g, x, y, o):
    return (_unbroadcast(g, x.shape), _unbroadcast(g, y.shape))


def _sub_backward(g, x, y, o):
    return (_unbroadcast(g, x.shape), _unbroadcast(neg(g), y.shape))


def _cj(t):
    """conj(t) for a complex tensor, t itself otherwise: a complex op's
    gradient is the upstream gradient times the conjugate derivative
    (PyTorch's convention for a real loss)."""
    return conj(t) if t.dtype.is_complex else t


def _mul_backward(g, x, y, o):
    return (_unbroadcast(mul(g, y), x.shape) if x.requires_grad else None, _unbroadcast(mul(g, x), y.shape) if y.requires_grad else None)


def _div_backward(g, x, y, o):
    return (_unbroadcast(div(g, y), x.shape) if x.requires_grad else None, _unbroadcast(neg(mul(g, div(o, y))), y.shape) if y.requires_grad else None)


def _mul_backward_c(g, x, y, o):
    return (_unbroadcast(mul(g, _cj(y)), x.shape) if x.requires_grad else None, _unbroadcast(mul(g, _cj(x)), y.shape) if y.requires_grad else None)


def _div_backward_c(g, x, y, o):
    return (_unbroadcast(div(g, _cj(y)), x.shape) if x.requires_grad else None, _unbroadcast(neg(mul(g, _cj(div(o, y)))), y.shape) if y.requires_grad else None)


# The backward a binary op with a complex result takes instead (the
# conjugate derivative); `_binary` swaps it in when it records one.
_COMPLEX_BACKWARD = {_mul_backward: _mul_backward_c, _div_backward: _div_backward_c}


def _scaled(b, alpha):
    if alpha == 1:
        return b
    return mul(b, alpha) if _isinstance(b, Tensor) else b * alpha


def _alpha_scaled(b, alpha):
    """alpha * b for a float16/bfloat16 add/sub: PyTorch's kernel rounds
    alpha to the format and forms the product in float, rounding only the
    sum."""
    return mul(_cast(b, float32), _k.item(_k.full(b.dtype.name, 1, alpha), 0))


def add(input, other, alpha=1):
    a, b = input, other
    if _graph_recording:
        # The multiply only when alpha asks for it: an eager `b * 1` would
        # hand the graph a copy of a captured tensor.
        if hasattr(a, "_zipp_graph"):
            return a.__add__(b if alpha == 1 else b * alpha)
        if hasattr(b, "_zipp_graph"):
            return (b if alpha == 1 else b * alpha).__radd__(a)
    if alpha != 1 and _isinstance(b, Tensor) and b.dtype._reduced and _isinstance(a, Tensor) and a.dtype is b.dtype:
        return _binary("add", a, _alpha_scaled(b, alpha), "Add", _add_backward, _NOSAVE, a.dtype)
    return _binary("add", a, b if alpha == 1 else _scaled(b, alpha), "Add", _add_backward, _NOSAVE)


def sub(input, other, alpha=1):
    a, b = input, other
    if _graph_recording:
        if hasattr(a, "_zipp_graph"):
            return a.__sub__(b if alpha == 1 else b * alpha)
        if hasattr(b, "_zipp_graph"):
            return (b if alpha == 1 else b * alpha).__rsub__(a)
    if _isinstance(a, Tensor) and a.dtype is _bool_dtype and _isinstance(b, Tensor) and b.dtype is _bool_dtype:
        raise RuntimeError("Subtraction, the `-` operator, with two bool tensors is not supported. If you are trying to invert a mask, use the `~` or `logical_not()` operator instead.")
    if alpha != 1 and _isinstance(b, Tensor) and b.dtype._reduced and _isinstance(a, Tensor) and a.dtype is b.dtype:
        return _binary("sub", a, _alpha_scaled(b, alpha), "Sub", _sub_backward, _NOSAVE, a.dtype)
    return _binary("sub", a, b if alpha == 1 else _scaled(b, alpha), "Sub", _sub_backward, _NOSAVE)


subtract = sub


def rsub(input, other, alpha=1):
    return sub(other, input, alpha=alpha)


def mul(input, other):
    a, b = input, other
    if _graph_recording:
        if hasattr(a, "_zipp_graph"):
            return a.__mul__(b)
        if hasattr(b, "_zipp_graph"):
            return b.__rmul__(a)
    return _binary("mul", a, b, "Mul", _mul_backward, ("y", "x"), None, True)


multiply = mul


def _float_result(a, b):
    if type(a) is Tensor:
        # A floating tensor with a Python number or a tensor of its own
        # dtype: `_result_type` gives that dtype.
        dt = a.dtype
        if dt._inexact:
            tb = type(b)
            if tb is _float or tb is _int or (tb is Tensor and b.dtype is dt):
                return dt
    dt = _result_type(a, b)
    return dt if dt._inexact else _default_dtype


def div(input, other, rounding_mode=None):
    a, b = input, other
    if _graph_recording and rounding_mode is None and (getattr(a, "_zipp_graph", False) or getattr(b, "_zipp_graph", False)):
        return a / b
    if rounding_mode is None:
        return _binary("div", a, b, "Div", _div_backward, ("y", "yo"), _float_result(a, b), True)
    if rounding_mode == "floor":
        return floor_divide(a, b)
    if rounding_mode == "trunc":
        _check_int_divisor(a, b)
        dt = _result_type(a, b)
        q = trunc(_binary_nograd("div", a if not _isinstance(a, Tensor) or a.dtype.is_floating_point else a.double(), b if not _isinstance(b, Tensor) or b.dtype.is_floating_point else b.double()))
        return q if q.dtype is dt else q.to(dt)
    raise RuntimeError("div expected rounding_mode to be one of None, 'trunc', or 'floor' but found '%s'" % rounding_mode)


divide = div
true_divide = div


def _check_int_divisor(a, b):
    # Integer division by zero raises, as PyTorch's integer kernels do.
    if not _result_type(a, b).is_floating_point:
        tb = b if _isinstance(b, Tensor) else tensor(b)
        if _bool(_reduce_nograd("any", _binary_nograd("eq", tb, 0), None, False).item()):
            raise RuntimeError("ZeroDivisionError")


def floor_divide(input, other):
    _check_int_divisor(input, other)
    return _binary_nograd("floordiv", input, other)


def remainder(input, other):
    _check_int_divisor(input, other)
    return _binary("mod", input, other, "Remainder", lambda g, x, y, o: (_unbroadcast(g, x.shape), _unbroadcast(mul(neg(g), floor(div(x, y))), y.shape) if y.requires_grad else None), ("", "xy"))


def fmod(input, other):
    _check_int_divisor(input, other)
    return _binary("fmod", input, other, "Fmod", lambda g, x, y, o: (_unbroadcast(g, x.shape), _unbroadcast(mul(neg(g), trunc(div(x, y))), y.shape) if y.requires_grad else None), ("", "xy"))


def _pow_backward(g, x, y, o):
    gx = gy = None
    if x.dtype.is_complex or y.dtype.is_complex:
        # d/dx x**y = y x**(y-1), d/dy = x**y log x (conjugated); x**0
        # contributes nothing to x, 0**y (y != 0) nothing to y.
        if x.requires_grad:
            gx = _unbroadcast(mul(g, _cj(where(y == 0, zeros_like(o), mul(y, pow(x, sub(y, 1)))))), x.shape)
        if y.requires_grad:
            gy = _unbroadcast(mul(g, _cj(where(x == 0, zeros_like(o), mul(o, log(x))))), y.shape)
        return (gx, gy)
    if x.requires_grad:
        gx = _unbroadcast(where(y == 0, 0.0, mul(g, mul(y, pow(x, sub(y, 1))))), x.shape)
    if y.requires_grad:
        gy = _unbroadcast(where(logical_and(x == 0, y >= 0), 0.0, mul(g, mul(o, log(x)))), y.shape)
    return (gx, gy)


def pow(input, exponent):
    a, b = input, exponent
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a ** b
    if _isinstance(a, Tensor) and not _isinstance(b, Tensor):
        if not a.dtype._inexact and _isinstance(b, _int) and not _isinstance(b, _b.bool) and b < 0:
            raise RuntimeError("Integers to negative integer powers are not allowed.")
        e = b

        def backward(g, x, y, o):
            if e == 0:
                return (zeros_like(x, dtype=g.dtype), None)
            return (mul(g, _cj(mul(pow(x, e - 1), e))), None)
        return _binary("pow", a, b, "Pow", backward, ("x", ""))
    return _binary("pow", a, b, "Pow", _pow_backward, ("xy", "xyo"))


def float_power(input, exponent):
    a = input.double() if _isinstance(input, Tensor) else _float(input)
    b = exponent.double() if _isinstance(exponent, Tensor) else _float(exponent)
    if not _isinstance(a, Tensor):
        a = tensor(a, dtype=float64)
    return pow(a, b)


def atan2(input, other):
    def backward(g, x, y, o):
        r = add(square(x), square(y))
        return (_unbroadcast(mul(g, div(y, r)), x.shape) if x.requires_grad else None, _unbroadcast(mul(g, div(neg(x), r)), y.shape) if y.requires_grad else None)
    return _binary("atan2", input, other, "Atan2", backward, ("xy", "xy"), want=_float_result(input, other))


arctan2 = atan2


def _split_ties(g, win_x, win_y, x, y):
    # Elementwise max/min: the winner takes the gradient and a tie splits it evenly, as in PyTorch.
    half = mul((x == y).to(g.dtype), 0.5)
    return (_unbroadcast(mul(g, add(win_x.to(g.dtype), half)), x.shape), _unbroadcast(mul(g, add(win_y.to(g.dtype), half)), y.shape))


def maximum(input, other):
    return _binary("max", input, other, "Maximum", lambda g, x, y, o: _split_ties(g, x > y, y > x, x, y))


def minimum(input, other):
    return _binary("min", input, other, "Minimum", lambda g, x, y, o: _split_ties(g, x < y, y < x, x, y))


def fmax(input, other):
    # NaN-ignoring maximum.
    return where(isnan(other), input, where(isnan(input), other, maximum(input, other)))


def fmin(input, other):
    return where(isnan(other), input, where(isnan(input), other, minimum(input, other)))


# The 0-d tensors a Python number becomes as an operand of a recorded op,
# per dtype name and number. Each is internal to the nodes that hold it
# (never written, never requiring grad), so one serves every op with that
# number as `_operands` would build it afresh. Keyed by the number alone:
# an int and the float equal to it store the same value in the dtype. Zero
# is not kept (0.0 == -0.0 as a key), NaN never matches itself, and a
# dtype's table is emptied when it fills.
_SCALARS = dict((name, {}) for name in _DTYPES)


def _scalar_operand(v, dt):
    if v != 0 and v == v:
        table = _SCALARS[dt.name]
        try:
            return table[v]
        except KeyError:
            pass
        if _len(table) >= 64:
            table.clear()
        t = table[v] = Tensor(_k.full(dt.name, 1, v), _SCALAR_SHAPE, dt)
        return t
    return Tensor(_k.full(dt.name, 1, v), _SCALAR_SHAPE, dt)


# The unary kernels whose result is not the (floating) input's dtype
# (tensor.js's BOOL_UNARY).
_BOOL_UNARY = frozenset(["isfinite", "isnan", "not", "isinf", "isposinf", "isneginf", "signbit"])


def _unary(op, a, name, backward, p1=None, p2=None, saves="x"):
    """An elementwise unary op; `saves` as in `_binary` ("x" input, "o" output)."""
    if a.__class__ is _SparseTensor:
        return _sp._unary(op, a, name, backward, p1, p2, saves)
    dt = a.dtype
    if dt.is_floating_point:
        storage = _k.unary(op, a._s, p1, p2)
        try:
            shape = a.shape
        except RuntimeError as e:
            _lazy_reraise(e, name.lower(), a)
        out = _new3(storage, shape, dt if op not in _BOOL_UNARY else _DTYPES[_k.dtype(storage)])
    elif dt.is_complex:
        return _cx_unary(op, a, name)
    else:
        storage = _k.unary(op, a._s, p1, p2)
        out = _new3(storage, a.shape, _DTYPES[_k.dtype(storage)])
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        sa = a._s
        saved = None if not saves else ([a] if saves == "x" else [out])
        out._node = _Node(lambda g: (backward(g, _frozen(a, sa), _frozen(out, storage)),), (a,), name, saved)
    return out


def neg(input):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return -a
    if a.dtype is _bool_dtype:
        raise RuntimeError("Negation, the `-` operator, on a bool tensor is not supported. If you are trying to invert a mask, use the `~` or `logical_not()` operator instead.")
    return _unary("neg", a, "Neg", lambda g, x, o: neg(g), saves="")


negative = neg


def exp(input):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.exp()
    return _unary("exp", a, "Exp", lambda g, x, o: mul(g, o), saves="o")


def exp2(input):
    return _unary("exp2", input, "Exp2", lambda g, x, o: mul(g, mul(o, _math.log(2.0))), saves="o")


def expm1(input):
    return _unary("expm1", input, "Expm1", lambda g, x, o: mul(g, add(o, 1)), saves="o")


def log(input):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.log()
    return _unary("log", a, "Log", lambda g, x, o: div(g, x))


def log2(input):
    return _unary("log2", input, "Log2", lambda g, x, o: div(g, mul(x, _math.log(2.0))))


def log10(input):
    return _unary("log10", input, "Log10", lambda g, x, o: div(g, mul(x, _math.log(10.0))))


def log1p(input):
    return _unary("log1p", input, "Log1p", lambda g, x, o: div(g, add(x, 1)))


def tanh(input):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.tanh()
    return _unary("tanh", a, "Tanh", lambda g, x, o: mul(g, sub(1, square(o))), saves="o")


def sigmoid(input):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.sigmoid()
    return _unary("sigmoid", a, "Sigmoid", lambda g, x, o: mul(g, mul(o, sub(1, o))), saves="o")


def silu(input):
    if _graph_recording and getattr(input, "_zipp_graph", False):
        return input.silu()
    return _unary("silu", input, "Silu", lambda g, x, o: mul(g, _silu_grad(x)))


def _gelu(a):
    """The exact-erf GELU, F.gelu's default: x * cdf(x), with the derivative
    cdf(x) + x * pdf(x) for autograd. Under a compiled graph it records the
    protocol's gelu, whose gradient is the same closed form."""
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.gelu()
    return _unary("gelu", a, "Gelu", lambda g, x, o: mul(g, _gelu_grad(x)))


def _gelu_grad(x):
    # gelu'(x) as its own op, differentiable once more: gelu''(x) = pdf(x) * (2 - x**2).
    return _unary("gelu_grad", x, "GeluBackward", lambda g, x, o: mul(g, mul(mul(exp(mul(square(x), -0.5)), 0.3989422804014327), sub(2, square(x)))))


def _silu_grad(x):
    s = sigmoid(x)
    return mul(s, add(1, mul(x, sub(1, s))))


def relu(input):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.relu()
    # PyTorch's threshold_backward: the gradient passes where the result is
    # not <= 0, so a NaN input (a NaN result) passes it too.
    return _unary("relu", a, "Relu", lambda g, x, o: where(o <= 0, 0.0, g), saves="o")


def sqrt(input):
    if _graph_recording and getattr(input, "_zipp_graph", False):
        return input.sqrt()
    return _unary("sqrt", input, "Sqrt", lambda g, x, o: div(g, mul(o, 2)), saves="o")


def rsqrt(input):
    if _graph_recording and getattr(input, "_zipp_graph", False):
        return input.rsqrt()
    # d/dx x**-0.5 = -0.5 * x**-1.5 = -o**3 / 2, written from the output so the
    # backward pass needs no second root.
    return _unary("rsqrt", input, "Rsqrt", lambda g, x, o: mul(g, div(neg(mul(o, square(o))), 2)), saves="o")


def reciprocal(input):
    if _graph_recording and getattr(input, "_zipp_graph", False):
        return input.reciprocal()
    return _unary("reciprocal", input, "Reciprocal", lambda g, x, o: mul(g, neg(square(o))), saves="o")


def repeat_interleave(input, repeats=None, dim=None, output_size=None):
    """Repeat each element along `dim`, as torch does: an int count by an
    unsqueeze, an expand and a reshape; per-element counts (a tensor or
    list) by index_select, so autograd flows either way."""
    t = _as_tensor(input)
    if repeats is None:
        # repeat_interleave(repeats): the indices repeated by their counts.
        counts = _k.to_list(t._s)
        return tensor([i for i, c in enumerate(counts) for _ in _range(_int(c))], dtype=int64)
    if dim is None:
        t = t.reshape(-1)
        dim = 0
    rank = _len(t.shape)
    dim = _norm_dim(dim, rank)
    if _isinstance(repeats, (Tensor, _list, _tuple)):
        counts = [_int(c) for c in (_k.to_list(repeats._s) if _isinstance(repeats, Tensor) else repeats)]
        if _len(counts) == 1:
            repeats = counts[0]
        else:
            if _len(counts) != t.shape[dim]:
                raise RuntimeError("repeats must have the same size as input along dim")
            idx = tensor([i for i, c in enumerate(counts) for _ in _range(c)], dtype=int64)
            return index_select(t, dim, idx)
    repeats = _int(repeats)
    if repeats < 0:
        raise RuntimeError("repeats can not be negative")
    if repeats == 1:
        return t
    expanded = _list(t.shape)
    expanded.insert(dim + 1, repeats)
    out = _list(t.shape)
    out[dim] = out[dim] * repeats
    return t.unsqueeze(dim + 1).expand(expanded).reshape(out)


def sin(input):
    return _unary("sin", input, "Sin", lambda g, x, o: mul(g, cos(x)))


def cos(input):
    return _unary("cos", input, "Cos", lambda g, x, o: mul(g, neg(sin(x))))


def tan(input):
    return _unary("tan", input, "Tan", lambda g, x, o: mul(g, add(1, square(o))), saves="o")


def asin(input):
    return _unary("asin", input, "Asin", lambda g, x, o: div(g, sqrt(sub(1, square(x)))))


def acos(input):
    return _unary("acos", input, "Acos", lambda g, x, o: neg(div(g, sqrt(sub(1, square(x))))))


def atan(input):
    return _unary("atan", input, "Atan", lambda g, x, o: div(g, add(1, square(x))))


def sinh(input):
    return _unary("sinh", input, "Sinh", lambda g, x, o: mul(g, cosh(x)))


def cosh(input):
    return _unary("cosh", input, "Cosh", lambda g, x, o: mul(g, sinh(x)))


def asinh(input):
    return _unary("asinh", input, "Asinh", lambda g, x, o: div(g, sqrt(add(square(x), 1))))


def acosh(input):
    return _unary("acosh", input, "Acosh", lambda g, x, o: div(g, sqrt(sub(square(x), 1))))


def atanh(input):
    return _unary("atanh", input, "Atanh", lambda g, x, o: div(g, sub(1, square(x))))


arcsin, arccos, arctan, arcsinh, arccosh, arctanh = asin, acos, atan, asinh, acosh, atanh


def erf(input):
    return _unary("erf", input, "Erf", lambda g, x, o: mul(g, mul(exp(neg(square(x))), 1.1283791670955126)))


def erfc(input):
    return _unary("erfc", input, "Erfc", lambda g, x, o: mul(g, mul(exp(neg(square(x))), -1.1283791670955126)))


def erfinv(input):
    return _unary("erfinv", input, "Erfinv", lambda g, x, o: mul(g, mul(exp(square(o)), 0.886226925452758)), saves="o")


# ---- special functions (torch.special has the rest) --------------------------------------
# The kernels compute in double precision with ATen's algorithms (Cephes),
# rounding once to the result dtype; integer inputs give the default float.
def _float_input(a):
    return a if _isinstance(a, Tensor) else tensor(a, dtype=_default_dtype)


def lgamma(input):
    return _unary("lgamma", _float_input(input), "Lgamma", lambda g, x, o: mul(g, digamma(x)))


def digamma(input):
    return _unary("digamma", _float_input(input), "Digamma", lambda g, x, o: mul(g, polygamma(1, x)))


def polygamma(n, input):
    n = _int(n)
    if n < 0:
        raise RuntimeError("polygamma(n, x) does not support negative n.")
    return _unary("polygamma", _float_input(input), "Polygamma", lambda g, x, o: mul(g, polygamma(n + 1, x)), n)


def mvlgamma(input, p):
    """The multivariate log-gamma of dimension p: sum over i < p of
    lgamma(x - i/2), plus p(p-1)/4 log(pi) (PyTorch's composition, so
    autograd follows)."""
    a = _float_input(input)
    p = _int(p)
    if p < 1:
        raise RuntimeError("p has to be greater than or equal to 1")
    if not a.dtype.is_floating_point:
        a = a.to(_default_dtype)
    offsets = tensor([-(i / 2.0) for i in _range(p)], dtype=a.dtype)
    return add(sum(lgamma(add(unsqueeze(a, -1), offsets)), -1), p * (p - 1) * _math.log(_math.pi) / 4)


def i0(input):
    return _unary("i0", _float_input(input), "I0", lambda g, x, o: mul(g, _unary("i1", x, "SpecialI1", _i1_backward)))


def _i1_backward(g, x, o):
    # i1'(x) = i0(x) - i1(x)/x, 1/2 at 0.
    return mul(g, where(x == 0, 0.5, sub(i0(x), div(o, x))))


def _i0e_backward(g, x, o):
    return mul(g, sub(_unary("i1e", x, "SpecialI1E", _i1e_backward), mul(sign(x), o)))


def _i1e_backward(g, x, o):
    return mul(g, where(x == 0, 0.5, sub(_unary("i0e", x, "SpecialI0E", _i0e_backward), mul(o, add(sign(x), reciprocal(x))))))


def sinc(input):
    def backward(g, x, o):
        px = mul(x, _math.pi)
        d = div(sub(mul(px, cos(px)), sin(px)), mul(px, x))
        return mul(g, where(x == 0, 0.0, d))
    return _unary("sinc", _float_input(input), "Sinc", backward)


def logit(input, eps=None):
    lo = None if eps is None else _float(eps)

    def backward(g, x, o):
        d = div(g, mul(x, sub(1, x)))
        if lo is None:
            return where(logical_or(x < 0, x > 1), _math.nan, d)
        return where(logical_or(x < lo, x > 1 - lo), 0.0, d)
    return _unary("logit", _float_input(input), "Logit", backward, lo)


def xlogy(input, other):
    def backward(g, x, y, o):
        # PyTorch's: xlogy(g, y), zero where x is 0 and y <= 0.
        return (_unbroadcast(where(logical_and(x == 0, y <= 0), 0.0, xlogy(g, y)), x.shape) if x.requires_grad else None,
                _unbroadcast(mul(g, div(x, y)), y.shape) if y.requires_grad else None)
    return _binary("xlogy", input, other, "Xlogy", backward, ("xy", "xy"), _float_result(input, other))


def igamma(input, other):
    """The regularized lower incomplete gamma function P(input, other)."""
    return _binary("igamma", input, other, "Igamma", _igamma_backward("igamma", 1), ("xy", "xy"), _float_result(input, other))


def igammac(input, other):
    """The regularized upper incomplete gamma function Q(input, other)."""
    return _binary("igammac", input, other, "Igammac", _igamma_backward("igammac", -1), ("xy", "xy"), _float_result(input, other))


def _igamma_backward(name, sign_):
    def backward(g, a, x, o):
        if a.requires_grad:
            raise NotImplementedError("the derivative for '%s: input' is not implemented." % name)
        # d/dx P(a, x) = x^(a-1) e^-x / Gamma(a).
        d = exp(sub(sub(mul(sub(a, 1), log(x)), x), lgamma(a)))
        return (None, _unbroadcast(mul(g, d if sign_ > 0 else neg(d)), x.shape))
    return backward


def square(input):
    if _graph_recording and getattr(input, "_zipp_graph", False):
        return input.square()
    return _unary("square", input, "Pow", lambda g, x, o: mul(g, mul(x, 2)))


def abs(input):
    if _graph_recording and getattr(input, "_zipp_graph", False):
        return input.abs()
    if input.dtype.is_complex:
        return _cx_abs(input)
    return _unary("abs", input, "Abs", lambda g, x, o: mul(g, _unary_nograd("sign", x)))


absolute = abs


def _zero_grad_unary(op, name):
    def f(input):
        a = input
        if not a.dtype._inexact and op in ("floor", "ceil", "round", "trunc"):
            # Integer values round to themselves.
            return Tensor(_k.copy(a._s), a.shape, a.dtype)
        return _unary(op, a, name, lambda g, x, o: zeros_like(g), saves="")
    f.__name__ = op
    return f


floor = _zero_grad_unary("floor", "Floor")
ceil = _zero_grad_unary("ceil", "Ceil")
trunc = _zero_grad_unary("trunc", "Trunc")
fix = trunc
_sign_real = _zero_grad_unary("sign", "Sign")


def sign(input):
    if input.dtype.is_complex:
        raise NotImplementedError("Unlike NumPy, torch.sign is not intended to support complex numbers. Please use torch.sgn instead.")
    return _sign_real(input)


def sgn(input):
    return _cx_sgn(input) if input.dtype.is_complex else _sign_real(input)

_round_half_even = _zero_grad_unary("round", "Round")


def round(input, decimals=0):
    if decimals == 0:
        return _round_half_even(input)
    # PyTorch's form: round half to even at the scaled value, then unscale.
    scale = 10.0 ** decimals
    return div(_round_half_even(mul(input, scale)), scale)


def frac(input):
    return _unary("frac", input, "Frac", lambda g, x, o: g, saves="")


def clamp(input, min=None, max=None):
    a = input
    if min is None and max is None:
        raise RuntimeError("torch.clamp: At least one of 'min' or 'max' must not be None")
    if _isinstance(min, Tensor) or _isinstance(max, Tensor):
        # Tensor bounds: where() picks each bound, so the gradient goes to
        # the input inside [min, max] and to the bound that was taken.
        out = a
        if min is not None:
            out = where(out < min, min, out)
        if max is not None:
            out = where(out > max, max, out)
        return out
    if not a.dtype.is_floating_point and (_isinstance(min, _float) or _isinstance(max, _float)):
        a = a.to(_default_dtype)
    lo = None if min is None else _float(min)
    hi = None if max is None else _float(max)

    def backward(g, x, o):
        mask = None
        if lo is not None:
            mask = x >= lo
        if hi is not None:
            mask = (x <= hi) if mask is None else logical_and(mask, x <= hi)
        return where(mask, g, 0.0)
    return _unary("clamp", a, "Clamp", backward, lo, hi)


clip = clamp


def clamp_min(input, min):
    return clamp(input, min, None)


def clamp_max(input, max):
    return clamp(input, None, max)


def logical_not(input):
    return _unary_nograd("not", input)


def eq(input, other):
    return _binary_nograd("eq", input, other)


def ne(input, other):
    return _binary_nograd("ne", input, other)


def lt(input, other):
    return _binary_nograd("lt", input, other)


def le(input, other):
    return _binary_nograd("le", input, other)


def gt(input, other):
    return _binary_nograd("gt", input, other)


def ge(input, other):
    return _binary_nograd("ge", input, other)


greater, greater_equal, less, less_equal, not_equal = gt, ge, lt, le, ne


def logical_and(input, other):
    return _binary_nograd("and", input, other)


def logical_or(input, other):
    return _binary_nograd("or", input, other)


def logical_xor(input, other):
    return _binary_nograd("xor", input, other)


def _bitwise(op, logical, name, a, b):
    ta, tb, dt = _operands(a, b)
    dt = dt or _promote_types(ta.dtype, tb.dtype)
    if dt.is_floating_point:
        raise NotImplementedError("\"%s_cpu\" not implemented for '%s'" % (name, _CAST_NAME[dt.name]))
    storage, shape = _k.binary(logical if dt is _bool_dtype and logical else op, ta._s, ta.shape, tb._s, tb.shape, dt.name)
    return Tensor(storage, shape, dt)


def bitwise_and(input, other):
    return _bitwise("bitand", "and", "bitwise_and", input, other)


def bitwise_or(input, other):
    return _bitwise("bitor", "or", "bitwise_or", input, other)


def bitwise_xor(input, other):
    return _bitwise("bitxor", "xor", "bitwise_xor", input, other)


def bitwise_left_shift(input, other):
    return _bitwise("lshift", None, "lshift", input, other)


def bitwise_right_shift(input, other):
    return _bitwise("rshift", None, "rshift", input, other)


def bitwise_not(input):
    if input.dtype is _bool_dtype:
        return logical_not(input)
    if input.dtype.is_floating_point:
        raise NotImplementedError("\"bitwise_not_cpu\" not implemented for '%s'" % _CAST_NAME[input.dtype.name])
    return _unary_nograd("bitnot", input)


def isfinite(input):
    return _unary_nograd("isfinite", input)


def isnan(input):
    return _unary_nograd("isnan", input)


def isinf(input):
    return _unary_nograd("isinf", input)


def isposinf(input):
    return _unary_nograd("isposinf", input)


def isneginf(input):
    return _unary_nograd("isneginf", input)


def isreal(input):
    if input.dtype.is_complex:
        return eq(imag(input), 0)
    return ones_like(input, dtype=_bool_dtype)


def signbit(input):
    return _unary_nograd("signbit", input)


def nan_to_num(input, nan=0.0, posinf=None, neginf=None):
    a = input
    if not a.dtype.is_floating_point:
        return a.clone()
    info = finfo(a.dtype)
    out = where(isnan(a), _float(nan), a)
    out = where(isposinf(out), info.max if posinf is None else _float(posinf), out)
    return where(isneginf(out), info.min if neginf is None else _float(neginf), out)


def _float_op(a, *others):
    """Whether a float16/bfloat16 op on `a` (with tensors of its dtype or
    Python numbers) should compute in float32 and round once, as PyTorch's
    fused kernels (lerp, addcmul, addcdiv) do in their float `opmath`."""
    if not (_isinstance(a, Tensor) and a.dtype._reduced):
        return False
    for o in others:
        if _isinstance(o, Tensor) and o.dtype is not a.dtype:
            return False
    return True


def _f32(v):
    return _cast(v, float32) if _isinstance(v, Tensor) else v


def lerp(input, end, weight):
    if _float_op(input, end, weight):
        return _cast(lerp(_f32(input), _f32(end), _f32(weight)), input.dtype)
    # PyTorch's two-branch form: start + w * (end - start) for |w| < 0.5,
    # end - (end - start) * (1 - w) otherwise, so both ends are exact.
    diff = sub(end, input)
    if _isinstance(weight, Tensor):
        return where(abs(weight) < 0.5, add(input, mul(weight, diff)), sub(end, mul(diff, sub(1, weight))))
    if _b.abs(weight) < 0.5:
        return add(input, mul(diff, weight))
    return sub(end, mul(diff, 1 - weight))


def addcmul(input, tensor1, tensor2, value=1):
    if _float_op(input, tensor1, tensor2):
        return _cast(addcmul(_f32(input), _f32(tensor1), _f32(tensor2), value), input.dtype)
    return add(input, mul(mul(tensor1, value) if value != 1 else tensor1, tensor2))


def addcdiv(input, tensor1, tensor2, value=1):
    if _float_op(input, tensor1, tensor2):
        return _cast(addcdiv(_f32(input), _f32(tensor1), _f32(tensor2), value), input.dtype)
    return add(input, div(mul(tensor1, value) if value != 1 else tensor1, tensor2))


def where(condition, input=None, other=None):
    if input is None and other is None:
        return nonzero(condition, as_tuple=True)
    if condition.dtype is not _bool_dtype:
        condition = condition.to(_bool_dtype)
    ta, tb, dt = _operands(input, other)
    storage, shape = _k.where(condition._s, condition.shape, ta._s, ta.shape, tb._s, tb.shape, None if dt is None else dt.name)
    out = _new3(storage, shape, _DTYPES[_k.dtype(storage)])
    if _needs_grad(ta, tb):
        out.requires_grad = True
        out._node = _Node(lambda g: (_unbroadcast(where(condition, g, 0.0), ta.shape) if ta.requires_grad else None, _unbroadcast(where(condition, 0.0, g), tb.shape) if tb.requires_grad else None), (ta, tb), "Where")
    return out


def _cast(a, dt):
    if a.__class__ is _SparseTensor:
        return _sp._to(a, dt)
    if a.dtype.is_complex and not dt.is_complex and dt is not _bool_dtype:
        _warn_complex_cast()
    out = Tensor(_k.astype(a._s, dt.name), a.shape, dt)
    if _grad_enabled and a.requires_grad and dt._inexact and a.dtype._inexact:
        out.requires_grad = True
        out._node = _Node(lambda g: (_grad_as(g, a.dtype),), (a,), "ToCopy")
    return out


def _grad_as(g, dt):
    """A gradient converted to its input's dtype: a real input of a complex
    result takes the real part (PyTorch's handle_r_to_c)."""
    if g.dtype.is_complex and not dt.is_complex:
        g = real(g)
        if g.dtype is dt:
            return g
    return _cast(g, dt)


def _replaced(src, values):
    """`values` (no history) standing in for `src`: backward gives the old
    values a zero gradient, as PyTorch's fill/random in-place ops do."""
    out = Tensor(values._s, src.shape, src.dtype) if values.dtype is src.dtype else Tensor(_k.astype(values._s, src.dtype.name), src.shape, src.dtype)
    if _grad_enabled and src.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (zeros_like(g),), (src,), "Fill")
    return out


def _filled(src, value):
    v = value.item() if _isinstance(value, Tensor) else value
    out = Tensor(_k.full(src.dtype.name, _numel(src.shape), v), src.shape, src.dtype)
    vt = value if _isinstance(value, Tensor) else None
    if _needs_grad(src, vt):
        out.requires_grad = True
        out._node = _Node(lambda g: (zeros_like(g), sum(g).reshape(vt.shape).to(vt.dtype) if vt is not None else None), (src, vt), "Fill")
    return out


def _zeroed(src):
    out = Tensor(_k.zeros(src.dtype.name, _numel(src.shape)), src.shape, src.dtype)
    if _grad_enabled and src.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (zeros_like(g),), (src,), "Zero")
    return out


def _copied(dst, src):
    e = src if src.shape == dst.shape else expand(src, *dst.shape)
    if e.dtype is not dst.dtype:
        e = _cast(e, dst.dtype)
    out = Tensor(e._s, dst.shape, dst.dtype)
    if _needs_grad(dst, e):
        out.requires_grad = True
        out._node = _Node(lambda g: (zeros_like(g), g), (dst, e), "Copy")
    return out


# ---- reductions --------------------------------------------------------------------------
def _dims_arg(dim, rank):
    """Normalised reduction dims (None: every dim). A 0-d tensor accepts
    dim 0 or -1, which reduces nothing, as in PyTorch."""
    if dim is None:
        return None
    if _isinstance(dim, (_list, _tuple, Size)):
        if rank == 0:
            for d in dim:
                _norm_dim(_int(d), 1)
            return []
        return [_norm_dim(_int(d), rank) for d in dim]
    if rank == 0:
        _norm_dim(_int(dim), 1)
        return []
    return [_norm_dim(_int(dim), rank)]


def _all_if_empty(dim):
    # sum/mean/amax take dim=() (or []) as every dim, as PyTorch still does.
    return None if _isinstance(dim, (_list, _tuple)) and not dim else dim


def _reduce_nograd(op, a, dim, keepdim):
    dims = _dims_arg(dim, _len(a.shape))
    storage, shape = _k.reduce(op, a._s, a.shape, dims, keepdim, True)
    return _new3(storage, shape, _DTYPES[_k.dtype(storage)])


def _expand_back(g, a_shape, dims, keepdim):
    """Broadcast a reduced gradient back over the reduced dims."""
    if dims is None:
        if not _grad_enabled and _numel(a_shape) != 1 and g.__class__ is Tensor:
            # The one value, broadcast: the kernel call the reshape and
            # expand below make, without the intermediate view. (A 1-element
            # target keeps that path, whose result shares g's storage.)
            return Tensor(_k.expand(g._s, g.shape, a_shape), a_shape, g.dtype)
        return expand(g.reshape(*([1] * _len(a_shape))), *a_shape)
    if not keepdim:
        for d in sorted(dims):
            g = g.unsqueeze(d)
    return expand(g, *a_shape)


def _check_nonempty(a, dims, name):
    if dims is None:
        if _numel(a.shape) == 0:
            raise RuntimeError("%s(): Expected reduction dim to be specified for input.numel() == 0. Specify the reduction dim with the 'dim' argument." % name)
        return
    for d in dims:
        if a.shape[d] == 0:
            raise IndexError("%s(): Expected reduction dim %d to have non-zero size." % (name, d))


def sum(input, dim=None, keepdim=False, dtype=None):
    a = input
    if _graph_recording and hasattr(a, "_zipp_graph"):
        return a.sum(dim, keepdim, dtype)
    if a.__class__ is _SparseTensor:
        return _sp._tensor_sum(a, dim, keepdim, dtype)
    if dtype is not None:
        a = a.to(dtype)
    elif not a.dtype._inexact and a.dtype is not int64:
        # `(pred == target).sum()` counts; a uint8 sum does not wrap.
        a = a.to(int64)
    try:
        dims = None if dim is None else _dims_arg(_all_if_empty(dim), _len(a.shape))
        storage, shape = _k.reduce("sum", a._s, a.shape, dims, keepdim, True)
    except RuntimeError as e:
        _lazy_reraise(e, "sum", a)
    out = _new3(storage, shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (_expand_back(g, a.shape, dims, keepdim),), (a,), "Sum")
    return out


def nansum(input, dim=None, keepdim=False, dtype=None):
    a = input
    if a.dtype.is_floating_point:
        a = where(isnan(a), 0.0, a)
    return sum(a, dim, keepdim, dtype=dtype)


def mean(input, dim=None, keepdim=False, dtype=None):
    a = input
    if _graph_recording and hasattr(a, "_zipp_graph"):
        return a.mean(dim, keepdim, dtype)
    if dtype is not None:
        a = a.to(dtype)
    if not a.dtype._inexact:
        raise RuntimeError("mean(): could not infer output dtype. Input dtype must be either a floating point or complex dtype. Got: %s" % _CAST_NAME[a.dtype.name])
    try:
        dims = None if dim is None else _dims_arg(_all_if_empty(dim), _len(a.shape))
        storage, shape = _k.reduce("mean", a._s, a.shape, dims, keepdim, True)
    except RuntimeError as e:
        _lazy_reraise(e, "mean", a)
    out = _new3(storage, shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        count = _numel(a.shape) / _b.max(1, _numel(shape))
        out.requires_grad = True
        out._node = _Node(lambda g: (div(_expand_back(g, a.shape, dims, keepdim), count),), (a,), "Mean")
    return out


def nanmean(input, dim=None, keepdim=False, dtype=None):
    a = input if dtype is None else input.to(dtype)
    keep = logical_not(isnan(a))
    return div(sum(where(keep, a, 0.0), dim, keepdim), sum(keep, dim, keepdim))


class _ReturnTypes(_tuple):
    """PyTorch's torch.return_types: a tuple with named fields."""
    _fields = ("values", "indices")
    _name = "torch.return_types.result"

    def __new__(cls, items, name=None, fields=None):
        out = _tuple.__new__(cls, items)
        if name is not None:
            out._name = "torch.return_types." + name
        if fields is not None:
            out._fields = fields
        return out

    def __getattr__(self, key):
        fields = self._fields
        if key in fields:
            return self[fields.index(key)]
        raise AttributeError("'%s' object has no attribute '%s'" % (self._name, key))

    def __repr__(self):
        fields = self._fields
        return "%s(\n%s)" % (self._name, ",\n".join("%s=%r" % (f, v) for f, v in zip(fields, self)))


def _returns(name, values, indices):
    return _ReturnTypes((values, indices), name)


def max(input, dim=None, keepdim=False, out=None):
    a = input
    if _isinstance(dim, Tensor):
        return maximum(a, dim)
    if dim is None:
        _check_nonempty(a, None, "max")
        out = _reduce_nograd("max", a, None, False)
        if _grad_enabled and a.requires_grad:
            out.requires_grad = True
            out._node = _Node(_ties_backward(a, out), (a,), "Max", (a, out))
        return out
    return _returns("max", *_arg_reduce(a, dim, keepdim, "max", "argmax"))


def min(input, dim=None, keepdim=False, out=None):
    a = input
    if _isinstance(dim, Tensor):
        return minimum(a, dim)
    if dim is None:
        _check_nonempty(a, None, "min")
        out = _reduce_nograd("min", a, None, False)
        if _grad_enabled and a.requires_grad:
            out.requires_grad = True
            out._node = _Node(_ties_backward(a, out), (a,), "Min", (a, out))
        return out
    return _returns("min", *_arg_reduce(a, dim, keepdim, "min", "argmin"))


def _arg_reduce(a, dim, keepdim, op, argop):
    dims = _dims_arg(dim, _len(a.shape))
    _check_nonempty(a, dims, op)
    values = _reduce_nograd(op, a, dim, keepdim)
    indices = _reduce_nograd(argop, a, dim, keepdim)
    if _grad_enabled and a.requires_grad:
        values.requires_grad = True
        if not dims:
            values._node = _Node(lambda g: (g,), (a,), op.capitalize())
        else:
            d = dims[0]
            values._node = _Node(lambda g: (_scatter_grad(a, d, indices, g, keepdim),), (a,), op.capitalize())
    return values, indices


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
    return scatter(zeros_like(a, dtype=g.dtype), d, indices, g)


def amax(input, dim=(), keepdim=False):
    return _amaxmin(input, dim, keepdim, "max")


def amin(input, dim=(), keepdim=False):
    return _amaxmin(input, dim, keepdim, "min")


def _amaxmin(a, dim, keepdim, op):
    dims = _dims_arg(_all_if_empty(dim), _len(a.shape))
    _check_nonempty(a, dims, "a" + op)
    out = _reduce_nograd(op, a, dims, keepdim)
    if _grad_enabled and a.requires_grad:
        # Tied extremes share the gradient evenly, as PyTorch's amax/amin.
        def backward(g):
            o = _expand_back(out, a.shape, dims, keepdim)
            mask = (a == o).to(g.dtype)
            count = _expand_back(sum(mask, dims, keepdim), a.shape, dims, keepdim)
            return (div(mul(_expand_back(g, a.shape, dims, keepdim), mask), count),)
        out.requires_grad = True
        out._node = _Node(backward, (a,), "A" + op, (a,))
    return out


def aminmax(input, dim=None, keepdim=False):
    lo = amin(input, () if dim is None else dim, keepdim)
    hi = amax(input, () if dim is None else dim, keepdim)
    return _ReturnTypes((lo, hi), "aminmax", ("min", "max"))


def argmax(input, dim=None, keepdim=False):
    if dim is None and keepdim:
        return _reduce_nograd("argmax", input.reshape(-1), 0, False).reshape(*([1] * _len(input.shape)))
    _check_nonempty(input, _dims_arg(dim, _len(input.shape)), "argmax")
    return _reduce_nograd("argmax", input, dim, keepdim)


def argmin(input, dim=None, keepdim=False):
    if dim is None and keepdim:
        return _reduce_nograd("argmin", input.reshape(-1), 0, False).reshape(*([1] * _len(input.shape)))
    _check_nonempty(input, _dims_arg(dim, _len(input.shape)), "argmin")
    return _reduce_nograd("argmin", input, dim, keepdim)


def all(input, dim=None, keepdim=False):
    return _reduce_nograd("all", input, dim, keepdim)


def any(input, dim=None, keepdim=False):
    return _reduce_nograd("any", input, dim, keepdim)


def _int64_acc(a):
    # bool and integer reductions accumulate in int64, as in PyTorch.
    return a if a.dtype._inexact or a.dtype is int64 else a.to(int64)


def prod(input, dim=None, keepdim=False, dtype=None):
    if _autocast_cpu is not None:
        return _autocast_run(prod, "fp32", (input, dim, keepdim, dtype))
    a = input if dtype is None else input.to(dtype)
    out = _reduce_nograd("prod", _int64_acc(a), dim, keepdim)
    if _grad_enabled and a.requires_grad:
        rank = _len(a.shape)
        dims = _dims_arg(dim, rank)
        sa = a._s

        def backward(g):
            if rank == 0 or dims == []:
                return (g,)
            # d(prod)/dx_i is the product of the other elements: prod / x_i
            # without zeros; at a sole zero, the product of the rest; with
            # two or more zeros, 0.
            x = _frozen(a, sa)
            zero = x == 0
            safe = where(zero, ones_like(x), x)
            kept = dims if dims is not None else _list(_range(rank))
            p = _reduce_nograd("prod", safe, kept, True)
            zeros_in = _reduce_nograd("sum", zero.to(x.dtype), kept, True)
            others = where(zero, where(zeros_in == 1, p, zeros_like(p)), where(zeros_in == 0, div(p, safe), zeros_like(x)))
            return (mul(_expand_back(g, a.shape, dims, keepdim), _cj(others)),)
        out.requires_grad = True
        out._node = _Node(backward, (a,), "Prod", (a,))
    return out


def count_nonzero(input, dim=None):
    return sum(_binary_nograd("ne", input, 0), dim)


def _correction(dim, unbiased, correction):
    if _isinstance(dim, _b.bool):
        # var(input, unbiased): the first positional argument is `unbiased`.
        dim, unbiased = None, dim
    if correction is None:
        correction = 0 if unbiased is False else 1
    return dim, correction


def var(input, dim=None, unbiased=None, keepdim=False, correction=None):
    """PyTorch's var(input, dim, unbiased, keepdim) (or var(input, unbiased),
    or `correction=` for the degrees of freedom removed)."""
    a = input
    dim, correction = _correction(dim, unbiased, correction)
    dims = _dims_arg(_all_if_empty(dim), _len(a.shape))
    # float32 is computed in float64 and rounded once, as PyTorch's
    # double-precision accumulation gives.
    x = a.double() if a.dtype is float32 or a.dtype._reduced else a
    m = mean(x, dims, True)
    sq = square(sub(x, m))
    n = _numel(a.shape) / _b.max(1, _numel(sum(sq, dims, True).shape))
    # No degrees of freedom left (n - correction <= 0) gives nan/inf, as in PyTorch.
    out = div(sum(sq, dims, keepdim), _float(_b.max(n - correction, 0)))
    return out if out.dtype is a.dtype else out.to(a.dtype)


def std(input, dim=None, unbiased=None, keepdim=False, correction=None):
    return _std_from_var(var(input, dim, unbiased, keepdim, correction=correction))


def _std_from_var(v):
    # sqrt with PyTorch's std backward: zero where the std is zero (not NaN).
    return _unary("sqrt", v, "Std", lambda g, x, o: where(o == 0, 0.0, div(g, mul(o, 2))), saves="o")


def var_mean(input, dim=None, unbiased=None, keepdim=False, correction=None):
    dim2, corr = _correction(dim, unbiased, correction)
    return (var(input, dim2, None, keepdim, correction=corr), mean(input, _all_if_empty(dim2), keepdim))


def std_mean(input, dim=None, unbiased=None, keepdim=False, correction=None):
    dim2, corr = _correction(dim, unbiased, correction)
    return (std(input, dim2, None, keepdim, correction=corr), mean(input, _all_if_empty(dim2), keepdim))


def _norm2(a, dims, keepdim):
    """The 2-norm with PyTorch's backward: g * x / norm, zero where the norm is zero."""
    with no_grad():
        out = sqrt(sum(square(a), dims, keepdim))
    if _grad_enabled and a.requires_grad:
        def backward(g):
            o = _expand_back(out, a.shape, dims, keepdim)
            return (where(o == 0, 0.0, div(mul(_expand_back(g, a.shape, dims, keepdim), a), o)),)
        out.requires_grad = True
        out._node = _Node(backward, (a,), "LinalgVectorNorm", (a, out))
    return out


def _pnorm(a, p, dims, keepdim):
    """The general p-norm; backward g * x |x|^(p-2) / norm^(p-1), zero where the norm is zero."""
    with no_grad():
        out = pow(sum(pow(abs(a), p), dims, keepdim), 1.0 / p)
    if _grad_enabled and a.requires_grad:
        def backward(g):
            o = _expand_back(out, a.shape, dims, keepdim)
            grad = div(mul(mul(_expand_back(g, a.shape, dims, keepdim), a), pow(abs(a), p - 2)), pow(o, p - 1))
            return (where(o == 0, 0.0, grad),)
        out.requires_grad = True
        out._node = _Node(backward, (a,), "LinalgVectorNorm", (a, out))
    return out


def norm(input, p="fro", dim=None, keepdim=False, out=None, dtype=None):
    a = input if dtype is None else input.to(dtype)
    if a.dtype.is_complex:
        # A complex tensor's norms are its moduli's.
        a = abs(a)
    if not a.dtype.is_floating_point:
        raise RuntimeError("linalg.vector_norm: Expected a floating point or complex tensor as input. Got %s" % _CAST_NAME[a.dtype.name])
    dims = _dims_arg(dim, _len(a.shape))
    if p is None or p == "fro" or p == 2:
        return _norm2(a, dims, keepdim)
    if p == "nuc":
        return linalg.matrix_norm(a, "nuc", dims if dims is not None else (-2, -1), keepdim)
    p = _float(p)
    if p == 1:
        return sum(abs(a), dims, keepdim)
    if p == _math.inf:
        return amax(abs(a), () if dims is None else dims, keepdim)
    if p == -_math.inf:
        return amin(abs(a), () if dims is None else dims, keepdim)
    if p == 0:
        return sum((a != 0).to(a.dtype), dims, keepdim)
    return _pnorm(a, p, dims, keepdim)


def logsumexp(input, dim, keepdim=False):
    a = input if input.dtype.is_floating_point else input.to(_default_dtype)
    dims = _dims_arg(dim, _len(a.shape))
    m = amax(a.detach(), dims, True)
    m = where(isinf(m), 0.0, m)
    out = add(log(sum(exp(sub(a, m)), dims, True)), m)
    return out if keepdim or not dims else squeeze(out, _tuple(dims))


def _moved_last(a, dim):
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    return (a if _len(a.shape) == 0 or d == _len(a.shape) - 1 else movedim(a, d, -1)), d


def _select_along(a, dim, index, keepdim, name):
    """values = a gathered at `index` (keepdim-shaped) along dim, as a return type."""
    values = gather(a, dim, index)
    if not keepdim:
        values, index = values.squeeze(dim), index.squeeze(dim)
    return _returns(name, values, index)


def median(input, dim=None, keepdim=False):
    a = input
    if dim is None:
        flat = a.reshape(-1)
        _check_nonempty(flat, None, "median")
        idx = _median_index(flat, 0, False)
        with no_grad():
            value = flat[idx.item()]
        if _grad_enabled and a.requires_grad:
            # The whole-tensor median shares the gradient among equal values.
            def backward(g):
                mask = (a == value).to(g.dtype)
                return (mul(div(mask, sum(mask)), g),)
            value.requires_grad = True
            value._node = _Node(backward, (a,), "Median", (a,))
        return value
    if _len(a.shape) == 0:
        return _returns("median", a, zeros((), dtype=int64))
    d = _norm_dim(dim, _len(a.shape))
    _check_nonempty(a, [d], "median")
    return _select_along(a, d, _median_index(a, d, True), keepdim, "median")


def _median_index(a, d, keepdim):
    # The lower median of a stable sort (NaN sorts last); a slice holding
    # NaN answers its first NaN, as PyTorch propagates NaN.
    n = a.shape[d]
    order = argsort(a, d)
    idx = order.narrow(d, (n - 1) // 2, 1)
    if a.dtype.is_floating_point:
        nan = isnan(a)
        has_nan = nan.any(d, True)
        if has_nan.any().item():
            idx = where(has_nan, argmax(nan.to(uint8), d, True), idx)
    return idx if keepdim else idx.squeeze(d)


def nanmedian(input, dim=None, keepdim=False):
    a = input
    if dim is None:
        flat = a.reshape(-1)
        keep = flat[logical_not(isnan(flat))]
        return median(keep) if keep.numel() else full((), _math.nan, dtype=a.dtype)
    d = _norm_dim(dim, _len(a.shape))
    order = argsort(a, d)
    count = sum(logical_not(isnan(a)), d, True)
    k = floor_divide(clamp(sub(count, 1), 0), 2)
    return _select_along(a, d, gather(order, d, k), keepdim, "nanmedian")


def kthvalue(input, k, dim=-1, keepdim=False):
    a = input
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    if _len(a.shape) == 0:
        return _returns("kthvalue", a, zeros((), dtype=int64))
    if not 1 <= k <= a.shape[d]:
        raise IndexError("kthvalue(): selected number k out of range for dimension %d" % d)
    idx = argsort(a, d).narrow(d, k - 1, 1)
    return _select_along(a, d, idx, keepdim, "kthvalue")


def mode(input, dim=-1, keepdim=False):
    """The most frequent value along dim (the smallest among equally
    frequent ones) and the index of its last occurrence, as PyTorch."""
    a = input
    if _len(a.shape) == 0:
        return _returns("mode", a, zeros((), dtype=int64))
    x, d = _moved_last(a, dim)
    n = x.shape[-1]
    rows = _k.to_list(x.reshape(-1, n)._s) if n else []
    vals, idxs = [], []
    for r in _range(_numel(x.shape) // n if n else 0):
        row = rows[r * n:(r + 1) * n]
        counts, last = {}, {}
        for i, v in enumerate(row):
            counts[v] = counts.get(v, 0) + 1
            last[v] = i
        best = None
        for v in counts:
            if best is None or counts[v] > counts[best] or (counts[v] == counts[best] and v < best):
                best = v
        vals.append(best)
        idxs.append(last[best])
    shape = _tuple(x.shape[:-1])
    index = Tensor(_k.from_flat("int64", idxs), shape, int64)
    index = index.unsqueeze(-1)
    if d != _len(a.shape) - 1:
        index = movedim(index, -1, d)
    return _select_along(a, d, index, keepdim, "mode")


def quantile(input, q, dim=None, keepdim=False, interpolation="linear"):
    if _autocast_cpu is not None:
        return _autocast_run(quantile, "fp32", (input, q, dim, keepdim, interpolation))
    a = input
    if dim is None:
        a, d = a.reshape(-1), 0
    else:
        d = _norm_dim(dim, _len(a.shape))
    s = sort(a, d)[0]
    n = a.shape[d]
    qs = [_float(q)] if not _isinstance(q, Tensor) else [_float(v) for v in _k.to_list(q._s)]
    outs = []
    for qq in qs:
        if not 0 <= qq <= 1:
            raise RuntimeError("quantile() q values must be in the range [0, 1]")
        pos = qq * (n - 1)
        lo, hi = _int(_math.floor(pos)), _int(_math.ceil(pos))
        vlo, vhi = s.narrow(d, lo, 1), s.narrow(d, hi, 1)
        frac_ = pos - lo
        if interpolation == "linear":
            v = lerp(vlo, vhi, frac_) if hi != lo else vlo
        elif interpolation == "lower":
            v = vlo
        elif interpolation == "higher":
            v = vhi
        elif interpolation == "nearest":
            v = s.narrow(d, _int(_b.round(pos)), 1)
        elif interpolation == "midpoint":
            v = div(add(vlo, vhi), 2)
        else:
            raise ValueError("quantile() interpolation must be one of linear, lower, higher, midpoint or nearest. Got %s" % interpolation)
        if not keepdim:
            v = v.squeeze(d)
        elif dim is None:
            v = v.reshape(*([1] * _len(input.shape)))
        outs.append(v)
    return outs[0] if not _isinstance(q, Tensor) or not q.shape else stack(outs, 0)


def nanquantile(input, q, dim=None, keepdim=False, interpolation="linear"):
    if _autocast_cpu is not None:
        return _autocast_run(nanquantile, "fp32", (input, q, dim, keepdim, interpolation))
    return quantile(input, q, dim, keepdim, interpolation)


def _scan(op, a, d):
    storage = _k.scan(op, a._s, a.shape, d)
    return storage


def _rev_cumsum(g, d):
    return flip(cumsum(flip(g, d), d), d)


def cumsum(input, dim, dtype=None):
    a = input.to(dtype) if dtype is not None else _int64_acc(input)
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    out = Tensor(_k.scan("cumsum", a._s, a.shape, d), a.shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (g if not a.shape else _rev_cumsum(g, d),), (a,), "Cumsum")
    return out


def cumprod(input, dim, dtype=None):
    a = input.to(dtype) if dtype is not None else _int64_acc(input)
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    out = Tensor(_k.scan("cumprod", a._s, a.shape, d), a.shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        def backward(g):
            if not a.shape:
                return (g,)
            if not (a == 0).any().item():
                return (div(_rev_cumsum(mul(g, _cj(out)), d), _cj(a)),)
            if a.dtype.is_complex:
                raise NotImplementedError("the derivative of cumprod over a complex tensor with zeros is not implemented on Zipp")
            # With zeros: grad_i = sum over j >= i of g_j * prod(x_k, k <= j, k != i).
            x, dd = _moved_last(a, d)
            gm, _ = _moved_last(g, d)
            n = x.shape[-1]
            xs, gs = _k.to_list(x.reshape(-1, n)._s), _k.to_list(gm.reshape(-1, n)._s)
            res = []
            for r in _range(_len(xs) // n if n else 0):
                row, grow = xs[r * n:(r + 1) * n], gs[r * n:(r + 1) * n]
                for i in _range(n):
                    total, p = 0.0, 1.0
                    for j in _range(n):
                        if j != i:
                            p *= row[j]
                        if j >= i:
                            total += grow[j] * p
                    res.append(total)
            out_g = Tensor(_k.from_flat(g.dtype.name, res), x.shape, g.dtype)
            return (out_g if d == _len(a.shape) - 1 else movedim(out_g, -1, d),)
        out.requires_grad = True
        out._node = _Node(backward, (a,), "Cumprod", (a, out))
    return out


def _cum_extreme(a, dim, op):
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    storage, idx = _k.scan(op, a._s, a.shape, d)
    values = Tensor(storage, a.shape, a.dtype)
    indices = Tensor(idx, a.shape, int64)
    if _grad_enabled and a.requires_grad:
        values.requires_grad = True
        values._node = _Node(lambda g: (g if not a.shape else scatter_add(zeros_like(a, dtype=g.dtype), d, indices, g),), (a,), op.capitalize())
    return _returns(op, values, indices)


def cummax(input, dim):
    return _cum_extreme(input, dim, "cummax")


def cummin(input, dim):
    return _cum_extreme(input, dim, "cummin")


def logcumsumexp(input, dim):
    a = input
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    out = Tensor(_k.scan("logcumsumexp", a._s, a.shape, d), a.shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        def backward(g):
            # grad_i = sum over j >= i of g_j * exp(x_i - out_j).
            return (mul(_rev_cumsum(mul(g, exp(neg(out))), d), exp(a)),)
        out.requires_grad = True
        out._node = _Node(backward, (a,), "Logcumsumexp", (a, out))
    return out


def diff(input, n=1, dim=-1, prepend=None, append=None):
    a = input
    d = _norm_dim(dim, _len(a.shape))
    if prepend is not None or append is not None:
        parts = ([prepend] if prepend is not None else []) + [a] + ([append] if append is not None else [])
        a = cat([p if p.shape else p.expand(*(_list(a.shape[:d]) + [1] + _list(a.shape[d + 1:]))) for p in parts], d)
    for _ in _range(n):
        m = a.shape[d]
        if m == 0:
            break
        a = (a.narrow(d, 1, m - 1) != a.narrow(d, 0, m - 1)) if a.dtype is _bool_dtype else sub(a.narrow(d, 1, m - 1), a.narrow(d, 0, m - 1))
    return a


def unique(input, sorted=True, return_inverse=False, return_counts=False, dim=None):
    """unique values (sorted; PyTorch sorts on CPU either way), with the
    inverse indices and counts on request. `dim` compares whole slices."""
    a = input
    if dim is None:
        keys = _k.to_list(a._s)
        shape = a.shape
        make = lambda vals: Tensor(_k.from_flat(a.dtype.name, vals), (_len(vals),), a.dtype)
    else:
        d = _norm_dim(dim, _len(a.shape))
        moved = movedim(a, d, 0) if d else a
        n = moved.shape[0]
        flat = _k.to_list(moved._s)
        step = _numel(moved.shape[1:])
        keys = [_tuple(flat[i * step:(i + 1) * step]) for i in _range(n)]
        shape = (n,)

        def make(vals):
            t = Tensor(_k.from_flat(a.dtype.name, [v for row in vals for v in row]), (_len(vals),) + _tuple(moved.shape[1:]), a.dtype)
            return movedim(t, 0, d) if d else t
    distinct = _b.sorted(set(keys))
    pos = {k: i for i, k in enumerate(distinct)}
    out = make(distinct)
    extra = []
    if return_inverse:
        extra.append(Tensor(_k.from_flat("int64", [pos[k] for k in keys]), shape, int64))
    if return_counts:
        counts = [0] * _len(distinct)
        for k in keys:
            counts[pos[k]] += 1
        extra.append(Tensor(_k.from_flat("int64", counts), (_len(distinct),), int64))
    return (out,) + _tuple(extra) if extra else out


def unique_consecutive(input, return_inverse=False, return_counts=False, dim=None):
    a = input
    if dim is not None:
        raise NotImplementedError("unique_consecutive(dim=...) is not supported on Zipp")
    keys = _k.to_list(a._s)
    vals, inverse, counts = [], [], []
    for k in keys:
        if not vals or vals[-1] != k:
            vals.append(k)
            counts.append(0)
        counts[-1] += 1
        inverse.append(_len(vals) - 1)
    out = Tensor(_k.from_flat(a.dtype.name, vals), (_len(vals),), a.dtype)
    extra = []
    if return_inverse:
        extra.append(Tensor(_k.from_flat("int64", inverse), a.shape, int64))
    if return_counts:
        extra.append(Tensor(_k.from_flat("int64", counts), (_len(counts),), int64))
    return (out,) + _tuple(extra) if extra else out


def bincount(input, weights=None, minlength=0):
    if _len(input.shape) != 1 or input.dtype.is_floating_point:
        raise RuntimeError("bincount only supports 1-d non-negative integral inputs.")
    idx = [_int(v) for v in _k.to_list(input._s)]
    if idx and _b.min(idx) < 0:
        raise RuntimeError("bincount only supports 1-d non-negative integral inputs.")
    n = _b.max(_b.max(idx) + 1 if idx else 0, _int(minlength))
    if weights is None:
        counts = [0] * n
        for i in idx:
            counts[i] += 1
        return Tensor(_k.from_flat("int64", counts), (n,), int64)
    w = _k.to_list(weights._s)
    totals = [0.0] * n
    for i, v in zip(idx, w):
        totals[i] += v
    dt = weights.dtype if weights.dtype.is_floating_point else float64
    return Tensor(_k.from_flat(dt.name, totals), (n,), dt)


def histc(input, bins=100, min=0, max=0):
    vals = _k.to_list(input._s)
    lo, hi = _float(min), _float(max)
    if lo == hi and vals:
        lo, hi = _b.min(vals), _b.max(vals)
    if lo == hi:
        lo, hi = lo - 1, hi + 1
    counts = [0.0] * bins
    for v in vals:
        if lo <= v <= hi:
            counts[_b.min(_int((v - lo) * bins / (hi - lo)), bins - 1)] += 1
    return Tensor(_k.from_flat(input.dtype.name, counts), (bins,), input.dtype)


def argsort(input, dim=-1, descending=False, stable=False):
    a = input
    if _len(a.shape) == 0:
        return zeros((), dtype=int64)
    d = _norm_dim(dim, _len(a.shape))
    return Tensor(_k.argsort(a._s, a.shape, d, _bool(descending)), a.shape, int64)


def sort(input, dim=-1, descending=False, stable=False):
    """Values and indices (a stable sort; NaN sorts as the largest value)."""
    a = input
    if _len(a.shape) == 0:
        return _returns("sort", a, zeros((), dtype=int64))
    idx = argsort(a, dim, descending)
    return _returns("sort", gather(a, dim, idx), idx)


def msort(input):
    return sort(input, 0)[0]


def topk(input, k, dim=-1, largest=True, sorted=True):
    a = input
    if _len(a.shape) == 0:
        return _returns("topk", a, zeros((), dtype=int64))
    d = _norm_dim(dim, _len(a.shape))
    if not 0 <= k <= a.shape[d]:
        raise RuntimeError("selected index k out of range")
    idx = argsort(a, d, largest).narrow(d, 0, k)
    return _returns("topk", gather(a, d, idx), idx)


# ---- shape ops ---------------------------------------------------------------------------
def reshape(input, *shape):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.reshape(*shape)
    shape = _shape_args(shape)
    n = _numel(a.shape)
    if -1 in shape:
        known = 1
        for d in shape:
            if d != -1:
                known *= d
        if known == 0:
            raise RuntimeError("cannot reshape tensor of %d elements into shape %s because the unspecified dimension size -1 can be any value and is ambiguous" % (n, _list(shape)))
        shape = _tuple(n // known if d == -1 else d for d in shape)
    if _numel(shape) != n:
        raise RuntimeError("shape '%s' is invalid for input of size %d" % (_list(shape), n))
    return _view(a, shape)


def _view(a, shape):
    """`a` reshaped to `shape`, a tuple of ints already checked to hold
    `a`'s element count."""
    out = Tensor(a._s, shape, a.dtype)
    # A view: it shares the storage (so writes show through both ways) and
    # remembers its base for in-place autograd (see Tensor._rebase).
    base = a._base
    out._base = a if base is None else base
    if a._untracked:
        out._untracked = True
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (g.reshape(*a.shape),), (a,), "View")
    return out


def flatten(input, start_dim=0, end_dim=-1):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.flatten(start_dim, end_dim)
    rank = _len(a.shape)
    if rank == 0:
        return reshape(a, 1)
    s, e = _norm_dim(start_dim, rank), _norm_dim(end_dim, rank)
    shape = _tuple(a.shape[:s]) + (_numel(a.shape[s:e + 1]),) + _tuple(a.shape[e + 1:])
    return reshape(a, *shape)


def ravel(input):
    return reshape(input, -1)


def unflatten(input, dim, sizes):
    a = input
    d = _norm_dim(dim, _len(a.shape))
    sizes = _list(sizes)
    if -1 in sizes:
        known = 1
        for s in sizes:
            if s != -1:
                known *= s
        sizes = [a.shape[d] // known if s == -1 else s for s in sizes]
    if _numel(sizes) != a.shape[d]:
        raise RuntimeError("unflatten: Provided sizes %s don't multiply up to the size of dim %d (%d) in the input tensor" % (sizes, dim, a.shape[d]))
    return reshape(a, *(_list(a.shape[:d]) + sizes + _list(a.shape[d + 1:])))


def unsqueeze(input, dim):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.unsqueeze(dim)
    d = _norm_dim(dim, _len(a.shape) + 1)
    shape = _list(a.shape)
    shape.insert(d, 1)
    return _view(a, _tuple(shape))


def squeeze(input, dim=None):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.squeeze() if dim is None else a.squeeze(dim)
    rank = _len(a.shape)
    if dim is None:
        shape = _tuple(d for d in a.shape if d != 1)
    else:
        dims = set(_norm_dim(_int(d), _b.max(rank, 1)) for d in (dim if _isinstance(dim, (_list, _tuple)) else (dim,)))
        shape = _tuple(s for i, s in enumerate(a.shape) if not (i in dims and s == 1))
    if shape == _tuple(a.shape):
        return a
    return reshape(a, *shape)


def permute(input, *dims):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.permute(*dims)
    dims = _shape_args(dims)
    rank = _len(a.shape)
    dims = _tuple(_norm_dim(d, rank) for d in dims)
    if _b.sorted(dims) != _list(_range(rank)):
        raise RuntimeError("permute(): dims must be a permutation of the tensor's dimensions")
    return _permute(a, _list(dims), rank)


def _permute(a, dims, rank):
    """`permute(a, *dims)` for `dims`, a list, already a permutation of
    range(rank)."""
    storage, shape = _k.permute(a._s, a.shape, dims)
    out = _new3(storage, shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        inverse = [0] * rank
        i = 0
        for d in dims:
            inverse[d] = i
            i += 1
        out.requires_grad = True
        out._node = _Node(lambda g: (permute(g, *inverse),), (a,), "Permute")
    return out


def transpose(input, dim0, dim1):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.transpose(dim0, dim1)
    if a.__class__ is _SparseTensor:
        return _sp._transpose(a, dim0, dim1)
    rank = _len(a.shape)
    if rank == 0:
        _norm_dim(dim0, 1)
        _norm_dim(dim1, 1)
        return a
    d0, d1 = _norm_dim(dim0, rank), _norm_dim(dim1, rank)
    dims = _list(_range(rank))
    dims[d0], dims[d1] = dims[d1], dims[d0]
    return _permute(a, dims, rank)


swapaxes = transpose
swapdims = transpose


def t(input):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.t()
    if a.__class__ is _SparseTensor:
        return _sp._t(a)
    if _len(a.shape) > 2:
        raise RuntimeError("t() expects a tensor with <= 2 dimensions, but self is %dD" % _len(a.shape))
    return a if _len(a.shape) < 2 else transpose(a, 0, 1)


def adjoint(input):
    return _cj(transpose(input, -2, -1))


def movedim(input, source, destination):
    a = input
    rank = _len(a.shape)
    src = [source] if _isinstance(source, _int) else _list(source)
    dst = [destination] if _isinstance(destination, _int) else _list(destination)
    if _len(src) != _len(dst):
        raise RuntimeError("movedim: Invalid source or destination dims: source (%s dims) should contain the same number of dims as destination (%s dims)" % (_len(src), _len(dst)))
    src = [_norm_dim(s, rank) for s in src]
    dst = [_norm_dim(d, rank) for d in dst]
    order = [-1] * rank
    for s, d in zip(src, dst):
        order[d] = s
    rest = iter([i for i in _range(rank) if i not in src])
    order = [o if o != -1 else next(rest) for o in order]
    return a if order == _list(_range(rank)) else permute(a, *order)


moveaxis = movedim


def expand(input, *sizes):
    a = input
    sizes = _shape_args(sizes)
    rank = _len(a.shape)
    if _len(sizes) < rank:
        raise RuntimeError("expand: the number of sizes provided (%d) must be greater or equal to the number of dimensions in the tensor (%d)" % (_len(sizes), rank))
    off = _len(sizes) - rank
    if -1 in sizes:
        target = []
        for i, s in enumerate(sizes):
            if s == -1:
                if i < off:
                    raise RuntimeError("expand: -1 is not allowed in a leading, non-existing dimension")
                target.append(a.shape[i - off])
            else:
                target.append(s)
        target = _tuple(target)
    else:
        target = sizes
    if _shape_eq(target, a.shape):
        return a
    out = Tensor(_k.expand(a._s, a.shape, target), target, a.dtype)
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (_unbroadcast(g, a.shape),), (a,), "Expand")
    return out


def broadcast_to(input, size):
    return expand(input, *_shape_args((size,)))


def broadcast_shapes(*shapes):
    out = ()
    for s in shapes:
        out = _broadcast_shapes(out, (s,) if _isinstance(s, _int) else _tuple(s))
    return Size(out)


def broadcast_tensors(*tensors):
    if _len(tensors) == 1 and _isinstance(tensors[0], (_list, _tuple)):
        tensors = _tuple(tensors[0])
    shape = broadcast_shapes(*[t.shape for t in tensors])
    return _tuple(expand(t, *shape) for t in tensors)


def repeat(input, *sizes):
    a = input
    sizes = _shape_args(sizes)
    if _len(sizes) < _len(a.shape):
        raise RuntimeError("Number of dimensions of repeat dims can not be smaller than number of dimensions of tensor")
    off = _len(sizes) - _len(a.shape)
    x = a.reshape(*([1] * off + _list(a.shape)))
    inter = []
    for i, s in enumerate(sizes):
        inter += [s, x.shape[i]]
    y = x.reshape(*[1 if j % 2 == 0 else x.shape[j // 2] for j in _range(2 * _len(sizes))])
    y = expand(y, *inter)
    return y.reshape(*[sizes[i] * x.shape[i] for i in _range(_len(sizes))])


def tile(input, *dims):
    dims = _shape_args(dims)
    rank = _len(input.shape)
    if _len(dims) < rank:
        dims = (1,) * (rank - _len(dims)) + _tuple(dims)
    return repeat(input, *dims)


def narrow(input, dim, start, length):
    a = input
    d = _norm_dim(dim, _len(a.shape))
    start, length = _int(start), _int(length)
    if start < 0:
        start += a.shape[d]
    if start < 0 or length < 0 or start + length > a.shape[d]:
        raise RuntimeError("start (%d) + length (%d) exceeds dimension size (%d)." % (start, length, a.shape[d]))
    if type(a).__getitem__ is _tensor_getitem:
        # What `a[:, ..., start:start + length]` builds: every other dim whole.
        spec = [(None, None, None)] * _len(a.shape)
        spec[d] = (start, start + length, None)
        return _slice(a, spec)
    spec = [slice(None)] * d + [slice(start, start + length)]
    return a[_tuple(spec)]


def cat(tensors, dim=0):
    if _autocast_cpu is not None:
        return _autocast_run(cat, "promote", (tensors, dim))
    tensors = [_as_tensor(t) for t in tensors]
    if not tensors:
        raise RuntimeError("torch.cat(): expected a non-empty list of Tensors")
    if tensors[0].__class__ is _SparseTensor:
        return _sp._cat(tensors, dim)
    # PyTorch skips legacy empty 1-d tensors when the others differ in rank.
    try:
        ranks = set(_len(t.shape) for t in tensors)
    except RuntimeError as e:
        _lazy_reraise(e, "cat", *tensors)
    if _len(ranks) > 1:
        tensors = [t for t in tensors if _tuple(t.shape) != (0,)] or tensors[:1]
    rank = _len(tensors[0].shape)
    if rank == 0:
        raise RuntimeError("zero-dimensional tensor (at position 0) cannot be concatenated")
    d = _norm_dim(dim, rank)
    storage, shape = _k.cat([(t._s, t.shape) for t in tensors], d)
    out = _new3(storage, shape, _DTYPES[_k.dtype(storage)])
    if _needs_grad(*tensors):
        sizes = [t.shape[d] for t in tensors]

        def backward(g):
            grads, start = [], 0
            for t_, s in zip(tensors, sizes):
                grads.append(narrow(g, d, start, s) if t_.requires_grad else None)
                start += s
            return _tuple(grads)
        out.requires_grad = True
        out._node = _Node(backward, _tuple(tensors), "Cat")
    return out


concat = cat
concatenate = cat


def stack(tensors, dim=0):
    if _autocast_cpu is not None:
        return _autocast_run(stack, "promote", (tensors, dim))
    tensors = [t if type(t) is Tensor else _as_tensor(t) for t in tensors]
    if not tensors:
        raise RuntimeError("stack expects a non-empty TensorList")
    if tensors[0].__class__ is _SparseTensor:
        return _sp._stack(tensors, dim)
    first = _tuple(tensors[0].shape)
    for i, t_ in enumerate(tensors):
        if not _shape_eq(t_.shape, first):
            raise RuntimeError("stack expects each tensor to be equal size, but got %s at entry 0 and %s at entry %d" % (_list(first), _list(t_.shape), i))
    d = _norm_dim(dim, _len(first) + 1)
    if not _graph_recording and not (_grad_enabled and _any_requires_grad(tensors)):
        # No history to record: concatenate the unsqueezed shapes directly,
        # the kernel call `cat` makes on the unsqueezed views.
        shape = _list(first)
        shape.insert(d, 1)
        shape = _tuple(shape)
        storage, out_shape = _k.cat([(t_._s, shape) for t_ in tensors], d)
        return Tensor(storage, out_shape, _DTYPES[_k.dtype(storage)])
    return cat([unsqueeze(t_, d) for t_ in tensors], d)


def atleast_1d(*tensors):
    out = [t_ if _len(t_.shape) >= 1 else t_.reshape(1) for t_ in (tensors[0] if _len(tensors) == 1 and _isinstance(tensors[0], (_list, _tuple)) else tensors)]
    return out[0] if _len(out) == 1 else _tuple(out)


def atleast_2d(*tensors):
    out = []
    for t_ in (tensors[0] if _len(tensors) == 1 and _isinstance(tensors[0], (_list, _tuple)) else tensors):
        out.append(t_.reshape(1, 1) if _len(t_.shape) == 0 else (t_.unsqueeze(0) if _len(t_.shape) == 1 else t_))
    return out[0] if _len(out) == 1 else _tuple(out)


def atleast_3d(*tensors):
    out = []
    for t_ in (tensors[0] if _len(tensors) == 1 and _isinstance(tensors[0], (_list, _tuple)) else tensors):
        r = _len(t_.shape)
        out.append(t_.reshape(1, 1, 1) if r == 0 else (t_.reshape(1, t_.shape[0], 1) if r == 1 else (t_.unsqueeze(-1) if r == 2 else t_)))
    return out[0] if _len(out) == 1 else _tuple(out)


def hstack(tensors):
    tensors = atleast_1d(*tensors) if _len(tensors) > 1 else [atleast_1d(tensors[0])]
    return cat(tensors, 0 if _len(tensors[0].shape) == 1 else 1)


def vstack(tensors):
    tensors = atleast_2d(*tensors) if _len(tensors) > 1 else [atleast_2d(tensors[0])]
    return cat(tensors, 0)


row_stack = vstack


def dstack(tensors):
    tensors = atleast_3d(*tensors) if _len(tensors) > 1 else [atleast_3d(tensors[0])]
    return cat(tensors, 2)


def column_stack(tensors):
    cols = [t_.reshape(-1, 1) if _len(t_.shape) <= 1 else t_ for t_ in tensors]
    return cat(cols, 1)


def split(tensor, split_size_or_sections, dim=0):
    a = tensor
    d = _norm_dim(dim, _len(a.shape))
    n = a.shape[d]
    size = split_size_or_sections
    if _isinstance(size, _int):
        if size <= 0 and n:
            raise RuntimeError("split_size can only be 0 if dimension size is 0, but got dimension size of %d" % n)
        sizes = [size] * (n // size) + ([n % size] if n % size else []) if size else [0]
    else:
        sizes = _list(size)
        if _b.sum(sizes) != n:
            raise RuntimeError("split_with_sizes expects split_sizes to sum exactly to %d (input tensor's size at dimension %d), but got split_sizes=%s" % (n, d, sizes))
    out, start = [], 0
    if _isinstance(size, _int) and type(a).__getitem__ is _tensor_getitem:
        # `narrow(a, d, start, s)` for pieces that are in range by
        # construction: the slice spec it builds, every other dim whole.
        rank = _len(a.shape)
        for s in sizes:
            spec = [(None, None, None)] * rank
            spec[d] = (start, start + s, None)
            out.append(_slice(a, spec))
            start += s
        return _tuple(out)
    for s in sizes:
        out.append(narrow(a, d, start, s))
        start += s
    return _tuple(out)


def tensor_split(input, indices_or_sections, dim=0):
    a = input
    d = _norm_dim(dim, _len(a.shape))
    n = a.shape[d]
    if _isinstance(indices_or_sections, Tensor):
        indices_or_sections = indices_or_sections.tolist()
    if _isinstance(indices_or_sections, _int):
        k = indices_or_sections
        if k <= 0:
            raise RuntimeError("number of sections must be larger than 0, got %d" % k)
        base, extra = n // k, n % k
        bounds, start = [], 0
        for i in _range(k):
            size = base + (1 if i < extra else 0)
            bounds.append((start, start + size))
            start += size
    else:
        points = [0] + [_b.min(_b.max(_int(i) if i >= 0 else _int(i) + n, 0), n) for i in indices_or_sections] + [n]
        bounds = [(points[i], _b.max(points[i], points[i + 1])) for i in _range(_len(points) - 1)]
    return _tuple(narrow(a, d, s, e - s) for s, e in bounds)


def hsplit(input, indices_or_sections):
    return tensor_split(input, indices_or_sections, 0 if _len(input.shape) == 1 else 1)


def vsplit(input, indices_or_sections):
    return tensor_split(input, indices_or_sections, 0)


def chunk(input, chunks, dim=0):
    a = input
    d = _norm_dim(dim, _len(a.shape))
    if chunks <= 0:
        raise RuntimeError("chunk expects `chunks` to be greater than 0, got: %d" % chunks)
    n = a.shape[d]
    if n == 0:
        # Every chunk of an empty dim is empty, as PyTorch returns them.
        return _tuple(a.narrow(d, 0, 0) for _ in _range(chunks))
    size = -(-n // chunks)
    return split(a, size, d)


def unbind(input, dim=0):
    a = input
    d = _norm_dim(dim, _len(a.shape))
    return _tuple(a.select(d, i) for i in _range(a.shape[d]))


def roll(input, shifts, dims=None):
    a = input
    if dims is None:
        if _isinstance(shifts, (_list, _tuple)):
            shifts = shifts[0]
        return roll(a.flatten(), shifts, 0).reshape(*a.shape)
    if _isinstance(shifts, (_list, _tuple)):
        out = a
        for s, d in zip(shifts, dims if _isinstance(dims, (_list, _tuple)) else [dims]):
            out = roll(out, s, d)
        return out
    if _isinstance(dims, (_list, _tuple)):
        dims = dims[0]
    d = _norm_dim(dims, _len(a.shape))
    if a.shape[d] == 0:
        return a
    out = Tensor(_k.roll(a._s, a.shape, _int(shifts), d), a.shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (roll(g, -_int(shifts), d),), (a,), "Roll")
    return out


def flip(input, dims):
    a = input
    out = a
    for d in ([dims] if _isinstance(dims, _int) else dims):
        d = _norm_dim(d, _len(a.shape))
        if a.shape[d] > 1:
            out = index_select(out, d, arange(a.shape[d] - 1, -1, -1))
    # Always a new tensor, as PyTorch's flip copies.
    return out if out is not a else a.clone()


def fliplr(input):
    if _len(input.shape) < 2:
        raise RuntimeError("Input must be >= 2-d.")
    return flip(input, [1])


def flipud(input):
    if _len(input.shape) < 1:
        raise RuntimeError("Input must be >= 1-d.")
    return flip(input, [0])


def rot90(input, k=1, dims=(0, 1)):
    a = input
    d0, d1 = _norm_dim(dims[0], _len(a.shape)), _norm_dim(dims[1], _len(a.shape))
    k = k % 4
    if k == 1:
        return transpose(flip(a, [d1]), d0, d1)
    if k == 2:
        return flip(a, [d0, d1])
    if k == 3:
        return flip(transpose(a, d0, d1), [d1])
    return a.clone()


def diagonal(input, offset=0, dim1=0, dim2=1):
    """The diagonal (offset above, or below when negative) of the dim1 x dim2
    planes, appended as the last dim (a copy here, not a view)."""
    a = input
    rank = _len(a.shape)
    d1, d2 = _norm_dim(dim1, rank), _norm_dim(dim2, rank)
    if d1 == d2:
        raise RuntimeError("diagonal dimensions cannot be identical %d, %d" % (dim1, dim2))
    x = movedim(a, [d1, d2], [-2, -1])
    r, c = x.shape[-2], x.shape[-1]
    if offset >= 0:
        n = _b.max(0, _b.min(r, c - offset))
        rows, cols = arange(n), arange(n) + offset
    else:
        n = _b.max(0, _b.min(r + offset, c))
        rows, cols = arange(n) - offset, arange(n)
    return x[(Ellipsis, rows, cols)]


_diagonal = diagonal


def diag(input, diagonal=0):
    a = input
    if _len(a.shape) == 1:
        return diag_embed(a, diagonal)
    if _len(a.shape) == 2:
        return _diagonal(a, diagonal)
    raise RuntimeError("diag(): Supports 1D or 2D tensors. Got %dD" % _len(a.shape))


def diag_embed(input, offset=0, dim1=-2, dim2=-1):
    """Vectors along the last dim placed on a diagonal of new dims: where()
    picks each value, so an inf or nan never leaks off the diagonal."""
    a = input
    n = a.shape[-1]
    m = n + _b.abs(offset)
    if offset:
        pad = zeros(*(_list(a.shape[:-1]) + [_b.abs(offset)]), dtype=a.dtype)
        a = cat([a, pad], -1)
    rows = unsqueeze(arange(m), 1)
    cols = unsqueeze(arange(m), 0)
    mask = (cols - rows) == offset
    values = unsqueeze(a, -1) if offset >= 0 else unsqueeze(a, -2)
    out = where(mask, values, 0)
    rank = _len(out.shape)
    if (_norm_dim(dim1, rank), _norm_dim(dim2, rank)) != (rank - 2, rank - 1):
        out = movedim(out, [-2, -1], [dim1, dim2])
    return out


def diagflat(input, offset=0):
    return diag_embed(input.reshape(-1), offset)


def trace(input):
    if _autocast_cpu is not None:
        return _autocast_run(trace, "fp32", (input,))
    return sum(diagonal(input))


def tril(input, diagonal=0):
    a = input
    r, c = a.shape[-2], a.shape[-1]
    mask = (unsqueeze(arange(c), 0) <= unsqueeze(arange(r), 1) + diagonal)
    return where(mask, a, zeros((), dtype=a.dtype))


def triu(input, diagonal=0):
    a = input
    r, c = a.shape[-2], a.shape[-1]
    mask = (unsqueeze(arange(c), 0) >= unsqueeze(arange(r), 1) + diagonal)
    return where(mask, a, zeros((), dtype=a.dtype))


def tril_indices(row, col, offset=0, dtype=None, device=None, layout=None):
    pairs = [(i, j) for i in _range(row) for j in _range(col) if j - i <= offset]
    return tensor([[p[0] for p in pairs], [p[1] for p in pairs]], dtype=_dtype_of(dtype) or int64).reshape(2, _len(pairs))


def triu_indices(row, col, offset=0, dtype=None, device=None, layout=None):
    pairs = [(i, j) for i in _range(row) for j in _range(col) if j - i >= offset]
    return tensor([[p[0] for p in pairs], [p[1] for p in pairs]], dtype=_dtype_of(dtype) or int64).reshape(2, _len(pairs))


def meshgrid(*tensors, indexing="ij"):
    if _len(tensors) == 1 and _isinstance(tensors[0], (_list, _tuple)):
        tensors = _tuple(tensors[0])
    if indexing not in ("ij", "xy"):
        raise RuntimeError("torch.meshgrid: indexing must be one of \"xy\" or \"ij\", but received: %s" % indexing)
    ts = [t_.reshape(-1) for t_ in tensors]
    swap = indexing == "xy" and _len(ts) >= 2
    if swap:
        ts[0], ts[1] = ts[1], ts[0]
    shape = [t_.shape[0] for t_ in ts]
    out = []
    for i, t_ in enumerate(ts):
        view_shape = [1] * _len(ts)
        view_shape[i] = shape[i]
        out.append(expand(t_.reshape(*view_shape), *shape))
    if swap:
        out[0], out[1] = out[1], out[0]
    return _tuple(out)


def cartesian_prod(*tensors):
    if _len(tensors) == 1:
        return tensors[0]
    grids = meshgrid(*tensors, indexing="ij")
    return stack([g.reshape(-1) for g in grids], 1)


def combinations(input, r=2, with_replacement=False):
    import itertools
    n = input.shape[0]
    pick = itertools.combinations_with_replacement(_range(n), r) if with_replacement else itertools.combinations(_range(n), r)
    rows = _list(pick)
    if not rows:
        return zeros(0, r, dtype=input.dtype)
    idx = tensor(rows, dtype=int64)
    return stack([index_select(input, 0, idx[:, j]) for j in _range(r)], 1)


# ---- indexing ----------------------------------------------------------------------------
def _index_value(v):
    # A slice bound: an int, None, or anything with __index__ (a 0-d
    # integer tensor, as in `data[i:i + block]` with `i` from a tensor).
    if v is None or _isinstance(v, _int):
        return v
    if _isinstance(v, Tensor):
        return v.__index__()
    return v.__index__()


def _is_bool_list(k):
    return _isinstance(k, _list) and _len(k) > 0 and _b.all(_isinstance(v, _b.bool) for v in k)


def _basic_spec(a, key):
    """Split an index into the basic part (ints, slices, None, Ellipsis) and
    the advanced tensor indices with the dims they apply to."""
    if not _isinstance(key, _tuple):
        key = (key,)
    masks = False
    for k in key:
        if _isinstance(k, _list) and _is_bool_list(k):
            # A list of Python bools is a mask, as a bool tensor is.
            key = _tuple(tensor(k, dtype=_bool_dtype) if _is_bool_list(k) else k for k in key)
            masks = True
            break
        if _isinstance(k, Tensor) and k.dtype is _bool_dtype:
            masks = True
    if masks:
        expanded = []
        dim = 0
        for k in key:
            if _isinstance(k, Tensor) and k.dtype is _bool_dtype:
                if k.shape:
                    for j, s in enumerate(k.shape):
                        if dim + j >= _len(a.shape) or a.shape[dim + j] != s:
                            raise IndexError("The shape of the mask %s at index %d does not match the shape of the indexed tensor %s at index %d" % (_list(k.shape), j, _list(a.shape), dim + j))
                    expanded.extend(nonzero(k).unbind(1))
                    dim += _len(k.shape)
                else:
                    # A 0-d mask adds a dim of length 1 (True) or 0 (False).
                    expanded.append(None)
                    if not k.item():
                        expanded.append(_EMPTY)
            else:
                expanded.append(k)
                if k is not None and k is not Ellipsis:
                    dim += 1
        key = _tuple(expanded)
    rank = _len(a.shape)
    consumed = 0
    for k in key:
        if k is not None and k is not Ellipsis and k is not _EMPTY:
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


class _EmptyMark:
    pass


_EMPTY = _EmptyMark()


def _getitem(a, key):
    if type(key) is _int and a.shape:
        # `a[i]`: the spec the general path below builds for a plain int
        # (the kernel range-checks it).
        spec = [key] + [(None, None, None)] * (_len(a.shape) - 1)
        return _slice(a, spec)
    items = _basic_spec(a, key)
    spec, advanced, new_axes = [], [], []
    empty_axis = None
    d = 0
    for k in items:
        if k is _EMPTY:
            empty_axis = new_axes[-1]
            continue
        if k is None or k is True:
            new_axes.append(_len(spec) - _b.sum(1 for s in spec if _isinstance(s, _int)) + _len(new_axes))
            continue
        if _isinstance(k, Tensor):
            if k.dtype.is_floating_point or k.dtype.name in _NARROW_INT:
                raise IndexError("tensors used as indices must be long, int, byte or bool tensors")
            advanced.append((d, k if k.dtype is int64 else k.long()))
            spec.append(slice(None))
        elif _isinstance(k, (_list, _tuple)):
            advanced.append((d, tensor(k, dtype=int64)))
            spec.append(slice(None))
        elif _isinstance(k, _b.bool):
            raise IndexError("a bool index False is not supported")
        elif _isinstance(k, _int):
            spec.append(k)
        elif _isinstance(k, slice):
            spec.append((_index_value(k.start), _index_value(k.stop), _index_value(k.step)))
        elif hasattr(k, "__index__"):
            spec.append(k.__index__())
        else:
            raise IndexError("only integers, slices (`:`), ellipsis (`...`), None and long or byte Variables are valid indices (got %s)" % type(k).__name__)
        d += 1
    while _len(spec) < _len(a.shape):
        spec.append(slice(None))
    spec = [(None, None, None) if _isinstance(s, slice) else s for s in spec]
    out = a if (advanced or new_axes) and _b.all(s == (None, None, None) for s in spec) else _slice(a, spec)
    if advanced:
        # Positions of the advanced dims after the basic step (ints drop dims).
        kept = []
        for i, s in enumerate(spec):
            if not _isinstance(s, _int):
                kept.append(i)
        adv_dims = [kept.index(pos) for pos, _ in advanced]
        out = _advanced_get(out, adv_dims, [t_ for _, t_ in advanced])
    for ax in new_axes:
        out = unsqueeze(out, ax)
    if empty_axis is not None:
        out = narrow(out, empty_axis, 0, 0)
    return out


def _slice(a, spec):
    storage, shape = _k.slice(a._s, a.shape, spec)
    out = _new3(storage, shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (_slice_scatter(g, a.shape, spec),), (a,), "Slice")
    return out


def _slice_scatter(g, shape, spec):
    """Zeros of `shape` with `g` at the sliced positions: the slice's
    gradient, itself differentiable (for create_graph)."""
    base = _new3(_k.zeros(g.dtype.name, _numel(shape)), shape, g.dtype) if shape.__class__ is Size else Tensor(_k.zeros(g.dtype.name, _numel(shape)), _shape_args(shape), g.dtype)
    _k.setslice(base._s, base.shape, spec, g._s, g.shape)
    if _grad_enabled and g.requires_grad:
        base.requires_grad = True
        base._node = _Node(lambda gg: (_slice(gg, spec),), (g,), "SliceBackward")
    return base


def _broadcast_indices(indices):
    shape = ()
    for t_ in indices:
        shape = _broadcast_shapes(shape, _tuple(t_.shape))
    return [expand(t_, *shape) if _tuple(t_.shape) != shape else t_ for t_ in indices], shape


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


def _inverse_perm(order):
    inverse = [0] * _len(order)
    for i, d in enumerate(order):
        inverse[d] = i
    return inverse


def _advanced_get(a, dims, indices):
    indices, ishape = _broadcast_indices(indices)
    rank = _len(a.shape)
    k = _len(dims)
    adjacent = dims == _list(_range(dims[0], dims[0] + k))
    order = dims + [i for i in _range(rank) if i not in dims]
    x = permute(a, *order) if order != _list(_range(rank)) else a
    storage, shape = _k.gather(x._s, x.shape, [t_._s for t_ in indices], ishape)
    out = _new3(storage, shape, a.dtype)
    if _grad_enabled and x.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (_index_put_add(x.shape, indices, ishape, g),), (x,), "Index")
    if adjacent and dims[0] > 0:
        # Index dims take the place of the first advanced dim.
        r = _len(out.shape)
        ni = _len(ishape)
        move = _list(_range(ni, ni + dims[0])) + _list(_range(ni)) + _list(_range(ni + dims[0], r))
        out = permute(out, *move)
    return out


def _index_put_add(shape, indices, ishape, g):
    """Zeros of `shape` accumulating `g` at the leading-dim `indices`: the
    advanced index's gradient, itself differentiable."""
    base = zeros(*shape, dtype=g.dtype)
    _k.scatter_add(base._s, base.shape, [t_._s for t_ in indices], ishape, g._s, g.shape)
    if _grad_enabled and g.requires_grad:
        base.requires_grad = True
        k = _len(indices)

        base._node = _Node(lambda gg: (_advanced_get(gg, _list(_range(k)), indices),), (g,), "IndexBackward")
    return base


def _dim_index(shape, i, target):
    view = [1] * _len(target)
    view[i] = shape[i]
    return expand(arange(shape[i]).reshape(*view), *target)


def _dim_grid(index, dim):
    # Index tensors addressing every position of `index`, with `index`
    # itself along `dim`: the full advanced index for gather/scatter.
    idx = [_dim_index(index.shape, i, index.shape) for i in _range(_len(index.shape))]
    idx[dim] = index
    return idx


def _setitem(a, key, value):
    value = _as_tensor(value, a)
    if value.dtype != a.dtype:
        value = value.to(a.dtype)
    # A value with more dims than the target broadcasts once its leading
    # 1s are dropped, as PyTorch's copy_ allows (t[0] = ones(1, 4)).
    if value.shape and value.shape[0] == 1:
        lead = 0
        while lead < _len(value.shape) and value.shape[lead] == 1:
            lead += 1
        value = value.reshape(*value.shape[lead:])
    if not (_grad_enabled and (a.requires_grad or value.requires_grad)):
        _setitem_into(a._s, a, key, value)
        a._wrote()
        return
    # Under autograd the assignment is an index_put: the overwritten
    # positions pass no gradient back to `a`, and `value` receives the
    # gradient of the positions it filled.
    a._inplace_op(_index_put, key, value)


def _index_put(src, key, value):
    storage = _k.copy(src._s)
    _setitem_into(storage, src, key, value)
    out = Tensor(storage, src.shape, src.dtype)
    if _needs_grad(src, value):
        shape = src.shape

        def backward(g):
            keep = ones(*shape, dtype=_bool_dtype)
            _setitem_into(keep._s, keep, key, tensor(False))
            return (where(keep, g, 0.0) if src.requires_grad else None, _getitem(g, key) if value.requires_grad else None)
        out.requires_grad = True
        out._node = _Node(backward, (src, value), "IndexPut")
    return out


def _setitem_into(dst, a, key, value):
    """Assign `value` into the storage `dst` laid out like `a` at `key`."""
    items = _basic_spec(a, key)
    spec, advanced = [], []
    d = 0
    for k in items:
        if k is None or k is _EMPTY:
            return _setitem_general(dst, a, key, value)
        if _isinstance(k, Tensor) or _isinstance(k, (_list, _tuple)):
            advanced.append((d, k.long() if _isinstance(k, Tensor) else tensor(k, dtype=int64)))
            spec.append((None, None, None))
        elif _isinstance(k, _int):
            spec.append(k)
        elif _isinstance(k, slice):
            spec.append((_index_value(k.start), _index_value(k.stop), _index_value(k.step)))
        else:
            spec.append(k.__index__())
        d += 1
    while _len(spec) < _len(a.shape):
        spec.append((None, None, None))
    if not advanced:
        _k.setslice(dst, a.shape, spec, value._s, value.shape)
        return
    dims = [pos for pos, _ in advanced]
    if dims == _list(_range(_len(dims))) and _b.all(s == (None, None, None) for s in spec):
        indices, ishape = _broadcast_indices([t_ for _, t_ in advanced])
        _k.scatter(dst, a.shape, [t_._s for t_ in indices], ishape, value._s, value.shape)
        return
    _setitem_general(dst, a, key, value)


def _setitem_general(dst, a, key, value):
    # Any other key (basic and advanced indices mixed, as t[:, idx] = v or
    # t[0, idx] = v): index a tensor of flat positions with the same key,
    # then scatter the broadcast value to those positions.
    n = _numel(a.shape)
    positions = _getitem(Tensor(_k.from_flat("int64", _list(_range(n))), a.shape, int64), key)
    target = positions.reshape(-1)
    vals = expand(value, *positions.shape) if _tuple(value.shape) != _tuple(positions.shape) else value
    _k.scatter(dst, (n,), [target._s], target.shape, vals._s, (_numel(vals.shape),))


def index_select(input, dim, index):
    a = input
    if a.__class__ is _SparseTensor:
        return _sp._index_select(a, dim, index)
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    if not a.shape:
        return a.clone()
    index = index.reshape(-1) if index.shape != (index.numel(),) else index
    storage, shape = _k.index_select(a._s, a.shape, d, index._s)
    out = _new3(storage, shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (index_add(zeros(*a.shape, dtype=g.dtype), d, index, g),), (a,), "IndexSelect")
    return out


def index_add(input, dim, index, source, alpha=1):
    """input with alpha * source added at `index` along dim (repeats accumulate)."""
    a = input
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    src = source if alpha == 1 else mul(source, alpha)
    if src.dtype is not a.dtype:
        src = src.to(a.dtype)
    base = movedim(a, d, 0) if d else a
    s_moved = movedim(src, d, 0) if d and src.shape else src
    storage = _k.copy(base._s)
    _k.scatter_add(storage, base.shape, [index.reshape(-1)._s], (index.numel(),), s_moved._s, s_moved.shape)
    out = Tensor(storage, base.shape, a.dtype)
    if d:
        out = Tensor(_k.permute(out._s, out.shape, _inverse_perm(_moved_order(_len(a.shape), d)))[0], a.shape, a.dtype)
    if _needs_grad(a, src):
        out.requires_grad = True
        out._node = _Node(lambda g: (g if a.requires_grad else None, index_select(g, d, index) if src.requires_grad else None), (a, src), "IndexAdd")
    return out


def _moved_order(rank, d):
    # The permutation movedim(x, d, 0) applies.
    return [d] + [i for i in _range(rank) if i != d]


def index_copy(input, dim, index, source):
    if _autocast_cpu is not None:
        return _autocast_run(index_copy, "promote", (input, dim, index, source))
    a = input
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    src = source if source.dtype is a.dtype else source.to(a.dtype)
    base = movedim(a, d, 0) if d else a
    s_moved = movedim(src, d, 0) if d and src.shape else src
    storage = _k.copy(base._s)
    _k.scatter(storage, base.shape, [index.reshape(-1)._s], (index.numel(),), s_moved._s, s_moved.shape)
    out = Tensor(storage, base.shape, a.dtype)
    if d:
        out = Tensor(_k.permute(out._s, out.shape, _inverse_perm(_moved_order(_len(a.shape), d)))[0], a.shape, a.dtype)
    if _needs_grad(a, src):
        out.requires_grad = True
        out._node = _Node(lambda g: (index_fill(g, d, index, 0) if a.requires_grad else None, index_select(g, d, index) if src.requires_grad else None), (a, src), "IndexCopy")
    return out


def index_fill(input, dim, index, value):
    a = input
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    if _isinstance(value, Tensor):
        value = value.item()
    mask = zeros(a.shape[d] if a.shape else 1, dtype=_bool_dtype)
    _k.scatter(mask._s, mask.shape, [index.reshape(-1)._s], (index.numel(),), tensor(True)._s, ())
    view = [1] * _len(a.shape)
    if a.shape:
        view[d] = a.shape[d]
    return where(mask.reshape(*view), full((), value, dtype=a.dtype), a)


def gather(input, dim, index, sparse_grad=False):
    a = input
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    if not a.shape:
        return a.clone()
    return _advanced_get(a, _list(_range(_len(index.shape))), _dim_grid(index, d))


def scatter(input, dim, index, src=None, value=None, reduce=None):
    """input with `src` (or the scalar `value`) written at `index` along dim."""
    a = input
    if src is not None and not _isinstance(src, Tensor):
        src, value = None, src
    if reduce is not None:
        if reduce == "add":
            return scatter_add(a, dim, index, src if src is not None else full(index.shape, value, dtype=a.dtype))
        raise NotImplementedError("scatter(reduce=%r) is not supported on Zipp" % (reduce,))
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    if src is None:
        src = full(index.shape, value, dtype=a.dtype)
    elif _tuple(src.shape) != _tuple(index.shape):
        src = src[_tuple(slice(0, s) for s in index.shape)]
    if src.dtype is not a.dtype:
        src = src.to(a.dtype)
    idx, ishape = _broadcast_indices(_dim_grid(index, d))
    storage = _k.copy(a._s)
    _k.scatter(storage, a.shape, [t_._s for t_ in idx], ishape, src._s, src.shape)
    out = Tensor(storage, a.shape, a.dtype)
    if _needs_grad(a, src):
        def backward(g):
            ga = None
            if a.requires_grad:
                keep = ones(*a.shape, dtype=_bool_dtype)
                _k.scatter(keep._s, keep.shape, [t_._s for t_ in idx], ishape, tensor(False)._s, ())
                ga = where(keep, g, 0.0)
            return (ga, gather(g, d, index) if src.requires_grad else None)
        out.requires_grad = True
        out._node = _Node(backward, (a, src), "Scatter")
    return out


def scatter_add(input, dim, index, src):
    a = input
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    if _tuple(src.shape) != _tuple(index.shape):
        src = src[_tuple(slice(0, s) for s in index.shape)]
    if src.dtype is not a.dtype:
        src = src.to(a.dtype)
    idx, ishape = _broadcast_indices(_dim_grid(index, d))
    storage = _k.copy(a._s)
    _k.scatter_add(storage, a.shape, [t_._s for t_ in idx], ishape, src._s, src.shape)
    out = Tensor(storage, a.shape, a.dtype)
    if _needs_grad(a, src):
        out.requires_grad = True
        out._node = _Node(lambda g: (g if a.requires_grad else None, gather(g, d, index) if src.requires_grad else None), (a, src), "ScatterAdd")
    return out


def masked_fill(input, mask, value):
    a = input
    if _isinstance(value, Tensor):
        if value.shape:
            raise RuntimeError("masked_fill_ only supports a 0-dimensional value tensor, but got tensor with %d dimension(s)." % _len(value.shape))
        v = value if value.dtype is a.dtype else value.to(a.dtype)
    else:
        v = full((), value, dtype=a.dtype)
    out = where(mask, v, a)
    return out if _tuple(out.shape) == _tuple(a.shape) else _raise_shape(a, out)


def _raise_shape(a, out):
    raise RuntimeError("output with shape %s doesn't match the broadcast shape %s" % (_list(a.shape), _list(out.shape)))


def masked_select(input, mask):
    shape = _broadcast_shapes(input.shape, mask.shape)
    a = expand(input, *shape)
    m = expand(mask, *shape)
    return a[m]


def masked_scatter(input, mask, source):
    a = input
    m = expand(mask, *a.shape) if _tuple(mask.shape) != _tuple(a.shape) else mask
    count = _int(sum(m).item())
    if count > source.numel():
        raise RuntimeError("masked_scatter: expected source to have at least %d elements" % count)
    out = a.clone()
    out[m] = source.reshape(-1)[:count]
    return out


def take(input, index):
    return input.reshape(-1)[index]


def take_along_dim(input, indices, dim=None):
    if dim is None:
        return take(input, indices.reshape(-1))
    d = _norm_dim(dim, _len(input.shape))
    shape_a = _list(input.shape)
    shape_i = _list(indices.shape)
    shape_a[d] = shape_i[d] = 1
    common = _list(_broadcast_shapes(shape_a, shape_i))
    ta = expand(input, *(common[:d] + [input.shape[d]] + common[d + 1:]))
    ti = expand(indices, *(common[:d] + [indices.shape[d]] + common[d + 1:]))
    return gather(ta, d, ti)


def nonzero(input, as_tuple=False):
    a = input
    flat = _k.to_list(a._s)
    rank = _len(a.shape)
    if rank == 0:
        hit = 1 if flat[0] else 0
        return (zeros(hit, dtype=int64),) if as_tuple else zeros(hit, 0, dtype=int64)
    coords = [[] for _ in a.shape]
    strides = a.stride()
    for i, v in enumerate(flat):
        if v:
            rest = i
            for dd, s in enumerate(strides):
                coords[dd].append(rest // s)
                rest %= s
    cols = [Tensor(_k.from_flat("int64", c), (_len(c),), int64) for c in coords]
    if as_tuple:
        return _tuple(cols)
    return stack(cols, 1)


def argwhere(input):
    return nonzero(input)


def searchsorted(sorted_sequence, input, out_int32=False, right=False, side=None, sorter=None):
    """Insertion points of `input` into the innermost dim of sorted_sequence
    (bisect_left, or bisect_right when right=True / side='right')."""
    import bisect
    right = right or side == "right"
    seq = sorted_sequence
    values = input if _isinstance(input, Tensor) else tensor(input)
    scalar = not _isinstance(input, Tensor)
    if sorter is not None:
        seq = gather(seq, -1, sorter)
    n = seq.shape[-1]
    rows = _k.to_list(seq._s)
    vals = _k.to_list(values._s)
    find = bisect.bisect_right if right else bisect.bisect_left
    out = []
    if _len(seq.shape) == 1:
        out = [find(rows, v) for v in vals]
    else:
        m = values.shape[-1]
        for r in _range(_len(rows) // n):
            row = rows[r * n:(r + 1) * n]
            out.extend(find(row, v) for v in vals[r * m:(r + 1) * m])
    dt = int32 if out_int32 else int64
    res = Tensor(_k.from_flat(dt.name, out), values.shape, dt)
    return res.reshape(()) if scalar else res


def bucketize(input, boundaries, out_int32=False, right=False):
    return searchsorted(boundaries, input, out_int32=out_int32, right=right)


# ---- linear algebra ----------------------------------------------------------------------
def matmul(input, other):
    if _autocast_cpu is not None:
        return _autocast_run(matmul, "lower", (input, other))
    a, b = input, other
    if _graph_recording:
        if getattr(a, "_zipp_graph", False):
            return a @ b
        if getattr(b, "_zipp_graph", False):
            return b.__rmatmul__(a)
    if type(a) is Tensor:
        ta = a
    elif a.__class__ is _SparseTensor:
        return _sp._matmul(a, b)
    else:
        ta = _as_tensor(a)
    if type(b) is Tensor:
        tb = b
    elif b.__class__ is _SparseTensor:
        return _sp._matmul(ta, b)
    else:
        tb = _as_tensor(b)
    if ta.dtype is not tb.dtype and ta.dtype.is_complex is not tb.dtype.is_complex:
        # PyTorch does not promote a real operand of a complex product.
        raise RuntimeError("expected m1 and m2 to have the same dtype, but got: %s != %s" % (_CPP_NAME.get(ta.dtype.name, ta.dtype.name), _CPP_NAME.get(tb.dtype.name, tb.dtype.name)))
    try:
        storage, shape = _k.matmul(ta._s, ta.shape, tb._s, tb.shape)
    except RuntimeError as e:
        _lazy_reraise(e, "matmul", ta, tb)
    out = _new3(storage, shape, _DTYPES[_k.dtype(storage)])
    if _grad_enabled and (ta.requires_grad or tb.requires_grad):
        sa, sb, ra, rb = ta._s, tb._s, ta.requires_grad, tb.requires_grad

        def backward(g):
            x, y = _frozen(ta, sa), _frozen(tb, sb)
            gx = gy = None
            xs = unsqueeze(x, 0) if _len(x.shape) == 1 else x
            ys = unsqueeze(y, 1) if _len(y.shape) == 1 else y
            # g with the dims a 1-d operand dropped put back: (..., m, n).
            if _len(x.shape) == 1 and _len(y.shape) == 1:
                gg = g.reshape(1, 1)
            elif _len(x.shape) == 1:
                gg = unsqueeze(g, -2)
            elif _len(y.shape) == 1:
                gg = unsqueeze(g, -1)
            else:
                gg = g
            if ra:
                gx = _unbroadcast(matmul(gg, _cj(transpose(ys, -1, -2))), xs.shape)
                if _len(x.shape) == 1:
                    gx = gx.reshape(*x.shape)
            if rb:
                gy = _unbroadcast(matmul(_cj(transpose(xs, -1, -2)), gg), ys.shape)
                if _len(y.shape) == 1:
                    gy = gy.reshape(*y.shape)
            return (gx, gy)
        saved = ([ta] if rb else []) + ([tb] if ra else [])
        out.requires_grad = True
        out._node = _Node(backward, (ta, tb), "Dot" if _len(ta.shape) == 1 and _len(tb.shape) == 1 else "Mm", saved)
    return out


def _max_pool2d(x, k, s, p, d, outs, replay):
    """F.max_pool2d's values and window indices for a batched 4-d floating
    `x` in one kernel, or None where the stacked-views path must run.

    That path pads with -inf, stacks the kh * kw strided views and takes
    `max(-1)`; its gradient reaches the input as the sum of every view's
    slice gradient. The kernel's values and indices are that max's. Taken
    only when windows cannot overlap (stride >= dilation * (k - 1) + 1 in
    both dims) and hold two or more positions: each input position then
    receives at most one nonzero term of that sum, so its value is the
    output gradient there (`g + 0`, the adds turning -0 into +0) or +0, in
    whichever order the terms were added. Under create_graph the gradient is
    the stacked-views path's own, recorded: `replay` runs that path."""
    if (_graph_recording or not _isinstance(x, Tensor) or not x.dtype.is_floating_point or _len(x.shape) != 4
            or k[0] * k[1] < 2 or s[0] < d[0] * (k[0] - 1) + 1 or s[1] < d[1] * (k[1] - 1) + 1):
        return None
    dims = (k[0], k[1], s[0], s[1], p[0], p[1], d[0], d[1], outs[0], outs[1])
    vs, ws = _k.max_pool2d(x._s, x.shape, dims)
    shape = Size((x.shape[0], x.shape[1], outs[0], outs[1]))
    values = Tensor(vs, shape, x.dtype)
    which = Tensor(ws, shape, int64)
    if _grad_enabled and x.requires_grad:
        sx = x._s
        xshape = x.shape
        xdt = x.dtype

        def backward(g):
            if _grad_enabled:
                # create_graph: the gradient as the stacked-views path
                # computes and records it (it reads only `which` and the
                # shapes, never x, so a detached input serves).
                xd = Tensor(sx, xshape, xdt)
                xd.requires_grad = True
                with enable_grad():
                    ref = replay(xd)
                return _autograd_grad([ref], [xd], [g], create_graph=True)
            return (Tensor(_k.max_pool2d_backward(g._s, ws, xshape, dims), xshape, g.dtype),)
        values.requires_grad = True
        values._node = _Node(backward, (x,), "Max")
    return values, which


def _linear(x, weight, bias=None):
    """F.linear: x @ weight.T (+ bias). The product reads the weight
    transposed in place (`matmul`'s transB) where `matmul(x, weight.T)`
    would copy it and record a Permute node; the values, gradients and their
    accumulation order are that path's exactly (see `_linear_mm`)."""
    if _autocast_cpu is not None:
        return _autocast_run(_linear, "lower", (x, weight, bias))
    try:
        if (not _graph_recording and _isinstance(x, Tensor) and _isinstance(weight, Tensor)
                and _len(weight.shape) == 2 and _len(x.shape) >= 2 and weight.dtype.is_floating_point and x.dtype.is_floating_point):
            out = _linear_mm(x, weight)
        else:
            out = matmul(x, weight.transpose(0, 1))
        return out if bias is None else out + bias
    except (RuntimeError, ValueError) as e:
        _lazy_reraise(e, "linear", x, weight, bias)


def _linear_mm(x, w):
    storage, shape = _k.matmul(x._s, x.shape, w._s, w.shape, True)
    out = _new3(storage, shape, _DTYPES[_k.dtype(storage)])
    rx = x.requires_grad
    rw = w.requires_grad
    if _grad_enabled and (rx or rw):
        sx = x._s
        wshape = w.shape
        wdt = w.dtype
        # The weight's values now, for x's gradient: `matmul(x, weight.T)`
        # kept them in its transposed copy, so a later in-place update of
        # the weight (an optimizer step before backward) is not seen.
        wsnap = _k.copy(w._s) if rx else None

        def backward(g):
            xs = _frozen(x, sx)
            gx = gw = None
            if _grad_enabled:
                # create_graph: the transposed path's own operations, so
                # the recorded gradient graph is the one it would record.
                wt = Tensor(_k.permute(wsnap, wshape, [1, 0])[0], (wshape[1], wshape[0]), wdt) if rx else None
                if rx and rw:
                    wt.requires_grad = True
                    pnode = _Node(lambda gg: (permute(gg, 1, 0),), (w,), "Permute")
                    # The weight's history as this product consumed it.
                    pnode.pstate = [node.pstate[1]]
                    wt._node = pnode
                if rx:
                    gx = _unbroadcast(matmul(g, transpose(wt, -1, -2)), xs.shape)
                if rw:
                    gw = permute(_unbroadcast(matmul(transpose(xs, -1, -2), g), (wshape[1], wshape[0])), 1, 0)
                return (gx, gw)
            if rx:
                gx = _unbroadcast(matmul(g, Tensor(wsnap, wshape, wdt)), xs.shape)
            if rw:
                # g^T @ x sums, per element, the products x^T @ g sums, in
                # the same order: the transposed path's value, transposed.
                gw = _unbroadcast(matmul(transpose(g, -1, -2), xs), wshape)
            return (gx, gw)
        out.requires_grad = True
        node = _Node(backward, (x, w), "Mm", [x] if rw else None)
        out._node = node
    return out


def mm(input, mat2):
    if _autocast_cpu is not None:
        return _autocast_run(mm, "lower", (input, mat2))
    if input.__class__ is _SparseTensor or mat2.__class__ is _SparseTensor:
        return _sp._mm(input, mat2)
    if _len(input.shape) != 2 or _len(mat2.shape) != 2:
        raise RuntimeError("self must be a matrix" if _len(input.shape) != 2 else "mat2 must be a matrix")
    return matmul(input, mat2)


def bmm(input, mat2):
    if _autocast_cpu is not None:
        return _autocast_run(bmm, "lower", (input, mat2))
    if _len(input.shape) != 3 or _len(mat2.shape) != 3:
        raise RuntimeError("batch1 must be a 3D tensor" if _len(input.shape) != 3 else "batch2 must be a 3D tensor")
    if input.shape[0] != mat2.shape[0]:
        raise RuntimeError("batch1 and batch2 must have same number of batches, got %d and %d" % (input.shape[0], mat2.shape[0]))
    return matmul(input, mat2)


def mv(input, vec):
    if _autocast_cpu is not None:
        return _autocast_run(mv, None, (input, vec))
    if _len(input.shape) != 2 or _len(vec.shape) != 1:
        raise RuntimeError("vector + matrix @ vector expected, got %d, %d" % (_len(input.shape), _len(vec.shape)))
    return matmul(input, vec)


def dot(input, other):
    if _autocast_cpu is not None:
        return _autocast_run(dot, None, (input, other))
    if _len(input.shape) != 1 or _len(other.shape) != 1:
        raise RuntimeError("1D tensors expected, but got %dD and %dD tensors" % (_len(input.shape), _len(other.shape)))
    if input.shape[0] != other.shape[0]:
        raise RuntimeError("inconsistent tensor size, expected tensor [%d] and src [%d] to have the same number of elements, but got %d and %d elements respectively" % (input.shape[0], other.shape[0], input.shape[0], other.shape[0]))
    return matmul(input, other)


inner = dot


def vdot(input, other):
    return dot(conj(input), other)


def outer(input, vec2):
    return mul(unsqueeze(input, 1), unsqueeze(vec2, 0))


ger = outer


def _scaled_sum(input, product, beta, alpha):
    if alpha != 1:
        product = mul(product, alpha)
    if beta == 0:
        # beta=0 ignores input entirely (a nan or inf in it does not propagate).
        return product
    return add(input if beta == 1 else mul(input, beta), product)


def addmm(input, mat1, mat2, beta=1, alpha=1):
    if _autocast_cpu is not None:
        return _autocast_run(addmm, "lower", (input, mat1, mat2, beta, alpha))
    return _scaled_sum(input, mm(mat1, mat2), beta, alpha)


def addmv(input, mat, vec, beta=1, alpha=1):
    if _autocast_cpu is not None:
        return _autocast_run(addmv, None, (input, mat, vec, beta, alpha))
    return _scaled_sum(input, mv(mat, vec), beta, alpha)


def addbmm(input, batch1, batch2, beta=1, alpha=1):
    if _autocast_cpu is not None:
        return _autocast_run(addbmm, "lower", (input, batch1, batch2, beta, alpha))
    return _scaled_sum(input, sum(bmm(batch1, batch2), 0), beta, alpha)


def baddbmm(input, batch1, batch2, beta=1, alpha=1):
    if _autocast_cpu is not None:
        return _autocast_run(baddbmm, "lower", (input, batch1, batch2, beta, alpha))
    return _scaled_sum(input, bmm(batch1, batch2), beta, alpha)


def addr(input, vec1, vec2, beta=1, alpha=1):
    return _scaled_sum(input, outer(vec1, vec2), beta, alpha)


def _tensordot_policy(a, b, dims):
    # PyTorch contracts through mm (autocast) unless every dim is contracted.
    n = dims if _isinstance(dims, _int) else _len(dims[0]) if _isinstance(dims[0], (_list, _tuple)) else 1
    return None if _len(a.shape) + _len(b.shape) - 2 * n <= 0 else "lower"


def _einsum_policy(equation, operands):
    # PyTorch contracts pairs of operands with bmm (autocast) when a label
    # is summed away; elementwise and single-operand equations do not.
    if _len(operands) == 1 and type(operands[0]) in (_list, _tuple):
        operands = operands[0]
    if _len(operands) < 2 or not _isinstance(equation, str):
        return None
    lhs, _, rhs = equation.replace(" ", "").partition("->")
    labels = [c for c in lhs if c.isalpha()]
    if "->" not in equation:
        return "lower" if _b.any(labels.count(c) > 1 for c in labels) else None
    return "lower" if _b.any(c not in rhs for c in labels) else None


def tensordot(a, b, dims=2):
    if _autocast_cpu is not None:
        return _autocast_run(tensordot, _tensordot_policy(a, b, dims), (a, b, dims))
    if _isinstance(dims, Tensor):
        dims = dims.tolist()
    if _isinstance(dims, _int):
        da = _list(_range(_len(a.shape) - dims, _len(a.shape)))
        db = _list(_range(dims))
    else:
        da, db = dims
        da = [da] if _isinstance(da, _int) else _list(da)
        db = [db] if _isinstance(db, _int) else _list(db)
    da = [_norm_dim(d, _len(a.shape)) for d in da]
    db = [_norm_dim(d, _len(b.shape)) for d in db]
    for x, y in zip(da, db):
        if a.shape[x] != b.shape[y]:
            raise RuntimeError("contracted dimensions need to match, but first has size %d in dim %d and second has size %d in dim %d" % (a.shape[x], x, b.shape[y], y))
    free_a = [d for d in _range(_len(a.shape)) if d not in da]
    free_b = [d for d in _range(_len(b.shape)) if d not in db]
    k = _numel([a.shape[d] for d in da])
    pa = permute(a, *(free_a + da)).reshape(_numel([a.shape[d] for d in free_a]), k)
    pb = permute(b, *(db + free_b)).reshape(k, _numel([b.shape[d] for d in free_b]))
    return matmul(pa, pb).reshape(*([a.shape[d] for d in free_a] + [b.shape[d] for d in free_b]))


def kron(input, other):
    a, b = input, other
    rank = _b.max(_len(a.shape), _len(b.shape))
    sa = [1] * (rank - _len(a.shape)) + _list(a.shape)
    sb = [1] * (rank - _len(b.shape)) + _list(b.shape)
    va, vb = [], []
    for x, y in zip(sa, sb):
        va += [x, 1]
        vb += [1, y]
    return mul(a.reshape(*va), b.reshape(*vb)).reshape(*[x * y for x, y in zip(sa, sb)])


def cross(input, other, dim=None):
    a, b = input, other
    shape = _broadcast_shapes(a.shape, b.shape)
    a, b = expand(a, *shape), expand(b, *shape)
    if dim is None:
        dims = [i for i, s in enumerate(shape) if s == 3]
        if not dims:
            raise RuntimeError("no dimension of size 3 in input")
        d = dims[0]
    else:
        d = _norm_dim(dim, _len(shape))
    if shape[d] != 3:
        raise RuntimeError("linalg.cross: inputs dimension %d must have length 3. Got %d and %d" % (d, shape[d], shape[d]))
    a0, a1, a2 = a.unbind(d)
    b0, b1, b2 = b.unbind(d)
    return stack([sub(mul(a1, b2), mul(a2, b1)), sub(mul(a2, b0), mul(a0, b2)), sub(mul(a0, b1), mul(a1, b0))], d)


def cdist(x1, x2, p=2.0, compute_mode=None):
    """Pairwise p-norm distances between the rows of x1 [..., P, M] and x2 [..., R, M]."""
    if _autocast_cpu is not None:
        return _autocast_run(cdist, "fp32", (x1, x2, p, compute_mode))
    d = sub(unsqueeze(x1, -2), unsqueeze(x2, -3))
    return norm(d, p, -1)


def pairwise_distance(x1, x2, p=2.0, eps=1e-6, keepdim=False):
    return norm(add(sub(x1, x2), eps), p, -1, keepdim)


def _einsum_split(spec, operands):
    """Parse an einsum spec, expanding '...' to per-dim labels (aligned from
    the right across operands, as broadcasting does)."""
    spec = spec.replace(" ", "")
    lhs, arrow, rhs = spec.partition("->")
    terms = lhs.split(",")
    if _len(terms) != _len(operands):
        raise ValueError("einsum(): more operands were provided than specified in the equation" if _len(terms) < _len(operands) else "einsum(): fewer operands were provided than specified in the equation")
    ell = 0
    for term, op in zip(terms, operands):
        if "..." in term:
            ell = _b.max(ell, _len(op.shape) - (_len(term) - 3))
    names = [chr(0x4e00 + i) for i in _range(ell)]
    out_terms = []
    for term, op in zip(terms, operands):
        if "..." in term:
            n = _len(op.shape) - (_len(term) - 3)
            term = term.replace("...", "".join(names[ell - n:]))
        if _len(term) != _len(op.shape):
            raise ValueError("einsum(): the number of subscripts in the equation (%d) does not match the number of dimensions (%d) for operand" % (_len(term), _len(op.shape)))
        out_terms.append(term)
    if arrow:
        rhs = rhs.replace("...", "".join(names))
    else:
        counts = {}
        for term in out_terms:
            for c in term:
                counts[c] = counts.get(c, 0) + 1
        rhs = "".join(names) + "".join(_b.sorted(c for c in counts if counts[c] == 1 and c not in names))
    return out_terms, rhs


def einsum(equation, *operands):
    """einsum by broadcasting: align every operand's labels, multiply, then
    sum the labels absent from the output. A label repeated within one
    operand takes its diagonal; '...' covers broadcast dims. Every step
    keeps autograd."""
    if _autocast_cpu is not None:
        return _autocast_run(einsum, _einsum_policy(equation, operands), (equation,) + _tuple(operands))
    if _len(operands) == 1 and _isinstance(operands[0], (_list, _tuple)):
        operands = _tuple(operands[0])
    terms, rhs = _einsum_split(equation, operands)
    ops = []
    for term, op in zip(terms, operands):
        # Repeated labels within an operand: its diagonal over those dims.
        while _len(set(term)) != _len(term):
            for i, c in enumerate(term):
                j = term.find(c, i + 1)
                if j >= 0:
                    op = diagonal(op, 0, i, j)
                    term = term[:i] + term[i + 1:j] + term[j + 1:] + c
                    break
        ops.append((term, op))
    labels = []
    for term, _ in ops:
        for c in term:
            if c not in labels:
                labels.append(c)
    for c in rhs:
        if c not in labels:
            raise ValueError("einsum(): output subscript %s does not appear in the equation for any input operand" % c)
    sizes = {}
    aligned = []
    for term, op in ops:
        for c, s in zip(term, op.shape):
            if c in sizes and sizes[c] != s and s != 1 and sizes[c] != 1:
                raise ValueError("einsum(): operands do not broadcast with remapped shapes (original->remapped)")
            sizes[c] = _b.max(sizes.get(c, 1), s)
        order = [term.index(c) for c in labels if c in term]
        x = permute(op, *order) if order != _list(_range(_len(order))) else op
        shape = [op.shape[term.index(c)] if c in term else 1 for c in labels]
        aligned.append(x.reshape(*shape))
    prod_ = aligned[0]
    for x in aligned[1:]:
        prod_ = mul(prod_, x)
    if _len(aligned) == 1:
        prod_ = expand(prod_, *[sizes[c] for c in labels])
    reduce_dims = [i for i, c in enumerate(labels) if c not in rhs]
    out = sum(prod_, reduce_dims) if reduce_dims else prod_
    remaining = [c for c in labels if c in rhs]
    order = [remaining.index(c) for c in rhs]
    if order != _list(_range(_len(order))):
        out = permute(out, *order)
    return out


def equal(input, other):
    return _tuple(input.shape) == _tuple(other.shape) and _bool(_k.equal(input._s, other._s))


def allclose(input, other, rtol=1e-05, atol=1e-08, equal_nan=False):
    a, b = _as_tensor(input), _as_tensor(other)
    if equal_nan or a.dtype.is_complex or b.dtype.is_complex:
        return _bool(isclose(a, b, rtol, atol, True).all().item())
    if _tuple(a.shape) != _tuple(b.shape):
        shape = _broadcast_shapes(a.shape, b.shape)
        a, b = expand(a, *shape), expand(b, *shape)
    return _bool(_k.allclose(a._s, b._s, _float(rtol), _float(atol)))


def isclose(input, other, rtol=1e-05, atol=1e-08, equal_nan=False):
    a, b = input, other
    close = logical_or(a == b, abs(sub(a, b)) <= add(mul(abs(b), rtol), atol))
    if equal_nan:
        close = logical_or(close, logical_and(isnan(_as_tensor(a)), isnan(_as_tensor(b))))
    return close


def softmax(input, dim=-1, dtype=None):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.softmax(dim, dtype)
    if a.__class__ is _SparseTensor:
        raise _sp._no_kernel("aten::_softmax", a)
    if dtype is not None:
        a = a.to(dtype)
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    out = Tensor(_k.softmax(a._s, a.shape or (1,), d, False), a.shape, _default_dtype if not a.dtype.is_floating_point else a.dtype)
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        so = out._s

        def backward(g):
            o = _frozen(out, so)
            return (mul(o, sub(g, sum(mul(g, o), d if a.shape else None, True))),)
        out._node = _Node(backward, (a,), "Softmax", (out,))
    return out


def log_softmax(input, dim=-1, dtype=None):
    a = input
    if _graph_recording and getattr(a, "_zipp_graph", False):
        return a.log_softmax(dim, dtype)
    if a.__class__ is _SparseTensor:
        raise _sp._no_kernel("aten::_log_softmax", a)
    if dtype is not None:
        a = a.to(dtype)
    d = _norm_dim(dim, _b.max(_len(a.shape), 1))
    out = Tensor(_k.softmax(a._s, a.shape or (1,), d, True), a.shape, _default_dtype if not a.dtype.is_floating_point else a.dtype)
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        so = out._s
        out._node = _Node(lambda g: (sub(g, mul(exp(_frozen(out, so)), sum(g, d if a.shape else None, True))),), (a,), "LogSoftmax", (out,))
    return out


def clip_grad_norm_(parameters, max_norm, norm_type=2.0):
    if _isinstance(parameters, Tensor):
        parameters = [parameters]
    grads = [p.grad for p in parameters if p.grad is not None]
    if not grads:
        return tensor(0.0)
    norm_type = _float(norm_type)
    if norm_type == 2.0:
        total = _math.sqrt(_b.sum(_float(_k.dot_sum(g._s, g._s)) for g in grads))
    elif norm_type == _math.inf:
        total = _b.max(abs(g).max().item() if g.numel() else 0.0 for g in grads)
    elif norm_type > 0:
        total = _b.sum(pow(abs(g), norm_type).sum().item() for g in grads) ** (1.0 / norm_type)
    else:
        raise NotImplementedError("clip_grad_norm_ on Zipp supports norm_type > 0 and inf, not %r" % norm_type)
    coef = _float(max_norm) / (total + 1e-6)
    if coef < 1.0:
        for g in grads:
            _k.copy_into(g._s, mul(g, coef)._s)
    return tensor(total)


# ---- autograd engine ---------------------------------------------------------------------
# Nodes whose backward is itself built from differentiable ops, so
# create_graph=True can record it. Any other node (a kernel-level gradient:
# convolution, prod, cumprod, ...) refuses create_graph rather than
# silently dropping second-order terms.
_DIFFERENTIABLE = frozenset([
    "Add", "Sub", "Mul", "Div", "Pow", "Neg", "Exp", "Exp2", "Expm1", "Log", "Log2", "Log10", "Log1p",
    "Tanh", "Sigmoid", "Silu", "Gelu", "GeluBackward", "Relu", "Sqrt", "Rsqrt", "Reciprocal", "Sin", "Cos",
    "Tan", "Asin", "Acos", "Atan", "Sinh", "Cosh", "Asinh", "Acosh", "Atanh", "Erf", "Erfc", "Erfinv", "Abs",
    "Floor", "Ceil", "Trunc", "Sign", "Round", "Frac", "Clamp", "Where", "ToCopy", "Sum", "Mean", "Max", "Min",
    "Amax", "Amin", "LinalgVectorNorm", "Std", "Clone", "View", "Permute", "Expand", "Cat", "Roll", "Slice", "SliceBackward",
    "Index", "IndexBackward", "IndexSelect", "IndexAdd", "IndexCopy", "IndexPut", "Scatter", "ScatterAdd",
    "Mm", "Dot", "Softmax", "LogSoftmax", "Cumsum", "Cummax", "Cummin", "Logcumsumexp", "Remainder", "Fmod",
    "Atan2", "Maximum", "Minimum", "Fill", "Zero", "Copy", "CopySlices", "Median",
    "Lgamma", "Digamma", "Polygamma", "I0", "SpecialI0E", "SpecialI1", "SpecialI1E", "Sinc", "Logit", "Xlogy", "Igamma", "Igammac",
    "SpecialErfcx", "SpecialNdtr", "SpecialNdtri", "SpecialLogNdtr", "SpecialEntr", "SpecialXlog1Py", "SpecialZeta",
    "ViewAsReal", "ViewAsComplex", "Conj", "ConjPhysical", "Select", "Angle", "Sgn",
])


def _run_backward(tensors, grad_tensors, create_graph=False, inputs=None):
    roots, grads = [], []
    for i, (t_, g) in enumerate(zip(tensors, grad_tensors)):
        if not t_.requires_grad:
            raise RuntimeError("element %d of tensors does not require grad and does not have a grad_fn" % i)
        if g is None:
            if _numel(t_.shape) != 1:
                raise RuntimeError("grad can be implicitly created only for scalar outputs")
            if t_.dtype.is_complex:
                raise RuntimeError("grad can be implicitly created only for real scalar outputs but got %s" % repr(t_.dtype))
            # ones(*t_.shape, dtype=t_.dtype), one element.
            g = Tensor(_k.full(t_.dtype.name, 1, 1), t_.shape, t_.dtype)
        else:
            g = _as_tensor(g, t_)
            if _tuple(g.shape) != _tuple(t_.shape):
                if _numel(g.shape) != _numel(t_.shape):
                    raise RuntimeError("Mismatch in shape: grad_output[%d] has a shape of %s and output[%d] has a shape of %s." % (i, g.shape, i, t_.shape))
                g = g.reshape(*t_.shape)
        roots.append(t_)
        grads.append(g)
    if inputs is not None:
        inputs = [inputs] if _isinstance(inputs, Tensor) else _list(inputs)
        if not inputs:
            raise RuntimeError("'inputs' argument to backward() cannot be empty.")
    _backward(roots, grads, accumulate=True, inputs=inputs, create_graph=create_graph)


def _accumulate_grad(t_, g, create_graph):
    if g.__class__ is _SparseTensor or t_.grad.__class__ is _SparseTensor:
        return _sp._accumulate_grad(t_, g)
    if g.dtype is not t_.dtype and t_.dtype._inexact:
        g = _grad_as(g, t_.dtype)
    if t_.grad is None:
        # A gradient of its own: a backward result can be shared (an add
        # hands one gradient to both operands, or the caller's `gradient`
        # reaches a leaf unchanged), and .grad is mutated in place later.
        t_.grad = g.clone() if create_graph else Tensor(_k.copy(g._s), g.shape, g.dtype)
    elif create_graph:
        t_.grad = add(t_.grad, g)
    else:
        # Outside create_graph PyTorch accumulates into the existing .grad
        # in place, so a reference to it sees the sum.
        grad = t_.grad
        total = add(grad, g)
        if total.shape is grad.shape and total.dtype is grad.dtype and not grad._untracked:
            # What `_write` does once its checks pass.
            _k.copy_into(grad._s, total._s)
        else:
            grad._write(total)


# The traversal marks the tensors it visits instead of keeping id-keyed
# sets and dicts (on this runtime `id()` and a dict or set method call each
# cost several attribute loads): `_bwe` is the traversal a tensor was last
# visited by, `_pg` its pending gradient and `_bps` its node's parents when
# one of them had to be replaced by an alias. A visit clears `_pg` and
# `_bps`, so values an earlier traversal left behind (one that raised) are
# never read. A backward started while another runs (a hook or a node's
# backward calling backward) takes `_backward_dict`, which keeps its state
# in local dicts and so cannot disturb the outer traversal's marks. Both
# visit, order and accumulate exactly alike.
_bwd_active = False


def _backward(roots, grads, accumulate=True, inputs=None, create_graph=False):
    global _bwd_active, _grad_enabled, _autocast_cpu
    if _autocast_cpu is not None:
        # Backward runs with autocast off: its ops take the dtypes the
        # forward ops chose, as PyTorch's backward does.
        fast, _autocast_cpu = _autocast_cpu, None
        try:
            return _backward(roots, grads, accumulate, inputs, create_graph)
        finally:
            _autocast_cpu = fast
    if _bwd_active:
        return _backward_dict(roots, grads, accumulate, inputs, create_graph)
    if _isinstance(roots, Tensor):
        roots, grads = [roots], [grads]
    _bwd_active = True
    try:
        cur = []
        order = [None] * 32
        cap = 32
        n_order = 0
        aliases = None
        # Depth-first post-order without recursion, as `_backward_dict`: a
        # stack of (tensor, done) pairs held in a list indexed by `sp`.
        stack = [None] * 32
        scap = 32
        sp = 0
        i = _len(roots) - 1
        while i >= 0:
            if sp == scap:
                stack += [None] * scap
                scap += scap
            stack[sp] = (roots[i], False)
            sp += 1
            i -= 1
        while sp:
            sp -= 1
            t_, done = stack[sp]
            if done:
                if n_order == cap:
                    order += [None] * cap
                    cap += cap
                order[n_order] = t_
                n_order += 1
                continue
            if t_._bwe is cur:
                continue
            t_._bwe = cur
            if t_._pg is not None:
                t_._pg = None
            if t_._bps is not None:
                t_._bps = None
            # The done entry takes this slot: the pop above freed it.
            stack[sp] = (t_, True)
            sp += 1
            node = t_._node
            if node is not None:
                # The parents as this node consumed them: one given a new
                # history by an in-place op since stands in as an alias
                # carrying the history it had then (one alias per earlier
                # version, shared by every node that consumed it).
                ps = node.parents
                pstate = node.pstate
                i = _len(ps) - 1
                while i >= 0:
                    p = ps[i]
                    if p is not None:
                        st = pstate[i]
                        if p._node is not st[0]:
                            if aliases is None:
                                aliases = {}
                            akey = (id(p), id(st[0]))
                            alias = aliases.get(akey)
                            if alias is None:
                                alias = aliases[akey] = Tensor(p._s, st[2], p.dtype, st[1], st[0])
                            if t_._bps is None:
                                ps = t_._bps = _list(ps)
                            ps[i] = alias
                            p = alias
                        if p.requires_grad and p._bwe is not cur:
                            if sp == scap:
                                stack += [None] * scap
                                scap += scap
                            stack[sp] = (p, False)
                            sp += 1
                    i -= 1
        i = 0
        for r in roots:
            g = grads[i]
            i += 1
            prior = r._pg
            r._pg = g if prior is None else (add(prior, g) if prior.__class__ is not _SparseTensor else _sp._buffer_add(prior, g))
        captured = {}
        wanted = None if inputs is None else set(id(t_) for t_ in inputs)
        # The grad mode for the backward functions: recording only for
        # create_graph (as `with enable_grad()` / `with no_grad()`).
        prev_mode = _grad_enabled
        _grad_enabled = create_graph is True or _bool(create_graph)
        try:
            k = n_order - 1
            while k >= 0:
                t_ = order[k]
                k -= 1
                g = t_._pg
                if g is None:
                    continue
                t_._pg = None
                if t_._hooks:
                    for hook in _list(t_._hooks):
                        replaced = hook(g)
                        if replaced is not None:
                            g = replaced
                if wanted is not None:
                    key = id(t_)
                    if key in wanted:
                        captured[key] = g if key not in captured else add(captured[key], g)
                        if accumulate:
                            _accumulate_grad(t_, g, create_graph)
                node = t_._node
                if node is None:
                    if accumulate and wanted is None and t_.requires_grad:
                        _accumulate_grad(t_, g, create_graph)
                    continue
                if t_._retain and accumulate and wanted is None:
                    _accumulate_grad(t_, g, create_graph)
                if node.saved is not None:
                    _check_saved(node)
                if create_graph and not (node.diff or node.name in _DIFFERENTIABLE):
                    raise NotImplementedError("backward with create_graph=True through %s is not supported on Zipp" % _grad_fn_name(node.name))
                parent_grads = node.backward(g)
                tp = parent_grads.__class__
                if tp is not _tuple and tp is not _list:
                    parent_grads = _tuple(parent_grads)
                ps = t_._bps
                if ps is None:
                    ps = node.parents
                else:
                    t_._bps = None
                count = _len(ps)
                m = _len(parent_grads)
                if m < count:
                    count = m
                i = 0
                while i < count:
                    p = ps[i]
                    pg = parent_grads[i]
                    i += 1
                    if p is None or pg is None or not p.requires_grad:
                        continue
                    pshape = p.shape
                    gshape = pg.shape
                    if gshape is not pshape and not _shape_eq(gshape, pshape) and _tuple(gshape) != _tuple(pshape):
                        pg = _unbroadcast(pg, pshape) if _numel(gshape) >= _numel(pshape) else expand(pg, *pshape)
                    pdt = p.dtype
                    if pg.dtype is not pdt and pg.dtype != pdt and pdt._inexact:
                        pg = _grad_as(pg, pdt)
                    prior = p._pg
                    p._pg = pg if prior is None else (add(prior, pg) if prior.__class__ is not _SparseTensor else _sp._buffer_add(prior, pg))
        finally:
            _grad_enabled = prev_mode
        return captured
    finally:
        _bwd_active = False


def _backward_dict(roots, grads, accumulate=True, inputs=None, create_graph=False):
    if _isinstance(roots, Tensor):
        roots, grads = [roots], [grads]
    order, seen = [], set()
    parents_of, aliases = {}, {}

    # Depth-first post-order without recursion (long chains of ops, an RNN
    # over many steps, must not hit the interpreter's recursion limit).
    stack = [(r, False) for r in reversed(roots)]
    while stack:
        t_, done = stack.pop()
        if done:
            order.append(t_)
            continue
        key = id(t_)
        if key in seen:
            continue
        seen.add(key)
        stack.append((t_, True))
        node = t_._node
        if node is not None:
            # The parents as this node consumed them: one given a new history
            # by an in-place op since stands in as an alias carrying the
            # history it had then (one alias per earlier version, shared by
            # every node that consumed it).
            pstate = node.pstate
            ps = []
            i = 0
            for p in node.parents:
                if p is not None:
                    st = pstate[i]
                    if p._node is not st[0]:
                        akey = (id(p), id(st[0]))
                        alias = aliases.get(akey)
                        if alias is None:
                            alias = aliases[akey] = Tensor(p._s, st[2], p.dtype, st[1], st[0])
                        p = alias
                ps.append(p)
                i += 1
            parents_of[key] = ps
            i = _len(ps) - 1
            while i >= 0:
                p = ps[i]
                if p is not None and p.requires_grad and id(p) not in seen:
                    stack.append((p, False))
                i -= 1
    pending = {}
    for r, g in zip(roots, grads):
        pending[id(r)] = g if id(r) not in pending else _sp._buffer_add(pending[id(r)], g)
    captured = {}
    wanted = None if inputs is None else set(id(t_) for t_ in inputs)
    # The grad mode for the backward functions: recording only for
    # create_graph (as `with enable_grad()` / `with no_grad()`).
    global _grad_enabled
    prev_mode = _grad_enabled
    _grad_enabled = create_graph is True or _bool(create_graph)
    try:
        for t_ in reversed(order):
            key = id(t_)
            g = pending.pop(key, None)
            if g is None:
                continue
            if t_._hooks:
                for hook in _list(t_._hooks):
                    replaced = hook(g)
                    if replaced is not None:
                        g = replaced
            if wanted is not None and key in wanted:
                captured[key] = g if key not in captured else add(captured[key], g)
                if accumulate:
                    _accumulate_grad(t_, g, create_graph)
            node = t_._node
            if node is None:
                if accumulate and wanted is None and t_.requires_grad:
                    _accumulate_grad(t_, g, create_graph)
                continue
            if t_._retain and accumulate and wanted is None:
                _accumulate_grad(t_, g, create_graph)
            if node.saved is not None:
                _check_saved(node)
            if create_graph and not (node.diff or node.name in _DIFFERENTIABLE):
                raise NotImplementedError("backward with create_graph=True through %s is not supported on Zipp" % _grad_fn_name(node.name))
            parent_grads = node.backward(g)
            if type(parent_grads) is not _tuple and type(parent_grads) is not _list:
                parent_grads = _tuple(parent_grads)
            ps = parents_of[key]
            count = _b.min(_len(ps), _len(parent_grads))
            for i in _range(count):
                p = ps[i]
                pg = parent_grads[i]
                if p is None or pg is None or not p.requires_grad:
                    continue
                if not _shape_eq(pg.shape, p.shape) and _tuple(pg.shape) != _tuple(p.shape):
                    pg = _unbroadcast(pg, p.shape) if _numel(pg.shape) >= _numel(p.shape) else expand(pg, *p.shape)
                if pg.dtype is not p.dtype and pg.dtype != p.dtype and p.dtype._inexact:
                    pg = _grad_as(pg, p.dtype)
                pk = id(p)
                prior = pending.get(pk)
                pending[pk] = pg if prior is None else _sp._buffer_add(prior, pg)
    finally:
        _grad_enabled = prev_mode
    return captured


def _autograd_grad(outputs, inputs, grad_outputs=None, retain_graph=None, create_graph=False, only_inputs=True, allow_unused=None, is_grads_batched=False, materialize_grads=False):
    outputs = [outputs] if _isinstance(outputs, Tensor) else _list(outputs)
    inputs = [inputs] if _isinstance(inputs, Tensor) else _list(inputs)
    if grad_outputs is None:
        grad_outputs = [None] * _len(outputs)
    elif _isinstance(grad_outputs, Tensor):
        grad_outputs = [grad_outputs]
    else:
        grad_outputs = _list(grad_outputs)
    for t_ in inputs:
        if not t_.requires_grad:
            raise RuntimeError("One of the differentiated Tensors does not require grad")
    roots, grads = [], []
    for i, (out, g) in enumerate(zip(outputs, grad_outputs)):
        if not out.requires_grad:
            raise RuntimeError("element %d of tensors does not require grad and does not have a grad_fn" % i)
        if g is None:
            if _numel(out.shape) != 1:
                raise RuntimeError("grad can be implicitly created only for scalar outputs")
            g = ones(*out.shape, dtype=out.dtype)
        roots.append(out)
        grads.append(g)
    captured = _backward(roots, grads, accumulate=False, inputs=inputs, create_graph=create_graph)
    result = []
    for t_ in inputs:
        g = captured.get(id(t_))
        if g is None:
            if materialize_grads:
                g = zeros_like(t_)
            elif not allow_unused:
                raise RuntimeError("One of the differentiated Tensors appears to not have been used in the graph. Set allow_unused=True if this is the desired behavior.")
        result.append(g)
    return _tuple(result)


# ---- environment knobs -------------------------------------------------------------------
_threads = 1


def set_num_threads(n):
    global _threads
    _threads = _int(n)


def get_num_threads():
    return _threads


def set_num_interop_threads(n):
    return None


def get_num_interop_threads():
    return 1


def use_deterministic_algorithms(mode, warn_only=False):
    return None


def are_deterministic_algorithms_enabled():
    return False


def set_default_dtype(d):
    global _default_dtype
    dt = _dtype_of(d)
    if not dt.is_floating_point:
        raise TypeError("only floating-point types are supported as the default type")
    _default_dtype = dt


def get_default_dtype():
    return _default_dtype


def set_default_device(device_):
    _check_cpu_device(device_)


def is_tensor(obj):
    return _isinstance(obj, Tensor)


def is_storage(obj):
    return _isinstance(obj, _Storage)


# ---- complex tensors ---------------------------------------------------------------------
# A complex64/complex128 tensor's storage holds interleaved (real, imaginary)
# float32/float64 pairs. `view_as_real`/`view_as_complex` share it (and its
# version counter); `.real`, `.imag` and `conj()` are copies, since strided
# views copy here. Every complex op is differentiable with PyTorch's
# convention for a real loss L: a complex input's gradient is
# dL/d(re) + i dL/d(im), so a holomorphic f passes grad * conj(f'(z)) back,
# and a real input of a complex result takes the real part.
#
# A complex element meets a Python value in the functions below: `_cx_parts`
# reads a Python complex (None for anything else), `_cx_scalar` makes one,
# and `_cx_real_scalar` narrows one to a real dtype as PyTorch's checked
# conversion does (a nonzero imaginary part is refused).
_PyComplex = _b.complex if _isinstance(_b.complex, type) else None


def _cx_parts(v):
    """(re, im) of a Python complex number, None for any other value."""
    if _PyComplex is not None and _isinstance(v, _PyComplex):
        return (_float(v.real), _float(v.imag))
    return None


def _cx_scalar(re, im):
    """A complex element as a Python value (item, tolist, iteration)."""
    if _PyComplex is None:
        raise NotImplementedError("complex Python scalars are not supported yet")
    return _PyComplex(re, im)


def _cx_real_scalar(v, cname):
    """The real part of a complex scalar converted to a real type `cname`."""
    if v.imag != 0:
        raise RuntimeError("value cannot be converted to type %s without overflow" % cname)
    return v.real


def _cx_scalars(s):
    """A complex storage's elements as Python values (`_cx_scalar`)."""
    flat = _k.to_list(_k.as_real(s))
    return [_cx_scalar(flat[i], flat[i + 1]) for i in _range(0, _len(flat), 2)]


def _cx_from_values(dt, flat):
    """A `dt` storage from Python numbers and complex numbers."""
    pairs = []
    for v in flat:
        p = _cx_parts(v)
        if p is None:
            pairs.append(_float(v))
            pairs.append(0.0)
        else:
            pairs.append(p[0])
            pairs.append(p[1])
    return _k.from_pairs(dt.name, pairs)


def _default_complex():
    return complex128 if _default_dtype is float64 else complex64


_complex_cast_warned = False


def _warn_complex_cast():
    # PyTorch's (once-per-process) UserWarning; this runtime has no
    # `warnings` module, so it goes to stderr as Python prints a warning.
    global _complex_cast_warned
    if not _complex_cast_warned:
        _complex_cast_warned = True
        import sys
        sys.stderr.write("UserWarning: Casting complex values to real discards the imaginary part\n")


def _as_cx(g, dt):
    """A gradient headed for a complex input: complex of `dt`."""
    return g if g.dtype is dt else _cast(g, dt)


def view_as_real(input):
    a = input
    if not a.dtype.is_complex:
        raise RuntimeError("view_as_real is only supported for complex tensors")
    out = Tensor(_k.as_real(a._s), _tuple(a.shape) + (2,), a.dtype.to_real())
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (view_as_complex(g),), (a,), "ViewAsReal")
    return out


def view_as_complex(input):
    a = input
    if a.dtype is not float32 and a.dtype is not float64:
        raise RuntimeError("view_as_complex is only supported for half, float and double tensors, but got a tensor of scalar type: %s" % _CAST_NAME[a.dtype.name])
    if not a.shape:
        raise RuntimeError("Input tensor must have one or more dimensions")
    if a.shape[-1] != 2:
        raise RuntimeError("Tensor must have a last dimension of size 2")
    cdt = a.dtype.to_complex()
    out = Tensor(_k.as_complex(a._s), _tuple(a.shape[:-1]), cdt)
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (view_as_real(_as_cx(g, cdt)),), (a,), "ViewAsComplex")
    return out


def complex(real, imag, *, out=None):
    re_, im_ = real, imag
    if not _isinstance(re_, Tensor) or not _isinstance(im_, Tensor):
        raise TypeError("complex(): argument 'real' and 'imag' must be Tensor")
    if re_.dtype is not im_.dtype:
        raise RuntimeError("Expected object of scalar type %s but got scalar type %s for second argument" % (_CAST_NAME[re_.dtype.name], _CAST_NAME[im_.dtype.name]))
    if re_.dtype is not float32 and re_.dtype is not float64:
        raise RuntimeError("Expected both inputs to be Half, Float or Double tensors but got %s and %s" % (_CAST_NAME[re_.dtype.name], _CAST_NAME[im_.dtype.name]))
    if _tuple(re_.shape) != _tuple(im_.shape):
        re_, im_ = broadcast_tensors(re_, im_)
    return view_as_complex(stack([re_, im_], -1))


def polar(abs, angle, *, out=None):
    r, th = abs, angle
    if not _isinstance(r, Tensor) or not _isinstance(th, Tensor):
        raise TypeError("polar(): argument 'abs' and 'angle' must be Tensor")
    if r.dtype is not th.dtype:
        raise RuntimeError("Expected object of scalar type %s but got scalar type %s for second argument" % (_CAST_NAME[r.dtype.name], _CAST_NAME[th.dtype.name]))
    return complex(mul(r, cos(th)), mul(r, sin(th)))


def _cx_part(a, which):
    """z.real (which 0) or z.imag (1), a copy with PyTorch's gradient."""
    out = Tensor(_k.unary("real" if which == 0 else "imag", a._s), a.shape, a.dtype.to_real())
    if _grad_enabled and a.requires_grad:
        out.requires_grad = True
        cdt = a.dtype
        if which == 0:
            backward = lambda g: (complex(g, zeros_like(g)).to(cdt),)
        else:
            backward = lambda g: (complex(zeros_like(g), g).to(cdt),)
        out._node = _Node(backward, (a,), "Select")
    return out


def real(input):
    if not input.dtype.is_complex:
        return input
    return _cx_part(input, 0)


def imag(input):
    if not input.dtype.is_complex:
        raise RuntimeError("imag is not implemented for tensors with non-complex dtypes.")
    return _cx_part(input, 1)


def conj_physical(input):
    a = input
    if not a.dtype.is_complex:
        return a
    return _cx_unary("conj", a, "ConjPhysical")


def conj(input):
    # PyTorch returns a lazy conjugate view (is_conj() True); here the
    # conjugate is computed at once, so is_conj() is always False.
    a = input
    if not a.dtype.is_complex:
        return a
    return _cx_unary("conj", a, "Conj")


def resolve_conj(input):
    return input


def resolve_neg(input):
    return input


def is_conj(input):
    return False


def is_neg(input):
    return False


# d/dz of the holomorphic unary ops, as functions of the input x and output o.
_CX_DERIV = {
    "exp": lambda x, o: o,
    "exp2": lambda x, o: mul(o, _math.log(2.0)),
    "expm1": lambda x, o: add(o, 1.0),
    "log": lambda x, o: reciprocal(x),
    "log2": lambda x, o: reciprocal(mul(x, _math.log(2.0))),
    "log10": lambda x, o: reciprocal(mul(x, _math.log(10.0))),
    "log1p": lambda x, o: reciprocal(add(x, 1.0)),
    "sqrt": lambda x, o: reciprocal(mul(o, 2.0)),
    "rsqrt": lambda x, o: mul(pow(o, 3), -0.5),
    "reciprocal": lambda x, o: neg(mul(o, o)),
    "square": lambda x, o: mul(x, 2.0),
    "sin": lambda x, o: cos(x),
    "cos": lambda x, o: neg(sin(x)),
    "tan": lambda x, o: add(mul(o, o), 1.0),
    "sinh": lambda x, o: cosh(x),
    "cosh": lambda x, o: sinh(x),
    "tanh": lambda x, o: sub(1.0, mul(o, o)),
    "sigmoid": lambda x, o: mul(o, sub(1.0, o)),
}


def _cx_unary(op, a, name):
    """A complex elementwise op (`_unary` hands complex inputs here)."""
    storage = _k.unary(op, a._s)
    out = Tensor(storage, a.shape, _DTYPES[_k.dtype(storage)])
    if _grad_enabled and a.requires_grad and out.dtype.is_complex:
        sa = a._s
        if op == "conj":
            backward = lambda g: (conj(_as_cx(g, a.dtype)),)
        elif op == "neg":
            backward = lambda g: (neg(g),)
        else:
            d = _CX_DERIV.get(op)
            if d is None:
                raise NotImplementedError("the derivative of %s is not implemented for complex tensors on Zipp" % op)
            backward = lambda g: (mul(g, conj(d(_frozen(a, sa), _frozen(out, storage)))),)
        out.requires_grad = True
        out._node = _Node(backward, (a,), name, [a, out])
    return out


def _cx_real_op(op, a):
    """A complex op with a real result (abs, angle), no history."""
    return Tensor(_k.unary(op, a._s), a.shape, a.dtype.to_real())


def _cx_abs(a):
    out = _cx_real_op("abs", a)
    if _grad_enabled and a.requires_grad:
        sa = a._s
        out.requires_grad = True
        # g * sgn(z): zero at z = 0.
        out._node = _Node(lambda g: (mul(g, _cx_sgn(_frozen(a, sa))),), (a,), "Abs", [a])
    return out


def angle(input):
    a = input
    if a.dtype.is_complex:
        out = _cx_real_op("angle", a)
        if _grad_enabled and a.requires_grad:
            sa = a._s
            out.requires_grad = True

            def backward(g):
                z = _frozen(a, sa)
                r2 = real(mul(z, conj(z)))
                # g * i z / |z|^2, zero at z = 0.
                safe = where(r2 == 0, ones_like(r2), r2)
                d = mul(z, div(g, safe))
                return (where(r2 == 0, zeros_like(d), complex(neg(imag(d)), real(d))),)
            out._node = _Node(backward, (a,), "Angle", [a])
        return out
    x = a if a.dtype.is_floating_point else a.to(_default_dtype)
    # pi for a negative number, 0 otherwise (NaN stays NaN); zero gradient.
    out = where(isnan(x), x, where(x < 0, _math.pi, 0.0)).detach()
    if _grad_enabled and x.requires_grad:
        out.requires_grad = True
        out._node = _Node(lambda g: (zeros_like(g),), (x,), "Angle")
    return out


def _cx_sgn(a):
    out = Tensor(_k.unary("sgn", a._s), a.shape, a.dtype)
    if _grad_enabled and a.requires_grad:
        sa = a._s
        so = out._s
        out.requires_grad = True

        def backward(g):
            z = _frozen(a, sa)
            s_ = _frozen(out, so)
            r = _cx_real_op("abs", z)
            safe = where(r == 0, ones_like(r), r)
            # -i sgn(z) Im(conj(g) sgn(z)) / |z|, zero at z = 0.
            t = div(imag(mul(conj(g), s_)), safe)
            d = mul(s_, t)
            return (where(r == 0, zeros_like(d), complex(imag(d), neg(real(d)))),)
        out._node = _Node(backward, (a,), "Sgn", [a, out])
    return out


# ---- windows and the short-time Fourier transform -------------------------------------------
def _window(fn_name, window_length, periodic, dtype, requires_grad):
    dt = _dtype_of(dtype) or _default_dtype
    if not dt.is_floating_point:
        raise RuntimeError("%s expects floating point dtypes, got: %s" % (fn_name, repr(dt)))
    n = _int(window_length)
    if n < 0:
        raise RuntimeError("%s requires non-negative window_length, got window_length=%d" % (fn_name, n))
    return dt, n, (n + 1 if periodic and n > 1 else n)


def _windowed(out, n, requires_grad):
    out = out[:n] if out.shape[0] != n else out
    return out.detach().requires_grad_(requires_grad) if requires_grad else out.detach()


def hamming_window(window_length, periodic=True, alpha=0.54, beta=0.46, *, dtype=None, layout=None, device=None, requires_grad=False):
    """PyTorch's formula in the window's dtype: alpha - beta cos(2 pi n / (N - 1))
    (a periodic window is the first N of N + 1)."""
    dt, n, m = _window("hamming_window", window_length, periodic, dtype, requires_grad)
    if n <= 1:
        return ones(n, dtype=dt, requires_grad=requires_grad)
    w = add(mul(cos(mul(arange(m, dtype=dt), _math.pi * 2.0 / (m - 1))), -beta), alpha)
    return _windowed(w, n, requires_grad)


def hann_window(window_length, periodic=True, *, dtype=None, layout=None, device=None, requires_grad=False):
    return hamming_window(window_length, periodic, 0.5, 0.5, dtype=dtype, requires_grad=requires_grad)


def blackman_window(window_length, periodic=True, *, dtype=None, layout=None, device=None, requires_grad=False):
    dt, n, m = _window("blackman_window", window_length, periodic, dtype, requires_grad)
    if n <= 1:
        return ones(n, dtype=dt, requires_grad=requires_grad)
    t = mul(arange(m, dtype=dt), _math.pi / (m - 1))
    w = add(sub(mul(cos(mul(t, 4)), 0.08), mul(cos(mul(t, 2)), 0.5)), 0.42)
    return _windowed(w, n, requires_grad)


def bartlett_window(window_length, periodic=True, *, dtype=None, layout=None, device=None, requires_grad=False):
    dt, n, m = _window("bartlett_window", window_length, periodic, dtype, requires_grad)
    if n <= 1:
        return ones(n, dtype=dt, requires_grad=requires_grad)
    w = mul(arange(m, dtype=dt), 2.0 / (m - 1))
    half = ((m - 1) >> 1) + 1
    w = cat([w[:half], add(mul(w[half:], -1), 2)])
    return _windowed(w, n, requires_grad)


def _stft_window(window, n_fft, win_length, dt):
    win_length = n_fft if win_length is None else _int(win_length)
    if win_length <= 0 or win_length > n_fft:
        raise RuntimeError("stft: expected 0 < win_length <= n_fft, but got win_length=%d" % win_length)
    if window is None:
        window = ones(win_length, dtype=dt)
    elif window.ndim != 1 or window.shape[0] != win_length:
        raise RuntimeError("stft: expected a 1D window tensor of size equal to win_length=%d, but got window with size %s" % (win_length, _list(window.shape)))
    if win_length < n_fft:
        left = (n_fft - win_length) // 2
        window = cat([zeros(left, dtype=window.dtype), window, zeros(n_fft - win_length - left, dtype=window.dtype)])
    return window


def _frames_index(n_frames, n_fft, hop):
    return tensor([[t * hop + j for j in _range(n_fft)] for t in _range(n_frames)], dtype=int64)


def stft(input, n_fft, hop_length=None, win_length=None, window=None, center=True, pad_mode="reflect", normalized=False, onesided=None, return_complex=None):
    """torch.stft: windowed frames of a 1-D or batched 2-D signal and their
    DFTs, (batch?, freq, frames), differentiable (the frames are gathered
    by indexing, the transforms are torch.fft's)."""
    import torch.fft as _fft
    x = input
    complex_io = x.dtype.is_complex or (window is not None and window.dtype.is_complex)
    if return_complex is None:
        if not complex_io:
            raise RuntimeError("stft requires the return_complex parameter be given for real inputs, and will further require that return_complex=True in a future PyTorch release.")
        return_complex = True
    if x.ndim not in (1, 2):
        raise RuntimeError("stft: expected a 1D or 2D tensor, but got %dD" % x.ndim)
    batched = x.ndim == 2
    if not batched:
        x = unsqueeze(x, 0)
    n_fft = _int(n_fft)
    hop = n_fft // 4 if hop_length is None else _int(hop_length)
    if hop <= 0:
        raise RuntimeError("stft: expected hop_length > 0, but got hop_length=%d" % hop)
    if center:
        pad = n_fft // 2
        x = _F.pad(x.unsqueeze(0), [pad, pad], mode=pad_mode).squeeze(0)
    L = x.shape[-1]
    if n_fft <= 0 or n_fft > L:
        raise RuntimeError("stft: expected 0 < n_fft < %d, but got n_fft=%d" % (L, n_fft))
    rdt = x.dtype.to_real() if x.dtype.is_complex else (x.dtype if x.dtype.is_floating_point else _default_dtype)
    w = _stft_window(window, n_fft, win_length, rdt)
    n_frames = 1 + (L - n_fft) // hop
    frames = x[:, _frames_index(n_frames, n_fft, hop)] * w
    norm = "ortho" if normalized else "backward"
    one = (not complex_io) if onesided is None else _bool(onesided)
    spec = _fft.rfft(frames, n_fft, -1, norm) if one else _fft.fft(frames, n_fft, -1, norm)
    spec = spec.transpose(1, 2)
    if not batched:
        spec = spec.squeeze(0)
    return spec if return_complex else view_as_real(spec)


def istft(input, n_fft, hop_length=None, win_length=None, window=None, center=True, normalized=False, onesided=None, length=None, return_complex=False):
    """torch.istft: the overlap-add inverse of stft, divided by the summed
    squared window (which must not vanish anywhere)."""
    import torch.fft as _fft
    if not input.dtype.is_complex:
        raise RuntimeError("istft requires a complex-valued input tensor matching the output from stft with return_complex=True.")
    spec = input
    batched = spec.ndim == 3
    if not batched:
        spec = unsqueeze(spec, 0)
    n_fft = _int(n_fft)
    hop = n_fft // 4 if hop_length is None else _int(hop_length)
    n_freq, n_frames = spec.shape[1], spec.shape[2]
    one = (n_freq != n_fft) if onesided is None else _bool(onesided)
    if one and n_freq != n_fft // 2 + 1:
        raise RuntimeError("istft: expected the frequency dimension (3rd to the last) of the input tensor to match n_fft / 2 + 1 when onesided=True, but got %d" % n_freq)
    rdt = spec.dtype.to_real()
    w = _stft_window(window, n_fft, win_length, rdt)
    norm = "ortho" if normalized else "backward"
    st = spec.transpose(1, 2)
    if one:
        if return_complex:
            raise RuntimeError("cannot have onesided output if window or input is complex")
        frames = _fft.irfft(st, n_fft, -1, norm)
    else:
        frames = _fft.ifft(st, n_fft, -1, norm)
        if not return_complex:
            frames = real(frames)
    frames = frames * w
    B = frames.shape[0]
    expected = n_fft + hop * (n_frames - 1)
    idx = _frames_index(n_frames, n_fft, hop).reshape(-1)
    y = zeros(B, expected, dtype=frames.dtype).index_add(1, idx, frames.reshape(B, -1))
    env = zeros(expected, dtype=w.dtype).index_add(0, idx, (w * w).repeat(n_frames))
    start = n_fft // 2 if center else 0
    end = expected - (n_fft // 2 if center else 0)
    if length is not None:
        end = _b.min(start + _int(length), expected)
    y = y[:, start:end]
    env = env[start:end]
    if env.numel() and _float(env.abs().min()) < 1e-11:
        raise RuntimeError("istft: window overlap add min: 1")
    y = y / env
    if length is not None and y.shape[1] < _int(length):
        y = cat([y, zeros(B, _int(length) - y.shape[1], dtype=y.dtype)], 1)
    return y if batched else y.squeeze(0)


def is_floating_point(input):
    return input.dtype.is_floating_point


def is_complex(input):
    return input.dtype.is_complex


def is_nonzero(input):
    return _bool(input)


def numel(input):
    return input.numel()


def clone(input, memory_format=None):
    return input.clone()


def detach(input):
    if _graph_recording and getattr(input, "_zipp_graph", False):
        return input.detach()
    return input.detach()


def select(input, dim, index):
    return input.select(dim, index)


def sigmoid_(input):
    return input.sigmoid_()


# torch.grid_sampler / torch.affine_grid_generator / torch.ctc_loss: the
# ATen ops behind F.grid_sample, F.affine_grid and F.ctc_loss, taking
# PyTorch's integer codes for the modes and the reduction.
_GRID_MODES = ("bilinear", "nearest", "bicubic")
_GRID_PADDINGS = ("zeros", "border", "reflection")
_REDUCTIONS = ("none", "mean", "sum")


def _code(codes, v, what):
    i = _int(v)
    if i < 0 or i >= _len(codes):
        raise RuntimeError("%s: invalid code %d (expected 0 to %d)" % (what, i, _len(codes) - 1))
    return codes[i]


def grid_sampler(input, grid, interpolation_mode, padding_mode, align_corners):
    return _F.grid_sample(input, grid, _code(_GRID_MODES, interpolation_mode, "grid_sampler interpolation_mode"),
                          _code(_GRID_PADDINGS, padding_mode, "grid_sampler padding_mode"), _bool(align_corners))


def affine_grid_generator(theta, size, align_corners):
    return _F.affine_grid(theta, _list(size), _bool(align_corners))


def ctc_loss(log_probs, targets, input_lengths, target_lengths, blank=0, reduction=0, zero_infinity=False):
    return _F.ctc_loss(log_probs, targets, input_lengths, target_lengths, blank, _code(_REDUCTIONS, reduction, "ctc_loss reduction"), zero_infinity)


def typename(obj):
    return obj.type() if _isinstance(obj, Tensor) else type(obj).__name__


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

    @staticmethod
    def is_bf16_supported():
        return False

    @staticmethod
    def synchronize(device=None):
        return None

    @staticmethod
    def empty_cache():
        return None


cuda = _Cuda()


class _Mps:
    @staticmethod
    def is_available():
        return False


class _Backends:
    class _Zipp:
        available = True
        description = "Zipp CPU kernels (JavaScript typed arrays on the engine)"

    zipp = _Zipp()

    class cudnn:
        deterministic = False
        benchmark = False
        enabled = True

    mps = _Mps()


backends = _Backends()


# ---- checkpoints -------------------------------------------------------------------------
class _Storage:
    """A typed storage as pickled by PyTorch; the class name is what a
    checkpoint refers to."""
    dtype = float32

    def __init__(self, storage=None):
        self._s = storage

    def element_size(self):
        return self.dtype.itemsize

    def nbytes(self):
        return _k.size(self._s) * self.dtype.itemsize


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


class HalfStorage(_Storage):
    dtype = float16


class BFloat16Storage(_Storage):
    dtype = bfloat16


class CharStorage(_Storage):
    dtype = int8


class ShortStorage(_Storage):
    dtype = int16


class ComplexFloatStorage(_Storage):
    dtype = complex64


class ComplexDoubleStorage(_Storage):
    dtype = complex128


_STORAGE_TYPES = {"float32": FloatStorage, "float64": DoubleStorage, "int64": LongStorage, "int32": IntStorage, "bool": BoolStorage, "uint8": ByteStorage,
                  "float16": HalfStorage, "bfloat16": BFloat16Storage, "int8": CharStorage, "int16": ShortStorage,
                  "complex64": ComplexFloatStorage, "complex128": ComplexDoubleStorage}


def _contiguous_strides(shape):
    out, acc = [], 1
    for d in reversed(shape):
        out.append(acc)
        acc *= d
    return _tuple(reversed(out))


def _rebuild_tensor_v2(storage, storage_offset, size, stride, requires_grad=False, backward_hooks=None, metadata=None):
    """A checkpoint tensor: the elements its (offset, strides) view of the
    storage reads, gathered into a contiguous copy. PyTorch saves a
    transpose, a column slice, a stepped slice or an expand that way."""
    shape = _tuple(_int(d) for d in size)
    strides = _tuple(_int(s) for s in stride)
    offset = _int(storage_offset)
    n = _numel(shape)
    st = storage._s
    dense = _contiguous_strides(shape)
    if strides == dense or n <= 1:
        if offset != 0 or _k.size(st) != n:
            st = _k.strided(st, shape, dense, offset)
    else:
        st = _k.strided(st, shape, strides, offset)
    t_ = Tensor(st, shape, _DTYPES[_k.dtype(st)])
    t_.requires_grad = _bool(requires_grad)
    return t_


# A checkpoint names the global by its module: PyTorch loads (and allows
# under weights_only) `torch._utils._rebuild_tensor_v2`, not `torch.…`.
_rebuild_tensor_v2.__module__ = "torch._utils"


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
        if t.__class__ is _SparseTensor:
            # As PyTorch pickles one: torch._utils._rebuild_sparse_tensor(
            # layout, (indices, values, size, is_coalesced)) for COO and
            # (crow_indices, col_indices, values, size) for CSR.
            import torch._utils
            return (torch._utils._rebuild_sparse_tensor, (t.layout, _sp._reduce_args(t)))
        if type(t) is not Tensor and type(t).__name__ == "Parameter":
            # As PyTorch pickles an nn.Parameter, so it loads back as one.
            import torch._utils
            data = Tensor(t._s, t.shape, t.dtype)
            return (torch._utils._rebuild_parameter, (data, _bool(t.requires_grad), _OrderedDict()))
        st = _STORAGE_TYPES[t.dtype.name](t._s)
        return (_rebuild_tensor_v2, (st, 0, _tuple(t.shape), t.stride(), _bool(t.requires_grad), _OrderedDict()))

    data = pickle.dumps(obj, protocol=2, persistent_id=persistent_id, reducers={Tensor: reduce_tensor, _Layout: lambda layout: (_get_layout, (repr(layout),))})
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

        # PyTorch's weights-only allowlist (torch._utils): the rebuild
        # hooks, storages, dtypes, Size, device, Parameter and plain data.
        import torch._utils
        return pickle.loads(z.read(pkl[0]), persistent_load=persistent_load, find_class=torch._utils._weights_only_find_class)


def _make_inplace(fn):
    def method(self, *args, **kwargs):
        if kwargs:
            return self._inplace_op(lambda src, *a: fn(src, *a, **kwargs), *args)
        return self._inplace_op(fn, *args)
    method.__name__ = fn.__name__ + "_"
    return method


# The in-place twins of the elementwise ops (x.exp_(), x.relu_(), ...): the
# op computed out of place, then written into the tensor (Tensor._inplace_op).
for _name, _fn in (("exp", exp), ("exp2", exp2), ("expm1", expm1), ("log", log), ("log2", log2), ("log10", log10), ("log1p", log1p),
                   ("sqrt", sqrt), ("rsqrt", rsqrt), ("neg", neg), ("negative", neg), ("abs", abs), ("absolute", abs), ("sigmoid", sigmoid),
                   ("tanh", tanh), ("relu", relu), ("floor", floor), ("ceil", ceil), ("round", round), ("trunc", trunc), ("fix", trunc),
                   ("frac", frac), ("sin", sin), ("cos", cos), ("tan", tan), ("asin", asin), ("acos", acos), ("atan", atan), ("sinh", sinh),
                   ("cosh", cosh), ("asinh", asinh), ("acosh", acosh), ("atanh", atanh), ("arcsin", asin), ("arccos", acos), ("arctan", atan),
                   ("erf", erf), ("erfc", erfc), ("erfinv", erfinv), ("reciprocal", reciprocal), ("square", square), ("sign", sign),
                   ("sgn", sign), ("logical_not", logical_not), ("bitwise_not", bitwise_not), ("atan2", atan2), ("arctan2", atan2),
                   ("float_power", float_power), ("logical_and", logical_and), ("logical_or", logical_or), ("logical_xor", logical_xor),
                   ("bitwise_and", bitwise_and), ("bitwise_or", bitwise_or), ("bitwise_xor", bitwise_xor), ("lgamma", lgamma),
                   ("digamma", digamma), ("i0", i0), ("sinc", sinc), ("xlogy", xlogy), ("igamma", igamma), ("igammac", igammac)):
    if _fn is not None and not hasattr(Tensor, _name + "_"):
        setattr(Tensor, _name + "_", _make_inplace(_fn))


Tensor.stft = stft
Tensor.istft = istft
_STORAGE_NAMES = {c.__name__: c for c in _STORAGE_TYPES.values()}
from collections import OrderedDict as _OrderedDict

# The dtype aliases that shadow builtins go last, after every use of the
# real `int`, `float` and `bool` above (module functions use the `_int`
# aliases, never the bare names).
float = float32
double = float64
long = int64
int = int32
bool = _bool_dtype
# torch.FloatTensor is the default-dtype Tensor class itself (isinstance
# holds for every tensor, as before); the others construct their dtype.
# Zipp's isinstance does not consult a metaclass __instancecheck__, so
# isinstance(t, torch.LongTensor) is False even for an int64 tensor.
FloatTensor = Tensor
DoubleTensor = _legacy_class("DoubleTensor", float64)
LongTensor = _legacy_class("LongTensor", int64)
IntTensor = _legacy_class("IntTensor", int32)
BoolTensor = _legacy_class("BoolTensor", _bool_dtype)
ByteTensor = _legacy_class("ByteTensor", uint8)
HalfTensor = _legacy_class("HalfTensor", float16)
BFloat16Tensor = _legacy_class("BFloat16Tensor", bfloat16)
CharTensor = _legacy_class("CharTensor", int8)
ShortTensor = _legacy_class("ShortTensor", int16)
_LEGACY_TYPES = {_CAST_NAME[_n] + "Tensor": _DTYPES[_n] for _n in _DTYPES}
_LEGACY_CLASSES = {Tensor: float32, DoubleTensor: float64, LongTensor: int64, IntTensor: int32, BoolTensor: _bool_dtype, ByteTensor: uint8,
                   HalfTensor: float16, BFloat16Tensor: bfloat16, CharTensor: int8, ShortTensor: int16}
inf = _math.inf
nan = _math.nan
pi = _math.pi
e = _math.e

import torch.autograd as autograd
import torch.nn as nn
import torch.optim as optim
import torch.nn.functional as _F
# The submodules PyTorch exposes as attributes after `import torch`.
# special, linalg (which installs torch.det/inverse/... and their Tensor
# methods) and amp (the real torch.autocast, is_autocast_enabled) are
# imported now; distributions, the largest, on first use (this runtime has
# no module-level __getattr__, so a stand-in forwards attribute reads to the
# real module, which replaces it as `torch.distributions` once imported).
import torch.special as special
import torch.linalg as linalg
import torch.amp as amp
# torch.sparse builds the sparse layouts; the dense ops above that accept a
# sparse operand hand it over through `_sp`.
import torch.sparse as sparse
_sp = sparse
sparse_coo_tensor = sparse._sparse_coo_tensor
sparse_csr_tensor = sparse._sparse_csr_tensor
sparse_csc_tensor = sparse._sparse_csc_tensor
sparse_bsr_tensor = sparse._sparse_bsr_tensor
sparse_bsc_tensor = sparse._sparse_bsc_tensor
sparse_compressed_tensor = sparse._sparse_compressed_tensor
smm = sparse._smm
hspmm = sparse._hspmm


class _LazySubmodule:
    def __init__(self, name):
        self._lazy_name = name

    def _lazy_load(self):
        if self._lazy_name == "fft":
            import torch.fft as module
        else:
            import torch.distributions as module
        globals()[self._lazy_name] = module
        return module

    def __getattr__(self, attr):
        if attr.startswith("_lazy_"):
            raise AttributeError(attr)
        return getattr(self._lazy_load(), attr)

    def __dir__(self):
        return dir(self._lazy_load())

    def __repr__(self):
        return repr(self._lazy_load())


distributions = _LazySubmodule("distributions")
fft = _LazySubmodule("fft")


class _Fx:
    """`torch.fx`: there is no graph tracing here, so `wrap` (which only marks
    a function as a leaf for the tracer) returns what it is given."""

    @staticmethod
    def wrap(fn_or_name):
        return fn_or_name


fx = _Fx()
