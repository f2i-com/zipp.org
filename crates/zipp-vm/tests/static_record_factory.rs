//! Bounded static-record factory semantics, guard declines, GC/meter exclusion,
//! and same-binary kill-switch coverage. Child processes isolate env latches.

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
use std::process::{Command, Output};

const CHILD_ENV: &str = "ZIPP_STATIC_RECORD_CHILD";
#[cfg(feature = "instrument")]
const METER_CHILD_ENV: &str = "ZIPP_STATIC_RECORD_METER_CHILD";

const SOURCE: &str = r#"
  function stable(value, kind) {
    "use strict";
    return { value, kind, left: value ^ 85, right: value + 3 };
  }
  function mega(value, kind) {
    "use strict";
    switch (kind & 15) {
      case 0: return { value, kind, left: value ^ 85, right: value + 3, a0: 0 };
      case 1: return { a1: 1, value, kind, left: value ^ 85, right: value + 3 };
      case 2: return { kind, a2: 2, value, right: value + 3, left: value ^ 85 };
      case 3: return { left: value ^ 85, kind, a3: 3, right: value + 3, value };
      case 4: return { right: value + 3, value, a4: 4, left: value ^ 85, kind };
      case 5: return { a5: 5, left: value ^ 85, value, kind, right: value + 3 };
      case 6: return { kind, right: value + 3, a6: 6, value, left: value ^ 85 };
      case 7: return { left: value ^ 85, a7: 7, kind, value, right: value + 3 };
      case 8: return { a8: 8, right: value + 3, left: value ^ 85, value, kind };
      case 9: return { value, a9: 9, right: value + 3, kind, left: value ^ 85 };
      case 10: return { kind, left: value ^ 85, right: value + 3, a10: 10, value };
      case 11: return { right: value + 3, kind, value, left: value ^ 85, a11: 11 };
      case 12: return { a12: 12, value, left: value ^ 85, kind, right: value + 3 };
      case 13: return { left: value ^ 85, right: value + 3, value, a13: 13, kind };
      case 14: return { kind, a14: 14, value, left: value ^ 85, right: value + 3 };
      default: return { right: value + 3, a15: 15, kind, left: value ^ 85, value };
    }
  }

  function stableBatch(seed) {
    let sum = 0, last;
    for (let i = 0; i < 96; i++) {
      last = stable((seed + i) | 0, i & 15);
      sum = (sum + last.value + last.left + last.right + last.kind) | 0;
    }
    return sum + ":" + Object.keys(last).join(",");
  }
  function megaBatch(seed) {
    let sum = 0, last;
    for (let i = 0; i < 96; i++) {
      last = mega((seed + i) | 0, i & 15);
      sum = (sum + last.value + last.left + last.right + last.kind) | 0;
    }
    return sum + ":" + Object.keys(last).join(",");
  }
  let stableResult, megaResult;
  for (let round = 0; round < 10; round++) {
    stableResult = stableBatch(round * 1000);
    megaResult = megaBatch(round * 1000);
  }

  // Warm a single exact call site, then feed every runtime decline class into
  // that same compiled prefix. Coercion must occur only in ordinary bytecode.
  function invoke(value, kind) {
    const out = stable(value, kind);
    return out;
  }
  // Run well past both the function and backedge thresholds so every guarded
  // call below executes the already-emitted prefix instead of merely causing
  // `invoke` itself to compile.
  for (let i = 0; i < 160; i++) invoke(i, i & 15);
  let coercions = 0;
  const observable = { valueOf() { coercions++; return 1; } };
  const observed = invoke(observable, 2);
  const fractional = invoke(1.5, 3);
  const overflow = invoke(2147483647, 4);
  const stringKind = invoke(7, "5");

  // The exact live callee guard must observe rebinding at the already-hot site.
  const savedStable = stable;
  let replacementCalls = 0;
  stable = function(value, kind) {
    replacementCalls++;
    return { value, kind, left: -1, right: -2 };
  };
  const replaced = invoke(9, 6);
  stable = savedStable;

  // A closure stamped with a direct-eval scope is deliberately ineligible at
  // runtime even though its immutable call bytecode targets the root factory.
  // Passing the target as a parameter keeps this caller Tier-C eligible; a
  // direct global reference would compile to LoadGlobalDyn and make this guard
  // test vacuous.
  function makeEvalCaller() {
    eval("var hidden = 1");
    return function(value, kind, fn) {
      const out = fn(value, kind);
      return out;
    };
  }
  const evalCaller = makeEvalCaller();
  let evalResult;
  for (let i = 0; i < 160; i++) evalResult = evalCaller(i, i & 15, stable);

  // A different-realm live callee at a polymorphic hot site must also take the
  // ordinary call. Its object is born with that realm's Object prototype.
  function invokeAny(fn, value, kind) {
    const out = fn(value, kind);
    return out;
  }
  for (let i = 0; i < 160; i++) invokeAny(stable, i, i & 15);
  const realm = $262.createRealm().global;
  const realmFactory = realm.eval("(function(value,kind){ 'use strict'; return {value:value,kind:kind,left:value^85,right:value+3}; })");
  const realmObject = invokeAny(realmFactory, 11, 7);

  const descriptor = Object.getOwnPropertyDescriptor(evalResult, "left");
  const beforeDelete = Object.keys(evalResult).join(",");
  delete evalResult.left;
  evalResult.left = 123;
  const afterDelete = Object.keys(evalResult).join(",");
  console.log("batches", stableResult, megaResult);
  console.log("declines", observed.left, observed.right, coercions,
              fractional.left, fractional.right,
              overflow.right, stringKind.kind,
              replaced.left, replaced.right, replacementCalls);
  console.log("semantics", descriptor.enumerable, descriptor.writable,
              beforeDelete, afterDelete, evalResult.left);
  console.log("realm", realmObject.value,
              Object.getPrototypeOf(realmObject) === realm.Object.prototype,
              Object.getPrototypeOf(realmObject) === Object.prototype);
"#;

#[cfg(feature = "instrument")]
const METER_SOURCE: &str = r#"
  function stable(value, kind) {
    "use strict";
    return { value, kind, left: value ^ 85, right: value + 3 };
  }
  function drive(seed) {
    let sum = 0;
    for (let i = 0; i < 96; i++) {
      const out = stable((seed + i) | 0, i & 15);
      sum = (sum + out.value + out.left + out.right + out.kind) | 0;
    }
    return sum;
  }
"#;

fn execute_source() -> String {
    let out = zipp_vm::run(SOURCE).expect("record source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    out.output.join("|")
}

#[test]
fn static_record_execution_child() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return;
    };
    let result = execute_source();
    let stats = zipp_vm::static_record_factory_stats();
    println!("static-record-result:{result}");
    println!(
        "static-record-stats:{},{},{},{}",
        stats.0, stats.1, stats.2, stats.3
    );
    if mode == "enabled" {
        assert!(
            stats.0 >= 2,
            "stable/mega plans were not installed: {stats:?}"
        );
        assert!(stats.1 > 100, "factory prefix was not attempted: {stats:?}");
        assert!(stats.2 > 100, "factory prefix was not served: {stats:?}");
        assert!(
            stats.3 > 100,
            "negative live/EvalScope guards were vacuous: {stats:?}"
        );
    } else {
        assert_eq!(stats, (0, 0, 0, 0), "{mode} unexpectedly used factory");
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn child(mode: &str, env: &[(&str, &str)]) -> Output {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args(["static_record_execution_child", "--exact", "--nocapture"])
        .env(CHILD_ENV, mode)
        .env("ZIPP_STATIC_RECORD_STATS", "1")
        .env("ZIPP_STATIC_KEY_STATS", "1")
        .env("ZIPP_JIT_THRESHOLD", "4")
        .env("ZIPP_JITLOG", "1");
    for key in [
        "ZIPP_NO_STATIC_RECORD_FACTORY",
        "ZIPP_NO_STATIC_KEY_PLANS",
        "ZIPP_NO_CROSSCALL",
        "ZIPP_NOJIT",
        "ZIPP_GC_STRESS",
    ] {
        cmd.env_remove(key);
    }
    cmd.envs(env.iter().copied());
    cmd.output().expect("spawn static-record child")
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn result(out: &Output, mode: &str) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && !stdout.contains("running 0 tests"),
        "{mode} child failed:\n{stdout}\n{stderr}"
    );
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("static-record-result:"))
        .unwrap_or_else(|| panic!("{mode} child emitted no result:\n{stdout}\n{stderr}"))
        .to_owned()
}

#[test]
fn static_record_matches_off_nojit_and_gc_stress() {
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    {
        let enabled = child("enabled", &[]);
        let enabled_result = result(&enabled, "enabled");
        let stderr = String::from_utf8_lossy(&enabled.stderr);
        assert!(
            stderr.contains("STATIC-RECORD"),
            "record call prefix was not emitted:\n{stderr}"
        );
        for (mode, env) in [
            ("off", &[(("ZIPP_NO_STATIC_RECORD_FACTORY"), "1")][..]),
            ("nojit", &[("ZIPP_NOJIT", "1")][..]),
            ("gc", &[("ZIPP_GC_STRESS", "1")][..]),
        ] {
            let out = child(mode, env);
            assert_eq!(
                enabled_result,
                result(&out, mode),
                "{mode} semantics differ"
            );
        }
    }

    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    {
        assert!(!execute_source().is_empty());
        assert_eq!(zipp_vm::static_record_factory_stats(), (0, 0, 0, 0));
    }
}

#[cfg(feature = "instrument")]
#[test]
fn static_record_metered_child() {
    if std::env::var_os(METER_CHILD_ENV).is_none() {
        return;
    }
    let mut state = zipp_vm::embed::compile_script(METER_SOURCE).expect("meter source compiles");
    state.run_init().expect("unmetered source initializes");
    for round in 0..16 {
        state
            .call_global(
                "drive",
                &[zipp_vm::embed::JsValue::Number((round * 1000) as f64)],
            )
            .expect("unmetered drive runs");
    }
    let before = zipp_vm::static_record_factory_stats();
    assert!(
        before.0 > 0 && before.1 > 100 && before.2 > 100,
        "unmetered prefix did not engage before late metering: {before:?}"
    );

    // Attaching instrumentation must discard both installed native code and
    // its record metadata. Recompiled metered code is ineligible, so neither
    // plans nor attempts may increase after this boundary.
    state.set_limits(u64::MAX, None);
    for round in 16..24 {
        state
            .call_global(
                "drive",
                &[zipp_vm::embed::JsValue::Number((round * 1000) as f64)],
            )
            .expect("metered drive runs");
    }
    assert_eq!(zipp_vm::static_record_factory_stats(), before);
}

#[cfg(all(feature = "instrument", feature = "jit", target_arch = "x86_64"))]
#[test]
fn late_metering_keeps_static_record_plan_and_prefix_disabled() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = Command::new(exe)
        .args(["static_record_metered_child", "--exact", "--nocapture"])
        .env(METER_CHILD_ENV, "1")
        .env("ZIPP_STATIC_RECORD_STATS", "1")
        .env("ZIPP_JIT_THRESHOLD", "4")
        .env_remove("ZIPP_NO_STATIC_RECORD_FACTORY")
        .env_remove("ZIPP_GC_STRESS")
        .output()
        .expect("spawn metered static-record child");
    assert!(
        out.status.success(),
        "meter child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
