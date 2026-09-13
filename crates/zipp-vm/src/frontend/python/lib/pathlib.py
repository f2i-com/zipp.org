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
        return isinstance(other, PurePath) and _norm(other._path) == _norm(self._path)

    def __hash__(self):
        return hash(_norm(self._path))

    def __lt__(self, other):
        return str(self) < str(other)

    @property
    def parts(self):
        p = self._path
        parts = [x for x in p.split("/") if x]
        return tuple((["/"] if p.startswith("/") else []) + parts)

    @property
    def name(self):
        return self._path.rstrip("/").rsplit("/", 1)[-1] if self._path not in ("", "/") else ""

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
        return _fnmatch(self.name, pattern) if "/" not in pattern else _fnmatch(_norm(self._path), pattern)


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
        if self.exists() and not exist_ok:
            raise FileExistsError("[Errno 17] File exists: '%s'" % self._path)
        if parents or exist_ok:
            os.makedirs(self._path, exist_ok=True)
        else:
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
        return _glob(self, pattern, recursive=False)

    def rglob(self, pattern):
        return _glob(self, "**/" + pattern, recursive=True)

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
    return out


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


def _walk(root):
    """Every path under `root` (files and directories), root-relative."""
    base = _norm(str(root))
    out = []
    for name in sorted(os.listdir(base)):
        full = (base + "/" + name) if base else name
        out.append(full)
        if os.path.isdir(full):
            out.extend(_walk(full))
    return out


def _glob(root, pattern, recursive):
    base = _norm(str(root))
    results = []
    if "**" in pattern:
        tail = pattern.replace("**/", "")
        for path in _walk(root):
            rel = path[len(base) + 1:] if base else path
            if _fnmatch(rel.rsplit("/", 1)[-1], tail) or _fnmatch(rel, tail):
                results.append(Path(path))
        return results
    segments = pattern.split("/")
    candidates = [base]
    for seg in segments:
        nxt = []
        for c in candidates:
            if not os.path.isdir(c) and c != "":
                continue
            for name in sorted(os.listdir(c)):
                if _fnmatch(name, seg):
                    nxt.append((c + "/" + name) if c else name)
        candidates = nxt
    return [Path(c) for c in candidates]
