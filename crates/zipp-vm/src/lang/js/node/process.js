// `process` (see mod.rs), over the host's environment, arguments, streams,
// clock and exit.
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

  const listeners = ObjectCreate(null);
  function on(event, fn) { (listeners[event] ||= []).push({ fn, once: false }); return process; }
  function once(event, fn) { (listeners[event] ||= []).push({ fn, once: true }); return process; }
  function off(event, fn) {
    const list = listeners[event];
    if (list) {
      const i = list.findIndex((l) => l.fn === fn);
      if (i >= 0) list.splice(i, 1);
    }
    return { process };
  }
  function emit(event, ...args) {
    const list = listeners[event];
    if (!list || list.length === 0) return false;
    for (const l of list.slice()) {
      if (l.once) off(event, l.fn);
      apply(l.fn, process, args);
    }
    return true;
  }
  // The standard streams are node:stream's, made the first time one is
  // read (a program that never touches them never loads the module), and
  // handled as Node handles stdio: writes go out in order with console
  // output, and destroying one (a `pipeline` into stdout does) leaves the
  // descriptor open, emitting 'close' so `finished` and `pipeline` complete.
  function stdioDestroy(err, cb) {
    cb(err);
    this._undestroy();
    if (!this._writableState.emitClose) process.nextTick(() => this.emit("close"));
  }
  function writableStdio(fd) {
    const { Writable } = host.cjs().builtin("stream");
    const s = new Writable({
      decodeStrings: false,
      write(chunk, encoding, cb) {
        host.write(fd, typeof chunk === "string" ? chunk : new TextDecoder().decode(chunk));
        cb();
      },
    });
    s.fd = fd;
    s._isStdio = true;
    s.isTTY = host.isatty(fd);
    s.columns = 80;
    s.rows = 24;
    s.destroySoon = s.destroy;
    s._destroy = stdioDestroy;
    s.getColorDepth = () => 1;
    s.hasColors = () => false;
    s.getWindowSize = () => [80, 24];
    return s;
  }
  function readableStdin() {
    const { Readable } = host.cjs().builtin("stream");
    const s = new Readable({
      highWaterMark: 65536,
      // A blocking read: a terminal hands over a line at a time, a pipe or
      // a file what it has. The program waits for input as it asked to.
      read(size) {
        const chunk = host.readStdin(Math.max(size, 1), true);
        this.push(chunk === null ? null : Buffer.from(chunk));
      },
    });
    s.fd = 0;
    s._isStdio = true;
    s.isTTY = host.isatty(0);
    s.isRaw = false;
    s.setRawMode = function (mode) { this.isRaw = !!mode; return this; };
    s.ref = s.unref = function () { return this; };
    return s;
  }
  function hrtime(prev) {
    const [s, ns] = host.hrtime();
    if (ArrayIsArray(prev)) {
      let ds = s - prev[0], dns = ns - prev[1];
      if (dns < 0) { ds -= 1; dns += 1e9; }
      return [ds, dns];
    }
    return [s, ns];
  }
  hrtime.bigint = () => {
    const [s, ns] = host.hrtime();
    return BigInt(s) * 1000000000n + BigInt(ns);
  };
  const start = host.hrtime();
  const argv = host.argv();
  // `nextTick`'s queue, run by the VM before the microtasks.
  const ticks = [];
  host.onTicks(() => {
    while (ticks.length) {
      const callback = ticks.shift(), args = ticks.shift();
      apply(callback, undefined, args);
    }
  });
  const version = "v24.11.0";
  const process = {
    title: "zipp",
    version,
    versions: { node: version.slice(1), zipp: host.zippVersion, v8: "13.6.233.10-node.31", uv: "1.51.0", modules: "137" },
    release: { name: "node" },
    platform: host.platform,
    arch: host.arch,
    pid: host.pid,
    ppid: 0,
    execPath: host.execPath,
    argv,
    argv0: argv.length ? argv[0] : "zipp",
    execArgv: [],
    env: host.env(),
    exitCode: undefined,
    config: { variables: {} },
    features: {},

    cwd: () => host.cwd(),
    chdir() { throw new Error("process.chdir() is not supported"); },
    exit(code) {
      if (code !== undefined) process.exitCode = code;
      const c = process.exitCode === undefined ? 0 : +process.exitCode | 0;
      emit("exit", c);
      host.exit(c);
    },
    abort() { host.exit(134); },
    kill() { return true; },
    nextTick(callback, ...args) {
      if (typeof callback !== "function") throw new TypeError('The "callback" argument must be of type function');
      ticks.push(callback, args);
      if (ticks.length === 2) host.requestTicks();
    },
    hrtime,
    get stdout() { return defineProperty(this, "stdout", { value: writableStdio(1), writable: true, configurable: true, enumerable: true }).stdout; },
    set stdout(v) { defineProperty(this, "stdout", { value: v, writable: true, configurable: true, enumerable: true }); },
    get stderr() { return defineProperty(this, "stderr", { value: writableStdio(2), writable: true, configurable: true, enumerable: true }).stderr; },
    set stderr(v) { defineProperty(this, "stderr", { value: v, writable: true, configurable: true, enumerable: true }); },
    get stdin() { return defineProperty(this, "stdin", { value: readableStdin(), writable: true, configurable: true, enumerable: true }).stdin; },
    set stdin(v) { defineProperty(this, "stdin", { value: v, writable: true, configurable: true, enumerable: true }); },
    uptime() {
      const [s, ns] = hrtime(start);
      return s + ns / 1e9;
    },
    memoryUsage: Object.assign(() => ({ rss: 0, heapTotal: 0, heapUsed: 0, external: 0, arrayBuffers: 0 }), { rss: () => 0 }),
    cpuUsage: () => ({ user: 0, system: 0 }),
    resourceUsage: () => ({}),
    umask: () => 0o22,
    getuid: () => 0,
    getgid: () => 0,
    emitWarning(warning, type) {
      const name = typeof warning === "object" && warning ? warning.name : (typeof type === "string" ? type : "Warning");
      const message = typeof warning === "object" && warning ? warning.message : String(warning);
      host.write(2, "(node:" + host.pid + ") " + name + ": " + message + "\n");
    },
    on, once, off,
    addListener: on,
    removeListener: off,
    removeAllListeners(event) { if (event === undefined) for (const k in listeners) delete listeners[k]; else delete listeners[event]; return process; },
    prependListener: on,
    listeners: (event) => (listeners[event] || []).map((l) => l.fn),
    listenerCount: (event) => (listeners[event] || []).length,
    emit,
    binding(name) { throw new Error("No such module: " + name); },
    allowedNodeEnvironmentFlags: new SetCtor(),
  };
  defineProperty(process, Symbol.toStringTag, { value: "process", configurable: true });
  host.onExit(() => {
    const c = process.exitCode === undefined ? 0 : +process.exitCode | 0;
    emit("beforeExit", c);
    emit("exit", c);
    return process.exitCode === undefined ? 0 : +process.exitCode | 0;
  });
  return { process };
})
