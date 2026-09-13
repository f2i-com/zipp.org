"""inspect for Zipp: signatures from the runtime's function records."""
from collections import OrderedDict


class Parameter:
    POSITIONAL_ONLY = "POSITIONAL_ONLY"
    POSITIONAL_OR_KEYWORD = "POSITIONAL_OR_KEYWORD"
    VAR_POSITIONAL = "VAR_POSITIONAL"
    KEYWORD_ONLY = "KEYWORD_ONLY"
    VAR_KEYWORD = "VAR_KEYWORD"
    empty = type("_empty", (), {"__repr__": lambda self: "<empty>"})()

    def __init__(self, name, kind, default=empty, annotation=empty):
        self.name = name
        self.kind = kind
        self.default = default
        self.annotation = annotation

    def __repr__(self):
        return "<Parameter %r>" % self.name

    def __str__(self):
        text = self.name
        if self.kind == "VAR_POSITIONAL":
            text = "*" + text
        elif self.kind == "VAR_KEYWORD":
            text = "**" + text
        if self.default is not Parameter.empty:
            text += "=%r" % (self.default,)
        return text


class Signature:
    empty = Parameter.empty

    def __init__(self, parameters=None, return_annotation=Parameter.empty):
        self.parameters = OrderedDict((p.name, p) for p in (parameters or []))
        self.return_annotation = return_annotation

    def __repr__(self):
        return "<Signature (%s)>" % ", ".join(str(p) for p in self.parameters.values())

    __str__ = lambda self: "(%s)" % ", ".join(str(p) for p in self.parameters.values())

    def bind(self, *args, **kwargs):
        return _BoundArguments(self, args, kwargs)


class _BoundArguments:
    def __init__(self, sig, args, kwargs):
        self.signature = sig
        self.arguments = OrderedDict()
        names = list(sig.parameters)
        for name, value in zip(names, args):
            self.arguments[name] = value
        self.arguments.update(kwargs)


def signature(obj):
    fn = obj
    if isinstance(obj, type):
        fn = getattr(obj, "__init__", None)
    elif not callable(obj):
        raise TypeError("%r is not a callable object" % (obj,))
    while hasattr(fn, "__wrapped__"):
        fn = fn.__wrapped__
    code = getattr(fn, "__code__", None)
    names = list(getattr(code, "co_varnames", ())) if code is not None else []
    argcount = getattr(code, "co_argcount", len(names)) if code is not None else len(names)
    defaults = getattr(fn, "__defaults__", None) or ()
    kwdefaults = getattr(fn, "__kwdefaults__", None) or {}
    annotations = getattr(fn, "__annotations__", None) or {}
    params = []
    skip = 1 if isinstance(obj, type) or getattr(obj, "__self__", None) is not None else 0
    for i, name in enumerate(names):
        if i < skip:
            continue
        if i < argcount:
            offset = i - (argcount - len(defaults))
            default = defaults[offset] if offset >= 0 else Parameter.empty
            params.append(Parameter(name, Parameter.POSITIONAL_OR_KEYWORD, default, annotations.get(name, Parameter.empty)))
        else:
            params.append(Parameter(name, Parameter.KEYWORD_ONLY, kwdefaults.get(name, Parameter.empty), annotations.get(name, Parameter.empty)))
    return Signature(params, annotations.get("return", Parameter.empty))


def getfullargspec(fn):
    sig = signature(fn)
    return _ArgSpec([p.name for p in sig.parameters.values()])


class _ArgSpec:
    def __init__(self, args):
        self.args = args
        self.varargs = None
        self.varkw = None
        self.defaults = None


def isfunction(obj):
    return type(obj).__name__ == "function"


def ismethod(obj):
    return type(obj).__name__ == "method"


def isclass(obj):
    return isinstance(obj, type)


def ismodule(obj):
    return type(obj).__name__ == "module"


def isbuiltin(obj):
    return type(obj).__name__ == "builtin_function_or_method"


def isroutine(obj):
    return callable(obj) and not isclass(obj)


def getmembers(obj, predicate=None):
    out = []
    for name in dir(obj):
        try:
            value = getattr(obj, name)
        except AttributeError:
            continue
        if predicate is None or predicate(value):
            out.append((name, value))
    return sorted(out)


def getdoc(obj):
    doc = getattr(obj, "__doc__", None)
    return doc.strip() if isinstance(doc, str) else None


def getsource(obj):
    raise OSError("source code is not available in the Python sandbox")


def stack():
    return []


def currentframe():
    return None
