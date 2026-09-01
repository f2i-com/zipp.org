//! W23 Rung A: identifier-`+=` right-pair fusion.
//!
//! `x += b + c`, when `b` is syntactically guaranteed to produce a primitive
//! String, may lower to one `AddRightPair` instruction.  Its bounded primitive
//! arm builds `x + (b + c)` in one allocation; every other shape executes the
//! original inner Add and outer Add in that order.  These tests pin evaluation
//! and coercion order against Node, exercise interpreter/MEM/Tier-C and GC
//! modes, prove the compiler switch restores the historical two-Add shape,
//! and ensure the new lowering does not steal a local accumulator's existing
//! `StrAppendInPlace` licence.

//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the interpreter-only profiles never emit; compiled only where that tier exists, like the other tier-pinning suites.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

use std::process::Command;

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
    let out = Command::new("node")
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
        .map(str::to_owned)
        .collect()
}

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

/// Primitive String/Int leaves, conditional and template left producers, a
/// captured binding (Cell), and the deliberately-unfused mutable-local path.
#[test]
fn pair_parity_primitive_and_binding_shapes() {
    assert_matches_node(
        r#""use strict";
var log = [];
var p = "root";
p += "/" + 7;
p += (true ? "//" : "/") + "seg";
function mark(v) { log.push("m" + v); return v; }
p += (mark(false) ? "/" : "//") + mark(9);
p += `${mark("[")}` + 5;
log.push(p);

function make() {
    let cell = "C";
    return function (n) { cell += "/" + n; return cell; };
}
var step = make(), cellLast = "";
for (var i = 0; i < 2500; i++) cellLast = step(i & 31);
log.push(cellLast.length + ":" + cellLast.slice(-5));

function local(n) {
    var s = "";
    for (var j = 0; j < n; j++) s += "/" + (j & 15);
    return s.length + ":" + s.slice(-4);
}
log.push(local(3000));
console.log(log.join("|"));
"#,
    );
}

const TIER_C_SRC: &str = r#""use strict";
var tierGlobal = "";
function appendPair(n) {
    tierGlobal = "x";
    tierGlobal += "/" + n;
    return tierGlobal;
}
var tierSum = 0, tierLast = "";
for (var ti = 0; ti < 6000; ti++) {
    tierLast = appendPair(ti & 1023);
    tierSum += tierLast.length;
}
console.log(tierSum, tierLast);
"#;

/// A whole-function Tier-C body containing AddRightPair, distinct from the
/// top-level MEM-region and interpreter coverage in the other cases.
#[test]
fn pair_parity_tier_c_global_body() {
    assert_matches_node(TIER_C_SRC);
}

#[test]
fn pair_jit_tier_c_census() {
    let exe = std::env::current_exe().expect("test exe path");
    let got = Command::new(&exe)
        .args(["pair_parity_tier_c_global_body", "--exact", "--nocapture"])
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDUMP", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .output()
        .expect("spawn Tier-C census child");
    let stderr = String::from_utf8_lossy(&got.stderr);
    assert!(
        got.status.success(),
        "Tier-C census child failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&got.stdout)
    );
    assert!(
        stderr.contains("Tier C fn") && stderr.contains("whole-function mem path"),
        "AddRightPair body did not compile on Tier C:\n{stderr}"
    );
    assert!(
        !stderr.contains("[tierC-reject] op AddRightPair")
            && !stderr.contains("[decline] AddRightPair")
            && !stderr.contains("op AddRightPair"),
        "a native tier rejected AddRightPair:\n{stderr}"
    );
}

/// The inner right operand is coerced before the outer left.  A getter may
/// replace the target global after GetValue, but PutValue still uses the old
/// captured left value.  Const assignment throws only after both Adds ran.
#[test]
fn pair_parity_getters_toprimitive_and_putvalue_order() {
    assert_matches_node(
        r#""use strict";
var log = [];
var outer = {
  [Symbol.toPrimitive]: function (hint) { log.push("A:" + hint); return "outer"; }
};
var inner = {
  [Symbol.toPrimitive]: function (hint) { log.push("C:" + hint); return "inner"; }
};
var x = outer;
x += "/" + inner;
log.push("x=" + x);

var y = "old";
var box = { get v() { log.push("get"); y = "changed"; return "leaf"; } };
y += "/" + box.v;
log.push("y=" + y);

const fixed = "F";
try { fixed += "/" + inner; } catch (e) { log.push("const:" + (e instanceof TypeError)); }
log.push("fixed=" + fixed);

var side = 0;
try { missing_pair_name += "/" + (side = 1); }
catch (e) { log.push("missing:" + (e instanceof ReferenceError) + ":" + side); }
console.log(log.join("|"));
"#,
    );
}

/// Symbol/BigInt and exotic primitive leaves force the exact fallback.  The
/// Symbol at the inner step must throw before the outer object is coerced; an
/// outer Symbol throws only after the inner object's hook has run.
#[test]
fn pair_parity_throw_bigint_and_exotic_fallbacks() {
    assert_matches_node(
        r#""use strict";
var out = [], order = [];
var a = { [Symbol.toPrimitive]: function () { order.push("A"); return "a"; } };
try { a += "/" + Symbol("inner"); }
catch (e) { out.push("innerSym:" + (e instanceof TypeError) + ":" + order.join("")); }

order = [];
var c = { [Symbol.toPrimitive]: function () { order.push("C"); return "c"; } };
var s = Symbol("outer");
try { s += "/" + c; }
catch (e) { out.push("outerSym:" + (e instanceof TypeError) + ":" + order.join("")); }

var q = "q";
q += "/" + 123456789012345678901234567890n;
q += "/" + 1.25;
q += "/" + true;
q += "/" + null;
q += "/" + undefined;
out.push(q);

var mix = 1n;
try { mix += "/" + 2; } catch (e) { out.push("mix:" + (e instanceof TypeError)); }
console.log(out.join("|"));
"#,
    );
}

/// Non-ASCII/WTF-8 and long/rope operands decline the ASCII shortcut.  Assert
/// code units and slices rather than raw lone-surrogate terminal output.
#[test]
fn pair_parity_nonascii_rope_and_alias_safety() {
    assert_matches_node(
        r#""use strict";
var hi = "\uD83D", lo = "\uDE00";
var s = "é";
s += "/" + "☃";
s += hi + lo;
var units = [];
for (var i = 0; i < s.length; i++) units.push(s.charCodeAt(i));

var long = "L".repeat(400);
long += "/" + "tail";

var acc = "seed", saved = acc;
for (var j = 0; j < 2400; j++) acc += "/" + (j & 31);
console.log(units.join(","));
console.log(long.length, long.slice(-8));
console.log(saved, acc.length, acc.slice(-7));
"#,
    );
}

const BYTECODE_SRC: &str = r#"var path = ""; path += "/" + 7; console.log(path);"#;

#[test]
fn pair_bytecode_child() {
    if std::env::var_os("ZIPP_PAIR_BC_CHILD").is_none() {
        return;
    }
    let bc = zipp_vm::compile_to_text(BYTECODE_SRC, false).expect("compile bytecode");
    if std::env::var_os("ZIPP_NO_CONCAT_PAIR_FUSE").is_some() {
        assert!(
            !bc.contains("AddRightPair"),
            "off switch left fused bytecode:\n{bc}"
        );
        assert_eq!(
            bc.matches("Add {").count(),
            2,
            "off switch did not restore the exact inner+outer Add pair:\n{bc}"
        );
    } else {
        assert_eq!(
            bc.matches("AddRightPair {").count(),
            1,
            "fused opcode absent/duplicated:\n{bc}"
        );
        assert_eq!(
            bc.matches("Add {").count(),
            0,
            "fused lowering retained an intermediate Add:\n{bc}"
        );
    }
}

/// Compiler switches latch process-wide, so inspect ON and OFF in isolated
/// children.  Also pin the non-overlap rule: a mutable function-local loop
/// keeps the pre-existing proven-linear StrAppendInPlace lowering.
#[test]
fn pair_bytecode_switch_and_local_append_license() {
    let local = zipp_vm::compile_to_text(
        r#"function f(n) { var s = ""; for (var i = 0; i < n; i++) s += "/" + i; return s; } console.log(f(20));"#,
        false,
    )
    .expect("compile local accumulator");
    assert!(
        !local.contains("AddRightPair"),
        "mutable local was pair-fused:\n{local}"
    );
    assert!(
        local.contains("StrAppendInPlace"),
        "pair lowering displaced the local append licence:\n{local}"
    );

    let exe = std::env::current_exe().expect("test exe path");
    for off in [false, true] {
        let mut cmd = Command::new(&exe);
        cmd.args(["pair_bytecode_child", "--exact", "--nocapture"])
            .env("ZIPP_PAIR_BC_CHILD", "1")
            .env_remove("ZIPP_NO_CONCAT_PAIR_FUSE");
        if off {
            cmd.env("ZIPP_NO_CONCAT_PAIR_FUSE", "1");
        }
        let got = cmd.output().expect("spawn bytecode child");
        let stdout = String::from_utf8_lossy(&got.stdout);
        assert!(
            got.status.success() && !stdout.contains("running 0 tests"),
            "bytecode child off={off} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&got.stderr)
        );
    }
}

const MECHANISM_SRC: &str = r#""use strict";
var p = "";
for (var i = 0; i < 1800; i++) p += "/" + i;

var q = "", qSum = 0;
for (var j = 0; j < 1600; j++) {
  q = "q";
  q += "/" + j;
  qSum += q.length;
}

var r = "", rSum = 0;
for (var k = 0; k < 1500; k++) {
  r = "r";
  r += (k & 1 ? "/" : "//") + "seg";
  rSum += r.length;
}

var calls = 0;
var obj = { toString: function () { calls++; return "x"; } };
var f = "", fSaved = f;
for (var m = 0; m < 1200; m++) f += "/" + obj;
console.log(p.length, qSum, rSum, f.length, calls, fSaved);
"#;

#[test]
fn pair_mechanism_child() {
    if std::env::var_os("ZIPP_PAIR_MECH_CHILD").is_none() {
        return;
    }
    assert_matches_node(MECHANISM_SRC);
    let (fast_str, fast_int, in_place, fallback) = zipp_vm::concat_pair_stats();
    eprintln!("[pair-test] str={fast_str} int={fast_int} in_place={in_place} fallback={fallback}");
    if std::env::var_os("ZIPP_NO_CONCAT_PAIR_FUSE").is_some() {
        assert_eq!((fast_str, fast_int, in_place, fallback), (0, 0, 0, 0));
    } else {
        assert!(
            fast_str > 1_000,
            "string one-allocation arm did not engage: {fast_str}/{fast_int}/{in_place}/{fallback}"
        );
        assert!(
            fast_int > 1_000,
            "int one-allocation arm did not engage: {fast_str}/{fast_int}/{in_place}/{fallback}"
        );
        assert!(in_place > 1_000, "proven-linear in-place arm did not engage: {fast_str}/{fast_int}/{in_place}/{fallback}");
        assert!(
            fallback > 500,
            "pairwise fallback did not engage: {fast_str}/{fast_int}/{in_place}/{fallback}"
        );
    }
}

#[test]
fn pair_counter_and_switch() {
    if std::env::var_os("ZIPP_PAIR_MECH_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    for off in [false, true] {
        let mut cmd = Command::new(&exe);
        cmd.args(["pair_mechanism_child", "--exact", "--nocapture"])
            .env("ZIPP_PAIR_MECH_CHILD", "1")
            .env("ZIPP_ICSTATS", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_CONCAT_PAIR_FUSE");
        if off {
            cmd.env("ZIPP_NO_CONCAT_PAIR_FUSE", "1");
        }
        let got = cmd.output().expect("spawn mechanism child");
        let stdout = String::from_utf8_lossy(&got.stdout);
        assert!(
            got.status.success() && !stdout.contains("running 0 tests"),
            "mechanism child off={off} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&got.stderr)
        );
    }
}

/// Re-run every Node differential under the compiler rollback, interpreter,
/// immediate JIT, GC-at-every-safepoint, and old full-GC modes.
#[test]
fn zz_pair_modes_agree() {
    if std::env::var_os("ZIPP_PAIR_MODE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_CONCAT_PAIR_FUSE", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NO_NURSERY", "1"),
    ] {
        let got = Command::new(&exe)
            .args(["pair_parity_", "--test-threads=1"])
            .env("ZIPP_PAIR_MODE_CHILD", "1")
            .env(key, val)
            .output()
            .expect("spawn pair mode child");
        let stdout = String::from_utf8_lossy(&got.stdout);
        assert!(
            got.status.success() && !stdout.contains("running 0 tests"),
            "mode {key}={val} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&got.stderr)
        );
    }
}
