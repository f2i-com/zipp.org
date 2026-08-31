//! Safe-profile accounting and expansion limits for compiled RegExp programs.

#![cfg(feature = "safe-sandbox")]

use zipp_vm::embed::{self, HostValue, ScriptState};

fn slot(state: &ScriptState, name: &str) -> u32 {
    state
        .symbols()
        .into_iter()
        .find(|symbol| symbol.name == name)
        .unwrap_or_else(|| panic!("missing global function {name}"))
        .index
}

fn call_number(state: &mut ScriptState, slot: u32, value: f64) {
    state
        .call_slot(slot, &[HostValue::Number(value)])
        .expect("guest function succeeds");
}

#[test]
fn compiled_programs_and_ascii_twins_are_counted_once_per_shared_arc() {
    let mut state = embed::compile_script(
        r#"
        var keep = [];
        function add(n) {
            for (let i = 0; i < n; i++) {
                keep.push(new RegExp("\\p{RGI_Emoji}", "v"));
            }
            return keep.length;
        }
        function testAll() {
            for (let i = 0; i < keep.length; i++) keep[i].test("x");
            return keep.length;
        }
        "#,
    )
    .expect("script compiles");
    state.set_limits(20_000_000, None);
    state.set_heap_limit(128 * 1024 * 1024);
    state.run_init().expect("script initializes");

    let add = slot(&state, "add");
    let test_all = slot(&state, "testAll");
    let baseline = state.heap_bytes();

    call_number(&mut state, add, 1.0);
    let one_program = state.heap_bytes();
    let primary_growth = one_program.saturating_sub(baseline);
    assert!(
        primary_growth > 400 * 1024,
        "the 524 KiB RGI_Emoji program must be visible; growth was {primary_growth}"
    );

    // All constructors hit the same cache Arc. The audit scratch sees the heap
    // references and cache reference but must charge the compiled program once.
    call_number(&mut state, add, 63.0);
    let shared_clones = state.heap_bytes();
    let clone_growth = shared_clones.saturating_sub(one_program);
    assert!(
        clone_growth < 512 * 1024,
        "63 shared RegExp objects must not be charged as 63 compiled programs; growth was {clone_growth}"
    );

    state
        .call_slot(test_all, &[])
        .expect("ASCII searches build/share their byte-optimised twin");
    let with_twin = state.heap_bytes();
    let twin_growth = with_twin.saturating_sub(shared_clones);
    eprintln!(
        "VM RegExp growth: primary={primary_growth}, 63 shared clones={clone_growth}, shared ASCII twin={twin_growth}"
    );
    assert!(
        twin_growth > 400 * 1024 && twin_growth < 2 * 1024 * 1024,
        "one shared ASCII twin must be charged exactly once; growth was {twin_growth}"
    );
}

#[test]
fn repeated_unicode_string_property_expansion_is_catchable() {
    let result = zipp_vm::run(
        r#"
        try {
            new RegExp("\\p{RGI_Emoji}\\p{RGI_Emoji}", "v");
        } catch (error) {
            console.log(error instanceof SyntaxError,
                        String(error).includes("sandbox limit"));
        }
        "#,
    )
    .expect("script compiles");
    assert!(
        result.error.is_none(),
        "unexpected error: {:?}",
        result.error
    );
    assert_eq!(result.output, ["true true"]);
}

#[test]
fn aggregate_unique_programs_respect_the_embedder_heap_limit() {
    let mut state = embed::compile_script(
        r#"
        var keep = [];
        function compileUnique(n) {
            let prefixes = ["A", "B", "C", "D"];
            for (let i = 0; i < n; i++) {
                keep.push(new RegExp(prefixes[i] + "\\p{RGI_Emoji}", "v"));
            }
            return keep.length;
        }
        "#,
    )
    .expect("script compiles");
    state.set_limits(20_000_000, None);
    state.run_init().expect("script initializes");
    let compile_unique = slot(&state, "compileUnique");
    let baseline = state.heap_bytes();

    // Each unique UTF-16 program is about 1.0 MiB. Two fit; admission of the
    // third is rejected before its Arc/cache entry is retained.
    state.set_heap_limit(baseline + 2_500 * 1024);
    let call = state.call_slot(compile_unique, &[HostValue::Number(4.0)]);
    assert!(call.is_err(), "projected compiled-program growth must stop");
    let status = state
        .resource_limit_error()
        .expect("heap preflight must latch a typed resource failure");
    assert!(
        status.contains("memory budget"),
        "unexpected status: {status}"
    );
    let growth = state.heap_bytes().saturating_sub(baseline);
    assert!(
        growth > 2 * 1024 * 1024 && growth < 2_500 * 1024,
        "two programs should remain while the third is declined; growth was {growth}"
    );
}

#[test]
fn regexp_compile_reuses_the_program_that_passed_preflight() {
    let mut state = embed::compile_script(
        r#"
        var target = /seed/;
        function recompile() {
            target.compile("A\\p{RGI_Emoji}", "v");
            return target.source.length;
        }
        "#,
    )
    .expect("script compiles");
    state.set_limits(20_000_000, None);
    state.run_init().expect("script initializes");
    let recompile = slot(&state, "recompile");
    let baseline = state.heap_bytes();

    // The UnicodeSets matcher is about 1 MiB. `compile` historically rebuilt
    // an equivalent second program after the constructor path had preflighted
    // the first, so one successful call retained roughly 2 MiB and silently
    // crossed this ceiling. The receiver and constructor cache must share the
    // one admitted Arc instead.
    state.set_heap_limit(baseline + 1_500 * 1024);
    state
        .call_slot(recompile, &[])
        .expect("one preflighted program fits the budget");
    assert!(state.resource_limit_error().is_none());
    let growth = state.heap_bytes().saturating_sub(baseline);
    assert!(
        growth > 900 * 1024 && growth < 1_500 * 1024,
        "compile must retain one shared program, not a duplicate; growth was {growth}"
    );
}
