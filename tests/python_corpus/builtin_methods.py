# Methods of builtin types called directly and through subclasses that override them;
# the specializations for list.append, dict.get, str.join must respect overrides.
xs = []
for i in range(10):
    xs.append(i * i)
xs.extend(range(3))
xs.insert(2, -1)
print("list", xs, xs.pop(), xs.pop(0), xs.index(9), xs.count(1), len(xs))
xs.remove(-1)
xs.sort(reverse=True)
print("list-sort", xs, xs.copy() == xs, xs.clear(), xs)
d = {}
for w in "the cat and the hat and the bat".split():
    d[w] = d.get(w, 0) + 1
print("dict-get", d, d.get("zzz"), d.get("zzz", "dflt"), d.setdefault("new", []), d.pop("new"), sorted(d.keys()))
print("dict-methods", list(d.items())[:2], d.pop("cat"), d.popitem(), len(d), d.copy() == d, dict.fromkeys("ab", 0))
parts = [str(i) for i in range(5)]
print("str-join", ",".join(parts), "".join(parts), " - ".join(("a", "b")), "x".join("abc"), "".join([]))
s = "Hello World"
print("str", s.lower(), s.split(), s.replace("o", "0"), s.startswith("He"), s.find("World"), s.upper().count("L"), s.encode()[:3], s.zfill(13))
print("unbound-builtin", str.upper("abc"), str.join("-", ["p", "q"]), dict.get({"k": 1}, "k"), list.__len__([1, 2]))
join = "/".join
app = xs.append
get = d.get
for i in range(3):
    app(i)
print("stored-builtin-methods", join(["a", "b"]), xs, get("the"), get("nope", -1))


class LoggingList(list):
    def __init__(self, *args):
        super().__init__(*args)
        self.log = []

    def append(self, x):
        self.log.append(("append", x))
        super().append(x * 10)

    def __getitem__(self, i):
        self.log.append(("get", i))
        return super().__getitem__(i)

    def __len__(self):
        return super().__len__() + 100


ll = LoggingList([1, 2])
ll.append(3)
for i in range(2):
    ll.append(i)
print("list-subclass", list(ll), ll[0], ll[-1], len(ll), ll.log, isinstance(ll, list), type(ll).__name__)
print("list-subclass-builtins", sum(ll), sorted(ll), ll + [9], ll * 1 == list(ll), 30 in ll, ll.count(30), ll.index(30))


class DefaultGet(dict):
    def get(self, key, default="SUB-DEFAULT"):
        return super().get(key, default)

    def __missing__(self, key):
        return f"missing:{key}"


dg = DefaultGet(a=1)
print("dict-subclass", dg.get("a"), dg.get("b"), dg["a"], dg["zz"], "zz" in dg, len(dg), dict.get(dg, "b"))


class GetItemDict(dict):
    def __getitem__(self, key):
        return "overridden-" + str(key)


gd = GetItemDict(k="real")
print("dict-getitem-override", gd["k"], gd.get("k"), list(gd.values()), list(gd.items()), gd.copy())


class CountingDict(dict):
    def __setitem__(self, key, value):
        super().__setitem__(key, value + 1)


cd = CountingDict()
cd["a"] = 1
cd.update(b=1)
cd.setdefault("c", 1)
print("setitem-override", sorted(cd.items()))


class Stack(list):
    def push(self, x):
        self.append(x)
        return self

    def peek(self):
        return self[-1]


st = Stack()
for ch in "abc":
    st.push(ch)
print("stack", st, st.peek(), st.pop(), st, len(st), Stack([1, 2]) == [1, 2])
methods = [list.append, list.pop]
tmp = [1, 2, 3]
methods[0](tmp, 4)
print("unbound-list-method", tmp, methods[1](tmp), tmp)
print("method-on-literals", [3, 1, 2].index(2), (1, 2, 1).count(1), {1: 2}.items(), "a,b".split(","), b"x y".split(), {1, 2}.union({3}))
text = "  mixed Case text  "
chain = text.strip().lower().replace(" ", "_").split("_")
print("chained", chain, "_".join(reversed(chain)).title(), text.strip().center(20, "*"))
nums = list(range(20))
total = 0
for n in nums:
    if n % 2:
        total += len(str(n).zfill(3))
print("hot-str-method", total, ",".join(map(str, nums[:5])), sum(map(int, "1 2 3".split())))
