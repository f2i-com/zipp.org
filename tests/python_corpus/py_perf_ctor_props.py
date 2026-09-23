# Class construction through the positional entries and property accessors
# through the inline caches: warm sites, then the changes that must take
# them off the fast path.


def make(cls, *args):
    r = None
    for _ in range(4):
        r = cls(*args)
    return r


class P:
    def __init__(self, x, y=0):
        self.x = x
        self.y = y


p = make(P, 1, 2)
q = make(P, 3)
print(p.x, p.y, q.x, q.y)
for bad in [(), (1, 2, 3)]:
    try:
        P(*bad)
    except TypeError as e:
        print("TypeError", e)


def new_init(self, *args):
    self.args = args


P.__init__ = new_init
r = make(P, 1, 2, 3)
print(r.args, hasattr(r, "x"))
del P.__init__
try:
    P(1)
except TypeError as e:
    print("TypeError", e)
print(type(make(P)).__name__)


class Base:
    def __init__(self, v):
        self.v = v


class Child(Base):
    pass


c = make(Child, 5)
print(c.v)
Base.__init__ = lambda self, v: setattr(self, "v", v * 100)
print(make(Child, 5).v)


class ReturnsValue:
    def __init__(self):
        return None


make(ReturnsValue)


class Bad:
    def __init__(self):
        return 1


try:
    Bad()
except TypeError as e:
    print("TypeError", e)


class WithNew:
    def __init__(self, v):
        self.v = v


w = make(WithNew, 1)


def custom_new(cls, v):
    obj = object.__new__(cls)
    obj.made_by_new = True
    return obj


WithNew.__new__ = staticmethod(custom_new)
w2 = WithNew(2)
print(w2.v, getattr(w2, "made_by_new", False))


class Meta(type):
    def __call__(cls, *args):
        return ("meta call", args)


class M(metaclass=Meta):
    def __init__(self, a):
        self.a = a


print(make(M, 1))

import abc


class Abstract(abc.ABC):
    @abc.abstractmethod
    def f(self):
        pass


class Concrete(Abstract):
    def f(self):
        return 1


print(make(Concrete).f())
try:
    Abstract()
except TypeError as e:
    print("TypeError", e)


class Slotted:
    __slots__ = ("a",)

    def __init__(self, a):
        self.a = a


print(make(Slotted, 4).a)


class ListSub(list):
    def __init__(self, n):
        super().__init__(range(n))


print(make(ListSub, 3))


class NoInit:
    pass


print(type(make(NoInit)).__name__)
try:
    NoInit(1)
except TypeError as e:
    print("TypeError", e)
import dataclasses


@dataclasses.dataclass
class DC:
    a: int
    b: str = "x"


print(make(DC, 1), make(DC, 2, "y"), DC(a=3))


class KW:
    def __init__(self, a, *, b=2):
        self.t = (a, b)


print(make(KW, 1).t, KW(1, b=5).t)


# ---- properties ----------------------------------------------------------------
class Account:
    def __init__(self, cents):
        self._cents = cents
        self.log = []

    @property
    def balance(self):
        return self._cents

    @balance.setter
    def balance(self, value):
        if value < 0:
            raise ValueError("negative")
        self.log.append(value)
        self._cents = value


def spend(acct, n):
    for _ in range(3):
        acct.balance = acct.balance - n
    return acct.balance


a = Account(100)
print(spend(a, 10), a.log)
try:
    spend(a, 50)
except ValueError as e:
    print("ValueError", e, a.balance)
Account.balance = property(lambda self: -1)
print(a.balance)
try:
    a.balance = 5
except AttributeError:
    print("AttributeError no setter")
Account.balance = 42
print(a.balance)
a.balance = 7
print(a.balance, Account.balance)


class Defaults:
    def getter(self, extra=1):
        return self.__dict__.get("v", 0) + extra

    v = property(getter)


d = Defaults()
print(d.v, d.v)


class Lazy:
    @property
    def value(self):
        return "computed"


lz = Lazy()
for _ in range(3):
    got = lz.value
print(got)
try:
    lz.value = 1
except AttributeError:
    print("AttributeError read-only")


class Chained(Lazy):
    pass


ch = Chained()
print(ch.value)
Lazy.value = property(lambda self: "replaced")
print(ch.value, lz.value)
