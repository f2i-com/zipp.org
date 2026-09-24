# set.pop() and dict.popitem() remove the entry they pick, NaN included (a
# NaN equals nothing, itself included, so removing it by equality would
# leave it in place): pop loops over sets holding one NaN, several distinct
# NaN objects and other values, the popped NaN being the set's own object,
# dict.popitem() and OrderedDict.popitem() over NaN keys (LIFO and FIFO),
# and the ordinary cases beside them.
import math
from collections import OrderedDict

nan = float("nan")
s = {nan}
pops = 0
while s:
    x = s.pop()
    pops += 1
    if pops > 10:
        break
print("single", pops, len(s), math.isnan(x), x is nan)

s = {float("nan"), float("nan"), float("nan"), 1.5, "a"}
print("distinct", len(s))
popped = []
while s and len(popped) < 20:
    popped.append(s.pop())
print(len(popped), len(s), sum(1 for p in popped if isinstance(p, float) and math.isnan(p)), sorted(repr(p) for p in popped))

s = set()
for i in range(4):
    s.add(float("nan"))
s.add(2)
print("added", len(s))
n = 0
while s and n < 20:
    s.pop()
    n += 1
print("drained", n, len(s))

try:
    set().pop()
except KeyError as e:
    print("KeyError", e)

d = {float("nan"): 1, float("nan"): 2, "k": 3, nan: 4}
print("dict", len(d))
items = []
while d and len(items) < 20:
    k, v = d.popitem()
    items.append((repr(k), v))
print(items, len(d))

od = OrderedDict()
od[float("nan")] = "first"
od["x"] = "mid"
od[float("nan")] = "last"
print(od.popitem(last=False), list(od.values()))
print(od.popitem(), list(od.values()), len(od))
print(od.popitem(), len(od))

fs = {1, 2, 3}
out = []
while fs:
    out.append(fs.pop())
print(sorted(out))
d2 = {i: i * i for i in range(5)}
print([d2.popitem() for _ in range(5)], d2)
