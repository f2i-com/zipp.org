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
function __zgpuInit(backend, policy, adapter) {
  __zgpuInitError = null;
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
      // Direct3D 12 compiles WGSL through FXC, which takes ~20 s over the
      // register-blocked matmul tile (the 16x16 kernel: half a second). The
      // tile changes no result bit, only speed, so D3D12 keeps the 16x16 one.
      if (/\(dx12[,)]/.test(__zgpuAdapter || '') && 'matmulTile' in runtime.impl) runtime.impl.matmulTile = 1;
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

function __zgpuHandle(id, kind, payload, finite) {
  if (finite === true) __zgpuMarkOwned(payload, 0);
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
  started.then((value) => {
    // Which device ran it: the browser names only the backend; natively the
    // adapter is worth reporting too (an additive, informational field).
    if (__zgpuAdapter !== null && value !== null && typeof value === 'object' && value.stats !== null && typeof value.stats === 'object')
      value.stats.adapter = __zgpuAdapter;
    __zgpuReplies.push([id, {ok: true, value}]);
  }, (error) => {
    const code = error && error.code ? String(error.code) : 'GPU';
    const message = String(error && error.message ? error.message : error).slice(0, 512);
    const reply = {ok: false, error: {code, message}};
    if (error && error.poisoned) reply.error.poisoned = true;
    __zgpuReplies.push([id, reply]);
  });
  return null;
}

function __zgpuTake() {
  const out = __zgpuReplies;
  __zgpuReplies = [];
  return out;
}

function __zgpuDispose() {
  if (__zgpuHandler !== null) { __zgpuHandler.invalidate(); __zgpuHandler = null; }
  if (__zgpuRuntime !== null) { try { __zgpuRuntime.dispose(); } catch (e) { /* busy or gone */ } __zgpuRuntime = null; }
  return null;
}
