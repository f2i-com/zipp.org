//! B118: GPR homes for bitwise/int-chain regions on the INTEGER tier.
//!
//! `region_int_gpr.rs` re-emits an admitted INT region with each numeric home
//! in a general-purpose register instead of an xmm low quadword, eliminating
//! the three xmm↔gpr `movq`s every `Bitwise`/`Math.imul` op paid on the loop's
//! serial dependency chain (the fnv1a hash and xorshift fill shapes measured
//! 5-10x off node on the xmm-home tier from exactly that traffic).
//!
//! Every `gprhome_parity_*` case asserts byte-identical output against
//! `node -e` at DEFAULT settings (the mode ON). The final test re-runs the set
//! in four more modes in child processes: `ZIPP_NO_GPR_HOMES=1` (the xmm-home
//! emitter — the off-switch must be a pure fallback), `ZIPP_JIT_THRESHOLD=1`
//! (compile everything immediately), `ZIPP_GC_STRESS=1` (every flushed home
//! must box to a traceable Value at every exit), and `ZIPP_NOJIT=1` (pure
//! interpreter). The shapes lean on the B97 lesson: a lost entry load or a
//! garbage home flush is a wrong ANSWER only an exit-heavy shape can see, so
//! the cases exercise break/return/throw exits, mid-loop deopts (a value going
//! non-int, an i32 `<<` result crossing tags), and defs on untaken branches.

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

/// The fnv1a shape from bench/real/regex-log-scan.js — pinned-string
/// `charCodeAt` feeding `^` and `Math.imul` on a loop-carried accumulator.
#[test]
fn gprhome_parity_fnv1a_hash() {
    assert_matches_node(
        r#"
        "use strict";
        function fnv1a(str) {
          var h = 0x811c9dc5;
          for (var i = 0; i < str.length; i++) {
            h = Math.imul(h ^ str.charCodeAt(i), 16777619);
          }
          return h >>> 0;
        }
        var s = "";
        for (var i = 0; i < 400; i++) s += String.fromCharCode(32 + (i * 7) % 90);
        var acc = 0;
        for (var r = 0; r < 50; r++) acc = (acc + fnv1a(s)) | 0;
        console.log(acc, fnv1a(""), fnv1a("a"));
        "#,
    );
}

/// The xorshift32 Int32Array fill — three shift/xor pairs on one carried
/// value plus a masked pinned store.
#[test]
fn gprhome_parity_xorshift_fill() {
    assert_matches_node(
        r#"
        "use strict";
        var N = 1024;
        var a = new Int32Array(N);
        function fill(seed, M, mask) {
          var x = seed | 0;
          for (var i = 0; i < M; i++) {
            x ^= x << 13;
            x ^= x >>> 17;
            x ^= x << 5;
            a[i & mask] = x;
          }
          return x;
        }
        var x = fill(88172645, 50000, N - 1);
        var s = 0;
        for (var i = 0; i < N; i++) s = (s + a[i]) | 0;
        console.log(x, s);
        "#,
    );
}

/// i32 boundary wraps: `<<`/`>>>`/`|0` at ±2^31, plus shift counts ≥ 32
/// (JS masks the count to 5 bits) and a `>>>` result above 2^31 (boxes as a
/// double, not a negative Int).
#[test]
fn gprhome_parity_i32_boundary_wraps() {
    assert_matches_node(
        r#"
        "use strict";
        function grind(n) {
          var lo = 0, hi = 0, u = 0, w = 0;
          var v = 2147483645;
          for (var i = 0; i < n; i++) {
            v = (v + 1) | 0;              // wraps +2^31-1 -> -2^31
            lo = v << 1;                  // sign-bit churn
            hi = (v >>> 1) ^ lo;
            u = (v >>> 0) & 0x7fffffff;
            w = (w + (v << 33) + (v >>> 34)) | 0;  // counts masked to 1 and 2
          }
          return [v, lo, hi, u, w].join(",");
        }
        console.log(grind(64), grind(1000));
        var big = -1 >>> 0;               // 4294967295 — exceeds i32 on exit
        console.log(big, big >>> 1, (big >>> 0) + 1);
        "#,
    );
}

/// Mixed int/double flowing through the SAME variables: the loop runs the
/// bitwise chain while ints, then a value turns fractional mid-loop (deopt to
/// the interpreter; the flushed homes must hold the exact pre-deopt values)
/// and integral again (the region re-enters through the integral-double entry
/// guard).
#[test]
fn gprhome_parity_mid_loop_deopt_and_reentry() {
    assert_matches_node(
        r#"
        "use strict";
        function mix(n) {
          var x = 1, s = 0;
          for (var i = 0; i < n; i++) {
            x = (x ^ (i << 2)) | 0;
            if (i === 700) x = x + 0.5;   // goes non-int mid-loop
            if (i === 800) x = (x - 0.5) | 0; // back to int -> re-enter
            s = (s + x) | 0;
          }
          return s;
        }
        console.log(mix(50), mix(1000), mix(2000));
        // Accumulator crossing 2^31: exit boxes a double, re-entry must accept it.
        function grow(n) {
          var s = 1073741824, t = 0;
          for (var i = 0; i < n; i++) { s = s + 1073741824; t = t ^ (s & 1023); }
          return s + "," + t;
        }
        console.log(grow(4), grow(64));
        "#,
    );
}

/// Exits via break / return / throw: every path out of the region must flush
/// every home (including values whose last def was many iterations back).
#[test]
fn gprhome_parity_break_return_throw_exits() {
    assert_matches_node(
        r#"
        "use strict";
        function viaBreak(n) {
          var h = 7, k = 0;
          for (var i = 0; i < n; i++) {
            h = Math.imul(h ^ i, 31);
            if (i === 900) { k = h | 1; break; }
          }
          return h + "," + k + "," + i;
        }
        function viaReturn(n) {
          var h = 7;
          for (var i = 0; i < n; i++) {
            h = (h + (i << 1)) | 0;
            if (h > 100000) return h ^ i;
          }
          return -1;
        }
        function viaThrow(n) {
          var h = 3;
          try {
            for (var i = 0; i < n; i++) {
              h = (h * 3) ^ i;
              if (i === 500) undefined.x;
            }
          } catch (e) {
            return h + "," + i;
          }
          return "no-throw";
        }
        console.log(viaBreak(5000), viaReturn(5000), viaThrow(5000));
        "#,
    );
}

/// The B97 shape: a def sitting on a branch that NEVER runs. Its register
/// still gets a home and is flushed on every exit — the entry load is what
/// makes that flush write back the value the frame already held instead of
/// garbage (a hoisted-const twin sits on the taken side so the region still
/// engages the bitwise mode).
#[test]
fn gprhome_parity_untaken_branch_def() {
    assert_matches_node(
        r#"
        "use strict";
        function f(n, flag) {
          var ghost = 42;      // only re-defined on the untaken branch
          var h = 1;
          for (var i = 0; i < n; i++) {
            if (flag) { ghost = 1000000; }
            h = (h ^ (ghost + i)) | 0;
          }
          return h + "," + ghost;
        }
        console.log(f(2000, false), f(50, false));
        "#,
    );
}

/// Negative operands through every admitted op: `%` keeping the dividend's
/// sign, `-0` from Neg bailing, signed vs unsigned shifts of negatives.
#[test]
fn gprhome_parity_negatives_and_mod() {
    assert_matches_node(
        r#"
        "use strict";
        function f(n) {
          var s = 0, m = 0;
          for (var i = -n; i < n; i++) {
            m = (i % 7) ^ (i >> 3);
            s = (s + m + ((i >>> 5) & 63)) | 0;
          }
          return s + "," + m;
        }
        console.log(f(1500));
        function negz(n) {
          var z = 0, s = 0;
          for (var i = 0; i < n; i++) {
            z = -(s & 0);          // -0: the Neg arm must bail, not box +0
            s = (s + i) | 0;
          }
          return (1 / z) + "," + s;
        }
        console.log(negz(600));
        "#,
    );
}

/// Everything above must answer identically in every mode. The parity tests
/// re-run in child processes; their node-derived assertions passing in all
/// modes IS the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 4] = [
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("gprhome_parity_");
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
            "the gprhome_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}
