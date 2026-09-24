// Loader for zipp_torch.wasm: adds the torch package to a ZIPP engine built
// without it (the `python` artifact).
//
//   import init, * as zipp from "./zipp_wasm.js";
//   import { addTorch } from "./zipp_torch.js";
//   await init();
//   await addTorch(zipp, "./zipp_torch.wasm");   // or a URL, Response, bytes or Module
//   new zipp.Engine().initSource("import torch\n...", "python");
//
// `addTorchSync(zipp, bytesOrModule)` does the same synchronously (Node, a
// Worker that already holds the bytes). The package is process-wide: add it
// once, before creating the engines that import torch. The engine checks the
// archive (format, engine ABI, SHA-256 of every file) and throws on any
// mismatch; pin the .wasm itself by hash (SRI, SHA256SUMS) as you would the
// engine: its runtime code and kernels run as part of the engine.
//
// The module has no imports and its own memory. Each tensor kernel call
// arrives as request bytes (what the engine's `vm::py_tensor::wire` writes),
// is copied into this module's memory, runs there, and its response is
// copied back.

function install(zipp, instance) {
  const x = instance.exports;
  const archive = new Uint8Array(x.memory.buffer, x.zipp_package_ptr(), x.zipp_package_len()).slice();
  const kernels = {
    zippTorchKernel(request) {
      const ptr = x.zipp_alloc(request.length);
      new Uint8Array(x.memory.buffer, ptr, request.length).set(request);
      const out = x.zipp_kernel(ptr, request.length);
      const len = new DataView(x.memory.buffer).getUint32(out, true);
      const response = new Uint8Array(x.memory.buffer, out + 4, len).slice();
      x.zipp_free(out, len + 4);
      return response;
    },
  };
  return JSON.parse(zipp.addPythonPackage(archive, kernels));
}

export function addTorchSync(zipp, wasm) {
  const module = wasm instanceof WebAssembly.Module ? wasm : new WebAssembly.Module(wasm);
  return install(zipp, new WebAssembly.Instance(module, {}));
}

export async function addTorch(zipp, source) {
  let module;
  if (source instanceof WebAssembly.Module) module = source;
  else if (typeof Response !== "undefined" && source instanceof Response) module = await WebAssembly.compileStreaming(source);
  else if (typeof source === "string" || source instanceof URL) module = await WebAssembly.compileStreaming(fetch(source));
  else module = await WebAssembly.compile(source);
  return install(zipp, await WebAssembly.instantiate(module, {}));
}
