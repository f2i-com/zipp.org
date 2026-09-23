# printf-style formatting calls __float__/__int__/__index__ like CPython.
class Scalar:
    def __init__(self, v):
        self.v = v
    def __float__(self):
        return float(self.v)
    def __int__(self):
        return int(self.v)
    def __index__(self):
        return int(self.v)


class OnlyFloat:
    def __float__(self):
        return 2.5


class OnlyIndex:
    def __index__(self):
        return 7


class OnlyInt:
    def __int__(self):
        return 9


s = Scalar(3.75)
print("%.3f" % s, "%d" % s, "%i" % s, "%5.1f|" % s, "%e" % s, "%g" % s)
print("%x %o %X" % (OnlyIndex(), OnlyIndex(), Scalar(255)))
print("%f" % OnlyFloat(), "%.2f" % OnlyIndex(), "%d" % OnlyIndex(), "%d" % OnlyInt())
print("loss %.4f acc %d%%" % (Scalar(0.123456), Scalar(97)))
print("%(a).2f %(b)d" % {"a": Scalar(1.5), "b": Scalar(2)})
print("%s %r" % (1.5, "x"), "%d" % 3.99, "%d" % True, "%.1f" % 2)
for fmt, v in [("%d", OnlyFloat()), ("%x", OnlyInt()), ("%f", "text"), ("%d", "3"), ("%x", 1.5)]:
    try:
        print(fmt % v)
    except TypeError as e:
        print(fmt, "TypeError", e)
