# isinstance and len at warm sites over values of many kinds, classes that
# change, subclasses of builtin types, and records of the same class with
# different layouts; method calls on builtin receivers at one site.


class A:
    pass


class B(A):
    pass


class L(list):
    pass


class D(dict):
    def __len__(self):
        return 99


class S(str):
    pass


vals = [1, True, 2.5, "s", None, b"x", bytearray(b"y"), A(), B(), L([1]), D(a=1), S("t"), (1,), [1, 2], {1: 2}, {3}]
kinds = [int, bool, float, str, type(None), bytes, bytearray, A, B, list, dict, tuple, set, object]
for _ in range(3):
    print([[isinstance(v, t) for t in kinds] for v in vals])
    print([isinstance(v, (A, list)) for v in vals])

sized = [[1, 2, 3], (1, 2), {1: 1, 2: 2}, {1, 2, 3, 4}, "hello", "héllo", "\U0001f600x", L([1, 2]), D(), S("abc"), b"xyz", bytearray(b"ab"), range(7), frozenset([1])]
for _ in range(3):
    print([len(v) for v in sized])

growing = []
out = []
for i in range(10):
    growing.append(i)
    d = {str(k): k for k in range(i)}
    out.append((len(growing), len(d), len(set(growing)), len(tuple(growing))))
print(out)


def methods(recv, n):
    out = []
    for i in range(n):
        r = recv[i % len(recv)]
        out.append(r.count(1) if not isinstance(r, str) else r.count("a"))
    return out


print(methods([[1, 1, 2], (1, 2, 1, 1), "banana", L([1, 1, 1, 1])], 8))


def strm(n):
    out = []
    for i in range(n):
        s = "Ab" * (i + 1)
        out.append((s.upper(), s.lower(), s.startswith("A"), s.find("b")))
    return out


print(strm(4))


class M:
    def __init__(self):
        self.v = 1


ms = [M(), M(), M()]
ms[1].extra = 2
ms[2].__dict__.clear()
ms[2].v = 3
print([m.v for m in ms] * 2)
del ms[0].v
ms[0].v = 5
print([m.v for m in ms] * 2)

for _ in range(2):
    print([isinstance(m, M) for m in ms], isinstance(M, type), isinstance(3, (str, (int, float))))
