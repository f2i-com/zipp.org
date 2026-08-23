//! Exactness boundary for the guarded Tier-C whole-tree JSON walk reducer.
//!
//! `walk` is deliberately the closed bytecode shape recognised by
//! `JsonWalkPlan`. Each test warms past Tier C before its measured call. Plain
//! dense data may be reduced as one host traversal; every observable or exotic
//! shape must decline before effects and execute the ordinary body.

const PRELUDE: &str = r#"
"use strict";
var nodes = 0, numSum2x = 0, strs = 0, bools = 0, nulls = 0, strLen = 0;
function walk(v) {
  nodes++;
  if (v === null) { nulls++; return; }
  var t = typeof v;
  if (t === "number") { numSum2x += v * 2; return; }
  if (t === "string") { strs++; strLen += v.length; return; }
  if (t === "boolean") { bools++; return; }
  if (Array.isArray(v)) {
    for (var i = 0; i < v.length; i++) walk(v[i]);
    return;
  }
  for (var k in v) walk(v[k]);
}
function reset() { nodes = 0; numSum2x = 0; strs = 0; bools = 0; nulls = 0; strLen = 0; }
function stats() { return nodes + "," + numSum2x + "," + strs + "," + strLen + "," + bools + "," + nulls; }
function invoke(v) { return walk(v); }
var warmTree = {a:[1,"x",true,null,{b:2.5}]};
for (var warm = 0; warm < 40; warm++) { reset(); invoke(warmTree); }
reset();
"#;

fn run_case(body: &str) -> Vec<String> {
    let src = format!("{PRELUDE}\n{body}");
    let out = zipp_vm::run(&src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

#[test]
fn plain_dense_alias_and_utf16_length() {
    let out = run_case(
        r#"
        var shared = {a:3, b:["x", false]};
        invoke({first:[1, "😀", true, null, {x:2.5}], second:shared, third:shared});
        console.log(stats());
        "#,
    );
    // The shared object is intentionally visited twice. String length is in
    // UTF-16 units (astral 😀 = 2), matching `v.length`.
    assert_eq!(out, ["18,19,3,4,3,1"]);
}

#[test]
fn accessors_prototypes_sparse_arrays_and_integer_order_decline() {
    let out = run_case(
        r#"
        var effects = 0;
        var a = {x:1};
        Object.defineProperty(a, "y", {enumerable:true, get:function () { effects++; return [2]; }});
        invoke(a);
        console.log("access=" + stats() + ":" + effects);

        reset();
        Object.prototype.tail = 2;
        invoke({a:1});
        delete Object.prototype.tail;
        console.log("objproto=" + stats());

        reset();
        var custom = Object.create({z:2}); custom.a = 1;
        invoke(custom);
        console.log("custom=" + stats());

        reset();
        Array.prototype[1] = 4;
        invoke([1,,3]);
        delete Array.prototype[1];
        console.log("hole=" + stats());

        reset();
        invoke({"2":-10000000000000000, "1":1, x:10000000000000000});
        console.log("indexorder=" + stats());

        reset(); effects = 0;
        var quiet = {a:1};
        Object.defineProperty(quiet, "hidden", {enumerable:false, get:function () { effects++; return 9; }});
        invoke(quiet);
        console.log("hidden=" + stats() + ":" + effects);
        "#,
    );
    assert_eq!(
        out,
        [
            "access=4,6,0,0,0,0:1",
            "objproto=3,6,0,0,0,0",
            "custom=3,6,0,0,0,0",
            "hole=4,16,0,0,0,0",
            "indexorder=4,0,0,0,0,0",
            "hidden=2,2,0,0,0,0:0",
        ]
    );
}

#[test]
fn proxy_counter_coercion_and_live_self_binding_are_observed() {
    let out = run_case(
        r#"
        var own = 0, desc = 0, gets = 0;
        var p = new Proxy({a:1,b:2}, {
          ownKeys:function (t) { own++; return Reflect.ownKeys(t); },
          getOwnPropertyDescriptor:function (t,k) { desc++; return Object.getOwnPropertyDescriptor(t,k); },
          get:function (t,k) { gets++; return t[k]; }
        });
        invoke(p);
        console.log("proxy=" + stats() + ":" + own + "," + desc + "," + gets);

        reset();
        var coerces = 0;
        nodes = {valueOf:function () { coerces++; return 10; }};
        invoke({a:1});
        console.log("coerce=" + stats() + ":" + coerces);

        reset();
        var original = walk;
        walk = function () { nodes += 1000; };
        function invokeOriginal(v) { return original(v); }
        for (var q = 0; q < 40; q++) invokeOriginal({});
        reset();
        invokeOriginal({a:1});
        console.log("rebind=" + stats());
        "#,
    );
    assert_eq!(
        out,
        [
            "proxy=3,6,0,0,0,0:2,2,2",
            "coerce=12,2,0,0,0,0:1",
            "rebind=1001,0,0,0,0,0",
        ]
    );
}

#[test]
fn deep_acyclic_input_declines_without_changing_the_result() {
    let out = run_case(
        r#"
        var deep = 1;
        for (var i = 0; i < 300; i++) deep = [deep];
        invoke(deep);
        console.log(stats());
        "#,
    );
    // The reducer's depth cap is lower than this tree, so the ordinary recursive
    // body must run. This keeps native reduction from masking stack-limit errors.
    assert_eq!(out, ["301,2,0,0,0,0"]);
}

#[test]
fn cyclic_input_declines_to_the_normal_catchable_stack_limit() {
    // The parent test below re-runs this binary under GC-on-every-safe-point;
    // combining that mode with the intentional 100k-frame overflow would spend
    // minutes collecting. The default process is sufficient for this decline.
    if std::env::var_os("ZIPP_JSON_WALK_CHILD").is_some() {
        return;
    }
    let out = run_case(
        r#"
        var cycle = []; cycle.push(cycle);
        try { invoke(cycle); console.log("missed"); }
        catch (e) { console.log("cycle=" + (e instanceof RangeError)); }
        "#,
    );
    assert_eq!(out, ["cycle=true"]);
}

/// The plan switch is read when each Tier-C function is compiled; fresh test
/// processes give a real same-binary old-path run. GC stress collects at the
/// cross-call safe point immediately before the reducer, proving that the root
/// remains live and every borrowed heap index survives the admitted traversal.
#[test]
fn zz_off_switch_and_gc_stress_agree() {
    if std::env::var_os("ZIPP_JSON_WALK_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (label, key) in [
        ("off", "ZIPP_NO_JSON_WALK_REDUCE"),
        ("gc", "ZIPP_GC_STRESS"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["--skip", "zz_off_switch_and_gc_stress_agree"])
            .env("ZIPP_JSON_WALK_CHILD", "1")
            .env(key, "1")
            .output()
            .expect("re-run focused test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && !stdout.contains(" 0 passed"),
            "{label} child diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
