//! Strength reduction on induction variables.
//!
//! Recognises `v = IV * const_k` inside a loop body and rewrites it
//! into an auxiliary IV:
//!
//! ```text
//!   original:
//!     header: params(i: val, ...)
//!       body:  v = i * k        ; ← per-iter multiply
//!
//!   after strength reduction:
//!     header: params(i: val, j: val, ...)
//!       body:  (no multiply)
//!     latch:  i' = i + step
//!             j' = j + step*k   ; ← strength-reduced update
//!             jump header(i', j', ...)
//! ```
//!
//! The payoff is per-iteration: one `mul` becomes one `add` (same
//! cycle cost on modern x86_64, but the add typically folds into
//! an existing LEA or fused macro-op). More importantly, downstream
//! passes can see `v` as a simple inductive value and apply the same
//! analyses they'd apply to `i` — opens up loop-bound simplification
//! and eventual overflow elision.
//!
//! ## Preconditions
//!
//! * The loop must have a unique pre-header (one non-body predecessor
//!   of the header). Without one we can't synthesise the initial
//!   `init * k` computation.
//! * The BIV must have a compile-time-integer step. Non-constant
//!   steps are loop-invariant but need a `step * k` computation we'd
//!   have to hoist; deferred to a later iteration of this pass.
//! * The factor `k` must be a compile-time `ConstI32`.
//!
//! These rules keep the first cut safe and rewrite-local. Broader
//! patterns (non-const steps, nested strength reduction) are
//! phase-7.5 additions.

use std::collections::BTreeSet;

use super::super::iv::{find_basic_ivs, find_preheader, find_reducible_muls, BasicIv, ReducibleMul};
use super::super::loops::{find_loops, Loop};
use super::super::types::{BlockId, IrFunction, IrOp, Terminator, ValueId, ValueType};

/// Run the pass. Returns `true` if any reduction was performed.
pub fn run(func: &mut IrFunction) -> bool {
    let loops = find_loops(func);
    if loops.is_empty() {
        return false;
    }

    // Dedup by header — multiple latches would try to thread new
    // params twice. `find_loops` sorts by (header, latch); keep the
    // first.
    let mut seen = BTreeSet::new();
    let unique: Vec<Loop> = loops.into_iter().filter(|l| seen.insert(l.header)).collect();

    let mut any_reduced = false;
    for l in unique {
        any_reduced |= reduce_in_loop(func, &l);
    }
    any_reduced
}

fn reduce_in_loop(func: &mut IrFunction, l: &Loop) -> bool {
    let Some(preheader) = find_preheader(func, l) else {
        return false;
    };

    let ivs = find_basic_ivs(func, l, preheader);
    if ivs.is_empty() {
        return false;
    }
    let candidates = find_reducible_muls(func, l, &ivs);
    if candidates.is_empty() {
        return false;
    }

    let mut changed = false;
    // Process one candidate at a time. Each reduction can introduce a
    // new block parameter that affects jump arg indices, so we
    // re-collect before the next reduction to pick up the new shape.
    // (Rerun of `find_loops` on the mutated IR is cheap — small
    // functions, fast dominator computation.)
    //
    // Limit iterations to `candidates.len() * 2` as a defensive cap
    // against pathological mutate-and-reclassify loops.
    let max_iters = candidates.len() * 2 + 1;
    for _ in 0..max_iters {
        let loops_now = find_loops(func);
        let Some(l_now) = loops_now.into_iter().find(|x| x.header == l.header) else {
            break;
        };
        let Some(preheader_now) = find_preheader(func, &l_now) else {
            break;
        };
        let ivs_now = find_basic_ivs(func, &l_now, preheader_now);
        let candidates_now = find_reducible_muls(func, &l_now, &ivs_now);
        let Some(cand) = candidates_now.into_iter().next() else {
            break;
        };
        let Some(biv) = ivs_now.get(&cand.biv).cloned() else {
            break;
        };
        if apply_reduction(func, &l_now, preheader_now, &biv, &cand) {
            changed = true;
        } else {
            break;
        }
    }
    changed
}

/// Perform one reduction: replace `result = biv * k` with a new
/// auxiliary IV. Returns true if we actually made the change.
fn apply_reduction(
    func: &mut IrFunction,
    l: &Loop,
    preheader: BlockId,
    biv: &BasicIv,
    cand: &ReducibleMul,
) -> bool {
    // A BIV without a known const step isn't reducible in this first
    // cut — we'd need to emit `step * k` which requires hoisting.
    let Some(const_step) = biv.const_step else {
        return false;
    };
    let factor = cand.factor_const;
    let new_step = const_step.wrapping_mul(factor);

    // Allocate fresh ValueIds we'll need. `IrFunction::num_values()`
    // scans the IR to find the current max — since we haven't
    // mutated yet, each call would return the same value. Allocate
    // sequentially from a single snapshot instead.
    let base = func.num_values() as u32;
    let factor_const = ValueId(base);
    let init_for_aux = ValueId(base + 1);
    let aux_phi = ValueId(base + 2);
    let new_step_const = ValueId(base + 3);
    let update_for_aux = ValueId(base + 4);

    // ── Emit initial `init_for_aux` in the preheader ──
    let preheader_block = &mut func.blocks[preheader.0 as usize];
    preheader_block
        .ops
        .push((factor_const, IrOp::ConstI32(factor)));
    preheader_block
        .ops
        .push((init_for_aux, IrOp::MulGeneric(biv.init, factor_const)));

    // ── Add `aux_phi` to header params ──
    let aux_param_idx = func.blocks[l.header.0 as usize].params.len();
    func.blocks[l.header.0 as usize]
        .params
        .push((aux_phi, ValueType::Value));

    // ── Emit `update_for_aux` in the latch, right after the biv
    //    update op ──
    {
        let latch_block = &mut func.blocks[biv.update_block.0 as usize];
        // Insert new step const + update op immediately AFTER the
        // existing BIV update. Indices shift below.
        let insert_at = biv.update_op_idx + 1;
        latch_block
            .ops
            .insert(insert_at, (new_step_const, IrOp::ConstI32(new_step)));
        latch_block.ops.insert(
            insert_at + 1,
            (update_for_aux, IrOp::AddGeneric(aux_phi, new_step_const)),
        );
    }

    // ── Thread the new arg through every jump into the header ──
    //
    // At the preheader edge: pass `init_for_aux`.
    // At the latch edge: pass `update_for_aux`.
    // Anywhere else: pass `undef` — shouldn't happen for well-
    // structured loops with a single preheader + single latch, but
    // guard anyway.
    let mut undef_for_unknown: Option<ValueId> = None;
    let header_id = l.header;
    for (idx, block) in func.blocks.iter_mut().enumerate() {
        let from = BlockId(idx as u32);
        let is_preheader = from == preheader;
        let is_latch = from == biv.update_block;
        match &mut block.term {
            Terminator::Jump(t, args) if *t == header_id => {
                args.push(pick_arg(
                    is_preheader,
                    is_latch,
                    init_for_aux,
                    update_for_aux,
                    &mut undef_for_unknown,
                    block.id,
                ));
            }
            Terminator::Branch {
                then_block,
                then_args,
                else_block,
                else_args,
                ..
            } => {
                if *then_block == header_id {
                    then_args.push(pick_arg(
                        is_preheader,
                        is_latch,
                        init_for_aux,
                        update_for_aux,
                        &mut undef_for_unknown,
                        block.id,
                    ));
                }
                if *else_block == header_id {
                    else_args.push(pick_arg(
                        is_preheader,
                        is_latch,
                        init_for_aux,
                        update_for_aux,
                        &mut undef_for_unknown,
                        block.id,
                    ));
                }
            }
            _ => {}
        }
    }

    // ── Replace uses of `cand.result` with `aux_phi` and remove the
    //    now-dead multiply op. ──
    //
    // The multiply itself becomes dead after replacement; DCE will
    // remove it on a subsequent pipeline pass.
    let result = cand.result;
    rewrite_uses(func, result, aux_phi);

    // Remove the multiply op (DCE would also do it — but doing it
    // here keeps the IR clean in a single pass, and avoids a
    // verifier warning about a duplicate definition if any later
    // rewrite resurrects the id).
    {
        let block = &mut func.blocks[cand.block.0 as usize];
        block.ops.retain(|(vid, _)| *vid != result);
    }

    // Drop the aux_param_idx variable — the compiler warns on unused
    // locals, but we keep it named so a future pass can verify shape.
    let _ = aux_param_idx;

    true
}

// ─── Small helpers ──────────────────────────────────────────────────────────

fn pick_arg(
    is_preheader: bool,
    is_latch: bool,
    init: ValueId,
    update: ValueId,
    undef: &mut Option<ValueId>,
    _block: BlockId,
) -> ValueId {
    if is_preheader {
        init
    } else if is_latch {
        update
    } else {
        // This shouldn't happen for simple for/while loops, but if
        // some other block also jumps to the header we pass an
        // `undefined` placeholder and rely on the existing BIV check
        // to reject the reduction next time around. Synthesising a
        // `ConstUndef` op would be cleaner but requires allocating
        // and inserting mid-block; use the init value as a safe
        // fallback — the IR stays well-formed even if semantically
        // "pessimistic" on the unreachable-in-practice path.
        *undef = Some(init);
        init
    }
}

/// Replace every use of `old` with `new` throughout the function,
/// including all block-param-arg positions in terminators.
fn rewrite_uses(func: &mut IrFunction, old: ValueId, new: ValueId) {
    for block in &mut func.blocks {
        for (_, op) in &mut block.ops {
            rewrite_in_op(op, old, new);
        }
        match &mut block.term {
            Terminator::Return(Some(v)) => {
                if *v == old {
                    *v = new;
                }
            }
            Terminator::Return(None) | Terminator::Deopt(_) | Terminator::Unreachable => {}
            Terminator::Jump(_, args) => {
                for v in args {
                    if *v == old {
                        *v = new;
                    }
                }
            }
            Terminator::Branch {
                cond,
                then_args,
                else_args,
                ..
            } => {
                if *cond == old {
                    *cond = new;
                }
                for v in then_args {
                    if *v == old {
                        *v = new;
                    }
                }
                for v in else_args {
                    if *v == old {
                        *v = new;
                    }
                }
            }
        }
    }
}

fn rewrite_in_op(op: &mut IrOp, old: ValueId, new: ValueId) {
    let rw = |v: &mut ValueId| {
        if *v == old {
            *v = new;
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
        | IrOp::StoreGlobal(_, v) => rw(v),

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
            rw(a);
            rw(b);
        }

        IrOp::StoreSlot(obj, _, val) => {
            rw(obj);
            rw(val);
        }

        IrOp::CallRuntime(_, args) => {
            for v in args {
                rw(v);
            }
        }
        IrOp::CallValue(callee, args) => {
            rw(callee);
            for v in args {
                rw(v);
            }
        }
        IrOp::MakeClosureNoCapture(_) => {}
    }
}
