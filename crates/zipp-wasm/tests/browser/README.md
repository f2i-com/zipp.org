# Browser Worker smoke test

`cargo test` and the Node harnesses prove the engine and the wasm-bindgen
boundary under Node. They do not prove that the **web-target package** behaves
inside real browser Workers, which is where a host actually runs it (the
11 September 2026 audit's ZIPP-17). This directory does that, through the
reference host adapter in [`../../host-sdk/`](../../host-sdk/README.md):

- `worker-smoke.mjs` — a Node script that serves this crate over loopback,
  launches each requested browser with Playwright, opens `index.html`, and
  reads the results the page records.
- `index.html` — the page: every scenario runs through `createZippHost`, each
  guest in its own Worker and WASM instance.
- `bridges.mjs` — the in-memory `db`/`localStorage` fixture the Worker builds
  for the sync-bridge scenarios.

Scenarios: initialization and the symbol map; strict mode surviving the
preamble; structured reads, writes and fingerprints; the rich eval;
`resourceUsage` from inside the Worker; termination as the lifetime boundary;
a syntax error as a `source` failure; an instruction-budget crossing as a
terminal `resource` failure; the host-call queue with completion, duplicate
completion and cancellation; Worker-side sync bridges under default-deny; a
runaway guest stopped only by the deadline terminating the Worker, with prompt
rejection and a working replacement host; two tenants at once; events and
chronological console records.

```sh
# From crates/zipp-wasm, with the artifact built as release.yml builds it:
wasm-bindgen --target web --out-dir tests/browser/pkg \
  --remove-name-section --remove-producers-section \
  target/wasm32-unknown-unknown/release/zipp_wasm.wasm
npm install --no-save playwright            # or NODE_PATH to a copy
npx playwright install --with-deps chromium firefox webkit
node tests/browser/worker-smoke.mjs --browsers chromium,firefox,webkit
node tests/browser/worker-smoke.mjs --channel chrome   # an installed Chrome
```

The page runs the exact stripped web package (`tests/browser/pkg/`), which is
not committed. Passing here is Worker behaviour on the browsers named in the
output; it is not conformance coverage, and it does not replace the Node
boundary suite, which pins the resource contracts in detail.
