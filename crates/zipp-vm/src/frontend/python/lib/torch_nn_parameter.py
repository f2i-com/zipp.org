"""torch.nn.parameter for Zipp: Parameter and Buffer."""
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


class Buffer:
    """`nn.Buffer(t)`: a plain tensor marked so that assigning it to a Module
    attribute registers it as a buffer (persistent unless told otherwise)."""

    def __new__(cls, data=None, persistent=True):
        if data is None:
            data = torch.empty(0)
        t = data.detach().requires_grad_(data.requires_grad)
        t.persistent = persistent
        t._is_buffer = True
        return t


def is_lazy(param):
    return False
