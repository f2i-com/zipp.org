# str subscripts and len() through the inline paths: ASCII and non-ASCII
# text, astral characters, negative and out-of-range indices, bool and
# __index__ keys, str subclasses, and the other containers len() takes.
s = "abcdefghij" * 3
print(len(s), s[0], s[9], s[-1], s[-30], s[29], s[True], s[False])
for i in (30, -31, 100, -100):
    try:
        print(s[i])
    except IndexError as e:
        print("IndexError:", e)
for bad in ("x", 1.0, None):
    try:
        s[bad]
    except TypeError as e:
        print("TypeError", type(bad).__name__)


class Idx:
    def __index__(self):
        return 3


print(s[Idx()], s[2:5], s[::-7], s[-3:], s[100:])
u = "h\u00e9llo w\u00f6rld \u4e2d\u6587"
print(len(u), u[1], u[-1], u[-2], u[12], [u[i] for i in range(len(u))] == list(u))
a = "x\U0001F600y\U0001F680z"
print(len(a), a[1] == "\U0001F600", a[2], a[3] == "\U0001F680", a[-1], a[-2] == "\U0001F680", len(a[1]))
for i in (5, -6):
    try:
        print(a[i])
    except IndexError as e:
        print("IndexError:", e)
e = ""
print(len(e))
try:
    e[0]
except IndexError as ex:
    print("IndexError:", ex)
c = "\x00\x7f\x01"
print(len(c), ord(c[0]), ord(c[1]), ord(c[-1]))


class MyStr(str):
    def __len__(self):
        return 42

    def __getitem__(self, k):
        return "item%r" % (k,)


m = MyStr("hello")
print(len(m), m[0], m[-1], str.__len__(m), str.__getitem__(m, 1))


class Plain(str):
    pass


p = Plain("plain")
print(len(p), p[0], p[-1], type(p[0]).__name__)
print(len([1, 2, 3]), len((1,)), len({"a": 1, "b": 2}), len({1, 2, 3}), len(range(10)), len(frozenset([1])), len(b"xyz"), len({}.keys()))


class Sized:
    def __len__(self):
        return 7


print(len(Sized()))
try:
    len(5)
except TypeError as ex:
    print("TypeError:", ex)
big = "q" * 100000 + "Z"
total = 0
for i in range(0, 100001, 997):
    total += ord(big[i])
print(total, big[-1], len(big))
t = (1, 2, 3)
print(t[0], t[-1], [10, 20][1])
d = {"k": "v"}
print(d["k"])
len = lambda x: "shadowed"
print(len("abc"))
del len
print(len("abc"))
