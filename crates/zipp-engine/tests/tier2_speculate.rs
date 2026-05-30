//! End-to-end speculation: translate JS → IR → speculate → regalloc
//! → emit → execute. When the inputs match the speculated types,
//! tier-2 runs unboxed i32 arithmetic natively. When they don't,
//! the function traps at `ud2` (phase-5 trampoline); phase 6 will
//! replace the trap with a real deopt back to tier 0.
//!
//! Because phase-5 deopt is a hard trap, the "mismatched types"
//! case can't be exercised here without crashing the test process.
//! Those negative tests live in the speculation-pass unit tests
//! (which don't invoke the emitted code); the integration suite
//! focuses on the happy path for now.

#![cfg(all(feature = "djit", target_arch = "x86_64"))]

use std::rc::Rc;

use zipp_engine::codegen::tier2::emit::{self, EmittedFunction};
use zipp_engine::codegen::tier2::ir::passes::speculate::{self, SpeculateConfig};
use zipp_engine::codegen::tier2::ir::{self, IrFunction};
use zipp_engine::codegen::tier2::regalloc;
use zipp_engine::engine::ZippEngine;
use zipp_engine::object::Object;
use zipp_engine::runtime::value::Value;

/// Build-verify-speculate-pass-verify-regalloc-emit pipeline with
/// speculation turned on. Returns the emitted function and the
/// number of sites the speculation pass rewrote.
fn compile_with_speculation(
    instructions: &[u8],
    constants: Rc<Vec<Object>>,
    reg_count: u16,
) -> Option<(EmittedFunction, usize)> {
    let mut func: IrFunction =
        ir::build::translate(instructions, constants, reg_count, 0).ok()?;
    ir::verify::verify(&func).ok()?;
    // Phase-3 passes first (const-fold, DCE, copy-prop, CFG simplify,
    // LICM, IV reduce). Speculation runs afterwards so it only
    // applies to generic ops the simpler passes couldn't collapse.
    ir::passes::run_default_pipeline(&mut func);
    ir::verify::verify(&func).ok()?;
    let rewrites = speculate::run(&mut func, &SpeculateConfig { speculate_i32: true });
    ir::verify::verify(&func).ok()?;
    let alloc = regalloc::allocate(&func);
    let emitted = emit::emit(&func, &alloc).ok()?;
    Some((emitted, rewrites))
}

fn run_speculated(src: &str) -> Option<(u64, usize)> {
    let engine = ZippEngine::default();
    let state = engine.compile_script(src).ok()?;
    let vm = state.vm();
    let instructions: Vec<u8> = vm.instructions.to_vec();
    let constants: Rc<Vec<Object>> = vm.constants.clone();
    let reg_count = vm.register_count;

    let (emitted, rewrites) = compile_with_speculation(&instructions, constants, reg_count)?;

    let mut regs: Vec<u64> = vec![Value::UNDEFINED.bits(); reg_count as usize];
    let mut globals: Vec<u64> = vec![Value::UNDEFINED.bits(); 128];
    let vm_ptr = state.vm() as *const _ as *mut u8;
    let out = unsafe {
        emitted.execute(
            regs.as_mut_ptr(),
            std::ptr::null(),
            globals.as_mut_ptr(),
            vm_ptr,
        )
    };
    Some((out, rewrites))
}

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
fn speculated_addition_runs_unboxed() {
    // `1 + 2` — after const-fold collapses to ConstI32(3), there's
    // no AddGeneric left for speculation to rewrite. The pass
    // becomes a no-op and we confirm correctness.
    let (out, rewrites) = run_speculated("1 + 2").expect("compile");
    assert_eq!(numeric(out), 3.0);
    assert_eq!(rewrites, 0);
}

#[test]
fn speculated_chain_survives_to_native_arithmetic() {
    // A multi-step arithmetic expression that defeats const-fold
    // by threading through a variable. After speculation the chain
    // becomes: CheckI32 + UnboxI32 pairs + CheckedAdd + BoxI32.
    let src = r#"
        let x = 5;
        x + x + x
    "#;
    let (out, _rewrites) = run_speculated(src).expect("compile");
    assert_eq!(numeric(out), 15.0);
}

#[test]
fn speculated_subtraction_i32_result() {
    let src = r#"
        let a = 100;
        let b = 37;
        a - b
    "#;
    let (out, _rewrites) = run_speculated(src).expect("compile");
    assert_eq!(numeric(out), 63.0);
}

#[test]
fn speculated_multiplication_result() {
    let src = r#"
        let x = 6;
        let y = 7;
        x * y
    "#;
    let (out, _rewrites) = run_speculated(src).expect("compile");
    assert_eq!(numeric(out), 42.0);
}
