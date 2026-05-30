//! Constant folding.
//!
//! Evaluate `Add/Sub/Mul` (and friends) at compile time when both
//! operands are constants. Limited in phase 3 to integer arithmetic +
//! boolean / comparison folds — floating-point folding is deferred
//! to avoid NaN-corner-case land-mines, and generic-value folds (with
//! string coercion) wait until the speculation pass has narrowed
//! types.
//!
//! Overflow policy: `wrapping_*` semantics, matching the JS behaviour
//! of `|0` coerced arithmetic. Folds that would overflow an i32 keep
//! the generic op so the runtime coerces to f64 at execution — tier 0
//! and tier 1 both have that exact same fallback so behaviour stays
//! identical.
//!
//! ## Algorithm
//!
//! Two passes over the function:
//!
//! 1. **Build a map** `ValueId → ConstVal` for every `ConstI32` /
//!    `ConstF64` / `ConstBool` / `ConstNull` / `ConstUndef` op. Cheap;
//!    linear in IR size.
//! 2. **Walk each op.** When an op's operands are all in the map and
//!    we have a fold rule, rewrite the op in place to the corresponding
//!    `Const*` and add the new mapping so downstream folds can chain.
//!
//! Copy-propagation should run after this pass so chained folds
//! (`v4 = const.i32 7` replacing `v4 = add.val v1 v2`) don't leave
//! the old operands as dead code — DCE handles that.

use std::collections::HashMap;

use super::super::types::{IrFunction, IrOp, ValueId};

/// Run the pass. Returns `true` if anything changed.
pub fn run(func: &mut IrFunction) -> bool {
    let mut known: HashMap<ValueId, ConstVal> = HashMap::new();
    // Seed: record every currently-constant value.
    for block in &func.blocks {
        for (vid, op) in &block.ops {
            if let Some(v) = op_to_const(op) {
                known.insert(*vid, v);
            }
        }
    }

    let mut changed = false;
    // Rewrite.
    for block in &mut func.blocks {
        for (vid, op) in &mut block.ops {
            if let Some(new_op) = fold(op, &known) {
                if let Some(v) = op_to_const(&new_op) {
                    known.insert(*vid, v);
                }
                *op = new_op;
                changed = true;
            }
        }
    }
    changed
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum ConstVal {
    I32(i32),
    F64(u64), // bits
    Bool(bool),
    Null,
    Undef,
}

fn op_to_const(op: &IrOp) -> Option<ConstVal> {
    match op {
        IrOp::ConstI32(v) => Some(ConstVal::I32(*v)),
        IrOp::ConstF64(b) => Some(ConstVal::F64(*b)),
        IrOp::ConstBool(b) => Some(ConstVal::Bool(*b)),
        IrOp::ConstNull => Some(ConstVal::Null),
        IrOp::ConstUndef => Some(ConstVal::Undef),
        _ => None,
    }
}

fn get(known: &HashMap<ValueId, ConstVal>, v: &ValueId) -> Option<ConstVal> {
    known.get(v).copied()
}

/// Try to fold a single op. Returns `Some(new_op)` if we simplified,
/// `None` otherwise.
fn fold(op: &IrOp, known: &HashMap<ValueId, ConstVal>) -> Option<IrOp> {
    match op {
        // ── Typed i32 arithmetic (wrapping) ──
        IrOp::AddI32(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstI32(x.wrapping_add(y))),
            _ => None,
        },
        IrOp::SubI32(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstI32(x.wrapping_sub(y))),
            _ => None,
        },
        IrOp::MulI32(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstI32(x.wrapping_mul(y))),
            _ => None,
        },
        IrOp::NegI32(a) => match get(known, a)? {
            ConstVal::I32(x) => Some(IrOp::ConstI32(x.wrapping_neg())),
            _ => None,
        },
        IrOp::NotBool(a) => match get(known, a)? {
            ConstVal::Bool(x) => Some(IrOp::ConstBool(!x)),
            _ => None,
        },

        // ── Generic arithmetic with narrow-to-i32 when safe ──
        //
        // `AddGeneric(i32, i32)` folds to an i32 constant *only* if
        // the arithmetic doesn't overflow i32. Overflowed cases keep
        // the generic op — the runtime promotes to f64 the same way
        // tier 0 / tier 1 do, and we refuse to change observable
        // behaviour at the IR layer.
        IrOp::AddGeneric(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => x.checked_add(y).map(IrOp::ConstI32),
            _ => None,
        },
        IrOp::SubGeneric(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => x.checked_sub(y).map(IrOp::ConstI32),
            _ => None,
        },
        IrOp::MulGeneric(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => x.checked_mul(y).map(IrOp::ConstI32),
            _ => None,
        },

        // ── Comparisons ──
        IrOp::EqI32(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x == y)),
            _ => None,
        },
        IrOp::NeI32(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x != y)),
            _ => None,
        },
        IrOp::LtI32(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x < y)),
            _ => None,
        },
        IrOp::LeI32(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x <= y)),
            _ => None,
        },
        IrOp::GtI32(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x > y)),
            _ => None,
        },
        IrOp::GeI32(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x >= y)),
            _ => None,
        },

        // Generic value-level comparisons on known integer operands.
        // Same soundness reasoning: two known-i32 operands have
        // identical `<` semantics in integer and value domains (JS ===
        // / < behave as numeric for numbers). We only fold when BOTH
        // are integers; mixed types stay generic.
        IrOp::EqValue(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x == y)),
            (ConstVal::Bool(x), ConstVal::Bool(y)) => Some(IrOp::ConstBool(x == y)),
            (ConstVal::Null, ConstVal::Null) => Some(IrOp::ConstBool(true)),
            _ => None,
        },
        IrOp::NeValue(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x != y)),
            (ConstVal::Bool(x), ConstVal::Bool(y)) => Some(IrOp::ConstBool(x != y)),
            (ConstVal::Null, ConstVal::Null) => Some(IrOp::ConstBool(false)),
            _ => None,
        },
        IrOp::LtValue(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x < y)),
            _ => None,
        },
        IrOp::LeValue(a, b) => match (get(known, a)?, get(known, b)?) {
            (ConstVal::I32(x), ConstVal::I32(y)) => Some(IrOp::ConstBool(x <= y)),
            _ => None,
        },

        _ => None,
    }
}
