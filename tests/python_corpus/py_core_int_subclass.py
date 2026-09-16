# Pending: subclassing int (split out of tests/python_corpus/truthiness.py).
class BoolSub(int):
    pass
try:
    z, t = BoolSub(0), BoolSub(3)
    print("int-subclass", bool(z), bool(t), z + 1, t * 2, type(t).__name__, isinstance(t, int), repr(t), t == 3)
except TypeError as e:
    print("TypeError", e)
class Money(int):
    def __new__(cls, v, cur="EUR"):
        o = super().__new__(cls, v)
        o.cur = cur
        return o
    def __repr__(self):
        return f"{int(self)} {self.cur}"
try:
    m = Money(5, "USD")
    print("int-subclass-new", m, m + 1, m.cur, int(m), m > 4)
except TypeError as e:
    print("TypeError", e)
class F(float):
    pass
try:
    print("float-subclass", F(1.5) * 2, type(F(2.0)).__name__, F("3.25"))
except TypeError as e:
    print("TypeError", e)
class S(str):
    def shout(self):
        return self.upper() + "!"
try:
    print("str-subclass", S("hi").shout(), S("ab") + "c", len(S("xyz")), isinstance(S("q"), str))
except TypeError as e:
    print("TypeError", e)
