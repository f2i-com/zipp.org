//! Typed splice lanes: a proven-numeric straight-line leaf-splice body is
//! emitted register-resident (entry tag guards + unboxed GPR/xmm ops + one
//! exit box) instead of the boxed per-op loop. Every guard or range bail
//! jumps to the per-call helper fallback as a PURE PREFIX — nothing is
//! committed (upval writes stay buffered in registers), so the fallback
//! re-runs the whole call with full semantics.
//!
//! The pins below are the mechanism's non-negotiable hazards:
//! * ToInt32 of an out-of-i32 double BAILS (modular wrap via the fallback,
//!   never a silent in-lane wrap) — driven with products ≥ 2^31.
//! * IEEE op order is preserved bytecode-op-for-op: `(u/10)*n` must print
//!   the double-rounded 0.7000000000000001 shape, never the fused 0.7.
//! * An externally reseeded closure upval holding a non-int hits the entry
//!   guard and degrades to the (correct) helper call.
//! * An add chain whose magnitude bound could pass 2^53 must DECLINE the
//!   lane (i64 arithmetic would diverge from f64 rounding).
//! * A body needing more live integers than the free register set declines.
//! * An immediate operand that folds OUTSIDE i32 range cannot be spelled as
//!   an ALU imm32, and the one immediate-lhs template the lane owns
//!   (`IAddImmRev`) computes `imm - b` — Sub's form only. Add must never
//!   borrow it; both ops must handle a wide immediate on either side.
//!
//! Every `tl_parity_` expectation is byte-compared against `node -e`, and the
//! final test re-runs the whole matrix under `ZIPP_NO_TYPED_SPLICE=1` (the
//! off-switch), `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1`, and
//! `ZIPP_GC_STRESS=1` in child processes.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
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
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for: {src}");
}

const MULBERRY: &str = r#"
    function mulberry32(seed) {
      var a = seed | 0;
      return function () {
        a = (a + 0x6D2B79F5) | 0;
        var t = Math.imul(a ^ (a >>> 15), 1 | a);
        t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
      };
    }
"#;

/// The row shape itself: the nested-spliced ri(n) = (rnd()*n)|0 stream. The
/// accumulator digests every activation, so a single wrong lane value (a
/// missed |0 wrap, an imul width bug, a stale upval commit) changes the line.
/// A raw rnd() double is printed too — pins the f64 division and the
/// shortest-round-trip formatting of the lane's F64 exit.
#[test]
fn tl_parity_mulberry_ri_stream() {
    let src = format!(
        r#""use strict";
        {MULBERRY}
        var rnd = mulberry32(0x10C5CAB);
        function ri(n) {{ return (rnd() * n) | 0; }}
        var acc = 0;
        for (var i = 0; i < 120000; i++) {{
            acc = (acc + ri(12) + ri(900) + ri(100000)) | 0;
        }}
        console.log("acc:" + acc + " tail:" + rnd());
        "#
    );
    assert_matches_node(&src);
}

/// ToInt32-of-out-of-i32 pin: products ≥ 2^31 (n = 2^33, 2^40, and a huge
/// near-2^53 scale) must produce the MODULAR wrap — the lane bails to the
/// helper, which re-runs the call in full semantics. A silent in-lane
/// truncation-to-i32 (or an unwrapped i64) changes every line.
#[test]
fn tl_parity_toint32_wrap_bail() {
    let src = format!(
        r#""use strict";
        {MULBERRY}
        var rnd = mulberry32(0xBEEF);
        function ri(n) {{ return (rnd() * n) | 0; }}
        var acc = 0, big = 0;
        for (var i = 0; i < 60000; i++) {{
            acc = (acc + ri(8589934592)) | 0;         // 2^33: wraps ~half the time
            big = (big + ri(1099511627776)) | 0;      // 2^40
            if (i === 30000) console.log("mid:" + acc + ":" + big);
        }}
        console.log("acc:" + acc + " big:" + big + " last:" + ri(9007199254740992));
        "#
    );
    assert_matches_node(&src);
}

/// IEEE op-order pin: `(u/10)*n` double-rounds (0.1 is inexact), so the
/// unfused result differs from `(u*n)/10` — e.g. (1/10)*7 prints
/// 0.7000000000000001 while the fused form prints 0.7. Any future algebraic
/// fusion of the lane's Div+Mul flips these lines.
#[test]
fn tl_parity_ieee_div_mul_order() {
    let src = r#""use strict";
        function f(u, n) { return (u / 10) * n; }
        var acc = 0;
        for (var i = 0; i < 60000; i++) acc += f(i & 15, 7);
        var parts = [];
        for (var u = 1; u <= 8; u++) parts.push(f(u, 7));
        console.log(parts.join(",") + " acc:" + acc);
        "#;
    assert_matches_node(src);
}

/// Externally reseeded closure upval: mid-loop the cell is written a non-int
/// (0.5) by a SECOND closure over the same variable — the lane's entry Int
/// guard must miss and the helper must coerce ((0.5*3)|0 === 1) until the
/// cell goes back to an int.
#[test]
fn tl_parity_reseeded_upval() {
    let src = r#""use strict";
        function mk() {
            var u = 1;
            function get(x) { return (u * x) | 0; }
            function set(v) { u = v; }
            return [get, set];
        }
        var p = mk();
        var g = p[0], s = p[1];
        var acc = 0;
        for (var i = 0; i < 90000; i++) {
            if (i === 30000) s(0.5);
            if (i === 60000) s(7);
            acc = (acc + g(3)) | 0;
        }
        console.log("acc:" + acc);
        "#;
    assert_matches_node(src);
}

/// Zero-arg activation of a param-reading leaf: `ri()` binds `undefined`,
/// so the site must DECLINE the lane (plan-time argc gate) and the generic
/// path's NaN arithmetic must produce (NaN*12)|0 === 0 — pinned via node.
#[test]
fn tl_parity_zero_arg_activation() {
    let src = format!(
        r#""use strict";
        {MULBERRY}
        var rnd = mulberry32(0x51CA);
        function ri(n) {{ return (rnd() * n) | 0; }}
        var acc = 0;
        for (var i = 0; i < 60000; i++) acc = (acc + ri()) | 0;
        console.log("acc:" + acc + " next:" + ri(1000));
        "#
    );
    assert_matches_node(&src);
}

/// Adversarial add chain: doubled magnitudes pass 2^53 where i64 adds would
/// stay exact but f64 rounds — the large doubles printed pin the f64
/// semantics against node. (The compiler's per-statement temps push this
/// body past the leaf register cap, so the 2^53 DECLINE itself is pinned by
/// the `lane_tests` unit tests on `build_typed_lane` in codegen/inline.rs,
/// where the body shape is synthetic and reaches the bound check.)
#[test]
fn tl_parity_add_chain_semantics() {
    let src = r#""use strict";
        function chain(a, b) {
            var t = a + b;
            t = t + t; t = t + t; t = t + t; t = t + t; t = t + t;
            t = t + t; t = t + t; t = t + t; t = t + t; t = t + t;
            t = t + t; t = t + t; t = t + t; t = t + t; t = t + t;
            t = t + t; t = t + t; t = t + t; t = t + t; t = t + t;
            t = t + t; t = t + t; t = t + t; t = t + t; t = t + t;
            return t;
        }
        var acc = 0;
        for (var i = 0; i < 60000; i++) acc += chain(i & 1023, 2147480000);
        console.log("acc:" + acc + " probe:" + chain(3, 2147483647));
        "#;
    assert_matches_node(src);
}

/// ── wide-immediate operand matrix ──
///
/// Builds, for each `(name, prelude)` case, four laned leaf bodies that place
/// the prelude's folded constant `c` on each side of `+` and `-`:
/// `c + x`, `x + c`, `c - x`, `x - c`. Every one is driven hot in the loop
/// (so the splice site compiles and the lane engages) and then probed at
/// three fixed arguments, so a wrong operand order shows up as a changed
/// probe list even where the accumulator happens to be symmetric.
///
/// A negative constant is spelled `z - N` over `var z = 0`, never as a `-N`
/// literal: unary minus compiles to `Neg`, which is outside the lane's closed
/// op set, so a `-N` literal DECLINES the lane and the operand shape under
/// test would never reach the scheduler at all.
fn wide_imm_matrix(cases: &[(&str, &str)], iters: u32) -> String {
    let mut fns = String::new();
    let mut names: Vec<String> = Vec::new();
    for (name, prelude) in cases {
        for (suffix, body) in [
            ("al", "return c + x;"), // wide imm on the LEFT of Add
            ("ar", "return x + c;"), // wide imm on the RIGHT of Add
            ("sl", "return c - x;"), // wide imm on the LEFT of Sub  (IAddImmRev)
            ("sr", "return x - c;"), // wide imm on the RIGHT of Sub
        ] {
            let f = format!("{name}_{suffix}");
            fns.push_str(&format!("        function {f}(x) {{ {prelude} {body} }}\n"));
            names.push(f);
        }
    }
    let calls = names
        .iter()
        .map(|f| format!("{f}(x)"))
        .collect::<Vec<_>>()
        .join(" + ");
    let probes = names
        .iter()
        .map(|f| format!("probes.push({f}(j));"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#""use strict";
{fns}        var acc = 0;
        for (var i = 0; i < {iters}; i++) {{ var x = i & 1023; acc += {calls}; }}
        var probes = [];
        for (var j = 0; j < 3; j++) {{ {probes} }}
        console.log("acc:" + acc);
        console.log(probes.join(","));
        "#
    )
}

/// THE ESCAPED BUG. `var k = 2000000000, m = 2000000000; var s = k + m;`
/// folds to 4000000000 at plan time — outside i32, so the lane cannot spell
/// it as an ALU imm32 operand. `s + x` (an imm-LHS Add) must still ADD: the
/// only immediate-lhs template the lane owns is `IAddImmRev`, which computes
/// `imm - b`, and borrowing it for Add silently returned `4000000000 - x`.
/// The `sl` row pins IAddImmRev's LEGITIMATE use (`c - x` really is
/// `imm - b`) so the fix cannot be "delete the reversed form".
#[test]
fn tl_parity_wide_imm_add_lhs_regression() {
    assert_matches_node(&wide_imm_matrix(
        &[("w4e9", "var k = 2000000000, m = 2000000000; var c = k + m;")],
        120000,
    ));
}

/// The exact narrow/wide frontier the imm32 encoding turns on, each constant
/// folded (never written as one literal, so the fold really happens inside
/// the lane) and placed on both sides of both ops:
///   2^31-1 / -2^31   — the last values that DO fit an imm32,
///   2^31   / -2^31-1 — the first that do NOT (one ulp past, both signs),
///   2^32-1           — a full unsigned word,
///   (z-2) >>> 0      — ToUint32 of a negative, which folds to a wide
///                      POSITIVE immediate (4294967294) with no wide literal
///                      anywhere in the source.
#[test]
fn tl_parity_wide_imm_boundaries() {
    assert_matches_node(&wide_imm_matrix(
        &[
            // 2^31-1: the largest immediate that still fits imm32.
            ("i32max", "var k = 2147483646, m = 1; var c = k + m;"),
            // 2^31: one past — the first wide positive.
            ("w2p31", "var k = 2147483647, m = 1; var c = k + m;"),
            // -2^31: the most negative immediate that still fits imm32.
            (
                "i32min",
                "var z = 0; var k = z - 2147483647, m = z - 1; var c = k + m;",
            ),
            // -2^31-1: one past — the first wide negative.
            (
                "wm2p31",
                "var z = 0; var k = z - 2147483647, m = z - 2; var c = k + m;",
            ),
            // 2^32-1.
            (
                "wu32max",
                "var k = 2147483647, m = 2147483647, n = 1; var c = k + m + n;",
            ),
            // `>>> 0` of a negative → a wide positive immediate.
            ("wushr", "var z = 0; var c = (z - 2) >>> 0;"),
        ],
        60000,
    ));
}

/// A chain that folds TWO wide immediates (4000000000 and 3500000000, each
/// already past i32) into a third (7500000000) before the register even
/// enters the expression — the fold must stay exact-i64 through both steps
/// and only then meet `x`.
#[test]
fn tl_parity_wide_imm_chain_fold() {
    assert_matches_node(&wide_imm_matrix(
        &[(
            "wchain",
            "var p = 2000000000, q = 2000000000, r = 2000000000, s = 1500000000; \
             var u = p + q; var v = r + s; var c = u + v;",
        )],
        60000,
    ));
}

/// The literal spelling of the same wide fold, kept as a CONTROL: `-2` is
/// `Neg`, which is outside the lane's closed op set, so this body declines
/// (the census below pins the reason) and the generic splice must produce
/// the identical `4294967294 ± x`. It is the reason `wide_imm_matrix` builds
/// its negative constants as `z - N`: written the obvious way, the operand
/// shapes under test never reach the lane at all — and a matrix that quietly
/// stopped laning would go green while testing nothing.
#[test]
fn tl_parity_ushr_neg_literal_control() {
    let src = r#""use strict";
        function u32l(x) { var n = -2; var c = n >>> 0; return c + x; }
        function u32r(x) { var n = -2; var c = n >>> 0; return x - c; }
        var acc = 0;
        for (var i = 0; i < 60000; i++) { var x = i & 1023; acc += u32l(x) + u32r(x); }
        console.log("acc:" + acc + " probe:" + u32l(3) + "," + u32r(3));
        "#;
    assert_matches_node(src);
}

/// Register budget: six xor subexpressions pending at the deepest nesting
/// point (plus the still-live params feeding them) exceed the five GPR
/// homes — the lane declines (census below asserts the reason) and the
/// generic splice computes the same value.
#[test]
fn tl_parity_reg_budget_decline() {
    let src = r#""use strict";
        function wide(a, b, c, d, e, g) {
            return (a ^ b) ^ ((c ^ d) ^ ((e ^ g) ^ ((a ^ c) ^ ((b ^ e) ^ (d ^ e)))));
        }
        var acc = 0;
        for (var i = 0; i < 60000; i++) {
            acc = (acc + wide(i, i * 3 | 0, i ^ 77, i + 13, i * 5 | 0, i ^ 4095)) | 0;
        }
        console.log("acc:" + acc);
        "#;
    assert_matches_node(src);
}

/// tokIs-shaped control: a string-compare leaf is NOT numeric and must keep
/// the generic splice (the census test below asserts the DECLINE reason).
#[test]
fn tl_parity_tokis_control() {
    let src = r#""use strict";
        function tokIs(t, s) { return t === s; }
        var toks = ["if", "for", "var", "let", "if", "while", "if", "do"];
        var hits = 0;
        for (var i = 0; i < 120000; i++) {
            if (tokIs(toks[i & 7], "if")) hits++;
        }
        console.log("hits:" + hits);
        "#;
    assert_matches_node(src);
}

/// JITLOG census: the ri splice sites must plan TYPED-LANE; the non-numeric
/// / unbounded / over-budget / zero-arg bodies must DECLINE with their exact
/// reasons. Each case runs in a child so the log lands on capturable stderr.
#[test]
fn typed_lane_jitlog_census() {
    let exe = std::env::current_exe().expect("test exe path");
    for (filter, needle) in [
        ("tl_parity_mulberry_ri_stream", "TYPED-LANE (ops="),
        (
            "tl_parity_tokis_control",
            "typed-lane=DECLINED(op-outside-lane-set)",
        ),
        (
            "tl_parity_reg_budget_decline",
            "typed-lane=DECLINED(gpr-budget)",
        ),
        (
            "tl_parity_zero_arg_activation",
            "typed-lane=DECLINED(param-not-passed)",
        ),
        // Unary minus is `Neg` — outside the closed set, so a `-N` literal
        // costs the lane. This is what forces the wide-immediate matrix to
        // spell its negative constants `z - N`.
        (
            "tl_parity_ushr_neg_literal_control",
            "typed-lane=DECLINED(op-outside-lane-set)",
        ),
    ] {
        let out = std::process::Command::new(&exe)
            .arg(filter)
            .arg("--exact")
            .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
            .env("ZIPP_JITLOG", "1")
            .env("ZIPP_JIT_THRESHOLD", "1")
            .output()
            .expect("spawn the test binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{filter} child failed:\n{stderr}");
        assert!(
            stderr.contains(needle),
            "{filter}: expected '{needle}' in JITLOG:\n{stderr}"
        );
    }
}

/// Engagement pin for the wide-immediate matrix: those programs contain
/// NOTHING but the laned leaf bodies, so the census must show typed lanes and
/// not a single DECLINE. Without this the parity tests above could go green
/// on a build where every wide-immediate site quietly fell back to the
/// generic boxed loop — which is exactly how the escaped miscompile would
/// have hidden from a repaired-but-untested lane.
#[test]
fn typed_lane_wide_imm_sites_all_lane() {
    let exe = std::env::current_exe().expect("test exe path");
    for filter in [
        "tl_parity_wide_imm_add_lhs_regression",
        "tl_parity_wide_imm_boundaries",
        "tl_parity_wide_imm_chain_fold",
    ] {
        let out = std::process::Command::new(&exe)
            .arg(filter)
            .arg("--exact")
            .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
            .env("ZIPP_JITLOG", "1")
            .output()
            .expect("spawn the test binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{filter} child failed:\n{stderr}");
        assert!(
            stderr.contains("TYPED-LANE (ops="),
            "{filter}: no site planned a typed lane, so the wide-immediate \
             operand shapes were never scheduled:\n{stderr}"
        );
        assert!(
            !stderr.contains("typed-lane=DECLINED"),
            "{filter}: a wide-immediate site DECLINED the lane — the parity \
             assertion above is no longer testing the lane:\n{stderr}"
        );
    }
}

/// Off-switch: with `ZIPP_NO_TYPED_SPLICE=1` no site may plan a lane (the
/// generic boxed loop is emitted byte-identically to pre-lane builds).
#[test]
fn typed_lane_off_switch_never_plans() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg("tl_parity_mulberry_ri_stream")
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_NO_TYPED_SPLICE", "1")
        .output()
        .expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed:\n{stderr}");
    assert!(
        !stderr.contains("TYPED-LANE"),
        "off-switch still planned typed lanes:\n{stderr}"
    );
}

/// Re-run the whole `tl_parity_` matrix with the lane off, the JIT off, the
/// JIT forced hot, and under GC stress — each in its own child process (env
/// latches are read once per process). Identical node-pinned answers in
/// every mode is the tier-parity check the engine gates hardest on.
#[test]
fn typed_lane_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_TYPED_SPLICE", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("tl_parity_")
            .env(key, val)
            .output()
            .expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{key}={val} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the tl_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
