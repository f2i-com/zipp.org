# Iterating lists while mutating them (append, remove, insert, slice assignment),
# the runtime errors for dicts and sets that change size, and iterator exhaustion.
xs = [1, 2, 3]
seen = []
for x in xs:
    seen.append(x)
    if x < 5:
        xs.append(x + 3)
print("append-during", seen, xs)

xs = list(range(10))
visited = []
for x in xs:
    visited.append(x)
    if x % 2 == 0:
        xs.remove(x)
print("remove-during", visited, xs)

xs = [0, 1, 2, 3]
out = []
for i, x in enumerate(xs):
    out.append((i, x))
    if i == 1:
        xs.insert(0, "ins")
print("insert-during", out, xs)

xs = [1, 2, 3, 4, 5]
out = []
for x in xs:
    out.append(x)
    if x == 2:
        xs[:] = [10, 20]
print("slice-replace-during", out, xs)

xs = [1, 2, 3, 4]
out = []
for x in xs:
    out.append(x)
    xs.pop()
print("pop-during", out, xs)

xs = [3, 1, 2]
out = []
for x in xs:
    out.append(x)
    xs.sort()
print("sort-during", out, xs)

xs = [5, 6, 7]
it = iter(xs)
first = next(it)
xs.clear()
print("clear-iter", first, list(it))
xs = [1, 2]
it = iter(xs)
print("exhaust", list(it), list(it), next(it, "done"))

xs = [1, 2, 3]
copy_iter = []
for x in xs[:]:
    copy_iter.append(x)
    xs.append(x)
print("copy-iter", copy_iter, xs)

d = {"a": 1, "b": 2}
try:
    for k in d:
        d["c"] = 3
except RuntimeError as e:
    print("RuntimeError", e)
print("dict-after", d)
d = {"a": 1, "b": 2, "c": 3}
try:
    for k in d:
        del d[k]
except RuntimeError as e:
    print("RuntimeError", e)
d = {"a": 1, "b": 2}
for k in d:
    d[k] = d[k] * 10
print("dict-value-update-ok", d)
d = {"a": 1, "b": 2, "c": 3}
for k in list(d):
    if d[k] % 2:
        del d[k]
print("dict-list-copy", d)
s = {1, 2, 3}
try:
    for v in s:
        s.add(v + 10)
except RuntimeError as e:
    print("RuntimeError", e)
s = {1, 2, 3}
try:
    for v in s:
        s.discard(v)
except RuntimeError as e:
    print("RuntimeError", e)
d = {"x": 1, "y": 2}
for k, v in d.items():
    d[k] = v + 1
print("items-value-update-ok", d, [v for v in d.values()])

queue = [1]
processed = 0
for item in queue:
    processed += 1
    if item < 20:
        queue.append(item * 2)
        queue.append(item * 3)
    if len(queue) > 30:
        break
print("worklist", processed, len(queue), queue[:10])
matrix = [[1, 2], [3, 4]]
for row in matrix:
    row.append(sum(row))
print("inner-mutation", matrix)
nums = [1, 2, 3, 4, 5, 6]
i = 0
while i < len(nums):
    if nums[i] % 3 == 0:
        nums.pop(i)
    else:
        nums[i] *= 2
        i += 1
print("while-index", nums)
gen_src = [1, 2, 3]
gen = (x * 2 for x in gen_src)
gen_src.append(4)
print("genexp-late", list(gen))
tup = (1, 2)
acc = []
for x in tup:
    tup = tup + (x,)
    acc.append(x)
print("rebind-tuple", acc, tup)
