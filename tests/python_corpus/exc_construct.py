# Exception classes called like any class: builtin and user exception
# classes with 0 to 5 arguments, subclasses with and without __init__,
# exceptions made inside a handler (their __context__ is set by raising,
# not by making), made and never raised, and the same in loops long enough
# to be compiled.


class Plain(Exception):
    pass


class WithInit(ValueError):
    def __init__(self, a, b=2):
        super().__init__(a, b)
        self.extra = a * b


class Leaf(Plain):
    pass


def make_all(i):
    return [Exception(), ValueError(i), KeyError(i, i + 1), Plain(i, "a", None), Plain(1, 2, 3, 4),
            Leaf(), WithInit(i), WithInit(i, 3), RuntimeError("x", i, 2.5, None, True)]


for e in make_all(5):
    print(type(e).__name__, e.args, repr(e), str(e), e.__context__, e.__cause__, e.__traceback__, getattr(e, "extra", None))


def in_handler():
    try:
        raise KeyError("first")
    except KeyError:
        made = ValueError("made")
        print("made in handler", made.__context__)
        try:
            raise made
        except ValueError as e:
            print("raised in handler", repr(e.__context__))


in_handler()


def hot(n):
    out = []
    caught = 0
    for i in range(n):
        e = Plain(i)
        k = KeyError(i, -i)
        if i % 3 == 0:
            try:
                raise e
            except Plain as x:
                caught += len(x.args) + len(k.args)
        out.append(e.args[0])
    return caught, out[-3:], sum(out)


print(hot(3000))
e = Plain()
e.note = 1
print(e.args, e.note, isinstance(e, Exception), issubclass(type(e), Plain))
