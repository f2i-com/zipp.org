// Run in graph.mjs's scope, after its body (bundle.rs `bundle_with`): what
// is rebound here is what graph.mjs's own code, its exports and every module
// importing from it use.
//
// Two of gpu-lab's helpers walk a whole tensor in JavaScript, and this
// engine's typed-array loops cost about 60-130 ns an element (V8's are under
// 2), so on a per-call compiled training step they were most of the call:
//
// - `float32Data`, how gpu-lab takes ownership of a tensor it was given (a
//   float32 copy, every value checked finite): every input of an executed
//   program (validateProgram) and every feed of a prepared step.
// - `checkFiniteOutput`, readback's rule that every output value is finite:
//   every output of an execution or a session run.
//
// For a Float32Array (what a ZIPP Python program sends and what readback
// produces) the finiteness scan runs natively over a copy in `__zgpuUp`, and
// float32Data's copy is `slice()`. Anything else, and any array holding a
// non-finite value, takes gpu-lab's own function, so the result and every
// error are gpu-lab's.
{
  const float32DataInJs = float32Data;
  const checkFiniteOutputInJs = checkFiniteOutput;
  const nativeFinite = (data) => {
    __zgpuShim.upload(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
    return __zippHostCall('gpu.allFinite', data.length) === '1';
  };
  float32Data = function float32Data(data) {
    if (!(data instanceof Float32Array) || data.length < 256 || !nativeFinite(data)) return float32DataInJs(data);
    return data.slice();
  };
  checkFiniteOutput = function checkFiniteOutput(values) {
    if (!(values instanceof Float32Array) || values.length < 256 || !nativeFinite(values)) checkFiniteOutputInJs(values);
  };
}
