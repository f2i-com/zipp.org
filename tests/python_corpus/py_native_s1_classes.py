# Class mutation while attribute, method and construction sites are warm:
# every change happens mid-loop, after the site has answered the same way
# many times, so a per-site cache that missed the change would show it.
# Instance attributes against properties and data descriptors added and
# removed, methods replaced and deleted (falling back to a base class),
# class attributes changed and shadowed, subclasses seeing base changes,
# __class__ assigned, __setattr__ / __getattr__ added,
# setattr/delattr/type.__setattr__ on the class, and __init__ replaced.


class Base:
    def who(self):
        return "base"


class P(Base):
    kind = "p"

    def __init__(self, x):
        self.x = x

    def who(self):
        return "p"


def reads(objs, n, change=None, at=-1):
    out = []
    for i in range(n):
        if i == at and change is not None:
            change()
        o = objs[i % len(objs)]
        out.append(o.x)
    return out


objs = [P(i) for i in range(4)]
print(reads(objs, 12))


def add_property():
    P.x = property(lambda self: "prop")


print(reads(objs, 12, add_property, 6))


def drop_property():
    del P.x


print(reads(objs, 12, drop_property, 5))


class Desc:
    def __get__(self, obj, owner):
        return "desc-get"

    def __set__(self, obj, value):
        obj.__dict__["x"] = value * 10


def add_descriptor():
    P.x = Desc()


print(reads(objs, 10, add_descriptor, 4))


def writes(objs, n, change=None, at=-1):
    for i in range(n):
        if i == at and change is not None:
            change()
        o = objs[i % len(objs)]
        o.x = i
    return [o.__dict__.get("x") for o in objs]


print(writes(objs, 8))
del P.x
print(writes(objs, 8))
print(writes(objs, 8, add_descriptor, 3))
del P.x
print(reads(objs, 4))


def calls(objs, n, change=None, at=-1):
    out = []
    for i in range(n):
        if i == at and change is not None:
            change()
        out.append(objs[i % len(objs)].who())
    return out


print(calls(objs, 6))


def replace_method():
    P.who = lambda self: "replaced"


print(calls(objs, 8, replace_method, 3))


def delete_method():
    del P.who


print(calls(objs, 8, delete_method, 4))


def base_change():
    Base.who = lambda self: "base2"


print(calls(objs, 8, base_change, 2))


def shadow_method():
    objs[1].who = lambda: "own"


print(calls(objs, 8, shadow_method, 3))
del objs[1].who
print(calls(objs, 4))


def class_attr(objs, n, change=None, at=-1):
    out = []
    for i in range(n):
        if i == at and change is not None:
            change()
        out.append(objs[i % len(objs)].kind)
    return out


print(class_attr(objs, 6))


def set_kind():
    P.kind = "q"


print(class_attr(objs, 8, set_kind, 3))


def own_kind():
    objs[2].kind = "own"


print(class_attr(objs, 8, own_kind, 5))
del objs[2].kind


def base_kind():
    del P.kind
    Base.kind = "from-base"


print(class_attr(objs, 8, base_kind, 2))


class Q(Base):
    def __init__(self, x):
        self.x = -x

    def who(self):
        return "q"


def swap_class():
    objs[0].__class__ = Q
    objs[3].__class__ = Q


print(calls(objs, 8, swap_class, 4))
print(reads(objs, 8))
objs[0].__class__ = P
objs[3].__class__ = P


def add_setattr():
    def __setattr__(self, name, value):
        object.__setattr__(self, name, ("set", value))
    P.__setattr__ = __setattr__


print(writes(objs, 8, add_setattr, 4))
del P.__setattr__
print(writes(objs, 4))


class G:
    def __init__(self):
        self.a = 1


def gets(objs, n, change=None, at=-1):
    out = []
    for i in range(n):
        if i == at and change is not None:
            change()
        o = objs[i % len(objs)]
        try:
            out.append(o.missing)
        except AttributeError:
            out.append("AE")
        out.append(o.a)
    return out


gs = [G(), G()]
print(gets(gs, 6))


def add_getattr():
    G.__getattr__ = lambda self, name: "ga:" + name


print(gets(gs, 6, add_getattr, 3))


del G.__getattr__
print(gets(gs, 4))


def meta_ops(objs, n):
    out = []
    for i in range(n):
        if i == 2:
            setattr(P, "who", lambda self: "setattr")
        if i == 4:
            type.__setattr__(P, "who", lambda self: "type.__setattr__")
        if i == 6:
            delattr(P, "who")
        out.append(objs[i % len(objs)].who())
    return out


print(meta_ops(objs, 8))


def news(n, change=None, at=-1):
    out = []
    for i in range(n):
        if i == at and change is not None:
            change()
        out.append(P(i).x)
    return out


print(news(6))


def replace_init():
    def __init__(self, x):
        self.x = x * 100
    P.__init__ = __init__


print(news(6, replace_init, 3))


class Prop:
    def __init__(self):
        self._v = 1

    @property
    def v(self):
        return self._v

    @v.setter
    def v(self, value):
        self._v = value + 1


def props(o, n, change=None, at=-1):
    out = []
    for i in range(n):
        if i == at and change is not None:
            change()
        o.v = i
        out.append(o.v)
    return out


pr = Prop()
print(props(pr, 6))


def replace_prop():
    Prop.v = property(lambda self: "new-getter", lambda self, value: setattr(self, "_v", -value))


print(props(pr, 6, replace_prop, 3))


def drop_prop():
    del Prop.v


print(props(pr, 6, drop_prop, 2))
print(sorted(pr.__dict__.items()))


class Sub(P):
    pass


subs = [Sub(i) for i in range(3)]
print(calls(subs, 4))
P.who = lambda self: "p-again"
print(calls(subs, 4))


def sub_override():
    Sub.who = lambda self: "sub"


print(calls(subs, 6, sub_override, 2))
del Sub.who
print(calls(subs, 3))
