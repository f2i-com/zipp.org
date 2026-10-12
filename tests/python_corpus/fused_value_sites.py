# The instructions a compiled loop runs through helpers of their own (a
# global, a module attribute, a method lookup, isinstance, len, a list or
# tuple display, an unpack, an exception made): each on its fast path and on
# its slow edge, the operands changing kind mid-loop, in loops long enough
# to compile.
import math


class Box:
    def __init__(self, items):
        self.items = items

    def __len__(self):
        return len(self.items) + 100

    def upper(self):
        return "BOX"


class Sub(Box):
    pass


LIMIT = 10


def scale(x):
    return x * 2


def negate(x):
    return -x


def fifty(x):
    return 50


def shadow():
    return "shadow"


def globals_loop(n):
    global LIMIT, scale
    total = 0
    for i in range(n):
        total += scale(i) + LIMIT
        total += len([i, i])
        if i == n // 2:
            LIMIT = 1000
            scale = negate
        if i == n // 2 + 50:
            del LIMIT
            try:
                total += LIMIT
            except NameError as e:
                print("name", e)
            LIMIT = 7
    return total


def shadow_loop(n):
    global len
    total = 0
    for i in range(n):
        total += len("abc")
        if i == 300:
            len = fifty
        if i == 600:
            del len
    return total


def module_loop(n):
    total = 0.0
    m = math
    for i in range(n):
        total += math.sqrt(i) + math.floor(i / 3) + m.pi
        if i == 400:
            m = Box([1])
            try:
                total += m.pi
            except AttributeError as e:
                print("attr", e)
            m = math
    return round(total, 6)


def method_loop(n):
    out = []
    objs = ["abc", Box([1, 2]), Sub([3]), "xyz"]
    for i in range(n):
        o = objs[i % 4]
        out.append(o.upper())
        if i % 97 == 0:
            objs[1].upper = shadow
        if i % 97 == 50:
            del objs[1].upper
    return len(out), out[:6], out[96:102], sorted(set(out))


def isinstance_loop(n):
    vals = [1, 2.5, "s", None, True, [1], (1,), {"a": 1}, Box([]), Sub([]), 2**70, b"x", bytearray(b"y")]
    kinds = [int, float, str, bool, list, tuple, dict, Box, Sub, object, bytes, (int, str), type(None)]
    hits = 0
    rows = []
    for i in range(n):
        v = vals[i % len(vals)]
        k = kinds[(i // len(vals)) % len(kinds)]
        r = isinstance(v, k)
        hits += r
        if i < len(vals) * len(kinds):
            rows.append(int(r))
    try:
        for i in range(n):
            isinstance(vals[i % 3], 5 if i == 700 else int)
    except TypeError as e:
        print("isinstance", type(e).__name__)
    return hits, "".join(map(str, rows))


def len_loop(n):
    things = ["abcd", [1, 2, 3], (1, 2), {"a": 1, "b": 2}, {1, 2, 3, 4, 5}, Box([1]), Sub([1, 2]), range(7), b"xyz", "héllo"]
    total = 0
    for i in range(n):
        total += len(things[i % len(things)])
    try:
        for i in range(n):
            total += len(things[i % 4] if i != 900 else 5)
    except TypeError as e:
        print("len", e)
    return total


def seq_loop(n):
    acc = 0
    for i in range(n):
        t = (i, i + 1, i + 2)
        l = [i, t]
        a, b, c = t
        x, (p, q, r) = l
        acc += a + b + c + x + p + q + r + len(l) + len(t)
    pairs = [(1, 2), [3, 4], (5, 6), "ab", (7, 8, 9)]
    try:
        for i in range(n):
            a, b = pairs[i % 3 if i < 800 else (3 if i < 801 else 4)]
            acc += len(str(a)) + len(str(b))
    except ValueError as e:
        print("unpack", e)
    return acc


def exc_loop(n):
    d = {"a": 1}
    caught = 0
    last = None
    for i in range(n):
        try:
            caught += d["a" if i % 3 else "missing%d" % (i % 5)]
        except KeyError as e:
            caught += 2
            last = e.args
    return caught, last


N = 2000
print("globals", globals_loop(N), LIMIT, scale(3))
print("shadow", shadow_loop(N), len("abcd"))
print("module", module_loop(N))
print("method", method_loop(N))
print("isinstance", isinstance_loop(N))
print("len", len_loop(N))
print("seq", seq_loop(N))
print("exc", exc_loop(N))
