// node:util — format, inspect (the engine's console rendering), inherits,
// promisify, deprecate, deep equality, types.
(function (host) {
  "use strict";
  const inspectCustom = Symbol.for("nodejs.util.inspect.custom");
  const promisifyCustom = Symbol.for("nodejs.util.promisify.custom");
  const toStr = Object.prototype.toString;
  const tag = (v) => toStr.call(v);

  function inspect(value, opts) {
    void opts;
    if (value !== null && typeof value === "object" && typeof value[inspectCustom] === "function") {
      const r = value[inspectCustom](2, { stylize: (s) => s }, inspect);
      return typeof r === "string" ? r : inspect(r);
    }
    return host.inspect(value);
  }
  inspect.custom = inspectCustom;
  inspect.defaultOptions = { depth: 2, colors: false, showHidden: false, maxArrayLength: 100, breakLength: 80 };
  inspect.colors = {};
  inspect.styles = {};

  function formatWithOptions(opts, ...args) {
    void opts;
    return format(...args);
  }
  function format(...args) {
    const first = args[0];
    let a = 0;
    let out = "";
    if (typeof first === "string") {
      if (args.length === 1) return first;
      a = 1;
      let last = 0;
      for (let i = 0; i < first.length - 1; i++) {
        if (first.charCodeAt(i) !== 37) continue;
        const c = first[i + 1];
        let rep;
        if (c === "%") {
          out += first.slice(last, i) + "%";
          last = i + 2;
          i++;
          continue;
        }
        if (a >= args.length) continue;
        const v = args[a];
        switch (c) {
          case "s":
            rep = typeof v === "string" ? v : typeof v === "bigint" ? v + "n" : typeof v === "symbol" ? String(v)
              : v !== null && typeof v === "object" ? inspect(v, { depth: 0 }) : String(v);
            break;
          case "d":
          case "i": {
            if (typeof v === "bigint") rep = v + "n";
            else if (typeof v === "symbol") rep = "NaN";
            else {
              const n = Number(v);
              rep = String(c === "i" ? Math.trunc(n) : n);
            }
            break;
          }
          case "f":
            rep = typeof v === "symbol" ? "NaN" : String(parseFloat(v));
            break;
          case "j":
            try {
              rep = JSON.stringify(v);
            } catch {
              rep = "[Circular]";
            }
            break;
          case "o":
          case "O":
            rep = inspect(v);
            break;
          case "c":
            rep = "";
            break;
          default:
            continue;
        }
        out += first.slice(last, i) + rep;
        last = i + 2;
        i++;
        a++;
      }
      out += first.slice(last);
    }
    for (; a < args.length; a++) {
      const v = args[a];
      if (out.length > 0 || a > 0) out += " ";
      out += typeof v === "string" ? v : inspect(v);
    }
    return out;
  }

  function inherits(ctor, superCtor) {
    if (typeof ctor !== "function") throw new TypeError('The "ctor" argument must be of type function');
    if (typeof superCtor !== "function") throw new TypeError('The "superCtor" argument must be of type function');
    if (superCtor.prototype === undefined) throw new TypeError('The "superCtor.prototype" property must be of type object');
    Object.defineProperty(ctor, "super_", { value: superCtor, writable: true, configurable: true });
    Object.setPrototypeOf(ctor.prototype, superCtor.prototype);
  }
  function promisify(original) {
    if (typeof original !== "function") throw new TypeError('The "original" argument must be of type function');
    if (original[promisifyCustom]) {
      const fn = original[promisifyCustom];
      return Object.defineProperty(fn, promisifyCustom, { value: fn, enumerable: false, writable: false, configurable: true });
    }
    function fn(...args) {
      return new Promise((resolve, reject) => {
        Reflect.apply(original, this, [...args, (err, ...values) => {
          if (err) return reject(err);
          resolve(values.length > 1 ? values : values[0]);
        }]);
      });
    }
    Object.setPrototypeOf(fn, Object.getPrototypeOf(original));
    Object.defineProperty(fn, promisifyCustom, { value: fn, enumerable: false, writable: false, configurable: true });
    return Object.defineProperties(fn, Object.getOwnPropertyDescriptors(original));
  }
  promisify.custom = promisifyCustom;
  function callbackify(original) {
    if (typeof original !== "function") throw new TypeError('The "original" argument must be of type function');
    return function (...args) {
      const cb = args.pop();
      if (typeof cb !== "function") throw new TypeError('The last argument must be of type function');
      Reflect.apply(original, this, args).then(
        (v) => process.nextTick(cb, null, v),
        (e) => process.nextTick(cb, e || Object.assign(new Error("Promise was rejected with a falsy value"), { reason: e })),
      );
    };
  }
  const warned = new Set();
  function deprecate(fn, msg, code) {
    if (typeof fn !== "function") throw new TypeError('The "fn" argument must be of type function');
    return function (...args) {
      const key = code || msg;
      if (!warned.has(key)) {
        warned.add(key);
        if (!(globalThis.process && process.noDeprecation)) {
          process.emitWarning(msg, "DeprecationWarning");
        }
      }
      return new.target ? Reflect.construct(fn, args, new.target) : Reflect.apply(fn, this, args);
    };
  }
  function debuglog(section) {
    const env = (globalThis.process && process.env.NODE_DEBUG) || "";
    const enabled = env.split(/[\s,]+/).some((s) => s && new RegExp("^" + s.replace(/\*/g, ".*") + "$", "i").test(section));
    const log = enabled
      ? (...args) => process.stderr.write(section.toUpperCase() + " " + process.pid + ": " + format(...args) + "\n")
      : () => {};
    Object.defineProperty(log, "enabled", { get: () => enabled });
    return log;
  }

  // Deep equality: assert.deepStrictEqual's comparison (`strict`) and
  // deepEqual's (loose: == on primitives, prototypes ignored).
  function deepEqual(a, b, strict, memo) {
    if (strict ? Object.is(a, b) : a == b || (a !== a && b !== b)) return true;
    if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) {
      return false;
    }
    if (strict && Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
    const ta = tag(a), tb = tag(b);
    if (ta !== tb) return false;
    if (ta === "[object Date]") return a.getTime() === b.getTime();
    if (ta === "[object RegExp]") return a.source === b.source && a.flags === b.flags && a.lastIndex === b.lastIndex;
    if (a instanceof Error && b instanceof Error && (a.message !== b.message || a.name !== b.name)) return false;
    if (ArrayBuffer.isView(a)) {
      if (a.length !== b.length || a.byteLength !== b.byteLength) return false;
      for (let i = 0; i < a.length; i++) if (!Object.is(a[i], b[i]) && !(!strict && a[i] == b[i])) return false;
    }
    if (ta === "[object Number]" || ta === "[object String]" || ta === "[object Boolean]") {
      if (!Object.is(a.valueOf(), b.valueOf())) return false;
    }
    memo ||= new Map();
    const seen = memo.get(a);
    if (seen !== undefined && seen === b) return true;
    memo.set(a, b);
    if (ta === "[object Map]") {
      if (a.size !== b.size) return false;
      for (const [k, v] of a) {
        if (!b.has(k) || !deepEqual(v, b.get(k), strict, memo)) return false;
      }
    } else if (ta === "[object Set]") {
      if (a.size !== b.size) return false;
      outer: for (const v of a) {
        if (b.has(v)) continue;
        if (typeof v === "object" && v !== null) {
          for (const w of b) if (deepEqual(v, w, strict, memo)) continue outer;
        }
        return false;
      }
    }
    const ka = Object.keys(a), kb = Object.keys(b);
    if (ka.length !== kb.length) return false;
    for (const k of ka) {
      if (!Object.prototype.hasOwnProperty.call(b, k)) return false;
      if (!deepEqual(a[k], b[k], strict, memo)) return false;
    }
    if (strict) {
      const sa = Object.getOwnPropertySymbols(a).filter((s) => Object.prototype.propertyIsEnumerable.call(a, s));
      const sb = Object.getOwnPropertySymbols(b).filter((s) => Object.prototype.propertyIsEnumerable.call(b, s));
      if (sa.length !== sb.length) return false;
      for (const s of sa) if (!deepEqual(a[s], b[s], strict, memo)) return false;
    }
    return true;
  }

  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const GeneratorFunction = Object.getPrototypeOf(function* () {}).constructor;
  const typedArrayTag = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(Uint8Array.prototype), Symbol.toStringTag).get;
  const types = {
    isPromise: (v) => tag(v) === "[object Promise]",
    isDate: (v) => tag(v) === "[object Date]",
    isRegExp: (v) => tag(v) === "[object RegExp]",
    isMap: (v) => tag(v) === "[object Map]",
    isSet: (v) => tag(v) === "[object Set]",
    isWeakMap: (v) => tag(v) === "[object WeakMap]",
    isWeakSet: (v) => tag(v) === "[object WeakSet]",
    isArrayBuffer: (v) => tag(v) === "[object ArrayBuffer]",
    isSharedArrayBuffer: (v) => tag(v) === "[object SharedArrayBuffer]",
    isAnyArrayBuffer: (v) => tag(v) === "[object ArrayBuffer]" || tag(v) === "[object SharedArrayBuffer]",
    isDataView: (v) => tag(v) === "[object DataView]",
    isTypedArray: (v) => typedArrayTag.call(v) !== undefined,
    isUint8Array: (v) => typedArrayTag.call(v) === "Uint8Array",
    isArrayBufferView: (v) => ArrayBuffer.isView(v),
    isAsyncFunction: (v) => typeof v === "function" && v instanceof AsyncFunction,
    isGeneratorFunction: (v) => typeof v === "function" && v instanceof GeneratorFunction,
    isGeneratorObject: (v) => tag(v) === "[object Generator]",
    isNativeError: (v) => v instanceof Error,
    isBoxedPrimitive: (v) => ["[object Number]", "[object String]", "[object Boolean]", "[object BigInt]", "[object Symbol]"].includes(tag(v)) && typeof v === "object",
    isProxy: () => false,
    isModuleNamespaceObject: (v) => tag(v) === "[object Module]",
  };

  return {
    format,
    formatWithOptions,
    inspect,
    inherits,
    promisify,
    callbackify,
    deprecate,
    debuglog,
    debug: debuglog,
    isDeepStrictEqual: (a, b) => deepEqual(a, b, true),
    _deepEqual: deepEqual,
    types,
    TextEncoder,
    TextDecoder,
    isArray: Array.isArray,
    isBoolean: (v) => typeof v === "boolean",
    isNull: (v) => v === null,
    isNullOrUndefined: (v) => v == null,
    isNumber: (v) => typeof v === "number",
    isString: (v) => typeof v === "string",
    isSymbol: (v) => typeof v === "symbol",
    isUndefined: (v) => v === undefined,
    isRegExp: types.isRegExp,
    isObject: (v) => v !== null && typeof v === "object",
    isDate: types.isDate,
    isError: (v) => v instanceof Error,
    isFunction: (v) => typeof v === "function",
    isPrimitive: (v) => v === null || (typeof v !== "object" && typeof v !== "function"),
    stripVTControlCharacters: (s) => String(s).replace(/\u001b\[[0-9;]*[A-Za-z]/g, ""),
    styleText: (_format, text) => String(text),
    toUSVString: (s) => String(s).toWellFormed(),
    getSystemErrorName: (n) => "Unknown system error " + n,
    aborted: () => Promise.resolve(),
  };
})
