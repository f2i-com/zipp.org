"""float16/bfloat16 storage, view(dtype), optimizer and 0-d promotion cases,
run unchanged by CPython with PyTorch 2.11 and by Zipp
(python_torch_dtypes2.rs). Regenerate the expected lines, from this
directory, with:
    python dtypes2_cases.py > dtypes2_expected.txt
"""
import torch


def vals(n, seed):
    return [((i * 37 + seed * 11) % 23 - 11) / 7.0 + (seed % 5) * 0.0137 for i in range(n)]


def bits_sum(t):
    # An exact fingerprint of a 2-byte tensor's elements.
    acc = 0
    for i, b in enumerate(t.detach().view(torch.int16).tolist()):
        acc = (acc * 1000003 + (b & 0xffff) + i) % 2305843009213693951
    return acc


CONFIGS = [
    ("SGD", dict(lr=0.1)), ("SGD", dict(lr=0.05, momentum=0.9)), ("SGD", dict(lr=0.05, momentum=0.9, nesterov=True, weight_decay=0.01)),
    ("SGD", dict(lr=0.03, momentum=0.5, dampening=0.2, maximize=True)),
    ("Adam", dict(lr=0.01)), ("Adam", dict(lr=0.01, weight_decay=0.1, amsgrad=True)), ("AdamW", dict(lr=0.01)),
    ("AdamW", dict(lr=0.02, amsgrad=True, weight_decay=0.3)), ("RMSprop", dict(lr=0.01)),
    ("RMSprop", dict(lr=0.01, momentum=0.9, centered=True, weight_decay=0.01)),
    ("Adagrad", dict(lr=0.1, lr_decay=0.01, weight_decay=0.01, initial_accumulator_value=0.1)),
    ("Adamax", dict(lr=0.02, weight_decay=0.01)), ("NAdam", dict(lr=0.02)),
    ("NAdam", dict(lr=0.02, weight_decay=0.01, decoupled_weight_decay=True)), ("RAdam", dict(lr=0.02)),
    ("RAdam", dict(lr=0.02, weight_decay=0.01, decoupled_weight_decay=True)), ("Adadelta", dict(lr=1.0, weight_decay=0.01)),
    ("ASGD", dict(lr=0.05, t0=2, weight_decay=0.01)), ("Rprop", dict(lr=0.01)),
]


def optimizer_lines():
    """float16/bfloat16 parameters (64 elements: PyTorch's vectorized CPU
    loop covers every element) after 8 steps of each optimizer, as an exact
    fingerprint of their bits."""
    out = []
    for dt in (torch.float16, torch.bfloat16):
        for name, kw in CONFIGS:
            p = torch.nn.Parameter(torch.tensor(vals(64, 1), dtype=torch.float64).to(dt))
            opt = getattr(torch.optim, name)([p], **kw)
            for step in range(8):
                p.grad = torch.tensor(vals(64, step + 2), dtype=torch.float64).to(dt) * (0.3 if step % 2 else 1.7)
                opt.step()
            out.append("%s %s %s %d" % (str(dt), name, sorted(kw.items()), bits_sum(p)))
    return out


def view_lines():
    out = []
    h = torch.tensor([1.5, 2.5, -0.0, 65504.0], dtype=torch.half)
    i = h.view(torch.int16)
    i[0] = 15360
    h[1] = -2.0
    out.append(repr([h.tolist(), i.tolist(), h.view(torch.bfloat16).tolist()]))
    b = torch.tensor([1.5, -3.0], dtype=torch.bfloat16)
    bv = b.view(torch.float16)
    bv.fill_(0.0)
    out.append(repr([b.tolist(), bv.dtype, bv.shape]))
    u = torch.tensor([255, 1, 128], dtype=torch.uint8)
    s8 = u.view(torch.int8)
    s8[1] = -1
    out.append(repr([u.tolist(), s8.tolist()]))
    f = torch.arange(4, dtype=torch.float32)
    out.append(repr([f.view(torch.int16).tolist(), list(f.view(torch.float16).shape), f.view(torch.float64).tolist(), f.view(torch.int32).tolist()]))
    out.append(repr([torch.tensor([15360, 16384, 16896, 17408], dtype=torch.int16).view(torch.float32).tolist(),
                     torch.arange(4, dtype=torch.int16).view(torch.int32).tolist(), torch.tensor([1.0, 2.0], dtype=torch.half).view(torch.uint8).tolist()]))
    for fn in (lambda: torch.tensor(1.0).view(torch.int16), lambda: torch.arange(3.).view(torch.float64)):
        try:
            fn()
            out.append("no error")
        except RuntimeError as e:
            out.append("RuntimeError " + str(e))
    return out


def storage_lines():
    out = []
    for dt in (torch.float16, torch.bfloat16, torch.float32, torch.int16):
        t = torch.zeros(1000, dtype=dt)
        out.append(repr([str(dt), t.element_size(), t.nbytes, t.itemsize, t.untyped_storage().nbytes()]))
    return out


def grad_lines():
    # A 0-d operand that loses the type promotion still passes its gradient.
    out = []
    for dt in (torch.bfloat16, torch.float16):
        w = torch.ones(3, requires_grad=True)
        o = torch.ones(2, requires_grad=True)
        (o.sum() + w.to(dt).sum() * 0.5).backward()
        out.append(repr([w.grad.tolist(), o.grad.tolist()]))
        w = torch.ones(3, requires_grad=True)
        (w.to(dt).sum() * torch.tensor(3.0)).backward()
        out.append(repr(w.grad.tolist()))
    return out


def lines():
    return optimizer_lines() + view_lines() + storage_lines() + grad_lines()


if __name__ == "__main__":
    for line in lines():
        print(line)
