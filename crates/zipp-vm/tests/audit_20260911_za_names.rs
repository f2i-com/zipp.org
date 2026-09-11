//! The 11 September 2026 close audit's ZA-09: the native name entries
//! (`call_global`, `has_global_function`) accept every IdentifierName the
//! parser accepts. They validated names with `char::is_alphabetic` /
//! `is_alphanumeric`, which are not `ID_Start` / `ID_Continue`: a combining
//! mark, ZWNJ, ZWJ and `Other_ID_Start` characters were refused although the
//! script had defined functions by exactly those names. The validator is now
//! the lexer's. No name query compiles anything, as before.

use zipp_vm::embed::{compile_script, JsValue, ScriptState};

fn prepare(src: &str) -> ScriptState {
    let mut st = compile_script(src).expect("compiles");
    st.run_init().expect("runs");
    st
}

const NAMES: &[(&str, &str)] = &[
    ("a\u{0301}", "combining acute accent (ID_Continue, Mn)"),
    ("a\u{200C}", "ZWNJ (added to IdentifierPart by the grammar)"),
    ("a\u{200D}", "ZWJ (added to IdentifierPart by the grammar)"),
    ("\u{2118}", "SCRIPT CAPITAL P (Other_ID_Start, not Alphabetic)"),
    ("\u{1D49C}x", "MATHEMATICAL SCRIPT CAPITAL A (supplementary plane)"),
    ("ünïcödé", "Latin-1 letters"),
    ("$_ok1", "ASCII with the two extra start characters"),
    ("a\u{00B7}b", "MIDDLE DOT (Other_ID_Continue)"),
    ("\u{0AD0}", "GUJARATI OM (Other_ID_Start)"),
];

#[test]
fn every_parser_accepted_name_is_callable_by_name() {
    let mut src = String::new();
    for (i, (name, _)) in NAMES.iter().enumerate() {
        src.push_str(&format!("function {name}() {{ return {i}; }}\n"));
    }
    let mut st = prepare(&src);
    for (i, (name, what)) in NAMES.iter().enumerate() {
        assert!(st.has_global_function(name), "{what}: {name:?} not found");
        assert_eq!(
            st.call_global(name, &[]),
            Ok(JsValue::Number(i as f64)),
            "{what}: {name:?}"
        );
    }
}

#[test]
fn a_reassigned_binding_is_read_live() {
    let mut st = prepare(
        "var \u{2118} = function () { return 'first'; };
         function swap() { \u{2118} = function () { return 'second'; }; }",
    );
    assert_eq!(st.call_global("\u{2118}", &[]), Ok(JsValue::String("first".into())));
    st.call_global("swap", &[]).expect("swap");
    assert_eq!(st.call_global("\u{2118}", &[]), Ok(JsValue::String("second".into())));
}

#[test]
fn expressions_and_invalid_spellings_are_still_refused() {
    let mut st = prepare("function f() { return 1; } var o = { f: f };");
    for bad in [
        "", " f", "f ", "o.f", "f()", "f;", "1f", "f\u{0301}()", "\u{0301}a", "f\n", "f\u{0000}",
        "\\u0066", "f\u{2E2F}", "f\u{00B2}", "this", // a reserved word resolves to nothing
    ] {
        assert!(!st.has_global_function(bad), "accepted {bad:?}");
        assert!(st.call_global(bad, &[]).is_err(), "called {bad:?}");
    }
    // Still compile-free: the name entries never went near the parser.
    assert_eq!(st.call_global("f", &[]), Ok(JsValue::Number(1.0)));
}
