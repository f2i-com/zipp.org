//! Tier-2 IR: bytecode → IR round-trip tests.
//!
//! Phase 1 deliverable: every arithmetic/control-flow-only function
//! from the parity corpus should translate successfully, pass the
//! verifier, and pretty-print to something non-empty.
//!
//! This corpus is intentionally narrow for phase 1. Property access,
//! calls, and heap allocation are `BuildError::UnsupportedOp` for
//! now — they come online in later phases. A test here that tries an
//! unsupported pattern returns an error; that's fine and is asserted
//! alongside the positive cases.
//!
//! Each test:
//!   1. Compiles a snippet of JS source via `ZippEngine`.
//!   2. Grabs the resulting bytecode + constants from the cache.
//!   3. Runs `tier2::ir::build::translate` on it.
//!   4. Runs `tier2::ir::verify::verify` on the output.
//!   5. Dumps the IR and asserts a loose shape-check (block count,
//!      op count, etc.).

use zipp_engine::codegen::tier2::ir::{self, build, print, verify};
use zipp_engine::engine::ZippEngine;

/// Compile a JS snippet and translate the resulting top-level
/// bytecode. Returns the successful IR or the first `BuildError` /
/// verifier complaint.
fn translate_source(src: &str) -> Result<ir::IrFunction, String> {
    let engine = ZippEngine::default();
    let compiled = engine
        .compile_script(src)
        .map_err(|e| format!("compile: {e}"))?;
    // Reach into the compiled ScriptState's VM for the raw bytecode.
    // ScriptState::vm() is a public read-only accessor so this is the
    // canonical way to introspect a compiled program from outside the
    // crate.
    let vm = compiled.vm();
    let instructions: Vec<u8> = vm.instructions.to_vec();
    let constants = vm.constants.clone();
    let num_regs = vm.register_count;

    let func = build::translate(&instructions, constants, num_regs, 0)
        .map_err(|e| format!("translate: {e}"))?;
    verify::verify(&func).map_err(|e| {
        format!(
            "verify: {e}\n-- IR dump --\n{}\n-------------",
            print::dump(&func)
        )
    })?;
    Ok(func)
}

// ─── Positive cases ─────────────────────────────────────────────────────────

#[test]
fn translate_add_literals() {
    let ir = translate_source("1 + 2;").expect("translate");
    // Should produce at least one arithmetic op plus a return.
    let total_ops: usize = ir.blocks.iter().map(|b| b.ops.len()).sum();
    assert!(
        total_ops >= 1,
        "expected at least one op; got:\n{}",
        print::dump(&ir)
    );
}

#[test]
fn translate_loop_sum() {
    // The arithmetic-bench inner loop shape. Exercises:
    //   * register reads / writes
    //   * fused AddRegConst / TestLeConstJump
    //   * at least two basic blocks (header + body)
    let src = "let sum = 0; for (let i = 1; i <= 100; i = i + 1) { sum = sum + i; } sum;";
    let ir = translate_source(src).expect("translate loop");
    assert!(
        ir.blocks.len() >= 2,
        "loop should produce ≥ 2 blocks; got:\n{}",
        print::dump(&ir)
    );
}

#[test]
fn translate_if_else() {
    let src = "let x = 5; let y; if (x < 10) { y = 1; } else { y = 2; } y;";
    let ir = translate_source(src).expect("translate if/else");
    // An if-else should produce at least a branch + two arms + a join.
    assert!(ir.blocks.len() >= 3, "if/else should produce ≥ 3 blocks");
}

#[test]
fn translate_while_counter() {
    let src = "let i = 0; while (i < 10) { i = i + 1; } i;";
    let ir = translate_source(src).expect("translate while");
    assert!(ir.blocks.len() >= 2);
}

#[test]
fn translate_const_math() {
    let ir = translate_source("1 - 2 + 3 * 4;").expect("translate const math");
    // All constant folding is deferred to pass 3; phase 1 should emit
    // the generic ops verbatim.
    let ops: usize = ir.blocks.iter().map(|b| b.ops.len()).sum();
    assert!(ops >= 3, "expected ≥ 3 arithmetic ops; got {ops}");
}

#[test]
fn translate_bool_literals() {
    let ir = translate_source("let x = true; let y = false; x;").expect("translate booleans");
    let _ = print::dump(&ir);
}

#[test]
fn dump_produces_plausible_output() {
    let ir = translate_source("1 + 2;").unwrap();
    let s = print::dump(&ir);
    assert!(s.contains("function("));
    assert!(s.contains("bb0"));
    // The closing brace comes out at the end of the dump.
    assert!(s.trim_end().ends_with('}'));
}

// ─── Negative cases (expected failures) ─────────────────────────────────────

#[test]
fn reject_property_access_for_now() {
    // Phase 1 doesn't support GetProp — translator returns
    // UnsupportedOp. When phase 6+ lands, flip this to a positive
    // test and assert the IR contains a LoadSlot / check_shape pair.
    let src = "let o = { x: 1 }; o.x;";
    let err = translate_source(src).expect_err("should reject");
    assert!(
        err.contains("UnsupportedOp") || err.contains("doesn't support"),
        "unexpected error: {err}"
    );
}

#[test]
fn accept_function_calls_after_phase_11() {
    // Phase 11 added `ROp::Call` + zero-capture `MakeClosure` +
    // LoadConst-of-CompiledFunction support. The translate step now
    // succeeds for this shape. (Running the resulting IR still
    // requires proper VM::Call dispatch context; that's a separate
    // integration path covered by tier2_dispatch.)
    let src = "function f(x) { return x + 1; } f(5);";
    translate_source(src).expect("translate should now succeed");
}

// ─── Verifier sanity tests ──────────────────────────────────────────────────
//
// These construct a malformed IR by hand and assert the verifier
// rejects it with an informative message. They're the primary
// safety net against bugs in passes that'll land in phases 3+.

#[test]
fn verify_catches_duplicate_definition() {
    use zipp_engine::codegen::tier2::ir::{
        Block, BlockId, IrOp, Terminator, ValueId, ValueType,
    };

    // Hand-build an IR where v0 is defined by two different ops —
    // verifier should complain.
    let func = ir::IrFunction {
        bytecode_len: 0,
        num_bytecode_regs: 1,
        num_parameters: 0,
        blocks: vec![Block {
            id: BlockId(0),
            params: vec![(ValueId(0), ValueType::Value)],
            ops: vec![(ValueId(0), IrOp::ConstI32(1))], // same id again
            term: Terminator::Return(Some(ValueId(0))),
        }],
        deopt_points: Vec::new(),
        constants: std::rc::Rc::new(Vec::new()),
    };
    let err = verify::verify(&func).expect_err("expected verifier to reject");
    assert!(
        err.contains("defined more than once"),
        "got: {err}"
    );
}

#[test]
fn verify_catches_bad_jump_arity() {
    use zipp_engine::codegen::tier2::ir::{
        Block, BlockId, IrOp, Terminator, ValueId, ValueType,
    };

    // bb0 jumps to bb1 with 0 args, but bb1 has 1 parameter.
    let func = ir::IrFunction {
        bytecode_len: 0,
        num_bytecode_regs: 0,
        num_parameters: 0,
        blocks: vec![
            Block {
                id: BlockId(0),
                params: vec![],
                ops: vec![],
                term: Terminator::Jump(BlockId(1), vec![]),
            },
            Block {
                id: BlockId(1),
                params: vec![(ValueId(0), ValueType::Value)],
                ops: vec![(ValueId(1), IrOp::ConstI32(0))],
                term: Terminator::Return(Some(ValueId(1))),
            },
        ],
        deopt_points: Vec::new(),
        constants: std::rc::Rc::new(Vec::new()),
    };
    let err = verify::verify(&func).expect_err("expected verifier to reject");
    assert!(err.contains("passes 0 args"), "got: {err}");
}

#[test]
fn verify_catches_undominated_use() {
    use zipp_engine::codegen::tier2::ir::{
        Block, BlockId, IrOp, Terminator, ValueId, ValueType,
    };

    // bb0 jumps to bb1 which uses v0 defined in bb2 (not a dominator).
    let func = ir::IrFunction {
        bytecode_len: 0,
        num_bytecode_regs: 0,
        num_parameters: 0,
        blocks: vec![
            Block {
                id: BlockId(0),
                params: vec![],
                ops: vec![],
                term: Terminator::Jump(BlockId(1), vec![]),
            },
            Block {
                id: BlockId(1),
                params: vec![],
                // Using v42 which is defined in bb2 (not reachable
                // from dominance chain of bb1).
                ops: vec![(ValueId(10), IrOp::Copy(ValueId(42)))],
                term: Terminator::Return(Some(ValueId(10))),
            },
            Block {
                id: BlockId(2),
                params: vec![],
                ops: vec![(ValueId(42), IrOp::ConstI32(7))],
                term: Terminator::Unreachable,
            },
        ],
        deopt_points: Vec::new(),
        constants: std::rc::Rc::new(Vec::new()),
    };
    let err = verify::verify(&func).expect_err("expected verifier to reject");
    assert!(
        err.contains("not dominated") || err.contains("undefined value"),
        "got: {err}"
    );
}
