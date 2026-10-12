# Tuples as dict keys and set members: equal tuples of equal but different
# numbers, nested tuples, tuples holding strs, None and frozensets, tuple
# subclasses (namedtuple), and unhashable contents, in loops long enough to
# be compiled.
from collections import namedtuple

P = namedtuple("P", "a b")


def counts(n):
    d = {}
    for i in range(n):
        k = (i % 5, ("s%d" % (i % 3), None), frozenset([i % 2]))
        d[k] = d.get(k, 0) + 1
    return sorted((repr(k), v) for k, v in d.items())[:4], len(d)


print(counts(3000))
d = {(1, 2): "int", ((1, 2), 3): "nested"}
print(d[(1.0, 2.0)], d[(True, 2)], d.get(((1.0, 2), 3.0)), (1, 2) in d, (2, 1) in d)
s = set()
for i in range(1000):
    s.add((i % 7, i % 3))
    s.add(P(i % 7, i % 3))
print(len(s), (3, 1) in s, P(3, 1) in s, P(9, 9) in s)
dp = {P(1, 2): "p"}
print(dp[(1, 2)], dp.get(P(a=1, b=2)), {(1, [2]): 0} if False else "skip")
try:
    {(1, [2]): 0}
except TypeError as e:
    print("TypeError", e)
print(hash((1, 2)) == hash((1.0, 2.0)), hash(()) == hash(()), (1, (2, 3)) == (1.0, (2.0, 3)))
