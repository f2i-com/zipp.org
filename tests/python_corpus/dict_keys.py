# dict and set keys of mixed types: 1 == 1.0 == True collisions, -0.0/0, tuple and
# frozenset keys, big ints, views and their set operations, ordering after updates.
d = {}
d[1] = "int"
d[1.0] = "float"
d[True] = "bool"
print("collide", d, len(d), d[1], d[1.0], d[True], type(list(d)[0]).__name__)
d2 = {True: "bool-first"}
d2[1] = "int-second"
d2[1.0] = "float-third"
print("collide-first-key-wins", d2, list(d2)[0] is True)
z = {0: "zero"}
z[0.0] = "float-zero"
z[-0.0] = "neg-zero"
z[False] = "false"
print("zeros", z, len(z))
s = {1, 1.0, True, 2, 2.0, 0, False, 0.0, -0.0}
print("set-collide", len(s), sorted(s), 1.0 in s, True in s, 3.0 in s)

big = {2**64: "a", 2**64 + 0.0: "b", -2**63: "c", float(2**53): "d", 2**53: "e"}
print("big-keys", sorted(big.items(), key=lambda kv: kv[0]), len(big), big[18446744073709551616])
tk = {(1, 2): "t12", (1, (2, 3)): "nested", (): "empty", (1.0, 2.0): "float-tuple"}
print("tuple-keys", tk, tk[(1, 2)], tk[(1, (2, 3))], tk[()], (True, 2) in tk)
fk = {frozenset([1, 2]): "fs", frozenset(): "empty-fs"}
print("frozenset-keys", fk[frozenset([2, 1])], fk[frozenset()], frozenset({1: 0, 2: 0}) in fk)
mixed = {"a": 1, b"a": 2, 1: 3, None: 4, (None,): 5, "1": 6, 1.5: 7}
print("mixed", mixed, mixed[None], mixed["1"], mixed[1.5], mixed[b"a"])
try:
    {[]: 1}
except TypeError as e:
    print("TypeError", e)
try:
    {1: 2}[[1]]
except TypeError as e:
    print("TypeError", e)
try:
    {(1, [2]): 3}
except TypeError as e:
    print("TypeError", e)
try:
    set().add({})
except TypeError as e:
    print("TypeError", e)

order = {}
for k in ["b", "a", "c"]:
    order[k] = len(order)
order["a"] = 99
del order["b"]
order["b"] = -1
print("order", order, list(order.keys()), list(order.values()))
order.update({"c": 0, "z": 26})
print("order-update", list(order.items()))
order.pop("a")
order.setdefault("a", "re-added")
print("order-readd", list(order))

counts = {}
for v in [1, 1.0, True, 2, 2.0, "2", (2,), 0, False]:
    counts[v] = counts.get(v, 0) + 1
print("counts", counts)
dd = {i: i * i for i in range(-3, 4)}
dd.update((float(i), "f") for i in range(0, 2))
print("int-float-update", dd)

a = {"x": 1, "y": 2, "z": 3}
b = {"y": 20, "w": 0}
ka, kb = a.keys(), b.keys()
print("views", ka & kb, sorted(ka | kb), sorted(ka - kb), sorted(ka ^ kb), "x" in ka, len(ka))
va = a.values()
ia = a.items()
a["new"] = 4
print("live-views", list(ka), list(va), len(ia), ("new", 4) in ia, ("new", 5) in ia)
print("items-setops", sorted(ia & {("x", 1), ("q", 0)}), sorted(k for k, _ in ia - {("x", 1)}))
print("view-eq", set(a.keys()) == {"x", "y", "z", "new"}, sorted(a.items()) == sorted(ia), list(reversed(a.keys())), list(reversed(a.values())))
print("dict-eq", {1: "a", 2: "b"} == {2: "b", 1: "a"}, {1: "a"} == {1.0: "a"}, {"a": [1]} == {"a": [1]}, {} != {0: 0})
print("dict-ops", {**a, **b}, a | b, b | a)
c = dict(a)
c |= b
print("dict-ior", c)
print("fromkeys", dict.fromkeys([1, 1.0, 2]), dict.fromkeys("aba", []))
print("dict-constructor", dict([(1, "a"), (1.0, "b")]), dict(zip([True, 1, 2], "xyz")), dict({1: 2}, **{"k": 3}))
nested = {}
for i in range(12):
    nested.setdefault(i % 3, {}).setdefault(i % 2, []).append(i)
print("nested", nested)
inv = {v: k for k, v in {"a": 1, "b": 2, "c": 1}.items()}
print("invert", inv)
sq = set()
for i in range(-5, 6):
    sq.add(i * i)
print("set-ints", sorted(sq), len(sq), 25 in sq, 25.0 in sq, -25 in sq)
fs = {frozenset({1}), frozenset({1.0}), frozenset({True})}
print("set-of-frozensets", len(fs))
print("set-ops", sorted({1, 2, 3} & {2, 3, 4}),sorted({1, 2} | {True, 3}), {1, 2} == {1.0, 2.0}, {1} < {1, 2}, {0} == {False})
