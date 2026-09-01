//! Deferred-deopt raw shadows for call-free pinned-DataView INT-GPR regions.
//!
//! Each parity case has a distinct eligible exit: a throwing DV deopt, an
//! out-of-region branch, an elided constant Mul guard, an i53 guard, and
//! entry_bail's cvttsd2si `i64::MIN` sentinel. Return is deliberately refused
//! by V1 because today's upstream DV pin planner cannot produce a pinned region
//! with an in-body Return.
//! Node is the oracle; child-process modes cover the kill switch, tier
//! fallbacks, immediate compilation, GC stress and the interpreter.

const PRELUDE: &str = r#"
"use strict";
var shadowBuf = new ArrayBuffer(4096);
var shadowU8 = new Uint8Array(shadowBuf);
for (var shadowFill = 0; shadowFill < 4096; shadowFill++) shadowU8[shadowFill] = 255;
var shadowDv = new DataView(shadowBuf);
"#;

fn program(body: &str) -> String {
    format!("{PRELUDE}\n{body}")
}

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
        .expect("node v24 on PATH (oracle)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn assert_matches_node(src: &str, expected: &[&str]) {
    let actual = run_ok(src);
    assert_eq!(actual, node_output(src), "zipp != node for:\n{src}");
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    assert_eq!(
        actual, expected,
        "exit sentinel was not reached for:\n{src}"
    );
}

/// The last receiver LoadGlobal executes, resets its split shadow EMPTY, then
/// getInt8 deopts before defining that recycled register. Other raw shadows
/// contain +/−4294967295 and must be boxed into observable globals before the
/// interpreter re-executes the call and throws RangeError.
#[test]
fn shadow_parity_throw_deopt_receiver_reload_and_wide_globals() {
    assert_matches_node(
        &program(
            r#"
var shadowWide = 0, shadowNegative = 0, shadowSum = 0, shadowCaught = "none";
function shadowThrowScan() {
  for (var shadowO = 0; shadowO <= 4096; shadowO += 4) {
    var shadowLe = (shadowO >> 2) & 1;
    var shadowSafe = shadowO & 4092;
    var shadowV = shadowDv.getUint32(shadowSafe, shadowLe === 1);
    shadowWide = shadowV;
    shadowNegative = -shadowWide;
    shadowSum = (shadowSum + (shadowV >>> 24) + (shadowV & 255) +
      shadowDv.getUint16(shadowSafe, shadowLe === 0) + shadowDv.getInt8(shadowO + 2)) | 0;
  }
}
try { shadowThrowScan(); }
catch (e) { shadowCaught = e.constructor.name; }
console.log(shadowWide, shadowNegative, shadowSum, shadowCaught,
  typeof shadowWide, typeof shadowNegative);
"#,
        ),
        &["4294967295 -4294967295 67629056 RangeError number number"],
    );
}

/// The loop header's normal out-of-region branch must publish the latest raw
/// logical globals, including a value above i32 and one below it. (A literal
/// `break` makes today's pin planner reject the region, so it cannot be a
/// non-vacuous shadow fixture; this reaches the same flush-exit stub class.)
#[test]
fn shadow_parity_loop_boundary_side_exit() {
    assert_matches_node(
        &program(
            r#"
var breakWide = 0, breakNegative = 0, breakAt = -1;
for (var breakO = 0; breakO < 2052; breakO += 4) {
  var breakLe = (breakO >> 2) & 1;
  var breakV = shadowDv.getUint32(breakO, breakLe === 1);
  breakWide = breakV;
  breakNegative = -breakWide;
  breakAt = breakO;
}
console.log(breakWide, breakNegative, breakAt, typeof breakWide);
"#,
        ),
        &["4294967295 -4294967295 2048 number"],
    );
}

/// The typed-array benchmark spells its loop bound as a constant multiply.
/// V1 admits Mul only when the existing range proof has already elided its
/// guard, keeping every raw result inside the sentinel-safe i53 domain.
#[test]
fn shadow_parity_elided_mul_loop_bound() {
    assert_matches_node(
        &program(
            r#"
var mulWide = 0, mulSum = 0;
for (var mulO = 0; mulO < 256 * 4; mulO += 4) {
  var mulV = shadowDv.getUint32(mulO);
  mulWide = mulV;
  mulSum = (mulSum + (mulV | 0)) | 0;
}
console.log(mulWide, mulSum, typeof mulWide, typeof mulSum);
"#,
        ),
        &["4294967295 -256 number number"],
    );
}

/// Repeated u32 additions cross 2^53 well after OSR without conditional CFG.
/// The i53 guard exits after the Add but before StoreGlobal; every previously
/// updated shadow must be published while the guarded dst is boxed normally.
#[test]
fn shadow_parity_i53_guard_exit() {
    assert_matches_node(
        &program(
            r#"
var guardWide = 0, guardAcc = 9000000000000000, guardMix = 0;
for (var guardO = 0; guardO < 4096; guardO++) {
  var guardV = shadowDv.getUint32(0);
  guardWide = guardV;
  guardAcc = guardAcc + guardV;
  guardMix = (guardMix + (guardV | 0)) | 0;
}
console.log(guardWide, guardAcc, guardMix,
  typeof guardWide, typeof guardAcc, typeof guardMix);
"#,
        ),
        &["4294967295 9017592186042740 -4096 number number number"],
    );
}

/// cvttsd2si maps the exact double -2^63 to i64::MIN. Entry admission must
/// reject it before any raw home/shadow write, and entry_bail must restore
/// without interpreting the high-word EMPTY markers as values.
#[test]
fn shadow_parity_i64_min_entry_bail() {
    assert_matches_node(
        &program(
            r#"
var minHuge = -9223372036854775808, minWide = 0, minCount = 0;
for (var minO = 0; minO < 4096; minO += 4) {
  var minLe = (minO >> 2) & 1;
  var minV = shadowDv.getUint32(minO, minLe === 1);
  minWide = minV;
  minCount = (minCount + (minHuge | 0) + 1) | 0;
}
console.log(minHuge, minWide, minCount, typeof minHuge, typeof minWide);
"#,
        ),
        &["-9223372036854776000 4294967295 1024 number number"],
    );
}

fn logged_child(test: &str, extra: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(test)
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NO_GPR_DEOPT_SHADOW")
        .env_remove("ZIPP_NO_DV_GPR")
        .env_remove("ZIPP_NO_GPR_HOMES")
        .env_remove("ZIPP_NO_GLOB_RANGE")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_GC_STRESS")
        .env_remove("ZIPP_NOJIT");
    for (key, val) in extra {
        cmd.env(key, val);
    }
    let out = cmd.output().expect("spawn logged test child");
    assert!(
        out.status.success(),
        "logged {test} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Non-vacuity and kill-switch gate. Four fixtures remain inside V1's closed
/// proof; their structural shadow census is fixed without pinning bytecode
/// spans, which legitimately move as the compiler's temporary layout changes.
/// The throw fixture needs a Bool/Num type split, which the raw-Int shadow
/// deliberately refuses, so it must retain the incumbent GPR path and execute
/// its dynamic deopt without weakening that shadow gate.
#[test]
fn shadow_mechanism_engages_and_switch_falls_back() {
    let cases = [
        // Census values follow the compiler's temporary layout (region spans
        // are deliberately not pinned — they move with it). These are the
        // pre-hardening values: the fused method-call lowering leaves no
        // receiver snapshot and no post-call register reset behind for these
        // four fixtures.
        (
            "shadow_parity_loop_boundary_side_exit",
            "regs=3 globs=5 reg-writes=6 glob-writes=5 recv-resets=1",
            None,
        ),
        (
            "shadow_parity_elided_mul_loop_bound",
            "regs=3 globs=2 reg-writes=3 glob-writes=2 recv-resets=1",
            None,
        ),
        (
            "shadow_parity_i53_guard_exit",
            "regs=3 globs=2 reg-writes=4 glob-writes=2 recv-resets=1",
            Some("deopt at ip"),
        ),
        (
            "shadow_parity_i64_min_entry_bail",
            "regs=3 globs=3 reg-writes=6 glob-writes=3 recv-resets=1",
            Some("deopt at ip"),
        ),
    ];
    for (test, expected_line, dynamic_exit) in cases {
        let on = logged_child(test, &[]);
        let lines: Vec<&str> = on
            .lines()
            .filter(|line| line.contains("GPR deopt-shadow engaged"))
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "expected one shadow region for {test}:\n{on}"
        );
        assert!(
            lines[0].contains(expected_line),
            "wrong shadow census for {test}:\n{}",
            lines[0]
        );
        if let Some(exit) = dynamic_exit {
            assert!(
                on.contains(exit),
                "the intended dynamic exit did not run for {test}:\n{on}"
            );
        }
    }

    let throw = logged_child(
        "shadow_parity_throw_deopt_receiver_reload_and_wide_globals",
        &[],
    );
    assert!(
        !throw.contains("GPR deopt-shadow engaged"),
        "the type-split throw fixture bypassed a closed V1 shadow guard:\n{throw}"
    );
    assert!(
        throw.contains("type-split r"),
        "the throw fixture no longer demonstrates the type-split shadow guard:\n{throw}"
    );
    assert!(
        throw.contains("GPR homes engaged"),
        "the guarded throw fixture should retain the incumbent GPR tier:\n{throw}"
    );
    assert!(
        throw.contains("deopt at ip"),
        "the guarded throw fixture did not execute its intended dynamic exit:\n{throw}"
    );

    let test = "shadow_parity_loop_boundary_side_exit";
    let off = logged_child(test, &[("ZIPP_NO_GPR_DEOPT_SHADOW", "1")]);
    assert!(
        !off.contains("GPR deopt-shadow engaged"),
        "kill switch still engaged the shadow path:\n{off}"
    );
    assert!(
        off.contains("GPR homes engaged"),
        "kill switch should retain the incumbent GPR tier:\n{off}"
    );
}

/// Every exit case remains node-identical under the shadow-off incumbent, the
/// lower tiers, immediate OSR, GC stress and the pure interpreter.
#[test]
fn shadow_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 6] = [
        &[("ZIPP_NO_GPR_DEOPT_SHADOW", "1")],
        &[("ZIPP_NO_DV_GPR", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("shadow_parity_");
        for (key, val) in mode {
            cmd.env(key, val);
        }
        let out = cmd.output().expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "shadow parity filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}
