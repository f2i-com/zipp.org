//! A global regex operation over a non-ASCII subject must not retain a copy of
//! the subject per match.
//!
//! The Annex B legacy statics (`leftContext`, `rightContext`, ...) used to be
//! materialised eagerly for non-ASCII subjects, so `"...".match(/\d+/g)` over a
//! 24 KB non-ASCII string allocated a thousand 72 KB slices inside one native
//! call -- 70 MB that the hardened profile's heap headroom convicted as
//! "regular expression exceeded its backtrack memory budget", while the ASCII
//! twin of the same program used a quarter of a megabyte. The statics are now
//! deferred for every subject and re-derived on the rare read.

#![cfg(feature = "safe-sandbox")]

use zipp_vm::embed::{self, HostValue, ScriptState};

const MIB: usize = 1024 * 1024;

fn slot(state: &ScriptState, name: &str) -> u32 {
    state
        .symbols()
        .into_iter()
        .find(|symbol| symbol.name == name)
        .unwrap_or_else(|| panic!("missing global function {name}"))
        .index
}

/// Run `function` under `headroom` bytes of heap above the settled baseline
/// and return its string result together with how far the estimate grew.
fn run_under_headroom(source: &str, function: &str, headroom: usize) -> (Result<String, String>, usize) {
    let mut state = embed::compile_script(source).expect("script compiles");
    state.disable_vm_jit();
    state.run_init().expect("script initializes");
    state.set_limits(50_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline + headroom);
    let result = state.call_slot(slot(&state, function), &[]);
    let grew = state.heap_bytes().saturating_sub(baseline);
    let result = match result {
        Ok(HostValue::String(s)) => Ok(s),
        Ok(other) => Ok(format!("{other:?}")),
        Err(message) => Err(message),
    };
    (result, grew)
}

// A 24,001-unit subject: one non-ASCII character, then a digit and 23 spaces
// a thousand times, so /\d+/g finds exactly 1,000 matches.
const NON_ASCII_SUBJECT: &str = r#""é" + ("1" + " ".repeat(23)).repeat(1000)"#;

#[test]
fn global_match_over_a_non_ascii_subject_fits_in_32_mib() {
    let source = format!(
        r#"
        var s = {NON_ASCII_SUBJECT};
        function run() {{
            var r = s.match(/\d+/g);
            return "len=" + s.length + " matches=" + r.length + " last=" + RegExp.lastMatch +
                " left=" + RegExp.leftContext.length + " right=" + RegExp.rightContext.length;
        }}
        "#
    );
    let (result, grew) = run_under_headroom(&source, "run", 32 * MIB);
    assert_eq!(
        result,
        Ok("len=24001 matches=1000 last=1 left=23977 right=23".to_string())
    );
    // The thousand matches used to grow the heap by 70 MB here.
    assert!(grew < 8 * MIB, "global match retained per-match subject copies: grew {grew} bytes");
}

#[test]
fn split_and_functional_replace_over_a_non_ascii_subject_fit_in_32_mib() {
    let source = format!(
        r#"
        var s = {NON_ASCII_SUBJECT};
        function split() {{ return "parts=" + s.split(/ /).length; }}
        function replace() {{
            var n = 0;
            var out = s.replace(/\d+/g, function (m) {{ n++; return "x"; }});
            return "calls=" + n + " out=" + out.length;
        }}
        "#
    );
    let (split, grew) = run_under_headroom(&source, "split", 32 * MIB);
    assert_eq!(split, Ok("parts=23001".to_string()));
    assert!(grew < 12 * MIB, "split retained per-match subject copies: grew {grew} bytes");
    let (replaced, grew) = run_under_headroom(&source, "replace", 32 * MIB);
    assert_eq!(replaced, Ok("calls=1000 out=24001".to_string()));
    assert!(grew < 8 * MIB, "replace retained per-match subject copies: grew {grew} bytes");
}
