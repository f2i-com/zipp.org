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

Two numbers, both measured on this repo. The headline speed goal is met; the
architecture diagnostics below are not.

| | |
|---|---|
| **Conformance** | **99.994% of test262** — 95,936 of 95,942 executions, four modes with the same 6 expected failures |
| **Performance** | **0.9695× Node** [0.9655, 0.9741] on the headline ten — about **3.1% faster than Node** |

### Speed vs Node, Bun and Deno

Cold wall time including process launch, 21 counterbalanced paired runs per
row. Bold time = fastest engine; bold ratio = zipp beats Node. Every output is
byte-identical across all four engines.
Node v24.12.0 · Bun 1.3.14 · Deno 2.6.10 · zipp at `200cbfc` (PGO build).

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| map-set-heavy | 609ms | 744ms | 1045ms | **470ms** | **0.78×** |
| typedarray-math | 207ms | 916ms | **140ms** | 194ms | **0.94×** |
| class-prototype-hot | 300ms | 341ms | 294ms | **286ms** | **0.95×** |
| parse-large-js | 274ms | **236ms** | 255ms | 262ms | **0.96×** |
| async-promise-chain | 337ms | 375ms | **325ms** | 327ms | **0.97×** |
| json-large | 261ms | **199ms** | 281ms | 264ms | 1.01× |
| markdown-render | 270ms | **217ms** | 282ms | 277ms | 1.02× |
| regex-log-scan | 454ms | 571ms | **428ms** | 457ms | 1.02× |
| sparse-array | **84ms** | 109ms | 96ms | 89ms | 1.04× |
| polymorphic-objects | 329ms | 339ms | **305ms** | 346ms | 1.05× |
| **zipp / engine geomean** | **0.97×** | **0.81×** | **0.96×** | — | |

The paired zipp/Node headline is **0.9695× [0.9655, 0.9741]**: the full interval
is below parity. By median geomeans zipp is also about **19% faster than Bun**
and **4% faster than Deno**. No headline row is more than 5% slower than Node,
while zipp wins `map-set-heavy` and `class-prototype-hot` outright and starts
far faster than all three engines.

Read the Bun result carefully: it is *consistency*, not uniform speed. Bun is
the fastest engine here on three rows, while its headline geomean is dragged by
`typedarray-math` (916ms) and clear losses on map/set and class workloads.

### Startup

Every row above includes process launch, and zipp launches first by a wide
margin — no snapshot to load:

| zipp | node | deno | bun |
|---|---|---|---|
| **10.6ms** | 34.4ms | 52.9ms | 63.7ms |

A long-running server would amortize that away; a CLI tool would not.

### Where zipp is still weak

Three *diagnostic* benchmarks sit outside the headline ten precisely because
they expose what the ten cannot:

| benchmark | node | bun | deno | **zipp** | ratio to node |
|---|---|---|---|---|---|
| sparse-array-v2 | 175ms | 375ms | **152ms** | 329ms | 1.88× |
| polymorphic-objects-v2 | **86ms** | 98ms | 100ms | 190ms | 2.21× |
| property-ic-shapes | 266ms | **165ms** | 273ms | 686ms | 2.58× |

`sparse-array-v2` was 3.76× before the sparse campaign and is now 1.88×. The
other two are acceptance benchmarks for stable shape metadata, the largest
piece of architecture this engine does not yet have — much of
`property-ic-shapes` sits beneath the inline caches and is not reachable by a
contained call-site fast path.

Node parity on the published cold headline is complete. The next performance
work is deliberately broader: bring the three architecture diagnostics down and
improve startup-adjusted compute without giving the cold result back.

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
