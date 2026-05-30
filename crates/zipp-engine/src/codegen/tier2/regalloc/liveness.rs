//! Liveness analysis.
//!
//! For each block, compute:
//!
//! * **live-in**  — ValueIds used at some op or terminator within the
//!   block or its reachable successors *before being redefined*.
//! * **live-out** — union of live-ins of successors, adjusted for the
//!   parallel moves implied by block-param args.
//!
//! Standard backward dataflow:
//!
//! ```text
//!   live_out(B) = Union over successors S of (
//!                   live_in(S)
//!                   − S.params             ; moved into via jump arg
//!                   ∪ jump_args_passed_to(S) ; the arg values themselves
//!                                              are live at B's exit
//!                 )
//!   live_in(B)  = use(B) ∪ (live_out(B) − def(B))
//! ```
//!
//! where `use(B)` is the set of values referenced by ops/terminator
//! BEFORE being defined within the block, and `def(B)` is the set of
//! values defined by ops or block parameters of B.
//!
//! The block-params-as-phi treatment deserves explaining: when B jumps
//! to S, each value in `jump_args` is live at B's exit *because* S's
//! block params will reference (via the edge ABI) whatever values B
//! passes. From S's perspective the block-param is a freshly-defined
//! value that's alive starting at op 0 — but the arg *producing* it
//! must stay live until the jump site, which is B's terminator. That's
//! why live-out includes jump args and excludes the corresponding
//! successor params.
//!
//! ## Complexity
//!
//! Iterative fixed-point over block-level sets. O(blocks · iters)
//! per round, bounded by O(V · B) where V is the value count and B
//! the block count. For the tier-2 target of ≤ 500-byte functions
//! this is microseconds — phase 4a doesn't need a fancier worklist
//! algorithm.

use std::collections::{BTreeSet, VecDeque};

use super::super::ir::{BlockId, IrFunction, IrOp, Terminator, ValueId};

/// Per-block liveness information.
#[derive(Clone, Debug, Default)]
pub struct BlockLiveness {
    pub live_in: BTreeSet<ValueId>,
    pub live_out: BTreeSet<ValueId>,
}

/// Liveness sets keyed by BlockId (dense — indexed by `block.0`).
pub type Liveness = Vec<BlockLiveness>;

/// Compute liveness for every block in `func`, using `block_order` as
/// the function's linearization.
pub fn compute(func: &IrFunction, _block_order: &[BlockId]) -> Liveness {
    let n = func.blocks.len();
    let mut live: Liveness = vec![BlockLiveness::default(); n];

    // Compute use / def sets once up front — they don't change.
    let mut uses: Vec<BTreeSet<ValueId>> = vec![BTreeSet::new(); n];
    let mut defs: Vec<BTreeSet<ValueId>> = vec![BTreeSet::new(); n];
    for (idx, block) in func.blocks.iter().enumerate() {
        let (u, d) = use_def(block);
        uses[idx] = u;
        defs[idx] = d;
    }

    // Iterate backward dataflow until stable. Worklist starts with
    // every block; we walk it in reverse index order which mirrors
    // reverse-postorder closely enough for a typical CFG and keeps
    // iteration count small.
    let mut worklist: VecDeque<BlockId> = (0..n as u32).rev().map(BlockId).collect();
    let preds = compute_preds(func);

    while let Some(b) = worklist.pop_front() {
        let block = &func.blocks[b.0 as usize];

        // live_out(B) from successors.
        let new_out = compute_live_out(func, block, &live);

        // live_in(B) = use(B) ∪ (live_out(B) − def(B))
        let mut new_in = uses[b.0 as usize].clone();
        for v in new_out.difference(&defs[b.0 as usize]) {
            new_in.insert(*v);
        }

        let changed = new_in != live[b.0 as usize].live_in
            || new_out != live[b.0 as usize].live_out;
        if changed {
            live[b.0 as usize].live_in = new_in;
            live[b.0 as usize].live_out = new_out;
            // Predecessors may now need re-evaluation.
            if let Some(ps) = preds.get(&b) {
                for p in ps {
                    if !worklist.contains(p) {
                        worklist.push_back(*p);
                    }
                }
            }
        }
    }

    live
}

/// Reverse-postorder linearisation — the conventional shape for
/// backward dataflow and for emitting code where fall-through is
/// more common than branching.
///
/// Computed via iterative DFS from block 0. Unreachable blocks are
/// omitted — callers should have run `cfg_simplify` first if they
/// care about those.
pub fn compute_rpo(func: &IrFunction) -> Vec<BlockId> {
    if func.blocks.is_empty() {
        return Vec::new();
    }
    let n = func.blocks.len();
    let mut visited = vec![false; n];
    let mut postorder = Vec::with_capacity(n);
    // Iterative DFS with (block, child_cursor) frames.
    let mut stack: Vec<(BlockId, usize)> = vec![(BlockId(0), 0)];
    visited[0] = true;
    while let Some(frame) = stack.last_mut() {
        let b = frame.0;
        let cursor = frame.1;
        let succs = term_successors(&func.blocks[b.0 as usize].term);
        if cursor < succs.len() {
            frame.1 += 1;
            let next = succs[cursor];
            if (next.0 as usize) < n && !visited[next.0 as usize] {
                visited[next.0 as usize] = true;
                stack.push((next, 0));
            }
        } else {
            postorder.push(b);
            stack.pop();
        }
    }
    postorder.reverse();
    postorder
}

// ─── Internal helpers ───────────────────────────────────────────────────────

fn compute_live_out(
    func: &IrFunction,
    block: &super::super::ir::Block,
    live: &Liveness,
) -> BTreeSet<ValueId> {
    let mut out = BTreeSet::new();
    match &block.term {
        Terminator::Return(Some(v)) => {
            out.insert(*v);
        }
        Terminator::Return(None) | Terminator::Deopt(_) | Terminator::Unreachable => {}
        Terminator::Jump(target, args) => {
            contribute_edge(func, *target, args, live, &mut out);
        }
        Terminator::Branch {
            cond,
            then_block,
            then_args,
            else_block,
            else_args,
        } => {
            out.insert(*cond);
            contribute_edge(func, *then_block, then_args, live, &mut out);
            contribute_edge(func, *else_block, else_args, live, &mut out);
        }
    }
    out
}

/// Compute this edge's contribution to the predecessor's live-out:
/// successor's live-in minus its params, plus the jump args.
fn contribute_edge(
    func: &IrFunction,
    target: BlockId,
    args: &[ValueId],
    live: &Liveness,
    out: &mut BTreeSet<ValueId>,
) {
    let target_block = &func.blocks[target.0 as usize];
    let target_params: BTreeSet<ValueId> = target_block.params.iter().map(|(v, _)| *v).collect();
    for v in live[target.0 as usize]
        .live_in
        .difference(&target_params)
    {
        out.insert(*v);
    }
    for v in args {
        out.insert(*v);
    }
}

/// Per-block use/def sets.
///
/// `use` = values referenced before being defined *within this block*.
/// `def` = values defined by ops or block parameters of this block.
fn use_def(block: &super::super::ir::Block) -> (BTreeSet<ValueId>, BTreeSet<ValueId>) {
    let mut uses = BTreeSet::new();
    let mut defs = BTreeSet::new();
    // Block params are defs on entry.
    for (v, _) in &block.params {
        defs.insert(*v);
    }
    // Walk ops in order: reads count as uses if the value isn't
    // already defined locally.
    for (vid, op) in &block.ops {
        for u in op_reads(op) {
            if !defs.contains(&u) {
                uses.insert(u);
            }
        }
        defs.insert(*vid);
    }
    // Terminator reads count as uses if not locally defined.
    for u in term_reads(&block.term) {
        if !defs.contains(&u) {
            uses.insert(u);
        }
    }
    (uses, defs)
}

fn op_reads(op: &IrOp) -> Vec<ValueId> {
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

fn term_reads(t: &Terminator) -> Vec<ValueId> {
    match t {
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

fn term_successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Return(_) | Terminator::Deopt(_) | Terminator::Unreachable => vec![],
        Terminator::Jump(b, _) => vec![*b],
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
    }
}

fn compute_preds(func: &IrFunction) -> std::collections::HashMap<BlockId, Vec<BlockId>> {
    let mut preds: std::collections::HashMap<BlockId, Vec<BlockId>> = Default::default();
    for (i, block) in func.blocks.iter().enumerate() {
        let me = BlockId(i as u32);
        for s in term_successors(&block.term) {
            preds.entry(s).or_default().push(me);
        }
    }
    preds
}
