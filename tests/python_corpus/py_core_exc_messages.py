# Pending divergences split out of tests/python_corpus/exc_messages.py: exception messages.
def show(label, thunk):
    try:
        r = thunk()
        print(label, "->", repr(r))
    except Exception as e:
        print(label, "!", type(e).__name__ + ":", e)


show("attr-set-int", lambda: setattr(5, "x", 1))
show("type-mul-list", lambda: [1] * [2])
show("type-in", lambda: 1 in 5)
show("type-join", lambda: ",".join([1, 2]))
show("value-max-empty", lambda: max([]))
show("value-min-empty", lambda: min(()))
show("type-in-float", lambda: 1 in 2.5)
show("type-join-later", lambda: ",".join(["a", "b", 3]))
show("value-max-empty-default-less", lambda: max(iter([])))
show("attr-set-none", lambda: setattr(None, "x", 1))
show("attr-set-str", lambda: setattr("s", "upper", 1))
