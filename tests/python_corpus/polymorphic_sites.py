# Attribute, property, method and call sites that meet several classes in
# turn (each class's answer kept beside the others'), in straight-line
# loops and in loops long enough to compile, while the classes change
# under them: every answer must be the one CPython gives.


class A:
    kind = "a"

    def __init__(self, v):
        self.v = v

    @property
    def p(self):
        return self.v

    @p.setter
    def p(self, x):
        self.v = x

    def m(self, d):
        return self.v + d


class B(A):
    kind = "b"

    @property
    def p(self):
        return self.v * 10

    def m(self, d):
        return self.v - d


class C:
    kind = "c"

    def __init__(self, v):
        self.w = 0
        self.v = v

    p = 7

    def m(self, d):
        return d


def f1(x):
    return x + 1


def f2(x):
    return x * 2


def f3(x):
    return -x


objs = [A(1), B(2), C(3), A(4)]
funcs = [f1, f2, f3]
total = 0
log = []
for i in range(6000):
    o = objs[i % 4]
    total += o.v + o.m(i % 5) + len(o.kind)
    total += o.p if not isinstance(o, C) else o.p * 3
    if i % 3 == 0 and type(o) is A:
        o.p = o.v + 1
    total += funcs[i % 3](i)
    if i == 1500:
        A.kind = "aa"
    if i == 2500:
        B.m = lambda self, d: 1000
    if i == 3000:
        del B.m
    if i == 3500:
        def m2(self, d):
            return self.v * d
        C.m = m2
    if i == 4000:
        objs[3].kind = "own"
    if i == 4500:
        del objs[3].kind
    if i == 5000:
        funcs[1] = f1
    if i % 1000 == 999:
        log.append((total, [o.v for o in objs], [o.kind for o in objs]))
print(total)
for entry in log:
    print(entry)

# Construction sites meeting several classes.
made = []
kinds = [A, B, C]
for i in range(3000):
    made.append(kinds[i % 3](i))
    if i == 1500:
        def init(self, v):
            self.v = -v
        A.__init__ = init
print(sum(o.v for o in made), [type(o).__name__ for o in made[:4]], made[1501].v, made[1503].v)

# Methods on builtin containers through one site.
seqs = [[1, 2], (3, 4), [5], (6,)]
c = 0
for i in range(3000):
    s = seqs[i % 4]
    c += s.count(i % 7) + s.index(s[0])
print(c)
