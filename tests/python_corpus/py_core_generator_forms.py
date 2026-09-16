# Pending divergences split out of tests/python_corpus/generator_forms.py.
# 1. Once a generator has finished, further next() raises a StopIteration with value None.
def tracer():
    yield 1
    return "result"
g = tracer()
print("drain", list(g))
try:
    next(g)
except StopIteration as e:
    print("exhausted-value", e.value, e.args)
g = tracer()
next(g)
try:
    next(g)
except StopIteration as e:
    print("first-stop", e.value)
try:
    next(g)
except StopIteration as e:
    print("second-stop", e.value)
# 2. close() raises GeneratorExit inside the suspended frame, so `except GeneratorExit` runs.
def closer():
    try:
        yield 1
        yield 2
    except GeneratorExit:
        print("  got GeneratorExit")
        raise
    finally:
        print("  cleanup")
c = closer()
next(c)
c.close()
print("closed", list(c))
def ignores_exit():
    try:
        yield 1
    except GeneratorExit:
        yield "refused"
bad = ignores_exit()
next(bad)
try:
    bad.close()
    print("close-ignored no error")
except RuntimeError as e:
    print("RuntimeError", e)
