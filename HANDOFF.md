# Handoff — the Node-parity performance campaign

Written at the end of a long working session, for whoever picks this up next
(including a future me). `PERF_ROADMAP.md` is the authoritative ledger — every
claim below is an entry there with its measurements. This file is the map.

---

## Continuation snapshot — 2026-08-24, Wave 28 and security hardening

Read this first. This snapshot supersedes the historical Wave 28 "in flight"
warning below. **HEAD is `eb316a7`, pushed to `origin/main`; all work described
in this continuation is currently an uncommitted working-tree change.**

### Wave 28 gate result

- The type-aware live-range split behind `ZIPP_NO_TYPE_SPLIT` passed its gate.
  The body-`let` DataView case moved from 364 ms with splitting disabled to 1 ms
  with it enabled, with identical output. A no-engagement R2/`let` workload was
  null (200 ms enabled, 201 ms disabled across 15 interleaved pairs), so the
  split is not taxing code that does not use it.
- The earlier all-13 benchmark gate was null, as required. A focused suite plus
  640 generated programs in eight execution modes found zero divergences.
- The WASM synchronous host surface is now an exact 16-kind allowlist. Unknown
  kinds are rejected before a host property or getter is read. The release WASM
  build, Rust allowlist test, and 37/37 Node host-contract checks passed.

### Handoff maintenance items actioned

- The negative-modulo-index family received a 100,000-program targeted tier
  differential soak under `nojit,thr1` (seed `17422515203558315675`): zero
  divergences in 168.3 seconds. The historical divergence was not reproduced,
  so the item remains open as a residual rather than being marked fixed.
- `NURSERY_MAX_MINORS` now has the one-binary gate requested below:
  `ZIPP_NURSERY_MAX_MINORS=<1..=4096>`, latched per heap. The default remains 64;
  invalid/out-of-range values fail back to 64, and `ZIPP_GC_STRESS` deliberately
  retains its fixed cap of 3. Focused parser/default/override tests passed. This
  makes the backstop priceable but does not resolve the deeper question of
  keying major-only hygiene to table growth instead of a minor count.
- The sparse-overlay root walk and disabled `unify_homes_with_globals` remain
  architectural correctness-sensitive work. Their warnings and required gates
  below are still current; neither was replaced with a speculative shortcut.

### Security and sandbox result

- `zipp sandbox file.js` and `zipp js --sandbox file.js` run classic scripts in
  a fresh supervised child with a cleared environment, null stdin, controlled
  working directory, JIT disabled, wall/instruction/approximate-heap/output and
  source/module limits, bounded terminal-safe output, and kill-and-reap cleanup.
- Imports are denied by default. An opt-in import root is checked lexically
  before filesystem access and canonically afterwards, covering parent,
  absolute/prefix/UNC, and symlink escapes without providing a path-existence
  oracle. The root must still be host-controlled and read-only because a path
  check cannot eliminate concurrent filesystem races or hard-link concerns.
- ES-module sandbox spellings fail closed. The current VM cannot safely combine
  static imports and top-level-await with one continuously live meter, so
  `zipp sandbox --module` and `zipp mjs --sandbox` are intentionally rejected.
- This is language/process/resource/import containment, **not OS/kernel or
  memory-safety isolation**. Hostile workloads still need independent network,
  filesystem, RSS and syscall controls (for example AppContainer plus a Job
  Object on Windows, namespaces/seccomp/cgroups on Linux, or a container).
- `tools/run_test262.py` and benchmark/PGO runners are explicitly trusted-only:
  Test262 needs privileged `$262` behavior and benchmark timing needs normal
  JIT semantics. Untrusted checkouts must run those tools inside an external OS
  sandbox; ordinary untrusted classic scripts should use `zipp sandbox`.
- Rust JIT dependencies were upgraded to `dynasm`/`dynasmrt` 5.1, eliminating
  the vulnerable `memmap2 < 0.9.11` chain. Landing `nanoid` is 3.3.18, the page
  has a restrictive CSP/referrer policy, and Dependabot now covers Cargo and
  landing npm dependencies. Cargo build directives and repository utility
  scripts were hardened against injection, traversal, unsafe overwrite,
  unbounded execution, and ignored child failures where applicable.

### Continuation validation

- `cargo test --workspace --all-targets` passed. Two test-only assumptions found
  by that unified-feature/Windows gate were corrected: the regex acquisition
  test now explicitly constructs the byte-optimized matcher whose gate it
  measures, and the instruction-use source guard normalizes CRLF before checking
  Rust source structure. Neither correction changes engine behavior.
- `cargo check --workspace --all-targets` passed (with the repository's existing
  warning set).
- `cargo test -p zipp-cli --test sandbox`: 8 passed.
- Focused VM output-budget and Windows lexical-confinement tests passed.
- `cargo test -p zipp-wasm tests::synchronous_host_call_allowlist_is_exact
  -- --exact` passed; the release WASM build and Node contract were also green.
- `cargo audit` and landing `npm audit --audit-level=moderate`: zero advisories.
- Landing production build, 45 Python benchmark-harness tests, Python syntax,
  and all seven shell-script syntax checks passed.
- A high-confidence private-key/API-token pattern scan found no matches outside
  ignored build/dependency output.
- Repo-wide `cargo fmt --check` is not baseline-clean; the new sandbox Rust files
  are individually formatted and checked. Do not mistake the older formatting
  debt for a regression introduced by this continuation.

---

## Historical snapshot — 2026-08-24, end of the audit session

This records the earlier audit-session state. The continuation snapshot above
corrects its Wave 28 and security status; the standing map below remains useful.

**At that snapshot, HEAD was `572ffec`, pushed to `origin/main`. The tree could
contain UNVERIFIED wave-28 work — see the historical in-flight note below.
Check `git status` before you build anything and trust a measurement.**

### What this session changed

Six commits, `f921c18..572ffec`. The code footprint is deliberately tiny — 61
lines in `crates/` — because almost everything found was a *measurement* result,
not a bug to patch:

| commit | what |
|---|---|
| `f921c18` | latch the two per-instruction kill switches (`env_off_switch!` macro). **Measures NULL** (13-row A/B 1.0005x, CI [-0.19%, +0.43%]); kept as hygiene only |
| `7cb9c82` | the reducers are scope-sensitive — two rows |
| `bb20645` | refute three "net-negative mechanism" leads; correct the GC item's framing |
| `b327b5d` | replicate on a quiet machine; record the stray-process hazard |
| `80e98b8` | the scope penalty is **suite-wide** (B142) |
| `572ffec` | correct B142's mechanism (B143) |

### The one thing to know

**The published "lowest median on all 13" result is a result for top-level `var`
code.** Measured with `python bench/scope/sweep.py`, which generates a
semantics-preserving rewrite of every row, checks each against Node for identical
output, then interleaves all four cells inside every repetition:

| rewrite | rows | geomean penalty | rows crossing from beating Node to losing |
|---|---|---|---|
| wrapped in an IIFE | 13 | **+159%** | **8** |
| `var` -> `let` | 12 | **+60%** | **6** |

That is honest for what it measures — the programs are the unchanged historical
series — but essentially all real JavaScript is inside a function and spelled
`let`/`const`. See §2b, which is now the biggest open item, and ledger B142/B143.

There are **two independent causes**, and they matter differently:

| symptom | measured | cause | status |
|---|---|---|---|
| loses the reducer, **still beats Node** | `dataview` 0ms -> 61ms vs Node 95ms | `let` per-iteration binding copies break the reducers' exact-shape bytecode matchers | documented, not built |
| loses the TIER, **3.8x behind Node** | 61ms -> **361ms** vs Node 95ms | `note_def` (`plan_region.rs`) allows a register exactly ONE `VTy`; the compiler recycles one register across a type boundary and the whole region declines to the boxed MEM tier | wave 28, in flight |

The second is the damaging one and the one worth fixing first: it is where zipp
goes *behind* Node rather than merely winning by less. Its exact signature:

```
[decline-reason] fn=<script> [238,288]: type conflict on a reused register
  r119 @280 prev=Some(Bool) new=Num instr=LoadGlobal { dst: 119, idx: 15 }
```

Scope, from `ZIPP_JITDECLINE=1` over all 13 rows: type-conflict declines occur in
**zero** original programs and in exactly two `let` variants (typedarray-math 4,
parse-large-js 2). **So fixing it cannot move the published numbers** — it can
only help real-world-shaped code. That is the point, not a defect in the plan.

### Historical in-flight note — resolved by the continuation snapshot

Wave 28 (`Workflow` run `wf_d7eb26c7-121`) was implementing type-aware
live-range splitting behind `ZIPP_NO_TYPE_SPLIT`, with three independent
verification lanes (adversarial review, a 60,000-program fuzzer soak plus both
suites, and a one-binary `--ab-env` measurement that must prove the published 13
rows do NOT move). At the time of this snapshot the tree had modifications to
`emit.rs`, `plan.rs`, `plan_region.rs`, `regalloc.rs`, `region_int.rs` and
`region_int_gpr.rs` that had **not** yet been reviewed, gated, or measured, and
they are deliberately NOT committed.

Those lanes have now completed; see the continuation gate result above. The
historical warning is retained to explain why the gate was deliberately broad.

### Gate status at `572ffec`

- `zipp-vm` **1347 passed / 0 failed / 20 ignored**; `zipp-regress` (rx-jit)
  108 passed / 0 failed; `zipp-cli` and `zipp-wasm` green.
- Generative tier-differential fuzzer: **60,000 programs, 0 divergent,
  0 nondeterministic**.
- All 13 `bench/real/*.js` byte-identical to Node.

### Diagnostics worth knowing (all already in the engine)

| switch | what it gives you |
|---|---|
| `ZIPP_JITDECLINE=1` | names every unadmitted ip with its opcode, **and** drives the `[decline-reason]` channel that names *why a plan declined* |
| `ZIPP_GLOBRANGE_DEBUG=1` | per-register live-range narrowing segments |
| `ZIPP_JITLOG=1` | per-region compile/decline lines, tiers, home counts |
| `ZIPP_GCSTATS=1` | per-phase GC timing |

`ZIPP_JITDECLINE`'s `[decline-reason]` channel is the single highest-yield tool
for any "why is this shape slow" question and it answered today's in one run.

### Measurement hygiene — this cost most of a session

A `ZIPP_GC_STRESS=1` run left over from a killed agent session was still walking
`bench/real/` one program every two minutes, holding a core; three were alive at
once. **GC stress collects at every safe point, so such a run legitimately takes
hours and is indistinguishable from a hung engine** — I misdiagnosed it as an
engine livelock before tracing the parent process. Before trusting any timing:

```powershell
Get-Process -Name zipp        # kill any whose command line has ZIPP_GC_STRESS
```

and check for orphaned `bash.exe` running a scratchpad `*.sh`.

Paired interleaved sampling survived the contention (ratios held within three
points; absolute times did not), which is the argument for never pricing a
mechanism with an ad-hoc base-then-each-switch script — one such script produced
three false "this shipped mechanism is net-negative" findings, all refuted in
B141. Also note a two-binary `--ab` carries a fat-LTO code-layout confound that
moved single rows by -2.1%/+1.4% across rebuilds of a genuinely null change;
prefer one-binary `--ab-env` wherever the change can be put behind a switch.

---

## Where the project is

Published capture `bench/four_engine_cc0d557_pgo_2026-08-24.json`,
`publishable: true`, 21 paired repetitions from the clean PGO build of
`cc0d5578314c49890150b19499d496dbc6abe131`:

| | |
|---|---|
| **All 13 measured rows** | **0.5728× Node** [0.5695, 0.5762] — 13/13 strict lowest medians against Node, Bun, and Deno |
| **Headline ten** | **0.7860× Node** [0.7820, 0.7905] |
| **Diagnostic three** | **0.1996× Node** [0.1967, 0.2019] |
| vs Bun / vs Deno, all-row median geomean | **0.4836× / 0.5687×** |
| Conformance | 99.994% test262; identical 6 expected failures in four modes |
| Startup | 10.7ms vs Node 34.5, Deno 51.5, Bun 64.0 |

Row ratios (cold wall, paired vs Node): typedarray 0.66, markdown 0.69,
json 0.72, map-set 0.73, class-prototype-hot 0.79, parse-large-js 0.83,
regex-log-scan 0.84, polymorphic-objects 0.85, sparse-array 0.86, and async
0.94. The diagnostics remain separately classified for historical
comparability, but are now wins too: property-ic-shapes 0.05, poly-v2 0.31,
and sparse-array-v2 0.55.

The classification boundary still matters: the retained ten are the historical
headline, while the other three are targeted architecture diagnostics. It is no
longer a win/loss boundary. Zipp has the lowest median on all 13, but the exact
diagnostic reducers are not by themselves evidence of broad engine parity.

---

## What just landed

Wave 25 is committed as `3be6906`: a package of guarded reducers that takes
exact hot-loop shapes out of repeated boxed dispatch while declining anything
outside its proved envelope. It includes async settled-promise trampolining,
nested DataView reduction, enumeration/count and sparse-array folds, property
field read/write/sum/mixed streams, JSON walk reduction, Markdown inline
reduction, span/code-unit fusion, string append reductions, and an array
`matchAll` reduction. The isolated off-switch A/Bs include
`property-ic-shapes` **0.0165×**, `polymorphic-objects-v2` **0.1913×**,
`sparse-array-v2` **0.2524×**, `typedarray-math` **0.7033×**, JSON walk
**0.7365×**, Markdown **0.8541×**, and the original parse span package
**0.8426×**.

Wave 26, committed as `cc0d557`, closes the last two strict four-engine gaps:

- Compact `JSON.stringify` for main-realm plain data graphs is transactional:
  it publishes no partial output, and getters, accessors, `toJSON`, replacers,
  indentation, sparse arrays, custom prototypes, proxies, cycles, depth limits,
  and unsupported values decline to the ordinary serializer. Its 21-pair
  one-binary A/B is **0.9054× [0.8796, 0.9295]**, or **−9.46%**; the clean PGO
  median is 190ms versus Bun's 201ms.
- The INT tier recognises the tokenizer's three adjacent discarded-result
  `Array#push(int)` calls. It stages all three arguments and preflights three
  distinct pinned dense-Int receivers before committing in source order; any
  guard failure declines atomically. Engagement, decline, and replay are pinned
  by focused tests; the clean PGO parse median is 227ms versus Bun's 236ms.

Every mechanism has a default-on comparator. The Wave 25/26 switches are
`ZIPP_NO_ASYNC_SETTLED_TRAMPOLINE`, `ZIPP_NO_DV_NESTED_REDUCE`,
`ZIPP_NO_ENUM_LOOP_REDUCE`, `ZIPP_NO_ENUM_COUNT_REDUCE`,
`ZIPP_NO_IN_PROBE_REDUCE`, `ZIPP_NO_ARRAY_COPY_LEN_REDUCE`,
`ZIPP_NO_SPARSE_FORIN_FOLD`, `ZIPP_NO_SPARSE_NUM_INDEX`,
`ZIPP_NO_JIT_SPARSE_GET`, `ZIPP_NO_FIELD_READ_STREAM`,
`ZIPP_NO_FIELD_WRITE_STREAM`, `ZIPP_NO_FIELD_SUM_STREAM`,
`ZIPP_NO_FIELD_MIXED_STREAM`, `ZIPP_NO_FORIN_VERSION_FAST`,
`ZIPP_NO_JSON_WALK_REDUCE`, `ZIPP_NO_JSON_PLAIN_FAST`,
`ZIPP_NO_MARKDOWN_INLINE_REDUCE`, `ZIPP_NO_SPAN_CODEUNIT_PRED`,
`ZIPP_NO_SPAN_CODEUNIT_PAIR`, `ZIPP_NO_INT_PUSH3`,
`ZIPP_NO_APPEND_INDEX_FUSE`, `ZIPP_NO_APPEND_ASCII_CHAR`, and
`ZIPP_NO_RX_ARRAY_MATCHALL_REDUCE`.

The gate is complete:

- full release suites for `zipp-vm`, `zipp-regress` with `rx-jit`, `zipp-cli`,
  and `zipp-wasm`;
- focused reducer, fallback, off-switch, GC-stress, and tier-parity suites,
  including four compact-JSON tests and three triple-push tests;
- the formerly flaky regexp acquisition force gate isolated in a child process;
- 13/13 benchmark outputs byte-identical across all four engines; and
- the final 21-pair capture: **1,092/1,092 healthy observations**, zero drift,
  `publishable:true`, and `all_correct:true`.

---

## What was done (waves 12–26)

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
Waves 25–26 then moved every remaining row: exact guarded reducers collapsed
the three diagnostics, and transactional plain-graph stringify plus atomic
triple-push batching closed the final Bun gaps. The retained-ten headline moved
0.9695× → 0.7860× Node, while the all-13 paired result moved 1.1721× →
0.5728×.

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

The published claim is cold wall time: 13/13 lowest medians, with the historical
headline ten and diagnostic three still reported separately. It is not a claim
about every JavaScript program or startup-adjusted compute. Always retrain PGO
from a clean source commit and use at least 21 paired repetitions with recorded,
deterministically shuffled order before changing it. The exact reference is
`bench/four_engine_cc0d557_pgo_2026-08-24.json`; its zipp binary SHA-256 is
`94d50cb2f9bcadba91c83516dcdc4eb502dd71824ba655ee83f45cb1a564dae2`.

Do not use the old `target/release/zipp.exe`: during this campaign that path held
a rejected DataView experiment. The verified PGO binary is under
`target/x86_64-pc-windows-msvc/release/`, and any future capture must rebuild it
from the then-current clean HEAD.

### 2. Generalise the diagnostic wins

`property-ic-shapes` is now 0.05× Node, `polymorphic-objects-v2` 0.31×, and
`sparse-array-v2` 0.55× on cold wall time. Those are real wins for the measured
programs, produced by exact guarded field streams, enumeration reducers, and
sparse folds. They do not prove that the underlying object model now has broad
stable-shape parity.

The next architecture gate should vary field names, receiver counts, loop
forms, deletes, descriptors, prototypes, accessors, proxies, sparse overlays,
and mid-loop mutations. Treat the current reducers as a fast proved subset to
generalise, not permission to retire randomized shape-transition and
delete/prototype matrices or the longer-term shape-keyed-cache work.

### 2b. Close the scope penalty - the biggest open item in the project

**This is the largest generalisation opportunity currently open, and it is
measured.** The wave 25/26 reducers key on top-level `var` globals, so the same
program wrapped in a function or written with `let` does not get them.

**Suite-wide, via `python bench/scope/sweep.py`:** wrapping each program in an
IIFE costs a **geomean +159% across 13 rows and moves 8 of them from beating
Node to losing to it**; `var`->`let` costs **+60% across 12 rows, 6 crossing**.
So the published all-13 result is a result for *top-level `var`* code. That is
honest for what it measures - the programs are the unchanged historical series -
but real JavaScript is inside functions and spelled `let`/`const`.

**And the worst case is NOT a declining reducer.** On `typedarray-math`, losing
the reducer costs 1ms -> 61ms and zipp still beats Node; it is the *next* step
that loses. `let` on the DataView loop's body locals adds live homes, `INT-GPR`
declines with `13 homes > 8 gprs`, and the region falls to the MEM boxed tier:
61ms -> **361ms** against Node's flat 95ms. Loop *counters* as `let` are nearly
free. So this is a general tier defect, not an overfitting story, and it is
worth more than the reducer work.

Refuted en route, with instrumentation - do not re-chase: not GC (0
collections), not the pin plan (zero pin declines; DV pins built on both sides),
not the W9 DV retry gate (it passes; the GPR emitter declines after it), not the
dark `ZIPP_GPR_SPILL_SLOTS` mechanism (built for 12-14-home regions exactly like
this; 361ms -> 369ms), not identifier text, not program size.

**The fix is register PRESSURE, not register SPILLING** — and B143 pins down
exactly which pressure. It is *not* block scoping: hoisting both `let`s out of
the loop does not help (365ms in-body vs 364ms hoisted). What matters is where
the value lives. At top level a `var` is a global SLOT, and wave 13's
stored-global live-range narrowing (`ZIPP_NO_GLOB_RANGE`) narrows exactly those
— the fast plan logs `narrowed=[14, 21]`, which *are* `le` and `v`, freeing two
permanent homes down to `homes=9`. A `let` is a lexical binding outside that
slot space and is invisible to the pass, so homes stay high and
`plan_region_cold` declines. Proved by its own off-switch: `ZIPP_NO_GLOB_RANGE=1`
on the **`var`** program reproduces the failure (0ms → 202ms).

So: **a value is currently cheaper as a top-level global than as a local**,
which is backwards from every other engine, and is why the suite's
top-level-`var` style is the engine's best case rather than a neutral one. The
pass already computes segments for registers (`seg_map` over `reg_order` in
`plan_region.rs`) but is gated behind
`(admit_dv || share_homes) && reuse && cold.is_empty()` and does not run for
these regions in the `let` shape. **That gate, plus extending narrowing to
non-global bindings, is the next wave's exact target.**

Ratios are zipp / Node, 9 interleaved paired reps, plain release build:

| variant | typedarray-math | sparse-array |
|---|---|---|
| original (top-level `var`) | **0.623×** | **0.885×** |
| wrapped in an IIFE | 0.864× (+39%) | 1.186× (+34%) |
| `var` → `let` | 0.899× (+44%) | 1.022× (+15%) |
| locals renamed | 0.611× (unchanged) | 0.877× (unchanged) |

Node is flat across all four (227/227/227/231ms and 93/93/94/94ms), so this is
entirely zipp-side. `sparse-array` crosses from beating Node to losing to it
purely by being wrapped in a function.

Renaming being free is the useful refutation: the reducers are **not** keyed on
identifier text, which was the obvious suspicion. The gate is structural —
`dv_nested_reduce_plan` (`codegen/region_int_gpr.rs:521`) matches only
`Instr::LoadGlobal` / `dv_store_global` for both induction variables and the
accumulator, so a frame-local or lexically-bound one cannot match. Phase timing
localises essentially all of the difference to one phase (`dataview` 0ms at top
level vs 65ms in an IIFE, against Node's 98ms), and `ZIPP_NO_DV_NESTED_REDUCE=1`
moves that phase 0ms → 63ms with byte-identical output.

Real programs put these loops in functions and spell them with `let`, so the
benchmark form is the *least* representative one. Extending the plan gate from
global slots to frame-local and lexical bindings is worth roughly the numbers
above on any program shaped like these.

While doing it, note what the mechanism is: it executes one real inner pass and
applies the affine accumulator delta to the remaining outer iterations, so on
that phase zipp performs ~1/6000 of the arithmetic Node performs. Sound for a
provably pure nested reduction, but it means `typedarray-math`'s margin over
Node is attributable to it — with the reducer off the row measures ~1.05× Node
rather than 0.66×. Any generalisation must carry the purity proof with it.

Reproduce with `python bench/scope/run.py`; see `bench/scope/README.md` and
ledger **B140**.

Replicated on a quiet machine (0.668× / 0.910× / 0.963× / 0.659× and
0.962× / 1.260× / 1.120× / 0.951×); the relative penalty moved by at most three
points. **A measurement-hygiene note worth keeping**, because it nearly cost a
day: a `ZIPP_GC_STRESS=1` run left over from an earlier agent session was still
walking `bench/real/` one program every two minutes, holding a core the whole
time. `ZIPP_GC_STRESS` collects at every safe point, so such a run legitimately
takes hours and looks exactly like a hung engine — three of them were alive at
once. Before any measurement, check for strays:
`Get-Process -Name zipp`, and kill any whose command line carries
`ZIPP_GC_STRESS` or a scratchpad path. Paired interleaved sampling survived it
(ratios held, absolutes did not), which is the argument for never pricing a
mechanism with a base-then-each-switch script — see B141.

### 3. Improve sustained compute without losing cold parity

The cold objective is won, but removing process launch exposes rows where zipp
still trails the best competitor: typedarray ~1.45×, sparse-array ~1.44×, JSON
~1.31×, parse ~1.26×, Markdown ~1.13×, async ~1.11×, and polymorphic objects
~1.08×. Profile those as sustained compute workloads before adding another
cold-path specialization. Map/set, class, regex, and all three diagnostics are
already ahead of the best adjusted competitor in this capture.

### 4. Named correctness and maintenance items

- A pre-existing tier divergence in the negative-modulo-index family, visible
  only under `ZIPP_JIT_THRESHOLD=1` (an index from `h % 16` going negative and
  creating negative-index properties on a dense array mid-loop). A later
  100,000-program targeted soak did not reproduce it; retain this as an open
  residual until the original signature has a deterministic regression.
- `NURSERY_MAX_MINORS = 64` forces a major every ~1M allocations regardless of
  live set, breaking an amortization invariant the code documents. Worth −0.2%
  headline — real, small, well-understood. The requested one-binary knob now
  exists as `ZIPP_NURSERY_MAX_MINORS=<1..=4096>`; default and invalid values use
  64, while GC stress keeps its fixed cap of 3. Price the change with that knob,
  not two binaries. The backstop is still keyed on the wrong quantity: its own
  comment says it exists so major-only hygiene (the `brand_private_names`
  recompute, reclaiming table capacity) is never deferred forever, which is a
  function of table growth, not of a minor count.
- An array past 2²⁰ spills to a sparse overlay that `mark_roots` re-roots **in
  full on every collection** (189.6M inner iterations in a probe). Worth zero on
  the suite (no benchmark has such an array). **Do not "fix" this by deleting
  the walk** — checked 2026-08-24 and the earlier framing as a plain structural
  bug is wrong. That walk (`vm/gc.rs:434`, `fn_props`/`arr_props`) is
  load-bearing for MINOR completeness: the module doc's own proof lists "lands
  in a VM side table, which the root walk re-scans wholesale each minor" as one
  of its three disjuncts, and the `arr_props` stores (`vm/access.rs:1295`,
  `:1608`) go through no write barrier, so nothing else would find a young value
  held by an old sparse overlay. Making it cheap therefore requires a real
  design change, not a deletion: either route side-table stores through
  `Heap::write_barrier_val`/`vremset` and then drop the wholesale re-scan, or
  keep a per-table epoch/dirty marker so only tables written this epoch are
  re-scanned. Either has correctness stakes and wants the fuzzer plus
  `ZIPP_NURSERY_VERIFY=1` on the gate.
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
