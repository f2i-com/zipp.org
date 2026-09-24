# isinstance() through the inline path: every primitive against every
# builtin type, class hierarchies, classes as values, bytes/bytearray,
# tuples and unions (the general path), shadowing, and error cases.
values = [None, True, False, 0, 7, -2 ** 70, 1.5, float("nan"), "s", "", b"b", bytearray(b"x"), [1], (1,), {1: 2}, {1}, frozenset(), range(3), 3j, len, int, object()]
types = [type(None), bool, int, float, str, bytes, bytearray, list, tuple, dict, set, frozenset, range, complex, object, type]
for v in values:
    print(type(v).__name__, "".join("1" if isinstance(v, t) else "0" for t in types))


class A:
    pass


class B(A):
    pass


class C(B, int):
    pass


class Meta(type):
    pass


class WithMeta(metaclass=Meta):
    pass


a, b, c = A(), B(), C(5)
for v in (a, b, c, A, B, WithMeta, WithMeta(), Meta):
    print([isinstance(v, t) for t in (A, B, C, int, object, type, Meta, WithMeta)])
print(isinstance(c, (str, B)), isinstance(c, (str, (float, int))), isinstance(a, ()), isinstance(1, int | str), isinstance("x", int | None), isinstance(None, int | None))
print(isinstance(True, int), isinstance(1, bool), isinstance(b"x", bytes), isinstance(bytearray(), bytes), isinstance(bytearray(), bytearray))


class MyInt(int):
    pass


class MyStr(str):
    pass


print(isinstance(MyInt(3), int), isinstance(MyInt(3), MyInt), isinstance(3, MyInt), isinstance(MyStr("q"), str), isinstance("q", MyStr))
for bad in (1, "int", [int]):
    try:
        isinstance(1, bad)
    except TypeError as e:
        print("TypeError", type(bad).__name__)
count = 0
for i in range(1000):
    x = values[i % len(values)]
    if isinstance(x, int):
        count += 1
    elif isinstance(x, str):
        count += 100
    elif isinstance(x, A):
        count += 10000
print(count)
isinstance = lambda v, t: "shadowed"
print(isinstance(1, int))
del isinstance
print(isinstance(1, int))
