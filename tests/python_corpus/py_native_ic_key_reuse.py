# A Python attribute site's cache entry names an instance-dict entry by its
# position and its key. The keys here are built at run time, so they are
# not the site's own constant: each dies with its instance, and its heap
# slot is reused, often by another instance's key for another name at the
# same position. The entry must then miss (CPython's answers below), not
# read or overwrite that other attribute. The reuse is reliable with
# ZIPP_GC_STRESS=1 (a collection at every safepoint), as the corpus's
# stress lane and tests/python_ic_key_reuse.rs run it.


class C:
    attr_x = "class value"


def get_x(o):
    return o.attr_x


def set_x(o, v):
    o.attr_x = v


def seed(r, tail):
    # Leaves get_x's and set_x's entries on this instance's key, then
    # drops the instance and its key.
    a = C()
    setattr(a, "attr_" + tail, r)
    ok = get_x(a) == r
    set_x(a, r + 1)
    return ok and get_x(a) == r + 1


def probe_get(i, tail):
    pad = [i] if i % 2 else None
    b = C()
    setattr(b, "attr_" + tail, i)
    return get_x(b) == "class value" and (pad is None) == (i % 2 == 0)


def probe_set(i, tail):
    b = C()
    setattr(b, "attr_" + tail, i)
    set_x(b, "own")
    d = vars(b)
    return d.get("attr_" + tail) == i and d.get("attr_x") == "own" and len(d) == 2


def trial(rounds, width):
    wrong_get = wrong_set = 0
    for r in range(rounds):
        if not seed(r, "x"):
            wrong_get += 1
        for i in range(width):
            if not probe_get(i, "y"):
                wrong_get += 1
        if not seed(r, "x"):
            wrong_set += 1
        if not probe_set(r, "y"):
            wrong_set += 1
    return wrong_get, wrong_set


print("wrong answers (get, set):", trial(200, 3))
print(C().attr_x, get_x(C()))
