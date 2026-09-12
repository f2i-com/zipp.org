//! Consuming a guest exception at an embedding boundary also consumes the
//! VM's pending exception. A swallowed conversion/callback failure must not
//! become the next independent call's exception.

use zipp_vm::embed::{compile_script, JsValue, ScriptState};

const SOURCE: &str = r#"
    function loops() {
        var sum = 0;
        for (var i = 0; i < 1000; i++) sum += i;
        return sum;
    }
    function rendered() {
        return { toString() { throw new Error('render failure'); } };
    }
    function throws() { throw new Error('callback failure'); }
    function numeric() { return Symbol('not a number'); }
    function numericBigInt() { return 1n; }
    function go() { return __zippHostCall('probe'); }
"#;

fn fixture() -> ScriptState {
    let mut state = compile_script(SOURCE).expect("compile fixture");
    state.run_init().expect("initialize fixture");
    // Compile the healthy loop before any exception is raised, so the native
    // pending-throw check is exercised deterministically on the later call.
    assert_eq!(
        state.call_global("loops", &[]),
        Ok(JsValue::Number(499500.0))
    );
    state
}

#[test]
fn swallowed_result_rendering_throw_does_not_poison_the_next_call() {
    let mut state = fixture();
    for _ in 0..3 {
        assert_eq!(
            state.call_global("rendered", &[]),
            Ok(JsValue::Object(String::new()))
        );
        assert_eq!(
            state.call_global("loops", &[]),
            Ok(JsValue::Number(499500.0))
        );
    }
}

#[test]
fn swallowed_eval_result_rendering_throw_does_not_poison_the_next_call() {
    let mut state = fixture();
    assert_eq!(
        state.eval_in_context("rendered()"),
        Ok(JsValue::Object(String::new()))
    );
    assert_eq!(
        state.call_global("loops", &[]),
        Ok(JsValue::Number(499500.0))
    );
}

#[test]
fn host_can_handle_a_guest_callback_throw_and_call_again() {
    let mut state = fixture();
    state.set_host_call_ctx(Box::new(|ctx, _, _| {
        let error = ctx
            .call_global_numbers("throws", &[])
            .expect_err("callback throws");
        assert!(error.contains("callback failure"), "{error}");
        let value = ctx.call_global_numbers("loops", &[])?;
        Ok(value.to_string())
    }));
    assert_eq!(
        state.call_global("go", &[]),
        Ok(JsValue::String("499500".into()))
    );
    assert_eq!(
        state.call_global("loops", &[]),
        Ok(JsValue::Number(499500.0))
    );
}

#[test]
fn host_can_handle_a_callback_number_conversion_throw_and_call_again() {
    let mut state = fixture();
    state.set_host_call_ctx(Box::new(|ctx, _, _| {
        let error = ctx
            .call_global_numbers("numeric", &[])
            .expect_err("Symbol is not numeric");
        assert!(error.contains("Symbol"), "{error}");
        let error = ctx
            .call_global_numbers("numericBigInt", &[])
            .expect_err("BigInt is not a Number result");
        assert!(error.contains("BigInt"), "{error}");
        let value = ctx.call_global_numbers("loops", &[])?;
        Ok(value.to_string())
    }));
    assert_eq!(
        state.call_global("go", &[]),
        Ok(JsValue::String("499500".into()))
    );
    assert_eq!(
        state.call_global("loops", &[]),
        Ok(JsValue::Number(499500.0))
    );
}
