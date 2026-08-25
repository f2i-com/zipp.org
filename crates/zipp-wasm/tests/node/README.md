# Node harnesses for the wasm boundary

`cargo test` covers the engine and the Rust side of the embedding, but not the
wasm-bindgen boundary itself — marshalling, the bridge closures, the queue. These
run against a real wasm build under node.

```sh
cd crates/zipp-wasm
cargo build --locked --release --target wasm32-unknown-unknown
wasm-bindgen --target nodejs --out-dir tests/node/pkg \
  target/wasm32-unknown-unknown/release/zipp_wasm.wasm
node tests/node/check-wasm-memory.cjs tests/node/pkg/zipp_wasm_bg.wasm
node tests/node/host-contract.cjs
node tests/node/worker-deadline.cjs
SOFTN_REPO=../softn.com node tests/node/softn-snakegame.cjs
```

Both expect the generated glue at `tests/node/pkg/` (adjust the `require` at the
top of each file if you put it elsewhere).

- **check-wasm-memory.cjs** — verifies the final wasm-bindgen artifact still has
  exactly one unshared, non-memory64 linear memory capped at 256 MiB and only
  the audited host-import surface.
- **host-contract.cjs** — every method a UI host depends on: the symbol map and
  what it hides, structured global reads/writes, batching and the function-slot
  protection, the synchronous db/localStorage bridges, event dispatch, the
  `host.call` queue, and that a throw leaves the engine usable.
- **worker-deadline.cjs** — proves a responsive supervisor can terminate a
  Worker blocked in synchronous runaway WASM, then serve the next tenant in a
  fresh Worker/WASM instance.
- **softn-snakegame.cjs** — a real SoftN bundle's `.logic`, unmodified, driven the
  way its runtime drives it: `_init`, listener registration, arrow keys through
  `window.__snakeNextDir`, the tick loop, eating, game over, and a high score
  surviving a reload. The load-bearing assertion is the read-spread-write of
  `window` followed by a key dispatch that still reaches its handler — the shape
  that silently unregistered every listener before `host_in_over` existed.
