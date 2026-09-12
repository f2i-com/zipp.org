//! A proxy trap created by a getter is a temporary, including when calling it
//! first performs another allocating proxy apply lookup.

use zipp_vm::embed::{compile_script, JsValue, ScriptState};

const SOURCE: &str = r#"
    function churn() {
        for (var wave = 0; wave < 4; wave++) {
            var garbage = [];
            for (var i = 0; i < 200; i++) garbage.push({n: i, text: 'garbage-' + i});
        }
    }
    var proxy = new Proxy(function(value) { return value; }, {
        get apply() {
            return new Proxy(function(target, receiver, args) { return args[0]; }, {
                get apply() { churn(); return undefined; }
            });
        }
    });
    function rendered() {
        return {
            get toString() {
                return new Proxy(function() { return 'fresh-proxy-render'; }, {
                    get apply() { churn(); return undefined; }
                });
            }
        };
    }
    function primitive() {
        return {
            get [Symbol.toPrimitive]() {
                return new Proxy(function(hint) { return 'primitive-' + hint; }, {
                    get apply() { churn(); return undefined; }
                });
            }
        };
    }
"#;

fn fixture() -> ScriptState {
    std::env::set_var("ZIPP_GC_STRESS", "1");
    let mut state = compile_script(SOURCE).expect("compile fixture");
    state.run_init().expect("initialize fixture");
    state
}

#[test]
fn a_fresh_proxy_trap_keeps_its_target_and_generated_arguments_alive() {
    let mut state = fixture();
    let value = JsValue::String("nested-trap-argument".into());
    assert_eq!(state.call_global("proxy", &[value.clone()]), Ok(value));
    assert_eq!(state.host_result_roots_for_test(), 0);
}

#[test]
fn result_rendering_keeps_a_fresh_proxy_method_alive() {
    let mut state = fixture();
    assert_eq!(
        state.call_global("rendered", &[]),
        Ok(JsValue::Object("fresh-proxy-render".into()))
    );
    assert_eq!(state.host_result_roots_for_test(), 0);
}

#[test]
fn primitive_conversion_keeps_a_fresh_proxy_method_and_hint_alive() {
    let mut state = fixture();
    assert_eq!(
        state.call_global("primitive", &[]),
        Ok(JsValue::Object("primitive-string".into()))
    );
    assert_eq!(state.host_result_roots_for_test(), 0);
}

#[test]
fn revocation_during_apply_lookup_keeps_the_captured_target_alive() {
    let mut state = fixture();
    state
        .eval_in_context(
            r#"
        var pair = Proxy.revocable(function(value) { return 'revoked-' + value; }, {
            get apply() { pair.revoke(); churn(); return undefined; }
        });
        var revocable = pair.proxy;
    "#,
        )
        .expect("initialize revocable Proxy");
    assert_eq!(
        state.call_global("revocable", &[JsValue::String("argument".into())]),
        Ok(JsValue::String("revoked-argument".into()))
    );
    assert_eq!(state.host_result_roots_for_test(), 0);
    let error = state
        .call_global("revocable", &[])
        .expect_err("now revoked");
    assert!(error.contains("revoked"), "{error}");
    assert_eq!(state.host_result_roots_for_test(), 0);
}

#[test]
fn non_callable_and_null_apply_traps_preserve_errors_and_release_roots() {
    let mut state = fixture();
    state
        .eval_in_context(
            r#"
        var badTrap = new Proxy(function() {}, {apply: 17});
        var nullTrap = new Proxy(function(value) { return value; }, {apply: null});
    "#,
        )
        .expect("initialize trap cases");
    let error = state
        .call_global("badTrap", &[])
        .expect_err("non-callable trap");
    assert!(
        error.contains("TypeError") && error.contains("apply"),
        "{error}"
    );
    assert_eq!(state.host_result_roots_for_test(), 0);
    assert_eq!(
        state.call_global("nullTrap", &[JsValue::Number(42.0)]),
        Ok(JsValue::Number(42.0))
    );
    assert_eq!(state.host_result_roots_for_test(), 0);
}
