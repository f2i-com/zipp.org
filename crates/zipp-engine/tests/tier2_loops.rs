//! Loop detection + LICM tests.
//!
//! Two angles:
//!
//! 1. **Unit tests on hand-built IR.** Verify the loop-finder
//!    identifies the right header/latch/body, and that LICM moves
//!    the specific ops we expect.
//! 2. **End-to-end differential.** Compile a JS loop, run the full
//!    pipeline (including LICM), assert the interpreter still
//!    produces the tier-0 result. Catches any semantics-breaking
//!    hoist.

use zipp_engine::codegen::tier2::ir::{
    build, eval::EvalSession, eval::EvalValue, loops, passes, print, verify,
    Block, BlockId, IrFunction, IrOp, Terminator, ValueId, ValueType,
};
use zipp_engine::engine::ZippEngine;
use zipp_engine::object::Object;

fn make_func(blocks: Vec<Block>) -> IrFunction {
    IrFunction {
        bytecode_len: 0,
        num_bytecode_regs: 0,
        num_parameters: 0,
        blocks,
        deopt_points: Vec::new(),
        constants: std::rc::Rc::new(Vec::new()),
    }
}

// ─── Loop detection ─────────────────────────────────────────────────────────

#[test]
fn find_simple_while_loop() {
    // bb0 → bb1(header) → branch(cond) → bb1 (latch) or bb2 (exit).
    // Classic structured while loop.
    let func = make_func(vec![
        Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![],
            term: Terminator::Jump(BlockId(1), vec![]),
        },
        Block {
            id: BlockId(1),
            params: vec![(ValueId(0), ValueType::Value)],
            ops: vec![(ValueId(1), IrOp::ConstBool(true))],
            term: Terminator::Branch {
                cond: ValueId(1),
                then_block: BlockId(1), // back-edge
                then_args: vec![ValueId(0)],
                else_block: BlockId(2),
                else_args: vec![ValueId(0)],
            },
        },
        Block {
            id: BlockId(2),
            params: vec![(ValueId(2), ValueType::Value)],
            ops: vec![],
            term: Terminator::Return(Some(ValueId(2))),
        },
    ]);
    let ls = loops::find_loops(&func);
    assert_eq!(ls.len(), 1);
    assert_eq!(ls[0].header, BlockId(1));
    assert_eq!(ls[0].latch, BlockId(1));
    assert!(ls[0].contains(BlockId(1)));
}

#[test]
fn find_for_loop_separate_latch() {
    // bb0 → bb1(header) → branch → bb2 (body/latch) → bb1,
    //                              → bb3 (exit).
    let func = make_func(vec![
        Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![(ValueId(100), IrOp::ConstI32(0))],
            term: Terminator::Jump(BlockId(1), vec![ValueId(100)]),
        },
        Block {
            id: BlockId(1),
            params: vec![(ValueId(0), ValueType::Value)],
            ops: vec![
                (ValueId(1), IrOp::ConstI32(10)),
                (ValueId(2), IrOp::LtI32(ValueId(0), ValueId(1))),
            ],
            term: Terminator::Branch {
                cond: ValueId(2),
                then_block: BlockId(2),
                then_args: vec![ValueId(0)],
                else_block: BlockId(3),
                else_args: vec![ValueId(0)],
            },
        },
        Block {
            id: BlockId(2),
            params: vec![(ValueId(3), ValueType::Value)],
            ops: vec![
                (ValueId(4), IrOp::ConstI32(1)),
                (ValueId(5), IrOp::AddI32(ValueId(3), ValueId(4))),
            ],
            term: Terminator::Jump(BlockId(1), vec![ValueId(5)]),
        },
        Block {
            id: BlockId(3),
            params: vec![(ValueId(6), ValueType::Value)],
            ops: vec![],
            term: Terminator::Return(Some(ValueId(6))),
        },
    ]);
    let ls = loops::find_loops(&func);
    assert_eq!(ls.len(), 1);
    assert_eq!(ls[0].header, BlockId(1));
    assert_eq!(ls[0].latch, BlockId(2));
    let body: std::collections::BTreeSet<_> = ls[0].body.iter().copied().collect();
    assert!(body.contains(&BlockId(1)));
    assert!(body.contains(&BlockId(2)));
    assert!(!body.contains(&BlockId(0)));
    assert!(!body.contains(&BlockId(3)));
}

#[test]
fn no_loops_in_straight_line() {
    let func = make_func(vec![Block {
        id: BlockId(0),
        params: vec![],
        ops: vec![(ValueId(0), IrOp::ConstI32(1))],
        term: Terminator::Return(Some(ValueId(0))),
    }]);
    assert!(loops::find_loops(&func).is_empty());
}

// ─── LICM ──────────────────────────────────────────────────────────────────

#[test]
fn licm_hoists_loop_invariant_const() {
    // bb0 (pre-header) → bb1 (header+latch) → loop or exit bb2.
    // Inside the loop body (bb1): v5 = mul v10 (outer), const_inside.
    // LICM should hoist the const into bb0.
    let func = make_func(vec![
        Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![(ValueId(10), IrOp::ConstI32(5))],
            term: Terminator::Jump(BlockId(1), vec![]),
        },
        Block {
            id: BlockId(1),
            params: vec![],
            ops: vec![
                // `const.i32 7` — pure, operands are all outside (none).
                (ValueId(11), IrOp::ConstI32(7)),
                // `mul v10, v11` — invariant (both from outside).
                (ValueId(12), IrOp::MulI32(ValueId(10), ValueId(11))),
                (ValueId(13), IrOp::ConstBool(false)),
            ],
            term: Terminator::Branch {
                cond: ValueId(13),
                then_block: BlockId(1),
                then_args: vec![],
                else_block: BlockId(2),
                else_args: vec![],
            },
        },
        Block {
            id: BlockId(2),
            params: vec![],
            ops: vec![],
            term: Terminator::Return(Some(ValueId(12))),
        },
    ]);
    let mut f = func;
    let changed = passes::licm::run(&mut f);
    assert!(changed, "expected LICM to hoist");

    // The ConstI32(7) and MulI32 should now live in bb0.
    let preheader_ops: Vec<_> = f.blocks[0].ops.iter().map(|(vid, _)| *vid).collect();
    assert!(
        preheader_ops.contains(&ValueId(11)),
        "expected ConstI32(7) in preheader; got {preheader_ops:?}\n{}",
        print::dump(&f)
    );
    assert!(
        preheader_ops.contains(&ValueId(12)),
        "expected MulI32 in preheader; got {preheader_ops:?}\n{}",
        print::dump(&f)
    );
    verify::verify(&f).unwrap_or_else(|e| panic!("post-LICM verify: {e}\n{}", print::dump(&f)));
}

#[test]
fn licm_leaves_side_effects_alone() {
    let func = make_func(vec![
        Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![(ValueId(10), IrOp::ConstI32(5))],
            term: Terminator::Jump(BlockId(1), vec![]),
        },
        Block {
            id: BlockId(1),
            params: vec![],
            ops: vec![
                // StoreGlobal is a side-effect — not pure, must stay.
                (ValueId(11), IrOp::StoreGlobal(0, ValueId(10))),
                (ValueId(13), IrOp::ConstBool(false)),
            ],
            term: Terminator::Branch {
                cond: ValueId(13),
                then_block: BlockId(1),
                then_args: vec![],
                else_block: BlockId(2),
                else_args: vec![],
            },
        },
        Block {
            id: BlockId(2),
            params: vec![],
            ops: vec![],
            term: Terminator::Return(None),
        },
    ]);
    let mut f = func;
    passes::licm::run(&mut f);
    let body_ops: Vec<_> = f.blocks[1].ops.iter().map(|(vid, _)| *vid).collect();
    assert!(
        body_ops.contains(&ValueId(11)),
        "StoreGlobal should not have been hoisted"
    );
}

#[test]
fn licm_respects_block_params() {
    // The loop header's block params (phi values) are loop-local;
    // anything computed from them is NOT invariant. LICM must leave
    // `add header_param, outer` inside the loop.
    let func = make_func(vec![
        Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(10), IrOp::ConstI32(1)),
                (ValueId(11), IrOp::ConstI32(0)),
            ],
            term: Terminator::Jump(BlockId(1), vec![ValueId(11)]),
        },
        Block {
            id: BlockId(1),
            // iv is the IV block param — not hoistable.
            params: vec![(ValueId(0), ValueType::Value)],
            ops: vec![
                // add IV + const_outside.
                // Depends on block param → must NOT be hoisted.
                (ValueId(12), IrOp::AddI32(ValueId(0), ValueId(10))),
                (ValueId(13), IrOp::ConstBool(false)),
            ],
            term: Terminator::Branch {
                cond: ValueId(13),
                then_block: BlockId(1),
                then_args: vec![ValueId(12)],
                else_block: BlockId(2),
                else_args: vec![ValueId(12)],
            },
        },
        Block {
            id: BlockId(2),
            params: vec![(ValueId(2), ValueType::Value)],
            ops: vec![],
            term: Terminator::Return(Some(ValueId(2))),
        },
    ]);
    let mut f = func;
    passes::licm::run(&mut f);
    let body_ops: Vec<_> = f.blocks[1].ops.iter().map(|(vid, _)| *vid).collect();
    assert!(
        body_ops.contains(&ValueId(12)),
        "body-dependent AddI32 must stay in body"
    );
}

// ─── End-to-end differential (pipeline including LICM) ─────────────────────

fn run_vm(src: &str) -> EvalValue {
    let engine = ZippEngine::default();
    let result = engine.eval(src).expect("vm eval");
    match result {
        Object::Integer(v) if v >= i32::MIN as i64 && v <= i32::MAX as i64 => {
            EvalValue::I32(v as i32)
        }
        Object::Integer(v) => EvalValue::F64(v as f64),
        Object::Float(f) => EvalValue::F64(f),
        Object::Boolean(b) => EvalValue::Bool(b),
        Object::Null => EvalValue::Null,
        Object::Undefined => EvalValue::Undef,
        other => panic!("unexpected shape: {other:?}"),
    }
}

fn run_ir_pipeline(src: &str) -> Option<EvalValue> {
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

fn values_equivalent(a: &EvalValue, b: &EvalValue) -> bool {
    match (a, b) {
        (EvalValue::I32(x), EvalValue::I32(y)) => x == y,
        (EvalValue::F64(x), EvalValue::F64(y)) => (x.is_nan() && y.is_nan()) || x == y,
        (EvalValue::I32(i), EvalValue::F64(f)) | (EvalValue::F64(f), EvalValue::I32(i)) => {
            (*i as f64) == *f
        }
        (EvalValue::Bool(x), EvalValue::Bool(y)) => x == y,
        (EvalValue::Null, EvalValue::Null) => true,
        (EvalValue::Undef, EvalValue::Undef) => true,
        _ => false,
    }
}

#[test]
fn pipeline_with_licm_sum_loop() {
    let src = "let s = 0; for (let i = 1; i <= 50; i = i + 1) { s = s + i; } s;";
    let vm_result = run_vm(src);
    let ir_result = run_ir_pipeline(src).expect("ir run");
    assert!(values_equivalent(&vm_result, &ir_result));
}

#[test]
fn pipeline_with_licm_product_loop() {
    let src = "let p = 1; for (let i = 1; i <= 6; i = i + 1) { p = p * i; } p;";
    let vm_result = run_vm(src);
    let ir_result = run_ir_pipeline(src).expect("ir run");
    assert!(values_equivalent(&vm_result, &ir_result));
}

#[test]
fn pipeline_with_licm_while_counter() {
    let src = "let n = 0; while (n < 100) { n = n + 1; } n;";
    let vm_result = run_vm(src);
    let ir_result = run_ir_pipeline(src).expect("ir run");
    assert!(values_equivalent(&vm_result, &ir_result));
}
