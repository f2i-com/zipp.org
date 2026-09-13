# Zipp browser GPU Lab

Python-authored float32 graphs, executed by a JavaScript host through WebGPU,
WebGL2, compiled WASM kernels or a JavaScript reference backend. This package is
integrated into Zipp's Python-enabled browser playground.

## Run Python on the browser GPU

From `crates/zipp-wasm`, build `./build-variants.sh all`, start
`node playground/serve.cjs`, and open the printed playground URL. Choose
**Samples → Python: GPU compute**, select WebGL2 or WebGPU and Run. The example
runs vector arithmetic, matrix multiplication, a tiny explicit-weight network,
and an animated Game of Life grid. The console reports the actual backend.

Python compiles to Zipp bytecode and executes inside the WASM Worker. Its built-in
`zipp_gpu` module records graphs; `src/zipp-python-adapter.mjs` drains
`takeHostRequests()` and calls `pythonCall("__zipp_py_deliver", ...)` with results.
The bundled `torch` subset is eager CPU-only; experimental `torch.compile` records
supported GPU inference and dense-model SGD training. General Python is not
compiled to shaders. See the [Torch compatibility guide](../../../docs/TORCH_COMPATIBILITY.md).

## Use JavaScript directly

Browser ES modules can import `createRuntime` from `src/runtime.mjs` and call
`await runtime.execute(graph)`. No Python or Zipp VM is required for that path.
See the [complete JavaScript and Python examples](../../../README.md#gpu-computing-from-javascript-and-python).
Guest JavaScript inside Zipp uses the separate explicit host queue adapter;
it is not automatically granted browser APIs by the stock playground.

## Diagnostic demo

`python scripts/serve.py` serves `demo/` with no npm install. It runs graphs
previously exported from `../../zipp-vm/src/frontend/python/lib/shared/zipp_gpu.py`, rather than executing the displayed
Python source. Use the project playground above to run/edit Python itself.
The native `examples/run_native.py` harness uses installed CPython and Node.

## Backends and limits

`auto` tries WebGPU, WebGL2, compiled WASM, then JavaScript. Failed initialization
attempts are reported by `runtime.info().fallbackAttempts`. Explicit backend
selection fails if unavailable. Hardware GPU paths reject recognized software
renderers. Browser/OS policy selects one adapter; this does not pool GPUs.

Supported operations are input/full tensors, scalar or shape-matched add/sub/mul,
ReLU, positive masks, matrix transpose/multiplication, sum and toroidal Life. Only named outputs are read
back. Inputs and outputs are finite float32. One graph runs at a time; await it
before submitting another. Dispose the runtime after outstanding work finishes.

Default validation limits include 512 nodes, 1,048,576 elements per tensor,
32 MiB summed **logical** node storage, 100 million estimated operations and
16 outputs. `maxWebGLTextureBytes` separately limits live WebGL textures to
128 MiB, including RGBA32F padding and reduction scratch. One scalar occupies a
16-byte RGBA texel, not four physical bytes. The result reports
`stats.webglTexturePeakBytes`. This is requested texture storage, not a bound on
all driver VRAM, shader objects or CPU staging/readback memory.

Limits are host policy and can be supplied through `createRuntime({limits: ...})`.
The adapter also bounds pending requests and rejects work after tenant invalidation.

## Source map

- `src/graph.mjs`: protocol, shapes and work/size validation.
- `src/runtime.mjs`, `src/backends/`: scheduling, shaders, fallback kernels and cleanup.
- `src/zipp-python-adapter.mjs`: actual Python host bridge.
- `src/zipp-adapter.mjs`, `src/zipp-guest.js`: opt-in JavaScript guest integration.
- `../../zipp-vm/src/frontend/python/lib/shared/zipp_gpu.py`: native Python graph authoring/export library.
- `wasm/kernels.c`, `wasm/kernels.wasm`: freestanding C kernels and included binary.
- `tests/`: numerical checks, allocation/lifecycle mocks and browser cases.
- `docs/INTEGRATION.md`, `docs/ARCHITECTURE.md`, `docs/VALIDATION.md`: contracts and evidence.

## Check and rebuild

```sh
npm test
python tests/test_python.py
python examples/run_native.py
```

After building a Python-enabled Node package in `../tests/node/pkg`, run
`node ../tests/node/python-frontend.cjs`, `node ../tests/node/python-gpu.cjs`
and `node ../tests/node/python-training.cjs`.
The dedicated Python CI lane builds this artifact explicitly and asserts Python
is enabled. Real WebGL2/WebGPU execution requires browser hardware: use the demo's
**Check all backends**, or `landing/scripts/smoke-browser.py` with `REQUIRE_GPU=1`.
A missing GPU is not a successful GPU check.

Rebuild the standalone kernel with `sh scripts/build_wasm.sh` using Clang/wasm-ld.
On Windows, use `./scripts/build_wasm.ps1` (LLVM defaults to
`C:\Program Files\LLVM\bin`; override with `-LlvmDirectory`).
The kernel module is separate from Zipp WASM; rebuilding it does not rebuild Python.

This remains an experimental graph engine: no kernel fusion, tiled matrix multiply,
persistent cross-request tensors or general shader compiler. Timings are not claims
of GPU speedup. Source and kernels are Apache-2.0; see LICENSE and NOTICE.
