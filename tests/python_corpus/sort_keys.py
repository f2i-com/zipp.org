# sorted() and list.sort() with key functions of every kind: lambdas,
# functions with defaults, builtins, bound methods, classes, callables,
# keys that raise, keys that see the list emptied, reverse and stability,
# short and long inputs, keys the engine orders natively and keys it does
# not.


def lcg(n, seed=7):
    out = []
    for _ in range(n):
        seed = (seed * 1103515245 + 12345) % 2147483648
        out.append(seed // 256 % 1000)
    return out


data = lcg(300)
short = data[:20]
words = ["pear", "Apple", "fig", "banana", "Cherry", "date", "elder", "Fig"]


def mod7(v, m=7):
    return v % m


class Box:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v < other.v

    def __repr__(self):
        return "Box(%d)" % self.v


class Neg:
    def __call__(self, v):
        return -v


for seq in (short, data):
    print(sorted(seq, key=lambda v: (v % 10, -v))[:8])
    print(sorted(seq, key=mod7)[:8], sorted(seq, key=mod7, reverse=True)[:8])
    print(sorted(seq, key=Neg())[:5], sorted(seq, key=str)[:5], sorted(seq, key=Box)[:3])
    print(sorted(seq, key=lambda v: Box(v % 5))[:6])
print(sorted(words, key=str.lower), sorted(words, key=len), sorted(words, key=len, reverse=True))
print(sorted(words, key="xyz".find), sorted(words, key=lambda w: w[-1]))
recs = [("r%d" % (v % 13), v) for v in data]
recs.sort(key=lambda r: r[0])
print(recs[:5], recs[-3:])
mod7.__defaults__ = (5,)
print(sorted(short, key=mod7)[:8])


def boom(v):
    if v == data[150]:
        raise ValueError("bad key %d" % v)
    return v


lst = list(data)
try:
    lst.sort(key=boom)
except ValueError as e:
    print(e, lst == data)


def peek(v):
    return len(lst2) * 0 + v


lst2 = list(short)
lst2.sort(key=peek)
print(lst2[:5])
print(sorted([], key=len), sorted([3], key=lambda v: 1 / 0 if v != 3 else 0))
try:
    sorted([1, 2], key=lambda v: 1 / 0)
except ZeroDivisionError as e:
    print("ZeroDivisionError", e)
try:
    sorted([1, "a"], key=lambda v: v)
except TypeError as e:
    print("TypeError", e)
