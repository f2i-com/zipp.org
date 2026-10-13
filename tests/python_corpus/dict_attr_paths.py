# `hasattr`, method calls whose receivers change class, `dict.get` and
# `try`/`except` in loops long enough to compile, each meeting what its fast
# path cannot answer: attributes added and deleted mid-loop, properties,
# `__getattr__`, builtin and module receivers, shadowing instance
# attributes, dict subclasses and objects with their own `get`, keys whose
# hash is user code, and exceptions chained inside handlers.
import math


class Plain:
    def __init__(self):
        self.present = 1

    def area(self):
        return 2


class Other:
    kind = "other"

    def area(self):
        return 3

    @property
    def prop(self):
        return "p"


class Dyn:
    def __getattr__(self, name):
        if name.startswith("dyn"):
            return name
        raise AttributeError(name)


p, o, d = Plain(), Other(), Dyn()
seen = []
for i in range(3000):
    if i == 1000:
        p.extra = i
    if i == 2000:
        del p.extra
    row = (hasattr(p, "present"), hasattr(p, "extra"), hasattr(p, "absent"), hasattr(o, "kind"),
           hasattr(o, "prop"), hasattr(d, "dynamic"), hasattr(d, "other"), hasattr(5, "real"),
           hasattr("s", "upper"), hasattr([], "nope"), hasattr(math, "sqrt"), hasattr(math, "nope"))
    if i % 499 == 0 or i in (999, 1000, 1999, 2000):
        seen.append((i, row))
print(seen)

real_hasattr = hasattr
count = 0
for i in range(2000):
    if i == 700:
        hasattr = lambda obj, name: name == "absent"
    if i == 1400:
        hasattr = real_hasattr
    if hasattr(p, "absent"):
        count += 1
print(count)
hasattr = real_hasattr


# One method site, receivers of several classes and kinds.
class Shadowed(Plain):
    pass


sh = Shadowed()
sh.area = lambda: 99
receivers = [p, o, sh, Plain(), Other()]
total = 0
for i in range(4000):
    r = receivers[i % len(receivers)]
    total += r.area()
    if i == 2500:
        del sh.area
print(total, sh.area())
names = []
for i in range(3000):
    r = [p, "abc", math, o][i % 4]
    if i % 4 == 1:
        names.append(r.upper())
    elif i % 4 == 2:
        names.append(r.floor(2.5))
    elif i % 4 == 0:
        names.append(r.area())
    else:
        names.append(r.area() + 1)
print(len(names), names[:8], names[-8:])


# dict.get with present and absent keys, on dicts and on other `get`s.
class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return self.v % 7

    def __eq__(self, other):
        return isinstance(other, Key) and other.v == self.v


class DSub(dict):
    def get(self, k, default=None):
        return ("sub", k, default)


class Getter:
    def get(self, k, default=None):
        return ("getter", k)


plain = {"a": 1, "b": 2, 3: "three", (1, 2): "pair", Key(10): "ten"}
others = [plain, DSub(a=1), Getter(), {}]
got = []
for i in range(4000):
    m = others[i % 4] if i % 50 == 0 else plain
    got.append(m.get("a", 0))
    got.append(m.get("zz", -1))
    got.append(m.get(3))
    got.append(m.get((1, 2), "none"))
    got.append(m.get(Key(10 + 7 * (i % 2)), "nokey"))
    try:
        m.get([i], 0)
    except TypeError as exc:
        if i < 3:
            got.append(str(exc))
print(len(got), got[:12], got[-12:])
print(sum(1 for g in got if g == "ten"), sum(1 for g in got if g == "nokey"))

# try/except in a compiled loop: caught, re-raised, chained, and the
# exception's args and context read in the handler.
log = []
for i in range(3000):
    try:
        try:
            if i % 3 == 0:
                {}["k%d" % (i % 5)]
            elif i % 3 == 1:
                raise ValueError(i)
            else:
                pass
        except KeyError as exc:
            if i % 9 == 0:
                raise RuntimeError("from key") from exc
            if i % 300 == 0:
                log.append(("key", exc.args))
        except ValueError as exc:
            if i % 7 == 1:
                raise TypeError("in handler")
            if i % 301 == 1:
                log.append(("value", exc.args))
    except RuntimeError as exc:
        if i % 900 == 0:
            log.append(("runtime", str(exc), type(exc.__cause__).__name__, exc.__cause__.args))
    except TypeError as exc:
        if i % 700 == 1:
            log.append(("type", str(exc), type(exc.__context__).__name__, exc.__context__.args))
print(log)
