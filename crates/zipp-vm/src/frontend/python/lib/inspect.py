"""inspect for Zipp: signatures from the runtime's function records."""
from collections import OrderedDict

_void = object()  # "not given" for the replace() methods


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
        if self.annotation is not Parameter.empty:
            text += ": " + _format_annotation(self.annotation)
        if self.default is not Parameter.empty:
            text += (" = %r" if self.annotation is not Parameter.empty else "=%r") % (self.default,)
        return text

    def replace(self, name=_void, kind=_void, default=_void, annotation=_void):
        return Parameter(self.name if name is _void else name, self.kind if kind is _void else kind,
                         self.default if default is _void else default, self.annotation if annotation is _void else annotation)


def _format_annotation(annotation):
    if isinstance(annotation, type):
        module = getattr(annotation, "__module__", "builtins")
        return annotation.__qualname__ if module == "builtins" else "%s.%s" % (module, annotation.__qualname__)
    return repr(annotation)


class Signature:
    empty = Parameter.empty

    def __init__(self, parameters=None, return_annotation=Parameter.empty):
        self.parameters = OrderedDict((p.name, p) for p in (parameters or []))
        self.return_annotation = return_annotation

    def __repr__(self):
        return "<Signature %s>" % self

    def __str__(self):
        parts = []
        star = False
        for p in self.parameters.values():
            if p.kind == Parameter.VAR_POSITIONAL:
                star = True
            elif p.kind == Parameter.KEYWORD_ONLY and not star:
                # Keyword-only parameters after a bare `*`.
                parts.append("*")
                star = True
            parts.append(str(p))
        text = "(%s)" % ", ".join(parts)
        if self.return_annotation is not Parameter.empty:
            text += " -> " + _format_annotation(self.return_annotation)
        return text

    def replace(self, parameters=_void, return_annotation=_void):
        return Signature(list(self.parameters.values()) if parameters is _void else parameters,
                         self.return_annotation if return_annotation is _void else return_annotation)

    def bind(self, *args, **kwargs):
        return _BoundArguments(self, args, kwargs)


class _BoundArguments:
    def __init__(self, sig, args, kwargs):
        self.signature = sig
        self.arguments = {}
        names = list(sig.parameters)
        for name, value in zip(names, args):
            self.arguments[name] = value
        self.arguments.update(kwargs)


def signature(obj, follow_wrapped=True):
    # The runtime's code records carry the named parameters (positional, then
    # keyword-only) but not `*args`/`**kwargs` or the positional-only marker,
    # so those are missing from the signature.
    fn = obj
    if isinstance(obj, type):
        fn = getattr(obj, "__init__", None)
    elif not callable(obj):
        raise TypeError("%r is not a callable object" % (obj,))
    if type(obj).__name__ == "partial" and hasattr(obj, "func"):
        return _partial_signature(obj)
    if follow_wrapped:
        while hasattr(fn, "__wrapped__"):
            fn = fn.__wrapped__
    # A bound method (or classmethod) describes its function without the bound first argument.
    fn = getattr(fn, "__func__", fn)
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


def _partial_signature(obj):
    """functools.partial: the bound positionals are gone, a bound keyword
    becomes that parameter's default (and makes it and the positional
    parameters after it keyword-only), as in CPython."""
    params = list(signature(obj.func).parameters.values())
    keywords = obj.keywords or {}
    out = []
    consumed = 0
    keyword_only = False
    for p in params:
        if p.kind in (Parameter.POSITIONAL_ONLY, Parameter.POSITIONAL_OR_KEYWORD) and consumed < len(obj.args):
            consumed += 1
            continue
        kind, default = p.kind, p.default
        if p.name in keywords:
            default = keywords[p.name]
            if kind == Parameter.POSITIONAL_OR_KEYWORD:
                keyword_only = True
        if keyword_only and kind == Parameter.POSITIONAL_OR_KEYWORD:
            kind = Parameter.KEYWORD_ONLY
        out.append(Parameter(p.name, kind, default, p.annotation))
    return Signature(out)


def getfullargspec(fn):
    sig = signature(fn, follow_wrapped=False)
    params = list(sig.parameters.values())
    args = [p.name for p in params if p.kind in (Parameter.POSITIONAL_ONLY, Parameter.POSITIONAL_OR_KEYWORD)]
    defaults = tuple(p.default for p in params if p.name in args and p.default is not Parameter.empty)
    kwonly = [p for p in params if p.kind == Parameter.KEYWORD_ONLY]
    kwdefaults = {p.name: p.default for p in kwonly if p.default is not Parameter.empty}
    return _ArgSpec(args, defaults or None, [p.name for p in kwonly], kwdefaults or None)


class _ArgSpec:
    def __init__(self, args, defaults=None, kwonlyargs=(), kwonlydefaults=None):
        self.args = args
        self.varargs = None
        self.varkw = None
        self.defaults = defaults
        self.kwonlyargs = list(kwonlyargs)
        self.kwonlydefaults = kwonlydefaults


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
