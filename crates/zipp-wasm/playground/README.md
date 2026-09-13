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

The server is a dependency-free static server for the repository root on
loopback; any static server that serves the repository root works the same.
`dist/` is build output and is not committed, so the first step is required.

## Using it

- **Open folder** (or drop a folder on the page) loads every `.py` / `.js` file
  at the folder's top level. **Open files** loads loose files. **Samples** has
  two bouncing-ball projects and two hello-world programs.
- The **entry** file runs first: `main.py` or `main.js` by default, or any file
  through *Set as entry*. The entry's extension decides the project language;
  files of the other language are listed but not run.
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
and can import the built-in `ui` module. JavaScript files share one global
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

Colors are CSS color strings. In Python every coordinate is an integer (the
subset has no floats); in JavaScript any finite number works. A program that
defines neither `draw` nor `update` just runs its top level: `print` and any
`ui` calls it makes are shown once.

## What is behind it

- `engine.worker.js` holds the engine. Python programs go through
  `Engine.initPythonProject` and the Python hooks (`pythonCall`, `pythonHas`,
  `takeUi`, `setPythonInput`); JavaScript programs are prepended with a
  one-line `ui` shim and go through `initSource(..., "javascript")`,
  `callFunction` and global slots. Both produce the same command arrays.
- `playground.js` is the page: project state, the editor, the frame loop, the
  canvas renderer and the deadline (5 s per request; a miss terminates the
  Worker and starts a fresh one).
- `serve.cjs` is the static server; `smoke.cjs` drives the page in a local
  Chrome or Edge through Playwright (`npm install --no-save playwright`, then
  `node playground/smoke.cjs`; set `PLAYWRIGHT_CHANNEL=msedge` for Edge).

Limits are the engine's: a 2,000,000,000-instruction lifetime budget per
run (the maximum the engine allows), the engine's heap and output ceilings,
and the Python frontend's compile-time caps. The Python side is the
experimental subset described in `docs/PYTHON_FRONTEND_EXPERIMENT.md`
(integers, strings, lists, tuples, ranges, functions, modules; no floats,
classes or exceptions yet).
