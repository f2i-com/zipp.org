# Dropout drawn on the device, CPU random draws fed to a prepared session,
# and failures, as ordinary synchronous code. Every line is the same on the
# GPU and on the CPU evaluator but for the backend's name: dropout masks are
# the protocol's integer-exact `uniform` draws, fed draws come from torch's
# CPU generator, and errors are raised where the CPU evaluator raises them.
import torch
from torch import nn
import torch.nn.functional as F


def bits(t):
    flat = (t != 0).flatten().tolist()
    return sum((i + 1) * int(v) for i, v in enumerate(flat))


# 1. Per-call dropout: kept values are float32(1 / float32(1 - p)) and the
# gradient carries the forward's own mask.
torch.manual_seed(1)
w = nn.Parameter(torch.ones(32))
opt = torch.optim.SGD([w], lr=0.0)


def dropout_step(x):
    opt.zero_grad()
    out = F.dropout(x * w, 0.5)
    out.sum().backward()
    opt.step()
    return out


step = torch.compile(dropout_step, training=True)
for i in range(2):
    got = []
    pending = step(torch.ones(64, 32))
    pending.submit(got.append)
    out = got[0]
    print("dropout call", i, "values", sorted(set(out.flatten().tolist())), "mask", bits(out),
          "grad matches mask", torch.equal(w.grad, (out != 0).float().sum(0) * 2.0), "backend", pending.backend)

# 2. A prepared session with dropout: a fresh device mask every step.
torch.manual_seed(7)
model = nn.Sequential(nn.Linear(8, 16), nn.ReLU(), nn.Dropout(0.3), nn.Linear(16, 2))
optimizer = torch.optim.Adam(model.parameters(), lr=0.01)


def train(x, y):
    optimizer.zero_grad()
    dropped = model[2](model[1](model[0](x)))
    loss = F.cross_entropy(model[3](dropped), y)
    loss.backward()
    optimizer.step()
    return dropped * 1.0


xs = [torch.randn(4, 8) for _ in range(4)]
ys = [torch.randint(0, 2, (4,)) for _ in range(4)]
session = torch.compile(train, training=True).prepare(xs[0], ys[0])
results = []
session.steps(results.extend, list(zip(xs, ys)))
print("prepared dropout masks", [bits(r) for r in results], "distinct", len(set(bits(r) for r in results)) == 4, "backend", session.backend)
session.sync(lambda s: None)
session.dispose()
try:
    session.step(lambda r: print("step after dispose ACCEPTED"), xs[0], ys[0])
except Exception as error:
    print("step after dispose", type(error).__name__)


# 3. CPU draws inside a prepared step are drawn from torch's generator every
# step and fed: the session sees exactly what eager steps draw.
def build():
    torch.manual_seed(11)
    enc = nn.Linear(4, 6)
    opt = torch.optim.Adam(enc.parameters(), lr=0.01)

    def step(x, y):
        opt.zero_grad()
        t = torch.randint(0, 3, (x.shape[0],))
        h = enc(x)
        loss = F.cross_entropy(h[:, 1:4], t) + F.cross_entropy(h[:, :3], y)
        loss.backward()
        opt.step()
        return torch.arange(3.0).index_select(0, t) + h.sum() * 0.0
    return step


torch.manual_seed(5)
xs = [torch.randn(5, 4) for _ in range(5)]
ys = [torch.randint(0, 3, (5,)) for _ in range(5)]
fn = build()
torch.manual_seed(100)
eager = [fn(xs[i], ys[i]) for i in range(5)]
fn = build()
torch.manual_seed(100)
session = torch.compile(fn, training=True).prepare(xs[0], ys[0])
got = []
session.steps(got.extend, [(xs[i], ys[i]) for i in range(5)])
print("fed draws == eager", all(torch.equal(a, b) for a, b in zip(got, eager)), "backend", session.backend)
session.dispose()

# 4. Failures, raised where the CPU evaluator raises them. A step whose
# result is not finite poisons its session after it has run on the device
# (only dispose() remains); an index outside its bound is refused before
# any device work.
torch.manual_seed(3)
lin = nn.Linear(3, 2)
session = torch.compile(lambda x: torch.log(lin(x) * 0.0 + x.sum(1, keepdim=True))).prepare(torch.ones(2, 3))
session.step(lambda r: print("finite step", r.shape), torch.ones(2, 3))
for attempt in ("non-finite step", "step after failure"):
    try:
        session.step(lambda r: print(attempt, "ACCEPTED"), -torch.ones(2, 3))
    except Exception as error:
        print(attempt, type(error).__name__, getattr(error, "code", None))
session.dispose()
# The same failure on a session's very first step: natively the session
# moves to the CPU evaluator, which raises exactly this.
session = torch.compile(lambda x: torch.log(lin(x) * 0.0 + x.sum(1, keepdim=True))).prepare(torch.ones(2, 3))
for attempt in ("non-finite first step", "then"):
    try:
        session.step(lambda r: print(attempt, "ACCEPTED"), -torch.ones(2, 3))
    except Exception as error:
        print(attempt, type(error).__name__, getattr(error, "code", None))
session.dispose()
emb = nn.Embedding(5, 2)
s2 = torch.compile(emb).prepare(torch.tensor([0, 1]))
try:
    s2.step(lambda r: print("bad index ACCEPTED"), torch.tensor([0, 9]))
except Exception as error:
    print("index out of bounds", type(error).__name__)
s2.step(lambda r: print("embedding", r.shape == (2, 2), torch.equal(r, emb(torch.tensor([3, 4])).detach())), torch.tensor([3, 4]))
s2.dispose()
print("done")
