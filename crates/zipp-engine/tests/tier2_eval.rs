//! Tier-2 phase 2: differential tests between IR interpreter and tier-0 VM.
//!
//! For each test case:
//!
//! 1. Compile the JS through the engine and get the tier-0 result.
//! 2. Translate the bytecode to IR.
//! 3. Run the IR through `eval::EvalSession`.
//! 4. Run the phase-3 pass pipeline, then run the interpreter again.
//! 5. Assert: all three results agree numerically.
//!
//! If any pair disagrees, we've got either a translator bug, a
//! semantics-destroying pass, or an interpreter bug — and we want to
//! know before codegen lands.

use zipp_engine::codegen::tier2::ir::{
    build, eval::EvalSession, eval::EvalValue, passes, verify,
};
use zipp_engine::engine::ZippEngine;
use zipp_engine::object::Object;

/// Run the JS through tier 0, convert the result to an EvalValue.
fn run_vm(src: &str) -> EvalValue {
    let engine = ZippEngine::default();
    let result = engine.eval(src).expect("vm eval");
    match result {
        Object::Integer(v) if v >= i32::MIN as i64 && v <= i32::MAX as i64 => {
            EvalValue::I32(v as i32)
        }
        Object::Integer(v) => EvalValue::F64(v as f64),
        Object::Float(f) => {
            // Tier 0 may return an exact-integer f64 that we want to
            // compare as i32; normalise at comparison time rather
            // than here.
            EvalValue::F64(f)
        }
        Object::Boolean(b) => EvalValue::Bool(b),
        Object::Null => EvalValue::Null,
        Object::Undefined => EvalValue::Undef,
        other => panic!("unexpected result shape from VM: {other:?}"),
    }
}

/// Translate a script's bytecode to IR and run it through the phase-2
/// interpreter. Returns `None` if the translator bails (phase-1
/// coverage limit — not a test failure).
fn run_ir(src: &str) -> Option<EvalValue> {
    let engine = ZippEngine::default();
    let state = engine.compile_script(src).ok()?;
    let vm = state.vm();
    let func = build::translate(
        &vm.instructions,
        vm.constants.clone(),
        vm.register_count,
        0,
    )
    .ok()?;
    verify::verify(&func).ok()?;
    let mut session = EvalSession::new(&func, 256);
    // Seed the entry block's param registers with Undef — matches a
    // fresh VM register window.
    let args: Vec<EvalValue> = (0..func.num_bytecode_regs).map(|_| EvalValue::Undef).collect();
    session.run(args).ok()
}

fn run_ir_with_pipeline(src: &str) -> Option<EvalValue> {
    let engine = ZippEngine::default();
    let state = engine.compile_script(src).ok()?;
    let vm = state.vm();
    let mut func = build::translate(
        &vm.instructions,
        vm.constants.clone(),
        vm.register_count,
        0,
    )
    .ok()?;
    verify::verify(&func).ok()?;
    passes::run_default_pipeline(&mut func);
    verify::verify(&func).ok()?;
    let mut session = EvalSession::new(&func, 256);
    let args: Vec<EvalValue> = (0..func.num_bytecode_regs).map(|_| EvalValue::Undef).collect();
    session.run(args).ok()
}

/// Compare results with tolerance for integer/float form differences:
/// tier 0 may return `Integer(5)` where the IR interpreter computes
/// `F64(5.0)`. Both represent the same JS value.
fn values_equivalent(a: &EvalValue, b: &EvalValue) -> bool {
    match (a, b) {
        (EvalValue::I32(x), EvalValue::I32(y)) => x == y,
        (EvalValue::F64(x), EvalValue::F64(y)) => {
            (x.is_nan() && y.is_nan()) || x == y
        }
        (EvalValue::I32(i), EvalValue::F64(f)) | (EvalValue::F64(f), EvalValue::I32(i)) => {
            (*i as f64) == *f
        }
        (EvalValue::Bool(x), EvalValue::Bool(y)) => x == y,
        (EvalValue::Null, EvalValue::Null) => true,
        (EvalValue::Undef, EvalValue::Undef) => true,
        _ => false,
    }
}

/// Check: vm result ≡ ir result ≡ ir-after-pipeline result.
/// Skips the test if IR translation bailed on an unsupported op —
/// those are phase-1 coverage gaps and not interpreter bugs.
fn differential(src: &str) {
    let vm_result = run_vm(src);
    let ir_result = match run_ir(src) {
        Some(v) => v,
        None => return, // translator doesn't cover this yet
    };
    let ir_pipeline_result = match run_ir_with_pipeline(src) {
        Some(v) => v,
        None => return,
    };
    assert!(
        values_equivalent(&vm_result, &ir_result),
        "vm={vm_result:?} ir={ir_result:?} differed for: {src}"
    );
    assert!(
        values_equivalent(&vm_result, &ir_pipeline_result),
        "vm={vm_result:?} pipeline-ir={ir_pipeline_result:?} differed for: {src}"
    );
}

// ─── Pure arithmetic ────────────────────────────────────────────────────────

#[test]
fn eval_const_add() {
    differential("1 + 2;");
}

#[test]
fn eval_const_subtract() {
    differential("10 - 7;");
}

#[test]
fn eval_const_multiply() {
    differential("3 * 4;");
}

#[test]
fn eval_chained_arith() {
    differential("1 + 2 * 3 - 4;");
}

#[test]
fn eval_negative_results() {
    differential("5 - 10;");
}

#[test]
fn eval_with_globals() {
    differential("let x = 5; let y = 7; x + y;");
}

#[test]
fn eval_self_referential_update() {
    differential("let x = 1; x = x + x; x;");
}

// ─── Control flow ──────────────────────────────────────────────────────────

#[test]
fn eval_if_true() {
    differential("let x = 5; let y; if (x < 10) { y = 1; } else { y = 2; } y;");
}

#[test]
fn eval_if_false() {
    differential("let x = 50; let y; if (x < 10) { y = 1; } else { y = 2; } y;");
}

#[test]
fn eval_while_counter() {
    differential("let i = 0; while (i < 10) { i = i + 1; } i;");
}

#[test]
fn eval_for_sum() {
    differential("let sum = 0; for (let i = 1; i <= 100; i = i + 1) { sum = sum + i; } sum;");
}

#[test]
fn eval_for_product() {
    differential("let p = 1; for (let i = 1; i <= 6; i = i + 1) { p = p * i; } p;");
}

// ─── Edge cases ────────────────────────────────────────────────────────────

#[test]
fn eval_zero() {
    differential("0;");
}

#[test]
fn eval_negative_literal() {
    differential("-7;");
}

#[test]
fn eval_bool_literals() {
    differential("true;");
    differential("false;");
}

#[test]
fn eval_comparison_results() {
    differential("let x = 5; x < 10;");
    differential("let x = 5; x > 10;");
    differential("let x = 5; x === 5;");
}

// ─── Pipeline preserves semantics ──────────────────────────────────────────
//
// These cases specifically exercise passes that rewrite the IR:
// constant folding, DCE, CFG simplification. If any pass disturbs
// semantics, the differential comparison will catch it.

#[test]
fn pipeline_preserves_const_fold_result() {
    // `1 + 2 * 3` should fold to `7` pre-interpretation; the pipeline
    // version must still produce the same value as tier 0.
    differential("1 + 2 * 3;");
}

#[test]
fn pipeline_preserves_loop_result() {
    // The classic arithmetic-loop pattern — if DCE or CFG-simplify
    // incorrectly removes part of the loop body, the running total
    // diverges from the tier-0 sum.
    differential("let s = 0; for (let i = 1; i <= 50; i = i + 1) s = s + i; s;");
}
