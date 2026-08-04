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
//! in five more modes in child processes: `ZIPP_NO_GPR_HOMES=1` (the xmm-home
//! emitter — the off-switch must be a pure fallback), `ZIPP_NO_GPR_LAZYSX=1`
//! (W8 deferred sign-extension off — immediate canonicalization must be a
//! pure fallback too), `ZIPP_JIT_THRESHOLD=1` (compile everything
//! immediately), `ZIPP_GC_STRESS=1` (every flushed home must box to a
//! traceable Value at every exit), and `ZIPP_NOJIT=1` (pure interpreter). The
//! shapes lean on the B97 lesson: a lost entry load or a garbage home flush is
//! a wrong ANSWER only an exit-heavy shape can see, so the cases exercise
//! break/return/throw exits, mid-loop deopts (a value going non-int, an i32
//! `<<` result crossing tags), and defs on untaken branches.
//!
//! The W8 (`gprhome_parity_imul_wrap…` onward) cases target the deferred
//! sign-extension invariant specifically: a LAZY home holds the zero-extended
//! low-32 form between defs, so every path that READS it as an i64 — array
//! index, compare, 64-bit add, `%`, unary minus, exit flush, deopt flush —
//! must observe the re-canonicalized value. Engagement of the GPR emitter
//! (not the xmm fallback) for these shapes was verified via ZIPP_JITLOG
//! (`… GPR homes engaged (N homes, 2 lazy-sx)` on every loop except `wide`,
//! which deliberately overflows the pool).

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

/// W8 lazy-sx: imul wrap at every i32 edge through the 3-operand/in-place
/// forms, and xor/or/and with negative operands on a LAZY accumulator (its
/// home holds the zero-extended form between defs — every printed value went
/// through an exit fix-up movsxd).
#[test]
fn gprhome_parity_imul_wrap_and_negative_bitwise() {
    assert_matches_node(
        r#"
        "use strict";
        function f(n) {
          var h = -1, p = 1, q = 0;
          for (var i = 0; i < n; i++) {
            h = Math.imul(h ^ (i - 512), -2147483648 + i); // sign-bit products
            p = Math.imul(p, 16777619);                    // wraps through 2^31 repeatedly
            q = (q & -7) | (h & 0x80000001);               // and/or with negatives
            q = q ^ (p | 0);
          }
          return h + "," + p + "," + q;
        }
        console.log(f(50), f(3000));
        console.log(Math.imul(-2147483648, -2147483648), Math.imul(2147483647, 2147483647));
        console.log(Math.imul(65536, 65536), Math.imul(-65536, 65535) | 0);
        "#,
    );
}

/// W8 mixed consumers: a lazy bitwise result feeding the i64-consuming arms.
/// Two loops sized to FIT the GPR pool (the one-function version counted 11
/// homes > 8 and fell back to xmm, exercising nothing): `idx` feeds a lazy
/// `&`-result to pinned SetIndex/GetIndex as the INDEX and to a 64-bit add;
/// `cmpadd` feeds one to a COMPARE, an add chain and a shift COUNT. Each use
/// re-canonicalizes and must read the same number node reads. (A third,
/// pool-overflowing mix with `%` and unary minus keeps the xmm-fallback
/// parity honest.)
#[test]
fn gprhome_parity_lazy_value_mixed_consumers() {
    assert_matches_node(
        r#"
        "use strict";
        var N = 256;
        var a = new Int32Array(N);
        function idx(n) {
          var s = 0;
          for (var i = 0; i < n; i++) {
            var k = (i * 31) & (N - 1);   // lazy: index + add consumers
            a[k] = k ^ i;
            s = (s + a[(k >>> 1) & (N - 1)] + k) | 0;
          }
          return s;
        }
        function cmpadd(n) {
          var s = 0, m = 0;
          for (var i = 0; i < n; i++) {
            var k = (i * 7) ^ (i & 63);   // lazy: compare/add/shift-count consumers
            if ((k ^ 3) < ((s & 1023) | 1)) m = (m + k) | 0;
            s = (s + k + (m << (k & 3))) | 0;
          }
          return s + "," + m;
        }
        function wide(n) {
          var s = 0, m = 0, r = 0;
          for (var i = 0; i < n; i++) {
            var k = (i * 31) & (N - 1);
            a[k] = k ^ i;
            m = a[(k >>> 1) & (N - 1)];
            if ((k ^ 3) < (m | 1)) s = s + k;
            s = (s + (k | 0) + m) | 0;
            r = (r + (k % 7) + (-(k | 1) | 0) + (m << (k & 3))) | 0;
          }
          return s + "," + m + "," + r;
        }
        console.log(idx(64), idx(5000));
        console.log(cmpadd(64), cmpadd(5000));
        console.log(wide(64), wide(5000));
        "#,
    );
}

/// W8 strict entry: a LAZY accumulator whose live-in value arrives OUTSIDE
/// i32 (an integral double). The strict entry guard must bail to the
/// interpreter — which applies real ToInt32 — rather than admit the wide
/// value into an i32-invariant home; the answers must stay node-identical
/// (mixed with iterations where the value is a plain i32 again, so the
/// region still runs).
#[test]
fn gprhome_parity_strict_entry_wide_livein() {
    assert_matches_node(
        r#"
        "use strict";
        function f(n, seed) {
          var h = seed;                    // 2^32+5 on the second call
          for (var i = 0; i < n; i++) {
            h = (h ^ i) | 0;               // lazy home: strict entry required
            h = Math.imul(h, 31);
          }
          return h;
        }
        console.log(f(1000, 1), f(1000, 4294967301), f(3, 4294967301));
        // A >>> result crossing i32 range keeps its home NON-lazy (u32 exit box).
        function g(n) {
          var u = 0, t = 0;
          for (var i = 0; i < n; i++) {
            u = (i * 2654435761) >>> 0;    // up to 4294967295
            t = (t + (u & 255)) | 0;
          }
          return u + "," + t;
        }
        console.log(g(700));
        "#,
    );
}

/// W8 deopt exits with a lazy accumulator LIVE: a pinned element going
/// non-int mid-loop (GetIndex deopt), an index going out of bounds
/// (charCodeAt-shaped deopt via a[k]), and an i53 guard failure — each exit
/// funnels through the flush fix-ups, so the boxed values must be exact.
#[test]
fn gprhome_parity_deopt_exits_with_lazy_accumulator() {
    assert_matches_node(
        r#"
        "use strict";
        function viaElementDeopt(n) {
          var arr = [];
          for (var i = 0; i < n; i++) arr.push(i);
          arr[600] = 0.5;                  // double element -> GetIndex deopt
          var h = 0x811c9dc5;
          for (var j = 0; j < n; j++) {
            h = Math.imul(h ^ arr[j], 16777619);
          }
          return h >>> 0;
        }
        function viaOob(s, n) {
          var h = 7;
          for (var i = 0; i < n; i++) {
            h = Math.imul(h ^ s.charCodeAt(i & 1023), 31); // in range
            if (i === 800) h = Math.imul(h ^ s.charCodeAt(5000), 31); // NaN -> deopt
          }
          return h | 0;
        }
        function viaGuard(n) {
          var h = 1, s = 1;
          for (var i = 0; i < n; i++) {
            h = (h ^ i) | 0;               // lazy
            s = s * 3;                     // i53 guard eventually fails
          }
          return h + "," + s;
        }
        var str = "";
        for (var i = 0; i < 1024; i++) str += String.fromCharCode(32 + (i % 90));
        console.log(viaElementDeopt(2000), viaOob(str, 2000), viaGuard(200));
        "#,
    );
}

/// Everything above must answer identically in every mode. The parity tests
/// re-run in child processes; their node-derived assertions passing in all
/// modes IS the parity check. `ZIPP_NO_GPR_LAZYSX=1` pins the W8 off-switch:
/// immediate canonicalization must be a pure fallback.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 5] = [
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_NO_GPR_LAZYSX", "1")],
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
