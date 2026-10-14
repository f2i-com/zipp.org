# Method calls on lists, tuples, dicts and user instances from loops long
# enough to compile, while the receivers, their classes and their
# attributes change under the call sites: every call must reach the method
# CPython reaches.


class Counter:
    def __init__(self):
        self.n = 0

    def bump(self, d):
        self.n += d
        return self.n

    def name(self):
        return "Counter"


class Loud(Counter):
    def bump(self, d):
        self.n += d * 10
        return self.n


class MyList(list):
    def append(self, x):
        list.append(self, x * 2)


def replaced_bump(self, d):
    self.n -= d
    return -self.n


# Builtin receivers: list.append / pop, dict.get / keys, tuple.count / index.
out = []
d = {"a": 1, "b": 2}
t = (1, 2, 2, 3)
acc = 0
for i in range(3000):
    out.append(i)
    if i % 3 == 0:
        out.pop()
    acc += d.get("a", 0) + d.get("zz", 5) + len(d.keys()) + t.count(2) + t.index(3)
print(len(out), out[:5], out[-3:], acc)

# One site meeting a list, a list subclass and a tuple-less fallback.
targets = [[], MyList(), []]
for i in range(3000):
    targets[i % 3].append(i)
print([len(x) for x in targets], targets[1][:4], sum(targets[2]))

# A user class's method: the class's method replaced, then restored, an
# instance attribute shadowing it, a subclass's override at the same site.
c, loud = Counter(), Loud()
total = 0
for i in range(4000):
    if i == 1000:
        Counter.bump = replaced_bump
    if i == 2000:
        del Counter.bump
        Counter.bump = lambda self, d: self.n
    if i == 2500:
        c.bump = lambda d: 1000
    if i == 3000:
        del c.bump
    who = loud if i % 7 == 0 else c
    total += who.bump(1)
print(total, c.n, loud.n, c.name(), loud.name())

# The method found through the instance's own storage changing layout.
class Bag:
    def __init__(self):
        self.items = []

    def add(self, x):
        self.items.append(x)
        return len(self.items)


bags = [Bag() for _ in range(4)]
n = 0
for i in range(3000):
    b = bags[i % 4]
    if i == 1500:
        bags[1].extra = 1
        bags[2].__dict__["add"] = lambda x: -1
    n += b.add(i)
print(n, [len(b.items) for b in bags])

# A dict's method name shadowed by nothing: dict subclasses and dicts.
class D(dict):
    def get(self, k, default=None):
        return "sub"


ds = [{"k": 1}, D(k=2)]
r = []
for i in range(3000):
    r.append(ds[i % 2].get("k", 0))
print(r[:4], r.count("sub"))
