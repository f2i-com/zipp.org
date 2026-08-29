//! Reflection producers allocate their result containers in the realm of the
//! built-in function, including when a child-realm native is called from main.

use std::process::Command;

const CHILD_ENV: &str = "ZIPP_REFLECTION_RESULT_REALM_CHILD";

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
    var foreign = $262.createRealm().global;
    var symbol = Symbol("symbol-key");
    var source = { plain: 1 };
    Object.defineProperty(source, "hidden", {
      value: 2, writable: false, enumerable: false, configurable: true
    });
    Object.defineProperty(source, "access", {
      get: function () { return 4; },
      set: function (_) {},
      enumerable: true,
      configurable: true
    });
    source[symbol] = 3;

    function isForeignArray(value) {
      return Object.getPrototypeOf(value) === foreign.Array.prototype &&
             Object.getPrototypeOf(value) !== Array.prototype;
    }
    function isForeignObject(value) {
      return Object.getPrototypeOf(value) === foreign.Object.prototype &&
             Object.getPrototypeOf(value) !== Object.prototype;
    }
    function throwsForeignTypeError(fn) {
      try {
        fn();
        return false;
      } catch (error) {
        return error.constructor === foreign.TypeError &&
               error.constructor !== TypeError;
      }
    }

    var proxy = new Proxy(source, {
      ownKeys: function (target) { return Reflect.ownKeys(target); },
      getOwnPropertyDescriptor: function (target, key) {
        return Object.getOwnPropertyDescriptor(target, key);
      }
    });
    var directKeys = foreign.Reflect.ownKeys(source);
    var directProxyKeys = foreign.Reflect.ownKeys(proxy);
    var directSymbols = foreign.Object.getOwnPropertySymbols(source);
    var directDescriptors = foreign.Object.getOwnPropertyDescriptors(proxy);
    console.log("direct:" + [
      isForeignArray(directKeys),
      isForeignArray(directProxyKeys),
      isForeignArray(directSymbols),
      isForeignObject(directDescriptors),
      isForeignObject(directDescriptors.plain),
      isForeignObject(directDescriptors.hidden),
      isForeignObject(directDescriptors.access),
      isForeignObject(directDescriptors[symbol]),
      throwsForeignTypeError(function () { foreign.Reflect.ownKeys(1); }),
      throwsForeignTypeError(function () { foreign.Object.getOwnPropertySymbols(null); }),
      throwsForeignTypeError(function () { foreign.Object.getOwnPropertyDescriptors(null); })
    ].join("|"));

    var savedOwnKeys = Reflect.ownKeys;
    var savedSymbols = Object.getOwnPropertySymbols;
    var savedDescriptors = Object.getOwnPropertyDescriptors;
    Reflect.ownKeys = foreign.Reflect.ownKeys;
    Object.getOwnPropertySymbols = foreign.Object.getOwnPropertySymbols;
    Object.getOwnPropertyDescriptors = foreign.Object.getOwnPropertyDescriptors;

    var transplantedKeys = Reflect.ownKeys(source);
    var transplantedSymbols = Object.getOwnPropertySymbols(source);
    var transplantedDescriptors = Object.getOwnPropertyDescriptors(source);
    var transplantedError = throwsForeignTypeError(function () { Reflect.ownKeys(1); });

    Reflect.ownKeys = savedOwnKeys;
    Object.getOwnPropertySymbols = savedSymbols;
    Object.getOwnPropertyDescriptors = savedDescriptors;

    var detachedOwnKeys = foreign.Reflect.ownKeys;
    var detachedKeys = detachedOwnKeys(source);
    console.log("transplanted:" + [
      isForeignArray(transplantedKeys),
      isForeignArray(transplantedSymbols),
      isForeignObject(transplantedDescriptors),
      isForeignObject(transplantedDescriptors.plain),
      isForeignObject(transplantedDescriptors.access),
      isForeignObject(transplantedDescriptors[symbol]),
      transplantedError,
      isForeignArray(detachedKeys)
    ].join("|"));
"#;

#[test]
fn reflection_result_realm_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    assert_eq!(
        run_ok(SOURCE),
        [
            "direct:true|true|true|true|true|true|true|true|true|true|true",
            "transplanted:true|true|true|true|true|true|true|true",
        ]
    );
}

#[test]
fn reflection_results_and_errors_follow_the_builtin_realm_in_all_modes() {
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
        cmd.args(["--exact", "reflection_result_realm_child", "--nocapture"])
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
