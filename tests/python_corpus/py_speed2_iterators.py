# enumerate, zip and dict iteration through the native steps: starts,
# growth and shrinking during iteration, exhaustion that stays exhausted,
# dict size changes, views, next() and list() on them, and non-list inputs.
xs = [10, 20, 30]
print(list(enumerate(xs)), list(enumerate(xs, 5)), list(enumerate(xs, start=-2)), list(enumerate((1, 2), True)), list(enumerate([])))
out = []
for i, x in enumerate(xs):
    if i == 0:
        xs.append(40)
    out.append((i, x))
print(out)
e = enumerate([1, 2])
print(next(e), next(e), next(e, "end"))
ys = [1]
e = enumerate(ys)
print(list(e))
ys.append(2)
print(list(e), list(enumerate(ys)))
print(list(enumerate("ab")), list(enumerate(range(3), 10)), list(enumerate(x * 2 for x in range(3))), list(enumerate(iter([7]))))
big = enumerate([0], 2 ** 70)
print(next(big))
print(list(zip([1, 2, 3], "ab")), list(zip([1, 2], (3, 4), [5, 6, 7])), list(zip()), list(zip([1])), list(zip([], [1])))
a, b = [1, 2], [3]
z = zip(a, b)
print(next(z), next(z, "stop"))
b.append(4)
print(next(z, "still stopped"), list(zip(a, b)))
try:
    list(zip([1, 2], [3], strict=True))
except ValueError as err:
    print("ValueError:", err)
print(list(zip([1], [2], strict=True)))
d = {"a": 1, "b": 2, 3: "c"}
print(list(d), list(d.keys()), list(d.values()), list(d.items()), [k for k in d], sorted((v, k) for k, v in [("x", 1)]))
s = {"k%d" % i: i for i in range(5)}
print([(k, v) for k, v in s.items()], sum(s.values()), list(s)[::2])
for bad in ("add", "del"):
    t = {"a": 1, "b": 2}
    try:
        for k in t:
            if bad == "add":
                t["c"] = 3
            else:
                del t["a"]
    except RuntimeError as err:
        print("RuntimeError:", err)
    t = {"a": 1, "b": 2}
    try:
        for k, v in t.items():
            t["z" + k] = v
    except RuntimeError as err:
        print("RuntimeError items:", err)
t = {"a": 1}
for k in t:
    t[k] = 5
print(t)
it = iter({"x": 1, "y": 2}.items())
print(next(it), list(it), list(it))
v = {"p": 1}.values()
print(list(v), list(v), len(v))
nested = {i: {j: i * j for j in range(3)} for i in range(3)}
print([[x for x in inner.values()] for inner in nested.values()], dict(zip("abc", range(3))), dict(enumerate("xy")))
