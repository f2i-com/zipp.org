//! Member calls and member tags must resolve their references before arguments
//! or template substitutions, and must retain the exact callable and receiver.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const SEMANTICS: &str = r#"
  var events = [];
  function eq(actual, expected, label) {
    if (actual !== expected) throw new Error(label + ": " + actual + " != " + expected);
  }
  function old(x) { events.push("old:" + this.id + ":" + x); return "old-" + this.id; }
  function wrong() { events.push("WRONG"); return "wrong"; }
  function arg(label, value) { events.push(label + "-arg"); return value; }

  // Named accessor Get and exact method/receiver capture.
  var o = {id: "O", m: old};
  Reflect.defineProperty(o, "g", {
    configurable: true,
    get: function () { events.push("named-get"); return old; }
  });
  eq(o.g(arg("named", 1)), "old-O", "named result");
  eq(events.splice(0).join("|"), "named-get|named-arg|old:O:1", "named order");

  function mutateNamed() { events.push("mutate"); o.m = wrong; return 2; }
  eq(o.m(mutateNamed()), "old-O", "named captured callable");
  eq(events.splice(0).join("|"), "mutate|old:O:2", "named mutation order");
  o.m = old;
  var recv = o;
  function rebindReceiver() { events.push("rebind"); recv = {id: "NEW", m: wrong}; return 3; }
  eq(recv.m(rebindReceiver()), "old-O", "captured receiver");
  eq(events.splice(0).join("|"), "rebind|old:O:3", "receiver order");

  // Proxy [[Get]] and computed ToPropertyKey both precede arguments.
  var proxied = new Proxy(o, {
    get: function (target, key, receiver) {
      events.push("trap:" + key);
      return target[key];
    }
  });
  eq(proxied.m(arg("proxy", 4)), "old-O", "proxy result");
  eq(events.splice(0).join("|"), "trap:m|proxy-arg|trap:id|old:O:4|trap:id", "proxy order");

  var key = {
    toString: function () { events.push("key"); return "m"; }
  };
  eq(o[key](arg("computed", 5)), "old-O", "computed result");
  eq(events.splice(0).join("|"), "key|computed-arg|old:O:5", "computed order");

  var nullArgRan = false;
  try { null.m((nullArgRan = true)); } catch (e) {}
  eq(nullArgRan, false, "nullish Get precedes arguments");

  function pseudoShadows(console, performance) {
    return console.log(arg("console-shadow", 5.25)) + performance.now(arg("performance-shadow", 5.5));
  }
  eq(pseudoShadows({id: "C", log: old}, {id: "PSEUDO", now: old}), "old-Cold-PSEUDO", "host pseudo lexical shadows");
  eq(events.splice(0).join("|"), "console-shadow-arg|old:C:5.25|performance-shadow-arg|old:PSEUDO:5.5", "host pseudo shadow order");
  var performanceArgRan = false;
  performance.now((performanceArgRan = true));
  eq(performanceArgRan, true, "performance.now evaluates arguments");

  // Spread iteration cannot redirect a previously resolved method.
  o.m = old;
  function spreadNamed() { events.push("spread"); o.m = wrong; return [6]; }
  eq(o.m(...spreadNamed()), "old-O", "spread captured callable");
  eq(events.splice(0).join("|"), "spread|old:O:6", "spread order");
  o.m = old;
  var spreadKey = {toString: function () { events.push("spread-key"); return "m"; }};
  function spreadComputed() { events.push("spread-values"); o.m = wrong; return [7]; }
  eq(o[spreadKey](...spreadComputed()), "old-O", "computed spread capture");
  eq(events.splice(0).join("|"), "spread-key|spread-values|old:O:7", "computed spread order");

  // Optional calls short-circuit before spread evaluation and retain `this`.
  var optionalRan = false;
  var nil = null;
  nil?.m(...[(optionalRan = true)]);
  eq(optionalRan, false, "optional base skips spread");
  var missing = {id: "M"};
  missing.m?.(...[(optionalRan = true)]);
  eq(optionalRan, false, "optional callee skips spread");
  o.m = old;
  function optionalSpread() { events.push("optional-spread"); o.m = wrong; return [8]; }
  eq(o.m?.(...optionalSpread()), "old-O", "optional spread capture");
  eq(events.splice(0).join("|"), "optional-spread|old:O:8", "optional spread order");
  o.m = old;
  var optionalKey = {toString: function () { events.push("optional-key"); return "m"; }};
  eq(o[optionalKey]?.(...arg("optional-computed", [8.5])), "old-O", "optional computed spread");
  eq(events.splice(0).join("|"), "optional-key|optional-computed-arg|old:O:8.5", "optional computed order");

  // Private accessor/method calls preserve both brand-check/Get order and this.
  class PrivateCalls {
    constructor() { this.id = "P"; }
    get #g() { events.push("private-get"); return old; }
    #m(x) { events.push("private-method:" + this.id + ":" + x); return this.id; }
    getter() { return this.#g(arg("private", 9)); }
    method() { return this.#m(arg("private-method", 10)); }
    spread() { return this.#m(...arg("private-spread", [10.5])); }
    optional() { return this.#m?.(...arg("private-optional", [10.75])); }
  }
  var pc = new PrivateCalls();
  eq(pc.getter(), "old-P", "private getter result");
  eq(events.splice(0).join("|"), "private-get|private-arg|old:P:9", "private getter order");
  eq(pc.method(), "P", "private method result");
  eq(events.splice(0).join("|"), "private-method-arg|private-method:P:10", "private method order");
  eq(pc.spread(), "P", "private spread result");
  eq(events.splice(0).join("|"), "private-spread-arg|private-method:P:10.5", "private spread order");
  eq(pc.optional(), "P", "private optional result");
  eq(events.splice(0).join("|"), "private-optional-arg|private-method:P:10.75", "private optional order");

  // Super property Gets (named/computed/spread) precede their arguments.
  class Base {}
  Reflect.defineProperty(Base.prototype, "n", {
    configurable: true,
    get: function () { events.push("super-get"); return old; }
  });
  Reflect.defineProperty(Base.prototype, "s", {
    configurable: true,
    get: function () { events.push("super-spread-get"); return old; }
  });
  Reflect.defineProperty(Base.prototype, "c", {
    configurable: true,
    get: function () { events.push("super-computed-spread-get"); return old; }
  });
  class Derived extends Base {
    constructor() { super(); this.id = "D"; }
    named() { return super.n(arg("super", 11)); }
    computed(k) { return super[k](arg("super-computed", 12)); }
    spread() { return super.s(...arg("super-spread", [13])); }
    computedSpread(k) { return super[k](...arg("super-computed-spread", [13.5])); }
    optional() { return super.n?.(...arg("super-optional", [13.75])); }
  }
  var d = new Derived();
  eq(d.named(), "old-D", "super named result");
  eq(events.splice(0).join("|"), "super-get|super-arg|old:D:11", "super named order");
  var superKey = {toString: function () { events.push("super-key"); return "n"; }};
  eq(d.computed(superKey), "old-D", "super computed result");
  eq(events.splice(0).join("|"), "super-key|super-get|super-computed-arg|old:D:12", "super computed order");
  eq(d.spread(), "old-D", "super spread result");
  eq(events.splice(0).join("|"), "super-spread-get|super-spread-arg|old:D:13", "super spread order");
  var superSpreadKey = {toString: function () { events.push("super-computed-spread-key"); return "c"; }};
  eq(d.computedSpread(superSpreadKey), "old-D", "super computed spread result");
  eq(events.splice(0).join("|"), "super-computed-spread-key|super-computed-spread-get|super-computed-spread-arg|old:D:13.5", "super computed spread order");
  eq(d.optional(), "old-D", "super optional result");
  eq(events.splice(0).join("|"), "super-get|super-optional-arg|old:D:13.75", "super optional order");

  // Static field initializers carry an effective `this` outside frame reg 0.
  class StaticBase {
    static get m() { events.push("static-super-get:" + this.id); return old; }
    static get t() {
      events.push("static-super-tag-get:" + this.id);
      return function (strings, value) {
        events.push("static-super-tag-call:" + this.id + ":" + value);
        return this.id;
      };
    }
  }
  class StaticDerived extends StaticBase {
    static id = "SD";
    static result = super.m(arg("static-super", 13.875));
    static computed = super[{toString: function () { events.push("static-super-key"); return "m"; }}](arg("static-super-computed", 13.9375));
    static tagged = super.t`x${arg("static-super-tag", 13.96875)}y`;
  }
  eq(StaticDerived.result, "old-SD", "static-field super receiver");
  eq(StaticDerived.computed, "old-SD", "static-field computed super receiver");
  eq(StaticDerived.tagged, "SD", "static-field super tag receiver");
  eq(events.splice(0).join("|"), "static-super-get:SD|static-super-arg|old:SD:13.875|static-super-key|static-super-get:SD|static-super-computed-arg|old:SD:13.9375|static-super-tag-get:SD|static-super-tag-arg|static-super-tag-call:SD:13.96875", "static-field super order");

  var savedFunctionCall = Function.prototype.call;
  Function.prototype.call = function () { throw new Error("static block observed .call replacement"); };
  class StaticBlock {
    static { this.ok = 1; }
  }
  Function.prototype.call = savedFunctionCall;
  eq(StaticBlock.ok, 1, "static block direct invocation");

  // Member tags resolve before substitutions and retain every receiver kind.
  var tags = {id: "T"};
  Reflect.defineProperty(tags, "tag", {
    configurable: true,
    get: function () {
      events.push("tag-get");
      return function (strings, value) {
        events.push("tag-call:" + this.id + ":" + value);
        return this.id;
      };
    }
  });
  eq(tags.tag`a${arg("tag", 14)}b`, "T", "static member tag");
  eq(events.splice(0).join("|"), "tag-get|tag-arg|tag-call:T:14", "static tag order");
  var tagKey = {toString: function () { events.push("tag-key"); return "tag"; }};
  eq(tags[tagKey]`a${arg("computed-tag", 15)}b`, "T", "computed member tag");
  eq(events.splice(0).join("|"), "tag-key|tag-get|computed-tag-arg|tag-call:T:15", "computed tag order");

  class PrivateTag {
    constructor() { this.id = "PT"; }
    #tag(strings, value) { events.push("private-tag:" + this.id + ":" + value); return this.id; }
    run() { return this.#tag`x${arg("private-tag", 16)}y`; }
  }
  eq(new PrivateTag().run(), "PT", "private tag result");
  eq(events.splice(0).join("|"), "private-tag-arg|private-tag:PT:16", "private tag order");

  class TagBase {
    tag(strings, value) { events.push("super-tag:" + this.id + ":" + value); return this.id; }
  }
  class TagDerived extends TagBase {
    constructor() { super(); this.id = "ST"; }
    run() { return super.tag`x${arg("super-tag", 17)}y`; }
  }
  eq(new TagDerived().run(), "ST", "super tag result");
  eq(events.splice(0).join("|"), "super-tag-arg|super-tag:ST:17", "super tag order");

  var withTag = {
    id: "W",
    tag: function (strings, value) { events.push("with-tag:" + this.id + ":" + value); return this.id; }
  };
  var withResult;
  with (withTag) { withResult = tag`x${arg("with-tag", 18)}y`; }
  eq(withResult, "W", "with tag result");
  eq(events.splice(0).join("|"), "with-tag-arg|with-tag:W:18", "with tag order");

  console.log("call-reference-order:ok");
"#;

const HOT_CAPTURE: &str = r#"
  var trace = [];
  var recording = false;
  function first(x) {
    if (recording) trace.push("first:" + this.id + ":" + x);
    return this.value + x;
  }
  function wrong(x) {
    if (recording) trace.push("WRONG");
    return 1000 + x;
  }
  var obj = {id: "O", value: 10, method: first};
  function invoke(makeArg) { return obj.method(makeArg()); }
  for (var i = 0; i < 6000; i++) invoke(function () { return 1; });
  recording = true;
  function replace() { trace.push("arg"); obj.method = wrong; return 2; }
  var replaced = invoke(replace);

  obj.method = first;
  var receiver = obj;
  function invokeReceiver(makeArg) { return receiver.method(makeArg()); }
  recording = false;
  for (var j = 0; j < 6000; j++) invokeReceiver(function () { return 1; });
  recording = true;
  function rebind() {
    trace.push("rebind");
    receiver = {id: "NEW", value: 100, method: wrong};
    return 3;
  }
  var rebound = invokeReceiver(rebind);
  console.log("hot:" + replaced + ":" + rebound + ":" + trace.join("|"));
"#;

#[test]
fn call_reference_order_child() {
    if std::env::var_os("ZIPP_CALL_REFERENCE_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(SEMANTICS), ["call-reference-order:ok"]);
}

#[test]
fn call_reference_hot_child() {
    if std::env::var_os("ZIPP_CALL_REFERENCE_HOT_CHILD").is_none() {
        return;
    }
    assert_eq!(
        run_ok(HOT_CAPTURE),
        ["hot:12:13:arg|first:O:2|rebind|first:O:3"]
    );
}

#[test]
fn call_reference_order_modes_match() {
    if std::env::var_os("ZIPP_CALL_REFERENCE_CHILD").is_some()
        || std::env::var_os("ZIPP_CALL_REFERENCE_HOT_CHILD").is_some()
    {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (test, marker) in [
        ("call_reference_order_child", "ZIPP_CALL_REFERENCE_CHILD"),
        ("call_reference_hot_child", "ZIPP_CALL_REFERENCE_HOT_CHILD"),
    ] {
        for (mode, env) in [
            ("default", None),
            ("interpreter", Some(("ZIPP_NOJIT", "1"))),
            ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
            ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ] {
            let mut cmd = std::process::Command::new(&exe);
            cmd.args(["--exact", test, "--nocapture"])
                .env(marker, "1")
                .env_remove("ZIPP_NOJIT")
                .env_remove("ZIPP_JIT_THRESHOLD")
                .env_remove("ZIPP_GC_STRESS");
            if let Some((key, value)) = env {
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
}
