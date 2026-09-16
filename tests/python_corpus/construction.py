# Instance construction: __new__ and __init__ variants, singletons, alternative
# constructors, subclassing list/dict/tuple, metaclass __call__, type().
class Plain:
    pass


class WithInit:
    def __init__(self, a, b=2):
        self.a = a
        self.b = b


class WithNew:
    def __new__(cls, *args, **kwargs):
        inst = super().__new__(cls)
        inst.created_by = "new"
        inst.args = args
        return inst

    def __init__(self, x, y=0):
        self.x = x
        self.y = y


p = Plain()
w = WithInit(1)
n = WithNew(5, y=6)
print("basic", type(p).__name__, w.a, w.b, WithInit(b=3, a=4).b, n.created_by, n.args, n.x, n.y)


class Singleton:
    _instance = None
    inits = 0

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def __init__(self):
        Singleton.inits += 1


s1, s2 = Singleton(), Singleton()
print("singleton", s1 is s2, Singleton.inits)


class ReturnsOther:
    def __new__(cls):
        return 42

    def __init__(self):
        print("  ReturnsOther.__init__ must not run")


print("new-returns-foreign", ReturnsOther())


class Point:
    def __init__(self, x, y):
        self.x, self.y = x, y

    @classmethod
    def origin(cls):
        return cls(0, 0)

    @classmethod
    def from_pair(cls, pair):
        return cls(*pair)

    def __repr__(self):
        return f"{type(self).__name__}({self.x}, {self.y})"


class Point3(Point):
    def __init__(self, x, y, z=0):
        super().__init__(x, y)
        self.z = z


print("alt-constructors", Point.origin(), Point.from_pair((3, 4)), Point3.origin(), Point3.from_pair([1, 2]).z)
pts = [Point(i, -i) for i in range(4)]
print("bulk", pts, sum(q.x - q.y for q in pts))


class BadInit:
    def __init__(self):
        return 5


try:
    BadInit()
except TypeError as e:
    print("TypeError", e)


class MyList(list):
    def __init__(self, items=(), tag="t"):
        super().__init__(items)
        self.tag = tag

    def total(self):
        return sum(self)


ml = MyList([1, 2, 3], tag="nums")
ml.append(4)
print("list-sub", ml, ml.total(), ml.tag, len(ml), ml[1:3], type(ml[1:3]).__name__, type(ml + [5]).__name__, isinstance(ml, list))


class MyDict(dict):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.created = True

    def __missing__(self, key):
        self[key] = len(key)
        return self[key]


md = MyDict({"a": 1}, b=2)
print("dict-sub", md["a"], md["xyz"], sorted(md.items()), md.created, type(md.copy()).__name__)


class Pair(tuple):
    @property
    def first(self):
        return self[0]

    def swap(self):
        return Pair((self[1], self[0]))


pr = Pair((1, "x"))
print("tuple-sub", pr, pr.first, pr.swap(), len(pr), pr == (1, "x"), isinstance(pr, tuple), type(pr.swap()).__name__, hash(pr) == hash((1, "x")))
a, b = pr
print("tuple-sub-unpack", a, b, list(pr), pr + (2,), type(pr + (2,)).__name__)


class Node:
    count = 0

    def __init__(self, value, children=None):
        Node.count += 1
        self.value = value
        self.children = children if children is not None else []

    def add(self, child):
        self.children.append(child)
        return child

    def total(self):
        return self.value + sum(c.total() for c in self.children)


root = Node(1)
for i in range(3):
    kid = root.add(Node(i + 10))
    for j in range(2):
        kid.add(Node(j))
print("tree", root.total(), Node.count, len(root.children), [len(c.children) for c in root.children])


class Meta(type):
    def __call__(cls, *args, **kwargs):
        inst = super().__call__(*args, **kwargs)
        inst.via_meta = True
        return inst


class Managed(metaclass=Meta):
    def __init__(self, v):
        self.v = v


mg = Managed(3)
print("metaclass-call", mg.v, mg.via_meta)
T = type("T", (WithInit,), {"extra": lambda self: self.a * 10})
print("type-call", T(7).extra(), T.__name__, T.__bases__[0].__name__)
print("object-new", type(object.__new__(Plain)).__name__, isinstance(object(), object))
objs = [cls() for cls in (Plain, list, dict, set, tuple, str, int, float, bool)]
print("zero-arg-builtins", [type(o).__name__ for o in objs], objs[1:])
print("conversions", list("ab"), dict([(1, 2)]), set([1, 1]), tuple([1]), str(12), int("7"), float("1.5"), bool(0), frozenset([2, 2]) == {2})
