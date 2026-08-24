# Scope sensitivity of the guarded reducers

These programs answer one question: **does a mechanism that wins on a benchmark
still win when the same code is written the way real programs are written?**

`typedarray-math.js` and `sparse-array.js` here are byte-identical copies of
`bench/real/`. The variants are semantics-preserving rewrites of those files:

| variant | rewrite | why it is a fair test |
|---|---|---|
| `.R1_iife.js` | the whole program wrapped in `(function main(){ … })()` | how essentially all real code is scoped: module bodies, functions, bundlers |
| `.R2_let.js` | every top-level `var` becomes `let` | how code written after ES2015 is spelled |
| `.R3_rename.js` | loop counters and accumulators renamed | a control: pure identifier text, nothing structural |

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
```
