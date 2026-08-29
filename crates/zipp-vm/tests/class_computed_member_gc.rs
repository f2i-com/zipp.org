//! Computed class members are installed after `MakeClass`, so user code in a
//! key expression can promote the class before the member callable is born.
//! Every callable stored after that point must cross the nursery barrier.

use std::process::Command;

const CHILD_ENV: &str = "ZIPP_CLASS_MEMBER_GC_CHILD";
const DUP_CHILD_ENV: &str = "ZIPP_CLASS_MEMBER_GC_DUP_CHILD";

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
    var keyCalls = 0;
    function key(name) {
      keyCalls++;
      return name;
    }

    function make(seed) {
      class C {
        [key("method")]() { return seed + 1; }
        get [key("read")]() { return seed + 2; }
        set [key("write")](value) { this.written = value + seed; }
        static [key("staticMethod")]() { return seed + 3; }
        accessor [key("auto")] = seed + 4;
        static { this.block = 6; }
      }
      return C;
    }

    var count = 24;
    var sum = 0;
    for (var i = 0; i < count; i++) {
      var C = make(i);
      var value = new C();
      sum += value.method() + value.read + C.staticMethod() + value.auto + C.block;
      value.write = 7;
      sum += value.written;
      value.auto = 8;
      sum += value.auto;
    }
    console.log("class-members", keyCalls, sum);
"#;

#[test]
fn class_computed_member_gc_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    assert_eq!(run_ok(SOURCE), ["class-members 120 2124"]);
    if std::env::var_os("ZIPP_GCSTATS").is_some() {
        let stores = zipp_vm::gc_oracle_stats()
            .into_iter()
            .find(|(name, _)| *name == "class_member")
            .expect("class-member barrier has its own oracle category")
            .1;
        assert_eq!(stores, 144, "six committed callable edges per class");
    }
}

#[test]
fn discarded_duplicate_member_does_not_count_as_a_store_child() {
    if std::env::var_os(DUP_CHILD_ENV).is_none() {
        return;
    }
    assert_eq!(
        run_ok(
            r#"
            function key(name) { return name; }
            function make() {
              class C {
                [key("same")]() { return "discarded"; }
                same() { return "named"; }
                static {}
              }
              return C;
            }
            var answer = "";
            for (var i = 0; i < 24; i++) answer = new (make())().same();
            console.log(answer);
            "#,
        ),
        ["named"]
    );
    let stores = zipp_vm::gc_oracle_stats()
        .into_iter()
        .find(|(name, _)| *name == "class_member")
        .expect("class-member barrier has its own oracle category")
        .1;
    assert_eq!(stores, 0, "the newly materialized callable was discarded");
}

#[test]
fn class_computed_members_survive_all_execution_modes() {
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
        (
            "minor-verify",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NURSERY_VERIFY", "1")][..],
        ),
        (
            "oracle-stress",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_GCSTATS", "1")][..],
        ),
    ] {
        let mut cmd = Command::new(&exe);
        cmd.args(["--exact", "class_computed_member_gc_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NURSERY_VERIFY")
            .env_remove("ZIPP_GCSTATS")
            .env_remove("ZIPP_NO_NURSERY");
        cmd.envs(env.iter().copied());
        let out = cmd.output().expect("spawn execution-mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let out = Command::new(&exe)
        .args([
            "--exact",
            "discarded_duplicate_member_does_not_count_as_a_store_child",
            "--nocapture",
        ])
        .env(DUP_CHILD_ENV, "1")
        .env("ZIPP_GC_STRESS", "1")
        .env("ZIPP_GCSTATS", "1")
        .env_remove("ZIPP_NO_NURSERY")
        .output()
        .expect("spawn duplicate-order child");
    assert!(
        out.status.success(),
        "duplicate/oracle failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
