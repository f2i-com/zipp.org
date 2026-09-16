# is / is not against None, True, False, singletons and shared objects; == versus is;
# `x is None` shapes a compiler would lower to a pointer compare.
def classify(x):
    if x is None:
        return "None"
    if x is True:
        return "True"
    if x is False:
        return "False"
    if x is Ellipsis:
        return "Ellipsis"
    if x is NotImplemented:
        return "NotImplemented"
    return "other:" + type(x).__name__


for v in [None, True, False, 0, 1, 0.0, 1.0, "", [], (), ..., NotImplemented, 2 > 1, 1 == 2, bool(3), not 0, None or None]:
    print("classify", repr(v), classify(v), v is not None, v is not True, v is not False, v == None, v != None)

a = [1, 2]
b = a
c = [1, 2]
print("lists", a is b, a is c, a == c, a is not c, b is a, [] is [], a[:] is a)
t = (1, 2)
u = t
print("tuples", t is u, t == (1, 2), t is not (1, 2, 3))
d = {}
print("dicts", d is d, {} is {}, d.get("x") is None, d.setdefault("k", None) is None, d.pop("k") is None)
s = "hello"
s2 = s
print("strs", s is s2, s == "hel" + "lo")
x = 10**30
y = x
print("bigint", x is y, x == 10**30, y is not x)
f = 1.5
g = f
print("float", f is g, f is not g)
n = float("nan")
m = n
print("nan", n is m, n is n, n == m)


class Sentinel:
    pass


MISSING = Sentinel()


def lookup(mapping, key, default=MISSING):
    v = mapping.get(key, MISSING)
    if v is MISSING:
        if default is MISSING:
            return "raise"
        return default
    return v


print("sentinel", lookup({"a": 1}, "a"), lookup({"a": None}, "a"), lookup({}, "a"), lookup({}, "a", None), lookup({}, "a", 0))


class EqAll:
    def __eq__(self, other):
        return True

    def __ne__(self, other):
        return False


e = EqAll()
print("eq-override", e == None, e != None, e is None, e is not None, None == e, e == 5, 5 == e)
print("bool-identity", (1 < 2) is True, (1 > 2) is False, bool([]) is False, isinstance(True, int), True is not 1 if False else "skip")
print("type-identity", type(1) is int, type(True) is bool, type(True) is int, type(None) is type(None), type([]) is list, int is int, type(type) is type)
print("method-identity", [].append is not [].append, len is len, print is print, str.upper is str.upper)


def returns_none():
    pass


def returns_explicit():
    return None


r1 = returns_none()
r2 = returns_explicit()
print("none-results", r1 is None, r2 is None, print("side") is None, [].sort() is None, {}.update() is None, r1 is r2)
seq = [None, 0, False, None, "", None]
print("none-count", sum(1 for v in seq if v is None), sum(1 for v in seq if v is not None), [i for i, v in enumerate(seq) if v is None], seq.count(None), seq.index(None, 1))
flags = [True, 1, 1.0, False, 0]
print("true-count", sum(1 for v in flags if v is True), sum(1 for v in flags if v == True), [v is False for v in flags])
node = {"next": {"next": {"next": None, "v": 3}, "v": 2}, "v": 1}
total = 0
while node is not None:
    total += node["v"]
    node = node["next"]
print("linked", total)
