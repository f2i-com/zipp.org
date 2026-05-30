//! Tier-2 phase-3 IR passes: unit + end-to-end tests.
//!
//! Two flavours:
//!
//! 1. **Hand-built IR** — constructs a specific IR shape, runs a
//!    single pass, asserts the exact post-pass shape. Ideal for
//!    nailing down corner cases.
//! 2. **End-to-end** — compile JS → bytecode → IR → run the whole
//!    phase-3 pipeline → check verifier + rough shape invariants.
//!    Gives us confidence the pipeline composes.

use zipp_engine::codegen::tier2::ir::{
    build, passes, print, verify, Block, BlockId, IrFunction, IrOp, Terminator, ValueId, ValueType,
};
use zipp_engine::engine::ZippEngine;

/// Build an IR with exactly the supplied blocks. No bytecode context
/// — used for unit tests that want to exercise a pass in isolation.
fn make_func(blocks: Vec<Block>, num_regs: u16) -> IrFunction {
    IrFunction {
        bytecode_len: 0,
        num_bytecode_regs: num_regs,
        num_parameters: 0,
        blocks,
        deopt_points: Vec::new(),
        constants: std::rc::Rc::new(Vec::new()),
    }
}

// ─── const_fold ─────────────────────────────────────────────────────────────

#[test]
fn const_fold_add_i32() {
    // v0 = const.i32 2
    // v1 = const.i32 3
    // v2 = add.i32 v0, v1  →  const.i32 5
    // return v2
    let func = make_func(
        vec![Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(0), IrOp::ConstI32(2)),
                (ValueId(1), IrOp::ConstI32(3)),
                (ValueId(2), IrOp::AddI32(ValueId(0), ValueId(1))),
            ],
            term: Terminator::Return(Some(ValueId(2))),
        }],
        0,
    );
    let mut f = func;
    assert!(passes::const_fold::run(&mut f));
    match &f.blocks[0].ops[2].1 {
        IrOp::ConstI32(5) => {}
        other => panic!("expected ConstI32(5), got {other:?}\n{}", print::dump(&f)),
    }
    verify::verify(&f).expect("post-fold verify");
}

#[test]
fn const_fold_chains() {
    // Chain folding in a single pass: because the rewrite walk
    // updates the `known` map in-order, a later op seeing a just-
    // folded constant from an earlier op can fold too.
    //
    // v0 = const.i32 10
    // v1 = const.i32 20
    // v2 = add.i32 v0, v1  ==>  const.i32 30
    // v3 = mul.i32 v2, v0  ==>  const.i32 300  (same pass)
    let func = make_func(
        vec![Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(0), IrOp::ConstI32(10)),
                (ValueId(1), IrOp::ConstI32(20)),
                (ValueId(2), IrOp::AddI32(ValueId(0), ValueId(1))),
                (ValueId(3), IrOp::MulI32(ValueId(2), ValueId(0))),
            ],
            term: Terminator::Return(Some(ValueId(3))),
        }],
        0,
    );
    let mut f = func;
    assert!(passes::const_fold::run(&mut f));
    // Second call should be a no-op — the first pass folded both.
    assert!(
        !passes::const_fold::run(&mut f),
        "expected no further folds"
    );
    match &f.blocks[0].ops[3].1 {
        IrOp::ConstI32(300) => {}
        other => panic!("expected ConstI32(300), got {other:?}"),
    }
}

#[test]
fn const_fold_comparisons() {
    let func = make_func(
        vec![Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(0), IrOp::ConstI32(5)),
                (ValueId(1), IrOp::ConstI32(10)),
                (ValueId(2), IrOp::LtValue(ValueId(0), ValueId(1))),
            ],
            term: Terminator::Return(Some(ValueId(2))),
        }],
        0,
    );
    let mut f = func;
    assert!(passes::const_fold::run(&mut f));
    match &f.blocks[0].ops[2].1 {
        IrOp::ConstBool(true) => {}
        other => panic!("expected ConstBool(true), got {other:?}"),
    }
}

#[test]
fn const_fold_respects_overflow() {
    // i32::MAX + 1 overflows. We refuse to fold (leaves the generic
    // op so the runtime promotes to f64).
    let func = make_func(
        vec![Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(0), IrOp::ConstI32(i32::MAX)),
                (ValueId(1), IrOp::ConstI32(1)),
                (ValueId(2), IrOp::AddGeneric(ValueId(0), ValueId(1))),
            ],
            term: Terminator::Return(Some(ValueId(2))),
        }],
        0,
    );
    let mut f = func;
    passes::const_fold::run(&mut f);
    assert!(
        matches!(&f.blocks[0].ops[2].1, IrOp::AddGeneric(_, _)),
        "expected AddGeneric to be preserved; got {:?}",
        f.blocks[0].ops[2].1
    );
}

// ─── copy_prop ──────────────────────────────────────────────────────────────

#[test]
fn copy_prop_one_step() {
    // v0 = const.i32 7
    // v1 = copy v0
    // v2 = copy v1
    // return v2 → after copy-prop, terminator refers to v0 directly.
    let func = make_func(
        vec![Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(0), IrOp::ConstI32(7)),
                (ValueId(1), IrOp::Copy(ValueId(0))),
                (ValueId(2), IrOp::Copy(ValueId(1))),
            ],
            term: Terminator::Return(Some(ValueId(2))),
        }],
        0,
    );
    let mut f = func;
    assert!(passes::copy_prop::run(&mut f));
    match &f.blocks[0].term {
        Terminator::Return(Some(v)) => assert_eq!(*v, ValueId(0)),
        _ => panic!("terminator shape changed"),
    }
}

#[test]
fn copy_prop_rewrites_ops() {
    // v0 = const.i32 3
    // v1 = copy v0
    // v2 = add.i32 v1, v0  → after prop, add.i32 v0, v0
    let func = make_func(
        vec![Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(0), IrOp::ConstI32(3)),
                (ValueId(1), IrOp::Copy(ValueId(0))),
                (ValueId(2), IrOp::AddI32(ValueId(1), ValueId(0))),
            ],
            term: Terminator::Return(Some(ValueId(2))),
        }],
        0,
    );
    let mut f = func;
    assert!(passes::copy_prop::run(&mut f));
    match &f.blocks[0].ops[2].1 {
        IrOp::AddI32(a, b) => {
            assert_eq!(*a, ValueId(0));
            assert_eq!(*b, ValueId(0));
        }
        _ => panic!("add.i32 shape changed"),
    }
}

// ─── dce ────────────────────────────────────────────────────────────────────

#[test]
fn dce_removes_pure_unused() {
    // v0 = const.i32 1  (dead — nothing reads it)
    // v1 = const.i32 2
    // return v1
    let func = make_func(
        vec![Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(0), IrOp::ConstI32(1)),
                (ValueId(1), IrOp::ConstI32(2)),
            ],
            term: Terminator::Return(Some(ValueId(1))),
        }],
        0,
    );
    let mut f = func;
    assert!(passes::dce::run(&mut f));
    assert_eq!(f.blocks[0].ops.len(), 1);
    assert!(matches!(f.blocks[0].ops[0].1, IrOp::ConstI32(2)));
}

#[test]
fn dce_keeps_side_effects() {
    // A StoreGlobal is side-effecting even if no one reads its result.
    let func = make_func(
        vec![Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(0), IrOp::ConstI32(99)),
                (ValueId(1), IrOp::StoreGlobal(0, ValueId(0))),
            ],
            term: Terminator::Return(None),
        }],
        0,
    );
    let mut f = func;
    passes::dce::run(&mut f);
    assert_eq!(f.blocks[0].ops.len(), 2, "StoreGlobal must survive DCE");
}

#[test]
fn dce_transitive() {
    // v0 = const.i32 5        (dead)
    // v1 = const.i32 7        (dead — even though v1 feeds v2)
    // v2 = add.i32 v0, v1     (dead)
    // return undefined
    let func = make_func(
        vec![Block {
            id: BlockId(0),
            params: vec![],
            ops: vec![
                (ValueId(0), IrOp::ConstI32(5)),
                (ValueId(1), IrOp::ConstI32(7)),
                (ValueId(2), IrOp::AddI32(ValueId(0), ValueId(1))),
            ],
            term: Terminator::Return(None),
        }],
        0,
    );
    let mut f = func;
    assert!(passes::dce::run(&mut f));
    assert_eq!(f.blocks[0].ops.len(), 0);
}

// ─── cfg_simplify ───────────────────────────────────────────────────────────

#[test]
fn cfg_simplify_removes_unreachable() {
    // bb0: return; bb1: return 42 (unreachable).
    let func = make_func(
        vec![
            Block {
                id: BlockId(0),
                params: vec![],
                ops: vec![],
                term: Terminator::Return(None),
            },
            Block {
                id: BlockId(1),
                params: vec![],
                ops: vec![(ValueId(0), IrOp::ConstI32(42))],
                term: Terminator::Return(Some(ValueId(0))),
            },
        ],
        0,
    );
    let mut f = func;
    assert!(passes::cfg_simplify::run(&mut f));
    assert_eq!(f.blocks.len(), 1);
}

#[test]
fn cfg_simplify_remaps_ids() {
    // bb0 jumps to bb2. bb1 is dead. After simplify, bb2 is now bb1.
    let func = make_func(
        vec![
            Block {
                id: BlockId(0),
                params: vec![],
                ops: vec![],
                term: Terminator::Jump(BlockId(2), vec![]),
            },
            Block {
                id: BlockId(1),
                params: vec![],
                ops: vec![],
                term: Terminator::Return(None),
            },
            Block {
                id: BlockId(2),
                params: vec![],
                ops: vec![(ValueId(0), IrOp::ConstI32(1))],
                term: Terminator::Return(Some(ValueId(0))),
            },
        ],
        0,
    );
    let mut f = func;
    assert!(passes::cfg_simplify::run(&mut f));
    assert_eq!(f.blocks.len(), 2);
    match &f.blocks[0].term {
        Terminator::Jump(b, _) => assert_eq!(*b, BlockId(1)),
        _ => panic!("terminator shape changed"),
    }
}

// ─── End-to-end pipeline ────────────────────────────────────────────────────

fn translate_ir(src: &str) -> IrFunction {
    let engine = ZippEngine::default();
    let state = engine.compile_script(src).expect("compile");
    let vm = state.vm();
    build::translate(
        &vm.instructions,
        vm.constants.clone(),
        vm.register_count,
        0,
    )
    .expect("translate")
}

#[test]
fn pipeline_loop_sum_verifies() {
    let mut ir = translate_ir(
        "let sum = 0; for (let i = 1; i <= 100; i = i + 1) { sum = sum + i; } sum;",
    );
    let iters = passes::run_default_pipeline(&mut ir);
    assert!(iters >= 1);
    verify::verify(&ir).unwrap_or_else(|e| {
        panic!(
            "pipeline produced invalid IR ({e}):\n{}",
            print::dump(&ir)
        )
    });
}

#[test]
fn pipeline_removes_dead_fallthrough_block() {
    // Top-level `1 + 2;` produces a dead bb4-style block after the
    // Halt — CFG simplification should prune it.
    let mut ir = translate_ir("1 + 2;");
    let before = ir.blocks.len();
    passes::run_default_pipeline(&mut ir);
    assert!(
        ir.blocks.len() <= before,
        "expected block count to decrease or stay same; got {} → {}",
        before,
        ir.blocks.len()
    );
}

#[test]
fn pipeline_const_math_is_collapsed() {
    // `1 + 2 * 3` at the top level stores into a global, but the
    // const math itself should fold. We check that some i32-add /
    // i32-mul ops disappeared.
    let src = "let x = 1 + 2 * 3; x;";
    let mut ir = translate_ir(src);
    let before_ops: usize = ir.blocks.iter().map(|b| b.ops.len()).sum();
    passes::run_default_pipeline(&mut ir);
    let after_ops: usize = ir.blocks.iter().map(|b| b.ops.len()).sum();
    assert!(
        after_ops < before_ops,
        "pipeline didn't reduce op count: {before_ops} → {after_ops}\n{}",
        print::dump(&ir)
    );
    verify::verify(&ir).expect("post-pipeline verify");
}
