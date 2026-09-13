// GPU compute from Python at the wasm boundary: a program's `zipp_gpu`
// graphs leave the engine as host requests (`takeHostRequests`), the
// gpu-lab compute runtime executes them, and the answers come back through
// `pythonCall("__zipp_py_deliver", ...)`. Runs the JavaScript reference and
// the compiled-WASM backends (both available in Node); the GPU backends
// need a browser and are covered by playground/smoke.cjs.
//
// Adapts to the artifact: without "python" there is nothing to check here.
"use strict";
const path = require("node:path");
const { readFile } = require("node:fs/promises");
const { Engine, zippProfile } = require("./pkg/zipp_wasm.js");

const profile = JSON.parse(zippProfile());
let pass = 0, fail = 0;
function ok(label, cond, extra = "") {
  if (cond) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label} ${extra}`); }
}
function eq(label, got, want) {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  if (g === w) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}\n         got  ${g}\n         want ${w}`); }
}

const PROGRAM = [
  "from zipp_gpu import Graph, ComputeError",
  "g = Graph()",
  "a = g.tensor([1, 2, 3, 4])",
  "b = g.tensor([10, 20, 30, 40])",
  "c = (a * b + 4).relu()",
  "def show(result):",
  "    print('result', result['backend'], *[result['outputs'][k]['data'] for k in sorted(result['outputs'])])",
  "def failed(error):",
  "    print('failed', error.code, str(error))",
  "g.submit(show, failed, result=c, total=c.sum())",
  "m = Graph()",
  "m.submit(show, failed, result=m.tensor([[1, 2], [3, 4]]) @ m.tensor([[1, 2], [3, 4]]))",
  "print('submitted')",
  "def draw():",
  "    pass",
  "",
].join("\n");

async function main() {
  if (!profile.languages.includes("python")) {
    console.log("  skip python-gpu: this artifact has no Python frontend");
    return;
  }
  const gpuLab = path.join(__dirname, "..", "..", "gpu-lab");
  const { pathToFileURL } = require("node:url");
  const { createRuntime } = await import(pathToFileURL(path.join(gpuLab, "src", "runtime.mjs")).href);
  const { createPythonGPUAdapter } = await import(pathToFileURL(path.join(gpuLab, "src", "zipp-python-adapter.mjs")).href);
  const wasmBytes = await readFile(path.join(gpuLab, "wasm", "kernels.wasm"));

  // ---- the raw channel: requests out, answers in --------------------------------
  {
    const e = new Engine();
    e.initPythonProject({ main: PROGRAM }, "main");
    eq("the program runs to its end before any request is answered", e.takeOutput(), ["submitted"]);
    const requests = e.takeHostRequests();
    ok("two gpu.execute requests are pending", requests.length === 2 && requests.every((r) => r.kind === "gpu.execute"), JSON.stringify(requests).slice(0, 200));
    ok("request ids are positive integers", requests.every((r) => Number.isSafeInteger(r.id) && r.id >= 1));
    eq("the payload is the plain graph (protocol version 1, seven nodes)", [requests[0].payload.version, requests[0].payload.nodes.length, requests[0].payload.outputs.map((o) => o.name)], [1, 7, ["result", "total"]]);
    eq("draining again yields nothing", e.takeHostRequests(), []);
    ok("the pending count is visible to the host", e.pythonCall("__zipp_py_pending_host", []) === 2);
    const delivered = e.pythonCall("__zipp_py_deliver", [requests[0].id, { ok: true, value: { backend: "test", outputs: { result: { shape: [4], dtype: "float32", data: [14, 44, 94, 164] }, total: { shape: [], dtype: "float32", data: [316] } }, stats: {} } }]);
    ok("a delivery reaches the program's callback", delivered === true);
    eq("the callback printed the result (host numbers become floats)", e.takeOutput(), ["result test [14.0, 44.0, 94.0, 164.0] [316.0]"]);
    ok("an unknown id is refused without a throw", e.pythonCall("__zipp_py_deliver", [999, { ok: true, value: {} }]) === false);
    ok("a second delivery of the same id is refused", e.pythonCall("__zipp_py_deliver", [requests[0].id, { ok: true, value: {} }]) === false);
    e.pythonCall("__zipp_py_deliver", [requests[1].id, { ok: false, error: { code: "SHAPE", message: "no" } }]);
    eq("an error reply reaches on_error as a ComputeError", e.takeOutput(), ["failed SHAPE SHAPE: no"]);
    ok("the program's hooks still work afterwards", e.pythonHas("draw") === true);
    e.dispose();
  }

  // ---- the adapter over the real compute runtime ---------------------------------
  for (const backend of ["cpu-js", "wasm"]) {
    const e = new Engine();
    const runtime = await createRuntime({ backend, wasmBytes });
    const events = [];
    const adapter = createPythonGPUAdapter(e, runtime, { allowExecute: true, onDelivered: (ev) => events.push(ev) });
    e.initPythonProject({ main: PROGRAM }, "main");
    const others = adapter.drain();
    eq(`${backend}: the adapter admits every gpu.execute request`, others, []);
    await adapter.idle();
    eq(`${backend}: both requests were delivered in order`, events.map((ev) => [ev.delivered, ev.reply.ok, ev.error === null]), [[true, true, true], [true, true, true]]);
    eq(`${backend}: the program saw the computed numbers`, e.takeOutput(), ["submitted", `result ${backend} [14.0, 44.0, 94.0, 164.0] [316.0]`, `result ${backend} [7.0, 10.0, 15.0, 22.0]`]);
    adapter.invalidate();
    await adapter.idle();
    runtime.dispose();
    e.dispose();
  }

  // ---- denial, disposal, and a callback that raises ---------------------------------
  {
    const e = new Engine();
    const runtime = await createRuntime({ backend: "cpu-js" });
    const denied = createPythonGPUAdapter(e, runtime, { allowExecute: false });
    e.initPythonProject({ main: PROGRAM }, "main");
    denied.drain();
    await denied.idle();
    eq("without the grant every request fails with DENIED", e.takeOutput(), ["submitted", "failed DENIED DENIED: GPU compute is not granted to this tenant", "failed DENIED DENIED: GPU compute is not granted to this tenant"]);
    denied.invalidate();
    e.dispose();

    // The host's own limits are enforced on the host, whatever Python accepted.
    const small = await createRuntime({ backend: "cpu-js", limits: { maxNodes: 4 } });
    const e4 = new Engine();
    const limited = createPythonGPUAdapter(e4, small, { allowExecute: true });
    e4.initPythonProject({ main: PROGRAM }, "main");
    limited.drain();
    await limited.idle();
    eq("a graph over the host's node limit fails with LIMIT while a small one runs", e4.takeOutput(), ["submitted", "failed LIMIT LIMIT: Invalid graph node count", "result cpu-js [7.0, 10.0, 15.0, 22.0]"]);
    limited.invalidate();
    small.dispose();
    e4.dispose();

    const e2 = new Engine();
    const events = [];
    const adapter = createPythonGPUAdapter(e2, runtime, { allowExecute: true, onDelivered: (ev) => events.push(ev) });
    e2.initPythonProject({ main: "from zipp_gpu import Graph\ng = Graph()\ndef boom(result):\n    raise ValueError('callback failed')\ng.submit(boom, result=g.tensor([1]))\n" }, "main");
    adapter.drain();
    await adapter.idle();
    ok("a callback that raises is reported to the host, not lost", events.length === 1 && events[0].error !== null && /callback failed/.test(String(events[0].error)), String(events[0] && events[0].error));
    ok("the engine survives a raising callback", !e2.disposed && e2.pythonCall("__zipp_py_pending_host", []) === 0);
    adapter.invalidate();

    const e3 = new Engine();
    const late = [];
    const gone = createPythonGPUAdapter(e3, runtime, { allowExecute: true, onDelivered: (ev) => late.push(ev) });
    e3.initPythonProject({ main: PROGRAM }, "main");
    gone.drain();
    e3.dispose();
    await gone.idle();
    ok("a disposed engine is never re-entered for a late result", late.length === 0);
    runtime.dispose();
    e2.dispose();
  }
}

main().then(() => {
  console.log(`\n${pass} passed, ${fail} failed`);
  process.exit(fail ? 1 : 0);
}, (error) => {
  console.error(error);
  process.exit(1);
});
