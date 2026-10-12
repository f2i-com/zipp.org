# `o[k] = v` and `o[k]` on lists and dicts in loops long enough to be
# compiled: in-range, negative and out-of-range indices, aliased and nested
# lists, augmented assignment, values of every kind (including big ints
# and objects the collector must keep), dict keys added and replaced, and
# the errors.


class Box:
    def __init__(self, v):
        self.v = v


def grid(n):
    g = [[0] * 4 for _ in range(4)]
    alias = g[1]
    keep = [None, None]
    for i in range(n):
        r = g[i % 4]
        r[i % 3] += i
        r[-1] = r[0] - r[1]
        keep[i % 2] = Box(i)
        alias[3] += 1
    return [row[:2] + [row[3]] for row in g], keep[0].v + keep[1].v


print(grid(4000))


def big_values(n):
    xs = [0] * 8
    for i in range(n):
        xs[i % 8] = 2 ** 64 + i
        xs[(i + 3) % 8] = xs[i % 8] * 2 - i
    return xs[0] % 1000003, xs[5] % 1000003


print(big_values(3000))


def dicts(n):
    d = {}
    for i in range(n):
        k = "k%d" % (i % 50)
        d[k] = d.get(k, 0) + i
        d[i % 7] = [i]
        d[i % 7][0] += 1
    return len(d), d["k3"], d[3]


print(dicts(3000))


def errors():
    xs = [1, 2, 3]
    out = []
    for k in (3, -4, 5, -9):
        try:
            xs[k] = 0
        except (IndexError, TypeError) as e:
            out.append((type(e).__name__, str(e)))
    t = (1, 2)
    try:
        t[0] = 5
    except TypeError as e:
        out.append(str(e))
    xs[-1] = 9
    return out, xs


print(errors())


def shared(n):
    a = [0, 0, 0]
    b = a
    c = [a, a]
    for i in range(n):
        b[i % 3] = i
        c[1][(i + 1) % 3] += 1
    return a, b is a, c[0] is a


print(shared(1000))
