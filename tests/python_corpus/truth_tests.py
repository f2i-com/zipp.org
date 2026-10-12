# Truth tests of every kind of value in `if`, `while`, `not`, `and`/`or`,
# conditional expressions and comprehension filters, in loops long enough
# to be compiled: bools, ints (zero, negative, beyond 64 bits), strs
# (empty, non-ASCII), floats (zero, negative zero, NaN), None, containers,
# ranges, and objects with __bool__ / __len__ (including bad results).


class B:
    def __init__(self, v):
        self.v = v

    def __bool__(self):
        return self.v


class L:
    def __init__(self, n):
        self.n = n

    def __len__(self):
        return self.n


class BadBool:
    def __bool__(self):
        return 1


class BadLen:
    def __len__(self):
        return -1


values = [True, False, 0, 1, -1, 2 ** 70, -(2 ** 70), "", "a", "é", "0", 0.0, -0.0, 1.5,
          float("nan"), float("inf"), None, [], [0], (), (0,), {}, {0: 0}, set(), {0},
          range(0), range(3), b"", b"x", B(True), B(False), L(0), L(2), 0j, 1j]


def kinds(vs):
    out = []
    for v in vs:
        a = 1 if v else 0
        b = 0 if not v else 1
        c = bool(v)
        d = (v and "t") or "f"
        if v:
            e = "if"
        else:
            e = "else"
        out.append((a, b, c, d, e))
    return out


print(kinds(values))
print([i for i in range(-3, 4) if i], [s for s in ["", "x", "", "y"] if s], [x for x in values if not x][:6])


def loops(n):
    total = 0
    i = n
    while i:
        i -= 1
        total += 1 if i % 3 else 0
    s = "abc"
    while s:
        s = s[1:]
        total += 10
    xs = [1, 2, 3]
    while xs:
        xs.pop()
        total += 100
    return total


print(loops(3000))


def hot(n):
    words = ["", "a", "bb", "", "ccc"]
    hits = 0
    for i in range(n):
        w = words[i % 5]
        if not w:
            continue
        if i % 2:
            hits += len(w)
        hits += 1 if (i & 4) else 0
    return hits


print(hot(5000))
for bad in (BadBool(), BadLen()):
    try:
        if bad:
            print("truthy")
    except (TypeError, ValueError) as e:
        print(type(e).__name__, e)
