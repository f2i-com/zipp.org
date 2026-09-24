// Run in graph.mjs's scope, after its body (bundle.rs `bundle_with`): what
// is rebound here is what graph.mjs's own code, its exports and every module
// importing from it use.
//
// Four of gpu-lab's helpers walk a whole tensor in JavaScript, and this
// engine's typed-array loops cost about 60-130 ns an element (V8's are under
// 2), so on a per-call compiled training step they were most of the call:
//
// - `float32Data`, how gpu-lab takes ownership of a tensor it was given (a
//   float32 copy, every value checked finite): every input of an executed
//   program (validateProgram) and every feed of a prepared step.
// - `checkFiniteOutput`, readback's rule that every output value is finite:
//   every output of an execution or a session run.
// - `checkIndices` and `checkClassTargets`, that every index or class target
//   fed to a prepared step is an integer in range.
//
// For a Float32Array (what a ZIPP Python program sends and what readback
// produces) the scans run natively over a copy in `__zgpuUp`, and
// float32Data's copy is `slice()`. A request's own arrays, which the host
// built for it and already found finite (driver.js `__zgpuOwnedFinite`), are
// neither scanned nor copied. Anything else, and any array failing a check,
// takes gpu-lab's own function, so the result and every error are gpu-lab's.
{
  const float32DataInJs = float32Data;
  const checkFiniteOutputInJs = checkFiniteOutput;
  const nativeFinite = (data) => {
    __zgpuShim.upload(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
    return __zippHostCall('gpu.allFinite', data.length) === '1';
  };
  float32Data = function float32Data(data) {
    // The host's own copy of a request's tensor, already checked finite
    // (driver.js): nothing else holds it, so it is taken as it is.
    if (typeof __zgpuOwnedFinite === 'object' && __zgpuOwnedFinite.has(data)) return data;
    if (!(data instanceof Float32Array) || data.length < 256 || !nativeFinite(data)) return float32DataInJs(data);
    return data.slice();
  };
  // Index and class-target feeds: every value an integer in [0, bound),
  // checked natively; a failure takes gpu-lab's function for its error.
  const checkIndicesInJs = checkIndices, checkClassTargetsInJs = checkClassTargets;
  const nativeIndices = (data, bound) => {
    __zgpuShim.upload(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
    return __zippHostCall('gpu.allIndices', data.length, bound) === '1';
  };
  checkIndices = function checkIndices(data, bound) {
    if (!(data instanceof Float32Array) || data.length < 64 || !nativeIndices(data, bound)) checkIndicesInJs(data, bound);
  };
  checkClassTargets = function checkClassTargets(data, classes, op) {
    if (!(data instanceof Float32Array) || data.length < 64 || !nativeIndices(data, classes)) checkClassTargetsInJs(data, classes, op);
  };
  checkFiniteOutput = function checkFiniteOutput(values) {
    if (!(values instanceof Float32Array) || values.length < 256 || !nativeFinite(values)) checkFiniteOutputInJs(values);
  };
}
