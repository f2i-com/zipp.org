//! The 11 September 2026 close audit's ZA-05: a host call's result stays
//! rooted while the microtasks it scheduled run.
//!
//! `host_call_slot_bounded` and `host_eval_rich` hold the callee's completion
//! value in a Rust local, drain the microtask queue — which runs guest code
//! and polls the collector between jobs — and only then marshal it. A value
//! reachable from nothing but that local is exactly what the collector frees.
//! `ZIPP_GC_STRESS` makes every safe point collect, so the test is
//! deterministic: the object is either rooted, or it is gone before the
//! marshal reads it. (The variable is read once, when the VM is built, so it
//! is set before the first `compile_script` in this binary and every VM here
//! runs under stress.)

use zipp_vm::embed::{compile_script, HostValue, HostValueBudget, ScriptState};

fn prepare(src: &str) -> ScriptState {
    // Read at `Vm::new`; every VM this binary builds collects at every safe
    // point. Process-wide, which is why this is its own test binary.
    std::env::set_var("ZIPP_GC_STRESS", "1");
    let mut st = compile_script(src).expect("compiles");
    st.run_init().expect("runs");
    st
}

fn slot_of(st: &ScriptState, name: &str) -> u32 {
    st.symbols()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no global named {name}"))
        .index
}

fn call(st: &mut ScriptState, name: &str) -> HostValue {
    let slot = slot_of(st, name);
    let mut budget = HostValueBudget::default();
    st.call_slot_bounded(slot, &[], &mut budget)
        .unwrap_or_else(|e| panic!("{name} failed: {}", e.into_message()))
}

fn object(pairs: &[(&str, HostValue)]) -> HostValue {
    HostValue::Object(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

fn num(n: f64) -> HostValue {
    HostValue::Number(n)
}

const CHURN: &str = r#"
    var jobRuns = 0;
    function churn() {
      for (var wave = 0; wave < 8; wave++) {
        var scratch = [];
        for (var i = 0; i < 500; i++) scratch.push({ n: i, a: [i, i + 1], s: "temporary-" + i });
      }
      jobRuns++;
    }
"#;

/// The audit's regression shape: the result is saved nowhere the callback
/// can see; a queued job allocates; the host reads the result afterwards.
#[test]
fn a_call_result_survives_the_microtasks_it_scheduled() {
    let mut st = prepare(&format!(
        "{CHURN}
        function makeResult() {{
          Promise.resolve().then(churn);
          return {{ marker: 'host-return-only', nested: [1, 2, 3], deep: {{ s: 'x'.repeat(40) }} }};
        }}
        function jobs() {{ return jobRuns; }}"
    ));
    let expected = object(&[
        ("marker", HostValue::String("host-return-only".into())),
        (
            "nested",
            HostValue::Array(vec![num(1.0), num(2.0), num(3.0)]),
        ),
        ("deep", object(&[("s", HostValue::String("x".repeat(40)))])),
    ]);
    for round in 1..=3 {
        assert_eq!(call(&mut st, "makeResult"), expected, "round {round}");
        assert_eq!(call(&mut st, "jobs"), num(round as f64));
    }
}

/// Strings and arrays are heap objects too; a primitive needs no root but
/// must still come back as itself.
#[test]
fn every_heap_shape_of_result_survives() {
    let mut st = prepare(&format!(
        "{CHURN}
        function str() {{ Promise.resolve().then(churn); return 'a fresh string ' + jobRuns; }}
        function arr() {{ Promise.resolve().then(churn); return [ [1], [2, [3]], 'z' ]; }}
        function prim() {{ Promise.resolve().then(churn); return 41 + 1; }}"
    ));
    assert_eq!(
        call(&mut st, "str"),
        HostValue::String("a fresh string 0".into())
    );
    assert_eq!(
        call(&mut st, "arr"),
        HostValue::Array(vec![
            HostValue::Array(vec![num(1.0)]),
            HostValue::Array(vec![num(2.0), HostValue::Array(vec![num(3.0)])]),
            HostValue::String("z".into()),
        ])
    );
    assert_eq!(call(&mut st, "prim"), num(42.0));
}

/// Several queued jobs, each collecting, before the marshal.
#[test]
fn a_result_survives_many_jobs() {
    let mut st = prepare(&format!(
        "{CHURN}
        function many() {{
          for (var i = 0; i < 12; i++) Promise.resolve().then(churn);
          return {{ after: 'twelve jobs' }};
        }}
        function jobs() {{ return jobRuns; }}"
    ));
    assert_eq!(
        call(&mut st, "many"),
        object(&[("after", HostValue::String("twelve jobs".into()))])
    );
    assert_eq!(call(&mut st, "jobs"), num(12.0));
}

/// A job that itself re-enters the host boundary (a nested drain through a
/// slot call from inside a microtask is not possible from guest code, but a
/// job that schedules more jobs is): the root stack must be a stack.
#[test]
fn a_result_survives_jobs_that_schedule_jobs() {
    let mut st = prepare(&format!(
        "{CHURN}
        function chain() {{
          Promise.resolve().then(function () {{ churn(); return Promise.resolve().then(churn); }}).then(churn);
          return [{{ id: 1 }}, {{ id: 2 }}];
        }}
        function jobs() {{ return jobRuns; }}"
    ));
    assert_eq!(
        call(&mut st, "chain"),
        HostValue::Array(vec![
            object(&[("id", num(1.0))]),
            object(&[("id", num(2.0))])
        ])
    );
    assert_eq!(call(&mut st, "jobs"), num(3.0));
}

/// The rich eval takes the same path: its completion value is marshalled
/// after the drain.
#[test]
fn a_rich_eval_result_survives_the_microtasks_it_scheduled() {
    let mut st = prepare(&format!(
        "{CHURN}
 function jobs() {{ return jobRuns; }}"
    ));
    let mut budget = HostValueBudget::default();
    let value = st
        .eval_in_context_rich(
            "(function () { Promise.resolve().then(churn); return { marker: 'eval-return-only', nested: [1, 2, 3] }; })()",
            &mut budget,
        )
        .unwrap_or_else(|e| panic!("eval failed: {}", e.into_message()));
    assert_eq!(
        value,
        object(&[
            ("marker", HostValue::String("eval-return-only".into())),
            (
                "nested",
                HostValue::Array(vec![num(1.0), num(2.0), num(3.0)])
            ),
        ])
    );
    assert_eq!(call(&mut st, "jobs"), num(1.0));
}

/// A throw and a conversion failure release the root too: nothing is left
/// pinned after either exit, so the next call's result is the only root.
#[test]
fn the_root_is_released_on_every_exit() {
    let mut st = prepare(&format!(
        "{CHURN}
        function throws() {{ Promise.resolve().then(churn); throw new Error('after scheduling'); }}
        function huge() {{ Promise.resolve().then(churn); var o = {{}}; for (var i = 0; i < 64; i++) o['k' + i] = i; return o; }}
        function fine() {{ Promise.resolve().then(churn); return {{ ok: true }}; }}
        function jobs() {{ return jobRuns; }}"
    ));
    let throws = slot_of(&st, "throws");
    let huge = slot_of(&st, "huge");
    let mut budget = HostValueBudget::default();
    let err = st
        .call_slot_bounded(throws, &[], &mut budget)
        .err()
        .expect("throws");
    assert!(err.into_message().contains("after scheduling"));
    let mut small = HostValueBudget::new(8, 1024);
    let err = st
        .call_slot_bounded(huge, &[], &mut small)
        .err()
        .expect("over budget");
    let message = err.into_message();
    assert!(
        message.contains("host value exceeds"),
        "expected a conversion failure, got {message:?}"
    );
    assert_eq!(
        st.host_result_roots_for_test(),
        0,
        "no root outlives its call"
    );
    assert_eq!(
        call(&mut st, "fine"),
        object(&[("ok", HostValue::Bool(true))])
    );
    assert_eq!(st.host_result_roots_for_test(), 0);
    assert_eq!(call(&mut st, "jobs"), num(3.0));
}

/// Surfaced by the tests above: a throw that reached the host stayed
/// recorded as "in flight", and the next call that entered compiled code
/// (a loop body, here) failed with the PREVIOUS call's error. Delivering a
/// throw to the host now clears it; the next call is the next call.
#[test]
fn a_delivered_throw_does_not_resurface_in_the_next_call() {
    let mut st = prepare(&format!(
        "{CHURN}
        function throws() {{ Promise.resolve().then(churn); throw new Error('after scheduling'); }}
        function loops() {{ var o = {{}}; for (var i = 0; i < 64; i++) o['k' + i] = i; return Object.keys(o).length; }}
        function throwsPlain() {{ throw new TypeError('plain'); }}"
    ));
    let throws = slot_of(&st, "throws");
    let plain = slot_of(&st, "throwsPlain");
    let mut budget = HostValueBudget::default();
    for round in 0..3 {
        let err = st
            .call_slot_bounded(throws, &[], &mut budget)
            .err()
            .expect("throws")
            .into_message();
        assert!(err.contains("after scheduling"), "round {round}: {err}");
        assert_eq!(call(&mut st, "loops"), num(64.0), "round {round}");
        let err = st
            .call_slot_bounded(plain, &[], &mut budget)
            .err()
            .expect("throws")
            .into_message();
        assert!(err.contains("plain"), "round {round}: {err}");
        assert_eq!(call(&mut st, "loops"), num(64.0), "round {round}");
    }
    // The same through the name-resolving entries.
    assert!(st.call_global("throwsPlain", &[]).is_err());
    assert_eq!(call(&mut st, "loops"), num(64.0));
    assert!(st.eval_in_context("throwsPlain()").is_err());
    assert_eq!(call(&mut st, "loops"), num(64.0));
}
