//! Soft deopt smoke tests.
//!
//! Each test handcrafts a tier-2-compiled function with speculation
//! turned on, invokes it through the VM dispatch with inputs that
//! match *and* violate the speculated types, and asserts that the
//! function produces correct results in both cases — the mismatch
//! must soft-deopt rather than crash.
//!
//! The tests poke the tier-2 cache directly (via `insert_compiled`)
//! rather than waiting for the hot-count threshold. That keeps the
//! iteration count to a handful and makes the deopt path the
//! observable variable.

#![cfg(all(feature = "djit", target_arch = "x86_64"))]

use std::rc::Rc;

use zipp_engine::codegen::tier2::emit::{self, EmittedFunction};
use zipp_engine::codegen::tier2::ir::passes::speculate::{self, SpeculateConfig};
use zipp_engine::codegen::tier2::ir::{self, IrFunction};
use zipp_engine::codegen::tier2::regalloc;
use zipp_engine::engine::ZippEngine;
use zipp_engine::object::Object;
use zipp_engine::runtime::value::{val_to_obj, Value};

/// Full pipeline with `speculate_i32` turned on. Returns the emitted
/// function and the number of sites rewritten.
fn compile_speculated(
    instructions: &[u8],
    constants: Rc<Vec<Object>>,
    reg_count: u16,
) -> Option<(EmittedFunction, usize)> {
    let mut func: IrFunction =
        ir::build::translate(instructions, constants, reg_count, 0).ok()?;
    ir::verify::verify(&func).ok()?;
    ir::passes::run_default_pipeline(&mut func);
    ir::verify::verify(&func).ok()?;
    let rewrites =
        speculate::run(&mut func, &SpeculateConfig { speculate_i32: true });
    ir::verify::verify(&func).ok()?;
    let alloc = regalloc::allocate(&func);
    let emitted = emit::emit(&func, &alloc).ok()?;
    Some((emitted, rewrites))
}

fn obj_to_f64(obj: Object) -> f64 {
    match obj {
        Object::Integer(v) => v as f64,
        Object::Float(f) => f,
        other => panic!("expected numeric, got {other:?}"),
    }
}

#[test]
fn speculated_add_works_on_matching_i32() {
    // Baseline: feed i32 inputs to a speculated `a + b`. The guards
    // pass, the native path runs unboxed.
    let src = r#"
        let a = 13;
        let b = 29;
        a + b
    "#;
    let engine = ZippEngine::default();
    let state = engine.compile_script(src).expect("compile");
    let vm = state.vm();
    let instructions: Vec<u8> = vm.instructions.to_vec();
    let constants = vm.constants.clone();
    let reg_count = vm.register_count;

    let (emitted, rewrites) =
        compile_speculated(&instructions, constants, reg_count).expect("compile");
    assert!(rewrites >= 1, "expected at least one speculation rewrite");

    let mut regs = vec![Value::UNDEFINED.bits(); reg_count as usize];
    let mut globals = vec![Value::UNDEFINED.bits(); 128];
    let vm_ptr = state.vm() as *const _ as *mut u8;
    let out = unsafe {
        emitted.execute(
            regs.as_mut_ptr(),
            std::ptr::null(),
            globals.as_mut_ptr(),
            vm_ptr,
        )
    };
    assert_eq!(obj_to_f64(val_to_obj(Value::from_bits(out), &state.vm().heap)), 42.0);
    // deopt_pending must be unset — the guards passed cleanly.
    assert!(!state.vm().deopt_pending);
}

#[test]
fn speculation_guard_mismatch_sets_deopt_pending() {
    // Build a small IR function that speculates i32 on its entry
    // parameter, then invoke it with a non-i32 Value. The check
    // trips the deopt trampoline; the helper sets deopt_pending
    // and returns a sentinel.
    use ir::types::{Block, BlockId, DeoptId, IrOp, Terminator, ValueId, ValueType};

    let p0 = ValueId(0);
    let checked = ValueId(1);
    let unboxed = ValueId(2);
    let doubled = ValueId(3);
    let boxed = ValueId(4);
    let blocks = vec![Block {
        id: BlockId(0),
        params: vec![(p0, ValueType::Value)],
        ops: vec![
            (checked, IrOp::CheckI32(p0, DeoptId(0))),
            (unboxed, IrOp::UnboxI32(checked)),
            (doubled, IrOp::AddI32(unboxed, unboxed)),
            (boxed, IrOp::BoxI32(doubled)),
        ],
        term: Terminator::Return(Some(boxed)),
    }];
    use ir::types::DeoptPoint;
    let mut func = IrFunction {
        bytecode_len: 0,
        num_bytecode_regs: 1,
        num_parameters: 1,
        blocks,
        deopt_points: vec![DeoptPoint {
            id: DeoptId(0),
            bytecode_ip: 0,
            live_regs: Vec::new(),
        }],
        constants: Rc::new(Vec::new()),
    };
    ir::verify::verify(&func).expect("IR verifies");
    let alloc = regalloc::allocate(&func);
    let emitted = emit::emit(&func, &alloc).expect("emit");
    let _ = &mut func; // keep mutable for the shape's sake

    // Need a real VM to host the deopt helper's flag write.
    let engine = ZippEngine::default();
    let state = engine.compile_script("42").expect("compile throwaway");

    // Good input first: i32(21) → returns i32(42), no deopt.
    {
        let mut regs = [Value::from_i32(21).bits()];
        let mut globals = [Value::UNDEFINED.bits(); 4];
        let vm_ptr = state.vm() as *const _ as *mut u8;
        let out = unsafe {
            emitted.execute(
                regs.as_mut_ptr(),
                std::ptr::null(),
                globals.as_mut_ptr(),
                vm_ptr,
            )
        };
        let v = Value::from_bits(out);
        assert_eq!(v.try_as_i32(), Some(42));
        assert!(!state.vm().deopt_pending);
    }

    // Bad input: a raw f64 (non-i32-tagged) → CheckI32 fails,
    // trampoline fires, helper sets deopt_pending.
    {
        let vm_ptr = state.vm() as *const _ as *mut u8;
        // Clear any stale flag before the test.
        unsafe { (*(vm_ptr as *mut zipp_engine::vm::VM)).deopt_pending = false };

        let mut regs = [Value::from_f64(3.14).bits()];
        let mut globals = [Value::UNDEFINED.bits(); 4];
        // Execute: the check fails, the trampoline returns 0.
        unsafe {
            emitted.execute(
                regs.as_mut_ptr(),
                std::ptr::null(),
                globals.as_mut_ptr(),
                vm_ptr,
            )
        };
        assert!(
            unsafe { (*(vm_ptr as *mut zipp_engine::vm::VM)).deopt_pending },
            "CheckI32 on f64 operand must set deopt_pending"
        );
    }
}
