"""Training steps that exercise graph protocol version 4 under torch.compile:
slicing (basic, strided, integer, last-token), split/chunk/unbind, narrow,
flip, cat/stack, index_select (with a list or tensor index, and F.embedding),
gather and x[torch.arange(n), idx], eager slices and cats of parameters, and
a small transformer head that looks tokens up in an embedding table, adds a
sliced positional table, splits a fused qkv projection, attends causally and
reads the last position's logits.

`case` is defined before this file. It runs unchanged under PyTorch, which
generated selection_expected.json (gen.py): one `state()` vector per eager step
over `batches` (see python_torch_gpu3.rs)."""
import torch
from torch import nn
import torch.nn.functional as F

_seed = [2718]


def _uniform(lo, hi):
    # A fixed LCG: identical weights and batches under every Torch.
    _seed[0] = (_seed[0] * 1103515245 + 12345) % 2147483648
    return lo + (hi - lo) * _seed[0] / 2147483648


def _parameter(*shape):
    flat = [round(_uniform(-0.6, 0.6), 4) for _ in range(torch.Size(shape).numel())]
    return nn.Parameter(torch.tensor(flat).reshape(shape))


FEATURES, HIDDEN, CLASSES, BATCH, STEPS = 4, 6, 3, 5, 4
VOCAB, DIM, SEQ, MAXLEN = 9, 4, 5, 8
W1, b1 = _parameter(HIDDEN, FEATURES), _parameter(HIDDEN)
Wc, bc = _parameter(CLASSES, 15), _parameter(CLASSES)
Ws, bs = _parameter(CLASSES, 7), _parameter(CLASSES)
W2, b2 = _parameter(CLASSES, HIDDEN), _parameter(CLASSES)
Wa, Wb = _parameter(CLASSES, 2), _parameter(CLASSES, 2)
Wrow = _parameter(CLASSES, HIDDEN)
E, P = _parameter(VOCAB, DIM), _parameter(MAXLEN, DIM)
Wqkv, Wo = _parameter(3 * DIM, DIM), _parameter(CLASSES, DIM)

CASES = {
    # name: (parameters, optimizer)
    "transformer": ([E, P, Wqkv, Wo], "adam"),
    "gather": ([W1, b1, W2, b2], "sgd"),
    "cat": ([W1, b1, Wc, bc], "adam"),
    "strided": ([W1, b1, Ws, bs], "momentum"),
    "params": ([W1, b1, Wa, Wb, b2, Wrow], "sgd"),
}
params, optimizer_kind = CASES[case]

batches = []
for step in range(STEPS):
    if case == "transformer":
        tokens = torch.tensor([[int(_uniform(0, VOCAB)) for _ in range(SEQ)] for _ in range(2)])
        batches.append((tokens, torch.tensor([(step + i) % CLASSES for i in range(2)])))
    else:
        inputs = torch.tensor([[round(_uniform(-1.5, 1.5), 3) for _ in range(FEATURES)] for _ in range(BATCH)])
        batches.append((inputs, torch.tensor([(i * 2 + step) % CLASSES for i in range(BATCH)])))

if optimizer_kind == "sgd":
    optimizer = torch.optim.SGD(params, lr=0.1)
elif optimizer_kind == "momentum":
    optimizer = torch.optim.SGD(params, lr=0.05, momentum=0.9)
else:
    optimizer = torch.optim.Adam(params, lr=0.02)


def attend(s):
    # One sequence [SEQ, DIM]: a fused qkv projection split three ways, causal attention.
    q, k, v = F.linear(s, Wqkv).split(DIM, dim=-1)
    causal = torch.tril(torch.ones(SEQ, SEQ))
    att = F.softmax((q @ k.T / 2.0).masked_fill(causal == 0, -1e9), dim=-1)
    return att @ v


def forward(x, y):
    if case == "transformer":
        h = F.embedding(x, E) + P[:SEQ]
        last = torch.stack([attend(h[i])[-1] for i in range(h.shape[0])])
        return F.linear(last, Wo)
    h = F.linear(x, W1, b1)
    if case == "gather":
        logits = F.linear(torch.tanh(h), W2, b2)
        logp = F.log_softmax(logits, dim=-1)
        picked = logp.gather(1, y.unsqueeze(1)).squeeze(1) + logp[torch.arange(BATCH), y] * 0.5
        extra = logits.index_select(1, torch.tensor([2, 0, 2])).sum(1) * 0.1 + torch.take_along_dim(logits, y.unsqueeze(1), 1).squeeze(1) * 0.05
        return logits + (extra - picked).unsqueeze(1) * 0.2
    if case == "cat":
        a, b = h.chunk(2, 1)
        c = torch.cat([b, a * 0.5, h[:, ::2]], 1)
        d = torch.stack(h.unbind(1)[:3], 1)
        e = h.narrow(1, 1, 3).flip(1)
        return F.linear(torch.cat([torch.relu(c), d, torch.tanh(e)], dim=1), Wc, bc)
    if case == "strided":
        g = torch.cat([h[:, [5, 1, 3]], h[..., None][:, 4:5, 0], h[:, -1:], h.select(1, 0)[:, None], h[1:4][:, 2:3].sum() + h[:, 2:3]], 1)
        return F.linear(g, Ws, bs)
    # "params": eager slices and cats of parameters, and a parameter row lookup by the targets.
    w = torch.cat([Wa, Wb], 1)
    z = F.linear(h[:, ::3], Wa) + F.linear(h[:, 1:5:2], Wb) * 0.5 + F.linear(h[:, :4], w) * 0.25 + b2[:CLASSES]
    rows = torch.index_select(Wrow, 0, y)
    return z + (rows * h).sum(1, keepdim=True) * 0.1 + W1[0, :3].sum() * 0.1


def train_step(x, y):
    optimizer.zero_grad()
    loss = F.cross_entropy(forward(x, y), y)
    loss.backward()
    optimizer.step()
    return loss


def state(loss):
    values = [loss.item()]
    for p in params:
        values.extend(p.grad.flatten().tolist())
    for p in params:
        values.extend(p.detach().flatten().tolist())
    for p in params:
        entry = optimizer.state.get(p) or {}
        for key in ("momentum_buffer", "exp_avg", "exp_avg_sq"):
            if key in entry and entry[key] is not None:
                values.extend(entry[key].flatten().tolist())
        if "step" in entry:
            values.append(float(entry["step"]))
    return values
