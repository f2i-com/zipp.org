//! Call-result pinned-string coverage.
//!
//! A MEM region may call a function and immediately scan the returned string.
//! Safe call exits refresh or identity-check pin snapshots after user code, so
//! a flat ASCII result can use the direct `.length` / `charCodeAt` lanes.
//! Non-strings, ropes and non-ASCII strings take the exact generic path.

const STABLE: &str = r#"
"use strict";
function makeText(i) {
  if (i < 0) return ["unreachable"].join("");
  return (i & 1) ? "abcdefgh" : "ABCDEFGH";
}
function scan(n) {
  var h = 1;
  for (var i = 0; i < n; i++) {
    var text = makeText(i);
    h = (Math.imul(h, 33) + text.length + text.charCodeAt(i & 7)) | 0;
  }
  return h;
}
console.log(scan(20000));
"#;

const MIXED: &str = r#"
"use strict";
function makeValue(i) {
  switch (i & 7) {
    case 0: return "plainASCII";
    case 1: return "café";
    case 2: return "left" + (i & 1 ? "R" : "S");
    case 3: return 17;
    case 4: return Object("boxed");
    case 5: return "";
    case 6: throw new Error("made");
    default: return "another";
  }
}
function scan(n) {
  var h = 1, caught = 0;
  for (var i = 0; i < n; i++) {
    try {
      var text = makeValue(i);
      h = (Math.imul(h, 33) + (text.length | 0) + (text.charCodeAt(i & 3) | 0)) | 0;
    } catch (e) {
      caught = (caught + (e.message === "made" ? 3 : 5)) | 0;
    }
  }
  return h + ":" + caught;
}
console.log(scan(12000));
"#;

// Keep the optimized region handler-free while changing the result family
// underneath an already-live string pin.  The first/hot-threshold iterations
// are flat ASCII, then later calls rotate through values whose `.length`
// requires the generic path.  Arrays are included specifically to ensure the
// call-result admission never treats their mutable backing store as a string.
const MIXED_WITH_LIVE_PIN: &str = r#"
"use strict";
function makeValue(i) {
  if (i < 2000) return "plainASCII";
  switch (i & 7) {
    case 0: return "plainASCII";
    case 1: return "café";
    case 2: return "left" + ((i & 16) ? "R" : "S");
    case 3: return 17;
    case 4: return Object("boxed");
    case 5: return [1, 2, 3, 4];
    case 6: return "";
    default: return { length: 9 };
  }
}
function scan(n) {
  var h = 1;
  for (var i = 0; i < n; i++) {
    var text = makeValue(i);
    h = (Math.imul(h, 33) + (text.length | 0)) | 0;
  }
  return h;
}
console.log(scan(12000));
"#;

const BRANCH_INTO_PREFIX: &str = r#"
"use strict";
function makeText(i) {
  return (i & 1) ? "abcdefgh" : "ABCDEFGH";
}
function scan(n) {
  var text = "initial!";
  var h = 1;
  for (var i = 0; i < n; i++) {
    if ((i & 3) !== 0) text = makeText(i);
    h = (Math.imul(h, 33) + text.length + text.charCodeAt(i & 7)) | 0;
  }
  return h;
}
console.log(scan(20000));
"#;

// NanoID's outer loop chooses between two string-producing calls and then
// converges on `.length` / `charCodeAt`.  The lexically-nearest Call does not
// dominate the lookup, but the pair forms an all-path Call reaching definition.
const TWO_CALL_CONVERGENCE: &str = r#"
"use strict";
function makeUpper(i) {
  return (i & 2) ? "ABCDEFGH" : "HGFEDCBA";
}
function makeLower(i) {
  return (i & 2) ? "abcdefgh" : "hgfedcba";
}
function scan(n) {
  var h = 1;
  for (var i = 0; i < n; i++) {
    const text = (i & 1) === 0 ? makeUpper(i) : makeLower(i);
    h = (Math.imul(h, 33) + text.length + text.charCodeAt(i & 7)) | 0;
  }
  return h;
}
console.log(scan(20000));
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

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH");
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

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

#[test]
fn call_result_str_pin_parity_stable_ascii() {
    assert_matches_node(STABLE);
}

#[test]
fn call_result_str_pin_parity_mixed_results_and_throws() {
    assert_matches_node(MIXED);
}

#[test]
fn call_result_str_pin_parity_mixed_results_with_live_pin() {
    assert_matches_node(MIXED_WITH_LIVE_PIN);
}

#[test]
fn call_result_str_pin_parity_branch_may_skip_writer() {
    assert_matches_node(BRANCH_INTO_PREFIX);
}

#[test]
fn call_result_str_pin_parity_two_call_convergence() {
    assert_matches_node(TWO_CALL_CONVERGENCE);
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn child_log(test: &str, envs: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args([test, "--exact", "--nocapture"])
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_CALL_RESULT_STR_PIN");
    for &(key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn test child");
    assert!(
        out.status.success(),
        "child {test} {envs:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn call_result_str_pin_mechanism_and_off_switch() {
    let on = child_log("call_result_str_pin_parity_stable_ascii", &[]);
    assert!(
        on.lines().any(|line| {
            line.contains("[pin] fn") && line.contains("built pins=1") && line.contains("access=[")
        }) && on.contains("[jit] MEM region"),
        "call-result string did not reach the pinned MEM lane:\n{on}"
    );
    assert!(
        !on.contains("decline: writer Call"),
        "enabled call-result pin was still declined:\n{on}"
    );

    let off = child_log(
        "call_result_str_pin_parity_stable_ascii",
        &[("ZIPP_NO_CALL_RESULT_STR_PIN", "1")],
    );
    assert!(
        off.contains("decline: writer Call")
            && off.contains("built pins=0")
            && off.contains("[jit] MEM region"),
        "off-switch did not restore the ordinary helper path:\n{off}"
    );

    let branched = child_log("call_result_str_pin_parity_branch_may_skip_writer", &[]);
    assert!(
        branched.contains("decline: call result not defined on every path"),
        "branch that may skip every Call was not rejected:\n{branched}"
    );

    let converged = child_log("call_result_str_pin_parity_two_call_convergence", &[]);
    assert!(
        converged.contains("call-result reaching-def all-paths")
            && converged.contains("built pins=1")
            && converged.contains("[jit] MEM region"),
        "two-Call convergence did not reach the pinned MEM lane:\n{converged}"
    );

    let mixed = child_log(
        "call_result_str_pin_parity_mixed_results_with_live_pin",
        &[],
    );
    assert!(
        mixed.contains("call-result reaching-def all-paths")
            && mixed.contains("built pins=1")
            && mixed.contains("[jit] MEM region"),
        "mixed result families never exercised a live call-result pin:\n{mixed}"
    );
}

#[test]
fn call_result_str_pin_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("off", "ZIPP_NO_CALL_RESULT_STR_PIN", "1"),
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("gc", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("call_result_str_pin_parity_")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "call-result parity filter matched nothing in {name} mode"
        );
    }
}
