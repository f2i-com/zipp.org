//! Bare builtin call/construct specialisations must be exact-identity guards.
//! Callee GetValue precedes every argument, every argument is evaluated, and a
//! replacement follows the ordinary Call/Construct path.

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
  function assert(v, label) { if (!v) throw new Error(label); }
  var trace = [];
  function mark(label, value) { trace.push(label); return value; }

  // GetValue(callee) is a snapshot, even when a local argument overwrites the
  // same register or spread iteration changes it.
  function first(a, b) { trace.push("first:" + a + ":" + b); return 11; }
  function wrong() { trace.push("WRONG"); return 99; }
  var f = first;
  assert(f((f = wrong, 1), mark("plain-extra", 2)) === 11, "plain capture");
  f = first;
  assert(f(...(f = wrong, [3, 4])) === 11, "spread capture");

  // Lexical shadows and their TDZ never become builtin operations.
  function shadow(Number, String, Boolean, parseInt, parseFloat, isNaN, isFinite, Array) {
    return [Number(), String(), Boolean(), parseInt(), parseFloat(), isNaN(), isFinite(), Array()].join("|");
  }
  function word(s) { return function () { return s; }; }
  assert(shadow(word("N"), word("S"), word("B"), word("PI"), word("PF"), word("NAN"), word("FIN"), word("A")) === "N|S|B|PI|PF|NAN|FIN|A", "lexical shadows");
  var tdzArg = false;
  try { { function capturesNumber() { return Number; } Number((tdzArg = true)); let Number = wrong; } } catch (e) {}
  assert(!tdzArg, "callee TDZ precedes args");

  // Each guarded GlobalFn miss ordinary-calls the captured replacement with
  // undefined this and all extra arguments.
  var SavedNumber = Number, SavedString = String, SavedBoolean = Boolean;
  var SavedParseInt = parseInt, SavedParseFloat = parseFloat;
  var SavedIsNaN = isNaN, SavedIsFinite = isFinite;
  function replacement(name) {
    return function () { "use strict"; return name + ":" + arguments.length + ":" + (this === undefined); };
  }
  Number = replacement("Number"); String = replacement("String"); Boolean = replacement("Boolean");
  parseInt = replacement("parseInt"); parseFloat = replacement("parseFloat");
  isNaN = replacement("isNaN"); isFinite = replacement("isFinite");
  assert(Number(1, 2, mark("number-extra", 3)) === "Number:3:true", "Number replacement");
  assert(String(1, 2, 3) === "String:3:true", "String replacement");
  assert(Boolean(1, 2, 3) === "Boolean:3:true", "Boolean replacement");
  assert(parseInt(1, 2, 3) === "parseInt:3:true", "parseInt replacement");
  assert(parseFloat(1, 2, 3) === "parseFloat:3:true", "parseFloat replacement");
  assert(isNaN(1, 2, 3) === "isNaN:3:true", "isNaN replacement");
  assert(isFinite(1, 2, 3) === "isFinite:3:true", "isFinite replacement");
  Number = SavedNumber; String = SavedString; Boolean = SavedBoolean;
  parseInt = SavedParseInt; parseFloat = SavedParseFloat;
  isNaN = SavedIsNaN; isFinite = SavedIsFinite;

  // Mutating the global during arguments cannot redirect an already-captured
  // intrinsic. Extra arguments still run even when the intrinsic ignores them.
  var intrinsicNumber = Number((Number = replacement("late"), "12"), mark("intrinsic-number-extra", 4));
  assert(intrinsicNumber === 12, "captured intrinsic Number");
  Number = SavedNumber;

  var SavedArray = Array;
  Array = replacement("ArrayCall");
  assert(Array((Array = SavedArray, 1), mark("array-call-extra", 2)) === "ArrayCall:2:true", "captured replacement Array call");
  var intrinsicArray = Array((Array = replacement("lateArray"), 1), mark("intrinsic-array-extra", 2));
  assert(intrinsicArray.length === 2 && intrinsicArray[1] === 2, "captured intrinsic Array call");
  Array = SavedArray;

  function FakeArray(a, b) { this.args = a + ":" + b; this.nt = new.target; }
  Array = FakeArray;
  var fakeArray = new Array((Array = SavedArray, 5), mark("array-new-extra", 6));
  assert(fakeArray instanceof FakeArray && fakeArray.args === "5:6" && fakeArray.nt === FakeArray, "Array construct fallback");
  Array = SavedArray;
  var proxyNewTarget;
  function ProxyTarget(a) { this.a = a; proxyNewTarget = new.target; }
  var ProxyArray = new Proxy(ProxyTarget, {});
  Array = ProxyArray;
  var proxyArray = new Array((Array = SavedArray, 7));
  assert(proxyArray.a === 7 && proxyNewTarget === ProxyArray, "proxy constructor/newTarget");
  Array = SavedArray;

  // Former low-frequency spelling shortcuts now use ordinary calls. Prove
  // replacement, `this`, and complete extras for every family.
  var SavedError = Error, SavedSymbol = Symbol, SavedRegExp = RegExp;
  var SavedBigInt = BigInt, SavedObject = Object, SavedPrint = print;
  Error = replacement("Error"); Symbol = replacement("Symbol"); RegExp = replacement("RegExp");
  BigInt = replacement("BigInt"); Object = replacement("Object"); print = replacement("print");
  assert(Error(1, 2, 3) === "Error:3:true", "Error replacement");
  assert(Symbol(1, 2, 3) === "Symbol:3:true", "Symbol replacement");
  assert(RegExp(1, 2, 3) === "RegExp:3:true", "RegExp replacement");
  assert(BigInt(1, 2, 3) === "BigInt:3:true", "BigInt replacement");
  assert(Object(1, 2, 3) === "Object:3:true", "Object replacement");
  assert(print(1, 2, 3) === "print:3:true", "print replacement");
  Error = SavedError; Symbol = SavedSymbol; RegExp = SavedRegExp;
  BigInt = SavedBigInt; Object = SavedObject; print = SavedPrint;

  // Intrinsics which consume fewer arguments must nevertheless evaluate all.
  Error("x", undefined, mark("Error-extra", 0));
  Symbol("x", mark("Symbol-extra", 0));
  RegExp("x", "", mark("RegExp-extra", 0));
  BigInt("1", mark("BigInt-extra", 0));
  Object(1, mark("Object-extra", 0));

  // Every former by-name `new` shortcut evaluates extras through generic New.
  new Error("x", undefined, mark("new-Error-extra", 0));
  new TypeError("x", undefined, mark("new-TypeError-extra", 0));
  new RangeError("x", undefined, mark("new-RangeError-extra", 0));
  new SyntaxError("x", undefined, mark("new-SyntaxError-extra", 0));
  new ReferenceError("x", undefined, mark("new-ReferenceError-extra", 0));
  new EvalError("x", undefined, mark("new-EvalError-extra", 0));
  new URIError("x", undefined, mark("new-URIError-extra", 0));
  new AggregateError([], "x", undefined, mark("new-AggregateError-extra", 0));
  new Object({}, mark("new-Object-extra", 0));
  new Promise(function (resolve) { resolve(1); }, mark("new-Promise-extra", 0));
  new RegExp("x", "", mark("new-RegExp-extra", 0));
  new Map([], mark("new-Map-extra", 0));
  new Set([], mark("new-Set-extra", 0));
  new WeakMap([], mark("new-WeakMap-extra", 0));
  new WeakSet([], mark("new-WeakSet-extra", 0));
  new String("x", mark("new-String-extra", 0));
  new Number(1, mark("new-Number-extra", 0));
  new Boolean(1, mark("new-Boolean-extra", 0));
  new WeakRef({}, mark("new-WeakRef-extra", 0));
  new FinalizationRegistry(function () {}, mark("new-FinalizationRegistry-extra", 0));
  new Date(0, mark("new-Date-extra", 0));

  assert(typeof String(1) === "string" && typeof new String(1) === "object", "call versus new String");
  assert(typeof Date() === "string" && typeof new Date() === "object", "call versus new Date");
  var promiseCallThrew = false; try { Promise(function () {}); } catch (e) { promiseCallThrew = e instanceof TypeError; }
  assert(promiseCallThrew, "Promise requires new");

  // A same-kind intrinsic from a child realm is not the main intrinsic. Generic
  // call/construct must preserve that realm's results and thrown errors.
  var child = $262.createRealm().global;
  Number = child.Number;
  var childTypeError = false;
  try { Number(Symbol("x")); } catch (e) { childTypeError = Object.getPrototypeOf(e) === child.TypeError.prototype; }
  assert(childTypeError, "foreign Number error realm");
  Number = SavedNumber;
  Array = child.Array;
  var childCallArray = Array(1, 2);
  var childNewArray = new Array(3, 4);
  assert(Object.getPrototypeOf(childCallArray) === child.Array.prototype, "foreign Array call realm");
  assert(Object.getPrototypeOf(childNewArray) === child.Array.prototype, "foreign Array new realm");
  Array = SavedArray;

  // The real host print remains a first-class callable, including extra args.
  print("bare-print", 8);
  console.log("trace:" + trace.join("|"));
  console.log("bare-builtin-guards:ok");
"#;

const HOT: &str = r#"
  function assert(v, label) { if (!v) throw new Error(label); }
  function hotString(v) { return String(v); }
  function hotArray(v) { return Array(v).length; }
  function hotNewArray(v) { return (new Array(v)).length; }
  for (var i = 0; i < 7000; i++) {
    hotString(i);
    hotArray(2);
    hotNewArray(2);
  }
  var SavedString = String, SavedArray = Array;
  String = function () { return "replaced"; };
  Array = function () { return {length: 41}; };
  assert(hotString(1) === "replaced", "JIT String identity guard");
  assert(hotArray(1) === 41, "JIT Array call identity guard");
  Array = function () { this.length = 42; };
  assert(hotNewArray(1) === 42, "JIT Array new identity guard");
  String = SavedString; Array = SavedArray;
  console.log("bare-builtin-hot:ok");
"#;

const HOT_NONCALLABLE: &str = r#"
  function assert(v, label) { if (!v) throw new Error(label); }
  var effects = 0;
  function effect(v) { effects++; return v; }
  function callNumber() { return Number(effect(1)); }
  function callString() { return String(effect(1)); }
  function callBoolean() { return Boolean(effect(1)); }
  function callParseInt() { return parseInt(effect("10"), effect(10)); }
  function callParseFloat() { return parseFloat(effect("1.5")); }
  function callIsNaN() { return isNaN(effect(1)); }
  function callIsFinite() { return isFinite(effect(1)); }
  function callArray() { return Array(effect(2)); }
  function newArray() { return new Array(effect(2)); }
  var calls = [callNumber, callString, callBoolean, callParseInt, callParseFloat,
               callIsNaN, callIsFinite, callArray, newArray];
  var saved = [Number, String, Boolean, parseInt, parseFloat, isNaN, isFinite, Array, Array];
  var setters = [
    function (v) { Number = v; }, function (v) { String = v; },
    function (v) { Boolean = v; }, function (v) { parseInt = v; },
    function (v) { parseFloat = v; }, function (v) { isNaN = v; },
    function (v) { isFinite = v; }, function (v) { Array = v; },
    function (v) { Array = v; }
  ];
  for (var warm = 0; warm < 7000; warm++) {
    for (var wi = 0; wi < calls.length; wi++) calls[wi]();
  }
  effects = 0;
  var bad = [undefined, null, 17];
  for (var i = 0; i < calls.length; i++) {
    var argEffects = i === 3 ? 2 : 1;
    for (var j = 0; j < bad.length; j++) {
      setters[i](bad[j]);
      var before = effects, threw = false;
      try { calls[i](); } catch (e) { threw = e instanceof TypeError; }
      assert(threw, "noncallable guard " + i + "/" + j);
      assert(effects === before + argEffects, "arguments before TypeError " + i + "/" + j);
      setters[i](saved[i]);
    }
  }
  console.log("bare-builtin-noncallable:ok");
"#;

#[test]
fn bare_builtin_semantics_child() {
    if std::env::var_os("ZIPP_BARE_BUILTIN_CHILD").is_none() {
        return;
    }
    let out = run_ok(SEMANTICS);
    assert_eq!(
        out.last().map(String::as_str),
        Some("bare-builtin-guards:ok")
    );
    assert!(out.iter().any(|line| line == "bare-print 8"));
}

#[test]
fn bare_builtin_hot_child() {
    if std::env::var_os("ZIPP_BARE_BUILTIN_HOT_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(HOT), ["bare-builtin-hot:ok"]);
}

#[test]
fn bare_builtin_noncallable_child() {
    if std::env::var_os("ZIPP_BARE_BUILTIN_NONCALLABLE_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(HOT_NONCALLABLE), ["bare-builtin-noncallable:ok"]);
}

#[test]
fn bare_builtin_guards_modes_match() {
    if std::env::var_os("ZIPP_BARE_BUILTIN_CHILD").is_some()
        || std::env::var_os("ZIPP_BARE_BUILTIN_HOT_CHILD").is_some()
        || std::env::var_os("ZIPP_BARE_BUILTIN_NONCALLABLE_CHILD").is_some()
    {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (test, marker) in [
        ("bare_builtin_semantics_child", "ZIPP_BARE_BUILTIN_CHILD"),
        ("bare_builtin_hot_child", "ZIPP_BARE_BUILTIN_HOT_CHILD"),
        (
            "bare_builtin_noncallable_child",
            "ZIPP_BARE_BUILTIN_NONCALLABLE_CHILD",
        ),
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
