# Optimizer trajectories: each case trains two parameter tensors on an
# elementwise loss (no matmul, so float32 rounding matches PyTorch closely)
# for eight steps and records every parameter value after every step, plus
# the closure's returned losses. `gen_expected.py` runs this file under
# PyTorch 2.11 and writes optim_expected.json; Zipp compares against it.
import torch


def _loss(w, b):
    return ((w - 0.3) ** 2 * torch.tensor([1.0, 2.0, 0.5])).sum() + (b * b).sum() * 0.7 + (w * w * w).sum() * 0.1


def trajectory(factory, steps=8, closure=False):
    w = torch.tensor([1.0, -2.0, 0.5], requires_grad=True)
    b = torch.tensor([0.25, -0.75], requires_grad=True)
    opt = factory([w, b])
    out = []
    for _ in range(steps):
        if closure:
            def closure_fn():
                opt.zero_grad()
                loss = _loss(w, b)
                loss.backward()
                return loss
            loss = opt.step(closure_fn)
            out.append(float(loss.detach()))
        else:
            opt.zero_grad()
            _loss(w, b).backward()
            opt.step()
        out.extend(w.detach().tolist())
        out.extend(b.detach().tolist())
    return out


CASES = {
    "sgd": lambda p: torch.optim.SGD(p, lr=0.05),
    "sgd_momentum_wd": lambda p: torch.optim.SGD(p, lr=0.05, momentum=0.9, dampening=0.1, weight_decay=0.01),
    "sgd_nesterov_max": lambda p: torch.optim.SGD(p, lr=0.02, momentum=0.8, nesterov=True, maximize=True),
    "sgd_foreach_fused": lambda p: torch.optim.SGD(p, lr=0.05, momentum=0.5, foreach=False, fused=None),
    "adam": lambda p: torch.optim.Adam(p, lr=0.1),
    "adam_amsgrad_wd": lambda p: torch.optim.Adam(p, lr=0.1, betas=(0.8, 0.9), weight_decay=0.1, amsgrad=True),
    "adam_decoupled": lambda p: torch.optim.Adam(p, lr=0.1, weight_decay=0.2, decoupled_weight_decay=True),
    "adamw": lambda p: torch.optim.AdamW(p, lr=0.1, weight_decay=0.2),
    "adamw_max_kwargs": lambda p: torch.optim.AdamW(p, lr=0.05, maximize=True, foreach=True, capturable=False, differentiable=False),
    "rmsprop": lambda p: torch.optim.RMSprop(p, lr=0.01),
    "rmsprop_centered_momentum": lambda p: torch.optim.RMSprop(p, lr=0.01, alpha=0.9, momentum=0.5, centered=True, weight_decay=0.05),
    "rmsprop_maximize": lambda p: torch.optim.RMSprop(p, lr=0.01, maximize=True),
    "adagrad": lambda p: torch.optim.Adagrad(p, lr=0.1),
    "adagrad_decay": lambda p: torch.optim.Adagrad(p, lr=0.1, lr_decay=0.05, weight_decay=0.1, initial_accumulator_value=0.2, eps=1e-6),
    "adamax": lambda p: torch.optim.Adamax(p, lr=0.1),
    "adamax_wd": lambda p: torch.optim.Adamax(p, lr=0.05, betas=(0.7, 0.95), weight_decay=0.1),
    "nadam": lambda p: torch.optim.NAdam(p, lr=0.05),
    "nadam_decoupled": lambda p: torch.optim.NAdam(p, lr=0.05, weight_decay=0.1, momentum_decay=0.01, decoupled_weight_decay=True),
    "nadam_wd": lambda p: torch.optim.NAdam(p, lr=0.05, weight_decay=0.1),
    "radam": lambda p: torch.optim.RAdam(p, lr=0.1),
    "radam_rect": lambda p: torch.optim.RAdam(p, lr=0.1, betas=(0.5, 0.6)),
    "radam_decoupled": lambda p: torch.optim.RAdam(p, lr=0.1, betas=(0.5, 0.6), weight_decay=0.1, decoupled_weight_decay=True),
    "adadelta": lambda p: torch.optim.Adadelta(p),
    "adadelta_wd": lambda p: torch.optim.Adadelta(p, lr=0.5, rho=0.8, weight_decay=0.1),
    "asgd": lambda p: torch.optim.ASGD(p, lr=0.05),
    "asgd_t0": lambda p: torch.optim.ASGD(p, lr=0.05, lambd=0.01, alpha=0.5, t0=3, weight_decay=0.01),
    "rprop": lambda p: torch.optim.Rprop(p, lr=0.05),
    "rprop_sizes": lambda p: torch.optim.Rprop(p, lr=0.2, etas=(0.3, 1.5), step_sizes=(0.01, 0.4)),
}
CLOSURE_CASES = ["sgd_momentum_wd", "adam", "rmsprop_centered_momentum", "adagrad", "adamax", "nadam", "radam", "adadelta", "asgd", "rprop"]
# L-BFGS steps only through a closure (each step runs several iterations).
LBFGS_CASES = {
    "lbfgs": lambda p: torch.optim.LBFGS(p, lr=0.5, max_iter=4),
    "lbfgs_history": lambda p: torch.optim.LBFGS(p, lr=1, max_iter=3, history_size=2),
    "lbfgs_wolfe": lambda p: torch.optim.LBFGS(p, max_iter=5, line_search_fn="strong_wolfe"),
}


def results():
    out = {}
    for name, factory in CASES.items():
        out[name] = trajectory(factory)
    for name in CLOSURE_CASES:
        out["closure_" + name] = trajectory(CASES[name], steps=6, closure=True)
    for name, factory in LBFGS_CASES.items():
        out[name] = trajectory(factory, steps=3, closure=True)
    return out
