"""Builtin `hash()` of short strings (the checksum never prints hash values)."""
import sys
import time

N = 110000


def bench(n):
    words = ["hello", "world", "zipp", "python"]
    copies = ["".join(list(w)) for w in words]
    equal = 0
    for i in range(n):
        if hash(words[i % 4]) == hash(copies[i % 4]):
            equal += 1
    return equal


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("hash_str", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
