# dict behaviour across its str-key and general representations: insertion
# order through the switch, deletes and re-inserts, NUL-led keys, lookups of
# non-str keys in an all-str dict, clear, copy, equality, views, popitem and
# OrderedDict reordering.
from collections import OrderedDict, Counter, defaultdict

d = {"b": 1, "a": 2, "\x00x": 3}
print("str-mode", d, list(d), d.get(1), 1 in d, d.get("\x00x"), len(d), d == {"a": 2, "b": 1, "\x00x": 3})
for bad in ([1], {}, {1}):
    try:
        d[bad]
    except TypeError as e:
        print("unhashable", e)
    try:
        bad in d
    except TypeError as e:
        print("unhashable-in", e)
d[1] = "one"
d["c"] = 4
d[2.5] = "f"
print("switched", d, list(d.keys()), list(d.values()), d[1], d.get("\x00x"), d[1.0])
del d["b"]
d["b"] = 9
del d[1]
d[True] = "t"
print("reinsert", list(d.items()), d.popitem(), len(d))
d.clear()
d["z"] = 0
d["y"] = 1
print("cleared", d, list(d), d.copy() == d, dict(d, w=2), {**d, 3: "x"})

e = {}
for i in range(5):
    e["k%d" % i] = i
for i in range(0, 5, 2):
    del e["k%d" % i]
e["k0"] = "back"
print("str-deletes", e, list(e.items()), e.pop("k1"), e.setdefault("k9", 9), e)

try:
    for k in e:
        e["new"] = 1
except RuntimeError as err:
    print("RuntimeError", err)

od = OrderedDict(a=1, b=2, c=3)
od.move_to_end("a")
od.move_to_end("c", last=False)
print("ordered", list(od), od.popitem(), od.popitem(last=False), od)
c = Counter("abracadabra")
print("counter", c.most_common(3), sorted(c.items()), c["z"], Counter({1: 2, "x": 3}).most_common())
dd = defaultdict(list)
dd["x"].append(1)
dd[2].append("two")
print("defaultdict", sorted(dd.items(), key=str), dict(dd))
m = {"x": 1}
m.update({2: "b"}, y=3)
print("update", m, {k: v for k, v in m.items() if k != 2}, dict.fromkeys(["p", "q", 3], 0))
