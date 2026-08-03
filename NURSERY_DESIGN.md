# B6 — Generational / nursery GC: design study and costing (W5)

Status: DESIGN ONLY. No code changed. All numbers below are either the roadmap's own
measured entries (cited by B-number) or file:line reads of the current tree.
No engine binary existed in this worktree to re-run `ZIPP_GCSTATS`; the B84 table IS
that instrument's output and is used as the primary input, per B101's costing method.

## 0. The fact that decides the whole design space

**`Value` holds a heap INDEX, not a pointer, and the collector is already non-moving.**

- `crates/zipp-vm/src/vm/gc.rs:1-7`: *"The heap is a `Vec<HeapObj>` indexed by `u32`;
  `Value` references objects by INDEX, so this collector is NON-MOVING — it only frees
  unreachable slots back to a free list … never relocating live objects."*
- `crates/zipp-vm/src/heap.rs:2244-2260` (`Heap::alloc`): pop free slot or push;
  `heap.rs:2291-2296` (`free_slot`): tombstone + version bump + push to `free`.
- Object IDENTITY is the slot index. Therefore **"promotion" never needs a copy**:
  young→old is a bookkeeping state change on the same index. A copying/moving nursery
  (the B6.1 "moving young-generation collector over a tagged-index heap", roadmap:821-835,
  effort XL, "highest risk in the document") buys nothing that index-state generational
  bookkeeping doesn't, and breaks things the non-moving form doesn't:
  - JIT IC `vals_ptr` (raw pointer into an ObjMap's `vals` Vec): `codegen/mod.rs:472,842`,
    inline call-free load/store `emit.rs:397-518` (`mov [rcx+rdx*8], r10` — the Set hit).
    Valid only because objects never move and version bumps catch `vals` reallocation.
  - Pinned TypedArray/receiver snapshots per region entry: `codegen/regalloc.rs:58-104`
    (buffer base pointers snapshotted into the native frame).
  - `SlotTable` sidecars keyed BY SLOT INDEX: `vm/mod.rs:570` (`proto_of`), `:590`
    (`fn_props`), `:610` (`arr_props`), `:615`, `:701`, `:959` — a move would need every
    one rekeyed.
  - `jit_const_strings` (string bits embedded in native code, gc.rs:129-132).

  **Conclusion: B6.1 as written should be retired. The only design worth costing is a
  non-moving generational split over the existing index heap.**

## 1. What a minor collection must trace — old→young edge sources and barrier forms

Young = the set of slots allocated since the last collection (an *allocation log*:
`alloc` at heap.rs:2244 pushes `idx` — one `Vec::push`, ~1-1.5ns on a 23-148ns
allocation). A minor GC is sound iff every path from an OLD object to a YOUNG one is
known. Edge sources, exhaustively (from `trace_edges`, gc.rs:552-781, which is the
complete edge catalogue):

| # | edge source | write chokepoint(s) | cheapest barrier |
|---|---|---|---|
| 1 | `ObjMap.vals` (SetProp/define, data props) | interp: `vm/access.rs:797 set_prop` and the props/ helpers; JIT slow/miss: `helpers_misc.rs:492 jit_set_prop_slow`, `:545 _acc`, `:1882 _miss`; **JIT IC hit: NO CALL** (`emit.rs:515-518`) | interp/helpers: 1 test in the shared store fn. JIT hit path: 3-5 emitted instrs (see below) |
| 2 | `Array` items (SetIndex/push/fill/copyWithin/…) | `helpers_misc.rs:710 jit_set_index`, `:2319 _concat`; interp array_ops.rs; region JIT inline element store (`region_mem.rs` fast paths) | same split: helper-side test cheap; inline element store needs emitted barrier |
| 3 | `Cell` (captured-variable write) | `dispatch.rs:3891 CellSet`, `:3897 CellSetChecked`, `:3924 UpvalSet`; note `region_admit.rs:153-155` currently DECLINES CellSet from the JIT (B10.3) — today this is interpreter-only, i.e. free to barrier; if B10.3 lands, its emitter must carry the barrier |
| 4 | Closure capture at creation | closure is itself young at creation ⇒ never an old→young edge at capture time. Cell REASSIGNMENT is case 3 |
| 5 | Promise fields (`result`, `reactions`) — resolving an old promise with a young value, `.then` on an old promise with a young callback | all through VM helpers (async_runtime.rs); no JIT path. 1 test each |
| 6 | Map/Set/WeakMap/WeakSet inserts, Generator/AsyncState saved `regs`, Bound/Boxed/Proxy/Iterator internals | all VM-side helper writes; generator reg save happens at suspend (VM code). 1 test at each site, ~a dozen sites |
| 7 | **VM side tables** (`arr_props`, `fn_props`, `private_fields`, `proto_of`, `super_this`, `dispose_*`, `regexp_result_props`, microtasks, frames, regs, globals — everything gc.rs:121-423 roots) | scattered across the whole VM | **don't barrier them at all: RE-SCAN them as roots at every minor GC.** The major GC already enumerates them wholesale; B84's numbers price that pass inside "trace" at ≤22ms worst-row. Registers/globals/frames must be rescanned anyway |
| 8 | `Heap::replace` (super() in-place swap, heap.rs:2327-2333) and `set_proto` | already version-bumping chokepoints | add the test where the bump already is |

**Can the barrier ride the version-bump machinery?** Mostly NO. `bump_version`
(heap.rs:2342) fires only on key-ADD / `vals` reallocation / slot free+reuse — an
overwrite of an existing property slot (the common store, and the entire JIT IC hit
path) bumps nothing by design. The bump sites (key-add, replace, free) can carry the
barrier for free, but they are the rare stores. The common store needs its own test.

**Cheapest barrier form.** Because identity = index, "young" is testable without
touching the object: keep `young: Vec<bool>` parallel to `objs` (B28: byte loads beat
bit shift/mask here — use bytes, not a bitmap). The barrier is:

    if young[receiver] == 0 && value.is_heap() && young[value.idx] { remset.push(receiver) }

Cheaper filtered form for the JIT hit path (emitted, ~4 instrs on the store path):
test `young_ptr[recv_idx]`; if old, unconditionally `remset.push(recv_idx)` (skip the
value test — a *card/dirty-object* set, deduped lazily at minor GC by re-tracing the
dirty object's full edge list). That is: 1 byte load, 1 test, 1 forward-taken jz over a
2-instr buffer append. On B99/B101's read that regions are 10-25% heap ops and most of
those are GETS, the emitted-store barrier tax is plausibly 0.3-1% on the JIT-store-heavy
rows and ~0 elsewhere. `r13`/`r14` are already pinned bases (emit.rs comment) — the
young-bytes base and remset cursor need two more pinned values or a Vm-relative load.

Remset representation: unbounded `Vec<u32>` of dirty OLD objects (object-grain, not
field-grain), deduped by a per-object "in remset" bit stolen from the same `young` byte
(0=young,1=old-clean,2=old-dirty). Minor trace = roots (case 7 rescan) + re-trace every
dirty old object's edges with `trace_edges` unchanged, pushing only young indices.

## 2. Recommended design — non-moving nursery by index state ("Option 2")

- `alloc` pushes `idx` to `alloc_log` and sets `young[idx]=YOUNG`. No dedicated index
  range: young objects live wherever `alloc` put them (free-list slot or fresh push).
  A dedicated range was considered and rejected: the free list already recycles the
  SAME hot slots every cycle, which is the locality the B25/B117 evidence actually
  rewards (see §5), and a separate range would fight the `objs.len()/2` sweep floor.
- **Minor GC** (triggered every `YOUNG_THRESHOLD` allocs, e.g. 16-64k, swept
  empirically): mark young objects reachable from {registers/globals/frames/side-table
  values (full rescan, case 7)} ∪ {edges of remset-dirty old objects}. Trace stops at
  old objects (they are treated as marked). **Sweep walks only `alloc_log`**, not
  `floor..n`. Freed slots: `free_slot` as today (version bump ⇒ every stale IC misses —
  the existing machinery, heap.rs:2253, already guarantees this) **plus per-slot
  `remove(idx)` on the ~36 side tables** instead of today's 36 whole-map `retain`
  passes (gc.rs:442-509) — O(swept×36) O(1)-removes vs O(total-entries×36); required
  for correctness (a recycled slot must not inherit `this_tdz`/frozen-length/brand
  state, gc.rs:444-459). Survivors: promote immediately (`young[idx]=OLD-CLEAN`,
  drop from log). No age counter in v1 — B84 says the retained-tree rows survive ~100%
  and the churn rows survive ~0%, so a 2-cycle age buys little and costs a second log.
- **Major GC** = today's `gc()` unchanged (plus clearing remset/young state), triggered
  by the existing `GC_GROWTH=3` live-set schedule.
- **Safe points unchanged** — and note B117's standing warning: any new call fast path
  must keep `maybe_gc` reachable; the nursery makes starvation WORSE (young log grows
  unboundedly), so the minor trigger must live in `Heap::alloc`'s existing
  `gc_requested` flag, which it does for free.

**Does this dodge every raw-pointer hazard?** Yes, by construction: nothing moves,
`vals_ptr`/pinned snapshots/`SlotTable` keys/`jit_const_strings` are all
index-or-address stable exactly as today; the only JIT-visible change is the emitted
store barrier, and IC staleness is still handled by the untouched version machinery.

**Locality.** Objects never move, so there is no compaction win — but there was never
going to be one: the B117 37x-balloon cost and B25's "GC off made `{}` 35→64ns" are
about *heap footprint and slot reuse rate*, not object order. A minor GC every 16-64k
allocs recycles dead slots ~4-40x MORE often than today's 65k-196k-alloc major cycle,
holding the churn working set in a smaller, cache-resident set of slots. This is the
one place the design may beat its own GC-time ceiling — and also the risk that the
`objs.len()/2` interaction re-runs B25's crossover; it must be swept, not reasoned.

## 3. Sticky-mark-bit alternative ("Option 3")

Persist the mark array in `Heap` (today it's a per-GC `vec![false; n]`, gc.rs:80);
minor = trace from roots+remset through UNMARKED objects only, sweep = walk the alloc
log for still-unmarked entries. This is Option 2 with `marks` playing the role of
`young` (marked ⇔ old). Cost/benefit vs Option 2:

- Saves: the separate `young` byte array and the `vec![false; n]` zeroing per major.
- Costs: the mark array becomes mutable engine state the JIT barrier writes into
  (same instruction count); majors must flip interpretation or re-zero anyway; and the
  "in-remset" dedup bit still needs a second state, landing you at the same 3-state
  byte. Functionally the designs converge. **Verdict: same design, worse names. Build
  Option 2's explicit 3-state byte; it IS the sticky mark bit.**

## 4. Pretenuring

B84's pool refutation reason: `json-large` RETAINS its tree (54,390 shapes; pool stayed
empty, row +2.9%); markdown retains its component tree. Under a nursery those rows'
allocations survive every minor GC — pure barrier+log+promotion tax, no reclaim.

- **Which sites:** JSON.parse's internal builders (helpers_json.rs), the tokenizer/
  parser AST allocs on parse-large-js, and any user site with measured survival ≥ ~80%.
- **How the engine knows:** the alloc log makes site-blame nearly free — log
  `(idx, site_id)` where `site_id` = (func,ip) for `NewObject/NewArray` bytecodes and a
  static id for builtin allocators. At each minor GC, credit survivors to their site;
  a site whose survival crosses a threshold over 2 consecutive minors allocates
  OLD-CLEAN directly (skip log, skip young byte). Builtins (JSON.parse) can be
  statically pretenured in v1. Standard V8-style dynamic pretenuring, but cheaper here
  because promotion is free — a WRONG pretenure costs only "waits for a major", never
  a copy. That asymmetry means the threshold can be aggressive.

## 5. Costing, per B101's method (per-row, from B84/B25/B81 measured numbers)

Inputs: B84 GCSTATS (collections/trace/sweep/retain, roadmap:3686-3698), B25 phase
split, B84's "GC is 2-12%" corroborated by the profiler (roadmap:3660-3667).
Model: minor GCs remove (sweep over old slots) + (trace of old live set) + (retain
passes) between majors, and cut majors ~3-5x in count; keep = young trace+sweep,
barrier tax (−0.3..−1% on JIT-store rows), log push (~1.5ns × allocs), per-slot
side-table removes. Survival: json/markdown/parse ≈ high (pretenure required to not
LOSE), regex corpus/matchAll, polymorphic, map-set churn ≈ low.

| row | GC today (B84/B25) | of which removable | expected row Δ (best design, with pretenure) |
|---|---|---|---|
| regex-log-scan | 185.7ms/1592 = 12% (trace 53, sweep 97, retain 28) | sweep+retain almost fully (dead corpus lines), trace mostly (150k retained lines stop being retraced per major) | **−6 to −9%** |
| json-large | 36.5/445 = 8% (trace 22, sweep 14; only 7 collections) | trace of the retained tree at each of 7 majors → fewer majors; sweep small; HIGH survival ⇒ pretenure-dependent | −3 to −6% (could be **+1-2% WITHOUT pretenuring** — B84's pool run 2 is the precedent) |
| markdown-render | 22.8/455 = 5% | as json | −2 to −4% |
| async-promise-chain | ~9% (B25, 67/723, pre-GROWTH-3; now less) | promises die young; sweep-heavy (B25: sweep 49 of 55) | −3 to −5% |
| polymorphic-objects | 13.3/593 = 2% (sweep 12.8) | sweep, fully | −1 to −2% |
| map-set-heavy | ~9% (B25 79/869, pre-GROWTH-3) | entries retained; map churn young | −1 to −3% (B19's +10.8% retained-buffer failure mode watches this row) |
| sparse-array | GC minor share (profiler: gc small) | little | 0 to −1% |
| class-prototype-hot | 0% (B25: 0ms) | none | 0 |
| parse-large-js | 5/599 = 1%; 99.2% time executing JS (B84) | none reachable | 0 (pretenure the AST or pay tax) |
| typedarray-math | ~0 (allocates nothing, B81) | none | 0 |

**Geomean: −1.5% to −3%, center ~−2%,** minus 0.3-0.7% aggregate barrier/log tax if
the emitted barrier lands sloppy ⇒ honest headline **−1 to −2.5%**, with an unpriced
locality upside (slot-reuse cadence, §2) that could add ~0.5-1% on churn rows — or
subtract it, per the B25 crossover.

**The skeptical paragraph the file would write itself:** B6's reputation ("allocation
is 10-50x node, the nursery fixes it") does not survive its own citations. B81's 49x
gap is CONSTRUCTION — `Box<ObjMap>` + key `String`s + three Vec first-pushes — and B81
itself measured the malloc at ~3-7ns of a 31-148ns object; a non-moving nursery
changes NONE of that (only V8's bump-alloc-with-inline-properties does, which is a
heap-representation project, not a collector project). The part a nursery addresses is
B81's +48ns/alloc live-set-scaling term, and B84 proved the SUITE never reaches the
live sets where it bites (avg live 1,378-78,160). What's left is the measured 2-12%
GC share. A −1 to −2.5% geomean is a real win at today's 1.7492x, comparable to one
mid-sized B10 item — but it is an L-effort, highest-correctness-risk-class item
(a missed barrier is silent heap corruption that only `ZIPP_GC_STRESS`+fuzz catches),
and it should be prioritized as such, i.e. behind M4-class work, exactly where B84
already put it ("worth ~12% of the worst row; not the headline").

## 6. Measurement plan

**Decision experiment first (the cheapest falsifier, B6.0-style, effort S):**
a stats-only patch, no behavior change: extend GCSTATS with a watermark/alloc-log
oracle that, at every existing collection, splits marked and swept objects into
"allocated since previous GC" vs older, and counts would-be old→young stores by
incrementing a counter in the ~8 helper chokepoints of §1 (no remset, no emitted
code — JIT-hit stores estimated from `ZIPP_NOJIT=1` delta). Outputs per row:
young survival %, old-attributable trace+sweep share (= the exact ceiling of ANY
generational split), and barrier-site frequency (= the tax numerator).
**Kill rule: if Σ(old-attributable GC share × row weight) < ~1.5% geomean, or churn
rows' survival >30% without an obvious pretenure site, B6 closes as REFUTED at
measurement cost.**

**Off-switch & stats the build must carry:** `ZIPP_NO_NURSERY=1` (single flag reverting
to today's collector — barrier compiled in but remset ignored, so the flag also
measures pure barrier tax); GCSTATS gains minors/majors, remset peak, promoted,
survival %, per-site pretenure table (`ZIPP_GCSTATS=1`); `ZIPP_GC_STRESS` must force
minor at every safe point AND a major every N minors; `ZIPP_SHAPE_VERIFY` runs on both.

**Staging (each behind the flag, each suite-gated per §14):**
1. Oracle instrumentation (above). Decides go/no-go with numbers.
2. **Barrier-free minor**: alloc log + young-only SWEEP with a FULL mark (trace
   unchanged). Sound with zero barriers — full marking finds all live young. Kills the
   sweep+retain terms only (regex ~125ms of 185; polymorphic ~13; B25 says sweep
   dominates), i.e. roughly half the total prize for a quarter of the risk. Also
   forces the side-table per-slot-remove machinery to exist and soak.
3. Write barriers + remset + young-only TRACE (the §1/§2 design). The JIT emitted
   barrier lands last within this stage, helpers first (`ZIPP_NOJIT` validates the
   scheme before a byte of dynasm changes).
4. Pretenuring: static (JSON.parse/parser) first, site-sampled dynamic after —
   gated specifically on json-large/markdown not regressing in stage 3.

Biggest risk: an unenumerated old→young store (the §1 table claims completeness
against trace_edges' arm list, but §1 case 6's "a dozen VM sites" is where the audit
lives or dies), surfacing as rare corruption. Mitigation is the stress mode in both
directions plus stage 2 shipping first, since it has no barrier soundness surface.
Second risk: B19/B84's precedent that retained-workload rows PAY for churn-side
mechanisms — pretenure or the flag stays on.
