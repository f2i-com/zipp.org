# zipp-wasm

A persistent zipp VM for browser hosts, over `wasm-bindgen`.

`zipp js file.js` runs a script and exits. A UI runtime needs the opposite: compile
once, then read and write the script's top-level bindings, call its functions, and
deliver events — for as long as the page lives. That is what `Engine` is.

```sh
rustup +1.92.0 target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version '=0.2.126' --locked
cd crates/zipp-wasm
RUSTFLAGS='-Dwarnings -C link-arg=--max-memory=1073741824 -C link-arg=-zstack-size=1048576' \
  cargo +1.92.0 build --locked --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg \
  --remove-name-section --remove-producers-section \
  target/wasm32-unknown-unknown/release/zipp_wasm.wasm
node tests/node/strip-target-features.cjs \
  pkg/zipp_wasm_bg.wasm pkg/zipp_wasm_bg.stripped.wasm
mv pkg/zipp_wasm_bg.stripped.wasm pkg/zipp_wasm_bg.wasm
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

### Why release uses four codegen units

Codegen-unit count changes both duplicate code and the layout V8 sees, so the
smallest module was not automatically the fastest one. The production setting
was selected from this complete bounded screen; sizes are the final
section-stripped `wasm-bindgen --target web` module:

| release profile | raw | gzip-9 | brotli-11 | observed steady time vs previous default | decision |
| --- | ---: | ---: | ---: | ---: | --- |
| previous default: 16 CGUs, no LTO | 5,671,347 | 1,870,840 | 1,262,145 | baseline | control |
| 8 CGUs, no LTO | 5,618,617 | 1,862,076 | 1,259,931 | +0.87% | reject: slower aggregate; array rows regressed |
| **4 CGUs, no LTO** | **5,568,906** | **1,845,830** | **1,245,797** | **-0.95%** | **ship** |
| 1 CGU, no LTO | 5,285,536 | 1,789,022 | 1,222,240 | +2.01% | reject: slower aggregate |
| 1 CGU + ThinLTO | 5,284,887 | 1,789,214 | 1,223,108 | not timed | reject: only 649 raw bytes below CGU1, larger on the wire |
| 1 CGU + fat LTO | 5,284,887 | 1,789,320 | 1,223,314 | not timed | reject: only 649 raw bytes below CGU1, larger on the wire |

These are a historical, reproducible build-policy comparison, not a claim that
every browser workload gets 0.95% faster. All candidates used commit
`d71168a9fba3c4b97a05aaacbf14cf046dc65d38`, rustc 1.92.0, locked dependencies,
and `wasm-bindgen` 0.2.126 with the name and producers sections removed. gzip was
level 9; Brotli was quality 11. The accepted CGU4 and rejected CGU8 builds were
timed together under Node 24.12 / V8 13.6 over 11 successful steady-state rows:
one warm round, then six measured rounds of 13 samples, for 78 paired samples
per row, with all six module execution orders balanced. The table reports the
equal-row geomean; no aggregate confidence interval was computed. CGU4's three
largest observed slower rows were +3.76%, +2.84%, and +2.76%, and each per-row
95% interval crossed zero. CGU1 used a separate quiet two-way run with the same
warm-round and measured-sample counts and counterbalanced order. LTO was already
dominated on transfer size and was not given a throughput claim.

The historical v0.0.5 landing-page module was rebuilt from the selected profile
at engine commit `7cb72106c9591613b170ba057d3c07e1cee01379`. It was 5,595,833
bytes raw, 1,859,668 at gzip-9, and 1,254,075 at Brotli-11, with SHA-256
`f3d67856f5853c235c12ee62a1cc86032492012e3942c032a08d8d22df85ff0b`.

The current v0.0.12 production artifact uses the same pinned Rust 1.92.0,
wasm-bindgen 0.2.126, `opt-level=3`, and four-codegen-unit policy. It also keeps
the 1 GiB linked memory maximum, 1 MiB linked stack, and isolated
`safe-sandbox`, `meter-only`, `wasm-no-fs-loader`, and `wasm-single-agent`
features. Name, producers, and optional `target_features` sections are removed;
no `wasm-opt` pass is used. The exact tracked module is:

```text
raw         5,558,860 bytes
gzip-9      1,812,458 bytes
Brotli-11   1,248,649 bytes
SHA-256     bd8614fe5f3a3b8ef67f4b917cdefebb3fe69afa39a9804a0d3f6b0b6b267126
```

QuickJS-NG v0.16.2's official reactor is 1,528,293 bytes raw and 417,087 at
Brotli-11. Zipp is therefore `3.586×` as large raw and `2.958×` as large on the
wire. Boa's official module remains larger than Zipp's.

The direct v0.0.6 WASM attempt uses the exact, unscaled current v0.0.6 normal 13
and hostile 17 sources from the v0.0.6 Node/Bun/Deno reruns. It does **not**
establish a full speed ranking. Production Zipp WASM cannot load the two module
rows and validated only 7 / 28 scripts:
seventeen rows reached fixed production instruction or heap ceilings and four
ended in other engine errors. QuickJS-NG's official reactor cannot drain
pending jobs for three async rows. There were no comparable normal-suite rows
and only five comparable hostile rows.

On those five available rows, Zipp / QuickJS-NG was `0.9604×` persistent and
`0.9567×` adjusted, but Zipp led only `warm-router` (1 / 5 point wins). The
capture records `publishable:false` and `evidence_usable:false`; these are
available-row geomeans, not complete-suite aggregates. The separately sampled
compile medians were 5.080 ms for Zipp and 1.795 ms for QuickJS-NG;
instantiation/start medians were 0.397 ms and 1.676 ms. Their sums are not
measured end-to-end module-ready medians.

An older five-workload speed-kernel experiment measured `0.0954663913×`
QuickJS-NG persistent, but disabling its exact workload lanes measured
`1.8150248200×` QuickJS-NG and `0.1992205735×` Boa. The latter was a five-of-five
Zipp point win, but the control used a dirty-tree diagnostic candidate and no
Boa run exists for the current full source inventory. That contrast makes the
boundary explicit: it measures exact artifact specialization and adapter
lifecycle, not general interpreter speed. Native interpreter results likewise
do not predict wasm32 performance. Exact commands, validation failures, and
per-row results are in
[`../../bench/comparison/README.md`](../../bench/comparison/README.md).

### Compression is where the bytes actually are

The tracked v0.0.6 artifact is ~1.23 MB Brotli and ~1.83 MB gzip. Serving it as
gzip therefore costs about 592 KB per cold load — more than every build-level
saving here put together. Confirm what your origin actually sends:

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

The browser artifact additionally selects zipp-vm's internal `meter-only`
profile. It retains step, heap, output, dynamic-code and regex limits but does
not compile the execution-proof trace recorder or cooperative abort polling into
WASM. The browser host enforces the wall-clock deadline by terminating the
Worker instead. This is an artifact profile rather than a generally composable
Cargo capability: `meter-only` deliberately removes trace-control methods even
though it implies `instrument`, so do not combine it into a native embedding
that consumes execution traces or expects an abort flag to be polled.

Two further artifact-internal profiles match this host surface.
`wasm-no-fs-loader` removes filesystem module-resolution machinery that the
browser API cannot configure; explicit loader APIs continue to fail closed.
`wasm-single-agent` removes the embedded test262 worker harness because browser
Workers are the isolation boundary. The implementation retains
SharedArrayBuffer, Atomics, and waiter behavior, but that retention does not
broaden the `safe-sandbox` guest-visible surface. Like `meter-only`, neither
feature belongs in a feature-unified general embedding.

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
  configuration links a 1 GiB maximum on the exported WebAssembly memory, above
  the VM's own 512 MiB accounting limit so exhaustion throws instead of trapping.
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
