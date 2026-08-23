# Handoff — the Node-parity performance campaign

Written at the end of a long working session, for whoever picks this up next
(including a future me). `PERF_ROADMAP.md` is the authoritative ledger — every
claim below is an entry there with its measurements. This file is the map.

---

## Where the project is

Published capture `bench/head_clean_0f1a4c7_pgo.json`, `publishable: true`:

| | |
|---|---|
| **Headline ten** | **1.10× Node** (was 1.2832× ten waves ago) |
| vs Bun / vs Deno | **0.92×** (zipp faster) / 1.10× |
| Conformance | 99.994% test262, unchanged at 6 expected failures |
| Startup | 7.9ms vs Node 29.5, Deno 45.7, Bun 56.2 |

Row ratios (cold wall, vs Node): map-set 0.82, class-prototype-hot 1.01,
async 1.03, markdown 1.04, json 1.05, typedarray 1.10, sparse-array 1.13,
parse-large-js 1.25, polymorphic-objects 1.28, regex-log-scan 1.40.
Diagnostics: sparse-array-v2 1.98, poly-v2 2.60, property-ic-shapes 3.07.

**Node parity is a three-row problem.** `regex-log-scan` and
`polymorphic-objects` at parity reaches 1.04×; adding `parse-large-js` reaches
1.01×. Everything else is already within 13%.

---

## THE IMMEDIATE THING: unverified work is committed

**Wave 21 is unfinished. It has been COMMITTED SO NOTHING IS LOST, in a commit
whose message says plainly that it is unverified — but it has NOT passed a
gate, and it must not be treated as landed work or included in any published
number until it does.**

It builds clean, and all 13 bench rows are byte-identical to Node on a quick
smoke check. That is *not* sufficient evidence in this repo — see "What the
benchmarks cannot see" below. If the gate below finds it unsound, reverting
that commit is the expected outcome, not a setback.

Three lanes landed code, five new switches: `ZIPP_NO_BOX_HOME`,
`ZIPP_NO_REGALLOC_GETPROP`, `ZIPP_NO_OWN_ACCESSOR_INLINE`, `ZIPP_NO_INT_PUSH`,
`ZIPP_NO_BOOL_REUSE`.

| lane | mechanism | projected |
|---|---|---|
| poly | shared property-name strings + own-accessor inlining | poly −35/−45ms |
| BOXREF | `VTy::Box` + two call-free heap fast paths, + a missing `GetProp` arm | poly −50ms, sparse −4ms, **property-ic-shapes −200/−240ms** |
| parse | INT-tier `arr.push(int)` arm + bool-home linear-scan reuse | parse tokenize 178 → ~85-100ms |

**Before any of it lands, it needs the verification gate that never ran:**

1. Full suites (`zipp-vm`, `zipp-regress --features rx-jit`, `zipp-cli`).
2. **A large fuzzer soak on fresh seeds, including `ZIPP_GC_STRESS`.** This is
   non-negotiable for the parse lane: it *widened the INT tier to permit
   allocation inside a region holding live values in registers and pin
   snapshots*, so a GC can now run where it previously could not. That is the
   exact shape of three wrong-answer classes this campaign already fixed. Build
   targeted probes: hot INT loops pushing to a dense array while holding live
   pinned receivers and bool homes, forced to collect mid-loop.
3. **Semantic matrices the fuzzer cannot reach**, because lane 1 changed the
   object model's key representation: property enumeration order (integer-like
   keys before string keys, insertion order within each), `Object.keys` /
   `getOwnPropertyNames` / `for-in` / `JSON.stringify` agreement, key identity
   through `Object.keys(o)[0] === "x"`, delete-and-re-add, accessors own vs
   inherited, `defineProperty` after the fact, Proxy receivers — sloppy *and*
   strict, against Node.
4. Byte gate: 13 rows × {no-env, `ZIPP_NOJIT=1`, each new switch}.
5. Paired A/B against a scratch build of `3a8e58e`, 21 reps, all 13 rows.
6. **The parse package pays nothing unless BOTH rungs landed** — the scout was
   explicit that neither rung is worth a millisecond alone. If only one is
   there, land neither and say why.

The scout maps that produced this work are in the session scratchpad under
`w20_maps/` (`poly.json`, `tier.json`, `parse.json`, `alloc.json`). If that
scratch is gone, `PERF_ROADMAP.md` B132 and the wave-20 notes carry the
substance.

---

## What was done (waves 12–20)

Ten waves, each: scout → implement → adversarial review or gate → measure →
ledger → commit. Every mechanism ships behind a `ZIPP_NO_*` off-switch.

**Performance waves.** Wave 12 cut the matchAll pipeline's host taxes. Wave 13
was the largest single wave (−7.3% headline): typed-splice lanes took boxed
arithmetic register-resident (−14% on one row), chain-link slimming, and
stored-global live-range narrowing took `typedarray-math` to parity. Wave 15
taught the register tier to admit spliced leaf calls (parse −9.6%). Wave 18
cashed a blocked −11.8% on parse. Wave 19 was the second-largest (−5.2%):
`class-prototype-hot` reached **parity**, `sparse-array-v2` went 3.76× → 1.98×.

**Correctness waves — the unplanned half of this campaign.** Wave 14 found the
INT tier had been returning **silently wrong answers for a month** (a dense-array
read scratched `r10`, a register the planner parks live values in). Every
benchmark had stayed green throughout. That prompted wave 15 to build
`crates/zipp-vm/tests/jit_tier_fuzz.rs`, a generative tier-differential fuzzer.

It found **eight silent wrong-answer classes in four waves**, all live in
shipped code:

- `x | 0` inside a float loop destroying a live boolean
- a compiled loop running fewer iterations than the interpreter
- reading past an array's end throwing instead of yielding `undefined`
- a live-out boolean reading back as `NaN` (an unfilled home holds
  address-derived garbage, which is also why two programs were *nondeterministic*)
- an operand table blind to **185 of 221 opcodes** → 23 wrong-answer shapes at
  once (`typeof`, `ToNum`, `~`, `!`, `JSON.*`, `==`, `**`, spread, `delete`,
  `instanceof`, `Object.keys`, `throw`, plus two spurious `TypeError`s)

Every one had been invisible to the benchmarks, to 95,936 test262 executions,
and to a thousand hand-written unit tests.

**The recurring root cause is worth internalising:** every single one was a fact
maintained *by hand* in several places that had drifted, or a predicate valid
only for a narrower situation than its callers assumed. The fixes that stuck
stated the fact **once** — a shared predicate, a register contract, an
exhaustive match the compiler enforces.

---

## What the benchmarks cannot see

Byte-identical benchmark output means those thirteen programs agree with Node
and nothing more. The `r10` bug shipped for a month with a fully green suite,
because the thirteen rows happen to place their equivalent loops on other tiers.

Correctness at a JIT tier depends on **register allocation**, which depends on
incidental properties of the program — how many booleans are live, how many
constants got hoisted, which pins exist. That is a combinatorial space and only
generated programs cover it. The fuzzer's CI slice (500 programs × 7 modes) runs
with the normal suite in ~4s; soaks run to hundreds of thousands. **Run it on
any JIT or object-model change.**

Correctness costs measured performance and is worth it: making the operand table
exhaustive cost +0.30% of geomean and closed 23 wrong-answer shapes.

---

## What still needs doing

### 1. Finish or discard wave 21 (above). Highest priority.

### 2. The three parity rows

**`polymorphic-objects` 1.28×** — best-understood of the three. Wave 19 found
the row is *not* a polymorphic-property benchmark (78% is a `delete` loop; its
own header comment is wrong) and took it 1.40 → 1.28. Wave 20 priced the
remainder at ~1.18× via two *disjoint* mechanisms (in wave 21's tree, unverified).
Below that lies the boxed-add tier floor and a prototype validity cell
(architecture).

**`parse-large-js` 1.25×** — a tier-admission row. 77% of samples are boxed-tier
doing integer-and-pinned work the register tier refuses over one opcode class.
When the register tier *is* allowed to run this shape it beats Node (the row's
own `mix` loop: 14ms vs Node's 70ms). The two-rung package is in wave 21's tree.

**`regex-log-scan` 1.40×** — hardest, and honestly bounded. Of ~174ms needed:
~31ms is the win64 call boundary (only removable by moving the advance loop into
emitted code — 10× emitter risk for 3% of the row, repeatedly refuted); ~50ms is
string *construction*, where the binding B6 refutation applies; ~45-60ms is
diffuse per-call entry across ~40 instructions and eight layers, with no term
above ~6ms — "death by a thousand cuts is the signature of a layering problem,
and layering problems are fixed by collapsing the layers".

### 3. The diagnostics

`property-ic-shapes` 3.07× is the acceptance bench for stable shape metadata,
the largest missing architecture. Two thirds of it sits beneath the inline
caches and no contained fix reaches it — but BOXREF was priced at −200/−240ms of
it, which would be the largest single number on the board. A **shape-keyed
inline cache** is separately named: `poly-v2` shows 4.87M property misses that
are **100% shape-known** — the identity-keyed IC collapses on many instances of
one shape. That is a design defect, not a tuning knob, worth −20/−25ms there and
more on `property-ic-shapes`.

### 4. Named open items

- **A ceiling to respect:** `typedarray-math` is 99.2% register-tier and still
  1.10×. **The register tier's own generated code is ~10% behind V8.** Moving
  work off the boxed tier moves it toward 1.10×, not 1.00×.
- A pre-existing tier divergence in the negative-modulo-index family, visible
  only under `ZIPP_JIT_THRESHOLD=1` (an index from `h % 16` going negative and
  creating negative-index properties on a dense array mid-loop).
- A process-global force race in `zipp-regress`'s
  `rx_acqgate_threshold_and_streams` — flaky under multi-threaded test runs,
  documented in its own source. Wants `#[serial]` or its own binary.
- `NURSERY_MAX_MINORS = 64` forces a major every ~1M allocations regardless of
  live set, breaking an amortization invariant the code documents. Worth −0.2%
  headline — real, small, well-understood.
- An array past 2²⁰ spills to a sparse overlay that `mark_roots` re-roots **in
  full on every collection** (189.6M inner iterations in a probe). Worth zero on
  the suite (no benchmark has such an array) but it is a genuine structural bug.
- `unify_homes_with_globals` is dead code behind `const UNIFY_HOMES = false` for
  a documented silent wrong answer. Wave 18's dominance predicate does **not**
  close it (wave 20 checked).

---

## How the process works

Read `PERF_ROADMAP.md` §2 for the gate and §14 for the promotion rules. In short:

- **Scout before building.** Decompose the row with in-process timers on a
  scratch copy, vary one thing at a time, and *refute the obvious suspects with
  measurements* before proposing anything. This has been the highest-yield step
  by a wide margin — wave 13 found four "obvious" suspects measuring null or
  *negative*, and wave 19 found two rows nobody had ever decomposed holding 60%
  of the gap.
- **Verify the scout's decisive measurement before building on it.** Four scout
  premises in eight waves have dissolved under an implementer's check, including
  one where the mechanism did not exist at all (the "elision" a −15/−22ms
  mechanism was to extend turned out to be a GC-collection-frequency artifact).
- **One-binary `--ab-env` ablations**, 15 reps under 10%, 21 for marginal, and
  ~1% needs independent replication. Never `--overwrite-json`.
- **PGO retrain before any headline capture** (`bash tools/pgo.sh`). The harness
  refuses to attribute a measurement to a commit it cannot verify.
- **Record refutations.** Most entries in the ledger are negative results, and
  they have saved more time than the wins. When something measures null or
  worse, write down *why*, with the numbers.

---

## Sub-agent work in this session

The waves above were run as orchestrated workflows: parallel scouts, then
parallel implementers with disjoint file ownership, then a verification gate
that re-measures rather than trusting the lanes. What that bought, concretely:

- **Gates caught things lanes missed.** Wave 17's gate blocked a −11.8%
  mechanism from going default-on because it exposed a latent defect; it landed
  dark and was cashed a wave later once the defect was fixed. Wave 19's gate
  wrote its *own* 426-line `delete` semantics matrix and 44,000 randomized
  programs rather than trusting the lane's tests.
- **Scouts refuted more than they proposed**, which is the point. Wave 20's
  allocator scout demolished the hypothesis it was given: allocation is O(1) as a
  nursery should be, the "scaling" was 35% of its recorded size, 77% of it was one
  scheduling constant, and the row attribution that motivated the whole
  investigation was wrong in mechanism.
- **Implementers disproved their briefings.** In wave 16 all three lanes found
  their stated root cause was wrong and located the real one — the reported
  "flush across tier change" was really `x | 0` scratching a register; the
  "OSR accounting" bug was really a live-range built from first/last *mention*.
- **Honest under-delivery was reported as such.** Wave 19's regex lane landed
  −2.1% against a −6.5% target and said so; wave 21's parse lane was told to land
  nothing if only one rung worked.

Two failure modes to expect: session limits killed mid-run workflows twice
(recovery is to assess the tree, then resume with the partial lane's prompt
rewritten as gap-analysis), and one skeptic-verification pass was silently
vacuous because prompt interpolation was over-escaped — every verdict came back
meaningless. If you delegate verification, **check that the agent actually
received the finding.**
