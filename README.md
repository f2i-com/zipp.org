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

The native CLI offers additional defense-in-depth limits for classic scripts:

```sh
./target/release/zipp sandbox file.js
# Equivalent discoverable spelling:
./target/release/zipp js --sandbox file.js

# Imports are denied by default. Opt in to one canonical filesystem tree:
./target/release/zipp sandbox --allow-imports ./plugins ./plugins/main.js
```

The native runner executes the script in a supervised child with a cleared
environment, closed stdin, a hard wall deadline, an instruction budget, a
payload-aware VM heap high-water ceiling, and a combined stdout/stderr cap.
Blocking `Atomics.wait`, the VM's native JIT, and the regex native JIT are
disabled before untrusted source is parsed.
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
Run `zipp sandbox --help` for the complete option list and defaults.

The sandbox currently has no ES-module entry mode: `zipp sandbox --module` and
`zipp mjs --sandbox` fail closed. Do not use the unbounded compatibility `mjs`
command for hostile input; bundle it as a classic script or add external OS
isolation first.

The native command is language, process, resource, and import containment—not
a kernel/OS sandbox or a memory-safety boundary. Its child retains the invoking
user's identity and the native executable still contains unsafe/JIT machinery
for the ordinary `js` command, even though the child disables JIT execution.
The heap figure is conservative but is not exact process RSS, and native
builtins can do work between instruction polls. Do not use this command as the
sole boundary for arbitrary hostile code: use the Worker/WASM design above, or
add an external restricted account/container and platform filesystem, network,
process, CPU, and memory controls. Making `sandbox` the default spelling would
also be a deliberate CLI compatibility change: existing `js`/`mjs` callers
need an import-policy migration before their unbounded path can be removed.

---

## Where it stands

The historical retained-ten headline and the three architecture diagnostics
are still reported separately so the long-running series remains comparable.
On 2026-08-25 the engine took a deliberate security-hardening turn (sandbox
metering, allocation/iteration ceilings, a hardened allocator build — see
[SECURITY.md](SECURITY.md)); some of that protection is paid for in hot-path
time, and the numbers below are the honest post-hardening state, measured on
the definitive clean PGO capture of `08dbdbc`.

| | |
|---|---|
| **Conformance** | **99.991% of test262** — 95,933 of 95,942 executions on this capture's binary: the six historical expected failures plus three module-code failures that arrived with the security hardening (they reproduce on the pristine hardening commit; ledger B181) — zero from the performance waves, and the waves' full sweeps have twice CAUGHT wrong-answer classes before landing (B181, and B189a's ctor-receiver find below) |
| **Performance** | **all-13 geomean 0.70× Node** [0.695, 0.702]; fastest engine on 6 of 13 rows; retained-ten **0.994×** Node [0.989, 0.999] — the whole interval under parity for the fourth capture running |

### Speed vs Node, Bun and Deno

Cold wall time including process launch, 21 paired runs per row with
deterministically shuffled engine and benchmark order. Bold time = fastest
engine; bold ratio = zipp beats Node. Every output is byte-identical across all
four engines.
Node v24.12.0 · Bun 1.3.14 · Deno 2.6.10 · zipp at `08dbdbc` (PGO build).

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| map-set-heavy | **591ms** | 728ms | 1028ms | 602ms | 1.02× |
| typedarray-math | 202ms | 908ms | 136ms | **134ms** | **0.66×** |
| class-prototype-hot | 298ms | 334ms | 294ms | **237ms** | **0.80×** |
| parse-large-js | 270ms | **229ms** | 253ms | 319ms | 1.18× |
| async-promise-chain | 333ms | 371ms | **329ms** | 381ms | 1.15× |
| json-large | 259ms | **194ms** | 277ms | 311ms | 1.20× |
| markdown-render | 269ms | **215ms** | 279ms | 253ms | **0.94×** |
| regex-log-scan | 454ms | 569ms | **423ms** | 534ms | 1.18× |
| sparse-array | 80ms | 103ms | 94ms | **74ms** | **0.91×** |
| polymorphic-objects | 326ms | 333ms | **304ms** | 338ms | 1.04× |
| **zipp / engine median geomean** | **0.70×** | **0.59×** | **0.68×** | — | |

The paired zipp/Node result across all 13 rows is **0.6976× [0.695, 0.702]**;
the retained-ten headline is **0.9936× [0.989, 0.999]** — under parity with
the whole interval below it for the fourth capture running. Zipp remains
ahead of every engine on the all-row geomean (0.70× Node, 0.59× Bun,
0.68× Deno), while six rows trail Node on this capture (json-large 1.20×,
regex-log-scan 1.18×, parse-large-js 1.18×, async-promise-chain 1.15× —
inside its documented retrain band — polymorphic-objects 1.04×,
map-set-heavy 1.02×). parse-large-js moved 1.22× → 1.18× on the
quick-`.length` prefix (ledger B190a: string/array lengths are deliberately
IC-uncachable, so every read paid the full property-miss helper; one
dedicated helper now answers them). async and map-set sit inside the
series' documented **PGO-retrain variance** band (`d990d73` was a
favourable retrain; the same rows' one-binary mechanism A/Bs were null
across the intervening waves). json/regex remain purely the
hardened-allocator trade (secure-off measures parity; ledger B182).

**Where those five rows went.** The previous capture (`cc0d557`, pre-hardening)
measured 13/13 fastest and retained-ten 0.786×. The hardening commit
intentionally spends time for safety: allocation and iteration ceilings on the
string/array builtins, spec-live (per-element, interleaved-observation) array
stringification, fallible allocation on guest-controlled growth, and a
guarded-allocator CLI build (`mimalloc` secure mode, measured at tens of
percent on allocation-heavy workloads, ~85% of that attributable to the
allocator feature itself). One outright defect in that commit — a full-heap
audit walk on every guarded string append even with no meter attached, which
took markdown-render from 0.3s to 258s+ — was found and fixed (`6e8898d`;
B180 in the ledger records why every relative A/B gate was blind to it).
Recovering the five rows *under the hardened semantics* is the active
campaign; the hardening itself is a deliberate trade and stays.

### Startup

Every row above includes process launch, and zipp launches first by a wide
margin — no snapshot to load:

| zipp | node | deno | bun |
|---|---|---|---|
| **9.2ms** | 30.3ms | 49.0ms | 57.3ms |

A long-running server would amortize that away; a CLI tool would not.

### Architecture diagnostics

Three *diagnostic* benchmarks sit outside the headline ten precisely because
they expose what the ten cannot. They remain a separate row set for historical
comparability; zipp is fastest on all three:

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| sparse-array-v2 | 171ms | 367ms | 149ms | **98ms** | **0.57×** |
| polymorphic-objects-v2 | 82ms | 91ms | 97ms | **31ms** | **0.38×** |
| property-ic-shapes | 261ms | 157ms | 278ms | **13ms** | **0.05×** |

The wins come from guarded, exact-shape stream and reducer paths plus the new
shape-keyed native ways described below. They are strong evidence for these
workloads, not proof that broad object-model parity is complete.

### Hostile application diagnostics

The retained series is intentionally stable and historically useful, but it is
also unusually friendly to Zipp: mostly top-level `var`, fixed shapes, and hot
paths already covered by exact reducers. The separate 17-case
[`bench/hostile`](bench/hostile/README.md) corpus deliberately attacks those
assumptions with nested functions, `let`/`const`, mutable closures, megamorphic
properties, mixed local types, allocation/GC pressure, exceptions, sustained
async work, modules, a React-shaped kernel, a warm router, and exact vendored
npm source.

Hostile results are never folded into the retained-ten headline. They are the
generalisation gate, and Zipp does **not** yet claim Node parity on that
corpus. The current publishable capture
([`head_clean_08dbdbc_pgo`](bench/hostile/head_clean_08dbdbc_pgo_2026-08-29.json),
zipp at `08dbdbc`, full corpus, 15 counterbalanced repetitions, exact on
all 17 rows) measures **1.0266× cold ordinary geomean** (the series best
remains the previous capture's 1.0218×; the difference is React-shaped
reconciliation's swing to 2.44× — latch-bisected against every new
mechanism and NULL on all of them, so it joins async in the documented
PGO-retrain band). **Vendored NanoID lands at 1.59×** (from 1.74× — the
`Math.random()*k|0` windows now fuse to fully-inline lanes against the
bench's seeded-xorshift override, B205), surviving allocation holds
under 3× (2.97×) and mutable closures at **1.48×** (from 1.65×). The
recent waves: captured-variable cells moved
to a single mirror authority whose fused counter increments emit INLINE
(B201: the closure row's remainder was 2.4M one-FFI-per-iteration
captured-counter writes, now four instructions with the int-tag guard
subsuming TDZ — taken with a recorded ~+0.5% diffuse retained-side
trade), the live cross-entry table (B199: emitted call lanes read the
callee entry through the VM instead of baking it against a fragile
global epoch, so recompiles never strand a lane and compile-order holes
self-heal — surviving allocation −7–9%), the alloc/free tail trims
(B197/B198), the hot mirror record (B195), and the recycle pool family
(B194/B196). Earlier moves stand: the PGO training-coverage fix (B194a),
vendored NanoID at 1.74× (B190a), the module tier demotion gone (B192).

Nine rows beat or sit at Node cold: ephemeral allocation 0.37×, throw/catch
0.40×, modules 0.40×, baseline calls 0.47×, stable numeric locals 0.47×,
async burst 0.72×, bytecode VM 0.92×, and branch control (1.05×) and
sustained async (1.06×) at parity within noise. The remaining gaps:
surviving allocation 2.97× (series best 2.91×), React-shaped
reconciliation 2.44× (retrain band; series best 2.22×), the warm router
2.00×, stable shapes 1.85×, megamorphic shapes 1.76×, vendored NanoID
1.59× (series best), mutable closures 1.48×, and mixed locals 1.49×.

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

```sh
bash tools/pgo.sh                                    # the measured binary
python tools/bench.py --engines node,bun,deno,zipp --reps 21
python tools/bench_hostile.py --reps 15              # separate generalisation gate
```

The harness counterbalances two-engine A/B order exactly for even repetition
counts and within one run for odd counts, and deterministically shuffles engine
and benchmark order for multi-engine captures. It pairs an empty
launch with every full launch, reports paired medians with bootstrap intervals,
and compares output as exact bytes. By default it refuses dirty or non-HEAD
engines. Diagnostic overrides preserve every provenance reason and force
`publishable:false`; source, harness, input, engine, process-health, and output
drift also fail publication closed. For the hostile corpus, publication also
requires the canonical unfiltered manifest, at least 15 repetitions, and at
least 10,000 bootstrap samples; the manifest, both harnesses, and every declared
input must be tracked and clean against `HEAD`. Captured environment values are
restricted to an explicit allowlist of safe numeric/boolean controls;
credentials, unknown keys, paths, and arbitrary runtime values are redacted. A
hostile run with any inherited engine/runtime control is diagnostic-only. Source
content is compared directly with its `HEAD` blob before and after measurement,
so Git index hints cannot hide local corpus or harness edits. A clean artifact
should still be audited before its numbers become a public claim.

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
