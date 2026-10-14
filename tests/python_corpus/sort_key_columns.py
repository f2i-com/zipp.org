# Sorts whose keys are all ints, all floats, all strs, or tuples of those
# (compared column by column), beside sorts whose keys are not of one form
# (mixed kinds, NaN, ragged or nested tuples, big ints, None): the order,
# its stability and the errors must be CPython's.

data = [(i * 7919) % 101 - 50 for i in range(300)]
print(sorted(data)[:12], sorted(data, reverse=True)[:6])
print(sorted(range(20), key=lambda v: v % 3))
print(sorted(range(20), key=lambda v: (v % 3, -v)))
print(sorted(range(20), key=lambda v: (v % 2, str(v % 5), v // 7)))
print(sorted([True, 1, 0, False, 2, -1, True]))
words = ["pear", "Apple", "fig", "apple", "Fig", "é", "e", "z", "", "pear"]
print(ascii(sorted(words)), ascii(sorted(words, key=len)), ascii(sorted(words, key=lambda w: (len(w), w))))
print(ascii(sorted(words, key=str.lower)))
floats = [3.5, -0.0, 0.0, -2.25, 1e300, -1e-300, 0.0, 7.0]
print(sorted(floats), sorted(floats, key=lambda f: (f > 0, f)))
pairs = [(i % 4, "k%d" % (i % 3), i * 0.5) for i in range(30)]
print(sorted(pairs)[:8])
print(sorted(pairs, key=lambda p: (p[1], -p[0]))[:8])

# Keys that are not of one form: compared as before.
print(sorted([3, 1.5, 2, -0.5, 10 ** 30, -(10 ** 40)]))
nan = float("nan")
print(sorted([2.0, nan, 1.0, 0.5])[0:1] != [], len(sorted([2.0, nan, 1.0])))
print(sorted([(1, 2), (1,), (0, 5, 5), (1, 2, 0), ()]))
print(sorted([(1, (2, 3)), (1, (2, 2)), (0, (9, 9))]))
print(sorted([("a", 1), ("a", 0), ("b", -1)], key=lambda t: t))
for bad in ([1, "a"], [(1, "a"), (1, 2)], [None, None], [(None, 1), (None, 0)]):
    try:
        print(sorted(bad))
    except TypeError as e:
        print("TypeError", str(e)[:40])

# Stability: equal keys keep their order.
recs = [("r%d" % (i % 5), i) for i in range(40)]
recs.sort(key=lambda r: r[0])
print(recs[:10])
recs.sort(key=lambda r: (r[1] % 3, r[0]))
print(recs[:10])
counts = {"te": 5, "no": 3, "len": 5, "ka": 3, "zu": 1}
print(sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])))

# Larger sorts, in a loop long enough to compile.
total = 0
for k in range(200):
    xs = [(j * 31 + k) % 97 for j in range(60)]
    ys = sorted(xs, key=lambda v: (v % 7, v))
    total += ys[0] * 3 + ys[-1] + sorted(xs)[30]
print(total)
