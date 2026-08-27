//! Generative TIER-DIFFERENTIAL fuzzing: the compiled tiers must answer exactly
//! what the interpreter answers, for programs nobody wrote by hand.
//!
//! `jit_tier_parity.rs` is the hand-picked half of this problem, and its own doc
//! note records the limit: every case there was invisible to 95,936 test262
//! executions, and hand-picking found them only after three bugs of the same
//! shape had already shipped. W14 then found a dense-array element read on the
//! INT tier scratching `r10` — `BOOL_GPRS[2]` — which destroys any live Bool
//! home or hoisted-compare mirror the planner parked there. It answered wrong
//! for a month across six waves. Nothing saw it, because the 13 benchmark rows
//! and every hand-written test happen to place their equivalent loops on other
//! tiers.
//!
//! That is the lesson this file is built on: correctness at a TIER is a function
//! of REGISTER ALLOCATION, and register allocation is a function of incidental
//! properties of the program — how many Bools are live, how many distinct
//! hoisted constants are compared against, which receivers got pinned, how deep
//! the loops nest. `BOOL_GPRS[2]` is only occupied when the region carries three
//! or more of (bool home, hoisted compare constant); a shape with two is clean
//! and a shape with three is wrong. No amount of care picks that third one by
//! hand. It has to be enumerated.
//!
//! ## What is compared
//!
//! The primary oracle is not node — it is ZIPP AGAINST ITSELF. Every
//! `ZIPP_NO_*` switch in the engine is specified as a PURE FALLBACK, and
//! `ZIPP_JIT_THRESHOLD` only moves WHEN a region compiles. So for a fixed
//! program, every mode in [`MODES`] must produce a byte-identical digest, and
//! `ZIPP_NOJIT=1` is the interpreter's own answer. A disagreement between two
//! zipp modes is a tier bug by construction — it needs no external reference and
//! cannot be a "zipp differs from node here" judgement call. node is the
//! SECONDARY oracle (`node_oracle_slice`), which catches the case where every
//! tier is wrong in the same way.
//!
//! ## Why the generated programs are exactly comparable
//!
//! A fuzzer with false positives gets ignored, so the generator is closed under
//! determinism by construction, enforced by `generator_emits_only_exact_js`:
//!
//! * every reported value passes through `ToInt32` (`| 0`), which is exact for
//!   every double including NaN, ±Infinity and values past 2^53 — so no answer
//!   ever depends on Number→String formatting;
//! * `Math` is restricted to the exactly-specified members (`imul`, `clz32`,
//!   `abs`, `min`, `max`, `floor`, `ceil`, `round`, `trunc`, `sqrt`, `fround`).
//!   `sin`/`cos`/`pow`/`exp`/`log` are implementation-approximated and are
//!   banned;
//! * no `Date`, no `Math.random`, no `for…in` (integer-like key order), no
//!   `Object.keys`, no locale, no prototype mutation of any builtin — programs
//!   are prefix-isolated and touch only their own bindings;
//! * every loop is a counted `for` whose induction variable the body never
//!   assigns, so every program terminates;
//! * `a < b` and its negated spelling `!(a >= b)` are only ever emitted over
//!   operands that cannot be NaN, so the two spellings are genuinely equivalent
//!   and a divergence between them is a real one.
//!
//! ## Calibration: what the INT tier will actually accept
//!
//! A generator that never reaches a tier cannot test it, and there is no way to
//! know which it reaches by reading the generator. So this one was calibrated
//! against a WORKTREE AT THE PRE-W14 COMMIT, where the r10 clobber is live and a
//! reachable shape therefore ANSWERS WRONG. The first version reached the INT
//! tier in 0 of 42 register-pressure programs and found nothing in 800; the
//! facts below are what closed that gap, each one measured by adding it to a
//! shape that miscompiled and watching the miscompile disappear:
//!
//! * a compare constant must be an in-loop LITERAL. A function-scope `var c =
//!   13` is a read-only live-in, and `plan_region` declines a region that uses
//!   one "where a number isn't required" — which `===` is — so the whole shape
//!   lands on MEM. A literal becomes an in-region `LoadConst` the planner
//!   hoists, and that is also the only thing that ever claims a `gpr_const`
//!   mirror in `BOOL_GPRS`;
//! * one `continue` anywhere in the loop declines the region outright;
//! * so does one `Math.max`, one uncoerced `h = h + x`, or one constant whose
//!   source spelling is not an int32 (`-2147483648` is unary minus over the
//!   double `2147483648`);
//! * an equality test against the loop ACCUMULATOR (`h === 0`) declines it; an
//!   ordering test (`h < 4`) does not;
//! * the negated spelling of an equality (`!(a !== k)`) puts the constant in a
//!   register and declines; the negated spelling of an ordering does not.
//!
//! Hence [`Flavor::Scan`] and [`Flavor::Pressure`], which hold to all of that on
//! purpose; [`Flavor::Sroa`], whose object must be a GLOBAL because
//! `plan_field_promotion` takes the globals table; and the other flavors, which
//! break the rules above on purpose and reach MEM / Tier C / DOUBLE instead.
//! After calibration the pre-W14 tree diverges on 6 of the CI slice's 300
//! programs, which [`tier_differential_ci_slice`] reports there in ~1.3s.
//!
//! ## What it found
//!
//! Five specs below carry minimized cases for four findings on the tree this
//! was written against — [`open_bool_local_reads_back_as_nan`] and
//! [`open_bool_live_out_reads_back_false`] (one defect, two faces),
//! [`open_nested_loop_drops_inner_iterations`],
//! [`open_cold_out_of_range_read_throws`] (FIXED in W16, now un-ignored —
//! the receiver-slot suite is `tests/cold_pinned_recv.rs`), and
//! [`open_fused_double_compare_takes_wrong_branch`]. All reproduced at HEAD
//! 6ed29ac and all are node-confirmed, so none of them is a zipp-vs-node
//! judgement call.
//!
//! All five are CLOSED (W16) and now run with the normal suite: the two
//! `open_bool_*` faces of one register defect, and
//! [`open_nested_loop_drops_inner_iterations`] +
//! [`open_fused_double_compare_takes_wrong_branch`], which also turned out to be
//! ONE defect wearing two tiers — a home-reuse live range that stopped at the
//! last MENTION of a value instead of its last live ip, so a value defined
//! outside an inner loop and read inside it lost its home to a later temp. And
//! [`open_cold_out_of_range_read_throws`], whose pinned receiver now stores the
//! object to its frame slot. No `open_*` spec is `#[ignore]`d any more.
//!
//! ## W17 found a sixth — CLOSED in W18
//!
//! [`open_conditional_def_loses_its_entry_load`] and its cold-block twin. A
//! local whose only in-region definition sits on a CONDITIONAL branch was
//! treated as though that def dominated every use, lost its entry load, and the
//! compiled body read its home as garbage on every pass that skipped the branch.
//! Two lines, wrong on both register tiers with two different answers, avoided
//! by no `ZIPP_NO_*` switch, and reproducing at the committed HEAD 0ade520.
//! `plan_region.rs` already named the distinction it turns on — `first_seen ==
//! true` says the first OCCURRENCE is a def, not that a def RUNS — and guarded
//! the constant-hoisting consumer with `runs_every_iteration`; the `shareable` /
//! live-in consumer beside it was unguarded.
//!
//! W18 closed it by deleting the flag from those two consumers rather than
//! adding a second guard: `region_liveness` (the same backward walk W16 added
//! for live SPANS) now also returns the region's true live-in set, and one
//! predicate — `live_in(r)` in `plan_region.rs` — answers "is the value this
//! register holds at entry still observable?" for `shareable`, `range` and
//! therefore `live_in_regs`. Both specs run with the normal suite again, and
//! `KNOWN_OPEN` is empty.
//!
//! What kept it hidden is worth as much as the bug: reading the local AFTER the
//! loop makes the program answer correctly, because `read_outside` forced a
//! permanent home with an entry load. Every hand-written test, every benchmark
//! row and this file's own return mix read their accumulators afterwards — which
//! is why 138,300 W15-generator programs never saw it.
//!
//! Two generator changes came out of that, and together they are the clearest
//! evidence in this file that a widening WORKED. `Program::dead_out` leaves one
//! local out of the return mix so it is genuinely dead after the region, and
//! [`Stmt::CondDef`] emits `if (cond) { t = <int>; }` — a definition that does
//! not DOMINATE its uses, half of them behind a condition that never fires.
//! Measured yield for this class:
//!
//! ```text
//! W15 generator                       138,300 programs …   0 divergences
//! W17 generator, before `CondDef`     188,000 programs …   1 divergence
//! W17 generator, with `CondDef`        48,000 programs … 252 divergences
//! ```
//!
//! The one before `CondDef` was luck: a `DeoptKind::TypedOob` guard that
//! happened to write a temp a later index read. All 252 after it are the SAME
//! class — every one contains a `CondDef` and not one is anything else — and so
//! are all 29 of the node-oracle disagreements. The CI slice saw it at exactly
//! one index of 640, which is why `KNOWN_OPEN` held one entry and not a page;
//! W18's fix emptied the list.
//!
//! A fifth result had no spec because it is about a SWITCH rather than about the
//! default: on 12 of the 28 divergent programs the soak found, the default
//! answer is right and `ZIPP_NO_FUSED_CMPJUMP=1` alone is wrong. That switch is
//! specified as a pure fallback, so any A/B measured through it was being
//! measured against wrong answers. W16 found it to be the SAME defect seen from
//! the other side — unfusing a compare adds a `Bool`, which pushes a live bool
//! into the register the DOUBLE tier's body was scratching — and closed it with
//! the two above; `tests/bool_home_clobber.rs` holds its spec, including the
//! switch-purity case.
//!
//! The honesty check that backs that up: over 4,000 generated programs run
//! against node (`ZIPP_FUZZ_NODE_COUNT=4000`), `ZIPP_NOJIT=1` agreed with node
//! on every single one, and every one of the five node disagreements was the
//! compiled tier being wrong. The generator produced no implementation-defined
//! answers at all.
//!
//! W17 re-ran that check over 8,000 programs of the WIDENED generator, with the
//! `typeof` folds, the f64 NaN/-0 probes, the post-region uses, the script-scope
//! spelling and its IIFE wrapping all in play. 29 programs disagreed with node,
//! and every single one of the 29 carries a [`Stmt::CondDef`] — i.e. every one
//! is [`open_conditional_def_loses_its_entry_load`] and not one is a generator
//! false positive. That is the property that has to hold for any of this to be
//! worth running, so it is measured again whenever the generator grows:
//! `ZIPP_FUZZ_NODE_COUNT=8000 cargo test --release --test jit_tier_fuzz -- //! node_oracle_slice --exact`. W18 re-ran it at 8,000 after closing the class:
//! ZERO node disagreements. The same binary with ONLY the `live_in` hunk
//! reverted — same generator, same seed, same count — disagrees with node on 29
//! programs, which is the W17 number reproduced first-hand. Any failure it
//! prints now is new.
//!
//! ## W18: the assumption underneath every comparison
//!
//! This instrument compares digests ACROSS modes. Every verdict it has ever
//! reached rests on one unstated assumption: that a mode answers the same thing
//! twice. W17's gate soak found two generated programs that do not.
//! `s3127_i361` and `s3129_i318` alternated `D 7840` / `D b08f` roughly 50/50
//! over runs of ONE binary, on the committed baseline as much as on the wave
//! tree.
//!
//! That is worse than two wrong programs. A fuzzer that cannot tell "these
//! tiers disagree" from "this program disagrees with itself" is unsound in both
//! directions at once — a real divergence gets waved off as flakiness, and a
//! flake gets written up as a tier bug. W17's gate hand-triaged 149 divergences
//! to find these two, which is the cost of not knowing.
//!
//! ROOT CAUSE, and it is the reason this section sits under the one above: they
//! are not a third defect. Both are
//! [`open_conditional_def_loses_its_entry_load`] wearing its worst face. `t0`'s
//! only in-region def sits behind `if (i === …)`, so it looked def-first, became
//! `shareable`, and dropped out of `live_in_regs`; its home was never filled at
//! OSR entry and the body read whatever the previous phase had left in that
//! register. An unfilled home does not hold a WRONG CONSTANT — it holds
//! whatever is there, and what is there is address-derived, so it differs run to
//! run. Every one of this file's other findings was a stable wrong answer
//! because the garbage it read happened to be stable. This class is what the
//! same defect looks like when it is not.
//!
//! What the instrument gained, so the next occurrence costs a minute instead of
//! a gate:
//!
//! * Every candidate divergence is CLASSIFIED before it is shrunk or reported.
//!   Each of the two disagreeing modes is re-run on its own, in fresh processes
//!   ([`SELF_RUNS`]); a program that disagrees with itself is reported as
//!   [`Flake::Nondeterministic`], not as a tier divergence.
//! * A flaky program is shrunk on the property it actually has — "this mode does
//!   not answer the same thing twice" — instead of on a cross-mode comparison
//!   that means nothing for it. On `s3127_i361` that reduces the generated
//!   program to the two-line kernel in
//!   [`conditional_def_answers_the_same_thing_every_run`], in 1.6s.
//! * The per-mode table is taken with TWO runs of each mode ([`mode_table`]), so
//!   a mode that flakes is caught even when it is not one of the two the shrink
//!   drove. On `s3127_i361` pre-fix, 17 of the 37 rows visibly disagree with
//!   themselves — invisible at one sample per row.
//! * A batch disagreement that no standalone re-run reproduces is labelled
//!   [`Flake::BatchOrderOnly`] — a HARNESS finding (state carried between
//!   programs in one process), not an engine one. That case was already
//!   detected; it was not labelled, and it reused samples the classifier now
//!   takes anyway, so it costs nothing.
//!
//! Cost: zero on a green run. Nothing is re-run until something has already
//! diverged, and the CI slice is unchanged at ~1s for all 17 tests.
//!
//! Measured yield, honestly: re-running all 149 of W17's triaged divergences
//! against the pre-fix binary, exactly 2 disagree with themselves — the two a
//! human found by hand. So the hardening auto-classifies 2 of 149. That is a
//! small number and the right one to report: the win is not volume, it is that
//! those two stop poisoning the other 147, and that the next one is labelled by
//! the machine. Against the FIXED tree all 149 are stable.
//!
//! One calibration fact worth keeping, because it sets [`SELF_RUNS`]: at 8 runs
//! per program that scan caught 1 of the 2, at 16 it caught both. A coin-flip
//! flake is not cheap to see. R = 6 against each of two modes is 2^-10.
//!
//! ## W17: the four places it was blind, measured
//!
//! The W15 author left a coverage report and an honest list of what it did not
//! reach. Each entry below is what was actually wrong, and each fact was
//! measured on this tree with `ZIPP_JITLOG=1 ZIPP_JITDECLINE=1` rather than
//! reasoned about.
//!
//! **Post-region uses did not exist.** Every use the generator emitted was
//! INSIDE the loop or in the return mix — and the return mix reads a local
//! through `| 0` or a truthiness test, both of which ERASE representation. A
//! `Bool` home that reads back as a raw `NaN` is FALSY, so `(b ? 17 : 0)` gives
//! exactly the answer an honest `false` gives and the digest never moves; that
//! is why a live `typeof x` after a loop was found by a human reading code and
//! not by 138,300 generated programs. Two things changed: the mix now folds
//! `typeof` in for every live local (and `d === d` / `1 / d < 0` for every
//! double), and [`Post`] generates what happens to a live-out AFTER the loop —
//! `typeof`, identity, a call boundary, a boxed store, and a SECOND hot loop
//! whose live-ins are the first region's live-outs.
//!
//! **The DOUBLE tier was thin because the tier is all-or-nothing.** 25 DOUBLE
//! regions per 400 programs, against 686 INT and 2,172 MEM. The cause is a short
//! list of ops any one of which declines the whole region to MEM: any `MathOp`
//! (`imul`, `floor`, `abs`, `sqrt`, `fround`, `min`, `max`, `clz32`), any
//! `Call`, `t === undefined`, `.length`, `charCodeAt`, an Int32Array or
//! Uint8Array read, an ordinary-Array WRITE. Three of those were in
//! [`Flavor::Double`]'s own statement menu, so a Double-flavor loop with four
//! double statements cleared them ~16% of the time. [`gen_double_body`] draws
//! only from what the tier admits: 25 -> 76 per 400.
//!
//! **B94 split receivers were unreachable, not merely rare.** A split needs the
//! bytecode compiler to RECYCLE a register — pinned element receiver over one
//! range, number over another — and inside a function it never does: the
//! generated kernels run to `regs=74`, one per expression node. At SCRIPT scope
//! the temp numbering restarts per statement and `LoadGlobal r7 <- arr` lands two
//! instructions after `LoadConst r7 <- 0.25`. [`Scope::Script`] is that axis and
//! [`gen_split_body`] is the shape built for it: 0 -> 9 per 400, on both register
//! tiers, sometimes two receivers in one region (which is what
//! `ZIPP_NO_MULTI_SPLIT` governs). Script scope is not free — a wide script-scope
//! body recycles a register across two TYPES and the planner declines it — so it
//! is drawn at 10% outside that flavor.
//!
//! **Tier A was not reached at all.** It was reported as "generated but
//! unverifiable", because a successful Tier A compile logged nothing. It now
//! logs `[jit] Tier A fn{id} compiled`, and the first thing that line said was
//! that no generated program had ever been on that tier: `Stmt::Rec` spelled its
//! body with `| 0`, and `fn_int::can_compile` admits no `Bitwise` op at all, so
//! every generated recursion had been landing on Tier C. The `| 0`s are gone and
//! [`tier_a_is_reached`] pins both halves.
//!
//! **`ZIPP_FUZZ_BIG` never ran.** 5,000 BIG programs found nothing while 5,000
//! ordinary ones found ~10 — with the soak driver passing `ZIPP_FUZZ_BIG=1`
//! through a bash assignment PREFIX that expands to a command name, so every
//! "BIG" soak had actually run the ordinary generator and the reported
//! comparison was a comparison of a thing with itself. The driver is fixed, and
//! BIG is re-aimed: it used to mean a bigger STATEMENT BUDGET, which dilutes the
//! tight shapes that trip the register allocator; it now means the same tight
//! bodies run LONGER — more iterations, more repetitions — which moves WHEN a
//! region compiles, deopts and is evicted.
//!
//! And now that it runs, here is what it is worth, measured rather than argued.
//! Over the same 400 programs the BIG tier mix is FLAT: 2162/551/75 MEM/INT/
//! DOUBLE regions against 2161/547/76 ordinary, the same 9 split receivers, the
//! same 8 Tier A compiles. The only things that move are +4% deopts and +11%
//! evictions — which is exactly what it was re-aimed at and nothing more — and
//! it costs 5x: 8,000 BIG programs took 145s where 20,000 ordinary ones took
//! 121s. So BIG is not a general soak mode and a soak budget is better spent on
//! 5x more distinct programs; it is the EVICTION-DENSITY knob, for when the bug
//! being hunted is in the evict/re-plan path (W16's live-out `Bool` was reached
//! that way). It is kept for that and documented at that price, not deleted:
//! it is now the only lever on how often a region is re-planned.
//!
//! Two engine changes came with this, both diagnostic: the Tier A line above,
//! and `fn=<name> [start,end]` attribution on every `[decline-reason]`, without
//! which a decline in a program with more than one hot function cannot be tied
//! to the region that produced it.
//!
//! ## Layout
//!
//! * [`tier_differential_ci_slice`] — the bounded seeded batch that runs with
//!   the normal suite. Fixed seed, fixed program count, seven modes.
//! * [`tier_differential_soak`] — `#[ignore]`d, env-driven (`ZIPP_FUZZ_SEED`,
//!   `ZIPP_FUZZ_COUNT`, `ZIPP_FUZZ_MODES`, `ZIPP_FUZZ_BIG`). This is the long
//!   run; it prints the seed it used so any finding is reproducible.
//! * [`fuzz_child`] — the worker. A no-op unless `ZIPP_FUZZ_JOB` is set, because
//!   a mode IS a process (every switch is a memoized `AtomicU8` latch, so it
//!   cannot be changed inside a running process). Same re-exec pattern as
//!   `int_gpr_homes.rs`.
//!
//! On a divergence the parent first CLASSIFIES it (re-running each of the two
//! disagreeing modes on its own — see [`Flake`]), then SHRINKS the program
//! against whichever property it actually has: the two modes disagreeing, or
//! the one mode disagreeing with itself. Statement deletion, loop-bound
//! reduction, declaration trimming. It prints the minimal source plus what
//! every mode answered on two runs.

#![allow(clippy::too_many_arguments)]

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// ─────────────────────────────── modes ───────────────────────────────

struct Mode {
    name: &'static str,
    env: &'static [(&'static str, &'static str)],
}

/// Every mode must answer identically for every program.
///
/// `nojit` is the interpreter — the reference. `thr1`/`thr200` move the OSR
/// point, which is what W14's `m` tracked exactly: a region compiled at
/// iteration 8 and one compiled at iteration 200 run a different number of
/// iterations natively, so a wrong answer that depends on the compiled body
/// changes VALUE with the threshold instead of vanishing. The `ZIPP_NO_*` rows
/// are the engine's own off-switches, each specified as a pure fallback.
/// `intsplit` is the one OPT-IN row (`ZIPP_INT_SPLIT=1` is default-off), which
/// is exactly where a path gets less exercise than it needs.
const MODES: &[Mode] = &[
    Mode {
        name: "base",
        env: &[],
    },
    Mode {
        name: "nojit",
        env: &[("ZIPP_NOJIT", "1")],
    },
    Mode {
        name: "thr1",
        env: &[("ZIPP_JIT_THRESHOLD", "1")],
    },
    Mode {
        name: "thr200",
        env: &[("ZIPP_JIT_THRESHOLD", "200")],
    },
    Mode {
        name: "nogprhomes",
        env: &[("ZIPP_NO_GPR_HOMES", "1")],
    },
    Mode {
        name: "noicgate",
        env: &[("ZIPP_NO_ICGATE", "1")],
    },
    Mode {
        name: "noshapeways",
        env: &[("ZIPP_NO_SHAPE_WAYS", "1")],
    },
    Mode {
        name: "noattrselide",
        env: &[("ZIPP_NO_ATTRS_ELIDE", "1")],
    },
    Mode {
        name: "nofusedcmp",
        env: &[("ZIPP_NO_FUSED_CMPJUMP", "1")],
    },
    Mode {
        name: "noglobrange",
        env: &[("ZIPP_NO_GLOB_RANGE", "1")],
    },
    Mode {
        name: "nomultisplit",
        env: &[("ZIPP_NO_MULTI_SPLIT", "1")],
    },
    Mode {
        name: "notypedsplice",
        env: &[("ZIPP_NO_TYPED_SPLICE", "1")],
    },
    Mode {
        name: "notypesplit",
        env: &[("ZIPP_NO_TYPE_SPLIT", "1")],
    },
    Mode {
        name: "nointsplit",
        env: &[("ZIPP_NO_INT_SPLIT", "1")],
    },
    Mode {
        name: "nointsplice",
        env: &[("ZIPP_NO_INT_SPLICE", "1")],
    },
    Mode {
        name: "intsplit",
        env: &[("ZIPP_INT_SPLIT", "1")],
    },
    Mode {
        name: "noguardhoist",
        env: &[("ZIPP_NO_GUARD_HOIST", "1")],
    },
    Mode {
        name: "nodensebackedge",
        env: &[("ZIPP_NO_DENSE_BACKEDGE", "1")],
    },
    Mode {
        name: "notierc",
        env: &[("ZIPP_NO_FNJIT_MEM", "1")],
    },
    Mode {
        name: "nocallinline",
        env: &[("ZIPP_NO_CALL_INLINE", "1")],
    },
    Mode {
        name: "gcstress",
        env: &[("ZIPP_GC_STRESS", "1")],
    },
    // ── B189 ── the Tier-C captured-read lanes. `noupvalinline` restores the
    // resolving helper for every emitted `UpvalGet` (differentials the
    // cell-value mirror + activation-cached base against it); `upvalmin12`
    // restores the B50-era admission floor (differentials compiled upval
    // bodies against blacklisted-and-interpreted ones). The generator's
    // `use_closure` programs (35%) put `up` behind exactly these paths.
    Mode {
        name: "noupvalinline",
        env: &[("ZIPP_NO_TIERC_UPVAL_INLINE", "1")],
    },
    // B189b: the emitted same-proto call lane vs the helper route.
    Mode {
        name: "nocross3",
        env: &[("ZIPP_NO_CROSS3", "1")],
    },
    // B192: INT-tier admission of dead-in-region completion writes.
    Mode {
        name: "noundefadmit",
        env: &[("ZIPP_NO_UNDEF_ADMIT", "1")],
    },
    // B187 stage 3: the `Box<ObjMap>` recycle pool vs fresh allocation +
    // courier for every finalize-born literal (the generator's literal
    // churn plus the GC-stress lane drive sweep-refill/pop constantly).
    Mode {
        name: "noobjpool",
        env: &[("ZIPP_NO_OBJ_POOL", "1")],
    },
    // B196a: the pool's serve-order and major-refill fallbacks (LIFO-only
    // serve; minor-only refill) — pure fallbacks by contract, so the
    // differential checks them for free.
    Mode {
        name: "noobjpoolsort",
        env: &[("ZIPP_NO_OBJ_POOL_SORT", "1")],
    },
    Mode {
        name: "noobjpoolmajor",
        env: &[("ZIPP_NO_OBJ_POOL_MAJOR", "1")],
    },
    // B210: the courier's per-item size gate (bulk payloads ship, small ones
    // drop inline). The off-row restores B185's ship-everything, so the full
    // shipping path — batch build, mpsc send, off-thread drop — stays
    // exercised as the pure fallback it claims to be.
    Mode {
        name: "nocouriergate",
        env: &[("ZIPP_NO_COURIER_GATE", "1")],
    },
    // B214: root-the-in-flight-microtask vs the whole-task GC suspension
    // (the generated async/promise shapes collect mid-reaction only on the
    // rooted side — a missed root answers wrong or crashes here).
    Mode {
        name: "nomicrotaskroot",
        env: &[("ZIPP_NO_MICROTASK_ROOT", "1")],
    },
    // B213: caller-side skip of handler-excluded callees vs the always-
    // attempt planners (pure planning; the generated try/finally shapes
    // exercise both routes).
    Mode {
        name: "nohandlerskip",
        env: &[("ZIPP_NO_HANDLER_CALLEE_SKIP", "1")],
    },
    // B212: the const+int concat memo vs fresh allocation — the generator's
    // string-key shapes plus the frozen-string append/chain fallbacks.
    Mode {
        name: "noconcatmemo",
        env: &[("ZIPP_NO_CONCAT_MEMO", "1")],
    },
    // B209: `SetHomeObject` elision for super-free literal methods is
    // compile-time; the off-row proves the always-wire fallback answers
    // identically (the generator's literal methods + prototype walks).
    Mode {
        name: "nohomeelide",
        env: &[("ZIPP_NO_HOME_ELIDE", "1")],
    },
    // B205: the fused Math.random()*k|0 window vs its ordinary ops, and
    // B199's compile-order retry vs permanent declines.
    Mode {
        name: "norandomfuse",
        env: &[("ZIPP_NO_RANDOM_FUSE", "1")],
    },
    Mode {
        name: "nocrossretry",
        env: &[("ZIPP_NO_CROSS_RETRY", "1")],
    },
    Mode {
        name: "noyieldentry",
        env: &[("ZIPP_NO_YIELD_ENTRY", "1")],
    },
    Mode {
        name: "upvalmin12",
        env: &[("ZIPP_TIERC_UPVAL_MIN", "12")],
    },
    // ── W17 ── the engine has ~50 `ZIPP_NO_*` switches and this list held 14 of
    // them. Every one is specified as a PURE FALLBACK, which is a claim the
    // differential can check for free: a switch that changes an answer is a bug
    // whichever side is wrong, and W16 found exactly that shape
    // (`ZIPP_NO_FUSED_CMPJUMP=1` alone answering wrong on 12 of 28 programs, so
    // every A/B measured through it had been measured against wrong answers).
    // The rows below are the switches that touch a path these programs actually
    // execute — register allocation, region admission, the element and property
    // fast paths, the GC. The regex / JSON / Promise / string-intrinsic switches
    // stay out, and so does `ZIPP_NO_ITER_REGION`: the generator emits no
    // `for…of` at all, so that row would cost process time to prove nothing.
    //
    // How far "actually execute" was checked, honestly: over a 900-program
    // sample under `ZIPP_JITLOG=1 ZIPP_JITDECLINE=1`, these rows visibly change
    // the JIT's own decisions — `nogprsplit`, `nogprnest`, `nogprlazysx`,
    // `nogprwtshare`, `nodoublebitwise`, `nodoublemod`, `notiercleaf`,
    // `nomethodinline`, `nosplicealias`, `nopolyeqfast`, `arrpinloose`,
    // `accalways`. The rest gate EMITTED CODE or a runtime fast path rather than
    // a plan, so they produce no log line either way and their reach is by code
    // inspection, not measurement. Both kinds are worth a row — a switch that
    // changes an answer without changing a plan is the harder bug.
    //
    // A mode is a PROCESS, so this list is the soak's cost driver. Measured:
    // 18 rows to 33 took a 2,000-program soak from ~6.0 to ~6.7 ms/program,
    // because the modes run as parallel threads. `CI_MODES` is unchanged.
    Mode {
        name: "nogprsplit",
        env: &[("ZIPP_NO_GPR_SPLIT", "1")],
    },
    Mode {
        name: "nogprnest",
        env: &[("ZIPP_NO_GPR_NEST", "1")],
    },
    Mode {
        name: "nogprlazysx",
        env: &[("ZIPP_NO_GPR_LAZYSX", "1")],
    },
    Mode {
        name: "nogprspill",
        env: &[("ZIPP_NO_GPR_SPILL_SLOTS", "1")],
    },
    Mode {
        name: "nogprwtshare",
        env: &[("ZIPP_NO_GPR_WT_SHARE", "1")],
    },
    Mode {
        name: "nodoublebitwise",
        env: &[("ZIPP_NO_DOUBLE_BITWISE", "1")],
    },
    Mode {
        name: "nodoublemod",
        env: &[("ZIPP_NO_DOUBLE_MOD", "1")],
    },
    Mode {
        name: "nocrosscall",
        env: &[("ZIPP_NO_CROSSCALL", "1")],
    },
    Mode {
        name: "notiercleaf",
        env: &[("ZIPP_NO_TIERC_LEAF", "1")],
    },
    Mode {
        name: "nomemcmpjump",
        env: &[("ZIPP_NO_MEM_CMPJUMP", "1")],
    },
    Mode {
        name: "nomethodinline",
        env: &[("ZIPP_NO_METHOD_INLINE", "1")],
    },
    Mode {
        name: "noleafgetprop",
        env: &[("ZIPP_NO_LEAF_GETPROP", "1")],
    },
    Mode {
        name: "nosplicealias",
        env: &[("ZIPP_NO_SPLICE_ALIAS", "1")],
    },
    Mode {
        name: "noshapes",
        env: &[("ZIPP_NO_SHAPES", "1")],
    },
    Mode {
        name: "noarrkeyfast",
        env: &[("ZIPP_NO_ARRKEY_FAST", "1")],
    },
    Mode {
        name: "nopolyeqfast",
        env: &[("ZIPP_NO_POLYEQ_FAST", "1")],
    },
    Mode {
        name: "nonursery",
        env: &[("ZIPP_NO_NURSERY", "1")],
    },
    // ── W19 ── the ordinary-object fast path at the head of `Vm::delete_prop`.
    // `DeoptKind::ElemDelete` emits `delete arr[7]`, so every program carrying
    // that deopt runs the new guard chain — and an Array is one of the receiver
    // kinds the guard must REJECT, which is the arm a mis-guard would break.
    //
    // Its two wave-mates are deliberately NOT rows here, on this file's own
    // `ZIPP_NO_ITER_REGION` reasoning: the generator emits no `delete o["k"+i]`
    // (so `ZIPP_NO_JIT_DELETE`, which gates the `DeleteIndexConcat` region arm,
    // would never see the op) and no object with enough keys to build a
    // `PropIndex` at all (so `ZIPP_NO_SPLIT_PROPINDEX` would never see the
    // table). Both rows would cost process time to prove nothing. They are
    // covered instead by `tests/w19_jit_delete.rs`, whose whole battery re-runs
    // in a child process under each latch, and by
    // `tests/w19_delete_differential.js`.
    Mode {
        name: "nodeletefast",
        env: &[("ZIPP_NO_DELETE_FASTPATH", "1")],
    },
    // ── W20 ── the two rungs that put an `arr.push(int)` and a linear-scan
    // bool allocator on the INT tier. Both are rows for the same reason the
    // W17 batch is: each is specified as a PURE FALLBACK, so a switch that
    // changes an answer is a bug whichever side is wrong. `nointpush` is the
    // one that matters most -- with it OFF the whole `Stmt::Push` family runs
    // on the boxed memory tier, which is the reference the register-tier arm
    // has to match byte for byte.
    Mode {
        name: "nointpush",
        env: &[("ZIPP_NO_INT_PUSH", "1")],
    },
    Mode {
        name: "noboolreuse",
        env: &[("ZIPP_NO_BOOL_REUSE", "1")],
    },
    // Two OPT-IN rows beside `intsplit`, which is where a path gets less
    // exercise than it needs by construction.
    Mode {
        name: "arrpinloose",
        env: &[("ZIPP_ARR_PIN_LOOSE", "1")],
    },
    Mode {
        name: "accalways",
        env: &[("ZIPP_ACC_ALWAYS_EMIT", "1")],
    },
];

/// The subset the normal suite runs: the interpreter, both threshold shifts, and
/// the switches whose emitters own the most register-allocation state.
const CI_MODES: &[&str] = &[
    "base",
    "nojit",
    "thr1",
    "thr200",
    "nogprhomes",
    "noicgate",
    "nointsplice",
    "notypesplit",
];

fn mode(name: &str) -> &'static Mode {
    MODES
        .iter()
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("unknown mode {name}"))
}

// ──────────────────────────────── prng ────────────────────────────────

/// splitmix64. Self-contained on purpose: the crate has no dev-dependencies and
/// a finding is only reproducible if the stream is pinned to this file.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xDEAD_BEEF_CAFE_F00D)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        (self.next() % n as u64) as usize
    }
    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

// ───────────────────────────────── ir ─────────────────────────────────

const TEMPS: usize = 4;
const BOOLS: usize = 4;
const DBLS: usize = 2;
const HOISTS: usize = 6;
const GLOBS: usize = 3;
const LEAFS: usize = 4;
const ARRS: usize = 8;

/// An operand. Everything here is guaranteed non-NaN, which is what licenses the
/// negated comparison spellings (`!(a >= b)` for `a < b`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Src {
    H,
    I,
    J,
    Q,
    Hoist(u8),
    Temp(u8),
    Glob(u8),
    Up,
    ALen,
    /// An f64 accumulator as an OPERAND. Only [`Flavor::DblScan`] produces one,
    /// which is what keeps every other flavor's calibration byte-identical: a
    /// double in an int expression is exact (the digest ends in `ToInt32`) but
    /// it also makes the region non-int, and the INT-tier flavors are built to
    /// stay int.
    Dbl(u8),
    Lit(i32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arr {
    Dense,
    Dense2,
    F64,
    I32,
    U8,
    Holey,
    Dbl,
    Str,
}

impl Arr {
    fn ix(self) -> usize {
        self as usize
    }
    fn name(self) -> &'static str {
        match self {
            Arr::Dense => "arr",
            Arr::Dense2 => "arr2",
            Arr::F64 => "farr",
            Arr::I32 => "iarr",
            Arr::U8 => "uarr",
            Arr::Holey => "harr",
            Arr::Dbl => "darr",
            Arr::Str => "str",
        }
    }
    fn all() -> [Arr; ARRS] {
        [
            Arr::Dense,
            Arr::Dense2,
            Arr::F64,
            Arr::I32,
            Arr::U8,
            Arr::Holey,
            Arr::Dbl,
            Arr::Str,
        ]
    }
}

#[derive(Clone, Copy, Debug)]
enum Idx {
    Mask(Src, u32),
    Mod(Src, u32),
    Raw(Src),
    Minus(Src, i32),
    Mul(Src, i32, u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    SEq,
    SNe,
}

/// `(src & mask) === val`, or `src === val` when `mask` is 0. Written this way so
/// that reducing a loop bound during shrinking cannot silently make the guard
/// unreachable — nothing here mentions the bound.
#[derive(Clone, Copy, Debug)]
struct Cond {
    src: Src,
    mask: u32,
    val: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IntOp {
    Add,
    Sub,
    Mul,
    Imul,
    XorShl,
    XorShr,
    XorSar,
    Or,
    And,
    Div,
    Mod,
    Ternary,
    Clz,
    MinMax,
    AddInt,
    /// Uncoerced accumulation. `h` may leave int32 and become a double, which is
    /// the i53-guard deopt path — and stays exact in both engines, because IEEE
    /// addition is exactly rounded and the digest ends in `ToInt32`.
    AddRaw,
    SubRaw,
    IncRaw,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DeoptKind {
    TempStr,
    TempUndef,
    TempObj,
    ElemDouble,
    ElemStr,
    ElemDelete,
    ArrShrink,
    ArrGrow,
    GlobDouble,
    ObjExtend,
    FnSwap,
    TypedOob,
}

#[derive(Clone, Debug)]
enum Stmt {
    Int {
        op: IntOp,
        a: Src,
        b: Src,
        n: u8,
    },
    BoolDef {
        k: u8,
        a: Src,
        b: Src,
        cmp: Cmp,
        neg: bool,
    },
    BoolUse {
        k: u8,
        c: i32,
        style: u8,
    },
    Read {
        arr: Arr,
        idx: Idx,
        t: u8,
        coerce: u8,
    },
    Write {
        arr: Arr,
        idx: Idx,
        v: Src,
    },
    ALen {
        arr: Arr,
    },
    If {
        a: Src,
        b: Src,
        cmp: Cmp,
        neg: bool,
        then_: Vec<Stmt>,
        else_: Vec<Stmt>,
    },
    Loop {
        var: char,
        n: u32,
        label: Option<u32>,
        body: Vec<Stmt>,
    },
    Break {
        label: Option<u32>,
        at: Cond,
    },
    Continue {
        label: Option<u32>,
        at: Cond,
    },
    Ret {
        at: Cond,
    },
    Leaf {
        f: u8,
        a: Src,
        b: Src,
    },
    Deep {
        a: Src,
    },
    Closure {
        a: Src,
    },
    Indirect {
        a: Src,
    },
    GlobRw {
        k: u8,
        a: Src,
        write: bool,
    },
    UpRw {
        a: Src,
    },
    Dbl {
        k: u8,
        op: u8,
        a: Src,
        f: u8,
    },
    DblMix {
        k: u8,
        style: u8,
    },
    Prop {
        k: u8,
        poly: u32,
        write: bool,
    },
    Deopt {
        kind: DeoptKind,
        at: Cond,
        k: u8,
    },
    Try {
        a: Src,
        body: Vec<Stmt>,
    },
    /// A definition that does not DOMINATE its uses: `if (cond) { t{k} = v; }`
    /// with an int constant, so the region still admits on a register tier and
    /// the fall-through path reaches every later read of `t{k}` without the def.
    ///
    /// This is the shape W17's
    /// [`open_conditional_def_loses_its_entry_load`] turns on, and W16's two
    /// `loop_home_liverange` faces were the same family — an analysis keyed on
    /// where a value is MENTIONED where it needs where the value is LIVE. The
    /// generator reached it once in 60,000 programs by accident (a
    /// `DeoptKind::TypedOob` guard that happened to write a temp a later index
    /// read), which is not reach, it is luck.
    CondDef {
        k: u8,
        at: Cond,
        v: i32,
    },
    /// Self-recursion — the ONLY shape that reaches Tier A.
    Rec {
        a: Src,
    },
    /// A kernel-local object whose fields are read and written in the loop: the
    /// admission shape for object scalar replacement (`[jit] SROA region`).
    Sroa {
        f: u8,
        op: u8,
        a: Src,
    },
    /// W20: `parr.push(v & 255)` -- the append the INT tier admits as of this
    /// wave, and the ONLY op on that tier that issues a call. `style` decides
    /// what the kernel reads back afterwards, which is what makes a STALE pin
    /// snapshot observable rather than merely unlucky:
    ///   0 the new length, 1 the element just appended (through the same pin
    ///   the helper rewrote), 2 an element from the array's stable prefix.
    /// The generator emits it into ordinary kernel bodies, so it lands beside
    /// bool temps, pinned element reads and nested loops -- the combination
    /// that has to survive the call-save/restore around the append.
    Push {
        v: Src,
        style: u8,
    },
}

/// Where the kernel lives.
///
/// This is not decoration: it selects a different REGISTER ALLOCATION regime in
/// the bytecode compiler, and therefore a different set of JIT plans. Inside a
/// function every expression node gets a fresh register (the generated kernels
/// run to `regs=74`); at script scope the temp numbering restarts and a
/// statement's registers are RECYCLED — so a register can be a pinned element
/// receiver over one range and a number over another, which is exactly and only
/// the shape B94 live-range splitting exists for. `[jit] … B94 split receiver`
/// never appeared in 400 programs of `Scope::Kernel`, and appears immediately at
/// script scope; `tier_coverage_report` counts it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// The body is `function kernel(n) { … }`, called `reps` times.
    Kernel,
    /// The body is the script, run `reps` times by an enclosing `for`.
    Script,
}

/// A value a POST-REGION use can name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PostVal {
    Temp(u8),
    Bool(u8),
    Dbl(u8),
    H,
    Glob(u8),
    Up,
    Elem(Arr, u32),
}

/// What happens to a value AFTER the loop that computed it.
///
/// The gap this closes: every use the generator emitted was INSIDE the region,
/// or was the return mix — and the return mix reads every local through `| 0` or
/// a truthiness test, both of which ERASE representation. A `Bool` home that
/// reads back as a raw `NaN` is falsy, so `(b ? 17 : 0)` gives the same answer
/// as an honest `false`; a live `typeof x` after a loop was found by a human
/// reading code, not by 138,300 generated programs.
#[derive(Clone, Debug)]
enum Post {
    /// `typeof x`, folded in as a small int — the representation probe.
    TypeOf(PostVal),
    /// Identity against each value the local can legally hold.
    Identity(PostVal),
    /// `d === d` and `1 / d < 0`: NaN-ness and the sign of zero, the two f64
    /// facts `| 0` throws away.
    DblShape(u8),
    /// The live-out crosses a CALL boundary, where it must be a well-formed
    /// boxed value and not a register home.
    Probe(PostVal),
    /// The live-out is stored into an Array and read straight back — a boxed
    /// store of whatever the home actually holds.
    Escape(PostVal),
    /// A SECOND hot loop, whose live-INS are the first region's live-OUTs.
    Loop2 { n: u32, vals: Vec<PostVal> },
}

#[derive(Clone, Debug)]
struct Program {
    scope: Scope,
    strict: bool,
    use_closure: bool,
    n: u32,
    reps: u32,
    hoists: [i64; HOISTS],
    leaf_kinds: [u8; LEAFS],
    /// The outer loop's bound. `ArrLen` is the dense back-edge shape — the guard
    /// is re-read every iteration and a `length` change mid-loop must be seen.
    bound: Bound,
    body: Vec<Stmt>,
    /// What happens to the loop's live-outs after it finishes. See [`Post`].
    post: Vec<Post>,
    /// One local DELIBERATELY left out of the return mix, so it is genuinely
    /// DEAD after the region.
    ///
    /// The complement of [`Post`], and it earns its place for the same reason:
    /// the mix used to read every live local, so no generated binding was ever
    /// dead-out, and `read_outside` is a decision the planner makes differently
    /// for each. It is what W17's [`open_conditional_def_loses_its_entry_load`]
    /// needs — the same program with `t` in the mix answers CORRECTLY, because
    /// being read after the region forced a permanent home with an entry load.
    /// One binding at a time, because every term dropped from the mix is
    /// sensitivity lost everywhere else.
    dead_out: Option<PostVal>,
    /// Fold every mutable datum the body could have touched into the answer, so
    /// a side effect that never reaches `h` still has to agree across tiers.
    /// The shrinker turns it off, because a minimal case reads better without
    /// it — and if the divergence WAS a side effect, the shrink is rejected.
    checksum: bool,
    /// Emit only the declarations and result mixes the body actually reaches.
    /// Off while generating (maximum sensitivity — an unused binding still gets
    /// a home and still shifts the allocation), tried by the shrinker at the end.
    trim: bool,
}

// ────────────────────────────── generator ──────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bound {
    N,
    ArrLen(Arr),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    /// Pure integer arithmetic on a carried accumulator — the INT / INT-GPR tier.
    Int,
    /// Bitwise chains and `Math.imul` — the GPR-home sub-mode.
    Bits,
    /// f64 accumulators — the REGALLOC/DOUBLE tier.
    Double,
    /// Typed and dense element traffic — pinning, splitting, backedge fast paths.
    Elem,
    /// Property loads and calls — MEM bodies and Tier C.
    Mem,
    /// Everything, mixed.
    Mixed,
    /// The REGISTER-PRESSURE enumeration: a flat integer loop carrying an
    /// explicit number of live boolean temps and an explicit number of distinct
    /// hoisted compare constants, with element reads placed among them.
    ///
    /// This exists because W14's clobber was a function of exactly those two
    /// counts and nothing else: `BOOL_GPRS[2]` is only occupied once (bools +
    /// hoisted compare mirrors) reaches three, so two is clean and three is
    /// wrong. Leaving that to a random statement menu leaves it to chance, and
    /// chance is what missed the bug for a month.
    Pressure,
    /// `Pressure` with nothing else in the loop: the canonical element scan,
    /// which is the shape the INT tier actually admits. Calibrated against the
    /// W14 tree — this is the only body form that reliably reaches the emitter
    /// that carried the defect, because a single `continue`, `Math.max`,
    /// uncoerced add, non-int32 literal or equality test against the accumulator
    /// anywhere in the loop declines the whole region.
    Scan,
    /// Field traffic on one global object, which is the only admission shape for
    /// object scalar replacement (`[jit] SROA region`).
    Sroa,
    /// The DOUBLE/REGALLOC tier's calibrated body — what [`Flavor::Scan`] is to
    /// the INT tier. See [`gen_double_body`] for the measured admission rules
    /// and for why [`Flavor::Double`] reached that tier in only 25 of 400
    /// programs without it.
    DblScan,
    /// The B94 SPLIT-RECEIVER shape. Always [`Scope::Script`]. See
    /// [`gen_split_body`].
    Split,
}

struct Gen<'a> {
    rng: &'a mut Rng,
    flavor: Flavor,
    depth: usize,
    labels: Vec<u32>,
    budget: usize,
    next_label: u32,
    big: bool,
}

const HOIST_POOL: [i64; 18] = [
    0,
    1,
    2,
    3,
    5,
    7,
    11,
    13,
    17,
    31,
    32,
    63,
    255,
    1024,
    1000003,
    -1,
    -7,
    -2147483648,
];

/// Constants for COMPARE operands, spelled as literals inside the loop.
///
/// The spelling is load-bearing and was found by calibration against the W14
/// tree. A constant declared as a function-scope `var c = 13` is a read-only
/// LIVE-IN of the region, and `plan_region` declines any region that uses a
/// read-only live-in "where a number isn't required" — which `===` is — so that
/// whole shape lands on the MEM tier and never sees the INT emitter at all. A
/// literal written inside the loop becomes an in-region `LoadConst` that the
/// planner hoists, and a hoisted constant compared against is exactly what
/// claims a `gpr_const` mirror in `BOOL_GPRS`. Values are small so that equality
/// against a data element actually MATCHES sometimes; a guard that never fires
/// tests nothing.
///
/// `-2147483648` is deliberately ABSENT. Its source spelling is unary minus over
/// the literal `2147483648`, which is not an int32, and one such constant
/// anywhere in a loop declines the whole region — so a pool containing it turns
/// one in fourteen pressure programs into a MEM program for a reason that has
/// nothing to do with the shape being enumerated. `HOIST_POOL` keeps it for
/// ARITHMETIC operands, where the idiv `INT_MIN / -1` edge is worth having.
const KONST_POOL: [i32; 14] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 13, 31, 255, -1, -7];

fn gen_program(seed: u64, big: bool) -> Program {
    let mut rng = Rng::new(seed);
    let flavor = rng.pick(&[
        Flavor::Int,
        Flavor::Bits,
        Flavor::Double,
        Flavor::Elem,
        Flavor::Mem,
        Flavor::Mixed,
        Flavor::Pressure,
        Flavor::Pressure,
        Flavor::Scan,
        Flavor::Scan,
        Flavor::Scan,
        Flavor::Sroa,
        Flavor::Int,
        Flavor::Elem,
        // W17: the DOUBLE tier's calibrated body, and three more INT-family
        // slots beside it. `DblScan` alone took 3 of 17 draws, which cost the
        // INT flavors 18% of their share and INT REGIONS 29% of their count
        // (686 -> 488 per 400 programs) — a tier traded for a tier. With these
        // the pool is 20 and the INT family holds the same 55% it held at 14.
        Flavor::DblScan,
        Flavor::DblScan,
        Flavor::DblScan,
        Flavor::Scan,
        Flavor::Scan,
        Flavor::Pressure,
        Flavor::Split,
        Flavor::Split,
    ]);
    // BIG, re-aimed. It used to mean "a bigger STATEMENT budget", and 5,000 BIG
    // programs found nothing while 5,000 ordinary ones found ~10 with the same
    // tier mix — a wider body dilutes the tight shapes that trip the register
    // allocator, which is the opposite of what a soak wants. (It had also never
    // actually run: the soak driver passed it through a bash assignment prefix
    // that expands to a command name, so every "BIG" soak ran the ordinary
    // generator. W16's gate found that.) BIG now means the same tight bodies
    // run LONGER and NEST DEEPER: more iterations per region, more repetitions,
    // and one more level of loop nesting — the axes that move WHEN a region
    // compiles, when it deopts, and when it is evicted and re-planned.
    let n = if big {
        rng.pick(&[12u32, 40, 120, 400, 400, 1200, 4000])
    } else {
        rng.pick(&[12u32, 40, 120, 400, 400])
    };
    let cap: u32 = if big { 48_000 } else { 4_800 };
    let reps = *[1u32, 3, 12, 40]
        .iter()
        .filter(|r| n * **r <= cap)
        .last()
        .unwrap_or(&1);
    let reps = rng.pick(&[1u32, 3, reps]).max(1);

    let mut hoists = [0i64; HOISTS];
    for h in hoists.iter_mut() {
        *h = rng.pick(&HOIST_POOL);
    }
    let mut leaf_kinds = [0u8; LEAFS];
    for k in leaf_kinds.iter_mut() {
        *k = rng.below(5) as u8;
    }

    let bound = if rng.chance(22) {
        Bound::ArrLen(rng.pick(&[Arr::Dense, Arr::Dense2, Arr::I32, Arr::Str]))
    } else {
        Bound::N
    };
    let budget = 3 + rng.below(8);
    let mut g = Gen {
        rng: &mut rng,
        flavor,
        depth: 0,
        labels: Vec::new(),
        budget,
        next_label: 0,
        big,
    };
    let body = if flavor == Flavor::Sroa {
        gen_sroa_block(&mut g)
    } else if flavor == Flavor::Split {
        gen_split_body(&mut g)
    } else if flavor == Flavor::DblScan {
        gen_double_body(&mut g)
    } else if flavor == Flavor::Pressure || flavor == Flavor::Scan {
        gen_pressure_body(&mut g, flavor == Flavor::Scan)
    } else {
        let count = 2 + g.rng.below(5);
        gen_stmts(&mut g, count)
    };
    // Post-region uses go on MOST programs: a live-out that nothing looks at
    // afterwards cannot expose a live-out defect, and the tight flavors are
    // where the register allocator is under the most pressure.
    let post = if g.rng.chance(72) {
        gen_post(&mut g, &body)
    } else {
        Vec::new()
    };
    let dead_out = if g.rng.chance(35) {
        gen_dead_out(&mut g, &body, &post)
    } else {
        None
    };
    // Script scope is an orthogonal axis (see `Scope`) and is drawn for every
    // flavor — but at a LOW rate, because it is not free: a script-scope loop
    // recycles registers, and a recycled register that carries two TYPES makes
    // the planner decline ("type conflict on a reused register"), so a wide
    // script-scope body lands on MEM. Measured: 30% script scope cost 32% of all
    // INT regions. `Flavor::Split` is the shape built to pay for itself there,
    // and takes the scope unconditionally.
    //
    // Script scope also forces `strict` off — a `"use strict"` directive has to
    // be the first statement of the file and the data declarations are — and
    // closure nesting off, which is a function-scope idea to begin with.
    let script = flavor == Flavor::Split || rng.chance(10);

    Program {
        scope: if script { Scope::Script } else { Scope::Kernel },
        strict: !script && rng.chance(50),
        use_closure: !script && rng.chance(35),
        n,
        reps,
        hoists,
        leaf_kinds,
        checksum: true,
        bound: match flavor {
            Flavor::Scan | Flavor::Sroa | Flavor::DblScan | Flavor::Split => Bound::N,
            Flavor::Pressure if rng.chance(85) => Bound::N,
            _ => bound,
        },
        body,
        post,
        dead_out,
        trim: matches!(
            flavor,
            Flavor::Pressure | Flavor::Scan | Flavor::Sroa | Flavor::DblScan | Flavor::Split
        ) && rng.chance(60),
    }
}

/// Build the flat pressure loop: `nb` bool homes, `nc` distinct hoisted compare
/// constants, and element reads dropped in at random positions among them. The
/// reads are placed by index rather than appended, so that across the seed space
/// a read lands BEFORE, BETWEEN and AFTER the bool definitions — W14 needed the
/// clobber to survive to a later use, in one case only across the back-edge.
fn gen_pressure_body(g: &mut Gen, strict: bool) -> Vec<Stmt> {
    // 1..4 bool homes and 0..3 hoisted compare constants: the two counts that
    // together decide whether anything lives in `BOOL_GPRS[2]`.
    let nb = 1 + g.rng.below(4);
    let nc = g.rng.below(4);
    let nreads = if strict { 1 } else { 1 + g.rng.below(2) };

    // Distinct constants, so `nb + nc` really is the number of distinct hoisted
    // compare constants the region carries.
    let base = g.rng.below(KONST_POOL.len());
    let konst = |c: usize| Src::Lit(KONST_POOL[(base + c) % KONST_POOL.len()]);

    let mut defs: Vec<Stmt> = Vec::new();
    for k in 0..nb {
        let cmp = g.rng.pick(&[
            Cmp::SEq,
            Cmp::SNe,
            Cmp::Lt,
            Cmp::Gt,
            Cmp::Le,
            Cmp::Ge,
            Cmp::Eq,
        ]);
        // The loop accumulator is admissible in an ORDERING compare (which
        // requires a number) but not in an equality one, so it is only offered
        // where the comparison is relational. Same reason the negated spelling is
        // withheld from equality here: `!(a !== k)` puts the constant in a
        // register, and a read-only live-in used where a number isn't required
        // declines the region.
        let equality = matches!(cmp, Cmp::SEq | Cmp::SNe | Cmp::Eq | Cmp::Ne);
        let a = if equality {
            g.rng.pick(&[Src::I, Src::I, Src::Temp(0), Src::Temp(0)])
        } else {
            g.rng
                .pick(&[Src::I, Src::I, Src::Temp(0), Src::Temp(0), Src::H])
        };
        let neg = !equality && g.rng.chance(40);
        defs.push(Stmt::BoolDef {
            k: k as u8,
            a,
            b: konst(k),
            cmp,
            neg,
        });
    }
    // Strictly integer filler. `region_is_int` is all-or-nothing: one uncoerced
    // add, one `Math.max`, one f64 multiply anywhere in the loop and the region
    // declines to MEM, taking the whole shape out of reach of the INT emitter.
    let int_filler = |g: &mut Gen| -> Stmt {
        let op = g
            .rng
            .pick(&[IntOp::Add, IntOp::Add, IntOp::XorShl, IntOp::Imul]);
        let a = g.rng.pick(&[Src::I, Src::H, Src::Temp(0), Src::Lit(3)]);
        let n = g.rng.pick(&[1u8, 2, 3, 5]);
        Stmt::Int {
            op,
            a,
            b: Src::I,
            n,
        }
    };

    let mut tail: Vec<Stmt> = Vec::new();
    for k in 0..nb {
        // Style 0 is `if (bk) …` — a real branch on the bool home, which is what
        // keeps the home live across the element read rather than folding it
        // into a select.
        // `if (bk) …` almost always: a branch keeps the bool in its gpr home
        // across the element read, where a select can fold it away.
        let style = if strict {
            0
        } else {
            g.rng.pick(&[0u8, 0, 0, 0, 0, 1, 2])
        };
        tail.push(Stmt::BoolUse {
            k: k as u8,
            c: g.rng.pick(&[1i32, 2, 4, 8, 17]),
            style,
        });
    }
    // Distinct hoisted constants used as compare operands: each one claims a
    // `gpr_const` mirror out of the SAME four-register pool the bools use.
    for c in 0..nc {
        let cmp = g.rng.pick(&[Cmp::SEq, Cmp::Lt, Cmp::Gt]);
        let equality = matches!(cmp, Cmp::SEq);
        let a = if equality {
            g.rng.pick(&[Src::Temp(0), Src::Temp(0), Src::I])
        } else {
            g.rng.pick(&[Src::Temp(0), Src::Temp(0), Src::H, Src::I])
        };
        let neg = !equality && g.rng.chance(35);
        let n = g.rng.pick(&[1u8, 2, 4]);
        tail.push(Stmt::If {
            a,
            b: konst(nb + c),
            cmp,
            neg,
            then_: vec![Stmt::Int {
                op: IntOp::AddInt,
                a: Src::I,
                b: Src::I,
                n,
            }],
            else_: Vec::new(),
        });
    }
    let op = g.rng.pick(&[IntOp::Add, IntOp::Add, IntOp::XorShl]);
    tail.push(Stmt::Int {
        op,
        a: Src::Temp(0),
        b: Src::I,
        n: 3,
    });

    if strict {
        // Nothing but the defs, one read, and the uses — see `Flavor::Scan`.
        tail.retain(|st| !matches!(st, Stmt::Int { .. }));
    }
    let mut body = defs;
    body.extend(tail);
    // A def of `t0` that does NOT dominate the reads below it. `t0` is the temp
    // this shape's compares and folds all read, so one of these makes every
    // later use reachable without a def — see `Stmt::CondDef`.
    if g.rng.chance(30) {
        let st = gen_cond_def(g, 0);
        body.insert(0, st);
    }
    for r in 0..nreads {
        // Dense int Arrays carried the defect; the typed, holey and string twins
        // are the controls that must not move.
        let arr = if r == 0 {
            g.rng.pick(&[
                Arr::Dense,
                Arr::Dense,
                Arr::Dense,
                Arr::Dense2,
                Arr::I32,
                Arr::Str,
            ])
        } else {
            g.rng.pick(&[
                Arr::Dense,
                Arr::Dense2,
                Arr::I32,
                Arr::F64,
                Arr::Holey,
                Arr::Str,
                Arr::Dbl,
                Arr::U8,
            ])
        };
        let idx = gen_idx(g);
        // `(t | 0)` and the imul mix keep the value integral; the `* 1024` mix
        // does not, and one of those in the loop declines the region.
        let coerce = if strict {
            0
        } else {
            g.rng.pick(&[0u8, 0, 0, 0, 3, 2])
        };
        // Mostly BETWEEN the definitions and the uses, which is where a scratched
        // register is observable; sometimes anywhere, which covers the variant
        // whose clobber is only seen after the back-edge.
        let at = if g.rng.chance(75) {
            nb + g.rng.below(body.len() + 1 - nb)
        } else {
            g.rng.below(body.len() + 1)
        };
        body.insert(
            at,
            Stmt::Read {
                arr,
                idx,
                t: r as u8,
                coerce,
            },
        );
    }
    // A couple of statements on top, appended rather than interleaved so they
    // cannot disturb the define/read/use ordering the shape is built around, and
    // drawn from the integer filler so they cannot decline the region either.
    // One program in six takes a free-form statement instead, which is what
    // pushes some of these onto MEM/Tier C on purpose.
    // Filler only. A `continue` — or a `break`, a `return`, a call, a `Math.max`
    // — anywhere in the loop declines the region to MEM, and this flavor exists
    // precisely to be ON the INT tier. Measured against the W14 tree: adding one
    // `continue` to a shape that miscompiles makes it stop miscompiling, because
    // it stops being compiled by that emitter at all. The other flavors carry the
    // control-flow coverage.
    let extra = if strict { 0 } else { g.rng.below(2) };
    for _ in 0..extra {
        let st = int_filler(g);
        body.push(st);
    }
    body
}

/// The DOUBLE / REGALLOC tier's calibrated body — what [`gen_pressure_body`]'s
/// `strict` mode is to the INT tier, and calibrated the same way: by putting
/// one construct at a time in a loop and watching whether
/// `[jit] DOUBLE region … compiled` survives.
///
/// [`Flavor::Double`] reached that tier in 25 of 400 programs. The tier is
/// all-or-nothing about a short list of ops, and the free-form statement menu
/// draws them constantly — measured on this tree, ONE of these anywhere in the
/// loop declines the whole region to MEM:
///
/// * any `MathOp` — `imul`, `floor`, `abs`, `sqrt`, `fround`, `min`, `max`,
///   `clz32` (`[decline-reason] regalloc-emit-unhandled: MathOp`). `Stmt::Dbl`
///   ops 2 and 4 and `Stmt::DblMix` style 1 are three of those, so a
///   Double-flavor loop with four double statements clears them ~16% of the
///   time;
/// * any `Call`;
/// * `t === undefined` — the coerce-2 element read ("read-only live-in used
///   where a number isn't required");
/// * `.length`, `charCodeAt`, an Int32Array or Uint8Array read (the DOUBLE
///   path's pin kind is 8, Float64), and an ordinary-Array WRITE.
///
/// What it DOES admit, each measured: f64 arithmetic (`*`, `/`, `+`),
/// Float64Array reads AND writes, dense ordinary-Array reads (B95), `d | 0` and
/// `(d * 1024) | 0`, bools defined from f64 compares, `if`/`else`, `break`,
/// `continue`, `return`, integer `/` and `%`, and nested loops. This body draws
/// only from that set.
fn gen_double_body(g: &mut Gen) -> Vec<Stmt> {
    let nd = 1 + g.rng.below(DBLS);
    let nb = g.rng.below(4);
    let mut out: Vec<Stmt> = Vec::new();

    // The f64 accumulators. Ops 0/1/3 only — 2 (`sqrt`/`abs`) and 4 (`fround`)
    // are MathOps and would take the region to MEM.
    //
    // `d0` never takes ITSELF as the addend, and that is a soundness rule, not a
    // style one. Every op here is `d = d*0.5 + a`, `d = a*f` or `d = d/3 + a`,
    // so an integer `a` holds `d` inside ~2·|a| forever — but `d0 = d0*0.5 + d0`
    // is `d0 *= 1.5`, which reaches `Infinity`, and `+Infinity + -Infinity` is
    // `NaN`. A NaN operand is the one thing that separates `a < b` from
    // `!(a >= b)`, and this file emits both spellings on the stated promise that
    // nothing it compares can be NaN. `d1` may read `d0` — bounded by a bounded
    // value is still bounded.
    for k in 0..nd {
        let op = g.rng.pick(&[0u8, 0, 1, 3]);
        let a = if k == 0 {
            g.rng.pick(&[Src::I, Src::H, Src::Temp(0), Src::Lit(3)])
        } else {
            g.rng
                .pick(&[Src::I, Src::H, Src::Temp(0), Src::Dbl(0), Src::Lit(3)])
        };
        out.push(Stmt::Dbl {
            k: k as u8,
            op,
            a,
            f: g.rng.below(6) as u8,
        });
    }
    // Bools from f64 compares — W16's two DOUBLE-tier defects were both a live
    // Bool losing `BOOL_GPRS[2]`, and a bool defined from a DOUBLE is the only
    // way to have one on this tier without an int chain beside it.
    for k in 0..nb {
        let cmp = g.rng.pick(&[Cmp::Lt, Cmp::Gt, Cmp::Le, Cmp::Ge]);
        let a = g
            .rng
            .pick(&[Src::Dbl(0), Src::Dbl(0), Src::H, Src::I, Src::Temp(0)]);
        let (k1, k2) = (g.rng.pick(&KONST_POOL), g.rng.pick(&KONST_POOL));
        let b = g
            .rng
            .pick(&[Src::Dbl((nd - 1) as u8), Src::Lit(k1), Src::Lit(k2), Src::I]);
        out.push(Stmt::BoolDef {
            k: k as u8,
            a,
            b,
            cmp,
            neg: g.rng.chance(35),
        });
    }
    for k in 0..nb {
        // Style 0 is a real branch on the bool home, which is what keeps it live
        // across the element traffic instead of folding it into a select.
        let style = g.rng.pick(&[0u8, 0, 0, 1, 2]);
        out.push(Stmt::BoolUse {
            k: k as u8,
            c: g.rng.pick(&[1i32, 2, 4, 8, 17]),
            style,
        });
    }
    // Element traffic, from the arrays this tier pins: a Float64Array (kind 8)
    // and a dense ordinary Array (B95). Reads only, plus the one admitted write.
    for r in 0..g.rng.below(3) {
        let arr = g
            .rng
            .pick(&[Arr::F64, Arr::F64, Arr::Dbl, Arr::Dense, Arr::Dense2]);
        let idx = gen_idx(g);
        // coerce 2 (`=== undefined`) and 3 (`Math.imul`) both decline.
        let coerce = g.rng.pick(&[0u8, 0, 1]);
        out.push(Stmt::Read {
            arr,
            idx,
            t: r as u8,
            coerce,
        });
    }
    if g.rng.chance(30) {
        out.push(Stmt::Write {
            arr: Arr::F64,
            idx: gen_idx(g),
            v: Src::H,
        });
    }
    // An inner loop, so a double home has to survive a back-edge — the shape
    // that carried W16's `loop_home_liverange` class on this exact tier.
    if g.rng.chance(35) {
        let k = g.rng.below(DBLS) as u8;
        // `Src::J`, not `Src::Dbl(0)` — see the boundedness rule above.
        let inner = vec![
            Stmt::Dbl {
                k,
                op: g.rng.pick(&[0u8, 3]),
                a: Src::J,
                f: g.rng.below(6) as u8,
            },
            Stmt::DblMix {
                k,
                style: g.rng.pick(&[0u8, 2]),
            },
        ];
        out.push(Stmt::Loop {
            var: 'j',
            n: g.rng.pick(&[2u32, 3, 4]),
            label: None,
            body: inner,
        });
    }
    if g.rng.chance(30) {
        let cmp = g.rng.pick(&[Cmp::Lt, Cmp::Gt, Cmp::Ge]);
        out.push(Stmt::If {
            a: Src::Dbl(0),
            b: Src::Lit(g.rng.pick(&KONST_POOL)),
            cmp,
            neg: g.rng.chance(35),
            then_: vec![Stmt::Int {
                op: IntOp::AddInt,
                a: Src::I,
                b: Src::I,
                n: 3,
            }],
            else_: if g.rng.chance(40) {
                vec![Stmt::Int {
                    op: IntOp::Sub,
                    a: Src::I,
                    b: Src::I,
                    n: 1,
                }]
            } else {
                Vec::new()
            },
        });
    }
    if g.rng.chance(22) {
        match g.rng.below(3) {
            0 => out.push(Stmt::Continue {
                label: None,
                at: gen_cond(g, 0),
            }),
            1 => out.push(Stmt::Break {
                label: None,
                at: gen_cond(g, 1),
            }),
            _ => out.push(Stmt::Ret { at: gen_cond(g, 2) }),
        }
    }
    if g.rng.chance(25) {
        let st = gen_cond_def(g, 0);
        out.insert(0, st);
    }
    // Always fold a double into `h`: style 1 is `Math.floor` and declines.
    for k in 0..nd {
        out.push(Stmt::DblMix {
            k: k as u8,
            style: g.rng.pick(&[0u8, 0, 2]),
        });
    }
    out
}

/// The B94 SPLIT-RECEIVER shape, calibrated the way [`Flavor::Scan`] was.
///
/// `[jit] … B94 split receiver` never appeared in 400 programs of the W15
/// generator, and the reason turned out to be structural rather than statistical:
/// a split needs the bytecode compiler to RECYCLE a register — pinned element
/// receiver over one range, number over another — and inside a function it never
/// does. The generated kernels run to `regs=74` with every expression node
/// getting its own register; at script scope the temp numbering restarts per
/// statement and `LoadGlobal r7 <- arr` sits two instructions after
/// `LoadConst r7 <- 0.25`. That is the whole difference, and it is why this
/// flavor is unconditionally [`Scope::Script`].
///
/// The second ingredient is SMALLNESS. A wide script-scope body recycles a
/// register across two TYPES and the planner declines the region outright
/// ("type conflict on a reused register"); a bool assigned at script scope also
/// declines it ("branch condition is not a bool", a global is not typed Bool).
/// So this body is two to four statements: an element read or two, one
/// accumulator, and at most one extra. Measured on the current tree, that shape
/// reaches a split on BOTH register tiers — `[jit] INT-GPR region … B94 split
/// receiver` for the int spelling and `[jit] DOUBLE region … B94 split receiver`
/// for the f64 one — and two reads produce TWO split receivers at once, which is
/// what `ZIPP_NO_MULTI_SPLIT` governs.
fn gen_split_body(g: &mut Gen) -> Vec<Stmt> {
    let dbl = g.rng.chance(55);
    let nreads = 1 + g.rng.below(2);
    let mut out: Vec<Stmt> = Vec::new();

    // An inner loop first, so the outer region carries a nested one — the
    // enclosing region is where the receiver and the inner counter compete.
    if g.rng.chance(25) {
        let inner = if dbl {
            vec![Stmt::Dbl {
                k: 0,
                op: g.rng.pick(&[0u8, 3]),
                a: Src::J,
                f: g.rng.below(6) as u8,
            }]
        } else {
            vec![Stmt::Int {
                op: IntOp::Add,
                a: Src::J,
                b: Src::I,
                n: 1,
            }]
        };
        out.push(Stmt::Loop {
            var: 'j',
            n: g.rng.pick(&[2u32, 3]),
            label: None,
            body: inner,
        });
    }
    for r in 0..nreads {
        let arr = if dbl {
            g.rng
                .pick(&[Arr::F64, Arr::F64, Arr::Dbl, Arr::Dense, Arr::Dense2])
        } else {
            g.rng.pick(&[
                Arr::Dense,
                Arr::Dense,
                Arr::Dense2,
                Arr::I32,
                Arr::U8,
                Arr::Str,
            ])
        };
        // coerce 2 (`=== undefined`) and 3 (`Math.imul`) both decline the DOUBLE
        // tier; on the int side they are fine and 3 is the fnv chain.
        let coerce = if dbl {
            g.rng.pick(&[0u8, 0, 1])
        } else {
            g.rng.pick(&[0u8, 0, 1, 3])
        };
        out.push(Stmt::Read {
            arr,
            idx: gen_idx(g),
            t: r as u8,
            coerce,
        });
    }
    if dbl {
        out.push(Stmt::Dbl {
            k: 0,
            op: g.rng.pick(&[0u8, 0, 1, 3]),
            a: Src::Temp(0),
            f: g.rng.below(6) as u8,
        });
        out.push(Stmt::DblMix {
            k: 0,
            style: g.rng.pick(&[0u8, 0, 2]),
        });
    } else {
        let op = g.rng.pick(&[
            IntOp::Add,
            IntOp::XorShl,
            IntOp::Imul,
            IntOp::Or,
            IntOp::Sub,
        ]);
        out.push(Stmt::Int {
            op,
            a: Src::Temp(0),
            b: Src::I,
            n: g.rng.pick(&[1u8, 3, 5]),
        });
    }
    if g.rng.chance(25) {
        let k = g.rng.below(2) as u8;
        let st = gen_cond_def(g, k);
        out.insert(0, st);
    }
    // The receiver's OTHER half: a store through the same array, which is what
    // makes the split's write-through observable rather than merely planned.
    if g.rng.chance(28) {
        let arr = if dbl { Arr::F64 } else { Arr::Dense2 };
        out.push(Stmt::Write {
            arr,
            idx: masked_idx(g),
            v: Src::H,
        });
    }
    // One late type change, so the region actually DEOPTS inside the receiver
    // window — the exit W16's `cold_pinned_recv` suite is about.
    if g.rng.chance(22) {
        let kind = g.rng.pick(&[
            DeoptKind::ElemDouble,
            DeoptKind::ElemStr,
            DeoptKind::ArrShrink,
            DeoptKind::TypedOob,
        ]);
        out.push(Stmt::Deopt {
            kind,
            at: gen_cond(g, 2),
            k: 0,
        });
    }
    out
}

/// Pick one local the body writes to leave OUT of the return mix. See
/// `Program::dead_out`.
///
/// Never one a post-region use already names — that would make the post
/// statement the only reader and leave the two axes fighting over one binding.
fn gen_dead_out(g: &mut Gen, body: &[Stmt], post: &[Post]) -> Option<PostVal> {
    let mut u = Used::default();
    collect_stmts(body, &mut u);
    let mut named = Used::default();
    collect_post(post, &mut named);
    let mut cands: Vec<PostVal> = Vec::new();
    for k in 0..TEMPS {
        if u.temps[k] && !named.temps[k] {
            cands.push(PostVal::Temp(k as u8));
        }
    }
    for k in 0..BOOLS {
        if u.bools[k] && !named.bools[k] {
            cands.push(PostVal::Bool(k as u8));
        }
    }
    for k in 0..DBLS {
        if u.dbls[k] && !named.dbls[k] {
            cands.push(PostVal::Dbl(k as u8));
        }
    }
    if cands.is_empty() {
        return None;
    }
    Some(cands[g.rng.below(cands.len())])
}

/// What happens to the loop's live-outs after it finishes. See [`Post`].
///
/// Values are drawn from what the BODY actually touched, so a probe always
/// names something the region really carried — probing a binding the loop never
/// wrote tests the declaration, not the allocator.
fn gen_post(g: &mut Gen, body: &[Stmt]) -> Vec<Post> {
    let mut u = Used::default();
    collect_stmts(body, &mut u);
    let mut vals: Vec<PostVal> = Vec::new();
    for k in 0..TEMPS {
        if u.temps[k] {
            vals.push(PostVal::Temp(k as u8));
        }
    }
    for k in 0..BOOLS {
        if u.bools[k] {
            vals.push(PostVal::Bool(k as u8));
        }
    }
    for k in 0..DBLS {
        if u.dbls[k] {
            vals.push(PostVal::Dbl(k as u8));
        }
    }
    for k in 0..GLOBS {
        if u.globs[k] {
            vals.push(PostVal::Glob(k as u8));
        }
    }
    if u.up {
        vals.push(PostVal::Up);
    }
    for (i, used) in u.arrs.iter().enumerate() {
        if *used && Arr::all()[i] != Arr::Str {
            vals.push(PostVal::Elem(Arr::all()[i], g.rng.pick(&[0u32, 3, 7, 31])));
        }
    }
    vals.push(PostVal::H);

    let n = 1 + g.rng.below(3);
    let mut out = Vec::new();
    for _ in 0..n {
        let v = vals[g.rng.below(vals.len())];
        let dbl = match v {
            PostVal::Dbl(k) => Some(k),
            _ => None,
        };
        out.push(match g.rng.below(if dbl.is_some() { 7 } else { 6 }) {
            0 | 1 => Post::TypeOf(v),
            2 => Post::Identity(v),
            3 => Post::Probe(v),
            4 => Post::Escape(v),
            5 => {
                let mut picked: Vec<PostVal> = Vec::new();
                for _ in 0..1 + g.rng.below(3) {
                    picked.push(vals[g.rng.below(vals.len())]);
                }
                Post::Loop2 {
                    n: g.rng.pick(&[12u32, 40, 120]),
                    vals: picked,
                }
            }
            _ => Post::DblShape(dbl.unwrap()),
        });
    }
    out
}

fn gen_src(g: &mut Gen) -> Src {
    let mut pool: Vec<Src> = vec![
        Src::H,
        Src::I,
        Src::Hoist(g.rng.below(HOISTS) as u8),
        Src::Hoist(g.rng.below(HOISTS) as u8),
        Src::Temp(g.rng.below(TEMPS) as u8),
        Src::Lit(
            g.rng
                .pick(&[0i32, 1, 2, 3, 7, 255, -1, 65535, 1000003, -2147483648]),
        ),
    ];
    if g.depth >= 1 {
        pool.push(Src::J);
    }
    if g.depth >= 2 {
        pool.push(Src::Q);
    }
    if matches!(g.flavor, Flavor::Mem | Flavor::Mixed | Flavor::Int) {
        pool.push(Src::Glob(g.rng.below(GLOBS) as u8));
        pool.push(Src::Up);
        pool.push(Src::ALen);
    }
    let i = g.rng.below(pool.len());
    pool[i]
}

/// An operand for the COMPARISON forms specifically. Biased hard toward in-loop
/// literals, because a hoisted constant compared against is what claims a
/// `gpr_const` mirror in `BOOL_GPRS` — the resource W14's clobber destroyed. The
/// function-scope `c{k}` live-in spelling stays in the pool at low weight: it
/// reaches the MEM tier instead, which is a different thing worth covering.
fn gen_cmp_src(g: &mut Gen) -> Src {
    let pool = [
        Src::Lit(g.rng.pick(&KONST_POOL)),
        Src::Lit(g.rng.pick(&KONST_POOL)),
        Src::Lit(g.rng.pick(&KONST_POOL)),
        Src::Lit(g.rng.pick(&KONST_POOL)),
        Src::H,
        Src::I,
        Src::Temp(g.rng.below(TEMPS) as u8),
        Src::Temp(g.rng.below(TEMPS) as u8),
        Src::Hoist(g.rng.below(HOISTS) as u8),
    ];
    g.rng.pick(&pool)
}

/// A write index that cannot GROW an ordinary Array without bound.
///
/// `Idx::Raw`/`Idx::Minus` over a temp is fine to READ with — an out-of-range
/// read yields `undefined`, which is the point — but a STORE at
/// `arr[1503238553]` sets `length` to 1.5 billion and the program's own checksum
/// loop then never finishes. `gen_stmt` has always masked its writes for exactly
/// this reason; the same rule is stated here once so the other body builders can
/// share it.
fn masked_idx(g: &mut Gen) -> Idx {
    match gen_idx(g) {
        Idx::Raw(s) | Idx::Minus(s, _) => Idx::Mask(s, 63),
        other => other,
    }
}

fn gen_idx(g: &mut Gen) -> Idx {
    let base = {
        let mut pool = vec![
            Src::I,
            Src::I,
            Src::H,
            Src::Hoist(g.rng.below(HOISTS) as u8),
        ];
        if g.depth >= 1 {
            pool.push(Src::J);
        }
        if g.depth >= 2 {
            pool.push(Src::Q);
        }
        pool.push(Src::Temp(g.rng.below(TEMPS) as u8));
        let i = g.rng.below(pool.len());
        pool[i]
    };
    match g.rng.below(8) {
        0 => Idx::Mod(base, g.rng.pick(&[8u32, 16, 32])),
        1 => Idx::Raw(base),
        2 => Idx::Minus(base, g.rng.pick(&[1i32, 4, -3])),
        3 => Idx::Mul(
            base,
            g.rng.pick(&[3i32, 5, 7]),
            g.rng.pick(&[15u32, 31, 63]),
        ),
        _ => Idx::Mask(base, g.rng.pick(&[7u32, 15, 31, 63])),
    }
}

fn gen_cond(g: &mut Gen, kind: u8) -> Cond {
    // kind 0 = continue (may fire often), 1 = break, 2 = return / deopt (must
    // fire LATE, past the OSR point, or the region never gets hot), 3 = NEVER
    // (`i === 100000`, past every bound this generator emits) — the cold-block
    // spelling, where the interpreter has no profile for the block at all.
    if kind == 3 {
        return Cond {
            src: Src::I,
            mask: 0,
            val: 100_000,
        };
    }
    let src = match g.rng.below(if g.depth >= 1 { 4 } else { 3 }) {
        0 => Src::I,
        1 => Src::H,
        2 => Src::I,
        _ => Src::J,
    };
    match kind {
        0 => Cond {
            src,
            mask: g.rng.pick(&[3u32, 7, 15]),
            val: g.rng.below(4) as u32,
        },
        1 => Cond {
            src,
            mask: g.rng.pick(&[15u32, 31, 63]),
            val: g.rng.below(8) as u32,
        },
        _ => {
            if g.rng.chance(60) {
                Cond {
                    src: Src::I,
                    mask: 0,
                    val: g.rng.pick(&[5u32, 9, 17, 33, 65, 200]),
                }
            } else {
                Cond {
                    src,
                    mask: g.rng.pick(&[127u32, 255]),
                    val: g.rng.pick(&[100u32, 200]),
                }
            }
        }
    }
}

/// `if (cond) { t{k} = v; }` — see [`Stmt::CondDef`]. Half the conditions
/// NEVER fire (the cold-block spelling) and half fire LATE, past the OSR point,
/// so the interpreter's profile for the block differs between the two.
fn gen_cond_def(g: &mut Gen, k: u8) -> Stmt {
    let kind = if g.rng.chance(50) { 3 } else { 2 };
    let at = gen_cond(g, kind);
    Stmt::CondDef {
        k,
        at,
        v: g.rng.pick(&[1i32, 2, 5, 7, 13, 0, -1]),
    }
}

fn gen_stmts(g: &mut Gen, count: usize) -> Vec<Stmt> {
    let mut out = Vec::new();
    for _ in 0..count {
        if g.budget == 0 {
            break;
        }
        g.budget -= 1;
        out.push(gen_stmt(g));
    }
    if out.is_empty() {
        out.push(Stmt::Int {
            op: IntOp::Add,
            a: Src::I,
            b: Src::I,
            n: 1,
        });
    }
    out
}

fn gen_stmt(g: &mut Gen) -> Stmt {
    // Menu weights per flavor. Every flavor keeps a floor of int arithmetic so a
    // region always has a carried accumulator to be wrong about.
    let w_int = match g.flavor {
        Flavor::Int => 34,
        Flavor::Bits => 30,
        Flavor::Double => 0,
        Flavor::Elem => 14,
        Flavor::Mem => 12,
        Flavor::Mixed => 18,
        Flavor::Pressure | Flavor::Scan => 24,
        Flavor::Sroa => 10,
        // Never consulted — these two build their whole body themselves, the
        // same way `Flavor::Scan` does — but the match has to be exhaustive.
        Flavor::DblScan | Flavor::Split => 0,
    };
    let w_bool = match g.flavor {
        Flavor::Int | Flavor::Bits => 20,
        Flavor::Pressure | Flavor::Scan => 22,
        Flavor::Sroa => 4,
        Flavor::Double => 6,
        _ => 14,
    };
    let w_elem = match g.flavor {
        Flavor::Elem => 30,
        Flavor::Mem => 8,
        Flavor::Double => 12,
        Flavor::Pressure | Flavor::Scan => 20,
        Flavor::Sroa => 2,
        _ => 14,
    };
    let w_dbl = match g.flavor {
        Flavor::Double => 34,
        _ => 6,
    };
    let w_call = match g.flavor {
        Flavor::Mem => 22,
        Flavor::Pressure | Flavor::Scan | Flavor::Sroa | Flavor::Double => 2,
        _ => 8,
    };
    let w_prop = match g.flavor {
        Flavor::Mem => 20,
        Flavor::Pressure | Flavor::Scan => 2,
        Flavor::Sroa => 30,
        _ => 6,
    };
    let w_ctrl = 12;
    let w_deopt = 6;
    let w_loop = if g.depth < 2 && g.budget > 1 { 8 } else { 0 };
    let w_if = if g.depth < 3 && g.budget > 0 { 10 } else { 0 };
    let w_try = if g.depth < 2 { 3 } else { 0 };
    // Tier A (self-recursion) and object scalar replacement are whole tiers that
    // no other row can reach.
    let w_shape = match g.flavor {
        Flavor::Mem | Flavor::Mixed => 10,
        Flavor::Pressure | Flavor::Scan | Flavor::Sroa | Flavor::Double => 2,
        _ => 5,
    };

    let table: [(u64, u8); 12] = [
        (w_int, 0),
        (w_bool, 1),
        (w_elem, 2),
        (w_dbl, 3),
        (w_call, 4),
        (w_prop, 5),
        (w_ctrl, 6),
        (w_deopt, 7),
        (w_loop, 8),
        (w_if, 9),
        (w_try, 10),
        (w_shape, 11),
    ];
    let total: u64 = table.iter().map(|(w, _)| *w).sum();
    let mut r = g.rng.next() % total;
    let mut kind = 0u8;
    for (w, k) in table {
        if r < w {
            kind = k;
            break;
        }
        r -= w;
    }

    match kind {
        0 => {
            let op = match g.flavor {
                Flavor::Bits => g.rng.pick(&[
                    IntOp::XorShl,
                    IntOp::XorShr,
                    IntOp::XorSar,
                    IntOp::Or,
                    IntOp::And,
                    IntOp::Imul,
                    IntOp::Clz,
                ]),
                _ => g.rng.pick(&[
                    IntOp::Add,
                    IntOp::AddInt,
                    IntOp::AddRaw,
                    IntOp::SubRaw,
                    IntOp::IncRaw,
                    IntOp::Sub,
                    IntOp::Mul,
                    IntOp::Imul,
                    IntOp::XorShl,
                    IntOp::Div,
                    IntOp::Mod,
                    IntOp::Ternary,
                    IntOp::MinMax,
                    IntOp::Or,
                    IntOp::And,
                ]),
            };
            let a = gen_src(g);
            let b = gen_src(g);
            Stmt::Int {
                op,
                a,
                b,
                n: g.rng.pick(&[1u8, 2, 3, 5, 8, 13, 31]),
            }
        }
        1 => {
            if g.rng.chance(55) {
                let cmp = g.rng.pick(&[
                    Cmp::Lt,
                    Cmp::Le,
                    Cmp::Gt,
                    Cmp::Ge,
                    Cmp::Eq,
                    Cmp::Ne,
                    Cmp::SEq,
                    Cmp::SNe,
                ]);
                Stmt::BoolDef {
                    k: g.rng.below(BOOLS) as u8,
                    a: gen_cmp_src(g),
                    b: gen_cmp_src(g),
                    cmp,
                    neg: g.rng.chance(45),
                }
            } else {
                Stmt::BoolUse {
                    k: g.rng.below(BOOLS) as u8,
                    c: g.rng.pick(&[1i32, 3, 17, 255, -9]),
                    style: g.rng.below(3) as u8,
                }
            }
        }
        2 => {
            let arr = g.rng.pick(&Arr::all());
            if g.rng.chance(68) || arr == Arr::Str {
                Stmt::Read {
                    arr,
                    idx: gen_idx(g),
                    t: g.rng.below(TEMPS) as u8,
                    coerce: g.rng.below(4) as u8,
                }
            } else if g.rng.chance(15) {
                Stmt::ALen { arr }
            } else if g.rng.chance(22) {
                Stmt::Push {
                    v: gen_src(g),
                    style: g.rng.below(3) as u8,
                }
            } else {
                // Writes always use a masked index so an array cannot grow
                // without bound; reads deliberately may go out of range.
                let idx = masked_idx(g);
                Stmt::Write {
                    arr,
                    idx,
                    v: gen_src(g),
                }
            }
        }
        3 => {
            if g.rng.chance(60) {
                Stmt::Dbl {
                    k: g.rng.below(DBLS) as u8,
                    op: g.rng.below(5) as u8,
                    a: gen_src(g),
                    f: g.rng.below(6) as u8,
                }
            } else {
                Stmt::DblMix {
                    k: g.rng.below(DBLS) as u8,
                    style: g.rng.below(3) as u8,
                }
            }
        }
        4 => match g.rng.below(4) {
            0 => Stmt::Deep { a: gen_src(g) },
            1 => Stmt::Closure { a: gen_src(g) },
            2 => Stmt::Indirect { a: gen_src(g) },
            _ => Stmt::Leaf {
                f: g.rng.below(LEAFS) as u8,
                a: gen_src(g),
                b: gen_src(g),
            },
        },
        5 => {
            if g.rng.chance(70) {
                Stmt::Prop {
                    k: g.rng.below(3) as u8,
                    poly: g.rng.pick(&[0u32, 1, 3, 7, 15]),
                    write: false,
                }
            } else {
                Stmt::Prop {
                    k: g.rng.below(3) as u8,
                    poly: g.rng.pick(&[0u32, 1, 3, 7, 15]),
                    write: true,
                }
            }
        }
        6 => {
            let lbl = if !g.labels.is_empty() && g.rng.chance(35) {
                Some(g.labels[g.rng.below(g.labels.len())])
            } else {
                None
            };
            match g.rng.below(6) {
                0 | 1 => Stmt::Continue {
                    label: lbl,
                    at: gen_cond(g, 0),
                },
                2 | 3 => Stmt::Break {
                    label: lbl,
                    at: gen_cond(g, 1),
                },
                4 => Stmt::Ret { at: gen_cond(g, 2) },
                _ => {
                    if g.rng.chance(50) {
                        Stmt::GlobRw {
                            k: g.rng.below(GLOBS) as u8,
                            a: gen_src(g),
                            write: g.rng.chance(60),
                        }
                    } else {
                        Stmt::UpRw { a: gen_src(g) }
                    }
                }
            }
        }
        7 => {
            if g.rng.chance(30) {
                let k = g.rng.below(TEMPS) as u8;
                return gen_cond_def(g, k);
            }
            let kind = g.rng.pick(&[
                DeoptKind::TempStr,
                DeoptKind::TempUndef,
                DeoptKind::TempObj,
                DeoptKind::ElemDouble,
                DeoptKind::ElemStr,
                DeoptKind::ElemDelete,
                DeoptKind::ArrShrink,
                DeoptKind::ArrGrow,
                DeoptKind::GlobDouble,
                DeoptKind::ObjExtend,
                DeoptKind::FnSwap,
                DeoptKind::TypedOob,
            ]);
            Stmt::Deopt {
                kind,
                at: gen_cond(g, 2),
                k: g.rng.below(TEMPS) as u8,
            }
        }
        8 => {
            let var = if g.depth == 0 { 'j' } else { 'q' };
            let n = if g.depth == 0 {
                g.rng.pick(&[2u32, 3, 4, 8])
            } else {
                g.rng.pick(&[2u32, 3, 4])
            };
            let label = if g.rng.chance(40) {
                let l = g.next_label;
                g.next_label += 1;
                Some(l)
            } else {
                None
            };
            g.depth += 1;
            if let Some(l) = label {
                g.labels.push(l);
            }
            let cnt = 1 + g.rng.below(if g.big { 4 } else { 3 });
            let body = gen_stmts(g, cnt);
            if label.is_some() {
                g.labels.pop();
            }
            g.depth -= 1;
            Stmt::Loop {
                var,
                n,
                label,
                body,
            }
        }
        9 => {
            let cmp = g.rng.pick(&[
                Cmp::Lt,
                Cmp::Le,
                Cmp::Gt,
                Cmp::Ge,
                Cmp::Eq,
                Cmp::Ne,
                Cmp::SEq,
            ]);
            let a = gen_cmp_src(g);
            let b = gen_cmp_src(g);
            let neg = g.rng.chance(45);
            g.depth += 1;
            let nt = 1 + g.rng.below(2);
            let then_ = gen_stmts(g, nt);
            let want_else = g.rng.chance(35);
            let ne = 1 + g.rng.below(2);
            let else_ = if want_else {
                gen_stmts(g, ne)
            } else {
                Vec::new()
            };
            g.depth -= 1;
            Stmt::If {
                a,
                b,
                cmp,
                neg,
                then_,
                else_,
            }
        }
        10 => {
            g.depth += 1;
            let nb = 1 + g.rng.below(2);
            let body = gen_stmts(g, nb);
            g.depth -= 1;
            Stmt::Try {
                a: gen_src(g),
                body,
            }
        }
        _ => {
            if g.rng.chance(35) {
                Stmt::Rec { a: gen_src(g) }
            } else {
                let f = g.rng.below(3) as u8;
                let op = g.rng.below(3) as u8;
                Stmt::Sroa {
                    f,
                    op,
                    a: gen_src(g),
                }
            }
        }
    }
}

/// Field traffic on the global SROA object, as a block: `plan_field_promotion`
/// wants the region's heap ops to ALL target the one object, so a lone field
/// write among unrelated work never qualifies.
fn gen_sroa_block(g: &mut Gen) -> Vec<Stmt> {
    let n = 2 + g.rng.below(3);
    let mut out = Vec::new();
    for k in 0..n {
        let f = (k % 3) as u8;
        let op = if k + 1 == n { 2 } else { g.rng.below(2) as u8 };
        let a = g.rng.pick(&[Src::I, Src::H, Src::Lit(3)]);
        out.push(Stmt::Sroa { f, op, a });
    }
    out
}

// ─────────────────────────── used-binding analysis ───────────────────────────

#[derive(Default, Clone)]
struct Used {
    temps: [bool; TEMPS],
    bools: [bool; BOOLS],
    dbls: [bool; DBLS],
    hoists: [bool; HOISTS],
    globs: [bool; GLOBS],
    arrs: [bool; ARRS],
    leafs: [bool; LEAFS],
    deep: bool,
    objs: bool,
    thrower: bool,
    cl: bool,
    up: bool,
    fnref: bool,
    rec: bool,
    sroa: bool,
    /// `probe(x)` — the post-region call-boundary check.
    probe: bool,
    /// `w`, the second region's counter.
    loop2: bool,
    /// W20: `parr`, the growable array `Stmt::Push` appends to.
    push_arr: bool,
    labels: Vec<u32>,
}

impl Used {
    fn everything() -> Used {
        Used {
            push_arr: true,
            temps: [true; TEMPS],
            bools: [true; BOOLS],
            dbls: [true; DBLS],
            hoists: [true; HOISTS],
            globs: [true; GLOBS],
            arrs: [true; ARRS],
            leafs: [true; LEAFS],
            deep: true,
            objs: true,
            thrower: true,
            cl: true,
            up: true,
            fnref: true,
            rec: true,
            sroa: true,
            probe: true,
            loop2: false,
            labels: Vec::new(),
        }
    }
    fn src(&mut self, s: Src) {
        match s {
            Src::Hoist(k) => self.hoists[k as usize] = true,
            Src::Temp(k) => self.temps[k as usize] = true,
            Src::Glob(k) => self.globs[k as usize] = true,
            Src::Dbl(k) => self.dbls[k as usize] = true,
            Src::Up => self.up = true,
            Src::ALen => self.arrs[Arr::Dense.ix()] = true,
            _ => {}
        }
    }
    fn idx(&mut self, i: Idx) {
        match i {
            Idx::Mask(s, _)
            | Idx::Mod(s, _)
            | Idx::Raw(s)
            | Idx::Minus(s, _)
            | Idx::Mul(s, _, _) => self.src(s),
        }
    }
    fn close(&mut self, prog: &Program) {
        // `fnref` is initialised to leaf0 and swapped to leaf1; `deep` calls both.
        if self.fnref || self.deep {
            self.leafs[0] = true;
            self.leafs[1] = true;
        }
        for k in 0..LEAFS {
            if self.leafs[k] && prog.leaf_kinds[k] == 4 {
                // the element-reading leaf shape
                self.arrs[Arr::Dense.ix()] = true;
            }
        }
        if self.cl {
            self.up = true;
        }
    }
}

fn collect(prog: &Program) -> Used {
    if !prog.trim {
        let mut u = Used::everything();
        collect_labels(&prog.body, &mut u.labels);
        // Even with nothing trimmed, these two are OFF unless a post-region use
        // asks for them: `probe`/`w` are not part of the kernel's frame and
        // declaring them unused would only add noise to a minimized case.
        u.probe = false;
        collect_post(&prog.post, &mut u);
        return u;
    }
    let mut u = Used::default();
    collect_stmts(&prog.body, &mut u);
    collect_post(&prog.post, &mut u);
    collect_labels(&prog.body, &mut u.labels);
    if let Bound::ArrLen(a) = prog.bound {
        u.arrs[a.ix()] = true;
    }
    u.close(prog);
    u
}

/// Post-region uses name bindings too, and `Escape` needs `arr2` whether the
/// body used it or not.
fn collect_post(ps: &[Post], u: &mut Used) {
    let val = |u: &mut Used, v: PostVal| match v {
        PostVal::Temp(k) => u.temps[k as usize] = true,
        PostVal::Bool(k) => u.bools[k as usize] = true,
        PostVal::Dbl(k) => u.dbls[k as usize] = true,
        PostVal::Glob(k) => u.globs[k as usize] = true,
        PostVal::Up => u.up = true,
        PostVal::Elem(a, _) => u.arrs[a.ix()] = true,
        PostVal::H => {}
    };
    for p in ps {
        match p {
            Post::TypeOf(v) | Post::Identity(v) => val(u, *v),
            Post::DblShape(k) => u.dbls[*k as usize] = true,
            Post::Probe(v) => {
                u.probe = true;
                val(u, *v);
            }
            Post::Escape(v) => {
                u.arrs[Arr::Dense2.ix()] = true;
                val(u, *v);
            }
            Post::Loop2 { vals, .. } => {
                u.loop2 = true;
                for v in vals {
                    val(u, *v);
                }
            }
        }
    }
}

fn collect_labels(ss: &[Stmt], out: &mut Vec<u32>) {
    for s in ss {
        match s {
            Stmt::Loop { label, body, .. } => {
                if let Some(l) = label {
                    out.push(*l);
                }
                collect_labels(body, out);
            }
            Stmt::If { then_, else_, .. } => {
                collect_labels(then_, out);
                collect_labels(else_, out);
            }
            Stmt::Try { body, .. } => collect_labels(body, out),
            _ => {}
        }
    }
}

fn collect_stmts(ss: &[Stmt], u: &mut Used) {
    for s in ss {
        match s {
            Stmt::Int { a, b, .. } => {
                u.src(*a);
                u.src(*b);
            }
            Stmt::BoolDef { k, a, b, .. } => {
                u.bools[*k as usize] = true;
                u.src(*a);
                u.src(*b);
            }
            Stmt::BoolUse { k, .. } => u.bools[*k as usize] = true,
            Stmt::Read { arr, idx, t, .. } => {
                u.arrs[arr.ix()] = true;
                u.idx(*idx);
                u.temps[*t as usize] = true;
            }
            Stmt::Write { arr, idx, v } => {
                u.arrs[arr.ix()] = true;
                u.idx(*idx);
                u.src(*v);
            }
            Stmt::ALen { arr } => u.arrs[arr.ix()] = true,
            Stmt::Push { v, .. } => {
                u.push_arr = true;
                u.src(*v);
            }
            Stmt::If {
                a, b, then_, else_, ..
            } => {
                u.src(*a);
                u.src(*b);
                collect_stmts(then_, u);
                collect_stmts(else_, u);
            }
            Stmt::Loop { body, .. } => collect_stmts(body, u),
            Stmt::Break { at, .. } | Stmt::Continue { at, .. } | Stmt::Ret { at } => u.src(at.src),
            Stmt::Leaf { f, a, b } => {
                u.leafs[*f as usize] = true;
                u.src(*a);
                u.src(*b);
            }
            Stmt::Deep { a } => {
                u.deep = true;
                u.src(*a);
            }
            Stmt::Closure { a } => {
                u.cl = true;
                u.src(*a);
            }
            Stmt::Indirect { a } => {
                u.fnref = true;
                u.src(*a);
            }
            Stmt::GlobRw { k, a, .. } => {
                u.globs[*k as usize] = true;
                u.src(*a);
            }
            Stmt::UpRw { a } => {
                u.up = true;
                u.src(*a);
            }
            Stmt::Dbl { k, a, .. } => {
                u.dbls[*k as usize] = true;
                u.src(*a);
            }
            Stmt::DblMix { k, .. } => u.dbls[*k as usize] = true,
            Stmt::Prop { .. } => u.objs = true,
            Stmt::Deopt { kind, at, k } => {
                u.src(at.src);
                match kind {
                    DeoptKind::TempStr | DeoptKind::TempUndef | DeoptKind::TempObj => {
                        u.temps[*k as usize] = true
                    }
                    DeoptKind::ElemDouble | DeoptKind::ElemStr | DeoptKind::ElemDelete => {
                        u.arrs[Arr::Dense.ix()] = true
                    }
                    DeoptKind::ArrShrink | DeoptKind::ArrGrow => u.arrs[Arr::Dense2.ix()] = true,
                    DeoptKind::GlobDouble => u.globs[(*k as usize) % GLOBS] = true,
                    DeoptKind::ObjExtend => u.objs = true,
                    DeoptKind::FnSwap => u.fnref = true,
                    DeoptKind::TypedOob => {
                        u.arrs[Arr::I32.ix()] = true;
                        u.temps[*k as usize] = true;
                    }
                }
            }
            Stmt::CondDef { k, at, .. } => {
                u.temps[*k as usize] = true;
                u.src(at.src);
            }
            Stmt::Rec { a } => {
                u.rec = true;
                u.src(*a);
            }
            Stmt::Sroa { a, .. } => {
                u.sroa = true;
                u.src(*a);
            }
            Stmt::Try { a, body } => {
                u.thrower = true;
                u.src(*a);
                collect_stmts(body, u);
            }
        }
    }
}

// ─────────────────────────────── emitter ───────────────────────────────

/// Name prefixes, plus the scope they belong to.
///
/// `g` prefixes every module-level binding so many programs can share one file
/// — that is how the node oracle batches them. `l` prefixes the KERNEL's own
/// names: empty under [`Scope::Kernel`], where `h`/`i`/`t0`/`b0` really are
/// function locals and cannot collide with anything, and equal to `g` under
/// [`Scope::Script`], where the kernel IS the script and its "locals" are
/// globals like every other binding.
///
/// `script` is that scope, which the statement emitter needs for one reason:
/// `return` is a SyntaxError at top level, so [`Stmt::Ret`] spells itself
/// `break` there.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    g: &'a str,
    l: &'a str,
    script: bool,
}

impl std::fmt::Display for Ctx<'_> {
    /// The GLOBAL prefix — so every `format!("{p}arr")` in this file keeps
    /// reading exactly as it did before a local prefix existed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.g)
    }
}

fn s_txt(p: &Ctx, s: Src) -> String {
    let l = p.l;
    match s {
        Src::H => format!("{l}h"),
        Src::I => format!("{l}i"),
        Src::J => format!("{l}j"),
        Src::Q => format!("{l}q"),
        Src::Hoist(k) => format!("{l}c{k}"),
        Src::Temp(k) => format!("({l}t{k} | 0)"),
        Src::Dbl(k) => format!("{l}d{k}"),
        Src::Glob(k) => format!("{p}g{k}"),
        Src::Up => format!("{p}up"),
        Src::ALen => format!("{p}arr.length"),
        Src::Lit(v) => format!("({v})"),
    }
}

fn idx_txt(p: &Ctx, i: Idx) -> String {
    match i {
        Idx::Mask(s, m) => format!("({} & {})", s_txt(p, s), m),
        Idx::Mod(s, m) => format!("({} % {})", s_txt(p, s), m),
        Idx::Raw(s) => s_txt(p, s),
        Idx::Minus(s, d) => format!("({} - {})", s_txt(p, s), d),
        Idx::Mul(s, a, m) => format!("(({} * {}) & {})", s_txt(p, s), a, m),
    }
}

fn cmp_txt(p: &Ctx, a: Src, b: Src, cmp: Cmp, neg: bool) -> String {
    let (op, nop) = match cmp {
        Cmp::Lt => ("<", ">="),
        Cmp::Le => ("<=", ">"),
        Cmp::Gt => (">", "<="),
        Cmp::Ge => (">=", "<"),
        Cmp::Eq => ("==", "!="),
        Cmp::Ne => ("!=", "=="),
        Cmp::SEq => ("===", "!=="),
        Cmp::SNe => ("!==", "==="),
    };
    let (a, b) = (s_txt(p, a), s_txt(p, b));
    // Both spellings are equivalent because no operand can be NaN — see the
    // module note. They compile to DIFFERENT ops and reach different fused
    // compare/jump paths, which is the whole reason the generator emits both.
    if neg {
        format!("!({a} {nop} {b})")
    } else {
        format!("{a} {op} {b}")
    }
}

fn cond_txt(p: &Ctx, c: Cond) -> String {
    if c.mask == 0 {
        format!("{} === {}", s_txt(p, c.src), c.val)
    } else {
        format!("({} & {}) === {}", s_txt(p, c.src), c.mask, c.val)
    }
}

fn line(o: &mut String, ind: usize, s: &str) {
    for _ in 0..ind {
        o.push(' ');
    }
    o.push_str(s);
    o.push('\n');
}

fn emit_stmt(o: &mut String, ind: usize, p: &Ctx, st: &Stmt) {
    let l = p.l;
    match st {
        Stmt::Int { op, a, b, n } => {
            let a = s_txt(p, *a);
            let b = s_txt(p, *b);
            let s = match op {
                IntOp::Add => format!("{l}h = ({l}h + {a}) | 0;"),
                IntOp::AddInt => format!("{l}h = ({l}h + {n}) | 0;"),
                IntOp::Sub => format!("{l}h = ({l}h - {a}) | 0;"),
                IntOp::Mul => format!("{l}h = ({l}h * {a}) | 0;"),
                IntOp::Imul => format!("{l}h = Math.imul({l}h, {a}) | 0;"),
                IntOp::XorShl => format!("{l}h = ({l}h ^ ({a} << {n})) | 0;"),
                IntOp::XorShr => format!("{l}h = ({l}h ^ ({a} >>> {n})) | 0;"),
                IntOp::XorSar => format!("{l}h = ({l}h ^ ({a} >> {n})) | 0;"),
                IntOp::Or => format!("{l}h = ({l}h | {a}) | 0;"),
                IntOp::And => format!("{l}h = ({l}h & {a}) | 0;"),
                IntOp::Div => format!("{l}h = ({l}h / {a}) | 0;"),
                IntOp::Mod => format!("{l}h = ({l}h % {a}) | 0;"),
                IntOp::Ternary => format!("{l}h = ({a} < {b} ? {l}h + {n} : {l}h - {n}) | 0;"),
                IntOp::Clz => format!("{l}h = ({l}h ^ Math.clz32({a})) | 0;"),
                IntOp::MinMax => {
                    format!("{l}h = ({l}h + Math.max({a}, Math.min({b}, {n}))) | 0;")
                }
                IntOp::AddRaw => format!("{l}h = {l}h + {a};"),
                IntOp::SubRaw => format!("{l}h = {l}h - {a};"),
                IntOp::IncRaw => format!("{l}h += {n};"),
            };
            line(o, ind, &s);
        }
        Stmt::BoolDef { k, a, b, cmp, neg } => {
            line(
                o,
                ind,
                &format!("{l}b{k} = {};", cmp_txt(p, *a, *b, *cmp, *neg)),
            );
        }
        Stmt::BoolUse { k, c, style } => {
            let s = match style {
                0 => format!("if ({l}b{k}) {l}h = ({l}h + {c}) | 0;"),
                1 => format!("{l}h = ({l}h + ({l}b{k} ? {c} : 1)) | 0;"),
                _ => format!("{l}h = ({l}h ^ ({l}b{k} ? {c} : 0)) | 0;"),
            };
            line(o, ind, &s);
        }
        Stmt::Read {
            arr,
            idx,
            t,
            coerce,
        } => {
            let ix = idx_txt(p, *idx);
            let load = if *arr == Arr::Str {
                format!("{l}t{t} = {p}str.charCodeAt({ix});")
            } else {
                format!("{l}t{t} = {p}{}[{ix}];", arr.name())
            };
            line(o, ind, &load);
            let s = match coerce {
                0 => format!("{l}h = ({l}h + ({l}t{t} | 0)) | 0;"),
                1 => format!("{l}h = ({l}h ^ (({l}t{t} * 1024) | 0)) | 0;"),
                2 => format!("{l}h = ({l}h + ({l}t{t} === undefined ? 17 : ({l}t{t} | 0))) | 0;"),
                _ => format!("{l}h = Math.imul({l}h ^ ({l}t{t} | 0), 16777619) | 0;"),
            };
            line(o, ind, &s);
        }
        Stmt::Write { arr, idx, v } => {
            let ix = idx_txt(p, *idx);
            let val = s_txt(p, *v);
            let s = match arr {
                Arr::Str => format!("{p}arr2[{ix}] = ({val} & 255);"),
                Arr::F64 | Arr::Dbl => format!("{p}{}[{ix}] = ({val} & 255) + 0.5;", arr.name()),
                _ => format!("{p}{}[{ix}] = ({val} & 255);", arr.name()),
            };
            line(o, ind, &s);
        }
        Stmt::ALen { arr } => {
            line(
                o,
                ind,
                &format!("{l}h = ({l}h + {p}{}.length) | 0;", arr.name()),
            );
        }
        Stmt::Push { v, style } => {
            let val = s_txt(p, *v);
            line(o, ind, &format!("{p}parr.push({val} & 255);"));
            let back = match style {
                0 => format!("{l}h = ({l}h + {p}parr.length) | 0;"),
                1 => format!("{l}h = ({l}h + {p}parr[{p}parr.length - 1]) | 0;"),
                _ => format!("{l}h = ({l}h + ({p}parr.length > 8 ? {p}parr[7] : 1)) | 0;"),
            };
            line(o, ind, &back);
        }
        Stmt::If {
            a,
            b,
            cmp,
            neg,
            then_,
            else_,
        } => {
            line(
                o,
                ind,
                &format!("if ({}) {{", cmp_txt(p, *a, *b, *cmp, *neg)),
            );
            emit_stmts(o, ind + 2, p, then_);
            if else_.is_empty() {
                line(o, ind, "}");
            } else {
                line(o, ind, "} else {");
                emit_stmts(o, ind + 2, p, else_);
                line(o, ind, "}");
            }
        }
        Stmt::Loop {
            var,
            n,
            label,
            body,
        } => {
            let lbl = label.map(|x| format!("L{x}: ")).unwrap_or_default();
            line(
                o,
                ind,
                &format!("{lbl}for ({l}{var} = 0; {l}{var} < {n}; {l}{var}++) {{"),
            );
            emit_stmts(o, ind + 2, p, body);
            line(o, ind, "}");
        }
        Stmt::Break { label, at } => {
            let lb = label.map(|x| format!(" L{x}")).unwrap_or_default();
            line(o, ind, &format!("if ({}) break{lb};", cond_txt(p, *at)));
        }
        Stmt::Continue { label, at } => {
            let lb = label.map(|x| format!(" L{x}")).unwrap_or_default();
            line(o, ind, &format!("if ({}) continue{lb};", cond_txt(p, *at)));
        }
        Stmt::Ret { at } => {
            // `return` is a SyntaxError at script scope. `break` is the nearest
            // legal spelling; it leaves the innermost loop rather than the whole
            // kernel, which is a DIFFERENT program — but a deterministic one,
            // and determinism is the only property this file needs of it.
            let s = if p.script {
                format!("if ({}) break;", cond_txt(p, *at))
            } else {
                format!("if ({}) return {l}h | 0;", cond_txt(p, *at))
            };
            line(o, ind, &s);
        }
        Stmt::Leaf { f, a, b } => {
            line(
                o,
                ind,
                &format!(
                    "{l}h = ({l}h + {p}leaf{f}({}, {})) | 0;",
                    s_txt(p, *a),
                    s_txt(p, *b)
                ),
            );
        }
        Stmt::Deep { a } => {
            line(
                o,
                ind,
                &format!("{l}h = ({l}h + {p}deep({})) | 0;", s_txt(p, *a)),
            );
        }
        Stmt::Closure { a } => {
            line(
                o,
                ind,
                &format!("{l}h = ({l}h + {p}cl({})) | 0;", s_txt(p, *a)),
            );
        }
        Stmt::Indirect { a } => {
            line(
                o,
                ind,
                &format!("{l}h = ({l}h + {p}fnref({}, 3)) | 0;", s_txt(p, *a)),
            );
        }
        Stmt::GlobRw { k, a, write } => {
            if *write {
                line(
                    o,
                    ind,
                    &format!("{p}g{k} = ({p}g{k} + {}) | 0;", s_txt(p, *a)),
                );
            }
            line(o, ind, &format!("{l}h = ({l}h ^ ({p}g{k} | 0)) | 0;"));
        }
        Stmt::UpRw { a } => {
            line(o, ind, &format!("{p}up = ({p}up + {}) | 0;", s_txt(p, *a)));
            line(o, ind, &format!("{l}h = ({l}h ^ {p}up) | 0;"));
        }
        Stmt::Dbl { k, op, a, f } => {
            let fv = ["0.5", "1.5", "0.25", "3.5", "1.0009765625", "-0.75"][*f as usize % 6];
            let a = s_txt(p, *a);
            let s = match op {
                0 => format!("{l}d{k} = {l}d{k} * 0.5 + {a};"),
                1 => format!("{l}d{k} = {a} * {fv};"),
                2 => format!("{l}d{k} = Math.sqrt(Math.abs({l}d{k})) + {fv};"),
                3 => format!("{l}d{k} = {l}d{k} / 3 + {a};"),
                _ => format!("{l}d{k} = Math.fround({l}d{k} * 0.5 + {fv});"),
            };
            line(o, ind, &s);
        }
        Stmt::DblMix { k, style } => {
            let s = match style {
                0 => format!("{l}h = ({l}h + (({l}d{k} * 1024) | 0)) | 0;"),
                1 => format!("{l}h = ({l}h ^ (Math.floor({l}d{k}) | 0)) | 0;"),
                _ => format!("{l}h = ({l}h + ({l}d{k} > 100 ? 7 : 3)) | 0;"),
            };
            line(o, ind, &s);
        }
        Stmt::Prop { k, poly, write } => {
            let sel = if *poly == 0 {
                format!("{p}objs[0]")
            } else {
                format!("{p}objs[({l}i & {poly})]")
            };
            if *write {
                line(o, ind, &format!("{sel}.f{k} = ({l}h & 63);"));
            }
            line(o, ind, &format!("{l}h = ({l}h + ({sel}.f{k} | 0)) | 0;"));
        }
        Stmt::Deopt { kind, at, k } => {
            let body = match kind {
                DeoptKind::TempStr => format!("{l}t{k} = \"sx\";"),
                DeoptKind::TempUndef => format!("{l}t{k} = undefined;"),
                DeoptKind::TempObj => format!("{l}t{k} = {p}objs[0];"),
                DeoptKind::ElemDouble => format!("{p}arr[3] = 0.5;"),
                DeoptKind::ElemStr => format!("{p}arr[5] = \"sx\";"),
                DeoptKind::ElemDelete => format!("delete {p}arr[7];"),
                DeoptKind::ArrShrink => format!("{p}arr2.length = 3;"),
                DeoptKind::ArrGrow => format!("{p}arr2.push(7);"),
                DeoptKind::GlobDouble => format!("{p}g{} = 1.5;", (*k as usize) % GLOBS),
                DeoptKind::ObjExtend => format!("{p}objs[0].zz = 4;"),
                DeoptKind::FnSwap => format!("{p}fnref = {p}leaf1;"),
                DeoptKind::TypedOob => format!("{l}t{k} = {p}iarr[9999];"),
            };
            line(o, ind, &format!("if ({}) {{ {body} }}", cond_txt(p, *at)));
        }
        Stmt::CondDef { k, at, v } => {
            line(
                o,
                ind,
                &format!("if ({}) {{ {l}t{k} = {v}; }}", cond_txt(p, *at)),
            );
        }
        Stmt::Rec { a } => {
            line(
                o,
                ind,
                &format!("{l}h = ({l}h + {p}rec((({}) & 7) + 2)) | 0;", s_txt(p, *a)),
            );
        }
        Stmt::Sroa { f, op, a } => {
            let fld = ["x", "y", "z"][*f as usize % 3];
            let txt = match op {
                0 => format!("{p}so.{fld} = ({p}so.{fld} + {}) | 0;", s_txt(p, *a)),
                1 => format!("{p}so.{fld} = ({p}so.{fld} ^ {}) | 0;", s_txt(p, *a)),
                _ => format!("{l}h = ({l}h + {p}so.{fld}) | 0;"),
            };
            line(o, ind, &txt);
        }
        Stmt::Try { a, body } => {
            line(o, ind, "try {");
            line(
                o,
                ind + 2,
                &format!("{l}h = ({l}h + {p}thrower({})) | 0;", s_txt(p, *a)),
            );
            emit_stmts(o, ind + 2, p, body);
            line(o, ind, "} catch (e) {");
            line(o, ind + 2, &format!("{l}h = ({l}h + 1) | 0;"));
            line(o, ind, "}");
        }
    }
}

fn emit_stmts(o: &mut String, ind: usize, p: &Ctx, ss: &[Stmt]) {
    for s in ss {
        emit_stmt(o, ind, p, s);
    }
}

// ───────────────────────── post-region emission ─────────────────────────

/// The value text for a post-region probe. Deliberately NOT wrapped in `| 0`:
/// the whole point of this axis is a live-out's REPRESENTATION, and `| 0` is
/// exactly the operator that erases it.
fn pv_txt(p: &Ctx, v: PostVal) -> String {
    let l = p.l;
    match v {
        PostVal::Temp(k) => format!("{l}t{k}"),
        PostVal::Bool(k) => format!("{l}b{k}"),
        PostVal::Dbl(k) => format!("{l}d{k}"),
        PostVal::H => format!("{l}h"),
        PostVal::Glob(k) => format!("{p}g{k}"),
        PostVal::Up => format!("{p}up"),
        PostVal::Elem(a, ix) => format!("{p}{}[{ix}]", a.name()),
    }
}

/// Fold `v` into `h` as a NUMBER — ordinary traffic, so the second region
/// carries real work as well as probes.
fn pv_num(p: &Ctx, v: PostVal) -> String {
    let t = pv_txt(p, v);
    match v {
        PostVal::Bool(_) => format!("({t} ? 3 : 5)"),
        PostVal::Dbl(_) => format!("(({t} * 4) | 0)"),
        _ => format!("({t} | 0)"),
    }
}

/// `typeof x` as a small int. Exhaustive over the six results `typeof` can
/// give for a value these programs can hold, so the last arm is unreachable
/// rather than a catch-all bucket.
fn typeof_code(x: &str) -> String {
    format!(
        "(typeof {x} === \"number\" ? 2 : typeof {x} === \"boolean\" ? 3 : typeof {x} === \"string\" ? 5 : typeof {x} === \"undefined\" ? 7 : typeof {x} === \"object\" ? 11 : 13)"
    )
}

fn emit_post(o: &mut String, ind: usize, p: &Ctx, ps: &[Post]) {
    let l = p.l;
    for st in ps {
        match st {
            Post::TypeOf(v) => {
                let x = pv_txt(p, *v);
                line(o, ind, &format!("{l}h = ({l}h + {}) | 0;", typeof_code(&x)));
            }
            Post::Identity(v) => {
                let x = pv_txt(p, *v);
                line(
                    o,
                    ind,
                    &format!(
                        "{l}h = ({l}h + ({x} === true ? 3 : {x} === false ? 5 : {x} === 0 ? 7 : {x} === undefined ? 11 : 13)) | 0;"
                    ),
                );
            }
            Post::DblShape(k) => {
                // The two f64 facts `| 0` throws away: NaN-ness, and the sign of
                // zero. A raw f64 home that reached the frame slot uncooked
                // shows up in the first; a `-0` a compiled body normalised to
                // `0` shows up in the second.
                line(
                    o,
                    ind,
                    &format!(
                        "{l}h = ({l}h + ({l}d{k} === {l}d{k} ? 1 : 2) + ((1 / {l}d{k}) < 0 ? 4 : 8)) | 0;"
                    ),
                );
            }
            Post::Probe(v) => {
                line(
                    o,
                    ind,
                    &format!("{l}h = ({l}h + {p}probe({})) | 0;", pv_txt(p, *v)),
                );
            }
            Post::Escape(v) => {
                let x = pv_txt(p, *v);
                line(o, ind, &format!("{p}arr2[5] = {x};"));
                line(
                    o,
                    ind,
                    &format!("{l}h = ({l}h + ({p}arr2[5] === {x} ? 1 : 2)) | 0;"),
                );
            }
            Post::Loop2 { n, vals } => {
                // A SECOND compiled region whose live-INS are the first
                // region's live-OUTs. Nothing else in this file asks a value to
                // survive from one region into another.
                line(o, ind, &format!("for ({l}w = 0; {l}w < {n}; {l}w++) {{"));
                let mut terms = vec![format!("{l}h")];
                for v in vals {
                    terms.push(pv_num(p, *v));
                }
                line(o, ind + 2, &format!("{l}h = ({}) | 0;", terms.join(" + ")));
                line(o, ind, "}");
            }
        }
    }
}

const STR_LIT: &str = "the quick brown fox jumps over the lazy dog 0123456789 ABCDEF";

fn int_list(n: usize, f: impl Fn(usize) -> i64) -> String {
    (0..n)
        .map(|i| f(i).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn emit_leaf(o: &mut String, p: &Ctx, k: usize, kind: u8) {
    let body = match kind {
        0 => "return (a + b * 3) | 0;".to_string(),
        1 => "return Math.imul(a ^ b, 5) | 0;".to_string(),
        2 => "var s = 0; for (var w = 0; w < 3; w++) s = (s + a + w) | 0; return (s ^ b) | 0;"
            .to_string(),
        3 => "return (a > b ? a - b : b - a) | 0;".to_string(),
        _ => format!("return {p}arr[(a ^ b) & 31] | 0;"),
    };
    line(o, 0, &format!("function {p}leaf{k}(a, b) {{ {body} }}"));
}

/// Emit the whole program. `gp` prefixes every module-level binding so many
/// programs can share one file (that is how the node oracle batches them);
/// under [`Scope::Script`] it prefixes the kernel's own names too, because
/// there they ARE module-level bindings.
///
/// `iife`: wrap a script-scope program in `(function () { … })()`. For the NODE
/// oracle only, and value-preserving by construction — every binding a program
/// touches is its own, so function scope and script scope compute the same
/// number. It exists because the scope axis is aimed squarely at zipp's
/// REGISTER ALLOCATOR (the bytecode compiler recycles a statement's temps at
/// script scope and does not inside a function, which is the whole reason B94
/// split receivers live there), and node's answer cannot depend on that.
fn emit(prog: &Program, gp: &str, tag: &str, iife: bool) -> String {
    let script = prog.scope == Scope::Script;
    let p = &Ctx {
        g: gp,
        l: if script { gp } else { "" },
        script,
    };
    let l = p.l;
    let u = collect(prog);
    let mut o = String::new();

    if u.push_arr {
        // Starts EMPTY on purpose: an empty dense array samples as all-Int, so
        // the pin planner offers it to the INT tier, and the kernel then walks
        // it through every `Vec` capacity doubling from zero.
        line(&mut o, 0, &format!("var {p}parr = [];"));
    }
    if u.arrs[Arr::Dense.ix()] {
        // Small values on purpose: `A[i] === 1` has to MATCH sometimes.
        line(
            &mut o,
            0,
            &format!("var {p}arr = [{}];", int_list(32, |i| (i as i64 * 3) % 7)),
        );
    }
    if u.arrs[Arr::Dense2.ix()] {
        line(
            &mut o,
            0,
            &format!("var {p}arr2 = [{}];", int_list(32, |i| (i as i64 * 5) % 11)),
        );
    }
    if u.arrs[Arr::F64.ix()] {
        line(
            &mut o,
            0,
            &format!(
                "var {p}farr = new Float64Array([{}]);",
                (0..32)
                    .map(|i| format!("{}.5", i * 2))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    if u.arrs[Arr::I32.ix()] {
        line(
            &mut o,
            0,
            &format!(
                "var {p}iarr = new Int32Array([{}]);",
                int_list(32, |i| if i % 4 == 3 {
                    (i as i64 * 1103515245) as i32 as i64
                } else {
                    (i as i64 * 5) % 9
                })
            ),
        );
    }
    if u.arrs[Arr::U8.ix()] {
        line(
            &mut o,
            0,
            &format!(
                "var {p}uarr = new Uint8Array([{}]);",
                int_list(32, |i| (i as i64 * 11) % 13)
            ),
        );
    }
    if u.arrs[Arr::Holey.ix()] {
        line(
            &mut o,
            0,
            &format!("var {p}harr = [1, 2, 3, 4]; {p}harr[12] = 9;"),
        );
    }
    if u.arrs[Arr::Dbl.ix()] {
        line(
            &mut o,
            0,
            &format!(
                "var {p}darr = [{}];",
                (0..32)
                    .map(|i| format!("{}.25", i * 3))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    if u.arrs[Arr::Str.ix()] {
        line(&mut o, 0, &format!("var {p}str = \"{STR_LIT}\";"));
    }
    if u.objs {
        // Sixteen distinct shapes: enough to cycle a JIT inline cache past its
        // eight ways, which is the condition the ICGATE switch governs.
        let mut shapes = Vec::new();
        for s in 0..16 {
            let mut fields = vec![format!("f0: {}", s * 2 + 1)];
            if s % 2 == 0 {
                fields.push(format!("f1: {}", s + 3));
            }
            if s % 3 == 0 {
                fields.insert(0, format!("k{s}: {s}"));
            }
            fields.push(format!("f2: {}", s * 5));
            if s % 4 == 1 {
                fields.push(format!("m{s}: {}", s));
            }
            shapes.push(format!("{{ {} }}", fields.join(", ")));
        }
        line(
            &mut o,
            0,
            &format!("var {p}objs = [{}];", shapes.join(", ")),
        );
    }
    for (k, used) in u.globs.iter().enumerate() {
        if *used {
            line(
                &mut o,
                0,
                &format!("var {p}g{k} = {};", (k as i64 + 1) * 11),
            );
        }
    }
    for k in 0..LEAFS {
        if u.leafs[k] {
            emit_leaf(&mut o, p, k, prog.leaf_kinds[k]);
        }
    }
    if u.deep {
        line(
            &mut o,
            0,
            &format!("function {p}deep(a) {{ return ({p}leaf0(a, 2) + {p}leaf1(a, 3)) | 0; }}"),
        );
    }
    if u.fnref {
        line(&mut o, 0, &format!("var {p}fnref = {p}leaf0;"));
    }
    if u.sroa {
        // A GLOBAL object, not a local: `plan_field_promotion` takes the globals
        // table, so a kernel-local literal is not an SROA candidate at all and a
        // generator that used one would never reach that tier.
        line(&mut o, 0, &format!("var {p}so = {{ x: 1, y: 2, z: 3 }};"));
    }
    if u.rec {
        // NOT one `| 0` in here, on purpose. Tier A (`fn_int::can_compile`)
        // admits LoadInt / Move / AddInt / Add / Sub / Mul / Mod / compares /
        // jumps / Return and the self-call, and NOTHING else — a single
        // `Bitwise` op puts the whole function on Tier C instead. The `| 0`
        // spelling this shape used to carry is why `[jit] Tier A … compiled`
        // never appeared for a generated program, and `tier_a_is_reached` now
        // pins that it does. Still exact: the call site masks `x` into [2, 9],
        // so every value here is a small integer.
        line(
            &mut o,
            0,
            &format!(
                "function {p}rec(x) {{ if (x < 2) return x; return {p}rec(x - 1) + {p}rec(x - 2); }}"
            ),
        );
    }
    if u.probe {
        line(
            &mut o,
            0,
            &format!(
                "function {p}probe(x) {{ return {} | 0; }}",
                typeof_code("x")
            ),
        );
    }
    if u.thrower {
        line(
            &mut o,
            0,
            &format!(
                "function {p}thrower(x) {{ if ((x & 15) === 7) throw new RangeError(\"t\"); return x | 0; }}"
            ),
        );
    }

    // ── the kernel's own bindings ── declared once; re-initialised per
    // repetition, which under `Scope::Kernel` is what a fresh call frame does
    // for free and under `Scope::Script` has to be written out.
    let mut decls = String::new();
    let mut init = String::new();
    {
        let d = &mut decls;
        let z = &mut init;
        line(
            d,
            0,
            &format!("var {l}h = 1, {l}i = 0, {l}j = 0, {l}q = 0;"),
        );
        line(z, 0, &format!("{l}h = 1; {l}i = 0; {l}j = 0; {l}q = 0;"));
        if u.loop2 {
            line(d, 0, &format!("var {l}w = 0;"));
        }
        for k in 0..TEMPS {
            if u.temps[k] {
                line(d, 0, &format!("var {l}t{k} = {};", k + 1));
                line(z, 0, &format!("{l}t{k} = {};", k + 1));
            }
        }
        for k in 0..BOOLS {
            if u.bools[k] {
                line(d, 0, &format!("var {l}b{k} = false;"));
                line(z, 0, &format!("{l}b{k} = false;"));
            }
        }
        for k in 0..DBLS {
            if u.dbls[k] {
                line(d, 0, &format!("var {l}d{k} = {}.5;", k + 1));
                line(z, 0, &format!("{l}d{k} = {}.5;", k + 1));
            }
        }
        for k in 0..HOISTS {
            if u.hoists[k] {
                line(d, 0, &format!("var {l}c{k} = {};", prog.hoists[k]));
            }
        }
    }

    // ── the kernel body: the hot loop, then the post-region uses ──
    let bound_txt = match prog.bound {
        Bound::N => {
            if script {
                prog.n.to_string()
            } else {
                "n".to_string()
            }
        }
        Bound::ArrLen(a) => format!("{p}{}.length", a.name()),
    };
    let mut body = String::new();
    line(
        &mut body,
        0,
        &format!("for ({l}i = 0; {l}i < {bound_txt}; {l}i++) {{"),
    );
    emit_stmts(&mut body, 2, p, &prog.body);
    line(&mut body, 0, "}");
    emit_post(&mut body, 0, p, &prog.post);

    // Mix every live local into the answer, so a wrong value anywhere in the
    // frame is visible and not just a wrong `h`.
    let dead = |v: PostVal| prog.dead_out == Some(v);
    let mut mix = vec![format!("{l}h")];
    for k in 0..TEMPS {
        if u.temps[k] && !dead(PostVal::Temp(k as u8)) {
            mix.push(format!("(({l}t{k} | 0) * {})", 3 + k));
            // …and its REPRESENTATION, not only its int32 value. W16's
            // `open_bool_local_reads_back_as_nan` is why: a Bool home that comes
            // back as a raw `NaN` is FALSY, so the `(b ? 17 : 0)` term below
            // reads it as an ordinary `false` and the digest never moves.
            // `typeof` is the only spelling that separates them, and it is
            // exactly specified for every value these programs can hold.
            mix.push(format!(
                "({} * {})",
                typeof_code(&format!("{l}t{k}")),
                64 + k
            ));
        }
    }
    for k in 0..BOOLS {
        if u.bools[k] && !dead(PostVal::Bool(k as u8)) {
            mix.push(format!("({l}b{k} ? {} : 0)", 17 + k));
            mix.push(format!(
                "(typeof {l}b{k} === \"boolean\" ? 0 : {})",
                1024 + k
            ));
        }
    }
    for k in 0..DBLS {
        if u.dbls[k] && !dead(PostVal::Dbl(k as u8)) {
            mix.push(format!("(({l}d{k} * 1024) | 0)"));
            mix.push(format!("({l}d{k} === {l}d{k} ? 0 : {})", 2048 + k));
            mix.push(format!("((1 / {l}d{k}) < 0 ? {} : 0)", 4096 + k));
        }
    }
    if u.sroa {
        mix.push(format!("({p}so.x | 0)"));
        mix.push(format!("({p}so.y | 0)"));
        mix.push(format!("({p}so.z | 0)"));
    }
    let mix = format!("({}) | 0", mix.join(" ^ "));

    // ── the checksum over every mutable datum the body could have touched:
    // side effects that never reach `h` still have to agree across tiers ──
    let mut tail = vec![format!("{l}acc")];
    let mut checksum = String::new();
    if prog.checksum {
        tail.push(format!("{l}s"));
        line(&mut checksum, 0, &format!("var {l}s = 0;"));
        for (zi, a) in Arr::all().into_iter().enumerate() {
            if !u.arrs[a.ix()] || a == Arr::Str {
                continue;
            }
            line(
                &mut checksum,
                0,
                &format!(
                    "for (var {l}z{zi} = 0; {l}z{zi} < {p}{n}.length; {l}z{zi}++) {l}s = (Math.imul({l}s, 31) + (({p}{n}[{l}z{zi}] * 1024) | 0)) | 0;",
                    n = a.name()
                ),
            );
        }
        if u.objs {
            line(
                &mut checksum,
                0,
                &format!(
                    "for (var {l}y = 0; {l}y < {p}objs.length; {l}y++) {l}s = (Math.imul({l}s, 31) + ({p}objs[{l}y].f0 | 0) + ({p}objs[{l}y].f1 | 0) + ({p}objs[{l}y].f2 | 0)) | 0;"
                ),
            );
        }
    }
    for k in 0..GLOBS {
        if u.globs[k] {
            tail.push(format!("({p}g{k} | 0)"));
        }
    }
    if u.up {
        tail.push(format!("({p}up | 0)"));
    }
    let tail = tail.join(" ^ ");

    if script {
        // ── SCRIPT SCOPE ── the kernel IS the script, and its bindings are
        // globals. That is a different register-allocation regime end to end:
        // the bytecode compiler recycles a statement's temps here and does not
        // inside a function, and a recycled PINNED RECEIVER is precisely what
        // B94 splitting exists for. No `"use strict"` (a directive prologue has
        // to be the first statement of the file, and the data declarations are)
        // and no closure nesting.
        let mut inner = String::new();
        if u.up {
            line(&mut inner, 0, &format!("var {p}up = 1;"));
        }
        if u.cl {
            line(
                &mut inner,
                0,
                &format!("function {p}cl(x) {{ {p}up = ({p}up + x) | 0; return {p}up & 1023; }}"),
            );
        }
        inner.push_str(&decls);
        line(&mut inner, 0, &format!("var {l}acc = 1;"));
        line(
            &mut inner,
            0,
            &format!("for (var {l}r = 0; {l}r < {}; {l}r++) {{", prog.reps),
        );
        for ln in init.lines() {
            line(&mut inner, 2, ln);
        }
        for ln in body.lines() {
            line(&mut inner, 2, ln);
        }
        line(
            &mut inner,
            2,
            &format!("{l}acc = (Math.imul({l}acc, 31) + ({mix})) | 0;"),
        );
        line(&mut inner, 0, "}");
        inner.push_str(&checksum);
        line(
            &mut inner,
            0,
            &format!(
                "try {{ console.log(\"{tag} \" + (({tail}) >>> 0).toString(16)); }} catch (e) {{ console.log(\"{tag} E:\" + (e && e.constructor ? e.constructor.name : \"?\")); }}"
            ),
        );
        if iife {
            line(&mut o, 0, "(function () {");
            for ln in inner.lines() {
                line(&mut o, 2, ln);
            }
            line(&mut o, 0, "})();");
        } else {
            o.push_str(&inner);
        }
        return o;
    }

    // ── FUNCTION SCOPE ── up / cl / kernel: module level, or nested inside main
    // (upvalues).
    let nested = prog.use_closure;
    let ind = if nested { 2 } else { 0 };
    let mut inner = String::new();
    if u.up {
        line(&mut inner, ind, &format!("var {p}up = 1;"));
    }
    if u.cl {
        line(
            &mut inner,
            ind,
            &format!("function {p}cl(x) {{ {p}up = ({p}up + x) | 0; return {p}up & 1023; }}"),
        );
    }
    line(&mut inner, ind, &format!("function {p}kernel(n) {{"));
    if prog.strict {
        line(&mut inner, ind + 2, "\"use strict\";");
    }
    for ln in decls.lines() {
        line(&mut inner, ind + 2, ln);
    }
    for ln in body.lines() {
        line(&mut inner, ind + 2, ln);
    }
    line(&mut inner, ind + 2, &format!("return {mix};"));
    line(&mut inner, ind, "}");

    // ── main ──
    line(&mut o, 0, &format!("function {p}main() {{"));
    if nested {
        o.push_str(&inner);
    }
    line(&mut o, 2, "var acc = 1;");
    line(
        &mut o,
        2,
        &format!(
            "for (var r = 0; r < {}; r++) acc = (Math.imul(acc, 31) + ({p}kernel({}) | 0)) | 0;",
            prog.reps, prog.n
        ),
    );
    for ln in checksum.lines() {
        line(&mut o, 2, ln);
    }
    line(&mut o, 2, &format!("return ({tail}) | 0;"));
    line(&mut o, 0, "}");
    if !nested {
        o.push_str(&inner);
    }

    line(
        &mut o,
        0,
        &format!(
            "try {{ console.log(\"{tag} \" + ({p}main() >>> 0).toString(16)); }} catch (e) {{ console.log(\"{tag} E:\" + (e && e.constructor ? e.constructor.name : \"?\")); }}"
        ),
    );
    o
}

// ─────────────────────────── child process protocol ───────────────────────────

/// The worker. A no-op in a normal `cargo test` run; the parent re-execs this
/// binary with `ZIPP_FUZZ_JOB` set, because every mode switch is a memoized
/// `AtomicU8` latch and therefore a property of a PROCESS, not of a call.
#[test]
fn fuzz_child() {
    let job = match std::env::var("ZIPP_FUZZ_JOB") {
        Ok(j) => j,
        Err(_) => return,
    };
    // Split on the FIRST colon only: a `file:` job carries a Windows path whose
    // drive letter is a colon of its own.
    let (kind, rest) = job.split_once(':').expect("ZIPP_FUZZ_JOB is kind:args");
    match kind {
        "batch" => {
            let mut parts = rest.split(':');
            let seed: u64 = parts.next().unwrap().parse().unwrap();
            let lo: u64 = parts.next().unwrap().parse().unwrap();
            let hi: u64 = parts.next().unwrap().parse().unwrap();
            let big = std::env::var_os("ZIPP_FUZZ_BIG").is_some();
            println!("<<<FUZZ");
            for i in lo..hi {
                let prog = gen_program(prog_seed(seed, i), big);
                let src = emit(&prog, "", &format!("D{i}"), false);
                println!("{}", run_digest(&src, &format!("D{i}")));
            }
            println!("FUZZ>>>");
        }
        "dump" => {
            // `ZIPP_FUZZ_JOB=dump:<seed>:<index>` prints the program a seed/index
            // pair denotes, which is how a soak finding is replayed by hand.
            let mut parts = rest.split(':');
            let seed: u64 = parts.next().unwrap().parse().unwrap();
            let i: u64 = parts.next().unwrap().parse().unwrap();
            let big = std::env::var_os("ZIPP_FUZZ_BIG").is_some();
            let prog = gen_program(prog_seed(seed, i), big);
            println!("<<<FUZZ");
            print!("{}", emit(&prog, "", "D", false));
            println!("FUZZ>>>");
        }
        "file" => {
            let src = std::fs::read_to_string(rest).expect("job source file");
            println!("<<<FUZZ");
            println!("{}", run_digest(&src, "D"));
            println!("FUZZ>>>");
        }
        other => panic!("unknown ZIPP_FUZZ_JOB kind {other}"),
    }
}

fn prog_seed(seed: u64, i: u64) -> u64 {
    seed ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(17)
}

/// Run one program and reduce it to a single comparable token.
fn run_digest(src: &str, tag: &str) -> String {
    match zipp_vm::run(src) {
        Err(_) => format!("{tag} COMPILE-ERR"),
        Ok(o) => {
            if let Some(_e) = o.error {
                // An UNCAUGHT throw escaped `main`'s own try/catch, i.e. it came
                // out of the console.log line or the engine itself. Message text
                // is not portable; the fact is.
                format!("{tag} UNCAUGHT")
            } else {
                o.output
                    .iter()
                    .find(|l| l.starts_with(tag))
                    .cloned()
                    .unwrap_or_else(|| format!("{tag} NO-OUTPUT({})", o.output.len()))
            }
        }
    }
}

// ───────────────────────────── process plumbing ─────────────────────────────

struct ChildOut {
    lines: Vec<String>,
    ok: bool,
    stderr: String,
}

fn spawn_job(job: &str, m: &Mode, timeout: Duration, jitlog: bool) -> ChildOut {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = Command::new(exe);
    cmd.args([
        "fuzz_child",
        "--exact",
        "--nocapture",
        "--test-threads",
        "1",
    ])
    .env("ZIPP_FUZZ_JOB", job)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    // Never inherit an outer mode; the parent may itself be running under one.
    for m2 in MODES {
        for (k, _) in m2.env {
            cmd.env_remove(k);
        }
    }
    cmd.env_remove("ZIPP_JITLOG");
    cmd.env_remove("ZIPP_JITDECLINE");
    for (k, v) in m.env {
        cmd.env(k, v);
    }
    if jitlog {
        cmd.env("ZIPP_JITLOG", "1");
        // The two are one diagnostic: JITLOG says which tier took a region,
        // JITDECLINE says why the faster ones did not.
        cmd.env("ZIPP_JITDECLINE", "1");
    }
    if std::env::var_os("ZIPP_FUZZ_BIG").is_some() {
        cmd.env("ZIPP_FUZZ_BIG", "1");
    }
    let mut child = cmd.spawn().expect("spawn the test binary");
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let (tx1, rx1) = mpsc::channel();
    let (tx2, rx2) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        let _ = tx1.send(s);
    });
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        let _ = tx2.send(s);
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => break None,
        }
    };
    let out = rx1
        .recv_timeout(Duration::from_secs(20))
        .unwrap_or_default();
    let err = rx2
        .recv_timeout(Duration::from_secs(20))
        .unwrap_or_default();
    // libtest prints `test fuzz_child ... ` with no newline before the body runs,
    // so the opening marker shares a line with it.
    let inside = out
        .lines()
        .skip_while(|l| !l.contains("<<<FUZZ"))
        .skip(1)
        .take_while(|l| !l.contains("FUZZ>>>"))
        .map(|l| l.trim_end().to_string())
        .collect::<Vec<_>>();
    let closed = out.contains("FUZZ>>>");
    ChildOut {
        lines: inside,
        ok: status.map(|s| s.success()).unwrap_or(false) && closed,
        stderr: err,
    }
}

/// `idx -> digest` for one mode over `[lo, hi)`. A child that dies takes its
/// whole chunk with it, so a failed chunk is re-run one program at a time —
/// which is also how a CRASH gets attributed to a single program.
fn batch_digests(seed: u64, lo: u64, hi: u64, m: &Mode, big: bool) -> BTreeMap<u64, String> {
    let secs = 30 + (hi - lo) / 4;
    let out = spawn_job(
        &format!("batch:{seed}:{lo}:{hi}"),
        m,
        Duration::from_secs(secs),
        false,
    );
    let mut map = BTreeMap::new();
    if out.ok {
        for l in &out.lines {
            if let Some((k, v)) = l.split_once(' ') {
                if let Some(i) = k.strip_prefix('D').and_then(|s| s.parse::<u64>().ok()) {
                    map.insert(i, v.to_string());
                }
            }
        }
    }
    if map.len() as u64 == hi - lo {
        return map;
    }
    // Fall back to one process per program for the whole chunk.
    map.clear();
    for i in lo..hi {
        let prog = gen_program(prog_seed(seed, i), big);
        let src = emit(&prog, "", "D", false);
        map.insert(i, single_digest(&src, m));
    }
    map
}

fn single_digest(src: &str, m: &Mode) -> String {
    let dir = std::env::temp_dir().join("zipp-jit-fuzz");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("case-{}-{:x}.js", std::process::id(), fnv(src)));
    std::fs::write(&path, src).expect("write case");
    let out = spawn_job(
        &format!("file:{}", path.display()),
        m,
        Duration::from_secs(60),
        false,
    );
    let _ = std::fs::remove_file(&path);
    if !out.ok {
        let why = out
            .stderr
            .lines()
            .find(|l| l.contains("panicked") || l.contains("error"))
            .unwrap_or("")
            .trim()
            .chars()
            .take(120)
            .collect::<String>();
        return format!("CRASH-OR-HANG[{why}]");
    }
    out.lines
        .iter()
        .find(|l| l.starts_with("D "))
        .map(|l| l[2..].to_string())
        .unwrap_or_else(|| "NO-DIGEST".into())
}

/// The JIT's own decisions for `src`, collapsed: repeated deopts at the same ip
/// become one line with a count, and everything else keeps its first form.
fn tier_trace(src: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join("zipp-jit-fuzz");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("trace-{}-{:x}.js", std::process::id(), fnv(src)));
    if std::fs::write(&path, src).is_err() {
        return Vec::new();
    }
    let out = spawn_job(
        &format!("file:{}", path.display()),
        mode("base"),
        Duration::from_secs(60),
        true,
    );
    let _ = std::fs::remove_file(&path);
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for l in out.stderr.lines() {
        let l = l.trim();
        // `[decline-reason]` carries its own `fn=<name> [start,end]` prefix as of
        // W17, so a decline in this trace names the region it belongs to and can
        // be read beside the `[jit]` line for the tier that took it instead.
        if !l.starts_with("[jit]") && !l.starts_with("[leaf]") && !l.starts_with("[decline-reason]")
        {
            continue;
        }
        let key = match l.find("deopt at ip") {
            Some(i) => format!("{} deopt at ip …", &l[..i]),
            None => l.to_string(),
        };
        if seen.insert(key.clone(), 0).is_none() {
            order.push(key.clone());
        }
        *seen.get_mut(&key).unwrap() += 1;
    }
    order
        .into_iter()
        .take(18)
        .map(|k| {
            let n = seen[&k];
            if n > 1 {
                format!("{n}x {k}")
            } else {
                k
            }
        })
        .collect()
}

fn fnv(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

// ──────────────────────────────── shrinker ────────────────────────────────

/// Greedy delta-debugging over the generator's own IR: delete a statement, halve
/// a loop bound, drop a repetition, then trim every declaration the survivor no
/// longer mentions. Each candidate is verified by re-running it in the two modes
/// that disagreed, so a shrink that loses the divergence is rejected.
fn shrink(prog: &Program, check: &mut impl FnMut(&Program) -> bool) -> Program {
    let mut best = prog.clone();
    for _round in 0..6 {
        let mut changed = false;

        // 1. statements, deepest-last so an outer delete subsumes inner ones
        loop {
            let n = count_stmts(&best.body);
            let mut removed = false;
            for k in (0..n).rev() {
                let mut cand = best.clone();
                let mut idx = 0usize;
                remove_nth(&mut cand.body, k, &mut idx);
                if count_stmts(&cand.body) == n {
                    continue;
                }
                if cand.body.is_empty() {
                    continue;
                }
                if check(&cand) {
                    best = cand;
                    removed = true;
                    changed = true;
                    break;
                }
            }
            if !removed {
                break;
            }
        }

        // 2. inline an `if` (keep the taken branch's statements, drop the test)
        loop {
            let n = count_stmts(&best.body);
            let mut did = false;
            for k in 0..n {
                let mut cand = best.clone();
                let mut idx = 0usize;
                if !unwrap_if_nth(&mut cand.body, k, &mut idx) {
                    continue;
                }
                if check(&cand) {
                    best = cand;
                    did = true;
                    changed = true;
                    break;
                }
            }
            if !did {
                break;
            }
        }

        // 3. post-region uses, deepest-last
        loop {
            let n = best.post.len();
            let mut removed = false;
            for k in (0..n).rev() {
                let mut cand = best.clone();
                cand.post.remove(k);
                if check(&cand) {
                    best = cand;
                    removed = true;
                    changed = true;
                    break;
                }
            }
            if !removed {
                break;
            }
        }

        // 4. loop bounds
        for &nn in &[400u32, 120, 40, 20, 12, 9] {
            if nn < best.n {
                let mut cand = best.clone();
                cand.n = nn;
                if check(&cand) {
                    best = cand;
                    changed = true;
                }
            }
        }
        for &rr in &[12u32, 3, 1] {
            if rr < best.reps {
                let mut cand = best.clone();
                cand.reps = rr;
                if check(&cand) {
                    best = cand;
                    changed = true;
                }
            }
        }
        {
            let mut cand = best.clone();
            if reduce_inner_bounds(&mut cand.body) && check(&cand) {
                best = cand;
                changed = true;
            }
        }

        // 5. structural knobs
        if !matches!(best.bound, Bound::N) {
            let mut cand = best.clone();
            cand.bound = Bound::N;
            if check(&cand) {
                best = cand;
                changed = true;
            }
        }
        if best.checksum {
            let mut cand = best.clone();
            cand.checksum = false;
            if check(&cand) {
                best = cand;
                changed = true;
            }
        }
        if best.strict {
            let mut cand = best.clone();
            cand.strict = false;
            if check(&cand) {
                best = cand;
                changed = true;
            }
        }
        if best.use_closure {
            let mut cand = best.clone();
            cand.use_closure = false;
            if check(&cand) {
                best = cand;
                changed = true;
            }
        }
        if !best.trim {
            let mut cand = best.clone();
            cand.trim = true;
            if check(&cand) {
                best = cand;
                changed = true;
            }
        }
        if best.dead_out.is_some() {
            let mut cand = best.clone();
            cand.dead_out = None;
            if check(&cand) {
                best = cand;
                changed = true;
            }
        }
        // A script-scope program reads worse than a function-scope one and its
        // whole point is a DIFFERENT register allocation, so try the function
        // spelling — but only accept it if the divergence survives, which for a
        // split-receiver finding it will not.
        if best.scope == Scope::Script {
            let mut cand = best.clone();
            cand.scope = Scope::Kernel;
            if check(&cand) {
                best = cand;
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
    best
}

fn count_stmts(ss: &[Stmt]) -> usize {
    let mut n = 0;
    for s in ss {
        n += 1;
        match s {
            Stmt::If { then_, else_, .. } => {
                n += count_stmts(then_) + count_stmts(else_);
            }
            Stmt::Loop { body, .. } | Stmt::Try { body, .. } => n += count_stmts(body),
            _ => {}
        }
    }
    n
}

/// Remove the `k`th statement in pre-order. `idx` is the running counter.
fn remove_nth(ss: &mut Vec<Stmt>, k: usize, idx: &mut usize) -> bool {
    let mut i = 0;
    while i < ss.len() {
        if *idx == k {
            ss.remove(i);
            return true;
        }
        *idx += 1;
        let done = match &mut ss[i] {
            Stmt::If { then_, else_, .. } => remove_nth(then_, k, idx) || remove_nth(else_, k, idx),
            Stmt::Loop { body, .. } | Stmt::Try { body, .. } => remove_nth(body, k, idx),
            _ => false,
        };
        if done {
            return true;
        }
        i += 1;
    }
    false
}

/// Replace the `k`th statement with the contents of its `then_` branch, when it
/// is an `If`. Legal because nothing moves between loop scopes.
fn unwrap_if_nth(ss: &mut Vec<Stmt>, k: usize, idx: &mut usize) -> bool {
    let mut i = 0;
    while i < ss.len() {
        if *idx == k {
            if let Stmt::If { then_, .. } = ss[i].clone() {
                ss.splice(i..=i, then_);
                return true;
            }
            return false;
        }
        *idx += 1;
        let done = match &mut ss[i] {
            Stmt::If { then_, else_, .. } => {
                unwrap_if_nth(then_, k, idx) || unwrap_if_nth(else_, k, idx)
            }
            Stmt::Loop { body, .. } | Stmt::Try { body, .. } => unwrap_if_nth(body, k, idx),
            _ => false,
        };
        if done {
            return true;
        }
        i += 1;
    }
    false
}

fn reduce_inner_bounds(ss: &mut [Stmt]) -> bool {
    let mut any = false;
    for s in ss {
        match s {
            Stmt::Loop { n, body, .. } => {
                if *n > 2 {
                    *n = 2;
                    any = true;
                }
                any |= reduce_inner_bounds(body);
            }
            Stmt::If { then_, else_, .. } => {
                any |= reduce_inner_bounds(then_);
                any |= reduce_inner_bounds(else_);
            }
            Stmt::Try { body, .. } => any |= reduce_inner_bounds(body),
            _ => {}
        }
    }
    any
}

// ──────────────────────────── divergence reporting ────────────────────────────

/// How many times ONE mode is re-run on ONE source to decide whether the
/// program agrees with ITSELF.
///
/// This whole instrument compares digests ACROSS modes, so it is only ever as
/// sound as the assumption underneath it: that one mode answers the same thing
/// twice. W18 met two generated programs that did not — `s3127_i361` and
/// `s3129_i318` of the W17 gate soak alternated `D 7840` / `D b08f` over runs
/// of ONE binary, on the committed baseline as much as on the wave tree. Both
/// were the [`open_conditional_def_loses_its_entry_load`] defect wearing its
/// worst face: the home the compiled body read was never filled, so the answer
/// was whatever the previous phase had left in that register.
///
/// An unlabelled flake costs a gate twice. It is reported as a tier divergence
/// it is not, and — the expensive half — a REAL divergence found beside it can
/// be waved off as "that flaky one again". W17's gate hand-triaged 149
/// divergences to find these two. So every candidate is classified BEFORE it is
/// shrunk or reported, and a flaky one is shrunk on self-disagreement instead
/// of on a cross-mode comparison that means nothing for it.
///
/// A coin-flip flake survives R runs of one mode with probability 2^-(R-1), and
/// the classifier spends R on EACH of the two modes that disagreed, so R = 6
/// leaves 2^-10. Measured against the real thing: the two programs above were
/// caught 2/2 at R = 16 and 1/2 at R = 8, which is why R is not 4. The cost on
/// a green run is zero — nothing is re-run until something has already
/// diverged.
const SELF_RUNS: usize = 6;

/// The same predicate inside the shrinker, where every candidate pays it.
/// Lower because a miss there is cheap and one-directional: it costs a shrink
/// STEP (the greedy loop keeps the bigger program), never a wrong verdict — a
/// step is accepted only when self-disagreement was actually OBSERVED.
const SELF_RUNS_SHRINK: usize = 4;

/// Bound on shrink candidates for a nondeterministic finding. Far below the
/// cross-mode shrink's 900 because each candidate costs [`SELF_RUNS_SHRINK`]
/// processes instead of two, and because a flaky predicate shrinks less per
/// round anyway.
const SELF_SHRINK_EVALS: usize = 150;

/// What re-running the SAME source in the SAME mode proved about a finding.
///
/// The three are different bugs with different owners, and telling them apart
/// is the difference between a gate that reports and a gate that guesses.
enum Flake {
    /// Every mode that was re-run agreed with itself. The tiers really do
    /// disagree with each other: a wrong answer in one of them.
    Stable,
    /// One mode answered two different things across runs of one binary. The
    /// cross-mode comparison that flagged this program is meaningless until
    /// that is fixed. An ENGINE bug, and the most serious kind — a wrong answer
    /// that is not even stable.
    Nondeterministic {
        mode: &'static str,
        answers: Vec<String>,
    },
    /// The BATCH run disagreed, but every standalone re-run of both modes
    /// agrees. The divergence is in running many programs in one process — engine
    /// state carried between them, or a chunk that died and was refilled — not
    /// in the program. A HARNESS finding.
    BatchOrderOnly,
}

struct Divergence {
    index: u64,
    source: String,
    digests: Vec<(String, String)>,
    /// What the JIT decided for the minimized program, from `ZIPP_JITLOG=1`.
    /// A wrong answer without this is a bug report that still needs an
    /// afternoon; with it, the tier is already named.
    trace: Vec<String>,
    /// Set before the shrink, from re-runs of the two modes that disagreed —
    /// never inferred from the table below, which is a sample and not a proof.
    flake: Flake,
}

impl Divergence {
    fn is_nondeterministic(&self) -> bool {
        matches!(self.flake, Flake::Nondeterministic { .. })
    }
}

/// Run `m` on `src` up to `runs` times and return the DISTINCT answers in
/// first-seen order, stopping the moment two of them differ.
///
/// Each run is a fresh process (see [`single_digest`]), which is the only way
/// this question can be asked honestly: a second run inside one process shares
/// the JIT's compiled regions, its IC state and its heap, so it would answer
/// the same thing for reasons that have nothing to do with determinism.
fn self_answers(src: &str, m: &'static Mode, runs: usize) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..runs {
        let d = single_digest(src, m);
        if !seen.contains(&d) {
            seen.push(d);
            if seen.len() > 1 {
                return seen;
            }
        }
    }
    seen
}

/// The per-mode answer table for a finding, taken with TWO runs of each mode
/// rather than one.
///
/// The second run is what makes the table readable. A single sample from a
/// flaky program prints a column of plausible-looking digests that invite a
/// diagnosis of the wrong bug, and it hides a mode that flakes when the two
/// modes the shrink happened to pick do not. A mode that answers two things is
/// rendered `x  ≠  y` and returned as a [`Flake::Nondeterministic`] the caller
/// may adopt if it had no stronger evidence of its own.
fn mode_table(src: &str) -> (Vec<(String, String)>, Flake) {
    let mut flake = Flake::Stable;
    let mut rows = Vec::new();
    for m in MODES {
        let ans = self_answers(src, m, 2);
        if ans.len() > 1 && matches!(flake, Flake::Stable) {
            flake = Flake::Nondeterministic {
                mode: m.name,
                answers: ans.clone(),
            };
        }
        rows.push((m.name.to_string(), ans.join("  ≠  ")));
    }
    (rows, flake)
}

fn describe(d: &Divergence, seed: u64) -> String {
    let mut s = String::new();
    match &d.flake {
        Flake::Nondeterministic { mode, answers } => {
            s.push_str(&format!(
                "\n═══ NONDETERMINISTIC  seed={seed} index={}  ═══\n",
                d.index
            ));
            s.push_str(&format!(
                "    mode `{mode}` answered {} across separate runs of ONE binary.\n",
                answers.join(" / ")
            ));
            s.push_str(
                "    This is NOT a tier divergence: the cross-mode comparison that flagged\n",
            );
            s.push_str(
                "    it says nothing while the program disagrees with itself. Look for an\n",
            );
            s.push_str(
                "    ENGINE read of something never written — an unfilled home at OSR entry,\n",
            );
            s.push_str("    an uninitialised register — not for a tier that computes the wrong\n");
            s.push_str("    thing. The table below is two runs per mode.\n");
        }
        Flake::BatchOrderOnly => {
            s.push_str(&format!(
                "\n═══ BATCH-ORDER DIVERGENCE  seed={seed} index={}  ═══\n",
                d.index
            ));
            s.push_str("    The batch disagreed; every standalone re-run of both modes agrees.\n");
            s.push_str(
                "    The finding is in running many programs in ONE process (engine state\n",
            );
            s.push_str(
                "    carried across them, or a dead chunk refilled), not in the program —\n",
            );
            s.push_str("    a HARNESS bug. Reported unshrunk.\n");
        }
        Flake::Stable => {
            s.push_str(&format!(
                "\n═══ TIER DIVERGENCE  seed={seed} index={}  ═══\n",
                d.index
            ));
        }
    }
    for (m, dig) in &d.digests {
        s.push_str(&format!("    {m:<16} {dig}\n"));
    }
    if !d.trace.is_empty() {
        s.push_str("── tier trace (ZIPP_JITLOG=1, default mode) ──\n");
        for l in &d.trace {
            s.push_str(&format!("    {l}\n"));
        }
    }
    s.push_str("── minimized source ──\n");
    s.push_str(&d.source);
    s.push_str("──────────────────────\n");
    s
}

/// Compare every mode over `[lo, hi)`, minimize whatever disagrees, and return
/// one entry per diverging program.
fn sweep(
    seed: u64,
    lo: u64,
    hi: u64,
    modes: &[&'static Mode],
    big: bool,
    verbose: bool,
) -> Vec<Divergence> {
    let mut handles = Vec::new();
    for m in modes {
        let m: &'static Mode = m;
        handles.push(std::thread::spawn(move || {
            (m.name, batch_digests(seed, lo, hi, m, big))
        }));
    }
    let mut per_mode: Vec<(&'static str, BTreeMap<u64, String>)> = Vec::new();
    for h in handles {
        per_mode.push(h.join().expect("mode thread"));
    }

    let mut out = Vec::new();
    for i in lo..hi {
        let vals: Vec<(&str, String)> = per_mode
            .iter()
            .map(|(n, m)| (*n, m.get(&i).cloned().unwrap_or_else(|| "MISSING".into())))
            .collect();
        let first = vals[0].1.clone();
        if vals.iter().all(|(_, v)| *v == first) {
            continue;
        }
        // Two modes that actually disagree drive the shrink.
        let a = mode(vals[0].0);
        let b = mode(vals.iter().find(|(_, v)| *v != first).unwrap().0);
        let prog = gen_program(prog_seed(seed, i), big);
        let base_src = emit(&prog, "", "D", false);

        // ── classify before shrinking ── the cross-mode shrink predicate below
        // asks "do a and b still disagree?". For a program that disagrees with
        // ITSELF that question has no stable answer, so the shrink wanders and
        // the report names the wrong bug. Re-run each of the two modes on its
        // own first (see `SELF_RUNS`); the samples are reused, so the old
        // reproduces-standalone check costs nothing extra.
        let sa = self_answers(&base_src, a, SELF_RUNS);
        let sb = if sa.len() > 1 {
            Vec::new()
        } else {
            self_answers(&base_src, b, SELF_RUNS)
        };
        if let Some((fm, answers)) = match (sa.len() > 1, sb.len() > 1) {
            (true, _) => Some((a, sa.clone())),
            (_, true) => Some((b, sb.clone())),
            _ => None,
        } {
            if verbose {
                eprintln!(
                    "[fuzz] index {i}: NONDETERMINISTIC — mode {} answered {} across runs of one binary; shrinking on self-disagreement",
                    fm.name,
                    answers.join(" / ")
                );
            }
            // Shrink on the property that actually holds: this program does not
            // answer the same thing twice in `fm`. Sound in the direction that
            // matters — a step is kept only when the disagreement was OBSERVED,
            // so no candidate is ever accepted for a divergence it does not have.
            let mut evals = 0usize;
            let mut check = |p: &Program| {
                if evals > SELF_SHRINK_EVALS {
                    return false;
                }
                evals += 1;
                self_answers(&emit(p, "", "D", false), fm, SELF_RUNS_SHRINK).len() > 1
            };
            let minimal = shrink(&prog, &mut check);
            let src = emit(&minimal, "", "D", false);
            let (digests, _) = mode_table(&src);
            let trace = tier_trace(&src);
            out.push(Divergence {
                index: i,
                source: src,
                digests,
                trace,
                flake: Flake::Nondeterministic {
                    mode: fm.name,
                    answers,
                },
            });
            continue;
        }

        // Both modes are self-consistent, so `sa[0]` / `sb[0]` ARE their
        // standalone answers: if those agree, only the batch disagreed.
        if sa[0] == sb[0] {
            if verbose {
                eprintln!(
                    "[fuzz] index {i}: NOT reproducible standalone — batch-order dependent, reporting unshrunk"
                );
            }
            // `a` and `b` are self-consistent, but the table covers all 37: if a
            // THIRD mode disagrees with itself, that is the stronger and more
            // actionable verdict — an engine finding rather than a harness one —
            // so it wins. Costs nothing; the table is taken either way.
            let (digests, table_flake) = mode_table(&base_src);
            let trace = tier_trace(&base_src);
            let flake = match table_flake {
                Flake::Stable => Flake::BatchOrderOnly,
                other => other,
            };
            out.push(Divergence {
                index: i,
                source: base_src,
                digests,
                trace,
                flake,
            });
            continue;
        }

        if verbose {
            eprintln!("[fuzz] index {i}: {} != {} — minimizing…", a.name, b.name);
        }
        let mut evals = 0usize;
        let mut check = |p: &Program| {
            if evals > 900 {
                return false;
            }
            evals += 1;
            let src = emit(p, "", "D", false);
            let da = single_digest(&src, a);
            let db = single_digest(&src, b);
            da != db
        };
        let minimal = shrink(&prog, &mut check);
        let src = emit(&minimal, "", "D", false);
        // The table's second run per mode is the wider net: `a` and `b` are
        // self-consistent, but a THIRD mode may be the flaky one, and shrinking
        // can also carry a program into a flaky shape. Adopt that verdict — the
        // evidence for it is the same kind, just cheaper.
        let (digests, flake) = mode_table(&src);
        let trace = tier_trace(&src);
        out.push(Divergence {
            index: i,
            source: src,
            digests,
            trace,
            flake,
        });
    }
    out
}

/// `(seed, index)` pairs whose divergence is a KNOWN OPEN finding rather than a
/// regression, so the CI slice can stay a REGRESSION gate while the finding is
/// still open. Every entry names the `#[ignore]`d spec that carries its
/// minimized case; delete the entry when that spec goes green.
///
/// This list is the only thing standing between an honest fuzzer and a
/// permanently red suite. It must never grow to hide a NEW divergence: an entry
/// without a named minimized spec beside it is a bug being swept under a rug.
///
/// If this slice goes red with an index that is NOT listed here, it is a new
/// divergence — compare the minimized case against the `open_*` specs before
/// assuming otherwise.
const KNOWN_OPEN: &[(u64, u64, &str)] = &[
    // EMPTY. Every divergence this fuzzer has found is closed and carried by a
    // green spec below. A failing index here is a NEW divergence, not a known
    // one — minimize it, root-cause it, and only then consider a line here.
    //
    // (W18 removed the list's second-ever entry — index 380 of seed 0x5A17…,
    // `if (i === 100000) { t0 = 5; }` ahead of `d1 = d1 * 0.5 + (t0 | 0)`: a
    // conditional in-region def treated as a dominating one, so the local lost
    // its entry load. `open_conditional_def_loses_its_entry_load` carries it.)
    // (W16 closed the only entry this list ever held — index 392 of seed
    // 0x5A17…, the live-out `Bool` defect on the DOUBLE tier over a
    // double-element Array. Its four answers were one register: the tier's
    // `Bitwise` sentinel and dense-Array tag check both scratched
    // `BOOL_GPRS[2]`. See `tests/bool_home_clobber.rs`.)
];

fn selected_modes(names: &[&str]) -> Vec<&'static Mode> {
    names.iter().map(|n| mode(n)).collect()
}

fn parent_guard() -> bool {
    std::env::var_os("ZIPP_FUZZ_JOB").is_some()
}

// ──────────────────────────────── the tests ────────────────────────────────

/// The bounded, seeded slice that runs with the normal suite.
///
/// Deterministic by construction: one fixed seed, one fixed program count, one
/// fixed mode list. If this ever fails it prints the minimized program and the
/// digest each mode produced for it.
#[test]
fn tier_differential_ci_slice() {
    if parent_guard() {
        return;
    }
    const SEED: u64 = 0x5A17_2026_0F1E_2D3C;
    // Sized by calibration, not by taste. Run against a worktree at the pre-W14
    // commit — where the r10 clobber is live and reachable shapes therefore
    // answer WRONG — this seed reports one divergence at 96 programs (a coin
    // flip, not a gate) and five at 500, which puts the slice's power against
    // that bug class at p ≈ 1 - e^-5. Five hundred programs across seven modes
    // is ~3s in the dev profile.
    // W17 raised it from 500. The slice's power was calibrated in PROGRAMS, but
    // what it is really calibrated in is INT REGIONS: the pool it draws flavors
    // from grew from 14 entries to 22 (DOUBLE, split-receiver and INT-family
    // shapes), so the INT family's share fell 57% -> 50% and INT regions per
    // program fell with it, 686 -> 537 per 400. 500 * 686/537 = 639 restores the
    // original absolute reach, and every new axis rides along at the same 28%.
    const COUNT: u64 = 640;
    let modes = selected_modes(CI_MODES);
    let found: Vec<Divergence> = sweep(SEED, 0, COUNT, &modes, false, false)
        .into_iter()
        .filter(|d| {
            !KNOWN_OPEN
                .iter()
                .any(|&(s, i, _)| s == SEED && i == d.index)
        })
        .collect();
    assert!(
        found.is_empty(),
        "{} of {COUNT} generated programs answer differently across tiers{}:{}",
        found.len(),
        flake_tally(&found),
        found.iter().map(|d| describe(d, SEED)).collect::<String>()
    );
}

/// The one-line breakdown that goes in front of a failure so the first thing
/// read is WHICH kind of bug this is. A nondeterministic program is an engine
/// defect of a different (worse) shape than a tier divergence, and a
/// batch-order one is not an engine defect at all.
fn flake_tally(ds: &[Divergence]) -> String {
    let nd = ds.iter().filter(|d| d.is_nondeterministic()).count();
    let bo = ds
        .iter()
        .filter(|d| matches!(d.flake, Flake::BatchOrderOnly))
        .count();
    let mut parts = Vec::new();
    if nd > 0 {
        parts.push(format!(
            "{nd} NONDETERMINISTIC (disagree with themselves in one mode — an engine read of something never written, not a tier disagreeing with a tier)"
        ));
    }
    if bo > 0 {
        parts.push(format!(
            "{bo} BATCH-ORDER ONLY (a harness finding: standalone re-runs agree)"
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" — of which {}", parts.join("; "))
    }
}

/// The long run. Not in the normal suite — it is minutes, not seconds.
///
/// ```text
/// ZIPP_FUZZ_SEED=12345 ZIPP_FUZZ_COUNT=20000 ZIPP_FUZZ_BIG=1 \
///   cargo test --release --test jit_tier_fuzz -- --ignored --nocapture tier_differential_soak
/// ```
///
/// `ZIPP_FUZZ_MODES` is a comma list from [`MODES`] (default: all of them).
/// The seed is PRINTED whether it was given or defaulted, so any finding here
/// replays exactly.
#[test]
#[ignore = "soak: run explicitly with ZIPP_FUZZ_SEED / ZIPP_FUZZ_COUNT"]
fn tier_differential_soak() {
    if parent_guard() {
        return;
    }
    let seed: u64 = std::env::var("ZIPP_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5A17_2026_0F1E_2D3C);
    let count: u64 = std::env::var("ZIPP_FUZZ_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);
    let start: u64 = std::env::var("ZIPP_FUZZ_START")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let big = std::env::var_os("ZIPP_FUZZ_BIG").is_some();
    let names: Vec<String> = match std::env::var("ZIPP_FUZZ_MODES") {
        Ok(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
        Err(_) => MODES.iter().map(|m| m.name.to_string()).collect(),
    };
    let modes: Vec<&'static Mode> = names.iter().map(|n| mode(n)).collect();

    eprintln!(
        "[fuzz] soak seed={seed} start={start} count={count} big={big} modes={}",
        modes.iter().map(|m| m.name).collect::<Vec<_>>().join(",")
    );
    // WHICH BINARY. A soak transcript is evidence about one build, and in a tree
    // several people are editing it is easy to read a finding from a run whose
    // engine is already two fixes behind — W18 spent an hour root-causing a
    // "live" nondeterministic program that a rebuild had already fixed. The exe
    // and its mtime make every transcript say what it was actually testing.
    if let Ok(exe) = std::env::current_exe() {
        let stamp = std::fs::metadata(&exe)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|| "?".into());
        eprintln!(
            "[fuzz] engine under test: {} (mtime {stamp})",
            exe.display()
        );
    }
    let chunk: u64 = 64;
    let mut all = Vec::new();
    let t0 = Instant::now();
    let mut i = start;
    while i < start + count {
        let hi = (i + chunk).min(start + count);
        let found = sweep(seed, i, hi, &modes, big, true);
        for d in &found {
            eprint!("{}", describe(d, seed));
        }
        all.extend(found);
        i = hi;
        eprintln!(
            "[fuzz] {}/{} programs, {} divergent ({} nondeterministic), {:.1}s",
            i - start,
            count,
            all.len(),
            all.iter().filter(|d| d.is_nondeterministic()).count(),
            t0.elapsed().as_secs_f64()
        );
    }
    assert!(
        all.is_empty(),
        "{} divergent programs (see above){}",
        all.len(),
        flake_tally(&all)
    );
}

/// node is the SECONDARY oracle: it catches the case where every zipp tier is
/// wrong in the same way, which cross-mode comparison cannot see. Programs are
/// prefix-isolated, so a whole batch runs as one node process.
#[test]
fn node_oracle_slice() {
    if parent_guard() {
        return;
    }
    const SEED: u64 = 0x0DDB_A11_0FF_1CE;
    // Ninety-six keeps this in the normal suite's budget. `ZIPP_FUZZ_NODE_COUNT`
    // raises it for a one-off audit of the generator itself: if the generator
    // could emit anything implementation-defined, a big node comparison is where
    // it would surface, and a fuzzer with false positives gets ignored.
    let count: u64 = std::env::var("ZIPP_FUZZ_NODE_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(96);
    let big = false;

    let mut batch = String::new();
    for i in 0..count {
        let prog = gen_program(prog_seed(SEED, i), big);
        batch.push_str(&emit(&prog, &format!("p{i}_"), &format!("D{i}"), true));
    }
    let dir = std::env::temp_dir().join("zipp-jit-fuzz");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("node-batch-{}.js", std::process::id()));
    std::fs::write(&path, &batch).expect("write node batch");
    let out = Command::new("node")
        .arg(&path)
        .output()
        .expect("node on PATH (the secondary oracle)");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let node: BTreeMap<u64, String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(k, v)| {
            k.strip_prefix('D')
                .and_then(|s| s.parse::<u64>().ok())
                .map(|i| (i, v.to_string()))
        })
        .collect();
    assert_eq!(
        node.len() as u64,
        count,
        "node did not report every program"
    );

    let ours = batch_digests(SEED, 0, count, mode("base"), big);
    let mut bad = Vec::new();
    for i in 0..count {
        let theirs = &node[&i];
        let mine = ours.get(&i).cloned().unwrap_or_else(|| "MISSING".into());
        // The batch tag is `D{i}` on both sides.
        let mine = mine.trim().to_string();
        if *theirs != mine {
            let prog = gen_program(prog_seed(SEED, i), big);
            bad.push(format!(
                "\nindex {i}: node={theirs} zipp={mine}\n{}",
                emit(&prog, "", "D", false)
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "zipp disagrees with node on {} programs:{}",
        bad.len(),
        bad.concat()
    );
}

/// OPEN #1, found by this harness on the tree it was written against.
///
/// A `Bool` local that is LIVE-OUT of a hot loop is lost once the loop's region
/// is evicted from the INT tier and re-compiled on the DOUBLE/regalloc tier. It
/// reads back as `NaN` in the case below and as `false` in the sibling case in
/// `open_bool_live_out_reads_back_false`, which is the same defect seen through
/// two different plans.
///
/// It is always the THIRD bool — never `b0`, which is branched on INSIDE the
/// region. `BOOL_GPRS[2]` is where the shape points, the same register W14's
/// dense-Array tag check was scratching, so the fix that landed there did not
/// close the class.
///
/// Reproduces at HEAD 6ed29ac, so it is NOT introduced by the INT-splice work in
/// flight beside it. Every mode reproduces except three: `ZIPP_NOJIT=1`,
/// `ZIPP_JIT_THRESHOLD=200` (n=120, so the region never compiles) and
/// `ZIPP_NO_FUSED_CMPJUMP=1`. The last is a re-plan, not a fix — at HEAD it
/// merely moves the corruption onto `b1`, and on other generated shapes the
/// DEFAULT is right while `ZIPP_NO_FUSED_CMPJUMP=1` alone is wrong, i.e. that
/// off-switch is not the pure fallback it is specified to be.
///
/// The tier trace is the diagnosis: INT admitted with GPR homes, ~80 deopts at
/// the element read, `EVICTED (retry=true)`, then re-compiled as a DOUBLE
/// region — and the wrong answer starts exactly at the call where the DOUBLE
/// region is installed. A raw f64 home reaching the slot is why it prints as
/// `NaN` rather than as a wrong boolean.
///
/// Every ingredient is load-bearing; the minimizer rejected removing any of
/// them: three bools, the first one branched on INSIDE the loop, a dense
/// all-Int Array read at a DATA-DEPENDENT index that leaves the array
/// (`(h * 3) & 63` over 32 elements — the out-of-range reads are what force the
/// deopt/eviction), and the third bool read only after the loop.
///
/// CLOSED IN W16, and the tier trace above was the red herring the minimizer
/// left behind: the eviction/re-compile is only how the program REACHES the
/// DOUBLE tier, whose body then destroys `BOOL_GPRS[2] = r10` twice over —
/// once in `regalloc.rs`'s `Bitwise` arm (the INT64_MIN sentinel) and once in
/// `emit_box_to_home`, the dense-Array element tag check. Both now scratch rdx.
/// `tests/bool_home_clobber.rs` carries the class; this stays as the fuzzer's
/// own minimized case.
#[test]
fn open_bool_local_reads_back_as_nan() {
    if parent_guard() {
        return;
    }
    const SRC: &str = r#"
var arr = [0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2];
function kernel(n) {
  var h = 1, i = 0, t0 = 1;
  var b0 = false, b1 = false, b2 = false;
  for (i = 0; i < n; i++) {
    b0 = (t0 | 0) < 2;
    b1 = (t0 | 0) < 3;
    b2 = i >= 4;
    t0 = arr[((h * 3) & 63)];
    h = (h + (t0 | 0)) | 0;
    if (b0) h = (h + 2) | 0;
  }
  return typeof b2 + ":" + b2;
}
console.log(kernel(120));
"#;
    assert_matches_node(SRC);
}

/// Run `src` in-process and require it to answer exactly what `node -e` answers.
///
/// Against node, never against `ZIPP_NOJIT=1`: an emitter bug that also existed
/// in the interpreter would pass that.
fn assert_matches_node(src: &str) {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    let node = Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (expected values come from `node -e`)");
    assert!(
        node.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&node.stderr)
    );
    let want: Vec<String> = String::from_utf8_lossy(&node.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(out.output, want, "zipp != node for:{src}");
}

/// OPEN #1, second face: the same live-out `Bool` comes back `false` instead of
/// `true` once the region is re-compiled on the DOUBLE tier.
///
/// Sharper than the NaN case because the boundary is visible: `kernel(12)`
/// returns 23 on the first 22 calls and 4 on every call after, with nothing
/// about the program changing in between. `ZIPP_NO_FUSED_CMPJUMP=1` returns 5
/// from the same point — a third answer for the same program.
///
/// CLOSED IN W16 with its sibling above, and this face is what named the
/// mechanism: 23 ^ 4 = 19 is exactly `b2`'s term and 23 ^ 5 = 18 exactly
/// `b1`'s, i.e. each spelling loses whichever bool the planner put in
/// `BOOL_GPRS[2]` — nothing about "live-out" or about the re-compile.
#[test]
fn open_bool_live_out_reads_back_false() {
    if parent_guard() {
        return;
    }
    const SRC: &str = r#"
var arr = [0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2];
function kernel(n) {
  "use strict";
  var h = 1, i = 0, t0 = 1;
  var b0 = false, b1 = false, b2 = false;
  for (i = 0; i < n; i++) {
    b0 = (t0 | 0) === 2;
    b1 = h >= 3;
    b2 = (t0 | 0) === 4;
    t0 = arr[(i - 4)];
    h = (h + (t0 | 0)) | 0;
  }
  return (h ^ ((t0 | 0) * 3) ^ (b0 ? 17 : 0) ^ (b1 ? 18 : 0) ^ (b2 ? 19 : 0)) | 0;
}
var o = [];
for (var r = 0; r < 40; r++) o.push(kernel(12));
console.log(o.join(","));
"#;
    assert_matches_node(SRC);
}

/// OPEN #4, found by this harness on the tree it was written against, and the
/// one with the widest blast radius: a compiled NESTED loop runs FEWER INNER
/// ITERATIONS than the interpreter does.
///
/// The arithmetic is exact, which is what makes it unambiguous. Each inner
/// iteration adds `(255 * -3 * 1024) | 0` = -783360 to `h`. node and
/// `ZIPP_NOJIT=1` both return `1 + 18 * -783360` = -14100479 for `kernel(9)`.
/// The compiled body returns -13317119, which is exactly ONE addend short, and
/// `ZIPP_JIT_THRESHOLD=1` returns -7833599, which is exactly EIGHT short. The
/// deficit tracks when the region compiled, so iterations are being dropped at
/// the OSR entry, not mis-added.
///
/// The invariant must be COMPUTED in the outer loop for this to fire. Writing
/// the same value as a literal (`d0 = -191.25`) does not reproduce, nor does
/// making it vary with `i`, nor does removing the assignment. Reproduces at
/// HEAD 6ed29ac, in every mode except `ZIPP_NOJIT=1` and a threshold high
/// enough that the loop never compiles — `ZIPP_NO_FUSED_CMPJUMP=1` and
/// `ZIPP_NO_GPR_HOMES=1` are both wrong in the same way.
///
/// CLOSED IN W16, and every word about iteration COUNTS above is the red
/// herring the arithmetic invited: the inner loop runs exactly 18 times, and
/// one of every two addends is ZERO. `d0` is defined in the outer loop and read
/// in the inner one, so it is live across the inner back-edge — but the
/// home-reuse allocator sized its home from the `[first mention, last mention]`
/// window `[10, 17]`, handed the same home to the `| 0` literal at ip 21, and
/// the second inner iteration multiplied 0 by 1024. The literal spelling
/// `d0 = -191.25` hoists to a permanent home and the `-i` spelling costs one
/// register fewer than the pool, which is why neither reproduces.
/// `tests/loop_home_liverange.rs` carries the class.
#[test]
fn open_nested_loop_drops_inner_iterations() {
    if parent_guard() {
        return;
    }
    const SRC: &str = r#"
function kernel(n) {
  var h = 1, i = 0, j = 0;
  var d0 = 1.5;
  for (i = 0; i < n; i++) {
    d0 = 255 * -3;
    for (j = 0; j < 2; j++) {
      h = (h + ((d0 * 1024) | 0)) | 0;
    }
  }
  return h;
}
console.log(kernel(9));
"#;
    assert_matches_node(SRC);
}

/// OPEN #3, found by this harness on the tree it was written against.
///
/// A fused DOUBLE compare inside a nested loop takes the wrong branch. `d1` is
/// `h * 0.5` and `h` never exceeds 109, so `d1 > 100` is false on every one of
/// the 36 evaluations — node and `ZIPP_NOJIT=1` both return 109. The compiled
/// body returns 117, i.e. it took the `7` arm twice.
///
/// No bools, no arrays, no globals: three nested `DOUBLE` regions
/// (`[18,36]`, `[14,39]`, `[8,42]`, all `region_is_int=false`) and one f64
/// compare feeding a select. `ZIPP_NO_FUSED_CMPJUMP=1` answers correctly, which
/// names the emitter; `ZIPP_JIT_THRESHOLD=200` also answers correctly, because
/// nine iterations never reach it. Reproduces at HEAD 6ed29ac.
///
/// This is a DIFFERENT shape from OPEN #1 with the same emitter under suspicion,
/// which is worth saying out loud: the fused compare/jump path is wrong in at
/// least two unrelated register-allocation situations, and in some generated
/// shapes it is the OFF-switch rather than the default that answers wrong.
///
/// CLOSED IN W16, and the fused compare was innocent: it is the SAME defect as
/// [`open_nested_loop_drops_inner_iterations`], one tier over. `d1` is defined
/// in the outer loop and read in the innermost one, so it is live across two
/// back-edges, but its home was sized from the mention window `[10, 25]` and
/// re-let to `h + (7|3)` at ip 30 — a value that reaches 109. Only the FIRST of
/// each outer iteration's four body passes compared `d1`; the other three
/// compared `h`, and `h > 100` on the last two. `ZIPP_NO_FUSED_CMPJUMP=1`
/// answered correctly because unfusing shifts the allocation, not because the
/// emitter was at fault. `tests/loop_home_liverange.rs` carries the class.
#[test]
fn open_fused_double_compare_takes_wrong_branch() {
    if parent_guard() {
        return;
    }
    const SRC: &str = r#"
function kernel(n) {
  var h = 1, i = 0, j = 0, q = 0;
  var d0 = 1.5, d1 = 2.5;
  for (i = 0; i < n; i++) {
    d1 = h * 0.5;
    for (j = 0; j < 2; j++) {
      for (q = 0; q < 2; q++) {
        d0 = d0 * 0.5 + j;
        h = (h + (d1 > 100 ? 7 : 3)) | 0;
      }
    }
  }
  return h;
}
console.log(kernel(9));
"#;
    assert_matches_node(SRC);
}

/// FIXED (W16) — was OPEN #2, found by this harness on the tree it was written
/// against. Kept under its original name so the finding stays greppable.
///
/// An out-of-range element read sitting in a COLD block of a compiled loop threw
/// `TypeError: cannot read property of undefined` instead of yielding
/// `undefined`. The receiver was a pinned GLOBAL (`pins=1/1` in the JITLOG), and
/// a pinned receiver has no numeric home — all three register emitters read it
/// through the pin's source, so its `LoadGlobal` emitted nothing and its frame
/// slot was never written. The bounds guard then deopted AT the `GetIndex` ip and
/// the interpreter re-executed it on the `undefined` the slot still held. The
/// receiver `LoadGlobal` now stores the object to that slot on every tier
/// (`emit_recv_slot_store`), which is the invariant B94's split receiver already
/// held.
///
/// Reproduced at HEAD 6ed29ac in EVERY compiled mode — no `ZIPP_NO_*` switch
/// avoided it, only `ZIPP_NOJIT=1` and a threshold high enough that the loop
/// never compiled. It was not typed-array-specific: a plain dense Array, an
/// `Int32Array` and a `Float64Array` all threw.
///
/// Load-bearing ingredients, each confirmed by removing it: the receiver must be
/// a GLOBAL (a parameter is fine — its slot is a live-in the interpreter already
/// filled), the read must be in a conditional block the interpreter has not
/// reached before the OSR compile (the same read on every iteration is fine —
/// the pre-OSR iterations leave the slot correct), and the index must be out of
/// range so the guard actually deopts. `a[33]` on a 32-element array threw just
/// as `a[9999]` did.
///
/// `tests/cold_pinned_recv.rs` carries the wider suite: the DOUBLE and INT-GPR
/// emitters, a negative index, an out-of-bounds TypedArray STORE, a pinned-string
/// `charCodeAt`, and the JITLOG mechanism pins that keep those cases non-vacuous.
#[test]
fn open_cold_out_of_range_read_throws() {
    if parent_guard() {
        return;
    }
    const SRC: &str = r#"
var a = [1, 2, 3];
function kernel(n) {
  var t = 4;
  for (var i = 0; i < n; i++) {
    if (i === 17) { t = a[9999]; }
  }
  return t;
}
console.log(typeof kernel(20));
"#;
    assert_matches_node(SRC);
}

/// OPEN #6 (W17), found by the widened generator at seed 170013 index 45378 and
/// minimized by hand to two lines. **Reproduces at the committed HEAD 0ade520**,
/// so it is not something this wave's in-flight work introduced.
///
/// A local whose ONLY in-region definition sits on a CONDITIONAL branch loses
/// its entry load, and the compiled body then reads its home as garbage on
/// every pass that skips the branch:
///
/// ```text
/// function k(n){var h=1,i=0,t=2;for(i=0;i<n;i++){if(i===3){t=7;}h=(h+t)|0;}return h;}
/// k(40)  →  node and ZIPP_NOJIT=1: 266.  Compiled: 74.
/// ```
///
/// 74 - 42 = 32 over the 32 native iterations, i.e. `t` reads as `1` where it
/// holds 2 (before `i === 3`) and 7 (after). `ZIPP_NO_GPR_HOMES=1` answers 42 —
/// a THIRD answer, so both register emitters are wrong, not one. No `ZIPP_NO_*`
/// switch avoids it; only `ZIPP_NOJIT=1` and a threshold high enough that the
/// loop never compiles.
///
/// The wrong number is not stable, which is the tell that this is a garbage READ
/// and not a mis-computation: 74 from `zipp.exe js`, `-2013640406` from
/// `zipp_vm::run` in this test binary, 264 when the same kernel is called a few
/// times first. Whatever the home last held is what the loop adds.
///
/// THE ROOT CAUSE is one sentence, and `plan_region.rs` already contains it —
/// about a different consumer. `first_seen[r] == true` means "the first
/// OCCURRENCE of `r` inside the region is a def"; the entry-load and
/// home-sharing decisions read it as "a def of `r` dominates every use of `r`".
/// Those are different properties, and the file says so at the CONSTANT-HOISTING
/// site: *"`first_seen == true` only says the first OCCURRENCE is a def — it
/// says nothing about whether that def runs. Hoisting a constant whose def sits
/// on an untaken branch is wrong twice over"*. That site guards itself with
/// `runs_every_iteration`. `shareable()` and the `live_in_regs` /
/// `(first_ip, last_ip)` range decision beside it do not:
///
/// ```text
/// let shareable = |r: u16| -> bool {
///     first_seen.get(&r) != Some(&false)        // ← "first mention is a def"
///         && !hoisted.contains(&r)
///         && (… read_outside …)
/// };
/// ```
///
/// In the bytecode above the region is `[5, 17]` and `t` is `r5`. Its first
/// occurrence in the region is `LoadInt { dst: 5, val: 7 }` at ip 9 — a def, so
/// `first_seen[r5] = true`, so `r5` is `shareable`, so it is dropped from
/// `live_in_regs` and starts as garbage. But ip 9 sits behind the
/// `JumpIfFalse` at ip 8: the path 5 → 8 → 11 reaches the USE at ip 11
/// (`Add { dst: 14, a: 3, b: 5 }`) without ever running the def. This is the
/// same shape as `hoisted_const_on_untaken_branch`, one consumer over.
///
/// WHY THE SUITE NEVER SAW IT, and why the generator only just did: reading `t`
/// AFTER the loop makes the program answer correctly, because `read_outside`
/// used to force a permanent home with an entry load. Every hand-written test
/// and every one of the 13 benchmark rows reads its accumulators afterwards; so
/// does this file's own return mix, which is why 138,300 W15-generator programs
/// missed it. The generated case that finally hit it needed the extra step of a
/// `Deopt` statement writing the temp inside a guard.
///
/// A SECOND FACE: W17's GPR write-through-sharing lever makes `read_outside`
/// registers shareable too (write-through instead of a pinned home), so the same
/// defect also reached a local that IS read after the loop. Same root cause; the
/// lever widened its reach rather than causing it, which is why it had to ship
/// dark. W18 closed the defect and the lever is default-on
/// (`ZIPP_NO_GPR_WT_SHARE=1` turns it off), so this spec is run in BOTH
/// positions by `MODES`.
///
/// CLOSED (W18): `plan_region.rs` now derives the region's live-in set from the
/// same backward liveness walk that produces the live spans, and `shareable` /
/// `range` ask that one predicate instead of `first_seen`. Before the fix this
/// answered 74 compiled and 42 under `ZIPP_NO_GPR_HOMES=1`, against 266.
#[test]
fn open_conditional_def_loses_its_entry_load() {
    if parent_guard() {
        return;
    }
    const SRC: &str = r#"
function kernel(n) {
  var h = 1, i = 0, t = 2;
  for (i = 0; i < n; i++) {
    if (i === 3) { t = 7; }
    h = (h + t) | 0;
  }
  return h;
}
console.log(kernel(40));
"#;
    assert_matches_node(SRC);
}

/// The same defect with the conditional def in a block the interpreter NEVER
/// reaches — the cold-side-exit spelling, which is how the fuzzer found it.
/// Kept separate because it is the one an engine author is likelier to reason
/// about ("the block never runs, so nothing about it can matter") and because
/// it is the one that survived every `ZIPP_NO_*` switch unchanged. CLOSED (W18).
#[test]
fn open_cold_conditional_def_loses_its_entry_load() {
    if parent_guard() {
        return;
    }
    const SRC: &str = r#"
function kernel(n) {
  var h = 1, i = 0, t = 2;
  for (i = 0; i < n; i++) {
    if (i === 100000) { t = 7; }
    h = (h + t) | 0;
  }
  return h;
}
console.log(kernel(9), kernel(40), kernel(400));
"#;
    assert_matches_node(SRC);
}

/// Assert `src` answers the SAME thing on every one of `runs` fresh processes,
/// and that the answer is node's.
///
/// [`assert_matches_node`] runs the engine ONCE, in-process. That is the right
/// test for a wrong answer and the wrong one for an unstable answer: it passes
/// on the run where the garbage happens to be benign. This asks the other
/// question, and it has to spend processes to ask it — see [`self_answers`].
fn assert_same_answer_every_run(src: &str, runs: usize) {
    let ans = self_answers(src, mode("base"), runs);
    assert_eq!(
        ans.len(),
        1,
        "the same binary answered {} on separate runs of this program — nondeterminism, \
         not a wrong constant; look for a home the compiled body reads before anything \
         writes it:{src}",
        ans.join(" / ")
    );
    assert_matches_node(src);
}

/// W18: [`open_conditional_def_loses_its_entry_load`] wearing the face that
/// made it dangerous to the instrument itself — the program does not answer the
/// same thing TWICE.
///
/// These two are verbatim from the W17 gate soak (`s3127_i361`, `s3129_i318`),
/// the programs that cost that gate its triage budget: `D 7840` / `D b08f`
/// alternating roughly 50/50 over runs of one binary, on the committed baseline
/// as much as on the wave tree. Sibling specs of the same defect, but they are
/// kept because they lock a DIFFERENT property, and one no other test in this
/// file asserts: that an answer is stable at all.
///
/// The mechanism, which is why an unfilled home reads as a coin flip rather
/// than as a fixed wrong constant. `t0`'s only in-region def sits behind
/// `if (i === …)`, so it looked def-first, became `shareable`, and dropped out
/// of `live_in_regs`. Its home — `xmm4`, mapped to a GPR by the INT tier's
/// GPR-home sub-mode — was therefore never filled at OSR entry, and
/// `(t0 | 0) > 31` read whatever the previous phase had left in that register.
/// What is left there is address-derived, so it differs run to run: the SAME
/// wrong-answer defect, but reported as an unstable one. `ZIPP_NO_GPR_HOMES=1`
/// hid it by moving the garbage to an xmm home that happened to hold a benign
/// value — which is exactly how a switch-differential can mislead.
///
/// The two spellings are both here because they fail in OPPOSITE directions:
/// the taken-branch one reads garbage as `true` where node says `false`, the
/// never-taken one as `false` where node says `true`. A fix that only ever
/// leaves zero in the register would pass one of them.
#[test]
fn conditional_def_answers_the_same_thing_every_run() {
    if parent_guard() {
        return;
    }
    // The branch IS taken (i === 5 of 9), and the wrong answer is the FIRST
    // call's: the region compiles on that call's last back-edge.
    const TAKEN: &str = r#"
function main() {
  var acc = 1;
  for (var r = 0; r < 3; r++) acc = (Math.imul(acc, 31) + (kernel(9) | 0)) | 0;
  return (acc) | 0;
}
function kernel(n) {
  "use strict";
  var h = 1, i = 0, j = 0, q = 0;
  var t0 = 1;
  var b0 = false;
  for (i = 0; i < n; i++) {
    if (i === 5) { t0 = -1; }
    b0 = (t0 | 0) > (31);
  }
  return (h ^ (b0 ? 17 : 0) ^ (typeof b0 === "boolean" ? 0 : 1024)) | 0;
}
try { console.log("D " + (main() >>> 0).toString(16)); } catch (e) { console.log("D E:" + (e && e.constructor ? e.constructor.name : "?")); }
"#;
    // The branch is NEVER taken (i === 65 of 9) — the cold spelling, and the
    // one where every call answers wrong together.
    const NEVER: &str = r#"
function main() {
  var acc = 1;
  for (var r = 0; r < 3; r++) acc = (Math.imul(acc, 31) + (kernel(9) | 0)) | 0;
  return (acc) | 0;
}
function kernel(n) {
  var h = 1, i = 0, j = 0, q = 0;
  var t0 = 1;
  var b0 = false;
  for (i = 0; i < n; i++) {
    if (i === 65) { t0 = -1; }
    b0 = (t0 | 0) <= (6);
  }
  return (h ^ (b0 ? 17 : 0) ^ (typeof b0 === "boolean" ? 0 : 1024)) | 0;
}
try { console.log("D " + (main() >>> 0).toString(16)); } catch (e) { console.log("D E:" + (e && e.constructor ? e.constructor.name : "?")); }
"#;
    // 12 fresh processes each. A 50/50 flake escapes that with probability
    // 2^-11; measured on the pre-fix binary, 16 runs caught both programs and
    // 8 runs caught one of the two.
    assert_same_answer_every_run(TAKEN, 12);
    assert_same_answer_every_run(NEVER, 12);
}

/// The generator must never emit anything whose answer is implementation-defined
/// — a fuzzer with false positives gets ignored, which is worse than no fuzzer.
/// This is the lint that keeps a later edit from reintroducing one.
///
/// It is also where W18 ruled the generator OUT as the source of the
/// run-to-run instability described in the module header: this list already
/// bans every construct whose value can vary between two runs of one program —
/// `Math.random`, `Date`, `performance`, `for…in` order, `Object.keys` order —
/// so a generated program that answers two things is the ENGINE answering two
/// things. Anything added here that reads a clock, an address, an iteration
/// order or an entropy source breaks that argument, not just this test.
#[test]
fn generator_emits_only_exact_js() {
    if parent_guard() {
        return;
    }
    // Everything here is either non-deterministic, locale-dependent, or (for the
    // Math members) explicitly implementation-approximated in the spec.
    const BANNED: &[&str] = &[
        "Math.random",
        "Math.sin",
        "Math.cos",
        "Math.tan",
        "Math.pow",
        "Math.exp",
        "Math.log",
        "Math.atan",
        "Math.asin",
        "Math.acos",
        "Math.hypot",
        "Math.cbrt",
        "Math.sinh",
        "Math.cosh",
        "Math.tanh",
        "**",
        "Date",
        "for (var kk in",
        " in ",
        "Object.keys",
        "JSON.stringify",
        "toLocale",
        "toFixed",
        "toPrecision",
        "performance",
        "prototype",
        "Reflect",
        "Proxy",
        "eval",
        "Function(",
        "WeakMap",
        "Symbol",
    ];
    let mut n_stmts = 0usize;
    for i in 0..2500u64 {
        let prog = gen_program(prog_seed(0xF00D_1234_5678_9ABC, i), i % 2 == 0);
        n_stmts += count_stmts(&prog.body);
        let src = emit(&prog, "", "D", false);
        for b in BANNED {
            assert!(
                !src.contains(b),
                "generator emitted banned construct {b:?}:\n{src}"
            );
        }
        // Every reported number is funnelled through ToInt32, so no answer can
        // depend on Number→String formatting.
        assert!(
            src.contains(">>> 0).toString(16)"),
            "the digest must be an unsigned int32 in hex"
        );
        assert!(
            !src.contains("while ("),
            "only counted `for` loops terminate by construction"
        );
    }
    assert!(
        n_stmts > 10_000,
        "the shape space collapsed: only {n_stmts} statements in 2500 programs"
    );
}

/// A seed must reproduce a program byte for byte, or nothing reported here is
/// replayable.
#[test]
fn generator_is_deterministic() {
    if parent_guard() {
        return;
    }
    for i in [0u64, 1, 17, 999, 123456] {
        let a = emit(&gen_program(prog_seed(42, i), false), "", "D", false);
        let b = emit(&gen_program(prog_seed(42, i), false), "", "D", false);
        assert_eq!(a, b, "generator is not deterministic at index {i}");
        let c = emit(&gen_program(prog_seed(43, i), false), "", "D", false);
        assert_ne!(
            a, c,
            "different seeds produced the same program at index {i}"
        );
    }
}

/// What the shape space actually REACHES, measured rather than asserted: run a
/// sample under `ZIPP_JITLOG=1` and tally the tier decisions the engine logged.
/// `#[ignore]`d because it is a report, not a pass/fail.
#[test]
#[ignore = "coverage report: cargo test --release --test jit_tier_fuzz -- --ignored --nocapture tier_coverage_report"]
fn tier_coverage_report() {
    if parent_guard() {
        return;
    }
    let seed: u64 = std::env::var("ZIPP_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5A17_2026_0F1E_2D3C);
    let count: u64 = std::env::var("ZIPP_FUZZ_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(400);
    let out = spawn_job(
        &format!("batch:{seed}:0:{count}"),
        mode("base"),
        Duration::from_secs(600),
        true,
    );
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    for l in out.stderr.lines() {
        let key = classify_jitlog(l);
        if let Some(k) = key {
            *tally.entry(k).or_default() += 1;
        }
    }

    // The HEADLINE, because the raw tally is 120 rows and the question a
    // widening has to answer is "which tiers, how many". Every row here is a
    // gap someone named: DOUBLE and Tier A and the split receiver were each
    // reported as thin-or-absent, and a report that does not add them up cannot
    // say whether they still are.
    let count_of = |pred: &dyn Fn(&str) -> bool| -> usize {
        tally.iter().filter(|(k, _)| pred(k)).map(|(_, n)| *n).sum()
    };
    let compiled = |t: &'static str| {
        move |k: &str| k.starts_with(&format!("[jit] {t} region")) && k.ends_with("compiled")
    };
    let headline: [(&str, usize); 9] = [
        ("MEM regions", count_of(&compiled("MEM"))),
        ("INT regions", count_of(&compiled("INT"))),
        ("DOUBLE regions", count_of(&compiled("DOUBLE"))),
        (
            "SROA regions",
            count_of(&|k| k.starts_with("[jit] SROA region")),
        ),
        (
            "Tier C fns",
            count_of(&|k| k.starts_with("[jit] Tier C") && k.contains("compiled")),
        ),
        (
            "Tier A fns",
            count_of(&|k| k.starts_with("[jit] Tier A") && k.contains("compiled")),
        ),
        (
            "B94 split receivers",
            count_of(&|k| k.contains("split receiver")),
        ),
        ("deopts", count_of(&|k| k.contains("deopt at ip"))),
        ("evictions", count_of(&|k| k.contains("EVICTED"))),
    ];

    // Generator-side axes: the engine cannot report these, and an axis that
    // silently stops being generated is exactly how a widening rots.
    let mut script = 0usize;
    let (mut posts, mut loop2, mut dblscan) = (0usize, 0usize, 0usize);
    for i in 0..count {
        let prog = gen_program(prog_seed(seed, i), false);
        if prog.scope == Scope::Script {
            script += 1;
        }
        posts += prog.post.len();
        loop2 += prog
            .post
            .iter()
            .filter(|p| matches!(p, Post::Loop2 { .. }))
            .count();
        if emit(&prog, "", "D", false).contains("probe(") {
            dblscan += 1;
        }
    }

    eprintln!("[fuzz] tier coverage over {count} programs (seed {seed}):");
    eprintln!("  ── headline ──");
    for (k, n) in headline {
        eprintln!("    {n:>7}  {k}");
    }
    eprintln!("  ── generator axes ──");
    eprintln!("    {script:>7}  script-scope programs");
    eprintln!("    {posts:>7}  post-region uses ({loop2} of them a second region)");
    eprintln!("    {dblscan:>7}  programs whose live-out crosses a call");
    eprintln!("  ── every decision, by frequency ──");
    let mut rows: Vec<_> = tally.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (k, n) in rows {
        eprintln!("    {n:>7}  {k}");
    }
}

/// Tier A is the whole-function self-recursive path, and until W17 it logged
/// NOTHING on a successful compile — so its reach could be assumed but not
/// measured. It turned out not to be reached at all: `Stmt::Rec` used to spell
/// its body with `| 0`, and `fn_int::can_compile` admits no `Bitwise` op, so
/// every generated recursion landed on Tier C instead. Both halves of that are
/// pinned here — the engine's new line, and the generator shape that reaches it.
#[cfg(target_arch = "x86_64")]
#[test]
fn tier_a_is_reached() {
    if parent_guard() {
        return;
    }
    const SRC: &str = r#"
function rec(x) { if (x < 2) return x; return rec(x - 1) + rec(x - 2); }
var h = 1;
for (var i = 0; i < 400; i++) h = (h + rec((i & 7) + 2)) | 0;
console.log(h);
"#;
    let dir = std::env::temp_dir().join("zipp-jit-fuzz");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("tiera-{}.js", std::process::id()));
    std::fs::write(&path, SRC).expect("write case");
    let out = spawn_job(
        &format!("file:{}", path.display()),
        mode("base"),
        Duration::from_secs(60),
        true,
    );
    let _ = std::fs::remove_file(&path);
    assert!(
        out.stderr
            .lines()
            .any(|l| l.contains("[jit] Tier A") && l.contains("compiled")),
        "no Tier A compile logged for a fib-shaped self-recursion; JITLOG said:\n{}",
        out.stderr
    );
    // …and the generator really does emit that spelling, so this is not a test
    // of a string that only lives in this file.
    let mut found = false;
    for i in 0..400u64 {
        let s = emit(
            &gen_program(prog_seed(0x7E1A_2026, i), false),
            "",
            "D",
            false,
        );
        if s.contains("function rec(x) { if (x < 2) return x; return rec(x - 1) + rec(x - 2); }") {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "the generator no longer emits the Tier A recursion shape"
    );
}

/// Every axis W17/W20 added must still be GENERATED. Cheap (no engine), and it is
/// the thing that fails when a later edit turns an axis off by accident — a
/// widened generator that silently stops widening is worse than none, because
/// the coverage claim outlives the coverage.
#[test]
fn widened_axes_are_still_generated() {
    if parent_guard() {
        return;
    }
    let (mut script, mut typeof_, mut ident, mut dblshape) = (0, 0, 0, 0);
    let (mut probe, mut escape, mut loop2, mut dblscan) = (0, 0, 0, 0);
    let (mut split, mut dead_out, mut cond_def) = (0, 0, 0);
    let (mut push, mut not) = (0, 0);
    for i in 0..1500u64 {
        let prog = gen_program(prog_seed(0x5A17_2026_0F1E_2D3C, i), false);
        if prog.scope == Scope::Script {
            script += 1;
        }
        for p in &prog.post {
            match p {
                Post::TypeOf(_) => typeof_ += 1,
                Post::Identity(_) => ident += 1,
                Post::DblShape(_) => dblshape += 1,
                Post::Probe(_) => probe += 1,
                Post::Escape(_) => escape += 1,
                Post::Loop2 { .. } => loop2 += 1,
            }
        }
        // `Src::Dbl` is produced by `gen_double_body` and by nothing else, so
        // its text is the flavor's fingerprint.
        let src = emit(&prog, "", "D", false);
        if src.contains("d0 = d0 * 0.5 + d0;") || src.contains("d1 = d1 * 0.5 + d0;") {
            dblscan += 1;
        }
        if prog.scope == Scope::Script && prog.body.len() <= 5 {
            split += 1;
        }
        if prog.dead_out.is_some() {
            dead_out += 1;
        }
        if src.contains("t0 = 1; }")
            || src.contains("t0 = 2; }")
            || src.contains("t0 = 5; }")
            || src.contains("t0 = 7; }")
            || src.contains("t0 = 13; }")
            || src.contains("t0 = 0; }")
            || src.contains("t0 = -1; }")
        {
            cond_def += 1;
        }
        // Check emitted source, not just the internal enum, so a deleted emit
        // arm cannot leave this test green while the actual soak loses an axis.
        push += src.matches("parr.push(").count();
        // A negated BoolDef is emitted as an assignment. Restricting this to
        // ` = !(` avoids counting a negated `if`, which the bytecode compiler
        // may fold into the branch rather than materialising `Instr::Not`.
        not += src.lines().filter(|line| line.contains(" = !(")).count();
    }
    for (n, what) in [
        (script, "script-scope programs"),
        (typeof_, "Post::TypeOf"),
        (ident, "Post::Identity"),
        (dblshape, "Post::DblShape"),
        (probe, "Post::Probe"),
        (escape, "Post::Escape"),
        (loop2, "Post::Loop2"),
        (dblscan, "Flavor::DblScan bodies"),
        (split, "Flavor::Split bodies"),
        (dead_out, "dead-out locals"),
        (cond_def, "Stmt::CondDef (a def that does not dominate)"),
        (push, "Stmt::Push / parr.push emission"),
        (not, "negated Stmt::BoolDef / Instr::Not emission"),
    ] {
        assert!(
            n > 0,
            "{what} is no longer generated at all (0 in 1500 programs)"
        );
    }
}

fn classify_jitlog(l: &str) -> Option<String> {
    let l = l.trim();
    let (rest, kind) = if let Some(r) = l.strip_prefix("[jit] ") {
        (r, "jit")
    } else if let Some(r) = l.strip_prefix("[leaf] ") {
        (r, "leaf")
    } else if let Some(r) = l.strip_prefix("[decline-reason] ") {
        // `fn=<name> [start,end]: <reason>` — the region is what makes the line
        // attributable and the REASON is what makes it a coverage fact, so keep
        // the reason and drop the region.
        (
            r.split_once(": ").map(|(_, why)| why).unwrap_or(r),
            "decline",
        )
    } else if let Some(r) = l.strip_prefix("[mi] ") {
        (r, "mi")
    } else {
        return None;
    };
    // Collapse the numeric detail: what matters is WHICH decision, not where.
    let body: String = rest
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect();
    let body = body.split(" (").next().unwrap_or(&body).to_string();
    let body = body
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("[{kind}] {body}"))
}
