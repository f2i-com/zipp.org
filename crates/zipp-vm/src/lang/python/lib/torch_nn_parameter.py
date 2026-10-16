"""torch.nn.parameter for Zipp: Parameter, Buffer and the uninitialized
placeholders lazy modules hold until their first forward."""
import torch
from torch import Tensor


class Parameter(Tensor):
    """A tensor a Module registers as a parameter when assigned as an attribute."""

    def __init__(self, data=None, requires_grad=True):
        if data is None:
            data = torch.empty(0)
        Tensor.__init__(self, data._s, data.shape, data.dtype, requires_grad)

    def __deepcopy__(self, memo):
        if id(self) in memo:
            return memo[id(self)]
        result = type(self)(self.data.clone(), self.requires_grad)
        if self.grad is not None:
            result.grad = self.grad.clone()
        memo[id(self)] = result
        return result

    def __repr__(self):
        return "Parameter containing:\n" + Tensor.__repr__(self)

    __str__ = __repr__


class Buffer(Tensor):
    """`nn.Buffer(t)`: a tensor sharing t's storage, marked so that assigning
    it to a Module attribute registers it as a buffer (persistent unless told
    otherwise). PyTorch returns a plain Tensor and answers isinstance through
    a metaclass; this runtime does not consult metaclass __instancecheck__,
    so the result is a Buffer instance instead (operations on it return plain
    tensors, and it pickles as one)."""

    def __init__(self, data=None, *, persistent=True):
        if data is None:
            data = torch.empty(0)
        Tensor.__init__(self, data._s, data.shape, data.dtype, data.requires_grad)
        self.persistent = persistent
        self._is_buffer = True


_UNINITIALIZED_SHAPE = ("Can't access the shape of an uninitialized parameter or buffer. "
                        "This error usually happens in `load_state_dict` when trying to load "
                        "an uninitialized parameter into an initialized one. "
                        "Call `forward` to initialize the parameters before accessing their attributes.")


class UninitializedTensorMixin:
    """A parameter or buffer without data or shape yet. `materialize(shape)`
    gives it storage and turns the same object into `cls_to_become`, so
    optimizers and modules holding it see the materialized tensor."""
    cls_to_become = None
    _uninitialized = True

    def _init_uninitialized(self, requires_grad, dtype):
        torch_dtype = torch.get_default_dtype() if dtype is None else dtype
        empty = torch.empty(0, dtype=torch_dtype)
        self.__dict__["_s"] = empty._s
        self.__dict__["dtype"] = torch_dtype
        if requires_grad:
            self.requires_grad = True

    def materialize(self, shape, device=None, dtype=None):
        torch._check_cpu_device(device)
        if dtype is None:
            dtype = self.dtype
        shape = tuple(shape) if isinstance(shape, (tuple, list, torch.Size)) else (shape,)
        data = torch.empty(*shape, dtype=dtype)
        self.__class__ = self.cls_to_become
        self.data = data

    @property
    def shape(self):
        raise RuntimeError(_UNINITIALIZED_SHAPE)

    @property
    def data(self):
        raise ValueError("Attempted to use an uninitialized parameter in <data>. This error happens when you are using a `LazyModule` or explicitly manipulating `torch.nn.parameter.%s` objects. When using LazyModules Call `forward` with a dummy batch to initialize the parameters before calling torch functions" % type(self).__name__)

    @data.setter
    def data(self, value):
        Tensor.data.fset(self, value)

    def size(self, dim=None):
        # Allowed by PyTorch: the size of the empty placeholder.
        return torch.Size([0]) if dim is None else torch.Size([0])[dim]

    def numel(self):
        raise ValueError("Attempted to use an uninitialized parameter in <method 'numel' of 'torch._C.TensorBase' objects>. This error happens when you are using a `LazyModule` or explicitly manipulating `torch.nn.parameter.%s` objects. When using LazyModules Call `forward` with a dummy batch to initialize the parameters before calling torch functions" % type(self).__name__)

    nelement = numel

    def share_memory_(self):
        raise RuntimeError("Can't share memory on an uninitialized parameter or buffer. Call `forward` to initialize the parameters before calling `module.share_memory()`.")

    def __repr__(self):
        return "<%s>" % type(self).__name__

    __str__ = __repr__

    def __hash__(self):
        return id(self)

    # The conversions PyTorch allows on an uninitialized tensor: the result
    # is uninitialized too, with the new dtype.
    def _converted(self, dtype):
        if dtype is None or dtype == self.dtype:
            return self
        if isinstance(self, UninitializedParameter):
            return type(self)(self.requires_grad, None, dtype)
        return type(self)(self.requires_grad, None, dtype, self.__dict__.get("persistent", True))

    def to(self, *args, **kwargs):
        dtype = kwargs.get("dtype")
        torch._check_cpu_device(kwargs.get("device"))
        for a in args:
            if isinstance(a, torch.dtype):
                dtype = a
            elif isinstance(a, Tensor):
                dtype = a.dtype
            elif isinstance(a, (str, torch.device)):
                torch._check_cpu_device(a)
        return self._converted(dtype)

    def float(self, memory_format=None):
        return self._converted(torch.float32)

    def double(self, memory_format=None):
        return self._converted(torch.float64)

    def half(self, memory_format=None):
        return self._converted(torch.float16)

    def bfloat16(self, memory_format=None):
        return self._converted(torch.bfloat16)

    def cpu(self, memory_format=None):
        return self


def is_lazy(param):
    return isinstance(param, UninitializedTensorMixin)


def _uninitialized_error(what):
    def fail(self, *args, **kwargs):
        raise ValueError("Attempted to use an uninitialized parameter in %s. This error happens when you are using a `LazyModule` or explicitly manipulating `torch.nn.parameter.%s` objects. When using LazyModules Call `forward` with a dummy batch to initialize the parameters before calling torch functions" % (what, type(self).__name__))
    return fail


# As PyTorch's __torch_function__ does, every tensor operation on an
# uninitialized parameter or buffer raises ValueError, except the ones it
# allows (size, the dtype conversions, copy_, ...). Operators report the
# method they dispatch to. (A torch.* function reading `.shape` still gets
# the RuntimeError that attribute raises.)
_UNINITIALIZED_OPERATORS = {
    "__add__": "add", "__radd__": "add", "__iadd__": "add_", "__sub__": "sub", "__rsub__": "rsub",
    "__isub__": "sub_", "__mul__": "mul", "__rmul__": "mul", "__imul__": "mul_", "__truediv__": "div",
    "__rtruediv__": "__rdiv__", "__itruediv__": "div_", "__floordiv__": "__floordiv__",
    "__rfloordiv__": "__rfloordiv__", "__ifloordiv__": "__ifloordiv__", "__mod__": "remainder",
    "__rmod__": "__rmod__", "__imod__": "remainder_", "__pow__": "pow", "__rpow__": "__rpow__",
    "__ipow__": "pow_", "__matmul__": "matmul", "__rmatmul__": "__rmatmul__", "__neg__": "neg",
    "__pos__": "positive", "__abs__": "abs", "__invert__": "bitwise_not", "__and__": "__and__",
    "__rand__": "__rand__", "__or__": "__or__", "__ror__": "__ror__", "__xor__": "__xor__",
    "__rxor__": "__rxor__", "__lshift__": "__lshift__", "__rshift__": "__rshift__",
    "__eq__": "__eq__", "__ne__": "__ne__", "__lt__": "lt", "__le__": "le", "__gt__": "gt",
    "__ge__": "ge", "__bool__": "__bool__", "__float__": "__float__", "__int__": "__int__",
    "__index__": "__index__", "__len__": "__len__", "__iter__": "dim", "__getitem__": "__getitem__",
    "__setitem__": "__setitem__", "__array__": "__array__",
}
_UNINITIALIZED_ALLOWED = frozenset(["size", "copy_", "is_complex", "is_floating_point", "half", "float", "double", "bfloat16", "char", "short", "int", "long", "cuda", "cpu", "to", "get_device", "type", "materialize", "share_memory_", "numel", "data"])


def _guard_uninitialized():
    for name, what in _UNINITIALIZED_OPERATORS.items():
        if name not in UninitializedTensorMixin.__dict__:
            label = "<slot wrapper '%s' of 'torch._C.TensorBase' objects>" % name if name in ("__getitem__", "__setitem__") else "<method '%s' of 'torch._C.TensorBase' objects>" % what
            setattr(UninitializedTensorMixin, name, _uninitialized_error(label))
    for name in dir(Tensor):
        if name.startswith("_") or name in _UNINITIALIZED_ALLOWED or name in UninitializedTensorMixin.__dict__:
            continue
        value = Tensor.__dict__.get(name)
        if value is None or isinstance(value, (property, type, staticmethod, classmethod)) or not callable(value):
            continue
        setattr(UninitializedTensorMixin, name, _uninitialized_error("<method '%s' of 'torch._C.TensorBase' objects>" % name))


_guard_uninitialized()


class UninitializedParameter(UninitializedTensorMixin, Parameter):
    cls_to_become = Parameter

    def __init__(self, requires_grad=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        self._init_uninitialized(requires_grad, dtype)

    def __deepcopy__(self, memo):
        if id(self) in memo:
            return memo[id(self)]
        result = type(self)(self.requires_grad, None, self.dtype)
        memo[id(self)] = result
        return result


class UninitializedBuffer(UninitializedTensorMixin, Tensor):
    cls_to_become = Tensor

    def __init__(self, requires_grad=False, device=None, dtype=None, persistent=True):
        torch._check_cpu_device(device)
        self._init_uninitialized(requires_grad, dtype)
        self.persistent = persistent
        self._is_buffer = True

    def __deepcopy__(self, memo):
        if id(self) in memo:
            return memo[id(self)]
        result = type(self)(self.requires_grad, None, self.dtype, self.__dict__.get("persistent", True))
        memo[id(self)] = result
        return result
