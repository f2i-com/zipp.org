# The messages of the builtin exceptions raised by common operations: KeyError,
# IndexError, AttributeError, TypeError for operators, NameError, ValueError, and hasattr/getattr.
import math


def show(label, thunk):
    try:
        r = thunk()
        print(label, "->", repr(r))
    except Exception as e:
        print(label, "!", type(e).__name__ + ":", e)


class Obj:
    cls_attr = 1

    def __init__(self):
        self.inst = 2


class Slots:
    __slots__ = ("a",)


o = Obj()
show("key-str", lambda: {"a": 1}["b"])
show("key-int", lambda: {1: 2}[3])
show("key-tuple", lambda: {}[(1, 2)])
show("key-none", lambda: {}[None])
show("key-pop", lambda: {}.pop("gone"))
show("key-popitem", lambda: {}.popitem())
show("set-remove", lambda: set().remove(5))
show("index-list", lambda: [1, 2, 3][3])
show("index-list-neg", lambda: [1][-2])
show("index-tuple", lambda: (1,)[5])
show("index-str", lambda: "abc"[10])
show("index-range", lambda: range(3)[3])
show("index-pop-empty", lambda: [].pop())
show("index-pop-range", lambda: [1].pop(5))
show("index-assign", lambda: [1].__setitem__(3, 0))
show("index-type", lambda: [1, 2]["x"])
show("index-float", lambda: (1, 2)[1.0])
show("attr-instance", lambda: o.missing)
show("attr-class", lambda: Obj.missing)
show("attr-int", lambda: (5).missing)
show("attr-str", lambda: "s".missing)
show("attr-list", lambda: [].missing)
show("attr-none", lambda: None.missing)
show("attr-module", lambda: math.missing)
show("attr-set-slots", lambda: setattr(Slots(), "b", 1))
show("attr-unset-slot", lambda: Slots().a)
show("type-add", lambda: 1 + "a")
show("type-radd", lambda: "a" + 1)
show("type-sub", lambda: [1] - [1])
show("type-mul", lambda: "a" * 2.0)
show("type-lt", lambda: 1 < "a")
show("type-lt-none", lambda: None < None)
show("type-lt-list", lambda: [1] < (1,))
show("type-neg", lambda: -"a")
show("type-invert", lambda: ~1.5)
show("type-call", lambda: (1)())
show("type-subscript", lambda: (5)[0])
show("type-subscript-none", lambda: None[0])
show("type-item-assign", lambda: (1, 2).__setitem__(0, 1))
show("type-iter", lambda: iter(5))
show("type-len", lambda: len(5))
show("type-hash", lambda: hash([]))
show("type-str-in", lambda: 1 in "abc")
show("type-concat-tuple", lambda: (1,) + [2])
show("type-int-arg", lambda: int(None))
show("type-sorted-mixed", lambda: sorted([1, "a"]))
show("value-int", lambda: int("12a"))
show("value-int-base", lambda: int("z", 10))
show("value-float", lambda: float("abc"))
show("value-list-index", lambda: [1, 2].index(3))
show("value-list-remove", lambda: [1].remove(2))
show("value-str-index", lambda: "abc".index("z"))
show("value-unpack", lambda: exec("a, b = 1, 2, 3") if False else [1, 2, 3].__iter__().__next__())
show("value-chr", lambda: chr(-1))
show("value-range-step", lambda: range(1, 2, 0))
show("zero-div", lambda: 1 // 0)
show("zero-mod", lambda: 5 % 0)
show("zero-float", lambda: 2.0 / 0)
show("name", lambda: undefined_variable)
show("stop", lambda: next(iter([])))
show("recursion-free", lambda: sum(range(10)))
show("hasattr-true", lambda: hasattr(o, "inst"))
show("hasattr-cls", lambda: hasattr(o, "cls_attr"))
show("hasattr-false", lambda: hasattr(o, "nothing"))
show("getattr-default", lambda: getattr(o, "nothing", "d"))
show("getattr-no-default", lambda: getattr(o, "nothing"))


class Raises:
    @property
    def bad(self):
        raise ValueError("property failed")

    @property
    def missing_inside(self):
        return self.does_not_exist


r = Raises()
show("hasattr-property-valueerror", lambda: hasattr(r, "bad"))
show("hasattr-property-attributeerror", lambda: hasattr(r, "missing_inside"))
show("getattr-property-default", lambda: getattr(r, "missing_inside", "fallback"))
show("getattr-property-valueerror", lambda: getattr(r, "bad", "fallback"))
d = {"present": 1}
for key in ["present", "absent", "present"]:
    try:
        print("loop-key", key, d[key])
    except KeyError as e:
        print("loop-key", key, "KeyError", e, e.args)
try:
    raise KeyError("a", "b")
except KeyError as e:
    print("keyerror-multi", e, e.args)
