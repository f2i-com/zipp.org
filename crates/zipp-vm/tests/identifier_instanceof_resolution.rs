//! Identifier value-properties and `instanceof` are resolved from live
//! references before any intrinsic shortcut is considered.

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
  function params(undefined, NaN, Infinity) {
    return [undefined, NaN, Infinity,
            typeof undefined, typeof NaN, typeof Infinity].join("|");
  }
  console.log("params:" + params("u", "n", "i"));

  function capture(undefined, NaN, Infinity) {
    return function () { return [undefined, NaN, Infinity].join("|"); };
  }
  console.log("capture:" + capture("cu", "cn", "ci")());

  function referenceError(thunk) {
    try { thunk(); return false; }
    catch (e) { return e.name === "ReferenceError"; }
  }
  var tdz = [
    referenceError(function () { { var x = typeof undefined; let undefined = 1; } }),
    referenceError(function () { { var x = typeof NaN; let NaN = 1; } }),
    referenceError(function () { { var x = typeof Infinity; let Infinity = 1; } }),
    referenceError(function (undefined = undefined) {}),
    referenceError(function (NaN = NaN) {}),
    referenceError(function (Infinity = Infinity) {})
  ];
  console.log("tdz:" + tdz.join("|"));

  var withObject = {undefined: "wu", NaN: "wn", Infinity: "wi"};
  var withClosure, withValues, withDeletes;
  with (withObject) {
    withClosure = function () {
      return [undefined, NaN, Infinity,
              typeof undefined, typeof NaN, typeof Infinity].join("|");
    };
    withValues = [undefined, NaN, Infinity].join("|") + ":" + withClosure();
    withDeletes = [delete undefined, delete NaN, delete Infinity].join("|");
  }
  console.log("with:" + withValues + ":" + withDeletes + ":" +
              ["undefined" in withObject, "NaN" in withObject,
               "Infinity" in withObject].join("|"));

  var withFallback;
  with ({}) {
    withFallback = [String(undefined), String(NaN), String(Infinity),
                    typeof undefined, typeof NaN, typeof Infinity].join("|");
  }
  console.log("with-fallback:" + withFallback);

  var hidden = {undefined: "keep"};
  hidden[Symbol.unscopables] = {undefined: true};
  var hiddenDelete;
  with (hidden) { hiddenDelete = delete undefined; }
  console.log("unscopables:" + hiddenDelete + ":" + hidden.undefined);

  function dynamicEval() {
    eval("var undefined='eu', NaN='en', Infinity='ei'");
    var before = [undefined, NaN, Infinity,
                  typeof undefined, typeof NaN, typeof Infinity].join("|");
    var deleted = [delete undefined, delete NaN, delete Infinity].join("|");
    var after = [String(undefined), String(NaN), String(Infinity)].join("|");
    return before + ":" + deleted + ":" + after;
  }
  console.log("eval:" + dynamicEval());

  function deleteLocals(undefined, NaN, Infinity) {
    return [delete undefined, delete NaN, delete Infinity].join("|");
  }
  console.log("delete:" + deleteLocals(1, 2, 3) + ":" +
              [delete undefined, delete NaN, delete Infinity].join("|"));

  var order = [];
  function CustomCtor() {}
  Object.defineProperty(CustomCtor, Symbol.hasInstance, {
    configurable: true,
    value: function (value) { order.push("has"); return value.token === 1; }
  });
  function shadow(Array) {
    return (order.push("lhs"), {token: 1}) instanceof Array;
  }
  console.log("instance-shadow:" + shadow(CustomCtor) + ":" + order.join("|"));

  order = [];
  var holder = {};
  Object.defineProperty(holder, "Array", {
    configurable: true,
    get: function () { order.push("rhs"); return CustomCtor; }
  });
  var withInstance;
  with (holder) {
    withInstance = (order.push("lhs"), {token: 1}) instanceof Array;
  }
  console.log("instance-order:" + withInstance + ":" + order.join("|"));

  console.log("intrinsics:" + [
    [] instanceof Array,
    ({}) instanceof Object,
    (function () {}) instanceof Function,
    new TypeError("x") instanceof Error,
    new TypeError("x") instanceof TypeError
  ].join("|"));

  var IntrinsicArray = Array;
  function FakeArray() {}
  var fakeValue = new FakeArray();
  Array = FakeArray;
  var rebound = [fakeValue instanceof Array, [] instanceof Array].join("|");
  Array = IntrinsicArray;
  console.log("rebound:" + rebound);

  function UserCtor() {}
  var oldValue = new UserCtor();
  UserCtor.prototype = {};
  var freshValue = new UserCtor();
  var prototypeResults = [oldValue instanceof UserCtor, freshValue instanceof UserCtor];
  var badPrototype;
  UserCtor.prototype = 1;
  try { freshValue instanceof UserCtor; badPrototype = "missed"; }
  catch (e) { badPrototype = e.name; }
  prototypeResults.push(badPrototype);
  console.log("prototype:" + prototypeResults.join("|"));

  var customObject = {};
  customObject[Symbol.hasInstance] = function (value) { return value === 7; };
  var borrowedDefault = {};
  Object.defineProperty(borrowedDefault, Symbol.hasInstance, {
    value: Function.prototype[Symbol.hasInstance]
  });
  var nonCallable = {};
  nonCallable[Symbol.hasInstance] = 1;
  function instanceError(rhs) {
    try { ({}) instanceof rhs; return "missed"; }
    catch (e) { return e.name; }
  }
  console.log("handlers:" + [7 instanceof customObject,
              1 instanceof borrowedDefault, instanceError({}),
              instanceError(1), instanceError(nonCallable)].join("|"));

  var ProxyCtor = function ProxyCtor() {};
  var proxyValue = new ProxyCtor();
  var proxyLog = [];
  var proxyCtor = new Proxy(ProxyCtor, {
    get: function (target, key, receiver) {
      if (key === Symbol.hasInstance) proxyLog.push("has");
      if (key === "prototype") proxyLog.push("prototype");
      return Reflect.get(target, key, receiver);
    }
  });
  console.log("proxy:" + (proxyValue instanceof proxyCtor) + ":" + proxyLog.join("|"));

  var lhsProxyLog = [];
  var lhsProxy = new Proxy([], {
    getPrototypeOf: function (target) {
      lhsProxyLog.push("getPrototypeOf");
      return Reflect.getPrototypeOf(target);
    }
  });
  console.log("proxy-lhs:" + (lhsProxy instanceof Array) + ":" + lhsProxyLog.join("|"));

  var relinkedError = new TypeError("x");
  Object.setPrototypeOf(relinkedError, null);
  console.log("relinked-error:" +
              [relinkedError instanceof TypeError,
               relinkedError instanceof Error].join("|"));

  class RelinkBase {}
  class RelinkSub extends RelinkBase {}
  var relinkedClass = new RelinkSub();
  var relinkedBefore = [relinkedClass instanceof RelinkSub,
                        relinkedClass instanceof RelinkBase].join("|");
  Object.setPrototypeOf(relinkedClass, null);
  console.log("relinked-class:" + relinkedBefore + ":" +
              [relinkedClass instanceof RelinkSub,
               relinkedClass instanceof RelinkBase].join("|"));

  var realm = $262.createRealm().global;
  realm.eval("function RealmCtor(){}; this.RealmCtor=RealmCtor; this.value=new RealmCtor");
  Array = realm.RealmCtor;
  var realmViaLiveName = realm.value instanceof Array;
  Array = IntrinsicArray;
  console.log("realm:" + [realm.value instanceof realm.RealmCtor,
              realmViaLiveName].join("|"));

  function HotCtor() {}
  var hotValue = new HotCtor();
  function hot(Array, value, count) {
    var matches = 0;
    for (var i = 0; i < count; i++) if (value instanceof Array) matches++;
    return matches;
  }
  console.log("hot:" + hot(HotCtor, hotValue, 3000));
"#;

const EXPECTED: &[&str] = &[
    "params:u|n|i|string|string|string",
    "capture:cu|cn|ci",
    "tdz:true|true|true|true|true|true",
    "with:wu|wn|wi:wu|wn|wi|string|string|string:true|true|true:false|false|false",
    "with-fallback:undefined|NaN|Infinity|undefined|number|number",
    "unscopables:false:keep",
    "eval:eu|en|ei|string|string|string:true|true|true:undefined|NaN|Infinity",
    "delete:false|false|false:false|false|false",
    "instance-shadow:true:lhs|has",
    "instance-order:true:lhs|rhs|has",
    "intrinsics:true|true|true|true|true",
    "rebound:true|false",
    "prototype:false|true|TypeError",
    "handlers:true|false|TypeError|TypeError|TypeError",
    "proxy:true:has|prototype",
    "proxy-lhs:true:getPrototypeOf",
    "relinked-error:false|false",
    "relinked-class:true|true:false|false",
    "realm:true|true",
    "hot:3000",
];

#[test]
fn identifier_instanceof_child() {
    if std::env::var_os("ZIPP_IDENTIFIER_INSTANCEOF_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(SEMANTICS), EXPECTED);
}

#[test]
fn identifier_instanceof_modes_match() {
    if std::env::var_os("ZIPP_IDENTIFIER_INSTANCEOF_CHILD").is_some() {
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
        cmd.args(["--exact", "identifier_instanceof_child", "--nocapture"])
            .env("ZIPP_IDENTIFIER_INSTANCEOF_CHILD", "1")
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
fn compiler_does_not_select_instanceof_from_identifier_spelling() {
    let text = zipp_vm::compile_to_text(
        "({}) instanceof Array; function f(Array, value) { return value instanceof Array; }",
        false,
    )
    .expect("source compiles");
    assert_eq!(text.matches("InstanceOfDyn {").count(), 2);
    assert!(!text.contains("\n            InstanceOf {"));
}
