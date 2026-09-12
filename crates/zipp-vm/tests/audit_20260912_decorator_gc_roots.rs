//! Decorator results stored on an old class must remain live through nursery GC.

use std::process::Command;
use zipp_vm::run;

const CHILD_ENV: &str = "ZIPP_DECORATOR_GC_ROOTS_CHILD";

fn out(source: &str) -> String {
    let result = run(source).expect("compile decorator GC regression");
    assert!(
        result.error.is_none(),
        "unexpected throw: {:?}\nsource:\n{source}",
        result.error
    );
    result.output.join("\n")
}

#[test]
fn audit_20260912_decorator_gc_roots_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }

    assert_eq!(
        out(r#"
            function field(tag) { return () => value => tag + "(" + value + ")"; }
            function accessor(tag) {
                return (value) => ({
                    get: value.get,
                    set: value.set,
                    init: initial => tag + "(" + initial + ")"
                });
            }
            class C {
                @field("outer") @field("inner") item = "value";
                @accessor("left") @accessor("right") accessor slot = "start";
                @field("static-outer") @field("static-inner") static item = "static";
            }
            const instance = new C();
            console.log(instance.item, instance.slot, C.item);
        "#),
        "inner(outer(value)) right(left(start)) static-inner(static-outer(static))"
    );

    assert_eq!(
        out(r#"
            const log = [];
            function add(tag) {
                return (value, context) => {
                    context.addInitializer(function () { log.push(tag + ":" + this.marker); });
                    return value;
                };
            }
            function replace(value, context) {
                context.addInitializer(function () { log.push("class:" + this.marker); });
                return class Replacement extends value {};
            }
            @replace
            class C {
                @add("instance-method") method() {}
                marker = "instance";
                @add("instance-field") field = 1;
                @add("static-method") static method() {}
                static marker = "static";
                @add("static-field") static field = 1;
            }
            new C();
            console.log(C.name, log.join("|"));
        "#),
        "Replacement static-method:undefined|static-field:static|class:static|instance-method:undefined|instance-field:instance"
    );

    assert_eq!(
        out(r#"
            let key;
            function replaceMethod(value, context) {
                context.metadata.decorated = String(context.name);
                return function () { return "replacement:" + context.metadata.decorated; };
            }
            class C {
                @replaceMethod [key = Symbol("dynamic")]() { return "original"; }
            }
            console.log(new C()[key](), C[Symbol.metadata].decorated);
        "#),
        "replacement:Symbol(dynamic) Symbol(dynamic)"
    );
}

#[test]
fn decorator_state_values_survive_normal_and_nursery_gc_modes() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }

    let exe = std::env::current_exe().expect("decorator regression test binary");
    for (mode, vars) in [
        ("normal", &[][..]),
        (
            "gc-stress-verified",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NURSERY_VERIFY", "1")][..],
        ),
        (
            "gc-stress-holder-grain",
            &[
                ("ZIPP_GC_STRESS", "1"),
                ("ZIPP_NURSERY_VERIFY", "1"),
                ("ZIPP_NO_VALGRAIN_REMSET", "1"),
            ][..],
        ),
    ] {
        let mut command = Command::new(&exe);
        command
            .args([
                "--exact",
                "audit_20260912_decorator_gc_roots_child",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NURSERY_VERIFY")
            .env_remove("ZIPP_NO_VALGRAIN_REMSET");
        command.envs(vars.iter().copied());
        let output = command.output().expect("spawn decorator regression child");
        assert!(
            output.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
