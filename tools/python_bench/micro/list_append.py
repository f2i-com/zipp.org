"""`list.append` into fresh 1000-element lists."""
import sys
import time

N = 350000


def bench(n):
    total = 0
    last = None
    for k in range(n // 1000):
        items = []
        for i in range(1000):
            items.append(i)
        total += len(items)
        last = items
    return total, last[-1]


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("list_append", N, result[0], result[1])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
