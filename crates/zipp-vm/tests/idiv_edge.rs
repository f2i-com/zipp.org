//! `idiv` #DE edge coverage for every JIT `Mod` arm: `-(2^63) % -1` overflows
//! the idiv quotient and raises a hardware fault that kills the process under
//! panic=abort. The B116 audit found the MEM-tier arms (region_mem.rs and both
//! leaf-inline arms in inline.rs) did NOT guard the divisor `-1` the way the
//! double-tier arm (regalloc.rs) does; this battery drives operands that reach
//! `INT64_MIN % -1` — plus the `-0` zero-remainder sign cases — through hot
//! compiled loops in every mode, asserting node's answer instead of a crash.
//!
//! Every `idiv_parity_` case runs at DEFAULT thresholds; the final test
//! re-runs the whole set in child processes under `ZIPP_JIT_THRESHOLD=1`
//! (compile everything immediately — the leaf-inline arms need the caller
//! compiled), `ZIPP_NO_DOUBLE_MOD=1` (declines the double-tier Mod so the
//! MEM-tier arm hosts the op), and `ZIPP_NOJIT=1` (pure interpreter).

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
/// hand-computed.
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
    assert_eq!(ours, node, "zipp != node for: {src}");
}

/// A hot top-level loop (region tiers) where the dividend is usually a small
/// int but periodically the exact f64 `-(2^63)` — which round-trips the
/// integer guard — and the divisor alternates 3 / -1. The `% -1` iterations
/// must deopt, not fault.
#[test]
fn idiv_parity_region_int_min_mod_minus_one() {
    assert_matches_node(
        r#"
        "use strict";
        var MIN = -9223372036854775808;
        var s = 0, zs = 0;
        for (var i = 0; i < 6000; i++) {
          var a = ((i & 15) === 7) ? MIN : (i - 3000);
          var b = ((i & 3) === 1) ? -1 : 3;
          var r = a % b;
          s += r;
          if (r === 0) zs += (1 / r) < 0 ? 1 : 0;   // count -0 remainders
        }
        console.log(s + " " + zs);
        "#,
    );
}

/// The same shape through a LEAF function so the caller's compile inlines the
/// `Mod` (the inline.rs arms). `ZIPP_JIT_THRESHOLD=1` in the mode re-run
/// forces the caller to compile immediately.
#[test]
fn idiv_parity_leaf_inline_int_min_mod_minus_one() {
    assert_matches_node(
        r#"
        "use strict";
        function m(a, b) { return a % b; }
        var MIN = -9223372036854775808;
        var s = 0, zs = 0;
        for (var i = 0; i < 6000; i++) {
          var a = ((i & 15) === 7) ? MIN : (i - 3000);
          var b = ((i & 3) === 1) ? -1 : 3;
          var r = m(a, b);
          s += r;
          if (r === 0) zs += (1 / r) < 0 ? 1 : 0;
        }
        console.log(s + " " + zs);
        "#,
    );
}

/// Zero remainders keep the ORIGINAL dividend's sign in every tier: `-6 % 3`
/// is -0, `-0 % 5` is -0 (passes the integer guard: 0.0 == -0.0), `6 % 3` is
/// +0 — observed through `1/r` and `Object.is` from inside the hot loop.
#[test]
fn idiv_parity_zero_remainder_sign() {
    assert_matches_node(
        r#"
        "use strict";
        function m(a, b) { return a % b; }
        var neg = 0, pos = 0, negz = 0;
        for (var i = 0; i < 6000; i++) {
          var r1 = m(-6, 3);
          var r2 = m(6, 3);
          var r3 = m(-0, 5);
          if (Object.is(r1, -0)) neg++;
          if (Object.is(r2, 0) && 1 / r2 > 0) pos++;
          if (Object.is(r3, -0)) negz++;
        }
        console.log(neg + " " + pos + " " + negz + " " + (1 / m(-6, 3)) + " " + (1 / m(-9, -3)));
        "#,
    );
}

/// i32-range `INT_MIN % -1` (the fn_int tier's own #DE shape) plus `% 0`
/// (NaN), NaN and fractional operands, and huge-magnitude dividends — the
/// deopt umbrella around every guard.
#[test]
fn idiv_parity_i32_min_and_degenerate_divisors() {
    assert_matches_node(
        r#"
        "use strict";
        function m(a, b) { return a % b; }
        var s = "";
        for (var i = 0; i < 6000; i++) {
          var x = (-2147483648) % -1;
          var y = m(-2147483648, -1);
          if (i === 5999) s = (1 / x) + " " + (1 / y);
        }
        console.log(s);
        console.log(m(7, 0) + " " + m(NaN, 3) + " " + m(7.5, 2) + " " + m(1e300, 7) + " " + m(-9007199254740991, -1));
        "#,
    );
}

/// Re-run every `idiv_parity_` case in three more modes, each in its own
/// child process (the env latches are read once per process). The same
/// node-derived assertions passing in all modes IS the parity check — and a
/// #DE in any tier aborts the child, which fails here loudly.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_NO_DOUBLE_MOD", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("idiv_parity_")
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
            "the idiv_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
