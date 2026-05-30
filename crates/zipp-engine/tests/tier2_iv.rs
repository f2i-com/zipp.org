//! Induction variable detection + strength reduction tests.
//!
//! Two flavours:
//!
//! 1. **IV detection**: hand-build a canonical for-loop IR and assert
//!    the analyser recognises the counter as a BIV with the right
//!    init / step.
//! 2. **End-to-end differential**: compile a JS loop that performs
//!    `i * k` repeatedly, run the full pipeline (including the IV
//!    reducer), assert the interpreter still produces tier-0's
//!    result. This is the standing regression test: any semantic
//!    drift from strength reduction shows up immediately.

use zipp_engine::codegen::tier2::ir::{
    build, eval::EvalSession, eval::EvalValue, iv, loops, passes, print, verify, Block, BlockId,
    IrFunction, IrOp, Terminator, ValueId, ValueType,
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

// ─── IV detection ──────────────────────────────────────────────────────────

#[test]
fn detect_simple_counter_iv() {
    // Canonical for-loop shape:
    //   bb0: init = 0; jump bb1(init)
    //   bb1(i): cond = i < 10; branch(cond, bb2, bb3)
    //   bb2(i2): step = 1; i_next = i2 + step; jump bb1(i_next)
    //   bb3: return i
    let func = make_func(vec![
        Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![(ValueId(10), IrOp::ConstI32(0))],
            term: Terminator::Jump(BlockId(1), vec![ValueId(10)]),
        },
        Block {
            id: BlockId(1),
            params: vec![(ValueId(0), ValueType::Value)],
            ops: vec![
                (ValueId(20), IrOp::ConstI32(10)),
                (ValueId(21), IrOp::LtI32(ValueId(0), ValueId(20))),
            ],
            term: Terminator::Branch {
                cond: ValueId(21),
                then_block: BlockId(2),
                then_args: vec![ValueId(0)],
                else_block: BlockId(3),
                else_args: vec![ValueId(0)],
            },
        },
        Block {
            id: BlockId(2),
            params: vec![(ValueId(30), ValueType::Value)],
            ops: vec![
                (ValueId(40), IrOp::ConstI32(1)),
                (ValueId(41), IrOp::AddI32(ValueId(30), ValueId(40))),
            ],
            term: Terminator::Jump(BlockId(1), vec![ValueId(41)]),
        },
        Block {
            id: BlockId(3),
            params: vec![(ValueId(50), ValueType::Value)],
            ops: vec![],
            term: Terminator::Return(Some(ValueId(50))),
        },
    ]);

    let ls = loops::find_loops(&func);
    assert_eq!(ls.len(), 1);
    let preheader = iv::find_preheader(&func, &ls[0]).expect("preheader");
    assert_eq!(preheader, BlockId(0));

    let ivs = iv::find_basic_ivs(&func, &ls[0], preheader);
    assert_eq!(ivs.len(), 1, "expected 1 BIV; got {ivs:?}");
    let (phi, info) = ivs.iter().next().unwrap();
    assert_eq!(*phi, ValueId(0));
    assert_eq!(info.init, ValueId(10));
    assert_eq!(info.const_step, Some(1));
}

#[test]
fn detect_two_ivs_sum_and_counter() {
    // Two IVs in the same loop: sum (init=0, step variable=i) and
    // counter i (init=0, step=1). Our analyser should identify
    // counter i as a BIV but NOT sum — sum's step isn't loop-
    // invariant (it's `i`, which changes each iteration).
    let func = make_func(vec![
        Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![(ValueId(10), IrOp::ConstI32(0))],
            term: Terminator::Jump(BlockId(1), vec![ValueId(10), ValueId(10)]),
        },
        Block {
            id: BlockId(1),
            params: vec![
                (ValueId(0), ValueType::Value), // sum
                (ValueId(1), ValueType::Value), // i
            ],
            ops: vec![
                (ValueId(20), IrOp::ConstI32(10)),
                (ValueId(21), IrOp::LtI32(ValueId(1), ValueId(20))),
            ],
            term: Terminator::Branch {
                cond: ValueId(21),
                then_block: BlockId(2),
                then_args: vec![ValueId(0), ValueId(1)],
                else_block: BlockId(3),
                else_args: vec![ValueId(0), ValueId(1)],
            },
        },
        Block {
            id: BlockId(2),
            params: vec![
                (ValueId(30), ValueType::Value), // sum_in_body
                (ValueId(31), ValueType::Value), // i_in_body
            ],
            ops: vec![
                // sum += i (step is i — NOT loop-invariant)
                (ValueId(40), IrOp::AddI32(ValueId(30), ValueId(31))),
                // i += 1 (step 1 — loop-invariant)
                (ValueId(41), IrOp::ConstI32(1)),
                (ValueId(42), IrOp::AddI32(ValueId(31), ValueId(41))),
            ],
            term: Terminator::Jump(BlockId(1), vec![ValueId(40), ValueId(42)]),
        },
        Block {
            id: BlockId(3),
            params: vec![
                (ValueId(50), ValueType::Value),
                (ValueId(51), ValueType::Value),
            ],
            ops: vec![],
            term: Terminator::Return(Some(ValueId(50))),
        },
    ]);

    let ls = loops::find_loops(&func);
    let preheader = iv::find_preheader(&func, &ls[0]).unwrap();
    let ivs = iv::find_basic_ivs(&func, &ls[0], preheader);
    assert_eq!(ivs.len(), 1, "only counter should be detected; got {ivs:?}");
    assert!(ivs.contains_key(&ValueId(1)), "counter i must be a BIV");
    assert!(!ivs.contains_key(&ValueId(0)), "sum must NOT be a BIV");
}

// ─── Strength reduction ────────────────────────────────────────────────────

#[test]
fn strength_reduce_mul_by_const() {
    // Build: for (i=0; i<10; i++) { v = i * 7; ... }
    // After reduction, we expect an auxiliary IV initialised to 0 that
    // steps by 7 each iteration, replacing the multiply.
    let func = make_func(vec![
        Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![(ValueId(10), IrOp::ConstI32(0))],
            term: Terminator::Jump(BlockId(1), vec![ValueId(10)]),
        },
        Block {
            id: BlockId(1),
            params: vec![(ValueId(0), ValueType::Value)], // i
            ops: vec![
                (ValueId(20), IrOp::ConstI32(10)),
                (ValueId(21), IrOp::LtI32(ValueId(0), ValueId(20))),
            ],
            term: Terminator::Branch {
                cond: ValueId(21),
                then_block: BlockId(2),
                then_args: vec![ValueId(0)],
                else_block: BlockId(3),
                else_args: vec![ValueId(0)],
            },
        },
        Block {
            id: BlockId(2),
            params: vec![(ValueId(30), ValueType::Value)], // i_in_body
            ops: vec![
                // v = i * 7 — the strength-reduction candidate.
                (ValueId(100), IrOp::ConstI32(7)),
                (ValueId(101), IrOp::MulI32(ValueId(30), ValueId(100))),
                // Step.
                (ValueId(40), IrOp::ConstI32(1)),
                (ValueId(41), IrOp::AddI32(ValueId(30), ValueId(40))),
            ],
            term: Terminator::Jump(BlockId(1), vec![ValueId(41)]),
        },
        Block {
            id: BlockId(3),
            params: vec![(ValueId(50), ValueType::Value)],
            ops: vec![],
            term: Terminator::Return(Some(ValueId(50))),
        },
    ]);

    let mut f = func;
    let changed = passes::iv_reduce::run(&mut f);
    assert!(changed, "expected reduction; IR:\n{}", print::dump(&f));
    verify::verify(&f).unwrap_or_else(|e| panic!("post-reduce verify: {e}\n{}", print::dump(&f)));

    // The `MulI32(i, 7)` op should be gone from block 2. Its result
    // id (v101) should no longer appear as an op result anywhere.
    let any_mul_101 = f
        .blocks
        .iter()
        .any(|b| b.ops.iter().any(|(vid, _)| *vid == ValueId(101)));
    assert!(
        !any_mul_101,
        "multiply result should be removed; IR:\n{}",
        print::dump(&f)
    );

    // Header block should have gained a new param (the aux IV).
    assert_eq!(f.blocks[1].params.len(), 2, "header should have 2 params now");
}

// ─── End-to-end differential ───────────────────────────────────────────────

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
    let args: Vec<EvalValue> =
        (0..func.num_bytecode_regs).map(|_| EvalValue::Undef).collect();
    session.run(args).ok()
}

fn eq(a: &EvalValue, b: &EvalValue) -> bool {
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
fn pipeline_sum_i_times_three() {
    // `sum += i * 3` — i * 3 is the strength-reduction target.
    let src = "let sum = 0; for (let i = 0; i < 10; i = i + 1) { sum = sum + i * 3; } sum;";
    let vm_result = run_vm(src);
    let ir_result = run_ir_pipeline(src).expect("ir pipeline");
    assert!(
        eq(&vm_result, &ir_result),
        "vm={vm_result:?} ir={ir_result:?}"
    );
}

#[test]
fn pipeline_sum_seven_times_i() {
    // Same shape, factor on the other side of the multiply.
    let src = "let sum = 0; for (let i = 0; i < 20; i = i + 1) { sum = sum + 7 * i; } sum;";
    let vm_result = run_vm(src);
    let ir_result = run_ir_pipeline(src).expect("ir pipeline");
    assert!(
        eq(&vm_result, &ir_result),
        "vm={vm_result:?} ir={ir_result:?}"
    );
}

#[test]
fn pipeline_double_loop_with_mul() {
    // Nested loops — iv_reduce should only affect the inner body, but
    // either way the final answer must match tier 0.
    let src = r#"
        let total = 0;
        for (let i = 0; i < 5; i = i + 1) {
            for (let j = 0; j < 5; j = j + 1) {
                total = total + i * 2;
            }
        }
        total
    "#;
    let vm_result = run_vm(src);
    let ir_result = run_ir_pipeline(src).expect("ir pipeline");
    assert!(
        eq(&vm_result, &ir_result),
        "vm={vm_result:?} ir={ir_result:?}"
    );
}
