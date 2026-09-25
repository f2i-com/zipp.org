# Iteration order and its guards: insertion order, a replaced value keeps
# its place, delete-then-reinsert goes last, popitem is last in first out,
# "changed size during iteration", and the order of small-int sets.
d = {}
for k in (5, 3, 9, 1):
    d[k] = k * 10
d[3] = "replaced"
print("insertion", d)
del d[5]
d[5] = "back"
print("reinsert", d, list(d.keys()), list(d.values()))
print("popitems", d.popitem(), d.popitem(), d)
for i in range(100):
    d[i % 7] = i
    if i % 3 == 0:
        del d[i % 7]
print("churn", d)

e = {i: i for i in range(10)}
for k in list(e):
    if k % 2:
        del e[k]
print("delete-while-listing", e)
try:
    for k in e:
        e[k + 100] = 1
except RuntimeError as ex:
    print("dict-grow", ex)
try:
    for k in e:
        del e[k]
except RuntimeError as ex:
    print("dict-shrink", ex)
f = {1: 1, 2: 2}
for k in f:
    f[k] = "same size is fine"
print("dict-assign", f)
try:
    for v in f.values():
        f.clear()
except RuntimeError as ex:
    print("values-clear", ex)
g = {1: 1, 2: 2, 3: 3}
try:
    for kv in g.items():
        g.pop(3, None)
        g[4] = 4
except RuntimeError as ex:
    print("items-same-size", ex)
print("items-after", g)

s = {1, 2, 3}
try:
    for x in s:
        s.add(x + 10)
except RuntimeError as ex:
    print("set-grow", ex)
try:
    for x in s:
        s.discard(x)
except RuntimeError as ex:
    print("set-shrink", ex)

print("small-ints", {3, 1, 2}, {7, 0, 5, 2}, set(range(9, -1, -1)), {4, 4, 1})
print("small-ints-build", set([6, 2, 4]), {x * 2 for x in (4, 1, 3)}, frozenset({3, 2}))
t = set()
for x in (5, 1, 4, 2, 3):
    t.add(x)
t.discard(4)
print("small-ints-mut", t, sorted(t | {0}), t.pop(), t)
print("set-pop-drains", [ {9, 3, 7}.pop() ], sorted({9, 3, 7} - {3}))
u = {2, 0, 1}
drained = []
while u:
    drained.append(u.pop())
print("drained", drained)
print("list-order", list({1: 0, 0: 0, 2: 0}), list(dict.fromkeys([3, 1, 2, 1, 3])))
print("comprehension", {k: v for k, v in zip("cab", range(3))}, {v: k for k, v in {"x": 1, "y": 2}.items()})
