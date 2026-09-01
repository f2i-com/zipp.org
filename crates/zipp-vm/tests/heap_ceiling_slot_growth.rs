//! A heap ceiling must be met with a catchable RangeError, never with an
//! allocation request the host cannot satisfy.
//!
//! The slot table and its parallel per-slot tables all double at the same
//! moment. A heap sitting just under its ceiling therefore used to ask the
//! allocator for a second copy of everything it held; in the WebAssembly build
//! that request reached the linked memory maximum and trapped with
//! `unreachable` before the ceiling check -- which runs AFTER an allocation --
//! ever saw it (`held.push([])` at four million live slots). With a ceiling
//! installed the tables now grow in small exact steps once doubling would
//! carry the resident estimate past it, so the estimate crosses the ceiling
//! gracefully and the guest gets its RangeError with the engine intact.
//!
//! Native memory cannot reproduce the trap, so this pins the mechanism the
//! trap depended on: the resident estimate must never pass the ceiling
//! before conviction, because the poll charges the next growth ahead of it.

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

#[test]
fn slot_table_growth_stops_at_the_ceiling_instead_of_doubling_past_it() {
    // Between what 2^20 slots cost in this profile (about 137 MiB across the
    // parallel tables plus the hoard array) and what their doubling costs, so
    // the tables reach the power-of-two boundary UNDER the ceiling and the
    // next growth step decides the outcome: a doubling lands near 260 MiB, a
    // gentle step (1/16 of the table, about 9 MiB) just past 160.
    const CEILING: usize = 160 << 20;
    let mut state = embed::compile_script(
        "let held = []\n\
         function hoard(n) { let i = 0; while (i < n) { held.push([]); i = i + 1 } return held.length }\n",
    )
    .expect("script compiles");
    state.disable_vm_jit();
    state.run_init().expect("script initializes");
    state.set_limits(u64::MAX, None);
    state.set_heap_limit(CEILING);
    let hoard = slot(&state, "hoard");

    let mut convicted = false;
    let mut peak = 0usize;
    let mut before_last_round = 0usize;
    for _ in 0..512 {
        state.renew_step_budget(u64::MAX);
        let settled = state.heap_bytes();
        let result = state.call_slot(hoard, &[HostValue::Number(65_536.0)]);
        peak = peak.max(state.heap_bytes());
        match result {
            Ok(_) => {
                before_last_round = settled.max(before_last_round);
                continue;
            }
            Err(message) => {
                assert!(
                    message.contains("memory budget"),
                    "expected the heap ceiling to convict, got: {message}"
                );
                assert_eq!(
                    state.resource_limit_error(),
                    Some("RangeError: script exceeded its memory budget")
                );
                convicted = true;
                break;
            }
        }
    }
    assert!(convicted, "the hoard never reached the {CEILING}-byte ceiling");

    // The ceiling charges the copy the next table growth would need, so the
    // resident estimate never passes the ceiling before the guest is
    // convicted. Before the fix the same hoard peaked at 203 MB against this
    // 160 MiB ceiling: the tables doubled on the push that crossed it.
    assert!(
        peak <= CEILING,
        "the resident estimate passed the ceiling before conviction: peak {peak} > {CEILING}          (last settled round {before_last_round})"
    );
}
