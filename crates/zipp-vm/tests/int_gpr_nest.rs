//! B119: nested loops on the GPR-home INT tier (the wave-6 named residual).
//!
//! When a GPR-eligible inner loop nests inside an enclosing region, OSR enters
//! whichever region hosts the OUTER back-edge once it compiles — so an outer
//! region that fell back to xmm homes (its one-home-per-value live set
//! overflowed the small GPR pool) shadowed an engaged GPR inner and the nest
//! ran at xmm speed. The fix re-plans an overflowed region with forced home
//! sharing (`plan_region_cold`'s `share_homes` — the same linear-scan reuse
//! that already serves >14-home xmm regions), which collapses the nest's
//! non-overlapping temps onto a few homes so the OUTER region itself goes GPR.
//!
//! Every `gprnest_parity_*` case asserts byte-identical output against
//! `node -e` at DEFAULT settings. The final test re-runs the set in five more
//! modes in child processes: `ZIPP_NO_GPR_NEST=1` (the retry alone off — the
//! wave-6 behavior), `ZIPP_NO_GPR_HOMES=1` (the whole sub-mode off),
//! `ZIPP_JIT_THRESHOLD=1`, `ZIPP_GC_STRESS=1`, and `ZIPP_NOJIT=1`. The shapes
//! are the exit-heavy nest forms where a lost flush or a wrongly shared home
//! is a wrong ANSWER: inner hot / outer cold, both loops hot, inner breaking
//! and throwing out to the outer body, an outer-level deopt while the inner
//! is engaged, and a nest too wide for the pool even shared (the xmm
//! fallback must still answer).

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

/// Inner hot, outer cold: the outer back-edge never reaches the OSR threshold,
/// so only the inner region compiles (and GPR-engages) — the pre-B119 happy
/// path must be untouched.
#[test]
fn gprnest_parity_inner_hot_outer_cold() {
    assert_matches_node(
        r#"
        "use strict";
        function f(seed, M) {
          var x = seed | 0;
          var acc = 0;
          for (var r = 0; r < 3; r++) {
            for (var i = 0; i < M; i++) {
              x ^= x << 13;
              x ^= x >>> 17;
              x ^= x << 5;
            }
            acc = (acc + (x ^ r)) | 0;
          }
          return acc + "," + x + "," + i;
        }
        console.log(f(88172645, 5000), f(424242, 64));
        "#,
    );
}

/// Both loops hot: the outer region compiles too and — via the shared-home
/// re-plan — hosts the whole nest on GPR homes. The pure-register form and the
/// pinned-Int32Array-store form (an extra snapshot + masked SetIndex in the
/// inner body) both must answer exactly like node.
#[test]
fn gprnest_parity_outer_hosts_nest() {
    assert_matches_node(
        r#"
        "use strict";
        function nest(seed, R, M) {
          var x = seed | 0;
          var acc = 0;
          for (var r = 0; r < R; r++) {
            for (var i = 0; i < M; i++) {
              x ^= x << 13;
              x ^= x >>> 17;
              x ^= x << 5;
            }
            acc = (acc + (x ^ r)) | 0;
          }
          return acc + "," + x;
        }
        var N = 256;
        var a = new Int32Array(N);
        function nestStore(seed, R, M, mask) {
          var x = seed | 0;
          for (var r = 0; r < R; r++) {
            for (var i = 0; i < M; i++) {
              x ^= x << 13;
              x ^= x >>> 17;
              x ^= x << 5;
              a[i & mask] = x;
            }
          }
          return x;
        }
        console.log(nest(88172645, 40, 500), nest(424242, 3, 50));
        var x = nestStore(20250804, 30, 400, N - 1);
        var s = 0;
        for (var i = 0; i < N; i++) s = (s + a[i]) | 0;
        console.log(x, s);
        "#,
    );
}

/// Exits from the middle of the nest: a plain `break` back to the outer body
/// and a labeled `break outer` straight out of both loops. Every such exit is
/// a region exit stub — all homes (including long-dead shared ones) must flush
/// values the frame can safely observe.
#[test]
fn gprnest_parity_break_to_outer_and_out() {
    assert_matches_node(
        r#"
        "use strict";
        function viaBreaks(R, M) {
          var x = 1, hit = 0, acc = 0;
          outer:
          for (var r = 0; r < R; r++) {
            for (var i = 0; i < M; i++) {
              x = (x ^ (i << 3) ^ r) | 0;
              if ((x & 1023) === 7) { hit = (hit + 1) | 0; break; }
              if (r === 37 && i === 11) { acc = (acc ^ x) | 0; break outer; }
            }
            acc = (acc + x) | 0;
          }
          return acc + "," + hit + "," + x + "," + r + "," + i;
        }
        console.log(viaBreaks(60, 300), viaBreaks(5, 40));
        "#,
    );
}

/// A throw from the inner loop caught in the outer body: the unwind must see
/// exactly the values the flush wrote, every outer pass, hot or not.
#[test]
fn gprnest_parity_inner_throw_caught_outside() {
    assert_matches_node(
        r#"
        "use strict";
        function viaThrow(R, M) {
          var h = 3, caught = 0;
          for (var r = 0; r < R; r++) {
            try {
              for (var i = 0; i < M; i++) {
                h = (h * 3) ^ (i + r);
                if (r === 20 && i === 15) undefined.x;
              }
            } catch (e) {
              caught = (caught + 1) | 0;
              h = (h + r) | 0;
            }
          }
          return h + "," + caught;
        }
        console.log(viaThrow(40, 60), viaThrow(6, 10));
        "#,
    );
}

/// An OUTER-level deopt while the inner is engaged: `p` grows past 2^53 in the
/// outer body (the i53 guard exits with `p` flushed as a double), after which
/// the outer region entry-bails on every attempt — the interpreter runs the
/// outer body while the inner loop keeps re-entering its own region. All three
/// tiers of that dance must agree with node to the last bit.
#[test]
fn gprnest_parity_outer_deopt_while_inner_engaged() {
    assert_matches_node(
        r#"
        "use strict";
        function f(R, M) {
          var x = 5, p = 3, acc = 0;
          for (var r = 0; r < R; r++) {
            for (var i = 0; i < M; i++) {
              x ^= x << 13;
              x ^= x >>> 17;
              x ^= x << 5;
            }
            p = p * 3;
            acc = (acc + (x & 65535) + (p % 97)) | 0;
          }
          return acc + "," + x + "," + p;
        }
        console.log(f(60, 200));
        "#,
    );
}

/// Too wide even shared: nine loop-carried accumulators are all live-in
/// (permanent homes — sharing cannot merge them), so the re-plan still
/// overflows the GPR pool and the region must fall back to the xmm emitter
/// with the ORIGINAL distinct-homes plan, answering identically.
#[test]
fn gprnest_parity_shared_replan_still_overflows() {
    assert_matches_node(
        r#"
        "use strict";
        function wide(R, M) {
          var a0 = 1, a1 = 2, a2 = 3, a3 = 4, a4 = 5, a5 = 6, a6 = 7, a7 = 8, a8 = 9;
          for (var r = 0; r < R; r++) {
            for (var i = 0; i < M; i++) {
              a0 = (a0 + (a8 ^ i)) | 0;
              a1 = (a1 ^ (a0 << 1)) | 0;
              a2 = (a2 + (a1 >>> 2)) | 0;
              a3 = (a3 ^ (a2 + r)) | 0;
              a4 = (a4 + (a3 & 255)) | 0;
              a5 = (a5 ^ (a4 << 3)) | 0;
              a6 = (a6 + (a5 >>> 1)) | 0;
              a7 = (a7 ^ (a6 + i)) | 0;
              a8 = (a8 + (a7 & 1023)) | 0;
            }
          }
          return [a0, a1, a2, a3, a4, a5, a6, a7, a8].join(",");
        }
        console.log(wide(25, 120), wide(2, 7));
        "#,
    );
}

/// The B119 mechanism itself, pinned: on the both-hot nest the outer region's
/// first GPR attempt overflows, the shared-home re-plan runs, and the SAME
/// region then engages GPR. Asserted from a child process's ZIPP_JITLOG (the
/// parity filter re-run with logging), so a planner change that silently stops
/// the outer region from engaging fails loudly here, not in a benchmark.
#[test]
fn gprnest_mechanism_outer_region_engages_gpr() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg("gprnest_parity_outer_hosts_nest")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NO_GPR_HOMES")
        .env_remove("ZIPP_NO_GPR_NEST")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_NOJIT")
        .output()
        .expect("spawn the test binary");
    assert!(
        out.status.success(),
        "logged re-run failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stderr);
    // "[jit] INT-GPR nest retry [A,B]: shared-home re-plan" → the same [A,B]
    // must then appear as "INT region [A,B] GPR homes engaged".
    let retry = log
        .lines()
        .find(|l| l.contains("INT-GPR nest retry"))
        .unwrap_or_else(|| panic!("no shared-home re-plan fired on the nested shape:\n{log}"));
    // "…nest retry [A,B]: …" — the span is the SECOND bracketed field (the
    // first is the "[jit]" prefix).
    let span = retry
        .split(['[', ']'])
        .nth(3)
        .expect("retry line carries the region span");
    assert!(
        log.contains(&format!("INT region [{span}] GPR homes engaged")),
        "re-plan for [{span}] did not engage GPR homes:\n{log}"
    );
}

/// Everything above must answer identically in every mode. The parity tests
/// re-run in child processes; their node-derived assertions passing in all
/// modes IS the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 5] = [
        &[("ZIPP_NO_GPR_NEST", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("gprnest_parity_");
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
            "the gprnest_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}
