# ZIPP Python GPU Lab 0.1.0

The integrated playground now includes a **native NCA lab** with CUDA device
selection, concurrent independent experiments on multiple GPUs, and hardware
WebGL2 visualization of actual model state. Start `node ../playground/serve.cjs`
from this directory, then use the playground's **NCA lab** link. See
[the playground instructions](../playground/README.md#native-nca-lab-your-gpus-and-live-model-state).
The graph backends request high-performance hardware and expose adapter identity.
The original standalone README below retains its historical validation notes;
the integrated NCA smoke test has since verified WebGL2 and WebGPU on RTX 5090 hardware.

> **In this repository** (`crates/zipp-wasm/gpu-lab/`): this is the GPU Lab
> package vendored and wired into Zipp. What changed relative to the standalone
> kit:
>
> - `python/zipp_gpu.py` moved to `crates/zipp-vm/src/frontend/python/lib/zipp_gpu.py`
>   and is bundled with the Python frontend: `from zipp_gpu import Graph` works
>   in `zipp py`, in the wasm engine and in the playground. The `async run`
>   transport became `Graph.submit(callback, on_error=None, **outputs)`; the
>   file also carries the float32 reference evaluator (`execute_locally`), which
>   `submit` uses when no host is attached (the CLI, CPython). The examples and
>   `tests/test_python.py` here import that copy.
> - `src/zipp-python-adapter.mjs` is the adapter for a Python state: requests
>   leave the engine through `Engine.takeHostRequests()` and answers return
>   through `pythonCall("__zipp_py_deliver", ...)`. The original
>   `src/zipp-adapter.mjs` (JavaScript `host.call` queue) and its tests are kept
>   unchanged.
> - The playground worker (`../playground/engine.worker.js`) is the host that
>   uses it; `../tests/node/python-gpu.cjs` covers the channel and the adapter
>   over the JavaScript and compiled-WASM backends; `../playground/smoke.cjs`
>   runs the graphs in a real browser and records the backend.
> - Verified in this integration: the four bundled graphs through the real
>   Python frontend natively, in Node (cpu-js and wasm backends) and in a
>   browser; WebGPU/WebGL2 execution depends on the machine and is recorded, not
>   assumed, by the smoke test. `node --test tests/*.test.mjs` (57 checks) and
>   `python -m unittest discover -s tests -p test_python.py` still pass here.
> - The standalone demo (`demo/`, `scripts/serve.py`) is kept as an isolated
>   diagnostic of a browser's GPU support: `node ../playground/serve.cjs` then
>   open `http://127.0.0.1:8765/crates/zipp-wasm/gpu-lab/demo/`.
>
> The original README follows.

**A runnable, standalone integration experiment for Python-authored browser compute.**

Python records a small float32 computation graph. A host executes the graph through
WebGPU, WebGL2, compiled WebAssembly, or a JavaScript reference implementation.
Only named outputs are read back. The package includes source, a compiled 3.5 KiB
WASM kernel module, a browser demo, a native Python development harness, and tests.

## Important scope

This is an **add-on package**, not a modified checkout of the private
`f2i-com/zipp-python` repository. That repository's source and authenticated access
were unavailable during this implementation. No private files were inspected,
changed, invented, or bundled. The public ZIPP host-queue contract was inspected.

This package does **not** implement a Python interpreter. The native harness uses
CPython installed on your computer. Browser examples use graphs exported by that
Python library; the browser does not run the displayed Python source. Loading
`zipp_gpu.py` into your Python WASM runtime and binding its transport is the next
integration step, subject to that runtime's actual language/module support.

**Verified here:** all 57 Node tests, all 12 Python tests, and 15 numerical checks
on each of the WASM and JavaScript backends in Chromium. The native Python-to-Node-
to-WASM demo also ran successfully. **Not verified here:** execution of WebGPU or
WebGL2 shaders, your private Python runtime, or real-origin browser Worker loading.
The environment could not create a GPU context. GPU implementations are real
source, but remain experimental until the included browser checks pass on a usable
adapter. A missing backend is never counted as a test pass.

## Try the browser demo

No npm installation or remote CDN is needed. The precompiled WASM binary is included.
Python 3 is needed only to serve the local files.

On Windows, double-click `START-DEMO.bat`, or run:

```powershell
py -3 scripts/serve.py
```

On macOS/Linux:

```sh
python3 scripts/serve.py
```

Open **http://localhost:8765/demo/**. Do not open `index.html` directly with `file://`.
A static HTTPS server can host the same folder. The launcher binds to loopback only;
it accepts no uploads, runs no submitted code, and does not install anything.

Choose a backend and press **Run graph**. Auto-selection tries:

```text
WebGPU -> WebGL2 with floating-point render targets -> WASM -> host JavaScript
```

The actual backend and any initialization fallback reasons are shown. Selecting
WebGPU or WebGL2 explicitly never silently changes to a CPU implementation. Automatic
fallback applies to initialization only, not to a failed calculation.

Press **Check all backends** for 15 numerical checks per available backend. Save the
report to capture browser identity, actual backend, individual checks, and failures.
Unavailability is distinct from a numerical or shader failure. The host applies a
30-second demo operation deadline, or 60 seconds for a backend check; expiry stops
the Worker, not necessarily work already submitted to a GPU.

## Run real Python now

The native development harness needs Python 3 and Node.js 18+. It runs without pip
or npm dependencies:

```powershell
py -3 examples/run_native.py
```

Expected output:

```text
Backend: wasm
Result:  [14, 44, 94, 164]
Total:   316
```

This is an actual Python library constructing and submitting a graph through a
transport to the included compiled WASM kernels. Python itself runs natively in
this harness. This is not evidence that the private Python WASM frontend works.

The underlying Python API is:

```python
from zipp_gpu import Graph

gpu = Graph()
a = gpu.tensor([1, 2, 3, 4])
b = gpu.tensor([10, 20, 30, 40])
c = (a * b + 4).relu()

# Every operation above records a node. One data-only request contains the graph.
program = gpu.program(result=c, total=c.sum())

# For an embedded runtime, supply an async transport that executes that graph.
# result = await gpu.run(transport, result=c, total=c.sum())
```

Place `python/` on your module path. The included examples do that themselves.
For a custom graph, edit `examples/vector.py`, then:

```powershell
py -3 examples/vector.py > my-program.json
```

Use **Open a graph JSON** in the demo, then **Run graph**. With older PowerShell
versions whose redirection changes encoding, write UTF-8 directly in Python using
`Path('my-program.json').write_text(gpu.to_json(result=c), encoding='utf-8')`.

Regenerate all four bundled graph examples with:

```powershell
py -3 examples/build_examples.py
```

## Implemented operations

| Operation | Python spelling | Semantics |
|---|---|---|
| Input | `gpu.tensor(data, shape=...)` | Owned float32 data; scalar, vector, or matrix |
| Fill | `gpu.full(shape, value)`, `gpu.zeros(shape)` | Allocate and fill on the chosen backend |
| Elementwise maths | `a + b`, `a - b`, `a * b` | Same shapes, or a rank-zero scalar |
| ReLU | `a.relu()` | Elementwise maximum with zero |
| Matrix multiplication | `a @ b` | `[M,K] @ [K,N]`, with a correctness-first kernel |
| Reduction | `a.sum()` | All elements, pairwise float32 reduction |
| Cellular update | `state.life()` | One wrapped-edge Conway-style update |

The examples include vector arithmetic, rectangular matrix multiplication, a tiny
explicit-weight neural-network inference graph, and a glider advanced 24 steps on
a 32 x 32 grid. The network is not trained here. `life()` is a fixed cellular rule,
not a neural cellular automaton or online learning implementation.

All values are float32. This is deliberately **not** transparent Python numeric
semantics or NumPy compatibility. Inputs must be finite and float32-representable.
Non-finite output readbacks reject instead of silently becoming JSON `null`.
Backends may differ in floating-point rounding and reduction details; compare with
tolerances, not universal bitwise equality.

## What is inside

```text
python/zipp_gpu.py            Symbolic Python tensor API and graph export
src/graph.mjs                 Data-only protocol, shape/work/size validation
src/runtime.mjs               Backend selection, liveness, cleanup, readback
src/backends/webgpu.mjs       WGSL compute kernels and asynchronous buffer readback
src/backends/webgl2.mjs       GLSL fragment kernels, float textures, readback
src/backends/wasm.mjs         Standalone compiled-kernel bridge and memory arena
src/backends/cpu.mjs          JavaScript float32 reference
src/zipp-adapter.mjs          Tenant-scoped asynchronous ZIPP request adapter
src/zipp-guest.js             Guest callback and Promise wrappers
wasm/kernels.c                Complete freestanding kernel source
wasm/kernels.wasm             Included binary compiled from kernels.c
demo/                        Native-module UI and dedicated compute Worker
examples/                    Python authoring and native transport examples
generated/                   Graph JSON produced by the included Python examples
tests/                       Numerical, validator, lifecycle and adapter tests
docs/                        Design, integration plan, limitations and raw reports
```

See **docs/INTEGRATION.md** before wiring this into ZIPP. See **docs/ARCHITECTURE.md**
for the memory model and next compiler steps. See **docs/VALIDATION.md** for exact
evidence boundaries and commands. **docs/AI-IMPLEMENTATION-BRIEF.md** is a handoff
for a developer who has access to your actual private repository.

## Development checks

```sh
node --test tests/*.test.mjs
python -m unittest discover -s tests -p 'test_python.py'
python examples/run_native.py
```

Optional browser-local runtime checks need Playwright, which is not bundled:

```sh
python -m pip install playwright
python -m playwright install chromium
python scripts/browser_smoke.py
```

`CHROMIUM_BIN` can select an installed browser. This smoke script evaluates local
source without a network origin. Consequently WebGPU may be unavailable even on a
machine where it works on HTTPS/localhost. **Use the actual localhost demo for
real-origin GPU and Worker acceptance.** The local script does not bypass browser
network policies and does not claim unavailable checks passed.

The WASM binary is already built. To rebuild it with a Clang toolchain that includes
`wasm-ld`:

```sh
sh scripts/build_wasm.sh
```

## Performance boundaries

This version is a correctness and integration prototype, not a tuned tensor engine.
The GPU backends cache generated pipelines/programs and keep intermediate tensor
data on the GPU, but currently submit each operation separately. Matrix multiply
is not tiled. A general Python kernel compiler, kernel fusion, graph scheduling
optimizations, autograd, persistent cross-request tensors, and binary bulk transport
are not implemented. Warm/cold timing and transfer costs are described in the docs.

Pinned WASM host memory is **not** automatically GPU-visible memory. This version
copies input arrays into GPU buffers/textures. It deliberately does not reinterpret
ZIPP's guest-address strings as browser GPU addresses.

## License

Apache-2.0. All experiment source and the compiled kernel binary are included.
No Python runtime, ZIPP engine build, third-party package, or private source tree
is redistributed in this package.
