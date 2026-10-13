# Properties whose getter only returns an attribute of `self`, and whose
# setter only stores one, read and written in straight-line code and in
# loops long enough to compile, while the instances, their attributes and
# the classes change: the values, the errors and their tracebacks must be
# CPython's.
import traceback


class Box:
    def __init__(self, size):
        self._size = size

    @property
    def size(self):
        return self._size

    @size.setter
    def size(self, value):
        self._size = value


class Checked(Box):
    @property
    def size(self):
        return self._size * 10

    @size.setter
    def size(self, value):
        if value < 0:
            raise ValueError("negative")
        self._size = value


class Nested:
    def __init__(self):
        self._inner = Box(7)

    @property
    def inner(self):
        return self._inner

    @property
    def deep(self):
        return self.inner


class WithDefault:
    def __init__(self):
        self._v = 3

    @property
    def v(self, unused=None):
        return self._v


b, c, n, w = Box(1), Checked(2), Nested(), WithDefault()
total = 0
for i in range(3000):
    total += b.size + c.size + n.inner.size + n.deep.size + w.v
    b.size = i
    c.size = i % 7
print(total, b.size, c.size, b._size, c._size)

# One site, instances of several classes, an attribute deleted and restored.
boxes = [Box(i) for i in range(5)] + [Checked(3)]
acc = []
for i in range(4000):
    x = boxes[i % len(boxes)]
    if i == 2000:
        del boxes[1]._size
    try:
        acc.append(x.size)
    except AttributeError as exc:
        if len(acc) < 4000:
            acc.append("missing")
    if i == 2600:
        boxes[1]._size = -1
    if i % 3 == 0:
        x.size = i % 50
print(len(acc), acc[:8], acc[1995:2015], acc[-8:])

# The class's property replaced, then deleted, mid-loop.
class Shape:
    def __init__(self):
        self._w = 2

    @property
    def w(self):
        return self._w


s = Shape()
out = []
for i in range(3000):
    if i == 1000:
        Shape.w = property(lambda self: self._w + 100)
    if i == 2000:
        del Shape.w
        s.w = "plain"
    out.append(s.w)
print(out[998:1003], out[1998:2003], out[-1])

# A setter that stores, then a value it rejects, in a compiled loop.
cb = Checked(0)
errs = 0
for i in range(2000):
    try:
        cb.size = i - 1990
    except ValueError:
        errs += 1
print(errs, cb.size)

# The traceback of a getter whose attribute is gone.
def read_all(items):
    t = 0
    for it in items:
        t += it.size
    return t


items = [Box(1) for _ in range(3000)]
print(read_all(items))
del items[-1]._size
try:
    read_all(items)
except AttributeError as exc:
    for f in traceback.extract_tb(exc.__traceback__):
        print(f.name, f.lineno)
    print(type(exc).__name__)
