# Argument binding: positional, defaults (and mutable default sharing), keywords,
# keyword-only, positional-only, *args/**kwargs at both ends, recursion.
def pos(a, b, c):
    return (a, b, c)


def dflt(a, b=10, c="c"):
    return (a, b, c)


def kwo(a, *, k, j=2):
    return (a, k, j)


def posonly(a, b=2, /, c=3, *, d=4):
    return (a, b, c, d)


def star(*args):
    return args


def dstar(**kw):
    return sorted(kw.items())


def everything(a, b=1, *args, c, d=4, **kw):
    return (a, b, args, c, d, sorted(kw.items()))


print("pos", pos(1, 2, 3), pos(*[1, 2, 3]), pos(1, *(2, 3)), pos(*"ab", 3), pos(c=3, b=2, a=1), pos(1, c=3, b=2))
print("dflt", dflt(1), dflt(1, 2), dflt(1, 2, 3), dflt(1, c=5), dflt(a=0), dflt(*[1], **{"c": 9}))
print("kwo", kwo(1, k=2), kwo(1, j=0, k=5), kwo(a=1, k=1), kwo(*[7], **{"k": 8}))
print("posonly", posonly(1), posonly(1, 5), posonly(1, 5, 6), posonly(1, c=7), posonly(1, 2, 3, d=0))
print("star", star(), star(1), star(*range(4)), star(*[], *[1], *(2, 3)), type(star()).__name__)
print("dstar", dstar(), dstar(a=1), dstar(**{"x": 1}, y=2), dstar(**{"b": 1}, **{"a": 2}))
print("everything", everything(1, c=3), everything(1, 2, 3, 4, c=5, e=6), everything(*[1, 2, 3], **{"c": 0, "z": 1}))
print("posonly-kw-name", posonly(1, **{"c": 30}), (lambda a, /, **kw: (a, kw))(1, a=2))


def mutable_default(x, acc=[]):
    acc.append(x)
    return acc


r1 = mutable_default(1)
r2 = mutable_default(2)
r3 = mutable_default(3, [])
print("mutable-default", r1, r2, r3, r1 is r2, mutable_default.__defaults__)


def dict_default(k, v, d={}):
    d[k] = v
    return len(d)


print("dict-default", dict_default("a", 1), dict_default("b", 2), dict_default("a", 3), dict_default.__defaults__)
calls = 0


def counted():
    global calls
    calls += 1
    return calls


def evaluated_once(x=counted()):
    return x


print("default-once", evaluated_once(), evaluated_once(), calls)


def fwd(*args, **kwargs):
    return everything(*args, **kwargs)


print("forward", fwd(1, c=2), fwd(1, 2, 3, c=4, zz=5))


def deco(fn):
    def wrapper(*a, **k):
        return ("wrapped", fn(*a, **k))
    return wrapper


@deco
def target(x, y=2, *, z=3):
    return x + y + z


print("decorated", target(1), target(1, 1, z=1), target(x=5))
print("lambda", (lambda: 0)(), (lambda x, y=1: x * y)(4), (lambda *a: len(a))(1, 2, 3), (lambda **k: k)(q=1), (lambda a, *, b: a - b)(5, b=2))


def fact(n):
    return 1 if n < 2 else n * fact(n - 1)


def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)


def ackermann(m, n):
    if m == 0:
        return n + 1
    if n == 0:
        return ackermann(m - 1, 1)
    return ackermann(m - 1, ackermann(m, n - 1))


def even(n):
    return True if n == 0 else odd(n - 1)


def odd(n):
    return False if n == 0 else even(n - 1)


print("recursion", fact(30), fib(18), ackermann(2, 3), even(101), odd(77))


def deep(n):
    return 0 if n == 0 else 1 + deep(n - 1)


print("deep", deep(400))


def kwargs_mutation(**kw):
    kw["added"] = True
    return kw


src = {"a": 1}
out = kwargs_mutation(**src)
print("kwargs-copy", src, sorted(out.items()), out is src)


def args_tuple(*args):
    return args


lst = [1, 2]
t = args_tuple(*lst)
lst.append(3)
print("args-copy", t, lst)


class Callable:
    def __call__(self, a, b=0, *rest, **kw):
        return (a, b, rest, sorted(kw))


c = Callable()
print("callable-obj", c(1), c(1, 2, 3, x=4), c(*[5], **{"b": 6}))


def gen_defaults(n, step=1):
    i = 0
    while i < n:
        yield i
        i += step


print("gen-defaults", list(gen_defaults(5)), list(gen_defaults(10, step=3)), list(gen_defaults(n=4, step=2)))
funcs = [pos, dflt, kwo, star]
print("defaults-attr", [f.__defaults__ for f in funcs], kwo.__kwdefaults__, posonly.__kwdefaults__, dflt.__name__)
big = list(range(100))
print("many-args", star(*big)[-3:], len(star(*big, *big)), pos(*big[:3]))
print("kw-order", dstar(z=1, a=2, m=3), list((lambda **k: k)(z=1, a=2, m=3)))
