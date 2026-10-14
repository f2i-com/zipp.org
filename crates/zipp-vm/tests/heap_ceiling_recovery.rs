//! A heap ceiling bounds what is alive, so a VM that hit it can allocate again
//! once the script lets go (f2i-com/zipp.org#55).
//!
//! The ceiling used to be enforced against the heap payload's all-time peak:
//! after one script filled the heap and dropped everything, every later
//! allocation of any size failed with "script exceeded its memory budget",
//! across entries and even after the embedder re-armed the limits. It is now
//! enforced against the payload alive, a poll that finds the heap over asks
//! for a major collection before it convicts, and re-arming the ceiling
//! collects a heap that is past half of it.

#![cfg(feature = "instrument")]

use zipp_vm::embed::{compile_script, JsValue, ScriptState};

const CEILING: usize = 24 << 20;

fn arm(state: &mut ScriptState) {
    state.set_limits(u64::MAX, None);
    state.set_heap_limit(CEILING);
}

fn fresh() -> ScriptState {
    let mut state = compile_script("").expect("empty script compiles");
    state.run_init().expect("empty script runs");
    state
}

#[test]
fn a_vm_that_hit_its_heap_ceiling_allocates_again_after_dropping_everything() {
    let mut state = fresh();
    arm(&mut state);
    let fill = state.eval_in_context(
        "var keep = []; for (let i = 0; i < 2e6; i++) keep.push({ a: ['x' + i, 'y' + i], n: i }); keep.length",
    );
    assert!(
        fill.as_ref().is_err_and(|e| e.contains("memory budget")),
        "the fill should exhaust the ceiling, got {fill:?}"
    );
    arm(&mut state);
    assert_eq!(state.eval_in_context("keep = null; 0"), Ok(JsValue::Number(0.0)));
    for round in 0..3 {
        arm(&mut state);
        let r = state.eval_in_context(
            "(function(){ let a = []; for (let i = 0; i < 20000; i++) a.push({ i, s: 'v' + i }); return a.length })()",
        );
        assert_eq!(r, Ok(JsValue::Number(20000.0)), "re-allocation round {round}");
    }
}

#[test]
fn garbage_does_not_count_against_the_ceiling_within_one_run() {
    // Each phase holds well under the ceiling, but together they exceed it:
    // only the collection the conviction now waits for lets the second phase
    // fit in the space the first one dropped.
    let mut state = fresh();
    arm(&mut state);
    let r = state.eval_in_context(
        "function phase(tag) { let a = []; for (let i = 0; i < 30000; i++) a.push({ tag, s: 'p' + i }); return a.length }\n\
         let total = 0;\n\
         for (let round = 0; round < 8; round++) total += phase(round);\n\
         total",
    );
    assert_eq!(r, Ok(JsValue::Number(240000.0)));
}

#[test]
fn a_heap_that_stays_over_the_ceiling_is_still_convicted() {
    let mut state = fresh();
    arm(&mut state);
    let r = state.eval_in_context(
        "var held = []; for (let i = 0; i < 2e6; i++) held.push({ s: 'h' + i }); held.length",
    );
    assert!(
        r.as_ref().is_err_and(|e| e.contains("memory budget")),
        "a live heap past the ceiling must still be convicted, got {r:?}"
    );
    // Still held: re-arming collects nothing, and the next growth is refused.
    arm(&mut state);
    let again = state.eval_in_context(
        "for (let i = 0; i < 2e6; i++) held.push({ s: 'more' + i }); held.length",
    );
    assert!(
        again.as_ref().is_err_and(|e| e.contains("memory budget")),
        "growing a heap still at the ceiling must be refused, got {again:?}"
    );
}
