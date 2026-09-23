# Compares results() against `expected` (loaded by the test harness): one
# line per mismatch. Tolerances are relative (floor 1): TOLERANCE for float64
# cases and 2e-5 for "f32." (float32) cases.
def _close(g, e, tol):
    if isinstance(e, str) or isinstance(g, str):
        return g == e
    return abs(g - e) / max(1.0, abs(e)) <= tol


def compare(got, expected, tolerance):
    bad = []
    for name in sorted(expected):
        if name not in got:
            bad.append("%s: missing" % name)
            continue
        e, g = expected[name], got[name]
        if len(e) != len(g):
            bad.append("%s: %d values, expected %d" % (name, len(g), len(e)))
            continue
        tol = 2e-5 if name.startswith("f32.") else tolerance
        worst = None
        for i, (a, b) in enumerate(zip(g, e)):
            if not _close(a, b, tol):
                worst = (i, a, b)
                break
        if worst is not None:
            bad.append("%s[%d]: %r != %r" % (name, worst[0], worst[1], worst[2]))
    return bad
