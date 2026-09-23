# zipp-bench: zipp-only
"""Eager torch 256x256 float32 matmul plus a batched bmm of [8, 64, 64]
matrices. Runs on Zipp's bundled torch subset; the checksum is validated
against the baseline Zipp. Inputs come from arange formulas and the printed
values are rounded."""
import sys
import time

import torch

SIZE = 256
BATCH = 8
SMALL = 64


def patterned(count, mul, add):
    return ((torch.arange(count, dtype=torch.float32) * mul + add) % 17) / 17.0


def bench():
    a = patterned(SIZE * SIZE, 3, 1).reshape(SIZE, SIZE)
    b = patterned(SIZE * SIZE, 5, 2).reshape(SIZE, SIZE)
    c = a @ b
    x = patterned(BATCH * SMALL * SMALL, 7, 3).reshape(BATCH, SMALL, SMALL)
    y = patterned(BATCH * SMALL * SMALL, 11, 4).reshape(BATCH, SMALL, SMALL)
    d = torch.bmm(x, y)
    return c.sum().item(), c[7, 11].item(), d.sum().item(), d[3, 5, 9].item()


t0 = time.perf_counter()
result = bench()
elapsed = time.perf_counter() - t0
print("torch_matmul_big", SIZE, BATCH, SMALL, "%.4e" % result[0], "%.4f" % result[1], "%.4e" % result[2], "%.4f" % result[3])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
