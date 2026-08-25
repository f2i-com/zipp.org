//! A region leaf splice must validate the globals used by the CALLEE, not only
//! the globals named by the caller that owns the OSR region.
//!
//! The regression is deliberately run at the normal threshold.  With
//! `ZIPP_JIT_THRESHOLD=1`, Tier C compiles `drive` on its first entry before its
//! call IC is monomorphic and masks the OSR path under test.  At the normal
//! threshold the first post-mutation iteration interprets, then the generic MEM
//! region enters on the back-edge.  Before the route guard, that produced one
//! accessor hit followed by four raw-slot writes/reads.

use std::process::Command;

use zipp_vm::run;

const SOURCE: &str = r#"
    imp = 1;
    var gets = 0;
    var sets = 0;
    var last = -1;

    function leaf(x) {
      imp = x;
      return imp;
    }

    function drive(n, base) {
      var out = 0;
      for (var i = 0; i < n; i++) {
        out = leaf(base + i);
      }
      return out;
    }

    function readLeaf() {
      return imp;
    }

    function driveRead(n) {
      var out = 0;
      for (var i = 0; i < n; i++) {
        out = readLeaf();
      }
      return out;
    }

    console.log("warm", drive(200, 1), driveRead(200), imp, gets, sets, last);
    Object.defineProperty(globalThis, "imp", {
      configurable: true,
      get: function () { gets++; return 700; },
      set: function (v) { sets++; last = v; }
    });
    console.log("after", drive(5, 10), imp, gets, sets, last);

    Object.defineProperty(globalThis, "imp", {
      configurable: true,
      writable: false,
      value: 900
    });
    console.log("data", drive(5, 20), imp);

    Object.defineProperty(globalThis, "imp", {
      configurable: true,
      writable: true,
      value: 33
    });
    delete globalThis.imp;
    var deleted;
    try { deleted = "value:" + driveRead(5); }
    catch (e) { deleted = "throw:" + e.constructor.name; }
    imp = 41;
    console.log("delete-recreate", deleted, driveRead(5), imp);
"#;

fn expected() -> Vec<String> {
    [
        "warm 200 200 200 0 0 -1",
        "after 700 700 6 5 14",
        "data 900 900",
        "delete-recreate throw:ReferenceError 41 41",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[test]
fn leaf_global_route_child() {
    if std::env::var_os("ZIPP_LEAF_GLOBAL_ROUTE_CHILD").is_none() {
        return;
    }
    let outcome = run(SOURCE).expect("source compiles");
    assert!(
        outcome.error.is_none(),
        "unexpected runtime error: {:?}",
        outcome.error
    );
    assert_eq!(outcome.output, expected());
}

fn child(env: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().expect("test executable");
    let mut cmd = Command::new(exe);
    cmd.args(["leaf_global_route_child", "--exact", "--nocapture"])
        .env("ZIPP_LEAF_GLOBAL_ROUTE_CHILD", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_TYPED_SPLICE")
        .env_remove("ZIPP_NO_TIERC_LEAF")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_JITLOG")
        .env_remove("ZIPP_GC_STRESS");
    for &(key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("spawn route child")
}

#[test]
fn callee_global_routes_match_every_fallback_mode() {
    for (name, env) in [
        ("default", &[][..]),
        ("generic-mem", &[("ZIPP_NO_TYPED_SPLICE", "1")][..]),
        ("eager", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        (
            "eager-generic",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_NO_TYPED_SPLICE", "1")][..],
        ),
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
    ] {
        let out = child(env);
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{name} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn generic_mem_leaf_route_guard_is_non_vacuous() {
    let out = child(&[("ZIPP_NO_TYPED_SPLICE", "1"), ("ZIPP_JITLOG", "1")]);
    assert!(
        out.status.success(),
        "mechanism child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[leaf] fn2@5 callee fn1 INLINE-ELIGIBLE"),
        "writer leaf was not admitted:\n{stderr}"
    );
    assert!(
        stderr.contains("[jit] MEM region fn2 [2,9] compiled"),
        "writer did not reach the generic MEM region:\n{stderr}"
    );
    assert!(
        !stderr.contains(" TYPED-LANE "),
        "typed splice unexpectedly masked the generic route test:\n{stderr}"
    );
}
