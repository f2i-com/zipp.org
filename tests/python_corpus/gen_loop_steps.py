# `for` loops over generators, long enough to compile: a plain generator,
# one that raises mid-way (caught inside and outside the loop), one with a
# finally, one left by `break` and resumed, a generator re-entered from its
# own body, generators nested deeper than compiled code nests its calls,
# and other iterables at the same loop.
def count(n):
    i = 0
    while i < n:
        yield i
        i += 1


def raising(n, at):
    for i in range(n):
        if i == at:
            raise ValueError("at %d" % i)
        yield i


def guarded(n, log):
    try:
        for i in range(n):
            yield i * 2
    finally:
        log.append("closed %d" % n)


def walk(depth):
    if depth:
        for x in walk(depth - 1):
            yield x
    yield depth


def total(it):
    s = 0
    for v in it:
        s += v
    return s


def plain(n):
    s = 0
    for k in range(n // 1000):
        for v in count(1000):
            s += v
    return s


def caught_inside(n):
    hits = 0
    s = 0
    for k in range(n // 100):
        try:
            for v in raising(100, 60):
                s += v
        except ValueError as e:
            hits += 1
            last = str(e)
    return s, hits, last


def caught_outside(n):
    try:
        return total(raising(n, n - 3))
    except ValueError as e:
        return "outside " + str(e)


def broken(n):
    g = count(n)
    first = 0
    for v in g:
        first += v
        if v == n // 2:
            break
    rest = 0
    for v in g:
        rest += v
    again = total(g)
    return first, rest, again


def finals(n):
    log = []
    s = total(guarded(n, log))
    g = guarded(5, log)
    for v in g:
        if v == 4:
            break
    g.close()
    return s, log


def reenter():
    def gen():
        yield 1
        try:
            next(me)
        except ValueError as e:
            yield str(e)
        yield 3

    me = gen()
    return [v for v in me]


def mixed(n):
    sources = [count(50), [1, 2, 3], (4, 5), range(6), {7: 0, 8: 0}, "ab", (x * x for x in range(40)), iter([9, 10])]
    seen = []
    for i in range(n):
        src = sources[i % len(sources)]
        for v in src:
            seen.append(v)
    return len(seen), seen[:12], seen[-4:]


N = 6000
print("plain", plain(N))
print("inside", caught_inside(N))
print("outside", caught_outside(N))
print("broken", broken(N))
print("finals", finals(N))
print("reenter", reenter())
print("walk", total(walk(60)), list(walk(4)), sum(1 for _ in walk(150)))
print("mixed", mixed(N))
print("genexp", sum(x * 2 for x in count(N)), max(v for v in count(N) if v % 7 == 0), sorted(v % 5 for v in count(40))[:8])
