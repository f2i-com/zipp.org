"""A pickle subset for Zipp: loading protocols 0-5 (what PyTorch checkpoints
and most data files use) and dumping plain data as protocol 2 (which every
reader, including PyTorch's weights-only loader, accepts), with persistent
ids and a restricted `find_class` for safety. Arbitrary classes are not
instantiated unless `find_class` allows them; the inert data globals a
protocol-2 stream uses for sets, bytes and bytearrays (`builtins.set`,
`_codecs.encode`, ...) are always resolved."""
import struct
from collections import Counter, OrderedDict

HIGHEST_PROTOCOL = 2
DEFAULT_PROTOCOL = 2


class PickleError(Exception):
    pass


class PicklingError(PickleError):
    pass


class UnpicklingError(PickleError):
    pass


_MARK = object()


def _codecs_encode(obj, encoding="utf-8", errors="strict"):
    # What `_codecs.encode` does for the (str, "latin1") pairs protocols 0-2
    # write for bytes.
    if isinstance(obj, bytes):
        return obj
    return obj.encode(encoding, errors)


_codecs_encode.__module__ = "_codecs"
_codecs_encode.__name__ = "encode"

# Globals that only build plain data; resolved before any custom find_class.
# The set matches what PyTorch's weights-only loader allows (so frozenset,
# which it refuses, is left to find_class).
_DATA_GLOBALS = {
    ("builtins", "set"): set, ("builtins", "bytearray"): bytearray,
    ("builtins", "bytes"): bytes, ("builtins", "complex"): complex, ("_codecs", "encode"): _codecs_encode,
    ("collections", "OrderedDict"): OrderedDict, ("collections", "Counter"): Counter,
}
# Python 2 module names that protocol 0-2 streams use (pickle's fix_imports).
_PY2_MODULES = {"__builtin__": "builtins", "copy_reg": "copyreg"}


class Unpickler:
    persistent_load = None

    def __init__(self, data, persistent_load=None, find_class=None, **kwargs):
        if hasattr(data, "read"):
            data = data.read()
        self._data = data if isinstance(data, bytes) else bytes(data)
        self._pos = 0
        if persistent_load is not None:
            self.persistent_load = persistent_load
        self._find_class = find_class
        self.memo = {}
        # ids of the objects REDUCE/NEWOBJ made during this load: the only
        # ones BUILD may set state on (never a class find_class handed out).
        self._made = set()

    def _read(self, n):
        # A negative length must never move the read position backwards.
        if n < 0 or self._pos + n > len(self._data):
            raise UnpicklingError("pickle data was truncated")
        out = self._data[self._pos:self._pos + n]
        self._pos += n
        return out

    def _line(self):
        end = self._data.index(b"\n", self._pos)
        out = self._data[self._pos:end]
        self._pos = end + 1
        return out

    def _global(self, module, name):
        module = _PY2_MODULES.get(module, module)
        found = _DATA_GLOBALS.get((module, name))
        if found is not None:
            return found
        return self.find_class(module, name)

    def find_class(self, module, name):
        if self._find_class is not None:
            return self._find_class(module, name)
        if module == "builtins" and name in ("frozenset", "list", "dict", "tuple", "range", "slice"):
            return {"frozenset": frozenset, "list": list, "dict": dict, "tuple": tuple, "range": range, "slice": slice}[name]
        raise UnpicklingError("global %s.%s is forbidden" % (module, name))

    def load(self):
        stack = []
        metastack = []
        data = self._data
        while True:
            if self._pos >= len(data):
                raise EOFError("Ran out of input")
            op = data[self._pos]
            self._pos += 1
            if op == 0x80:  # PROTO
                self._read(1)
            elif op == 0x2e:  # STOP
                return stack[-1]
            elif op == 0x28:  # MARK
                metastack.append(stack)
                stack = []
            elif op == 0x4e:  # NONE
                stack.append(None)
            elif op == 0x88:  # NEWTRUE
                stack.append(True)
            elif op == 0x89:  # NEWFALSE
                stack.append(False)
            elif op == 0x4b:  # BININT1
                stack.append(self._read(1)[0])
            elif op == 0x4d:  # BININT2
                stack.append(struct.unpack("<H", self._read(2))[0])
            elif op == 0x4a:  # BININT
                stack.append(struct.unpack("<i", self._read(4))[0])
            elif op == 0x8a:  # LONG1
                n = self._read(1)[0]
                stack.append(_decode_long(self._read(n)))
            elif op == 0x8b:  # LONG4
                n = struct.unpack("<i", self._read(4))[0]
                if n < 0:
                    raise UnpicklingError("LONG pickle has negative byte count")
                stack.append(_decode_long(self._read(n)))
            elif op == 0x49:  # INT (text)
                text = self._line()
                stack.append(True if text == b"01" else False if text == b"00" else int(text))
            elif op == 0x4c:  # LONG (text)
                stack.append(int(self._line().rstrip(b"L")))
            elif op == 0x47:  # BINFLOAT
                stack.append(struct.unpack(">d", self._read(8))[0])
            elif op == 0x46:  # FLOAT (text)
                stack.append(float(self._line().decode("ascii")))
            elif op == 0x58:  # BINUNICODE
                n = struct.unpack("<I", self._read(4))[0]
                stack.append(self._read(n).decode("utf-8"))
            elif op == 0x8c:  # SHORT_BINUNICODE
                n = self._read(1)[0]
                stack.append(self._read(n).decode("utf-8"))
            elif op == 0x8d:  # BINUNICODE8
                n = struct.unpack("<Q", self._read(8))[0]
                stack.append(self._read(n).decode("utf-8"))
            elif op == 0x56:  # UNICODE (text, raw-unicode-escape)
                stack.append(_raw_unicode_escape(self._line()))
            elif op == 0x53:  # STRING (text, a quoted repr)
                text = self._line().rstrip()
                if len(text) < 2 or text[:1] != text[-1:] or text[:1] not in (b"'", b'"'):
                    raise UnpicklingError("the STRING opcode argument must be quoted")
                stack.append(_unescape_string(text[1:-1]))
            elif op == 0x55:  # SHORT_BINSTRING
                n = self._read(1)[0]
                stack.append(self._read(n).decode("latin-1"))
            elif op == 0x54:  # BINSTRING
                n = struct.unpack("<i", self._read(4))[0]
                if n < 0:
                    raise UnpicklingError("BINSTRING pickle has negative byte count")
                stack.append(self._read(n).decode("latin-1"))
            elif op == 0x43:  # SHORT_BINBYTES
                n = self._read(1)[0]
                stack.append(self._read(n))
            elif op == 0x42:  # BINBYTES
                n = struct.unpack("<I", self._read(4))[0]
                stack.append(self._read(n))
            elif op == 0x8e:  # BINBYTES8
                n = struct.unpack("<Q", self._read(8))[0]
                stack.append(self._read(n))
            elif op == 0x5d:  # EMPTY_LIST
                stack.append([])
            elif op == 0x7d:  # EMPTY_DICT
                stack.append({})
            elif op == 0x29:  # EMPTY_TUPLE
                stack.append(())
            elif op == 0x8f:  # EMPTY_SET
                stack.append(set())
            elif op == 0x85:  # TUPLE1
                stack.append((stack.pop(),))
            elif op == 0x86:  # TUPLE2
                b = stack.pop()
                a = stack.pop()
                stack.append((a, b))
            elif op == 0x87:  # TUPLE3
                c = stack.pop()
                b = stack.pop()
                a = stack.pop()
                stack.append((a, b, c))
            elif op == 0x74:  # TUPLE
                items = stack
                stack = metastack.pop()
                stack.append(tuple(items))
            elif op == 0x6c:  # LIST
                items = stack
                stack = metastack.pop()
                stack.append(list(items))
            elif op == 0x64:  # DICT
                items = stack
                stack = metastack.pop()
                d = {}
                for i in range(0, len(items), 2):
                    d[items[i]] = items[i + 1]
                stack.append(d)
            elif op == 0x61:  # APPEND
                v = stack.pop()
                stack[-1].append(v)
            elif op == 0x65:  # APPENDS
                items = stack
                stack = metastack.pop()
                stack[-1].extend(items)
            elif op == 0x73:  # SETITEM
                v = stack.pop()
                k = stack.pop()
                stack[-1][k] = v
            elif op == 0x75:  # SETITEMS
                items = stack
                stack = metastack.pop()
                target = stack[-1]
                for i in range(0, len(items), 2):
                    target[items[i]] = items[i + 1]
            elif op == 0x91:  # FROZENSET
                items = stack
                stack = metastack.pop()
                stack.append(frozenset(items))
            elif op == 0x90:  # ADDITEMS
                items = stack
                stack = metastack.pop()
                for it in items:
                    stack[-1].add(it)
            elif op == 0x71:  # BINPUT
                self.memo[self._read(1)[0]] = stack[-1]
            elif op == 0x72:  # LONG_BINPUT
                self.memo[struct.unpack("<I", self._read(4))[0]] = stack[-1]
            elif op == 0x94:  # MEMOIZE
                self.memo[len(self.memo)] = stack[-1]
            elif op == 0x68:  # BINGET
                stack.append(self.memo[self._read(1)[0]])
            elif op == 0x6a:  # LONG_BINGET
                stack.append(self.memo[struct.unpack("<I", self._read(4))[0]])
            elif op == 0x67:  # GET
                stack.append(self.memo[int(self._line())])
            elif op == 0x70:  # PUT
                self.memo[int(self._line())] = stack[-1]
            elif op == 0x63:  # GLOBAL
                module = self._line().decode("utf-8")
                name = self._line().decode("utf-8")
                stack.append(self._global(module, name))
            elif op == 0x93:  # STACK_GLOBAL
                name = stack.pop()
                module = stack.pop()
                stack.append(self._global(module, name))
            elif op == 0x69:  # INST (protocol 0: class by name, args since MARK)
                module = self._line().decode("utf-8")
                name = self._line().decode("utf-8")
                cls = self._global(module, name)
                args = stack
                stack = metastack.pop()
                obj = _new_object(cls, tuple(args))
                self._made.add(id(obj))
                stack.append(obj)
            elif op == 0x6f:  # OBJ (protocol 1: class and args since MARK)
                items = stack
                stack = metastack.pop()
                obj = _new_object(items[0], tuple(items[1:]))
                self._made.add(id(obj))
                stack.append(obj)
            elif op == 0x52:  # REDUCE
                args = stack.pop()
                fn = stack.pop()
                obj = fn(*args)
                self._made.add(id(obj))
                stack.append(obj)
            elif op == 0x81:  # NEWOBJ
                args = stack.pop()
                cls = stack.pop()
                obj = _new_object(cls, args)
                self._made.add(id(obj))
                stack.append(obj)
            elif op == 0x92:  # NEWOBJ_EX
                kwargs = stack.pop()
                args = stack.pop()
                cls = stack.pop()
                obj = _new_object(cls, args, kwargs)
                self._made.add(id(obj))
                stack.append(obj)
            elif op == 0x62:  # BUILD
                state = stack.pop()
                obj = stack[-1]
                # State goes only onto instances this pickle created: a class,
                # function or singleton from find_class is shared by the whole
                # program and must not be patched by a data file.
                if isinstance(obj, type):
                    raise TypeError("'mappingproxy' object does not support item assignment")
                if id(obj) not in self._made:
                    raise UnpicklingError("BUILD is only allowed on objects created by this pickle")
                if hasattr(obj, "__setstate__"):
                    obj.__setstate__(state)
                elif isinstance(state, dict):
                    for k, v in state.items():
                        setattr(obj, k, v)
                elif isinstance(state, tuple) and len(state) == 2 and isinstance(state[0], dict):
                    for k, v in state[0].items():
                        setattr(obj, k, v)
            elif op == 0x51 or op == 0x50:  # BINPERSID / PERSID (text)
                pid = stack.pop() if op == 0x51 else self._line().decode("ascii")
                if self.persistent_load is None:
                    raise UnpicklingError("A load persistent id instruction was encountered, but no persistent_load function was specified.")
                stack.append(self.persistent_load(pid))
            elif op == 0x30:  # POP
                stack.pop()
            elif op == 0x31:  # POP_MARK
                stack = metastack.pop()
            elif op == 0x32:  # DUP
                stack.append(stack[-1])
            elif op == 0x95:  # FRAME
                self._read(8)
            elif op == 0x96:  # BYTEARRAY8
                n = struct.unpack("<Q", self._read(8))[0]
                stack.append(bytearray(self._read(n)))
            elif op in (0x82, 0x83, 0x84):  # EXT1/EXT2/EXT4
                raise UnpicklingError("unregistered extension code")
            else:
                raise UnpicklingError("unsupported pickle opcode %s at position %d" % (hex(op), self._pos - 1))


def _new_object(cls, args, kwargs=None):
    if cls is OrderedDict:
        return OrderedDict()
    if cls in (list, dict, set, tuple):
        return cls(*args)
    obj = cls.__new__(cls) if hasattr(cls, "__new__") else object.__new__(cls)
    if args or kwargs:
        try:
            obj.__init__(*args, **(kwargs or {}))
        except TypeError:
            pass
    return obj


def _decode_long(data):
    n = 0
    for i, b in enumerate(data):
        n |= b << (8 * i)
    if data and data[-1] & 0x80:
        n -= 1 << (8 * len(data))
    return n


def _encode_long(n):
    if n == 0:
        return b""
    nbytes = (n.bit_length() + 8) // 8
    return struct.pack("<q", n)[:nbytes] if -(1 << 63) <= n < (1 << 63) else _big_long(n, nbytes)


def _big_long(n, nbytes):
    if n < 0:
        n += 1 << (8 * nbytes)
    out = []
    for _ in range(nbytes):
        out.append(n & 255)
        n >>= 8
    return bytes(out)


_STRING_ESCAPES = {ord("n"): "\n", ord("t"): "\t", ord("r"): "\r", ord("\\"): "\\", ord("'"): "'", ord('"'): '"',
                   ord("a"): "\a", ord("b"): "\b", ord("f"): "\f", ord("v"): "\v", ord("0"): "\0"}


def _unescape_string(raw):
    # A protocol-0 STRING body: Python 2 `repr` escapes over latin-1 text.
    if b"\\" not in raw:
        return raw.decode("latin-1")
    out, i, n = [], 0, len(raw)
    while i < n:
        c = raw[i]
        if c == 92 and i + 1 < n:
            e = raw[i + 1]
            if e == ord("x") and i + 3 < n:
                out.append(chr(int(raw[i + 2:i + 4].decode("ascii"), 16)))
                i += 4
                continue
            if e in _STRING_ESCAPES:
                out.append(_STRING_ESCAPES[e])
                i += 2
                continue
        out.append(chr(c))
        i += 1
    return "".join(out)


def _raw_unicode_escape(raw):
    # raw-unicode-escape: latin-1 text where only \uXXXX and \UXXXXXXXX escape.
    if b"\\u" not in raw and b"\\U" not in raw:
        return raw.decode("latin-1")
    out, i, n = [], 0, len(raw)
    while i < n:
        c = raw[i]
        if c == 92 and i + 1 < n and raw[i + 1] in (117, 85):
            width = 4 if raw[i + 1] == 117 else 8
            out.append(chr(int(raw[i + 2:i + 2 + width].decode("ascii"), 16)))
            i += 2 + width
            continue
        out.append(chr(c))
        i += 1
    return "".join(out)


def loads(data, persistent_load=None, find_class=None, encoding="ASCII", errors="strict", fix_imports=True, buffers=None):
    return Unpickler(data, persistent_load=persistent_load, find_class=find_class).load()


def load(f, persistent_load=None, find_class=None, **kw):
    return loads(f.read(), persistent_load=persistent_load, find_class=find_class)


class Pickler:
    """Writes protocol 2 whatever `protocol` asks for: every reader accepts
    it, and PyTorch's weights-only loader accepts nothing newer. Bytes are
    `_codecs.encode(text, "latin1")` and bytearrays `builtins.bytearray(...)`,
    as CPython writes them at protocol 2. `reducers` maps a type (or a base
    class) to `obj -> (callable, args)`."""
    persistent_id = None

    def __init__(self, file=None, protocol=None, fix_imports=True, buffer_callback=None, persistent_id=None, reducers=None):
        if isinstance(file, int) and not isinstance(file, bool):
            file, protocol = None, file
        self._file = file
        self.protocol = 2
        if persistent_id is not None:
            self.persistent_id = persistent_id
        self.reducers = reducers or {}
        self._out = []
        self._memo = {}

    def _w(self, b):
        self._out.append(b)

    def dumps(self, obj):
        self._out = [b"\x80\x02"]
        self._save(obj)
        self._out.append(b".")
        # One join over the pieces; large payloads travel as persistent ids.
        return b"".join(self._out)

    def dump(self, obj):
        self._file.write(self.dumps(obj))

    def clear_memo(self):
        self._memo = {}

    def _memoize(self, obj):
        idx = len(self._memo)
        # Keep the object alive with its index: a memo keyed by id() must
        # not see a later object reuse a collected one's id.
        self._memo[id(obj)] = (idx, obj)
        self._w(b"r" + struct.pack("<I", idx))

    def _global(self, module, name):
        self._w(b"c" + module.encode("utf-8") + b"\n" + name.encode("utf-8") + b"\n")

    def _save_bytes(self, obj):
        self._global("_codecs", "encode")
        self._save(obj.decode("latin-1"))
        self._save("latin1")
        self._w(b"\x86R")

    def _reducer(self, obj):
        cls = type(obj)
        fn = self.reducers.get(cls)
        if fn is None:
            for base in getattr(cls, "__mro__", (cls,))[1:]:
                fn = self.reducers.get(base)
                if fn is not None:
                    break
        return fn

    def _save_reduced(self, obj, reduced):
        if isinstance(reduced, str):
            # A global: the object is module-level attribute `reduced`.
            module = getattr(obj, "__module__", None) or type(obj).__module__
            self._global(module, reduced)
            self._memoize(obj)
            return
        fn, args = reduced[0], reduced[1]
        self._global(getattr(fn, "__module__", None) or "builtins", fn.__name__)
        self._save(tuple(args))
        self._w(b"R")
        self._memoize(obj)
        state = reduced[2] if len(reduced) > 2 else None
        if state is not None:
            self._save(state)
            self._w(b"b")

    def _save(self, obj):
        if self.persistent_id is not None:
            pid = self.persistent_id(obj)
            if pid is not None:
                self._save(pid)
                self._w(b"Q")
                return
        if id(obj) in self._memo and not isinstance(obj, (int, float, bool, type(None))):
            self._w(b"j" + struct.pack("<I", self._memo[id(obj)][0]))
            return
        if obj is None:
            self._w(b"N")
        elif obj is True:
            self._w(b"\x88")
        elif obj is False:
            self._w(b"\x89")
        elif isinstance(obj, int):
            if 0 <= obj < 256:
                self._w(b"K" + bytes([obj]))
            elif -(1 << 31) <= obj < (1 << 31):
                self._w(b"J" + struct.pack("<i", obj))
            else:
                enc = _encode_long(obj)
                self._w(b"\x8a" + bytes([len(enc)]) + enc)
        elif isinstance(obj, float):
            self._w(b"G" + struct.pack(">d", obj))
        elif type(obj) is complex:
            # As CPython's copyreg reduces it: complex(real, imag).
            self._global("builtins", "complex")
            self._save(obj.real)
            self._save(obj.imag)
            self._w(b"\x86R")
            self._memoize(obj)
        elif isinstance(obj, str):
            enc = obj.encode("utf-8")
            self._w(b"X" + struct.pack("<I", len(enc)) + enc)
            self._memoize(obj)
        elif isinstance(obj, bytes):
            self._save_bytes(obj)
            self._memoize(obj)
        elif isinstance(obj, bytearray):
            self._global("builtins", "bytearray")
            self._save_bytes(bytes(obj))
            self._w(b"\x85R")
            self._memoize(obj)
        elif isinstance(obj, tuple):
            if len(obj) == 0:
                self._w(b")")
            elif len(obj) <= 3:
                for item in obj:
                    self._save(item)
                self._w({1: b"\x85", 2: b"\x86", 3: b"\x87"}[len(obj)])
            else:
                self._w(b"(")
                for item in obj:
                    self._save(item)
                self._w(b"t")
            self._memoize(obj)
        elif isinstance(obj, list):
            self._w(b"]")
            self._memoize(obj)
            if obj:
                self._w(b"(")
                for item in obj:
                    self._save(item)
                self._w(b"e")
        elif type(obj) is Counter:
            # As CPython reduces it: `Counter(dict(self))`.
            self._global("collections", "Counter")
            self._save(dict(obj))
            self._w(b"\x85R")
            self._memoize(obj)
        elif isinstance(obj, OrderedDict):
            self._w(b"ccollections\nOrderedDict\n)\x81")
            self._memoize(obj)
            if obj:
                self._w(b"(")
                for k, v in obj.items():
                    self._save(k)
                    self._save(v)
                self._w(b"u")
            # A state dict's `_metadata` (each module's version), which
            # PyTorch's loaders read, travels as the dict's BUILD state.
            metadata = getattr(obj, "_metadata", None)
            if metadata is not None:
                self._save({"_metadata": metadata})
                self._w(b"b")
        elif isinstance(obj, dict):
            self._w(b"}")
            self._memoize(obj)
            if obj:
                self._w(b"(")
                for k, v in obj.items():
                    self._save(k)
                    self._save(v)
                self._w(b"u")
        elif isinstance(obj, (set, frozenset)):
            self._global("builtins", "frozenset" if isinstance(obj, frozenset) else "set")
            self._save(list(obj))
            self._w(b"\x85R")
            self._memoize(obj)
        elif self._reducer(obj) is not None:
            self._save_reduced(obj, self._reducer(obj)(obj))
        elif isinstance(obj, type):
            self._global(obj.__module__, obj.__name__)
            self._memoize(obj)
        elif callable(getattr(obj, "state_dict", None)) and callable(getattr(obj, "parameters", None)):
            # A whole nn.Module pickles in PyTorch as a reference to its class
            # plus its attributes; loading that means importing and running
            # arbitrary code, which this pickle never does.
            raise PicklingError("cannot pickle %r object: Zipp saves tensors and plain data, not whole modules. Save "
                                "model.state_dict() and load it into a new instance with load_state_dict()" % (type(obj).__name__,))
        else:
            try:
                reduced = obj.__reduce_ex__(2) if hasattr(obj, "__reduce_ex__") else obj.__reduce__()
            except TypeError:
                reduced = None
            if isinstance(reduced, str) or (isinstance(reduced, tuple) and len(reduced) >= 2 and callable(reduced[0])):
                self._save_reduced(obj, reduced)
                return
            raise PicklingError("cannot pickle %r object" % (type(obj).__name__,))


def dumps(obj, protocol=None, *, fix_imports=True, buffer_callback=None, persistent_id=None, reducers=None):
    return Pickler(None, protocol, persistent_id=persistent_id, reducers=reducers).dumps(obj)


def dump(obj, file, protocol=None, *, fix_imports=True, buffer_callback=None, persistent_id=None, reducers=None):
    file.write(dumps(obj, protocol, persistent_id=persistent_id, reducers=reducers))
