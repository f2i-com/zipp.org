"""argparse for Zipp: positionals, options with type/default/nargs/choices/
required, store_true/store_false/count/append, subparsers, help output and
`sys.argv` from the host."""
import sys

SUPPRESS = "==SUPPRESS=="


class ArgumentError(Exception):
    pass


class ArgumentTypeError(Exception):
    pass


class Namespace:
    def __init__(self, **kw):
        for k, v in kw.items():
            setattr(self, k, v)

    def __repr__(self):
        return "Namespace(%s)" % ", ".join("%s=%r" % (k, v) for k, v in sorted(vars(self).items()))

    def __eq__(self, other):
        return isinstance(other, Namespace) and vars(self) == vars(other)

    def __contains__(self, key):
        return key in vars(self)


class _Action:
    def __init__(self, flags, dest, kind, type=None, default=None, nargs=None, choices=None, required=False, help=None, metavar=None, const=None):
        self.flags = flags
        self.dest = dest
        self.kind = kind
        self.type = type
        self.default = default
        self.nargs = nargs
        self.choices = choices
        self.required = required
        self.help = help
        self.metavar = metavar
        self.const = const

    @property
    def positional(self):
        return not self.flags


class ArgumentParser:
    def __init__(self, prog=None, usage=None, description=None, epilog=None, add_help=True, formatter_class=None, parents=None, **kw):
        self.prog = prog or (sys.argv[0].rsplit("/", 1)[-1] if sys.argv else "prog")
        self.usage = usage
        self.description = description
        self.epilog = epilog
        self._actions = []
        self._subparsers = None
        self._defaults = {}
        self.add_help = add_help
        for parent in parents or []:
            self._actions.extend(parent._actions)

    def add_argument(self, *flags, **kw):
        action = kw.pop("action", "store")
        dest = kw.pop("dest", None)
        if flags and not flags[0].startswith("-"):
            dest = dest or flags[0]
            flags = ()
        elif dest is None:
            longest = sorted(flags, key=len)[-1]
            dest = longest.lstrip("-").replace("-", "_")
        default = kw.pop("default", None)
        if action == "store_true":
            act = _Action(flags, dest, "store_true", default=False if default is None else default, help=kw.get("help"))
        elif action == "store_false":
            act = _Action(flags, dest, "store_false", default=True if default is None else default, help=kw.get("help"))
        elif action == "count":
            act = _Action(flags, dest, "count", default=0 if default is None else default, help=kw.get("help"))
        elif action == "append":
            act = _Action(flags, dest, "append", type=kw.get("type"), default=default, choices=kw.get("choices"), help=kw.get("help"), metavar=kw.get("metavar"))
        elif action == "store_const":
            act = _Action(flags, dest, "store_const", default=default, const=kw.get("const"), help=kw.get("help"))
        elif action == "help":
            return None
        elif action == "version":
            act = _Action(flags, dest, "version", const=kw.get("version"), help=kw.get("help"))
        else:
            act = _Action(flags, dest, "store", type=kw.get("type"), default=default, nargs=kw.get("nargs"), choices=kw.get("choices"), required=kw.get("required", False), help=kw.get("help"), metavar=kw.get("metavar"), const=kw.get("const"))
        self._actions.append(act)
        return act

    def add_argument_group(self, *a, **kw):
        return self

    def add_mutually_exclusive_group(self, required=False):
        return self

    def set_defaults(self, **kw):
        self._defaults.update(kw)

    def add_subparsers(self, dest=None, **kw):
        self._subparsers = _SubParsers(self, dest)
        return self._subparsers

    def error(self, message):
        self.print_usage(sys.stderr)
        sys.stderr.write("%s: error: %s\n" % (self.prog, message))
        sys.exit(2)

    def exit(self, status=0, message=None):
        if message:
            sys.stderr.write(message)
        sys.exit(status)

    def format_usage(self):
        if self.usage:
            return "usage: %s\n" % self.usage
        parts = [self.prog]
        for a in self._actions:
            if a.positional:
                parts.append(_metavar(a))
            else:
                text = "%s %s" % (a.flags[0], _metavar(a)) if a.kind in ("store", "append") else a.flags[0]
                parts.append(text if a.required else "[%s]" % text)
        if self._subparsers is not None:
            parts.append("{%s} ..." % ",".join(self._subparsers.names))
        return "usage: " + " ".join(parts) + "\n"

    def format_help(self):
        lines = [self.format_usage().rstrip("\n"), ""]
        if self.description:
            lines += [self.description, ""]
        positionals = [a for a in self._actions if a.positional]
        options = [a for a in self._actions if not a.positional]
        if positionals or self._subparsers is not None:
            lines.append("positional arguments:")
            for a in positionals:
                lines.append("  %-22s%s" % (_metavar(a), a.help or ""))
            if self._subparsers is not None:
                lines.append("  {%s}" % ",".join(self._subparsers.names))
            lines.append("")
        lines.append("options:")
        if self.add_help:
            lines.append("  %-22s%s" % ("-h, --help", "show this help message and exit"))
        for a in options:
            spec = ", ".join(a.flags)
            if a.kind in ("store", "append"):
                spec += " " + _metavar(a)
            lines.append("  %-22s%s" % (spec, a.help or "") if len(spec) <= 20 else "  %s\n%24s%s" % (spec, "", a.help or ""))
        if self.epilog:
            lines += ["", self.epilog]
        return "\n".join(lines) + "\n"

    def print_usage(self, file=None):
        (file or sys.stdout).write(self.format_usage())

    def print_help(self, file=None):
        (file or sys.stdout).write(self.format_help())

    def parse_known_args(self, args=None, namespace=None):
        args = list(sys.argv[1:] if args is None else args)
        ns = namespace or Namespace()
        for a in self._actions:
            if a.kind == "version":
                continue
            setattr(ns, a.dest, self._defaults.get(a.dest, a.default))
        for k, v in self._defaults.items():
            if not hasattr(ns, k):
                setattr(ns, k, v)
        positionals = [a for a in self._actions if a.positional]
        seen = set()
        rest = []
        i = 0
        while i < len(args):
            arg = args[i]
            if arg == "--":
                rest.extend(args[i + 1:])
                break
            if arg in ("-h", "--help") and self.add_help:
                self.print_help()
                sys.exit(0)
            if arg.startswith("-") and arg != "-" and not _looks_numeric(arg):
                name, value = (arg.split("=", 1) + [None])[:2] if arg.startswith("--") and "=" in arg else (arg, None)
                act = self._option(name)
                if act is None:
                    rest.append(arg)
                    i += 1
                    continue
                seen.add(act.dest)
                if act.kind == "store_true":
                    setattr(ns, act.dest, True)
                elif act.kind == "store_false":
                    setattr(ns, act.dest, False)
                elif act.kind == "store_const":
                    setattr(ns, act.dest, act.const)
                elif act.kind == "count":
                    setattr(ns, act.dest, (getattr(ns, act.dest) or 0) + 1)
                elif act.kind == "version":
                    sys.stdout.write(str(act.const) + "\n")
                    sys.exit(0)
                else:
                    if value is not None:
                        values = [value]
                        i += 1
                    else:
                        i += 1
                        values, i = self._collect(args, i, act)
                    converted = self._convert(act, values)
                    if act.kind == "append":
                        current = getattr(ns, act.dest) or []
                        current = list(current) + [converted]
                        setattr(ns, act.dest, current)
                    else:
                        setattr(ns, act.dest, converted)
                continue
            if self._subparsers is not None and arg in self._subparsers.parsers and all(a.dest in seen for a in positionals):
                setattr(ns, self._subparsers.dest or "command", arg)
                sub = self._subparsers.parsers[arg]
                sub.parse_known_args(args[i + 1:], ns)
                return ns, rest
            target = None
            for a in positionals:
                if a.dest not in seen:
                    target = a
                    break
            if target is None:
                rest.append(arg)
                i += 1
                continue
            values, i = self._collect(args, i, target, positional=True)
            seen.add(target.dest)
            setattr(ns, target.dest, self._convert(target, values))
        for a in positionals:
            if a.dest not in seen and a.nargs not in ("?", "*"):
                self.error("the following arguments are required: %s" % a.dest)
        missing = [a.flags[0] for a in self._actions if not a.positional and a.required and a.dest not in seen]
        if missing:
            self.error("the following arguments are required: %s" % ", ".join(missing))
        if self._subparsers is not None and self._subparsers.required and not hasattr(ns, self._subparsers.dest or "command"):
            self.error("the following arguments are required: %s" % (self._subparsers.dest or "command"))
        return ns, rest

    def parse_args(self, args=None, namespace=None):
        ns, rest = self.parse_known_args(args, namespace)
        if rest:
            self.error("unrecognized arguments: %s" % " ".join(rest))
        return ns

    def _option(self, name):
        for a in self._actions:
            if name in a.flags:
                return a
        for a in self._actions:
            for f in a.flags:
                if f.startswith("--") and f.startswith(name) and len(name) > 2:
                    return a
        return None

    def _collect(self, args, i, act, positional=False):
        n = act.nargs
        if n is None:
            if i >= len(args) or (args[i].startswith("-") and not _looks_numeric(args[i]) and args[i] != "-"):
                if positional:
                    return [], i
                self.error("argument %s: expected one argument" % "/".join(act.flags or [act.dest]))
            return [args[i]], i + 1
        if n == "?":
            if i < len(args) and not (args[i].startswith("-") and not _looks_numeric(args[i])):
                return [args[i]], i + 1
            return [], i
        values = []
        while i < len(args) and not (args[i].startswith("-") and not _looks_numeric(args[i]) and args[i] != "-"):
            values.append(args[i])
            i += 1
            if isinstance(n, int) and len(values) == n:
                break
        if n == "+" and not values:
            self.error("argument %s: expected at least one argument" % "/".join(act.flags or [act.dest]))
        if isinstance(n, int) and len(values) != n:
            self.error("argument %s: expected %d argument(s)" % ("/".join(act.flags or [act.dest]), n))
        return values, i

    def _convert(self, act, values):
        out = []
        for v in values:
            if act.type is not None:
                try:
                    v = act.type(v)
                except (ValueError, TypeError, ArgumentTypeError) as e:
                    name = getattr(act.type, "__name__", "value")
                    self.error("argument %s: invalid %s value: %r" % ("/".join(act.flags or [act.dest]), name, v))
            if act.choices is not None and v not in act.choices:
                self.error("argument %s: invalid choice: %r (choose from %s)" % ("/".join(act.flags or [act.dest]), v, ", ".join(repr(c) for c in act.choices)))
            out.append(v)
        n = act.nargs
        if n is None:
            return out[0] if out else act.default
        if n == "?":
            return out[0] if out else (act.const if act.const is not None else act.default)
        return out


class _SubParsers:
    def __init__(self, parent, dest):
        self.parent = parent
        self.dest = dest
        self.parsers = {}
        self.names = []
        self.required = False

    def add_parser(self, name, **kw):
        p = ArgumentParser(prog="%s %s" % (self.parent.prog, name), **kw)
        self.parsers[name] = p
        self.names.append(name)
        return p


def _metavar(a):
    if a.metavar:
        return a.metavar
    name = a.dest.upper() if not a.positional else a.dest
    if a.nargs in ("*", "+"):
        return "%s [%s ...]" % (name, name)
    if a.nargs == "?":
        return "[%s]" % name
    return name


def _looks_numeric(s):
    try:
        float(s)
        return True
    except ValueError:
        return False


class FileType:
    def __init__(self, mode="r"):
        self.mode = mode

    def __call__(self, name):
        return open(name, self.mode)


class RawDescriptionHelpFormatter:
    pass


class ArgumentDefaultsHelpFormatter(RawDescriptionHelpFormatter):
    pass


HelpFormatter = RawDescriptionHelpFormatter
RawTextHelpFormatter = RawDescriptionHelpFormatter
