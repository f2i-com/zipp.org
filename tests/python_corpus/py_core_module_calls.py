# Calls through a module attribute (`mod.name(args)`): builtins, Python
# functions with and without defaults, classes, keywords, star arguments,
# rebinding a module global, and the error paths.
import argparse
import collections
import functools
import heapq
import inspect
import itertools
import json
import math
import math as m2
import pickle

print(math.sqrt(16.0), m2.floor(2.5), math.isclose(1.0, 1.0 + 1e-12))
print(math.isclose(1.0, 1.1, rel_tol=0.2), math.gcd(*[12, 18]), math.hypot(*(3, 4)))
print(functools.reduce(lambda a, b: a + b, [1, 2, 3], 10))

P = collections.namedtuple("P", "x y")
print(P(1, 2), P(y=4, x=3), collections.Counter("aab").most_common(1))
print(collections.OrderedDict([("a", 1)]), collections.deque([1, 2], maxlen=3))
print(json.dumps({"b": 1, "a": [1, 2]}, sort_keys=True))
h = [3, 1, 2]
heapq.heapify(h)
print(heapq.heappop(h), h)
print(list(itertools.islice(itertools.count(5), 3)))

# Python-level library functions: plain, positional defaults, keywords.
print(inspect.isclass(P), inspect.isclass(1))
data = pickle.dumps([1, "x", (2, 3)])
print(pickle.loads(data), pickle.loads(data, encoding="ASCII"), pickle.loads(*[data]))
print(pickle.loads(pickle.dumps({"k": 2}, 2)), pickle.loads(pickle.dumps(3, protocol=2)))
print(len([name for name, _ in inspect.getmembers(P) if name == "x"]))
parser = argparse.ArgumentParser(prog="t")
parser.add_argument("--n", type=int, default=1)
print(parser.parse_args(["--n", "5"]).n, parser.parse_args([]).n)

# The argument array is the call's own: star arguments do not alias.
xs = [data]
print(pickle.loads(*xs), xs == [data])

# A rebound module global is called, not a cached one.
old = math.sqrt
math.sqrt = lambda x: "patched %s" % x
print(math.sqrt(4))
math.sqrt = old
print(math.sqrt(4.0))

try:
    math.nope(1)
except AttributeError as e:
    print("AttributeError:", e)
try:
    math.pi()
except TypeError as e:
    print("TypeError:", e)
try:
    collections.namedtuple()
except TypeError as e:
    print("TypeError raised")
