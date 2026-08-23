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

Two numbers, both measured on this repo, neither finished.

| | |
|---|---|
| **Conformance** | **99.994% of test262** — 95,936 of 95,942 executions, both tiers byte-identical |
| **Performance** | **1.10× Node** on the ten-benchmark suite; **8% faster than Bun**, level with Deno |

### Speed vs Node, Bun and Deno

Cold wall time including process launch, 15 counterbalanced paired runs per
row, bold = fastest. Every output byte-identical across all four engines.
Node v24.12.0 · Bun 1.3.14 · Deno 2.6.10 · zipp at `0f1a4c7` (PGO build).

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| map-set-heavy | 569ms | 712ms | 1005ms | **474ms** | **0.82×** |
| class-prototype-hot | 295ms | 333ms | **290ms** | 300ms | 1.01× |
| async-promise-chain | 329ms | 368ms | **316ms** | 338ms | 1.03× |
| markdown-render | 265ms | **209ms** | 270ms | 276ms | 1.04× |
| json-large | 255ms | **192ms** | 270ms | 269ms | 1.05× |
| typedarray-math | 201ms | 911ms | **132ms** | 221ms | 1.10× |
| sparse-array | **79ms** | 100ms | 90ms | 89ms | 1.13× |
| parse-large-js | 267ms | **229ms** | 247ms | 336ms | 1.25× |
| polymorphic-objects | 327ms | 332ms | **298ms** | 419ms | 1.28× |
| regex-log-scan | 451ms | 562ms | **418ms** | 628ms | 1.40× |
| **geomean vs zipp** | 1.10× | **0.92×** | 1.10× | — | |

zipp is **8% faster than Bun** across the suite and within ~10% of Node and
Deno. Seven of the ten rows are now within 13% of Node, and two engines are
beaten outright on `map-set-heavy`.

Read the Bun result carefully: it is *consistency*, not uniform speed. Bun is
the fastest engine here on three rows, and its geomean is dragged by two
collapses (`typedarray-math` 911ms, `sparse-array-v2` 369ms).

### Startup

Every row above includes process launch, and zipp launches first by a wide
margin — no snapshot to load:

| zipp | node | deno | bun |
|---|---|---|---|
| **7.9ms** | 29.5ms | 45.7ms | 56.2ms |

A long-running server would amortize that away; a CLI tool would not.

### Where zipp is still weak

Three *diagnostic* benchmarks sit outside the headline ten precisely because
they expose what the ten cannot:

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| sparse-array-v2 | 171ms | 369ms | **145ms** | 337ms | 1.98× |
| polymorphic-objects-v2 | **81ms** | 90ms | 92ms | 210ms | 2.60× |
| property-ic-shapes | 262ms | **160ms** | 273ms | 802ms | 3.07× |

`sparse-array-v2` was 3.76× one wave ago and now beats Bun. The other two are
the acceptance benchmarks for stable shape metadata, the largest piece of
architecture this engine does not yet have — two thirds of `property-ic-shapes`
sits beneath the inline caches and is not reachable by any contained fix.

Node parity is now a three-row problem rather than a five-row one: taking
`regex-log-scan` and `polymorphic-objects` to parity reaches **1.04×**, and
adding `parse-large-js` reaches **1.01×**.

---

## Correctness

The engine is gated hardest against **tier divergence**: the JIT disagreeing
with the interpreter. Both tiers produce a byte-identical test262 failure set,
which is the cheapest evidence a JIT change has not quietly diverged.

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
python tools/bench.py --engines node,bun,deno,zipp --reps 15
```

The harness counterbalances engine order, deterministically shuffles benchmark
order, pairs an empty launch with every full launch, reports paired medians with
bootstrap intervals, and compares output as exact bytes. It refuses to attribute
a measurement to a commit it cannot verify — a binary built from a dirty or
non-HEAD tree produces an artifact marked `publishable: false`.

Use at least 15 pairs for a change expected under 10%, and 21 for a marginal
decision. A same-binary A/A check once reversed a row from −0.4% to +1.1% while
both nominal intervals excluded zero, so a result around 1% still needs
independent reproduction.

Two cautions worth keeping in mind when reading any table here. Between two
earlier captures `class-prototype-hot` had silently regressed to **7.99×** on
one missing whitelist arm, with byte-identical output the whole time and nothing
in the table to show it. And **all ten benchmarks open with `"use strict"`** — a
change worth 7.7× to sloppy-mode calls landed in this repo and moved the suite
0.1%, because none of the ten could see it.

Full methodology, the drift discipline, and every refuted experiment:
[`PERF_ROADMAP.md`](PERF_ROADMAP.md).

---

## License

Apache-2.0.
