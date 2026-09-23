# zipp-bench: zipp-only
"""Eager torch autograd over many tiny tensors: each iteration composes a
dozen elementwise ops and small reductions on 4-element tensors, runs
backward and applies the gradient by hand, so per-op bookkeeping rather than
kernel arithmetic dominates. Runs on Zipp's bundled torch subset; the checksum
is validated against the baseline Zipp. Inputs come from arange formulas and
the printed values are rounded."""
import sys
import time

import torch

ITERATIONS = 60


def bench(iterations):
    w = (torch.arange(4, dtype=torch.float32) / 8.0 - 0.2).requires_grad_()
    b = torch.zeros(4, requires_grad=True)
    x = torch.arange(4, dtype=torch.float32) / 4.0 + 0.5
    total = 0.0
    for i in range(iterations):
        h = torch.tanh(x * w + b)
        s = torch.sigmoid(h * 2.0 - 0.5)
        e = torch.exp(-h * h)
        z = (s * e + h / 3.0).relu()
        loss = (z * z).sum() + (w * w).mean() * 0.1 - z.max() * 0.5
        loss.backward()
        with torch.no_grad():
            w -= 0.05 * w.grad
            b -= 0.05 * b.grad
            w.grad.zero_()
            b.grad.zero_()
        total += loss.item()
    return total, w.detach().sum().item(), b.detach().sum().item()


t0 = time.perf_counter()
result = bench(ITERATIONS)
elapsed = time.perf_counter() - t0
print("torch_small_ops", ITERATIONS, "%.4f" % result[0], "%.4f" % result[1], "%.4f" % result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
