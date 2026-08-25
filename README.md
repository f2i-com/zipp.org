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

For untrusted classic scripts, use the explicit sandbox runner instead of the
compatibility `js` command:

```sh
./target/release/zipp sandbox file.js
# Equivalent discoverable spelling:
./target/release/zipp js --sandbox file.js

# Imports are denied by default. Opt in to one canonical filesystem tree:
./target/release/zipp sandbox --allow-imports ./plugins ./plugins/main.js
```

The sandbox runs the script in a supervised child with a cleared environment,
closed stdin, a hard wall deadline, an instruction budget, an approximate VM
heap ceiling, and a combined stdout/stderr cap. Both the VM's native JIT and
the regex native JIT are disabled before untrusted source is parsed.
Runtime compilation is also bounded across `eval`, `Function`, and
`ShadowRealm.evaluate`: 64 KiB per source, 1 MiB and 256 attempts over the
child lifetime, with at most 4,096 retained function definitions and 1,024
retained class definitions. Hitting one of these limits is terminal for the
child even when guest code catches the immediate exception.

Module loading is off unless `--allow-imports` is supplied; then every dynamic,
static dependency, typed, deferred, source-phase, and re-export path is
canonicalized and rejected if it escapes that root through `..` or a symlink,
each imported module is capped at 16 MiB before its contents are read, and the
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

This is language, process, resource, and import containment—not a kernel/OS
sandbox or a memory-safety boundary. The child still has the invoking user's
OS identity, and the heap meter is approximate (large string/array payloads
are not fully accounted). Strong isolation for hostile code also requires an
external restricted account or container plus platform controls such as
namespaces/seccomp/cgroups on Linux, an AppContainer/restricted token and Job
Object on Windows, or a sandbox profile on macOS, with filesystem and network
access denied independently. Making `sandbox` the default spelling would also
be a deliberate CLI compatibility change: existing `js`/`mjs` callers need an
import-policy migration before their unbounded execution path can be removed.

---

## Where it stands

The historical retained-ten headline and the three architecture diagnostics
are still reported separately so the long-running series remains comparable.
On the definitive clean PGO capture, zipp has the lowest median on **all 13**
rows against Node, Bun, and Deno.

| | |
|---|---|
| **Conformance** | **99.994% of test262** — 95,936 of 95,942 executions, four modes with the same 6 expected failures |
| **Performance** | **13/13 fastest** — all measured **0.5728× Node** [0.5695, 0.5762], retained ten **0.7860×** [0.7820, 0.7905] |

### Speed vs Node, Bun and Deno

Cold wall time including process launch, 21 paired runs per row with
deterministically shuffled engine and benchmark order. Bold time = fastest
engine; bold ratio = zipp beats Node. Every output is byte-identical across all
four engines.
Node v24.12.0 · Bun 1.3.14 · Deno 2.6.10 · zipp at `cc0d557` (PGO build).

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| map-set-heavy | 658ms | 777ms | 1093ms | **479ms** | **0.73×** |
| typedarray-math | 207ms | 915ms | 138ms | **136ms** | **0.66×** |
| class-prototype-hot | 299ms | 340ms | 296ms | **237ms** | **0.79×** |
| parse-large-js | 274ms | 236ms | 254ms | **227ms** | **0.83×** |
| async-promise-chain | 335ms | 377ms | 324ms | **315ms** | **0.94×** |
| json-large | 262ms | 201ms | 282ms | **190ms** | **0.72×** |
| markdown-render | 270ms | 218ms | 283ms | **185ms** | **0.69×** |
| regex-log-scan | 456ms | 568ms | 428ms | **381ms** | **0.84×** |
| sparse-array | 86ms | 108ms | 95ms | **73ms** | **0.86×** |
| polymorphic-objects | 331ms | 338ms | 302ms | **281ms** | **0.85×** |
| **zipp / engine median geomean** | **0.79×** | **0.66×** | **0.78×** | — | |

The paired zipp/Node retained-ten result is **0.7860× [0.7820, 0.7905]**;
across all 13 it is **0.5728× [0.5695, 0.5762]**. By all-row median
geomeans, zipp is 0.5721× Node, 0.4836× Bun, and 0.5687× Deno. Zipp has the
lowest median on every measured row. This is a result for these cold workloads,
not a claim that one benchmark suite establishes general runtime superiority.

### Startup

Every row above includes process launch, and zipp launches first by a wide
margin — no snapshot to load:

| zipp | node | deno | bun |
|---|---|---|---|
| **10.7ms** | 34.5ms | 51.5ms | 64.0ms |

A long-running server would amortize that away; a CLI tool would not.

### Architecture diagnostics

Three *diagnostic* benchmarks sit outside the headline ten precisely because
they expose what the ten cannot. They remain a separate row set for historical
comparability, but zipp is now fastest on all three:

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| sparse-array-v2 | 175ms | 377ms | 150ms | **96ms** | **0.55×** |
| polymorphic-objects-v2 | 87ms | 98ms | 98ms | **26ms** | **0.31×** |
| property-ic-shapes | 266ms | 167ms | 278ms | **12ms** | **0.05×** |
| **zipp / engine median geomean** | **0.20×** | **0.17×** | **0.20×** | — | |

The new wins come from guarded, exact-shape stream and reducer paths. They are
strong evidence for these workloads, not proof that broad object-model or
shape-metadata parity is complete. The next performance work should generalise
those paths and improve startup-adjusted compute without giving the cold result
back.

### Hostile application diagnostics

The retained series is intentionally stable and historically useful, but it is
also unusually friendly to Zipp: mostly top-level `var`, fixed shapes, and hot
paths already covered by exact reducers. The separate 17-case
[`bench/hostile`](bench/hostile/README.md) corpus deliberately attacks those
assumptions with nested functions, `let`/`const`, mutable closures, megamorphic
properties, mixed local types, allocation/GC pressure, exceptions, sustained
async work, modules, a React-shaped kernel, a warm router, and exact vendored npm
source.

Hostile results are never folded into the retained-ten headline. They are the
generalisation gate, and Zipp does **not** yet claim Node parity on that corpus.
The harness reports cold and startup-adjusted ratios, category-balanced results,
and baseline/stressor degradation while requiring deterministic, exact output.
The historical
[Wave 30 diagnostic](bench/hostile/w30_combined_dirty_2026-08-25.json) measured
**3.1023× category-balanced / 2.7173× ordinary geomean**. The latest
[Wave 39 development checkpoint](bench/hostile/w39_final_cleanenv_dirty_2026-08-25.json)
uses the full corpus, 15 counterbalanced repetitions and 10,000 bootstrap
samples. It is exact on all 17 rows, healthy and drift-free, and measures
**1.3564× cold category-balanced / 1.2866× ordinary geomean**. The artifact
correctly remains `publishable:false` because it measures dirty/untracked
sources rather than a clean release.

That gain is broad but not parity. Bytecode VM 0.895×, modules 0.396×,
ephemeral allocation 0.368×, stable numeric locals 0.464×, throw/catch 0.527×,
and async burst 0.635× beat Node. The remaining cold gaps are surviving
allocation 4.623×, stable/megamorphic object shapes 3.871×/3.848×, the warm
router 3.792×, React-shaped reconciliation 3.272×, exact vendored NanoID
2.566×, mutable closures 1.976×, and mixed locals 1.686×. Object-shape
degradation is now Node-like (0.984× relative), while survivor lifetime
degradation remains 12.539×; the per-row/category gate therefore still fails.

A native shape-way experiment measured about 10.6% faster on the stable-shape
row, then independent security review found exotic-shape collisions and stale
raw-metadata paths. It was fully reverted and zero-symbol audited; no
experimental unsafe shape-way code ships. Later work added guarded closure and
local-allocation paths without reviving that design. The next targets are safe
ordinary-object storage/creation, survivor tracing, closure/frame dispatch, and
mixed local representation.

Two focused improvements landed after the Wave 39 full-corpus artifact and are
therefore not folded into its ratios: direct, semantics-preserving
`[[HomeObject]]` side-table storage made `allocation-survival` **14.1% faster**
in a 15-pair same-binary A/B, and an integer-only live-guarded rotating-arrow
cross-call descriptor made `calls-closures` about **5.1% faster** across 30
pairs. A separate pointer-free literal-transition cache was still about 1.9x
slower on both object-shape rows and was fully reverted. Exact mechanisms,
confidence intervals, safety invariants and negative evidence are recorded as
B162-B164 in [`PERF_ROADMAP.md`](PERF_ROADMAP.md); no new full-corpus parity
claim is made from the focused runs.

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
