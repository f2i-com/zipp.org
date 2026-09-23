"""torch.nn.utils.parametrize for Zipp.

`register_parametrization(module, "weight", p)` moves the tensor into
`module.parametrizations.weight.original` (the same Parameter object, so an
optimizer keeps training it), swaps the module's class for a generated
`Parametrized<Class>` subclass and gives that class a `weight` property
computing `p(original)` on every access. State dicts therefore carry
PyTorch's `parametrizations.<name>.original` layout.
"""
from contextlib import contextmanager
import torch
from torch import Tensor
from torch.nn import Module, ModuleDict, ModuleList, Parameter

__all__ = ["cached", "ParametrizationList", "register_parametrization", "is_parametrized", "remove_parametrizations", "type_before_parametrizations", "transfer_parametrizations_and_params"]

_cache_enabled = 0
_cache = {}


@contextmanager
def cached():
    """Within this context each parametrized tensor is computed once, on its
    first access, and reused."""
    global _cache
    global _cache_enabled
    _cache_enabled += 1
    try:
        yield
    finally:
        _cache_enabled -= 1
        if not _cache_enabled:
            _cache = {}


def _register_parameter_or_buffer(module, name, X):
    if isinstance(X, Parameter):
        module.register_parameter(name, X)
    else:
        module.register_buffer(name, X)


def _maybe_set(dest, src):
    # PyTorch's dest.set_(src): the tensor object keeps its identity and
    # takes src's storage, shape and dtype.
    dest.data = src


def _is_sequence(value):
    return isinstance(value, (list, tuple))


class ParametrizationList(ModuleList):
    """The chain of parametrizations of one tensor and its unconstrained
    `original` (or `original0`, `original1`, ... when `right_inverse`
    returns several tensors)."""

    def __init__(self, modules, original, unsafe=False):
        if len(modules) == 0:
            raise ValueError("ParametrizationList requires one or more modules.")
        super().__init__(modules)
        self.unsafe = unsafe
        original_shape = tuple(original.shape)
        original_dtype = original.dtype
        with torch.no_grad():
            new = original
            for module in reversed(list(self)):
                if hasattr(module, "right_inverse"):
                    try:
                        new = module.right_inverse(new)
                    except NotImplementedError:
                        pass
        if not isinstance(new, Tensor) and not _is_sequence(new):
            raise ValueError("'right_inverse' must return a Tensor or a Sequence of tensors (list, tuple...). Got %s" % type(new).__name__)
        self.is_tensor = isinstance(new, Tensor)
        self.ntensors = 1 if self.is_tensor else len(new)
        if self.is_tensor:
            if original.dtype != new.dtype:
                raise ValueError("When `right_inverse` outputs one tensor, it may not change the dtype.\noriginal.dtype: %s\nright_inverse(original).dtype: %s" % (original.dtype, new.dtype))
            if new is not original:
                with torch.no_grad():
                    _maybe_set(original, new)
            _register_parameter_or_buffer(self, "original", original)
        else:
            for i, originali in enumerate(new):
                if not isinstance(originali, Tensor):
                    raise ValueError("'right_inverse' must return a Tensor or a Sequence of tensors (list, tuple...). Got element %d of the sequence with type %s." % (i, type(originali).__name__))
                if isinstance(original, Parameter):
                    originali = Parameter(originali.detach(), original.requires_grad)
                originali.requires_grad_(original.requires_grad)
                _register_parameter_or_buffer(self, "original%d" % i, originali)
        if not self.unsafe:
            Z = self()
            if not isinstance(Z, Tensor):
                raise ValueError("A parametrization must return a tensor. Got %s." % type(Z).__name__)
            if Z.dtype != original_dtype:
                raise ValueError("Registering a parametrization may not change the dtype of the tensor, unless `unsafe` flag is enabled.\nunparametrized dtype: %s\nparametrized dtype: %s" % (original_dtype, Z.dtype))
            if tuple(Z.shape) != original_shape:
                raise ValueError("Registering a parametrization may not change the shape of the tensor, unless `unsafe` flag is enabled.\nunparametrized shape: %s\nparametrized shape: %s" % (torch.Size(original_shape), Z.shape))

    def right_inverse(self, value):
        with torch.no_grad():
            for module in reversed(list(self)):
                if hasattr(module, "right_inverse"):
                    value = module.right_inverse(value)
                else:
                    raise RuntimeError("parametrization %s does not implement right_inverse." % type(module).__name__)
            if self.is_tensor:
                if not isinstance(value, Tensor):
                    raise ValueError("`right_inverse` should return a tensor. Got %s" % type(value).__name__)
                if value.dtype != self.original.dtype:
                    raise ValueError("The tensor returned by `right_inverse` has dtype %s while `original` has dtype %s" % (value.dtype, self.original.dtype))
                _maybe_set(self.original, value)
            else:
                if not _is_sequence(value):
                    raise ValueError("'right_inverse' must return a sequence of tensors. Got %s." % type(value).__name__)
                if len(value) != self.ntensors:
                    raise ValueError("'right_inverse' must return a sequence of tensors of length %d. Got a sequence of length %d." % (self.ntensors, len(value)))
                for i, tensor in enumerate(value):
                    original_i = getattr(self, "original%d" % i)
                    if not isinstance(tensor, Tensor):
                        raise ValueError("`right_inverse` must return a sequence of tensors. Got element %d of type %s" % (i, type(tensor).__name__))
                    if original_i.dtype != tensor.dtype:
                        raise ValueError("Tensor %d returned by `right_inverse` has dtype %s while `original%d` has dtype %s" % (i, tensor.dtype, i, original_i.dtype))
                    _maybe_set(original_i, tensor)

    def forward(self):
        if self.is_tensor:
            x = self[0](self.original)
        else:
            x = self[0](*[getattr(self, "original%d" % i) for i in range(self.ntensors)])
        curr_idx = 1
        while hasattr(self, str(curr_idx)):
            x = self[curr_idx](x)
            curr_idx += 1
        return x


def _inject_new_class(module):
    cls = module.__class__

    def getstate(self):
        raise RuntimeError("Serialization of parametrized modules is only supported through state_dict(). See:\nhttps://pytorch.org/tutorials/beginner/saving_loading_models.html#saving-loading-a-general-checkpoint-for-inference-and-or-resuming-training")
    param_cls = type("Parametrized" + cls.__name__, (cls,), {"__getstate__": getstate})
    module.__class__ = param_cls


def _inject_property(module, tensor_name):
    if hasattr(module, tensor_name):
        raise AssertionError("Module already has an attribute named '%s'" % tensor_name)

    def get_cached_parametrization(parametrization):
        key = (id(module), tensor_name)
        tensor = _cache.get(key)
        if tensor is None:
            tensor = parametrization()
            _cache[key] = tensor
        return tensor

    def get_parametrized(self):
        parametrization = self.parametrizations[tensor_name]
        if _cache_enabled:
            return get_cached_parametrization(parametrization)
        return parametrization()

    def set_original(self, value):
        self.parametrizations[tensor_name].right_inverse(value)
    setattr(module.__class__, tensor_name, property(get_parametrized, set_original))


def register_parametrization(module, tensor_name, parametrization, *, unsafe=False):
    parametrization.train(module.training)
    if is_parametrized(module, tensor_name):
        if not unsafe:
            Y = getattr(module, tensor_name)
            X = parametrization(Y)
            if not isinstance(X, Tensor):
                raise ValueError("A parametrization must return a tensor. Got %s." % type(X).__name__)
            if X.dtype != Y.dtype:
                raise ValueError("Registering a parametrization may not change the dtype of the tensor, unless the `unsafe` flag is enabled.\nmodule.%s.dtype: %s\nparametrization(module.%s).dtype: %s" % (tensor_name, Y.dtype, tensor_name, X.dtype))
            if tuple(X.shape) != tuple(Y.shape):
                raise ValueError("Registering a parametrization may not change the shape of the tensor, unless the `unsafe` flag is enabled.\nmodule.%s.shape: %s\nparametrization(module.%s).shape: %s" % (tensor_name, Y.shape, tensor_name, X.shape))
            if hasattr(parametrization, "right_inverse"):
                try:
                    Z = parametrization.right_inverse(X)
                except NotImplementedError:
                    pass
                else:
                    if not isinstance(Z, Tensor):
                        raise ValueError("parametrization.right_inverse must return a tensor. Got: %s" % type(Z).__name__)
                    if Z.dtype != Y.dtype:
                        raise ValueError("The tensor returned by parametrization.right_inverse must have the same dtype as module.%s, unless the `unsafe` flag is enabled.\nmodule.%s.dtype: %s\nreturned dtype: %s" % (tensor_name, tensor_name, Y.dtype, Z.dtype))
                    if tuple(Z.shape) != tuple(Y.shape):
                        raise ValueError("The tensor returned by parametrization.right_inverse must have the same shape as module.%s, unless the `unsafe` flag is enabled.\nmodule.%s.shape: %s\nreturned shape: %s" % (tensor_name, tensor_name, Y.shape, Z.shape))
        module.parametrizations[tensor_name].append(parametrization)
        module.parametrizations[tensor_name].unsafe |= unsafe
    elif tensor_name in module._buffers or tensor_name in module._parameters:
        original = getattr(module, tensor_name)
        parametrizations = ParametrizationList([parametrization], original, unsafe=unsafe)
        delattr(module, tensor_name)
        if not is_parametrized(module):
            _inject_new_class(module)
            module.parametrizations = ModuleDict()
        _inject_property(module, tensor_name)
        module.parametrizations[tensor_name] = parametrizations
    else:
        raise ValueError("Module '%s' does not have a parameter, a buffer, or a parametrized element with name '%s'" % (module, tensor_name))
    return module


def is_parametrized(module, tensor_name=None):
    parametrizations = module.__dict__.get("_modules", {}).get("parametrizations")
    if parametrizations is None or not isinstance(parametrizations, ModuleDict):
        return False
    if tensor_name is None:
        return len(parametrizations) > 0
    return tensor_name in parametrizations


def remove_parametrizations(module, tensor_name, leave_parametrized=True):
    if not is_parametrized(module, tensor_name):
        raise ValueError("Module %s does not have a parametrization on %s" % (module, tensor_name))
    parametrizations = module.parametrizations[tensor_name]
    if parametrizations.is_tensor:
        original = parametrizations.original
        if leave_parametrized:
            with torch.no_grad():
                t = getattr(module, tensor_name)
            with torch.no_grad():
                _maybe_set(original, t.detach())
    else:
        if leave_parametrized:
            t = getattr(module, tensor_name)
            original = Parameter(t.detach()) if t.requires_grad else t.detach()
        else:
            raise ValueError("Cannot leave unparametrized (`leave_parametrized=False`) a tensor that is parametrized in terms of a sequence of tensors.")
    delattr(module.__class__, tensor_name)
    del module.parametrizations[tensor_name]
    _register_parameter_or_buffer(module, tensor_name, original)
    if not is_parametrized(module):
        delattr(module, "parametrizations")
        module.__class__ = module.__class__.__bases__[0]
    return module


def type_before_parametrizations(module):
    if is_parametrized(module):
        return module.__class__.__bases__[0]
    return type(module)


def transfer_parametrizations_and_params(from_module, to_module, tensor_name=None):
    if is_parametrized(from_module):
        names = list(from_module.parametrizations.keys()) if tensor_name is None else [tensor_name]
        for parameter_name in names:
            if not hasattr(to_module, parameter_name):
                setattr(to_module, parameter_name, Parameter(getattr(from_module, parameter_name).detach()))
            for param_func in from_module.parametrizations[parameter_name]:
                register_parametrization(to_module, parameter_name, param_func)
            source = from_module.parametrizations[parameter_name]
            target = to_module.parametrizations[parameter_name]
            if hasattr(source, "original"):
                target.original = source.original
            else:
                num = 0
                while hasattr(source, "original%d" % num):
                    setattr(target, "original%d" % num, getattr(source, "original%d" % num))
                    num += 1
    return to_module
