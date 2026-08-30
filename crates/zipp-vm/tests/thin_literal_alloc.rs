//! B257 thin literal allocation: node-oracled literal and function-creation
//! probes across every tier mode and the thin-alloc latches.
//!
//! The mechanism under test: the baked `FinalizeObject` helper copies its
//! values window -> slab directly (no staging buffer) and validates only the
//! range it reads; finalized objects, arrays, functions and closures are
//! installed into recycled slots by a known-variant write with their mirrors
//! settled from the values in hand; Tier-C `MakeFunc` sites take a
//! poll-and-allocate "plain" helper while the realm and eval-scope side
//! tables are empty.
//!
//! Every expected line is node's (v24.12.0), byte for byte. The driver test
//! re-runs the in-process probes in child processes under each mode so every
//! tier — and the latched-off general allocation paths — answers identically, and a
//! second driver reads the `ZIPP_GCSTATS=1` `[thinalloc]` telemetry to prove
//! the thin paths actually served (a mechanism that never engages is not a
//! passing test).

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

/// Literals of every slab class (1, 4, 8, 16 keys), a 17-key literal past
/// the baked helper's cap, an array literal, a method-carrying literal
/// (capture-free `MakeFunc`), a capturing arrow, a nested literal whose
/// values are themselves fresh literals, plus a retained sample so minors
/// and majors both run over the churn. `__N__` is the iteration count.
const LITERALS: &str = r#"
"use strict";
function mk1(i) { return { a: i }; }
function mk4(i) { return { a: i, b: i + 1, c: "s" + i, d: [i] }; }
function mk8(i) { return { a: i, b: i, c: i, d: i, e: i, f: i, g: i, h: i + 1 }; }
function mk16(i) { return { a: i, b: 1, c: 2, d: 3, e: 4, f: 5, g: 6, h: 7, i: 8, j: 9, k: 10, l: 11, m: 12, n: 13, o: 14, p: i }; }
function mk17(i) { return { a: i, b: 1, c: 2, d: 3, e: 4, f: 5, g: 6, h: 7, i: 8, j: 9, k: 10, l: 11, m: 12, n: 13, o: 14, p: 15, q: i }; }
function arr(i) { return [i, i + 1, "x"]; }
function fn(i) { return { v: i, get: function () { return this.v; } }; }
function arrow(i) { let k = i; return () => k + 1; }
function nested(i) { return { p: { q: [i, { r: i }] }, s: "t" + i }; }
var N = __N__;
var acc = 0;
var keep = [];
for (var i = 0; i < N; i++) {
  var o1 = mk1(i), o4 = mk4(i), o8 = mk8(i), o16 = mk16(i), o17 = mk17(i);
  var a = arr(i), f = fn(i), g = arrow(i), n = nested(i);
  acc = (acc + o1.a + o4.b + o4.d[0] + o8.h + o16.p + o17.q + a[1] + f.get() + g() + n.p.q[1].r) | 0;
  if (i % 997 === 0) keep.push(o4, a, f, g, n);
}
console.log(acc, keep.length, Object.keys(keep[5]).join(""), JSON.stringify(keep[6]), keep[7].get(), keep[8](), JSON.stringify(keep[9]), keep[0].c, keep[4].s);
"#;

const LITERALS_EXPECTED_20000: &str =
    r#"1999980000 105 abcd [997,998,"x"] 997 998 {"p":{"q":[997,{"r":997}]},"s":"t997"} s0 t0"#;
const LITERALS_EXPECTED_1500: &str =
    r#"11248500 10 abcd [997,998,"x"] 997 998 {"p":{"q":[997,{"r":997}]},"s":"t997"} s0 t0"#;

/// Literals born in eval-table protos (indirect eval, `new Function`) keep
/// the general (non-root-plan) helper or the interpreter; they must agree
/// with the root-plan `direct` literal.
const EVAL_LITERALS: &str = r#"
"use strict";
function direct(i) { return { a: i, b: i * 2 }; }
var indirectMk = (0, eval)("(function (i) { return { p: i, q: i + 1 }; })");
var fnMk = new Function("i", "return { x: i, y: [i, i] };");
var acc = 0;
for (var i = 0; i < 5000; i++) {
  var d = direct(i), e = indirectMk(i), f = fnMk(i);
  acc = (acc + d.b + e.q + f.y[1]) | 0;
}
console.log(acc, JSON.stringify(indirectMk(7)), JSON.stringify(fnMk(8)), Object.keys(direct(1)).join(""));
"#;
const EVAL_LITERALS_EXPECTED: &str = r#"49995000 {"p":7,"q":8} {"x":8,"y":[8,8]} ab"#;

/// `mk` is created under `outer`'s sloppy direct-eval scope, runs hot enough
/// to compile, and every `inner` it creates must inherit that EvalScope so
/// `hidden` resolves. Taking the plain `MakeFunc` lane here would mint an
/// unstamped `inner` and this line would be a ReferenceError, not 12 — the
/// lane's eval-scope guard byte is what routes it to the full helper.
const EVAL_SCOPE: &str = r#"
function outer() {
  eval("var hidden = 5");
  function mk() { return function inner() { return hidden + 1; }; }
  var fs = [];
  for (var i = 0; i < 3000; i++) fs.push(mk());
  return fs[2999]() + fs[0]();
}
console.log(outer());
"#;
const EVAL_SCOPE_EXPECTED: &str = "12";

/// The plain lane engages while no child realm exists, then a realm is
/// created mid-run (the realm guard byte flips under already-compiled code)
/// and the same site must keep producing main-realm functions through the
/// full helper; a child-realm literal (object + array + method, born through
/// the same heap paths from the child's own code) must carry the child's
/// prototypes. Node oracle: the `vm` module analogue of `$262.createRealm`.
/// (Functions minted by child-realm `eval` are deliberately not asserted:
/// zipp gives them the main `Function.prototype` on the B256 base and every
/// mode alike — a pre-existing divergence outside this change.)
const REALM: &str = r#"
function mk() { return function inner() { return 1; }; }
var fs = [];
for (var i = 0; i < 3000; i++) fs.push(mk());
var realm = $262.createRealm();
var other = realm.global;
for (var j = 0; j < 3000; j++) fs.push(mk());
var f = fs[5999];
console.log(f(), Object.getPrototypeOf(f) === Function.prototype, Object.getPrototypeOf(f) === other.Function.prototype, f instanceof Function, f instanceof other.Function);
var o = realm.evalScript("({ a: 1, b: [2], c: function () { return 3; } })");
console.log(o.a, o.b[0], o.c(), Object.getPrototypeOf(o) === other.Object.prototype, Object.getPrototypeOf(o) === Object.prototype, Object.getPrototypeOf(o.b) === other.Array.prototype, o.b instanceof Array);
"#;
const REALM_EXPECTED: [&str; 2] = ["1 true false true false", "1 2 3 true false true false"];

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("probe compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}; output: {:?}",
        out.error,
        out.output
    );
    out.output
}

#[test]
fn tl_lit_in_process() {
    let src = LITERALS.replace("__N__", "20000");
    assert_eq!(run_ok(&src), vec![LITERALS_EXPECTED_20000.to_string()]);
}

#[test]
fn tl_small_lit_in_process() {
    let src = LITERALS.replace("__N__", "1500");
    assert_eq!(run_ok(&src), vec![LITERALS_EXPECTED_1500.to_string()]);
}

#[test]
fn tl_small_eval_in_process() {
    assert_eq!(
        run_ok(EVAL_LITERALS),
        vec![EVAL_LITERALS_EXPECTED.to_string()]
    );
}

#[test]
fn tl_small_evalscope_in_process() {
    assert_eq!(run_ok(EVAL_SCOPE), vec![EVAL_SCOPE_EXPECTED.to_string()]);
}

#[test]
fn tl_small_realm_in_process() {
    assert_eq!(run_ok(REALM), REALM_EXPECTED);
}

fn child(filter: &str, envs: &[(&str, &str)], nocapture: bool) -> std::process::Output {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(filter);
    if nocapture {
        cmd.arg("--nocapture");
    }
    for (key, val) in envs {
        cmd.env(key, val);
    }
    cmd.output().expect("spawn the test binary")
}

fn assert_child_ok(out: &std::process::Output, what: &str) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{what} failed:\n{stdout}\n{stderr}");
    assert!(
        !stdout.contains("running 0 tests"),
        "the filter matched nothing under {what}:\n{stdout}"
    );
}

/// Every in-process probe, re-run in a child process under each tier mode
/// and each latch (the process-global latches make in-process env switching
/// unreliable — the concat_chain precedent). GC stress runs the small probes
/// only; the 20000-iteration literal loop would collect ~200k times.
#[test]
fn thin_literal_probes_agree_across_modes() {
    for (key, val) in [
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_NO_NURSERY", "1"),
        ("ZIPP_NO_THIN_ALLOC", "1"),
        ("ZIPP_NO_MAKEFUNC_PLAIN", "1"),
        ("ZIPP_NURSERY_VERIFY", "1"),
    ] {
        let out = child("_in_process", &[(key, val)], false);
        assert_child_ok(&out, &format!("{key}={val}"));
    }
    let out = child("tl_small_", &[("ZIPP_GC_STRESS", "1")], false);
    assert_child_ok(&out, "ZIPP_GC_STRESS=1");
    let out = child(
        "tl_small_",
        &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NO_THIN_ALLOC", "1")],
        false,
    );
    assert_child_ok(&out, "ZIPP_GC_STRESS=1 ZIPP_NO_THIN_ALLOC=1");
}

/// The telemetry gate: under the default configuration the literal probe's
/// heap must report every thin path serving — finalized literals, object
/// slot reuses, arrays, functions, closures and the plain `MakeFunc`
/// helper — and under `ZIPP_NO_THIN_ALLOC=1` it must report the fold off.
#[test]
fn thin_paths_serve_under_the_default_configuration() {
    fn counts(stderr: &str, on: bool) -> Vec<(String, u64)> {
        let line = stderr
            .lines()
            .filter(|l| l.starts_with(&format!("[thinalloc] on={on} ")))
            .last()
            .unwrap_or_else(|| panic!("no [thinalloc] on={on} line in:\n{stderr}"));
        line.split_whitespace()
            .filter_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                Some((k.to_string(), v.parse::<u64>().ok()?))
            })
            .collect()
    }
    let out = child("tl_lit_in_process", &[("ZIPP_GCSTATS", "1")], true);
    assert_child_ok(&out, "ZIPP_GCSTATS=1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let served = counts(&stderr, true);
    for key in [
        "finalized",
        "obj_reuse",
        "array",
        "func",
        "closure",
        "makefunc_plain",
    ] {
        let n = served
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("missing {key} in [thinalloc] line:\n{stderr}"));
        assert!(n > 0, "thin path `{key}` never served:\n{stderr}");
    }

    let out = child(
        "tl_lit_in_process",
        &[("ZIPP_GCSTATS", "1"), ("ZIPP_NO_THIN_ALLOC", "1")],
        true,
    );
    assert_child_ok(&out, "ZIPP_GCSTATS=1 ZIPP_NO_THIN_ALLOC=1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    for (key, n) in counts(&stderr, false) {
        assert_eq!(n, 0, "latched off, yet `{key}` served:\n{stderr}");
    }
}
