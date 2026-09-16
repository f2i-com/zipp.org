"""A training step run on several distinct batches: what `compiled.prepare()`
records once and then feeds batch by batch while the parameters and optimizer
state stay on the device. `case` is defined before this file; it runs
unchanged under PyTorch, which generated torch_prepared_expected.json (one
`state()` vector per eager step over `batches`, see python_torch.rs)."""
import torch
from torch import nn
import torch.nn.functional as F

_seed = [4242]


def _uniform(lo, hi):
    # A fixed LCG: identical weights and batches under every Torch.
    _seed[0] = (_seed[0] * 1103515245 + 12345) % 2147483648
    return lo + (hi - lo) * _seed[0] / 2147483648


def _fill(tensor, scale):
    with torch.no_grad():
        flat = [round(_uniform(-scale, scale), 4) for _ in range(tensor.numel())]
        tensor.copy_(torch.tensor(flat).reshape(tensor.shape))


FEATURES, HIDDEN, CLASSES, BATCH, STEPS = 4, 6, 3, 8, 6
activation, loss_kind, optimizer_kind = case.split("_")
model = nn.Sequential(
    nn.Linear(FEATURES, HIDDEN),
    {"relu": nn.ReLU(), "gelu": nn.GELU(), "sigmoid": nn.Sigmoid(), "tanh": nn.Tanh()}[activation],
    nn.Linear(HIDDEN, CLASSES),
)
for parameter in model.parameters():
    _fill(parameter, 0.6)

# Every step sees its own batch: the inputs and, for the fused cross-entropy,
# integer class targets, or for MSE a float one-hot target.
batches = []
for step in range(STEPS):
    inputs = torch.tensor([[round(_uniform(-1.5, 1.5), 3) for _ in range(FEATURES)] for _ in range(BATCH)])
    labels = [(i + step) % CLASSES for i in range(BATCH)]
    if loss_kind == "ce":
        target = torch.tensor(labels)
    else:
        target = torch.tensor([[1.0 if c == label else 0.0 for c in range(CLASSES)] for label in labels])
    batches.append((inputs, target))

if optimizer_kind == "sgd":
    optimizer = torch.optim.SGD(model.parameters(), lr=0.1, weight_decay=0.01)
elif optimizer_kind == "momentum":
    optimizer = torch.optim.SGD(model.parameters(), lr=0.05, momentum=0.9, dampening=0.1, weight_decay=0.01)
elif optimizer_kind == "nesterov":
    optimizer = torch.optim.SGD(model.parameters(), lr=0.05, momentum=0.9, nesterov=True)
elif optimizer_kind == "adam":
    optimizer = torch.optim.Adam(model.parameters(), lr=0.01, weight_decay=0.01)
elif optimizer_kind == "adamw":
    optimizer = torch.optim.AdamW(model.parameters(), lr=0.01, weight_decay=0.1)
else:
    raise ValueError(optimizer_kind)


def train_step(x, y):
    optimizer.zero_grad()
    out = model(x)
    loss = F.cross_entropy(out, y) if loss_kind == "ce" else F.mse_loss(out, y)
    loss.backward()
    optimizer.step()
    return loss


def state(loss):
    values = [loss.item()]
    for p in model.parameters():
        values.extend(p.grad.flatten().tolist())
    for p in model.parameters():
        values.extend(p.detach().flatten().tolist())
    for entry in optimizer.state.values():
        for key in ("momentum_buffer", "exp_avg", "exp_avg_sq"):
            if key in entry and entry[key] is not None:
                values.extend(entry[key].flatten().tolist())
        if "step" in entry:
            values.append(float(entry["step"]))
    return values
