# Validation and reproduction

The runtime is integrated into the Python-enabled Zipp WASM playground. The
original standalone report is preserved in [the historical archive](validation-archive-20260913.md),
along with its raw reports. Those old environment restrictions do not describe
current source integration.

Current checks are separated by what they establish:

| Check | Coverage |
|---|---|
| `npm test` in gpu-lab | JS/WASM numerical behavior, validators, adapter contracts, WebGPU lifecycle mocks, WebGL texture-budget bookkeeping, IR v2 differentials and finite-difference gradients; prepared sessions (`tests/sessions.test.mjs`: five Adam steps through a session equal five chained `execute()` calls bit for bit on cpu-js and WASM, carry/feed/resident validation, budgets and lifetime; the WebGPU mock pins one submit per multi-step run, resident buffers out of the pool and flat bind-group creation; the adapters pin opaque session ids and per-tenant disposal) |
| `py -3.13 tests/test_python.py` | Native Python graph construction/export, the `execute_locally` float32 reference, and `Graph.prepare` sessions on the reference path (equal to chained submits bit for bit, and to a Node cpu-js session within 2.5e-7 relative) |
| `py -3.13 tools/python_corpus.py` (repository root, default and `ZIPP_PY_NOFAST=1`) | `tests/python_corpus/ml_gpu_session.py`: the same session program under CPython and native `zipp py`, byte-identical output |
| `node ../tests/node/python-frontend.cjs` | Actual Python-enabled WASM ABI, projects, VFS mutations and dictionary conversion |
| `node ../tests/node/python-gpu.cjs` | Actual Python-to-host graph requests and JS/WASM evaluation |
| `python scripts/browser_smoke.py`, `REQUIRE_GPU=1` | Every IR v2 case on real WebGPU/WebGL2 hardware against the JavaScript reference, plus an MNIST-scale training step; writes `docs/browser-validation.json` |
| `landing/scripts/smoke-browser.py`, `REQUIRE_GPU=1` | Served playground, folder loading, examples, real hardware WebGL2/WebGPU and animation |
| `tests/browser-cases.mjs` in the diagnostic demo | the same numerical cases per selected browser backend |

The Python CI lane in `.github/workflows/ci.yml` builds the Python feature explicitly,
checks the artifact profile before running its boundary tests, and runs the native
VM/CLI Python tests. Existing workflow triggers remain manual/reusable while the
repository's automatic CI pause is in effect.

GPU correctness has been checked locally with RTX 5090 hardware; the browser
reports WebGL2 through ANGLE/D3D11 and WebGPU through the Blackwell adapter.
Portable mocks do not compile shaders. Unavailable GPU checks must be reported
as unavailable, never silently replaced by a CPU pass.

For repeatable numerical checks on real hardware, run
`REQUIRE_GPU=1 python scripts/browser_smoke.py` from this directory: it serves the
lab on 127.0.0.1 (a secure context, so WebGPU is exposed), runs every case and the
training step on each backend, and rewrites `docs/browser-validation.json`. An
unavailable GPU backend is recorded as unavailable and, under `REQUIRE_GPU=1`,
fails the run — it is never replaced by a CPU pass. For the served playground,
build the landing page, start its preview, and run
`python landing/scripts/smoke-browser.py http://127.0.0.1:4173` from the repository
root with `REQUIRE_GPU=1`. Both require Chrome and Python Playwright (installed for
this machine's 3.11 `python`, not for `py -3.13`).
The diagnostic demo provides **Check all backends** for the full numerical matrix.

Timings and GIFs are functional demonstrations, not performance benchmarks.
Shader compilation, transfers, driver memory and thermal/load differences affect
results. `maxLogicalBytes` and WebGL's explicit texture-byte ceiling are different
budgets; neither measures the entire browser's memory usage.

## Prepared sessions — 16 September 2026

Same machine, browser and adapters as the acceptance below; raw evidence in
`docs/browser-validation.json` (`training[*].executeTypedWarmMedianMs`,
`sessionOneStepWarmMedianMs`, `sessionEightStepsPerStepMedianMs`,
`sessionVsChainedMaxAbsError`). One 784-256-10, batch-64 Adam step, typed
outputs throughout, warm medians of 10 runs (8-step runs: median of 5 runs,
divided by 8):

| backend | (a) `execute()`, every output back | (b) session, 1 step per run, loss back | (c) session, 8 steps per run, per step | 5 session steps vs 5 chained executes |
|---|---|---|---|---|
| WebGPU (RTX 5090) | 7.50 ms | 3.60 ms | 0.79 ms | max abs error 0 |
| WebGL2 (ANGLE/D3D11) | 6.20 ms | 1.10 ms | 0.81 ms | 0 |
| WASM (SIMD, in-worker) | 3.30 ms | 1.70 ms | 1.64 ms | 0 |
| cpu-js | 27.1 ms | 35.6 ms | 23.4 ms | 0 |

What each layer removed on WebGPU, against the 6.6 ms measured before this
work (validate 1.3 ms, record 0.5 ms, Chrome's submit-to-completion wait
~2.6-2.9 ms, GPU work ~0.4 ms, 2.4 MB readback ~1.5 ms):

- **prepare once** removes `validateProgram` and the input copies from every
  step (1.3 ms), and the pre-created pipelines/bind groups plus the dynamic
  uniform offsets remove most of the recording (0.5 ms; warm runs create no
  bind group at all);
- **carried tensors** remove the 2.4 MB readback and the 1 MB re-upload of
  weights and moments (1.5 ms plus upload); a step uploads 200 KB of `x` and
  targets and reads back four bytes;
- **one submit for eight steps** amortizes the one cost that cannot be
  removed from JavaScript: any await that waits for GPU work to finish (a
  `mapAsync`, or `onSubmittedWorkDone` after a real dispatch) returns after
  ~2.6 ms in this Chrome, while `popErrorScope` and an empty submit cost
  0.1 ms (probe: `submit + mapAsync` 2.60 ms, `submit + onSubmittedWorkDone`
  0.10 ms). A one-step run is therefore floor-bound at 2.6 + ~0.4 GPU + ~0.6
  host ms; eight steps per run land at 0.79 ms per step, 32 at 0.88 ms.

The session numbers are bit-identical to chained `execute()` calls on every
backend here because a session dispatches the same kernels in the same order;
the tolerance the harness allows for GPUs (`compare`) was not needed.
WebGL2's `readPixels` is synchronous, so it has no completion-latency floor
(a two-node session run is 0.2 ms) and gains most from a session at one step
per run. cpu-js sessions are not faster than `execute()`: it never had a
transfer cost to remove, and the WASM arena gains only the validation.

## Graph IR v2 acceptance — 16 September 2026

Windows 11 / Chrome 153.0.8010.47 headless (`--force_high_performance_gpu`) /
NVIDIA GeForce RTX 5090. WebGPU reported a non-fallback `nvidia`/`blackwell`
adapter; WebGL2 reported `ANGLE (NVIDIA, NVIDIA GeForce RTX 5090 (0x00002B85)
Direct3D11 vs_5_0 ps_5_0, D3D11)` and took the R32F path (4 bytes per scalar),
so the RGBA32F fallback is exercised only by the allocation tests, not by this
hardware. Raw evidence: `docs/browser-validation.json`.
Reproduce with `REQUIRE_GPU=1 python scripts/browser_smoke.py` from this directory
(add `--headed` to watch it).

- **246 Node tests** (`npm test`) and **28 native Python tests**
  (`py -3.13 tests/test_python.py`) passed. These include a cpu-js-versus-WASM
  differential over every operation — each binary op across ten broadcasting
  shape pairs including rank-4, scalar and size-1 axes; each unary op at lengths
  1, 7, 64, 65 and 1025; axis and whole-tensor reductions with and without
  `keepdim`; permutations, reshape aliasing, rank-2 and batched rank-3 matmul;
  softmax, log_softmax, cross-entropy and its gradient; and all five optimizer
  ops — plus a seeded 600-graph walk over shape space per family, and
  finite-difference gradient checks: a whole MLP backward graph against central
  differences of its own loss for GELU, tanh and sigmoid networks (the
  differentiable activations; ReLU's kink makes central differences unreliable,
  so its mask is covered by the `positive` differential instead), and the
  cross-entropy and GELU gradient ops against double-precision differences.
- **155 numerical cases passed on each of the four backends** in the browser,
  every one compared element by element against the cpu-js reference.
- The R244 regression runs the **real** checked-in playground engine: a Python
  program that submits 17 graphs with an `on_error` handler that resubmits now
  leaves the `Engine` usable and disposable instead of overflowing the host
  stack while the engine is still borrowed.
- `py -3.13 tools/python_corpus.py --zipp target/release/zipp.exe --only gpu`
  passed for `gpu_graph.py`, `ml_gpu_ops.py` and `ml_gpu_training.py`: the
  native Zipp Python frontend and CPython 3.13 print byte-identical output for
  graph construction, the version rule and the `execute_locally` float32
  reference over the whole v2 operation set.

Per-operation maximum absolute error versus the cpu-js reference, browser run:

| Operation family | WebGPU | WebGL2 | WASM (SIMD) |
|---|---:|---:|---:|
| `add`, `sub`, `mul` (incl. broadcasting) | 0 | 0 | 0 |
| `div` | 6.1e-05 | 6.1e-05 | 0 |
| `neg`, `relu`, `positive` | 0 | 0 | 0 |
| `exp` | 1.22e-04 | 1.22e-04 | 0 |
| `log`, `sqrt` | 2.38e-07 | 2.38e-07 | 0 |
| `tanh`, `sigmoid`, `gelu_grad` | 1.19e-07 | 1.19e-07 | 0 |
| `gelu` | 4.77e-07 | 4.77e-07 | 0 |
| exact zero and tiny arguments of the odd functions | 5.96e-08 | 5.96e-08 | 0 |
| `transpose`, `permute`, `reshape` | 0 | 0 | 0 |
| `sum` over an axis | 0 | 0 | 0 |
| `mean` over an axis | 5.96e-08 | 5.96e-08 | 0 |
| whole-tensor `sum`/`mean` (pairwise) | 1.49e-08 | 1.49e-08 | 0 |
| `softmax`, `log_softmax` | 1.91e-06 | 1.91e-06 | 0 |
| `matmul` (rank 2 and batched rank 3) | 0 | 3.81e-06 | 0 |
| `cross_entropy`, `cross_entropy_grad` | 4.77e-07 | 4.77e-07 | 5.59e-09 |
| optimizer updates (SGD, momentum, Adam) | 0 | 2.38e-07 | 0 |
| `life` | 0 | 0 | 0 |

These are absolute errors on unnormalized fixtures, so read them against the
magnitudes involved. The two largest are both the platform's own arithmetic, not
the graph: `div` reaches 6.1e-05 where the largest quotient in the fixture is
582.6, which is **exactly one float32 ulp** (GPU division is not
correctly-rounded), and `exp` reaches 1.22e-04 where the largest value is 397,
which is **four ulp** of the GPU `exp` intrinsic against `Math.fround(Math.exp(x))`.
The WASM kernels are bit-identical to the JavaScript reference on every exact
operation; their only non-zero row, cross-entropy, is the double-precision `log`
implemented in `../rust/zipp-kernels` differing from V8's by under one ulp. The acceptance
tolerance is `2e-4 + 2e-4·|expected|`.

One MNIST-scale training step — 784-256-10 MLP, batch 64, ReLU, mean
cross-entropy over integer class targets, full backward pass and Adam updating
four parameters with both moments, all in **one graph** (58.1M estimated work
units, 254k uploaded and 611k read-back elements):

| Backend | cold | warm median | warm min | forward+loss only | batch 512, loss only | max abs error vs cpu-js |
|---|---:|---:|---:|---:|---:|---:|
| WebGPU | 18.6 ms | 7.8 ms | 5.8 ms | 5.4 ms | 8.6 ms | 4.35e-06 |
| WebGL2 | 30.4 ms | 5.6 ms | 4.9 ms | 2.6 ms | 5.5 ms | 9.96e-06 |
| WASM (SIMD) | 6.5 ms | 3.9 ms | 3.5 ms | 2.6 ms | over budget | 2.82e-06 |
| cpu-js | 27.9 ms | 27.0 ms | 23.5 ms | 25.3 ms | over budget | 0 (reference) |

All four backends produced the same five-step loss sequence to four decimals
(2.3450, 2.2566, 2.0440, 1.9046, 1.7521), so the updates agree, not just the
first forward pass. "over budget" is the work limit refusing the graph
(`maxWork` 400M on WASM and 100M on cpu-js), not a failure. The error column is
the worst over the loss, all four updated parameters and all eight Adam moments.

These are wall-clock times for `runtime.execute()`, including upload, all
dispatches and readback; they are not GPU timestamp queries and no speedup is
claimed. At this size the step is dominated by transfers and per-node host work,
which is why the WASM kernels — no PCIe round trip — beat both GPUs. What the
GPUs do show is scaling: eight times the batch (64 to 512, forward and loss only)
costs WebGPU 5.4 to 8.6 ms and WebGL2 2.6 to 5.5 ms, while both CPU backends
refuse a graph that large. Choosing a backend by these numbers alone would pick
WASM for a small model; the GPUs matter as the model grows.

Non-finite intermediates were checked on every backend: a graph that overflows to
infinity and then computes `inf - inf` fails readback with `NUMBER` through
`relu`, `sum`, `tanh`, `gelu`, `softmax` and `matmul` on all four, closing the
divergence where `relu` used to fold NaN to zero on some backends and propagate
it on others.

Limits raised for this work, with the hostile-input checks unchanged: elements
per tensor 1,048,576 → 4,194,304; per dimension 4,096 → 65,536; aggregate input
and output elements 1,048,576 → 4,194,304; named outputs 16 → 64; logical byte
budget 32 → 64 MiB. The playground no longer pins `maxWork` to 50M and takes the
backend's own budget instead; the measured cpu-js fallback cost above (27 ms for
58M work units, so roughly 50 ms at the 100M ceiling) stays far inside the page's
5-second frame deadline.

Not established here: resident cross-request tensors, kernel fusion, float16,
convolutions, a tiled WebGL2 matmul, or any claim about non-NVIDIA hardware,
other browsers or a shared multi-tenant device.

## Local acceptance — 13 September 2026

Windows / Chrome 152 / NVIDIA GeForce RTX 5090:

- 60 Node GPU contract tests and 12 native Python graph tests passed.
- Rebuilt default Python WASM: 57 frontend and 27 GPU boundary checks passed,
  including compiled Torch models and functional operations with CPU constants.
- 15 numerical cases each passed on actual WebGL2 and WebGPU hardware.
- Production landing build, 9 landing tests, folder/reload/arguments, browser VFS
  mutations, embedded Python, mobile layout, animation controls, Life and Torch
  inference passed. Torch predictions matched eager inference on WASM/WebGL2/WebGPU.
- 26 native Python VM tests passed; CLI tests and the new project regressions passed.
- Opt-in language interoperability passed 2 native tests and the WASM instance
  checks. The ordinary Python WASM build correctly rejects that optional module.
- All edited Rust files pass rustfmt. Repository-wide `cargo fmt --all -- --check`
  still reports pre-existing formatting differences in unrelated engine files.

These are local results. The updated manual/reusable CI lane has not been run on GitHub.

## Dense GPU training acceptance — 13 September 2026

- Six native Torch tests passed, including five-step PyTorch loss/gradient/weight
  parity and shared-parameter/input-gradient accumulation.
- Actual WASM training passed with JavaScript and standalone WASM kernels;
  backend failure, duplicate submission, stale weight/gradient/metadata changes
  and unsupported optimizer options are checked before committing results.
- Hardware WebGL2 (RTX 5090 / ANGLE D3D11) and WebGPU (NVIDIA Blackwell)
  each passed 17 kernel cases and five training steps against CPU PyTorch 2.11.
  Maximum absolute error across loss, gradients and weights was 7.45e-9 on
  WebGL2 and zero on WebGPU for this fixture. Reproduce with
  `python crates/zipp-wasm/tests/browser-training.py http://127.0.0.1:8766`
  while serving the repository root at that URL.
- 60 Node runtime tests, 13 native graph tests, 57 WASM frontend checks,
  29 WASM GPU bridge checks and the WASM CPU Conv2d fixture passed.
- Production playground acceptance passed on WASM, WebGL2 and WebGPU, including
  100 training steps with loss 0.05558 → 0.00383, alongside Life, inference,
  embedded Python, folder upload/VFS, media controls and responsive layout.

Training records/uploads every call and reads back loss, gradients and updated
weights. These measurements establish correctness, not speedup, resident model
state, GPU Conv2d or multi-GPU training. The archive above is unchanged.

### Training review fixes — 13 September 2026

- Source leaves keep intrinsic `requires_grad` when first encountered inside
  `no_grad`; only operations in that context are detached. The fixture matches
  native PyTorch: loss 202, parameter gradient 1 and updated weight 1.9.
- SGD capture snapshots optimizer identity, parameter identities, group order and
  membership, learning rate, weight decay, maximize, momentum, dampening and
  Nesterov. Stale options/groups reject completion before any tensor write;
  cleanup uses the captured parameters, preserving newly added parameters' grads.
- Eight native Torch tests passed. Actual WASM passed 26 optimizer mutation
  cases per JS/WASM backend, covering changes before submit and while pending,
  plus no_grad and the existing five-step reference/transaction tests.
- Hardware WebGL2 and WebGPU passed the new no_grad, learning-rate and parameter
  append checks, as well as five-step reference parity and 17 kernel cases each.
- The checked-in playground engine was rebuilt and the production landing build
  passed. Python caches are ignored; the older tracked Test262 `.pyc` was removed
  from version control. Historical validation reports remain unchanged.
