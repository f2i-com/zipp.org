# dataclasses: defaults, default_factory, ordering, frozen, post_init, inheritance,
# replace/asdict/astuple/fields, eq and repr generation.
from dataclasses import dataclass, field, replace, asdict, astuple, fields


@dataclass
class Item:
    name: str
    qty: int = 1
    tags: list = field(default_factory=list)

    def cost(self, unit):
        return self.qty * unit


i1 = Item("pen")
i2 = Item("pen")
i3 = Item("ink", 3, ["blue"])
i1.tags.append("x")
print("basic", i1, i2, i3, i1 == i2, i2 == Item("pen"), i3.cost(2.5), i2.tags is not Item("q").tags)


@dataclass(order=True)
class Version:
    major: int
    minor: int = 0
    patch: int = 0


vs = [Version(1, 2), Version(1, 10), Version(0, 9, 9), Version(1, 2, 1)]
print("order", sorted(vs), Version(1) < Version(1, 0, 1), max(vs), Version(2) >= Version(1, 99))


@dataclass(frozen=True)
class Coord:
    x: float
    y: float

    def dist2(self):
        return self.x ** 2 + self.y ** 2


c = Coord(3, 4)
try:
    c.x = 9
except Exception as e:
    print("frozen", type(e).__name__)
print("frozen-ok", c, c.dist2(), hash(c) == hash(Coord(3, 4)), {c: "v"}[Coord(3, 4)], replace(c, y=0))


@dataclass
class Rect:
    w: int
    h: int
    area: int = field(init=False)

    def __post_init__(self):
        self.area = self.w * self.h


print("post-init", Rect(2, 5), Rect(3, 3).area)


@dataclass
class Base:
    id: int
    label: str = "base"


@dataclass
class Derived(Base):
    extra: bool = False


dv = Derived(7, extra=True)
print("inherit", dv, [f.name for f in fields(Derived)], asdict(dv), astuple(dv), isinstance(dv, Base))


@dataclass
class Nested:
    items: list
    meta: dict = field(default_factory=dict)
    inner: Item = None


nd = Nested([Item("a"), Item("b", 2)], {"k": 1}, Item("c"))
print("asdict-nested", asdict(nd))
print("astuple-flat", astuple(Item("z", 5, [1, [2]])), astuple(Version(1, 2, 3)))


@dataclass(eq=False)
class Identity:
    v: int


print("eq-false", Identity(1) == Identity(1), Identity(1) != Identity(1))


@dataclass
class WithRepr:
    a: int
    secret: str = field(default="s", repr=False)
    cmp_ignored: int = field(default=0, compare=False)


print("field-options", WithRepr(1), WithRepr(1, "x", 5) == WithRepr(1, "x", 9), WithRepr(1, "x") == WithRepr(1, "y"))
inventory = [Item(f"i{k}", k % 4) for k in range(12)]
total = 0
for it in inventory:
    it.qty += 1
    total += it.cost(2)
print("workload", total, sorted({it.qty for it in inventory}), [it.name for it in inventory if it.qty == 1])
print("replace", replace(i3, qty=10), i3.qty, replace(Version(1, 2), patch=7))
