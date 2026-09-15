"""Common `str` methods: split, strip, lower/upper, startswith, replace, join, find."""
import sys
import time

N = 30000


def bench(n):
    line = "  Alpha,beta,GAMMA,delta  "
    total = 0
    last = ""
    for i in range(n):
        fields = line.strip().split(",")
        lowered = fields[2].lower()
        if fields[0].startswith("Al"):
            total += len(lowered)
        last = "-".join([fields[1].upper(), lowered.replace("m", "n")])
        total += last.find("GAN") + 1
    return total, last


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("str_methods", N, result[0], result[1])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
