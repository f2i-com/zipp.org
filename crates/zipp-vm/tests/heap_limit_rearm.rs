//! Re-arming the heap ceiling (`ScriptState::set_heap_limit`) collects the
//! heap only when the budget before it found the heap at its ceiling.
//!
//! It used to collect whenever the live heap was past half the new ceiling,
//! so a host that re-arms before every entry paid a full collection per
//! entry for as long as its live heap stayed there, with nothing exhausted:
//! 200 calls of an empty function took three seconds at 265 MB
//! (f2i-com/zipp.org#73). The collection after a conviction, which lets a VM
//! that filled its heap allocate again (f2i-com/zipp.org#55), stays; see
//! also tests/heap_ceiling_recovery.rs.
//!
//! The collector's counters are process-wide, so this binary holds one test.

#![cfg(feature = "instrument")]

use zipp_vm::embed::{compile_script, JsValue};

fn majors() -> u64 {
    zipp_vm::gc_nursery_stats().1
}

#[test]
fn re_arming_collects_only_after_the_heap_reached_its_ceiling() {
    let mut state = compile_script(
        "var keep = []; for (let i = 0; i < 1e5; i++) keep.push({ a: ['x' + i, i], n: i });\n\
         function f() { return 1; }\n\
         function fill() { for (let i = 0; i < 2e6; i++) keep.push({ s: 'h' + i }); return keep.length; }\n\
         function drop() { keep = null; return 0; }\n\
         function churn() { let a = []; for (let i = 0; i < 20000; i++) a.push({ i, s: 'v' + i }); return a.length; }",
    )
    .expect("script compiles");
    state.set_limits(u64::MAX, None);
    state.run_init().expect("script fills its heap");
    // A live heap past half the ceiling, with room left under it.
    let live = state.heap_bytes();
    let ceiling = live / 2 * 3;
    state.set_heap_limit(ceiling);

    // A host that re-arms before every entry, nothing exhausted: no
    // collection is due.
    let before = majors();
    for _ in 0..50 {
        assert!(state.renew_step_budget(u64::MAX));
        state.set_heap_limit(ceiling);
        assert_eq!(state.call_global("f", &[]), Ok(JsValue::Number(1.0)));
    }
    let rearm_majors = majors() - before;
    assert!(
        rearm_majors <= 2,
        "{rearm_majors} major collections for 50 re-armed calls at a {live}-byte heap"
    );

    // A budget that ends on the ceiling is followed by one collection when
    // the next one is armed...
    let filled = state.call_global("fill", &[]);
    assert!(
        filled.as_ref().is_err_and(|e| e.contains("memory budget")),
        "the fill should exhaust the ceiling, got {filled:?}"
    );
    let before = majors();
    state.set_limits(u64::MAX, None);
    state.set_heap_limit(ceiling);
    assert!(majors() > before, "re-arming after a conviction collects");

    // ...and the VM allocates again once the script lets go.
    assert_eq!(state.call_global("drop", &[]), Ok(JsValue::Number(0.0)));
    for round in 0..3 {
        state.set_limits(u64::MAX, None);
        state.set_heap_limit(ceiling);
        assert_eq!(
            state.call_global("churn", &[]),
            Ok(JsValue::Number(20000.0)),
            "re-allocation round {round}"
        );
    }
}
