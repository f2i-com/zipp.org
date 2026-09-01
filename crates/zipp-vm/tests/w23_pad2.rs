//! W23 Rung B: literal-prefix Pad2Concat and the immutable `"00".."99"` table.
//!
//! The compiler lowers only `"0" + x` and `"" + x`. Tagged integers in the
//! branch-compatible pad2 ranges return a pinned primitive string; every other
//! value takes the exact ordinary `+` fallback. These tests pin the switch,
//! exhaustive range semantics, coercion/throw order, equal-content String
//! semantics, native-tier admission, counters, and GC/JIT modes against Node.

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

const PAD2_FN: &str = r#"
function pad2(n) { return n < 10 ? "0" + n : "" + n; }
"#;

#[test]
fn pad2_parity_exhaustive_integer_window() {
    let src = format!(
        r#""use strict";
{PAD2_FN}
var out = [], hash = 2166136261;
for (var n = -256; n <= 512; n++) {{
  var s = pad2(n);
  out.push(s);
  for (var j = 0; j < s.length; j++) hash = Math.imul(hash ^ s.charCodeAt(j), 16777619);
}}
console.log(out.join(","));
console.log(hash >>> 0, pad2(0), pad2(9), pad2(10), pad2(99), pad2(100));
"#,
    );
    assert_matches_node(&src);
}

/// Non-Int operands and out-of-branch integers must run the ordinary `+`,
/// including BigInt formatting, Symbol errors and one-shot object coercions.
#[test]
fn pad2_parity_fallback_values_and_coercion_order() {
    let src = format!(
        r#""use strict";
{PAD2_FN}
var vals = [-0, 0.25, 9.5, 10.5, NaN, Infinity, -Infinity,
            true, false, null, undefined, "7", "12", 1n, 10n, 99n, 100n];
var out = [];
for (var i = 0; i < vals.length; i++) out.push(i + ":" + pad2(vals[i]));

var order = [], calls = 0;
var obj = {{
  [Symbol.toPrimitive]: function (hint) {{
    order.push(hint + (++calls));
    return calls === 1 ? 5 : 7;
  }}
}};
out.push("obj=" + pad2(obj) + ":" + order.join(","));

var direct = {{
  [Symbol.toPrimitive]: function (hint) {{ order.push("D:" + hint); return 8; }}
}};
out.push("direct=" + ("0" + direct));
try {{ pad2(Symbol("conditional")); }} catch (e) {{ out.push("symc=" + (e instanceof TypeError)); }}
try {{ "" + Symbol("plain"); }} catch (e) {{ out.push("sym0=" + (e instanceof TypeError)); }}
try {{ "0" + Symbol("zero"); }} catch (e) {{ out.push("sym1=" + (e instanceof TypeError)); }}

var throws = 0;
function once() {{
  var x = {{ [Symbol.toPrimitive]: function () {{ throws++; throw new Error("x"); }} }};
  try {{ return "" + x; }} catch (e) {{ return e.message; }}
}}
var last = "";
for (var k = 0; k < 3500; k++) last = once();
out.push("throws=" + throws + ":" + last);
console.log(out.join("|"));
"#,
    );
    assert_matches_node(&src);
}

/// The first (relational, number-hint) conversion may mutate the object's own
/// coercion method. The selected Add must then convert the ORIGINAL object a
/// second time with the default hint and observe that mutation.
#[test]
fn pad2_parity_conditional_object_self_mutation() {
    let src = format!(
        r#""use strict";
{PAD2_FN}
var log = [];
var obj = {{
  [Symbol.toPrimitive]: function (hint) {{
    log.push("first:" + hint);
    Object.defineProperty(this, Symbol.toPrimitive, {{
      value: function (nextHint) {{ log.push("next:" + nextHint); return 8; }},
      configurable: true
    }});
    return 5;
  }}
}};
console.log(pad2(obj), log.join(","));
"#,
    );
    assert_matches_node(&src);
}

/// A sloppy simple parameter can be changed through its mapped arguments
/// object while `<` coerces the first binding value. Captured and direct-eval
/// locals have the same stability hazard through their cells. All three shapes
/// must retain the ordinary two-read conditional lowering.
#[test]
fn pad2_parity_unstable_bindings_decline() {
    let src = r#"
var order = [];
function sloppy(n) {
  var args = arguments;
  n = { [Symbol.toPrimitive]: function (hint) {
    order.push("sloppy:" + hint); args[0] = 7; return 5;
  }};
  return n < 10 ? "0" + n : "" + n;
}
function captured(n) {
  "use strict";
  function set(v) { n = v; }
  n = { [Symbol.toPrimitive]: function (hint) {
    order.push("captured:" + hint); set(8); return 5;
  }};
  return n < 10 ? "0" + n : "" + n;
}
function directEval(x) {
  "use strict";
  var n = x;
  eval("");
  return n < 10 ? "0" + n : "" + n;
}
console.log(sloppy(1), captured(1), directEval(9), order.join(","));
"#;
    assert_matches_node(src);

    let bc = zipp_vm::compile_to_text(src, false).expect("compile unstable-binding bytecode");
    assert!(
        !bc.contains("Pad2Conditional"),
        "unstable binding admitted whole conditional fusion:\n{bc}"
    );
}

#[test]
fn pad2_conditional_compiler_eligibility_matrix() {
    let stable = zipp_vm::compile_to_text(
        r#""use strict";
function param(n) { return n < 10 ? "0" + n : "" + n; }
function local(x) { let n = x; return n < 10 ? "0" + n : "" + n; }
"#,
        false,
    )
    .expect("compile stable pad2 bindings");
    assert_eq!(
        stable.matches("Pad2Conditional {").count(),
        2,
        "strict parameter/local did not both engage:\n{stable}"
    );

    let mismatched = zipp_vm::compile_to_text(
        r#""use strict";
function a(n, m) { return n < 10 ? "0" + m : "" + n; }
function b(n) { return n <= 10 ? "0" + n : "" + n; }
function c(n) { return n < 11 ? "0" + n : "" + n; }
function d(n) { return n < 10 ? n + "0" : "" + n; }
"#,
        false,
    )
    .expect("compile non-pad2 shapes");
    assert!(
        !mismatched.contains("Pad2Conditional"),
        "near-miss shape admitted whole conditional fusion:\n{mismatched}"
    );
}

/// A local declared outside an active `with` is dynamically shadowable. The
/// recogniser must decline without materialising the with-object itself: the
/// three CellGets below are exactly the original condition/arm identifier
/// reads, with no fourth dead CellGet from an eligibility probe.
#[test]
fn pad2_conditional_with_shadow_declines_without_dead_materialization() {
    let src = r#"
function p(obj, x) {
  let n = x;
  with (obj) { return n < 10 ? "0" + n : "" + n; }
}
console.log(p({}, 7), p({ n: 12 }, 7));
"#;
    assert_matches_node(src);
    let bc = zipp_vm::compile_to_text(src, false).expect("compile with-shadow pad2");
    assert!(
        !bc.contains("Pad2Conditional"),
        "with-shadowed local admitted whole conditional fusion:\n{bc}"
    );
    assert_eq!(
        bc.matches("CellGet {").count(),
        3,
        "eligibility probe added dead with-object materialisation:\n{bc}"
    );
}

/// Cached primitives have no observable identity. In particular a dynamically
/// allocated equal-content string must compare strictly equal, while growing a
/// value derived from a cached slot must never mutate the saved primitive.
#[test]
fn pad2_parity_strict_eq_alias_and_string_exotic_semantics() {
    let src = format!(
        r#""use strict";
{PAD2_FN}
var pfx0 = "0", pfxe = "";
var yes = 0, no = 0;
for (var i = 0; i < 7000; i++) {{
  var n = i % 100;
  var cached = pad2(n);
  var dynamic = n < 10 ? pfx0 + n : pfxe + n;
  if (cached === dynamic) yes++;
  if (cached === pad2((n + 1) % 100)) no++;
}}

var s = pad2(7), saved = s;
s += "x";
var chain = saved + "a" + "b" + "c";
var w1 = Object(saved), w2 = Object(saved);
w1.extra = 4;
var boxedWrite;
try {{ w1[0] = "X"; boxedWrite = w1[0]; }} catch (e) {{ boxedWrite = e.name; }}
console.log(yes, no, saved, s, chain, w1 === w2, w1.extra, boxedWrite);
"#,
    );
    assert_matches_node(&src);
}

const TIER_C_SRC: &str = r#""use strict";
function pad2(n) { return n < 10 ? "0" + n : "" + n; }
var h = 0, last = "";
for (var i = 0; i < 8000; i++) {
  last = pad2(i % 100);
  h = Math.imul(h ^ last.charCodeAt(0), 16777619);
}
console.log(h, last);
"#;

const MEM_REGION_SRC: &str = r#""use strict";
var h = 0, last = "";
for (var i = 0; i < 12000; i++) {
  let n = i % 100;
  last = n < 10 ? "0" + n : "" + n;
  h = Math.imul(h ^ last.charCodeAt(1), 16777619);
}
console.log(h, last);
"#;

/// Top-level local shape: the hot loop is served by an OSR MEM region rather
/// than the whole-function Tier-C path exercised below.
#[test]
fn pad2_parity_mem_region_body() {
    let bc = zipp_vm::compile_to_text(MEM_REGION_SRC, false).expect("compile MEM pad2 source");
    let expected_conditional = usize::from(
        std::env::var_os("ZIPP_NO_PAD2_CACHE").is_none()
            && std::env::var_os("ZIPP_NO_PAD2_COND_FUSE").is_none(),
    );
    assert_eq!(
        bc.matches("Pad2Conditional {").count(),
        expected_conditional,
        "top-level local shape switch mismatch:\n{bc}"
    );
    assert_matches_node(MEM_REGION_SRC);
}

#[test]
fn pad2_jit_mem_region_census() {
    let exe = std::env::current_exe().expect("test exe path");
    let got = Command::new(&exe)
        .args(["pad2_parity_mem_region_body", "--exact", "--nocapture"])
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDUMP", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .output()
        .expect("spawn MEM-region census child");
    let stderr = String::from_utf8_lossy(&got.stderr);
    assert!(
        got.status.success(),
        "MEM-region child failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&got.stdout)
    );
    assert!(
        stderr.contains("MEM region"),
        "Pad2Conditional loop compiled no MEM region:\n{stderr}"
    );
    assert!(
        !stderr.contains("[decline] Pad2Conditional")
            && !stderr.contains("[tierC-reject] op Pad2Conditional"),
        "a native tier rejected Pad2Conditional:\n{stderr}"
    );
}

#[test]
fn pad2_parity_tier_c_body() {
    assert_matches_node(TIER_C_SRC);
}

#[test]
fn pad2_jit_tier_c_census() {
    let exe = std::env::current_exe().expect("test exe path");
    let got = Command::new(&exe)
        .args(["pad2_parity_tier_c_body", "--exact", "--nocapture"])
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDUMP", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .output()
        .expect("spawn Tier-C census child");
    let stderr = String::from_utf8_lossy(&got.stderr);
    assert!(
        got.status.success(),
        "Tier-C child failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&got.stdout)
    );
    assert!(
        stderr.contains("Tier C fn") && stderr.contains("whole-function mem path"),
        "Pad2Concat body did not compile on Tier C:\n{stderr}"
    );
    assert!(
        !stderr.contains("[tierC-reject] op Pad2Conditional")
            && !stderr.contains("[decline] Pad2Conditional")
            && !stderr.contains("op Pad2Conditional"),
        "a native tier rejected Pad2Conditional:\n{stderr}"
    );
}

const BYTECODE_SRC: &str = r#""use strict"; function p(n) { return n < 10 ? "0" + n : "" + n; }"#;

#[test]
fn pad2_bytecode_child() {
    if std::env::var_os("ZIPP_PAD2_BC_CHILD").is_none() {
        return;
    }
    let bc = zipp_vm::compile_to_text(BYTECODE_SRC, false).expect("compile bytecode");
    let op_count = |opcode: &str| {
        bc.lines()
            .filter(|line| line.trim_start().starts_with(opcode))
            .count()
    };
    if std::env::var_os("ZIPP_NO_PAD2_CACHE").is_some() {
        assert!(
            !bc.contains("Pad2Concat") && !bc.contains("Pad2Conditional"),
            "off switch left fused bytecode:\n{bc}"
        );
        assert_eq!(
            op_count("Add {"),
            2,
            "off switch did not restore two Adds:\n{bc}"
        );
        assert_eq!(
            op_count("JumpIfNotLt {"),
            1,
            "missing direct conditional branch:\n{bc}"
        );
        assert_eq!(
            op_count("Lt {") + op_count("JumpIfFalse {"),
            0,
            "cache switch unexpectedly disabled the independent branch fusion:\n{bc}"
        );
    } else if std::env::var_os("ZIPP_NO_PAD2_COND_FUSE").is_some() {
        assert!(
            !bc.contains("Pad2Conditional"),
            "conditional switch left whole fusion:\n{bc}"
        );
        assert_eq!(
            op_count("Pad2Concat {"),
            2,
            "conditional switch did not restore both literal arms:\n{bc}"
        );
        assert_eq!(
            op_count("JumpIfNotLt {"),
            1,
            "missing direct conditional branch:\n{bc}"
        );
        assert_eq!(op_count("Lt {") + op_count("JumpIfFalse {"), 0);
        assert_eq!(
            op_count("Add {"),
            0,
            "conditional switch lost Pad2Concat leaf lowering:\n{bc}"
        );
    } else {
        assert_eq!(
            op_count("Pad2Conditional {"),
            1,
            "whole Pad2Conditional absent/duplicated:\n{bc}"
        );
        assert!(
            !bc.contains("Pad2Concat")
                && !bc.contains("Lt {")
                && !bc.contains("JumpIfFalse {")
                && !bc.contains("Add {"),
            "whole lowering retained source operations:\n{bc}"
        );
    }
}

#[test]
fn pad2_bytecode_switch_identity() {
    let exe = std::env::current_exe().expect("test exe path");
    for (cache_off, cond_off) in [(false, false), (false, true), (true, false)] {
        let mut cmd = Command::new(&exe);
        cmd.args(["pad2_bytecode_child", "--exact", "--nocapture"])
            .env("ZIPP_PAD2_BC_CHILD", "1")
            .env_remove("ZIPP_NO_PAD2_CACHE")
            .env_remove("ZIPP_NO_PAD2_COND_FUSE");
        if cache_off {
            cmd.env("ZIPP_NO_PAD2_CACHE", "1");
        }
        if cond_off {
            cmd.env("ZIPP_NO_PAD2_COND_FUSE", "1");
        }
        let got = cmd.output().expect("spawn bytecode child");
        let stdout = String::from_utf8_lossy(&got.stdout);
        assert!(
            got.status.success() && !stdout.contains("running 0 tests"),
            "bytecode child cache_off={cache_off} cond_off={cond_off} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&got.stderr)
        );
    }
}

const MECHANISM_SRC: &str = r#""use strict";
function pad2(n) { return n < 10 ? "0" + n : "" + n; }
var hash = 0;
for (var round = 0; round < 100; round++) {
  for (var i = 0; i < 100; i++) hash = Math.imul(hash ^ pad2(i).charCodeAt(1), 33);
}
for (var j = 0; j < 1600; j++) {
  hash ^= pad2(-1).length;
  hash ^= pad2(100).length;
  hash ^= ("0" + 1.5).length;
}
console.log(hash);
"#;

#[test]
fn pad2_mechanism_child() {
    if std::env::var_os("ZIPP_PAD2_MECH_CHILD").is_none() {
        return;
    }
    assert_matches_node(MECHANISM_SRC);
    let (zero, plain, fallback) = zipp_vm::pad2_concat_stats();
    let (cond_hit, cond_slow) = zipp_vm::pad2_conditional_stats();
    eprintln!(
        "[pad2-test] zero={zero} plain={plain} fallback={fallback} cond_hit={cond_hit} cond_slow={cond_slow}"
    );
    if std::env::var_os("ZIPP_NO_PAD2_CACHE").is_some() {
        assert_eq!((zero, plain, fallback), (0, 0, 0));
        assert_eq!((cond_hit, cond_slow), (0, 0));
    } else {
        assert!(
            zero >= 1_000,
            "zero cache arm did not engage: {zero}/{plain}/{fallback}"
        );
        assert!(
            plain >= 9_000,
            "plain cache arm did not engage: {zero}/{plain}/{fallback}"
        );
        assert!(
            fallback >= 4_000,
            "ordinary Add fallback did not engage: {zero}/{plain}/{fallback}"
        );
        if std::env::var_os("ZIPP_NO_PAD2_COND_FUSE").is_some() {
            assert_eq!((cond_hit, cond_slow), (0, 0));
        } else {
            assert_eq!((cond_hit, cond_slow), (10_000, 3_200));
        }
    }
}

#[test]
fn pad2_counter_and_switch() {
    if std::env::var_os("ZIPP_PAD2_MECH_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    for (cache_off, cond_off) in [(false, false), (false, true), (true, false)] {
        let mut cmd = Command::new(&exe);
        cmd.args(["pad2_mechanism_child", "--exact", "--nocapture"])
            .env("ZIPP_PAD2_MECH_CHILD", "1")
            .env("ZIPP_ICSTATS", "1")
            .env_remove("ZIPP_NO_PAD2_CACHE")
            .env_remove("ZIPP_NO_PAD2_COND_FUSE");
        if cache_off {
            cmd.env("ZIPP_NO_PAD2_CACHE", "1");
        }
        if cond_off {
            cmd.env("ZIPP_NO_PAD2_COND_FUSE", "1");
        }
        let got = cmd.output().expect("spawn mechanism child");
        let stdout = String::from_utf8_lossy(&got.stdout);
        assert!(
            got.status.success() && !stdout.contains("running 0 tests"),
            "mechanism child cache_off={cache_off} cond_off={cond_off} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&got.stderr)
        );
    }
}

/// Re-run every Node differential under rollback, interpreter, immediate JIT,
/// GC-at-every-safepoint, and the old full-collection mode.
#[test]
fn zz_pad2_modes_agree() {
    if std::env::var_os("ZIPP_PAD2_MODE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_PAD2_CACHE", "1"),
        ("ZIPP_NO_PAD2_COND_FUSE", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NO_NURSERY", "1"),
    ] {
        let got = Command::new(&exe)
            .args(["pad2_parity_", "--test-threads=1"])
            .env("ZIPP_PAD2_MODE_CHILD", "1")
            .env(key, val)
            .output()
            .expect("spawn pad2 mode child");
        let stdout = String::from_utf8_lossy(&got.stdout);
        assert!(
            got.status.success() && !stdout.contains("running 0 tests"),
            "mode {key}={val} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&got.stderr)
        );
    }
}
