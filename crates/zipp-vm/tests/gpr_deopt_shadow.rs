//! Deferred-deopt raw shadows for call-free pinned-DataView INT-GPR regions.
//!
//! Each parity case has a distinct eligible exit: a throwing DV deopt, an
//! out-of-region branch, an elided constant Mul guard, an i53 guard, and
//! entry_bail's cvttsd2si `i64::MIN` sentinel. Return is deliberately refused
//! by V1 because today's upstream DV pin planner cannot produce a pinned region
//! with an in-body Return.
//! Node is the oracle; child-process modes cover the kill switch, tier
//! fallbacks, immediate compilation, GC stress and the interpreter.

//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the interpreter-only profiles never emit; compiled only where that tier exists, like the other tier-pinning suites.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

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

/// The census below describes the fused method-call lowering, and these
/// fixtures read their operands from top-level `var`s (global reads), which
/// the strict default lowering (B280) captures rather than fuses. The logged
/// child therefore opts into the relaxed lowering; the parity tests above run
/// under the default, so both lowerings stay checked against node.
fn logged_child(test: &str, extra: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(test)
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_RELAXED_CALL_ORDER", "1")
        .env_remove("ZIPP_STRICT_CALL_ORDER")
        .env_remove("ZIPP_NO_GPR_DEOPT_SHADOW")
        .env_remove("ZIPP_NO_DV_GPR")
        .env_remove("ZIPP_NO_GPR_HOMES")
        .env_remove("ZIPP_NO_GLOB_RANGE")
        // The register-allocation regime is an axis these censuses depend on
        // (07b400dc, 2026-09-02): cleared here so the default-mode cases are
        // measured under today's lowering, and re-set by `extra` for the
        // legacy-allocation test.
        .env_remove("ZIPP_NO_REG_CLASSES")
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

/// Assert the single `GPR deopt-shadow engaged` census line of each fixture,
/// and that the fixtures with a dynamic exit still take one.
fn assert_shadow_census(cases: &[(&str, &str, Option<&str>)], extra: &[(&str, &str)]) {
    for &(test, expected_line, dynamic_exit) in cases {
        let on = logged_child(test, extra);
        let lines: Vec<&str> = on
            .lines()
            .filter(|line| line.contains("GPR deopt-shadow engaged"))
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "expected one shadow region for {test} under {extra:?}:\n{on}"
        );
        assert!(
            lines[0].contains(expected_line),
            "wrong shadow census for {test} under {extra:?}:\n{}",
            lines[0]
        );
        if let Some(exit) = dynamic_exit {
            assert!(
                on.contains(exit),
                "the intended dynamic exit did not run for {test}:\n{on}"
            );
        }
    }
}

/// Non-vacuity and kill-switch gate. Four fixtures remain inside V1's closed
/// proof; their structural shadow census is fixed without pinning bytecode
/// spans, which legitimately move as the compiler's temporary layout changes.
///
/// ── 07b400dc (2026-09-02), "Give booleans and global receivers their own
/// register classes" ── these numbers were rewritten wholesale on that
/// commit's evidence, and they moved DOWN. A plain global receiver now takes a
/// RECV-class register that is never reclaimed, so the region has no B94 split
/// receiver: there is no receiver shadow to carry (`regs` 3 → 2) and no
/// receiver reset at its `LoadGlobal` (`recv-resets` 1 → 0), which in turn
/// removes that register's shadow stores (`reg-writes` 6 → 4 on the boundary
/// fixture). Every fixture publishes the same observable globals as before —
/// `glob-writes` is unchanged on all four — so this is less shadow bookkeeping
/// for identical exit state, not a weaker guarantee. The mechanism still
/// engages on every fixture, which is what the exact `regs=`/`reg-writes=`
/// counts here pin; `shadow_mechanism_type_split_guard_under_legacy_alloc`
/// pins the pre-07b400dc census and the type-split refusal under that commit's
/// own latch.
#[test]
fn shadow_mechanism_engages_and_switch_falls_back() {
    assert_shadow_census(
        &[
            (
                "shadow_parity_loop_boundary_side_exit",
                "regs=2 globs=5 reg-writes=4 glob-writes=5 recv-resets=0",
                None,
            ),
            (
                "shadow_parity_elided_mul_loop_bound",
                "regs=2 globs=2 reg-writes=2 glob-writes=2 recv-resets=0",
                None,
            ),
            (
                "shadow_parity_i53_guard_exit",
                "regs=2 globs=2 reg-writes=3 glob-writes=2 recv-resets=0",
                Some("deopt at ip"),
            ),
            (
                "shadow_parity_i64_min_entry_bail",
                "regs=2 globs=3 reg-writes=4 glob-writes=3 recv-resets=0",
                Some("deopt at ip"),
            ),
        ],
        &[],
    );

    // The throw fixture. Before 07b400dc its endian Bool shared a register
    // with a number, the raw-Int shadow refused that type-split register, and
    // the whole shadow declined. BOOL-class registers removed the type split,
    // so the shadow now engages here too — but with `regs=0`: not one raw
    // register is shadowed, only the two wide globals the interpreter must see
    // before it re-executes the throwing call. Pinning `regs=0` exactly is what
    // keeps this arm falsifiable; a nonzero count would mean a raw register
    // shadow has appeared on the one fixture whose deopt re-enters the
    // interpreter through a throw.
    let throw = logged_child(
        "shadow_parity_throw_deopt_receiver_reload_and_wide_globals",
        &[],
    );
    let throw_lines: Vec<&str> = throw
        .lines()
        .filter(|line| line.contains("GPR deopt-shadow engaged"))
        .collect();
    assert_eq!(
        throw_lines.len(),
        1,
        "expected one shadow region for the throw fixture:\n{throw}"
    );
    assert!(
        throw_lines[0].contains("regs=0 globs=2 reg-writes=0 glob-writes=2 recv-resets=0"),
        "the throw fixture must shadow its two wide globals and NO raw \
         register:\n{}",
        throw_lines[0]
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

/// The V1 shadow's TYPE-SPLIT REFUSAL, which only the pre-07b400dc allocation
/// can exercise.
///
/// A W28 type-split register holds a Bool over one range and a number over
/// another; the raw-Int shadow cannot represent that and declines the whole
/// region rather than guess. Since 07b400dc a syntactically boolean expression
/// takes a BOOL-class register, so no register is type-split and the guard has
/// nothing to refuse — under the default the throw fixture reaches a `regs=0`
/// shadow instead (pinned above). `ZIPP_NO_REG_CLASSES=1` is the latch that
/// commit shipped; it restores the v0.0.5 reclaim, and with it both the
/// type-split register and the pre-07b400dc census of all four closed-proof
/// fixtures. Keeping this test is what stops a change to the refusal from
/// going unnoticed, and keeping its census exact is what stops it going
/// vacuous.
#[test]
fn shadow_mechanism_type_split_guard_under_legacy_alloc() {
    const LEGACY_ALLOC: &[(&str, &str)] = &[("ZIPP_NO_REG_CLASSES", "1")];
    assert_shadow_census(
        &[
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
        ],
        LEGACY_ALLOC,
    );

    let throw = logged_child(
        "shadow_parity_throw_deopt_receiver_reload_and_wide_globals",
        LEGACY_ALLOC,
    );
    assert!(
        throw.contains("type-split r"),
        "the throw fixture no longer produces a type-split register, so the \
         refusal below is not being exercised:\n{throw}"
    );
    assert!(
        !throw.contains("GPR deopt-shadow engaged"),
        "the type-split throw fixture bypassed a closed V1 shadow guard:\n{throw}"
    );
    assert!(
        throw.contains("GPR homes engaged"),
        "the guarded throw fixture should retain the incumbent GPR tier:\n{throw}"
    );
    assert!(
        throw.contains("deopt at ip"),
        "the guarded throw fixture did not execute its intended dynamic exit:\n{throw}"
    );
}

/// Every exit case remains node-identical under the shadow-off incumbent, the
/// lower tiers, immediate OSR, GC stress and the pure interpreter.
#[test]
fn shadow_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 7] = [
        &[("ZIPP_NO_GPR_DEOPT_SHADOW", "1")],
        &[("ZIPP_NO_DV_GPR", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
        // 07b400dc's latch (the v0.0.5 reclaim), which is the allocation
        // `shadow_mechanism_type_split_guard_under_legacy_alloc` measures: the
        // split-receiver shadow and the type-split refusal both live there, so
        // that route stays checked against node rather than only asserted.
        &[("ZIPP_NO_REG_CLASSES", "1")],
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
