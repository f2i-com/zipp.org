//! Arrays that grow in place are collected when they are dropped.
//!
//! The collector schedules on what it allocates, and an Array filled by
//! `push` allocates nothing: its storage grows in place. A program that
//! refilled a few such arrays pass after pass (a tokenizer, in
//! `bench/real/parse-large-js.js`) never collected the old ones, and held
//! every pass at once: 106 MB where QuickJS holds 38. Under a heap ceiling
//! the meter's heap walk sees that growth and asks for a major once the heap
//! stands a step over the level the last major left.

#![cfg(feature = "instrument")]

use zipp_vm::embed::{compile_script, JsValue, ScriptState};

const PASSES: usize = 8;
/// Two arrays of 400,000 elements each pass: 4 MiB of values each at their
/// 512Ki capacity, so every pass drops 8 MiB of array storage.
const SCRIPT: &str = "var kinds, starts;\n\
     for (var pass = 0; pass < 8; pass++) {\n\
       kinds = []; starts = [];\n\
       for (var i = 0; i < 400000; i++) { kinds.push(i & 7); starts.push(i); }\n\
     }\n\
     kinds.length + starts.length";

fn armed() -> ScriptState {
    let mut state = compile_script("").expect("empty script compiles");
    state.run_init().expect("empty script runs");
    state.set_limits(u64::MAX, None);
    state.set_heap_limit(1 << 30);
    state
}

#[test]
fn refilled_arrays_do_not_accumulate_under_a_heap_ceiling() {
    let mut state = armed();
    assert_eq!(state.eval_in_context(SCRIPT), Ok(JsValue::Number(800000.0)));
    // Every pass held at once would be PASSES * 8 MiB of array storage alone.
    // Collected as they are dropped, the peak is a pass or two of it.
    let peak = state.heap_bytes();
    let all_passes = PASSES * (8 << 20);
    assert!(
        peak < all_passes / 2,
        "heap peaked at {} MiB; every pass held at once is {} MiB",
        peak >> 20,
        all_passes >> 20
    );
}

#[test]
fn a_growing_live_array_still_completes_and_keeps_its_values() {
    // One array that is never dropped grows past several steps: the majors
    // the walk asks for find it all live, and the step grows with the level,
    // so this costs a handful of collections and every value survives.
    let mut state = armed();
    let r = state.eval_in_context(
        "var big = []; for (var i = 0; i < 3000000; i++) big.push(i * 2);\n\
         var s = 0; for (var j = 0; j < big.length; j += 1000) s += big[j];\n\
         s + big.length",
    );
    let expected = (0..3_000_000u64).step_by(1000).map(|j| j * 2).sum::<u64>() + 3_000_000;
    assert_eq!(r, Ok(JsValue::Number(expected as f64)));
}
