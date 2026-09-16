"""Training steps that torch.compile(training=True) captures through the graph
protocol's second version: activations, softmax losses, the fused
cross-entropy, axis reductions, and SGD with momentum, Nesterov or Adam.

`case` is defined before this file. It runs unchanged under PyTorch, which
generated torch_training_v2_expected.json (see python_torch.rs)."""
import torch
from torch import nn
import torch.nn.functional as F

_seed = [12345]


def _uniform(lo, hi):
    # A fixed LCG: identical weights and data under every Torch.
    _seed[0] = (_seed[0] * 1103515245 + 12345) % 2147483648
    return lo + (hi - lo) * _seed[0] / 2147483648


def _fill(tensor, scale):
    with torch.no_grad():
        flat = [round(_uniform(-scale, scale), 4) for _ in range(tensor.numel())]
        tensor.copy_(torch.tensor(flat).reshape(tensor.shape))


FEATURES, HIDDEN, CLASSES, BATCH = 3, 5, 3, 6
activation, loss_kind, optimizer_kind = case.split("_")
model = nn.Sequential(
    nn.Linear(FEATURES, HIDDEN),
    {"relu": nn.ReLU(), "gelu": nn.GELU(), "gelutanh": nn.GELU(approximate="tanh"),
     "sigmoid": nn.Sigmoid(), "tanh": nn.Tanh()}[activation],
    nn.Linear(HIDDEN, CLASSES),
)
for parameter in model.parameters():
    _fill(parameter, 0.6)
inputs = torch.tensor([[round(_uniform(-1.5, 1.5), 3) for _ in range(FEATURES)] for _ in range(BATCH)])
classes = torch.tensor([i % CLASSES for i in range(BATCH)])
onehot = torch.tensor([[1.0 if c == i % CLASSES else 0.0 for c in range(CLASSES)] for i in range(BATCH)])
soft = onehot * 0.7 + 0.1

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


def loss_of(out, y):
    if loss_kind == "ce":
        # Integer class targets: the fused cross-entropy.
        return F.cross_entropy(out, y)
    if loss_kind == "softce":
        # Probability targets: log_softmax, a row sum and a mean.
        return F.cross_entropy(out, soft)
    if loss_kind == "softmaxmse":
        return ((F.softmax(out, dim=-1) - onehot) ** 2).sum(1, keepdim=True).mean()
    if loss_kind == "logsoftmaxnll":
        return -(F.log_softmax(out, dim=-1) * onehot).sum(-1).mean()
    if loss_kind == "centeredmse":
        # Broadcasting against a kept axis, and a reduction over the batch axis.
        centered = out - out.mean(1, keepdim=True)
        return ((centered - onehot) ** 2).sum(0).mean()
    return F.mse_loss(out, onehot)


def train_step(x, y):
    optimizer.zero_grad()
    loss = loss_of(model(x), y)
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
