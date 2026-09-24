# Module globals and builtins read at warm sites while they change: a
# global rebound, deleted (a builtin of the same name showing through
# again), a builtin shadowed by a new global mid-loop, globals() written,
# and names read from several functions.
import builtins

K = 1


def total(n, change=None, at=-1):
    s = 0
    for i in range(n):
        if i == at and change is not None:
            change()
        s += K
    return s


print(total(10))


def rebind():
    global K
    K = 100


print(total(10, rebind, 5))


def via_globals():
    globals()["K"] = 7


print(total(6, via_globals, 3))


def lens(xs, n, change=None, at=-1):
    out = []
    for i in range(n):
        if i == at and change is not None:
            change()
        out.append(len(xs))
    return out


print(lens([1, 2, 3], 5))


def shadow_len():
    global len
    len = lambda xs: -1


print(lens([1, 2, 3], 6, shadow_len, 3))


def unshadow_len():
    global len
    del len


print(lens([1, 2, 3], 6, unshadow_len, 2))

real_abs = builtins.abs


def abses(n, change=None, at=-1):
    out = []
    for i in range(n):
        if i == at and change is not None:
            change()
        out.append(abs(-i))
    return out


print(abses(5))


print(abses(3), builtins.abs is real_abs)


def late():
    return LATER


try:
    late()
except NameError as e:
    print("NameError", "LATER" in str(e))
LATER = "defined"
print(late())
del LATER
try:
    late()
except NameError as e:
    print("NameError again", "LATER" in str(e))


def fresh_globals(n):
    out = []
    for i in range(n):
        if i == 3:
            globals()["NEWNAME"] = i
        try:
            out.append(NEWNAME)
        except NameError:
            out.append("missing")
    return out


print(fresh_globals(6))


def many_globals(n):
    for i in range(n):
        globals()["G%d" % i] = i
    return sum(globals()["G%d" % i] for i in range(n))


print(many_globals(40))
print(total(4))


def isinst(vals, n, change=None, at=-1):
    out = []
    for i in range(n):
        if i == at and change is not None:
            change()
        out.append(isinstance(vals[i % len(vals)], int))
    return out


print(isinst([1, "a", True, 2.0], 8))


def shadow_isinstance():
    global isinstance
    isinstance = lambda v, t: "shadowed"


print(isinst([1, "a"], 6, shadow_isinstance, 3))
del isinstance
print(isinst([1, "a"], 2))
