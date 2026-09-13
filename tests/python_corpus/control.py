# Control flow, scoping, iteration protocols, misc statements.
for i in range(3):
    if i == 1:
        continue
    print("i", i)
else:
    print("for-else")

n = 0
while n < 10:
    n += 1
    if n % 2:
        continue
    if n > 6:
        break
    print("n", n)
else:
    print("not printed")

for i in range(2):
    for j in range(3):
        if j == 2:
            break
        print(i, j)


class Fib:
    def __init__(self, limit):
        self.limit = limit

    def __iter__(self):
        a, b = 0, 1
        while a < self.limit:
            yield a
            a, b = b, a + b


class Countdown:
    def __init__(self, n):
        self.n = n

    def __iter__(self):
        return self

    def __next__(self):
        if self.n <= 0:
            raise StopIteration
        self.n -= 1
        return self.n + 1


class Squares:
    def __getitem__(self, i):
        if i >= 5:
            raise IndexError
        return i * i


print(list(Fib(30)), list(Countdown(3)), list(Squares()), 9 in Squares(), sum(Squares()))
it = iter([1, 2, 3])
print(next(it), next(it), next(it), next(it, "end"))
try:
    next(it)
except StopIteration:
    print("exhausted")
it2 = iter(lambda: "x", "x")
print(list(it2))

x = 10
if x > 5:
    kind = "big"
elif x > 2:
    kind = "medium"
else:
    kind = "small"
print(kind, "pos" if x > 0 else "neg", (x > 5) * 2, [1, 2][x > 5])
if (y := x * 2) > 15:
    print("walrus", y)
print([w for w in ["a", "bb"] if (m := len(w)) > 1], m)

value = None
print(value is None, value is not None, value == None, type(value).__name__)
a = b = c = [0]
a.append(1)
print(a, b, c, a is c)
a = b = 5
print(a, b)
s = "global"


def scopes():
    s = "local"

    def inner():
        return s
    return inner()


print(scopes(), s)


def shadow():
    print(len("abc"))
    len = 5
    return len


try:
    shadow()
except UnboundLocalError as e:
    print("UnboundLocalError:", "len" in str(e))

del a
try:
    print(a)
except NameError as e:
    print("NameError:", e)

items = [3, 1, 2]
items[0], items[2] = items[2], items[0]
print(items)
d = {"k": [1]}
d["k"] += [2]
d["k"][0] += 10
print(d)
matrix = [[0] * 3 for _ in range(2)]
matrix[0][1] = 5
print(matrix)
alias = [[0] * 3] * 2
alias[0][1] = 5
print(alias)
i = 0
i += 1; i *= 5; i -= 2; i //= 2; i **= 3; i %= 5
print(i)
f = 1.0
f /= 4
print(f)
t = (1, 2)
t += (3,)
print(t)
st = "a"
st += "b"
st *= 2
print(st)
bits = 0b1100
bits |= 0b0011; bits &= 0b1010; bits ^= 0b1111; bits <<= 2; bits >>= 1
print(bits, ~bits, -bits, +bits, 5 & 3, 5 | 3, 5 ^ 3, 1 << 10, 1024 >> 3, ~0)
print(type(1) == int, type("") is str, isinstance(True, int), isinstance(1, (float, int)), callable(len), callable(1), callable(lambda: 0))
print(id(1) == id(1), hash("a") == hash("a"), hash(1) == hash(1.0), hash((1, 2)) == hash((1, 2)))
print(list(range(5))[slice(1, 3)], slice(1, 5, 2).indices(10), "abc"[slice(None, None, -1)])
print(divmod(17, 5), divmod(-17, 5), 17 % -5, -17 // 5, 2 ** 3 ** 2, (2 ** 3) ** 2, -3 ** 2, (-3) ** 2, 10 / 4, 10 // 4, 10.0 // 4)
print(1 if True else 2 if False else 3, (1, 2) < (1, 3), [1, 2] > [1], "b" > "a", (1, "a") == (1, "a"))
counter = 0


def side_effect():
    global counter
    counter += 1
    return counter


print(side_effect() or side_effect(), counter, side_effect() and side_effect(), counter, 0 and side_effect(), counter)
print(sorted({"b": 1, "a": 2}.items()), dict(sorted({"b": 1, "a": 2}.items())), list(map(len, ["ab", "c"])), [*range(3), *"ab"], {*[1, 2], 3}, (*[1], 2))
pass
print(globals()["counter"], "counter" in globals(), __name__)
print([x for x in range(10) if x % 2 == 0 and x > 2], list(filter(lambda v: v > 1, {1: "a", 2: "b"})), {k: v for k, v in zip("ab", [1, 2]) if v > 1})
for idx, (k, v) in enumerate({"x": 1, "y": 2}.items()):
    print(idx, k, v)
gen = (i * i for i in range(3))
print(type(gen).__name__, list(gen))
total = 0
for chunk in [range(3), [10, 20], (100,)]:
    for v in chunk:
        total += v
print(total)
print(str(1 == 1), repr(None), bool, int, float, str, list, dict, tuple, set, type, object)
