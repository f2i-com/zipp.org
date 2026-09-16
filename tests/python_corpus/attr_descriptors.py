# Properties with setters and deleters, data and non-data descriptors and their
# precedence against the instance dict, class-level access, descriptors in subclasses.
class Temperature:
    def __init__(self, c):
        self._c = c
        self.log = []

    @property
    def celsius(self):
        self.log.append("get")
        return self._c

    @celsius.setter
    def celsius(self, v):
        self.log.append("set")
        if v < -273.15:
            raise ValueError("below absolute zero")
        self._c = v

    @celsius.deleter
    def celsius(self):
        self.log.append("del")
        self._c = None

    @property
    def fahrenheit(self):
        return self._c * 9 / 5 + 32


t = Temperature(20)
t.celsius = 25
t.celsius += 5
print("property", t.celsius, t.fahrenheit, t.log)
del t.celsius
print("deleter", t._c, t.log[-1])
try:
    t.celsius = -300
except ValueError as e:
    print("ValueError", e)
try:
    t.fahrenheit = 1
except AttributeError as e:
    print("AttributeError", e)
t.__dict__["celsius"] = "in-dict"
t._c = 1
print("data-descriptor-wins", t.celsius, t.__dict__["celsius"])
print("class-access", type(Temperature.celsius).__name__, Temperature.celsius.fget.__name__)


class Verbose:
    def __init__(self, name):
        self.name = name

    def __get__(self, obj, owner):
        if obj is None:
            return f"<{self.name} on class {owner.__name__}>"
        return f"{self.name} read from {type(obj).__name__}"

    def __set__(self, obj, value):
        obj.__dict__["_" + self.name] = value * 2


class NonData:
    def __get__(self, obj, owner):
        return "non-data" if obj is not None else "non-data-class"


class Host:
    v = Verbose("v")
    n = NonData()


h = Host()
print("data", h.v, Host.v)
h.v = 21
print("data-set", h._v, h.v, sorted(h.__dict__))
print("nondata", h.n, Host.n)
h.n = "instance wins"
print("nondata-shadow", h.n, Host().n)
del h.n
print("nondata-unshadow", h.n)


class Sub(Host):
    pass


s = Sub()
s.v = 5
print("inherited-descriptor", s.v, s._v, Sub.v)
Host.v = "plain now"
print("descriptor-replaced", s.v, Sub.v)


class Typed:
    def __set_name__(self, owner, name):
        self.public = name
        self.private = "_" + name

    def __get__(self, obj, owner=None):
        if obj is None:
            return self
        return getattr(obj, self.private, 0)

    def __set__(self, obj, value):
        if not isinstance(value, int):
            raise TypeError(f"{self.public} must be int, got {type(value).__name__}")
        setattr(obj, self.private, value)


class Record:
    x = Typed()
    y = Typed()

    def __init__(self, x, y):
        self.x = x
        self.y = y


r = Record(3, 4)
r.x += 10
print("typed", r.x, r.y, Record.x.public, Record.y.private)
try:
    r.y = "no"
except TypeError as e:
    print("TypeError", e)
rs = [Record(i, -i) for i in range(5)]
print("typed-loop", sum(q.x * q.y for q in rs), [q.x for q in rs])


class Lazy:
    def __init__(self, fn):
        self.fn = fn
        self.name = fn.__name__

    def __get__(self, obj, owner):
        if obj is None:
            return self
        value = self.fn(obj)
        obj.__dict__[self.name] = value
        return value


class Heavy:
    computed = 0

    @Lazy
    def expensive(self):
        Heavy.computed += 1
        return [1, 2, 3]


hv = Heavy()
print("lazy", hv.expensive, hv.expensive, Heavy.computed, "expensive" in hv.__dict__)


class Account:
    def __init__(self):
        self._balance = 0

    def _get(self):
        return self._balance

    def _set(self, v):
        self._balance = max(0, v)

    balance = property(_get, _set, None, "the balance")


acct = Account()
acct.balance = 50
acct.balance -= 80
print("property()", acct.balance, Account.balance.__doc__)


class Base:
    @property
    def value(self):
        return "base"


class Derived(Base):
    @property
    def value(self):
        return "derived+" + super().value


print("property-super", Derived().value, Base().value)


class Meth:
    def plain(self):
        return "plain"

    @staticmethod
    def st():
        return "static"

    @classmethod
    def cm(cls):
        return cls.__name__


m = Meth()
print("function-descriptors", Meth.plain(m), m.plain(), Meth.st(), m.st(), Meth.cm(), m.cm())
m.plain = lambda: "instance function"
print("function-shadow", m.plain(), Meth().plain())
print("dict-entries", type(Meth.__dict__["cm"]).__name__, type(Meth.__dict__["st"]).__name__, Meth.__dict__["st"] is not Meth.st)
