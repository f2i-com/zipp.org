# Pending: assigning obj.__class__ (split out of tests/python_corpus/attr_basic.py).
class Base:
    kind = "base"
    def describe(self):
        return "base:" + self.kind
class Child(Base):
    kind = "child"
a = Base()
try:
    a.__class__ = Child
    print("class-assign", type(a).__name__, a.kind, a.describe(), isinstance(a, Child))
except AttributeError as e:
    print("AttributeError", e)
