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

  // Ordinary Torch models record a real graph and receive a regular tensor.
  for (const backend of ["cpu-js", "wasm"]) {
    const e = new Engine();
    const runtime = await createRuntime({ backend, wasmBytes });
    const adapter = createPythonGPUAdapter(e, runtime, { allowExecute: true });
    e.initPythonProject({ main: `import torch
from torch import nn
model = nn.Sequential(nn.Linear(2, 3), nn.ReLU(), nn.Linear(3, 1))
with torch.no_grad():
    for p in model.parameters():
        p.fill_(0.5)
x = torch.tensor([[1.0, 2.0], [3.0, 4.0]])
expected = model(x)
pending = torch.compile(model)(x)
def ready(y):
    print("torch", pending.backend, torch.allclose(y, expected), y.tolist())
pending.submit(ready)
@torch.compile
def reduce(x):
    return torch.mean(torch.add(torch.mul(x, x), 1.0))
reduce(x).submit(lambda y: print("mean", y.item()))
@torch.compile
def reverse(x):
    constants = torch.tensor([[2.0, 2.0], [2.0, 2.0]])
    return torch.matmul(constants, torch.sub(constants, torch.mul(constants, torch.add(constants, x))))
reverse(x).submit(lambda y: print("reverse", y.tolist()))
print("recorded")
` }, "main");
    eq(`${backend}: Torch inference waits for graph delivery`, e.takeOutput(), ["recorded"]);
    adapter.drain();
    await adapter.idle();
    eq(`${backend}: Torch model and functional operations match eager`, e.takeOutput(), [`torch ${backend} True [[3.5], [6.5]]`, "mean 8.5", "reverse [[-24.0, -32.0], [-24.0, -32.0]]"]);
    adapter.invalidate(); runtime.dispose(); e.dispose();
  }

  // Protocol version 3 (comparisons, maximum/minimum, where, uniform) through
  // the host: the answer equals zipp_gpu's own reference bit for bit, and
  // torch.compile records comparisons, where, clamp and hardtanh.
  for (const backend of ["cpu-js", "wasm"]) {
    const e = new Engine();
    const runtime = await createRuntime({ backend, wasmBytes });
    const adapter = createPythonGPUAdapter(e, runtime, { allowExecute: true });
    e.initPythonProject({ main: `import zipp_gpu
import torch
import torch.nn.functional as F
g = zipp_gpu.Graph()
a = g.tensor([[0.5, -1.0, 2.0], [2.0, 0.0, -0.5]])
b = g.tensor([2.0, 0.0, -1.0])
outs = {op: getattr(a, op)(b) for op in ("eq", "ne", "lt", "le", "gt", "ge", "maximum", "minimum")}
outs["pick"] = g.where(a > 0, b, a)
outs["u"] = g.uniform((3, 257), 2 ** 31 + 5, 7)
outs["drop"] = a * (g.uniform((2, 3), 99, 4) >= 0.25) * (1 / 0.75)
program = g.program(**outs)
reference = zipp_gpu.execute_locally(program)["outputs"]
def show(result):
    print("v3", result["backend"], program["version"], all(result["outputs"][k]["data"] == reference[k]["data"] for k in reference))
g.submit(show, lambda error: print("failed", error), **outs)
x = torch.tensor([[1.5, -2.0, 0.25], [0.0, 3.0, -0.75]])
fn = lambda x: torch.where(x > 0, x, x * 0.1).clamp(-1, 1) + F.hardtanh(x * 2) + x.masked_fill(x == 0, 5.0)
torch.compile(fn)(x).submit(lambda y: print("torch v3", torch.allclose(y, fn(x), atol=1e-6)))
` }, "main");
    adapter.drain();
    await adapter.idle();
    eq(`${backend}: version-3 graphs equal zipp_gpu's reference; compiled masks, where, clamp and hardtanh match eager`, e.takeOutput(),
      [`v3 ${backend} 3 True`, "torch v3 True"]);
    adapter.invalidate(); runtime.dispose(); e.dispose();
  }

  // Run the actual portable Life module: a glider shifts diagonally after 4 steps.
  const lifeSource = await readFile(path.join(__dirname, "../../../../examples/python/gpu/life.py"), "utf8");
  for (const backend of ["cpu-js", "wasm"]) {
    const e = new Engine();
    const runtime = await createRuntime({ backend, wasmBytes });
    const adapter = createPythonGPUAdapter(e, runtime, { allowExecute: true });
    e.initPythonProject({ life: lifeSource, main: `import torch
from life import step, neighbor_matrix
cells = torch.tensor([[0.,1.,0.,0.,0.],[0.,0.,1.,0.,0.],[1.,1.,1.,0.,0.],[0.,0.,0.,0.,0.],[0.,0.,0.,0.,0.]])
@torch.compile
def evolve(cells, neighbors):
    for _ in range(4):
        cells = step(cells, neighbors)
    return cells
evolve(cells, neighbor_matrix(5)).submit(lambda y: print(y.tolist()))
` }, "main");
    adapter.drain(); await adapter.idle();
    eq(`${backend}: portable Torch Life advances a glider by one cell`, e.takeOutput(), ["[[0.0, 0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0], [0.0, 1.0, 1.0, 1.0, 0.0], [0.0, 0.0, 0.0, 0.0, 0.0]]"]);
    adapter.invalidate(); runtime.dispose(); e.dispose();
  }

  // ---- sessions: Graph.prepare through the real engine -------------------------------------
  // The program prepares one Adam step of a tiny MLP, runs three steps one at
  // a time and three in one request, downloads a weight, and disposes; the
  // same steps as chained submits are its reference.
  const SESSION = `from zipp_gpu import Graph, ComputeError, execute_locally
xs = [[[0.1 * (i + j + s) for j in range(4)] for i in range(3)] for s in range(6)]
ys = [[(i + s) % 2 for i in range(3)] for s in range(6)]
w0 = [[0.05 * (i - j) for j in range(2)] for i in range(4)]
def step_graph(w, m, v, step):
    g = Graph()
    x, y, wt = g.tensor(xs[0]), g.tensor(ys[0]), g.tensor(w, shape=(4, 2))
    logits = x @ wt
    grad = x.T @ logits.cross_entropy_grad(y)
    p1, m1, v1 = g.adam(wt, grad, g.tensor(m, shape=(4, 2)), g.tensor(v, shape=(4, 2)), lr=0.1, step=step)
    return g, x, y, wt, {"loss": logits.cross_entropy(y), "w": p1, "m": m1, "v": v1}
chained, w, m, v = [], w0, [0.0] * 8, [0.0] * 8
for step in range(1, 7):
    g, x, y, wt, outs = step_graph(w, m, v, step)
    g._nodes[x._id]["data"], g._nodes[y._id]["data"] = [c for row in xs[step - 1] for c in row], list(ys[step - 1])
    # The in-guest float32 reference (a hosted submit would answer later).
    out = execute_locally(g.program(**outs))["outputs"]
    chained.append(out["loss"]["data"][0]); w, m, v = out["w"]["data"], out["m"]["data"], out["v"]["data"]
def close(a, b):
    return len(a) == len(b) and all(abs(p - q) <= 1e-6 * max(1.0, abs(q)) for p, q in zip(a, b))
g, x, y, wt, outs = step_graph(w0, [0.0] * 8, [0.0] * 8, 1)
events = []
session = g.prepare(feeds={"x": x, "y": y}, carry={wt: "w", g._tensors[g._nodes[outs["m"]._id]["a"]]: "m", g._tensors[g._nodes[outs["v"]._id]["a"]]: "v"},
                    resident=["w", "m", "v"], on_ready=lambda s: events.append("ready " + s.backend), on_error=lambda e: events.append("create failed " + e.code), **outs)
losses = []
def took(result):
    losses.extend(s["outputs"]["loss"]["data"][0] for s in result["steps"])
for s in range(3):
    session.run(took, on_error=lambda e: events.append("run failed " + e.code), x=xs[s], y=ys[s])
session.run_steps(took, [{"x": xs[s], "y": ys[s]} for s in range(3, 6)], on_error=lambda e: events.append("run failed " + e.code))
def got(result):
    events.append("downloaded w " + str(result["outputs"]["w"]["shape"]) + " close " + str(close(result["outputs"]["w"]["data"], w)))
    events.append("losses close to the reference " + str(close(losses, chained)) + " " + str(len(losses)))
    session.dispose()
    try:
        session.run(print, x=xs[0], y=ys[0])
    except ComputeError as e:
        events.append("after dispose " + e.code)
session.download(got, "w", on_error=lambda e: events.append("download failed " + e.code))
def report():
    print(*events, sep="; ")
print("prepared", session.backend)
`;
  for (const backend of ["cpu-js", "wasm"]) {
    const e = new Engine();
    const runtime = await createRuntime({ backend, wasmBytes });
    const adapter = createPythonGPUAdapter(e, runtime, { allowExecute: true, maxSessions: 2 });
    e.initPythonProject({ main: SESSION }, "main");
    eq(`${backend}: the session is requested, not created, before the host drains`, e.takeOutput(), ["prepared None"]);
    const kinds = [];
    for (let i = 0; i < 12 && (adapter.pending || i === 0 || e.pythonCall("__zipp_py_pending_host", []) > 0); i++) {
      for (const r of e.takeHostRequests()) { kinds.push(r.kind); adapter.admit(r).catch(() => {}); }
      await adapter.idle();
    }
    eq(`${backend}: the guest raised create, four runs, a download and a dispose in order`, kinds,
      ["gpu.session.create", "gpu.session.run", "gpu.session.run", "gpu.session.run", "gpu.session.run", "gpu.session.download", "gpu.session.dispose"]);
    e.pythonCall("report", []);
    eq(`${backend}: six session steps equal six chained submits, the weight came back, and the disposed session refuses`, e.takeOutput(),
      [`ready ${backend}; downloaded w [4, 2] close True; losses close to the reference True 6; after dispose DISPOSED`]);
    ok(`${backend}: the adapter holds no session after the guest disposed it`, adapter.sessions === 0 && runtime.sessions.size === 0);
    adapter.invalidate(); await adapter.idle(); runtime.dispose(); e.dispose();
  }
  {
    // A guest that never disposes: invalidating the adapter frees its sessions.
    const e = new Engine();
    const runtime = await createRuntime({ backend: "cpu-js" });
    const adapter = createPythonGPUAdapter(e, runtime, { allowExecute: true });
    e.initPythonProject({ main: "from zipp_gpu import Graph\ng = Graph()\na = g.tensor([1.0, 2.0])\nb = g.tensor([1.0, 1.0])\ns = g.prepare(carry={a: 'r'}, resident=['r'], r=a + b)\ns.run(lambda r: print('ran', r['step']), readback=['r'])\n" }, "main");
    adapter.drain(); await adapter.idle(); adapter.drain(); await adapter.idle();
    eq("a session with nothing to feed runs on the host", e.takeOutput(), ["ran 2"]);
    ok("the session is alive until its generation ends", adapter.sessions === 1 && runtime.sessions.size === 1);
    adapter.invalidate();
    ok("invalidating the generation disposes the tenant's sessions", adapter.sessions === 0 && runtime.sessions.size === 0);
    await adapter.idle(); runtime.dispose(); e.dispose();
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

  // ---- a poisoned hosted session is still disposed on the host -----------------------------
  // The second run's second step fails inside the backend after its first step advanced the
  // carry: the host poisons the session and says so in the reply; Python's wrapper refuses
  // further runs with STATE, and its dispose() must still reach the host, or the handler
  // keeps the session (and its resident tensors) until the whole generation is torn down.
  {
    const { ComputeRuntime } = await import(pathToFileURL(path.join(gpuLab, "src", "runtime.mjs")).href);
    const { CPUBackend } = await import(pathToFileURL(path.join(gpuLab, "src", "backends", "cpu.mjs")).href);
    const { ComputeError } = await import(pathToFileURL(path.join(gpuLab, "src", "graph.mjs")).href);
    class Fail extends CPUBackend {
      constructor() { super(); this.adds = 0; }
      async run(n, r) { if (n.op === "add" && ++this.adds === 3) throw new ComputeError("BACKEND", "device lost"); return super.run(n, r); }
    }
    const e = new Engine();
    const runtime = new ComputeRuntime(new Fail());
    const adapter = createPythonGPUAdapter(e, runtime, { allowExecute: true, maxSessions: 2 });
    e.initPythonProject({ main: `from zipp_gpu import Graph, ComputeError
g = Graph(); a = g.tensor([1.0, 1.0]); x = g.tensor([0.0, 0.0]); nxt = a + x
events = []
s = g.prepare(feeds={"x": x}, carry={a: nxt}, on_ready=lambda s: events.append("ready " + s.backend),
              on_error=lambda e: events.append("create failed " + e.code), next=nxt)
s.run(lambda r: events.append("step 1 " + str(r["outputs"]["next"]["data"])), on_error=lambda e: events.append("run 1 failed " + e.code), x=[1.0, 1.0])
def failed(e):
    events.append("run 2 failed " + e.code)
    try:
        s.run(print, x=[1.0, 1.0])
    except ComputeError as err:
        events.append("then " + err.code)
    s.dispose()
    events.append("disposed")
s.run_steps(lambda r: events.append("run 2 succeeded?!"), [{"x": [1.0, 1.0]}, {"x": [1.0, 1.0]}], on_error=failed)
def report():
    print(*events, sep="; ")
` }, "main");
    const kinds = [];
    for (let i = 0; i < 12 && (adapter.pending || i === 0 || e.pythonCall("__zipp_py_pending_host", []) > 0); i++) {
      for (const r of e.takeHostRequests()) { kinds.push(r.kind); adapter.admit(r).catch(() => {}); }
      await adapter.idle();
    }
    e.pythonCall("report", []);
    eq("poisoned host session: the failed run says BACKEND, the next run is refused with STATE, dispose completes", e.takeOutput(),
      ["ready cpu-js; step 1 [2.0, 2.0]; run 2 failed BACKEND; then STATE; disposed"]);
    eq("poisoned host session: create, two runs and the dispose reached the host", kinds,
      ["gpu.session.create", "gpu.session.run", "gpu.session.run", "gpu.session.dispose"]);
    ok("poisoned host session: the handler and the runtime hold no session after Python's dispose()",
      adapter.sessions === 0 && runtime.sessions.size === 0, ` adapter ${adapter.sessions} runtime ${runtime.sessions.size}`);
    adapter.invalidate(); runtime.dispose(); e.dispose();
  }
}

main().then(() => {
  console.log(`\n${pass} passed, ${fail} failed`);
  process.exit(fail ? 1 : 0);
}, (error) => {
  console.error(error);
  process.exit(1);
});
