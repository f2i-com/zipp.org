# Pending divergences split out of tests/python_corpus/name_resolution.py.
# 1. Attributes of builtin functions (zipp: "Cannot read properties of null (reading 'get') (in getattr)").
for fn in (print, len, abs, max, sorted, isinstance):
    try:
        print("builtin-name", fn.__name__, fn.__qualname__)
    except Exception as e:
        print(type(e).__name__, e)
try:
    print("builtin-module", len.__module__, hasattr(len, "__doc__"))
except Exception as e:
    print(type(e).__name__, e)
# 2. globals() is the live module namespace: writes through it create globals.
globals()["injected"] = 42
try:
    print("injected", injected)
except NameError as e:
    print("NameError", e)
g = globals()
g["later"] = "via-dict"
def read_later():
    return later
try:
    print("later", read_later())
except NameError as e:
    print("NameError", e)
del g["later"]
print("deleted", "later" in globals())
