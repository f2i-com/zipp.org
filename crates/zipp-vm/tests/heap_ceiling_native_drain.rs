//! A native iterator drain must meet the heap ceiling with a catchable
//! RangeError while the request is still affordable.
//!
//! `Array.from`, spread over a user iterator, the collection constructors and
//! `Object.fromEntries` step an iterator to exhaustion inside ONE instruction
//! with the collector suspended for the scope, so every `{value, done}` result
//! they allocate stays live until the drain returns. The dispatch-stride poll
//! never runs meanwhile, and the eager-result cap alone permits four million
//! results -- more than a gigabyte of them. `Array.from('ab'.repeat(3e6))`
//! reached 1.6 GB natively before its cap fired, and trapped the WebAssembly
//! build at its linked memory maximum. The drains now re-check the ceiling
//! every 1,024 steps.

#![cfg(feature = "safe-sandbox")]

use zipp_vm::embed::{self, ScriptState};

const CEILING: usize = 256 << 20;

fn slot(state: &ScriptState, name: &str) -> u32 {
    state
        .symbols()
        .into_iter()
        .find(|symbol| symbol.name == name)
        .unwrap_or_else(|| panic!("missing global function {name}"))
        .index
}

/// Run `body` once under the ceiling; it must be convicted by the heap
/// ceiling (not by a different limit) without the estimate running away.
fn assert_drain_is_convicted_under_the_ceiling(body: &str) {
    let source = format!("let held = []\nfunction run() {{ {body}; return held.length }}\n");
    let mut state = embed::compile_script(&source).expect("script compiles");
    state.disable_vm_jit();
    state.run_init().expect("script initializes");
    state.set_limits(u64::MAX, None);
    state.set_heap_limit(CEILING);
    let run = slot(&state, "run");
    let result = state.call_slot(run, &[]);
    let peak = state.heap_bytes();
    match result {
        Err(message) => assert!(
            message.contains("memory budget"),
            "{body}: expected the heap ceiling to convict, got: {message}"
        ),
        Ok(value) => panic!("{body}: completed with {value:?} at {peak} bytes"),
    }
    assert_eq!(
        state.resource_limit_error(),
        Some("RangeError: script exceeded its memory budget"),
        "{body}"
    );
    // One poll interval of results past the ceiling is the most a drain may
    // add before the check runs; the unchecked drain went past six times it.
    assert!(
        peak <= CEILING + CEILING / 2,
        "{body}: the drain ran away to {peak} bytes under a {CEILING}-byte ceiling"
    );
}

#[test]
fn array_from_a_string_iterator_stops_at_the_ceiling() {
    assert_drain_is_convicted_under_the_ceiling("held.push(Array.from('ab'.repeat(3000000)))");
}

#[test]
fn set_constructor_drain_stops_at_the_ceiling() {
    assert_drain_is_convicted_under_the_ceiling("held.push(new Set('ab'.repeat(3000000)))");
}

#[test]
fn map_constructor_drain_stops_at_the_ceiling() {
    assert_drain_is_convicted_under_the_ceiling(
        "held.push(new Map(function* () { for (let i = 0; i < 3000000; i++) yield [i, i]; }()))",
    );
}

#[test]
fn object_from_entries_drain_stops_at_the_ceiling() {
    assert_drain_is_convicted_under_the_ceiling(
        "held.push(Object.fromEntries(function* () { for (let i = 0; i < 3000000; i++) yield [i, i]; }()))",
    );
}

#[test]
fn spread_of_a_user_iterator_stops_at_the_ceiling() {
    assert_drain_is_convicted_under_the_ceiling(
        "held.push([...function* () { for (let i = 0; i < 3000000; i++) yield [i]; }()])",
    );
}
