//! W16 BUG: the region home-reuse allocator sized a value's home from where the
//! value was MENTIONED instead of from where it was LIVE, so a JS local defined
//! outside an inner loop and read inside it lost its home to a later temp and
//! read back garbage on every inner iteration but the first.
//!
//! THE DEFECT, and why it is a CLASS. `plan_region` builds `first_ip`/`last_ip`
//! by walking the region's instructions and recording every ip that names a
//! register. That is a live range only for STRAIGHT-LINE code. A region IS a
//! loop body, and it routinely contains an inner loop:
//!
//!     for (i = 0; i < n; i++) {
//!       d = 255 * -3;                                  // ip 10 — defines `d`
//!       for (j = 0; j < 2; j++)
//!         h = (h + ((d * 1024) | 0)) | 0;              // ip 17 — reads `d`
//!     }
//!
//! `d`'s mention window is `[10, 17]`, but `d` is live across the inner
//! back-edge at ip 26, so ips 18..26 all run while its home still matters. Once
//! numeric pressure passes the 14-home pool the linear-scan allocator engages,
//! sees the window close at 17, and hands the same xmm to the `| 0` literal at
//! ip 21. The first inner iteration reads `d`; every later one reads whatever
//! the literal left there.
//!
//! WHAT IT LOOKED LIKE. Two of the five wrong-answer classes the W16 tier
//! fuzzer found, one on each register tier, and neither looked like a register
//! bug:
//!
//!   * on the INT tier the clobbering value was the constant `0`, so an addend
//!     became zero and the loop looked like it was running FEWER ITERATIONS than
//!     the interpreter — `kernel(9)` returned exactly one addend short of
//!     -14100479, and the deficit tracked the compile threshold because it is
//!     one lost addend per outer iteration AFTER the region compiles;
//!   * on the DOUBLE tier the clobbering value was `h + (7|3)`, which reaches
//!     109, so `d1 > 100` — false on all 36 real evaluations — came out TRUE
//!     twice. `ZIPP_NO_FUSED_CMPJUMP=1` answered correctly, which framed the
//!     fused compare emitter; unfusing merely re-planned the allocation.
//!
//! THE FIX. `region_live_spans` (plan_region.rs) runs a backward liveness pass
//! over the region's own control flow (`region_succs`) and WIDENS the mention
//! windows with the result. Widening, not replacing: the use/def model is the
//! same partially-modelled one the windows are built from, so a range can only
//! ever grow relative to the shipping allocation.
//!
//! THE SHAPE OF THIS FILE. Every case is generated on four axes and asserted
//! against `node -e`, never against `ZIPP_NOJIT=1` (the interpreter has no
//! homes, so it cannot witness a home bug, but neither would it catch a shared
//! front-end bug):
//!
//!   * TIER — an all-int kernel (`region_int.rs` / `region_int_gpr.rs`) and an
//!     f64 kernel (`regalloc.rs`). One defect, two emitters.
//!   * INNER NESTING DEPTH 1..3 — the def is always in the outer loop and the
//!     read always in the innermost one, so the live range has to survive one,
//!     two, or three back-edges.
//!   * PRESSURE — 0, 2, 4 and 6 extra body statements. This is the axis that
//!     decides WHICH temp wins the freed home (and, at 0, whether reuse engages
//!     at all), and it is why the original minimized cases looked so fragile:
//!     changing one operand moved the region either side of the pool.
//!   * the INVARIANT's spelling — computed, literal, and induction-dependent.
//!     All three were reported as load-bearing when the fuzzer minimized this;
//!     they are not, they just shift the allocation.
//!
//! [`liverange_parity_merge_point_in_the_live_range`] covers the other way a
//! mention window is re-entered — a forward branch merging back inside it —
//! and [`liverange_mechanism_*`] read the tier out of a child's `ZIPP_JITLOG`,
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

/// The same program's output from `node`, so expectations are neither
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

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

// ── the matrix ──────────────────────────────────────────────────────────────

/// Extra innermost-body statements. Each one adds a temp (and, for the literal
/// ones, a constant register), which is what pushes the region past the home
/// pool and decides which value inherits the freed home. They fold into `h` so
/// none can be dead-code-eliminated.
const PRESSURE: [&str; 6] = [
    "h = (h ^ 5) | 0;",
    "h = (h + (i * 7)) | 0;",
    "h = (h + (j * 11)) | 0;",
    "h = (h - 13) | 0;",
    "h = (h ^ (i + 17)) | 0;",
    "h = (h + 19) | 0;",
];

/// The pressure levels swept. 0 is the pair of shapes the fuzzer minimized to;
/// the rest re-let the freed home to a different value each time.
const LEVELS: [usize; 4] = [0, 2, 4, 6];

/// How the outer-loop invariant is written. Computed, literal, and dependent on
/// the induction variable — the three spellings the original minimization
/// called load-bearing.
const INT_INVS: [&str; 3] = ["255 * -3", "-765", "255 * -i"];
const DBL_INVS: [&str; 3] = ["h * 0.5", "50.5", "i * 0.5"];

/// A kernel whose `d` is defined in the OUTER loop and read `depth` loops in.
///
/// `dbl` picks the tier: the int kernel keeps every value int32 so
/// `region_is_int` holds, the f64 kernel carries a running double so the region
/// declines INT and lands on `regalloc.rs`. In both, `d`'s only read sits at the
/// bottom of the nest, so its live range spans every back-edge between them —
/// exactly the span the mention window did not cover.
fn kernel(inv: &str, depth: usize, pressure: usize, dbl: bool) -> String {
    let ivs = ["j", "q", "u"];
    let mut open = String::new();
    let mut ind = String::from("    ");
    for v in &ivs[..depth] {
        open.push_str(&format!("{ind}for ({v} = 0; {v} < 2; {v}++) {{\n"));
        ind.push_str("  ");
    }
    let mut close = String::new();
    let mut back = "    ".to_string() + &"  ".repeat(depth - 1);
    for _ in 0..depth {
        close.push_str(&format!("{back}}}\n"));
        back.truncate(back.len().saturating_sub(2));
    }
    let (decl, body_head) = if dbl {
        (
            "var h = 1, i = 0, j = 0, q = 0, u = 0;\n  var d = 2.5, a = 1.5;",
            format!("{ind}a = a * 0.5 + j;\n{ind}h = (h + (d > 100 ? 7 : 3)) | 0;\n"),
        )
    } else {
        (
            "var h = 1, i = 0, j = 0, q = 0, u = 0;\n  var d = 1;",
            format!("{ind}h = (h + ((d * 1024) | 0)) | 0;\n"),
        )
    };
    let extra: String = PRESSURE[..pressure]
        .iter()
        .map(|s| format!("{ind}{s}\n"))
        .collect();
    format!(
        "function kernel(n) {{\n  {decl}\n  for (i = 0; i < n; i++) {{\n    d = {inv};\n\
         {open}{body_head}{extra}{close}  }}\n  return h;\n}}\nconsole.log(kernel(9));\n"
    )
}

// ── the parity cases ────────────────────────────────────────────────────────

/// The INT tier: every invariant spelling, every nesting depth, every pressure
/// level. Pre-fix all 36 of these answer wrong.
#[test]
fn liverange_parity_int_matrix() {
    for inv in INT_INVS {
        for depth in 1..=3 {
            for p in LEVELS {
                assert_matches_node(&kernel(inv, depth, p, false));
            }
        }
    }
}

/// The DOUBLE tier — the same sweep through `regalloc.rs`.
#[test]
fn liverange_parity_double_matrix() {
    for inv in DBL_INVS {
        for depth in 1..=3 {
            for p in LEVELS {
                assert_matches_node(&kernel(inv, depth, p, true));
            }
        }
    }
}

/// The INT face as the fuzzer minimized it, kept verbatim because its
/// arithmetic is exact and therefore diagnostic: each inner iteration adds
/// `(255 * -3 * 1024) | 0` = -783360, so a wrong answer is a whole number of
/// missing addends. Pre-fix `kernel(9)` returned -13317119 (one short) and
/// `ZIPP_JIT_THRESHOLD=1` returned -7833599 (eight short) — one lost addend for
/// every outer iteration that ran compiled, which is what made it read as
/// dropped ITERATIONS rather than as a zeroed operand.
#[test]
fn liverange_parity_int_exact_addends() {
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

/// The same program with the inner iterations COUNTED as well as summed, which
/// is what separates this defect from the one it was reported as: the count is
/// right in every mode. Pre-fix this printed 40 iterations and a sum 12 addends
/// short — the loop ran every iteration and 12 of them multiplied by a clobbered
/// home.
#[test]
fn liverange_parity_iteration_count_is_not_the_defect() {
    const SRC: &str = r#"
function kernel(n) {
  var h = 1, i = 0, j = 0, c = 0;
  var d0 = 1.5;
  for (i = 0; i < n; i++) {
    d0 = 255 * -3;
    for (j = 0; j < 2; j++) {
      c = (c + 1) | 0;
      h = (h + ((d0 * 1024) | 0)) | 0;
    }
  }
  return c + ":" + h;
}
console.log(kernel(20));
"#;
    assert_matches_node(SRC);
}

/// The DOUBLE face as the fuzzer minimized it. `d1` is `h * 0.5` and `h` never
/// exceeds 109, so `d1 > 100` is false on all 36 evaluations; pre-fix the answer
/// was 117, i.e. the `7` arm twice. The clobbering value was `h + (7|3)`, which
/// is exactly the quantity the compare was supposed to be independent of.
#[test]
fn liverange_parity_double_compare_reads_its_own_operand() {
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

/// The other way control re-enters a mention window: not a back-edge but a
/// FORWARD branch whose merge point lands inside it. `d` is read on one arm of
/// the `if` and the join sits between its def and that read, so the ips on the
/// untaken arm run while `d` is live. A live range that stops at the last
/// mention is wrong here for the same reason, one control-flow shape over.
#[test]
fn liverange_parity_merge_point_in_the_live_range() {
    const SRC: &str = r#"
function kernel(n) {
  var h = 1, i = 0, j = 0, t = 0;
  var d = 1;
  for (i = 0; i < n; i++) {
    d = 255 * -3;
    for (j = 0; j < 4; j++) {
      if ((j & 1) === 0) {
        t = ((d * 1024) | 0);
      } else {
        t = ((i * 31) ^ 5) | 0;
      }
      h = (h + t) | 0;
      h = (h ^ (j * 7)) | 0;
      h = (h - 13) | 0;
    }
  }
  return h;
}
console.log(kernel(9));
"#;
    assert_matches_node(SRC);
}

// ── mode and mechanism pins ─────────────────────────────────────────────────

/// Every parity case above must answer identically in every mode.
///
/// `ZIPP_JIT_THRESHOLD=1` is the load-bearing one: it compiles the OUTER region
/// before the interpreter has run the loop even once, so every iteration is
/// served by the region that carries the defect (at the default threshold only
/// the tail of the loop is). `ZIPP_NO_FUSED_CMPJUMP=1` and `ZIPP_NO_GPR_HOMES=1`
/// re-plan the allocation, which moves which temp inherits a freed home, and
/// `ZIPP_NO_GLOB_RANGE=1` pins the pre-glob-range linear scan — the branch the
/// two minimized cases actually took.
#[test]
fn liverange_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 5] = [
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_NO_FUSED_CMPJUMP", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_NO_GLOB_RANGE", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = Command::new(&exe);
        cmd.arg("liverange_parity_");
        for (key, val) in mode {
            cmd.env(key, val);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the liverange_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}

/// Run `src` in a child under `ZIPP_JITLOG=1` and hand back its stderr. The
/// child is this same test binary running [`liverange_jitlog_child`], with the
/// program passed through the environment.
fn jitlog_of(src: &str) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let out = Command::new(&exe)
        .arg("liverange_jitlog_child")
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_LIVERANGE_SRC", src)
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

/// The worker for [`jitlog_of`]. A no-op unless `ZIPP_LIVERANGE_SRC` is set,
/// because the JIT switches are memoized latches: a mode IS a process.
#[test]
#[ignore = "worker: spawned by jitlog_of with ZIPP_LIVERANGE_SRC set"]
fn liverange_jitlog_child() {
    let Some(src) = std::env::var_os("ZIPP_LIVERANGE_SRC") else {
        return;
    };
    let _ = run_ok(&src.to_string_lossy());
}

/// The kernels really do compile on the tier each one is named for, and the
/// widened live ranges did not cost so many homes that a case now declines to
/// MEM. Without this the parity assertions could go green vacuously.
#[test]
fn liverange_mechanism_kernels_reach_their_tier() {
    for depth in 1..=3 {
        for p in LEVELS {
            let src = kernel("255 * -3", depth, p, false);
            let log = jitlog_of(&src);
            assert!(
                log.contains("INT region fn1 ["),
                "int d={depth} p={p}: no INT region — the case no longer reaches \
                 the emitter it is testing:\n{log}"
            );
            assert!(
                !log.contains("MEM region fn1 ["),
                "int d={depth} p={p}: the kernel dropped to the MEM tier:\n{log}"
            );

            let src = kernel("h * 0.5", depth, p, true);
            let log = jitlog_of(&src);
            assert!(
                log.contains("DOUBLE region fn1 ["),
                "dbl d={depth} p={p}: no DOUBLE region:\n{log}"
            );
            assert!(
                !log.contains("MEM region fn1 ["),
                "dbl d={depth} p={p}: the kernel dropped to the MEM tier:\n{log}"
            );
        }
    }
}
