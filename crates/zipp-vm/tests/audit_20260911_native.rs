//! The 11 September 2026 audit's ZIPP-03: name-based re-entry through the
//! native embedding API must not compile anything.
//!
//! `call_global` used to resolve its callee by evaluating the name as a fresh
//! program, and `has_global_function` evaluated a generated `typeof` source,
//! so every name call and every optional-entry probe spent the dynamic-code
//! allowance and interned a program for the VM's lifetime. These tests pin the
//! compile-free contract with the smallest allowance the recorder accepts: a
//! single dynamic compilation for the whole lifetime. The slot call is the
//! control the audit supplied; the last test proves the allowance is live, so
//! the zero the others observe is a real zero and not a disabled counter.
//!
//! Requires `instrument` (the recorder that meters dynamic code):
//!
//! ```sh
//! cargo test --locked -p zipp-vm --features instrument --test audit_20260911_native
//! ```
#![cfg(feature = "instrument")]

use zipp_vm::embed::{compile_script, HostValue, JsValue, ScriptState};

fn fixture(source: &str) -> ScriptState {
    let mut state = compile_script(source).expect("compile fixture");
    state.set_limits(1_000_000, None);
    // One dynamic compilation, ever.
    state.set_dynamic_code_limits(65_536, 16_777_216, 1, 16_384, 1_024);
    state.run_init().expect("initialize fixture");
    state
}

#[test]
fn audit_name_calls_do_not_spend_dynamic_compile_allowance() {
    let mut state = fixture("var count=0; function bump(){return ++count;}");
    for expected in 1..=32 {
        match state.call_global("bump", &[]).expect("call global") {
            JsValue::Number(value) => assert_eq!(value, expected as f64),
            other => panic!("unexpected result: {other:?}"),
        }
    }
    assert!(state.resource_limit_error().is_none());
}

#[test]
fn audit_callable_queries_do_not_compile_or_consume_dynamic_allowance() {
    let mut state = fixture("function optionalEntry(){return 7;}");
    for _ in 0..32 {
        assert!(state.has_global_function("optionalEntry"));
        assert!(!state.has_global_function("absentEntry"));
        assert!(state.has_global_function("parseInt"), "builtins resolve too");
    }
    assert!(state.resource_limit_error().is_none());
}

#[test]
fn audit_slot_call_is_the_compile_free_control() {
    let mut state = fixture("var count=0; function bump(){return ++count;}");
    let index = state
        .symbols()
        .into_iter()
        .find(|s| s.name == "bump")
        .expect("function slot")
        .index;
    for expected in 1..=32 {
        match state.call_slot(index, &[]).expect("call slot") {
            HostValue::Number(value) => assert_eq!(value, expected as f64),
            other => panic!("unexpected result: {other:?}"),
        }
    }
    assert!(state.resource_limit_error().is_none());
}

/// Reassignment, builtins, a lexical binding, a global-object property and a
/// throwing callee, all under the one-compile allowance: the lookup reads the
/// current binding every time and never caches a function value.
#[test]
fn audit_name_calls_follow_reassignment_without_compiling() {
    let mut state = fixture(
        "var hits = 0; function entry(){ hits++; return 'first'; } \
         let lexicalEntry = function(){ return 'lexical'; }; \
         globalThis.ownEntry = function(){ return 'own'; }; \
         function retarget(){ entry = function(){ return 'second'; }; } \
         function boom(){ throw new TypeError('guest failure'); }",
    );
    assert_eq!(state.call_global("entry", &[]), Ok(JsValue::String("first".into())));
    state.call_global("retarget", &[]).expect("retarget");
    assert_eq!(state.call_global("entry", &[]), Ok(JsValue::String("second".into())));
    assert_eq!(
        state.call_global("lexicalEntry", &[]),
        Ok(JsValue::String("lexical".into()))
    );
    assert_eq!(
        state.call_global("ownEntry", &[]),
        Ok(JsValue::String("own".into()))
    );
    assert_eq!(
        state.call_global("isFinite", &[JsValue::Number(1.0)]),
        Ok(JsValue::Bool(true))
    );
    assert!(state
        .call_global("boom", &[])
        .expect_err("guest throw surfaces")
        .contains("guest failure"));
    assert_eq!(
        state.call_global("missing", &[]).expect_err("unbound name"),
        "ReferenceError: missing is not defined"
    );
    assert_eq!(
        state.call_global("hits", &[]).expect_err("not callable"),
        "TypeError: hits is not a function"
    );
    assert!(state.call_global("not an identifier", &[]).is_err());
    assert!(state.resource_limit_error().is_none());
}

/// The allowance the tests above observe is live: one compilation is
/// accepted and the second is refused, so "zero compilations" above is a
/// measured fact rather than a counter that never ran.
#[test]
fn audit_dynamic_allowance_is_live_so_zero_means_zero() {
    let mut state = fixture("var x = 1;");
    for _ in 0..8 {
        state.call_global("parseInt", &[JsValue::String("4".into())]).expect("builtin call");
        assert!(state.has_global_function("parseInt"));
    }
    assert!(state.resource_limit_error().is_none());
    assert_eq!(state.eval_in_context("x + 1"), Ok(JsValue::Number(2.0)));
    let second = state
        .eval_in_context("x + 2")
        .expect_err("the single dynamic compilation was already spent");
    assert!(second.contains("RangeError"), "{second}");
    assert!(
        state.resource_limit_error().is_some(),
        "the recorder reports the crossed ceiling"
    );
}
