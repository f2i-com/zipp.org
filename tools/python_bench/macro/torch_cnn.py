# zipp-bench: zipp-only
"""Eager torch CNN training: Conv2d + ReLU + MaxPool2d twice, then Linear,
cross-entropy backward and an SGD step on a small image batch, repeated.
Runs on Zipp's bundled torch subset; the host CPython has no torch, so the
checksum is validated against the baseline Zipp. Parameters and inputs come
from arange formulas and the printed values are rounded."""
import sys
import time

import torch
import torch.nn.functional as F
from torch import nn

STEPS = 6
BATCH = 8
SIDE = 12
CLASSES = 10


def patterned(shape, count, mul, scale):
    values = (torch.arange(count, dtype=torch.float32) * mul) % 13 - 6.0
    return (values / scale).reshape(shape)


def build():
    model = nn.Sequential(
        nn.Conv2d(1, 4, 3, padding=1), nn.ReLU(), nn.MaxPool2d(2),
        nn.Conv2d(4, 8, 3, padding=1), nn.ReLU(), nn.MaxPool2d(2),
        nn.Flatten(), nn.Linear(8 * (SIDE // 4) * (SIDE // 4), CLASSES))
    with torch.no_grad():
        for index, param in enumerate(model.parameters()):
            param.copy_(patterned(tuple(param.shape), param.numel(), 7 + 2 * index, 40.0))
    x = patterned((BATCH, 1, SIDE, SIDE), BATCH * SIDE * SIDE, 5, 6.0)
    y = torch.arange(BATCH) % CLASSES
    return model, x, y


def bench(steps):
    model, x, y = build()
    opt = torch.optim.SGD(model.parameters(), lr=0.05, momentum=0.9)
    first = None
    loss = None
    for step in range(steps):
        opt.zero_grad()
        loss = F.cross_entropy(model(x), y)
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
print("torch_cnn", STEPS, BATCH, "%.4f" % result[0], "%.4f" % result[1], "%.3f" % result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
