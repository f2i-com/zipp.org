// Node's globals (see mod.rs). One function of the host object, run once
// before the program. Each global is an accessor until it is first read: the
// read loads the part that defines it (`host.load`, compiled then) and
// leaves the global as an ordinary writable data property, enumerable as
// Node's are except `process`.
//
// Its bindings are `let`, never `const`: a `const` a closure captures is
// a const cell, and the first one in a run turns off compiled code's inline
// writes to EVERY captured variable (the sticky `const_cells_nonempty`),
// which a program that never touches these globals would pay for (18% of
// calls-closures with the JIT on).
(function (host, script) {
  "use strict";
  let G = globalThis;
  let defineProperty = Object.defineProperty;
  let nativeSetTimeout = G.setTimeout;
  let parts = Object.create(null);
  function part(name) {
    return (parts[name] ||= host.load(name)(host, nativeSetTimeout));
  }
  // The parts as the globals have them, for the parts and builtins that
  // share one (node:events and node:stream use web_events's classes).
  host.part = part;
  // Node's non-enumerable globals among these.
  let hiddenNames = new Set([
    "process", "Buffer", "URL", "URLSearchParams",
    "Event", "EventTarget", "CustomEvent", "AbortController", "AbortSignal", "DOMException",
  ]);
  let hidden = (name) => hiddenNames.has(name);
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
  for (let name of ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "setImmediate", "clearImmediate", "queueMicrotask"]) {
    lazy(name, "timers");
  }
  lazy("structuredClone", "clone");
  lazy("URL", "url_whatwg");
  lazy("URLSearchParams", "url_whatwg");
  for (let name of ["Event", "EventTarget", "CustomEvent", "AbortController", "AbortSignal", "DOMException"]) {
    lazy(name, "web_events");
  }
  lazy("process", "process");
  for (let name of ["Buffer", "atob", "btoa"]) {
    lazy(name, null, () => host.cjs().builtin("buffer")[name]);
  }
  settle("global", G);
  // A script is a CommonJS module of its own: `require` and the rest are
  // its, made on first use (a module has none, as in Node).
  if (script) {
    let main;
    let cjs = () => (main ||= host.cjs().main(host.argv()[1] || "[stdin]"));
    let bindings = {
      require: () => cjs().require,
      module: () => cjs().module,
      exports: () => cjs().module.exports,
      __filename: () => cjs().filename,
      __dirname: () => cjs().dirname,
    };
    for (let name of Object.keys(bindings)) {
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
