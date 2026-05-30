//! IR soundness verifier.
//!
//! Runs after every IR-rewriting pass in debug/test builds. Keeps the
//! later passes honest: if any transformation breaks SSA or dominance
//! invariants, the verifier fails loudly before codegen amplifies the
//! bug into native-code corruption.
//!
//! The checks implemented here are proportional to phase 1's scope —
//! types aren't fully tracked yet (later passes will plumb narrowed
//! `ValueType` through each op's operands), so some checks are
//! "structural" rather than "typed". Tighter type checking lands as
//! part of phase 5 (speculate).
//!
//! # Checks
//!
//! 1. **Single definition.** Each `ValueId` appears as the result of
//!    exactly one block-param or `IrOp`.
//! 2. **Dominance.** Each use of a `ValueId` is dominated by its
//!    definition. A use in block `B` at position `P` is legal iff
//!    either the definition is in `B` before `P`, or the definition's
//!    block strictly dominates `B`.
//! 3. **Terminator discipline.** Every block has exactly one terminator
//!    in the `term` field and no op implicitly terminates.
//! 4. **Successor shape.** Every `Jump` / `Branch` targets an existing
//!    block, and the number of arguments matches the target's
//!    parameter count.
//! 5. **Void-op discipline.** Results of [`IrOp::is_void`] ops are
//!    never referenced by any other op.

use std::collections::HashSet;

use super::types::{Block, BlockId, IrFunction, IrOp, Terminator, ValueId};

/// Top-level entry point. Returns `Ok(())` on a well-formed function,
/// `Err(msg)` with a human-readable explanation otherwise.
pub fn verify(func: &IrFunction) -> Result<(), String> {
    // ── 1. Build id → (defining block, position within block) map ──
    //
    // Positions: negative (= -1 - param_index) means "the Nth block
    // parameter"; non-negative means the Nth op in the block.
    let mut defs: Vec<Option<DefSite>> = vec![None; func.num_values()];

    for block in &func.blocks {
        for (i, (vid, _)) in block.params.iter().enumerate() {
            if defs[vid.0 as usize].is_some() {
                return Err(format!(
                    "ValueId {vid} defined more than once (block param in {})",
                    block.id
                ));
            }
            defs[vid.0 as usize] = Some(DefSite::Param(block.id, i));
        }
        for (i, (vid, _)) in block.ops.iter().enumerate() {
            if defs[vid.0 as usize].is_some() {
                return Err(format!(
                    "ValueId {vid} defined more than once (op in {})",
                    block.id
                ));
            }
            defs[vid.0 as usize] = Some(DefSite::Op(block.id, i));
        }
    }

    // ── 2. Compute block dominance ──
    //
    // For phase 1 we use a trivial iterative dataflow — good enough
    // for a few hundred blocks. Phase 4 will want a proper Lengauer-
    // Tarjan pass.
    let dom = compute_dominators(&func.blocks);

    // ── 3. Check every use ──
    for block in &func.blocks {
        // Collect which values are available at each position.
        //
        // A value is "available at op position i" if it's defined at
        // a strictly earlier position in this block OR in a block
        // that (strictly or non-strictly) dominates this block.
        for (pos, (_vid, op)) in block.ops.iter().enumerate() {
            check_op_uses(op, block, pos, &defs, &dom, func)?;
        }
        check_term_uses(&block.term, block, &defs, &dom, func)?;
    }

    // ── 4. Check successor shape ──
    for block in &func.blocks {
        match &block.term {
            Terminator::Return(_) | Terminator::Deopt(_) | Terminator::Unreachable => {}
            Terminator::Jump(target, args) => {
                let t = get_block(func, *target)?;
                if args.len() != t.params.len() {
                    return Err(format!(
                        "{} Jump to {target} passes {} args but target expects {}",
                        block.id,
                        args.len(),
                        t.params.len()
                    ));
                }
            }
            Terminator::Branch {
                then_block,
                then_args,
                else_block,
                else_args,
                ..
            } => {
                let tb = get_block(func, *then_block)?;
                let eb = get_block(func, *else_block)?;
                if then_args.len() != tb.params.len() {
                    return Err(format!(
                        "{} Branch.then to {then_block} passes {} args but target expects {}",
                        block.id,
                        then_args.len(),
                        tb.params.len()
                    ));
                }
                if else_args.len() != eb.params.len() {
                    return Err(format!(
                        "{} Branch.else to {else_block} passes {} args but target expects {}",
                        block.id,
                        else_args.len(),
                        eb.params.len()
                    ));
                }
            }
        }
    }

    // ── 5. Void-op discipline ──
    let void_values = collect_void_values(func);
    for block in &func.blocks {
        for (_, op) in &block.ops {
            for used in op_uses(op) {
                if void_values.contains(&used) {
                    return Err(format!(
                        "ValueId {used} is a void op result but used as an operand"
                    ));
                }
            }
        }
        for used in term_uses(&block.term) {
            if void_values.contains(&used) {
                return Err(format!(
                    "{} terminator uses void-op result {used}",
                    block.id
                ));
            }
        }
    }

    Ok(())
}

// ─── Internal helpers ───────────────────────────────────────────────────────

#[derive(Copy, Clone, Debug)]
enum DefSite {
    /// Block parameter: (block, index into params).
    Param(BlockId, usize),
    /// Op within the block: (block, index into ops).
    Op(BlockId, usize),
}

fn get_block(func: &IrFunction, id: BlockId) -> Result<&Block, String> {
    func.blocks
        .get(id.0 as usize)
        .ok_or_else(|| format!("referenced block {id} does not exist"))
}

/// Check all operand-uses of a single op for dominance correctness.
fn check_op_uses(
    op: &IrOp,
    block: &Block,
    pos: usize,
    defs: &[Option<DefSite>],
    dom: &Dominance,
    func: &IrFunction,
) -> Result<(), String> {
    for used in op_uses(op) {
        check_use(used, block, Some(pos), defs, dom, func)
            .map_err(|e| format!("op #{pos} in {}: {e}", block.id))?;
    }
    Ok(())
}

fn check_term_uses(
    term: &Terminator,
    block: &Block,
    defs: &[Option<DefSite>],
    dom: &Dominance,
    func: &IrFunction,
) -> Result<(), String> {
    for used in term_uses(term) {
        check_use(used, block, None, defs, dom, func)
            .map_err(|e| format!("terminator of {}: {e}", block.id))?;
    }
    Ok(())
}

/// Returns `Ok(())` if `used` is available at the use site. The
/// `use_pos` argument describes the position within the using block:
/// `Some(n)` for op n, `None` for the terminator.
fn check_use(
    used: ValueId,
    use_block: &Block,
    use_pos: Option<usize>,
    defs: &[Option<DefSite>],
    dom: &Dominance,
    _func: &IrFunction,
) -> Result<(), String> {
    let Some(def_site) = defs.get(used.0 as usize).and_then(|x| *x) else {
        return Err(format!("use of undefined value {used}"));
    };
    match def_site {
        DefSite::Param(def_block, _) => {
            // Block params are "available" from position 0 of their
            // own block onward.
            if def_block == use_block.id {
                // Same-block use: any op or terminator can see it.
                return Ok(());
            }
            if !dom.strictly_dominates(def_block, use_block.id) {
                return Err(format!(
                    "{used} (param of {def_block}) is used in {} which is not dominated by it",
                    use_block.id
                ));
            }
            Ok(())
        }
        DefSite::Op(def_block, def_pos) => {
            if def_block == use_block.id {
                // Within the same block, the definition must come
                // before the use.
                match use_pos {
                    Some(p) if def_pos >= p => Err(format!(
                        "{used} used at op #{p} but defined at op #{def_pos} (must be earlier)"
                    )),
                    _ => Ok(()),
                }
            } else if dom.strictly_dominates(def_block, use_block.id) {
                Ok(())
            } else {
                Err(format!(
                    "{used} defined in {def_block} but used in {} — not dominated",
                    use_block.id
                ))
            }
        }
    }
}

/// Which ValueIds does this op read?
fn op_uses(op: &IrOp) -> Vec<ValueId> {
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

fn collect_void_values(func: &IrFunction) -> HashSet<ValueId> {
    let mut out = HashSet::new();
    for block in &func.blocks {
        for (vid, op) in &block.ops {
            if op.is_void() {
                out.insert(*vid);
            }
        }
    }
    out
}

// ─── Dominance computation ─────────────────────────────────────────────────
//
// Phase-1-grade iterative fixpoint. For any sensible function this
// runs in microseconds; phase 4 swaps in Lengauer–Tarjan when compile
// time starts to matter.

struct Dominance {
    /// `doms[i]` = set of blocks that dominate block i (including i
    /// itself). A block B1 strictly dominates B2 iff B1 ∈ doms[B2]
    /// and B1 ≠ B2.
    doms: Vec<HashSet<BlockId>>,
}

impl Dominance {
    fn strictly_dominates(&self, a: BlockId, b: BlockId) -> bool {
        if a == b {
            return false;
        }
        self.doms
            .get(b.0 as usize)
            .map(|s| s.contains(&a))
            .unwrap_or(false)
    }
}

fn compute_dominators(blocks: &[Block]) -> Dominance {
    let n = blocks.len();
    // Start: entry dominates only itself; every other block is
    // dominated by every block (we shrink toward the fixpoint).
    let all: HashSet<BlockId> = (0..n as u32).map(BlockId).collect();
    let mut doms: Vec<HashSet<BlockId>> = (0..n)
        .map(|i| {
            if i == 0 {
                let mut s = HashSet::new();
                s.insert(BlockId(0));
                s
            } else {
                all.clone()
            }
        })
        .collect();

    // Compute predecessor sets once.
    let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); n];
    for (i, block) in blocks.iter().enumerate() {
        let me = BlockId(i as u32);
        match &block.term {
            Terminator::Jump(t, _) => push_pred(&mut preds, *t, me),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                push_pred(&mut preds, *then_block, me);
                push_pred(&mut preds, *else_block, me);
            }
            _ => {}
        }
    }

    // Iterate.
    let mut changed = true;
    while changed {
        changed = false;
        for i in 1..n {
            let i_id = BlockId(i as u32);
            let my_preds = &preds[i];
            if my_preds.is_empty() {
                continue;
            }
            // new_dom = {i} ∪ ⋂_{p ∈ preds} dom(p)
            let mut it = my_preds.iter();
            let first = it.next().unwrap();
            let mut new_dom = doms[first.0 as usize].clone();
            for p in it {
                new_dom = new_dom
                    .intersection(&doms[p.0 as usize])
                    .copied()
                    .collect();
            }
            new_dom.insert(i_id);
            if new_dom != doms[i] {
                doms[i] = new_dom;
                changed = true;
            }
        }
    }

    Dominance { doms }
}

fn push_pred(preds: &mut [Vec<BlockId>], block: BlockId, me: BlockId) {
    if let Some(list) = preds.get_mut(block.0 as usize) {
        list.push(me);
    }
}
