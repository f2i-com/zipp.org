"""torch.optim for Zipp: SGD, Adam and AdamW over the tensor library."""
import math
import torch


class _ParamState:
    """`optimizer.state`: per-parameter dicts keyed by the parameter tensor
    itself (by identity, as PyTorch's tensors hash), created on first access
    like PyTorch's `defaultdict(dict)`. A parameter's `id()` also works as a
    key (the GPU capture records parameters by id)."""

    def __init__(self):
        self._entries = {}

    @staticmethod
    def _key(p):
        return p if type(p) is int else id(p)

    def __getitem__(self, p):
        k = self._key(p)
        if k not in self._entries:
            if type(p) is int:
                raise KeyError(p)
            self._entries[k] = (p, {})
        return self._entries[k][1]

    def __setitem__(self, p, value):
        k = self._key(p)
        old = self._entries.get(k)
        self._entries[k] = (old[0] if old is not None and type(p) is int else p, value)

    def __delitem__(self, p):
        del self._entries[self._key(p)]

    def __contains__(self, p):
        return self._key(p) in self._entries

    def __len__(self):
        return len(self._entries)

    def __iter__(self):
        return iter([p for p, _ in self._entries.values()])

    def get(self, p, default=None):
        entry = self._entries.get(self._key(p))
        return default if entry is None else entry[1]

    def setdefault(self, p, default=None):
        if p not in self:
            self[p] = default
        return self[p]

    def keys(self):
        return [p for p, _ in self._entries.values()]

    def values(self):
        return [v for _, v in self._entries.values()]

    def items(self):
        return list(self._entries.values())

    def clear(self):
        self._entries.clear()

    def __repr__(self):
        return repr(dict((self._key(p), v) for p, v in self._entries.values()))


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
        self.state = _ParamState()

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
        # PyTorch's layout: parameters are numbered in group order, "state"
        # maps a number to that parameter's state, and each group lists its numbers.
        state, groups, index = {}, [], 0
        for g in self.param_groups:
            numbers = []
            for p in g["params"]:
                if p in self.state:
                    state[index] = self.state[p]
                numbers.append(index)
                index += 1
            group = {k: v for k, v in g.items() if k != "params"}
            group["params"] = numbers
            groups.append(group)
        return {"state": state, "param_groups": groups}

    def load_state_dict(self, sd):
        saved_groups = sd.get("param_groups", [])
        if len(saved_groups) != len(self.param_groups):
            raise ValueError("loaded state dict has a different number of parameter groups")
        saved_state = sd.get("state", {})
        index = 0
        self.state.clear()
        for g, saved in zip(self.param_groups, saved_groups):
            numbers = saved.get("params", list(range(index, index + len(g["params"]))))
            if len(numbers) != len(g["params"]):
                raise ValueError("loaded state dict contains a parameter group that doesn't match the size of optimizer's group")
            for number, p in zip(numbers, g["params"]):
                if number in saved_state:
                    self.state[p] = dict(saved_state[number])
            index += len(g["params"])
            g.update({k: v for k, v in saved.items() if k != "params"})

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
        from torch._gpu import active_capture, eager_step
        capture = active_capture()
        if capture is not None:
            return capture.sgd(self, closure)
        eager_step(self)
        loss = closure() if closure is not None else None
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            if g["weight_decay"]:
                grad = grad + p.detach() * g["weight_decay"]
            if g["momentum"]:
                st = self.state[p]
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
        from torch._gpu import active_capture, eager_step
        capture = active_capture()
        if capture is not None:
            return capture.adam(self, closure)
        eager_step(self)
        loss = closure() if closure is not None else None
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            st = self.state[p]
            if not st:
                st.update({"step": 0, "exp_avg": torch.zeros_like(p), "exp_avg_sq": torch.zeros_like(p)})
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
        from torch._gpu import active_capture, eager_step
        capture = active_capture()
        if capture is not None:
            raise NotImplementedError("Compiled GPU training supports SGD, Adam and AdamW; RMSprop is not captured")
        eager_step(self)
        loss = closure() if closure is not None else None
        for g, p in self._params():
            grad = p.grad
            value = p.detach()
            if g["weight_decay"]:
                grad = grad + value * g["weight_decay"]
            st = self.state[p]
            if not st:
                st["square_avg"] = torch.zeros_like(p)
            st["square_avg"] = st["square_avg"] * g["alpha"] + grad * grad * (1 - g["alpha"])
            avg = torch.sqrt(st["square_avg"]) + g["eps"]
            if g["momentum"] > 0:
                # PyTorch: buf = momentum * buf + grad / avg; p -= lr * buf.
                buf = st.get("momentum_buffer")
                buf = grad / avg if buf is None else buf * g["momentum"] + grad / avg
                st["momentum_buffer"] = buf
                p.data = value - buf * g["lr"]
            else:
                p.data = value - grad / avg * g["lr"]
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
