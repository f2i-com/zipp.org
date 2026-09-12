//! Weak references must agree across the major collector, nursery verification,
//! and the interpreter.  The child-process boundary is required because the GC
//! mode latches are process-wide.

use std::process::Command;
use zipp_vm::run;

const CHILD_ENV: &str = "ZIPP_WEAK_GC_AUDIT_CHILD";

fn output(source: &str) -> Vec<String> {
    let result = run(source).expect("compile weak-GC audit source");
    assert!(
        result.error.is_none(),
        "unexpected throw: {:?}\nsource:\n{source}",
        result.error
    );
    result.output
}

#[test]
fn audit_20260912_weak_gc_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }

    let harness_boundary = zipp_vm::run_with_harness(
        "console.log(preludeRef.deref() === undefined);",
        r#"
            var preludeRef;
            (function () {
                const target = { from: "harness" };
                preludeRef = new WeakRef(target);
            })();
        "#,
        None,
    )
    .expect("compile harness WeakRef boundary");
    assert!(harness_boundary.error.is_none());
    assert_eq!(harness_boundary.output, vec!["true"]);

    let mut host = zipp_vm::embed::compile_script(
        r#"
        var hostRef;
        function makeHostRef() {
            const target = { from: "host-call" };
            hostRef = new WeakRef(target);
            return hostRef.deref() !== undefined;
        }
        function hostRefIsCleared() { return hostRef.deref() === undefined; }
        "#,
    )
    .expect("compile host-boundary fixture");
    host.run_init().expect("initialize host-boundary fixture");
    assert_eq!(
        host.call_global("makeHostRef", &[]),
        Ok(zipp_vm::embed::JsValue::Bool(true))
    );
    assert_eq!(
        host.call_global("hostRefIsCleared", &[]),
        Ok(zipp_vm::embed::JsValue::Bool(true))
    );
    assert_eq!(
        host.eval_in_context(
            "(function(){ const target = {}; hostRef = new WeakRef(target); })(), \
             hostRef.deref() !== undefined"
        ),
        Ok(zipp_vm::embed::JsValue::Bool(true))
    );
    assert_eq!(
        host.eval_in_context("hostRef.deref() === undefined"),
        Ok(zipp_vm::embed::JsValue::Bool(true))
    );

    assert_eq!(
        output(
            r#"
            const cleanup = [];
            const registry = new FinalizationRegistry(held => cleanup.push(held.tag));
            const heldRegistry = new FinalizationRegistry(held => cleanup.push(held.tag));

            const liveKey = { id: "live" };
            const liveMap = new WeakMap();
            const middle = { id: "middle" };
            const leaf = { id: "leaf" };
            liveMap.set(liveKey, middle);
            liveMap.set(middle, leaf);
            const leafRef = new WeakRef(leaf);

            let deadRef;
            let deadMap;
            let deadSet;
            let heldRef;
            (function () {
                const target = { id: "dead" };
                const held = { tag: "collected" };
                deadRef = new WeakRef(target);
                heldRef = new WeakRef(held);
                // A value-to-key cycle must not make the ephemeron key live.
                deadMap = new WeakMap([[target, { back: target }]]);
                deadSet = new WeakSet([target]);
                // Keep this registration isolated from cells that may be
                // cleared earlier in the main job. Its cleanup job is first
                // scheduled at the main-to-promise boundary, after the promise
                // reaction already in the queue.
                heldRegistry.register(target, held);
            })();

            // unregister removes every cell carrying the token.
            const canceledToken = {};
            (function () {
                registry.register({}, { tag: "canceled-1" }, canceledToken);
                registry.register({}, { tag: "canceled-2" }, canceledToken);
            })();
            const unregisterResult = registry.unregister(canceledToken) + ":" +
                registry.unregister(canceledToken);

            // The token is weak, but losing it does not lose the registration.
            let tokenRef;
            (function () {
                const target = {};
                const token = {};
                tokenRef = new WeakRef(token);
                registry.register(target, { tag: "token-dead" }, token);
            })();

            // An abrupt cleanup callback is consumed at the job boundary and
            // remaining cleared records are delivered by a later cleanup job.
            const throwLog = [];
            const throwingRegistry = new FinalizationRegistry(function (held) {
                throwLog.push(held);
                if (held === "throw") throw new Error("cleanup boom");
            });
            (function () {
                throwingRegistry.register({}, "throw");
                throwingRegistry.register({}, "after");
            })();

            // Mutation during cleanup sees cleared cells: unregistering the
            // shared token from the first callback cancels the other callback.
            const mutationLog = [];
            const mutationToken = {};
            const mutationRegistry = new FinalizationRegistry(function (held) {
                mutationLog.push(held);
                mutationRegistry.unregister(mutationToken);
            });
            (function () {
                mutationRegistry.register({}, "one", mutationToken);
                mutationRegistry.register({}, "two", mutationToken);
            })();

            // AddToKeptObjects: construction and deref keep the target through
            // the current synchronous job even under collection-per-safe-point.
            console.log("same-job", deadRef.deref().id);

            Promise.resolve().then(function () {
                // Capture the registry itself so its strong holding edge
                // remains observable through this job.
                heldRegistry.unregister(liveKey);
                console.log(
                    "next-job",
                    deadRef.deref() === undefined,
                    leafRef.deref().id,
                    liveMap.get(liveKey).id,
                    heldRef.deref().tag,
                    tokenRef.deref() === undefined,
                    unregisterResult
                );
            }).then(function () {
                // The cleanup job may be interleaved with promise jobs; a timer
                // observes the queue after it has drained.
                setTimeout(function () {
                    console.log("cleanup", cleanup.sort().join(","));
                    console.log("throw", throwLog.sort().join(","));
                    console.log("mutation", mutationLog.length);
                }, 0);
            });
            "#,
        ),
        vec![
            "same-job dead",
            "next-job true leaf middle collected true true:false",
            "cleanup collected,token-dead",
            "throw after,throw",
            "mutation 1",
        ]
    );
}

#[test]
fn weak_gc_major_nursery_safe_and_nojit_modes_agree() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }

    let exe = std::env::current_exe().expect("weak-GC audit test binary");
    for (mode, vars) in [
        (
            "major-stress",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NO_NURSERY", "1")][..],
        ),
        (
            "nursery-verified-stress",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NURSERY_VERIFY", "1")][..],
        ),
        (
            "interpreter-major-stress",
            &[
                ("ZIPP_GC_STRESS", "1"),
                ("ZIPP_NO_NURSERY", "1"),
                ("ZIPP_NOJIT", "1"),
            ][..],
        ),
    ] {
        let mut command = Command::new(&exe);
        command
            .args(["--exact", "audit_20260912_weak_gc_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NURSERY_VERIFY")
            .env_remove("ZIPP_NO_NURSERY")
            .env_remove("ZIPP_NOJIT")
            .envs(vars.iter().copied());
        let result = command.output().expect("spawn weak-GC audit child");
        assert!(
            result.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
