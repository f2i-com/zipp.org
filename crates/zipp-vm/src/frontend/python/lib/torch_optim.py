"""torch.optim for Zipp: SGD, Adam and AdamW over the tensor library."""
import math
import torch


class Optimizer:
    def __init__(self, params, defaults):
        params = list(params)
        if params and isinstance(params[0], dict):
            self.param_groups = [dict(defaults, **g) for g in params]
            for g in self.param_groups:
                g["params"] = list(g["params"])
        else:
            self.param_groups = [dict(defaults, params=params)]
        self.defaults = defaults
        self.state = {}

    def zero_grad(self, set_to_none=True):
        from torch._gpu import active_capture
        capture = active_capture()
        if capture is not None:
            return capture.zero_grad(self, set_to_none)
        for g in self.param_groups:
            for p in g["params"]:
                if set_to_none:
                    p.grad = None
                elif p.grad is not None:
                    p.grad.zero_()

    def state_dict(self):
        return {"state": self.state, "param_groups": [{k: v for k, v in g.items() if k != "params"} for g in self.param_groups]}

    def load_state_dict(self, sd):
        for g, saved in zip(self.param_groups, sd.get("param_groups", [])):
            g.update(saved)

    def add_param_group(self, group):
        self.param_groups.append(dict(self.defaults, **group))

    def _params(self):
        for g in self.param_groups:
            for p in g["params"]:
                if p.grad is not None:
                    yield g, p

    def __repr__(self):
        return "%s(%s)" % (type(self).__name__, ", ".join("%s=%r" % (k, v) for k, v in self.defaults.items()))


class SGD(Optimizer):
    def __init__(self, params, lr=1e-3, momentum=0.0, dampening=0.0, weight_decay=0.0, nesterov=False, maximize=False):
        super().__init__(params, dict(lr=lr, momentum=momentum, dampening=dampening, weight_decay=weight_decay, nesterov=nesterov, maximize=maximize))

    @torch.no_grad()
    def step(self, closure=None):
        from torch._gpu import active_capture
        capture = active_capture()
        if capture is not None:
            return capture.sgd(self, closure)
        loss = closure() if closure is not None else None
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            if g["weight_decay"]:
                grad = grad + p.detach() * g["weight_decay"]
            if g["momentum"]:
                st = self.state.setdefault(id(p), {})
                buf = st.get("momentum_buffer")
                buf = grad.clone() if buf is None else buf * g["momentum"] + grad * (1 - g["dampening"])
                st["momentum_buffer"] = buf
                grad = grad + buf * g["momentum"] if g["nesterov"] else buf
            p.data = p.detach() - grad * g["lr"]
        return loss


class Adam(Optimizer):
    _decoupled = False

    def __init__(self, params, lr=1e-3, betas=(0.9, 0.999), eps=1e-8, weight_decay=0.0, amsgrad=False, maximize=False):
        super().__init__(params, dict(lr=lr, betas=betas, eps=eps, weight_decay=weight_decay, amsgrad=amsgrad, maximize=maximize))

    @torch.no_grad()
    def step(self, closure=None):
        from torch._gpu import active_capture
        capture = active_capture()
        if capture is not None:
            return capture.adam(self, closure)
        loss = closure() if closure is not None else None
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            st = self.state.setdefault(id(p), {"step": 0, "exp_avg": torch.zeros_like(p), "exp_avg_sq": torch.zeros_like(p)})
            st["step"] += 1
            b1, b2 = g["betas"]
            lr, wd = g["lr"], g["weight_decay"]
            value = p.detach()
            if wd:
                if self._decoupled:
                    value = value * (1 - lr * wd)
                else:
                    grad = grad + value * wd
            st["exp_avg"] = st["exp_avg"] * b1 + grad * (1 - b1)
            st["exp_avg_sq"] = st["exp_avg_sq"] * b2 + grad * grad * (1 - b2)
            if g["amsgrad"]:
                st["max_exp_avg_sq"] = torch.maximum(st.get("max_exp_avg_sq", st["exp_avg_sq"]), st["exp_avg_sq"])
                denom_sq = st["max_exp_avg_sq"]
            else:
                denom_sq = st["exp_avg_sq"]
            bias1 = 1 - b1 ** st["step"]
            bias2 = 1 - b2 ** st["step"]
            step_size = lr / bias1
            denom = torch.sqrt(denom_sq) / math.sqrt(bias2) + g["eps"]
            p.data = value - st["exp_avg"] / denom * step_size
        return loss


class AdamW(Adam):
    _decoupled = True

    def __init__(self, params, lr=1e-3, betas=(0.9, 0.999), eps=1e-8, weight_decay=1e-2, amsgrad=False, maximize=False):
        Adam.__init__(self, params, lr, betas, eps, weight_decay, amsgrad, maximize)


class RMSprop(Optimizer):
    def __init__(self, params, lr=1e-2, alpha=0.99, eps=1e-8, weight_decay=0.0, momentum=0.0):
        super().__init__(params, dict(lr=lr, alpha=alpha, eps=eps, weight_decay=weight_decay, momentum=momentum))

    @torch.no_grad()
    def step(self, closure=None):
        from torch._gpu import active_capture
        capture = active_capture()
        if capture is not None:
            raise NotImplementedError("Compiled GPU training supports SGD, Adam and AdamW; RMSprop is not captured")
        loss = closure() if closure is not None else None
        for g, p in self._params():
            grad = p.grad
            value = p.detach()
            if g["weight_decay"]:
                grad = grad + value * g["weight_decay"]
            st = self.state.setdefault(id(p), {"square_avg": torch.zeros_like(p)})
            st["square_avg"] = st["square_avg"] * g["alpha"] + grad * grad * (1 - g["alpha"])
            p.data = value - grad / (torch.sqrt(st["square_avg"]) + g["eps"]) * g["lr"]
        return loss


class _LRScheduler:
    def __init__(self, optimizer):
        self.optimizer = optimizer
        self.base_lrs = [g["lr"] for g in optimizer.param_groups]
        self.last_epoch = 0

    def get_last_lr(self):
        return [g["lr"] for g in self.optimizer.param_groups]


class StepLR(_LRScheduler):
    def __init__(self, optimizer, step_size, gamma=0.1):
        super().__init__(optimizer)
        self.step_size = step_size
        self.gamma = gamma

    def step(self):
        self.last_epoch += 1
        for g, base in zip(self.optimizer.param_groups, self.base_lrs):
            g["lr"] = base * self.gamma ** (self.last_epoch // self.step_size)


class lr_scheduler:
    StepLR = StepLR
