"""Builtin `isinstance()` against builtin types over mixed values."""
import sys
import time

N = 220000


def bench(n):
    values = [1, "a", 2.0, None]
    ints = 0
    strs = 0
    for i in range(n):
        x = values[i % 4]
        if isinstance(x, int):
            ints += 1
        elif isinstance(x, str):
            strs += 1
    return ints, strs


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("builtin_isinstance", N, result[0], result[1])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
