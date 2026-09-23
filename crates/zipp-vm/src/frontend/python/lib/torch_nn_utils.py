"""torch.nn.utils for Zipp."""
import math
import torch
import torch.nn.utils.rnn as rnn


def _grads(parameters):
    if isinstance(parameters, torch.Tensor):
        parameters = [parameters]
    return [p.grad for p in parameters if p.grad is not None]


def get_total_norm(tensors, norm_type=2.0, error_if_nonfinite=False, foreach=None):
    if isinstance(tensors, torch.Tensor):
        tensors = [tensors]
    tensors = list(tensors)
    if not tensors:
        return torch.tensor(0.0)
    norm_type = float(norm_type)
    if norm_type == math.inf:
        total = max(torch.abs(t).max().item() if t.numel() else 0.0 for t in tensors)
    elif norm_type == 2.0:
        total = math.sqrt(sum(float((t.double() * t.double()).sum().item()) for t in tensors))
    else:
        total = sum(float((torch.abs(t.double()) ** norm_type).sum().item()) for t in tensors) ** (1.0 / norm_type)
    if error_if_nonfinite and (math.isnan(total) or math.isinf(total)):
        raise RuntimeError("The total norm of order %s for gradients from `parameters` is non-finite, so it cannot be clipped. To disable this error and scale the gradients by the non-finite norm anyway, set `error_if_nonfinite=False`" % norm_type)
    return torch.tensor(total)


def clip_grads_with_norm_(parameters, max_norm, total_norm, foreach=None):
    grads = _grads(parameters)
    coef = float(max_norm) / (float(total_norm) + 1e-6)
    if coef < 1.0:
        with torch.no_grad():
            for g in grads:
                g.copy_(g * coef)


def clip_grad_norm_(parameters, max_norm, norm_type=2.0, error_if_nonfinite=False, foreach=None):
    if isinstance(parameters, torch.Tensor):
        parameters = [parameters]
    parameters = list(parameters)
    if error_if_nonfinite:
        get_total_norm(_grads(parameters), norm_type, True)
    return torch.clip_grad_norm_(parameters, max_norm, norm_type)


def clip_grad_value_(parameters, clip_value, foreach=None):
    if isinstance(parameters, torch.Tensor):
        parameters = [parameters]
    for p in parameters:
        if p.grad is not None:
            p.grad.clamp_(-clip_value, clip_value)


def parameters_to_vector(parameters):
    return torch.cat([p.reshape(-1) for p in parameters])


def vector_to_parameters(vec, parameters):
    start = 0
    for p in parameters:
        n = p.numel()
        with torch.no_grad():
            p.copy_(vec[start:start + n].reshape(*p.shape))
        start += n


def _norm_except_dim(v, dim):
    if dim is None or dim == -1:
        return torch.sqrt((v * v).sum())
    dims = [d for d in range(len(v.shape)) if d != dim % len(v.shape)]
    return torch.sqrt((v * v).sum(dims, keepdim=True))


class _WeightNorm:
    def __init__(self, name, dim):
        self.name = name
        self.dim = dim

    def compute_weight(self, module):
        g = getattr(module, self.name + "_g")
        v = getattr(module, self.name + "_v")
        return v * (g / _norm_except_dim(v, self.dim))

    def __call__(self, module, inputs):
        setattr(module, self.name, self.compute_weight(module))


def weight_norm(module, name="weight", dim=0):
    """The legacy `torch.nn.utils.weight_norm`: `name` becomes g * v / ||v||
    recomputed before every forward from the parameters `name_g` and `name_v`."""
    from torch.nn import Parameter
    fn = _WeightNorm(name, dim)
    weight = getattr(module, name)
    del module._parameters[name]
    module.register_parameter(name + "_g", Parameter(_norm_except_dim(weight, dim).detach()))
    module.register_parameter(name + "_v", Parameter(weight.detach()))
    setattr(module, name, fn.compute_weight(module))
    handle = module.register_forward_pre_hook(fn)
    module.__dict__.setdefault("_weight_norm_hooks", {})[name] = (fn, handle)
    return module


def remove_weight_norm(module, name="weight"):
    from torch.nn import Parameter
    hooks = module.__dict__.get("_weight_norm_hooks", {})
    if name not in hooks:
        raise ValueError("weight_norm of '%s' not found in %s" % (name, module))
    fn, handle = hooks.pop(name)
    weight = fn.compute_weight(module).detach()
    handle.remove()
    delattr(module, name + "_g")
    delattr(module, name + "_v")
    if name in module.__dict__:
        del module.__dict__[name]
    module.register_parameter(name, Parameter(weight))
    return module
