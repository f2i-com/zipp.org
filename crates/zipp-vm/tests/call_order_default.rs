//! A method call resolves its reference before it evaluates its arguments —
//! by default, in every execution mode, whatever the environment carries.
//!
//! The 6 September 2026 audit (Z01) reproduced the pre-audit default with
//! `receiver.m(input.value)`: the argument's getter ran before the method's,
//! because the fused `CallMethod` performed its property Get after the
//! arguments were in their registers and the "primitive-operand" argument
//! class admitted a property read. That class is now opt-in
//! (`ZIPP_RELAXED_CALL_ORDER=1`, diagnostics only); the shipped default takes
//! the captured lowering for every argument that could observe the order.
//!
//! What is pinned, per probe: a method getter runs before an argument getter;
//! an argument's coercion cannot replace the method that was already fetched;
//! a method getter's write to a global is what the argument reads; template
//! and unary coercions are ordered the same way; a proxy receiver's `get` trap
//! precedes the arguments; when both sides throw, the method's exception is
//! the one seen; and a warmed site behaves like a cold one. Each runs in a
//! clean child process under the default, interpreter, forced-JIT, GC-stress
//! and explicit strict modes. The relaxed switch is checked to still exist
//! and to be the only way to get the old order.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The audit's own probe, verbatim, plus one line per sibling shape.
const PROBES: &str = r#"
  var lines = [];
  function log(label, value) { lines.push(label + "=" + value); }

  // 1. The audit's regression: method Get before argument Get.
  (function () {
    var trace = [];
    var receiver = {
      get m() { trace.push("method"); return function (value) { trace.push("call:" + value); }; }
    };
    var input = { get value() { trace.push("argument"); return 7; } };
    receiver.m(input.value);
    log("getter-order", trace.join("|"));
  })();

  // 2. An argument's coercion cannot replace a method already fetched.
  (function () {
    var o = { m: function (x) { return "first:" + x; } };
    var coerce = { valueOf: function () { o.m = function (x) { return "second:" + x; }; return 5; } };
    log("coercion-binary", o.m(coerce + 1));
    o.m = function (x) { return "first:" + x; };
    log("coercion-unary", o.m(+coerce));
    o.m = function (x) { return "first:" + x; };
    var str = { toString: function () { o.m = function (x) { return "second:" + x; }; return "s"; } };
    log("coercion-template", o.m(`${str}`));
  })();

  // 3. The method getter runs first, so its write is what the argument reads.
  var shared = 1;
  var holder = { get m() { shared = 2; return function (x) { return x; }; } };
  log("getter-writes-global", holder.m(shared));

  // 4. A proxy receiver's trap precedes the arguments.
  (function () {
    var trace = [];
    var target = { m: function (x) { trace.push("call:" + x); } };
    var proxied = new Proxy(target, { get: function (t, key) { trace.push("trap:" + String(key)); return t[key]; } });
    var input = { get value() { trace.push("argument"); return 3; } };
    proxied.m(input.value);
    log("proxy-order", trace.join("|"));
  })();

  // 5. Both sides throw: the method's exception is the one seen.
  (function () {
    var receiver = { get m() { throw new Error("M"); } };
    var input = { get value() { throw new Error("A"); } };
    try { receiver.m(input.value); log("throw-order", "none"); }
    catch (e) { log("throw-order", e.message); }
  })();

  // 6. A warmed site: the same order after the tiers have had their chance.
  (function () {
    var trace = [];
    var receiver = {
      get m() { trace.push("m"); return function (value) { return value; }; }
    };
    var input = { get value() { trace.push("a"); return 1; } };
    var last = "";
    for (var i = 0; i < 64; i++) {
      trace.length = 0;
      receiver.m(input.value);
      last = trace.join("|");
    }
    log("hot-order", last);
  })();

  // 7. What stays fused: arguments that cannot observe the order still work.
  (function () {
    var trace = [];
    var receiver = {
      get m() { trace.push("m"); return function () { return Array.prototype.slice.call(arguments).join(","); }; }
    };
    var local = 4;
    log("fused-args", receiver.m(1, "x", local, -1, 2 + 3, `${"a"}${1}`, [local, 2], { k: local }, function () {}));
    log("fused-trace", trace.join("|"));
  })();

  console.log(lines.join(";"));
"#;

const EXPECTED: &str = "getter-order=method|argument|call:7;\
coercion-binary=first:6;\
coercion-unary=first:5;\
coercion-template=first:s;\
getter-writes-global=2;\
proxy-order=trap:m|argument|call:3;\
throw-order=M;\
hot-order=m|a;\
fused-args=1,x,4,-1,5,a1,4,2,[object Object],function () {};\
fused-trace=m";

#[test]
fn call_order_default_child() {
    if std::env::var_os("ZIPP_CALL_ORDER_DEFAULT_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(PROBES), [EXPECTED]);
}

/// The relaxed switch still exists, is the only way to get the old order,
/// and does what its name says: with it set, the audit's probe logs the
/// argument first. Update this expectation when the relaxed class gains a
/// captured-call inline cache that makes it correct (HANDOFF B279).
#[test]
fn call_order_relaxed_child() {
    if std::env::var_os("ZIPP_CALL_ORDER_RELAXED_CHILD").is_none() {
        return;
    }
    let out = run_ok(PROBES);
    assert!(
        out[0].starts_with("getter-order=argument|method|call:7;"),
        "relaxed order should reorder the audit's probe: {}",
        out[0]
    );
}

#[test]
fn call_order_default_modes_match() {
    if std::env::var_os("ZIPP_CALL_ORDER_DEFAULT_CHILD").is_some()
        || std::env::var_os("ZIPP_CALL_ORDER_RELAXED_CHILD").is_some()
    {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let mut runs: Vec<(&str, &str, Vec<(&str, &str)>)> = Vec::new();
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("strict-order", Some(("ZIPP_STRICT_CALL_ORDER", "1"))),
    ] {
        runs.push((
            "call_order_default_child",
            mode,
            env.into_iter()
                .chain([("ZIPP_CALL_ORDER_DEFAULT_CHILD", "1")])
                .collect(),
        ));
    }
    // Strict wins when both are set: the relaxed switch alone reorders, the
    // pair does not.
    runs.push((
        "call_order_default_child",
        "both-switches",
        vec![
            ("ZIPP_STRICT_CALL_ORDER", "1"),
            ("ZIPP_RELAXED_CALL_ORDER", "1"),
            ("ZIPP_CALL_ORDER_DEFAULT_CHILD", "1"),
        ],
    ));
    runs.push((
        "call_order_relaxed_child",
        "relaxed-order",
        vec![
            ("ZIPP_RELAXED_CALL_ORDER", "1"),
            ("ZIPP_CALL_ORDER_RELAXED_CHILD", "1"),
        ],
    ));
    for (test, mode, env) in runs {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", test, "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_STRICT_CALL_ORDER")
            .env_remove("ZIPP_RELAXED_CALL_ORDER");
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{test}/{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
