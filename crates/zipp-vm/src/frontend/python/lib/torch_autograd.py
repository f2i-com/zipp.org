"""torch.autograd for Zipp: the engine lives in `torch`; this module names it."""
import torch

grad = torch.autograd.grad
no_grad = torch.no_grad
enable_grad = torch.enable_grad
set_grad_enabled = torch.set_grad_enabled
is_grad_enabled = torch.is_grad_enabled


def backward(tensors, grad_tensors=None):
    tensors = [tensors] if isinstance(tensors, torch.Tensor) else list(tensors)
    grads = [None] * len(tensors) if grad_tensors is None else list(grad_tensors)
    for t, g in zip(tensors, grads):
        t.backward(g)


class Function:
    """Custom autograd functions: subclass with static `forward(ctx, ...)`
    and `backward(ctx, grad)`; call through `apply`."""

    @classmethod
    def apply(cls, *args):
        ctx = _Context()
        with torch.no_grad():
            out = cls.forward(ctx, *args)
        inputs = [a for a in args if isinstance(a, torch.Tensor)]
        if torch._needs_grad(*inputs):
            def backward(g):
                grads = cls.backward(ctx, g)
                grads = (grads,) if not isinstance(grads, tuple) else grads
                # Align with the tensor arguments only.
                out_grads, k = [], 0
                for a in args:
                    if isinstance(a, torch.Tensor):
                        out_grads.append(grads[k] if k < len(grads) else None)
                    k += 1
                return tuple(out_grads)
            out.requires_grad = True
            out._node = torch._Node(backward, tuple(inputs), cls.__name__)
        return out


class _Context:
    def __init__(self):
        self.saved_tensors = ()

    def save_for_backward(self, *tensors):
        self.saved_tensors = tensors
