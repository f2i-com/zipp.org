//! Host-created arguments need roots before a callable Proxy's apply getter
//! runs: the target function's register frame does not exist yet.

use zipp_vm::embed::{compile_script, HostValue, JsValue, ScriptState};

const SOURCE: &str = r#"
    function churn() {
        for (var wave = 0; wave < 4; wave++) {
            var scratch = [];
            for (var i = 0; i < 200; i++) scratch.push({ n: i, s: 'temporary-' + i });
        }
    }
    var proxy = new Proxy(function (value) { return value; }, {
        get apply() { churn(); return undefined; }
    });
"#;

fn fixture() -> ScriptState {
    // This variable is read during VM creation; every test in this binary
    // enables it before constructing a VM.
    std::env::set_var("ZIPP_GC_STRESS", "1");
    let mut state = compile_script(SOURCE).expect("compile fixture");
    state.run_init().expect("initialize fixture");
    state
}

#[test]
fn structured_arguments_survive_a_proxy_apply_getter() {
    let mut state = fixture();
    let slot = state
        .symbols()
        .into_iter()
        .find(|s| s.name == "proxy")
        .unwrap()
        .index;
    let value = HostValue::Object(vec![
        ("marker".into(), HostValue::String("host-input-only".into())),
        (
            "nested".into(),
            HostValue::Array(vec![HostValue::Number(42.0)]),
        ),
    ]);
    for _ in 0..3 {
        assert_eq!(state.call_slot(slot, &[value.clone()]), Ok(value.clone()));
        assert_eq!(state.host_result_roots_for_test(), 0);
    }
}

#[test]
fn primitive_arguments_survive_a_proxy_apply_getter() {
    let mut state = fixture();
    let value = JsValue::String("host-created string argument".into());
    for _ in 0..3 {
        assert_eq!(
            state.call_global("proxy", &[value.clone()]),
            Ok(value.clone())
        );
        assert_eq!(state.host_result_roots_for_test(), 0);
    }
}

#[test]
fn a_top_level_return_survives_its_queued_jobs() {
    std::env::set_var("ZIPP_GC_STRESS", "1");
    let mut state = compile_script(&format!(
        "{SOURCE}\nPromise.resolve().then(churn); return ['host', 'return'].join('-');"
    ))
    .expect("compat goal accepts top-level return");
    assert_eq!(state.run_init(), Ok(JsValue::String("host-return".into())));
}
