# zipp

A clean-sheet JavaScript engine in Rust — a NaN-boxed register VM with
per-call-site inline caches and a native x86-64 OSR JIT. No third-party parser,
no third-party runtime: the lexer, parser, bytecode compiler, interpreter, JIT
and GC are all in this repo.

Home: <https://github.com/f2i-com/zipp.org> · Reference: [`DOC.md`](DOC.md) ·
Measurements and refutations: [`PERF_ROADMAP.md`](PERF_ROADMAP.md)

```sh
git clone https://github.com/f2i-com/zipp.org.git zipp && cd zipp
cargo build --release
./target/release/zipp js file.js
./target/release/zipp mjs file.mjs      # ES module entry (top-level await)
```

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

The JIT is x86-64 only and feature-gated. Every other target builds a pure
interpreter — aarch64 and wasm32 are both built and tested.
`crates/zipp-wasm` embeds the engine for browser hosts, and `embed::ScriptState`
keeps a VM alive across host calls for runtimes that render, wait, and call
back in ([`DOC.md`](DOC.md#embedding)).

---

## Reproducing the numbers

```sh
bash tools/pgo.sh                                    # the measured binary
python tools/bench.py --engines node,bun,deno,zipp --reps 21
```

The harness exactly counterbalances two-engine A/B order and deterministically
shuffles engine and benchmark order for multi-engine captures. It pairs an empty
launch with every full launch, reports paired medians with bootstrap intervals,
and compares output as exact bytes. By default it refuses dirty or non-HEAD
engines; explicit override flags exist for diagnostics, so publication requires
auditing the recorded provenance rather than trusting `publishable` alone.

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
