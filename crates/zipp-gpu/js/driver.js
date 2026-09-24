// The native host's driver: what the playground's Worker does with
// `createPythonGPUAdapter`, minus the Worker. One gpu-lab runtime, one
// `createZippGPUHandler` (the five `gpu.*` request kinds, the session map,
// the backend check), and a reply list the Rust host drains after each call.
//
// Every WebGPU promise the shim returns is already settled, so a request
// started by `__zgpuHandle` has finished by the time the engine call that
// started it returns (the embedding API drains microtasks after each call);
// `__zgpuTake` then hands its reply to the host, which returns it to the
// Python program's synchronous request (crates/zipp-gpu/src/bridge.rs).
var __zgpuRuntime = null;
var __zgpuHandler = null;
var __zgpuReplies = [];
var __zgpuInitError = null;
var __zgpuAdapter = null;

// `policy` 'native': the trusted CLI's limits. gpu-lab's defaults are a
// browser tab's (64 MiB of graph storage, 4M elements a tensor, 100M-1G
// estimated operations); the native CPU evaluator a hosted program would
// otherwise run on has none of them, so here the device alone bounds a graph:
// a tensor up to the largest storage binding, no work budget, and storage up
// to 8 GiB (allocation beyond the device's memory fails as a GPU error).
// Node, output, session and step-count limits stay the protocol's.
function __zgpuInit(backend, policy, adapter, replay) {
  __zgpuInitError = null;
  __zgpuReplayOn = replay !== false;
  __zgpuAdapter = adapter ? String(adapter) : null;
  const {createRuntime} = __zgpuModules['src/runtime.mjs'];
  const {createZippGPUHandler} = __zgpuModules['src/zipp-adapter.mjs'];
  createRuntime({backend}).then((runtime) => {
    if (policy === 'native' && runtime.impl && runtime.impl.device) {
      const d = runtime.impl.device.limits;
      const cap = Math.floor(Math.min(d.maxStorageBufferBindingSize, d.maxBufferSize) / 4);
      runtime.limits = {maxElements: cap, maxInputElements: cap, maxOutputElements: cap,
        maxWork: Number.MAX_SAFE_INTEGER, maxLogicalBytes: 8 * 1024 * 1024 * 1024, ...runtime.limits};
      // Keep up to 2 GiB of freed buffers for reuse (the browser keeps 256
      // MiB): a large model's steps then allocate nothing after the first.
      runtime.impl.poolBytes = Math.max(runtime.impl.poolBytes || 0, 2 * 1024 * 1024 * 1024);
      // Direct3D 12 with FXC takes ~18 s to compile the register-blocked
      // matmul tile (the 16x16 kernel: half a second; DXC, when a
      // dxcompiler.dll is there, 1.5 s). The tile changes no result bit, only
      // speed, so D3D12 on FXC keeps the 16x16 one.
      if (/\(dx12\b.*\bfxc\)$/.test(__zgpuAdapter || '') && 'matmulTile' in runtime.impl) runtime.impl.matmulTile = 1;
    }
    __zgpuRuntime = runtime;
    __zgpuHandler = createZippGPUHandler(runtime, {allowExecute: true, maxSessions: 16});
  }, (error) => { __zgpuInitError = String(error && error.message ? error.message : error); });
  return null;
}

// Called after the drain: whether the runtime came up, and what it runs on.
function __zgpuStatus() {
  if (__zgpuInitError !== null) return {ok: false, error: __zgpuInitError};
  if (__zgpuRuntime === null) return {ok: false, error: 'the GPU runtime did not finish initializing'};
  const info = __zgpuRuntime.info();
  return {ok: true, backend: info.backend, description: info.description, adapter: info.adapter, limits: info.limits};
}

// Float32Arrays of the current request that the host built for it alone and
// found all finite (`finite` below): js/accelerate.js's float32Data takes them
// as they are. A WeakSet, so an array leaves it with the request's payload.
var __zgpuOwnedFinite = new WeakSet();
function __zgpuMarkOwned(value, depth) {
  if (value instanceof Float32Array) { __zgpuOwnedFinite.add(value); return; }
  if (depth > 4 || value === null || typeof value !== 'object') return;
  if (Array.isArray(value)) { for (let i = 0; i < value.length; i++) __zgpuMarkOwned(value[i], depth + 1); return; }
  for (const key of Object.keys(value)) __zgpuMarkOwned(value[key], depth + 1);
}

// ---- Native replay of prepared steps (crates/zipp-gpu/src/replay.rs) ----
//
// The handler names sessions by token; its map is its own, so the session a
// token names is found as the one `gpu.session.create` added to the runtime.
// A session that has run once has fixed buffers for everything it holds;
// its next single-step run through gpu-lab is captured (the command script,
// in the host, and here which buffer each feed and read-back landed in and
// which uniform words depend on the step), and later runs are replayed by
// the host, which asks `__zgpuReplayBegin` for each step's uniform words
// (computed by gpu-lab's own adamStep, as session.run computes them) and
// reports back through `__zgpuReplayEnd`.
var __zgpuReplayOn = true;
var __zgpuSessions = new Map();   // token -> Session
var __zgpuReplays = new Map();    // token -> replay record
var __zgpuWord = new Float32Array(1), __zgpuBits = new Uint32Array(__zgpuWord.buffer);

// Each step-dependent uniform word, as session.run's derived node fills it
// (webgpu.mjs: adam_update's f[20] stepSize and f[21] bc2Sqrt, uniform's
// u[17] step): [word, bits, ...].
function __zgpuPatches(dependent, stepNo) {
  const {adamStep} = __zgpuModules['src/graph.mjs'];
  const out = [];
  for (const {word, node: n} of dependent) {
    if (n.op === 'adam_update') {
      const v = stepNo === 1 ? n : adamStep(n.raw, n.step + stepNo - 1);
      __zgpuWord[0] = v.stepSize; out.push(word + 20, __zgpuBits[0]);
      __zgpuWord[0] = v.bc2Sqrt; out.push(word + 21, __zgpuBits[0]);
    } else out.push(word + 17, (n.step + stepNo - 1) >>> 0);
  }
  return out;
}

// Wraps the backend for one session run: what the replay record needs.
function __zgpuCaptureStart(session) {
  const impl = session.impl, cap = {feeds: new Map(), slots: [], complete: null, node: null};
  const run = impl.run, uniform = impl.uniform, complete = impl.complete;
  impl.run = function (n, refs) {
    const outer = cap.node;
    cap.node = n;
    return run.call(this, n, refs).then((h) => {
      if (n.op === 'input') cap.feeds.set(n.id, h);
      cap.node = outer;
      return h;
    });
  };
  // A fused Adam group's one dispatch carries its adam_update's step words.
  const adam = impl.adam;
  if (adam) impl.adam = function (m, v, u, ...rest) {
    const outer = cap.node;
    cap.node = u;
    return adam.call(this, m, v, u, ...rest).then((hs) => { cap.node = outer; return hs; });
  };
  impl.uniform = function (fill) { const offset = uniform.call(this, fill); cap.slots.push([offset, cap.node]); return offset; };
  impl.complete = function (handles) { cap.complete = handles.slice(); return complete.call(this, handles); };
  __zippHostCall('gpu.capture', impl.device._id, '1');
  cap.restore = () => {
    delete impl.run; delete impl.uniform; delete impl.complete; delete impl.adam;
    __zippHostCall('gpu.capture', impl.device._id, '0');
  };
  return cap;
}

function __zgpuReplayRecord(token, session, cap, payload) {
  const impl = session.impl, plan = session.plan, nodes = plan.nodes;
  const names = payload.readback ?? plan.outputs.map((o) => o.name).filter((n) => !session.resident.has(n));
  const wanted = new Set(names), outs = plan.outputs.filter((o) => wanted.has(o.name));
  if (!cap.complete || cap.complete.length !== outs.length || wanted.size !== names.length) return;
  // Step-dependent dispatches: one uniform slot each.
  const dependent = [], seen = new Set();
  for (const [offset, node] of cap.slots) {
    if (!node || (node.op !== 'adam_update' && node.op !== 'uniform')) continue;
    if (seen.has(node.id)) return;
    seen.add(node.id); dependent.push({word: offset / 4, node: nodes[node.id]});
  }
  const template = new Uint8Array(impl.uniformData, 0, impl.slot * 256).slice();
  // The words as this run filled them must be what the patch rule gives for its step.
  const words = new Uint32Array(template.buffer), check = __zgpuPatches(dependent, session.stepNumber - 1);
  for (let i = 0; i < check.length; i += 2) if (words[check[i]] !== check[i + 1]) return;
  const feeds = [];
  let feedElements = 0;
  for (const [id, h] of cap.feeds) {
    const n = nodes[id];
    if (n.op !== 'input' || n.quant) return;
    feeds.push({id, buffer: h.buffer._id, size: n.size, classes: n.classes ?? null, bound: n.indexBound ?? null});
    feedElements += n.size;
  }
  const held = [...session.retained.keys()].map((h) => h.buffer._id);
  const outputs = outs.map((o, i) => ({name: o.name, buffer: cap.complete[i].buffer._id, size: nodes[o.id].size, shape: [...nodes[o.id].shape]}));
  const spec = {uniform: impl.uniformBuffer._id, staging: impl.staging ? impl.staging._id : -1, held, feeds, outputs};
  __zgpuShim.upload(template);
  if (String(__zippHostCall('gpu.replayCreate', impl.device._id, token, JSON.stringify(spec), template.byteLength)) !== '1') return;
  let readbackElements = 0;
  for (const o of outputs) readbackElements += o.size;
  __zgpuReplays.set(token, {session, dependent, feedElements, readbackElements});
}

function __zgpuReplayDrop(token) {
  if (__zgpuReplays.delete(token)) {
    try { __zippHostCall('gpu.replayDrop', __zgpuRuntime.impl.device._id, token); } catch (e) { /* device gone */ }
  }
}

// The host is about to replay `count` steps of `token`'s session: the checks
// session.run makes before any device work, then each step's uniform words.
// null: let gpu-lab run it (it gives the result or error it always gives).
function __zgpuReplayBegin(token, count, step) {
  const r = __zgpuReplays.get(token);
  if (!r || __zgpuHandler === null) return null;
  const s = r.session, limits = s.plan.limits;
  if (s.disposed || s.poisoned || s.busy || s.runtime.busy || s.runtime.disposed || __zgpuSessions.get(token) !== s) {
    __zgpuReplayDrop(token);
    return null;
  }
  if (!(count >= 1 && count <= limits.maxStepsPerRun)) return null;
  const first = step === null || step === undefined ? s.stepNumber : step;
  if (!(Number.isSafeInteger(first) && first >= 1 && first + count - 1 <= 2 ** 31)) return null;
  if (r.readbackElements * count > limits.maxOutputElements || r.feedElements * count > limits.maxInputElements) return null;
  const patches = [];
  try { for (let i = 0; i < count; i++) patches.push(__zgpuPatches(r.dependent, first + i)); } catch (e) { return null; }
  s.busy = s.runtime.busy = true;
  return {first, patches, nodes: s.plan.nodes.length, estimatedWork: s.plan.work * count,
    logicalAllocationBytes: s.plan.logicalBytes, residentBytes: s.residentBytes};
}

// The replay ran (`ok`) or failed once device work had begun (the session
// is then poisoned, as a failed session.run leaves it).
function __zgpuReplayEnd(token, count, ok, first) {
  const r = __zgpuReplays.get(token);
  if (!r) return null;
  const s = r.session;
  s.busy = s.runtime.busy = false;
  if (ok) { s.stepNumber = first + count; s.runs++; } else s.poisoned = true;
  if (s.disposeRequested) s.dispose();
  return {step: s.stepNumber, peak: s.impl.peakBufferBytes};
}

function __zgpuHandle(id, kind, payload, finite) {
  if (finite === true) __zgpuMarkOwned(payload, 0);
  const token = payload !== null && typeof payload === 'object' && typeof payload.session === 'string' ? payload.session : null;
  let before = null, capture = null, session = null;
  if (kind === 'gpu.session.create') before = new Set(__zgpuRuntime.sessions);
  else if (kind === 'gpu.session.dispose' && token !== null) { __zgpuReplayDrop(token); __zgpuSessions.delete(token); }
  else if (kind === 'gpu.session.run' && token !== null && __zgpuReplayOn && !__zgpuReplays.has(token)) {
    session = __zgpuSessions.get(token);
    // A session that has run holds fixed buffers; one step, WebGPU only.
    if (session && session.runs >= 1 && !session.poisoned && Array.isArray(payload.steps) && payload.steps.length === 1 &&
        session.impl.name === 'webgpu' && typeof session.impl.carry === 'function')
      try { capture = __zgpuCaptureStart(session); } catch (e) { capture = null; }
  }
  if (capture !== null) {
    const done = () => { if (capture !== null) { const c = capture; capture = null; c.restore(); return c; } return null; };
    const handled = __zgpuHandleNow(id, kind, payload);
    handled.then((ok) => {
      const c = done();
      if (ok && c !== null && !session.poisoned && !session.disposed)
        try { __zgpuReplayRecord(token, session, c, payload); } catch (e) { /* no replay: gpu-lab runs every step */ }
    }, () => { done(); });
    return null;
  }
  const handled = __zgpuHandleNow(id, kind, payload);
  if (before !== null) handled.then((ok) => {
    if (!ok) return;
    const value = __zgpuReplies.length ? __zgpuReplies[__zgpuReplies.length - 1][1].value : null;
    for (const s of __zgpuRuntime.sessions) if (!before.has(s) && value && typeof value.session === 'string') __zgpuSessions.set(value.session, s);
  });
  return null;
}

// Starts the request; the promise settles (to whether it succeeded) after
// its reply is queued.
function __zgpuHandleNow(id, kind, payload) {
  // The browser adapter's rule: an engine that sends tensor inputs as
  // Float32Arrays takes outputs the same way; sessions are always typed.
  const typedOutputs = kind !== 'gpu.execute' ||
    (payload !== null && typeof payload === 'object' && Array.isArray(payload.nodes) &&
      payload.nodes.some((node) => node !== null && typeof node === 'object' && node.data instanceof Float32Array));
  let started;
  try {
    started = __zgpuHandler.handle(kind, [payload], {typedOutputs});
  } catch (error) {
    started = Promise.reject(error);
  }
  return started.then((value) => {
    // Which device ran it: the browser names only the backend; natively the
    // adapter is worth reporting too (an additive, informational field).
    if (__zgpuAdapter !== null && value !== null && typeof value === 'object' && value.stats !== null && typeof value.stats === 'object')
      value.stats.adapter = __zgpuAdapter;
    __zgpuReplies.push([id, {ok: true, value}]);
    return true;
  }, (error) => {
    const code = error && error.code ? String(error.code) : 'GPU';
    const message = String(error && error.message ? error.message : error).slice(0, 512);
    const reply = {ok: false, error: {code, message}};
    if (error && error.poisoned) reply.error.poisoned = true;
    __zgpuReplies.push([id, reply]);
    return false;
  });
}

function __zgpuTake() {
  const out = __zgpuReplies;
  __zgpuReplies = [];
  return out;
}

function __zgpuDispose() {
  for (const token of [...__zgpuReplays.keys()]) __zgpuReplayDrop(token);
  __zgpuSessions.clear();
  if (__zgpuHandler !== null) { __zgpuHandler.invalidate(); __zgpuHandler = null; }
  if (__zgpuRuntime !== null) { try { __zgpuRuntime.dispose(); } catch (e) { /* busy or gone */ } __zgpuRuntime = null; }
  return null;
}
