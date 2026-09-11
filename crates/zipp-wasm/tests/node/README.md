# Node harnesses for the wasm boundary

`cargo test` covers the engine and the Rust side of the embedding, but not the
wasm-bindgen boundary itself — marshalling, the bridge closures, the queue. These
run against a real wasm build under node.

```sh
cd crates/zipp-wasm
RUSTFLAGS='-Dwarnings -C link-arg=--max-memory=1073741824 -C link-arg=-zstack-size=1048576' \
  cargo +1.92.0 build --locked --release --target wasm32-unknown-unknown
wasm-bindgen --target nodejs --out-dir tests/node/pkg \
  target/wasm32-unknown-unknown/release/zipp_wasm.wasm
node tests/node/run-boundary-suite.cjs          # every required check, in order
node tests/node/host-contract.cjs                # or any one of them
SOFTN_REPO=../softn.com node tests/node/softn-snakegame.cjs
```

`run-boundary-suite.cjs` is the one list CI, the release workflow and the
security workflow all run, so the lanes cannot drift apart; `--list` prints it.

They all expect the generated glue at `tests/node/pkg/` (adjust the `require` at the
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
- **syntax-corpus.cjs** — the hardened profile's parse-shape limits, checked
  against the artifact they are calibrated for. Parses the reduced real
  application sources in `tests/syntax-corpus/` (they must be accepted) and a
  set of deliberately over-deep shapes (they must come back as a SyntaxError
  with the instance still usable, never as a linear-memory trap). v0.0.1 shipped
  limits that rejected two working applications; the only browser check in the
  release workflow at the time parsed a three-token source.
- **audit-defaults.cjs** — the host-boundary defaults the 6 September 2026
  audit found wanting (accelerator spec grammar, the profile, pre-init
  instruction budgets, the clock receiver, prototype-named events, request
  normalization before registration).
- **audit-2026-09-11.cjs** — the 11 September 2026 audit's contracts against the
  artifact: the guest's `"use strict"` surviving the preamble, the transactional
  `host.call` drain with rejection-and-settlement, the fingerprint work budget,
  pre-init fingerprint seeds, accelerator spec validation before any pin,
  strict batch-write arity, the `evalInContext` JSON projection, Number-width
  callback ids with cancellation, chronological console output,
  `window.dispatchEvent`, and the profile's provenance and policy fields. The
  audit's own 15-case probe bundle ran red on v0.0.15 for 13 of them.
- **resource-usage.cjs** — `resourceUsage()`/`zippInstanceUsage()` and the
  rich eval: a 200-engine create/run/dispose churn in one instance, reporting
  what the instance retains next to the process RSS delta (ZIPP-06 stage 1).
- **profile-matches-readme.cjs** — holds the README's resource table to the
  figures `zippProfile()` reports.
- **softn-snakegame.cjs** — a real SoftN bundle's `.logic`, unmodified, driven the
  way its runtime drives it: `_init`, listener registration, arrow keys through
  `window.__snakeNextDir`, the tick loop, eating, game over, and a high score
  surviving a reload. The load-bearing assertion is the read-spread-write of
  `window` followed by a key dispatch that still reaches its handler — the shape
  that silently unregistered every listener before `host_in_over` existed.
