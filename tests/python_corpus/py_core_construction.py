# Pending: tuple.__new__(cls, iterable) called through super() in a tuple subclass ignores
# the iterable and builds an empty tuple (split out of tests/python_corpus/construction.py).
class Pair(tuple):
    def __new__(cls, a, b):
        return super().__new__(cls, (a, b))
    @property
    def first(self):
        return self[0]
class Sorted(tuple):
    def __new__(cls, *items):
        return tuple.__new__(cls, sorted(items))
pr = Pair(1, "x")
print("tuple-sub", type(pr).__name__, len(pr), tuple(pr), pr == (1, "x"), hash(pr) == hash((1, "x")))
try:
    print("tuple-sub-index", pr[0], pr.first)
except IndexError as e:
    print("IndexError", e)
print("tuple-new-transform", Sorted(3, 1, 2), len(Sorted(5, 4)), Sorted())
