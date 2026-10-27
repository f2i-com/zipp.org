// node:stream — Readable, Writable, Duplex, Transform, PassThrough, the
// legacy Stream, pipe/pipeline/finished, Readable.from and the promise forms.
// A port of Node's lib/internal/streams that keeps its scheduling: whatever
// Node defers with process.nextTick is deferred here at the same point, so
// events fire in Node's order and on Node's tick. Node's state bitfields are
// plain fields here, under the names packages read (`_readableState.ended`).
//
// Derived from Node.js (lib/internal/streams, lib/stream.js, lib/readline.js,
// lib/internal/readline), which carries this notice:
//
// Copyright Node.js contributors. All rights reserved.
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
(function (host, builtin) {
  "use strict";
  const EE = builtin("events");
  const { Buffer } = builtin("buffer");
  const ArrayIsArray = Array.isArray;
  const ObjectSetPrototypeOf = Object.setPrototypeOf;
  const ObjectDefineProperty = Object.defineProperty;
  const ObjectDefineProperties = Object.defineProperties;
  const kIsDestroyed = Symbol.for("nodejs.stream.destroyed");
  const kIsErrored = Symbol.for("nodejs.stream.errored");
  const kIsReadable = Symbol.for("nodejs.stream.readable");
  const kIsWritable = Symbol.for("nodejs.stream.writable");
  const kIsDisturbed = Symbol.for("nodejs.stream.disturbed");
  const kPromisifyCustom = Symbol.for("nodejs.util.promisify.custom");
  const kDestroy = Symbol("kDestroy");
  const kConstruct = Symbol("kConstruct");
  const kOnFinished = Symbol("kOnFinished");
  const kCallback = Symbol("kCallback");
  const nop = () => {};
  const toBuffer = (c) => Buffer.from(c.buffer, c.byteOffset, c.byteLength);

  // ---- errors: Node's codes and messages, which packages test -------------
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
  const kChunkTypes = "of type string or an instance of Buffer, TypedArray, or DataView";
  const errInvalidArgType = (name, expected, actual) =>
    nodeError(TypeError, "ERR_INVALID_ARG_TYPE", 'The "' + name + '" ' + (name.includes(".") ? "property" : "argument") +
      " must be " + expected + "." + received(actual));
  const errInvalidArgValue = (name, v) =>
    nodeError(TypeError, "ERR_INVALID_ARG_VALUE", "The " + (name.includes(".") ? "property" : "argument") + " '" + name +
      "' is invalid. Received " + (typeof v === "string" ? "'" + v + "'" : String(v)));
  const errUnknownEncoding = (enc) => nodeError(TypeError, "ERR_UNKNOWN_ENCODING", "Unknown encoding: " + enc);
  const errNotImplemented = (m) => nodeError(Error, "ERR_METHOD_NOT_IMPLEMENTED", "The " + m + " method is not implemented");
  const errMultipleCallback = () => nodeError(Error, "ERR_MULTIPLE_CALLBACK", "Callback called multiple times");
  const errDestroyed = (m) => nodeError(Error, "ERR_STREAM_DESTROYED", "Cannot call " + m + " after a stream was destroyed");
  const errPrematureClose = () => nodeError(Error, "ERR_STREAM_PREMATURE_CLOSE", "Premature close");
  const errPushAfterEof = () => nodeError(Error, "ERR_STREAM_PUSH_AFTER_EOF", "stream.push() after EOF");
  const errUnshiftAfterEnd = () => nodeError(Error, "ERR_STREAM_UNSHIFT_AFTER_END_EVENT", "stream.unshift() after end event");
  const errWriteAfterEnd = () => nodeError(Error, "ERR_STREAM_WRITE_AFTER_END", "write after end");
  const errAlreadyFinished = (m) => nodeError(Error, "ERR_STREAM_ALREADY_FINISHED", "Cannot call " + m + " after a stream was finished");
  const errNullValues = () => nodeError(TypeError, "ERR_STREAM_NULL_VALUES", "May not write null values to stream");
  const errCannotPipe = () => nodeError(Error, "ERR_STREAM_CANNOT_PIPE", "Cannot pipe, not readable");
  const errUnableToPipe = () => nodeError(Error, "ERR_STREAM_UNABLE_TO_PIPE", "Cannot pipe to a closed or destroyed stream");
  const errMissingArgs = (name) => nodeError(TypeError, "ERR_MISSING_ARGS", 'The "' + name + '" argument must be specified');
  const errInvalidReturnValue = (input, name, v) =>
    nodeError(TypeError, "ERR_INVALID_RETURN_VALUE", "Expected " + input + ' to be returned from the "' + name + '" function but got ' +
      (v?.constructor?.name ? "instance of " + v.constructor.name : "type " + typeof v) + ".");
  class AbortError extends Error {
    constructor(message = "The operation was aborted", options = undefined) {
      super(message, options);
      this.code = "ABORT_ERR";
      this.name = "AbortError";
    }
  }
  function aggregateTwoErrors(inner, outer) {
    if (inner && outer && inner !== outer) {
      if (ArrayIsArray(outer.errors)) {
        outer.errors.push(inner);
        return outer;
      }
      const err = new AggregateError([outer, inner], outer.message);
      err.code = outer.code;
      return err;
    }
    return inner || outer;
  }
  function validateFunction(v, name) {
    if (typeof v !== "function") throw errInvalidArgType(name, "of type function", v);
  }
  function validateObject(v, name) {
    if (v === null || typeof v !== "object" || ArrayIsArray(v)) throw errInvalidArgType(name, "of type object", v);
  }
  function validateAbortSignal(signal, name) {
    if (signal !== undefined && (signal === null || typeof signal !== "object" || !("aborted" in signal))) {
      throw errInvalidArgType(name, "an instance of AbortSignal", signal);
    }
  }
  function once(fn) {
    let called = false;
    return function (...args) {
      if (called) return;
      called = true;
      return Reflect.apply(fn, this, args);
    };
  }
  // The layer's AbortController (web_events.js, the globals' own, whatever a
  // program assigns over them), and its flag that keeps an abort listener
  // running past stopImmediatePropagation.
  const webEvents = () => host.part("web_events");
  function newAbortController() {
    return new (webEvents().AbortController)();
  }
  function addAbortListener(signal, listener) {
    if (signal.aborted) {
      queueMicrotask(() => listener());
      return nop;
    }
    signal.addEventListener("abort", listener, { __proto__: null, once: true, [webEvents().kResistStopPropagation]: true });
    return () => signal.removeEventListener("abort", listener);
  }

  // ---- highWaterMark ----------------------------------------------------------
  let defaultHwmBytes = host && host.platform === "win32" ? 16 * 1024 : 64 * 1024;
  let defaultHwmObjects = 16;
  function getDefaultHighWaterMark(objectMode) {
    return objectMode ? defaultHwmObjects : defaultHwmBytes;
  }
  function setDefaultHighWaterMark(objectMode, value) {
    if (!Number.isInteger(value) || value < 0) throw errInvalidArgValue("value", value);
    if (objectMode) defaultHwmObjects = value;
    else defaultHwmBytes = value;
  }
  function getHighWaterMark(state, options, duplexKey, isDuplex) {
    const hwm = options.highWaterMark != null ? options.highWaterMark : isDuplex ? options[duplexKey] : null;
    if (hwm != null) {
      if (!Number.isInteger(hwm) || hwm < 0) throw errInvalidArgValue(isDuplex ? "options." + duplexKey : "options.highWaterMark", hwm);
      return Math.floor(hwm);
    }
    return getDefaultHighWaterMark(state.objectMode);
  }
  // An encoding name as Node's StringDecoder reports it.
  function decoderEncoding(enc) {
    if (enc == null || enc === "") return "utf8";
    switch (String(enc).toLowerCase()) {
      case "utf8": case "utf-8": return "utf8";
      case "ucs2": case "ucs-2": case "utf16le": case "utf-16le": return "utf16le";
      case "latin1": case "binary": return "latin1";
      case "base64": return "base64";
      case "base64url": return "base64url";
      case "hex": return "hex";
      case "ascii": return "ascii";
    }
    throw errUnknownEncoding(enc);
  }
  let StringDecoder;
  function newDecoder(enc) {
    StringDecoder ||= builtin("string_decoder").StringDecoder;
    return new StringDecoder(decoderEncoding(enc));
  }

  // ---- the legacy Stream ------------------------------------------------------
  function Stream(opts) {
    EE.call(this, opts);
  }
  ObjectSetPrototypeOf(Stream.prototype, EE.prototype);
  ObjectSetPrototypeOf(Stream, EE);
  Stream.prototype.pipe = function (dest, options) {
    const source = this;
    function ondata(chunk) {
      if (dest.writable && dest.write(chunk) === false && source.pause) source.pause();
    }
    source.on("data", ondata);
    function ondrain() {
      if (source.readable && source.resume) source.resume();
    }
    dest.on("drain", ondrain);
    if (!dest._isStdio && (!options || options.end !== false)) {
      source.on("end", onend);
      source.on("close", onclose);
    }
    let didOnEnd = false;
    function onend() {
      if (didOnEnd) return;
      didOnEnd = true;
      dest.end();
    }
    function onclose() {
      if (didOnEnd) return;
      didOnEnd = true;
      if (typeof dest.destroy === "function") dest.destroy();
    }
    function onerror(er) {
      cleanup();
      if (this.listenerCount?.("error") === 0) this.emit("error", er);
    }
    prependListener(source, "error", onerror);
    prependListener(dest, "error", onerror);
    function cleanup() {
      source.removeListener("data", ondata);
      dest.removeListener("drain", ondrain);
      source.removeListener("end", onend);
      source.removeListener("close", onclose);
      source.removeListener("error", onerror);
      dest.removeListener("error", onerror);
      source.removeListener("end", cleanup);
      source.removeListener("close", cleanup);
      dest.removeListener("close", cleanup);
    }
    source.on("end", cleanup);
    source.on("close", cleanup);
    dest.on("close", cleanup);
    dest.emit("pipe", source);
    return dest;
  };
  // Ahead of the emitter's own listeners, on emitters without prependListener too.
  function prependListener(emitter, event, fn) {
    if (typeof emitter.prependListener === "function") return emitter.prependListener(event, fn);
    if (!emitter._events || !emitter._events[event]) emitter.on(event, fn);
    else if (ArrayIsArray(emitter._events[event])) emitter._events[event].unshift(fn);
    else emitter._events[event] = [fn, emitter._events[event]];
  }
  const listenerCount = (emitter, event) => (typeof emitter.listenerCount === "function" ? emitter.listenerCount(event) : 0);

  // ---- what kind of stream, and how far along --------------------------------
  function isReadableNodeStream(obj, strict = false) {
    return !!(obj && typeof obj.pipe === "function" && typeof obj.on === "function" &&
      (!strict || (typeof obj.pause === "function" && typeof obj.resume === "function")) &&
      (!obj._writableState || obj._readableState?.readable !== false) &&
      (!obj._writableState || obj._readableState));
  }
  function isWritableNodeStream(obj) {
    return !!(obj && typeof obj.write === "function" && typeof obj.on === "function" &&
      (!obj._readableState || obj._writableState?.writable !== false));
  }
  function isDuplexNodeStream(obj) {
    return !!(obj && typeof obj.pipe === "function" && obj._readableState && typeof obj.on === "function" && typeof obj.write === "function");
  }
  function isNodeStream(obj) {
    return obj && (obj._readableState || obj._writableState ||
      (typeof obj.write === "function" && typeof obj.on === "function") ||
      (typeof obj.pipe === "function" && typeof obj.on === "function"));
  }
  function isIterable(obj, isAsync) {
    if (obj == null) return false;
    if (isAsync === true) return typeof obj[Symbol.asyncIterator] === "function";
    if (isAsync === false) return typeof obj[Symbol.iterator] === "function";
    return typeof obj[Symbol.asyncIterator] === "function" || typeof obj[Symbol.iterator] === "function";
  }
  function isDestroyed(stream) {
    if (!isNodeStream(stream)) return null;
    const state = stream._writableState || stream._readableState;
    return !!(stream.destroyed || stream[kIsDestroyed] || state?.destroyed);
  }
  function isWritableEnded(stream) {
    if (!isWritableNodeStream(stream)) return null;
    if (stream.writableEnded === true) return true;
    const w = stream._writableState;
    if (w?.errored) return false;
    if (typeof w?.ended !== "boolean") return null;
    return w.ended;
  }
  function isWritableFinished(stream, strict) {
    if (!isWritableNodeStream(stream)) return null;
    if (stream.writableFinished === true) return true;
    const w = stream._writableState;
    if (w?.errored) return false;
    if (typeof w?.finished !== "boolean") return null;
    return !!(w.finished || (strict === false && w.ended === true && w.length === 0));
  }
  function isReadableFinished(stream, strict) {
    if (!isReadableNodeStream(stream)) return null;
    const r = stream._readableState;
    if (r?.errored) return false;
    if (typeof r?.endEmitted !== "boolean") return null;
    return !!(r.endEmitted || (strict === false && r.ended === true && r.length === 0));
  }
  function isReadable(stream) {
    if (stream && stream[kIsReadable] != null) return stream[kIsReadable];
    if (typeof stream?.readable !== "boolean") return null;
    if (isDestroyed(stream)) return false;
    return isReadableNodeStream(stream) && stream.readable && !isReadableFinished(stream);
  }
  function isWritable(stream) {
    if (stream && stream[kIsWritable] != null) return stream[kIsWritable];
    if (typeof stream?.writable !== "boolean") return null;
    if (isDestroyed(stream)) return false;
    return isWritableNodeStream(stream) && stream.writable && !isWritableEnded(stream);
  }
  function isFinished(stream, opts) {
    if (!isNodeStream(stream)) return null;
    if (isDestroyed(stream)) return true;
    if (opts?.readable !== false && isReadable(stream)) return false;
    if (opts?.writable !== false && isWritable(stream)) return false;
    return true;
  }
  function isWritableErrored(stream) {
    if (!isNodeStream(stream)) return null;
    if (stream.writableErrored) return stream.writableErrored;
    return stream._writableState?.errored ?? null;
  }
  function isReadableErrored(stream) {
    if (!isNodeStream(stream)) return null;
    if (stream.readableErrored) return stream.readableErrored;
    return stream._readableState?.errored ?? null;
  }
  function isClosed(stream) {
    if (!isNodeStream(stream)) return null;
    if (typeof stream.closed === "boolean") return stream.closed;
    const w = stream._writableState, r = stream._readableState;
    if (typeof w?.closed === "boolean" || typeof r?.closed === "boolean") return w?.closed || r?.closed;
    return null;
  }
  function willEmitClose(stream) {
    if (!isNodeStream(stream)) return null;
    const state = stream._writableState || stream._readableState;
    return !!(state?.autoDestroy && state.emitClose && state.closed === false);
  }
  function isDisturbed(stream) {
    return !!(stream && (stream[kIsDisturbed] ?? (stream.readableDidRead || stream.readableAborted)));
  }
  function isErrored(stream) {
    return !!(stream && (stream[kIsErrored] ?? stream.readableErrored ?? stream.writableErrored ??
      stream._readableState?.errorEmitted ?? stream._writableState?.errorEmitted ??
      stream._readableState?.errored ?? stream._writableState?.errored));
  }

  // ---- destroy ----------------------------------------------------------------
  function checkError(err, w, r) {
    if (err) {
      if (w && !w.errored) w.errored = err;
      if (r && !r.errored) r.errored = err;
    }
  }
  // Both sides' destroy; with a duplex the writable side's state leads.
  function destroy(err, cb) {
    const r = this._readableState, w = this._writableState;
    const s = w || r;
    if ((w && w.destroyed) || (r && r.destroyed)) {
      if (typeof cb === "function") cb();
      return this;
    }
    // Destroyed before the error callbacks, so destroy() in them is a no-op.
    checkError(err, w, r);
    if (w) w.destroyed = true;
    if (r) r.destroyed = true;
    if (!s.constructed) {
      this.once(kDestroy, function (er) {
        _destroy(this, aggregateTwoErrors(er, err), cb);
      });
    } else {
      _destroy(this, err, cb);
    }
    return this;
  }
  function _destroy(self, err, cb) {
    let called = false;
    function onDestroy(err) {
      if (called) return;
      called = true;
      const r = self._readableState, w = self._writableState;
      checkError(err, w, r);
      if (w) w.closed = true;
      if (r) r.closed = true;
      if (typeof cb === "function") cb(err);
      if (err) process.nextTick(emitErrorCloseNT, self, err);
      else process.nextTick(emitCloseNT, self);
    }
    try {
      self._destroy(err || null, onDestroy);
    } catch (err) {
      onDestroy(err);
    }
  }
  function emitErrorCloseNT(self, err) {
    emitErrorNT(self, err);
    emitCloseNT(self);
  }
  function emitCloseNT(self) {
    const r = self._readableState, w = self._writableState;
    if (w) w.closeEmitted = true;
    if (r) r.closeEmitted = true;
    if ((w && w.emitClose) || (r && r.emitClose)) self.emit("close");
  }
  function emitErrorNT(self, err) {
    const r = self._readableState, w = self._writableState;
    if ((w && w.errorEmitted) || (r && r.errorEmitted)) return;
    if (w) w.errorEmitted = true;
    if (r) r.errorEmitted = true;
    self.emit("error", err);
  }
  function undestroy() {
    const r = this._readableState, w = this._writableState;
    if (r) {
      r.constructed = true;
      r.closed = r.closeEmitted = r.destroyed = r.errorEmitted = r.reading = false;
      r.errored = null;
      r.ended = r.endEmitted = r.readable === false;
    }
    if (w) {
      w.constructed = true;
      w.destroyed = w.closed = w.closeEmitted = w.errorEmitted = w.finalCalled = w.prefinished = false;
      w.errored = null;
      w.ended = w.ending = w.finished = w.writable === false;
    }
  }
  // Destroys with the error when autoDestroy is on; otherwise only records
  // and emits it (on the next tick when `sync`).
  function errorOrDestroy(stream, err, sync) {
    const r = stream._readableState, w = stream._writableState;
    if ((w && w.destroyed) || (r && r.destroyed)) return;
    if ((r && r.autoDestroy) || (w && w.autoDestroy)) {
      stream.destroy(err);
    } else if (err) {
      if (w && !w.errored) w.errored = err;
      if (r && !r.errored) r.errored = err;
      if (sync) process.nextTick(emitErrorNT, stream, err);
      else emitErrorNT(stream, err);
    }
  }
  // An async `_construct(cb)`: reads, writes and destroy wait for it.
  function construct(stream, cb) {
    if (typeof stream._construct !== "function") return;
    const r = stream._readableState, w = stream._writableState;
    if (r) r.constructed = false;
    if (w) w.constructed = false;
    stream.once(kConstruct, cb);
    if (stream.listenerCount(kConstruct) > 1) return; // A duplex: both sides wait on one call.
    process.nextTick(constructNT, stream);
  }
  function constructNT(stream) {
    let called = false;
    function onConstruct(err) {
      if (called) {
        errorOrDestroy(stream, err ?? errMultipleCallback());
        return;
      }
      called = true;
      const r = stream._readableState, w = stream._writableState;
      const s = w || r;
      if (r) r.constructed = true;
      if (w) w.constructed = true;
      if (s.destroyed) stream.emit(kDestroy, err);
      else if (err) errorOrDestroy(stream, err, true);
      else stream.emit(kConstruct);
    }
    try {
      stream._construct((err) => {
        process.nextTick(onConstruct, err);
      });
    } catch (err) {
      process.nextTick(onConstruct, err);
    }
  }
  function emitCloseLegacy(stream) {
    stream.emit("close");
  }
  function emitErrorCloseLegacy(stream, err) {
    stream.emit("error", err);
    process.nextTick(emitCloseLegacy, stream);
  }
  // Destroys any stream-like thing, legacy ones included (stream.destroy).
  function destroyer(stream, err) {
    if (!stream || isDestroyed(stream)) return;
    if (!err && !isFinished(stream)) err = new AbortError();
    if (typeof stream.destroy === "function") stream.destroy(err);
    else if (typeof stream.close === "function") stream.close();
    else if (err) process.nextTick(emitErrorCloseLegacy, stream, err);
    else process.nextTick(emitCloseLegacy, stream);
    if (!stream.destroyed) stream[kIsDestroyed] = true;
  }

  // ---- finished (end-of-stream) ----------------------------------------------
  function eos(stream, options, callback) {
    if (arguments.length === 2) {
      callback = options;
      options = {};
    } else if (options == null) {
      options = {};
    } else {
      validateObject(options, "options");
    }
    validateFunction(callback, "callback");
    validateAbortSignal(options.signal, "options.signal");
    callback = once(callback);
    if (!isNodeStream(stream)) throw errInvalidArgType("stream", "an instance of ReadableStream, WritableStream, or Stream", stream);
    const readable = options.readable ?? isReadableNodeStream(stream);
    const writable = options.writable ?? isWritableNodeStream(stream);
    const wState = stream._writableState, rState = stream._readableState;
    const onlegacyfinish = () => {
      if (!stream.writable) onfinish();
    };
    // With 'close' coming (autoDestroy and emitClose), wait for it rather
    // than call back on 'end'/'finish'.
    let emitsClose = willEmitClose(stream) && isReadableNodeStream(stream) === readable && isWritableNodeStream(stream) === writable;
    let writableFinished = isWritableFinished(stream, false);
    const onfinish = () => {
      writableFinished = true;
      if (stream.destroyed) emitsClose = false;
      if (emitsClose && (!stream.readable || readable)) return;
      if (!readable || readableFinished) callback.call(stream);
    };
    let readableFinished = isReadableFinished(stream, false);
    const onend = () => {
      readableFinished = true;
      if (stream.destroyed) emitsClose = false;
      if (emitsClose && (!stream.writable || writable)) return;
      if (!writable || writableFinished) callback.call(stream);
    };
    const onerror = (err) => {
      callback.call(stream, err);
    };
    let closed = isClosed(stream);
    const onclose = () => {
      closed = true;
      const errored = isWritableErrored(stream) || isReadableErrored(stream);
      if (errored && typeof errored !== "boolean") return callback.call(stream, errored);
      if (readable && !readableFinished && isReadableNodeStream(stream, true)) {
        if (!isReadableFinished(stream, false)) return callback.call(stream, errPrematureClose());
      }
      if (writable && !writableFinished) {
        if (!isWritableFinished(stream, false)) return callback.call(stream, errPrematureClose());
      }
      callback.call(stream);
    };
    const onclosed = () => {
      closed = true;
      const errored = isWritableErrored(stream) || isReadableErrored(stream);
      if (errored && typeof errored !== "boolean") return callback.call(stream, errored);
      callback.call(stream);
    };
    if (writable && !wState) {
      // A legacy stream: no writable state to tell 'finish' by.
      stream.on("end", onlegacyfinish);
      stream.on("close", onlegacyfinish);
    }
    if (!emitsClose && typeof stream.aborted === "boolean") stream.on("aborted", onclose);
    stream.on("end", onend);
    stream.on("finish", onfinish);
    if (options.error !== false) stream.on("error", onerror);
    stream.on("close", onclose);
    if (closed) {
      process.nextTick(onclose);
    } else if (wState?.errorEmitted || rState?.errorEmitted) {
      if (!emitsClose) process.nextTick(onclosed);
    } else if (!readable && (!emitsClose || isReadable(stream)) && (writableFinished || isWritable(stream) === false) &&
      (wState == null || wState.pendingcb === undefined || wState.pendingcb === 0)) {
      process.nextTick(onclosed);
    } else if (!writable && (!emitsClose || isWritable(stream)) && (readableFinished || isReadable(stream) === false)) {
      process.nextTick(onclosed);
    }
    const cleanup = () => {
      callback = nop;
      stream.removeListener("aborted", onclose);
      stream.removeListener("end", onlegacyfinish);
      stream.removeListener("close", onlegacyfinish);
      stream.removeListener("finish", onfinish);
      stream.removeListener("end", onend);
      stream.removeListener("error", onerror);
      stream.removeListener("close", onclose);
    };
    if (options.signal && !closed) {
      const abort = () => {
        const endCallback = callback;
        cleanup();
        endCallback.call(stream, new AbortError(undefined, { cause: options.signal.reason }));
      };
      if (options.signal.aborted) {
        process.nextTick(abort);
      } else {
        const dispose = addAbortListener(options.signal, abort);
        const originalCallback = callback;
        callback = once((...args) => {
          dispose();
          originalCallback.apply(stream, args);
        });
      }
    }
    return cleanup;
  }
  function finished(stream, opts) {
    let autoCleanup = false;
    if (opts === null) opts = {};
    if (opts?.cleanup) {
      if (typeof opts.cleanup !== "boolean") throw errInvalidArgType("cleanup", "of type boolean", opts.cleanup);
      autoCleanup = opts.cleanup;
    }
    return new Promise((resolve, reject) => {
      const cleanup = eos(stream, opts, (err) => {
        if (autoCleanup) cleanup();
        if (err) reject(err);
        else resolve();
      });
    });
  }
  function addAbortSignal(signal, stream) {
    validateAbortSignal(signal, "signal");
    if (signal === undefined) throw errInvalidArgType("signal", "an instance of AbortSignal", signal);
    if (!isNodeStream(stream)) throw errInvalidArgType("stream", "an instance of ReadableStream, WritableStream, or Stream", stream);
    const onAbort = () => {
      stream.destroy(new AbortError(undefined, { cause: signal.reason }));
    };
    if (signal.aborted) onAbort();
    else eos(stream, addAbortListener(signal, onAbort));
    return stream;
  }

  // ---- Readable -----------------------------------------------------------------
  function ReadableState(options, stream, isDuplex) {
    // objectMode: read(n) ignores n, every chunk counts 1 toward the hwm.
    this.objectMode = !!(options && (options.objectMode || (isDuplex && options.readableObjectMode)));
    // Where _read stops being called to fill the buffer; 0 never reads ahead.
    this.highWaterMark = options ? getHighWaterMark(this, options, "readableHighWaterMark", isDuplex) : getDefaultHighWaterMark(false);
    this.buffer = [];
    this.bufferIndex = 0;
    this.length = 0;
    this.pipes = [];
    this.flowing = null;
    this.ended = false;
    this.endEmitted = false;
    this.reading = false;
    this.constructed = true;
    // Whether 'readable'/'data' would fire inside the read() that pushed.
    this.sync = true;
    this.needReadable = false;
    this.emittedReadable = false;
    this.readableListening = false;
    this.resumeScheduled = false;
    this.errorEmitted = false;
    this.emitClose = !(options && options.emitClose === false);
    this.autoDestroy = !(options && options.autoDestroy === false);
    this.destroyed = false;
    this.errored = null;
    this.closed = false;
    this.closeEmitted = false;
    this.multiAwaitDrain = false;
    this.readingMore = false;
    this.dataEmitted = false;
    this.dataListening = false;
    this.hasPaused = false;
    this.paused = false;
    const defaultEncoding = options?.defaultEncoding;
    if (defaultEncoding == null || defaultEncoding === "utf8" || defaultEncoding === "utf-8") this.defaultEncoding = "utf8";
    else if (Buffer.isEncoding(defaultEncoding)) this.defaultEncoding = defaultEncoding;
    else throw errUnknownEncoding(defaultEncoding);
    // The piped destinations a 'drain' is awaited from: null, one, or a Set.
    this.awaitDrainWriters = null;
    this.decoder = null;
    this.encoding = null;
    if (options?.encoding) {
      this.decoder = newDecoder(options.encoding);
      this.encoding = options.encoding;
    }
  }
  ObjectDefineProperty(ReadableState.prototype, "pipesCount", {
    get() { return this.pipes.length; },
  });
  function readableOnConstructed(stream, state) {
    if (state.needReadable) maybeReadMore(stream, state);
  }

  function Readable(options) {
    if (!(this instanceof Readable)) return new Readable(options);
    this._readableState = new ReadableState(options, this, false);
    if (options) {
      if (typeof options.read === "function") this._read = options.read;
      if (typeof options.destroy === "function") this._destroy = options.destroy;
      if (typeof options.construct === "function") this._construct = options.construct;
      if (options.signal) addAbortSignal(options.signal, this);
    }
    Stream.call(this, options);
    if (this._construct != null) {
      construct(this, () => {
        readableOnConstructed(this, this._readableState);
      });
    }
  }
  ObjectSetPrototypeOf(Readable.prototype, Stream.prototype);
  ObjectSetPrototypeOf(Readable, Stream);
  Readable.ReadableState = ReadableState;
  Readable.prototype.destroy = destroy;
  Readable.prototype._undestroy = undestroy;
  Readable.prototype._destroy = function (err, cb) {
    cb(err);
  };
  Readable.prototype[EE.captureRejectionSymbol] = function (err) {
    this.destroy(err);
  };
  if (typeof Symbol.asyncDispose === "symbol") {
    Readable.prototype[Symbol.asyncDispose] = async function () {
      let error;
      if (!this.destroyed) {
        error = this.readableEnded ? null : new AbortError();
        this.destroy(error);
      }
      await new Promise((resolve, reject) => eos(this, (err) => (err && err !== error ? reject(err) : resolve(null))));
    };
  }
  // Returns whether more may be pushed before the highWaterMark.
  Readable.prototype.push = function (chunk, encoding) {
    const state = this._readableState;
    return state.objectMode ? addChunkObjectMode(this, state, chunk, encoding) : addChunkByteMode(this, state, chunk, encoding);
  };
  Readable.prototype.unshift = function (chunk, encoding) {
    const state = this._readableState;
    if (chunk === null) {
      state.reading = false;
      onEofChunk(this, state);
      return false;
    }
    if (!state.objectMode) {
      if (typeof chunk === "string") {
        encoding ||= state.defaultEncoding;
        if (state.encoding !== encoding) {
          // With an encoding set, the buffer holds strings in it.
          chunk = state.encoding ? Buffer.from(chunk, encoding).toString(state.encoding) : Buffer.from(chunk, encoding);
        }
      } else if (ArrayBuffer.isView(chunk)) {
        if (!(chunk instanceof Buffer)) chunk = toBuffer(chunk);
      } else if (chunk !== undefined) {
        errorOrDestroy(this, errInvalidArgType("chunk", kChunkTypes, chunk));
        return false;
      }
      if (!(chunk && chunk.length > 0)) return canPushMore(state);
    }
    if (state.endEmitted) errorOrDestroy(this, errUnshiftAfterEnd());
    else if (state.destroyed || state.errored) return false;
    else addChunk(this, state, chunk, true);
    return canPushMore(state);
  };
  function addChunkByteMode(stream, state, chunk, encoding) {
    if (chunk === null) {
      state.reading = false;
      onEofChunk(stream, state);
      return false;
    }
    if (typeof chunk === "string") {
      encoding ||= state.defaultEncoding;
      if (state.encoding !== encoding) {
        chunk = Buffer.from(chunk, encoding);
        encoding = "";
      }
    } else if (chunk instanceof Buffer) {
      encoding = "";
    } else if (ArrayBuffer.isView(chunk)) {
      chunk = toBuffer(chunk);
      encoding = "";
    } else if (chunk !== undefined) {
      errorOrDestroy(stream, errInvalidArgType("chunk", kChunkTypes, chunk));
      return false;
    }
    if (!chunk || chunk.length <= 0) {
      state.reading = false;
      maybeReadMore(stream, state);
      return canPushMore(state);
    }
    if (state.ended) {
      errorOrDestroy(stream, errPushAfterEof());
      return false;
    }
    if (state.destroyed || state.errored) return false;
    state.reading = false;
    if (state.decoder && !encoding) {
      chunk = state.decoder.write(chunk);
      if (chunk.length === 0) {
        maybeReadMore(stream, state);
        return canPushMore(state);
      }
    }
    addChunk(stream, state, chunk, false);
    return canPushMore(state);
  }
  function addChunkObjectMode(stream, state, chunk, encoding) {
    if (chunk === null) {
      state.reading = false;
      onEofChunk(stream, state);
      return false;
    }
    if (state.ended) {
      errorOrDestroy(stream, errPushAfterEof());
      return false;
    }
    if (state.destroyed || state.errored) return false;
    state.reading = false;
    if (state.decoder && !encoding) chunk = state.decoder.write(chunk);
    addChunk(stream, state, chunk, false);
    return canPushMore(state);
  }
  function canPushMore(state) {
    // Below the hwm, or nothing buffered at all (an hwm of 0 still reads).
    return !state.ended && (state.length < state.highWaterMark || state.length === 0);
  }
  function addChunk(stream, state, chunk, addToFront) {
    if (state.flowing === true && !state.sync && state.dataListening && state.length === 0) {
      // Flowing with nothing queued: straight to 'data'.
      if (state.multiAwaitDrain) state.awaitDrainWriters.clear();
      else state.awaitDrainWriters = null;
      state.dataEmitted = true;
      stream.emit("data", chunk);
    } else {
      state.length += state.objectMode ? 1 : chunk.length;
      if (addToFront) {
        if (state.bufferIndex > 0) state.buffer[--state.bufferIndex] = chunk;
        else state.buffer.unshift(chunk);
      } else {
        state.buffer.push(chunk);
      }
      if (state.needReadable) emitReadable(stream);
    }
    maybeReadMore(stream, state);
  }
  Readable.prototype.isPaused = function () {
    const state = this._readableState;
    return state.paused || state.flowing === false;
  };
  Readable.prototype.setEncoding = function (enc) {
    const state = this._readableState;
    const decoder = newDecoder(enc);
    state.decoder = decoder;
    state.encoding = decoderEncoding(enc);
    // What is already buffered is decoded now.
    const buffer = state.buffer;
    let content = "";
    for (let i = state.bufferIndex; i < buffer.length; i++) content += decoder.write(buffer[i]);
    buffer.length = 0;
    state.bufferIndex = 0;
    if (content !== "") buffer.push(content);
    state.length = content.length;
    return this;
  };
  const MAX_HWM = 0x40000000;
  function computeNewHighWaterMark(n) {
    if (n > MAX_HWM) {
      throw nodeError(RangeError, "ERR_OUT_OF_RANGE", 'The value of "size" is out of range. It must be <= 1GiB. Received ' + n);
    }
    // The next power of two, so the hwm does not creep up a byte at a time.
    n--;
    n |= n >>> 1;
    n |= n >>> 2;
    n |= n >>> 4;
    n |= n >>> 8;
    n |= n >>> 16;
    return n + 1;
  }
  function howMuchToRead(n, state) {
    if (n <= 0 || (state.length === 0 && state.ended)) return 0;
    if (state.objectMode) return 1;
    if (n !== n) {
      // read() with no size: flowing hands out one chunk at a time.
      if (state.flowing === true && state.length) return state.buffer[state.bufferIndex].length;
      return state.length;
    }
    if (n <= state.length) return n;
    return state.ended ? state.length : 0;
  }
  Readable.prototype.read = function (n) {
    if (n === undefined) n = NaN;
    else if (!Number.isInteger(n)) n = Number.parseInt(n, 10);
    const state = this._readableState;
    const nOrig = n;
    if (n > state.highWaterMark) state.highWaterMark = computeNewHighWaterMark(n);
    if (n !== 0) state.emittedReadable = false;
    // read(0) to trigger 'readable' with enough already buffered: just emit it.
    if (n === 0 && state.needReadable &&
      ((state.highWaterMark !== 0 ? state.length >= state.highWaterMark : state.length > 0) || state.ended)) {
      if (state.length === 0 && state.ended) endReadable(this);
      else emitReadable(this);
      return null;
    }
    n = howMuchToRead(n, state);
    if (n === 0 && state.ended) {
      if (state.length === 0) endReadable(this);
      return null;
    }
    // Call _read when a 'readable' is owed or the buffer would drop below the
    // hwm, unless a read is in flight, the stream has ended or failed, or it
    // is still being constructed. _read may push synchronously, so how much
    // to return is worked out again after it.
    let doRead = state.needReadable;
    if (state.length === 0 || state.length - n < state.highWaterMark) doRead = true;
    if (state.reading || state.ended || state.destroyed || state.errored || !state.constructed) {
      doRead = false;
    } else if (doRead) {
      state.reading = true;
      state.sync = true;
      if (state.length === 0) state.needReadable = true;
      try {
        this._read(state.highWaterMark);
      } catch (err) {
        errorOrDestroy(this, err);
      }
      state.sync = false;
      if (!state.reading) n = howMuchToRead(nOrig, state);
    }
    const ret = n > 0 ? fromList(n, state) : null;
    if (ret === null) {
      if (state.length <= state.highWaterMark) state.needReadable = true;
      n = 0;
    } else {
      state.length -= n;
      if (state.multiAwaitDrain) state.awaitDrainWriters.clear();
      else state.awaitDrainWriters = null;
    }
    if (state.length === 0) {
      // Empty: be told as soon as something arrives.
      if (!state.ended) state.needReadable = true;
      // Read past the end: 'end' on the next tick.
      if (nOrig !== n && state.ended) endReadable(this);
    }
    if (ret !== null && !state.errorEmitted && !state.closeEmitted) {
      state.dataEmitted = true;
      this.emit("data", ret);
    }
    return ret;
  };
  function onEofChunk(stream, state) {
    if (state.ended) return;
    if (state.decoder) {
      const chunk = state.decoder.end();
      if (chunk?.length) {
        state.buffer.push(chunk);
        state.length += state.objectMode ? 1 : chunk.length;
      }
    }
    state.ended = true;
    if (state.sync) {
      // Pushed inside read(): wait a tick, or flow() would emit within it.
      emitReadable(stream);
    } else {
      // Packages rely on this 'readable' being synchronous.
      state.needReadable = false;
      state.emittedReadable = true;
      emitReadable_(stream);
    }
  }
  // 'readable' on the next tick, never inside read() (which could recurse).
  function emitReadable(stream) {
    const state = stream._readableState;
    state.needReadable = false;
    if (!state.emittedReadable) {
      state.emittedReadable = true;
      process.nextTick(emitReadable_, stream);
    }
  }
  function emitReadable_(stream) {
    const state = stream._readableState;
    if (!state.destroyed && !state.errored && (state.length || state.ended)) {
      stream.emit("readable");
      state.emittedReadable = false;
    }
    // Owed another 'readable' if not flowing (flow covers that), not ended,
    // and at or under the hwm.
    if (state.flowing !== true && !state.ended && state.length <= state.highWaterMark) state.needReadable = true;
    flow(stream);
  }
  // Read ahead up to the hwm on the next tick, once per tick.
  function maybeReadMore(stream, state) {
    if (!state.readingMore && !state.reading && state.constructed) {
      state.readingMore = true;
      process.nextTick(maybeReadMore_, stream, state);
    }
  }
  function maybeReadMore_(stream, state) {
    while (!state.reading && !state.ended &&
      (state.length < state.highWaterMark || (state.flowing === true && state.length === 0))) {
      const len = state.length;
      stream.read(0);
      if (len === state.length) break; // Nothing came; stop spinning.
    }
    state.readingMore = false;
  }
  Readable.prototype._read = function (n) {
    throw errNotImplemented("_read()");
  };
  Readable.prototype.pipe = function (dest, pipeOpts) {
    const src = this;
    const state = this._readableState;
    if (state.pipes.length === 1 && !state.multiAwaitDrain) {
      state.multiAwaitDrain = true;
      state.awaitDrainWriters = new Set(state.awaitDrainWriters ? [state.awaitDrainWriters] : []);
    }
    state.pipes.push(dest);
    const doEnd = (!pipeOpts || pipeOpts.end !== false) && dest !== process.stdout && dest !== process.stderr;
    const endFn = doEnd ? onend : unpipe;
    if (state.endEmitted) process.nextTick(endFn);
    else src.once("end", endFn);
    dest.on("unpipe", onunpipe);
    function onunpipe(readable, unpipeInfo) {
      if (readable === src && unpipeInfo && unpipeInfo.hasUnpiped === false) {
        unpipeInfo.hasUnpiped = true;
        cleanup();
      }
    }
    function onend() {
      dest.end();
    }
    let ondrain;
    let cleanedUp = false;
    function cleanup() {
      dest.removeListener("close", onclose);
      dest.removeListener("finish", onfinish);
      if (ondrain) dest.removeListener("drain", ondrain);
      dest.removeListener("error", onerror);
      dest.removeListener("unpipe", onunpipe);
      src.removeListener("end", onend);
      src.removeListener("end", unpipe);
      src.removeListener("data", ondata);
      cleanedUp = true;
      // A source waiting on this dest's 'drain' would never flow again.
      if (ondrain && state.awaitDrainWriters && (!dest._writableState || dest._writableState.needDrain)) ondrain();
    }
    function pause() {
      // Unpiped during dest.write(): pausing for it would stick for good.
      if (!cleanedUp) {
        if (state.pipes.length === 1 && state.pipes[0] === dest) {
          state.awaitDrainWriters = dest;
          state.multiAwaitDrain = false;
        } else if (state.pipes.length > 1 && state.pipes.includes(dest)) {
          state.awaitDrainWriters.add(dest);
        }
        src.pause();
      }
      if (!ondrain) {
        ondrain = pipeOnDrain(src, dest);
        dest.on("drain", ondrain);
      }
    }
    src.on("data", ondata);
    function ondata(chunk) {
      try {
        if (dest.write(chunk) === false) pause();
      } catch (error) {
        dest.destroy(error);
      }
    }
    // Stop piping into a dest that errors, without swallowing the error.
    function onerror(er) {
      unpipe();
      dest.removeListener("error", onerror);
      if (listenerCount(dest, "error") === 0) {
        const s = dest._writableState || dest._readableState;
        if (s && !s.errorEmitted) errorOrDestroy(dest, er);
        else dest.emit("error", er);
      }
    }
    prependListener(dest, "error", onerror);
    function onclose() {
      dest.removeListener("finish", onfinish);
      unpipe();
    }
    dest.once("close", onclose);
    function onfinish() {
      dest.removeListener("close", onclose);
      unpipe();
    }
    dest.once("finish", onfinish);
    function unpipe() {
      src.unpipe(dest);
    }
    dest.emit("pipe", src);
    if (dest.writableNeedDrain === true) pause();
    else if (state.flowing !== true) src.resume();
    return dest;
  };
  function pipeOnDrain(src, dest) {
    return function pipeOnDrainFunctionResult() {
      const state = src._readableState;
      if (state.awaitDrainWriters === dest) state.awaitDrainWriters = null;
      else if (state.multiAwaitDrain) state.awaitDrainWriters.delete(dest);
      if ((!state.awaitDrainWriters || state.awaitDrainWriters.size === 0) && state.dataListening) src.resume();
    };
  }
  Readable.prototype.unpipe = function (dest) {
    const state = this._readableState;
    if (state.pipes.length === 0) return this;
    if (!dest) {
      const dests = state.pipes;
      state.pipes = [];
      this.pause();
      for (let i = 0; i < dests.length; i++) dests[i].emit("unpipe", this, { hasUnpiped: false });
      return this;
    }
    const index = state.pipes.indexOf(dest);
    if (index === -1) return this;
    state.pipes.splice(index, 1);
    if (state.pipes.length === 0) this.pause();
    dest.emit("unpipe", this, { hasUnpiped: false });
    return this;
  };
  // A 'data' listener starts the flow (unless paused explicitly); a
  // 'readable' listener switches to paused mode.
  Readable.prototype.on = function (ev, fn) {
    const res = Stream.prototype.on.call(this, ev, fn);
    const state = this._readableState;
    if (ev === "data") {
      state.dataListening = true;
      if (this.listenerCount("readable") > 0) state.readableListening = true;
      if (state.flowing !== false) this.resume();
    } else if (ev === "readable") {
      if (!state.endEmitted && !state.readableListening) {
        state.readableListening = state.needReadable = true;
        state.flowing = false;
        state.emittedReadable = false;
        if (state.length) emitReadable(this);
        else if (!state.reading) process.nextTick(nReadingNextTick, this);
      }
    }
    return res;
  };
  Readable.prototype.addListener = Readable.prototype.on;
  Readable.prototype.removeListener = function (ev, fn) {
    const state = this._readableState;
    const res = Stream.prototype.removeListener.call(this, ev, fn);
    // On the next tick, so once('readable', fn) cycles keep paused mode.
    if (ev === "readable") process.nextTick(updateReadableListening, this);
    else if (ev === "data" && this.listenerCount("data") === 0) state.dataListening = false;
    return res;
  };
  Readable.prototype.off = Readable.prototype.removeListener;
  Readable.prototype.removeAllListeners = function (ev) {
    const res = Reflect.apply(Stream.prototype.removeAllListeners, this, arguments);
    if (ev === "readable" || ev === undefined) process.nextTick(updateReadableListening, this);
    return res;
  };
  function updateReadableListening(self) {
    const state = self._readableState;
    state.readableListening = self.listenerCount("readable") > 0;
    if (state.resumeScheduled && state.hasPaused && !state.paused) {
      // A resume() is coming: it must flow.
      state.flowing = true;
    } else if (state.dataListening) {
      self.resume();
    } else if (!state.readableListening) {
      state.flowing = null;
    }
  }
  function nReadingNextTick(self) {
    self.read(0);
  }
  Readable.prototype.resume = function () {
    const state = this._readableState;
    if (state.flowing !== true) {
      // Flowing only with no 'readable' listener; resume() still schedules.
      state.flowing = !state.readableListening;
      if (!state.resumeScheduled) {
        state.resumeScheduled = true;
        process.nextTick(resume_, this, state);
      }
    }
    state.hasPaused = true;
    state.paused = false;
    return this;
  };
  function resume_(stream, state) {
    if (!state.reading) stream.read(0);
    state.resumeScheduled = false;
    stream.emit("resume");
    flow(stream);
    if (state.flowing === true && !state.reading) stream.read(0);
  }
  Readable.prototype.pause = function () {
    const state = this._readableState;
    if (state.flowing !== false) {
      state.flowing = false;
      this.emit("pause");
    }
    state.hasPaused = true;
    state.paused = true;
    return this;
  };
  function flow(stream) {
    const state = stream._readableState;
    while (state.flowing === true && stream.read() !== null);
  }
  // An old-style stream ('data'/'end' events) as this stream's source.
  Readable.prototype.wrap = function (stream) {
    let paused = false;
    stream.on("data", (chunk) => {
      if (!this.push(chunk) && stream.pause) {
        paused = true;
        stream.pause();
      }
    });
    stream.on("end", () => {
      this.push(null);
    });
    stream.on("error", (err) => {
      errorOrDestroy(this, err);
    });
    stream.on("close", () => {
      this.destroy();
    });
    stream.on("destroy", () => {
      this.destroy();
    });
    this._read = () => {
      if (paused && stream.resume) {
        paused = false;
        stream.resume();
      }
    };
    for (const key of Object.keys(stream)) {
      if (this[key] === undefined && typeof stream[key] === "function") this[key] = stream[key].bind(stream);
    }
    return this;
  };
  Readable.prototype[Symbol.asyncIterator] = function () {
    return streamToAsyncIterator(this);
  };
  Readable.prototype.iterator = function (options) {
    if (options !== undefined) validateObject(options, "options");
    return streamToAsyncIterator(this, options);
  };
  function streamToAsyncIterator(stream, options) {
    if (typeof stream.read !== "function") stream = Readable.wrap(stream, { objectMode: true });
    const iter = createAsyncIterator(stream, options);
    iter.stream = stream;
    return iter;
  }
  async function* createAsyncIterator(stream, options) {
    let callback = nop;
    // As the 'readable' listener (this === stream) it wakes the loop; as a
    // promise executor it is the loop going to sleep.
    function next(resolve) {
      if (this === stream) {
        callback();
        callback = nop;
      } else {
        callback = resolve;
      }
    }
    stream.on("readable", next);
    let error;
    const cleanup = eos(stream, { writable: false }, (err) => {
      error = err ? aggregateTwoErrors(error, err) : null;
      callback();
      callback = nop;
    });
    try {
      while (true) {
        const chunk = stream.destroyed ? null : stream.read();
        if (chunk !== null) yield chunk;
        else if (error) throw error;
        else if (error === null) return;
        else await new Promise(next);
      }
    } catch (err) {
      error = aggregateTwoErrors(error, err);
      throw error;
    } finally {
      // Leaving early (break, return, throw) destroys the stream, unless it
      // is a half-open duplex still being written.
      const preserveHalfOpenDuplex = error === null && stream.allowHalfOpen === true && stream.writable === true && stream.writableEnded !== true;
      if ((error || options?.destroyOnReturn !== false) && (error === undefined || stream._readableState.autoDestroy) && !preserveHalfOpenDuplex) {
        destroyer(stream, null);
      } else {
        stream.off("readable", next);
        cleanup();
      }
    }
  }
  ObjectDefineProperties(Readable.prototype, {
    readable: {
      get() {
        const r = this._readableState;
        // r.readable === false: a duplex built without its readable side.
        return !!r && r.readable !== false && !r.destroyed && !r.errorEmitted && !r.endEmitted;
      },
      set(val) {
        if (this._readableState) this._readableState.readable = !!val;
      },
    },
    readableDidRead: { get() { return this._readableState.dataEmitted; } },
    readableAborted: {
      get() {
        const r = this._readableState;
        return !!(r.readable !== false && (r.destroyed || r.errored) && !r.endEmitted);
      },
    },
    readableHighWaterMark: { get() { return this._readableState.highWaterMark; } },
    readableBuffer: { get() { return this._readableState?.buffer; } },
    readableFlowing: {
      get() { return this._readableState.flowing; },
      set(value) {
        if (this._readableState) this._readableState.flowing = value == null ? null : !!value;
      },
    },
    readableLength: { get() { return this._readableState.length; } },
    readableObjectMode: { get() { return this._readableState ? this._readableState.objectMode : false; } },
    readableEncoding: { get() { return this._readableState ? this._readableState.encoding : null; } },
    errored: { get() { return this._readableState ? this._readableState.errored : null; } },
    closed: { get() { return this._readableState ? this._readableState.closed : false; } },
    destroyed: {
      get() { return this._readableState ? this._readableState.destroyed : false; },
      set(value) {
        if (this._readableState) this._readableState.destroyed = !!value;
      },
    },
    readableEnded: { get() { return this._readableState ? this._readableState.endEmitted : false; } },
  });
  // Takes n bytes (or chars, or one object) off the front of the buffer.
  function fromList(n, state) {
    if (state.length === 0) return null;
    let idx = state.bufferIndex;
    let ret;
    const buf = state.buffer;
    const len = buf.length;
    if (state.objectMode) {
      ret = buf[idx];
      buf[idx++] = null;
    } else if (!n || n >= state.length) {
      // All of it.
      if (state.decoder) {
        ret = "";
        while (idx < len) {
          ret += buf[idx];
          buf[idx++] = null;
        }
      } else if (len - idx === 0) {
        ret = Buffer.alloc(0);
      } else if (len - idx === 1) {
        ret = buf[idx];
        buf[idx++] = null;
      } else {
        ret = Buffer.allocUnsafe(state.length);
        let i = 0;
        while (idx < len) {
          ret.set(buf[idx], i);
          i += buf[idx].length;
          buf[idx++] = null;
        }
      }
    } else if (n < buf[idx].length) {
      // Part of the first chunk (slice suits strings and buffers alike).
      ret = buf[idx].slice(0, n);
      buf[idx] = buf[idx].slice(n);
    } else if (n === buf[idx].length) {
      ret = buf[idx];
      buf[idx++] = null;
    } else if (state.decoder) {
      ret = "";
      while (idx < len) {
        const str = buf[idx];
        if (n > str.length) {
          ret += str;
          n -= str.length;
          buf[idx++] = null;
        } else {
          if (n === str.length) {
            ret += str;
            buf[idx++] = null;
          } else {
            ret += str.slice(0, n);
            buf[idx] = str.slice(n);
          }
          break;
        }
      }
    } else {
      ret = Buffer.allocUnsafe(n);
      const retLen = n;
      while (idx < len) {
        const data = buf[idx];
        if (n > data.length) {
          ret.set(data, retLen - n);
          n -= data.length;
          buf[idx++] = null;
        } else {
          if (n === data.length) {
            ret.set(data, retLen - n);
            buf[idx++] = null;
          } else {
            ret.set(data.subarray(0, n), retLen - n);
            buf[idx] = data.subarray(n);
          }
          break;
        }
      }
    }
    // The consumed head is dropped in batches, not per chunk.
    if (idx === len) {
      state.buffer.length = 0;
      state.bufferIndex = 0;
    } else if (idx > 1024) {
      state.buffer.splice(0, idx);
      state.bufferIndex = 0;
    } else {
      state.bufferIndex = idx;
    }
    return ret;
  }
  Readable._fromList = fromList;
  function endReadable(stream) {
    const state = stream._readableState;
    if (!state.endEmitted) {
      state.ended = true;
      process.nextTick(endReadableNT, state, stream);
    }
  }
  function endReadableNT(state, stream) {
    // Not if an unshift came in after all.
    if (!state.errored && !state.closeEmitted && !state.endEmitted && state.length === 0) {
      state.endEmitted = true;
      stream.emit("end");
      if (stream.writable && stream.allowHalfOpen === false) {
        process.nextTick(endWritableNT, stream);
      } else if (state.autoDestroy) {
        // A duplex is destroyed once its writable side is done too.
        const wState = stream._writableState;
        if (!wState || (wState.autoDestroy && (wState.finished || wState.writable === false))) stream.destroy();
      }
    }
  }
  function endWritableNT(stream) {
    if (stream.writable && !stream.writableEnded && !stream.destroyed) stream.end();
  }
  Readable.from = function (iterable, opts) {
    return from(Readable, iterable, opts);
  };
  Readable.wrap = function (src, options) {
    return new Readable({
      objectMode: src.readableObjectMode ?? src.objectMode ?? true,
      ...options,
      destroy(err, callback) {
        destroyer(src, err);
        callback(err);
      },
    }).wrap(src);
  };

  // Readable.from: an iterable, async iterable, string or Buffer as a stream.
  function from(Readable, iterable, opts) {
    if (typeof iterable === "string" || iterable instanceof Buffer) {
      return new Readable({
        objectMode: true,
        ...opts,
        read() {
          this.push(iterable);
          this.push(null);
        },
      });
    }
    let iterator, isAsync;
    if (iterable?.[Symbol.asyncIterator]) {
      isAsync = true;
      iterator = iterable[Symbol.asyncIterator]();
    } else if (iterable?.[Symbol.iterator]) {
      isAsync = false;
      iterator = iterable[Symbol.iterator]();
    } else {
      throw errInvalidArgType("iterable", "an instance of Iterable", iterable);
    }
    const readable = new Readable({ objectMode: true, highWaterMark: 1, ...opts });
    const originalDestroy = readable._destroy;
    // One pull loop at a time; a full buffer ends it until the next _read.
    let reading = false;
    let isAsyncValues = false;
    readable._read = function () {
      if (!reading) {
        reading = true;
        if (isAsync) nextAsync();
        else if (isAsyncValues) nextSyncWithAsyncValues();
        else nextSyncWithSyncValues();
      }
    };
    readable._destroy = function (error, cb) {
      originalDestroy.call(this, error, (destroyError) => {
        const combinedError = destroyError || error;
        close(combinedError).then(
          () => process.nextTick(cb, combinedError),
          (closeError) => process.nextTick(cb, aggregateTwoErrors(combinedError, closeError)),
        );
      });
    };
    // Lets the iterator run its finally blocks (and see the error, if any).
    async function close(error) {
      const hadError = error !== undefined && error !== null;
      if (hadError && typeof iterator.throw === "function") {
        const { value, done } = await iterator.throw(error);
        await value;
        if (done) return;
      }
      if (typeof iterator.return === "function") {
        const { value } = await iterator.return();
        await value;
      }
    }
    function nextSyncWithSyncValues() {
      for (;;) {
        try {
          const { value, done } = iterator.next();
          if (done) {
            readable.push(null);
            return;
          }
          if (value && typeof value.then === "function") return changeToAsyncValues(value);
          if (value === null) {
            reading = false;
            throw errNullValues();
          }
          if (readable.push(value)) continue;
          reading = false;
        } catch (err) {
          readable.destroy(err);
        }
        break;
      }
    }
    async function changeToAsyncValues(value) {
      isAsyncValues = true;
      try {
        const res = await value;
        if (res === null) {
          reading = false;
          throw errNullValues();
        }
        if (readable.push(res)) {
          nextSyncWithAsyncValues();
          return;
        }
        reading = false;
      } catch (err) {
        readable.destroy(err);
      }
    }
    async function nextSyncWithAsyncValues() {
      for (;;) {
        try {
          const { value, done } = iterator.next();
          if (done) {
            readable.push(null);
            return;
          }
          const res = value && typeof value.then === "function" ? await value : value;
          if (res === null) {
            reading = false;
            throw errNullValues();
          }
          if (readable.push(res)) continue;
          reading = false;
        } catch (err) {
          readable.destroy(err);
        }
        break;
      }
    }
    async function nextAsync() {
      for (;;) {
        try {
          const { value, done } = await iterator.next();
          if (done) {
            readable.push(null);
            return;
          }
          if (value === null) {
            reading = false;
            throw errNullValues();
          }
          if (readable.push(value)) continue;
          reading = false;
        } catch (err) {
          readable.destroy(err);
        }
        break;
      }
    }
    return readable;
  }

  // ---- Writable -----------------------------------------------------------------
  function WritableState(options, stream, isDuplex) {
    this.objectMode = !!(options && (options.objectMode || (isDuplex && options.writableObjectMode)));
    // Where write() starts returning false; 0 means whenever not flushed at once.
    this.highWaterMark = options ? getHighWaterMark(this, options, "writableHighWaterMark", isDuplex) : getDefaultHighWaterMark(false);
    this.finalCalled = false;
    this.needDrain = false;
    // ending: end() was called; ended: it returned; finished: 'finish' was emitted.
    this.ending = false;
    this.ended = false;
    this.finished = false;
    this.destroyed = false;
    // Strings become Buffers before _write unless decodeStrings is false.
    this.decodeStrings = !(options && options.decodeStrings === false);
    const defaultEncoding = options ? options.defaultEncoding : null;
    if (defaultEncoding == null || defaultEncoding === "utf8" || defaultEncoding === "utf-8") this.defaultEncoding = "utf8";
    else if (Buffer.isEncoding(defaultEncoding)) this.defaultEncoding = defaultEncoding;
    else throw errUnknownEncoding(defaultEncoding);
    // Bytes (or objects) written but not yet through _write.
    this.length = 0;
    this.writing = false;
    this.corked = 0;
    // Whether the _write callback came back inside _write.
    this.sync = true;
    this.bufferProcessing = false;
    this.onwrite = (er) => onwrite(stream, er);
    this.writecb = null;
    this.writelen = 0;
    // A synchronously completed write's callbacks, batched into one tick.
    this.afterWriteTickInfo = null;
    this.afterWritePending = false;
    this.expectWriteCb = false;
    this.buffered = [];
    this.bufferedIndex = 0;
    this.allBuffers = true;
    this.allNoop = true;
    // Write callbacks still owed: 'finish' waits for 0.
    this.pendingcb = 0;
    this.constructed = true;
    this.prefinished = false;
    this.errorEmitted = false;
    this.emitClose = !(options && options.emitClose === false);
    this.autoDestroy = !(options && options.autoDestroy === false);
    this.errored = null;
    this.closed = false;
    this.closeEmitted = false;
    this[kOnFinished] = null;
  }
  function resetBuffer(state) {
    state.buffered = [];
    state.bufferedIndex = 0;
    state.allBuffers = true;
    state.allNoop = true;
  }
  WritableState.prototype.getBuffer = function getBuffer() {
    return this.buffered.slice(this.bufferedIndex);
  };
  ObjectDefineProperty(WritableState.prototype, "bufferedRequestCount", {
    get() { return this.buffered.length - this.bufferedIndex; },
  });
  function writableOnConstructed(stream, state) {
    if (!state.writing) clearBuffer(stream, state);
    if (state.ending) finishMaybe(stream, state);
  }

  function Writable(options) {
    if (!(this instanceof Writable)) return new Writable(options);
    this._writableState = new WritableState(options, this, false);
    if (options) {
      if (typeof options.write === "function") this._write = options.write;
      if (typeof options.writev === "function") this._writev = options.writev;
      if (typeof options.destroy === "function") this._destroy = options.destroy;
      if (typeof options.final === "function") this._final = options.final;
      if (typeof options.construct === "function") this._construct = options.construct;
      if (options.signal) addAbortSignal(options.signal, this);
    }
    Stream.call(this, options);
    if (this._construct != null) {
      construct(this, () => {
        writableOnConstructed(this, this._writableState);
      });
    }
  }
  ObjectSetPrototypeOf(Writable.prototype, Stream.prototype);
  ObjectSetPrototypeOf(Writable, Stream);
  Writable.WritableState = WritableState;
  // A Duplex is a Writable too, though it inherits from Readable.
  ObjectDefineProperty(Writable, Symbol.hasInstance, {
    value: function (object) {
      if (Function.prototype[Symbol.hasInstance].call(this, object)) return true;
      if (this !== Writable) return false;
      return !!object && object._writableState instanceof WritableState;
    },
  });
  Writable.prototype.pipe = function () {
    errorOrDestroy(this, errCannotPipe());
  };
  function _write(stream, chunk, encoding, cb) {
    const state = stream._writableState;
    if (cb == null || typeof cb !== "function") cb = nop;
    if (chunk === null) throw errNullValues();
    if (!state.objectMode) {
      if (!encoding) encoding = state.defaultEncoding;
      else if (encoding !== "buffer" && !Buffer.isEncoding(encoding)) throw errUnknownEncoding(encoding);
      if (typeof chunk === "string") {
        if (state.decodeStrings) {
          chunk = Buffer.from(chunk, encoding);
          encoding = "buffer";
        }
      } else if (chunk instanceof Buffer) {
        encoding = "buffer";
      } else if (ArrayBuffer.isView(chunk)) {
        chunk = toBuffer(chunk);
        encoding = "buffer";
      } else {
        throw errInvalidArgType("chunk", kChunkTypes, chunk);
      }
    }
    let err;
    if (state.ending) err = errWriteAfterEnd();
    else if (state.destroyed) err = errDestroyed("write");
    if (err) {
      process.nextTick(cb, err);
      errorOrDestroy(stream, err, true);
      return err;
    }
    state.pendingcb++;
    return writeOrBuffer(stream, state, chunk, encoding, cb);
  }
  Writable.prototype.write = function (chunk, encoding, cb) {
    if (encoding != null && typeof encoding === "function") {
      cb = encoding;
      encoding = null;
    }
    return _write(this, chunk, encoding, cb) === true;
  };
  Writable.prototype.cork = function () {
    this._writableState.corked++;
  };
  Writable.prototype.uncork = function () {
    const state = this._writableState;
    if (state.corked) {
      state.corked--;
      if (!state.writing) clearBuffer(this, state);
    }
  };
  Writable.prototype.setDefaultEncoding = function setDefaultEncoding(encoding) {
    if (typeof encoding === "string") encoding = encoding.toLowerCase();
    if (!Buffer.isEncoding(encoding)) throw errUnknownEncoding(encoding);
    this._writableState.defaultEncoding = encoding === "utf-8" ? "utf8" : encoding;
    return this;
  };
  // Straight to _write when idle; queued behind the write in flight (or the
  // cork, or construction) otherwise. False means wait for 'drain'.
  function writeOrBuffer(stream, state, chunk, encoding, callback) {
    const len = state.objectMode ? 1 : chunk.length;
    state.length += len;
    if (state.writing || state.errored || state.corked || !state.constructed) {
      state.buffered.push({ chunk, encoding, callback });
      if (state.allBuffers && encoding !== "buffer") state.allBuffers = false;
      if (state.allNoop && callback !== nop) state.allNoop = false;
    } else {
      state.writelen = len;
      if (callback !== nop) state.writecb = callback;
      state.writing = state.sync = state.expectWriteCb = true;
      stream._write(chunk, encoding, state.onwrite);
      state.sync = false;
    }
    const ret = state.length < state.highWaterMark || state.length === 0;
    if (!ret) state.needDrain = true;
    // False once errored or destroyed, to break `while (write())` loops.
    return ret && !state.destroyed && !state.errored;
  }
  function doWrite(stream, state, writev, len, chunk, encoding, cb) {
    state.writelen = len;
    if (cb !== nop) state.writecb = cb;
    state.writing = state.sync = state.expectWriteCb = true;
    if (state.destroyed) state.onwrite(errDestroyed("write"));
    else if (writev) stream._writev(chunk, state.onwrite);
    else stream._write(chunk, encoding, state.onwrite);
    state.sync = false;
  }
  function onwriteError(stream, state, er, cb) {
    --state.pendingcb;
    cb(er);
    // The queued writes' callbacks fail too, then the error is emitted.
    errorBuffer(state);
    errorOrDestroy(stream, er);
  }
  // The callback _write gets.
  function onwrite(stream, er) {
    const state = stream._writableState;
    if (!state.expectWriteCb) {
      errorOrDestroy(stream, errMultipleCallback());
      return;
    }
    const sync = state.sync;
    const cb = state.writecb || nop;
    state.writecb = null;
    state.writing = state.expectWriteCb = false;
    state.length -= state.writelen;
    state.writelen = 0;
    if (er) {
      if (!state.errored) state.errored = er;
      // A duplex's readable side learns of it too.
      if (stream._readableState && !stream._readableState.errored) stream._readableState.errored = er;
      if (sync) process.nextTick(onwriteError, stream, state, er, cb);
      else onwriteError(stream, state, er, cb);
    } else {
      if (state.buffered.length > 0) clearBuffer(stream, state);
      if (sync) {
        const needDrain = state.needDrain && state.length === 0;
        // Node writes this test as `state & kDestroyed !== 0`, which by
        // precedence is its objectMode bit; kept, as it decides the tick
        // 'finish' lands on.
        const needTick = needDrain || state.objectMode || cb !== nop;
        if (cb === nop) {
          if (!state.afterWritePending && needTick) {
            process.nextTick(afterWrite, stream, state, 1, cb);
            state.afterWritePending = true;
          } else {
            state.pendingcb--;
            if (state.ending) finishMaybe(stream, state, true);
          }
        } else if (state.afterWriteTickInfo !== null && state.afterWriteTickInfo.cb === cb) {
          // The same callback as the pending batch: count it in.
          state.afterWriteTickInfo.count++;
        } else if (needTick) {
          state.afterWriteTickInfo = { count: 1, cb, stream, state };
          process.nextTick(afterWriteTick, state.afterWriteTickInfo);
          state.afterWritePending = true;
        } else {
          state.pendingcb--;
          if (state.ending) finishMaybe(stream, state, true);
        }
      } else {
        afterWrite(stream, state, 1, cb);
      }
    }
  }
  function afterWriteTick({ stream, state, count, cb }) {
    state.afterWriteTickInfo = null;
    return afterWrite(stream, state, count, cb);
  }
  function afterWrite(stream, state, count, cb) {
    state.afterWritePending = false;
    if (!state.ending && state.needDrain && !state.destroyed && state.length === 0) {
      state.needDrain = false;
      stream.emit("drain");
    }
    while (count-- > 0) {
      state.pendingcb--;
      cb(null);
    }
    if (state.destroyed) errorBuffer(state);
    if (state.ending) finishMaybe(stream, state, true);
  }
  // Fails every queued write's callback (and end()'s).
  function errorBuffer(state) {
    if (state.writing) return;
    const buffered = state.buffered;
    for (let n = state.bufferedIndex; n < buffered.length; ++n) {
      const { chunk, callback } = buffered[n];
      state.length -= state.objectMode ? 1 : chunk.length;
      callback(state.errored ?? errDestroyed("write"));
    }
    callFinishedCallbacks(state, state.errored ?? errDestroyed("end"));
    resetBuffer(state);
  }
  // Writes out the queue: all at once through _writev when there is one,
  // else one by one for as long as each _write completes synchronously.
  function clearBuffer(stream, state) {
    if (state.destroyed || state.bufferProcessing || state.corked || state.buffered.length === 0 || !state.constructed) return;
    const objectMode = state.objectMode;
    const buffered = state.buffered;
    const bufferedLength = buffered.length - state.bufferedIndex;
    if (!bufferedLength) return;
    let i = state.bufferedIndex;
    state.bufferProcessing = true;
    if (bufferedLength > 1 && stream._writev) {
      state.pendingcb -= bufferedLength - 1;
      const callback = state.allNoop ? nop : (err) => {
        for (let n = i; n < buffered.length; ++n) buffered[n].callback(err);
      };
      const chunks = state.allNoop && i === 0 ? buffered : buffered.slice(i);
      chunks.allBuffers = state.allBuffers;
      doWrite(stream, state, true, state.length, chunks, "", callback);
      resetBuffer(state);
    } else {
      do {
        const { chunk, encoding, callback } = buffered[i];
        buffered[i++] = null;
        doWrite(stream, state, false, objectMode ? 1 : chunk.length, chunk, encoding, callback);
      } while (i < buffered.length && !state.writing);
      if (i === buffered.length) {
        resetBuffer(state);
      } else if (i > 256) {
        buffered.splice(0, i);
        state.bufferedIndex = 0;
      } else {
        state.bufferedIndex = i;
      }
    }
    state.bufferProcessing = false;
  }
  Writable.prototype._write = function (chunk, encoding, cb) {
    if (this._writev) this._writev([{ chunk, encoding }], cb);
    else throw errNotImplemented("_write()");
  };
  Writable.prototype._writev = null;
  Writable.prototype.end = function (chunk, encoding, cb) {
    const state = this._writableState;
    if (typeof chunk === "function") {
      cb = chunk;
      chunk = null;
      encoding = null;
    } else if (typeof encoding === "function") {
      cb = encoding;
      encoding = null;
    }
    let err;
    if (chunk != null) {
      const ret = _write(this, chunk, encoding);
      if (ret instanceof Error) err = ret;
    }
    // end() uncorks fully.
    if (state.corked) {
      state.corked = 1;
      this.uncork();
    }
    if (err) {
      // Already reported.
    } else if (!state.ending && !state.errored) {
      // A second end() is forgiven rather than an error.
      state.ending = true;
      finishMaybe(this, state, true);
      state.ended = true;
    } else if (state.finished) {
      err = errAlreadyFinished("end");
    } else if (state.destroyed) {
      err = errDestroyed("end");
    }
    if (typeof cb === "function") {
      if (err) process.nextTick(cb, err);
      else if (state.errored) process.nextTick(cb, state.errored);
      else if (state.finished) process.nextTick(cb, null);
      else (state[kOnFinished] ||= []).push(cb);
    }
    return this;
  };
  // Ended, constructed and drained, not destroyed, failed or finished.
  function needFinish(state) {
    return state.ending && state.constructed && state.length === 0 && !state.destroyed && !state.finished &&
      !state.writing && !state.errorEmitted && !state.closeEmitted && !state.errored && state.buffered.length === 0;
  }
  function onFinish(stream, state, err) {
    if (state.prefinished) {
      errorOrDestroy(stream, err ?? errMultipleCallback());
      return;
    }
    state.pendingcb--;
    if (err) {
      callFinishedCallbacks(state, err);
      errorOrDestroy(stream, err, state.sync);
    } else if (needFinish(state)) {
      state.prefinished = true;
      stream.emit("prefinish");
      // 'finish' a tick after _final's callback, which packages assume.
      state.pendingcb++;
      process.nextTick(finish, stream, state);
    }
  }
  // _final if there is one, else 'prefinish' now.
  function prefinish(stream, state) {
    if (state.prefinished || state.finalCalled) return;
    if (typeof stream._final === "function" && !state.destroyed) {
      state.finalCalled = state.sync = true;
      state.pendingcb++;
      try {
        stream._final((err) => onFinish(stream, state, err));
      } catch (err) {
        onFinish(stream, state, err);
      }
      state.sync = false;
    } else {
      state.finalCalled = state.prefinished = true;
      stream.emit("prefinish");
    }
  }
  function finishMaybe(stream, state, sync) {
    if (needFinish(state)) {
      prefinish(stream, state);
      if (state.pendingcb === 0) {
        if (sync) {
          state.pendingcb++;
          process.nextTick(finishMaybeNT, stream, state);
        } else if (needFinish(state)) {
          state.pendingcb++;
          finish(stream, state);
        }
      }
    }
  }
  function finishMaybeNT(stream, state) {
    if (needFinish(state)) finish(stream, state);
    else state.pendingcb--;
  }
  function finish(stream, state) {
    state.pendingcb--;
    state.finished = true;
    callFinishedCallbacks(state, null);
    stream.emit("finish");
    if (state.autoDestroy) {
      // A duplex is destroyed once its readable side has ended too.
      const rState = stream._readableState;
      if (!rState || (rState.autoDestroy && (rState.endEmitted || rState.readable === false))) stream.destroy();
    }
  }
  function callFinishedCallbacks(state, err) {
    const callbacks = state[kOnFinished];
    if (callbacks === null) return;
    state[kOnFinished] = null;
    for (let i = 0; i < callbacks.length; i++) callbacks[i](err);
  }
  ObjectDefineProperties(Writable.prototype, {
    closed: { get() { return this._writableState ? this._writableState.closed : false; } },
    destroyed: {
      get() { return this._writableState ? this._writableState.destroyed : false; },
      set(value) {
        if (this._writableState) this._writableState.destroyed = !!value;
      },
    },
    writable: {
      get() {
        const w = this._writableState;
        // w.writable === false: a duplex built without its writable side.
        return !!w && w.writable !== false && !w.ending && !w.ended && !w.destroyed && !w.errored;
      },
      set(val) {
        if (this._writableState) this._writableState.writable = !!val;
      },
    },
    writableFinished: { get() { return this._writableState ? this._writableState.finished : false; } },
    writableObjectMode: { get() { return this._writableState ? this._writableState.objectMode : false; } },
    writableBuffer: { get() { return this._writableState && this._writableState.getBuffer(); } },
    writableEnded: { get() { return this._writableState ? this._writableState.ending : false; } },
    writableNeedDrain: {
      get() {
        const w = this._writableState;
        return w ? !w.destroyed && !w.ending && w.needDrain : false;
      },
    },
    writableHighWaterMark: { get() { return this._writableState?.highWaterMark; } },
    writableCorked: { get() { return this._writableState ? this._writableState.corked : 0; } },
    writableLength: { get() { return this._writableState?.length; } },
    errored: { get() { return this._writableState ? this._writableState.errored : null; } },
    writableAborted: {
      get() {
        const w = this._writableState;
        return w.writable !== false && (w.destroyed || !!w.errored) && !w.finished;
      },
    },
  });
  Writable.prototype.destroy = function (err, cb) {
    const state = this._writableState;
    // Queued writes and end() callbacks fail on the next tick.
    if ((state.buffered.length > 0 || state[kOnFinished] !== null) && !state.destroyed) process.nextTick(errorBuffer, state);
    destroy.call(this, err, cb);
    return this;
  };
  Writable.prototype._undestroy = undestroy;
  Writable.prototype._destroy = function (err, cb) {
    cb(err);
  };
  Writable.prototype[EE.captureRejectionSymbol] = function (err) {
    this.destroy(err);
  };
  if (typeof Symbol.asyncDispose === "symbol") {
    Writable.prototype[Symbol.asyncDispose] = async function () {
      let error;
      if (!this.destroyed) {
        error = this.writableFinished ? null : new AbortError();
        this.destroy(error);
      }
      await new Promise((resolve, reject) =>
        eos(this, (err) => (err && err.name !== "AbortError" ? reject(err) : resolve(null))));
    };
  }

  // ---- Duplex -------------------------------------------------------------------
  function Duplex(options) {
    if (!(this instanceof Duplex)) return new Duplex(options);
    this._readableState = new ReadableState(options, this, true);
    this._writableState = new WritableState(options, this, true);
    if (options) {
      this.allowHalfOpen = options.allowHalfOpen !== false;
      if (options.readable === false) {
        const r = this._readableState;
        r.readable = false;
        r.ended = r.endEmitted = true;
      }
      if (options.writable === false) {
        const w = this._writableState;
        w.writable = false;
        w.ending = w.ended = w.finished = true;
      }
      if (typeof options.read === "function") this._read = options.read;
      if (typeof options.write === "function") this._write = options.write;
      if (typeof options.writev === "function") this._writev = options.writev;
      if (typeof options.destroy === "function") this._destroy = options.destroy;
      if (typeof options.final === "function") this._final = options.final;
      if (typeof options.construct === "function") this._construct = options.construct;
      if (options.signal) addAbortSignal(options.signal, this);
    } else {
      this.allowHalfOpen = true;
    }
    Stream.call(this, options);
    if (this._construct != null) {
      construct(this, () => {
        readableOnConstructed(this, this._readableState);
        writableOnConstructed(this, this._writableState);
      });
    }
  }
  ObjectSetPrototypeOf(Duplex.prototype, Readable.prototype);
  ObjectSetPrototypeOf(Duplex, Readable);
  for (const method of Object.keys(Writable.prototype)) {
    if (!Duplex.prototype[method]) Duplex.prototype[method] = Writable.prototype[method];
  }
  Duplex.prototype.destroy = Writable.prototype.destroy;
  {
    const props = {};
    for (const name of ["writable", "writableHighWaterMark", "writableObjectMode", "writableBuffer", "writableLength",
      "writableFinished", "writableCorked", "writableEnded", "writableNeedDrain"]) {
      props[name] = Object.getOwnPropertyDescriptor(Writable.prototype, name);
    }
    props.destroyed = {
      get() {
        if (this._readableState === undefined || this._writableState === undefined) return false;
        return this._readableState.destroyed && this._writableState.destroyed;
      },
      set(value) {
        if (this._readableState && this._writableState) {
          this._readableState.destroyed = !!value;
          this._writableState.destroyed = !!value;
        }
      },
    };
    ObjectDefineProperties(Duplex.prototype, props);
  }
  // Streams as they are; iterables, strings, Buffers and promises as a
  // read-only duplex. (Node's other forms, functions and {readable,
  // writable} pairs, are not here.)
  Duplex.from = function (body) {
    if (isDuplexNodeStream(body) || isNodeStream(body)) return body;
    if (typeof body === "string" || body instanceof Buffer || isIterable(body)) {
      return from(Duplex, body, { objectMode: true, writable: false });
    }
    if (typeof body?.then === "function") {
      let d;
      body.then((val) => {
        if (val != null) d.push(val);
        d.push(null);
      }, (err) => destroyer(d, err));
      return (d = new Duplex({ objectMode: true, writable: false, read() {} }));
    }
    throw errInvalidArgType("body", "an instance of Stream, Iterable, AsyncIterable, or Promise", body);
  };

  // ---- Transform, PassThrough -----------------------------------------------
  function Transform(options) {
    if (!(this instanceof Transform)) return new Transform(options);
    // A readable hwm of 0 would never let a transform run: the writable side
    // takes the buffering instead.
    const readableHighWaterMark = options ? getHighWaterMark(this, options, "readableHighWaterMark", true) : null;
    if (readableHighWaterMark === 0) {
      options = { ...options, highWaterMark: null, readableHighWaterMark, writableHighWaterMark: options.writableHighWaterMark || 0 };
    }
    Duplex.call(this, options);
    // What _transform pushes is emitted at once, not deferred like a _read push.
    this._readableState.sync = false;
    this[kCallback] = null;
    if (options) {
      if (typeof options.transform === "function") this._transform = options.transform;
      if (typeof options.flush === "function") this._flush = options.flush;
    }
    this.on("prefinish", transformPrefinish);
  }
  ObjectSetPrototypeOf(Transform.prototype, Duplex.prototype);
  ObjectSetPrototypeOf(Transform, Duplex);
  // _flush, then the end of the readable side.
  function transformFinal(cb) {
    if (typeof this._flush === "function" && !this.destroyed) {
      this._flush((er, data) => {
        if (er) {
          if (cb) cb(er);
          else this.destroy(er);
          return;
        }
        if (data != null) this.push(data);
        this.push(null);
        if (cb) cb();
      });
    } else {
      this.push(null);
      if (cb) cb();
    }
  }
  // A subclass with its own _final still gets the flush, on 'prefinish'.
  function transformPrefinish() {
    if (this._final !== transformFinal) transformFinal.call(this);
  }
  Transform.prototype._final = transformFinal;
  Transform.prototype._transform = function (chunk, encoding, callback) {
    throw errNotImplemented("_transform()");
  };
  Transform.prototype._write = function (chunk, encoding, callback) {
    const rState = this._readableState;
    const wState = this._writableState;
    const length = rState.length;
    this._transform(chunk, encoding, (err, val) => {
      if (err) {
        callback(err);
        return;
      }
      if (val != null) this.push(val);
      if (rState.ended) {
        process.nextTick(callback);
      } else if (wState.ended || length === rState.length || rState.length < rState.highWaterMark) {
        callback();
      } else {
        // The readable side is full: the next _read lets the write finish.
        this[kCallback] = callback;
      }
    });
  };
  Transform.prototype._read = function () {
    if (this[kCallback]) {
      const callback = this[kCallback];
      this[kCallback] = null;
      callback();
    }
  };
  function PassThrough(options) {
    if (!(this instanceof PassThrough)) return new PassThrough(options);
    Transform.call(this, options);
  }
  ObjectSetPrototypeOf(PassThrough.prototype, Transform.prototype);
  ObjectSetPrototypeOf(PassThrough, Transform);
  PassThrough.prototype._transform = function (chunk, encoding, cb) {
    cb(null, chunk);
  };

  // ---- pipeline -----------------------------------------------------------------
  // A stage's end-of-stream watch, and how to tear it down on failure.
  function pipelineDestroyer(stream, reading, writing) {
    let done = false;
    stream.on("close", () => {
      done = true;
    });
    const cleanup = eos(stream, { readable: reading, writable: writing }, (err) => {
      done = !err;
    });
    return {
      destroy: (err) => {
        if (done) return;
        done = true;
        destroyer(stream, err || errDestroyed("pipe"));
      },
      cleanup,
    };
  }
  function makeAsyncIterable(val) {
    if (isIterable(val)) return val;
    if (isReadableNodeStream(val)) return fromReadable(val);
    throw errInvalidArgType("val", "an instance of Readable, Iterable, or AsyncIterable", val);
  }
  async function* fromReadable(val) {
    yield* Readable.prototype[Symbol.asyncIterator].call(val);
  }
  // Writes an (async) iterable into a writable, honouring backpressure.
  async function pumpToNode(iterable, writable, finish, { end }) {
    let error;
    let onresolve = null;
    const resume = (err) => {
      if (err) error = err;
      if (onresolve) {
        const callback = onresolve;
        onresolve = null;
        callback();
      }
    };
    const wait = () => new Promise((resolve, reject) => {
      if (error) {
        reject(error);
      } else {
        onresolve = () => {
          if (error) reject(error);
          else resolve();
        };
      }
    });
    writable.on("drain", resume);
    const cleanup = eos(writable, { readable: false }, resume);
    try {
      if (writable.writableNeedDrain) await wait();
      for await (const chunk of iterable) {
        if (!writable.write(chunk)) await wait();
      }
      if (end) {
        writable.end();
        await wait();
      }
      finish();
    } catch (err) {
      finish(error !== err ? aggregateTwoErrors(error, err) : err);
    } finally {
      cleanup();
      writable.off("drain", resume);
    }
  }
  function pipeline(...streams) {
    const last = streams[streams.length - 1];
    validateFunction(last, "streams[stream.length - 1]");
    streams.pop();
    return pipelineImpl(streams, once(last));
  }
  function pipelineImpl(streams, callback, opts) {
    if (streams.length === 1 && ArrayIsArray(streams[0])) streams = streams[0];
    if (streams.length < 2) throw errMissingArgs("streams");
    const ac = newAbortController();
    const signal = ac.signal;
    const outerSignal = opts?.signal;
    const lastStreamCleanup = [];
    validateAbortSignal(outerSignal, "options.signal");
    function abort() {
      finishImpl(new AbortError(undefined, { cause: outerSignal?.reason }));
    }
    const disposeAbort = outerSignal ? addAbortListener(outerSignal, abort) : nop;
    let error;
    let value;
    const destroys = [];
    let finishCount = 0;
    function finish(err) {
      finishImpl(err, --finishCount === 0);
    }
    function finishOnlyHandleError(err) {
      finishImpl(err, false);
    }
    // The first real error wins; on any error every stage is destroyed.
    function finishImpl(err, final) {
      if (err && (!error || error.code === "ERR_STREAM_PREMATURE_CLOSE" || error.name === "AbortError")) error = err;
      if (!error && !final) return;
      while (destroys.length) destroys.shift()(error);
      disposeAbort();
      ac.abort();
      if (final) {
        if (!error) lastStreamCleanup.forEach((fn) => fn());
        process.nextTick(callback, error, value);
      }
    }
    let ret;
    for (let i = 0; i < streams.length; i++) {
      const stream = streams[i];
      const reading = i < streams.length - 1;
      const writing = i > 0;
      const next = i + 1 < streams.length ? streams[i + 1] : null;
      const end = reading || opts?.end !== false;
      const isLastStream = i === streams.length - 1;
      if (isNodeStream(stream)) {
        if (next !== null && (next?.closed || next?.destroyed)) throw errUnableToPipe();
        if (end) {
          const { destroy, cleanup } = pipelineDestroyer(stream, reading, writing);
          destroys.push(destroy);
          if (isReadable(stream) && isLastStream) lastStreamCleanup.push(cleanup);
        }
        // Errors are taken here; premature closes and aborts come via eos.
        function onError(err) {
          if (err && err.name !== "AbortError" && err.code !== "ERR_STREAM_PREMATURE_CLOSE") finishOnlyHandleError(err);
        }
        stream.on("error", onError);
        if (isReadable(stream) && isLastStream) {
          lastStreamCleanup.push(() => {
            stream.removeListener("error", onError);
          });
        }
      }
      if (i === 0) {
        if (typeof stream === "function") {
          ret = stream({ signal });
          if (!isIterable(ret)) throw errInvalidReturnValue("Iterable, AsyncIterable or Stream", "source", ret);
        } else if (isIterable(stream) || isReadableNodeStream(stream)) {
          ret = stream;
        } else {
          ret = Duplex.from(stream);
        }
      } else if (typeof stream === "function") {
        // A generator stage: it takes the source as an async iterable.
        ret = stream(makeAsyncIterable(ret), { signal });
        if (reading) {
          if (!isIterable(ret, true)) throw errInvalidReturnValue("AsyncIterable", "transform[" + (i - 1) + "]", ret);
        } else {
          // The last stage: its result (a promise or an iterable) ends up in
          // a PassThrough that the pipeline returns.
          const pt = new PassThrough({ objectMode: true });
          const then = ret?.then;
          if (typeof then === "function") {
            finishCount++;
            then.call(ret, (val) => {
              value = val;
              if (val != null) pt.write(val);
              if (end) pt.end();
              process.nextTick(finish);
            }, (err) => {
              pt.destroy(err);
              process.nextTick(finish, err);
            });
          } else if (isIterable(ret, true)) {
            finishCount++;
            pumpToNode(ret, pt, finish, { end });
          } else {
            throw errInvalidReturnValue("AsyncIterable or Promise", "destination", ret);
          }
          ret = pt;
          const { destroy, cleanup } = pipelineDestroyer(ret, false, true);
          destroys.push(destroy);
          if (isLastStream) lastStreamCleanup.push(cleanup);
        }
      } else if (isNodeStream(stream)) {
        if (isReadableNodeStream(ret)) {
          finishCount += 2;
          const cleanup = pipe(ret, stream, finish, finishOnlyHandleError, { end });
          if (isReadable(stream) && isLastStream) lastStreamCleanup.push(cleanup);
        } else if (isIterable(ret)) {
          finishCount++;
          pumpToNode(ret, stream, finish, { end });
        } else {
          throw errInvalidArgType("val", "an instance of Readable, Iterable, AsyncIterable, ReadableStream, or TransformStream", ret);
        }
        ret = stream;
      } else {
        ret = Duplex.from(stream);
      }
    }
    if (signal?.aborted || outerSignal?.aborted) process.nextTick(abort);
    return ret;
  }
  // One src→dst link of a pipeline; dst is ended here (not by pipe) so the
  // 'end' can be told from a premature close.
  function pipe(src, dst, finish, finishOnlyHandleError, { end }) {
    let ended = false;
    dst.on("close", () => {
      if (!ended) finishOnlyHandleError(errPrematureClose());
    });
    src.pipe(dst, { end: false });
    if (end) {
      function endFn() {
        ended = true;
        dst.end();
      }
      if (isReadableFinished(src)) process.nextTick(endFn);
      else src.once("end", endFn);
    } else {
      finish();
    }
    eos(src, { readable: true, writable: false }, (err) => {
      const rState = src._readableState;
      if (err && err.code === "ERR_STREAM_PREMATURE_CLOSE" && rState?.ended && !rState.errored && !rState.errorEmitted) {
        // Ended but closed before 'end' was emitted: wait for it.
        src.once("end", finish).once("error", finish);
      } else {
        finish(err);
      }
    });
    return eos(dst, { readable: false, writable: true }, finish);
  }
  function pipelinePromise(...streams) {
    return new Promise((resolve, reject) => {
      let signal, end;
      const lastArg = streams[streams.length - 1];
      if (lastArg && typeof lastArg === "object" && !isNodeStream(lastArg) && !isIterable(lastArg)) {
        const options = streams.pop();
        signal = options.signal;
        end = options.end;
      }
      pipelineImpl(streams, (err, value) => {
        if (err) reject(err);
        else resolve(value);
      }, { signal, end });
    });
  }

  // ---- operators (readable.map(), .toArray(), ...) --------------------------
  // Sequential: a `concurrency` option is accepted but items are handled one at a time.
  const streamOperators = {
    map(fn, options) {
      validateFunction(fn, "fn");
      return (async function* map() {
        for await (const val of this) yield await fn(val, { signal: options?.signal });
      }).call(this);
    },
    filter(fn, options) {
      validateFunction(fn, "fn");
      return (async function* filter() {
        for await (const val of this) if (await fn(val, { signal: options?.signal })) yield val;
      }).call(this);
    },
    flatMap(fn, options) {
      validateFunction(fn, "fn");
      return (async function* flatMap() {
        for await (const val of this) yield* await fn(val, { signal: options?.signal });
      }).call(this);
    },
    drop(number) {
      number = Math.trunc(number) || 0;
      return (async function* drop() {
        for await (const val of this) if (number-- <= 0) yield val;
      }).call(this);
    },
    take(number) {
      number = Math.trunc(number) || 0;
      return (async function* take() {
        if (number <= 0) return;
        for await (const val of this) {
          if (number-- > 0) yield val;
          if (number <= 0) return;
        }
      }).call(this);
    },
  };
  const promiseOperators = {
    async forEach(fn, options) {
      validateFunction(fn, "fn");
      for await (const val of this) await fn(val, { signal: options?.signal });
    },
    async toArray() {
      const result = [];
      for await (const val of this) result.push(val);
      return result;
    },
    async some(fn, options) {
      validateFunction(fn, "fn");
      for await (const val of this) if (await fn(val, { signal: options?.signal })) return true;
      return false;
    },
    async every(fn, options) {
      validateFunction(fn, "fn");
      for await (const val of this) if (!(await fn(val, { signal: options?.signal }))) return false;
      return true;
    },
    async find(fn, options) {
      validateFunction(fn, "fn");
      for await (const val of this) if (await fn(val, { signal: options?.signal })) return val;
      return undefined;
    },
    async reduce(reducer, initialValue, options) {
      validateFunction(reducer, "reducer");
      let hasInitialValue = arguments.length > 1;
      for await (const val of this) {
        if (!hasInitialValue) {
          initialValue = val;
          hasInitialValue = true;
        } else {
          initialValue = await reducer(initialValue, val, { signal: options?.signal });
        }
      }
      if (!hasInitialValue) throw nodeError(TypeError, "ERR_MISSING_ARGS", "Reduce of an empty stream requires an initial value");
      return initialValue;
    },
  };
  for (const [key, op] of Object.entries(streamOperators)) {
    ObjectDefineProperty(Readable.prototype, key, {
      value: { [key](...args) { return Readable.from(Reflect.apply(op, this, args)); } }[key],
      enumerable: false,
      configurable: true,
      writable: true,
    });
  }
  for (const [key, op] of Object.entries(promiseOperators)) {
    ObjectDefineProperty(Readable.prototype, key, { value: op, enumerable: false, configurable: true, writable: true });
  }

  // ---- the module ---------------------------------------------------------------
  const promises = { pipeline: pipelinePromise, finished };
  Stream.isDestroyed = isDestroyed;
  Stream.isDisturbed = isDisturbed;
  Stream.isErrored = isErrored;
  Stream.isReadable = isReadable;
  Stream.isWritable = isWritable;
  Stream.Readable = Readable;
  Stream.Writable = Writable;
  Stream.Duplex = Duplex;
  Stream.Transform = Transform;
  Stream.PassThrough = PassThrough;
  Stream.pipeline = pipeline;
  Stream.addAbortSignal = addAbortSignal;
  Stream.finished = eos;
  Stream.destroy = destroyer;
  Stream.setDefaultHighWaterMark = setDefaultHighWaterMark;
  Stream.getDefaultHighWaterMark = getDefaultHighWaterMark;
  ObjectDefineProperty(Stream, "promises", { configurable: true, enumerable: true, get: () => promises });
  ObjectDefineProperty(pipeline, kPromisifyCustom, { enumerable: true, get: () => promises.pipeline });
  ObjectDefineProperty(eos, kPromisifyCustom, { enumerable: true, get: () => promises.finished });
  Stream.Stream = Stream;
  Stream._isArrayBufferView = ArrayBuffer.isView;
  Stream._isUint8Array = (v) => v instanceof Uint8Array;
  Stream._uint8ArrayToBuffer = toBuffer;
  return Stream;
})
