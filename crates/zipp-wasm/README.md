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

## Resource limits

Every `Engine` has fixed fail-closed ceilings. They are intentionally lifetime
limits rather than per-entry allowances, so repeatedly re-entering one VM cannot
reset its meter:

| Resource | Limit |
| --- | ---: |
| Initial guest source | 2,097,152 UTF-8 bytes, checked before preamble concatenation or compilation |
| One `evalInContext` expression | 65,490 UTF-8 bytes (plus its fixed 46-byte host wrapper) |
| Retained `evalInContext` wrapper source | 1,048,576 UTF-8 bytes total and 256 calls per engine |
| All runtime compilation (`eval`, `Function`, `ShadowRealm`, and host eval) | 65,536 UTF-8 bytes per complete source, 1,048,576 source bytes and 256 attempts total; at most 4,096 retained function definitions and 1,024 retained class definitions |
| VM execution | 50,000,000 bytecode instructions total, starting at guest top-level execution |
| Approximate VM heap | 134,217,728 bytes |
| Lifetime console output | 98,304 UTF-8 bytes total, including newlines |
| Synchronous host bridge | 64-byte kind, at most 16 arguments, 1,048,576 combined kind/argument bytes, and a 1,048,576-byte serialized reply |

The instruction, dynamic-compilation and output counters are not credited when
an entry returns or when `takeOutput()` drains buffered lines. Dynamic source and
attempt counts are charged before parsing, so syntax errors consume the lifetime
allowance too. Successful runtime compilations need stable function/class
definitions; the concrete definition caps are checked before those allocations
are retained. The current stable-address allocations deliberately survive
`Engine.dispose()`, so the caps strictly bound one Engine's contribution, not the
sum from repeatedly constructing Engines in one WASM instance. Tear down and
recreate the dedicated Worker/WASM instance to reclaim them between untrusted
tenants; do not treat `dispose()` as compiler-allocation reclamation.

The heap figure is the VM object-table high-water estimate, checked periodically
during execution and after host writes/calls; it can overshoot by a bounded amount
and does not count every string/array payload or represent exact WASM memory/RSS.
The retained-source limits are conservative source-size proxies for compilations
the VM must keep alive, not exact measurements of compiler allocations.

An initialization failure, an initial/eval source-growth violation, or any VM
instruction/abort/heap/output/dynamic-compilation limit violation disposes the
engine and drops its guest state and bridge handles. This stays terminal even if
guest code catches the resulting error or a promise turns it into a rejection;
the host reads a typed recorder status rather than matching guest-controlled
exception text. A synchronous bridge envelope violation is a guest-visible
`RangeError` raised before the bridge method is called (or before an oversized
reply enters the VM). Ordinary script throws and host-value marshalling errors
remain recoverable.

## Notes

- `Engine::new()` must not be constructed before the module's `start` function has
  run — wasm32 has no clock, `Vm::new` reads one, and an un-shimmed VM traps on
  construction. `wasm-bindgen(start)` handles this; nothing else should.
- `callFunction` resolves by slot. `evalInContext` compiles fresh on every call and
  installs stable-address definitions, so it is for one-off host
  queries, never a per-frame path; the per-source, retained-source and call-count
  ceilings above enforce that boundary. Guest `eval`, `Function` constructors and
  `ShadowRealm.evaluate` share the VM-wide runtime-compilation ceilings rather
  than bypassing the host helper's counters.
- The engine is interpreter-only here: the x86-64 and ARM64 JIT tiers emit
  native machine code and have no meaning on wasm.
- Engine entry points are synchronous and this API has no wall-time preemption. Run
  untrusted WASM/script execution in a dedicated Web Worker and terminate the
  Worker when its deadline expires; a timer on the blocked Worker or main thread
  cannot interrupt an in-progress engine call. A native builtin, regex operation,
  JSON conversion, or host bridge implementation can perform substantial work as
  one VM instruction, so the instruction and approximate-heap meters are not an
  OS sandbox, an exact RSS cap, or a substitute for Worker termination.
