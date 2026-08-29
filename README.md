# zipp

A clean-sheet JavaScript engine in Rust — a NaN-boxed register VM with
per-call-site inline caches, a mature native x86-64 JIT, and a guarded ARM64
integer baseline JIT. No third-party parser, no third-party runtime: the lexer,
parser, bytecode compiler, interpreter, JIT and GC are all in this repo.

Home: <https://github.com/f2i-com/zipp.org> · Reference: [`DOC.md`](DOC.md) ·
Measurements and refutations: [`PERF_ROADMAP.md`](PERF_ROADMAP.md)

```sh
git clone https://github.com/f2i-com/zipp.org.git zipp && cd zipp
cargo build --release
./target/release/zipp js file.js
./target/release/zipp mjs file.mjs      # ES module entry (top-level await)
```

## Running untrusted scripts

For arbitrary hostile scripts without an OS VM, the supported security boundary
is [`zipp-wasm`](crates/zipp-wasm/README.md), with one WebAssembly instance in a
dedicated Web Worker per tenant. Its separately resolved `safe-sandbox` build
forbids unsafe code in zipp-vm and the regex engine, excludes both native JITs
and guest shared-memory APIs, defaults every synchronous host capability to
deny, and links WebAssembly memory with a 256 MiB hard maximum. A responsive
host context must enforce the wall deadline by terminating the Worker; then
discard and recreate the Worker/WASM instance between tenants. See
[`SECURITY.md`](SECURITY.md) for the complete threat model and deployment
checklist.

For a hardened native interpreter, build the separately resolved
[`zipp-sandbox`](crates/zipp-sandbox/README.md) executable. Its zipp-vm
dependency has default features disabled and `safe-sandbox` enabled, so neither
native JIT can be unified into the artifact and unsafe code is a compile-time
error in both the VM and regex engine. Release builds also retain overflow
checks and use mimalloc's secure mode.

```sh
cargo build --locked --release --manifest-path crates/zipp-sandbox/Cargo.toml
./crates/zipp-sandbox/target/release/zipp-sandbox file.js

# Imports are denied by default. Opt in to one canonical filesystem tree:
./crates/zipp-sandbox/target/release/zipp-sandbox \
  --allow-imports ./plugins ./plugins/main.js
```

The hardened native runner executes the script in a supervised child with a
cleared environment, closed stdin, a hard wall deadline, an instruction budget,
a payload-aware VM heap high-water ceiling, and a combined stdout/stderr cap.
Blocking `Atomics.wait` is disabled before untrusted source is parsed; the VM
and regex native JITs are absent at compile time.
Runtime compilation is also bounded across `eval`, `Function`, and
`ShadowRealm.evaluate`: 64 KiB per source, 1 MiB and 256 attempts over the
child lifetime, with at most 4,096 retained function definitions and 1,024
retained class definitions. Hitting one of these limits is terminal for the
child even when guest code catches the immediate exception.

Module loading is off unless `--allow-imports` is supplied; then every dynamic,
static dependency, typed, deferred, source-phase, and re-export path is
canonicalized and rejected if it escapes that root through `..` or a symlink,
each imported module is capped at 2 MiB before its contents are read, and the
whole confined graph is capped at 256 canonical files, 64 MiB of aggregate
source, with module-loader recursion capped at a depth of 64. Re-reading one
canonical file through eager, typed, deferred, or source-phase imports shares
one high-water charge rather than consuming the budget again.
Keep an enabled import root host-controlled and read-only for the run: path
canonicalization cannot prevent races in an attacker-writable tree, and a hard
link inside the root still names its target as an in-root path.
Forwarded output preserves ordinary Unicode, newlines, and tabs; other terminal
control characters and bidi direction overrides/isolates are replaced with `?`
before reaching the caller's terminal.
Run `zipp-sandbox --help` for the complete option list and defaults.

The sandbox currently has no ES-module entry mode: `zipp-sandbox --module`
fails closed. Do not use the unbounded compatibility `mjs` command for hostile
input; bundle it as a classic script or add external OS isolation first.

The native command is language, process, resource, and import containment—not
a kernel/OS sandbox. Its child retains the invoking user's identity.
The heap figure is conservative but is not exact process RSS, and native
builtins can do work between instruction polls. Do not use this command as the
sole boundary for arbitrary hostile code: use the Worker/WASM design above, or
add an external restricted account/container and platform filesystem, network,
process, CPU, and memory controls.

For compatibility, the fast JIT-enabled CLI still accepts `zipp sandbox` and
`zipp js --sandbox`. Those aliases use the same supervisor and limits but their
executable contains the ordinary CLI's unsafe/JIT machinery; treat them only as
defense in depth and use `zipp-sandbox` when the hardened native profile
matters.

The ordinary `zipp` binary deliberately defaults to mimalloc's high-throughput
mode for trusted workloads. `cargo build --release --features secure-allocator`
enables mimalloc's guarded metadata/randomization in that JIT binary when its
measured cost is acceptable. This opt-in does not replace `zipp-sandbox`: only
the separately resolved runner excludes both JITs and forbids unsafe engine
code at compile time.

---

## Where it stands

The current measurement is the policy-valid PGO capture
[`real13_0bff482_pgo_2026-08-30`](bench/real13_0bff482_pgo_2026-08-30.json) at
`0bff482`. It records `publishable:true`, `ALL_CORRECT=1`, a clean repository
before and after measurement, 15 complete counterbalanced repetitions, and no
engine, source, input, or process-health drift. The retained ten and the three
architecture diagnostics are reported separately so the long-running series
does not silently change meaning.

| | |
|---|---|
| **Conformance** | **99.997% of test262** — 95,939 of 95,942 executions on the release build of the module-cycle-root commit that follows the capture commit `0bff482` (it changes only the module loader): the three remaining rows are `annexB/language/function-code/block-decl-func-skip-arguments.js` (it asserts the ES2017 `arguments` text that ES2018 deleted) and `staging/sm/String/internalUsage.js` ×2 (needs the `de` CLDR locale), the blessed list in `tools/test262-expected-failures.txt`. The two dynamic-import errored-cycle rows and the import-defer async-cycle row are fixed by recording `[[CycleRoot]]` at DFS time (`tests/module_cycle_root.rs`, adversarially reviewed twice). The three module-code rows that arrived with the security hardening (a `<module source>` synthetic specifier was sent through path resolution at link time) and the four cross-realm `Array.from` / `TypedArray.from` rows from `85fca98` (three ShadowRealm-only "the main global object stands in for the realm" interceptions fired for a `$262.createRealm()` child, which has a global object of its own) are fixed in this commit, each behind an adversarial review (ledger B249). The workspace release suite (1,886 tests), the safe-sandbox and standalone-sandbox suites and the tier-differential fuzz slice are green. |
| **Security** | Root, standalone-sandbox, and wasm dependency audits report zero known advisories; the landing package reports zero npm vulnerabilities. The separately resolved `zipp-sandbox` remains no-JIT, secure-allocator, unsafe-forbidden, metered, and resource-bounded. Regex transient limits, realm/call guards, exact deopt replay tests, and fuzz differentials remain enabled. |
| **Performance** | Current all-13 Zipp/Node geomean **0.635×** [0.633, 0.637] — the series best (0.665× at `271647a`, 0.682× at `b2db432`); retained-ten headline **0.918×** [0.914, 0.922] (0.977× at `271647a`); diagnostic-three **0.186×** [0.184, 0.188]. Zipp is the lowest median engine on 7 of 13 rows and starts in **7.3ms**; the literal all-engine/all-row target is still **not met**: 29/39 median comparisons and 29/39 Bonferroni exact-sign comparisons are wins. |

### Speed vs Node, Bun and Deno

Cold wall time including process launch, 15 paired runs per row with
deterministically shuffled engine and benchmark order. Bold time is the lowest
median; bold ratio means the paired Zipp/Node median is below one. Every stdout
result is byte-identical across all four engines.

Node v24.12.0 · Bun 1.3.14 · Deno 2.6.10 · Zipp at `0bff482` (canonical PGO).
The Zipp executable SHA-256 is
`575b66efe492d167389088f38ecf0f97ffede8c444d50f63f4e07b67bf3653e4`.

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| async-promise-chain | **329ms** | 360ms | 353ms | 410ms | 1.24× |
| class-prototype-hot | 296ms | 326ms | 325ms | **221ms** | **0.75×** |
| json-large | 255ms | **185ms** | 304ms | 267ms | 1.04× |
| map-set-heavy | 571ms | 715ms | 1048ms | **534ms** | **0.93×** |
| markdown-render | 266ms | **202ms** | 305ms | 224ms | **0.83×** |
| parse-large-js | 268ms | **224ms** | 281ms | 240ms | **0.90×** |
| polymorphic-objects | 325ms | 325ms | 332ms | **303ms** | **0.93×** |
| regex-log-scan | 457ms | 552ms | **452ms** | 461ms | 1.02× |
| sparse-array | **80ms** | 94ms | 123ms | 80ms | 1.01× |
| typedarray-math | 201ms | 898ms | 167ms | **132ms** | **0.66×** |
| **Zipp / engine paired geomean** | **0.918×** [0.914, 0.922] | **0.792×** [0.788, 0.795] | **0.804×** [0.798, 0.808] | — | |

Across all 13 measured rows, including the three diagnostics below, the paired
Zipp/engine geomeans are **0.635× Node** [0.633, 0.637], **0.556× Bun**
[0.554, 0.563], and **0.546× Deno** [0.541, 0.553]. Zipp has a sub-one paired
median on 9/13 Node comparisons, 9/13 Bun comparisons, and 11/13 Deno
comparisons: **29/39** point wins. The Bonferroni-adjusted exact one-sided sign
gate proves **29/39** at 5% family-wise alpha. Therefore
`FASTER_THAN_EVERY_ENGINE_ON_EVERY_ROW=0`; the remaining gaps are work to do,
not wins hidden by an aggregate.

**What moved since `271647a`.** B248 had restored the fused method call where
an argument cannot observe the property Get, and left three diagnosed
residuals. B249 shows they were one mechanism — the registers the hardening's
captured lowerings leave in a loop body — and closes it: property and element
reads over transparent parts join the order-transparent class (so
`routes.get(request.path)` and `helpers[k](regs[a])` fuse again), the fused
computed call is back, a plain local callee is called from its own register,
captured temporaries are never recycled after their call, and
`Math.<op>(args)` with transparent arguments compiles to a BARE op that
captures nothing and validates the live `Math` global and the live
`Math.<op>` slot at execution instead — the pre-hardening register layout with
the hardening's semantics (a replaced method, a rebound `Math`, an accessor or
a deleted slot are all observed; `tests/bare_math_op.rs` pins each against
node in five child modes). Against `271647a`: regex-log-scan 1.30× →
**1.02×**, json-large 1.21× → **1.04×**, markdown-render 0.99× → **0.83×**,
parse-large-js 0.95× → **0.90×**; the other six rows read within noise of
their previous values. The rows still behind Node are async-promise-chain
(1.24×) and json-large (1.04×); regex-log-scan and sparse-array sit at parity
within their intervals.

### Startup

Every row above includes process launch, and zipp launches first by a wide
margin — no snapshot to load:

| zipp | node | deno | bun |
|---|---|---|---|
| **7.3ms** | 30.8ms | 79.6ms | 40.9ms |

A long-running server would amortize that away; a CLI tool would not.

### Architecture diagnostics

Three *diagnostic* benchmarks sit outside the headline ten precisely because
they expose what the ten cannot. They remain a separate row set for historical
comparability; zipp is fastest on all three:

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| polymorphic-objects-v2 | 81ms | 84ms | 124ms | **23ms** | **0.29×** |
| property-ic-shapes | 259ms | 151ms | 306ms | **10ms** | **0.04×** |
| sparse-array-v2 | 169ms | 361ms | 180ms | **100ms** | **0.59×** |
| **Zipp / engine paired geomean** | **0.186×** [0.184, 0.188] | **0.171×** [0.170, 0.180] | **0.151×** [0.146, 0.157] | — | |

The wins come from guarded exact-shape paths. They are strong evidence for
these workloads, not proof of broad object-model parity; this is why their
0.186× Node geomean is kept out of the retained-ten headline.

### Hostile application diagnostics

The retained series is intentionally stable and historically useful, but it is
also unusually friendly to Zipp: mostly top-level `var`, fixed shapes, and hot
paths already covered by exact reducers. The separate 17-case
[`bench/hostile`](bench/hostile/README.md) corpus deliberately attacks those
assumptions with nested functions, `let`/`const`, mutable closures, megamorphic
properties, mixed local types, allocation/GC pressure, exceptions, sustained
async work, modules, a React-shaped kernel, a warm router, and exact vendored
npm source.

Hostile results are never folded into the retained-ten headline. They are a
separate stress corpus. The current capture
([`head_clean_0bff482_pgo`](bench/hostile/head_clean_0bff482_pgo_2026-08-30.json),
zipp at `0bff482`, full corpus, 15 counterbalanced repetitions, exact on
all 17 rows) measures **0.866× cold ordinary geomean** [0.859, 0.871] — the
series best, under parity for the seventh capture running and ahead of the
0.920× at `b2db432` (0.730× Bun [0.720, 0.733], 0.472× Deno [0.469, 0.477]).
The two B248 residuals are closed: the bytecode VM 3.68× → **0.93×** (its
spliced dispatch loop is back on the integer-register tier — a captured
`Math` pair and a recycled receiver had cost it the one home it was over the
pool) and the warm router 2.18× → **1.62×** (below the 1.69× it read before
the hardening). Every other row reads within noise of `271647a`. The capture
before this one, at `b2db432`, was the first PROFILE-guided one: the
engine gained an instruction-pointer profiler (B237) after three
allocation-side charters had been designed against guesses and measured
null, and its first profile showed the four biggest hostile rows sharing
one object-lifecycle profile — freeing, dropping, allocating and
mirror-settling ~25–30% of each. Two bounded fixes to that path (B238,
B239) took stable shapes 1.66× → **1.53×**, the React-shaped reconciler
1.86× → **1.78×**, megamorphic shapes 1.60× → **1.57×** and the bytecode
interpreter 0.96× → **0.92×**; and the same capture discipline caught a
fixed +1.5ms startup cost the profiler's own linking had introduced, which
no latch could see (B240). Earlier waves: a
25-agent adversarial review of the object-lifecycle campaign confirmed and
fixed seven defects, two of them wrong-answer classes (B207); a one-line
W11-era staleness fix converted 243k fail-closed cross-call window
zero-fills into fast fills (B208); super-free object-literal methods no
longer wire an unobservable [[HomeObject]] — surviving allocation alone
took −8.3% (B209); the B185 free courier went adaptive — per-item size
gate, bulk-sweep override, lazy thread spawn (B210); the const+int concat
memo landed with a frozen-string defence of the engine's two in-place
growth licences, taking surviving allocation −24.9% and mixed locals
−10.4% in one move (B212 — JS strings have no observable identity, so
repeated `"prefix" + i` now serves a version-guarded cached index instead
of allocating); the call planners stopped attempting cross lanes at
callees that can never hold one, which was costing the React-shaped row a
futile enter-helper prefix AND a framed re-dispatch on each of 130k calls
(B213, −12.8%); and a top-level-await module's body stopped running with
GC suspended — it is one microtask, so NanoID collected once in 240k
allocations and a long-lived TLA server grew without bound (B214, −8.0%
and an unbounded-heap hole closed).

Nine rows beat Node cold at `0bff482`: ephemeral allocation 0.34×,
module-hot-graph 0.37×, throw/catch 0.42×, baseline calls 0.45×, stable
numeric locals 0.48×, async burst 0.64×, mixed locals 0.77×, branch control
0.85×, and the **bytecode VM 0.93×** (across from 3.68×), with sustained
async 1.05× at parity within noise.

The gaps: surviving allocation 1.71×, React-shaped reconciliation 1.68×, the
warm router 1.62×, mutable closures 1.46×, megamorphic shapes 1.43×, vendored
NanoID 1.40×, and stable shapes 1.38×.

**Five of those moved at once, and not by a new optimisation.** The
emitted native→native call lane existed already; it was simply not being
reached. Three defects kept it away: a plain MONOMORPHIC call site — one
filled inline-cache way naming one callee, the most predictable shape
there is — was excluded from it while the harder rotating-closure shape
was admitted (B227); a callee wider than 64 registers was refused even
when the compiler's own analysis said none of its registers could be read
before being written, i.e. that there was nothing to fill (B228); and a
caller that had baked a good lane was never re-planned when its callee was
later evicted and recompiled, so the lane guarded false for the rest of
the run (B229). Mixed locals alone fell 13% once those were fixed. The
lane is now worth, on a live latch A/B, 24.8% on surviving allocation,
12.1% on megamorphic shapes and 11.5% on stable shapes — it had been
carrying almost none of that traffic.

**Two live wrong answers were found and fixed this wave, both by perf
work.** Neither came from a bug report: a lever scout attributing
polymorphic-objects noticed that the JIT's loop-invariant `g.length` hoist
asked “is this global written in the region?” with a single forward pass,
so a store standing BEFORE the load was checked before that global was
known and invalidated nothing — `s = "prop_" + p; acc += s.length` summed
1229600 where Node and zipp's own interpreter both say 1230000 (B216). The
five-lens fan-out chartered to hunt that bug's class then found the same
function's other half rotten: its “can anything here run user code?”
pre-scan was a blacklist of four opcodes that had gone stale as region
admission widened, so a computed call or a Proxy `deleteProperty` trap
could mutate the very container being measured — the loudest reading
1999972 against Node's 1300000 (B217). Both are fixed, the second by
inverting the predicate to fail closed, and six node-oracled cases pin
them. The differential fuzzer had no shape that rebinds a
`.length`-consumed global at all, which is why 8000-program soaks never
caught it; the shape is now in the grammar and negative-controlled — with
the bug reintroduced it finds 66 divergences in 2000 programs, and 0 with
it fixed.

**Shape-keyed native ways shipped.** An earlier shape-way experiment (B152)
was reverted after independent review found exotic-shape collisions and
stale-metadata hazards; its raw-pointer design stayed dead. Waves 57–58
rebuilt the idea the way that review prescribed: the guard reads a slot-typed
live mirror the heap maintains under its existing version-bump discipline
(non-object heap slots and every cache-excluded exotic receiver are pinned
unmatchable), reads and writes go through live pointers with the nursery
write barrier preserved, and the fill side reuses the interpreter's shape
memo. The mechanism went through a four-lens adversarial review (one real
finding, fixed before shipping), byte-exactness against Node with the
mechanism on and off, GC-stress and nursery-verify gates, and a
tier-differential fuzzer soak. Measured on the hostile corpus it is worth
−31% on stable shapes and −29% on the warm router
([B178/B179 in `PERF_ROADMAP.md`](PERF_ROADMAP.md)); the earlier attribute
column elision (B177) and the preflight fix (B180) are recorded there with
the same standard of evidence. The next targets, in evidence order: the
cross-call enter/close per-call floor (mutable closures' whole remainder
sits in it), the GC sweep and trace mass, the mixed-locals type
narrowing, and the NanoID string lane.

The wave-54 closure-creation lane, parked when test262 revalidation caught
a cross-called body double-applying an effect, is back on: the two mid-body
deopt edges that forced whole-call replays now COMPLETE through
interpreter-equivalent slow paths (B184), the lane measures −13.7% on the
React-shaped row, and the full-sweep failure list is byte-identical to the
baseline. Three sloppy-mode module-code test262 failures that reproduce on
the pristine hardening commit (before any performance work) are recorded in
the ledger for their own investigation.

**The closure campaign (B189).** New per-reason cross-call decline counters
showed the mutable-closures row's floor was never dispatch: a B50-era
admission rule blacklisted every tiny capturing closure from the native
tier, so eight million calls per run fell back to interpreter frames. The
rule's price assumption was removed on both ends — a captured read is now
three emitted loads through a write-through cell-value mirror, and the
same-prototype cross call itself is emitted (identity-free fid guard, so
sixteen rotating closures of one function stay monomorphic; window and
completion kept in Rust helpers; every baked assumption revalidated per
call). Mutable closures moved 1.93× → 1.63× across the two waves. The
admission widening also EXPOSED a pre-existing wrong-answer class the full
test262 sweep caught before landing: the JIT property walks defaulted a
ctor-map receiver's prototype to `%Object.prototype%`, so a hot
`Function.arguments` read answered `undefined` instead of throwing — fixed
fail-closed and pinned by a regression suite (B189a in the ledger).

---

## Correctness

The engine is gated hardest against **tier divergence**: the JIT disagreeing
with the interpreter. Default JIT, interpreter-only, forced-JIT, and
majors-only-GC Test262 runs produce the same six failure identities, which is
the cheapest evidence a JIT change has not quietly diverged.

That gate is not sufficient on its own, and this repo has the scar tissue to
prove it. For about a month the INT tier returned silently wrong answers for an
ordinary loop over an array of integers — a dense-element read scratched a
register the planner was using — and every benchmark stayed green throughout,
because the thirteen programs here happen to place their equivalent loops on
other tiers.

So there is now an instrument for it. `crates/zipp-vm/tests/jit_tier_fuzz.rs`
generates self-checking programs over the shapes that actually drive tier
selection — loop nesting, guard spelling, how many booleans are live, how many
constants got hoisted, element reads across array kinds, planted deopt triggers
— and runs each one against Node, the interpreter, and every tier-forcing
switch, shrinking any disagreement to a minimal case. A 500-program slice runs
with the normal suite; soaks run to hundreds of thousands.

It found **eight silent wrong-answer classes in its first four waves**, all
live in shipped code, several reachable from ordinary JavaScript:

- `x | 0` inside a float loop destroying a live boolean
- a compiled loop running fewer iterations than the interpreter
- reading past the end of an array throwing instead of yielding `undefined`
- an operand table blind to 185 of 221 opcodes — 23 wrong-answer shapes at once

Every one had been invisible to the benchmarks, to 95,936 test262 executions,
and to a thousand hand-written unit tests. Correctness work of this kind costs
measured performance and is worth it: making that operand table exhaustive cost
+0.30% of geomean and closed all 23 shapes.

**Byte-identical benchmark output means those thirteen programs agree with
Node, and nothing more.**

---

## Language support

ES2015–ES2025 is essentially complete — classes with private fields and static
blocks, generators and async generators, the full `Promise` surface, all 11
TypedArray kinds plus `DataView`, `SharedArrayBuffer` and `Atomics`, `BigInt`,
`Proxy`/`Reflect` with all 13 traps, ES modules with top-level await, `eval`,
`Temporal`, iterator helpers, and the modern `RegExp` surface including
named groups, lookbehind, `/d` indices and `/v` sets.

Decorators work end to end, all eight kinds. Fifteen Temporal calendars are
implemented, twelve of them closed-form.

The gaps are narrow and named: only the `en` CLDR locale ships, module
evaluation errors are not memoised across a cycle, and one top-level-await
ordering case in a deferred-module cycle. See [`DOC.md`](DOC.md) for the full
list and the six remaining test262 failures.

---

## Platforms

Native code generation is feature-gated. x86-64 has the mature function, OSR,
helper, IC, and reducer tiers; ARM64 starts with a deliberately smaller
whole-function tier for call-free integer code and hot numeric loops, with
exact-ip fallback to the interpreter on every unsupported value or overflow.
The ARM tier has bounded executable-code caches and native Linux, Windows, and
macOS CI. wasm32 and other targets build the pure interpreter.
`crates/zipp-wasm` embeds the engine for browser hosts, and `embed::ScriptState`
keeps a VM alive across host calls for runtimes that render, wait, and call
back in ([`DOC.md`](DOC.md#embedding)).

---

## Reproducing the numbers

```powershell
# Run from an x64 Visual Studio Developer PowerShell. Native Git Bash is
# required; PATH's WSL bash is rejected.
& 'C:\Program Files\Git\bin\bash.exe' tools/pgo.sh  # the measured binary
```

```sh
python tools/bench.py --zipp target/x86_64-pc-windows-msvc/release/zipp.exe --engines node,bun,deno,zipp --reps 15 --bootstrap-samples 10000 --json bench/real13_<commit>_pgo_<date>.json
python tools/bench_hostile.py --zipp target/x86_64-pc-windows-msvc/release/zipp.exe --reps 15  # separate generalisation gate
```

The harness counterbalances two-engine A/B order exactly for even repetition
counts and within one run for odd counts, and deterministically shuffles engine
and benchmark order for multi-engine captures. It pairs an empty
launch with every full launch, reports paired medians with descriptive
percentile-bootstrap intervals, and compares stdout as exact bytes. Every
measured process, launcher-resolution probe, and metadata probe gets its own
fresh, isolated home/cache/temp tree, created outside timing and removed after
that process; arbitrary or future ambient runtime variables are not inherited.
By default the harness refuses dirty or non-HEAD engines. Diagnostic overrides
preserve every provenance reason and force `publishable:false`; source, harness, input, engine,
process-health, and output drift also fail publication closed. A publishable
real-suite headline requires the complete Node/Bun/Deno/zipp table, Node as
baseline, cold wall time, the modern report, and a canonical PGO binary. That
measurement first copies every selected program (and every declared hostile
dependency) into a private read-only tree before any engine probe, and every
timed process executes only those staged bytes. The live checkout and the stage
are rechecked afterward, so an editor cannot create a mixed-source artifact by
temporarily replacing a file and restoring it. The canonical PGO binary binds
its profile and structural-similarity-guarded training recipe plus the release
profile, target, JIT/features, exact codegen flags, selected Cargo/rustc/rust-lld
and MSVC `cl.exe`/`lib.exe` driver identities and byte hashes, allowlisted build
environment, and Cargo definition files into a recomputed build-recipe hash.
MSVC backend DLLs, SDK headers, and import libraries are represented by the
validated environment paths, not a byte-complete SDK manifest. Both Cargo
stages build the same private read-only clone of one clean `HEAD`; publication
rechecks the original checkout. Recipe and source-snapshot verification reads
the clean commit's Git blob bytes, so Windows checkout EOL materialization
cannot change the byte domain. The PGO recipe trains only seven deterministic, LF-only
mechanism workloads under `bench/pgo-training`; every tracked JavaScript/module
benchmark outside that directory (including the legacy, long, scope, real, and
hostile sets), plus every manifest-declared non-code hostile input, is excluded
from training and bound by path and byte digest as publication data. Before
training, a fail-closed validator also normalizes
identifiers and literal values and rejects suspicious token-gram containment,
shared token runs, padded fragments, distinctive shared integers and shift
tuples, long cooked strings/regex bodies, and ambiguous source spellings. Training
runs from an exclusive read-only stage with secondary runtime compilation and
module loading disabled, byte-exact expected stdout/stderr, bounded time/output,
and an explicitly hashed one-profile-per-input merge. Profiles and binaries are
atomically published after path and digest checks. These policies, their helper
bytes, and the output manifest are part of the recipe hash. This is an auditable
anti-leakage boundary, not proof of statistical independence or a hostile-code
sandbox.
For the hostile corpus, publication also requires the canonical unfiltered
manifest, at least 15 repetitions, and at least 10,000 bootstrap samples. The
literal “faster than Node, Bun, and Deno on every row” result uses paired
per-competitor ratios plus exact one-sided paired sign tests of whether the
strict per-run win probability exceeds 0.5. Every point ratio must be below
one, and every exact p-value must meet `0.05 / (rows × competitors)`, controlling
family-wise type-I error at 5% by Bonferroni. Bootstrap intervals remain
descriptive and never decide this gate. The entire repository, manifest,
harnesses, and declared inputs must remain clean
against the same `HEAD` before and after measurement. A clean artifact should
still be independently audited before its numbers become a public claim.
These controls address ordinary editor/build races and accidental clobbering;
a malicious process already running as the benchmark account is outside this
boundary and requires a separate build account or isolated host.

Use at least 15 pairs for a change expected under 10%, and 21 for a marginal
decision. A same-binary A/A check once reversed a row from −0.4% to +1.1% while
both nominal intervals excluded zero, so a result around 1% still needs
independent reproduction.

Two cautions worth keeping in mind when reading any table here. Between two
earlier captures `class-prototype-hot` had silently regressed to **7.99×** on
one missing whitelist arm, with byte-identical output the whole time and nothing
in the table to show it. And **all retained-ten benchmarks open with `"use strict"`** — a
change worth 7.7× to sloppy-mode calls landed in this repo and moved the suite
0.1%, because none of the ten could see it.

Full methodology, the drift discipline, and every refuted experiment:
[`PERF_ROADMAP.md`](PERF_ROADMAP.md).

---

## License

Apache-2.0.
