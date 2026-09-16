# __getattr__, __setattr__, __delattr__, __slots__, getattr/hasattr/setattr with
# defaults and error propagation from __getattr__ that raises something else.
class Proxy:
    def __init__(self, target):
        object.__setattr__(self, "_target", target)
        object.__setattr__(self, "_reads", [])

    def __getattr__(self, name):
        self._reads.append(name)
        return getattr(self._target, name)

    def __setattr__(self, name, value):
        setattr(self._target, name, value)


class Target:
    def __init__(self):
        self.a = 1

    def hello(self):
        return "hello from target"


tg = Target()
p = Proxy(tg)
p.b = 2
print("proxy", p.a, p.b, p.hello(), tg.b, p._reads, sorted(vars(tg)))
try:
    p.missing
except AttributeError as e:
    print("AttributeError", e)


class Defaults:
    x = "class-x"

    def __init__(self):
        self.y = "inst-y"

    def __getattr__(self, name):
        if name.startswith("auto_"):
            return name[5:]
        raise AttributeError(f"no {name}")


d = Defaults()
print("getattr-fallback", d.x, d.y, d.auto_thing, getattr(d, "auto_z"), getattr(d, "other", "dflt"), hasattr(d, "auto_q"), hasattr(d, "other"))


class Exploding:
    def __getattr__(self, name):
        if name == "boom":
            raise ValueError("exploded while looking up " + name)
        if name == "key":
            raise KeyError(name)
        raise AttributeError(name)


ex = Exploding()
print("hasattr-attributeerror", hasattr(ex, "plain"), getattr(ex, "plain", "default"))
for probe in [lambda: hasattr(ex, "boom"), lambda: getattr(ex, "boom", None), lambda: hasattr(ex, "key"), lambda: ex.boom]:
    try:
        print("probe", probe())
    except (ValueError, KeyError) as e:
        print("propagated", type(e).__name__, e)


class Recorder:
    def __init__(self):
        self.__dict__["events"] = []

    def __setattr__(self, name, value):
        self.events.append(("set", name, value))
        super().__setattr__(name, value)

    def __delattr__(self, name):
        self.events.append(("del", name))
        super().__delattr__(name)


rec = Recorder()
rec.a = 1
rec.a += 1
rec.b = [rec.a]
del rec.a
print("recorder", rec.events, sorted(rec.__dict__))


class Frozen:
    __slots__ = ()

    def __setattr__(self, name, value):
        raise AttributeError(f"cannot set {name!r}: frozen")


try:
    Frozen().x = 1
except AttributeError as e:
    print("AttributeError", e)


class Slotted:
    __slots__ = ("x", "y")

    def __init__(self, x):
        self.x = x


s = Slotted(1)
s.y = 2
print("slots", s.x, s.y, Slotted.__slots__)
try:
    s.z = 3
except AttributeError as e:
    print("slots-AttributeError", type(e).__name__)
del s.y
print("slots-del", hasattr(s, "y"), getattr(s, "y", "unset"))
try:
    s.y
except AttributeError as e:
    print("slots-unset", type(e).__name__)


class SlotChild(Slotted):
    __slots__ = ("z",)


sc = SlotChild(5)
sc.z = 6
sc.y = 7
print("slot-child", sc.x, sc.y, sc.z)


class Loose(Slotted):
    pass


lo = Loose(1)
lo.anything = "has dict"
print("slot-subclass-dict", lo.anything, lo.x, "anything" in lo.__dict__)


class AttrDict(dict):
    def __getattr__(self, name):
        try:
            return self[name]
        except KeyError:
            raise AttributeError(name) from None

    def __setattr__(self, name, value):
        self[name] = value


ad = AttrDict(a=1)
ad.b = 2
print("attrdict", ad.a, ad.b, sorted(ad.items()), hasattr(ad, "c"), getattr(ad, "keys") is not None)


class Chain:
    def __init__(self, path=()):
        object.__setattr__(self, "path", path)

    def __getattr__(self, name):
        return Chain(self.path + (name,))

    def __call__(self, *args):
        return ".".join(self.path) + str(args)


print("chain", Chain().a.b.c(1, 2), Chain().x())
names = ["alpha", "beta", "gamma"]
obj = Target()
for i, n in enumerate(names):
    setattr(obj, n, i * i)
print("setattr-loop", [getattr(obj, n) for n in names], sum(getattr(obj, n, 0) for n in names + ["delta"]))
for n in names[:2]:
    delattr(obj, n)
print("delattr-loop", [hasattr(obj, n) for n in names])
print("getattr-special", getattr(obj, "__class__").__name__, getattr(5, "real"), getattr("s", "upper")(), hasattr(1, "imag"), hasattr(None, "x"))
