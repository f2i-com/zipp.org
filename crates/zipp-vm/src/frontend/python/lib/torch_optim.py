"""torch.optim for Zipp: the Optimizer base class and SGD, Adam, AdamW,
RMSprop, Adagrad, Adamax, NAdam, RAdam, Adadelta, ASGD, Rprop and LBFGS over
the tensor library, each following PyTorch's single-tensor update order.
Learning-rate schedulers live in `torch.optim.lr_scheduler`.

`foreach`, `fused`, `capturable` and `differentiable` are accepted and kept
in the parameter groups (so state dicts match PyTorch's) but change nothing:
every update runs the one CPU loop, under `no_grad`."""
import math
import sys
import torch


class _RequiredParameter:
    """Singleton marking a default that every parameter group must supply."""

    def __repr__(self):
        return "<required parameter>"


required = _RequiredParameter()


def _warn(message):
    print("UserWarning: " + message, file=sys.stderr)


def _typename(value):
    if isinstance(value, torch.Tensor):
        return value.type()
    return type(value).__name__


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


class _RemovableHandle:
    def __init__(self, hooks, key):
        self._hooks = hooks
        self.id = key

    def remove(self):
        self._hooks.pop(self.id, None)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.remove()
        return False


_hook_ids = [0]


def _with_step_hooks(step):
    """Wrap an optimizer class's `step` so registered pre/post hooks run
    around it (PyTorch patches every subclass's step the same way)."""
    def step_with_hooks(*args, **kwargs):
        # As in PyTorch, hooks see the call's full args, the optimizer first.
        self = args[0]
        for hook in list(self._optimizer_step_pre_hooks.values()):
            result = hook(self, args, kwargs)
            if result is not None:
                if not (isinstance(result, tuple) and len(result) == 2):
                    raise RuntimeError("%s must return None or a tuple of (new_args, new_kwargs), but got %s." % (hook, result))
                args, kwargs = result
        out = step(*args, **kwargs)
        for hook in list(self._optimizer_step_post_hooks.values()):
            hook(self, args, kwargs)
        return out
    step_with_hooks._zipp_hooked = True
    step_with_hooks.__name__ = "step"
    step_with_hooks.__wrapped__ = step
    return step_with_hooks


def _closure_loss(closure):
    # step() runs under no_grad; the closure re-evaluates the model and calls
    # backward(), so it runs with gradients enabled, as in PyTorch.
    if closure is None:
        return None
    with torch.enable_grad():
        return closure()


def _check_non_negative(name, value, label=None):
    if not 0.0 <= value:
        raise ValueError("Invalid %s: %s" % (label or (name + " value"), value))


def _check_betas(betas):
    if not 0.0 <= betas[0] < 1.0:
        raise ValueError("Invalid beta parameter at index 0: %s" % (betas[0],))
    if not 0.0 <= betas[1] < 1.0:
        raise ValueError("Invalid beta parameter at index 1: %s" % (betas[1],))


def _check_flags(foreach, fused, differentiable):
    if fused and foreach:
        raise RuntimeError("`fused` and `foreach` cannot be `True` together.")
    if fused and differentiable:
        raise RuntimeError("`fused` does not support `differentiable`")


def _value(v):
    return v.item() if isinstance(v, torch.Tensor) else v


class Optimizer:
    def __init__(self, params, defaults):
        self.defaults = defaults
        self._optimizer_step_pre_hooks = {}
        self._optimizer_step_post_hooks = {}
        # Hook the step this optimizer runs: the nearest class defining one
        # (AdamW inherits Adam's).
        for klass in type(self).__mro__:
            step = klass.__dict__.get("step")
            if step is not None:
                if not getattr(step, "_zipp_hooked", False):
                    klass.step = _with_step_hooks(step)
                break
        if isinstance(params, torch.Tensor):
            raise TypeError("params argument given to the optimizer should be an iterable of Tensors or dicts, but got " + _typename(params))
        self.state = _ParamState()
        self.param_groups = []
        param_groups = list(params)
        if len(param_groups) == 0:
            raise ValueError("optimizer got an empty parameter list")
        if not isinstance(param_groups[0], dict):
            param_groups = [{"params": param_groups}]
        for group in param_groups:
            self.add_param_group(group)

    def __getstate__(self):
        return {"defaults": self.defaults, "state": self.state, "param_groups": self.param_groups}

    def __setstate__(self, state):
        for k, v in state.items():
            setattr(self, k, v)

    def __repr__(self):
        lines = [type(self).__name__ + " ("]
        for i, group in enumerate(self.param_groups):
            lines.append("Parameter Group %d" % i)
            for key in sorted(group.keys()):
                if key != "params":
                    lines.append("    %s: %s" % (key, group[key]))
        lines.append(")")
        return "\n".join(lines)

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
        # PyTorch's layout: parameters are numbered in group order (a
        # parameter listed twice keeps its first number), "state" maps a
        # number to that parameter's state, and each group lists its numbers.
        numbers, groups, index = {}, [], 0
        for g in self.param_groups:
            packed = {k: v for k, v in g.items() if k != "params"}
            for p in g["params"]:
                if id(p) not in numbers:
                    numbers[id(p)] = index
                    index += 1
            packed["params"] = [numbers[id(p)] for p in g["params"]]
            groups.append(packed)
        state = {}
        for p, value in self.state.items():
            state[numbers[id(p)] if id(p) in numbers else p] = value
        return {"state": state, "param_groups": groups}

    def load_state_dict(self, state_dict):
        groups = self.param_groups
        saved_groups = [dict(g) for g in state_dict["param_groups"]]
        # Validate everything before touching the current state.
        if len(groups) != len(saved_groups):
            raise ValueError("loaded state dict has a different number of parameter groups")
        for g, saved in zip(groups, saved_groups):
            if len(g["params"]) != len(saved["params"]):
                raise ValueError("loaded state dict contains a parameter group that doesn't match the size of optimizer's group")
        id_map = {}
        for g, saved in zip(groups, saved_groups):
            for number, p in zip(saved["params"], g["params"]):
                id_map[number] = p
        state = _ParamState()
        for key, value in state_dict["state"].items():
            if key in id_map:
                param = id_map[key]
                state[param] = self._load_param_state(param, dict(value))
            else:
                state[key] = value
        new_groups = []
        for g, saved in zip(groups, saved_groups):
            saved["params"] = g["params"]
            if "param_names" in g and "param_names" not in saved:
                saved["param_names"] = g["param_names"]
            # Options a checkpoint predates take this optimizer's defaults,
            # as PyTorch's __setstate__ fills them in.
            for name, default in self.defaults.items():
                if default is not required:
                    saved.setdefault(name, default)
            new_groups.append(saved)
        self.state = state
        self.param_groups = new_groups

    def _load_param_state(self, param, st):
        # PyTorch keeps `step` as a float tensor; here it is an int (what the
        # updates, the GPU capture and PyTorch's own loader all accept).
        step = st.get("step")
        if isinstance(step, torch.Tensor):
            st["step"] = int(step.item())
        elif isinstance(step, float) and step == int(step):
            st["step"] = int(step)
        for key, value in st.items():
            if key != "step" and isinstance(value, torch.Tensor) and param.dtype.is_floating_point and value.dtype.is_floating_point and value.dtype != param.dtype:
                st[key] = value.to(param.dtype)
        return st

    def add_param_group(self, param_group):
        if not isinstance(param_group, dict):
            raise TypeError("param_group must be a dict, but got %s" % (type(param_group),))
        params = param_group["params"]
        if isinstance(params, torch.Tensor):
            params = [params]
        elif isinstance(params, (set, frozenset)):
            raise TypeError("optimizer parameters need to be organized in ordered collections, but the ordering of tensors in sets will change between runs. Please use a list instead.")
        else:
            params = list(params)
        tensors, names = [], []
        for param in params:
            if isinstance(param, tuple):
                names.append(param[0])
                tensors.append(param[1])
            else:
                tensors.append(param)
        param_group["params"] = tensors
        if names:
            if len(names) != len(tensors):
                raise ValueError("all optimizer params should be with/without names. Some param names are missing")
            param_group["param_names"] = names
        for param in tensors:
            if not isinstance(param, torch.Tensor):
                raise TypeError("optimizer can only optimize Tensors, but one of the params is " + _typename(param))
            if not self.defaults.get("differentiable", None) and not (param.is_leaf or getattr(param, "_retain", False)):
                raise ValueError("can't optimize a non-leaf Tensor")
        for name, default in self.defaults.items():
            if default is required and name not in param_group:
                raise ValueError("parameter group didn't specify a value of required optimization parameter " + name)
            param_group.setdefault(name, default)
        if len(set(id(p) for p in tensors)) != len(tensors):
            _warn("optimizer contains a parameter group with duplicate parameters; in future, this will cause an error; see github.com/pytorch/pytorch/issues/40967 for more information")
        existing = set()
        for group in self.param_groups:
            existing.update(id(p) for p in group["params"])
            if ("param_names" in param_group) != ("param_names" in group):
                raise ValueError("all optimizer param groups should be with/without names. cannot add param group %s to the optimizer"
                                 % ("with names" if "param_names" in param_group else "without names"))
        if any(id(p) in existing for p in tensors):
            raise ValueError("some parameters appear in more than one parameter group")
        self.param_groups.append(param_group)

    def register_step_pre_hook(self, hook):
        _hook_ids[0] += 1
        self._optimizer_step_pre_hooks[_hook_ids[0]] = hook
        return _RemovableHandle(self._optimizer_step_pre_hooks, _hook_ids[0])

    def register_step_post_hook(self, hook):
        _hook_ids[0] += 1
        self._optimizer_step_post_hooks[_hook_ids[0]] = hook
        return _RemovableHandle(self._optimizer_step_post_hooks, _hook_ids[0])

    def step(self, closure=None):
        raise NotImplementedError

    def _params(self):
        for g in self.param_groups:
            for p in g["params"]:
                if p.grad is not None:
                    yield g, p

    def _eager(self, name=None):
        """Refuse parameters a prepared GPU session holds; for optimizers the
        GPU capture does not record, refuse capture too."""
        from torch._gpu import active_capture, eager_step
        if name is not None and active_capture() is not None:
            raise NotImplementedError("Compiled GPU training supports SGD, Adam and AdamW; %s is not captured" % name)
        eager_step(self)


class SGD(Optimizer):
    def __init__(self, params, lr=1e-3, momentum=0, dampening=0, weight_decay=0, nesterov=False, *, maximize=False,
                 foreach=None, differentiable=False, fused=None):
        _check_non_negative("lr", _value(lr), "learning rate")
        _check_non_negative("momentum", momentum)
        _check_non_negative("weight_decay", _value(weight_decay))
        if nesterov and (momentum <= 0 or dampening != 0):
            raise ValueError("Nesterov momentum requires a momentum and zero dampening")
        _check_flags(foreach, fused, differentiable)
        super().__init__(params, dict(lr=lr, momentum=momentum, dampening=dampening, weight_decay=weight_decay, nesterov=nesterov,
                                      maximize=maximize, foreach=foreach, differentiable=differentiable, fused=fused))

    @torch.no_grad()
    def step(self, closure=None):
        from torch._gpu import active_capture
        capture = active_capture()
        if capture is not None:
            return capture.sgd(self, closure)
        self._eager()
        loss = _closure_loss(closure)
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

    def __init__(self, params, lr=1e-3, betas=(0.9, 0.999), eps=1e-8, weight_decay=0, amsgrad=False, *, foreach=None,
                 maximize=False, capturable=False, differentiable=False, fused=None, decoupled_weight_decay=None):
        if decoupled_weight_decay is None:
            decoupled_weight_decay = self._decoupled
        _check_non_negative("lr", _value(lr), "learning rate")
        _check_non_negative("eps", eps, "epsilon value")
        _check_betas(betas)
        _check_non_negative("weight_decay", weight_decay)
        _check_flags(foreach, fused, differentiable)
        # Adam(decoupled_weight_decay=True) is AdamW; the GPU capture reads
        # the optimizer-level flag.
        self._decoupled = bool(decoupled_weight_decay)
        super().__init__(params, dict(lr=lr, betas=betas, eps=eps, weight_decay=weight_decay, amsgrad=amsgrad, maximize=maximize,
                                      foreach=foreach, capturable=capturable, differentiable=differentiable, fused=fused,
                                      decoupled_weight_decay=bool(decoupled_weight_decay)))

    @torch.no_grad()
    def step(self, closure=None):
        from torch._gpu import active_capture
        capture = active_capture()
        if capture is not None:
            if any(bool(g.get("decoupled_weight_decay", self._decoupled)) != self._decoupled for g in self.param_groups):
                raise NotImplementedError("GPU Adam needs one decoupled_weight_decay setting for every parameter group")
            return capture.adam(self, closure)
        self._eager()
        loss = _closure_loss(closure)
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            st = self.state[p]
            if not st:
                st.update({"step": 0, "exp_avg": torch.zeros_like(p), "exp_avg_sq": torch.zeros_like(p)})
                if g["amsgrad"]:
                    st["max_exp_avg_sq"] = torch.zeros_like(p)
            st["step"] += 1
            b1, b2 = g["betas"]
            lr, wd = g["lr"], g["weight_decay"]
            value = p.detach()
            if wd:
                if g.get("decoupled_weight_decay", self._decoupled):
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

    def __init__(self, params, lr=1e-3, betas=(0.9, 0.999), eps=1e-8, weight_decay=1e-2, amsgrad=False, *, maximize=False,
                 foreach=None, capturable=False, differentiable=False, fused=None):
        Adam.__init__(self, params, lr, betas, eps, weight_decay, amsgrad, foreach=foreach, maximize=maximize, capturable=capturable,
                      differentiable=differentiable, fused=fused, decoupled_weight_decay=True)


class RMSprop(Optimizer):
    def __init__(self, params, lr=1e-2, alpha=0.99, eps=1e-8, weight_decay=0, momentum=0, centered=False, capturable=False,
                 foreach=None, maximize=False, differentiable=False):
        _check_non_negative("lr", _value(lr), "learning rate")
        _check_non_negative("eps", eps, "epsilon value")
        _check_non_negative("momentum", momentum)
        _check_non_negative("weight_decay", weight_decay)
        _check_non_negative("alpha", alpha)
        super().__init__(params, dict(lr=lr, momentum=momentum, alpha=alpha, eps=eps, centered=centered, weight_decay=weight_decay,
                                      capturable=capturable, foreach=foreach, maximize=maximize, differentiable=differentiable))

    @torch.no_grad()
    def step(self, closure=None):
        self._eager("RMSprop")
        loss = _closure_loss(closure)
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            value = p.detach()
            st = self.state[p]
            if not st:
                st["step"] = 0
                st["square_avg"] = torch.zeros_like(p)
                if g["momentum"] > 0:
                    st["momentum_buffer"] = torch.zeros_like(p)
                if g["centered"]:
                    st["grad_avg"] = torch.zeros_like(p)
            st["step"] = st.get("step", 0) + 1
            if g["weight_decay"]:
                grad = grad + value * g["weight_decay"]
            alpha = g["alpha"]
            st["square_avg"] = st["square_avg"] * alpha + grad * grad * (1 - alpha)
            if g["centered"]:
                grad_avg = st.get("grad_avg")
                grad_avg = torch.zeros_like(p) if grad_avg is None else grad_avg
                st["grad_avg"] = grad_avg * alpha + grad * (1 - alpha)
                avg = torch.sqrt(st["square_avg"] - st["grad_avg"] * st["grad_avg"]) + g["eps"]
            else:
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


class Adagrad(Optimizer):
    def __init__(self, params, lr=1e-2, lr_decay=0, weight_decay=0, initial_accumulator_value=0, eps=1e-10, foreach=None, *,
                 maximize=False, differentiable=False, fused=None):
        _check_non_negative("lr", _value(lr), "learning rate")
        _check_non_negative("lr_decay", lr_decay)
        _check_non_negative("weight_decay", weight_decay)
        _check_non_negative("initial_accumulator_value", initial_accumulator_value)
        _check_non_negative("eps", eps, "epsilon value")
        _check_flags(foreach, fused, differentiable)
        super().__init__(params, dict(lr=lr, lr_decay=lr_decay, eps=eps, weight_decay=weight_decay,
                                      initial_accumulator_value=initial_accumulator_value, foreach=foreach, maximize=maximize,
                                      differentiable=differentiable, fused=fused))
        # Adagrad creates its state up front, as PyTorch's does.
        for group in self.param_groups:
            for p in group["params"]:
                st = self.state[p]
                st["step"] = 0
                st["sum"] = torch.full_like(p, group["initial_accumulator_value"])

    def share_memory(self):
        return None

    @torch.no_grad()
    def step(self, closure=None):
        self._eager("Adagrad")
        loss = _closure_loss(closure)
        for g, p in self._params():
            grad = p.grad
            st = self.state[p]
            if not st:
                st["step"] = 0
                st["sum"] = torch.full_like(p, g["initial_accumulator_value"])
            st["step"] += 1
            if g["maximize"]:
                grad = -grad
            value = p.detach()
            if g["weight_decay"]:
                grad = grad + value * g["weight_decay"]
            clr = g["lr"] / (1 + (st["step"] - 1) * g["lr_decay"])
            st["sum"] = st["sum"] + grad * grad
            std = torch.sqrt(st["sum"]) + g["eps"]
            p.data = value - grad / std * clr
        return loss


class Adamax(Optimizer):
    def __init__(self, params, lr=2e-3, betas=(0.9, 0.999), eps=1e-8, weight_decay=0, foreach=None, *, maximize=False,
                 differentiable=False, capturable=False):
        _check_non_negative("lr", _value(lr), "learning rate")
        _check_non_negative("eps", eps, "epsilon value")
        _check_betas(betas)
        _check_non_negative("weight_decay", weight_decay)
        super().__init__(params, dict(lr=lr, betas=betas, eps=eps, weight_decay=weight_decay, foreach=foreach, maximize=maximize,
                                      differentiable=differentiable, capturable=capturable))

    @torch.no_grad()
    def step(self, closure=None):
        self._eager("Adamax")
        loss = _closure_loss(closure)
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            st = self.state[p]
            if not st:
                st.update({"step": 0, "exp_avg": torch.zeros_like(p), "exp_inf": torch.zeros_like(p)})
            st["step"] += 1
            b1, b2 = g["betas"]
            value = p.detach()
            if g["weight_decay"]:
                grad = grad + value * g["weight_decay"]
            st["exp_avg"] = st["exp_avg"] + (grad - st["exp_avg"]) * (1 - b1)
            st["exp_inf"] = torch.maximum(st["exp_inf"] * b2, torch.abs(grad) + g["eps"])
            clr = g["lr"] / (1 - b1 ** st["step"])
            p.data = value - st["exp_avg"] / st["exp_inf"] * clr
        return loss


class NAdam(Optimizer):
    def __init__(self, params, lr=2e-3, betas=(0.9, 0.999), eps=1e-8, weight_decay=0, momentum_decay=4e-3,
                 decoupled_weight_decay=False, *, foreach=None, maximize=False, capturable=False, differentiable=False):
        _check_non_negative("lr", _value(lr), "learning rate")
        _check_non_negative("eps", eps, "epsilon value")
        _check_betas(betas)
        _check_non_negative("weight_decay", weight_decay)
        _check_non_negative("momentum_decay", momentum_decay)
        super().__init__(params, dict(lr=lr, betas=betas, eps=eps, weight_decay=weight_decay, momentum_decay=momentum_decay,
                                      decoupled_weight_decay=decoupled_weight_decay, maximize=maximize, foreach=foreach,
                                      capturable=capturable, differentiable=differentiable))

    def _load_param_state(self, param, st):
        st = Optimizer._load_param_state(self, param, st)
        if isinstance(st.get("mu_product"), torch.Tensor):
            st["mu_product"] = st["mu_product"].item()
        return st

    @torch.no_grad()
    def step(self, closure=None):
        self._eager("NAdam")
        loss = _closure_loss(closure)
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            st = self.state[p]
            if not st:
                st.update({"step": 0, "mu_product": 1.0, "exp_avg": torch.zeros_like(p), "exp_avg_sq": torch.zeros_like(p)})
            st["step"] += 1
            step = st["step"]
            lr, wd = g["lr"], g["weight_decay"]
            b1, b2 = g["betas"]
            value = p.detach()
            if wd:
                if g["decoupled_weight_decay"]:
                    value = value * (1 - lr * wd)
                else:
                    grad = grad + value * wd
            bias2 = 1 - b2 ** step
            mu = b1 * (1.0 - 0.5 * 0.96 ** (step * g["momentum_decay"]))
            mu_next = b1 * (1.0 - 0.5 * 0.96 ** ((step + 1) * g["momentum_decay"]))
            mu_product = st["mu_product"] * mu
            st["mu_product"] = mu_product
            st["exp_avg"] = st["exp_avg"] + (grad - st["exp_avg"]) * (1 - b1)
            st["exp_avg_sq"] = st["exp_avg_sq"] * b2 + grad * grad * (1 - b2)
            denom = torch.sqrt(st["exp_avg_sq"] / bias2) + g["eps"]
            value = value - grad / denom * (lr * (1.0 - mu) / (1.0 - mu_product))
            p.data = value - st["exp_avg"] / denom * (lr * mu_next / (1.0 - mu_product * mu_next))
        return loss


class RAdam(Optimizer):
    def __init__(self, params, lr=1e-3, betas=(0.9, 0.999), eps=1e-8, weight_decay=0, decoupled_weight_decay=False, *,
                 foreach=None, maximize=False, capturable=False, differentiable=False):
        _check_non_negative("lr", _value(lr), "learning rate")
        _check_non_negative("eps", eps, "epsilon value")
        _check_betas(betas)
        _check_non_negative("weight_decay", weight_decay)
        super().__init__(params, dict(lr=lr, betas=betas, eps=eps, weight_decay=weight_decay, maximize=maximize, foreach=foreach,
                                      capturable=capturable, decoupled_weight_decay=decoupled_weight_decay,
                                      differentiable=differentiable))

    @torch.no_grad()
    def step(self, closure=None):
        self._eager("RAdam")
        loss = _closure_loss(closure)
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            st = self.state[p]
            if not st:
                st.update({"step": 0, "exp_avg": torch.zeros_like(p), "exp_avg_sq": torch.zeros_like(p)})
            st["step"] += 1
            step = st["step"]
            lr, wd = g["lr"], g["weight_decay"]
            b1, b2 = g["betas"]
            value = p.detach()
            if wd:
                if g["decoupled_weight_decay"]:
                    value = value * (1 - lr * wd)
                else:
                    grad = grad + value * wd
            st["exp_avg"] = st["exp_avg"] + (grad - st["exp_avg"]) * (1 - b1)
            st["exp_avg_sq"] = st["exp_avg_sq"] * b2 + grad * grad * (1 - b2)
            bias1 = 1 - b1 ** step
            bias2 = 1 - b2 ** step
            corrected = st["exp_avg"] / bias1
            rho_inf = 2 / (1 - b2) - 1
            rho_t = rho_inf - 2 * step * (b2 ** step) / bias2
            if rho_t > 5.0:
                rect = ((rho_t - 4) * (rho_t - 2) * rho_inf / ((rho_inf - 4) * (rho_inf - 2) * rho_t)) ** 0.5
                adaptive = (bias2 ** 0.5) / (torch.sqrt(st["exp_avg_sq"]) + g["eps"])
                p.data = value - corrected * lr * adaptive * rect
            else:
                p.data = value - corrected * lr
        return loss


class Adadelta(Optimizer):
    def __init__(self, params, lr=1.0, rho=0.9, eps=1e-6, weight_decay=0, foreach=None, *, capturable=False, maximize=False,
                 differentiable=False):
        _check_non_negative("lr", _value(lr), "learning rate")
        if not 0.0 <= rho <= 1.0:
            raise ValueError("Invalid rho value: %s" % (rho,))
        _check_non_negative("eps", eps, "epsilon value")
        _check_non_negative("weight_decay", weight_decay)
        super().__init__(params, dict(lr=lr, rho=rho, eps=eps, weight_decay=weight_decay, maximize=maximize, capturable=capturable,
                                      foreach=foreach, differentiable=differentiable))

    @torch.no_grad()
    def step(self, closure=None):
        self._eager("Adadelta")
        loss = _closure_loss(closure)
        for g, p in self._params():
            grad = p.grad
            st = self.state[p]
            if not st:
                st.update({"step": 0, "square_avg": torch.zeros_like(p), "acc_delta": torch.zeros_like(p)})
            st["step"] += 1
            if g["maximize"]:
                grad = -grad
            value = p.detach()
            if g["weight_decay"]:
                grad = grad + value * g["weight_decay"]
            rho, eps = g["rho"], g["eps"]
            st["square_avg"] = st["square_avg"] * rho + grad * grad * (1 - rho)
            std = torch.sqrt(st["square_avg"] + eps)
            delta = torch.sqrt(st["acc_delta"] + eps) / std * grad
            st["acc_delta"] = st["acc_delta"] * rho + delta * delta * (1 - rho)
            p.data = value - delta * g["lr"]
        return loss


class ASGD(Optimizer):
    def __init__(self, params, lr=1e-2, lambd=1e-4, alpha=0.75, t0=1e6, weight_decay=0, foreach=None, maximize=False,
                 differentiable=False, capturable=False):
        _check_non_negative("lr", _value(lr), "learning rate")
        _check_non_negative("weight_decay", weight_decay)
        super().__init__(params, dict(lr=lr, lambd=lambd, alpha=alpha, t0=t0, weight_decay=weight_decay, foreach=foreach,
                                      maximize=maximize, differentiable=differentiable, capturable=capturable))

    def _load_param_state(self, param, st):
        st = Optimizer._load_param_state(self, param, st)
        for key in ("eta", "mu"):
            if isinstance(st.get(key), torch.Tensor):
                st[key] = st[key].item()
        return st

    @torch.no_grad()
    def step(self, closure=None):
        self._eager("ASGD")
        loss = _closure_loss(closure)
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            st = self.state[p]
            if not st:
                st.update({"step": 0, "eta": float(_value(g["lr"])), "mu": 1.0, "ax": torch.zeros_like(p)})
            st["step"] += 1
            value = p.detach()
            if g["weight_decay"]:
                grad = grad + value * g["weight_decay"]
            eta = st["eta"]
            value = value * (1 - g["lambd"] * eta) - grad * eta
            p.data = value
            if st["mu"] != 1:
                st["ax"] = st["ax"] + (value - st["ax"]) * st["mu"]
            else:
                st["ax"] = value.clone()
            step = st["step"]
            st["eta"] = g["lr"] / ((1 + g["lambd"] * g["lr"] * step) ** g["alpha"])
            st["mu"] = 1 / max(1, step - g["t0"])
        return loss


class Rprop(Optimizer):
    def __init__(self, params, lr=1e-2, etas=(0.5, 1.2), step_sizes=(1e-6, 50), *, capturable=False, foreach=None,
                 maximize=False, differentiable=False):
        _check_non_negative("lr", _value(lr), "learning rate")
        if not 0.0 < etas[0] < 1.0 < etas[1]:
            raise ValueError("Invalid eta values: %s, %s" % (etas[0], etas[1]))
        super().__init__(params, dict(lr=lr, etas=etas, step_sizes=step_sizes, foreach=foreach, maximize=maximize,
                                      differentiable=differentiable, capturable=capturable))

    @torch.no_grad()
    def step(self, closure=None):
        self._eager("Rprop")
        loss = _closure_loss(closure)
        for g, p in self._params():
            grad = p.grad
            if g["maximize"]:
                grad = -grad
            st = self.state[p]
            if not st:
                st.update({"step": 0, "prev": torch.zeros_like(p), "step_size": torch.full_like(grad, _value(g["lr"]))})
            st["step"] += 1
            etaminus, etaplus = g["etas"]
            step_size_min, step_size_max = g["step_sizes"]
            product = grad * st["prev"]
            sign = torch.where(product > 0, etaplus, torch.where(product < 0, etaminus, 1.0)).to(grad.dtype)
            st["step_size"] = torch.clamp(st["step_size"] * sign, step_size_min, step_size_max)
            grad = torch.where(product < 0, 0.0, grad).to(grad.dtype)
            p.data = p.detach() - torch.sign(grad) * st["step_size"]
            st["prev"] = grad.clone()
        return loss


def _dot(a, b):
    # A 1-D inner product as a float32 scalar tensor, as Tensor.dot gives.
    return (a * b).sum()


def _cubic_interpolate(x1, f1, g1, x2, f2, g2, bounds=None):
    # The minimiser of the cubic through (x1, f1, g1) and (x2, f2, g2).
    if bounds is not None:
        xmin_bound, xmax_bound = bounds
    else:
        xmin_bound, xmax_bound = (x1, x2) if x1 <= x2 else (x2, x1)
    d1 = g1 + g2 - 3 * (f1 - f2) / (x1 - x2)
    d2_square = d1 ** 2 - g1 * g2
    if d2_square >= 0:
        d2 = d2_square.sqrt() if isinstance(d2_square, torch.Tensor) else math.sqrt(d2_square)
        if x1 <= x2:
            min_pos = x2 - (x2 - x1) * ((g2 + d2 - d1) / (g2 - g1 + 2 * d2))
        else:
            min_pos = x1 - (x1 - x2) * ((g1 + d2 - d1) / (g1 - g2 + 2 * d2))
        return min(max(min_pos, xmin_bound), xmax_bound)
    return (xmin_bound + xmax_bound) / 2.0


def _strong_wolfe(obj_func, x, t, d, f, g, gtd, c1=1e-4, c2=0.9, tolerance_change=1e-9, max_ls=25):
    """PyTorch's strong-Wolfe line search (after minFunc), step for step."""
    d_norm = d.abs().max()
    g = g.clone()
    f_new, g_new = obj_func(x, t, d)
    ls_func_evals = 1
    gtd_new = _dot(g_new, d)
    t_prev, f_prev, g_prev, gtd_prev = 0, f, g, gtd
    done = False
    ls_iter = 0
    bracket = bracket_f = bracket_g = bracket_gtd = None
    while ls_iter < max_ls:
        if f_new > f + c1 * t * gtd or (ls_iter > 1 and f_new >= f_prev):
            bracket, bracket_f = [t_prev, t], [f_prev, f_new]
            bracket_g, bracket_gtd = [g_prev, g_new.clone()], [gtd_prev, gtd_new]
            break
        if abs(gtd_new) <= -c2 * gtd:
            bracket, bracket_f, bracket_g = [t], [f_new], [g_new]
            done = True
            break
        if gtd_new >= 0:
            bracket, bracket_f = [t_prev, t], [f_prev, f_new]
            bracket_g, bracket_gtd = [g_prev, g_new.clone()], [gtd_prev, gtd_new]
            break
        min_step = t + 0.01 * (t - t_prev)
        max_step = t * 10
        tmp = t
        t = _cubic_interpolate(t_prev, f_prev, gtd_prev, t, f_new, gtd_new, bounds=(min_step, max_step))
        t_prev, f_prev, g_prev, gtd_prev = tmp, f_new, g_new.clone(), gtd_new
        f_new, g_new = obj_func(x, t, d)
        ls_func_evals += 1
        gtd_new = _dot(g_new, d)
        ls_iter += 1
    if ls_iter == max_ls:
        bracket, bracket_f, bracket_g = [0, t], [f, f_new], [g, g_new]
    insuf_progress = False
    low_pos, high_pos = (0, 1) if bracket_f[0] <= bracket_f[-1] else (1, 0)
    while not done and ls_iter < max_ls:
        if abs(bracket[1] - bracket[0]) * d_norm < tolerance_change:
            break
        t = _cubic_interpolate(bracket[0], bracket_f[0], bracket_gtd[0], bracket[1], bracket_f[1], bracket_gtd[1])
        eps = 0.1 * (max(bracket) - min(bracket))
        if min(max(bracket) - t, t - min(bracket)) < eps:
            if insuf_progress or t >= max(bracket) or t <= min(bracket):
                if abs(t - max(bracket)) < abs(t - min(bracket)):
                    t = max(bracket) - eps
                else:
                    t = min(bracket) + eps
                insuf_progress = False
            else:
                insuf_progress = True
        else:
            insuf_progress = False
        f_new, g_new = obj_func(x, t, d)
        ls_func_evals += 1
        gtd_new = _dot(g_new, d)
        ls_iter += 1
        if f_new > f + c1 * t * gtd or f_new >= bracket_f[low_pos]:
            bracket[high_pos], bracket_f[high_pos] = t, f_new
            bracket_g[high_pos], bracket_gtd[high_pos] = g_new.clone(), gtd_new
            low_pos, high_pos = (0, 1) if bracket_f[0] <= bracket_f[1] else (1, 0)
        else:
            if abs(gtd_new) <= -c2 * gtd:
                done = True
            elif gtd_new * (bracket[high_pos] - bracket[low_pos]) >= 0:
                bracket[high_pos], bracket_f[high_pos] = bracket[low_pos], bracket_f[low_pos]
                bracket_g[high_pos], bracket_gtd[high_pos] = bracket_g[low_pos], bracket_gtd[low_pos]
            bracket[low_pos], bracket_f[low_pos] = t, f_new
            bracket_g[low_pos], bracket_gtd[low_pos] = g_new.clone(), gtd_new
    return bracket_f[low_pos], bracket_g[low_pos], bracket[low_pos], ls_func_evals


class LBFGS(Optimizer):
    """PyTorch's L-BFGS (one parameter group; `step(closure)` re-evaluates
    the loss), with optional strong-Wolfe line search. Scalars that PyTorch
    keeps as float32 tensors (curvatures, step products) stay tensors here."""

    def __init__(self, params, lr=1, max_iter=20, max_eval=None, tolerance_grad=1e-7, tolerance_change=1e-9, history_size=100,
                 line_search_fn=None):
        _check_non_negative("lr", _value(lr), "learning rate")
        if max_eval is None:
            max_eval = max_iter * 5 // 4
        super().__init__(params, dict(lr=lr, max_iter=max_iter, max_eval=max_eval, tolerance_grad=tolerance_grad,
                                      tolerance_change=tolerance_change, history_size=history_size, line_search_fn=line_search_fn))
        if len(self.param_groups) != 1:
            raise ValueError("LBFGS doesn't support per-parameter options (parameter groups)")
        self._params = self.param_groups[0]["params"]
        self._numel_cache = None

    def _numel(self):
        if self._numel_cache is None:
            self._numel_cache = sum(p.numel() for p in self._params)
        return self._numel_cache

    def _gather_flat_grad(self):
        views = []
        for p in self._params:
            views.append(torch.zeros(p.numel(), dtype=p.dtype) if p.grad is None else p.grad.reshape(-1))
        return torch.cat(views, 0)

    def _add_grad(self, step_size, update):
        offset = 0
        for p in self._params:
            numel = p.numel()
            p.data = p.detach() + update[offset:offset + numel].view_as(p) * step_size
            offset += numel

    def _clone_param(self):
        return [p.detach().clone() for p in self._params]

    def _set_param(self, params_data):
        for p, data in zip(self._params, params_data):
            p.data = data.clone()

    def _directional_evaluate(self, closure, x, t, d):
        self._add_grad(t, d)
        loss = float(_closure_loss(closure))
        flat_grad = self._gather_flat_grad()
        self._set_param(x)
        return loss, flat_grad

    @torch.no_grad()
    def step(self, closure):
        self._eager("LBFGS")
        group = self.param_groups[0]
        lr = _value(group["lr"])
        max_iter, max_eval = group["max_iter"], group["max_eval"]
        tolerance_grad, tolerance_change = group["tolerance_grad"], group["tolerance_change"]
        line_search_fn, history_size = group["line_search_fn"], group["history_size"]
        state = self.state[self._params[0]]
        state.setdefault("func_evals", 0)
        state.setdefault("n_iter", 0)
        orig_loss = _closure_loss(closure)
        loss = float(orig_loss)
        current_evals = 1
        state["func_evals"] += 1
        flat_grad = self._gather_flat_grad()
        if flat_grad.abs().max() <= tolerance_grad:
            return orig_loss
        d, t = state.get("d"), state.get("t")
        old_dirs, old_stps, ro = state.get("old_dirs"), state.get("old_stps"), state.get("ro")
        H_diag = state.get("H_diag")
        prev_flat_grad, prev_loss = state.get("prev_flat_grad"), state.get("prev_loss")
        n_iter = 0
        while n_iter < max_iter:
            n_iter += 1
            state["n_iter"] += 1
            if state["n_iter"] == 1:
                d = -flat_grad
                old_dirs, old_stps, ro = [], [], []
                H_diag = 1
            else:
                y = flat_grad - prev_flat_grad
                s = d * t
                ys = _dot(y, s)
                if ys > 1e-10:
                    if len(old_dirs) == history_size:
                        old_dirs.pop(0)
                        old_stps.pop(0)
                        ro.pop(0)
                    old_dirs.append(y)
                    old_stps.append(s)
                    ro.append(1.0 / ys)
                    H_diag = ys / _dot(y, y)
                num_old = len(old_dirs)
                if "al" not in state:
                    state["al"] = [None] * history_size
                al = state["al"]
                q = -flat_grad
                for i in range(num_old - 1, -1, -1):
                    al[i] = _dot(old_stps[i], q) * ro[i]
                    q = q - old_dirs[i] * al[i]
                r = q * H_diag
                for i in range(num_old):
                    be_i = _dot(old_dirs[i], r) * ro[i]
                    r = r + old_stps[i] * (al[i] - be_i)
                d = r
            prev_flat_grad = flat_grad.clone()
            prev_loss = loss
            if state["n_iter"] == 1:
                t = min(1.0, 1.0 / flat_grad.abs().sum()) * lr
            else:
                t = lr
            gtd = _dot(flat_grad, d)
            if gtd > -tolerance_change:
                break
            ls_func_evals = 0
            if line_search_fn is not None:
                if line_search_fn != "strong_wolfe":
                    raise RuntimeError("only 'strong_wolfe' is supported")
                x_init = self._clone_param()

                def obj_func(x, t, d):
                    return self._directional_evaluate(closure, x, t, d)
                loss, flat_grad, t, ls_func_evals = _strong_wolfe(obj_func, x_init, t, d, loss, flat_grad, gtd, max_ls=max_eval - current_evals)
                self._add_grad(t, d)
                opt_cond = flat_grad.abs().max() <= tolerance_grad
            else:
                self._add_grad(t, d)
                opt_cond = False
                if n_iter != max_iter:
                    loss = float(_closure_loss(closure))
                    flat_grad = self._gather_flat_grad()
                    opt_cond = flat_grad.abs().max() <= tolerance_grad
                    ls_func_evals = 1
            current_evals += ls_func_evals
            state["func_evals"] += ls_func_evals
            if n_iter == max_iter or current_evals >= max_eval or opt_cond:
                break
            if (d * t).abs().max() <= tolerance_change:
                break
            if abs(loss - prev_loss) < tolerance_change:
                break
        state.update({"d": d, "t": t, "old_dirs": old_dirs, "old_stps": old_stps, "ro": ro, "H_diag": H_diag,
                      "prev_flat_grad": prev_flat_grad, "prev_loss": prev_loss})
        return orig_loss


class optimizer:
    """`torch.optim.optimizer` as an attribute namespace (the module itself is
    not bundled): `Optimizer` and `required`."""
    Optimizer = Optimizer
    required = required


# PyTorch's torch.optim imports torch.utils (hooks, foreach helpers) and
# torch._utils; doing the same makes `torch.utils.data` and the checkpoint
# hooks in `torch._utils` reachable after a bare `import torch`.
import torch.utils
import torch._utils
import torch.optim.lr_scheduler as lr_scheduler
