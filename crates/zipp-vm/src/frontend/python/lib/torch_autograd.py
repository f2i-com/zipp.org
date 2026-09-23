"""torch.autograd for Zipp: the engine lives in `torch`; this module names it.

`torch` imports this module at the end of its own import, so everything
here reads the engine from `torch`'s private names, which exist by then.
"""
import torch

grad = torch._autograd_grad
no_grad = torch.no_grad
enable_grad = torch.enable_grad
set_grad_enabled = torch.set_grad_enabled
is_grad_enabled = torch.is_grad_enabled
inference_mode = torch.inference_mode


def backward(tensors, grad_tensors=None, retain_graph=None, create_graph=False, grad_variables=None, inputs=None):
    tensors = [tensors] if isinstance(tensors, torch.Tensor) else list(tensors)
    if grad_tensors is None:
        grad_tensors = grad_variables
    if grad_tensors is None:
        grads = [None] * len(tensors)
    elif isinstance(grad_tensors, torch.Tensor):
        grads = [grad_tensors]
    else:
        grads = list(grad_tensors)
    torch._run_backward(tensors, grads, create_graph, inputs)


def Variable(data, requires_grad=False, volatile=False):
    """The pre-0.4 wrapper: a Variable is a tensor."""
    return data.requires_grad_(requires_grad) if requires_grad else data


class _Functional:
    """torch.autograd.functional: jacobian and hessian by repeated backward."""

    @staticmethod
    def vjp(func, inputs, v=None, create_graph=False, strict=False):
        single = isinstance(inputs, torch.Tensor)
        xs = [inputs] if single else list(inputs)
        xs = [x.detach().requires_grad_(True) for x in xs]
        with torch.enable_grad():
            out = func(*xs)
        grads = torch._autograd_grad(out, xs, v, create_graph=create_graph, allow_unused=True, materialize_grads=True)
        return (out.detach() if not create_graph else out), (grads[0] if single else grads)

    @staticmethod
    def jacobian(func, inputs, create_graph=False, strict=False, vectorize=False, strategy="reverse-mode"):
        single = isinstance(inputs, torch.Tensor)
        xs = [inputs] if single else list(inputs)
        xs = [x.detach().requires_grad_(True) for x in xs]
        with torch.enable_grad():
            out = func(*xs)
        flat = out.reshape(-1)
        rows = [[] for _ in xs]
        for i in range(flat.numel()):
            gs = torch._autograd_grad(flat[i], xs, retain_graph=True, create_graph=create_graph, allow_unused=True, materialize_grads=True)
            for k, g in enumerate(gs):
                rows[k].append(g)
        jac = [torch.stack(r, 0).reshape(*(tuple(out.shape) + tuple(x.shape))) if r else torch.zeros(*(tuple(out.shape) + tuple(x.shape))) for r, x in zip(rows, xs)]
        return jac[0] if single else tuple(jac)

    @staticmethod
    def hessian(func, inputs, create_graph=False, strict=False, vectorize=False, outer_jacobian_strategy="reverse-mode"):
        single = isinstance(inputs, torch.Tensor)
        if not single:
            raise NotImplementedError("torch.autograd.functional.hessian on Zipp takes a single input tensor")

        def gradient(x):
            with torch.enable_grad():
                y = func(x)
                return torch._autograd_grad(y, x, create_graph=True)[0]
        return _Functional.jacobian(gradient, inputs, create_graph=create_graph)


functional = _Functional()


class Function:
    """Custom autograd functions: subclass with static `forward(ctx, ...)`
    and `backward(ctx, *grads)`; call through `apply`. Several outputs,
    `ctx.save_for_backward` (checked against in-place writes, as PyTorch),
    `ctx.needs_input_grad` and non-tensor arguments are supported."""

    @classmethod
    def apply(cls, *args, **kwargs):
        ctx = FunctionCtx()
        ctx.needs_input_grad = tuple(isinstance(a, torch.Tensor) and a.requires_grad for a in args)
        with torch.no_grad():
            result = cls.forward(ctx, *args, **kwargs)
        single = not isinstance(result, tuple)
        outs = [result] if single else list(result)
        inputs = [a for a in args if isinstance(a, torch.Tensor)]
        if torch._needs_grad(*inputs):
            tensor_outs = [i for i, o in enumerate(outs) if isinstance(o, torch.Tensor)]
            for i in tensor_outs:
                o = outs[i]
                if any(o is a for a in args):
                    # Returning an input unchanged: a new tensor sharing its
                    # storage carries the history, not the input itself.
                    o = outs[i] = torch.Tensor(o._s, o.shape, o.dtype)
            differentiable = [i for i in tensor_outs if outs[i].dtype.is_floating_point and id(outs[i]) not in ctx._non_differentiable]
            shapes = {i: (tuple(outs[i].shape), outs[i].dtype) for i in tensor_outs}
            for i in differentiable:
                out = outs[i]

                def backward(g, i=i):
                    # Each output's gradient runs backward with zeros for
                    # the other outputs (gradients are linear, so the sum
                    # over outputs is the whole gradient).
                    grads = []
                    for j in range(len(outs)):
                        if j == i:
                            grads.append(g)
                        elif j in shapes:
                            grads.append(torch.zeros(*shapes[j][0], dtype=shapes[j][1]) if ctx._materialize else None)
                        else:
                            grads.append(None)
                    got = cls.backward(ctx, *grads)
                    got = (got,) if not isinstance(got, tuple) else got
                    # Align with the tensor arguments only.
                    out_grads = []
                    for k, a in enumerate(args):
                        if isinstance(a, torch.Tensor):
                            out_grads.append(got[k] if k < len(got) else None)
                    return tuple(out_grads)
                out.requires_grad = True
                node = torch._Node(backward, tuple(inputs), cls.__name__ + "Backward")
                node.diff = True
                out._node = node
        return outs[0] if single else tuple(outs)


class FunctionCtx:
    def __init__(self):
        self._saved = ()
        self._non_differentiable = set()
        self._materialize = True
        self.needs_input_grad = ()

    def save_for_backward(self, *tensors):
        self._saved = tuple((t, None if t is None else torch._k.aversion(t._s)) for t in tensors)

    @property
    def saved_tensors(self):
        out = []
        for t, version in self._saved:
            if t is not None and torch._k.aversion(t._s) != version:
                node = torch._Node(None, (), "Saved")
                node.saved = [(t._s, version, t)]
                torch._check_saved(node)
            out.append(t)
        return tuple(out)

    def mark_non_differentiable(self, *tensors):
        for t in tensors:
            self._non_differentiable.add(id(t))

    def mark_dirty(self, *tensors):
        return None

    def set_materialize_grads(self, value):
        self._materialize = bool(value)


_Context = FunctionCtx


class _SavedHooks:
    def __init__(self, *args, **kwargs):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class _Profiler:
    profile = _SavedHooks
    record_function = _SavedHooks


profiler = _Profiler()


def set_detect_anomaly(mode, check_nan=True):
    return None


class detect_anomaly(_SavedHooks):
    pass
