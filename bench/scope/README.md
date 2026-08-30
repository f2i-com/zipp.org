# Scope sensitivity of the guarded reducers

> **Historical research snapshot (2026-08-24).** The measurements below answer
> a still-useful lowering question, but they are not the current public Zipp
> score. Use the root README and `bench/README.md` for current captures and
> runner policy.

These programs answer one question: **does a mechanism that wins on a benchmark
still win when the same code is written the way real programs are written?**

`typedarray-math.js` and `sparse-array.js` here are byte-identical copies of
`bench/real/`. The variants are semantics-preserving rewrites of those files:

| variant | rewrite | why it is a fair test |
|---|---|---|
| `.R1_iife.js` | the whole program wrapped in `(function main(){ … })()` | how essentially all real code is scoped: module bodies, functions, bundlers |
| `.R2_let.js` | every top-level `var` becomes `let` | how code written after ES2015 is spelled |
| `.R3_rename.js` | loop counters and accumulators renamed | a control: pure identifier text, nothing structural |

Wave 29 additionally retains `property-ic-shapes.R1_iife.js`, the exact
whole-program-IIFE rewrite used to price B144's captured-limit field-stream
generalisation. Its 15-pair schema-v2 observations and source hash are in
`bench/w29_property_ic_iife_field_stream_abenv_2026-08-24.json`.

Every variant is verified to produce the same output as its original under Node,
and zipp is verified to agree with Node on all of them — this is a performance
question, not a correctness one.

## The measured result (2026-08-24, plain release build, 9 interleaved paired reps)

Ratios are zipp / Node, so lower is better and below 1.00 beats Node. Measured
twice: first on a machine that turned out to have leftover CPU-bound processes
on it, then again once it was quiet. **The relative penalty — the finding —
replicated to within a point**, because engines and variants are interleaved
inside every repetition and background load therefore hits both sides equally.
The absolute ratios did move, so the quiet run is the one to quote.

| variant | typedarray-math (quiet) | (loaded) | sparse-array (quiet) | (loaded) |
|---|---|---|---|---|
| original (top-level `var`) | **0.668×** | 0.623× | **0.962×** | 0.885× |
| `.R1_iife` | 0.910× (**+36%**) | (+39%) | 1.260× (**+31%**) | (+34%) |
| `.R2_let` | 0.963× (**+44%**) | (+44%) | 1.120× (**+16%**) | (+15%) |
| `.R3_rename` | 0.659× (unchanged) | (unchanged) | 0.951× (unchanged) | (unchanged) |

Node is flat across all four variants in both runs (203/205/203/203ms and
82/82/81/82ms quiet), so the sensitivity is entirely zipp-side.

Two things follow. **Renaming is free**, so the reducers are not keyed on
identifier text — that was the obvious suspicion and it is refuted. But
**scope is not free**: `sparse-array` crosses from beating Node to losing to it
purely by being wrapped in a function.

`ta_phase*.js` are phase-timed copies that localise it to a single phase:

```
              dataview phase:   zipp      node
  ta_phase.js (top-level var)      0ms      98ms
  ta_phase.R1_iife.js             65ms      97ms
  ta_phase.R2_let.js              59ms      90ms
```

and a one-binary ablation confirms the mechanism exactly — `ZIPP_NO_DV_NESTED_REDUCE=1`
moves that phase 0ms → 63ms with byte-identical output.

## Suite-wide (2026-08-24, `python bench/scope/sweep.py`)

The two-row result above is not a two-row problem. Across the whole suite:

| rewrite | rows | geomean penalty | rows that cross from beating Node to losing |
|---|---|---|---|
| wrapped in an IIFE | 13 | **+159%** | **8** |
| `var` → `let` | 12 | **+60%** | **6** |

Worst cases, zipp/Node before → after: `property-ic-shapes` 0.043 → 4.621 under
IIFE (11ms → 1208ms), `parse-large-js` 0.947 → 3.619, `regex-log-scan`
0.969 → 3.106, `polymorphic-objects-v2` 0.360 → 3.650, `typedarray-math`
0.665 → 2.443 under `let`.

`parse-large-js` is excluded from the `let` sweep only: it embeds JavaScript
source as *data*, so a textual `var`→`let` rewrite edits the corpus it parses.
That is a property of the rewrite, not of the engine; the sweep checks every
variant against Node and drops any whose answer changed.

**The published "lowest median on all 13" result is a result for top-level `var`
code.** It is honest for what it measures, and the programs are unchanged from
the historical series — but essentially all real JavaScript is inside a function
and spelled with `let`/`const`, and in that form zipp currently loses most of
these rows to Node.

## Root cause of the worst case, traced to the opcode

Two words do it. In `ta_phase.js`, changing the DataView inner loop's body
locals — `var le` / `var v` — to `let`:

```
                       zipp dataview phase    node
  orig (all var)                     1ms       95ms     0.01x
  loop COUNTERS let only            61ms       95ms     0.64x
  loop BODY let only               361ms       95ms     3.80x   <-- the cliff
  all let                          358ms       86ms     4.16x
```

Node is flat. The chain, from `ZIPP_JITLOG=1` + `ZIPP_JITDECLINE=1`:

1. With `let` body locals the region carries more live homes.
2. `INT-GPR decline: 13 homes > 8 gprs` — the GPR pool is exhausted.
3. The region falls to the **MEM (boxed) tier**, which the ledger prices at
   ~3.5ns per boxed op. That is the ~6x.

Note this is *not* the reducer declining. Losing the reducer costs 1ms → 61ms
and zipp still beats Node. The extra 61ms → 361ms is the tier demotion, and it
is what puts zipp 3.8x *behind* Node.

### Refuted along the way (do not re-chase)

- **Not GC/allocation.** `ZIPP_GCSTATS=1` reports **0 collections** in both.
- **Not the pin plan.** Instrumenting every `continue` in `build_ta_pin_plan`
  showed zero pin declines; the DataView pins are built in both.
- **Not the DV retry gate.** Instrumented, it *passes*:
  `admit_dv_unadmitted=Some(0) pins=1 access=3`. The retry fires and the GPR
  emitter declines afterwards, on pool exhaustion.
- **Not the existing spill-slot mechanism.** `ZIPP_GPR_SPILL_SLOTS=1` was built
  for 12–14-home regions exactly like this and does not recover it:
  361ms → 369ms. The wave-9 refutation holds on this shape too.
- **Not identifier text.** Renaming is free (see the table above).
- **Not program size.** Top-level `let` involves no function wrapper at all and
  still costs +60% geomean.

### What actually distinguishes the fast case (corrected)

A follow-up narrowed this further, and it is *not* about block scoping or
per-iteration bindings. Hoisting the two `let`s out of the loop entirely — one
`let le, v;` before the outer loop, assigned inside — does **not** recover it:

```
  var in loop body     zipp   1ms     node 95ms
  let in loop body     zipp 365ms     node 95ms
  let hoisted out      zipp 364ms     node 95ms   <-- hoisting does not help
```

The real distinction is **where the value lives**. At top level a `var` is a
GLOBAL SLOT, and wave 13's stored-global live-range narrowing
(`ZIPP_NO_GLOB_RANGE`) narrows exactly those: the fast plan logs
`narrowed=[14, 21]` — slots 14 and 21 are `le` and `v` — which frees two
permanent homes and brings the region to `homes=9`, inside the 8-GPR pool after
sharing. A `let` is a lexical binding that does not live in a plain global slot,
so it is invisible to that pass: the plan logs `narrowed=[]`, homes stay high,
and `plan_region_cold` declines before it ever reports a home count.

Proof by its own off-switch: disabling the pass on the *`var`* program
reproduces the failure — `ZIPP_NO_GLOB_RANGE=1` moves the phase 0ms → **202ms**
(it falls to the DOUBLE tier). So glob-range narrowing is precisely what is
carrying the fast case, and it only reaches global slots.

This is the inversion worth internalising: **a value is currently cheaper as a
top-level global than as a local**, which is backwards from every other engine.
The narrowing pass computes segments for registers too (`seg_map` over
`reg_order` in `plan_region.rs`), but the whole block is gated behind
`(admit_dv || share_homes) && reuse && cold.is_empty()` and never runs for these
regions in the `let` shape.

So the fix is register **pressure**, not register **spilling**: give
non-global bindings the same live-range narrowing that global slots already get,
so a local costs no more homes than the global it replaced. That is a
compiler/planner change upstream of the emitter that currently declines.

## Why

`dv_nested_reduce_plan` in `codegen/region_int_gpr.rs` matches only
`Instr::LoadGlobal` / `dv_store_global` for the induction variables and the
accumulator, so a frame-local or lexically-bound one cannot match. The gate is
structural, not heuristic — see ledger entry **B140**.

Note what the mechanism does: it runs one real inner pass and applies the affine
accumulator delta to the remaining outer iterations, so on that phase zipp
performs about 1/6000 of the arithmetic Node performs. That is sound for a
provably pure nested reduction, but it does mean `typedarray-math`'s margin over
Node is attributable to it — with the reducer off the row measures ~1.05× Node
instead of 0.66×.

## Reproducing

```sh
python bench/scope/run.py            # 9 reps, both rows, all variants
python bench/scope/run.py 15         # more reps
python tools/bench.py --reps 15 --bench-dir bench/scope \
  --benches property-ic-shapes.R1_iife \
  --ab target/release/zipp target/release/zipp \
  --ab-env ZIPP_NO_FIELD_READ_STREAM=1 - --allow-dirty-engine
```
