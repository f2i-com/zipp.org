# The dict protocol end to end: __missing__, defaultdict, Counter,
# OrderedDict, ChainMap, views and their set algebra, comparison and the
# | / |= operators, copy / deepcopy / pickle, json round trips.
import copy
import json
import pickle
from collections import ChainMap, Counter, OrderedDict, defaultdict


class Missing(dict):
    def __missing__(self, key):
        return "missing:%r" % (key,)


m = Missing(a=1)
print("missing", m["a"], m["b"], m.get("b"), "b" in m, len(m))

dd = defaultdict(list)
for i, w in enumerate("the quick brown the lazy the end".split()):
    dd[w].append(i)
print("defaultdict", sorted(dd.items()), dd.default_factory is list, len(dd))
dd2 = defaultdict(int, {"x": 1})
dd2["y"] += 2
print("defaultdict-int", sorted(dd2.items()))

c = Counter("mississippi")
print("counter", sorted(c.items()), c.most_common(1), c["z"], len(c))
c.update("sip")
c.subtract({"s": 10})
print("counter-update", sorted(c.items()), sorted((+c).items()), sorted((-c).items()))
print("counter-ops", sorted((Counter(a=3, b=1) + Counter(a=1, c=2)).items()), sorted((Counter(a=3, b=1) - Counter(a=1)).items()),
      sorted((Counter(a=3, b=1) | Counter(a=1, b=5)).items()), sorted((Counter(a=3, b=1) & Counter(a=1, b=5)).items()), Counter(a=2).total())

od = OrderedDict()
for k in "dcba":
    od[k] = ord(k)
od.move_to_end("c")
od.move_to_end("a", last=False)
print("ordered", list(od), od.popitem(), od.popitem(last=False), list(od))
print("ordered-eq", OrderedDict(a=1, b=2) == OrderedDict(b=2, a=1), OrderedDict(a=1, b=2) == {"b": 2, "a": 1})

cm = ChainMap({"a": 1, "b": 2}, {"b": 3, "c": 4})
print("chainmap", cm["a"], cm["b"], cm["c"], len(cm), sorted(cm), sorted(cm.items()))
cm2 = cm.new_child({"d": 5})
print("chainmap-child", cm2["d"], cm2["a"], len(cm2))

d = {"a": 1, "b": 2, "c": 3}
k, v, it = d.keys(), d.values(), d.items()
d["d"] = 4
print("views-live", list(k), list(v), list(it), len(k), "a" in k, ("a", 1) in it, ("a", 2) in it, 4 in v)
print("keys-algebra", sorted(k & {"a", "z"}), sorted(k | {"z"}), sorted(k - {"a"}), sorted(k ^ {"a", "z"}))
print("items-algebra", sorted(it & {("a", 1), ("b", 9)}), sorted(it - {("a", 1)})[:2])
print("keys-eq", d.keys() == {"a", "b", "c", "d"}, d.keys() == {"a"}, d.items() == {("a", 1), ("b", 2), ("c", 3), ("d", 4)})
print("reversed", list(reversed(d)))

a, b = {"x": 1, "y": 2}, {"y": 3, "z": 4}
print("or", a | b, b | a, {**a, **b})
a |= b
print("ior", a)
print("compare", {1: 2, 3: 4} == {3: 4, 1: 2}, {1: 2} != {1: 3}, {1: [1, 2]} == {1: [1, 2]}, {} == {}, {1: 1} == {1.0: 1.0})
try:
    {1: 2} < {1: 3}
except TypeError as e:
    print("lt", e)

nested = {"l": [1, 2, {"deep": {3, 4}}], "t": (1, 2), "s": {5, 6}, "fs": frozenset({7})}
shallow = copy.copy(nested)
deep = copy.deepcopy(nested)
nested["l"][2]["deep"].add(99)
print("copy", shallow["l"] is nested["l"], deep["l"] is nested["l"], 99 in deep["l"][2]["deep"], 99 in shallow["l"][2]["deep"], deep == shallow)
back = pickle.loads(pickle.dumps(deep))
print("pickle", back == deep, back is deep, type(back["s"]).__name__, sorted(back["s"]), back["fs"])
od2 = copy.deepcopy(OrderedDict([("z", 1), ("a", 2)]))
print("deepcopy-ordered", type(od2).__name__, list(od2.items()))
ds = copy.copy(defaultdict(set, {"k": {1}}))
print("copy-defaultdict", type(ds).__name__, ds.default_factory is set, dict(ds))

doc = {"name": "x", "vals": [1, 2.5, None, True, "s"], "nested": {"a": {"b": {}}}, "dup": 1}
text = json.dumps(doc, sort_keys=True)
print("json", text)
again = json.loads(text)
print("json-roundtrip", again == doc, list(again), json.loads('{"a": 1, "a": 2, "b": 3}'))
print("json-indent", json.dumps({"b": [1, {"c": 2}], "a": None}, indent=2))
big = json.loads("{" + ", ".join('"k%d": %d' % (i, i) for i in range(200)) + "}")
print("json-big", len(big), big["k0"], big["k199"], list(big)[:3], sum(big.values()))
print("json-nonstr-keys", json.dumps({1: "a", 2.5: "b", True: "c", None: "d"}))

print("fromkeys", dict.fromkeys("abc", 0), dict.fromkeys(range(3)))
print("setdefault", d.setdefault("a", 100), d.setdefault("new", []), d)
print("pop", d.pop("new"), d.pop("zz", "dflt"), len(d))
try:
    d.pop("zz")
except KeyError as e:
    print("pop-missing", repr(e))
print("popitem", d.popitem(), d)
try:
    {}.popitem()
except KeyError as e:
    print("popitem-empty", e)
d.clear()
print("cleared", d, len(d), bool(d))
d.update({"q": 1}, r=2)
d.update([("s", 3)])
print("update", d)
print("dict-ctor", dict({"a": 1}, b=2), dict(zip("xy", (1, 2))), dict(m))
