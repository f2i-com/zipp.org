# `len(x)` and `isinstance(x, t)` call the builtin directly only while the
# name still means the builtin: shadowing at any scope, rebinding the
# global mid-run, and every argument kind keep Python's behaviour.


class Sized:
    def __len__(self):
        return 7


class Base:
    pass


class Child(Base):
    pass


values = [[1, 2], (1,), "h\xe9", {"a": 1}, {1, 2, 3}, range(5), b"xyz", Sized(), ""]
print([len(v) for v in values])
for bad in [5, None, 1.5, Base()]:
    try:
        len(bad)
    except TypeError as e:
        print("TypeError", e)
checks = [(1, int), (True, int), (True, bool), (1.0, int), ("s", str), (Child(), Base),
          (Base(), Child), (None, type(None)), ([], (dict, list)), (3, (str, float)),
          (2, int | str), ("x", int | None), (None, int | None), (Child, type)]
print([isinstance(v, t) for v, t in checks])
try:
    isinstance(1, 5)
except TypeError as e:
    print("TypeError", e)


def local_shadow():
    def len(x):
        return "local len"

    def isinstance(a, b):
        return "local isinstance"

    return len([1]), isinstance(1, int)


print(local_shadow())


def uses_global():
    return len([1, 2, 3]), isinstance(1, int)


print(uses_global())
real_len = len
real_isinstance = isinstance
len = lambda x: "global len " + str(real_len(x))
isinstance = lambda a, b: "global isinstance"
print(uses_global())
del len
del isinstance
print(uses_global())


class Scoped:
    len = lambda x: "class len"
    here = len([1])

    def method(self):
        return len([1, 2])


print(Scoped.here, Scoped().method())
total = 0
xs = [1, 2, 3]
for i in range(1000):
    total += len(xs)
    if isinstance(i, int) and not isinstance(i, bool):
        total += 1
print(total)
