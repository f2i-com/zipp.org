"""Raise and catch: a missing dict key raising `KeyError` caught by `except`."""
import sys
import time

N = 110000


def bench(n):
    d = {"present": 1}
    caught = 0
    for i in range(n):
        try:
            d["absent"]
        except KeyError:
            caught += 1
    return caught


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("try_raise_catch", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
