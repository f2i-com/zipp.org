# torch.optim's Optimizer API: parameter groups, option validation, state
# dicts, closures and hooks. Prints lines that must equal what CPU PyTorch
# 2.11 prints (groups_expected.txt, from `python groups_cases.py`).
import torch
from torch import nn

OPTIMIZERS = ["SGD", "Adam", "AdamW", "RMSprop", "Adagrad", "Adamax", "NAdam", "RAdam", "Adadelta", "ASGD", "Rprop"]


def make(name, params, **kw):
    if name == "SGD":
        kw.setdefault("lr", 0.1)
    return getattr(torch.optim, name)(params, **kw)


def err(fn):
    try:
        fn()
        return "ok"
    except (TypeError, ValueError, RuntimeError, KeyError) as e:
        return "%s: %s" % (type(e).__name__, str(e).split("\n")[0])


# -- step hooks on an optimizer that inherits its step (AdamW from Adam),
# before any Adam exists
hooked = torch.zeros(2, requires_grad=True)
adamw = torch.optim.AdamW([hooked], lr=0.1)
adamw.register_step_post_hook(lambda o, args, kwargs: print("AdamW post hook", type(o).__name__, len(args)))
hooked.grad = torch.ones(2)
adamw.step()

# -- parameter groups
m1, m2 = nn.Linear(1, 1, bias=False), nn.Linear(1, 1, bias=False)
with torch.no_grad():
    m1.weight.fill_(1.0)
    m2.weight.fill_(1.0)
opt = torch.optim.SGD(m1.parameters(), lr=0.1)
opt.add_param_group({"params": m2.parameters(), "lr": 0.2})
for _ in range(3):
    m2.weight.grad = torch.ones(1, 1)
    opt.step()
print("generator group", round(m2.weight.item(), 6), type(opt.param_groups[1]["params"]).__name__, opt.param_groups[1]["momentum"])
w = torch.ones(2, requires_grad=True)
opt = torch.optim.SGD([{"params": w}], lr=0.5)
w.grad = torch.tensor([1.0, 1.0])
opt.step()
print("bare tensor group", w.tolist(), type(opt.param_groups[0]["params"]).__name__)
print("tensor params", err(lambda: torch.optim.SGD(w, lr=0.1)))
print("empty params", err(lambda: torch.optim.Adam([])))
print("set params", err(lambda: torch.optim.SGD([{"params": {w}}], lr=0.1)))
print("non-tensor", err(lambda: torch.optim.SGD([w, 3], lr=0.1)))
print("non-leaf", err(lambda: torch.optim.SGD([w * 2], lr=0.1)))
print("overlap", err(lambda: torch.optim.SGD([{"params": [w]}, {"params": [w]}], lr=0.1)))
opt = torch.optim.SGD([w], lr=0.1)
print("overlap add", err(lambda: opt.add_param_group({"params": [w]})), len(opt.param_groups))
print("group not dict", err(lambda: opt.add_param_group([w])))
named = torch.optim.SGD([("a", torch.zeros(1, requires_grad=True)), ("b", torch.zeros(1, requires_grad=True))], lr=0.1)
print("named", named.param_groups[0]["param_names"], err(lambda: named.add_param_group({"params": [torch.zeros(1, requires_grad=True)]})))
dup = torch.zeros(1, requires_grad=True)
dup_opt = torch.optim.SGD([dup, dup], lr=0.1)
dup.grad = torch.ones(1)
dup_opt.step()
print("duplicate", dup.tolist(), len(dup_opt.param_groups[0]["params"]))

# -- group keys, defaults and repr match PyTorch
for name in OPTIMIZERS:
    p = torch.zeros(2, requires_grad=True)
    o = make(name, [p])
    print(name, list(o.param_groups[0].keys()), list(o.defaults.keys()) == [k for k in o.param_groups[0] if k != "params"])
print(repr(torch.optim.SGD([torch.zeros(1, requires_grad=True)], lr=0.1, momentum=0.9)))

# -- option validation
checks = [
    ("SGD lr", lambda p: torch.optim.SGD(p, lr=-0.1)),
    ("SGD momentum", lambda p: torch.optim.SGD(p, lr=0.1, momentum=-0.5)),
    ("SGD weight_decay", lambda p: torch.optim.SGD(p, lr=0.1, weight_decay=-1)),
    ("SGD nesterov no momentum", lambda p: torch.optim.SGD(p, lr=0.1, nesterov=True)),
    ("SGD nesterov dampening", lambda p: torch.optim.SGD(p, lr=0.1, momentum=0.9, dampening=0.1, nesterov=True)),
    ("SGD fused foreach", lambda p: torch.optim.SGD(p, lr=0.1, fused=True, foreach=True)),
    ("Adam lr", lambda p: torch.optim.Adam(p, lr=-1e-3)),
    ("Adam eps", lambda p: torch.optim.Adam(p, eps=-1e-8)),
    ("Adam beta0", lambda p: torch.optim.Adam(p, betas=(1.0, 0.999))),
    ("Adam beta1", lambda p: torch.optim.Adam(p, betas=(0.9, -0.1))),
    ("Adam weight_decay", lambda p: torch.optim.Adam(p, weight_decay=-0.1)),
    ("Adam fused differentiable", lambda p: torch.optim.Adam(p, fused=True, differentiable=True)),
    ("AdamW beta1", lambda p: torch.optim.AdamW(p, betas=(0.9, 1.5))),
    ("RMSprop alpha", lambda p: torch.optim.RMSprop(p, alpha=-0.1)),
    ("RMSprop momentum", lambda p: torch.optim.RMSprop(p, momentum=-0.1)),
    ("RMSprop eps", lambda p: torch.optim.RMSprop(p, eps=-0.1)),
    ("Adagrad lr_decay", lambda p: torch.optim.Adagrad(p, lr_decay=-1)),
    ("Adagrad init", lambda p: torch.optim.Adagrad(p, initial_accumulator_value=-1)),
    ("Adamax beta", lambda p: torch.optim.Adamax(p, betas=(0.9, 1.0))),
    ("NAdam momentum_decay", lambda p: torch.optim.NAdam(p, momentum_decay=-1)),
    ("RAdam eps", lambda p: torch.optim.RAdam(p, eps=-1)),
    ("Adadelta rho", lambda p: torch.optim.Adadelta(p, rho=1.5)),
    ("ASGD weight_decay", lambda p: torch.optim.ASGD(p, weight_decay=-1)),
    ("Rprop etas", lambda p: torch.optim.Rprop(p, etas=(1.5, 1.2))),
    ("kwargs accepted", lambda p: [torch.optim.SGD(p, lr=0.1, foreach=True, differentiable=False, fused=False),
                                   torch.optim.Adam(p, foreach=False, fused=False, capturable=True, differentiable=False,
                                                    decoupled_weight_decay=True),
                                   torch.optim.Adagrad(p, fused=None, foreach=None)]),
]
for label, fn in checks:
    print(label, "->", err(lambda: fn([torch.zeros(2, requires_grad=True)])))
print("decoupled flag", torch.optim.Adam([torch.zeros(1, requires_grad=True)], decoupled_weight_decay=True).param_groups[0]["decoupled_weight_decay"],
      torch.optim.AdamW([torch.zeros(1, requires_grad=True)]).param_groups[0]["decoupled_weight_decay"])


# -- state dicts
def loss_of(w):
    return ((w - 0.3) ** 2).sum() + (w ** 3).sum() * 0.1


def train(opt, w, steps):
    for _ in range(steps):
        opt.zero_grad()
        loss_of(w).backward()
        opt.step()


for name in OPTIMIZERS:
    w = torch.tensor([1.0, -2.0, 0.5], requires_grad=True)
    o = make(name, [w])
    before = sorted(o.state_dict()["state"].keys())
    train(o, w, 3)
    sd = o.state_dict()
    keys = sorted(sd["state"].get(0, {}).keys())
    reloaded_w = w.detach().clone().requires_grad_(True)
    o2 = make(name, [reloaded_w])
    # Through a checkpoint: PyTorch's state_dict() aliases the live state
    # tensors, which its in-place updates would then share.
    torch.save(sd, "optim_state.pt")
    o2.load_state_dict(torch.load("optim_state.pt"))
    train(o, w, 3)
    train(o2, reloaded_w, 3)
    print("state", name, before, keys, int(o2.state[reloaded_w].get("step", -1)), torch.equal(w.detach(), reloaded_w.detach()))

a, b = torch.ones(2, requires_grad=True), torch.ones(2, requires_grad=True)
opt = torch.optim.Adam([a, b], lr=0.1)
a.grad = torch.ones(2)
b.grad = torch.ones(2)
opt.step()
print("mismatch size", err(lambda: opt.load_state_dict(torch.optim.Adam([a], lr=0.1).state_dict())), len(opt.state))
print("mismatch groups", err(lambda: opt.load_state_dict(torch.optim.Adam([{"params": [a]}, {"params": [b]}], lr=0.1).state_dict())), len(opt.state))
sd = opt.state_dict()
sd["param_groups"][0]["lr"] = 0.5
opt.load_state_dict(sd)
print("loaded lr", opt.param_groups[0]["lr"], sorted(k for k in opt.param_groups[0] if k != "params") == sorted(opt.defaults))

# -- closures, zero_grad and hooks
w = torch.tensor([1.0, -2.0], requires_grad=True)
opt = torch.optim.SGD([w], lr=0.1)
seen = []


def closure():
    opt.zero_grad()
    seen.append(torch.is_grad_enabled())
    loss = (w * w).sum()
    loss.backward()
    return loss


loss = opt.step(closure)
print("closure", float(loss), seen, [round(v, 6) for v in w.tolist()], opt.step() is None)
opt.zero_grad(set_to_none=False)
print("zero_grad keep", w.grad.tolist())
opt.zero_grad()
print("zero_grad none", w.grad)
calls = []
pre = opt.register_step_pre_hook(lambda o, args, kwargs: calls.append(("pre", o is opt, len(args))))
post = opt.register_step_post_hook(lambda o, args, kwargs: calls.append(("post", o is opt)))
w.grad = torch.ones(2)
opt.step()
pre.remove()
opt.step()
post.remove()
opt.step()
print("hooks", calls)


class SignSGD(torch.optim.Optimizer):
    def __init__(self, params, lr=0.1):
        super().__init__(params, dict(lr=lr))

    @torch.no_grad()
    def step(self, closure=None):
        for group in self.param_groups:
            for p in group["params"]:
                if p.grad is not None:
                    p.add_(torch.sign(p.grad), alpha=-group["lr"])


w = torch.tensor([1.0, -2.0], requires_grad=True)
custom = SignSGD([w], lr=0.25)
custom.register_step_post_hook(lambda o, a, k: print("custom post hook"))
w.grad = torch.tensor([3.0, -0.5])
custom.step()
print("custom", w.tolist(), custom.defaults, isinstance(custom, torch.optim.Optimizer))
