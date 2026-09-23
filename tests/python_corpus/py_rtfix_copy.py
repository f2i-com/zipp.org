# copy.copy / copy.deepcopy of dict, list and set subclasses, objects with
# nested containers, and the __copy__/__deepcopy__/__reduce_ex__/
# __getstate__/__setstate__ hooks.
import copy
from collections import OrderedDict, defaultdict, Counter, deque, namedtuple


class MyDict(dict):
    def __init__(self, *a, **kw):
        super().__init__(*a, **kw)
        self.tag = "t"


class MyList(list):
    pass


class MySet(set):
    pass


od = OrderedDict([("b", [1]), ("a", [2])])
dd = defaultdict(list, x=[1])
cn = Counter("abracadabra")
md = MyDict(k=[1, 2])
md.extra = {"n": 1}
ml = MyList([[1], [2]])
ml.note = "hi"
ms = MySet({1, 2, 3})
for name, fn in (("copy", copy.copy), ("deepcopy", copy.deepcopy)):
    c = fn(od)
    print(name, type(c).__name__, list(c.items()), c["b"] is od["b"])
    c = fn(dd)
    c["y"].append(5)
    print(name, type(c).__name__, c.default_factory is list, sorted(c.items()), "y" in dd, c["x"] is dd["x"])
    c = fn(cn)
    print(name, type(c).__name__, c.most_common(3), c == cn)
    c = fn(md)
    print(name, type(c).__name__, dict(c), c.tag, c.extra, c["k"] is md["k"], c.extra is md.extra)
    c = fn(ml)
    print(name, type(c).__name__, list(c), c.note, c[0] is ml[0])
    c = fn(ms)
    print(name, type(c).__name__, sorted(c), c is ms)

# Shared references and cycles survive deepcopy as one object.
shared = [1, 2]
box = {"a": shared, "b": shared, "od": OrderedDict(s=shared)}
box["self"] = box
d = copy.deepcopy(box)
print(d["a"] is d["b"], d["a"] is not shared, d["od"]["s"] is d["a"], d["self"] is d)


class Node:
    def __init__(self, name):
        self.name = name
        self.children = OrderedDict()
        self.parent = None

    def add(self, child):
        child.parent = self
        self.children[child.name] = child
        return child


root = Node("root")
leaf = root.add(Node("leaf")).add(Node("leaf2"))
r2 = copy.deepcopy(root)
print(list(r2.children), r2.children["leaf"].parent is r2, r2.children["leaf"] is not root.children["leaf"],
      list(r2.children["leaf"].children), r2.__dict__.keys() == root.__dict__.keys())
r3 = copy.copy(root)
print(r3 is not root, r3.children is root.children)


class WithHooks:
    def __init__(self, v):
        self.v = v

    def __copy__(self):
        return WithHooks(("copied", self.v))

    def __deepcopy__(self, memo):
        print("  deepcopy hook got memo dict:", isinstance(memo, dict))
        return WithHooks(("deep", copy.deepcopy(self.v, memo)))


w = WithHooks([1])
print(copy.copy(w).v, copy.deepcopy(w).v, copy.deepcopy([w, w])[0].v)


class Reduced:
    def __init__(self, a, b=0):
        self.a, self.b = a, b

    def __reduce_ex__(self, proto):
        return (Reduced, (self.a,), {"b": self.b, "via": "reduce"})


r = copy.deepcopy(Reduced([1], 5))
print(r.a, r.b, r.via, copy.copy(Reduced(2, 3)).__dict__)


class Stateful:
    def __init__(self):
        self.x = 1
        self.cache = "big"

    def __getstate__(self):
        s = self.__dict__.copy()
        del s["cache"]
        return s

    def __setstate__(self, s):
        self.__dict__.update(s)
        self.cache = "rebuilt"


s2 = copy.deepcopy(Stateful())
print(s2.x, s2.cache, copy.copy(Stateful()).cache)

P = namedtuple("P", "x y")
p = P([1], 2)
pc, pd = copy.copy(p), copy.deepcopy(p)
print(pc, pd, type(pd).__name__, pd.x is p.x, pc.x is p.x)
dq = deque([[1], [2]], maxlen=3)
dqc = copy.deepcopy(dq)
print(dqc, dqc.maxlen, dqc[0] is dq[0])
t = (1, "a", (2, 3))
print(copy.deepcopy(t) is t, copy.deepcopy(([1],))[0] is not None)
e = copy.deepcopy(ValueError("bad", [1]))
print(type(e).__name__, e.args)
print(copy.deepcopy(len) is len, copy.deepcopy(Node) is Node, copy.copy(3.5), copy.deepcopy("s"))
