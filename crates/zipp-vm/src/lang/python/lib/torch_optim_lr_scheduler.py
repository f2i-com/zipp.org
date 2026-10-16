"""torch.optim.lr_scheduler for Zipp, following PyTorch's schedulers.

A scheduler's `step()` is chainable: it derives the next learning rate from
each group's *current* `lr` (so two schedulers compose, and a manual change
to `group["lr"]` carries forward), while `step(epoch)` uses the closed form
from `base_lrs` where one exists. Construction records `initial_lr` in every
parameter group and takes the first step, so `last_epoch` is 0 and
`get_last_lr()` is valid immediately; `last_epoch=` resumes a schedule.
PyTorch's warnings (stepping before `optimizer.step()`, the deprecated epoch
argument) are not emitted."""
import math
from collections import Counter
import torch

__all__ = ["LambdaLR", "MultiplicativeLR", "StepLR", "MultiStepLR", "ConstantLR", "LinearLR", "ExponentialLR", "SequentialLR",
           "CosineAnnealingLR", "ChainedScheduler", "ReduceLROnPlateau", "CyclicLR", "CosineAnnealingWarmRestarts", "OneCycleLR",
           "PolynomialLR", "LRScheduler"]

inf = math.inf


def _check_optimizer(optimizer):
    # Looked up at call time: this module loads while torch.optim is loading.
    if not isinstance(optimizer, torch.optim.Optimizer):
        raise TypeError("%s is not an Optimizer" % (type(optimizer).__name__,))


def _bisect_right(values, x):
    lo, hi = 0, len(values)
    while lo < hi:
        mid = (lo + hi) // 2
        if x < values[mid]:
            hi = mid
        else:
            lo = mid + 1
    return lo


def _copy(value):
    return value.clone() if isinstance(value, torch.Tensor) else value


def _format_param(name, optimizer, param):
    if isinstance(param, (list, tuple)):
        if len(param) != len(optimizer.param_groups):
            raise ValueError("%s must have the same length as optimizer.param_groups. %s has %d values, param_groups has %d."
                             % (name, name, len(param), len(optimizer.param_groups)))
    else:
        param = [param] * len(optimizer.param_groups)
    return [_copy(p) for p in param]


def _param_groups_val_list(optimizer, key):
    return [_copy(group[key]) for group in optimizer.param_groups]


def _update_param_group_val(param_group, key, val):
    if isinstance(param_group[key], torch.Tensor):
        param_group[key].fill_(val.item() if isinstance(val, torch.Tensor) else val)
    else:
        param_group[key] = val


def _is_plain_function(fn):
    return type(fn).__name__ == "function"


class LRScheduler:
    _get_lr_called_within_step = False
    _is_initial = False

    def __init__(self, optimizer, last_epoch=-1):
        _check_optimizer(optimizer)
        self.optimizer = optimizer
        if last_epoch == -1:
            for group in optimizer.param_groups:
                group.setdefault("initial_lr", _copy(group["lr"]))
        else:
            for i, group in enumerate(optimizer.param_groups):
                if "initial_lr" not in group:
                    raise KeyError("param 'initial_lr' is not specified in param_groups[%d] when resuming scheduler with last_epoch >= 0.\n"
                                   "This typically happens when:\n"
                                   "1. You're trying to resume training from a checkpoint but haven't properly loaded the optimizer state\n"
                                   "2. You're using last_epoch >= 0 for a fresh training run (not recommended)" % i)
        self.base_lrs = _param_groups_val_list(optimizer, "initial_lr")
        self.last_epoch = last_epoch
        self._initial_step()

    def _initial_step(self):
        self._step_count = 0
        self._is_initial = True
        try:
            self.step()
        finally:
            self._is_initial = False

    def state_dict(self):
        return {key: value for key, value in self.__dict__.items() if key != "optimizer"}

    def load_state_dict(self, state_dict):
        for key, value in state_dict.items():
            setattr(self, key, value)

    def get_last_lr(self):
        return self._last_lr

    def get_lr(self):
        raise NotImplementedError

    def step(self, epoch=None):
        self._step_count += 1
        self._update_lr(epoch)

    def _update_lr(self, epoch=None):
        self._get_lr_called_within_step = True
        try:
            if epoch is None:
                self.last_epoch += 1
                values = self.get_lr()
            else:
                self.last_epoch = epoch
                if hasattr(self, "_get_closed_form_lr"):
                    values = self._get_closed_form_lr()
                else:
                    values = self.get_lr()
        finally:
            self._get_lr_called_within_step = False
        groups = self.optimizer.param_groups
        if len(values) != len(groups):
            raise ValueError("zip() argument 2 is shorter than argument 1" if len(values) < len(groups) else "zip() argument 2 is longer than argument 1")
        for param_group, lr in zip(groups, values):
            _update_param_group_val(param_group, "lr", lr)
        self._last_lr = _param_groups_val_list(self.optimizer, "lr")


class _LRScheduler(LRScheduler):
    pass


def _lambdas(optimizer, lr_lambda):
    if not isinstance(lr_lambda, (list, tuple)):
        return [lr_lambda] * len(optimizer.param_groups)
    if len(lr_lambda) != len(optimizer.param_groups):
        raise ValueError("Expected %d lr_lambdas, but got %d" % (len(optimizer.param_groups), len(lr_lambda)))
    return list(lr_lambda)


def _lambda_state_dict(scheduler):
    state = {key: value for key, value in scheduler.__dict__.items() if key not in ("optimizer", "lr_lambdas")}
    state["lr_lambdas"] = [None if _is_plain_function(fn) else dict(fn.__dict__) for fn in scheduler.lr_lambdas]
    return state


def _lambda_load_state_dict(scheduler, state_dict):
    lr_lambdas = state_dict.pop("lr_lambdas")
    LRScheduler.load_state_dict(scheduler, state_dict)
    state_dict["lr_lambdas"] = lr_lambdas
    for idx, fn in enumerate(lr_lambdas):
        if fn is not None:
            for key, value in fn.items():
                setattr(scheduler.lr_lambdas[idx], key, value)


class LambdaLR(LRScheduler):
    def __init__(self, optimizer, lr_lambda, last_epoch=-1):
        self.optimizer = optimizer
        self.lr_lambdas = _lambdas(optimizer, lr_lambda)
        super().__init__(optimizer, last_epoch)

    def state_dict(self):
        return _lambda_state_dict(self)

    def load_state_dict(self, state_dict):
        _lambda_load_state_dict(self, state_dict)

    def get_lr(self):
        return [base_lr * lmbda(self.last_epoch) for lmbda, base_lr in zip(self.lr_lambdas, self.base_lrs)]


class MultiplicativeLR(LRScheduler):
    def __init__(self, optimizer, lr_lambda, last_epoch=-1):
        self.optimizer = optimizer
        self.lr_lambdas = _lambdas(optimizer, lr_lambda)
        for fn in self.lr_lambdas:
            if not callable(fn):
                raise TypeError("lr_lambda should be a function, but got %s" % (type(fn).__name__,))
        super().__init__(optimizer, last_epoch)

    def state_dict(self):
        return _lambda_state_dict(self)

    def load_state_dict(self, state_dict):
        _lambda_load_state_dict(self, state_dict)

    def get_lr(self):
        if not self._is_initial:
            return [group["lr"] * lmbda(self.last_epoch) for lmbda, group in zip(self.lr_lambdas, self.optimizer.param_groups)]
        return _param_groups_val_list(self.optimizer, "lr")


class StepLR(LRScheduler):
    def __init__(self, optimizer, step_size, gamma=0.1, last_epoch=-1):
        self.step_size = step_size
        self.gamma = gamma
        super().__init__(optimizer, last_epoch)

    def get_lr(self):
        if self.last_epoch == 0 or self.last_epoch % self.step_size != 0:
            return _param_groups_val_list(self.optimizer, "lr")
        return [group["lr"] * self.gamma for group in self.optimizer.param_groups]

    def _get_closed_form_lr(self):
        return [base_lr * self.gamma ** (self.last_epoch // self.step_size) for base_lr in self.base_lrs]


class MultiStepLR(LRScheduler):
    def __init__(self, optimizer, milestones, gamma=0.1, last_epoch=-1):
        self.milestones = Counter(milestones)
        self.gamma = gamma
        super().__init__(optimizer, last_epoch)

    def get_lr(self):
        if self.last_epoch not in self.milestones:
            return _param_groups_val_list(self.optimizer, "lr")
        return [group["lr"] * self.gamma ** self.milestones[self.last_epoch] for group in self.optimizer.param_groups]

    def _get_closed_form_lr(self):
        milestones = sorted(self.milestones.elements())
        return [base_lr * self.gamma ** _bisect_right(milestones, self.last_epoch) for base_lr in self.base_lrs]


class ConstantLR(LRScheduler):
    def __init__(self, optimizer, factor=1.0 / 3, total_iters=5, last_epoch=-1):
        if factor > 1.0 or factor < 0:
            raise ValueError("Constant multiplicative factor expected to be between 0 and 1.")
        self.factor = factor
        self.total_iters = total_iters
        super().__init__(optimizer, last_epoch)

    def get_lr(self):
        if self.last_epoch == 0:
            return [group["lr"] * self.factor for group in self.optimizer.param_groups]
        if self.last_epoch != self.total_iters:
            return _param_groups_val_list(self.optimizer, "lr")
        return [group["lr"] * (1.0 / self.factor) for group in self.optimizer.param_groups]

    def _get_closed_form_lr(self):
        return [base_lr * (self.factor + (self.last_epoch >= self.total_iters) * (1 - self.factor)) for base_lr in self.base_lrs]


class LinearLR(LRScheduler):
    def __init__(self, optimizer, start_factor=1.0 / 3, end_factor=1.0, total_iters=5, last_epoch=-1):
        if start_factor > 1.0 or start_factor <= 0:
            raise ValueError("Starting multiplicative factor expected to be greater than 0 and less or equal to 1.")
        if end_factor > 1.0 or end_factor < 0:
            raise ValueError("Ending multiplicative factor expected to be between 0 and 1.")
        self.start_factor = start_factor
        self.end_factor = end_factor
        self.total_iters = total_iters
        super().__init__(optimizer, last_epoch)

    def get_lr(self):
        if self.last_epoch == 0:
            return [group["lr"] * self.start_factor for group in self.optimizer.param_groups]
        if self._is_initial or self.last_epoch > self.total_iters:
            return _param_groups_val_list(self.optimizer, "lr")
        return [group["lr"] * (1.0 + (self.end_factor - self.start_factor)
                               / (self.total_iters * self.start_factor + (self.last_epoch - 1) * (self.end_factor - self.start_factor)))
                for group in self.optimizer.param_groups]

    def _get_closed_form_lr(self):
        return [base_lr * (self.start_factor + (self.end_factor - self.start_factor) * min(self.total_iters, self.last_epoch) / self.total_iters)
                for base_lr in self.base_lrs]


class ExponentialLR(LRScheduler):
    def __init__(self, optimizer, gamma, last_epoch=-1):
        self.gamma = gamma
        super().__init__(optimizer, last_epoch)

    def get_lr(self):
        if self._is_initial:
            return _param_groups_val_list(self.optimizer, "lr")
        return [group["lr"] * self.gamma for group in self.optimizer.param_groups]

    def _get_closed_form_lr(self):
        return [base_lr * self.gamma ** self.last_epoch for base_lr in self.base_lrs]


def _check_schedulers(owner, optimizer, schedulers):
    for idx, scheduler in enumerate(schedulers):
        if not hasattr(scheduler, "optimizer"):
            raise TypeError("%s at index %d should have `optimizer` as its attribute." % (owner, idx))
        if isinstance(scheduler, ReduceLROnPlateau):
            raise ValueError("%s does not support `ReduceLROnPlateau` scheduler as it requires additional kwargs to be specified when "
                             "calling `step`, but got one at index %d in the given schedulers sequence." % (owner, idx))
        if optimizer is not scheduler.optimizer:
            raise ValueError("%s expects all schedulers to belong to the same optimizer, but got scheduler %s at index %d has %s, which "
                             "is different from %s." % (owner, type(scheduler).__name__, idx, scheduler.optimizer, type(optimizer).__name__))


def _nested_state_dict(scheduler):
    state = {key: value for key, value in scheduler.__dict__.items() if key not in ("optimizer", "_schedulers")}
    state["_schedulers"] = [s.state_dict() for s in scheduler._schedulers]
    return state


def _nested_load_state_dict(scheduler, state_dict):
    schedulers = state_dict.pop("_schedulers")
    for key, value in state_dict.items():
        setattr(scheduler, key, value)
    state_dict["_schedulers"] = schedulers
    for idx, s in enumerate(schedulers):
        scheduler._schedulers[idx].load_state_dict(s)


class SequentialLR(LRScheduler):
    def __init__(self, optimizer, schedulers, milestones, last_epoch=-1):
        if len(schedulers) < 1:
            raise ValueError("%s expects at least one scheduler, but got no scheduler." % type(self).__name__)
        _check_schedulers(type(self).__name__, optimizer, schedulers)
        if len(milestones) != len(schedulers) - 1:
            raise ValueError("Sequential Schedulers expects number of schedulers provided to be one more than the number of milestone "
                             "points, but got number of schedulers %d and the number of milestones to be equal to %d"
                             % (len(schedulers), len(milestones)))
        self._schedulers = schedulers
        self._milestones = milestones
        self.last_epoch = last_epoch + 1
        self.optimizer = optimizer
        # Undo the steps the schedulers took when they were built, then
        # start the first one again from the initial learning rates.
        for group in self.optimizer.param_groups:
            _update_param_group_val(group, "lr", group["initial_lr"])
        self.recursive_undo()
        self._schedulers[0]._initial_step()
        self._last_lr = schedulers[0].get_last_lr()

    def recursive_undo(self, sched=None):
        scheds = self if sched is None else sched
        if hasattr(scheds, "_schedulers"):
            for s in scheds._schedulers:
                self.recursive_undo(s)
        elif hasattr(scheds, "last_epoch"):
            scheds.last_epoch -= 1

    def step(self):
        self.last_epoch += 1
        idx = _bisect_right(self._milestones, self.last_epoch)
        scheduler = self._schedulers[idx]
        if idx > 0 and self._milestones[idx - 1] == self.last_epoch:
            scheduler._update_lr(0)
        else:
            scheduler.step()
        self._last_lr = scheduler.get_last_lr()

    def state_dict(self):
        return _nested_state_dict(self)

    def load_state_dict(self, state_dict):
        _nested_load_state_dict(self, state_dict)


class PolynomialLR(LRScheduler):
    def __init__(self, optimizer, total_iters=5, power=1.0, last_epoch=-1):
        self.total_iters = total_iters
        self.power = power
        super().__init__(optimizer, last_epoch)

    def get_lr(self):
        if self._is_initial or self.last_epoch > self.total_iters:
            return _param_groups_val_list(self.optimizer, "lr")
        decay_factor = ((1.0 - self.last_epoch / self.total_iters) / (1.0 - (self.last_epoch - 1) / self.total_iters)) ** self.power
        return [group["lr"] * decay_factor for group in self.optimizer.param_groups]

    def _get_closed_form_lr(self):
        return [base_lr * (1.0 - min(self.total_iters, self.last_epoch) / self.total_iters) ** self.power for base_lr in self.base_lrs]


class CosineAnnealingLR(LRScheduler):
    def __init__(self, optimizer, T_max, eta_min=0.0, last_epoch=-1):
        self.T_max = T_max
        self.eta_min = eta_min
        super().__init__(optimizer, last_epoch)

    def get_lr(self):
        if self._is_initial:
            return _param_groups_val_list(self.optimizer, "lr")
        if self._step_count == 1 and self.last_epoch > 0:
            return [self.eta_min + (base_lr - self.eta_min) * (1 + math.cos(self.last_epoch * math.pi / self.T_max)) / 2
                    for base_lr in self.base_lrs]
        if (self.last_epoch - 1 - self.T_max) % (2 * self.T_max) == 0:
            return [group["lr"] + (base_lr - self.eta_min) * (1 - math.cos(math.pi / self.T_max)) / 2
                    for base_lr, group in zip(self.base_lrs, self.optimizer.param_groups)]
        return [(1 + math.cos(math.pi * self.last_epoch / self.T_max)) / (1 + math.cos(math.pi * (self.last_epoch - 1) / self.T_max))
                * (group["lr"] - self.eta_min) + self.eta_min for group in self.optimizer.param_groups]

    def _get_closed_form_lr(self):
        return [self.eta_min + (base_lr - self.eta_min) * (1 + math.cos(math.pi * self.last_epoch / self.T_max)) / 2 for base_lr in self.base_lrs]


class ChainedScheduler(LRScheduler):
    def __init__(self, schedulers, optimizer=None):
        if len(schedulers) < 1:
            raise ValueError("%s expects at least one scheduler to be chained, but got no scheduler." % type(self).__name__)
        optimizer = optimizer or schedulers[0].optimizer
        _check_schedulers(type(self).__name__, optimizer, schedulers)
        self._schedulers = schedulers
        self.optimizer = optimizer
        self._last_lr = _param_groups_val_list(self._schedulers[-1].optimizer, "lr")

    def step(self):
        for scheduler in self._schedulers:
            scheduler.step()
        self._last_lr = _param_groups_val_list(self._schedulers[-1].optimizer, "lr")

    def state_dict(self):
        return _nested_state_dict(self)

    def load_state_dict(self, state_dict):
        _nested_load_state_dict(self, state_dict)


class ReduceLROnPlateau(LRScheduler):
    def __init__(self, optimizer, mode="min", factor=0.1, patience=10, threshold=1e-4, threshold_mode="rel", cooldown=0, min_lr=0,
                 eps=1e-8):
        if factor >= 1.0:
            raise ValueError("Factor should be < 1.0.")
        self.factor = factor
        _check_optimizer(optimizer)
        self.optimizer = optimizer
        if isinstance(min_lr, (list, tuple)):
            if len(min_lr) != len(optimizer.param_groups):
                raise ValueError("expected %d min_lrs, got %d" % (len(optimizer.param_groups), len(min_lr)))
            self.default_min_lr = None
            self.min_lrs = list(min_lr)
        else:
            self.default_min_lr = min_lr
            self.min_lrs = [min_lr] * len(optimizer.param_groups)
        self.patience = patience
        self.cooldown = cooldown
        self.eps = eps
        self.last_epoch = 0
        self._last_lr = _param_groups_val_list(self.optimizer, "lr")
        self._init_is_better(mode=mode, threshold=threshold, threshold_mode=threshold_mode)
        self._reset()

    def _reset(self):
        self.best = self.mode_worse
        self.cooldown_counter = 0
        self.num_bad_epochs = 0

    def step(self, metrics, epoch=None):
        current = float(metrics)
        if epoch is None:
            epoch = self.last_epoch + 1
        self.last_epoch = epoch
        if self._is_better(current, self.best):
            self.best = current
            self.num_bad_epochs = 0
        else:
            self.num_bad_epochs += 1
        if self.in_cooldown:
            self.cooldown_counter -= 1
            self.num_bad_epochs = 0
        if self.num_bad_epochs > self.patience:
            self._reduce_lr(epoch)
            self.cooldown_counter = self.cooldown
            self.num_bad_epochs = 0
        self._last_lr = _param_groups_val_list(self.optimizer, "lr")

    def _reduce_lr(self, epoch):
        if len(self.optimizer.param_groups) != len(self.min_lrs):
            if self.default_min_lr is None:
                raise RuntimeError("The number of param groups in the `optimizer` (%d) differs from when `ReduceLROnPlateau` was initialized "
                                   "(%d), usually due to a new param group being added to the optimizer. Please modify the `min_lrs` field "
                                   "to match the length of the `optimizer` param groups." % (len(self.optimizer.param_groups), len(self.min_lrs)))
            self.min_lrs = [self.default_min_lr] * len(self.optimizer.param_groups)
        for i, param_group in enumerate(self.optimizer.param_groups):
            old_lr = float(param_group["lr"])
            new_lr = max(old_lr * self.factor, self.min_lrs[i])
            if old_lr - new_lr > self.eps:
                _update_param_group_val(param_group, "lr", new_lr)

    @property
    def in_cooldown(self):
        return self.cooldown_counter > 0

    def _is_better(self, a, best):
        if self.mode == "min" and self.threshold_mode == "rel":
            return a < best * (1.0 - self.threshold)
        if self.mode == "min" and self.threshold_mode == "abs":
            return a < best - self.threshold
        if self.mode == "max" and self.threshold_mode == "rel":
            return a > best * (self.threshold + 1.0)
        return a > best + self.threshold

    def _init_is_better(self, mode, threshold, threshold_mode):
        if mode not in ("min", "max"):
            raise ValueError("mode " + mode + " is unknown!")
        if threshold_mode not in ("rel", "abs"):
            raise ValueError("threshold mode " + threshold_mode + " is unknown!")
        self.mode_worse = inf if mode == "min" else -inf
        self.mode = mode
        self.threshold = threshold
        self.threshold_mode = threshold_mode

    def load_state_dict(self, state_dict):
        LRScheduler.load_state_dict(self, state_dict)
        self._init_is_better(mode=self.mode, threshold=self.threshold, threshold_mode=self.threshold_mode)


class CyclicLR(LRScheduler):
    def __init__(self, optimizer, base_lr, max_lr, step_size_up=2000, step_size_down=None, mode="triangular", gamma=1.0, scale_fn=None,
                 scale_mode="cycle", cycle_momentum=True, base_momentum=0.8, max_momentum=0.9, last_epoch=-1):
        _check_optimizer(optimizer)
        self.optimizer = optimizer
        base_lrs = _format_param("base_lr", optimizer, base_lr)
        if last_epoch == -1:
            for lr, group in zip(base_lrs, optimizer.param_groups):
                _update_param_group_val(group, "lr", lr)
        self.max_lrs = _format_param("max_lr", optimizer, max_lr)
        step_size_up = float(step_size_up)
        step_size_down = float(step_size_down) if step_size_down is not None else step_size_up
        self.total_size = step_size_up + step_size_down
        self.step_ratio = step_size_up / self.total_size
        if mode not in ("triangular", "triangular2", "exp_range") and scale_fn is None:
            raise ValueError("mode is invalid and scale_fn is None")
        self.mode = mode
        self.gamma = gamma
        self._scale_fn_custom = scale_fn
        self.scale_mode = scale_mode
        self._init_scale_fn()
        self.cycle_momentum = cycle_momentum
        if cycle_momentum:
            if "momentum" not in optimizer.defaults and "betas" not in optimizer.defaults:
                raise ValueError("optimizer must support momentum or beta1 with `cycle_momentum` option enabled")
            self.use_beta1 = "betas" in self.optimizer.defaults
            self.base_momentums = _format_param("base_momentum", optimizer, base_momentum)
            self.max_momentums = _format_param("max_momentum", optimizer, max_momentum)
            if last_epoch == -1:
                for m_momentum, b_momentum, group in zip(self.max_momentums, self.base_momentums, optimizer.param_groups):
                    if self.use_beta1:
                        group["betas"] = (m_momentum,) + tuple(group["betas"][1:])
                    else:
                        group["momentum"] = m_momentum
                    group["max_momentum"] = m_momentum
                    group["base_momentum"] = b_momentum
        super().__init__(optimizer, last_epoch)
        self.base_lrs = base_lrs

    def _init_scale_fn(self):
        if self._scale_fn_custom is not None:
            return
        if self.mode == "triangular":
            self.scale_mode = "cycle"
        elif self.mode == "triangular2":
            self.scale_mode = "cycle"
        elif self.mode == "exp_range":
            self.scale_mode = "iterations"

    def scale_fn(self, x):
        if self._scale_fn_custom is not None:
            return self._scale_fn_custom(x)
        if self.mode == "triangular2":
            return 1 / (2.0 ** (x - 1))
        if self.mode == "exp_range":
            return self.gamma ** x
        return 1.0

    def get_lr(self):
        cycle = math.floor(1 + self.last_epoch / self.total_size)
        x = 1.0 + self.last_epoch / self.total_size - cycle
        if x <= self.step_ratio:
            scale_factor = x / self.step_ratio
        else:
            scale_factor = (x - 1) / (self.step_ratio - 1)
        position = cycle if self.scale_mode == "cycle" else self.last_epoch
        lrs = [base_lr + (max_lr - base_lr) * scale_factor * self.scale_fn(position) for base_lr, max_lr in zip(self.base_lrs, self.max_lrs)]
        if self.cycle_momentum:
            momentums = [max_momentum - (max_momentum - base_momentum) * scale_factor * self.scale_fn(position)
                         for base_momentum, max_momentum in zip(self.base_momentums, self.max_momentums)]
            for param_group, momentum in zip(self.optimizer.param_groups, momentums):
                if self.use_beta1:
                    param_group["betas"] = (momentum,) + tuple(param_group["betas"][1:])
                else:
                    param_group["momentum"] = momentum
        return lrs

    def state_dict(self):
        state = LRScheduler.state_dict(self)
        fn = state.pop("_scale_fn_custom", None)
        state["_scale_fn_custom"] = None
        if fn is not None and not _is_plain_function(fn):
            state["_scale_fn_custom"] = dict(fn.__dict__)
        return state

    def load_state_dict(self, state_dict):
        fn = state_dict.pop("_scale_fn_custom")
        custom = self._scale_fn_custom
        LRScheduler.load_state_dict(self, state_dict)
        state_dict["_scale_fn_custom"] = fn
        self._scale_fn_custom = custom
        if fn is not None:
            for key, value in fn.items():
                setattr(self._scale_fn_custom, key, value)
        self._init_scale_fn()


class CosineAnnealingWarmRestarts(LRScheduler):
    def __init__(self, optimizer, T_0, T_mult=1, eta_min=0.0, last_epoch=-1):
        if T_0 <= 0 or not isinstance(T_0, int):
            raise ValueError("Expected positive integer T_0, but got %s" % (T_0,))
        if T_mult < 1 or not isinstance(T_mult, int):
            raise ValueError("Expected integer T_mult >= 1, but got %s" % (T_mult,))
        if not isinstance(eta_min, (float, int)):
            raise ValueError("Expected float or int eta_min, but got %s of type %s" % (eta_min, type(eta_min).__name__))
        self.T_0 = T_0
        self.T_i = T_0
        self.T_mult = T_mult
        self.eta_min = eta_min
        self.T_cur = last_epoch
        super().__init__(optimizer, last_epoch)

    def get_lr(self):
        return [self.eta_min + (base_lr - self.eta_min) * (1 + math.cos(math.pi * self.T_cur / self.T_i)) / 2 for base_lr in self.base_lrs]

    def step(self, epoch=None):
        if epoch is None and self.last_epoch < 0:
            epoch = 0
        if epoch is None:
            epoch = self.last_epoch + 1
            self.T_cur = self.T_cur + 1
            if self.T_cur >= self.T_i:
                self.T_cur = self.T_cur % self.T_i
                self.T_i = self.T_i * self.T_mult
        else:
            if epoch < 0:
                raise ValueError("Expected non-negative epoch, but got %s" % (epoch,))
            if epoch >= self.T_0:
                if self.T_mult == 1:
                    self.T_cur = epoch % self.T_0
                else:
                    n = int(math.log(epoch / self.T_0 * (self.T_mult - 1) + 1, self.T_mult))
                    self.T_cur = epoch - self.T_0 * (self.T_mult ** n - 1) / (self.T_mult - 1)
                    self.T_i = self.T_0 * self.T_mult ** n
            else:
                self.T_i = self.T_0
                self.T_cur = epoch
        self.last_epoch = math.floor(epoch)
        self._get_lr_called_within_step = True
        try:
            values = self.get_lr()
        finally:
            self._get_lr_called_within_step = False
        for param_group, lr in zip(self.optimizer.param_groups, values):
            _update_param_group_val(param_group, "lr", lr)
        self._last_lr = _param_groups_val_list(self.optimizer, "lr")


class OneCycleLR(LRScheduler):
    def __init__(self, optimizer, max_lr, total_steps=None, epochs=None, steps_per_epoch=None, pct_start=0.3, anneal_strategy="cos",
                 cycle_momentum=True, base_momentum=0.85, max_momentum=0.95, div_factor=25.0, final_div_factor=1e4, three_phase=False,
                 last_epoch=-1):
        _check_optimizer(optimizer)
        self.optimizer = optimizer
        if total_steps is not None:
            if total_steps <= 0 or not isinstance(total_steps, int):
                raise ValueError("Expected positive integer total_steps, but got %s" % (total_steps,))
            self.total_steps = total_steps
        elif epochs is not None and steps_per_epoch is not None:
            if not isinstance(epochs, int) or epochs <= 0:
                raise ValueError("Expected positive integer epochs, but got %s" % (epochs,))
            if not isinstance(steps_per_epoch, int) or steps_per_epoch <= 0:
                raise ValueError("Expected positive integer steps_per_epoch, but got %s" % (steps_per_epoch,))
            self.total_steps = epochs * steps_per_epoch
        else:
            raise ValueError("You must define either total_steps OR (epochs AND steps_per_epoch)")
        if three_phase:
            self._schedule_phases = [
                {"end_step": float(pct_start * self.total_steps) - 1, "start_lr": "initial_lr", "end_lr": "max_lr",
                 "start_momentum": "max_momentum", "end_momentum": "base_momentum"},
                {"end_step": float(2 * pct_start * self.total_steps) - 2, "start_lr": "max_lr", "end_lr": "initial_lr",
                 "start_momentum": "base_momentum", "end_momentum": "max_momentum"},
                {"end_step": self.total_steps - 1, "start_lr": "initial_lr", "end_lr": "min_lr",
                 "start_momentum": "max_momentum", "end_momentum": "max_momentum"},
            ]
        else:
            self._schedule_phases = [
                {"end_step": float(pct_start * self.total_steps) - 1, "start_lr": "initial_lr", "end_lr": "max_lr",
                 "start_momentum": "max_momentum", "end_momentum": "base_momentum"},
                {"end_step": self.total_steps - 1, "start_lr": "max_lr", "end_lr": "min_lr",
                 "start_momentum": "base_momentum", "end_momentum": "max_momentum"},
            ]
        if pct_start < 0 or pct_start > 1 or not isinstance(pct_start, float):
            raise ValueError("Expected float between 0 and 1 pct_start, but got %s" % (pct_start,))
        if anneal_strategy not in ("cos", "linear"):
            raise ValueError("anneal_strategy must be one of 'cos' or 'linear', instead got %s" % (anneal_strategy,))
        self._anneal_func_type = anneal_strategy
        max_lrs = _format_param("max_lr", self.optimizer, max_lr)
        if last_epoch == -1:
            for idx, group in enumerate(self.optimizer.param_groups):
                group["initial_lr"] = max_lrs[idx] / div_factor
                group["max_lr"] = max_lrs[idx]
                group["min_lr"] = group["initial_lr"] / final_div_factor
        self.cycle_momentum = cycle_momentum
        if self.cycle_momentum:
            if "momentum" not in self.optimizer.defaults and "betas" not in self.optimizer.defaults:
                raise ValueError("optimizer must support momentum or beta1 with `cycle_momentum` option enabled")
            self.use_beta1 = "betas" in self.optimizer.defaults
            max_momentums = _format_param("max_momentum", optimizer, max_momentum)
            base_momentums = _format_param("base_momentum", optimizer, base_momentum)
            if last_epoch == -1:
                for m_momentum, b_momentum, group in zip(max_momentums, base_momentums, optimizer.param_groups):
                    if self.use_beta1:
                        group["betas"] = (m_momentum,) + tuple(group["betas"][1:])
                    else:
                        group["momentum"] = m_momentum
                    group["max_momentum"] = m_momentum
                    group["base_momentum"] = b_momentum
        super().__init__(optimizer, last_epoch)

    def _anneal_func(self, start, end, pct):
        if self._anneal_func_type == "cos":
            return end + (start - end) / 2.0 * (math.cos(math.pi * pct) + 1)
        return (end - start) * pct + start

    def get_lr(self):
        lrs = []
        step_num = self.last_epoch
        if step_num > self.total_steps:
            raise ValueError("Tried to step %d times. The specified number of total steps is %d" % (step_num, self.total_steps))
        for group in self.optimizer.param_groups:
            start_step = 0.0
            computed_lr = computed_momentum = None
            for i, phase in enumerate(self._schedule_phases):
                end_step = phase["end_step"]
                if step_num <= end_step or i == len(self._schedule_phases) - 1:
                    pct = (step_num - start_step) / (end_step - start_step)
                    computed_lr = self._anneal_func(group[phase["start_lr"]], group[phase["end_lr"]], pct)
                    if self.cycle_momentum:
                        computed_momentum = self._anneal_func(group[phase["start_momentum"]], group[phase["end_momentum"]], pct)
                    break
                start_step = phase["end_step"]
            lrs.append(computed_lr)
            if self.cycle_momentum:
                if self.use_beta1:
                    group["betas"] = (computed_momentum,) + tuple(group["betas"][1:])
                else:
                    group["momentum"] = computed_momentum
        return lrs
