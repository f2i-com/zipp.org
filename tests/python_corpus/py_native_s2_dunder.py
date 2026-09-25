# obj.__dict__ is a live view of the instance's attributes (it aliases and
# stays identical; non-str keys are in py_native_s3_dunder.py),
# vars() is the same view,
# `self.__dict__ = self` in a dict subclass joins keys and attributes, and
# assigning a dict as __dict__ adopts it.
class P:
    cls_attr = "c"

    def __init__(self):
        self.a = 1


p = P()
d = p.__dict__
print("view", d, d is p.__dict__, vars(p) is d, len(d), "a" in d, "cls_attr" in d)
d["b"] = 2
p.c = 3
print("alias", p.b, d, sorted(vars(p)), d.get("c"), d.pop("a"), hasattr(p, "a"))
print("int-missing", d.get(1), 1 in d, len(d))
d.update(x=10, y=20)
print("update", p.x, p.y, sorted(d.items()))
d.clear()
print("clear", d, hasattr(p, "b"), p.cls_attr)
p.z = 26
print("copy", dict(d), d.copy(), {**d}, type(d.copy()).__name__)
print("eq", d == {"z": 26}, {"z": 26} == d, d != {})


class AttrDict(dict):
    def __init__(self, *a, **kw):
        super().__init__(*a, **kw)
        self.__dict__ = self


ad = AttrDict(k=1)
ad.m = 2
ad["n"] = 3
print("attrdict", ad, ad.k, ad.n, ad["m"], sorted(vars(ad)), len(ad))
del ad["k"]
print("attrdict-del", hasattr(ad, "k"), ad)


class Holder:
    pass


h = Holder()
src = {"u": 1, "v": 2}
h.__dict__ = src
h.w = 3
print("adopt", h.u, src, sorted(vars(h)))
h2 = Holder()
h2.__dict__ = {"only": 1}
print("adopt-literal", h2.only, h2.__dict__)


class Slotted:
    __slots__ = ("s",)

    def __init__(self):
        self.s = 1


try:
    Slotted().__dict__
except AttributeError as e:
    print("slots", "no __dict__")


def f():
    pass


f.attr = 5
print("function-dict", f.__dict__)
g = globals()
g["made"] = 7
print("globals", made, "P" in g, isinstance(g, dict))
