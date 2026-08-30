# Zipp performance roadmap

> **Goal:** become faster than Node, Bun, and Deno on every maintained
> benchmark while preserving exact JavaScript semantics and tier parity.

This file is the current performance board. The append-only B001–B252 ledger,
including the negative results and original line-level evidence, is archived at
[`docs/archive/PERF_LEDGER-B001-B252.md`](docs/archive/PERF_LEDGER-B001-B252.md).
Search that file before reopening an older idea.

## Evidence legend

| Label | Meaning |
|---|---|
| **CANONICAL** | Clean, HEAD-matching, provenance-stamped PGO capture with the complete required engine/suite protocol and `publishable:true`. May update public Node/Bun/Deno ratios. |
| **DIAGNOSTIC A/B** | Focused, filtered, same-binary, dirty-comparator, non-PGO, or incomplete-engine evidence. Useful for decisions; never a public engine score. |
| **LANDED** | Default-on code is committed and pushed after its correctness and performance gates. |
| **VERIFIED** | Mechanism and result are established; the surrounding campaign or public capture may still be pending. |
| **REFUTED** | Built or measured and rejected. Do not retry without new evidence that invalidates the kill reason. |
| **REVERTED** | Experimental code is absent from the default tree. |

Cold wall time is the headline. Startup-adjusted time is diagnostic, especially
on short cases. Ratios are new/old for binary A/Bs and Zipp/competitor for engine
tables; below `1.0×` is faster.

## Current status — 2026-08-30

### Public canonical capture

The public score remains the clean PGO capture at `0bff482`:

| Corpus | Zipp / Node | Status |
|---|---:|---|
| retained ten | **0.918×** [0.914, 0.922] | 6 point wins, four gaps/parity rows |
| diagnostics three | **0.186×** [0.184, 0.188] | Zipp wins all three |
| all 13 | **0.635×** [0.633, 0.637] | 9 / 13 Node point wins |
| hostile all 17, ordinary | **0.866177×** [0.858901, 0.870652] | separate stress corpus |
| hostile category-balanced | **0.906139×** [0.899437, 0.911396] | separate stress corpus |

Across Node, Bun, and Deno, 29 / 39 point comparisons and 29 / 39
Bonferroni exact-sign comparisons are wins. The literal all-row target remains
false. See [`README.md`](README.md#performance-measured-honestly) for the table.

### Correctness

- test262: **95,939 / 95,942** required executions (99.997%)
- expected failures: one stale Annex B assertion and two German-CLDR rows
- former errored-module-cycle and deferred top-level-await gaps: fixed
- default JIT, interpreter-only, forced-JIT, and majors-only-GC identities are
  the conformance gate
- tier-differential fuzzing remains mandatory for JIT work

Do not edit historical B entries merely because their old pass counts were
correct for their own commit.

## Latest experiment registry

### B254 LANDED — pinned call-result string length

Commit `4ff3bdf` uses the MEM string pin's immutable `{obj_bits, bytes, units}`
snapshot to answer `.length` before the eight-way identity IC and B190 helper.
The emitted lane checks the exact receiver identity, rejects an invalid
snapshot, loads UTF-16 units, boxes the value, and otherwise falls through to
the unchanged generic path. `ZIPP_NO_PINNED_STR_LEN=1` restores the prior route.

Final reviewed binary: commit `2869e91`, SHA-256 `bf85742b…f85952`, clean
release build.

| DIAGNOSTIC A/B | Result |
|---|---|
| same-binary NanoID, 32 pairs | **−13.11%**, 0.868869 [0.860467, 0.873758], 31 / 32 wins |
| final vs B252 NanoID, 32 pairs | **−12.85%**, 0.871516 [0.866427, 0.881037], 32 / 32 wins |
| final vs frozen B253, hostile all-17 | **−1.28%**, 0.987230 [0.984695, 0.991308] |
| final vs frozen B253, category-balanced | **−1.74%**, 0.982579 [0.979742, 0.987197] |
| final vs frozen B253, NanoID | **−12.88%**, 0.871198 [0.866817, 0.877921] |
| final vs frozen B253, React | **−2.12%**, 0.978798 [0.974462, 0.991749] |
| final vs frozen B253, normal all-13 | **−0.50%**, 0.9950 [0.9873, 1.0017], neutral |

Every output was exact and no cold row had a supported regression. The full
hostile artifact's adjusted analysis is incomplete only because the very short
allocation-ephemeral case had no usable adjusted ratio; the cold analysis is
complete. The B253 comparator is a frozen dirty feature build, so the sweep is
not publication evidence.

Filtered 16-pair Node diagnostics put final NanoID at **1.169× Node** [1.160,
1.195] and React at **1.673× Node** [1.618, 1.715]. The target remains open.

### B254 hardening LANDED — impossible identity-IC empty marker

Commit `6f945ce` replaces zero in fresh identity-cache ways with
`0x7FFE000000000001`, a NaN bit pattern outside all five `Value` tags. Numeric
boxing canonicalizes it, so raw f64 `+0` cannot match an empty way when pinned
length or quick length is switched off. Direct shape-pair sites deliberately
retain zero because their packed pattern slots use it as the free marker.

Release tests prove raw `+0` reaches the target MEM property region under all
three feature off-switches. This is a safety fix with no speed claim.

### B253 LANDED — stable concat suffix memo

Commit `ff1c737` adds a weak, lazy 256-entry suffix memo for a size-bounded
B212-frozen `const + int` head followed by one pinned immutable one-byte ASCII
suffix. Exact keys plus left/result versions cover ABA; results stay frozen and
cannot seed recursively. The emitter admits only an adjacent terminal
`StrConcatChain` followed by the lowering's exact `Move`, and the runtime proves
the final leaf is tagged `Int`. `ZIPP_NO_CONCAT_SUFFIX_MEMO=1` restores the old
route.

Same-binary React evidence, 32 pairs: **−4.14%**, 0.958636 [0.949720,
0.965530], 31 / 32 wins. Complete normal and hostile safety sweeps were neutral.

### B253 hardening LANDED — validate both concat links

Commit `2869e91` requires both current and next chain links to satisfy
`dst == a == accumulator`, and the trailing `Move` to consume that accumulator,
before emitting the suffix hint. The compiler already generated that invariant;
the validation protects future rewrites. Focused invariant and all 19 concat
parity/mechanism tests pass. No speed claim.

### B252 LANDED — merge monotonic recycled-object address runs

Commit `f5c33df` proves an ascending retained prefix and monotonic newly-dead
suffix, then joins them with reverse/rotate; uncertain layouts retain the full
sort. `ZIPP_NO_OBJ_POOL_RUN_SORT=1` restores the old route. Allocation survival
improved **2.93%** [0.28%, 3.34%] across 31 pairs; shapes and the normal suite
were neutral. A MEM-to-nested-INT NanoID handoff was separately **REFUTED** at
**+86.11% slower** and fully reverted.

### B251 LANDED — scalar singleton `[[IsHTMLDDA]]`

Commit `c575105` mirrors the production VM's one HTMLDDA exotic as a scalar
comparison instead of paying a hash lookup in `typeof`, loose-null, truthiness,
callability, and Annex B paths. React improved **4.11%** [3.75%, 5.05%]; normal
all-13 was neutral. `ZIPP_NO_HTMLDDA_SCALAR=1` restores the old path.

### B250 VERIFIED — call-result string pins and call-entry cleanup

B250 admitted call-written string receivers only under a forward MUST
reaching-definition proof, refreshed snapshots after calls, loaded Tier-C bases
directly, and emitted same-`FuncProto` guards. Its combined exact hostile A/B
improved **3.07%** [1.57%, 4.30%], with NanoID and closure-call gains; the
normal all-13 sweep was neutral. B254 removes the remaining `.length` IC/helper
cost from that string-pin path.

### B249 CANONICAL — captured-call scaffolding closed

B249 restored fused direct and computed method calls where argument evaluation
cannot observe the property read, stopped recycling captured temporaries, and
added bare guarded `MathOp` lowering. The clean `0bff482` PGO capture moved the
retained-ten result to 0.918× Node, all-13 to 0.635×, and hostile ordinary to
0.866×. It is the current public baseline.

Earlier entries, designs, hazards, and refutations remain searchable in the
[historical ledger](docs/archive/PERF_LEDGER-B001-B252.md).

## Active targets

Priorities are ordered by current measured gap and by whether the next question
can be answered with a bounded experiment.

### 1. React-shaped reconcile — 1.673× Node

B251 and B253 removed two known taxes. Re-profile the final B254 layout before
acting on the older attribution. Measure these terms independently:

- recursive framed self-calls and same-prototype guard/fallback traffic;
- handler-bearing `for-of` loop admission (`PushFinally` / `IterCloseFinally`);
- repeated `typeof` and loose-null helper traffic after HTMLDDA scalarization;
- allocation throughput and sweep share after suffix memoization; and
- object-pattern argument allocation if it remains visible.

Kill any proposal whose focused lane does not explain at least 3% of the row or
whose safety tax reaches unrelated normal cases.

### 2. NanoID — 1.169× Node

The call-result pin and direct `.length` lane have moved the row materially.
Profile again. Likely questions, not pre-approved fixes:

- remaining per-character `charCodeAt` helper/lane cost;
- why the outer MEM region still owns the inner integer loop;
- whether a direct pinned ASCII char load can be reused without widening the
  receiver or mutation proof; and
- whether the residual is launch/layout rather than runtime work.

Do not retry the nested-OSR host handoff refuted by B252.

### 3. Fresh canonical capture

After documentation and the repository-wide release gate, build one clean PGO
artifact and run the complete normal and hostile Node/Bun/Deno/Zipp protocols.
Until then, public ratios stay at `0bff482`.

### 4. Remaining canonical Node gaps

The last clean hostile capture still has substantial Node gaps in allocation
survival, warm router, closures, and shapes; the normal retained set still has
async-promise-chain and smaller json/regex/sparse gaps. Each needs a current
profile because B250–B254 changed code shape and attribution.

## Standing gate

### Correctness

```powershell
cargo test --workspace --release
cargo check -p zipp-vm --no-default-features
cargo check -p zipp-vm --no-default-features --features safe-sandbox
```

For changes that touch sandbox-only code, also run the standalone sandbox
workspace. For interpreter/JIT semantics, compare the test262 expected-failure
identity in default, `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1`, and
`ZIPP_NO_NURSERY=1` modes. Run the tier-differential fuzz slice for codegen,
register-planning, deopt, or heap-layout changes.

### Focused performance

1. Add an off-switch where a same-binary A/B is practical.
2. Use at least 16 exactly balanced pairs; use 32 for marginal or
   layout-sensitive decisions.
3. Require exact output and empty health/correctness/drift failure lists.
4. Re-run the affected normal and hostile safety sets.
5. Fail a candidate when a supported unrelated cold regression exceeds 0.5%
   unless a documented trade is explicitly accepted.
6. Freeze and hash the final clean binary; do not benchmark a moving target.

Routine artifacts go under ignored `target/bench-results/`. See
[`bench/README.md`](bench/README.md) for commands and publication policy.

### Public capture

Use `tools/pgo.sh`, then the complete `tools/bench.py` and
`tools/bench_hostile.py` engine protocols. `bench/run_real.sh` is legacy and is
not the publication runner. Promote an artifact only after an independent audit
confirms `publishable:true`, clean source/binary provenance, full engine order,
complete suite selection, exact output, adequate repetitions/bootstrap samples,
and no drift.

## Experiment discipline

- Profile the current binary before editing; old attribution expires quickly.
- Change one mechanism at a time and push each verified small win.
- Prefer same-binary attribution, then a frozen-binary layout/generalisation
  comparison, then a fresh engine diagnostic.
- Record the exact comparator, binary hash, commit, switches, repetitions,
  ordering, medians, ratio, interval, strict wins, and correctness status.
- Neutral results are useful. Revert experiments that miss their gate and keep
  the kill reason here.
- Never use an aggregate to imply that every row wins.
- Never convert a security-policy change into a performance default without an
  explicit policy decision.

## Historical lookup

The archived ledger retains B001–B252, old section numbers, detailed hazard
proofs, and refuted experiments. Source comments that cite “`PERF_ROADMAP.md`
B59”, B9, B29, or another historical ID refer to that archive. The durable
nursery design remains at [`NURSERY_DESIGN.md`](NURSERY_DESIGN.md) because code
comments cite its numbered sections directly.
