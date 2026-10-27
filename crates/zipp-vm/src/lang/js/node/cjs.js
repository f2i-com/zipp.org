// CommonJS (see mod.rs): `require`, `module`, `exports` as Node gives them,
// over the host's resolution (`host.resolve`), file reads and Node's
// wrapper function (`host.compileCjs`). One loader per run, shared by the
// main script's `require` and the module loader's imports of CommonJS
// files; a module is cached by its real path and run once, so a cycle sees
// the other module's `exports` as far as it had got.
(function (host) {
  "use strict";
  const call = Function.prototype.call.bind(Function.prototype.call);
  const ObjectCreate = Object.create;
  const ObjectKeys = Object.keys;
  const hasOwn = Object.prototype.hasOwnProperty;
  const cache = ObjectCreate(null);
  const builtinCache = ObjectCreate(null);
  const builtinModules = [
    "assert", "assert/strict", "async_hooks", "buffer", "child_process", "cluster", "console",
    "constants", "crypto", "dgram", "diagnostics_channel", "dns", "dns/promises", "events", "fs",
    "fs/promises", "http", "http2", "https", "inspector", "module", "net", "os", "path",
    "path/posix", "path/win32", "perf_hooks", "process", "punycode", "querystring", "readline",
    "readline/promises", "repl", "stream", "stream/promises", "stream/web", "string_decoder",
    "sys", "timers", "timers/promises", "tls", "trace_events", "tty", "url", "util",
    "util/types", "v8", "vm", "wasi", "worker_threads", "zlib",
  ];
  const builtinSet = new Set(builtinModules);
  function isBuiltin(id) {
    return typeof id === "string" && (id.startsWith("node:") || builtinSet.has(id));
  }
  function dirname(p) {
    const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
    if (i < 0) return ".";
    if (i === 0) return p.slice(0, 1);
    if (i === 2 && p[1] === ":") return p.slice(0, 3);
    return p.slice(0, i);
  }

  class Module {
    constructor(id = "", parent) {
      this.id = id;
      this.path = dirname(id);
      this.exports = {};
      this.filename = id || null;
      this.loaded = false;
      this.children = [];
      this.paths = [];
      if (parent) {
        this.parent = parent;
        parent.children.push(this);
      }
    }
    require(id) {
      return requireFrom(this, id);
    }
  }
  Module.builtinModules = builtinModules;
  Module.isBuiltin = isBuiltin;
  Module._cache = cache;
  Module.createRequire = (filename) => {
    let f = String(filename);
    if (f.startsWith("file:")) f = builtin("url").fileURLToPath(f);
    return makeRequire(new Module(f));
  };
  Module.Module = Module;

  function notFound(id, from) {
    const e = new Error("Cannot find module '" + id + "'" + (from && from.filename ? "\nRequire stack:\n- " + from.filename : ""));
    e.code = "MODULE_NOT_FOUND";
    e.requireStack = from && from.filename ? [from.filename] : [];
    return e;
  }
  function resolve(id, from) {
    if (isBuiltin(id)) return id;
    const r = host.resolve(id, from.path, true);
    if (r == null) throw notFound(id, from);
    return r;
  }
  function makeRequire(mod) {
    function require(id) {
      return requireFrom(mod, id);
    }
    require.resolve = (id) => resolve(String(id), mod);
    require.resolve.paths = () => null;
    require.cache = cache;
    require.main = mainModule;
    require.extensions = ObjectCreate(null);
    return require;
  }
  function requireFrom(parent, id) {
    if (typeof id !== "string") {
      throw new TypeError('The "id" argument must be of type string. Received ' + (id === null ? "null" : typeof id));
    }
    if (id === "") throw new TypeError("The argument 'id' must be a non-empty string. Received ''");
    if (isBuiltin(id)) return builtin(id.startsWith("node:") ? id.slice(5) : id);
    return loadFile(resolve(id, parent), parent);
  }
  function stripBOM(s) {
    return s.charCodeAt(0) === 0xfeff ? s.slice(1) : s;
  }
  function loadFile(filename, parent) {
    const hit = cache[filename];
    if (hit) return hit.exports;
    const format = host.format(filename);
    if (format === "module") return host.importEsm(filename);
    const m = new Module(filename, parent);
    cache[filename] = m;
    try {
      if (format === "json") {
        m.exports = JSON.parse(stripBOM(host.readFile(filename, true)));
      } else {
        const fn = host.compileCjs(host.readFile(filename, true), filename);
        call(fn, m.exports, m.exports, makeRequire(m), m, filename, m.path);
      }
    } catch (e) {
      delete cache[filename];
      throw e;
    }
    m.loaded = true;
    return m.exports;
  }

  // ---- builtins --------------------------------------------------------------
  const implemented = {
    path: "path", "path/posix": "path", "path/win32": "path", events: "events", util: "util",
    "util/types": "util", sys: "util", fs: "fs", "fs/promises": "fs", os: "os", tty: "tty",
    url: "url", module: "module", assert: "assert", "assert/strict": "assert", buffer: "buffer",
    crypto: "crypto", string_decoder: "string_decoder",
  };
  function unsupported(name) {
    // A module the layer does not have: requiring it works (packages often
    // do at their top), using it says what is missing.
    const fail = (what) => () => {
      const e = new Error("zipp: node:" + name + (what ? "." + what : "") + " is not supported");
      e.code = "ERR_ZIPP_UNSUPPORTED";
      throw e;
    };
    const stub = new Proxy(fail(""), {
      get(_, key) {
        if (key === "__esModule" || typeof key === "symbol") return undefined;
        if (key === "default") return stub;
        return fail(String(key));
      },
    });
    return stub;
  }
  function builtin(name) {
    if (name in builtinCache) return builtinCache[name];
    let value;
    if (name === "process") value = globalThis.process;
    else if (name === "timers") {
      value = { setTimeout, clearTimeout, setInterval, clearInterval, setImmediate, clearImmediate };
    } else if (name === "timers/promises") {
      value = {
        setTimeout: (ms, v) => new Promise((r) => setTimeout(() => r(v), ms)),
        setImmediate: (v) => new Promise((r) => setImmediate(() => r(v))),
      };
    } else if (name === "console") value = console;
    else if (hasOwn.call(implemented, name)) {
      const part = implemented[name];
      const base = builtinCache["%" + part] ||= host.load("builtin/" + part)(host, builtin, Module);
      if (name === "fs/promises") value = base.promises;
      else if (name === "path/posix") value = base.posix;
      else if (name === "path/win32") value = base.win32;
      else if (name === "util/types") value = base.types;
      else if (name === "assert/strict") value = base.strict;
      else value = base;
    } else if (builtinSet.has(name)) value = unsupported(name);
    else {
      const e = new Error("No such built-in module: node:" + name);
      e.code = "ERR_UNKNOWN_BUILTIN_MODULE";
      throw e;
    }
    builtinCache[name] = value;
    return value;
  }

  let mainModule;
  return {
    Module,
    builtin,
    isBuiltin,
    // A CommonJS file an ES module imports.
    importCjs(filename) {
      return loadFile(filename, undefined);
    },
    // The names an ES module importing a CommonJS module may import.
    exportNames(exports) {
      if (exports === null || (typeof exports !== "object" && typeof exports !== "function")) return [];
      const names = ObjectKeys(exports);
      if (hasOwn.call(exports, "__esModule") && !names.includes("__esModule")) names.push("__esModule");
      return names;
    },
    // The main script's CommonJS bindings.
    main(filename) {
      mainModule = new Module(filename);
      mainModule.loaded = false;
      cache[filename] = mainModule;
      return { module: mainModule, require: makeRequire(mainModule), filename, dirname: mainModule.path };
    },
  };
})
