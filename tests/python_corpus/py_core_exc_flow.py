# Pending divergences split out of tests/python_corpus/exc_flow.py.
# 1. An exception raised by an operation (not a raise statement) inside an except block
#    gets __context__ set to the exception being handled.
def exc_in_except():
    try:
        raise KeyError("first")
    except KeyError:
        return {}["second"]
try:
    exc_in_except()
except KeyError as e:
    print("op-in-except", e, repr(e.__context__))
try:
    try:
        1 / 0
    except ZeroDivisionError:
        [][0]
except IndexError as e:
    print("op-in-except-2", type(e.__context__).__name__)
try:
    try:
        1 / 0
    except ZeroDivisionError:
        try:
            [][0]
        except IndexError:
            raise
except IndexError as e:
    print("bare-reraise-context", type(e.__context__).__name__)
try:
    try:
        raise ValueError("v")
    except ValueError:
        int("x")
except ValueError as e:
    print("builtin-in-except", e, repr(e.__context__))
