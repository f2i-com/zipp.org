# `==` and `!=` between lists, dicts and tuples nested in each other and
# holding numbers of every kind, strs, None and bools, beside subclasses,
# objects with their own `__eq__`, sets and deep nesting, in straight-line
# code and in loops long enough to compile: CPython's answers.
import json


class E:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return isinstance(other, E) and other.v % 10 == self.v % 10


class LL(list):
    def __eq__(self, other):
        return True


class DD(dict):
    pass


big = 2 ** 70
pairs = [
    ([], []), ([1], [1]), ([1], [1.0]), ([1], [True]), ([0], [False]), ([1, 2], [2, 1]),
    ([big], [big]), ([big], [float(big)]), ([2 ** 53 + 1], [float(2 ** 53 + 1)]),
    (["a", "b"], ["a", "b"]), (["a"], ["a" + ""]), ([None], [None]), ([None], [0]),
    ([[1, [2, [3]]]], [[1, [2, [3]]]]), ([[1, [2, [3]]]], [[1, [2, [4]]]]),
    ({}, {}), ({"a": 1}, {"a": 1.0}), ({"a": 1, "b": 2}, {"b": 2, "a": 1}), ({"a": 1}, {"a": 1, "b": 2}),
    ({1: "x"}, {1.0: "x"}), ({(1, 2): [3]}, {(1, 2): [3]}), ({(1, 2): [3]}, {(1, 2): [4]}),
    ({"k": {"n": [1, {"m": None}]}}, {"k": {"n": [1, {"m": None}]}}), ({"k": {"n": [1, {"m": None}]}}, {"k": {"n": [1, {"m": 0}]}}),
    ((1, 2), (1, 2)), ((1, [2]), (1, [2])), ([1, 2], (1, 2)), ([], {}), ([1], 1), ({}, None),
    ([E(1)], [E(11)]), ([E(1)], [E(2)]), ({"e": E(3)}, {"e": E(13)}), ([2], LL([1])),
    (DD(a=1), {"a": 1}), ({"a": 1}, DD(a=2)), ([{1, 2}], [{2, 1}]), ([frozenset([1])], [frozenset([1])]),
    ([float("inf")], [float("inf")]), ([-0.0], [0.0]), (["é", "中"], ["é", "中"]),
]
for a, b in pairs:
    print(a == b, a != b, b == a)

class Never:
    def __eq__(self, other):
        return False


nv, cn = Never(), complex(float("nan"), 1)
print(nv == nv, [nv] == [nv], {"k": nv} == {"k": nv}, cn == cn, [cn] == [cn], (1, cn) == (1, cn))
deep_a = deep_b = 1
for i in range(60):
    deep_a = [deep_a, i]
    deep_b = [deep_b, i]
print(deep_a == deep_b, deep_a != [deep_b, 0])
shared = [1, 2]
print([shared, shared] == [shared, [1, 2]])

# The same comparisons in a loop long enough to compile, with the operands
# changing kind from one iteration to the next.
doc = {"records": [{"id": i, "tags": ["t%d" % i, "g"], "geo": {"x": i % 7, "y": -i}} for i in range(50)],
       "meta": {"v": 2, "ok": True, "none": None}}
copy = json.loads(json.dumps(doc))
counts = [0, 0, 0]
for i in range(3000):
    a, b = pairs[i % len(pairs)]
    if a == b:
        counts[0] += 1
    if doc == copy:
        counts[1] += 1
    if i == 1500:
        copy["records"][7]["geo"]["y"] = 1
    if [i, "x", (i, 2)] != [i, "x", (i, 2)]:
        counts[2] += 1
print(counts, doc == copy)
