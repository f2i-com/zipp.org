# `o[k]` that finds nothing: the error each kind of receiver raises and
# its message, for dict keys of every kind, dict subclasses with and
# without __missing__, sequences out of range, and the same in loops long
# enough to compile.


def miss(o, k):
    try:
        return ("value", o[k])
    except Exception as e:
        return (type(e).__name__, e.args)


class Missing(dict):
    def __missing__(self, key):
        return ("missing", key)


class Plain(dict):
    pass


class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and other.v == self.v

    def __repr__(self):
        return "Key(%r)" % (self.v,)


d = {"a": 1, 2: "two", (1, 2): "pair", 3.5: "f", None: "none", True: "t"}
for k in ("a", "b", "it's", 2, 4, (1, 2), (2, 1), 3.5, 0.5, None, True, False, 1, frozenset([1]), Key(1)):
    print(repr(k), miss(d, k))
print(miss(d, [1]), miss(d, {}))
dk = {Key(1): "k1"}
print(miss(dk, Key(1)), miss(dk, Key(2)))
print(miss(Missing(a=1), "a"), miss(Missing(a=1), "zz"), miss(Plain(a=1), "zz"))
print(miss([1, 2, 3], 3), miss([1, 2, 3], -4), miss([1, 2, 3], -1), miss([], 0))
print(miss((1, 2), 2), miss((1, 2), -3), miss("ab", 2), miss("ab", -1), miss("", 0))
print(miss([1], "x"), miss((1,), 1.0), miss(5, 0), miss(None, 0))
print(miss({}, "x"), miss(dict(), 0))


def hot(n):
    words = {"w%d" % i: i for i in range(50)}
    nums = {i: i * i for i in range(50)}
    hits = misses = 0
    last = None
    for i in range(n):
        try:
            hits += words["w%d" % (i % 70)]
        except KeyError as e:
            misses += 1
            last = e.args
        try:
            hits += nums[i % 60]
        except KeyError as e:
            misses += 1
            last = (last, e.args)
    return hits, misses, last


print(hot(3000))


def hot_missing(n):
    m = Missing(x=1)
    out = 0
    for i in range(n):
        r = m["y" if i % 2 else "x"]
        out += 1 if isinstance(r, tuple) else r
    return out


print(hot_missing(2000))
try:
    {"a": 1}["b"]
except KeyError as e:
    print("KeyError", repr(e), str(e), e.args)
