# A generator suspended inside an `except` (or `finally`) block keeps its
# handled exception to itself: the caller's sys.exc_info() and __context__ do
# not see it, and it comes back when the generator resumes.
import sys

def in_except():
    try:
        raise ValueError("inside gen")
    except ValueError:
        yield 1
        print("resumed, handling:", repr(sys.exc_info()[1]))
        yield 2
    print("after handler:", sys.exc_info()[0])

it = in_except()
print(next(it))
print("caller sees:", sys.exc_info()[0])
try:
    raise KeyError("x")
except KeyError as e:
    print("context:", repr(e.__context__))
print(next(it))
print("caller sees:", sys.exc_info()[0])
print(list(it))
print("caller sees:", sys.exc_info()[0])

# Resumed from inside the caller's own handler: the generator's exception
# stacks on top of the caller's, and the caller's comes back afterwards.
it = in_except()
next(it)
try:
    raise TypeError("outer")
except TypeError:
    print(next(it))
    print("caller handling:", repr(sys.exc_info()[1]))
    try:
        raise OSError("again")
    except OSError as e:
        print("context:", repr(e.__context__))
print("caller sees:", sys.exc_info()[0])

# close() lands in `except GeneratorExit` blocks; nothing leaks from there.
def closer():
    try:
        yield 1
    except GeneratorExit:
        print("closing, handling:", sys.exc_info()[0].__name__)

g = closer(); next(g); g.close()
print("after close:", sys.exc_info()[0])
try:
    raise KeyError("y")
except KeyError as e:
    print("context:", repr(e.__context__))

# A generator paused in `finally` while an exception propagates, then closed.
def fin():
    try:
        try:
            raise ValueError("in try")
        finally:
            yield "in finally"
    except ValueError:
        pass
g = fin(); print(next(g))
print("caller sees:", sys.exc_info()[0])
g.close()
print("after close:", sys.exc_info()[0])

# throw() into a generator suspended in a handler.
def thrown():
    try:
        raise ValueError("a")
    except ValueError:
        try:
            yield 1
        except KeyError as e:
            print("caught", repr(e), "context", repr(e.__context__))
            yield 2
g = thrown(); next(g)
print(g.throw(KeyError("b")))
print("caller sees:", sys.exc_info()[0])
g.close()
print("after close:", sys.exc_info()[0])
g = thrown(); next(g)
print(g.throw(KeyError))
g.close()
