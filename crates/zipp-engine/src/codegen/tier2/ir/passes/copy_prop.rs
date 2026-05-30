//! Copy propagation.
//!
//! Replace uses of `Copy v` (and any other identity-ish op the IR
//! produces) with the value they copy. Followed by DCE the dead
//! Copy ops disappear.
//!
//! Phase 3 handles exactly `IrOp::Copy`. Later passes may introduce
//! more identity ops (e.g. a redundant `BoxI32 → UnboxI32` pair) —
//! those get their own fold in [`const_fold`](super::const_fold) or a
//! dedicated `peephole` pass.

use std::collections::HashMap;

use super::super::types::{IrFunction, IrOp, Terminator, ValueId};

/// Run the pass. Returns `true` if anything changed.
pub fn run(func: &mut IrFunction) -> bool {
    // Build `alias: ValueId → ValueId`. If `v = Copy w` and `w = Copy x`
    // we want uses of `v` to go all the way back to `x` — so we
    // compute transitive aliases by walking chains in a single pass.
    let mut alias: HashMap<ValueId, ValueId> = HashMap::new();
    for block in &func.blocks {
        for (vid, op) in &block.ops {
            if let IrOp::Copy(src) = op {
                // Resolve `src` through any existing aliases so `alias`
                // is always flattened by the end of the seeding phase.
                let ultimate = resolve(&alias, *src);
                alias.insert(*vid, ultimate);
            }
        }
    }
    if alias.is_empty() {
        return false;
    }

    let mut changed = false;

    // Helper: rewrite a single ValueId operand.
    let rewrite = |v: &mut ValueId, changed: &mut bool| {
        if let Some(&tgt) = alias.get(v) {
            *v = tgt;
            *changed = true;
        }
    };
    let rewrite_slice = |vs: &mut [ValueId], changed: &mut bool| {
        for v in vs {
            rewrite(v, changed);
        }
    };

    for block in &mut func.blocks {
        for (_, op) in &mut block.ops {
            rewrite_op(op, &alias, &mut changed);
        }
        match &mut block.term {
            Terminator::Return(Some(v)) => rewrite(v, &mut changed),
            Terminator::Return(None) | Terminator::Deopt(_) | Terminator::Unreachable => {}
            Terminator::Jump(_, args) => rewrite_slice(args, &mut changed),
            Terminator::Branch {
                cond,
                then_args,
                else_args,
                ..
            } => {
                rewrite(cond, &mut changed);
                rewrite_slice(then_args, &mut changed);
                rewrite_slice(else_args, &mut changed);
            }
        }
    }

    changed
}

fn resolve(alias: &HashMap<ValueId, ValueId>, mut v: ValueId) -> ValueId {
    // Path-compression-lite: bounded to keep the pass O(n) even on
    // pathological chains (which shouldn't exist in well-formed IR,
    // but defensive programming costs nothing).
    for _ in 0..64 {
        match alias.get(&v) {
            Some(&next) if next != v => v = next,
            _ => break,
        }
    }
    v
}

/// Rewrite every `ValueId` operand inside an op.
fn rewrite_op(op: &mut IrOp, alias: &HashMap<ValueId, ValueId>, changed: &mut bool) {
    let rw = |v: &mut ValueId, changed: &mut bool| {
        if let Some(&tgt) = alias.get(v) {
            *v = tgt;
            *changed = true;
        }
    };
    match op {
        IrOp::ConstI32(_)
        | IrOp::ConstF64(_)
        | IrOp::ConstBool(_)
        | IrOp::ConstNull
        | IrOp::ConstUndef
        | IrOp::ConstValue(_)
        | IrOp::LoadReg(_)
        | IrOp::LoadGlobal(_) => {}

        IrOp::Copy(v)
        | IrOp::NegI32(v)
        | IrOp::NegF64(v)
        | IrOp::NotBool(v)
        | IrOp::BoxI32(v)
        | IrOp::BoxF64(v)
        | IrOp::BoxBool(v)
        | IrOp::UnboxI32(v)
        | IrOp::UnboxF64(v)
        | IrOp::UnboxBool(v)
        | IrOp::CheckI32(v, _)
        | IrOp::CheckF64(v, _)
        | IrOp::CheckHeap(v, _)
        | IrOp::CheckHeapShape(v, _, _)
        | IrOp::CheckFunctionIs(v, _, _)
        | IrOp::LoadSlot(v, _, _)
        | IrOp::StoreGlobal(_, v) => rw(v, changed),

        IrOp::AddI32(a, b)
        | IrOp::SubI32(a, b)
        | IrOp::MulI32(a, b)
        | IrOp::CheckedAddI32(a, b, _)
        | IrOp::CheckedSubI32(a, b, _)
        | IrOp::CheckedMulI32(a, b, _)
        | IrOp::AddF64(a, b)
        | IrOp::SubF64(a, b)
        | IrOp::MulF64(a, b)
        | IrOp::DivF64(a, b)
        | IrOp::AddGeneric(a, b)
        | IrOp::SubGeneric(a, b)
        | IrOp::MulGeneric(a, b)
        | IrOp::DivGeneric(a, b)
        | IrOp::ModGeneric(a, b)
        | IrOp::EqI32(a, b)
        | IrOp::NeI32(a, b)
        | IrOp::LtI32(a, b)
        | IrOp::LeI32(a, b)
        | IrOp::GtI32(a, b)
        | IrOp::GeI32(a, b)
        | IrOp::EqValue(a, b)
        | IrOp::NeValue(a, b)
        | IrOp::LooseEqValue(a, b)
        | IrOp::LtValue(a, b)
        | IrOp::LeValue(a, b) => {
            rw(a, changed);
            rw(b, changed);
        }

        IrOp::StoreSlot(obj, _, val) => {
            rw(obj, changed);
            rw(val, changed);
        }

        IrOp::CallRuntime(_, args) => {
            for v in args {
                rw(v, changed);
            }
        }
        IrOp::CallValue(callee, args) => {
            rw(callee, changed);
            for v in args {
                rw(v, changed);
            }
        }
        IrOp::MakeClosureNoCapture(_) => {}
    }
}
