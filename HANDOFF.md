# Zipp performance handoff

This is the current continuation note. Historical snapshots through B252 are
archived in [`docs/archive/HANDOFF-through-B252.md`](docs/archive/HANDOFF-through-B252.md),
and the B001–B252 experiment ledger is preserved in
[`docs/archive/PERF_LEDGER-B001-B252.md`](docs/archive/PERF_LEDGER-B001-B252.md).

## Current engine baseline

Main is at `8229b3fc`: v0.0.12 plus B263-B273, with the `8229b3fc` canonical
capture (all-30 0.728× Node, 21/30 Node point wins). The tracked production
WebAssembly module was rebuilt from the v0.0.12 engine with B269's regex fix
(B270-B273 landed after it; rebuild the module at the next release): the
landing page ships 5,558,860 bytes raw, 1,812,458 gzip-9, 1,248,649
Brotli-11, SHA-256 `bd8614fe5f3a3b8ef67f4b917cdefebb3fe69afa39a9804a0d3f6b0b6b267126`.

2026-09-05 (B274-B278, from the external WASM audit's handoff): the wasm
interpreter's four cliffs are closed — non-ASCII string indexing was
quadratic (64K-unit sequential scan 4.5 s → 3 ms; a word tokenizer over
mostly-ASCII text 7.6 s → 8 ms), `join`/builder parts paid a full heap walk
on a window blind to the heap's size (`join` × 200 with 300K objects
retained 19.8 s with the 256-call stride, 8.4 s with the 8 MB byte window,
138 ms with the window scaled by slots),
`eval` / `new Function` code ran with no inline caches (a property loop
70% slower than main code, now within 5%), and an array with a named
property lost its dense read path (43%). These are interpreter-only rows
measured with `crates/zipp-wasm/tests/node/bench.cjs`-style interleaved
A/Bs, not the native PGO capture; the tracked module must be rebuilt to
ship them. The audit's captured-call IC was measured and not built (B279).

## 2026-09-06 (B280-B289, from the ZIPP engineering audit)

The audit reviewed `40993c4d` (v0.0.14). Its two P0 findings and six of the
P1/P2 findings are closed in v0.0.15; the rest are recorded below as open.

- **B280 (Z01, P0) — correct call evaluation is the default.** The fused
  `CallMethod` lowering performed its property Get after the arguments were
  in their registers, and the default "primitive-operand" argument class
  admitted property, global, cell and upvalue reads and arithmetic over
  them, so `receiver.m(input.value)` ran the argument's getter before the
  method's. That class is now opt-in (`ZIPP_RELAXED_CALL_ORDER=1`,
  diagnostics and benchmarking only; `ZIPP_STRICT_CALL_ORDER=1` wins when
  both are set). The provable class grew to keep fusion where it is
  actually safe: arithmetic, comparison, concatenation and template forms
  over literals (`provably_primitive`), and array, plain-keyed object and
  closure literals of provable parts, since an allocation runs no user
  code. `tests/call_order_default.rs` runs the audit's probes — getter
  order, coercion that replaces the method, a getter that writes the
  argument's global, proxy trap order, which of two throws is seen, a
  warmed site — under the default, interpreter, forced-JIT, GC-stress,
  strict and both-switches modes in clean child processes, and pins that
  the relaxed switch still reorders (change that when B279's captured-call
  IC lands). `call_reference_order` and `bare_math_op` now clear both
  switches from their child environments and add both as modes.
- **B281 (Z02, P0 before untrusted acceleration) — accel.make forwards only
  the public grammar.** `resolve_accel_spec` refused nothing but a
  non-typed-array `g:` target and forwarded every other entry verbatim, so
  a guest could hand the adapter an `r:address:length:kind` region of its
  own spelling, indistinguishable from an engine-resolved one. Every entry
  is now held to `NAME=g:GLOBAL`, `NAME=c:GLOBAL`, `NAME=a:ID`,
  `NAME=n:NUMBER`, `NAME=t` with identifier names bound once, integral
  ids and finite numbers, before the adapter is invoked;
  `tests/node/audit-defaults.cjs` drives a recording adapter and shows no
  refused spec reaches it. Still open from Z02: opaque per-engine region
  handles instead of raw addresses, and a bounded reference adapter with
  a defined numeric language, allocation and deadline.
- **B282 (Z06, P1) — `setInstructionBudget` before `initScript` governs the
  top level.** The engine records the allowance and attaches it as the
  limit at initialization; a renewal restores the size the host chose.
  NaN and infinities select the default, fractions truncate, zero and
  negatives clamp to one step, a disposed engine answers false. Pinned in
  `audit-defaults.cjs`.
- **B283 (Z07, P1) — the monotonic clock keeps its receiver.** `mono_now`
  called `performance.now` with an undefined receiver; a receiver-strict
  host threw "Illegal invocation", the error was swallowed, and every
  reading came from `Date.now()`. Invoked with the Performance object now;
  the Node test installs a receiver-strict stub and counts its calls.
- **B284 (Z08, P2) — own-key event storage.** `__zEvents` and `__zHostCbs`
  are null-prototype dictionaries keyed by `String(type)`; a non-function
  listener is skipped at dispatch; `constructor`, `toString` and
  `__proto__` are ordinary event names.
- **B285 (Z09, P2) — transactional `host.call`.** Arguments are converted
  before an id is taken or a callback registered, so a throwing
  `toString` retains nothing. Bounded: 4,096 requests queued between
  drains, 65,536 callbacks awaiting a reply (both a `RangeError` to the
  guest), with `__zHostPending` as the count.
- **B286 (Z04, P1) — the artifact describes itself.** `zippProfile()`
  exports engine, version, isolated features, `callOrder: "strict"` and
  every limit the module owns as JSON, read from the constants the engine
  enforces. The README's resource table had four rows from an older build
  (2 MiB / 16 MiB initial source, 128 / 512 MiB heap, 256 MiB / 1 GiB
  linked memory, four codegen units / one with fat LTO) and five more that
  had drifted (dynamic-code retained bytes, attempts and functions;
  lifetime output; sync bridge bytes); it is corrected and held to the
  profile by `tests/node/profile-matches-readme.cjs`.
- **B287 (Z03, P1) — an automatic gate.** `.github/workflows/ci.yml` runs
  on every push and pull request: the engine suite with default features
  (whose semantic regressions run their own mode matrix), the isolated
  host workspace, the advertised feature combinations, and the production
  WebAssembly module under every Node harness. The manual and release
  lanes are unchanged. Not gated: `cargo fmt` (the native workspace is not
  fmt-clean) and clippy (the engine crate does not pass it); both are
  worth a separate change. The gate runs every binary (`--no-fail-fast`)
  and quarantines, by name and with a notice in the log, the x86-64
  register-tier mechanism assertions that fail on the GitHub runner at
  `40993c4d` itself — control runs of the audited commit plus only the
  workflow: 34034702340 (the first binary) and 34042369088 (every binary,
  120-minute cap): three of `bool_home_clobber`'s INT-GPR planning
  assertions, `multi_split`'s four split-receiver mechanism tests,
  `gpr_deopt_shadow`'s census, `split_recv_writethrough`'s three
  write-through mechanism tests (one of which passes on some runs) and
  `sroa_objref_slot`'s four SROA mechanism tests — on bytecode identical
  to the developer machine's (the relaxed switch reproduces `40993c4d`'s
  bytecode for every fixture, and the first two binaries' logged children
  run under it). The planner's expectation there is machine-dependent and
  is a separate investigation; the parity tests in the same binaries run
  and pass on the runner. Nothing else in the 228-binary suite fails
  there, with Node 24 installed for the node-comparison tests.
- **B288 — the ARM64 default-feature build.** `note_reg_kind` named
  `codegen::writes_reg`, which exists only for x86-64, under
  `feature = "jit"` alone, so `cargo test -p zipp-vm` did not compile on an
  aarch64 host (the ARM64 lane is manual and had not caught it). Gated on
  the architecture as well; the register-kind history only serves the
  x86-64 register tiers.

- **B289 — the captured call keeps the builtin lanes.** B280's cost was
  not in the harness rows (below) but in the shapes it does not contain: a
  method call whose argument reads a global, an element or a property, or
  does arithmetic on a local — `arr.push(i % 13)`, `s.charCodeAt(a[i])`,
  `m.get(k + 1)`, the house style of a SoftN `.logic` file and of this
  repository's own JIT fixtures (133 of the 342 fixtures in the 61 x86-64
  mechanism tests change lowering under the default; the relaxed switch
  reproduces `40993c4d`'s bytecode for all 342). Two costs, measured on
  the production wasm build: the captured `GetProp` on an Array or String
  receiver took the generic walk, since the property IC is shape-keyed and
  those receivers have no shape (~70ns over an own-property hit); and
  `CallWithThis` invoked a native through the generic `call_value`, missing
  the fused lowering's inline `push`, `charCodeAt`, DataView and
  name-dispatched builtin lanes (an `arr.push` went from 89ns to 200ns).
  Now: `CallWithThis` carries the member name it was captured from
  (`NO_NAME` for `with`-calls, chains and static blocks; the field is
  semantically inert and the instruction did not grow); the interpreter's
  `captured_intrinsic_lane` serves a captured native through the same
  lanes once the captured Value is proven identical to the live prototype
  intrinsic — the B191 baseline bits for Array/String receivers, the B215
  collection proof (given a bits-returning form) for Map/Set — and any
  other captured Value, including an intrinsic whose prototype slot the
  arguments replaced, is invoked exactly as captured; `proto_intrinsic_read`
  answers the read itself from the same proofs (own-shadow and custom
  [[Prototype]] excluded receiver-side, as `proto_intrinsic_bits` documents
  is the caller's duty); the inline push lane is one helper shared by both
  lowerings. Identity probes against node found three pre-existing defects
  in the name-dispatched paths, all at `40993c4d`: an own `push`/`indexOf`
  on an array instance was ignored by the fused call (`arr.push = fn;
  arr.push(1)` ran the intrinsic), a `class extends Array` instance's own
  `push` was ignored the same way, and a null [[Prototype]]
  (`Object.setPrototypeOf(arr, null)`) still resolved `Array.prototype`
  methods, for arrays (`array_eff_proto`) and exotics
  (`exotic_own_or_proto`) alike. All fixed; `tests/captured_intrinsic_lane.rs`
  pins the contract with three node-checked probe programs (captured push
  and charCodeAt, own shadows, argument-installed and pre-capture
  overrides, a foreign intrinsic under the name, frozen receivers, ropes,
  subclass instances, replaced and null prototypes, accessors, Map/Set) in
  the default, relaxed, interpreter and GC-stress modes.

  The shapes (`crates/zipp-wasm/tests/node/bench-shapes.cjs`, an A/B over
  two package directories, run in both orders), same harness conditions as
  the table below, best of 20, ms per 100,000 calls (the imul/charCodeAt
  rows loop 4,096 elements per call), baseline `40993c4d` → B280 alone →
  B280 + B289:

  | shape | 40993c4d | B280 | B280+B289 |
  |---|---:|---:|---:|
  | `arr.push(g % 13)`, global `g` | 8.9 | 20.2 | 12.8 |
  | `a.push(i % 13)`, local `i` | 6.1 | 17.5 | 10.1 |
  | `src.charCodeAt(starts[ti])` | 16.1 | 26.0 | 21.5 |
  | `e.indexOf(w0 + (i & 3))` | 16.4 | 24.5 | 20.6 |
  | `m.get((i & 63) + 0)`, Map | 17.4 | 25.6 | 21.6 |
  | `Math.imul(h, C)`, global `h` | 37.3 | 41.8 | 45.1 |
  | `ctx.fillRect(px, py, size, i)`, plain function | 16.9 | 15.8 | 15.9 |
  | bare `arr.push` read | 11.6 | 11.6 | 5.6 |
  | bare `s.charCodeAt` read | 9.2 | 9.2 | 5.4 |
  | the same calls over plain locals | unchanged | unchanged | unchanged |

  The remaining gap is the second dispatch the spec-order lowering pays,
  plus proving the intrinsic twice (once for the read, once for the call)
  and the name-keyed memo lookups. Next, in order of value: a per-site
  memo on the captured pair (name → proven bits under the prototype
  version) so both halves are a few loads; the same identity proof for
  TypedArray, DataView and RegExp receivers (`ta.fill(i & 255)` is +6%,
  the DataView getters have a fused lane the captured form cannot reach);
  and the x86-64 register tiers, whose method lanes recognise only the
  fused shape — `multi_split` and `gpr_deopt_shadow`'s logged children now
  opt into `ZIPP_RELAXED_CALL_ORDER=1` to keep studying the fused lanes,
  and the other 31 mechanism binaries whose fixtures change lowering pass
  under the default on the runner, so their lanes either accept the
  captured shape or are pinned by fixtures over locals. `Math.imul` with
  global operands is the one row that lost ground twice: `Math` is a plain
  object, so neither proof applies, and the captured form pays the
  `GetProp` plus a generic native call; a Math-intrinsic identity lane is
  the same shape as the collection one.

**Call-order cost, measured.** `tests/node/bench.cjs`, the production wasm
build (interpreter-only, fat LTO), Node 22.15 / V8 12.4 on a Snapdragon X
Plus (win32-arm64), baseline `40993c4d` against this tree, best of two
order-balanced runs, ms:

| row | 40993c4d | B280 | delta |
|---|---:|---:|---:|
| arith-int | 18.65 | 18.82 | +0.9% |
| arith-mod | 25.94 | 26.17 | +0.9% |
| arith-float | 30.51 | 31.23 | +2.4% |
| prop-mono | 10.45 | 10.57 | +1.1% |
| prop-poly | 9.74 | 9.73 | -0.1% |
| alloc-object | 16.08 | 16.47 | +2.4% |
| array-build | 11.39 | 11.38 | 0.0% |
| array-hof | 5.51 | 5.52 | +0.2% |
| string-build | 4.59 | 4.49 | -2.2% |
| regex | 274.79 | 274.60 | -0.1% |
| json | 12.36 | 12.39 | +0.3% |

Within this machine's noise on every row (the harness reports 1-6% spread
on the host rows). That is expected rather than reassuring: the harness's
method calls take literals and register-resident locals, which stay fused.
The shape that now takes the captured path is a method call whose argument
reads a property, a global or a captured variable, or does arithmetic on a
local — `s.charCodeAt(i + 1)`, `out.push(a[i])`, `ctx.fill(x * 2)` — and no
harness row is built from those. The next measurement should be: a row of
exactly those shapes, A/B against `ZIPP_RELAXED_CALL_ORDER=1` on the native
CLI (the wasm build has no environment), to size what B279's captured-call
IC would recover.

Verified on an aarch64 Windows host (the first time the default-feature
suite has compiled there, see B288): 1,279 engine tests pass; the
call-order, captured-argument, bare-Math and register-class suites pass in
every mode; the wasm crate's host tests and every Node harness pass on a
fresh production build. Failures on that host, each re-run under
`ZIPP_RELAXED_CALL_ORDER=1` to prove it is the host and not B280:
`accessor_ic_way`'s gate tests, `regexp_call_direct`,
`regexp_string_call_direct` and `int32_trunc_add` assert x86-64 JIT lanes
the ARM64 baseline tier declines; `typedarray_interp_index_fast` compares
against a Node 22 that lacks `Float16Array`; `nursery_minor`'s sustained-
garbage test runs past twenty CPU-minutes in a debug build there; and
Windows refuses to start `clock_installed`, `promise_pristine_dispatch`,
`regexp_dispatch_arm` and `tier_a_call_setup` without elevation, because
their file names look like installers (os error 740). The x86-64 lane in
`ci.yml` is the authoritative run.

Open from the audit, with what a decision would need: **Z05** compiled-code
lifetime (retained-code accounting first, then container-owned programs);
**Z10** a fresh production-WASM coverage capture with per-case outcomes;
**Z11** the four profiling experiments (IC site indexing, numeric host-call
transport, GC scratch reuse, heap-accounting chokepoints); **Z12** browser
acceptance in real Workers, a versioned host SDK, a machine-checkable
opcode contract; and the rest of Z02 above.

## v0.0.6 native interpreter / QuickJS-NG confirmation

The clean default-feature release binary at `e3acee352074` reran the current
real13 suite for six counterbalanced rounds with `ZIPP_NOJIT=1`. All 39
canonicalized validation outputs matched after QuickJS CRLF-to-LF normalization
and all 234 measured launch pairs completed. Zipp / QuickJS-NG v0.16.2 was
`0.6089665×` cold [0.6072021, 0.6122180] and `0.6058409×` startup-adjusted
[0.6041440, 0.6090422], with 13 / 13 point wins; intervals are descriptive 95%.
Raw evidence is
`target/comparison/results/native-real13-v006-e3acee352074-clean-6.json`
(SHA-256 `38915c58…2fe5`). This is native CLI evidence, not WASM evidence.

## v0.0.6 same-source WASM / QuickJS-NG diagnostic

The clean six-round capture is
`target/comparison/results/wasm-suites-v006-c77829269703-final-6.json`. It uses
the exact, unscaled current v0.0.6 source bytes used by the v0.0.6 Node/Bun/Deno
reruns, validates against Node output, and records empty source, artifact,
harness, and Git drift. Capture integrity passed, but `capture_usable:false`
and `evidence_usable:false` reflect four unexpected Zipp validation errors and
their skipped timed rows. Publication also failed because the cross-engine
aggregate is incomplete.

- Two hostile module rows cannot run through the production Zipp WASM API,
  which is built without a filesystem loader.
- The official QuickJS-NG v0.16.2 reactor cannot drain pending jobs for the
  three async rows.
- Zipp validated 7 / 28 scripts. Fourteen rows reached its fixed 50-million
  instruction budget, three reached its fixed 128 MiB approximate heap budget,
  and four ended in other errors: an async trap, two invalid-string-length
  errors, and one typed-array-length error.

There are no comparable normal-suite rows. Only five hostile rows produced
complete samples for both engines:

| Hostile row | Zipp / QuickJS-NG persistent | adjusted |
|---|---:|---:|
| shapes-stable | `1.2860×` | `1.2811×` |
| allocation-ephemeral | `1.0507×` | `1.0482×` |
| allocation-survival | `1.0777×` | `1.0732×` |
| reactish-reconcile | `1.1755×` | `1.1694×` |
| warm-router | `0.4773×` | `0.4755×` |

The available five-row geomeans are `0.9604×` persistent and `0.9567×`
adjusted, with Zipp ahead on only one row. They are not complete hostile or
combined-suite aggregates and must not be presented as such. The separately
sampled compile / instantiation medians were 5.07995 / 0.39700 ms for Zipp and
1.79525 / 1.67570 ms for QuickJS-NG. Their unpaired sums, 5.47695 / 3.47095 ms,
are not measured end-to-end medians. The direct result does not establish that
Zipp WASM is faster than QuickJS-NG WASM.

### Specialization-sensitive micro diagnostic

The earlier five-workload speed-kernel experiment remains attribution evidence.
Its persistent geomean was `0.0954663913×` QuickJS-NG, but three adjusted rows
were at the subtraction noise floor. More importantly, disabling the exact
workload lanes measured `1.815×` QuickJS-NG persistent. Treat the apparent
10.5-fold result as adapter-plus-specialization behavior, not general
interpreter throughput. The older pre-kernel baseline measured
`1.8115847493×` persistent and `1.7725499353×` adjusted.

That exact-lanes-off candidate measured `0.1992206×` Boa persistent and
`0.1922208×` adjusted with five point wins, but it records a dirty heap source
and candidate module SHA `09c0772f…a6bc`; it is not release evidence. No Boa run
exists for the current exact normal-13-plus-hostile-17 WASM inventory.

QuickJS-NG also retains the payload-size lead: its reactor is 1,528,293 bytes
raw and 417,087 Brotli-11, making Zipp `3.586×` as large raw and `2.958×` as
large at Brotli-11. Exact commands, artifacts, and interface caveats are in
[`bench/comparison/README.md`](bench/comparison/README.md).

## Historical v0.0.5 QuickJS-NG / Boa release diagnostic

All source output matched before timing. Six complete counterbalanced rounds and
10,000 bootstrap samples produced:

| Diagnostic | Zipp / competitor | 95% interval | point wins |
|---|---:|---:|---:|
| native interpreter real13 / QuickJS-NG | **0.6413×** | [0.6386, 0.6452] | 12 / 13 |
| native interpreter micro5 / QuickJS-NG | **0.8556×** | [0.8405, 0.8761] | 5 / 5 |
| native interpreter micro5 / Boa | **0.2539×** | [0.2501, 0.2590] | 5 / 5 |
| WASM adjusted execution / QuickJS-NG | **2.1074×** | descriptive | 0 / 5 |
| WASM adjusted execution / Boa | **0.2274×** | descriptive | 5 / 5 |

The native sparse-array point median was the only Zipp loss at 1.0099×;
the aggregate was never an every-row claim. Historical raw files remain under
`target/comparison/results/`: `native-real13-v005-qjsng-clean-6.json`,
`native-micro5-v005-clean-6.json`, and `wasm-v005-clean-6.json`.

### Historical v0.0.5 release validation

Focused release gates were green: safe-sandbox library 463 passed / 1 ignored,
benchmark-tool tests 164 passed / 2 skipped, bool-home 11 passed / 1 worker
ignored, split-receiver 7 / 7, isolated WASM Rust host 4 / 4, Node host contract
137 / 137, and syntax corpus 23 / 23. These counts describe v0.0.5, not the
v0.0.6 release gate.

## Canonical public state

The separate Node/Bun/Deno PGO publication series below remains the current
canonical public capture; the v0.0.5 ecosystem comparison above does not
silently replace it.

The current raw captures are:

- `bench/real13_8229b3fc_pgo_2026-09-02.json`
- `bench/hostile/head_clean_8229b3fc_pgo_2026-09-02.json`

Both use Node v24.12.0, Bun 1.3.14, Deno 2.6.10, and Zipp 0.0.11. Both report
`publishable:true`, `ALL_CORRECT=1`, 15 complete counterbalanced repetitions,
10,000 bootstrap samples, and empty provenance, publication, correctness,
health, source-drift, engine-drift, input-drift, and harness-drift failures.

| Corpus | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| retained ten | **0.886×** [0.880, 0.892] | 0.757× [0.752, 0.764] | 0.770× [0.761, 0.778] |
| diagnostics three | **0.189×** [0.186, 0.192] | 0.171× [0.169, 0.175] | 0.149× [0.146, 0.153] |
| normal all 13 | **0.620×** [0.616, 0.624] | 0.537× [0.534, 0.542] | 0.527× [0.521, 0.532] |
| hostile all 17 | **0.824×** [0.800, 0.838] | 0.657× [0.648, 0.666] | 0.429× [0.422, 0.436] |
| hostile category-balanced | **0.860×** [0.834, 0.873] | 0.676× [0.663, 0.683] | 0.442× [0.435, 0.449] |
| all 30, equal row weight | **0.729×** [0.716, 0.736] | 0.602× [0.597, 0.607] | 0.469× [0.464, 0.474] |

The all-30 point is
`exp((13 × ln(G13) + 17 × ln(G17)) / 30)`. Its 10,000-sample descriptive
bootstrap shares resampled repetition indices within a suite and resamples the
normal and hostile captures independently. It is not a hypothesis test and does
not estimate machine-to-machine variability.

Normal has 33 / 39 point and 29 / 39 Bonferroni exact-sign wins across all
competitors; hostile has 39 / 51 point and 34 / 51 exact-sign wins. Against
Node alone, Zipp has 21 / 30 point wins. The literal all-row target remains
false.

## Current Node gaps

Canonical capture `8229b3fc` (2026-09-02); Zipp / Node paired cold medians.

| Row | Zipp / Node | Descriptive 95% interval |
|---|---:|---:|
| reactish-reconcile | **1.578×** | [1.490, 1.608] |
| allocation-survival | **1.559×** | [1.500, 1.594] |
| warm-router | **1.520×** | [1.503, 1.592] |
| shapes-megamorphic | **1.245×** | [1.204, 1.260] |
| shapes-stable | **1.241×** | [1.226, 1.294] |
| async-promise-chain | **1.118×** | [1.108, 1.132] |
| calls-closures | **1.117×** | [1.068, 1.137] |
| async-lived | **1.005×** | [0.969, 1.039] |
| json-large | **1.005×** | [0.988, 1.034] |

json-large and async-lived are point gaps whose intervals cross parity;
bytecode-vm (0.978×) and npm-nanoid (0.975×) are point wins. All-30 node point
wins: 21/30 (normal 11/13, hostile 10/17); all-30 equal-row geomean 0.728× Node
[0.723, 0.730]. Relative to `c28781cf`: warm-router 1.563 → 1.520, async-lived
1.065 → 1.005, json-large 1.022 → 1.005; allocation-survival 1.484 → 1.559 and
shapes-stable 1.199 → 1.241 moved the other way with no covering mechanism (the
one-binary latches of B270-B273 on the PGO binary are neutral or positive on
both rows), i.e. PGO-profile and layout variation of the size the intervals show.

## Verification completed

- `cargo test -p zipp-vm --lib` plus the compiler and tier suites
  (`reg_classes`, `jit_tier_parity`, `jit_tier_fuzz`, `typeof_alias`,
  `int_split`, `int_gpr_homes`, `int_splice`, `shell_cell`, `int32_trunc_add`,
  `json_plain_key`, `combinator_job_order`, `real_program_corpus`,
  `instr_uses_exhaustive`, `double_mod`)
- `cargo check -p zipp-vm --no-default-features`,
  `--no-default-features --features safe-sandbox`, and the sandbox and wasm
  workspaces
- a 188-run four-mode output identity (default, `ZIPP_NOJIT=1`,
  `ZIPP_JIT_THRESHOLD=1`, `ZIPP_NO_NURSERY=1`) against node over every bench
  and syntax-corpus program
- clean provenance-stamped PGO build from committed source
- complete normal and hostile Node/Bun/Deno/Zipp captures with exact output

## Highest-value next work

0. **Handler-op bodies on the emitted lanes.** B272 gives them a
   frame-backed entry through the generic helper (reactish ~4-5%); two
   follow-ups remain: an exception-edge-aware may-read-before-write pass so
   the masked fast fill applies (today every such call pays the full
   zero-fill `resize`, deliberately — see B272), and a frame-pushing variant
   of the CROSS3 lane for
   the recursive `diff`-shaped site, whose per-call helper cost (~20 ns) is
   now the dominant remaining call overhead on that row.
1a. **Young arena for planned literals (the structural lever).** The churn
   probes put a number on the pipeline: `scratchpad/examples/churn.js`
   (literal dies at once) 29.5 ns per object, `churn_keep.js` (literal stored
   into a 1024-slot ring) 40 ns after B273 — Node 3.5 / 5.3 ns. The PC
   profile of the store shape splits it as birth ~16 ns (`alloc_finalized` +
   `refit_finalized_inner` + `finalized_from_store_inner` +
   `jit_finalize_object_thin` + `reuse_slot_stamp`), death ~9 ns
   (`free_slot`: mirror read+clear, the 80-byte `objs` tombstone move, pool
   push, free push), allocator ~3 ns, barrier ~3 ns. Every remaining gap row
   spends 25-35% here (shapes-stable: `free_slot` 10.1%, `alloc_finalized`
   7.4%, refit 4.9%). Micro-tuning is exhausted (B185-B268); the design that
   removes the work: allocate plain finalized literals (Planned keys, slab or
   short vals, no index tables — the pool's admission set) from a BUMP arena
   instead of a pooled `Box`, hold the key plan alive from the `FuncProto`
   rather than a per-object `Arc` so a dead arena object needs no drop, and
   at each minor copy the FEW survivors out to a `Box` (identity is the slot
   index, so moving the payload only means rewriting `objs[idx]` and
   re-settling that slot's hot mirror), then reset the arena: death costs one
   free-list push (the young log filtered by the mark bits IS the free list),
   birth a bump + the field writes. Expected ~20 ns per object, i.e. 10-15%
   on shapes-stable, survival, reactish and router. Risks to plan for: the
   JIT's `hot_mirror.vals` pointer into moved payloads, the courier and
   payload accounting (arena bytes are one block), the sandbox heap ceiling
   (charge the arena as resident), `ObjMap` fields that own heap memory must
   stay on the `Box` path. Start as a latch (`ZIPP_NO_YOUNG_ARENA`) beside
   the pool, prove with `nursery_minor`, `shell_cell`, `thin_literal_alloc`
   and the churn probes, then A/B the four rows.
1. **React reconcile and warm router (`1.574×` / `1.563×`).** The birth/death
   pipeline dominates both (`free_slot`, `refit_finalized_inner`,
   `alloc_finalized`, `alloc_settled`, malloc/free ≈ 19–24% of each row);
   B265's slot resurrection lost, so the next design must remove work from the
   pool refit or the sweep itself, not move it. The router's Map path (about
   21%) is the intrinsic proof plus index lookup plus key hashing — B266
   showed the byte compare is not it.
1b. **Slot-grain remembered set.** B273 dirties small holders and keeps the
   value record for large ones; a `(holder, slot)` record would make large
   overwrite holders exact too (`buf[i % 1e6] = fresh` still floats every
   store). Sound only with every element-moving array builtin (`shift`,
   `unshift`, `splice`, `sort`, `reverse`, `copyWithin`, length shrink)
   invalidating recorded slots — audit `array_ops.rs` for those sites
   first. The probe pair is `scratchpad/examples/churn.js` /
   `churn_keep.js` (29.5 vs 40 ns per object after B273; Node 3.5 / 5.3).
2. **Allocation survival (`1.484×`).** `free_slot` 12%, `trace_edges` 6%,
   `jit_get_index` 4% (Tier C reads `this.values[i]` through a helper), the
   method-call ICs 5.6%.
3. **Stable and megamorphic shapes (`1.199×` / `1.226×`).** Now the same
   alloc pipeline; B264 removed the store helper.
4. **Async (`1.074×`, `1.065×`).** partA's `p.then(addOne)` loop is declined
   by the call-mix gate (a native-callee site); partB's `await` body is
   interpreted by the async tier gate. A native `then` lane in regions is the
   bounded next step.
5. **json-large (`1.022×`).** Parsed objects miss the object pool (a fresh
   `Box` and key `String` per object); the pooled birth is drafted as B268.
5b. **Tier A for cell-captured self-recursion.** Tier A (the leaf-int whole
   function tier) admits the self-call only as `LoadGlobal(name_global)`; a
   pure-int recursive function declared inside a closure now takes the Tier-C
   CROSS3 self arm (B270: 106 ms for IIFE fib(32)) but Tier A's direct
   `call self_entry` would put it at the top-level figure (47 ms). Extend
   `is_self_call`/`can_compile` to an `UpvalGet` whose cell holds the
   running closure, with the entry-time self-binding re-check reading the
   cell instead of the global.
6. **Sandbox RegExp exec overhead.** After B269 a sticky `exec` costs about
   13 µs in the WASM build against 4 µs natively (the spec `split` loop runs
   one per character); the remaining per-exec work is the limited backtracker
   setup, the transient reservation and the lastIndex property round trips.
7. **Interval prover.** Widening after pass 8 re-widens a compare-narrowed
   loop bound inside the body, so `o + 2` keeps its i53 guard; head-only
   widening would free r13/r14 on more INT-GPR regions.
8. **WASM follow-ups from the 2026-09-05 audit.** (a) Uniformly random
   access into a long non-ASCII string is still O(distance to the nearest of
   start/end/memo) after B274; a sparse UTF-16→byte checkpoint table (one
   entry per 32-64 units, built lazily through a `&mut` path with a per-VM
   budget) makes it O(1) if a workload shows up. `indexOf(x, from)` loops
   still convert `from` and the result through `unit_byte_bounds` on the
   `&str` view. (b) The preflight audit stride is proportional (B275);
   dirty-holder accounting at `Heap::get_mut` is the exact successor.
   (c) `embed::compile_script` leaks its `Program` under `safe-sandbox`
   (the `Drop` reclaim is `cfg(not(safe-sandbox))`), so a host that
   creates many `Engine`s in one wasm instance never gets that memory back;
   a safe self-referential holder or a program arena is the fix, and a
   repeated create/dispose test should track allocator live bytes.

## Commands for the next session

```powershell
git status --short --branch
git log -6 --oneline --decorate
cargo build -p zipp-cli --release
& target\release\zipp.exe --version --json
```

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

## Working rules

- Keep implementation, validation, documentation, and cleanup in small commits.
- Prefer a same-binary off-switch A/B for attribution, then compare frozen
  binaries for layout and generalisation.
- Cold time is the publication gate; adjusted time is diagnostic on short rows.
- Preserve exact output and empty health, correctness, and drift failure lists.
- Keep routine artifacts under ignored `target/bench-results/`.
- Never promote a dirty, filtered, non-PGO, or incomplete-engine artifact.
- Record neutral and refuted ideas so they are not repeated.
