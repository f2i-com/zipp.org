# dataclasses: ClassVar annotations are not fields, InitVar parameters go to
# __post_init__, unhashable defaults and non-default-after-default fields
# are rejected. enum: aliases in __members__, @unique, iterating a Flag
# class, class membership, and fixed members.
from dataclasses import dataclass, field, fields, InitVar, asdict
from typing import ClassVar
import enum


@dataclass
class C:
    x: int
    k: ClassVar[int] = 5
    j: "ClassVar[str]" = "s"
    tags: list = field(default_factory=list)


print("classvar", C(1), [f.name for f in fields(C)], C.k, C.j, asdict(C(2, ["t"])))


@dataclass
class Scaled:
    base: int
    factor: InitVar[int] = 2
    total: int = 0

    def __post_init__(self, factor):
        self.total = self.base * factor


print("initvar", Scaled(3), Scaled(3, 5), Scaled(base=4, factor=10).total, [f.name for f in fields(Scaled)])


def make_mutable():
    @dataclass
    class D:
        x: list = []
    return D


def make_order():
    @dataclass
    class D:
        x: int = 1
        y: int
    return D


def make_ok():
    @dataclass
    class D:
        x: dict = field(default_factory=dict)
        y: set = None
        z: tuple = (1, 2)
    return D()


for thunk in (make_mutable, make_order, make_ok):
    try:
        print("dataclass-check", thunk())
    except (ValueError, TypeError) as e:
        print("dataclass-check", type(e).__name__, e)


class Color(enum.Enum):
    RED = 1
    GREEN = 2
    CRIMSON = 1


print("aliases", list(Color.__members__), list(Color), Color.CRIMSON is Color.RED, Color(1), Color["CRIMSON"], len(Color), Color.RED in Color)


def make_unique():
    @enum.unique
    class E(enum.Enum):
        A = 1
        B = 1
        C = 2
        D = 2
    return E


try:
    make_unique()
except ValueError as e:
    print("unique", e)


@enum.unique
class Fine(enum.Enum):
    A = 1
    B = 2


print("unique-ok", list(Fine))


class Perm(enum.Flag):
    R = 4
    W = 2
    X = 1


print("flag", list(Perm), len(Perm), Perm.R | Perm.W, list(Perm.R | Perm.W), Perm.R in (Perm.R | Perm.X), Perm.W in Perm, bool(Perm.R & Perm.X))
try:
    Color.RED = 5
except AttributeError as e:
    print("reassign", e)
Color.extra = "ok"
print("non-member-attr", Color.extra)


# A user __init__ receives each member's value (a tuple is unpacked).
class Planet(enum.Enum):
    MERCURY = (3.303e+23, 2.4397e6)
    EARTH = (5.976e+24, 6.37814e6)

    def __init__(self, mass, radius):
        self.mass = mass
        self.radius = radius

    @property
    def surface_gravity(self):
        return 6.673e-11 * self.mass / (self.radius * self.radius)


class Single(enum.Enum):
    A = 1
    B = 2

    def __init__(self, v):
        self.double = v * 2


print("member-init", Planet.EARTH.mass, Planet.EARTH.radius, Planet.EARTH.value, Planet((3.303e+23, 2.4397e6)), round(Planet.EARTH.surface_gravity, 3), Single.A.double, Single(2).double)

# The functional API.
E = enum.Enum("E", "X Y")
print("functional", list(E), E.X.value, E(2), E["Y"], repr(E.X), E.__name__)
print("functional-forms", list(enum.Enum("F", "A, B, C")), list(enum.Enum("G", ["P", "Q"])), list(enum.Enum("H", [("ONE", 1), ("TEN", 10)])), enum.Enum("K", {"a": "x", "b": "y"})("y"))
L = enum.IntFlag("L", "R W X")
print("functional-flag", list(L), L.R | L.X, int(L.W))


# auto(): one past the largest value, or the next power of two for a Flag.
class Auto(enum.Enum):
    A = 5
    B = enum.auto()
    C = 2
    D = enum.auto()


class AutoFlag(enum.Flag):
    A = enum.auto()
    B = 8
    C = enum.auto()


print("auto", [m.value for m in Auto], [m.value for m in AutoFlag])


# Flag and IntFlag pseudo-members: unnamed bits, zero, invalid values.
class IP(enum.IntFlag):
    A = 1
    B = 2
    C = 8


print("intflag-names", repr(IP.A | 4), repr(IP.A | IP.B | 16), repr(IP(20)), str(IP.A | 4), repr(IP.C | IP.A), repr(IP(0)), IP(3))
print("flag-names", repr(Perm.X | Perm.R), repr(Perm(0)), str(Perm(0)), repr(~Perm.R), repr(Perm(6)), bool(Perm(0)))
for thunk in (lambda: Perm(8), lambda: E(3)):
    try:
        thunk()
    except ValueError as e:
        print("ValueError", e)
