//! `%` (Instr::Mod) on the DOUBLE (regalloc) tier: exact-integer operands run
//! an inline `idiv` remainder (a zero remainder takes the ORIGINAL dividend's
//! sign, so `-6 % 3` and `-0 % 5` stay `-0`); everything else — fractional
//! operands, NaN/Infinity, `b === 0`, `b === -1`, |x| >= 2^63 — DEOPTs to the
//! interpreter AT the op, which computes full fmod semantics.
//!
//! Every `mod_parity_*` case asserts byte-identical output against `node -e`
//! (node v24 expected on PATH, the same precondition as `dv_double_tier.rs`),
//! at DEFAULT thresholds — hot enough for the loops to OSR-compile on the
//! double tier. The final test re-runs the whole `mod_parity_` set in four
//! more modes, each in its own child process (the env latches are read once
//! per process): `ZIPP_NO_DOUBLE_MOD=1` (the off-switch — the region declines
//! to the memory tier exactly as before), `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1`
//! and `ZIPP_GC_STRESS=1`.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
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
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
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

/// The bench's fill-loop shape (bench/real/typedarray-math.js:11-14), scaled
/// down: `(u32-expr) % 100000` feeding a pinned Float64Array store — the exact
/// region B113 named as `[decline-reason] regalloc-emit-unhandled: Mod`.
#[test]
fn mod_parity_bench_fill_shape() {
    assert_matches_node(
        r#"
        "use strict";
        var NF = 40000;
        var x = new Float64Array(NF), y = new Float64Array(NF);
        for (var i = 0; i < NF; i++) {
          x[i] = (((i * 2654435761) >>> 0) % 100000) / 100000;
          y[i] = (((i * 40503 + 12345) >>> 0) % 100000) / 100000;
        }
        var s = 0;
        for (var i = 0; i < NF; i++) s += x[i] + y[i];
        console.log("s=" + s.toFixed(6));
        "#,
    );
}

/// Sign matrix from a hot loop: every combination of dividend/divisor sign,
/// including divisors larger than the dividend and results that are exactly
/// zero. The zero-remainder cases print `1/r` so a +0/-0 confusion cannot
/// hide behind `0 === -0`.
#[test]
fn mod_parity_sign_matrix_and_signed_zero() {
    assert_matches_node(
        r#"
        "use strict";
        var As = [7, -7, 6, -6, 0, 100000, 2654435761, 1, -1, 999999937];
        var Bs = [3, -3, 2, -2, 5, -5, 7, 100000, 2654435761, 999999937];
        var s = 0, inv = 0;
        for (var r = 0; r < 3000; r++) {
          for (var i = 0; i < 10; i++) {
            for (var j = 0; j < 10; j++) {
              var m = As[i] % Bs[j];
              s += m;
              inv += 1 / m; // -0 → -Infinity, +0 → Infinity: sign-of-zero sensitive
            }
          }
        }
        console.log(s + " " + inv + " " + (1 / (-6 % 3)) + " " + (1 / (6 % 3)) + " " + (1 / (-6 % -3)));
        "#,
    );
}

/// `-0` as the DIVIDEND round-trips the integer guard as 0 but must keep its
/// sign; also `-0` and huge-but-exact divisors, from inside the compiled loop.
#[test]
fn mod_parity_negative_zero_dividend() {
    assert_matches_node(
        r#"
        "use strict";
        var z = -0;
        var s = "";
        for (var r = 0; r < 4000; r++) {
          var m = z % 5;
          if (r === 3999) s = (1 / m) + " " + (1 / (z % -5)) + " " + (1 / (0 % 5));
        }
        console.log(s);
        "#,
    );
}

/// Deopt operands executed FROM the hot loop: fractional dividend/divisor,
/// NaN, ±Infinity, b === 0, b === -1 (the idiv #DE guard; the ±0 result's
/// sign follows the dividend), and 2^53/2^63-scale
/// values where cvttsd2si exactness runs out. Each deopts, re-executes in the
/// interpreter, and must not perturb the loop's other iterations.
#[test]
fn mod_parity_deopt_operands() {
    assert_matches_node(
        r#"
        "use strict";
        var As = [7.5, -7.5, 0.5, NaN, Infinity, -Infinity, 9007199254740992, 9007199254740993,
                  18446744073709552000, 12345, -12345, 1e300, 5, -5, 0.1, -0.1];
        var Bs = [2, 3.5, -3.5, 0, -1, NaN, Infinity, -Infinity, 9007199254740992, 0.25, 1e300, 7];
        var out = [];
        for (var r = 0; r < 2000; r++) {
          var s = 0, inv = 0;
          for (var i = 0; i < 16; i++) {
            for (var j = 0; j < 12; j++) {
              var m = As[i] % Bs[j];
              if (m === m) { s += m; inv += 1 / m; }
            }
          }
          if (r === 1999) out.push(s.toFixed(4) + " " + inv.toFixed(6));
        }
        console.log(out[0]);
        console.log((5 % -1) + " " + (1 / (5 % -1)) + " " + (1 / (-5 % -1)) + " " + (1 / (-5 % 1)));
        console.log((-Math.pow(2, 63)) % 3, Math.pow(2, 63) % 3, Math.pow(2, 62) % 3);
        "#,
    );
}

/// Non-number operands (strings, booleans, null, objects with valueOf) keep
/// taking the generic path — a region containing them either never admits or
/// deopts, and coercion runs exactly once per `%`.
#[test]
fn mod_parity_coercing_operands() {
    assert_matches_node(
        r#"
        "use strict";
        var calls = 0;
        var obj = { valueOf: function () { calls++; return 13; } };
        var s = 0;
        for (var r = 0; r < 3000; r++) {
          s = (s + ("17" % 5) + (true % 2) + (null % 3 === 0 ? 1 : 0) + (obj % 7)) | 0;
        }
        console.log(s + " " + calls);
        "#,
    );
}

/// Mod result feeding further double-tier arithmetic and a pinned typed-array
/// store in the same region (the bench composition), plus a remainder whose
/// magnitude needs more than 32 bits (the i64 idiv path, not a 32-bit one).
#[test]
fn mod_parity_wide_remainders_compose() {
    assert_matches_node(
        r#"
        "use strict";
        var NF = 20000;
        var x = new Float64Array(NF);
        var big = 281474976710597; // < 2^48, prime-ish
        var s = 0;
        for (var i = 0; i < NF; i++) {
          var m = (i * 87654321 + 281474976710000) % big;
          x[i] = m / 65536;
          s += x[i] % 97;
        }
        console.log("s=" + s.toFixed(6) + " m=" + (281474976710600 % big));
        "#,
    );
}

/// Re-run every `mod_parity_` case in four more modes, each in its own child
/// process (the env latches are read once per process): the off-switch, the
/// pure interpreter, threshold-1, and GC stress. The same node-derived
/// assertions passing in all modes IS the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_DOUBLE_MOD", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("mod_parity_")
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
            "the mod_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
