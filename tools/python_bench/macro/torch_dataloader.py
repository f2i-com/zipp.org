# zipp-bench: zipp-only
"""A torch.utils.data DataLoader over a TensorDataset with shuffling and
batching, iterated for a few epochs with a light reduction per batch. Runs on
Zipp's bundled torch subset; the checksum is validated against the baseline
Zipp. Data comes from arange formulas, the shuffle order from a seeded
generator, and the printed values are rounded."""
import sys
import time

import torch
from torch.utils.data import DataLoader, TensorDataset

ROWS = 256
FEATURES = 8
BATCH = 16
EPOCHS = 4


def bench(epochs):
    x = ((torch.arange(ROWS * FEATURES, dtype=torch.float32) * 7) % 19 / 19.0).reshape(ROWS, FEATURES)
    y = torch.arange(ROWS) % 5
    loader = DataLoader(TensorDataset(x, y), batch_size=BATCH, shuffle=True, generator=torch.Generator().manual_seed(0))
    total = 0.0
    weighted = 0.0
    batches = 0
    for epoch in range(epochs):
        for index, (xb, yb) in enumerate(loader):
            total += xb.sum().item()
            weighted += (xb[:, 0] * yb).sum().item() * (index + 1)
            batches += 1
    return batches, total, weighted


t0 = time.perf_counter()
result = bench(EPOCHS)
elapsed = time.perf_counter() - t0
print("torch_dataloader", result[0], "%.3f" % result[1], "%.3f" % result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
