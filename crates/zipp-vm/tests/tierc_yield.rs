//! W9: Tier-C yield — while a function owns a live register-homed loop region
//! the whole-function Tier C offer is declined (`Jit::should_yield_to_region`)
//! and the interpreter's back-edge keeps entering the region, instead of a
//! Tier C body shadowing it forever with mem-homed loop code (B121's 12x
//! tier-SELECTION gap; fn-scoped fnv1a measured 10.6 → 4.9 ns/iter).
//!
//! Semantics must be tier-invariant, so each case asserts byte-identical
//! output against `node -e`, and the final test re-runs the set under
//! `ZIPP_NO_TIERC_YIELD=1` (the off-switch — the old always-compile-Tier-C
//! behaviour), `ZIPP_NOJIT=1` and `ZIPP_JIT_THRESHOLD=1` in child processes.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

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

/// The exact B121 shape: a small function whose INT-GPR loop region compiles
/// during call #1 and whose Tier C offer trips at call #8 — the yield declines
/// it and every later call re-enters the region via OSR.
#[test]
fn tcy_parity_fn_scoped_fnv1a() {
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
        var lines = [];
        for (var i = 0; i < 64; i++) {
          var s = "";
          for (var j = 0; j < 48; j++) s += String.fromCharCode(97 + ((i * 31 + j * 7) % 26));
          lines.push(s);
        }
        var acc = 0;
        for (var c = 0; c < 30000; c++) acc = (acc + fnv1a(lines[c & 63])) | 0;
        console.log(acc);
        "#,
    );
}

/// A function with BOTH a hot register-homed loop and real work after it: the
/// yield keeps the whole function interpreted outside the region, and the
/// answer must not change (the loop still runs native via OSR).
#[test]
fn tcy_parity_loop_plus_tail() {
    assert_matches_node(
        r#"
        "use strict";
        function mix(o) {
          var h = 0;
          for (var i = 0; i < 200; i++) h = (h + ((i ^ h) >>> 3) + i * 7) | 0;
          o.h = h;
          return o.h + o.k;
        }
        var t = 0;
        for (var c = 0; c < 5000; c++) t = (t + mix({ k: c, h: 0 })) | 0;
        console.log(t);
        "#,
    );
}

/// A MEM-region function keeps Tier C (the carve-out: MEM per-op code equals
/// Tier C's, so nothing is declined) — the answer is the same either way,
/// pinned so the carve-out can never silently invert.
#[test]
fn tcy_parity_mem_region_keeps_tier_c() {
    assert_matches_node(
        r#"
        "use strict";
        function build(n) {
          var out = {};
          for (var i = 0; i < 40; i++) out["k" + (i & 7)] = i + n;
          return out.k0 + out.k7;
        }
        var t = 0;
        for (var c = 0; c < 5000; c++) t = (t + build(c)) | 0;
        console.log(t);
        "#,
    );
}

/// Re-run every `tcy_parity_` case with the yield off, the JIT off, and the
/// JIT forced hot, each in a child process.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_TIERC_YIELD", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("tcy_parity_")
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
            "the tcy_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
