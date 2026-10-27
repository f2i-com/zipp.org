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
  function stream(fd) {
    return {
      fd,
      isTTY: host.isatty(fd),
      columns: 80,
      rows: 24,
      writable: true,
      write(chunk, encoding, cb) {
        host.write(fd, typeof chunk === "string" ? chunk : new TextDecoder().decode(chunk));
        if (typeof encoding === "function") encoding();
        else if (typeof cb === "function") cb();
        return true;
      },
      end(chunk) { if (chunk != null) this.write(chunk); },
      on() { return this; },
      once() { return this; },
      off() { return this; },
      removeListener() { return this; },
      emit() { return false; },
      getColorDepth() { return 1; },
      hasColors() { return false; },
    };
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
    stdout: stream(1),
    stderr: stream(2),
    stdin: { fd: 0, isTTY: host.isatty(0), on() { return this; }, once() { return this; }, setEncoding() {}, resume() {}, pause() {} },
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
