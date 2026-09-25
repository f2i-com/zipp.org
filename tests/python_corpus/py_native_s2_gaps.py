# Dict views (isdisjoint, __reversed__, attribute lookup), `d |= pairs`,
# and pickling a Counter.
import pickle, copy
from collections import Counter, OrderedDict, defaultdict
d = {"a": 1, "b": 2}
k = d.keys()
for f in [lambda: k.isdisjoint({"q"}), lambda: k.isdisjoint(["a"]), lambda: d.items().isdisjoint([("a", 1)]),
          lambda: hasattr(k, "__reversed__"), lambda: list(reversed(d.keys())), lambda: list(reversed(d.values())), lambda: list(reversed(d.items())),
          lambda: hasattr(k, "isdisjoint")]:
    try:
        print(f())
    except Exception as e:
        print("ERR", type(e).__name__, e)
e = {"x": 1}
try:
    e |= [("w", 0)]
    print(e)
except Exception as ex:
    print("ERR", type(ex).__name__, ex)
try:
    e |= 5
except TypeError as ex:
    print("TE", ex)
for obj in [Counter("aab"), OrderedDict(a=1)]:
    try:
        b = pickle.loads(pickle.dumps(obj))
        print(type(b).__name__, sorted(b.items()), b == obj)
    except Exception as ex:
        print("ERR", type(ex).__name__, ex)
