# zipp-vm — roadmap

> **Goal:** match V8 on speed, and pass all of test262.
>
> Engine: `crates/zipp-vm/src` — a NaN-boxed, explicit-frame register VM with
> per-call-site inline caches, a native x86-64 OSR JIT (dynasm), and a
> whole-heap mark-sweep GC.
>
> Last re-measured **2026-07-25**. Every number below was measured on this repo;
> nothing here is an estimate unless it says "inferred".

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

### Performance — geomean 3.31× slower than node

`bench/real/*.js`, best-of-7, output byte-identical to node.

| bench | node | zipp | ratio |
|---|---|---|---|
| map-set-heavy | 607ms | 977ms | 1.61× |
| async-promise-chain | 333ms | 794ms | 2.38× |
| json-large | 232ms | 556ms | 2.40× |
| polymorphic-objects | 299ms | 807ms | 2.70× |
| parse-large-js | 237ms | 692ms | 2.92× |
| class-prototype-hot | 260ms | 777ms | 2.99× |
| markdown-render | 236ms | 942ms | 3.99× |
| typedarray-math | 170ms | 696ms | 4.09× |
| sparse-array | 52ms | 286ms | 5.50× |
| regex-log-scan | 411ms | 3354ms | 8.16× |

**Run-to-run variance is ±10–17%** — node's own `map-set` time has ranged
609–966ms and `markdown` 231–416ms across runs on the same machine. A
single-row move under ~10% is noise; re-run before attributing it to a change.
Track the geomean, which has moved 4.77× → 4.20× → **3.31×** (the last step
being the mimalloc global allocator plus the string-receiver borrow).

Startup is ~1.9× faster than node (30ms vs 58ms).

### What the engine already wins

Scalar-numeric kernels, self-recursive integer functions, `s += …` string
accumulation, `charCodeAt`/`s[i]` scan loops (measured at 1.7 ns/op — parity),
and non-capturing arithmetic array pipelines all compile to native code with no
per-element call. None of this carries to the ten benches above, which are
bound by property access, allocation and enumeration.

---

## 2. The standing gate

Every engine change must pass, in full:

1. **Build:** `cargo build --release` — verify the binary mtime advanced.
2. **test262, BOTH tiers:** `tools/run_test262.py --dump-fails f.txt`, then
   `diff <(sort f.txt) <(sort tools/test262-expected-failures.txt)` — zero new
   entries. Repeat with `ZIPP_NOJIT=1`.
3. **Unit tests:** `cargo test --release` = **249 passing, 0 failed**. Check the
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
- [ ] **B5.2 Lazy RegExp result objects.** `vm/proxy_regexp.rs` — `exec` builds
  ≥8 heap objects per call and is ~69% result construction; `matchAll` ~79%.
  Only ~13% of `regex-log-scan` is actually matcher-bound, so do this **before**
  any linear-matcher epic. **Effort:** M. **Gain:** regex −25–35%.
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

### B7 — Optimizing tier (SSA + deopt)

Deliberately last. It consumes everything above: it needs B3's shape feedback to
speculate on, and B4's allocation admission to be worth entering. Building it
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
