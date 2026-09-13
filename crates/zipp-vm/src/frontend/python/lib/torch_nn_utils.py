"""torch.nn.utils for Zipp."""
import torch


def clip_grad_norm_(parameters, max_norm, norm_type=2.0, error_if_nonfinite=False, foreach=None):
    return torch.clip_grad_norm_(parameters, max_norm, norm_type)


def clip_grad_value_(parameters, clip_value):
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
