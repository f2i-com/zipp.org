//! Dense-vs-HashMap `[[HomeObject]]` storage parity under Tier-C publication,
//! extracted methods, slot churn, minors/majors and GC stress.

use std::process::Command;

const SOURCE: &str = r#"
    "use strict";
    const proto = { answer: 42 };
    function make(i) {
      return {
        i,
        plain() { return this.i + 1; },
        read() { return super.answer + this.i; }
      };
    }

    let home;
    for (let i = 0; i < __WARM__; i++) home = make(i);
    Object.setPrototypeOf(home, proto);
    const plain = home.plain;
    const read = home.read;
    home = null;

    let garbage = [];
    for (let i = 0; i < __CHURN__; i++) {
      garbage.push({ i, nested: { value: i + 1 } });
      if (garbage.length === 32) garbage = [];
    }
    console.log("homes", plain.call({ i: 7 }), read.call({ i: 1 }));
"#;

fn child_source() -> String {
    let stress = std::env::var_os("ZIPP_GC_STRESS").is_some();
    SOURCE
        .replace("__WARM__", if stress { "24" } else { "1400" })
        .replace("__CHURN__", if stress { "180" } else { "90000" })
}
fn run_child(env: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args(["execution_child", "--exact", "--nocapture"])
        .env("ZIPP_CLOSURE_HOME_DENSE_CHILD", "1")
        .env_remove("ZIPP_NO_DENSE_CLOSURE_HOME")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_JITLOG")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_GC_STRESS")
        .env_remove("ZIPP_NURSERY")
        .env_remove("ZIPP_NO_NURSERY")
        .env_remove("ZIPP_NURSERY_VERIFY");
    cmd.envs(env.iter().copied());
    cmd.output().expect("spawn mode child")
}

#[test]
fn execution_child() {
    if std::env::var_os("ZIPP_CLOSURE_HOME_DENSE_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(&child_source()).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, ["homes 8 43"]);
}

#[test]
fn dense_and_hashmap_modes_preserve_every_home_edge() {
    for (mode, env) in [
        (
            "dense_hot",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_JITLOG", "1")][..],
        ),
        (
            "map_hot",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_DENSE_CLOSURE_HOME", "1"),
            ][..],
        ),
        (
            "dense_stress",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_NURSERY", "1"),
                ("ZIPP_GC_STRESS", "1"),
            ][..],
        ),
        (
            "map_stress",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_NURSERY", "1"),
                ("ZIPP_GC_STRESS", "1"),
                ("ZIPP_NO_DENSE_CLOSURE_HOME", "1"),
            ][..],
        ),
        (
            "dense_minor_verify",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_NURSERY", "1"),
                ("ZIPP_NURSERY_VERIFY", "1"),
            ][..],
        ),
        (
            "map_major",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_NO_NURSERY", "1"),
                ("ZIPP_NO_DENSE_CLOSURE_HOME", "1"),
            ][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
    ] {
        let out = run_child(env);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && !stdout.contains("running 0 tests"),
            "{mode} child failed:\n{stdout}\n{stderr}"
        );
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if matches!(mode, "dense_hot" | "map_hot") {
            assert!(
                stderr.contains("Tier C"),
                "{mode} did not exercise native SetHomeObject:\n{stderr}"
            );
        }
    }
}
