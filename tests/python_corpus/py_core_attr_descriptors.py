# Pending: the descriptor protocol methods of builtin descriptors are not exposed
# (split out of tests/python_corpus/attr_descriptors.py).
class Meth:
    def plain(self):
        return "plain"
    @staticmethod
    def st():
        return "static"
    @classmethod
    def cm(cls):
        return cls.__name__
    @property
    def prop(self):
        return "prop"
m = Meth()
for label, thunk in [
    ("classmethod.__get__", lambda: Meth.__dict__["cm"].__get__(None, Meth)()),
    ("function.__get__", lambda: Meth.__dict__["plain"].__get__(m, Meth)()),
    ("staticmethod.__get__", lambda: Meth.__dict__["st"].__get__(None, Meth)()),
    ("property.__get__", lambda: Meth.__dict__["prop"].__get__(m, Meth)),
    ("classmethod.__func__", lambda: Meth.__dict__["cm"].__func__.__name__),
    ("bound.__func__", lambda: m.plain.__func__ is Meth.plain),
    ("bound.__self__", lambda: m.plain.__self__ is m),
]:
    try:
        print(label, thunk())
    except AttributeError as e:
        print(label, "AttributeError", e)
