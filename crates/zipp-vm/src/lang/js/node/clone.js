// `structuredClone` (see mod.rs): the HTML structured clone of plain data,
// with cycles, as Node's.
(function (host) {
  "use strict";
  const G = globalThis;
  const defineProperty = Object.defineProperty;
  const ObjectCreate = Object.create;
  const ObjectKeys = Object.keys;
  const getPrototypeOf = Object.getPrototypeOf;
  const PromiseResolve = Promise.resolve.bind(Promise);
  const then = Promise.prototype.then;
  const call = Function.prototype.call.bind(Function.prototype.call);
  const apply = Function.prototype.call.bind(Function.prototype.apply);
  const MapCtor = Map;
  const mapGet = Map.prototype.get;
  const mapSet = Map.prototype.set;
  const mapHas = Map.prototype.has;
  const mapDelete = Map.prototype.delete;
  const SetCtor = Set;
  const ArrayIsArray = Array.isArray;
  const SymbolToPrimitive = Symbol.toPrimitive;
  // ---- structuredClone ------------------------------------------------------
  function DataCloneError(what) {
    const e = new Error(String(what) + " could not be cloned.");
    e.name = "DataCloneError";
    e.code = 25;
    return e;
  }
  const errorNames = new SetCtor(["Error", "EvalError", "RangeError", "ReferenceError", "SyntaxError", "TypeError", "URIError"]);
  const errorCtors = { Error, EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError };
  const typedArrayProto = getPrototypeOf(Uint8Array.prototype);
  const taTag = Object.getOwnPropertyDescriptor(typedArrayProto, Symbol.toStringTag).get;
  const objToString = Object.prototype.toString;
  function structuredClone(value, options) {
    if (arguments.length === 0) {
      throw new TypeError('The "value" argument must be specified');
    }
    const memory = new MapCtor();
    function clone(v) {
      const t = typeof v;
      if (t === "symbol") throw DataCloneError(String(v));
      if (t === "function") throw DataCloneError(call(Function.prototype.toString, v));
      if (t !== "object" || v === null) return v;
      if (call(mapHas, memory, v)) return call(mapGet, memory, v);
      const tag = call(objToString, v);
      let out;
      switch (tag) {
        case "[object Boolean]": out = Object(call(Boolean.prototype.valueOf, v)); break;
        case "[object Number]": out = Object(call(Number.prototype.valueOf, v)); break;
        case "[object String]": out = Object(call(String.prototype.valueOf, v)); break;
        case "[object BigInt]": out = Object(call(BigInt.prototype.valueOf, v)); break;
        case "[object Date]": out = new Date(call(Date.prototype.getTime, v)); break;
        case "[object RegExp]": out = new RegExp(v.source, v.flags); break;
        case "[object ArrayBuffer]": out = call(ArrayBuffer.prototype.slice, v, 0); break;
        case "[object DataView]": {
          const buf = clone(v.buffer);
          out = new DataView(buf, v.byteOffset, v.byteLength);
          break;
        }
        case "[object Map]": {
          out = new MapCtor();
          call(mapSet, memory, v, out);
          for (const [k, val] of call(Map.prototype.entries, v)) call(mapSet, out, clone(k), clone(val));
          return out;
        }
        case "[object Set]": {
          out = new SetCtor();
          call(mapSet, memory, v, out);
          for (const val of call(Set.prototype.values, v)) call(Set.prototype.add, out, clone(val));
          return out;
        }
        case "[object Error]": {
          const name = errorNames.has(v.name) ? v.name : "Error";
          out = new errorCtors[name](v.message);
          call(mapSet, memory, v, out);
          if ("stack" in v) defineProperty(out, "stack", { value: v.stack, writable: true, configurable: true, enumerable: false });
          if ("cause" in v) defineProperty(out, "cause", { value: clone(v.cause), writable: true, configurable: true, enumerable: false });
          return out;
        }
        case "[object Array]": {
          out = new Array(v.length);
          call(mapSet, memory, v, out);
          for (const k of ObjectKeys(v)) out[k] = clone(v[k]);
          return out;
        }
        case "[object Object]":
        case "[object Arguments]": {
          if (call(taTag, v) !== undefined) break;
          out = {};
          call(mapSet, memory, v, out);
          for (const k of ObjectKeys(v)) out[k] = clone(v[k]);
          return out;
        }
        default: {
          const kind = call(taTag, v);
          if (kind !== undefined) {
            const buf = clone(v.buffer);
            out = new G[kind](buf, v.byteOffset, v.length);
            break;
          }
          if (typeof v === "object" && !ArrayIsArray(v)) {
            // A Promise, a WeakMap, a Proxy of a function…: not cloneable.
            if (tag !== "[object Object]") throw DataCloneError(tag === "[object Promise]" ? "#<Promise>" : tag);
          }
          out = {};
          call(mapSet, memory, v, out);
          for (const k of ObjectKeys(v)) out[k] = clone(v[k]);
          return out;
        }
      }
      call(mapSet, memory, v, out);
      return out;
    }
    void options;
    return clone(value);
  }
  return { structuredClone };
})
