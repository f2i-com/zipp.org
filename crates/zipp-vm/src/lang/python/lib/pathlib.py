"""pathlib for Zipp: POSIX-style paths over the program's virtual filesystem
(the project folder is the root and the working directory)."""
import os


class PurePath:
    def __init__(self, *parts):
        self._path = _join(*parts)

    def __fspath__(self):
        return self._path

    def __str__(self):
        return self._path if self._path else "."

    def __repr__(self):
        return "%s(%r)" % (type(self).__name__, str(self))

    def __truediv__(self, other):
        return type(self)(self._path, str(other))

    def __rtruediv__(self, other):
        return type(self)(str(other), self._path)

    def __eq__(self, other):
        # Paths are compared as written (after `_join`'s cleanup); `..` is not collapsed.
        return isinstance(other, PurePath) and other._path == self._path

    def __hash__(self):
        return hash(self._path)

    def __lt__(self, other):
        if not isinstance(other, PurePath):
            return NotImplemented
        return self.parts < other.parts

    @property
    def parts(self):
        p = self._path
        parts = [x for x in p.split("/") if x and x != "."]
        return tuple((["/"] if p.startswith("/") else []) + parts)

    @property
    def name(self):
        return self._path.rstrip("/").rsplit("/", 1)[-1] if self._path not in ("", "/", ".") else ""

    @property
    def stem(self):
        n = self.name
        return n.rsplit(".", 1)[0] if "." in n[1:] else n

    @property
    def suffix(self):
        n = self.name
        return "." + n.rsplit(".", 1)[1] if "." in n[1:] else ""

    @property
    def suffixes(self):
        n = self.name
        return ["." + s for s in n.split(".")[1:]] if "." in n[1:] else []

    @property
    def parent(self):
        p = self._path.rstrip("/")
        if "/" not in p:
            return type(self)("/" if self._path.startswith("/") else "")
        head = p.rsplit("/", 1)[0]
        return type(self)(head if head else "/")

    @property
    def parents(self):
        out, cur = [], self
        while True:
            nxt = cur.parent
            if str(nxt) == str(cur):
                break
            out.append(nxt)
            cur = nxt
        return out

    @property
    def anchor(self):
        return "/" if self._path.startswith("/") else ""

    def is_absolute(self):
        return self._path.startswith("/")

    def joinpath(self, *parts):
        return type(self)(self._path, *[str(p) for p in parts])

    def with_name(self, name):
        return self.parent / name

    def with_suffix(self, suffix):
        return self.parent / (self.stem + suffix)

    def with_stem(self, stem):
        return self.parent / (stem + self.suffix)

    def relative_to(self, other):
        base = _norm(str(other)).rstrip("/")
        me = _norm(self._path)
        if base and not (me == base or me.startswith(base + "/")):
            raise ValueError("%r is not in the subpath of %r" % (self._path, str(other)))
        return type(self)(me[len(base):].lstrip("/") if base else me)

    def is_relative_to(self, other):
        try:
            self.relative_to(other)
            return True
        except ValueError:
            return False

    def as_posix(self):
        return self._path

    def match(self, pattern):
        # Matched from the right, one component per pattern component; an
        # absolute pattern must match the whole path.
        pat = PurePath(pattern)
        pattern_parts = pat.parts
        if not pattern_parts:
            raise ValueError("empty pattern")
        parts = self.parts
        if pat.is_absolute() and len(parts) != len(pattern_parts):
            return False
        if len(pattern_parts) > len(parts):
            return False
        for part, pat_part in zip(reversed(parts), reversed(pattern_parts)):
            if not _fnmatch(part, pat_part):
                return False
        return True


PurePosixPath = PurePath


class Path(PurePath):
    def resolve(self, strict=False):
        return Path("/" + _norm(self._path))

    def absolute(self):
        return self.resolve()

    def expanduser(self):
        return self

    @classmethod
    def cwd(cls):
        return cls(os.getcwd())

    @classmethod
    def home(cls):
        return cls("/")

    def exists(self):
        return os.path.exists(self._path)

    def is_file(self):
        return os.path.isfile(self._path)

    def is_dir(self):
        return os.path.isdir(self._path)

    def open(self, mode="r", encoding=None, errors=None, newline=None):
        return open(self._path, mode, encoding=encoding)

    def read_text(self, encoding=None, errors=None):
        with open(self._path, "r", encoding=encoding) as f:
            return f.read()

    def read_bytes(self):
        with open(self._path, "rb") as f:
            return f.read()

    def write_text(self, text, encoding=None, errors=None, newline=None):
        with open(self._path, "w", encoding=encoding) as f:
            return f.write(text)

    def write_bytes(self, data):
        with open(self._path, "wb") as f:
            return f.write(data)

    def mkdir(self, mode=0o777, parents=False, exist_ok=False):
        if self.exists():
            if exist_ok and self.is_dir():
                return
            raise FileExistsError("[Errno 17] File exists: '%s'" % self._path)
        if parents:
            os.makedirs(self._path, exist_ok=True)
            return
        parent = self.parent._path
        if parent and not os.path.isdir(parent):
            raise FileNotFoundError("[Errno 2] No such file or directory: '%s'" % self._path)
        os.mkdir(self._path)

    def touch(self, exist_ok=True):
        if not self.exists():
            self.write_bytes(b"")

    def unlink(self, missing_ok=False):
        try:
            os.remove(self._path)
        except FileNotFoundError:
            if not missing_ok:
                raise

    def rmdir(self):
        os.rmdir(self._path)

    def rename(self, target):
        os.rename(self._path, str(target))
        return Path(str(target))

    replace = rename

    def iterdir(self):
        for name in os.listdir(self._path):
            yield self / name

    def glob(self, pattern):
        return _glob(self, pattern)

    def rglob(self, pattern):
        return _glob(self, "**/" + pattern)

    def stat(self):
        return _Stat(os.path.getsize(self._path))

    def samefile(self, other):
        return _norm(self._path) == _norm(str(other))


PosixPath = Path


class _Stat:
    def __init__(self, size):
        self.st_size = size
        self.st_mtime = 0
        self.st_mode = 0o100644


def _join(*parts):
    out = ""
    for p in parts:
        p = str(p)
        if p == "":
            continue
        if p.startswith("/"):
            out = p
        elif out == "" or out.endswith("/"):
            out += p
        else:
            out += "/" + p
    # As pathlib does: drop empty and "." components and a trailing slash,
    # but keep ".." (collapsing it could change what a symlinked path means).
    rest = "/".join(s for s in out.split("/") if s not in ("", "."))
    return "/" + rest if out.startswith("/") else (rest or ".")


def _norm(p):
    parts = []
    for s in p.split("/"):
        if s in ("", "."):
            continue
        if s == "..":
            if parts:
                parts.pop()
            continue
        parts.append(s)
    return "/".join(parts)


def _fnmatch(name, pattern):
    """Shell-style matching: `*`, `?`, `[...]`, no path separators in `*`."""
    return _match(name, 0, pattern, 0)


def _match(s, i, p, j):
    while j < len(p):
        c = p[j]
        if c == "*":
            j += 1
            if j == len(p):
                return "/" not in s[i:]
            while i <= len(s):
                if _match(s, i, p, j):
                    return True
                if i < len(s) and s[i] == "/":
                    return False
                i += 1
            return False
        if i >= len(s):
            return False
        if c == "?":
            if s[i] == "/":
                return False
        elif c == "[":
            end = p.find("]", j + 1)
            if end < 0:
                if s[i] != "[":
                    return False
            else:
                chars = p[j + 1:end]
                negate = chars.startswith("!")
                if negate:
                    chars = chars[1:]
                hit = False
                k = 0
                while k < len(chars):
                    if k + 2 < len(chars) and chars[k + 1] == "-":
                        if chars[k] <= s[i] <= chars[k + 2]:
                            hit = True
                        k += 3
                    else:
                        if chars[k] == s[i]:
                            hit = True
                        k += 1
                if hit == negate:
                    return False
                j = end
        elif c != s[i]:
            return False
        i += 1
        j += 1
    return i == len(s)


def _child(base, name):
    return name if base == "." else (base + name if base == "/" else base + "/" + name)


def _dirs(base):
    """`base` and every directory below it."""
    out = [base]
    for name in sorted(os.listdir(base)):
        full = _child(base, name)
        if os.path.isdir(full):
            out.extend(_dirs(full))
    return out


def _glob(root, pattern):
    segments = PurePath(pattern).parts
    if not segments:
        raise ValueError("Unacceptable pattern: %r" % pattern)
    if pattern.startswith("/"):
        raise NotImplementedError("Non-relative patterns are unsupported")
    for seg in segments:
        if "**" in seg and seg != "**":
            raise ValueError("Invalid pattern: '**' can only be an entire path component")
    candidates = [root._path]
    for seg in segments:
        nxt, seen = [], set()
        for c in candidates:
            if not os.path.isdir(c):
                continue
            if seg == "**":
                # This directory and every directory below it.
                matches = _dirs(c)
            else:
                matches = [_child(c, name) for name in sorted(os.listdir(c)) if _fnmatch(name, seg)]
            for m in matches:
                if m not in seen:
                    seen.add(m)
                    nxt.append(m)
        candidates = nxt
    if pattern.endswith("/"):
        # A trailing separator selects directories only.
        candidates = [c for c in candidates if os.path.isdir(c)]
    return [Path(c) for c in candidates]
