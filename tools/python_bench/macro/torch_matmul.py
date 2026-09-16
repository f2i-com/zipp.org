# zipp-bench: zipp-only
"""Eager torch 128x128 float32 matrix multiply in a loop. Runs on Zipp's bundled
torch subset; the host CPython has no torch, so the checksum is validated
against the baseline Zipp. Inputs come from arange formulas and the printed sum
is rounded to five significant digits so a reordered float32 accumulation in
the kernel does not fail validation."""
import sys
import time

import torch

SIZE = 128
ITERATIONS = 3


def patterned(mul, add):
    count = SIZE * SIZE
    values = (torch.arange(count, dtype=torch.float32) * mul + add) % 17
    return (values / 17.0).reshape(SIZE, SIZE)


def bench(iterations):
    a = patterned(3, 1)
    b = patterned(5, 2)
    total = 0.0
    c = None
    for i in range(iterations):
        c = a @ b
        total += c.sum().item()
    return total, c[7, 11].item()


t0 = time.perf_counter()
result = bench(ITERATIONS)
elapsed = time.perf_counter() - t0
print("torch_matmul", SIZE, ITERATIONS, "%.4e" % result[0], "%.4f" % result[1])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
