//! Compiled code borrows the step budget in chunks (`codegen::meter`,
//! `NATIVE_CHUNK` = 2^20 steps). A native loop that outlasts its chunk with
//! budget to spare must go on (the interpreter resumes it, and the next
//! native entry borrows the next chunk), charging what the interpreter
//! charges; only the budget's real end stops it. Once a loop that merely
//! outran its first chunk failed with "exceeded its instruction budget"
//! after about a million steps of a billion-step budget.
#![cfg(feature = "instrument")]
use zipp_vm::embed::compile_script;

const LOOP: &str = "var s = 0; for (var i = 0; i < 3000000; i++) { s = (s + i) | 0; } s;";

fn run(src: &str, budget: u64, jit: bool) -> (Result<(), String>, u64) {
    let mut st = compile_script(src).unwrap();
    if !jit {
        st.disable_vm_jit();
    }
    st.set_limits(budget, None);
    let r = st.run_init().map(|_| ());
    (r, st.steps_used())
}

#[test]
fn a_native_loop_longer_than_a_chunk_finishes_within_its_budget() {
    let (interp, interp_steps) = run(LOOP, 1_000_000_000, false);
    assert!(interp.is_ok(), "{interp:?}");
    let (jit, jit_steps) = run(LOOP, 1_000_000_000, true);
    assert!(jit.is_ok(), "{jit:?} after {jit_steps} steps");
    // Each chunk boundary may charge one block twice (the block the native
    // run did not enter, which the interpreter then runs): a handful of
    // steps over tens of millions.
    assert!(
        jit_steps >= interp_steps && jit_steps - interp_steps < 1_000,
        "interpreter {interp_steps} steps, JIT {jit_steps}"
    );
}

#[test]
fn the_budget_still_ends_a_native_loop() {
    let spin = "var s = 0; for (var i = 0; i < 1e9; i++) { s = (s + i) | 0; } s;";
    for budget in [100_000u64, 5_000_000] {
        let (r, used) = run(spin, budget, true);
        assert!(
            r.as_ref().is_err_and(|e| e.contains("instruction budget")),
            "budget {budget}: {r:?}"
        );
        assert!(used <= budget + 64, "budget {budget}: charged {used}");
    }
}
