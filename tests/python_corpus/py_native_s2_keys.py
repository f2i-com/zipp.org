# Dict and set keys of every native kind: equal numbers of different types
# share one entry, -0.0 equals 0.0, NaN equals nothing (not even itself),
# big ints hash modulo 2**61 - 1, tuples and frozensets hash by value.
import math

d = {}
d[1] = "int"
d[1.0] = "float"
d[True] = "bool"
print("one", d, len(d), type(list(d)[0]).__name__)
d[0] = "zero"
d[-0.0] = "negzero"
d[False] = "false"
print("zero", d, len(d))
print("get", d.get(1.0), d.get(True), d.get(0.0), d.get(-0.0), d.get(2), 1 in d, 1.5 in d)

# (A NaN key is found only through an identical object in CPython; floats
# have no identity here, so only separate NaNs are compared.)
dn = {float("nan"): 1, float("nan"): 2}
print("nan-two", len(dn), float("nan") in dn, dn.get(float("nan")), sorted(dn.values()))
sn = {float("nan"), float("nan"), 1.0}
print("nan-set", len(sn), float("nan") in sn, 1 in sn)

M = 2**61 - 1
big = {M: "m", 2 * M: "2m", M + 5: "m+5", 5: "five", 2**64: "2^64", -(2**70): "neg", -1: "minus1", -2: "minus2"}
print("big", len(big), big[5], big[M + 5], big[2 * M], big[2**64], big[-(2**70)], big[-1], big[-2])
print("big-hash", hash(M), hash(2 * M), hash(M + 5), hash(-1), hash(-2), hash(2**64), hash(-(2**70)))
print("big-float", big.get(2.0**64), float(2**64) in big, 5.0 in big)
del big[5]
print("big-del", M + 5 in big, 5 in big, len(big))

t = {(1, 2): "a", (1, (2, 3)): "b", (): "empty", ("x", 1.0): "c", (1, 2.0, True): "d"}
print("tuple", t[(1, 2.0)], t[(1, (2, 3.0))], t[()], t[("x", 1)], t[(1.0, 2, 1)])
print("tuple-hash", hash((1, 2)) == hash((1.0, 2.0)), hash(()) == hash(()), hash((1, (2, 3))) == hash((True, (2.0, 3))))

f = {frozenset(): 0, frozenset({1, 2}): 12, frozenset({(1, 2), "s"}): "mixed"}
print("frozen", f[frozenset()], f[frozenset({2, 1})], f[frozenset({2.0, 1.0})], f[frozenset({"s", (1, 2)})])
print("frozen-in-set", frozenset({1, 2}) in {frozenset({1, 2})}, {frozenset({1}): 1} == {frozenset({1.0}): 1})

s = {1, 1.0, True, 2, 2.0}
print("set-dedup", len(s), sorted(s))
print("set-members", 1.0 in s, 2 in s, 3 in s, None in s)
n = {None: "none", "": "empty", "a": "a"}
print("none-and-str", n[None], n[""], n["a"], None in n)

inf = {math.inf: "inf", -math.inf: "-inf", 0.5: "half", 1e300: "huge"}
print("floats", inf[float("inf")], inf[-math.inf], inf[0.5], inf[1e300], hash(math.inf), hash(-math.inf), hash(0.5))

for bad in ([1], {1: 2}, {3}):
    try:
        {bad: 1}
    except TypeError as e:
        print("unhashable", e)
    try:
        bad in {1: 2}
    except TypeError as e:
        print("unhashable-in", e)
try:
    {(1, [2]): 3}
except TypeError as e:
    print("unhashable-tuple", e)

strs = {}
for i in range(300):
    strs["k%d" % i] = i
print("many-str", len(strs), strs["k0"], strs["k299"], sum(strs.values()), list(strs)[:3], list(strs)[-2:])
ints = {}
for i in range(-500, 500, 3):
    ints[i * 7919] = i
print("many-int", len(ints), ints[-500 * 7919], ints[499 * 7919 - 7919 * 2] if (499 * 7919 - 7919 * 2) in ints else None)
for i in range(-500, 500, 6):
    del ints[i * 7919]
print("many-int-del", len(ints), list(ints)[:4], list(ints)[-3:])
