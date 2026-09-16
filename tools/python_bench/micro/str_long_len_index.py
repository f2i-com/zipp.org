"""`len(s)` and `s[i]` on a 100,000-character string."""
import sys
import time

N = 140000


def bench(n):
    s = "abcdefghij" * 10000
    total = 0
    vowels = 0
    for i in range(n):
        total += len(s)
        if s[i % 100000] == "e":
            vowels += 1
    return total, vowels


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("str_long_len_index", N, result[0], result[1])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
