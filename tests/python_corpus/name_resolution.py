# Global and builtin name resolution: shadowing builtins at module level and
# deleting the shadow, `global`, rebinding functions after callers are compiled.
def use_len(x):
    return len(x)


def use_print_name():
    return print("  called print") is None


print("builtin", use_len("abc"), use_print_name(), len([1, 2]))


def len(x):
    return "shadowed:" + str(type(x).__name__)


print("shadowed", use_len("abc"), len([1]))
del len
print("restored", use_len("abcd"), len((1, 2, 3)))

sum = lambda xs: "fake-sum"
print("sum-shadow", sum([1, 2]), [sum(r) for r in ([1], [2])])
del sum
print("sum-restored", sum([1, 2]))

for i in range(3):
    if i == 1:
        abs = str
    print("loop-shadow", i, abs(-5))
del abs
print("abs-restored", abs(-5))

try:
    del len
except NameError as e:
    print("NameError", e)

counter = 0


def bump(n=1):
    global counter
    counter += n
    return counter


def read_counter():
    return counter


def local_shadow():
    counter = "local"
    return counter


print("global", bump(), bump(5), read_counter(), local_shadow(), counter)


def create_global():
    global created_later
    created_later = "made"


try:
    print(created_later)
except NameError as e:
    print("NameError", e)
create_global()
print("created", created_later)


def delete_global():
    global created_later
    del created_later


delete_global()
try:
    created_later
except NameError as e:
    print("NameError", e)


def helper():
    return "v1"


def caller():
    return helper()


print("rebind", caller())


def helper():
    return "v2"


print("rebind", caller())
helper = lambda: "v3"
print("rebind", caller())
saved = caller
del caller
print("alias", saved())
try:
    caller()
except NameError as e:
    print("NameError", e)


def make_getter():
    def get():
        return value
    return get


getter = make_getter()
try:
    getter()
except NameError as e:
    print("NameError", e)
value = 10
print("late-global", getter())
value = "changed"
print("late-global", getter())


class Scope:
    x = "class-x"
    y = [x for _ in range(1)] if False else None

    def method(self):
        return x


x = "module-x"
print("class-scope", Scope.x, Scope().method())


def builtins_in_functions():
    out = []
    for name in ["min", "max", "sorted", "isinstance", "type", "str"]:
        out.append(name)
    return out, min(3, 1), max("ab"), sorted([2, 1]), isinstance(1, int), type(1).__name__, str(5)


print("builtins", builtins_in_functions())
import builtins
print("builtins-module", builtins.len("xy"), builtins.abs(-2), builtins.max is max)


def shadow_param(len=len, print=None):
    return len("four"), print


print("param-shadow", shadow_param(), shadow_param(lambda s: 0, "p"))
True_ = True
None_ = None
print("names", True_, None_, globals().get("True_"), "True_" in globals(), "missing" in globals())


def outer():
    y = 1

    def inner():
        global y
        y = "global-y"
        return y
    inner()
    return y


print("global-in-nested", outer(), y)
