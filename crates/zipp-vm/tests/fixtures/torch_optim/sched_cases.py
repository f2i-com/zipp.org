# Learning-rate schedules: each case records get_last_lr() (and, where a
# scheduler cycles momentum, the group's momentum or beta1) after
# construction and after each of 30 steps. `gen_expected.py` runs this under
# PyTorch 2.11 and writes sched_expected.json.
import math
import torch
from torch.optim import lr_scheduler
from torch.optim.lr_scheduler import (LambdaLR, MultiplicativeLR, StepLR, MultiStepLR, ConstantLR, LinearLR, ExponentialLR,
                                      PolynomialLR, CosineAnnealingLR, CosineAnnealingWarmRestarts, CyclicLR, OneCycleLR,
                                      ReduceLROnPlateau, SequentialLR, ChainedScheduler)

STEPS = 30


def _opt(kind="sgd", lr=0.1, groups=1):
    params = [torch.zeros(2, requires_grad=True) for _ in range(groups)]
    spec = [{"params": [p], "lr": lr * (i + 1)} for i, p in enumerate(params)]
    if kind == "adam":
        return torch.optim.Adam(spec, lr=lr)
    return torch.optim.SGD(spec, lr=lr, momentum=0.9)


def _record(opt, sched, steps=STEPS, momentum=False, before=None):
    out = list(sched.get_last_lr())
    for i in range(steps):
        if before is not None:
            before(i, opt)
        opt.step()
        sched.step()
        out.extend(sched.get_last_lr())
        if momentum:
            for g in opt.param_groups:
                out.append(g["betas"][0] if "betas" in g else g["momentum"])
    return out


def lambda_lr():
    opt = _opt(groups=2)
    return _record(opt, LambdaLR(opt, [lambda e: 0.95 ** e, lambda e: 1.0 / (1 + e)]))


def multiplicative():
    opt = _opt()
    return _record(opt, MultiplicativeLR(opt, lambda e: 0.9))


def step_lr():
    opt = _opt(groups=2)
    return _record(opt, StepLR(opt, step_size=4, gamma=0.5))


def multistep():
    opt = _opt()
    return _record(opt, MultiStepLR(opt, milestones=[3, 7, 7, 20], gamma=0.3))


def constant():
    opt = _opt()
    return _record(opt, ConstantLR(opt, factor=0.25, total_iters=6))


def linear():
    opt = _opt()
    return _record(opt, LinearLR(opt, start_factor=0.2, end_factor=0.9, total_iters=12))


def exponential():
    opt = _opt()
    return _record(opt, ExponentialLR(opt, gamma=0.87))


def polynomial():
    opt = _opt()
    return _record(opt, PolynomialLR(opt, total_iters=20, power=2.0))


def cosine():
    opt = _opt()
    return _record(opt, CosineAnnealingLR(opt, T_max=8, eta_min=0.01))


def warm_restarts():
    opt = _opt()
    return _record(opt, CosineAnnealingWarmRestarts(opt, T_0=3, T_mult=2, eta_min=0.001))


def cyclic_triangular2():
    opt = _opt()
    return _record(opt, CyclicLR(opt, base_lr=0.01, max_lr=0.1, step_size_up=4, step_size_down=3, mode="triangular2"), momentum=True)


def cyclic_exp_range_adam():
    opt = _opt("adam")
    return _record(opt, CyclicLR(opt, base_lr=0.001, max_lr=0.01, step_size_up=5, mode="exp_range", gamma=0.97), momentum=True)


def one_cycle_cos():
    opt = _opt()
    return _record(opt, OneCycleLR(opt, max_lr=0.5, total_steps=STEPS), steps=STEPS - 1, momentum=True)


def one_cycle_linear_three_phase_adam():
    opt = _opt("adam")
    return _record(opt, OneCycleLR(opt, max_lr=[0.3], epochs=5, steps_per_epoch=7, pct_start=0.25, anneal_strategy="linear",
                                   three_phase=True, div_factor=10.0, final_div_factor=100.0), momentum=True)


def plateau():
    opt = _opt(groups=2)
    sched = ReduceLROnPlateau(opt, mode="min", factor=0.5, patience=2, threshold=0.01, cooldown=1, min_lr=[0.02, 0.0])
    metrics = [1.0, 0.9, 0.95, 0.91, 0.92, 0.93, 0.5, 0.6, 0.6, 0.6, 0.6, 0.6, 0.4, 0.41, 0.42, 0.43, 0.44, 0.45, 0.46, 0.47]
    out = []
    for m in metrics:
        sched.step(m)
        out.extend(sched.get_last_lr())
    return out


def plateau_max_abs():
    opt = _opt()
    sched = ReduceLROnPlateau(opt, mode="max", factor=0.2, patience=1, threshold=0.1, threshold_mode="abs", eps=1e-3)
    out = []
    for m in [0.1, 0.15, 0.3, 0.35, 0.39, 0.5, 0.52, 0.55, 0.57, 0.58]:
        sched.step(torch.tensor(m))
        out.extend(sched.get_last_lr())
    return out


def sequential():
    opt = _opt()
    warm = LinearLR(opt, start_factor=0.1, total_iters=5)
    cos = CosineAnnealingLR(opt, T_max=10)
    decay = ExponentialLR(opt, gamma=0.9)
    return _record(opt, SequentialLR(opt, [warm, cos, decay], milestones=[5, 15]))


def chained():
    opt = _opt()
    return _record(opt, ChainedScheduler([ConstantLR(opt, factor=0.5, total_iters=4), ExponentialLR(opt, gamma=0.9)]))


def two_steplr():
    # Two schedulers on one optimizer compose multiplicatively.
    opt = _opt()
    a = StepLR(opt, step_size=1, gamma=0.5)
    b = StepLR(opt, step_size=3, gamma=0.1)
    out = []
    for _ in range(10):
        opt.step()
        a.step()
        b.step()
        out.append(opt.param_groups[0]["lr"])
    return out


def manual_change():
    # A manual lr change carries forward through a chainable scheduler.
    opt = _opt()
    sched = StepLR(opt, step_size=2, gamma=0.5)

    def bump(i, o):
        if i == 3:
            o.param_groups[0]["lr"] *= 10
    return _record(opt, sched, steps=10, before=bump)


def epoch_argument():
    # step(epoch) uses the closed form.
    opt = _opt()
    sched = StepLR(opt, step_size=3, gamma=0.5)
    out = []
    for e in [4, 7, 2, 9]:
        sched.step(e)
        out.extend(sched.get_last_lr())
    opt2 = _opt()
    cos = CosineAnnealingLR(opt2, T_max=10, eta_min=0.001)
    for e in [3, 5, 12]:
        cos.step(e)
        out.extend(cos.get_last_lr())
    return out


def resume_last_epoch():
    # A schedule rebuilt with last_epoch= continues from initial_lr.
    opt = _opt()
    first = CosineAnnealingLR(opt, T_max=12)
    for _ in range(5):
        opt.step()
        first.step()
    state = {"groups": [dict((k, v) for k, v in g.items() if k != "params") for g in opt.param_groups]}
    opt2 = _opt()
    for g, saved in zip(opt2.param_groups, state["groups"]):
        g.update(saved)
    second = CosineAnnealingLR(opt2, T_max=12, last_epoch=4)
    out = list(second.get_last_lr()) + [second.last_epoch]
    for _ in range(8):
        opt2.step()
        second.step()
        out.extend(second.get_last_lr())
    return out


def state_dict_round_trip():
    opt = _opt()
    sched = MultiStepLR(opt, milestones=[2, 5], gamma=0.5)
    for _ in range(3):
        opt.step()
        sched.step()
    sd = sched.state_dict()
    keys = sorted(sd.keys())
    opt2 = _opt()
    opt2.load_state_dict(opt.state_dict())
    sched2 = MultiStepLR(opt2, milestones=[1], gamma=0.9)
    sched2.load_state_dict(sd)
    out = [sched2.last_epoch] + list(sched2.get_last_lr())
    for _ in range(4):
        opt2.step()
        sched2.step()
        out.extend(sched2.get_last_lr())
    return out, keys


def results():
    out = {}
    for fn in [lambda_lr, multiplicative, step_lr, multistep, constant, linear, exponential, polynomial, cosine, warm_restarts,
               cyclic_triangular2, cyclic_exp_range_adam, one_cycle_cos, one_cycle_linear_three_phase_adam, plateau, plateau_max_abs,
               sequential, chained, two_steplr, manual_change, epoch_argument, resume_last_epoch]:
        out[fn.__name__] = [float(v) for v in fn()]
    values, keys = state_dict_round_trip()
    out["state_dict_round_trip"] = [float(v) for v in values]
    out["state_dict_keys"] = keys
    out["initial_lr_in_groups"] = sorted(k for k in _with_scheduler().param_groups[0].keys() if k != "params")
    out["aliases"] = [lr_scheduler.StepLR is StepLR, torch.optim.lr_scheduler.LambdaLR is LambdaLR,
                      issubclass(StepLR, lr_scheduler.LRScheduler), issubclass(lr_scheduler._LRScheduler, lr_scheduler.LRScheduler)]
    return out


def _with_scheduler():
    opt = _opt()
    StepLR(opt, 3)
    return opt
