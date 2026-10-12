# Sites whose cache entries hold for many executions and then stop holding:
# attribute reads and stores, method lookups, global reads and calls, in
# function bodies run thousands of times (interpreted) and in the loops that
# call them (compiled when long enough). Each site must answer exactly as an
# uncached lookup would at every step.


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def norm1(self):
        return abs(self.x) + abs(self.y)

    def shift(self, d):
        self.x += d
        return self.x


class Point3(Point):
    def __init__(self, x, y, z):
        self.z = z
        Point.__init__(self, x, y)

    def norm1(self):
        return Point.norm1(self) + abs(self.z)


class Loose:
    def __getattr__(self, name):
        return len(name)


def read_x(p):
    return p.x


def write_x(p, v):
    p.x = v


def call_norm(p):
    return p.norm1()


def reads(n):
    out = []
    a, b, c = Point(1, 2), Point3(3, 4, 5), Loose()
    total = 0
    for i in range(n):
        total += read_x(a)
    out.append(total)
    # Other layouts and kinds at the same site.
    for i in range(n):
        total += read_x(a if i % 3 else b) + read_x(c if i % 5 == 0 else a)
    out.append(total)
    # The attribute deleted and set again (the layout changes and comes back).
    for i in range(n):
        if i == n // 2:
            del a.x
            try:
                read_x(a)
            except AttributeError as e:
                out.append(str(e))
            Point.x = "class value"
            out.append(read_x(a))
            del Point.x
            a.x = 10
        total += read_x(a)
    out.append(total)
    # The name becomes a property on the class, then stops being one.
    for i in range(n):
        if i == n // 3:
            Point.x = property(lambda self: 1000)
        if i == 2 * n // 3:
            del Point.x
        total += read_x(a)
    out.append(total)
    return out


def writes(n):
    out = []
    a, b = Point(0, 0), Point3(0, 0, 0)
    for i in range(n):
        write_x(a, i)
        write_x(b if i % 2 else a, -i)
    out.append((a.x, b.x))
    # Fresh instances: each store appends to new storage.
    made = [Point(i, -i) for i in range(n)]
    out.append(sum(p.x - p.y for p in made))
    seen = []

    def setter(self, v):
        seen.append(v)

    for i in range(n):
        if i == n // 2:
            Point.x = property(lambda self: -1, setter)
        if i == n // 2 + 3:
            del Point.x
        write_x(a, i)
    out.append((a.x, seen))

    class Guarded(Point):
        def __setattr__(self, name, value):
            object.__setattr__(self, name, value * 2)

    g = Guarded(1, 2)
    for i in range(n):
        write_x(g if i % 4 == 0 else a, 5)
    out.append((g.x, g.y, a.x))
    try:
        write_x(None, 1)
    except AttributeError as e:
        out.append(type(e).__name__)
    return out


def methods(n):
    out = []
    a, b = Point(1, -2), Point3(1, -2, 3)
    total = 0
    for i in range(n):
        total += call_norm(a) + call_norm(b if i % 2 else a)
    out.append(total)
    original = Point.norm1
    for i in range(n):
        if i == n // 4:
            Point.norm1 = lambda self: 100
        if i == n // 2:
            a.norm1 = lambda: 7
        if i == 3 * n // 4:
            del a.norm1
            Point.norm1 = original
        total += call_norm(a)
    out.append(total)
    del Point.norm1
    try:
        call_norm(a)
    except AttributeError as e:
        out.append(str(e))
    try:
        call_norm(b)
    except AttributeError as e:
        out.append(str(e))
    Point.norm1 = original
    words = ["ab", "CD", "ef"]
    texts = 0
    for i in range(n):
        texts += len(words[i % 3].upper()) + words[i % 3].count("e")
    out.append(texts)
    for i in range(n):
        total += a.shift(1) + (b.shift(-1) if i % 7 == 0 else 0)
    out.append((total, a.x, b.x))
    return out


LIMIT = 3


def read_limit():
    return LIMIT


def read_len():
    return len


def use_len(x):
    return len(x)


def global_reads(n):
    global LIMIT, len
    out = []
    total = 0
    for i in range(n):
        total += read_limit() + use_len("abcd")
        if i == n // 2:
            LIMIT = 50
    out.append(total)
    del LIMIT
    try:
        read_limit()
    except NameError as e:
        out.append(str(e))
    LIMIT = 1
    builtin_len = read_len()
    for i in range(n):
        if i == n // 3:
            len = lambda x: 99
        if i == 2 * n // 3:
            del len
        total += use_len("ab") + read_limit()
    out.append((total, read_len() is builtin_len))
    return out


def inc(a, b=1):
    return a + b


def twice(a):
    return a * 2


def depth(n):
    return 0 if n == 0 else 1 + depth(n - 1)


def apply(f, x):
    return f(x)


def calls(n):
    global inc
    out = []
    total = 0
    for i in range(n):
        total += inc(i) + inc(i, 2) + apply(twice, i)
        if i == n // 2:
            inc.__defaults__ = (10,)
    out.append(total)
    kept = inc
    for i in range(n):
        if i == n // 4:
            inc = twice
        if i == n // 2:
            inc = abs
        if i == 3 * n // 4:
            inc = kept
        total += inc(-i)
    out.append(total)
    for f in (twice, abs, str, Point3, lambda v: [v], twice):
        try:
            total += len(str(apply(f, 5)))
        except TypeError as e:
            out.append(type(e).__name__)
    out.append(total)
    inc.__defaults__ = None
    try:
        inc(1)
    except TypeError as e:
        out.append(type(e).__name__)
    inc.__defaults__ = (1,)
    out.append((depth(200), depth(50)))
    return out


N = 3000
print("reads", reads(N))
print("writes", writes(N))
print("methods", methods(N))
print("globals", global_reads(N))
print("calls", calls(N))
