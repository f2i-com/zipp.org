# Imports of modules that do not exist compile and raise where they run, so
# the common optional-dependency guards work.
try:
    import zz_no_such_module_anywhere as np
except ImportError as e:
    np = None
    print("guard 1:", type(e).__name__, e.name, e)
print("np is", np)

try:
    from zz_no_such_module_anywhere import tqdm
except ImportError:
    def tqdm(it, **kw):
        return it
print("tqdm fallback:", list(tqdm(range(3), desc="x")))

try:
    import zz_no_such_pkg.sub.mod
except ModuleNotFoundError as e:
    print("dotted:", type(e).__name__, isinstance(e, ImportError), e.name)

try:
    from math import zz_no_such_name
except ImportError as e:
    print("missing name:", type(e).__name__, isinstance(e, ModuleNotFoundError))

try:
    from collections import OrderedDict, zz_nope
except ImportError as e:
    print("second name missing:", type(e).__name__)


def load(name):
    try:
        import zz_optional_backend
        return "backend"
    except ImportError:
        return "fallback for " + name


print(load("a"), load("b"))

HAVE = True
try:
    import zz_accel
except ModuleNotFoundError:
    HAVE = False
print("HAVE", HAVE)

# An import that does exist still binds normally inside the same guards.
try:
    import json
except ImportError:
    json = None
print("json ok", json.dumps({"k": 1}))

if False:
    import zz_never_reached  # never runs, never fails
print("unreached import is fine")

try:
    __import__("zz_dynamic_missing")
except ImportError as e:
    print("dynamic:", type(e).__name__)
