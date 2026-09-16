# Pending: unpacking a non-iterable says "cannot unpack non-iterable X object"
# (split out of tests/python_corpus/unpack_forms.py).
for bad in (5, None, 2.5, len):
    try:
        a, b = bad
    except TypeError as e:
        print("unpack", type(e).__name__, e)
    try:
        for a, b in [bad]:
            pass
    except TypeError as e:
        print("for-unpack", type(e).__name__, e)
    try:
        a, *b = bad
    except TypeError as e:
        print("star-unpack", type(e).__name__, e)
