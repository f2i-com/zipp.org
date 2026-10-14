# Subscript reads and stores on lists and tuples (and things that are not
# plain lists or tuples) through one site, in loops long enough to compile:
# the values, the stores and the errors must be CPython's.


class L(list):
    def __getitem__(self, i):
        return ("sub", list.__getitem__(self, i))

    def __setitem__(self, i, v):
        list.__setitem__(self, i, v * 10)


class T(tuple):
    pass


grid = [[i * 3 + j for j in range(3)] for i in range(3)]
seqs = [[1, 2, 3], (4, 5, 6), L([7, 8, 9]), T((10, 11, 12)), "abc", range(3), {0: "d0", 1: "d1", 2: "d2", -1: "m1", -2: "m2", -3: "m3"}]
got = []
for i in range(3000):
    s = seqs[i % 7]
    got.append(s[i % 3])
    got.append(s[-1 - i % 3])
    if i % 7 == 2:
        s[i % 3] = i
    if i % 7 == 0:
        s[i % 3] = s[(i + 1) % 3] + 1
    grid[i % 3][(i * 7) % 3] += grid[(i + 1) % 3][i % 3]
print(got[:16], got[-8:], len(got))
print(seqs[0], seqs[2], grid)

errors = []
for i in range(2000):
    s = seqs[i % 4]
    for k in (3, -4, True, 1.0, "1"):
        try:
            s[k]
        except (IndexError, TypeError) as e:
            errors.append(type(e).__name__)
        try:
            if i % 4 == 0:
                s[k] = 0
        except (IndexError, TypeError) as e:
            errors.append("set" + type(e).__name__)
print(len(errors), errors[:12], seqs[0])

# A list that grows and shrinks under one site.
xs = []
total = 0
for i in range(3000):
    xs.append(i)
    if len(xs) > 5:
        xs.pop(0)
    total += xs[len(xs) - 1] - xs[0] + xs[-1]
    xs[0] = xs[-1]
print(total, xs)
