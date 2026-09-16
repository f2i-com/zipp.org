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

The server serves browser assets on loopback. No native Python process or
CUDA endpoint is part of Zipp's playground. `dist/` is build output, so the first
step is required. The independently maintained native research lab is at
[neuralautomata.com](https://github.com/f2i-com/neuralautomata.com).

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
  tree tagged *written* (by the latest run), openable like any other, and
  sent to later runs with the rest of the project, so a program can keep
  state in its own files. Binary files show a placeholder in the editor and
  cannot be edited; text files can. Nothing is written back to disk: the
  browser holds the project, and the written files live in its
  `localStorage` copy with the rest.
- Edit in place; `Tab` indents, `Ctrl+Enter` runs. The project is autosaved
  in the browser's `localStorage` and restored on the next visit, within the
  browser's storage quota (about 5 MB of text; binaries share a 3 MiB
  allowance). A project that does not fit is not autosaved, the console says
  so, and no older snapshot is left to restore in its place.
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

**JavaScript GPU access:** ordinary browser JavaScript can call the shared
`createRuntime` API directly or create its own WebGL canvas. The
[JavaScript and Python GPU examples](../../../README.md#gpu-computing-from-javascript-and-python)
show both paths. JavaScript in this playground's editor executes inside Zipp's
VM, where browser DOM/WebGL objects are not automatically available. Only the
Python guest GPU bridge is currently wired here; the separate JavaScript host
adapter requires integration by an embedder.

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
| `ui.text(x, y, text, color)` / `ui.font(size)` | draw text (the page paints at most 4096 characters of one text, 65,536 per frame); set the text size |
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
  the program back with the outputs between frames. The "Game of Life" sample
  uses ordinary Torch rules in `life.py`, with GPU submission and drawing in
  `main.py`. The rules also run unchanged in regular PyTorch.
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

## Torch inference and training

Choose **Samples → Python: Torch ML inference (GPU)** to run a supported
`torch.nn` model through `torch.compile(model)` on the selected backend.
No `zipp_gpu` import is needed. `.submit(callback)` delivers a CPU tensor after
the GPU completes; this is an experimental asynchronous extension, not full
PyTorch. **Samples → Python: Torch ML training (GPU)** adds a live loss curve
for a dense ReLU network trained using MSE and SGD. `torch.compile(training=True)`
records forward, backward and updates. Each call uploads weights and reads them
back; persistent compiled models and GPU Conv2d are not implemented. See the [compatibility guide](../../../docs/TORCH_COMPATIBILITY.md).
