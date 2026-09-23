# torch.utils.data: prints lines that must equal what CPU PyTorch 2.11 prints
# (data_expected.txt, from `python data_cases.py`). Orders that depend on the
# random stream are checked for their properties (a permutation, reproducible
# under manual_seed/generator) rather than printed.
import math
from collections import namedtuple, OrderedDict

import torch
import torch.utils.data as tud
from torch.utils.data import (DataLoader, Dataset, IterableDataset, TensorDataset, ConcatDataset, Subset, random_split,
                              SequentialSampler, RandomSampler, SubsetRandomSampler, WeightedRandomSampler, BatchSampler,
                              default_collate)

x = torch.arange(20, dtype=torch.float32).reshape(10, 2)
y = torch.arange(10)
ds = TensorDataset(x, y)
print("len", len(ds), [t.tolist() for t in ds[3]])

loader = DataLoader(ds, batch_size=4)
print("batches", len(loader), [(xb.shape[0], yb.tolist()) for xb, yb in loader])
print("drop_last", len(DataLoader(ds, batch_size=4, drop_last=True)), [yb.tolist() for _, yb in DataLoader(ds, batch_size=4, drop_last=True)])
xb, yb = next(iter(DataLoader(ds, batch_size=3)))
print("first", tuple(xb.shape), xb.dtype, yb.dtype, xb.tolist())
print("no batching", [type(item).__name__ for item in DataLoader(ds, batch_size=None)][:2], len(DataLoader(ds, batch_size=None)))
print("collate_fn", list(DataLoader(ds, batch_size=5, collate_fn=lambda items: len(items))))

# default_collate across Python containers and scalar types.
Point = namedtuple("Point", "a b")
samples = [{"f": 1.5, "i": 2, "s": "u", "t": torch.tensor([1.0, 2.0]), "p": Point(1, 2.0), "l": [1, 2], "tup": (3, "x")},
           {"f": 2.5, "i": 3, "s": "v", "t": torch.tensor([3.0, 4.0]), "p": Point(3, 4.0), "l": [3, 4], "tup": (5, "y")}]
batch = default_collate(samples)
print("collate keys", list(batch.keys()))
print("collate f", batch["f"].dtype, batch["f"].tolist(), "i", batch["i"].dtype, batch["i"].tolist(), "s", batch["s"])
print("collate t", batch["t"].shape, "p", type(batch["p"]).__name__, batch["p"].a.tolist(), batch["p"].b.dtype)
print("collate l", type(batch["l"]).__name__, [v.tolist() for v in batch["l"]], "tup", type(batch["tup"]).__name__, batch["tup"][0].tolist(), batch["tup"][1])
print("collate bool", default_collate([True, False]).dtype, "od", type(default_collate([OrderedDict(a=1), OrderedDict(a=2)])).__name__)
try:
    default_collate([[1, 2], [3]])
except RuntimeError as e:
    print("ragged", e)
try:
    default_collate([object(), object()])
except TypeError as e:
    print("unsupported", str(e).split(";")[0])
print("tud.default_collate", tud.default_collate is default_collate)


class Squares(Dataset):
    def __init__(self, n):
        self.n = n

    def __len__(self):
        return self.n

    def __getitem__(self, i):
        return {"i": i, "sq": float(i * i)}


cat = ConcatDataset([Squares(3), Squares(4)])
print("concat", len(cat), [cat[i]["i"] for i in range(len(cat))], cat[-1]["i"], cat.cumulative_sizes)
print("add", len(Squares(2) + Squares(5)))
sub = Subset(Squares(10), [9, 2, 5])
print("subset", len(sub), [sub[i]["i"] for i in range(3)])
print("dict batches", [(b["i"].tolist(), b["sq"].dtype) for b in DataLoader(Squares(5), batch_size=2)])

torch.manual_seed(0)
parts = random_split(Squares(10), [0.5, 0.3, 0.2])
print("random_split fractions", [len(p) for p in parts], sorted(sum([list(p.indices) for p in parts], [])) == list(range(10)))
parts = random_split(Squares(7), [0.34, 0.33, 0.33])
print("random_split remainder", [len(p) for p in parts])
parts = random_split(Squares(6), [4, 2], generator=torch.Generator().manual_seed(1))
again = random_split(Squares(6), [4, 2], generator=torch.Generator().manual_seed(1))
print("random_split generator", [len(p) for p in parts], [list(p.indices) for p in parts] == [list(p.indices) for p in again])
try:
    random_split(Squares(6), [4, 3])
except ValueError as e:
    print("random_split bad", e)

# Samplers.
print("sequential", list(SequentialSampler(range(4))))
print("batch sampler", list(BatchSampler(SequentialSampler(range(7)), 3, False)), list(BatchSampler(range(7), 3, True)),
      len(BatchSampler(range(7), 3, False)), len(BatchSampler(range(7), 3, True)))
perm = list(RandomSampler(range(9)))
print("random sampler", sorted(perm) == list(range(9)), len(RandomSampler(range(9), num_samples=4)), len(list(RandomSampler(range(9), num_samples=13))))
rep = list(RandomSampler(range(5), replacement=True, num_samples=40))
print("replacement", len(rep), all(0 <= i < 5 for i in rep))
sub_idx = list(SubsetRandomSampler([10, 20, 30]))
print("subset random", sorted(sub_idx))
w = list(WeightedRandomSampler([0.0, 1.0, 0.0, 1.0], 12))
print("weighted", len(w), sorted(set(w)))
w2 = list(WeightedRandomSampler([0.1, 0.2, 0.3, 0.4], 4, replacement=False))
print("weighted no replacement", sorted(w2))


def order(loader):
    return [yb.tolist() for _, yb in loader]


torch.manual_seed(3)
a = order(DataLoader(ds, batch_size=4, shuffle=True))
torch.manual_seed(3)
b = order(DataLoader(ds, batch_size=4, shuffle=True))
print("shuffle reproducible", a == b, sorted(sum(a, [])) == list(range(10)), [len(v) for v in a])
g_loader = DataLoader(ds, batch_size=10, shuffle=True, generator=torch.Generator().manual_seed(5))
first, second = order(g_loader), order(g_loader)
print("generator epochs differ", first != second, order(DataLoader(ds, batch_size=10, shuffle=True, generator=torch.Generator().manual_seed(5))) == first)
torch.manual_seed(11)
before = torch.rand(3).tolist()
torch.manual_seed(11)
list(DataLoader(ds, batch_size=5))
after = torch.rand(3).tolist()
print("iteration draws a seed", before != after)
# Zipp runs workers in-process; PyTorch here would spawn processes (which a
# script without a __main__ guard cannot do on Windows) for the same batches.
workers = dict(num_workers=2, persistent_workers=True, prefetch_factor=2) if torch.__version__.endswith("+zipp") else {}
print("workers ignored", order(DataLoader(ds, batch_size=5, pin_memory=True, **workers)))
sampler_loader = DataLoader(ds, batch_size=3, sampler=[9, 0, 8, 1])
print("sampler", order(sampler_loader), len(sampler_loader))
bs_loader = DataLoader(ds, batch_sampler=[[0, 1], [5], [2, 3, 4]])
print("batch_sampler", order(bs_loader), len(bs_loader))


class Stream(IterableDataset):
    def __init__(self, n):
        self.n = n

    def __iter__(self):
        return iter(range(self.n))

    def __len__(self):
        return self.n


print("iterable", [b.tolist() for b in DataLoader(Stream(7), batch_size=3)], [b.tolist() for b in DataLoader(Stream(7), batch_size=3, drop_last=True)],
      len(DataLoader(Stream(7), batch_size=3)), len(DataLoader(Stream(7), batch_size=3, drop_last=True)))
print("iterable unbatched", list(DataLoader(Stream(3), batch_size=None)))
for kwargs in [dict(sampler=[0], shuffle=True), dict(batch_sampler=[[0]], batch_size=2), dict(batch_sampler=[[0]], drop_last=True),
               dict(batch_size=None, drop_last=True), dict(num_workers=-1), dict(prefetch_factor=2), dict(persistent_workers=True)]:
    try:
        DataLoader(ds, **kwargs)
        print("accepted", sorted(kwargs))
    except ValueError as e:
        print("ValueError", str(e)[:48])
try:
    DataLoader(Stream(3), shuffle=True)
except ValueError as e:
    print("ValueError", str(e)[:48])
for bad in [lambda: BatchSampler(range(3), 0, False), lambda: RandomSampler(range(3), num_samples=0), lambda: TensorDataset(x, torch.arange(3))]:
    try:
        bad()
    except (ValueError, AssertionError) as e:
        print(type(e).__name__, str(e)[:48])
print("worker info", tud.get_worker_info())
print("utils attribute", torch.utils.data.DataLoader is DataLoader)
