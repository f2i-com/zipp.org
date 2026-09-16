# Global reads compiled inline (module dict, then builtins, then the
# helper): rebinding, deletion, builtin shadowing and None-valued globals
# must all be seen at once.
K = 3
NOTHING = None


def read_k():
    return K


def read_nothing():
    return NOTHING


def set_k(v):
    global K
    K = v


def read_len(xs):
    return len(xs)


print(read_k(), read_nothing())
K = 4
print(read_k())
set_k(5)
print(K, read_k())
NOTHING = 0
print(read_nothing())

# A module-level name shadows the builtin; deleting it restores the builtin.
print(read_len([1, 2, 3]))
len = lambda xs: -1  # noqa: E731
print(read_len([1, 2, 3]))
del len
print(read_len([1, 2, 3]))

# A deleted global raises NameError at the next read.
del K
try:
    read_k()
except NameError as e:
    print("NameError:", e)
K = 6
print(read_k())

# globals() is the live module dictionary.
globals()["LATE"] = 7


def read_late():
    return LATE


print(read_late())
del globals()["LATE"]
try:
    read_late()
except NameError as e:
    print("NameError:", e)


def read_missing():
    return never_bound  # noqa: F821


try:
    read_missing()
except NameError as e:
    print("NameError:", e)

# Builtins read through functions and nested scopes; a global bound to a
# builtin's name inside a class body is a class attribute, not a global.
def outer():
    def inner():
        return list(range(3)), isinstance(1, int), abs(-2)
    return inner()


print(outer())


class Shadow:
    range = "class attr"

    def use(self):
        return range(2), self.range


s = Shadow()
print(list(s.use()[0]), s.use()[1])

# A global bound to a falsy value is a hit, not a miss.
ZERO = 0
EMPTY = ""
FALSE = False


def falsy():
    return ZERO, EMPTY, FALSE


print(falsy())

# A loop that reads a global rebound by the loop body.
COUNT = 0


def bump():
    global COUNT
    COUNT += 1
    return COUNT


for _ in range(5):
    bump()
print(COUNT)

# __builtins__ is answered by the helper.
print(type(__builtins__).__name__ in ("module", "dict"))
