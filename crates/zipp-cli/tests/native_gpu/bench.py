# The Torch guide's benchmark: an MLP (default 784-256-10, batch 64, Adam,
# cross-entropy) trained by per-call torch.compile, by prepared.step and by
# prepared.steps (8 per run), warm medians. Ordinary synchronous code: it
# runs on the native GPU when there is one and on the CPU evaluator with
# ZIPP_GPU=0, with the same results.
#   zipp py bench.py [hidden,... [batch [calls [runs [ce-adam|mse-sgd]]]]]
# `mse-sgd` (MSE against one-hot targets, plain SGD) records a version-1
# graph, which the CPU evaluator runs on its tensor kernels; `ce-adam`
# records version 2, which it runs on zipp_gpu's pure-Python reference.
# `calls` 0 times one compiled call only (a CPU sample of a big model).
import sys
import time
import torch
from torch import nn
import torch.nn.functional as F

hidden = [int(h) for h in sys.argv[1].split(",")] if len(sys.argv) > 1 else [256]
BATCH = int(sys.argv[2]) if len(sys.argv) > 2 else 64
CALLS = int(sys.argv[3]) if len(sys.argv) > 3 else 15
RUNS = int(sys.argv[4]) if len(sys.argv) > 4 else 7
KIND = sys.argv[5] if len(sys.argv) > 5 else "ce-adam"
sizes = [784] + hidden + [10]
torch.manual_seed(0)
layers = []
for a, b in zip(sizes[:-1], sizes[1:]):
    layers += [nn.Linear(a, b), nn.ReLU()]
model = nn.Sequential(*layers[:-1])
if KIND == "ce-adam":
    optimizer = torch.optim.Adam(model.parameters(), lr=1e-3)
else:
    optimizer = torch.optim.SGD(model.parameters(), lr=0.05)
xs = [torch.rand(BATCH, 784) for _ in range(8)]
ys = [torch.randint(0, 10, (BATCH,)) for _ in range(8)]
if KIND != "ce-adam":
    ys = [F.one_hot(y, 10).float() for y in ys]


def train_step(x, y):
    optimizer.zero_grad()
    loss = F.cross_entropy(model(x), y) if KIND == "ce-adam" else F.mse_loss(model(x), y)
    loss.backward()
    optimizer.step()
    return loss


def median(times):
    times = sorted(times)
    return times[len(times) // 2]


compiled = torch.compile(train_step, training=True)
label = "model %s batch %d %s" % ("-".join(str(n) for n in sizes), BATCH, KIND)
losses = []
if CALLS == 0:
    start = time.perf_counter()
    pending = compiled(xs[0], ys[0])
    pending.submit(lambda loss: losses.append(loss.item()))
    print(label, "backend", pending.backend, "one compiled call %.1f ms" % ((time.perf_counter() - start) * 1000), "loss %.4f" % losses[0])
    sys.exit(0)

times = []
for i in range(CALLS + 2):
    start = time.perf_counter()
    pending = compiled(xs[i % 8], ys[i % 8])
    pending.submit(lambda loss: losses.append(loss.item()))
    times.append((time.perf_counter() - start) * 1000)
per_call = median(times[2:])
session = compiled.prepare(xs[0], ys[0])
one = []
for i in range(CALLS + 2):
    start = time.perf_counter()
    session.step(lambda loss: losses.append(loss.item()), xs[i % 8], ys[i % 8])
    one.append((time.perf_counter() - start) * 1000)
eight = []
for i in range(RUNS + 1):
    start = time.perf_counter()
    session.steps(lambda out: losses.extend(l.item() for l in out), list(zip(xs, ys)))
    eight.append((time.perf_counter() - start) * 1000 / 8)
session.sync(lambda s: None)
backend = session.backend
session.dispose()
print(label, "backend", backend)
print("%-18s %9.2f ms" % ("compiled", per_call))
print("%-18s %9.2f ms" % ("prepared.step", median(one[2:])))
print("%-18s %9.2f ms" % ("prepared.steps x8", median(eight[1:]) if RUNS else float("nan")))
print("loss first %.4f last %.4f" % (losses[0], losses[-1]))
