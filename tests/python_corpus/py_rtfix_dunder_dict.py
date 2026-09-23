# obj.__dict__ is a real dict that is also a live view of the attributes.
import copy
import json


class C:
    def __init__(self):
        self.a = 1
        self.b = [2]


c = C()
d = c.__dict__
print(type(d).__name__, type(d) is dict, isinstance(d, dict), d is c.__dict__)
print(json.dumps(d), json.dumps(vars(c), sort_keys=True))
d["z"] = 26
c.y = 25
print(c.z, d["y"], sorted(d), len(d), "a" in d)
del d["y"]
print(hasattr(c, "y"), list(d.keys()), list(d.values()), list(d.items()))
d.update({"n": 5}, m=6)
print(c.n, c.m, d.get("q", "missing"), d.pop("m"), hasattr(c, "m"))
print(dict(d) == d, dict(**d)["a"], {**d}["z"], d | {"x": 0})
cp = copy.copy(d)
cp["a"] = 100
print(type(cp).__name__, c.a, copy.deepcopy(d)["b"] is not c.b)
print(sorted(k for k in d), [k for k, v in sorted(d.items()) if isinstance(v, int)])
d.setdefault("s", "set")
print(c.s, d == {"a": 1, "b": [2], "z": 26, "n": 5, "s": "set"})
print(json.loads(json.dumps(C().__dict__)))


class P:
    x = 3

    def __init__(self):
        self.__dict__["direct"] = "yes"


p = P()
print(p.direct, p.__dict__, "x" in p.__dict__, vars(p) is p.__dict__)
p.__dict__.clear()
print(p.__dict__, hasattr(p, "direct"), p.x)
g = globals()
g["made_here"] = 42
print(made_here, isinstance(g, dict), "C" in g)
import math
print(isinstance(math.__dict__, dict), math.__dict__["pi"] == math.pi)


# Assigning __dict__ replaces the attributes; a dict subclass that makes its
# own __dict__ itself (the AttrDict config idiom) shares keys and attributes.
class A:
    pass


a = A()
a.__dict__ = {"x": 1}
a.y = 2
print(a.x, a.__dict__, sorted(vars(a)))
src = {"p": 1}
b = A()
b.__dict__ = src
b.q = 2
print(src, b.__dict__ is src, len(src))


class AttrDict(dict):
    def __init__(self, *args, **kw):
        super().__init__(*args, **kw)
        self.__dict__ = self


cfg = AttrDict(lr=0.1, bs=32)
cfg.wd = 0.01
cfg["momentum"] = 0.9
print(cfg.lr, cfg["wd"], cfg.momentum, len(cfg), sorted(cfg), json.dumps(cfg, sort_keys=True))
del cfg.bs
cfg.update(epochs=3)
print("bs" in cfg, cfg.epochs, dict(cfg) == {"lr": 0.1, "wd": 0.01, "momentum": 0.9, "epochs": 3})
try:
    a.__dict__ = [1]
except TypeError:
    print("TypeError for a non-dict __dict__")
