# inspect.signature: bound methods and classmethods drop their bound first
# argument, annotations and a bare `*` show, functools.partial binds its
# arguments. (`*args`, `**kwargs` and `/` are left out of this program.)
import functools
import inspect


def f(a, b=2, *, c, d=4):
    pass


def ann(a: int, b: "str" = "x", *, k: float = 1.5) -> float:
    pass


class C:
    def m(self, p, q=1):
        pass

    @classmethod
    def cm(cls, r):
        pass

    @staticmethod
    def sm(s, t=3):
        pass


print(inspect.signature(f), inspect.signature(ann))
print(inspect.signature(C().m), inspect.signature(C.m), inspect.signature(C.cm), inspect.signature(C().cm), inspect.signature(C.sm))
print(inspect.signature(functools.partial(f, 1, c=2)), inspect.signature(functools.partial(C.sm, t=5)))
print(inspect.signature(functools.partial(lambda x, y, z=0: 0, y=1)))
print(list(inspect.signature(C().m).parameters), [p.kind for p in inspect.signature(f).parameters.values()] == [inspect.Parameter.POSITIONAL_OR_KEYWORD] * 2 + [inspect.Parameter.KEYWORD_ONLY] * 2)
print(inspect.signature(f).bind(1, c=3).arguments)
print(inspect.signature(f).replace(parameters=[]), inspect.signature(f, follow_wrapped=False))
spec = inspect.getfullargspec(f)
print(spec.args, spec.defaults, spec.kwonlyargs, spec.kwonlydefaults)
