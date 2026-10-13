# The truth of lists, tuples, dicts and sets (empty or not, and changing
# under the test), of their subclasses with their own `__bool__` or
# `__len__`, and of other containers, in `if`, `while`, `not`, `and`/`or`
# and conditional expressions, in loops long enough to compile: CPython's.
from collections import deque


class L(list):
    def __bool__(self):
        return True


class D(dict):
    def __len__(self):
        return 0


class S(set):
    pass


class T(tuple):
    def __len__(self):
        return 5


values = [[], [0], (), (0,), {}, {0: 0}, set(), {0}, frozenset(), frozenset([1]),
          L(), L([1]), D(), D(a=1), S(), S([1]), T(), T((1, 2)), deque(), deque([1]),
          range(0), range(3), "", "x", 0, 1, None, 0.0, b"", b"x"]
row = []
for i in range(3000):
    v = values[i % len(values)]
    a = 1 if v else 0
    b = 0 if not v else 1
    c = (v and "t") or "f"
    if i < len(values):
        row.append((a, b, c, bool(v)))
    if a != b:
        print("mismatch", i)
print(row)

# A list emptied by the loop that tests it, and a dict filled by one.
work = list(range(2500))
popped = 0
while work:
    work.pop()
    popped += 1
print(popped, work, bool(work))
seen = {}
n = 0
while not seen or len(seen) < 2000:
    seen[n] = n
    n += 1
print(n, len(seen))
stack = [[1, 2], [], [3], (), (4,)]
out = []
for i in range(3000):
    s = stack[i % 5]
    if s:
        out.append(len(s))
    elif not s:
        out.append(0)
print(sum(out), out[:10])
q = [i for i in range(10)]
total = 0
while q and total < 10 ** 6:
    total += q.pop(0)
    if len(q) < 3 and total < 1000:
        q.extend([1, 2, 3])
print(total, q)
