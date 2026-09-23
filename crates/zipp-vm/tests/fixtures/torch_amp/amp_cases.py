# torch.amp parity cases, run unchanged under PyTorch 2.11 (gen.py writes
# amp_expected.json and amp_lines_expected.txt) and under Zipp
# (python_torch_amp.rs). GradScaler runs with device="cpu": the oracle
# machine has CUDA, Zipp does not, so the CUDA path (which disables itself
# without CUDA) is checked separately against PyTorch's documented rules.
import math

import torch
import torch.amp as amp

D = torch.float64


def _loss(w, b, k):
    x = torch.tensor([1.0, -2.0, 0.5], dtype=w.dtype) * (1.0 + 0.1 * k)
    return ((w * x - 0.3) ** 2).sum() + (b * b).sum() * 0.7 + (w * w * w).sum() * 0.1


def train(dtype, opt_name, steps, bad_steps, unscale_first=False, growth_interval=3):
    w = torch.tensor([1.0, -2.0, 0.5], dtype=dtype, requires_grad=True)
    b = torch.tensor([0.25, -0.75], dtype=dtype, requires_grad=True)
    opt = torch.optim.SGD([w, b], lr=0.05) if opt_name == "sgd" else torch.optim.Adam([w, b], lr=0.1)
    scaler = amp.GradScaler("cpu", init_scale=2.0 ** 6, growth_factor=2.0, backoff_factor=0.25, growth_interval=growth_interval)
    out = [scaler.get_scale()]
    for k in range(steps):
        opt.zero_grad()
        loss = _loss(w, b, k)
        if k in bad_steps:
            loss = loss * (math.inf if k % 2 else math.nan)
        scaler.scale(loss).backward()
        if unscale_first:
            scaler.unscale_(opt)
            out.extend(w.grad.tolist())
            torch.nn.utils.clip_grad_norm_([w, b], 1.5)
        ret = scaler.step(opt)
        out.append(0.0 if ret is None else 1.0)
        scaler.update()
        out.append(scaler.get_scale())
        out.append(float(scaler._get_growth_tracker()))
        out.extend(w.detach().tolist())
        out.extend(b.detach().tolist())
        g = w.grad.tolist()
        out.extend([v if math.isfinite(v) else 777.0 for v in g])
    sd = scaler.state_dict()
    out.extend([sd["scale"], sd["growth_factor"], sd["backoff_factor"], float(sd["growth_interval"]), float(sd["_growth_tracker"])])
    return out


def results():
    R = {}
    R["sgd_f64"] = train(D, "sgd", 9, (2, 5))
    R["adam_f64"] = train(D, "adam", 9, (4,))
    R["sgd_f32"] = train(torch.float32, "sgd", 7, (1,))
    R["unscale_clip"] = train(D, "sgd", 5, (3,), unscale_first=True)
    R["growth_interval_1"] = train(D, "sgd", 4, (), growth_interval=1)

    # Two optimizers, one with an inf gradient: only its step is skipped,
    # and update() backs the scale off once.
    p = torch.tensor([1.0, 2.0], dtype=D, requires_grad=True)
    q = torch.tensor([3.0], dtype=D, requires_grad=True)
    o1 = torch.optim.SGD([p], lr=0.1)
    o2 = torch.optim.SGD([q], lr=0.1)
    s = amp.GradScaler("cpu", init_scale=8.0, growth_interval=2)
    out = []
    for k in range(4):
        o1.zero_grad()
        o2.zero_grad()
        l1 = (p * p).sum()
        l2 = (q * q).sum() * (math.inf if k == 1 else 1.0)
        s.scale([l1, l2])[0].backward()
        s.scale((l1, l2))[1].backward()
        s.step(o1)
        s.step(o2)
        s.update()
        out += [s.get_scale()] + p.detach().tolist() + q.detach().tolist()
    R["two_optimizers"] = out

    # update(new_scale=...) with a float and a tensor; state_dict round trip.
    s = amp.GradScaler("cpu", init_scale=4.0)
    t = torch.ones(2, dtype=D, requires_grad=True)
    o = torch.optim.SGD([t], lr=0.1)
    out = [s.get_scale(), float(s._get_growth_tracker())]
    s.scale(t.sum()).backward()
    s.step(o)
    s.update(3.5)
    out.append(s.get_scale())
    o.zero_grad()
    s.scale(t.sum()).backward()
    s.step(o)
    s.update(torch.tensor(1.25))
    out.append(s.get_scale())
    s2 = amp.GradScaler("cpu", init_scale=100.0, growth_factor=3.0)
    s2.load_state_dict(s.state_dict())
    sd = s2.state_dict()
    out += [sd["scale"], sd["growth_factor"], sd["backoff_factor"], float(sd["growth_interval"]), float(sd["_growth_tracker"]), float(len(sd))]
    out += [float(s2.get_growth_factor()), float(s2.get_backoff_factor()), float(s2.get_growth_interval())]
    s2.set_growth_factor(4.0)
    s2.set_backoff_factor(0.125)
    s2.set_growth_interval(7)
    out += [float(s2.get_growth_factor()), float(s2.get_backoff_factor()), float(s2.get_growth_interval()), float(s2.is_enabled())]
    out += t.detach().tolist()
    R["update_and_state"] = out
    return R


def lines():
    """Printed behaviour: errors, disabled scalers, autocast state."""
    out = []

    def attempt(label, fn):
        try:
            r = fn()
            out.append("%s: ok %r" % (label, r))
        except Exception as e:
            out.append("%s: %s %s" % (label, type(e).__name__, str(e).split("\n")[0]))

    w = torch.ones(2, requires_grad=True)
    opt = torch.optim.SGD([w], lr=0.1)
    s = amp.GradScaler("cpu")
    attempt("step before scale", lambda: s.step(opt))
    attempt("update before scale", lambda: s.update())
    s.scale((w * 2).sum()).backward()
    s.unscale_(opt)
    attempt("unscale twice", lambda: s.unscale_(opt))
    s.step(opt)
    attempt("step twice", lambda: s.step(opt))
    attempt("unscale after step", lambda: s.unscale_(opt))
    attempt("closure", lambda: s.step(opt, closure=lambda: None))
    s.update()
    attempt("growth factor", lambda: amp.GradScaler("cpu", growth_factor=1.0))
    attempt("backoff factor", lambda: amp.GradScaler("cpu", backoff_factor=1.0))
    attempt("load empty", lambda: amp.GradScaler("cpu").load_state_dict({}))
    attempt("scale bad", lambda: amp.GradScaler("cpu").scale(3))
    empty = torch.optim.SGD([torch.ones(1, requires_grad=True)], lr=0.1)
    s3 = amp.GradScaler("cpu")
    s3.scale(torch.ones(1))
    attempt("no grads", lambda: s3.step(empty))

    # A disabled scaler is the identity.
    off = amp.GradScaler("cpu", enabled=False)
    x = torch.ones(2)
    out.append("disabled: %s %r %r %s %r" % (off.scale(x) is x, off.get_scale(), off.state_dict(), off.is_enabled(), off.update()))
    w2 = torch.tensor([1.0, 2.0], requires_grad=True)
    o2 = torch.optim.SGD([w2], lr=0.5)
    (w2 * w2).sum().backward()
    out.append("disabled step: %r %r" % (off.step(o2), w2.detach().tolist()))
    off.load_state_dict({})
    off.unscale_(o2)
    out.append("disabled grads: %r" % (w2.grad.tolist(),))

    # autocast state
    out.append("available: %s %s %s" % (amp.is_autocast_available("cpu"), amp.is_autocast_available("cuda"), amp.is_autocast_available("meta")))
    attempt("autocast int device", lambda: amp.autocast(1))
    attempt("autocast unknown device", lambda: amp.autocast("xyz"))
    out.append("default: %s %s %s" % (torch.is_autocast_enabled("cpu"), torch.get_autocast_dtype("cpu") == torch.bfloat16, torch.is_autocast_enabled()))
    with amp.autocast("cpu") as ctx:
        out.append("inside: %s %s %s %s" % (torch.is_autocast_enabled("cpu"), torch.get_autocast_dtype("cpu") == torch.bfloat16, ctx.fast_dtype == torch.bfloat16, isinstance(ctx, torch.autocast)))
        with torch.autocast("cpu", enabled=False):
            out.append("nested off: %s" % torch.is_autocast_enabled("cpu"))
        out.append("restored: %s" % torch.is_autocast_enabled("cpu"))
    out.append("after: %s" % torch.is_autocast_enabled("cpu"))

    @amp.autocast("cpu")
    def decorated(v):
        return torch.is_autocast_enabled("cpu"), v + 1

    out.append("decorated: %r %s %s" % (decorated(1), decorated.__name__, torch.is_autocast_enabled("cpu")))
    try:
        with amp.autocast("cpu"):
            raise KeyError("boom")
    except KeyError:
        out.append("exception restores: %s" % torch.is_autocast_enabled("cpu"))

    # custom_fwd / custom_bwd
    seen = []

    class Square(torch.autograd.Function):
        @staticmethod
        @amp.custom_fwd(device_type="cpu")
        def forward(ctx, x):
            seen.append(("fwd", torch.is_autocast_enabled("cpu")))
            ctx.save_for_backward(x)
            return x * x

        @staticmethod
        @amp.custom_bwd(device_type="cpu")
        def backward(ctx, g):
            seen.append(("bwd", torch.is_autocast_enabled("cpu")))
            x, = ctx.saved_tensors
            return 2 * x * g

    class Cube(torch.autograd.Function):
        @staticmethod
        @amp.custom_fwd(device_type="cpu", cast_inputs=torch.float32)
        def forward(ctx, x):
            seen.append(("cast fwd", torch.is_autocast_enabled("cpu"), x.dtype == torch.float32))
            ctx.save_for_backward(x)
            return x * x * x

        @staticmethod
        @amp.custom_bwd(device_type="cpu")
        def backward(ctx, g):
            seen.append(("cast bwd", torch.is_autocast_enabled("cpu")))
            x, = ctx.saved_tensors
            return 3 * x * x * g

    a = torch.tensor([1.5, -2.0], requires_grad=True)
    with amp.autocast("cpu"):
        y = Square.apply(a).sum() + Cube.apply(a).sum()
    y.backward()
    z = Square.apply(a).sum()
    z.backward()
    out.append("custom: %r %r" % (seen, a.grad.tolist()))
    attempt("custom_fwd device", lambda: amp.custom_fwd(lambda c, x: x, device_type=1))
    return out
