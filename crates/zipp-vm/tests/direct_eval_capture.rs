//! A syntactic `eval(...)` call resolves its reference before evaluating the
//! argument list. Direct semantics depend on that captured value's identity;
//! argument-side rebinding must neither turn a fake callee into %eval% nor turn
//! a captured %eval% into an ordinary call. Guard misses receive every arg.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}\nsource:\n{src}",
        out.error
    );
    out.output
}

const SEMANTICS: &str = r#"
  var intrinsicEval = eval;
  var calls = [];
  function eq(actual, expected, label) {
    if (actual !== expected) throw new Error(label + ": " + actual + " != " + expected);
  }
  function fake(a, b, c) {
    calls.push("fake:" + arguments.length + ":" + a + ":" + b + ":" + c);
    return "fake-result";
  }
  function withFake(a, b, c) {
    calls.push("with:" + this.tag + ":" + arguments.length + ":" + a + ":" + b + ":" + c);
    return "with-result";
  }
  function extra(label) { calls.push(label); return label; }

  // Global reference: fake -> intrinsic during arg evaluation must still call
  // fake, with all args. The old dispatch re-read global `eval` and leaked the
  // caller lexical named by the source string.
  eval = fake;
  function globalFakeFirst() {
    let secret = "GLOBAL-LEAK";
    return eval((eval = intrinsicEval, "secret"), extra("g-extra"), 3);
  }
  eq(globalFakeFirst(), "fake-result", "global fake capture");
  eq(calls.splice(0).join("|"), "g-extra|fake:3:secret:g-extra:3", "global fake args");

  eval = fake;
  eq(eval(), "fake-result", "zero-argument guard miss");
  eq(calls.splice(0).join("|"), "fake:0:undefined:undefined:undefined", "zero argument count");
  eq(eval(...[]), "fake-result", "empty-spread guard miss");
  eq(calls.splice(0).join("|"), "fake:0:undefined:undefined:undefined", "empty spread count");

  // The inverse snapshot: capture %eval%, then replace the global from an arg.
  // Direct eval still runs, while every extra argument is evaluated and ignored.
  eval = intrinsicEval;
  function globalIntrinsicFirst() {
    let secret = "GLOBAL-DIRECT";
    return eval((eval = fake, "secret"), extra("gi-extra"), 4);
  }
  eq(globalIntrinsicFirst(), "GLOBAL-DIRECT", "global intrinsic capture");
  eq(calls.splice(0).join("|"), "gi-extra", "global intrinsic extras");
  eval = intrinsicEval;

  // Local/parameter and upvalue bindings named `eval` obey the same value
  // identity rule. Their live binding register/cell may change during args;
  // the captured call target must not.
  function localFakeFirst(eval) {
    let secret = "LOCAL-LEAK";
    return eval((eval = intrinsicEval, "secret"), extra("l-extra"), 5);
  }
  eq(localFakeFirst(fake), "fake-result", "local fake capture");
  eq(calls.splice(0).join("|"), "l-extra|fake:3:secret:l-extra:5", "local fake args");

  function localIntrinsicFirst(eval) {
    let secret = "LOCAL-DIRECT";
    return eval((eval = fake, "secret"), extra("li-extra"));
  }
  eq(localIntrinsicFirst(intrinsicEval), "LOCAL-DIRECT", "local intrinsic capture");
  eq(calls.splice(0).join("|"), "li-extra", "local intrinsic extras");

  function makeUpvalueFake(eval) {
    return function () {
      let secret = "UPVALUE-LEAK";
      return eval((eval = intrinsicEval, "secret"), extra("u-extra"), 6);
    };
  }
  eq(makeUpvalueFake(fake)(), "fake-result", "upvalue fake capture");
  eq(calls.splice(0).join("|"), "u-extra|fake:3:secret:u-extra:6", "upvalue fake args");

  function makeUpvalueIntrinsic(eval) {
    return function () {
      let secret = "UPVALUE-DIRECT";
      return eval((eval = fake, "secret"), extra("ui-extra"));
    };
  }
  eq(makeUpvalueIntrinsic(intrinsicEval)(), "UPVALUE-DIRECT", "upvalue intrinsic capture");
  eq(calls.splice(0).join("|"), "ui-extra", "upvalue intrinsic extras");

  // `with` adds an observable reference receiver. A non-intrinsic guard miss
  // must call the captured function with WithBaseObject, not undefined.
  function withFakeFirst() {
    let secret = "WITH-LEAK";
    var holder = {tag: "WF", eval: withFake};
    var result;
    with (holder) {
      result = eval((holder.eval = intrinsicEval, "secret"), extra("w-extra"), 7);
    }
    return result;
  }
  eq(withFakeFirst(), "with-result", "with fake capture");
  eq(calls.splice(0).join("|"), "w-extra|with:WF:3:secret:w-extra:7", "with receiver and args");

  function withIntrinsicFirst() {
    let secret = "WITH-DIRECT";
    var holder = {tag: "WI", eval: intrinsicEval};
    var result;
    with (holder) {
      result = eval((holder.eval = withFake, "secret"), extra("wi-extra"));
    }
    return result;
  }
  eq(withIntrinsicFirst(), "WITH-DIRECT", "with intrinsic capture");
  eq(calls.splice(0).join("|"), "wi-extra", "with intrinsic extras");

  // Spread iteration occurs after reference capture and materializes the full
  // argument vector. It can mutate the binding but cannot redirect the call.
  eval = fake;
  function globalSpreadFake() {
    let secret = "SPREAD-LEAK";
    function values() { eval = intrinsicEval; calls.push("gs-build"); return ["secret", "S", 8]; }
    return eval(...values());
  }
  eq(globalSpreadFake(), "fake-result", "spread fake capture");
  eq(calls.splice(0).join("|"), "gs-build|fake:3:secret:S:8", "spread fake args");

  eval = intrinsicEval;
  function globalSpreadIntrinsic() {
    let secret = "SPREAD-DIRECT";
    function values() { eval = fake; calls.push("gsi-build"); return ["secret", "ignored", 9]; }
    return eval(...values());
  }
  eq(globalSpreadIntrinsic(), "SPREAD-DIRECT", "spread intrinsic capture");
  eq(calls.splice(0).join("|"), "gsi-build", "spread intrinsic extras");
  eval = intrinsicEval;

  function localSpread(eval) {
    function values() { eval = intrinsicEval; calls.push("ls-build"); return ["secret", "LS", 10]; }
    let secret = "LOCAL-SPREAD-LEAK";
    return eval(...values());
  }
  eq(localSpread(fake), "fake-result", "local spread fake capture");
  eq(calls.splice(0).join("|"), "ls-build|fake:3:secret:LS:10", "local spread args");

  function withSpread() {
    var holder = {tag: "WS", eval: withFake};
    function values() { holder.eval = intrinsicEval; calls.push("ws-build"); return ["secret", "WS", 11]; }
    var result;
    with (holder) { result = eval(...values()); }
    return result;
  }
  eq(withSpread(), "with-result", "with spread fake capture");
  eq(calls.splice(0).join("|"), "ws-build|with:WS:3:secret:WS:11", "with spread receiver");

  // Proper-tail-call lowering uses the same snapshot. Both directions are
  // tested in strict functions so the `tail` DirectEval path is emitted.
  function setIntrinsicArg() { eval = intrinsicEval; return "secret"; }
  function setFakeArg() { eval = fake; return "secret"; }
  eval = fake;
  function tailFakeFirst() {
    "use strict";
    let secret = "TAIL-LEAK";
    return eval(setIntrinsicArg(), extra("t-extra"), 12);
  }
  eq(tailFakeFirst(), "fake-result", "tail fake capture");
  eq(calls.splice(0).join("|"), "t-extra|fake:3:secret:t-extra:12", "tail fake args");

  eval = intrinsicEval;
  function tailIntrinsicFirst() {
    "use strict";
    let secret = "TAIL-DIRECT";
    return eval(setFakeArg(), extra("ti-extra"));
  }
  eq(tailIntrinsicFirst(), "TAIL-DIRECT", "tail intrinsic capture");
  eq(calls.splice(0).join("|"), "ti-extra", "tail intrinsic extras");
  eval = intrinsicEval;

  // Argument abrupt completion wins and later args/the captured callee do not
  // run. The reference still resolves before the first argument.
  var throwHolder = {tag: "TH", eval: withFake};
  try {
    with (throwHolder) {
      eval(extra("before-throw"), (function () { calls.push("throw"); throw "boom"; })(), extra("after-throw"));
    }
  } catch (e) { calls.push("caught:" + e); }
  eq(calls.splice(0).join("|"), "before-throw|throw|caught:boom", "argument throw ordering");

  // Every createRealm child owns a distinct %eval%. A direct call inside child
  // code recognizes only that child's canonical eval, inherits child locals,
  // and keeps declarations in the child. Main/foreign eval identities remain
  // ordinary indirect calls even when held in a binding literally named eval.
  eval = intrinsicEval;
  var child = $262.createRealm().global;
  child.mainEval = intrinsicEval;
  eq(child.eval("var x=1; eval('x=2'); x"), 2, "child nested direct global");
  eq(child.x, 2, "child global updated");
  eq(child.eval("(function(){ let localOnly=1; var r=eval('localOnly=2; localOnly'); return r+':' + localOnly; })()"),
     "2:2", "child direct local scope");
  eq(typeof child.localOnly, "undefined", "child local did not leak");
  child.childMarker = 1;
  eq(child.eval("(function(eval){ let localOnly=1; return eval('typeof localOnly + \":\" + typeof childMarker'); })(mainEval)"),
     "undefined:undefined", "main eval is foreign in child");
  function mainCallsForeign(eval) {
    let localOnly = 1;
    return eval("typeof localOnly + ':' + typeof x");
  }
  eq(mainCallsForeign(child.eval), "undefined:number", "child eval is foreign in main");
  eq(typeof x, "undefined", "child global did not leak to main");

  console.log("direct-eval-capture:ok");
"#;

#[test]
fn direct_eval_capture_child() {
    if std::env::var_os("ZIPP_DIRECT_EVAL_CAPTURE_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(SEMANTICS), ["direct-eval-capture:ok"]);
}

#[test]
fn direct_eval_capture_modes_match() {
    if std::env::var_os("ZIPP_DIRECT_EVAL_CAPTURE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "direct_eval_capture_child", "--nocapture"])
            .env("ZIPP_DIRECT_EVAL_CAPTURE_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn direct_eval_bytecode_carries_reference_and_complete_arguments() {
    let text = zipp_vm::compile_to_text(
        "function f(eval, a, b) { return eval((eval = a, 'x'), b, 3); }",
        false,
    )
    .expect("source compiles");
    assert!(text.contains("DirectEval {"), "{text}");
    assert!(text.contains("callee:"), "{text}");
    assert!(text.contains("arg_base:"), "{text}");
    assert!(text.contains("argc: 3"), "{text}");
    assert!(!text.contains("IsEvalFn"), "{text}");

    let tail = zipp_vm::compile_to_text(
        "function side(){ return 'x'; } function f(){ 'use strict'; return eval(side(), 2); }",
        false,
    )
    .expect("tail source compiles");
    assert!(tail.contains("DirectEval {"), "{tail}");
    assert!(tail.contains("argc: 2"), "{tail}");
    assert!(tail.contains("tail: true"), "{tail}");
}
