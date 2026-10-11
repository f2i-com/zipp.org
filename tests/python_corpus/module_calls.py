# `name.attr(args)` where `name` is bound by an import: module functions
# called directly, the name rebound to a non-module, a module attribute
# replaced, deleted and added mid-loop, aliased imports, and instances or
# strs at the same kind of site, in loops long enough to be compiled.
import math
import math as m
import os.path
import json
import string


class FakeMath:
    def sqrt(self, x):
        return ("fake", x)


def roots(n):
    xs = [4.0, 9.0, 16.0]
    out = []
    for i in range(n):
        out.append(math.sqrt(xs[i % 3]) + m.floor(xs[i % 3] / 2))
    return out[-3:], len(out)


print(roots(500))


def patched(n):
    out = []
    for i in range(n):
        if i == 100:
            math.hypot2 = lambda a, b: a * a + b * b
        if i == 200:
            math.isqrt_saved = math.isqrt
            math.isqrt = lambda v: -v
        if i == 300:
            math.isqrt = math.isqrt_saved
            del math.hypot2
        try:
            h = math.hypot2(i, 1)
        except AttributeError as e:
            h = str(e)
        out.append((math.isqrt(i), h))
    return out[99:102], out[199:202], out[299:302], out[-1]


print(patched(400))


def rebound(n):
    global math
    out = []
    saved = math
    for i in range(n):
        if i == 5:
            math = FakeMath()
        if i == 10:
            math = "str"
        if i == 15:
            math = saved
        try:
            out.append(math.sqrt(16))
        except AttributeError as e:
            out.append(str(e))
    return out


print(rebound(20))


def mixed(n):
    out = []
    for i in range(n):
        out.append((os.path.basename("a/b" + str(i % 3)), json.dumps([i % 2]), string.capwords("ab cd"), math.copysign(1.0, -i)))
    return out[-2:]


print(mixed(300))


def local_shadow():
    math = FakeMath()
    return [math.sqrt(i) for i in range(3)]


print(local_shadow())
try:
    math.nosuch(1)
except AttributeError as e:
    print(e)
try:
    math.sqrt(-1.0)
except ValueError as e:
    print(e)
