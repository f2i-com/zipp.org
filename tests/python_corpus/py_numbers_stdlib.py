# Numeric and data stdlib fidelity: json int/float distinction and dict
# keys, math overflow and domain errors, bisect bounds, accumulate(initial=),
# OrderedDict equality, struct 'p', typing.NamedTuple without collections,
# and a re.sub callback that runs its own pattern.
from typing import NamedTuple
import json
import math
import bisect
import itertools
import operator
import struct
import re


class NT(NamedTuple):
    a: int
    b: str = "d"


print("namedtuple", NT(1), NT(2, "x").b, NT._fields, NamedTuple("P", [("x", int), ("y", int)])(3, 4))

# json keeps ints and floats apart and renders dict keys as plain keys.
print("json-floats", json.dumps({"loss": 1.0, "big": 1e16, "neg0": -0.0, "tiny": 1e-7, "n": 123456789.0, "i": 2**70}))
print("json-roundtrip", type(json.loads(json.dumps(2.0))).__name__, json.loads(json.dumps([1, 1.0, -0.0])), json.dumps(3.0), json.dumps(True & False))
print("json-keys", json.dumps({"b": 1, "10": 2, "2": 3}), json.dumps({"__proto__": 1, "a": 2}), json.dumps({1: "int", "1": "str"}))
print("json-markers", json.dumps({"__big": "1, \"admin\": true"}), json.dumps({"__special": "alert(1)"}), json.dumps({"profile": json.loads('{"__big": "x"}')}))
print("json-keytypes", json.dumps({1.5: 1, 1.0: 2, float("inf"): 3, None: 4, True: 5, -0.0: 6}), json.dumps({(1, 2): 3, "a": 1}, skipkeys=True))
print("json-special", json.dumps([float("nan"), float("inf"), -float("inf")]), json.loads("[NaN, -Infinity]", parse_constant=lambda s: s))
print("json-sorted", json.dumps({"b": 1, "a": [1, {"d": 2, "c": 3}]}, sort_keys=True), json.dumps({2: 1, 1: 2}, sort_keys=True))
print("json-indent", json.dumps({"x": [1, 2]}, indent="\t") == '{\n\t"x": [\n\t\t1,\n\t\t2\n\t]\n}', json.dumps([1, [2, []], {}], indent=0) == "[\n1,\n[\n2,\n[]\n],\n{}\n]")
print("json-ascii", json.dumps("é\x7f"), json.dumps("é\x7f", ensure_ascii=False), json.dumps({"k": 1}, separators=(",", ":")))
print("json-hooks", json.loads('{"a": {"b": 1}}', object_hook=lambda d: sorted(d)), json.loads('{"a": 1, "b": 2}', object_pairs_hook=lambda p: p),
      json.loads("[1.5, 2]", parse_float=lambda s: ("F", s), parse_int=lambda s: ("I", s)))
print("json-default", json.dumps([1, {"a": (1, 2)}, {3, 1}], default=sorted), json.dumps({"o": object()}, default=lambda o: "obj"))
for thunk in (lambda: json.dumps(float("nan"), allow_nan=False), lambda: json.dumps({(1, 2): 3}), lambda: json.dumps(object()),
              lambda: json.dumps({"a": 1, 2: 3}, sort_keys=True), lambda: json.loads("[1,\n 2,\n x]"), lambda: json.loads('{"a" 1}'),
              lambda: json.loads(""), lambda: json.loads("[1] x")):
    try:
        print("json-error", thunk())
    except (ValueError, TypeError) as e:
        print("json-error", type(e).__name__, e)
cyc = []
cyc.append(cyc)
try:
    json.dumps(cyc)
except ValueError as e:
    print("json-cycle", e)

# math: range errors, domain errors, and logs of huge ints.
for name, thunk in [("exp", lambda: math.exp(1000)), ("exp2", lambda: math.exp2(1024)), ("cosh", lambda: math.cosh(1000)),
                    ("pow-range", lambda: math.pow(10.0, 400)), ("pow-domain", lambda: math.pow(0.0, -1)), ("pow-neg", lambda: math.pow(-1, 0.5)),
                    ("ldexp", lambda: math.ldexp(1, 1024)), ("gamma", lambda: math.gamma(172)), ("gamma0", lambda: math.gamma(0)),
                    ("log0", lambda: math.log(0)), ("log1p", lambda: math.log1p(-1)), ("atanh", lambda: math.atanh(1)),
                    ("fmod", lambda: math.fmod(float("inf"), 1)), ("tan", lambda: math.tan(float("inf"))), ("sqrt-big", lambda: math.sqrt(10**400)),
                    ("log-base1", lambda: math.log(10, 1)), ("floor-inf", lambda: math.floor(float("inf"))), ("trunc-nan", lambda: math.trunc(float("nan"))),
                    ("round-inf", lambda: round(float("-inf"))), ("fsum", lambda: math.fsum([1e308, 1e308])), ("lgamma", lambda: math.lgamma(1e308))]:
    try:
        print("math", name, thunk())
    except (OverflowError, ValueError, ZeroDivisionError) as e:
        print("math", name, type(e).__name__, e)
print("math-ok", math.pow(1, float("nan")), math.pow(float("nan"), 0), math.exp(float("inf")), math.exp(-1000), math.ldexp(0.5, 1024),
      math.ldexp(1.5, -1074), math.ldexp(3, -1076), math.ldexp(-1.0, -2000), math.ldexp(1, -10**30), math.hypot(1e308, 1e308), math.gamma(float("inf")))
print("math-bigint", math.log2(2**2000), math.log10(10**400), math.log(10**400), math.log(2**1100, 2), math.log2(3**3000), math.isqrt(10**400) == 10**200, math.isqrt(2**1000 - 1) ** 2 < 2**1000)

# bisect honours lo and hi, positionally and by keyword.
a = [0, 1, 2, 3, 4, 5, 6, 7]
print("bisect", bisect.bisect_left(a, 6, 0, 4), bisect.bisect_left(a, 1, 3), bisect.bisect_right(a, 6, hi=3), bisect.bisect_right([1, 2, 2, 3], 2, 0, 2),
      bisect.bisect(a, 3, lo=5), bisect.bisect_left(a, 10, 2, 6), bisect.bisect_left((1, 3, 5), 4))
try:
    bisect.bisect_left(a, 1, -1)
except ValueError as e:
    print("ValueError", e)
b = [1, 5, 9]
bisect.insort(b, 5, 0, 1)
bisect.insort_left(b, 7, lo=2)
rows = [("a", 1), ("c", 3)]
bisect.insort(rows, ("b", 2), key=lambda r: r[1])
print("insort", b, rows, bisect.bisect_left(rows, 2, key=lambda r: r[1]))

# itertools.accumulate with an initial value yields it first.
print("accumulate", list(itertools.accumulate([1, 2, 3], operator.mul, initial=10)), list(itertools.accumulate([], initial=5)),
      list(itertools.accumulate([1, 2, 3], initial=None)), list(itertools.accumulate([3, 1, 2], max)))

# OrderedDict equality is order-sensitive only between two OrderedDicts.
from collections import OrderedDict
x, y = OrderedDict(a=1, b=2), OrderedDict(b=2, a=1)
print("ordereddict", x == y, x != y, x == dict(b=2, a=1), dict(b=2, a=1) == x, x == OrderedDict(a=1, b=2), OrderedDict() == {})

# struct 'p' writes a length byte.
print("struct-p", struct.pack("<5p", b"ab"), struct.pack("3p", b"abcdef"), struct.unpack("5p", struct.pack("5p", b"hey")), struct.pack("1p", b"z"))


# A replacement callable may run the same pattern again.
env = {"a": "{b}", "b": "B", "c": "C"}


def expand(text):
    return re.sub(r"\{(\w+)\}", lambda m: expand(env[m.group(1)]), text)


pat = re.compile(r"\d+")
print("re-reentrant", expand("x {a} y {c} z"), pat.sub(lambda m: str(len(pat.findall("1 22 333"))) + m.group(), "a1b22"),
      re.sub("x*", "-", "abxd"), re.sub("", "-", "a😀b"), re.findall("", "😀a"), re.split("x*", "axbc"))
