// The playground's engine host: one ZIPP Engine per run, off the main thread.
//
// The page never touches the engine directly. It sends `run` and `frame`
// requests, each answered by one reply carrying the console lines and the
// `ui` commands the program produced, and it terminates this Worker if a
// reply does not arrive within its deadline — so a runaway `while True:` (or
// `for(;;)`) costs the program its engine, never the tab. That is the
// isolation model the engine documents: one tenant per Worker, a deadline on
// the outside, the whole instance discarded on a trap.
//
// Both languages share one contract. Python programs use the built-in `ui`
// module and the engine's Python hooks (`pythonCall`, `takeUi`,
// `setPythonInput`); JavaScript programs get an identical `ui` object from
// the one-line shim below and the ordinary JavaScript ABI (`callFunction`,
// global slots). Either way the page sees the same command arrays.
import init, { Engine, zippProfile } from "../dist/all/zipp_wasm.js";

const HOOK_NAMES = ["draw", "update", "on_click", "on_key"];
// One line, so JavaScript compile errors are off by exactly one line plus the
// preamble the engine reports through `preambleLines`.
const UI_SHIM =
  'var __ui = []; var __input = {mx:0,my:0,down:false,clicked:false,keys:{},w:640,h:480}; ' +
  'var ui = (function () { "use strict"; ' +
  'function push(c) { if (__ui.length >= 100000) throw new RangeError("ui: command limit exceeded for one frame"); __ui.push(c); return null; } ' +
  'function n(v) { if (typeof v !== "number" || !isFinite(v)) throw new TypeError("ui: expected a finite number"); return v; } ' +
  'function s(v) { v = String(v); if (v.length > 4096) throw new RangeError("ui: text too long"); return v; } ' +
  'return Object.freeze({ ' +
  'canvas: function (w, h) { return push(["canvas", n(w), n(h)]); }, ' +
  'clear: function (c) { return push(["clear", s(c)]); }, ' +
  'rect: function (x, y, w, h, c) { return push(["rect", n(x), n(y), n(w), n(h), s(c)]); }, ' +
  'circle: function (x, y, r, c) { return push(["circle", n(x), n(y), n(r), s(c)]); }, ' +
  'line: function (x1, y1, x2, y2, c) { return push(["line", n(x1), n(y1), n(x2), n(y2), s(c)]); }, ' +
  'text: function (x, y, t, c) { return push(["text", n(x), n(y), s(t), s(c)]); }, ' +
  'font: function (size) { return push(["font", n(size)]); }, ' +
  'button: function (x, y, w, h, label) { push(["button", n(x), n(y), n(w), n(h), s(label)]); var i = __input; return !!i.clicked && i.mx >= x && i.mx < x + w && i.my >= y && i.my < y + h; }, ' +
  'mouse: function () { return [__input.mx, __input.my, !!__input.down]; }, ' +
  'clicked: function () { return !!__input.clicked; }, ' +
  'key: function (k) { return __input.keys[k] === true; }, ' +
  'width: function () { return __input.w; }, ' +
  'height: function () { return __input.h; } ' +
  '}); })();';

const ready = init().then(() => JSON.parse(zippProfile()));

let engine = null;
let language = null;
let hooks = {};
let jsSlots = null;

function disposeEngine() {
  if (engine) {
    try { engine.dispose(); } catch { /* already gone */ }
  }
  engine = null;
  language = null;
  hooks = {};
  jsSlots = null;
}

function live() {
  return engine !== null && !engine.disposed;
}

// Every console line the program has produced since the last drain, in
// order, tagged with its stream.
function drainConsole() {
  if (!live()) return [];
  try {
    return engine.takeConsole().map((entry) => ({
      stream: String(entry.stream),
      text: String(entry.text),
    }));
  } catch {
    return [];
  }
}

function takeUi() {
  if (!live()) return [];
  if (language === "python") return engine.takeUi();
  if (jsSlots.ui === undefined) return [];
  const commands = engine.getGlobalByIndex(jsSlots.ui);
  engine.setGlobalByIndex(jsSlots.ui, []);
  return Array.isArray(commands) ? commands : [];
}

function call(name, args) {
  return language === "python" ? engine.pythonCall(name, args) : engine.callFunction(name, args);
}

function setInput(input) {
  if (language === "python") engine.setPythonInput(JSON.stringify(input));
  else if (jsSlots.input !== undefined) engine.setGlobalByIndex(jsSlots.input, input);
}

function run(m) {
  disposeEngine();
  engine = new Engine();
  engine.setInstructionBudget(m.budget);
  language = m.language;
  let prelude = 0;
  if (language === "python") {
    engine.initPythonProject(m.files, m.entry);
    hooks = Object.fromEntries(HOOK_NAMES.map((name) => [name, engine.pythonHas(name)]));
  } else {
    // Classic-script semantics: every file shares one global scope, in the
    // order the page lists them (the entry last), like successive <script>
    // tags. Compile-error line numbers count the preamble and the shim.
    prelude = engine.preambleLines + 1;
    const source = UI_SHIM + "\n" + m.order.map((name) => m.files[name]).join("\n");
    const symbols = engine.initSource(source, "javascript");
    const functions = new Set();
    jsSlots = {};
    for (const [name, info] of Object.entries(symbols)) {
      if (info.scope === "function") functions.add(name);
      if (name === "__ui") jsSlots.ui = info.index;
      if (name === "__input") jsSlots.input = info.index;
    }
    hooks = Object.fromEntries(HOOK_NAMES.map((name) => [name, functions.has(name)]));
  }
  return { hooks, prelude, console: drainConsole(), ui: takeUi() };
}

function frame(m) {
  setInput(m.input);
  for (const event of m.events) {
    if (event.type === "click" && hooks.on_click) call("on_click", [event.x, event.y]);
    else if (event.type === "key" && hooks.on_key) call("on_key", [event.key]);
  }
  if (hooks.update) call("update", []);
  if (hooks.draw) call("draw", []);
  return { ui: takeUi(), console: drainConsole() };
}

self.onmessage = async (event) => {
  const m = event.data;
  let profile;
  try {
    profile = await ready;
  } catch (error) {
    self.postMessage({ id: m.id, type: "error", fatal: true, error: `engine failed to load: ${error}` });
    return;
  }
  try {
    switch (m.type) {
      case "hello":
        self.postMessage({ id: m.id, type: "hello", profile });
        break;
      case "run":
        self.postMessage({ id: m.id, type: "ran", ...run(m) });
        break;
      case "frame":
        self.postMessage({ id: m.id, type: "frame", ...frame(m) });
        break;
      case "stop":
        disposeEngine();
        self.postMessage({ id: m.id, type: "stopped" });
        break;
      default:
        self.postMessage({ id: m.id, type: "error", error: `unknown request ${m.type}` });
    }
  } catch (error) {
    // A guest throw leaves the engine usable; a resource-limit crossing or a
    // failed initialization disposes it. Report which, with whatever output
    // preceded the failure.
    let kind = "guest";
    let disposed = true;
    try {
      kind = engine ? engine.lastErrorKind() : "usage";
      disposed = !live();
    } catch { /* engine gone */ }
    self.postMessage({
      id: m.id,
      type: "error",
      error: String(error && error.message ? error.message : error),
      kind,
      disposed,
      console: drainConsole(),
      ui: disposed ? [] : takeUi(),
    });
  }
};
