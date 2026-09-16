# list.sort while the key or a comparison mutates the list, pow() with a
# negative exponent and a modulus, the sign of a zero float remainder, and
# bound-method equality and hashing.
l = [3, 2, 1]
def k(x):
    l.append(0)
    return x
try:
    l.sort(key=k)
except ValueError as e:
    print('VE', e)
print(l)

m = [3, 2, 1]
def k2(x):
    m.clear()
    return x
try:
    m.sort(key=k2)
    print("cleared", m)
except ValueError as e:
    print('VE2', e, m)

n = [3, 1, 2]
def k3(x):
    print("len during sort", len(n), n)
    return x
n.sort(key=k3)
print(n)

o = [3, 1, 2]
def bad(x):
    if x == 1:
        raise KeyError("boom")
    return x
try:
    o.sort(key=bad)
except KeyError as e:
    print("KeyError", e, o)

class C:
    def __init__(self, v): self.v = v
    def __lt__(self, other):
        p.append(C(0))
        return self.v < other.v
    def __repr__(self): return "C%d" % self.v
p = [C(2), C(1)]
try:
    p.sort()
except ValueError as e:
    print("VE3", e, p)
q = [1, "a"]
try:
    q.sort()
except TypeError as e:
    print("TypeError", e, q)
print(sorted([3, 1, 2], reverse=True))
for args in [(3, -1, 7), (3, -2, 7), (3, -1, -7), (-3, -1, 7), (2, -1, 4), (5, 3, -7), (5, 0, -7), (0, -1, 5), (7, -1, 1), (7, -1, -1), (12345678901234567890, -3, 1000000007)]:
    try:
        print(args, pow(*args))
    except ValueError as e:
        print(args, "ValueError", e)
print(-7.5 % 0.5, 7.5 % -0.5, -1.0 % 1.0, 1.0 % -1.0, 0.0 % 3.0, -0.0 % 3.0, 3.0 % float("inf"))
try:
    pow(2.0, 2, 3)
except TypeError as e:
    print("TypeError", e)
class W:
    def handler(self): pass
w = W()
cbs = [w.handler]
try:
    cbs.remove(w.handler); print("removed", len(cbs))
except ValueError as e: print("VE", e)
print(w.handler == w.handler, {w.handler: 1}.get(w.handler), w.handler in [w.handler])
print(hash(w.handler) == hash(w.handler), w.handler != w.handler)
v = W(); print(w.handler == v.handler, len({w.handler, w.handler}))
l = [1]
print(l.append == l.append, [].append == [].append)
