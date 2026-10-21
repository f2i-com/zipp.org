//! `ScriptState::run_script` runs a classic script, as a `<script>` element
//! does (#79): ScriptEvaluation with GlobalDeclarationInstantiation, the
//! `$262.evalScript` path, so a script's declarations reach the scripts after
//! it. `eval_in_context` keeps its indirect-eval semantics.

use zipp_vm::embed::{compile_script, JsValue, ScriptState};

fn state() -> ScriptState {
    let mut st = compile_script("").expect("compiles");
    st.run_init().expect("runs");
    st
}

fn text(s: &str) -> Result<JsValue, String> {
    Ok(JsValue::String(s.into()))
}

#[test]
fn a_scripts_declarations_reach_later_scripts() {
    let mut st = state();
    st.run_script("'use strict'; var v = 1; function f() { return 2; } let l = 3; const c = 4; class K {}")
        .expect("the first script runs");
    assert_eq!(
        st.run_script("[typeof v, typeof f, typeof l, typeof c, typeof K].join(' ')"),
        text("number function number number function")
    );
    assert_eq!(st.eval_in_context("l + c + f()"), Ok(JsValue::Number(9.0)));
    assert_eq!(
        st.run_script("[Object.hasOwn(globalThis, 'v'), Object.hasOwn(globalThis, 'f'), Object.hasOwn(globalThis, 'l')].join()"),
        text("true,true,false")
    );
}

#[test]
fn the_completion_value_and_a_throw() {
    let mut st = state();
    assert_eq!(st.run_script("1 + 1"), Ok(JsValue::Number(2.0)));
    assert_eq!(st.run_script("var z = 5;"), Ok(JsValue::Undefined));
    let err = st.run_script("throw new TypeError('nope')").expect_err("the throw is the result");
    assert!(err.contains("TypeError: nope"), "{err}");
    let err = st.run_script("let x = ;").expect_err("a syntax error");
    assert!(err.contains("SyntaxError"), "{err}");
}

#[test]
fn a_redeclaration_is_a_syntax_error_that_runs_nothing() {
    let mut st = state();
    st.run_script("let l = 1; var w = 1;").expect("runs");
    let err = st.run_script("var ran = true; let l = 2;").expect_err("l is already declared");
    assert!(err.contains("SyntaxError"), "{err}");
    let err = st.run_script("var ran = true; let w = 2;").expect_err("w is a var");
    assert!(err.contains("SyntaxError"), "{err}");
    assert_eq!(st.run_script("typeof ran"), text("undefined"));
    st.run_script("const c = 1;").expect("runs");
    let err = st.run_script("c = 2;").expect_err("c is const");
    assert!(err.contains("TypeError"), "{err}");
    assert_eq!(st.run_script("l + c"), Ok(JsValue::Number(2.0)));
}

#[test]
fn eval_in_context_keeps_indirect_eval_semantics() {
    let mut st = state();
    st.eval_in_context("let e1 = 1; var e2 = 2; 'use strict';").expect("runs");
    st.eval_in_context("'use strict'; var e3 = 3;").expect("runs");
    assert_eq!(st.run_script("[typeof e1, typeof e2, typeof e3].join()"), text("undefined,number,undefined"));
}
