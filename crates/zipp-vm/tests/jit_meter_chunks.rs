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

/// Work nested inside a native run (a call the compiled loop makes into
/// the interpreter, or into another compiled function) spends the budget
/// the run has not: a budget smaller than one chunk is lent to the loop
/// whole, and the nested work once found nothing left and failed at its
/// first step, far inside the budget.
#[test]
fn work_nested_in_a_native_run_spends_the_rest_of_a_small_budget() {
    // Callees the loop calls through the interpreter (or a generator,
    // or a getter): each once failed after about 200 steps.
    let callees = [
        "function f(x) { return arguments.length + x; }",
        "function f(x) { try { if (x < 0) throw 1; return x | 0; } catch (e) { return 0; } }",
        "function* g(x) { yield x; } function f(x) { return g(x).next().value | 0; }",
        "function f(x) { return [x].map(function (v) { return v + 1; })[0] | 0; }",
        "var o = { get v() { return 3; } }; function f(x) { return (x + o.v) | 0; }",
        "function f(x) { return String(x).length; }",
    ];
    for callee in callees {
        let src = format!("{callee}
var s = 0; for (var i = 0; i < 20000; i++) {{ s = (s + f(i)) | 0; }} s;");
        let (interp, needed) = run(&src, 1_000_000_000, false);
        assert!(interp.is_ok(), "{interp:?}");
        assert!(needed < 1 << 20, "the program must fit in one chunk: {needed}");
        // Twice what the interpreter needs, still under one chunk: enough
        // for any tier (a guard bail over-charges at most a block).
        let (jit, used) = run(&src, 2 * needed, true);
        assert!(jit.is_ok(), "{callee}: {jit:?} after {used} of {} steps", 2 * needed);
        // And the budget still ends it when it is really too small.
        let (short, used) = run(&src, needed / 2, true);
        assert!(short.is_err_and(|e| e.contains("instruction budget")), "{callee}: {used}");
        assert!(used <= needed / 2 + 64, "{callee}: charged {used} of {}", needed / 2);
    }
}
