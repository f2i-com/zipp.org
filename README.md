<p align="center">
  <img src="docs/assets/zipp-hero.svg" alt="Zipp — two languages, one VM, your GPU" width="100%">
</p>

<h1 align="center">Zipp: two languages. One VM. Your GPU.</h1>

<p align="center">
  <strong>Write Python or JavaScript. Run natively or in WebAssembly. Give supported compute to the GPU.</strong>
</p>

<p align="center">
  <a href="#start-the-local-gpu-lab"><strong>Run the GPU lab</strong></a> ·
  <a href="#gpu-computing-from-javascript-and-python"><strong>Write GPU code</strong></a> ·
  <a href="DOC.md"><strong>Read the docs</strong></a>
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#what-runs-where">What runs where</a> ·
  <a href="#performance-measured-honestly">Performance</a> ·
  <a href="#correctness-and-language-coverage">Language support</a> ·
  <a href="#choose-the-right-execution-profile">Security</a> ·
  <a href="#reproduce-and-contribute">Contribute</a>
</p>

**Zipp brings Python and JavaScript into the same Rust register-bytecode VM.**
Build an embedded scripting runtime, run a folder of code in a browser Worker,
or turn a familiar Torch model into a browser GPU compute graph.

The experimental Python frontend compiles directly to Zipp bytecode. It does not
ship CPython or translate your Python program into browser JavaScript. Both
frontends use the same engine, with native and WebAssembly builds.

- **Bring a project, not just a snippet.** The playground loads folders, modules
  and data, with an editor, virtual files, console and graphics in one place.
- **Start with familiar ML code.** The bundled Torch subset supports eager CPU
  tensors, autograd and training. Experimental `torch.compile(model)` records
  supported inference and dense-model SGD training for WebGPU, WebGL2 or an explicit CPU fallback.
- **Keep the host in control.** Execution budgets and explicit host capabilities
  let embedders decide which resources a program can use.
- **Explore one engine across languages.** An optional trusted-code build adds
  Python-to-JavaScript evaluation inside the very same VM instance.

Python support and Torch compatibility are experimental. The
[Torch compatibility guide](docs/TORCH_COMPATIBILITY.md) and
[Python frontend guide](docs/PYTHON_FRONTEND_EXPERIMENT.md) explain the supported
surface and remaining differences.

## What runs where

| What you want to do | Where your code runs | Where the numerical or drawing work runs |
|---|---|---|
| Run JavaScript scripts or embed Zipp | Native Rust VM/JIT, or Zipp's WebAssembly build | CPU; host APIs are supplied by the embedding application |
| Run Python projects in Zipp | Experimental Python frontend on the same VM | CPU, including the bundled `torch` subset |
| Compile a supported Torch model or submit a Python `zipp_gpu` graph from the playground | Python in the WASM Worker; JavaScript handles the graph | WebGPU compute shaders or WebGL2 fragment shaders; visible CPU fallback in `auto` mode |
| Use GPU graphs from browser JavaScript | An ordinary browser ES module or Worker | The same GPU runtime, without requiring Python or the Zipp VM |
| Draw a custom browser visualization | Browser JavaScript with a canvas | WebGL/WebGL2 through the browser-selected adapter |

**GPU support does not automatically move all Python or JavaScript onto a GPU.**
The graph API executes its supported operations on the selected backend.
The playground's `ui` drawing API uses a 2D canvas; the Life computation runs
on the selected graph backend.

## Familiar Torch code, browser GPU execution

```python
import torch
from torch import nn

model = nn.Sequential(nn.Linear(2, 4), nn.ReLU(), nn.Linear(4, 1))
x = torch.tensor([[1.0, 2.0], [3.0, 4.0]])

inference = torch.compile(model)(x)  # no zipp_gpu import or backend name
inference.submit(lambda y: print(y.tolist()))
```

Your model, tensor creation and forward pass use the supported Torch API.
Zipp records the inference graph and the host executes it on the selected
backend. **`submit(callback)` is a Zipp extension:** browser GPU completion is
asynchronous; this is not a drop-in implementation of PyTorch's `torch.compile`.
It supports float32 inference and an opt-in dense-model GPU training path; it does not run CUDA scripts.
The callback receives a regular CPU Torch-compatible tensor. Eager CPU
`nn.Conv2d` also supports forward/backward passes and optimizer updates;
GPU convolution remains future work.

Try **Samples → Python: Torch ML inference (GPU)** in the playground. The
[complete example](examples/python/torch_gpu/main.py) draws its predictions and
checks them against eager inference with the same weights. Explicit GPU selection
fails visibly if unavailable; automatic selection reports the backend it used.

[![Torch model predictions computed on WebGL2 from Python in Zipp WASM](landing/public/demos/torch-inference.png)](examples/python/torch_gpu/main.py)

## Train a small model on the browser GPU

The ordinary training step stays familiar:

```python
import torch.nn.functional as F

optimizer = torch.optim.SGD(model.parameters(), lr=0.01)
target = torch.tensor([[1.0], [2.0]])

def train_step(x, target):
    optimizer.zero_grad()
    loss = F.mse_loss(model(x), target)
    loss.backward()
    optimizer.step()
    return loss

compiled_step = torch.compile(train_step, training=True)
compiled_step(x, target).submit(lambda loss: print(loss.item()))
```

**`training=True` and `.submit(...)` are experimental Zipp extensions**, not
PyTorch's synchronous `torch.compile` API. Forward computation, first-order
gradients and SGD updates execute on the selected backend. The success callback
receives the loss after the CPU model's weights and gradients have been updated.
Wait for that callback before recording the next step.

Try **Samples → Python: Torch ML training (GPU)** to watch a small network learn
`y = x²`, with predictions and a live loss curve. Its
[model and training step](examples/python/torch_training/model.py) also run in
native PyTorch; the [playground driver](examples/python/torch_training/main.py)
provides asynchronous scheduling and graphics.

[![Torch training on WebGL2: Python source, learned curve and falling loss](landing/public/demos/torch-training.png)](examples/python/torch_training/model.py)

This first path supports float32 dense layers, ReLU, MSE and SGD without momentum.
**Each call captures a new graph, uploads inputs and weights, and reads back the
loss, gradients and updated weights.** There is no `compiled.prepare()` API,
resident model/optimizer state, graph cache or multi-GPU training yet. Small
examples demonstrate correctness, not a GPU speedup. See the
[Torch compatibility guide](docs/TORCH_COMPATIBILITY.md) for supported operations,
limits and failure behavior. NCA experiments remain in their separate repository.

## Python and JavaScript inside one VM

Build with `python-js-interop` for trusted mixed-language projects:

```python
import javascript
print(javascript.eval("[1, 2, 3].map(x => x * 2)"))  # [2, 4, 6]
```

This uses Zipp's own JavaScript evaluator in the **same VM instance**, with copied
lists, dictionaries and scalar results. `from js import eval` is also available.
It is opt-in because JavaScript shares VM globals with the Python runtime.
There are no live cross-language object proxies or JavaScript `.py` imports yet.
See the [build instructions and contract](docs/LANGUAGE_INTEROP.md).

## See Zipp in action

**Python running on Zipp WASM, with Game of Life on the GPU.** This is a recording
of the actual folder playground: the Python source is compiled and executed by
Zipp's WebAssembly VM. The displayed `life.py` uses ordinary Torch matrix
operations and ReLU, with no `zipp_gpu` imports. These same rules run in regular
PyTorch. A separate playground driver compiles each update for WebGL2 and
receives its result asynchronously for drawing.

[![Python runs on Zipp WASM: source, compiler, VM and GPU host](landing/public/demos/python-wasm-flow.svg)](landing/public/demos/python-wasm-flow.svg)

[![Actual Python Game of Life running in the Zipp WASM playground on WebGL2](landing/public/demos/python-life.gif)](landing/public/demos/python-life.gif)

**[Open the full project playground](https://zipp.org/playground/)** ·
[Static screenshot](landing/public/demos/python-playground.png) ·
[Portable Torch Life rules](examples/python/gpu/life.py) ·
[Playground driver](examples/python/gpu/main.py)

<details>
<summary>Inspect the Python editor, canvas, WASM status and GPU output in a still screenshot</summary>

![Python source executing in the actual Zipp WASM playground](landing/public/demos/python-playground.png)

</details>

The recording shows a glider gun, pulsar, growing patterns and interacting
debris in one toroidal Life grid. GIFs autoplay and loop; the landing page offers
a pause button and honors reduced-motion settings. [Capture provenance](landing/public/demos/provenance.json)
records the engine, adapter, source hashes and duration. Regenerate with
`python crates/zipp-wasm/playground/capture-demos.py` (Chrome, Python Playwright,
ffmpeg and the Python WASM build required).

## Why Zipp

| Fast to start | Modern JavaScript | Ready to embed |
|---|---|---|
| **7.4 ms** median process launch in the canonical capture. No snapshot to load. | **99.997%** of core Test262 executions: **95,939 / 95,942**. | A native CLI, a Rust embedding API, and a browser WebAssembly runtime. |

- **Explore the whole engine.** The lexer, parser, register VM, GC, inline caches
  and JITs live together in this repository.
- **Choose how to run it.** Use the native JIT for trusted programs, a browser
  Worker for WebAssembly, or the separately built hardened native runner.
- **Use Python and JavaScript.** Explore the experimental Python frontend or
  call the browser's GPU graph runtime directly from JavaScript.
- **Watch code at work.** Explore Game of Life, Langton's ant, bouncing balls,
  and GPU graph examples with visible source, canvas and console output.
- **See the evidence.** Benchmarks include exact-output checks, raw results,
  confidence intervals and the workloads that still need work.

The [performance results](#performance-measured-honestly) and
[language coverage](#correctness-and-language-coverage) explain the measurements
and their scope.

## Quick start

### Start the local GPU lab

The landing page embeds the complete project playground and exposes it at
**[`/playground`](https://zipp.org/playground/)**. It supports folders, loose files,
Python/JavaScript samples, entry files, arguments, editing, console/canvas output
and GPU selection. Files stay in the browser's virtual filesystem.

For a local source checkout:

```sh
cd crates/zipp-wasm
./build-variants.sh all
node playground/serve.cjs
```

Open the printed playground URL, choose **Samples → Python: Game of Life**, select
WebGL2 or WebGPU, and press Run. The console identifies the actual backend.
`auto` visibly falls back to CPU/WASM when hardware is unavailable; explicit GPU
selection reports an error instead. Browser settings decide which adapter is used.

The native NCA research project now lives independently at
[neuralautomata.com](https://github.com/f2i-com/neuralautomata.com).
Zipp does not include its PyTorch runner, models, checkpoints or native endpoints.

### Run the JavaScript engine

The published [`v0.0.17` release](https://github.com/f2i-com/zipp.org/releases/tag/v0.0.17)
provides x86-64 binaries and a browser WebAssembly package for the JavaScript
engine. Those release downloads predate this checkout's Python and browser GPU
lab additions; build this repository for its current engine features.

Save this as `app.js`, then choose your platform below:

```js
const greet = name => `Hello, ${name}!`;
console.log(greet("Zipp"));
```

Building an application? Start with the [Rust embedding guide](DOC.md#embedding),
the [browser example](#embed-zipp-webassembly-in-a-web-app), or the
[execution profiles](#choose-the-right-execution-profile).

### Windows

<details>
<summary><strong>Download and run with PowerShell</strong></summary>

Download, extract, and run the native Windows executable from PowerShell:

```powershell
$version = '0.0.17'
$archive = "zipp-$version-x86_64-pc-windows-msvc.zip"
Invoke-WebRequest "https://github.com/f2i-com/zipp.org/releases/download/v$version/$archive" -OutFile $archive
Expand-Archive -LiteralPath $archive -DestinationPath .

& ".\zipp-$version-x86_64-pc-windows-msvc\zipp.exe" js .\app.js
```

Use `mjs` instead of `js` for an ES module entry, including top-level `await`.

</details>

### Linux

<details>
<summary><strong>Download and run from your shell</strong></summary>

Download, extract, and run the native Linux binary:

```sh
version=0.0.17
archive="zipp-$version-x86_64-unknown-linux-gnu.tar.gz"
curl -fLO "https://github.com/f2i-com/zipp.org/releases/download/v$version/$archive"
tar -xzf "$archive"

"./zipp-$version-x86_64-unknown-linux-gnu/zipp" js ./app.js
```

The archive preserves the executable bit. If another tool removes it, restore it
with `chmod +x zipp-0.0.17-x86_64-unknown-linux-gnu/zipp`.

</details>

### Build from source

<details>
<summary><strong>Clone the repository and build with Cargo</strong></summary>

Install stable Rust and its platform toolchain (MSVC Build Tools on Windows, or
a C compiler and linker on Linux). On Windows, run this in PowerShell:

```powershell
git clone https://github.com/f2i-com/zipp.org.git zipp
Set-Location zipp
cargo build --locked --release

.\target\release\zipp.exe js .\app.js
```

On Linux:

```sh
git clone https://github.com/f2i-com/zipp.org.git zipp
cd zipp
cargo build --locked --release

./target/release/zipp js app.js
./target/release/zipp mjs app.mjs   # ES module entry, including top-level await
```

A release build uses fat LTO and one codegen unit, so the final link is
deliberately slower than a development build. The resulting executable has no
runtime data-file dependency.

</details>

### Run Python (experimental)

The CLI also runs Python: Zipp's own Python 3 implementation, with the
source lowered straight to the engine's register bytecode (no transpilation
to JavaScript and no second interpreter), so a `.py` file runs on the same VM.
Save this as `fib.py`:

```python
def fib(n):
    a = 0
    b = 1
    for i in range(n):
        a, b = b, a + b
    return a

print(fib(30))
```

```sh
zipp py fib.py                 # 832040
zipp run fib.py                # frontend chosen by extension, shebang or directive
zipp run --lang=python -       # from standard input
```

Classes (including metaclasses, descriptors and `__slots__`), exceptions
with full tracebacks, generators, closures, comprehensions, `match`
statements, f-strings, the builtin types and a set of standard-library
modules (`math`, `json`, `re`, `collections`, `itertools`, `functools`,
`dataclasses`, `enum`, `contextlib`, `typing`, `struct`, `hashlib`, ...)
all work; `async` does not yet. Semantics are checked
differentially against CPython: `tests/python_corpus/*.py` must print
exactly what CPython prints.

A folder runs as a project: `zipp py examples/python/project` runs its
`main.py`, and `zipp py lab/train.py --steps 20` runs one script of a
folder with arguments. Every file of the folder (subfolders included, up to
8 MiB each and 64 MiB in total) is loaded into the program's virtual
filesystem, so `open()`, `os`, `os.path`, `pathlib` and `json.load` see the
project's data; `.py` files are modules and packages by folder
(`legacy/fast_memory.py` is `legacy.fast_memory`, with or without an
`__init__.py`); `sys.argv` carries the arguments; and files the program
writes are copied back under the folder when it finishes. A `test_*.py`
entry runs its tests through the bundled `pytest` subset. The bundled
library also includes a `torch` subset (tensors over typed arrays with
reverse-mode autograd, `nn`, `nn.functional`, `optim`, `save`/`load` in
supported PyTorch checkpoint layouts) that runs on the engine's CPU kernels, so
supported ML code can train and evaluate inside Zipp. Eager execution is CPU;
`torch.compile` adds supported asynchronous GPU inference and dense-model SGD training. The scope
matrix, limits and the bytecode design are in
[docs/PYTHON_FRONTEND_EXPERIMENT.md](docs/PYTHON_FRONTEND_EXPERIMENT.md). The
feature is on by default in the CLI (`--no-default-features` builds the
JavaScript-only binary) and off by default in the `zipp-vm` library and the
WebAssembly package, which offers it as a
[separate build variant](crates/zipp-wasm/README.md#build-variants-javascript-only-or-javascript-and-python).

Python programs can also compute on the GPU in the browser: the bundled
`zipp_gpu` library records a float32 graph (`+`, `*`, `@`, `relu`, `sum`,
a Conway-life step) and `submit`s it, and the host runs it through WebGPU,
WebGL2, compiled WebAssembly kernels or a JavaScript reference, calling the
program back with the outputs; natively the same code evaluates on the CPU.
See [crates/zipp-wasm/README.md](crates/zipp-wasm/README.md#gpu-compute-for-python-programs).

There is also a local [playground](crates/zipp-wasm/playground/README.md)
that runs a folder of Python or JavaScript files on the WebAssembly engine:
open or drop a whole project folder (subfolders, data files and binary
checkpoints included), browse it in a file tree, pick any script as the
entry, give it arguments, and run it in the browser; files the program
writes show up in the tree. It has an editor, a console and a canvas the
program draws on through a small `ui` API (`draw`/`update`/`on_click`/
`on_key` hooks for animation and input), and a GPU sample that steps life
on the compute backend every frame:

```sh
cd crates/zipp-wasm && ./build-variants.sh all && node playground/serve.cjs
```

## GPU computing from JavaScript and Python

**Yes: JavaScript can use WebGL directly.** [WebGL is a browser JavaScript API](https://developer.mozilla.org/en-US/docs/Web/API/WebGL_API),
and this project's [WebGL2 graph backend](crates/zipp-wasm/gpu-lab/src/backends/webgl2.mjs),
[WebGPU graph backend](crates/zipp-wasm/gpu-lab/src/backends/webgpu.mjs), and
[WebGL2 backend](crates/zipp-wasm/gpu-lab/src/backends/webgl2.mjs) are written in
JavaScript. Python is one way to author work for that runtime.

### Browser JavaScript: run a GPU graph

Save the following as `gpu-example.html` in the repository root, start the local
server above, and open `http://127.0.0.1:8765/gpu-example.html` (using the printed
port). It runs directly in the browser and requires no Python or Zipp build.

```html
<!doctype html>
<meta charset="utf-8">
<title>JavaScript GPU graph</title>
<pre id="output">Running a WebGL2 graph…</pre>
<script type="module">
import { createRuntime } from "/crates/zipp-wasm/gpu-lab/src/runtime.mjs";

const output = document.getElementById("output");
let runtime;
try {
  runtime = await createRuntime({ backend: "webgl2" });
  const result = await runtime.execute({
    version: 1,
    nodes: [
      { id: 0, op: "input", shape: [3], data: [-2, 3, 4] },
      { id: 1, op: "input", shape: [3], data: [10, 20, 30] },
      { id: 2, op: "mul", a: 0, b: 1 },
      { id: 3, op: "relu", a: 2 }
    ],
    outputs: [{ name: "values", id: 3 }]
  });
  output.textContent = JSON.stringify({
    backend: result.backend,
    adapter: runtime.info().adapter,
    values: result.outputs.values.data // [0, 60, 120]
  }, null, 2);
} catch (error) {
  output.textContent = error.message;
} finally {
  runtime?.dispose();
}
</script>
```

Use `backend: "webgpu"` for WGSL compute shaders, or `"auto"` to try WebGPU,
WebGL2, compiled WASM and JavaScript in that order. Explicit GPU selections
fail if unavailable; `auto` reports any initialization fallback through
`runtime.info().fallbackAttempts`. Await each execution before submitting
another to the same runtime. Only named outputs are read back to JavaScript.

For your own graphics, browser JavaScript can create a separate canvas and call
`canvas.getContext("webgl2", { powerPreference: "high-performance" })` to work
with WebGL directly. The graph API supplies a bounded set of float32 operations;
it does not turn arbitrary JavaScript into shaders. The WebGL2 backend shows
how float tensors are uploaded and processed with GLSL.

### Python: author the same graph in the playground

Paste this into a Python entry file in the WASM playground, select WebGL2 or
WebGPU in its toolbar, and press **Run**:

```python
from zipp_gpu import Graph

def show(result):
    print(result["backend"], result["outputs"]["values"]["data"])

g = Graph()
a = g.tensor([-2, 3, 4])
b = g.tensor([10, 20, 30])
g.submit(show, values=(a * b).relu())  # callback receives [0, 60, 120]
```

Python records the graph and submits it through a host request. The Worker's
JavaScript runtime validates it, allocates GPU buffers/textures, runs the shaders,
reads the requested outputs, and delivers the callback between VM calls.
Supported operations include elementwise arithmetic, ReLU, matrix multiplication,
sum and a wrapped Conway-Life update. These are float32 operations, with the
shape/work limits in the [GPU Lab documentation](crates/zipp-wasm/gpu-lab/README.md).
Running this Python code through the native Zipp CLI instead uses its local CPU
reference evaluator; it does not start PyTorch or CUDA.

### Browser JavaScript versus JavaScript inside Zipp

The HTML example runs in the **browser's JavaScript engine**. JavaScript files
loaded into the **Zipp playground editor** run inside the Zipp VM and do not
automatically receive browser `document`, canvas or WebGL objects.

The current playground connects `zipp_gpu` requests for **Python guest programs**.
Its JavaScript guest GPU bridge is **not yet wired into that playground**. For a
custom Zipp embedder, the repository supplies
[`createZippGPUAdapter`](crates/zipp-wasm/gpu-lab/src/zipp-adapter.mjs) and
[`gpuExecute` / `gpuExecuteAsync`](crates/zipp-wasm/gpu-lab/src/zipp-guest.js):
the host must install the guest shim, grant `gpu.execute`, drain and dispatch
the host-call queue, and deliver callbacks. The adapter tests cover that contract;
they are not a claim that the stock JavaScript playground exposes it already.

## Embed Zipp WebAssembly in a web app

Run the browser build in a dedicated Worker, with a deadline controlled by
your page. The complete example includes setup, cleanup and resource limits.

<details>
<summary><strong>Complete browser setup and Worker example</strong></summary>

Download the browser bundle, then serve its JavaScript and WebAssembly files
from the same origin as your app:

```sh
version=0.0.17
archive="zipp-wasm-$version-web.zip"
curl -fLO "https://github.com/f2i-com/zipp.org/releases/download/v$version/$archive"
unzip "$archive"

mkdir -p public/zipp-wasm
cp "zipp-wasm-$version-web/zipp_wasm.js" \
   "zipp-wasm-$version-web/zipp_wasm_bg.wasm" \
   public/zipp-wasm/
```

For arbitrary code, do not run the synchronous engine on the page's main
thread. Add this dedicated module Worker as `public/zipp-wasm/worker.js`:

```js
import init, { Engine } from "./zipp_wasm.js";

await init({
  module_or_path: new URL("./zipp_wasm_bg.wasm", import.meta.url),
});

self.onmessage = ({ data }) => {
  let engine;
  try {
    engine = new Engine();
    engine.initScript(data.source);
    self.postMessage({ type: "result", output: engine.takeOutput() });
  } catch (error) {
    self.postMessage({ type: "error", error: String(error) });
  } finally {
    engine?.dispose();
  }
};

self.postMessage({ type: "ready" });
```

Start one fresh Worker per run from responsive page code. The page owns both
the load deadline and the execution deadline, so it can forcibly terminate a
Worker even while guest JavaScript is blocking it:

```js
export function runZipp(source, timeoutMs = 2_500) {
  return new Promise((resolve, reject) => {
    const worker = new Worker("/zipp-wasm/worker.js", { type: "module" });
    let settled = false;
    let timer = setTimeout(
      () => finish(reject, new Error("Zipp WebAssembly failed to load")),
      15_000,
    );

    function finish(callback, value) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      worker.terminate();
      callback(value);
    }

    worker.onmessage = ({ data }) => {
      if (data.type === "ready") {
        clearTimeout(timer);
        timer = setTimeout(
          () => finish(reject, new Error("JavaScript execution timed out")),
          timeoutMs,
        );
        worker.postMessage({ source });
      } else if (data.type === "result") {
        finish(resolve, data.output);
      } else if (data.type === "error") {
        finish(reject, new Error(data.error));
      }
    };

    worker.onerror = (event) =>
      finish(reject, event.error ?? new Error(event.message));
  });
}

const lines = await runZipp('console.log("Hello from Zipp");');
document.querySelector("#output").textContent = lines.join("\n");
```

Serve the app over HTTP(S), not `file://`, configure `.wasm` as
`application/wasm`, and adjust `/zipp-wasm/worker.js` if the app is hosted below
a URL prefix. A Content Security Policy must allow `'wasm-unsafe-eval'` in
`script-src` and the Worker URL in `worker-src`. `Engine` also enforces
instruction, heap, output, source, and WebAssembly-memory ceilings; Worker
termination supplies the separate wall-clock boundary. The browser build is
interpreter-only and grants no host capabilities by default. See the
[`zipp-wasm` guide](crates/zipp-wasm/README.md) before exposing bridges or
accepting multi-tenant input.

</details>

## Choose the right execution profile

| Input | Use | Boundary |
|---|---|---|
| Trusted programs and benchmarks | `zipp js` / `zipp mjs` | Maximum-throughput native CLI with JITs enabled. |
| Arbitrary browser-hosted code | [`zipp-wasm`](crates/zipp-wasm/README.md) in a dedicated Worker | Interpreter-only `safe-sandbox` build; terminate and replace the Worker at the wall deadline and between tenants. |
| Hardened native execution | [`zipp-sandbox`](crates/zipp-sandbox/README.md) | Separately resolved, no-JIT, unsafe-forbidden engine with instruction, heap, output, import, and wall-time limits. |

Build the hardened native runner separately so Cargo cannot unify its safety
features with the ordinary JIT workspace:

```sh
cargo build --locked --release --manifest-path crates/zipp-sandbox/Cargo.toml
./crates/zipp-sandbox/target/release/zipp-sandbox script.js
```

Imports are denied unless the host supplies one canonical root. The native
runner is language/process/resource containment, not a kernel sandbox; use a
restricted account, container, or OS sandbox when the threat model requires
one. See [`SECURITY.md`](SECURITY.md) for the full deployment checklist.

### Semantics switches

<details>
<summary><strong>Call evaluation order and diagnostic switches</strong></summary>

Every shipping profile evaluates a method call's reference before its
arguments, as EvaluateCall requires: `receiver.m(input.value)` runs the
getter on `m` before the getter on `value`, an argument's coercion cannot
replace the method already fetched, and when both sides throw the method's
exception is the one seen. The fused `CallMethod` lowering — the op the method
inline caches, intrinsic arms and inlining key on — is used only for
arguments that provably cannot observe the order (literals, register-resident
locals, arithmetic over literals, array/object/closure literals of such
parts); every other argument shape takes the captured `GetProp` +
`CallWithThis` path. That path is not the slow one: the interpreter serves
a captured boot intrinsic (`arr.push(i % 13)`, `s.charCodeAt(a[i])`,
`m.get(k + 1)`) through the same inline and name-dispatched builtin lanes as
the fused form once the captured value is proven identical to the live
prototype intrinsic, and answers the read itself from the same proof; a
captured value that differs — an own shadow, an override installed before or
by the arguments, a subclass method — is invoked exactly as captured. Two
environment variables exist for diagnostics and benchmarking, read once per
process:

| Variable | Effect |
|---|---|
| `ZIPP_RELAXED_CALL_ORDER=1` | Re-admits the pre-audit "primitive-operand" class (global, cell and property reads and arithmetic over them) to the fused lowering. Faster on some rows; observably wrong for a getter or proxy trap on either side of the call. Never a shipping profile. |
| `ZIPP_STRICT_CALL_ORDER=1` | Forces the default, and wins when both are set. |

`crates/zipp-vm/tests/call_order_default.rs` runs the audit's probes under the
default, the interpreter, forced JIT, GC stress and both switches in clean
child processes.

</details>

## Performance, measured honestly

Start with the recorded native results. These are stamped benchmark captures,
with the source revisions and raw evidence below; they are not a new measurement
of every change on `main`. Ratios are **Zipp / competitor**, so **lower is faster**.

| Native workload group | Zipp / Node geomean | Coverage |
|---|---:|---|
| All 30 workloads | **0.728×** | 13 normal + 17 hostile rows, weighted equally. |
| Normal workloads | **0.614×** | All 13 normal rows. |
| Hostile workloads | **0.829×** | All 17 stress-oriented rows. |

Median process launch in the canonical four-engine capture is **7.4 ms** for
Zipp, **30.4 ms** for Node, **43.3 ms** for Bun and **82.6 ms** for Deno.

> The goal is ambitious and literal: become faster than Node, Bun, and Deno on
> every maintained benchmark while preserving exact output and tier parity.
> Zipp is not there yet. The tables below show both the wins and the remaining
> gaps.

### Canonical public capture

<details>
<summary><strong>Full Node, Bun and Deno results, confidence intervals and methodology</strong></summary>

The current public evidence is the clean PGO capture at engine commit
`8229b3fc`: [`real13_8229b3fc_pgo_2026-09-02.json`](bench/real13_8229b3fc_pgo_2026-09-02.json)
and [`head_clean_8229b3fc_pgo_2026-09-02.json`](bench/hostile/head_clean_8229b3fc_pgo_2026-09-02.json).
Both artifacts record `publishable:true`, `ALL_CORRECT=1`, 15 complete
counterbalanced repetitions, 10,000 bootstrap samples, exact output, and no
source, engine, input, environment, process-health, or harness drift.

Node v24.12.0 · Bun 1.3.14 · Deno 2.6.10 · Zipp 0.0.13 canonical PGO SHA-256
`bf9fddab…dc9986`.

Cold medians include process launch; bold marks the lowest displayed median.

| Retained benchmark | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| async-promise-chain | **334 ms** | 369 ms | 359 ms | 372 ms | 1.12× |
| class-prototype-hot | 297 ms | 332 ms | 329 ms | **226 ms** | **0.77×** |
| json-large | 270 ms | **192 ms** | 322 ms | 271 ms | 1.01× |
| map-set-heavy | 784 ms | 855 ms | 1,264 ms | **672 ms** | **0.84×** |
| markdown-render | 268 ms | **207 ms** | 316 ms | 209 ms | **0.77×** |
| parse-large-js | 273 ms | **230 ms** | 296 ms | 233 ms | **0.86×** |
| polymorphic-objects | 328 ms | 331 ms | 340 ms | **309 ms** | **0.94×** |
| regex-log-scan | 478 ms | 564 ms | 460 ms | **448 ms** | **0.94×** |
| sparse-array | 81 ms | 113 ms | 129 ms | **73 ms** | **0.91×** |
| typedarray-math | 200 ms | 914 ms | 170 ms | **144 ms** | **0.72×** |
| **Zipp / engine paired geomean** | **0.878×** [0.875, 0.884] | **0.752×** [0.747, 0.755] | **0.765×** [0.761, 0.774] | — | — |

The three architecture diagnostics remain outside the retained-ten headline:

| Diagnostic | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| polymorphic-objects-v2 | 81 ms | 87 ms | 131 ms | **24 ms** | **0.30×** |
| property-ic-shapes | 265 ms | 158 ms | 319 ms | **10 ms** | **0.04×** |
| sparse-array-v2 | 171 ms | 366 ms | 184 ms | **99 ms** | **0.59×** |
| **Zipp / engine paired geomean** | **0.186×** [0.183, 0.188] | **0.166×** [0.164, 0.169] | **0.145×** [0.143, 0.149] | — | — |

Across all 13 normal rows, Zipp measures **0.614× Node** [0.611, 0.617],
**0.531× Bun** [0.528, 0.533], and **0.521× Deno** [0.519, 0.526]. It wins
33 of 39 point comparisons and 31 of 39 Bonferroni exact-sign comparisons.

The separately measured 17-case hostile corpus covers closures, mixed locals,
shape churn, GC survival, async lifetimes, modules, a React-shaped kernel, a
warm router, a JavaScript bytecode VM, and vendored NanoID:

| Hostile metric | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| ordinary equal-row geomean | **0.829×** [0.820, 0.833] | **0.647×** [0.643, 0.655] | **0.419×** [0.415, 0.423] |
| category-balanced geomean | **0.862×** [0.852, 0.865] | **0.661×** [0.656, 0.673] | **0.432×** [0.429, 0.436] |

For the requested project-wide view, the explicit equal-row aggregate across
all 30 normal and hostile rows is **0.728× Node** [0.723, 0.730], **0.594× Bun**
[0.591, 0.598], and **0.460× Deno** [0.458, 0.464]. It is calculated as
`exp((13 × ln(G13) + 17 × ln(G17)) / 30)`; its descriptive bootstrap resamples
the two separately captured suites as independent strata.

The aggregate is ahead, but the literal every-row target is not met. Zipp has
21 of 30 Node point wins. The current Node point gaps are async promises and
JSON in the normal set, plus closure calls, both shape stressors, allocation
survival, long-lived async, React reconcile, and the warm router in the hostile
set (the JSON and long-lived async intervals cross parity; both are 1.005×).
The hostile guide reports each ratio rather than hiding these behind the
geomean.

```text
FASTER_THAN_NODE_ON_EVERY_ROW=0
FASTER_THAN_EVERY_ENGINE_ON_EVERY_ROW=0
```

The `8229b3fc` engine keeps the `c28781cf` levers (the inline dense-Array
store lane, the fused `| 0` add, B263's register classes) and adds B269-B273:
RegExp exec under a heap ceiling no longer walks the heap per exec, the recycle
pool's fallback sort is run-adaptive, a function that reaches itself through a
captured cell gets the native cross lane, bodies with `for...of` or `try`
receive a frame-backed cross entry instead of the interpreter trampoline, and
small holders take the holder-grain write barrier so an overwritten young value
no longer floats into old space. Each landed with a one-binary latch A/B; the
capture-to-capture row moves sit inside the intervals. Main has since added
B274-B278, which are interpreter-side (the wasm rows above) and leave this
native capture as the current public score. See the
[`bench` guide](bench/README.md), [hostile suite](bench/hostile/README.md), and
[`PERF_ROADMAP.md`](PERF_ROADMAP.md) for exact methodology and remaining work.

</details>

### QuickJS-NG and Boa diagnostics

These sections preserve earlier measurements and module snapshots. They
describe the named captures, including their limitations, rather than the
current release artifact.

<details>
<summary><strong>Historical interpreter and WebAssembly comparisons</strong></summary>

#### Historical v0.0.5 release

The v0.0.5 release was also measured against pinned interpreter builds of
QuickJS-NG v0.16.2 and Boa v0.22.0. These are clean release-default builds on
the same Windows x86-64 host, with identical generated source, exact-output
validation, six counterbalanced repetitions, and 10,000 paired-bootstrap
samples. Ratios are Zipp / competitor, so lower is faster.

| Native diagnostic | Zipp interpreter / competitor | 95% CI | point wins |
|---|---:|---:|---:|
| frozen real13 vs QuickJS-NG | **0.6413×** | 0.6386–0.6452 | 12 / 13 |
| micro5 vs QuickJS-NG | **0.8556×** | 0.8405–0.8761 | 5 / 5 |
| micro5 vs Boa | **0.2539×** | 0.2501–0.2590 | 5 / 5 |
| micro5 vs Boa `--optimize` | **0.2522×** | 0.2493–0.2593 | 5 / 5 |

The native result is an aggregate win, not a universal claim: QuickJS-NG was
1.0099× faster at the point median on the retained sparse-array row, while
Zipp led the other twelve. In the historical v0.0.5 browser-WASM release
capture, Zipp measured **0.2274× Boa** but **2.1074× QuickJS-NG** on adjusted
execution across the five diagnostic workloads. That release's stripped module
is 5,595,833 bytes raw (1,254,075 Brotli-11), between QuickJS-NG's
1,528,293-byte reactor (417,087 Brotli-11) and Boa's 21,296,176-byte module
(5,484,164 Brotli-11).

#### v0.0.6 native confirmation

The clean default-feature v0.0.6 release binary at engine-source commit
`e3acee352074` reran all 13 current real13 inputs against QuickJS-NG v0.16.2,
with the runner selecting Zipp's interpreter through `ZIPP_NOJIT=1`. All 39
canonicalized validation outputs matched after the documented QuickJS CRLF-to-LF
normalization; raw output bytes and hashes remain recorded. Across six
counterbalanced rounds, Zipp won all 13 point medians: Zipp / QuickJS-NG was
`0.6089665×` (descriptive 95% interval 0.6072021–0.6122180) for cold
fresh-process time and `0.6058409×` (0.6041440–0.6090422) after paired
empty-launch subtraction. This confirms the native interpreter result on the
final engine code; it does not predict WASM performance.

#### v0.0.6 browser-WASM status

The production module recorded in this historical comparison, built from v0.0.13, is 5,558,860 bytes raw, 1,812,458 at
gzip-9, and 1,248,649 at Brotli-11 (SHA-256
`bd8614fe5f3a3b8ef67f4b917cdefebb3fe69afa39a9804a0d3f6b0b6b267126`). The
official QuickJS-NG v0.16.2 reactor is 1,528,293 bytes raw and 417,087 at
Brotli-11, so Zipp is `3.586×` as large raw and `2.958×` as large on the wire.

Main has moved past that module. On 2026-09-05 an external audit of the
WASM build was implemented as B274-B278 (see
[`PERF_ROADMAP.md`](PERF_ROADMAP.md)): interpreter-side changes that close
four cliffs the native PGO capture never sees. Every unit-addressed read on a
non-ASCII string decoded from byte zero, so scanning loops were quadratic;
the string-part allocation preflight walked the whole heap on a window blind
to the heap's size; `eval` / `new Function` code owned no inline caches; and
an array with a named property lost its dense read path. The figures below
are interleaved A/B medians of the wasm artifact built from `400bcfe3`
against the same artifact with these changes, on a shared developer machine
with other work running, so they are diagnostic, not a canonical capture;
the control kernels (ASCII scans, plain and fused calls, main-code property
loops) moved within ±4%.

| Wasm kernel | `400bcfe3` | with B274-B278 |
|---|---:|---:|
| sequential `charCodeAt` over 64K non-ASCII units | 4,462 ms | 3.2 ms |
| word tokenizer over 64K mostly-ASCII units with a few accents | 9,828 ms | 8.2 ms |
| one-unit `slice` loop, 16K non-ASCII units | 449 ms | 4.3 ms |
| `join` of 4,000 parts × 200, 300K objects retained | 8,423 ms | 138 ms |
| `join` of 4,000 parts × 200, small heap | 548 ms | 130 ms |
| monomorphic property loop installed through `new Function` | 20.9 ms | 14.8 ms |
| `a[i]` loop on an array carrying a named property | 13.3 ms | 9.6 ms |

That recorded module predates these changes. The harness that produced the rows is
`crates/zipp-wasm/tests/node/bench.cjs`-style (persistent `Engine`, warmed,
interleaved builds).

We also attempted a direct, unscaled WASM run over the same v0.0.6
normal 13 and hostile 17 sources used by the v0.0.6 Node/Bun/Deno reruns in
`target/bench-results/real13-v006-6650647a718c-pgo-15.json` and
`target/bench-results/hostile17-v006-6650647a718c-pgo-15.json`. Those sources
are newer than the retained canonical public capture below. The WASM capture
preserves their exact bytes and Node output oracle, but it is explicitly
`publishable:false`: the production Zipp WASM API cannot load the two module
rows, QuickJS-NG's official reactor cannot drain pending jobs for three async
rows, and Zipp validated only 7 of the 28 script rows. Seventeen Zipp rows hit
the fixed production instruction or heap ceilings and four ended in other
engine errors. There were consequently no comparable normal-suite rows and
only five comparable hostile rows.

On those five available rows, Zipp / QuickJS-NG was `0.9604×` for persistent
time and `0.9567×` after paired-control subtraction, with Zipp ahead only on
`warm-router` (1 / 5 point wins). Those are incomplete row-level diagnostics,
not full-suite geomeans: this run does **not** establish that Zipp WASM is faster
than QuickJS-NG WASM. Zipp's separately sampled compile median was slower
(5.080 ms versus 1.795 ms), while its instantiation/start median was faster
(0.397 ms versus 1.676 ms). Their sums are not a measured end-to-end median.

The separate five-workload speed-kernel experiment remains useful attribution
evidence: it measured `0.0954663913×` QuickJS-NG on persistent time. It is highly
specialization-sensitive, however; disabling the exact workload lanes measured
`1.815×` QuickJS-NG but `0.199×` Boa, with Zipp ahead of Boa on all five rows.
That control used a dirty-tree diagnostic candidate, not the release artifact.
No current same-source normal-13-plus-hostile-17 Boa WASM run exists, so neither
micro result is a general interpreter ranking or a substitute for the
incomplete exact-suite result above.

The commands, exact revisions, all validation failures, per-row numbers, module
hashes, host-interface differences, and limitations are in
[`bench/comparison/README.md`](bench/comparison/README.md). These ecosystem
comparisons are deliberately separate from the canonical Node/Bun/Deno series
below.

</details>

## Correctness and language coverage

Zipp currently passes **95,939 of 95,942** required test262 executions. The
three known remaining cases are one Annex B test carrying a superseded ES2017
expectation and two rows that require German CLDR data; the exact list is
[`tools/test262-expected-failures.txt`](tools/test262-expected-failures.txt).

The [12 September 2026 correctness audit](docs/audits/2026-09-12-correctness.md)
verified this result against Test262 `defaaf1571`, including staging and excluding
the separate ECMA-402 suite. It tightened negative-test scoring and fixed 42
executions previously counted as passes despite reporting the wrong error type.
The runner's `--expected-failures` option now rejects unexpected failures, stale
expectations and skips; `--json` records the engine and corpus identities.
The former errored-module-cycle and deferred top-level-await failures are fixed.

The [deep follow-up audit](docs/audits/2026-09-12-follow-up.md) then exercises
re-entrant buffer operations, strict numeric coercion, Promise capabilities,
temporary GC roots, live collection iteration, weak references/finalization,
compiler metadata boundaries and hostile browser-host exceptions. Its focused
regressions run in both the ordinary and safe-sandbox profiles, and its weak
reference/finalization Test262 shards pass 603 of 603 executions.

The standing correctness strategy compares default JIT, interpreter-only,
forced-JIT, and majors-only-GC modes. A tier-differential fuzzer also generates
self-checking programs and compares Node, the interpreter, and tier-forcing
switches; benchmarks alone are never treated as proof of language correctness.

ES2015–ES2025 is essentially complete, including:

- classes, private elements, static blocks, and all eight decorator kinds;
- generators, async generators, promises, iterator helpers, and explicit
  resource management;
- 12 TypedArray kinds including `Float16Array`, `DataView`, shared memory, and
  atomics;
- `BigInt`, `Proxy`/`Reflect`, modern regular expressions, and `Temporal` with
  fifteen calendars;
- ES modules, dynamic/typed/deferred/source-phase imports, and top-level await;
- `eval`, `Function`, `ShadowRealm`, structured cloning, and browser-oriented
  embedding APIs.

Only the `en` CLDR locale ships today. The detailed support notes and durable
architecture reference live in [`DOC.md`](DOC.md).

## How it works

Follow a program from source text to running code. Zipp's lexer, parser,
bytecode compiler, NaN-boxed register VM, garbage collector, inline caches and
native JITs are implemented here, so you can explore each stage in one codebase.

```mermaid
flowchart LR
    A[JavaScript source] --> B[Lexer and parser]
    B --> C[Register bytecode]
    C --> D[Interpreter]
    D --> E[Hot-loop OSR]
    D --> F[Whole-function JIT]
    E --> G[x86-64 / ARM64 native code]
    F --> G
    D <--> H[GC, shapes, inline caches]
    G <--> H
```

x86-64 has the mature function, OSR, helper, inline-cache, integer, double, and
guarded reducer tiers. ARM64 has a smaller guarded whole-function integer
baseline. wasm32 and unsupported native targets use the pure interpreter.

Workspace map:

| Path | Purpose |
|---|---|
| [`crates/zipp-vm`](crates/zipp-vm) | Parser, compiler, VM, runtime, GC, JITs, and the experimental Python frontend (`src/frontend`). |
| [`crates/zipp-cli`](crates/zipp-cli) | `zipp js` / `zipp mjs` / `zipp py` command line. |
| [`crates/regress-fork`](crates/regress-fork) | ECMAScript regex engine fork and conformance fixes. |
| [`crates/zipp-wasm`](crates/zipp-wasm/README.md) | Browser/Worker embedding. |
| [`crates/zipp-sandbox`](crates/zipp-sandbox/README.md) | Separately resolved hardened native runner. |

## Reproduce and contribute

Bug reports, documentation improvements, small fixes and careful measurements
are welcome. Browse the [open issues](https://github.com/f2i-com/zipp.org/issues)
or the [roadmap](PERF_ROADMAP.md) to find a place to start.

For bug reports, include a small JavaScript example, the expected output, your
Zipp version and execution profile. A clear reproducer makes it much easier to help.

Run the release tests before changing the engine:

```sh
cargo test --workspace --release
cargo check -p zipp-vm --no-default-features
cargo check -p zipp-vm --no-default-features --features safe-sandbox
```

Build the measured Windows PGO binary from an x64 Visual Studio Developer
PowerShell with native Git Bash:

```powershell
& 'C:\Program Files\Git\bin\bash.exe' tools/pgo.sh
```

Routine benchmark artifacts belong under ignored `target/bench-results/`.
Only deliberately reviewed canonical evidence is promoted into `bench/`.
Commands, suite ownership, publication rules, and A/B examples are in
[`bench/README.md`](bench/README.md).

The project keeps negative results because a measured refutation is cheaper
than repeating the same attractive mistake. Start with:

| Document | Use it for |
|---|---|
| [`DOC.md`](DOC.md) | Durable architecture, language, embedding, and development reference. |
| [`PERF_ROADMAP.md`](PERF_ROADMAP.md) | Current performance evidence, open targets, and next gates. |
| [`HANDOFF.md`](HANDOFF.md) | Exact current continuation state and commands. |
| [`SECURITY.md`](SECURITY.md) | Threat model and deployment requirements. |
| [`docs/archive`](docs/archive/README.md) | Dated historical handoffs, designs, and the full experiment ledger. |

Small, independently measured changes are preferred. Keep correctness and
benchmark output exact, include an off-switch for risky optimizations, report
neutral or negative evidence, and do not update the public engine table without
a clean canonical capture.

If you find Zipp useful, [give the project a star](https://github.com/f2i-com/zipp.org)
or share what you're building with it.

## License

[Apache-2.0](LICENSE-APACHE).
