# Prepared sessions whose later steps the native GPU host replays
# (crates/zipp-gpu/src/replay.rs): an MLP with Adam and cross-entropy, one
# with dropout (masks drawn on the device per step) and SGD with momentum,
# and an embedding lookup fed its token indices each step. Single steps,
# runs of eight and a sync; every loss and every synced parameter value is
# printed, so a run with ZIPP_GPU_REPLAY=0 must print exactly the same.
import torch
from torch import nn
import torch.nn.functional as F


def batches(n, shape, classes, seed):
    torch.manual_seed(seed)
    return [(torch.rand(*shape), torch.randint(0, classes, (shape[0],))) for _ in range(n)]


def emb_batches(n, seed):
    torch.manual_seed(seed)
    return [(torch.randint(0, 50, (6, 4)), torch.randint(0, 5, (6,))) for _ in range(n)]


def run(name, model, optimizer, loss_of, data):
    def step(x, y):
        optimizer.zero_grad()
        loss = loss_of(x, y)
        loss.backward()
        optimizer.step()
        return loss

    losses = []
    prepared = torch.compile(step, training=True).prepare(*data[0])
    for i in range(4):
        prepared.step(lambda l: losses.append(l.item()), *data[i])
    prepared.steps(lambda out: losses.extend(l.item() for l in out), data[4:12])
    prepared.step(lambda l: losses.append(l.item()), *data[12])
    prepared.steps(lambda out: losses.extend(l.item() for l in out), data[13:16])
    prepared.sync(lambda s: None)
    prepared.dispose()
    print(name, "losses", " ".join(repr(l) for l in losses))
    for pname, p in model.named_parameters():
        values = p.detach().flatten().tolist()
        print(name, pname, len(values), hash(tuple(values)), repr(values[0]), repr(values[-1]))
    for pname, p in model.named_parameters():
        state = optimizer.state[p]
        for key in sorted(state):
            v = state[key]
            print(name, pname, key, hash(tuple(v.flatten().tolist())) if isinstance(v, torch.Tensor) else v)


torch.manual_seed(1)
mlp = nn.Sequential(nn.Linear(40, 32), nn.ReLU(), nn.Linear(32, 5))
run("adam", mlp, torch.optim.Adam(mlp.parameters(), lr=1e-2), lambda x, y: F.cross_entropy(mlp(x), y), batches(16, (12, 40), 5, 2))

torch.manual_seed(3)
drop = nn.Sequential(nn.Linear(20, 16), nn.ReLU(), nn.Dropout(0.3), nn.Linear(16, 4))
run("dropout", drop, torch.optim.SGD(drop.parameters(), lr=0.05, momentum=0.9), lambda x, y: F.cross_entropy(drop(x), y), batches(16, (10, 20), 4, 4))

torch.manual_seed(5)
table, head = nn.Embedding(50, 8), nn.Linear(8, 5)
params = list(table.parameters()) + list(head.parameters())
emb = nn.ModuleDict({"table": table, "head": head})
run("embedding", emb, torch.optim.Adam(params, lr=1e-2), lambda t, y: F.cross_entropy(head(table(t).mean(1)), y), emb_batches(16, 6))
