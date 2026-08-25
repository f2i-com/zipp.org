//! W14 BUG: the xmm INT region emitter's dense-Array element read destroyed a
//! planner-owned general-purpose register, so an ordinary integer scan over an
//! array of ints silently answered wrong.
//!
//! THE DEFECT. `BOOL_GPRS = [8, 9, 10, 11]` (codegen/plan.rs) is the pool the
//! xmm INT planner hands out for (a) `Bool` register homes and (b) `gpr_const`
//! — prologue-filled mirrors of hoisted integer compare constants that
//! `emit_icmp_flags` reads straight out of the gpr in the loop BODY. Every
//! other body arm of `region_int.rs` scratches only rax/rcx/rdx (the `Mod` arm
//! says so explicitly), the prologue orders its bool entry loads last because
//! its entry-load helper scratches r10, and `emit_bool_entry_load` avoids r10
//! for the same reason. The dense-Array (`ARR_INT_PIN_KIND`) branch of the
//! `GetIndex` arm did not get the memo: its NaN-box tag check ran
//! `mov r10, rax ; shr r10, 48 ; cmp r10d, INT_TAG_HI`, so EVERY element read
//! overwrote whatever the plan had parked in r10 — for the rest of the region,
//! across the backedge. The INT-GPR twin of the same arm (region_int_gpr.rs)
//! already scratched rdx and documented the divergence; the xmm arm now does
//! too.
//!
//! WHAT IT LOOKED LIKE. The reduced reproducer — a nested scan with a `break`
//! over a dense int array — froze its match counter at whatever value it held
//! when the region OSR-compiled (`m` tracked ZIPP_JIT_THRESHOLD exactly), while
//! node and ZIPP_NOJIT=1 both answered 28571. The apparent trigger ("an outer
//! fused guard leaving the region, an inner fused guard staying in it, and a
//! pinned dense-array read") is a register-numbering coincidence, not a
//! control-flow property: that shape has exactly TWO `Bool` registers, so the
//! bool bump allocator stops at BOOL_GPRS[1] and the first `gpr_const` mirror
//! — the literal `1` of `A[i] === 1` — lands on BOOL_GPRS[2] = r10. Unfusing
//! either guard adds a third bool and pushes the mirrors past r10; dropping the
//! array removes the clobber. Hence the cases below deliberately reach past
//! that shape: a FLAT single loop with no nesting and no break miscompiled the
//! same way, so did two sequential loops, a three-level nest, and — with no
//! `gpr_const` involved at all — any region whose THIRD or fourth `Bool` home
//! is r10 and is read after an element load.
//!
//! Every `intguard_parity_*` case asserts byte-identical output against
//! `node -e`, never against ZIPP_NOJIT=1 (an emitter bug that also existed in
//! the interpreter would pass that). `intguard_all_modes_answer_identically`
//! re-runs the whole set in child processes under `ZIPP_NO_FUSED_CMPJUMP=1`
//! (the compiler-side workaround this replaces), `ZIPP_NO_GPR_HOMES=1` (pins
//! the shapes onto the xmm emitter that carried the defect), `ZIPP_JIT_THRESHOLD=1`
//! and `ZIPP_NOJIT=1`. `intguard_mechanism_*` reads each case's tier back out
//! of a child's ZIPP_JITLOG, so an admission change that quietly drops these
//! kernels to the MEM tier fails the suite instead of making it vacuous.

/// Dense all-Int Array `K` (the pin family the defect lived in) and the
/// Int32Array `T` twin (the same loop through the other `GetIndex` branch —
/// the control that must NOT move).
const PRELUDE: &str = r#""use strict";
var N = 4000;
var K = [], T = new Int32Array(N);
for (var q = 0; q < N; q++) { K.push(q % 7); T[q] = q % 7; }
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The same program's output from `node -e`, so expectations aren't
/// hand-computed and aren't taken from our own interpreter.
fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node v24 on PATH (expected values come from `node -e`)");
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
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for:\n{src}");
}

fn prog(body: &str) -> String {
    format!("{PRELUDE}{body}")
}

// ── the matrix builder ───────────────────────────────────────────────────────
// Three axes, spelled as source text so each case is a program a user could
// have written:
//   * outer / inner loop guard: `Fused` (one `JumpIfNotLt`), `Split` (the
//     `Lt` + `JumpIfFalse` pair, written so it stays INT-admissible), `Not`
//     (`!(a >= b)` — an `Instr::Not` the INT admission rejects today, so these
//     land on MEM; kept as parity guards for when `Not` is admitted).
//   * element source: dense Array, Int32Array, or none (`i % 7`).
//   * loop exit: `break` out of the inner loop, `continue` in the outer one,
//     or the inner test folded into the loop condition (no `break` at all).

#[derive(Clone, Copy, PartialEq)]
enum Guard {
    Fused,
    Split,
    Not,
}

impl Guard {
    fn outer(self) -> &'static str {
        match self {
            Guard::Fused => "for (var i = 0; i + 3 < n; i++) {",
            Guard::Split => "for (var i = 0; ; i++) { var og = i + 3 < n; if (og) {} else break;",
            Guard::Not => "for (var i = 0; !(i + 3 >= n); i++) {",
        }
    }
    fn inner(self) -> &'static str {
        match self {
            Guard::Fused => "while (j < n) {",
            Guard::Split => "for (;;) { var ig = j < n; if (ig) {} else break;",
            Guard::Not => "while (!(j >= n)) {",
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Guard::Fused => "fused",
            Guard::Split => "split",
            Guard::Not => "not",
        }
    }
}

/// `(tag, argument, outer element expression, inner element expression)`.
const ELEMS: [(&str, &str, &str, &str); 3] = [
    ("dense", "K", "A[i]", "A[j]"),
    ("i32", "T", "A[i]", "A[j]"),
    ("none", "K", "i % 7", "j % 7"),
];

fn scan_break(outer: Guard, inner: Guard, elem: usize) -> String {
    let (tag, arg, ei, ej) = ELEMS[elem];
    prog(&format!(
        "function f(A, n) {{
  var m = 0;
  {o}
    if ({ei} === 1) {{
      var j = i + 3;
      {i} if ({ej} === 4) break; j++; }}
      m++; i = j;
    }}
  }}
  return m;
}}
var s = 0; for (var r = 0; r < 3; r++) s += f({arg}, N);
console.log(\"break {ot} {it} {tag} \" + s);
",
        o = outer.outer(),
        i = inner.inner(),
        ot = outer.tag(),
        it = inner.tag(),
    ))
}

fn scan_continue(outer: Guard, inner: Guard, elem: usize) -> String {
    let (tag, arg, ei, ej) = ELEMS[elem];
    prog(&format!(
        "function f(A, n) {{
  var m = 0;
  {o}
    if ({ei} !== 1) continue;
    var j = i + 3;
    {i} if ({ej} === 4) break; j++; }}
    m++; i = j;
  }}
  return m;
}}
var s = 0; for (var r = 0; r < 3; r++) s += f({arg}, N);
console.log(\"cont {ot} {it} {tag} \" + s);
",
        o = outer.outer(),
        i = inner.inner(),
        ot = outer.tag(),
        it = inner.tag(),
    ))
}

/// The inner test folded into the loop CONDITION — the element read now lives
/// in the guard itself and there is no `break` at all.
fn scan_condition(outer: Guard, elem: usize) -> String {
    let (tag, arg, ei, ej) = ELEMS[elem];
    prog(&format!(
        "function f(A, n) {{
  var m = 0;
  {o}
    if ({ei} === 1) {{ var j = i + 3; while (j < n && {ej} !== 4) j++; m++; i = j; }}
  }}
  return m;
}}
var s = 0; for (var r = 0; r < 3; r++) s += f({arg}, N);
console.log(\"cond {ot} {tag} \" + s);
",
        o = outer.outer(),
        ot = outer.tag(),
    ))
}

// ── the reduced reproducer and its immediate neighbours ──────────────────────

/// The 3-line reproducer. Pre-fix this printed the value `m` happened to hold
/// when the region OSR-compiled (4 here, 2 at the original N=200000) instead of
/// 1713 — the `A[i] === 1` compare was reading the clobbered r10 mirror of the
/// literal `1`, so no iteration ever matched again.
#[test]
fn intguard_parity_reduced_repro_nested_break() {
    assert_matches_node(&prog(
        r#"function f(A, n) {
  var m = 0;
  for (var i = 0; i + 3 < n; i++) {
    if (A[i] === 1) { var j = i + 3; while (j < n) { if (A[j] === 4) break; j++; } m++; i = j; }
  }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(K, N);
console.log("nested-break " + s);
"#,
    ));
}

/// The same scan writing its spans into an Int32Array, so a wrong answer shows
/// up in the OUTPUT ARRAY as well as the count (the original bug report's form:
/// `A2` came out 0 instead of 18).
#[test]
fn intguard_parity_span_writes_into_int32array() {
    assert_matches_node(&prog(
        r#"function findSpans(A, n, O) {
  var m = 0;
  for (var i = 0; i + 3 < n; i++) {
    if (A[i] === 1) {
      var j = i + 3;
      while (j < n) { if (A[j] === 4) break; j++; }
      O[m] = i + 3; m++; i = j;
    }
  }
  return m;
}
var O = new Int32Array(N), tot = 0;
for (var r = 0; r < 3; r++) tot += findSpans(K, N, O);
console.log("bug1 " + tot + " " + O[0] + " " + O[1] + " " + O[2] + " " + O[500]);
"#,
    ));
}

/// NO NESTING, no `break`, one loop. The scout's trigger triple said this shape
/// was safe; it was not — two `===` against literals is all it takes, because
/// two bools is exactly what leaves BOOL_GPRS[2] to the first `gpr_const`.
#[test]
fn intguard_parity_flat_loop_two_const_compares() {
    assert_matches_node(&prog(
        r#"function f(A, n) {
  var m = 0;
  for (var i = 0; i < n; i++) { var v = A[i]; if (v === 1) m += 1; if (v === 4) m += 2; }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(K, N);
console.log("flat " + s);
"#,
    ));
}

/// The element read placed AFTER both compares, so the destroyed mirror is only
/// observed on the NEXT iteration — the clobber has to survive the backedge for
/// this one to fail, which it did.
#[test]
fn intguard_parity_clobber_survives_the_backedge() {
    assert_matches_node(&prog(
        r#"function f(A, n) {
  var m = 0, v = 0;
  for (var i = 0; i < n; i++) { if (v === 1) m += 1; if (v === 4) m += 2; v = A[i]; }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(K, N);
console.log("carry " + s);
"#,
    ));
}

/// A DIFFERENT victim: no `gpr_const` mirror at all, just three boolean temps.
/// The third `Bool` home IS r10, it is defined before the element read and
/// consumed after it, so the read destroyed a live boolean. Pre-fix: 97962 for
/// node's 59586.
#[test]
fn intguard_parity_bool_home_live_across_element_read() {
    assert_matches_node(&prog(
        r#"function f(A, n) {
  var m = 0;
  for (var i = 0; i < n; i++) {
    var p = i % 2 === 0, q = i % 3 === 0, w = i % 5 === 0;
    var v = A[i];
    if (p) m += 1;
    if (q) m += 2;
    if (w) m += 4;
    m += v;
  }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(K, N);
console.log("bool3 " + s);
"#,
    ));
}

/// The same three bools compared against GLOBALS instead of literals, so the
/// region provably has NO `gpr_const` mirror at all (a mirror is only created
/// for a HOISTED CONSTANT compare operand). That isolates the second victim
/// class: the destroyed r10 here can only be a `Bool` home. Pre-fix: 97948 for
/// node's 69180.
#[test]
fn intguard_parity_bool_home_with_no_const_mirrors() {
    assert_matches_node(&prog(
        r#"var l1 = 0, l2 = 1, l3 = 2;
function f(A, n) {
  var m = 0;
  for (var i = 0; i < n; i++) {
    var p = (i % 2) > l1;
    var q = (i % 3) > l2;
    var w = (i % 5) > l3;
    var v = A[i];
    if (p) m += 1;
    if (q) m += 2;
    if (w) m += 4;
    m += v;
  }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(K, N);
console.log("boolonly " + s);
"#,
    ));
}

/// Four boolean temps derived FROM the element, i.e. every BOOL_GPR occupied
/// and a second element read between the third bool's def and its use.
#[test]
fn intguard_parity_four_bools_from_the_element() {
    assert_matches_node(&prog(
        r#"function f(A, n) {
  var m = 0;
  for (var i = 0; i < n; i++) {
    var v = A[i];
    var p = v > 1, q = v > 3, w = v > 5, u = A[i] === 0;
    if (p) m += 1;
    if (q) m += 2;
    if (w) m += 4;
    if (u) m += 8;
  }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(K, N);
console.log("bool4 " + s);
"#,
    ));
}

/// Two SEQUENTIAL loops in one function — two independent INT regions, each
/// with its own plan, so the clobber has to be wrong twice.
#[test]
fn intguard_parity_sequential_loops() {
    assert_matches_node(&prog(
        r#"function f(A, n) {
  var m = 0;
  for (var i = 0; i < n; i++) { var v = A[i]; if (v === 1) m += 1; if (v === 4) m += 2; }
  for (var i2 = 0; i2 < n; i2++) { var v2 = A[i2]; if (v2 === 2) m += 8; if (v2 === 5) m += 16; }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(K, N);
console.log("seq " + s);
"#,
    ));
}

/// Three nesting levels, two `break`s, and a nested region compiled alongside
/// the enclosing one.
#[test]
fn intguard_parity_three_level_nesting() {
    assert_matches_node(&prog(
        r#"function f(A, n) {
  var m = 0;
  for (var i = 0; i + 9 < n; i++) {
    if (A[i] === 1) {
      var j = i + 1;
      while (j < n) {
        if (A[j] === 4) {
          var k = j + 1;
          while (k < n) { if (A[k] === 6) break; k++; }
          m += k - j; j = k; break;
        }
        j++;
      }
      m++; i = j;
    }
  }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(K, N);
console.log("three " + s);
"#,
    ));
}

/// A `while` head instead of a `for` head, so the outer guard's failure target
/// is the region's LAST ip rather than an ip past its end.
#[test]
fn intguard_parity_inner_exit_at_region_last_ip() {
    assert_matches_node(&prog(
        r#"function f(A, n) {
  var m = 0, i = 0;
  while (i + 3 < n) {
    if (A[i] === 1) { var j = i + 3; while (j < n) { if (A[j] === 4) break; j++; } m++; i = j; }
    i++;
  }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(K, N);
console.log("lastip " + s);
"#,
    ));
}

/// A double element mid-array: the tag check must DEOPT (that is the guard
/// whose scratch register was the bug), the interpreter takes over at that ip,
/// and the region re-enters afterwards.
#[test]
fn intguard_parity_deopt_on_double_element() {
    assert_matches_node(&prog(
        r#"var D = []; for (var q = 0; q < N; q++) D.push(q % 7);
D[N - 5] = 2.5;
function f(A, n) {
  var m = 0;
  for (var i = 0; i < n; i++) { var v = A[i]; if (v === 1) m += 1; if (v === 4) m += 2; }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(D, N);
console.log("deopt " + s);
"#,
    ));
}

/// A HOLE mid-array — the other value the tag check rejects.
#[test]
fn intguard_parity_deopt_on_hole() {
    assert_matches_node(&prog(
        r#"var H = []; for (var q = 0; q < N; q++) H.push(q % 7);
delete H[N - 9];
function f(A, n) {
  var m = 0;
  for (var i = 0; i < n; i++) { var v = A[i]; if (v === 1) m += 1; if (v === 4) m += 2; }
  return m;
}
var s = 0; for (var r = 0; r < 3; r++) s += f(H, N);
console.log("hole " + s);
"#,
    ));
}

// ── the neighbour matrix ─────────────────────────────────────────────────────

/// `{outer fused, outer split} x {inner fused, inner split} x {dense, Int32Array,
/// no array}`, inner loop left by `break`. The dense/Int32Array members all
/// compile on the xmm INT tier with a pin (pinned by
/// `intguard_mechanism_matrix_break_stays_on_the_register_tier`).
#[test]
fn intguard_parity_matrix_break() {
    for outer in [Guard::Fused, Guard::Split] {
        for inner in [Guard::Fused, Guard::Split] {
            for elem in 0..ELEMS.len() {
                assert_matches_node(&scan_break(outer, inner, elem));
            }
        }
    }
}

/// The same twelve with the outer loop left by `continue` instead.
#[test]
fn intguard_parity_matrix_continue() {
    for outer in [Guard::Fused, Guard::Split] {
        for inner in [Guard::Fused, Guard::Split] {
            for elem in 0..ELEMS.len() {
                assert_matches_node(&scan_continue(outer, inner, elem));
            }
        }
    }
}

/// The inner test folded into the loop condition: the element read moves into
/// the guard and there is no `break`.
#[test]
fn intguard_parity_matrix_condition_exit() {
    for outer in [Guard::Fused, Guard::Split] {
        for elem in 0..ELEMS.len() {
            assert_matches_node(&scan_condition(outer, elem));
        }
    }
}

/// The `!(a >= b)` spellings of both guards. These decline INT admission today
/// on `Instr::Not` and run on the MEM tier, so they cannot exercise the defect
/// — they are here so that admitting `Not` later cannot widen the emitter's
/// reach without this matrix noticing.
#[test]
fn intguard_parity_matrix_not_spelled_guards() {
    for outer in [Guard::Fused, Guard::Split, Guard::Not] {
        for inner in [Guard::Fused, Guard::Split, Guard::Not] {
            if outer != Guard::Not && inner != Guard::Not {
                continue;
            }
            for elem in 0..ELEMS.len() {
                assert_matches_node(&scan_break(outer, inner, elem));
            }
        }
    }
    for elem in 0..ELEMS.len() {
        assert_matches_node(&scan_condition(Guard::Not, elem));
    }
}

// ── mode and mechanism pins ──────────────────────────────────────────────────

/// Every case above must answer identically in every mode. `ZIPP_NO_GPR_HOMES=1`
/// is the load-bearing one: it forces the xmm emitter that carried the defect
/// even for shapes the GPR emitter would otherwise take (the GPR twin of the
/// element read always scratched rdx, so it never had the bug and would mask a
/// regression).
#[test]
fn intguard_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 4] = [
        &[("ZIPP_NO_FUSED_CMPJUMP", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("intguard_parity_");
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
            "the intguard_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}

/// Run one test in a child under ZIPP_JITLOG and hand back its stderr.
fn jitlog_of(test_name: &str) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg(test_name)
        .arg("--exact")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .output()
        .expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{test_name} child failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}

/// `[jit] INT region [s,e] guard-hoist pins=…` is printed ONLY by the xmm INT
/// emitter (`region_int.rs`), and only when the region carries at least one
/// pin; the GPR emitter prints `INT-GPR region [` instead. So counting these
/// lines counts regions that went through the arm this file is about.
fn xmm_int_regions_with_pins(log: &str) -> usize {
    log.lines()
        .filter(|l| l.starts_with("[jit] INT region [") && l.contains("guard-hoist pins="))
        .count()
}

/// Each single-shape case really does compile its kernel on the xmm INT tier
/// with a pinned receiver. Without this the parity assertions could go green on
/// a build where every one of these loops quietly fell back to MEM — which is
/// precisely the state the `not`-spelled matrix members are already in.
#[test]
fn intguard_mechanism_cases_compile_xmm_int_with_a_pin() {
    for name in [
        "intguard_parity_reduced_repro_nested_break",
        "intguard_parity_span_writes_into_int32array",
        "intguard_parity_flat_loop_two_const_compares",
        "intguard_parity_clobber_survives_the_backedge",
        "intguard_parity_bool_home_live_across_element_read",
        "intguard_parity_bool_home_with_no_const_mirrors",
        "intguard_parity_four_bools_from_the_element",
        "intguard_parity_sequential_loops",
        "intguard_parity_three_level_nesting",
        "intguard_parity_inner_exit_at_region_last_ip",
        "intguard_parity_deopt_on_double_element",
        "intguard_parity_deopt_on_hole",
    ] {
        let log = jitlog_of(name);
        assert!(
            xmm_int_regions_with_pins(&log) >= 1,
            "{name}: no pinned xmm INT region compiled — the case no longer \
             reaches the emitter it is testing:\n{log}"
        );
        assert!(
            log.contains("INT region fn1 ["),
            "{name}: the kernel function's region is not INT:\n{log}"
        );
        assert!(
            !log.contains("MEM region fn1 ["),
            "{name}: the kernel function dropped to the MEM tier:\n{log}"
        );
    }
}

/// The fused/split matrix members: no kernel may fall to MEM, and the eight
/// dense/Int32Array members must each contribute a pinned xmm INT region.
#[test]
fn intguard_mechanism_matrix_stays_on_the_register_tier() {
    for (name, min_pinned) in [
        ("intguard_parity_matrix_break", 8),
        ("intguard_parity_matrix_continue", 8),
        ("intguard_parity_matrix_condition_exit", 4),
    ] {
        let log = jitlog_of(name);
        assert!(
            !log.contains("MEM region fn1 ["),
            "{name}: a matrix kernel dropped to the MEM tier:\n{log}"
        );
        let n = xmm_int_regions_with_pins(&log);
        assert!(
            n >= min_pinned,
            "{name}: only {n} pinned xmm INT regions (expected >= {min_pinned}) \
             — the array members stopped reaching the emitter:\n{log}"
        );
    }
}
