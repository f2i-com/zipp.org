// Run between gpu-lab's graph.mjs and the modules that import from it.
//
// `float32Data` is how gpu-lab takes ownership of a tensor it was given: a
// float32 copy, every value checked finite. It is a per-element JavaScript
// loop, and this engine's typed-array loops cost about 60 ns an element
// (V8's cost under 2), so a prepared step spends most of its host time
// checking the batch it was fed. For a Float32Array (what a ZIPP Python
// program sends) the check runs natively over a copy in `__zgpuUp` and the
// copy is `slice()`; anything else, and any array holding a non-finite
// value, takes gpu-lab's own function, so the result and every error are
// gpu-lab's. Modules bind their imports when they load, so what imports
// `float32Data` after this point (session.mjs: every prepared step's feeds)
// gets this one; graph.mjs's own uses keep the original.
(function (graph) {
  "use strict";
  const original = graph.float32Data;
  graph.float32Data = function float32Data(data) {
    if (!(data instanceof Float32Array) || data.length < 256) return original(data);
    __zgpuShim.upload(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
    if (__zippHostCall('gpu.allFinite', data.length) !== '1') return original(data);
    return data.slice();
  };
})(__zgpuModules['src/graph.mjs']);
