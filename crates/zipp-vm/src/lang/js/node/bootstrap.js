// Node's globals (see mod.rs). One function of the host object, run once
// before the program. Each global is an accessor until it is first read: the
// read loads the part that defines it (`host.load`, compiled then) and
// leaves the global as an ordinary writable data property, enumerable as
// Node's are except `process`.
(function (host, script) {
  "use strict";
  const G = globalThis;
  const defineProperty = Object.defineProperty;
  const nativeSetTimeout = G.setTimeout;
  const parts = Object.create(null);
  function part(name) {
    return (parts[name] ||= host.load(name)(host, nativeSetTimeout));
  }
  // Node's non-enumerable globals among these.
  const hidden = (name) =>
    name === "process" || name === "Buffer" || name === "URL" || name === "URLSearchParams";
  function settle(name, value) {
    defineProperty(G, name, { value, writable: true, configurable: true, enumerable: !hidden(name) });
    return value;
  }
  function lazy(name, from, make = () => part(from)[name]) {
    defineProperty(G, name, {
      configurable: true,
      enumerable: !hidden(name),
      get() { return settle(name, make()); },
      set(v) { settle(name, v); },
    });
  }
  for (const name of ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "setImmediate", "clearImmediate", "queueMicrotask"]) {
    lazy(name, "timers");
  }
  lazy("structuredClone", "clone");
  lazy("URL", "url_whatwg");
  lazy("URLSearchParams", "url_whatwg");
  lazy("process", "process");
  for (const name of ["Buffer", "atob", "btoa"]) {
    lazy(name, null, () => host.cjs().builtin("buffer")[name]);
  }
  settle("global", G);
  // A script is a CommonJS module of its own: `require` and the rest are
  // its, made on first use (a module has none, as in Node).
  if (script) {
    let main;
    const cjs = () => (main ||= host.cjs().main(host.argv()[1] || "[stdin]"));
    const bindings = {
      require: () => cjs().require,
      module: () => cjs().module,
      exports: () => cjs().module.exports,
      __filename: () => cjs().filename,
      __dirname: () => cjs().dirname,
    };
    for (const name of Object.keys(bindings)) {
      defineProperty(G, name, {
        configurable: true,
        enumerable: false,
        get() { return settleHidden(name, bindings[name]()); },
        set(v) { settleHidden(name, v); },
      });
    }
  }
  function settleHidden(name, value) {
    defineProperty(G, name, { value, writable: true, configurable: true, enumerable: false });
    return value;
  }
})
