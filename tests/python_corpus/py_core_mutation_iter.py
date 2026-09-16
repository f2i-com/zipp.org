# Pending divergences split out of tests/python_corpus/mutation_iter.py.
# 1. An exhausted list iterator stays exhausted after the list grows.
xs = [1, 2]
it = iter(xs)
print("exhaust", list(it))
xs.append(3)
print("after-exhaust", list(it), next(it, "done"))
# 2. Changing a dict's size while iterating items() or values() raises RuntimeError.
d = {"x": 1}
try:
    for k, v in d.items():
        d["y"] = 2
    print("items no error", d)
except (RuntimeError, KeyError) as e:
    print(type(e).__name__, e)
d = {"x": 1}
try:
    for v in d.values():
        d.pop("x")
    print("values no error", d)
except (RuntimeError, KeyError) as e:
    print(type(e).__name__, e)
d = {"x": 1, "z": 0}
try:
    for k in d.keys():
        del d["z"]
    print("keys() no error", d)
except (RuntimeError, KeyError) as e:
    print(type(e).__name__, e)
