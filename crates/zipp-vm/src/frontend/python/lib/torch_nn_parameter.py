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
        raise RuntimeError(_UNINITIALIZED_SHAPE)

    def numel(self):
        raise ValueError("Attempted to use an uninitialized parameter in <method 'numel'>. This error happens when you are using a `LazyModule` or explicitly manipulating `torch.nn.parameter.%s` objects. When using LazyModules Call `forward` with a dummy batch to initialize the parameters before calling torch functions" % type(self).__name__)

    def share_memory_(self):
        raise RuntimeError("Can't share memory on an uninitialized parameter or buffer. Call `forward` to initialize the parameters before calling `module.share_memory()`.")

    def __repr__(self):
        return "<%s>" % type(self).__name__

    __str__ = __repr__


def is_lazy(param):
    return isinstance(param, UninitializedTensorMixin)


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
