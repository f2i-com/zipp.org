// node:fs — the synchronous file API over the host's file operations, its
// callback and promise forms over that, and read and write streams.
//
// ReadStream and WriteStream are derived from Node.js
// (lib/internal/fs/streams.js), which carries this notice:
//
// Copyright Node.js contributors. All rights reserved.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to permit
// persons to whom the Software is furnished to do so, subject to the
// following conditions:
//
// The above copyright notice and this permission notice shall be included
// in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
// NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
// DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
// OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
// USE OR OTHER DEALINGS IN THE SOFTWARE.
(function (host, builtin) {
  "use strict";
  const { Buffer, _toBuffer } = builtin("buffer");
  const pathMod = builtin("path");
  function fsError(e, syscall, path) {
    const m = /^(?:Error: )?([A-Z]+): (.*)$/.exec(e && e.message ? e.message : String(e));
    if (!m) return e;
    const err = new Error(m[1] + ": " + m[2]);
    err.errno = { ENOENT: -2, EACCES: -13, EEXIST: -17, ENOTDIR: -20, EISDIR: -21, ENOTEMPTY: -39, EBADF: -9 }[m[1]] || -5;
    err.code = m[1];
    err.syscall = syscall;
    if (path !== undefined) err.path = path;
    return err;
  }
  function toPath(p) {
    if (typeof p === "string") return p;
    if (p && typeof p === "object" && typeof p.href === "string") return builtin("url").fileURLToPath(p.href);
    if (ArrayBuffer.isView(p)) return new TextDecoder().decode(p);
    throw new TypeError('The "path" argument must be of type string or an instance of Buffer or URL. Received ' + (p === null ? "null" : typeof p));
  }
  function op(name, syscall, path, a, b) {
    try {
      return host.fs(name, path, a, b);
    } catch (e) {
      throw fsError(e, syscall, path);
    }
  }
  function encodingOf(options) {
    if (typeof options === "string") return options;
    return options && options.encoding ? options.encoding : null;
  }
  class Stats {
    constructor(raw) {
      this._kind = raw.kind;
      this.size = raw.size;
      this.mtimeMs = raw.mtimeMs;
      this.atimeMs = raw.atimeMs;
      this.ctimeMs = raw.mtimeMs;
      this.birthtimeMs = raw.birthtimeMs;
      this.mtime = new Date(raw.mtimeMs);
      this.atime = new Date(raw.atimeMs);
      this.ctime = new Date(raw.mtimeMs);
      this.birthtime = new Date(raw.birthtimeMs);
      this.mode = raw.kind === "dir" ? 0o40755 : raw.readonly ? 0o100444 : 0o100644;
      this.uid = 0;
      this.gid = 0;
      this.nlink = 1;
      this.dev = 0;
      this.ino = 0;
      this.rdev = 0;
      this.blksize = 4096;
      this.blocks = Math.ceil(raw.size / 512);
    }
    isFile() { return this._kind === "file"; }
    isDirectory() { return this._kind === "dir"; }
    isSymbolicLink() { return this._kind === "link"; }
    isBlockDevice() { return false; }
    isCharacterDevice() { return false; }
    isFIFO() { return false; }
    isSocket() { return false; }
  }
  class Dirent {
    constructor(name, parentPath, kind) {
      this.name = name;
      this.parentPath = parentPath;
      this.path = parentPath;
      this._kind = kind;
    }
    isFile() { return this._kind === "file"; }
    isDirectory() { return this._kind === "dir"; }
    isSymbolicLink() { return this._kind === "link"; }
    isBlockDevice() { return false; }
    isCharacterDevice() { return false; }
    isFIFO() { return false; }
    isSocket() { return false; }
  }

  const fs = {
    constants: { F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1, COPYFILE_EXCL: 1, O_RDONLY: 0, O_WRONLY: 1, O_RDWR: 2, O_CREAT: 64, O_EXCL: 128, O_TRUNC: 512, O_APPEND: 1024 },
    readFileSync(path, options) {
      const enc = encodingOf(options);
      // Standard input, all of it: `readFileSync(0)`, as scripts read their
      // input (and "/dev/stdin", which Windows has no file for).
      if (path === 0 || path === "/dev/stdin") {
        const text = host.readStdin(-1);
        return enc === "utf8" || enc === "utf-8" ? text : enc ? Buffer.from(text).toString(enc) : Buffer.from(text);
      }
      path = toPath(path);
      let data;
      try {
        data = host.readFile(path, false);
      } catch (e) {
        throw fsError(e, "open", path);
      }
      const buf = _toBuffer(data);
      return enc ? buf.toString(enc) : buf;
    },
    writeFileSync(path, data, options) {
      path = toPath(path);
      const enc = encodingOf(options);
      const flag = options && typeof options === "object" && options.flag;
      if (typeof data === "string" && enc && enc !== "utf8" && enc !== "utf-8") data = Buffer.from(data, enc);
      op(flag === "a" ? "append" : "write", "open", path, ArrayBuffer.isView(data) ? data : String(data));
    },
    appendFileSync(path, data, options) {
      path = toPath(path);
      const enc = encodingOf(options);
      if (typeof data === "string" && enc && enc !== "utf8" && enc !== "utf-8") data = Buffer.from(data, enc);
      op("append", "open", path, ArrayBuffer.isView(data) ? data : String(data));
    },
    existsSync(path) {
      try {
        return host.fs("stat", toPath(path), true) !== undefined;
      } catch {
        return false;
      }
    },
    statSync(path, options) {
      path = toPath(path);
      const raw = op("stat", "stat", path, options && options.throwIfNoEntry === false);
      return raw === undefined ? undefined : new Stats(raw);
    },
    lstatSync(path, options) {
      path = toPath(path);
      const raw = op("lstat", "lstat", path, options && options.throwIfNoEntry === false);
      return raw === undefined ? undefined : new Stats(raw);
    },
    readdirSync(path, options) {
      path = toPath(path);
      const names = op("readdir", "scandir", path);
      if (options && typeof options === "object" && options.withFileTypes) {
        return names.map((n) => {
          const raw = host.fs("lstat", pathMod.join(path, n), true);
          return new Dirent(n, path, raw ? raw.kind : "file");
        });
      }
      return names;
    },
    mkdirSync(path, options) {
      path = toPath(path);
      const recursive = typeof options === "object" && options !== null && !!options.recursive;
      if (recursive && fs.existsSync(path)) return undefined;
      op("mkdir", "mkdir", path, recursive);
      return recursive ? path : undefined;
    },
    rmSync(path, options) {
      path = toPath(path);
      op("rm", "rm", path, !!(options && options.recursive), !!(options && options.force));
    },
    rmdirSync(path, options) {
      path = toPath(path);
      op("rm", "rmdir", path, !!(options && options.recursive), false);
    },
    unlinkSync(path) {
      path = toPath(path);
      op("rm", "unlink", path, false, false);
    },
    renameSync(from, to) {
      op("rename", "rename", toPath(from), toPath(to));
    },
    copyFileSync(from, to) {
      op("copy", "copyfile", toPath(from), toPath(to));
    },
    realpathSync(path) {
      return op("realpath", "realpath", toPath(path));
    },
    accessSync(path) {
      path = toPath(path);
      op("stat", "access", path, false);
    },
    mkdtempSync(prefix) {
      const p = prefix + Math.random().toString(36).slice(2, 8);
      fs.mkdirSync(p);
      return p;
    },
  };
  fs.realpathSync.native = fs.realpathSync;
  fs.Stats = Stats;
  fs.Dirent = Dirent;
  fs.F_OK = 0;
  fs.R_OK = 4;
  fs.W_OK = 2;
  fs.X_OK = 1;

  // Callback forms: the work done now, the callback called after.
  const asyncNames = ["readFile", "writeFile", "appendFile", "stat", "lstat", "readdir", "mkdir", "rm", "rmdir", "unlink", "rename", "copyFile", "realpath", "access", "mkdtemp"];
  for (const name of asyncNames) {
    const sync = fs[name + "Sync"];
    fs[name] = function (...args) {
      const cb = typeof args[args.length - 1] === "function" ? args.pop() : undefined;
      let result, error = null;
      try {
        result = sync(...args);
      } catch (e) {
        error = e;
      }
      if (cb) process.nextTick(() => (error ? cb(error) : cb(null, result)));
    };
  }
  fs.exists = (path, cb) => process.nextTick(() => cb(fs.existsSync(path)));
  fs.promises = {};
  for (const name of asyncNames) {
    const sync = fs[name + "Sync"];
    fs.promises[name] = (...args) => {
      try {
        return Promise.resolve(sync(...args));
      } catch (e) {
        return Promise.reject(e);
      }
    };
  }
  fs.promises.constants = fs.constants;

  // ---- streams ----------------------------------------------------------------
  // The descriptors streams open, the layer's own: opening checks and
  // creates or truncates the file as the flags say, a reader takes the file
  // whole and reads slice it, a write appends (or rewrites the file, for one
  // at an offset). Each completes on a later turn of the event loop, as
  // Node's I/O does. 0, 1 and 2 are standard input, output and error.
  const { O_WRONLY, O_RDWR, O_CREAT, O_EXCL, O_TRUNC, O_APPEND } = fs.constants;
  const files = new Map();
  let lastFd = 2;
  const later = (fn, ...args) => host.part("timers").setImmediate(fn, ...args);
  const ioError = (code, what, syscall, path) =>
    fsError(new Error(code + ": " + what + ", " + syscall + (path === undefined ? "" : " '" + path + "'")), syscall, path);
  const stdio = [null, null, null];
  function readWhole(path, syscall) {
    try {
      return _toBuffer(host.readFile(path, false));
    } catch (e) {
      throw fsError(e, syscall, path);
    }
  }
  function flagsOf(flags) {
    if (typeof flags === "number") return flags;
    switch (flags) {
      case "r": case "rs": case "sr": return 0;
      case "r+": case "rs+": case "sr+": return O_RDWR;
      case "w": return O_TRUNC | O_CREAT | O_WRONLY;
      case "wx": case "xw": return O_TRUNC | O_CREAT | O_WRONLY | O_EXCL;
      case "w+": return O_TRUNC | O_CREAT | O_RDWR;
      case "wx+": case "xw+": return O_TRUNC | O_CREAT | O_RDWR | O_EXCL;
      case "a": case "as": case "sa": return O_APPEND | O_CREAT | O_WRONLY;
      case "ax": case "xa": return O_APPEND | O_CREAT | O_WRONLY | O_EXCL;
      case "a+": case "as+": case "sa+": return O_APPEND | O_CREAT | O_RDWR;
      case "ax+": case "xa+": return O_APPEND | O_CREAT | O_RDWR | O_EXCL;
    }
    throw errInvalidArgValue("flags", flags);
  }
  function openFile(path, flags) {
    const st = host.fs("stat", path, true);
    const writable = (flags & (O_WRONLY | O_RDWR)) !== 0;
    const file = { path, data: null, pos: 0, size: st ? st.size : 0, append: (flags & O_APPEND) !== 0, dir: false };
    if (st && flags & O_CREAT && flags & O_EXCL) throw ioError("EEXIST", "file already exists", "open", path);
    if (st && st.kind === "dir") {
      if (writable) throw ioError("EISDIR", "illegal operation on a directory", "open", path);
      file.dir = true;
      return file;
    }
    if (flags & O_CREAT && (!st || flags & O_TRUNC)) {
      op(flags & O_TRUNC ? "write" : "append", "open", path, "");
      file.size = 0;
    }
    if (!writable || flags & O_RDWR) {
      file.data = readWhole(path, "open");
      file.size = file.data.length;
    }
    return file;
  }
  function writeAt(file, chunk, position) {
    const at = position === null ? file.pos : position;
    if (file.append || at === file.size) {
      op("append", "write", file.path, chunk);
    } else {
      const old = readWhole(file.path, "write");
      const out = new Uint8Array(Math.max(old.length, at + chunk.length));
      out.set(old);
      out.set(chunk, at);
      op("write", "write", file.path, out);
    }
    file.size = file.append ? file.size + chunk.length : Math.max(file.size, at + chunk.length);
    if (position === null) file.pos = at + chunk.length;
  }
  function positionOf(p) {
    if (typeof p === "bigint") p = Number(p);
    return typeof p === "number" && p >= 0 ? p : null;
  }
  const fileOps = {
    open(path, flags, mode, cb) {
      const f = flagsOf(flags);
      let file;
      try {
        file = openFile(toPath(path), f);
      } catch (e) {
        later(cb, e);
        return;
      }
      files.set(++lastFd, file);
      later(cb, null, lastFd);
    },
    read(fd, buffer, offset, length, position, cb) {
      const file = files.get(fd);
      let n = 0;
      if (fd === 0) {
        const chunk = host.readStdin(Math.max(length, 1), true);
        if (chunk !== null) {
          n = Math.min(chunk.length, length);
          buffer.set(chunk.subarray(0, n), offset);
        }
      } else if (!file) {
        return later(cb, ioError("EBADF", "bad file descriptor", "read"), 0, buffer);
      } else if (file.dir) {
        return later(cb, ioError("EISDIR", "illegal operation on a directory", "read"), 0, buffer);
      } else if (file.data === null) {
        return later(cb, ioError("EBADF", "bad file descriptor", "read"), 0, buffer);
      } else {
        const p = positionOf(position);
        const at = p === null ? file.pos : p;
        n = Math.max(0, Math.min(length, file.data.length - at));
        buffer.set(file.data.subarray(at, at + n), offset);
        if (p === null) file.pos = at + n;
      }
      later(cb, null, n, buffer);
    },
    write(fd, buffer, offset, length, position, cb) {
      const file = files.get(fd);
      const chunk = buffer.subarray(offset, offset + length);
      if (fd === 1 || fd === 2) {
        host.write(fd, (stdio[fd] ||= new TextDecoder()).decode(chunk, { stream: true }));
      } else if (!file) {
        return later(cb, ioError("EBADF", "bad file descriptor", "write"), 0, buffer);
      } else {
        try {
          writeAt(file, chunk, positionOf(position));
        } catch (e) {
          return later(cb, e, 0, buffer);
        }
      }
      later(cb, null, length, buffer);
    },
    writev(fd, buffers, position, cb) {
      const all = Buffer.concat(buffers.filter((b) => b.length > 0));
      fileOps.write(fd, all, 0, all.length, position, (er, n) => cb(er, n, buffers));
    },
    fsync(fd, cb) {
      later(cb, files.has(fd) || fd <= 2 ? null : ioError("EBADF", "bad file descriptor", "fsync"));
    },
    close(fd, cb) {
      later(cb, files.delete(fd) || fd <= 2 ? null : ioError("EBADF", "bad file descriptor", "close"));
    },
  };

  // ReadStream and WriteStream, Node's (lib/internal/fs/streams.js) over those
  // descriptors. They join node:stream's classes when the first is made, so
  // a program that never makes one never loads stream.
  function nodeError(Base, code, message) {
    const err = new Base(message);
    Object.defineProperty(err, "toString", {
      value() { return this.name + " [" + code + "]: " + this.message; },
      writable: true,
      configurable: true,
    });
    err.code = code;
    return err;
  }
  function received(v) {
    if (v == null) return " Received " + v;
    if (typeof v === "function") return " Received function " + (v.name || "<anonymous>");
    if (typeof v === "object") {
      const name = v.constructor && v.constructor.name;
      return name ? " Received an instance of " + name : " Received " + String(v);
    }
    let shown = String(v);
    if (shown.length > 28) shown = shown.slice(0, 25) + "...";
    if (typeof v === "string") shown = "'" + shown + "'";
    return " Received type " + typeof v + " (" + shown + ")";
  }
  function quoted(v) {
    if (typeof v !== "string") return String(v);
    return "'" + v.replace(/[\\'\x00-\x1f\x7f]/g, (c) => (c === "\\" || c === "'" ? "\\" + c : "\\x" + c.charCodeAt(0).toString(16).padStart(2, "0").toUpperCase())) + "'";
  }
  const errInvalidArgType = (name, expected, actual) =>
    nodeError(TypeError, "ERR_INVALID_ARG_TYPE", 'The "' + name + '" ' + (name.includes(".") ? "property" : "argument") +
      " must be " + expected + "." + received(actual));
  const errInvalidArgValue = (name, v, reason = "is invalid") =>
    nodeError(TypeError, "ERR_INVALID_ARG_VALUE", "The " + (name.includes(".") ? "property" : "argument") + " '" + name +
      "' " + reason + ". Received " + quoted(v));
  function errOutOfRange(name, range, v) {
    let shown = String(v);
    if (Number.isInteger(v) && Math.abs(v) > 2 ** 32) shown = shown.replace(/(\d)(?=(\d{3})+$)/g, "$1_");
    return nodeError(RangeError, "ERR_OUT_OF_RANGE", 'The value of "' + name + '" is out of range. It must be ' + range + ". Received " + shown);
  }
  function validateInteger(v, name, min = 0, max = Number.MAX_SAFE_INTEGER) {
    if (typeof v !== "number") throw errInvalidArgType(name, "of type number", v);
    if (!Number.isInteger(v)) throw errOutOfRange(name, "an integer", v);
    if (v < min || v > max) throw errOutOfRange(name, ">= " + min + " && <= " + max, v);
  }
  function validateFunction(v, name) {
    if (typeof v !== "function") throw errInvalidArgType(name, "of type function", v);
  }
  function validatePath(path) {
    if (typeof path !== "string" && !(path instanceof Uint8Array)) {
      throw errInvalidArgType("path", "of type string or an instance of Buffer or URL", path);
    }
    if (path.includes(typeof path === "string" ? "\u0000" : 0)) {
      throw errInvalidArgValue("path", path, "must be a string, Uint8Array, or URL without null bytes");
    }
  }
  function toPathIfFileURL(p) {
    if (p != null && typeof p === "object" && typeof p.href === "string" && p.protocol !== undefined && p.auth === undefined && p.path === undefined) {
      return builtin("url").fileURLToPath(p.href);
    }
    return p;
  }
  function getOptions(options) {
    if (options == null || typeof options === "function") return {};
    if (typeof options === "string") options = { encoding: options };
    else if (typeof options !== "object") throw errInvalidArgType("options", "one of type string or object", options);
    if (options.encoding !== "buffer" && options.encoding && !Buffer.isEncoding(options.encoding)) {
      throw errInvalidArgValue("encoding", options.encoding, "is invalid encoding");
    }
    const signal = options.signal;
    if (signal !== undefined && (signal === null || typeof signal !== "object" || !("aborted" in signal))) {
      throw errInvalidArgType("options.signal", "an instance of AbortSignal", signal);
    }
    const copy = {};
    for (const key in options) copy[key] = options[key];
    return copy;
  }
  function importFd(stream, options) {
    if (typeof options.fd !== "number") {
      throw errInvalidArgType("options.fd", "of type number or an instance of FileHandle", options.fd);
    }
    stream[kFs] = options.fs || fileOps;
    const fd = Object.is(options.fd, -0) ? 0 : options.fd;
    validateInteger(fd, "fd", 0, 2147483647);
    return fd;
  }
  function errorOrDestroy(stream, err) {
    const r = stream._readableState, w = stream._writableState;
    if ((w && w.destroyed) || (r && r.destroyed)) return;
    if ((r && r.autoDestroy) || (w && w.autoDestroy)) {
      stream.destroy(err);
    } else if (err) {
      if (w && !w.errored) w.errored = err;
      if (r && !r.errored) r.errored = err;
      if ((w && w.errorEmitted) || (r && r.errorEmitted)) return;
      if (w) w.errorEmitted = true;
      if (r) r.errorEmitted = true;
      stream.emit("error", err);
    }
  }

  const kFs = Symbol("kFs");
  const kIoDone = Symbol("kIoDone");
  const kIsPerformingIO = Symbol("kIsPerformingIO");
  let Readable, Writable, finished;
  function joinStream() {
    if (Readable) return;
    ({ Readable, Writable, finished } = builtin("stream"));
    Object.setPrototypeOf(ReadStream.prototype, Readable.prototype);
    Object.setPrototypeOf(ReadStream, Readable);
    Object.setPrototypeOf(WriteStream.prototype, Writable.prototype);
    Object.setPrototypeOf(WriteStream, Writable);
    WriteStream.prototype.destroySoon = Writable.prototype.end;
  }
  function deprecatedOpen(name) {
    let warned = false;
    return function open() {
      if (warned || process.noDeprecation) return;
      warned = true;
      process.emitWarning(name + ".prototype.open() is deprecated", "DeprecationWarning", "DEP0135");
    };
  }
  const openReadFs = deprecatedOpen("ReadStream");
  const openWriteFs = deprecatedOpen("WriteStream");
  function construct(callback) {
    const stream = this;
    if (typeof stream.fd === "number") {
      callback();
      return;
    }
    if (stream.open !== openWriteFs && stream.open !== openReadFs) {
      // An `open` of the program's own (as Node keeps for monkey patching):
      // its 'open' or 'error' ends the construction.
      const orgEmit = stream.emit;
      stream.emit = function (...args) {
        if (args[0] === "open") {
          this.emit = orgEmit;
          callback();
          Reflect.apply(orgEmit, this, args);
        } else if (args[0] === "error") {
          this.emit = orgEmit;
          callback(args[1]);
        } else {
          Reflect.apply(orgEmit, this, args);
        }
      };
      stream.open();
    } else {
      stream[kFs].open(stream.path, stream.flags, stream.mode, (er, fd) => {
        if (er) {
          callback(er);
        } else {
          stream.fd = fd;
          callback();
          stream.emit("open", stream.fd);
          stream.emit("ready");
        }
      });
    }
  }
  function close(stream, err, cb) {
    if (!stream.fd) {
      cb(err);
    } else if (stream.flush) {
      stream[kFs].fsync(stream.fd, (flushErr) => closeFd(stream, err || flushErr, cb));
    } else {
      closeFd(stream, err, cb);
    }
  }
  function closeFd(stream, err, cb) {
    stream[kFs].close(stream.fd, (er) => cb(er || err));
    stream.fd = null;
  }
  // Destroy waits for a read or write in flight before closing.
  function destroyStream(err, cb) {
    if (this[kIsPerformingIO]) this.once(kIoDone, (er) => close(this, err || er, cb));
    else close(this, err, cb);
  }
  function autoCloseAccessor(name, state) {
    return {
      get() {
        if (!(kFs in this)) throw nodeError(TypeError, "ERR_INVALID_THIS", 'Value of "this" must be of type ' + name);
        return this[state].autoDestroy;
      },
      set(val) {
        if (!(kFs in this)) throw nodeError(TypeError, "ERR_INVALID_THIS", 'Value of "this" must be of type ' + name);
        this[state].autoDestroy = val;
      },
    };
  }
  const pendingAccessor = { get() { return this.fd === null; }, configurable: true };

  function ReadStream(path, options) {
    if (!(this instanceof ReadStream)) return new ReadStream(path, options);
    joinStream();
    options = getOptions(options);
    if (options.highWaterMark === undefined) options.highWaterMark = 64 * 1024;
    if (options.autoDestroy === undefined) options.autoDestroy = false;
    if (options.fd == null) {
      this.fd = null;
      this[kFs] = options.fs || fileOps;
      validateFunction(this[kFs].open, "options.fs.open");
      this.path = toPathIfFileURL(path);
      this.flags = options.flags === undefined ? "r" : options.flags;
      this.mode = options.mode === undefined ? 0o666 : options.mode;
      validatePath(this.path);
    } else {
      this.fd = importFd(this, options);
    }
    options.autoDestroy = options.autoClose === undefined ? true : options.autoClose;
    validateFunction(this[kFs].read, "options.fs.read");
    if (options.autoDestroy) validateFunction(this[kFs].close, "options.fs.close");
    this.start = options.start;
    this.end = options.end;
    this.pos = undefined;
    this.bytesRead = 0;
    this[kIsPerformingIO] = false;
    if (this.start !== undefined) {
      validateInteger(this.start, "start", 0);
      this.pos = this.start;
    }
    if (this.end === undefined) {
      this.end = Infinity;
    } else if (this.end !== Infinity) {
      validateInteger(this.end, "end", 0);
      if (this.start !== undefined && this.start > this.end) {
        throw errOutOfRange("start", '<= "end" (here: ' + this.end + ")", this.start);
      }
    }
    Readable.call(this, options);
  }
  Object.defineProperty(ReadStream.prototype, "autoClose", autoCloseAccessor("ReadStream", "_readableState"));
  ReadStream.prototype.open = openReadFs;
  ReadStream.prototype._construct = construct;
  ReadStream.prototype._read = function (n) {
    n = this.pos !== undefined ? Math.min(this.end - this.pos + 1, n) : Math.min(this.end - this.bytesRead + 1, n);
    if (n <= 0) {
      this.push(null);
      return;
    }
    const buf = Buffer.allocUnsafeSlow(n);
    this[kIsPerformingIO] = true;
    this[kFs].read(this.fd, buf, 0, n, this.pos, (er, bytesRead, buf) => {
      this[kIsPerformingIO] = false;
      if (this.destroyed) {
        this.emit(kIoDone, er);
        return;
      }
      if (er) {
        errorOrDestroy(this, er);
      } else if (bytesRead > 0) {
        if (this.pos !== undefined) this.pos += bytesRead;
        this.bytesRead += bytesRead;
        if (bytesRead !== buf.length) {
          const dst = Buffer.allocUnsafeSlow(bytesRead);
          buf.copy(dst, 0, 0, bytesRead);
          buf = dst;
        }
        this.push(buf);
      } else {
        this.push(null);
      }
    });
  };
  ReadStream.prototype._destroy = destroyStream;
  ReadStream.prototype.close = function (cb) {
    if (typeof cb === "function") finished(this, cb);
    this.destroy();
  };
  Object.defineProperty(ReadStream.prototype, "pending", pendingAccessor);

  function WriteStream(path, options) {
    if (!(this instanceof WriteStream)) return new WriteStream(path, options);
    joinStream();
    options = getOptions(options);
    options.decodeStrings = true;
    if (options.fd == null) {
      this.fd = null;
      this[kFs] = options.fs || fileOps;
      validateFunction(this[kFs].open, "options.fs.open");
      this.path = toPathIfFileURL(path);
      this.flags = options.flags === undefined ? "w" : options.flags;
      this.mode = options.mode === undefined ? 0o666 : options.mode;
      validatePath(this.path);
    } else {
      this.fd = importFd(this, options);
    }
    options.autoDestroy = options.autoClose === undefined ? true : options.autoClose;
    if (!this[kFs].write && !this[kFs].writev) throw errInvalidArgType("options.fs.write", "of type function", this[kFs].write);
    if (this[kFs].write) validateFunction(this[kFs].write, "options.fs.write");
    if (this[kFs].writev) validateFunction(this[kFs].writev, "options.fs.writev");
    if (options.autoDestroy) validateFunction(this[kFs].close, "options.fs.close");
    this.flush = options.flush;
    if (this.flush == null) {
      this.flush = false;
    } else {
      if (typeof this.flush !== "boolean") throw errInvalidArgType("options.flush", "of type boolean", this.flush);
      validateFunction(this[kFs].fsync, "options.fs.fsync");
    }
    if (!this[kFs].write) this._write = null;
    if (!this[kFs].writev) this._writev = null;
    this.start = options.start;
    this.pos = undefined;
    this.bytesWritten = 0;
    this[kIsPerformingIO] = false;
    if (this.start !== undefined) {
      validateInteger(this.start, "start", 0);
      this.pos = this.start;
    }
    Writable.call(this, options);
    if (options.encoding) this.setDefaultEncoding(options.encoding);
  }
  Object.defineProperty(WriteStream.prototype, "autoClose", autoCloseAccessor("WriteStream", "_writableState"));
  WriteStream.prototype.open = openWriteFs;
  WriteStream.prototype._construct = construct;
  // A write is retried for what it left unwritten, up to five times in a row
  // for none of it.
  function writeAll(data, size, pos, cb, retries = 0) {
    this[kFs].write(this.fd, data, 0, size, pos, (er, bytesWritten, buffer) => {
      if (er?.code === "EAGAIN") {
        er = null;
        bytesWritten = 0;
      }
      if (this.destroyed || er) return cb(er || nodeError(Error, "ERR_STREAM_DESTROYED", "Cannot call write after a stream was destroyed"));
      this.bytesWritten += bytesWritten;
      retries = bytesWritten ? 0 : retries + 1;
      size -= bytesWritten;
      pos += bytesWritten;
      if (retries > 5) cb(nodeError(Error, "ERR_SYSTEM_ERROR", "A system error occurred: write failed"));
      else if (size) writeAll.call(this, buffer.slice(bytesWritten), size, pos, cb, retries);
      else cb();
    });
  }
  function writevAll(chunks, size, pos, cb, retries = 0) {
    this[kFs].writev(this.fd, chunks, this.pos, (er, bytesWritten, buffers) => {
      if (er?.code === "EAGAIN") {
        er = null;
        bytesWritten = 0;
      }
      if (this.destroyed || er) return cb(er || nodeError(Error, "ERR_STREAM_DESTROYED", "Cannot call writev after a stream was destroyed"));
      this.bytesWritten += bytesWritten;
      retries = bytesWritten ? 0 : retries + 1;
      size -= bytesWritten;
      pos += bytesWritten;
      if (retries > 5) cb(nodeError(Error, "ERR_SYSTEM_ERROR", "A system error occurred: writev failed"));
      else if (size) writevAll.call(this, [Buffer.concat(buffers).slice(bytesWritten)], size, pos, cb, retries);
      else cb();
    });
  }
  function afterWrite(stream, cb) {
    return (er) => {
      stream[kIsPerformingIO] = false;
      if (stream.destroyed) {
        cb(er);
        return stream.emit(kIoDone, er);
      }
      cb(er);
    };
  }
  WriteStream.prototype._write = function (data, encoding, cb) {
    this[kIsPerformingIO] = true;
    writeAll.call(this, data, data.length, this.pos, afterWrite(this, cb));
    if (this.pos !== undefined) this.pos += data.length;
  };
  WriteStream.prototype._writev = function (data, cb) {
    const chunks = new Array(data.length);
    let size = 0;
    for (let i = 0; i < data.length; i++) {
      chunks[i] = data[i].chunk;
      size += chunks[i].length;
    }
    this[kIsPerformingIO] = true;
    writevAll.call(this, chunks, size, this.pos, afterWrite(this, cb));
    if (this.pos !== undefined) this.pos += size;
  };
  WriteStream.prototype._destroy = destroyStream;
  WriteStream.prototype.close = function (cb) {
    if (cb) {
      if (this.closed) {
        process.nextTick(cb);
        return;
      }
      this.on("close", cb);
    }
    // Not closing by itself, it is destroyed at 'finish'.
    if (!this.autoClose) this.on("finish", this.destroy);
    this.end();
  };
  WriteStream.prototype.destroySoon = undefined;
  Object.defineProperty(WriteStream.prototype, "pending", pendingAccessor);

  fs.ReadStream = fs.FileReadStream = ReadStream;
  fs.WriteStream = fs.FileWriteStream = WriteStream;
  fs.createReadStream = function createReadStream(path, options) {
    return new fs.ReadStream(path, options);
  };
  fs.createWriteStream = function createWriteStream(path, options) {
    return new fs.WriteStream(path, options);
  };
  fs.watch = fs.watchFile = () => {
    throw new Error("zipp: fs.watch is not supported");
  };
  return fs;
})
