# Property reads and writes at sites that run many times, while the class
# changes under them: the getter or setter is replaced, the property becomes
# a plain attribute, is deleted, comes back; a subclass overrides it; the
# accessor is a function with defaults, with *args, a lambda, a generator
# function, a bound method, a builtin. Every site must go on answering what
# the class says at that moment.


class Shape:
    def __init__(self, w, h):
        self._w = w
        self._h = h
        self.log = []

    @property
    def size(self):
        return self._w * self._h

    @property
    def width(self):
        return self._w

    @width.setter
    def width(self, value):
        self.log.append(value)
        self._w = value


def read_size(shapes, rounds):
    total = 0
    for _ in range(rounds):
        for s in shapes:
            total += s.size
    return total


def write_width(shapes, rounds):
    for r in range(rounds):
        for s in shapes:
            s.width = r
    return [s.width for s in shapes]


def bump_width(shape, rounds):
    for _ in range(rounds):
        shape.width += 2
    return shape.width


shapes = [Shape(2, 3), Shape(4, 5)]
print("== hot reads and writes")
print(read_size(shapes, 300), write_width(shapes, 200), len(shapes[0].log))
print(bump_width(shapes[0], 150), shapes[0].log[-3:])

print("== the getter is replaced")
Shape.size = property(lambda self: self._w + self._h)
print(read_size(shapes, 3))
print("== the property becomes a plain class attribute")
Shape.size = 7
print(read_size(shapes, 3))
shapes[0].size = 100
print(read_size(shapes, 3), shapes[0].__dict__.get("size"))
del shapes[0].size
print("== and a property again: the instance value is shadowed")
shapes[1].__dict__["size"] = -1
Shape.size = property(lambda self: self._h)
print(read_size(shapes, 3), shapes[1].__dict__["size"])
print("== deleted from the class")
del Shape.size
try:
    print(read_size(shapes, 1))
except AttributeError as e:
    print("AttributeError", e)
print(shapes[1].size)
del shapes[1].__dict__["size"]


def area(self):
    return self._w * self._h


Shape.size = property(area)
print("== back", read_size(shapes, 2))

print("== the setter is replaced, removed, restored")


def loud(self, value):
    self.log.append(("loud", value))
    self._w = value * 10


Shape.width = Shape.width.setter(loud)
print(write_width(shapes, 3), shapes[0].log[-1])
Shape.width = property(Shape.width.fget)
try:
    write_width(shapes, 1)
except AttributeError as e:
    print("AttributeError", e)
Shape.width = Shape.width.setter(lambda self, value: setattr(self, "_w", value + 1))
print(write_width(shapes, 3), bump_width(shapes[1], 4))

print("== a subclass overrides the property; one site sees both classes")


class Square(Shape):
    def __init__(self, side):
        Shape.__init__(self, side, side)

    @property
    def size(self):
        return -Shape.size.fget(self)

    @property
    def width(self):
        return "sq%d" % self._w

    @width.setter
    def width(self, value):
        self._w = value + 100


class Plain(Shape):
    pass


mixed = [Shape(2, 3), Square(3), Plain(1, 9), Square(2)]
print(read_size(mixed, 200), write_width(mixed, 50))
Square.size = property(lambda self: 1000)
print(read_size(mixed, 2))
del Square.size
print(read_size(mixed, 2))
Plain.size = 0.5
print(read_size(mixed, 2))

print("== accessors of other kinds")


def with_default(self, scale=3):
    return self._w * scale


def with_star(self, *rest):
    return (self._w, rest)


def as_generator(self):
    yield self._w
    yield self._h


class Bound:
    def __init__(self, tag):
        self.tag = tag

    def get(self, obj):
        return (self.tag, obj._w)

    def put(self, obj, value):
        obj._w = (self.tag, value)


class Kinds:
    def __init__(self):
        self._w = 5
        self._h = 6

    scaled = property(with_default)
    starred = property(with_star)
    lazy = property(as_generator)
    bound = property(Bound("b").get, Bound("b").put)
    typed = property(lambda self: str(self._h))
    count = property(len)
    stored = property(lambda self: self._w, lambda self, value, extra=4: setattr(self, "_w", value * extra))

    def __len__(self):
        return 42


def read_kinds(k, rounds):
    out = None
    for _ in range(rounds):
        out = (k.scaled, k.starred, list(k.lazy), k.bound, k.typed, k.count)
    return out


k = Kinds()
print(read_kinds(k, 120))
for _ in range(120):
    k.bound = 1
    k.stored = 2
print(k.bound, k.stored, k._w)

print("== an accessor that raises, at a hot site")


class Fragile:
    def __init__(self):
        self.calls = 0

    @property
    def value(self):
        self.calls += 1
        if self.calls % 50 == 0:
            raise ValueError("call %d" % self.calls)
        return self.calls

    @value.setter
    def value(self, v):
        if v < 0:
            raise ValueError("negative")
        self.calls = v


f = Fragile()
caught = []
total = 0
for i in range(175):
    try:
        total += f.value
    except ValueError as e:
        caught.append(str(e))
print(total, caught)
for v in (5, -1, 7):
    try:
        f.value = v
    except ValueError as e:
        print("ValueError", e)
print(f.calls)

print("== accessors that read and write other properties, and recurse")


class Chain:
    def __init__(self, depth):
        self.depth = depth
        self.below = Chain(depth - 1) if depth else None

    @property
    def total(self):
        return self.depth + (self.below.total if self.below else 0)

    @property
    def twice(self):
        return self.total * 2

    @twice.setter
    def twice(self, value):
        if self.below:
            self.below.twice = value
        self.depth = value


c = Chain(6)
print(c.total, c.twice, [c.twice for _ in range(3)])
for v in range(120):
    c.twice = v % 3
print(c.total, c.below.below.depth)

print("== a generator reading a property between yields")


def sizes(shapes):
    for s in shapes:
        yield s.size
        s.width = 1
        yield s.size


print(list(sizes([Shape(2, 3), Shape(4, 5)])))

print("== the getter changes the class while its site is hot")


class Morph:
    def __init__(self):
        self.reads = 0

    @property
    def state(self):
        self.reads += 1
        if self.reads == 150:
            Morph.state = property(lambda self: "second")
        return "first"


m = Morph()
seen = []
for _ in range(300):
    v = m.state
    if not seen or seen[-1] != v:
        seen.append(v)
print(seen, m.reads)

print("== a class attribute and a method at sites that also see properties")


class Tagged:
    kind = "tag"

    def __init__(self, n):
        self.n = n

    def describe(self):
        return "n%d" % self.n


class Computed(Tagged):
    @property
    def kind(self):
        return "computed%d" % self.n

    @property
    def n2(self):
        return self.n * 2


items = [Tagged(1), Computed(2), Tagged(3), Computed(4)]
print([x.kind for _ in range(60) for x in items][-4:], [x.describe() for x in items])
print([getattr(x, "n2", None) for x in items])

print("== accessors that are hot loops calling the same accessor, 180 deep")


class Node:
    def __init__(self, child, tag):
        self.child = child
        self.tag = tag
        self._seen = 0

    @property
    def depth(self):
        total = 0
        for i in range(2):
            if i == 0:
                total = 1 + (self.child.depth if self.child is not None else 0)
        return total

    @property
    def seen(self):
        return self._seen

    @seen.setter
    def seen(self, value):
        for i in range(2):
            if i == 0 and self.child is not None:
                self.child.seen = value + 1
        self._seen = value

    @property
    def fragile(self):
        for i in range(2):
            if i == 0:
                if self.child is None:
                    raise KeyError(self.tag)
                return self.child.fragile
        return None


node = None
for k in range(180):
    node = Node(node, k)
out = []
for r in range(260):
    out.append(node.depth)
print(out[0], out[-1], len(set(out)))
for r in range(260):
    node.seen = r
tail = node
count = 0
while tail.child is not None:
    tail = tail.child
    count += 1
print(node.seen, tail.seen, count)
caught = 0
for r in range(260):
    try:
        node.fragile
    except KeyError as e:
        caught += e.args[0] + 1
print(caught)

print("== one read and one write site, three classes, the class changing mid-loop")


class Wide:
    def __init__(self, n):
        self.n = n

    @property
    def value(self):
        return self.n

    @value.setter
    def value(self, v):
        self.n = v


class WideA(Wide):
    pass


class WideB(Wide):
    @property
    def value(self):
        return -self.n

    @value.setter
    def value(self, v):
        self.n = v * 2


def churn(items, rounds):
    total = 0
    for r in range(rounds):
        for it in items:
            it.value = it.value + 1
            total += it.value
        if r == rounds // 2:
            WideA.value = property(lambda self: 1000)
        if r == rounds // 2 + 5:
            del WideA.value
    return total


items = [Wide(1), WideA(2), WideB(3), Wide(4), WideB(5), WideA(6)]
try:
    print(churn(items, 400))
except AttributeError:
    print("AttributeError: the replaced property has no setter")
print([it.n for it in items])
print(churn([Wide(1), WideB(2)], 300), churn([WideB(7)], 300))
