//! Regression coverage for the zipp-wasm-only single-agent specialization.
//!
//! The feature removes the test262 worker/thread/channel harness, not the
//! language's shared-memory implementation or the embedded host boundary.

fn run_output(source: &str) -> Vec<String> {
    let outcome = zipp_vm::run(source).expect("source compiles");
    assert_eq!(outcome.error, None, "unexpected top-level throw");
    outcome.output
}

#[test]
fn embedded_code_still_has_no_test262_host_object() {
    let mut state = zipp_vm::embed::compile_script("0").expect("source compiles");
    state.run_init().expect("source runs");
    assert_eq!(
        state
            .eval_in_context("typeof $262 + ',' + typeof globalThis.$262")
            .expect("probe evaluates")
            .as_str(),
        Some("undefined,undefined")
    );
    assert!(state.eval_in_context("$262.agent.start('')").is_err());
}

/// A normal native build continues to exercise the real concurrent test262
/// host. This catches an accidental broad cfg that removes agents outside the
/// dedicated artifact profile.
#[cfg(not(feature = "wasm-single-agent"))]
#[test]
fn ordinary_profile_still_runs_a_worker_agent() {
    assert_eq!(
        run_output(
            r#"
            $262.agent.start("$262.agent.report('worker-ready'); $262.agent.leaving()")
            let report = null
            for (let i = 0; i < 200 && report === null; i++) {
              $262.agent.sleep(1)
              report = $262.agent.getReport()
            }
            console.log(report)
            "#,
        ),
        ["worker-ready"]
    );
}

/// The specialization must keep SharedArrayBuffer storage and Atomics native
/// operations. `safe-sandbox` intentionally hides those globals as a separate
/// policy, so run this assertion in the non-sandboxed feature profile.
#[cfg(all(feature = "wasm-single-agent", not(feature = "safe-sandbox")))]
#[test]
fn specialized_profile_keeps_shared_array_buffer_and_atomics() {
    assert_eq!(
        run_output(
            r#"
            const sab = new SharedArrayBuffer(16)
            const words = new Int32Array(sab)
            console.log(
              sab.byteLength,
              Atomics.store(words, 0, 40),
              Atomics.add(words, 0, 2),
              Atomics.load(words, 0)
            )
            "#,
        ),
        ["16 40 40 42"]
    );
}

#[cfg(feature = "wasm-single-agent")]
#[test]
fn specialized_agent_start_fails_catchably_after_argument_coercion() {
    let outcome = zipp_vm::run(
        r#"
        $262.agent.start({
          toString() { console.log("coerced"); return "" }
        })
        "#,
    )
    .expect("source compiles");

    assert_eq!(outcome.output, ["coerced"]);
    assert_eq!(
        outcome.error.as_deref(),
        Some("EvalError: external code is disabled by the host")
    );
}

#[cfg(feature = "wasm-single-agent")]
#[test]
fn specialized_agent_surface_preserves_local_and_validation_semantics() {
    assert_eq!(
        run_output(
            r#"
            console.log($262.agent.getReport())
            $262.agent.report("local")
            console.log($262.agent.getReport(), $262.agent.getReport())
            try { $262.agent.broadcast({}) }
            catch (e) { console.log(e.name, e.message) }
            "#,
        ),
        [
            "null",
            "local null",
            "TypeError $262.agent.broadcast requires a SharedArrayBuffer",
        ]
    );
}
