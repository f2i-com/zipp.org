# Call evaluation order: the callee (or the receiver and its attribute
# lookup) before the positional arguments, then the keyword arguments; an
# operand already evaluated keeps its value when a later operand rebinds
# the same name with a walrus.
log = []


def t(tag, value=None):
    log.append(tag)
    return tag if value is None else value


def flush(label):
    print(label, " ".join(str(x) for x in log))
    log.clear()


class O:
    def m(self, *a, **k):
        return (a, sorted(k.items()))


def f3(a, b, c):
    return a + b + c


print(t("obj", O()).m(t("arg", 1)))
flush("receiver-arg")
print(t("callee", f3)(t("a", 1), t("b", 2), t("c", 3)))
flush("callee-args")
print(t("obj", O()).m(t("p", 1), *t("star", [2]), k=t("kw", 3), **t("kwargs", {"z": 4})))
flush("positional-star-keywords")
print(t("outer", O()).m(t("inner", O()).m(t("leaf", 3))))
flush("nested")
print(t("s", "-").join(t("parts", ["a", "b"])))
flush("builtin-method")


def old(x):
    return "old"


def rebind():
    global old
    old = lambda x: "new"
    return 1


print(old(rebind()))
flush("callee-before-rebind")


class Swap:
    def m(self, x):
        return "first"


def swap_method(obj):
    obj.__class__.m = lambda self, x: "second"
    return 0


s = Swap()
print(s.m(swap_method(s)))
flush("method-before-rebind")

try:
    undefined_function(t("never"))
except NameError as e:
    print("NameError", e)
flush("name-error-first")
try:
    O().missing(t("never"))
except AttributeError:
    print("AttributeError")
flush("attribute-error-first")


class Getattr:
    def __getattr__(self, name):
        log.append("getattr " + name)
        return lambda *a: a


print(Getattr().dynamic(t("arg")))
flush("getattr-before-arg")


class Instance:
    def m(self, x):
        return "class " + str(x)


inst = Instance()
inst.m = lambda x: "instance " + str(x)
print(inst.m(t("v", 5)), Instance.m(inst, 6))
flush("instance-shadow")


class Meta:
    @classmethod
    def c(cls, x):
        return cls.__name__ + str(x)

    @staticmethod
    def s(x):
        return "static" + str(x)


print(Meta.c(t("c", 1)), Meta().c(t("c2", 2)), Meta.s(t("s", 3)), Meta().s(t("s2", 4)))
flush("class-and-static")

import math

print(math.floor(t("f", 2.5)), "abc".upper(), [3, 1, 2].index(t("i", 1)))
flush("module-and-builtin")


def walrus_operands():
    x = 1
    print(x + (x := 5), x)
    y = 1
    print([y, (y := 3), y])
    z = 10
    print(z < (z := 0), z)

    def g(*a):
        return a

    w = 1
    print(g(w, (w := 2), w))
    u = 2
    print(u * (u := 3) + u)
    v = 4
    v += (v := 10)
    print(v)


walrus_operands()


def cell_arguments():
    # A cell is not order-transparent: the attribute lookup may rebind or
    # delete it through `nonlocal` before the argument is read.
    x = 1

    class P:
        def __getattr__(self, name):
            nonlocal x
            x = 2
            return lambda v: v

    print("cell-rebound", P().m(x))
    y = 5

    class Q:
        @property
        def meth(self):
            nonlocal y
            del y
            return print

    try:
        Q().meth(y)
    except NameError:
        print("cell-deleted NameError")


cell_arguments()
