//! Static-call specialisations must preserve EvaluateCall's observable order.
//!
//! Every optimised Object/Promise/Array/String/Number call snapshots the live
//! namespace receiver and method before evaluating arguments. The specialised
//! implementation is legal only for the main-realm intrinsic pair; all other
//! pairs are ordinary calls with the captured `this`.

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
  function mark() { return this.tag; }

  var SO = {
    tag: "O", keys: mark, values: mark, entries: mark, assign: mark,
    fromEntries: mark, defineProperty: mark, getOwnPropertyDescriptor: mark,
    getOwnPropertyNames: mark, getPrototypeOf: mark, create: mark,
    defineProperties: mark
  };
  var SP = {
    tag: "P", resolve: mark, reject: mark, all: mark, allSettled: mark,
    race: mark, any: mark
  };
  var SA = { tag: "A", isArray: mark, from: mark, of: mark };
  var SS = { tag: "S", fromCharCode: mark };
  var SN = {
    tag: "N", isInteger: mark, isNaN: mark, isFinite: mark,
    isSafeInteger: mark
  };

  function shadow(Object, Promise, Array, String, Number) {
    var os = [
      Object.keys(1), Object.values(1), Object.entries(1), Object.assign(1),
      Object.fromEntries(1), Object.defineProperty(1),
      Object.getOwnPropertyDescriptor(1), Object.getOwnPropertyNames(1),
      Object.getPrototypeOf(1), Object.create(1), Object.defineProperties(1)
    ].join("");
    var ps = [
      Promise.resolve(1), Promise.reject(1), Promise.all(1),
      Promise.allSettled(1), Promise.race(1), Promise.any(1)
    ].join("");
    var as = [Array.isArray(1), Array.from(1), Array.of(1)].join("");
    var ss = String.fromCharCode(1);
    var ns = [
      Number.isInteger(1), Number.isNaN(1), Number.isFinite(1),
      Number.isSafeInteger(1)
    ].join("");
    return os + "|" + ps + "|" + as + "|" + ss + "|" + ns;
  }
  console.log("shadow:" + shadow(SO, SP, SA, SS, SN));

  var BO = Object, BP = Promise, BA = Array, BS = String, BN = Number;
  Object = SO; var go = Object.assign(1); Object = BO;
  Promise = SP; var gp = Promise.resolve(1); Promise = BP;
  Array = SA; var ga = Array.of(1); Array = BA;
  String = SS; var gs = String.fromCharCode(1); String = BS;
  Number = SN; var gn = Number.isInteger(1); Number = BN;
  console.log("global:" + [go, gp, ga, gs, gn].join(""));

  function tagged(tag, recv) {
    return function () { return this === recv ? tag : "bad-this"; };
  }
  var replaced = [];
  var old;
  old = Object.keys; Object.keys = tagged("o", Object); replaced.push(Object.keys()); Object.keys = old;
  old = Object.values; Object.values = tagged("o", Object); replaced.push(Object.values()); Object.values = old;
  old = Object.entries; Object.entries = tagged("o", Object); replaced.push(Object.entries()); Object.entries = old;
  old = Object.assign; Object.assign = tagged("o", Object); replaced.push(Object.assign()); Object.assign = old;
  old = Object.fromEntries; Object.fromEntries = tagged("o", Object); replaced.push(Object.fromEntries()); Object.fromEntries = old;
  old = Object.defineProperty; Object.defineProperty = tagged("o", Object); replaced.push(Object.defineProperty()); Object.defineProperty = old;
  old = Object.getOwnPropertyDescriptor; Object.getOwnPropertyDescriptor = tagged("o", Object); replaced.push(Object.getOwnPropertyDescriptor()); Object.getOwnPropertyDescriptor = old;
  old = Object.getOwnPropertyNames; Object.getOwnPropertyNames = tagged("o", Object); replaced.push(Object.getOwnPropertyNames()); Object.getOwnPropertyNames = old;
  old = Object.getPrototypeOf; Object.getPrototypeOf = tagged("o", Object); replaced.push(Object.getPrototypeOf()); Object.getPrototypeOf = old;
  old = Object.create; Object.create = tagged("o", Object); replaced.push(Object.create()); Object.create = old;
  old = Object.defineProperties; Object.defineProperties = tagged("o", Object); replaced.push(Object.defineProperties()); Object.defineProperties = old;
  old = Promise.resolve; Promise.resolve = tagged("p", Promise); replaced.push(Promise.resolve()); Promise.resolve = old;
  old = Promise.reject; Promise.reject = tagged("p", Promise); replaced.push(Promise.reject()); Promise.reject = old;
  old = Promise.all; Promise.all = tagged("p", Promise); replaced.push(Promise.all()); Promise.all = old;
  old = Promise.allSettled; Promise.allSettled = tagged("p", Promise); replaced.push(Promise.allSettled()); Promise.allSettled = old;
  old = Promise.race; Promise.race = tagged("p", Promise); replaced.push(Promise.race()); Promise.race = old;
  old = Promise.any; Promise.any = tagged("p", Promise); replaced.push(Promise.any()); Promise.any = old;
  old = Array.isArray; Array.isArray = tagged("a", Array); replaced.push(Array.isArray()); Array.isArray = old;
  old = Array.from; Array.from = tagged("a", Array); replaced.push(Array.from()); Array.from = old;
  old = Array.of; Array.of = tagged("a", Array); replaced.push(Array.of()); Array.of = old;
  old = String.fromCharCode; String.fromCharCode = tagged("s", String); replaced.push(String.fromCharCode()); String.fromCharCode = old;
  old = Number.isInteger; Number.isInteger = tagged("n", Number); replaced.push(Number.isInteger()); Number.isInteger = old;
  old = Number.isNaN; Number.isNaN = tagged("n", Number); replaced.push(Number.isNaN()); Number.isNaN = old;
  old = Number.isFinite; Number.isFinite = tagged("n", Number); replaced.push(Number.isFinite()); Number.isFinite = old;
  old = Number.isSafeInteger; Number.isSafeInteger = tagged("n", Number); replaced.push(Number.isSafeInteger()); Number.isSafeInteger = old;
  console.log("replaced:" + replaced.join(""));

  var order = [];
  function arg(label) { order.push(label + "a"); return 7; }
  function accessor(ns, name, label, result) {
    Reflect.defineProperty(ns, name, {
      configurable: true,
      get: function () {
        order.push(label + "g");
        return function (v) {
          order.push(label + "c:" + (this === ns) + ":" + v);
          return result;
        };
      }
    });
  }
  function restore(ns, name, value) {
    Reflect.defineProperty(ns, name, {
      value: value, writable: true, enumerable: false, configurable: true
    });
  }
  old = Object.keys; accessor(Object, "keys", "O", 1); Object.keys(arg("O")); restore(Object, "keys", old);
  old = Array.isArray; accessor(Array, "isArray", "I", 1); Array.isArray(arg("I")); restore(Array, "isArray", old);
  old = Array.from; accessor(Array, "from", "F", 1); Array.from(arg("F")); restore(Array, "from", old);
  old = Number.isInteger; accessor(Number, "isInteger", "N", 1); Number.isInteger(arg("N")); restore(Number, "isInteger", old);
  old = Promise.resolve; accessor(Promise, "resolve", "P", 1); Promise.resolve(arg("P")); restore(Promise, "resolve", old);
  console.log("order:" + order.join("|"));

  var captured = [];
  old = Object.assign;
  var target = {};
  function mutateAssign() { Object.assign = function () { return "wrong"; }; return {x: 1}; }
  var ar = Object.assign(target, mutateAssign()); Object.assign = old;
  captured.push(ar === target && target.x === 1);
  old = Object.keys;
  function mutateKeys() { Object.keys = function () { return ["wrong"]; }; return {x: 1, y: 2}; }
  captured.push(Object.keys(mutateKeys()).join("") === "xy"); Object.keys = old;
  old = Array.isArray;
  function mutateIsArray() { Array.isArray = function () { return false; }; return []; }
  captured.push(Array.isArray(mutateIsArray())); Array.isArray = old;
  old = Array.from;
  function mutateFrom() { Array.from = function () { return [9]; }; return [4, 5]; }
  captured.push(Array.from(mutateFrom()).join("") === "45"); Array.from = old;
  old = String.fromCharCode;
  function mutateChar() { String.fromCharCode = function () { return "wrong"; }; return 65; }
  captured.push(String.fromCharCode(mutateChar()) === "A"); String.fromCharCode = old;
  old = Number.isInteger;
  function mutateInteger() { Number.isInteger = function () { return false; }; return 3; }
  captured.push(Number.isInteger(mutateInteger())); Number.isInteger = old;
  old = Promise.resolve;
  function mutateResolve() { Promise.resolve = function () { return 0; }; return 8; }
  captured.push(Promise.resolve(mutateResolve()) instanceof Promise); Promise.resolve = old;
  console.log("captured:" + captured.join(""));

  function spreadShadow(Object, Promise, Array, String, Number) {
    return [
      Object.assign(...[1]), Promise.resolve(...[1]), Array.of(...[1]),
      String.fromCharCode(...[1]), Number.isInteger(...[1])
    ].join("");
  }
  var spreadOrder = [];
  old = Object.assign;
  Reflect.defineProperty(Object, "assign", {
    configurable: true,
    get: function () {
      spreadOrder.push("get");
      return function (a, b) {
        spreadOrder.push("call:" + (this === Object) + ":" + a + ":" + b);
        return "ok";
      };
    }
  });
  function spreadArgs() { spreadOrder.push("args"); return [3, 4]; }
  var spreadResult = Object.assign(...spreadArgs());
  restore(Object, "assign", old);
  old = Number.isInteger;
  function mutateSpread() {
    Number.isInteger = function () { return false; };
    return [3];
  }
  var spreadCaptured = Number.isInteger(...mutateSpread());
  Number.isInteger = old;
  console.log(
    "spread:" + spreadShadow(SO, SP, SA, SS, SN) + ":" +
    spreadOrder.join("|") + ":" + spreadResult + ":" + spreadCaptured
  );

  // Returning the saved intrinsic from an accessor still takes the fast path,
  // but only after the observable getter and argument have run.
  var intrinsicKeys = Object.keys;
  var intrinsicOrder = [];
  Reflect.defineProperty(Object, "keys", {
    configurable: true,
    get: function () { intrinsicOrder.push("get"); return intrinsicKeys; }
  });
  function intrinsicArg() { intrinsicOrder.push("arg"); return {z: 1}; }
  var intrinsicResult = Object.keys(intrinsicArg()).join("");
  restore(Object, "keys", intrinsicKeys);
  console.log("intrinsic-get:" + intrinsicOrder.join("|") + ":" + intrinsicResult);
"#;

const EXPECTED: &[&str] = &[
    "shadow:OOOOOOOOOOO|PPPPPP|AAA|S|NNNN",
    "global:OPASN",
    "replaced:oooooooooooppppppaaasnnnn",
    "order:Og|Oa|Oc:true:7|Ig|Ia|Ic:true:7|Fg|Fa|Fc:true:7|Ng|Na|Nc:true:7|Pg|Pa|Pc:true:7",
    "captured:truetruetruetruetruetruetrue",
    "spread:OPASN:get|args|call:true:3:4:ok:true",
    "intrinsic-get:get|arg:z",
];

#[test]
fn static_call_semantics_child() {
    if std::env::var_os("ZIPP_STATIC_CALL_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(SEMANTICS), EXPECTED);
}

const HOT: &str = r#"
  function nums(n) {
    var c = 0;
    for (var i = 0; i < n; i++) if (Number.isInteger(i)) c++;
    return c;
  }
  function arrays(n) {
    var c = 0, a = [];
    for (var i = 0; i < n; i++) if (Array.isArray(a)) c++;
    return c;
  }
  function keys(n) {
    var c = 0, o = {x: 1};
    for (var i = 0; i < n; i++) c += Object.keys(o).length;
    return c;
  }
  function promises(n) {
    var c = 0;
    for (var i = 0; i < n; i++) if (Promise.resolve(i) instanceof Promise) c++;
    return c;
  }
  var n = 6000;
  var a = nums(n), b = arrays(n), c = keys(n), d = promises(n);
  var ni = Number.isInteger, ia = Array.isArray, ok = Object.keys, pr = Promise.resolve;
  Number.isInteger = function () { return false; };
  Array.isArray = function () { return false; };
  Object.keys = function () { return []; };
  Promise.resolve = function (v) { return v; };
  console.log([a, b, c, d, nums(n), arrays(n), keys(n), promises(n)].join("|"));
  Number.isInteger = ni; Array.isArray = ia; Object.keys = ok; Promise.resolve = pr;
"#;

#[test]
fn static_call_jit_guard_child() {
    if std::env::var_os("ZIPP_STATIC_CALL_JIT_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(HOT), ["6000|6000|6000|6000|0|0|0|0"]);
}

#[test]
fn static_call_modes_match() {
    if std::env::var_os("ZIPP_STATIC_CALL_CHILD").is_some()
        || std::env::var_os("ZIPP_STATIC_CALL_JIT_CHILD").is_some()
    {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (test, marker) in [
        ("static_call_semantics_child", "ZIPP_STATIC_CALL_CHILD"),
        ("static_call_jit_guard_child", "ZIPP_STATIC_CALL_JIT_CHILD"),
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
