// node:events — EventEmitter.
(function () {
  "use strict";
  const kCapture = Symbol("kCapture");
  const errorMonitor = Symbol("events.errorMonitor");
  let defaultMaxListeners = 10;
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
    set: (v) => { defaultMaxListeners = v; },
  });
  EventEmitter.listenerCount = (emitter, type) => emitter.listenerCount(type);
  EventEmitter.once = function (emitter, name) {
    return new Promise((resolve, reject) => {
      const onError = (err) => {
        emitter.removeListener(name, resolver);
        reject(err);
      };
      const resolver = (...args) => {
        if (name !== "error") emitter.removeListener("error", onError);
        resolve(args);
      };
      emitter.once(name, resolver);
      if (name !== "error") emitter.once("error", onError);
    });
  };
  EventEmitter.on = function () {
    throw new Error("zipp: events.on is not supported");
  };
  EventEmitter.setMaxListeners = () => {};
  EventEmitter.getEventListeners = (emitter, name) => emitter.listeners(name);
  return EventEmitter;
})
