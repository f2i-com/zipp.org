//! Exactness, GC/rooting and same-binary ablation coverage for Tier-C static
//! aggregate allocation/appends, tagged-Int `String`, and the cold
//! `DeleteProp` exit.

use std::process::Command;

const OBJECT_SOURCE: &str = r#"
    "use strict";
    function make(i) {
      const child = { value: i, next: i + 1 };
      const outer = {
        alpha: i,
        child: child,
        truth: (i & 1) === 0,
        omega: i + 3
      };
      return outer;
    }
    function makePair(i) { return [i, i + 1]; }
    function nullable(value) { return value == null ? 1 : 0; }
    function holey(i) { return [i, , i + 2]; }
    function spread(i) { return [0, ...[i, i + 1], 3]; }
    function reusedTemp() { return { a: 1, b: 2, c: 3 }; }

    const retained = [];
    let checksum = 0;
    for (let i = 0; i < 1200; i++) {
      const o = make(i);
      if ((i & 63) === 0) retained.push(o);
      checksum = (checksum + o.alpha + o.child.next + (o.truth ? 1 : 0) + o.omega) | 0;
    }
    const sample = make(7);
    console.log("object", checksum, retained.length,
                Object.keys(sample).join(","), sample.child.value, sample.omega);
    let pairSum = 0;
    for (let i = 0; i < 1200; i++) pairSum = (pairSum + makePair(i)[1]) | 0;
    console.log("array", pairSum, makePair(4).join(","));
    console.log("nullish",
                nullable(null), nullable(undefined), nullable(0), nullable({}),
                nullable($262.IsHTMLDDA));
    const h = holey(4);
    const s = spread(4);
    console.log("excluded", h.length, 1 in h, h[2], s.join(","));
    const reused = reusedTemp();
    console.log("reuse", reused.a, reused.b, reused.c);

    // Eval-installed functions use unified function ids.  A child-realm object
    // must still be born with that realm's %Object.prototype%.
    const realm = $262.createRealm().global;
    realm.eval(`
      function realmMake(i) {
        return { local: i, nested: { value: i + 1 }, pair: [i, i + 1] };
      }
      let hold;
      for (let i = 0; i < 600; i++) hold = realmMake(i);
      this.hold = hold;
    `);
    console.log("realm",
                realm.hold.local,
                Object.getPrototypeOf(realm.hold) === realm.Object.prototype,
                Object.getPrototypeOf(realm.hold.pair) === realm.Array.prototype,
                realm.hold instanceof realm.Object,
                realm.hold instanceof Object);
"#;

const DELETE_SOURCE: &str = r#"
    "use strict";
    function touch(o, serial) {
      let x = (o.a + 1) | 0;
      x = (x + 2) | 0;
      x = (x + 3) | 0;
      if ((serial & 4095) === 17) delete o.drop;
      x = (x + o.a) | 0;
      return x;
    }
    const common = { a: 4, drop: 9 };
    let sum = 0;
    for (let i = 0; i < 6000; i++) sum = (sum + touch(common, i)) | 0;
    console.log("common", sum, "drop" in common);

    function remove(o) {
      let x = 1;
      x = (x + 2) | 0;
      x = (x + 3) | 0;
      return delete o.fixed;
    }
    const sealed = {};
    Object.defineProperty(sealed, "fixed", { value: 1, configurable: false });
    let strictResult = "none";
    try { remove(sealed); } catch (e) { strictResult = e.name; }
    let nullResult = "none";
    try { remove(null); } catch (e) { nullResult = e.name; }
    const open = { fixed: 2 };
    console.log("delete", strictResult, nullResult, remove(open), "fixed" in open);
"#;

const INT_STRING_SOURCE: &str = r#"
    "use strict";
    function stringify(value) {
      let work = 1;
      work = (work + 2) | 0;
      work = (work + 3) | 0;
      work = (work + 4) | 0;
      work = (work + 5) | 0;
      const result = String(value);
      return result;
    }

    let units = 0;
    for (let i = 0; i < 6000; i++) units = (units + stringify(i & 63).length) | 0;
    console.log("ints", units, stringify(-2147483648), stringify(2147483647));

    let calls = 0;
    const observable = {
      toString() { calls++; return "object-value"; }
    };
    console.log("fallback", stringify(observable), calls,
                stringify(true), stringify(Symbol("desc")));
"#;

fn run_child(filter: &str, marker: &str, env: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args([filter, "--exact", "--nocapture"])
        .env(marker, "1")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_JITLOG")
        .env_remove("ZIPP_NO_TIERC_OBJECT_LITERAL")
        .env_remove("ZIPP_NO_TIERC_PLANNED_APPEND_PROBE")
        .env_remove("ZIPP_NO_TIERC_NEW_ARRAY")
        .env_remove("ZIPP_NO_TIERC_LOOSE_NULL_EQ")
        .env_remove("ZIPP_NO_TIERC_INT_STRING")
        .env_remove("ZIPP_NO_TIERC_COLD_DELETE")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_GC_STRESS");
    for &(key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("spawn mode child")
}

fn assert_child(mode: &str, out: &std::process::Output) {
    assert!(
        out.status.success() && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
        "{mode} child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn object_execution_child() {
    if std::env::var_os("ZIPP_TIERC_OBJECT_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(OBJECT_SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(
        out.output,
        [
            "object 2163600 19 alpha,child,truth,omega 7 10",
            "array 720600 4,5",
            "nullish 1 1 0 0 1",
            "excluded 3 false 6 0,4,5,3",
            "reuse 1 2 3",
            // zipp's Object @@hasInstance accepts ordinary cross-realm
            // objects, but the explicit prototype identity still proves that
            // the helper selected the child realm's intrinsic.
            "realm 599 true true true true",
        ]
    );
}

#[test]
fn object_literal_ablation_nojit_and_gc_modes_match() {
    for (mode, env) in [
        (
            "hot",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_JITLOG", "1")][..],
        ),
        (
            "object_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_TIERC_OBJECT_LITERAL", "1"),
            ][..],
        ),
        (
            "planned_probe_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_TIERC_PLANNED_APPEND_PROBE", "1"),
            ][..],
        ),
        (
            "array_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_TIERC_NEW_ARRAY", "1"),
            ][..],
        ),
        (
            "loose_null_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_TIERC_LOOSE_NULL_EQ", "1"),
            ][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let out = run_child("object_execution_child", "ZIPP_TIERC_OBJECT_CHILD", env);
        assert_child(mode, &out);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        let stderr = String::from_utf8_lossy(&out.stderr);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if mode == "hot" || mode == "planned_probe_off" {
            assert!(
                stderr.contains("Tier C"),
                "{mode} object body did not compile:\n{stderr}"
            );
        } else if mode == "object_off" {
            assert!(
                stderr.contains("object literal op (disabled)"),
                "off switch did not reject the lane:\n{stderr}"
            );
        } else if mode == "array_off" {
            assert!(
                stderr.contains("NewArray (disabled)"),
                "array off switch did not reject the lane:\n{stderr}"
            );
        } else if mode == "loose_null_off" {
            assert!(
                stderr.contains("LooseEq (not adjacent nullish comparison)"),
                "null-equality off switch did not reject the lane:\n{stderr}"
            );
        }
    }
}

#[test]
fn delete_execution_child() {
    if std::env::var_os("ZIPP_TIERC_DELETE_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(DELETE_SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(
        out.output,
        [
            "common 84000 false",
            "delete TypeError TypeError true false"
        ]
    );
}

#[test]
fn cold_delete_common_path_compiles_and_rare_path_resumes_exactly() {
    for (mode, env) in [
        (
            "hot",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_JITLOG", "1")][..],
        ),
        (
            "delete_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_TIERC_COLD_DELETE", "1"),
            ][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
    ] {
        let out = run_child("delete_execution_child", "ZIPP_TIERC_DELETE_CHILD", env);
        assert_child(mode, &out);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        let stderr = String::from_utf8_lossy(&out.stderr);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if mode == "hot" {
            assert!(
                stderr.contains("Tier C"),
                "touch did not compile:\n{stderr}"
            );
        } else if mode == "delete_off" {
            assert!(
                stderr.contains("DeleteProp (disabled)"),
                "off switch did not reject the cold exit:\n{stderr}"
            );
        }
    }
}

#[test]
fn int_string_execution_child() {
    if std::env::var_os("ZIPP_TIERC_INT_STRING_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(INT_STRING_SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(
        out.output,
        [
            "ints 11060 -2147483648 2147483647",
            "fallback object-value 1 true Symbol(desc)",
        ]
    );
}

#[test]
fn int_string_ablation_fallback_and_gc_modes_match() {
    for (mode, env) in [
        (
            "hot",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_JITLOG", "1")][..],
        ),
        (
            "int_string_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_TIERC_INT_STRING", "1"),
            ][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let out = run_child(
            "int_string_execution_child",
            "ZIPP_TIERC_INT_STRING_CHILD",
            env,
        );
        assert_child(mode, &out);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        let stderr = String::from_utf8_lossy(&out.stderr);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if mode == "hot" {
            assert!(
                stderr.contains("Tier C") && !stderr.contains("GlobalFn String"),
                "String(Int) body did not compile:\n{stderr}"
            );
        } else if mode == "int_string_off" {
            assert!(
                stderr.contains("GlobalFn String (disabled)"),
                "off switch did not reject the lane:\n{stderr}"
            );
        }
    }
}

#[cfg(feature = "instrument")]
#[test]
fn object_allocation_remains_bounded_by_embedder_heap_limit() {
    use zipp_vm::embed;

    let mut state = embed::compile_script("var ready = true;").expect("compiles");
    state.run_init().expect("runs");
    state.set_limits(50_000_000, None);
    state.set_heap_limit(state.heap_bytes() + 400_000);
    let err = state
        .eval_in_context(
            "(function(){var a=[];for(var i=0;i<1000000;i++)a.push({a:i,b:i+1,c:i+2,d:i+3});return a.length})()",
        )
        .expect_err("object allocation must hit the host ceiling");
    assert!(err.contains("memory budget"), "unexpected error: {err}");
    assert!(
        state.steps_remaining() > 0,
        "heap, not step, limit must win"
    );
}
