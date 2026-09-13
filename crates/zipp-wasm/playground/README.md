# Zipp playground

A browser page that runs a folder of Python **or** JavaScript files on the
Zipp WebAssembly engine: the files in a sidebar, an editor, a console, and a
canvas the program draws on through a small `ui` API. Everything runs in a
Worker with a deadline, so a runaway program costs itself its engine and never
the tab.

```sh
cd crates/zipp-wasm
./build-variants.sh all          # once: the Python-enabled engine into dist/all/
node playground/serve.cjs        # then open http://127.0.0.1:8765/crates/zipp-wasm/playground/
```

The server serves the repository root on loopback and provides the optional
native NCA runner described below. A static server supports the WASM playground
and GPU graph examples; native NCA execution requires `serve.cjs`.
`dist/` is build output and is not committed, so the first step is required.

## Native NCA lab: your GPUs and live model state

From the repository root, double-click `START-GPU-LAB.cmd`, or run:

```powershell
node crates/zipp-wasm/playground/serve.cjs
```

Open **http://127.0.0.1:8765/crates/zipp-wasm/playground/nca.html**, or choose
**NCA lab · native GPUs** from the playground toolbar / Samples menu. Restart
an already-running older playground server to enable its new native endpoints.
If the default port is occupied, the launcher tries the next local port and
prints the actual **native GPU lab** URL. An explicitly set `PORT` is respected.
The NCA page itself does not require a WASM build or npm install.

The default source is the sibling `nca_fast_memory_language_lab` directory.
The runner imports its real `CellularMemory`, `CausalNCALM`, task generator,
batch sampler and evaluation functions. Four runnable examples are supplied:

- **Memory replay:** loads `results/fast_seed0/model.pt`, writes fresh observations,
  displays the private matrices and query packet's ring position, and checks that
  shared parameters stayed frozen. The observed answer is used only for evaluation.
- **Memory training:** trains shared weights on fresh episodes, displays actual
  loss / bit accuracy and memory tensors, then replays the trained model.
- **Language generation:** loads `language_toy_longer/model.pt`, streams generated
  bytes and displays actual per-stage incremental cache activations.
- **Language training:** trains from scratch on `toy_text/train.txt`, evaluates on
  `toy_text/validation.txt`, and displays loss, final recurrent activations and a
  generated sample. Short smoke runs produce untrained, mostly unreadable text.

Choose one or more CUDA devices. Each selected device runs an **independent
experiment in its own Python process**, with a distinct seed. This uses both
GPUs concurrently; it does not shard one model or combine their VRAM. The view
selector switches between their live states. CPU is an explicit alternative.
The capability probe runs a small matrix multiplication on each CUDA device
before marking it usable; a CUDA failure never silently becomes a CPU run.

Native numerical work runs in PyTorch/CUDA. The browser draws the tensor heatmap
and ring using hardware WebGL2, requesting `high-performance` and rejecting
recognizable software renderers. WebGL / WebGPU choose one browser adapter;
JavaScript cannot force a numbered CUDA device or combine browser GPUs.
The graph backends also request hardware acceleration and report adapter identity.
The existing `auto` graph mode still visibly falls back to WASM / JavaScript when
hardware is unavailable; explicit WebGL2 / WebGPU selections fail instead.

Colors represent signed tensor values, normalized by that frame's RMS; ring
brightness represents each cell's memory RMS. Cells run clockwise from 0 at the
top, with the current writer / query cell highlighted. NVIDIA utilization, VRAM,
temperature and power are **system-wide** readings, not per-job measurements.
These tiny research models and paced replays may use only a small percentage of
a large GPU; this is not a speedup benchmark.

New outputs, checkpoints and full training logs go into unique
`target/nca-runs/<run>-<device>/` directories. The original lab and its checkpoints
are not modified. Training starts from initialization; this UI does not resume
optimizer state or execute arbitrary edited Python. Stop terminates all workers
in the active run; completed checkpoints survive. Closing the tab lets the run
continue; reopening recovers the active state. Ctrl+C in the server stops workers.
Only one run group is accepted at once, with a 30-minute per-process deadline.

Optional configuration, before starting the server:

```powershell
$env:NCA_PYTHON = 'C:\Python311\python.exe'
$env:NCA_LAB_DIR = 'C:\path\to\nca_fast_memory_language_lab'
$env:PORT = '8771' # if the default port is occupied
node crates/zipp-wasm/playground/serve.cjs
```

Use a [CUDA-enabled PyTorch build](https://pytorch.org/get-started/locally/) that
supports your GPU. The UI reports missing Python, PyTorch, checkpoints and source
paths. If WebGL is unavailable, enable browser graphics acceleration and restart
the browser. Native compute can still run while the visualization is unavailable.
The runner is local only: fixed workloads, bounded options, loopback binding,
Host / Origin checks and a same-origin token on run / stop requests. No shell or
browser-submitted source is executed.

Acceptance checks (Python Playwright plus installed Chrome required):

```powershell
python crates/zipp-wasm/playground/nca-smoke.py
node --test --test-isolation=none crates/zipp-wasm/gpu-lab/tests/*.test.mjs
```

The smoke script tests all four examples, concurrent CUDA devices, explicit CPU,
stop / reload, API validation, responsive layout, and 15 numerical cases on each
real WebGL2 / WebGPU backend. Missing GPU hardware is an acceptance failure.
Reports and screenshots are written under `target/nca-smoke/`.
Verified on Windows with two RTX 5090s, PyTorch 2.11.0+cu128 and Chrome:
both CUDA devices passed, and both browser GPU backends passed all 15 cases.

The memory and language models remain separate research examples. Supplied
language weights use template text and a 17-byte context, as documented by the lab.

## Using it

- **Open folder** (or drop a folder on the page) loads the whole folder:
  subfolders, data files (`.json`, `.txt`, `.md`, ...) and binaries such as
  model checkpoints, shown as a tree in the sidebar. Tool folders (`.git`,
  `__pycache__`, `node_modules`, `target`, virtual environments) are skipped,
  as are files over 8 MiB or beyond 64 MiB in total. **Open files** loads
  loose files. **Samples** has two bouncing-ball projects, a Langton's ant
  written with classes, dataclasses and enums, a GPU compute demo, and two
  hello-world programs.
- The **entry** file runs first: `main.py` or `main.js` by default, or any
  `.py`/`.js` file in the tree through *Set as entry* (a `tests/test_x.py`
  entry runs its tests). The entry's extension decides the project language;
  files of the other language are listed but not run. The **arguments** box
  in the toolbar is the command line: `run.py --steps 20 "two words"` gives
  the program `sys.argv[1:] == ["--steps", "20", "two words"]`.
- A Python program sees the folder as its filesystem: `open()`, `os`,
  `os.path`, `pathlib` and `json.load` read the loaded files (packages by
  folder import as `legacy.fast_memory`), and files it writes appear in the
  tree tagged *written*, openable like any other. Binary files show a
  placeholder in the editor and cannot be edited; text files can. Nothing is
  written back to disk: the browser holds the project, and the written
  files live in its `localStorage` copy with the rest.
- Edit in place; `Tab` indents, `Ctrl+Enter` runs. The project is autosaved
  in the browser's `localStorage` and restored on the next visit.
- **Run** compiles and runs the top level. If the program defines `draw` or
  `update`, the playground then runs a frame loop: each frame it sends the
  input snapshot, delivers queued `on_click`/`on_key` calls, calls `update()`
  then `draw()`, and paints whatever `ui` commands came back. Click the canvas
  to give it keyboard focus. **Stop** ends the loop.
- The console shows `print`/`console.log` output, errors (with the file and
  line for Python compile errors, e.g. `physics.py:2:12`), and playground
  notes. The status bar shows the engine version and the languages it was
  built with.

## The program contract

Python modules `import` each other by file stem (`from physics import step`)
or by folder (`from pkg.tools import twice`), can import the built-in
modules and the bundled libraries (`zipp_gpu`, the `torch` subset,
`pytest`, ...), and the built-in `ui` module. JavaScript files share one global
scope in sidebar order with the entry last, like successive `<script>` tags,
and `ui` is a global. Both languages see the same API:

| call | effect |
| --- | --- |
| `ui.canvas(w, h)` | resize the canvas |
| `ui.clear(color)` | fill the canvas |
| `ui.rect(x, y, w, h, color)` / `ui.circle(x, y, r, color)` / `ui.line(x1, y1, x2, y2, color)` | draw shapes |
| `ui.text(x, y, text, color)` / `ui.font(size)` | draw text; set the text size |
| `ui.button(x, y, w, h, label)` | draw a button; returns `True` in the frame it was clicked |
| `ui.mouse()` | `(x, y, down)` |
| `ui.clicked()` | whether the mouse was clicked this frame |
| `ui.key(name)` | whether a key is held (`"ArrowLeft"`, `" "`, `"a"`, …) |
| `ui.width()` / `ui.height()` | the canvas size |

Optional top-level hooks in the entry file:

| hook | when |
| --- | --- |
| `update()` | once per frame, before `draw` |
| `draw()` | once per frame |
| `on_click(x, y)` | for each click on the canvas since the last frame |
| `on_key(key)` | for each key press since the last frame |

Colors are CSS color strings and coordinates are numbers (ints or floats).
A program that defines neither `draw` nor `update` just runs its top level:
`print` and any `ui` calls it makes are shown once.

## What is behind it

- `engine.worker.js` holds the engine. Python programs go through
  `Engine.initPythonProject` and the Python hooks (`pythonCall`, `pythonHas`,
  `takeUi`, `setPythonInput`); JavaScript programs are prepended with a
  one-line `ui` shim and go through `initSource(..., "javascript")`,
  `callFunction` and global slots. Both produce the same command arrays.
- `playground.js` is the page: project state, the editor, the frame loop, the
  canvas renderer and the deadline (5 s per request; a miss terminates the
  Worker and starts a fresh one).
- Python programs can compute on the GPU: `from zipp_gpu import Graph`, build
  a float32 graph with tensor arithmetic, and `graph.submit(callback,
  result=...)`. The worker hands the graph to the vendored GPU Lab runtime
  (`../gpu-lab/`), which runs it on WebGPU, WebGL2, compiled WASM kernels or
  a JavaScript reference (the **GPU** selector in the toolbar; `auto` tries
  them in that order and the console says which one answered), then calls
  the program back with the outputs between frames. The "GPU compute" sample
  runs a few graphs and steps Conway's life on the backend every frame.
- `serve.cjs` is the server; `smoke.cjs` drives the page in a local
  Chrome or Edge through Playwright (`npm install --no-save playwright`, then
  `node playground/smoke.cjs`; set `PLAYWRIGHT_CHANNEL=msedge` for Edge).

Limits are the engine's: a 2,000,000,000-instruction budget (the maximum
the engine allows) for the program's top level and then for each frame,
renewed by the worker before every frame so a long-running animation is
bounded per frame rather than in total, the engine's heap and output ceilings,
and the Python frontend's compile-time caps. The Python side is Zipp's own
Python 3 implementation described in `docs/PYTHON_FRONTEND_EXPERIMENT.md`
(classes, exceptions, generators, `match`, the builtin types and a set of
standard modules; no `async` or real files). GPU graphs are validated and
bounded again by the host runtime (node count, tensor sizes, work and
allocation budgets) and served one at a time, at most 16 pending.
