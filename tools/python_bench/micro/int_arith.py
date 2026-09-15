"""Int arithmetic: `% + * // -` and compares on small and i64-sized ints."""
import sys
import time

N = 1000000


def bench(n):
    total = 0
    seed = 12345
    hits = 0
    for i in range(n):
        total += i % 7
        seed = (seed * 1103515245 + 12345) % 2147483648
        if seed // 65536 < 16384:
            hits += 1
        total -= seed % 3
    return total, seed, hits


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("int_arith", N, result[0], result[1], result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
