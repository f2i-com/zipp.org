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
supported GPU inference and training steps (dense layers with relu, gelu, sigmoid
or tanh, softmax, MSE or fused cross-entropy, SGD with momentum, Adam or AdamW,
comparisons and masks, where/masked_fill, clamp, hardtanh/relu6/leaky_relu,
maximum/minimum and dropout drawn on the device).
General Python is not compiled to shaders. See the [Torch compatibility guide](../../../docs/TORCH_COMPATIBILITY.md).

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

Graph IR v2 (stage 1: MLP classification training and inference) and v3
(masks, selection and on-device random numbers) cover, on every backend,
float32 tensors of rank 0-4:

| Family | Operations |
|---|---|
| Sources | `input`, `full`; v3: `uniform` (see below) |
| Elementwise | `add`, `sub`, `mul`, `div` with NumPy broadcasting; `neg`, `exp`, `log`, `sqrt`, `tanh`, `sigmoid`, `relu`, `positive` (ReLU mask), `gelu` and `gelu_grad` |
| v3 elementwise | `maximum`, `minimum` (NaN from either side; a tie, -0 against +0 included, returns `a`, as PyTorch does); comparisons `eq`, `ne`, `lt`, `le`, `gt`, `ge` returning float32 1/0 masks (a NaN operand satisfies only `ne`); all with NumPy broadcasting |
| v3 selection | `where` with `c`, `a`, `b` (three-way broadcasting): `a` where `c` is nonzero (a NaN counts as nonzero), `b` where it is +0 or -0 |
| Shape | `reshape` (shares storage), `permute`, `transpose` (matrices) |
| Reductions | `sum`, `mean` over one `axis` (with `keepdim`) or the whole tensor |
| Rows | `softmax`, `log_softmax` over the last axis (max-subtracted) |
| Linear algebra | `matmul`: [M,K]@[K,N] and batched [B,M,K]@[B,K,N] (a batch of 1 or a matrix broadcasts) |
| Integer linear algebra | `matmul_fixed`: the same product over int16 quants and an exact integer sum, `transposed` only. The weight is quantized per step, or once beforehand as an `i16` input with its scales. cpu-js and wasm; the GPU backends refuse it. See [docs/FIXED-POINT.md](docs/FIXED-POINT.md) |
| Losses | `cross_entropy` (mean over rows, integer class targets) and `cross_entropy_grad` = (softmax - onehot)/N |
| Optimizers | `sgd_update`, `momentum_update`, `adam_m`, `adam_v`, `adam_update` (PyTorch's update order) |
| Other | toroidal `life` |

`uniform` (`shape`, `seed` an integer in [0, 2^32), `step` an integer in
[1, 2^31], default 1) is a counter-based generator: element i is
`mix(mix(i ^ k2) + k1) >> 8` times 2^-24, with k1 = `mix(seed ^ 0x9e3779b9)`,
k2 = `mix(step ^ k1)` and `mix` Chris Wellons' lowbias32 hash, all in 32-bit
unsigned arithmetic. It is integer-exact, so every backend (and `zipp_gpu`'s
Python reference) draws the same float32 bits in [0, 1); it is not PyTorch's
random stream. A prepared session evaluates it at the recorded `step` plus the
session step minus one, as it advances `adam_update`, so one recorded dropout
mask (`ge(uniform, p)`) is fresh at every step. Comparisons, `maximum`,
`minimum`, `where` and `uniform` are exact on every backend; the browser harness
holds WebGPU and WebGL2 to bit equality with cpu-js on them (under ANGLE's
Direct3D backend `x < y ? y : x` lost a tied zero's sign, so the shaders decide
ties before ordering).

GELU is the exact-erf form 0.5*x*(1 + erf(x/sqrt(2))). No backend language has
erf, so all of them evaluate one shared approximation (a Taylor series below 0.5
and Numerical Recipes' erfc fit above it, fractional error below 1.2e-7); see
`src/kernel-math.mjs`. MSE needs no fused op: `sub`, `mul` and `mean` express it
and its gradient. Optimizer nodes return the next value of a tensor, so a whole
training step (forward, loss, backward and update of every parameter and moment)
runs on the device as one graph; the next step feeds those outputs back as inputs.

Only named outputs are read back. Inputs and outputs are finite float32: NaN
produced inside a graph (overflow, then `inf - inf`) propagates through ReLU and
the reductions and fails readback with `NUMBER` on every backend, including the
GPUs checked here. One graph runs at a time; await it before submitting another.
Dispose the runtime after outstanding work finishes. Protocol versions 1, 2
and 3 are accepted; each adds operations and every older graph means the same
thing under a newer version. `zipp_gpu.Graph.program()` labels a graph 2 only
once it uses something version 1 did not define (a new operation, rank above
two, broadcasting beyond a scalar operand, an axis reduction or a batched
matmul), and 3 only once it uses `maximum`, `minimum`, a comparison, `where` or
`uniform`, so graphs an older host understands still arrive labelled the way
it expects and a version-2 host refuses a version-3 graph instead of misreading it.

Default validation limits include 512 nodes, 4,194,304 elements per tensor,
65,536 per dimension, 64 MiB summed **logical** node storage, 100 million
estimated operations and 64 outputs. A backend may raise the work budget
(SIMD WASM 400M, WebGL2 500M, WebGPU 1G) and lower size limits to what its
device holds; host limits passed to `createRuntime({limits: ...})` always win,
and `runtime.info().limits` reports the result. `maxWebGLTextureBytes`
separately limits live WebGL textures to 128 MiB, including padding, reduction
scratch and pooled textures. WebGL2 stores one scalar per R32F texel (4 bytes)
where R32F is renderable and falls back to RGBA32F (16 bytes). The result reports
`stats.webglTexturePeakBytes`. This is requested texture storage, not a bound on
all driver VRAM, shader objects or CPU staging/readback memory.

The GPU backends record each execution as one WebGPU command buffer (one submit,
one readback map) or one run of WebGL draws, reuse buffers and textures across
executions, and key pipelines by kernel with shapes passed as uniforms. Driver
errors are checked once per execution; `createRuntime({debug: true})` restores
per-dispatch WebGL error/framebuffer checks and per-node WebGPU error scopes.

Limits are host policy. The adapter also bounds pending requests, refuses work
over quota through the program's own error callback (asynchronously, so a
callback that resubmits cannot recurse), and rejects work after tenant invalidation.

## Source map

- `src/graph.mjs`: protocol, shapes and work/size validation.
- `src/runtime.mjs`, `src/backends/`: scheduling, shaders, fallback kernels and cleanup.
- `src/kernel-math.mjs`: the scalar definitions (erf-based GELU, sigmoid, NaN-keeping ReLU) every backend implements.
- `src/zipp-python-adapter.mjs`: actual Python host bridge.
- `src/zipp-adapter.mjs`, `src/zipp-guest.js`: opt-in JavaScript guest integration.
- `../../zipp-vm/src/frontend/python/lib/shared/zipp_gpu.py`: native Python graph authoring/export library.
- `src/quant.mjs`: Q4_K block decoding, a port of ggml's own, checked against
  the `f2i-gguf-quants` crate rather than only against itself. An `input` node may
  declare `dtype: 'q4_k'` and carry blocks instead of values; a `matmul` with
  `transposed: true` reads them as it goes. The weight stays 144 bytes per 256
  values on the device — 7.1 times smaller — and the result is bit for bit what
  the same matmul over decoded values gives, because decoding is exact. All four
  backends do this; each has its own decoder, and each is held to that equality.
  `scripts/check-gpu-matmul.cjs` measures the two GPU ones in a real browser.
- `docs/FIXED-POINT.md`: `matmul_fixed`, the one operation here that is *not*
  float32. Both sides are quantized to int16 with a per-row scale and the
  products are summed as integers, so cpu-js and wasm reach the same answer by
  construction rather than by agreeing about rounding order — which is what a
  proof system over a prime field needs, and what lets a redundant-execution
  check compare for equality instead of within a tolerance. It costs 1.8e-04
  median relative error against `matmul`. Quantising the weight once at bind
  time -- `quantizeWeight`, an `i16` input and its scales -- gives bit-identical
  results and, at m = 1, runs *faster* than the float32 matmul (0.21 ms against
  0.30) by skipping the decode, for 3.56 times the resident bytes. WebGPU and
  WebGL2 have no 64-bit integer to accumulate in and refuse the graph rather
  than returning zeros.
- `wasm/kernels.wasm`: the COMMITTED binary of the freestanding kernels, built
  from `../rust/zipp-kernels` (`no_std` Rust, SIMD, no allocator, no imports).
  `npm test`, the Node GPU suites and the `javascript-python` release archive
  all load this exact blob. Rebuild it with `scripts/build_wasm.sh` — cargo
  alone, no separate link step, `--locked`, Rust 1.92.0 — and commit the source
  change and the new binary together. The build does not depend on where it ran
  -- recorded paths are remapped and symbol names stripped, and a Linux and a
  Windows build are byte-identical -- so CI rebuilds it and fails if the result
  differs from what is committed.
  `backend-bits.test.mjs` holds the compiled module to bit-for-bit equality with
  the JavaScript reference over whole training steps.
- `tests/`: numerical checks, allocation/lifecycle mocks and browser cases; `tests/ml-cases.mjs`
  holds the per-operation fixtures, the MLP training-step generator and the prepared
  dropout MLP shared with the browser; `tests/ir-v3.test.mjs` covers version 3 (NaN,
  ties and signed zeros, the generator's known answers and statistics, per-step draws).
- `docs/INTEGRATION.md`, `docs/ARCHITECTURE.md`, `docs/VALIDATION.md`: contracts and evidence.

## Check and rebuild

```sh
npm test
py -3.13 tests/test_python.py
python examples/run_native.py
python scripts/browser_smoke.py        # real WebGPU/WebGL2 in Chrome (Playwright); REQUIRE_GPU=1 to insist
```

After building a Python-enabled Node package in `../tests/node/pkg`, run
`node ../tests/node/python-frontend.cjs`, `node ../tests/node/python-gpu.cjs`
and `node ../tests/node/python-training.cjs`.
The dedicated Python CI lane builds this artifact explicitly and asserts Python
is enabled. Real WebGL2/WebGPU execution requires browser hardware: use
`scripts/browser_smoke.py` (serves this directory on 127.0.0.1, runs every case on
every backend against the JavaScript reference plus an MNIST-sized training step,
and writes `docs/browser-validation.json`), the demo's **Check all backends**, or
`landing/scripts/smoke-browser.py` with `REQUIRE_GPU=1`. On hybrid-GPU Windows
machines Chrome ignores `powerPreference`; the script passes
`--force_high_performance_gpu` (override with `GPU_FLAGS`). A missing GPU is not a
successful GPU check.

Rebuild the standalone kernel with `sh scripts/build_wasm.sh`: cargo with Rust
1.92.0 and the `wasm32-unknown-unknown` target. A Linux build and a Windows one
(Git Bash) produce the same bytes (built with `+simd128`; browsers without
WebAssembly SIMD fall back to JavaScript).
The kernel module is separate from Zipp WASM; rebuilding it does not rebuild Python.

This remains an experimental graph engine: no kernel fusion, persistent
cross-request tensors, float16, convolutions or general shader compiler. Matrix
products are tiled only on WebGPU (16x16 workgroup tiles) and register-blocked
in the SIMD WASM kernels. Measured timings are in `docs/VALIDATION.md`; at MNIST
scale a step is dominated by host transfers, not GPU arithmetic. Source and
kernels are Apache-2.0; see LICENSE and NOTICE.
