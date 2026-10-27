// node:events — EventEmitter, and the module's functions over emitters and
// EventTargets alike: once, on, getEventListeners, getMaxListeners,
// setMaxListeners, addAbortListener. An EventTarget's internals are the
// web_events part's, the globals' own.
//
// The module's functions are derived from Node.js (lib/events.js,
// lib/internal/events/abort_listener.js), which carries this notice:
//
// Copyright Joyent, Inc. and other Node contributors.
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
(function (host) {
  "use strict";
  const kCapture = Symbol("kCapture");
  const errorMonitor = Symbol("events.errorMonitor");
  const kMaxEventTargetListeners = Symbol("events.maxEventTargetListeners");
  const kMaxEventTargetListenersWarned = Symbol("events.maxEventTargetListenersWarned");
  const kWatermarkData = Symbol.for("nodejs.watermarkData");
  const kIsEventTarget = Symbol.for("nodejs.event_target");
  let defaultMaxListeners = 10;
  let webEvents;
  const web = () => (webEvents ||= host.part("web_events"));
  const isEventTarget = (obj) => obj?.constructor?.[kIsEventTarget];

  // ---- errors: Node's codes and messages ----------------------------------
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
    if (typeof v === "function") return " Received function " + v.name;
    if (typeof v === "object") {
      if (v.constructor && "name" in v.constructor) return " Received an instance of " + v.constructor.name;
      return " Received " + host.inspect(v);
    }
    if (typeof v === "string") {
      const s = v.length > 28 ? v.slice(0, 25) + "..." : v;
      return " Received type string (" + (s.indexOf("'") === -1 ? "'" + s + "'" : JSON.stringify(s)) + ")";
    }
    if (typeof v === "bigint") return " Received type bigint (" + v + "n)";
    return " Received type " + typeof v + " (" + (Object.is(v, -0) ? "-0" : String(v)) + ")";
  }
  const errInvalidArgType = (name, expected, actual) =>
    nodeError(TypeError, "ERR_INVALID_ARG_TYPE", 'The "' + name + '" ' + (name.includes(".") ? "property" : "argument") +
      " must be " + expected + "." + received(actual));
  function addNumericalSeparator(val) {
    let res = "";
    let i = val.length;
    const start = val[0] === "-" ? 1 : 0;
    for (; i >= start + 4; i -= 3) res = "_" + val.slice(i - 3, i) + res;
    return val.slice(0, i) + res;
  }
  const errOutOfRange = (name, range, v) =>
    nodeError(RangeError, "ERR_OUT_OF_RANGE", 'The value of "' + name + '" is out of range. It must be ' + range + ". Received " +
      (Number.isInteger(v) && Math.abs(v) > 2 ** 32 ? addNumericalSeparator(String(v)) : Object.is(v, -0) ? "-0" : String(v)));
  class AbortError extends Error {
    constructor(message = "The operation was aborted", options = undefined) {
      if (options !== undefined && typeof options !== "object") throw errInvalidArgType("options", "of type object", options);
      super(message, options);
      this.code = "ABORT_ERR";
      this.name = "AbortError";
    }
  }
  function validateNumber(value, name, min) {
    if (typeof value !== "number") throw errInvalidArgType(name, "of type number", value);
    if (value < min || Number.isNaN(value)) throw errOutOfRange(name, ">= " + min, value);
  }
  function validateInteger(value, name, min) {
    if (typeof value !== "number") throw errInvalidArgType(name, "of type number", value);
    if (!Number.isInteger(value)) throw errOutOfRange(name, "an integer", value);
    if (value < min || value > Number.MAX_SAFE_INTEGER) throw errOutOfRange(name, ">= " + min + " && <= " + Number.MAX_SAFE_INTEGER, value);
  }
  function validateObject(value, name) {
    if (value === null || Array.isArray(value) || typeof value !== "object") throw errInvalidArgType(name, "of type object", value);
  }
  function validateAbortSignal(signal, name) {
    if (signal !== undefined && (signal === null || typeof signal !== "object" || !("aborted" in signal))) {
      throw errInvalidArgType(name, "an instance of AbortSignal", signal);
    }
  }
  function checkListener(listener) {
    if (typeof listener !== "function") {
      throw new TypeError('The "listener" argument must be of type function. Received ' + (listener === null ? "null" : typeof listener));
    }
  }
  function EventEmitter(opts) {
    EventEmitter.init.call(this, opts);
  }
  EventEmitter.init = function (opts) {
    if (this._events === undefined || this._events === Object.getPrototypeOf(this)._events) {
      this._events = Object.create(null);
      this._eventsCount = 0;
    }
    this._maxListeners = this._maxListeners || undefined;
    if (opts && opts.captureRejections) this[kCapture] = true;
  };
  const proto = EventEmitter.prototype;
  proto._events = undefined;
  proto._eventsCount = 0;
  proto._maxListeners = undefined;
  proto.setMaxListeners = function (n) {
    this._maxListeners = n;
    return this;
  };
  proto.getMaxListeners = function () {
    return this._maxListeners === undefined ? defaultMaxListeners : this._maxListeners;
  };
  proto.emit = function (type, ...args) {
    const events = this._events;
    if (type === "error" && events !== undefined && events[errorMonitor] !== undefined) {
      this.emit(errorMonitor, ...args);
    }
    const handler = events !== undefined ? events[type] : undefined;
    if (handler === undefined) {
      if (type === "error") {
        const er = args[0];
        if (er instanceof Error) throw er;
        const err = new Error("Unhandled error." + (er === undefined ? "" : " (" + String(er) + ")"));
        err.context = er;
        throw err;
      }
      return false;
    }
    if (typeof handler === "function") {
      Reflect.apply(handler, this, args);
    } else {
      for (const listener of handler.slice()) Reflect.apply(listener, this, args);
    }
    return true;
  };
  function addListener(target, type, listener, prepend) {
    checkListener(listener);
    let events = target._events;
    if (events === undefined) {
      events = target._events = Object.create(null);
      target._eventsCount = 0;
    } else if (events.newListener !== undefined) {
      target.emit("newListener", type, listener.listener ? listener.listener : listener);
      events = target._events;
    }
    const existing = events[type];
    if (existing === undefined) {
      events[type] = listener;
      ++target._eventsCount;
    } else if (typeof existing === "function") {
      events[type] = prepend ? [listener, existing] : [existing, listener];
    } else if (prepend) {
      existing.unshift(listener);
    } else {
      existing.push(listener);
    }
    return target;
  }
  proto.addListener = function (type, listener) {
    return addListener(this, type, listener, false);
  };
  proto.on = proto.addListener;
  proto.prependListener = function (type, listener) {
    return addListener(this, type, listener, true);
  };
  function onceWrapper(target, type, listener) {
    const state = { fired: false, target, type, listener };
    const wrapped = function (...args) {
      if (!state.fired) {
        state.target.removeListener(state.type, wrapped);
        state.fired = true;
        return Reflect.apply(state.listener, state.target, args);
      }
    };
    wrapped.listener = listener;
    return wrapped;
  }
  proto.once = function (type, listener) {
    checkListener(listener);
    this.on(type, onceWrapper(this, type, listener));
    return this;
  };
  proto.prependOnceListener = function (type, listener) {
    checkListener(listener);
    this.prependListener(type, onceWrapper(this, type, listener));
    return this;
  };
  proto.removeListener = function (type, listener) {
    checkListener(listener);
    const events = this._events;
    if (events === undefined) return this;
    const list = events[type];
    if (list === undefined) return this;
    if (list === listener || list.listener === listener) {
      if (--this._eventsCount === 0) this._events = Object.create(null);
      else delete events[type];
      if (events.removeListener) this.emit("removeListener", type, list.listener || listener);
    } else if (typeof list !== "function") {
      let position = -1;
      for (let i = list.length - 1; i >= 0; i--) {
        if (list[i] === listener || list[i].listener === listener) {
          position = i;
          break;
        }
      }
      if (position < 0) return this;
      list.splice(position, 1);
      if (list.length === 1) events[type] = list[0];
      if (events.removeListener !== undefined) this.emit("removeListener", type, listener);
    }
    return this;
  };
  proto.off = proto.removeListener;
  proto.removeAllListeners = function (type) {
    const events = this._events;
    if (events === undefined) return this;
    if (type === undefined) {
      this._events = Object.create(null);
      this._eventsCount = 0;
    } else if (events[type] !== undefined) {
      if (--this._eventsCount === 0) this._events = Object.create(null);
      else delete events[type];
    }
    return this;
  };
  function listeners(target, type, unwrap) {
    const events = target._events;
    if (events === undefined) return [];
    const l = events[type];
    if (l === undefined) return [];
    if (typeof l === "function") return unwrap ? [l.listener || l] : [l];
    return unwrap ? l.map((x) => x.listener || x) : l.slice();
  }
  proto.listeners = function (type) {
    return listeners(this, type, true);
  };
  proto.rawListeners = function (type) {
    return listeners(this, type, false);
  };
  proto.listenerCount = function (type) {
    const events = this._events;
    if (events === undefined) return 0;
    const l = events[type];
    return l === undefined ? 0 : typeof l === "function" ? 1 : l.length;
  };
  proto.eventNames = function () {
    return this._eventsCount > 0 ? Reflect.ownKeys(this._events) : [];
  };
  EventEmitter.EventEmitter = EventEmitter;
  EventEmitter.errorMonitor = errorMonitor;
  EventEmitter.captureRejectionSymbol = Symbol.for("nodejs.rejection");
  Object.defineProperty(EventEmitter, "defaultMaxListeners", {
    enumerable: true,
    get: () => defaultMaxListeners,
    set: (v) => {
      validateNumber(v, "defaultMaxListeners", 0);
      defaultMaxListeners = v;
    },
  });
  Object.defineProperties(EventEmitter, {
    kMaxEventTargetListeners: { value: kMaxEventTargetListeners, enumerable: false, configurable: false, writable: false },
    kMaxEventTargetListenersWarned: { value: kMaxEventTargetListenersWarned, enumerable: false, configurable: false, writable: false },
  });
  EventEmitter.listenerCount = (emitter, type) => emitter.listenerCount(type);
  EventEmitter.setMaxListeners = function (n = defaultMaxListeners, ...eventTargets) {
    validateNumber(n, "setMaxListeners", 0);
    if (eventTargets.length === 0) {
      defaultMaxListeners = n;
      return;
    }
    for (const target of eventTargets) {
      if (isEventTarget(target)) {
        target[kMaxEventTargetListeners] = n;
        target[kMaxEventTargetListenersWarned] = false;
      } else if (typeof target.setMaxListeners === "function") {
        target.setMaxListeners(n);
      } else {
        throw errInvalidArgType("eventTargets", "an instance of EventEmitter or EventTarget", target);
      }
    }
  };
  EventEmitter.getMaxListeners = function (emitterOrTarget) {
    if (typeof emitterOrTarget?.getMaxListeners === "function") {
      return emitterOrTarget._maxListeners === undefined ? defaultMaxListeners : emitterOrTarget._maxListeners;
    }
    if (typeof emitterOrTarget?.[kMaxEventTargetListeners] === "number") return emitterOrTarget[kMaxEventTargetListeners];
    throw errInvalidArgType("emitter", "an instance of EventEmitter or EventTarget", emitterOrTarget);
  };
  EventEmitter.getEventListeners = function (emitterOrTarget, type) {
    if (typeof emitterOrTarget.listeners === "function") return emitterOrTarget.listeners(type);
    if (isEventTarget(emitterOrTarget)) {
      const root = emitterOrTarget[web().kEvents]?.get(type);
      const out = [];
      let handler = root?.next;
      while (handler?.listener !== undefined) {
        out.push(handler.listener?.deref ? handler.listener.deref() : handler.listener);
        handler = handler.next;
      }
      return out;
    }
    throw errInvalidArgType("emitter", "an instance of EventEmitter or EventTarget", emitterOrTarget);
  };

  // An emitter's own methods, or an EventTarget's.
  function agnosticRemoveListener(emitter, name, listener, flags) {
    if (typeof emitter.removeListener === "function") emitter.removeListener(name, listener);
    else if (typeof emitter.removeEventListener === "function") emitter.removeEventListener(name, listener, flags);
    else throw errInvalidArgType("emitter", "an instance of EventEmitter", emitter);
  }
  function agnosticAddListener(emitter, name, listener, flags) {
    if (typeof emitter.on === "function") {
      if (flags?.once) emitter.once(name, listener);
      else emitter.on(name, listener);
    } else if (typeof emitter.addEventListener === "function") {
      emitter.addEventListener(name, listener, flags);
    } else {
      throw errInvalidArgType("emitter", "an instance of EventEmitter", emitter);
    }
  }
  const onceFlags = () => ({ __proto__: null, once: true, [web().kResistStopPropagation]: true });
  EventEmitter.once = async function once(emitter, name, options = {}) {
    validateObject(options, "options");
    const { signal } = options;
    validateAbortSignal(signal, "options.signal");
    if (signal?.aborted) throw new AbortError(undefined, { cause: signal.reason });
    return new Promise((resolve, reject) => {
      const errorListener = (err) => {
        emitter.removeListener(name, resolver);
        if (signal != null) agnosticRemoveListener(signal, "abort", abortListener);
        reject(err);
      };
      const resolver = (...args) => {
        if (typeof emitter.removeListener === "function") emitter.removeListener("error", errorListener);
        if (signal != null) agnosticRemoveListener(signal, "abort", abortListener);
        resolve(args);
      };
      agnosticAddListener(emitter, name, resolver, typeof emitter.on === "function" ? { once: true } : onceFlags());
      if (name !== "error" && typeof emitter.once === "function") emitter.once("error", errorListener);
      function abortListener() {
        agnosticRemoveListener(emitter, name, resolver);
        agnosticRemoveListener(emitter, "error", errorListener);
        reject(new AbortError(undefined, { cause: signal?.reason }));
      }
      if (signal != null) agnosticAddListener(signal, "abort", abortListener, onceFlags());
    });
  };
  const AsyncIteratorPrototype = Object.getPrototypeOf(Object.getPrototypeOf(async function* () {}).prototype);
  EventEmitter.on = function on(emitter, event, options = {}) {
    validateObject(options, "options");
    const signal = options.signal;
    validateAbortSignal(signal, "options.signal");
    if (signal?.aborted) throw new AbortError(undefined, { cause: signal.reason });
    const highWatermark = options.highWaterMark ?? options.highWatermark ?? Number.MAX_SAFE_INTEGER;
    validateInteger(highWatermark, "options.highWaterMark", 1);
    const lowWatermark = options.lowWaterMark ?? options.lowWatermark ?? 1;
    validateInteger(lowWatermark, "options.lowWaterMark", 1);
    const unconsumedEvents = [];
    const unconsumedPromises = [];
    let paused = false;
    let error = null;
    let finished = false;
    let size = 0;
    const iterator = Object.setPrototypeOf({
      next() {
        if (size) {
          const value = unconsumedEvents.shift();
          size--;
          if (paused && size < lowWatermark) {
            emitter.resume();
            paused = false;
          }
          return Promise.resolve({ value, done: false });
        }
        if (error) {
          const p = Promise.reject(error);
          error = null;
          return p;
        }
        if (finished) return closeHandler();
        return new Promise((resolve, reject) => unconsumedPromises.push({ resolve, reject }));
      },
      return() {
        return closeHandler();
      },
      throw(err) {
        if (!err || !(err instanceof Error)) throw errInvalidArgType("EventEmitter.AsyncIterator", "an instance of Error", err);
        errorHandler(err);
      },
      [Symbol.asyncIterator]() {
        return this;
      },
      [kWatermarkData]: {
        get size() { return size; },
        get low() { return lowWatermark; },
        get high() { return highWatermark; },
        get isPaused() { return paused; },
      },
    }, AsyncIteratorPrototype);
    const added = [];
    const listen = (name, handler) => {
      agnosticAddListener(emitter, name, handler);
      added.push([name, handler]);
    };
    listen(event, (...args) => eventHandler(args));
    if (event !== "error" && typeof emitter.on === "function") listen("error", errorHandler);
    const closeEvents = options?.close;
    if (closeEvents?.length) for (let i = 0; i < closeEvents.length; i++) listen(closeEvents[i], closeHandler);
    const abortListenerDisposable = signal ? EventEmitter.addAbortListener(signal, abortListener) : null;
    return iterator;

    function abortListener() {
      errorHandler(new AbortError(undefined, { cause: signal?.reason }));
    }
    function eventHandler(value) {
      if (unconsumedPromises.length === 0) {
        size++;
        if (!paused && size > highWatermark) {
          paused = true;
          emitter.pause();
        }
        unconsumedEvents.push(value);
      } else {
        unconsumedPromises.shift().resolve({ value, done: false });
      }
    }
    function errorHandler(err) {
      if (unconsumedPromises.length === 0) error = err;
      else unconsumedPromises.shift().reject(err);
      closeHandler();
    }
    function closeHandler() {
      abortListenerDisposable?.[Symbol.dispose]();
      while (added.length > 0) {
        const [name, handler] = added.pop();
        agnosticRemoveListener(emitter, name, handler);
      }
      finished = true;
      const doneResult = { value: undefined, done: true };
      while (unconsumedPromises.length > 0) unconsumedPromises.shift().resolve(doneResult);
      return Promise.resolve(doneResult);
    }
  };
  EventEmitter.addAbortListener = function addAbortListener(signal, listener) {
    if (signal === undefined) throw errInvalidArgType("signal", "an instance of AbortSignal", signal);
    validateAbortSignal(signal, "signal");
    if (typeof listener !== "function") throw errInvalidArgType("listener", "of type function", listener);
    let removeEventListener;
    if (signal.aborted) {
      queueMicrotask(() => listener());
    } else {
      signal.addEventListener("abort", listener, onceFlags());
      removeEventListener = () => {
        signal.removeEventListener("abort", listener);
      };
    }
    return {
      __proto__: null,
      [Symbol.dispose]() {
        removeEventListener?.();
      },
    };
  };
  return EventEmitter;
})
