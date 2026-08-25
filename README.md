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
the definitive clean PGO capture of `1b4af1d`.

| | |
|---|---|
| **Conformance** | **99.991% of test262** — 95,933 of 95,942 executions on this capture's binary: the six historical expected failures plus three module-code failures that arrived with the security hardening (they reproduce on the pristine hardening commit; ledger B181) — zero from the performance waves |
| **Performance** | **all-13 geomean 0.70× Node** [0.69, 0.70]; fastest engine on 6 of 13 rows; retained-ten **0.99×** Node |

### Speed vs Node, Bun and Deno

Cold wall time including process launch, 21 paired runs per row with
deterministically shuffled engine and benchmark order. Bold time = fastest
engine; bold ratio = zipp beats Node. Every output is byte-identical across all
four engines.
Node v24.12.0 · Bun 1.3.14 · Deno 2.6.10 · zipp at `1b4af1d` (PGO build).

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| map-set-heavy | **591ms** | 735ms | 1034ms | 611ms | 1.03× |
| typedarray-math | 202ms | 910ms | 138ms | **134ms** | **0.67×** |
| class-prototype-hot | 299ms | 334ms | 293ms | **237ms** | **0.80×** |
| parse-large-js | 271ms | **230ms** | 253ms | 329ms | 1.22× |
| async-promise-chain | 332ms | 369ms | **327ms** | 331ms | **1.00×** |
| json-large | 260ms | **194ms** | 280ms | 323ms | 1.24× |
| markdown-render | 268ms | **211ms** | 277ms | 266ms | **0.99×** |
| regex-log-scan | 450ms | 565ms | **426ms** | 533ms | 1.18× |
| sparse-array | 80ms | 103ms | 95ms | **74ms** | **0.92×** |
| polymorphic-objects | 327ms | 333ms | **302ms** | 335ms | 1.02× |
| **zipp / engine median geomean** | **0.70×** | **0.59×** | **0.68×** | — | |

The paired zipp/Node result across all 13 rows is **0.6960× [0.693, 0.702]**;
the retained-ten headline is **0.9899× [0.985, 0.994]**. Zipp remains ahead of
every engine on the all-row geomean (0.69× Node, 0.59× Bun, 0.68× Deno) on the
strength of the diagnostics and its startup, while five rows currently trail
Node (json-large 1.24×, parse-large-js 1.22×, regex-log-scan 1.18×,
map-set-heavy 1.03×, polymorphic-objects 1.02×).

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
| **9.1ms** | 30.3ms | 49.0ms | 57.5ms |

A long-running server would amortize that away; a CLI tool would not.

### Architecture diagnostics

Three *diagnostic* benchmarks sit outside the headline ten precisely because
they expose what the ten cannot. They remain a separate row set for historical
comparability; zipp is fastest on all three:

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| sparse-array-v2 | 171ms | 369ms | 147ms | **97ms** | **0.57×** |
| polymorphic-objects-v2 | 82ms | 92ms | 97ms | **31ms** | **0.37×** |
| property-ic-shapes | 264ms | 159ms | 278ms | **12ms** | **0.05×** |

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
([`w58_capture_1b4af1d_pgo`](bench/hostile/w58_capture_1b4af1d_pgo_2026-08-26.json),
full corpus, 15 counterbalanced repetitions, exact on all 17 rows) measures
**1.1278× cold ordinary geomean** [1.119, 1.133] — the best full-corpus result recorded, and
it now carries the full security hardening that the earlier 1.28×-era
checkpoints did not.

Nine rows beat or match Node cold: ephemeral allocation 0.37×, modules 0.40×,
throw/catch 0.45×, baseline calls 0.46×, stable numeric locals 0.48×, async
burst 0.66×, bytecode VM 0.95×, branch control 1.00×, sustained async 1.05×.
The remaining gaps are surviving allocation 3.90×, megamorphic shapes 2.63×,
vendored NanoID 2.43×, React-shaped reconciliation 2.34×, stable shapes
2.17×, the warm router 2.15×, mutable closures 1.97×, and mixed locals 1.54×.

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
object build path (allocation bookkeeping), survivor tracing, the closure
per-op floor, and the NanoID string-append lane.

One Tier-C lane is deliberately parked: the wave-54 closure-creation lane is
default-off (B181) after test262 revalidation caught a cross-called body
double-applying an effect when a mid-body miss forced a whole-call replay.
The numbers on this page are measured with the lane off; it returns once the
plan-time cross-entry exclusion for effect-then-deopt bodies lands with a
full gate. Three sloppy-mode module-code test262 failures that reproduce on
the pristine hardening commit (before any performance work) are recorded in
the ledger for their own investigation.

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
