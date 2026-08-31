# zipp-wasm

A persistent zipp VM for browser hosts, over `wasm-bindgen`.

`zipp js file.js` runs a script and exits. A UI runtime needs the opposite: compile
once, then read and write the script's top-level bindings, call its functions, and
deliver events — for as long as the page lives. That is what `Engine` is.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --locked
cd crates/zipp-wasm
cargo build --locked --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg \
  --remove-name-section --remove-producers-section \
  target/wasm32-unknown-unknown/release/zipp_wasm.wasm
# Verify the final post-processed artifact's memory and host-import surface.
node tests/node/check-wasm-memory.cjs pkg/zipp_wasm_bg.wasm
# Pre-compress. Serve this body with `Content-Encoding: br`; see below.
brotli -q 11 -f -o pkg/zipp_wasm_bg.wasm.br pkg/zipp_wasm_bg.wasm
```

### Why there is no `wasm-opt` step

There used to be a `wasm-opt -Oz --strip-debug` line here, marked optional. It is
worse than nothing on both axes it was supposed to help, measured on this module:

| post-processing | raw | brotli (the wire) |
| --- | --- | --- |
| none | 5,998,514 | 1,337,361 |
| strip sections only | 5,669,892 | **1,261,091** |
| `wasm-opt -O3` | 5,299,317 | 1,280,742 |
| `wasm-opt -Oz` | 5,280,172 | 1,282,981 |

`-Oz` is 390 KB smaller *raw* and **22 KB larger on the wire**. Binaryen's
rewrites trade away the regularity brotli feeds on, and every byte of the real
saving comes from dropping the 329 KB name section — which `wasm-bindgen` does
by itself, without the 90-second pass. `-Oz` also measured **2.04% slower** and
`-O3` **1.39% slower** on a paired counterbalanced benchmark against a
strip-only control (~0.15% noise floor). Do not reintroduce it without
re-measuring both numbers.

Keep the release profile at `opt-level = 3`. `opt-level = "s"` and `"z"` were
measured: `"z"` cuts the wire to 974,657 bytes and makes the interpreter
**1.9x-2.5x slower**, which is not a trade this artifact should take.

### Compression is where the bytes actually are

The artifact is ~1.26 MB brotli and ~1.87 MB gzip. Serving it as gzip therefore
costs ~600 KB per cold load — more than every build-level saving here put
together. Confirm what your origin actually sends:

```sh
curl -sS -o /dev/null -w '%{size_download}\n' \
  -H 'Accept-Encoding: gzip, deflate, br, zstd' https://<host>/wasm/zipp_wasm_bg.wasm
```

If that number is closer to the gzip figure than the brotli one, the origin is
negotiating gzip. Cloudflare in front of an origin passes the origin's encoding
through, and only emits brotli itself when `br` is the *sole* accepted encoding —
which no real browser sends. Serve the pre-compressed `.br` body explicitly.
`Content-Type: application/wasm` must survive that, or `instantiateStreaming`
falls back to the slower buffering path (the generated glue warns when it does).

Give it a `Cache-Control` too: without one the module is revalidated or refetched
per visit. The filename is not content-hashed, so `immutable` needs a hash in the
name first; until then a bounded `max-age` with revalidation is the honest
ceiling.

This crate is deliberately a separate Cargo workspace and lockfile. Cargo
unifies features inside one workspace; isolation prevents the native CLI's JIT
feature from being combined with `safe-sandbox`. It builds for any target (so
`cargo test --locked` here works), but only does anything on wasm32.

## The two host channels

Everything the script can reach outside itself is defined in `src/preamble.js` as
ordinary JavaScript. It reaches the host through exactly two channels, and the
split is not stylistic:

- **`__zippHostCall(kind, ...args)` — synchronous.** Strings in, one string out;
  anything structured crosses as JSON. `db` and `localStorage` use it, because
  scripts call `db.query(...)` in the middle of an expression and cannot await.
  The bridge contract cannot await; its trusted host adapter can still perform
  arbitrary synchronous host work, so keep that adapter small and bounded.
- **A queue drained by `drainPendingHostCalls()` — asynchronous.** For
  `host.call(kind, args, cb)`, whose callback the host resolves later with
  `resolveHostCallback(id, result)`. An asynchronous bridge cannot be read inline.

The synchronous channel is default-deny per `Engine`. Installing a bridge object
does **not** grant authority. Before `initScript`, the host must explicitly set the
exact operations that tenant needs:

```js
const engine = new Engine();
engine.setSyncHostCapabilities(["db.query", "ls.getItem"]);
engine.setDbBridge(tenantScopedDb);
engine.setLocalStorageBridge(tenantScopedStorage);
engine.initScript(source);
```

The recognized names are `db.query`, `db.get`, `db.create`, `db.update`,
`db.delete`, `db.hardDelete`, `db.startSync`, `db.stopSync`, `db.getSyncStatus`,
`db.getSavedSyncRoom`, `ls.getItem`, `ls.setItem`, `ls.removeItem`, `ls.clear`,
`nav.clipboardWrite`, and `nav.clipboardRead`. Unknown names reject the complete
configuration, and all bridge handles and capability grants become immutable as
soon as initialization starts. Every call also has an exact operation-specific
arity. Unknown, unauthorized, wrong-arity, and malformed-JSON calls are rejected
before a bridge method runs.

Clipboard access uses its own `setClipboardBridge` handle; a local-storage bridge
is never reused for it. The handle must be a synchronous host adapter with
`writeText`/`readText` methods, not the browser's Promise-returning Clipboard API
directly. Host exceptions are returned to the guest as an opaque error rather
than exposing backend messages. Operation grants still do not confer tenant or
row-level authorization: each bridge must independently restrict collections,
IDs, storage keys/prefixes, sync rooms, and the authenticated tenant. Bridge
objects are trusted host code: never accept one from a guest, and never
synchronously re-enter the same `Engine` from a bridge method. Prefer a frozen,
null-prototype adapter whose methods are frozen own data properties: method lookup
is dynamic, so getters, proxies, mutable prototypes, and later method replacement
would otherwise remain part of the trusted computing base.

`host.call` kinds are application-defined requests, not authority granted by the
engine. Treat every drained `kind` and argument as guest-controlled: the host must
apply its own exact allowlist before dispatch and must not turn a kind into a
dynamic property name, URL, command, or unrestricted API path. Queue IDs and
callback records are guest-controlled too. Bind every dispatch to an immutable
Worker/tenant generation, cancel or discard outstanding work when that Worker is
terminated, and never route a completion using only a guest-provided ID.

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

Host-to-guest values should be copied plain arrays, plain data objects, and
primitives. Boundary inspection uses exception-catching operations throughout;
if a proxy trap or accessor throws (including a revoked proxy), the call returns a
controlled marshalling error and the `Engine` remains usable. A non-throwing
proxy can still run host JavaScript during inspection, so do not use proxies or
accessors as an authority boundary.

`JSON.stringify` can express none of the three: it *drops* function-valued
properties and *throws* on a cycle.

## Resource limits

Every `Engine` has fixed fail-closed ceilings. Cumulative counters are lifetime
limits rather than per-entry allowances, so repeatedly re-entering one VM cannot
reset them; size and nesting limits apply to each live value or operation:

| Resource | Limit |
| --- | ---: |
| Initial guest source | 2,097,152 UTF-8 bytes, checked before preamble concatenation or compilation |
| One `evalInContext` expression | 65,490 UTF-8 bytes (plus its fixed 46-byte host wrapper) |
| Retained `evalInContext` wrapper source | 1,048,576 UTF-8 bytes total and 256 calls per engine |
| All runtime compilation (`eval`, `Function`, `ShadowRealm`, and host eval) | 65,536 UTF-8 bytes per complete source, 1,048,576 source bytes and 256 attempts total; at most 4,096 retained function definitions and 1,024 retained class definitions |
| Source syntax/compile nesting | 48 active recursive parser entries, 16 links in one iterative operator/member grammar chain, and 32 structural AST levels before recursive compiler/capture walks |
| VM execution | 50,000,000 bytecode instructions total, starting at guest top-level execution |
| Payload-aware VM heap high-water | 134,217,728 bytes |
| WebAssembly linear memory | 268,435,456-byte link-time maximum |
| One materialized guest string | 1,048,576 WTF-8 bytes; concatenation ropes and padding additionally cap their UTF-16-unit growth at 262,144 |
| One regular-expression pattern | 16,384 UTF-8/WTF-8 source bytes; 32 nested groups/Unicode sets and 64 explicit alternatives per disjunction; Unicode-property expansion limited to 32,768 intervals, 4,096 string alternatives, and 65,536 string code points; 4,194,304 retained compiled-program bytes; compile cache limited to 32 entries |
| One BigInt magnitude | 1,048,576 bits (approximately 128 KiB) |
| One eager dense array/result | 131,072 elements; larger spec-visible lengths remain sparse where supported |
| JavaScript call frames | 4,096 active frames |
| Native VM re-entry | 3 simultaneous interpreter entries (the outer run plus at most 2 nested observable callbacks/traps), sized for the 1 MiB Worker stack |
| Proxy/prototype native meta-operation recursion | 32 guest-controlled transparent forwarding/prototype edges |
| Guarded array-like/list-building native loops | 262,144 guest-directed iterations; recursive array flattening is capped at 64 active levels |
| Array `join`/`toString`/`toLocaleString` recursion | 4 active nested arrays; cycles contribute an empty element and deeper acyclic graphs throw `RangeError` |
| JSON parse/stringify nesting | 64 levels, with bounded traversal/output work |
| JSON replacer/object-key snapshots | 8,388,608 private allocation bytes per stringify, including key and container capacities |
| Lifetime console output | 98,304 UTF-8 bytes total, including newlines |
| Synchronous host bridge | 64-byte kind, exact operation-specific arity (and never more than 16 arguments), 1,048,576 combined kind/argument bytes, and a 1,048,576-byte serialized reply |

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

The heap figure counts the VM object table and owned payload capacities, checked
periodically during execution and after host writes/calls. It is conservative,
can overshoot by a bounded amount, and is not exact WASM memory/RSS or complete
compiler/dependency accounting. The separately configured WebAssembly maximum is
the final allocator backstop. The retained-source limits are conservative
source-size proxies for compilations the VM must keep alive, not exact
measurements of compiler allocations.

Initialization performs one preamble-plus-guest compilation. Preamble binding
names come from a compiler-checked static manifest, so filtering host-visible
slots does not retain a second preamble-only `ScriptState` allocation.

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
- The engine is interpreter-only here and selects zipp-vm's `safe-sandbox`
  profile, which rejects unsafe Rust in the VM and its regex engine at compile
  time. The x86-64 and ARM64 JIT tiers emit native machine code and are excluded;
  js-sys's optional `unsafe-eval` constructors are disabled as well.
- Build from this directory (or the repository root) so the checked-in Cargo
  configuration links a 256 MiB maximum on the exported WebAssembly memory.
  Verify that maximum in any post-processed production artifact. This is a
  per-instance limit: the host must separately cap concurrent Workers and
  aggregate origin/process memory.
- Engine entry points are synchronous and this API has no wall-time preemption. Run
  untrusted WASM/script execution in a dedicated Web Worker and terminate the
  Worker when its deadline expires. A timer in that blocked Worker cannot fire:
  the deadline must run in a different responsive context and call
  `Worker.terminate()` rather than asking the guest Worker to cooperate. A native
  builtin, regex operation, JSON conversion, or host bridge implementation can
  perform substantial work as one VM instruction, so the instruction and
  approximate-heap meters are not an OS sandbox, an exact RSS cap, or a
  substitute for Worker termination.
