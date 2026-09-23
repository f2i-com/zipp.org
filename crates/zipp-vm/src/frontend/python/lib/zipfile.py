"""zipfile for Zipp: reading and writing archives with STORED entries (what
PyTorch checkpoints use); DEFLATE entries can be listed but not read, as the
runtime has no zlib. Mode "a" appends to an existing archive (its entries,
compressed or not, are kept byte for byte) and mode "x" refuses to replace
an existing file. The archive is assembled in memory and written on close."""
import os
import struct
import _zipp_tensor as _k

ZIP_STORED = 0
ZIP_DEFLATED = 8


class BadZipFile(Exception):
    pass


BadZipfile = BadZipFile


class ZipInfo:
    def __init__(self, filename="", date_time=(1980, 1, 1, 0, 0, 0)):
        self.filename = filename
        self.date_time = date_time
        self.compress_type = ZIP_STORED
        self.file_size = 0
        self.compress_size = 0
        self.header_offset = 0
        self.CRC = 0
        self.external_attr = 0

    def is_dir(self):
        return self.filename.endswith("/")

    def __repr__(self):
        return "<ZipInfo filename=%r file_size=%d>" % (self.filename, self.file_size)


def _crc32(data):
    # A table-driven CRC-32 in the runtime: a per-bit Python loop took
    # seconds per megabyte of checkpoint.
    return _k.crc32(data if isinstance(data, bytes) else bytes(data))


def _concat(pieces):
    # Pairwise concatenation: `+` copies natively, while one join over a few
    # large pieces took most of a second per megabyte-sized checkpoint.
    pieces = [p for p in pieces if p]
    if not pieces:
        return b""
    while len(pieces) > 1:
        merged = [pieces[i] + pieces[i + 1] for i in range(0, len(pieces) - 1, 2)]
        if len(pieces) % 2:
            merged.append(pieces[-1])
        pieces = merged
    return pieces[0]


# The end-of-central-directory record is 22 bytes plus a comment of at most
# 65535: only that tail needs searching, not a checkpoint's whole payload.
_EOCD_SEARCH = 22 + 65535


class ZipFile:
    def __init__(self, file, mode="r", compression=ZIP_STORED, allowZip64=True, compresslevel=None, *, strict_timestamps=True):
        if mode not in ("r", "w", "x", "a"):
            raise ValueError("ZipFile requires mode 'r', 'w', 'x', or 'a'")
        self.mode = mode
        self.filename = None
        self._entries = []
        self._data = b""
        self._closed = False
        self._file = None
        # Bytes kept ahead of the new entries (mode "a"), and the central
        # directory records of the entries already there.
        self._prefix = b""
        self._records = []
        if hasattr(file, "__fspath__"):
            file = file.__fspath__()
        if mode == "r":
            if isinstance(file, (bytes, bytearray)):
                self._data = bytes(file)
            elif hasattr(file, "read"):
                self._data = file.read()
            else:
                self.filename = str(file)
                with open(self.filename, "rb") as f:
                    self._data = f.read()
            self._read_directory()
            return
        if hasattr(file, "write"):
            self._file = file
            if mode == "a" and hasattr(file, "read") and hasattr(file, "seek"):
                file.seek(0)
                self._append_to(file.read())
                file.seek(0)
                if hasattr(file, "truncate"):
                    file.truncate()
            return
        self.filename = str(file)
        exists = os.path.exists(self.filename)
        if mode == "x" and exists:
            raise FileExistsError("[Errno 17] File exists: %r" % (self.filename,))
        if mode == "a" and exists:
            with open(self.filename, "rb") as f:
                self._append_to(f.read())

    def _append_to(self, data):
        # An existing archive keeps everything before its central directory
        # and its entries' directory records; anything else (a non-zip file)
        # is kept whole with a new archive after it, as CPython does.
        if not data:
            return
        self._data = data
        try:
            start = self._read_directory()
        except BadZipFile:
            self._prefix = data
            self._entries = []
            self._data = b""
            return
        self._prefix = data[:start]

    def _read_directory(self):
        data = self._data
        tail = max(0, len(data) - _EOCD_SEARCH)
        end = data[tail:].rfind(b"PK\x05\x06")
        if end < 0:
            raise BadZipFile("File is not a zip file")
        end += tail
        count = struct.unpack("<H", data[end + 10:end + 12])[0]
        size = struct.unpack("<I", data[end + 12:end + 16])[0]
        offset = struct.unpack("<I", data[end + 16:end + 20])[0]
        if (offset == 0xFFFFFFFF or count == 0xFFFF) and end >= 20 and data[end - 20:end - 16] == b"PK\x06\x07":
            zip64 = struct.unpack("<Q", data[end - 12:end - 4])[0]
            count = struct.unpack("<Q", data[zip64 + 32:zip64 + 40])[0]
            offset = struct.unpack("<Q", data[zip64 + 48:zip64 + 56])[0]
        pos = offset
        for _ in range(count):
            if data[pos:pos + 4] != b"PK\x01\x02":
                raise BadZipFile("Bad magic number for central directory")
            (compress, crc, csize, usize, nlen, elen, clen, hoff) = (
                struct.unpack("<H", data[pos + 10:pos + 12])[0],
                struct.unpack("<I", data[pos + 16:pos + 20])[0],
                struct.unpack("<I", data[pos + 20:pos + 24])[0],
                struct.unpack("<I", data[pos + 24:pos + 28])[0],
                struct.unpack("<H", data[pos + 28:pos + 30])[0],
                struct.unpack("<H", data[pos + 30:pos + 32])[0],
                struct.unpack("<H", data[pos + 32:pos + 34])[0],
                struct.unpack("<I", data[pos + 42:pos + 46])[0])
            name = data[pos + 46:pos + 46 + nlen].decode("utf-8")
            extra = data[pos + 46 + nlen:pos + 46 + nlen + elen]
            if csize == 0xFFFFFFFF or usize == 0xFFFFFFFF or hoff == 0xFFFFFFFF:
                usize, csize, hoff = _zip64(extra, usize, csize, hoff)
            info = ZipInfo(name)
            info.compress_type = compress
            info.CRC = crc
            info.compress_size = csize
            info.file_size = usize
            info.header_offset = hoff
            info._central = data[pos:pos + 46 + nlen + elen + clen]
            self._entries.append(info)
            pos += 46 + nlen + elen + clen
        return offset

    def namelist(self):
        return [e.filename for e in self._entries]

    def infolist(self):
        return list(self._entries)

    def getinfo(self, name):
        for e in self._entries:
            if e.filename == name:
                return e
        raise KeyError("There is no item named %r in the archive" % name)

    def read(self, name):
        info = self.getinfo(name) if not isinstance(name, ZipInfo) else name
        data = self._data
        pos = info.header_offset
        if data[pos:pos + 4] != b"PK\x03\x04":
            raise BadZipFile("Bad magic number for file header")
        nlen = struct.unpack("<H", data[pos + 26:pos + 28])[0]
        elen = struct.unpack("<H", data[pos + 28:pos + 30])[0]
        start = pos + 30 + nlen + elen
        if info.compress_type == ZIP_STORED:
            return data[start:start + info.file_size]
        raise NotImplementedError("compression method %d is not supported (the Python sandbox has no zlib); store entries uncompressed" % info.compress_type)

    def open(self, name, mode="r"):
        from io import BytesIO
        return BytesIO(self.read(name))

    def extract(self, member, path=None):
        raise NotImplementedError("extract() is not available; use read()")

    def writestr(self, name, data, compress_type=None, compresslevel=None):
        if self.mode == "r":
            raise ValueError("write() requires mode 'w', 'x', or 'a'")
        if self._closed:
            raise ValueError("Attempt to write to ZIP archive that was already closed")
        if isinstance(name, ZipInfo):
            name = name.filename
        if isinstance(data, str):
            data = data.encode("utf-8")
        self._records.append((name, data if isinstance(data, bytes) else bytes(data)))
        info = ZipInfo(name)
        info.file_size = info.compress_size = len(data)
        self._entries.append(info)

    def write(self, filename, arcname=None):
        with open(filename, "rb") as f:
            self.writestr(arcname or filename, f.read())

    def close(self):
        if self._closed:
            return
        self._closed = True
        if self.mode == "r":
            return
        out = [self._prefix]
        central = [info._central for info in self._entries if getattr(info, "_central", None) is not None]
        offset = len(self._prefix)
        for name, data in self._records:
            enc = name.encode("utf-8")
            crc = _crc32(data)
            header = b"PK\x03\x04" + struct.pack("<HHHHHIIIHH", 20, 0, 0, 0, 0x21, crc, len(data), len(data), len(enc), 0) + enc
            out.append(header)
            out.append(data)
            central.append(b"PK\x01\x02" + struct.pack("<HHHHHHIIIHHHHHII", 20, 20, 0, 0, 0, 0x21, crc, len(data), len(data), len(enc), 0, 0, 0, 0, 0, offset) + enc)
            offset += len(header) + len(data)
        cd = b"".join(central)
        end = b"PK\x05\x06" + struct.pack("<HHHHIIH", 0, 0, len(central), len(central), len(cd), offset, 0)
        out.append(cd)
        out.append(end)
        blob = _concat(out)
        if self._file is not None:
            self._file.write(blob)
        else:
            with open(self.filename, "wb") as f:
                f.write(blob)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def testzip(self):
        return None


def _zip64(extra, usize, csize, hoff):
    pos = 0
    while pos + 4 <= len(extra):
        tag = struct.unpack("<H", extra[pos:pos + 2])[0]
        size = struct.unpack("<H", extra[pos + 2:pos + 4])[0]
        if tag == 1:
            fields = []
            p = pos + 4
            while p + 8 <= pos + 4 + size:
                fields.append(struct.unpack("<Q", extra[p:p + 8])[0])
                p += 8
            values = list(fields)
            if usize == 0xFFFFFFFF and values:
                usize = values.pop(0)
            if csize == 0xFFFFFFFF and values:
                csize = values.pop(0)
            if hoff == 0xFFFFFFFF and values:
                hoff = values.pop(0)
            return usize, csize, hoff
        pos += 4 + size
    return usize, csize, hoff


def is_zipfile(file):
    try:
        ZipFile(file)
        return True
    except Exception:
        return False
