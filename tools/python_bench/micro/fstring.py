"""f-string formatting of an int into a short label, collected and joined."""
import sys
import time

N = 65000


def bench(n):
    total = 0
    last = ""
    for k in range(n // 1000):
        parts = []
        for i in range(1000):
            parts.append(f"item{i}")
        joined = "".join(parts)
        total += len(joined)
        last = parts[-1]
    return total, last


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("fstring", N, result[0], result[1])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
