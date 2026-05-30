//! Speculative type narrowing.
//!
//! Rewrites the untyped arithmetic ops (`AddGeneric`, `SubGeneric`,
//! `MulGeneric`) that the translator emits by default into their
//! typed + checked variants when the caller opts in via
//! [`SpeculateConfig`]. The rewrite adds explicit
//! [`IrOp::CheckI32`] guards for each `Value`-typed operand and
//! [`IrOp::CheckedAddI32`] / `…Sub` / `…Mul` for the operation
//! itself, so a type mismatch at runtime lands on a deopt instead
//! of the generic helper's `to_number` coercion.
//!
//! ## Scope (phase 5)
//!
//! No tier-1 feedback yet — the caller decides the speculation
//! policy. Phase 5 ships one option: "assume i32 everywhere". The
//! pass exists mainly so phase-6's deopt runtime has real inputs
//! to react to. Default pipeline does **not** run it yet; callers
//! opt in by invoking [`run`] directly after the phase-3 passes.
//!
//! ## Mechanics
//!
//! For each `(vid, AddGeneric(a, b))` op, the pass rewrites the
//! block's `ops` vector in place, replacing the single entry with
//! a sequence:
//!
//! ```text
//!   ca   = CheckI32(a, deopt_a)       (only if a is Value-typed)
//!   ua   = UnboxI32(ca)                (only if a was Value-typed)
//!   cb   = CheckI32(b, deopt_b)       (only if b is Value-typed)
//!   ub   = UnboxI32(cb)
//!   sum  = CheckedAddI32(ua, ub, deopt_sum)
//!   vid  = BoxI32(sum)
//! ```
//!
//! The original `vid` (result of the `AddGeneric`) stays the result
//! of the final `BoxI32`, so downstream consumers are not rewritten.
//! Intermediate `ValueId`s and `DeoptId`s are freshly allocated.

use super::super::{BlockId, DeoptId, DeoptPoint, IrFunction, IrOp, ValueId, ValueType};
use super::super::types::Location;

/// Configuration for the speculation pass.
#[derive(Clone, Debug)]
pub struct SpeculateConfig {
    /// When `true`, every `AddGeneric` / `SubGeneric` / `MulGeneric`
    /// whose result is used downstream is rewritten to speculate
    /// i32 on both operands. When `false` the pass is a no-op.
    pub speculate_i32: bool,
}

impl Default for SpeculateConfig {
    fn default() -> Self {
        SpeculateConfig { speculate_i32: false }
    }
}

/// Run the speculation pass with the given configuration. Returns
/// the number of generic-arithmetic sites rewritten. A zero return
/// means the IR was left untouched.
pub fn run(func: &mut IrFunction, cfg: &SpeculateConfig) -> usize {
    if !cfg.speculate_i32 {
        return 0;
    }

    // Allocate a single ValueId counter outside the loop so
    // rewrites across blocks don't collide. `num_values` walks the
    // IR to find the current max — safer than tracking an internal
    // next-id field.
    let mut next_vid = func.num_values() as u32;
    let mut next_deopt = func.deopt_points.len() as u32;
    let mut rewrites = 0usize;

    let block_ids: Vec<BlockId> = (0..func.blocks.len())
        .map(|i| BlockId(i as u32))
        .collect();

    // Snapshot the per-block op list + types lookup BEFORE mutating.
    // `ir_type` needs a read-only view; doing all the inspection
    // first lets us replace ops en masse per block.
    let types = infer_types(func);

    for bid in block_ids {
        let block = &mut func.blocks[bid.0 as usize];
        let old_ops = std::mem::take(&mut block.ops);
        let mut new_ops: Vec<(ValueId, IrOp)> = Vec::with_capacity(old_ops.len());

        for (result_vid, op) in old_ops {
            match op {
                IrOp::AddGeneric(a, b) => {
                    if speculate_binop(
                        &mut new_ops,
                        result_vid,
                        a,
                        b,
                        BinopKind::Add,
                        &types,
                        &mut next_vid,
                        &mut next_deopt,
                        &mut func.deopt_points,
                    ) {
                        rewrites += 1;
                    }
                }
                IrOp::SubGeneric(a, b) => {
                    if speculate_binop(
                        &mut new_ops,
                        result_vid,
                        a,
                        b,
                        BinopKind::Sub,
                        &types,
                        &mut next_vid,
                        &mut next_deopt,
                        &mut func.deopt_points,
                    ) {
                        rewrites += 1;
                    }
                }
                IrOp::MulGeneric(a, b) => {
                    if speculate_binop(
                        &mut new_ops,
                        result_vid,
                        a,
                        b,
                        BinopKind::Mul,
                        &types,
                        &mut next_vid,
                        &mut next_deopt,
                        &mut func.deopt_points,
                    ) {
                        rewrites += 1;
                    }
                }
                other => new_ops.push((result_vid, other)),
            }
        }
        func.blocks[bid.0 as usize].ops = new_ops;
    }

    rewrites
}

#[derive(Copy, Clone)]
enum BinopKind {
    Add,
    Sub,
    Mul,
}

/// Emit the speculated sequence for `result = OpGeneric(a, b)` into
/// `new_ops`, preserving the original `result` ValueId as the final
/// op's output.
///
/// Three cases, in decreasing order of specificity:
///
///   1. Both operands statically F64 → rewrite to `AddF64` /
///      `SubF64` / `MulF64`. No deopt needed; types are static.
///      (Division has no generic counterpart, so this branch only
///      fires for Add/Sub/Mul.) The result is F64-typed.
///   2. Both operands I32 or Value → speculate i32 as before:
///      `CheckI32 + UnboxI32 + CheckedAddI32 + BoxI32`. A mismatch
///      at runtime trips the soft-deopt runtime.
///   3. Mixed (one F64, one I32/Value) → leave as the original
///      generic op. The runtime helper widens to f64 and returns
///      the correct Value; speculating either type would deopt on
///      the other.
#[allow(clippy::too_many_arguments)]
fn speculate_binop(
    new_ops: &mut Vec<(ValueId, IrOp)>,
    result: ValueId,
    a: ValueId,
    b: ValueId,
    kind: BinopKind,
    types: &TypeMap,
    next_vid: &mut u32,
    next_deopt: &mut u32,
    deopt_points: &mut Vec<DeoptPoint>,
) -> bool {
    let a_ty = types.get(a);
    let b_ty = types.get(b);
    let both_f64 = matches!(a_ty, Some(ValueType::F64))
        && matches!(b_ty, Some(ValueType::F64));
    let either_f64 = matches!(a_ty, Some(ValueType::F64))
        || matches!(b_ty, Some(ValueType::F64));

    if both_f64 {
        let f64_op = match kind {
            BinopKind::Add => IrOp::AddF64(a, b),
            BinopKind::Sub => IrOp::SubF64(a, b),
            BinopKind::Mul => IrOp::MulF64(a, b),
        };
        new_ops.push((result, f64_op));
        return true;
    }
    if either_f64 {
        // Mixed: skip speculation, keep the generic helper call.
        let generic_op = match kind {
            BinopKind::Add => IrOp::AddGeneric(a, b),
            BinopKind::Sub => IrOp::SubGeneric(a, b),
            BinopKind::Mul => IrOp::MulGeneric(a, b),
        };
        new_ops.push((result, generic_op));
        return false;
    }

    let a_i32 = ensure_i32(new_ops, a, types, next_vid, next_deopt, deopt_points);
    let b_i32 = ensure_i32(new_ops, b, types, next_vid, next_deopt, deopt_points);

    let sum_vid = alloc_vid(next_vid);
    let deopt_sum = alloc_deopt(next_deopt, deopt_points);
    let sum_op = match kind {
        BinopKind::Add => IrOp::CheckedAddI32(a_i32, b_i32, deopt_sum),
        BinopKind::Sub => IrOp::CheckedSubI32(a_i32, b_i32, deopt_sum),
        BinopKind::Mul => IrOp::CheckedMulI32(a_i32, b_i32, deopt_sum),
    };
    new_ops.push((sum_vid, sum_op));
    // Preserve the original result ValueId as the final BoxI32.
    new_ops.push((result, IrOp::BoxI32(sum_vid)));
    true
}

/// If `v` is already typed I32 (from `ConstI32`, `UnboxI32`, another
/// `CheckedAdd*`, …) return it unchanged. Otherwise emit
/// `CheckI32(v) + UnboxI32` and return the unboxed VID.
fn ensure_i32(
    new_ops: &mut Vec<(ValueId, IrOp)>,
    v: ValueId,
    types: &TypeMap,
    next_vid: &mut u32,
    next_deopt: &mut u32,
    deopt_points: &mut Vec<DeoptPoint>,
) -> ValueId {
    if matches!(types.get(v), Some(ValueType::I32)) {
        return v;
    }
    let deopt_id = alloc_deopt(next_deopt, deopt_points);
    let checked = alloc_vid(next_vid);
    new_ops.push((checked, IrOp::CheckI32(v, deopt_id)));
    let unboxed = alloc_vid(next_vid);
    new_ops.push((unboxed, IrOp::UnboxI32(checked)));
    unboxed
}

fn alloc_vid(next: &mut u32) -> ValueId {
    let v = ValueId(*next);
    *next += 1;
    v
}

fn alloc_deopt(next: &mut u32, deopt_points: &mut Vec<DeoptPoint>) -> DeoptId {
    let id = DeoptId(*next);
    *next += 1;
    // Phase-5 placeholder metadata: empty live-regs list and a
    // zero resume-ip. Phase 6 replaces this with the real
    // reconstruction payload before emit.
    deopt_points.push(DeoptPoint {
        id,
        bytecode_ip: 0,
        live_regs: Vec::<(u16, Location)>::new(),
    });
    id
}

// ── Type inference ──────────────────────────────────────────────────

/// Maps each `ValueId` to its inferred IR-level type. Values that
/// don't appear (void ops) default to `ValueType::Value` when
/// queried.
struct TypeMap {
    by_vid: Vec<ValueType>,
}

impl TypeMap {
    fn get(&self, v: ValueId) -> Option<ValueType> {
        self.by_vid.get(v.0 as usize).copied()
    }
}

fn infer_types(func: &IrFunction) -> TypeMap {
    let total = func.num_values();
    let mut by_vid = vec![ValueType::Value; total];
    for block in &func.blocks {
        for (vid, ty) in &block.params {
            let idx = vid.0 as usize;
            if idx < by_vid.len() {
                by_vid[idx] = *ty;
            }
        }
        for (vid, op) in &block.ops {
            let idx = vid.0 as usize;
            if idx < by_vid.len() {
                by_vid[idx] = result_type(op);
            }
        }
    }
    TypeMap { by_vid }
}

fn result_type(op: &IrOp) -> ValueType {
    match op {
        IrOp::ConstI32(_) => ValueType::I32,
        IrOp::ConstF64(_) => ValueType::F64,
        IrOp::ConstBool(_) => ValueType::Bool,
        IrOp::ConstNull
        | IrOp::ConstUndef
        | IrOp::ConstValue(_)
        | IrOp::LoadReg(_)
        | IrOp::LoadGlobal(_) => ValueType::Value,

        IrOp::AddI32(..) | IrOp::SubI32(..) | IrOp::MulI32(..) | IrOp::NegI32(..)
        | IrOp::CheckedAddI32(..) | IrOp::CheckedSubI32(..) | IrOp::CheckedMulI32(..)
        | IrOp::UnboxI32(..) | IrOp::CheckI32(..) => ValueType::I32,

        IrOp::AddF64(..) | IrOp::SubF64(..) | IrOp::MulF64(..)
        | IrOp::DivF64(..) | IrOp::NegF64(..)
        | IrOp::UnboxF64(..) | IrOp::CheckF64(..) => ValueType::F64,

        IrOp::NotBool(..) | IrOp::UnboxBool(..)
        | IrOp::EqI32(..) | IrOp::NeI32(..)
        | IrOp::LtI32(..) | IrOp::LeI32(..)
        | IrOp::GtI32(..) | IrOp::GeI32(..)
        | IrOp::EqValue(..) | IrOp::NeValue(..) | IrOp::LooseEqValue(..)
        | IrOp::LtValue(..) | IrOp::LeValue(..) => ValueType::Bool,

        IrOp::AddGeneric(..) | IrOp::SubGeneric(..) | IrOp::MulGeneric(..)
        | IrOp::DivGeneric(..) | IrOp::ModGeneric(..)
        | IrOp::BoxI32(..) | IrOp::BoxF64(..) | IrOp::BoxBool(..)
        | IrOp::CallRuntime(..) | IrOp::CallValue(..)
        | IrOp::MakeClosureNoCapture(..) | IrOp::Copy(..)
        | IrOp::CheckFunctionIs(..) => ValueType::Value,

        IrOp::CheckHeap(..) => ValueType::Heap,
        IrOp::CheckHeapShape(_, shape, _) => ValueType::HeapShape(*shape),

        IrOp::LoadSlot(_, _, ty) => *ty,
        IrOp::StoreSlot(..) | IrOp::StoreGlobal(..) => ValueType::Bottom,
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::tier2::ir::types::{Block, Terminator};
    use crate::codegen::tier2::ir::verify;
    use std::rc::Rc;

    fn make_func(blocks: Vec<Block>, num_bytecode_regs: u16) -> IrFunction {
        IrFunction {
            bytecode_len: 0,
            num_bytecode_regs,
            num_parameters: num_bytecode_regs,
            blocks,
            deopt_points: Vec::new(),
            constants: Rc::new(Vec::new()),
        }
    }

    #[test]
    fn no_op_when_disabled() {
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstI32(1)),
                (v1, IrOp::ConstI32(2)),
                (v2, IrOp::AddGeneric(v0, v1)),
            ],
            term: Terminator::Return(Some(v2)),
        }];
        let mut func = make_func(blocks, 0);
        let rewrites = run(&mut func, &SpeculateConfig { speculate_i32: false });
        assert_eq!(rewrites, 0);
        assert!(matches!(func.blocks[0].ops[2].1, IrOp::AddGeneric(..)));
    }

    #[test]
    fn rewrites_add_generic_on_typed_operands() {
        // ConstI32 results are already i32-typed → no CheckI32/UnboxI32
        // wrappers needed; rewrite is a single CheckedAddI32 + BoxI32.
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstI32(1)),
                (v1, IrOp::ConstI32(2)),
                (v2, IrOp::AddGeneric(v0, v1)),
            ],
            term: Terminator::Return(Some(v2)),
        }];
        let mut func = make_func(blocks, 0);
        let rewrites = run(&mut func, &SpeculateConfig { speculate_i32: true });
        assert_eq!(rewrites, 1);
        verify::verify(&func).expect("post-pass IR must verify");

        // Expect: ConstI32, ConstI32, CheckedAddI32, BoxI32 (in that order).
        let ops = &func.blocks[0].ops;
        assert_eq!(ops.len(), 4);
        assert!(matches!(ops[2].1, IrOp::CheckedAddI32(..)));
        assert!(matches!(ops[3].1, IrOp::BoxI32(_)));
        // Original result VID is preserved as the final BoxI32 output.
        assert_eq!(ops[3].0, v2);
    }

    #[test]
    fn rewrites_add_generic_on_value_operands_inserts_checks() {
        // Value-typed entry params require CheckI32 + UnboxI32 each.
        let p0 = ValueId(0);
        let p1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![(p0, ValueType::Value), (p1, ValueType::Value)],
            ops: vec![(v2, IrOp::AddGeneric(p0, p1))],
            term: Terminator::Return(Some(v2)),
        }];
        let mut func = make_func(blocks, 2);
        let rewrites = run(&mut func, &SpeculateConfig { speculate_i32: true });
        assert_eq!(rewrites, 1);
        verify::verify(&func).expect("post-pass IR must verify");

        let ops = &func.blocks[0].ops;
        // Expected sequence: CheckI32(p0), UnboxI32, CheckI32(p1),
        // UnboxI32, CheckedAddI32, BoxI32 — 6 ops total.
        assert_eq!(ops.len(), 6);
        assert!(matches!(ops[0].1, IrOp::CheckI32(..)));
        assert!(matches!(ops[1].1, IrOp::UnboxI32(_)));
        assert!(matches!(ops[2].1, IrOp::CheckI32(..)));
        assert!(matches!(ops[3].1, IrOp::UnboxI32(_)));
        assert!(matches!(ops[4].1, IrOp::CheckedAddI32(..)));
        assert!(matches!(ops[5].1, IrOp::BoxI32(_)));
        assert_eq!(ops[5].0, v2);
    }

    #[test]
    fn rewrites_sub_and_mul_similarly() {
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let v3 = ValueId(3);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstI32(10)),
                (v1, IrOp::ConstI32(3)),
                (v2, IrOp::SubGeneric(v0, v1)),
                (v3, IrOp::MulGeneric(v0, v2)),
            ],
            term: Terminator::Return(Some(v3)),
        }];
        let mut func = make_func(blocks, 0);
        let rewrites = run(&mut func, &SpeculateConfig { speculate_i32: true });
        assert_eq!(rewrites, 2);
        verify::verify(&func).expect("post-pass IR must verify");
    }

    #[test]
    fn deopt_points_are_registered() {
        // Each new Check* / CheckedAdd lands a DeoptPoint entry.
        let p0 = ValueId(0);
        let p1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![(p0, ValueType::Value), (p1, ValueType::Value)],
            ops: vec![(v2, IrOp::AddGeneric(p0, p1))],
            term: Terminator::Return(Some(v2)),
        }];
        let mut func = make_func(blocks, 2);
        run(&mut func, &SpeculateConfig { speculate_i32: true });
        // 2 × CheckI32 + 1 × CheckedAddI32 = 3 deopt points.
        assert_eq!(func.deopt_points.len(), 3);
    }
}
