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
//! Five `#[ignore]`d specs below carry minimized cases for four findings on the
//! tree this was written against — [`open_bool_local_reads_back_as_nan`] and
//! [`open_bool_live_out_reads_back_false`] (one defect, two faces),
//! [`open_nested_loop_drops_inner_iterations`],
//! [`open_cold_out_of_range_read_throws`], and
//! [`open_fused_double_compare_takes_wrong_branch`]. All reproduce at HEAD
//! 6ed29ac and all are node-confirmed, so none of them is a zipp-vs-node
//! judgement call.
//!
//! A fifth result has no spec because it is about a SWITCH rather than about the
//! default: on 12 of the 28 divergent programs the soak found, the default
//! answer is right and `ZIPP_NO_FUSED_CMPJUMP=1` alone is wrong. That switch is
//! specified as a pure fallback, so any A/B measured through it is being
//! measured against wrong answers.
//!
//! The honesty check that backs that up: over 4,000 generated programs run
//! against node (`ZIPP_FUZZ_NODE_COUNT=4000`), `ZIPP_NOJIT=1` agreed with node
//! on every single one, and every one of the five node disagreements was the
//! compiled tier being wrong. The generator produced no implementation-defined
//! answers at all.
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
//! On a divergence the parent SHRINKS the program against the two disagreeing
//! modes — statement deletion, loop-bound reduction, declaration trimming — and
//! prints the minimal source plus the digest every mode gave it.

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
    Mode { name: "base", env: &[] },
    Mode { name: "nojit", env: &[("ZIPP_NOJIT", "1")] },
    Mode { name: "thr1", env: &[("ZIPP_JIT_THRESHOLD", "1")] },
    Mode { name: "thr200", env: &[("ZIPP_JIT_THRESHOLD", "200")] },
    Mode { name: "nogprhomes", env: &[("ZIPP_NO_GPR_HOMES", "1")] },
    Mode { name: "noicgate", env: &[("ZIPP_NO_ICGATE", "1")] },
    Mode { name: "nofusedcmp", env: &[("ZIPP_NO_FUSED_CMPJUMP", "1")] },
    Mode { name: "noglobrange", env: &[("ZIPP_NO_GLOB_RANGE", "1")] },
    Mode { name: "nomultisplit", env: &[("ZIPP_NO_MULTI_SPLIT", "1")] },
    Mode { name: "notypedsplice", env: &[("ZIPP_NO_TYPED_SPLICE", "1")] },
    Mode { name: "nointsplit", env: &[("ZIPP_NO_INT_SPLIT", "1")] },
    Mode { name: "nointsplice", env: &[("ZIPP_NO_INT_SPLICE", "1")] },
    Mode { name: "intsplit", env: &[("ZIPP_INT_SPLIT", "1")] },
    Mode { name: "noguardhoist", env: &[("ZIPP_NO_GUARD_HOIST", "1")] },
    Mode { name: "nodensebackedge", env: &[("ZIPP_NO_DENSE_BACKEDGE", "1")] },
    Mode { name: "notierc", env: &[("ZIPP_NO_FNJIT_MEM", "1")] },
    Mode { name: "nocallinline", env: &[("ZIPP_NO_CALL_INLINE", "1")] },
    Mode { name: "gcstress", env: &[("ZIPP_GC_STRESS", "1")] },
];

/// The subset the normal suite runs: the interpreter, both threshold shifts, and
/// the three switches whose emitters own the most register-allocation state.
const CI_MODES: &[&str] =
    &["base", "nojit", "thr1", "thr200", "nogprhomes", "noicgate", "nointsplice"];

fn mode(name: &str) -> &'static Mode {
    MODES.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("unknown mode {name}"))
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
        [Arr::Dense, Arr::Dense2, Arr::F64, Arr::I32, Arr::U8, Arr::Holey, Arr::Dbl, Arr::Str]
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
    Int { op: IntOp, a: Src, b: Src, n: u8 },
    BoolDef { k: u8, a: Src, b: Src, cmp: Cmp, neg: bool },
    BoolUse { k: u8, c: i32, style: u8 },
    Read { arr: Arr, idx: Idx, t: u8, coerce: u8 },
    Write { arr: Arr, idx: Idx, v: Src },
    ALen { arr: Arr },
    If { a: Src, b: Src, cmp: Cmp, neg: bool, then_: Vec<Stmt>, else_: Vec<Stmt> },
    Loop { var: char, n: u32, label: Option<u32>, body: Vec<Stmt> },
    Break { label: Option<u32>, at: Cond },
    Continue { label: Option<u32>, at: Cond },
    Ret { at: Cond },
    Leaf { f: u8, a: Src, b: Src },
    Deep { a: Src },
    Closure { a: Src },
    Indirect { a: Src },
    GlobRw { k: u8, a: Src, write: bool },
    UpRw { a: Src },
    Dbl { k: u8, op: u8, a: Src, f: u8 },
    DblMix { k: u8, style: u8 },
    Prop { k: u8, poly: u32, write: bool },
    Deopt { kind: DeoptKind, at: Cond, k: u8 },
    Try { a: Src, body: Vec<Stmt> },
    /// Self-recursion — the ONLY shape that reaches Tier A.
    Rec { a: Src },
    /// A kernel-local object whose fields are read and written in the loop: the
    /// admission shape for object scalar replacement (`[jit] SROA region`).
    Sroa { f: u8, op: u8, a: Src },
}

#[derive(Clone, Debug)]
struct Program {
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
    0, 1, 2, 3, 5, 7, 11, 13, 17, 31, 32, 63, 255, 1024, 1000003, -1, -7, -2147483648,
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
    ]);
    let n = if big {
        rng.pick(&[12u32, 40, 120, 400, 400, 1200])
    } else {
        rng.pick(&[12u32, 40, 120, 400, 400])
    };
    // Total kernel iterations stay bounded so one program is always a
    // sub-millisecond release run: the point is crossing the OSR point many
    // times, not doing work.
    let cap: u32 = if big { 20_000 } else { 4_800 };
    let reps = *[1u32, 3, 12, 40].iter().filter(|r| n * **r <= cap).last().unwrap_or(&1);
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
    let budget = if big { 4 + rng.below(12) } else { 3 + rng.below(8) };
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
    } else if flavor == Flavor::Pressure || flavor == Flavor::Scan {
        gen_pressure_body(&mut g, flavor == Flavor::Scan)
    } else {
        let count = 2 + g.rng.below(5);
        gen_stmts(&mut g, count)
    };

    Program {
        strict: rng.chance(50),
        use_closure: rng.chance(35),
        n,
        reps,
        hoists,
        leaf_kinds,
        checksum: true,
        bound: match flavor {
            Flavor::Scan | Flavor::Sroa => Bound::N,
            Flavor::Pressure if rng.chance(85) => Bound::N,
            _ => bound,
        },
        body,
        trim: matches!(flavor, Flavor::Pressure | Flavor::Scan | Flavor::Sroa)
            && rng.chance(60),
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
        let cmp = g.rng.pick(&[Cmp::SEq, Cmp::SNe, Cmp::Lt, Cmp::Gt, Cmp::Le, Cmp::Ge, Cmp::Eq]);
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
            g.rng.pick(&[Src::I, Src::I, Src::Temp(0), Src::Temp(0), Src::H])
        };
        let neg = !equality && g.rng.chance(40);
        defs.push(Stmt::BoolDef { k: k as u8, a, b: konst(k), cmp, neg });
    }
    // Strictly integer filler. `region_is_int` is all-or-nothing: one uncoerced
    // add, one `Math.max`, one f64 multiply anywhere in the loop and the region
    // declines to MEM, taking the whole shape out of reach of the INT emitter.
    let int_filler = |g: &mut Gen| -> Stmt {
        let op = g.rng.pick(&[IntOp::Add, IntOp::Add, IntOp::XorShl, IntOp::Imul]);
        let a = g.rng.pick(&[Src::I, Src::H, Src::Temp(0), Src::Lit(3)]);
        let n = g.rng.pick(&[1u8, 2, 3, 5]);
        Stmt::Int { op, a, b: Src::I, n }
    };

    let mut tail: Vec<Stmt> = Vec::new();
    for k in 0..nb {
        // Style 0 is `if (bk) …` — a real branch on the bool home, which is what
        // keeps the home live across the element read rather than folding it
        // into a select.
        // `if (bk) …` almost always: a branch keeps the bool in its gpr home
        // across the element read, where a select can fold it away.
        let style = if strict { 0 } else { g.rng.pick(&[0u8, 0, 0, 0, 0, 1, 2]) };
        tail.push(Stmt::BoolUse { k: k as u8, c: g.rng.pick(&[1i32, 2, 4, 8, 17]), style });
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
            then_: vec![Stmt::Int { op: IntOp::AddInt, a: Src::I, b: Src::I, n }],
            else_: Vec::new(),
        });
    }
    let op = g.rng.pick(&[IntOp::Add, IntOp::Add, IntOp::XorShl]);
    tail.push(Stmt::Int { op, a: Src::Temp(0), b: Src::I, n: 3 });

    if strict {
        // Nothing but the defs, one read, and the uses — see `Flavor::Scan`.
        tail.retain(|st| !matches!(st, Stmt::Int { .. }));
    }
    let mut body = defs;
    body.extend(tail);
    for r in 0..nreads {
        // Dense int Arrays carried the defect; the typed, holey and string twins
        // are the controls that must not move.
        let arr = if r == 0 {
            g.rng.pick(&[Arr::Dense, Arr::Dense, Arr::Dense, Arr::Dense2, Arr::I32, Arr::Str])
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
        let coerce = if strict { 0 } else { g.rng.pick(&[0u8, 0, 0, 0, 3, 2]) };
        // Mostly BETWEEN the definitions and the uses, which is where a scratched
        // register is observable; sometimes anywhere, which covers the variant
        // whose clobber is only seen after the back-edge.
        let at = if g.rng.chance(75) {
            nb + g.rng.below(body.len() + 1 - nb)
        } else {
            g.rng.below(body.len() + 1)
        };
        body.insert(at, Stmt::Read { arr, idx, t: r as u8, coerce });
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

fn gen_src(g: &mut Gen) -> Src {
    let mut pool: Vec<Src> = vec![
        Src::H,
        Src::I,
        Src::Hoist(g.rng.below(HOISTS) as u8),
        Src::Hoist(g.rng.below(HOISTS) as u8),
        Src::Temp(g.rng.below(TEMPS) as u8),
        Src::Lit(g.rng.pick(&[0i32, 1, 2, 3, 7, 255, -1, 65535, 1000003, -2147483648])),
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

fn gen_idx(g: &mut Gen) -> Idx {
    let base = {
        let mut pool = vec![Src::I, Src::I, Src::H, Src::Hoist(g.rng.below(HOISTS) as u8)];
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
        3 => Idx::Mul(base, g.rng.pick(&[3i32, 5, 7]), g.rng.pick(&[15u32, 31, 63])),
        _ => Idx::Mask(base, g.rng.pick(&[7u32, 15, 31, 63])),
    }
}

fn gen_cond(g: &mut Gen, kind: u8) -> Cond {
    // kind 0 = continue (may fire often), 1 = break, 2 = return / deopt (must
    // fire LATE, past the OSR point, or the region never gets hot).
    let src = match g.rng.below(if g.depth >= 1 { 4 } else { 3 }) {
        0 => Src::I,
        1 => Src::H,
        2 => Src::I,
        _ => Src::J,
    };
    match kind {
        0 => Cond { src, mask: g.rng.pick(&[3u32, 7, 15]), val: g.rng.below(4) as u32 },
        1 => Cond { src, mask: g.rng.pick(&[15u32, 31, 63]), val: g.rng.below(8) as u32 },
        _ => {
            if g.rng.chance(60) {
                Cond { src: Src::I, mask: 0, val: g.rng.pick(&[5u32, 9, 17, 33, 65, 200]) }
            } else {
                Cond { src, mask: g.rng.pick(&[127u32, 255]), val: g.rng.pick(&[100u32, 200]) }
            }
        }
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
        out.push(Stmt::Int { op: IntOp::Add, a: Src::I, b: Src::I, n: 1 });
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
            Stmt::Int { op, a, b, n: g.rng.pick(&[1u8, 2, 3, 5, 8, 13, 31]) }
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
            } else {
                // Writes always use a masked index so an array cannot grow
                // without bound; reads deliberately may go out of range.
                let idx = match gen_idx(g) {
                    Idx::Raw(s) | Idx::Minus(s, _) => Idx::Mask(s, 63),
                    other => other,
                };
                Stmt::Write { arr, idx, v: gen_src(g) }
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
                Stmt::DblMix { k: g.rng.below(DBLS) as u8, style: g.rng.below(3) as u8 }
            }
        }
        4 => match g.rng.below(4) {
            0 => Stmt::Deep { a: gen_src(g) },
            1 => Stmt::Closure { a: gen_src(g) },
            2 => Stmt::Indirect { a: gen_src(g) },
            _ => Stmt::Leaf { f: g.rng.below(LEAFS) as u8, a: gen_src(g), b: gen_src(g) },
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
                0 | 1 => Stmt::Continue { label: lbl, at: gen_cond(g, 0) },
                2 | 3 => Stmt::Break { label: lbl, at: gen_cond(g, 1) },
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
            Stmt::Deopt { kind, at: gen_cond(g, 2), k: g.rng.below(TEMPS) as u8 }
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
            Stmt::Loop { var, n, label, body }
        }
        9 => {
            let cmp =
                g.rng.pick(&[Cmp::Lt, Cmp::Le, Cmp::Gt, Cmp::Ge, Cmp::Eq, Cmp::Ne, Cmp::SEq]);
            let a = gen_cmp_src(g);
            let b = gen_cmp_src(g);
            let neg = g.rng.chance(45);
            g.depth += 1;
            let nt = 1 + g.rng.below(2);
            let then_ = gen_stmts(g, nt);
            let want_else = g.rng.chance(35);
            let ne = 1 + g.rng.below(2);
            let else_ = if want_else { gen_stmts(g, ne) } else { Vec::new() };
            g.depth -= 1;
            Stmt::If { a, b, cmp, neg, then_, else_ }
        }
        10 => {
            g.depth += 1;
            let nb = 1 + g.rng.below(2);
            let body = gen_stmts(g, nb);
            g.depth -= 1;
            Stmt::Try { a: gen_src(g), body }
        }
        _ => {
            if g.rng.chance(35) {
                Stmt::Rec { a: gen_src(g) }
            } else {
                let f = g.rng.below(3) as u8;
                let op = g.rng.below(3) as u8;
                Stmt::Sroa { f, op, a: gen_src(g) }
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
    labels: Vec<u32>,
}

impl Used {
    fn everything() -> Used {
        Used {
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
            labels: Vec::new(),
        }
    }
    fn src(&mut self, s: Src) {
        match s {
            Src::Hoist(k) => self.hoists[k as usize] = true,
            Src::Temp(k) => self.temps[k as usize] = true,
            Src::Glob(k) => self.globs[k as usize] = true,
            Src::Up => self.up = true,
            Src::ALen => self.arrs[Arr::Dense.ix()] = true,
            _ => {}
        }
    }
    fn idx(&mut self, i: Idx) {
        match i {
            Idx::Mask(s, _) | Idx::Mod(s, _) | Idx::Raw(s) | Idx::Minus(s, _) | Idx::Mul(s, _, _) => {
                self.src(s)
            }
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
        return u;
    }
    let mut u = Used::default();
    collect_stmts(&prog.body, &mut u);
    collect_labels(&prog.body, &mut u.labels);
    if let Bound::ArrLen(a) = prog.bound {
        u.arrs[a.ix()] = true;
    }
    u.close(prog);
    u
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
            Stmt::If { a, b, then_, else_, .. } => {
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

fn s_txt(p: &str, s: Src) -> String {
    match s {
        Src::H => "h".into(),
        Src::I => "i".into(),
        Src::J => "j".into(),
        Src::Q => "q".into(),
        Src::Hoist(k) => format!("c{k}"),
        Src::Temp(k) => format!("(t{k} | 0)"),
        Src::Glob(k) => format!("{p}g{k}"),
        Src::Up => format!("{p}up"),
        Src::ALen => format!("{p}arr.length"),
        Src::Lit(v) => format!("({v})"),
    }
}

fn idx_txt(p: &str, i: Idx) -> String {
    match i {
        Idx::Mask(s, m) => format!("({} & {})", s_txt(p, s), m),
        Idx::Mod(s, m) => format!("({} % {})", s_txt(p, s), m),
        Idx::Raw(s) => s_txt(p, s),
        Idx::Minus(s, d) => format!("({} - {})", s_txt(p, s), d),
        Idx::Mul(s, a, m) => format!("(({} * {}) & {})", s_txt(p, s), a, m),
    }
}

fn cmp_txt(p: &str, a: Src, b: Src, cmp: Cmp, neg: bool) -> String {
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

fn cond_txt(p: &str, c: Cond) -> String {
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

fn emit_stmt(o: &mut String, ind: usize, p: &str, st: &Stmt) {
    match st {
        Stmt::Int { op, a, b, n } => {
            let a = s_txt(p, *a);
            let b = s_txt(p, *b);
            let s = match op {
                IntOp::Add => format!("h = (h + {a}) | 0;"),
                IntOp::AddInt => format!("h = (h + {n}) | 0;"),
                IntOp::Sub => format!("h = (h - {a}) | 0;"),
                IntOp::Mul => format!("h = (h * {a}) | 0;"),
                IntOp::Imul => format!("h = Math.imul(h, {a}) | 0;"),
                IntOp::XorShl => format!("h = (h ^ ({a} << {n})) | 0;"),
                IntOp::XorShr => format!("h = (h ^ ({a} >>> {n})) | 0;"),
                IntOp::XorSar => format!("h = (h ^ ({a} >> {n})) | 0;"),
                IntOp::Or => format!("h = (h | {a}) | 0;"),
                IntOp::And => format!("h = (h & {a}) | 0;"),
                IntOp::Div => format!("h = (h / {a}) | 0;"),
                IntOp::Mod => format!("h = (h % {a}) | 0;"),
                IntOp::Ternary => format!("h = ({a} < {b} ? h + {n} : h - {n}) | 0;"),
                IntOp::Clz => format!("h = (h ^ Math.clz32({a})) | 0;"),
                IntOp::MinMax => format!("h = (h + Math.max({a}, Math.min({b}, {n}))) | 0;"),
                IntOp::AddRaw => format!("h = h + {a};"),
                IntOp::SubRaw => format!("h = h - {a};"),
                IntOp::IncRaw => format!("h += {n};"),
            };
            line(o, ind, &s);
        }
        Stmt::BoolDef { k, a, b, cmp, neg } => {
            line(o, ind, &format!("b{k} = {};", cmp_txt(p, *a, *b, *cmp, *neg)));
        }
        Stmt::BoolUse { k, c, style } => {
            let s = match style {
                0 => format!("if (b{k}) h = (h + {c}) | 0;"),
                1 => format!("h = (h + (b{k} ? {c} : 1)) | 0;"),
                _ => format!("h = (h ^ (b{k} ? {c} : 0)) | 0;"),
            };
            line(o, ind, &s);
        }
        Stmt::Read { arr, idx, t, coerce } => {
            let ix = idx_txt(p, *idx);
            let load = if *arr == Arr::Str {
                format!("t{t} = {p}str.charCodeAt({ix});")
            } else {
                format!("t{t} = {p}{}[{ix}];", arr.name())
            };
            line(o, ind, &load);
            let s = match coerce {
                0 => format!("h = (h + (t{t} | 0)) | 0;"),
                1 => format!("h = (h ^ ((t{t} * 1024) | 0)) | 0;"),
                2 => format!("h = (h + (t{t} === undefined ? 17 : (t{t} | 0))) | 0;"),
                _ => format!("h = Math.imul(h ^ (t{t} | 0), 16777619) | 0;"),
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
            line(o, ind, &format!("h = (h + {p}{}.length) | 0;", arr.name()));
        }
        Stmt::If { a, b, cmp, neg, then_, else_ } => {
            line(o, ind, &format!("if ({}) {{", cmp_txt(p, *a, *b, *cmp, *neg)));
            emit_stmts(o, ind + 2, p, then_);
            if else_.is_empty() {
                line(o, ind, "}");
            } else {
                line(o, ind, "} else {");
                emit_stmts(o, ind + 2, p, else_);
                line(o, ind, "}");
            }
        }
        Stmt::Loop { var, n, label, body } => {
            let lbl = label.map(|l| format!("L{l}: ")).unwrap_or_default();
            line(o, ind, &format!("{lbl}for ({var} = 0; {var} < {n}; {var}++) {{"));
            emit_stmts(o, ind + 2, p, body);
            line(o, ind, "}");
        }
        Stmt::Break { label, at } => {
            let l = label.map(|l| format!(" L{l}")).unwrap_or_default();
            line(o, ind, &format!("if ({}) break{l};", cond_txt(p, *at)));
        }
        Stmt::Continue { label, at } => {
            let l = label.map(|l| format!(" L{l}")).unwrap_or_default();
            line(o, ind, &format!("if ({}) continue{l};", cond_txt(p, *at)));
        }
        Stmt::Ret { at } => {
            line(o, ind, &format!("if ({}) return h | 0;", cond_txt(p, *at)));
        }
        Stmt::Leaf { f, a, b } => {
            line(
                o,
                ind,
                &format!("h = (h + {p}leaf{f}({}, {})) | 0;", s_txt(p, *a), s_txt(p, *b)),
            );
        }
        Stmt::Deep { a } => {
            line(o, ind, &format!("h = (h + {p}deep({})) | 0;", s_txt(p, *a)));
        }
        Stmt::Closure { a } => {
            line(o, ind, &format!("h = (h + {p}cl({})) | 0;", s_txt(p, *a)));
        }
        Stmt::Indirect { a } => {
            line(o, ind, &format!("h = (h + {p}fnref({}, 3)) | 0;", s_txt(p, *a)));
        }
        Stmt::GlobRw { k, a, write } => {
            if *write {
                line(o, ind, &format!("{p}g{k} = ({p}g{k} + {}) | 0;", s_txt(p, *a)));
            }
            line(o, ind, &format!("h = (h ^ ({p}g{k} | 0)) | 0;"));
        }
        Stmt::UpRw { a } => {
            line(o, ind, &format!("{p}up = ({p}up + {}) | 0;", s_txt(p, *a)));
            line(o, ind, &format!("h = (h ^ {p}up) | 0;"));
        }
        Stmt::Dbl { k, op, a, f } => {
            let fv = ["0.5", "1.5", "0.25", "3.5", "1.0009765625", "-0.75"][*f as usize % 6];
            let a = s_txt(p, *a);
            let s = match op {
                0 => format!("d{k} = d{k} * 0.5 + {a};"),
                1 => format!("d{k} = {a} * {fv};"),
                2 => format!("d{k} = Math.sqrt(Math.abs(d{k})) + {fv};"),
                3 => format!("d{k} = d{k} / 3 + {a};"),
                _ => format!("d{k} = Math.fround(d{k} * 0.5 + {fv});"),
            };
            line(o, ind, &s);
        }
        Stmt::DblMix { k, style } => {
            let s = match style {
                0 => format!("h = (h + ((d{k} * 1024) | 0)) | 0;"),
                1 => format!("h = (h ^ (Math.floor(d{k}) | 0)) | 0;"),
                _ => format!("h = (h + (d{k} > 100 ? 7 : 3)) | 0;"),
            };
            line(o, ind, &s);
        }
        Stmt::Prop { k, poly, write } => {
            let sel = if *poly == 0 {
                format!("{p}objs[0]")
            } else {
                format!("{p}objs[(i & {poly})]")
            };
            if *write {
                line(o, ind, &format!("{sel}.f{k} = (h & 63);"));
            }
            line(o, ind, &format!("h = (h + ({sel}.f{k} | 0)) | 0;"));
        }
        Stmt::Deopt { kind, at, k } => {
            let body = match kind {
                DeoptKind::TempStr => format!("t{k} = \"sx\";"),
                DeoptKind::TempUndef => format!("t{k} = undefined;"),
                DeoptKind::TempObj => format!("t{k} = {p}objs[0];"),
                DeoptKind::ElemDouble => format!("{p}arr[3] = 0.5;"),
                DeoptKind::ElemStr => format!("{p}arr[5] = \"sx\";"),
                DeoptKind::ElemDelete => format!("delete {p}arr[7];"),
                DeoptKind::ArrShrink => format!("{p}arr2.length = 3;"),
                DeoptKind::ArrGrow => format!("{p}arr2.push(7);"),
                DeoptKind::GlobDouble => format!("{p}g{} = 1.5;", (*k as usize) % GLOBS),
                DeoptKind::ObjExtend => format!("{p}objs[0].zz = 4;"),
                DeoptKind::FnSwap => format!("{p}fnref = {p}leaf1;"),
                DeoptKind::TypedOob => format!("t{k} = {p}iarr[9999];"),
            };
            line(o, ind, &format!("if ({}) {{ {body} }}", cond_txt(p, *at)));
        }
        Stmt::Rec { a } => {
            line(o, ind, &format!("h = (h + {p}rec((({}) & 7) + 2)) | 0;", s_txt(p, *a)));
        }
        Stmt::Sroa { f, op, a } => {
            let fld = ["x", "y", "z"][*f as usize % 3];
            let txt = match op {
                0 => format!("{p}so.{fld} = ({p}so.{fld} + {}) | 0;", s_txt(p, *a)),
                1 => format!("{p}so.{fld} = ({p}so.{fld} ^ {}) | 0;", s_txt(p, *a)),
                _ => format!("h = (h + {p}so.{fld}) | 0;"),
            };
            line(o, ind, &txt);
        }
        Stmt::Try { a, body } => {
            line(o, ind, "try {");
            line(o, ind + 2, &format!("h = (h + {p}thrower({})) | 0;", s_txt(p, *a)));
            emit_stmts(o, ind + 2, p, body);
            line(o, ind, "} catch (e) {");
            line(o, ind + 2, "h = (h + 1) | 0;");
            line(o, ind, "}");
        }
    }
}

fn emit_stmts(o: &mut String, ind: usize, p: &str, ss: &[Stmt]) {
    for s in ss {
        emit_stmt(o, ind, p, s);
    }
}

const STR_LIT: &str = "the quick brown fox jumps over the lazy dog 0123456789 ABCDEF";

fn int_list(n: usize, f: impl Fn(usize) -> i64) -> String {
    (0..n).map(|i| f(i).to_string()).collect::<Vec<_>>().join(", ")
}

fn emit_leaf(o: &mut String, p: &str, k: usize, kind: u8) {
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

/// Emit the whole program. `p` prefixes every module-level binding so many
/// programs can share one file (that is how the node oracle batches them);
/// nothing else about the text depends on it.
fn emit(prog: &Program, p: &str, tag: &str) -> String {
    let u = collect(prog);
    let mut o = String::new();

    if u.arrs[Arr::Dense.ix()] {
        // Small values on purpose: `A[i] === 1` has to MATCH sometimes.
        line(&mut o, 0, &format!("var {p}arr = [{}];", int_list(32, |i| (i as i64 * 3) % 7)));
    }
    if u.arrs[Arr::Dense2.ix()] {
        line(&mut o, 0, &format!("var {p}arr2 = [{}];", int_list(32, |i| (i as i64 * 5) % 11)));
    }
    if u.arrs[Arr::F64.ix()] {
        line(
            &mut o,
            0,
            &format!(
                "var {p}farr = new Float64Array([{}]);",
                (0..32).map(|i| format!("{}.5", i * 2)).collect::<Vec<_>>().join(", ")
            ),
        );
    }
    if u.arrs[Arr::I32.ix()] {
        line(
            &mut o,
            0,
            &format!(
                "var {p}iarr = new Int32Array([{}]);",
                int_list(32, |i| if i % 4 == 3 { (i as i64 * 1103515245) as i32 as i64 } else { (i as i64 * 5) % 9 })
            ),
        );
    }
    if u.arrs[Arr::U8.ix()] {
        line(
            &mut o,
            0,
            &format!("var {p}uarr = new Uint8Array([{}]);", int_list(32, |i| (i as i64 * 11) % 13)),
        );
    }
    if u.arrs[Arr::Holey.ix()] {
        line(&mut o, 0, &format!("var {p}harr = [1, 2, 3, 4]; {p}harr[12] = 9;"));
    }
    if u.arrs[Arr::Dbl.ix()] {
        line(
            &mut o,
            0,
            &format!(
                "var {p}darr = [{}];",
                (0..32).map(|i| format!("{}.25", i * 3)).collect::<Vec<_>>().join(", ")
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
        line(&mut o, 0, &format!("var {p}objs = [{}];", shapes.join(", ")));
    }
    for (k, used) in u.globs.iter().enumerate() {
        if *used {
            line(&mut o, 0, &format!("var {p}g{k} = {};", (k as i64 + 1) * 11));
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
        line(
            &mut o,
            0,
            &format!(
                "function {p}rec(x) {{ if (x < 2) return x | 0; return ({p}rec(x - 1) + {p}rec(x - 2)) | 0; }}"
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

    // ── up / cl / kernel: module level, or nested inside main (upvalues) ──
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
    line(&mut inner, ind + 2, "var h = 1, i = 0, j = 0, q = 0;");
    for k in 0..TEMPS {
        if u.temps[k] {
            line(&mut inner, ind + 2, &format!("var t{k} = {};", k + 1));
        }
    }
    for k in 0..BOOLS {
        if u.bools[k] {
            line(&mut inner, ind + 2, &format!("var b{k} = false;"));
        }
    }
    for k in 0..DBLS {
        if u.dbls[k] {
            line(&mut inner, ind + 2, &format!("var d{k} = {}.5;", k + 1));
        }
    }
    for k in 0..HOISTS {
        if u.hoists[k] {
            line(&mut inner, ind + 2, &format!("var c{k} = {};", prog.hoists[k]));
        }
    }

    let bound_txt = match prog.bound {
        Bound::N => "n".to_string(),
        Bound::ArrLen(a) => format!("{p}{}.length", a.name()),
    };
    line(&mut inner, ind + 2, &format!("for (i = 0; i < {bound_txt}; i++) {{"));
    emit_stmts(&mut inner, ind + 4, p, &prog.body);
    line(&mut inner, ind + 2, "}");
    // Mix every live local into the answer, so a wrong value anywhere in the
    // frame is visible and not just a wrong `h`.
    let mut mix = vec!["h".to_string()];
    for k in 0..TEMPS {
        if u.temps[k] {
            mix.push(format!("((t{k} | 0) * {})", 3 + k));
        }
    }
    for k in 0..BOOLS {
        if u.bools[k] {
            mix.push(format!("(b{k} ? {} : 0)", 17 + k));
        }
    }
    for k in 0..DBLS {
        if u.dbls[k] {
            mix.push(format!("((d{k} * 1024) | 0)"));
        }
    }
    if u.sroa {
        mix.push(format!("({p}so.x | 0)"));
        mix.push(format!("({p}so.y | 0)"));
        mix.push(format!("({p}so.z | 0)"));
    }
    line(&mut inner, ind + 2, &format!("return ({}) | 0;", mix.join(" ^ ")));
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
    // A checksum over every mutable datum the body could have touched: side
    // effects that never reach `h` still have to agree across tiers.
    let mut tail = vec!["acc".to_string()];
    if prog.checksum {
        tail.push("s".to_string());
        line(&mut o, 2, "var s = 0;");
        for (zi, a) in Arr::all().into_iter().enumerate() {
            if !u.arrs[a.ix()] || a == Arr::Str {
                continue;
            }
            line(
                &mut o,
                2,
                &format!(
                    "for (var z{zi} = 0; z{zi} < {p}{n}.length; z{zi}++) s = (Math.imul(s, 31) + (({p}{n}[z{zi}] * 1024) | 0)) | 0;",
                    n = a.name()
                ),
            );
        }
        if u.objs {
            line(
                &mut o,
                2,
                &format!(
                    "for (var y = 0; y < {p}objs.length; y++) s = (Math.imul(s, 31) + ({p}objs[y].f0 | 0) + ({p}objs[y].f1 | 0) + ({p}objs[y].f2 | 0)) | 0;"
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
    line(&mut o, 2, &format!("return ({}) | 0;", tail.join(" ^ ")));
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
                let src = emit(&prog, "", &format!("D{i}"));
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
            print!("{}", emit(&prog, "", "D"));
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
                o.output.iter().find(|l| l.starts_with(tag)).cloned().unwrap_or_else(|| {
                    format!("{tag} NO-OUTPUT({})", o.output.len())
                })
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
    cmd.args(["fuzz_child", "--exact", "--nocapture", "--test-threads", "1"])
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
    for (k, v) in m.env {
        cmd.env(k, v);
    }
    if jitlog {
        cmd.env("ZIPP_JITLOG", "1");
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
    let out = rx1.recv_timeout(Duration::from_secs(20)).unwrap_or_default();
    let err = rx2.recv_timeout(Duration::from_secs(20)).unwrap_or_default();
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
    let out = spawn_job(&format!("batch:{seed}:{lo}:{hi}"), m, Duration::from_secs(secs), false);
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
        let src = emit(&prog, "", "D");
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
        if !l.starts_with("[jit]") && !l.starts_with("[leaf]") {
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
        .take(14)
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

        // 3. loop bounds
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

        // 4. structural knobs
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
            Stmt::If { then_, else_, .. } => {
                remove_nth(then_, k, idx) || remove_nth(else_, k, idx)
            }
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

struct Divergence {
    index: u64,
    source: String,
    digests: Vec<(String, String)>,
    /// What the JIT decided for the minimized program, from `ZIPP_JITLOG=1`.
    /// A wrong answer without this is a bug report that still needs an
    /// afternoon; with it, the tier is already named.
    trace: Vec<String>,
}

fn describe(d: &Divergence, seed: u64) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "\n═══ TIER DIVERGENCE  seed={seed} index={}  ═══\n",
        d.index
    ));
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

        if verbose {
            eprintln!("[fuzz] index {i}: {} != {} — minimizing…", a.name, b.name);
        }
        let mut evals = 0usize;
        let mut check = |p: &Program| {
            if evals > 900 {
                return false;
            }
            evals += 1;
            let src = emit(p, "", "D");
            let da = single_digest(&src, a);
            let db = single_digest(&src, b);
            da != db
        };
        let base_src = emit(&prog, "", "D");
        let mut minimal = prog.clone();
        if single_digest(&base_src, a) != single_digest(&base_src, b) {
            minimal = shrink(&prog, &mut check);
        } else if verbose {
            eprintln!(
                "[fuzz] index {i}: NOT reproducible standalone — batch-order dependent, reporting unshrunk"
            );
        }
        let src = emit(&minimal, "", "D");
        let digests: Vec<(String, String)> = MODES
            .iter()
            .map(|m| (m.name.to_string(), single_digest(&src, m)))
            .collect();
        let trace = tier_trace(&src);
        out.push(Divergence { index: i, source: src, digests, trace });
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
    // The live-out `Bool` defect, this time on the DOUBLE tier over a
    // double-element Array. Four different answers for one program: 0x3b from
    // the interpreter and node, 0x1f at the default threshold, 0x11b at
    // `ZIPP_JIT_THRESHOLD=1`, 0x2d under `ZIPP_NO_FUSED_CMPJUMP=1` — with `b2`
    // reading back as NaN in two of them.
    (0x5A17_2026_0F1E_2D3C, 392, "open_bool_local_reads_back_as_nan"),
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
    const COUNT: u64 = 500;
    let modes = selected_modes(CI_MODES);
    let found: Vec<Divergence> = sweep(SEED, 0, COUNT, &modes, false, false)
        .into_iter()
        .filter(|d| !KNOWN_OPEN.iter().any(|&(s, i, _)| s == SEED && i == d.index))
        .collect();
    assert!(
        found.is_empty(),
        "{} of {COUNT} generated programs answer differently across tiers:{}",
        found.len(),
        found.iter().map(|d| describe(d, SEED)).collect::<String>()
    );
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
    let count: u64 =
        std::env::var("ZIPP_FUZZ_COUNT").ok().and_then(|s| s.parse().ok()).unwrap_or(2000);
    let start: u64 = std::env::var("ZIPP_FUZZ_START").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
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
            "[fuzz] {}/{} programs, {} divergent, {:.1}s",
            i - start,
            count,
            all.len(),
            t0.elapsed().as_secs_f64()
        );
    }
    assert!(all.is_empty(), "{} divergent programs (see above)", all.len());
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
    let count: u64 =
        std::env::var("ZIPP_FUZZ_NODE_COUNT").ok().and_then(|s| s.parse().ok()).unwrap_or(96);
    let big = false;

    let mut batch = String::new();
    for i in 0..count {
        let prog = gen_program(prog_seed(SEED, i), big);
        batch.push_str(&emit(&prog, &format!("p{i}_"), &format!("D{i}")));
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
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    let node: BTreeMap<u64, String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(k, v)| {
            k.strip_prefix('D').and_then(|s| s.parse::<u64>().ok()).map(|i| (i, v.to_string()))
        })
        .collect();
    assert_eq!(node.len() as u64, count, "node did not report every program");

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
                emit(&prog, "", "D")
            ));
        }
    }
    assert!(bad.is_empty(), "zipp disagrees with node on {} programs:{}", bad.len(), bad.concat());
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
#[test]
#[ignore = "OPEN: a Bool local reads back as NaN after its region compiles"]
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
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let node = Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (expected values come from `node -e`)");
    assert!(node.status.success(), "node failed: {}", String::from_utf8_lossy(&node.stderr));
    let want: Vec<String> =
        String::from_utf8_lossy(&node.stdout).lines().map(|l| l.to_string()).collect();
    assert_eq!(out.output, want, "zipp != node for:{src}");
}

/// OPEN #1, second face: the same live-out `Bool` comes back `false` instead of
/// `true` once the region is re-compiled on the DOUBLE tier.
///
/// Sharper than the NaN case because the boundary is visible: `kernel(12)`
/// returns 23 on the first 22 calls and 4 on every call after, with nothing
/// about the program changing in between. `ZIPP_NO_FUSED_CMPJUMP=1` returns 5
/// from the same point — a third answer for the same program.
#[test]
#[ignore = "OPEN: a live-out Bool local reads back false after its region is re-compiled"]
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
#[test]
#[ignore = "OPEN: a compiled nested loop drops inner iterations"]
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
#[test]
#[ignore = "OPEN: a fused double compare in a nested loop takes the wrong branch"]
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

/// OPEN #2, found by this harness on the tree it was written against.
///
/// An out-of-range element read that sits in a COLD block of a compiled loop
/// throws `TypeError: cannot read property of undefined` instead of yielding
/// `undefined`. The receiver is a hoisted/pinned global (`pins=1/1` in the
/// JITLOG); on the cold path its register is not the array, so the region deopts
/// at that ip and the interpreter resumes reading a property of `undefined`.
///
/// Reproduces at HEAD 6ed29ac and in EVERY compiled mode — no `ZIPP_NO_*` switch
/// avoids it, only `ZIPP_NOJIT=1` and a threshold high enough that the loop never
/// compiles. It is not typed-array-specific: a plain dense Array, an
/// `Int32Array` and a `Float64Array` all throw.
///
/// Load-bearing ingredients, each confirmed by removing it: the receiver must be
/// a GLOBAL (a parameter is fine), the read must be in a conditional block (the
/// same read on every iteration is fine), and the index must be out of range (an
/// in-range one is fine). `a[33]` on a 32-element array throws just as `a[9999]`
/// does.
#[test]
#[ignore = "OPEN: a cold out-of-range element read throws TypeError instead of yielding undefined"]
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

/// The generator must never emit anything whose answer is implementation-defined
/// — a fuzzer with false positives gets ignored, which is worse than no fuzzer.
/// This is the lint that keeps a later edit from reintroducing one.
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
        let src = emit(&prog, "", "D");
        for b in BANNED {
            assert!(!src.contains(b), "generator emitted banned construct {b:?}:\n{src}");
        }
        // Every reported number is funnelled through ToInt32, so no answer can
        // depend on Number→String formatting.
        assert!(
            src.contains(">>> 0).toString(16)"),
            "the digest must be an unsigned int32 in hex"
        );
        assert!(!src.contains("while ("), "only counted `for` loops terminate by construction");
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
        let a = emit(&gen_program(prog_seed(42, i), false), "", "D");
        let b = emit(&gen_program(prog_seed(42, i), false), "", "D");
        assert_eq!(a, b, "generator is not deterministic at index {i}");
        let c = emit(&gen_program(prog_seed(43, i), false), "", "D");
        assert_ne!(a, c, "different seeds produced the same program at index {i}");
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
    let count: u64 =
        std::env::var("ZIPP_FUZZ_COUNT").ok().and_then(|s| s.parse().ok()).unwrap_or(400);
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
    eprintln!("[fuzz] tier coverage over {count} programs (seed {seed}):");
    let mut rows: Vec<_> = tally.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (k, n) in rows {
        eprintln!("    {n:>7}  {k}");
    }
}

fn classify_jitlog(l: &str) -> Option<String> {
    let l = l.trim();
    let (rest, kind) = if let Some(r) = l.strip_prefix("[jit] ") {
        (r, "jit")
    } else if let Some(r) = l.strip_prefix("[leaf] ") {
        (r, "leaf")
    } else if let Some(r) = l.strip_prefix("[mi] ") {
        (r, "mi")
    } else {
        return None;
    };
    // Collapse the numeric detail: what matters is WHICH decision, not where.
    let body: String = rest.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
    let body = body.split(" (").next().unwrap_or(&body).to_string();
    let body = body.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
    Some(format!("[{kind}] {body}"))
}
