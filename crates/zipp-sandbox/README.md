# zipp-sandbox

`zipp-sandbox` is the hardened native runner for untrusted classic JavaScript.
It is deliberately a separate Cargo workspace and executable from the fast
`zipp` CLI. Its dependency on `zipp-vm` uses `default-features = false` plus
`safe-sandbox`, so Cargo cannot unify the native VM or regex JITs into this
artifact. That feature also makes unsafe code a compile-time error in zipp-vm
and the regex engine.

Release builds enable integer-overflow checks, abort on an internal panic, and
use mimalloc's secure mode. The existing audited supervisor/worker source is
shared directly with `zipp-cli`: it starts a fresh child with a cleared
environment and closed stdin, enforces wall-clock, instruction, heap, dynamic
source, and output limits, sanitizes terminal output, and denies imports by
default.

This is intentionally a repository-built binary (`publish = false`), not a
crates.io package: it shares that supervisor source directly so security fixes
cannot drift between copied implementations.

## Build and run

From the repository root:

```sh
cd crates/zipp-sandbox
cargo build --locked --release
./target/release/zipp-sandbox --help
./target/release/zipp-sandbox ../../file.js
```

On Windows the executable is `target\release\zipp-sandbox.exe`.

Imports require an explicit canonical root:

```sh
./target/release/zipp-sandbox \
  --allow-imports ../../plugins ../../plugins/main.js
```

Run its smoke tests with `cargo test --locked`. To audit the resolved feature
set, `cargo tree -e features` must show `zipp-vm/safe-sandbox` and must not show
`zipp-vm/jit`, `zipp-regress/rx-jit`, `dynasm`, or `dynasmrt`.

## Boundary

This executable is a language/process/resource containment layer, not an OS
sandbox. It still runs with the caller's identity. For hostile code, also use a
restricted account, container, or platform sandbox with filesystem, network,
process, CPU, and memory controls. Keep any enabled import root host-controlled
and read-only. Where native execution is unnecessary, the repository's
`zipp-wasm` dedicated-Worker design provides the stronger memory boundary.
