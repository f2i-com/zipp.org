# zipp-wasm

A persistent zipp VM for browser hosts, over `wasm-bindgen`.

`zipp js file.js` runs a script and exits. A UI runtime needs the opposite: compile
once, then read and write the script's top-level bindings, call its functions, and
deliver events — for as long as the page lives. That is what `Engine` is.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --locked
cargo build --release --target wasm32-unknown-unknown -p zipp-wasm
wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/zipp_wasm.wasm
wasm-opt -Oz --strip-debug -o pkg/zipp_wasm_bg.wasm pkg/zipp_wasm_bg.wasm   # optional
```

The crate builds for any target (so a workspace-wide `cargo test` works), but only
does anything on wasm32.

## The two host channels

Everything the script can reach outside itself is defined in `src/preamble.js` as
ordinary JavaScript. It reaches the host through exactly two channels, and the
split is not stylistic:

- **`__zippHostCall(kind, ...args)` — synchronous.** Strings in, one string out;
  anything structured crosses as JSON. `db` and `localStorage` use it, because
  scripts call `db.query(...)` in the middle of an expression and cannot await.
  A synchronous bridge therefore cannot do IO.
- **A queue drained by `drainPendingHostCalls()` — asynchronous.** For
  `host.call(kind, args, cb)`, whose callback the host resolves later with
  `resolveHostCallback(id, result)`. An asynchronous bridge cannot be read inline.

The synchronous channel is an exact capability allowlist. The accepted kinds are
`db.query`, `db.get`, `db.create`, `db.update`, `db.delete`, `db.hardDelete`,
`db.startSync`, `db.stopSync`, `db.getSyncStatus`, `db.getSavedSyncRoom`,
`ls.getItem`, `ls.setItem`, `ls.removeItem`, `ls.clear`, `nav.clipboardWrite`, and
`nav.clipboardRead`. An unknown kind is rejected before any bridge property is
looked up or called. Adding a preamble wrapper therefore also requires an explicit
entry in the Rust allowlist.

`host.call` kinds are application-defined requests, not authority granted by the
engine. Treat every drained `kind` and argument as guest-controlled: the host must
apply its own exact allowlist before dispatch and must not turn a kind into a
dynamic property name, URL, command, or unrestricted API path.

## Values

Reads and writes cross as structured data — nested arrays and plain objects — not
JSON text and not `ToString`. Three rules, each because the alternative is worse:

- **Only data crosses.** Functions, classes, `Map`/`Set`/`Date`/`RegExp`, typed
  arrays and proxies read as `null`. A `Value` is a heap *index* whose meaning
  depends on the live VM, so handing one out would hand out a dangling reference
  the moment the collector moves.
- **Writes skip those slots.** Setting a global that currently holds a function is
  a no-op, so a host that reads every global, edits one field and writes them all
  back cannot destroy the script's own functions on the round trip.
- **Cycles read as `null`, depth is capped.** A graph the host cannot represent
  must not become a hang or a stack overflow.

`JSON.stringify` can express none of the three: it *drops* function-valued
properties and *throws* on a cycle.

## Notes

- `Engine::new()` must not be constructed before the module's `start` function has
  run — wasm32 has no clock, `Vm::new` reads one, and an un-shimmed VM traps on
  construction. `wasm-bindgen(start)` handles this; nothing else should.
- `callFunction` resolves by slot. `evalInContext` compiles fresh on every call and
  the compilation is interned for the VM's lifetime, so it is for one-off host
  queries, never a per-frame path.
- The engine is interpreter-only here: the JIT tier is native x86-64 codegen and has
  no meaning on wasm.
- Engine entry points are synchronous and this API has no wall-time preemption. Run
  untrusted WASM/script execution in a dedicated Web Worker and terminate the
  Worker when its deadline expires; a timer on the blocked Worker or main thread
  cannot interrupt an in-progress engine call.
