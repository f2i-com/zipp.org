//! Externally observable native result containers belong to the built-in
//! function's [[Realm]], including detached/transplanted foreign functions.
//! Constructors explicitly supplied by the caller (Array.of / withResolvers)
//! still control the realm of the instance/functions they construct.

use std::process::Command;

const CHILD_ENV: &str = "ZIPP_NATIVE_RESULT_REALM_CHILD";

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

    function isForeignArray(value) {
      return Object.getPrototypeOf(value) === foreign.Array.prototype &&
             Object.getPrototypeOf(value) !== Array.prototype;
    }
    function isMainArray(value) {
      return Object.getPrototypeOf(value) === Array.prototype;
    }
    function isForeignObject(value) {
      return Object.getPrototypeOf(value) === foreign.Object.prototype &&
             Object.getPrototypeOf(value) !== Object.prototype;
    }
    function isMainObject(value) {
      return Object.getPrototypeOf(value) === Object.prototype;
    }
    function isForeignFunction(value) {
      return Object.getPrototypeOf(value) === foreign.Function.prototype &&
             Object.getPrototypeOf(value) !== Function.prototype;
    }
    function isMainFunction(value) {
      return Object.getPrototypeOf(value) === Function.prototype;
    }
    function isForeignMap(value) {
      return Object.getPrototypeOf(value) === foreign.Map.prototype &&
             Object.getPrototypeOf(value) !== Map.prototype;
    }
    function isMainMap(value) {
      return Object.getPrototypeOf(value) === Map.prototype;
    }

    var foreignArrayOf = foreign.Array.of;
    var grouped = foreign.Object.groupBy([1, 2], function () { return "x"; });
    var mapGrouped = foreign.Map.groupBy([1, 2], function () { return "x"; });
    var detachedObjectGroupBy = foreign.Object.groupBy;
    var detachedMapGroupBy = foreign.Map.groupBy;
    var detachedGrouped = detachedObjectGroupBy([3], function () { return "y"; });
    var detachedMapGrouped = detachedMapGroupBy([4], function () { return "y"; });
    var mainGrouped = Object.groupBy(foreign.Array.of(1), function () { return "x"; });
    var mainMapGrouped = Map.groupBy(foreign.Array.of(1), function () { return "x"; });
    console.log("collections:" + [
      isForeignArray(foreign.Array.of(1)),
      isForeignArray(foreignArrayOf(1)),
      Object.getPrototypeOf(grouped) === null,
      isForeignArray(grouped.x),
      isForeignMap(mapGrouped),
      isForeignArray(mapGrouped.get("x")),
      Object.getPrototypeOf(detachedGrouped) === null,
      isForeignArray(detachedGrouped.y),
      isForeignMap(detachedMapGrouped),
      isForeignArray(detachedMapGrouped.get("y")),
      isMainArray(mainGrouped.x),
      isMainMap(mainMapGrouped),
      isMainArray(mainMapGrouped.get("x"))
    ].join("|"));

    var directCapability = foreign.Promise.withResolvers();
    var foreignWithResolvers = foreign.Promise.withResolvers;
    var foreignOuterMainConstructor = foreignWithResolvers.call(Promise);
    var mainOuterForeignConstructor = Promise.withResolvers.call(foreign.Promise);
    function injectedResolve() {}
    function injectedReject() {}
    foreign.injectedResolve = injectedResolve;
    foreign.injectedReject = injectedReject;
    var ForeignCustom = foreign.eval(
      "(function ForeignCustom(executor) {" +
      " executor(injectedResolve, injectedReject);" +
      "})"
    );
    var customCapability = foreignWithResolvers.call(ForeignCustom);
    var directRevocable = foreign.Proxy.revocable({}, {});
    var detachedRevocable = foreign.Proxy.revocable;
    var detachedRecord = detachedRevocable({}, {});
    console.log("records:" + [
      isForeignObject(directCapability),
      Object.getPrototypeOf(directCapability.promise) === foreign.Promise.prototype,
      isForeignFunction(directCapability.resolve),
      isForeignFunction(directCapability.reject),
      isForeignObject(foreignOuterMainConstructor),
      Object.getPrototypeOf(foreignOuterMainConstructor.promise) === Promise.prototype,
      isMainFunction(foreignOuterMainConstructor.resolve),
      isMainFunction(foreignOuterMainConstructor.reject),
      isMainObject(mainOuterForeignConstructor),
      Object.getPrototypeOf(mainOuterForeignConstructor.promise) === foreign.Promise.prototype,
      isForeignFunction(mainOuterForeignConstructor.resolve),
      isForeignFunction(mainOuterForeignConstructor.reject),
      isForeignObject(customCapability),
      customCapability.resolve === injectedResolve,
      customCapability.reject === injectedReject,
      isMainFunction(customCapability.resolve),
      isMainFunction(customCapability.reject),
      isForeignObject(directRevocable),
      isForeignFunction(directRevocable.revoke),
      isForeignObject(detachedRecord),
      isForeignFunction(detachedRecord.revoke)
    ].join("|"));

    var getCanonicalLocales = foreign.Intl.getCanonicalLocales;
    var supportedValuesOf = foreign.Intl.supportedValuesOf;
    console.log("intl:" + [
      isForeignArray(foreign.Intl.getCanonicalLocales(["en"])),
      isForeignArray(getCanonicalLocales(["en"])),
      isForeignArray(foreign.Intl.supportedValuesOf("calendar")),
      isForeignArray(supportedValuesOf("calendar"))
    ].join("|"));

    var mainArrayIterProto = Object.getPrototypeOf([].entries());
    var foreignArrayIter = foreign.Array.prototype.entries.call([7]);
    var foreignArrayIterProto = Object.getPrototypeOf(foreignArrayIter);
    var foreignArrayStep = foreignArrayIter.next();
    var foreignArrayNext = foreignArrayIterProto.next;
    var detachedArrayStep = foreignArrayNext.call([8].entries());
    var mainArrayStep = Array.prototype.entries.call(foreign.Array.of(9)).next();

    var mainMapIterProto = Object.getPrototypeOf(new Map().entries());
    var foreignMapIter = foreign.Map.prototype.entries.call(new Map([["x", 1]]));
    var foreignMapIterProto = Object.getPrototypeOf(foreignMapIter);
    var foreignMapStep = foreignMapIter.next();
    var foreignMapNext = foreignMapIterProto.next;
    var detachedMapStep = foreignMapNext.call(new Map([["y", 2]]).entries());
    var mainMapStep = Map.prototype.entries.call(
      new foreign.Map([["z", 3]])
    ).next();

    var mainTypedArrayIterProto = Object.getPrototypeOf(new Uint8Array().entries());
    var foreignTypedArrayIter = foreign.Uint8Array.prototype.entries.call(
      new Uint8Array([10])
    );
    var foreignTypedArrayIterProto = Object.getPrototypeOf(foreignTypedArrayIter);
    var foreignTypedArrayStep = foreignTypedArrayIter.next();
    var foreignTypedArrayNext = foreignTypedArrayIterProto.next;
    var detachedTypedArrayStep = foreignTypedArrayNext.call(
      new Uint8Array([11]).entries()
    );
    var mainTypedArrayStep = mainTypedArrayIterProto.next.call(
      foreign.Uint8Array.prototype.entries.call(new Uint8Array([12]))
    );
    console.log("iterators:" + [
      foreignArrayIterProto !== mainArrayIterProto,
      Object.getPrototypeOf(foreignArrayIterProto) === foreign.Iterator.prototype,
      isForeignObject(foreignArrayStep),
      isForeignArray(foreignArrayStep.value),
      isForeignObject(detachedArrayStep),
      isForeignArray(detachedArrayStep.value),
      isMainObject(mainArrayStep),
      isMainArray(mainArrayStep.value),
      foreignMapIterProto !== mainMapIterProto,
      Object.getPrototypeOf(foreignMapIterProto) === foreign.Iterator.prototype,
      isForeignObject(foreignMapStep),
      isForeignArray(foreignMapStep.value),
      isForeignObject(detachedMapStep),
      isForeignArray(detachedMapStep.value),
      isMainObject(mainMapStep),
      isMainArray(mainMapStep.value),
      foreignTypedArrayIterProto !== mainTypedArrayIterProto,
      isForeignObject(foreignTypedArrayStep),
      isForeignArray(foreignTypedArrayStep.value),
      isForeignObject(detachedTypedArrayStep),
      isForeignArray(detachedTypedArrayStep.value),
      isMainObject(mainTypedArrayStep),
      isMainArray(mainTypedArrayStep.value)
    ].join("|"));

    var mainSetIterProto = Object.getPrototypeOf(new Set().entries());
    var foreignSetIter = foreign.Set.prototype.entries.call(new Set([4]));
    var foreignSetIterProto = Object.getPrototypeOf(foreignSetIter);
    var foreignSetStep = foreignSetIter.next();
    var foreignSetNext = foreignSetIterProto.next;
    var detachedSetStep = foreignSetNext.call(new Set([5]).entries());
    var mainSetStep = Set.prototype.entries.call(new foreign.Set([6])).next();

    var mainStringIterProto = Object.getPrototypeOf(""[Symbol.iterator]());
    var foreignStringIterator = foreign.String.prototype[Symbol.iterator];
    var foreignStringIter = foreignStringIterator.call("a");
    var foreignStringIterProto = Object.getPrototypeOf(foreignStringIter);
    var foreignStringStep = foreignStringIter.next();
    var foreignStringNext = foreignStringIterProto.next;
    var detachedStringStep = foreignStringNext.call("b"[Symbol.iterator]());
    var mainStringStep = mainStringIterProto.next.call(foreignStringIterator.call("c"));

    var mainMatchAll = RegExp.prototype[Symbol.matchAll];
    var mainRegExpIterProto = Object.getPrototypeOf(mainMatchAll.call(/a/g, "a"));
    var foreignMatchAll = foreign.RegExp.prototype[Symbol.matchAll];
    var foreignRegExpIter = foreignMatchAll.call(/a/g, "a");
    var foreignRegExpIterProto = Object.getPrototypeOf(foreignRegExpIter);
    var foreignRegExpStep = foreignRegExpIter.next();
    var foreignRegExpNext = foreignRegExpIterProto.next;
    var detachedRegExpStep = foreignRegExpNext.call(mainMatchAll.call(/a/g, "a"));
    var mainRegExpStep = mainRegExpIterProto.next.call(
      foreignMatchAll.call(/a/g, "a")
    );
    console.log("more-iterators:" + [
      foreignSetIterProto !== mainSetIterProto,
      Object.getPrototypeOf(foreignSetIterProto) === foreign.Iterator.prototype,
      isForeignObject(foreignSetStep),
      isForeignArray(foreignSetStep.value),
      isForeignObject(detachedSetStep),
      isForeignArray(detachedSetStep.value),
      isMainObject(mainSetStep),
      isMainArray(mainSetStep.value),
      foreignStringIterProto !== mainStringIterProto,
      Object.getPrototypeOf(foreignStringIterProto) === foreign.Iterator.prototype,
      isForeignObject(foreignStringStep),
      isForeignObject(detachedStringStep),
      isMainObject(mainStringStep),
      foreignRegExpIterProto !== mainRegExpIterProto,
      Object.getPrototypeOf(foreignRegExpIterProto) === foreign.Iterator.prototype,
      isForeignObject(foreignRegExpStep),
      isForeignArray(foreignRegExpStep.value),
      isForeignObject(detachedRegExpStep),
      isForeignArray(detachedRegExpStep.value),
      isMainObject(mainRegExpStep),
      isMainArray(mainRegExpStep.value)
    ].join("|"));

    var foreignExec = foreign.RegExp.prototype.exec;
    var directMatch = foreignExec.call(/(?<word>a)/d, "a");
    var detachedMatch = foreignExec.call(/(?<word>a)/d, "a");
    var mainMatch = RegExp.prototype.exec.call(
      new foreign.RegExp("(?<word>a)", "d"),
      "a"
    );
    console.log("regexp:" + [
      isForeignArray(directMatch),
      Object.getPrototypeOf(directMatch.groups) === null,
      isForeignArray(directMatch.indices),
      isForeignArray(directMatch.indices[0]),
      Object.getPrototypeOf(directMatch.indices.groups) === null,
      isForeignArray(directMatch.indices.groups.word),
      isForeignArray(detachedMatch),
      isForeignArray(detachedMatch.indices),
      isForeignArray(detachedMatch.indices[0]),
      isMainArray(mainMatch),
      isMainArray(mainMatch.indices),
      isMainArray(mainMatch.indices[0])
    ].join("|"));
"#;

#[test]
fn native_result_realm_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    assert_eq!(
        run_ok(SOURCE),
        [
            "collections:true|true|true|true|true|true|true|true|true|true|true|true|true",
            "records:true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true",
            "intl:true|true|true|true",
            "iterators:true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true",
            "more-iterators:true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true|true",
            "regexp:true|true|true|true|true|true|true|true|true|true|true|true",
        ]
    );
}

#[test]
fn native_result_containers_follow_builtin_realm_in_all_modes() {
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
        cmd.args(["--exact", "native_result_realm_child", "--nocapture"])
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
