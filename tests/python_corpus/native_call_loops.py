# Loops whose every iteration calls a native (a str method, hash, heapq,
# bisect, math.sqrt): long enough to compile, with the callee at a site
# changing to a Python function and back mid-loop, natives that raise in
# the loop, receivers of other kinds at the same site, and non-ASCII text.
import bisect
import heapq
import math


class Shout:
    def __init__(self, text):
        self.text = text

    def upper(self):
        return self.text + "!"

    def strip(self):
        return self

    def split(self, sep):
        return [self.text, sep]


def str_loop(n):
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


def mixed_receivers(n):
    things = ["  pad  ", Shout("hey"), "été ", Shout("yo"), "x,y"]
    out = []
    for i in range(n):
        t = things[i % 5]
        s = t.strip()
        u = s.upper()
        if i < 10:
            out.append((u, type(s).__name__))
    return out


def hash_loop(n):
    words = ["hello", "world", "zipp", "python", "über", ""]
    copies = ["".join(list(w)) for w in words]
    equal = 0
    for i in range(n):
        if hash(words[i % 6]) == hash(copies[i % 6]):
            equal += 1
    return equal


def seven(s):
    return 7


def rebound_hash(n):
    global hash
    seen = 0
    for i in range(n):
        seen += hash("k") == hash("k")
        if i == 400:
            hash = seven
        if i == 900:
            del hash
    return seen


def heap_loop(n):
    heap = []
    out = 0
    for i in range(n):
        heapq.heappush(heap, (i * 7919) % 1009)
        if i % 3 == 2:
            out += heapq.heappop(heap)
    return out, len(heap), sorted(heap)[:5], heap[0]


def bisect_loop(n):
    xs = []
    for i in range(n):
        bisect.insort(xs, (i * 31) % 257)
    hits = 0
    for i in range(n):
        hits += bisect.bisect_left(xs, i % 300) + bisect.bisect_right(xs, i % 300)
    return hits, xs[:8], xs[-3:]


def sqrt_loop(n):
    total = 0.0
    bad = 0
    for i in range(n):
        try:
            total += math.sqrt(i - 5 if i % 250 == 0 else i)
        except ValueError as e:
            bad += 1
            if bad == 1:
                print("sqrt", e)
    return round(total, 6), bad


def wrong_args(n):
    errors = []
    s = "abc"
    for i in range(n):
        try:
            if i == 500:
                s.startswith(5)
            elif i == 600:
                s.replace("a")
            elif i == 700:
                "-".join([s, 5])
            elif i == 800:
                hash([1])
            else:
                s.startswith("a")
        except TypeError as e:
            errors.append(type(e).__name__)
    return errors


N = 3000
print("str", str_loop(N))
print("mixed", mixed_receivers(N))
print("hash", hash_loop(N))
print("rebound", rebound_hash(N), hash("k") == hash("k"))
print("heap", heap_loop(N))
print("bisect", bisect_loop(N))
print("sqrt", sqrt_loop(N))
print("wrong", wrong_args(N))
