//! Function inlining pass.
//!
//! Takes each `IrOp::CallValue(callee, args)` whose callee can be
//! statically traced to a `MakeClosureNoCapture` or a
//! `LoadConst`-of-`CompiledFunction`, and splices the callee's IR
//! body directly into the caller. The tier-2 emitter then never
//! emits a `djit_call_helper` invocation for the inlined site — the
//! callee's arithmetic runs as native code inside the caller's
//! function body.
//!
//! ## Scope
//!
//! Phase 12 handles the single-block-callee case only. Multi-block
//! callees (functions with internal control flow) are left in place
//! until a follow-up phase wires up Jump / Branch splicing. Recursive
//! and generic callees are rejected at the heuristic stage.
//!
//! ## Algorithm
//!
//! For each inlineable `CallValue` site:
//!
//! 1. **Provenance**: walk the callee-`ValueId`'s def-use chain —
//!    backward through block-parameter edges, with cycle detection for
//!    loop-carried values — until every path terminates at the same
//!    `MakeClosureNoCapture(const_idx)`. Anything else (multiple
//!    targets, an unresolvable origin) aborts the inline.
//! 2. **Translate** the callee's bytecode into a fresh `IrFunction`.
//! 3. **Heuristic check**: reject if the callee has more than
//!    [`MAX_INLINE_OPS`] ops, any nested call ops, multi-block
//!    control flow, async / generator / rest-param shape, or a
//!    bare `Return(None)` (treated as deoptimising for now).
//! 4. **Splice**: the callee's entry-block params map to the
//!    caller's call args (plus a synthesised `this`/undefined
//!    filler for registers the caller didn't pass). Each callee op
//!    is rewritten with a fresh caller-space `ValueId` and
//!    appended to the caller's block at the `CallValue` site. A
//!    final `Copy(return_vid → call_result)` preserves the
//!    original op's result `ValueId` so downstream uses don't
//!    need rewriting.
//!
//! Downstream passes (`const_fold`, `copy_prop`, `dce`,
//! `cfg_simplify`) clean up the merged IR — inlining almost always
//! reveals new folding and dead-code opportunities.

use rustc_hash::FxHashMap;

use crate::object::Object;

use super::super::types::{DeoptId, DeoptPoint, IrOp, Terminator, ValueType};
use super::super::{BlockId, IrFunction, ValueId};

/// Maximum total op count of a callee that's still eligible for
/// inlining. Small-functions-only keeps the heuristic simple and
/// avoids exploding the caller's IR — the typical inlinee (like
/// `function(a, b) { return a + b; }`) is ≤ 10 ops after
/// translation. Tunable; higher values broaden coverage at the
/// cost of larger emitted native code.
pub const MAX_INLINE_OPS: usize = 30;

/// Run the inline pass over `func`. Returns the number of call sites
/// that were successfully inlined.
pub fn run(func: &mut IrFunction) -> usize {
    let sites = find_inline_sites(func);
    let mut inlined = 0;
    // Process sites in reverse order so earlier block/op indices
    // remain valid as we mutate later ones.
    for (block_idx, op_idx, const_idx) in sites.into_iter().rev() {
        if try_inline_site(func, block_idx, op_idx, const_idx) {
            inlined += 1;
        }
    }
    inlined
}

fn find_inline_sites(func: &IrFunction) -> Vec<(usize, usize, u32)> {
    let mut sites = Vec::new();
    for (b_idx, block) in func.blocks.iter().enumerate() {
        for (o_idx, (_, op)) in block.ops.iter().enumerate() {
            if let IrOp::CallValue(callee, _) = op {
                if let Some(const_idx) = resolve_callee(func, *callee) {
                    sites.push((b_idx, o_idx, const_idx));
                }
            }
        }
    }
    sites
}

/// Trace `vid` back to a `MakeClosureNoCapture(const_idx)` by
/// walking def-use chains. Returns `Some(idx)` iff every path ends
/// at the same constant index. Self-referential block-param cycles
/// (loop-carried values that never change) are ignored.
///
/// Also handles `LoadGlobal(slot)` when the function writes that
/// slot exactly once and the store's source traces to a
/// `MakeClosureNoCapture` — the common case for
/// `let add = function(...)` inside a function body, where the
/// bytecode compiler may promote `add` to a function-scope global.
fn resolve_callee(func: &IrFunction, vid: ValueId) -> Option<u32> {
    use std::collections::HashSet;
    let mut seen: HashSet<ValueId> = HashSet::new();
    let mut worklist: Vec<ValueId> = vec![vid];
    let mut result: Option<u32> = None;
    while let Some(v) = worklist.pop() {
        if !seen.insert(v) {
            continue;
        }
        if let Some(op) = find_op_def(func, v) {
            match op {
                IrOp::MakeClosureNoCapture(idx) => match result {
                    None => result = Some(*idx),
                    Some(prev) if prev == *idx => {}
                    _ => return None,
                },
                IrOp::Copy(src) => worklist.push(*src),
                IrOp::LoadGlobal(slot) => {
                    match resolve_single_global_writer(func, *slot) {
                        Some(src_vid) => worklist.push(src_vid),
                        None => return None,
                    }
                }
                _ => return None,
            }
        } else if let Some((block_id, param_idx)) = find_block_param_def(func, v) {
            for pred_id in find_predecessors(func, block_id) {
                let pred_block = &func.blocks[pred_id.0 as usize];
                let arg = match &pred_block.term {
                    Terminator::Jump(target, args) if *target == block_id => {
                        if param_idx < args.len() {
                            args[param_idx]
                        } else {
                            continue;
                        }
                    }
                    Terminator::Branch {
                        then_block,
                        then_args,
                        else_block,
                        else_args,
                        ..
                    } => {
                        if *then_block == block_id && param_idx < then_args.len() {
                            then_args[param_idx]
                        } else if *else_block == block_id && param_idx < else_args.len() {
                            else_args[param_idx]
                        } else {
                            continue;
                        }
                    }
                    _ => continue,
                };
                worklist.push(arg);
            }
        } else {
            // Unresolvable origin.
            return None;
        }
    }
    result
}

/// Return the source ValueId of the sole `StoreGlobal(slot, v)` op
/// in `func`, or `None` if the global is written zero or multiple
/// times (ambiguous). Enables closure-through-global resolution for
/// `let f = function(...)` patterns the compiler promotes to
/// function-scope globals.
fn resolve_single_global_writer(func: &IrFunction, slot: u16) -> Option<ValueId> {
    let mut writer: Option<ValueId> = None;
    for block in &func.blocks {
        for (_, op) in &block.ops {
            if let IrOp::StoreGlobal(s, v) = op {
                if *s == slot {
                    if writer.is_some() {
                        return None;
                    }
                    writer = Some(*v);
                }
            }
        }
    }
    writer
}

fn find_op_def(func: &IrFunction, vid: ValueId) -> Option<&IrOp> {
    for block in &func.blocks {
        for (v, op) in &block.ops {
            if *v == vid {
                return Some(op);
            }
        }
    }
    None
}

fn find_block_param_def(func: &IrFunction, vid: ValueId) -> Option<(BlockId, usize)> {
    for block in &func.blocks {
        for (idx, (pvid, _)) in block.params.iter().enumerate() {
            if *pvid == vid {
                return Some((block.id, idx));
            }
        }
    }
    None
}

fn find_predecessors(func: &IrFunction, target: BlockId) -> Vec<BlockId> {
    let mut preds = Vec::new();
    for block in &func.blocks {
        match &block.term {
            Terminator::Jump(t, _) if *t == target => preds.push(block.id),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                if *then_block == target || *else_block == target {
                    preds.push(block.id);
                }
            }
            _ => {}
        }
    }
    preds
}

/// Try to inline the `CallValue` op at `(block_idx, op_idx)`.
/// Returns `true` iff the inline succeeded and the IR was mutated.
fn try_inline_site(
    func: &mut IrFunction,
    block_idx: usize,
    op_idx: usize,
    const_idx: u32,
) -> bool {
    // Verify the op is still a CallValue (sibling inlines could have
    // rewritten it, though we process in reverse to avoid that).
    let (call_result, call_args) = {
        let block = &func.blocks[block_idx];
        if op_idx >= block.ops.len() {
            return false;
        }
        let (vid, op) = &block.ops[op_idx];
        match op {
            IrOp::CallValue(_, args) => (*vid, args.clone()),
            _ => return false,
        }
    };

    // Extract the compiled function from the caller's constants.
    let (callee_instructions, callee_constants, reg_count, num_params, takes_this) = {
        let obj = func.constants.get(const_idx as usize);
        let cf = match obj {
            Some(Object::CompiledFunction(cf)) => cf,
            _ => return false,
        };
        if cf.is_async || cf.is_generator || cf.rest_parameter_index.is_some() {
            return false;
        }
        (
            cf.instructions.clone(),
            cf.constants.clone(),
            cf.register_count,
            cf.num_parameters as u16,
            cf.takes_this,
        )
    };

    // Translate callee's bytecode to IR.
    let mut callee_ir = match super::super::build::translate(
        &callee_instructions,
        callee_constants,
        reg_count,
        num_params,
    ) {
        Ok(f) => f,
        Err(_) => return false,
    };
    if super::super::verify::verify(&callee_ir).is_err() {
        return false;
    }

    // Clean up the callee: the translator emits unreachable blocks
    // after a trailing Halt / ReturnUndef in the bytecode, which
    // defeats the single-block heuristic. Run DCE + CFG simplify to
    // collapse those before we inspect the shape.
    super::dce::run(&mut callee_ir);
    super::cfg_simplify::run(&mut callee_ir);

    // Heuristic: reject multi-block callees (phase 12 scope), nested
    // calls, and excessive size.
    if callee_ir.blocks.len() != 1 {
        return false;
    }
    let callee_block = &callee_ir.blocks[0];
    if callee_block.ops.len() > MAX_INLINE_OPS {
        return false;
    }
    for (_, op) in &callee_block.ops {
        if matches!(
            op,
            IrOp::CallValue(..) | IrOp::CallRuntime(..) | IrOp::MakeClosureNoCapture(..)
        ) {
            return false;
        }
    }
    let return_vid = match &callee_block.term {
        Terminator::Return(Some(v)) => *v,
        // Return(None) would need a synthesised ConstUndef at the
        // splice point; skip for now to keep phase 12 focused on
        // the common case.
        _ => return false,
    };

    // ── Build caller→callee VID map ─────────────────────────────
    //
    // The callee's entry block has `reg_count` params. Caller
    // passes `call_args.len()` actual args; remaining registers are
    // filled with a synthesised `ConstUndef` in the caller.
    //
    // Layout (mirrors `djit_call_helper`):
    //   takes_this == false: reg[0..num_args] = call_args
    //                        reg[num_args..reg_count] = Undefined
    //   takes_this == true:  reg[0] = Undefined (`this` default)
    //                        reg[1..=num_args] = call_args
    //                        reg[num_args+1..reg_count] = Undefined

    let arg_offset_i = if takes_this { 1usize } else { 0usize };
    let mut next_vid = func.num_values() as u32;

    // New ops that will precede the CallValue's position (filler
    // `ConstUndef` values for register slots we don't pass).
    let mut prefix_ops: Vec<(ValueId, IrOp)> = Vec::new();

    // For each callee-entry-block param slot, decide what caller VID
    // should substitute for it.
    let mut param_subst: Vec<ValueId> = Vec::with_capacity(callee_block.params.len());
    for i in 0..callee_block.params.len() {
        let caller_vid = if takes_this && i == 0 {
            let v = ValueId(next_vid);
            next_vid += 1;
            prefix_ops.push((v, IrOp::ConstUndef));
            v
        } else {
            let arg_i = if takes_this { i - 1 } else { i };
            if arg_i < call_args.len() {
                call_args[arg_i]
            } else {
                let v = ValueId(next_vid);
                next_vid += 1;
                prefix_ops.push((v, IrOp::ConstUndef));
                v
            }
        };
        param_subst.push(caller_vid);
    }

    // Build the callee → caller VID map.
    let mut vid_map: FxHashMap<ValueId, ValueId> = FxHashMap::default();
    for ((param_vid, _), &subst) in callee_block.params.iter().zip(param_subst.iter()) {
        vid_map.insert(*param_vid, subst);
    }
    // Every callee op gets a freshly-allocated caller-space VID.
    for (op_vid, _) in &callee_block.ops {
        vid_map.insert(*op_vid, ValueId(next_vid));
        next_vid += 1;
    }

    // DeoptId offset: callee deopt_points get appended to caller's.
    let deopt_offset = func.deopt_points.len() as u32;

    // Rewrite each callee op into the caller's namespace.
    let mut inlined_ops: Vec<(ValueId, IrOp)> = Vec::with_capacity(callee_block.ops.len());
    for (orig_vid, op) in &callee_block.ops {
        let new_vid = vid_map[orig_vid];
        let new_op = rewrite_op(op, &vid_map, deopt_offset);
        inlined_ops.push((new_vid, new_op));
    }

    // Final op: Copy(caller_return_vid) → call_result, so downstream
    // uses of the original CallValue result keep referencing the
    // same `ValueId`. A later copy_prop collapses this.
    let caller_return_vid = resolve_in_map(&vid_map, return_vid);
    let closing_copy = (call_result, IrOp::Copy(caller_return_vid));

    // ── Splice into the caller block ────────────────────────────
    let block = &mut func.blocks[block_idx];
    // Remove the CallValue op itself.
    block.ops.remove(op_idx);
    // Insert prefix (Undefined fillers) + inlined callee ops +
    // closing copy, all at the CallValue's original position.
    let mut new_ops: Vec<(ValueId, IrOp)> =
        Vec::with_capacity(prefix_ops.len() + inlined_ops.len() + 1);
    new_ops.extend(prefix_ops);
    new_ops.extend(inlined_ops);
    new_ops.push(closing_copy);
    // splice at op_idx
    block.ops.splice(op_idx..op_idx, new_ops);

    // Append callee's deopt points (renumbered).
    for dp in callee_ir.deopt_points {
        func.deopt_points.push(DeoptPoint {
            id: DeoptId(dp.id.0 + deopt_offset),
            bytecode_ip: dp.bytecode_ip,
            live_regs: dp.live_regs,
        });
    }

    true
}

fn resolve_in_map(map: &FxHashMap<ValueId, ValueId>, v: ValueId) -> ValueId {
    *map.get(&v).unwrap_or(&v)
}

/// Deep-rewrite a single callee op into the caller's VID namespace.
fn rewrite_op(op: &IrOp, map: &FxHashMap<ValueId, ValueId>, deopt_offset: u32) -> IrOp {
    let sv = |v: ValueId| resolve_in_map(map, v);
    let sd = |d: DeoptId| DeoptId(d.0 + deopt_offset);
    match op {
        IrOp::ConstI32(v) => IrOp::ConstI32(*v),
        IrOp::ConstF64(v) => IrOp::ConstF64(*v),
        IrOp::ConstBool(v) => IrOp::ConstBool(*v),
        IrOp::ConstNull => IrOp::ConstNull,
        IrOp::ConstUndef => IrOp::ConstUndef,
        IrOp::ConstValue(v) => IrOp::ConstValue(*v),
        IrOp::LoadReg(r) => IrOp::LoadReg(*r),
        IrOp::AddI32(a, b) => IrOp::AddI32(sv(*a), sv(*b)),
        IrOp::SubI32(a, b) => IrOp::SubI32(sv(*a), sv(*b)),
        IrOp::MulI32(a, b) => IrOp::MulI32(sv(*a), sv(*b)),
        IrOp::CheckedAddI32(a, b, d) => IrOp::CheckedAddI32(sv(*a), sv(*b), sd(*d)),
        IrOp::CheckedSubI32(a, b, d) => IrOp::CheckedSubI32(sv(*a), sv(*b), sd(*d)),
        IrOp::CheckedMulI32(a, b, d) => IrOp::CheckedMulI32(sv(*a), sv(*b), sd(*d)),
        IrOp::AddF64(a, b) => IrOp::AddF64(sv(*a), sv(*b)),
        IrOp::SubF64(a, b) => IrOp::SubF64(sv(*a), sv(*b)),
        IrOp::MulF64(a, b) => IrOp::MulF64(sv(*a), sv(*b)),
        IrOp::DivF64(a, b) => IrOp::DivF64(sv(*a), sv(*b)),
        IrOp::AddGeneric(a, b) => IrOp::AddGeneric(sv(*a), sv(*b)),
        IrOp::SubGeneric(a, b) => IrOp::SubGeneric(sv(*a), sv(*b)),
        IrOp::MulGeneric(a, b) => IrOp::MulGeneric(sv(*a), sv(*b)),
        IrOp::DivGeneric(a, b) => IrOp::DivGeneric(sv(*a), sv(*b)),
        IrOp::ModGeneric(a, b) => IrOp::ModGeneric(sv(*a), sv(*b)),
        IrOp::NegI32(v) => IrOp::NegI32(sv(*v)),
        IrOp::NegF64(v) => IrOp::NegF64(sv(*v)),
        IrOp::NotBool(v) => IrOp::NotBool(sv(*v)),
        IrOp::EqI32(a, b) => IrOp::EqI32(sv(*a), sv(*b)),
        IrOp::NeI32(a, b) => IrOp::NeI32(sv(*a), sv(*b)),
        IrOp::LtI32(a, b) => IrOp::LtI32(sv(*a), sv(*b)),
        IrOp::LeI32(a, b) => IrOp::LeI32(sv(*a), sv(*b)),
        IrOp::GtI32(a, b) => IrOp::GtI32(sv(*a), sv(*b)),
        IrOp::GeI32(a, b) => IrOp::GeI32(sv(*a), sv(*b)),
        IrOp::EqValue(a, b) => IrOp::EqValue(sv(*a), sv(*b)),
        IrOp::NeValue(a, b) => IrOp::NeValue(sv(*a), sv(*b)),
        IrOp::LooseEqValue(a, b) => IrOp::LooseEqValue(sv(*a), sv(*b)),
        IrOp::LtValue(a, b) => IrOp::LtValue(sv(*a), sv(*b)),
        IrOp::LeValue(a, b) => IrOp::LeValue(sv(*a), sv(*b)),
        IrOp::CheckI32(v, d) => IrOp::CheckI32(sv(*v), sd(*d)),
        IrOp::CheckF64(v, d) => IrOp::CheckF64(sv(*v), sd(*d)),
        IrOp::CheckHeap(v, d) => IrOp::CheckHeap(sv(*v), sd(*d)),
        IrOp::CheckHeapShape(v, s, d) => IrOp::CheckHeapShape(sv(*v), *s, sd(*d)),
        IrOp::CheckFunctionIs(v, bits, d) => IrOp::CheckFunctionIs(sv(*v), *bits, sd(*d)),
        IrOp::BoxI32(v) => IrOp::BoxI32(sv(*v)),
        IrOp::BoxF64(v) => IrOp::BoxF64(sv(*v)),
        IrOp::BoxBool(v) => IrOp::BoxBool(sv(*v)),
        IrOp::UnboxI32(v) => IrOp::UnboxI32(sv(*v)),
        IrOp::UnboxF64(v) => IrOp::UnboxF64(sv(*v)),
        IrOp::UnboxBool(v) => IrOp::UnboxBool(sv(*v)),
        IrOp::LoadSlot(v, off, ty) => IrOp::LoadSlot(sv(*v), *off, *ty),
        IrOp::StoreSlot(o, off, val) => IrOp::StoreSlot(sv(*o), *off, sv(*val)),
        IrOp::LoadGlobal(slot) => IrOp::LoadGlobal(*slot),
        IrOp::StoreGlobal(slot, v) => IrOp::StoreGlobal(*slot, sv(*v)),
        IrOp::CallRuntime(h, args) => {
            IrOp::CallRuntime(*h, args.iter().map(|v| sv(*v)).collect())
        }
        IrOp::CallValue(callee, args) => IrOp::CallValue(
            sv(*callee),
            args.iter().map(|v| sv(*v)).collect(),
        ),
        IrOp::MakeClosureNoCapture(idx) => IrOp::MakeClosureNoCapture(*idx),
        IrOp::Copy(v) => IrOp::Copy(sv(*v)),
    }
}

// Silence "ValueType unused" — it's referenced via types in the
// heuristic check for completeness but not pattern-matched above.
#[allow(dead_code)]
const _PHANTOM: ValueType = ValueType::Value;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::tier2::ir::types::Block;
    use std::rc::Rc;

    fn empty_func() -> IrFunction {
        IrFunction {
            bytecode_len: 0,
            num_bytecode_regs: 0,
            num_parameters: 0,
            blocks: vec![Block {
                id: BlockId(0),
                params: Vec::new(),
                ops: Vec::new(),
                term: Terminator::Return(None),
            }],
            deopt_points: Vec::new(),
            constants: Rc::new(Vec::new()),
        }
    }

    #[test]
    fn no_callvalue_means_no_sites() {
        let func = empty_func();
        assert!(find_inline_sites(&func).is_empty());
    }

    #[test]
    fn run_is_zero_on_callvalueless_function() {
        let mut func = empty_func();
        assert_eq!(run(&mut func), 0);
    }

    #[test]
    fn resolve_callee_on_direct_make_closure() {
        // bb0:
        //   v0 = MakeClosureNoCapture(const_idx = 7)
        //   v1 = CallValue(v0, [])  — we don't actually check CallValue
        //                             here, just provenance of v0.
        //   return v1
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::MakeClosureNoCapture(7)),
                (v1, IrOp::CallValue(v0, vec![])),
            ],
            term: Terminator::Return(Some(v1)),
        }];
        let func = IrFunction {
            bytecode_len: 0,
            num_bytecode_regs: 0,
            num_parameters: 0,
            blocks,
            deopt_points: Vec::new(),
            constants: Rc::new(Vec::new()),
        };
        assert_eq!(resolve_callee(&func, v0), Some(7));
    }

    #[test]
    fn run_inlines_callvalue_with_direct_makeclosure() {
        // bb0:
        //   v0 = MakeClosureNoCapture(0)       // `add`
        //   v1 = ConstI32(3)
        //   v2 = ConstI32(4)
        //   v3 = CallValue(v0, [v1, v2])
        //   return v3
        //
        // After inlining, v3 should be Copy of the add's return
        // value, not a CallValue.
        //
        // We need a CompiledFunction at constants[0] representing
        // `function(a, b) { return a + b; }`. Build that minimally.
        use crate::backend::rcode::ROp;
        use crate::object::CompiledFunctionObject;
        let mut add_bytecode: Vec<u8> = Vec::new();
        // Add r2, r0, r1  [dst:2, left:2, right:2]
        add_bytecode.push(ROp::Add as u8);
        add_bytecode.extend_from_slice(&[0, 2, 0, 0, 0, 1]);
        // Return r2  [src:2]
        add_bytecode.push(ROp::Return as u8);
        add_bytecode.extend_from_slice(&[0, 2]);
        let add_cf = CompiledFunctionObject {
            instructions: Rc::new(add_bytecode),
            constants: Rc::new(Vec::new()),
            num_locals: 0,
            num_parameters: 2,
            rest_parameter_index: None,
            takes_this: false,
            is_async: false,
            is_generator: false,
            num_cache_slots: 0,
            max_stack_depth: 0,
            register_count: 3,
            inline_cache: Rc::new(crate::object::VmCell::new(Vec::new())),
            closure_captures: Vec::new(),
            captured_values: Vec::new(),
            properties: None,
        };
        let constants = Rc::new(vec![Object::CompiledFunction(Box::new(add_cf))]);

        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let v3 = ValueId(3);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::MakeClosureNoCapture(0)),
                (v1, IrOp::ConstI32(3)),
                (v2, IrOp::ConstI32(4)),
                (v3, IrOp::CallValue(v0, vec![v1, v2])),
            ],
            term: Terminator::Return(Some(v3)),
        }];
        let mut func = IrFunction {
            bytecode_len: 0,
            num_bytecode_regs: 0,
            num_parameters: 0,
            blocks,
            deopt_points: Vec::new(),
            constants,
        };

        let n_inlined = run(&mut func);
        assert_eq!(n_inlined, 1, "expected one inline");

        // After inlining, the block should no longer contain a
        // CallValue. It should contain AddGeneric (the inlined
        // callee body) and a final Copy linking the result to v3.
        let block = &func.blocks[0];
        for (_, op) in &block.ops {
            assert!(
                !matches!(op, IrOp::CallValue(..)),
                "CallValue should have been inlined away, found: {:?}",
                op
            );
        }
        // The closing Copy that preserves `v3` should be present.
        let has_v3_copy = block.ops.iter().any(|(vid, op)| {
            *vid == v3 && matches!(op, IrOp::Copy(_))
        });
        assert!(has_v3_copy, "expected closing Copy for original result v3");
    }

    #[test]
    fn resolve_callee_returns_none_for_unknown_shape() {
        // v0 = LoadGlobal(3) — can't resolve statically.
        let v0 = ValueId(0);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![(v0, IrOp::LoadGlobal(3))],
            term: Terminator::Return(Some(v0)),
        }];
        let func = IrFunction {
            bytecode_len: 0,
            num_bytecode_regs: 0,
            num_parameters: 0,
            blocks,
            deopt_points: Vec::new(),
            constants: Rc::new(Vec::new()),
        };
        assert_eq!(resolve_callee(&func, v0), None);
    }
}
