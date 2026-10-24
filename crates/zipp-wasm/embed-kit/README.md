# ZIPP {{VERSION}} WebAssembly embed kit

ZIPP, the **Zippy Interpreter for Polyglot Programming**, is a portable
polyglot runtime: one virtual machine that runs JavaScript and Python today.
This kit runs its WebAssembly engines in a web page, a Worker, Node, Deno or
Bun, inside a sandbox that can reach nothing you have not allowed.

## The files

Pick an engine; each is one file from the release:

| Module | Runs |
| --- | --- |
| `zipp-{{VERSION}}-javascript.wasm` | JavaScript, with `Intl` and `Temporal` |
| `zipp-{{VERSION}}-javascript-lite.wasm` | JavaScript without `Intl` and `Temporal`, built for size |
| `zipp-{{VERSION}}-javascript-python.wasm` | JavaScript and Python, torch built in |
| `zipp-{{VERSION}}-javascript-python-base.wasm` | JavaScript and Python, without torch (smaller) |
| `zipp-{{VERSION}}-torch-package.wasm` | torch, added to the `-python-base` engine when you need it |

Each also comes compressed with Brotli as `.wasm.br`, about a quarter of the
size. From this kit you need one file, `zipp.mjs`. `modules.json` lists each
module's size and SHA-256 (and its `.wasm.br`'s, when the release has one).

## Quick start

```js
import { loadZipp } from "./zipp.mjs";

const zipp = await loadZipp("./zipp-{{VERSION}}-javascript.wasm");
const sandbox = await zipp.createSandbox();          // nothing is permitted
try {
  const { output } = await sandbox.run("console.log([1, 2, 3].map(x => x * 2))");
  console.log(output);                               // ["[ 2, 4, 6 ]"]
} finally {
  await sandbox.close();
}
```

Serve the folder over HTTP (`python -m http.server` or `npx serve`); a page
opened as `file://` cannot load modules.

## How it works

1. **The module carries its own bindings.** Every engine `.wasm` holds the
   JavaScript glue that was generated for exactly that build, in a custom
   section, beside a small record of its version and variant. `loadZipp`
   compiles the module, reads that glue out of it and starts the engine. You
   never manage a separate glue file, and the two can never come from
   different builds.
2. **A sandbox is one program in its own engine.** `createSandbox` makes a
   fresh engine; `run(code)` runs the program; `call(name, args)` calls its
   functions afterwards, with its state kept between calls.
3. **Nothing is allowed until you allow it.** The program cannot see the
   page, its `localStorage`, cookies, the DOM, Node's `process` or
   `require`, the file system or the network. It can compute and print.
   Each permission you grant opens one door, and only that door.
4. **It runs in a Worker.** By default a sandbox runs in its own Worker
   (a worker thread in Node), so a program that never stops cannot freeze
   your page: when it passes its time limit, the Worker is ended.
5. **Limits are hard.** A step budget stops runaway computation inside the
   engine; the time limit ends the Worker from outside. Either way the
   sandbox is finished and you create a new one.

## Permissions

```js
const sandbox = await zipp.createSandbox({
  limits: { instructions: 50_000_000, timeoutMs: 5000 },
  permissions: {
    storage: { initial: { theme: "dark" } },        // its own private localStorage
    files: { "data.csv": "a,b\n1,2" },              // files it may read
    network: ["https://api.example.com/"],          // where its fetch() may go
  },
  host: { "clock.now": () => Date.now() },          // functions it may ask you to run
});
```

| Permission | What the program gets | Without it |
| --- | --- | --- |
| `storage: true` or `{ initial, quota }` | `localStorage`, private to this sandbox, in memory, 5 MB by default; read it back with `await sandbox.snapshot()` | `localStorage` throws `SecurityError` |
| `files: { path: text }` or `{ files, writable: true, quota }` | `host.callSync("fs.read", path)` (text or `null`), `"fs.list"`, `"fs.exists"`; with `writable`, `"fs.write"` and `"fs.remove"`. Paths stay inside the map | the calls throw `SecurityError` |
| `network: [allowed URLs]`, `true`, or `{ allow, methods, maxResponseBytes, timeoutMs, redirects }` | a `fetch(url, init)` that reaches only the URLs listed (an origin, or a URL prefix), with no cookies or credentials, the forbidden headers removed, redirects refused unless allowed (then up to 5, each destination checked against the list before it is requested; a browser does not reveal a redirect's destination, so there one still fails the request), a time limit and a 1 MB response limit by default; the reply has `status`, `ok`, `headers.get()`, `text()` and `json()` | `fetch` does not exist |
| `host: { kind: fn }` | `host.call(kind, args, reply => ...)`; your function's return value (or promise) arrives as `{ ok: true, value }` | `{ ok: false, error: { code: "denied" } }` |

Seeded storage and file entries must fit their quotas before a sandbox starts.
A trusted in-process storage `backend` owns its data and limits, so it cannot
be combined with `initial` or `quota`. Sandboxes using the same backend and
prefix share that backend's state; assign a distinct prefix per tenant.

A host function runs with `this.signal`, an `AbortSignal` that aborts when
the sandbox closes or gives the request up (its time limit passed); a
`fetch` the program makes is aborted the same way. At most 64 of a
sandbox's host requests run at once and the rest wait, in order, without
starting. A function that ignores the signal keeps its place until it
returns, and whatever it returns then is discarded; `usage()` reports
`hostRequestsRunning` and `hostRequestsWaiting`. `close()` settles a `run` or
`call` still waiting on host requests at once, with a `closed` error.
Under isolation, a host function's return value must be transferable by the
Worker's structured clone operation. An unsupported value answers that call
with `{ ok: false, error: { code: "host" } }`.

Unknown option names are errors, so a typo cannot quietly drop a limit.
Only an options object's own properties are read; inherited properties cannot
grant permissions or change the default Worker isolation.
`network: true` allows any http(s) URL under the same rules; prefer listing
the origins or URL prefixes a program needs.
A bare canonical origin such as `https://api.example.com` grants that whole
origin; its root-slash prefix (`https://api.example.com/`) covers the same
paths. Other entries are matched as full URL prefixes. For example,
`https://api.example.com/?scope=read` does not allow
`https://api.example.com/other`.

What each language can be given in this release:

| Permission | JavaScript | Python |
| --- | --- | --- |
| `storage` | yes | no |
| `files` | yes, through `host.callSync("fs.*")` | yes, as its importable modules (`name.py`, read-only) |
| `network` | yes, through `fetch` | no |
| `host` | yes, through `host.call` | no |
| `limits` | yes | yes |

This is layered protection, not a guarantee: WebAssembly's memory isolation,
an engine built without `unsafe` code or a JIT, default-deny capabilities, a
Worker per program, and the limits below. Keep a Content-Security-Policy on
the page and treat a program's output as untrusted data.

## Limits and isolation

| Option | Default | Effect |
| --- | --- | --- |
| `limits.instructions` | 50,000,000 | steps the program may take in total; past it the sandbox stops with a `resource` error |
| `limits.timeoutMs` | 30,000 | how long `run` or `call` may take, host requests included; past it an isolated sandbox's Worker is ended (`timeout`) |
| `isolate` | `true` | run in a Worker; `false` runs on the calling thread, where only the step budget can stop a loop |

The engine also has fixed ceilings (heap, string size, output, nesting);
`zipp.profile.limits` lists them. Failures are `ZippSandboxError`s with a
`category` (`guest`, `source`, `resource`, `timeout`, `host`, `usage`), a
`terminal` flag, and `output`: what was printed before the failure.

## Python and torch

```js
const zipp = await loadZipp({
  engine: "./zipp-{{VERSION}}-javascript-python-base.wasm",
  packages: ["./zipp-{{VERSION}}-torch-package.wasm"],   // optional: adds torch
});
const sandbox = await zipp.createSandbox({
  language: "python",
  permissions: { files: { "helpers.py": "def double(x):\n    return 2 * x\n" } },
});
try {
  const { output } = await sandbox.run("import helpers, torch\nprint(helpers.double(21), torch.tensor([1.0, 2.0]).tolist())");
  console.log(output);   // ["42 [1.0, 2.0]"]
} finally {
  await sandbox.close();
}
```

The complete `-javascript-python` engine has torch built in. A Python
program's `files` are its importable modules (`name.py`); it has no storage,
network or host functions in this release. `docs/TORCH_COMPATIBILITY.md`
lists what the torch package covers, and `gpu-lab/` is the browser GPU
adapter the complete engine's `torch.compile` path can use.

## Without a sandbox

`zipp.run(code, { language })` runs a program here and now, with nothing
permitted and no time limit, and returns `{ output }`. `new zipp.Engine()` is
the engine's full API (globals, events, host-call queues, bridges), declared
in `zipp-engine.d.ts` and described in `docs/zipp-wasm.md`. `host-sdk/` is a
lower-level Worker adapter for applications that manage the engine
themselves; its README has the contract.

## Serving the files

- Serve `.wasm` as `application/wasm` so browsers compile while
  downloading (with another type the loader downloads first).
- **Brotli.** The `.wasm.br` beside each module is the same engine, about a
  quarter of the bytes. Serve it with `Content-Encoding: br` and
  `Content-Type: application/wasm` and the browser decodes it on arrival; on
  Apache, `AddType application/wasm .wasm` with `AddEncoding br .br` does
  that, and a rewrite can send it to Brotli-capable browsers in place of the
  `.wasm`. `loadZipp("...wasm.br")` (loader 1.2.0 and later) also works where
  the server does not decode it: it decompresses the bytes itself where the
  runtime can (Node, Firefox) and otherwise falls back to the plain `.wasm`
  beside it, so keep both deployed.
- The loader imports the bindings from a `blob:` URL (browsers, Deno, Bun)
  or a `data:` URL (Node), and a sandbox starts a Worker from `zipp.mjs`. A
  Content-Security-Policy must allow them, e.g.
  `script-src 'self' blob: 'wasm-unsafe-eval'; worker-src 'self' blob:`.
- Cross-origin modules need CORS headers, as any `fetch` does. If you bundle
  `zipp.mjs`, pass `workerUrl` (a served copy of `zipp.mjs`) to
  `createSandbox`.

## Examples

Put the `.wasm` files beside this README, serve the directory
(`npx serve .` or `python -m http.server`), and open:

- `examples/browser.html`: an editor that runs each program in a fresh sandbox;
- `examples/sandbox.html`: one program with nothing permitted, then with
  storage, a file and one network folder, then a loop stopped by its time limit;
- `node examples/node.mjs`: the same from Node.

## Files

| Path | Purpose |
| --- | --- |
| `zipp.mjs`, `zipp.d.ts` | the loader and sandbox, and their types |
| `zipp-engine.d.ts` | the engine API (the complete engine's; Python members exist in the `-python` modules only; TypeScript needs `esnext` in `lib` for its `Symbol.dispose`) |
| `host-sdk/` | a lower-level Worker host adapter |
| `gpu-lab/` | the browser GPU adapter for the complete Python engine |
| `docs/` | the engine reference and torch coverage |
| `modules.json`, `BUILD-INFO.txt`, `SHA256SUMS` | the release's modules, this kit's build and its checksums |

Licensed under the Apache License 2.0 (`LICENSE-APACHE`); `gpu-lab/` carries
its own `LICENSE` and `NOTICE`.
