# Pending divergences split out of tests/python_corpus/truthiness.py.
# 1. bytearray is not a subclass of bytes.
print("bytearray-isinstance", isinstance(bytearray(), bytes), issubclass(bytearray, bytes), isinstance(b"", bytearray))
# 2. The type name of Ellipsis.
print("ellipsis-type", type(Ellipsis).__name__, type(...).__name__, repr(type(...)))
# 3. __len__ returning a negative number makes truth testing (and len) raise ValueError.
class BadLen:
    def __len__(self):
        return -1
for thunk in (lambda: bool(BadLen()), lambda: len(BadLen()), lambda: not BadLen()):
    try:
        print("badlen", thunk())
    except ValueError as e:
        print("ValueError", e)
