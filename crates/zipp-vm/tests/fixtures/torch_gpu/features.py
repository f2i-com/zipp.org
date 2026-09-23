"""Training steps that exercise what torch.compile(training=True) records
beyond dense layers: eager operations on parameters inside the step (an L2
penalty, tied weights, `W * 2`, `torch.add(h, b)`), stop-gradients, views and
permutations, tensor division, sqrt/rsqrt/abs/square, multi-dimensional
reductions, softmax over a non-last dimension, a hand-written layer norm and
vector (1-D) inputs.

`case` is defined before this file. It runs unchanged under PyTorch, which
generated features_expected.json: one `state()` vector per eager step over
`batches` (see python_torch_gpu.rs)."""
import torch
from torch import nn
import torch.nn.functional as F

_seed = [777]


def _uniform(lo, hi):
    # A fixed LCG: identical weights and batches under every Torch.
    _seed[0] = (_seed[0] * 1103515245 + 12345) % 2147483648
    return lo + (hi - lo) * _seed[0] / 2147483648


def _parameter(*shape):
    flat = [round(_uniform(-0.6, 0.6), 4) for _ in range(torch.Size(shape).numel())]
    return nn.Parameter(torch.tensor(flat).reshape(shape))


FEATURES, HIDDEN, CLASSES, BATCH, STEPS = 4, 6, 3, 5, 4
W1, b1 = _parameter(HIDDEN, FEATURES), _parameter(HIDDEN)
W2, b2 = _parameter(CLASSES, HIDDEN), _parameter(CLASSES)
gamma, beta = _parameter(HIDDEN), _parameter(HIDDEN)
CASES = {
    # name: (parameters, target kind, optimizer)
    "l2reg": ([W1, b1, W2, b2], "ce", "momentum"),
    "tied": ([W1, b1], "rec", "adam"),
    "eagerops": ([W1, b1, W2, b2], "ce", "adam"),
    "addparam": ([W1, b1, W2, b2], "mse", "sgd"),
    "detach": ([W1, b1, W2, b2], "ce", "nesterov"),
    "shapes": ([W1, b1, W2, b2], "ce", "adam"),
    "layernorm": ([W1, b1, gamma, beta, W2, b2], "ce", "adamw"),
    "arith": ([W1, b1, W2, b2], "mse", "momentum"),
    "softmaxdim": ([W1, b1, W2, b2], "ce", "adam"),
    "vector": ([W1, b1, W2, b2], "vec", "nesterov"),
}
params, target_kind, optimizer_kind = CASES[case]

batches = []
for step in range(STEPS):
    if target_kind == "vec":
        batches.append((torch.tensor([round(_uniform(-1.5, 1.5), 3) for _ in range(FEATURES)]),
                        torch.tensor([round(_uniform(-1.0, 1.0), 3) for _ in range(CLASSES)])))
        continue
    inputs = torch.tensor([[round(_uniform(-1.5, 1.5), 3) for _ in range(FEATURES)] for _ in range(BATCH)])
    labels = [(i * 2 + step) % CLASSES for i in range(BATCH)]
    if target_kind == "ce":
        target = torch.tensor(labels)
    elif target_kind == "mse":
        target = torch.tensor([[1.0 if c == label else 0.0 for c in range(CLASSES)] for label in labels])
    else:
        target = torch.tensor([[round(_uniform(-1.0, 1.0), 3) for _ in range(FEATURES)] for _ in range(BATCH)])
    batches.append((inputs, target))

if optimizer_kind == "sgd":
    optimizer = torch.optim.SGD(params, lr=0.1)
elif optimizer_kind == "momentum":
    optimizer = torch.optim.SGD(params, lr=0.05, momentum=0.9, weight_decay=0.01)
elif optimizer_kind == "nesterov":
    optimizer = torch.optim.SGD(params, lr=0.05, momentum=0.9, nesterov=True)
elif optimizer_kind == "adam":
    optimizer = torch.optim.Adam(params, lr=0.01)
else:
    optimizer = torch.optim.AdamW(params, lr=0.01, weight_decay=0.1)


def forward(x):
    if case == "l2reg":
        return F.linear(torch.tanh(F.linear(x, W1, b1)), W2, b2)
    if case == "tied":
        # A tied autoencoder: encode with W1.T (an eager transpose), decode with W1.
        return torch.tanh(x @ W1.T + b1) @ W1
    if case == "eagerops":
        h = torch.tanh(x @ W1.transpose(0, 1) + b1)
        mixed = W2 * 0.5 - b2.view(-1, 1) / 4.0
        out = h @ mixed.T + W2.sum(1) * 0.1 + W1.mean()
        return out * torch.softmax(W2, dim=1).sum(1) + W2.abs().mean(1) + (W1.square() + 1.0).sqrt().mean(0).sum()
    if case == "addparam":
        h = torch.add(torch.tanh(F.linear(x, W1)), b1)
        return torch.sub(F.linear(h + b1, W2), b2) + b2 * 2.0
    if case == "detach":
        h = torch.tanh(F.linear(x, W1, b1))
        centred = h - h.detach().mean(0, keepdim=True)
        return F.linear(centred, W2, b2) + (h.detach() * h).mean() * 0.1
    if case == "shapes":
        h = F.linear(x, W1, b1)
        h3 = h.view(BATCH, 2, HIDDEN // 2)
        moved = h3.permute(2, 0, 1).transpose(0, 2)
        flat = moved.reshape(2, -1).transpose(0, 1).flatten().unsqueeze(0).squeeze(0).view(BATCH, HIDDEN)
        e = torch.tanh(flat)[:, None][:, 0][...]
        return F.linear(e, W2, b2) + h3.sum((1, 2)).unsqueeze(1) * 0.1 + h3.mean((0, 2)).sum() * 0.05
    if case == "layernorm":
        h = F.linear(x, W1, b1)
        mean = h.mean(-1, keepdim=True)
        var = (h - mean).square().mean(-1, keepdim=True)
        normed = (h - mean) / (var + 1e-5).sqrt() * gamma + beta
        return F.linear(torch.relu(normed), W2, b2)
    if case == "arith":
        h = F.linear(x, W1, b1)
        soft = h / (h.abs() + 1.0)
        r = (h.square() + 1.0).rsqrt()
        q = (h.square() + 0.5) ** 0.5
        out = F.linear(soft + r * 0.5 + q.pow(2) * 0.1 - 1.0 / (q + 1.0), W2, b2)
        return out / (out.abs().sum(1, keepdim=True) + 1.0).sqrt()
    if case == "softmaxdim":
        h = F.linear(x, W1, b1)
        mixed = F.softmax(h, dim=0) * 3.0 + F.log_softmax(h, dim=0) * 0.1
        mixed = mixed + torch.log_softmax(h.view(BATCH, 2, HIDDEN // 2), dim=1).view(BATCH, HIDDEN) * 0.1
        return F.linear(mixed, W2, b2)
    # vector: a 1-D input to F.linear and a matrix-vector product
    h = torch.tanh(F.linear(x, W1, b1))
    return W2 @ h + b2 + (h * h).sum() * 0.01


def train_step(x, y):
    optimizer.zero_grad()
    out = forward(x)
    if target_kind == "ce":
        loss = F.cross_entropy(out, y)
    else:
        loss = ((out - y) ** 2).mean()
    if case == "l2reg":
        loss = loss + 0.05 * sum((p ** 2).sum() for p in params)
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

