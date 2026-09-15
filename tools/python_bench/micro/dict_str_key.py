"""Dict subscript get and set with `str` keys: `d[k] = d[k] + 1`."""
import sys
import time

N = 450000


def bench(n):
    keys = ["key%d" % j for j in range(64)]
    d = {}
    for k in keys:
        d[k] = 0
    for i in range(n):
        k = keys[i % 64]
        d[k] = d[k] + 1
    return d["key0"], d["key63"], len(d)


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("dict_str_key", N, result[0], result[1], result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
