// Node's web event globals (see mod.rs): Event, CustomEvent, EventTarget,
// AbortController, AbortSignal and DOMException, shaped as Node's are (the
// same accessors, tags, error codes and dispatch order). A listener that
// throws does not stop the others: its error is thrown again on the next
// tick, as an uncaught exception, as Node reports it. The part's other
// exports are the internals node:events and node:stream share.
//
// Derived from Node.js (lib/internal/event_target.js,
// lib/internal/abort_controller.js, lib/internal/per_context/domexception.js),
// which carries this notice:
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
(function (host) {
  "use strict";
  const ObjectDefineProperty = Object.defineProperty;
  const ObjectDefineProperties = Object.defineProperties;
  const ObjectSetPrototypeOf = Object.setPrototypeOf;
  const ObjectPrototypeIsPrototypeOf = Function.prototype.call.bind(Object.prototype.isPrototypeOf);
  const ReflectApply = Reflect.apply;
  const NumberIsInteger = Number.isInteger;
  const SymbolToStringTag = Symbol.toStringTag;
  const inspectCustom = Symbol.for("nodejs.util.inspect.custom");
  const kEnumerableProperty = { __proto__: null, enumerable: true };
  const kEmptyObject = Object.freeze({ __proto__: null });

  const EventEmitter = host.cjs().builtin("events");
  const { kMaxEventTargetListeners, kMaxEventTargetListenersWarned } = EventEmitter;
  let proc, timers;
  const process = () => (proc ||= host.part("process").process);
  const timer = () => (timers ||= host.part("timers"));
  function now() {
    const t = host.hrtime();
    return t[0] * 1e3 + t[1] / 1e6;
  }
  function show(value, options, inspect) {
    return typeof inspect === "function" ? inspect(value, options) : host.inspect(value);
  }
  function nestedOptions(options, depth) {
    return Object.assign({}, options, { depth });
  }

  // ---- errors: Node's codes and messages ----------------------------------
  function nodeError(Base, code, message) {
    const err = new Base(message);
    ObjectDefineProperty(err, "toString", {
      value() { return this.name + " [" + code + "]: " + this.message; },
      writable: true,
      configurable: true,
    });
    err.code = code;
    return err;
  }
  function codedTypeError(message) {
    const err = new TypeError(message);
    err.code = "ERR_INVALID_ARG_TYPE";
    return err;
  }
  function received(v) {
    if (v == null) return " Received " + v;
    switch (typeof v) {
      case "function": return " Received function " + v.name;
      case "object":
        if (v.constructor && "name" in v.constructor) return " Received an instance of " + v.constructor.name;
        return " Received " + host.inspect(v);
      case "string": {
        const s = v.length > 28 ? v.slice(0, 25) + "..." : v;
        return " Received type string (" + (s.indexOf("'") === -1 ? "'" + s + "'" : JSON.stringify(s)) + ")";
      }
      case "number": return " Received type number (" + (Object.is(v, -0) ? "-0" : String(v)) + ")";
      case "bigint": return " Received type bigint (" + v + "n)";
      default: return " Received type " + typeof v + " (" + String(v) + ")";
    }
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
      (NumberIsInteger(v) && Math.abs(v) > 2 ** 32 ? addNumericalSeparator(String(v)) : Object.is(v, -0) ? "-0" : String(v)));
  const errMissingArgs = (...names) =>
    nodeError(TypeError, "ERR_MISSING_ARGS", "The " + (names.length === 1 ? '"' + names[0] + '" argument' : '"' + names[0] + '" and "' + names[1] + '" arguments') + " must be specified");
  const errInvalidThis = (type) => nodeError(TypeError, "ERR_INVALID_THIS", 'Value of "this" must be of type ' + type);
  const errIllegalConstructor = () => nodeError(TypeError, "ERR_ILLEGAL_CONSTRUCTOR", "Illegal constructor");
  const errEventRecursion = (type) => nodeError(Error, "ERR_EVENT_RECURSION", 'The event "' + type + '" is already being dispatched');
  function validateObject(value, name, allowArrayAndFunction) {
    if (value === null || (!allowArrayAndFunction && Array.isArray(value)) ||
        (typeof value !== "object" && (!allowArrayAndFunction || typeof value !== "function"))) {
      throw errInvalidArgType(name, "of type object", value);
    }
  }
  function validateAbortSignal(signal, name) {
    if (signal !== undefined && (signal === null || typeof signal !== "object" || !("aborted" in signal))) {
      throw errInvalidArgType(name, "an instance of AbortSignal", signal);
    }
  }
  function toDOMString(v) {
    if (typeof v === "symbol") throw codedTypeError("Value is a Symbol and cannot be converted to a string.");
    return String(v);
  }

  // ---- DOMException ---------------------------------------------------------
  // An Error (the constructor returns one, its prototype set), so it has
  // Error's internal slot, `stack` and `instanceof Error`.
  const internalsMap = new WeakMap();
  const nameToCodeMap = new Map();
  const disusedNamesSet = new Set(["DOMStringSizeError", "NoDataAllowedError", "ValidationError"]);
  function domInternals(self) {
    const internals = internalsMap.get(self);
    if (internals === undefined) {
      const err = new TypeError();
      ObjectDefineProperties(err, {
        message: { value: 'Value of "this" must be of DOMException', enumerable: false, writable: true, configurable: true },
        toString: { value() { return this.name + " [ERR_INVALID_THIS]: " + this.message; }, enumerable: false, writable: true, configurable: true },
      });
      err.code = "ERR_INVALID_THIS";
      throw err;
    }
    return internals;
  }
  class DOMException {
    constructor(message = "", options = "Error") {
      const self = new Error();
      ObjectSetPrototypeOf(self, new.target.prototype);
      if (options && typeof options === "object") {
        const { name } = options;
        internalsMap.set(self, { message: `${message}`, name: `${name}` });
        if ("cause" in options) {
          ObjectDefineProperty(self, "cause", { value: options.cause, configurable: true, writable: true, enumerable: false });
        }
      } else {
        internalsMap.set(self, { message: `${message}`, name: `${options}` });
      }
      return self;
    }
    get name() {
      return domInternals(this).name;
    }
    get message() {
      return domInternals(this).message;
    }
    get code() {
      const { name } = domInternals(this);
      if (disusedNamesSet.has(name)) return 0;
      const code = nameToCodeMap.get(name);
      return code === undefined ? 0 : code;
    }
  }
  ObjectSetPrototypeOf(DOMException.prototype, Error.prototype);
  ObjectDefineProperties(DOMException.prototype, {
    [SymbolToStringTag]: { configurable: true, value: "DOMException" },
    name: { enumerable: true, configurable: true },
    message: { enumerable: true, configurable: true },
    code: { enumerable: true, configurable: true },
  });
  for (const [name, codeName, value] of [
    ["IndexSizeError", "INDEX_SIZE_ERR", 1],
    ["DOMStringSizeError", "DOMSTRING_SIZE_ERR", 2],
    ["HierarchyRequestError", "HIERARCHY_REQUEST_ERR", 3],
    ["WrongDocumentError", "WRONG_DOCUMENT_ERR", 4],
    ["InvalidCharacterError", "INVALID_CHARACTER_ERR", 5],
    ["NoDataAllowedError", "NO_DATA_ALLOWED_ERR", 6],
    ["NoModificationAllowedError", "NO_MODIFICATION_ALLOWED_ERR", 7],
    ["NotFoundError", "NOT_FOUND_ERR", 8],
    ["NotSupportedError", "NOT_SUPPORTED_ERR", 9],
    ["InUseAttributeError", "INUSE_ATTRIBUTE_ERR", 10],
    ["InvalidStateError", "INVALID_STATE_ERR", 11],
    ["SyntaxError", "SYNTAX_ERR", 12],
    ["InvalidModificationError", "INVALID_MODIFICATION_ERR", 13],
    ["NamespaceError", "NAMESPACE_ERR", 14],
    ["InvalidAccessError", "INVALID_ACCESS_ERR", 15],
    ["ValidationError", "VALIDATION_ERR", 16],
    ["TypeMismatchError", "TYPE_MISMATCH_ERR", 17],
    ["SecurityError", "SECURITY_ERR", 18],
    ["NetworkError", "NETWORK_ERR", 19],
    ["AbortError", "ABORT_ERR", 20],
    ["URLMismatchError", "URL_MISMATCH_ERR", 21],
    ["QuotaExceededError", "QUOTA_EXCEEDED_ERR", 22],
    ["TimeoutError", "TIMEOUT_ERR", 23],
    ["InvalidNodeTypeError", "INVALID_NODE_TYPE_ERR", 24],
    ["DataCloneError", "DATA_CLONE_ERR", 25],
  ]) {
    const desc = { __proto__: null, enumerable: true, value };
    ObjectDefineProperty(DOMException, codeName, desc);
    ObjectDefineProperty(DOMException.prototype, codeName, desc);
    nameToCodeMap.set(name, value);
  }

  // ---- Event, CustomEvent -----------------------------------------------------
  const kIsEventTarget = Symbol.for("nodejs.event_target");
  const kEvents = Symbol("kEvents");
  const kIsBeingDispatched = Symbol("kIsBeingDispatched");
  const kStop = Symbol("kStop");
  const kTarget = Symbol("kTarget");
  const kHandlers = Symbol("kHandlers");
  const kWeakHandler = Symbol("kWeak");
  const kResistStopPropagation = Symbol("kResistStopPropagation");
  const kHybridDispatch = Symbol.for("nodejs.internal.kHybridDispatch");
  const kRemoveWeakListenerHelper = Symbol("kRemoveWeakListenerHelper");
  const kCreateEvent = Symbol("kCreateEvent");
  const kNewListener = Symbol("kNewListener");
  const kRemoveListener = Symbol("kRemoveListener");
  const kIsNodeStyleListener = Symbol("kIsNodeStyleListener");
  const kTrustEvent = Symbol("kTrustEvent");
  const kType = Symbol("type");
  const kDetail = Symbol("detail");
  const kInPassiveListener = Symbol("kInPassiveListener");

  const isTrustedSet = new WeakSet();
  const isTrusted = Object.getOwnPropertyDescriptor({
    get isTrusted() {
      return isTrustedSet.has(this);
    },
  }, "isTrusted").get;
  function isEvent(value) {
    return typeof value?.[kType] === "string";
  }

  class Event {
    #cancelable = false;
    #bubbles = false;
    #composed = false;
    #defaultPrevented = false;
    #timestamp = now();
    #propagationStopped = false;
    constructor(type, options = undefined) {
      if (arguments.length === 0) throw errMissingArgs("type");
      if (options != null) validateObject(options, "options");
      this.#bubbles = !!options?.bubbles;
      this.#cancelable = !!options?.cancelable;
      this.#composed = !!options?.composed;
      this[kType] = `${type}`;
      if (options?.[kTrustEvent]) isTrustedSet.add(this);
      this[kTarget] = null;
      this[kIsBeingDispatched] = false;
      this[kInPassiveListener] = false;
    }
    initEvent(type, bubbles = false, cancelable = false) {
      if (arguments.length === 0) throw errMissingArgs("type");
      if (this[kIsBeingDispatched]) return;
      this[kType] = `${type}`;
      this.#bubbles = !!bubbles;
      this.#cancelable = !!cancelable;
    }
    [inspectCustom](depth, options, inspect) {
      if (!isEvent(this)) throw errInvalidThis("Event");
      const name = this.constructor.name;
      if (depth < 0) return name;
      return name + " " + show({
        type: this[kType],
        defaultPrevented: this.#defaultPrevented,
        cancelable: this.#cancelable,
        timeStamp: this.#timestamp,
      }, nestedOptions(options, NumberIsInteger(options?.depth) ? options.depth - 1 : options?.depth), inspect);
    }
    stopImmediatePropagation() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      this.stopPropagation();
      this[kStop] = true;
    }
    preventDefault() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      if (!this.#cancelable || this[kInPassiveListener]) return;
      this.#defaultPrevented = true;
    }
    get target() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this[kTarget];
    }
    get currentTarget() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this[kIsBeingDispatched] ? this[kTarget] : null;
    }
    get srcElement() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this[kTarget];
    }
    get type() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this[kType];
    }
    get cancelable() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this.#cancelable;
    }
    get defaultPrevented() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this.#cancelable && this.#defaultPrevented;
    }
    get timeStamp() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this.#timestamp;
    }
    composedPath() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this[kIsBeingDispatched] ? [this[kTarget]] : [];
    }
    get returnValue() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return !this.#cancelable || !this.#defaultPrevented;
    }
    set returnValue(value) {
      if (!isEvent(this)) throw errInvalidThis("Event");
      if (!value) {
        if (!this.#cancelable || this[kInPassiveListener]) return;
        this.#defaultPrevented = true;
      }
    }
    get bubbles() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this.#bubbles;
    }
    get composed() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this.#composed;
    }
    get eventPhase() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this[kIsBeingDispatched] ? Event.AT_TARGET : Event.NONE;
    }
    get cancelBubble() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      return this.#propagationStopped;
    }
    set cancelBubble(value) {
      if (!isEvent(this)) throw errInvalidThis("Event");
      if (value) this.#propagationStopped = true;
    }
    stopPropagation() {
      if (!isEvent(this)) throw errInvalidThis("Event");
      this.#propagationStopped = true;
    }
  }
  ObjectDefineProperties(Event.prototype, {
    [SymbolToStringTag]: { writable: false, enumerable: false, configurable: true, value: "Event" },
    initEvent: kEnumerableProperty,
    stopImmediatePropagation: kEnumerableProperty,
    preventDefault: kEnumerableProperty,
    target: kEnumerableProperty,
    currentTarget: kEnumerableProperty,
    srcElement: kEnumerableProperty,
    type: kEnumerableProperty,
    cancelable: kEnumerableProperty,
    defaultPrevented: kEnumerableProperty,
    timeStamp: kEnumerableProperty,
    composedPath: kEnumerableProperty,
    returnValue: kEnumerableProperty,
    bubbles: kEnumerableProperty,
    composed: kEnumerableProperty,
    eventPhase: kEnumerableProperty,
    cancelBubble: kEnumerableProperty,
    stopPropagation: kEnumerableProperty,
    isTrusted: { configurable: false, enumerable: true, get: isTrusted },
  });
  ["NONE", "CAPTURING_PHASE", "AT_TARGET", "BUBBLING_PHASE"].forEach((name, value) => {
    ObjectDefineProperty(Event, name, { writable: false, configurable: false, enumerable: true, value });
  });

  function isCustomEvent(value) {
    return isEvent(value) && value?.[kDetail] !== undefined;
  }
  class CustomEvent extends Event {
    constructor(type, options = kEmptyObject) {
      if (arguments.length === 0) throw errMissingArgs("type");
      super(type, options);
      this[kDetail] = options?.detail ?? null;
    }
    get detail() {
      if (!isCustomEvent(this)) throw errInvalidThis("CustomEvent");
      return this[kDetail];
    }
  }
  ObjectDefineProperties(CustomEvent.prototype, {
    [SymbolToStringTag]: { writable: false, enumerable: false, configurable: true, value: "CustomEvent" },
    detail: kEnumerableProperty,
  });

  // ---- listeners ----------------------------------------------------------------
  // A target's listeners for a type are a linked list from a root record;
  // a weak one (an `{ signal }` option's abort listener) lives as long as
  // the target it removes a listener from.
  let weakListenersState = null;
  let objectToWeakListenerMap = null;
  function weakListeners() {
    weakListenersState ??= new FinalizationRegistry(
      ({ eventTarget, listener, eventType }) => eventTarget.deref()?.[kRemoveWeakListenerHelper](eventType, listener),
    );
    objectToWeakListenerMap ??= new WeakMap();
    return { registry: weakListenersState, map: objectToWeakListenerMap };
  }
  const kFlagOnce = 1 << 0;
  const kFlagCapture = 1 << 1;
  const kFlagPassive = 1 << 2;
  const kFlagNodeStyle = 1 << 3;
  const kFlagWeak = 1 << 4;
  const kFlagRemoved = 1 << 5;
  const kFlagResistStopPropagation = 1 << 6;
  class Listener {
    constructor(eventTarget, eventType, previous, listener, once, capture, passive, isNodeStyleListener, weak, resistStopPropagation) {
      this.next = undefined;
      if (previous !== undefined) previous.next = this;
      this.previous = previous;
      this.listener = listener;
      let flags = 0;
      if (once) flags |= kFlagOnce;
      if (capture) flags |= kFlagCapture;
      if (passive) flags |= kFlagPassive;
      if (isNodeStyleListener) flags |= kFlagNodeStyle;
      if (weak) flags |= kFlagWeak;
      if (resistStopPropagation) flags |= kFlagResistStopPropagation;
      this.flags = flags;
      this.removed = false;
      if (this.weak) {
        this.callback = new WeakRef(listener);
        const { registry, map } = weakListeners();
        registry.register(listener, { __proto__: null, eventTarget: new WeakRef(eventTarget), listener: this, eventType }, this);
        const held = map.get(weak);
        if (held) held.add(listener);
        else map.set(weak, new Set([listener]));
        this.listener = this.callback;
      } else if (typeof listener === "function") {
        this.callback = listener;
        this.listener = listener;
      } else {
        this.callback = async (...args) => {
          if (listener.handleEvent) await ReflectApply(listener.handleEvent, listener, args);
        };
        this.listener = listener;
      }
    }
    get once() { return Boolean(this.flags & kFlagOnce); }
    get capture() { return Boolean(this.flags & kFlagCapture); }
    get passive() { return Boolean(this.flags & kFlagPassive); }
    get isNodeStyleListener() { return Boolean(this.flags & kFlagNodeStyle); }
    get weak() { return Boolean(this.flags & kFlagWeak); }
    get resistStopPropagation() { return Boolean(this.flags & kFlagResistStopPropagation); }
    get removed() { return Boolean(this.flags & kFlagRemoved); }
    set removed(value) {
      if (value) this.flags |= kFlagRemoved;
      else this.flags &= ~kFlagRemoved;
    }
    same(listener, capture) {
      const myListener = this.weak ? this.listener.deref() : this.listener;
      return myListener === listener && this.capture === capture;
    }
    remove() {
      if (this.previous !== undefined) this.previous.next = this.next;
      if (this.next !== undefined) this.next.previous = this.previous;
      this.removed = true;
      if (this.weak) weakListeners().registry.unregister(this);
    }
  }

  // ---- EventTarget ----------------------------------------------------------------
  function initEventTarget(self) {
    self[kEvents] = undefined;
    self[kMaxEventTargetListeners] = EventEmitter.defaultMaxListeners;
    self[kMaxEventTargetListenersWarned] = false;
    self[kHandlers] = undefined;
  }
  function getEventsMap(self) {
    if (self[kEvents] === undefined) self[kEvents] = new Map();
    return self[kEvents];
  }
  function isEventTarget(obj) {
    return obj?.constructor?.[kIsEventTarget];
  }
  function validateEventListener(listener) {
    if (typeof listener === "function" || typeof listener?.handleEvent === "function") return true;
    if (listener == null) return false;
    if (typeof listener === "object") return true;
    throw errInvalidArgType("listener", "an instance of EventListener", listener);
  }
  function validateEventListenerOptions(options) {
    if (typeof options === "boolean") return { capture: options };
    if (options === null) return kEmptyObject;
    validateObject(options, "options", true);
    return {
      once: Boolean(options.once),
      capture: Boolean(options.capture),
      passive: Boolean(options.passive),
      signal: options.signal,
      weak: options[kWeakHandler],
      resistStopPropagation: options[kResistStopPropagation] ?? false,
      isNodeStyleListener: Boolean(options[kIsNodeStyleListener]),
    };
  }
  function emitUncaughtException(err) {
    process().nextTick(() => { throw err; });
  }
  function addCatch(promise) {
    const then = promise.then;
    if (typeof then === "function") {
      ReflectApply(then, promise, [undefined, function (err) { emitUncaughtException(err); }]);
    }
  }

  class EventTarget {
    static [kIsEventTarget] = true;
    constructor() {
      initEventTarget(this);
    }
    [kNewListener](size, type, listener, once, capture, passive, weak) {
      if (this[kMaxEventTargetListeners] > 0 && size > this[kMaxEventTargetListeners] && !this[kMaxEventTargetListenersWarned]) {
        this[kMaxEventTargetListenersWarned] = true;
        const w = new Error("Possible EventTarget memory leak detected. " + size + " " + String(type) + " listeners added to " +
          this.constructor.name + ". MaxListeners is " + this[kMaxEventTargetListeners] + ". Use events.setMaxListeners() to increase limit");
        w.name = "MaxListenersExceededWarning";
        w.target = this;
        w.type = type;
        w.count = size;
        process().emitWarning(w);
      }
    }
    [kRemoveListener](size, type, listener, capture) {}
    addEventListener(type, listener, options = kEmptyObject) {
      if (!isEventTarget(this)) throw errInvalidThis("EventTarget");
      if (arguments.length < 2) throw errMissingArgs("type", "listener");
      let once = false;
      let capture = false;
      let passive = false;
      let isNodeStyleListener = false;
      let weak = false;
      let resistStopPropagation = false;
      if (options !== kEmptyObject) {
        options = validateEventListenerOptions(options);
        once = options.once;
        capture = options.capture;
        passive = options.passive;
        isNodeStyleListener = options.isNodeStyleListener;
        weak = options.weak;
        resistStopPropagation = options.resistStopPropagation;
        const signal = options.signal;
        validateAbortSignal(signal, "options.signal");
        if (signal) {
          if (signal.aborted) return;
          signal.addEventListener("abort", () => {
            this.removeEventListener(type, listener, options);
          }, { __proto__: null, once: true, [kWeakHandler]: this, [kResistStopPropagation]: true });
        }
      }
      if (!validateEventListener(listener)) {
        const w = new Error("addEventListener called with " + listener + " which has no effect.");
        w.name = "AddEventListenerArgumentTypeWarning";
        w.target = this;
        w.type = type;
        process().emitWarning(w);
        return;
      }
      type = toDOMString(type);
      const eventsMap = getEventsMap(this);
      let root = eventsMap.get(type);
      if (root === undefined) {
        root = { size: 1, next: undefined, resistStopPropagation: Boolean(resistStopPropagation) };
        new Listener(this, type, root, listener, once, capture, passive, isNodeStyleListener, weak, resistStopPropagation);
        this[kNewListener](root.size, type, listener, once, capture, passive, weak);
        eventsMap.set(type, root);
        return;
      }
      let handler = root.next;
      let previous = root;
      while (handler !== undefined && !handler.same(listener, capture)) {
        previous = handler;
        handler = handler.next;
      }
      if (handler !== undefined) return;
      new Listener(this, type, previous, listener, once, capture, passive, isNodeStyleListener, weak, resistStopPropagation);
      root.size++;
      root.resistStopPropagation ||= Boolean(resistStopPropagation);
      this[kNewListener](root.size, type, listener, once, capture, passive, weak);
    }
    removeEventListener(type, listener, options = kEmptyObject) {
      if (!isEventTarget(this)) throw errInvalidThis("EventTarget");
      if (arguments.length < 2) throw errMissingArgs("type", "listener");
      if (!validateEventListener(listener)) return;
      type = toDOMString(type);
      const capture = options?.capture === true;
      if (this[kEvents] === undefined) return;
      const root = this[kEvents].get(type);
      if (root === undefined || root.next === undefined) return;
      let handler = root.next;
      while (handler !== undefined) {
        if (handler.same(listener, capture)) {
          handler.remove();
          root.size--;
          if (root.size === 0) this[kEvents].delete(type);
          this[kRemoveListener](root.size, type, listener, capture);
          break;
        }
        handler = handler.next;
      }
    }
    [kRemoveWeakListenerHelper](type, listener) {
      if (this[kEvents] === undefined) return;
      const root = this[kEvents].get(type);
      if (root === undefined || root.next === undefined) return;
      const capture = listener.capture === true;
      let handler = root.next;
      while (handler !== undefined) {
        if (handler === listener) {
          handler.remove();
          root.size--;
          if (root.size === 0) this[kEvents].delete(type);
          this[kRemoveListener](root.size, type, undefined, capture);
          break;
        }
        handler = handler.next;
      }
    }
    dispatchEvent(event) {
      if (!isEventTarget(this)) throw errInvalidThis("EventTarget");
      if (arguments.length < 1) throw errMissingArgs("event");
      if (!(event instanceof Event)) throw errInvalidArgType("event", "an instance of Event", event);
      if (event[kIsBeingDispatched]) throw errEventRecursion(event.type);
      this[kHybridDispatch](event, event.type, event);
      return event.defaultPrevented !== true;
    }
    [kHybridDispatch](nodeValue, type, event) {
      const createEvent = () => {
        if (event === undefined) {
          event = this[kCreateEvent](nodeValue, type);
          event[kTarget] = this;
          event[kIsBeingDispatched] = true;
        }
        return event;
      };
      if (event !== undefined) {
        event[kTarget] = this;
        event[kIsBeingDispatched] = true;
      }
      const root = this[kEvents]?.get(type);
      if (root === undefined || root.next === undefined) {
        if (event !== undefined) event[kIsBeingDispatched] = false;
        return true;
      }
      let handler = root.next;
      let next;
      const iterationCondition = () => {
        if (handler === undefined) return false;
        return root.resistStopPropagation || handler.passive || event?.[kStop] !== true;
      };
      while (iterationCondition()) {
        next = handler.next;
        if (handler.removed || (event?.[kStop] === true && !handler.resistStopPropagation)) {
          handler = next;
          continue;
        }
        if (handler.once) {
          handler.remove();
          root.size--;
          const { listener, capture } = handler;
          this[kRemoveListener](root.size, type, listener, capture);
        }
        let arg;
        try {
          arg = handler.isNodeStyleListener ? nodeValue : createEvent();
          const callback = handler.weak ? handler.callback.deref() : handler.callback;
          let result;
          if (callback) {
            if (handler.passive && !handler.isNodeStyleListener) arg[kInPassiveListener] = true;
            result = ReflectApply(callback, this, [arg]);
            if (!handler.isNodeStyleListener) arg[kIsBeingDispatched] = false;
          }
          if (result !== undefined && result !== null) addCatch(result);
        } catch (err) {
          emitUncaughtException(err);
        } finally {
          if (arg?.[kInPassiveListener]) arg[kInPassiveListener] = false;
        }
        handler = next;
      }
      if (event !== undefined) event[kIsBeingDispatched] = false;
    }
    [kCreateEvent](nodeValue, type) {
      return new CustomEvent(type, { detail: nodeValue });
    }
    [inspectCustom](depth, options, inspect) {
      const name = this.constructor.name;
      if (depth < 0) return name;
      return name + " " + show({}, nestedOptions(options, NumberIsInteger(options?.depth) ? options.depth - 1 : options?.depth), inspect);
    }
  }
  ObjectDefineProperties(EventTarget.prototype, {
    addEventListener: kEnumerableProperty,
    removeEventListener: kEnumerableProperty,
    dispatchEvent: kEnumerableProperty,
    [SymbolToStringTag]: { writable: false, enumerable: false, configurable: true, value: "EventTarget" },
  });

  // `on<event>` attributes: dispatched in the order they were first set.
  function makeEventHandler(handler) {
    function eventHandler(...args) {
      if (typeof eventHandler.handler !== "function") return;
      return ReflectApply(eventHandler.handler, this, args);
    }
    eventHandler.handler = handler;
    return eventHandler;
  }
  function validateThisInternalField(object, fieldKey, className) {
    if (typeof object !== "object" || !(fieldKey in object)) throw errInvalidThis(className);
  }
  function defineEventHandler(emitter, name, event = name) {
    const propName = `on${name}`;
    function get() {
      validateThisInternalField(this, kHandlers, "EventTarget");
      return this[kHandlers]?.get(event)?.handler ?? null;
    }
    ObjectDefineProperty(get, "name", { value: `get ${propName}` });
    function set(value) {
      validateThisInternalField(this, kHandlers, "EventTarget");
      if (this[kHandlers] === undefined) this[kHandlers] = new Map();
      let wrappedHandler = this[kHandlers].get(event);
      if (wrappedHandler) {
        const eventsMap = getEventsMap(this);
        if (typeof wrappedHandler.handler === "function") {
          const root = eventsMap.get(event);
          if (root !== undefined) {
            root.size--;
            this[kRemoveListener](root.size, event, wrappedHandler.handler, false);
          }
        }
        wrappedHandler.handler = value;
        if (typeof wrappedHandler.handler === "function") {
          const root = eventsMap.get(event);
          if (root !== undefined) {
            root.size++;
            this[kNewListener](root.size, event, value, false, false, false, false);
          } else {
            this.addEventListener(event, wrappedHandler);
          }
        }
      } else {
        wrappedHandler = makeEventHandler(value);
        this.addEventListener(event, wrappedHandler);
      }
      this[kHandlers].set(event, wrappedHandler);
    }
    ObjectDefineProperty(set, "name", { value: `set ${propName}` });
    ObjectDefineProperty(emitter, propName, { get, set, configurable: true, enumerable: true });
  }

  // ---- AbortSignal, AbortController -----------------------------------------------
  // A timeout's timer and a composite signal's sources hold their signals
  // weakly; a signal with an abort listener that a timer or a source may
  // still abort is kept (gcPersistentSignals) until it aborts or loses its
  // listeners.
  const kDontThrowSymbol = Symbol("kDontThrowSymbol");
  const kAborted = Symbol("kAborted");
  const kReason = Symbol("kReason");
  const kTimeout = Symbol("kTimeout");
  const kComposite = Symbol("kComposite");
  const kFollowing = Symbol("kFollowing");
  const kResultSignalWeakRef = Symbol("kResultSignalWeakRef");
  const kSourceSignals = Symbol("kSourceSignals");
  const kDependantSignals = Symbol("kDependantSignals");
  const gcPersistentSignals = new Set();
  const clearTimeoutRegistry = new FinalizationRegistry((t) => timer().clearTimeout(t));
  const sourceSignalsCleanupRegistry = new FinalizationRegistry(({ sourceSignalRef, composedSignalRef }) => {
    const composedSignal = composedSignalRef.deref();
    if (composedSignal !== undefined) {
      composedSignal[kSourceSignals].delete(sourceSignalRef);
      if (composedSignal[kSourceSignals].size === 0) gcPersistentSignals.delete(composedSignal);
    }
  });
  const dependantSignalsCleanupRegistry = new FinalizationRegistry(({ sourceSignalRef, dependantSignalRef, sourceSignalsCleanupToken }) => {
    sourceSignalsCleanupRegistry.unregister(sourceSignalsCleanupToken);
    const sourceSignal = sourceSignalRef.deref();
    if (sourceSignal === undefined) return;
    sourceSignal[kDependantSignals].delete(dependantSignalRef);
  });

  function customInspect(self, obj, depth, options, inspect) {
    if (depth < 0) return self;
    return self.constructor.name + " " + show(obj, nestedOptions(options, options?.depth == null ? options?.depth : options.depth - 1), inspect);
  }
  function validateThisAbortSignal(obj) {
    if (obj?.[kAborted] === undefined) throw errInvalidThis("AbortSignal");
  }
  function validateUint32(value, name) {
    if (typeof value !== "number") throw errInvalidArgType(name, "of type number", value);
    if (!NumberIsInteger(value)) throw errOutOfRange(name, "an integer", value);
    if (value < 0 || value > 4294967295) throw errOutOfRange(name, ">= 0 && <= 4294967295", value);
  }
  // Web IDL's sequence<AbortSignal>.
  function toSignals(V) {
    const fail = () => codedTypeError("signals cannot be converted to sequence.");
    if (V === null || (typeof V !== "object" && typeof V !== "function")) throw fail();
    const method = V[Symbol.iterator];
    if (typeof method !== "function") throw fail();
    const iterator = ReflectApply(method, V, []);
    const nextMethod = iterator?.next;
    if (typeof nextMethod !== "function") throw fail();
    const signals = [];
    while (true) {
      const next = ReflectApply(nextMethod, iterator, []);
      if (next === null || (typeof next !== "object" && typeof next !== "function")) throw fail();
      if (next.done) break;
      if (!ObjectPrototypeIsPrototypeOf(AbortSignal.prototype, next.value)) {
        throw codedTypeError("signals[" + signals.length + "] is not of type AbortSignal.");
      }
      signals.push(next.value);
    }
    return signals;
  }
  function refreshCompositeSignal(signal) {
    if (!signal[kComposite] || signal[kAborted] || !signal[kSourceSignals]?.size) return;
    for (const sourceSignalWeakRef of signal[kSourceSignals]) {
      const sourceSignal = sourceSignalWeakRef.deref();
      if (sourceSignal === undefined) {
        signal[kSourceSignals].delete(sourceSignalWeakRef);
        continue;
      }
      if (sourceSignal.aborted) {
        abortSignal(signal, sourceSignal.reason);
        return;
      }
    }
  }
  function followCompositeSignal(signal) {
    if (signal[kFollowing] || signal[kAborted] || !signal[kSourceSignals]?.size) return;
    const resultSignalWeakRef = signal[kResultSignalWeakRef] ??= new WeakRef(signal);
    for (const sourceSignalWeakRef of signal[kSourceSignals]) {
      const sourceSignal = sourceSignalWeakRef.deref();
      if (sourceSignal === undefined) {
        signal[kSourceSignals].delete(sourceSignalWeakRef);
        continue;
      }
      if (sourceSignal.aborted) {
        abortSignal(signal, sourceSignal.reason);
        return;
      }
      sourceSignal[kDependantSignals] ??= new Set();
      sourceSignal[kDependantSignals].add(resultSignalWeakRef);
      dependantSignalsCleanupRegistry.register(signal, {
        sourceSignalRef: sourceSignalWeakRef,
        dependantSignalRef: resultSignalWeakRef,
        sourceSignalsCleanupToken: sourceSignalWeakRef,
      });
      sourceSignalsCleanupRegistry.register(sourceSignal, {
        sourceSignalRef: sourceSignalWeakRef,
        composedSignalRef: resultSignalWeakRef,
      }, sourceSignalWeakRef);
    }
    signal[kFollowing] = true;
  }
  function setWeakAbortSignalTimeout(weakRef, delay) {
    const timeout = timer().setTimeout(() => {
      const signal = weakRef.deref();
      if (signal !== undefined) {
        clearTimeoutRegistry.unregister(signal);
        gcPersistentSignals.delete(signal);
        abortSignal(signal, new DOMException("The operation was aborted due to timeout", "TimeoutError"));
      }
    }, delay);
    timeout.unref();
    return timeout;
  }

  class AbortSignal extends EventTarget {
    constructor(dontThrowSymbol = undefined, init = kEmptyObject) {
      if (dontThrowSymbol !== kDontThrowSymbol) throw errIllegalConstructor();
      super();
      this[kMaxEventTargetListeners] = 0;
      const { aborted = false, reason = undefined, composite = false } = init;
      this[kAborted] = aborted;
      this[kReason] = reason;
      this[kComposite] = composite;
    }
    get aborted() {
      validateThisAbortSignal(this);
      refreshCompositeSignal(this);
      return !!this[kAborted];
    }
    get reason() {
      validateThisAbortSignal(this);
      refreshCompositeSignal(this);
      return this[kReason];
    }
    throwIfAborted() {
      validateThisAbortSignal(this);
      refreshCompositeSignal(this);
      if (this[kAborted]) throw this[kReason];
    }
    [inspectCustom](depth, options, inspect) {
      return customInspect(this, { aborted: this.aborted }, depth, options, inspect);
    }
    static abort(reason = new DOMException("This operation was aborted", "AbortError")) {
      return new AbortSignal(kDontThrowSymbol, { aborted: true, reason });
    }
    static timeout(delay) {
      validateUint32(delay, "delay");
      const signal = new AbortSignal(kDontThrowSymbol);
      signal[kTimeout] = true;
      clearTimeoutRegistry.register(signal, setWeakAbortSignalTimeout(new WeakRef(signal), delay), signal);
      return signal;
    }
    static any(signals) {
      const signalsArray = toSignals(signals);
      const resultSignal = new AbortSignal(kDontThrowSymbol, { composite: true });
      if (!signalsArray.length) return resultSignal;
      resultSignal[kSourceSignals] = new Set();
      let hasTimeoutSignals = false;
      for (let i = 0; i < signalsArray.length; i++) {
        const signal = signalsArray[i];
        if (signal[kTimeout]) {
          hasTimeoutSignals = true;
          gcPersistentSignals.add(signal);
        }
        if (signal.aborted) {
          abortSignal(resultSignal, signal.reason);
          return resultSignal;
        }
        if (!signal[kComposite]) {
          resultSignal[kSourceSignals].add(new WeakRef(signal));
        } else if (!signal[kSourceSignals]) {
          continue;
        } else {
          refreshCompositeSignal(signal);
          if (signal.aborted) {
            abortSignal(resultSignal, signal.reason);
            return resultSignal;
          }
          for (const sourceSignalWeakRef of signal[kSourceSignals]) {
            const sourceSignal = sourceSignalWeakRef.deref();
            if (!sourceSignal) continue;
            if (sourceSignal.aborted) {
              abortSignal(resultSignal, sourceSignal.reason);
              return resultSignal;
            }
            if (resultSignal[kSourceSignals].has(sourceSignalWeakRef)) continue;
            resultSignal[kSourceSignals].add(sourceSignalWeakRef);
          }
        }
      }
      if (hasTimeoutSignals && resultSignal[kSourceSignals].size > 0) resultSignal[kTimeout] = true;
      return resultSignal;
    }
    [kNewListener](size, type, listener, once, capture, passive, weak) {
      super[kNewListener](size, type, listener, once, capture, passive, weak);
      if (this[kComposite] && type === "abort" && !this.aborted && size === 1) followCompositeSignal(this);
      const isTimeoutOrNonEmptyCompositeSignal = this[kTimeout] || (this[kComposite] && this[kSourceSignals]?.size);
      if (isTimeoutOrNonEmptyCompositeSignal && type === "abort" && !this.aborted && !weak && size === 1) {
        gcPersistentSignals.add(this);
      }
    }
    [kRemoveListener](size, type, listener, capture) {
      super[kRemoveListener](size, type, listener, capture);
      const isTimeoutOrNonEmptyCompositeSignal = this[kTimeout] || (this[kComposite] && this[kSourceSignals]?.size);
      if (isTimeoutOrNonEmptyCompositeSignal && type === "abort" && size === 0) gcPersistentSignals.delete(this);
    }
  }
  ObjectDefineProperties(AbortSignal.prototype, {
    aborted: kEnumerableProperty,
  });
  ObjectDefineProperty(AbortSignal.prototype, SymbolToStringTag, { writable: false, enumerable: false, configurable: true, value: "AbortSignal" });
  defineEventHandler(AbortSignal.prototype, "abort");

  function abortSignal(signal, reason) {
    if (signal[kAborted]) return;
    signal[kAborted] = true;
    signal[kReason] = reason;
    const dependentSignalsToAbort = [];
    signal[kDependantSignals]?.forEach((s) => {
      const dependentSignal = s.deref();
      if (dependentSignal && !dependentSignal[kAborted]) {
        dependentSignal[kReason] = reason;
        dependentSignal[kAborted] = true;
        dependentSignalsToAbort.push(dependentSignal);
      }
    });
    runAbort(signal);
    for (let i = 0; i < dependentSignalsToAbort.length; i++) runAbort(dependentSignalsToAbort[i]);
    gcPersistentSignals.delete(signal);
    if (signal[kComposite] && signal[kSourceSignals]) {
      signal[kSourceSignals].forEach((sourceWeakRef) => {
        const sourceSignal = sourceWeakRef.deref();
        if (sourceSignal) gcPersistentSignals.delete(sourceSignal);
      });
    }
  }
  function runAbort(signal) {
    signal.dispatchEvent(new Event("abort", { [kTrustEvent]: true }));
  }

  class AbortController {
    #signal;
    get signal() {
      this.#signal ??= new AbortSignal(kDontThrowSymbol);
      return this.#signal;
    }
    abort(reason = new DOMException("This operation was aborted", "AbortError")) {
      abortSignal(this.#signal ??= new AbortSignal(kDontThrowSymbol), reason);
    }
    [inspectCustom](depth, options, inspect) {
      return customInspect(this, { signal: this.signal }, depth, options, inspect);
    }
  }
  ObjectDefineProperties(AbortController.prototype, {
    signal: kEnumerableProperty,
    abort: kEnumerableProperty,
  });
  ObjectDefineProperty(AbortController.prototype, SymbolToStringTag, { writable: false, enumerable: false, configurable: true, value: "AbortController" });

  return {
    Event,
    CustomEvent,
    EventTarget,
    AbortController,
    AbortSignal,
    DOMException,
    isDOMException: (value) => internalsMap.has(value),
    isEventTarget,
    kEvents,
    kResistStopPropagation,
    kWeakHandler,
  };
})
