# Method calls: bound method objects stored and called later, identity and equality
# of bound methods, staticmethod/classmethod through instances and subclasses, super() chains.
class Greeter:
    greeting = "hello"

    def __init__(self, name):
        self.name = name

    def greet(self, punct="!"):
        return f"{self.greeting} {self.name}{punct}"

    @classmethod
    def make(cls, name):
        return cls(name)

    @classmethod
    def kind(cls):
        return cls.__name__

    @staticmethod
    def helper(x):
        return x * 2


g = Greeter("ann")
bm = g.greet
print("bound", bm(), bm("?"), bm.__name__, bm.__self__ is g, bm.__func__ is Greeter.greet)
g.name = "bob"
print("bound-late-state", bm())
Greeter.greeting = "hi"
print("bound-class-change", bm(), g.greet())
methods = [g.greet, Greeter("cy").greet, Greeter.make("dee").greet]
print("stored", [m() for m in methods], [m(".") for m in methods])
print("bound-eq", g.greet == Greeter("x").greet, g.greet is g.greet, g.greet() == bm())
unbound = Greeter.greet
print("unbound", unbound(g), unbound(g, "..."), type(unbound).__name__, type(bm).__name__)
print("classmethod", Greeter.make("e").name, g.make("f").name, Greeter.kind(), g.kind(), Greeter.kind.__self__ is Greeter)
print("staticmethod", Greeter.helper(4), g.helper("ab"), [Greeter.helper(i) for i in range(3)])


class Loud(Greeter):
    greeting = "HEY"

    def greet(self, punct="!!"):
        return super().greet(punct).upper()


lg = Loud.make("gus")
print("subclass", type(lg).__name__, lg.greet(), Loud.kind(), lg.helper(5), Greeter.greet(lg))
cm = Loud.make
print("stored-classmethod", type(cm("h")).__name__, cm.__self__.__name__)


class A:
    def who(self):
        return ["A"]

    def chain(self, depth):
        return f"A{depth}"


class B(A):
    def who(self):
        return ["B"] + super().who()

    def chain(self, depth):
        return "B" + super().chain(depth + 1)


class C(A):
    def who(self):
        return ["C"] + super().who()

    def chain(self, depth):
        return "C" + super().chain(depth + 1)


class D(B, C):
    def who(self):
        return ["D"] + super().who()

    def chain(self, depth=0):
        return "D" + super().chain(depth + 1)


d = D()
print("mro-super", d.who(), d.chain(), [k.__name__ for k in D.__mro__])
print("explicit-super", super(B, d).who(), super(C, d).who(), super(D, d).chain(10))


class Init1:
    def __init__(self):
        self.trace = ["Init1"]


class Init2(Init1):
    def __init__(self):
        super().__init__()
        self.trace.append("Init2")


class Init3(Init2):
    def __init__(self, extra):
        super().__init__()
        self.trace.append(f"Init3({extra})")


print("init-chain", Init3("x").trace)


class Registry:
    handlers = {}

    @classmethod
    def register(cls, name):
        def deco(fn):
            cls.handlers[name] = fn
            return fn
        return deco


@Registry.register("double")
def double(x):
    return x * 2


@Registry.register("neg")
def neg(x):
    return -x


print("registry", {k: f(7) for k, f in sorted(Registry.handlers.items())})


class Acc:
    def __init__(self):
        self.items = []

    def add(self, x):
        self.items.append(x)
        return self


acc = Acc()
add = acc.add
for i in range(5):
    add(i)
print("hot-bound", acc.items, add(9).add(10).items[-2:])
push = acc.items.append
for ch in "xyz":
    push(ch)
print("builtin-bound", acc.items[-3:])
callbacks = {name: getattr(acc, name) for name in ["add"]}
callbacks["add"](42)
print("getattr-bound", acc.items[-1])


class Counter:
    def __init__(self):
        self.n = 0

    def __call__(self):
        self.n += 1
        return self.n

    def reset(self):
        self.n = 0


cnt = Counter()
fns = [cnt, cnt.reset, cnt]
out = [f() for f in fns]
print("callable-mix", out, cnt.n)


def free_function(self, v):
    return (type(self).__name__, v)


Counter.attached = free_function
print("attached", cnt.attached(1), Counter.attached(cnt, 2))
Counter.st = staticmethod(lambda v: v + 1)
Counter.cm = classmethod(lambda cls, v: (cls.__name__, v))
print("attached-descriptors", cnt.st(1), Counter.st(2), cnt.cm(3), Counter.cm(4))
m1 = cnt.__call__
print("dunder-bound", m1(), m1(), cnt.n)
