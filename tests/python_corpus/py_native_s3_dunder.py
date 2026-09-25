# obj.__dict__ over layout-mode storage: identity, aliasing both ways,
# non-str keys, __slots__ beside a __dict__, `self.__dict__ = self`,
# assigning a dict as __dict__, __class__ reassignment, and the protocols
# that walk an instance's attributes (copy, deepcopy, pickle, dataclasses,
# vars, json, dir).
import copy
import dataclasses
import json
import pickle


class A:
    def __init__(self):
        self.x = 1
        self.y = 2


a = A()
d = a.__dict__
print("identity", d is a.__dict__, vars(a) is d, id(d) == id(a.__dict__))
d["z"] = 3
a.w = 4
print("alias", a.z, d["w"], list(d), len(d))
d[5] = "five"
d[(1, 2)] = "tuple"
print("non-str", d[5], d[(1, 2)], a.x, list(d))
a.v = 6
print("after", sorted(k for k in d if isinstance(k, str)), a.v, d.get("v"))
del d["x"]
print("del-via-dict", hasattr(a, "x"), "x" in d)


class Slotted:
    __slots__ = ("s", "__dict__")

    def __init__(self):
        self.s = 1
        self.t = 2


sl = Slotted()
print("slots", sl.s, sl.t, vars(sl))


class AttrDict(dict):
    def __init__(self, *args, **kw):
        super().__init__(*args, **kw)
        self.__dict__ = self


ad = AttrDict(one=1)
ad.two = 2
ad["three"] = 3
print("attrdict", ad, ad.one, ad["two"], ad.three, len(ad))


class H:
    pass


h = H()
shared = {"p": 1}
h.__dict__ = shared
h.q = 2
shared["r"] = 3
print("adopted", h.p, h.r, shared, h.__dict__ is shared)


class B:
    def __init__(self):
        self.x = "b"

    def who(self):
        return "B"


class C:
    def who(self):
        return "C"


b = B()
print("before-class", b.who(), b.x)
b.__class__ = C
print("after-class", b.who(), b.x, type(b).__name__)


class Node:
    def __init__(self, val, nxt=None):
        self.val = val
        self.nxt = nxt


chain = Node(1, Node(2, Node(3)))
c1 = copy.copy(chain)
c2 = copy.deepcopy(chain)
chain.nxt.val = 20
print("copy", c1.nxt.val, c2.nxt.val, c1.nxt is chain.nxt, vars(c2.nxt.nxt))


@dataclasses.dataclass
class DC:
    a: int
    b: str = "x"
    c: list = dataclasses.field(default_factory=list)


dc = DC(1)
dc.c.append(5)
print("dataclass", dc, dataclasses.asdict(dc), dataclasses.astuple(dc), dataclasses.replace(dc, b="y"), vars(dc))
print("json", json.dumps(A().__dict__), json.dumps(vars(dc)))
print("dir", [n for n in dir(A()) if not n.startswith("__")])


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __eq__(self, other):
        return isinstance(other, Point) and vars(self) == vars(other)

    def __hash__(self):
        return hash((self.x, self.y))


pp = {Point(1, 2): "a", Point(3, 4): "b"}
print("hashable", pp[Point(1, 2)], Point(3, 4) in pp, vars(Point(1, 2)) == {"x": 1, "y": 2})


class State:
    def __init__(self):
        self.a = 1
        self.b = [1, 2]

    def __getstate__(self):
        s = dict(self.__dict__)
        s["b"] = tuple(s["b"])
        return s

    def __setstate__(self, s):
        self.__dict__.update(s)
        self.restored = True


st = copy.deepcopy(State())
print("state", st.a, st.b, st.restored, list(vars(st)))
print("repr-default", repr(vars(Node(1)))[:30])
