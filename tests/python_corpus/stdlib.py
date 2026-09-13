# The built-in modules: math, json, itertools, functools, collections, re, string, copy, dataclasses, enum.
import math
import json
import itertools
import functools
import collections
from collections import Counter, defaultdict, deque, namedtuple, OrderedDict
import re
import string
import copy
import operator
from dataclasses import dataclass, field
from enum import Enum, IntEnum, auto
import random
import sys
import os.path
from typing import List, Optional

print(math.pi, math.sqrt(16), math.floor(2.7), math.ceil(2.1), math.gcd(12, 18), math.factorial(10), math.isclose(0.1 + 0.2, 0.3), math.inf > 1e308, math.hypot(3, 4))
print(round(math.sin(math.pi / 2), 6), round(math.log(math.e), 6), math.log(8, 2), round(math.log10(1000), 6), math.trunc(-2.7), math.pow(2, 3), math.fabs(-3), math.isqrt(17), math.comb(5, 2), math.prod([1, 2, 3, 4]))

data = {"name": "zipp", "n": 3, "ok": True, "none": None, "list": [1, 2.5, "x"], "nested": {"k": [1, {"deep": "y"}]}}
text = json.dumps(data)
print(text)
print(json.dumps(data, indent=2, sort_keys=True))
back = json.loads(text)
print(back == data, back["nested"]["k"][1]["deep"], type(back["n"]).__name__, type(back["list"][1]).__name__, json.loads("[1, 2.0, \"s\", null, true]"), json.dumps("é\n"))
print(json.dumps([1, 2], separators=(",", ":")), json.dumps({"b": 1, "a": 2}, sort_keys=True), json.loads('{"x": 1e3, "y": -0.5, "z": 12345678901234567890}'))

print(list(itertools.islice(itertools.count(10, 5), 4)), list(itertools.chain([1], (2, 3), "ab")), list(itertools.product("ab", [1, 2])), list(itertools.permutations([1, 2, 3], 2)))
print(list(itertools.combinations("abcd", 2)), list(itertools.accumulate([1, 2, 3, 4])), list(itertools.zip_longest([1, 2], "abc", fillvalue="-")), [list(g) for k, g in itertools.groupby("aabbbc")])
print(list(itertools.takewhile(lambda x: x < 3, [1, 2, 3, 1])), list(itertools.dropwhile(lambda x: x < 3, [1, 2, 3, 1])), list(itertools.islice("abcdef", 1, 5, 2)), list(itertools.repeat("x", 3)), list(itertools.pairwise([1, 2, 3])))
print(functools.reduce(lambda a, b: a * b, range(1, 6)), functools.reduce(operator.add, [[1], [2]], []))
add5 = functools.partial(lambda a, b, c=0: a + b + c, 5)
print(add5(1), add5(1, c=10), add5.func is not None)


@functools.lru_cache(maxsize=None)
def fibm(n):
    return n if n < 2 else fibm(n - 1) + fibm(n - 2)


print(fibm(80), fibm.cache_clear() is None)


@functools.wraps(fibm)
def wrapped(n):
    return fibm(n)


print(wrapped.__name__)

c = Counter("mississippi")
print(c.most_common(2), c["s"], c["z"], sorted(c.elements())[:4], sum(c.values()), c + Counter("sip"), Counter(a=3) - Counter(a=1))
dd = defaultdict(list)
for k, v in [("a", 1), ("b", 2), ("a", 3)]:
    dd[k].append(v)
print(dict(dd), dd["missing"], len(dd), defaultdict(int)["x"])
dq = deque([1, 2, 3], maxlen=4)
dq.appendleft(0)
dq.append(4)
dq.rotate(1)
print(dq, dq.popleft(), dq.pop(), len(dq), list(dq), dq.maxlen)
Point = namedtuple("Point", ["x", "y"])
pt = Point(1, y=2)
print(pt, pt.x + pt.y, pt[0], tuple(pt), pt._asdict(), pt._replace(x=9), Point._fields, isinstance(pt, tuple), pt == (1, 2))
od = OrderedDict([("b", 1), ("a", 2)])
od.move_to_end("b")
print(list(od), od)
print(collections.Counter([1, 1, 2]).most_common(1), list(collections.ChainMap({"a": 1}, {"a": 2, "b": 3}).maps[1].items()))

m = re.search(r"(\w+)@(\w+)\.com", "mail bob@example.com now")
print(m.group(), m.group(1), m.group(2), m.groups(), m.span(), m.start(), m.end(), bool(re.match(r"\d+", "12ab")), re.match(r"\d+", "ab12"))
print(re.findall(r"\d+", "a1b22c333"), re.findall(r"(\w)(\d)", "a1 b2"), re.sub(r"\s+", " ", "a   b \t c"), re.sub(r"(\w+)@", r"\1 at ", "x@y"), re.split(r"[,;]\s*", "a, b;c"))
pat = re.compile(r"^(?P<key>\w+)=(?P<val>\d+)$")
mm = pat.match("answer=42")
print(mm.group("key"), mm.groupdict(), mm["val"], pat.pattern, re.sub(r"\d", lambda m2: str(int(m2.group()) * 2), "a1b2"), re.escape("a.b*c"), re.fullmatch(r"\w+", "abc") is not None, re.IGNORECASE == re.I)
print([m3.group() for m3 in re.finditer(r"\w+", "one two")], re.subn(r"o", "0", "foo boo"), re.search(r"x", "abc") is None, re.split(r"(-)", "a-b"), re.findall(r"[A-Z]", "Hello World", re.I))

print(string.ascii_lowercase[:5], string.digits, string.punctuation[:5], string.capwords("hello big world"), len(string.ascii_letters))
orig = {"a": [1, 2], "b": {"c": 3}}
shallow = copy.copy(orig)
deep = copy.deepcopy(orig)
orig["a"].append(3)
print(shallow["a"], deep["a"], shallow is orig, deep["b"] is orig["b"])
print(operator.add(1, 2), operator.itemgetter(1)([5, 6]), operator.attrgetter("real")(3), list(map(operator.mul, [1, 2], [3, 4])), operator.neg(5))


@dataclass
class Item:
    name: str
    price: float = 1.0
    tags: List[str] = field(default_factory=list)

    def total(self, qty):
        return self.price * qty


@dataclass(order=True, frozen=True)
class Version:
    major: int
    minor: int = 0


it = Item("pen", tags=["office"])
print(it, it == Item("pen", 1.0, ["office"]), it.total(3), Item("x").tags, Version(1, 2) < Version(1, 10), sorted([Version(2), Version(1, 5)]), hash(Version(1)) == hash(Version(1)))
try:
    Version(1).major = 2
except AttributeError as e:
    print("frozen:", type(e).__name__)


class Color(Enum):
    RED = 1
    GREEN = 2
    BLUE = auto()


class Level(IntEnum):
    LOW = 1
    HIGH = 2


print(Color.RED, repr(Color.GREEN), Color.BLUE.value, Color(2), Color["RED"], list(Color), Color.RED is Color(1), Color.RED == Color.RED, len(Color), Color.RED.name)
print(Level.LOW < Level.HIGH, Level.HIGH + 1, int(Level.LOW), Level(2), sorted([Level.HIGH, Level.LOW]), Level.LOW == 1)

random.seed(12345)
a = [random.randint(1, 100) for _ in range(5)]
random.seed(12345)
b = [random.randint(1, 100) for _ in range(5)]
print(a == b, all(1 <= x <= 100 for x in a), 0 <= random.random() < 1, random.choice([7]) == 7, len(random.sample(range(10), 3)))
print(sys.version_info[0], sys.maxsize > 2 ** 40, os.path.splitext("x/y.tar.gz"), os.path.basename("/p/q.py"), os.path.dirname("/p/q.py"), isinstance(sys.argv, list))
opt: Optional[int] = None
print(opt, List[int] is not None)
