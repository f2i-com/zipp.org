// Node's timers and `queueMicrotask` (see mod.rs), over the host's one-shot
// timer: ids, arguments, cancellation, intervals.
(function (host, nativeSetTimeout) {
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
  // ---- queueMicrotask -----------------------------------------------------
  function queueMicrotask(callback) {
    if (typeof callback !== "function") {
      throw new TypeError('The "callback" argument must be of type function. Received ' + typeof callback);
    }
    call(then, PromiseResolve(), () => callback());
  }

  // ---- timers -------------------------------------------------------------
  // Over the host's one-shot timer: ids, arguments, cancellation, intervals.
  const active = new MapCtor();
  let lastId = 0;
  function delayOf(ms) {
    ms = +ms;
    return ms >= 1 && ms <= 2147483647 ? ms : 1;
  }
  class Timeout {
    constructor(callback, ms, args, repeat) {
      this._id = ++lastId;
      this._onTimeout = callback;
      this._idleTimeout = ms;
      this._args = args;
      this._repeat = repeat;
      this._ref = true;
      this._generation = 0;
      this._destroyed = false;
    }
    ref() { this._ref = true; return this; }
    unref() { this._ref = false; return this; }
    hasRef() { return this._ref; }
    refresh() { if (!this._destroyed) arm(this); return this; }
    close() { clear(this); return this; }
    [SymbolToPrimitive]() { return this._id; }
  }
  function arm(t) {
    const generation = ++t._generation;
    call(mapSet, active, t._id, t);
    nativeSetTimeout(() => {
      if (t._destroyed || t._generation !== generation) return;
      if (t._repeat) arm(t);
      else { t._destroyed = true; call(mapDelete, active, t._id); }
      apply(t._onTimeout, undefined, t._args);
    }, t._idleTimeout);
  }
  function clear(t) {
    if (t == null) return;
    const timer = typeof t === "object" ? t : call(mapGet, active, +t);
    if (timer instanceof Timeout || (timer && typeof timer === "object" && "_destroyed" in timer)) {
      timer._destroyed = true;
      call(mapDelete, active, timer._id);
    }
  }
  function checkCallback(callback) {
    if (typeof callback !== "function") {
      throw new TypeError('The "callback" argument must be of type function. Received ' + (callback === null ? "null" : typeof callback));
    }
  }
  function setTimeout(callback, ms, ...args) {
    checkCallback(callback);
    const t = new Timeout(callback, delayOf(ms), args, false);
    arm(t);
    return t;
  }
  function setInterval(callback, ms, ...args) {
    checkCallback(callback);
    const t = new Timeout(callback, delayOf(ms), args, true);
    arm(t);
    return t;
  }
  class Immediate {
    constructor(callback, args) { this._onImmediate = callback; this._argv = args; this._destroyed = false; this._ref = true; }
    ref() { this._ref = true; return this; }
    unref() { this._ref = false; return this; }
    hasRef() { return this._ref; }
  }
  function setImmediate(callback, ...args) {
    checkCallback(callback);
    const im = new Immediate(callback, args);
    nativeSetTimeout(() => { if (!im._destroyed) { im._destroyed = true; apply(callback, undefined, args); } }, 0);
    return im;
  }
  function clearImmediate(im) {
    if (im && typeof im === "object") im._destroyed = true;
  }
  return {
    setTimeout,
    clearTimeout: clear,
    setInterval,
    clearInterval: clear,
    setImmediate,
    clearImmediate,
    queueMicrotask,
  };
})
