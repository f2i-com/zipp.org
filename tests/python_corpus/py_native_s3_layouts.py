# Instance attributes in layout mode: many instances of one class sharing a
# layout, a class whose __init__ stores in two orders, attributes added
# after construction, deletion and re-adding (goes last), class attributes
# shadowed and unshadowed, methods shadowed by instance attributes, and
# properties beside plain attributes.


class Point:
    kind = "point"

    def __init__(self, x, y):
        self.x = x
        self.y = y

    def norm1(self):
        return abs(self.x) + abs(self.y)


pts = [Point(i, -i) for i in range(50)]
print("same-layout", sum(p.x for p in pts), sum(p.norm1() for p in pts), [vars(p) for p in pts[:2]])


class TwoOrders:
    def __init__(self, flip):
        if flip:
            self.b = 2
            self.a = 1
        else:
            self.a = 1
            self.b = 2


objs = [TwoOrders(i % 2 == 0) for i in range(6)]
print("two-orders", [list(vars(o)) for o in objs[:2]], sum(o.a + o.b for o in objs))

p = Point(1, 2)
p.z = 3
p.w = 4
print("grown", vars(p), p.z + p.w)
del p.x
print("deleted", vars(p), hasattr(p, "x"))
p.x = 10
print("readded", vars(p), p.x)
del p.z
del p.w
p.w = 5
print("after-deletes", list(vars(p)), p.w, p.y)

q = Point(5, 6)
print("class-attr", q.kind, Point.kind)
q.kind = "mine"
print("shadowed", q.kind, Point(0, 0).kind)
del q.kind
print("unshadowed", q.kind)
Point.kind = "changed"
print("class-changed", q.kind, pts[0].kind)

r = Point(7, 8)
r.norm1 = lambda: "instance function"
print("method-shadowed", r.norm1(), Point(7, 8).norm1())
del r.norm1
print("method-back", r.norm1())


class WithProp:
    def __init__(self):
        self._v = 1
        self.plain = 2

    @property
    def v(self):
        return self._v * 100

    @v.setter
    def v(self, value):
        self._v = value


w = WithProp()
w.v = 3
print("property", w.v, w.plain, vars(w))


class Many:
    def __init__(self):
        for i in range(40):
            setattr(self, "a%d" % i, i)


m = Many()
print("many", len(vars(m)), m.a0, m.a39, sum(getattr(m, "a%d" % i) for i in range(40)))
m2 = Many()
print("many-again", list(vars(m2))[:3], list(vars(m2))[-2:])

dyn = Point(0, 0)
for name in ["alpha", "beta", "gamma"]:
    setattr(dyn, name, name.upper())
print("setattr", vars(dyn), getattr(dyn, "beta"), getattr(dyn, "nope", None))
dyn.__dict__.update({"delta": 4, "x": 99})
print("dict-update", dyn.delta, dyn.x, list(vars(dyn)))
dyn.__dict__.clear()
print("dict-clear", vars(dyn), hasattr(dyn, "x"), dyn.kind)
dyn.x = 1
print("after-clear", vars(dyn), dyn.x)

print("int-like names", end=" ")
o = Point(0, 0)
setattr(o, "1", "one")
o.__dict__["2"] = "two"
setattr(o, "0", "zero")
print(list(vars(o)), getattr(o, "1"), o.__dict__["2"])
