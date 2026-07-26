# zipp-vm — roadmap

> **Goal:** match V8 on speed, and pass all of test262.
>
> Engine: `crates/zipp-vm/src` — a NaN-boxed, explicit-frame register VM with
> per-call-site inline caches, a native x86-64 OSR JIT (dynasm), and a
> whole-heap mark-sweep GC.
>
> Last re-measured **2026-07-26**. Every number below was measured on this repo;
> nothing here is an estimate unless it says "inferred".
>
> **Sections 1 and 3 below were written at 3.31x and several of their
> conclusions have since been REFUTED by measurement.** The corrections live in
> B8 (the regex engine is not the bottleneck — we beat V8 at scanning), B17 (key
> interning and inline storage both measured slower), and B16 (which loops the
> JIT never reaches). Read those before acting on anything here.

---

## 1. Where the project actually is

### Conformance — 96.97%

| slice | executions | pass | fail |
|---|---|---|---|
| ECMA-262 + `staging`, sloppy **and** strict | 96,029 | 93,122 (96.97%) | 2,907 |
| `intl402` (opt-in) | 3,341 | 563 (16.9%) | 2,778 |

The failures are extremely concentrated — this is a short list, not a long tail:

| cause | failures | note |
|---|---|---|
| static-semantics early errors never raised | **2,214** | 76% of everything |
| `staging` (SpiderMonkey-derived) | ~430 | |
| everything else in ECMA-262 | ~260 | |
| `intl402`: non-ISO calendars, `DateTimeFormat` | 2,778 | separate baseline |

`tools/test262-expected-failures.txt` is the checked-in baseline; a regression
is a `diff`, not a remembered number.

### Performance — geomean 2.72× slower than node (was 3.31×)

`bench/real/*.js`, best-of-5, output compared against node.

| bench | node | zipp | ratio |
|---|---|---|---|
| map-set-heavy | 625ms | 783ms | 1.25× |
| json-large | 265ms | 631ms | 2.38× |
| parse-large-js | 255ms | 617ms | 2.42× |
| polymorphic-objects | 294ms | 715ms | 2.43× |
| async-promise-chain | 297ms | 731ms | 2.46× |
| markdown-render | 240ms | 676ms | 2.82× |
| class-prototype-hot | 253ms | 753ms | 2.98× |
| sparse-array | 43ms | 156ms | 3.63× |
| typedarray-math | 174ms | 688ms | 3.95× |
| regex-log-scan | 408ms | 1745ms | 4.28× |

**Run-to-run variance is ±10–17%** — node's own `map-set` time has ranged
609–966ms and `markdown` 231–416ms across runs on the same machine. A
single-row move under ~10% is noise; re-run before attributing it to a change.
Track the geomean, which has moved 4.77× → 4.20× → 3.31× → 2.82× → **2.72×**.

⚠️ **These three tables drift.** README, this file and `bench/results_real.txt`
are maintained by hand from separate runs and have disagreed by up to 0.1×.
Treat `bench/results_real.txt` as authoritative; the fix is to generate both
tables from structured output (see §1b).

Startup is ~2× faster than node (26ms vs 53ms).

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

Harness gaps that keep costing real time (±10% run-to-run variance has already
produced at least two false readings this cycle): no raw samples retained, no
engine interleaving, mean-of-best rather than median with an interval, and no
separation of cold start / warm steady state / GC / RSS. A change under ~10% on
one row currently cannot be distinguished from noise without hand-repeating it.

## 2. The standing gate

Every engine change must pass, in full:

1. **Build:** `cargo build --release` — verify the binary mtime advanced.
2. **test262, BOTH tiers:** `tools/run_test262.py --dump-fails f.txt`, then
   `diff <(sort f.txt) <(sort tools/test262-expected-failures.txt)` — zero new
   entries. Repeat with `ZIPP_NOJIT=1`.
3. **Unit tests:** `cargo test --release` = **287 passing, 0 failed**. Check the
   SUMMED pass count; the suite count is invariant to deleting tests.
4. **Bench correctness:** `bash bench/run_real.sh` → `ALL_CORRECT=1`, default
   **and** `ZIPP_NOJIT=1`.
5. **GC stress:** add `ZIPP_GC_STRESS=1` when the change touches GC/heap —
   mandatory for anything in Track B3/B4/B6.

Any change touching the JIT must produce identical output with and without it;
`assert_jit_matches` in `crates/zipp-vm/src/lib.rs` pins that per case, and new
JIT work is expected to add cases there.

**Measurement protocol.** Baseline is the live `bench/results_real.txt`. Re-run
with `bash bench/run_real.sh` (or `BENCHES=<row> bash bench/run_real.sh`). Given
the variance above, an A/B needs best-of-7 on a quiet machine, and a claimed win
under 10% on one row needs a second run before it goes in a commit message.

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

- [ ] **A1 — Static-semantics early errors (2,214 failures, 76% of the total).**
  `crates/zipp-vm/Cargo.toml` pulls `oxc_parser` but not `oxc_semantic`, so the
  engine happily runs programs that must be a `SyntaxError`:
  `let x; let x;`, `let x; var x;`, `class C{constructor(){}constructor(){}}`,
  `a: a: ;`, script-level `export`, and invalid regexp literals in dead code.
  Add `oxc_semantic` (or an equivalent validation pass) between parse and
  compile, and map its diagnostics onto the engine's `SyntaxError` path.
  Concentrations: `language/statements/class` 274, `language/expressions/class`
  244, `language/block-scope/syntax` 201, `language/statements/switch` 127,
  `language/expressions/dynamic-import` 120, `literals/regexp` 165,
  `RegExp/property-escapes` 163.
  **Effort:** M–L. **Risk:** med — it must not reject valid programs; the
  expected-failures diff is the backstop.

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
