# zipp-bench: zipp-only
"""Eager torch recurrent training: an LSTM layer feeding a GRU layer and a
Linear head over a short sequence batch, MSE loss, backward through time and
an Adam step, repeated. Runs on Zipp's bundled torch subset; the checksum is
validated against the baseline Zipp. Parameters and inputs come from arange
formulas and the printed values are rounded."""
import sys
import time

import torch
import torch.nn.functional as F
from torch import nn

STEPS = 4
BATCH = 4
SEQ = 10
FEATURES = 6
HIDDEN = 12


def patterned(shape, count, mul, scale):
    values = (torch.arange(count, dtype=torch.float32) * mul) % 13 - 6.0
    return (values / scale).reshape(shape)


class Model(nn.Module):
    def __init__(self):
        super().__init__()
        self.lstm = nn.LSTM(FEATURES, HIDDEN, batch_first=True)
        self.gru = nn.GRU(HIDDEN, HIDDEN, batch_first=True)
        self.head = nn.Linear(HIDDEN, 1)

    def forward(self, x):
        h, _ = self.lstm(x)
        h, _ = self.gru(h)
        return self.head(h[:, -1, :])


def build():
    model = Model()
    with torch.no_grad():
        for index, param in enumerate(model.parameters()):
            param.copy_(patterned(tuple(param.shape), param.numel(), 7 + 2 * index, 30.0))
    x = patterned((BATCH, SEQ, FEATURES), BATCH * SEQ * FEATURES, 5, 6.0)
    y = patterned((BATCH, 1), BATCH, 3, 4.0)
    return model, x, y


def bench(steps):
    model, x, y = build()
    opt = torch.optim.Adam(model.parameters(), lr=0.01)
    first = None
    loss = None
    for step in range(steps):
        opt.zero_grad()
        loss = F.mse_loss(model(x), y)
        loss.backward()
        opt.step()
        if first is None:
            first = loss.item()
    weight_sum = 0.0
    for param in model.parameters():
        weight_sum += param.detach().abs().sum().item()
    return first, loss.item(), weight_sum


t0 = time.perf_counter()
result = bench(STEPS)
elapsed = time.perf_counter() - t0
print("torch_rnn", STEPS, BATCH, SEQ, "%.4f" % result[0], "%.4f" % result[1], "%.3f" % result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
