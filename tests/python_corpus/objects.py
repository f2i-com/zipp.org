import itertools, functools, collections, math, json, re, string, operator, abc, enum, dataclasses, typing

def section(name, fn):
    try:
        fn()
    except Exception as e:
        print(f"[{name}] {type(e).__name__}: {e}")

def s_classes():
    class Meta(type):
        def __new__(mcls, name, bases, ns, **kw):
            ns["tag"] = name.lower()
            return super().__new__(mcls, name, bases, ns)
        def __call__(cls, *a, **k):
            obj = super().__call__(*a, **k)
            obj.made = True
            return obj
    class A(metaclass=Meta):
        def __init__(self, v): self.v = v
    a = A(3)
    print(A.tag, a.made, a.v, type(A).__name__, isinstance(A, Meta))
    class Base:
        registry = []
        def __init_subclass__(cls, key=None, **kw):
            super().__init_subclass__(**kw)
            Base.registry.append((cls.__name__, key))
    class C1(Base, key="x"): pass
    class C2(Base): pass
    print(Base.registry)
    class Desc:
        def __set_name__(self, owner, name): self.name = "_" + name
        def __get__(self, obj, owner=None): return obj.__dict__.get(self.name, "unset") if obj is not None else self
        def __set__(self, obj, value): obj.__dict__[self.name] = value * 2
    class D:
        x = Desc()
    d = D(); print(d.x); d.x = 4; print(d.x, D.x.name)
    class S:
        __slots__ = ("a", "b")
        def __init__(self): self.a = 1
    s = S(); s.b = 2; print(s.a, s.b)
    try:
        s.c = 3
    except AttributeError as e:
        print("slots:", e)
    class G:
        def __class_getitem__(cls, item): return f"G[{item}]"
    print(G[int])
    class V:
        def __init__(self, x): self.x = x
        def __add__(self, o): return V(self.x + (o.x if isinstance(o, V) else o))
        def __radd__(self, o): return V(o + self.x)
        def __mul__(self, o): return V(self.x * o)
        __rmul__ = __mul__
        def __neg__(self): return V(-self.x)
        def __abs__(self): return abs(self.x)
        def __bool__(self): return self.x != 0
        def __repr__(self): return f"V({self.x})"
        def __eq__(self, o): return isinstance(o, V) and o.x == self.x
        def __lt__(self, o): return self.x < o.x
        def __hash__(self): return hash(self.x)
        def __getitem__(self, i): return self.x * i
        def __contains__(self, i): return i == self.x
        def __len__(self): return 3
        def __iter__(self): return iter([self.x] * 2)
        def __call__(self, y): return self.x + y
        def __format__(self, spec): return f"<{self.x:{spec}}>"
        def __index__(self): return self.x
        def __int__(self): return self.x
        def __float__(self): return float(self.x)
    v = V(2)
    print(v + 1, 1 + v, 3 * v, -v, abs(V(-5)), bool(V(0)), v == V(2), sorted([V(3), V(1)]), {v, V(2)}, v[4], 2 in v, len(v), list(v), v(5), f"{v:>4}", [1, 2, 3][v], int(v), float(v), sum([V(1), V(2)], V(0)))
    print(max(V(1), V(4)), min([V(9), V(4)]), V(1) < V(2), V(2) > V(1), V(2) >= V(2), V(1) != V(2))
    class P:
        def __init__(self): self._x = 0
        @property
        def x(self): return self._x
        @x.setter
        def x(self, v): self._x = v + 1
        @x.deleter
        def x(self): self._x = None
        @staticmethod
        def s(a): return a * 2
        @classmethod
        def c(cls): return cls.__name__
    p = P(); p.x = 5; print(p.x, P.s(3), P.c(), p.c()); del p.x; print(p.x)
    class Dyn:
        def __getattr__(self, name): return name.upper()
        def __setattr__(self, name, value): object.__setattr__(self, name, value * 10)
        def __delattr__(self, name): print("del", name); object.__delattr__(self, name)
    dy = Dyn(); dy.q = 1; print(dy.q, dy.zz); del dy.q; print(hasattr(dy, "q"), getattr(dy, "q", "dflt"))
    class M1:
        def who(self): return "M1"
    class M2:
        def who(self): return "M2" + super().who()
    class M3(M2, M1): pass
    print(M3().who(), [c.__name__ for c in M3.__mro__])
    print(type("Dynamic", (M1,), {"z": 5})().who(), type("Dynamic", (M1,), {"z": 5}).z)
    class AB(abc.ABC):
        @abc.abstractmethod
        def run(self): ...
    try:
        AB()
    except TypeError as e:
        print("abc:", e)
    class Impl(AB):
        def run(self): return "ran"
    print(Impl().run())
    print(vars(a), a.__dict__, A.__name__, A.__qualname__, a.__class__.__name__, A.__mro__[-1].__name__)
section("classes", s_classes)
