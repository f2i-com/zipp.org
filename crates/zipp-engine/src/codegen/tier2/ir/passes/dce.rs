//! Dead code elimination.
//!
//! A pure-IR op whose result isn't read anywhere (directly or
//! transitively) by a side-effecting op or by the function's return
//! value contributes no observable behaviour — we remove it.
//!
//! # What counts as "side-effecting"
//!
//! * Any op matching [`IrOp::is_void`]: stores, global writes.
//! * Any op with deopt side-effects: `Check*`, `CheckedAdd*` etc.
//! * Any op that could throw or trap: `CallRuntime` (conservative).
//! * Any op transitively feeding a block terminator.
//!
//! # Algorithm
//!
//! 1. Seed the "live" set with the above side-effectful ops and the
//!    operands of every terminator.
//! 2. Iterate: for every live op, mark its operands as live too.
//! 3. Stop when nothing new turns live.
//! 4. Drop all non-live ops from block op lists (preserving order of
//!    the survivors).
//!
//! Non-blind: block parameters are never removed in this pass —
//! they're part of the IR's ABI with predecessors. A separate "dead
//! block-param" pass (phase 4) will prune them once we have liveness
//! info at the regalloc level.

use std::collections::HashSet;

use super::super::types::{IrFunction, IrOp, Terminator, ValueId};

/// Run the pass. Returns `true` if any op was removed.
pub fn run(func: &mut IrFunction) -> bool {
    let mut live: HashSet<ValueId> = HashSet::new();

    // Seed from terminators.
    for block in &func.blocks {
        for v in term_uses(&block.term) {
            live.insert(v);
        }
    }

    // Seed from side-effecting ops.
    for block in &func.blocks {
        for (vid, op) in &block.ops {
            if is_side_effectful(op) {
                live.insert(*vid);
                for used in op_uses(op) {
                    live.insert(used);
                }
            }
        }
    }

    // Seed from block parameters — conservatively keep all of them
    // for phase 3. Phase 4 will prune block params that aren't
    // actually read.
    for block in &func.blocks {
        for (vid, _) in &block.params {
            live.insert(*vid);
        }
    }

    // Build a map `ValueId → operands it reads` so we can propagate
    // liveness without re-walking the whole IR.
    let mut uses_of: Vec<(ValueId, Vec<ValueId>)> = Vec::new();
    for block in &func.blocks {
        for (vid, op) in &block.ops {
            uses_of.push((*vid, op_uses(op)));
        }
    }

    // Fixpoint: any live op's operands become live too.
    loop {
        let mut changed = false;
        for (vid, used) in &uses_of {
            if live.contains(vid) {
                for u in used {
                    if live.insert(*u) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Prune.
    let mut removed = false;
    for block in &mut func.blocks {
        let before = block.ops.len();
        block.ops.retain(|(vid, op)| live.contains(vid) || is_side_effectful(op));
        if block.ops.len() != before {
            removed = true;
        }
    }
    removed
}

/// True if removing this op would change observable behaviour.
fn is_side_effectful(op: &IrOp) -> bool {
    match op {
        IrOp::StoreSlot(..) | IrOp::StoreGlobal(..) => true,
        IrOp::CallRuntime(..) => true,
        IrOp::CallValue(..) => true,
        IrOp::MakeClosureNoCapture(..) => true,
        IrOp::CheckI32(..)
        | IrOp::CheckF64(..)
        | IrOp::CheckHeap(..)
        | IrOp::CheckHeapShape(..)
        | IrOp::CheckFunctionIs(..)
        | IrOp::CheckedAddI32(..)
        | IrOp::CheckedSubI32(..)
        | IrOp::CheckedMulI32(..) => true,
        // Everything else is pure — even loads, because we treat the
        // heap as read-only from the IR's perspective between calls.
        // Phase 5 tightens this by introducing explicit effect edges
        // once speculation moves things around more aggressively.
        _ => false,
    }
}

/// Which ValueIds does this op read?
fn op_uses(op: &IrOp) -> Vec<ValueId> {
    // Mirror of verify::op_uses — kept here to avoid cross-module
    // dependency on an internal helper.
    match op {
        IrOp::ConstI32(_)
        | IrOp::ConstF64(_)
        | IrOp::ConstBool(_)
        | IrOp::ConstNull
        | IrOp::ConstUndef
        | IrOp::ConstValue(_)
        | IrOp::LoadReg(_)
        | IrOp::LoadGlobal(_) => vec![],

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
        | IrOp::StoreGlobal(_, v) => vec![*v],

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
        | IrOp::LeValue(a, b) => vec![*a, *b],

        IrOp::StoreSlot(obj, _, val) => vec![*obj, *val],

        IrOp::CallRuntime(_, args) => args.clone(),
        IrOp::CallValue(callee, args) => {
            let mut uses = Vec::with_capacity(args.len() + 1);
            uses.push(*callee);
            uses.extend_from_slice(args);
            uses
        }
        IrOp::MakeClosureNoCapture(_) => vec![],
    }
}

fn term_uses(term: &Terminator) -> Vec<ValueId> {
    match term {
        Terminator::Return(Some(v)) => vec![*v],
        Terminator::Return(None) | Terminator::Deopt(_) | Terminator::Unreachable => vec![],
        Terminator::Jump(_, args) => args.clone(),
        Terminator::Branch {
            cond,
            then_args,
            else_args,
            ..
        } => {
            let mut v = Vec::with_capacity(1 + then_args.len() + else_args.len());
            v.push(*cond);
            v.extend_from_slice(then_args);
            v.extend_from_slice(else_args);
            v
        }
    }
}
