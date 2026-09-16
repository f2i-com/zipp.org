"""A pickle subset for Zipp: protocol-2 loading (what PyTorch checkpoints and
most data files use) and dumping of plain data, with persistent ids and a
restricted `find_class` for safety. Arbitrary classes are not instantiated
unless `find_class` allows them."""
import struct
from collections import OrderedDict

HIGHEST_PROTOCOL = 2
DEFAULT_PROTOCOL = 2


class PickleError(Exception):
    pass


class PicklingError(PickleError):
    pass


class UnpicklingError(PickleError):
    pass


_MARK = object()


class Unpickler:
    def __init__(self, data, persistent_load=None, find_class=None):
        self._data = bytes(data)
        self._pos = 0
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

    def find_class(self, module, name):
        if self._find_class is not None:
            return self._find_class(module, name)
        if module == "collections" and name == "OrderedDict":
            return OrderedDict
        if module == "builtins" and name in ("set", "frozenset", "list", "dict", "tuple", "bytearray", "complex", "range", "slice"):
            return {"set": set, "frozenset": frozenset, "list": list, "dict": dict, "tuple": tuple, "bytearray": bytearray,
                    "complex": complex, "range": range, "slice": slice}[name]
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
                stack.append(float(self._line()))
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
                stack.append(self._line().decode("utf-8"))
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
                stack.append(self.find_class(module, name))
            elif op == 0x93:  # STACK_GLOBAL
                name = stack.pop()
                module = stack.pop()
                stack.append(self.find_class(module, name))
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
            elif op == 0x51:  # BINPERSID
                pid = stack.pop()
                if self.persistent_load is None:
                    raise UnpicklingError("A load persistent id instruction was encountered, but no persistent_load function was specified.")
                stack.append(self.persistent_load(pid))
            elif op == 0x50:  # PERSID (text)
                pid = self._line().decode("utf-8")
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
                stack.append(self._read(n))
            elif op == 0x62 or op == 0x60:
                raise UnpicklingError("unsupported opcode %s" % hex(op))
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


def loads(data, persistent_load=None, find_class=None, encoding="ASCII", errors="strict", fix_imports=True):
    return Unpickler(data, persistent_load=persistent_load, find_class=find_class).load()


def load(f, persistent_load=None, find_class=None, **kw):
    return loads(f.read(), persistent_load=persistent_load, find_class=find_class)


class Pickler:
    def __init__(self, protocol=2, persistent_id=None, reducers=None):
        self.protocol = 2
        self.persistent_id = persistent_id
        self.reducers = reducers or {}
        self._out = []
        self._memo = {}

    def _w(self, b):
        self._out.append(b)

    def dumps(self, obj):
        self._w(b"\x80\x02")
        self._save(obj)
        self._w(b".")
        return b"".join(self._out)

    def _memoize(self, obj):
        idx = len(self._memo)
        self._memo[id(obj)] = idx
        self._w(b"r" + struct.pack("<I", idx))

    def _save(self, obj):
        if self.persistent_id is not None:
            pid = self.persistent_id(obj)
            if pid is not None:
                self._save(pid)
                self._w(b"Q")
                return
        if id(obj) in self._memo and not isinstance(obj, (int, float, bool, type(None))):
            self._w(b"j" + struct.pack("<I", self._memo[id(obj)]))
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
        elif isinstance(obj, str):
            enc = obj.encode("utf-8")
            self._w(b"X" + struct.pack("<I", len(enc)) + enc)
            self._memoize(obj)
        elif isinstance(obj, bytes):
            self._w(b"B" + struct.pack("<I", len(obj)) + obj)
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
        elif isinstance(obj, OrderedDict):
            self._w(b"ccollections\nOrderedDict\n)\x81")
            self._memoize(obj)
            if obj:
                self._w(b"(")
                for k, v in obj.items():
                    self._save(k)
                    self._save(v)
                self._w(b"u")
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
            self._w(b"cbuiltins\n" + type(obj).__name__.encode() + b"\n")
            self._save(list(obj))
            self._w(b"\x85R")
            self._memoize(obj)
        elif type(obj) in self.reducers:
            fn, args = self.reducers[type(obj)](obj)
            self._w(b"c" + fn.__module__.encode("utf-8") + b"\n" + fn.__name__.encode("utf-8") + b"\n")
            self._save(tuple(args))
            self._w(b"R")
            self._memoize(obj)
        elif isinstance(obj, type):
            self._w(b"c" + obj.__module__.encode("utf-8") + b"\n" + obj.__name__.encode("utf-8") + b"\n")
            self._memoize(obj)
        elif hasattr(obj, "__reduce__") and not isinstance(obj, (str, bytes)):
            reduced = obj.__reduce__()
            if isinstance(reduced, tuple):
                fn, args = reduced[0], reduced[1]
                self._w(b"c" + fn.__module__.encode("utf-8") + b"\n" + fn.__name__.encode("utf-8") + b"\n")
                self._save(tuple(args))
                self._w(b"R")
                self._memoize(obj)
                return
            raise PicklingError("cannot pickle %r" % (type(obj).__name__,))
        else:
            raise PicklingError("cannot pickle %r object" % (type(obj).__name__,))


def dumps(obj, protocol=2, persistent_id=None, reducers=None, fix_imports=True):
    return Pickler(protocol, persistent_id=persistent_id, reducers=reducers).dumps(obj)


def dump(obj, f, protocol=2, **kw):
    f.write(dumps(obj, protocol, **kw))
