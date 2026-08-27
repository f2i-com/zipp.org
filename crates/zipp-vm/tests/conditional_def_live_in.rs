//! W18 BUG: a local whose only in-region definition sits on a CONDITIONAL
//! branch lost its entry load, so the compiled body read its home as garbage on
//! every pass that skipped the branch.
//!
//! ```js
//! function k(n){ var h=1,i=0,t=2;
//!   for (i=0;i<n;i++){ if (i===3) { t=7; } h=(h+t)|0; }
//!   return h; }
//! k(40)   // node & ZIPP_NOJIT=1: 266.   compiled: 74.   ZIPP_NO_GPR_HOMES=1: 42.
//! ```
//!
//! THE DEFECT, and why it is a CLASS. `plan_region.rs` builds `first_seen[r]`,
//! which records whether a register's FIRST OCCURRENCE inside the region is a
//! def. Two consumers read that flag as though it meant "a def of `r` dominates
//! every use of `r`":
//!
//!   * `shareable(r)` — a shareable register may share an xmm home with another
//!     value AND is dropped from `live_in_regs`, whose invariant is "every
//!     flushed home is entry-loaded". So its home starts as whatever the last
//!     region left in that xmm.
//!   * `range(r)` — a non-live-in register gets a narrow `[first, last]` home
//!     interval instead of the whole region.
//!
//! "The first occurrence is a def" is a TEXTUAL fact. In the kernel above the
//! region is `[5, 17]`, `t` is r5, and its first occurrence is the `LoadInt` at
//! ip 9 — behind the `JumpIfFalse` at ip 8. The path 5 → 8 → 11 reaches the
//! `Add` at ip 11 without ever executing the def. Both wrong answers above are
//! that one uninitialised home, read on 39 of 40 iterations.
//!
//! THE FIX is the shape W16 used for the same confusion one level down (mention
//! window vs live range): state the fact ONCE, from real dataflow.
//! `region_liveness` — the backward walk that already produced the live spans —
//! now also returns the region's true live-in set (`live_in[s]`), and a single
//! `live_in(r)` predicate answers both consumers. It is UNIONed with the old
//! flag rather than replacing it, so the change can only ever move a register
//! from "shareable" to "permanent + entry loaded", never the other way.
//!
//! WHY NOTHING CAUGHT IT FOR SO LONG. Reading the local AFTER the loop made the
//! program answer correctly: `read_outside` forced a permanent home with an
//! entry load. Every hand-written test, every benchmark row and the tier
//! fuzzer's own return mix read their accumulators afterwards, which is why
//! 138,300 generated programs missed it. Hence the `after` axis below: every
//! case is generated BOTH ways, and the `dead_out` half is the half that failed.
//!
//! It also had a second face. W17's GPR write-through sharing (default-on since
//! W18, `ZIPP_NO_GPR_WT_SHARE=1` turns it off) makes `read_outside` registers
//! shareable too — which is exactly why that mechanism had to ship dark: it
//! made this defect reachable on the `after` half as well. Both halves run
//! under both positions of that switch in
//! [`conddef_all_modes_answer_identically`].
//!
//! SHAPE OF THIS FILE. Every case is asserted against `node -e`, never against
//! `ZIPP_NOJIT=1` — a planner bug that also existed in the interpreter would
//! pass that. Three axes:
//!
//!   * the non-dominating DEF SHAPE (`DEFS`) — a branch that fires sometimes,
//!     one that never fires (the cold-block spelling, which survives every
//!     `ZIPP_NO_*` switch), an `else` arm, a zero-trip inner loop, and — as
//!     controls that must keep working — an `if/else` that DOES dominate and an
//!     unconditional def;
//!   * the TIER (`INT` int32 accumulator vs `DOUBLE` f64 accumulator), because
//!     the two register emitters gave two DIFFERENT wrong answers;
//!   * `after` — whether the local is read once the loop is over.
//!
//! [`conddef_mechanism_*`] reads the tier back out of a child's `ZIPP_JITLOG`,
//! so an admission change that quietly drops these kernels to the MEM tier
//! fails the suite instead of making it vacuous.

use std::process::Command;

// ── oracles ─────────────────────────────────────────────────────────────────

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The same program's output from `node -e`, so expectations are neither
/// hand-computed nor taken from our own interpreter.
fn node_output(src: &str) -> Vec<String> {
    let out = Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (expected values come from `node -e`)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(tag: &str, src: &str) {
    assert_eq!(
        run_ok(src),
        node_output(src),
        "[{tag}] zipp != node for:\n{src}"
    );
}

// ── the matrix ──────────────────────────────────────────────────────────────

/// One way to write a def of `t` that does NOT dominate the use below it.
///
/// `stmt` runs per iteration, before `t` is folded into the accumulator. Every
/// entry must leave `t` numeric on every path (a `String` or `undefined` `t`
/// would decline the region and make the case vacuous), so each relies on the
/// initialiser the kernel writes before the loop.
struct Def {
    tag: &'static str,
    stmt: &'static str,
    /// Does a def of `t` run on EVERY iteration? The `false` rows are the bug;
    /// the `true` rows are controls that must not regress.
    dominates: bool,
}

const DEFS: &[Def] = &[
    // The minimized repro: fires on exactly one iteration of forty.
    Def {
        tag: "eq3",
        stmt: "if (i === 3) { t = 7; }",
        dominates: false,
    },
    // The cold-block spelling — the branch the interpreter NEVER takes, so the
    // block is `cold` in the plan. This is the one an engine author is likeliest
    // to reason wrongly about ("it never runs, so it cannot matter") and the one
    // that survives every `ZIPP_NO_*` switch unchanged.
    Def {
        tag: "never",
        stmt: "if (i === 100000) { t = 7; }",
        dominates: false,
    },
    // Fires on all but the first iteration — so the home is garbage exactly
    // once, which is the hardest version to see in an answer.
    Def {
        tag: "gt0",
        stmt: "if (i > 0) { t = (i + 1) | 0; }",
        dominates: false,
    },
    // Fires every other iteration.
    Def {
        tag: "even",
        stmt: "if ((i & 1) === 0) { t = (i + 2) | 0; }",
        dominates: false,
    },
    // The def on the ELSE arm, with an empty consequent.
    Def {
        tag: "elsearm",
        stmt: "if (i > 100000) { } else { t = (i + 3) | 0; }",
        dominates: false,
    },
    // A zero-trip inner loop: `t`'s def is inside a nested region that runs on
    // half the outer iterations and not at all on the rest.
    Def {
        tag: "innerloop",
        stmt: "for (var j = 0; j < (i & 1); j++) { t = (j + 5) | 0; }",
        dominates: false,
    },
    // Two conditional defs that between them cover every path but STILL do not
    // dominate on the first pass through a `switch`-like chain.
    Def {
        tag: "twoarm",
        stmt: "if (i === 2) { t = 11; } if (i === 6) { t = 13; }",
        dominates: false,
    },
    // ── controls: these DO dominate and must keep answering correctly ──
    Def {
        tag: "ifelse",
        stmt: "if ((i & 1) === 0) { t = 4; } else { t = 9; }",
        dominates: true,
    },
    Def {
        tag: "uncond",
        stmt: "t = (i & 3) | 0;",
        dominates: true,
    },
];

/// The INT kernel: everything stays int32, so `region_is_int` holds and the
/// region reaches `region_int.rs` / `region_int_gpr.rs`.
///
/// `after` decides whether `t` is read once the loop is over — the axis that
/// hid the defect, because a read-after-region register used to be pinned to a
/// permanent home with an entry load.
fn int_case(d: &Def, after: bool) -> String {
    let tail = if after {
        "return h + t * 1000;"
    } else {
        "return h;"
    };
    format!(
        "function kernel(n) {{\n  \
           var h = 1, i = 0, t = 2, j = 0;\n  \
           for (i = 0; i < n; i++) {{\n    \
             {}\n    \
             h = (h + t) | 0;\n  \
           }}\n  \
           {tail}\n\
         }}\n\
         console.log(kernel(9), kernel(40), kernel(400));\n",
        d.stmt
    )
}

/// The DOUBLE kernel: the accumulator is an f64, so the region declines INT and
/// lands on `regalloc.rs` — the emitter that gave the OTHER wrong answer.
fn double_case(d: &Def, after: bool) -> String {
    let tail = if after {
        "return h + t * 0.125;"
    } else {
        "return h;"
    };
    format!(
        "function kernel(n) {{\n  \
           var h = 1.5, i = 0, t = 2, j = 0;\n  \
           for (i = 0; i < n; i++) {{\n    \
             {}\n    \
             h = h * 0.5 + t;\n  \
           }}\n  \
           {tail}\n\
         }}\n\
         console.log(kernel(9), kernel(40), kernel(400));\n",
        d.stmt
    )
}

/// Names the case in a failure message, and says which half of the matrix it is
/// in: `nondom` rows are the defect, `dom` rows are the controls beside it.
fn tag_of(d: &Def, after: bool) -> String {
    format!(
        "{}-{}-{}",
        d.tag,
        if d.dominates { "dom" } else { "nondom" },
        if after { "after" } else { "deadout" }
    )
}

// ── the parity sweeps ───────────────────────────────────────────────────────

/// The INT tier: `k(40)` answered 74 (xmm INT) / 42 (GPR INT) instead of 266.
#[test]
fn conddef_parity_int_matrix() {
    for d in DEFS {
        for after in [false, true] {
            assert_matches_node(&format!("int-{}", tag_of(d, after)), &int_case(d, after));
        }
    }
}

/// The DOUBLE tier, where the same plan defect surfaces through `regalloc.rs`.
#[test]
fn conddef_parity_double_matrix() {
    for d in DEFS {
        for after in [false, true] {
            assert_matches_node(&format!("dbl-{}", tag_of(d, after)), &double_case(d, after));
        }
    }
}

/// The matrix must keep BOTH halves: the non-dominating rows are the defect and
/// the dominating ones are the controls that prove the fix did not simply turn
/// every register permanent. A future edit that deletes one half fails here.
#[test]
fn conddef_matrix_covers_both_halves() {
    let nondom = DEFS.iter().filter(|d| !d.dominates).count();
    let dom = DEFS.iter().filter(|d| d.dominates).count();
    assert!(
        nondom >= 6,
        "the defect half of the matrix shrank to {nondom} rows"
    );
    assert!(
        dom >= 2,
        "the control half of the matrix shrank to {dom} rows"
    );
}

/// The exact two-line case from the W17 report, kept verbatim so the number in
/// the module header stays checkable by eye.
#[test]
fn conddef_minimized_repro_answers_266() {
    const SRC: &str = r#"
function k(n){var h=1,i=0,t=2;for(i=0;i<n;i++){if(i===3){t=7;}h=(h+t)|0;}return h;}
console.log(k(40));
"#;
    assert_matches_node("minimized", SRC);
    assert_eq!(run_ok(SRC), vec!["266".to_string()]);
}

/// A conditional def of a value the region carries ACROSS the back-edge: `t`
/// keeps whatever the last firing iteration wrote, so a lost entry load is
/// visible even on iterations that do run the def. This is the shape a plain
/// `[first mention, last mention]` window gets most wrong.
#[test]
fn conddef_value_carried_across_the_backedge() {
    const SRC: &str = r#"
function kernel(n) {
  var h = 0, i = 0, t = 3;
  for (i = 0; i < n; i++) {
    if ((i % 5) === 0) { t = (t * 2 + 1) | 0; }
    h = (h + t) | 0;
  }
  return h;
}
console.log(kernel(7), kernel(31), kernel(120));
"#;
    assert_matches_node("carried", SRC);
}

/// Several non-dominating locals at once. One garbage home can be masked by an
/// accumulator that happens to swamp it; four cannot, and the count is also what
/// pushes the plan past the point where home SHARING starts — the mechanism
/// `shareable` gates.
#[test]
fn conddef_four_non_dominating_locals() {
    const SRC: &str = r#"
function kernel(n) {
  var h = 1, i = 0, a = 2, b = 3, c = 5, d = 7;
  for (i = 0; i < n; i++) {
    if (i === 3) { a = 11; }
    if (i === 100000) { b = 13; }
    if ((i & 1) === 0) { c = (i + 1) | 0; }
    if (i > 2) { d = (i * 2) | 0; }
    h = (h + a + b + c + d) | 0;
  }
  return h;
}
console.log(kernel(9), kernel(40), kernel(400));
"#;
    assert_matches_node("four-locals", SRC);
}

// ── mode sweep ──────────────────────────────────────────────────────────────

/// Every parity case again in a child process under each switch, because the
/// JIT switches are memoized latches — a mode IS a process.
///
/// `ZIPP_NO_GPR_WT_SHARE=1` is in the list on purpose: that mechanism is what
/// made this defect reachable on the `after` half of the matrix, so both of its
/// positions have to be green before it can be default-on.
#[test]
fn conddef_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 7] = [
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_NO_GPR_WT_SHARE", "1")],
        &[("ZIPP_NO_INT_SPLIT", "1")],
        &[("ZIPP_NO_GLOB_RANGE", "1")],
        &[("ZIPP_NO_FUSED_CMPJUMP", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = Command::new(&exe);
        cmd.arg("conddef_parity_");
        for (key, val) in mode {
            cmd.env(key, val);
        }
        let out = cmd.output().expect("spawn the test binary");
        assert!(
            out.status.success(),
            "mode {mode:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ── mechanism pins ──────────────────────────────────────────────────────────

fn jitlog_of(src: &str) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let out = Command::new(&exe)
        .arg("conddef_jitlog_child")
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_CONDDEF_SRC", src)
        .output()
        .expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "jitlog child failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}

/// The worker for [`jitlog_of`]. A no-op unless `ZIPP_CONDDEF_SRC` is set,
/// because the JIT switches are memoized latches: a mode IS a process.
#[test]
#[ignore = "worker: spawned by jitlog_of with ZIPP_CONDDEF_SRC set"]
fn conddef_jitlog_child() {
    let Some(src) = std::env::var_os("ZIPP_CONDDEF_SRC") else {
        return;
    };
    let _ = zipp_vm::run(&src.to_string_lossy()).expect("source compiles");
}

/// The INT matrix really does reach an INT emitter. Without this the whole file
/// could go green by quietly declining to the memory tier.
///
/// B210 housekeeping: both mechanism tests are gated out under `safe-sandbox`
/// — there is no JIT there, so no tier is reachable by construction (a latent
/// sandbox-build-matrix failure that batteries running only the sandbox LIB
/// tests never surfaced; pre-existing at c4666b4). The semantics matrices
/// above still run under the sandbox interpreter.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn conddef_mechanism_int_matrix_reaches_the_int_tier() {
    for d in DEFS {
        for after in [false, true] {
            let src = int_case(d, after);
            let log = jitlog_of(&src);
            assert!(
                log.contains("INT region fn1 ["),
                "int-{}: the kernel's region is not INT — the case no longer \
                 reaches the emitter it is testing:\n{log}",
                tag_of(d, after)
            );
        }
    }
}

/// The DOUBLE matrix really does reach `regalloc.rs`. (Sandbox-gated with the
/// INT twin above.)
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn conddef_mechanism_double_matrix_reaches_the_double_tier() {
    for d in DEFS {
        for after in [false, true] {
            let src = double_case(d, after);
            let log = jitlog_of(&src);
            assert!(
                log.contains("DOUBLE region fn1 ["),
                "dbl-{}: the kernel's region is not DOUBLE:\n{log}",
                tag_of(d, after)
            );
        }
    }
}
