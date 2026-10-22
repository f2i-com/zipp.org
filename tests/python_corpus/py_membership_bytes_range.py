# `in` for bytes, bytearray and range, through the operator and through
# `__contains__`: a byte must be an integer in range(0, 256) (anything with
# __index__ counts), anything else bytes-like; a range answers ints by
# arithmetic and asks everything else's equality, member by member.

def t(label, f):
    try:
        print(label, f())
    except Exception as e:
        print(label, type(e).__name__, e)

class I(int):
    pass

class Idx:
    def __index__(self):
        return 97

class BadIdx:
    def __index__(self):
        return 300

class EqualsTwo:
    def __eq__(self, other):
        return other == 2

class Boom:
    def __eq__(self, other):
        raise KeyError("boom")

class Counting:
    calls = 0
    def __eq__(self, other):
        Counting.calls += 1
        return False

class TwoInt(int):
    def __eq__(self, other):
        return other == 2

for value in [-1, 0, 97, 255, 256, 1 << 100, -(1 << 100), True, False]:
    t("bytes %r" % (value,), lambda: value in b"a\x00\xff\x01")
    t("bytearray %r" % (value,), lambda: value in bytearray(b"a\x00\xff\x01"))
t("bytes sub", lambda: b"bc" in b"abc")
t("bytes sub absent", lambda: b"cb" in b"abc")
t("bytes empty sub", lambda: b"" in b"")
t("bytes bytearray needle", lambda: bytearray(b"bc") in b"abc")
t("bytearray sub", lambda: b"bc" in bytearray(b"abc"))
t("bytearray bytearray", lambda: bytearray(b"c") in bytearray(b"abc"))
t("bytes int subclass", lambda: I(97) in b"a")
t("bytes int subclass absent", lambda: I(98) in b"a")
t("bytes __index__", lambda: Idx() in b"a")
t("bytes __index__ out of range", lambda: BadIdx() in b"a")
t("bytes str", lambda: "a" in b"a")
t("bytes float", lambda: 97.0 in b"a")
t("bytes None", lambda: None in b"a")
t("bytes list", lambda: [97] in b"a")
t("bytes not in", lambda: 98 not in b"a")
t("bytes not in out of range", lambda: 300 not in b"a")
t("bytes dunder", lambda: b"a".__contains__(97))
t("bytes dunder sub", lambda: b"abc".__contains__(b"bc"))
t("bytes dunder range", lambda: b"a".__contains__(-1))
t("bytearray dunder", lambda: bytearray(b"ab").__contains__(b"b"))
t("bytes has dunder", lambda: hasattr(b"", "__contains__"))

t("range custom eq", lambda: EqualsTwo() in range(4))
t("range custom eq absent", lambda: EqualsTwo() in range(2))
t("range custom eq descending", lambda: EqualsTwo() in range(5, 0, -1))
t("range custom eq stepped", lambda: EqualsTwo() in range(0, 10, 2))
t("range custom eq stepped absent", lambda: EqualsTwo() in range(1, 10, 2))
t("range custom eq empty", lambda: EqualsTwo() in range(0))
t("range not in", lambda: EqualsTwo() not in range(4))
t("range raising eq", lambda: Boom() in range(3))
t("range raising eq empty", lambda: Boom() in range(0))
t("range int subclass", lambda: I(2) in range(4))
t("range int subclass absent", lambda: I(9) in range(4))
t("range int subclass eq", lambda: TwoInt(7) in range(4))
t("range float", lambda: 2.0 in range(4))
t("range float fraction", lambda: 2.5 in range(4))
t("range float nan", lambda: float("nan") in range(4))
t("range float inf", lambda: float("inf") in range(4))
t("range str", lambda: "2" in range(4))
t("range None", lambda: None in range(4))
t("range complex", lambda: complex(2, 0) in range(4))
t("range bool", lambda: True in range(2))
t("range big", lambda: 10 ** 30 in range(0, 10 ** 31, 10 ** 29))
t("range big absent", lambda: 10 ** 30 + 1 in range(0, 10 ** 31, 10 ** 29))
t("range negative step", lambda: -4 in range(0, -10, -2))
t("range dunder", lambda: range(4).__contains__(EqualsTwo()))
Counting.calls = 0
t("range scan count", lambda: Counting() in range(7))
print("range eq calls", Counting.calls)
