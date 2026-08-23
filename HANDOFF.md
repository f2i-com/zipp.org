# Handoff — the Node-parity performance campaign

Written at the end of a long working session, for whoever picks this up next
(including a future me). `PERF_ROADMAP.md` is the authoritative ledger — every
claim below is an entry there with its measurements. This file is the map.

---

## Where the project is

Published capture `bench/four_engine_200cbfc_pgo_2026-08-24.json`,
`publishable: true`, 21 paired repetitions from the clean PGO build of
`200cbfc`:

| | |
|---|---|
| **Headline ten** | **0.9695× Node** [0.9655, 0.9741] — parity reached; zipp is ~3.1% faster |
| vs Bun / vs Deno | **0.81× / 0.96×** — zipp faster than both |
| Conformance | 99.994% test262; identical 6 expected failures in four modes |
| Startup | 10.6ms vs Node 34.4, Deno 52.9, Bun 63.7 |

Row ratios (cold wall, paired vs Node): map-set 0.78, typedarray 0.94,
class-prototype-hot 0.95, parse-large-js 0.96, async 0.97, json 1.01,
markdown 1.02, regex-log-scan 1.02, sparse-array 1.04,
polymorphic-objects 1.05. Diagnostics remain deliberately outside the
headline: sparse-array-v2 1.88, poly-v2 2.21, property-ic-shapes 2.58.

The headline parity objective is complete. Do not confuse the all-13 geomean
(1.1721×) with the headline: the extra three rows are architecture diagnostics
and have never been members of the timed ten.

---

## What just landed

The old warning above this section is retired: wave 21 and the follow-on parity
package are verified and committed as `200cbfc`. The work divides into four
coherent groups:

- BOXREF/INT-tier completion: boxed homes, register-tier `GetProp`, own-accessor
  inline, integer `Array#push`, bool-home reuse, and pin filtering.
- String construction: pure concat append, right-pair fusion, the two-digit
  `pad2` cache, and conditional `pad2` fusion.
- RegExp: direct regexp/string call lowering, a conservative ASCII suffix-start
  prefilter, exact-shape scalar `matchAll`, and exact-shape scalar non-global
  `exec` with deopt-safe deferred materialisation.
- JIT state traffic: one GPR deopt shadow instead of per-definition spills, and
  an exact Array `DeleteIndex` helper that deliberately declines overlays.

The last four mechanisms as one binary measured **0.9673×** headline
[0.9629, 0.9709] against their off-switches. `regex-log-scan` was **−19.4%**
and `sparse-array` **−8.4%**; the three diagnostics were flat. The final clean
PGO comparison then moved the published headline from 1.0997× to **0.9695×
Node**.

Every mechanism has a default-on `ZIPP_NO_*` comparator. The new package's
important switches are `ZIPP_NO_BOXREF_OWN_GETTER`, `ZIPP_NO_CONCAT_PURE_APPEND`,
`ZIPP_NO_CONCAT_PAIR_FUSE`, `ZIPP_NO_PAD2_CACHE`, `ZIPP_NO_PAD2_COND_FUSE`,
`ZIPP_NO_PUSH_PIN_FILTER`, `ZIPP_NO_GPR_DEOPT_SHADOW`,
`ZIPP_NO_RX_CALL_DIRECT`, `ZIPP_NO_RX_STRING_CALL_DIRECT`,
`ZIPP_NO_RX_SUFFIX_START`, `ZIPP_NO_RX_SUFFIX_REQUIRED_PREFIX`,
`ZIPP_NO_RX_SUFFIX_RUNLITERAL`, `ZIPP_NO_RX_SCALAR_MATCHALL`,
`ZIPP_NO_RX_SCALAR_EXEC`, and `ZIPP_NO_JIT_ARRAY_DELETE`. The original wave-21
package switches (`ZIPP_NO_BOX_HOME`, `ZIPP_NO_REGALLOC_GETPROP`,
`ZIPP_NO_OWN_ACCESSOR_INLINE`, `ZIPP_NO_INT_PUSH`, `ZIPP_NO_BOOL_REUSE`) remain
load-bearing too.

The gate is complete:

- full release suites for `zipp-vm`, `zipp-regress` with `rx-jit`, `zipp-cli`,
  and `zipp-wasm`;
- 20,000 fresh generated programs across 40 JIT/interpreter/GC/off-switch modes:
  zero divergence and zero nondeterminism;
- Test262 at `defaaf15` in default, `ZIPP_NOJIT=1`,
  `ZIPP_JIT_THRESHOLD=1`, and `ZIPP_NO_NURSERY=1`: **95,936 pass / the same 6
  expected failures** in every mode;
- 13/13 benchmark outputs byte-identical to Node in JIT and interpreter modes;
- two independent reviews of the scalar-exec alias, deopt, re-entry, and
  materialisation protocol.

One known pre-existing test exception remains: the single
`rx_acqgate_threshold_and_streams` assertion fails when `zipp-regress` is built
with the combined `utf16,rx-jit` feature set because its force gate is
process-global. The exact standalone `rx-jit` suite passes. This is not a new
engine failure and should be fixed by isolating or serialising that test.

Negative results were kept rather than quietly dropped: DataView bounds reuse
did not move its row; Array sparse-overlay `GetIndex` was −0.13% headline with a
CI spanning zero; weak capture caching, lazy-result/lazy-element work, and the
iterative-regex proposal did not survive verification. Their raw A/B artifacts
are retained under `bench/`.

---

## What was done (waves 12–24)

The campaign rhythm was: scout → implement → adversarial review or gate →
measure → ledger → commit. Every mechanism ships behind a `ZIPP_NO_*`
off-switch.

**Performance waves.** Wave 12 cut the matchAll pipeline's host taxes. Wave 13
was the largest single wave (−7.3% headline): typed-splice lanes took boxed
arithmetic register-resident (−14% on one row), chain-link slimming, and
stored-global live-range narrowing took `typedarray-math` to parity. Wave 15
taught the register tier to admit spliced leaf calls (parse −9.6%). Wave 18
cashed a blocked −11.8% on parse. Wave 19 was the second-largest (−5.2%):
`class-prototype-hot` reached **parity**, `sparse-array-v2` went 3.76× → 1.98×.
Waves 21–24 verified the previously unverified tier work, collapsed the hot
RegExp result paths, removed redundant deopt traffic, and crossed the
headline-ten Node line. From the last pre-package PGO capture to the new one,
parse moved
1.25× → 0.96×, regex 1.40× → 1.02×, polymorphic objects 1.28× → 1.05×,
typedarray 1.10× → 0.94×, and the headline 1.0997× → 0.9695×.

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

### 1. Preserve the parity result

The published claim is the **cold headline ten**, not the three diagnostics and
not startup-adjusted compute. Always retrain PGO from a clean source commit and
use at least 21 counterbalanced pairs before changing it. The exact reference is
`bench/four_engine_200cbfc_pgo_2026-08-24.json`; its zipp binary SHA-256 is
`e9d91210985faa49f093480631b3b1fb972578b62a2d1cba72e7191034ef5d02`.

Do not use the old `target/release/zipp.exe`: during this campaign that path held
a rejected DataView experiment. The verified PGO binary is under
`target/x86_64-pc-windows-msvc/release/`, and any future capture must rebuild it
from the then-current clean HEAD.

### 2. Attack the architecture diagnostics

`property-ic-shapes` is now 2.58×, `polymorphic-objects-v2` 2.21×, and
`sparse-array-v2` 1.88×. They remain the clearest next work because they are far
behind Node even with zipp's fast process-launch advantage.

The first two are acceptance benches for stable shape metadata and a
**shape-keyed inline cache**. `poly-v2` previously showed 4.87M property misses
that were 100% shape-known: the identity-keyed IC collapses across many
instances of one shape. Treat this as an architecture project with randomized
shape-transition/delete/prototype matrices, not a larger identity cache.

For sparse arrays, keep cold and compute time separate. The new exact
`DeleteIndex` path wins the headline row but correctly leaves `sparse-array-v2`
flat; the rejected overlay-`GetIndex` probe proves that specific helper is not
the missing 1.88×.

### 3. Improve sustained compute without losing cold parity

The ten-row cold objective is won, but startup-adjusted ratios still expose work
for long-lived programs: polymorphic objects ~1.14×, JSON and markdown ~1.12×,
regex ~1.07×, and sparse arrays ~1.57× in this capture. Profile those as compute
workloads before proposing another cold-path specialization. Conversely,
`typedarray-math` is now 0.94× and parse 0.96× cold; their old "10% native-code
ceiling" is no longer current evidence.

### 4. Named correctness and maintenance items

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
