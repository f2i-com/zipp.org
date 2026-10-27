// node:fs — the synchronous file API over the host's file operations, its
// callback and promise forms over that.
(function (host, builtin) {
  "use strict";
  const { Buffer, _toBuffer } = builtin("buffer");
  const pathMod = builtin("path");
  function fsError(e, syscall, path) {
    const m = /^(?:Error: )?([A-Z]+): (.*)$/.exec(e && e.message ? e.message : String(e));
    if (!m) return e;
    const err = new Error(m[1] + ": " + m[2]);
    err.errno = { ENOENT: -2, EACCES: -13, EEXIST: -17, ENOTDIR: -20, EISDIR: -21, ENOTEMPTY: -39 }[m[1]] || -5;
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
      path = toPath(path);
      const enc = encodingOf(options);
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
  fs.createReadStream = () => {
    throw new Error("zipp: fs.createReadStream is not supported");
  };
  fs.createWriteStream = () => {
    throw new Error("zipp: fs.createWriteStream is not supported");
  };
  fs.watch = fs.watchFile = () => {
    throw new Error("zipp: fs.watch is not supported");
  };
  return fs;
})
