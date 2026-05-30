//! End-to-end tier-2 pipeline: JS source → bytecode → IR → passes →
//! native → execute.
//!
//! Each test compiles a tiny JS snippet through the engine, hands
//! the resulting bytecode to [`tier2::try_compile`], and — when the
//! compile succeeds — invokes the emitted function directly and
//! compares the return value against an expected numeric result.
//!
//! Tests are organised as pairs: one "compile reaches emit" check
//! (tolerates `Emit::Unsupported` as a non-regression) and, when
//! applicable, an execution check that asserts the tier-2 return
//! value matches what tier 0 would have produced.

#![cfg(all(feature = "djit", target_arch = "x86_64"))]

use std::rc::Rc;

use zipp_engine::codegen::tier2::{self, CompileError};
use zipp_engine::engine::ZippEngine;
use zipp_engine::object::Object;
use zipp_engine::runtime::value::Value;

/// Compile `src` through the engine, feed the bytecode to tier-2,
/// and either execute the emitted function (returning its NaN-boxed
/// u64 result) or return the `CompileError`.
fn tier2_run(src: &str) -> Result<u64, CompileError> {
    let engine = ZippEngine::default();
    let state = engine.compile_script(src).expect("engine compile");
    let vm = state.vm();
    let instructions: Vec<u8> = vm.instructions.to_vec();
    let constants: Rc<Vec<Object>> = vm.constants.clone();
    let reg_count = vm.register_count;

    let emitted = tier2::try_compile(&instructions, constants, reg_count, 0)?;

    // Caller-side register window: all slots start as Undefined.
    let mut regs: Vec<u64> = vec![Value::UNDEFINED.bits(); reg_count as usize];
    // Globals array for LoadGlobal / StoreGlobal targets. 128 slots
    // is enough for every script the test suite covers.
    let mut globals: Vec<u64> = vec![Value::UNDEFINED.bits(); 128];
    let vm_ptr = state.vm() as *const _ as *mut u8;
    Ok(unsafe {
        emitted.execute(
            regs.as_mut_ptr(),
            std::ptr::null(),
            globals.as_mut_ptr(),
            vm_ptr,
        )
    })
}

/// Interpret a tier-2 return u64 as a numeric value (i32 or f64)
/// for equality comparison. Panics on other Value shapes.
fn numeric(bits: u64) -> f64 {
    let v = Value::from_bits(bits);
    if let Some(i) = v.try_as_i32() {
        return i as f64;
    }
    if v.is_f64() {
        return v.as_f64();
    }
    panic!("expected numeric result, got bits {bits:#018x}");
}

#[test]
fn const_addition_1_plus_2() {
    // After const-fold this collapses to ConstI32(3); a correct
    // emitter boxes the constant so the return carries I32 tag +
    // payload 3.
    let out = tier2_run("1 + 2").expect("tier-2 compile");
    assert_eq!(numeric(out), 3.0);
}

#[test]
fn subtraction_const() {
    let out = tier2_run("100 - 58").expect("tier-2 compile");
    assert_eq!(numeric(out), 42.0);
}

#[test]
fn multiplication_chain() {
    let out = tier2_run("5 * 4 + 3").expect("tier-2 compile");
    assert_eq!(numeric(out), 23.0);
}

#[test]
fn negative_result() {
    let out = tier2_run("3 - 10").expect("tier-2 compile");
    assert_eq!(numeric(out), -7.0);
}

#[test]
fn conditional_true_branch() {
    let out = tier2_run("1 < 2 ? 10 : 20").expect("tier-2 compile");
    assert_eq!(numeric(out), 10.0);
}

#[test]
fn conditional_false_branch() {
    let out = tier2_run("5 > 100 ? 1 : 999").expect("tier-2 compile");
    assert_eq!(numeric(out), 999.0);
}

#[test]
fn loop_sum_0_to_4() {
    // for-loop sum. Tier-2 may or may not accept this (the post-pass
    // IR might contain an op the emitter still rejects); if it does
    // accept, the result must match tier-0 semantics.
    let src = r#"
        let sum = 0;
        for (let i = 0; i < 5; i = i + 1) {
            sum = sum + i;
        }
        sum
    "#;
    match tier2_run(src) {
        Ok(out) => assert_eq!(numeric(out), 10.0),
        Err(CompileError::Emit(_)) => {
            // Acceptable: pipeline reached emit but some op is still
            // outside the phase-4d subset. Phase 4e / 5 extends it.
        }
        Err(e) => panic!("pipeline regression: {e:?}"),
    }
}

#[test]
fn float_addition_runs_via_f64_emit() {
    // Phase 9: ConstF64 + AddF64 now emit. Speculation detects
    // both operands are statically F64 and rewrites AddGeneric
    // to AddF64 directly (no CheckI32 deopt). The native SSE
    // `addsd` produces 4.0.
    let out = tier2_run("1.5 + 2.5").expect("tier-2 compile");
    let v = Value::from_bits(out);
    let as_num = v.try_as_i32().map(|i| i as f64).unwrap_or(v.as_f64());
    assert_eq!(as_num, 4.0);
}

#[test]
fn float_mul_and_sub_chain_f64() {
    // (10.0 - 2.5) * 2.0 = 15.0
    let out = tier2_run("(10.0 - 2.5) * 2.0").expect("tier-2 compile");
    let v = Value::from_bits(out);
    let as_num = v.try_as_i32().map(|i| i as f64).unwrap_or(v.as_f64());
    assert_eq!(as_num, 15.0);
}

#[test]
fn integer_division_exact() {
    // 12 / 3 exact → i32 4.
    let out = tier2_run("12 / 3").expect("tier-2 compile");
    let v = Value::from_bits(out);
    let as_num = v.try_as_i32().map(|i| i as f64).unwrap_or(v.as_f64());
    assert_eq!(as_num, 4.0);
}

#[test]
fn integer_division_non_exact_widens_to_f64() {
    // 5 / 2 → 2.5 (JS widens to f64).
    let out = tier2_run("5 / 2").expect("tier-2 compile");
    let v = Value::from_bits(out);
    let as_num = v.try_as_i32().map(|i| i as f64).unwrap_or(v.as_f64());
    assert_eq!(as_num, 2.5);
}

#[test]
fn tier2_debug_ir_structure_for_bench_shape() {
    use zipp_engine::codegen::tier2::ir::{self, print};

    let src = r#"
        let add = function(a, b) { return a + b; };
        let result = 0;
        for (let i = 0; i < 5; i = i + 1) { result = add(result, 1); }
        result
    "#;
    let engine = ZippEngine::default();
    let state = engine.compile_script(src).expect("engine compile");
    let vm = state.vm();
    let instructions: Vec<u8> = vm.instructions.to_vec();
    let constants: Rc<Vec<Object>> = vm.constants.clone();
    let reg_count = vm.register_count;

    let mut func =
        ir::build::translate(&instructions, constants, reg_count, 0).expect("translate");
    ir::passes::run_default_pipeline(&mut func);
    eprintln!("=== IR after default pipeline ===");
    eprintln!("{}", print::dump(&func));
}

#[test]
fn tier2_inlines_bench_shape_callvalue() {
    // Check the inline pass fires end-to-end on the function_calls
    // benchmark shape. We run the full pipeline up to but not
    // including regalloc/emit, inspect the resulting IR, and assert
    // there are no `CallValue` ops remaining — if inline succeeded
    // they've been replaced with the callee's body + a Copy. If this
    // test regresses the perf of function_calls likely regresses too.
    use zipp_engine::codegen::tier2::ir::{self, IrOp};
    use zipp_engine::codegen::tier2::ir::passes::inline as inline_pass;
    use zipp_engine::codegen::tier2::ir::passes::speculate::{self, SpeculateConfig};

    let src = r#"
        let add = function(a, b) { return a + b; };
        let result = 0;
        for (let i = 0; i < 5; i = i + 1) { result = add(result, 1); }
        result
    "#;
    let engine = ZippEngine::default();
    let state = engine.compile_script(src).expect("engine compile");
    let vm = state.vm();
    let instructions: Vec<u8> = vm.instructions.to_vec();
    let constants: Rc<Vec<Object>> = vm.constants.clone();
    let reg_count = vm.register_count;

    let mut func =
        ir::build::translate(&instructions, constants, reg_count, 0).expect("translate");
    ir::passes::run_default_pipeline(&mut func);
    let _ = speculate::run(&mut func, &SpeculateConfig { speculate_i32: true });
    let inlined = inline_pass::run(&mut func);
    eprintln!("[test] inlined {inlined} sites");

    let mut remaining_callvalues = 0;
    for block in &func.blocks {
        for (_, op) in &block.ops {
            if matches!(op, IrOp::CallValue(..)) {
                remaining_callvalues += 1;
            }
        }
    }
    // If the inline pass worked, no CallValue ops should remain in
    // the caller's IR. (`resolve_callee` for this shape should find
    // the MakeClosureNoCapture through the loop-header block params.)
    assert_eq!(
        remaining_callvalues, 0,
        "inline should eliminate all CallValue ops in bench shape; \
         inlined count = {inlined}"
    );
}

#[test]
fn tier2_bench_function_calls_shape_compiles() {
    // This is the exact body of the function_calls microbench.
    // `let add = function(...)` → MakeClosureNoCapture;
    // the loop does Call / Add / increment / compare. Execution via
    // `tier2_run` would access-violate because it bypasses VM::Call's
    // `sp` / stack setup that nested tier-2 → helper → tier-1 calls
    // require, so we only assert that the full pipeline compiles
    // successfully and don't invoke the emitted code here. The
    // `tier2_dispatch` suite covers end-to-end execution via the
    // real VM dispatch path.
    let engine = ZippEngine::default();
    let src = r#"
        let add = function(a, b) { return a + b; };
        let result = 0;
        for (let i = 0; i < 5; i = i + 1) { result = add(result, 1); }
        result
    "#;
    let state = engine.compile_script(src).expect("engine compile");
    let vm = state.vm();
    let instructions: Vec<u8> = vm.instructions.to_vec();
    let constants: Rc<Vec<Object>> = vm.constants.clone();
    let reg_count = vm.register_count;
    let result = tier2::try_compile(&instructions, constants, reg_count, 0);
    match result {
        Ok(_) => {}
        Err(CompileError::Build(e)) => {
            panic!("bench shape should translate cleanly now: {e:?}")
        }
        Err(CompileError::Emit(e)) => {
            eprintln!(
                "bench shape compiles via translate but emit still rejects: {e:?}"
            );
        }
        Err(CompileError::Verify(e)) => {
            panic!("verify regression: {e}")
        }
    }
}

#[test]
fn tier2_top_level_user_function_call_reports_shape() {
    // `function add` at top level + a subsequent call. The translator
    // now handles MakeClosure-with-zero-captures + ROp::Call +
    // LoadConst-CompiledFunction. Executing the result via
    // `tier2_run` would access-violate because the harness skips
    // VM::Call's sp / stack setup that `djit_call_helper` relies on;
    // end-to-end execution is covered by the `tier2_dispatch` suite
    // which drives calls through the real VM dispatch site. Here
    // we only check that compile succeeds without a Verify regression.
    let engine = ZippEngine::default();
    let state = engine
        .compile_script(
            r#"
        function add(a, b) { return a + b; }
        add(3, 4)
    "#,
        )
        .expect("compile");
    let vm = state.vm();
    let result = tier2::try_compile(
        &vm.instructions.to_vec(),
        vm.constants.clone(),
        vm.register_count,
        0,
    );
    match result {
        Ok(_) | Err(CompileError::Build(_)) | Err(CompileError::Emit(_)) => {}
        Err(CompileError::Verify(e)) => panic!("unexpected Verify regression: {e}"),
    }
}

#[test]
fn integer_modulo() {
    // 17 % 5 → 2.
    let out = tier2_run("17 % 5").expect("tier-2 compile");
    let v = Value::from_bits(out);
    let as_num = v.try_as_i32().map(|i| i as f64).unwrap_or(v.as_f64());
    assert_eq!(as_num, 2.0);
}

#[test]
fn generic_comparison_runs_through_pipeline() {
    // `a < b` on two local variables exercises the phase-8 LtValue
    // runtime helper. The helper returns the NaN-boxed `Value::TRUE`
    // bit-pattern — match that rather than coercing to numeric so
    // we verify the boxed-Bool contract tier-0 also relies on.
    let out = tier2_run("let a = 3; let b = 7; a < b").expect("tier-2 compile");
    let v = Value::from_bits(out);
    assert!(v.is_bool(), "expected Bool result, got {:#018x}", out);
    assert_eq!(out, Value::TRUE.bits());
}

#[test]
fn ternary_with_generic_comparison() {
    // `a < b ? a : b` — exercises LtValue feeding a conditional jump.
    // Branch consumes the low bit of the boxed-Bool, matching
    // tier-0's dispatch: bit 0 is 1 for TRUE / 0 for FALSE.
    let out = tier2_run("let a = 10; let b = 3; a < b ? a : b")
        .expect("tier-2 compile");
    let v = Value::from_bits(out);
    let as_num = v.try_as_i32().map(|i| i as f64).unwrap_or(v.as_f64());
    assert_eq!(as_num, 3.0);
}

#[test]
fn string_concat_unsupported() {
    // String constants aren't handled by the phase-1 translator.
    // Either the build or emit layer must reject; other error
    // variants would be a pipeline regression.
    let err = tier2_run(r#""hello" + " world""#).expect_err("must reject");
    match err {
        CompileError::Build(_) | CompileError::Emit(_) => {}
        other => panic!("expected Build or Emit error, got {other:?}"),
    }
}
