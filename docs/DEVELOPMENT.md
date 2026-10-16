# Developing Zipp

How to build, verify and measure a change. The technical reference is
[`REFERENCE.md`](REFERENCE.md); current performance work is on
[`PERF_ROADMAP.md`](PERF_ROADMAP.md); benchmark suites and their rules are in
[`bench/README.md`](../bench/README.md).

## Everyday commands

```powershell
git status --short --branch
git log -6 --oneline --decorate
cargo build -p zipp-cli --release
& target\release\zipp.exe --version --json
```

## Local verification

CI (`ci.yml`) is deliberately the minimum: the VM's unit tests plus the
audit-day regression binaries, the advertised feature builds, the isolated
host workspace and the Node boundary suite against the production wasm32
artifact, plus the landing site and its synced playground copy. Everything else
runs locally before a push (and on demand, or at release, in `security.yml`;
there is no schedule while automatic CI is paused):

```sh
# the full engine suite (about 30 minutes; the quarantine manifest is exact)
cargo test --locked -p zipp-vm --no-fail-fast -- $(python tools/ci_quarantine.py --emit-skips)
# the safe profile and the instrumented embedding contracts
cargo test --locked -p zipp-vm --no-default-features --features safe-sandbox --lib
cargo test --locked -p zipp-vm --features instrument --test audit_20260911_native
# the close audit's deterministic proofs (forced GC; exact work counts; Unicode names)
cargo test --locked -p zipp-vm --test audit_20260911_za_host_roots --test audit_20260911_za_host_work --test audit_20260911_za_names
# the WASM boundary and the browser Workers (from crates/zipp-wasm)
cargo build --locked --release --target wasm32-unknown-unknown   # RUSTFLAGS as in ci.yml
wasm-bindgen --target nodejs --out-dir tests/node/pkg target/wasm32-unknown-unknown/release/zipp_wasm.wasm
node tests/node/run-boundary-suite.cjs
wasm-bindgen --target web --out-dir tests/browser/pkg --remove-name-section --remove-producers-section target/wasm32-unknown-unknown/release/zipp_wasm.wasm
node tests/browser/worker-smoke.mjs --browsers chromium,firefox,webkit
# conformance (the failures must equal tools/test262-expected-failures.txt)
python tools/run_test262.py --t262 <checkout> --expected-failures tools/test262-expected-failures.txt --dump-fails fails.txt --json test262-results.json
# density (idle host)
node tests/node/bench-density.cjs
```

## Measuring a change

Run a focused normal binary A/B:

```powershell
python tools\bench.py `
  --ab target\bench-binaries\old.exe target\bench-binaries\new.exe `
  --ab-env - - `
  --benches <case> `
  --reps 16 `
  --seed 0x5a172026 `
  --json target\bench-results\focused-ab.json `
  --allow-nonhead-engine
```

Run a focused hostile engine diagnostic:

```powershell
python tools\bench_hostile.py `
  --cases <case> `
  --engines node,zipp `
  --zipp target\release\zipp.exe `
  --reps 16 `
  --json target\bench-results\focused-node.json
```

The published interpreter and cross-engine numbers come from the
profile-guided build (`tools/pgo.sh`); judge a change on such a build, and on
more than one row. A change to the main interpreter loop moves its code
layout enough to shift unrelated rows by several percent, so a same-binary
`ZIPP_NO_*` switch attributes a change and a second build confirms it.

## Working rules

- Keep implementation, validation, documentation, and cleanup in small commits.
- Prefer a same-binary off-switch A/B for attribution, then compare frozen
  binaries for layout and generalisation.
- Cold time is the publication gate; adjusted time is diagnostic on short rows.
- Preserve exact output and empty health, correctness, and drift failure lists.
- Keep routine artifacts under ignored `target/bench-results/`; condense a
  superseded capture into [`bench/history`](../bench/history/README.md).
- Never promote a dirty, filtered, non-PGO, or incomplete-engine artifact.
- Record neutral and refuted ideas so they are not repeated.

The dated session log that held these sections until 0.0.25 is archived at
[`archive/HANDOFF-through-0.0.25.md`](archive/HANDOFF-through-0.0.25.md).
