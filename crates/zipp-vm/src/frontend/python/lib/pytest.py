"""A small pytest for Zipp: `raises`, `approx`, `mark.parametrize`, `skip`,
`fixture` (plain functions, plus the built-in `tmp_path`), and `main()`,
which collects and runs `test_*` functions of a module and prints a report.
The runtime calls it automatically when the program's entry file is a
`test_*.py` or `*_test.py`."""
import sys
import math


class _Outcome(Exception):
    pass


class Skipped(_Outcome):
    pass


class Failed(_Outcome):
    pass


class _Mark:
    def __init__(self, name, args, kwargs):
        self.name = name
        self.args = args
        self.kwargs = kwargs


class _MarkDecorator:
    def __init__(self, name, args=(), kwargs=None):
        self.name = name
        self.args = args
        self.kwargs = kwargs or {}

    def __call__(self, *args, **kwargs):
        configured = bool(self.args or self.kwargs)
        if len(args) == 1 and not kwargs and callable(args[0]) and (configured or self.name in ("skip", "xfail", "slow")):
            return self._apply(args[0])
        if len(args) == 1 and not kwargs and callable(args[0]) and not isinstance(args[0], str):
            return self._apply(args[0])
        return _MarkDecorator(self.name, args, kwargs)

    def _apply(self, fn):
        marks = list(getattr(fn, "pytestmark", []))
        marks.append(_Mark(self.name, self.args, self.kwargs))
        fn.pytestmark = marks
        return fn

    def __getattr__(self, name):
        return _MarkDecorator(name)


class _MarkNamespace:
    def __getattr__(self, name):
        return _MarkDecorator(name)

    def parametrize(self, names, values, ids=None):
        return _MarkDecorator("parametrize", (names, values), {"ids": ids})

    def skip(self, reason=""):
        return _MarkDecorator("skip", (), {"reason": reason})

    def skipif(self, condition, reason=""):
        return _MarkDecorator("skipif", (condition,), {"reason": reason})

    def xfail(self, *a, **k):
        return _MarkDecorator("xfail", a, k)


mark = _MarkNamespace()


def skip(reason=""):
    raise Skipped(reason)


def fail(reason=""):
    raise Failed(reason)


def xfail(reason=""):
    raise Skipped("xfail: " + reason)


class raises:
    def __init__(self, expected, match=None):
        self.expected = expected
        self.match = match
        self.value = None
        self.type = None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        if exc_type is None:
            raise Failed("DID NOT RAISE %s" % _name(self.expected))
        if not issubclass(exc_type, self.expected):
            return False
        if self.match is not None:
            import re
            if not re.search(self.match, str(exc)):
                raise Failed("Regex pattern %r does not match %r." % (self.match, str(exc)))
        self.value = exc
        self.type = exc_type
        return True


class warns(raises):
    def __exit__(self, exc_type, exc, tb):
        return True


class approx:
    def __init__(self, expected, rel=None, abs=None, nan_ok=False):
        self.expected = expected
        self.rel = 1e-6 if rel is None else rel
        self.abs = 1e-12 if abs is None else abs

    def _close(self, a, b):
        if isinstance(b, (list, tuple)):
            return isinstance(a, (list, tuple)) and len(a) == len(b) and all(self._close(x, y) for x, y in zip(a, b))
        if isinstance(b, dict):
            return isinstance(a, dict) and set(a) == set(b) and all(self._close(a[k], b[k]) for k in b)
        a, b = float(a), float(b)
        if a == b:
            return True
        return math.fabs(a - b) <= max(self.abs, self.rel * math.fabs(b))

    def __eq__(self, other):
        return self._close(other, self.expected)

    def __ne__(self, other):
        return not self.__eq__(other)

    def __repr__(self):
        return "approx(%r)" % (self.expected,)


def fixture(fn=None, **kw):
    if fn is None:
        return lambda f: fixture(f, **kw)
    fn._pytest_fixture = True
    return fn


def _name(t):
    return getattr(t, "__name__", str(t))


class _TmpPath:
    _count = 0

    @classmethod
    def make(cls):
        import os
        from pathlib import Path
        cls._count += 1
        p = Path("/tmp/pytest/test%d" % cls._count)
        os.makedirs(str(p), exist_ok=True)
        return p


def _fixtures_for(fn, module_globals):
    """Arguments for a test from fixtures of the same module and built-ins."""
    code = getattr(fn, "__code__", None)
    names = list(getattr(code, "co_varnames", ()))[:getattr(code, "co_argcount", 0)] if code is not None else []
    return names


def _cases(fn):
    """Parameter sets from @mark.parametrize, innermost first."""
    sets = [{}]
    for m in getattr(fn, "pytestmark", []):
        if m.name != "parametrize":
            continue
        names, values = m.args[0], m.args[1]
        names = [n.strip() for n in names.split(",")] if isinstance(names, str) else list(names)
        expanded = []
        for base in sets:
            for v in values:
                row = dict(base)
                vals = v if isinstance(v, (tuple, list)) and len(names) > 1 else (v,)
                for n, x in zip(names, vals):
                    row[n] = x
                expanded.append(row)
        sets = expanded
    return sets


def _skipped(fn):
    for m in getattr(fn, "pytestmark", []):
        if m.name == "skip":
            return m.kwargs.get("reason", "skipped")
        if m.name == "skipif" and m.args and m.args[0]:
            return m.kwargs.get("reason", "skipped")
    return None


def run_module(module_globals, name="module", verbose=True):
    """Run every `test_*` function (and `Test*` class methods) in a namespace."""
    items = []
    for key in sorted(module_globals):
        obj = module_globals[key]
        if key.startswith("test_") and callable(obj) and not isinstance(obj, type):
            items.append((key, obj, None))
        elif key.startswith("Test") and isinstance(obj, type):
            for attr in sorted(dir(obj)):
                if attr.startswith("test_"):
                    items.append((key + "::" + attr, getattr(obj, attr), obj))
    fixtures = {k: v for k, v in module_globals.items() if getattr(v, "_pytest_fixture", False)}
    passed, failed, skipped = 0, 0, 0
    failures = []
    for label, fn, owner in items:
        reason = _skipped(fn)
        if reason is not None:
            skipped += 1
            if verbose:
                print("%s SKIPPED (%s)" % (label, reason))
            continue
        for case in _cases(fn):
            case_label = label + ("[%s]" % "-".join(repr(v) for v in case.values()) if case else "")
            try:
                kwargs = dict(case)
                for arg in _fixtures_for(fn, module_globals):
                    if arg in kwargs or (owner is not None and arg == "self"):
                        continue
                    if arg == "tmp_path":
                        kwargs[arg] = _TmpPath.make()
                    elif arg in fixtures:
                        kwargs[arg] = fixtures[arg]()
                if owner is not None:
                    fn(owner(), **kwargs)
                else:
                    fn(**kwargs)
                passed += 1
                if verbose:
                    print("%s PASSED" % case_label)
            except Skipped as e:
                skipped += 1
                if verbose:
                    print("%s SKIPPED (%s)" % (case_label, e))
            except AssertionError as e:
                failed += 1
                failures.append((case_label, "AssertionError" + (": %s" % e if str(e) else "")))
                print("%s FAILED" % case_label)
            except Exception as e:
                failed += 1
                failures.append((case_label, "%s: %s" % (type(e).__name__, e)))
                print("%s FAILED" % case_label)
    for label, message in failures:
        print("FAILED %s - %s" % (label, message))
    summary = []
    if failed:
        summary.append("%d failed" % failed)
    if passed:
        summary.append("%d passed" % passed)
    if skipped:
        summary.append("%d skipped" % skipped)
    print("=" * 20, ", ".join(summary) or "no tests ran", "=" * 20)
    return 1 if failed else 0


def main(args=None):
    """`pytest.main(["tests/test_x.py"])`: import each named module and run it."""
    import importlib
    args = list(args or [])
    targets = [a for a in args if not a.startswith("-")] or [sys.argv[0]]
    status = 0
    for target in targets:
        name = target[:-3] if target.endswith(".py") else target
        name = name.replace("/", ".").replace("\\", ".")
        module = importlib.import_module(name)
        status = max(status, run_module(vars(module), name))
    return status
