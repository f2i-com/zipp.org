# Pending: dict(d) and {**d} on a dict subclass that overrides __getitem__ copy the
# underlying storage without calling __getitem__ (split out of tests/python_corpus/builtin_methods.py).
class GetItemDict(dict):
    def __getitem__(self, key):
        return "overridden-" + str(key)
gd = GetItemDict(k="real")
print("dict-copy", dict(gd), {**gd}, gd.copy(), dict(**gd))
d2 = {}
d2.update(gd)
print("update", d2)
# str subclasses with overridden methods (see also int_subclass.py): construction fails.
class Upper(str):
    def upper(self):
        return "custom:" + super().upper()
    def join(self, items):
        return "<" + super().join(items) + ">"
try:
    u = Upper("ab")
    print("str-subclass", u.upper(), u.lower(), u.join(["x", "y"]), "-".join([u, u]), len(u), u + "c", type(u + "c").__name__)
except TypeError as e:
    print("TypeError", e)
