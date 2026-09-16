# Pending divergences split out of tests/python_corpus/attr_dynamic.py.
# 1. object.__delattr__ on a missing attribute: message.
class Recorder:
    def __delattr__(self, name):
        super().__delattr__(name)
try:
    del Recorder().zzz
except AttributeError as e:
    print("delattr-missing", e)
# 2. Instances of a class with __slots__ have no __dict__; a subclass without __slots__ keeps slot values out of its __dict__.
class Slotted:
    __slots__ = ("x", "y")
    def __init__(self, x):
        self.x = x
class Loose(Slotted):
    pass
s = Slotted(1)
lo = Loose(1)
lo.anything = "has dict"
print("slots-dict", hasattr(s, "__dict__"), sorted(lo.__dict__), "x" in vars(lo))
try:
    print("slots-vars", vars(s))
except TypeError as e:
    print("TypeError", e)
# 3. getattr with a non-str name: message.
for thunk in (lambda: getattr(s, 5), lambda: setattr(s, 5, 1), lambda: hasattr(s, None)):
    try:
        thunk()
    except TypeError as e:
        print("TypeError", e)
