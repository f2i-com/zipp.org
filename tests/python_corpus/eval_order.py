# Evaluation order with visible side effects: operands, chained comparisons,
# arguments among themselves, subscripts, assignment targets, displays, boolean ops.
from contextlib import nullcontext

log = []


def t(tag, value=None):
    log.append(tag)
    return tag if value is None else value


def flush(label):
    print(label, " ".join(str(x) for x in log))
    log.clear()


r = t("a", 1) + t("b", 2) * t("c", 3)
flush(f"binop={r}")
r = t("a", 2) ** t("b", 3) ** t("c", 1)
flush(f"pow={r}")
r = t("a", 1) - t("b", 2) - t("c", 3)
flush(f"sub={r}")
r = t("a", 1) < t("b", 2) < t("c", 0) < t("d", 5)
flush(f"chain={r}")
r = t("a", 1) < t("b", 2) <= t("c", 2) != t("d", 3)
flush(f"chain2={r}")
r = t("a", 0) or t("b", "") or t("c", 3) or t("d", 4)
flush(f"or={r}")
r = t("a", 1) and t("b", 0) and t("c", 3)
flush(f"and={r}")
r = t("x", 5) if t("cond", True) else t("y", 6)
flush(f"ifexp={r}")
r = not t("a", 0)
flush(f"not={r}")


def f3(a, b, c):
    return a + b + c


def fkw(a, b=0, **kw):
    return a + b + sum(kw.values())


r = f3(t("a", 1), t("b", 2), t("c", 3))
flush(f"args={r}")
r = fkw(t("pos", 1), b=t("kw-b", 2), z=t("kw-z", 3), y=t("kw-y", 4))
flush(f"kwargs={r}")
r = f3(*t("star", [1, 2]), t("after", 3))
flush(f"star={r}")
r = fkw(t("p", 1), **t("dstar", {"b": 2}), q=t("q", 3))
flush(f"dstar={r}")

data = {"k": [10, 20, 30]}
r = t("obj", data)[t("key", "k")][t("idx", 1)]
flush(f"subscript={r}")
r = t("seq", [0, 1, 2, 3, 4])[t("lo", 1):t("hi", 4):t("step", 2)]
flush(f"slice={r}")

target = [0, 0, 0]
t("container", target)[t("index", 1)] = t("value", 99)
flush(f"store-subscript={target}")
d = {}
d[t("k1", "a")] = d[t("k2", "b")] = t("v", 1)
flush(f"chained-store={sorted(d.items())}")
x, y = t("rhs1", 1), t("rhs2", 2)
flush(f"tuple-assign={x},{y}")


class Box:
    pass


box = Box()
t("obj", box).attr = t("val", 7)
flush(f"store-attr={box.attr}")
counts = {"n": 0}
t("aug-obj", counts)[t("aug-key", "n")] += t("aug-val", 5)
flush(f"augassign={counts}")
box.attr += t("inc", 1)
flush(f"aug-attr={box.attr}")

r = [t("l1", 1), t("l2", 2), *t("lstar", [3]), t("l4", 4)]
flush(f"list={r}")
r = (t("t1", 1), t("t2", 2))
flush(f"tuple={r}")
r = {t("k1", "a"): t("v1", 1), t("k2", "b"): t("v2", 2), **t("dd", {"c": 3})}
flush(f"dict={sorted(r.items())}")
r = {t("s1", 1), t("s2", 2)}
flush(f"set={sorted(r)}")
r = [t(f"elem{i}", i) for i in t("iterable", range(3)) if t(f"cond{i}", i != 1)]
flush(f"listcomp={r}")
r = {t(f"key{i}", i): t(f"val{i}", i * i) for i in range(2)}
flush(f"dictcomp={r}")
r = f"{t('f1', 1)}-{t('f2', 2)}-{t('f3', 3)!r}"
flush(f"fstring={r}")
r = "%s %s" % (t("m1", 1), t("m2", 2))
flush(f"percent={r}")
r = t("needle", 2) in t("hay", [1, 2])
flush(f"in={r}")
i = 0
arr = [10, 20, 30]
i, arr[i] = 1, 99
flush(f"target-order={i},{arr}")


def gen():
    t("gen-start")
    yield t("y1", 1)
    t("gen-mid")
    yield t("y2", 2)
    t("gen-end")


g = gen()
t("created")
for v in g:
    t(f"body{v}")
flush("generator")
try:
    t("try")
    raise ValueError(t("exc-arg", "boom"))
except ValueError as e:
    t("except")
finally:
    t("finally")
flush("try")


def ret():
    try:
        return t("return-expr", 1)
    finally:
        t("finally")


ret()
flush("return-finally")
with nullcontext(t("ctx", 1)) as c:
    t("with-body")
flush("with")
r = max(t("m1", 3), t("m2", 9), key=t("keyfn", lambda v: -v))
flush(f"builtin-args={r}")
lam = lambda a=t("default-eval", 1): a
flush("lambda-default")
