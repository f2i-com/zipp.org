//! Tier-2 dispatch: verify that hot functions transparently
//! promote from tier-0 → tier-1 → tier-2 and produce identical
//! results throughout.
//!
//! Each test runs a small JS program that invokes a function many
//! times. Tier-1 promotes at call 4 (`DJIT_THRESHOLD`); tier-2
//! normally waits for call 100+ after that, but we lower
//! [`Tier2Jit::set_threshold`] to a handful so the test doesn't
//! need 100 warm-up iterations.

#![cfg(all(feature = "djit", target_arch = "x86_64"))]

use zipp_engine::engine::ZippEngine;
use zipp_engine::object::Object;
use zipp_engine::runtime::value::val_to_obj;

/// Compile `src` with a lowered tier-2 threshold, run it, and
/// return `(numeric_result, tier2_compiled_count)`. All tests
/// here produce numeric (integer or float) results; the helper
/// normalises both to f64 so comparisons ignore `Integer`
/// vs. `Float` distinctions (tier-2's i32-preservation fast path
/// matches tier-0 semantics, but overflow cases widen).
fn run_with_tier2(src: &str, tier2_threshold: u32) -> (f64, usize) {
    let engine = ZippEngine::default();
    let mut state = engine.compile_script(src).expect("compile");
    state.vm_mut().tier2.set_threshold(tier2_threshold);
    state.run_init().expect("run_init");
    let last = state
        .vm_mut()
        .last_popped
        .take()
        .expect("no last_popped");
    let obj = val_to_obj(last, &state.vm().heap);
    let tier2_count = state.vm().tier2.compiled_count();
    (obj_to_f64(obj), tier2_count)
}

/// Same as `run_with_tier2` but with the production default
/// threshold, so tier-2 ~never fires for few-iteration scripts.
fn run_default(src: &str) -> f64 {
    let engine = ZippEngine::default();
    let obj = engine.eval(src).expect("eval");
    obj_to_f64(obj)
}

fn obj_to_f64(obj: Object) -> f64 {
    match obj {
        Object::Integer(v) => v as f64,
        Object::Float(f) => f,
        other => panic!("expected numeric result, got {other:?}"),
    }
}

#[test]
fn hot_function_correctness_parity() {
    // `add(a, b)` called many times. With a low tier-2 threshold,
    // the second run_init fires tier-2 (the first one only
    // compiles tier-1, which is deferred until the next run).
    // Regardless of which tier ends up running the function, the
    // result must match tier-0.
    let src = r#"
        function add(a, b) { return a + b; }
        let x = 0;
        for (let i = 0; i < 50; i = i + 1) {
            x = add(x, 1);
        }
        x
    "#;
    let (result, _tier2_count) = run_with_tier2(src, 5);
    assert_eq!(result, 50.0);
}

#[test]
fn tier2_promotes_across_runs() {
    // Multi-run promotion: run_init once to let tier-1 compile
    // (it defers), reset, run again so tier-1 executes natively
    // and the tier-2 counter trips, reset, run once more so tier-2
    // is actually invoked. At the end the cache holds ≥1 entry.
    let src = r#"
        function add(a, b) { return a + b; }
        let x = 0;
        for (let i = 0; i < 50; i = i + 1) {
            x = add(x, 1);
        }
        x
    "#;
    let engine = ZippEngine::default();
    let mut state = engine.compile_script(src).expect("compile");
    state.vm_mut().tier2.set_threshold(3);

    let instructions = state.vm().instructions.clone();
    let constants = state.vm().constants.clone();
    let num_cache_slots = state.vm().register_count;
    let reg_count = state.vm().register_count;

    for _ in 0..3 {
        state.run_init().expect("run_init");
        // Reset ip/sp for the next top-level evaluation. The djit
        // / tier2 caches survive the reset so promotion makes
        // cross-run progress.
        state.vm_mut().reset_for_run(
            instructions.clone(),
            constants.clone(),
            num_cache_slots,
            0,
            reg_count,
        );
    }
    assert!(
        state.vm().tier2.compiled_count() >= 1,
        "expected tier-2 to compile at least one function across \
         multiple runs; got {}",
        state.vm().tier2.compiled_count()
    );
}

#[test]
fn tier2_result_matches_tier0_sum() {
    // Sum 1..=50 using a helper. Tier-2 handles both the helper
    // and the loop's own arithmetic.
    let src = r#"
        function add(a, b) { return a + b; }
        let s = 0;
        for (let i = 1; i <= 50; i = i + 1) {
            s = add(s, i);
        }
        s
    "#;
    let (result, _) = run_with_tier2(src, 3);
    assert_eq!(result, 1275.0); // 1 + 2 + ... + 50
}

#[test]
fn tier2_handles_multiplication_chain() {
    let src = r#"
        function mul3(a, b, c) { return a * b * c; }
        let total = 0;
        for (let i = 1; i <= 20; i = i + 1) {
            total = total + mul3(i, 2, 3);
        }
        total
    "#;
    let (result, _) = run_with_tier2(src, 3);
    // total = sum_{i=1..=20} (i * 6) = 6 * 210 = 1260.
    assert_eq!(result, 1260.0);
}

#[test]
fn tier2_respects_blacklist_for_unsupported_ops() {
    // A function that uses property access — tier-2 can't emit
    // GetProp yet (phase 5+). Compile fails → blacklist, and the
    // program keeps running correctly through tier-1 / tier-0.
    // (Uses `read` not `get` because `get`/`set` are reserved for
    // getter/setter syntax in the parser.)
    let src = r#"
        function read(o) { return o.x + o.y; }
        let obj = { x: 10, y: 20 };
        let s = 0;
        for (let i = 0; i < 30; i = i + 1) {
            s = s + read(obj);
        }
        s
    "#;
    let (tier2_result, _count) = run_with_tier2(src, 3);
    // Correctness regardless of which tier ran `read`: 30 * 30 = 900.
    assert_eq!(tier2_result, 900.0);
    // Parity with pure tier-0/tier-1.
    assert_eq!(tier2_result, run_default(src));
}

#[test]
fn adversarial_deopt_matches_tier0_parity() {
    // Function speculated on i32 operands, warmed up with i32 calls,
    // then fed a single f64 call. The tier-2 guard trips, the soft
    // deopt machinery takes over, tier-1 retries, and the program
    // still produces the same value tier-0 would have.
    //
    // Happens across three `run_init` calls because tier-1 and
    // tier-2 compiles defer to the next run to avoid mid-recursion
    // installation. By run 3 both tiers are live and the deopt path
    // runs for real.
    let src = r#"
        function add(a, b) { return a + b; }
        let sum = 0;
        for (let i = 0; i < 20; i = i + 1) {
            sum = add(sum, 1);
        }
        sum = add(sum, 0.5);
        sum
    "#;

    let engine = ZippEngine::default();
    let mut state = engine.compile_script(src).expect("compile");
    state.vm_mut().tier2.set_threshold(3);

    let instructions = state.vm().instructions.clone();
    let constants = state.vm().constants.clone();
    let reg_count = state.vm().register_count;

    let mut final_obj: Option<Object> = None;
    for _ in 0..3 {
        state.run_init().expect("run_init");
        let last = state.vm_mut().last_popped.take();
        if let Some(val) = last {
            final_obj = Some(val_to_obj(val, &state.vm().heap));
        }
        state.vm_mut().reset_for_run(
            instructions.clone(),
            constants.clone(),
            reg_count,
            0,
            reg_count,
        );
    }
    let result = obj_to_f64(final_obj.expect("last_popped on final run"));
    // 20 i32 additions (sum = 20) + 1 f64 addition (0.5) = 20.5.
    assert_eq!(result, 20.5);
    // Tier-0 parity: a fresh VM running the same script produces
    // the same numeric value.
    assert_eq!(run_default(src), 20.5);
}

#[test]
fn cold_function_never_triggers_tier2() {
    // One-shot program: every function runs at most a couple of
    // times. Default threshold (100) isn't reached.
    let src = r#"
        function once(n) { return n * 2; }
        once(21)
    "#;
    let engine = ZippEngine::default();
    let mut state = engine.compile_script(src).expect("compile");
    // Default threshold = 100: one call nowhere near it.
    assert_eq!(state.vm().tier2.compiled_count(), 0);
    state.run_init().expect("run_init");
    let last = state
        .vm_mut()
        .last_popped
        .take()
        .expect("no last_popped");
    let obj = val_to_obj(last, &state.vm().heap);
    assert_eq!(obj_to_f64(obj), 42.0);
    // Still 0: the single call didn't trip promotion.
    assert_eq!(state.vm().tier2.compiled_count(), 0);
}
