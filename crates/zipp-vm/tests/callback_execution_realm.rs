//! A native's realm is ambient only while that native executes.  User JS
//! callbacks install their own execution realm and restore the native/caller
//! context on every kind of return.

use std::process::Command;

const CHILD_ENV: &str = "ZIPP_CALLBACK_EXECUTION_REALM_CHILD";

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}; output: {:?}",
        out.error,
        out.output
    );
    out.output
}

const SOURCE: &str = r#"
    "use strict";
    // Before createRealm, realm_global_objs/nonempty_raw is still the fast
    // empty state.  Every case below runs after insertion flipped it nonempty.
    console.log(
      "before-realm:" +
      Array.from({0: 6, length: 1}, function (value) { return value + 1; })[0]
    );
    var realm = $262.createRealm();
    var foreign = realm.global;
    var mainArrayProto = Array.prototype;
    var childArrayProto = foreign.Array.prototype;
    var mainPromiseProto = Promise.prototype;
    var childPromiseProto = foreign.Promise.prototype;

    // Capture both identities lexically: the test remains diagnostic even when
    // the callback's bare `Array` / `globalThis` resolution is the thing broken.
    var main = (function (mainArray, childArray, mainPromise, childPromise) {
      function arrayRealm(value) {
        var proto = Object.getPrototypeOf(value);
        return proto === mainArray ? "M" : proto === childArray ? "C" : "?";
      }
      function promiseRealm(value) {
        var proto = Object.getPrototypeOf(value);
        return proto === mainPromise ? "M" : proto === childPromise ? "C" : "?";
      }
      var asyncSeen = "unset";
      return {
        arrayRealm: arrayRealm,
        promiseRealm: promiseRealm,
        normal: function () {
          return arrayRealm(Object.keys({ x: 1 })) +
                 arrayRealm(JSON.parse("[1]"));
        },
        throwing: function () { throw new TypeError("main"); },
        generator: function* (value = Object.keys({ x: 1 })) { yield value; },
        asyncFn: async function (value = Object.keys({ x: 1 })) {
          asyncSeen = arrayRealm(value);
          return 1;
        },
        asyncGen: async function* () { yield 1; },
        asyncSeen: function () { return asyncSeen; }
      };
    })(mainArrayProto, childArrayProto, mainPromiseProto, childPromiseProto);

    // The factory itself is a child-realm callable.  Its returned callbacks
    // exercise Closure rather than just Func realm metadata, and retain a
    // lexical value across repeated cross-realm entries.
    var childFactory = realm.evalScript(
      "(function (mainArray, childArray) {" +
      " function arrayRealm(value) {" +
      "   var proto = Object.getPrototypeOf(value);" +
      "   return proto === mainArray ? 'M' : proto === childArray ? 'C' : '?';" +
      " }" +
      " var captured = 'captured';" +
      " return {" +
      "   normal: function () {" +
      "     return arrayRealm(Object.keys({x:1})) +" +
      "            arrayRealm(JSON.parse('[1]')) + captured;" +
      "   }," +
      "   throwing: function () { throw new TypeError('child'); }" +
      " };" +
      "})"
    );
    var child = childFactory(mainArrayProto, childArrayProto);
    var arrayLike = { 0: 1, length: 1 };
    var undefinedLike = { 0: undefined, length: 1 };

    // The realm Array facade is callable as well as constructable.  Direct,
    // detached/.call, and transplanted bare-Array calls all retain the facade as
    // their effective newTarget; the one-number length case remains Array's
    // call/construct semantics rather than becoming a one-element array.
    var directArrayCall = foreign.Array(1, 2);
    var ArrayAlias = foreign.Array;
    var detachedArrayCall = ArrayAlias(3);
    var originalArray = Array;
    Array = foreign.Array;
    var transplantedArrayCall = Array("x", "y");
    Array = originalArray;
    var viaCallArray = foreign.Array.call(null, 4, 5);
    var constructedArray = new foreign.Array(6, 7);
    var mainArray = Array(8, 9);
    console.log(
      "array-call-facade:" +
      [main.arrayRealm(directArrayCall),
       directArrayCall.constructor === foreign.Array,
       directArrayCall.length,
       directArrayCall[0],
       directArrayCall[1],
       main.arrayRealm(detachedArrayCall),
       detachedArrayCall.length,
       0 in detachedArrayCall,
       main.arrayRealm(transplantedArrayCall),
       transplantedArrayCall.constructor === foreign.Array,
       transplantedArrayCall.join(","),
       main.arrayRealm(viaCallArray),
       viaCallArray.join(","),
       main.arrayRealm(constructedArray),
       constructedArray.join(","),
       main.arrayRealm(mainArray)].join("|")
    );

    // A child native stays child-realm before/after its main-realm callback;
    // Object.keys and JSON.parse inside that callback must stay main-realm.
    var direct = foreign.Array.from(arrayLike, main.normal);
    console.log("main-via-child:" + main.arrayRealm(direct) + "|" + direct[0]);

    // Same child native transplanted onto an ordinary main-realm holder.  The
    // holder is not a constructor, so Array.from's default result is still made
    // in the native function's child realm.
    var holder = { from: foreign.Array.from };
    var transplanted = holder.from(arrayLike, main.normal);
    console.log(
      "main-via-transplanted:" +
      main.arrayRealm(transplanted) + "|" + transplanted[0]
    );

    // active_realm belongs to evalScript while its body runs, but a main-realm
    // callback temporarily suspends it rather than inheriting the child realm.
    foreign.mainNormal = main.normal;
    console.log(
      "main-via-child-eval:" +
      realm.evalScript("Array.from({0:1,length:1}, mainNormal)[0]")
    );

    console.log("child-via-main:" + Array.from(arrayLike, child.normal)[0]);
    console.log("child-direct:" + child.normal());
    var repeated = true;
    for (var i = 0; i < 48; i++) {
      if (Array.from(arrayLike, child.normal)[0] !== "CCcaptured") repeated = false;
    }
    console.log("child-repeated:" + repeated);

    // A thrown callback keeps its own error constructors.  After unwinding,
    // the caller's realm is restored in both directions.
    var mainError;
    try { foreign.Array.from(arrayLike, main.throwing); }
    catch (error) { mainError = error; }
    console.log(
      "main-throw:" +
      [mainError.constructor === TypeError,
       mainError.constructor === foreign.TypeError].join("|")
    );

    var childError;
    try { child.throwing(); }
    catch (error) { childError = error; }
    var afterChildThrow = Object.keys({ x: 1 });
    console.log(
      "child-throw:" +
      [childError.constructor === foreign.TypeError,
       childError.constructor === TypeError,
       main.arrayRealm(afterChildThrow)].join("|")
    );

    foreign.mainThrow = main.throwing;
    foreign.mainArrayRealm = main.arrayRealm;
    foreign.mainTypeError = TypeError;
    console.log(
      "throw-restores-child-eval:" +
      realm.evalScript(
        "var er, ar;" +
        "try { mainThrow(); } catch (e) { er = e.constructor === mainTypeError; }" +
        "ar = mainArrayRealm(Object.keys({x:1}));" +
        "[er, ar].join('|')"
      )
    );

    // call_value_plain has early returns for generator, async, and async
    // generator functions.  The child native must regain its context after all
    // three; generator/async parameter work itself runs in the main callee.
    var generatorOuter = foreign.Array.from(undefinedLike, main.generator);
    var generatorValue = generatorOuter[0].next().value;
    var asyncOuter = foreign.Array.from(undefinedLike, main.asyncFn);
    var asyncGeneratorOuter = foreign.Array.from(arrayLike, main.asyncGen);
    console.log(
      "early-returns:" +
      [main.arrayRealm(generatorOuter),
       main.arrayRealm(generatorValue),
       main.arrayRealm(asyncOuter),
       main.promiseRealm(asyncOuter[0]),
       main.asyncSeen(),
       main.arrayRealm(asyncGeneratorOuter)].join("|")
    );
"#;

#[test]
fn callback_execution_realm_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    assert_eq!(
        run_ok(SOURCE),
        [
            "before-realm:7",
            "array-call-facade:C|true|2|1|2|C|3|false|C|true|x,y|C|4,5|C|6,7|M",
            "main-via-child:C|MM",
            "main-via-transplanted:C|MM",
            "main-via-child-eval:MM",
            "child-via-main:CCcaptured",
            "child-direct:CCcaptured",
            "child-repeated:true",
            "main-throw:true|false",
            "child-throw:true|false|M",
            "throw-restores-child-eval:true|C",
            "early-returns:C|M|C|M|M|C",
        ]
    );
}

#[test]
fn js_callbacks_install_and_restore_their_own_realm_in_all_modes() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", &[][..]),
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
        ("forced-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc-stress", &[("ZIPP_GC_STRESS", "1")][..]),
        (
            "interpreter-gc-stress",
            &[("ZIPP_NOJIT", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let mut cmd = Command::new(&exe);
        cmd.args(["--exact", "callback_execution_realm_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        cmd.envs(env.iter().copied());
        let out = cmd.output().expect("spawn execution-mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
