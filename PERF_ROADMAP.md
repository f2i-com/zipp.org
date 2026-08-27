# zipp-vm — roadmap

> **Goal:** match V8 on speed, and pass all of test262.
>
> Engine: `crates/zipp-vm/src` — a NaN-boxed, explicit-frame register VM with
> per-call-site inline caches, a mature native x86-64 JIT, a guarded ARM64
> whole-function baseline (both dynasm), and a generational nursery backed by a
> mark-sweep old space.
>
> Last re-measured **2026-08-25**. Every number below was measured on this repo;
> nothing here is an estimate unless it says "inferred".
>
> **Sections 1 and 3 below were written at 3.31x and several of their
> conclusions have since been REFUTED by measurement.** The corrections live in
> B8 (the regex engine is not the bottleneck — we beat V8 at scanning), B17 (key
> interning and inline storage both measured slower), and B16 (which loops the
> JIT never reaches). Read those before acting on anything here.

## Current experiment registry — 2026-08-25

This is the short, current view. Detailed measurements and retained raw results
are in the numbered B entries and under `bench/`.

The Wave 39 boundary audit closes Wave 30's effectful-helper debt for routed JIT
helpers: pure preflight failure may deopt, while an unwind after possible
allocation, mutation, or user-code re-entry aborts instead of returning a
replay sentinel. New effectful helpers must use that boundary or prove
panic-before-effect; the generic catch/deopt convention is still not, by
itself, a transactional boundary.

B147's final publication gate compares actual manifest/harness/input content
with the corresponding `HEAD` blobs both before and after measurement; it does
not trust `git status`, so `assume-unchanged` and `skip-worktree` cannot hide a
stationary edit. Any inherited Zipp, allocator, Node, Deno, or Bun runtime
control also makes the hostile run diagnostic-only—including an allowlisted,
fully recorded value—closing `NODE_OPTIONS=--jitless` and `ZIPP_NOJIT=1`
benchmark manipulation.

| item | current disposition | measured result |
|---|---|---|
| **B203/B204/B205 charter: the three remaining structural epics, sized** | **arena (biggest, policy fork), types-churn per-arm narrowing (~10% of the row, new compile shape), nanoid fused append lane (bespoke)** | Where the parity ledger stands after the B194–B201 arc (survival 3.67→2.91, hostile 1.0798→1.0218), every remaining behind-node row's next lever is a multi-hour structural epic, recorded here sized so any session resumes warm. (B203, THE ARENA): the unserved malloc mass (survival's 74%, every row's construct-in-place, the 80-byte sweep slot swap, the enum-layout blocks that stopped the cell/tombstone/inline levers) all point at repr-stable object storage. TWO shapes: (a) an `ObjBox` newtype over arena cells — ~132 construction sites + Drop-discipline chokepoints (free_slot/replace/courier/heap-drop), safe-sandbox keeps `Box` via a cfg type alias; (b) a `#[global_allocator]` 112-byte size-class cache in front of mimalloc — 20 lines, but it EXEMPTS the guest-data object class from the user's secure-allocator hardening, which is the USER'S policy call, not the campaign's (same class as the recorded json/regex trade; the B195a policy shares say survival 19%/router 16%/react 16% of those rows is already the hardening price). DECISION NEEDED from the user for (b); (a) needs no permission but a half-session of careful unsafe. REPRICED before building: the recycle pool already serves ~71% of survival's ObjMap deaths (pushed 364k/popped 319k of ~450k literals), the B195a “74% unserved” figure counted ALL slot kinds, and survival's strings/closures are slot-only (cons labels never flatten; `apply` captures nothing, so upvalue Vecs are empty) — the ObjMap arena's honest INCREMENT over the landed pools is ~8–12ms on survival (~4–5%) and similar fractions elsewhere, not the −70ms the conflated figure implied. Still worth building for construct-in-place + the unserved tail + retiring the pool's demand machinery, but it is no longer the silver bullet; survival's residual decomposes as GC trace+sweep bookkeeping (~35ms), the hardening policy (~35ms), mutator cross-call+field work, and the pool-unserved tail. (B204, TYPES-CHURN NARROWING — REPRICED DOWN by the bytecode scout): the dispatch prefix (ips 0–28, pure int ops ×1.2M calls) plus the int arms ALREADY execute at ~0.6ns/op in the boxed tier (38ms native for ~60M ops — the MEM tier's int arms are that good), so per-arm typed sub-regions would recover little; the row's ~21ms gap decomposes into the ALLOCATING arms (~450k object/array/string constructions per run) plus 13ms GC — i.e. types-churn is another ARENA row, not a narrowing row. The narrowing shape stays recorded for workloads whose arms are numeric-heavy rather than allocating. (B205, NANOID — structure nailed by the follow-up scout): the bench OVERRIDES `Math.random` with a seeded xorshift over a module-level `let` — which compiles as a GLOBAL slot, and fn2's body is ALREADY the fused `global-xorshift chains=1 steps=3` Tier-C form; the 10M inner calls take the METHOD-INLINE identity arm (zero window fills — ICSTATS shows only the 240k per-ID outer calls), so the remaining ~5-6ns/char over node is the mi-guarded call + property route + the un-fused mul/floor/index/append tail. THE BUILD: recognize the exact window `CallMethod(Math.random) → Mul k → Bitwise|0 → GetIndex(alphabet) → Concat(id)` in a region/whole-fn loop and emit ONE fused sequence that inlines fn2's 3-step global-xorshift directly (no call), converts, scales, indexes the PINNED alphabet string (STR_PIN precedent), and appends via the chain-fast link — guards: the mi identity/global-route epoch for the `random` binding, fn2's xorshift-plan validity, the alphabet pin. Estimated −40–60ms (nanoid 1.74× → ~1.2×); the recognizer/emitter scale mirrors TiercUpvalXorShift + chain-fast (~200-250 lines). Also standing: the retained json/regex ≈ 1.20× rows remain the allocator policy (secure-off = parity, B182), and async-promise-chain 1.15× sits in its documented retrain band. |
| **B202-scout: SORTED minor sweeps REFUTED in one experiment** | **survival 222→229ms, minors 43→44.7ms — the young log's natural order already wins** | The sweep epic's cheapest hypothesis — free the minor's dead set in slot order so the six parallel-array writes stream — measured WORSE: one `sort_unstable` per minor (~80k u32s) plus the reordering cost more than the misses it removed, because the log's alloc-order + the free-list's LIFO reuse already give the sweep temporal locality that sorted spatial order cannot beat. Reverted same-session. What remains of the sweep's ~39ns/dead: the 80-byte `mem::replace` slot swap and the per-dead bookkeeping, both pointing (with the cell/array/object levers before them) at the same structural endgame — repr-stable slot payloads / the ObjBox arena — rather than at walk-order tweaks. |
| **B217 LANDED (CORRECTNESS): `hoistable_length`'s reject scan was a STALE BLACKLIST — four more live wrong answers, and the predicate is now fail-closed** | **no speed claim; `len-fills=1` confirms the hoist still fires for the read-only `for (i < s.length)` idiom it exists for** | The five-lens fan-out chartered by B216 went looking for more of that bug's CLASS and found something better: the class is a single instance (see below), but two lenses independently noticed the same function's OTHER half was rotten. Its “can anything here run user code or change a length?” pre-scan rejected exactly four ops — `SetIndex`, `SetProp`, `Call`, and `CallMethod` other than `charCodeAt` — while `region_can_compile` has since gone on admitting more: `CallMethodComputed`, `DeleteIndex`/`DeleteIndexConcat`, `SetIndexConcat`, `CellSet`/`UpvalSet`, `IterNext`, `ForInLive`, `StaticFn`. Each admits a hoist over a loop that DOES change the length. FOUR independent verifiers each re-derived the mechanism from source and wrote their OWN minimisation rather than trusting the finder's: a computed call `fns[0](p)` that pushes/pops the measured array (1049816 vs 1050000), an array global rebound through a computed call (899157 vs 900000), and a `delete px[k]` whose Proxy `deleteProperty` trap reassigns the measured string — the loudest reading 1999972 where node and the interpreter both say 1300000, a 54% wrong answer. All four reproduce on the B216-fixed binary, so this is a genuinely separate defect. THE FIX IS STRUCTURAL: a blacklist is the wrong shape for the question, because every future op added to region admission silently re-opens it. The scan is now a FAIL-CLOSED WHITELIST — pure producers, global stores, arithmetic/compare, in-region control flow, reads, and the one proven-read-only `charCodeAt` — with `_ => return None` catching everything else, the same discipline `cross_ud` uses (and whose absence was B208). A new op now costs at most a missed optimisation, never a wrong answer. `tests/hoist_store_order.rs` grows the computed-call and Proxy-delete cases (six node-oracled tests total). |
| **B220 LANDED: the sweep-AND-DELETE-fed key-buffer pool — the third member of the B194/B196b pool family** | **polymorphic-objects −2.08% (446.8→437.5ms, 21 reps); json-large and markdown-render null; 3-row geomean −0.68% [−1.0,−0.3]. With B219 the row compounds from capture #11's 1.045× to ~0.998× — UNDER PARITY, the first retained row this campaign has crossed by charted lever rather than by retrain** | Evidence first, because B219 had just repriced its own estimate 5×: a new `bench_append_decompose` micro prices the dynamic append's Rust side at **51.9ns owned / 33.2ns with a recycled buffer / 27.8ns on the no-alloc planned path**, so the per-append key malloc is 36% of it and a pool captures 78% of what the far more complex site-learned-plan adoption could. The pool: a dying object's OWNED key Strings are drained at `free_slot` (a `Planned` map's keys belong to a shared plan Arc and are not ours to take) and `obj["prefix" + i] = v` fills a recycled buffer instead of mallocing; bounded at 4096 entries × 64 bytes so a pathological key set cannot pin memory, and charged in `resident_bytes` (the B207 lesson). THE DIAGNOSTIC THAT MATTERED: the first build measured only −0.53%, and rather than accept or abandon it the new `[keypool]` oracle line said why — **served=450,560 missed=2,249,440**, a 16.7% serve rate. The sweep is a poor feed because objects do not die fast enough; DELETES are the good one, since `delete obj[k]` frees a buffer immediately and the very next append usually wants it. Routing `ObjMap::remove_at`'s freed String (it already owned it and simply dropped it) into the pool took the serve rate to **50.0%** and the row win to −2.08%, a 4× improvement from a one-line feed change. |
| **B219 LANDED: hash the dynamic append key ONCE — thread one `prop_tag` through the presence probe and the index insert** | **polymorphic-objects −2.5% (457.7→446.3ms, 21 reps; 1.045× → ~1.019×), json-large −1.0%, class-prototype-hot null; 3-row geomean −1.11% [−1.85,−0.46]. The lever hunt's −≈12% estimate for this piece is REPRICED to −2.5% — about 5× optimistic** | The retained-row panel attributed polymorphic-objects (the suite's only `obj["prefix" + i] = v` row, and 97% of its wall time is the dictionary-churn phase) to the APPEND path: 1.8M shaped appends at 127ns against node's 53. Its decomposition named “~2.6 hashes of the same 7-byte key” as the largest slimmable term. Confirmed by reading: `try_set_index_concat_prebuilt` hashes the key in `ObjMap::pos` for the presence probe and then AGAIN in `index_appended` for the table insert (`prop_tag` is a per-byte FNV loop plus a 3-round finalizer). This threads one tag through both: `PropIndex::{find,insert}_tagged`, `ObjMap::{pos,push_data}_tagged` and `index_appended_tagged`, with the untagged spellings kept as wrappers so no other caller changes. SAFETY IS STRUCTURAL: the tag is only an accelerator — every table hit is still confirmed by a real string compare — so a wrong tag could only cause a MISS, never a wrong answer; and a `debug_assert_eq!(tag, prop_tag(key))` at each tagged entry point pins that anyway (475 DEBUG-build lib tests run with those assertions live). `ZIPP_NO_HASH_ONCE=1` recomputes at each site (the comparator this was measured with). WHAT THIS MEANS FOR THE ROW: −2.5% leaves polymorphic-objects at ~1.019×, so hashing was NOT the dominant term. The remaining append cost is the per-add key String malloc under mimalloc-secure, the per-add prototype proof, `shape::add`'s own edge hash, and Vec growth — the panel's other two pieces (a sweep-fed key-String pool, and site-learned `PropKeys::Planned` adoption) stay chartered, now with an honest prior that their estimates are also likely optimistic. Note the row's keys REPEAT (“prop_0”..“prop_59” for every object), so key interning is the shape-appropriate idea the pool piece should be judged against. |
| **B218 LANDED: eager `Promise.all` resolve-element jobs — collapse the per-element queue round-trip when the queue is empty, defer the settle to one job** | **async-promise-chain −3.5% [−5.1,−2.9] non-PGO (the retained-row scout measured −6.5% [−7.7,−5.9] PGO-grade; PGO reading to be confirmed at the next capture); map-set-heavy and json-large null; ALL_CORRECT** | The retained-row lever panel isolated this row's whole deficit to `partC`: partA (1.5M then-chains) BEATS node by ~35ms and partB (1.5M awaits) is at par because the settled-await trampoline already collapses those round-trips, but partC's `Promise.all` batches spend ~18ms in promise machinery, mostly 3M `CombinatorStep` queue round-trips. The lane: when the microtask queue is EMPTY and the element is an already-fulfilled native promise, run the resolve-element job at subscription instead of queueing it. ORDER-PRESERVATION is the whole question, and the argument is two-part — an empty queue means the collapsed job would have been the very next to run anyway, and the job runs NO user code (it records a value and decrements `remaining`, both combinator-internal); the one observable event, the result settling, is NOT run eagerly but re-queued as a single `CombinatorFinish` standing where the FIRST element job would have been — which puts the result's reactions exactly where the spec's LAST element job would have put them, since that settle queues them behind everything already queued. The moment anything else is in the queue (a thenable's `then` getter, a pending element, an enclosing job) the lane switches itself off. `tests/combinator_job_order.rs` pins ten node-oracled interleavings chosen to attack exactly that argument: more unrelated jobs than elements and fewer, attachment in a later job, a pending element mid-list, a NON-empty queue, nesting, the empty iterable, a thenable whose getter is user code, a rejection, and the sibling combinators — all byte-identical with the lane on, off, and under node. Refuted alongside it (recorded so it is not re-proposed): a `Promise.resolve` intrinsic-call lane is worth only ~7ns/call over the generic object-literal alloc pipeline, which is charted arena territory. `ZIPP_NO_EAGER_COMB=1` + fuzz mode `noeagercomb`. |
| **B215 LANDED: the collection intrinsic proof caches its RECEIVER half per (idx, method) under the receiver's heap version** | **map-set-heavy −3.1% (0.92× → ~0.89×); warm-router −1.7% [−2.5,+1.2] NULL at 15 reps; every other hostile row and json-large null — the judged −2.0..−2.5% router estimate is REFUTED, its Map traffic is a smaller share of that row than the lever hunt priced** | The bytecode-shapes lens measured `Map.get` at 21ns vs node's 4, and read the cost correctly: every call re-runs the whole intrinsic proof (kind match, own-shadow `arr_props` probe, custom-prototype `proto_of` probe, realm check) before `coll_find` does any lookup. B183 already memoised the PROTOTYPE half (per kind+method, version+bits guarded); this adds the RECEIVER half — a 16-entry direct-mapped cache keyed (idx, method) and guarded by the receiver's own heap version. Soundness rests on a checked claim: every install that could change the answer bumps `versions[idx]` — an own-shadow ADD (the named-prop store bumps on add), `defineProperty` (bumps unconditionally), `setPrototypeOf` (bumps by documented design), and slot reuse (free bumps); a shadow DELETE only widens validity, and the cache holds proven-TRUE entries only, so its missing bump is harmless. The prototype half deliberately stays live: an in-place value overwrite need NOT bump an ObjMap version (`string_regexp_method_is_intrinsic` documents exactly this), so B183's version+bits pair remains the authority there and the new cache never short-circuits it. Root realm only — the multi-realm dance keeps the full proof. `ZIPP_NO_COLL_PROOF_CACHE=1` + fuzz mode `nocollproofcache`. |
| **B216 LANDED (CORRECTNESS): `hoistable_length`'s mutation check was ORDER-DEPENDENT — a live wrong answer whenever the store preceded the load** | **no speed claim; the length hoist narrows to the cases where it was always legal** | Found by the retained-row lever panel's polymorphic-objects scout while attributing that row — an unlooked-for find, and the third time a perf investigation has caught a wrong-answer class before a user did (B181, B189a, now this). `hoistable_length` admits a loop-invariant `g.length` hoist when the region never mutates `g`. It walked the region ONCE, accumulating `g` from `LoadGlobal{dst==obj}` while, in the SAME pass, bailing on a `StoreGlobal{idx}` with `Some(idx) == g`. For the extremely ordinary shape `s = "prop_" + p; acc += s.length` the STORE stands before the LOAD in instruction order, so it was examined while `g` was still `None`, invalidated nothing, and the hoist was admitted for a global the loop reassigns every iteration — the region then served the region-entry value's length for the whole loop. Node and the interpreter sum 1230000; the JIT summed 1229600. NOT the B190a quick-`.length` lane (`ZIPP_NO_QUICK_LEN=1` reproduced it identically), and the fuzzer's grammar never emitted the store-before-load shape. The fix is two passes: establish `g` completely, THEN reject on ANY store to it anywhere in the region, whatever its position. `tests/hoist_store_order.rs` pins four node-oracled cases — the original string shape, the array-rebinding twin, a store-AFTER-load case (the direction the old check did catch, so a rewrite cannot regress it), and a read-only global that must STILL hoist, which pins the fix as a narrowing rather than a blanket disable. The five-lens fan-out chartered here has since REPORTED: the order-dependence class is a SINGLE instance. Nine agents swept every admission/hoist predicate in codegen/{region_admit,plan_region,plan,int_splice,region_int,region_int_gpr,region_mem,proto_mem,inline,absint,fn_int,self_call,regalloc,mod}.rs and vm/engine/jit_plans.rs (~30 accumulate-then-invalidate loops by hand, 17 named and cleared with a stated reason each) and ran ~1,400 order-flipped differential programs plus 37 hand-built probes across node / JIT / NOJIT — zero further divergences. Two structural findings worth keeping: the MEM emitters are IMMUNE BY CONSTRUCTION (they consume fully-built plans and re-validate every one at RUNTIME via snapshot/version/deopt sentinels, so a wrong plan degrades to the helper, never to a wrong answer), and the correct template is already the common one — `filter_push_only_pins`, `plan_field_promotion`, `span_is_replayable`, `unify_homes_with_globals` and `build_ta_pin_plan` all do the full pre-pass first. The one genuine sibling (`plan_region.rs`'s ToPropKey Bool-key check, which types from the same forward walk) is backstopped by the type-consistency pass: two lenses and an independent third attempt of my own failed to make it diverge. What the fan-out DID find is a separate live defect in this same function — see B217. |
| **B214 LANDED: root the in-flight microtask instead of suspending GC for its whole scope — the top-level-await starvation fix** | **npm-nanoid −8.0% (paired median −7.8%, band −9.1..−7.0; 1.65× → ~1.52× territory) and a demonstrated UNBOUNDED-HEAP hole closed; hostile scripts and retained-13 null, ALL_CORRECT** | The lever-hunt GC-accounting lens isolated it with identical-hot-code twins: a static-import module runs 11 minors for nanoid's 240k allocs, the top-level-await form runs ONE (peak 241,429 slots vs the 16,384 budget — the collection requested at 65k live is never served), because `drain_microtasks` held `gc_lock_guard` across each task's entire scope and a TLA module's whole body IS one microtask; its judge found the robustness prong stronger than claimed (a call-free compiled loop measured a 56× working-set blow-up — 2,760MB vs 49MB — that half stays chartered, needing its own review of helper-entry safepoints vs the same-bits refetch-elision contracts). The fix: `Microtask` derives `Clone` (plain Values + indices); a copy of the executing task is stashed in `Vm::current_microtask`, traced by `mark_roots` beside the queue with the exact same four arms, saved/restored around nested drains; the whole-scope lock is gone, so the interpreter's ordinary safepoints (frame pushes, the B189b lane guard) work inside resumed bodies. `run_microtask`'s internals need nothing new — frame registers root call arguments exactly as the lockless builtin callers (map/forEach) already rely on. Verified: nanoid now runs the static-twin's exact cadence (11 minors, peak 65,636), checksum byte-identical; async-promise-chain +0.7% null (its 202 sweeps were already served by the between-task `maybe_gc`). `ZIPP_NO_MICROTASK_ROOT=1` restores the lock (pricing only — it re-opens the hole) + fuzz mode `nomicrotaskroot`; the gcstress soak mode now actually collects inside reaction bodies. |
| **B213 LANDED: caller-side skip of handler-excluded callees — the planners stop attempting cross lanes that can never fill** | **reactish-reconcile −12.8% [−13.9,−12.4] (2.08× → ~1.81× territory); every other hostile row null; retained-13 null (−0.14%)** | The lever-hunt bytecode-shapes lens proved react's whole 15-16ms residue was diff's for-of machinery poisoning its cross-entry eligibility (130,188 no-entry declines/run), and its judge measured a planner-only fallback at ~−5%; the landed fix beats that floor 2.5× because a decline is dearer than the isolated estimate: each one runs the specialized enter helper's full prefix (closure resolution, realm/eval-scope checks, entry lookup) and then re-dispatches through the ordinary framed path — the work done twice per call. A body containing PushHandler/PushFinally can NEVER hold a cross entry (the compile-complete path clears it — the handler stack needs a frame), so the exclusion is a static, permanent property of the proto; both plan arms (the call filter and the CROSS3M method-slot walk) now consult the shared `proto_has_handler_ops` predicate and take the framed path directly, which also stops parking such fids in the B199 pending set forever. ICSTATS: react's no-entry decline counter disappears outright, fast fills unchanged (251,211). The full for-of-over-Object.keys twin-body lowering (the same lens's −9..12% design) is hereby REPRICED: non-stacking with this wave, its remaining increment is the small difference between framed-call cost and a true indexed walk — parked unless fresh evidence re-prices it. `ZIPP_NO_HANDLER_CALLEE_SKIP=1` + fuzz mode `nohandlerskip`. Gates: suites, sandbox lib, warnings ×2, 8k soak, full test262 identical. |
| **B212 LANDED: the const+int concat memo — spec-invisible interning of `"prefix" + int`, self-defended by a frozen bit on `JsStr`** | **allocation-survival −24.9% [−26.5,−23.9] (!), types-churn −10.4% [−12.6,−8.2] (unpriced bonus), reactish −4.6%, warm-router −4.6%, 7-row hostile geomean −7.13% [−7.68,−6.28]; retained-13 null (+0.01% [−0.97,+0.41]); ALL_CORRECT everywhere** | The lever-hunt node-diff lens priced one-shot small-string creation at ~115ns full freight vs node's ~10ns and put 53ms of survival's row in label strings (`"node-" + (serial & 1023)` — 450k hits over 1024 keys). JS strings have no observable identity, so serving a CACHED heap index for a repeated `str + int` is unobservable — IF nothing can mutate the shared buffer. The engine's two in-place growth licences (the append/chain accumulators' compiler-proven linearity) both assumed fresh results, so the memo would have been a corruption engine; the fix is structural: `JsStr` gains a `frozen` bit set on every memo-served string, and both growth predicates (`str_append_inplace`'s `mutable`, `jit_concat_chain_fast`'s entry condition — the only two buffer-mutation gates in the engine, verified by a `get_mut`-on-Str sweep) now require `!frozen`, taking their existing fresh-copy fallbacks. The memo: 2048-entry direct-mapped on `Heap`, keyed (left_idx, int), guarded by BOTH the left's and the result's heap version (the B207 ABA discipline — slot recycling bumps versions, so a stale entry can never resurrect against a different occupant); never roots (a collected result is a plain miss); non-sandbox only; 40KB charged in `resident_bytes`; `[concatmemo]` oracle telemetry (99.5% hit rate on the survival kernel). Hooked before the format in `add_values`' str+int arm, so a hit skips fmt + alloc + the fresh string's whole GC life — which is why the row beats the −15..18% estimate: fewer allocations also mean fewer minors. Semantics: a 12-check node-oracle file (repeats, negatives, i32 boundaries, chain heads over served strings, append loops seeded from served strings, served+served concat) agrees byte-exact across node / zipp-JIT / zipp-NOJIT; full test262 sweep. `ZIPP_NO_CONCAT_MEMO=1` + fuzz mode `noconcatmemo`. Mirror int+str arm left unmemoized (no evidenced row). |
| **B211 CHARTER: the sandbox × integration-test matrix has ~103 pre-existing failures across ~40 targets — all one class** | housekeeping, no speed | First surfaced this wave: every prior battery ran the sandbox LIB tests only (452), and `cargo test --no-default-features --features safe-sandbox -p zipp-vm` over ALL test targets was never exercised. A `--no-fail-fast` census shows 1,513 passing and 103 failing, and every failure inspected is sandbox-by-design: JIT-mechanism proofs (JITLOG census / tier-reach / engagement counters — no JIT exists there), SharedArrayBuffer parity (excluded by hardening policy), depth-5000 recursion parity (the hardened frame cap throws by policy), and node-oracle slices assuming tiers. Four targets were gated this wave as exemplars (`build_floor_micro` cfg'd out, the two conddef tier proofs + `field_read_stream::mechanism_is_not_vacuous` behind the jit cfg, the SAB guard test `cfg_attr(ignore)`); spot checks confirmed pre-existence at c4666b4 via stash. The remaining ~99 want one mechanical sweep applying those four patterns — mechanical, low-risk, and it would make the FULL matrix a runnable gate. Until then the canonical sandbox gate stays `--lib` (452). |
| **B208 LANDED: `StrConcatChain` joins the `cross_ud` use/def table — the one-line W11/B124 staleness fix (found by the lever-hunt panel's IC-traffic lens)** | **warm-router −0.3..−0.8% (latch-derived floor); 243k full window zero-fills/run → 5** | The five-lens lever-hunt workflow's IC-traffic scout tabulated ZIPP_ICSTATS across all 8 behind rows against per-iteration counts and found 242,920 full cross-call window zero-fills per run concentrated on warm-router (125,967; 0.50/request — the /user and /search handlers each carry 2× StrConcatChain) and reactish (116,953; 27.8/render — Item). Root cause: the op was introduced by the W11/B124 n-ary concat fusion AFTER the cross table was frozen from the pre-W11 mask, so it fell to `cross_ud`'s `_ => return None` and every callee containing a fused chain declined to the fail-closed full zero-fill on EVERY cross call. `bytecode.rs` declares the op SEMANTICALLY IDENTICAL to `Add` for every operand pair, so it takes Add's exact `(u2(a,b), Some(dst))` licence. Verified: fills convert 125,967→4 / 116,953→1, all row outputs byte-identical, react null under noise as the fill-cost floor predicts (3-9ns/fill). |
| **B209 LANDED: `SetHomeObject` elision for super-free object-literal methods and accessors** | **allocation-survival −8.3% [−9.2,−5.2] at 15 paired reps (2.91× → ~2.6× territory pre-PGO); react/stable null as predicted; zero `SetHomeObject` left in survival's bytecode** | The lever-hunt node-diff lens measured survival's per-node method closure at ~50ms, ~12-16ms of it the dead [[HomeObject]] wire: `makeNode` emits MakeFunc + FinalizeObject + SetHomeObject per node though `apply` never references `super`, and each wire runs `record_closure_home` — a store barrier + ClosureHomeTable insert + the table's GC retain/prune share, 450k times. The [[HomeObject]] internal slot of a super-free method is UNOBSERVABLE (no reflection API exposes it), so the compiler now emits the wire only when `func_can_ref_super(fid)`: a post-compile scan of the method proto for any Super* opcode, any `eval_sites` entry (direct eval may contain `super.x` in a method context), recursing ONLY through `lexical_this` protos reached via MakeFunc/MakeClosure/MakeArrow (arrows capture `super` lexically; nested plain functions/classes establish their own home — `super` there is a parse error). All four emission sites gated (FinalizeObject method_slots, accessor, computed-key, static-key). Semantics: an 8-probe node-oracled file (proto super, nested arrow, double-nested arrow, direct eval, accessor, computed key, mixed elided+kept literal) matches node exactly; full test262 identical. `ZIPP_NO_HOME_ELIDE=1` restores always-wire (fuzz mode `nohomeelide`). |
| **B210 LANDED: the B185 free courier goes ADAPTIVE — per-item 4KB size gate, per-sweep 1MB bulk override, lazy thread spawn, by-ref kind pre-filter** | **vs ship-all: hostile 6-row geomean −1.61% [−2.89,−0.73] (react −5.0% [−5.3,−4.1], router −3.2% [−4.5,−0.4]); markdown-render's courier win PRESERVED (fully-off costs it +4.7% [+2.8,+7.6]); async-promise-chain's hidden courier tax removed (was −3.9..−4.5% recoverable by off, now +0.4% null)** | The lever-hunt latch-matrix lens found B185 flipped net-negative on 4 of 6 hostile object rows (21-rep, twice-replicated: ship-everything cost +6.1% react / +3.9% router / +2.8% stable / +1.8% mega vs off) — the B194/B196 pools intercepted the object/array mass it was built for. But the retained-13 gate read: markdown-render WANTS shipping (+5.7% when off, at 0.94× parity headroom) and async wanted it gone. New [courier] oracle telemetry split the regimes: react/router carry ~100KB of sub-4KB strings per sweep across many sweeps (shipping costs — the cross-thread free lands on mimalloc-secure thread-delayed lists the mutator reclaims in its own allocation path anyway), markdown carries ~2.5MB per teardown wave (shipping pays), async ships nothing but swept 3.2M non-shippable closure/cell/promise deaths whose fat-enum move through the courier match alone cost 13.5%→8.3% of profile samples in the GC phase. Three mechanisms, each measured: (1) per-item gate — only payload buffers ≥ 4KB ship in a small sweep; (2) bulk flip — a sweep whose sub-4KB shippable mass reached 1MB puts the NEXT sweep in ship-everything regime (markdown's waves re-engage within one sweep; the 1MB line sits an order of magnitude from both regimes); (3) the courier thread spawns lazily inside `ship` (a run that never ships never registers the thread) and non-shippable kinds are rejected BY REFERENCE before the payload moves. Trim-waves ride the same gate; `ZIPP_NO_COURIER_GATE=1` restores B185 ship-everything (fuzz mode `nocouriergate`), `ZIPP_NO_GC_COURIER=1` still forces fully-inline. json-large null. ALL_CORRECT on every A/B. |
| **B207 LANDED: the B194–B206 adversarial review — five mechanism-matched lenses, ten findings, ten confirmed by paired skeptics, seven root causes fixed** | **the B205 lane gains route-epoch + three heap-version guards (two REAL wrong-answer classes closed); B206's metering double-charge enforced away; nanoid holds −6.8% hardened** | The B178 review precedent, scaled: a 25-agent workflow (5 finders × the pool family / mirrors / cross-table / random-fuse / dynasm-ABI lenses, then 2 independent skeptics per finding — confirm-by-default REFUTERS) over everything the B194–B206 arc touched. All ten findings survived verification; the fixes: (1) THE ROUTE-EPOCH ESCAPE (major, found independently by two lenses): the fused `Math.random()*k|0` lane raw-reads/writes the CALLEE override's state global — a slot the CALLER's entry revalidation never scans, so `Object.defineProperty(globalThis,'seed',…)` after warmup left the lane on the dead slot (setter never fired, getter never seen, streams diverged from the interpreter). The lane now carries the cross3 lane's `JIT_GLOBAL_ROUTE_EPOCH == 0` guard (plus a plan-time epoch gate), and `tests/random_fuse_guards.rs` pins the node-oracled scenario (3150 setter hits — 50 ids × 21 chars × 3 steps). (2) HEAP-INDEX ABA (major): the lane's identity guards were pure bits-compares of baked heap values, and index recycling can resurrect bit-equal values naming DIFFERENT occupants (a recycled closure index re-assigned to `Math.random` would replay the stale xorshift; a recycled shorter alphabet would index past its end) — all three identities (Math, the random closure, the alphabet) now also compare `versions[idx]` via the pinned r13 base, the exact mechanism slot recycling already bumps. (3) B206 METERING DOUBLE-CHARGE (major, found by three lenses): the “metered bodies never carry yield heads” contract was documented but enforced nowhere — a metered Tier-C body charged the loop-head block then bailed, and the interpreter re-charged it, an UNBOUNDED per-call overcharge breaking the meter's exactness contract; `Jit::compile` now empties the heads whenever a meter is present (chokepoint enforcement, not caller trust). (4) SET_METER DANGLING ENTRIES (latent UB): set_meter drops Tier-C buffers but left `cross_table` entries armed — pre-B199 the epoch guard covered that resume, post-B199 nothing did; every wholesale `cross_entries.clear()` now zeroes the live table's entries. (5) CLEAR_CROSS_ENTRY MASK SENTINEL (minor but load-bearing): resetting the recorded mask to `u64::MAX` on clear meant every evict+recompile bumped `mask_gen` (B199's same-mask resume could never fire) AND the one mask change the gen guard must observe — a recompile declining to MAX — compared equal and did NOT bump; the clear now preserves the recorded mask. (6) `take_arr_buf` under-reserved recycled buffers (`reserve(cap - capacity)` with len 0 guarantees only the difference — now `reserve(cap)`), re-growing mid-build and silently draining the pool past `ARR_POOL_MAX_CAP`. (7) The recycle pools were invisible to `resident_bytes`, so the instrument-feature heap ceiling never charged pool-retained memory — both pools are now in the sum. Gates: the new pin test, suites 151 ok, sandbox 452, `-D warnings` ×2, 8k soak, full test262 identical, nanoid −6.8% with all guards (from −7.2% unhardened). |
| **B205 stage 2 LANDED: the compactId variant — dynamic `alphabet.length` over a CAPTURED alphabet, exemplar-resolved and cell-mirror-guarded** | **npm-nanoid −7.2% both stages together (166.9→154.8 interleaved latch medians; ~1.61× at capture scale from 1.74×); checksum byte-identical** | The named follow-up, an 8-op window: `UpvalGet alph · LoadGlobal Math · CallMethod random · UpvalGet alph · GetProp length · Mul · LoadInt 0 · Or`. The alphabet is a captured CELL fixed per closure instance: the builder resolves it from the LIVE exemplar frame (the B193 pattern — the compile trigger's own frame), requires an immutable flat `Str`, and bakes its length as k; per call the emitted lane adds ONE guard — the activation upval's cell read through the B201 cell-mirror authority compared against the baked alphabet bits — before the shared identity chain, so a different closure instance (another alphabet) or any rebinding takes the ordinary ops. The window's first `UpvalGet` sits before the fused span and runs as the ordinary op; the covered span is variant-dependent (7 vs 6). Plan keyed at the `LoadGlobal Math` ip. Gates: suites 150 ok, sandbox 452, `-D warnings` ×2, 8k soak, full test262 identical, ALL_CORRECT, checksum byte-identical with both arms firing (`RANDOM*64|0` + `RANDOM*len(28)|0`). |
| **B205 LANDED (stage 1): `Math.random() * k | 0` fuses to a fully-inline lane against a recognized seeded-xorshift override** | **npm-nanoid −4.2% (170.4→163.2 interleaved latch medians); retained −0.15% null; byte-exact checksum** | The charter design, built: `build_random_fuse_plan` recognizes the caller window `LoadGlobal Math · CallMethod random(0 args) · LoadInt k · Mul · LoadInt 0 · Or` (operand orders both ways, 2 ≤ k ≤ 65536, name proven “random”, Math slot routable), resolves the LIVE binding to a no-upvalue closure whose linked body is the EXACT seeded-xorshift shape (three `g ^= g SHIFT c` steps over one global + `(g>>>0)/4294967296` — the linked proto carries a trailing fall-off `ReturnUndefined` the first build missed: 24-vs-25 ops), requires the state slot routable both ways, SETTLES Math's hot mirror at plan time (a documented 4th settle event: the map is read at the same moment), and bakes the plan. `compile_proto_mem` then emits the six-op window as ONE lane: the B193-form identity chain (Math global VALUE bits → settled shape → `random` own-slot VALUE bits), the state slot's Int-tag guard (the double-literal seed bails once and settles), the three shifts in registers, an int-boxed state commit, the result as the INTEGER identity floor(u·k/2^32) via one 64-bit `imul`/`shr 32`, and EXACT f64 materializations of the window's intermediates (u32→f64 exact; u/2^32 and ×k stay under 2^53) — all guards precede the state store, so a miss is a pure prefix at the window ip. En route the sandbox `-D warnings` gate caught the classic cfg-theft (the new builder inserted between `build_cross_call_plan` and its `#[cfg(jit)]` attribute broke the no-JIT build — both fns now carry their own gates). Latch `ZIPP_NO_RANDOM_FUSE` + fuzz mode `norandomfuse` (and `nocrossretry`/`noyieldentry` joined the mode table for B199/B206's fallbacks). NAMED FOLLOW-UP: the compactId variant (`alphabet[(random()*alphabet.length)|0]` — dynamic length via an upval alphabet + GetProp) covers the row's other 5M chars for roughly the same again. Gates: suites 150 ok, sandbox 452, 8k soak with the three new modes, full test262 identical, retained −0.15% [−0.63,+0.24], ALL_CORRECT, checksum byte-identical. |
| **B206 LANDED (enabler, null in-corpus): yield-with-entry — a fn whose loops own reg-homed regions still compiles a Tier-C body, bailing at each region head; PLUS react's 130k “no-entry” declines root-caused to the handler-ops exclusion** | **retained −0.36% [−0.68,+0.29] null, react null; the mechanism's trigger population is empty in today's corpus but the W9 trade no longer costs call coverage** | Chased from react's ICSTATS: 130,188 runtime cross-call no-entry declines, all fid 10 (`diff`). FIRST theory — the W9 yield gate starves callable fns of entries — produced the mechanism: `should_yield_to_region` fns now compile WITH `yield_heads` (the live REG-region entry ips, `Jit::reg_region_heads`), and `compile_proto_mem` emits an unconditional exit-to-interpreter at each head (`mov [rsi], ip; jmp epilogue` — NOT `emit_region_bail`, whose skip-over shape would no-op a fall-through), so calls get the native lane and prologue while the loop's first iteration re-enters the interpreter whose back-edge OSRs straight into the reg region; metered bodies pass no heads (charge-then-bail would double-charge). SECOND diagnosis (the enriched `ZIPP_DECLLOG` census: `compiled=true` on 130,176 of the declines) found the ACTUAL cause: `diff` carries try/catch, and handler-op bodies are excluded from frame-free cross entries BY DESIGN (a frame-free call has no frame to hold the handler stack) — its calls already run FRAMED-native, so the decline costs only the failed attempt (~2ms/run), and react's real remainder stays with its GC per-death tax (~21ms; the non-moving-nursery architecture) and the policy share. The yield-entry mechanism lands anyway: correct (suites 150, `-D warnings`, 8k soak, full test262 identical, ALL_CORRECT), latched (`ZIPP_NO_YIELD_ENTRY`), null in-corpus, and it retires the W9 rule's blind spot for any future workload that calls a region-owning fn. Recorded follow-up: a FRAMED cross-call lane for handler-carrying callees is the direct-tierc-call parked epic's territory (the B126-era stale-len hazard set). |
| **B201c LANDED (null rider): the general Tier-C `UpvalSet` emits inline for NON-HEAP sources** | **all probed rows null (calls 64.1, nanoid 167, react 123, types 82) — whole-fn `UpvalSet` is simply not hot anywhere; the fused increment was the prize** | The write-path symmetry completing B201: at the whole-fn `UpvalSet` site, a non-heap source (the `0x7FFD` tag check routes heap values to the helper, which owns the write barrier) stores straight through the cell-mirror authority under the same guard prefix as the inline increment — activation upvals, sticky const/fn-name nonempty bytes, and the old value's UNINITIALIZED compare keeping TDZ exact. Landed on correctness-tightness and future-proofing (a later closure-heavy workload gets it for free); priced honestly as NULL today. Region-tier `UpvalSet`/`UpvalGet` stay helper-based (no activation there — a region-entry upvals scratch field is the recorded shape if a row ever flags). Suites 150 ok, `-D warnings`, 8k soak, full test262 identical, byte-identical rows. |
| **B201 LANDED: the cell-value mirror becomes the single cell authority, and the fused captured-counter increment emits INLINE** | **calls-closures −9.6% [−10.6,−8.6] (72→64ms); NAMED TRADE: retained-13 ~+0.5% diffuse interpreter-cell tax (readings +1.18/+0.88/+0.61 across three layouts against a baseline whose own A/A luck is −0.44)** | The enter-floor scout re-decomposed the row first: calls-baseline runs the SAME cross3 enter/close pair at 7ns/iter total, so the pair is ≤5ns and the closure row's 20ns remainder had to be closure-specific — it is the 2.4M captured-counter writes (`calls = (calls+1)|0`, already recognized and FUSED to one helper, but still one FFI per iteration). The blocker for inlining the write was the enum-layout dependence of `HeapObj::Cell(Value)`; the fix makes the B189 cell-value MIRROR the single authority: `Cell` is now a payload-free marker born only through `Heap::alloc_cell(initial)`, both write chokepoints (`cell_set`/`cell_write_no_barrier`) store to the mirror alone, the GC tracer and all 28 former payload readers go through it, and `cell_get` is a BLIND one-load read — sound because B198's kind-conditional invariant guarantees a non-Cell occupant leaves the field at UNDEFINED bits (the redundant kind-match was itself a second cache line). The fused increment then emits inline: activation upvals → cell index, two sticky nonempty bytes decline const/fn-name-cell programs (`const_cells_nonempty`/`fn_name_cells_nonempty`, set at the single insert sites, sticky-by-design — GC pruning may empty the sets but a stale byte only declines the inline lane), mirror load, `0x7FF9` int-tag guard (subsumes TDZ), low-32 wrap-inc, int-box, mirror store — no barrier needed (int values are barrier-exempt by `write_barrier_val`'s own filter), helper as pure-prefix fallback leaving OLD bits in rax either way so the register materialization is shared. THE TRADE, recorded per the nursery/allocator precedent: the retained suite reads ~+0.5% diffusely (every row +0.4..+1.3, nothing cell-heavy concentrated; three new layouts +1.18 [+0.71,+1.66] / +0.88 [+0.14,+1.37] / +0.61 [+0.18,+1.04] vs one baseline whose old-vs-rebuilt-old A/A reads −0.44 [−0.78,+0.13]) — the interpreter's cell traffic moved from the objs-slot line to the mirror line and the margins land where they land; taken for calls-closures −9.6% plus the architecture stages 3+ need (general inline `UpvalSet`/xorshift — the claw-back path for the same retained rows). GCSTATS oracle note: the inline lane bypasses the CELL_SET oracle counter (measurement-only). Survival/ephemeral spans-zero; suites 150 ok, sandbox, `-D warnings` ×2, full test262 identical, 8k soak ×2, byte-identical rows. |
| **B199 LANDED: the live cross-entry table — lanes stop baking entry addresses against a global epoch, and compile-order holes self-heal (defer + retry)** | **allocation-survival −7.2%/−9.2% at two layouts (retry component −2.1% latch-confirmed); calls-closures null (the enter floor, not the order hole, is its remainder)** | The scout's recorded fix shape, built in three pieces with two REAL bugs caught by the standing battery en route. (1) THE TABLE: `Jit::cross_table` — `#[repr(C)] CrossEntryRec {entry:u64 @0, mask_gen:u32 @8}`, 16 bytes, raw base re-derived through the VM per access (the mirror discipline) — replaces the baked-entry + global-`cross_code_epoch` pair in every emitted lane: the guard loads the LIVE entry (`[table + fid*16]`, fid a baked constant displacement; null routes to the helper), checks `mask_gen` against the baked zeroing mask's generation (bumped ONLY when a re-set actually changes the mask, so same-shape recompiles keep dependents valid), stashes the entry in the c3 scratch (+56) and calls through it. A recompiled or evicted callee is picked up or gracefully bypassed with NO stranding — the fragility class where any later compile's epoch bump silently parked every lane on its helper is gone by construction. (2) REGION DEFER: a “no entry yet” decline at a REGION compile defers the region a back-edge (capped) instead of baking the decline — both compile thresholds are 8, so the callee this very loop is heating lands its entry within a couple of edges and the region compiles WITH the lane; the cap started at 512 and the `instr_uses` region-coverage test FAILED — a loop calling a never-compiling callee would interpret for 512 edges — so the cap is 8 (one extra threshold window). (3) WHOLE-FN RETRY: no-entry declines are recorded (callee→caller, deduped, bounded) and the callee's first `set_cross_entry` parks the recorded callers' artifacts (yield/deopt retire discipline, no blacklists) for a recompile that bakes the lane — and the `tierc_activation_roots` cap test caught a SELF-EVICTION LOOP: a self-recursive fn's plan always sees its own entry missing (plans build before the set), recorded itself, and evicted itself at its own install, forever — self-pends are skipped (self-calls have their own lane). Verdicts: survival −7.2% [−8.3,−6.5] / −9.2% [−9.8,−7.0] two-binary at two layouts — its `apply` CallMethod/`makeNode` graph had order holes the defer+retry close — with the retry's own share −2.1% [−2.6,−0.5] by one-binary latch; calls-closures NULL (+0.4%) — its inner lane lands (JITLOG-verified) but the row's remainder is the per-call enter/close floor, now the named epic; react's +1.3% two-binary flag is NULL under the latch (noise-band structural residue, noted); ephemeral/calls-baseline flags died by adjusted+spans-zero. `ZIPP_NO_CROSS_RETRY` latch. Gates: suites 150 ok, sandbox, `-D warnings` ×2, full test262 identical, 8k soak clean, byte-identical outputs everywhere. |
| **B199-scout: the cross-lane compile-ORDER hole located and the naive retry REFUTED — the fix needs per-callee entry generations** | **calls-closures: 2.4M/2.4M calls run the helper at ONE site (`no entry yet` at plan time); retry-by-eviction measured 71.2→81.6ms and was reverted** | The row's remainder decomposed: 98.3% jit-mem, and `ZIPP_ICSTATS` shows 2,399,998 cross-call window fills — one per outer iteration. `ZIPP_JITLOG` names the hole: the arrow's inner `rotate` call site declined its emitted lane with “CROSS3 decline: no entry yet” (the callee had not whole-fn-compiled when the caller's plan was built), and a decline is baked forever; the reverse direction (main→arrow) got its lane because the order happened to favor it. A retry mechanism was BUILT AND REFUTED: record pending (callee→caller) at the decline, and at `set_cross_entry` evict the recorded caller's regions AND whole-fn (the yield-eviction discipline: park, reset counters, no blacklists) so its recompile bakes the lane. It converges for the recorded pair but STRANDS the rest of the graph: every `set_cross_entry` bumps the GLOBAL `cross_code_epoch` that every emitted lane guards by equality, so the retried caller's re-set knocked its OWN callers' lanes onto the helper — with no bail and no deopt signal, a guard-miss lane never recompiles — and the fills just moved sites (2.4M → 2.4M) while the eviction churn cost +10ms (71.2→81.6 interleaved medians). Reverted whole. THE RECORDED FIX SHAPE for a future wave: replace the baked-entry + global-epoch pair with per-callee validity — an indirect call THROUGH the live entries table (derived `[rdi+…]` per access, the mirror discipline, so a recompiled callee is picked up with no guard at all) plus a small per-fid generation for the baked uninit-mask (the one thing that must still match). That removes the stranding class entirely and makes compile order irrelevant — and it is the prerequisite the retry needs to be net-positive. |
| **B198 LANDED: cold-mirror maintenance is kind-conditional — the plain-object mass stops touching cell/this/upvals lines at alloc and free** | **shapes-megamorphic −2.2%/−3.1% at two layouts; boot 7.10→6.62ms and ephemeral 13.61→13.21 by direct interleave; rest null** | The B197 refutation's constructive sequel: the cold mirrors could not be PACKED (density loss on streamed fields), but they can be SKIPPED — only Cell and Closure occupants ever set them, so `refresh_mirror` writes them only in those arms and `free_slot`/`replace` clear only what the dying/old occupant OWNED, by the stated induction: a cold field is at its default unless the CURRENT occupant set it (replace clears the old kind's fields because a replace can change kind; `pin_mirror_dict` still clears wholesale). Every literal and instance allocation/free drops from four mirror-line touches to one. Measured: megamorphic −2.2% [−4.2,−1.5] / −3.1% [−4.6,+0.1] at two layouts; survival null (its sweep clears were prefetch-covered — batch sweeps stream the mirror arrays; the win concentrates at RANDOM alloc/free); the harness's small-cold-row flags (ephemeral +3.7%, calls-baseline +3.9%) died the standard deaths — calls at 21 pairs, ephemeral by adjusted≈null twice plus DIRECT interleaved medians showing the new binary faster on both boot (7.10→6.62ms) and the row itself (13.61→13.21ms). Gates: suites 150 ok, sandbox 452, `-D warnings` ×2, full test262 identical, 8k soak clean. |
| **B197 LANDED: the payload-charge cell leaves the per-object alloc/free paths when accounting is off; the COLD-MIRROR consolidation measured, REFUTED and reverted** | **survival −1.6% [−2.6,−0.7]; the refutation: calls-closures +4.1% [+3.2,+4.7] replicated at two layouts from 24-byte-record density loss on the streamed `upvals` field** | Two halves, one survivor. (1) LANDED: `resident_payload_charged` was read-modify-written on EVERY `free_slot` and written on every slot-reuse alloc even with accounting OFF (the default — every charge is 0), a pure extra cache line per object on the sweep's per-dead path; both sites now gate on `payload_accounting` (the lazily-enabled audit backfills exactly as before — cells not written while off are already 0). survival −1.6% [−2.6,−0.7] two-binary, everything else null, calls-baseline's +4.1% flag DEAD at 21 pairs (B141, again). (2) REFUTED, recorded so the B195 trade's boundary is explicit: consolidating cell/this/upvals into a 24-byte `ColdMirror` (the hot record's sequel; one `lea r,[idx+idx*2]` addressing, 2 fewer lines per alloc/free settle) was built, compiled clean, and measured calls-closures +3.8%/+4.1% at TWO layouts — its `function`-closures are not arrows, so each cross-call reads ONLY the upvalue base: the old dense u64 array held 8 slots per line, the record 2.67, and a purely-STREAMED single-field mirror bought nothing back from co-location. The B195 hot record won because a guard DEREFERENCES what it guards (shape→vals on one line); a cold field read alone only loses density. Rule for future mirror packing: pack fields read TOGETHER on one path; never pack a field that streams alone. (`Value::bits` became `const fn` en route — kept.) Gates: suites 150 ok, sandbox, `-D warnings` ×2, full test262 identical, 8k soak clean. |
| **B196b LANDED: the array-buffer pool — dying dense arrays' element Vecs restock the two NewArray paths** | **survival −5.9% [−8.0,−4.2] vs pool-off (the shell pool alone was −3.3%); objects −3.55%, router/retained null; ALL_CORRECT** | The B196a serve ceiling named it: survival's shell pool saturates near 29% of its dead mass because the rest is arrays, strings and closures — and the workload's `values: [a,b,c]` per node makes the ARRAY share the next biggest. Mechanism, the shell pool's exact twin: `free_slot`'s dying `HeapObj::Array(v)` with capacity 1..=32 (`ARR_POOL_MAX_CAP`, bounding retention and pop-waste) pushes its element Vec to `Heap::arr_pool` under the same refill scope (minors + majors), and `Heap::take_arr_buf(cap)` serves the interpreter `NewArray` op and the `jit_new_array` helper — cleared at pop with capacity retained (the pooled stale words are plain `Value` bits nobody dereferences, the same argument as the shells). Demand-trimmed (2×pops, len/2 decay, floor) at the same notes; excess rides the courier as `Item::Arr`; address-ordered by `v.as_ptr()` under the same first-major flip and `ZIPP_NO_OBJ_POOL_SORT` latch; the whole pool family stays behind `ZIPP_NO_OBJ_POOL`. Paired: survival −5.9% [−8.0,−4.2] at 21 pairs (both pools vs neither), objects-dir geomean −3.55% [−4.93,−2.38], router −0.3% [−3.1,−0.0], retained-13 −0.44% [−0.96,+0.14] with map-set's +1.3% spanning zero, `ALL_CORRECT` everywhere. Gates: suites 150 ok, sandbox 452, `-D warnings` ×2, FULL test262 identical, 8k soak clean. Remaining survival dead mass: cons strings (the `"node-" + n` label per node) and the closure-per-literal — the string side is the recorded nanoid append-lane epic's territory; the GC sweep-walk epic stands unchanged. |
| **B196a LANDED: the recycle pool serves EVERY workload — survivor gate retired, majors restock, serve order flips LIFO→address at the first major** | **survival −3.3% [−6.8,−2.1] AND router −2.1% [−3.1,−1.6] simultaneously; stable −5.6%, megamorphic −3.4%, react −1.3%, ephemeral −1.9%; retained-13 null** | The arena question got a 20-line answer first: B194's survivor gate existed because recycled shells SCATTERED what fresh sequential mallocs packed (+8.7% mutator on survival), so the pool sat idle exactly where the malloc round-trip hurt most (B195a: 111.6ns unpooled vs 43.3 pooled). FOUR measured iterations, each reversing a specific mechanism: (1) gate OFF + FULL ADDRESS SORT at the demand trim → survival FLIPS to −4.3% (adjacency was the whole locality story) and the sort is cheap for it; (2) MAJOR sweeps restock too (`obj_pool_refill_scope` around the vm/gc.rs dead walks) → survival −5.6% at 26% serve (319k pops, near the ~29% ceiling — the rest of its dead mass is arrays/strings, a named Vec-pool follow-up); but (3) the router read +2.1%, latch-bisected (`ZIPP_NO_OBJ_POOL_MAJOR` → null; `ZIPP_NO_OBJ_POOL_SORT` → +2.5% = THE SORT) — and a 1/4-survivor gate variant REFUTED (router's minors are all under 25%, and it cut survival's serve in half); (4) span-only ordering (addr>>16) REFUTED the cost-of-sorting theory from the other side — router healed but survival exploded +11.3%, proving router's cost was never sort WORK but serve ORDER: LIFO hands back the shells the sweep just touched (cache-warm — what a warm server's fast turnover wants) while address order packs spans (what a workload STREAMING a retained set needs). The discriminator is already in the heap: MAJORS are what deep retention causes — `obj_pool_addr_order` flips once at the first `note_gc_done` and the trim sorts only from then on; router (zero majors) keeps warmth forever, survival flips early and gets packing. Final paired verdicts (21/15-pair one-binary latch): survival −3.3% [−6.8,−2.1], router −2.1% [−3.1,−1.6], stable −5.6% [−9.0,−4.3], megamorphic −3.4% [−5.4,−2.1], react −1.3% [−3.0,−1.0], ephemeral −1.9% [−3.1,−0.2], retained-13 −0.53% [−1.10,+0.52] ALL_CORRECT. Latches `ZIPP_NO_OBJ_POOL_SORT`/`ZIPP_NO_OBJ_POOL_MAJOR` join the fuzz MODES (pure fallbacks); suites 150 ok, sandbox, `-D warnings` ×2, full test262 identical, 8k soak clean. The full ObjBox ARENA (B196) stays charted but the cheap experiment captured the serve-rate half of its win; the arena's remaining case is the 74% of dead mass the shell conditions exclude plus construct-in-place. |
| **B195a-scout: the engine round-trip decomposed + the secure-allocator share of every behind row quantified** | **engine alloc+free = 43.3ns pooled / 111.6ns unpooled vs 19.5ns pure construction; survival is 19.1% policy, router 16.4%, react 16.2%, types-churn 13.0%, stable 7.4%, mega 6.4%, calls 0.3%** | Two permanent micro variants join `build_floor_micro` (in-heap, private-field access): (f) the REAL `alloc_finalized` + `free_slot` round-trip on a live heap — 111.6ns/obj when every free ships to the courier and every alloc mallocs (survival's world: the survivor gate keeps its pool off), 43.3ns with the recycle pool serving (the churn rows' world) — against 19.5ns for pooled construction alone. So the slot/mirror/log tail is ~24ns and the malloc+courier delta is ~68ns/object; survival's ~140ms mutator IS 1.24M × the unpooled path. The policy share: a one-off secure-OFF probe build (`mimalloc` default vs `secure`, interleaved 7-medians, non-PGO) prices the hardened-allocator trade per hostile row — survival 257→216ms (19.1%), reactish 16.2%, warm-router 16.4%, types-churn 13.0%, stable 7.4%, megamorphic 6.4%, calls-closures 0.3% — the B176/B182 json/regex finding extended to the object corpus: a sixth to a fifth of the worst rows' remaining gap is the deliberate hardening price, NOT reachable by engine work, and the honest reachable targets shrink accordingly (survival nosecure is still ~3.6× non-PGO, so the structural gap dominates). NAMED NEXT EPIC (B196): the ObjMap ARENA — a 112-byte-class chunk slab (the ValSlab pattern, one size class) behind an `ObjBox` newtype replacing `Box<ObjMap>` in `HeapObj::Object`: cell pop + construct-in-place kills the malloc round-trip for EVERY object row including survival (packed chunk-sequential carving also retires the B194 pool's scatter trade and its survivor gate), with the known risk set: drop chokepoint discipline (free_slot/replace/courier/heap-drop), HeapObj Clone, safe-sandbox keeps Box, Stacked-Borrows hygiene per ValSlab precedent. Also landed here: `refresh_mirror` fused to one occupant match (measured neutral — LLVM had fused it — kept for clarity). |
| **B195 LANDED: the hot mirror record — shape + fid + vals consolidated to one 16-byte per-slot line** | **shapes-megamorphic −5.3% [−7.8,−3.4] (replicated at two layouts), shapes-stable −2.6% [−3.7,−1.1]; everything else null** | The alloc tail behind the B194 pool: `Heap::alloc` (slot reuse) touched ~10 distinct cache lines per object, six of them the parallel mirror arrays that `refresh_mirror` and `free_slot` each walk. Phase 1 folds the three JIT-hot mirrors (`shape_mirror`/`fid_mirror`/`vals_ptr_mirror`) into `Vec<HotMirror>` — `#[repr(C)] {shape:u32 @+0, fid:u32 @+4, vals:u64 @+8}`, 16 bytes — so a settle or clear is one line of traffic instead of three, and the emitted guard-then-deref sequences touch ONE mirror line: the addressing is `lea r, [base + idx*8]` then `[r + idx*8 (+off)]` (base + idx×16), same instruction count as the old two separate base loads at the shape+vals sites, +1 lea at fid-only sites. Converted: both emit.rs IC probes (head + hit), the cross3 fid guard, the B193 method lane (its shape→vals→fid triple now reads receiver facts off one line); `bump_version` invalidates shape+vals with two stores to one line and the fid field survives untouched (the B189 immutability contract, now enforced by field, not by array choice); `pin_mirror_dict`/`free_slot`/`refresh_mirror`/alloc-growth all collapse; cell/this/upvals mirrors stay separate (phase-2 candidates — only two emitted sites). host_api gains `JIT_HOT_MIRROR_RAW_OFFSET` + compile-checked field offsets. GATING was the story: the first two-binary A/B read survival +4.7% and react +1.2% — both DIED under the layout-perturbation replication (a comment-only relink: survival −0.1% [−1.6,+2.7], react +0.04%), while megamorphic's win REPLICATED (−4.6% / −5.3%) — exactly the fat-LTO ±2% discipline the method prescribes when no one-binary latch can exist for a structural change; the harness's per-run startup skew (8.2 vs 9.5ms) was also refuted by direct interleaved boot medians (6.74 vs 6.66ms — equal). Retained-13 −0.17% [−1.12,+0.59]; json +3.2% and parse +1.3% flags dead at 21 pairs (B141). Suites 150 ok, sandbox, `-D warnings` ×2, full test262 identical, 8k soak clean, `ALL_CORRECT` everywhere. Mechanism note for the ledger: the win concentrates where mirror access is RANDOM (megamorphic guard thrash, free-list slot reuse); dense sequential scans see the u32 shape array's 16-slots-per-line density traded for 4-records-per-line — measured null on stable/react/survival, a real trade to re-examine if a scan-heavy row ever flags. |
| **B194a LANDED: the hostile corpus joins PGO training — the published profile no longer rolls dice on cross-call helper layout** | **calls-closures 100.0→64.4ms on the trained profile (was +43% retrain luck); survival −7%; capture 1.0380× [1.030,1.044] best-ever** | The 6cbf669 capture cycle caught it: calls-closures read 2.41× vs the prior capture's 1.65× with NO relevant code change — latch-bisected on the capture binary to `ZIPP_NO_CROSS3` (100.0 default vs 72.9 latched), while the same-source non-PGO build had the lane as a WIN (71.4 vs 80.6) — i.e. the regression lived entirely in profile-guided layout. Root cause: `tools/pgo.sh` trained on the 13 retained rows + 6 micros only; the hostile corpus's helper-heavy paths (cross3 enter/finish, megamorphic method-IC fallbacks) were UNPROFILED, so their layout was whatever the retained profile implied — pure luck per retrain, and this retrain's luck ran out on a published target row. The W13-era verdict “do NOT change pgo.sh” (sparse-array-v2 drift = accepted variance) does not bind: that was chasing noise on a diagnostic row; this is a coverage gap for a corpus with its own README table. Fix: train on `bench/hostile/*/*.js` (+~3s; scripts only — the module rows sit far under parity). Trained-profile results: calls-closures 64.4ms (BETTER than the prior capture's 69.9 — the row had been paying stale-layout tax before, too), survival 215→200ms direct, and the 64c1583 capture prices the corpus at 1.0380× [1.030,1.044] (from 1.0798) with EVERY behind-node row at its series best. The superseded 6cbf669 captures are committed as the evidence pair. Standing lesson appended to the method: a capture-to-capture row move with no covering mechanism A/B is a LATCH-BISECT trigger, and PGO layout is a first-class suspect alongside machine drift. |
| **B194 LANDED: the Box<ObjMap> recycle pool — the sweep restocks literal-shell allocations, demand-sized, survivor-gated** | **warm-router −3.3% [−3.7,−2.4], shapes-stable −4.9% [−5.9,−2.8], reactish −1.7% [−3.7,−0.3]; survival NULL after three root-caused iterations; all-13 null** | B187 stage 3 opened with a decomposition ladder in `build_floor_micro` (variants live in `bench_support::floor_decompose`): today's engine construction (fresh Box + slab vals) is 38.6ns/obj, and a POOLED box + slab is 19.2ns — the mimalloc-`secure` Box malloc/free pair IS half the literal construction floor (23.6ns measured alone), which redirected the epic from the 603-match-site compact HeapObj variant to a zero-consumer-churn recycle pool. Mechanism: `free_slot` pushes a dying object's box to `Heap::obj_pool` instead of the courier when the shell is drop-cheap, and `alloc_finalized` pops one and overwrites it wholesale (`*b = finalized_from_store(..)` — the deferred plan-Arc decrement happens at the overwrite; shells hold NO heap references, so the pool is never traced). THREE MEASURED ITERATIONS to a clean gate, each root-caused not tuned: (1) a fixed 4096 cap served ~3% of a 100k+ minor window — replaced by uncapped sweep refill + a demand trim at the collection-done notes (`keep = max(2×pops, floor)`); survival still +7.6% because its minor→major cadence lands majors on ~zero-pop windows and the snap-to-floor dumped the pool each time — the trim gains a `len/2` decay bound (one halving per note); (2) still +4.4–2.8%: majors retire the whole live set in one walk and pooling that burst ballooned then dribbled it — refill is now MINOR-ONLY (`obj_pool_refill`, set inside `sweep_young`), and the recycle condition itself was reading the dying map's far cache line per object (+5.8ms on survival's 1.1M-object minors) — replaced by the `shape_mirror[idx] != DICT` settled bit captured before `free_slot` clears it (a line the function writes anyway; rich-but-settled shells pool too, their contents drop at overwrite, rare and bounded); (3) the residual +8.7% was MUTATOR-side with GC time equal: recycled shells hand back scattered addresses and survival RE-READS its retained set — churn rows never do — so refill is survivor-fraction-gated (previous minor's survivors < 1/16 of its log; survival's 17.5% shuts the pool off, pushed 277k→13.6k, and the row goes NULL +0.1% [−1.4,+2.6] at 21 pairs while stable's 39-minor churn keeps 622k pops and the −4.9%). Latch `ZIPP_NO_OBJ_POOL`; fuzz mode `noobjpool`; `[objpool]` telemetry under `ZIPP_GCSTATS` (pushed/popped/trimmed/resident, printed at heap drop). Gates (final build): suites 150 ok / 0 FAILED, sandbox 452, `-D warnings` ×2, FULL test262 identical (the three module-resolution fails predate the wave and are latch-proven pool-innocent), 8k soak ×2 clean incl. the new mode, retained-13 +0.11% [−0.53,+0.62] with the poly-v2 +1.3% flag dead at 21 pairs (B141), `ALL_CORRECT` everywhere. Micro: JS literal pipeline 176→154ms (−12.5%). Remaining floor: 19.2ns pooled construction (plan-Arc pair + 112B init + slab copy) + the slot/mirror/dispatch tail — the compact-variant question stays open behind it, and the sweep WALK epic is unchanged. |
| **B193 LANDED: the emitted CallMethod cross lane — shape-mirror method load + fid guard + the shared cross3 invoke** | **survival −3.5% mechanism A/B; all-13 null; a dangling-label defect root-caused in gating** | allocation-survival (3.67×, the largest hostile gap) profiles as jit 65% / gc 23.5% / interp 11.6%; its kernel calls `node.apply(...)` on 5,000 FRESH receivers per epoch (450k calls) — every receiver a new identity, every method the SAME concise-method fid: the exact rotating shape B189 solved for plain `Call`, at a `CallMethod` site. The lane composes from landed pieces: the emitted GET probe (shape ways serve the method slot load on rotating same-shape receivers) yields the callee bits → the `fid_mirror` guard + the cross3 body (enter/args/entry/close, `this` = the receiver instead of the arrow mirror). react (2.49×) shares the shape. AS LANDED: `emit_cross3_call` refactored into a guard prefix + a shared `emit_cross3_invoke` (this-source enum Arrow/Undefined/Receiver; the 64-byte scratch gains a callee-bits stash for the finish helper); the method lane guards receiver tag → `shape_mirror[obj] == baked shape` (any version bump — accessor flips and method redefinitions included — pins the mirror to DICT until the miss path re-settles, so the guard subsumes them) → the live own slot via `vals_ptr_mirror` → callee tag + `fid_mirror == baked fid` → the invoke with `this` = the receiver bits. Plan built from the LIVE exemplar (`build_cross_call_plan` gains `exemplar_base`; the callback-compile path passes None). GATING CAUGHT A REAL DEFECT, root-caused not papered: the lane referenced the arm-level `bail` label, which only the generic helper's `emit_region_bail` DEFINES — an mi-plan site (whose fallback uses a private label) left it dangling → dynasm `UnknownLabel` at finalize; a first-theory mutual-exclusion "fix" was REJECTED when it erased survival's win (its sites carry thrashing mi plans), and the real fix gives the lane its own bail stub. Latch `ZIPP_NO_CROSS3M`; the rotating-receiver + mid-run fid-swap case (node-oracled) joins `tests/cross3_lane.rs`; the lib mi test's fourth case pins the co-emission. Measured: survival 257→248ms one-binary A/B (−3.5%; the calls are ~10-20ms of a 250ms allocation-dominated row — the charter's weighting was optimistic and the honest share is recorded), react null (its sites do not admit), all-13 −0.29% [−1.05,+0.34] byte-identical; full test262 IDENTICAL; 8k soak clean. The remaining survival rocks stay the recorded epics: B187 stage 3 (the 79.6ns ObjMap construction floor × 1.8M allocs) and the sweep WALK. |
| **B192 LANDED: the module top-level tier demotion is GONE — the isolated nest runs 14ms as a module, byte-equal to the script** | **module completion tracking removed (spec-unobservable) + the INT tier admits dead-in-region completion writes for the eval shapes that keep them** | Two halves. (1) COMPILER: a module's completion value is spec-unobservable (Module Record `Evaluate()` resolves undefined; dynamic `import()` resolves the namespace), yet the eval-mode pipeline modules ride tracked it — planting `LoadUndefined` + a per-statement `Move` into every module top-level loop body. One line (`eval_completion = is_script && eval_mode && !module_mode`) removes the ops; eval scripts keep them (`eval()`'s RESULT is the completion value — pinned by test). (2) JIT: for the shapes that legitimately keep completion regs (hot `eval` bodies), the INT tier now admits `LoadUndefined` via `undef_dead_regs` — regs written only by `LoadUndefined`/`Move` and NEVER read in-region — kept UNTYPED/UNHOMED by the planner (a totality decline covers any other def-op; completion `Move` sources must be Num-typed), with every def emitted as a write-through frame-slot store (canonical UNDEFINED bits; `emit_int_box_rax` for the Move) in both INT emitters, plus the `hoistable_pins` whitelist and a `lazy_sx_sets` skip (whose missing skip was caught as a compile-time missing-home panic during gating, exactly the loud failure the exclusion design intends). `ZIPP_NO_UNDEF_ADMIT` latch; fuzz mode `noundefadmit`; `tests/module_completion.rs` pins module-equals-script on the hot nest and eval's observable completion values (node-oracled). Gates: suites, sandbox, `-D warnings` ×2, FULL test262 byte-identical (module/eval semantics), 8k soak with the new mode. Priced: the isolated nest 43→14ms (module == script); one-binary latch A/B on the flagged row NULL (−0.09% [−0.32,+0.76] at 21 pairs); the two-binary all-13's +0.69% decomposes into launch drift (direct boot medians: the B192 binary is FASTER, 6.4 vs 6.7ms) and fat-LTO layout on poly-objects (+2.7% two-binary vs null one-binary — the documented rebuild-noise class); npm-nanoid ≈ flat (its remaining cost is not completion-bound). `bench/w70_modcompletion_real13_ab_2026-08-26.json`, `bench/hostile/w70_modcompletion_modules_2026-08-26.json`. |
| **B192-scout: the MODULE top-level tier demotion — same loop 14ms as a script, 43ms as a module (node flat 42)** | **SCOUTED, precise repro + mechanism; the next wave's target** | Post-B190a the nanoid-class module adder persists and is now isolated: an identical checksum nest over module-scope bindings (which DO compile as r12 globals — StoreGlobal/StoreGlobalResolved, so the B143 local-vs-global story does NOT apply) runs 3× slower under `zipp mjs`. Chain: the script's OUTER loop compiles INT ([23,49], 7 homes, charCodeAt receiver pinned) and the whole nest runs native; the module's outer attempt [28,56] declines INT with "CallMethod (receiver not a pinned string/DataView)" and falls to boxed MEM, which then OWNS the nest. FIRST THEORY REFUTED IN THE SAME SCOUT (recorded for the method's sake): a static `zipp bc --module` op-dump suggested the region span swallowed the wrapper's trailing `Print`, but runtime module ips shift (+6; the TLA scaffolding makes the wrapper async-flavored), the Print sits OUTSIDE the span, and the decline text names the true failure: the OUTER candidate's pin builder does not pin the charCodeAt receiver `id` in the module fn, while (a) the INNER module region pins it fine (`pinned receiver r10/r17`) and (b) the script's outer [23,49] pins it and compiles INT. SECOND THEORY ALSO REFUTED, ROOT CAUSE NAILED: pin-builder summary prints show the module outer plan builds IDENTICALLY to the script's (pins=1, both accesses mapped) — the per-ip decline report names the true blocker: **`[int-reject] @31 LoadUndefined { dst: 1 }`**. The module wrapper's STATEMENT-COMPLETION bookkeeping plants `LoadUndefined` into the completion reg inside the loop body, and `LoadUndefined` is missing from the INT tier's admission set — the EXACT B183 class ("LoadUndefined was in NEITHER admission list, which kept map-set's region interpreted"), fixed then for the MEM path only. IMPLEMENTATION DESIGN (next wave): the completion reg is written (`LoadUndefined`, and a `Move` of the statement value) but NEVER READ in-region, so introduce a dead-in-region def class — collect `undef_regs` (LoadUndefined dsts, require no in-region reads via `instr_uses`), exclude them from `note_def` typing/homes (the `ta_recv_regs` precedent), and emit every def of them as a write-through frame-slot store (canonical UNDEFINED bits for LoadUndefined; box-the-home for a Move) in region_int + region_int_gpr (regalloc/DOUBLE later). VTy has no dead/untyped variant, so the exclusion set is the right mechanism, not a new type. Worth ~3× on module top-level nests (14ms script vs 43ms module, node-parity today only by accident). Repro scratchpad `checkloop.{js,mjs}`; the `[pin] fnN [s,e] built pins=…` summaries and the pin-decline prints added this session stay as diagnostics. |
| **B190a the QUICK `.length` prefix — parse-large-js −9.2%, npm-nanoid capture ratio 2.39→1.93** | **LANDED; all-13 geomean −0.96% [−1.60,−0.43], headline-10 −1.39%, no replicated regression** | The B190-scout attribution's dominant item, mechanized: Str/Cons/dense-Array lengths are DELIBERATELY uncachable in the property IC (correct — they derive from the container), so every `.length` read at a compiled GetProp site paid the FULL miss helper per read (~14ns: private-name/module-ns/shape-memo/key-scan preamble) — and `.length` lives in loop CONDITIONS, so nanoid-class checksum loops paid it per iteration (hoisting it by hand was −66ms on the micro). Fix: `jit_quick_len` — one receiver-kind match replicating the miss helper's own arms exactly (Str units, Cons len, dense Array with the virtual-length side table behind an `is_empty` short-circuit, `arguments` objects DEFER, everything else takes the sentinel to the unchanged miss path) — emitted after the IC probe's miss at region AND whole-fn GetProp sites whose baked name is `length` (`ZIPP_NO_QUICK_LEN` latch). Instance `length` is exotic OWN data, so the B191 shadow protocol does not apply. 15-pair one-binary A/B: parse-large-js **−9.2% [−9.8,−7.8]**, markdown −3.8% [−5.5,−1.2], regex −1.2% [−2.6,−0.6], json −1.0%; the sparse rows' +0.6% flag died at 21 pairs after the `is_empty` short-circuit (−0.1%/+0.0%); npm-nanoid (module harness) cold 2.39× → **1.93× [1.892,1.941]** (absolute 255→164ms unPGO). Suites, safe-sandbox `-D warnings`, full test262 byte-identical ×2 (incl. the final `is_empty` tree), 8k soak clean, every A/B byte-correct. `bench/w69_quicklen_real13_abenv_2026-08-26.json`, `bench/hostile/w69_quicklen_{types,apps,scope,objects}_abenv_2026-08-26.json`, `bench/hostile/w69_quicklen_nanoid_2026-08-26.json`. |
| **B191 FIXED: primitive-prototype method shadows are honored engine-wide — byte-for-byte node parity on the shadow probe across every tier** | **LANDED; the gates measure FREE (string-row two-binary A/B −0.38% null; boot cost unmeasurable at ±0.2ms)** | Found while designing the region charCodeAt arm: the shadow probe (`hot loop, then String.prototype.charCodeAt = () => 7, then re-run`) shows zipp still answering the intrinsic for charCodeAt / indexOf / substring / slice and `Array.prototype.push` — interpreter included, so this is the BUILTIN METHOD DISPATCH fast path resolving by receiver-kind+name without a prototype-pristinity consult, and the JIT's dedicated helper arms (proto_mem's `charCodeAt|indexOf|push|substring|slice` cluster) inherit it. In-place overwrites of existing proto slots bump no version (the B183 lesson), so version guards alone cannot fix it. THE PATTERN ALREADY IN-TREE THREE TIMES: `Map.get` (B183 coll memo: version+fn-bits pair) and `toUpperCase` (its helper checks live prototype/realm) both HONOR shadows, as does `Math.random` (deliberately routed through the live IC). Fix shape: per-primitive-prototype pristinity summaries (String/Array at minimum) maintained at the prototype objects' write chokepoints, consulted by the interpreter's builtin fast dispatch AND the JIT helpers; per-name B183-style memos where a summary is too coarse. FIX AS LANDED: boot-time baselines of %String.prototype%/%Array.prototype%'s own method slots (`capture_proto_baselines`, natives pinned below the GC floor so bits-equality is tamper-proof) + `string_method_is_intrinsic`/`array_method_is_intrinsic` proofs (B183-form `(version, slot, VALUE-bits)` memos for 16 string + 8 array hot names; full pos+bits proof otherwise), gating: the dispatch string arm, the Array arm, the interpreter's inline `arr.push` lane, and the JIT helpers `jit_char_code_at`/`jit_str_index_of`/`jit_str_substring` (mode-aware slice/substring)/`jit_array_push` (per call — the hoisted gate can't see a mid-region shadow)/`jit_span_code_unit_pred`, plus the STR_PIN snapshot (which licenses raw charCodeAt byte loads; region-pass granularity is exact because only user code can shadow and every user-code helper re-snapshots). `tests/proto_method_shadow.rs` pins the node-oracled probe in-process + NOJIT/THR1/GC-stress children; `markdown_inline_reduce`'s three expectation blocks that had PINNED the old divergence ("not yet observable… until that engine-wide protocol issue is repaired") now pin node parity, child realms included. Two-binary all-13: +0.88% cold geomean fully explained by a 1.3ms empty-launch drift between sides (direct 25-run boot medians: identical ±0.2ms; adjusted ratios null-or-better, property-ic adjusted 0.87). Full test262 byte-identical; 8k soak clean; -D warnings both configs. REMAINING KINDS for a follow-up sweep: Number.prototype methods, Date, RegExp names beyond test/exec, TypedArray methods, and the recorded Promise-then arm (§6 note). |
| **B190-scout: npm-nanoid's gap ATTRIBUTED — it is the checksum harness loop, NOT the append lane** | **MEASURED; the queue's "53ns/char append" claim is REFUTED (append ≈ 3ns/char, faster than node)** | Kernel-ablation micros against node: the append machinery costs zipp ~14ms of the row (~3ns/char — node pays ~34ms for the same appends); the id-GENERATION half including the seeded `Math.random` closure calls is near parity (67 vs 61ms); the dominant gap is the row's own CHECKSUM loop — `checksum = Math.imul(checksum ^ id.charCodeAt(j), P) >>> 0` over FRESH per-iteration strings — at ~103ms vs node ~21. Three compounding attributions, in evidence order: (1) the method-inline pass DECLINES `charCodeAt`/`length` on string receivers ("every arm declined" — its arms are object/class-shaped), so the outer MEM region pays a per-char interpreter-IC CallMethod helper; (2) the outer MEM region owns the whole nest, so the compiled inner INT-GPR region (whose pins re-derive fine per entry) does not run it; (3) a module-context adder (single-string checksum loop: 14ms script vs 42ms module — module-scope `let` locals vs the B143 top-level-var global-slot advantage). The bounded first lever: string-receiver arms (`charCodeAt`, `length`) for the method-inline lane over the existing `char_code_at` helper. |
| **B189b/c the EMITTED same-proto cross call — the call itself goes native, from region AND whole-fn callers** | **LANDED (c5ffb5b + follow-on); calls-closures −13.1% [−14.5,−12.0] on top of B189a; capture ratio 1.93→1.63** | The ~15ns/call cross-helper steady state, emitted: guards (callee tag + `fid_mirror` identity-free fid match + the three env nonempty bytes + depth + `cross_code_epoch` for the baked entry/mask + route epoch + GC-due + prior-activation shape) → `jit_window_open` (contiguity/capacity/set_len — the only Vec-touching steps left in Rust) → baked this (arrow via `this_mirror` / strict-plain UNDEFINED) + args + mask-zero stores → activation save/install/restore as three qwords over the `repr(C)` state (`upvals_mirror` supplies `upvals_raw`) → direct `call` to the baked entry → `jit_window_close`. A mid-body bail calls `jit_cross3_finish`, the exact completing tail of the helper (B184: never replay); CALL_THREW unwinds via the region bail. Admission (plan-build time): same-proto site, argc==params≤6, arrow or strict-plain, live cross entry with an inline ≤64-reg mask, no reducer plans, unmetered; every baked datum revalidated per call, any miss = the unchanged helper as a pure prefix. B189c extended the lane to WHOLE-FN callers by folding the activation install into `jit_cross3_enter` (root-stack duplication for a suspended frame-free prior — the nested native chain react-class code produces — reported via bit 1 and popped by `jit_cross3_unroot` after the inline restore; the emitted prior-shape guard died with it), with the rooted branch PROVEN executed by the counter-asserting nested test. `ZIPP_NO_CROSS3` latch; fuzz mode `nocross3`; `tests/cross3_lane.rs` pins mid-body-bail completion, throw unwind through the finish helper, and arrow-`this` via the mirror (node-oracled outputs). |
| **B189a Tier-C closures unblacklisted + inline captured reads — the calls-closures floor attacked at its root** | **LANDED; calls-closures −7% and the tiny-capturing-closure class compiles at all (call-floor micro 347→153ms, 2.27×)** | New cross-call DECLINE counters (`ZIPP_ICSTATS=1` prints per-reason counts) found the real calls-closures story: a 5-op closure body reading one captured variable was REJECTED by Tier-C's B50-era `>= 12 other ops` upval floor, so the whole body was blacklisted and every native call site declined `no-entry` (7,999,992 of 8M on the micro) into an interpreter Frame. The floor was priced when every `UpvalGet` cost a resolving helper round-trip; this wave makes the read ~free and drops the floor to 0 (`ZIPP_TIERC_UPVAL_MIN` restores it for A/B). Mechanism: (1) `TiercActivationState` gains `upvals_raw` — the closure's upvalue base resolved ONCE per native entry (the cross helper folds it plus the arrow's `this` into the callee match it already performs, net-zero heap touches); (2) a heap `cell_vals_mirror` parallel to `objs` (write-through at BOTH payload-write chokepoints — `cell_set` and the mapped-`arguments` aliasing path, which the audit caught bypassing it; settling refresh/free/pin cover occupancy) carries every cell's value incl. the TDZ sentinel; (3) the emitted `UpvalGet` is three loads + a TDZ compare (`ZIPP_NO_TIERC_UPVAL_INLINE=1` restores the helper). Also landed as B189b groundwork: `fid_mirror` (callee proto id per slot, immutable per occupant), `JitGuardedMap` wrappers giving `obj_realm`/`closure_eval_scope`/`realm_global_objs` an emitted-readable nonempty byte (no `DerefMut`, so an unrouted mutation is a compile error, not a stale flag), and the host-api offsets for the emitted same-proto call lane. Self-call-lane audit: `self_slot` exists only for hoisted top-level declarations (structurally capture-free), so the inherited activation can never alias another closure's upvalues. AND THE FLOOR WAS MASKING A WRONG-ANSWER CLASS: the moment tiny `() => f.arguments` bodies compiled, the full test262 sweep caught `Function.arguments` answering `undefined` instead of throwing — the three JIT property-walk helpers (`jit_get_prop_miss`/`jit_get_prop_leaf`/`jit_set_prop_miss`) treat "no `proto_of` entry" as "[[Prototype]] = %Object.prototype%", which is wrong for CTOR-map receivers (chain runs through %Function.prototype%, home of the restricted `caller`/`arguments` throwing accessors); hops were `!is_ctor`-guarded, receivers were not. Fixed fail-closed (ctor receivers decline to the interpreter's restricted protocol; own statics unaffected), pinned by `tests/ctor_restricted_props.rs`, latch-bisected to `ZIPP_TIERC_UPVAL_MIN` (the inline-read lane was innocent). calls-closures 87→81ms (helper-off A/B 90ms); node 41ms — the remaining gap is the cross-call helper's ~15ns/call, which is B189b's target. |
| **B188 four shape pairs per way — 32 shapes per direct site; megamorphic's access phase 47→16ms** | **LANDED; THE MECHANISM'S COMBINED VALUE NOW: mega −28.6%, stable −33.8%, router −29.5%, react −14.9%, survival −8.0%** | The megamorphic bench cycles SIXTEEN layouts through one site — exactly 2× the 8-way table, so half the shapes thrashed to the helper. Instead of widening the table, each 64-byte way now packs FOUR `(pattern, slot)` pairs at fixed byte offsets (`obj_bits/slot_nhops`, `vals_ptr/version`, `hops[0..2)/hops[1].0`, `hops[2..4)/hops[3].0` — `fill_shape_pair` and the emitted probe share the layout; `repr(C)` + the compile-checked stride pin it), quadrupling capacity to 32 shapes with zero table growth. The unrolled probe pre-loads each pair's slot with a flags-preserving `mov` before its `je`, so a hit costs the same tail as before and a full miss adds six flag-safe instructions per way. Direct sites hold ONLY shape pairs (every pattern carries the bit-32 marker; zero words are free slots; real receiver bits cannot collide), fills update-in-place → first-free → round-robin. Phase timer: shapes-megamorphic touch **47→16ms**; whole-mechanism 15-pair `--ab-env ZIPP_NO_SHAPE_WAYS=1`: **shapes-megamorphic −28.6% [−29.2,−28.1]** (B188's increment ≈ −20pp over B179-era), shapes-stable −33.8%, warm-router −29.5%, reactish −14.9%, survival −8.0%, ephemeral null. Suites, safe-sandbox `-D warnings`, 10k soak with `noshapeways`+`gcstress` modes, byte-identity all green. `bench/hostile/w64_pairways_{objects,apps,server,allocation}_abenv_2026-08-26.json`, `bench/w64_pairways_real13_abenv_2026-08-26.json`. |
| **B187 stages 1-2: `vals` behind the accessor totality; the literal value slab** | **LANDED; EVERY OBJECT ROW WINS — shapes-stable −5.3%, megamorphic −5.1%, survival −4.7%, react −3.7%, router −3.3%** | Stage 1 (separate commit): `ObjMap.vals` privatized behind a total accessor API, `vals_slice` deliberately the representation-proof read (any store must stay contiguous for the JIT's scale-8 loads). Stage 2: `ValStore { Vec, Slab }` — a FINALIZE-BORN literal borrows a fixed-capacity cell from the heap's stable-chunk slab (three capacity classes, base-pointer free lists, chunks never move; `Heap::alloc_finalized` fuses cell alloc + construction + the ordinary slot alloc so every mirror/gen/log bookkeeping stays authoritative; all three finalize paths — interpreter, general and baked JIT helpers — stage values in stack buffers and route through it). Growth SPILLS to the Vec form under the key-add version bump that already invalidates every cached pointer; the vacated cell is deliberately leaked within its heap's own chunks — TWO DEFECTS WERE CAUGHT AND FIXED IN GATING, both recorded for the method's sake: (1) a thread-local spill-parking design could hand one VM's cells to another VM sharing the thread (a use-after-free once the first heap drops; it also explained a boxref GC-stress flake — parking removed, leak-on-spill is the sound form); (2) an editing accident made `replace()` restore the OLD occupant (caught by re-reading before trusting a green suite). The hardened `safe-sandbox` build compiles the slab OUT entirely (it forbids `unsafe`; that build always takes the Vec form), and slab shells still ship to the B185 courier AFTER their cell goes home — dropping them inline measured survival +5.8%, exactly B185's win handed back. Phase timer: shapes-stable build 62→54ms. 15-pair `--ab-env ZIPP_NO_VAL_SLAB=1`: shapes-stable **−5.3% [−6.6,−4.2]**, shapes-megamorphic **−5.1% [−6.2,−4.8]**, allocation-survival **−4.7% [−9.2,−4.2]**, reactish **−3.7% [−4.3,−1.8]**, warm-router **−3.3% [−4.5,−3.1]**, ephemeral NULL. Suites, safe-sandbox `-D warnings`, 10k+8k soaks with gcstress, boxref stress ×3, byte-identity on/off vs node all green. `bench/hostile/w63_valslab_{objects,allocation,apps,server}_abenv_2026-08-26.json`, `bench/w63_valslab_real13_abenv_2026-08-26.json`. |
| **B187-scout: the build floor attributed — 86% is the `ObjMap` STRUCT CONSTRUCTION, not allocation, not the VM** | **MEASURED; THE ARENA REFRAMES AS A COMPACT LITERAL-OBJECT REPRESENTATION** | Post-B185 phase timers: shapes-stable build 62ms vs node 3ms (touch is down to 19ms — the shape-way waves won that half). A three-level micro (`tests/build_floor_micro.rs` + `bench_support`) attributes the 92ns/object: the FULL JS pipeline 92ns; pure Rust `finalized_from_plan` + `Box` with NO VM involved = **79.6ns** — the two mallocs are ~10ns of that (B177's ~1ns/free finding already said so); the rest is the 112-byte `ObjMap` + 32-byte Vec header initialization and moves for fields a literal never uses (`index`/`numeric_index`/class/flags machinery). Consequence: an allocation arena alone cannot reach node's ~4ns; the design that can is a COMPACT literal-object representation — plan-shared keys, inline values, shape, ~48 bytes touched — promoted to a full `ObjMap` on first structural mutation, behind the accessor APIs the B177 migration installed for exactly this. Staged plan in the B187 design doc. |
| **B185 the GC FREE COURIER — the campaign's first helper thread; survival −11.1%** | **LANDED; SWEEP −63% ON THE WORST ROW, THE SECURE FREE-TAX MOVES OFF THE MUTATOR** | One process-global background thread (lazily spawned, `zipp-gc-courier`) performs the DROP of dead heap payloads the sweep collects — the V8 division of labor (mutator marks, helper frees), scoped to the payload drop only: every index/gen/remset/mirror bookkeeping step stays on the mutator. Only variants proven `Send` by construction ship (Object/Str/Array/Map/Set — plain owned data; the plan `Arc` decrements atomically by design); everything exotic drops inline exactly as before, and a send failure falls back to an inline drop, so a free can never be lost or unboundedly delayed. Batches live exactly one sweep (flushed at `note_gc_done`/`note_minor_done`). Instrumented: allocation-survival's sweep phase **117.1→43.2ms (−63%)**, GC total 148→76ms — the hardened allocator's free-side guard work now runs on the second core, which is precisely the multi-threading the maintainer asked for. 15-pair `--ab-env ZIPP_NO_GC_COURIER=1`: **allocation-survival −11.1% [−12.8,−8.8], shapes-megamorphic −6.2% [−7.9,−4.4]**, shapes-stable/router/ephemeral NULL. Gates: suites, safe-sandbox `-D warnings`, 8k soak with `gcstress`+`nonursery` modes, GC-stress parity courier-on/off (identical output, identical 20s), `ZIPP_NURSERY_VERIFY`, byte-identity vs node. GCSTATS' sweep number now excludes the shipped drop cost (it measures mutator time, which is what the mutator pays); peak RSS can carry one sweep's dead mass in flight. `bench/hostile/w62_courier_{allocation,objects,server}_abenv_2026-08-26.json`, `bench/w62_courier_real13_abenv_2026-08-26.json`. |
| **B184 the mid-body deopt edges COMPLETE; the closure lane returns default-on, now worth −13.7% on reactish** | **LANDED; B181's ROOT CAUSE FIXED RATHER THAN PARKED — test262 full sweep at the exact baseline 9** | B181's hazard was never the closure lane itself but two mid-body `SELF_CALL_DEOPT` edges a cross-called frame can only survive by replaying the whole call: (1) `jit_method_builtin_fallback`'s final decline — reached by e.g. a user function stored as a property of a FUNCTION receiver (`assert.notSameValue`) — now COMPLETES via the interpreter-equivalent tail: the ordinary property Get (getters run exactly as interpreted, with the same `(in <fn>)` message wrap), the ctor-object `this = undefined` route, then `call_value` with `this = recv` (natives, bound, proxies, generators, user functions uniformly; the interpreter's TypeError for non-callables via `resolve_callable_named`); (2) the arrow `lexical_this` resolution deopt now completes through `call_value`'s `setup_call` rebinding. With no replay-hazard edges left, `tierc_closure_make_enabled` is DEFAULT-ON again. Gates: the B181 forEach repro passes with the lane on; targeted test262 (arrow-function 643, forEach 376, Function.prototype 602) 100%; FULL sweep 95,933/95,942 — byte-identical failure list to the pre-B184 baseline (six historical + three hardening-era); suites + 10k soak green; react byte-identical vs node. Value: 15-pair `--ab-env`: **reactish −13.7% [−15.1,−13.4]** (was −2.9% at B174 — the shape-way waves multiplied what a compiled body is worth), calls-closures null (its cost is the per-op floor, unchanged); real-13 0.9980x [−1.22,+0.40] exact. Residual named hazard for the ledger: the entry `jit_call_depth` cap still returns a caller-visible deopt — deep-recursion sites are framed in practice, but the class deserves the same completion treatment when the per-op-floor wave touches these paths. `bench/hostile/w61_lane_{apps,scope}_abenv_2026-08-26.json`, `bench/w61_lane_real13_abenv_2026-08-26.json`. |
| **B183 the memoized collection-intrinsic proof + the region mutate arms — map-set recovered to code-parity** | **LANDED; map-set 740→671ms (−69), real-13 −0.95% [−1.68,−0.48] NET WIN; secure-off HEAD now beats node on the row** | Two regressions were pinned by an interleaved 7-binary race: the b950013 mega-commit (+80ms, its per-call `collection_method_is_intrinsic` full prototype re-proof — correct hardening replacing the old kind-only check — plus its ungated region rewrite) and the security commit (allocator ≈70ms + code). Recovery: (1) the proof's prototype half is memoized per (kind, method) under the VERSION + VALUE-BITS guard pair — the version bump covers add/remove/redefine/freeze/setPrototypeOf, the fn-bits equality catches the in-place slot overwrite that bumps nothing (their own string-method doc names exactly that hazard for version-only caches); receiver-side hashmap probes gain `is_empty` early-outs. (2) `jit_coll_mutate` widened to Set.add / Map.delete / Set.delete, and the REGION CallMethod emitter gains the mutate arm the Tier-C emitter already had (with a Set retry for `delete`, mirroring `has`) — the row's set/add/delete ops stop frame-calling generically. Tamper tests pin all three guard directions (in-place overwrite, accessor redefinition, own shadow). Final race: HEAD 671 / HEAD-secure-off **594 vs the fast-era 576 (+3%, race-noise floor)** / node 631 — the CODE share of both regressions is recovered; the remaining gap is the allocator trade, and the PGO capture projects the shipping binary under node. Suites + 4 safe-sandbox targets + 8k soak + absolute node check green; byte-identity vs node. `bench/w60_collmemo_real13_abenv_2026-08-26.json`. |
| **B182 baked `FinalizeObject` (emit-time plan + shape fold)** | **LANDED; ROUTER −3.5%, SHAPES-STABLE −2.9%, EVERYTHING ELSE NULL** | A `FinalizeObject` site whose plan resolves at compile time now calls a slim helper with the plan ADDRESS (stable: root-program plan tables never degrade — B173's own licensing argument) and the emit-time `shape::add` fold (identical by construction to `finalize_shape`'s memo: same fold, same thread-local transition tree) baked as immediates, plus `reg_count` as the 5th stack argument. The general helper's per-call plan double-index + validity checks + shape-memo hash probe drop out; the checks a compile cannot freeze (window bounds, GC poll, `catch_unwind` abort discipline) stay. 15-pair `--ab-env ZIPP_NO_FINALIZE_BAKED=1`: **warm-router −3.5% [−4.1,−2.5], shapes-stable −2.9% [−4.7,−0.1]**, shapes-megamorphic/react NULL; real-13 1.0001x [−0.70,+0.48] exact, no row outside CI; suites + safe-sandbox limit tests + 8k soak green; byte-identity vs node both switch states; absolute node check (markdown 401ms) per the B180 rule. `bench/hostile/w59_baked_{objects,apps,server}_abenv_2026-08-26.json`, `bench/w59_baked_real13_abenv_2026-08-26.json`. |
| **B182-scout: the five behind-Node retained rows decomposed — json/regex are PURELY the secure-allocator trade** | **MEASURED; THE POLICY LEVER IS THE MAINTAINER'S, THE TRAFFIC LEVER IS THE CAMPAIGN'S** | Same-day best-of-3 against a clean pre-hardening parent build: json-large parent 292ms vs HEAD 391ms, regex-log-scan 510 vs 599 — but HEAD rebuilt with mimalloc `secure` OFF measures **291ms and 523ms, exactly parent parity** (and that control is un-PGO'd; PGO'd it would lead). The hardened CODE on those rows is fully recovered post-B180; the remaining 1.24×/1.18× vs node is the allocator feature. Documented so the maintainer can price the trade; the campaign's compatible lever is allocation-traffic reduction in the JSON/regex paths. The other three: parse-large-js is a JS-implemented parser benchmark (95% jit-mem/jit-fast — the boxed-tier/object-build campaign IS its fix; already 1.58×→1.22× via B178/B179), map-set-heavy and polymorphic-objects measure parent-equal (not hardening regressions) and need their own small waves. |
| **B181 the closure lane's cross-entry effect-then-deopt hole — test262 caught a double-applied effect** | **LANE DEFAULTED OFF PENDING THE EXCLUSION FIX; test262 ARROW ROWS GREEN AGAIN** | The full-sweep revalidation for the README republish (the first test262 run since the security merge) failed `cannot-override-this-with-thisArg` in both modes: a one-element `forEach`'s arrow callback ran TWICE (`calls == 2`). Minimized: the arrow's `calls++` is a `CellSet` (admitted by B174's closure lane), the following `assert.notSameValue(...)` is a general-route `CallMethod` on a FUNCTION receiver whose miss returns `SELF_CALL_DEOPT` — and a CROSS-CALLED body has no frame to resume mid-function, so the caller replays the WHOLE call, double-applying the committed effect. `ZIPP_NOJIT` passes, `ZIPP_NO_TIERC_CLOSURE_MAKE=1` passes, the pristine security-commit binary passes (643/643 on the subtree), and neither wave-56/57/58 latch is implicated — the defect shipped with B174 and sat latent until a body with exactly effect-then-general-method compiled. W54's own admission note claimed "every decline happens before the first observable effect"; the general `CallMethod` arm broke that ordering contract. DISPOSITION: `tierc_closure_make_enabled` is now DEFAULT-OFF (opt-in `ZIPP_TIERC_CLOSURE_MAKE=1`; the lane's equivalence tests opt in explicitly), costing B174's react −2.9% until the real fix — a plan-time cross-entry exclusion for bodies where a deopt-capable op follows an effectful op — lands with its own full gate. ALSO recorded from the same sweep: three module failures (`source-phase-import` ×2, `ambiguous-export-bindings` ×1, all sloppy-mode) REPRODUCE ON THE PRISTINE SECURITY-COMMIT BINARY — they arrived with the hardening era, not the perf waves, and are queued for their own investigation rather than silently added to the expected-failures baseline. |
| **B180 the un-instrumented heap-walk preflight — markdown-render 258s→0.42s, regex-log-scan 139s→0.69s** | **FIXED; A ~600x RETAINED-ROW REGRESSION THAT EVERY SWITCH-BASED GATE WAS BLIND TO** | The security hardening made the CLI build zipp-vm with `features=["instrument"]` (the sandbox command's meter) and put `instrument_preflight_heap_growth` on the per-part/per-result string paths (`append_guest_string`: join parts, builders, regex result strings). The preflight computed `heap_bytes()` — a FULL O(heap-slots) audit walk — BEFORE its `instr_rec` early-return, so every plain `zipp js` run paid an O(heap) walk per append for a figure that was then discarded: markdown-render went 0.3s→258s+ and regex-log-scan 0.5s→139s, WORSENING per round because the slot table only grows (~6µs/append × 200k parts on the join micro, +9s/round on the bench). Found only because a 15-pair real-13 gate "taking a while" prompted a direct node-vs-zipp wall check: **every `--ab-env` A/B had measured the broken rows as a clean 1.00x, because both sides carried the tax** — same-binary switch gates CANNOT see a regression that rides in on the baseline; only absolute captures vs node can (real-13 absolutes had not been re-timed since the security merge). Bisected across the commit (parent 0.3s, security commit 258s+, secure-allocator ruled out by a same-tree secure-off build), then within it to `array_ops.rs`, then to `append_guest_join_part`, then to the preflight's walk by transplanting the security join arm verbatim onto the parent (fast) and isolating the appender (13x on a 200k-part join micro). FIX: recorder-first, and the walk runs only under a FINITE heap limit — every hardening semantic preserved (all 13 safe-sandbox limit tests pass; the live `--max-heap-mb 16` ceiling still trips). Sandboxed runs with a finite limit still pay the per-append walk BY THE HARDENING'S OWN DESIGN ("the periodic full-heap scan remains the backstop") — an incremental-resident refinement is possible if sandbox string throughput ever matters. ALSO fixed here: wave 58's helper insertion had split the `#[cfg(jit)]` attribute off `jit_set_prop_miss`, breaking the `--no-default-features --features safe-sandbox` build — caught by running that configuration's tests. LESSON FOR THE METHOD RULES: a wave gate must include ONE absolute cold row-check vs node (not only paired switch ratios), and the safe-sandbox build in the compile gate. |
| **B179 shape-way STORES with the inline-filtered value-grain barrier** | **LANDED; shapes-stable 0.67x AND warm-router 0.70x OF THEIR PRE-WAVE SELVES (combined switch)** | The direct-miss SET form gets the same native shape ways as B178's GET: guard the receiver's live shape mirror, commit `vals[slot] = val` call-free, then run the nursery value-grain barrier ONLY when the stored value is a heap Value — one tag compare filters the numeric majority; the rare heap-value store calls the slim infallible `jit_shape_set_barrier` (= `write_barrier_val`), replacing the identity-way approach of registering every holder as a persistent minor-scan root, which a shape way cannot do (its receivers are unknown at fill time). Writability needs no per-hit re-check: the fill proved `!accessor && writable` AT THIS SHAPE and equal shape means equal attribute bits (freeze/seal/redefine drop to DICT + bump ⇒ mirror invalidated). Barrier-after-commit is sound single-threaded: the helper cannot trigger a VM collection (it only pushes the Rust-side remset). The gcoracle store accounting is skipped on native hits exactly as identity-way IC-HIT stores skip it. Combined-switch 15-pair `--ab-env ZIPP_NO_SHAPE_WAYS=1` (GET+SET vs neither): **shapes-stable −30.7% [−32.3,−30.1] (136→94ms), warm-router −28.7% [−29.1,−28.3] (173→123ms), reactish −12.0% [−13.0,−11.3], shapes-megamorphic −8.3% [−9.0,−8.0], allocation-survival −6.5% [−6.8,−4.7]**; the SET increment over B178's GET-only artifacts ≈ −7.5pp on shapes-stable and −11pp on the write-heavy router. Gates: suites green, byte-identity vs node both switch states on all cluster rows, ZIPP_GC_STRESS + ZIPP_NURSERY_VERIFY passes on the barrier-critical rows, 12k tier-differential soak including `gcstress`+`nonursery` modes 0 divergent. Estimated standings vs node (cold): shapes-stable ~2.2x (from 3.73 at w56), warm-router ~2.4x (from 3.55). `bench/hostile/w58_shapeset_{objects,server,apps,allocation}_abenv_2026-08-26.json`, `bench/w58_shapeset_real13_abenv_2026-08-26.json`. |
| **B178 native shape ways at direct-miss property sites** | **LANDED; THE LARGEST OBJECT-ROW WIN SINCE B173 — EVERY CLUSTER ROW MOVES, CIs EXCLUDE ZERO** | THIS IS THE B152 RETRY DONE THE WAY B152'S REJECTION PRESCRIBED ("memory-safe ownership model and explicit exotic-kind separation"), and it closes both of that review's design-level blockers BY CONSTRUCTION rather than by narrowing: (1) exotic-kind collision is impossible because the guard reads a SLOT-TYPED live mirror, not object-carried metadata — every non-`HeapObj::Object` heap slot holds the never-matching DICT sentinel, side-table maps are never receivers, and the exotic-but-Object receivers `ic_obj_ok` excludes are pinned unmatchable at registration; (2) stale-storage observation is governed by one stated discipline (refresh at alloc/replace/repair, INVALIDATE at every version bump) whose fail-safe direction is a miss, and the mirrors are plain heap-owned `Vec`s read through the VM pointer per access — no raw metadata view, no baked storage pointers. The direct-miss GET form (a site that proved identity-thrashing while the interpreter-side shape memo hit — `touch()` compiles with `direct_miss=5`) now emits up to 8 NATIVE SHAPE WAYS before its helper call: guard the receiver's live shape against the way's baked shape, read `vals[slot]` through a live pointer, no call. Soundness is a pair of heap MIRROR arrays (`shape_mirror`/`vals_ptr_mirror`, parallel to `objs` like `versions`) with a version-bump-piggybacked discipline stated ONCE in heap.rs: alloc/replace REFRESH, `bump_version` INVALIDATES to DICT (order-independent — a refresh would capture the wrong side at a bump-first caller), the miss helper REPAIRS after resolving own data, `free_slot` clears, and the `ic_obj_ok`-excluded receivers (global object, %Array.prototype%, realm globals, module namespaces) are PINNED unmatchable at registration — the adversarial review's one confirmed major was exactly a realm global populated after alloc with no bump, turning the fill-side exclusion from an accident of shape non-collision into an invariant. Fills come from the memo's Direct arm, self-limiting (one helper trip per (site, shape) for finalize-born objects whose mirrors are birth-correct). Same-binary 15-pair `--ab-env ZIPP_NO_SHAPE_WAYS=1`, post-review-fix binary: **shapes-stable −23.2% [−23.9,−20.4] (138→105ms), warm-router −17.4% [−17.6,−15.8] (170→141ms), reactish-reconcile −13.7% [−14.3,−10.8] (147→128ms), shapes-megamorphic −6.6% [−7.4,−2.5], allocation-survival −5.4% [−8.9,−3.7], types-churn −4.4% [−6.5,−3.4]**; ephemeral/types-stable NULL as expected. Gates: full suites green, byte-identity vs node on all cluster rows both switch states, 8k-program tier-differential soak over `noshapeways`+`noattrselide` modes 0 divergent (both added to the fuzzer's mode table), 4-lens adversarial review (mirror invariant, emitted stream, aliasing/growth, fill policy) — 1 major confirmed and fixed (realm-global pin), 5 minors recorded (register-tier fills feed ways its probe doesn't read — wasted fill only; the native probe trusts the mirror discipline instead of consulting `ic_obj_ok`, which the pins make sound). SET stays on its existing form (write-barrier design deferred); proto-chain and accessor resolutions stay identity/version-guarded (B111). `bench/hostile/w57_shapeways_{objects,server,apps,allocation,types}_abenv_2026-08-26.json`, `bench/w57_shapeways_real13_abenv_2026-08-26.json`. |
| **B177 attribute-column elision (`PropAttrs::AllData`); the allocation-op cost model refuted** | **LANDED AS MEMORY WIN + ENABLER; PERF NULL; PRICED THE REAL COST AT ~1NS/FREE** | The per-property `Vec<PropAttr>` (16B/property) is now elided for the all-default-data case — the overwhelmingly common object shape. `attrs` went private behind a total accessor API (233 call sites converted; the `setter_ref` IC pointer contract preserved: an accessor forces materialization BEFORE any pointer can be cached), representation `AllData{len} | Mixed(Vec)` with the tag in the Vec pointer's null niche (ObjMap stays exactly 112B, pinned test unchanged). A unit probe proves user literals stay elided on a live heap (the ~157 deviating maps are boot prototypes with non-enumerable methods — legitimately explicit). Same-binary 15-pair `--ab-env ZIPP_NO_ATTRS_ELIDE=1`: objects **+0.71% [-0.45,+2.73] NULL**, allocation **+0.06% [-3.24,+1.77] NULL**; peak-RSS harness PASS all cases. THE INSTRUMENTED REFUTATION THAT MATTERS: eliding 665k frees moved shapes-stable's sweep only 15.4→14.7ms — **mimalloc-secure small-object free ≈ 1ns**, so the "3-allocations-per-object floor" (B175) and any pool/alloc-count mechanism CANNOT buy meaningful time on these rows; B176's secure-mode tax must live at page/segment grain, not per-op. The row's real cost is NATIVE ACCESS: 2.77M property accesses take 242 IC misses (interp shape memo absorbs identity thrash) but each access at a direct-miss site pays a helper call — see B178. Kept for: -64B/object resident memory, GC setter-walk skip (`may_deviate_attrs`), and the accessor-API migration the shared-descriptor plan requires. `bench/hostile/w56_attrs_{shapes,alloc}_abenv_2026-08-25.json`, `bench/hostile/w56_triage_5675b79_2026-08-25.json` (the post-security 17-row baseline: 10 rows below parity, worst survival 5.42x/shapes-stable 3.73x/shapes-mega 3.68x/router 3.55x/react 3.23x). |
| **B176 security-merge attribution; ObjMap-pool revisit refuted on the new baseline** | **MERGE LANDED CLEAN; POOL REVERTED (NULL); ALLOCATOR TAX QUANTIFIED FOR THE MAINTAINER** | The security-hardening commit (`4195e06`, +10.7k lines) rebased under the perf waves with one conflict (inside the static-record function B172 already deletes — deletion kept). All suites green post-merge; hostile outputs byte-identical vs node. Its perf cost was then attributed by three interleaved same-machine builds: the dominant term is `mimalloc`'s new **`secure` feature on the CLI** (guarded metadata/integrity checks on every malloc-free), not VM code — quiet-machine best-of-3 interleaved across the three builds: **shapes-megamorphic 199→254ms (+28%; secure-off control 207ms), allocation-survival 262→411ms (+57%; control 285ms), allocation-ephemeral NULL (84-87ms everywhere — slot-recycled rows dodge the allocator)** — the control puts ~85% of the tax on the allocator feature, the residual single-digit % on the rest of the hardening; GC sweep alone 19→72ms. This is a deliberate defense-in-depth trade and is left standing — recorded so the ~1.13 hostile geomean regressing toward ~1.4 is attributed to policy, not the perf work. The B84-pool revisit (HANDOFF map 1a) was built and gated on this baseline: thread-local retire/take with capacity caps, exhaustive-field reset, full-check-before-reset ordering — measured **~NULL on shapes and allocation** (`w55_objpool_{shapes,alloc}_abenv`) because POOL_MAX=2048 covers <1% of a 200k-object sweep and the nursery already slot-recycles the ephemeral majority, so it was fully reverted per the no-dead-machinery rule. Kept from the wave: the payload-accounting gate (`Heap::payload_accounting`, off until the first `audit_resident_bytes` consumer, which backfills exactly — removes a per-allocation sizing walk from trusted runs) and the `regs_would_overflow` unused-param CI fix. |
| **B175 post-W54 decomposition of the remaining hostile gaps** | **SCOUTED AND INSTRUMENTED; THREE THEORIES REFUTED, MAP RECORDED** | Cross-call overhead REFUTED for the object rows: 1.38M window fills cost single-digit ms (a source-inlined shapes variant moved ~146->139ms; Node ran the inlined form SLOWER). Property-IC pressure REFUTED as the lead cost: probes are shape-known and hit (242 total GetProp misses across 2.77M accesses); the hit path is ~10-15ns/access. The real split, by in-source phase timers on shapes-stable: **build 61ms vs Node 5ms** (the ~3-allocations-per-object floor in `finalized_from_plan` - `Box<ObjMap>` + vals Vec + a 64-byte attrs Vec for a 4-field object) and **touch 43ms vs 3ms** (hit-path probe cost x4 accesses). NanoID's replaced `Math.random` is already a typed mi LANE (`ops=16 guards=4`); its residual is the per-character append machinery (4.5M `id += alphabet[...]` steps) - the W25 append reducers' top-level-`var` scope sensitivity (B140/B142) excludes module function locals. Priorities recorded in HANDOFF: attrs-Vec elision, the module-local append lane, survival's 16% GC share, the helper-per-op Tier-C floor. |
| **B174 Tier-C application-op lanes** | **LANDED AS AN ENABLER; REACT -2.9%, EVERYTHING ELSE NULL** | Two independent lanes remove the whole-function blacklists on application-shaped code. `ZIPP_NO_TIERC_CLOSURE_MAKE`: MakeCell/MakeCellTdz/MakeCellFnName/MarkCellConst, MakeClosure/MakeArrow (full lexical inheritance: capture cells, `this`, [[HomeObject]], new.target, EvalScope, realm), CellGet/CellSet/CellSetChecked, SetIndex, ArrayCtor (dense subset), CallMethodComputed (dense), and a GENERAL CallMethod live-IC route for every non-intrinsic name. `ZIPP_NO_TIERC_ITER`: the sync for-of machinery (pristine-array GetIterator identity, IterPrime, the region `jit_iter_next` step, the finally bracket via the region push/pop helpers with normal-completion EndFinally/IterCloseFinally fall-throughs), ObjectKeys (ordinary snapshot), and HasProp brand=false via the region `jit_has_property`. Functions containing handler ops are excluded from native cross entries, so the bracket only ever runs frame-backed. The react App/diff/Item/makeStore bodies now compile (interp share 51% -> 23%); same-binary 15-pair gates: reactish **-2.91% [-3.80,-2.42]**, scope/shapes/allocation/real-13 all NULL (real-13 **1.0003x [-0.51,+0.71]**, no row outside CI). Kept on the `ZIPP_NO_MULTI_SPLIT` enabler precedent: the interpretation ceiling is gone and per-op Tier-C costs (IC arms, direct-miss evictions) are now the measurable next target. Artifacts: `bench/hostile/w54_tierc_app_{react,scope,shapes,alloc}_abenv_2026-08-25.json`, `bench/w54_tierc_app_real13_abenv_2026-08-25.json`. |
| **B173 one-step `FinalizeObject` literal lowering** | **LANDED; DOUBLE-DIGIT WINS ON EVERY OBJECT-HEAVY HOSTILE ROW, GUARDS NULL** | The compiler stages every all-static literal value into a contiguous register block (the `NewArray` discipline) and allocates+populates with ONE bytecode op / ONE Tier-C helper, replacing `NewPlannedObject` + per-field `AppendDataProp`. Same-binary 15-pair `--ab-env` vs `ZIPP_NO_OBJECT_FINALIZE=1`: shapes-stable **-28.5% [-29.7,-27.6]**, shapes-megamorphic **-31.0% [-31.5,-30.3]**, warm-router **-20.7% [-21.3,-20.2]**, reactish-reconcile **-20.2% [-21.7,-18.9]**, allocation-survival **-16.9% [-18.1,-16.4]**, types-churn **-11.5% [-11.8,-9.2]**; allocation-ephemeral is NULL (+0.6% [-3.2,+4.2]) because the local-SROA object lane and the virtual concat-length sub-lane were both ported to the finalized form (`finalized=1 concat_lens=1 tier=DOUBLE` re-verified); the frozen real-13 measured **0.9932x [-1.49%,-0.06%]** with no row regressing outside its CI. The op's shape memo folds `shape::add` once per (fid, plan); metering charges 1+count in BOTH tiers (interpreter `charge_steps` remainder, weighted native meter blocks, pinned by a meter unit test). Restricted to ROOT programs (`compile_main_program`/`compile_main_module`): a dynamically-installed program's over-cap plan degradation clears the plan pool, which a finalize site cannot survive, so eval/module installs keep the legacy lowering by construction. Artifacts: `bench/hostile/w53_finalize_{shapes,alloc,router,react,scope,types,errors}_abenv_2026-08-25.json`, `bench/w53_finalize_real13_abenv_2026-08-25.json`. |
| **B172 static-record factory speed gate** | **REJECTED AND FULLY REMOVED - THE PREFIX WAS PURE OVERHEAD** | B171's pending promotion gate ran as specified: 31 same-binary pairs on both shape rows measured the mechanism-ON default **+2.1% [+1.0,+2.8] SLOWER** on shapes-stable and **+1.9% [+0.6,+4.2] SLOWER** on shapes-megamorphic (per-row CIs exclude zero). Both rows fail the >=5% improvement floor, so the mechanism, its helper, plan tables, emission blocks, switch and tests were removed completely (-1,732 lines); a zero-symbol audit and the full suite (1,553 passed) confirm nothing remains. `bench/hostile/w53_static_record_shapes_abenv_clean_2026-08-25.json`. |
| **B171 bounded static-record factory prefix** | **SUPERSEDED BY B172 - GATE FAILED, MECHANISM REMOVED** | Recognizes immutable bytecode only for the hostile two-Int scalar factories: one 4-field arm or the exact 16-way 5-field `arg1 & 15` dispatch, with argument/constant/checked-add/xor recipes. Pointer-free plans are capped at 128 and all code/register/field dimensions are bounded. Runtime revalidates exact live Func/fid, root realm, EvalScope absence, canonical register window, Int tags, plan, MAX_FRAMES, native depth and pinned register capacity before and after GC; metered and GC-stress VMs decline, and the post-allocation boundary is fail-stop. Independent review found and fixed stale-IC recursive realm traversal, missing capacity parity, malformed register bounds and raw incoming-pointer reads. Focused gates: 7/7 unit, 4/4 all-feature integration, warning-clean host/Windows ARM64 checks. **No speed claim yet:** run the documented 31-pair ≥8%/row gate before promotion, otherwise revert. `ZIPP_NO_STATIC_RECORD_FACTORY`. |
| **B170 native meter terminators and remaining loan debt** | **RETURN OVERCHARGE FIXED; MULTI-CHUNK EXACTNESS STILL OPEN** | Meter blocks now split after both `Return` and `ReturnUndefined`, removing charges for unreachable synthetic tails; a real native/interpreter oracle pins exact equality below `NATIVE_CHUNK`. Audit found no undercharge or sandbox bypass, but finite JIT budgets above 1,048,576 can lose an unused native-loan tail, and native→interpreter nesting can strand the parent loan and stop early. This is fail-closed availability/accounting debt. CLI sandbox continues to force no-JIT. The required follow-up is an exact Active/Exit-tail state machine plus centralized interpreter handoff and exact self-call resume IP; do not advertise exact finite JIT metering before it lands. |
| **B169 typed closure trace** | **REJECTED AND FULLY REMOVED — CALLS-CLOSURES ~40% SLOWER** | Thirty-one same-binary pairs measured the proposed exact typed trace at about **+40% runtime** on `calls-closures`, with baseline calls neutral. The full trace, plan metadata, ABI plumbing, counters, switch and trace-only tests were removed. W52 post-removal is neutral against W50; zero-symbol audit passed. `bench/hostile/w51_closure_trace_calls_abenv_dirty_2026-08-25.json`, `w52_trace_rejected_vs_w50_calls_dirty_2026-08-25.json`. |
| **B168 Wave 50 hostile checkpoint** | **LATEST COMPLETE DEVELOPMENT CAPTURE; PARITY OPEN** | Exact 17-row, 15-repetition dirty-tree diagnostic: cold **1.2786× ordinary geomean** and **1.3509× category-balanced**. Largest rows are router 3.839×, survival 3.740×, stable/mega shapes 3.699×/3.640×, React 3.135×, NanoID 2.570× and closures 1.991×. `bench/hostile/w50_planned_probe_full_dirty_2026-08-25.json`. |
| **B167 compiler-planned static object keys** | **LANDED; DOUBLE-DIGIT FOCUSED WINS WITH BOUNDED METADATA** | Unique all-static object literals use compiler-owned key plans to avoid per-object key cloning and repeated growth. Plans have source/count/byte/site/retained-memory caps and fail closed on malformed metadata; runtime preserves descriptors, key order, shape fallback, GC edges and kill-switch/no-JIT behavior. Same-binary focused A/Bs measured about **16.8% faster** on shape/survival rows, **11.7%** on React-ish, and **10%** on router, with no peak-RSS regression in the dedicated harness. `ZIPP_NO_STATIC_KEY_PLANS`. |
| **B166 broad cross-call and activation-root hardening** | **LANDED; WIDE WINDOWS AND DIFFERENT-FID SITES STAY DYNAMICALLY GUARDED** | Selective cross-call initialization now supports >64-register callees with owned bounded masks; saturated sites with different ordinary FuncProto ids may try the generic live-resolution cross-call rather than permanent deopt. Frame-free Tier-C activations expose bounded closure/callee roots across GC safe points. Exact callee/entry/realm/EvalScope/depth/meter/window checks remain authoritative, and Windows ARM64 all-feature fixtures are warning-clean. Focused artifacts are retained under `bench/hostile/w40_*` and `w42_*`. |
| **B164 same-prototype lexical-arrow cross-call descriptor** | **LANDED; INTEGER-ONLY LIVE-GUARDED SPECIALIZATION, CALL ROW 5.1% FASTER** | A rotating `Call(argc=2)` site whose filled ways share one lexical-arrow FuncProto may select a const-specialized sibling of the existing native cross-call helper. The plan embeds only `fid`, caller window and callee window integers: every invocation re-resolves the live heap closure and live Tier-C entry, preserving fresh identities, mutable captured cells and lexical `this`. Different fid/non-closure, invalid window, child realm, live EvalScope, route invalidation, depth/entry/meter mismatch, throw and bailout use the unchanged generic/fallback semantics; the effectful core is shared rather than duplicated. `ZIPP_NO_SAME_PROTO_CROSS2=1`. Focused fresh-process coverage is 12/12 across switch, generic cross-call, GC stress, NOJIT, mutable captures, lexical `this`, throw/re-entry and post-hot different-fid replacement; JIT logs pin the specialized site. Thirty counterbalanced same-release-binary pairs on `calls-closures` measured **92.438 ms enabled vs 97.340 ms disabled**; paired mean saving **4.836 ms**, deterministic 10,000-sample bootstrap 95% CI **3.548..6.439 ms**, 27/30 pairs favourable. This focused result postdates B161 and is not folded into its full-corpus ratios. |
| **B163 direct dense `[[HomeObject]]` side table** | **LANDED; EXACT EDGE SEMANTICS, SURVIVOR ROW 14.1% FASTER** | `closure_home` defaults to a direct heap-slot-indexed `Vec<Value>` plus an authoritative `u64` presence bitmap; `undefined` remains a valid stored edge and no sentinel is used. This is storage replacement, not home elision: all required strong `[[HomeObject]]` edges, write barriers, reachable-holder tracing, and major/minor pruning remain. Removal clears both bit and value, slot-reuse tests forbid stale inheritance, insertion asserts the key is below the real heap slot count, and retained capacity follows the heap high-water at about **8.125 bytes/slot**. `ZIPP_NO_DENSE_CLOSURE_HOME=1` retains the HashMap differential oracle. After the rejected B162 cache was removed, 15 paired runs on the same release binary measured `allocation-survival` **301 ms map -> 260 ms dense**, paired **14.1% faster**, row 95% CI **11.2%..15.5% faster**, bootstrap geomean 0.8593x with `ALL_CORRECT=1`. Focused unit/integration coverage includes extracted no-super and `super` methods, nested arrows, GC stress, old-to-young barriers, major/minor collection, and recycled slots. This postdates B161 and is not a revised full-corpus claim. |
| **B162 VM-owned object-literal transition cache** | **REJECTED AND FULLY REVERTED — SAFE INTEGER METADATA WAS ~1.9x SLOWER** | A second shape experiment deliberately avoided B152's unsafe model: VM-owned records contained only function/ip/name/shape/length integers, used an authoritative first `shape::add`, checked exact live predecessor and length, preserved the barrier, declined mismatches before mutation, and fell back for DICT/cap state. Focused equivalence, eval-id, GC, delete/redefine and cap tests passed. Performance decisively rejected it: 25 same-binary paired runs measured stable shapes **363.8 ms cache vs 190.0 ms off (1.915x slower)** and megamorphic shapes **422.7 vs 226.6 ms (1.865x slower)**, exact output. Per-append thread-id plus map lookup cost overwhelmed the already-cheap transition hit. The implementation, helper arity change, tests and switch were removed; a zero-symbol audit confirms none ships. Do not retry a per-property map lookup without first proving it cheaper than `shape::add`. |
| **B161 Wave 39 hostile checkpoint** | **MAJOR GENERALISATION GAIN; STRICT HOSTILE PARITY STILL OPEN** | The final clean-environment dirty-tree diagnostic uses all 17 cases, 15 counterbalanced repetitions and 10,000 bootstrap samples. It is exact and healthy with no mid-run drift, but correctly records `publishable:false` because the measured engine/publication sources are dirty/untracked. Cold Zipp/Node is **1.3564× category-balanced [1.350,1.363]** and **1.2866× row geomean [1.281,1.293]**, down from Wave 30's 3.1023×/2.7173×. Below-Node rows include ephemeral allocation 0.368×, modules 0.396×, stable types 0.464×, throw/catch 0.527×, async burst 0.635× and bytecode VM 0.895×. Remaining rows: survival 4.623×, stable/megamorphic shapes 3.871×/3.848×, router 3.792×, React-ish 3.272×, NanoID 2.566×, closures 1.976× and type churn 1.686×. Relative cold degradation is shape 0.984×, calls 4.250×, types 3.679×, lifetime 12.539×, async 1.695×, errors 0.526×. The every-category ≤1.15× / every-row ≤1.50× gate is not met. `bench/hostile/w39_final_cleanenv_dirty_2026-08-25.json` |
| **B160 JIT route, replay, metering and panic boundaries** | **LANDED; REAL CALLEE-GLOBAL WRONG ANSWER FIXED, OLD PANIC DEBT CLOSED** | A generic leaf inline could keep raw-routing CALLEE globals after `defineProperty`, because entry validation scanned only caller bytecode. `LeafInlinePlan.direct_global_route_epoch` is now proved independently over the final flattened body (epoch zero, root realm, JIT-eligible/no-EvalScope exact and nested callees, direct live loads and mutable StoreGlobal/Strict slots) and consumed VM-relatively by generic MEM, typed, and flattened INT emission. Same-prototype runtime guards reject each live EvalScope/child-realm callee; `StoreGlobalResolved` remains conservative. A normal-threshold JIT-log-pinned regression covers accessor, non-writable overlay, delete/recreate, typed-off/eager/NOJIT modes; moved persistent state covers both MEM and INT stores. Replay spans are fail-closed pure allowlists, metered bytecode-skipping shortcuts decline, and effectful helper panics after possible effects abort instead of deopting/replaying. |
| **B159 post-W36 native-coverage bundle** | **LANDED; FINAL COMPOSITION MEASURED BY B161, NOT INDIVIDUALLY ATTRIBUTED** | Adds adaptive direct IC misses; Tier-C object/array/function literals, home objects, nullish equality, integer String, cold delete, modulo/coercibility, collection mutation/string case conversion and transactional own-method/global splices; TypedArray length/narrow reads, dense Array length, computed INT leaves, GPR spill homes, MEM Int compare-jumps, first/reserved string-index append, and direct `Promise.all` reactions. Every speculative route has a switch and focused semantic/mechanism tests. Same-binary focused evidence includes bytecode VM computed-leaf ON/OFF **0.2075×** (79.25% faster), NanoID transactional method/global ON/OFF **0.6185×** (38.15% faster), and async-lived direct reactions about **2.5% faster**; do not assign B161's aggregate delta to any one mechanism. |
| **B158 Tier-C negation and captured/global integer-state fusion** | **LANDED; BRANCH CONTROL REACHES NODE, NANOID IMPROVES** | Sequential dirty checkpoints moved branch-control from Wave 34's 2.9098× Node to Wave 35's 1.1002× and Wave 36's **0.9593×**; NanoID at Wave 36 was 4.7781×. B161 confirms branch-control **0.975×** and NanoID **2.566×** after later composition. Switches: `ZIPP_NO_TIERC_NEG`, `ZIPP_NO_TIERC_UPVAL_FORWARD`, `ZIPP_NO_TIERC_UPVAL_INC_I32`, `ZIPP_NO_TIERC_UPVAL_XORSHIFT`, `ZIPP_NO_TIERC_GLOBAL_XORSHIFT`. Negation preserves negative zero and declines nonnumbers before effects. Bounded xorshift rejects internal jump targets, reconstructs skipped destinations in bytecode order, commits last, and is disabled under metering. Cross-artifact deltas are chronology, not isolated A/B. |
| **B157 immutable string-constant cache** | **LANDED WITH BOUNDS; NO ISOLATED SPEED CLAIM** | `ZIPP_NO_CONST_STRING_CACHE`. Unified function/constant-slot keys are capped at 4,096 entries, 256 bytes per literal, and 4,096 eligibility records; cached Values are explicit GC roots. Any function containing unique-buffer mutation bytecodes keeps fresh allocation, while capacity/size/eligibility uncertainty fails closed. |
| **B156 local aggregate SROA and virtual concat length** | **LANDED; EPHEMERAL CONTROL BELOW NODE, SURVIVORS STILL THE WORST DEGRADATION** | A focused earlier checkpoint had ephemeral allocation 7.6989× Node; the Wave 34 combined binary measured **0.3906×**, and B161 confirms **0.368×**. This chronology is not an isolated A/B. `ZIPP_NO_LOCAL_SROA` / `ZIPP_NO_LOCAL_CONCAT_LEN`. Admission is closed, straight-line, provably nonescaping fresh object/dense-array construction with primitive/re-entry proofs; metered and GC-stress VMs decline, and every guard exit materialises created virtual state before interpreter resume. Allocation-survival is still **4.623×** and lifetime degradation **12.539×**, so medium-lived tracing/object representation remains open. |
| **B155 closure/method dispatch and widened typed leaves** | **LANDED; LARGE NANOID/CLOSURE PROGRESS, PER-ROW PARITY OPEN** | Sequential NanoID dirty checkpoints moved 6.1537× → 6.0458× → 4.9603×; the Wave 34 combined binary was 4.6965× and B161 is **2.566×**. Wave 34 closures were 2.3515×; B161 is **1.976×**. Mechanisms include Tier-C upvalues, polymorphic/native cross-calls, same-prototype/default capture-free leaves, random-method admission, 40-register leaves and typed global loads (`ZIPP_NO_TIERC_UPVAL`, `ZIPP_NO_POLY_CROSSCALL`, `ZIPP_NO_POLY_LEAF_INLINE`, `ZIPP_NO_TIERC_RANDOM_METHOD`, `ZIPP_NO_METHOD_CROSSCALL`, `ZIPP_NO_WIDE_LEAF`, `ZIPP_NO_TYPED_GLOBAL_LOAD`). Live identity/property/realm/EvalScope/default-undefined and scratch-window guards fail closed. Chronological captures are not per-feature attribution. |
| **B154 narrow lexical-arrow unboxing** | **LANDED; NANOID CHECKPOINT 10.4932× → 6.1537× CHRONOLOGICALLY** | `ZIPP_NO_ARROW_LEXICAL_UNBOX`. The Zipp median moved 933.292 → 550.982 ms across separate dirty builds, about 41% lower; use as directional chronology only. Only simple first-referenced `let`/`const` declarations in block-bodied arrows use registers. Captures, direct eval, forward access, destructuring, multiple declarations, classes and other TDZ-sensitive shapes retain cells. Exact TDZ, capture, eval and fused-string bytecode regressions accompany it. |
| **B153 loader ICs and live module-namespace JIT reads** | **LANDED; MODULE ROW NOW WELL BELOW NODE** | Wave 31 same-binary seven-repetition module diagnostics measured default **1.4937× Node**, `ZIPP_NO_MODULE_IC=1` **2.5095×**, and `ZIPP_NO_JIT_MODULE_NS_GET=1` **2.7874×**; B161's composed result is **0.396×**. Loader-recorded immutable ranges exclude eval/new-Function gaps. Namespace ways resolve the exact side-table slot, re-read the live global value, guard namespace version, and decline deferred/unlinked/missing/TDZ state. The boot-sized global allocation is nonmoving. The switch captures are valid ablations but were separate runs and remain dirty diagnostics. |
| **B152 native shape-keyed property way** | **REJECTED AND FULLY REVERTED — ~10.6% STABLE-SHAPE WIN DID NOT CLEAR SECURITY REVIEW** | The experiment emitted a native own-property way backed by a raw shape-metadata view and measured about **10.6% faster** on `shapes-stable`. Independent review found two design-level blockers: VM-exotic heap objects can collide with ordinary object shape IDs, invalidating the assumption that a shape uniquely selects ordinary property storage; and metadata refresh/pointer mutation paths could leave native code observing stale storage. Either can turn a performance guard into a wrong-slot access or obsolete-pointer dereference. The entire experiment was reverted instead of narrowing around symptoms. A zero-symbol audit found no experimental metadata view, native gate, helper, or counters: **no unsafe shape-way code ships**. Retry only with a memory-safe ownership model and explicit exotic-kind separation; the next property/call target is a prototype-keyed closure JIT lane, not this raw-pointer design. |
| **B151 `closure_home` / `closure_new_target` are directed GC edges, not roots** | **LANDED — RETENTION CATASTROPHE FIXED; SANDBOX 128 MiB RUN NOW PASSES** | Both keyed side tables were scanned as unconditional roots, so a dead object-literal home↔method cycle and an arrow's lexical-`new.target` cycle became immortal. Values are now traced only while the keyed closure is reachable, dead keys are pruned, and both record paths issue the nursery write barrier for an old closure pointing to a young value. On the hostile allocation-survival profile: **peak slots −81.4%, average live slots −88.6%, GC time −45.6%**; the workload now completes under the sandbox's 128 MiB approximate heap ceiling, and the final cold row is **5.336× Node** rather than the first sweep's 7.717×. Unit tests prove dead major-cycle collection, old-key→young-value survival across a minor, and `closure_new_target` non-root behavior; integration tests preserve extracted-method `super`, lexical `new.target`, and nested-arrow home inheritance through repeated GC. The next allocation target is local-allocation SROA for provably non-escaping aggregates. |
| **B150 loader-recorded module functions reach the native tiers** | **LANDED; LARGE ELIGIBILITY WIN, MODULE/NPM PARITY STILL OPEN** | Module-loader function ranges are now eligible for x86 function/OSR JIT and the guarded ARM64 whole-function tier instead of being rejected solely for living after `main_func_count`. Range membership is half-open and loader-recorded, so unrelated `eval`/`new Function` functions in the gaps remain ineligible. With `ZIPP_NO_MODULE_JIT=1` as the comparator, the hostile ESM graph moved **234.655 → 188.504 ms (1.245× faster)** and exact vendored `nanoid` moved **1742.314 → 925.523 ms (1.883× faster)**. The module graph profile moved from 100% interpreted to 17.9% JIT-fast; `nanoid` moved to 70.7% JIT-MEM + 4.4% JIT-fast with 18.8% remaining interpreted. The final combined cold ratios are still **3.914× and 10.237× Node** respectively, so this closes an eligibility defect, not the broader module or npm gap. Integration/range-gap, no-default-feature, GC-stress, and compiler-M1 checks accompany it; the final combined corpus remained exact. |
| **B149 guarded dense computed calls** | **LANDED — `bytecode-vm` 0.2212× DEFAULT/OFF, EXACT** | x86 MEM/Tier-C regions now handle `CallMethodComputed` only when the key is a canonical numeric index, the receiver is a dense Array with an own non-hole element, and the live element is an ordinary function/closure; `this` remains the array. Holes/prototypes, index overrides/accessors, mapped arguments, native/bound/proxy/exotic/non-callable targets, and malformed values decline without observable work. Nine one-binary paired repetitions measured **915.75 ms with `ZIPP_NO_COMPUTED_CALL_DENSE=1` versus 202.11 ms default**, default/off **0.221159× (77.9% faster), 95% CI −78.1%..−77.7%**, with exact output. The hostile bytecode-interpreter gap fell from roughly 16× before the mechanism to **4.000× Node** in the final combined capture; it is materially better and still not parity. The artifact is a dirty-tree diagnostic and correctly says `publishable:false`: `bench/hostile/w30_computed_call_abenv_2026-08-24.json`. |
| **B148 saturated plain-call IC fallback** | **LANDED — `calls-closures` −11.0%, BUT CLOSURE DEGRADATION REMAINS THE WORST FAMILY SIGNAL** | After an eight-way identity call IC saturates, a live ordinary function/closure is resolved dynamically and frame-called instead of forcing recurring deopt/eviction. Native, bound, proxy/exotic, and non-callable targets stay on the existing path; focused coverage was added for live replacement and lexical-`this` semantics. Nine one-binary paired repetitions measured **398.11 ms with `ZIPP_NO_POLY_CALL_FALLBACK=1` versus 354.14 ms default**, default/off **0.889559× (11.0% faster), 95% CI −12.1%..−10.4%**, exact output. This is a real local win, but final `calls-closures` remains **7.576× Node** and the call-family stressor/baseline degradation is **15.722× worse than Node's**, so the closure/upvalue/frame-call problem remains open. The next targeted design is a prototype-keyed closure JIT lane. Dirty A/B diagnostic, `publishable:false`: `bench/hostile/w30_poly_call_abenv_2026-08-24.json`. |
| **B147 hostile-runner fidelity and publication state** | **LANDED; FAILS CLOSED FOR PUBLICATION, REDACTS ENVIRONMENT SECRETS, NOT A SANDBOX** | `tools/bench_hostile.py` supports script/module goals, exact repeatable stdout, paired empty launches, Node/Zipp order balanced exactly for even repetitions and within one for odd repetitions, cold and immediately-paired startup-adjusted metrics, category-balanced geomeans, family ratio-of-ratios, filters, and atomic publication of a fully written JSON file. It hashes its own source and imported `tools/bench.py`, the manifest and every declared input, and the engine before/after; drift, process/output failure, dirty/non-HEAD source, or any diagnostic override keeps `publishable:false`. Publication additionally requires the canonical default manifest, the full unfiltered corpus, at least 15 repetitions, at least 10,000 bootstrap samples, and every manifest/harness/input file tracked and byte-clean against `HEAD`; stationary local edits cannot masquerade as canonical inputs. Prefix-wide raw environment capture was a credential leak risk (`NODE_AUTH_TOKEN`, `DENO_AUTH_TOKENS`, numeric custom Zipp tokens); artifacts now retain values only for an explicit allowlist of audited numeric/boolean controls and redact unknown keys, credentials, paths, or arbitrary runtime values. The shared provenance fix preserves every recorded violation and lets an override cover only the matching rejection instead of erasing the reason and accidentally making a dirty artifact publishable. Module imports are not discovered: each reviewed graph must list every dependency, so the path check covers declared inputs only and the runner remains trusted measurement infrastructure. Family degradation remains a composite scenario signal rather than an isolated feature cost, and the category taxonomy is frozen by the canonical manifest for each publication series. Separately, the compiler no longer lowers syntactic `Math.random()` to an unconditional random opcode: dynamic replacement is now observed, which restores ordinary semantics and makes the exact vendored npm driver deterministic without rewriting package source. |
| **B146 hostile 17-case application corpus** | **FINAL WAVE 30 DIAGNOSTIC IS 3.1023× NODE; BROAD PARITY IS NOT REACHED** | Six paired families deliberately vary nested/mutable closure calls, stable/megamorphic shapes, stable/mixed locals, branches/exceptions, ephemeral/surviving allocation, and burst/long-lived async work; five standalones add React-shaped reconciliation, a warm router, a JavaScript bytecode VM, an ESM graph, and exact `nanoid@3.3.17/non-secure` source with licence/provenance. The historical first dirty sweep was **3.0076× cold category-balanced / 2.6501× row geomean** and is retained only as context. The final seven-repetition combined artifact is `ALL_CORRECT=1`, healthy and drift-free, but its dirty source correctly makes it **`publishable:false`**: **3.1023× cold category-balanced / 2.7173× ordinary geomean**. Notable rows: `nanoid` **10.237×**, allocation ephemeral **7.870×**, closures **7.576×**, allocation survival **5.336×**, bytecode VM **4.000×**, ESM graph **3.914×**; controls stable locals **0.497×**, throw/catch **0.548×**, async burst **0.635×**. Cold relative degradation: calls **15.722×**, types **7.941×**, async **1.826×**, object shape **1.542×**, object lifetime **0.680×**, errors **0.171×**. Adjusted values remain noisy diagnostics and the final adjusted aggregate is unavailable after a non-positive paired Node work estimate. This corpus never enters the retained-ten headline and Zipp does **not** claim Node parity on it. `bench/hostile/w30_combined_dirty_2026-08-25.json`, `bench/hostile/README.md` |
| **B145 guarded ARM64 native baseline** | **LANDED AS A CORRECTNESS BASELINE; NO PERFORMANCE CLAIM** | Hot, call-free whole functions containing tagged-i32 loads/arithmetic, comparisons, branches, loops, and returns now compile natively on AArch64. Every type, overflow, and negative-zero guard bails at the exact bytecode ip without clobbering its destination; unsupported functions stay interpreted. The first tier deliberately has no helper calls, OSR, regex codegen, or native meter: attaching instrumentation disables it, as does sandbox mode. One body is capped at 4,096 ops/registers, each VM's emitted-code cache at 16 MiB and 4,096 retained executable allocations, and 64 chronic exits evict and blacklist a body. Docker Desktop's QEMU path executed the generated instructions as `aarch64`: 5/5 backend mechanism tests (including a greater-than-32 KiB far-bail relocation regression), the end-to-end hot/mixed-input test, and the complete post-fix VM library suite (**373 passed, 1 ignored, 0 failed**) were green. Cross-compilation also passed for `aarch64-linux-android` with all targets and with instrumentation. Native Linux ARM64, Windows ARM64, and macOS ARM64 runners are wired for the library suite, with the Linux differential and sandbox gates. Emulated timings are intentionally not quoted. |
| **B144 captured-limit field streams close B142's worst IIFE row** | **LANDED; WORST REDUCER GAP CLOSED, BROAD SCOPE PENALTY STILL OPEN** | The exact guarded cyclic field read/write plans now accept a live `UpvalGet` loop bound as well as `LoadGlobal`. The helper resolves the running closure cell on every entry and fails closed on malformed, TDZ, non-Int, accessor/prototype, or mutation-sensitive shapes; the unchanged MEM region then preserves ordinary semantics. On the retained `property-ic-shapes.R1_iife` variant, 15 interleaved paired repetitions on the same release binary measured **1,015 ms with `ZIPP_NO_FIELD_READ_STREAM=1` vs 14 ms default**, default/off **0.0137x (98.6% faster), 95% CI −98.65%..−98.56%**, with exact output. The schema-v2 artifact records the dirty source stamp and `ALL_CORRECT=1`; the full field-stream suite is 15/15 green, including live bound mutation and non-Int fallback. This closes the single 4.6x-Node IIFE catastrophe from B142; it does **not** erase the suite-wide +159% IIFE geomean or the local/register-pressure work still described in B142/B143. `bench/w29_property_ic_iife_field_stream_abenv_2026-08-24.json` |
| **B143 B142's mechanism, corrected: it is not block scoping, it is that live-range narrowing only reaches GLOBAL SLOTS** | **CORRECTS B142's framing. The next wave's target is now exact** | B142 proposed that a block-scoped `let`/`const` in a loop body should occupy the same register a `var` would. That framing is WRONG and the refutation is one experiment: hoisting both `let`s out of the loop entirely (`let le, v;` before the outer loop, assigned inside) does NOT recover the phase -- var-in-body 1ms, let-in-body 365ms, **let-hoisted-out 364ms**, Node flat at 95ms. So per-iteration binding semantics are not the cost. The real distinction is WHERE THE VALUE LIVES. At top level a `var` is a global SLOT, and wave 13's stored-global live-range narrowing (`ZIPP_NO_GLOB_RANGE`) narrows exactly those: the fast plan logs `narrowed=[14, 21]` -- slots 14 and 21 ARE `le` and `v` -- freeing two permanent homes and bringing the region to `homes=9`, inside the pool after sharing. A `let` is a lexical binding outside that slot space, so it is invisible to the pass: `narrowed=[]`, homes stay high, and `plan_region_cold` declines before reporting a home count at all (no `homes` line is emitted for the DV regions in the `let` shape). PROVED BY ITS OWN OFF-SWITCH: `ZIPP_NO_GLOB_RANGE=1` on the *`var`* program reproduces the failure, moving the phase 0ms -> **202ms** (falling to the DOUBLE tier), so glob-range narrowing is exactly what carries the fast case and it only reaches global slots. **The inversion worth internalising: a value is currently CHEAPER as a top-level global than as a local**, which is backwards from every other engine, and it is why `bench/real`'s top-level-`var` style is the engine's best case rather than a neutral one. The pass already computes segments for REGISTERS as well (`seg_map` over `reg_order`, `plan_region.rs`), but the whole block is gated behind `(admit_dv || share_homes) && reuse && cold.is_empty()` and does not run for these regions in the `let` shape -- that gate, and extending narrowing to non-global bindings, is the next wave's exact target. |
| **B142 the scope penalty is SUITE-WIDE, and its worst case is a tier demotion, not a declining reducer** | **THE BIGGEST OPEN ITEM. The fix is compiler-side register pressure, NOT emitter-side spilling** | B140 measured two rows; this measures all of them with `bench/scope/sweep.py`, which generates the rewrite, checks every variant against Node for identical output, then interleaves all four cells inside each repetition. Wrapped in an IIFE: **geomean +159% over 13 rows, 8 rows cross from beating Node to losing to it**. `var` -> `let`: **geomean +60% over 12 rows, 6 cross** (`parse-large-js` excluded from `let` only -- it embeds JS source as DATA, so a textual rewrite edits the corpus it parses; that is the rewrite's property, not the engine's). Worst, zipp/Node before->after: `property-ic-shapes` 0.043->4.621 under IIFE (11ms->1208ms), `parse-large-js` 0.947->3.619, `regex-log-scan` 0.969->3.106, `polymorphic-objects-v2` 0.360->3.650, `typedarray-math` 0.665->2.443 under `let`. **The published all-13 result is a result for top-level `var` code** -- honest for what it measures, and the programs are the unchanged historical series, but real JavaScript lives in functions and is spelled `let`/`const`. ROOT-CAUSED TO THE OPCODE on the worst case: changing only the DataView inner loop's BODY locals (`var le`/`var v`) to `let` moves that phase 1ms -> **361ms** while Node stays flat at 95ms (0.01x -> **3.80x** Node); loop COUNTERS as `let` cost almost nothing (61ms, 0.64x). Chain from `ZIPP_JITLOG` + `ZIPP_JITDECLINE`: more live homes -> `INT-GPR decline: 13 homes > 8 gprs` -> the region falls to the **MEM boxed tier** (~3.5ns/op, B39). **Losing the reducer is only 1->61ms and zipp still WINS; the 61->361ms tier demotion is what loses to Node**, so this is not an overfit story, it is a general defect. REFUTED en route, each by instrumentation or measurement: not GC (`ZIPP_GCSTATS` reports 0 collections on both sides); not the pin plan (every `continue` in `build_ta_pin_plan` instrumented, zero declines, DV pins built on both sides); not the W9 DV retry gate (instrumented, it PASSES -- `admit_dv_unadmitted=Some(0) pins=1 access=3` -- the GPR emitter declines after it); not the existing dark spill-slot mechanism (`ZIPP_GPR_SPILL_SLOTS=1` was built for 12-14-home regions exactly like this and gives 361->**369ms**, so wave 9's refutation holds on this shape too); not identifier text; not program size (top-level `let` has no function wrapper and still costs +60%). **Therefore the fix is register PRESSURE, not register SPILLING: a block-scoped `let`/`const` in a loop body that no closure captures should occupy the same register a `var` would rather than adding a live home -- a compiler change upstream of the emitter that currently declines.** Repro `python bench/scope/sweep.py`; see `bench/scope/README.md`. |
| **B141 three "the mechanism is net-negative on its own row" leads, all REFUTED; and the two sparse folds serve different rows** | **REFUTED — do not re-chase; the sequential-median script that produced them is unsound** | A batch script timing `base`, then each switch in turn, 9 medians each and no interleaving, reported three shipped mechanisms as costing time on their own target rows: `ZIPP_NO_ASYNC_SETTLED_TRAMPOLINE` "0.835×", `ZIPP_NO_SPARSE_FORIN_FOLD` "0.635×", `ZIPP_NO_SPARSE_NUM_INDEX` "0.818×". Re-measured with the standing instrument — ONE binary, `--ab-env`, 21 paired reps, interleaved — all three invert or vanish: the async trampoline OFF is **+6.1% [+5.1, +6.4]** (the mechanism is a real −6% win), `SPARSE_NUM_INDEX` OFF is **+43.4% [+42.8, +43.8]** (a real −43% win), and `SPARSE_FORIN_FOLD` OFF is **−0.3% [−0.9, +0.9]**, null. **The lesson is methodological: a base-then-each-switch loop attributes machine drift to whichever switch was measured late.** Only interleaved paired sampling is safe, which is what `tools/bench.py` already does and why ad-hoc timing scripts must not be used to price a mechanism. One real finding survives the refutation, as a corrected ATTRIBUTION rather than a defect: `SPARSE_FORIN_FOLD` is null on `sparse-array-v2` even with `SPARSE_NUM_INDEX` also off (−0.2% [−0.8, +1.0]), but on the headline `sparse-array` row the same comparison costs **+19.2% [+18.7, +20.1]**. The two sparse mechanisms are not redundant and neither masks the other; they simply serve different rows, and `SPARSE_FORIN_FOLD` should be priced against `sparse-array`, not against the diagnostic it sits next to. `bench/w27_async_tramp_abenv_2026-08-24.json`, `bench/w27_sparse_forin_abenv_2026-08-24.json`, `bench/w27_sparse_numidx_abenv_2026-08-24.json`, `bench/w27_forin_masked_abenv_2026-08-24.json` |
| **B140 the reducers are scope-sensitive: the same program in a function or with `let` loses a third of the win** | **CHARACTERISED, NOT A DEFECT — the largest generalisation opportunity now open** | The wave 25/26 guarded reducers key on TOP-LEVEL `var` globals, so they decline on the same source wrapped in an IIFE or written with `let`. Measured on the plain release build, 9 interleaved paired reps, zipp vs Node per variant: `typedarray-math` original **0.623×** Node, wrapped in an IIFE **0.864×** (+39%), `var`→`let` **0.899×** (+44%), locals renamed **0.611×** (unchanged). `sparse-array` original **0.885×**, IIFE **1.186×** (+34%, crosses from beating Node to losing to it), `let` **1.022×** (+15%), renamed **0.877×**. REPLICATED on a quiet machine after a leftover `ZIPP_GC_STRESS=1` process from an earlier agent run was cleared: typedarray 0.668× / 0.910× (+36%) / 0.963× (+44%) / 0.659×, sparse-array 0.962× / 1.260× (+31%) / 1.120× (+16%) / 0.951×. The relative penalty is the finding and it moved by at most 3 points; interleaving engines and variants inside each repetition is what made the ratios survive the load, and the same check re-run on `ZIPP_NO_ASYNC_SETTLED_TRAMPOLINE` gave +5.71% [+4.98, +6.82] quiet against +6.1% [+5.1, +6.4] loaded. **Quote the quiet numbers; absolute ratios do shift.** Node is flat across all four variants (227/227/227/231ms and 93/93/94/94ms), so the sensitivity is entirely zipp-side. **Renaming is free, so the reducers are not keyed on identifier text** — the refutation that matters, since that was the obvious suspicion. Phase decomposition attributes essentially all of it to ONE phase: `ta_phase.js` `dataview=0ms` at top level vs `65ms` in an IIFE and `59ms` with `let`, against Node's 98/97/90. One-binary ablation confirms the mechanism exactly: `ZIPP_NO_DV_NESTED_REDUCE=1` moves that phase 0ms → 63ms with byte-identical output. Root cause is structural and visible in the plan gate — `dv_nested_reduce_plan` (`codegen/region_int_gpr.rs:521`) matches only `Instr::LoadGlobal`/`dv_store_global` for both induction variables and the accumulator, so a lexical or frame-local binding cannot match. Note also what the mechanism IS: it executes one real inner pass and applies the affine accumulator delta to the remaining outer iterations, so on this row zipp performs ~1/6000 of the arithmetic Node performs. That is a sound whole-loop transformation for a provably pure nested reduction, but it means `typedarray-math`'s margin over Node is attributable to it — with the reducer off the row measures ~1.05× Node instead of 0.66×. **Next wave: extend the plan gate from global slots to frame-local and lexical bindings**, which is where real programs put these loops. `scratchpad/sustained/*.R1_iife.js`, `*.R2_let.js`, `*.R3_rename.js` |
| **B139 the kill-switch latch was hand-written 34 times and forgotten 8 times; latching the two per-instruction ones measures NULL** | **LANDED as hygiene, REFUTED as a speed win** | `region_admit`'s `leaf_getprop_enabled()` and `iter_region_enabled()` are consulted from INSIDE the per-instruction admission walk (`for (i, instr) in code.iter().enumerate()`), so each ran an unlatched `env::var_os` per `GetProp` / `IterNext` / finally-bracket op examined, on every region compile — a direct violation of the standing "never `env::var` on a hot path" rule. Instrumented counts are under 250k per benchmark run, and the measurement says that is not enough to matter: full 13-row two-binary A/B at 15 reps **1.0005× (+0.05%), CI [−0.19%, +0.43%] — NULL**. Two rows moved outside CI on the first run (`json-large` −2.1%, `property-ic-shapes` +2.5%) and NEITHER replicated in direction: `json-large` went −2.1% → −0.2% → +1.4% across three builds of decreasing scope, which is the fat-LTO code-layout confound §2 warns about, and is exactly why `leaf_getprop_enabled`'s own comment says the switch exists so the mechanism can be A/B'd on ONE binary. A latch removal cannot be A/B'd on one binary, so this change is not cleanly measurable by the standing instrument; the 13-row geomean is the honest summary and it is null. Kept anyway, narrowed to the two per-instruction sites: it removes a real per-op environment lookup and replaces the 43rd hand-copy of the latch with one `env_off_switch!` macro — the "fact maintained by hand in several places" shape behind every silent wrong answer this engine has had. The six per-region-compile switches were converted, measured, and REVERTED: they are read once per compile, gained nothing, and narrowing did not remove the `property-ic-shapes` movement, which confirms that movement was layout and not attributable to them. Also reunites an orphaned doc comment that `tools/split_rs.py` had stranded above `iter_region_enabled` with its real owner `region_can_compile`. Gate: 1347 zipp-vm + 108 zipp-regress tests, 0 failures. `bench/w27_latch_ab_2026-08-24.json`, `bench/w27_latch_ab2_2026-08-24.json`, `bench/w27_narrow_ab_2026-08-24.json` |
| **B138 wave 26 closes the last two gaps and publishes 13/13** | **GOAL REACHED ON EVERY MEASURED ROW — all-13 zipp/Node 0.572835× [0.569491, 0.576240]** | Two guarded mechanisms close the Bun rows left by B137. (1) Transactional compact `JSON.stringify` for main-realm plain data graphs publishes nothing until the whole walk succeeds and declines getters/accessors, `toJSON`, replacers, indentation, sparse arrays, custom prototypes, proxies, cycles, depth limits and unsupported values to the ordinary serializer: 21-pair one-binary A/B **0.905395× [0.879598, 0.929469]**, **−9.46%**, `ZIPP_NO_JSON_PLAIN_FAST`; final JSON 189.65ms vs Bun 200.97ms. (2) The INT tier batches the tokenizer's three adjacent discarded-result `Array#push(int)` calls only after staging all arguments and atomically preflighting three distinct pinned dense-Int receivers; engagement, decline and replay are pinned by focused tests, `ZIPP_NO_INT_PUSH3`; final parse 226.95ms vs Bun 236.05ms. Clean PGO source `cc0d5578314c49890150b19499d496dbc6abe131`, schema v2 cold metric, 21 paired repetitions with deterministic engine/benchmark shuffling, seed 1511464998, 10,000 bootstrap samples, Node 24.12 / Bun 1.3.14 / Deno 2.6.10, `publishable:true`, **1,092/1,092 healthy**, `ALL_CORRECT=1`, and 13/13 strict lowest medians. Retained ten **0.785991× [0.782006, 0.790530]**; diagnostics **0.199559× [0.196732, 0.201854]**. Startup 10.72ms vs Node 34.46, Deno 51.55, Bun 64.01. Binary SHA-256 `94d50cb2f9bcadba91c83516dcdc4eb502dd71824ba655ee83f45cb1a564dae2`. `bench/four_engine_cc0d557_pgo_2026-08-24.json`, `bench/w25_json_plain_stringify_abenv_2026-08-24.json` |
| **B137 wave 25 guarded reducers** | **LANDED — every focused off-switch gate exact; final four-engine composition is B138** | Exact guarded reducers remove repeated boxed dispatch from settled-promise chains, nested DataView loops, enumeration/count/`in`/copy-length and sparse folds, property-field read/write/sum/mixed streams, JSON walks, Markdown inline parsing, span/code-unit predicates, string appends, and array `matchAll`. Focused off-switch A/Bs: property-IC field stream **0.0165×**, poly-v2 enum reduction **0.1913×**, sparse-v2 reducers **0.2524×**, typedarray nested DataView **0.7033×**, JSON walk **0.7365×**, parse spans **0.8426×**, Markdown **0.8541×**, async trampoline **0.9421×**; all outputs exact. Every path has a `ZIPP_NO_*` comparator and fails closed on unsupported receiver/realm/prototype/effect shapes. Full release VM, regress `rx-jit`, CLI and wasm suites passed; focused fallback/off-switch/GC/tier tests landed, and the regexp acquisition force gate was isolated from process-global races. Source `3be6906`; evidence under `bench/w25_*_abenv_2026-08-24.json`. |
| **B136 headline Node parity on a clean PGO build** | **GOAL REACHED FOR THE RETAINED TEN — zipp/Node 0.969460× [0.965450, 0.974097]** | Clean commit `200cbfc`, 21 paired repetitions with deterministic engine/benchmark shuffling, Node 24.12 / Bun 1.3.14 / Deno 2.6.10, `publishable:true`, and **ALL_CORRECT=1**. The entire headline interval is below 1.0: zipp is ~3.1% faster than Node, ~18.8% faster than Bun and ~4.1% faster than Deno on the retained ten. Rows vs Node: map 0.779, typedarray 0.935, class 0.951, parse 0.956, async 0.974, JSON 1.012, markdown 1.016, regex 1.018, sparse 1.038, poly 1.048. Startup is 10.6ms vs Node 34.4, Deno 52.9, Bun 63.7. **Classification matters:** diagnostics are separately 2.206709× [2.195027, 2.217186], making the legacy all-13 number 1.172099×; neither number retracts headline parity, and headline parity is not a claim of broad architectural parity. Binary SHA-256 `e9d91210985faa49f093480631b3b1fb972578b62a2d1cba72e7191034ef5d02`. `bench/four_engine_200cbfc_pgo_2026-08-24.json` |
| **B135 suffix starts + scalar matchAll + scalar exec + exact Array delete** | **LANDED — default/off-switch headline ratio 0.967348× [0.962856, 0.970919]** | The final four mechanisms compound to `regex-log-scan` **−19.35% [−20.08, −18.76]** and `sparse-array` **−8.35% [−8.95, −6.74]** versus their off-switch comparator, with diagnostics 0.99896× (flat) and all 13 outputs exact. (1) a conservative ASCII suffix-start plan (`RequiredPrefix` or `RunLiteral`, capped at 64) cuts regex **5.32%**; 239,110 deterministic comparisons and the full rx-jit suite agree. (2) Exact-shape scalar `matchAll` keeps the pending match rooted and materialises only at an observable boundary: **−7.28%**, 299,984 results elided. (3) Exact non-global four-capture `exec` scalarisation directly feeds four `ToNumber`s while preserving Annex-B statics and every deopt/throw/re-entry exit: **−7.40%**, 149,991/149,992 results elided. (4) MEM-tier `DeleteIndex` on an exact Array preserves version bumps and rejects overlays/arguments: headline sparse **−7.61%** isolated. Switches: `ZIPP_NO_RX_SUFFIX_START` plus its two plan switches, `ZIPP_NO_RX_SCALAR_MATCHALL`, `ZIPP_NO_RX_SCALAR_EXEC`, `ZIPP_NO_JIT_ARRAY_DELETE`. `bench/w24_regex_delete_exec_combined_all13_abenv_2026-08-24.json` |
| **B134 wave 21 verified; concat/direct-call/GPR follow-ons landed** | **LANDED AFTER THE GATE THE WIP COMMIT LACKED** | The original boxed-home/GetProp/own-accessor and INT-push/bool-reuse work is no longer “unverified.” Follow-ons add own-getter BOXREF, pinned-push filtering, pure concat append, right-pair and `pad2` fusion, direct RegExp/string call lowering, and a GPR deopt shadow. The deopt shadow alone is **0.987765× vs its off-switch** on the headline [0.982016, 0.990063]; concat pair + `pad2` moves regex 6.31% in its focused off-switch gate. Verification: full release suites (`zipp-vm` library 364 pass/2 ignored plus integrations, `zipp-regress --features rx-jit`, CLI, wasm); 20,000 fresh generated programs ×40 modes with **0 divergence / 0 nondeterminism**; Test262 `defaaf15` in default, NOJIT, forced-JIT, and no-nursery modes, each **95,936 pass / the same 6 expected failures**; 13/13 benchmark outputs exact in JIT and interpreter modes. At that capture, the only workspace-test exception was the process-global `rx_acqgate_threshold_and_streams` force race under combined `utf16,rx-jit`; B137 later isolated it in a child process. Source and evidence committed at `200cbfc`. |
| **B133 tempting follow-ons that died to measurement** | **REFUTED AND REMOVED; RAW EVIDENCE RETAINED** | DataView bounds reuse did not survive its A/B. Sparse Array overlay `GetIndex` was **−0.13% headline [−1.06%, +0.88%]** and +0.05% on its diagnostic, so it was removed while the exact delete helper stayed. Weak capture caching, lazy-result/lazy-element variants, and iterative regex either had no valid mechanism or failed the benefit/risk gate. The stale `target/release/zipp.exe` once contained the rejected DataView code; published numbers use a freshly rebuilt `target/x86_64-pc-windows-msvc/release/zipp.exe`. `bench/dv_bounds_reuse_abenv_2026-08-23.json`, `bench/w24_sparse_overlay_get_*` |
| M0.1 counterbalanced harness | **IMPLEMENTED; A/A DRIFT OPEN** | paired AB/BA observations, raw schedules, bootstrap intervals, metadata, timeouts, and schema-v1/v2 reading; an A/A regex rerun reversed from −0.4% to +1.1% with both nominal CIs excluding zero, so ~1% claims still require independent replication |
| M1.1 compiler global lookup | **HISTORICAL WORKTREE RESULT — RE-AUDIT BEFORE REUSE** | 3k/6k/12k/24k generated-function sweep stayed approximately linear; largest/middle ns-per-MB ratio 0.975 |
| M1.2 expression-arrow analysis | **HISTORICAL WORKTREE RESULT — RE-AUDIT BEFORE REUSE** | analysis consumed the expression directly; capture/`this`/`arguments`/`super`/`await` tests passed at the time |
| M1.3 first-way shape `SetProp` | **HISTORICAL WORKTREE RESULT — SUPERSEDED BY LATER IC WAVES** | NOJIT own-store microbenchmark −46.66% (95% CI −47.80% to −45.51%); removing it was +0.52% on four affected suite rows (95% CI −0.66% to +1.53%) |
| M2 regular-subset regex tier | **EXPERIMENTAL, OFF BY DEFAULT** | regex row −2.82% (95% CI −3.8% to −2.0%), far below the 25% promotion gate; feature binary +14.7% |
| default regex capture-name clone removal | **REVERTED** | restoring the original code measured −0.51% (95% CI −1.05% to −0.24%), inside the independently observed ~1% A/A drift floor |
| M4.0 TypedArray guard reduction | **REVERTED** | −0.11% (95% CI −1.10% to +0.55%): statistically neutral |
| M3-M5 object metadata, CFG/SSA, arena/nursery | **OPEN** | these remain the required architectural path toward broad V8 parity |
| B67 three open tier divergences + eval `SetProp` | **LANDED, FREE** | `tests/jit_tier_parity.rs` 11/11 with zero ignored; two of the three had an interpreter conformance bug UNDERNEATH the tier bug, so four interpreter answers changed to match node as a script. Suite geomean **+0.23%, 95% CI [−0.16%, +0.42%]** (21 pairs, interval includes zero); binary 2,560 bytes smaller. The first attempt cost `map-set-heavy` +3.9% by reading the heap on every `LoadGlobal` — re-gated on a `u32` epoch compare, isolated at +12% on a top-level global loop |
| B67 owned JSON keys (plan M2.2) | **LANDED, `json-large` −3.9% (REPLICATED)** | `ObjMap::set_owned` removes the second allocation per first-inserted JSON key. −3.9% [−5.4, −2.2] and −3.9% [−5.0, −3.2] across two independent 21-pair runs. Order/duplicate/reviver/`context.source` invariants pinned by `tests/json_owned_keys.rs` |
| B67 corrected benchmarks (plan M0.3) | **ADDED, OUTSIDE `ALLBENCHES`** | `property-ic-shapes` (the M3 acceptance benchmark, 1..1024 same-shape receivers), `polymorphic-objects-v2`, `sparse-array-v2`. Output byte-identical to node. Excluded from the timed ten so the retained geomean stays comparable |
| plan M2.1 `typeof`-local fusion | **OPEN — measurement-blocked** | needs an exact-HEAD baseline + 15-21 pairs; belongs in `compile/exprs.rs` beside the existing AST-level fusion, not in a bytecode def-use pass |
| plan M2.3 `regexp_string_iters` → `SlotTable` | **REFUTED, REVERTED (B68)** | built and measured: `regex-log-scan` **+0.1% [−0.7, +1.0]** over 21 pairs — no movement. The step spends ~418ns in `exec`; one SipHash probe beside it is noise. Also perturbed the `map-set-heavy` sentinel to +3.0% with no connecting mechanism |
| plan M7.1 RegExp fast-path protectors | **PREMISE REFUTED (B68)** | ablating `regexp_exec_fast_ok` to `true` saved ~7% of the exec/test phase (6ms of 209, 22ms of 290). The plan hedged this correctly — "after telemetry proves the fixed gate is material". It is not |
| plan M2.4 `array_length_nonwritable` → `SlotSet` | **OPEN** | B66 identified it as the real fix for that probe. Note B68 refuted the sibling conversion, so measure before believing this one |
| **B102 B95 shipped a 19x pathology the benchmarks could not see** | **FIXED; a third sampled pin kind** | B95 admitted dense-Array `GetIndex` to the DOUBLE tier on `is_arr_pin(k)`, which matches `ARR_PIN_KIND` — **any** dense array, including one of OBJECTS. The element's dst then takes a numeric home, `live_in_regs` entry-loads it, the load sees the previous iteration's object, and the region `entry_bail`s on EVERY OSR entry, self-evicts, and displaces the memory compile that was working: **124ms -> 2349ms, 19x, running 100% interpreted**. Found from a CONTROL micro that was SLOWER than the thing it controlled for (property reads *removed*: 509ms -> 2047ms). The fix is a THIRD kind, not a narrower one — restricting to `ARR_INT_PIN_KIND` killed the pathology but cost `sparse-array-v2` **+6.2% [+0.9, +13.9]** by excluding arrays of DOUBLES, which the double tier hosts fine. `ARR_NUM_PIN_KIND` samples all-NUMBER over the same bounded 64-head/64-stride walk and sits between the two. Suite A/B vs the unsampled build, 21 pairs: **+0.64% [-1.84, +2.66]** — neutral, with `sparse-array-v2`'s regression gone. **All 13 benches stayed byte-identical and the gate was green through the whole pathology**; two of this session's three real defects (this and B97's flush bug) were invisible to the suite and both surfaced as "a number that cannot be right". `ZIPP_ARR_PIN_LOOSE=1` |
| **B107 the inline-cache probe was written out four times** | **FACTORED, NEUTRAL; two latent hazards closed** | `GetProp`/`SetProp` in `region_mem.rs` and again in `proto_mem.rs` emitted byte-identical 8-way probes — ~140 lines of dynasm with the entry layout as literal displacements and a literal stride of 64, in four places. They did NOT stay identical: the store path's `and edx, 0x00FF_FFFF`, which masks the hop count out of `slot_nhops`, was once absent from one — a wild WRITE at `vals + nhops*2^24*8`, not a wrong read. Now one `emit_ic_probe`, plus `assert!(size_of::<IcEntry>() == JIT_IC_STRIDE)` (raising `JIT_IC_MAX_HOPS` to 6 would silently make every probe read each way from the middle of the previous one) and a corrected layout comment that was wrong on all three of stride, hop offsets and sentinel. **The dead `jmp` was not dead**: deleting the `jmp => miss` that sat immediately before `=> miss` cost `property-ic-shapes` **+1.4% [+1.1, +1.8]**; restoring it made the emitted stream byte-identical and the row returned to **+0.0% [-0.1, +0.2]**. Five bytes of probe-loop ALIGNMENT, kept and renamed `PROBE_ALIGN_PAD` — which also makes neutrality provable rather than statistical, since the JIT now emits the same bytes it did before. 8 tests drive all four probes through thrash, mid-loop shape change, freeze, delete, a PROP_VIA_IC setter and a Tier-C chain call, identical to node and to the pre-refactor binary in four modes. **Step 1 of the shape-keyed IC; step 2's real cost is that there is NO flat shape array** — a version is one instruction only because `Heap::versions` is index-parallel, while a shape lives inside `ObjMap`. Folding the metadata refresh into `bump_version` makes all 35 sites correct by construction (realloc ⇒ bump is already a soundness invariant), but a descriptor-only change alters a shape WITHOUT bumping — so a shape-keyed hit would read a stale guard. That is why WP-1A is a prerequisite |
| **B106 the argument-`Vec` work package was mostly already done** | **LANDED, NEUTRAL AS PREDICTED** | Plan WP-1D asks for inline argument buffers on generic call paths; reading the tree first found seven of them already there — `setup_call` copies register-to-register, `try_builtin_method` gathers into `[Value; 8]`, `run_method_inline` into `[Value; 24]`, `super.m()` skips `call_value`, `f.call(...)` slices, callbacks pass stack literals, `arr.push` has its own lane — and B27 had already added `smallvec` once, measured **102 vs 100ns**, and reverted it with the dependency. What was left is `with_argv` at `eval_math`, the namespace-native-as-method arm and its JIT twin, and the bound-call concat (which allocated TWICE). **Predicted flat before measuring**: B104 had just priced an alloc/free pair at ~3ns, and `Math.imul` in a compiled region never reaches `eval_math` — `emit_math_op` emits it natively, so the Vec was on the INTERPRETED path only. Measured with B107: geomean **1.0011x [-0.19%, +0.38%]**. `TailCall` left alone: `try_tail_reuse` truncates `self.regs` before the values are written back |
| **B132 wave 19: the three rows nobody had ever decomposed were 60% of the gap — class-prototype-hot reaches node parity, sparse-array-v2 halves** | **LANDED — headline ten 1.1603x -> 1.1002x projected PGO (paired 0.9482 [21 reps, all 13 rows] against a scratch build of 0233ca3), exactly the ~1.10 the scouts projected; class-prototype-hot 1.264 -> 1.0025 (NODE PARITY), sparse-array 1.3785 -> 1.1393, sparse-array-v2 −46.3% (816 -> 438ms); 672,000-program fuzzer soak on fresh seeds: 0 nondeterministic, 1 divergence and it is pre-existing; 182/182 byte cells; 1,115 zipp-vm tests** | The wave started from arithmetic, not from a list of ideas. Node parity needs 1.48 of summed ln(ratio) removed, and the five worst rows hold 1.46 of it — so parity is not reachable by any stack of small wins, it means taking regex, polymorphic-objects, sparse-array, class-prototype-hot and parse-large-js ALL to ~1.0. Then the finding that set the slate: **`polymorphic-objects` and `class-prototype-hot` had ZERO decomposition mentions in this entire ledger, and `sparse-array` had one — despite appearing 104, 73 and 82 times as ABLATION rows confirming 'no regression'.** Ten waves went into regex, parse and typedarray; the three rows nobody had ever taken apart were 60% of what remained. Four scouts decomposed them. Three returned an honest NO on parity for their row, and every one of them refuted more than it proposed. **`polymorphic-objects` IS NOT A POLYMORPHIC-PROPERTY BENCHMARK** — its own header comment says it is, and it is wrong. 78% of zipp's time is a `delete`-and-rebuild loop nobody had named; the polymorphic-read prologue the row advertises is 11%. The decisive experiment, verified independently by the implementer before building: replace the 30-delete loop with an equal-count READ loop, change nothing else, and zipp measures 245ms against node's 243ms — **parity**. The delete construct owns the entire dict-phase gap, in three parts. (i) `PropIndex::remove_slot` (heap.rs) ended in a full linear scan of the ENTIRE hash table on every delete, to renumber shifted slots — and the cost tracks table CAPACITY, not shift distance (K=48 -> 104ns, K=384 -> 350ns; deleting front-first vs back-first is identical, which refutes the Vec-compaction hypothesis). (ii) `Vm::delete_prop` is a ~280-line unguarded spec waterfall — proxies, global bindings, arr_props, arguments, TypedArray indices, String exotics, RegExp lastIndex, class statics, sealed/frozen — with no fast path for the ordinary case; deleting a key that does NOT EXIST on a 60-property object still cost 41ns of pure waterfall. (iii) `region_mem.rs` had NO Delete arm at all, so the loop was permanently JIT-BLACKLISTED on one opcode. Fixed as a PropIndex split into parallel tags/slots arrays that lets the renumber vectorize (28.3ns -> 7.4ns at cap 128; 104 -> 11.2ns at cap 512), a conjunction-of-negative-guards fast path, and a MEM Delete arm. Measured −8.2% on the row — **the three mechanisms overlap and are worth about half the projection**, which the ledger records rather than the sum. Nine hypotheses were refuted with measurements first, including two that would each have been a plausible wave: IC capacity (36 GetProp misses in the whole run), the wave-14 thrash gate (identical timings on and off), GC (14.2ms of 449), B77 layout (dict GETS are 15ms FASTER than node), and a branchless renumber (MEASURED WORSE, 27.0 -> 30.6ns). **`class-prototype-hot` REACHES NODE PARITY** and was the wave's cleanest win. The scout found nothing structural in the way — 99.9% jit-mem, ZERO GC collections, 8 IC misses in the whole run, and the method/accessor inline already engaging perfectly — so the entire 1.26x was one sentence: **the only emitter that can host the method-inline machinery is the boxed one**, and every intermediate in every hot method body went through a memory slot with a NaN-box tag test. MI-LANE gives `MethodInlineShape` the typed lane `LeafInlinePlan` got in B126, extending it with two ops: a baked `[vals_ptr + slot*8]` field load with a tag guard (representation chosen from the slot's live value at plan time), and a `SuperGuard` that flattens the super body inline as ONE straight-line body rather than recursing the builder (which would alias two functions' register namespaces). v1 gates fail-closed: effect-free bodies only, so every bail stays a pure prefix. Measured **−20.4% [−21.2, −19.7], the row 1.2640 -> 1.0025 — parity with node, and faster than bun (362ms)**. Six arms lane; the two pass-through getters correctly decline `mi-nothing-to-unbox`. The engagement proof is stronger than a CI: `mi_lanes=6` on this row and **0 on every other row**, so the switch is byte-identical elsewhere by construction. **B126's critical-miscompile hazard was honoured explicitly**: the lane adds no new immediate template, but admitting field loads and super results feeds the EXISTING ones operand shapes a leaf body never produced, so the implementer ran three mutations — including B126's exact escaped bug — and reported that one of the three (a SuperGuard written with the boxed emitter's registers) was NOT caught by its first test set, then closed it. That is the report this campaign wants. **`sparse-array` −17.1% and `sparse-array-v2` −46.3% (816 -> 438ms)**, from five mechanisms sharing one insight: an absent index, a hole, an `in` probe and a for-in step on an array with a sparse overlay were each taking a builtin walk or a receiver-level refusal where one alloc-free probe of the side table IS the answer. The scout split the verdict honestly and it held — COLD parity (the metric the headline scores) is roughly reachable, COMPUTE parity is not, because the last ~25ms is MEM-tier per-op overhead. Note the row is only ~108ms and the harness metric includes process launch, so the implementer established startup-subtracted compute times for both engines before anything else. **`sparse-array-v2` is now 3.76x -> ~2.0x**, moving the second-worst diagnostic row further than any previous wave moved any diagnostic row. **`regex-log-scan` is the wave's honest miss: −2.1% against a needed −6.5%**, and the lane is recorded PARTIAL. Its first mechanism landed (group initialization moves out of the Rust attempt loop into the emitted code, −13.3ms, two independent runs agreeing). **Its second was REFUTED BY THE IMPLEMENTER against its own map**: the premise was '/g drain allocates 4 slots per match where the identical non-global exec loop allocates ZERO — the elision exists, extend it'. There is no elision. `regexp_build_result` allocates unconditionally on BOTH paths; the apparent difference was a GC-COLLECTION-FREQUENCY ARTIFACT — the compiled non-global loop never reaches a nursery trigger during the bench, so its garbage is never swept and never counted. Proved directly by scaling that loop to 3.0M results with a tail allocation burst so a collection actually observes the heap: 4.07 slots/result, identical to the /g path's 4.00. A mechanism priced at −15/−22ms did not exist. **This is the third scout premise in six waves to dissolve under an implementer's verification, and the reason the standing rule is verify-then-build.** **Verification.** The gate did not take the lanes' word on any of it. It rebuilt baseline `0233ca3` in scratch for a true paired A/B; it soaked **672,000 unique programs on fresh seeds across 38 modes** (0 nondeterministic; the single tier divergence reproduces identically on the baseline binary and carries all three ingredients of the documented negative-modulo-index family); and because two lanes changed SPEC-VISIBLE paths it wrote its OWN semantic matrices rather than trusting the lanes' — 426 lines for `delete` (non-configurable data and accessor properties, sealed/frozen/preventExtensions, five Proxy trap shapes, array and TypedArray indices, String exotics, RegExp lastIndex, globalThis bindings, `arguments`, 26 key spellings, symbols, 80-key churn past the PropIndex threshold, sloppy AND strict, unqualified-binding deletes) and 345 lines for holes (`in`, hasOwnProperty, Reflect.has, gOPD, for-in, Object.keys/values/entries, every hole-skipping array method, prototype-chain fallback, a side-effecting proto getter, Array.prototype re-prototyping) — plus 44,000 randomized delete programs and 1,680 randomized hole programs against node. Byte-identical everywhere, in all 14 latch configurations and under `ZIPP_JIT_THRESHOLD=1` and the debug build. It also correctly identified the one flaky regress test as PRE-EXISTING by finding the identical failure, same file and line, in wave 18's own gate log. `bench/wave19_*` |
| **B131 wave 18: one dominance predicate closes the conditional-def defect — and with it the fuzzer's nondeterminism and the −11.8% that was blocked on it** | **LANDED — bundle vs `f0f3fd9` 0.9870x [0.984, 0.989] headline / 0.9902x [0.988, 0.992] all-13; parse-large-js −10.9% [−11.6, −10.3]; `bench/object.js` 5.97x FASTER; NO row regressing (the only two CIs excluding zero are both improvements); 400,000-program soak on 16 unused seeds x 37 modes: 1 divergence, pre-existing and not default-visible, 0 nondeterministic; test262 THR1 IDENTICAL (6); 91/91 byte cells** | Three lanes, and TWO OF THEM TURNED OUT TO BE THE SAME BUG. **The defect**: `shareable()` read `first_seen[r] == true` — 'the first OCCURRENCE inside the region is a def' — as 'a def DOMINATES every use', so a local defined only inside an `if` dropped out of `live_in_regs`, whose stated invariant is 'every flushed home is entry-loaded', and 39 of 40 iterations added an unfilled home. **The fix is four non-comment lines**: B129's backward-liveness walk already computed the fixpoint, so it now returns `entry_live` (= `live_in[s]`) alongside its spans, and ONE closure `live_in(r) = first_seen == Some(false) || entry_live.contains(r)` answers BOTH consumers — `range()` and `shareable()`, and through them `live_in_regs`, `write_through` and the seg-split. Deliberately a UNION with the old flag rather than a replacement (a textual test and a dataflow test, neither strictly containing the other under an under-modelled use), and it FAILS CLOSED three ways: an unconverged walk and a `cold` entry ip both report every touched register live-in. Verified from the bytecode before being written, not taken on the briefing's word. **The audit that came with it is the durable part**: all EIGHT `first_seen` consumers classified (the DCE term is redundant — `used` already implies it; constant hoisting and pinned-`.length` hoisting are guarded by `runs_every_iteration`; the glob-range remat carries its own genuine dominance proof, now commented as the THIRD hand-written one with a note that folding it into the predicate would remat MORE and is a perf change to measure), plus three siblings reasoning the same way (`strict_entry_globs` conservative; `narrow_globs` a real per-load proof that correctly refuses this very shape; B94's `split_home_provably_safe` a must-def fixpoint that is the exact dual of what was added). **THE FUZZER'S NONDETERMINISM WAS THIS DEFECT WEARING ITS DANGEROUS FACE.** B130's gate found two generated programs answering differently across runs of the SAME binary and flagged it as its own investigation. It was not a third defect: in both, the conditional-only def meant the home was never filled at OSR entry, and **an unfilled home does not hold a wrong CONSTANT — it holds address-derived leftovers, which differ run to run.** Every previous finding in this fuzzer's history was a stable wrong answer because the garbage it read happened to be stable; this is what the same class looks like when it is not. Categories (b) generator and (c) harness were RULED OUT rather than assumed — the generator bans every run-varying construct and a lint enforces it, and both programs reproduce from a bare `zipp.exe js file.js` outside the harness entirely. The instrument was hardened anyway, because the next one will not be free: a candidate divergence is now re-run and a program that disagrees with ITSELF is labelled NONDETERMINISTIC instead of reported as a tier divergence. Calibrated honestly — at 8 self-runs the scan caught only 1 of the 2 known cases, at 16 it caught both, so the classifier spends 6 runs on each of two modes. The honest measure of the hardening: **2 of B130's 149 triaged divergences would have been auto-classified** — a small number, and the right one to report, because those two were poisoning the triage of the other 147. **THE SROA REGRESSION WAS HALF A STORY, AND THE OTHER HALF WAS LATENT UNSOUNDNESS.** B130 made `instr_uses` exhaustive and `bench/object.js` went 4.33x SLOWER, because `rewrite_for_field_promotion` neutralises an object-ref `LoadGlobal` to `LoadInt 0` and relies on DCE to drop the register — and DCE's guard `!read_outside.contains(r)` had only been true while the operand table was blind. The lane confirmed that, then found the fact the briefing did not have: **the neutralisation was only ever SAFE because the entry guard never let the region run.** Had DCE ever passed, `flush_exit` would have written `0` over the object into a slot the interpreter reads back at the very heap op a deopt resumes on. So B130 did not create a regression so much as expose an optimization that was living off a bug in both directions. Root cause is the REWRITE, not the liveness (`read_outside` and the DCE guard are correct and untouched): it was claiming 'r stays dead/unread', a statement about the WHOLE function that a rewrite removing only region-local consumers has no standing to make. Fixed by using the fact the engine already states once — `RegionPlan::ta_recv_regs`, 'an object-valued register with no numeric home'. **Result: `bench/object.js` 253 → 42ms at 100x scale, a 5.97x SPEEDUP over `f0f3fd9`**, and structurally confirmed, which is the stronger evidence: the old JITLOG read `SROA region compiled` → 64x `deopt at ip 11` → `EVICTED` → `MEM region compiled`; it is now one line, `SROA region fn0 [11,35] fields=3 -> compiled`, zero deopts. The sibling audit notes B130 made `read_outside` LARGER, which is the conservative direction for every consumer but the one that bit SROA. **THE −11.8% IS CASHED.** B130 built `gpr_wt_share` and had to land it DARK because it made the conditional-def defect reachable on programs that previously avoided it; its gate blocked the default-on flip and was right to. With the defect fixed the flip is now default-on (`ZIPP_NO_GPR_WT_SHARE=1` as the off-switch), **on evidence rather than faith**: the two programs B130's gate named as regressing were re-run 12 fresh processes per cell and answer correctly by default; the gate's own 400,000-program soak carries BOTH switch positions in every sweep; and the mechanism pin now reads a DEFAULT child's JITLOG. Measured on the default build: **parse-large-js 412 → 366ms, −10.9%**, reproducing B130's −11.8% [−12.2, −11.3] switch-isolated figure. **NON-VACUITY, twice over**: reverting ONLY the two-line `live_in` hunk in a scratch worktree brings back exactly the predicted wrong answers — 74 on the xmm INT tier, 42 under `ZIPP_NO_GPR_HOMES=1`, the two values the source comment predicts — and 5 of 10 new `conditional_def_live_in` tests plus both un-ignored fuzzer specs fail on that build. `KNOWN_OPEN` is now EMPTY and its comment says a failing index is a NEW divergence. **The one soak divergence is pre-existing and not default-visible**: it reproduces byte-identically at `f0f3fd9` with no lane's work applied, only under `ZIPP_JIT_THRESHOLD=1`, and ingredient-testing places it in the already-documented negative-modulo-index family (`h % 16` produces negative indices that create negative-index properties on a dense array mid-loop; `h & 15` fixes it). **Open, honestly**: a second nondeterminism in `crates/regress-fork`'s `rx_acqgate_threshold_and_streams`, a process-global force race its own source comment documents, untouched by any lane and handed off rather than quietly absorbed. `bench/wave18_*`, `w18_gprwtshare_default_2026-08-22.json`, `w18_condef_fix_cost_2026-08-22.json` |
| **B130 wave 17: the operand table was blind to 185 of 221 opcodes — 23 wrong-answer shapes, one root cause; the fuzzer reaches where it admitted it could not, and finds another** | **LANDED — bundle vs `0ade520` 1.0030x [1.000, 1.005] all-13 / 1.0034x headline: the correctness fixes COST +0.30%, measured against a scratch-built baseline rather than assumed; test262 THR1 sweep IDENTICAL (6); 1,040 zipp-vm tests, 13/13 rows byte-identical vs node. One perf mechanism worth −11.8% on parse-large-js LANDED DARK, blocked by a defect it exposes** | **THE `typeof` BUG WAS THE TIP OF A 23-SHAPE CLASS.** B129 left a root-caused-but-unfixed critical: `typeof x` after a loop returning `"number"` for an `undefined` variable, because `read_outside` collects post-region uses via `instr_uses`, whose catch-all `_ => vec![]` silently declares a forgotten opcode to use nothing. The lane reproduced and re-checked the chain against the source before touching it (B127 and B129 both had briefings whose framing was wrong), then fixed the CLASS rather than the instance: **`instr_uses` is now EXHAUSTIVE — 221 explicit arms, zero `_` arms — so adding an opcode without declaring its operands is a BUILD ERROR.** The old table named 36 variants; **the other 185 fell through the catch-all.** The audit that followed found the defect was never about `typeof`: **23 of 37 tested shapes answered WRONG at HEAD** — `TypeOf`, `TypeOfIs`, `ToNum`, `BitNot`, `Not`, `ToStr`, `IsArray`, `JsonParse`, `JsonStringify`, `LooseEq`, `LooseNe`, `Pow`, `MathSpread`, `InitDataProp`, `ObjectSpread`, `DeleteProp`, `DeleteIndex`, `HasProp`, `InstanceOf`, `ObjectKeys`, `ToObject`, `IterPrime`, `Throw` — and two of them were not wrong values but SPURIOUS EXCEPTIONS, `"a" in x` and `Math.max(...x)` throwing TypeErrors where node returns a value, because the flushed home had turned an object into a number. The gate confirmed all 23 independently by reverting only that hunk. Conventions are stated ONCE on the function (read-modify-write fields count; `arg_base`+`argc` windows expand, with `DecElem`/`DecClass` reading 2*argc because they take (decorator, receiver) PAIRS — verified in dispatch.rs, a hand-written argc window would have been WRONG; the `super`-* family and `FieldInit` read register 0 as `this`, which no operand field names), ambiguous ops were decided from the interpreter rather than field names, and the one read a signature cannot express (`MakeClosure` capturing cells listed in the CALLEE's proto) is documented and attributed to the `MakeCell` that boxes each register. A test fails if `_ =>` ever reappears inside the function, so the enforcement cannot be removed with one line. Four comments elsewhere in plan_region.rs that stated the now-false fact in other words were corrected rather than left to drift into a fifth copy. **HONEST COST**: the bundle is **+0.30% [+0.04, +0.53]** — small, real, and the price of correctness. One named regression: `bench/object.js`, a legacy micro (not in the ten or the three), 0.89 → 3.84ms, diagnosed to the exact register and ip — SROA's `rewrite_for_field_promotion` neutralises a `LoadGlobal` and relies on DCE to drop the register, and DCE's guard is `!read_outside.contains(r)`, which is now CORRECTLY false. **An optimization had been silently depending on the bug.** Wave 18. **THE FUZZER NOW REACHES WHERE IT ADMITTED IT COULD NOT, AND FOUND ANOTHER CRITICAL THERE.** B128's coverage report named its own gaps; this wave closed the three highest-value ones and settled a fourth. Measured tier mix over 400 programs, before → after: **DOUBLE regions 25 → 76** (the gap the author called highest-value, and correctly: two of B129's five bugs lived in that emitter), **B94 split receivers 0 → 9** (never reached before, though B127 fixed a wrong answer in exactly that machinery — found by hand), **Tier A 0 → 10** (previously unloggable; the wave added the `[jit] Tier A fn{id} compiled` line the author had requested, plus fn/ip attribution on `[decline-reason]`), **post-region uses 0 → 596** and script-scope programs 0 → 66 — the axis whose absence is precisely why a live `typeof`-after-loop wrong answer had to be found by a human reading code rather than by 138,300 generated programs. `ZIPP_FUZZ_BIG` was settled: B129's gate had already found the driver silently never ran it (a bash assignment-prefix bug), and re-aimed it now runs for real. **THE NEW FINDING (the lane's success condition), minimized to two lines and reproducing at the committed `0ade520`**: `function k(n){var h=1,i=0,t=2;for(i=0;i<n;i++){if(i===3){t=7;}h=(h+t)|0;}return h;}` — node and the interpreter say 266, compiled says 74, and `ZIPP_NO_GPR_HOMES=1` says 42. **A local whose only in-region definition sits on a conditional branch loses its entry load**, and every pass that skips the branch reads its home as garbage; wrong on BOTH register emitters with DIFFERENT wrong answers, and no `ZIPP_NO_*` switch avoids it. Root-caused: `shareable()` reads `first_seen[r]` as 'a def of r dominates every use of r' when it only says 'the first OCCURRENCE inside the region is a def' — the same family as B129's mention-vs-dominance liveness bug, and the file already states that distinction at the constant-hoisting site and guards THAT consumer with `runs_every_iteration` while leaving the sharing consumer unguarded. Carried as `#[ignore]`d specs; wave 18. The gate's own fresh soak (31,000 programs, ten unused seeds, 37 modes) returned 149 divergences that ALL carry this one construct — 132 pre-existing at `0ade520`, 13 the same class seen through a switch. **It also found a NON-DETERMINISM class**: two programs return different answers across runs of the SAME binary, on `0ade520` as well — which weakens every digest comparison a tier-differential fuzzer makes and earns its own investigation. **THE PERF LANE PRICED FIRST, CORRECTED THE MAP, AND REFUTED BOTH ITS OWN RESIDUALS.** B128 named two residuals on the flattened parse mix loop. Re-measured at HEAD (B129's liveness change moved neither number): 13 homes, 11 after the shared-home re-plan. **The map's 'needs ~4 fewer homes' was WRONG — `gpr_home_map` admits r13/r14 as `inline_guards` whenever `used.len() <= pool.len()+2`, so the real threshold is 10, not 8, and every `N homes > 8 gprs` decline line where N is 9 or 10 is actually a FIT.** Recorded because anyone reading those lines literally will over-price the work. Residual A lever 1 (dedup the repeated `LoadInt`) was ALREADY CASHED — the three copies are hoisted and cost zero mapped homes. Lever 2 (copy-coalesce a `LoadGlobal`) exists as `unify_homes_with_globals` and is disabled behind `const UNIFY_HOMES: bool = false` for a documented silent wrong answer. Residual B (teach `confined` about relocated regions) is live but does NOT cash and the naive form is UNSOUND — `int_splice` appends the flat body at `code.len()`, so a relocated region has no ips after `e` and `outside_dead`'s post-region scan is VACUOUS; done properly against the origin span, `confined` is STILL false, because fn0's enclosing round-loop back edge makes the pre-region ips reachable from every exit. **What shipped instead** is one flag at two call sites: the two `share_homes` GPR re-plans now pass `admit_wt_share` where they passed `false`, on the licence the DV retry has used since B122 (the GPR emitter has been def-complete for write-through since then), so the three retry sites now AGREE instead of two silently disagreeing with the third. Homes 11 → 9, the region reaches the GPR emitter instead of paying three xmm↔gpr transfers on nine ops per iteration, and **parse-large-js measures −11.8% [−12.2, −11.3] (412 → 364ms)**. **IT LANDED DARK (`ZIPP_GPR_WT_SHARE=1`), because the gate BLOCKED it**: releasing those pins makes the conditional-def defect above reachable on programs that previously avoided it — two soak programs that `0ade520` answers correctly the wave tree answered wrong, in the DEFAULT configuration, and 62 of 62 mode-cells are restored by turning it off. The mechanism is sound; the defect it exposes is not its own. Fix the conditional-def entry load, re-verify with a soak, and making this default-on is a one-line change — that is wave 18's first item, and it is worth −1.26% on the headline geomean. **Process note**: the gate's verdict was `failures`, and it was right to block. Discarding the work would have thrown away a measured −11.8% for a defect it did not cause; landing it default-on would have shipped wrong answers. Dark-with-a-named-blocker is the third option, and it follows the B123 spill-slots precedent. `bench/wave17_bundle_2026-08-21.json`, `w17_wtshare_abenv_2026-08-21.json` |
| **B129 wave 16: the five wrong-answer classes reduce to THREE root causes, all three reported framings refuted; `x | 0` was destroying live booleans** | **LANDED — 138,300 fresh generated programs on ten unused seeds x 18 modes: ZERO divergences (28 before); all five `#[ignore]`d specs un-ignored and passing; `KNOWN_OPEN` now EMPTY; perf cost +0.02% geomean [−0.37, +0.20]** | Every one of B128's five defects is fixed, and not one of the three reported framings survived contact with the code — the lanes disproved their briefings rather than coding to them. **DEFECTS 1 AND 5 ARE ONE DEFECT, and it is not a flush or tier-change bug** (the eviction-to-DOUBLE trace was only how a program REACHES the broken emitter). The DOUBLE/regalloc emitter scratched `r10` = `BOOL_GPRS[2]` in its BODY at two sites. The first is the `Instr::Bitwise` arm, which materialized the B92 `i64::MIN` indefinite sentinel into r10 — so **every `|`, `&`, `^`, `<<`, `>>`, `>>>` in a DOUBLE region, including the bare `x | 0` that int-flavoured JS writes on every line, overwrote the third Bool home**; `flush_exit` then boxed it as `BOOL_TAG | i64::MIN`, whose sign bit breaks the NaN-boxing, and the interpreter read the slot back as a raw negative double — defect 1's `typeof b2 === 'number'` exactly. The second is `emit_box_to_home`, whose doc claimed 'region entry only' while regalloc's dense-Array `GetIndex` arm called it in the BODY: **the exact twin, one tier over, of the bug B127 fixed — B127 closed one USE and left the CLASS open.** Defect 5 needs no separate cause: `ZIPP_NO_FUSED_CMPJUMP=1` is a BYTECODE re-plan (an unfused guard becomes `Lt` + `JumpIfFalse`, one extra `Bool`), so it shifts which live value lands in `BOOL_GPRS[2]` — in defect 5's program the default has two bools and is right while the unfused spelling has three and is wrong; in defect 1's program it is the reverse, which is why that one produced a THIRD answer under the switch. Fix: three register renames to `rdx` (dead at every site, and the register B127's twin fix already chose), no logic change. **The invariant had been restated as five hand-written comments that had already drifted** — `emit_box_to_home` said 'entry only' when the body calls it — and is now stated ONCE as the register contract on `BOOL_GPRS` in `plan.rs`, with the five sites pointing at it; the audit behind that covers every helper reachable from a region prologue or body. **DEFECTS 2 AND 4 ARE ALSO ONE DEFECT, and it is neither OSR entry accounting nor the fused-compare emitter** (both framings disproven directly: the loop does NOT drop iterations, and the compare emitter is innocent — unfusing merely re-plans the allocation, the same mechanism the bool lane found on its own side). The home-reuse allocator built live ranges from `first_ip`/`last_ip`, which record only the ips that MENTION a register. **That is a live range only for straight-line code, and a region IS a loop body**: a value defined before an inner loop and read inside it stays live across the inner back-edge, so the allocator saw the window close and re-let the home to a later value, clobbering it on the second and every later inner iteration. Proven from the actual linear-scan intervals: in defect 2, `d0`'s interval `[10,17]` took xmm9 and `LoadInt{dst:25,val:0}` — the outer `| 0` — took xmm9 at `[21,22]`, so inner iteration 2 read 0 and the addend vanished; in defect 4 the same collision made the `d1 > 100` compare read `h`, and `97 → 100 → 103 → 110 → 117` reproduces the reported 117 exactly. Fix: real backward liveness (`region_succs` + `region_live_spans`, bounded and fail-safe to whole-region spans if unconverged) WIDENING the mention windows rather than replacing them — a dataflow fact, not a special case. **DEFECT 3: the hoist was innocent; the missing store was the defect.** A pinned access's receiver register is deliberately given no numeric home (the element emitters read the receiver through the pin's own source), so its defining `LoadGlobal` was emitted as a literal no-op in all THREE register emitters — the interpreter frame slot was never written by compiled code. That collides with the deopt contract of every pinned access ('resume AT this ip', sound for side effects but it re-reads `regs[obj]`), and `flush_exit` cannot repair a register that has no home and appears in none of its lists. So the slot held whatever the interpreter last left there — which is why all three ingredients are load-bearing: a GLOBAL receiver (a parameter's slot is a live-in the interpreter already filled), and a COLD block (an unconditional read leaves the slot correct during the 8 interpreted iterations before compilation). Fix: the receiver `LoadGlobal` now stores the object to its frame slot via one shared `emit_recv_slot_store`, merged with the B127 split-receiver arm whose body was already byte-for-byte the same code — a fourth hand-maintained copy avoided. Two alternative repairs were considered and rejected on soundness, not taste (resuming at the `LoadGlobal` ip would replay side effects of ops in between; materializing in the prologue would break `var x; for(..){ if (rare) { x = g; ... } } return typeof x`). **VERIFICATION.** A fresh soak on the fixed tree: **138,300 programs, ten seeds never used by any lane, all 18 modes, ~2.49M executions — ZERO divergences**, against 28 in the same classifier over the pre-fix transcripts. A SECOND ORACLE guards the blind spot a tier-differential soak structurally cannot see (all tiers agreeing on a wrong answer): 19,000 generated programs run under default zipp and compared to node, 0 mismatches. Non-vacuity was established twice — every one of six repro faces reproduces on an `ee14c71` baseline binary in EXACTLY the mode set its defect JSON names with the exact wrong values, and isolated lane reverts in scratch copies bring back precisely the defects that lane owned. `ZIPP_NO_FUSED_CMPJUMP=1` is now a TRUE pure fallback, verified three ways (13/13 byte-identical rows, 0 divergences across 138,300 programs in that mode, and defect 5's repro correct in all 18 modes) — **which retroactively settles B128's methodological worry: one-binary ablations taken through that switch are trustworthy again, and only measurements taken through it BEFORE this wave were measured against wrong answers.** **PERF COST, measured against a scratch build of `ee14c71` rather than asserted: +0.02% geomean [−0.37, +0.20]** over 13 rows at 15 reps, with a 25-rep confirmation run separating the real movements from noise (map-set's +2.6% did not reproduce) and a 31-rep attribution run assigning 100% of the only reproducible regression — typedarray-math +1.4% [+0.5, +2.0] — to the cold lane's two L1-resident `mov`s per pinned access. parse-large-js −0.6% and sparse-array-v2 −1.5% are real gains from the liveness widening. **INSTRUMENT DEFECT FOUND BY THE GATE**: the soak driver's `${BIG:+ZIPP_FUZZ_BIG=1} "$EXE"` form means bash resolves the assignment prefix before expansion, so the expansion becomes the command name and **every `--big` soak the lanes ran died instantly** — those runs were vacuous. The gate's own big soaks used a corrected driver. Recorded because a fuzzing campaign that silently runs nothing is worse than one that runs less. **NEW CRITICAL FOUND, ROOT-CAUSED, DELIBERATELY NOT FIXED** (out of the finding lane's ownership, and it earns its own change): **`typeof x` after a loop returns `"number"` for a variable that is `undefined`.** `read_outside` scans post-region uses via `instr_uses`, which ends in `_ => vec![]` with `TypeOf`/`TypeOfIs` ABSENT — so a register whose only later use is `typeof x` looks dead-after-region, becomes `shareable`, and is dropped from `live_in_regs` (whose doc states the invariant 'every flushed home is entry-loaded') while remaining in `num_regs`; `flush_exit` then writes a home nothing ever filled into its frame slot. Nine-line repro, no arrays, no deopt, wrong at HEAD. Wave 17. `crates/zipp-vm/tests/bool_home_clobber.rs` (a generated matrix over 1–4 live bools x 13 body ops, on both the DOUBLE and INT tiers, asserted against `node -e`), plus the five un-ignored fuzzer specs. Gate: 1024 zipp-vm + 84 zipp-regress + 1 zipp-cli tests, 78/78 byte cells, debug-assert clean. |
| **B128 wave 15: the leaf-splice reaches the INT tier; and a generative tier-differential fuzzer finds five live wrong-answer classes on its first soak** | **LANDED — bundle 0.9924x [0.990, 0.995] headline / 0.9942x [0.992, 0.997] all-13; parse-large-js −9.6% [−10.1, −9.0] (454 → 411ms); test262 THR1 sweep IDENTICAL (6); no row regressing outside its CI. FIVE PRE-EXISTING DEFECT CLASSES FOUND AND SPEC'd, none introduced by this wave** | **Splice-aware INT admission (`ZIPP_NO_INT_SPLICE`)** — B127 scouted this as HALF A of the parse mix-loop promotion, measured that BOTH halves are required, landed HALF B (multi-receiver split, which measured null alone precisely because this half was missing), and correctly declined to half-build it. The blocker: three calls in the mix loop are ALREADY PROVEN to splice (JITLOG shows INLINE-ELIGIBLE + slot_guard + splice-lite masks for each), but `codegen/mod.rs:1908` called `compile_region_int` WITHOUT the leaf plan, so the INT admission walk saw raw `Instr::Call` and rejected `region_is_int=false`. The mechanism threads the leaf plan into the INT path, flattens proven-splice bodies into a virtual body (callee registers offset by `reg_window`, the callee `Return` mapped to a `Move`, an ip map retained for deopt resume), runs `int_unadmitted_ips` on the FLATTENED body, and emits the callee identity guard ONCE at region entry instead of per call site — strictly stronger than the per-site guard it replaces, and backed by B125's slot-generation proof. Declines fail-closed (a callee op INT rejects, an upvalue read needing the `jit_cell_get` FFI call, `argc != param_count`). JITLOG now reads `INT splice [274,302]: 3 call(s) flattened, 1 entry guard(s), 44 ops` where it read `INT decline` plus three `[int-reject] Call` lines. **HONEST PRICE — SHORT OF THE MAP, AND DIAGNOSED**: it lands at **−42ms**, not the mapped −65 (range −55/−90). The map's −65 came from a vehicle variant that hoisted the loop to PARAMETERS, removing the global loads AND the recycled pinned receivers; what ships keeps both. The map's own matched micro for the SHIPPED shape predicted 64–72ms for the phase and the phase measures 65ms — so the mechanism hits the map's number for its actual shape, and the −65 was the vehicle, not the end state. Phase timers: **mix 107 → 65ms, which now BEATS node's 69ms**; tokenize and parse unchanged. Two named residuals for a future wave: the flattened region plans 12–13 numeric homes against an 8-GPR pool, so it runs on the xmm integer emitter paying transfers on 9 ops x 3.6M iterations (copy-coalescing a `LoadGlobal` into the global's own home and deduping a repeated `LoadInt` would free the ~4 homes needed); and the flatten relocates the region to the end of the code array with exit targets still pointing at the original lower ips, so B126's `outside_dead` `confined` predicate is false for every flattened region — a conservative loss, never a wrong answer. The map's hope that markdown and json would also fire does NOT hold: their in-region calls decline as not-monomorphic or upvalue-reading. **THE TIER-DIFFERENTIAL FUZZER — the wave's real deliverable.** B127 established that correctness at a TIER depends on REGISTER ALLOCATION, which depends on incidental properties of a program (how many Bools are live, how many constants got hoisted, which pins exist) — a combinatorial space that 95,936 test262 executions, 13 benchmarks, 900+ unit tests and nine hand-written `jit_tier_parity` cases all failed to cover for a month. `crates/zipp-vm/tests/jit_tier_fuzz.rs` (3,006 lines) generates self-checking programs over that space — loop nest depth and labels, guard SPELLING (`a<b` vs `!(a>=b)` compile to different ops and reach different tiers), 1–4 live Bool temps, 0–3 hoisted compare constants, element reads across dense/holey/Int32/Float64/Uint8/string forms, break/continue/labelled-continue/early-return, splice-candidate and non-leaf calls, upvalue reads, and planted deopt triggers — then differentials each program across node, `ZIPP_NOJIT=1`, several `ZIPP_JIT_THRESHOLD` values (thresholds change WHICH iterations run compiled; B127's bug tracked the OSR point exactly) and every tier-forcing switch, and SHRINKS any divergence on the generator's own IR so minimal cases come out at 10–15 lines. A 500-program x 7-mode slice runs in the normal suite in ~4s, deterministic (486 distinct digests, zero degenerate rows); the soak is seeded and re-runnable. **NON-VACUITY PROVEN, not asserted**: the gate copied the tree to scratch, reverted ONLY B127's `ARR_INT_PIN_KIND` hunk back to the `r10` form, rebuilt, and the CI slice FAILED on 5 of 500 programs — surfacing in the `nogprhomes` mode, which is exactly where that emitter lives. **FIRST SOAK: ~29,300 programs, ~420,000 executions, 34 divergences reducing to FIVE defect classes, ALL PRE-EXISTING AT HEAD `6ed29ac`** and all now carried as `#[ignore]`d failing specs: (1) CRITICAL — a live-out Bool local reads back as **NaN** (a JS variable holding `false` observably contains a Number); it is always the THIRD bool, `BOOL_GPRS[2]` = **r10**, so B127's fix closed one USE of that register and not the CLASS; the wrong answer starts exactly at the call where an evicted INT region is re-compiled as DOUBLE. (2) CRITICAL — a compiled nested loop silently runs FEWER inner iterations than the interpreter, the deficit tracking WHEN the region compiled (one addend short at default, eight at threshold 1); it needs the invariant COMPUTED in the outer loop, not written as a literal. (3) CRITICAL — a cold out-of-range element read throws a spurious `TypeError` instead of yielding `undefined`, in EVERY compiled mode with no switch avoiding it; the receiver must be a global for it to fire. (4) MAJOR — a fused f64 compare in a nested loop takes the wrong branch (no bools, no arrays, no globals involved). (5) MAJOR — **`ZIPP_NO_FUSED_CMPJUMP=1` is not a pure fallback**: it answers WRONG on programs the default gets right, and it is the single most common soak signature (12 of 28). That last one has a methodological sting this file must record: **this campaign has run one-binary A/B ablations THROUGH that switch, so any 'old side' measured with it was measured against wrong answers.** **Process note**: the two lanes were paired deliberately — one widens what reaches the INT emitter, the other is the instrument that makes widening safe — and the pairing worked as intended: the fuzzer's soak against the widened tree found the one divergence in 3,000 that also reproduces at HEAD under `ZIPP_NO_INT_SPLICE=1`, proving it belonged to the pre-existing class rather than the new mechanism. The INT lane independently ran 6,720 splice-shaped program-mode executions (12 seeds x 80 kernels x 7 modes), every one byte-identical to node, and pinned its own mechanism with a switch-controlled test262 comparison (fail set byte-identical ON vs OFF) rather than against a stale baseline. Gate: 991 zipp-vm tests, 65/65 byte cells, 1,302 wave-14 repro comparisons with zero value disagreements, debug-assert clean on four rows. `bench/wave15_bundle_2026-08-21.json`, `w15_intsplice_abenv_2026-08-21.json` |
| **B127 wave 14: the INT tier was silently returning wrong answers on ordinary loops — a correctness wave, plus a 28% cut on the suite's worst ratio** | **LANDED — bundle 0.9982x [0.996, 1.001] headline (NULL, CI spans zero) / 0.8961x [0.895, 0.898] diagnostic / 0.9736x [0.972, 0.976] all-13; property-ic-shapes −28.3% [−28.5, −28.2]; TWO silent-wrong-answer classes fixed; test262 THR1 sweep IDENTICAL (6)** | Scouting found a defect that outranked every perf item on the slate. **THE INT EMITTER RETURNED SILENTLY WRONG ANSWERS (`69ac0a4`, 2026-07-26 — shipped through six waves and every capture since B121).** A scout reduced it to 22 lines: a nested scan with `break` over a dense int array printed `m=2` where node and `ZIPP_NOJIT=1` both print `m=28571`; no throw, no deopt log. The scout's hypothesis — that it needed three simultaneous conditions (an outer `JumpIfNotLt` exiting the region, an inner one staying in-region, and a pinned-array `GetIndex`) — was a RED HERRING, and the implementer refuted it rather than coding to it. **True defect**: the `ARR_INT_PIN_KIND` arm of `GetIndex` in the xmm INT emitter used `r10` as scratch for its NaN-box tag check, and `r10` is `BOOL_GPRS[2]` — a register the planner parks loop-invariant values in (Bool register homes, and `gpr_const` mirrors of hoisted compare constants that `emit_icmp_flags` reads straight out of the register inside the loop body). So EVERY dense-Array element read destroyed whatever the plan had parked there, for the rest of the region and across the back-edge. The trigger is not control flow at all — it is simply 'the plan parked something in r10', which in the reducer happens because two `===` stop the Bool bump allocator at `BOOL_GPRS[1]` and push the first constant mirror onto `BOOL_GPRS[2]`. The invariant was already written down THREE times (the `Mod` arm's comment, the prologue's bool-load ordering, `emit_bool_entry_load`'s doc) and the INT-GPR twin of this very arm already scratched `rdx` with a comment explaining why; one site violated it. Fix: scratch `rdx` (dead after the load) and compare `edx` — byte-for-byte the choice the GPR emitter had already made. **Blast radius far beyond the reducer**: a FLAT SINGLE LOOP with no nesting and no break was wrong (51427 vs node's 77142), as were two sequential loops (462843 vs 694278), a three-level nest, a `while`-head shape (4 vs 1713), the deopt paths themselves (double/hole mid-array), and shapes with three or four live Bool temps — including one where `gpr_const` is provably empty, isolating Bool homes as an independent victim class. That the `Bitwise`-containing variants did NOT reproduce is itself confirmation: the GPR retry takes those, and the GPR arm was correct. Proof the region froze rather than mis-branched: `m` tracked the OSR point exactly (`ZIPP_JIT_THRESHOLD` 1/8/20/50/200/2000 gave m = 1/2/5/13/50/500 pre-fix, 28571 at every threshold after). The gate independently rebuilt a baseline binary at cdf064c and confirmed **15 of 21 repro shapes were silently wrong before this wave and correct after**. `tests/int_nested_fused_guard.rs` (19 tests: 12 parity cases + three generated matrices over 51 programs + a mode sweep + two JITLOG tier pins) verified non-vacuous by reverting the hunk — 17 of 18 then-present tests failed. **The second correctness class**: B126's deferred DataView defect, verified and PARTLY REFUTED before fixing (the reviewer blamed `flush_exit`, which is already correct at :849; the real defect is the write-through at regalloc.rs:813-828 not skipping the split-receiver `LoadGlobal` ip, so a recycled B94 split register's numeric half lands on the slot still holding the receiver and the deopt resumes reading it). An out-of-bounds DataView threw `TypeError: undefined is not a function` instead of `RangeError`; two further repros returned wrong NUMBERS (`NaN` and a spurious read-only-property TypeError). Fixed with plan-level hardening (a split receiver may never enter `write_through`) and a refactor folding the exclusion into `emit::wt_def_at` so an emitter cannot obtain the def without it. 4 repros x 10 modes = 40/40 node-identical; emitted code −12 bytes per split-receiver site (the removed store pair), 13/13 rows byte-identical emitted code, geomean 0.9973x — a fix with NEGATIVE code size, measured on an isolated binary rather than argued. Neither fix ships behind a switch: a bug fix must not be switchable off. **IC refill thrash-gate (`ZIPP_NO_ICGATE`)** — the wave's one real perf win, and on the row everyone had written off as architectural. The inline caches thrash: every refill evicts a way about to be needed again, so an 8-way identity cache behaves far worse than its floor. Gating refills at thrashing sites and escaping rotation drops **GetProp misses 45,917,773 → 25,957,757 and SetProp misses 12,001,065 → 6,490,229 — exactly the theoretical `(n−8)/n` floor** for an 8-way cache. Ablation: **property-ic-shapes −28.3% [−28.5, −28.2] (1306 → 937ms)**, every other row null (poly +0.2%, poly-v2 +0.5%, class +0.2%, all CIs at zero), and ICSTATS proves the null rows cannot be affected — nine of thirteen rows take literally zero IC misses. The row's ratio goes 3.90x → ~2.8x. **Multi-receiver live-range split (`ZIPP_NO_MULTI_SPLIT`)**: the plan's hard budget of ONE non-DataView element split per region lifted, with the `pin_obj` match extended. Measured **0.9996x [0.996, 1.003] — NULL**, as scouted: it is an ENABLER, not a win, and the win it enables (splice-aware INT admission, priced −65ms on parse-large-js) was NOT built this wave. Kept default-on: null with no row regressing, 14 tests including four JITLOG mechanism pins, and it is the prerequisite for that −65ms. Honest status: this wave's headline geomean is UNCHANGED. **Refutations that saved a wave**: typed-lane admission widening — the obvious follow-up to B126's biggest win — measured **ZERO across all thirteen rows**. `Neg`: 0 sites. Wider op set: 0 sites. GPR/XMM budget relaxation: 0 sites. Multi-upval: 0 sites. The lane's deliberately narrow admission rule already captures everything the suite contains, so the plan is REFUTED, not deferred. The direct Tier-C call edge was re-anchored against the post-B126 tree and re-priced DOWN to −9/−12ms (ceiling −15) on a 412ms row; it stays parked. On property-ic-shapes the scout also bounded the rest of the row honestly: the megamorphic stub-cache design (M3 step 2) has a **ceiling of −390ms** and the MEM-tier loop floor beneath it — **609ms of the 1003ms row** — is NOT available to any contained mechanism, so 'property-ic-shapes is the M3 acceptance bench' understates it: even a perfect inline-cache architecture leaves that row above 2x. **Process record**: three lanes ran in parallel with the correctness item gating the INT-widening work in its own lane (widening admission puts MORE shapes through the emitter under repair). The INT lane finished item 1 complete and reported item 2 HALF A as NOT DONE rather than half-landing it — the right call, and it declined to ship an off-switch latch for a mechanism that does not exist. Notable discipline from the gate: rather than trusting the lanes, it built a baseline binary (stash → build → snapshot → pop, with the working diff snapshotted and byte-verified after restore) to prove the repros were non-vacuous, then chased the six that passed on baseline and confirmed via three named switches that they were the DataView class. It also added three byte-gate cells the brief had not asked for, on the grounds that the required cells never reach the tier the DataView fix lives in — without them that fix would have been gated by nothing. 104/104 byte cells, 964 zipp-vm + 84 zipp-regress + 1 zipp-cli tests, debug-assert clean on four rows, GC-stress on four. `bench/wave14_bundle_2026-08-21.json`, `w14_{icgate,msplit}_abenv_2026-08-21.json` |
| **B126 wave 13: the spliced arithmetic leaves the box; the chain link sheds its dispatch; the register tier stops paying rent on stored globals — the largest wave of the campaign** | **LANDED — bundle 0.9274x [0.924, 0.931] headline / 0.9439x [0.941, 0.947] all-13; regex-log-scan −20.8% [−21.4, −20.4]; typedarray-math −33.5% [−33.9, −33.0] (the row lands AT node); markdown −4.4%, parse −3.4%, json −3.3%; test262 THR1 sweep IDENTICAL (6); no row regressing outside its CI** | The wave began with the FIRST decomposition of regex-log-scan's corpus-gen loop — the row's largest absolute gap and, after four waves of regex-pipeline work, the part nobody had measured: gen 376ms vs node 180ms, splitting into arithmetic 156ms (node 22) and string build 187ms (node 39), with zipp WINNING retention 33ms vs node's 119ms. Four suspects a reasonable engineer would have guessed were REFUTED before a line was written: GC/write-barriers (25.9ms total, 16 young slots/line), number formatting (pre-stringifying every numeric piece measured **+53ms SLOWER** — MEM table reads cost more than the conversions B124's in-place digit path already does), ropes/flattening (none exist in gen), and concat fusion itself (already banked; what remains is per-link dispatch, not rope allocation). **Typed-splice lanes (`ZIPP_NO_TYPED_SPLICE`)**: the spliced ri+mulberry body (4.45M activations/run) was emitted as ~38 BOXED MEM ops — two slot loads, tag checks, the op, a box, a slot store apiece — and no existing tier could take it (INT declines on a double LoadConst + fractional Div; DOUBLE has no Imul arm). A plan-time mini-absint types branch-free numeric leaf bodies, proves magnitude bounds FAIL-CLOSED under 2^53, assigns physical homes (r8/r9/r10/r11/rdx + xmm2-5), and the emitter runs the body register-resident: entry tag guards on params and upvals, upval writes buffered in registers and committed only at exit, every bail (tag miss, nested-callee guard, out-of-range ToInt32) jumping to the existing per-call helper as a PURE PREFIX with nothing committed. IEEE semantics preserved op-for-op in bytecode order (no algebraic div+mul fusion — pinned by a test whose fused form prints 0.7 instead of 0.7000000000000001). One mapped decline was narrowed in the build: LoadGlobal is admitted Callee-only and FUSED into the nested-call guard, because a literal reading of 'any LoadGlobal declines' would have declined the motivating body. Ablation: **regex −14.1% [−14.5, −13.7]** plus unmapped free wins on three rows nobody aimed at — json −3.3%, parse −2.5%, markdown −2.0% (numeric leaves everywhere). Isolated arithmetic micro 153 → 38ms (node 21), BEATING the map's 45–70 target. **Chain-link slimming (`ZIPP_NO_CHAIN_FAST`)**: each of ~7.2M StrConcatChain links/run paid an FFI helper whose interior did three heap lookups, a mem::replace take/put dance and an unhinted realloc ladder. A single-dispatch fast sibling serves the int and str leaf arms in place, the emitter elides the destination refetch on a same-bits-plus-heap-tag compare, and a first-link capacity hint (an emit-time scan of the pattern the chain lowering guarantees) removes the ladder. Aliasing (`a += a`), interned or rope accumulators, and effectful-toString leaves fall to the generic path; cross-link batching is explicitly NOT done because leaf evaluation interleaves with links in the bytecode. Engagement: 4.80M of 4.95M links (97%) on the fast arms. Ablation: **regex −8.8% [−9.1, −8.3]**, markdown −1.4%, json +0.4% (null). **Stored-global live-range narrowing (`ZIPP_NO_GLOB_RANGE`)**: B122 priced DV-on-GPR at ~90–110ms on typedarray-math and was ROW-REFUTED on pool pressure (13–14 homes vs a 7–9 GPR pool); B123's spill slots refuted too. The scout killed the obvious third idea as well — read-only-global demotion to baked immediates has ZERO demotable homes in the blocking regions, RETIRING B122's named follow-up. What actually blocks the pool is STORED globals holding permanent homes across a whole region when every in-region load is dominated by a same-iteration store. Narrowing those intervals (per-store boxed write-through so the slot is interpreter-exact at EVERY exit — fail-closed, chosen over the map's dominance-flush, which the planner cannot prove from one FuncProto), splitting mixed-role temps across multi-segment intervals bound to one home by a new bounded-backtracking allocator, plus three pieces the map underestimated (phantom elided-Eq reservations dropped, const rematerialization with a slot-materialized fallback, an outside-dead whole-proto scan) take the two blocking regions from 13/14 homes to 8/9. The declines are GONE and B122's DV-GPR arms finally engage. Ablation: **typedarray-math −33.2% [−33.5, −32.9] (320 → 213ms; node ~205–212 — THE ROW LANDS AT NODE)**; every other row null. B96 permanence is AMENDED, not skirted: genuinely loop-carried globals keep permanent homes. **Acquire gate (`ZIPP_NO_RX_ACQGATE`)** closes B125's named residual: the session entry probe ticked the rx-jit use counter on zero-attempt scans, compiling regexes that never ran natively. A read-only acquire_if_compiled keeps ticks on real attempts (compilation stays strictly monotone, never later than the 64th attempt); RXSTATS is bit-invariant and native+interp now sums EXACTLY to attempts — the 83 phantom ticks are gone, zero-attempt sessions 21 → 0. Ablation −0.2% [−0.7, +0.1], null as priced. **THE FIRST CRITICAL FINDING OF THE CAMPAIGN, and a refuted instrument.** Adversarial review found the typed lane lowering `Add` whose LEFT operand folds to an immediate OUTSIDE i32 range as IAddImmRev (`imm - b`) — a SUBTRACT: a body folding 1.4e9+1.4e9 returned 2799999993 instead of 2800000007. It escaped 906 green tests and a 78-cell byte-identity gate because neither the new suite nor any bench row produces a wide-immediate operand. Fixed (materialize the immediate, add reg-reg; IAddImmRev kept for Sub) with a regression matrix — both operand positions × both ops × the 2^31/2^32 boundaries × a `>>> 0` fold — whose DISCRIMINATING POWER was verified by reverting the fix and watching exactly the new tests go red; a branch-coverage audit of every other immediate-folded and reversed-operand template found no sibling. THE REVIEW INSTRUMENT WAS ALSO REFUTED: the skeptic pass mis-escaped its prompt interpolation, so all thirteen skeptics received literal template text instead of a finding and returned vacuous refutations — the same latent bug sat unnoticed in B125, where no major findings existed to expose it. Re-run with findings passed as FILES, all four majors were CONFIRMED by two independent skeptics each: (1) the chain capacity hint permanently over-allocated every retained chain-built string — two skeptics independently measured **+194MB / +90% steady-state RSS** on 1.2M short strings and isolated the cause (waste tracks hint-minus-content; benchmarks CANNOT see it because the reseat allocates a Rust Vec, invisible to GCSTATS) — FIXED with a last-link trim (fires only when ≥32B AND ≥half the buffer is slack), RSS restored to the switch-off reference exactly; (2) the designated B9-class pin test never engaged the mechanism — replaced with a shape that bails inside the narrowed window, the old shape kept and honestly renamed, both new pins mutation-tested non-vacuous; (3) slot_consts shipped untested — a nest sized so the slot-const region is the hot one; (4) a PRE-EXISTING DOUBLE-tier defect surfaced under ZIPP_NO_GLOB_RANGE=1 (an out-of-bounds DataView RangeError becomes a TypeError; regalloc's flush_exit skips split receivers and a recycled B94 register's numeric half overwrites the receiver slot) — NOT this wave's (regalloc.rs is untouched; JITLOG plans byte-identical pre/post on all 13 rows), DEFERRED with a guard test that goes red the moment a plan change pushes that shape onto DOUBLE at default. **Open bug for a future wave.** Two agreed minors fixed (stacked-borrows read order in the chain str arm; allow_hoist/RetryNoHoist gating). **Scouting refutations (no code)**: rx-native-scan-loop RE-PRICED DOWN and DECLINED — after B125's session hoist the residual decomposes to a ~24ms ceiling against 10× the emitter risk (B125's deferral had assumed a larger residual). sparse-array-v2's 606 → 631ms move between PGO captures is RETRAIN VARIANCE, not code: the one-binary bundle measures that row at +0.0% [−0.1, +0.2]; recorded, tools/pgo.sh unchanged. **Process record**: the implementation workflow was KILLED MID-RUN by a session limit (three of four lanes lost; the acquire gate complete, chain-slim left ~660 building lines with no tests, two lanes left nothing) — B124's failure mode exactly. Recovery used B124's protocol: assess the tree (it built clean), then resume with the partial lane's prompt rewritten as gap-analysis and the empty lanes started fresh. The review's first launch died on a model limit and was relaunched. Post-fix gate: 916 tests across 64 binaries, 78/78 byte-identity cells vs node across the switch matrix, debug-build assert-clean on the target rows, JITLOG plans byte-identical to the pre-fix binary on all 13 rows. DEFERRED to wave 14 with its map already written: the direct Tier-C-to-Tier-C call edge (−12/−16ms on parse), whose STALE-LEN invariant — parked native windows living above regs.len — is the highest-risk mechanism scouted to date and earns its own wave. `bench/wave13_bundle_2026-08-20.json`, `w13_{lanes,chain,globrange,acqgate}_abenv_2026-08-20.json` |
| **B125 wave 12: the matchAll pipeline sheds its per-creation and per-attempt host taxes; Tier C admits TailCall behind a depth guard; the splice guard hoists onto the global slot and measures null** | **LANDED — bundle 0.9941x [0.990, 0.996] headline / 0.9959x [0.992, 0.998] all-13; regex-log-scan −4.6% [−5.2, −4.0] with an unmapped markdown-render bonus −2.7% [−3.4, −2.3]; parse-large-js −1.4% [−1.8, −0.6]; test262 THR1 sweep IDENTICAL (6); no row regressing outside its CI (async's bundle +1.0% [+0.2, +1.3] did NOT replicate: −0.39% [−1.15, +0.66] at 21 pairs — drift, cleared)** | Four scouts priced the slate against the tree before a line was written; two candidates died in scouting and the refutations are the wave's cheapest wins. **Twin-at-create (`ZIPP_NO_TWIN_AT_CREATE`)**: the row's kv regex is used ONLY via matchAll, so its ascii twin was never built on the SOURCE — every one of 150k creations re-paid the ensure body (two compile_cps Vec builds BEFORE the cache probe, the (String,String,bool) key, a SipHash probe, an Arc clone: 136ns measured by paired micro). The creation arm now runs the ensure once on the source; matcher clones carry Some from birth; the cold slim arm stays live for the off-switch and failure carry. Ablation: **regex −2.8% [−3.3, −1.7]** — the map's −15/−22ms BEATEN (~−27ms). **rx-scan-session-hoist (`ZIPP_NO_RX_SCANSESSION`, crates/regress-fork)**: run_attempt's per-attempt TLS SCRATCH.with + RefCell borrow + groups alloc/resize + JitCtx build hoisted to a per-next_match Session; each attempt now pays a groups refill + ctx.start/skip_hint + the native call; try_borrow_mut degrades to fresh local buffers (never panics); bt-grow recomputes ctx.bt_base/bt_limit after every resize. Ablation: regex −1.7% [−2.0, −1.1] (~−16ms) — the map's −30/−50 HONESTLY RE-PRICED: the removable host share of the 21ns/attempt marginal was ~4ns, not 8–12. Named residual: the entry acquire probe ticks the JIT use counter on zero-attempt scans (wants a read-only acquire_if_compiled). **matchall-drain-batch (`ZIPP_NO_MATCHALL_BATCH`)**: one host-side scan per subject (cap 16) inside one Session, integer triples memoed on the iterator record (MatchBatch keyed by expected_li, u32 flat vec, GC-pruned with the record in both collectors), per-step publish through the SHARED result-build helpers — lastIndex written before each result, Annex-B statics recorded lazily PER STEP, and the pristine-protocol version memo still checked EVERY step so a mid-loop exec/flags/@@species swap takes the full observable path from the batch's own last-written lastIndex. Eligibility: fused + non-sticky + no named groups + no /d + subj < u32::MAX. Attempts and swept-young PROVEN invariant (5,514,748 / 5,213,473 both ways — the batch removes setup, never scan or result work). Ablation: regex −1.4% [−1.8, −1.2] (~−13ms marginal on top of session+twin; the map's −35/−55 double-counted the shared kv session share and over-priced the per-step alloc slice). **Fused/pristine result-build dedup + RegexpIterRec named struct + subj_units (`ZIPP_NO_ITER_SUBJ_UNITS`)**: ~150 duplicated lines → `regexp_record_statics`/`regexp_build_result` generic over the slicer, byte-identity by construction (the fused instantiation IS the pristine text monomorphized to the ascii slicer); the iterator tuple became a named struct carrying creation-time subj_units. Ablation −0.0% [−0.9, +0.6] — scouting had already REFUTED the ledger's ~10ms sizing by arithmetic (units()/is_ascii() are stored fields; ≤2ns/step) and the measurement agrees; landed as mechanism footing (doc'd caveat: a faithful A/B of the cache needs ZIPP_NO_MATCHALL_BATCH=1 too — the batch path reads it unconditionally). **Tier-C TailCall admission (`ZIPP_NO_TIERC_TAILCALL`)**: fn6-shaped functions were blacklisted on a single TailCall op while the compiler emits TailCall as a frame-reuse prefix before an ordinary Call+Return. The scout's empty-emission-arm premise was EMPIRICALLY REFUTED by the implementer — a 500k strict tail chain died at MAX_FRAMES=100k because past the region-call cap the interpreter pushes real frames — so the landed arm emits a 3-instruction depth guard (jit_call_depth via offset_of! against JIT_REGION_CALL_MAX=64, jae bail at the TAILCALL ip) and the hop degrades to the interpreter's frame-reuse arm: constant stack, streak-bounded as today. cross_ud gained an exact-Uses TailCall arm (the fail-closed u64::MAX mask would have re-taxed every cross call into the newly admitted body). Ablation: parse −1.1% [−1.6, −0.7] (~−5ms). Review minor recorded: once compiled, the tail-depth RangeError threshold shifts +64 frames (strictly more permissive; repro'd at f(1000062) vs f(1000006) interpreted). **Splice slot-generation guard (`ZIPP_NO_SPLICE_SLOTGEN`) — mechanism lands, headline claim NULL**: the per-execution callee identity+version guard (10 instructions / 3 loads × 13.7M hot executions) becomes one baked-address 32-bit generation compare (4 / 1). Soundness story: `global_gens` Vec (never reallocated; the non-moving collector + rooted globals kill ABA so the version guard's job transfers to rooting) + a fail-closed `bytecode_stored_slots` boot scan covering every StoreGlobal-family op across the interpreter arms AND all six JIT raw-store emitters + enumerated Rust bump sites (access/host_api/run/eval_prog/modules/setup) + an eval-registration hook that bumps AND permanently un-keys any slot new eval code stores; keying requires a reaching-def LoadGlobal proof with no jump target in the gap, slot < global_count, direct routability, and a live-value match at plan time; a gen mismatch falls to the existing call-IC fallback — exactly today's post-rebind behavior, no evict, no patching. JITLOG: KEYED on all 11 mapped parse sites + regex's ri wrappers; the adversarial reviewer re-audited all 26 global-write sites from scratch and confirmed bump completeness. Measurement: parse −0.4% [−0.8, +0.5] at 21 pairs, replication −0.25% [−0.85, +0.44] at 31 pairs — the implementer's 15-rep −0.99% did not replicate; the guard head is throughput-hidden behind the dependent call chain (the scout's own 0.08–0.13 ns/instruction calibration priced 5.5–10.7ms; the OoO core ate it). Kept DEFAULT-ON as a strict instruction-count reduction with zero added hot-path cost and no negative row anywhere (regex −0.0, class +0.1, poly +0.0); the honest claim is mechanism, not wall time. **Scouting refutations (no code written)**: parse INT sub-regions REFUTED — the five carvable tokenizer loops average 5.4 iters/activation, so the 9.39M-iteration × 3.5ns ceiling (+33ms) loses to 26–44ms of region-chaining entry/exit; and the row's EXISTING INT region [121,154] is DEAD in steady state (MEM [9,409] OSR-installs at the outer back-edge and no region-to-region dispatch exists — the W9 yield leaves a compiled-but-never-executed INT region on the row, pure compile waste, noted for a future stats counter). rx-native-scan-loop DEFERRED behind the session hoist's measured result — both eat the same 21ns/attempt budget, the emitter risk is 10×, and the post-session residual is now known to be ~4ns/attempt useful: re-price before ever building. **Process record**: 4 scouts → 3 parallel implementers + 1 dependent (the batch composed on the landed Session API + shared helpers) + an integration gate (104/104 byte-identity cells across the switch matrix) → 5 scoped adversarial reviewers with skeptic verification: ZERO confirmed majors, 3 minors (all recorded above). The scout maps carried exact anchors and were implemented near-verbatim; the one map hazard analysis an implementer refuted (TailCall) was caught by the implementer's own boundary probe before review. Gate (user-shortened): cargo suites green (regexp_slim_exec 19, matchall_batch 12, splice_slotgen 9, tierc_tailcall 6, scan_drain 3, rxjit_differential under --features rx-jit; 871 zipp-vm tests total); 13/13 byte-identical vs node under every new switch and all-off; test262 single `ZIPP_JIT_THRESHOLD=1` sweep IDENTICAL (6). `bench/wave12_bundle_2026-08-20.json`, `w12_{twin,session,batch,subjunits,slotgen,tailcall}_abenv_2026-08-20.json`, `w12_slotgen_repl_2026-08-20.json`, `w12_async_check_2026-08-20.json` |
| **B124 wave 11: string-concat chains fuse; the leaf-splice sheds its per-execution tax; the regex exec fixed path slims — the worst row takes its largest landed win since PGO** | **LANDED — bundle 0.9801x [0.976, 0.985] headline / 0.9847x [0.981, 0.988] all-13; regex-log-scan −9.8% [−10.9, −9.6]; test262 THR1 sweep IDENTICAL (6); no row regressing outside its CI** | **StrConcatChain fusion (`ZIPP_NO_CONCAT_FUSE`)**: `binary()` and template chains of n≥3 with a syntactic string leaf emit INTERLEAVED chain ops — leaf evaluation stays in exact pairwise order (the scout's n-ary window opcode was proven UNSOUND by the mapper: it would move E_i before C_j, observable with object/callable leaves — precisely the gen loop's shape); `add_values_chain` composes in-place growth (non-interned flat acc; the FIRST link stays a plain `Add` so a shared LoadConst string is never grown) with the full pairwise semantics; a direct-append fix removed str_append_inplace's per-link Rust malloc+copy+free (take/put-back split borrow, no VM-heap alloc in the window). Gen-region MEM admission pinned by a JITLOG census test. Ablation: **regex −8.0% [−8.7, −7.4], json −3.6% [−4.9, −3.1]**, markdown −0.3% (its chains fuse in Tier-C bodies but measure ~0 — the map's other-rows hope was optimistic, recorded). **HONEST RE-PRICE**: the map's −200/−250ms did not materialize by ANY implementation — decomposition micros prove call-leaf chains recover mostly the GC share of the pairwise tax (young alloc is bump-cheap, the memcpys cheap at these lengths): gen allocs 7.24M → 2.44M slots, minors 442 → 153, GC 58.6 → 25.0ms; chainFixed 515-530 → 201-212 ns/chain (node 23). Ledger flag: template fusion FIXES a self-referential `${}`-reads-destination case to match node (old zipp exposed partial intermediates), gated by the same switch. **Slim exec fixed path (`ZIPP_NO_SLIM_EXEC`) + ascii-slice fast (`ZIPP_NO_ASCII_SLICE_FAST`) + `regexp_string_iters` → FxHashMap** (switchless data-structure change; B68 refuted the SlotTable sibling at the 418ns/step era — re-tried at the ~280ns step where the probe's share grew): the fused matchAll step skips 8 of the per-call protocol steps (prof + gc_lock KEPT), direct lastIndex heap writes (debug_assert pins the no-user-reference premise — reviewer-flagged, sound today: the matcher idx provably never escapes), one-pass flag decode, merged subject read (flatten only on a Cons), and `JsStr::from_ascii` (a slice of an ascii subject is ascii by construction — kills `from_wtf8`'s rescan on 1.8M slices/run). Ablations: slim −1.0% [−1.37, +0.31] (mechanism footing), ascii-slice **−1.3% [−1.51, −0.34]**. Named follow-ups, sized: RegexpIterRec struct + subj_units (~10ms), creation-arm twin ensure, fused/pristine result-build dedup (150 duplicated lines). **Splice-lite (`ZIPP_NO_SPLICE_FILL` / `ZIPP_NO_SPLICE_ALIAS`)**: the leaf-splice's UNCONDITIONAL per-execution zero-fill of all callee locals (tokIs: 19 stores × 2.89M executions) becomes a may-read-before-write MASKED fill — `cross_uninit_mask`'s fixpoint extracted and reused with a splice ud table (`Call` = guard marker defining NOTHING; Mod/Neg/Upval arms added; masks PROVEN 0x0 on tokIs/mix via a new JITLOG line) — plus read-only param ALIASING (fail-closed def-scan; nested-guard belt-and-braces). Ablation: **parse −2.0% [−2.8, −1.6]**, 4-row −0.70% [−1.15, −0.19]. The guard-hoist cut is DEFERRED to W12 with its map corrected (the callee LoadGlobal sits INSIDE the loop at every hot site, so hoisting must key on the global slot with revalidation at every user-code refetch point — the hazard surface that earns its own wave). The scout's B75-splice reprisal was PRICED AND DECLINED AGAIN (−1.7ms ceiling here; the parse trio is mutual recursion no finite splice depth covers). **Process record**: both implementer agents were killed mid-run by a session usage limit (agents_done=0, ~941 partial lines in-tree that BUILT and passed both new suites); finisher agents gap-analyzed tree-vs-map item by item — concat was structurally COMPLETE (the price gap was the map's overestimate, not missing code), slim-exec's out-of-ownership cuts were finished by hand. A GC-schedule-sensitive nursery-probe assertion (`swept_young > 0`) tripped when fusion legitimately cut the row's allocation count — the probe now makes its young garbage explicit; recorded: allocation-cutting mechanisms can flip schedule-sensitive GC asserts. Two adversarial reviews: all findings minor, three defensive one-liners applied (fn_int/instrument/TaPinPlan chain-op arms). Gate (user-shortened): all suites green (concat_chain 14, regexp_slim_exec 15, nursery 20, workspace); 13/13 byte-identical across all five switches vs node on every run; test262 single `ZIPP_JIT_THRESHOLD=1` sweep IDENTICAL (6). `bench/wave11_bundle_2026-08-19.json`, `w11_concat_abenv`, `w11_slim_abenv`, `w11_asciislice_abenv`, `w11_splice_abenv` |
| **B123 wave 10: the remembered set goes value-grain; the minor's O(heap) mark rebuild dies; the young budget adapts to survival; spill slots are built and REFUTED on their own row** | **LANDED — bundle 1.0004x [0.995, 1.005] headline / 0.9995x [0.995, 1.003] all-13, NULL at suite grain; async −2.4% [−3.9, −1.6] the row win; a composition finding: valgrain's isolated regex −2.0% is given back in-bundle by the adaptive budget settling regex at 32k (adapt alone +0.7% there), and json +1.7% [+0.4, +2.3] is the named sub-2% counter-trade — three GC mechanisms land default-on on their individual verdicts, spilling dark** | **Value-grain remset (the B119/B120 dirty-holder residual)**: `write_barrier_val` records the young VALUE (`vremset`, `GEN_VLOG`-deduped, one masked compare tests young+unrecorded) instead of dirtying the holder, and the minor marks entries directly — the holder's full edge-list re-trace stops existing for value-form stores. regex-log-scan: dirty peak 6 → **0**, wall trace 65.0 → 33.5ms; the [gc-gen] old-trace counter is IDENTICAL in both configs because it always was the MAJORS' trace work — an instrument-reading trap now documented. The value-BLIND card form and its 8 callers (`Heap::replace`, ArrayAppend, the async re-parks, the define arm) keep holder-grain; scan_roots unchanged; a recorded value overwritten pre-minor floats one epoch (conservative, zero on the measured rows); stale-VLOG hygiene at the promote arms and the major wipe (`GEN_SCAN`-only preservation), pinned by unit test incl. the after-major re-record. Ablation (21 pairs, a NOISY block — absolute times +10-15%, paired ratios hold): suite −0.25% [−0.56, +0.07], **regex −2.0% [−2.2, −1.6]** — the target row's CI excludes zero and tracks the trace saving. `ZIPP_NO_VALGRAIN_REMSET=1`; 3 new parity cases + 4 mode rows. **The minor's O(heap) roots term dies (the async-row mystery solved by a controlled ladder)**: async's 24.5ms "roots" was never the enumerated root walk (~6.6µs fixed — microtask queue O(1)-deep, activation registry 2-3 entries, parked windows already incremental via the re-park cards) but `gen_nonyoung_marks` materializing a fresh O(heap) Vec<bool> EVERY minor — measured 0.039ns/slot × 1.37M slots × 463 minors. `take_nonyoung_marks` reuses the post-minor all-true vector (resize + O(young-log) clear; freed-restore + stash after sweep; invalidated at the major wipe/`young_reset`/`set_nursery`); stale-TRUE on young is impossible by construction and `ZIPP_NURSERY_VERIFY` asserts cached == fresh at every minor. async roots 15.2 → 5.0ms same-session; 4-row probe noise-bound (mechanism footing). `ZIPP_NO_NONYOUNG_CACHE=1`. **Survival-adaptive young budget (the B122 async trade's lever)**: after each sweep, survival >25% doubles the budget (cap `NURSERY_BUDGET_MAX` 128k), <5% halves it (floor 16384), 5-25% dead band; pinned by an explicit `ZIPP_NURSERY_YOUNG_BUDGET` or `ZIPP_NO_NURSERY_ADAPT=1`; skipped under GC stress and on all-pretenured epochs. Trajectories exactly as designed: async ramps 16k→131k inside the chain-build and falls back for the drains (minors 454 → 375, minor ms 57.9 → 42.3), json minors 44 → 25, regex settles 32k, map-set 32k; `[gc-nursery]` reports `budget last (peak)` via a separate accessor (the 8-tuple keeps its shape). Suite ablation +0.17% [−0.27, +0.45] NULL (async −0.7% [−2.1, +0.7], direction right in heavy noise vs B121's −3.3% fixed-64k calibration) — mechanism footing, B76/B78. Oscillation bounded by the 5×-wide dead band and monotone negative feedback. **Spill slots are BUILT, review-hardened, and REFUTED on their own row — kept DARK**: the W9 pool wall falls mechanically (weighted-use census with the CallMethod/MathOp/fused-Eq blind spots covered, 8^depth loop weighting; split homes force-resident; r13/r14 join with inline guards; coldest ≤8 homes to 16-aligned frame slots as canonical i64; `Src::S`/`Loc` threading through every arm; spilled homes never lazy; wt raw-then-boxed-then-guard preserved) and BOTH typedarray-math swizzle regions engage (13-14 homes, 4-5 spilled) — but the row measures **448ms spilled vs 361ms declining-to-DOUBLE** (best-of-5, replicating the implementer's independent 451 vs 344), even after the **DV guard prover** landed (`analyze_int_guards` gains DV-gated arms — Bitwise/imul ToInt32, DV results by kind, charCodeAt by the flat-ASCII pin — plus STRICT-ENTRY seeding: loop-carried `|0` accumulators are optimistically seeded i32, verified at the fixpoint's header join, and their entry loads made strict — sound because an entry bail computes nothing; this elided the six bsum-chain guards and freed r13/r14, pool 7 → 9, and STILL lost: four spilled homes' slot traffic plus per-def movsxd on a 0-lazy region loses to fourteen xmm homes with none). B99's lesson generalizes and is recorded: cold-value spilling does not rescue a region whose hot representation is not already winning. `ZIPP_GPR_SPILL_SLOTS=1` opt-in / `ZIPP_NO_GPR_SPILL_SLOTS=1` force-off; the adversarial review (two lenses) found one real hazard — a write-through BOOL home's flush could box caller garbage on a pre-first-def exit — closed with an entry bool load, plus the vacuous None-home escape removed, census sums saturated, decline-log comparability restored. The DV prover stays LIVE by default (it costs nothing when spilling declines and is the named enabler for any future pool work). 12 dv_gpr_tier cases × 6 modes; 4 spill parity kernels. Gate (user-shortened this wave): 36 release suites re-run green; test262 SINGLE sweep `ZIPP_JIT_THRESHOLD=1` (the mode that exercises both the new emitter and the default nursery) — IDENTICAL (6); 13/13 byte-identical on every ablation pair; NURSERY_VERIFY + GC_STRESS via the 12-mode parity matrix. `bench/wave10_bundle_2026-08-19.json`, `w10_valgrain_abenv`, `w10_adapt_abenv`, `w10_nycache_abenv` |
| **B122 wave 9: the nursery goes DEFAULT-ON at last; Tier C yields to the region tier; DV lands on GPR homes and its row is refuted by register pressure** | **LANDED — bundle 0.9946x [0.991, 1.000] headline, all-13 0.9953x [0.992, 0.999]; the engine's collector is generational by default** | **The nursery flips ON (B6's four-wave arc closes)**: two levers changed the economics B120/B121 refuted. (1) The `NURSERY_YOUNG_BUDGET` sweep — executed exactly as NURSERY_DESIGN §2 ordered ("swept empirically") — INVERTED its window guess: vs the 64k default (nursery both sides, 6 rows, 9 pairs), 16384 measured **−1.94% [−2.57, −0.34]** (markdown −6.1%, polymorphic −4.2%, json −2.9%, regex −2.6%) with larger budgets monotonically worse (131072 +1.31%, 262144 +3.64%) and 8192 past the knee (async +7.6%) — frequent cheap minors keep the recycled-slot window cache-hot, the same crossover the GC_GROWTH sweep found for majors; `ZIPP_NURSERY_YOUNG_BUDGET=<n>` is the new knob (latched in `Heap::new`, ≥1024). (2) **Static pretenure** (NURSERY_DESIGN §4): `Heap::pretenure_begin/end` (a depth, exception-safe wraps) routes whole builders OLD-clean past the young log — JSON.parse's tree (all three entries; the reviver path stays OUT so its results stay young) and String.split's parts (user coercions hoisted above the scope so no `?` can leak the depth). Mechanism: a parse-loop's young trace work 160k → **0**, minors 7.4 → 1.0ms; json-large minors 14.9 → 9.1ms (the tree is promoted-vs-never-logged; swept-young identical because it counts freed churn — a wrong-conclusion trap recorded). Pretenure alone −0.81% [−1.86, +0.90]; `ZIPP_NO_PRETENURE=1`. **The default retrial (one binary, 21 pairs, budget 16384 + pretenure): net −0.70% [−0.99, −0.48]**, and the 5-row replication **−2.15% [−3.07, −1.35]**: regex −6.3/−7.1%, json −4.3/−4.4%, markdown −1.3/−2.1% BOTH times, against two named, mechanism-connected sub-2%-to-borderline trades — async +1.1/+1.2% and map-set +1.8/+1.9% (+2.6% in the bundle; the B119-idiom-2 row — 133k old-Map inserts paying the coll_insert barrier — and the sweep shows both rows prefer FEWER minors at every budget). Landed on B113/B115's named-trade footing; `ZIPP_NO_NURSERY=1` is the opt-out, `ZIPP_NURSERY=1` a compat no-op, born-stamping under pretenure is epoch−1 so oracle survival tables stay honest. **Tier C YIELDS to the region tier (B121's 12x tier-selection residual)**: while a function owns a LIVE register-homed region (SROA/INT/DOUBLE — not MEM, whose per-op code equals Tier C's per B107) the whole-function offer is DECLINED (`should_yield_to_region`: one dense read per trip, re-armed via `compile_defer` so a region eviction re-opens Tier C) and an installed Tier C body is EVICTED when such a region lands (the acc_way_gate parking recipe; Tier A keeps its code; the census is maintained at all three insert sites, both removals, and `set_meter`, with a debug recount). fn-scoped fnv1a **10.6 → 4.9 ns/iter** (the ZIPP_NO_FNJIT_MEM approximation predicted 4.8 on the same binary; node 1.05 — the residual is per-call OSR entry: interpreted prologue + one interpreted iteration + `try_run_osr` sync). Suite: **+0.14% [−0.20, +0.31] — NULL**, the B76/B78 mechanism footing (the strict-mode ten's hot loops are loop-scoped; the same blind spot that hid the 7.7x sloppy-call win). Known gap recorded: after an INT-region deopt-eviction Tier C can capture the function before the demoted region recompiles — pre-wave behaviour. `ZIPP_NO_TIERC_YIELD=1`; tests/tierc_yield.rs. **DV get* arms land ON GPR homes — and B121's ~100-120ms price is REFUTED at the register pool**: the arms mirror the B115 DOUBLE arm minus the cvttsd2si integral-pos round-trip and the cvtsi2sd landing (pos is already an integral i64 home; loads land 64-bit-canonical — movsx/movsxd signed, 32-bit-write zext unsigned; the fused Eq becomes an integer `cmp` with no `jp`; BE via rol/bswap), int-lane kinds only (kind 6 joins `>>>` in the lazy-sx WIDE set — the census arm that classified EVERY CallMethod def i32 would have sign-mangled u32s, closed before it could fire), routed EXCLUSIVELY through a `region_int` DV retry so the xmm INT emitter never sees a widened plan (B119 contract). On the fitting shape the tier win is real: fused getUint32 **3.47 → 1.10 ns/iter — node is 2.86**. But every DV region in typedarray-math carries 12-14 homes against the 7-9 GPR pool: top-level vars are GLOBALS and every global pins a permanent home — **B96's permanence finding now binds a third tier**, and the swizzle loop's four globals (o, le, v, bsum) are all written in-loop, so read-only demotion buys nothing. Suite-inert by census (DV retries fire ONLY on typedarray-math, fall to the DOUBLE arm exactly as before; row ablation −0.07% [−0.46, +0.45]). Named follow-ups: read-only-global home demotion, global write-through, or the M4 spilling allocator. En route the GPR emitter's write-through became DEF-COMPLETE: `writes_reg` is blind to CallMethod/MathOp, so a split receiver's Imul or charCodeAt def silently skipped its write-through — the B94 stale-slot failure mode, latent since W8, closed with explicit arm-site calls (zero engaged suite regions change bytes: the one live split's defs are Bitwise); B97 wt-share is now IN scope for DV plans. `ZIPP_NO_DV_GPR=1`; tests/dv_gpr_tier.rs (8 node-derived cases × 5 modes). Gate: test262 IDENTICAL ×4 (default-with-nursery/NO_NURSERY/NOJIT/THR1); release suites green incl. 2 new pretenure parity cases; 13/13 byte-identical in every ablation pair (ALL_CORRECT on all five 21-pair runs). `bench/wave9_bundle_v2_2026-08-19.json` (the landing; `wave9_bundle_2026-08-19.json` is a killed partial kept as the interruption record), `bench/w9_yield_abenv_2026-08-19.json`, `bench/w9_dvgpr_abenv_2026-08-19.json`, `bench/nursery_trial2_2026-08-19.json` + `_rep2` |
| **B121 wave 8: the INT-GPR tier reaches per-op parity with V8; stage-3 nursery lands with its completeness prover; the default trial splits** | **LANDED — headline 0.9837x [0.981, 0.989]; the nursery stays dark on a split verdict** | **Chain-shortening takes the GPR tier past node on its own shape**: the fnv1a census found the ENTIRE gap in two def-site `movsxd` on the h-chain (6 cycles vs node's 4); deferred sign-extension (`lazy_sx_sets` — a home whose defs all provably write i32 keeps either representation, canonicalized at every i64 consumer and ONE cold fix-up at flush_exit, written as the W8 invariant) plus in-place selection, imul immediate forms, and `movzx`-direct charCodeAt: **fnv1a 1.09 → 0.71-0.74 ns/iter — FASTER than node's 0.78**; xorshift 1.95 → 1.05 ≈ node's 1.10; the regex row's fnv1a phase −33%. The lazy×split reconciliation is EXCLUSION (split homes stay canonical at every def — the B94-on-GPR invariant textually untouched; copies from lazy sources canonicalize off-chain). `ZIPP_NO_GPR_LAZYSX=1`. Residual named: Tier C SHADOWS the region tier on call-heavy shapes (fn-scoped fnv1a runs 9ns/iter on Tier C while a compiled 0.73ns region sits unused — a 12x tier-SELECTION gap, now the top INT-arithmetic item). **The W5 INT-split refutation's own follow-up closes**: split-receiver plans now route into the GPR emitter (write-through mirrored instruction-for-instruction from the proven xmm arm), the i32 xorshift fill drops **54 → 21ms** (row −9.6% single-run; exactly ONE region engages suite-wide, provably inert elsewhere). `ZIPP_NO_GPR_SPLIT=1`. **The DV hoist candidate was built, measured NULL, and REVERTED** — B120's issue-parallel lesson replicated with an instrument: the swizzle is bound by the bsum chain (~32c/iter floor), not by guards; the REAL DV path is scoped and priced for a future wave — DV get\* arms ON GPR homes (pos already integral in a GPR, result lands in eax, chain add×4+or ≈ 5c; ~210 → ~100-120ms expected; B22/B32's refutations do not bind — both priced against the boxed baseline). **Stage-3 nursery lands (dark)**: value-grain remembered set at the oracle chokepoints + ~20 new Rust barriers + scan-roots for the exactly-two call-free native store paths (zero dynasm changed); young-only minors via PRE-MARKING old space wholesale, so the unchanged root walk and trace_edges stop at old objects — zero new trace code; float-discount threshold (majors keyed to true live only); one-minor promotion; and `ZIPP_NURSERY_VERIFY=1`, an EXECUTABLE completeness prover that re-runs the full mark beside every minor and panics naming any missed slot — clean on all 13 rows, and **test262 IDENTICAL (6) under `ZIPP_NURSERY=1`**. The oracle's prize is collected where it lives: regex trace 105.3 → 20.3ms, remset peak 1-7 holders (the two-idiom finding confirmed live). **The default trial (one binary, 21 pairs) splits**: async **−3.3% [−4.7, −2.1]**, regex **−2.9% [−3.4, −2.0]** against markdown **+6.2% [+1.5, +7.4]**, map-set +2.6% [+0.1, +8.1], polymorphic +1.6% [+0.8, +2.5] — net +0.34% [−0.3, +0.9], and rows regressing outside their CIs keep the default DARK per §14. Named levers before the next trial: static pretenure of the json/markdown builders and a `NURSERY_YOUNG_BUDGET` sweep. **The async 1.00 → 1.06 capture creep is EXONERATED**: every wave-7 switch null-or-favorable, the source-level A/B measured wave-7 code −2.4% FASTER on the row, the PGO-profile hypothesis did not replicate; verdict is session drift + a noisy midnight capture, and the row is recorded as the suite's most session-sensitive — single-capture deltas under ~5 points do not warrant investigation without a same-session A/B. Gate: test262 IDENTICAL ×4 (default/NOJIT/THR1/NURSERY); 68 release suites; 13/13 byte-identical across six modes incl. NURSERY_VERIFY. `bench/wave8_bundle_2026-08-04.json`, `bench/nursery_trial_2026-08-04.json` |
| **B120 wave 7: the nursery's stage 1 is built, gated, and its DEFAULT is refuted; guard hoisting pays only where issue-width binds** | **LANDED — headline 0.9901x [0.985, 0.995] with the nursery dark; four ablation verdicts, two refutations** | **Stage 1 of B6 (barrier-free minor GC) is BUILT and lands OPT-IN (`ZIPP_NURSERY=1`), its default-on REFUTED by per-row ablation**: async +3.60% [+2.1, +5.1], polymorphic +3.07% [+0.2, +5.1], markdown +2.09% [+0.4, +4.8] against json −1.51% [−3.2, −0.5] — the mechanism is structural, not a bug: a minor still pays the FULL mark (that is what makes it barrier-free), and on churn rows it reclaims nothing a major would not, while float-inflated thresholds schedule MORE marks; the single-run wall wins the build measured were real but two-binary-era noise at suite grain. The implementation is stage-3's substrate and it is COMPLETE: alloc log (~1ns/alloc when latched), `sweep_young`, all 43 retain tables mirrored one-for-one in `prune_freed` (three special cases documented: tuple-keyed intrinsics, value-keyed brand_owner, monotone-brand-id deferral), an exact float census from the full mark driving the major trigger, GC_STRESS extended to interleave m,m,m,M, `[gc-nursery]` stats, and **test262 IDENTICAL (6) under `ZIPP_NURSERY=1`** — the dark collector is conformance-proven. Stage 3 (remset on the oracle's two sites + young-only trace) is where the economics flip: the regex row's ~128ms/run of 95.8%-old TRACE work is its prize, and only it can make minors cheap enough for churn rows. **Guard hoisting landed with its own attribution corrected**: identity guards hoist to region entry under a closed no-user-code whitelist (strings also hoist length into a permanent home; detach/resize impossible by construction in eligible regions), `typedarray-math` **−1.95% [−2.7, −0.8]** isolated and −6.7% in the wave bundle — but the fnv1a target measured NULL, because those guards were ISSUE-PARALLEL all along: the loop is latency-bound on the accumulator's dependency chain (per-op `mov`/`movsxd` round-trips), so removing 8 instructions/iter moved nothing. The win appears exactly where issue-width binds (dense-array sum −4-6%, now BEATING node); B119's "guards are the residual" is corrected to: **chain-shortening in the GPR emitter's instruction selection is the fnv1a path**. `ZIPP_NO_GUARD_HOIST=1`. **Nested GPR shadowing fixed by making the OUTER region fit**: a shared-home re-plan retry (the landed B96/B97 `shareable` machinery, not new liveness) — nested xorshift **11.5 → 2.035 ns/iter** (fn-scoped 2.05 unchanged), provably inert on rows where the retry never fires (zero decline lines). `ZIPP_NO_GPR_NEST=1`. **The cross-call residual was censused and partly reclaimed**: window zero-fill −2.2ns/call via a must-defined dataflow mask over the closed Tier-C op set (u64; unknown op ⇒ fail-safe full fill; GC root set stays complete — `len` spans the window and every exposed slot holds a valid stale-at-worst Value), 4.2M/4.2M fills fast on the micro; the IC-style resolve cache was REFUTED (≈0, the dense probe is L1-hot) and NOT landed; the `maybe_gc` safe point KEPT verbatim — GC schedule byte-identical (72 collections, every counter equal) in all three configs; the native→native tail deliberately not attempted (unproven remainder vs the B117 hazard). `ZIPP_NO_CROSSCALL2=1`. Final bundle (nursery dark): **headline 0.9901x [0.985, 0.995]**, CI excluding zero, no row regressing outside its CI. Gate: test262 IDENTICAL ×4 (default/NOJIT/THR1/NURSERY), 67 release suites, 13/13 byte-identical in default / switches-old / NOJIT. `bench/wave7_final_2026-08-04.json` (the landing) and `bench/wave7_bundle_2026-08-04.json` (the nursery-on bundle, kept as the refutation record) |
| **B119 wave 6: GPR homes land — and reach three rows nobody aimed them at; the B6 oracle keeps the nursery alive** | **LANDED — headline 0.9762x [0.972, 0.981]; one null mechanism, one design-changing measurement** | **The INT tier's bitwise wall falls: `compile_region_int_gpr`** reuses the SAME RegionPlan (B96 liveness, entry loads, guards, deopt ips) but homes values in GPRs instead of xmm-low-quadwords — the census confirmed B118's diagnosis: every Bitwise/imul paid 3 xmm↔gpr transfers on a serial chain (xorshift: 6 ops × 3 transfers/iter) where node pays zero. Pool scraped from the ABI: r15+rbp, idle BOOL_GPRS, rsi (resume-ip demoted to a frame slot), rdi when unmetered, r13/r14 when no i53 guard reads them; hoisted i32 constants become immediates. Anything out of scope (cold blocks, splits, write-through, DV fusion, pool overflow) falls back to the xmm emitter byte-identically. **xorshift 11.10 → 2.00 ns/iter (5.5x; node 1.10)** — and the suite ablations found it engaging on rows nobody aimed it at: **`markdown-render` −8.87% [−10.9, −7.9], `json-large` −7.83% [−8.3, −7.0], `regex-log-scan` −5.31% [−5.7, −4.5]** — their hash/scan loops were all bitwise-demoted to memory traffic and nobody had ever decomposed those rows to see it. `ZIPP_NO_GPR_HOMES=1`. Named residuals: nested-loop shadowing (an enclosing xmm region bypasses an engaged GPR inner), per-iteration pinned-access guard hoisting (fnv1a's remaining 2.7x), Tier-C call-heavy shapes. **The Tier-C compare-fusion follow-up landed as a mechanism**: proto_mem now fuses like region_mem (same `ZIPP_NO_MEM_CMPJUMP` knob, −9.5% on a compare-dominated Tier-C micro) but the parse phase itself did NOT move — it is call-bound at Tier C, not compare-bound; no parse gains booked. **The fused matchAll step is a measured NULL and closes the protocol chapter**: 600,000/600,000 steps fused, zero fallbacks, phase timing within noise — B118's 50-100ms estimate for the step protocol is REFUTED (the eliminated work was ~10-20ns of a ~550ns step), and the per-step alloc census proves everything left is OBSERVABLE (match substring, captures, elements — uncuttable) while the `{value,done}` object was already never allocated on for-of paths. Kept for the deduped pristine proof and the B71-style `regexp_last` deferral. `ZIPP_NO_MATCHALL_STEP=1`. **The B6 generational oracle ran and the nursery STAYS OPEN**: stats-only, zero default-mode change, byte-identical output on all 13 rows. Mean old-attributable trace+sweep over the timed ten = **2.67% > the 1.5% kill line** (regex-log-scan alone 10.7% — 202ms of retracing the retained corpus per run); young survival far under the 30% churn line everywhere (poly 0.1%, regex 3.6%). The design-changing surprise: **old→young stores are TWO idioms, not eight** — young values pushed into a retained array (227k, `jit_set_index`) and young keys into an old Map (133k, `coll_insert`); property stores are ~zero old→young on every row, so the feared emitted JIT SetProp barrier would fire ~never and the write-barrier problem collapses to two helper sites. json-large's 48% old-trace share with only 12 old→young stores is the pretenure case, exactly as `NURSERY_DESIGN.md` §4 predicted. Gate: test262 IDENTICAL (6) ×3; 64 release suites; 13/13 byte-identical in default / switches-old / NOJIT / ZIPP_GCSTATS modes. `bench/wave6_bundle_2026-08-04.json` |
| **B118 wave 5: compare-fusion on the mem tier, for-of finally reaches the JIT, three crash sites closed, and B6 priced honestly** | **LANDED — headline 0.9835x [0.980, 0.990]; two refutations and a design study alongside** | **MEM-tier compare→branch fusion: `parse-large-js` −12.8% [−13.2, −11.7]**, `property-ic-shapes` −6.9%, `polymorphic-objects-v2` −3.8% — the tokenize scout found `charCodeAt` itself AT PARITY (0.8ns/char, str-pinned on every tier) and the real per-char cost in the boxed round trip around every compare: `poly_eq → box bool → store → reload → tag-dispatch → test`, ~4x per character, ~47 cycles/char against node's ~10. The fusion compares Int-tagged operands on flags and branches directly — while STILL STORING the bool, because chained-`||` arms enter at the JumpIf ip and deopt wants the register file exact; that is what separates it from B115's refuted pair-shape experiment. Async ablation null (+0.28% [−0.70, +0.94]). `ZIPP_NO_MEM_CMPJUMP=1`; the Tier-C `proto_mem` emitter wants the same recipe (recorded). **`IterNext`/`PushFinally`/`PopFinally` admitted to MEM regions — B15's "for-of is 28x" item, open since the file began, finally closes**: the for-of dense-array micro goes **326 → 131ms (2.5x)**, the regex row's matchAll phase −8.5%, row −2.2% [−3.3, −1.7]; 449,993 of 449,993 bench steps native, 0 deopts; the helper mirrors the interpreter arm check-for-check, carries its OWN `maybe_gc` (B117's standing warning, honored), and unwinds through a native finally bracket. Plus a `ToNum` string arm (pure StringToNumber; objects still deopt) that stopped the admitted region bailing per-iteration on `+km[2]`. `ZIPP_NO_ITER_REGION=1`, `ZIPP_NO_TONUM_STR=1`. **The regex row re-decomposed post-wave-4**: zipp now WINS the literal-test scan (0.52x) and the replace loop proper (96 vs 147ms — the phase gap is a 17.5M-iteration fnv1a loop at 5.8 vs 1.1 ns/iter, INT-tier per-op quality); the remaining matchAll term is Rust-side exec protocol (4 `flags.contains` scans + `regexp_last` resize + ~5 allocs per step) — a bespoke fused step is the named candidate; corpus generation is substrate (75.3% jit-mem + 20.0% gc on its own script). **Three reachable idiv #DE sites FIXED** — `region_mem`'s Mod and BOTH leaf-inline Mod arms accepted `INT64_MIN % -1` and would have killed the process under `panic = "abort"`; a six-site audit table (two already guarded, one provably unreachable by the i53 entry guards) and a test PROVEN to catch it (guards stashed → child dies to #DE; guards in → node's answer). The inline arms also gained the `-0` remainder sign fix `region_mem` already had. **Strict-mode runaway recursion no longer hangs**: the cause was a feature — `tail_call_position` implements ES6 PROPER TAIL CALLS, strict-only, so `return f()` runs in O(1) stack and a runaway loop simply never overflows, where node (no PTC) throws RangeError at ~10-12k. Bounded with a pop-reset streak counter at 1,000,000 (~100x node's depth): legitimate constant-stack tail loops keep working — including a 100k-iteration loop node cannot run — and runaway shapes now throw node's catchable RangeError. Unswitched (one inc+cmp on the tail path; the bundle shows nothing attributable). **The INT-tier receiver split was built, proven sound (199,992/200,000 native stores), and REFUTED for its purpose**: the split INT fill runs ~55% SLOWER than the MEM tier it was meant to replace, because every Bitwise op round-trips xmm↔gpr on the xorshift's serial dependency chain — wave 3's "+43ms from the decline" attribution was wrong, and the real blocker is now named: gpr homes for bitwise-chain regions. Ships opt-in (`ZIPP_INT_SPLIT=1`), refutation documented at the gate. **B6 priced honestly at last** (`NURSERY_DESIGN.md`, full study): `Value` holds heap INDICES and the collector never moves — so the moving-collector variant retires, promotion is a bookkeeping bit, and the right design is a non-moving index-state nursery (young/old-clean/old-dirty slot bytes + alloc log; the sticky-mark-bit option CONVERGES with it). Expected headline **−1 to −2.5% — smaller than B6's reputation**, because B81's 49x is CONSTRUCTION (Boxes/Strings/Vec pushes), which no nursery touches; the addressable term is B84's 2-12% GC share plus the B117-demonstrated locality effect. First step is a stats-only generational ORACLE with a kill rule (old-attributable trace+sweep < ~1.5% geomean or churn survival > 30% ⇒ B6 closes refuted at measurement cost); pretenuring GATES the retained-tree rows per the B84 pool precedent. Also: the fork's `--no-default-features` build fixed (contradictory cfg stack + missing alloc imports + memchr `alloc` feature). Gate: test262 IDENTICAL (6) ×3; 62 release suites; 13/13 byte-identical in default / switches-old / NOJIT. `bench/wave5_bundle_2026-08-03.json` |
| **B117 wave 4: the regex JIT, native cross-calls, B82 landed — and a GC starvation bug only the suite could see** | **LANDED — headline 0.9849x [0.982, 0.990] after one one-line fix; no row regressing outside its CI** | **The regress bytecode now compiles to native x86-64** (`rx-jit` feature inside `crates/regress-fork`, workspace dynasm, gated exactly like the engine JIT; `--no-default-features` stays pure interpreter): the existing insn stream is compiled — not a backend swap, so semantics carry by construction — with per-byte match tables built by evaluating the INTERPRETER'S OWN predicates per byte value, a native backtrack stack mirroring `BacktrackInsn` entry-for-entry, per-regex compile-on-64th-attempt with compile-time fallback for lookaround/backrefs/general loops/non-byte inputs. On the row: 5/5 hot patterns compiled, **99.994% of 5.51M attempts native**, isolated ablation **−1.74% [−2.2, −1.0]**; 440/440 node-identical on the seeded fuzz corpus + 37 curated cases. `ZIPP_NO_RX_JIT=1`, `ZIPP_RX_JIT_THRESHOLD`. **Tier-C cross-calls go native→native**: the parse row's 1.38M frame calls (87% `compiled → helper → setup_call → frames.push → nested run_loop → dispatch → native callee` — and `call_value` = 0, so B80's 57-63ns attribution does NOT describe this row; the cost was the region-call sandwich, priced component-by-component) drop to 59k; the mutual-recursion micro goes **64.3 → 19.5 ns/call**; `parse-large-js` **−6.83% [−7.7, −5.4]**, `json-large` **−5.14% [−6.1, −4.3]**. `ZIPP_NO_CROSSCALL=1`. **The first bundle showed `regex-log-scan` +8.95% [+7.2, +9.7] and every mechanism-level hypothesis was FALSIFIED by instrumentation** (0 bails, 0 entry misses, 100% of windows in the 9-16-reg bucket): the cause was **GC SAFE-POINT STARVATION** — the replaced route's `dispatch_body` ran `maybe_gc` on every frame push, and those 750k transitions were the corpus loop's ONLY safe points; with them gone, collections went **74 → 1**, the heap ballooned ~37x (199k → 7.4M avg slots), and every subsequent heap access paid the locality bill, visible only as jit-mem sample inflation. One line (`maybe_gc()` in the cross-call helper) restored a byte-identical GC schedule; the row is **−3.7%** in the fixed bundle. STANDING WARNING: any future call fast path that skips `dispatch_body` must carry its own safe point — this failure mode is silent, benchmark-shaped, and invisible to every correctness test. **B82 lands**: `f.call`/`f.apply` target splicing, idiom micro 62.25 → 26.2ns (2.4x — the registry's aspirational 20-60x needs a plan-time splice into emitted code, recorded as the follow-up); its suite row measured **−0.18% [−1.3, +0.4], null**, because the hasOwn-native arm `af125aa` landed already serves the phase (135,707 of ~135,715 calls — credited to that commit, again). En route it FIXED a pre-existing conformance hole: the name-dispatched `call`/`apply` arm ignored own shadows, patched prototypes, class statics and Proxy get traps in ALL modes; now gated by a pristine proof, node-identical, pinned by tests. `ZIPP_NO_CALL_INLINE=1`. **Two pre-existing defects recorded**: strict-mode runaway mutual recursion HANGS the engine instead of throwing RangeError (reproduces on the base binary under `ZIPP_NOJIT=1`; sloppy mode is fine), and `cargo check -p zipp-regress --no-default-features` was already broken before this wave. Artifacts: `bench/wave4_bundle_2026-08-03.json` (the regression, retained as the record) and `bench/wave4_bundle_v2_2026-08-03.json` (the landing) |
| **B116 wave 3: the Mod hole closed, the backtracker stops doing dead work, and the accessor trade reclaimed** | **LANDED — headline 0.9734x [0.969, 0.977]; no row regressing outside its CI** | Six mechanisms, five landed, every ablation single-row 21-pair on one binary. **`Mod` on the double tier: `typedarray-math` −19.28% [−20.0, −18.4]** — the exact `regalloc-emit-unhandled: Mod` hole B113's decline-naming exposed was demoting the whole f64-fill region to MEM; a scout-first pass re-attributed the row phase-by-phase (fill 135ms vs node 26, MEM, named blocker) before any code. Exact-int `idiv` behind cvt round-trip guards; a ZERO remainder takes the ORIGINAL dividend's sign bit (`-6 % 3` → `-0`); `b == -1` deopts (the idiv #DE case — and the audit found the MEM tier's own Mod arm does NOT guard `INT64_MIN % -1`, a pre-existing latent hazard recorded for follow-up). 39,992 of 40,000 fill iterations inline; fill 135 → ~48ms. `ZIPP_NO_DOUBLE_MOD=1`. **Regex auto-possessify + failed-run skip: `regex-log-scan` −5.93% [−6.8, −5.8]** — a greedy single-char loop whose class is disjoint from the follow's first-set pushed a backtrack entry per boundary and retried a follow that provably cannot match: pushes **−98.9%**, retries **−99.4%**, attempts −43% (`ZIPP_RXSTATS=1`). The run-skip is restricted to UNBOUNDED first-atom loops — the audit's own verifier proved the unrestricted form wrong on `/(\d{1,3})\./` against `"12345.6"` (a real match starts inside the run), and that counterexample is pinned as a test; the optimizer's `c+` → `c c*` peel allows exactly ONE peeled atom before the loop (two could consume past the run end). `ZIPP_NO_RX_POSSESS=1`. **B77's matchAll retry finally lands + the B68 fast_ok memo: row −1.41% [−1.6, −0.8]** — the pristine dispatch reconstructed (the original code was never committed; the revert commit holds only the roadmap entry) in a `#[inline(never)]` fn per B77's own instruction, wired into BOTH dispatch entries after a probe showed the single described site was DEAD on the primitive-receiver hot path (0 of 2000 hits); the nine-`pos()` `regexp_matchall_fast_ok` gate becomes version-guarded slots (the promise-cache idiom; per-call value re-reads for what versions do not guard). B77's replicated async collateral did NOT reappear (bundle async −0.1%) — the B114 PGO layout determinism is the plausible difference. `ZIPP_NO_MATCHALL_PRISTINE=1`, `ZIPP_NO_FASTOK_MEMO=1`. **Site-gated accessor emission: `class-prototype-hot` −1.80% [−2.4, −0.8]** — B115's trade reclaimed with room to spare; only sites that have filled an accessor way pay the probe arms, via a monotone `(func_id, ip)` set + evict-and-recompile on first accessor fill. The first build evicted WITHOUT deopting and the mechanism silently died — parked single-activation loops rode the arm-less code to loop exit, misses back at 1.25M with every test green; returning `SELF_CALL_DEOPT` from the fill is load-bearing. polymorphic-objects preserved exactly (misses 36/17, accessor hits ~1.5M; gate flips: 3, then stable). `ZIPP_ACC_ALWAYS_EMIT=1` is the comparator. **The match-result compact record was found ALREADY LANDED** — `af125aa` (the reconciled zipp.org commit) built B60's prescription as a `SlotTable<RegexpResultProps>` sidecar; this wave adds its off-switch, `ZIPP_RXSTATS` counters (600,000/600,000 results compact, 0 materialized on the row), a 10-case node-parity battery incl. GC stress, and the ablation that prices the standing mechanism: **−14.71% [−15.1, −13.6] of the regex row** — recorded so the ledger credits the commit that earned it. `ZIPP_NO_MATCH_VARIANT=1`. **Three delete-canonicality bugs fixed** (conformance): the B113-recorded `delete a["05"]` deleting element 5, `delete a["4294967295"]` answering true while leaving the property, and a non-configurable named `"4294967295"` bypassing the configurable check — all routed through `canonical_u32_key`; targeted test262 slices (delete/Array/Reflect/defineProperty, ~8,800 tests) unchanged. Gate: test262 IDENTICAL (6) ×3 tiers, 54 release suites, 13/13 byte-identical in default / all-switches-old / NOJIT. `bench/wave3_bundle_2026-08-03.json` |
| **B115 wave 2: DataView on the double tier, accessor IC ways; the B113 follow-up refuted** | **LANDED — headline 0.9680x [0.962, 0.974]; one named +1.0% trade** | Two of the three built mechanisms survived their ablations. **DataView get\* hosted on the DOUBLE tier: `typedarray-math` −24.76% [−25.3, −24.3]** — FOUR blockers stood between the swizzle regions and the tier, and B32's warning that "gates opened one at a time hid a fourth" was exact: CallMethod admission (a `pinned_dv` predicate on the tier-agnostic pin plan), TWO recycled receivers per region (B94's single `split_recv` generalized to a set, each split independently proven), a Bool/Num type conflict on the endian flag (solved by fusing the adjacent `===` into the access — `ucomisd; jp/jne`, deopt resumes AT the Eq ip, a one-op pure re-execution window), and xmm pool exhaustion (a no-hoist REPLAN on exhaustion: constants become short in-body ranges). Float loads canonicalise NaN before landing in a home — raw `0x7FF9…` bytes would otherwise be flushed as FORGED Value tags; a test pins it. Mechanism: 73.7M inline DV executions per bench run, ZIPP_PROF 88.3/11.6 jit-mem/jit-fast → 39.8/60.1, tier lines byte-identical across the other 12 benches. `ZIPP_NO_DV_DOUBLE=1`. **Accessor ways in the JIT IC: `polymorphic-objects` −5.66% [−6.1, −5.0]** — B111 proved that row's 1.25M GetProp + 250k SetProp native misses are the accessor receiver BY CONSTRUCTION and can never fill a data way; an identity+version-guarded way tagged in `slot_nhops` bit 31 (stride unchanged, own-data hit path instruction-identical, emission byte-identical with the switch off) now dispatches them, with the getter/setter BAKED behind a live fn-bits re-read (the `__defineGetter__` in-place swap writes without a bump — B78's guard closes it). ICSTATS: misses **1,250,016 → 20** with exact conservation into 1,249,996 accessor-way hits; the baked arm serves ~all of them. The named trade: `class-prototype-hot` **+1.0% [+0.5, +1.8]** from chain-hit probe growth (+4 instr on non-own-data arms); follow-up recorded: site-gated accessor-branch emission. `ZIPP_NO_ACCESSOR_WAY=1`. **The B113 follow-up (MEM-tier pair-shape emission for fused guards) is REFUTED, REVERTED** — built to reclaim `typedarray-math`'s +1.2%, it measured parse-large-js **+1.11% [+0.64, +2.74]**, sparse-array **+1.55% [+1.10, +2.80]**, class-prototype-hot +1.32%, and typedarray-math **+0.23% (null)** — null because the DV landing had already moved those regions OFF the MEM tier, and positive elsewhere because the short `djump_if_not_cmp` shape WAS the B113 win on mem-tier rows. The hypothesis died to its own ablation; only its three MEM-tier differential tests land. Final bundle (one binary, both switches, 21 pairs): headline **0.9680x [0.962, 0.974]**, all-13 0.9749x, diagnostics 0.9984x — the 3-mechanism bundle's +8.8% property-ic-shapes collateral vanished with the revert. `bench/wave2_final_2026-08-03.json`. **Measured at HEAD on the adopted PGO build: headline 1.4513x [1.438, 1.456] vs node** (`bench/head_clean_5cafcb1_pgo.json`, `publishable: true`) — from 1.8012x at B109, three commits earlier; `async-promise-chain` is AT PARITY (1.01x [0.99, 1.05]) and `map-set-heavy` beats node by 29% |
| **B114 the build campaign this file never ran: PGO is the largest lever ever measured here; v3 is null** | **PGO ADOPTED — headline 0.8672x [0.863, 0.870] from build flags alone; two probes closed two items for free** | Zero prior hits for PGO/BOLT/profile-guided in ~113 experiments. Two-stage rustc PGO (`-Cprofile-generate` → train on all 19 benches → `llvm-profdata merge` → `-Cprofile-use`, explicit `--target` to keep RUSTFLAGS off the dynasm proc-macro), measured against the identical-source stock build, 21 pairs: **headline −13.3% [−13.7, −13.0], all-13 −14.9%** — `async-promise-chain` −29.6%, `polymorphic-objects` −20.1%, `sparse-array-v2` −21.8%, `property-ic-shapes` −18.3%, `markdown-render` −16.5%, `regex-log-scan` −15.7%, `json-large` −13.5%, `map-set-heavy` −13.0%, `parse-large-js` −10.9% — and the two ≥85%-jit-native rows (`class-prototype-hot` −0.1%, `typedarray-math` −0.5%) exactly flat, which is the predicted blind spot (PGO cannot see runtime-emitted dynasm pages) and doubles as evidence the effect is mechanism, not layout luck. This is also the deterministic fix for the B61/B77 placement lottery the file has wanted since B77's revert. Adopted as the published-headline build: `tools/pgo.sh` (retrain per capture — a stale profile silently decays toward stock; the capture policy is PGO-build-from-clean-HEAD). The estimate was 1-3%; the measurement is 13-15% — the audit's verifier trimmed the right direction and was still off 5x LOW, because interpreter-dispatch + branchy-helper workloads are PGO's best case and this engine is MADE of them. **x86-64-v3 is NULL**: 0.9955x [0.992, 0.999] headline, diagnostics +1.1% — Cargo.toml's "benchmark it locally if you want the number" is answered; the portable baseline stays. **Two probes closed two items without building them**: the B96 `[pool]` diagnostic printed NOTHING across all ten rows (zero xmm-exhaustion declines), so the global-homes/permanent-pins item is CLOSED — its only claimed value was conditional on measured exhaustion; and the B75 census counted 13 `inner-not-leaf` sites in parse-large-js, 7 in markdown-render, 0 elsewhere — sites exist but B83's mutual-recursion diagnosis stands, so the depth-≥3 splice stays parked pending per-site termination analysis |
| **B113 seven verified quick wins, one binary, per-switch attribution** | **LANDED — headline −3.39%, THE LARGEST SUITE WIN IN THIS FILE; one +1.2% trade, named** | A 30-candidate audit of this file plus the tree, adversarially verified before any code was written (killed there: KeyStore shape-sharing — refuted on polymorphic-objects' own delete loop; for-in gather — refuted by B49's 5.2ns protocol number; INT-tier DataView — refuted by B32's re-run control). Seven survivors, seven `ZIPP_NO_*` switches, measured as ONE binary with `--ab-env` all-switches vs default, 21 pairs: **headline 0.9661x [0.963, 0.968]; all-13 0.9608x [0.958, 0.962]**; all 13 rows byte-identical to node in both modes, and test262 IDENTICAL (6) on all THREE tiers. Single-row ablations: `async-promise-chain` **−17.6% [−18.3, −17.1]** = slot-indexed pristine-Promise cache **−15.4%** (2-4 `pos()` scans per `.then`/`await`/`Promise.all` element → version compares, re-reading per call exactly the slot VALUES versions do not guard — B110's bump table) + one-discriminant `call_value` **−3.7%** (the microtask path paid a 10-arm cascade 1.5M times FROM RUST, unreachable by any inliner — not B82's JS-site item) + dense back-edge dead-check **−1.7%** (a blacklisted loop paid 2 hash probes/iteration; DEAD is sticky — `region_blacklist` has no remove site — so the `Vec<u8>` mirror needs no invalidation; FN_DEAD's precedent). `json-large` **−5.1%** = leaf emission −2.9% (borrowed flat-string quote, `fmt_f64_into`, version-guarded key slots) + per-code-point quote loop → bulk clean-run copy −1.8%. **Fused `JumpIfNotLt/Le` finally emitted** — reserved since their arms landed, and "every backend handles it" was FALSE in four places found by reading first (nested-splice branchy matcher, leaf-inline admission, a leaf-inline emitter catch-all that was a latent `unreachable!` PANIC, the fib base-case recognizer): `property-ic-shapes` **−7.6%**, `sparse-array` −3.2%, `polymorphic-objects` −1.5%, `parse-large-js` −1.0%. **The regression is priced: `typedarray-math` +1.5% [+0.6, +1.7], entirely fused emission (+1.2% isolated)** — JITLOG line-identical modulo ip shifts, ZIPP_PROF 88.5/11.5 jit-mem/jit-fast in BOTH modes, and the double tier's fused arm is strictly shorter than the pair (no `setcc`, no bool home), so the cost sits in MEM-tier fused-guard execution or placement; follow-up: emit the pair shape on the MEM tier only. Array-key canonicality measured **−0.4% [−1.05, +0.10] on its row — a mechanism-only land** (B104 class), and its battery found a PRE-EXISTING bug: `delete a["05"]` deletes element 5 (non-canonical parse in the delete machinery, both tiers, recorded, unfixed). Attribution hygiene alongside (B107 class): every post-plan regalloc/int-emitter decline now prints a named `[decline-reason]` — `typedarray-math`'s [16,47] region visibly falls to MEM for want of a Mod arm, the silent hole that could mislabel MEM as REGALLOC in any tier-share reading. Measured at HEAD after landing: **1.7492x [1.737, 1.760]** (`bench/head_clean_819ab45.json`, `publishable: true`, Node v24.12.0) against B109's 1.8012x [1.790, 1.812] — the projection was 1.74x. `async-promise-chain` 1.68x -> **1.41x**; README updated from the capture in the same push |
| **B112 a cheaper way-0 probe** | **REFUTED, REVERTED — and a 3x bug only wall time saw** | B111 argued that with zero misses on eight rows, the lever is the probe HIT's instruction count. Inlining way 0 (no loop scaffolding) and rewriting the own/chain test as `slot_nhops <= 0x00FF_FFFF` takes a way-0 own hit from **19 instructions to 16**. Suite, 21 pairs, one binary: **0.9986x [-0.51%, +0.33%]** — nothing, with `class-prototype-hot` **+1.0% [+0.4, +2.2]** moving the wrong way outside its interval. Reverted. **The lesson is how the first version failed**: a way-0 CHAIN entry took the `ja` exit and the loop it fell into started at way 1, so that entry was permanently unreachable and every access it should have served called the miss helper — `class-prototype-hot` **+221%**, `polymorphic-objects-v2` +96%, while all 13 benches stayed **byte-identical**, the 8-state IC differential passed in four modes, `ZIPP_JITLOG` was unchanged line for line and `ZIPP_PROF` still said **100.0% jit-mem**. The only instrument that saw it was the wall clock — the fourth time this session (B97, B102, B108). One bisect (restart the loop at way 0) named the cause and refuted two microarchitectural guesses that had already been built and measured to no effect |
| **B111 the shape-keyed IC is worth ZERO on the retained ten** | **PRICED BEFORE BUILDING; diagnostics-only, geomean 0.00%** | `vm.jit_shape_slot` is already a `(site, shape) -> slot` memo at the top of the miss helper, so its HIT RATE is exactly the fraction of native property misses an emitted shape way would convert to call-free hits — the memo cannot remove the call, a way can. `ZIPP_ICSTATS=1` counts it. **Eight of the ten headline rows take ZERO native GetProp misses and nine take zero SetProp misses**; `class-prototype-hot` takes EIGHT in a whole run. The one headline row with misses cannot use them: `polymorphic-objects` is **0.0% shape-known** over 1.25M misses because two of its eight receivers are an accessor layout and an `Object.create` proto object, which return early and never reach the memo — and which an own-property shape way cannot serve BY CONSTRUCTION. The diagnostics behave as designed: `polymorphic-objects-v2` **100.0% shape-known** (5.0M), `property-ic-shapes` **47.7%** of 45.9M. So Phase 2 moves the headline geomean by **0.00%** — the plan's hedge was right and the number behind it is zero, not "little". Build it as O3's substrate, not for the suite. **The more useful reading**: zero misses means those rows' property access is already call-free, so the lever is making a WAY-0 HIT cheaper (~19 instructions, incl. loop setup and a hop test a monomorphic site never needs) rather than adding a ninth way |
| **B110 the shape invariant is now checked, and it was wrong in one place** | **PREREQUISITE FOR THE SHAPE-KEYED IC; one real hazard fixed** | An object's recorded shape must always describe its layout, and nothing checked that, because nothing native reads a shape yet. The audit is far smaller than plan WP-1A implies: structural mutation is ALREADY centralized in `heap.rs`, and of the 20 raw in-slot writes outside it, **19 are `vals[i] = v` (shape-NEUTRAL) and exactly one wrote `attrs`**. That one — `eval_prog.rs` hoisting a redeclared `var` onto `globalThis` — bypassed `ObjMap::set_attr_at`, which exists to make a descriptor change a DICT transition and had **zero callers in the tree**, leaving `globalThis` claiming a shape that lied about its own attr bits. Harmless only because `ic_obj_ok` bans `global_this` from every cache: an exclusion list, not an invariant. Fixed, plus `%Array.prototype%.length = n` (removes keys, no bump) and 13 whole-`HeapObj` overwrites in `construct.rs` (frees the old `vals`, no bump) now via `Heap::replace`. `shape::describe` + `ObjMap::verify_shape` + `ZIPP_SHAPE_VERIFY=1` sweep every live object at every collection and PANIC on disagreement. The existing test helper checked key->slot but **not attr bits**, which is exactly where the one raw write sat — a test pins the verifier going red on it. All 13 benches OK; test262 **IDENTICAL (6) across ~96,000 executions** with the verifier on; 344 lib tests. Off it costs one relaxed load per collection |
| **B109 the first capture this repo can attribute to a commit** | **headline 1.8012x [1.790, 1.812]; `publishable: true`** | `bench/head_clean_e839613.json` — engine clean, equal to workspace HEAD before AND after measurement, binary hash unchanged across the run, Node v24.12.0, 21 counterbalanced pairs, output byte-identical. Diagnostics 4.5981x, all-13 2.2361x, recorded separately by the harness rather than by hand. **The headline did not move**: 1.798x directional -> 1.801x measured. B105 took `async-promise-chain` 1.775x -> **1.68x**, worth ~-0.55% of a ten-row geomean — and Node itself got **6-12% faster on four rows** between the two captures (`map-set-heavy` 972 -> 858ms, `markdown-render` 300 -> 269, `parse-large-js` 289 -> 270, async 363 -> 339). Nothing regressed; the denominator moved, which is precisely why the Node version has to be pinned. Ten rows in a geomean means one row's factor *f* moves the headline by *f*^(1/10), so 1.80x -> 1.0x needs the PRODUCT of ten improvements to be 0.56; both worst rows at exact parity still leaves 1.41x |
| **B108 the gate reported IDENTICAL for code it had never run** | **PROCESS BUG FIXED; one gate result retracted** | `cargo test --workspace --release` builds the LIB and the test harnesses but **not** `target/release/zipp.exe` — nothing under test depends on the CLI bin. `gate.sh` ran `cargo test` and then pointed test262 at whatever binary was on disk, which after a `git stash` cycle for a two-binary A/B was the PRE-change build. It reported `default/nojit/thr1 IDENTICAL (6)` — ~96k executions x3, all green — for code it had never executed, and B104/B105 were pushed on it. Found not by any check but by the next A/B, where two binaries differing only in an argument-buffer change reported `async-promise-chain` **-6.8%**: exactly B105's number, in a comparison where B105 was meant to be on both sides. `gate.sh` now builds first and prints `zipp --version --json`. **This is B103's bug in the correctness gate instead of the performance harness** — the harness had been taught to check its binary's provenance that morning; the gate had not |
| **B105 a promise's two reaction vectors were two halves of one record** | **LANDED — `async-promise-chain` -6.7%, REPLICATED at -7.6%** | Every registration site supplies BOTH handlers at once with the same `dependent`/`finally`/`is_async` — `.then(f)` gives `f` plus a pass-through rejection, `await` a pair of async resumes — and there are exactly two such sites. Storing them in `fulfill: Vec<Reaction>` and `reject: Vec<Reaction>` made the single-subscriber promise (a chain link, an `await`, every `Promise.all` element) allocate **two** first buffers for two halves of one record. `Reactions::{None, One, Many}` holds it inline: **1,530,004 subscriptions, 1,530,004 inline (100.0%), 0 spilled — 3.06M allocations removed**. Row **604 -> 564ms**; headline geomean **0.9939x [0.988, 0.997]**, largest unrelated mover `sparse-array-v2` +0.8% (a row with no promise in it — the two-binary layout confound B77 documented). **Seven times B104's effect for the same order of allocations**, because the win is the OBJECTS that no longer exist (two Vec headers, 48 of a ~58-byte payload; Promise payload 64 -> 48) rather than the allocator calls B104 saved at ~3ns each. Also removes a real retention leak, inseparably: settlement used to drain only the matching vector and the GC kept tracing the other for the promise's life. Eleven ordering/selection tests + a 39-outcome differential byte-identical to node in four modes on both binaries |
| **B104 one malloc + one free per `await`, for a buffer already in hand** | **LANDED, MECHANISM; ~1% and at the drift floor** | Resuming a suspended activation detaches its parked register window with `mem::take` (a move) and memcpys it onto the live file; re-suspending then called `Vec::split_off`, which **allocates a fresh right-sized Vec** while the detached buffer fell out of scope and was freed. Same size, every time — an activation's window is fixed by its `reg_count`. `clear` + `extend_from_slice` keeps the capacity and does the identical memcpy. One `repark_window` now serves all five suspension points (`drive_async`, `drive_async_gen` yield and await, `gen_resume`); the two INITIAL parks keep `split_off` as there is no buffer to recycle at a generator's birth. Mechanism: **1,530,000 re-parks, 100% reused, 0 grew, 26.55M values copied**. Result: `async-promise-chain` **−0.7% [−1.3, +0.2]** over 21 pairs and **−0.9% [−1.3, −0.2]** over 41 — reproducing, second interval excluding zero — but `map-set-heavy` −0.9% and `polymorphic-objects-v2` −1.0% in the same run have no `await` in them, so **it is not distinguishable from the ~1% A/A drift M0.1 measured**. Below §14's bar; landed on the mechanism like B78/B92. Prices a small-class mimalloc alloc/free pair at **~3ns**, which is why removing allocations one site at a time is not a route to parity. `ZIPP_NO_BUF_REUSE=1`, `ZIPP_ASYNCSTATS=1` |
| **B103 the harness could name a commit it never measured** | **FIXED; provenance gated BEFORE measurement** | `README.md` cites `bench/head_clean_2a616f5.json`; that artifact records `git_commit: 2a616f5` and an engine reporting `cdda4e8 + dirty:true` — the PARENT commit, from a dirty tree, in a file named "head_clean". `bench.py` collected the workspace HEAD and the binary's own build identity from two independent sources, both AFTER measurement and only under `--json`, and never compared them; a sweep of all 57 retained artifacts found a second disagreement and only **two** that were ever clean. A headline capture now fails before the first benchmark runs unless identity is present, the tree is clean, the engine's commit equals HEAD, and neither the binary hash nor the reported source changes between the probes taken before and after the run. **2026-08-24 correction:** `--allow-dirty-engine`/`--allow-nonhead-engine` suppress those rejection reasons and can leave `publishable:true`, so overridden artifacts require manual provenance audit. An **A/B is never blocked**: it compares two builds that cannot both be HEAD, and the `--ab-env`-on-one-binary idiom reports the same source on both sides by design. Also moves the retained-ten/diagnostic-three split out of `run_real.sh` shell variables and into the harness — a default run globbed all 13 files and printed one geomean about **0.43x high**; artifacts now carry both row sets and both bootstrapped geomeans. 17 new tests (45 total) where provenance had zero |
| **B101 the tier programme has a ~15% CEILING** | **COSTING; B94's 3.2x does NOT transfer to real code** | Prices the FINISHED programme (heap ops hosted on the register tier) before building it. Homes in **callee-saved xmm6..15 survive helper calls**, so no spilling is needed and the earlier "spill 12 homes per call" estimate was far too pessimistic; a heap op costs only boxing its operands and unboxing its result. `7 x reads - 10 x heapops`, weighted by each row's mem share: **32 / 22 / 15 / 14 / 12 / 5 / 4%**, geomean **~15%** — **1.79x -> ~1.51x, not parity**. B94's 3.2x micro is ~100% numeric ops; real regions are 10-25% heap ops and (B99) mostly single-use temps. Parity has to attack the NUMBER and COST of heap ops instead: GetProp/SetProp are helper calls (~20-40 instrs) where an inline monomorphic access behind a shape check is 3-4 — the same order as the whole tier merge, and they compose |
| **B99 register homes IN the memory tier** | **REFUTED BY MEASUREMENT; no code written** | A home saves ~7 instructions per operand read but costs ~7 to FILL, so it breaks even at one use. Counting the biggest region per row, only **6-17 of 34-88** numeric reads land on a multi-use register: net instructions/iteration from promoting the ten best candidates is **0, 0, 0, -28, -14, +42** — five of six rows gain nothing or LOSE. Cause is the bytecode shape (`LoadGlobal t; use t` per operand, so almost everything is a single-use temp), the cost-side view of B93's "LoadGlobal is 29-37% of every mem region". Corrects why the register tier is fast: not caching multi-use values (there are none) but keeping a GLOBAL in one home for the whole region, guarded once. Next probe is therefore LoadGlobal/consumer FUSION — which was then priced the same way and ALSO refuted: only **0-11%** of numeric reads are fusable (0% on map-set-heavy, parse-large-js and markdown-render), because a global must be used ONLY numerically to be homeable and these regions use globals for objects, strings and receivers too. Both local routes into the memory tier are closed; whole-region TYPE SPECIALISATION is the only thing that removes the per-operand check, and the hot regions cannot have it while they contain Call/CallMethod |
| **B97/B98 write-through home sharing + Add live-ins (double path)** | **MECHANISM; suite NULL; first DOUBLE regions ever compiled here** | B97 lets a `read_outside` register SHARE an xmm home by generalising B94's write-through (store each def to `[rbx+dreg(r)]`, skip it in the flush) — removing the `xmm pool exhausted` blocker B95 called terminal. B98 admits `Add` operands as numeric-required uses of a read-only live-in **on the double path only**: the 3.31x->3.45x regression that refuted this was BLANKET admission on the INT path, and its stated causes were string/double/object live-ins — a double is native here, so the largest cause does not apply. Together: **class-prototype-hot 3 declines -> 1**, polymorphic-objects 7 -> 3, and DOUBLE regions **0 -> 1 / 0 -> 1 / 0 -> 3**. Suite **-0.16% [-1.87, +1.42]** — null; the promoted regions are COLD, and the hot ones are now blocked by `CallMethod`, a MISSING CAPABILITY (the register tier issues no calls; B78's method inlining is memory-path only) rather than an admission gate. **Introduced a wrong-answer bug** (a shareable reg loses its entry load; an untaken-branch def then flushed a garbage home) caught by the kept `hoisted_const_on_untaken_branch` tests while all 13 benches still said ALL_CORRECT=1. Also: a pre-fix A/B showed sparse-array -3.0% twice; it did NOT reproduce on the fixed build. `ZIPP_NO_WT_SHARE=1` |
| **B96 B95's register-pressure figure was a bad proxy** | **CORRECTION; the fix is smaller than B95 concluded** | B95 priced the wall as 40/76/73/70 distinct VM regs vs 14 homes. Max SIMULTANEOUSLY live is **12/14/16/13** (8/11/17/15 with ranges split), plus 9-17 permanently-homed globals — a **~1.5x shortfall, not 5x**. And the excess is PERMANENCE, not liveness: `read_outside`, live-in and hoisted regs each pin a whole-region home, and every global does unconditionally. The `read_outside` rule is conservative for exactly the unsoundness **B94 already fixed** (flush_exit writing a shared home into every sharer's slot) — write-through makes sharing safe. So the next step is NOT a general spilling allocator: generalise B94's write-through to `read_outside` regs, admit Add-using read-only live-ins backed by a plan-time numeric OBSERVATION (blanket admission measured 3.31x -> 3.45x), and spill only if pressure still exceeds 14. Adds a `[pool]` demand diagnostic, which prints nothing until the read-only-live-in rung is cleared |
| **B95 dense-Array reads on the double tier; the ladder ENDS at register pressure** | **MECHANISM (12 declines cleared, 0 promotions); the wall is now NAMED and MEASURED** | The double tier admitted only kind-8 Float64Array elements (`pin_kind = if admit_bitwise {5} else {8}`); a dense ORDINARY Array — what almost all JS indexing touches — declined the whole region. Now admitted with the INT tier's per-access tag guard (`emit_box_to_home`): Int converts, double moves, HOLE/bool/null/heap deopts. Writes still decline (Value::num narrowing is separate). GetIndex declines: polymorphic-objects **3->0**, class-prototype-hot **2->0**, sparse-array 5->2. **Not one region promoted** — third consecutive rung. So the REST of the ladder was priced in one experiment instead of climbed: admitting `Add` operands clears the read-only-live-in blocker and reveals **`xmm pool exhausted even with home reuse`** as terminal. It is a hard limit — **14 homes** against **40 / 76 / 73 / 70 distinct VM regs** plus 10-17 globals in the four regions — and the tier gives one PERMANENT home per value with **NO SPILLING**, so an oversized region declines wholesale to a tier B94 measured at 3.2x slower. Retires the B81/B83/B90 framing AND the B92/B94/B95 ladder: the one remaining item is a real register allocator with spill/reload |
| **B94 live-range splitting for a recycled receiver** | **MECHANISM ONLY — 3.2x on the shape, ZERO suite regions** | Gives a VM register the bytecode compiler recycled (pinned array at ip37, sum at ip45, counter at ip49) a numeric xmm home while its memory slot stays the receiver's, via write-through at every numeric def + a flush skip — chosen over per-ip flush variants because it makes every exit correct without knowing the path. Delivers the number B93 lacked: an `a[i&1023]` loop goes **98ms -> 31ms (node 16ms), 6.5x -> 1.94x, 99.5% jit-mem -> 100% jit-fast** — so promoting a real array loop is worth **3.2x**. But the trigger needs a GLOBAL receiver and every benchmark loop is inside a function (`TaPinSrc::Reg`), where one slot cannot hold both the array and the number: **0 splits in all ten rows**, decline histogram unchanged. Method note: the first differential passed in 4 modes while the feature NEVER FIRED (IIFE locals emit no LoadGlobal) — a green differential proves nothing until ZIPP_JITLOG shows the mechanism ran. Also fixed a panic from sharing the planner with region_int |
| **B93 `jit-native` was TWO tiers wearing one name** | **INSTRUMENT ADDED; the B90/B92 reading of four rows INVERTED** | Splitting the profiler's `jit-native` bucket by tier shows six rows are **57-100% in the MEMORY tier**: class-prototype-hot **99.9%**, typedarray-math 87.8%, map-set-heavy 84.5%, parse-large-js 78.3%, polymorphic-objects 67.5%, sparse-array 57.0%. class-prototype-hot was cited as the row where tier entry is SOLVED. Counting the ops responsible: **2-5% of a region's ops force the other 95-98% onto the slow tier** (polymorphic-objects 3 of 135, class-prototype-hot 3 of 71, sparse-array 2 of 63) and the blocking set is Call/CallMethod/GetIndex/SetIndex. Root cause under several declines is **bytecode temp-register recycling**, not the ops: the simplest possible Float64Array loop declines `pinned receiver reg not cleanly excludable` because r17 is the pinned receiver at ip37 and an arithmetic temp at ip45 — so regalloc's pinned-element `movsd` path is near-dead on real code. Validated on two known-answer workloads + ZIPP_JITLOG. Ceiling is INFERRED from B92's 4.20->1.05ns, not proven on these rows |
| **B92 one bitwise op demoted a whole region out of register homes** | **LANDED as a MECHANISM (suite flat, mechanism 4x)** | Corrects B81/B83/B90: there are **three** region tiers, and the middle one (`compile_region_regalloc`) ALREADY has the f64 xmm/gpr homes that M4 describes — it just declined `Instr::Bitwise` outright. One `&`/`\|`/`>>>`/`\|0` in a loop demoted the whole region to memory: an identical 20M-iteration loop went **0.75 -> 4.20ns/iter** from adding `i & 1023` (node 0.75 either way). Emitting ToInt32 inline via 64-bit `cvttsd2si` (exact below 2^63; the INT64_MIN indefinite bails) fixes it: **4.20 -> 1.05ns/iter**, verified over **6,144 cases** against node in 4 modes. Suite **-0.39% [-0.78, +0.25]** — flat, because the Bitwise declines went to zero and every one of those 13 regions hit its NEXT blocker. **Tier promotion is a ladder, not a switch**; the next rungs are counted: read-only live-in (5), unpinned GetIndex/SetIndex (7). `ZIPP_NO_DOUBLE_BITWISE=1` |
| **B91 INT-tier promotion is closed by B9** | **NO CODE CHANGE; hazard documented at the switch** | Four rows are >=85% native, so promoting regions to the INT tier (xmm homes, at parity with V8) is the cheap route to M4. The declines are everywhere — `typedarray-math` 7 regions, `sparse-array` 8, `polymorphic-objects` 7 — under one blanket reason that names nothing, and `compile_region_int_maybe_cold` ALREADY implements the fix behind a `cold_exit` flag the only caller hardcodes to `false`. **That flag is dead on purpose: it is B9, which passed a fully green gate (96,029 test262 executions, both tiers, GC stress) and still returned `s = 0` for `3050`.** The comment now carries the warning at the point of temptation, including that the soundness argument beneath it is the one B9 shipped with and is wrong. Stopped one edit short of reintroducing it — by a kept negative result |
| **B90 the profiler mis-attributed 24% of json-large** | **INSTRUMENT FIXED; a B83 conclusion CORRECTED** | Single-argument JSON.stringify/JSON.parse are FUSED by the compiler into their own ops and never reach call_native, where the tag sat — a 470ms stringify-only workload reported **100% interp**. Retagging moves json-large to **stringify 24.0%, interp 40.0% -> 15.4%**. B83 had read that 40% as the row not compiling, and B87 was aimed at it. The resting bucket is now interp/untagged, documented as no-tag-active rather than interpreter-running. Added a microtask phase: async-promise-chain splits 79.1% into **60.6% real interpreted user JS + 16.9% event loop**. Corrected profile: **four rows at or above 85% jit-native**, the clearest statement yet that M4 is the wall |
| **B89 pure `ToNum` helper for a string operand** | **REFUTED, REVERTED** | `+x` bailed for every non-number; a string's ToNumber is a pure parse, so `+m[1]` / `+k` deopted 64 and 128 times and evicted. Built to dodge B87's trap — no refetch (the helper cannot allocate or re-enter) and `ToNum` is rare — and it dodged it: deopts **64 → 0**, 28 operand kinds node-identical on all tiers, user `valueOf`/`toString` running exactly once per `+`. Still lost: `regex-log-scan` **−0.2% [−0.4, +0.1]**, `sparse-array` **+1.3% [+0.5, +1.9]**, suite +0.04%. `sparse-array` now pays a helper call per for-in key where interpreting was cheaper. **Second consecutive deopt-removal to measure ≤0**: a deopt is only worth removing when the replacement beats the interpreter ON THAT PATH — B88 won because it was a wrong branch, B86 because the op was rare |
| **B88 `===` deopted the region on a double operand** | **LANDED — suite −1.64%, THE LARGEST WIN IN THIS FILE** | `region_poly_eq` jumped to the numeric compare as soon as operand **a** was a double, without checking **b** — and that path bails for a tagged non-number. So `x !== undefined` / `!== null` / `!== "s"` with `x` a double bailed EVERY iteration: 64 deopts, eviction, loop interpreted forever. `map-set-heavy` contains its own control — the same comparison twenty lines earlier does NOT deopt, because that map holds Ints. Fix is a definition, not a heuristic: a Number is never `===` to a non-Number, so a tagged non-Int operand has a constant answer. Isolated **49→8ms / 49→8ms / 83→8ms** (int arms unchanged; node ~2ms); deopts **64 → 0**. **`map-set-heavy` −11.3% [−15.9, −8.4]**, `json-large` −3.1%, `typedarray-math` −1.0%, `property-ic-shapes` −0.9%, **suite −1.64% [−2.21, −1.00]**, no row regressing. 362-pair exhaustive `===`/`!==` matrix (NaN, −0/0, every tag pair) byte-identical to node on all three tiers. Found by a 3-probe workflow whose adversarial pass refuted **12 of 14** claims; this was one of two survivors. Off-switch `ZIPP_NO_POLYEQ_FAST=1` |
| **B87 same fix for `SetIndex`** | **REFUTED, REVERTED** | `json-large`'s `build` deopted **101 times** at a `SetIndex` (42.2% of that row interpreted), so B86's fix was applied to it — narrowly, only the plain-object/string-key branch. Deopts **101 → 0**, interpreted 42.2% → 36.3%, all 13 benches byte-identical — and the row got **+1.1% SLOWER [+0.4, +2.0]**, suite +0.09%. Because the delegating helper can allocate and frame-call a setter, the emitter needed `emit_refetch_pinned` (two native calls) after EVERY `SetIndex` including the hot dense store that never delegates. **Where the new cost lands matters more than how much old cost was removed**: `SetIndexConcat` is rare so B86 was nearly free; `SetIndex` is `a[i] = v`. "Deopts went to zero" is not evidence of a win |
| **B86 `SetIndexConcat` appends instead of deopting** | **LANDED — `polymorphic-objects` −3.9%** | The profiler's first catch. That row was **60.5% interpreted with six decline messages** — not rejected, but compiled and then thrown away: `SetIndexConcat` handled only an own-slot HIT, and the dict-churn loop writes sixty NEW keys into a fresh `{}` per iteration, so it deopted every time (**131 deopts**), passed `OSR_DEOPT_LIMIT`, and got the region evicted and blacklisted. New keys now delegate to `set_index_concat` — the interpreter's own function, so semantics are identical by construction — with `CALL_THREW` and the pinned refetch the newly-allocating path requires. **Deopts 131 → 0, interpreted 60.5% → 26.6%**, row **−3.9% [−4.5, −2.9]**, no row past the +2% rule, gate green. Invisible to every tool that existed this morning: B83's own phase decomposition called this row "diffuse, no single term to attack" |
| **B85 `ZIPP_PROF=1` sampling profiler — §6's item since B3** | **LANDED; it reprioritises B6 vs M4 on its first run** | Phase-tagged sampler (200µs), not a stack sampler — fat LTO would have inlined away most frames a `StackWalk64` wanted, and suspending the engine thread to symbolize is a deadlock hazard. Splitting `interp` from `jit-native` is what made it actionable — the two have OPPOSITE fixes. **Compiled-code-bound: `typedarray-math` 99.7%, `parse-large-js` 91.3%, `markdown-render` 68.6% in native code** (only M4 reaches these). **Interpreter-bound: `polymorphic-objects` 60.5%, `json-large` 42.2% INTERPRETED**, with only 6 and 11 decline messages — so their loops are not rejected, they are never offered, which is a tier-entry lead rather than a codegen-quality one. GC reads 0.8-12.6%, independently corroborating B84's `ZIPP_GCSTATS` numbers. So the dominant substrate item is **M4 (memory-backed register file + per-op NaN-boxing)**, not B6 — `parse-large-js` is 2.49x with 99.2% of its time executing JS, which no allocator or collector fix can reach. Built because B84 had to revert a real −35% win for want of attribution |
| **B84 GC is 2-12% of the suite — B81 CORRECTED; `ZIPP_GCSTATS=1` added** | **INSTRUMENT LANDED; ObjMap pool measured −35% on construction and REVERTED anyway** | Per-phase collector timing says GC is **12% of `regex-log-scan`, 8% of `json-large`, 5% of `markdown-render`, 2% of `polymorphic-objects`** — their live sets (1.4k-78k) never reach where B81's micro curve bites, so the cost is CONSTRUCTION, not collection. B81's table stands; its conclusion did not. The pool's first null result was also wrong — a 4096 cap against a 65,536 GC threshold serves ~6% of a cycle. Re-capped it is **`{a:1}` 71.0 → 46.5ns, `{a,b,c,d}` 138.5 → 88.5ns (−35%)** — but `polymorphic-objects` −5.7% came with `json-large` +1.6% (which RETAINS its tree, so the pool stays empty), a suite CI including zero, and a targeted fix that made BOTH rows worse. Two runs disagreeing by five points on the same change ⇒ revert per §14 | **B81**, B6 |
| **B81 the COLLECTOR is 10-50x node — B6.0's precondition met, its guess REFUTED** | **MEASURED; ObjMap recycle pool built and REVERTED as a null result; B6 is now the best-evidenced item in this file** | `{}` 41.0ns vs 3.5ns, `{a:1}` 81.5 vs 2.5, `{a,b,c,d}` **148 vs 3.0 (49x)**, `[]` 23.5 vs 2.5, `s.slice(0,5)` 38.5 vs 4.0 — and the no-allocation control is 1.0ns vs node's 1.5ns, i.e. **zipp is FASTER when it allocates nothing**. Reached independently from `regex-log-scan`'s phase split (matchAll 496 vs 71ms; `exec` = 227ns scan + 169ns result object + ~60ns/capture against node's 40ns total, while the non-capturing literal `test` is **0.53x — zipp wins**) and from the object-literal micro. A 4-property literal has gone 513ns → 148ns since B6.0 was written and is still 49x. An `ObjMap` recycle pool (reset + stash the swept box, capacity retained) measured **ZERO** and was reverted — because `[]` costs 24.5ns while mallocing nothing at all, so the malloc is only ~7ns of an object. The cost that remains is the COLLECTOR: holding a larger live set makes the IDENTICAL allocation loop cost more — 74.5 → 101.5 → 122.5ns at live sets of ~0 / 400k / 1.2M, i.e. **+48ns per allocation from nothing but a bigger heap to mark**, where node goes 2.0 → 7.5 → 9.5. B6.0 asked whether the cost was construction or collection and guessed construction; it is both, and only the collection term scales. `typedarray-math` is the one row this does NOT explain — its DataView loop allocates nothing and is already a fully inlined native load, so that one is M4 | **B6.0**, B1 |
| **B80 sparse enumeration hoists its overlay probe** | **LANDED — `sparse-array` −16.2%, suite −1.41%, THE LARGEST SUITE WIN IN THIS FILE** | `object_enum_own`'s array arm walked `0..dense_len` doing an `array_index_override` HASH PROBE per slot. Strided writes grow the dense vector far past the populated count — measured `dense_len=1,040,001` holding **105** elements for an array with 5,000 keys — so `Object.keys` paid **1.04M hash probes to find 105 elements**: 25ms against node's 0ms, and independent of key count, which is the shape that gave it away. Asked once over the overlay's keys instead; a `defineProperty` on a dense index keeps the per-slot probe. `for…in` 50e6/5k **26ms → 2ms**; the bench phase 42ms → 18ms. Row **−16.2% [−16.9, −14.5]**, suite **−1.41% [−1.90, −1.05]**, both CIs excluding zero, no row regressing outside its CI. Found by phase-timing the SMALLEST bad row, not the largest. Off-switch `ZIPP_NO_ENUM_HOIST=1`; both paths byte-identical on the whole case set |
| **B79 B5.3 refuted; `promise.then` pristine guard** | **LANDED — `async-promise-chain` −3.2%** | `ZIPP_BUILTINSTATS=1` (new) counts the builtin calls that reach the generic chain: `parse-large-js` **89**, `polymorphic-objects` **0**, `markdown-render` 252,669 (~2.3% of its row) — so **B5.3 is REFUTED**, Effort-M saved. The one row it pointed at is `async-promise-chain`, whose 1,500,003 dispatches are **100% `promise.then`**; that arm proved intrinsic-ness with a full `get_prop` chain walk per call and now uses B69's three-read pristine probe. **100ns → 87ns** per `.then()` (node 73ns); row **−3.2% [−4.0, −2.5]**, suite −0.48% [−0.91, +0.01], no timed row regressing outside its CI. Second finding, bigger than the first: for every builtin WITHOUT a region intrinsic the JIT is SLOWER than the interpreter (`str.startsWith` 44.5 vs 39.0ns) — the generic `CallMethod` arm has no native IC where `GetProp` has an 8-way one. Off-switch `ZIPP_NO_PROMISE_PRISTINE=1` |
| **B78 method inliner admits the PROTOTYPE chain** | **LANDED, MECHANISM ONLY** | `build_method_shape` had arms for a class instance and for an own data slot, and declined everything else — so `Object.create(proto)` and `Ctor.prototype.m = fn` inlined NEVER, at any receiver count: **29.5ns/call at ONE receiver against 5.5ns for the same method on a class**, node 1.0ns. With the arm: **5.5ns (−81%)**, and the indirectly-loaded receiver 34.8ns → 6.0ns. Guards reuse `SuperInline`'s hop-version emission plus a `holder_vals_ptr[slot] == fn_bits` re-read; resolution is `ic_walk`, so the baked answer is by construction the interpreter's. Suite `--ab-env` on ONE binary, 21 pairs: geomean **−0.28% [−0.81, +0.17]**, **no row regressing with an interval excluding zero**. Ships on the mechanism — the ten rows do not contain the construct, which is a fact about the benches. Off-switch `ZIPP_NO_PROTO_METHOD_INLINE=1` |
| B77 pristine matchAll dispatch | **REVERTED — layout collateral, REPLICATED** | won its row twice (−2.1%, −2.8%, CIs excluding zero) and regressed `async-promise-chain` twice (+5.4%, +3.1%, CIs excluding zero) — a row with no `matchAll` in it. The fat-LTO layout hazard, confirmed by replication for the first time this session. §14's unrelated-row rule applies. The guards themselves are sound and node-verified; a retry should change the code PLACEMENT, not the semantics |
| B76 nested splice admits ARGS | **LANDED, MECHANISM ONLY** | `wrap(n){ return inner(n,7)+1 }` was rejected (`inner-call-has-args`); now spliced: **−55.1%** on the 3M-call micro, faster than node. Suite uptake ZERO — every previously-blocked site moved to `inner-not-leaf` (depth ≥ 3), which is B75's real design task. Also fixed a latent `unreachable!` panic: the emitter had no `LoadUndefined` arm for a void inner return |
| **B74 leaf inliner admits `GetProp`** | **LANDED** | the plain-call shape goes **29.2ns → 9.7ns (−66.8%)**; `class-prototype-hot` −1.0% [−1.5,−0.1], `polymorphic-objects` −0.9% [−1.3,−0.6], `async-promise-chain` −0.6% [−1.2,−0.2], suite −0.18% [−0.43,+0.03]. Measured with `--ab-env` on ONE binary, so no layout confound. Site-free helper (the `GetIndex` precedent), so B73's IC-budgeting problem never arose. Off-switch `ZIPP_NO_LEAF_GETPROP=1` |
| ~~B73 leaf inliner lacks `GetProp`~~ | **CLOSED BY B74** | a plain `f(o)` whose body reads a NAMED property off an argument is `(not leaf-eligible)` and pays a full frame call per iteration in a hot loop: **30.1ns against 7.0ns** for the identical body written as a method, which the method inliner DOES inline. `callee_leaf_ok`'s whitelist admits `GetIndex` and not `GetProp`. 28 declined sites in `parse-large-js`, 20 in `markdown-render`, 7 in `json-large`, 5 in `regex-log-scan` — unweighted. Broader than any single row |
| class-prototype-hot decomposed (B73) | **ACCESSOR PHASE IS A 2.9x WIN** | 66ms against node's 191ms with `super.v` chains and overridden setters — larger than the plan recorded. Do not "optimise" it. The row's loss is entirely method + plain calls |
| B72 SetProp shape memo | **REFUTED, REVERTED** | +15.9% on the write micro. `PROP_INDEX_THRESHOLD` is 12, so a narrow map's `pos(key)` is a linear scan over a few short strings — cheaper than hashing a `(u32,u32)` and probing. Break-even is above the threshold; retry only gated on `keys.len() >= 12`, and re-measure the GET side the same way |
| B72 string_method receiver clone | **REFUTED** | flagged as an O(len) copy every string method pays; `s.slice(0,5)` is FLAT from 64B to 32KB receivers and zipp beats node at every size. Needs a scaling curve before anyone "fixes" it |
| typedarray-math decomposed (B72) | **63% IS DATAVIEW, NEEDS TIER D** | the DataView loop compiles but only to the MEM tier: `v = dv.getUint32(...)` ranges through 2^32-1 so INT declines. 4.9ns/call against node's 1.3 is a per-op arithmetic gap, not dispatch. f64 fill is another 20% |
| B71 `.test()` allocates nothing | **LANDED, ROW FLAT** | Annex B statics slot 1 (`lastMatch`) joins the deferred-range set, so a successful `.test()` stops materialising the matched span; plus an `is_empty` gate on the always-missing `regexp_exact_source` probe. **−10.0% median on anchored `.test()` x300k**, but `regex-log-scan` −0.3% [−0.9, +0.2] — the `.test()` phases are ~2% of that row. Suite −0.28% [−0.62, +0.10]. Ships on the mechanism, not the row |
| B70 `re.flags` pristine shortcut + RegExp text shared | **LANDED — FIRST SUITE-LEVEL WIN** | `re.flags` was reading EIGHT per-flag accessors per call (200ns vs node's 10); `matchAll` reads it just to test for `g`, making it 175ns of a 493ns call. Plus `source`/`flags` → `Arc<str>`, so a matcher clone shares text instead of copying it. **`regex-log-scan` −2.9% [−3.8, −1.5]; suite geomean −0.55% [−0.91%, −0.19%]**, both CIs excluding zero. `HeapObj` size hypothesis REFUTED (still 80) |
| B69 RegExp `test`/`exec` dispatch arm | **LANDED** | the builtin dispatch had eleven receiver-kind arms and none for RegExp. A hot `re.test()` loop **−16.8%**, 1.17x → **0.97x node** (interleaved best-of-9); `regex-log-scan` only −1.1%, because the arm fires on a RegExp receiver and 88% of that row's deficit is elsewhere. Suite −0.16% [−0.40%, +0.09%]. Override-safe, unlike its siblings — 8 node-verified cases in `tests/regexp_dispatch_arm.rs` |
| regex-log-scan, rephased (B68) | **DECOMPOSED; SMALL-PHASE NUMBERS CORRECTED IN B69** | 42% of the row is corpus generation, which contains NO REGEX; 46% is matchAll (64ms of it in iterator CREATION, ~178ms stepping); the literal-`test` phase is **0.40x** — zipp wins. Per successful capturing `exec` the deficit is three comparable terms: +139ns result construction, +87ns matcher, +80ns fixed call overhead |

---

## 1. Where the project actually is

### Conformance — 99.994% test262, 96.8% intl402

| slice | executions | pass | fail |
|---|---|---|---|
| ECMA-262 + `staging`, sloppy **and** strict | 95,942 | 95,936 (99.994%) | 6 |
| `intl402` (opt-in) | 6,714 | 6,502 (96.8%) | 212 |

The 2026-08-24 gate ran default JIT, `ZIPP_NOJIT=1`, forced JIT
(`ZIPP_JIT_THRESHOLD=1`), and majors-only GC (`ZIPP_NO_NURSERY=1`). All four
produce the same ordered six failure identities.

The main-suite denominator moved twice, both times for a reason worth recording.
It dropped by 2 (95,848 → 95,846) when a leftover scratch file from a crashed
sweep was deleted from the test262 checkout: it predated the `.zipptmp-` prefix
the walk now skips, and being a harness+test concatenation it ran and scored as a
pass — the exact phantom that prefix exists to prevent, still present from before
the prefix existed. It then rose to 95,942 when the vendored suite was updated to
`defaaf15`. Quote the test262 commit alongside the percentage; without it the
number is not reproducible.

The intl402 denominator changed because the runner was not parsing YAML
list-form `flags:` — roughly half that suite silently never ran, so the old
16.9% was measured against a half-skipped suite.

**30 → 3**, across two steps: closing the original 30, then moving the vendored
suite a month forward (`de8e621c` → `defaaf15`, +96 executions) and closing what
that surfaced. Each was diagnosed against the spec text and reproduced against V8
rather than assumed:

| cause | was | now | how |
|---|---|---|---|
| runner made the *harness* strict | 19 | 0 | harness is a separate realm script; the directive goes on the test text alone |
| `Iterator.prototype.join` unimplemented | 36 | 0 | new helper; the suite is the oracle — V8 24.12 does not have it either |
| `using` at MODULE top level never disposed | 6 | 0 | module-slot binding took the global path, which never emitted `RegisterDisposable` |
| Windows `core.autocrlf` checkout | 3 | 0 | `core.autocrlf=false` + renormalise; `import-bytes` fixtures were inflated LF→CRLF |
| `matchAll` skipped `Get(@@match)` | 2 | 0 | observable `IsRegExp` on the primitive path; matcher-clone fast path bails when `@@match` is patched |
| Annex B `arguments` | 2 | 0 | `"arguments"` blocks the var binding only, never the `SetMutableBinding` |
| upstream test predates the feature | 2 | 0 | closed by the checkout update (`250f204f`) |
| `en`-only CLDR | 2 | 2 | needs real CLDR data; refusing to hand-write one German pattern |
| ES2017 text the spec deleted | 0 | 1 | `block-decl-func-skip-arguments.js` is red *on purpose*; V8 fails it too |

**One of these is unfixable, and worth stating plainly: the suite contains two
tests with opposite expectations.** `block-decl-func-skip-arguments.js` requires
that a block `function arguments(){}` NOT overwrite the arguments object;
`staging/sm/regress/regress-602621.js` requires that it DOES, which is the
current text. So 95,942/95,942 does not exist for any engine; the ceiling is
95,941.

Of the remaining 6: 1 is that contradiction, 2 are CLDR data, and **3 are a real
open engine defect** — module `[[EvaluationError]]` is recorded only on the
synchronous rejection path, so a module that suspends at top-level `await` and
rejects later memoises nothing, and a later `import()` of an already-fulfilled
member of an errored cycle resolves instead of re-throwing. The two `import-defer`
failures sit in the same code. Three other engine bugs were fixed
along the way, and **two of them were tier divergences** — the JIT disagreeing
with the interpreter, which is the failure mode this file has warned about since
the first JIT landed. Both were latent long before this work and reachable from
ordinary `$262.evalScript`; running the harness as a real separate script is what
made them fire:

* **A binding the JIT cannot see.** A script GDI parks `$262.evalScript`'s
  var/function bindings as own properties of the global object and leaves the
  slot `UNINITIALIZED` by design, so the interpreter's own-prop fallbacks govern
  them. Every JIT tier compiles `LoadGlobal` to a bare `mov rax,[r12+idx*8]`.
  A harness function called in a loop therefore worked for the interpreted
  iterations and became `undefined is not a function` the instant the region
  tiered up — at the *same iteration every time*, which reads like a scoping bug
  and is not one. Fixed by giving a prelude ordinary slot bindings; the region
  and leaf-inline planners now also decline any body reading an own-backed slot.
* **`jit_get_prop_miss` indexed the wrong function table.** It read
  `program.functions[func_id]`, but a compiled function can be an *eval*
  function living past `main_func_count` in `eval_funcs`. It panicked —
  `len is 3 but the index is 45` — as soon as such a function got hot and took a
  property miss.

Still open, same family, not currently reachable from the suite: the whole-
function (Tier C) path still reads an own-backed slot directly, so
`$262.evalScript("function f(){}")` called hot from the main program returns
`undefined` at iteration 8. The region and leaf planners decline; Tier C does not.

An earlier version of this table estimated ~45 fixable engine bugs and claimed
~10 executions where "zipp is right, node is wrong" — naming Annex B `arguments`
first. That claim was **false**, and it survived because it was never re-checked
against the spec text. `paramNames` has not been mutated to contain `"arguments"`
since ES2018 (a separate `paramBindings` list carries it), so Annex B's guard
does not fire, and the `SetMutableBinding` at block-declaration evaluation always
runs. node is right; zipp is wrong. The test262 test zipp *passed* here quotes
the removed ES2017 step, which is why V8 fails it. This is the fourth time an
entry in this file was refuted by re-measurement, and the first where the
refuted entry was a claim of superiority. The engine now matches the current
text, and that test is the baselined failure.

### intl402 — 208, and why the number has not moved

It is data, not logic, and that is a statement about SIZE as much as kind. Of
the 104 distinct failing files, **68 reference a non-`en` locale** — `de`, `ja`,
`ar`, `th`, `sr` and friends — so they need CLDR content this engine does not
ship: number patterns and grouping, currency display names, compact notation,
date patterns, unit display names with plural selection, list patterns, plural
categories, collation order. The remaining 36 are `en`-only or locale-agnostic,
and they concentrate:

| cluster | files | what it needs |
|---|---|---|
| `NumberFormat` (grouping, ranges, decimal strings) | 8 | `en-IN` 2,2,3 grouping; range patterns; algorithm, small data |
| `DateTimeFormat` non-ISO calendar formatting | 15 | the formatter can only produce `gregory` — every `-u-ca-` request silently resolves to it |
| Temporal `toLocaleString` (`calendar-mismatch`, `dateStyle`) | 11 | blocked on the same thing |
| assorted (`Segmenter`, `PluralRules` order, `canonicalize-calendar`) | 2 | logic |

The largest single lever is that **`Intl.DateTimeFormat` implements two
calendars while Temporal implements fifteen**. `AVAILABLE_CALENDARS` is
`["gregory", "iso8601"]`, and `Intl.supportedValuesOf("calendar")` reports it
verbatim — which is *honest*, because ECMA-402 defines that list as the calendars
for which the implementation provides `Intl.DateTimeFormat` functionality, and it
does not. The calendar ARITHMETIC already exists in `vm/temporal`; what is
missing is era/month display names and wiring the formatter to a non-ISO
calendar. Doing that closes roughly 40 executions — and cannot be done by
widening the constant, which would make the engine advertise formatting it
cannot perform.

Where a table IS carried it comes from the real upstream source with recorded
provenance and is verified value-by-value against node's ICU — that is the
standard, and approximating it would be worse than the honest failure. Which is
why `staging/sm/String/internalUsage.js` stays red over a single German date:
one hand-written pattern would turn it green and quietly lower the bar.

`tools/test262-expected-failures.txt` is the checked-in baseline; a regression
is a `diff`, not a remembered number. It was stale for a long stretch (the
2,194-line oxc-era list against a 938-failure run), which made that diff
meaningless — regenerate it in the same commit that moves the number.

### Performance — current all-13 cold result 0.5728× Node (2026-08-24, 21 reps)

The authoritative current capture is
`bench/four_engine_cc0d557_pgo_2026-08-24.json`: all 13 **0.572835× Node
[0.569491, 0.576240]**, headline ten **0.785991× [0.782006, 0.790530]**,
diagnostics-only **0.199559× [0.196732, 0.201854]**, and zipp holds the strict
lowest median against Node, Bun, and Deno on every row. All outputs are exact;
the clean PGO source is `cc0d557`. The retained-ten/diagnostic classification is
preserved for historical comparison, not because either set now loses. See
B138 for provenance and the final two mechanisms.

Everything below in this subsection is a retained historical snapshot of how
the campaign moved from 1.86×; its old “at HEAD” language refers to the commit
named beside it, not the current tree.

### Historical performance snapshot — 1.86× at `7c760c1` (2026-07-29, 21 reps)

> **B63/B64/B65 moved it three times in one session**, from a clean tree each
> time — `zipp --version --json` reports `dirty: false` at `7c760c1`
> (`bench/head_clean_7c760c1.json`):
>
> | | node | zipp | ratio | was (B62) |
> |---|---:|---:|---:|---:|
> | regex-log-scan | 469ms | 1709ms | **3.62×** | 4.14× |
> | map-set-heavy | 907ms | 818ms | **0.90×** | 0.97× |
> | async-promise-chain | 342ms | 617ms | **1.81×** | 1.90× |
> | json-large | 292ms | 483ms | 1.66× | 1.78× |
> | geomean | | | **1.86×** [1.85, 1.87] | 1.95× |
>
> `ALL_CORRECT=1`. The suite delta agrees with the isolated `--ab`s (−3.63% and
> −1.06%, with B65 neutral) to within noise, which is the check that the suite
> number and the paired A/Bs are measuring the same thing. `map-set-heavy` is now
> clearly ahead of node rather than at parity, and `regex-log-scan` has gone
> 4.46× → 4.12× → **3.62×** across B60/B63/B64 without the compact-metadata
> project the audit plan called for.
>
> All three of those came from ONE observation — that a side table keyed by heap
> slot was an `FxHashMap` — and none of them is on the audit plan's list.

> **B60 (lazy Annex B legacy statics) moved it before that**, same box, same 21-rep
> protocol (`bench/lazystatics_2026-07-29.json`):
>
> | | node | zipp | ratio | was (B59) |
> |---|---:|---:|---:|---:|
> | regex-log-scan | 451ms | 1842ms | **4.12×** | 4.46× |
> | geomean | | | **1.95×** [1.94, 1.96] | 1.98× |
>
> Every other row inside its B59 interval; `ALL_CORRECT=1`. The row delta agrees
> with the isolated `--ab` (−8.5%) to within noise, which is the check that the
> suite number and the paired A/B are measuring the same thing.


> **RE-MEASURED 2026-07-29 at `799ead6` + the B59 fix**, because the table below
> had gone stale in a way that mattered: `class-prototype-hot` had silently
> regressed to **7.99×** and the suite to **2.38×**, and this file still said
> 1.27× / 1.90×. See **B59** — the cause was one missing arm in a whitelist.
>
> | | node | zipp | cold paired ratio | baseline (regressed) |
> |---|---|---|---|---|
> | map-set-heavy | 709ms | 711ms | 1.00× | 1.00× |
> | class-prototype-hot | 293ms | 378ms | **1.28×** | **7.99×** |
> | markdown-render | 268ms | 447ms | 1.67× | 1.70× |
> | json-large | 265ms | 498ms | 1.88× | 1.88× |
> | polymorphic-objects | 324ms | 604ms | 1.86× | 1.86× |
> | async-promise-chain | 330ms | 639ms | 1.93× | 1.90× |
> | sparse-array | 80ms | 159ms | 1.99× | 2.03× |
> | parse-large-js | 268ms | 596ms | 2.23× | 2.24× |
> | typedarray-math | 202ms | 640ms | 3.16× | 3.17× |
> | regex-log-scan | 451ms | 2010ms | 4.46× | 4.51× |
>
> Geomean **1.98×** (95% CI 1.97×–1.98×), from **2.38×**; startup node 29.8ms vs
> zipp 7.7ms; `ALL_CORRECT=1`. Raw: `bench/final_2026-07-29.json`, with the
> regressed baseline retained beside it as `bench/opt_baseline_2026-07-29.json`
> so the delta is reproducible from artifacts rather than from this prose. The two
> runs' NODE medians agree within 1% on every row, which is what makes the last
> column a fair comparison; a third run mid-session on a loaded box put every row
> ~10% slower on both engines and read 1.92× (`bench/superinline_2026-07-29.json`)
> — same fix, same ratios, different absolute times.
>
> Note what did NOT move: nine of ten rows are inside their old intervals. The
> whole 2.38× → 1.98× is one row. Two rows sit outside the 2026-07-28 table's
> intervals in BOTH the before and after runs — `map-set-heavy` 0.90×→1.00× and
> `regex-log-scan` 4.00×→4.46× — so they predate this work, are unexplained, and
> want an independent session before anything is concluded from them.

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
2. ~~**A profitable compiled regex backend.**~~ **SUPERSEDED by B60 — the matcher
   is the SMALLEST of the four terms in `regex-log-scan`.** Measured with `test`
   only (no result object anywhere), an anchored 5-byte literal that hits at index
   0 costs 197ns against node's 7ns, so ~113ns is a fixed per-call floor, ~85ns is
   success bookkeeping and ~60ns is per capture group; the actual matching is ~4×
   off and, as B8 said, flat in subject length. B58's regular-subset tier moved the
   row 2.82% because it was aimed at that 4×. B60 landed one of the real terms
   (−7.9%) and priced the next (−13.5%, the result array's `index`/`input`/
   `groups` living in the `arr_props` side map). A further 27% of that row's gap is
   corpus generation, which contains no regex at all.
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

> **Build the binary first.** `cargo test --workspace --release` does not build
> `target/release/zipp.exe`; test262 and the benchmarks do. A gate that skips
> `cargo build --release` tests whatever binary is on disk, which is how a fully
> green three-mode test262 run once certified code it had never executed
> (**B108**). `gate.sh` builds first and prints `zipp --version --json`.

Every engine change must pass, in full:

0. **Identity FIRST:** `zipp --version` on both sides, and confirm their
   `sha256` differ. Not optional and not pedantry — B61 records a gate that
   passed while comparing one build against itself, because a `git stash` +
   rebuild cycle never rebuilt after `stash pop`. `--ab` now refuses two
   byte-identical executables outright (exit 1; `--allow-aa` for a deliberate
   A/A, or differing `--ab-env` for an ablation), and `zipp --version` reports
   `<commit>+dirty.<digest>` so a dirty build stops claiming its parent commit.
1. **Build:** `cargo build --release` — verify the binary mtime advanced.
2. **test262, ALL FOUR modes:** `tools/run_test262.py --dump-fails f.txt`, then
   `diff <(sort f.txt) <(sort tools/test262-expected-failures.txt)` — zero new
   entries. Repeat with `ZIPP_NOJIT=1`, with `ZIPP_JIT_THRESHOLD=1`, and with
   `ZIPP_NO_NURSERY=1` (the default collector is generational since W9/B122;
   the majors-only sweep is the cross-check that the two collectors agree).
   The third pass is not redundant: the region JIT compiles only hot LOOPS and
   test262 asserts once, straight-line, so **the default pass never reaches Tier
   C or any JIT-only helper**. B63 found an `arr[oob]` prototype divergence there
   by hand; B65 added the switch and it immediately found a second bug —
   `this.x = 0` globals reading as `NaN` from compiled code — that was live at
   DEFAULT thresholds and invisible to 95,936 executions.
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
interval, exact output, and the full-suite regression check. Confirm a MECHANISM
counter moved, not just wall time — and confirm the row moved in the direction
the change predicts before believing a green correctness gate.

**Code layout is not free in this profile.** Release is fat LTO with one codegen
unit, so adding code reachable from `main` can move hot rows with no runtime
mechanism at all: B61's CLI-only `--version` change measured a replicated
markdown-render +1.5% until `build_identity` was marked `#[cold]
#[inline(never)]`. Mark genuinely cold additions cold, and do not attribute a
~1% move to semantics before ruling layout out.

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

**Status 2026-07-29: 938 → 6 failures (99.0% → 99.994%), and intl402 2,778 →
208 (16.9% → 96.9%).** Every step gated against the checked-in baseline with
zero regressions, both tiers byte-identical, `cargo test --workspace --release`
green at 421. **None of the 5 is a live engine defect** — 2 are fixed upstream in
a test262 commit four weeks newer than the vendored checkout, 2 need real CLDR
data, and 1 is a test encoding spec text deleted in ES2018 that V8 fails too.
Track A is effectively closed on ECMA-262; what is left of the track is intl402,
and that is a data-vendoring project (see §1). What the work actually taught,
beyond the number:

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

> ## ⚠ DO NOT WORK THIS LIST FROM THE TOP. B1–B6 ARE THE 3.31× PLAN AND MOST ARE REFUTED BELOW.
>
> B1–B6 were written when the suite was 3.31×, and their prose still reads as
> actionable — unchecked boxes, effort estimates, "do this first". Nearly all of
> it was later measured and closed **further down this same file**, where a reader
> starting at the top will not look. Audited 2026-07-29 against every later entry:
>
> | item | verified disposition | refuted/landed in |
> |---|---|---|
> | **B1** property-name interning ("do this first") | **REFUTED ×3, CLOSED** — permanent-root, capped, and weak interning each failed for a separately measured reason; suite +0.9%, 7/10 benches slower | B29, **B49** |
> | **B2** for-in enumeration cache | **SUPERSEDED.** The interning half is B29's no-op; the memo half became B48, and what remains is the narrow one-pass level gather | B29, B48 |
> | **B3.1** `ObjMap` → `{shape, vals}` | **PREMISE STALE** — shapes already exist (`shape.rs`); the shape is maintained alongside the vectors, deliberately | `shape.rs` |
> | **B3.2** shape-keyed JIT ICs | **STILL THE REAL ITEM, OPEN** — and the naive form has a fatal bug: `DICT == 0` plus a zero-filled IC table makes an unfilled way MATCH every dictionary receiver and dereference an empty `Vec` | §8 of the external audit |
> | **B4** admit allocation into JIT regions | **REFUTED — built, correct, SLOWER** (`{}` 35→62ns), and separately hits a nesting trap that moves zero regions | **B24**, B50 |
> | **B5.1** widen the `.length` hoist to live-ins | **0ms on every named bench** — `typedarray-math` has no `.length`, and every container is a global the existing hoist already covers | B50 |
> | **B5.2** lazy RegExp result objects | already marked refuted in place; superseded again by **B60**, which priced the real term (the `arr_props` entry, −13.5%) | B60 |
> | **B5.2b** `matchAll` iterator step | **ALREADY LANDED** (`fast0`). Re-measured ~10ms against the ~552ms still written beside it — "the largest phantom in this file" | B50 |
> | **B5.3** builtin dispatch jump table | **REFUTED as a suite lever (B79)** — `ZIPP_BUILTINSTATS=1` counts the dispatches that actually reach the generic chain: `parse-large-js` makes **89**, `json-large` 13, `polymorphic-objects` **0**, `class-prototype-hot` 12, `typedarray-math` 47. `markdown-render`, which this row called "the largest single term", makes 252,669 — ~10ms of a 438ms row at 40ns each, i.e. ≤2.3%, and much of that 40ns is real work. Only two rows light up, and neither wants a jump table: `map-set-heavy` (4.0M, already 0.99x node) and `async-promise-chain` (1.5M, 100% `promise.then` — taken by B79 instead) | **B79** |
> | **B5.4** `JSON.parse` double key allocation | **OPEN**, unrefuted, and independently re-proposed | external audit §7.4 |
> | **B6** generational nursery | **OPEN but hard-gated on B6.0** — two thirds of the apparent "GC pressure" was allocator cost | B6.0, B37 |
>
> **Where to actually start:** the prize-ordered table at the end of **B50**, then
> **B60** for `regex-log-scan` and **B61** for what is already refuted. Note B50's
> own "still open" table has itself aged — entries are newest-first, so B51/B52
> land *after* it, and its top row (accessor inlining declining on `super.v`,
> ~300ms) was fixed by B51/B52, silently regressed by the `SuperBase` whitelist
> gap, and re-fixed in **B59**.
>
> Kept rather than deleted: the reasoning is still the useful record. And the cost
> of NOT having this banner is documented in the file itself — interning was built
> three separate times before B49 closed it ("recorded three times on purpose"),
> and DataView integer admission three times (B22, again per B32's warning, and
> once more in B61) because each attempt reached a different gate. Both are in the
> table above.

Ordered by measured impact per unit of effort, not by the original stage
numbering. **The next sentence is preserved as written and is now FALSE — see the
banner above; B1 is closed and B2 is superseded.**

Original: B1 and B2 are the ones the measurements actually support.

### B1 — Property-name interning — REFUTED THREE TIMES, CLOSED (see B29, B49)

> Title kept for the cross-references. It said "do this first"; do not do it at
> all. B49: "the item is closed, not parked" — permanent-root, capped and weak
> interning each failed for a separately measured reason. The 12.2ns it saves is
> real and simply not reachable by caching at this hit rate. B49's own pointer:
> attack the ~20ns cost of CREATING a heap object (B37) instead.

- [~] **B1.1 REFUTED — see the banner. Original text:** Intern property names to a `u32` id. Replace
  `ObjMap.keys: Vec<String>` with `Vec<NameId>` plus a crate-global interner.
  Removes the per-property `String` malloc on every object construction and
  turns every key comparison into a `u32` compare. ~122 sites touch `.keys`,
  55 of them iterating or indexing it directly — this is the whole cost of the
  change, and it is mechanical.
  **Gain:** should remove ~100 of the ~128 ns/property construction cost;
  helps json / parse / class / polymorphic / markdown simultaneously.
  **Effort:** L. **Risk:** med. **Gate:** `ZIPP_GC_STRESS` mandatory.
- [~] **B1.2 moot (B1.1 closed). Original text:** Re-measure the microbenchmark table in §3 and the ten benches
  before starting B2. B1 alone may change the ranking.

### B2 — `for-in` / enumeration cache — SUPERSEDED (B29 no-op; memo landed as B48)

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

### B3 — Shared hidden classes — B3.1's PREMISE IS STALE (shapes exist); B3.2 is the live item

Only after B1. This is the months-long item; B1 is deliberately structured to
be the first half of it.

- [ ] **B3.1** `ObjMap` → `{ shape: u32, vals: Vec<Value> }` with a shape
  transition tree. **Effort:** XL. **Risk:** high.
- [ ] **B3.2** Shape-keyed JIT ICs: `IcEntry.obj_bits` → `shape_id`, and the
  probe becomes a shape compare. Removes the 8-receiver cliff.
  **Depends:** B3.1. **Effort:** L.
- [ ] **B3.3** Megamorphic stub cache for sites that exceed the way count.

### B4 — Admit allocation into JIT regions — REFUTED: BUILT, CORRECT, SLOWER (B24)

- [~] **B4.1 REFUTED (B24 built it; `{}` went 35→62ns) and separately blocked by a
  nesting trap (B50: moves ZERO regions).** Original text: Admit `NewObject`/`NewArray` (then `MakeClosure`/`CellSet`) in
  `codegen/region_admit.rs`, using the GC-safepoint refetch discipline already
  proven for `StrConcat` in `codegen/region_mem.rs`. A dead literal currently
  blacklists an entire loop permanently. **Effort:** L. **Risk:** high (GC
  safepoints inside a region). **Gate:** `ZIPP_GC_STRESS` mandatory.

### B5 — Contained wins — MIXED: B5.1 is 0ms, B5.2b ALREADY LANDED, B5.3/B5.4 still open

- [~] **B5.1 Widen the loop-invariant `.length` hoist to live-in registers — WORTH 0ms
  ON EVERY NAMED BENCH (B50). Do not schedule.**
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
- [x] **B5.2b `matchAll` iterator step overhead — ALREADY LANDED** as the `fast0`
  path in `proxy_regexp.rs`, and re-measured at ~10ms, NOT the ~552ms recorded
  below. B50 calls this "the largest phantom in this file". Text preserved:
  Measured 1.38 µs per match
  through `for-of matchAll` vs 678 ns through an equivalent manual
  `while ((m = re.exec(s)))` loop — so the iterator path costs ~700 ns per match
  ON TOP of the exec it performs. The `{value, done}` object is already skipped
  by the for-of fast path (`vm/dispatch.rs`), so the cost is inside
  `regexp_string_iter_step` and its `get_index(r, 0)` / `to_str_value` /
  double `regexp_string_iters` hash lookups. This is the one contained regex win
  left; unlike B5.2 it is measured against a control. **Effort:** M.
  **Gain:** the bench's matchAll section is 552 ms of ~1276 ms.

- [x] ~~**B5.3 Builtin method dispatch jump table.**~~ **REFUTED (B79) — do not
  build this.** The premise was that `vm/builtins.rs` and `vm/string_ops.rs`
  resolve `CallMethod` by a chained `match` on `&str`, and that this is "the
  largest single term in markdown-render". The first half is true and the second
  is not. `ZIPP_BUILTINSTATS=1` counts every dispatch that actually reaches the
  generic chain (calls a region intrinsic already serves never get there, which
  is the point):

  | bench | dispatches | ≈cost @40ns | row | share |
  |---|---:|---:|---:|---:|
  | `parse-large-js` | **89** | ~0 | 583ms | ~0% |
  | `json-large` | 13 | ~0 | 441ms | ~0% |
  | `polymorphic-objects` | **0** | 0 | 616ms | 0% |
  | `class-prototype-hot` | 12 | ~0 | 372ms | ~0% |
  | `typedarray-math` | 47 | ~0 | 644ms | ~0% |
  | `markdown-render` | 252,669 | ~10ms | 438ms | ~2.3% |
  | `regex-log-scan` | 750,009 | ~30ms | 1567ms | ~1.9% |
  | `sparse-array` | 137,720 | ~5.5ms | 149ms | ~3.7% |
  | `map-set-heavy` | 3,999,908 | ~160ms | 675ms | ~24% |
  | `async-promise-chain` | 1,500,003 | ~60ms | 625ms | ~10% |

  Eight of the ten rows are at or under 4%, and five make essentially none at
  all. The 40ns unit price is itself measured and real — a builtin WITH a region
  intrinsic runs at or near node (`charCodeAt` 0.5ns, `map.get` 6.5ns, `set.has`
  7.0ns) and one WITHOUT costs 26-45ns — but almost nothing in this suite pays
  it. The two rows that do are not jump-table problems: `map-set-heavy` is
  already 0.99x node, and `async-promise-chain`'s 1.5M calls are 100%
  `promise.then`, which B79 took directly.

  **The finding worth keeping from this probe is a different one.** Comparing
  the tiers, for every builtin WITHOUT a working region intrinsic the JIT is
  *slower than the interpreter* — `str.charAt` 30.0 vs 26.0ns, `str.startsWith`
  44.5 vs 39.0, `arr.indexOf` 45.0 vs 41.0, `Object.keys` 108 vs 95.5 — because
  the region pays the `jit_call_method_ic` round trip (plus the two r13/r14
  refetch calls) on top of the identical shared dispatch. The generic
  `CallMethod` arm has no native inline cache at all, unlike `GetProp` in the
  same file. THAT is the real open item, and it is a codegen item, not a naming
  one.
- [ ] **B82 Inline the target of `f.call(…)` / `f.apply(…)`.** Measured in B80:
  `.call`, `.apply` and a PRE-BOUND function all cost ~60ns against node's
  1-3ns, while the same function called directly costs 2.5ns because B78's
  inliner inlines it. The pre-bound number is the one that localises the cause —
  a bound call never enters `dispatch_builtin_method_inner`, so this is not
  builtin name dispatch, it is `call_value`'s `frames.push` + nested `run_loop`,
  which no inliner reaches. The fix is to recognise the `call`/`apply` shape at
  a `CallMethod` site and inline the TARGET, continuing B74/B76/B78. Suite prize
  is small (`sparse-array` alone, 135,715 calls, ~4% of that row); the
  real-world prize is every polyfill, `hasOwn.call(o, k)` and
  `Array.prototype.slice.call(arguments)`. **Effort:** M. **Gain:** ~4% on one
  row, 20-60x on a ubiquitous idiom.
- [ ] **B5.4 `JSON.parse` allocates every key twice.** `vm/mathjson.rs` collects
  `Vec<(String, Value)>` and then calls `map.set(&k, …)`, which does
  `key.to_string()` again. Subsumed by B1.1, but trivial standalone.
  **Effort:** S.

### B6 — Generational nursery GC — OPEN, hard-gated on B6.0 (allocator, not collector)

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

### B106 — the argument-`Vec` work package was mostly already done

Plan WP-1D asks for an inline argument buffer on the generic call paths. Reading
the tree first, as this file's §14 requires, most of it was already there:

| already at HEAD | where |
|---|---|
| plain user calls never allocated — arguments are copied register-to-register inside the pinned `self.regs` | `setup_call` |
| builtin methods gather into `[Value; 8]` on the stack, heap only above 8 args | `try_builtin_method` |
| method inlining uses a `[Value; 24]` stack window | `run_method_inline` |
| `super.m()` skips `call_value` entirely via its IC | `dispatch.rs` |
| `f.call(…)` slices `&args[1..]` in both implementations | `builtins.rs`, `natives.rs` |
| array/string/promise callbacks pass stack array literals | `array_ops.rs`, `string_ops.rs` |
| `arr.push(x)` has a dedicated fast path that skips the gather | `dispatch.rs` |

And the dependency question was already answered NEGATIVELY: B27 added
`smallvec` once, measured **102ns vs 100ns — nothing**, and reverted it along
with the dependency. So this copies the in-repo `[Value; 8]` idiom instead.

What was left is `with_argv`, applied to the sites ranked by how often this suite
reaches them: `eval_math` (one `Vec` per interpreted `Math.*`, and six of the
thirteen rows call `Math.imul` per element), the namespace-native-as-method arm
and its JIT twin (`Object.keys(o)` in an enumerate loop), and the bound-call
concat, which allocated twice — `bargs.clone()` sizes exactly to the bound args,
then `extend_from_slice` reallocates.

`Vec` exists on these paths for a borrowck reason, not an oversight:
`call_value(&mut self, …, args: &[Value])` cannot be handed
`&self.regs[base + arg_base..]`, because that slice borrows `self` immutably
across a `&mut self` call. A stack buffer sidesteps it; a `Vm`-owned scratch
buffer would not, since `call_value` re-enters the interpreter and a single
shared buffer would be clobbered by the nested call.

**Predicted flat before it was measured, and it is.** B104 had just priced a
small-class mimalloc alloc/free pair at ~3ns, and `Math.imul` inside a compiled
region never reaches `eval_math` at all — `emit_math_op` emits it natively, so
the remaining `Vec` is on the INTERPRETED path only. Measured with B107 in one
A/B, since neither change is expected to move anything: geomean **1.0011x [-0.19%, +0.38%]**,
largest mover `json-large` +1.3% [-0.3, +1.9], every interval spanning zero.

Kept for the reason it was written: it removes work on paths that are hot in
no-JIT and warm-up execution, it costs nothing, and `TailCall`'s unconditional
gather was deliberately left alone — `try_tail_reuse` truncates `self.regs`
before the values are written back, which is a rooting hazard a stack buffer
would need its own argument to clear.

### B107 — the inline-cache probe was written out four times

`GetProp` and `SetProp` each emitted their own 8-way probe in `region_mem.rs`,
and the same two again in `proto_mem.rs`. Comment-stripped, the four `dynasm`
streams are byte-identical: ~140 lines of assembly with the entry layout written
out as literal displacements (`+0`, `+8`, `+16`, `+20`, `+24`) and a literal
stride of 64, in four places, guarded by no static assertion.

They did not stay identical. The store path's `and edx, 0x00FF_FFFF` — which
masks the hop count out of `slot_nhops` before indexing — was once **absent** from
one of them. An unmasked hop count on the read path is a wrong value; on the
store path it is a write to `vals + nhops*2^24*8`. That is a wild write, and it
was fixed one file at a time, with a comment in each explaining why the other one
needed it too.

`emit_ic_probe(ops, IcProbe::Get { dst } | IcProbe::Set { val }, obj, ic_off,
cont)` is now the only place the layout appears. On a hit it reads or writes
`vals_ptr[slot]` call-free and jumps to `cont`; when all eight ways miss it falls
through with `rax` still holding the receiver bits, which is what each caller's
miss-helper sequence passes in `rdx`. The only structural difference between the
two kinds — `Get` walks the guarded proto-chain hops, `Set` does not, because the
miss helper only ever fills OWN ways for a store — is now one `if` instead of two
divergent copies.

Two latent hazards closed with it:

```rust
const _: () = assert!(std::mem::size_of::<IcEntry>() == JIT_IC_STRIDE);
```

Raising `JIT_IC_MAX_HOPS` from 5 to 6 makes `size_of::<IcEntry>()` 72 while
`JIT_IC_STRIDE` stays 64, and every probe then reads each way's fields from the
middle of the previous one — silently, with no type error anywhere in the tree.
One line, checked at compile time, and absent until now.

The region `GetProp` arm also carried a layout note describing a **superseded**
entry: "stride 40, hops at +24/+32, `u32::MAX` = none". The stride is 64, there
are five hop pairs spanning +24..+64, and the count lives in `slot_nhops >> 24`.
All three details were wrong, and the comment sat directly above the code that
indexes by them. `proto_mem.rs` did not repeat the error, which is itself the
argument for one emitter.

**The dead `jmp` was not dead.** Both `GetProp` probes emitted a `jmp => miss`
immediately before `=> miss` — a jump to the following instruction, reachable
only by falling into it, and the obvious thing to delete while factoring.
Deleting it cost `property-ic-shapes` **+1.4% [+1.1, +1.8]** over 21 pairs.
Putting it back made the emitted stream byte-identical to the old build's and the
row returned to **+0.0% [−0.1, +0.2]** over 31.

Nothing else in this change touches a byte of emitted code, so that 1.4% is
entirely the ALIGNMENT of the 8-way probe loop — the hottest loop there is in an
IC-bound workload. Five bytes. It is kept, renamed to what it actually is
(`PROBE_ALIGN_PAD`), with the measurement beside it.

That also makes the neutrality claim provable rather than statistical: with the
pad in, the JIT emits the same bytes it did before, so any residual difference
between the two builds can only come from the Rust binary's own layout — the
fat-LTO confound B77 documented — and not from the probe.

**No behaviour change is the claim, so it is measured.** `tests/ic_probe_shared.rs`
drives all four probes through the states that must invalidate a cached way: own
get/set in a hot region, chain reads at depths 1–3 plus an accessor, seventeen
same-shape receivers against eight ways (the thrash path), a key added mid-loop,
a frozen receiver (strict store throws 50,000 times and the slot keeps its
value), a deleted key shifting slots, a prototype setter routing through
PROP_VIA_IC and frame-calling user code, and a chain method call that compiles as
a whole function so `proto_mem.rs`'s copies are the ones under test. Every result
is identical to node and to the pre-refactor binary under default, `ZIPP_NOJIT=1`,
`ZIPP_JIT_THRESHOLD=1` and `ZIPP_GC_STRESS=1`.

This is step 1 of the native shape-keyed IC. Step 2 is where the real cost sits,
and it is not where the plan for it assumed: **there is no flat shape array.**
The emitter reads a version in one instruction (`mov edx, [r13 + rcx*4]`) only
because `Heap::versions` is a `Vec<u32>` parallel to `objs` with a stable base.
An object's shape lives inside its `ObjMap`, reachable only through
`heap.objs[idx]` — a `Vec<HeapObj>` of 80-byte enums with no guaranteed internal
layout. A native shape guard therefore needs a new index-parallel array *and its
maintenance*, which is the actual content of the `ObjMeta` work.

The maintenance is smaller than it looks, and the reason is worth recording. A
shape-keyed way needs the live `vals_ptr` of whatever receiver matched — it
cannot bake one, since the whole point is that many receivers share the shape.
`vals_ptr` changes exactly when `vals` reallocates, and **"vals reallocated" is
already the thing `bump_version` announces** — the existing IC bakes a
`vals_ptr` and validates it with the version, so "realloc ⇒ version bump" is an
invariant the engine already depends on for soundness. Folding the metadata
refresh into `bump_version` itself makes all 35 call sites correct by
construction rather than by audit.

What does NOT hold is the shape half: a descriptor-only change (`set_attr_at`,
which has no callers, and the direct `attrs` writes that bypass it) alters an
object's shape without necessarily bumping its version. Today that is harmless,
because nothing native reads a shape. A shape-keyed hit would read a **stale**
`guard_shape` and match a receiver whose layout had moved. That is why WP-1A —
routing every descriptor and structural write through one API that owns the
version bump — is a prerequisite and not a tidying exercise.

### B115 — wave 2: DataView on the double tier, accessor ways, and a follow-up that died to its own ablation

Three mechanisms were built in parallel worktrees behind three switches and
measured as one binary; the ablations kept two. Full detail in the registry row.
The method note worth keeping: the 3-mechanism bundle showed `property-ic-shapes`
+8.8% and every instinct said "the accessor way's probe growth" — the per-switch
ablations said otherwise (accessor way alone: −0.5% on that row in the final
binary; the pair-shape MEM lowering owned the damage), and the mechanism that
was BUILT to fix a B113 regression turned out to be un-fixing B113's actual win.
Wall-clock ablation attribution beat mechanism intuition for the second time
(B112 was the first). Final: headline **0.9680x [0.962, 0.974]**,
`typedarray-math` 3.22x → ~2.4x, `polymorphic-objects` 1.83x → ~1.74x, one named
trade (`class-prototype-hot` +1.0%, chain-hit probe growth, follow-up:
site-gated accessor-branch emission), artifact
`bench/wave2_final_2026-08-03.json`. Gate: test262 IDENTICAL (6) ×3 tiers,
13/13 benches byte-identical in default / both-switches-old / NOJIT modes,
full workspace tests green.

### B114 — PGO, measured at last: −13.3% headline from build flags alone

The registry row carries the numbers. What belongs here is the shape of the
mistake that kept this on the shelf: ~113 experiments optimized the engine's
CODE and zero touched how the compiler LAYS IT OUT, while the file itself
documented (B61, B77) that layout moves rows by ±1.5-5% — evidence that
placement was load-bearing, filed as a measurement hazard instead of a lever.
The 1-3% estimate assumed PGO's reach was "the helper-call fraction"; the
measurement says the whole AOT surface — dispatch loop, MEM-tier helpers,
regress exec, JSON, GC — was sitting at branch-prediction and inlining
defaults. Policy from here: `tools/pgo.sh` builds the published binary; every
headline capture retrains first; A/B measurements of ENGINE changes stay on
stock builds (a PGO profile trained on the old code would bias against the new
code's paths).

### B113 — seven verified quick wins in one binary, and the audit that filtered them

The candidates came from a 15-agent audit (7 area finders, 7 adversarial
verifiers, 1 completeness critic) run over this file and the tree together.
30 candidates went in; 6 were killed or reworked BEFORE any code was written,
each by this file's own numbers — KeyStore shape-sharing died on the fact that
`polymorphic-objects` deletes 30 props per object, so the Arc'd key vector
materializes right back on the DICT transition; the for-in gather died on B49's
5.2ns protocol measurement; the INT-tier DataView reserve died on B32's re-run
control. The seven survivors were implemented in parallel worktrees, each behind
its own off-switch, and measured as ONE binary — `--ab-env` with all seven
switches on the old side, 21 pairs — so there is no two-binary layout confound
anywhere in the headline number.

**Suite: headline 0.9661x [0.963, 0.968] (−3.39%), all-13 0.9608x [0.958,
0.962], diagnostics 0.9430x.** Retained artifact:
`bench/wave1_bundle_2026-08-03.json`. Directional headline vs node was
1.8012 × 0.9661 ≈ 1.74x; the clean HEAD capture then measured
**1.7492x [1.737, 1.760]** (`bench/head_clean_819ab45.json`,
`publishable: true`) — `async-promise-chain` 1.68x -> **1.41x**, `json-large`
1.60x -> 1.52x, `sparse-array` 1.68x -> 1.56x, `typedarray-math` 3.13x -> 3.22x
(the named fused trade), diagnostics geomean 4.60x -> 4.33x.

Per-row, with single-row 21-pair ablation attribution:

| row | bundle | attribution (each its own 21-pair ablation) |
|---|---|---|
| async-promise-chain | **−17.6% [−18.3, −17.1]** | promise slot cache −15.4% [−16.3, −13.9]; call_value flat −3.7% [−4.4, −2.2]; dense back-edge −1.7% [−3.3, −0.3]; fused ~0 |
| property-ic-shapes | −7.6% [−8.0, −7.3] | fused cmp+jump (diagnostic row) |
| polymorphic-objects-v2 | −6.6% [−7.0, −5.6] | fused, diagnostic |
| sparse-array | −5.3% [−5.6, −5.3] | fused −3.2% [−3.6, −2.7]; arrkey −0.4% [−1.05, +0.10] |
| json-large | −5.1% [−5.8, −4.6] | leaf −2.9% [−3.9, −2.0]; quote bulk −1.8% [−3.1, −0.6] |
| polymorphic-objects | −3.1% [−3.7, −2.3] | fused −1.5% [−1.8, −1.2]; call_value −0.3% (null); remainder plausibly back-edge on the blacklisted churn loops |
| sparse-array-v2 | −2.9% [−3.5, −2.7] | fused/arrkey mix, diagnostic |
| parse-large-js | −1.0% [−1.7, −0.5] | fused |
| typedarray-math | **+1.5% [+0.6, +1.7]** | fused +1.2% [+0.9, +1.5]; back-edge +0.5% [−0.36, +0.94] (null) |
| class-prototype-hot, markdown-render, map-set-heavy, regex-log-scan | 0 within CI | no mechanism present, as predicted |

Three findings worth their own lines:

* **"Every backend handles the fused ops" was false in four places**, found by
  grepping and READING each site before emitting the first fused op: the
  nested-splice branchy matcher would have spliced a fused jump into a
  flattening that cannot remap branch targets; leaf-inline admission lacked the
  combined forward-target/seen-effect rule; the leaf-inline emitter's catch-all
  was a latent `unreachable!` — a PANIC one whitelist line away; and the fib
  base-case recognizer only knew the two-op shape. All four got the mechanical
  arm first, and a branchy inlined leaf was then demonstrated compiling and
  running through the new `djump_if_not_cmp` arms.
* **The `typedarray-math` +1.2% is named to the level this file requires.**
  ZIPP_JITLOG is line-identical between modes modulo ip shifts (same regions,
  same tiers, same declines — the H-item's new `[decline-reason]
  regalloc-emit-unhandled: Mod` shows the same [16,47] region falling to MEM in
  both modes); ZIPP_PROF splits 88.5/11.5 jit-mem/jit-fast in both modes; and
  the double tier's fused arm is strictly SHORTER than the old pair (the pair
  cost `ucomisd` + `setcc`-to-bool-home + flag-fused `jbe`; the fused arm is
  `ucomisd` + `jbe`). The cost therefore sits in MEM-tier execution of
  `djump_if_not_cmp` vs the old Lt-helper + test, or in placement. Follow-up
  recorded: emit the pair shape for fused guards on the MEM tier only, and take
  the +1.2% back.
* **The array-key canonicality item is a mechanism-only land** (B104's class):
  −0.4% [−1.05, +0.10] on its target row. The finder's arithmetic was already
  cut once by its verifier and still overstated — the honest reading is that
  sparse-array's for-in phase gap is construction/allocator substrate, exactly
  where the audit's critic said this row's residual lives. Kept for the
  allocation removal, the `in`-operator stack-buffer spelling, and the test
  battery, which found a PRE-EXISTING bug: **`delete a["05"]` deletes element
  5** — the delete machinery parses the key non-canonically, both tiers agree,
  node keeps the element and deletes nothing. Recorded here, not fixed.

Gate: test262 **IDENTICAL (6) on all three tiers** (default / `ZIPP_NOJIT=1` /
`ZIPP_JIT_THRESHOLD=1`, ~96k executions each); all 13 benches byte-identical to
node in default, all-switches-old, and `ZIPP_NOJIT=1` modes; full workspace
tests green in both switch modes during development and on the merged tree.
Off-switches: `ZIPP_NO_PROMISE_SLOT_CACHE`, `ZIPP_NO_CALLVALUE_FLAT`,
`ZIPP_NO_DENSE_BACKEDGE`, `ZIPP_NO_JSON_LEAF_FAST`, `ZIPP_NO_JSON_QUOTE_BULK`,
`ZIPP_NO_FUSED_CMPJUMP`, `ZIPP_NO_ARRKEY_FAST`.

### B112 — a cheaper way-0 probe: REFUTED, after a 3x bug that nothing but wall time saw

B111's closing suggestion was that since eight of the ten headline rows take zero
native property misses, every property access they make is a probe HIT, and the
lever is therefore the instruction count of a hit rather than the miss rate. Built,
measured, refuted.

The probe walked ways in a loop, so a MONOMORPHIC site paid loop scaffolding to
iterate exactly once, and asked "is this an own entry?" in four instructions.
Way 0 inlined, plus `nhops == 0` rewritten as `slot_nhops <= 0x00FF_FFFF` (the
slot is the low 24 bits, the hop count everything above), takes a way-0 own hit
from **19 instructions to 16**:

```
mov  rax, [rbx+dreg]        mov  rax, [rbx+dreg]
lea  r9, [r14+off]          lea  r9, [r14+off]
mov  r8d, 8                 cmp  rax, [r9]
cmp  rax, [r9]              jne  slow
jne  next                   mov  ecx, eax
mov  ecx, eax               mov  edx, [r13+rcx*4]
mov  edx, [r13+rcx*4]       cmp  edx, [r9+16]
cmp  edx, [r9+16]           jne  slow
jne  next                   cmp  DWORD [r9+20], 0x00FFFFFF
mov  ecx, [r9+20]           ja   slow0
shr  ecx, 24                mov  rcx, [r9+8]
test ecx, ecx               mov  edx, [r9+20]
jz   hit                    and  edx, 0x00FFFFFF
mov  rcx, [r9+8]            mov  rax, [rcx+rdx*8]
mov  edx, [r9+20]           mov  [rbx+dreg], rax
and  edx, 0x00FFFFFF        jmp  cont
mov  rax, [rcx+rdx*8]
mov  [rbx+dreg], rax        16
jmp  cont
(+5-byte align pad)

19
```

**Result: nothing.** 21 pairs, one binary, `ZIPP_NO_IC_FAST0=1` as the other side:
geomean **0.9986× [−0.51%, +0.33%]**. `property-ic-shapes` −0.9% [−1.3, −0.4] and
`class-prototype-hot` **+1.0% [+0.4, +2.2]** — a row moving the WRONG way with an
interval excluding zero, which §14 alone would reject it on. Three instructions
off the hottest property path in the engine, and the suite cannot tell.

Reverted. `ZIPP_ICSTATS` (B111) stays; the mechanism does not.

**The part worth keeping is how the first version failed.** A way-0 CHAIN entry
(`nhops > 0`) took the `ja` exit, and the loop it fell into started at **way 1** —
because way 0 had "already been tested". It had not: only its own-property case
had. A chain entry in way 0 became permanently unreachable, so every access it
should have served scanned all eight ways and called the miss helper instead.

| | |
|---|---:|
| `class-prototype-hot` | **+221%** (376 → 1209ms) |
| `polymorphic-objects-v2` | **+96%** |
| `property-ic-shapes` | +7.2% |
| output on all 13 benches | **byte-identical** |
| the 8-state IC differential vs node, 4 modes | **passed** |
| `ZIPP_JITLOG` decline/compile histogram | **unchanged, line for line** |
| `ZIPP_PROF` | **100.0% jit-mem** — same tier, same region |

Everything that could have caught it said the code was fine. It was fine — it
computed the right answers, in the right tier, from the right regions. It was
just doing it through the miss helper several million times. **The only instrument
that saw it was the wall clock**, which is the fourth time this session
(B97, B102, B108, now this) that the sole detector of a real defect was a number
that could not be right.

The diagnosis took one bisect, not a theory: restarting the loop at way 0 instead
of way 1 made the regression vanish entirely (+1.4% / −1.0%), which named the
skipped way as the cause and refuted two microarchitectural guesses (disp32
addressing, code size) that had already been built and measured to no effect.

The corrected form uses two loop entry points — way 1 when way 0 failed on
identity or version (it cannot hit in the loop either), way 0 when it failed only
because it is a chain entry. That is the version measured above at neutral. It is
still reverted, because neutral is not a reason to carry a second emitted path
through the hottest loop in the JIT.

**What this leaves.** The retained ten have no property lever left that is about
the CACHE: not the miss rate (B111: zero misses), and not the hit's instruction
count (this). What remains is B101's version of the question — reduce the NUMBER
of heap operations, not their per-op cost — which is the optimizing tier, not the
inline cache.

### B111 — the shape-keyed IC is worth ZERO on the retained ten, measured before building it

Plan Phase 2 — a native shape-keyed own-property inline cache — is its largest
single item and the substrate its Phase 4 property lowering depends on. It is
also the third "Large" mechanism this file has been asked to build; B99 and B101
each priced one and refuted it before a line was written. This one prices in a
single measurement, because the engine already contains the oracle.

**The oracle.** `vm.jit_shape_slot` is a `(site, shape) -> slot` memo consulted at
the top of `jit_get_prop_miss`. A hit means the slot was determined by the
receiver's LAYOUT, not its identity — which is exactly what a shape-keyed way
decides. What the memo cannot do is remove the CALL: by the time it runs, the
region has already executed eight failed 64-byte-strided compares and an
`extern "win64"` call. **An emitted shape way would have served that same access
with no call at all.** So the memo's hit rate IS the fraction of native property
misses the emitted probe converts to call-free hits. `ZIPP_ICSTATS=1` counts it.

| bench | GetProp misses | shape-known | shape-new | DICT | SetProp misses |
|---|---:|---:|---:|---:|---:|
| async-promise-chain | **0** | — | — | — | **0** |
| class-prototype-hot | **8** | 2 | 6 | 0 | **0** |
| json-large | **0** | — | — | — | **0** |
| map-set-heavy | **0** | — | — | — | **0** |
| markdown-render | **0** | — | — | — | **0** |
| parse-large-js | **0** | — | — | — | **0** |
| polymorphic-objects | 1,250,016 | **0 (0.0%)** | 1,250,016 | 0 | 250,006 |
| regex-log-scan | **0** | — | — | — | **0** |
| sparse-array | **0** | — | — | — | **0** |
| typedarray-math | **0** | — | — | — | **0** |
| *polymorphic-objects-v2* | 4,997,418 | **4,997,398 (100.0%)** | 20 | 0 | 0 |
| *property-ic-shapes* | 45,917,773 | **21,917,735 (47.7%)** | 12,000,026 | 12,000,012 | 12,001,065 |

**Eight of the ten headline rows take ZERO native property misses.** Not few —
none. Their eight ways serve every access call-free already, so there is nothing
for a ninth, shape-keyed way to convert. `class-prototype-hot` takes eight misses
in an entire run.

The one headline row with misses cannot use them. `polymorphic-objects` takes
1.25M GetProp misses of which **0.0% are shape-known**, and its own file header
explains why before the measurement does: it reads `shapes[i & 7]` — exactly
eight receivers against `JIT_IC_WAYS == 8`, so the ways always fill and never
thrash — and two of those eight are an ACCESSOR-backed layout and an
`Object.create` proto-chain object. Both return early from the miss helper
(`PROP_VIA_IC` / the chain walk) and never reach the memo fill, so the same
receivers miss forever. An own-property shape way cannot serve either of them **by
construction**: the plan itself keeps accessors and prototype chains on
identity/version guards, because a receiver's own shape does not identify its
prototype or prove the chain is unchanged. Its 250k SetProp misses are the
accessor's setter, for the same reason.

The two DIAGNOSTICS behave exactly as designed. `polymorphic-objects-v2` is
**100.0% shape-known** — every one of 5.0M misses becomes a call-free hit.
`property-ic-shapes` is 47.7%, with the remaining 52% split between layouts the
site has not yet seen (26.1%, which a shape way would also miss and then fill)
and DICT receivers (26.1%, which no shape can ever guard).

**So Phase 2 is a diagnostics-only mechanism, and this is now measured rather
than predicted.** The plan hedged it correctly — "the retained ten currently have
little direct exposure… judge this work first on diagnostics and mechanism
activation, not on an invented geomean promise" — and the number behind that
hedge is **zero**, not "little". Building it will move `property-ic-shapes`
(5.53×) and `polymorphic-objects-v2` (3.61×) and the headline geomean by
**0.00%**.

That is not an argument against building it. It is an argument against building
it *for the geomean*, and for stating the real reason: it is the substrate for
lowering `GetProp`/`SetProp` into an optimizing tier (plan milestone O3), where
the win is removing the property operation rather than caching it better.

**A second reading, which is the more useful one.** Zero misses on eight rows
means property access on the JIT path in those rows is ALREADY call-free — the
cost there is the ~19-instruction probe itself, every time, not the miss. B101
priced an inline monomorphic access behind a shape check at 3–4 instructions
against that. The lever on the retained ten is therefore **making a way-0 hit
cheaper**, not adding a ninth way: a site with one receiver still pays the loop
setup (`lea r9`, `mov r8d, 8`) and the hop-count test on every access. That is
the next thing to price, and it is a much smaller change than Phase 2.

`ZIPP_ICSTATS=1` costs one relaxed atomic load per miss when off — on a path that
has already made a call, and which eight of the ten rows never take at all.

### B110 — the shape invariant is now checked, and it was wrong in one place

B107 named the prerequisite for a native shape-keyed inline cache: an object's
recorded shape must always describe its actual layout, and **nothing checks
that** because nothing native reads a shape yet. A stale shape is invisible
today and is a call-free read of the wrong slot the moment a probe guards on one.

**The audit is much smaller than plan WP-1A ("centralize object mutation")
suggests, and the reason matters.** Structural mutation is ALREADY centralized:
nothing outside `heap.rs` pushes to, removes from, truncates, drains or
`iter_mut`s `keys`/`vals`/`attrs`. What escaped is IN-SLOT mutation — 20 raw
writes across 8 files — and of those:

| kind | count | shape effect |
|---|---:|---|
| `vals[i] = v` | 19 | **none** — same keys, same descriptor bits, no reallocation |
| `attrs[i] = a` | **1** | **changes the shape**, and said nothing |

So the hazard is one line. `eval_prog.rs`, hoisting a redeclared `var` onto
`globalThis`, wrote `m.attrs[i] = attr` directly. `ObjMap::set_attr_at` exists
precisely to make that a transition — it drops the object to `DICT` when any
descriptor bit actually changes — and had **zero callers anywhere in the tree**.
The raw write left `globalThis` claiming a shape that lied about its own
descriptor bits, harmless only because `ic_obj_ok` bans `global_this` from every
cache. That is an accident of an exclusion list, not an invariant.

It now goes through `set_attr_at` and is paired with a version bump. Two adjacent
gaps closed with it: `%Array.prototype%.length = n` removes keys (shifting every
slot after them) without bumping, and thirteen whole-`HeapObj` overwrites in
`construct.rs` — a subclass `super()` replacing its plain object with a Map, Set,
Promise or cloned Array — freed the old `vals` buffer without bumping. Both were
safe by argument (an exclusion list; "no cache has seen this object yet"), which
is not the same as safe by construction. `Heap::replace(idx, obj)` now owns the
second.

**The check.** `shape::describe(shape)` returns what a shape CLAIMS —
`(key, attr_bits)` per slot — and `ObjMap::verify_shape()` compares it to what
the object holds. `ZIPP_SHAPE_VERIFY=1` sweeps every live object at every
collection and **panics** on the first disagreement, naming the slot and the key.
A desync then surfaces at the first GC after the write that caused it rather than
as a wrong answer much later.

The distinction from the test helper that already existed is the point.
`assert_shape_agrees` checks key → slot agreement, which is what a guard needs to
read the RIGHT slot. It says nothing about descriptor bits — and those are part of
a shape's identity, deliberately, because two objects whose `x` differs in
enumerability do not have interchangeable layouts for a descriptor read. **The one
raw write in the tree was exactly in that blind spot**, and a test that pins the
verifier catching it is included, because this file's standing rule is that a
green check proves nothing until it is shown to go red.

**Evidence.**

| run | result |
|---|---|
| all 13 benchmarks, verifier on | every one OK |
| test262, verifier on, ~96,000 executions | **IDENTICAL (6)** — the expected-failures list unchanged |
| `cargo test --lib` | 344 passed (5 new) |
| full gate (39 suites + test262 x3) | green |
| 21-pair suite A/B vs the previous build | geomean **1.0012x [-0.28%, +0.66%]**, every row's interval spanning zero |

Off, the verifier costs one relaxed atomic load per collection. On, it is
O(live objects) per collection and unusable with `ZIPP_GC_STRESS=1` — the two
together take longer than the benchmark suite, which is the expected shape for a
whole-heap invariant sweep and worth saying so nobody assumes the combination was
run.

**What this does NOT do.** The three vectors are still `pub`, so a future raw
`attrs[i] = a` is still writable — it is now merely detected rather than
prevented. Privatising them means converting 164 external `.attrs` uses to
`attr_at(i)`, a large mechanical diff with no performance content, and the
verifier catches the same class at a fraction of the cost. Recorded as the
cheaper option taken deliberately, not as the job being finished.

Next is `ObjMeta`: an index-parallel `(version, guard_shape, vals_ptr)` array
replacing `Heap::versions`, refreshed inside `bump_version` so all 35 of its call
sites are correct by construction, with the same whole-heap sweep extended to
check the metadata against the objects it describes.

### B109 — the first capture this repo can attribute to a commit

`bench/head_clean_e839613.json`, `publishable: true`, engine
`e8396130cb1fa4c8716a6c03537bd94229592c41` clean and equal to the workspace HEAD
before AND after measurement, binary hash unchanged across the run, Node
v24.12.0, 21 counterbalanced pairs, every output byte-identical.

| row set | geomean | 95% CI |
|---|---:|---|
| **headline (the retained ten)** | **1.8012×** | [1.790, 1.812] |
| diagnostic (three) | 4.5981× | [4.542, 4.617] |
| all thirteen | 2.2361× | [2.222, 2.247] |

| bench | node | zipp | ratio |
|---|---:|---:|---:|
| map-set-heavy | 858ms | 678ms | **0.79×** |
| class-prototype-hot | 291ms | 377ms | 1.30× |
| json-large | 276ms | 449ms | 1.60× |
| async-promise-chain | 339ms | 566ms | **1.68×** |
| sparse-array | 79ms | 131ms | 1.68× |
| markdown-render | 269ms | 456ms | 1.70× |
| polymorphic-objects | 323ms | 598ms | 1.86× |
| parse-large-js | 270ms | 604ms | 2.23× |
| typedarray-math | 203ms | 633ms | 3.13× |
| regex-log-scan | 452ms | 1593ms | 3.53× |

**The headline did not move, and that is the honest result.** The directional
figure it replaces was 1.798×; this is 1.801×. B105 took
`async-promise-chain` from 1.775× to **1.68×**, which is worth about −0.55% of a
ten-row geomean on its own — and it was cancelled by Node itself getting **6–12%
faster on four rows** between the two captures (`map-set-heavy` 972 → 858ms,
`markdown-render` 300 → 269, `parse-large-js` 289 → 270, `async` 363 → 339).
Nothing in zipp regressed to produce that; the denominator moved.

Which is exactly why §2.5 says to pin the Node version and the executable hash.
"V8 parity" measured against a moving V8 is a moving target, and this capture is
the first one in the file that records which V8 it was measured against.

**What one row is worth, concretely.** Ten rows, geometric mean: a row improving
by factor *f* moves the headline by *f*^(1/10). B105's −5.4% on the async row is
−0.55% of geomean. To reach 1.0× from 1.80× needs a **44% reduction**, i.e. the
product of all ten improvements must be 0.56. Nine of ten rows going to Node
parity leaves the geomean at the tenth's ratio^(1/10) — so there is no
combination of contained fixes that gets there, and B101 already priced the
finished tier programme at ~15%.

The two worst rows remain `regex-log-scan` 3.53× and `typedarray-math` 3.13×,
and taking BOTH to exact parity yields 1.41×, not 1.0×. That arithmetic has been
in this file since §3 and every capture reconfirms it.

### B108 — the gate reported IDENTICAL for code it had never run

`cargo test --workspace --release` builds the LIBRARY and the test harnesses. It
does **not** build `target/release/zipp.exe`, because nothing under test depends
on the `zipp-cli` binary. The gate script ran `cargo test` and then pointed
`run_test262.py` at `./target/release/zipp.exe` — whatever happened to be on
disk.

On 2026-08-01 what was on disk was a build from a `git stash` cycle: the
pre-change binary, made while producing the "old" side of a two-binary A/B. The
gate then reported

```
default  IDENTICAL (6)
nojit    IDENTICAL (6)
thr1     IDENTICAL (6)
```

— three modes, ~96,000 executions each, all green, **for a build that did not
contain the change being gated**. B104 and B105 were pushed on it.

The tell was not in the gate. It was in the next A/B: two binaries that should
have differed only by an argument-buffer change and a codegen refactor reported
`async-promise-chain` **−6.8%**, which is B105's number, in a comparison where
B105 was supposed to be on both sides. A row moving by exactly the amount of a
change neither side contained.

`gate.sh` now runs `cargo build --release` first and prints
`zipp --version --json`, so the gate states which source it is about to test. The
same reasoning is why B103 made the benchmark harness refuse to measure a binary
it cannot attribute — **this is that bug, in the correctness gate instead of the
performance harness, found six hours later.** The harness had been taught to
check; the gate had not.

Two lessons this file already had, restated because neither was enough:

- *"A green differential proves nothing until the mechanism is shown to have
  run"* (B94). Extend it: a green GATE proves nothing until the BINARY is shown
  to be the one built from the tree under test.
- Every genuine defect this session surfaced as a number that could not be right
  (B97's flush bug, B102's entry-bail, and now this), never as a failing check.

### B105 — a promise's two reaction vectors were two halves of one record

`async-promise-chain` **604ms → 564ms, −6.7% [−7.7, −6.4]**, replicated at
**−7.6% [−8.7, −6.9]**. The largest single-row win in this file after B80.

Every site that registers a promise reaction registers BOTH handlers at once,
with the same `dependent`, the same `finally` and the same `is_async` — they
differed only in the callback. `.then(f)` supplies `f` and a pass-through
rejection; `.catch(g)` the reverse; `await` a pair of async resumes. There are
exactly two such sites (`then_internal`, `settle_subscribe`).

They were stored in `fulfill: Vec<Reaction>` and `reject: Vec<Reaction>`. So the
single-subscriber promise — a chain link, an `await`, every `Promise.all`
element, which is nearly all of them — allocated **two** first buffers to hold
two halves of one record:

```
[async] 1530004 promise subscriptions  1530004 inline (100.0%)  0 spilled to a Vec
```

**3,060,008 allocations removed on that row, and not one subscription needed a
`Vec`.** `Reactions::{None, One(pair), Many(Vec<pair>)}` holds the common case
inline.

**Why this is 7% when B104's 1.53M removed allocations were ~1%.** Same row, same
order of magnitude of allocations, seven times the effect — because these are not
the same cost. B104 recycled a buffer that was already the right size, saving a
malloc/free pair (~3ns). B105 removes the buffers themselves: two `Vec` headers
(48 of the Promise payload's ~58 bytes) stop being written per promise, the
payload drops 64 → 48 bytes, and a settled promise stops carrying two heap
pointers the collector must trace. The win is **the objects that no longer
exist**, not the allocator calls. That is the same distinction B81 drew when it
found `[]` costs 24.5ns while mallocing nothing at all.

**Order is the whole correctness surface**, and merging changes how it is
represented — from structural (drain the matching vector, leave the other) to a
field selection per pair. `promise_reaction_pairs.rs` pins the eleven ways that
can break: the `One → Many` upgrade keeping the first registration first,
fulfil/reject handlers registered alternately draining in registration order,
`undefined` handlers forwarding value and reason (the spec's Identity/Thrower
defaults are not function objects here), `finally` forwarding the original
completion, subscribing after settlement, a reaction subscribing to its own
promise mid-drain, `await` on both settlements, the combinators' shared
dependent, GC over a pending pair's two closures, and a subclass promise built
from the variant literally in `construct.rs`. A 39-outcome ordering differential
covering the same ground plus thenables and heavy GC pressure is byte-identical
to node under default, `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1` and
`ZIPP_GC_STRESS=1`, on BOTH binaries.

One of those expectations was wrong when written — `finally` that throws prints
before the two pass-through forwards, not after, because it rejects its dependent
from inside the tick that ran it. Node prints exactly what zipp prints. The test
now records why.

**A retention leak went with it, not separably.** Settlement used to `mem::take`
only the MATCHING vector; the opposite-kind reactions stayed on the promise for
its whole life and `gc.rs` kept marking their callbacks and dependents. One list
cannot express "half drained", so merging clears both. That is strictly less
retention, and it is why this entry does not claim to be only an allocation
change.

**No suite regression.** 21 pairs, all 13 rows: headline geomean **0.9939×
[0.988, 0.997]**. The largest unrelated movers are `sparse-array-v2` +0.8%
[+0.3, +1.3] and `polymorphic-objects-v2` +0.6% [+0.1, +1.6], both under §14's
+2% rule and both in rows containing no promise at all — the two-binary fat-LTO
layout confound B77 documented. `sparse-array-v2` reproduced at +0.5%
[+0.1, +0.9] and `json-large` at +0.4% [−1.4, +1.3].

Measured as a two-binary A/B because a data-layout change has no `--ab-env`
form: keeping both representations alive to switch between would itself be the
confound.

`Reaction.finally` survives untouched and is currently DEAD — nothing in the tree
sets it, because `Promise.prototype.finally` went generic through
FINALLY_THEN/FINALLY_CATCH bound natives. It is kept rather than removed so this
entry is one mechanism; a fast `.finally` lane that bypasses `then` would want it
back.

### B104 — one malloc and one free per `await`, for a buffer already in hand

Every resume of a suspended async activation DETACHES its parked register window
with `mem::take` — a move, no allocation — memcpys it onto the live register
file, and runs. Every re-suspension then called `Vec::split_off(new_base)`, which
**allocates a fresh right-sized `Vec`** and memcpys the tail into it. The buffer
the resume had detached went out of scope unused and was freed.

The two are the same size: an activation's window length is fixed by its
`reg_count`. So the steady state was a malloc and a free per suspension for a
buffer that was sitting in a local variable. `clear` + `extend_from_slice` keeps
the capacity and does the identical memcpy.

Five suspension points share the shape and all five now go through one
`repark_window`: `drive_async` (async functions), `drive_async_gen` at both its
yield and await parks, and `gen_resume` (sync generators). The two INITIAL parks
(`alloc_generator`, `alloc_async_generator`) keep `split_off` — there is no
detached buffer to recycle at a generator's birth.

The length comes from the LIVE file (`self.regs.len() - new_base`), not from the
recycled buffer, because a `finally` running between two suspension points can
leave the window at a different height than the one that was restored.

**Mechanism, on `async-promise-chain`** (`ZIPP_ASYNCSTATS=1`):

```
1530000 window re-parks   1530000 reused (100.0%)   0 grew   26550000 values copied
```

1.53M allocation/free pairs removed, 17.35 values per window, and not one window
outgrew the buffer it was handed.

**And it is worth about 1%.** Two independent runs:

| run | rows | `async-promise-chain` | control |
|---|---|---|---|
| 21 pairs, all 13 | 13 | **−0.7% [−1.3, +0.2]** | suite −0.32% [−0.60, +0.05] |
| 41 pairs, 2 rows | 2 | **−0.9% [−1.3, −0.2]** | `json-large` −0.6% [−1.8, +1.1] |

It reproduces, and the second run's interval excludes zero. But look at the first
run's other rows: `map-set-heavy` −0.9%, `polymorphic-objects-v2` −1.0%,
`markdown-render` −0.7% — rows with no `await` in them, moving as much as the row
that has 1.5 million. **The async row is not distinguishable from the drift
floor**, which M0.1 measured at ~1% and which reversed a nominal −0.4% into +1.1%
on an identical binary. `json-large` as an explicit no-promise control in run 2
moved −0.6% with an interval spanning zero.

So: below §14's 2–3% promotion bar, and landed anyway on the same footing as B78
and B92 — the mechanism provably fires, it removes work rather than adding a
path, and there is no branch that can be slower. What it is NOT is a
demonstrated 1% win, and the entry should not be read as one.

This also prices B81's finding for this case: 1.53M alloc/free pairs against a
row that moved ≲1% puts a small-class mimalloc pair at **~3ns**, not the ~7ns B81
attributed to the malloc inside a `{}`. Removing allocations one call site at a
time is not a route to parity; that arithmetic is why WP-1C targets the
allocation COUNT per promise rather than another buffer.

`ZIPP_NO_BUF_REUSE=1` restores `split_off` at every suspension — the rollback
switch, and the two sides of the one-binary A/B (fat LTO makes a two-binary
comparison a layout confound; see B77). `ZIPP_ASYNCSTATS=1` prints the counters.

### B103 — the harness could name a commit it had never measured

`README.md` cites `bench/head_clean_2a616f5.json`. That artifact records
`git_commit: 2a616f5…` and its zipp engine reports
`commit: cdda4e8…, dirty: true, diff_digest: a8cbe062…`. cdda4e8 is 2a616f5's
PARENT. The file named "head_clean" was measured by a binary built from a dirty
tree one commit behind the name it carries.

Not a slip — a missing check. `tools/bench.py` collected provenance from two
independent sources, both at the very END of `main()` and both only when
`--json` was passed:

| source | function | lands at |
|---|---|---|
| the WORKSPACE's HEAD | `git_revision()` | `git_commit` |
| the BINARY's own account | `build_identity()` | `engines[i].build_identity` |

Nothing compared them. `build_identity` appeared exactly twice in the file: its
own `def`, and the dict key it fills. Sweeping all 57 retained artifacts found a
second disagreement (`overlay_narrow_ab_2026-07-29.json`) and only **two**
artifacts that were ever clean.

**The gate is asymmetric on purpose.** A HEADLINE capture claims to measure a
commit, so it must: identity present, tree not dirty, engine commit == workspace
HEAD, and neither the binary hash nor the reported source may change between the
probe before the first measurement and the probe after the last. Failing any of
those is fatal before a single benchmark runs by default. **Correction found on
2026-08-24:** `--allow-dirty-engine` / `--allow-nonhead-engine` suppress the
corresponding rejection reasons and can leave the artifact `publishable: true`;
an overridden artifact therefore requires manual provenance audit.

An **A/B is never blocked**, because two of its rules would reject the protocol
rather than a mistake: an A/B compares two builds that by construction cannot
both be HEAD, and this repo's most-used ablation idiom is ONE binary with two
`--ab-env` sides, which reports the same source on both sides deliberately. The
source identities remain recorded, but the override bug means `publishable`
alone does not classify that artifact. The rebuild-that-did-not-happen case an
A/B *does* need is still caught by
`reject_identical_ab_binaries` on the binary hash, and now also on the reported
source when the two binaries differ but the tree did not.

**The row sets moved out of a person's head.** The retained-ten/diagnostic-three
split lived only as shell variables in `bench/run_real.sh`; `bench.py` globbed
all thirteen `.js` files and printed one geomean. The three diagnostics are
3.5–5.5× rows, so that number ran about **0.43× high** and was not the historical
series. `HEADLINE_BENCHES` and `DIAGNOSTIC_BENCHES` are now constants in the
harness, every artifact carries `headline_benches` / `diagnostic_benches` /
`unclassified_benches`, and both geomeans are computed with their own bootstrap
CI and printed.

Also added: `workspace_source`, `engine_source_before` / `engine_source_after`,
`engine_binary_sha_before` / `engine_binary_sha_after`, `publishable`,
`provenance_reasons`, `engine_drift`, a sha256 of the harness itself, and a
sha256 of every benchmark program actually run.

17 new tests (45 total) cover clean match, dirty build, non-HEAD build, the exact
`head_clean_2a616f5` shape, identity drift, binary drift, deliberate A/A,
deliberate A/B, `--ab-env` on one binary, each override in isolation, and the row
sets. Provenance had zero test coverage before this.

Verified on the tree as it stood: the gate refuses a headline run against the
current (dirty) binary, and the `--ab-env` ablation still measures.

No engine code changed. The point is narrow: **no performance claim in this file
can be attributed to a commit unless the harness proves the binary came from it**,
and until now it could not.

### B102 — B95 shipped a 19× pathology, and the benchmarks could not see it

Found while pricing inline property access. The control micro — the SAME loop
with the property reads *removed* — ran **four times slower** than the one with
them:

```js
for (i = 0; i < 20000000; i++) { o = objs[i & 63]; s = s + o.a + o.b + o.c; }  // 509ms
for (i = 0; i < 20000000; i++) { o = objs[i & 63]; s = s + 1.5 + 2.5 + 3.5; }  // 2047ms
```

Removing work made it slower, which is always a compiled-tier accident. It was:
`ZIPP_JITLOG` shows the second loop compiling a **DOUBLE region**, then
`deopt at ip 33` — the region header — on every entry, self-evicting, displacing
the memory compile that had been working, and ending **100% interpreted**.

**The cause is B95, landed earlier tonight.** It admitted a dense-Array
`GetIndex` to the double tier on `is_arr_pin(k)`, which matches `ARR_PIN_KIND`:
**any** dense array, including an array of OBJECTS. The element's dst then gets a
numeric home, and `live_in_regs` entry-loads every numeric home (deliberately —
so an early exit flushes the frame's own value rather than garbage). That entry
load sees the previous iteration's object, `emit_box_to_home` rejects it, and
`entry_bail` fires before the loop body ever runs.

The INT tier never had this problem because it requires `ARR_INT_PIN_KIND`, a
bounded plan-time SAMPLE of the elements. **B95 took the emitter idiom and
dropped the sampling hint that makes it safe** — and the entry in this file even
quotes the reason it exists ("Sampling keeps a known-double array from compiling
INT and then deopt-thrashing to eviction").

**The fix is a third pin kind, not a narrower one.** The first attempt restricted
the double tier to `ARR_INT_PIN_KIND`; that killed the pathology but cost
`sparse-array-v2` **+6.2% [+0.9, +13.9]**, because it also excluded arrays of
DOUBLES, which the double tier can host perfectly well and was legitimately
promoting. So `ARR_NUM_PIN_KIND` samples for all-NUMBER (Int or double) with the
same bounded 64-head-plus-64-stride walk, and sits between the two existing kinds:

| kind | sample | tier that can host it |
|---|---|---|
| `ARR_INT_PIN_KIND` | all Int | INT (i64 homes) |
| **`ARR_NUM_PIN_KIND`** | **all number** | **DOUBLE (f64 homes)** |
| `ARR_PIN_KIND` | none | memory only |

| shape | B95 | first fix | B102 |
|---|---:|---:|---:|
| array of OBJECTS (the pathology) | 2349ms | 181ms | **124ms** |
| array of doubles | promoted | *declined* | **promoted** |
| `sparse-array-v2` vs loose | — | **+6.2%** | **−0.1%** |

Suite A/B against the unsampled version, 21 pairs: geomean **+0.64%
[−1.84, +2.66]** — neutral, `polymorphic-objects` −2.8% [−12.0, −0.1], and
`sparse-array-v2`'s regression gone.

**Method.** The benchmarks never saw this. All 13 stayed byte-identical, the
gate was green, and the A/B I ran at the time showed nothing — because none of
the ten rows indexes an array of objects in a hot loop while the JIT is deciding
tiers. It surfaced only from a CONTROL micro built to isolate something else, and
only because the control was *slower than the thing it controlled for*. Two of
tonight's three real defects (this and B97's flush bug) were invisible to the
benchmark suite; both showed up as "a number that cannot be right".

`ZIPP_ARR_PIN_LOOSE=1` restores the unsampled behaviour for bisection.

### B101 — the whole tier programme has a ceiling of ~15%, and B94's 3.2× does not transfer

Every entry from B92 to B100 has been about getting regions out of the memory
tier, on the strength of B94's measurement that a promoted array loop runs
**3.2× faster**. Before building the one remaining capability (heap ops on the
register tier), it is worth asking what the FINISHED programme is worth. The
answer is ~15%, and the reason B94's 3.2× does not transfer is worth stating
plainly.

**Cost model.** The memory tier reads a numeric operand in ~8 instructions and
runs ~10 per op. On the register tier, values are typed once at entry and read
with one instruction, so each numeric operand read saves ~7. Homes live in
**xmm6..15, which are callee-saved under win64** — every helper here is an
`extern "win64"` Rust function, so a home SURVIVES a helper call and no spilling
is required, which is why the earlier "spill 12 homes per call site" estimate was
far too pessimistic. What a heap op does cost is boxing the operands it needs and
unboxing its result: ~10 instructions.

Applying `7 × numeric_reads − 10 × heap_ops` to the largest region of each row,
then weighting by how much of the row is actually in the memory tier (B93):

| row | ops | numeric reads | heap ops | net % of region | mem share | net % of ROW |
|---|---:|---:|---:|---:|---:|---:|
| `typedarray-math` | 114 | 63 | 3 | 36% | 0.88 | **32%** |
| `class-prototype-hot` | 71 | 34 | 8 | 22% | 1.00 | **22%** |
| `polymorphic-objects` | 135 | 63 | 13 | 23% | 0.66 | 15% |
| `sparse-array` | 132 | 56 | 6 | 25% | 0.57 | 14% |
| `map-set-heavy` | 241 | 81 | 23 | 14% | 0.85 | 12% |
| `parse-large-js` | 404 | 88 | 34 | 7% | 0.78 | 5% |
| `markdown-render` | 211 | 102 | 26 | 22% | 0.18 | 4% |

**Geomean if EVERY memory region were merged into the register tier: ~15%.
1.79× → ~1.51×.**

**Why B94's 3.2× does not transfer.** That micro is ~100% numeric operations, so
every instruction saved is on the critical path and nothing is paid back. A real
benchmark region is 10–25% heap ops, each of which pays boxing at its boundary,
and B99 showed the numeric ops in between are mostly single-use temps rather than
long register-resident chains. The tier gap is real and the micro is honest; it
is simply not a model of these workloads.

**What this changes.** It does not say the tier work is worthless — 15% is a real
15%, and four of the seven rows are above 12%. It says the programme cannot reach
parity, and that the remaining capability work (heap ops on the register tier)
should be costed as a ~15% project, not as a 3.2× one. Anything claiming parity
has to attack the **number and cost of heap operations themselves**, not which
tier they run in: `GetProp`/`SetProp` are helper calls today (~20–40 instructions
each), and inline monomorphic property access behind a shape check is 3–4. On
`class-prototype-hot` that is 6 prop ops in a 71-op region; on
`polymorphic-objects`, 7 in 135. That is the same order as the entire tier merge,
and the two compose.

No code change; this is a costing, and it is recorded because four consecutive
landed mechanisms (B92, B94, B95, B97/B98) each moved zero suite time and the
reason was never in any single one of them.

### B99 — register homes IN the memory tier: refuted before it was built

With the hot regions unable to leave the memory tier (B97/B98: they are blocked by
`CallMethod`, a capability the register tier does not have), the obvious remaining
move is to bring the registers to them — keep numeric values in xmm6..15 homes
inside `region_mem`, write through to memory so deopts stay correct, and let
operand reads skip the load-and-tag-check dance. That is M4 as B81/B83/B90 stated
it, and B97 had just proved the write-through half works.

**Priced first, and it does not pay.** The memory tier reads an operand with
~8 instructions (`mov` from `[rbx + dreg(r)]`, shift, two tag compares, branch,
`movq`/`cvtsi2sd`). A home turns that into one `movapd` — but it must be FILLED,
which costs the same ~8 at the def. So promoting register R saves
`7 x (uses - defs)`: it breaks even at one use and only wins on multi-use values.

Counting the biggest region of each row — numeric operand reads, how many land on
a register with two or more such reads, and the net instructions per iteration
from promoting the ten best candidates:

| row | numeric reads | reads on multi-use regs | net instrs/iter |
|---|---:|---:|---:|
| `class-prototype-hot` | 34 | 8 | **0** |
| `polymorphic-objects` | 63 | 6 | **0** |
| `sparse-array` | 56 | 17 | **0** |
| `map-set-heavy` | 81 | 6 | **-28** |
| `typedarray-math` | 63 | 7 | **-14** |
| `parse-large-js` | 88 | 8 | +42 |

**Five of six rows gain nothing or lose.** The cause is the bytecode shape: the
compiler emits `LoadGlobal t; <use t>` per operand, so almost every value is a
SINGLE-USE temp. B93 already measured `LoadGlobal` at 29-37% of every memory
region's ops; this is the same fact seen from the cost side.

**And it corrects the mental model of why the register tier is fast.** It is not
that it caches multi-use values — there are barely any. It is that a GLOBAL lives
in one xmm home for the whole region, guarded once at entry, so `LoadGlobal`
becomes a register move or nothing at all and no operand is ever tag-checked. The
memory tier reloads and re-checks the same global every iteration.

That points at the actual next probe — **fuse `LoadGlobal` into its single-use
consumer**, reading a global's entry-guarded home instead of round-tripping
through the temp's frame slot (~7 instructions saved on the 30-45% of numeric
reads that are LoadGlobal temps, with no per-def cost). It is NOT free: it needs
the memory tier to home globals with an entry guard, and a memory region routinely
carries string and object globals that cannot inhabit an f64 home. Promoting only
globals whose every in-region use is a numeric-required read makes an entry bail
equivalent to the bail those uses would take anyway — that is the shape a next
attempt should take, and it should be priced the same way before it is built.

No code change. Recorded because building it first would have cost hours to reach
a table of zeroes, and because the "extend register homes to the memory tier"
framing carried from B81 to B90 is now refuted on its own terms rather than
sidestepped.

**FOLLOW-UP, same session: the "next probe" this entry proposed is ALSO refuted.**
Pricing `LoadGlobal`/consumer fusion under the restrictions that make it sound —
the temp single-def and single-use (so the fused pair can resume at the
`LoadGlobal`'s ip and never needs the temp's slot written), and the global used
ONLY numerically in the region (so an entry bail is equivalent to the bail its
uses would take anyway):

| row | numeric reads | fusable | share | globals homed |
|---|---:|---:|---:|---:|
| `class-prototype-hot` | 34 | 4 | 11% | 4 |
| `polymorphic-objects` | 63 | 4 | 6% | 3 |
| `typedarray-math` | 63 | 4 | 6% | 3 |
| `sparse-array` | 56 | 2 | 3% | 2 |
| `map-set-heavy` | 81 | 0 | **0%** | 0 |
| `parse-large-js` | 88 | 0 | **0%** | 0 |
| `markdown-render` | 102 | 0 | **0%** | 0 |

The killer is the numeric-only condition: these regions use their globals for
objects, strings and method receivers as well as arithmetic, so almost no global
qualifies. Dropping the condition means homing the global BOXED, which saves the
one-instruction global load and leaves the six-instruction tag check at every
consumer — one of ten, not worth the prologue.

**So both local routes into the memory tier's per-operand cost are closed**, and
the reason is the same in each: the bytecode is a sea of single-use temps drawn
from heterogeneous globals, so there is nothing to cache and nothing to hoist.
What removes the cost is whole-region TYPE SPECIALISATION — knowing once, at
entry, that a value is a number so no operand is ever checked again. That is
precisely what the register tier does, and it is why it is 3.2x faster (B94).

The hot regions cannot get it because they contain `Call`/`CallMethod` and the
register tier issues no calls. **That is now the only remaining path**, and it is
a capability, not a gate: either spill the homes around a call and reload after,
or extend B78's method inlining (memory-path only today) to the register tier so
the call disappears. Everything cheaper than that has been measured and refuted.

### B97/B98 — the first regions actually reach the register tier, and the hot ones are blocked by `CallMethod`

B96 named two steps and predicted neither would show anything alone. Both were
built; both did nothing alone; together they compile the first DOUBLE regions
these benchmarks have ever produced.

**B97 — a `read_outside` register may now SHARE an xmm home.** Three rules pinned
a value to a permanent whole-region home regardless of how briefly it was live,
and `read_outside` (read after the region) was the widest. The comment beside it
stated the hazard exactly:

> `flush_exit` writes the shared home to EVERY sharer's slot, so a sharer whose
> value still matters after the region would come back holding an unrelated temp.

That is the same unsoundness **B94 already solved**. Write-through — store each
def to `[rbx + dreg(r)]`, skip the register in `flush_exit` — makes sharing safe:
each slot receives its own value at its own def, before the home is reused. Two
instructions per def against a tier B94 measured at 3.2× slower.

**B98 — an `Add` operand counts as a numeric-required use of a read-only live-in,
on the DOUBLE path only.** `Add` is excluded globally because it is also string
concat, so a string live-in is unremarkable and the entry guard would miss on
every OSR entry — the 3.31× → 3.45× regression recorded at `ro_live_in`. But that
measurement was of BLANKET admission on the INT path, and its stated causes were
live-ins that are *"strings, doubles or objects"*. **A double live-in bails the
int path and is perfectly native here**, so the largest of the three does not
apply. A string or object still bails, correctly, at `emit_box_to_home`.

**Result — DOUBLE regions where there were none:**

| row | DOUBLE regions before | after | remaining declines |
|---|---:|---:|---|
| `class-prototype-hot` | 0 | **1** | **1** (was 3) — `CallMethod` |
| `polymorphic-objects` | 0 | **1** | 3 (was 7) |
| `typedarray-math` | 0 | **3** | — |

`class-prototype-hot` went from three decline reasons to **one**, and the
`xmm pool exhausted` blocker B95 called terminal is **gone** — B97 removed it, as
B96 predicted it would.

**And the time did not move, because the promoted regions are the COLD ones.**
The row is still 99.6% `jit-mem`: its hot region is one of the three that remain
on the memory path, blocked by the single surviving `CallMethod` decline. The
register tier issues no calls at all, so that region cannot promote until method
inlining — which B78 built, but only for the memory path — is extended to it.
That is the next item, and it is the first time the blocker has been a *missing
capability* rather than an admission gate.


**A wrong-answer bug this introduced, and what caught it.** Making a register
`shareable` also drops it from `live_in_regs`, so its home is no longer filled at
entry. Two existing regression tests failed immediately:

```js
function f(){ let s=0, c=3; for (let i=0;i<200000;i++){ if (i>1e9) { c=7; s+=c; } s+=i; } return c; }
```

`c`'s only in-region def sits on a branch that never runs, so nothing ever fills
its home — and `flush_exit` wrote that garbage home into `c`'s frame slot, so `f()`
returned garbage instead of 3. Two separate mistakes produced it: `write_through`
was populated only inside the `reuse` allocation branch (the one-home-per-value
branch shares nothing, so it looked unnecessary — but the missing ENTRY LOAD
applies to both), and the change was not gated to the path that implements
write-through, so `region_int` silently inherited it. B94 hit the same gate and
merely *panicked*; here it returned a wrong answer.

This is the B9 failure class — plausible, benchmark-invisible, correct-looking —
and it was caught by a kept regression test rather than by review or by the
benchmarks, which all still reported `ALL_CORRECT=1` at the time.

**Suite: NULL.** `--ab-env` on one binary, 25 pairs, after the fix
(`bench/b97_abenv3_2026-07-31.json`):

| row | paired | 95% CI |
|---|---:|---|
| `map-set-heavy` | −3.0% | [−10.4, −0.8] |
| `class-prototype-hot` | −2.7% | [−9.7, +2.1] |
| `polymorphic-objects-v2` | −2.6% | [−6.5, +2.0] |
| `parse-large-js` | +3.6% | [−2.3, +10.3] |
| **geomean** | **−0.16%** | **[−1.87, +1.42]** |

Two pre-fix runs had shown `sparse-array` at −3.0% [−6.6, −1.7] and −2.7%
[−8.5, −0.1] and I was ready to call that a reproducible target-row win. **On the
fixed build it is −0.8% [−8.0, +8.4] — it did not reproduce**, and it was measured
on a binary that returned wrong answers. `map-set-heavy` is the only row excluding
zero now, on a machine whose absolute times are ~40% above the same benchmarks
earlier in the session; that is not enough to claim.

So this lands as a **mechanism with an explicitly null suite result**, like B92,
B94 and B95 before it. What it buys is structural: the `xmm pool exhausted`
blocker B95 called terminal is gone, `class-prototype-hot` is down to a single
decline, and the first DOUBLE regions in this suite's history now compile. What it
does not buy is time, because the promoted regions are cold.

Off-switch `ZIPP_NO_WT_SHARE=1` (both halves — neither is independently
meaningful). Verified with B94's and B95's differentials, 8 runs across the JIT,
`ZIPP_JIT_THRESHOLD=1`, `ZIPP_GC_STRESS=1` and the off-switch, byte-identical to
node; gate green (37 suites / 0 failures, test262 ×3 identical, 13/13 benches).

### B96 — correcting B95's pressure figure: it is ~22 against 14, not 76, and the excess is PERMANENCE not liveness

B95 reported the register-pressure wall using "distinct VM registers in the
region" — 40, 76, 73, 70 against a 14-home pool. **That proxy is wrong**, and it
overstates the problem by a factor of about four. Counting max SIMULTANEOUSLY
live values instead:

| region | ops | distinct regs *(B95's figure)* | max simultaneous, by VM-register | by VALUE (ranges split) | globals |
|---|---:|---:|---:|---:|---:|
| `class-prototype-hot` 0 | 71 | ~~40~~ | **12** | 8 | 10 |
| `polymorphic-objects` 0 | 135 | ~~76~~ | **14** | 11 | 15 |
| `sparse-array` 0 | 132 | ~~73~~ | **16** | 17 | 17 |
| `sparse-array` 1 | 63 | ~~34~~ | **6** | 5 | 9 |
| `typedarray-math` 0 | 114 | ~~70~~ | **13** | 15 | 12 |
| `typedarray-math` 1 | 79 | ~~38~~ | **24** | 11 | 9 |

The wall is real — `class-prototype-hot` needs 12 register homes plus 10 global
homes against a pool of 14 — but the shortfall is **~1.5×, not 5×**, which makes
it a much more tractable problem than B95 implied.

**And the excess is mostly PERMANENCE, not simultaneous liveness.** Three rules
pin a value to a whole-region home regardless of how briefly it is live:

```rust
if first_seen.get(&r) == Some(&false)   // loop-carried live-in
    || hoisted.contains(&r)             // prologue-materialised constant
    || read_outside.contains(&r)        // READ AFTER THE REGION
{ (s, e) }                              // ⇒ permanent home
```

plus every global unconditionally: `intervals.push((s, e, NumVal::Glob(gi)))`.

`read_outside` is the interesting one, and the comment beside it states exactly
why it must be conservative today:

> Sharing one is only sound when clobbering the loser's frame slot is invisible
> … `flush_exit` writes the shared home to EVERY sharer's slot, so a sharer whose
> value still matters after the region would come back holding an unrelated temp.

**That is the same unsoundness B94 already solved, by a mechanism now landed and
gated.** Write-through — store each def to `[rbx + dreg(r)]` and skip the
register in `flush_exit` — makes a shared home safe for a `read_outside`
register: its slot receives its own value at its own def, before the home is
reused, and the flush never overwrites it. Two instructions per def, against a
tier B94 measured at 3.2× slower.

So the remaining item is **not** a general spilling allocator, which is what B95
concluded from the bad proxy. It is:

1. Generalise B94's write-through from one split receiver to any register that
   would otherwise need a permanent home for the `read_outside` reason.
2. Admit read-only live-ins used by `Add` (10 occurrences across three rows) —
   but backed by a plan-time numeric OBSERVATION rather than blanket admission,
   since blanket admission was measured slower (suite 3.31× → 3.45×: the int path
   then accepts string live-ins and entry-bails on every OSR entry). The
   `ARR_INT_PIN_KIND` sample is the in-repo precedent for "observation as a hint,
   guard for soundness".
3. Only then, if pressure still exceeds 14, spill.

**Method note.** The `[pool]` diagnostic added here reports the demand breakdown
(numeric regs, globals, permanent split by reason, shareable) at the exhaustion
decline. It prints nothing today, because pool exhaustion sits BEHIND the
read-only-live-in blocker and only surfaces once that is cleared — which is how
B95 saw it (under a temporary `Add`-admitting experiment, since removed). It is
kept deliberately: it is the instrument the next step needs, and this entry
exists because B95 published a number that was never measured directly.

### B95 — dense-Array reads on the double tier, and the ladder's LAST rung is register pressure

The double/regalloc tier admitted exactly one element kind:

```rust
let pin_kind: u8 = if admit_bitwise { 5 } else { 8 };   // int path: Int32Array
                                                        // double path: Float64Array
```

Everything else — including a dense ORDINARY `Array`, which is what almost all JS
indexing actually touches — declined the whole region to memory. The INT tier had
already solved this for itself via `ARR_INT_PIN_KIND`: pin the array, guard the
element's tag per access, unbox into a home. B95 gives the double tier the same
treatment, with `emit_box_to_home` (the guard the prologue already uses on
live-ins) instead of kind-8's bare `movsd`: an Int element converts, a real
double moves, and a HOLE, bool, null/undefined or heap value **deopts at that
ip**. The plan-time sample stays a hint; the per-access guard is the soundness.

Writes are deliberately not admitted — boxing an f64 home back to a `Value` must
reproduce `Value::num`'s exact-int narrowing and `-0`/NaN handling bit for bit,
which is separate work — so a `SetIndex` on an Array pin still declines.
Staleness cannot bite: the snapshot's `base` goes stale on Vec growth, and a
regalloc region contains no `Call`/`CallMethod` and (by that same rule) no
`SetIndex`, so nothing in it can grow the array or trigger a GC.

**It clears what it aimed at.** `GetIndex/SetIndex (element not a pinned
TypedArray)` declines:

| row | before | after |
|---|---:|---:|
| `polymorphic-objects` | 3 | **0** |
| `class-prototype-hot` | 2 | **0** |
| `sparse-array` | 5 | 2 *(the remaining two are `SetIndex`)* |
| `typedarray-math` | 2 | 2 *(both `SetIndex`)* |

**And not one region promoted.** Every row's `jit-fast`/`jit-mem` split is within
noise of B93's. The regions cleared this blocker and hit the next one, for the
third time in a row (B92 → B94 → B95).

**So the ladder was priced in one experiment instead of climbed.** Temporarily
admitting `Add` operands and branch conditions as read-only-live-in uses — the
blocker that B95 exposed, 10 occurrences across three rows — cleared them and
revealed the terminus:

| row | blockers with `Add` admitted | `jit-mem` |
|---|---|---:|
| `class-prototype-hot` | **xmm pool exhausted (2)**, CallMethod (1) | 99.9% |
| `polymorphic-objects` | **xmm pool exhausted (2)**, live-in (2), type conflict (1) | 65.0% |

**`xmm pool exhausted even with home reuse` is the real wall**, and it is a hard
resource limit, not a gate:

**(B96 CORRECTS THE TABLE BELOW: "distinct VM regs" is a bad proxy. Max
SIMULTANEOUSLY live is 12/14/16/13, not 40/76/73/70, so the shortfall is ~1.5x
rather than 5x, and the excess is PERMANENCE rather than liveness.)**

| region | ops | distinct VM regs | globals | homes available |
|---|---:|---:|---:|---:|
| `class-prototype-hot` 0 | 71 | **40** | 10 | **14** |
| `polymorphic-objects` 0 | 135 | **76** | 15 | **14** |
| `sparse-array` 0 | 132 | **73** | 17 | **14** |
| `typedarray-math` 0 | 114 | **70** | 12 | **14** |

The tier gives every value ONE PERMANENT home for the whole region
(`HOME_XMM_FIRST=2 ..= HOME_XMM_LAST=15`), and **there is no spilling** — a region
that does not fit declines wholesale to a tier B94 measured at 3.2× slower. These
regions want 3–6× the pool. Linear-scan reuse already runs (that is what "even
with home reuse" means) and still overflows.

**This retires the framing every entry since B81 has used.** It is not "extend
register homes to the memory tier" (B81/B83/B90), and it is not the op-admission
ladder (B92/B94/B95). Both were real and both are now cleared far enough to see
past. The single remaining item is **spilling in the register tier**: keep the
hot ~14 values in homes and spill the rest to `[rbx + dreg(r)]`, so a 76-value
region compiles at register speed for the 14 that matter instead of declining
entirely. That is a genuine register allocator, and it is the only thing left
between these six rows and the 3.2× B94 measured.

The `Add` admission itself was NOT kept: it promoted nothing, and the note at
`ro_live_in` records it was measured slower before (suite 3.31× → 3.45×) because
the int path then accepts string live-ins and entry-bails on every OSR entry.

Verified over a 10-case differential built for this change — plain doubles,
all-Int elements, an element turned into a string mid-loop, HOLES,
null/undefined/bool/object elements, `-0`/NaN/±Infinity/denormal round-trips, the
array shrunk mid-loop, grown mid-loop (base realloc), a `defineProperty` accessor
overlay, and fractional/NaN/negative indices — byte-identical to node under the
JIT, `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1` and `ZIPP_GC_STRESS=1`, with
`ZIPP_JITLOG` confirming the path compiled (B94's lesson: a green differential
proves nothing until the mechanism is shown to have run).

### B94 — live-range splitting: the tier gap on an array loop is 3.2×, and the mechanism reaches ZERO suite regions

B93 ended with a gap it was explicit about: six rows are 57–100% in the memory
tier, but nothing had measured what promotion is *worth* on those shapes,
because every attempt to build a comparable array-read loop on the register tier
declined. This closes that gap, and the honest headline is two numbers pointing
opposite ways.

**The mechanism.** The bytecode compiler recycles temp registers, so one VM
register plays several roles in a loop. In the simplest Float64Array loop
writable, `r17` is the pinned array at ip37, the running sum at ip45 and the
loop counter at ip49:

```
37: LoadGlobal dst=17 idx=0      ; r17 = a  (the pinned receiver)
39: GetIndex   dst=16 obj=17 key=18
45: Add        dst=17 a=18 b=19  ; r17 = the sum
49: LoadGlobal dst=17 idx=1      ; r17 = i
51: StoreGlobal idx=1 src=17
```

`def_n[17] == 4`, so the receiver failed the one-def "cleanly excludable" test
and the whole region went to memory. Splitting the ranges gives `r17` a numeric
xmm home while its memory slot stays the receiver's storage: its receiver
`LoadGlobal` emits a real store, every numeric def writes THROUGH to memory, and
`flush_exit` skips it.

Write-through (two instructions per def) was chosen over the alternative — flush
variants selected by a per-ip validity dataflow — because it makes **every exit
correct without knowing which path reached it.** The dataflow version needs the
exit stubs keyed by SOURCE rather than target, and cannot recover the source for
a jump leaving the region. What remains to be proved is only that no use reads
the home before a numeric def fills it (the home is deliberately not
entry-loaded — its slot holds the receiver OBJECT, so an entry load would bail
on every OSR entry), and that is a forward fixed-point meeting to INVALID, run
as a whole-region veto.

**Result on the shape — the number B93 was missing:**

| `a[i & 1023]`, 20M iterations | before | after | node |
|---|---:|---:|---:|
| median of 9 | 98ms | **31ms** | 16ms |
| vs node | 6.5× | **1.94×** | — |
| tier | 99.5% `jit-mem` | **100% `jit-fast`** | — |

**So promoting a real array-read loop from the memory tier to the register tier
is worth 3.2×.** That is the first direct measurement of the tier gap on an
array shape rather than B92's bitwise micro, and it is what justifies the whole
promotion programme.

**And the mechanism fires on ZERO benchmark regions.** Not "the suite is flat" —
the trigger never engages. `ZIPP_JITLOG` counts splits: 1 in the micro, 2 in the
differential, **0 in each of the ten rows**, and the decline histogram is
byte-identical to B93's. The cause is that the split requires the receiver to
come from a GLOBAL slot (`TaPinSrc::Global`); real hot loops live inside
functions, where the receiver is a local and the pin is `TaPinSrc::Reg(r)`. For
those, the pin's identity guard re-reads `[rbx + dreg(r)]` — the same slot the
numeric half must own — so one storage location genuinely cannot hold both the
array and the number. That case is NOT solved here.

**A method note that nearly cost a false pass.** The first differential suite —
10 cases attacking exactly the deopt paths this change touches (out-of-bounds,
fractional and NaN indices, receiver swapped mid-loop, array grown mid-loop,
element turned non-numeric, exception unwinding out of the region, Int32Array
with bitwise, GC stress) — passed in all four modes while the feature **never
fired**, because the cases were wrapped in IIFEs and IIFE locals produce no
`LoadGlobal`. Rewritten at top level it fires twice and still passes all four
modes. A green differential proves nothing until the mechanism is shown to have
run; `ZIPP_JITLOG` is the check.

**Also fixed here:** admitting the split on the shared planner panicked
`region_int` (`no entry found for key` — it looks up a home the plan
deliberately withheld). `plan_region` now takes an explicit `admit_split`, true
only for the regalloc path.

**Disposition.** Lands as a mechanism with no suite claim, because it has none to
make. Its value is the 3.2× measurement and the analysis that names the real
blocker: the double tier accepts only kind-8 (Float64Array) elements
(`pin_kind = if admit_bitwise { 5 } else { 8 }`), while the INT tier already
admits dense ordinary Arrays via `ARR_INT_PIN_KIND` with a per-access tag guard.
Twelve declining regions across four rows are waiting on exactly that, including
`polymorphic-objects`' single 135-op region whose ONLY blockers are three
`GetIndex`. That is B95.

### B93 — `jit-native` was two tiers wearing one name: six rows are 57–100% in the SLOW one, blocked by 2–5% of their ops

B92 established that there are three region tiers and that the middle one already
has register homes. It could not say **which tier the benchmarks actually run
in**, because the profiler had a single `jit-native` bucket. Splitting that
bucket is a four-line change and it inverts the reading of every row:

| row | jit-fast | **jit-mem** | interp/untagged | other |
|---|---:|---:|---:|---|
| `class-prototype-hot` | 0.0% | **99.9%** | 0.1% | — |
| `typedarray-math` | 11.7% | **87.8%** | 0.4% | — |
| `map-set-heavy` | 0.0% | **84.5%** | 10.7% | gc 4.7% |
| `parse-large-js` | 14.3% | **78.3%** | 6.5% | — |
| `polymorphic-objects` | 0.0% | **67.5%** | 28.7% | gc 3.8% |
| `sparse-array` | 0.0% | **57.0%** | 42.6% | — |
| `regex-log-scan` | 5.6% | 27.1% | 13.2% | regex 29.8%, gc 16.2% |
| `markdown-render` | 50.3% | 18.4% | 20.1% | gc 6.9% |
| `async-promise-chain` | 0.9% | 8.8% | 61.7% | microtask 16.5% |
| `json-large` | 39.2% | 0.2% | 14.0% | stringify 23.9%, parse 13.8% |

**`class-prototype-hot` was reported as 99.9% `jit-native` and taken as the row
where "tier entry is solved". It is 99.9% in the memory tier.** So were three of
the other four rows that carried that reading. B90's corrected profile was right
about the numbers and wrong about what they meant, and README.md said so too.

**Instrument check first**, since two instruments have already been wrong this
session. A pure numeric loop reports 98.8% `jit-fast`; a loop over an array of
64 distinct objects (which defeats field promotion) reports 100.0% `jit-mem`;
`ZIPP_JITLOG=1` independently confirms one `MEM region` for the latter and
`INT`/`DOUBLE` for the former. One prediction failed and the instrument was
right: `o.a` in a loop reports `jit-fast`, because SROA rewrites the field to a
scratch global and the region genuinely does compile to a register path.

**Then the part that changes what to build.** Counting the ops in each memory
region that are the reason it is *in* the memory region:

| row | region | ops | blocking | share |
|---|---|---:|---:|---:|
| `polymorphic-objects` | 0 | 135 | **3** (GetIndex) | **2%** |
| `class-prototype-hot` | 0 | 71 | **3** (CallMethod 1, GetIndex 2) | **4%** |
| `sparse-array` | 1 | 63 | **2** (GetIndex) | **3%** |
| `sparse-array` | 0 | 132 | 7 | 5% |
| `typedarray-math` | 0 | 114 | 6 (CallMethod 3, SetIndex 3) | 5% |
| `typedarray-math` | 1 | 79 | 4 | 5% |
| `map-set-heavy` | 0 | 241 | 20 (CallMethod) | 8% |
| `parse-large-js` | 2 | 404 | 33 (CallMethod) | 8% |

Admission is all-or-nothing, so **2–5% of a region's ops force the other 95–98%
onto the slow tier**. That is B92's cliff again, general: B92 removed one op
class and the regions kept declining, because the blocking set is
`Call`/`CallMethod`/`GetIndex`/`SetIndex`, not `Bitwise`.

**And the root cause under several of the declines is register recycling, not
the ops themselves.** The simplest Float64Array loop that can be written —

```js
for (var i = 0; i < 1024; i++) { var v = a[i]; s = s + v * 1.5 - 0.25; }
```

— declines with `pinned receiver reg not cleanly excludable`, so regalloc's
pinned-element fast path (a direct `movsd` into an xmm home) is close to dead
code on real programs. The bytecode says why:

```
37: LoadGlobal dst=17 idx=0      ; r17 = a, the pinned receiver
39: GetIndex   dst=16 obj=17 key=18
45: Add        dst=17 a=18 b=19  ; r17 REDEFINED as an arithmetic temp
```

`def_n[17] == 2`, so the receiver fails the `Some(&1)` test and the region
declines. r18 is recycled the same way (index `i`, then `s`). The two live
ranges are disjoint and never interfere — this is temp-register recycling by the
bytecode compiler, and the planner's non-SSA register model cannot see past it.
The same cause produces `type conflict on a reused register`. The existing
comment predicted it: *"Generalizing this needs SSA-like per-use
disambiguation."*

**What this does and does not license.** It is measured that six rows are in the
memory tier and that 2–5% of their ops put them there. It is NOT measured what
those rows would gain from promotion — every attempt to construct a comparable
array-read loop on the register tier declined for the reason above, which is
itself the finding. The only tier-gap number in hand is B92's, on a shape both
tiers accept: **4.20 → 1.05ns/iter**. Treat the ceiling as inferred, not proven.
For scale, the array-read loop above runs 120ms (Float64Array) / 129ms (Array)
against node's 17–18ms — **7×**, on the shape zipp is supposedly strongest at.

Two routes out, and the measurement favours the second: make the memory tier
faster (M4 as written since B81), or stop regions landing there. The blocking
ops being 2–5% is the argument for the second, and **live-range splitting inside
the region** is the prerequisite for most of it.

No behaviour change in this entry — `is_mem` is diagnostic and only selects a
profiler phase.

### B92 — one bitwise op demoted a whole region out of register homes: 4.20 → 1.05ns on the shape, suite flat

**This entry begins with a correction to several earlier ones in this file.**
B81, B83 and B90 all concluded "M4 — the memory-backed register file — is the
wall", on the strength of the profiler putting four rows at or above 85%
`jit-native`. That framing had the architecture wrong. There are **three**
region tiers, not two:

| tier | homes | restriction |
|---|---|---|
| `region_int` | i64 in xmm | every op must be integer-valued |
| **`compile_region_regalloc`** | **f64 in xmm / bool in gpr** | **no `Bitwise`** |
| `region_mem` | the register file, in memory | none |

The middle one already does what M4 asks for — its own doc says fixed homes,
"NO per-op guards or memory traffic (this is what makes it competitive with
V8)". So the question was never "build register allocation"; it was "why do hot
regions keep missing the tier that already has it".

For 13 regions across four rows, the answer was one line: `plan_region.rs`
declined `Instr::Bitwise` on the double path outright, because those homes are
f64 rather than the int path's sign-extended i64. A single `&`, `|`, `>>>` or
`|0` anywhere in a loop sent the whole region to memory:

| 20M-iteration loop | before | after | node |
|---|---:|---:|---:|
| double arithmetic, no bitwise | 0.75ns/iter | 0.75ns/iter | 0.75ns |
| **identical + one `i & 1023`** | **4.20ns/iter** | **1.05ns/iter** | 0.75ns |
| bitwise-dominated | 2.85ns/iter | 2.90ns/iter | 0.95ns |

**How ToInt32 is done without a call.** `cvttsd2si` in its 64-BIT form truncates
toward zero exactly for |x| < 2^63 — which covers every u32, the case that
matters for `dv.getUint32(…) >>> 24` — and ToInt32 is then just the low 32 bits
of that i64, because ToInt32 *is* trunc-then-mod-2^32 and taking `eax` performs
the mod. The only unrepresentable input is the "integer indefinite" INT64_MIN
that `cvttsd2si` returns for NaN, ±Infinity and |x| ≥ 2^63; those bail to the
interpreter, which computes the real answer. A legitimate operand of exactly
INT64_MIN also bails — correct, merely slower, and unreachable from an f64 that
is not already enormous. `>>>` takes its u32 result from the full 64-bit `rax`
so the value comes back positive.

Verified before timing, because this changes emitted arithmetic: **6,144 cases**
— 1,024 operand pairs across all six operators, the operands including `NaN`,
`±Infinity`, `±0`, 2^31, 2^32, 2^53, 1e300, 1e21, fractionals and negatives —
byte-identical to node under the JIT, `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1`,
and again with `ZIPP_NO_DOUBLE_BITWISE=1` flipped both ways. All 13 benchmarks
also stay byte-identical.

**Suite, `--ab-env` on ONE binary**, 21 pairs, `bench/b92_abenv_2026-07-31.json`:

| row | paired | 95% CI |
|---|---:|---|
| `polymorphic-objects-v2` (diagnostic) | −1.7% | [−2.4, −1.2] |
| `sparse-array` | −1.6% | [−2.4, +0.4] |
| `class-prototype-hot` | −0.5% | [−2.1, −0.1] |
| `typedarray-math` | **+0.0%** | [−0.8, +0.7] |
| suite geomean | −0.39% | [−0.78, +0.25] |

**Ships as a MECHANISM (§14, in the manner of B76/B78), and the reason the suite
is flat is the useful part.** The `Bitwise` declines are now **zero** in every
row that had them — and each of those regions immediately hit its NEXT blocker:

| row | what blocks it now |
|---|---|
| `typedarray-math` | type conflict on a reused register (2), `GetIndex`/`SetIndex` element not a pinned TypedArray (2), pinned receiver reg not cleanly excludable (1) |
| `polymorphic-objects` | read-only live-in used where a number isn't required (4), `GetIndex`/`SetIndex` (3) |
| `class-prototype-hot` | `GetIndex`/`SetIndex` (2), read-only live-in (1) |

A region needs EVERY blocker cleared to promote, so removing one of three moves
nothing — which is exactly what a verified 4× on the shape and a +0.0% on
`typedarray-math` look like together. That is not an argument against the change
(it is strictly better on the shape, nothing regresses, and the cliff is real
and measured); it is the discovery that **tier promotion is a ladder, not a
switch**, and the next two rungs are now named and counted: `read-only live-in
used where a number isn't required` (5 across two rows) and `GetIndex`/`SetIndex`
on an unpinned element (7 across three).

Off-switch `ZIPP_NO_DOUBLE_BITWISE=1`.

### B91 — the INT-promotion route is CLOSED, and the dead switch that hides it now says so

The profiler put four of the ten rows at or above 85% native, which makes the
general tier's code quality the wall. The cheap route to that is not building
MEM register allocation but PROMOTING regions to the INT tier, which already has
xmm homes, a copy-elision peephole, and measured parity with V8 on shapes it
accepts (`s = s + 1.25`: 0.45ns/iter against 0.40ns).

The decline inventory says the opportunity is real and everywhere:
`typedarray-math` 7 regions, `sparse-array` 8, `polymorphic-objects` 7,
`parse-large-js` 5, `json-large` 3, `class-prototype-hot` 3 — all reporting the
same blanket reason, `INT decline: region_is_int=false`, which names nothing.

Reading the predicate turns up `int_unadmitted_ips`, which already returns the
exact blocking ips, and `compile_region_int_maybe_cold`, which already compiles
them as SIDE EXITS instead of declining the region — behind a `cold_exit: bool`
that the only live caller hardcodes to `false`. A dormant feature with a written
soundness argument, one line from being switched on.

**It is dead on purpose. This is B9, and B9 shipped wrong answers.** It went out
opt-in after a fully green gate — test262 byte-identical across 96,029
executions on BOTH tiers, GC stress, and six hand-written cold-block shapes —
and still returned `s = 0` where every other engine returns `3050`, on a ten-line
`delete`-and-rebuild loop. The register plan is built by SKIPPING the cold
blocks, so the plan and the emitted code disagree about what those blocks do, and
the disagreement only surfaces for region shapes nobody thought to write down.

Nothing was changed except the comment on `cold_exit`, which now carries the
warning at the point of temptation rather than three thousand lines away in this
file. It states plainly that the parameter is always `false`, that the soundness
argument printed beneath it is the one B9 shipped with and is wrong, where the
reproduction lives, and what a correct version would need (a register plan that
accounts for the cold blocks, not block-granular exits over a plan that ignored
them).

**Worth recording as a process result, not just a code comment.** The reasoning
that led here was sound at every step — profiler says native-bound, INT tier is
at parity, promotion is cheaper than new regalloc, the mechanism already exists,
the flag is one line — and it terminated one edit away from reintroducing a
known wrong-answer bug in a JIT tier. What stopped it was `PERF_ROADMAP.md`
keeping a REMOVED feature and its counterexample. A repository that records only
what worked would have shipped it.

### B90 — the profiler was lying about a quarter of `json-large`, and `interp` never meant what it said

Found while checking that a newly-added `json-stringify` tag actually fired. It
did not: a workload that is 470ms of nothing but `JSON.stringify` reported
**100% `interp`**.

The cause is a compiler fusion. `compile/calls.rs` turns single-argument
`JSON.stringify(v)` and `JSON.parse(s)` into dedicated ops
(`Instr::JsonStringify` / `Instr::JsonParse`); only the replacer and reviver
forms reach `call_native`. The tag had been placed on the `call_native` arm —
the path almost nothing takes.

**It changes a conclusion this file drew earlier the same day:**

| `json-large` | before | after |
|---|---:|---:|
| `jit-native` | 38.0% | 38.1% |
| **`json-stringify`** | *(invisible)* | **24.0%** |
| `interp` | **40.0%** | **15.4%** |
| `json-parse` | 12.7% | 13.1% |

B83 read that 40% as "its hot code never becomes native" and made the row its
headline interpreter-bound example. **A quarter of it was `JSON.stringify`
wearing the interpreter name.** B87 — the `SetIndex` delegation that removed 101
deopts and made the row 1.1% SLOWER — was aimed at that mis-measurement.

**So the resting bucket is now called `interp/untagged`**, and `prof.rs` says
plainly that it means "no tag was active", not "the interpreter was running
bytecode": any native work reached through an untagged path lands there and
reads as interpretation. A large `interp` share is a question, not a finding.

A `microtask` phase was added at the same time, which splits
`async-promise-chain` 79.1% into **60.6% genuinely-interpreted user JS** (1.5M
`addOne` reactions, each a fresh frame the loop-based OSR JIT never sees) plus
**16.9%** event-loop machinery. The nesting is correct: `run_microtask`
re-enters `run_loop`, which re-tags itself, so callback time is charged to the
callback rather than to the drain.

**Corrected profile of all ten rows** (post-B86/B88):

| row | jit-native | interp/untagged | gc | other |
|---|---:|---:|---:|---|
| `class-prototype-hot` | 99.9% | 0.1% | — | — |
| `typedarray-math` | 99.7% | 0.3% | — | — |
| `parse-large-js` | 91.6% | 7.6% | 0.8% | — |
| `map-set-heavy` | 84.8% | 11.4% | 3.8% | — |
| `markdown-render` | 67.4% | 21.2% | 7.7% | string-ops 3.8% |
| `polymorphic-objects` | 66.9% | 28.0% | 5.1% | — |
| `sparse-array` | 55.1% | 44.1% | 0.9% | — |
| `json-large` | 38.1% | 15.4% | 9.4% | stringify 24.0%, parse 13.1% |
| `regex-log-scan` | 36.7% | 12.6% | 13.9% | regex 27.4%, string-ops 9.5% |
| `async-promise-chain` | 13.2% | 60.6% | 9.3% | microtask 16.9% |

Four rows are now at or above 85% in native code, which is the clearest
statement yet that **M4 — the memory-backed register file and per-op NaN-boxing
— is the wall**, not tier entry. The exceptions each name their own subsystem:
`regex-log-scan` its matcher, `json-large` its serialiser, `async-promise-chain`
its event loop.

**Method note.** This is the second instrument bug this session (after B84
`ZIPP_GCSTATS`, which corrected B81 claiming the collector dominates). Both were
found by checking that a measurement said what it appeared to say, on a workload
constructed so the answer was known in advance. That check is cheap and has now
twice overturned a conclusion that had already been acted on.

### B89 — REFUTED: the second "remove the deopts" fix in a row that bought nothing

`ToNum` (`+x`) bailed the region for every non-number operand. A STRING is the
common one — `+m[1]` on a regex capture (`regex-log-scan`), `+k` on a for-in key
(`sparse-array`) — and its ToNumber is a pure numeric-literal parse, so it
deopted 64 and 128 times respectively, evicted, and left the loops interpreted.

The fix routed those through a helper. It was built to avoid B87's trap
explicitly: `to_number` takes `&self`, allocates nothing and cannot re-enter the
VM, so **no pinned-pointer refetch was needed**, and `ToNum` is a rare op whose
Int/double fast path never reaches the call. One real hazard was found and
handled — `to_number`'s own doc records that it *"returns NaN for an un-handled
object"* while `+obj` must run ToPrimitive, so the helper takes only strings and
the non-object primitives and still deopts objects, Dates, boxed primitives and
Symbols. Verified: a user `valueOf`/`toString` ran EXACTLY once per `+` over 500
iterations, 28 operand kinds byte-identical to node on all three tiers.

`regex-log-scan` deopts **64 → 0**. And:

| row | paired | 95% CI |
|---|---:|---|
| `regex-log-scan` (the target) | −0.2% | [−0.4, +0.1] |
| **`sparse-array`** | **+1.3%** | **[+0.5, +1.9]** |
| suite geomean | +0.04% | [−0.42, +0.47] |

**REVERTED.** The target row did not move, and the only interval excluding zero
points the wrong way — `sparse-array` now pays a helper CALL per for-in key
where it previously ran the loop interpreted, and interpreting it was cheaper.

**Two of these in a row is a pattern, and it is worth naming.** B87 and B89 both
removed real deopts (101 and 64), both were semantically verified, and both
measured zero or negative. B86 and B88 removed deopts and won big. The
difference is not the deopt count:

* **B88** was a WRONG BRANCH — `===` jumped to the numeric path without checking
  the second operand. Fixing it removed work and added none.
* **B86** was a missing case on a RARE op (`SetIndexConcat` exists only for
  `obj["k" + e] = v`), so the added work landed where nothing hot pays it.
* **B87** put the new cost on `SetIndex` — the generic `a[i] = v`.
* **B89** put it on a path whose alternative (interpreting) was already cheap.

So: **a deopt is only worth removing when the replacement is cheaper than the
interpreter doing the same work, on the same path.** Deopt count is not a proxy
for time, and "the region now survives" is not a result. Both of this session's
real wins were found by the PROFILER pointing at interpreted time, then
`ZIPP_JITLOG` explaining it — not by counting deopts and fixing the biggest.

### B88 — `===` deopted the whole region whenever one operand was a double

A codegen bug, not a tuning question. `region_poly_eq` (`codegen/emit_misc.rs`)
dispatches on operand tags:

```
; ja => numeric      // a is a double (tag out of tagged range)
```

It jumps to the numeric path as soon as **a** is a double, without ever looking
at **b**. That path then calls `load_num_xmm` on BOTH operands, and
`load_num_xmm` bails for any tagged non-number. So `x === undefined`,
`x !== null`, `x !== "s"` — with `x` holding a double — **bailed out of the
region on every iteration**: 64 deopts, `OSR_DEOPT_LIMIT`, eviction, and the
whole enclosing loop ran interpreted for the rest of the program.

**`map-set-heavy` contains its own control**, which is how the bug was confirmed
rather than merely spotted. Its lookup loop does `m.get(k) !== undefined` twice.
The first (line 30) does NOT deopt; the second (line 32) deopts 64 times. The
difference is not the comparison — it is that the first map was filled with an
Int and the second with `i * 2 + 1`, a double.

**The fix is a definition, not a heuristic.** A Number is never `===` to a
non-Number, so when one operand is a double and the other is a tagged non-Int,
the answer is a CONSTANT — emit it (`false` for `===`, `true` for `!==`) and
stay in the region. Double-vs-Int still takes the real f64 compare, because
`1.0 === 1` is true, and double-vs-double is unchanged.

| shape, 2M iterations | before | after | node |
|---|---:|---:|---:|
| `!== undefined`, double operand | 49ms | **8ms** | 2ms |
| `!== null`, double operand | 49ms | **8ms** | 2ms |
| `!== "zz"`, double operand | 83ms | **8ms** | 2ms |
| the same three with an Int operand | ~10ms | ~10ms | ~2ms |

The int arms are untouched, which is the check that the fix is on the path it
claims to be. `map-set-heavy` deopts **64 → 0**.


**Suite, `--ab-env` on ONE binary**, 21 pairs, `bench/b88_abenv_2026-07-31.json`,
`ALL_CORRECT=1`:

| row | paired | 95% CI |
|---|---:|---|
| **`map-set-heavy`** | **−11.3%** | **[−15.9, −8.4]** |
| **`json-large`** | **−3.1%** | [−4.6, −1.0] |
| `typedarray-math` | −1.0% | [−1.9, −0.3] |
| `property-ic-shapes` (diagnostic) | −0.9% | [−2.3, −0.5] |
| **suite geomean (13 rows)** | **−1.64%** | **[−2.21%, −1.00%]** |

**The largest suite win in this file**, past B80's −1.41% from earlier the same
day. Four rows improve with intervals excluding zero and none regresses.

**The `map-set-heavy` result deserves its own sentence.** That row is §14's
designated NO-REGRESSION SENTINEL — it sits at 1.02x node and exists to catch
collateral damage, not to be optimised. It had a 400,000-iteration loop running
interpreted the whole time. A row already at parity attracts no attention, which
is exactly why the bug survived: nobody profiles the control.

**Semantics were verified, not argued.** An exhaustive matrix of `===` and `!==`
over every operand-tag pair — 362 comparisons including `NaN` (never equal to
itself), `-0` against `0` (equal), `Infinity`, interned and non-interned
strings, and every double-against-tagged combination — is byte-identical to node
under the JIT, under `ZIPP_NOJIT=1`, under `ZIPP_JIT_THRESHOLD=1`, and identical
again with `ZIPP_NO_POLYEQ_FAST=1` flipped both ways.

**How it was found.** A three-probe workflow over the deopt inventory, the
emitted per-op cost, and the interpreter loop. Its adversarial pass then refuted
**twelve of fourteen** claims, including two that read persuasively: a
`DeleteIndexConcat` admission whose prize arithmetic was wrong by 4-7x (it
assumed compiling the loop removes cost that is paid inside the helper and would
still be paid), and a "blacklisted function never runs native" lead where the
blacklisted version measured **2.2x FASTER** than the compiled one. This was one
of only two findings to survive all three checks — source, reproduction, and
prize arithmetic.

Off-switch `ZIPP_NO_POLYEQ_FAST=1`.

### B87 — REFUTED: removing 101 deopts made the row SLOWER, because the fix taxed the path that was already fast

B86 fixed `polymorphic-objects` by letting `SetIndexConcat` append a NEW key
instead of deopting. `json-large` looked like the same disease in a different
op: fn6 (`build`) region [40] deopted **101 times** at ip 54, a `SetIndex`, and
that row ran **42.2% interpreted**. `jit_set_index`'s own comment named the
cause — *"Everything else keeps deopting — a NEW key (shape change)…"* — and
`build` assigns a fresh computed key on every write.

The same fix was applied, deliberately narrower than B86's: only the
plain-object-with-string-key branch delegates to `set_index` (the interpreter's
own function, so semantics identical by construction), leaving every array and
TypedArray path alone, because each of those deopts guards a documented hazard —
one exists so that a huge sparse resize panics on the interpreter's stack rather
than across an `extern "win64"` boundary, where it would be UB.

**Mechanically it worked and it still lost.** Deopts 101 → **0**, interpreted
42.2% → 36.3%, jit-native 36.4% → 41.8%, all 13 benches byte-identical to node:

| row | paired | 95% CI |
|---|---:|---|
| `json-large` | **+1.1%** | **[+0.4, +2.0]** |
| suite geomean | +0.09% | [−0.16, +0.40] |

**REVERTED.** And the mechanism is visible in the diff rather than hypothetical:
because the delegating helper can allocate and can frame-call an inherited
setter, the emitter had to gain `emit_refetch_pinned` — TWO native calls — after
**every** `SetIndex` in every region, including the hot dense-array store path
that never delegates and never needed it. The change taxed every array write in
the engine to remove deopts from one loop, and the tax was larger than the
deopts.

**The lesson generalises past this patch, and it is the reason B86 worked and
B87 did not.** B86's helper (`SetIndexConcat`) is a rare op that appears only in
`obj["k" + e] = v`, so making it heavier is nearly free. `SetIndex` is the
generic `a[i] = v` — one of the hottest ops in the engine. *Where you put the
new cost matters more than how much of the old cost you removed*, and "deopts
went to zero" is not by itself evidence of a win. Any future attempt here has to
keep the dense-array fast path free of the refetch — e.g. by giving the
delegating case its own opcode or its own emitted branch, so the common path
never reaches the code that can allocate.

### B86 — the profiler's first catch: a region that compiled, then threw itself away

`polymorphic-objects` reported **60.5% INTERPRETED** with only **six**
`ZIPP_JITDECLINE` messages. Those two facts together are the whole story: the
loop was not being REJECTED, so nothing logged — it was being compiled and then
discarded.

`ZIPP_JITLOG=1` found it in one line, repeated: `region fn0 [130] deopt at ip
142`, **65 times**, plus 66 at ip 177, then two blacklists. Both ips are
`SetIndexConcat` — the fused `obj["prop_" + p] = v` — and the emitter's own
comment said what was wrong without anyone noticing the consequence:

> own writable data-slot HIT in place … a NEW key / exotic / non-Int key deopts

The dict-churn loop builds a **fresh `{}` per outer iteration** and writes sixty
computed keys into it. Every single write is a new key. So the region deopted
every iteration, passed `OSR_DEOPT_LIMIT`, was evicted and blacklisted, and the
loop — along with everything else in it — ran interpreted for the remaining
29,000 iterations.

**The fix delegates the new-key case to `set_index_concat`**, the exact function
the interpreter's `Instr::SetIndexConcat` arm calls. That makes the semantics
identical BY CONSTRUCTION rather than re-derived: extensibility, an inherited
setter, `__proto__`, canonical-index keys, frozen and sealed receivers, and the
strict-mode TypeError all keep whatever behaviour that path already has. Because
it can now allocate (the key `String`, three `Vec` growths) and can frame-call a
setter, the emitter gained the `CALL_THREW` check — a throw must unwind rather
than re-execute an op whose setter side effects already ran — and the pinned
r13/r14 + TypedArray refetch that every other allocating helper gets.

| | before | after |
|---|---:|---:|
| deopts | **131** | **0** |
| interpreted share | **60.5%** | **26.6%** |
| jit-native share | 36.0% | **69.4%** |

**Suite, `--ab-env` on ONE binary**, 21 pairs, `bench/b86_abenv_2026-07-31.json`,
`ALL_CORRECT=1`:

| row | paired | 95% CI |
|---|---:|---|
| **`polymorphic-objects`** | **−3.9%** | **[−4.5, −2.9]** |
| `map-set-heavy` | +1.1% | [+0.1, +2.1] |
| suite geomean | −0.11% | [−0.42, +0.19] |

Target row clears §14's 2-3% bar with an interval excluding zero, and no row
regresses above the +2% rule. Gate: 37 suites / 0 failures, test262 ×3
byte-identical. Off-switch `ZIPP_NO_CONCAT_APPEND=1`.

**Worth stating as a method result.** This was invisible to every tool that
existed before today. The decline log said six lines. The bench said 2.13x. The
phase decomposition said "dict churn, 2.0x, diffuse, no single term to attack" —
and that was WRONG, in this file, written earlier today. It took the profiler
saying *interpreted* to know to look at `ZIPP_JITLOG` at all. A row can be
slow because its code never compiles, and nothing in the previous toolchain
could distinguish that from code that compiles badly.

### B85 — the profiler §6 has wanted since B3, and what it says on its first run: M4, not B6

**Why it finally got built.** §6: *"There is no way to attribute engine time to a
source construct, which is precisely how the two reverted epics happened. A
sampling profiler behind `ZIPP_PROF=1` would pay for itself immediately and is a
prerequisite for honest work on B3/B6."* B84 is that sentence made concrete — an
`ObjMap` recycle pool measured −35% on object construction, passed the entire
gate including GC stress and an adversarial field-leak script, and still had to
be reverted because `json-large` regressed +2.9% for a reason TWO hypotheses
failed to explain (memory retention was measured and refuted at +1.8% RSS).
Without attribution, the only honest move was to throw away a real win.

**What it is, and what it deliberately is not.** Not a native stack sampler:
that means `SuspendThread` + `StackWalk64` + `dbghelp`, a new dependency, a
deadlock hazard the moment the sampler allocates while the engine holds the
allocator lock — and it would be reading a fat-LTO binary in which most of the
frames it wants have been inlined away. Instead the engine publishes a PHASE TAG
(one relaxed atomic store at a subsystem boundary), a sampler thread reads it
every 200µs, and the tag histogram is the breakdown. Coarser than stack
sampling, and it cannot be lied to by inlining, because the boundaries are
placed by hand rather than recovered from frames. Off, `enter()` is a cached
`AtomicU8` load and a branch, and no thread is spawned.

**Splitting `interp` from `jit-native` is what made it actionable**, and it was
worth the second pass: the two have OPPOSITE fixes. Time in `jit-native` means
compiled code is slow (M4). Time in `interp` means code is not being COMPILED —
a completely different, and usually much cheaper, problem. `run_loop` is tagged
`Interp` explicitly rather than by omission, so a region call helper that
re-enters the interpreter has its nested time charged correctly instead of to
the enclosing region.

| bench | jit-native | interp | gc | other |
|---|---:|---:|---:|---|
| `typedarray-math` | **99.7%** | 0.3% | — | — |
| `parse-large-js` | **91.3%** | 7.7% | 1.0% | — |
| `markdown-render` | 68.6% | 20.3% | 6.8% | string-ops 4.1% |
| `regex-log-scan` | 37.0% | 13.5% | 13.1% | regex 28.0%, string 8.5% |
| `json-large` | 36.4% | **42.2%** | 8.8% | json-parse 12.6% |
| `polymorphic-objects` | 36.0% | **60.5%** | 3.4% | — |

**The suite is two different diseases, not one.**

* **Compiled-code-bound** — `typedarray-math` (99.7%), `parse-large-js` (91.3%),
  `markdown-render` (68.6%). Nothing but M4 reaches these. `typedarray-math`
  spends essentially ALL of its time in native code that is still 3.7x node,
  which is the cleanest statement of the M4 case in this file.
* **Interpreter-bound** — `polymorphic-objects` (**60.5%** interpreted),
  `json-large` (**42.2%**). These rows are slow because their hot code never
  becomes native, and `ZIPP_JITDECLINE=1` shows only **6 and 11** decline
  messages respectively — so the loops are not being REJECTED, they are never
  being offered. That is a lead, and a cheap one: it is a tier-entry question,
  not a codegen-quality question. It did not exist before this instrument.

**NEXT SESSION STARTS HERE**: find why `polymorphic-objects`' dict-churn loop and
`json-large`'s tree build never reach a region. Both allocate object literals in
the loop body, and `NewObject`/`NewArray` appear NOWHERE in `codegen/` — worth
checking against B4, which built "admit allocation into JIT regions", measured
it SLOWER (`{}` 35→62ns), and may have been answering a narrower question than
this one.

**First run, before the split (kept for the record):**

| bench | interp+jit | regex-exec | gc | string-ops | json-parse |
|---|---:|---:|---:|---:|---:|
| `parse-large-js` | **99.2%** | — | 0.8% | — | — |
| `polymorphic-objects` | **96.6%** | — | 3.4% | — | — |
| `markdown-render` | **88.7%** | — | 7.4% | 3.8% | — |
| `json-large` | **78.4%** | — | 9.1% | — | 12.4% |
| `regex-log-scan` | 50.1% | 27.7% | 12.6% | 9.5% | — |

`interp+jit` is the resting tag: time in the dispatch loop or in compiled
regions, i.e. **running JavaScript rather than sitting in an engine service**.
It is 50-99% of every row.

**This settles B6 vs M4, and not the way B81 guessed.** B81 concluded the
collector was the dominant systemic cost; B84 corrected that to 2-12% using
`ZIPP_GCSTATS`; and the profiler now independently corroborates it (gc 0.8-12.6%,
matching) while showing where the time actually is. `parse-large-js` is 2.49x
with **99.2%** of its time executing JS — no allocator, collector or builtin fix
can touch that row. The dominant item is **M4: the memory-backed register file
and per-op NaN-boxing**, i.e. CFG/SSA plus a real register allocator. B6 remains
worth doing and is worth ~12% of the worst row; it is not the headline.

Three claims from this session's own earlier entries are therefore superseded,
which is exactly what §7 asks for when a measurement contradicts the document.

### B84 — the collector is 2-12%, not dominant: B81 CORRECTED, and the ObjMap pool reverted twice

Two of this session's own claims turned out to be wrong, and the instrument that
caught both is now committed (`ZIPP_GCSTATS=1`).

**Correction 1 — B81 overstated the collector.** B81 concluded from a micro that
"the collector is the largest systemic cost", on the strength of the same
allocation loop costing 74.5 → 122.5ns as the live set grew to 1.2M. That effect
is real, but the SUITE never reaches it:

| bench | collections | trace | sweep | retain | GC total | row | GC share |
|---|---:|---:|---:|---:|---:|---:|---:|
| `regex-log-scan` | 74 | 53.0ms | 96.6ms | 28.5ms | 185.7ms | 1592ms | **12%** |
| `json-large` | 7 | 22.2ms | 14.1ms | 0.0ms | 36.5ms | 445ms | 8% |
| `markdown-render` | 20 | 7.2ms | 15.4ms | 0.0ms | 22.8ms | 455ms | 5% |
| `polymorphic-objects` | 28 | 0.3ms | 12.8ms | 0.0ms | 13.3ms | 593ms | 2% |

**GC is 2-12% of these rows.** Their average live sets are 1,378 to 78,160 —
one to three orders of magnitude below where the micro's curve bites. The cost
is CONSTRUCTION, not collection. B81's table stands; its conclusion did not.

**Correction 2 — the pool's first "null result" was a bad experiment.** B81
reported an `ObjMap` recycle pool measuring zero and reverted it. The cap was
4096 against a `GC_MIN_THRESHOLD` of 65,536, so it could serve ~6% of the next
cycle's allocations — indistinguishable from none. Re-capped at
`GC_MIN_THRESHOLD * 2` the mechanism is real:

| shape | pool ON | pool OFF |
|---|---:|---:|
| `{}` | 25.0ns | 30.5ns |
| `{a:1}` | **46.5ns** | 71.0ns |
| `{a,b,c,d}` | **88.5ns** | 138.5ns |

−35% on object construction. It still does not ship, and the reason is the
suite rather than the mechanism.

**Why it was reverted anyway.** 21 pairs, `--ab-env` on one binary, gate fully
green (37 suites / 0 failures, test262 ×3 byte-identical, and an adversarial
freeze/seal/delete/`defineProperty`/`setPrototypeOf`/class-churn script
byte-identical across pool on/off × `ZIPP_GC_STRESS` on/off and against node):

| row | run 1 | run 2 (after a fast-path meant to help `json-large`) |
|---|---:|---:|
| `polymorphic-objects` | **−5.7%** [−6.2, −4.9] | −0.9% [−1.2, −0.3] |
| `json-large` | +1.6% [+0.8, +4.0] | **+2.9%** [+1.7, +3.3] |
| suite geomean | −0.34% [−0.89, **+0.09**] | — |

`json-large` RETAINS its tree, so almost nothing is swept, the pool stays empty,
and the row pays the check for no benefit. The targeted fix — skip the 80-byte
`mem::replace` unless the object is actually recyclable — made BOTH rows worse,
and the two runs then disagree about the same change by five points.

**One mechanism was proposed and REFUTED rather than left as a story**: that the
pool retains `Vec` capacity, so `json-large`'s wide objects (54,390 shapes) would
blow up memory and cost cache locality. Measured peak RSS is **137.1MB with the
pool against 134.7MB without** — +1.8%, nowhere near enough to explain +2.9% of
runtime. So the regression is real and its cause is still unknown. Naming that
honestly matters more than a plausible story: the next attempt should start by
building the sampling profiler §6 has wanted since B3, because this is the
second time this session a mechanism could not be settled by reasoning. Combined with a suite interval that included
zero even in the favourable run, and one row now clearly past §14's +2% bar,
this is a revert on the rules as written.

**What survives**: `ZIPP_GCSTATS=1` (per-phase collector timing — it is what
corrected B81 and it costs one relaxed atomic load per collection when off), and
the knowledge that a recycle pool is worth −35% on construction for anything
whose objects actually die. A nursery gets that win *and* the retention case,
which is the argument for doing B6 properly rather than approximating it.

### B83 — every remaining row decomposed, and what parity actually costs

The three worst rows and `parse-large-js` were phase-timed against node in one
sitting. Together they say the remaining distance is THREE substrate items and
essentially nothing else — which is worth writing down plainly, because the
contained-fix well is now visibly dry.

**`parse-large-js` (2.49x), zipp 569ms / node 223ms:**

| phase | zipp | node | ratio | gap |
|---|---:|---:|---:|---:|
| **recursive-descent parse** | **162ms** | **17ms** | **9.5x** | **145ms** |
| tokenize | 239ms | 117ms | 2.0x | 122ms |
| mix-hash | 114ms | 68ms | 1.7x | 46ms |
| gen-source | 54ms | 21ms | 2.6x | 33ms |

The parse phase is the worst ratio in the row and `ZIPP_JITDECLINE=1` names the
cause without ambiguity — 47 × `[leaf-reject] Call (outer-context-disallows-call)`,
13 × `[nested-reject] inner-not-leaf`, 5 × `[leaf-reject] Call (branchy-body)`,
and **not one global-read reject**. `pExpr → pTerm → pFactor → pExpr` is MUTUAL
RECURSION: no inliner flattens it, so every call is a full frame call, and the
row is paying B73's un-inlined call cost several million times. This is not a
whitelist line away — it needs either cheaper non-inlined calls or bounded
recursive inlining.

**The three remaining rows, decomposed for completeness — and no cliff in any
of them:**

| row | dominant phase | zipp | node | ratio | gap |
|---|---|---:|---:|---:|---:|
| `polymorphic-objects` | dict churn, 30k objects × 60 props | 502ms | 255ms | 2.0x | 247ms |
| `markdown-render` | 6 render rounds | 416ms | 222ms | 1.9x | 194ms |
| `json-large` | 6 stringify/parse rounds | 383ms | 210ms | 1.8x | 173ms |

Each is a DIFFUSE ~2x over allocation and string building, with no single term
to attack — which is the opposite of `sparse-array`, where one hoisted
loop-invariant was worth 16%. Two contained hypotheses were tested and refuted
on the way: array `push` is **6.20ns against node's 9.20ns — zipp WINS** (and
reads are 3.80 vs 3.00), so `parse-large-js`'s tokenizer gap is not its 14M
array pushes; and the `ObjMap` recycle pool above measured zero.

**The synthesis. All ten rows are now phase-decomposed against node**, and every
phase gap attributes to one of three things:

| substrate item | evidence | rows |
|---|---|---|
| **B6 generational GC** | allocation cost rises 74.5 → 122.5ns purely from a bigger live set; node 2.0 → 9.5 | `regex-log-scan` (result objects), `json-large`, `markdown-render`, corpus generation |
| **M4 CFG/SSA + real regalloc** | the DataView loop is ALREADY a fully inlined native load with a pinned bounds check, and still 3.7x — the cost is the memory-backed register file and per-op NaN-boxing | `typedarray-math`, and the general 2-3x on basic ops |
| **B75/B82 call inlining depth** | mutual recursion and `f.call`/`f.apply` both fall to `call_value`'s `frames.push` + nested `run_loop` | `parse-large-js` parse phase, `sparse-array`'s `hasOwn.call` |

and the diffuse ~2x rows (`polymorphic-objects`, `markdown-render`,
`json-large`) are the first item arriving without a cliff to mark it.

**What that means for parity.** The geomean moved 2.161x → 2.100x this session
on three contained wins, the largest of which (B80, −1.41%) is the biggest in
this file's history. Nothing left in the decompositions is that shape. Closing
the remaining 2.1x is the three items above, and they are architectural: a
generational collector, an SSA register allocator, and deeper inlining. They are
each multi-session, and they are now each backed by a specific measurement
rather than an assertion — which is the state this file has always asked its
work to reach BEFORE the work starts.

### B81 — B6.0's measurement, finally taken: ALLOCATION is 10-50x node, and it is the largest systemic cost in the engine

B6.0 has gated the nursery since it was written — *"Measure first. The previous
roadmap asserted ~214 ns/object allocation against node's ~10 ns, but the
microbenchmark above puts a 4-property literal at 513 ns total, most of it
construction rather than collection."* Here is that measurement, taken from
three independent directions that all converged on it.

**Direct, by shape** (2M allocations per loop, each discarded immediately):

| shape | zipp | node | ratio |
|---|---:|---:|---:|
| `{}` | 41.0ns | 3.5ns | 12x |
| `{a:1}` | 81.5ns | 2.5ns | 33x |
| `{a,b,c,d}` | **148.0ns** | 3.0ns | **49x** |
| `[]` | 23.5ns | 2.5ns | 9x |
| `[1,2,3]` | 29.5ns | 2.5ns | 12x |
| `"str" + int` | 38.0ns | 15.0ns | 2.5x |
| `s.slice(0,5)` | 38.5ns | 4.0ns | 10x |
| *no allocation (control)* | *1.0ns* | *1.5ns* | *0.7x* |

The control row is the important one: with no allocation zipp is FASTER than
node. Every gap above is allocation and nothing else. A 4-property literal has
come 513ns → 148ns since B6.0 was written (mimalloc plus the intervening work),
and is still **49x** node — because V8 bump-allocates into a nursery with inline
properties, and zipp mallocs a `Box<ObjMap>` plus a `String` per key plus the
first push on each of three parallel `Vec`s, then frees all of it on sweep
(`Heap::free_slot` replaces the object with a tombstone, dropping the Box).

**Why this is the headline and not a footnote — the two worst rows both reduce
to it.** `regex-log-scan` (3.69x), phase-timed:

| phase | zipp | node | gap |
|---|---:|---:|---:|
| **matchAll** | 496ms | 71ms | **425ms** |
| **corpus generation** | 560ms | 211ms | **349ms** |
| ip capture | 165ms | 21ms | 144ms |
| replace | 199ms | 60ms | 139ms |
| alternation | 61ms | 8ms | 53ms |
| literal test | 37ms | 70ms | **−33ms (zipp WINS)** |

and `exec` decomposes as **227ns scan + 169ns result object + ~60ns per capture
group**, against node's 40ns total. The result object is an Array allocation
plus one string allocation per capture — i.e. the table above. That the
non-capturing literal `test` is 0.53x is the control that proves it: B8's "we
beat V8 at scanning" survives, and what does not survive is producing objects.

`typedarray-math` (3.74x) is the one row where this is NOT the story — its
DataView swizzle (359ms vs 97ms) allocates nothing and is already a fully
inlined native load with a pinned bounds check. That gap is the MEM tier's
memory-backed register file and per-op NaN-boxing, i.e. M4.

**It is the COLLECTOR, not the allocator — and two experiments say so.**

*First, a null result.* An `ObjMap` recycle pool was built and measured: the
sweep drops each `Box<ObjMap>` and its three parallel `Vec`s, so
`Heap::free_slot` was changed to reset and stash the box (capacity retained) and
`NewObject` to take one. Bounded at 4096, off-switch `ZIPP_NO_OBJMAP_POOL=1`.
Result, on one binary:

| shape | pool ON | pool OFF |
|---|---:|---:|
| `{}` | 31.5ns | 31.5ns |
| `{a:1}` | 72.5ns | 77.0ns |
| `{a,b,c,d}` | 130.5ns | 131.5ns |

Nothing. **REVERTED.** And the reason is visible in the table above it: `[]`
costs 24.5ns while allocating no `malloc` at all (`Vec::new()` does not
allocate), against `{}`'s 31.5ns WITH a `Box`. So the malloc is ~7ns of an
object's cost and everything else is elsewhere. Recycling what is already cheap
buys what it should: nothing.

*Second, the experiment that finds the "elsewhere".* Hold a larger LIVE SET and
re-run the identical allocation loop:

| live set | zipp | node |
|---|---:|---:|
| tiny | 74.5ns | 2.0ns |
| 400,000 objects | 101.5ns | 7.5ns |
| 1,200,000 objects | 122.5ns | 9.5ns |

**+48ns per allocation from nothing but a bigger heap to trace.** zipp marks the
whole live heap every `GC_MIN_THRESHOLD` (65,536) allocations, so the cost of
allocating an object is partly the cost of re-marking every object that already
exists. node's curve has the same SHAPE — a generational collector still pays
something — but 13x smaller, because a scavenge traces the nursery rather than
the heap.

**What this licenses.** B6 is no longer speculative and B6.0's precondition is
met — with its own question answered against its guess. B6.0 asked whether the
cost is *construction* or *collection* and reasoned it was construction
(*"B1 is likely to remove more of this than a nursery would"*). The answer is
BOTH, split: a fixed construction term (~31ns for `{}`, ~74ns for `{a:1}` at a
small live set) and a collection term proportional to the live set that
overtakes it on any real heap. **A nursery is the item that addresses the part
that scales, and B1 does not touch either half.**

**What it does NOT license**: a claim that any contained change closes it.
Nothing in §5 does. This is the M5/B6 substrate item, and it is now the
best-evidenced item in this file.

**Where the 169ns result object goes, and why no contained fix exists.** Per
`exec`, `regexp_build_result` allocates: the result Array (~30ns), one string
per capture (~38ns each), and a side-table `ObjMap` in `arr_props` carrying
`index`/`input`/`groups` — which costs **~100ns**, because three of its four
allocations are `"index".to_string()`, `"input".to_string()`,
`"groups".to_string()`. Those are compile-time constants, allocated fresh
450,000 times in the `matchAll` phase, and NONE of them is ever read by that
loop.

Three routes out were considered and all of them leave "contained":

* **Intern the key strings.** `ObjMap.keys` is `Vec<String>`, so a key must be
  owned; sharing needs `Rc<str>`/`Arc<str>` across every property path in the
  engine. B17 already measured key interning as SLOWER once.
* **Defer the side table** until one of the three names is read. The natural
  marker for "no side table" is the `arr_props` entry's absence, which is
  already load-bearing, so the pending `(index, input, groups)` would need a
  second side structure — and every named read on ANY array would have to
  consult it. That is the hottest path in several rows, to save allocations in
  one.
* **Defer the capture strings** the same way. Needs a sentinel `Value` meaning
  "unmaterialised range", checked on every array element read. Same objection,
  worse blast radius.

So the regex result object is not a regex problem. It is the allocator, arriving
by a different road — which is the same conclusion the table above reaches, and
the reason B6 is the item rather than another `proxy_regexp.rs` fast path.

### B80 — a sparse array's enumeration paid 1.04 MILLION hash probes to find 105 elements

**How it was found: by decomposing the worst small row, not by reading code.**
`sparse-array` is 3.17x and only 149ms, which makes it the cheapest high-ratio
row to iterate on. Phase-timing it against node put two thirds of the gap in two
places:

| phase | zipp | node | gap |
|---|---:|---:|---:|
| stride-writes | 8ms | 5ms | 3ms |
| in+hasOwn | 21ms | 7ms | 14ms |
| **for-in** | **42ms** | **9ms** | **33ms** |
| packed-build+indexOf | 11ms | 4ms | 7ms |
| delete+hole-iter | 50ms | 15ms | 35ms |
| slice/concat+iter | 21ms | 8ms | 13ms |

Scaling the `for…in` phase produced the shape that gave it away — a cost almost
INDEPENDENT of how many keys the array has, where node's is purely per-key:

| populated keys (length 50e6) | zipp | node |
|---:|---:|---:|
| 5,000 | 26ms | 0ms |
| 20,000 | 31ms | 3ms |
| 80,000 | 47ms | 17ms |

`Object.keys` alone accounted for 25 of those 26ms, so it was the enumeration
and not the for-in protocol. Two hypotheses died on the way — it is not O(length)
(500k length is fast, 5M and 50M are the same 29ms) and not about WHERE the keys
sit (5,000 CONTIGUOUS keys enumerate in 0ms wherever they start). A one-line
debug print settled it:

```
[enum] dense_len=1040001 arr_props_keys=4895
```

**Writing `a[i] = v` on a stride extends the DENSE vector until it stops
growing.** For 5,000 strided keys over a 50e6 length that vector reaches
1,040,001 slots holding 105 elements, and the other 4,895 keys go to the
`arr_props` overlay. `object_enum_own`'s array arm then walks `0..dense_len`
calling `array_index_override(idx, i)` — a `pos()` HASH PROBE on the overlay —
per slot. **1.04 million hash probes to discover 105 elements**, every one of
them destined to return `None`.

They were destined to return `None` because the overlay exists precisely to hold
the indices ABOVE the dense prefix. So the question is now asked ONCE, over the
overlay's own keys, instead of once per dense slot. The case that puts an
overlay key BELOW the prefix — a `defineProperty` on a dense index — keeps the
per-slot probe exactly.

| shape | before | after | node |
|---|---:|---:|---:|
| `for…in`, 50e6 / 5,000 keys | 26ms | **2ms** | 0ms |
| `for…in`, 50e6 / 20,000 keys | 31ms | **5ms** | 3ms |
| `for…in`, 50e6 / 80,000 keys | 47ms | **22ms** | 17ms |
| `for…in`, 5e6 / 20,000 keys | 29ms | **4ms** | 5ms — now FASTER than node |
| `Object.keys`, 50e6 / 5,000 keys | 25ms | **1ms** | 0ms |

The bench's `for-in` phase goes **42ms → 18ms**.

**Suite, `--ab-env` on ONE binary**, 21 pairs,
`bench/b80_abenv_2026-07-31.json`, `ALL_CORRECT=1`:

| row | paired | 95% CI |
|---|---:|---|
| **`sparse-array`** | **−16.2%** | **[−16.9, −14.5]** |
| `property-ic-shapes` (diagnostic) | −0.8% | [−1.4, −0.3] |
| `polymorphic-objects` | −0.5% | [−1.9, −0.1] |
| `sparse-array-v2` (diagnostic) | −0.9% | [−1.8, +0.0] |
| **suite geomean (13 rows)** | **−1.41%** | **[−1.90%, −1.05%]** |

**The largest suite-level win recorded in this file** — 2.6x B70's −0.55%, which
had held the record — and the interval excludes zero comfortably. Three rows
improve with intervals excluding zero and none regresses with one. The two
unrelated movers are not a surprise: `polymorphic-objects` walks 30,000
dictionaries with `for…in`, and `property-ic-shapes` builds its receivers with
`Object.create` and enumerates them.

Worth noting WHY this was available: nothing here is clever. It is one hoisted
loop-invariant, in a function nobody had profiled, found by phase-timing the
smallest bad row instead of the largest. `sparse-array` is 149ms — the whole
investigation, from first phase timing to landed fix, cost less wall-clock than
one 21-pair A/B.

**What the same decomposition found NEXT, measured and left open.** After the
hoist, `sparse-array` is 120ms against node's 50ms and the remaining gap is:
`delete`+hole-iter 46 vs 18ms, `in`+`hasOwn` 22 vs 7ms, slice/concat 18 vs 7ms,
`for…in` 15 vs 9ms, packed-build+indexOf 11 vs 5ms, stride-writes 8 vs 4ms.
Splitting the `in`+`hasOwn` phase produced the sharpest number in this file:

| | zipp | node | ratio |
|---|---:|---:|---:|
| `triv()` (direct, inlined) | 2.5ns | 0.5ns | 5x |
| `o.m()` (method, inlined) | 2.5ns | 0.5ns | 5x |
| **`triv.call(null)`** | **63.0ns** | 3.0ns | **21x** |
| `triv.apply(null)` | 61.5ns | 0.5ns | 123x |
| **pre-bound `bound()`** | **59.5ns** | 1.0ns | **60x** |
| `Reflect.apply(triv, null, [])` | 111.0ns | 3.5ns | 32x |

The pre-bound row is what localises it: a bound call never touches
`dispatch_builtin_method_inner` at all, so the ~60ns is NOT the builtin
name-dispatch preamble — it is the generic reflective call path (`call_value`),
which `.call`, `.apply` and a bound function all share. And the reason a direct
call is 2.5ns is that B78's inliner INLINES it; `call_value` ends in
`frames.push` + a nested `run_loop`, which no inliner reaches.

So the fix is not a faster `call_value` — it is teaching the call inliner to
recognise `Function.prototype.call`/`apply` at a call site and inline the
TARGET, which is the natural continuation of B74/B76/B78. `hasOwn.call(o, k)`,
`Array.prototype.slice.call(arguments)` and every polyfill in existence are this
shape. Suite prize is small (only `sparse-array` uses it, 135,715 times, ~4% of
that row); the real-world prize is not. **Recorded, not started.**

Also measured while there and left alone: `delete a[i]` on a dense array is a
FLAT ~50-60ns/op at every scale tested (200k and 1M elements, stride 5 and 50)
against node's ~25ns. Flat means it is a per-op constant, not an algorithmic
problem — worth ~7ms of `sparse-array` and nothing structural to fix.

Pinned by 8 cases in `tests/sparse_enum_hoist.rs`, all node-verified, and the
whole case set produces byte-identical output with `ZIPP_NO_ENUM_HOIST=1` (the
old per-slot path) — which is the check that this is a pure speedup and not a
semantic change. One case in that set fails against node either way: a
non-enumerable index override is missing from `Object.getOwnPropertyNames`. That
is the §6 divergence `descriptors.rs`'s dense loop already owned, confirmed
pre-existing by rebuilding HEAD without the change; it is untouched here.

### B79 — B5.3 refuted by counting, and the one row it pointed at taken directly: async-promise-chain −3.2%

**The probe that refuted the plan.** B5.3 ("builtin method dispatch jump table",
Effort M, *"Gain: the largest single term in markdown-render"*) had sat OPEN and
unrefuted since B5. Before building it, `ZIPP_BUILTINSTATS=1` was added — a
histogram of every `(receiver kind, method name)` that reaches
`dispatch_builtin_method_inner`, i.e. every builtin call a region intrinsic does
NOT already serve. The table is in §5's B5.3 entry; the headline is that
`parse-large-js` makes **89** such calls, `polymorphic-objects` makes **zero**,
and `markdown-render` — the row the plan named — makes 252,669, worth ~10ms of
438ms. Eight of the ten rows are at or under 4%.

The unit price the plan assumed is real and was measured on the way past: a
builtin WITH a region intrinsic runs at or near node (`charCodeAt` 0.5ns,
`map.get` 6.5ns, `set.has` 7.0ns), one WITHOUT costs 26-45ns in both tiers.
Almost nothing in this suite pays it. **A second finding is worth more than the
first**: comparing tiers, every builtin without a working intrinsic is SLOWER in
compiled code than interpreted — `str.startsWith` 44.5 vs 39.0ns, `arr.indexOf`
45.0 vs 41.0, `Object.keys` 108 vs 95.5 — because the region pays the
`jit_call_method_ic` round trip plus two pinned-pointer refetches on top of the
identical shared dispatch. The generic `CallMethod` arm has no native inline
cache, where `GetProp` twenty lines away in `region_mem.rs` has an 8-way one.
That is the real open item, and it is codegen, not naming.

**What landed.** Exactly one row lit up in a way a contained change could take:
`async-promise-chain` makes 1,500,003 builtin dispatches and **100% of them are
`promise.then`**. That arm has always had to prove `then`/`catch`/`finally`
really resolve to the intrinsic — an own shadow, a patched
`Promise.prototype.then` and a subclass override must each win, and test262
observes all three — and it proved it with a full `get_prop(recv, name)`. A
Promise receiver misses `get_member`'s fast path on the heap discriminant, so
that meant `get_member_slow`'s exotic preamble plus a chain walk, per call.

`promise_method_is_intrinsic` decides the same question from three cheap reads —
the `proto_of` slot table (a paged direct index, not a hash map), the
`arr_props` own-property side table, and one `pos` on %Promise.prototype% — and
is `regexp_method_is_intrinsic` (B69) verbatim over `promise_proto`. A Promise
with no `proto_of` entry is accepted because `object_get_prototype_of` sends
`HeapObj::Promise` to `promise_proto` by default; a SUBCLASS instance has an
explicit entry naming the subclass prototype and fails on the first check.

**The probe sits behind a `||`, so a decline runs the ORIGINAL expression
unchanged** — which is what makes the override semantics provably untouched, and
what let `ZIPP_NO_PROMISE_PRISTINE=1` be used as a bisector rather than just an
A/B switch.

| | old | new | node |
|---|---:|---:|---:|
| per `p.then(f)` | 100-103ns | **87-90ns** | 73ns |

**Suite, `--ab-env` on ONE binary**, 21 pairs,
`bench/b79_abenv_2026-07-31.json`, `ALL_CORRECT=1`:

| row | paired | 95% CI |
|---|---:|---|
| **`async-promise-chain`** | **−3.2%** | **[−4.0, −2.5]** |
| `json-large` | −1.4% | [−2.4, +1.7] |
| `map-set-heavy` | −1.4% | [−4.1, +0.3] |
| `sparse-array-v2` (diagnostic) | +0.3% | [+0.1, +0.8] |
| suite geomean (13 rows) | −0.48% | [−0.91%, +0.01%] |

The target row clears §14's 2-3% bar with a tight interval excluding zero, and
**no timed row regresses with an interval excluding zero**. The single
interval-excluding-zero regression is `sparse-array-v2` at +0.3%, a diagnostic
outside `ALLBENCHES` and an order of magnitude under the +2% rule.

**What the histogram itself costs, stated rather than assumed.** With
`ZIPP_BUILTINSTATS` unset the counter is one relaxed atomic load and a
predicted branch, on a path that already costs 26-45ns — under 0.1% of it, and
~1ms on `map-set-heavy`'s four million dispatches. It is NOT separately priced
against a pre-instrumentation binary: both sides of the A/B above carry it (it
is one binary), and pricing it would need a two-binary A/B, which reintroduces
exactly the fat-LTO layout confound the `--ab-env` protocol exists to avoid.
The trade was taken deliberately — an unpriced sub-0.1% against never again
guessing which builtins matter.

**Two pre-existing bugs found and NOT introduced**, both confirmed by running
the same case with `ZIPP_NO_PROMISE_PRISTINE=1` (which restores the old proof
exactly) and both now in §6: a null-prototype Promise answers `p.then(f)` where
node throws `TypeError`, and an accessor `Promise.prototype.then` is read TWICE
per call. The second is pinned AT THE WRONG VALUE by
`tests/promise_pristine_dispatch.rs`, so whoever closes it gets a failing test
rather than a silent pass. Its fix — hand the resolved callee back to the caller
instead of discarding it — would also delete the second lookup on the ordinary
path, so it is a performance item as well as a conformance one.

### B78 — the method inliner had no arm for an INHERITED method: 29.5ns → 5.5ns at every receiver count, suite flat

`build_method_shape` resolved exactly two receiver shapes and declined the third
in a single unreachable line:

```rust
let (fid, method_slot) = match (recv_class, own_slot) {
    (Some(c), None) => (self.ic_class_method_fid(func_id, ip, c)?, None), // class instance
    (_, Some(slot)) => { … }                                             // own data slot
    _ => return None,                                                    // ← everything else
};
```

`(None, None)` is a plain object whose method is **inherited** — which is
`Object.create(proto)`, `Ctor.prototype.m = function …`, and most of what a
transpiler emits. Every such site fell through to `jit_call_method_ic` →
`jit_region_call_impl` → `ic_call_method` → `try_method_inline` on **every
iteration**, and it was invisible in the shape of the numbers precisely because
it never degraded: there is no cliff to see when the arm count is zero. The
existing suite could not have found it either — `class-prototype-hot` is the
only one of the ten rows that defines a user method at all, and it uses `class`.

Measured, `objs[k].m()` over a 4M-iteration loop, each phase owning its own loop
function so no two phases share an IC site:

| receivers | 1 | 8 | 9 | 16 | 1024 |
|---|---:|---:|---:|---:|---:|
| ES class method | 5.5 | 5.5 | 8.3 | 16.3 | 25.5 |
| own-slot function | 5.0 | 6.0 | 8.8 | 17.3 | 27.3 |
| **prototype method, before** | **29.5** | **31.0** | **30.0** | **30.0** | **29.8** |
| **prototype method, after** | **5.5** | **5.8** | **8.3** | **18.8** | **31.5** |
| node | 1.0 | 1.0 | 1.3 | 0.8 | 0.8 |

**−81% at one receiver**, and the row now has the same SHAPE as the other two:
flat to the 8-arm cap, then a linear decay as the arms stop covering. A receiver
loaded INDIRECTLY (`var o = a[k]; o.m()`, which defeats the planner's `arr[idx]`
dense-element scan) goes **34.8ns → 6.0ns**; that one needed `mi_record_recv`
extended to the `ChainData` IC fill, which previously recorded only class
receivers.

**What the arm guards, and why it is allowed to guard so little.** The receiver
identity+version compare that every arm already emits does more work here than
it looks like it does: the version bumps on an own-key ADD, so a later
`recv.m = …` SHADOW misses it, and it bumps inside `ordinary_set_prototype_of`
(`props/descriptors.rs`, the single choke point for `Object.setPrototypeOf`,
`Reflect.setPrototypeOf` and the `__proto__` setter — and `proto_of` has no
`remove` anywhere in the crate), so a re-pointed first link misses it too. That
is the whole reason this arm may guard the first chain link by the receiver's
version alone where the interpreter's `ic_chain_ok` re-reads `proto_of`
explicitly. What is left is:

* one version compare per chain hop, receiver's prototype down to the holder,
  emitted exactly as `SuperInline::hops` already emits them; and
* `holder_vals_ptr[slot] == fn_bits`, which is REQUIRED and not defensive —
  `PROTO.m = other` overwrites an existing slot in place and deliberately does
  not bump the holder's version, so the hop guards alone would keep running the
  OLD body.

Resolution goes through `ic_walk`, the interpreter's own side-effect-free fill
walk, so what gets baked is BY CONSTRUCTION what the interpreter resolves, and
every exclusion it already makes (accessors, `#`-names, a class link mid-chain,
chains past `IC_MAX_HOPS`, exotic receivers) is inherited rather than re-derived
here. The callee restrictions are the own-slot arm's verbatim: a plain
capture-free function, never an arrow — inlining drops `HeapObj::Closure`'s
captured `this_val` and would silently rebind `this` to the receiver.

**Suite, `--ab-env` on ONE binary** (`ZIPP_NO_PROTO_METHOD_INLINE=1` vs unset),
21 pairs, `bench/b78_abenv_2026-07-31.json`, `ALL_CORRECT=1`:

| row | paired | 95% CI |
|---|---:|---|
| `map-set-heavy` | −3.1% | [−6.5, −0.3] |
| `class-prototype-hot` | −1.4% | [−2.8, +0.2] |
| `async-promise-chain` | −1.0% | [−2.9, +0.4] |
| `sparse-array` | +1.7% | [−0.9, +4.2] |
| `markdown-render` | +0.6% | [−1.2, +2.4] |
| suite geomean (13 rows) | −0.28% | [−0.81%, +0.17%] |

**This ships as a MECHANISM, not as a row** — the B76 disposition. The geomean
interval includes zero and the only interval excluding it is `map-set-heavy`
going the RIGHT way, on a row with no user prototype method in it, which is a
layout/noise artefact and is not claimed. What matters for the promote/revert
rule is the other direction: **no row regresses with an interval excluding
zero**, which is the §14 condition B77 failed. The honest summary is that the
ten benches do not contain the construct this fixes, and that is a fact about
the benches — `Object.create`/`prototype.m =` is not exotic JavaScript, it is
what every pre-2015 library and most transpiler output is made of.

**Cost where it does not hit.** A region that gains a method plan flips
`refetch_pinned` on (`region_mem.rs`: `has_prop || do_leaf || do_method`), which
adds the two r13/r14 re-derivation calls after every helper call in that region,
and a fully-missing 8-arm tree with hop guards is ~1.5ns of dead compares. That
is the 29.8 → 31.5 at 1024 receivers in the table above, and it is the same
trade the class and own-slot arms already made.

Pinned by 13 cases in `tests/proto_method_inline.rs` (mid-loop `P.m` reassign,
own shadow, `setPrototypeOf`, a nearer holder gaining the name, an inherited
arrow keeping its lexical `this`, an inherited getter still running on every
call, a `delete` mid-loop, 20 receivers past the arm cap, the constructor-
prototype shape, sloppy `this`) plus `proto_method_inline_matches_across_tiers`
in `lib.rs`. Every expectation was executed in node as a SCRIPT and diffs
byte-identical, and each was confirmed identical under `ZIPP_NOJIT=1` and
`ZIPP_JIT_THRESHOLD=1`. Off-switch `ZIPP_NO_PROTO_METHOD_INLINE=1` (the B74
pattern), which is what let the A/B run on one binary and carry no fat-LTO
layout confound at all — the thing B77 was reverted for.

**Note on the refreshed `bench/results_real.txt`.** It was re-run at ITERS=5 on
the same box an hour after the pre-B78 table and several ratios moved
(`regex-log-scan` 3.48x → 3.76x, `sparse-array` 2.96x → 3.17x). Both engines got
FASTER in absolute terms between the two runs — node's `regex-log-scan` went
475ms → 417ms while zipp's went 1651ms → 1567ms — so the ratio movement is
node's best-of-5 variance, not a zipp regression. This is exactly the confound
the paired A/B above exists to remove, and it is why the A/B and not the table
is the evidence for this entry.

### B77 — REVERTED: the pristine matchAll dispatch shortcut wins its row and loses a bigger one to LAYOUT

A pristine-guarded direct dispatch for `String.prototype.matchAll` (skip IsRegExp's
`@@match` Get, `Get(@@matchAll)` and the `call_value` when both symbols and the flag
accessors are provably intrinsic — the B69/B70 guard pattern, override semantics
node-verified including a non-global regex with a lying `global` getter).

Two independent 21-pair A/Bs (`bench/b77_ab_2026-07-30.json`, `b77_ab2_…`):

| | run 1 | run 2 |
|---|---:|---:|
| `regex-log-scan` (target) | −2.1% [−3.7, −0.8] | −2.8% [−3.7, −1.3] |
| `async-promise-chain` (unrelated) | **+5.4% [+3.1, +6.4]** | **+3.1% [+0.6, +4.2]** |
| suite geomean | +0.04% | −0.16% |

Both effects REPLICATED. The target win is real — and so is a regression on a row that
contains no `matchAll` at all, which is the fat-LTO code-layout hazard §2 documents,
twice confirmed with intervals excluding zero. §14 is unambiguous ("no unrelated row
above +2% outside its CI") — **reverted**, raw JSONs retained.

Two notes for whoever retries. First, the mechanism itself is good: the guards passed
every override test byte-identical to node, and the row moved as predicted twice; the
failure is where the linker put ~40 lines of hot code, not what they do. A retry should
measure with the code placed differently (a separate `#[inline(never)]` function, or
landed alongside other changes) rather than re-derive the semantics. Second, this is the
first time this session the "unrelated CI excludes zero" signal REPLICATED — B70's
`markdown-render` −2.0% and B72's `map-set-heavy` +3.0% did not — so the replication rule
is doing its job in both directions: it saved the B70 bundle and it kills this one.

### B76 — the nested splice accepts arguments: −55% on the wrapper-with-args shape, zero suite uptake, and one latent panic fixed

B75 said "every remaining leaf decline is `Call`" and framed the successor as a design
task. Finer-grained logging (`[nested-reject]`, naming which CONSTRAINT a splice failed)
splits that verdict into something much cheaper plus something that really is the design
task:

* the first-pass rejects were mostly retried and spliced (`NESTED-INLINE`) already;
* every splice that still failed, failed on **`inner-call-has-args`** — the nested
  splice only accepted a ZERO-arg inner call (13 sites in `parse-large-js`, 7 in
  `markdown-render`).

**Args are now admitted.** The emitter already zero-fills every register past the outer
window to undefined — which covers the inner's `this` (strict plain call) and unfilled
params — so the splice only seeds the params that ARE passed, with plain `Move`s inserted
after the guard marker. Pure ops: a bail re-runs the whole outer call with nothing
committed, so deopt-idempotency is untouched.

**Mechanism: −55.1%** on a 3M-call `wrap(n){ return inner(n, 7) + 1 }` micro, 136ms →
61ms, now FASTER than node (63ms).

**Suite uptake: ZERO, stated plainly.** With the args gate cleared, all 20 previously
argc-blocked sites moved to the NEXT constraint — `inner-not-leaf`: their inners contain
calls themselves (depth ≥ 3) or non-admitted ops. Not one suite row contains the
wrapper-with-args-over-a-leaf shape. So this ships on the mechanism and on generality
(the shape is ordinary code), with no suite claim at all; B75's multi-level design task
remains the real successor and is now precisely scoped to `inner-not-leaf`.

**The latent panic mattered more than expected:** the splice has always been able to
push `Instr::LoadUndefined` into a flat body (a VOID inner return), and the leaf emitter
had **no arm for it** — `unreachable!` at region compile time under `panic = "abort"`.
Unreachable before only because a zero-arg void inner never survived the other gates;
args widened the reachable set and the new
`a_void_inner_return_reads_undefined` test hits it directly. Arm added.

`tests/nested_leaf_args.rs` (5 cases, all executed in node and byte-identical): args flow
through, missing params read undefined, EXTRA args are still evaluated (observable
side effects) though unbound, a void inner return, and rebinding the inner mid-loop
falls back to the new function.

### B75 — every remaining leaf decline is `Call`: the next step is multi-level inlining, not another whitelist line

After B74, the `(not leaf-eligible)` message now names the disqualifying op under
`ZIPP_JITDECLINE=1` (`[leaf-reject]` in `region_admit.rs` — a decline COUNT without the
opcode made the next whitelist candidate a guess). The survey answer is unanimous:

| bench | leaf rejects | op |
|---|---:|---|
| `parse-large-js` | 39 | **`Call` — all of them** |
| `markdown-render` | 30 | `Call` |
| `regex-log-scan` | 25 | `Call` |
| `json-large` | 11 | `Call` |

Not one site rejects on a missing simple op any more — B74 consumed that category
(several `parse-large-js` sites with bodies up to 38 ops now report INLINE-ELIGIBLE,
including nested splices). What remains is functions whose bodies CALL other functions,
beyond the existing single-call wrapper splice (`callee_leaf_ok_one_call`, which is
one call, branch-free, pre-effect).

So the next capability on this path is **multi-level leaf inlining** — either
generalising the splice to N call sites with per-site guards, or making the plan builder
recursive with a depth/size budget. That is a design task with real deopt-ordering
implications (every spliced call must still precede any committed effect, or carry its
own resume semantics), not an afternoon whitelist edit. Recorded here as the measured
successor; nothing beyond the logging shipped in this entry.

### B74 — `GetProp` admitted to the leaf inliner: the plain-call shape goes 29.2ns → 9.7ns, three rows move

B73's finding, implemented. A plain `function f(o) { return o.k; }` called from a hot
loop was `(not leaf-eligible)` because `callee_leaf_ok`'s whitelist admitted `GetIndex`
and not `GetProp`, so it paid a full frame call per iteration while the identical body
written as a METHOD was inlined.

**Mechanism, decisive:** the plain-call phase of `class-prototype-hot` goes
**935ms → 310ms, 29.2ns → 9.7ns per call, −66.8%**. The method version of the same body
is 7.0ns, so the remaining 2.7ns is the helper call the method inliner avoids by baking a
shape-guarded slot — a later refinement, not a blocker.

**Suite, measured with `--ab-env` on ONE binary** so there is no code-layout confound at
all (the thing §2 warns about and B70 had to reason around).
`bench/b73_abenv_2026-07-30.json`, 21 pairs, `ZIPP_NO_LEAF_GETPROP=1` vs unset:

| row | paired | 95% CI |
|---|---:|---|
| `class-prototype-hot` | **−1.0%** | [−1.5, −0.1] |
| `polymorphic-objects` | **−0.9%** | [−1.3, −0.6] |
| `async-promise-chain` | **−0.6%** | [−1.2, −0.2] |
| `parse-large-js` | −0.5% | [−1.2, +0.3] |
| `map-set-heavy` | +1.0% | [−0.5, +1.2] |
| suite geomean | **−0.18%** | [−0.43%, +0.03%] |

Three rows improve with intervals excluding zero and none regresses with one, which is
the corroboration the suite interval (just touching zero) does not give on its own. It is
below §14's 2–3% row bar and is not claimed to meet it; it ships as a broad improvement
to the commonest call shape in the language, behind an off-switch.

#### Design: site-free, which is why the budgeting problem vanished

B73 named IC-site budgeting as the hard part — `reserve_ic_sites` counts only the
REGION's own GetProp/SetProp, and `ic_table`'s base is pinned in a callee-saved register
for the whole native run, so it must not grow. That problem does not arise, because the
leaf emitter's existing `GetIndex` arm shows the way: it calls a **site-free helper** and
bails on the deopt sentinel. `jit_get_prop_leaf` is the same shape — three register args,
no IC, no site.

No inline cache also means no `(site, shape)` memo, which is the right call independently:
B72 measured that memo as a PESSIMISATION below `PROP_INDEX_THRESHOLD`, and a leaf's
receiver is usually a small object.

The helper answers an own non-accessor data property, walks a provably clean prototype
chain, and returns `undefined` for a provably absent one. That last case is deliberate
rather than a convenience: a leaf that deopted on every call would drive the enclosing
region past `OSR_DEOPT_LIMIT` and get it evicted for the life of the process — the cliff
shape B69 fixed elsewhere. Everything else defers: accessors (own and inherited), class
chains, Proxies, exotic receivers, private names, module-namespace state.

`packed` carries the CALLEE's func id, because a body's `GetProp` indexes the callee's own
string constants; the caller's id would resolve a different string. It goes through
`vm.func`, not `program.functions[..]` — the M1.3 lesson.

#### Correctness

`tests/leaf_getprop_inline.rs` (12 cases) pins every deferral: own and inherited getters
still run exactly N times, a getter installed MID-loop starts running, a Proxy trap fires
per access, a deleted own property falls through to the prototype, a polymorphic receiver
reads each shape, `.length` on Array/String/Function stays right, `null` still throws, and
a class with private fields reads its public one. **All twelve expectations were executed
in node and diff byte-identical**, and identical again with the flag off and under
`ZIPP_NOJIT=1`. Three `assert_jit_matches` cases in `lib.rs` pin JIT-on == JIT-off, which
is the axis that matters since the inline only exists once the loop is hot.

### B73 — a plain function call in a hot loop costs 4.3x a METHOD call, and one missing opcode explains it. THE most actionable un-started item

`class-prototype-hot` decomposed (five phase scripts + two controls), and it does not say
what its 1.27x ratio suggests:

| phase | node | zipp | ratio | ns/op |
|---|---:|---:|---:|---|
| polymorphic `.area()` x32M | 38ms | 281ms | 7.39x | 1.2 → 8.8 |
| MONOMORPHIC `.area()` x32M | 7ms | 224ms | 32x | 0.2 → 7.0 |
| **plain `area0(one)` x32M** | 6ms | **962ms** | **160x** | 0.2 → **30.1** |
| accessor round-trip x8M | 191ms | **66ms** | **0.35x** | 23.9 → 8.2 |
| depth-5 proto reads x8M | 29ms | 65ms | 2.24x | 3.6 → 8.1 |

(node's 0.2ns figures are a loop it optimised away; the number that matters is zipp
against itself.) Two things fall out.

**The accessor phase is a 2.9x WIN.** The plan's claim that this row's aggregate hides a
method loss behind an accessor win is confirmed, and the win is larger than recorded:
66ms against node's 191ms, with `super.v` chains and overridden setters. Do not "optimise"
this phase.

**A plain function call costs 4.3x what the same body costs as a method** — 30.1ns against
7.0ns — which is backwards. The JIT log says exactly why:

```text
c4_mono   [mi]   fn0@77 INLINE method arms=1 win_top=28          <- inlined
c5_plainfn [leaf] fn0@78 callee fn16 DECLINE (not leaf-eligible)  <- NOT inlined
```

Same body (`return o._v + 1`). The METHOD inliner takes it; the LEAF-call inliner refuses.
The disqualifying op is a single missing entry in `callee_leaf_ok`'s whitelist
(`codegen/region_admit.rs`): it admits `GetIndex` — indexed reads — and **not `GetProp`**.
So any plain function that reads a NAMED property off an argument (`o._v`, `tok.kind`,
`node.len`) pays a full frame call per iteration in a hot loop, while the identical code
written as a method is inlined.

**This is broader than any single row**, which is why it is worth doing before more
row-specific work. Declines observed with `ZIPP_JITDECLINE=1`, every one
`(not leaf-eligible)`:

| bench | leaf declines |
|---|---:|
| `parse-large-js` (2.20x) | 28 |
| `markdown-render` (1.67x) | 20 |
| `json-large` (1.65x) | 7 |
| `regex-log-scan` (3.18x) | 5 |
| `polymorphic-objects` | 0 |

A decline count is not a hotness measure and these have NOT been weighted — that is step
one for whoever picks this up. But `parse-large-js` and `markdown-render` are the
call-and-string-heavy rows, and B68 already established that
`regex-log-scan`'s largest single term is a corpus-generation loop calling `ri()`/`pad2()`.
Note `ri` is separately covered by `callee_leaf_ok_one_call` (the nested-forwarder case
that exists precisely because `ri` contains a `Call`), so its remaining barrier may be the
same `GetProp` gap or something else — measure before assuming.

#### What implementing it requires

Three coordinated pieces, none of them speculative:

1. **The whitelist** (`region_admit.rs::leaf_ok_impl`). `GetProp` is deopt-capable, so it
   joins the existing `seen_effect` ordering rule — it may not follow a committed effect,
   because a bail re-runs the WHOLE call from the call ip. That machinery is already there
   for `GetIndex`/`charCodeAt`/the comparisons.
2. **An emitter arm** (`codegen/inline.rs`). The file already carries IC/shape machinery
   (~30 references) and emits `GetIndex` and `CallMethod`, so this is an addition rather
   than new infrastructure. The cheaper variant is to copy what the METHOD inliner already
   does — observe the live receiver at plan time and bake a shape-guarded slot
   (`build_method_inline_plan`) — instead of allocating a full 8-way IC.
3. **IC-site budgeting**, which is the actual plumbing. `reserve_ic_sites(n)` counts only
   the GetProp/SetProp in the REGION's own code (`codegen/mod.rs:985`, `:1191`); inlined
   leaf bodies would need sites of their own, and the table must not grow during a native
   run (the pinned `r14` base). Option 2's shape-guarded bake sidesteps this entirely,
   which is the reason to prefer it.

Gate it behind an env flag first, per §2's heavy-codegen discipline, and weight the
decline sites before promising a number.

### B72 — three refutations worth more than the code they killed: the string-clone "bonus", the SetProp shape memo, and where typedarray-math's deficit actually is

No code shipped. Three things measured and closed, each of which would have been
plausible to just implement.

#### REFUTED — the "dead full-receiver clone" in `string_method`

B68's hot-path pass flagged `js_recv = js.clone()` (`vm/string_ops.rs:298`) as an O(len)
copy of the whole receiver that every string method past the early fast paths pays and
`matchAll` never uses. It reads that way. **It is not measurable.** A constant-size
operation on a growing receiver, 300k calls:

| receiver length | node | zipp | |
|---:|---:|---:|---|
| 64 B | 217ns | 190ns | 0.9x |
| 512 B | 233ns | 193ns | 0.8x |
| 4 KB | 227ns | 197ns | 0.9x |
| 32 KB | 223ns | 187ns | 0.8x |

`s.slice(0, 5)` is FLAT in receiver length across a 512x range, and zipp beats node at
every size. Either `slice` returns before that line or `JsStr::clone` is not a byte copy;
either way there is nothing to reclaim. Do not "fix" this without a scaling curve that
shows a slope.

#### REFUTED — a shape memo for the SetProp miss path

`jit_get_prop_miss` keeps a `(site, shape) -> slot` memo; `jit_set_prop_miss` never had
one, so a SetProp site with more than `JIT_IC_WAYS` same-shape receivers redid the full
key lookup on every access. Implemented as the mirror image (with the attributes still
read live, since the memo is shared with the get side and says nothing about
writability). **Measured +15.9% on the write micro** — median and best agreeing — and
+2.8% on `property-ic-shapes`. Reverted.

The mechanism is the useful part: `PROP_INDEX_THRESHOLD` is 12, so a map below it resolves
`pos(key)` by **linear scan over a handful of short strings**, and that is cheaper than
hashing a `(u32, u32)` tuple and probing an `FxHashMap` — plus an insert per miss on the
fill path. The memo's break-even is somewhere ABOVE the PropIndex threshold, and the
receivers in both the micro and `property-ic-shapes` carry three keys. Anyone retrying
this must gate it on `map.keys.len() >= PROP_INDEX_THRESHOLD` and bring a workload with
wide objects AND >8 receivers; the get side is presumably paying the same tax on narrow
maps and is worth re-measuring the same way.

#### typedarray-math decomposed: 63% of it is DataView, and it needs Tier D

Seven phase scripts, cost by subtraction against the relevant fill:

| phase | node | zipp | ratio | share of deficit |
|---|---:|---:|---:|---:|
| **DataView swizzle** | 95ms | 363ms | **3.82x** | **63%** |
| f64 fill (`x[i] = …`, top-level) | 96ms | 182ms | 1.90x | 20% |
| prefix sum | 7ms | 30ms | 4.29x | 5% |
| axpy + dot + normalize | 19ms | 47ms | ~2.5x | 7% |
| i32 xorshift fill | 76ms | 97ms | 1.28x | 5% |

The DataView loop DOES compile — two regions, `[67,115]` and `[61,119]` — but to the
**MEM tier**, because the INT tier declines with `region_is_int=false` /
"Bitwise on the double path". The reason is exactly M4.5's warning: the loop holds
`v = dv.getUint32(...)`, a Uint32 that ranges through 2^32-1 and does not fit an i32
register, then feeds it to `>>>` and `&`. There is no contained fix — this is the
Int32/Uint32/Int53 representation problem, i.e. Tier D. §13's "DataView admission/helper
alone — mispriced; most phase gap is surrounding arithmetic" holds, and the marginal
per-call cost confirms it: 73.7M getter calls in 363ms is **4.9ns per call** against
node's 1.3ns, which is a per-op arithmetic gap and not a dispatch gap.

Note the two SMALL phases whose ratios look alarming (prefix 4.29x, dot 3.20x) are
7-30ms marginals recovered by subtracting ~190ms fills — the B69/B70 lesson applies, and
they should not be quoted without an interleaved re-measure.

### B71 — `.test()` becomes allocation-free: −10% on the shape it targets, and the ROW does not move at all

The last two contained items B70 left open. Both are pure removals of dead work, both
mechanisms move, and **`regex-log-scan` does not: −0.3% [−0.9, +0.2]**, CI including
zero. Suite −0.28% [−0.62%, +0.10%], also including zero
(`bench/rx6_ab_2026-07-30.json`, 21 pairs).

**1. Slot 1 of the Annex B statics joins the deferred set.** B60 deferred slots 2..=13 of
`regexp_last` to unit ranges over a rooted subject, materialised on the first
legacy-static read, and left slots 0/1 eager with the justification that `input_val` is
already a Value and `whole` "is computed for the result array regardless". That second
half is true for `exec` and **false for `test`**, which returns a boolean: every
successful `.test()` computed `whole = mk(self, mstart..mend)` 65 lines above the
`if !build { return TRUE }` early-out, i.e. a `Vec<u8>` malloc, a memcpy of the matched
span, an `is_ascii` rescan of those same bytes inside `JsStr::from_wtf8`, and a heap slot,
for a string nothing ever reads.

`RegexpLastLazy::ranges` widens 12 → 13 to cover slots 1..=13, the getter gate moves from
`slot >= 2` to `slot >= 1`, and `whole` moves below the early-out. **The `test` path on an
ASCII subject now allocates nothing at all.** No new GC root: the ranges point into
`lazy.subj`, which `gc.rs` already roots for exactly this reason.

Measured on the shape it targets — an anchored pattern whose match IS the whole ~112-byte
line, 300k calls: **−10.0% median, −8.2% best** over 15 interleaved pairs (both statistics
agree, which after B69/B70 is the bar for believing a shell-timed number at all).

**2. `regexp_exact_source` is gated on `is_empty()`.** It is a SipHash `HashMap` holding
only lone-surrogate patterns, so on the `matchAll` fast path the probe is a guaranteed
miss for every ordinary regex — once per call.

#### Why the row is flat, and why this still ships

The `.test()` phases are **~2% of `regex-log-scan`'s deficit** (phase 1 is 38ms against
node's 94 — zipp already wins it; phase 5 is 77ms against 65). A −10% on 2% of the row is
unmeasurable there, and phase 5 is only ~65ms of a 675ms script, so the phase script
cannot resolve it either (+0.1%). Nothing contradicts anything; the row simply has almost
no `.test()` in it.

Shipped on the same basis as B69: the mechanism moves decisively on its shape, no row
regressed beyond +0.5%, the suite is favourable-but-inconclusive, and both halves are
strict removals of work rather than additions. **It does NOT meet §14's row-based
promotion bar and is not claimed to.** `json-large` −1.3% [−2.6, −0.3] and
`sparse-array-v2` −0.8% [−1.5, −0.4] came along with it; neither has a mechanism connecting
it to regex, so both are read as layout, not as wins.

`tests/regexp_legacy_statics.rs` gains two cases for the newly-deferred slot: that the
range reads back exactly after `test`/a second match/a failed match, and that it survives
a 20k-object GC churn *and* a `RegExp.input = x` write that clobbers the only other
reference to the subject.

#### Correction to §6

The known-deviation entry says `String.prototype.replace` fails to refresh the Annex B
statics "with a global regex". Checked while verifying this change: a **non-global**
regex has the same defect — after `"aXbXc".replace(/X(b)/, "-")`, `RegExp["$&"]` still
holds the previous match. Verified identical before and after B71, so it is pre-existing
and the entry is simply narrower than the bug.

### B70 — `re.flags` was reading EIGHT properties, and a RegExp clone was copying its text: regex-log-scan −2.9%, suite −0.55%

Two mechanisms on the `matchAll` creation path, which B69 left as the largest contained
item. **First real suite-level improvement of this sequence: geomean −0.55%, 95% CI
[−0.91%, −0.19%], interval excludes zero** (`bench/rx3_ab_2026-07-30.json`, 21 pairs).

| row | paired | 95% CI |
|---|---:|---|
| **`regex-log-scan`** | **−2.9%** | [−3.8, −1.5] |
| `map-set-heavy` | −3.0% | [−7.6, −0.1] |
| `parse-large-js` | −0.6% | [−1.1, −0.1] |
| `property-ic-shapes` | +0.4% | [+0.1, +0.8] |
| **suite geomean** | **−0.55%** | **[−0.91%, −0.19%]** |

#### Where the 480ns per `matchAll()` call actually went

A hot-path attribution pass, anchored on ablations rather than reading. `s.matchAll(re)`
with the iterator NEVER stepped: zipp 493ns, node 43ns. Of that, `@@matchAll`'s whole
native body is only 134ns (27%). **The other 359ns is `String.prototype.matchAll`'s
preamble**, which no earlier entry had looked at — and 175ns of it is one property read:

`get_prop(re, "flags")`. `props/member.rs` synthesizes the flags string by reading the
**eight** per-flag accessors off the receiver in canonical order, each a full
RegExp-exotic `get_member_slow` traversal. `re.flags` measured **200ns against node's
10ns**, and `matchAll` reads it purely to test for `g`. The code already knew: a comment
there records "`re.flags` read nine such properties, so it cost 227ns against node's 3ns."

Fixed with `regexp_pristine_flags`: when the receiver IS the regex, its `[[Prototype]]`
is %RegExp.prototype%, it shadows none of the eight names nor `flags`, and all eight on
the prototype are still their intrinsic ACCESSORS, the eight reads are unobservable and
the answer is derivable from the internal flag string. Eight `pos()` probes replace eight
property traversals. **`re.flags` −35%** (2M reads, 510ms → 331ms, interleaved).

**The internal string is stored AS WRITTEN, not canonicalised** — `new RegExp("a","ig")`
keeps `"ig"` — so the shortcut rebuilds the result in canonical `dgimsuvy` order by
membership test. Returning the field raw was the first version and it was a conformance
regression (`"ig"` where node says `"gi"`); `tests/regexp_flags_fast_path.rs` diffs the
shortcut against the observable synthesis over all 192 legal flag combinations AND both
spellings of each, which is what caught it.

#### `source`/`flags` become `Arc<str>`

`matchAll` clones a matcher per call — the iterator must advance a `lastIndex`
independently of the source regex — and that clone allocated two heap `String`s. It
cannot be elided: the matcher is observable to a user `exec` as its receiver, and
`this === re` is `false` there in node too, verified by running it. But its TEXT need not
be duplicated, so the fields are now shared and the clone is two atomic increments.

**The size rationale was refuted.** The hypothesis was that this variant sets `HeapObj`'s
width — its payload was exactly 80 (8+24+24+8+16), the cap `heap_obj_slot_stays_small`
pins, and the roadmap records +64 bytes of padding costing 7.9% of the suite. `Arc<str>`
does drop the payload to 64, and **`HeapObj` stayed 80**: `Generator` is 72 and something
else still pins the ceiling. Measured payloads for the next time someone tries this:
`Generator` 72, `Promise` 64, `RegExp` 64 (now), `IterHelper` 56, `Iterator`/`NativeClosure`/`Map` 48,
`Str(JsStr)`/`Closure`/`Bound`/`Wrapped` 40.

#### Attribution, and a method correction

Two A/Bs, so the split is measured rather than asserted:

| | `regex-log-scan` | suite geomean |
|---|---:|---:|
| `flags` shortcut alone (`rx4`) | −0.9% [−1.5, −0.7] | +0.23% [−0.33, +0.56] |
| both (`rx3`, shipped) | **−2.9%** [−3.8, −1.5] | **−0.55%** [−0.91, −0.19] |

So `Arc<str>` carries roughly −2.0pp of the row and −0.78pp of the suite — the larger
half, because `source`/`flags` are cloned on several regex paths, not just `matchAll`.

**And a repeat of B69's methodological lesson, which I walked into again.** An interleaved
best-of-9 comparison put the `flags`-only row win at −3.0% and the bundle at −3.6%,
i.e. it attributed almost everything to the `flags` shortcut. The 21-pair paired-bootstrap
A/B says −0.9% vs −2.9%. **Best-of-N over ~9 samples on a 1600ms row cannot resolve 2pp**;
it nearly caused the more valuable half of this change to be dropped as noise. Use
`tools/bench.py --ab` for attribution, not shell timing loops — that is what §2's
measurement protocol is for.

#### Still open on this row, unchanged

The two large terms remain: corpus generation (42% of the deficit, contains no regex) and
`matchAll`'s per-step `exec`. Also still unfixed: the eager `whole` allocation per
successful `.test()` (`proxy_regexp.rs:1060` above the `!build` early-out), the
`regexp_exact_source` SipHash probe that is always a miss for patterns without lone
surrogates, and a full clone of the receiver string at `string_ops.rs:298` that `matchAll`
pays and never uses.

### B69 — the missing RegExp dispatch arm: `re.test()` loops −16.8%, and zipp now BEATS node on them. The ROW moves only −1.1%

B68 verified that `dispatch_builtin_method_inner` has arms for eleven receiver kinds
and **none for RegExp**, so `re.test(s)` / `re.exec(s)` ran the entire builtin probe as
dead work, returned `Ok(None)`, and then took the generic route: `get_prop(recv,"test")`
→ `get_member_slow`'s exotic preamble → a per-call `Vec<Value>` → `call_value`. Arm added.

**Measured, interleaved best-of-9 (alternating binaries per rep to cancel drift):**

| workload | node | before | after | |
|---|---:|---:|---:|---|
| hot `re.test()` loop, 300k iters | 112ms | 131ms (1.17x) | **109ms (0.97x)** | **−16.8%** |
| `regex-log-scan` (the row) | — | 1670ms | 1652ms | **−1.1%** |

21-pair A/B (`bench/rx2_ab_2026-07-30.json`): suite geomean **−0.16%, 95% CI
[−0.40%, +0.09%]**; `regex-log-scan` −0.3% [−1.6, +0.3]; no row regressed; binary
+1,024 bytes (+0.017%); startup unchanged.

**Why the row barely moves, stated plainly.** The arm only fires when the RECEIVER is a
RegExp. Of the row's deficit, 42% is corpus generation (no regex at all) and 46% is
`matchAll` — whose receiver is a STRING and whose per-step `exec` goes through
`regexp_exec` internally, never through builtin dispatch. Only phases 1, 2 and 5 call
`re.test`/`re.exec` on a RegExp receiver, and they are 11% of the deficit between them.
So this is a real win on a very common shape that this particular row mostly does not
exercise. It is below the §14 promotion bar for a target row (2–3%); it ships on the
strength of the isolated −16.8%, a non-regressing suite, and being a strict removal of
work rather than an addition.

**CORRECTION to B68's phase table.** B68 reported per-phase figures obtained by
subtracting a generation-only run from a generation-plus-one-phase run. **That method is
too noisy for the small phases and B68's numbers for them are not trustworthy.**
Subtracting two ~610ms runs to recover a ~50ms phase amplifies drift: a 3% wobble on the
baseline is ±18ms, i.e. ±36% of the phase. It first showed this change as −76% on
`test-literal`; interleaved measurement of the same scripts put it at −2.0% of the script,
about −26% of the phase. The two LARGE terms — corpus generation (628ms, measured
directly) and `matchAll` (584ms) — are 600ms-scale and their 42%/46% shares stand. Every
small-phase number in B68 should be re-measured interleaved before it is used.

#### The guard, and why it is not cached

The sibling arms are **not** override-safe — B68 measured
`String.prototype.indexOf = f; "abc".indexOf("b")` answering `1` against node's override —
and RegExp was correct precisely because it had no arm. So `regexp_method_is_intrinsic`
checks all three ways the name can stop reaching the intrinsic, per call: the instance's
`[[Prototype]]` is still %RegExp.prototype%, the instance has no own `test`/`exec`, and
the prototype's slot still holds the intrinsic native as a non-accessor data property.

Deliberately uncached. `ObjMap::set` bumps the heap version only when a key is ADDED
(`if added` — B67), so `RegExp.prototype.test = f` is invisible to a version-keyed cache;
`a_prototype_method_replaced_AFTER_the_loop_is_hot_still_wins` is that exact case. The
uncached form is affordable because B68's ablation put the near-identical
`regexp_exec_fast_ok` at ~7% of the call while the generic path being skipped is the bulk.

`tests/regexp_dispatch_arm.rs` pins eight ways the arm must NOT fire — own method,
replaced prototype method, replaced after warmup, an accessor holding the same intrinsic
value, a re-prototyped instance, a patched `exec` under an intrinsic `test`, and a
subclass override — plus the unpatched fast path itself. **Every expectation was executed
in node and the outputs diff byte-identical**, rather than reasoned about.

Not fixed, still open from B68: the eager `whole` allocation per successful `.test()`
(`proxy_regexp.rs:1060` above the `!build` early-out at `:1125`), and the two large row
terms — corpus generation and `matchAll()` creation's nine `pos()` lookups.

### B68 — regex-log-scan rephased at HEAD: the row's two biggest terms are matchAll and a phase with NO REGEX IN IT. M2.3 REFUTED, M7.1's premise refuted

An attempt to move `regex-log-scan` (3.65x, the worst row and the largest single
contributor to the summed deficit). **Nothing shipped.** What came out is a corrected
phase decomposition that contradicts three of this file's own standing assumptions, and
two measured refutations. Recorded because the next person to open this row should start
from these numbers, not from B60's.

#### The decomposition, at `0b290a0`

Six scripts, each = corpus generation + ONE phase, best-of-7, phase cost by subtraction.

| phase | node | zipp | ratio | share of zipp's deficit |
|---|---:|---:|---:|---:|
| **corpus generation — CONTAINS NO REGEX** | 249ms | 628ms | **2.52x** | **42%** |
| **matchAll** | 169ms | 584ms | **3.46x** | **46%** |
| exec + captures | 96ms | 179ms | 1.86x | 9% |
| replace with `$1` | 151ms | 213ms | 1.41x | 7% |
| test, alternation + anchors | 65ms | 77ms | 1.18x | 1% |
| test, literal `/\[ERROR\]/` | 94ms | **38ms** | **0.40x** | zipp WINS by 56ms |

**42% of the worst regex row is not regex.** It is 150k iterations of a top-level loop
doing string concatenation, ~10 small function calls (`ri`, `pad2`) and a dense array
store. B60 put corpus generation at "about 27% of the older gap"; it is now the joint
largest term. And the literal-`test` phase is 2.5x FASTER than node, which is the B8
correction holding up.

#### Per-call bisection: three roughly equal terms, none of them the gate

150k calls, 1 per line, all matching, cost per call:

| what | node | zipp | zipp − node |
|---|---:|---:|---:|
| `indexOf("status=")`, no regex | 53ns | 53ns | **0 — parity** |
| `test /status=/` (literal, non-global) | 133ns | 213ns | +80ns |
| `test /([a-z]+)=(\d+)/` (non-global) | 133ns | 300ns | +167ns |
| …same, `/g` + `lastIndex = 0` | 107ns | 333ns | +226ns |
| `exec`, non-global, 2 captures | 207ns | 513ns | +306ns |

Reading the deltas WITHIN zipp: fixed regex-call overhead above `indexOf` is +160ns
(node +80); the character class costs a further +87ns (node: **zero** — Irregexp compiles
it); `/g` bookkeeping +33ns; and building the result with 2 captures +213ns (node +74).
So the ~306ns deficit on a successful capturing `exec` is roughly **+139ns result
construction, +87ns matcher, +80ns fixed call overhead** — three comparable terms, which
is why no single fix moves this row much.

#### `matchAll`: the per-CALL cost is as big a problem as the per-step cost

| | count | node | zipp |
|---|---:|---:|---:|
| `matchAll()` call, iterator never stepped | 150k | 53ns | **480ns** |
| per subsequent step | 450k | 22ns | **418ns** |

That is 64ms of deficit in iterator CREATION and ~178ms in stepping. Creation is
`regexp_matchall_fast_ok` — **nine `ObjMap::pos()` lookups** (5 on the instance's
`arr_props`, 4 on %RegExp.prototype%, which is past `PROP_INDEX_THRESHOLD` so each is a
hash) plus the iterator allocation and a side-table insert. The step is dominated by the
`exec` inside it, not by iterator machinery: a bare `exec` measures 513ns against the
step's 418ns.

#### REFUTED 1 — `regexp_exec_fast_ok` is not material (M7.1's premise)

The audit plan's M7.1 proposes protector-based RegExp fast-path gates, correctly hedged
with "**after telemetry proves the fixed gate is material**". Telemetry says it is not.
An ablation build short-circuiting `regexp_exec_fast_ok` to `true` (unsound, measurement
only) saved **6ms of 209ms** on the global-`test` loop and **22ms of 290ms** on `exec` —
~7%. The gate looks expensive when read (two side-table probes plus a `pos("exec")` over
%RegExp.prototype%) and is not. Do not build the protector for this reason; if it is
built, justify it from a workload where it actually shows.

#### REFUTED 2 — `regexp_string_iters` → `SlotTable` (plan M2.3) is a wash

Implemented (a pure type swap — `SlotTable` is API-compatible), plus removal of a genuine
redundancy: `regexp_string_iter_step_inner` computed `pristine_exec` and then called
`regexp_exec_abstract`, which re-evaluated the identical gate, so every step ran it twice.

21 counterbalanced pairs (`bench/rx1_ab_2026-07-30.json`): **`regex-log-scan` +0.1%
[−0.7, +1.0]** — no movement at all. Suite +0.29% [−0.08, +0.60]. It also perturbed
`map-set-heavy` to +3.0% [+1.7, +4.6], a sentinel breach with no mechanism connecting the
two (fat LTO + one codegen unit; the layout hazard §2 warns about). **Reverted**, both
halves, under the §14 revert rule: the target row did not move and the result sits inside
drift.

The reason is now obvious from the decomposition and was hedged in the plan itself
("do not overstate: the earlier supposed 552 ms iterator prize was superseded by the
already-landed `fast0` path"). One SipHash probe per step is noise beside the ~418ns the
step spends in `exec`.

#### Three VERIFIED structural defects on the regex call path

Found by a hot-path mapping pass and each confirmed by hand afterwards. None is fixed
here; all three are cheap to re-verify from the file:line.

1. **`dispatch_builtin_method_inner` has no `HeapObj::RegExp` arm** (`vm/builtins.rs:252`).
   It has arms for Array, Str/Cons, Map, Set, Generator, AsyncGenerator, Promise, Date,
   TypedArray, DataView and ArrayBuffer — and RegExp falls to `_ => Ok(None)`. So every
   `re.test(s)` / `re.exec(s)` runs the ENTIRE builtin probe as dead work (several random
   heap-discriminant loads, `is_callable`, a Boxed probe, a chain of `&str` compares),
   returns `None`, and only then takes the generic route: `get_prop(recv, "test")` →
   whose fast path bails on the RegExp discriminant into `get_member_slow`'s exotic
   preamble (~10 guards, an `FxHashMap` probe on `module_namespaces`, a
   `"test".parse::<usize>()`, two `SlotTable` probes, then a `PropIndex` hash + memcmp on
   the **20-key** `%RegExp.prototype%`) → a per-call `Vec<Value>` allocation → `call_value`
   → `call_native`'s ~600-arm jump table. That is the bulk of the +160ns of fixed
   regex-call overhead measured above `indexOf`.
2. **A hot loop containing `re.test()` cannot stay compiled.** In a region,
   `jit_method_builtin_fallback` (`engine/jit_calls.rs:393`) calls `try_builtin_method`,
   gets `Ok(None)` because of defect 1, and returns `SELF_CALL_DEOPT` **without setting
   `osr_deopt_exempt`** — so every call exits the region AND is counted as a deopt by
   `note_region_resume`. Past `OSR_DEOPT_LIMIT` the region is evicted. This is a
   performance cliff of the same shape as B59's, and it is invisible to every gate.
3. **One heap string allocated per successful `.test()`.** `whole = mk(self, mstart..mend)`
   (`vm/proxy_regexp.rs:1060`) is computed before the `if !build { return TRUE }` early-out
   at `:1125`. The comment there records that slots 2..=13 of the Annex B statics were
   made lazy for exactly this reason; slot 1 (`lastMatch`) kept `whole` eager, so `test`
   still allocates the matched substring it never returns.

#### …and why the obvious fix for defect 1 is FORBIDDEN as written

Adding a `regexp_method` arm beside the others would be wrong, because the existing arms
are **not** override-safe and RegExp currently is:

```js
String.prototype.indexOf = function () { return "OVERRIDDEN"; };
"abc".indexOf("b");        // node: "OVERRIDDEN"   zipp: 1
Array.prototype.slice = function () { return "OVERRIDDEN"; };
[1,2,3].slice(1);          // node: "OVERRIDDEN"   zipp: 2,3
RegExp.prototype.test = function () { return "PROTO-OVERRIDDEN"; };
/a/.test("a");             // node AND zipp: "PROTO-OVERRIDDEN"  ← correct TODAY
```

The receiver-kind fast paths bind a builtin target from its NAME, which the audit plan
warns against in M2.6 ("Do not bind a builtin target solely from its name"). Only
`toString`/`valueOf`/`toLocaleString`/`toJSON` are deferred to the prototype chain
(`vm/builtins.rs:95`); every other builtin method name is shadow-blind. RegExp is correct
precisely BECAUSE it has no fast path. Giving it one the same way would trade a correct
path for a faster wrong one.

**This reframes M7.1.** Its value is not skipping `regexp_exec_fast_ok` — that gate is
~7% (refuted above). Its value is being the PROTECTOR that makes an override-safe
dispatch arm possible: a pristine-`%RegExp.prototype%.test`/`exec` epoch plus a
per-instance own-override check, checked once per call, admits the fast arm and falls back
to the generic path the moment anything is shadowed. Cheap, and it fixes defect 2 as a
side effect. Do that before the arm, not after.

#### Where this row actually has to be attacked

In order of measured size, with the honest difficulty attached:

1. **Corpus generation, 379ms (42%).** Not a regex item at all, and it helps
   `markdown-render`/`parse-large-js`/`json-large` too. The string paths are already
   good — `add_values` has flat/rope fast paths and BOTH `string + int` and `int + string`
   write the integer's decimal straight into the buffer, so there is no naive conversion
   to remove. What is left is generic interpreted call overhead (~1.5M `ri`/`pad2` calls)
   and the fact that this is a TOP-LEVEL loop. Note B56's in-place string accumulator does
   handle a global target (`global_inits_string`), but `loop_inplace_safe` rejects a body
   containing CALLS — and for a global accumulator that rejection is load-bearing, because
   a callee can read the global directly. Extending it needs escape analysis, i.e. Tier D.
2. **`matchAll()` creation, 64ms.** The nine `pos()` lookups in `regexp_matchall_fast_ok`
   are a real, contained target — unlike the per-step gate, this one runs on a path with
   no `exec` to hide behind. A version-keyed cache is the obvious shape, but note the trap
   found in B67: `ObjMap::set` bumps the heap version only when a key is ADDED
   (`if added`), so a plain `RegExp.prototype.exec = f` overwrite does NOT bump it. A
   sound cache must key on `(version, keys.len(), slot, value bits, accessor bit)` and
   re-read the slot each call — which still removes the key HASH, the expensive part.
3. **Result construction + captures, +139ns/match.** Plan M7.2, explicitly "a measured
   research item, not a guaranteed win".
4. **The matcher on character classes, +87ns/match where node pays zero.** This is the
   Irregexp gap (B8b), an XL epic, and it is the SMALLEST of the three per-match terms.

### B67 — B66's three open specs closed, and each one had an INTERPRETER bug underneath it

B66 left three `#[ignore]`d failing specs in `tests/jit_tier_parity.rs` as "open tier
divergences". `cargo test -- --ignored` now reports nothing there: all three are fixed
and unignored, 11/11 green.

The finding worth recording is not that they closed. It is that **two of the three were
not tier divergences at all** — or not only. B66 diagnosed them from the JIT side and
was wrong about the mechanism in both cases, which the first `ZIPP_NOJIT=1` run said
immediately:

| B66 said | what was actually true |
|---|---|
| "compiled code keeps reading the slot as `undefined` where the interpreter throws ReferenceError" | `delete globalThis.implicitG` **never cleared the slot at all**, so the INTERPRETER answered `5` too. Only the unqualified `delete implicitG` cleared it. Two spellings of one deletion disagreeing. |
| "a `StoreGlobal` to a REAL own property of the global object writes the slot directly" | true, AND `LoadGlobal` never routed either — in both tiers. AND `defineProperty(globalThis, …)` on a slot-only binding built a property with no value. Three bugs stacked, only the first visible from the JIT. |

**Lesson for the audit plan's M1.1, which called this out and was right:** "fix the
interpreter semantics against the specification/Node first; making native code match
the current interpreter is not sufficient." Had the JIT been taught to agree with the
interpreter here, all three would have gone green while every answer stayed wrong.

#### What landed

**1. The indexed-prototype protector now has ONE invalidation point.**
`array_proto_has_index` is read by 11 sites (interpreter fast paths, JIT helpers) and
every one treats `true` as "MAY supply an index ⇒ run the full protocol" — verified
site by site, and `codegen/` never bakes it, so flipping the bool is enough and takes
effect at the next helper call. It was set only when an integer-like key was DEFINED on
`Array.prototype`/`Object.prototype`; `setPrototypeOf(Array.prototype, x)` splices a
whole new chain in and was invisible. Both mutations now go through
`invalidate_indexed_proto_protector`, called from `note_array_proto_index` and from
`ordinary_set_prototype_of`. Invalidation is unconditional — NOT "does the new chain
carry an index" — because `x` can be index-free at `setPrototypeOf` and gain `x[5] = …`
a line later, on an ordinary object no protector watches.

**2. A global-route epoch, checked at native entry.** Every JIT tier proves each direct
global access legal at COMPILE time, on the stated assumption that "once a slot holds a
real value it can never go back". `delete` falsifies that. `Vm::global_route_epoch`
is bumped by `uninitialize_global` (both delete spellings) and by
`note_global_own_property_change` (a real descriptor appearing on the global object
behind a slot); `try_run_jit`/`try_run_osr` refuse to enter when it moved and the
function's globals no longer check out. Cost in a program that never does either — i.e.
every program — is **one compare against zero**; the rescan is memoized per
`(func_id, epoch)` for programs that do. Declining does not evict, so a re-created
binding heals by itself (`a_recreated_global_is_readable_from_compiled_code_again`).

**3. One predicate instead of five copies.** `global_slot_directly_routable` is now the
single source for "may a tier emit a raw `[r12 + slot*8]` access", used by the Tier A/C
gate, `region_globals_ok`, `build_leaf_inline_plan`, the array-builtin callback compile,
and the entry revalidation. Three things fell out of consolidating them:

* the region gate had a **byte-for-byte duplicate** scan over the identical range 200
  lines later, and that copy declined **without** `region_defer` — so a region rejected
  only by it was never reconsidered even after the binding became a real slot. Deleted.
* `native_cb_entry` (Tier A compile of an array-builtin callback) had **no global scan
  at all**. Added, and deliberately BEFORE `jit.compile`, because compile blacklists on
  failure and `FN_DEAD` is sticky.
* stores now also refuse a slot the global object shadows with a real descriptor, which
  only the interpreter checked.

**4. Self-call callee identity.** Tier A's `emit_self_call` emits a direct `call` to its
own entry in place of `LoadGlobal(self_slot) + Call`, with no callee guard, so a rebound
name kept recursing into the old function. `JitFn` now records
`self_binding: Option<(slot, expected bits)>` — set only when a self-call was actually
emitted, so `function f(){…}; var g = f; f = 1;` keeps `g` fast — and `try_run_jit`
checks it. Checked at the OUTER entry, not inline per recursion: declining there makes
the whole activation interpret, and the interpreter re-resolves every inner call
correctly. `fib`'s hot path is untouched.

**5. Four interpreter conformance fixes, all verified against node as a SCRIPT.**
`node file.js` runs CommonJS, where top-level `var` is module-scoped and never a global
property — comparing against it silently tests the wrong semantics. Use
`node -e 'require("vm").runInThisContext(require("fs").readFileSync(process.argv[1],"utf8"))'`.

| case | was | now (== node) |
|---|---|---|
| `delete globalThis.implicitG; implicitG` | `5` | ReferenceError |
| `defineProperty(globalThis,"imp",{get,set}); imp` | stale slot `2999` | `"G"` — the getter runs |
| `foo=1; defineProperty(globalThis,"foo",{writable:false}); foo` | `undefined` | `1` |
| `var late=…; defineProperty(globalThis,"late",{…,configurable:true})` | silently accepted | `TypeError: Cannot redefine property: late` |

The third and fourth are one fix: the global object's properties and the global slots
are two views of one set of bindings, but only the slot view existed for `var foo` /
`foo = 1`, so a define was a CREATE rather than a REDEFINE. `object_define_property`
now MATERIALIZES the binding's current descriptor into the `ObjMap` first, after which
the ordinary redefine path is correct by construction —
ValidateAndApplyPropertyDescriptor keeps every field the incoming descriptor omits, and
rejects a `configurable: true` request on a non-configurable `var` binding.
`global_slot_binding_descriptor` owns that slot→descriptor classification and is shared
with `object_get_own_property_descriptor`.

**6. A latent OOB, and a JIT coverage hole nobody had named.**
`jit_set_prop_miss` indexed `vm.program.functions[func_id]` where `jit_get_prop_miss`
was already fixed to use `vm.func` — an out-of-bounds panic for an eval/module function.
It is currently UNREACHABLE, and the reason is itself a finding: `dispatch.rs` gates
**both** the function JIT and the OSR tier on `func_id < main_func_count`, so **eval,
`new Function`, and every MODULE body are never JIT-compiled at all** (module functions
also live in `eval_funcs`). Fixed anyway, because the gate is what makes it latent and
the gate is a performance debt someone will want to lift.

#### Fallout from fixing `LoadGlobal`, which is the interesting risk

Making a bare global read route through a real own descriptor makes **every
JS-implemented polyfill shadowable**. `Array.fromAsync` is one: its polyfill read
`items[Symbol.asyncIterator]` off the global `Symbol` binding, so
`globalThis.Symbol = {asyncIterator: Symbol("asyncIterator")}` redirected an INTRINSIC
lookup and the polyfill called the user's fake method —
`built-ins/Array/fromAsync/asyncitems-uses-intrinsic-iterator-symbols` failed both ways
the moment the shadow became visible. Fixed by keying off the engine-internal
`'@@asyncIterator'` / `'@@iterator'` strings. **Any other intrinsic a polyfill reads by
global name (`TypeError`, `Object`, `Array`) has the same exposure**; only the symbols
are test262-visible today, but the pattern is the hazard.

#### Gate

test262 95936/95942 with the fail set byte-identical to
`tools/test262-expected-failures.txt` in all three passes (default, `ZIPP_NOJIT=1`,
`ZIPP_JIT_THRESHOLD=1`); `cargo test --workspace --release` 495 passed / 0 failed;
bench correctness `ALL_CORRECT=1` over all 13 files, JIT and NOJIT, plus `ALL_CORRECT=1`
from the A/B harness itself (exact bytes, no normalisation); `ZIPP_GC_STRESS=1` over the
changed paths identical to node in all three tier modes.
`tests/jit_tier_parity.rs` 11/11 with **zero** ignored (was 5 passing + 3 ignored),
`tests/json_owned_keys.rs` (6), and 6 new `assert_jit_matches` cases in `lib.rs` — which
is the gate clause that matters here, since each asserts JIT-on == JIT-off rather than
just the right answer.

#### Measured: FREE, after the first attempt was not

21 counterbalanced pairs against a clean `735535c` build (both sides' sha256 recorded;
new binary is 2,560 bytes SMALLER). **Suite geomean +0.23%, 95% CI [−0.16%, +0.42%] —
the interval includes zero**, so the correctness work is free. `bench/b67b_ab_2026-07-30.json`.

The first attempt was NOT free, and the reason is worth keeping. Run 1
(`bench/b67_ab_2026-07-30.json`) measured **`map-set-heavy` +3.9% [+0.5, +5.6]** — a
sentinel breach, the gate caps it at +2% — and `polymorphic-objects` +2.4% [+1.2, +3.6].

The mechanism was one line. The new `LoadGlobal` arm gated its route check on ground
truth: `global_obj_has_own_keys()`, which is a heap load plus `keys.is_empty()` through
globalThis's cold `ObjMap` box. **`LoadGlobal` is far too hot to touch the heap
speculatively**, and both offending rows are top-level scripts whose hot loops read
globals — so every iteration paid it. Isolated on a 20M-iteration top-level global loop
under `ZIPP_NOJIT=1`: **585ms → 655ms, +12%.** (Default mode was 46 vs 47ms — the region
JIT covers that micro entirely, which is why only the interpreter-bound rows moved.)

Re-gated on `global_route_epoch != 0`, a single `u32` compare against a field that is
zero in every program that neither deletes a global nor puts a descriptor on one:

| row | run 1 (ground truth) | run 2 (epoch) |
|---|---:|---:|
| `map-set-heavy` (sentinel) | +3.9% [+0.5, +5.6] | **+1.8% [−0.5, +3.3]** — CI includes zero |
| `polymorphic-objects` | +2.4% [+1.2, +3.6] | **+1.5% [+0.5, +1.7]** |
| suite geomean | +0.32% [−0.08, +0.61] | **+0.23% [−0.16, +0.42]** |

The tradeoff is stated at the code: if a future path shadows a live slot without bumping
the epoch, that read falls back to the SLOT — the pre-B67 answer. Wrong, but no worse
than before, and **not a tier divergence**, because the JIT gates still read ground truth
through `global_slot_directly_routable` and so decline, leaving the interpreter to answer
consistently.

#### An engine table from the same session, and why it is NOT a new headline

15 pairs, node vs the B67 build, historical ten rows (`bench/head_b67b_15.json`):

| workload | node | zipp | zipp/node |
|---|---:|---:|---:|
| `map-set-heavy` | 681ms | 713ms | 1.03x |
| `class-prototype-hot` | 297ms | 378ms | 1.27x |
| `json-large` | 273ms | 449ms | 1.65x |
| `markdown-render` | 268ms | 446ms | 1.67x |
| `polymorphic-objects` | 325ms | 615ms | 1.89x |
| `async-promise-chain` | 331ms | 626ms | 1.89x |
| `sparse-array` | 79ms | 157ms | 1.98x |
| `parse-large-js` | 270ms | 597ms | 2.20x |
| `typedarray-math` | 204ms | 642ms | 3.15x |
| `regex-log-scan` | 459ms | 1671ms | 3.65x |

geomean **1.91x** [1.89, 1.92]; startup node 29.6ms vs zipp 8.5ms (0.29x).

**Do not read 1.8626x → 1.91x as a 2.5% regression, and do not install this as the
retained headline.** It is a different day on a host that was demonstrably drifting — the
`LoadGlobal` micro above measured its own baseline at 585ms in one run and 834ms in
another, and `map-set-heavy` here reports 1.03x against the retained 0.897x with a p10/p90
of 681/809ms, the widest spread in the table. B66's +0.40% and B67's +0.23% together
account for ~0.63% of the difference; the rest is host condition.

The A/B is the number that means something, because it is counterbalanced, same-session
and same-host: **+0.23%, CI includes zero**. M0.1's rebaseline still wants what it always
wanted — a quiet host, a deliberate A/A calibration alongside it, and the peak-RSS and
counter captures this run does not have. This table is an indicative snapshot, nothing more.

Two side lessons from having run it twice. `markdown-render` measured −2.0% [−3.1, −0.4]
in run 1 and +0.2% [−1.4, +1.1] in run 2 — an interval excluding zero that did not
replicate, exactly the ~1% drift M0.1 warns about; no claim was made for it. And the
`json-large` win DID replicate (−3.9% [−5.4, −2.2] then −3.9% [−5.0, −3.2]), which is
what makes it quotable.

### B67b — Contained: owned JSON keys, and the three corrected benchmarks

**Owned JSON keys (audit plan M2.2) — LANDED, UNMEASURED.** `json_parse_object` and
`json_parse_object_src` built `Vec<(String, Value)>` and then called `map.set(&k, v)`,
which cloned a SECOND copy of a key the parser had already allocated and dropped the
first. `ObjMap::set_owned(String, Value)` is `set`'s clone-free twin over the existing
`push_data`, and both sites now also `with_capacity(pairs.len())`. Duplicate-key
last-value-wins, first-insertion position, integer-key enumeration order, reviver
visitation order and `context.source`-reports-the-last-duplicate are pinned by
`tests/json_owned_keys.rs`; the reviver-order expectation was checked against node
rather than reasoned about, and the first guess was wrong (`b` comes FIRST — the
duplicate keeps its original position).

**Measured: `json-large` −3.9%, and it REPLICATED.** Two independent 21-pair runs against
clean `735535c`: −3.9% [−5.4, −2.2] (`bench/b67_ab_2026-07-30.json`) and −3.9%
[−5.0, −3.2] (`bench/b67b_ab_2026-07-30.json`). Same point estimate twice, both intervals
excluding zero, comfortably past the 2–3% target-row promotion threshold — and the plan's
own estimate for this item was only "unknown-small until measured".

Caveat on attribution: this A/B carries the whole B67 changeset, so −3.9% is the NET
`json-large` movement, not an isolated measurement of `set_owned`. The mechanism is
certain (one fewer allocation per first-inserted key, on the row whose generator produces
effectively random keys) and nothing else in the changeset plausibly speeds JSON parsing
up, but an ablation would be needed to attribute the number to this line alone.

**Three corrected benchmarks (audit plan M0.3) — ADDED, NOT in `ALLBENCHES`.** All three
produce byte-identical output to node. They are deliberately excluded from the timed
default set, because the retained headline is a geomean over exactly ten rows and adding
rows would silently redefine the number every earlier entry is quoted against. Run them
with `BENCHES="…" bash bench/run_real.sh`; `CORRECTBENCHES` checks them by default.

* **`property-ic-shapes.js`** — 1/2/4/8/9/16/1024 receivers sharing ONE shape, separate
  `GetProp` and existing-slot `SetProp`, plus distinct-shape, dictionary-mode and
  prototype-chain controls. **This is the acceptance benchmark for M3** and it exists
  before the implementation it judges. The wrap is an explicit counter, never
  `i & (n - 1)`: 9 and 1024 are not both powers of two and a mask would have benchmarked
  8 and 1024 while the phase name claimed otherwise. Each phase reads a distinct
  property name so phases cannot share an IC site and measure the order they ran in.
  First look: node 346ms, zipp 1814ms.
* **`polymorphic-objects-v2.js`** — the original row split into
  same-layout-many-instances (the case it omits entirely), 8/9/16 LAYOUTS, dict churn,
  proto walk and enumeration, each with its own checksum. node 166ms, zipp 442ms.
* **`sparse-array-v2.js`** — gap-size and logical-length curves with packed / holey /
  `in` / `for-in` / read / create-write / slice / concat separated, and a final phase
  that re-runs the hole reads with the indexed-proto protector INVALIDATED, so no
  hole/OOB fast path can be promoted on protector-valid numbers alone.

  Writing it produced a finding: **`concat` on a 4089-length sparse array costs zipp
  ~300 µs per call.** At the same iteration count as `slice` it was 6.0s of a 6.9s run —
  one builtin drowning every other phase, and the file would have reported "sparse
  arrays are 22× node". Sized to 1,000 iterations against `slice`'s 20,000, with both
  counts printed so the two numbers are not read as comparable throughputs.
  `slice(0, 64)` over the same receiver is ~1000× cheaper. node 305ms, zipp 1086ms.

Benchmark headers corrected per M0.4: `parse-large-js` now says it measures USERLAND
source tokenization and not zipp's frontend; `polymorphic-objects` says it stops at the
IC way count and cannot judge a shape IC; `map-set-heavy` says it is a no-regression
sentinel, not a target.

**Not started, with reasons.** `typeof`-local fusion (M2.1) — its promotion gate needs
an exact-HEAD baseline plus 15–21 pairs, and the compiler has no AST visitor, so the
pass is real work whose value cannot be confirmed in the same sitting; note the existing
fusion is an AST-level match in `compile/exprs.rs::binary`, so the local form belongs
there rather than in a bytecode def-use pass, and the safe rule is to fuse only when the
`typeof` operand is a LOCAL register (storing the operand in the local's slot and
rewriting each comparison to `TypeOfIs`), declining a bare global operand because
`typeof undeclaredGlobal` must not throw. `regexp_string_iters` → `SlotTable` (M2.3) and
`array_length_nonwritable` → `SlotSet` (M2.4) are unstarted; B66 already established the
latter is the right fix for the probe its guard-reordering attempt failed to buy back.

### B66 — Systematic sweep for the B59/B63/B65 shape: NINE tier divergences, six fixed, three left as failing specs

B65 argued that the remedy for a fact hand-maintained across tiers is one source
the compiler checks. Before building that, it is worth knowing **how big the
existing drift is** — so: a workflow that enumerated every guard the JIT applies
before compiling or before taking a fast path, recorded which of the six tiers
applies each, and hunted the asymmetries with runnable probes.

**Result: nine confirmed tier divergences, every one a silent WRONG ANSWER at
DEFAULT thresholds** — the interpreter and node agree, compiled code does not.
None is exotic; none was visible to 95,936 test262 executions in any of the three
passes. Each was verified by hand afterwards, four ways (default,
`ZIPP_JIT_THRESHOLD=1`, `ZIPP_NOJIT=1`, node), not taken on the agents' word.

**Fixed here (six):**

| # | what compiled code did | correct |
|---|---|---|
| 1 | `a.push(x)` on a FROZEN / sealed / non-extensible array succeeded and grew it | TypeError |
| 2 | …and on one whose `length` was made non-writable | TypeError |
| 3 | `a[3] = v` grew an array whose `length` is non-writable | no-op, `length` stays 3 |
| 4 | an append / HOLE-fill skipped OrdinarySet, so an `Array.prototype[i]` setter never ran | setter runs, no own index created |
| 5 | `o.m()` where `m` is an ARROW bound `this` to the RECEIVER | the arrow's captured `this` |
| 6 | …same for an arrow installed as a getter/setter via `defineProperty` | the arrow's captured `this` |

1–4 are one root cause: `jit_array_push` had **no** integrity guard at all, and
`jit_set_index` tested only `arr_props` — but a non-writable `length` lives in
`array_length_nonwritable`, a *different* side table, and creating a new index is
OrdinarySet rather than a store. 5–6 are one root cause with three call sites:
`HeapObj::Closure` carries the arrow's captured `this_val` and three separate
paths dropped it (`build_method_shape`'s own-slot arm, `ic_call_method`'s
`this_v = recv`, and both accessor arms in `jit_frame.rs`). Note that
`jit_self_call_impl` and `jit_fast_call_impl` **already had** the arrow guard,
with the same comment — a fourth instance of the same drift, inside one commit.

**Left open (three), as `#[ignore]`d failing specs in
`tests/jit_tier_parity.rs`** so `cargo test -- --ignored` is an accurate list:

* **A `StoreGlobal` to a REAL own property of the global object** (defineProperty'd,
  non-writable, accessor, or after `Object.freeze(globalThis)`) writes the slot
  directly instead of routing through `[[Set]]`. B65's guard only catches an
  UNINITIALIZED slot; here the slot is live *and* the object has a real own
  property, and the interpreter's `global_real_own_route` handles it. Present in
  Tier A/C, `region_globals_ok`, and `build_leaf_inline_plan` alike. The
  interpreter also differs from node on part of this, so it is a tier divergence
  AND a conformance gap — which is why it is described here rather than asserted
  in the test file.
* **`delete` of an implicit global returns its slot to the uninitialized
  sentinel**, and already-compiled code keeps reading the slot as `undefined`
  where the interpreter throws ReferenceError. B65's check is compile-time and is
  never re-validated at entry, so this is the one case where "once a slot holds a
  real value it can never go back" — the assumption `region_defer` and
  `compile_defer` both rest on — is false.
* **`setPrototypeOf(Array.prototype, x)` is invisible to `array_proto_has_index`**,
  so B63's fix still invents absence for an out-of-range index the new prototype
  supplies. The protector is set when an integer-like key is DEFINED on
  Array/Object.prototype; re-prototyping is a different mutation.

Two further candidates the agents reported were NOT reproducible under
independent re-run and are not counted: a nested-leaf-splice double-apply and a
strict-mode variant of the global-store hole (the latter is the same root cause
as the first open item).

**What this says about the plan's item 3.** Six bugs of one shape in three
commits, plus three more still open, all from cross-cutting facts maintained by
hand. The audit plan lists "central instruction effects and structured decline
reasons" as P0 infrastructure with no measured payoff attached. It now has one:
this file has spent B59, B63, B65 and B66 paying for its absence, and the cost is
not performance — it is that the engine returns wrong answers on ordinary code
while every gate is green.

**Cost: +0.40% [+0.10, +0.67], and that is a REAL cost, not noise.** 15
counterbalanced pairs (`bench/tier_parity_ab_2026-07-29.json`); every row inside
±1.2%; the interval excludes zero. Correctness is not optional, so it ships — but
it is recorded as a debit rather than rounded to "neutral", because this file's
own rule is that an interval excluding zero means something moved.

An attempt to buy it back was **refuted and reverted**: reordering the guards
cheapest-first and putting the `array_length_nonwritable` `HashSet` probe behind
its own `is_empty()` (the idiom five other sites use) measured **+0.55%
[+0.13, +1.06]** — indistinguishable from the unoptimised form, so it was pure
added complexity. The real fix for that probe is the B63/B64 one: it is a
heap-slot-keyed `HashSet`, and it wants a `SlotSet` sibling of `SlotTable`. That
is a separate, measurable change and it is not made here.

Gate: test262 95936/95942 with the FAIL set byte-identical in all three passes;
`cargo test --workspace --release` green; 5 new passing tests and 3 new failing
specs in `tests/jit_tier_parity.rs`.

### B65 — `ZIPP_JIT_THRESHOLD` makes test262 a JIT gate, and it found a wrong answer on its first run

B63 recorded a coverage hole rather than just a patch: §2 runs test262 under
`ZIPP_NOJIT=1` to prove the interpreter, but nothing forces the JIT — and the
region compiler only compiles hot LOOPS while test262 asserts once,
straight-line. **So Tier C and every JIT-only helper are gated by ten benchmarks'
stdout and nothing else.** B63's `arr[oob]` prototype divergence was found there
by hand, while doing something unrelated. This closes it.

`ZIPP_JIT_THRESHOLD=<n>` replaces both `JIT_THRESHOLD` and `OSR_THRESHOLD` for
the process. Read once in `Jit::new` into a `threshold_override` field where `0`
means "use the constants", so the count paths stay a field compare and a
perfectly-predicted branch — no env read, no atomic, and `Default` (which yields
0) cannot silently disable the tier.

**First run at `ZIPP_JIT_THRESHOLD=1` produced four new failures**
(`language/types/object/S8.6.2_A5_T1.js` and `_T2`, ×2 modes), and reducing them
found a bug that is NOT an artifact of the low threshold — it reproduces on the
committed binary at default settings with an ordinary 50-iteration loop:

| global created as | interpreter | Tier C |
|---|---:|---:|
| `var v = 0` | 50 | 50 |
| `this.count = 0` | 50 | **NaN** |
| `globalThis.g = 0` | 50 | **NaN** |

A binding created as an own PROPERTY of the global object does not live in the
`globals` slot array. The interpreter's `LoadGlobal` sees the slot uninitialized
and falls back to the global object's own property; Tier C emits
`mov rax, [r12 + idx*8]`, reads the uninitialized sentinel, and `x++` quietly
evaluates to `NaN`. Silent wrong arithmetic, not a crash.

**The guard already existed in two of the three places that need it** —
`region_globals_ok` in the region compiler (`dispatch.rs`) and the same scan in
`build_leaf_inline_plan`, whose decline message even names the cause: "reads a
global whose binding is an own property, not a slot". It was missing from the
whole-function Tier C path, which is exactly why loops were right and plain hot
functions were wrong. **That is B59's failure mode again** — the same fact
hand-maintained in three places, drifting in one — and the third instance in this
file. The audit plan's item 3 (one exhaustive `InstrInfo` + structured decline
reasons) is the standing fix; this is the second measured argument for it.

**And the class was already known.** `README.md` describes this exact bug, as one
of the two tier divergences that mattered most while getting test262 to 99.994%:
"`$262.evalScript`'s var/function bindings live as own properties of the global
object with the slot left `UNINITIALIZED`, and every JIT tier reads the slot
directly. A harness function called from a loop therefore worked for the
interpreted iterations and became `undefined is not a function` the instant the
region tiered up". It says *every* JIT tier — and the fix went into the region
compiler and the leaf planner, both of which are reached from a LOOP, and never
into the one tier a loop is not required to reach. The knowledge was written
down, in prose, in the README, and the code still drifted. That is the argument
for item 3 stated better than anything else in this file: the remedy for
hand-maintained cross-cutting facts is not to write them down more carefully, it
is to have one source the compiler checks.

Fixed by giving Tier C the same scan plus `Jit::compile_defer`, the sibling of
`region_defer` that was also missing. A function reading an own-prop global now
stays interpreted rather than compiling wrong — the same trade the loop path has
always made. Pinned by 6 tests in `tests/jit_global_own_prop.rs`, including the
late-binding case (the global appears only after the function is already hot, so
deferral must re-arm rather than blacklist).

**Cost: none measurable.** `--ab` against `5198911`, 15 counterbalanced pairs
(`bench/jit_global_guard_ab_2026-07-29.json`): geomean **−0.09% [−0.42, +0.34]**,
`ALL_CORRECT=1`. The one row nominally outside its interval is
`async-promise-chain` at **+1.8% [+1.1, +2.4]**, and it is NOT claimed as a
mechanism: that bench declares no own-prop globals, so nothing in it can defer,
and the two added costs are a once-per-compile body scan and a
perfectly-predicted branch on a field. B61 measured this class of row moving
±1.5% from pure code layout under fat LTO with one codegen unit. Recorded rather
than explained away — and it ships regardless, because it is a correctness fix.

**Gate updated: §2 step 2 now requires all THREE passes** — default,
`ZIPP_NOJIT=1`, and `ZIPP_JIT_THRESHOLD=1`. All three fail sets are byte-identical
to `tools/test262-expected-failures.txt` at this commit. It is one more test262
run, it needs no new tests authored, and it caught a silent wrong answer the
moment it existed.

### B64 — The same mechanism, four more tables: `proto_of`, `arguments_objs`, `array_js_len`, `fn_props` go slot-indexed — suite −1.06%

B63's closing note said `Vm` has ~40 more `HashMap<u32, _>` side tables keyed by
heap slot and that B63 was the first evidence about what that shape costs. This
collects on it. Reference counts said `proto_of` (190 uses) dwarfs the rest, but
count is not hotness — the four picked here are the ones probed on paths that run
per *operation* rather than per *object*, with no `is_empty()` guard in front:

* **`proto_of`** — every prototype-chain walk, and `proto_chain_blocks_set` probes
  it again at each of up to eight hops per property write;
* **`arguments_objs`** — `args_mapped_get` does an unguarded
  `arguments_objs.get(&idx)?` and is called from `get_index`'s Array arm and
  `member.rs`, i.e. on **every array element read**, for a table that is empty in
  any program with no mapped `arguments`;
* **`array_js_len`** — `js_array_len` probes it for **every array `.length`**,
  for a table only sparse arrays ever populate;
* **`fn_props`** — every named property access on a callable.

Deliberately NOT converted: `module_namespaces`, `deferred_ns_state` and
`obj_realm`, which sit on the same hot paths but *are* behind `is_empty()` and are
empty in every non-module program; and `array_length_nonwritable`, which is a
`HashSet` and would need a set variant for a colder path. Converting a cold table
buys nothing and costs its paged index.

All four are drop-in — the only new `SlotTable` API this needed is one
`Entry::or_default`, for `setup.rs:877`.

Measured `--ab`, 15 counterbalanced paired reps, `3eaf9ed` vs this
(`bench/slottable_4more_ab_2026-07-29.json`):

| row | old | new | paired | 95% CI |
|---|---:|---:|---:|---|
| `async-promise-chain` | 634ms | 618ms | **−3.1%** | [−3.7, −1.8] |
| `regex-log-scan` | 1706ms | 1657ms | **−2.9%** | [−4.8, −1.6] |
| `parse-large-js` | 597ms | 589ms | −1.2% | [−1.8, −1.0] |
| `sparse-array` | 158ms | 157ms | −1.1% | [−1.9, −0.6] |
| `json-large` | 481ms | 473ms | −1.0% | [−2.0, +0.2] |
| `markdown-render` | 460ms | 451ms | −0.7% | [−1.4, +0.6] |

**Suite geomean −1.06% [−1.68, −0.70]**, nothing regressed, `ALL_CORRECT=1`. Not
decomposed per table: they are one mechanism, B63 already priced that mechanism,
and the row profile (async and regex first, both prototype-walk-heavy) is what
`proto_of` predicts. Memory is self-limiting — `SlotTable` frees a page when its
last entry goes, so the two tables that are empty in most programs cost nothing.

**Running total for the container shape: −3.63% (B63) and −1.06% here, from a
data-structure choice nobody had priced.** The remaining ~35 tables are colder
and mostly `is_empty()`-guarded; the next one worth measuring is whichever a
profiler says, not whichever has the most references.

### B63 — The match-result side table, decomposed: 79% of it is the CONTAINER. `arr_props` goes slot-indexed — regex-log-scan −10.6%, async-promise-chain −12.4%, suite −3.63%

B33-C / RLS-1 again — the item B55 deferred and B60 called "the largest measured
item in the file". B55 priced it as construction cost (~456ns/match) and specified
an XL lazy sidecar gated on hand-bucketing ~256 `arr_props` uses. **The pricing was
wrong, and one extra ablation says so.**

**Re-price at HEAD first.** B60's −13.5% predates the lazy statics, so it is
against a bigger denominator. The same switch rebuilt on `7d10389`, `--ab-env`
against the identical binary, counterbalanced:

| ablation on `regex-log-scan` | result | 95% CI | reps |
|---|---|---|---|
| skip `index`/`input`/`groups`/`indices` entirely | **−14.9%** (1882→1600ms) | [−15.4, −13.7] | 15 |
| build the `ObjMap` exactly as now, then DROP it instead of inserting | **−12.2%** (1853→1629ms) | [−12.5, −12.0] | 11 |

**Read those two together: 79% of the prize survives when the three `String`s, the
three `Vec`s and all the shape work are still paid.** It was never the properties.
It is that they are parked in an `FxHashMap<u32, ObjMap>` that a burst inflates to
hundreds of thousands of entries, is probed with an effectively-random key, is
walked whole for roots on every collection, is `retain`ed whole afterwards, and
never shrinks. B55 attributed all 456ns to construction and designed against the
21%. Artifacts: `bench/ablate_regexp_result_props_2026-07-29.json`,
`bench/ablate_objmap_only_2026-07-29.json`.

**Landed: `crate::slot_table::SlotTable<V>`.** A side table keyed by heap SLOT
should not hash: slots are small, dense and handed out in ascending order. A paged
slot→dense-position index (1024-slot 4 KiB pages, released the moment a page's last
entry goes) over a dense value array — lookup is two dependent loads, insert is a
store and a push, and the GC's prune walks the LIVE entries instead of the capacity
a burst left behind. The API is deliberately the exact `HashMap` subset the VM uses
(`get`/`get_mut`/`entry(..).or_insert_with(..)`/`contains_key`/`is_empty`/`values`/
`retain`/`remove`/`insert`), so `arr_props`' type change is **two lines and zero
call-site changes across all 147 of them**, and cannot alter behaviour. Iteration
order is unspecified in both, and the only two iterations are the GC's root walk
and its prune.

Measured `--ab`, 15 counterbalanced paired reps, `bench/slottable_ab_2026-07-29.json`:

| row | old | new | paired | 95% CI |
|---|---:|---:|---:|---|
| `async-promise-chain` | 717ms | 629ms | **−12.4%** | [−12.9, −11.4] |
| `regex-log-scan` | 1883ms | 1687ms | **−10.6%** | [−10.9, −10.3] |
| `parse-large-js` | 607ms | 595ms | −2.8% | [−3.0, −1.7] |
| `sparse-array` | 162ms | 158ms | −2.4% | [−4.0, −2.0] |
| `json-large` | 485ms | 473ms | −2.3% | [−3.9, −0.7] |
| `markdown-render` | 462ms | 449ms | −2.3% | [−3.6, −1.0] |
| `map-set-heavy` | 745ms | 735ms | −1.3% | [−3.5, −0.1] |
| `polymorphic-objects` | 612ms | 604ms | −1.3% | [−1.8, −0.1] |
| `class-prototype-hot` | 382ms | 382ms | −0.2% | [−1.6, +1.2] |
| `typedarray-math` | 646ms | 647ms | +0.2% | [−0.6, +0.5] |

**Suite geomean −3.63% [−3.94, −3.37]**, nothing regressed, `ALL_CORRECT=1`.

**The async row is the interesting one, and it is not about regexes.**
`ordinary_set_ok` (`access.rs:717`) probes this table on EVERY ordinary property
write, and `proto_chain_blocks_set` (`:751`, `:769`) probes it again at every
prototype hop, up to eight. Both sit behind `!arr_props.is_empty()` — so **one
array named property, anywhere in the program, switches a hash probe per
write-plus-hop on for the rest of the process**. That guard is why the cost was
invisible to reasoning about regexes, and direct indexing is why it is now
nearly free. The lesson generalises: `Vm` has ~40 more `HashMap<u32, _>` side
tables keyed by heap slot, `proto_of` and `prototypes` among them, and this is
the first evidence about what that shape costs.

**Second half: a match result is not an element overlay. NEUTRAL on the suite,
and it cost two of its eight sites to a test262 regression — which is the part
worth reading.**

~15 dense-array fast paths ask "can an element of this array be shadowed?" and
answer it with `arr_props.contains_key(&idx)` — the presence of ANY entry. A match
result always has one, because that is where `index`/`input`/`groups` live. So
every `exec` result in every program fell off `map`/`filter`/`indexOf`/`slice`/
`join`/`for…of`/`JSON.stringify`, could not be pinned by the region planner, and
deopted the JIT on every `m[i]` — for four names that cannot shadow an element.
`ObjMap::overlays_elements` asks the precise question: is there a canonical index
key or `"length"`, or has an integrity level been applied? The bit is maintained by
the same three appends and one removal that maintain `shape`, and
`assert_map_consistent` recomputes and compares it.

**Then test262 rejected two of the eight narrowed sites, and B55's warning was
exactly right.** `staging/sm/Array/splice-species-changes-length.js` began failing
in BOTH tiers. Its array is set up with `array.constructor = {[Symbol.species]:
…}` — an own property that names no element, so `overlays_elements` says `false`
and `splice` took the dense `Vec` arm. But `splice` runs ArraySpeciesCreate, which
does `Get(O, "constructor")`; the species callback pushes three elements and makes
`length` non-writable mid-operation, and the dense arm sees none of it. `map` and
`filter` have the same exposure.

The lesson generalises past the one key. **`array_ops.rs`'s family gates are not
two paths to the same answer — they are two implementations of an observable
protocol, and only `array_like_*` implements all of it.** The presence test was
doing double duty: "no element overlay" AND, incidentally, "nothing here makes an
array method's spec machinery observable". Narrowing it kept the first meaning and
silently dropped the second. So those two gates keep the coarse
`contains_key`, with the reason written above them. The six that stayed narrowed
are pure ELEMENT READS with no array-method protocol in them at all — the JIT's
`a[i]` helper, the region pin, `i in a`, JSON's per-element read, and `for…of`'s —
plus `indexOf`/`lastIndexOf`/`includes`, which build no new array and never touch
`constructor`. Pinned by two new tests and re-gated on the full suite.

This is the first concrete instance of the failure mode B55's judge predicted for
the whole `arr_props` centralization: "one wrong bucket is a silent wrong answer".
It took **eight** buckets to produce one, and neither a 108-line reflection
differential nor eight hand-written unit tests found it — test262's `staging/sm`
corner did. Anyone sizing item 6's 147-site version should price that experience
in.

**It measured NOTHING on the suite: geomean +0.14% [−0.34, +0.88], every row
inside noise, `regex-log-scan` −0.2% [−0.7, +0.2]**
(`bench/overlay_narrow_ab_2026-07-29.json`, 15 pairs). The reason is mechanical,
and `ZIPP_JITLOG` says it plainly: **`regex-log-scan` does not region-compile its
`exec` loops at all** (a method call keeps them out), so the deopt gate is not on
its hot path — and the bench never calls `indexOf`/`for…of`/`JSON.stringify` on a
match result. The suite does not contain the pattern.

The targeted micro does — 2,000 live match results against 2,000 identical plain
arrays, the same operation on each, timed inside one process:

| operation | before | after | plain-array control (before → after) | node (match/plain) |
|---|---:|---:|---:|---:|
| `m.indexOf(s)` | 251ms | **127ms** | 126 → 127ms | 7/5 |
| `for (x of m)` | 197ms | **157ms** | 147 → 152ms | 5/4 |
| `JSON.stringify(m)` | 267ms | **218ms** | 197 → 196ms | 29/27 |
| JIT'd `m[i]` reads | 71ms | **59ms** | 62 → 55ms | 31/4 |
| `m.map(fn)` | 292ms | 273ms | 173 → 161ms | 8/7 |

Read the control column: a match result was **1.3–2.0× slower than a
byte-identical plain array** at the same operation, and afterwards `indexOf` is
exactly equal to one, `for…of` within 3%, and `JSON.stringify` within 11%. `map`
is the row that did NOT move, and that is the species revert below, not a
measurement failure.

**Retained deliberately as a neutral-on-suite change** under §2's rule: it removes
a real cliff on a common pattern, it costs one `bool` per `ObjMap` plus an
allocation-free key test on append, and the alternative reading — that this is
worth zero — is contradicted by the micro. Do not cite it as a suite win; it is
not one.

**Found while narrowing, and fixed here: a JIT/interpreter divergence.**
`jit_get_index` returned `undefined` for an out-of-range array index without
walking the prototype chain, so with `Array.prototype[5] = "P"` a hot `a[5]` read
`undefined` while the interpreter and node both read `"P"`. Three shapes:
`Array.prototype`, `Object.prototype`, and a `setPrototypeOf`'d custom prototype.
It survived 95,936 test262 executions **because the JIT only compiles hot loop
REGIONS and test262 asserts once, straight-line — `jit_get_index` is never
reached**. That is a coverage hole in the standing gate worth more than the patch:
§2 runs test262 under `ZIPP_NOJIT=1` to prove the interpreter, but has no
corresponding mode that forces the region tier, so every JIT-only helper is
gated only by the ten benchmarks' stdout. Match results were accidentally immune
here precisely because they always deopted, so narrowing the gate without this fix
would have shipped the bug straight into the regex row. Guarded now with the same
pair the `i in a` inline uses: the `array_proto_has_index` protector plus "no
custom prototype".

### B62 — `typeof` interned: json-large −6.7%, and the roadmap's own estimate for it was wrong in both directions

The last unrefuted row of B50's prize table said "`typeof` allocates its result
string, ~45ms suite-wide, 8 permanent interned slots would do it". Both halves of
that were off, and the way they were off is the useful part.

**"~45ms suite-wide" was wrong: it is ONE site.** B54 fused `typeof x === "lit"`
into `TypeOfIs`, which allocates nothing — so the estimate, written before B54,
counted sites that no longer exist. Counting `TypeOf` vs `TypeOfIs` in the actual
bytecode: `markdown-render` 0/0, `parse-large-js` 0/0, `polymorphic-objects` 0/0,
`json-large` **1**/0, `map-set-heavy` 0/**2**. So the entire suite exposure is
`json-large:68`, `var t = typeof v;` in `walk` — which escapes B54 precisely
because the result goes through a local and is then compared three times, not
compared inline.

**And the per-call cost was much worse than "an allocation".** Isolated, 5M
iterations:

| | zipp | node |
|---|---:|---:|
| `var x = typeof v; x === "number"` | **345ms** (65ns) | 8ms |
| `typeof v === "number"` (B54-fused) | 38ms (4ns) | 4ms |
| control, no `typeof` | 18ms | 8ms |

**16× between two spellings of the same operation.** Two mechanisms, not one:
`alloc_str(type_of(v).to_string())` allocated a fresh heap string per evaluation
AND, because each result was a distinct object, the `t === "number"` that follows
had to CONTENT-compare it.

**Landed:** the eight results are interned once in `setup_globals`, which runs
before `set_gc_floor`, so they sit below the floor and are pinned for the VM's
lifetime — no rooting, no per-GC work. `Vm::typeof_value` maps `type_of`'s
`&'static str` through `bytecode::TYPEOF_NAMES` (already the canonical table, used
by `TypeOfIs`) and both materialization sites — the interpreter arm and Tier C's
`jit_typeof` — now share it. Sharing is sound because heap strings are immutable
and a primitive's identity is not observable; `typeof a === typeof b` was already
true whenever the names matched.

Measured `--ab`, 15 counterbalanced paired reps: **json-large −6.7% [−8.5, −6.2]**
(499→463ms), suite geomean **−1.01% [−1.44, −0.59]**, nothing regressed.

The same run showed `markdown-render` −2.1% [−2.5, −0.7], and that is NOT claimed
as a win: `markdown-render.js` contains no `typeof` at all (0 `TypeOf`, 0
`TypeOfIs`), and B61 measured this exact row moving +1.5% from a pure code-layout
perturbation. Attribute it to layout until something explains it. `json-large` is
the only row here with a mechanism.

**The estimate was also wrong in the OTHER direction, which is worth recording
because it is the first time in this file.** Bottom-up, this should have been
~13ms: 788k evaluations (150,021 nodes × 6 rounds, minus nulls) × the 17ns the
microbenchmark attributes to the allocation. It measured 36ms. The difference is
second-order: 788k fewer heap strings is 788k fewer `live` increments, so fewer
collections on a row this file puts at ~22% GC. **Removing an allocation from a
GC-heavy row can beat its own direct cost** — the mirror image of B29/B49/B33/B61,
where allocation counts over-predicted. Neither direction is safe to assume; both
need the ablation.

Not done: the remaining 48ns. The interned handle still differs from the *bytecode
string constant* `"number"`, so `t === "number"` is still a content compare. The
real fix for that shape is to extend B54's fusion to a single-assignment local —
if every use of a `TypeOf` dst is an `Eq`/`Ne` against a string literal, rewrite
each to `TypeOfIs` and drop the `TypeOf`. The microbenchmark bounds that at
another ~44ns/call, i.e. ~35ms of `json-large`. That is a compile-side peephole,
not a runtime change.

Gate: test262 95936/95942, FAIL set identical (6/6); `cargo test --workspace
--release` green; 8 new tests in `tests/typeof_interned.rs`; a 22-value ×
8-property differential identical to node, to the pre-change binary, and under
both `ZIPP_NOJIT=1` and `ZIPP_GC_STRESS=1`; GC-stress also on a bounded
`typeof`-over-JSON-walk built for this (the full `json-large` under
`ZIPP_GC_STRESS=1` does not terminate in reasonable time, which is pre-existing).

### B61 — Build identity + an A/A refusal in the harness; and the async register-window allocation is REFUTED at 0.34%

Three things, from acting on an external audit of `7dfcfe8`.

**1. LANDED — `zipp --version` and `--ab` refusing two identical binaries.**

The motivation is a measurement failure, not tidiness. B60's first gate was
worthless: capturing the A-side binary with `git stash` + rebuild left
`target/release/zipp.exe` at committed HEAD, nothing rebuilt it after
`stash pop`, and so test262 and a 21-case differential ran HEAD against HEAD and
"passed" identically. Two holes made that silent:

  * nothing could ask a binary what it was built FROM. `zipp --version` did not
    exist, so `engine_metadata`'s probe recorded only a failure string, and the
    artifact's `git_commit` came from the harness's own `git rev-parse` — which
    for a DIRTY tree names the parent commit. The external audit hit exactly this
    and had to caveat its own baseline as "near-HEAD evidence".
  * `--ab` recorded each side's `sha256` and never compared them.

Now: a `build.rs` stamps commit, dirty flag, a digest of `git diff HEAD`, rustc,
target, profile, opt-level, features and RUSTFLAGS; `zipp --version [--json]`
prints it; `engine_metadata` embeds the JSON form as `build_identity`. The
`source` field is `<commit>` clean or `<commit>+dirty.<digest>` — so two builds
of DIFFERENT uncommitted edits have different identities, which a bare commit
hash cannot express. `reject_identical_ab_binaries` then makes an accidental
A/A a hard error (exit 1) BEFORE any measurement, with two deliberate escapes
that must keep working: `--allow-aa`, and per-side `--ab-env` differing (the
ablation-pricing idiom B60 relies on). Six tests in `tools/test_bench.py`.

`jit_enabled()` is exported from zipp-vm rather than `cfg!`d in the CLI: the
`jit` feature belongs to the VM crate, so a local `cfg!` reports every build as
interpreter-only.

**The build script had the bug it exists to prevent, twice.** Both were caught by
running the obvious probe — edit a source file, rebuild, see whether the reported
identity changes — and neither would have shown up in any test:

  * v1 declared `rerun-if-changed` on `.git/HEAD` and `.git/index` only. Editing
    `crates/zipp-vm/src` therefore did NOT re-run the script, so the rebuilt
    binary reported the previous tree's digest: two different builds, same
    claimed source. Exactly the failure the stamp is for.
  * v2 tried to fix that by enumerating `git ls-files`. But `git ls-files` with no
    pathspec lists only files under the CWD, and a build script's CWD is its own
    package — so it watched `zipp-cli` alone and the staleness survived. Cargo
    silently ignores a `rerun-if-changed` path it cannot match, so a wrong path
    is indistinguishable from a correct one.

v3 watches the `crates/` and `tools/` DIRECTORIES (cargo treats a directory as
"anything beneath it changed"), plus the root manifests. Verified: adding a line
to `zipp-vm/src/lib.rs` moved the digest `f57d79a1…` → `8cfbeea4…`, and removing
it moved it back — the same tree gives the same digest. The digest also covers
`git status --porcelain` WITH untracked names, so adding a new untracked source
file changes the identity even though its content is absent from `git diff HEAD`.

**Neutrality, and a layout lesson worth more than the feature.** A CLI-only
change with no runtime mechanism measured a REPLICATED `markdown-render` +2.1%
then +1.5% [+0.5, +2.4] and `json-large` +1.2%. Cause: release is fat LTO with
ONE codegen unit, so adding `format!` machinery reachable from `main` perturbs
hot-code placement. Marking `build_identity` `#[cold] #[inline(never)]` fixed it:
final A/B on the committed build is `markdown-render` +0.4% [−0.6, +1.4],
`json-large` −0.2% [−1.8, +0.8], geomean +0.13% [−0.90, +1.14] — every interval
straddling zero. **In this build configuration,
adding cold code is not free; mark it cold or it moves hot rows by ~1.5%.**

**2. REFUTED — reusing the parked async register window (audit §7.1).**

The audit sizes this at Part B's ~70ms, reasoning that every await resumption
`mem::take`s the saved `Vec`, copies it into `self.regs`, and later `split_off`s
a fresh one — one malloc + one free per resume, 1.5M times.

The mechanism is real and was implemented: keep the resumed-from buffer alive and
`clear()`/`extend_from_slice` the window back into it, so the round trip
allocates nothing. **Measured −0.34% [−0.79, +0.19]** on `async-promise-chain`
(21 paired reps) — interval straddles zero, fails the gate, reverted. Phase
timings agree it is real but small: Part B 99→94ms.

This is the FOURTH allocation-count over-prediction in this file (B29 ~0, B33,
the SmallVec matcher case, this). Removing an allocation is not worth its
allocation count.

Where Part B's gap actually is, phase-split (1.5M awaits each):

| | zipp | node |
|---|---:|---:|
| A then-chain, 1.5M links | 249ms | 154ms |
| B await resolved | 99ms | 30ms |
| B await, ~9 more live registers | **142ms** | **35ms** |
| C `Promise.all` 30k×100 | 241ms | 96ms |

The width row is the useful new datum: 9 extra live registers cost zipp +43ms and
node +5ms, and that scaling SURVIVES removing the allocation — so it is the two
memcpys of the window, ~2.7ns per register, not the malloc. The remaining ~63ns
per await is frame push + microtask queue + `run_loop` re-entry. Anyone
re-opening this should target those, not the `Vec`.

**3. An unsoundness in the audit, worth recording before someone implements it.**
Audit §7.1 step 1 says "if `self.regs` is empty during the normal microtask
drain, swap the parked vector into `self.regs`". That is UNSOUND. `self.regs` is
pinned for the VM's lifetime — `reserve_jit_regs` reserves the worst-case
capacity precisely so a native frame's raw window pointer can never dangle, and
`reg_capacity` records it for every growth guard. Swapping a different `Vec` in
changes the base pointer and invalidates both. Only `truncate` and
within-capacity `extend` are permitted.

### B60 — regex-log-scan, phase by phase: B8 was measured on the one pattern shape where we win, and the success path is where the cost is

**B8 says "the regex ENGINE is not the problem — matching cost is FLAT, we beat
V8 at scanning". That is true of the pattern B8 measured and false in general, and
the difference decides what to work on.** B8's probe is `/zqx/.test(s)` — a single
literal that NEVER matches, i.e. exactly the case `regress`'s memchr prefilter
answers. Every hot loop in `regex-log-scan` MATCHES (`ipMatches` is 150000 of
150000). Re-measured with `test` only, so no result object exists in any row and
the only variable is the pattern:

| `re.test(LINE)`, 300k calls, LINE = 112 chars | zipp | node | ×  |
|---|---:|---:|---:|
| `/zqx/` — B8's case, never matches | 113ns | 13ns | 8.7 |
| `/^2026-/` — anchored, hits at index 0, 5 literal bytes | **197ns** | **7ns** | **28** |
| `/ERROR/` — hits, 0 capture groups | 220ns | 17ns | 13 |
| `/\d{1,3}\.\d{1,3}/` — hits, 0 groups | 343ns | 20ns | 17 |
| `/\d{1,3}\.\d{1,3}/` — same regex, MISSES | 107ns | 13ns | 8.2 |
| `/(\d{1,3})\.(\d{1,3})/` — 2 groups | 453ns | 23ns | 20 |
| the bench's 4-octet ip pattern | 590ns | 33ns | 18 |

Read the third and fifth rows together: the SAME pattern costs 343ns when it hits
and 107ns when it misses. **Failure is cheap and success is not**, so B8 measured
the cheap half. `/^2026-/` isolates it — anchored, immediate hit, nothing to
search — at 28× for essentially no matching work. Decomposed:

    fixed per-call floor      ~113ns   (node ~13ns)   8.7x
    success bookkeeping        ~85ns   (node  ~4ns)    21x
    per capture group          ~60ns   (node  ~4ns)    15x
    the actual MATCHING         ~4x off, and flat in subject length as B8 said

**So the matcher is the least of the four problems, and a compiled-regex backend
(B8b) is aimed at the smallest term.** Do not start there.

**Phase table** (150k lines, `Date.now()` around each phase, min of 3):

| phase | zipp | node | ×  | share of the 1579ms gap |
|---|---:|---:|---:|---:|
| corpus generation — **contains no regex** | 594ms | 164ms | 3.6 | **27%** |
| `test`, literal `/\[ERROR\]/` | 58ms | 73ms | **0.79** | −1% |
| `exec` + 4 captures + `+m[i]` | 258ms | 21ms | 12.3 | 15% |
| `replace(/\/\/+(\w+)/g, "/$1")` | 83ms | 31ms | 2.7 | 3% |
| `join("\n")` + fnv1a over it | 120ms | 28ms | 4.3 | 6% |
| `matchAll` + `for-of` | 784ms | 70ms | 11.2 | **45%** |
| `test`, anchored alternation | 78ms | 9ms | 8.7 | 4% |

`matchAll` decomposes further (450k matches): 257ms is match+bookkeeping, 186ms
result-object construction, 26ms group reads, 107ms the iterator. `for-of` itself
is NOT the blocker — driving the same iterator by hand with `.next()` is SLOWER
(803ms vs 576ms), and `for-of` over a plain 5-element array 150k times is 19ms.

**Two ablations, each priced against the same binary** (`tools/bench.py --ab-env`,
so the schedule is counterbalanced and the binary identical; both leave the
bench's output byte-identical because it reads neither):

| ablation | regex-log-scan | 95% CI |
|---|---|---|
| skip the Annex B legacy-statics refresh | **−8.65%** (2015→1844ms) | [−8.86, −7.77] |
| skip the result array's `index`/`input`/`groups` | **−13.5%** (2009→1738ms) | [−14.09, −13.14] |
| both | **−21.4%** (2011→1576ms) | [−22.51, −21.18] |

Nearly additive, and together they are 4.46× → ~3.50× on the row, ~2.4% of the
suite geomean. **Neither ablation is shippable** — the statics and the result
properties are both observable — so each needs a real implementation. One landed:

**LANDED: the legacy statics are now lazy — regex-log-scan 4.46× → 4.12×.** On the
21-rep suite that is 2010ms → 1842ms and a geomean of 1.98× → 1.95×. Slots 2..=13
(`lastParen`, `leftContext`, `rightContext`, `$1`..`$9`) are all slices of the
subject, and `ascii_slice_value` COPIES: `as_bytes()[r].to_vec()`, an `is_ascii`
rescan inside `from_wtf8`, and a heap slot. So the eager form copied
`leftContext` + `rightContext` — together ~87% of the subject — on every
successful match, `test` included (the refresh sits above the `!build`
early-out), plus one slice per capture that the result array then sliced AGAIN.
For ~800k successful matches on ~112-byte lines that is ~59MB of memcpy and ~1.2M
heap strings, and virtually no program reads `RegExp.leftContext`.

Now: root the subject, keep unit ranges (`RegexpLastLazy`), and materialise all
twelve on the first getter read (`regexp_last_materialise`). Slots 0/1 stay eager
— `input_val` is already a Value and `lastMatch` is computed for the result array
regardless. Only a flat-ASCII subject defers, because a non-ASCII slice reads the
locally decoded `u16s` buffer that does not outlive the call; that arm is
unchanged. The deferred record roots the subject, or a collection between the
match and the read would free the bytes still to be sliced.

Measured `--ab` (committed HEAD `23975df` vs this, 15 counterbalanced paired reps,
both binaries retained): **regex-log-scan −8.5% [−8.8, −8.2]**, 2003→1832ms — i.e.
the whole −8.65% ablation ceiling. Suite geomean −1.18% [−1.53, −0.90]. Nothing
off-target: every other row inside ±1.1%. Two rows read nominally better
(`json-large` −2.6%, `async-promise-chain` −1.1%); the plausible cause is ~1.2M
fewer short-lived heap strings and so fewer collections, but that is NOT claimed —
it is one session and below the ~1% replication floor this file insists on.

Gate: test262 95936/95942 with the FAIL set byte-identical to
`tools/test262-expected-failures.txt`; `cargo test --workspace --release` green;
12 new tests in `tests/regexp_legacy_statics.rs`; a 21-case × 19-static
differential byte-identical to the PRE-CHANGE binary and, separately, identical
under `ZIPP_NOJIT=1` and `ZIPP_GC_STRESS=1`.

**A process note worth more than the patch.** The first pass of this gate was
WORTHLESS and said so only because a number failed to move: `git stash`/rebuild
to capture the A-side binary left `target/release/zipp.exe` at committed HEAD, and
nothing rebuilt it afterwards, so test262 and the whole differential ran HEAD
against HEAD and "passed" identically. The tell was the following suite run
reporting `regex-log-scan` at 4.42× — unchanged. `sha256sum` on the three
binaries showed `zipp.exe` byte-identical to the saved A side. **Hash the binary
you are about to gate, and check that a measurement moved in the direction the
change predicts before believing a green gate.**

**NOT landed, and the bigger half: the result array's `index`/`input`/`groups`
(−13.5%).** They cannot be skipped, but the price is not the three
`"index".to_string()` allocations — it is that they live in `arr_props`, an
`FxHashMap` keyed by heap index. Every `exec`/`matchAll` result inserts an entry
plus a side-table `ObjMap` with three `Vec`s, the map grows to hundreds of
thousands of entries inside one phase, and GC prunes it wholesale. That matches
this file's own 456ns-to-create measurement almost exactly: 456ns × ~600k
matches ≈ 274ms ≈ the measured 271ms. Note the shape of the fix is NOT the
already-refuted result-object POOL: it is getting those three names off the side
map, e.g. a dedicated match-result heap variant that answers them directly.
Anyone picking this up should re-price it with `ZIPP_NO_REGEXP_RESULT_PROPS`-style
ablation first — that switch was removed with this commit rather than left in as
observably-wrong code behind an env var.

**Third item, unpriced and NOT regex: corpus generation is 27% of this row's
gap** — 594ms vs 164ms of string concat, `Array` stores and number→string, with
no `RegExp` in it at all. Anyone trying to "fix regex-log-scan" should know that
more than a quarter of it is the general string/number path.

Also found: `String.prototype.replace` with a GLOBAL regex does not refresh the
legacy statics (they keep the previous match's values); V8 refreshes them. Present
in the pre-change binary too, so it predates this work — recorded in §6.

### B59 — `SuperBase` was not in the method-inline whitelist: class-prototype-hot had silently regressed 1.27× → 7.99×

**The single largest performance item in this file, and it was a restoration, not
an optimisation.** Between the `1388621` benchmark artifact and `799ead6`,
`class-prototype-hot` went from 381ms / **1.27×** to 2340ms / **7.99×** — and the
cold suite geomean from 1.90× to **2.38×**. Nothing in this document recorded it.
Fixed; re-measured at 378ms / **1.28×** and geomean **1.98×** (21 counterbalanced
paired reps, `bench/final_2026-07-29.json` against
`bench/opt_baseline_2026-07-29.json`, node medians agreeing within 1% row-for-row,
`ALL_CORRECT=1`). Nine of the ten rows did not move: the entire 0.40 geomean delta
is this one row.

**Cause.** `aca09d3` split `super.m()` codegen into a separate `SuperBase`
capture plus `SuperMethod { base }`, because GetSuperBase happens at
MakeSuperPropertyReference time — before the argument list runs. The compiler now
plants `SuperBase` ahead of every `super.m()`, `super.x` write and computed super
form (`compile/calls.rs:687`, `compile/assign.rs:296,304,787,907`,
`compile/exprs.rs:1830,1865`). Two whitelists gate super bodies, and only one was
taught the new op:

  * `method_body_inlinable_scan` (`vm/engine/method_inline.rs:171`) — the
    OFF-FRAME evaluator. Updated in the same commit. Still worked.
  * `method_inline_body_ok` (`vm/engine/jit_plans.rs`) — the NATIVE region
    inliner. Not updated, so every super-using body hit its `_ => return None`.
    `build_method_shape` / `build_accessor_shape` then declined, and
    `build_method_inline_plan` returned **silently** at its `shapes.is_empty()`
    continue — no log line, no counter, no decline reason.

So B51/B52's inlined `super.m()` / `super.v` / `super.v = x` all stopped being
emitted, and `objs[i & 3].area()` fell back to two nested frame calls:

    per-call, 8M iterations              was      now
    mono `super.area()`                55.8ns    6.9ns
    4-shape polymorphic, 2 use super   32.6ns    6.6ns
    same method with NO super           4.3ns    4.3ns   (never regressed)

    class-prototype-hot phase          was      now     node
    32M polymorphic method calls      1923ms   261ms     40ms
    8M accessor round-trips            339ms    63ms    183ms   (now 2.9x FASTER)
    8M depth-5 proto-chain reads        61ms    62ms     31ms   (untouched)

**Why it hid.** `ZIPP_NO_METHOD_INLINE=1` changed the regressed timings by 0ms —
the kill switch for a mechanism that was already fully declining looks exactly
like the mechanism being absent. There is no `[mi] … DECLINE` log to pair with
`[mi] … INLINE`, and test262 cannot see it: these paths only compile in hot
loops, and the output stayed byte-identical the whole time. It was found by
re-running the suite at HEAD rather than trusting this file's table.

**The fix** admits `I::SuperBase { dst, .. }` under `allow_super` and drops the op
in `emit_mi_body`. Dropping it is what needs the argument: the inlined Super* arms
resolve through their BAKED plan (class epoch + one version guard per chain hop +
a holder-slot re-read) and never dereference `base`, so the register the capture
writes has no inlined consumer. Rather than assert that, `mi_super_base_dst_dead`
PROVES it per body — it enumerates the reads of exactly the ops
`method_inline_body_ok` admits, with the `base` fields deliberately excluded, and
counts anything it does not recognise as a read. Growing that whitelist without
revisiting it therefore makes a body DECLINE, never silently read a register the
emitter left stale. That is the property whose absence caused this entry.

**Standing-gate lesson, and it is the same one as B50.** Two admission lists for
the same concept drifted apart, silently, and the cost was 6× on a benchmark row
for an unknown number of commits. B50 converged three of them and said so. This
is the fourth. Any new bytecode op must be added to every whitelist that
enumerates ops, or the emitters must share one list.

**Two pre-existing spec deviations surfaced while writing the differential tests**
(both identical under `ZIPP_NOJIT=1` and on the pre-fix build, so neither is
caused by inlining — pinned in `tests/super_method_inline.rs` as tier-consistency
tests and recorded in §6):

  * `super.m(arg)` where `arg` re-targets the chain: zipp resolves the METHOD
    after the argument list, V8 before it (13.3.6.1 GetValue precedes
    ArgumentListEvaluation). `aca09d3` captured the base up front but not the
    callee, so the ordering fix is only half done.
  * Re-executing a class declaration retargets an OLD instance's `super`, because
    `super` resolves through the one `class_values` slot a `class_id` owns.

**Also re-confirmed here: B22/B32 stand.** Admitting a DV-pinned integer
`CallMethod` to the INT tier was built again (`dv_get_kind_int`, planner
receiver-exemption, and the int-lane load with both endianness branches) and
**reverted again**. It moves the log line from `INT decline: region_is_int=false`
to `[decline-reason] pinned receiver reg not cleanly excludable` and nothing else
— `dv_ms` 370 → 361, inside noise — exactly the second gate B32 names. The
emitter is correct and unreachable, which is the B9 failure mode. Do not restart
it before the receiver multi-def blocker; and note B32's other gate too (a 43-op
region exceeds the 14-home pool).

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

> **This table has itself aged — re-audited 2026-07-29.** Entries in this file are
> newest-first, so B51/B52 landed AFTER B50 and closed row 1; B60 priced row 2 and
> landed part of it; row 3 landed as B57. Rows are struck through rather than
> deleted so the prize estimates stay checkable against what actually happened.

| item | prize | where |
|---|---|---|
| ~~accessor inlining declines on `super.v`~~ **LANDED, then regressed, then re-fixed** | ~~**~300ms**~~ | Landed as B51 (getter) + B52 (setter). Then the `SuperBase` opcode arrived and one whitelist was not taught it, silently costing 6× on this row until **B59**. The setter hazard this row flags — the setter living in `attrs[slot].setter`, not `vals` — is exactly what `ic_super_setter_baked` handles |
| ~~the match result's `arr_props` side table~~ **MOSTLY COLLECTED as B63, suite −3.63%** | ~~~190ms est~~ — re-priced at HEAD as −14.9%, then DECOMPOSED: **79% was the container**, and a slot-indexed table collected it with zero call-site changes | `regex-log-scan` −10.6%, `async-promise-chain` −12.4%. What remains for the XL "compact metadata" project is the ~21% residue — see B63 before committing to the 147-site centralization it was gated on |
| ~~`o["k" + i] = v` fusion, done soundly~~ **LANDED as B57, −16.2%** | ~~~108ms~~ | `polymorphic-objects`; the second attempt was the sound one (`ToConcatKey`) |
| ~~`ToPropKey` invisible to `writes_reg`/`instr_uses`~~ **LANDED as B53** | ~~~39ms~~ | `typedarray-math` `normalize`. The external audit's do-not-repeat list names re-adding this |
| ~~`typeof` allocates its result string~~ **LANDED as B62: json-large −6.7%** | ~~~45ms suite-wide~~ — the estimate was wrong BOTH ways: it is ONE site in the whole suite (B54 fused the rest), and it measured 36ms not 13ms because of second-order GC | Interned in `setup_globals` below `gc_floor`. Remaining ~44ns/call needs B54's fusion extended to a single-assignment local — see B62 |

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

**Do not read that +0.4% as "M3 was tried and failed" — it prices a different
thing.** B44 made no codegen change at all: what it measured is the MISS path in
plain Rust (a `(site, shape) -> slot` memo and a thrash-gated refill). The
shape-keyed guard in the EMITTED probe — the route to a call-free shape hit —
is still unbuilt, and `region_mem.rs` still opens each way with `cmp rax, [r9]
// identity`. So M3's codegen half is UNMEASURED, not refuted. What the +0.4%
does license is the narrower claim that the ten timed rows would not pay for it:
no timed row has a constant-name `GetProp`/`SetProp` site with more than eight
same-layout receivers. (`polymorphic-objects` does build 30,000 objects over
~1,071 shapes, but every access in that loop uses a COMPUTED key, which no
inline cache in this engine serves — so it is not the counter-example it looks
like. B78's adversarial pass raised it, and that is the answer.)

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

- **`String.prototype.replace` does not refresh the Annex B legacy statics** —
  with a global regex OR a non-global one (B71 checked the latter; the entry used to
  say "global").** After `"a//b //c".replace(/\/\/+(\w+)/g, "/$1")`, `RegExp.$1`
  and friends still hold the PREVIOUS match's values; V8 refreshes them from the
  last replacement match. The per-match path there does not funnel through the
  `regexp_exec_impl` refresh. Predates B60 (the pre-change binary agrees), and it
  is the one row where a 21-case all-19-statics differential disagrees with node.
- **`super.m(arg)` resolves the method AFTER the argument list.** Per 13.3.6.1
  the reference's `GetValue` — the `super.m` lookup itself — precedes
  ArgumentListEvaluation, so an argument that calls
  `Object.setPrototypeOf(C.prototype, other)` must not affect the call it is an
  argument to. `aca09d3` fixed the BASE half of this (the `SuperBase` capture)
  and not the callee half, so zipp prints `LATER2` where V8 prints `A2`. Both
  tiers agree; pinned by `super_ordering_argument_list_swap_is_tier_consistent`
  in `tests/super_method_inline.rs`. Closing it means carrying the resolved
  callee out of `SuperBase`, not just the base.
- **Re-executing a class declaration retargets an older instance's `super`.**
  `super` resolves through the single `class_values` slot a `class_id` owns, so
  a second evaluation of the same `class B extends A` — a class declared inside
  a function called twice — makes the FIRST instance's `super.m()` reach the
  SECOND `A.prototype`. V8 gives each evaluation a distinct class. Both tiers
  agree; pinned by
  `super_method_inline_class_redefinition_is_tier_consistent`. The fix is a
  per-closure home object rather than a per-class-id slot.
- **TypedArray named properties ignore the prototype chain.**
  `vm/props/member.rs` answers `length`/`byteLength`/`byteOffset`/`buffer`/
  `BYTES_PER_ELEMENT`/`@@toStringTag` from the instance, so
  `Object.setPrototypeOf(ta, {length: 7}); ta.length` reports the TypedArray's
  length where V8 reports 7. The spec-correct lookup is implemented
  (`ta_named_is_intrinsic`, `vm/typedarray.rs`) but cannot be enabled until
  **A5** — with a faithful walk, cross-realm TypedArrays return `undefined` and
  24 currently-passing tests break.
- **Nine own-property divergences found by B63's adversarial review, all
  pre-existing** (verified identical on the pre-B63 binary; none is test262-
  visible). Each snippet below is `node` vs `zipp`:
  - `String.prototype[7]="SP"; "ab"[7]` — `"SP"` vs `undefined`. The String
    exotic's out-of-range index never reaches the prototype chain. Both tiers.
  - `(function(){return Object.getOwnPropertyNames(arguments)})(1,2)` —
    `["0","1","length","callee"]` vs a **duplicate** `"length"`.
    `descriptors.rs:539` pushes `"length"` unconditionally and the named tail
    then emits the stored one. Same in `Reflect.ownKeys`.
  - an index override on a HOLE below the dense length vanishes from
    `Object.getOwnPropertyNames` while `Object.keys` still reports it — zipp
    disagreeing with itself. `enumerate.rs`'s dense loop consults
    `array_index_override`; `descriptors.rs`'s does not.
  - `"use strict"; Object.seal(a); a.length = 0` — TypeError vs silently
    succeeding. `array_shrink_blocker` scans only per-key non-configurables, and
    a sealed exotic's elements are non-configurable by the marker, with no key.
  - `propertyIsEnumerable` on an Array ignores holes, non-canonical keys and
    index attributes (`iterate.rs:571-580` short-circuits on
    `i < items.len()`): `delete a[1]; a.propertyIsEnumerable("1")`,
    `a.propertyIsEnumerable("01")`, and a non-enumerable index override all
    report `true` where node reports `false` — and `Object.keys`/`for…in`/
    `JSON.stringify` all honour the attribute that this does not.
  - `Reflect.set(Object.preventExtensions(function f(){}), "nk", 1)` — `false`
    vs `true` (the `fn_props.extensible` quirk).
  None is a tier divergence, which is why the §2 gate never saw them.
- **The receiver-kind builtin fast paths ignore PROTOTYPE OVERRIDES.** `dispatch_builtin_method_inner`
  binds a builtin from the method NAME plus the receiver's heap kind, and only
  `toString`/`valueOf`/`toLocaleString`/`toJSON` are deferred to the prototype chain
  (`vm/builtins.rs:95`). So `String.prototype.indexOf = f; "abc".indexOf("b")` gives `1`
  where node gives the override's result, and the same for
  `Array.prototype.slice`/`Map`/`Set`/`Date`/TypedArray methods. An OWN method on the
  instance is honoured (the IC/get_prop path sees it); it is the PROTOTYPE replacement
  that is missed. RegExp is unaffected only because it has no fast-path arm at all —
  which is why B68 declined to add one without the M7.1 protector first. Both tiers
  agree, so no gate sees it. Same family as B63's nine own-property divergences.
- **`eval`, `new Function`, and every MODULE body are never JIT-compiled.**
  `dispatch.rs` gates the function JIT *and* the OSR tier on
  `func_id < self.main_func_count`, and runtime-compiled functions — including
  module bodies, which `prepare_eval_program` also parks in `eval_funcs` — sit
  past that bound. So a module's hot loop always interprets. Found while fixing
  `jit_set_prop_miss`'s `program.functions[..]` out-of-bounds (B67): the OOB is
  unreachable *because* of this gate. Lifting the gate is a real performance item
  and needs the `vm.func` route audited everywhere the JIT resolves a name.
- **`StoreGlobalResolved` has no `global_real_own_route` arm.** The other four
  global-store ops route through `[[Set]]` when the global object shadows the
  slot with a real descriptor; this one (strict `x += 1` / `x++` after a
  `CheckGlobalResolvable`) writes the slot. Both tiers agree, so it is a pure
  conformance gap, not a divergence — but fixing it will CREATE a divergence
  unless the JIT's `global_slot_directly_routable` check covers the same op,
  which it already does. Reported by B67's recon and left alone.
- **`Object.freeze(globalThis)` does not make slot-backed bindings non-writable.**
  Freeze marks existing `ObjMap` keys non-writable, and a `var x` / `x = 1`
  binding has no key, so `global_real_own_route` stays false and `x = 2` still
  writes the slot. Both tiers agree. The B67 materialization fix covers
  `defineProperty` on a single name; freeze would need the same treatment applied
  to every live slot binding at once.
- **The standing gate cannot see a JIT-only bug.** test262 runs under the
  default tier and under `ZIPP_NOJIT=1`, but the region JIT only compiles hot
  LOOPS and test262 asserts once, straight-line — so a helper like
  `jit_get_index` is never reached by 95,936 executions. B63 found a real
  `arr[oob]` prototype-chain divergence that way, by hand, while doing something
  else. The missing piece is a force-the-tier mode (an `OSR_THRESHOLD`
  override), which would make the existing suite a JIT gate at no authoring cost.
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
- **`ObjMap::set_attr_at` — the method written to enforce the shape invariant —
  has ZERO callers, and the one place that needs it writes `attrs` raw.**
  `heap.rs:377` exists so that an in-place attribute change drops the object to
  `shape::DICT`; nothing in the crate calls it. `vm/engine/eval_prog.rs` instead
  does `m.attrs[i] = attr;` directly on the GLOBAL object's map, and under
  `script_gdi` that attr carries `configurable: false` over a previously
  configurable property — a shape-relevant flag change that leaves the shape
  claiming the old bits. The same block then calls `m.define(&name, v, attr)`, a
  key ADD on a live object, with no `Heap::bump_version`. Both are contained
  today *only* because `ic_obj_ok` (`vm/ic.rs`) bans `globalThis` from every
  cache in the engine — so neither is reachable as a wrong answer, and neither
  would survive that exclusion being relaxed. Found by B78's adversarial audit
  of the premises a shape-keyed cache would rest on; verified by hand, not taken
  from the report. Any future shape-keyed IC has to close both first.
- **`vm/construct/construct.rs` replaces a whole `HeapObj` at an allocated index
  — freeing the old `ObjMap` and its `vals` buffer — with no version bump**
  (`*self.heap.get_mut(oidx) = cloned`, ~10 sites, `grep -c bump_version` is 0
  for the file). Audited and judged BENIGN, and recorded so the next audit does
  not have to re-derive it: `oidx` is the instance the `class D extends
  DataView` construction just allocated, and a slot recycled from the free list
  has already had its version bumped by `Heap::alloc`, so no live cache entry
  can name it. The same reasoning is what `bytecode.rs` writes out for
  `InitDataProp`/`AppendDataProp`. It is one `bump_version` away from not
  needing the argument at all.
- **An ACCESSOR `Promise.prototype.then` is read TWICE per call.**
  `Object.defineProperty(Promise.prototype, "then", {get(){…}})` then
  `p.then(f)` runs the getter twice where node runs it once — an observable
  side effect, duplicated. When `dispatch_builtin_method_inner`'s Promise arm
  cannot prove the intrinsic it re-proves with `get_prop` (running the getter),
  returns `Ok(None)`, and the caller's `get_prop` + `call_value` runs it again.
  Both tiers agree. Confirmed PRE-EXISTING via `ZIPP_NO_PROMISE_PRISTINE=1`.
  Pinned at the wrong value on purpose by
  `an_accessor_on_the_prototype_is_not_taken_as_intrinsic` in
  `tests/promise_pristine_dispatch.rs`, so closing it makes that test fail
  rather than pass silently. The fix is for the arm to hand the resolved callee
  back to the caller instead of discarding it — which would also delete the
  second lookup on the ordinary path.
- **A null-prototype Promise answers `p.then(f)` instead of throwing.**
  `Object.setPrototypeOf(p, null); p.then(f)` returns without error where node
  throws `TypeError` — there is no `then` anywhere on that receiver's (empty)
  chain, so the call has nothing to invoke. Both tiers agree, so no gate sees
  it. Found while building B79 and confirmed PRE-EXISTING by running the same
  case with `ZIPP_NO_PROMISE_PRISTINE=1`, which restores the old `get_prop`
  proof exactly: both paths print `nothrow`. The bug is in what
  `dispatch_builtin_method_inner`'s Promise arm does after its proof FAILS — it
  returns `Ok(None)` and the caller's `get_prop` + `call_value` should raise,
  and does not.
- **CI is intentionally narrow.** `.github/workflows/arm64-native.yml` runs the
  native ARM64 VM gate on Linux, Windows, and macOS, plus the Linux tier
  differential and sandbox slices. The complete cross-platform/performance gate
  in §2 is still run by hand.
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
