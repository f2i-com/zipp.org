// node:readline — lines from a stream: createInterface with its 'line' and
// 'close' events, question(), prompts, async iteration over the lines, the
// promise form (`promises`, which is readline/promises), keypress events and
// the cursor helpers. Lines split as Node splits them: at `\n`, `\r\n`, a
// lone `\r`, U+2028 and U+2029, with a `\r\n` across two chunks counted once
// (inside crlfDelay). A terminal interface reads lines the same way; line
// editing and echo are left to the terminal rather than done in raw mode.
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
  const EventEmitter = builtin("events");
  const ObjectDefineProperty = Object.defineProperty;
  const kPromisifyCustom = Symbol.for("nodejs.util.promisify.custom");
  const lineEnding = /\r?\n|\r(?!\n)|\u2028|\u2029/g;
  const kMinCrlfDelay = 100;
  const kHistorySize = 30;
  const kPrompt = Symbol("_prompt");
  const kOldPrompt = Symbol("_oldPrompt");
  const kQuestionCallback = Symbol("_questionCallback");
  const kLineBuffer = Symbol("_line_buffer");
  const kSawReturnAt = Symbol("_sawReturnAt");
  const kDecoder = Symbol("_decoder");
  const kLineIterator = Symbol("line object stream");
  let StringDecoder;
  const newDecoder = () => new (StringDecoder ||= builtin("string_decoder").StringDecoder)("utf8");

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
  const errUseAfterClose = () => nodeError(Error, "ERR_USE_AFTER_CLOSE", "readline was closed");
  const errInvalidArgValue = (name, v) =>
    nodeError(TypeError, "ERR_INVALID_ARG_VALUE", "The argument '" + name + "' is invalid. Received " + String(v));
  class AbortError extends Error {
    constructor(message = "The operation was aborted", options = undefined) {
      super(message, options);
      this.code = "ABORT_ERR";
      this.name = "AbortError";
    }
  }
  function onAbort(signal, listener) {
    signal.addEventListener("abort", listener, { once: true });
    return () => signal.removeEventListener("abort", listener);
  }

  // ---- Interface ----------------------------------------------------------------
  // (input, output, completer, terminal), or ({ input, output, ... }).
  function InterfaceConstructor(input, output, completer, terminal) {
    this[kSawReturnAt] = 0;
    this.isCompletionEnabled = true;
    this.escapeCodeTimeout = 500;
    this.tabSize = 8;
    EventEmitter.call(this);
    let crlfDelay;
    let prompt = "> ";
    let signal;
    let historySize = kHistorySize;
    let history = [];
    if (input?.input) {
      output = input.output;
      completer = input.completer;
      terminal = input.terminal;
      signal = input.signal;
      if (input.historySize !== undefined) {
        const size = input.historySize;
        if (typeof size !== "number") {
          throw nodeError(TypeError, "ERR_INVALID_ARG_TYPE", 'The "size" argument must be of type number. Received type ' +
            typeof size + " (" + (typeof size === "string" ? "'" + size + "'" : String(size)) + ")");
        }
        if (!(size >= 0)) {
          throw nodeError(RangeError, "ERR_OUT_OF_RANGE", 'The value of "size" is out of range. It must be >= 0. Received ' + size);
        }
        historySize = size;
      }
      if (Array.isArray(input.history)) history = input.history;
      if (input.tabSize !== undefined) this.tabSize = input.tabSize;
      if (input.prompt !== undefined) prompt = input.prompt;
      if (input.escapeCodeTimeout !== undefined) {
        if (!Number.isFinite(input.escapeCodeTimeout)) throw errInvalidArgValue("input.escapeCodeTimeout", input.escapeCodeTimeout);
        this.escapeCodeTimeout = input.escapeCodeTimeout;
      }
      crlfDelay = input.crlfDelay;
      input = input.input;
    }
    if (completer !== undefined && typeof completer !== "function") throw errInvalidArgValue("completer", completer);
    if (terminal === undefined && output != null) terminal = !!output.isTTY;
    this.history = history;
    this.historyIndex = -1;
    this.historySize = historySize;
    this.line = "";
    this.output = output;
    this.input = input;
    this.crlfDelay = crlfDelay ? Math.max(kMinCrlfDelay, crlfDelay) : kMinCrlfDelay;
    this.completer = completer;
    this.setPrompt(prompt);
    this.terminal = !!terminal;
    if (this.terminal) this.cursor = 0;
    this[kQuestionCallback] = null;
    this[kLineBuffer] = null;
    this[kLineIterator] = undefined;
    this[kDecoder] = newDecoder();
    const self = this;
    function onerror(err) {
      self.emit("error", err);
    }
    function ondata(data) {
      self._normalWrite(data);
    }
    // The last line need not end in a newline.
    function onend() {
      if (typeof self[kLineBuffer] === "string" && self[kLineBuffer].length > 0) self.emit("line", self[kLineBuffer]);
      self.close();
    }
    input.on("error", onerror);
    input.on("data", ondata);
    input.on("end", onend);
    this.once("close", () => {
      input.removeListener("data", ondata);
      input.removeListener("error", onerror);
      input.removeListener("end", onend);
    });
    if (signal) {
      const onAborted = () => self.close();
      if (signal.aborted) process.nextTick(onAborted);
      else this.once("close", onAbort(signal, onAborted));
    }
    if (typeof input.resume === "function") input.resume();
  }
  Object.setPrototypeOf(InterfaceConstructor.prototype, EventEmitter.prototype);
  Object.setPrototypeOf(InterfaceConstructor, EventEmitter);
  const proto = InterfaceConstructor.prototype;
  ObjectDefineProperty(proto, "columns", {
    configurable: true,
    get() { return this.output?.columns || Infinity; },
  });
  proto.setPrompt = function (prompt) {
    this[kPrompt] = prompt;
  };
  proto.getPrompt = function () {
    return this[kPrompt];
  };
  proto.prompt = function (preserveCursor) {
    if (this.paused) this.resume();
    if (this.terminal && !preserveCursor) this.cursor = 0;
    this._writeToOutput(this.terminal ? this[kPrompt] + this.line : this[kPrompt]);
  };
  // question()'s prompt replaces the set one until the answer comes.
  proto._question = function (query, cb) {
    if (this.closed) throw errUseAfterClose();
    if (this[kQuestionCallback]) {
      this.prompt();
    } else {
      this[kOldPrompt] = this[kPrompt];
      this.setPrompt(query);
      this[kQuestionCallback] = cb;
      this.prompt();
    }
  };
  proto._questionCancel = function () {
    if (this[kQuestionCallback]) {
      this[kQuestionCallback] = null;
      this.setPrompt(this[kOldPrompt]);
      this.clearLine();
    }
  };
  // A line answers a pending question, or is emitted.
  proto._onLine = function (line) {
    if (this[kQuestionCallback]) {
      const cb = this[kQuestionCallback];
      this[kQuestionCallback] = null;
      this.setPrompt(this[kOldPrompt]);
      cb(line);
    } else {
      this.emit("line", line);
    }
  };
  proto._writeToOutput = function (stringToWrite) {
    if (typeof stringToWrite !== "string") {
      throw nodeError(TypeError, "ERR_INVALID_ARG_TYPE", 'The "stringToWrite" argument must be of type string.');
    }
    if (this.output !== null && this.output !== undefined) this.output.write(stringToWrite);
  };
  proto.clearLine = function () {
    this._writeToOutput("\r\n");
    this.line = "";
    if (this.terminal) this.cursor = 0;
  };
  proto.getCursorPos = function () {
    const before = this[kPrompt] + this.line.slice(0, this.terminal ? this.cursor : this.line.length);
    const cols = this.columns;
    return cols === Infinity ? { rows: 0, cols: before.length } : { rows: Math.floor(before.length / cols), cols: before.length % cols };
  };
  proto.close = function () {
    if (this.closed) return;
    this.pause();
    this.closed = true;
    this.emit("close");
  };
  proto.pause = function () {
    if (this.closed) throw errUseAfterClose();
    if (this.paused) return;
    if (typeof this.input.pause === "function") this.input.pause();
    this.paused = true;
    this.emit("pause");
    return this;
  };
  proto.resume = function () {
    if (this.closed) throw errUseAfterClose();
    if (!this.paused) return;
    if (typeof this.input.resume === "function") this.input.resume();
    this.paused = false;
    this.emit("resume");
    return this;
  };
  // As if `d` had been read from the input (and, on a terminal, typed).
  proto.write = function (d, key) {
    if (this.closed) throw errUseAfterClose();
    if (this.paused) this.resume();
    if (this.terminal && typeof d === "string") this._writeToOutput(d);
    this._normalWrite(d);
  };
  proto._normalWrite = function (b) {
    if (b === undefined) return;
    let string = this[kDecoder].write(b);
    // A \r that ended the last chunk already ended the line: skip its \n.
    if (this[kSawReturnAt] && Date.now() - this[kSawReturnAt] <= this.crlfDelay) {
      if (string.codePointAt(0) === 10) string = string.slice(1);
      this[kSawReturnAt] = 0;
    }
    let newPartContainsEnding = lineEnding.exec(string);
    if (newPartContainsEnding !== null) {
      if (this[kLineBuffer]) {
        string = this[kLineBuffer] + string;
        this[kLineBuffer] = null;
        lineEnding.lastIndex = 0;
        newPartContainsEnding = lineEnding.exec(string);
      }
      this[kSawReturnAt] = string.endsWith("\r") ? Date.now() : 0;
      const indexes = [0, newPartContainsEnding.index, lineEnding.lastIndex];
      let nextMatch;
      while ((nextMatch = lineEnding.exec(string)) !== null) indexes.push(nextMatch.index, lineEnding.lastIndex);
      const lastIndex = indexes.length - 1;
      this[kLineBuffer] = string.slice(indexes[lastIndex]);
      for (let i = 1; i < lastIndex; i += 2) this._onLine(string.slice(indexes[i - 1], indexes[i]));
    } else if (string) {
      this[kLineBuffer] = this[kLineBuffer] ? this[kLineBuffer] + string : string;
    }
  };
  proto[Symbol.asyncIterator] = function () {
    if (this[kLineIterator] === undefined) this[kLineIterator] = lineIterator(this);
    return this[kLineIterator];
  };
  if (typeof Symbol.dispose === "symbol") {
    proto[Symbol.dispose] = function () {
      this.close();
    };
  }
  for (const [name, key] of [["_prompt", kPrompt], ["_oldPrompt", kOldPrompt], ["_questionCallback", kQuestionCallback],
    ["_line_buffer", kLineBuffer], ["_sawReturnAt", kSawReturnAt], ["_decoder", kDecoder]]) {
    ObjectDefineProperty(proto, name, {
      configurable: true,
      get() { return this[key]; },
      set(v) { this[key] = v; },
    });
  }

  // The lines as an async iterator (events.on(rl, 'line') in Node): queued
  // until asked for, the input paused while over 1024 wait, done at 'close'.
  const AsyncIteratorPrototype = Object.getPrototypeOf(Object.getPrototypeOf(async function* () {}).prototype);
  function lineIterator(rl) {
    const queue = [];
    let head = 0;
    const waiting = [];
    let error = null;
    let finished = false;
    let paused = false;
    const iterator = Object.setPrototypeOf({
      next() {
        if (head < queue.length) {
          const value = queue[head];
          queue[head++] = undefined;
          if (head === queue.length) queue.length = head = 0;
          if (paused && queue.length === head) {
            rl.resume();
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
        return new Promise((resolve, reject) => waiting.push({ resolve, reject }));
      },
      return() {
        return closeHandler();
      },
      throw(err) {
        if (!(err instanceof Error)) {
          throw nodeError(TypeError, "ERR_INVALID_ARG_TYPE", 'The "EventEmitter.AsyncIterator" property must be an instance of Error.');
        }
        errorHandler(err);
      },
      [Symbol.asyncIterator]() {
        return this;
      },
    }, AsyncIteratorPrototype);
    function lineHandler(line) {
      if (waiting.length === 0) {
        queue.push(line);
        if (!paused && queue.length - head > 1024) {
          paused = true;
          rl.pause();
        }
      } else {
        waiting.shift().resolve({ value: line, done: false });
      }
    }
    function errorHandler(err) {
      if (waiting.length === 0) error = err;
      else waiting.shift().reject(err);
      closeHandler();
    }
    function closeHandler() {
      rl.removeListener("line", lineHandler);
      rl.removeListener("error", errorHandler);
      rl.removeListener("close", closeHandler);
      finished = true;
      paused = false;
      const done = { value: undefined, done: true };
      while (waiting.length) waiting.shift().resolve(done);
      return Promise.resolve(done);
    }
    rl.on("line", lineHandler);
    rl.on("error", errorHandler);
    rl.on("close", closeHandler);
    return iterator;
  }

  // readline.Interface: question(query[, options], cb).
  function Interface(input, output, completer, terminal) {
    if (!(this instanceof Interface)) return new Interface(input, output, completer, terminal);
    // A one-argument completer is called the two-argument (callback) way.
    if (input?.input && typeof input.completer === "function" && input.completer.length !== 2) {
      const { completer } = input;
      input.completer = (v, cb) => cb(null, completer(v));
    } else if (typeof completer === "function" && completer.length !== 2) {
      const realCompleter = completer;
      completer = (v, cb) => cb(null, realCompleter(v));
    }
    InterfaceConstructor.call(this, input, output, completer, terminal);
  }
  Object.setPrototypeOf(Interface.prototype, proto);
  Object.setPrototypeOf(Interface, InterfaceConstructor);
  Interface.prototype.question = function question(query, options, cb) {
    cb = typeof options === "function" ? options : cb;
    if (options === null || typeof options !== "object") options = {};
    if (options.signal) {
      if (options.signal.aborted) return;
      const dispose = onAbort(options.signal, () => this._questionCancel());
      const originalCb = cb;
      cb = typeof cb === "function" ? (answer) => {
        dispose();
        return originalCb(answer);
      } : dispose;
    }
    if (typeof cb === "function") this._question(query, cb);
  };
  Interface.prototype.question[kPromisifyCustom] = function question(query, options) {
    if (options === null || typeof options !== "object") options = {};
    if (options.signal?.aborted) return Promise.reject(new AbortError(undefined, { cause: options.signal.reason }));
    return new Promise((resolve, reject) => {
      let cb = resolve;
      if (options.signal) {
        const dispose = onAbort(options.signal, () => reject(new AbortError(undefined, { cause: options.signal.reason })));
        cb = (answer) => {
          dispose();
          resolve(answer);
        };
      }
      this.question(query, options, cb);
    });
  };
  function createInterface(input, output, completer, terminal) {
    return new Interface(input, output, completer, terminal);
  }

  // readline/promises: question(query[, options]) returns a promise.
  class PromisesInterface extends InterfaceConstructor {
    question(query, options = {}) {
      return new Promise((resolve, reject) => {
        let cb = resolve;
        if (options?.signal) {
          if (options.signal.aborted) return reject(new AbortError(undefined, { cause: options.signal.reason }));
          const dispose = onAbort(options.signal, () => {
            this._questionCancel();
            reject(new AbortError(undefined, { cause: options.signal.reason }));
          });
          cb = (answer) => {
            dispose();
            resolve(answer);
          };
        }
        this._question(query, cb);
      });
    }
  }

  // ---- cursor control (ANSI escapes) ----------------------------------------
  const CSI = "\x1b[";
  const kClearToLineBeginning = CSI + "1K";
  const kClearToLineEnd = CSI + "0K";
  const kClearLine = CSI + "2K";
  const kClearScreenDown = CSI + "0J";
  function validateCallback(callback) {
    if (callback !== undefined && typeof callback !== "function") {
      throw nodeError(TypeError, "ERR_INVALID_ARG_TYPE", 'The "callback" argument must be of type function.');
    }
  }
  function cursorTo(stream, x, y, callback) {
    validateCallback(callback);
    if (typeof y === "function") {
      callback = y;
      y = undefined;
    }
    if (Number.isNaN(x)) throw errInvalidArgValue("x", x);
    if (Number.isNaN(y)) throw errInvalidArgValue("y", y);
    if (stream == null || (typeof x !== "number" && typeof y !== "number")) {
      if (typeof callback === "function") process.nextTick(callback, null);
      return true;
    }
    if (typeof x !== "number") throw nodeError(TypeError, "ERR_INVALID_CURSOR_POS", "Cannot set cursor row without setting its column");
    return stream.write(typeof y !== "number" ? CSI + (x + 1) + "G" : CSI + (y + 1) + ";" + (x + 1) + "H", callback);
  }
  function moveCursorData(dx, dy) {
    let data = "";
    if (dx < 0) data += CSI + -dx + "D";
    else if (dx > 0) data += CSI + dx + "C";
    if (dy < 0) data += CSI + -dy + "A";
    else if (dy > 0) data += CSI + dy + "B";
    return data;
  }
  function moveCursor(stream, dx, dy, callback) {
    validateCallback(callback);
    if (stream == null || !(dx || dy)) {
      if (typeof callback === "function") process.nextTick(callback, null);
      return true;
    }
    return stream.write(moveCursorData(dx, dy), callback);
  }
  function clearLine(stream, dir, callback) {
    validateCallback(callback);
    if (stream === null || stream === undefined) {
      if (typeof callback === "function") process.nextTick(callback, null);
      return true;
    }
    return stream.write(dir < 0 ? kClearToLineBeginning : dir > 0 ? kClearToLineEnd : kClearLine, callback);
  }
  function clearScreenDown(stream, callback) {
    validateCallback(callback);
    if (stream === null || stream === undefined) {
      if (typeof callback === "function") process.nextTick(callback, null);
      return true;
    }
    return stream.write(kClearScreenDown, callback);
  }
  // readline/promises' Readline: the same escapes, queued until commit().
  class Readline {
    #stream;
    #todo = [];
    #autoCommit = false;
    constructor(stream, options = undefined) {
      if (!stream || typeof stream.write !== "function") {
        throw nodeError(TypeError, "ERR_INVALID_ARG_TYPE", 'The "stream" argument must be an instance of Writable.');
      }
      this.#stream = stream;
      if (options?.autoCommit != null) this.#autoCommit = !!options.autoCommit;
    }
    #queue(data) {
      if (this.#autoCommit) process.nextTick(() => this.#stream.write(data));
      else this.#todo.push(data);
      return this;
    }
    cursorTo(x, y = undefined) {
      return this.#queue(y == null ? CSI + (x + 1) + "G" : CSI + (y + 1) + ";" + (x + 1) + "H");
    }
    moveCursor(dx, dy) {
      return dx || dy ? this.#queue(moveCursorData(dx, dy)) : this;
    }
    clearLine(dir) {
      return this.#queue(dir < 0 ? kClearToLineBeginning : dir > 0 ? kClearToLineEnd : kClearLine);
    }
    clearScreenDown() {
      return this.#queue(kClearScreenDown);
    }
    commit() {
      return new Promise((resolve) => {
        this.#stream.write(this.#todo.join(""), resolve);
        this.#todo = [];
      });
    }
    rollback() {
      this.#todo = [];
      return this;
    }
  }

  // ---- keypress events --------------------------------------------------------
  // 'keypress' (str, key) on `stream` from its 'data', once something listens
  // for it: printable characters, control keys, and the common escape
  // sequences (arrows, home/end, insert/delete, page up/down, F1-F4).
  const kKeypressDecoder = Symbol("keypress-decoder");
  const csiNames = { A: "up", B: "down", C: "right", D: "left", E: "clear", F: "end", H: "home", P: "f1", Q: "f2", R: "f3", S: "f4", Z: "tab" };
  const tildeNames = { 1: "home", 2: "insert", 3: "delete", 4: "end", 5: "pageup", 6: "pagedown", 7: "home", 8: "end" };
  const keySequence = /\x1b(?:\[|O)(\d*)(?:;(\d+))?([A-Za-z~])|\x1b?[\s\S]/gu;
  function charKey(ch, key) {
    if (ch === "\r") key.name = "return";
    else if (ch === "\n") key.name = "enter";
    else if (ch === "\t") key.name = "tab";
    else if (ch === "\b" || ch === "\x7f") key.name = "backspace";
    else if (ch === "\x1b") key.name = "escape";
    else if (ch === " ") key.name = "space";
    else if (ch < " ") {
      key.name = String.fromCharCode(ch.charCodeAt(0) + 96);
      key.ctrl = true;
    } else if (/^[0-9a-z]$/.test(ch)) {
      key.name = ch;
    } else if (/^[A-Z]$/.test(ch)) {
      key.name = ch.toLowerCase();
      key.shift = true;
    }
  }
  function emitKeys(stream, s) {
    keySequence.lastIndex = 0;
    let m;
    while ((m = keySequence.exec(s)) !== null) {
      const seq = m[0];
      const key = { sequence: seq, name: undefined, ctrl: false, meta: false, shift: false };
      if (m[3] !== undefined) {
        const code = m[3] === "~" ? tildeNames[m[1]] : csiNames[m[3]];
        const mod = (+m[2] || 1) - 1;
        key.name = code || "undefined";
        key.code = seq.slice(1);
        key.shift = !!(mod & 1) || m[3] === "Z";
        key.meta = !!(mod & 10);
        key.ctrl = !!(mod & 4);
      } else if (seq.length > 1 && seq[0] === "\x1b") {
        charKey(seq.slice(1), key);
        key.meta = true;
      } else {
        charKey(seq, key);
      }
      // A plain character comes with its text; an escape sequence without.
      stream.emit("keypress", seq.length === 1 || (seq.length === 2 && seq.codePointAt(0) > 0xffff) ? seq : undefined, key);
    }
  }
  function emitKeypressEvents(stream, iface = {}) {
    if (stream[kKeypressDecoder]) return;
    stream[kKeypressDecoder] = newDecoder();
    function onData(input) {
      if (stream.listenerCount("keypress") > 0) {
        const string = stream[kKeypressDecoder].write(input);
        if (string) emitKeys(stream, string);
      } else {
        stream.removeListener("data", onData);
        stream.on("newListener", onNewListener);
      }
    }
    function onNewListener(event) {
      if (event === "keypress") {
        stream.on("data", onData);
        stream.removeListener("newListener", onNewListener);
      }
    }
    if (stream.listenerCount("keypress") > 0) stream.on("data", onData);
    else stream.on("newListener", onNewListener);
  }

  const promises = {
    Interface: PromisesInterface,
    Readline,
    createInterface: (input, output, completer, terminal) => new PromisesInterface(input, output, completer, terminal),
  };
  return {
    Interface,
    clearLine,
    clearScreenDown,
    createInterface,
    cursorTo,
    emitKeypressEvents,
    moveCursor,
    promises,
  };
})
