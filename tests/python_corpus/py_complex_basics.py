# complex: literals, repr/str, the constructor (numbers, strings, keywords,
# __complex__/__float__/__index__), attributes, equality with int/float,
# hashing and dict/set keys, subclasses, copy, pickle and json.
import copy
import json
import pickle

inf = float("inf")
nan = float("nan")

print("-- literals and repr")
for z in [1j, 2.5J, 1e3j, 0j, -0j, -1j, 1 + 2j, 1 - 2j, -1 - 2j, 1.5 + 0j, -0.0 + 1j, 0.0 - 0j,
          1e16j, 1e-5j, 123456789012345678j, 1e300 * 1e300j, 3.14159j, 0.1 + 0.2j, 1e-310j]:
    print(repr(z), str(z), type(z).__name__)
for re in [0.0, -0.0, 1.0, -1.0, 1e16, 1.5e-7, inf, -inf, nan, 100.0, 1e22, 2.5]:
    for im in [0.0, -0.0, 1.0, -2.5, inf, -inf, nan, 1e16, 1e-5]:
        print(complex(re, im), end=" ")
    print()

print("-- constructor")
print(complex(), complex(0), complex(3), complex(3.5), complex(True), complex(False), complex(-0.0))
print(complex(1, 2), complex(1.5, -2), complex(real=2), complex(imag=3), complex(real=1, imag=-1), complex(2, imag=5))
print(complex(1j), complex(1j, 1j), complex(1 + 2j, 3 + 4j), complex(2, 1j), complex(1j, 2), complex(-0.0, -0.0))
z = 5 + 6j
print(complex(z) is z, complex(z, 0) is z)
print(complex(10 ** 20), complex(2 ** 53 + 1), complex(-(10 ** 15), 10 ** 15))
for s in ["1", "1j", "-1j", "+1.5J", "1+2j", "1-2j", "  3.5  ", "(1+2j)", " ( 1+2j ) ", "\t-4.25e2+1e-2j\n", "j", "-j", "+J",
          "1+j", "2-J", "inf", "-infj", "infinity+nanj", "nan", "NaN-InFj", "1e5", "1E5j", ".5", "5.", ".5j", "1_000+2_0j",
          "1_0.5_0e1_0j", "0x10", "1e", "1+", "(1+2j", "1+2j)", "()", "", " ", "1+2", "1 + 2j", "1+2jj", "j1", "1_", "_1",
          "1__0", "1._5", "(j)", "--1", "+-1j", "1e+j", "infj+1", "\u0661\u0662+\u0663j", "1\u00a0", "\u20001j\u3000", "1\u200b"]:
    try:
        print(repr(s), "->", complex(s))
    except (ValueError, TypeError) as e:
        print(repr(s), "!!", type(e).__name__, e)
for args, kwargs in [((1, 2, 3), {}), ((), {"foo": 1}), ((1,), {"real": 2}), ((1, 2), {"imag": 2}), (("1", 2), {}), ((1, "2"), {}),
                     (([],), {}), ((1, []), {}), ((b"1",), {}), ((None,), {}), ((1, None), {}), (("1",), {"imag": 1}),
                     ((10 ** 400,), {}), ((1, 10 ** 400), {}), (({},), {})]:
    try:
        print(args, kwargs, "->", complex(*args, **kwargs))
    except (ValueError, TypeError, OverflowError) as e:
        print(args, kwargs, "!!", type(e).__name__, e)


class WithComplex:
    def __init__(self, v):
        self.v = v

    def __complex__(self):
        return self.v


class WithFloat:
    def __float__(self):
        return 2.5


class WithIndex:
    def __index__(self):
        return 7


class BadFloat:
    def __float__(self):
        return 1


class Both:
    def __complex__(self):
        return 1 + 1j

    def __float__(self):
        return 9.0


print(complex(WithComplex(3 + 4j)), complex(WithFloat()), complex(WithIndex()), complex(WithFloat(), WithIndex()), complex(Both()))
print(complex(WithIndex(), 1j), complex(Both(), 2), complex(1, WithFloat()))
for bad in [lambda: complex(WithComplex(5)), lambda: complex(WithComplex("x")), lambda: complex(BadFloat()), lambda: complex(1, Both()),
            lambda: complex(1, WithComplex(1j))]:
    try:
        print(bad())
    except TypeError as e:
        print("TypeError", e)

print("-- attributes")
z = 3 - 4j
print(z.real, z.imag, type(z.real).__name__, z.conjugate(), (1j).conjugate(), (-0j).conjugate(), z.__complex__(), z.__getnewargs__())
print((5).real, (5).imag, (2.5).imag, (5).conjugate(), (2.5).conjugate(), True.imag)
print(hasattr(z, "__float__"), hasattr(z, "__int__"), hasattr(z, "__floordiv__"), hasattr(z, "__mod__"), hasattr(z, "__divmod__"))
print(z.__lt__(1), z.__eq__("x"), z.__add__("x"), z.__radd__(1), z.__rsub__(1), z.__rtruediv__(1), z.__rpow__(2))
print(type(z) is complex, isinstance(z, complex), isinstance(1, complex), isinstance(1.0, complex), isinstance(True, complex),
      issubclass(complex, object), complex.__name__, complex.__mro__, type(complex))
for obj, attr in [(z, "x"), (z, "real")]:
    try:
        setattr(obj, attr, 1)
    except AttributeError:
        print("AttributeError on set", attr)

print("-- equality and hashing")
print(1 + 0j == 1, 1 == 1 + 0j, 1j == 1, 1.5 + 0j == 1.5, 1.5 == 1.5 + 0j, 1j != 1j, True == 1 + 0j, 0j == False, 0j == 0.0, -0j == 0)
print(complex(nan, 0) == complex(nan, 0), complex(nan, 0) != nan, 2 ** 53 + 1 == complex(2 ** 53, 0), 10 ** 400 == 1j, 1j == "1j", 1j == None)
print(hash(0j), hash(1j), hash(2.5j), hash(1.5 + 2.5j), hash(-1 + 0j), hash(complex(-1, -1)), hash(3 + 0j) == hash(3), hash(1e100 + 0j) == hash(1e100))
print(hash(complex(inf, 0)), hash(complex(0, inf)), hash(complex(-inf, inf)), hash(complex(1e300, 1e300)), hash(-0j), hash(complex(0.5, -0.5)))
d = {1: "int one", 2.5: "float", 3j: "three j"}
print(d[1 + 0j], d[complex(2.5, 0)], d[3j], 1j in d, complex(1, 0) in d, 2.5 + 0j in {2.5}, 1 + 0j in [1], (1 + 0j) in {1: 0})
d[1 + 0j] = "replaced"
print(d)
s = {1j, 1j + 0, complex(0, 1), 1, 1.0, 1 + 0j, 2 + 3j}
print(len(s), sorted(map(repr, s)))
print(sorted([3, 1, 2], key=lambda x: complex(x, 0).real), [1j, 2j].index(2j), [1, 1j, 1.0].count(1 + 0j), (1j, 2) == (1j, 2))
for bad in [lambda: 1j < 2j, lambda: 1j <= 2j, lambda: 1j > 2j, lambda: 1j >= 2j, lambda: 1j < 1, lambda: 1.5 >= 1j, lambda: sorted([1j, 2j]), lambda: max(1j, 2j), lambda: min([1, 1j])]:
    try:
        bad()
    except TypeError as e:
        print(e)
print(bool(0j), bool(-0j), bool(1j), bool(complex(0, nan)), bool(complex(nan, 0)), not 1e-300j)

print("-- subclasses")


class C(complex):
    pass


class Tagged(complex):
    def __new__(cls, re, im, tag):
        self = super().__new__(cls, re, im)
        self.tag = tag
        return self

    def __repr__(self):
        return "Tagged(%r, %s)" % (complex(self), self.tag)

    def __add__(self, other):
        return "Tagged.add"

    def __radd__(self, other):
        return "Tagged.radd"


c = C(1, 2)
print(c, repr(c), type(c).__name__, c.real, c.imag, c.conjugate(), type(c.conjugate()).__name__, isinstance(c, complex))
print(c + 1, type(c + 1).__name__, 1 + c, type(-c).__name__, type(+c).__name__, +c == c, abs(C(3, 4)), c == 1 + 2j, hash(c) == hash(1 + 2j))
print(C(), C("3+4j"), C(5), C(1j), type(C(1j)).__name__, complex(c), type(complex(c)).__name__, {c: 1}[1 + 2j], format(c, ".1f"))
c.extra = "attr"
print(c.extra, vars(c), C.__mro__)
t = Tagged(1, 2, "t")
print(t, t.tag, t + 1, 1 + t, 1j + t, t - 1, str(t), format(t), f"{t}", f"{t:.2f}")
print(complex(t), abs(t), t == 1 + 2j, t.real, type(t * 2).__name__)

print("-- copy, pickle, json")
z = 1.5 - 2j
print(copy.copy(z) is z, copy.deepcopy(z) is z, copy.deepcopy([z, {z: z}]), copy.copy(c), type(copy.copy(c)).__name__, copy.deepcopy(c).extra)
for v in [z, 0j, complex(-0.0, inf), [1j, (2 + 3j, {"k": -1j})], {1j: 2j}]:
    back = pickle.loads(pickle.dumps(v))
    print(back, back == v or v != v)
print(pickle.loads(pickle.dumps(complex(nan, 1))))
for data in [b'c__builtin__\ncomplex\np0\n(F1.0\nF2.0\ntp1\nRp2\n.',
             b'\x80\x02c__builtin__\ncomplex\nq\x00G?\xf0\x00\x00\x00\x00\x00\x00G@\x00\x00\x00\x00\x00\x00\x00\x86q\x01Rq\x02.',
             b'\x80\x04\x95.\x00\x00\x00\x00\x00\x00\x00\x8c\x08builtins\x94\x8c\x07complex\x94\x93\x94G?\xf0\x00\x00\x00\x00\x00\x00G@\x00\x00\x00\x00\x00\x00\x00\x86\x94R\x94.']:
    print(pickle.loads(data))
for bad in [lambda: json.dumps(1j), lambda: json.dumps({"a": [1, 2j]}), lambda: json.dumps({1j: 1})]:
    try:
        bad()
    except TypeError as e:
        print(e)
print(json.dumps({"z": 1j}, default=lambda o: [o.real, o.imag]))
