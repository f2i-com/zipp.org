"""Naive doubly recursive Fibonacci: call-heavy int code."""
import sys
import time

N = 28


def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)


t0 = time.perf_counter()
result = fib(N)
elapsed = time.perf_counter() - t0
print("fib_recursive", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
