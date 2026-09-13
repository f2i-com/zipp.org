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
import { createRuntime } from "../gpu-lab/src/runtime.mjs";
import { createPythonGPUAdapter } from "../gpu-lab/src/zipp-python-adapter.mjs";

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

// ---- GPU compute for Python programs -----------------------------------------
// A Python program's `zipp_gpu` graphs leave the engine as host requests
// (`takeHostRequests`), run on the gpu-lab compute runtime (WebGPU, WebGL2,
// compiled WASM or the JavaScript reference, per the page's choice), and the
// answers go back through `pythonCall("__zipp_py_deliver", ...)` between
// engine calls. The runtime is created on the first request so a program
// that never computes never touches the GPU; it is kept across runs and
// replaced when the page picks another backend.
const gpu = { backend: "auto", runtime: null, creating: null, adapter: null };

async function computeRuntime() {
  if (gpu.runtime) return gpu.runtime;
  if (!gpu.creating) {
    gpu.creating = createRuntime({
      backend: gpu.backend,
      wasmUrl: new URL("../gpu-lab/wasm/kernels.wasm", import.meta.url),
      limits: { maxNodes: 512, maxWork: 50_000_000 },
    }).then((runtime) => {
      gpu.runtime = runtime;
      const info = runtime.info();
      const attempts = (info.fallbackAttempts || []).map((a) => `${a.backend}: ${a.error}`);
      self.postMessage({ type: "event", gpu: { backend: info.backend, description: info.description, adapter: info.adapter, attempts } });
      return runtime;
    }).finally(() => { gpu.creating = null; });
  }
  return gpu.creating;
}
// `createZippGPUHandler` only needs `execute`; creating the runtime lazily
// keeps the adapter synchronous to create.
const lazyRuntime = { async execute(program) { return (await computeRuntime()).execute(program); } };

function selectGpuBackend(backend) {
  const wanted = ["auto", "webgpu", "webgl2", "wasm", "cpu-js"].includes(backend) ? backend : "auto";
  if (wanted === gpu.backend) return;
  gpu.backend = wanted;
  if (gpu.runtime && !gpu.runtime.busy) { try { gpu.runtime.dispose(); } catch { /* keep going */ } }
  gpu.runtime = null;
}

function attachGpu() {
  gpu.adapter = createPythonGPUAdapter(engine, lazyRuntime, {
    allowExecute: true,
    maxPending: 16,
    maxRequests: 100000,
    onDelivered: ({ error }) => {
      // The callback ran (or failed) outside any page request: report what
      // it printed and drew as an unsolicited event, then look for the
      // requests it may have submitted in turn.
      const event = { type: "event", console: drainConsole(), ui: takeUi(), files: takeWrittenFiles() };
      if (error) {
        let kind = "guest", disposed = true;
        try { kind = engine ? engine.lastErrorKind() : "usage"; disposed = !live(); } catch { /* engine gone */ }
        event.error = String(error && error.message ? error.message : error);
        event.kind = kind;
        event.disposed = disposed;
      }
      self.postMessage(event);
      drainHost();
    },
  });
}

// After every engine call: hand new GPU requests to the adapter.
function drainHost() {
  if (language !== "python" || !gpu.adapter || !live()) return;
  const others = gpu.adapter.drain();
  for (const request of others) {
    // No other request kinds exist yet; answer so the program is not left waiting.
    try { engine.pythonCall("__zipp_py_deliver", [request.id, { ok: false, error: { code: "DENIED", message: `unknown host request ${request.kind}` } }]); } catch { /* reported with the next reply */ }
  }
}

function disposeEngine() {
  if (gpu.adapter) {
    gpu.adapter.invalidate();
    gpu.adapter = null;
  }
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
  if (!live()) return failedConsole();
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
  selectGpuBackend(m.gpuBackend);
  engine = new Engine();
  engine.setInstructionBudget(m.budget);
  language = m.language;
  let prelude = 0;
  if (language === "python") {
    attachGpu();
    engine.initPythonProject(m.files, m.entry, Array.isArray(m.argv) ? m.argv.map(String) : []);
    hooks = Object.fromEntries(HOOK_NAMES.map((name) => [name, engine.pythonHas(name)]));
    drainHost();
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
  return { hooks, prelude, console: drainConsole(), ui: takeUi(), files: takeWrittenFiles() };
}

// What a program printed before its top level failed: the engine keeps it
// past its disposal so the console shows the output ahead of the error.
function failedConsole() {
  try {
    if (!engine || typeof engine.takeFailedConsole !== "function") return [];
    return engine.takeFailedConsole().map((entry) => ({ stream: String(entry.stream), text: String(entry.text) }));
  } catch {
    return [];
  }
}

// Explicit VFS changes: {path, base64} (including empty files) or {path, deleted:true}.
function takeWrittenFiles() {
  if (!live() || language !== "python") return [];
  try {
    const text = engine.pythonCall("__zipp_py_vfs_changed", []);
    if (typeof text !== "string" || !text) return [];
    const result = JSON.parse(text);
    if (result.version !== 1 || !Array.isArray(result.changes)) throw Error('Unsupported VFS change protocol');
    return result.changes;
  } catch {
    return [];
  }
}

function frame(m) {
  // The engine's instruction budget is a lifetime total; renewing it before
  // each frame makes it a per-frame bound instead, so an animation runs for
  // as long as the page keeps calling it while one frame still cannot spin
  // forever (the page's deadline covers the rest).
  engine.renewInstructionBudget();
  setInput(m.input);
  for (const event of m.events) {
    if (event.type === "click" && hooks.on_click) call("on_click", [event.x, event.y]);
    else if (event.type === "key" && hooks.on_key) call("on_key", [event.key]);
  }
  if (hooks.update) call("update", []);
  if (hooks.draw) call("draw", []);
  drainHost();
  return { ui: takeUi(), console: drainConsole(), files: takeWrittenFiles() };
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
      files: disposed ? [] : takeWrittenFiles(),
    });
  }
};
