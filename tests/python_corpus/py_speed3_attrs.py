# Attribute reads, writes and method calls at one site over many instances:
# instances whose dicts hold the same names in different orders, attributes
# deleted and re-added (a dict entry left behind as a hole), a site that sees
# instances of several classes, a data descriptor or __setattr__ added to a
# class after instances exist, an instance attribute shadowing a method, an
# instance __dict__ replaced, class attributes read through instances, and
# builtin methods (list/dict/str) called at one site on different receivers.


class P:
    def __init__(self, a, b):
        self.a = a
        self.b = b

    def total(self):
        return self.a + self.b


class Q:
    def __init__(self, a, b):
        self.b = b
        self.a = a

    def total(self):
        return self.a * self.b


def read_all(objs):
    out = []
    for o in objs:
        out.append((o.a, o.b, o.total()))
    return out


def bump_all(objs, n):
    for o in objs:
        o.a = o.a + n
        o.b += n


objs = [P(i, i + 1) if i % 3 else Q(i, 2) for i in range(12)]
print(read_all(objs))
bump_all(objs, 10)
print(read_all(objs))
# Delete and re-add: the re-added name goes to the end of the dict.
for o in objs[:4]:
    del o.a
    o.c = "c"
    o.a = -1
print(read_all(objs), [sorted(vars(o)) for o in objs[:4]])
try:
    del objs[5].a
    objs[5].a
except AttributeError as e:
    print("AttributeError:", e)
objs[5].a = 5
print(objs[5].a, objs[5].total())
# An attribute shadowing a method on one instance only.
objs[6].total = lambda: "shadowed"
print([o.total() for o in objs[5:8]])
del objs[6].total
print(objs[6].total())


class Watched:
    def __init__(self):
        self.x = 1

    def get(self):
        return self.x


ws = [Watched() for _ in range(3)]
print([w.get() for w in ws], [w.x for w in ws])
log = []


def setattr_hook(self, name, value):
    log.append(name)
    object.__setattr__(self, name, value * 10)


Watched.__setattr__ = setattr_hook
for w in ws:
    w.x = 2
print([w.x for w in ws], log)
del Watched.__setattr__
for w in ws:
    w.x = 3
print([w.x for w in ws])
Watched.x = property(lambda self: "prop")
print([w.x for w in ws], [w.get() for w in ws])
del Watched.x
print([w.x for w in ws])
w = ws[0]
w.__dict__ = {"x": "replaced", "y": 7}
print(w.x, w.y, w.get())


class Defaults:
    kind = "default"
    size = 3

    def __init__(self, i):
        if i % 2:
            self.kind = "own%d" % i


ds = [Defaults(i) for i in range(6)]
print([d.kind for d in ds], [d.size for d in ds])
Defaults.kind = "changed"
print([d.kind for d in ds])
receivers = [[1, 2], [3], [], [4, 5, 6]]
for r in receivers:
    r.append(len(r))
print(receivers, [r.count(1) for r in receivers], [r.index(len(r) - 1) for r in receivers])
dicts = [{"a": 1}, {"b": 2}, {}]
print([d.get("a", 0) for d in dicts], [d.get("b") for d in dicts], [sorted(d.keys()) for d in dicts])
strs = ["Ab", "cD", ""]
print([s.lower() for s in strs], [s.upper() for s in strs], [s.startswith("A") for s in strs])
mixed = [[1], "x", {"k": 1}]
print([m.__len__() for m in mixed], [type(m).__name__ for m in mixed])


class Slotted:
    __slots__ = ("u", "v")

    def __init__(self, u):
        self.u = u
        self.v = u * 2


ss = [Slotted(i) for i in range(3)]
for s in ss:
    s.u += 1
print([(s.u, s.v) for s in ss])
try:
    ss[0].w = 1
except AttributeError as e:
    print("AttributeError:", e)
many = [P(i, -i) for i in range(200)]
acc = 0
for m in many:
    acc += m.a - m.b + m.total()
print(acc)


class Wide:
    def m0(self): return 0
    def m1(self): return 1
    def m2(self): return 2
    def m3(self): return 3
    def m4(self): return 4
    def m5(self): return 5
    def m6(self): return 6
    def m7(self): return 7
    def m8(self): return 8
    def m9(self): return 9
    def m10(self): return 10
    def m11(self): return 11


w = Wide()
calls = [w.m0(), w.m1(), w.m2(), w.m3(), w.m4(), w.m5(), w.m6(), w.m7(), w.m8(), w.m9(), w.m10(), w.m11()]
acc = 0
for i in range(100):
    acc += w.m11() + w.m3()
w.m11 = lambda: 100
print(calls, acc, w.m11(), w.m3())
del w.m11
print(w.m11())
Wide.m3 = lambda self: 33
print(w.m3(), Wide().m3())
