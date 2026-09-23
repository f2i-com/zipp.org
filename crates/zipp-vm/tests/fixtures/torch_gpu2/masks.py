"""Training steps that exercise protocol version 3 under torch.compile:
comparisons used as masks in arithmetic, torch.where / Tensor.where /
masked_fill, clamp with scalar and tensor bounds, hardtanh / relu6 /
leaky_relu (functional and nn modules), maximum / minimum with ties, logical
mask algebra, and dropout in eval mode. The "edges" case starts with inputs
exactly on every boundary (clamp bounds, hardtanh bounds, relu6's 0 and 6,
leaky_relu's and relu's 0, a maximum tie), where PyTorch's gradients differ
by operation: clamp passes the gradient at a bound, hardtanh and relu6 do
not, a tie splits it.

`case` is defined before this file. It runs unchanged under PyTorch, which
generated masks_expected.json (gen.py): one `state()` vector per eager step
over `batches` (see python_torch_gpu2.rs)."""
import torch
from torch import nn
import torch.nn.functional as F

_seed = [4242]


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
lo = nn.Parameter(torch.tensor([-0.3, -0.5, 0.1, -0.2, 0.4, -1.0]))
hi = nn.Parameter(torch.tensor([0.3, 0.2, 0.5, -0.4, 0.6, 0.0]))
scale, shift = nn.Parameter(torch.ones(FEATURES)), nn.Parameter(torch.zeros(FEATURES))
We = _parameter(CLASSES, FEATURES)
# Constants built outside the step: an eager bool mask and a float bound.
COLUMNS = torch.tensor([True, False, True, True, False, True])
BOUND = torch.tensor([0.1, -0.2, 0.3, 0.0, 0.25, -0.1])
dropout = nn.Dropout(0.5)
dropout.eval()

CASES = {
    # name: (parameters, optimizer)
    "masks": ([W1, b1, W2, b2], "sgd"),
    "where": ([W1, b1, W2, b2], "adam"),
    "clamp": ([W1, b1, W2, b2], "sgd"),
    "activations": ([W1, b1, W2, b2], "adam"),
    "maxmin": ([W1, b1, W2, b2], "sgd"),
    "edges": ([scale, shift, We, b2], "sgd"),
    "tensorclamp": ([W1, b1, lo, hi, W2, b2], "sgd"),
    "logical": ([W1, b1, W2, b2], "momentum"),
    "evaldropout": ([W1, b1, W2, b2], "sgd"),
}
params, optimizer_kind = CASES[case]

batches = []
for step in range(STEPS):
    if case == "edges" and step == 0:
        # Every input on a boundary: scale = 1 and shift = 0 make u = x exactly.
        inputs = torch.tensor([[-1.0, -0.5, 0.0, 0.5], [0.5, 1.0, -0.5, 0.0], [1.0, 0.0, 0.5, -1.0],
                               [0.0, 0.5, 1.0, -0.5], [-0.5, -1.0, 0.0, 1.0]])
    else:
        inputs = torch.tensor([[round(_uniform(-1.5, 1.5), 3) for _ in range(FEATURES)] for _ in range(BATCH)])
    batches.append((inputs, torch.tensor([(i * 2 + step) % CLASSES for i in range(BATCH)])))

if optimizer_kind == "sgd":
    optimizer = torch.optim.SGD(params, lr=0.1)
elif optimizer_kind == "momentum":
    optimizer = torch.optim.SGD(params, lr=0.05, momentum=0.9)
else:
    optimizer = torch.optim.Adam(params, lr=0.01)


def forward(x):
    if case == "edges":
        u = x * scale + shift
        y = (u.clamp(-0.5, 0.5) + F.hardtanh(u, -0.5, 0.5) + F.relu6(u * 6.0) + F.leaky_relu(u, 0.1)
             + torch.maximum(u, torch.zeros(FEATURES)) + torch.where(u >= 0.5, u, -u) + F.relu(u)
             + torch.clamp(u, min=0.0) * 0.5 + nn.Hardtanh(-1.0, 1.0)(u) * 0.25)
        return F.linear(y, We, b2)
    h = F.linear(x, W1, b1)
    if case == "masks":
        a = h * (h > 0) + 0.25 * h * (h <= 0) + (h >= 0.3).float() * 0.1 + (0.1 < h) * h
    elif case == "where":
        a = (torch.where(h > 0.2, h, h * 0.5) + h.where(h < 0.8, torch.tanh(h)) - h.masked_fill(h < -0.4, 0.25)
             + torch.where(COLUMNS, h, 0.0) + torch.where(h > BOUND, torch.zeros_like(h), h) + h.masked_fill(~COLUMNS, -1.5))
    elif case == "clamp":
        a = (h.clamp(-0.5, 0.5) + torch.clamp(h, min=-0.25) * 0.5 + h.clamp_max(0.3) + torch.clip(h, -1, 1)
             + h.clamp_min(0.1) + torch.clamp(h, BOUND, None) + h.clip(max=BOUND))
    elif case == "activations":
        a = (F.hardtanh(h, -0.5, 0.5) + F.relu6(h * 4.0) + F.leaky_relu(h, 0.2) + nn.LeakyReLU(0.05)(h)
             + nn.ReLU6()(h * 3.0) + nn.Hardtanh()(h) + F.threshold(h, 0.1, -0.2) + F.elu(h) * 0.5)
    elif case == "maxmin":
        a = (torch.maximum(h, h * 0.5) + torch.minimum(h, BOUND) + torch.max(h, h) + h.maximum(torch.zeros(HIDDEN))
             + torch.min(h.clamp(-0.3, 0.3), torch.full((HIDDEN,), 0.3)) + h.minimum(h.detach()))
    elif case == "tensorclamp":
        a = torch.clamp(h, lo, hi) + torch.clamp(h, min=lo) * 0.5 + h.clamp(max=hi) * 0.25
    elif case == "logical":
        m1 = (h > -0.3) & (h < 0.3)
        a = (h * m1 + h * ~(h > 0) * 0.5 + h * ((h > 0.5) | (h < -0.5)) + torch.logical_and(h > 0, h < 1).float() * h
             + h * torch.logical_not(h > 0.1) + (h != h * 2).float() + h * (h == h) * 0.1 + h * ((h > 0) ^ COLUMNS))
    else:
        a = dropout(torch.tanh(h)) + F.dropout(h, 0.3, training=False)
    return F.linear(a, W2, b2)


def train_step(x, y):
    optimizer.zero_grad()
    loss = F.cross_entropy(forward(x), y)
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
