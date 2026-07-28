# zipp-vm — roadmap

> **Goal:** match V8 on speed, and pass all of test262.
>
> Engine: `crates/zipp-vm/src` — a NaN-boxed, explicit-frame register VM with
> per-call-site inline caches, a native x86-64 OSR JIT (dynasm), and a
> whole-heap mark-sweep GC.
>
> Last re-measured **2026-07-28**. Every number below was measured on this repo;
> nothing here is an estimate unless it says "inferred".
>
> **Sections 1 and 3 below were written at 3.31x and several of their
> conclusions have since been REFUTED by measurement.** The corrections live in
> B8 (the regex engine is not the bottleneck — we beat V8 at scanning), B17 (key
> interning and inline storage both measured slower), and B16 (which loops the
> JIT never reaches). Read those before acting on anything here.

## Current experiment registry — 2026-07-28

This is the short, current view. Detailed measurements and retained raw results
are in B58.

| item | current disposition | measured result |
|---|---|---|
| M0.1 counterbalanced harness | **IMPLEMENTED; A/A DRIFT OPEN** | paired AB/BA observations, raw schedules, bootstrap intervals, metadata, timeouts, and schema-v1/v2 reading; an A/A regex rerun reversed from −0.4% to +1.1% with both nominal CIs excluding zero, so ~1% claims still require independent replication |
| M1.1 compiler global lookup | **IMPLEMENTED IN WORKTREE** | 3k/6k/12k/24k generated-function sweep stays approximately linear; largest/middle ns-per-MB ratio 0.975 |
| M1.2 expression-arrow analysis | **IMPLEMENTED IN WORKTREE** | analysis consumes the expression directly; capture/`this`/`arguments`/`super`/`await` tests pass |
| M1.3 first-way shape `SetProp` | **IMPLEMENTED IN WORKTREE** | NOJIT own-store microbenchmark −46.66% (95% CI −47.80% to −45.51%); removing it was +0.52% on four affected suite rows (95% CI −0.66% to +1.53%) |
| M2 regular-subset regex tier | **EXPERIMENTAL, OFF BY DEFAULT** | regex row −2.82% (95% CI −3.8% to −2.0%), far below the 25% promotion gate; feature binary +14.7% |
| default regex capture-name clone removal | **REVERTED** | restoring the original code measured −0.51% (95% CI −1.05% to −0.24%), inside the independently observed ~1% A/A drift floor |
| M4.0 TypedArray guard reduction | **REVERTED** | −0.11% (95% CI −1.10% to +0.55%): statistically neutral |
| M3-M5 object metadata, CFG/SSA, arena/nursery | **OPEN** | these remain the required architectural path toward broad V8 parity |

---

## 1. Where the project actually is

### Conformance — 99.97% test262, 96.9% intl402

| slice | executions | pass | fail |
|---|---|---|---|
| ECMA-262 + `staging`, sloppy **and** strict | 95,846 | 95,816 (99.97%) | 30 |
| `intl402` (opt-in) | 6,682 | 6,474 (96.9%) | 208 |

Both tiers (`ZIPP_NOJIT=1` and JIT) produce a **byte-identical** failure set.

The main-suite denominator dropped by 2 (95,848 → 95,846) when a leftover
scratch file from a crashed sweep was deleted from the test262 checkout. It
predated the `.zipptmp-` prefix the walk now skips, and being a harness+test
concatenation it ran and scored as a pass — the exact phantom the prefix exists
to prevent, still present from before the prefix existed.

Of the 30, **only 6 are engine defects** (`matchAll` @@match lookups 2, `en`-only
CLDR 2, Annex B `arguments` 2). 19 are this repo's runner making the *harness*
strict — `INTERPRETING.md` puts the directive on the test file and evaluates
`includes` as separate sloppy scripts. On a conformant assembly both engines
pass all 19; on the bytes this runner emits zipp passes 0 and V8 passes 3. 3
more are a Windows `core.autocrlf` checkout inflating `import-bytes` fixtures,
and 2 are an upstream test that predates immutable `ArrayBuffer`. Fixing the
runner and the checkout is worth 22 executions and no engine work.

One genuine defect surfaced while proving that, and it is invisible to the
current runner: inside a **strict** `$262.evalScript`, assigning to a `var`
declared without an initializer throws `ReferenceError`, though the binding
exists (`"x" in globalThis` is true), the sloppy form works, and an initialised
`var x = 5` reads back fine. Not in the 30; worth fixing before the runner is
corrected, since a conformant runner reaches it.

The intl402 denominator changed because the runner was not parsing YAML
list-form `flags:` — roughly half that suite silently never ran, so the old
16.9% was measured against a half-skipped suite.

What is left, and it is a different SHAPE from the long tail this section used
to describe. Decorators — once the largest single item — are implemented. So
are all fifteen Temporal calendars and the IANA time zone database. At 30
executions the residue is small enough to classify exhaustively rather than
estimate, so these are counts, not `~` figures, each reproduced against V8:

| cause | executions | note |
|---|---|---|
| runner makes the harness strict | 19 | runner's assembly: zipp 0/19, V8 3/19. `INTERPRETING.md` assembly: **both 19/19** |
| Windows `core.autocrlf` checkout | 3 | `import-bytes` fixtures inflated LF→CRLF; normalise them and all 5 pass |
| upstream test predates the feature | 2 | `TypedArray` slice species vs immutable `ArrayBuffer`; zipp passes the sibling test asserting the throw |
| **fixable engine bugs** | **6** | `matchAll` @@match lookups 2, `en`-only CLDR 2, Annex B `arguments` 2 |

The previous version of this table estimated ~45 fixable engine bugs and claimed
~10 executions where "zipp is right, node is wrong" — naming Annex B `arguments`
first. That claim was **false**, and it survived because it was never re-checked
against the spec text. `paramNames` has not been mutated to contain `"arguments"`
since ES2018 (a separate `paramBindings` list carries it), so Annex B's guard
does not fire, and the `SetMutableBinding` at block-declaration evaluation always
runs. node is right; zipp is wrong. The test262 test zipp *passes* here quotes
the removed ES2017 step, which is why V8 fails it. This is the fourth time an
entry in this file was refuted by re-measurement, and the first where the
refuted entry was a claim of superiority.

The intl402 remainder is data, not logic: CLDR content (patterns, unit display
names with plural selection, collation order, plural categories) and the
Unicode algorithm data behind `Segmenter` (UAX #29) and `Collator` (UCA/DUCET).
Where a table IS carried it comes from the real upstream source with recorded
provenance and is verified value-by-value against node's ICU — that is the
standard, and approximating it would be worse than the honest failure.

`tools/test262-expected-failures.txt` is the checked-in baseline; a regression
is a `diff`, not a remembered number. It was stale for a long stretch (the
2,194-line oxc-era list against a 938-failure run), which made that diff
meaningless — regenerate it in the same commit that moves the number.

### Performance — cold geomean 1.90×; historical adjusted geomean 2.15×

`bench/real/*.js` via the schema-v2 `tools/bench.py`, 15 counterbalanced paired
observations, exact-byte output comparison. These are cold total medians and
paired ratios from `bench/final_default_2026-07-28.json`.
The cold suite-level 95% paired-bootstrap interval is 1.87×-1.92×.

| bench | node | zipp | cold paired ratio |
|---|---|---|---|
| map-set-heavy | 923ms | 818ms | 0.90× |
| class-prototype-hot | 299ms | 381ms | 1.27× |
| markdown-render | 287ms | 478ms | 1.65× |
| json-large | 300ms | 534ms | 1.77× |
| async-promise-chain | 351ms | 644ms | 1.85× |
| polymorphic-objects | 331ms | 622ms | 1.88× |
| sparse-array | 85ms | 162ms | 1.92× |
| parse-large-js | 280ms | 601ms | 2.16× |
| typedarray-math | 210ms | 650ms | 3.11× |
| regex-log-scan | 467ms | 1890ms | 4.00× |

The repaired harness treats total wall time as the primary cold metric. For
continuity with the old table, subtracting the median empty-process launch gives
a **2.15× historical-adjusted geomean** (Node startup 32.2ms, Zipp startup
7.8ms), effectively confirming the former 2.17× headline from the same run.

Run-to-run variance remains material, so a raw percentage is no longer treated
as evidence on its own: use the retained paired observations and paired-bootstrap
95% interval. The historical adjusted geomean has moved 4.77× → 4.20× → 3.31×
→ 2.82× → 2.72× → 2.56× → 2.29× → 2.17× → **2.15×**. On the current cold
run, map-set-heavy is ahead of Node at 0.90×, while class-prototype-hot is
1.27×, markdown-render 1.65×, and polymorphic-objects 1.88×.

**What it would take to move the adjusted 2.15× result toward parity**, from
the phase-level measurements in B31–B33 — none of these is a tuning change:

1. **Hidden classes / shapes.** The property fast path is keyed to object
   IDENTITY, not shape, with a cliff at exactly `IC_WAYS = 8`: the same
   `{alpha,beta,gamma}` read costs 4.2ns at 8 distinct receivers and 18.9ns at
   16, where node is flat at 0.6ns. This is the only fix for that cliff and for
   the unconditional `pos()` the interpreter IC pays on every access.
2. **A profitable compiled regex backend.** `regress` is a backtracking
   interpreter at 6.9ns per failed match attempt against Irregexp's 0.37ns,
   but B58's conservative regular-subset tier moved the complete row only 2.82%
   and therefore remains off by default.
3. **An optimizing tier with SSA.** `typedarray-math`'s DataView phase and its
   prefix-sum phase are both op-count bound on a register machine with no such
   tier (B32, B7) — prefix-sum is already on the BEST tier and is still 3.4×.

Startup is ~4× faster than node (7.8ms vs 32.2ms).

### What the engine already wins

Scalar-numeric kernels, self-recursive integer functions, `s += …` string
accumulation, `charCodeAt`/`s[i]` scan loops (measured at 1.7 ns/op — parity),
and non-capturing arithmetic array pipelines all compile to native code with no
per-element call. None of this carries to the ten benches above, which are
bound by property access, allocation and enumeration.

---

## 1b. Benchmark-harness debts (found by external review, VERIFIED)

Both of these are confirmed in the source, and both mean a headline number is
measuring something other than its name:

- **`parse-large-js` does not measure zipp parsing.** Its own header says it
  builds ~2MB of synthetic source and then "tokenize[s] it with a hand-written
  charCodeAt scanner" plus a recursive-descent parser written IN JAVASCRIPT. It
  is a fine JS-execution benchmark; it is not a parser benchmark, and no result
  from it says anything about `oxc_parser` or our bytecode emitter.
- **`polymorphic-objects` never reaches the IC cliff.** It indexes
  `shapes[i & 7]` — exactly 8 receivers — and `JIT_IC_WAYS == 8`. It therefore
  sits precisely AT capacity and never exercises the ninth-receiver fall-off
  that §3 blames it for. Any claim about megamorphic behaviour needs a 9th
  shape.

M0.1 resolved the raw-sample, engine-order, paired-startup, median/interval, and
metadata debts. Still open are persistent warm execution, compile/JIT/GC phase
timing, RSS, and the coverage corrections above. A small change now needs its
paired confidence interval rather than hand-repeated best-of-N timing.

## 2. The standing gate

Every engine change must pass, in full:

1. **Build:** `cargo build --release` — verify the binary mtime advanced.
2. **test262, BOTH tiers:** `tools/run_test262.py --dump-fails f.txt`, then
   `diff <(sort f.txt) <(sort tools/test262-expected-failures.txt)` — zero new
   entries. Repeat with `ZIPP_NOJIT=1`.
3. **Unit tests:** `cargo test --workspace --release`. Check the summed pass
   count and every ignored test; do not rely on one package's summary.
4. **Bench correctness:** `bash bench/run_real.sh` → `ALL_CORRECT=1`, default
   **and** `ZIPP_NOJIT=1`.
5. **GC stress:** add `ZIPP_GC_STRESS=1` when the change touches GC/heap —
   mandatory for anything in Track B3/B4/B6.

Any change touching the JIT must produce identical output with and without it;
`assert_jit_matches` in `crates/zipp-vm/src/lib.rs` pins that per case, and new
JIT work is expected to add cases there.

**Measurement protocol.** Use `tools/bench.py --ab old.exe new.exe` and retain
its schema-v2 JSON. A change expected under 10% needs at least 15
counterbalanced pairs (21 for a marginal decision), a paired-bootstrap 95%
interval, exact output, and the full-suite regression check.

**Heavy-codegen discipline.** Develop behind an opt-in env flag, flip the default
last, only after the full gate is green across several milestones.

---

## 3. What the measurements say the gap is

Isolated microbenchmarks (`zipp`, absolute ns/op — V8's own figures for these
are low-nanosecond, but its loops are partly dead-code-eliminated, so treat
them as a floor rather than a target):

| operation | zipp | was (system alloc) | where it hurts |
|---|---|---|---|
| add a dynamic key | **197 ns** | 530 ns | polymorphic |
| build a 4-property object literal | **170 ns** | 513 ns | json, parse, poly, class |
| `str.indexOf` on an 880-char string | **137 ns** | 240 ns | markdown, parse |
| `for-in` per key | **72 ns** | 180 ns | json (25% of `walk`), sparse, poly |
| `Object.keys` per key | **53 ns** | 127 ns | json, markdown |
| property read `o.a` | **17 ns** | 41 ns | everything |
| `s.charCodeAt(i)` in a JIT'd loop | **1.7 ns** | 1.7 ns | already at parity |

The object-construction ladder that found the allocator (per-property marginal
cost, so the first row includes the three `Vec` first-allocations):

| | system alloc | mimalloc |
|---|---|---|
| `{}` | 26 ns | 26 ns |
| `{a}` | 316 ns | **88 ns** |
| `{a..d}` | 434 ns | **134 ns** |
| `{a..h}` | 896 ns | **278 ns** |

### Where the remaining gap actually is

Decomposed by absolute milliseconds behind node, not by ratio — a 10x bench that
takes 50ms matters less than a 3x bench that takes 900ms:

| bench | gap | share |
|---|---|---|
| regex-log-scan | 2943ms | **41.8%** |
| markdown-render | 706ms | 10.0% |
| typedarray-math | 526ms | 7.5% |
| class-prototype-hot | 517ms | 7.3% |
| polymorphic-objects | 508ms | 7.2% |
| async-promise-chain | 461ms | 6.5% |
| parse-large-js | 455ms | 6.5% |
| map-set-heavy | 370ms | 5.3% |
| json-large | 324ms | 4.6% |
| sparse-array | 234ms | 3.3% |

**REFUTED — see B8.** This section claimed regex was 41.8% of the gap and
that the MATCHER was at fault. Matching cost is flat in subject length and zipp
is FASTER than V8 on a 2000-char scan (25ms vs 42ms); the cost is per-call
dispatch and result construction. The paragraph below is kept for the history of
how the wrong conclusion was reached.

**Superseded claim:**
Measured per call:

| | zipp | node |
|---|---|---|
| `/ERROR/.test('')` — pure call overhead | 98 ns | 8 ns |
| `.test()` on a 200-char miss | 110 ns | 14 ns |
| `.test()` on a 200-char hit at the end | 120 ns | 24 ns |
| `.exec()` with 4 capture groups | 765 ns | 50 ns |

Scanning 200 characters costs 12 ns, so the byte path, the `ascii_twin`
byteopt compile and memchr prefiltering are all working. Fixed per-call
overhead is ~90 ns excess, which over the bench's ~1.2M regex operations is
only ~110 ms of the 2943 ms. The rest is the backtracking VM executing capture
groups: ~277 ns of matching where V8's Irregexp — which compiles the pattern to
native code — takes ~22 ns. **This corrects two earlier claims:** that regex was
mostly result-object construction (it is ~270 ns fixed, real but not dominant),
and that a lazy-result change (old B5.2) was the lever. It is not; the engine is.

**Property access is boxing-bound, not cache-bound.** The 8-way identity-keyed
IC does have a hard cliff at 9 same-shape receivers — measured 8.5 ns/read at 8
receivers, 20.5 ns at 9, flat thereafter, against node's 1.0 ns at every count.
But neither bench blamed for it reaches the cliff: `class-prototype-hot` cycles
4 receivers and `polymorphic-objects` uses `shapes[i & 7]`, i.e. 8. The number
that matters is that a **hitting** IC still costs 8.5 ns against node's 1.0 ns.
That gap is the per-operation NaN-box/tag-guard tax, and only an optimizing
tier that keeps values unboxed across operations (B7) removes it — shapes (B3)
would flatten the cliff but not the 8.5 ns.

Three architectural causes, in order of cost:

1. **No shared hidden classes.** `heap.rs` — every object owns
   `ObjMap { keys: Vec<String>, vals: Vec<Value>, attrs: Vec<PropAttr> }`, and
   `ObjMap::set` does `self.keys.push(key.to_string())`: a fresh `String` malloc
   per property per object. This is the dominant term in five of the ten
   benches, and the reason both object construction and enumeration are
   ~100× off.
2. **JIT inline caches key on receiver identity, not shape.**
   `codegen/mod.rs` — `IcEntry { obj_bits, vals_ptr, version, … }` probed by
   comparing the receiver's bits, `JIT_IC_WAYS = 8` with round-robin eviction.
   Nine same-shape instances thrash a cache V8 keeps flat.
3. **JIT regions decline allocation.** `codegen/region_admit.rs` —
   `NewObject`/`NewArray`/`MakeClosure`/`MakeCell` are not admitted, so one
   object literal anywhere in a hot loop keeps the whole loop interpreted.

**Honest floors.** Fixing 1–3 is a multi-month program. Even completed, the
per-bench floors are ~1.8–2.5× for the property/alloc-bound benches; only
`typedarray-math` (~1.0–1.2×) and `map-set-heavy` (~1.4×, already close) are
true parity candidates without further work. `regex-log-scan` has a ~5× floor
until the regex engine is replaced. **Do not chase 1.0× on the others before
the substrate exists.**

**Lesson from the last campaign, worth repeating.** Eighteen commits of
contained fast-path work moved the geomean 4.77× → 4.20× (~12%), and two
fully-implemented, gate-green epics (tombstone delete; hole-`in` fast path)
were **reverted for zero measurable gain** because the attribution had been
read from code rather than measured. Measure the section you intend to fix,
with a definitive A/B, before building anything large.

---

## 4. Track A — conformance to 100% test262

This track did not exist in the previous roadmap; test262 was only a gate. It is
now the shorter of the two tracks and should go first — the work is bounded and
the payoff is a headline number.

**Status 2026-07-29: 938 → 30 failures (99.0% → 99.97%), and intl402 2,778 →
208 (16.9% → 96.9%).** Every step gated against the checked-in baseline with
zero regressions; the 2026-07-29 run reproduced the 30-failure baseline exactly,
on both tiers. Of those 30, 24 are not engine defects at all (19 runner, 3
checkout, 2 upstream test) — so the remaining engine work in this track is 6
executions, and the largest single lever left is fixing the runner. What the
work actually taught, beyond the number:

* **Cluster, then fix.** The wins came from root causes, not assertions. One
  sentence — "nothing created inside a child realm carried that realm's
  intrinsics" — explained ten files. 79% of intl402 was a single defect:
  Temporal rejecting every non-ISO calendar.
* **Re-diagnose rather than trust a stale list.** Roughly one diagnosis in four
  turned out wrong or already fixed. Two recorded blockers were simply false —
  `Intl.DateTimeFormat` was never unconstructable — and a cluster estimated at
  10 executions delivered 0. Re-classify at each new failure count; a list
  written at 396 failures is worthless at 151.
* **node is a good oracle, not an authority — but "node is wrong" is a claim,
  and claims get checked.** Three divergences are deliberate and proved: the
  ICU4C hebrew calendar, a V8 `DurationFormat` 2^53 bound, and the chinese
  calendar (where test262's own expectations back zipp against ICU). A fourth,
  Annex B `arguments`, was carried here for months and is simply **wrong** —
  see §1. It was believed because zipp passed a test262 test that node fails,
  which felt like proof; the test encodes ES2017 wording removed in ES2018. A
  passing test is evidence about the test as much as about the engine. When the
  divergence is "we are more correct than V8", read the current spec text before
  writing it down — that is the one direction where nobody files a bug against
  you.
* **Ship the semantics or ship the honest error.** Decorators sat at a flat
  SyntaxError for a long time, and that was correct: a parser that accepts
  `@dec` and drops the semantics turns a missing feature into silently wrong
  user code. They landed only once the runtime existed.
* **The prerequisite was the real cost.** Decorators looked expensive because
  they needed stateful native callables and every stateful native was its own
  `HeapObj` variant. Building `HeapObj::NativeClosure` as a GENERAL mechanism
  cost one audit of 42 match sites — 39 of them `_` catch-alls the compiler
  would never have flagged — and now any stateful builtin is one match arm.

- [x] **A1 — Static-semantics early errors. DONE: 2,214 → 135.** Not by adding
  `oxc_semantic` as this entry proposed, but by the engine growing its own front
  end (`src/parse/`), which is the only arrangement that has the binding,
  strictness and positional state these rules need *while parsing*. The rules
  that took the longest are listed in the README's front-end section; the
  clusters that fell were Annex B function-declaration positions, ClassBody
  static semantics (private-name duplicates, `#constructor`, static
  `prototype`, special-method `constructor`), `UniqueFormalParameters`,
  parameter-vs-body lexical collisions, object-shorthand IdentifierReferences,
  escaped `yield`/`await`, `ContainsArguments`, block-scoped `var`/`let`,
  module-goal early errors, and three lexer rules (`0_0`, `""` under strict,
  HTML-like comments in a module).
  Remaining: 135 executions across a long tail, of which 20 are the deliberate
  top-level-`return` trade and 26 are decorators (unimplemented).

- [ ] **A2 — `Intl.DateTimeFormat` is unconstructable.** `vm/intl.rs:436` reads
  each component option with `opt_string(options, name, "", allowed)`, passing
  the empty default into `unit_allowed`, which rejects it — so *every*
  `new Intl.DateTimeFormat(...)` throws
  `RangeError: Value  out of range for option weekday`, including
  `resolvedOptions()`. Read the option, and validate only when it was present.
  The same shape breaks `Intl.DisplayNames` (`:556-566`). ~222 direct
  intl402 failures plus cascades. **Effort:** S. **Risk:** lo.

- [ ] **A3 — Non-ISO calendars for Temporal/Intl.** ~1,400 intl402 failures are
  `RangeError: invalid calendar "<gregory|chinese|hebrew|islamic-*|…>"`.
  **Effort:** XL. **Risk:** med. Schedule after A1/A2; it is the long pole of
  intl402 and does not block ECMA-262.

- [ ] **A4 — `name`/`length` own-property descriptors on Intl methods.** 78
  failures ("name should be an own property"). **Effort:** S.

- [ ] **A5 — `$262.createRealm()` builds an incomplete TypedArray prototype
  chain.** A cross-realm TA's chain is `OtherUint8Array.prototype ->
  Object.prototype`, missing the `%TypedArray%.prototype` level that carries
  `length`/`buffer`/`byteLength`/`byteOffset`/`@@toStringTag`. This is why the
  spec-correct lookup in `vm/props/member.rs` is currently disabled (see §6).
  **Effort:** M. **Unblocks:** the deviation in §6.

- [ ] **A6 — `staging` failures (~430).** SpiderMonkey-derived tests; triage for
  genuine engine bugs rather than treating the directory as a unit.

---

## 5. Track B — performance to V8 parity

Ordered by measured impact per unit of effort, not by the original stage
numbering. B1 and B2 are the ones the measurements actually support.

### B1 — Property-name interning (do this first)

- [ ] **B1.1 Intern property names to a `u32` id.** Replace
  `ObjMap.keys: Vec<String>` with `Vec<NameId>` plus a crate-global interner.
  Removes the per-property `String` malloc on every object construction and
  turns every key comparison into a `u32` compare. ~122 sites touch `.keys`,
  55 of them iterating or indexing it directly — this is the whole cost of the
  change, and it is mechanical.
  **Gain:** should remove ~100 of the ~128 ns/property construction cost;
  helps json / parse / class / polymorphic / markdown simultaneously.
  **Effort:** L. **Risk:** med. **Gate:** `ZIPP_GC_STRESS` mandatory.
- [ ] **B1.2 Re-measure** the microbenchmark table in §3 and the ten benches
  before starting B2. B1 alone may change the ranking.

### B2 — `for-in` / enumeration cache

- [ ] **B2.1 Memoize the enumeration key vector** in
  `vm/props/enumerate.rs` (`for_in_keys`), keyed by
  `(heap_idx, heap.version_of(idx))` — the version counter already exists and
  is already bumped on mutation. Today each `for-in` re-walks the own map, the
  prototype chain (including `Object.prototype`, which contributes nothing),
  allocates a `String` clone per key and then a heap string per key.
  **Gain:** 180 ns/key is 25% of json-large's `walk`, ~10% of polymorphic and
  ~34% of sparse-array. **Effort:** M. **Risk:** med — the cache must be
  invalidated by prototype mutation as well as own-map mutation, and cached
  heap strings must be GC-rooted (or cache the *slot plan* and re-alloc the
  strings, which sidesteps rooting entirely — prefer that unless measurement
  says otherwise).

### B3 — Shared hidden classes (the substrate)

Only after B1. This is the months-long item; B1 is deliberately structured to
be the first half of it.

- [ ] **B3.1** `ObjMap` → `{ shape: u32, vals: Vec<Value> }` with a shape
  transition tree. **Effort:** XL. **Risk:** high.
- [ ] **B3.2** Shape-keyed JIT ICs: `IcEntry.obj_bits` → `shape_id`, and the
  probe becomes a shape compare. Removes the 8-receiver cliff.
  **Depends:** B3.1. **Effort:** L.
- [ ] **B3.3** Megamorphic stub cache for sites that exceed the way count.

### B4 — Admit allocation into JIT regions

- [ ] **B4.1** Admit `NewObject`/`NewArray` (then `MakeClosure`/`CellSet`) in
  `codegen/region_admit.rs`, using the GC-safepoint refetch discipline already
  proven for `StrConcat` in `codegen/region_mem.rs`. A dead literal currently
  blacklists an entire loop permanently. **Effort:** L. **Risk:** high (GC
  safepoints inside a region). **Gate:** `ZIPP_GC_STRESS` mandatory.

### B5 — Contained wins, individually gateable

- [ ] **B5.1 Widen the loop-invariant `.length` hoist to live-in registers.**
  `codegen/region_admit.rs` (`hoistable_length`) only hoists when the container
  is loaded by `LoadGlobal`, so a container passed as a **parameter** re-reads
  its length through the miss helper every iteration. Measured on an
  8×1M-element `Float64Array` dot product: 197ms un-hoisted vs 37ms hoisted vs
  11ms with a constant bound. Admitting a register never written in the region
  is sound (the pass already rejects `Call`/`SetIndex`/`SetProp`, so nothing in
  range can change a length). **Effort:** M. **Gain:** measured ~5× on that
  kernel shape.
- [~] **B5.2 Lazy RegExp result objects — REFUTED as a lever, do not schedule.**
  The premise ("exec is ~69% result construction; only ~13% is matcher-bound")
  does not survive measurement. Result construction is ~270 ns fixed per exec;
  the matcher is ~277 ns for a 4-group pattern against Irregexp's ~22 ns, and
  `test()` — which builds no result at all — is already 375 ns vs node's 30 ns.
  A lazy-result change would win a few percent of a bench that is 42% of the
  total gap. The lever is B8, not this. Kept as a note so it is not re-derived.
- [ ] **B5.2b `matchAll` iterator step overhead.** Measured 1.38 µs per match
  through `for-of matchAll` vs 678 ns through an equivalent manual
  `while ((m = re.exec(s)))` loop — so the iterator path costs ~700 ns per match
  ON TOP of the exec it performs. The `{value, done}` object is already skipped
  by the for-of fast path (`vm/dispatch.rs`), so the cost is inside
  `regexp_string_iter_step` and its `get_index(r, 0)` / `to_str_value` /
  double `regexp_string_iters` hash lookups. This is the one contained regex win
  left; unlike B5.2 it is measured against a control. **Effort:** M.
  **Gain:** the bench's matchAll section is 552 ms of ~1276 ms.

- [ ] **B5.3 Builtin method dispatch jump table.** `vm/builtins.rs` and
  `vm/string_ops.rs` resolve `CallMethod` by a chained `match` on `&str`.
  Resolve to a `u16` builtin id at compile time. **Effort:** M. **Gain:**
  the largest single term in markdown-render.
- [ ] **B5.4 `JSON.parse` allocates every key twice.** `vm/mathjson.rs` collects
  `Vec<(String, Value)>` and then calls `map.set(&k, …)`, which does
  `key.to_string()` again. Subsumed by B1.1, but trivial standalone.
  **Effort:** S.

### B6 — Generational nursery GC

- [ ] **B6.0 Measure first.** The previous roadmap asserted ~214 ns/object
  allocation against node's ~10 ns, but the microbenchmark above puts a
  4-property literal at 513 ns total, most of it *construction* (the three
  `Vec` first-allocations plus the key `String`s) rather than collection —
  and a nursery does not fix construction. **B1 is likely to remove more of
  this than a nursery would**, and the mimalloc switch already took the
  4-property literal from 513 ns to 170 ns — i.e. two thirds of what looked
  like "GC pressure" was allocator cost, not collection. Do not start B6 until
  a profiler attributes the remaining cost to GC. **Effort:** S (measurement). **This is a hard gate
  on the rest of B6.**
- [ ] **B6.1+** Moving young-generation collector over a tagged-index heap.
  **Effort:** XL. **Risk:** highest in the document.

### B10 — Measured backlog from the 2026-07-25 agent hunt

25 agents: six subsystem hunts, each finding independently re-measured by a
verifier that wrote its own benchmark and tried to refute it. 213 microbenchmarks,
every row with an in-file control and 2+ runs. Ranked by (impact x confidence) /
effort. **Nothing here clears 5% geomean alone** — that is the honest headline.

**B10.1 — Answer a HOLE inline in the array JIT `HasProp` (biggest single item).**
Phase split of sparse-array (zipp 299ms / node 52ms): the `if (i in holey)` loop
is **147ms vs 12ms = 55% of that bench's entire gap**. Measured 29.1 ns/elem vs
8.75 for the same loop on a packed array (node 0.88). `codegen/region_mem.rs:585`
already answers `true` call-free for a present element but routes every HOLE to
`jit_has_property` -> `has_property_jit` (`vm/values.rs:867`), which walks the
prototype chain and allocates a transient key String. Fix: on a pinned array with
the existing `array_proto_has_index` protector clear and no `arr_props` overlay,
emit `false` inline. **Note this is where the old T0.3 went wrong** — it added the
fast path to the *helper*, measured nothing, and was reverted; the helper is not
the layer that costs. **Effort M. Estimated geomean -4.8% (+/-1.5).**

**B10.2 — Plain-object arms in `jit_get_index` / `jit_set_index`.**
`vm/helpers_misc.rs:264` rejects every string key and `:317` deopts on every
non-Array/Str receiver, so `o[k]` is never JIT-compiled AND four deopts evict the
enclosing region. Dict-shape read 52.8ns JIT / 48.6 NOJIT / node 3.25 — the JIT is
a net loss. Verified eviction sites: json-large `fn7` ip65 is `for (var k in v)
walk(v[k])`; polymorphic `fn0 [132]`/`[167]`. The interpreter already has the
correct path at `vm/indexing_date.rs:66-95` to mirror. **Effort M per part.
~3% geomean for read + write + GetIndexConcat.**

**B10.3 — Admit `CellSet`/`UpvalSet` to the region JIT.** A loop writing a
CAPTURED local costs 26.3-35.0 ns/iter vs 2.67 for the identical loop over a
non-captured local (node shows no penalty). `codegen/region_admit.rs:153-155`
admits the reads and says outright that the writes "keep declining". **These are
markdown-render's ONLY region declines** — `[decline] CellSet at region [16,158]`.
markdown is 3.99x and no other finding produced a lever for it. **Effort M.
Caveat: microbenchmark ratio only, the markdown slice is unsized — size it first.**

**B10.4 — for-of poisons the ENCLOSING region.** for-of 29.6 vs indexed 3.00
ns/elem, but under `ZIPP_NOJIT=1` both cost the same (26.8 vs 26.9) — so the gap
is entirely missing JIT coverage, not iterator-protocol cost.
`codegen/region_admit.rs:197` rejects `GetIterator`/`IterPrime`/`IterNext`;
`GetIterator` runs once per loop entry but sits in the enclosing region, so one
for-of de-JITs the whole nest. Split it: admitting `GetIterator`+`IterPrime` alone
un-poisons the nest and is low risk; `IterNext` + per-iteration
`PushHandler`/`PopHandler` mutate frame state across a mid-region deopt and are
not. **Effort M for the low-risk half. Bench exposure thin — only regex-log-scan
has an enclosed for-of; size it first.**

**B10.5 — for-in snapshot CONSTRUCTION.** 129 ns fixed per loop entry + 38.7 ns
per key (node ~2.5 + ~0.1); `break` after one key on a 32-key object still costs
820 ns. End-to-end ablation: polymorphic 811->737ms (-9.1%), sparse 316->270ms
(-14.6%). **The obvious fix is wrong**: a per-shape enum cache does nothing here
because all three real uses enumerate each object exactly once. Target the
construction instead — `helpers_numeric.rs:233 spec_key_order` allocates two Vecs
and re-parses every key, `enumerate.rs:440` clones each key, `:450` allocates a
heap string, `:377` a heap Array, and ~70 ns/entry is a walk to `Object.prototype`
that finds nothing. **Effort L, ceiling 3% geomean, realistically ~1.5%.**

**B10.6 — Cheap S-effort items.** (a) `ToNum` is simply missing from the
`region_admit.rs` whitelist and declines sparse-array's 54ms for-in phase.
(b) `typeof` allocates its result string every time (`vm/dispatch.rs:890`) though
`vm/access.rs:35` returns a `&'static str`; better still, peephole-fuse
`TypeOf`+`StrictEq(const)` into a tag check. (c) Cache `promise_ctor_value()`'s
per-call `get_prop(promise_proto,"constructor")` (`async_runtime.rs:1560`),
invalidating on writes to `Promise.prototype.constructor` and the global binding.
(d) `emit_misc.rs:57-61` already computes `cvttsd2si` and then does a
`cvtsi2sd`/`ucomisd` round-trip only to detect NaN/Inf/huge — replace with a
single sentinel compare; strictly fewer uops, and integral doubles get faster.
(e) Box the `Combinator` variant of `HeapObj`: 112B -> 96B. Boxing `ObjMap`
instead — the obvious guess — saves **nothing**, `Combinator` is what pins the size.

**Refuted by the hunt, do not re-derive:** async/generator functions are NOT
excluded from the JIT (identical `Math.imul` loops run at 3.00 ns/iter in sync,
async and generator functions alike) — the async gap is entirely the promise
runtime, and the claim to the contrary in earlier notes was wrong. Rewriting
map-set-heavy's for-of as `.next()` loops made it SLOWER (1.33s vs 1.09s).

### B9 — Cold-branch side exits — BUILT, THEN REMOVED (wrong answers)

Do not rebuild this as it was. It shipped opt-in behind `ZIPP_JIT_COLD_EXIT`
after a full green gate — test262 byte-identical across all 96,029 executions on
both tiers, GC stress, and targeted never-taken / sometimes-taken /
always-taken / cold-block-writes-a-value-read-after / early-`continue` cases —
and it was still **wrong**.

Admitting `GetIndexConcat` later let regions containing it reach the tier for the
first time (previously `region_can_compile` rejected them outright, so they never
got there). With cold exits on, this returns `s = 0`:

    let o={},s=0;
    for(let i=0;i<50;i++) o['k'+i]=i*2;
    for(let i=0;i<50;i+=2) delete o['k'+i];
    for(let i=0;i<50;i+=2) o['k'+i]=i*3;
    for(let i=0;i<50;i++) s+=(o['k'+i]||0);   // 3050 everywhere else

Verified as B9's fault, not the new op's: the same program is correct at HEAD
with cold exits on, and correct with the new op and cold exits off. The idea —
one op in a cold block should not demote a region — remains sound and is worth
0 to 4.7x locally on a `charCodeAt` scan whose rare branch slices. But
block-granular exits over a register plan built by SKIPPING those blocks is not
the right mechanism: the plan and the emitted code disagree about what the cold
block does, and the disagreement is invisible until a region shape nobody tested
reaches it.

Retained as a regression test (`fused_concat_key_in_a_branchy_loop`), so the
shape that exposed it is pinned even though the feature is gone.

**Lesson worth more than the feature.** The gate passed. 96,029 test262
executions, both tiers, GC stress and six hand-written shapes did not catch a
wrong answer in a JIT tier, because none of them produced the region shape that
triggers it. For codegen that changes TIER SELECTION, passing the gate is not
evidence of correctness — only of not having found the counterexample yet. That
is also the argument for keeping such work opt-in until something independent
forces new shapes through it, which is exactly what happened here.

### B11 — Region flush soundness — FIXED, and it cost nothing

The exit flush wrote a strictly larger set than the entry prologue loaded.
`plan_region` built the flush set from every homed value (`num_regs`,
`bool_regs`, `globs`) but the entry-load set only from values **read before
written** (`live_in_regs`, `live_in_globs`). A register whose first occurrence in
the region is a *write* therefore got a home that was never initialised — and
`flush_exit` wrote it back anyway.

That is reachable by ordinary code, because OSR entry happens at a back-edge: a
loop with a trip count of exactly `OSR_THRESHOLD` (8) compiles the region on the
final back-edge, enters it, finds the condition already false, runs **zero** body
iterations, and flushes. Same story for a write that sits on an untaken branch,
or any guard that side-exits before the write. Nine shapes, seven wrong:

    var s = 999; for (var i = 0; i < 8; i++) { s = (i * 3) | 0 | 0; }  // 0, want 21
    var s = 999; for (var i = 0; i < 8; i++) { s = i; }                // 8, want 7
    var s = 5;   for (var i = 0; i < 8; i++) { if (i > 100) { s = i; } } // 8, want 5
    var g = 42;  (function(){ for (var i=0;i<8;i++) { g = i*2; } })();   // 4626604192193053000, want 14

The last one is the signature: an uninitialised xmm flushed through the
double-boxing path, so a raw f64 bit pattern surfaced as a JavaScript number.
Bool homes were worse — they live in gprs the prologue never touched at all, so
the flush boxed whatever the register happened to hold into a `Boolean`.

**Fix:** entry-load every home the flush writes, type-guarded exactly like a
live-in (`entry_bail` when the guard fails — sound, because a bail restores
without flushing). `hoisted` constants are the one exclusion; the prologue
materialises them immediately after.

This forced out **home unification** (copy coalescing, `unify_homes_with_globals`
/ `unify_move_homes`). It shares one home between two registers, so the home
cannot be initialised from either register's own frame slot, and an exit before
the alias's def flushes the *other* value — that is exactly why `s = i` returned
`i`. Restoring it needs per-exit flush sets driven by a must-def dataflow over
the region CFG, so an exit only flushes what provably reached its def.

**Measured before removing it, on the two shapes it was written for** (a
global-shuttle accumulator and a move-heavy 8-variable loop, 20M iterations
each): 369ms with unification, 371ms without — best-of-3, i.e. nothing. The four
real benches were likewise unchanged (2242ms vs 2240ms total). So the must-def
dataflow buys back ~0.5% at best and is not worth building; the flag
(`UNIFY_HOMES` in `plan_region.rs`) is left in place documenting why.

A separate crash fell out of the same audit: a `Bool`-typed register stored to a
global asked `xh()` for the xmm home of a gpr-homed register, which is
`unreachable!`. `var b = i < 100;` at top level inside any hot loop **panicked
the engine**. Globals are unconditionally homed as numbers and there is no boxing
path between the two home kinds, so the region now declines to the memory tier.

Six regression tests pin these (`early_exit_flush_*` in `lib.rs`), each
differential (JIT vs interpreter) rather than golden-output.

**Lesson.** B9's was "a green gate is not evidence for tier-selection changes."
This one is narrower and sharper: *the set you restore must cover the set you
spill.* It is a one-line invariant, it was violated for the whole life of the
register tiers, and no amount of test262 caught it — 8-iteration loops whose
result is read afterwards are simply not what a conformance suite is made of.

#### B11b — three more of the same, found by auditing the invariant

Stating the invariant precisely made it worth re-deriving *every* place the
prologue writes state the body might not produce. Three more violations, all
confirmed with repros, all the same missing analysis: **is this op guaranteed to
run?** The planner was answering it with `first_seen == true`, which only means
"the first occurrence in ip order is a def" — it says nothing about reachability.

1. **Hoisted constants** (`plan_region.rs`, both register tiers). A
   `LoadInt`/`LoadConst` on a branch that never runs was still materialised in
   the prologue *and* its body op elided. Doubly wrong: the flush wrote the
   constant over the register's real value, and reads inside the region saw it
   too — so this was also an unsound LICM.

       function f(){ let s=0, c=3;
         for (let i=0;i<200000;i++){ if (i>1e9) { c=7; s+=c; } s+=i; }
         return c; }                                  // returned 7, want 3

2. **Hoisted `arr.length`** (`region_admit.rs::hoistable_length`, memory tier).
   Same shape, worse mechanism: the prologue writes the length *straight into
   the register file*, so no flush is even involved.

       let n=99; for (...) { if (i>1e9) { n = arr.length; } }   // n became 7

3. **Linear-scan home reuse** (`plan_region.rs`). Two registers with
   non-overlapping *in-region* live ranges share one xmm, and `flush_exit`
   writes that xmm to **both** frame slots — so the sharer whose range already
   ended came back holding an unrelated temp. Region-local liveness is not
   function liveness. It also silently defeated the entry-load fix above:
   `live_in_regs` then contains several `(reg, xmm)` pairs sharing one `xmm`, so
   the prologue loads overwrite each other and only the last survives.

**Fixes.** (1) and (2) now require `runs_every_iteration` — no branch in
`[s, def_ip)` may jump past the def *and stay in the region*. Branches that
leave the region are deliberately allowed, and that is the whole trick: OSR
entry only happens after the interpreter has run the loop `OSR_THRESHOLD` times,
so a def that runs every iteration has already written its value to the frame,
and re-materialising it is a no-op. This is the cheap sound approximation of
"the def dominates every exit" and needs no dominator tree.

(3) is disabled (`REUSE_HOMES`), so regions above the 14-home pool fall back to
the memory tier. **Measured cost: 3.6%, on one bench.** typedarray-math 753ms
sound vs 726ms unsound; every other bench was inside ambient noise. Doing better
needs per-exit flush sets — but note this case is *easier* than the general
must-def dataflow that `UNIFY_HOMES` would need, because the live ranges already
exist: at an exit ip, each home has at most one owner (the register whose range
covers it), so the per-exit set is a lookup, not a fixpoint. Worth ~0.4% geomean.

**Tooling that came out of it.** `ZIPP_JITDECLINE=1` now names which of the
planner's ~25 exit points rejected a region, instead of `plan_region=None`.
First census over the ten real benches:

    12  GetIndex/SetIndex (element not a pinned TypedArray)
    10  Bitwise on the double path
     4  Call
     3  CallMethod (receiver not a pinned string)
     2  type conflict on a reused register

The obvious read — "admit plain dense arrays into the register tiers" — was
measured before being built, and is **not worth it**: summing 3M elements six
times costs 90ms through a plain `Array` vs 87ms through a `Float64Array`. The
memory tier's element path is already fine; those 12 declines are close to free.

#### B11c — the numeric seam: `-0`, `NaN % k`, and a live-out hole in DCE

A differential fuzz sweep (~2,400 generated programs plus ~460 hand probes,
comparing JIT vs interpreter vs node) turned up 27 more JIT-only wrong answers.
They collapsed to four defects:

1. **DCE had no live-out analysis.** `dead` meant "written in the region but
   never read *in the region*"; a dead reg gets no home, its defining op is
   skipped, and nothing is flushed — so the frame keeps whatever the interpreter
   last left there. `function f(){ for (var i=0;i<40;i++) { var q = i; } return q; }`
   returned **7** (the value at the last pre-OSR iteration) instead of 39. The
   declarator form is what exposes it: a plain `q = expr` also emits
   `Move{dst:temp, src:q}` for the statement value, which keeps `q` in `used`.
   Now a register read anywhere outside `[s, e]` is never classed dead.

2. **Negation was `0.0 - x`** in the f64 and memory tiers. Under round-to-nearest
   `0.0 - 0.0` is `+0.0`, so `-(+0)` produced `+0` and `1 / -0` printed
   `Infinity`. JS negation is a sign-bit flip. This is not an exotic input: the
   compiler lowers the *literal* `-0` to `LoadInt 0; Neg`.

3. **The INT tier cannot represent `-0` at all** (i64 homes), so `Neg` of zero
   silently produced integer 0 — `Object.is(-0, -0)` was false inside a compiled
   loop. Same for `%`: a zero remainder from a negative dividend is `-0` in JS
   (`-20 % 5`), not `0`. Both now bail for that one input.

4. **`NaN % k` took the integer fast path.** The guard is
   `cvtsi2sd; ucomisd; jne => bail`, but NaN compares *unordered* (ZF=PF=CF=1) so
   `jne` is not taken — the guard fell through and ran `idiv` on the
   integer-indefinite `i64::MIN` that `cvttsd2si` produces. `NaN % 1` gave `0`,
   and **`NaN % -1` raised #DE and killed the process** (`i64::MIN / -1`
   overflows the quotient). The rest of the codegen pairs `jne` with `jp`; three
   copies of this block did not.

Fixing (2) and (3) cleared 26 of the 27 by itself — the fuzzer's programs are
dense with `-0`, so a single sign bug accounted for nearly the whole set. After
all four: **0 JIT-vs-interpreter divergences** across the 114 accumulated repro
programs. Thirteen cases remain where *both* zipp modes differ from node; those
are interpreter-level conformance gaps, not miscompiles, and are tracked under
Track A rather than here.

Also worth recording, because it invalidated the first ~370 comparisons in that
sweep: **`ZIPP_NOJIT` is presence-checked**, so `ZIPP_NOJIT=0` also disables the
JIT. A differential run must *unset* it.

### B12 — Read-only live-ins: numeric parameters now reach the INT tier

`plan_region` declined any region containing a register that is *used but never
defined* in it. That is every function whose loop reads a numeric parameter —
`function f(n){ for (var k=0;k<n;k++) ... }` — so the single most ordinary shape
in numeric JavaScript was locked out of the fastest tier.

The blanket fix had already been tried and reverted (geomean 3.31x → 3.45x): it
admitted live-ins that are strings, doubles or objects, which entry-bail on every
OSR entry and displace the memory compile that was working. The note left behind
asked for "registers used ONLY as arithmetic operands", and that is what shipped:
`numeric_operand_uses` admits a live-in only when *every* use of it is an
operand position that requires a number. `Add` is excluded (also string
concatenation), as are `Eq`/`Ne` (defined on every type), `Move`, `StoreGlobal`
and every heap-op receiver. When the entry guard does fail, `entry_bail` resumes
at the loop header — an in-region ip — so it counts as a deopt and the region
self-evicts to the memory path after `OSR_DEOPT_LIMIT`, which bounds the damage
the blanket version did unboundedly.

**Measured 2.2x on the shape it unblocks**: the identical 20M-iteration loop ran
115ms with a parameter bound (INT declined → DOUBLE/MEM) and 52ms with a literal
bound; it is now 48ms vs 49ms. Across the ten real benches it converts 4 declines
into INT regions (5 → 9) with no new evictions, but **no measurable time**: those
particular regions aren't hot. Kept because it is strictly more admission for no
cost, and because the shape it fixes is everywhere in real numeric code even
though this bench set happens not to lean on it.

### B13 — `x | 0` on a fractional double no longer deopts

`load_toint32` demanded an *exactly integral* double and bailed otherwise. ToInt32
truncates toward zero, so a fractional value is perfectly representable — the
requirement was simply wrong, and it sent the most common truncation idiom in
JavaScript to the interpreter on every iteration:

    |0 on INTEGRAL double     15ms      (3M ops)
    |0 on FRACTIONAL double  127ms      node: 3ms
    after                     17ms

The fix is also *cheaper* than what it replaced. Truncate to i64 and keep the low
32 bits: that IS ToInt32 for every `|x| < 2^63`, covering fractional values
(`3.7 → 3`) and large ones alike (`5e9 → 705032704`, `2^31 → -2147483648`,
`2^32 → 0`). Only `cvttsd2si` OVERFLOWING needs a bail, which it signals with the
`0x8000_0000_0000_0000` indefinite — NaN, ±Inf and `|x| ≥ 2^63`. No round-trip
`cvtsi2sd`/`ucomisd` pair any more.

**Watch the first attempt.** Requiring the truncation to fit *i32* looks
equivalent and is not: the old code already handled large integral doubles by
taking their low 32 bits, and narrowing to i32 range regressed typedarray-math
**7.8x** (743ms → 5882ms) by deopting them instead. Caught because the bench
suddenly showed 256 deopts. The lesson is that `load_toint32`'s accept set must
only ever *grow*: it is on the hot path of every bitwise op in the engine.

Incidentally it removed a pre-existing deopt source — typedarray-math went from
256 deopts per run to **zero**, because its fractional values had been bailing all
along.

### B14 — Inlining through a wrapper (nested leaf inline)

Closure inlining (B-closures, previous section) fixed `rnd()` but not the shape
that actually appears:

    function ri(n){ return (rnd() * n) | 0; }        // called 3.75M times

`ri` inlines `rnd` happily, but the hot loop could not inline `ri`, because its
body contains a `Call` and the leaf subset rejects that outright. So the call was
still real. `callee_leaf_ok_one_call` now admits exactly one `Call`, and the
planner splices the inner callee's body in at that index — registers shifted
above the wrapper's window so the two never alias, the inner `Return` rewritten
as a `Move` into the call's `dst`, and the whole thing behind its own
`(bits, version)` identity guard whose miss jumps to the outer fallback.

Measured on the wrapper shape, 3M iterations: **129-158ms → 17ms** for a plain
inner, **200ms → 24ms** for a closure inner. v1 restrictions: the inner call
passes no arguments, the wrapper captures nothing, and both bodies are
branch-free (the splice renumbers ops without remapping branch targets).

Two bugs worth recording, because both were *invisible* — the answers stayed
correct and only the timing gave them away:

1. The guard read `dreg(callee_reg)` directly. `callee_reg` is the wrapper's own
   register number and had to go through the scratch-window mapping like every
   other body operand. It therefore compared the wrong slot, missed every time,
   and silently took the fallback — a correct real call. Zero test would ever
   catch this; only the absent speedup did.
2. The spliced closure's upvalue cells were never baked, so `UpvalGet` fell back
   to cell `0`, hit the deopt sentinel and re-ran the whole call in the
   interpreter — again correct, and ~10x slower than the plain call it replaced.

**Still not reached:** the log-scan generator remains ~1.5s. Its `rnd` ends in
`(… >>> 0) / 4294967296`, and something in that spliced body still bails at the
call site (removing the division takes the same wrapper from 597ms to 65ms with
zero deopts). That is the next thread to pull.

### B15 — `for-of` is 28x node, and `IterNext` is NOT what blocks it

`for (v of a)` over a plain array costs 113ms where the equivalent counted loop
over the same array costs 16ms — 7x, in our own engine, on the most common loop
form in modern JavaScript. node makes the two equal (4ms vs 3ms). Iterating a
Map is 61x node.

Half of it was the interpreter: each step ran a generic cascade of six separate
`heap.get` probes (generator test, iterator-object test, tombstone scan,
`flatten`, string-step test, length test) and then a full `get_index`. A direct
dense-array read cut that to 74ms, falling through to the generic path for holes,
sparse arrays and side-table-carrying arrays. Verified on 18 shapes including
`Array.prototype[1]` shadowing a hole and mutation during iteration.

The other half is that **every `for-of` region is declined outright by the JIT**.
The obvious hypothesis — that `region_can_compile` lacks an `IterNext` arm — is
WRONG, and was tried: admitting `IterNext` (with a helper that handles a dense
array and deopts otherwise) changes nothing, because the actual decline is

    [decline] PushHandler { catch_target: 44 } at region [32,43]
    [decline] GetIterator { dst: 11, src: 11 } at region [24,50]

`for-of` desugars to a try/finally so that `iterator.return()` runs on `break` or
throw, and the region contains the `PushHandler` that installs it. Compiling a
`for-of` body therefore needs EXCEPTION-HANDLER state in compiled code, not
iterator support. That is a much larger item and it gates the remaining
74ms → ~25ms.

The `IterNext` admission was reverted rather than kept: it is inert for the shape
it was written for, and unexercised codegen that some future region shape reaches
is exactly the B9 failure mode.

### B18 — `s += a[i]` over a plain Array now reaches the INT tier (24x → 1.5x)

The most common hot loop in all of JavaScript was running on the boxed memory
path. `for (i < a.length) s += a[i]` over a 200k `Array` measured **12 ns per
element against V8's 0.5 ns — 24x**, worse than any bench in the suite, and it
was invisible because no bench in `bench/real/` is dominated by that shape.

Three separate gates had to fall, and each was only visible after the previous
one was removed — worth recording, because "the tier declined" is one log line
that can mean four different things:

1. `region_is_int` had no arm for `GetIndex` on a dense Array. The pin machinery
   (`ARR_PIN_KIND`) already existed for the memory path; what was missing was an
   INT arm that tag-checks the element and unboxes it into an i64 home. Added
   `ARR_INT_PIN_KIND` (252) — the same snapshot and the same memory-path
   treatment, taken when the array is OBSERVED all-Int at OSR compile time, so a
   known-double array does not compile INT and then deopt-thrash to eviction.
   The observation is a bounded sample and only an admission hint: the emitted
   per-access tag guard is what makes it sound.
2. `a.length` was still a `GetProp`, admitted only for string pins, so ONE
   property read demoted the whole loop. `.length` now resolves against the LIVE
   receiver (`Recv::Len`) and coalesces onto whichever pin the receiver already
   has — both pin families keep the length in the same third snapshot word, so
   the existing `str.length` emitter serves an Array unchanged.
3. `plan_region` then declined silently (`return None`, no reason logged) because
   its receiver-exemption chain tested the ip-keyed `pinned_elem` predicate FIRST
   and only then matched the instruction. On a receiver carrying both an element
   access and a `.length` read, `pinned_elem` is true at the GetProp ip too, so
   the `GetIndex | SetIndex => .., _ => None` arm swallowed it and exempted
   nothing — the receiver looked "used elsewhere" and the region died. Now it
   matches the instruction first and the predicate second. That silent `return
   None` is now a named decline.

**A fourth gate was not about arrays at all**, and is the more valuable find: the
INT tier's entry guard demanded an Int TAG of every live-in, while region exit
boxes an i64 home as Int only when it fits i32 and as a double otherwise. So any
accumulator crossing 2^31 exited as a double and could never re-enter — 64 deopts
then permanent eviction to the boxed path. `emit_int_entry_load` now also admits
a double holding an exact integer in [-2^53, 2^53]. Measured on a 40M-iteration
nested sum: **425ms → 37ms** (node 22ms). The same loop whose accumulator
happened to stay under 2^31 had always run at 50ms — the two differed by nothing
but the magnitude of the data, which is exactly the kind of cliff that never
shows up in a benchmark suite and always shows up in someone's real program.

The `-0` trap is worth stating explicitly: `ucomisd` reports `-0.0 == +0.0`, so
the round-trip check ACCEPTS `-0.0` and lands 0 in the home, which exits boxed as
Int `+0` and turns `1/s` from `-Infinity` into `+Infinity`. An i64 home cannot
represent `-0` at all — the same reason `Neg` bails on a zero operand — so the
entry load rejects it and keeps that invariant true of every value entering the
tier. Caught while writing the code, not by a test; the differential case that
pins it is a zero-iteration inner loop over a `-0` accumulator.

    20M-element single sum      18ms   (node 12ms)   was ~250ms
    40M nested, acc > 2^31      37ms   (node 22ms)   was 425ms

Suite geomean is UNCHANGED — `bench/real/` is dominated by objects, strings and
regex, not int-array loops. That is a gap in the bench set, not evidence the fix
does not matter; see §1b.

### B19 — Recycling dead `ObjMap`s: 38% faster construction, 3% SLOWER suite (REVERTED)

Every plain object costs a `Box<ObjMap>` allocation plus one per parallel vector
on first push. The obvious fix is to stop returning them to the allocator: on
sweep, leave the dead map IN PLACE in its slot, cleared, and let the next
allocation refill it. The free list is exactly the right pool, so this costs no
memory over the tombstone it replaces, and the GC already pre-marks free slots
without tracing them, so a cleared map sitting in one is unreachable.

It worked, on the microbenchmark:

    {}          34 -> 20ns   (-41%)
    {a:i}       77 -> 48ns   (-38%)
    {a,b,c}    111 -> 70ns   (-37%)
    new P2     244 -> 200ns  (-18%)

And it lost, on the actual suite — interleaved best-of-5, both binaries built
from the same tree:

    class-prototype-hot  +3.8%    json-large      +2.2%
    map-set-heavy       +10.8%    markdown-render +0.8%
    polymorphic-objects  +1.0%    parse-large-js  +1.0%
                                  mean            +3.0%

`map-set-heavy` identified the mechanism: retained buffers. A recycled map keeps
its vectors' capacity, so every free slot parks up to 32*24 bytes of `keys`
buffer that the allocator can no longer hand to the Map entries the bench is
actually building. Capping recycling at capacity 4 fixed that one specifically
(+10.8% -> -2.1%) but left the mean at +0.6% — still a loss.

The deeper reason the win does not transfer, and the part worth remembering:
**the microbenchmark allocates and immediately drops, so recycling hits every
time; the real benches RETAIN their objects** — a parsed AST, a JSON document, a
component tree. Their free list is mostly empty, so the allocation path never
finds a recycled map and only the GC-side cost is paid. A construction
microbenchmark measures the churn case, which is not the case the suite is made
of.

This is the same shape as the earlier `SmallVec` result (B-series): a local
construction win that a global memory-footprint effect erases. Two independent
attempts at "make object construction cheaper by managing its memory better" have
now regressed the suite. The conclusion is not that construction does not matter
— it is that the win has to come from objects that are SMALLER and hold FEWER
allocations by construction, not from recycling the same allocations faster.
That is shapes: no per-object `keys` vector, no per-object `attrs` vector, no
`String` per property. Shapes reduce steady-state memory and construction cost at
the same time, which is exactly the pair every attempt so far has traded against.

### B20 — Tier C admissions: Bitwise/Not WON, MathOp/UpvalGet did NOT

Tier C (whole-function) rejections were entirely silent: a function containing
one unsupported op was blacklisted and INTERPRETED for the rest of the process,
however hot it got, and nothing said so. `[jit] fnN BLACKLISTED` plus
`[tierC-reject] op <Instr>` under `ZIPP_JITLOG` turned "calls are slow" into a
ranked list. That instrumentation is worth more than either patch below.

The list, over four benches: Bitwise 10, UpvalGet 10, TailCall 8, substring 4,
MathOp 4→10, NewArray 2, SetProp 2.

**Bitwise + Not: kept.** The emitters already existed in the region path and
Tier C already shared its addressing, bail labels and helpers — an allowlist gap
that outlived its emitter. Microbenchmark: a call to a non-inlinable function
went 81.5 → 37.0 ns/op. Suite, paired medians of 7: **-0.9% mean**, concentrated
where it should be (json-large -4.0%, markdown -2.4%, parse-large-js -2.1%,
map-set-heavy -1.3%); everything else inside noise.

**MathOp + UpvalGet: reverted.** Both ported the same way, both reduced the
blacklist count (parse-large-js 4→3, regex-log-scan 2→1), and both made the
suite SLOWER: +0.6% for MathOp alone, +0.9% for the pair, with async-promise-chain
and map-set-heavy consistently worse in every variant.

The reason is the shape of the emitted code, not the admission: in Tier C both
ops emit a win64 helper CALL per occurrence. The interpreter reads an upvalue
with a direct cell access. So for a function whose body is mostly upvalue reads
or `Math.*`, the "compiled" version is a sequence of C calls and is genuinely
slower than interpreting it — and admitting it also costs the compile.

**The generalisable lesson: fewer blacklisted functions is not the objective
function.** Tier C only pays when the ops it admits emit real inline code. An op
that lowers to a helper call should be admitted only when it is incidental to a
body that is otherwise inline-able — which the current all-or-nothing allowlist
cannot express. Making that per-op judgement (or giving Tier C a cost model)
is the prerequisite for the rest of the list.

### B21 — The benchmark harness was the blocker (tools/bench.py)

`bench/run_real.sh` cannot resolve the size of change this work now produces.
Measured: back-to-back runs of the SAME binary drift 3-10%, and best-of-N
reports the luckiest sample. A best-of-3 comparison credited the Bitwise change
with **-2.0%**; paired medians of 7 put it at **-0.9%**. The first number was
reported before this was understood — it was wrong, not merely imprecise.

Worse, its correctness claim was false: it pipes both outputs through
`tr -d '-ÿ'`, deleting every non-ASCII byte before comparing, so
"byte-identical to node" was never checked for any bench emitting non-ASCII. It
also discards stderr and ignores exit status, so a crashed engine scores as a
fast one.

`tools/bench.py` replaces it for anything that matters: engines run PAIRED
(one repetition covers every engine on the same bench, so drift lands on all of
them), medians with p10/p90 so spread is visible, raw samples to JSON,
per-engine startup subtracted so the numbers stay comparable with this series,
and output compared as EXACT BYTES with a non-zero exit treated as a failure.

    python tools/bench.py --reps 7 --json bench/results.json
    python tools/bench.py --ab old.exe new.exe      # the A/B optimisation loop

First authoritative reading: **geomean 2.60x**, ALL_CORRECT=1 on exact bytes.

### B22 — DataView reads on the INT tier: prize measured at ~240ms, BLOCKED in the planner

> **SUPERSEDED — see B32.** The ~240ms figure does not reproduce. Re-run with
> the same arithmetic in every control, the DataView getters are 54ms and the
> other 174ms is the boxed arithmetic. Do not restart this as written.

`typedarray-math` decomposes into eight phases. Two are at parity already (axpy
17ms vs 12, dot 11 vs 10 — the DOUBLE tier works), and one dominates:

    8-dataview     363ms vs node 94ms   (3.9x, 269ms of the bench's 513ms gap)
    1-f64-fill     128ms vs        26ms
    4-normalize     53ms vs        12ms
    5-xorshift      56ms vs        16ms

The DataView loop is `dv.getUint32/getUint16/getInt8` plus integer arithmetic.
The memory path ALREADY inlines the loads (full `DV_PIN_KIND` emitter with
dynamic endianness); what costs is the boxed arithmetic around them.

The prize is measured, not estimated. The same arithmetic shape reading an
`Int32Array` element — which IS on the integer tier — runs at **parity**:

    A  dataview loop      zipp 352ms   node 90ms   3.9x
    B  int32 elem loop    zipp 100ms   node 92ms   1.09x   <-- parity
    C  single getUint32   zipp 134ms   node 71ms   1.9x

So getting the loop onto the INT tier is worth ~240ms of the 269ms gap, which
is ~5% of the suite geomean.

**Built and reverted, unfinished.** The emitter works and was verified
byte-identical to node and to the interpreter: pinned-receiver identity guard,
`pos >= 0` and `pos <= byteLength - size` bounds, both endianness branches, and
the unbox into an i64 home (sign-extend for Int8/16/32; the unsigned kinds are
already correct because a 32-bit write zeroes the upper half, and a getUint32
result up to 2^32-1 is well inside i53). Float32/Float64 decline — an i64 home
cannot hold them. The endianness flag is SIMPLER here than on the memory path: a
Bool on the integer tier lives in a GPR home holding 0/1, so `test/jz` is exactly
ToBoolean with no tag test.

**ROOT CAUSE (second attempt, instrumented rather than guessed).** Not the
emitter, and not the sequence of admission gates — those all pass now. The
receiver enumeration is CORRECT: for `bsum = (b + dv.getUint32(o,le===1) + … +
dv.getUint16(o,le===0) + dv.getInt8(o+2)) | 0` it finds exactly three receivers,
r96/r108/r112, one per call, each written by its own `LoadGlobal dv` immediately
before its call. The blocker is that **r96 is ALSO reused as a numeric temp**
inside the same region:

    [recv+] ip=245 r96  dv=true  CallMethod { dst:95, obj:96, argc:2 }   <- dv
    [excl]  r96 used_elsewhere at ip=238 by Bitwise { dst:95, a:96, op:And }
    [excl]  r96 used_elsewhere at ip=274 by Add { dst:96, a:96, b:97 }
    [excl]  r96 used_elsewhere at ip=275 by StoreGlobalStrict { idx:20, src:96 }

So one register holds the DataView at ip 245 and a number at 238/274/275. The
planner's whole receiver mechanism assumes a receiver register is ONLY ever a
receiver — it excludes the register from typing and homing entirely — which is
exactly the limitation its own comment already names ("a receiver register
reused for other numeric values can't be cleanly excluded under the non-SSA
register model; generalizing this needs SSA-like per-use disambiguation").

The promising narrow form, for whoever picks this up: the pin's source here is
`Global(g)`, so the emitted code NEVER reads the receiver register — the
`LoadGlobal dv` feeding it is dead for every purpose except satisfying the
planner. If those defs are proven to feed only pinned-receiver uses and are then
elided, the register keeps its numeric home and the conflict disappears without
any general SSA pass. `instr_uses(CallMethod)` already returns `vec![]`, so the
def may already look unused to the existing DCE.

Superseded detail from the first attempt (the gates, all now passing):

  * `dv.getUint32(…)` three times in one body emits three `LoadGlobal dv` into
    the SAME register, and the exclusion required EXACTLY ONE def. Relaxing that
    to "every def is a LoadGlobal of the same slot" (sound — the register holds
    that global at every pinned access, and the emitter reads the receiver via
    the pin's `Global(g)` source anyway) was tried and did not finish the job.
  * `pinned receiver reg not cleanly excludable` still fires, and
    `dv.getUint32(o, true)` additionally needs `LoadBool` admitted on the INT
    path (it is not in `int_unadmitted_ips`).

Reverted rather than left in: admission code that never fires is unexercised
codegen reachable by some future region shape, which is precisely the B9 failure
mode. The next attempt should start by instrumenting WHICH receiver register and
which use makes `used_elsewhere` true, rather than fixing gates one at a time —
three were opened in sequence here and a fourth remained.

### B23 — Bitwise on the f64 (regalloc) path: correct, exercised, and SLOWER

`x[i] = (((i * K) >>> 0) % M) / M` — a Float64Array fill, about as ordinary as
numeric code gets — had no tier at all: the INT path refuses the division
(fractional) and the regalloc path refused the shift (`admit_bitwise=false`), so
it ran on the boxed memory path at 128ms against node's 26ms.

Admitting it is easy and was done: ToInt32 each f64 home via a `cvttsd2si`
round-trip (rejecting NaN, the infinities, fractions and out-of-i32 values, all
of which JS defines by a modular reduction the interpreter performs), the 32-bit
op, then convert back. `-0` needs no special case here, unlike an i64 home:
ToInt32(-0) is +0, and the round-trip compares equal because IEEE says
-0.0 == +0.0, so the +0.0 written back IS the defined answer. Verified
byte-identical to node and the interpreter over both zeroes, both i32 bounds,
2^32-1, ±Inf, NaN, fractions and 1e21.

It is genuinely exercised — the regalloc tier wins two regions in
`typedarray-math` that previously fell to memory. And it is a REGRESSION:
paired medians of 11, `async-promise-chain +5.5%`, mean **+1.3%**.

**Why, and this is the part that generalises.** The cost of a bitwise op depends
entirely on how the tier represents its operands:

  * INT tier (i64 homes): the low 32 bits ARE ToInt32 already. One instruction,
    no conversion, no guard. This is why admitting Bitwise to Tier C won.
  * MEM path (boxed Values): `load_toint32` takes an Int-TAGGED value's i32
    payload directly — a tag check and a move.
  * f64 homes: every operand needs `cvttsd2si` + `cvtsi2sd` + `ucomisd` to
    convert AND prove the conversion exact. Two round-trips per binary op, and
    the result needs a third conversion going back.

So for integer-valued data the BOXED representation is cheaper than the f64-home
representation, because a tagged Int carries its payload and an f64 home has to
compute it. Bitwise belongs on the INT tier or the memory path, never the f64
path — and the region that "upgraded" from memory to regalloc got slower for
exactly that reason.

Third consecutive revert in tier-admission work (B20 MathOp/UpvalGet, B22
DataView, this). The pattern is now explicit enough to state as a rule:
**admitting an op to a tier is only a win when that tier's value representation
makes the op cheaper than the tier it is displacing.** Blacklist counts and
decline logs identify candidates; they do not predict the sign of the change.

### B24 — B4 "admit allocation into JIT regions" is REFUTED (built, correct, slower)

`region_can_compile` had no arm for `NewObject` or `AppendDataProp`, so **any
loop containing an object literal was declined at every tier and ran wholly in
the interpreter.** That is worth stating plainly because it invalidates how the
object-construction numbers elsewhere in this file were read: `{}` at 34ns and
`{a:i,b:i}` at 92ns were INTERPRETED-loop measurements, not compiled ones, and
the "object construction is 143x off" framing conflated allocation cost with
never being compiled at all.

So it was built: `jit_new_object` / `jit_append_data_prop` win64 helpers, the
`HeapHelpers` wiring, admission, and memory-path emitters following the
`StrConcat` allocating-op discipline (pinned-pointer re-derivation after each
call). Output verified identical to node and to the interpreter. Regions
compiled, no deopts, no evictions — it worked exactly as intended.

And it is SLOWER than interpreting:

    {}          35 -> 62ns          {a,b,c}   111 -> 123ns
    {a:1}       74 -> 70ns          {a..f}    167 -> 207ns
    {a,b}       94 -> 103ns         new P2    254 -> 381ns

`{}` is the clean case: that loop has no `GetProp`, so no pinned-pointer refetch
runs, and it STILL went 35 -> 62ns. The win64 call sequence for one allocation
costs more than the interpreter's own `NewObject` arm, and a 6-property literal
pays it seven times.

This is the B23 rule applied to allocation, and it is now the FOURTH consecutive
confirmation: MathOp, UpvalGet, f64-path Bitwise, and now NewObject —
**admitting an op to a tier is only a win when that tier's representation makes
the op cheaper than the tier it displaces.** An op that lowers to a helper call
is not cheaper in a region than it is in the interpreter; the interpreter's
dispatch for it is already a direct match arm. Region compilation pays off on
ARITHMETIC and register traffic, which is why the Bitwise-into-Tier-C change won
and every helper-call admission has lost.

The corollary for B4 as written: allocation will not become cheaper by being
admitted to a region. It becomes cheaper by allocating less, or by allocating
into something cheaper than the current heap-slot-plus-Box-plus-three-Vecs — the
substrate work — and only then is admitting it worth revisiting.

### B25 — GC was 17-22% of three benches; collecting a third as often is -1.0%

Instrumenting `Vm::gc` with a wall clock (rather than reasoning about it) gave
the first per-bench GC numbers this project has had:

    json-large       122ms of 558   22%      map-set-heavy    79ms of 869    9%
    markdown-render  141ms of 697   20%      async-promise    67ms of 723    9%
    regex-log-scan   324ms of 1853  17%      polymorphic      34ms of 731    5%
                                             parse-large-js    5ms of 599    1%
                                             class-prototype   0ms           0%

The threshold collected at `2 * live`, i.e. one full trace per allocation. The
three expensive benches all keep a LARGE live set (a parsed document, 150k
retained log lines) and allocate garbage against it, so the same live objects
were retraced continuously. `GC_GROWTH = 3` traces them once per two
allocations instead.

Paired medians of 7: **-1.0% mean**, json-large -7.6%, markdown-render -2.9%,
map-set-heavy -2.0%, everything else inside noise.

3 and not more, swept by WALL TIME rather than by GC time — the two disagree,
which is the point. GC time keeps falling at 4 and 6 (json-large 56ms then
36ms), but total time stops improving: 3 is -1.4% on the swept subset, 4 only
-0.9%, 6 worse. Past 3 the larger slot array costs more in cache misses than the
skipped tracing saves.

That crossover was visible in an earlier ablation and is worth keeping: setting
the GC to NEVER collect made a 3M-object allocation loop SLOWER, `{}` going
35ns -> 64ns, because `objs` grew to 240MB and every allocation touched cold
memory. Non-moving mark-sweep with slot recycling is doing useful cache work,
not only reclaiming memory — so "collect less" has an optimum, and it is near.

Cost: peak slots, regex-log-scan 496k -> 804k (~40MB -> 64MB). The
`objs.len() / 2` floor is unchanged, so an already-grown heap keeps its schedule.

**The remaining GC cost needs a generational nursery, not more tuning** — and
the phase split says which part and why. Timing mark / sweep / prune separately:

    bench              mark   sweep   prune
    json-large           43      28       0
    markdown-render      37      69       0
    regex-log-scan       45     104      56
    async-promise-chain   6      49       0

**Sweep dominates, not mark.** Sweep walks `floor..n` — every slot, live or dead
— and its real cost is `free_slot` dropping each dead `HeapObj`, which for an
object frees a `Box<ObjMap>` plus three `Vec`s. So it is proportional to the
HEAP, and its constant is the allocator's free() cost. That is exactly what a
copying nursery removes: young garbage is never freed individually, the
semispace is reset.

`prune` (the 36 side-table `retain`s) is only visible on regex-log-scan, and
guarding each with `is_empty()` was tried and changed NOTHING (56ms -> 56ms) —
the cost is a genuinely large `arr_props`, which holds an entry per live
`exec` result. It goes away by not putting match results in a side table (see
B8b), not by skipping empty maps.

So the GC ordering for future work is: nursery first (sweep + mark), match-result
representation second (prune), and no further threshold tuning — 3 is the
measured optimum.

### B26 — Where the object-construction gap ISN'T: escape analysis, not allocation

Two measurements that correct the framing this file has used throughout.

**The microbenchmark gap is mostly escape analysis.** node reports `{}` at 0.2ns
and `[]` at 0.6ns per iteration — that is not fast allocation, it is NO
allocation: the object does not escape the loop and V8 removes it. Comparing our
35ns against it and calling the difference "allocation cost" is wrong. In the
real benches, where objects DO escape (an AST, a JSON document, a component
tree), V8 allocates too, and the gap there is the 2.2-2.9x the suite shows.

**The per-key `String` is not the cost either.** Ablating it (push an empty key
instead) moved `{a,b,c}` only 114 -> 108ns and `{a..f}` not at all — about 2ns
per property. This is the third independent refutation of property-key
interning: the `Rc<str>` intern table was 5-8% slower, the regex-result
decomposition put the three keys at 42ns of 316, and now direct ablation puts
them at ~5% of literal construction.

### B27 — Plain-object method inlining, and a measurement trap in the call microbenches

`build_method_shape` required `m.class`, so a method held as an own property —
`{ m() {…} }`, the module/callback/vtable shape — never inlined, while the
identical method on a class did:

    call add2(i,1)         3.8ns      method obj.m(i)        21.1 -> 3.8ns
    class  c.mm(i)         3.8ns      polymorphic o4[i&3].k  25.4 -> 5.4ns

The polymorphic case now beats node (9.2ns). Suite impact is +0.3% over 11
paired reps, i.e. not a win there — the ten benches do not call plain-object
methods in their hot loops. Kept on the `fmt_f24`/`fmt_f64` precedent (strictly
faster in isolation, correct, suite-neutral within noise) rather than the
tier-admission precedent (clear regression).

The guard that makes it sound is worth remembering: a class method is covered by
the receiver-version guard, but an own property is a `vals` SLOT, and
`o.m = other` overwrites that slot in place WITHOUT bumping the version —
deliberately, since the ordinary-set fast path keeps the shape stable so JIT
caches survive. Each plain-object arm therefore also guards
`vals_ptr[slot] == baked_bits`.

**The measurement trap.** Several call/regex microbenchmarks in this file are
measured in loops the JIT DECLINES, so their numbers include interpretation:

    [jit] region fnN [2] DECLINED (call-mix gate)

That gate is deliberate and already tuned — a call site whose interpreter IC
stayed empty (a native callee) pays ~10ns of FFI per iteration in compiled code,
so a region compiles only when it has ≥ 20 other ops per such site, and its
comment records that loosening the ratio cost async-promise-chain 8%. A bare
`for (…) re.test(lines[i])` therefore runs interpreted BY DESIGN.

So `/a/.test("a")` measuring 100ns flat in subject length (100ns at 1 char,
107ns at 200) does NOT mean 95ns of regex setup — it is interpreted loop
overhead plus native dispatch plus setup, and the three were not separated.
What the flatness DOES establish is that matching itself is cheap and the cost
is per-call. The obvious suspect was `MatchAttempter::new` building three `Vec`s
(`bts`, `loops`, `groups`) on every call — regress's own source even says
`// TODO: avoid allocating so much`.

**Probed and refuted.** All three were converted to `SmallVec` with inline
capacity (8 backtrack entries, 8 groups, 2 loops), which for a simple pattern
removes every one of those allocations. Result: **102ns vs 100ns — nothing**,
and the regex bench phases unchanged. Reverted, along with the `smallvec`
dependency it added.

Two things follow. The allocations were mostly not happening in the first place
(`vec![x; 0]` does not allocate, and a simple pattern has zero loops and zero
groups, so only the one-element `bts` did). And whatever the ~100ns is, it is
not per-call matcher setup — the remaining candidates are the generic native
`CallMethod` dispatch and, in these microbenchmarks specifically, the
interpretation the call-mix gate guarantees.

Estimating a win from reading the source — "three Vecs per call, obviously
20-30ns" — was wrong by the entire amount. The prediction is left here next to
its refutation deliberately.

Before quoting any microbenchmark in this file, check with `ZIPP_JITLOG=1`
whether its loop actually compiled.

### B28 — Mark bits as a BITMAP: slower (+0.8%)

Sweep is the dominant GC phase (B25), and roughly a third of it is simply
walking the `marks` array. That array is `Vec<bool>` — one BYTE per slot — and
mark, sweep and the 36 side-table prunes each stream all of it, so on a 1.2M-slot
heap it is a 1.2MB array (larger than L2) re-zeroed every collection. As bits it
would be 147KB and stay resident.

Built it (`MarkBits` over `Vec<u64>`, `get`/`set` by shift and mask), verified
correct under `ZIPP_GC_STRESS` and against node. Result: **+0.8% mean** over 9
paired reps — map-set-heavy +2.1%, regex-log-scan +1.8%, sparse-array +3.1%.
Reverted.

The shift/mask per access costs more than the byte load it replaces, and the
prune closures (`retain(|&k, _| marks[k])`) pay it per map entry, which is where
the regressing benches spend their prune time. The array being smaller than L2
did not matter because it is streamed SEQUENTIALLY — the hardware prefetcher
already handled the byte version, so the win was hypothetical while the extra ALU
work per access was not.

Eighth probe refuted this session against two suite wins (B25 GC threshold, B20
Bitwise into Tier C). The two that worked were both found by MEASURING FIRST —
timing the GC, logging tier declines. Every probe that started from reading the
code and reasoning about what ought to be expensive has been wrong.

### B58 — The V8-parity plan audit: contained work is safe; the architectural gap remains

The implementation plan was applied against exact baseline
`1388621f86ac92188f66c8402a8a070428d01438`. Its definition of parity is the
current ten-program suite, not general V8 parity.

M0.1 is now implemented in `tools/bench.py`: each repetition retains paired
full and empty launches, two-engine order is counterbalanced AB/BA, benchmark
order and larger engine sets are shuffled deterministically, cold total is
primary, startup-adjusted time is separate, paired medians and bootstrap
intervals are reported, raw execution order is retained, and schema-v1 results
remain readable. The harness also records
timeouts, failures, stderr previews, output byte lengths/digests,
engine/binary/host metadata, arguments, environment, seed, and digest, and
refuses silent JSON overwrite. Its 22 Python
tests cover scheduling, confidence intervals, old-schema reading, correctness
failures, timeouts, and overwrite protection.

The no-change validation found one remaining measurement debt. In the full
15-pair A/A suite, the suite CI contained zero (−1.31% to +0.21%) and nine of
ten row CIs contained zero, but `regex-log-scan` reported −1.2% to −0.1%. The
required 21-pair marginal rerun then moved in the opposite direction, from +0.6%
to +1.4%, despite both sides being the same executable. This is environmental
drift, not a binary effect, and demonstrates that the nominal within-run
bootstrap interval is too optimistic at roughly the 1% scale. Treat changes in
that range as inconclusive without an independent run even when one interval
excludes zero. Raw data:

- `bench/harness_aa_final_2026-07-28.json`
- `bench/harness_aa_regex_21_2026-07-28.json`

The isolated implementation results are:

| experiment | disposition | paired result |
|---|---|---|
| compiler hash lookups and direct expression-arrow analysis | **kept** | 3k/6k/12k/24k generated-function sweep: 6.62/16.43/43.81/86.91ms; largest/middle ns-per-MB ratio 0.975 |
| first-way own-data shape probe in interpreter `SetProp` | **kept** | focused NOJIT store micro −46.66%, 95% CI −47.80% to −45.51%; removing it was +0.52% on the four affected suite rows, 95% CI −0.66% to +1.53% |
| optional conservative ASCII regular-subset executor | **off by default** | regex row −2.82%, 95% CI −3.8% to −2.0%; feature binary +14.7%; misses the plan's 25% row promotion gate |
| classical-path capture-name clone removal | **reverted** | restoring the original path measured −0.51%, 95% CI −1.05% to −0.24%; this is inside the independently observed ~1% A/A drift floor |
| M4.0 TypedArray guard reduction | **reverted** | −0.11%, 95% CI −1.10% to +0.55%: neutral |

The final default build, containing only the retained runtime/compiler changes,
is neutral against the exact baseline over 15 counterbalanced pairs:
**0.9974× (−0.26%) cold geomean**, with a suite-level 95% CI of −0.76% to +0.90%
and exact output for every row. No unrelated row regressed more than 2% beyond
its confidence interval; `json-large`
improved 3.0% (95% CI 0.1% to 3.7%). Raw data:

- `bench/final_default_2026-07-28.json`
- `bench/final_default_nojit_smoke_2026-07-28.json`
- `bench/final_default_vs_1388621_ab_2026-07-28.json`
- `bench/setprop_suite_subset_ab_2026-07-28.json`
- `bench/regex_linear_final_ab_2026-07-28.json`
- `bench/group_name_clone_ab_2026-07-28.json`
- `bench/typedarray_m4_guard_reduction_ab_2026-07-28.json`

The same final binary against Node is **1.90× on primary cold total** and
**2.15× in the historical startup-adjusted units**, with `ALL_CORRECT=1`.
It wins `map-set-heavy` at 0.90×, but `regex-log-scan` (4.00× cold) and
`typedarray-math` (3.11× cold) show why this cannot be described as parity. A
one-repetition no-JIT smoke also produced exact output on all ten programs; it
is a correctness check, not a performance estimate.

Correctness checks passed for the workspace release suite, default and
feature-gated regexp differential suites, compiler boundary and semantic tests,
strict/sloppy/proxy/accessor/dictionary `SetProp`, GC stress, WASM, and
no-default-feature compilation. Test262 was not rerun because no Test262 checkout
was available locally; its baseline must therefore be checked before release.

This is useful substrate, but it is not parity. Still open from the plan:

1. M0.2-M0.4: persistent warm/compile separation, phase telemetry, RSS, and
   materially broader benchmark coverage.
2. M3: stable per-object metadata and shape-key native ICs.
3. M4: a CFG/SSA tier with precise deoptimization, then typed-array/DataView
   range and bounds specialization.
4. M5: stable object arena, scalar replacement, and a handle-preserving nursery.
5. Validation on real application bundles before any broad V8-parity claim.

### B57 — `o["k" + i] = v` fuses soundly: polymorphic-objects −16%

The F1 finding, landed on the second attempt. The first (B50's wrong-answer
note) emitted `SetIndexConcat` directly, which performs the concatenation —
and therefore the key's observable `ToPrimitive`/`ToString` — at the STORE,
after the RHS has evaluated. The `+` sits BEFORE the RHS, so a user `toString`
on the key ran in the wrong order. Ten lines of JS showed it, and it had
passed an adversarial verifier; the probe is what caught it.

The sound shape splits the `+` into its two halves: a new `ToConcatKey { dst,
src }` runs at the `+`'s own position — identity for every primitive and for
heap strings (their concat runs no user code, so deferring it is
unobservable, and an Int key keeps the store's allocation-free scratch path),
the real `ToPrimitive(default)` + Symbol-TypeError protocol for a non-string
heap value — and `SetIndexConcat`'s concatenation is then PURE at the store.
Emission order: receiver, key-rhs, ToConcatKey, RHS, store — the unfused
sequence's order exactly. The read/delete/for-of-target fusions stay as they
were: nothing evaluates between their key and their store, which is why they
were always sound.

Both ops are admitted to the region MEM path in the same commit — mandatory,
or every loop that previously compiled its `Add`+`SetIndex` would now decline:
`jit_to_concat_key` (pure identity, deopts a real coercion) and
`jit_set_index_concat`, the write twin of `jit_get_index_concat` (own writable
data-slot hit in place — scratch key, no alloc, no version bump, exactly the
interpreter's hit arm; a NEW key / exotic / non-Int key deopts, which is the
same set the old pair failed to compile).

**Measured (quiet box, `tools/bench.py --ab`, paired medians of 9):
polymorphic-objects 733 → 614ms, −16.2% [p10 604 p90 639]**, above the
survey's verified ~13% — its two keyed-write phases are interpreted
(blacklisted for other ops), and the interpreter arm saves the throwaway
key-string alloc plus the map re-probe per write. json-large +1.1% (noise).
Pinned by `set_index_concat_fusion_order`: the exact B50 ordering case, key
valueOf/@@toPrimitive/Symbol-throw before the RHS, coercion mutating the
receiver, `__proto__` (runs the inherited setter — node semantics),
frozen/non-extensible, an inherited setter, new-key attributes/order, a hot
JIT loop with a mid-loop new-key deopt, and double/negative/1e21 key
formatting — all from node, byte-identical on JIT/NOJIT/GC-stress.

### B56 — Function-local string accumulators go in-place: markdown-render −30%

The survey's M1, landed after its prerequisite (the `StrAppendInPlace`
ToPrimitive fix) shipped. `rewrite_string_accumulators` proved in-place safety
only for a TOP-LEVEL GLOBAL accumulator with a call-free body — because a call
can read a global by NAME. A function-local register cannot be named by any
other code, so `rewrite_local_accumulators` admits calls in the body and moves
the whole burden onto proving the REGISTER never leaks a second live reference
while appends can still run:

* `r` is not a parameter (sloppy `arguments` aliases parameter registers
  invisibly), no `arguments`/rest object, not a generator/async;
* the loop is not enclosed by another back-edge and no outside jump targets
  its interior — once it exits, no append runs again in this activation;
* before/inside the loop `r` is touched only by one pre-loop `LoadConst` of a
  string literal, the `Add{dst:r, a:r}` appends themselves, and the discarded
  statement value of `out += x` — a `Move` whose destination no read can
  observe.

That last clause is where the first attempt died, and the fix is the
interesting part: the register allocator REUSES statement-value slots as
scratch in other branches (renderInline's reg 30 is written by a `LoadInt` at
ip 16 and read at ip 17, in a different branch from the discarded `Move` at
ip 27), so "never read anywhere" declines the exact function this exists for.
`move_dst_unobservable` instead proves every read of the slot has a DOMINATING
WRITE, by scanning straight-line code backwards from the read and failing
conservatively at any jump target, any unconditional transfer, or ANY op
outside the enumerated set — which is also what keeps `PushHandler`-style ops
with hidden control targets from being scanned across.

The register-field enumeration (`accum_may_read` / `accum_touches` /
`accum_writes`) is the hazard M1's verifier named: one missed READ field is a
silently published alias. Every arm was written against the variant's
declaration in bytecode.rs, `arg_base`/`argc` pairs are treated as windows,
and everything not enumerated is conservative in the safe direction (reads:
assume yes; writes: assume no).

**Measured (quiet box, `tools/bench.py --ab`, paired medians of 9):
markdown-render 677 → 476ms, −29.7% [p10 473 p90 479]** — M1's predicted band
(5.05M appends × ~35ns saved). parse-large-js +0.9%: its accumulators do not
match the proof, honestly declined. Verified by a 13-case aliasing probe
(mid-loop escape, later-read snapshot, `out += out`, closure capture, eval,
mid-loop reset, sibling loops, enclosed loops both with and without re-init,
try/catch, helper-call appends, a generator, and the hot shape run twice) —
byte-identical to node on JIT, NOJIT and GC-stress, and pinned as
`local_accumulator_inplace_aliasing`.

### B55 — The match-result side table: DICT-mode landed (neutral), the
recycling pool REFUTED (+2.2% on its own target), and the design space mapped

B33-C / RLS-1, attacked with a three-design workflow (lazy sidecar / cheap
construction / pristine-path elision) plus an exhaustive territory map. What
came out is mostly refutations, and they close the item's cheap ends:

**Landed: the match-result entry now starts in DICT mode**
(`ObjMap::side_table_with_capacity`). It was the ONE side table built with
`with_capacity`, whose shape starts at the EMPTY root — so every match ran 3-4
real `shape::add` transitions (a TLS table probe each) for a map that can never
serve a shape guard. Predicted 15-25ms on regex-log-scan; measured **+0.9%,
i.e. at the noise floor and indistinguishable**. Kept because it is correct by
construction (it closes an accidental exception to `new_side_table`'s
documented contract), not because it measured.

**Refuted: the GC-harvested recycling pool** — B19's counterexample design,
demand-trimmed (pool truncated each GC to the results created since the last
one, so it decays to zero in non-regex workloads), content-verified (exact
3-key check, attrs, no index, DICT), vals cleared so the pool holds no heap
reference. Built, verified byte-identical vs node incl. GC stress and a
mutation/defineProperty/delete probe — and **regex-log-scan measured +2.2%**,
the bench it exists for. Reverted. That is the FOURTH recycling/caching
refutation in this file (B19 ObjMap recycling, B29/B49 interning twice, now
this), each killed by a different term: this one presumably the per-GC dead-
entry scan plus cold pooled memory against mimalloc's fresh allocations.
**Construction cost is real (~456ns/match) and caching does not recover it.
Stop proposing caches for it.** What would recover it is not building the
representation at all — which is Design A below.

**Measured and closed: pristine-path elision (Design C) is worth ~0ms HERE.**
Instrumented on the real corpus: 825k successful matches, of which the phases
that don't need results already build none (`test` via build=false, replace via
`regex_replace`) — and **100% of the 600k results that ARE built escape to
user code**, so there is nothing left to elide on this bench. Two cheap grafts
remain real for OTHER workloads and are specified in the workflow output
(delegate a pristine `RegExp.prototype[Symbol.replace]` to the trusted
`regex_replace` path; a no-build arm for `@@search`): ~10 lines each, no suite
movement expected.

**Deferred with a full map: the lazy sidecar (Design A)** — index/input/groups
unboxed in a POD side map, arr_props materialised on first exotic touch.
Predicted ~140ms of the ~190ms ceiling, effort XL, and the judge's verdict is
the reason it waits: correctness rests on ~256 hand-bucketed `arr_props` uses
(the workflow's territory map lists every one), and one wrong bucket is a
silent wrong answer — e.g. `has_property_jit` answers `"index" in m` only
because a match array HAS a side table today. The map, the three designs and
the judge's synthesis live in the session workflow output; anyone attempting
A starts from there, not from scratch.

The honest bottom line for regex-log-scan: after this, its remaining gap is
the matcher's execution model (B8b) and the boxed loops that surround it —
representation tweaks are tapped out.

### B54 — `typeof x === "lit"` fuses to `TypeOfIs`: map-set-heavy −10%,
json-large −4%

B10.6(b) and the survey's JL-TYPEOF-FUSE, landed as one fused opcode instead of
two half-fixes. The unfused pair allocates a heap string per evaluation
(`alloc_str(type_of(v).to_string())`) and then content-compares it through the
Eq machinery; `TypeOfIs { a, code, neg }` compares the classifier's `&'static
str` against `TYPEOF_NAMES[code]` and allocates nothing. Fuses `===`, `!==`,
and the loose forms (equal by construction: one side is a string literal and
`typeof` always yields a string), in both operand orders.

The two details that make it exactly equivalent, not just close:

* The operand of a fused `typeof <bare ident>` compiles through the SAME
  factored `typeof_operand` path as the unfused form, so `typeof undeclared
  === "undefined"` keeps its non-throwing `LoadGlobalOrUndefined` read and a
  TDZ'd lexical still throws.
* A literal that is not one of the eight results fuses as code 255 — matches
  nothing — rather than declining, so the operand's side effects still run.
  Comparing BY CONTENT against `type_of`'s return means the fused op cannot
  diverge from the unfused pair (including `[[IsHTMLDDA]]` → "undefined").

JIT: `jit_typeof_is` is PURE (no alloc, no user code, total — no bail, no
refetch), admitted to the region MEM path and Tier C. The bare `TypeOf` stays
un-admitted in regions: it allocates, and after this fusion it is rare.

**Measured (quiet window, paired medians of 11): map-set-heavy 1457 → 1318ms
(−9.6%), json-large 804 → 773ms (−3.9%).** A second 9-rep suite run under a
load spike reproduced both target rows (−10.4%, −4.4%) with off-target rows
swinging ±10% — the agreement of the two target rows across both runs is the
evidence, the suite means are not. map-set-heavy's −10% exceeds the ~26ms its
1.33M allocations price at B37's ~20ns; the remainder is presumably GC
pressure (2.7M fewer transient strings per run) — not decomposed further.

NOT done here, deliberately: interning the 8 result strings for the BARE
`typeof x` (B10.6(b)'s other half). After the fusion the bare form's remaining
traffic is storing `typeof v` into collections (map-set-heavy still does), but
B49 killed three interning attempts on GC-root/probe-cost grounds — re-derive
the arithmetic before touching it.

### B53 — `ToPropKey` is now visible to the regalloc planner: the normalize
loop reaches the unboxed tier, typedarray-math −15% under load

B32 open item 2, landed as specified there and verified by this session's
survey. `x[i] *= v` emits `ToPropKey; GetIndex; Mul; SetIndex`, and `ToPropKey`
was invisible to `writes_reg` and `instr_uses` — so its dst looked
never-defined, landed in `ro_live_in`, and fired the ONE site in the whole
suite where `read-only live-in used where a number isn't required` declines
(typedarray-math's normalize region [104,124], to the boxed MEM tier).

Five edits, all planner/emitter bookkeeping: `writes_reg` gets the def;
`instr_uses` gets `[obj, src]`; `numeric_operand_uses` gets `[src]`, which is
what makes a live-in key sound — the entry guard bails for anything that is not
a genuine number, and a NUMBER key is exactly where ToPropertyKey is the
identity; the pinned-receiver use-site scan exempts ToPropKey's `obj` (the
nullish check is subsumed: the plan proved the receiver a live TypedArray,
LoadConst admits no null/undefined, no calls run in a numeric region, and every
pinned access re-checks identity); and the emitter compiles it as `Move`. One
type rule carries the correctness: a **Bool-typed src declines** — the
interpreter coerces `true` to the STRING key `"true"`, where a copy would index
element 1. Fractional/NaN/-0 keys are covered by the entry/access guards and
pinned against node in `topropkey_regalloc_key_semantics`.

**Measured (focused A/B, paired medians of 11, heavy concurrent load):
typedarray-math 1558 → 1324ms, −15.0%.** The verified prediction was ~39ms
(~5%); −15% suggests the fix un-declined more than the one region, but the box
was too loaded to decompose honestly — re-measure the phase split on a quiet
machine before quoting a number finer than "the row moved, double digits". The
full-suite A/B from the same window was thrash-contaminated (absolute times
2-5× inflated, p10/p90 spanning 2×) and is recorded in `ab_topropkey.json` as
unusable rather than averaged into a claim.

The deterministic evidence, load-proof: the decline-reason line between
[104,124]'s INT decline and its compile is GONE (it reads regalloc now), and
`region [136,167]` (xorshift) still declines with `pinned receiver reg not
cleanly excludable` — which is CORRECT, because B32 item 1 measured that tier
1.65× slower for that loop. Do not "fix" that decline.

### B52 — `super.v = x` inside a class setter inlines too: another −28.8%, and
the soundness probe found a process-killing engine bug that predates it

The B51 asymmetry, closed. The setter half needed what the getter half got for
free: an accessor's setter lives in `attrs[slot].setter` (heap.rs:257), which
the super guard set's `holder_vals_ptr[holder_slot]` re-check cannot reach. So
`ic_super_setter_baked` bakes the ABSOLUTE address of that word into
`holder_vals_ptr` (holder_slot = 0) and the emitter's identical `[ptr + slot*8]`
re-read checks the live setter half. Deref safety is the vals argument verbatim:
`attrs` reallocates only on a key add/delete, both bump the holder version, and
the hop guards run first. An in-place swap of the setter half (`defineProperty`
with a new `set`, keeping `get`) moves nothing and bumps nothing — ONLY the
value compare catches it, which is why the re-check exists.

Being a STORE, the op is effectful, so it obeys the same rule as the trivial
setter's own `this._v = x`: admitted only as the LAST op before the terminator
(no op may bail after the effect commits), and the inlined parent setter's own
store is likewise its last op. One planner guard that is easy to miss: a
class-syntax setter always has exactly one formal, but a `defineProperty`-
installed one is an arbitrary function, and the emitter binds the value to
sub-window reg 1 unconditionally — with 0 formals reg 1 is a LOCAL that must
start undefined, so the plan requires `param_count == 1` rather than
special-casing.

**Measured, `tools/bench.py --ab` against the B51 binary, paired medians of 9:**

| | old | new | |
|---|---|---|---|
| class-prototype-hot, alone | 804ms | 572ms | **−28.8%** |
| class-prototype-hot, in the full suite | 761ms | 582ms | **−23.6%** |

Suite mean −3.3%, but the off-target rows (markdown −13%, parse −16%, regex
+13%) are load noise from the concurrent conformance session — p10/p90 spreads
of 50%+ — so claim the ROW, not the mean. B51+B52 together: the bench went
~933ms → 572ms on a loaded box, and its historical 3.30× ratio is now in the
~1.8× band. ≈−3% geomean per step, twice.

**The bug the probe found is the better story.** Probe case "parent accessor
replaced by a data property" crashed the PROCESS — on the baseline binary too,
both tiers, so it predates this work. `super.v = x` falling through to the
receiver (parent slot deleted or turned into data) must CreateDataProperty on
the receiver — an OWN define. `reflect_set_on_receiver`'s no-own-property arm
instead ran a full `set_index`, i.e. [[Set]], which walks the receiver's
prototype chain, finds the derived class's own `set v`, and re-enters the very
setter the write was falling back FROM: unbounded recursion, native stack
overflow, and under `panic = "abort"` a dead process from two lines of JS. The
same wrong path made `Reflect.set(t, k, v, receiver)` run a setter INHERITED by
the receiver where the spec defines an own property and never consults the
chain. Fixed by mirroring the Proxy-receiver branch fifteen lines up, which
already did the define correctly. Verified against node across the 12-case
setter probe + an 8-case Reflect.set probe, both tiers, GC stress; test262
super/class/Reflect.set subsets byte-identical to the baseline binary (the
crash shape simply isn't in the suite, which is how it survived).

The lesson for the file: **the adversarial probe against node is what found a
pre-existing crasher that 95k test262 executions never touched.** Write the
probe before shipping the fast path, not after.

### B51 — `super.v` inside a class getter now inlines: class-prototype-hot −27%

The first thing this session measured that MOVED. `build_accessor_shape` passed
`allow_super = false`, so a getter whose body reads `super.v` got no JIT
treatment at all while the *method* case had inlined `super.m()` since Stage 3.
On `bench/real/class-prototype-hot.js` that is `Tri`/`Hex`, i.e. half of the
four receivers in the accessor round-trip phase — visible in one log line:

```
before:  [mi] fn0@111 INLINE getter arms=2      <- Circle, Square only
after:   [mi] fn0@111 INLINE getter arms=4
```

**Measured, `tools/bench.py --ab` against `b550a4c`, paired medians of 9:**

| | old | new | |
|---|---|---|---|
| class-prototype-hot, alone (9 reps) | 933ms | 680ms | **−27.1%** |
| class-prototype-hot, in the full suite | 903ms | 663ms | **−26.6%** |

Two independent runs agreeing to 0.5 points, with a tight interval
(p10 647 / p90 702), and every other row inside noise. ≈**−3.0% geomean**.

**Why it was cheap, and the fact that made it cheap.** `heap.rs:257` stores an
accessor's GETTER in `vals[i]` (`attrs[i].setter` holds the other half). The
existing super guard set re-reads `holder_vals_ptr[holder_slot]` and compares it
to the baked `fn_bits` — for a getter that is *the same load, at the same
address, for the same reason*, so the whole guard set transfers verbatim and the
emitter needed one changed line:

```rust
Instr::SuperMethod { dst: d, .. } | Instr::SuperGet { dst: d, .. } => { … }
```

Invoking a getter IS running its body with `this` = the receiver, which is
exactly what the method arm already does. The work was resolution, not codegen:
a `SuperGet` site fills `IcEntry::SuperAcc` rather than `SuperData`, hence
`ic_super_getter_baked` — the `SuperAcc` twin of `ic_super_method_baked`,
requiring `attrs[s].accessor` where the method version requires `!accessor`.

**SETTERS are deliberately NOT admitted**, and this is the load-bearing
asymmetry rather than an oversight to fix later: `super.v = x` resolves to
`attrs[slot].setter`, which the `holder_vals_ptr[holder_slot]` re-check does not
reach. Admitting it needs a second baked pointer into `attrs` plus its own
staleness argument. `build_accessor_shape` therefore sets
`allow_super = !is_setter`, and the setter site stays at `arms=2`. That leaves
roughly half the phase's super traffic on the helper — the remaining prize here
is real but it is NOT a flag flip.

**Guards, each with a regression test that breaks it after the arm is baked**
(`super_getter_inline_invalidates`, `super_getter_inline_preserves_values_and_effects`,
plus a 16-case probe diffed against node on both tiers and under
`ZIPP_GC_STRESS`): redefining the parent getter (caught by the holder slot
re-read — the epoch alone does NOT catch this), replacing the accessor with a
data property of the same name, `delete`, `setPrototypeOf` on the derived
prototype, class re-declaration (`mi_class_epoch`), a receiver field mutated
under the arm, a getter with side effects (it must still run every time — the
body is re-executed, never memoised), a set-only parent (`undefined`, not a
call), a Proxy receiver, a three-level super chain, and `-0`/NaN/string/object
passing through unchanged.

One process note worth keeping: the two regression expectations I hand-computed
were both WRONG, and the test caught me rather than the engine —
`assert_jit_matches` asserts JIT == NOJIT *before* comparing to the expectation,
so the mismatch was provably mine. node arbitrated. Compute expectations with
the reference engine, not by hand.

### B50 — The three JIT admission lists had drifted apart, and converging them
naively is a WASH: two ops win, one loses, and the suite mean hid both

Three separate op whitelists gate the three mem paths, and they had silently
diverged:

| list | gates | was missing (of what another list already had) |
|---|---|---|
| `region_admit.rs::region_can_compile` | Tier B, one loop region | `LoadUndefined`, `LoadNull`, `TypeOf`, `LenOf`, `ForInKeys`, `IsArray` |
| `proto_mem.rs::mem_can_compile` | Tier C, a WHOLE function | `CellGet/Set`, `UpvalGet/Set`, `Div`, `MathOp`, `SetProp`, `LoadUndefined` |
| `region_int.rs::int_unadmitted_ips` | the INT tier | (correctly strict — not a divergence) |

Tier C's gap is the expensive one, because a Tier C rejection **blacklists the
whole function for the rest of the run** — its own source says so and it is
still the right description. Admitting everything took blacklisted functions
across the ten benches from **16 → 9**; what actually SHIPPED (see the probe
below) takes them **16 → 13**, and `map-set-heavy`'s largest loop region
([39,110], 71 ops), which three `LoadUndefined`s were declining, now compiles.
`class-prototype-hot` goes to zero — its only blacklisted function was refused
for `SetProp`, which the tier already reserved an IC site for (`compile`'s
`n_sites` filter and the desync assertion at the end of `compile_proto_mem`
both name it), i.e. it was gated out one line short of working.

**First measurement of the whole batch: `tools/bench.py --ab`, paired medians
of 9, ALL_CORRECT=1 — eight of ten rows SLOWER, mean +0.6%.**

```
async-promise-chain +1.4%   class-prototype-hot -0.7%   json-large    +2.5%
map-set-heavy       +1.0%   markdown-render     +1.2%   parse-large-js +0.6%
polymorphic-objects -1.8%   regex-log-scan      -0.1%   sparse-array   +0.9%
typedarray-math     +1.1%                         mean  +0.6%
```

**Do not stop there, as I nearly did.** A suite mean hides opposite-signed
per-op effects, and here it hid two real wins under one real loss. The probe
that separates them uses a CONTROL ARM that is Tier C compiled in BOTH binaries,
so machine load cancels and only the ratio matters
(`scratchpad/tierc_probe.js`, 3M calls per arm, median of 3 interleaved rounds):

| arm | old ratio vs control | new | |
|---|---|---|---|
| `MathOp` + `Div` | 8.23 | **5.67** | **−31%** |
| `SetProp` | 11.42 | **9.42** | **−18%** |
| one `UpvalGet` | 4.16 | 6.00 | **+44%** |
| `UpvalGet` + `UpvalSet` | 6.10 | 6.71 | +10% |

So the closure-cell admissions were paying for the other two. The mechanism is
the shape of the tier, not the op: `jit_upval_get` is a win64 CALL that resolves
the closure from `frames.last()`, does a heap get and a match — exactly the work
the interpreter's arm does inline, with an FFI boundary added. Inside a Tier B
loop region the surrounding ops are compiled and it nets out positive, which is
why B10.3 was right to admit it THERE. Tier C's shape is a small function whose
body is mostly the upvalue access, so the call overhead plus the native
entry/exit is the whole story and it loses.

`CellGet`/`UpvalGet`/`CellSet`/`UpvalSet` are therefore admitted to Tier B and
NOT to Tier C, with that probe recorded at the rejection site.

**Reaching a tier is not the same as being faster in it** — the generalisation
of B39, and the thing to test per op before admitting anything anywhere.

`emit_math_op` is now factored into `emit_misc.rs` and shared, with
`math_op_emittable` shared by both admission checks, so that one op cannot
drift again.

**And the shipped subset is STILL a wash on the suite: mean +0.2%**, paired
medians of 11, ALL_CORRECT=1, on a quieter box (`bench/ab_final2.json`):

```
async-promise-chain +2.0%   class-prototype-hot +0.2%   json-large    -4.7%
map-set-heavy       -2.8%   markdown-render     +3.1%   parse-large-js -3.3%
polymorphic-objects +1.5%   regex-log-scan      +1.5%   sparse-array   +4.5%
typedarray-math     +0.1%                         mean  +0.2%
```

Five rows slower, four faster, one flat, no direction. So the two per-op wins
are real and do not reach the suite — the functions they unblock are not hot
enough to matter against everything else those benches do. **Kept on B44's
precedent** (correct, closes a real divergence, has a measured per-op win, costs
nothing) and NOT as a performance improvement. The honest one-line summary of
this entry is: *the admission lists were wrong, fixing them is worth nothing
here, and the diagnostic that fell out of it is the part to keep.*

**That diagnostic is the by-product worth more than the change. Both admission
checks reported only the FIRST op they could not handle.** That is actively
misleading when prioritising — admitting Tier C's `UpvalGet` moved the blacklist
count by exactly zero, because the same markdown-render functions were also
using `UpvalSet`, `join` and `push`, which the first-only report had never
shown. Under `ZIPP_JITDUMP` both scans now run to completion and print every
offender. That turns "which op should I admit" from three build-measure cycles
into one `grep`, and it is how this table — the state AFTER this change — was
produced in a single run:

```
markdown-render  fn1  <- UpvalGet, UpvalSet     json-large  fn1 <- UpvalGet, UpvalSet
                 fn6  <- TailCall                           fn5 <- TailCall
                 fn8  <- substring/1, /2                    fn6 <- TailCall, NewObject,
                 fn9  <- substring/2                              push, NewArray, SetIndex
                 fn10 <- UpvalGet, push, join
                 fn11 <- UpvalGet, UpvalSet,   parse-large-js fn1 <- UpvalGet, UpvalSet
                         push                                 fn6 <- TailCall
regex-log-scan   fn1  <- UpvalGet, UpvalSet                   fn8 <- push, NewArray
class-prototype-hot, map-set-heavy: none left
```

`Cell*`/`Upval*` dominate what remains, and they are there ON PURPOSE per the
probe above — so the next real items are `CallMethod` (general, via
`jit_call_method_ic`: markdown-render fn8/fn9/fn10/fn11, parse fn8, json fn6)
and `TailCall`. Both should be probed per-op BEFORE landing, not measured only
on the suite afterwards.

**A wrong answer this session nearly shipped, recorded because the review that
should have caught it did not.** A survey agent found that
`compile/assign.rs`'s plain `=` arm omits the `concat_key_literal_prefix`
fusion that the READ (`exprs.rs:722`), the delete (`exprs.rs:1647`) and
`assign_target` (`assign.rs:137`) all perform, so `o["k" + i] = v` builds and
throws away a heap string per iteration while its own read fuses. Priced at
~108ms of `polymorphic-objects` by an in-file control, and passed by an
adversarial verifier explicitly asked to find a wrong answer. It is unsound,
and ten lines of JS show it:

```js
var o = {}, log = [];
function k(){ log.push("key"); return { toString(){ log.push("keyToString"); return "X" } } }
function v(){ log.push("val"); return 7 }
o["p" + k()] = v();
// node and zipp at HEAD: key,keyToString,val      fused: key,val,keyToString
```

`SetIndexConcat` performs the concatenation — and therefore the key's
observable `ToPrimitive`/`ToString` — at the STORE, which is after the RHS.
The three sites that already fuse are all safe for the same reason: none of
them has an operand left to evaluate after the key. This one does.
Fixing it needs the observable coercion hoisted before the RHS while leaving a
primitive for the op to concatenate purely — i.e. a new opcode with
ToPrimitive(DEFAULT) semantics (`ToPropKey` is the STRING hint and would call
`toString` before `valueOf`). Not a quick win; do not re-attempt it as a
one-line change.

**Refuted or closed by the same survey, so nobody re-derives them:**

* **B32 open item 1 (pinned-receiver multi-def, for `xorshift`) is NEGATIVE.**
  The tier it would unlock measures **1.65× slower** than the MEM tier the loop
  takes today, so landing it costs `typedarray-math` ~35ms. Delete the item.
* **B5.1 (`.length` hoist to live-in registers) is worth 0ms on every named
  bench.** `typedarray-math` contains no `.length` at all, and holds every
  container in a global, which the existing `LoadGlobal` hoist already covers.
* **B5.2b (`matchAll` iterator step) HAS LANDED** — the `fast0` path in
  `proxy_regexp.rs`. Re-measured at ~10ms, not the ~552ms still recorded
  against it. That was the largest phantom in this file.
* **B10.1's prize was already collected in the helper.** Making the hole answer
  call-free inline in codegen is worth ~6.6ms of `sparse-array`, ≈0.1% of the
  suite.
* **B10.4 and B4 hit a NESTING TRAP.** Admitting `NewObject`, or
  `GetIterator`+`IterPrime`, moves ZERO regions on this suite: every such op
  sits in a region that also contains a second unadmitted op (`IterNext` and
  the `PushFinally`/`PopFinally`/`IterCloseFinally` quartet for the for-of
  regions; `LenOf`+`ForInKeys`+`DeleteIndexConcat` for polymorphic-objects
  [122,229]), so the region declines again at the next op. The full-blocker
  dump above is what makes this checkable in one run.

**Still open and independently verified, in prize order** — these are the real
backlog, and none of them is an admission change:

| item | prize | where |
|---|---|---|
| accessor inlining declines on `super.v` | **~300ms** (band 250–385) | `class-prototype-hot`; `jit_plans.rs::build_accessor_shape` handles no super, where `build_method_shape` does. Hazard: an accessor's setter lives in `attrs[slot].setter`, NOT `vals`, so the method case's `holder_vals_ptr[holder_slot] == fn_bits` re-check does not transfer to the setter arm |
| the match result's `arr_props` side table | **~190ms** | `regex-log-scan`; B33-C's mechanism at 5× its recorded price — 456ns to CREATE the entry vs 115ns for a first property on a plain object. Effort XL |
| `o["k" + i] = v` fusion, done soundly | ~108ms | `polymorphic-objects`; see the wrong answer above |
| `ToPropKey` invisible to `writes_reg`/`instr_uses` | ~39ms | `typedarray-math` `normalize`; the ONE site in the whole suite where `read-only live-in used where a number isn't required` fires |
| `typeof` allocates its result string | ~45ms suite-wide | `type_of` already returns `&'static str`; 8 permanent interned slots would do it |

### B49 — B36's MARGINAL term: 40% of it IS allocation, and interning it does
not pay — CLOSED after three attempts

Splitting the 36.2ns/key that remains after B48, on a 32-key object:

| | zipp | node |
|---|---|---|
| `for (k in o)` | 36.2 | 12.8 |
| `Object.keys(o)` + a plain loop over the result | 31.0 | 9.1 |
| `Object.keys(o)` alone | **27.3** | 8.6 |
| a plain loop over a 32-element array | 2.1 | 0.8 |

**75% of the marginal cost is building the key array**, not the for-in protocol
(5.2ns) and not iterating the result (3.7ns).

And the allocation inside that is measurable, using the heap's own pre-interned
single-ASCII-char strings as the control — `alloc_str` reuses those slots, so a
one-char key allocates nothing:

| `Object.keys` over 32 keys | ns/key |
|---|---|
| one-char keys (pre-interned, no allocation) | **18.8** |
| multi-char keys (fresh `JsStr` + heap slot) | **31.0** |

So the heap string is **12.2ns, ~40% of the marginal term** — which is B37's
"creating any heap object costs ~20ns" showing up again.

**B29 refuted interning at +0.1%. That refutation had EXPIRED**, and re-testing
it was right: B29 measured on an 8-key object where B36's 151ns per-call fixed
cost swamped the per-key term. B48 removed that fixed cost. Re-landed, the
interner is worth **`Object.keys` 27.6 -> 22.4ns/key (-19%)** and **for-in
37.0 -> 32.6**.

**And it still does not pay on the suite, so it is reverted again.**
`json-large` measured **+1.0%** across two independent runs. Ruled out: the
failed probes past the cap (bypassing the table entirely once full — the
self-tuning trick that fixed B43 — changed nothing). The remaining suspect is
that ~1,500 interned strings become permanent GC ROOTS traced on every
collection, and `json-large` collects often. Not chased further: a microbench win
that costs the benchmark suite 1% is not shippable at face value.

**Then built the weak version too, and it is REFUTED as well.** Entries pruned
in the same pass that drops the other side tables — no traced roots, and sound
because the prune runs after `free_slot` and before any subsequent `alloc`, so an
entry cannot survive into the moment its slot is recycled. That did recover part
of the cost (`json-large` +1.0% -> +0.6%), confirming rooting was a real term.
But the SUITE is **+0.9% mean, 7 of 10 benches slower**, and the microbench win
holds throughout (`Object.keys` 27.6 -> 23.7ns/key).

**So the item is closed, not parked.** Three attempts — permanent-root interning
(B29), capped interning, weak interning — refuted for three separately measured
reasons. What remains is the honest explanation: the benches barely enumerate, so
the interner's per-key hash probe and per-GC prune are paid everywhere while the
12.2ns it saves is collected almost nowhere. The 12.2ns is real; it is simply not
reachable by caching, because the cache costs more than it saves at this hit
rate.

Anyone reopening this should attack the **~20ns cost of creating a heap object**
(B37) rather than try to avoid creating one. That is the term under
`alloc_str`, it is the same term under `{}` and `Promise.resolve`, and unlike
interning it pays everywhere at once.

Recorded three times on purpose. The first refutation was the wrong workload, the
second was GC rooting, the third is the cache's own overhead — and only the first
was an error. That is what an item looks like when it is genuinely finished
rather than merely abandoned.

### B48 — B36's for-in fixed cost: it was the prototype walk, not the allocations

B36 fitted `for-in` at **A = 134ns fixed + 30.5ns/key** and guessed the fixed
term was ~10 per-call allocations (`out`, `plain_levels`, `emit`,
`spec_key_order`'s vectors, the result Array). It was not — B29 had already shown
that removing allocations from this path measures ~0, and the guess ignored it.

Re-fitted on the current tree: **A = 151ns, B = 34.3ns/key**. The fixed term is
the engine re-deriving, on EVERY `for-in`, that `%Object.prototype%` contributes
nothing — running `spec_key_order` over its dozen own methods and testing each
for enumerability and hidden-key-ness, every time.

`for_in_level_barren` memoises that per (heap index, heap VERSION). Both halves
of the predicate are load-bearing:

  * every own key non-enumerable, AND
  * the level TERMINATES the chain (its own prototype is null).

The second is what makes stopping sound. A level with non-enumerable keys still
SHADOWS those names on farther prototypes, so an early exit is only correct when
there is nothing farther.

| own keys | 1 | 2 | 4 | 8 | 16 | 64 |
|---|---|---|---|---|---|---|
| before (ns/for-in) | 178 | 202 | 277 | 420 | 720 | 2347 |
| after | **121** | **145** | **200** | **353** | **600** | 2293 |

Fixed cost 151ns -> ~90ns; **-32% on the small-object case** that dominates real
`for-in` traffic, tapering to nothing at 64 keys where the marginal term rules.
**`json-large` -1.7%** — its `walk()` phase is a for-in over thousands of small
objects, and this is the first change in the B33/B36 family to move a bench at
all.

Versioning is the whole safety argument, so it is tested directly: 200,000 warm
iterations to bury the memo, then `Object.prototype.INJECTED = 42`, which the
next `for-in` must observe. Also checked against node: a non-enumerable addition
staying invisible, custom prototype chains, own-non-enumerable shadowing a proto
enumerable, `Object.create(null)`, a three-level chain, a Proxy in the chain, and
array `for-in` with a named extra property. Both tiers.

**What is left of B36:** the 34.3ns/key MARGINAL term, which is `k.clone()` +
`alloc_str` + the result-array push per key, plus the iteration protocol that
consumes it. B29 says the allocations there are not the cost either, so the next
person should time the protocol before touching them.

### B47 — B33-F landed: Map/Set iterator results elided too

| loop | before | after | node |
|---|---|---|---|
| `for (v of set)` (positional fast path) | 24.3 | 24.0 | 2.3 |
| `for (e of map)` (positional fast path) | 51.7 | 51.7 | 5.0 |
| `map.keys()` | 155.7 | **38.0** | 3.3 |
| `map.values()` | 166.3 | **39.7** | 3.0 |
| `map.entries()` | 175.3 | **62.0** | 5.0 |

ns/step. **4.1x, 4.2x, 2.8x** — the same magnitude as B45, and predicted by it:
this is the identical shape (an iterator OBJECT stepped by the pristine
`ITER_NEXT`, where the step itself is a trivial collection walk so the result
object dominates). The rows that already had a positional fast path do not move,
which is the control.

`collection_iter_step` is the Map/Set + snapshot tail lifted out of `ITER_NEXT`
and shared with the `IterNext` opcode. It answers `None` for a TypedArray-backed
iterator, whose per-step out-of-bounds check can THROW and therefore keeps its
own path.

Verified against node across twelve shapes: deleting during iteration (tombstones
skipped, not shifted), appending during iteration (seen), exhaustion LATCHING so
a later add is not iterated, a patched `%MapIteratorPrototype%.next` (honoured,
then restored), Set holes, `entries` shape, TypedArray iterators, destructuring,
spread, and the result object when the USER holds it. Both tiers, under
`ZIPP_GC_STRESS`.

**Suite: `map-set-heavy` -0.3%, i.e. nothing** — it uses `for (const [k, v] of
map)`, which takes the POSITIONAL fast path that already existed and never built
an iterator object. The benchmark exercises the one Map/Set iteration form this
does not touch. Worth stating plainly, because "the Map/Set bench did not move"
is otherwise easy to read as "Map/Set iteration did not improve".

**The B45/B46/B47 sequence is the useful record.** The same optimisation was
worth 3.8x (array iterators), 2% (generators) and 4.1x (Map/Set). B33 priced all
three from the object's cost alone and got two right and one wrong. What decides
it is what SURROUNDS the object: a trivial step leaves the object dominant, a
step that suspends and resumes a frame does not.

### B46 — B33-B is REFUTED as written: the generator result object is not the cost

B33 priced `for (v of gen())` at 174ns/step against node's 8.3 and attributed it
to the `{value, done}` object, predicting 174 -> ~85. Tested the cheap half of
that — reading the pair straight out of the result map instead of through two
full `get_prop`s — and it is worth **215.5 -> 211.0ns, about 2%**, i.e. noise.

So the two property reads are not the cost, and by extension neither is the
object: allocating it is ~13ns of a 211ns step. **The generator gap (20x) is the
suspend/resume machinery** — activation frames, state save/restore — not the
result protocol. Anyone picking up B33-B should measure `gen.next()` against a
bare function call before extracting anything.

Kept anyway, because it is strictly less work and the guard it needs is
interesting in its own right: `iter_result_unwrap` verifies the map is EXACTLY
two own data properties named `value` then `done`, rather than assuming, because
`yield*` returns the inner iterator's result VERBATIM (spec GeneratorYield). A
generator's `next` can therefore hand back an arbitrary user object whose
`value`/`done` are accessors, inherited, or in the other order — all three are
in the regression check, along with throw/return completions.

Contrast with B45, where the same idea was worth 3.8x. The difference is what
surrounds the object: an array iterator's step is otherwise trivial, so the
object dominated; a generator's step suspends and resumes a frame, so it does
not. Same optimisation, opposite verdicts, decided by measurement in both cases.

### B45 — B33-A landed: the array-iterator result object is elided

| loop | before | after | node |
|---|---|---|---|
| `for (v of a)` (existing fast path) | 16.5 | 16.8 | 1.3 |
| `for (v of a.values())` | 167.5 | **44.3** | 1.8 |
| `for (k of a.keys())` | 157.0 | **37.5** | 1.3 |
| `for (e of a.entries())` | 176.5 | **76.5** | 3.0 |

ns/step. **3.8x, 4.2x and 2.3x**, against B33's predicted 143 -> ~45.

The step is now `Vm::array_iter_step`, lifted verbatim out of the `ITER_NEXT`
native and shared with the `IterNext` opcode, which takes the `(value, done)`
pair and never builds the object. The elision is legal for the reason
`dispatch.rs` already states where it does the same for %RegExpStringIterator%:
7.4.14 makes an ordinary object with two own DATA properties, which shadow
anything on `Object.prototype`, so neither `IteratorComplete`'s nor
`IteratorValue`'s Get can run user code, and the object's identity never leaves
the loop. The caller checks that `next` is the PRISTINE intrinsic first, so a
patched `%ArrayIteratorPrototype%.next` is still honoured.

Verified against node across eleven shapes: a patched `next` (honoured, then
restored), growth during iteration (length re-read every step), exhaustion
LATCHING so a later grow is not iterated, `next.call({})` throwing, holes,
`entries`, the result object when the USER holds it (still a real object with
own `value`/`done`), iterator helpers, TypedArray iterators (not this path),
destructuring and spread — in both tiers and under `ZIPP_GC_STRESS`.

**And it found a conformance bug that had nothing to do with performance.**
`var [p, q] = [10, 20].values()` bound two `undefined`s. `iter_to_array`'s drain
match had arms for Generator, Object, TypedArray and Array but none for
`HeapObj::Iterator`, so an iterator object fell through to the POSITIONAL fast
path and read `it[0]`, `it[1]` off the iterator. Arrays, Sets, strings and
generators all worked, which is why it survived — and test262 does not cover the
shape, so the fix shows **zero newly-passing tests**. It was found only because
writing the adversarial check for the elision meant enumerating every way an
array iterator can be consumed.

Suite effect: none — no bench in `bench/real` iterates via `.values()`/`.keys()`/
`.entries()`, exactly as B33 warned. This is a real-world win (every iterator
helper chain, `map.entries()`, `Object.entries()`-free iteration) and a
correctness fix, not a geomean move.

Still open from B33: the same treatment for GENERATOR results
(`async_runtime.rs` `iter_result`, 174ns/step, with the `yield*` verbatim case
that must stay an object), and Map/Set entry `[k,v]` arrays.

### B44 — Hidden classes, part 2: the JIT cliff, without touching codegen

The 8 -> 9 receiver cliff, re-measured with an explicit wrap counter (see the
trap below):

| receivers | 1 | 2 | 8 | **9** | 16 | 1024 |
|---|---|---|---|---|---|---|
| before | 4.00 | 5.00 | 5.67 | **17.33** | 17.00 | 16.67 |
| after | 4.67 | 5.00 | 5.67 | **11.67** | 11.67 | 11.67 |
| node | 1.67 | 0.67 | 0.67 | 0.33 | 0.33 | 0.67 |

**-33% past the cliff, nothing worse before it, and no codegen change at all.**
The plan in B43 was a shape-keyed guard in the emitted probe, which needs the
receiver's `vals_ptr` loaded rather than baked, i.e. a heap-index-parallel
`ObjMeta` array — and that runs into a design problem: `ObjMap` does not know its
own heap index, so it cannot maintain such an array. That is still the route to a
CALL-FREE shape hit and it is still unbuilt.

But the cliff is not the hit path. Past 9 receivers the site misses **100%** of
the time, so all of its cost is `jit_get_prop_miss` — plain Rust. Two changes
there:

1. **A `(site, shape) -> slot` memo**, so a miss stops re-running `map.pos(key)`
   to rediscover a slot the shape already fixes. Sound for the same reason as the
   interpreter guard: a JIT GetProp site's key is a compile-time constant, and a
   shape fixes the whole key -> slot mapping. Worth only 14.5 -> 13.75ns — the
   key scan was never the cost.
2. **Stop refilling ways once the site is thrashing.** `ic_rot` only advances
   when every way is full and one must be evicted, so a full round of rotations
   proves the site is megamorphic by IDENTITY. Filling another identity-keyed way
   then costs the write AND displaces a way that may still be serving someone.
   This is the -33%.

**The mistake worth recording**, because it inverts the obvious reasoning:
skipping the refill UNCONDITIONALLY looked strictly better and was much worse —
2-8 receivers went 5.5ns -> 12ns. A site fills its ways one MISS at a time, so
refusing the first refill for receiver 2 means receiver 2 never gets a way at
all. The gate on `ic_thrashing` is what makes it safe.

**And a measurement trap that caught me twice.** §1b warns not to index receivers
with `i & (n - 1)` for non-power-of-two `n`; at `n = 9` that cycles TWO objects.
My first cliff measurement reported the step "between 12 and 16" because of it,
and the second reported a phantom regression at 9. Use an explicit wrap counter.
The corrected numbers put the cliff at exactly 8 -> 9, flat thereafter, which is
`JIT_IC_WAYS` precisely.

Suite effect **+0.4% mean**, i.e. nothing — no bench in `bench/real/` is
megamorphic by identity while monomorphic by shape. Real code frequently is
(every `for (const o of manyObjectsOfOneKind)`), which is why this is kept.

### B43 — Hidden classes, part 1: the shape tree and a shape-keyed interpreter IC

**What landed.** `crates/zipp-vm/src/shape.rs` — a transition tree in which each
edge adds one property, so objects built by the same sequence of appends share a
`u32`. `ObjMap` carries a `shape` field maintained by the same methods that
mutate `keys`/`attrs`, and `vm/ic.rs`'s own-property entries carry the shape they
were filled against; a match proves the key -> slot mapping and the entry is used
WITHOUT looking the key up.

**Deliberately NOT a layout change.** The keys, values and attributes stay in the
three parallel vectors. The shape is a redundant summary, which is what makes the
landing safe: it can be checked against the real data (it is — see below), none
of the 7 sites that bake `vals.as_ptr()` move, and none of the 368 external field
uses have to be converted first. The allocation win from actually moving keys
into the shape is worth far less than it looks (B29, B37: removing allocations
from these paths has measured ~0 four times); the CLIFF is what is worth having.

**The invariant, and why it is the whole safety story.** A shape-keyed guard
matches on an integer and then reads the slot it recorded at fill time. A shape
that disagreed with its object by one position would return a plausible wrong
value — the failure mode this engine has shipped twice. So `assert_shape_agrees`
runs inside `assert_map_consistent`, which every existing `ObjMap` test already
calls: for a guardable map, the shape's length must equal the key vector's and
`slot_of(shape, k)` must equal the map's own slot for every key. Three new tests
pin the layout-changing operations — `delete`, `seal`, `freeze`, and a
`defineProperty` that changes attributes mid-sequence all drop to `DICT`, while
re-stating identical attributes and plain value overwrites do not.

**Measured (interpreter, `ZIPP_NOJIT=1`, 4M reads):** 36.25ns -> **32.25ns**, flat
in receiver count both before and after. That is the skipped `m.pos(key)`; ~11%.
Suite: **+0.4% mean** over 7 paired reps, i.e. noise, with `polymorphic-objects`
at -1.3% — the one bench whose objects share shapes well.

**Getting to that +0.4% took three measured corrections, and they are the
interesting part.**

1. **First landing was +1.9% mean, `json-large` +12%.** The cause was mine:
   `shape::add` built a `Box<str>` for the probe key on every property append —
   a malloc on the construction path, in a session spent hunting exactly that.
   Replaced with a `&str` scan.
2. **Still +8.8%.** `json-large`'s transition tree has **max fan-out 313**, so
   the scan was walking hundreds of edges per append. Added a hash index above a
   fan-out of 8, with every hit VERIFIED against the node's real key so a
   collision is a miss rather than a wrong shape. **Still +9.4%** — so fan-out
   was not it either.
3. **The bisect that settled it.** A `ZIPP_NO_SHAPES=1` kill switch (field
   present, maintenance off) put the bench back at baseline, proving the cost was
   maintenance rather than `ObjMap` growing by 4 bytes. And the reason
   maintenance was so expensive is the data: `bench/real/json-large.js` builds
   objects with `obj[WORDS[ri(256)] + "_" + j]` — **randomly-named keys** — and
   so wants **54,390 shapes for 18,604 objects**. It is a worst case for hidden
   classes by construction, and no guard will ever hit there.

**Which produced the design's best feature, and it was not planned.** Capping the
tree at 4,096 shapes makes the mechanism **self-tuning**: a program whose objects
share layouts stays far below the cap (`polymorphic-objects`: 1,071 shapes for
30,000 objects), while one whose keys are effectively unique blows through it in
a single pass and every object thereafter is `DICT` — today's behaviour, at the
cost of one compare per append. `json-large` went +9.5% -> **+1.7%** on that
change alone.

Side tables are exempt for the same reason: `ObjMap` doubles as the `arr_props`
store for an Array's or RegExp's named properties, which is keyed by index
STRINGS, so a sparse array would mint one shape per element. `ObjMap::
new_side_table` starts them in dictionary mode. (Measured: this was NOT what
`json-large` was hitting — the random keys were — but it is a real hazard for
`sparse-array`-shaped work and costs nothing to close.)

**A correction to two earlier claims, one of them mine.**

* PERF_ROADMAP said the interpreter IC "pays a full key lookup on every access,
  hit or miss". True — and the reason is structural rather than an oversight:
  `IcEntry::OwnData { slot }` carried NO receiver identity, so one way already
  served every receiver with the key at that slot. The interpreter IC was already
  shape-polymorphic, and the unconditional `pos` was the price of it. That is why
  it has no receiver cliff and never did.
* **The cliff is exclusively `JIT_IC_WAYS`, and it is exactly at 8 -> 9**, 100%
  miss thereafter. My own first measurement put it "between 12 and 16" because it
  indexed receivers with `i & (n-1)` for non-power-of-two `n` — at `n = 9` that
  cycles two objects and shows no cliff at all. §1b already warns about this
  trap; it caught me anyway. Use an explicit wrap counter.

**Part 2 — the JIT guard — is specified but NOT landed.** It is where the 3x
lives, and it is a bigger change than it looks because the JIT's IC entry BAKES
the receiver's `vals_ptr`:

```text
#[repr(C)] IcEntry { obj_bits @0, vals_ptr @8, version @16, slot_nhops @20, hops @24..64 }
probe: cmp rax,[r9] (identity) ; version ; hops ; hit: mov rcx,[r9+8] ; mov rax,[rcx+rdx*8]
```

Shape-keying the ways means different receivers share one way, so `vals_ptr` can
no longer be baked — the hit path has to load it from the RECEIVER. That needs a
heap-index-parallel `ObjMeta { version, shape, vals_base }` array replacing the
bare versions array now pinned in `r13`, which is a stride change at 10 indexing
sites plus a new heap-wide invariant (`vals_base` must be refreshed on every
`vals` reallocation). The emitter exists in TWO byte-identical copies
(`region_mem.rs` Tier B and `proto_mem.rs` Tier C) — factor before editing.

Predicted by the survey: the flat ~12ns miss term disappears for the
same-shape-many-instances case, leaving ~2.5-3.0ns FLAT. It does not reach node's
0.75ns — that residual is the NaN-box tag tax, i.e. B7.

**One latent hazard to fix in the same commit**, found while surveying: the
SetProp hit path (`region_mem.rs`) reads the slot with `mov edx,[r9+20]` and no
`and edx, 0x00FF_FFFF` mask, where GetProp masks. It is safe only because SetProp
ways are never filled with `nhops != 0`. A shape-keyed world that ever caches a
chain-bearing entry at a SetProp site turns that into a wild store. Add the mask
or a debug assertion on the fill path.

Also add a `ZIPP_NO_SHAPE_IC` kill switch mirroring `ZIPP_NO_METHOD_INLINE`, so
the standing gate can A/B without a rebuild.

### B42 — async-promise-chain, phase-split for the first time

Never analysed before, and it is the last bench in the suite that had no phase
table. `Promise.all` dominates it, not the then-chain:

| part | zipp | node | ratio |
|---|---|---|---|
| A — 1.5M-link `.then` chain | 251ms | 154ms | 1.63x |
| B — 1.5M `await` of a resolved promise | 100ms | 30ms | 3.33x |
| **C — 30k x `Promise.all` of 100** | **288ms** | **89ms** | **3.24x** |

C is +199ms of the bench's +366ms gap. Split further, 20k batches of 100:

| | zipp | node |
|---|---|---|
| `Promise.resolve(j)` alone | **40ns/elem** | 8ns |
| `Promise.all` over pre-built promises | 41ns/elem | 21ns |
| both together | 80ns/elem | 26ns |
| array fill only (control) | 8ns/elem | 2ns |

So `Promise.resolve` is the single largest term. **And it is not the promise
allocation.** The fast path (`natives.rs` PROMISE_RESOLVE -> `to_promise` ->
`alloc_promise` + `resolve`) is one heap slot with two EMPTY `Vec`s (no malloc),
and `resolve` short-circuits before the thenable check for a non-heap value. The
work is ~10ns; the measured 40ns is the loop being interpreted, because
`a[j] = Promise.resolve(j)` allocates and B38's blacklist applies.

That is the fifth independent area — objects, enumeration, calls, property
access, and now promises — where the measured per-op figure resolves to the same
cause. Recorded so the next person does not re-derive it from promises too.

### B41 — The MEM tier is already well optimised. There is no single missing mechanism.

Checked the emitter rather than assuming. `codegen/region_mem.rs` already emits
INLINE machine code — no helper call — for a pinned dense Array read: identity
guard against the snapshot, unsigned bounds check, `mov rax, [rdx + rcx*8]`,
HOLE compare, store. Same shape for a pinned TypedArray. 32 call sites in 1,826
lines, so the tier is mostly inline already.

Nor is `GetProp` a helper call. `Instr::GetProp` in the same file carries an
**8-way inline cache that is CALL-FREE on a hit** — receiver identity plus live
receiver version, plus the live version of each guarded proto hop, then a direct
read of the holder's `vals_ptr[slot]`. It even inlines a trivial class getter.
Constant-key object property reads are already about as good as an
identity-keyed cache gets.

**So the inventory of what IS already inlined in compiled code is:** constant-key
property reads (8-way IC), pinned dense-Array elements, pinned TypedArray
elements, monomorphic method calls, and — since this session — sloppy leaf calls.
The one shape left on the generic helper is a property access with a
NON-CONSTANT key (`o[expr]`), which is the dictionary-churn form that
`polymorphic-objects` is built from and which is rare in ordinary code.

That materially shrinks what B39 implied. The compiled tier is not missing a
mechanism; it is missing the last few percent on several axes at once, exactly
as B40's arithmetic says. The honest ordering for anyone taking this further is
still shapes FIRST — a shape-keyed guard is what lifts the existing 8-way IC off
the `IC_WAYS = 8` identity cliff and makes it work for megamorphic sites — and
only then a dynamic-key variant of it.

### B40 — What "under 2x geomean" actually costs, computed from the current ratios

Not an opinion — arithmetic on the ten numbers in §1, so it can be re-derived
whenever they move:

| scenario | geomean |
|---|---|
| today | **2.56x** |
| `regex-log-scan` made EXACTLY as fast as V8 (4.09 -> 1.00) | 2.22x |
| `typedarray-math` made EXACTLY as fast as V8 (3.89 -> 1.00) | 2.23x |
| **BOTH of the two worst at V8 parity** | **1.94x** |
| the worst THREE at V8 parity | 1.74x |
| uniform alternative | **every bench 21.8% faster** |

So the target is reached in exactly two ways: match V8 on the two hardest
benchmarks in the suite — a compiled regex backend on par with Irregexp AND a
numeric tier that closes DataView and the f64 kernels — or move every one of the
ten by ~22%.

That is the scoping fact this file was missing. It means:

* Beating the WORST bench alone is not enough. Taking `regex-log-scan` from
  4.09x to parity still leaves 2.22x.
* No stack of 1-2% items composes into it. B29 (+0.1%), B33's result-object
  sites, B36-corrected's `key_of` (~1-2%), B34 (+0.3%), B35 (0.1%) — the entire
  named-mechanism inventory is roughly 3-5% together against the 21.8% needed.
* The only single change with the right SHAPE is the one in B39: the MEM tier's
  ~3.5ns per boxed op is paid by every bench, so improving it is the only lever
  that is uniform. A 22% cut there is a 22% cut everywhere.

Record any future "get to 2x" plan against this table. A proposal that does not
either match V8 on two benches or move the MEM tier is not a plan for 2x,
whatever else it is worth.

### B39 — The unifying number: the MEM tier costs ~3.5ns per boxed op, and
everything object-shaped lands there

Six probes this session each produced a per-operation figure — 21.7ns for a
property read, 33.2ns for `{}`, 26.7ns for a plain call, 64ns/key for `for-in`,
55ns for a dictionary read. They are all the same number wearing different
clothes. `ZIPP_JITLOG` on the property-read loop:

```
[jit] INT decline [43,60]: region_is_int=false
[decline-reason] GetIndex/SetIndex (element not a pinned TypedArray)
[jit] DOUBLE/MEM region fn0 [43,60] compiled      -> 21.0ns/iter, ~6 ops
```

**~3.5ns per boxed op.** The read is not 21ns; the ITERATION is, and the read is
one of six ops all paying the MEM tier's rate. The INT tier does the same
arithmetic at ~1ns/op (B38's control), and node's optimised code is under
0.3ns/op.

The admission rule is the whole story: `GetIndex`/`SetIndex` reach the unboxed
tiers only for a PINNED TypedArray, so ANY loop touching a plain object or a
plain Array falls to MEM. That is every object-shaped benchmark in the suite, and
it is why:

* making the key scan cheaper (atoms) moves ~1 op of 6;
* removing an allocation moves ~1 op of 6 (B29: +0.1%);
* eliding a result object moves ~1 op of 6;
* and a 7.7x win on call dispatch (B35) moved 0.1%.

Each is a real improvement to one op among many that all cost the same 3.5ns.
**No accumulation of them reaches 2x**, which needs every bench ~22% faster.

**The one project.** An optimizing tier with SSA form gives, from the same
infrastructure: unboxed representations for plain-object element/property access
(so these loops leave MEM), escape analysis (B37 — node's `{}` is 0.5ns because
it does not allocate), inline bump allocation from a nursery (B38 — so an
allocating loop is not blacklisted and its other ops keep their tier), and real
register allocation instead of memory homes. Everything in B29-B39 is a symptom
of its absence, and every entry in this file that was built, measured and
reverted (B23, B24, B28, B29, and the GC probe in B37) failed for the same
reason: it optimised one op inside a loop where all the ops cost the same.

Scheduling note: this supersedes "compact object storage" and "result-object
allocation" as headline items. Both are worth doing — B36-corrected puts
dynamic-key access at 4.6-6.8x and names `key_of`'s owned `String` inside it,
and B33 prices the five result-object sites — but they are ~1-2% each, and this
file should stop implying otherwise.

### B38 — One allocation in a loop body costs the OTHER ops 3x. B24 measured
the wrong thing.

B37's `{}` = 33.2ns is not the price of a malloc. `ZIPP_JITLOG` on that exact
loop says:

```
[jit] region fn0 [6,15] DECLINED (blacklisted)      -> 33.4ns/iter
[jit] INT region fn0 [6,17] compiled                -> 0.4ns/iter   (same loop, no `{}`)
```

The allocation does not cost 33ns; it **blacklists the region**, and the whole
loop drops to the interpreter. Isolating that with five integer ops in the body:

| loop body | zipp | node |
|---|---|---|
| 5 int ops | **15.2ns** (INT tier) | 1.2ns |
| the same 5 ops **+ one `{}`** | **80.0ns** | 1.4ns |
| the same 5 ops + one `[]` | 63.4ns | 1.2ns |

The arithmetic did not change. It went from 15.2ns compiled to ~47ns
interpreted, and the allocation added ~33 on top. **Adding one allocation to a
loop costs ~3x on everything else in that loop.**

**This corrects B24.** That entry refuted "admit allocation into JIT regions"
because `{}` went 35 -> 62ns when emitted as a win64 call. But it measured `{}`
IN ISOLATION, where the comparison is an interpreted allocation against a
compiled one — and there the call overhead is the whole story. The measurement
that matters is the loop: 80ns today against `15.2 + call`. At B24's 62ns call
that is 77ns, i.e. still nothing — which is why the right conclusion is not
"admit allocation" but **"make the JIT-side allocation cheap"**: 15.2 + ~10ns of
inline bump allocation would be 25ns, a 3.2x on the shape. That is a nursery,
i.e. the generational-GC project, and it is a PREREQUISITE for admission rather
than an alternative to it.

**Scope, measured, so this is not oversold.** Blacklisted regions per bench:
polymorphic-objects 2 (of 7 compiled), regex-log-scan 2 (of 5), and **zero** in
json-large, markdown-render, parse-large-js and class-prototype-hot. So this is
not a suite-wide lever — but polymorphic-objects' two are its dictionary-churn
loops, which B36 measured at 564ms of that bench's 701ms. The benches that
allocate inside FUNCTIONS called from a loop are unaffected, because the loop
region itself stays clean.

### B37 — node's `{}` is 0.4ns because it does not allocate. That is the gap.

The last measurement needed to close the "compact object storage" question, and
it is the simplest one in this file:

| expression | zipp | node |
|---|---|---|
| loop baseline | 0.3ns | 0.3ns |
| `{}` | **33.2ns** | **0.5ns** |
| `{a:1}` | 54.4ns | 0.4ns |
| `{a:1,b:2}` | 71.2ns | 0.5ns |
| `[]` | 19.8ns | 0.4ns |
| `[1,2]` | 26.8ns | 0.4ns |

`{}` performs essentially ONE malloc — `ObjMap`'s three `Vec`s are empty and
`Vec::new()` does not allocate — plus a heap-slot push. It costs 33.2ns. So the
malloc is a small fraction of it and the rest is the `NewObject` opcode path.

And node is at **0.5ns for all six**, flat in property count. V8 is not
allocating faster; **V8 is not allocating.** Escape analysis proves the object
never escapes the loop and deletes it. That is why every layout change measured
in this file lands on nothing: a cheaper object is still an object, and the
competitor is making zero.

Together with B36 (property reads at PARITY on a 60-key dictionary object) and
B29 (removing 7 of ~13 allocations per enumeration: +0.1%), the compact-object-
storage line of attack is closed by measurement from three independent angles.
It agrees with what B26 concluded from the other direction — "where the object
construction gap ISN'T: escape analysis, not allocation" — and with B24, which
built and reverted allocation-in-JIT-regions.

**The consequence for planning.** The remaining gap is not any single missing
optimisation; it is a uniform per-operation constant across a register machine
with no SSA form. That is one project — an optimizing tier — and escape
analysis, hidden classes and inlined allocation all fall out of it. Anything
scheduled ahead of it should be justified by a MEASURED phase, not by an
allocation count.

**Refuted while establishing this** (ninth and tenth probes of the session):
* `GC_GROWTH` 3 -> 8 with `GC_MIN_THRESHOLD` 64k -> 256k: **+1.2% mean**,
  regressing polymorphic-objects +4.5%, regex-log-scan +4.4%, markdown +3.9%.
  A larger live heap costs more in cache misses than the skipped collections
  save. B25's 3x was already the right side of that curve.
* Property-name interning for enumeration (B29), and Option A atoms generally
  for this suite (B36).

### B36 — Property READS are at parity on a 60-key object. Compact object
storage is not this suite's lever, and here is the measurement that settles it

`polymorphic-objects` (2.44x) is the bench the "compact object storage"
hypothesis is aimed at. Phase-split, with `Date.now()` marks at the file's own
section boundaries:

| phase | zipp | node |
|---|---|---|
| 8 megamorphic layouts + writes | 108ms | 32ms |
| **dictionary churn (add 60 / delete 30 / re-add 30 / read 60 / for-in)** | **564ms** | **255ms** |
| proto-chain walks | 29ms | 6ms |

So 80% of the bench is the dictionary phase, not the megamorphic reads. Isolated
per-operation, 30,000 objects of 60 dynamic-string keys:

| op | zipp | node | ratio |
|---|---|---|---|
| add | 103.9ns | 44.4 | 2.3x |
| delete | 125.6ns | 101.1 | 1.24x |
| re-add after delete | 282.2ns | 64.4 | 4.4x |
| **read** | **55.0ns** | **56.1** | **1.00x** |
| for-in | 183.3ns/key | 17.8 | 10.3x |

**CORRECTION — that "parity" row is an artifact; do not cite it.** Both engines
were paying for the `"p" + k` concat that produces the key, and node's
concat-key path is unusually slow (35.0ns against zipp's 24.3ns — zipp WINS
that shape). Re-measured with the keys precomputed into an array, so only the
property operation varies:

| op, key already a string | zipp | node | ratio |
|---|---|---|---|
| read from a 60-key object | 21.7ns | 4.7ns | 4.6x |
| write to a 60-key object | 32.0ns | 4.7ns | 6.8x |
| `"prop_" + n` concat alone | 43.3ns | 2.0ns | 21x |

So property access by dynamic key IS ~5-7x off, and Option A is NOT ruled out
the way the first pass claimed. What the corrected numbers point at inside that
5-7x is `key_of` (`vm/values.rs`), which returns an OWNED `String` — a full copy
of the key text on every computed-key read and write. It cannot simply return
`Cow<'_, str>`, because the borrow would still be live when the caller needs
`&mut self` for the lookup; the shape that works is a Vm-owned scratch `String`
taken with `mem::take` and put back, so the capacity is reused and the steady
state is zero allocations. Estimated at ~9ns of the ~20ns access, i.e. ~8% of
polymorphic-objects and ~1-2% of the suite — worth doing, not a geomean lever.

B29's +0.1% on the enumeration half still stands, and so does B37/B38 on
construction.

Also checked and NOT true, so nobody re-derives them: for-in is not superlinear
in key count (flat 64ns/key at 4 keys down to 31.5ns at 128 — the small-object
ratio is a FIXED per-call cost, not a scan), and delete+re-add is flat in key
count too (181ns at n=16, 200ns at n=64).

**What the for-in fixed cost is.** Fitting `cost(n) = A + B·n` over n = 4..128
gives **A = 134ns, B = 30.5ns/key**, against node's A ≈ 0. An
`Object.create(null)` receiver removes only 37ns of it, so the `Object.prototype`
level is not the bulk. The 134ns is ~10 heap allocations per call: `out`,
`plain_levels`, the final result `Array` and its `Vec`, plus `emit` +
`spec_key_order`'s `ints`/`rest` for each of the two levels. Reusing Vm-owned
scratch buffers for all of them is the obvious fix and is UNTRIED — note only
that B29 is the cautionary tale (removing 7 allocations per 8-key enumeration
moved nothing), so it must be measured before it is believed.

### B35 — Sloppy leaf calls were declined for a `this` that is a constant: 7.7x

`plain function f(a,b){ return a+b; }` called in a hot loop cost **26.7ns**
against the equivalent object method's **3.6ns** — the commonest call shape in
the language taking the slowest path, and 8x slower than the shape everyone
assumes is more expensive. `ZIPP_JITLOG=1` names it in one line:

```
[leaf] fn0@16 callee fn1 DECLINE (lexical_this=false strict=false)
```

The leaf-inline emitter hard-coded `this = undefined` into the callee window,
which is only right for a strict callee, so the planner declined every SLOPPY
one. But the site is a plain `Call`, so `thisArg` is undefined and
`OrdinaryCallBindThis` has exactly two answers — `undefined` for a strict
callee, the realm's global object for a sloppy one — and BOTH are compile-time
constants. The plan now carries `this_bits` and the emitter writes it. Only an
ARROW still declines: its `this` is captured lexically and is neither constant.

| shape | before | after | node |
|---|---|---|---|
| `f(i,1)` plain function | 26.65ns | **3.45ns** | 0.55 |
| `clo(1)` closure | 32.75ns | **5.05ns** | 0.65 |
| `obj.m(i,1)` | 3.65ns | 3.50ns | 0.55 |

Verified against node: sloppy `this === globalThis`, strict `this === undefined`,
arrow `this` lexical, and a body that READS `this` — all identical, under
`ZIPP_GC_STRESS=1` too. test262 938 failures, no regressions.

**Suite effect: +0.1%, i.e. nothing — and the reason is worth writing down.
All ten benches in `bench/real/` open with `"use strict"`.** Their callees were
already strict, so they were already inlining; there was no decline to remove.
The fix is worth 7.7x to sloppy code, which is most of the real world and every
CommonJS bundle that never opted in, and 0% to this suite.

Two things follow. First, the suite systematically under-samples sloppy-mode
behaviour — worth fixing by adding a non-strict bench rather than by editing the
existing ten (which would break every historical ratio). Second, and more
usefully: with the sloppy decline gone, the remaining leaf-inline misses are
`NOT-MONO (no single Callee IC way)` and `DECLINE (not leaf-eligible)`, which
markdown-render hits 22 and 20 times respectively. Those are the next questions,
and they are about eligibility, not about `this`.

### B34 — `hasOwnProperty(i)` spelled the index; `"02"` was an array index

`Object.prototype.hasOwnProperty` ran `to_property_key` (which allocates a
`String` for a numeric key) and then `has_own_property`, which parsed it
straight back. That round trip IS the probe: `hasOwn.call(a, i)` over a
1000-element array measured **204ms against node's 16ms**, while `i in a` over
the same array — which never spells the key — was 17ms against 3ms.

`has_own_index_fast` answers an Array receiver with a numeric key directly,
mirroring the `HeapObj::Array` arm exactly (a hole is absent; the `arr_props`
side table can still carry an index the dense storage does not). Wired into both
routes: the `PROTO_HAS_OWN` native (`hasOwn.call(a, i)`) and the builtin-method
dispatch (`a.hasOwnProperty(i)`). **−27.6%** on the probe microbench, paired
medians of 7.

Suite effect **+0.3% mean** — i.e. nothing, except `sparse-array` at −2.2%,
which is the only bench that probes this way. Kept because it is strictly less
work and because of what the correctness check turned up:

`has_own_property` decided array-index-ness with `key.parse::<usize>()`, so
`hasOwn.call([1,2,3], "02")` reported **true** — `"02"` parses to 2, but it is
an ordinary string key that no element answers. Same bug in the `Str` and `Cons`
arms. Now `canonical_index_str`, which is the check the rest of the engine
already uses. Verified against node across 24 shapes including holes,
`defineProperty` overrides, `arguments`, a Proxy and `-0`.

### B33 — Result-object allocation: the five sites, measured and priced

The unit prices, from 20M-iteration loops (`{}` 44ns, `{value:i}` 70ns,
`{value,done}` **90ns**, a full property descriptor **151ns**, `[i,i]` 34ns —
against ~0 for all of them in node). Every finding below is one of those five
constants multiplied by a call count.

**A. `{value,done}` from the built-in iterators — the largest single win, and it
does not touch the bench suite.** `natives.rs` `ITER_NEXT` builds one at four
sites; `dispatch.rs`'s `IterNext` then does `get_prop(res,"done")` and
`get_prop(res,"value")` and drops it.

| loop | zipp ns/step | node | ratio |
|---|---|---|---|
| `for (v of a)` — existing fast path | 18.8 | 0.6 | 31x |
| `for (v of a.values())` | **143** | 0.55 | 260x |
| `for (k of map.keys())` | **133** | 1.65 | 81x |
| `for (v of gen())` | **174** | 8.3 | 21x |
| `for (v of a.values().map(f))` | **324** | 14 | 23x |

`a.values()` costs **7.6x** what `for (v of a)` costs on the identical data: 90ns
object + ~3ns for the two Gets + ~50ns dispatch.

It is **elidable outright**, and the precedent is already in the tree.
7.4.14 CreateIterResultObject makes an ordinary object with two own DATA
properties; 7.4.5/7.4.6 read them with plain `Get`. Own data properties shadow
`Object.prototype`, so neither Get can run user code and the object's identity
never escapes the loop. `dispatch.rs` already skips it for
%RegExpStringIterator% with exactly this argument — extend that arm to
`HeapObj::Iterator`/`IterHelper` stepped by the PRISTINE `ITER_NEXT` native.
Predicted 143 → ~45ns. Verified the guards that keep it honest: a patched
`%ArrayIteratorPrototype%.next` is still honoured, `a.values().next.call({})`
still throws, done still latches.

**Do not schedule this expecting a geomean move** — no bench in `bench/real/`
iterates via `.values()`/`.keys()`/`.entries()` or a generator. It is a
real-world win (every iterator-helper chain, every `map.entries()`), not a suite
win. Same for the generator case (`async_runtime.rs` `iter_result`), where the
`yield*` verbatim return must stay an object.

**C. The RegExp match result's `index`/`input`/`groups`.** ~150ns of the 367ns
per match, and **not elidable** — they are spec-required own data properties
that must appear in `Object.keys` after the numeric indices. Needs a cheaper
representation (lazy materialisation from a side table), not removal. Worth
~68ms in `regex-log-scan`'s matchAll phase and ~22ms in its exec phase.

**D. DONE** — see the `index_key` commit. `array_index_override` allocated a
`String` per indexed read of any array carrying a side table, which every match
result does. `a[2]` was 2.3ns plain and 36.9ns after `a.tag = "x"`.

**E. A property descriptor per key in object spread / `Object.assign`.** 151ns
each, purely internal at those callers (built by the engine, consumed by the
engine one line later); observable only as the return value of
`getOwnPropertyDescriptor`. `{...src}` 276 → ~90ns/key predicted, matching what
`ObjectRest` already achieves.

**F. `[k,v]` entry arrays for Map/Set/TypedArray.** 34ns each. Observable in
`for (const e of map)` where the user holds the array; purely internal in
`for (const [k,v] of map)`, where the compiler emits `IterNext` followed
immediately by the destructure that drops it.

### B32 — Where typedarray-math and sparse-array actually spend the gap

Phase tables, min of 5, with the tier each region reaches decoded from
`ZIPP_JITLOG=1 ZIPP_JITDECLINE=1`. (Reading that log: `[jit] DOUBLE/MEM …
compiled` covers BOTH the regalloc path and the boxed mem path. What tells them
apart is whether a `[decline-reason]` line appears between the `INT decline` and
the `DOUBLE/MEM … compiled` — that reason is `compile_region_regalloc`'s
`plan_region` failing.)

**typedarray-math — 706ms vs node 202ms.**

| phase | zipp | node | tier | why |
|---|---|---|---|---|
| f64-fill | 148ms | 37ms | MEM | `Bitwise on the double path` — **this is B23 verbatim, refuted** |
| axpy | 17ms | 14ms | REGALLOC | parity |
| dot | 12ms | 12ms | REGALLOC | parity |
| normalize | 55ms | 12ms | MEM | receiver multi-def (B30), then `ToPropKey` in `ro_live_in` |
| xorshift | 60ms | 14ms | MEM | receiver multi-def — the collision `plan_region.rs` already names in a comment |
| prefix-sum | 31ms | 9ms | **INT** | already on the best tier and still 3.4x |
| **dataview** | **376ms** | **97ms** | MEM | **55% of the whole gap** |

Two framing points that stop wasted work:

* **prefix-sum is on the best tier and is still 3.4x.** 3.9ns/iter against node's
  1.1, for ~22 ops with an identity guard plus an unsigned bounds check per
  pinned element access. No tier admission can move this row — it is the INT
  tier's own per-op cost, i.e. B7.
* **f64-fill is B23's exact loop.** Do not re-attempt. Note additionally that
  `i * 2654435761` reaches 2.1e16 > 2^53, so an i64-home INT tier would be
  *unsound* there without double-rounding semantics.

**B22's "~240ms DataView prize" does not reproduce — correcting it here.**
Re-running B22's own three controls with the SAME arithmetic in each (24.6M
iterations, so the only variable is the memory op):

| control | zipp | node |
|---|---|---|
| A the bench's DataView loop verbatim | 326ms | 98ms |
| B same shape, three Int32Array element reads | 304ms | 100ms |
| C same shape, no memory reads at all | **272ms** | 98ms |

The three `getUint32`/`getUint16`/`getInt8` calls are **A−C = 54ms**, not 240.
The other **174ms is the boxed arithmetic itself** — 43 bytecode ops per
iteration on the mem path against node's 4.0ns/iter for the same expression.
B22's control B must have been a simpler loop, so it removed the arithmetic
along with the getters and credited all of it to the DataView. The prize is
gated twice more even if a DV-pinned `CallMethod` were admitted: the receiver
multi-def blocker, and `xmm pool exhausted even with home reuse` (a
43-instruction region exceeds the 14-home pool). **Do not restart B22 as
written** — the DataView phase is op-count bound, which is B7, not tier
admission.

**sparse-array — 161ms vs node 55ms**, and the file's own CALIBRATION NOTE is
right that it no longer measures what it was written to measure:

| phase | zipp | node | share of gap |
|---|---|---|---|
| for-in key walk | 47ms | 9ms | **36%** |
| holey `in` loop | 36ms | 10ms | **25%** |
| `in`/hasOwn probes | 24ms | 7ms | 16% |
| slice/concat over holey windows | 12ms | 2ms | 9% |
| everything else | 42ms | 27ms | 14% |

Two thirds of it is key/`in` machinery, not sparseness. See B29 for what is
*not* the cause of the for-in cost.

**Still open, with the specs measured.** In rough order of prize per unit risk:

1. `plan_region`'s "pinned receiver reg must have exactly one def" — B30 removed
   the postfix-update cause; the remaining cases (xorshift's genuine register
   reuse, ~35ms inferred) need the narrow generalisation: allow multiple defs
   when every def reaching a pinned-receiver use is a `LoadGlobal` of the same
   slot, and retarget that access's deopt ip to the `LoadGlobal` so the
   interpreter re-executes the load rather than reading a flushed numeric home
   as an object pointer.
2. `Instr::ToPropKey` is missing from `writes_reg` (`codegen/fn_int.rs`), so its
   destination looks never-defined, lands in `ro_live_in`, and declines
   `normalize`. Fixing the analysis alone is not enough — the tier needs a
   `ToPropKey` arm, which on a pinned receiver with a numeric key is a plain
   home-to-home copy, so it does satisfy B23.
3. `int_unadmitted_ips`'s `LoadConst` arm rejects any constant that is not
   `is_int()`, though a constant in [2^31, 2^53] is exactly representable in an
   i64 home. Measured: `(o * 2654435761)|0` MEM 156ms vs `(o * 65537)|0` INT
   123ms.

### B29 — Property-name interning for enumeration: measured a NO-OP, reverted

`for-in` over an 8-key object is 66ns/key against node's 1.3, and `Object.keys`
46ns against 1.7 — the largest ratios anywhere in the engine. The obvious
culprit: `for_in_keys` (`vm/props/enumerate.rs`) hands out a FRESH heap string
per key per call, so every iteration allocates a `String` clone, a `Vec<u8>` and
a heap slot for a name that has not changed since the object was built.

Built the fix: a `key_strs` interner (name text → one shared `Str` heap index),
rooted permanently in GC and capped at 16k entries so an `obj["k" + i]` loop
cannot pin the heap. Correct — for-in order, shadowing, `Object.keys`, deletion
holes and `JSON.stringify` all matched node exactly.

Result: **+0.1%** on the for-in microbench and **+0.6%** on `json-large`, paired
medians of 9 and 7. Reverted; a permanent GC root and a cap are not worth zero.

The lesson is the same one B28 recorded, and it is worth stating in the positive:
counting allocations does not locate time. Seven of the ~13 allocations a for-in
over an 8-key object performs are the per-key JsStr, and removing all seven moved
nothing — so the 66ns lives in the surrounding machinery (the `Vec<usize>` emit
plan, the shadow set, the result Array, and the iteration protocol that consumes
it), not in the allocator. The next attempt on this should START by timing those
four separately.

Kept from the attempt: `canonical_u32_key` in `vm/helpers_numeric.rs`. The old
`spec_key_order` decided integer-key canonicality with `k.parse::<u32>()` then
`n.to_string() == *k`, allocating a String per numeric key to re-derive text it
already had. The byte-level test is strictly less work and holds no state, so it
stays regardless of the measurement.

### B30 — A discarded `i++` is `++i`, and the difference was a whole JIT tier

`plan_region` requires a pinned TypedArray receiver register to have exactly ONE
in-region definition (`codegen/plan_region.rs`, `"pinned receiver reg not cleanly
excludable"`). The bytecode for a POSTFIX update emits two `AddInt`s and takes an
extra temp (`compile/exprs.rs`); the prefix form emits one. That extra temp
shifts register allocation by one and lands on the receiver, so the whole loop
declines from REGALLOC to the boxed MEM tier.

Measured on a Float64Array read loop: `for (i = 0; i < n; i++)` **27ms** against
**7ms** for the byte-identical `while (i < n) { …; ++i; }` — 3.9x, decided
entirely by which spelling of increment the author happened to use.

Postfix and prefix perform the same single `ToNumeric` and the same single store
and differ only in which value they hand back, so where the result is discarded
they are the same program. `expr_discarded` now compiles the postfix form as the
prefix form in the two positions where nothing can read it: a `for` head's update
expression, and an expression statement in a non-eval program. (An eval program
keeps the postfix form — there the statement's value IS the completion value.)

Suite effect: **−0.2% mean** over 7 paired reps — typedarray-math −2.2%,
markdown-render −2.4%, parse-large-js −2.3%, class-prototype-hot −2.0%, against
map-set-heavy +4.9% on a wide p10/p90 spread. Kept, because it emits strictly
less code for every counted `for` loop in the language and the one regression is
not distinguishable from this box's drift. It does NOT collect the typedarray
loops' full prize: `normalize` and `xorshift` decline for two further reasons
(`ToPropKey` missing from `writes_reg`, and Bitwise on the double path — the
latter is B23 and stays refuted).

### B31 — RegExp property reads cloned the pattern text; matchAll dropped the twin

Two independent allocation bugs on the RegExp path, both found by differential
measurement against pattern LENGTH — the giveaway that a memcpy is in the loop.

**(a)** `vm/props/member.rs` cloned `source` AND `flags` out of the heap before
looking at the key, because `regexp_get_prop` needs `&mut self`. So every
property read on a RegExp — including the `test`/`exec` method lookup in a hot
loop — cost two heap allocations sized by the pattern text: 31ns for a one-char
pattern against 120ns for a 20,000-char one, on a read that returns an integer.
`re.flags` is specified to read the eight per-flag accessors off the receiver, so
it performed NINE such reads and cost 227ns against node's 3ns.

`lastIndex` and the eight flag booleans are now answered inside the heap borrow,
and so is every other key, which is a prototype walk. Only `source` still needs
owned text.

**(b)** `String.prototype.matchAll` builds an independent matcher RegExp so the
iterator can advance its own `lastIndex`. It built it with `ascii_twin: None`,
so the first `exec` on the matcher rebuilt the pattern's code-point vector and
hashed a two-String cache key — 3.2us per call on a 2,000-char pattern, against
a flat 13ns for the same work on the original object, whose twin caching worked
perfectly. The clone copies `source` and `flags` verbatim, so the twin is
provably the same program and is now carried over.

Neither is the main term in `regex-log-scan`. That bench is 59% regex, and 25% of
the whole is regress's backtracking inner loop at **6.9ns per failed match
attempt against node's 0.37** — a `\d` or `[a-z]` start predicate yields dozens
of candidate positions per line and each is a full interpreted attempt. A further
41% of the bench is not regex at all (corpus generation 34%, `fnv1a` over 23MB
5%), so even an infinitely fast matcher leaves it at 2.9x. Recorded so the next
person does not start with the result objects.

### B17 — Object CONSTRUCTION is the biggest single gap (143x), and it is one fix

Property READS are fine. Construction is not:

```text
                        zipp    node
mono read o.a           13ms     2ms    5M iterations — 2.6ns/read, 6.5x
poly read (4 shapes)    26ms     4ms
array element           18ms     2ms
new Pt(x,y)            287ms     2ms    1M iterations — 143x
{a:i,b:i,c:i}          108ms     2ms    54x
```

Decomposed by key count, the shape of it is unambiguous — a fixed base plus a
per-key term:

```text
{}            21ms    ~21ns   the heap slot alone
1 key         79ms    +58ns   three Vec allocations + one String
2 keys        88ms    +14ns
3 keys       106ms    +17ns
6 keys       215ms    +36ns/key at the tail (Vec regrowth on top)
```

**The per-key term is `ObjMap::define` doing `key.to_string()`** — a malloc and
copy for a property name that is nearly always already interned in the callee's
`string_constants`, and repeated for every object of the same shape. The base
term is `keys`/`vals`/`attrs` being three separate `Vec`s.

**Interning at define-time does NOT work — MEASURED, do not repeat it.** The
obvious fix is `keys: Vec<Rc<str>>` behind a process-wide table so adding a key
is a refcount bump. That was built to completion (88 mechanical errors across 12
files, all resolved, tests green, benches matching node) and it came out
**5-8% SLOWER**:

```text
                  before   interned
3-key literal      106ms      113ms
6-key literal      215ms      233ms
```

The reason is arithmetic that should have been done first: `intern_key` hashes
the string, probes a `HashSet`, and clones an `Arc` (atomic increment) — call it
~20ns — to avoid a malloc+memcpy of a short string, which is ~15ns. The
allocation was never the expensive part relative to a hash lookup.

**What would actually work: intern at COMPILE time, not per `define`.** Every hot
property name already exists in the callee's `string_constants`. Interning that
pool once at load and passing a pre-made `PropKey` down to `define` makes the
runtime cost a bare refcount bump with NO hashing — ~2ns instead of ~15ns. That
needs `PropKey` threaded from `SetProp { name: idx }` / the object-literal path
through to `ObjMap::define`, which is a different (and smaller) change than
retyping every key site.

**Inline storage does not work either — MEASURED.** If the cost is the three
`Vec`s allocating, `SmallVec<[T; N]>` should remove it, and in isolation it does:

```text
                  before   inline(4)
1-key literal       82ms       44ms
3-key literal      106ms       75ms
new Pt(x,y)        288ms      221ms
```

But the SUITE regressed hard, 2.82x -> 3.05x — json-large 545->610ms,
markdown-render 730->863, map-set-heavy 790->999, async-promise-chain 752->910.
The reason is structural: `HeapObj::Object(ObjMap)` stores the map INLINE, so
`HeapObj`'s size is the max over all variants and four inline slots grew EVERY
heap slot — strings, arrays, numbers, all of them. Cache footprint dominates the
allocation saved. Dropping to two slots still regressed. Reverted.

**Boxing does not rescue it either — MEASURED.** `HeapObj::Object(Box<ObjMap>)`
was built (442 pattern sites, nearly all unchanged because `Box` auto-derefs;
110 construction sites needed `Box::new`). Two findings:

1. `HeapObj` stayed at **112 bytes**, because `ObjMap` (96) is not the only fat
   variant — `Combinator` (8 fields, three `Vec`s, ~104 bytes) is the other one.
   Boxing `ObjMap` alone shrinks nothing.
2. The extra allocation costs **~20% on construction** on its own: `{}` 21->32ms,
   3-key literal 104->123ms.

Boxing `Combinator` too would take `HeapObj` to ~80 (then `Generator` sets it).
By the SmallVec calibration above — +118 bytes cost 8% — a 32-byte reduction is
worth roughly 2%, which does not pay for the 20% construction cost.

**Three failed directions is the finding.** Interning (hash > malloc), inline
storage (grows the enum), and boxing (adds an allocation) all fail for the same
underlying reason: `ObjMap` lives INSIDE `HeapObj`, so its size is charged to
every heap slot and its storage cannot be made inline OR indirect without paying
somewhere else.

**The design that squares it is a side arena**: `HeapObj::Object(u32)` indexing
a dedicated `Vec<ObjMap>`. Then the enum is tiny (so every string/number slot
shrinks), `ObjMap` can carry inline property storage (so a small object needs no
Vec allocations), and there is no per-object `Box` because the arena slot is
amortised. That is a real structural change — GC has to trace and compact a
second arena — and it should be attempted only with the whole gate plus the
117-program differential set, not incrementally.

Also still open, and independent: give `NewObject` a key-count hint so the
literal path pre-sizes instead of regrowing (6 keys pays ~36ns/key at the tail
against ~17ns steady-state), and skip the `pos()` existence probe when building
an object literal, whose keys the compiler already knows are distinct — that
probe makes literal construction O(n^2) in key count.

Second-order once that lands: fold `keys`/`vals`/`attrs` into one `Vec<Prop>` to
turn three allocations into one, and give `NewObject` a key-count hint so the
literal path pre-sizes instead of regrowing.

### B16 — Where the JIT actually reaches, per bench

Census with `ZIPP_JITLOG=1` (`bench/tiers.sh` extended). Useful because "the JIT
is worth 5x" is only true where it runs:

```text
bench                   INT  MEM black callmix deopts
parse-large-js            1    8     0     0      0
json-large                1    5     0     0    101
markdown-render           4    7     0     0      0
map-set-heavy             0    0     2     8      0
typedarray-math           1    8     0     0      0
regex-log-scan            1    4     2     2     64
class-prototype-hot       1    3     0     0      0
async-promise-chain       0    1     3     1      0
polymorphic-objects       0    7     2     0    131
sparse-array              0    8     1     0    128
```

**map-set-heavy compiles NOTHING** — all eight loop regions are declined by the
call-mix gate. Its JIT time (890ms) equals its interpreter time (876ms). Same
story for async-promise-chain (one region, three blacklisted).

The obvious move — relax the gate — was tried and reverted: it gives
map-set-heavy 6% but costs async-promise-chain a reproducible 8%, netting zero.
The gate is right that a region whose calls always fall back is not worth
compiling. The correct order is the one the whitelist already encodes: give the
method an INTRINSIC first, then whitelist it. `Map.get`/`set`/`has` are the
candidates for map-set-heavy.

Sizing it honestly first, though: map-set-heavy is 890ms against node's 564ms —
**1.58x, our best bench ratio, while running fully interpreted**. Its lookup is
already O(1) (`CollIndex`). Compiling it perfectly caps out at ~326ms, and
realistically recovers less than half. That is ~3% of the ~5.1s parity needs.

The deopt columns were chased too. `polymorphic-objects` (ips 145, 182),
`sparse-array` (ip 27) and `json-large` (ip 55) are **all `SetIndex`**, and both
causes are deliberate:

- a SPARSE write (`i > len`) resizes-with-holes, possibly hugely, and the
  helper deopts so the allocation happens in the interpreter where a panic
  unwinds through normal Rust rather than across an `extern "win64"` boundary;
- a NEW key on a plain object is a shape change, which reallocates `vals` and
  invalidates the inline caches that address values through `vals_ptr + slot`.

Neither is a bug, but each costs 64+ deopts and then an eviction with
`retry=false`, so the region is lost to the interpreter for the whole run. The
tractable half is the sparse write: handling a SMALL gap inline (push a bounded
number of holes) would keep those regions alive without reintroducing the
unbounded-allocation hazard. The new-key half needs the inline caches to stop
caching raw `vals` pointers, which is the same property-storage item as B1/B3.

Sizing, so nobody starts here expecting a lot: `sparse-array` is 163ms against
node's 50ms, and `polymorphic-objects` 722ms against 288ms. Recovering both
evicted regions perfectly is worth ~2-3% of the parity gap.

### B8 — CORRECTED: the regex ENGINE is not the problem

This section previously said regex was "41.8% of the remaining gap, and not
reachable by tuning the wrapper — `regress` is a backtracking VM; V8's Irregexp
compiles each pattern to native code", and named an engine rewrite as the single
largest item. **That is wrong, and the measurement that shows it is cheap to
repeat.**

Matching cost is FLAT in subject length, which is what a working literal
prefilter looks like — `regress` already has one (`startpredicate.rs`,
`bytesearch.rs`, memchr/memmem), and zipp already feeds it the byte path
(`find_from_ascii`, plus the `ascii_twin` compile):

```text
/zqx/.test(s), no match       zipp     node
  subject 20 chars            105ns     15ns
  subject 10,000 chars        200ns     20ns      → ~0.01ns/char, i.e. memchr
  2000-char scan × 200k        25ms     42ms      → FASTER THAN V8
```

The gap is entirely in the JS-level wrapper, and it splits cleanly:

```text
                              zipp     node
  test, matches early          30ms      3ms     ~135ns fixed per-call
  exec, matches early          76ms      7ms     +~230ns result construction
```

**Fixed per-call (~135ns).** The builtin-method dispatch chain, plus
`regexp_exec_fast_ok` on every call (two HashMap probes and an `exec` lookup on
`RegExp.prototype`). Identical in kind to the ~70ns every String method pays —
`charCodeAt` and `length`, which have inline JIT fast paths and skip the chain
entirely, are at parity with node.

**Result construction (~230ns, and ~530ns more for two capture groups).** Each
`exec` allocates the result array, an `ObjMap` for its properties (three `Vec`s),
and a **`String` per key** for `index`/`input`/`groups` — `ObjMap::define` does
`key.to_string()`. Roughly eight allocations per match.

So the top item is NOT an engine rewrite. It is **property storage and name
interning** (B1/B3), which now has three independent measurements pointing at it:
string-method dispatch, regex per-call dispatch, and regex result construction.
An engine rewrite would buy nothing on these benches — our matcher already beats
V8 at the thing an engine rewrite would improve.

### B8b — Regex work that IS worth doing

41.8% of the remaining gap, and not reachable by tuning the wrapper. `regress`
is a backtracking VM; V8's Irregexp compiles each pattern to native code. The
existing byte path, `ascii_twin` byteopt compile and memchr prefilters are
already in use, so the ~9x per-match difference is the execution model.
Realistic options, in increasing order of work: emit a DFA/Pike-VM for patterns
without backreferences or lookaround (covers most real patterns); or compile
patterns to native code through the existing dynasm infrastructure. Either is a
multi-week epic. **Effort:** XL. Note the previously-quoted "~5x floor without a
native regex JIT" is consistent with this measurement.

### B7 — Optimizing tier (SSA + deopt)

Deliberately last, but note it is what the *majority* of the non-regex gap
needs: a hitting inline cache still costs 8.5 ns against node's 1.0 ns, and
that difference is per-operation boxing and tag-guarding, which no amount of
cache tuning removes. It consumes everything above — B3's shape feedback to
speculate on, B4's allocation admission to be worth entering — so building it
before the object model is stable means speculating against a moving target.

---

## 6. Known deviations and debts

Things that are wrong on purpose, or wrong and unfixed. Keep this list short and
current.

- **TypedArray named properties ignore the prototype chain.**
  `vm/props/member.rs` answers `length`/`byteLength`/`byteOffset`/`buffer`/
  `BYTES_PER_ELEMENT`/`@@toStringTag` from the instance, so
  `Object.setPrototypeOf(ta, {length: 7}); ta.length` reports the TypedArray's
  length where V8 reports 7. The spec-correct lookup is implemented
  (`ta_named_is_intrinsic`, `vm/typedarray.rs`) but cannot be enabled until
  **A5** — with a faithful walk, cross-realm TypedArrays return `undefined` and
  24 currently-passing tests break.
- **`console` is not an object.** It is a compile-time pattern match in
  `compile/`, so `typeof console === "undefined"` and `const log = console.log`
  throws.
- **Native stack overflow aborts the process.** `a.push(a); String(a)` — two
  lines — exits 127 with no catchable error, because recursion in the JSON and
  several array natives is not depth-bounded and the release profile is
  `panic = "abort"`. Node returns a catchable `RangeError`.
- **Extremely sparse arrays iterate in O(length).** `a[2**32-2] = 1;
  a.forEach(…)` walks the whole range. V8 does too (measured 58s), but zipp's
  per-index probe is far more expensive. Bounding the probe by the receiver's
  own element extent would fix it; it needs care around callbacks that add
  elements mid-iteration.
- **`Number.prototype.toString(radix)` drops the fraction** — `(1.5).toString(2)`
  gives `"1"`, not `"1.1"`.
- **No CI.** No `.github/workflows`. The gate in §2 is run by hand.
- **No profiler.** There is no way to attribute engine time to a source
  construct, which is precisely how the two reverted epics happened. A sampling
  profiler behind `ZIPP_PROF=1` would pay for itself immediately and is a
  prerequisite for honest work on B3/B6.

---

## 7. How to use this doc

- Check tasks off as they land. Record the **measured** delta in the commit
  message and update §1's tables; a task without a measured delta is not done.
- Every task carries the §2 gate. `Gain:` figures are measured unless marked
  inferred.
- Respect §3's floors. Only `typedarray-math` and `map-set-heavy` are true
  parity candidates before the substrate work lands.
- When a measurement contradicts this document, the measurement wins — update
  the document in the same commit.
