// The `WebAssembly` namespace (see mod.rs). One function of the runtime's
// native `op`, run the first time a member of the namespace is read; it
// returns the members, which the namespace's accessors then hand out.
//
// The runtime itself is wasmi, behind `op(name, ...args)`. Modules, instances,
// memories, tables and globals are numbered there; the objects here carry
// their numbers. An error the runtime reports comes back as a record
// `[ERR, kind, message]` and is thrown here as the right class, so that
// `CompileError`, `LinkError` and `RuntimeError` are real subclasses of Error.
// An exception thrown by a JavaScript import is not a record: it unwinds
// through the WebAssembly frames and arrives as itself.
//
// Its bindings are `let`, never `const` (see node/bootstrap.js: a captured
// const cell turns off compiled code's inline writes for the whole run).
(function (op) {
  "use strict";
  let ERR = "\u0000wasm-error";
  let defineProperty = Object.defineProperty;
  let isArray = Array.isArray;

  function makeError(name) {
    let C = class extends Error {
      constructor(message) {
        super(message);
      }
    };
    defineProperty(C, "name", { value: name });
    defineProperty(C.prototype, "name", { value: name, writable: true, configurable: true });
    return C;
  }
  let CompileError = makeError("CompileError");
  let LinkError = makeError("LinkError");
  let RuntimeError = makeError("RuntimeError");
  let errors = { CompileError, LinkError, RuntimeError, TypeError, RangeError };

  function check(r) {
    if (isArray(r) && r[0] === ERR) throw new errors[r[1]](r[2]);
    return r;
  }

  // Which object each number belongs to, and back.
  let ids = new WeakMap();
  let funcIds = new WeakMap();
  let funcs = new Map();
  let memories = new Map();
  let tables = new Map();
  let globals = new Map();

  function idOf(value, C, what) {
    let id = value !== null && typeof value === "object" ? ids.get(value) : undefined;
    if (id === undefined || !(value instanceof C)) {
      throw new TypeError(`WebAssembly: ${what} must be a WebAssembly.${C.name}`);
    }
    return id;
  }

  function bufferSource(source) {
    if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) return source;
    throw new TypeError("WebAssembly: argument must be an ArrayBuffer or a typed array");
  }

  function toIndex(value, what) {
    let n = Number(value);
    if (!Number.isFinite(n) || n < 0 || n > 0xffffffff) {
      throw new TypeError(`WebAssembly: ${what} must be a non-negative integer`);
    }
    return Math.trunc(n);
  }

  // An exported function: one object per runtime function, so the same
  // function exported twice, or read back out of a table, is `===`.
  function exportedFunction(id, arity) {
    let f = funcs.get(id);
    if (f !== undefined) return f;
    f = function (...args) {
      return check(op("call", id, args));
    };
    defineProperty(f, "length", { value: arity });
    defineProperty(f, "name", { value: String(id) });
    funcIds.set(f, id);
    funcs.set(id, f);
    return f;
  }

  function adopt(map, C, id) {
    let object = map.get(id);
    if (object === undefined) {
      object = Object.create(C.prototype);
      ids.set(object, id);
      map.set(id, object);
    }
    return object;
  }

  // What the runtime hands back for a funcref: null, or [id, arity].
  function funcref(r) {
    return r === null ? null : exportedFunction(r[0], r[1]);
  }

  class Module {
    constructor(bytes) {
      if (new.target === undefined) throw new TypeError("WebAssembly.Module must be called with new");
      ids.set(this, check(op("compile", bufferSource(bytes))));
    }
    static imports(module) {
      return check(op("moduleImports", idOf(module, Module, "argument")))
        .map(([module, name, kind]) => ({ module, name, kind }));
    }
    static exports(module) {
      return check(op("moduleExports", idOf(module, Module, "argument")))
        .map(([name, kind]) => ({ name, kind }));
    }
    static customSections(module, name) {
      return check(op("customSections", idOf(module, Module, "argument"), String(name)));
    }
  }

  function resolveImports(moduleId, importObject) {
    let list = check(op("moduleImports", moduleId));
    if (list.length > 0 && (importObject === null || (typeof importObject !== "object" && typeof importObject !== "function"))) {
      throw new TypeError("WebAssembly.Instance: imports argument must be present and must be an object");
    }
    return list.map(([module, name, kind]) => {
      let namespace = importObject[module];
      if (namespace === null || (typeof namespace !== "object" && typeof namespace !== "function")) {
        throw new TypeError(`WebAssembly.Instance: import module "${module}" is not an object`);
      }
      let value = namespace[name];
      let where = `"${module}" "${name}"`;
      switch (kind) {
        case "function": {
          if (typeof value !== "function") throw new LinkError(`import ${where}: function import requires a callable`);
          let id = funcIds.get(value);
          return id !== undefined ? ["wasmfunc", id] : ["jsfunc", value];
        }
        case "memory":
          if (!(value instanceof Memory)) throw new LinkError(`import ${where}: memory import must be a WebAssembly.Memory`);
          return ["memory", ids.get(value)];
        case "table":
          if (!(value instanceof Table)) throw new LinkError(`import ${where}: table import must be a WebAssembly.Table`);
          return ["table", ids.get(value)];
        case "global":
          if (value instanceof Global) return ["global", ids.get(value)];
          if (typeof value === "number" || typeof value === "bigint") return ["value", value];
          throw new LinkError(`import ${where}: global import must be a number, a BigInt or a WebAssembly.Global`);
        default:
          throw new LinkError(`import ${where}: unsupported import kind ${kind}`);
      }
    });
  }

  class Instance {
    constructor(module, importObject) {
      if (new.target === undefined) throw new TypeError("WebAssembly.Instance must be called with new");
      let moduleId = idOf(module, Module, "first argument");
      let instanceId = check(op("instantiate", moduleId, resolveImports(moduleId, importObject)));
      let exports = Object.create(null);
      for (let [name, kind, id, arity] of check(op("instanceExports", instanceId))) {
        exports[name] =
          kind === "function" ? exportedFunction(id, arity)
          : kind === "memory" ? adopt(memories, Memory, id)
          : kind === "table" ? adopt(tables, Table, id)
          : adopt(globals, Global, id);
      }
      Object.freeze(exports);
      defineProperty(this, "exports", { value: exports, enumerable: true, configurable: true });
    }
  }

  class Memory {
    constructor(descriptor) {
      if (new.target === undefined) throw new TypeError("WebAssembly.Memory must be called with new");
      if (descriptor === null || typeof descriptor !== "object") throw new TypeError("WebAssembly.Memory: descriptor must be an object");
      let initial = toIndex(descriptor.initial ?? descriptor.minimum ?? 0, "initial");
      let maximum = descriptor.maximum === undefined ? undefined : toIndex(descriptor.maximum, "maximum");
      let id = check(op("memoryNew", initial, maximum));
      ids.set(this, id);
      memories.set(id, this);
    }
    get buffer() {
      return check(op("memoryBuffer", idOf(this, Memory, "receiver")));
    }
    grow(delta) {
      return check(op("memoryGrow", idOf(this, Memory, "receiver"), toIndex(delta, "delta")));
    }
  }

  class Table {
    constructor(descriptor, value) {
      if (new.target === undefined) throw new TypeError("WebAssembly.Table must be called with new");
      if (descriptor === null || typeof descriptor !== "object") throw new TypeError("WebAssembly.Table: descriptor must be an object");
      let element = String(descriptor.element);
      if (element !== "anyfunc" && element !== "funcref") {
        throw new TypeError("WebAssembly.Table: only funcref tables are supported");
      }
      let initial = toIndex(descriptor.initial ?? descriptor.minimum ?? 0, "initial");
      let maximum = descriptor.maximum === undefined ? undefined : toIndex(descriptor.maximum, "maximum");
      let id = check(op("tableNew", initial, maximum));
      ids.set(this, id);
      tables.set(id, this);
      if (value !== undefined && value !== null) {
        for (let i = 0; i < initial; i++) this.set(i, value);
      }
    }
    get length() {
      return check(op("tableSize", idOf(this, Table, "receiver")));
    }
    get(index) {
      return funcref(check(op("tableGet", idOf(this, Table, "receiver"), toIndex(index, "index"))));
    }
    set(index, value) {
      let id = idOf(this, Table, "receiver");
      let f = value === undefined || value === null ? null : funcIds.get(value);
      if (f === undefined) throw new TypeError("WebAssembly.Table.set: value must be null or an exported WebAssembly function");
      check(op("tableSet", id, toIndex(index, "index"), f));
    }
    grow(delta, value) {
      let id = idOf(this, Table, "receiver");
      let old = check(op("tableGrow", id, toIndex(delta, "delta")));
      if (value !== undefined && value !== null) {
        for (let i = old; i < old + delta; i++) this.set(i, value);
      }
      return old;
    }
  }

  class Global {
    constructor(descriptor, value) {
      if (new.target === undefined) throw new TypeError("WebAssembly.Global must be called with new");
      if (descriptor === null || typeof descriptor !== "object") throw new TypeError("WebAssembly.Global: descriptor must be an object");
      let type = String(descriptor.value);
      if (!["i32", "i64", "f32", "f64"].includes(type)) {
        throw new TypeError(`WebAssembly.Global: unsupported value type ${type}`);
      }
      let initial = value === undefined ? (type === "i64" ? 0n : 0) : value;
      let id = check(op("globalNew", type, !!descriptor.mutable, initial));
      ids.set(this, id);
      globals.set(id, this);
    }
    get value() {
      return check(op("globalGet", idOf(this, Global, "receiver")));
    }
    set value(v) {
      check(op("globalSet", idOf(this, Global, "receiver"), v));
    }
    valueOf() {
      return this.value;
    }
  }

  for (let [C, tag] of [[Module, "Module"], [Instance, "Instance"], [Memory, "Memory"], [Table, "Table"], [Global, "Global"]]) {
    defineProperty(C.prototype, Symbol.toStringTag, { value: `WebAssembly.${tag}`, configurable: true });
  }

  function validate(bytes) {
    return check(op("validate", bufferSource(bytes)));
  }

  function compile(bytes) {
    return new Promise((resolve, reject) => {
      try {
        resolve(new Module(bytes));
      } catch (e) {
        reject(e);
      }
    });
  }

  function instantiate(source, importObject) {
    if (source instanceof Module) {
      return new Promise((resolve, reject) => {
        try {
          resolve(new Instance(source, importObject));
        } catch (e) {
          reject(e);
        }
      });
    }
    return compile(source).then((module) => ({ module, instance: new Instance(module, importObject) }));
  }

  // A `Response` (or a promise of one), as `fetch` gives: its bytes are what
  // is compiled. Nothing here depends on how the host implements fetch.
  async function compileStreaming(source) {
    let response = await source;
    if (response === null || typeof response !== "object" || typeof response.arrayBuffer !== "function") {
      throw new TypeError("WebAssembly.compileStreaming: argument must be a Response or a promise of one");
    }
    return compile(await response.arrayBuffer());
  }

  async function instantiateStreaming(source, importObject) {
    let module = await compileStreaming(source);
    return { module, instance: new Instance(module, importObject) };
  }

  return {
    Module, Instance, Memory, Table, Global,
    CompileError, LinkError, RuntimeError,
    validate, compile, instantiate, compileStreaming, instantiateStreaming,
  };
})
