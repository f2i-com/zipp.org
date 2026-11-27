//! Tier O SSA optimizations: natural-loop LICM and dominance-based GVN.
//!
//! Runs between `build::build` and `alloc::allocate` on the SSA `Func`:
//! 1. **Natural-loop LICM**: hoists loop-invariant pure ops and pure deopting
//!    guards/loads (`LoadGlobal`, `GuardConstObj`, `GuardClass`, `GuardFid`,
//!    `ShapeVals`, `LoadSlot`, `ClassConst`, `MathGuard`, `TaggedToF64`,
//!    `TaggedToI32`, `F64ToI32`) from always-executed loop blocks into the loop
//!    preheader (splitting a preheader when needed). Hoisted guards deopt at
//!    the loop header's entry `FrameState` (with header phis substituted by
//!    their preheader inputs, preserving `why` for deopt feedback) so even
//!    0-iteration loops preserve exact JS semantics.
//! 2. **Dominator-tree GVN**: eliminates redundant pure conversions, atomicity
//!    checks, guards, shape checks, global loads, and slot loads across
//!    dominated blocks whenever no intervening CFG path invalidates them.

use rustc_hash::{FxHashMap, FxHashSet};

use super::build::{Block, FrameState, Func, Inst, Op, Term, B, V};

/// Run LICM and dominance-based GVN in place on `f`.
pub(crate) fn optimize(f: &mut Func) {
    if f.blocks.is_empty() || f.insts.len() < 2 {
        return;
    }
    licm(f);
    dom_gvn(f);
}

// ── Dominators & RPO ────────────────────────────────────────────────────────

fn compute_idom(f: &Func) -> Vec<usize> {
    let nb = f.blocks.len();
    let mut idom = vec![usize::MAX; nb];
    idom[0] = 0;

    fn intersect(mut b1: usize, mut b2: usize, idom: &[usize]) -> usize {
        while b1 != b2 {
            while b1 > b2 {
                b1 = idom[b1];
            }
            while b2 > b1 {
                b2 = idom[b2];
            }
        }
        b1
    }

    let mut changed = true;
    while changed {
        changed = false;
        for b in 1..nb {
            let mut new_idom = usize::MAX;
            for &p in &f.blocks[b].preds {
                let pi = p as usize;
                if idom[pi] != usize::MAX {
                    new_idom = if new_idom == usize::MAX {
                        pi
                    } else {
                        intersect(pi, new_idom, &idom)
                    };
                }
            }
            if new_idom != usize::MAX && idom[b] != new_idom {
                idom[b] = new_idom;
                changed = true;
            }
        }
    }
    idom
}

#[inline]
fn dominates(a: usize, mut b: usize, idom: &[usize]) -> bool {
    while b > a {
        let d = idom[b];
        if d == b || d == usize::MAX {
            return false;
        }
        b = d;
    }
    b == a
}

/// Reorder `f.blocks` into clean reverse postorder from block 0, remapping all
/// block indices in terminators, predecessor lists, and `Inst::block`.
fn rebuild_rpo(f: &mut Func) {
    let nb = f.blocks.len();
    let mut order = Vec::with_capacity(nb);
    let mut seen = vec![false; nb];
    let mut stack: Vec<(usize, Vec<B>, usize)> = vec![(0, f.blocks[0].term.succs(), 0)];
    seen[0] = true;
    while let Some((b, succs, k)) = stack.last_mut() {
        if *k < succs.len() {
            let s = succs[*k] as usize;
            *k += 1;
            if !seen[s] {
                seen[s] = true;
                let ss = f.blocks[s].term.succs();
                stack.push((s, ss, 0));
            }
        } else {
            order.push(*b);
            stack.pop();
        }
    }
    order.reverse();
    let mut old_to_new = vec![B::MAX; nb];
    for (new_id, &old_id) in order.iter().enumerate() {
        old_to_new[old_id] = new_id as B;
    }
    let mut old_blocks: Vec<Option<Block>> = f.blocks.drain(..).map(Some).collect();
    let mut new_blocks = Vec::with_capacity(order.len());
    for &old_id in &order {
        let mut blk = old_blocks[old_id].take().expect("unique in RPO");
        for p in &mut blk.preds {
            *p = old_to_new[*p as usize];
        }
        match &mut blk.term {
            Term::Jump(t) => *t = old_to_new[*t as usize],
            Term::Branch { t, f: fb, .. } => {
                *t = old_to_new[*t as usize];
                *fb = old_to_new[*fb as usize];
            }
            Term::Switch { targets, default, .. } => {
                for opt in targets.iter_mut() {
                    if let Some(tb) = opt {
                        *tb = old_to_new[*tb as usize];
                    }
                }
                *default = old_to_new[*default as usize];
            }
            Term::Return(_) | Term::Exit => {}
        }
        new_blocks.push(blk);
    }
    f.blocks = new_blocks;
    for inst in &mut f.insts {
        let m = old_to_new[inst.block as usize];
        if m != B::MAX {
            inst.block = m;
        }
    }
}

fn redirect_term(term: &mut Term, old: B, new: B) {
    match term {
        Term::Jump(t) => {
            if *t == old {
                *t = new;
            }
        }
        Term::Branch { t, f, .. } => {
            if *t == old {
                *t = new;
            }
            if *f == old {
                *f = new;
            }
        }
        Term::Switch { targets, default, .. } => {
            for opt in targets.iter_mut() {
                if *opt == Some(old) {
                    *opt = Some(new);
                }
            }
            if *default == old {
                *default = new;
            }
        }
        Term::Return(_) | Term::Exit => {}
    }
}

// ── Natural-Loop LICM ───────────────────────────────────────────────────────

struct NatLoop {
    header: usize,
    body: Vec<bool>,
    body_count: usize,
    dom_all_latches: Vec<bool>,
}

fn is_pure_licm_op(op: &Op, loop_stores_slot: bool) -> bool {
    match op {
        Op::IsAtomOrPrim(_)
        | Op::I32ToF64(_)
        | Op::Box(_)
        | Op::NotB(_)
        | Op::BitNotI(_)
        | Op::NegF(_)
        | Op::AddF(..)
        | Op::SubF(..)
        | Op::MulF(..)
        | Op::DivF(..)
        | Op::Bit(..)
        | Op::UshrI(..)
        | Op::UshrF(..)
        | Op::ImulI(..)
        | Op::CmpF(..)
        | Op::CmpI(..)
        | Op::CmpB(..)
        | Op::StrictEqBits(..)
        | Op::StrictEqAtom { .. }
        | Op::F64Truthy(_)
        | Op::I32Truthy(_)
        | Op::TypeOfIs { .. }
        | Op::MathF1(..) => true,
        Op::LoadSlot { .. } => !loop_stores_slot,
        _ => false,
    }
}

fn is_guard_licm_op(op: &Op, loop_reshapes: bool, loop_stores_global: bool) -> bool {
    match op {
        Op::LoadGlobal(_) | Op::LoadGlobalOrUndef(_) => !loop_stores_global,
        Op::GuardConstObj { .. }
        | Op::GuardClass { .. }
        | Op::ShapeVals { .. }
        | Op::ClassConst { .. } => !loop_reshapes,
        Op::MathGuard { .. } => !loop_reshapes && !loop_stores_global,
        Op::GuardFid { .. }
        | Op::TaggedToF64(_)
        | Op::TaggedToI32(_)
        | Op::F64ToI32(_) => true,
        _ => false,
    }
}

fn licm(f: &mut Func) {
    let nb = f.blocks.len();
    if nb <= 1 {
        return;
    }
    let idom = compute_idom(f);
    // Group all back-edges by header.
    let mut latches_by_header: FxHashMap<usize, Vec<usize>> = FxHashMap::default();
    for bi in 0..nb {
        for s in f.blocks[bi].term.succs() {
            let h = s as usize;
            if h != 0 && dominates(h, bi, &idom) {
                latches_by_header.entry(h).or_default().push(bi);
            }
        }
    }
    if latches_by_header.is_empty() {
        return;
    }
    let mut loops: Vec<NatLoop> = Vec::with_capacity(latches_by_header.len());
    let mut work = Vec::new();
    for (header, latches) in latches_by_header {
        let mut body = vec![false; nb];
        body[header] = true;
        let mut body_count = 1usize;
        for &l in &latches {
            if !body[l] {
                body[l] = true;
                body_count += 1;
                work.push(l);
            }
        }
        while let Some(u) = work.pop() {
            for &p in &f.blocks[u].preds {
                let pi = p as usize;
                if !body[pi] {
                    body[pi] = true;
                    body_count += 1;
                    work.push(pi);
                }
            }
        }
        let mut dom_all_latches = vec![false; nb];
        for b in 0..nb {
            if body[b] && latches.iter().all(|&l| dominates(b, l, &idom)) {
                dom_all_latches[b] = true;
            }
        }
        loops.push(NatLoop {
            header,
            body,
            body_count,
            dom_all_latches,
        });
    }
    // Innermost loops first so hoisted invariants cascade out of outer loops.
    loops.sort_unstable_by_key(|l| (l.body_count, l.header));

    let mut any_preheader_created = false;
    let mut li = 0;
    while li < loops.len() {
        let h = loops[li].header;
        let outside_preds: Vec<B> = f.blocks[h]
            .preds
            .iter()
            .copied()
            .filter(|&p| !loops[li].body[p as usize])
            .collect();
        if outside_preds.is_empty() {
            li += 1;
            continue;
        }
        // Loop-wide side-effect summary.
        let mut loop_reshapes = false;
        let mut loop_stores_global = false;
        let mut loop_stores_slot = false;
        for b in 0..f.blocks.len() {
            if !loops[li].body[b] {
                continue;
            }
            for &v in &f.blocks[b].insts {
                let op = &f.insts[v as usize].op;
                if op.may_reshape() {
                    loop_reshapes = true;
                    loop_stores_global = true;
                    loop_stores_slot = true;
                    break;
                }
                match op {
                    Op::StoreGlobal(..) => loop_stores_global = true,
                    Op::StoreSlot { .. } => loop_stores_slot = true,
                    _ => {}
                }
            }
            if loop_reshapes {
                break;
            }
        }
        // Check whether header `h` has a valid entry state whose values all
        // resolve cleanly at the preheader (defined outside `body` or phis of `h`).
        let entry_ok = f.blocks[h].entry_state.as_ref().is_some_and(|(_, regs)| {
            regs.iter().all(|&(_, u)| {
                let ub = f.insts[u as usize].block as usize;
                !loops[li].body[ub] || (ub == h && matches!(f.insts[u as usize].op, Op::Phi(_)))
            })
        });

        // Fixpoint selection of invariant instructions from always-executed blocks.
        let mut hoisted: Vec<(usize, V)> = Vec::new();
        let mut hoisted_set: FxHashSet<V> = FxHashSet::default();
        let mut changed = true;
        while changed {
            changed = false;
            for b in 0..f.blocks.len() {
                if !loops[li].dom_all_latches[b] {
                    continue;
                }
                for &v in &f.blocks[b].insts {
                    if hoisted_set.contains(&v) {
                        continue;
                    }
                    let inst = &f.insts[v as usize];
                    let eligible = if inst.fs.is_none() {
                        is_pure_licm_op(&inst.op, loop_stores_slot)
                    } else {
                        entry_ok && is_guard_licm_op(&inst.op, loop_reshapes, loop_stores_global)
                    };
                    if !eligible {
                        continue;
                    }
                    let mut inv = true;
                    inst.op.for_each_operand(|u| {
                        let ub = f.insts[u as usize].block as usize;
                        if loops[li].body[ub] && !hoisted_set.contains(&u) {
                            inv = false;
                        }
                    });
                    if inv {
                        hoisted_set.insert(v);
                        hoisted.push((b, v));
                        changed = true;
                    }
                }
            }
        }
        if hoisted.is_empty() {
            li += 1;
            continue;
        }

        // Reuse dedicated preheader or split a new one on all outside edges into `h`.
        let mut h_phi_to_pre: FxHashMap<V, V> = FxHashMap::default();
        let pre: usize = if outside_preds.len() == 1
            && matches!(f.blocks[outside_preds[0] as usize].term, Term::Jump(t) if t == h as B)
        {
            let p = outside_preds[0] as usize;
            let pidx = f.blocks[h]
                .preds
                .iter()
                .position(|&q| q as usize == p)
                .expect("outside pred in h.preds");
            for &phi in &f.blocks[h].insts {
                if let Op::Phi(ins) = &f.insts[phi as usize].op {
                    h_phi_to_pre.insert(phi, ins[pidx]);
                }
            }
            p
        } else {
            let pre_id = f.blocks.len();
            for &op in &outside_preds {
                redirect_term(&mut f.blocks[op as usize].term, h as B, pre_id as B);
            }
            let mut out_indices = Vec::new();
            let mut in_indices = Vec::new();
            let mut inside_preds = Vec::new();
            for (idx, &p) in f.blocks[h].preds.iter().enumerate() {
                if loops[li].body[p as usize] {
                    in_indices.push(idx);
                    inside_preds.push(p);
                } else {
                    out_indices.push(idx);
                }
            }
            let mut pre_insts: Vec<V> = Vec::new();
            let h_insts_snapshot = f.blocks[h].insts.clone();
            for phi in h_insts_snapshot {
                let Op::Phi(ins) = f.insts[phi as usize].op.clone() else {
                    break;
                };
                let out_ins: Vec<V> = out_indices.iter().map(|&idx| ins[idx]).collect();
                let in_ins: Vec<V> = in_indices.iter().map(|&idx| ins[idx]).collect();
                let pre_val = if out_ins.len() == 1 || out_ins.iter().all(|&x| x == out_ins[0]) {
                    out_ins[0]
                } else {
                    let pv = f.insts.len() as V;
                    let rep = f.insts[phi as usize].rep;
                    f.insts.push(Inst {
                        op: Op::Phi(out_ins),
                        rep,
                        fs: None,
                        block: pre_id as B,
                    });
                    pre_insts.push(pv);
                    pv
                };
                h_phi_to_pre.insert(phi, pre_val);
                let mut new_ins = Vec::with_capacity(1 + in_ins.len());
                new_ins.push(pre_val);
                new_ins.extend(in_ins);
                f.insts[phi as usize].op = Op::Phi(new_ins);
            }
            let mut new_h_preds = Vec::with_capacity(1 + inside_preds.len());
            new_h_preds.push(pre_id as B);
            new_h_preds.extend(inside_preds);
            f.blocks[h].preds = new_h_preds;
            f.blocks.push(Block {
                insts: pre_insts,
                term: Term::Jump(h as B),
                preds: outside_preds,
                entry_state: None,
            });
            any_preheader_created = true;
            // Extend membership bitvectors for all outer loops.
            for outer in &mut loops[li + 1..] {
                let inside = outer.body[h];
                outer.body.push(inside);
                outer.dom_all_latches.push(inside && outer.dom_all_latches[h]);
            }
            pre_id
        };

        // Precompute `pre`-substituted `FrameState` entries lazily per `why`.
        let pre_entry = f.blocks[h].entry_state.as_ref().map(|(ip, regs)| {
            let mapped: Vec<(u16, V)> = regs
                .iter()
                .map(|&(r, u)| (r, h_phi_to_pre.get(&u).copied().unwrap_or(u)))
                .collect();
            (*ip, mapped)
        });
        let mut fs_by_why: FxHashMap<u32, u32> = FxHashMap::default();

        // Remove hoisted instructions from their original blocks and append to `pre`.
        let mut by_block: FxHashMap<usize, FxHashSet<V>> = FxHashMap::default();
        for &(b, v) in &hoisted {
            by_block.entry(b).or_default().insert(v);
        }
        for (b, set) in by_block {
            f.blocks[b].insts.retain(|v| !set.contains(v));
        }
        for (_, v) in hoisted {
            if let Some(orig_s) = f.insts[v as usize].fs {
                let why = f.states[orig_s as usize].why;
                let new_s = *fs_by_why.entry(why).or_insert_with(|| {
                    let (ip, ref regs) = pre_entry.as_ref().expect("guard hoist requires entry_state");
                    let id = f.states.len() as u32;
                    f.states.push(FrameState {
                        ip: *ip,
                        why,
                        regs: regs.clone(),
                    });
                    id
                });
                f.insts[v as usize].fs = Some(new_s);
            }
            f.insts[v as usize].block = pre as B;
            f.blocks[pre].insts.push(v);
        }
        li += 1;
    }

    if any_preheader_created {
        rebuild_rpo(f);
    }
}

// ── Dominance-Based GVN ─────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct GvnEnv {
    // Pure immutable expressions (survive all side effects).
    atoms: FxHashMap<V, V>,
    i2f: FxHashMap<V, V>,
    t2f: FxHashMap<V, V>,
    t2i: FxHashMap<V, V>,
    f2i: FxHashMap<V, V>,
    boxed: FxHashMap<V, V>,
    fids: FxHashSet<(V, u32)>,
    // Reshape-sensitive expressions.
    shapes: FxHashMap<(V, u32), V>,
    const_objs: FxHashSet<(V, u32, u32)>,
    classes: FxHashSet<(V, u32, u32, u32, u32)>,
    class_consts: FxHashMap<(u64, u32), V>,
    math_guards: FxHashSet<(Option<V>, u32, u8)>,
    // Global-sensitive expressions.
    globals: FxHashMap<u32, V>,
    globals_undef: FxHashMap<u32, V>,
    // Slot-sensitive expressions.
    slots: FxHashMap<(V, u32), V>,
}

impl GvnEnv {
    fn clear_reshape(&mut self) {
        self.shapes.clear();
        self.const_objs.clear();
        self.classes.clear();
        self.class_consts.clear();
        self.math_guards.clear();
        self.globals.clear();
        self.globals_undef.clear();
        self.slots.clear();
    }

    fn clear_globals(&mut self) {
        self.globals.clear();
        self.globals_undef.clear();
    }

    fn clear_slots(&mut self) {
        self.slots.clear();
    }
}

fn resolve(subst: &[V], mut v: V) -> V {
    while subst[v as usize] != v {
        v = subst[v as usize];
    }
    v
}

/// Side effects on any CFG path strictly between dominator `d`'s exit and `c`'s
/// entry (including `c` itself if `c` is re-entered via a back-edge).
fn between_effects(
    d: usize,
    c: usize,
    f: &Func,
    blk_reshapes: &[bool],
    blk_stores_global: &[bool],
    blk_stores_slot: &[bool],
) -> (bool, bool, bool) {
    let preds = &f.blocks[c].preds;
    if preds.len() == 1 && preds[0] as usize == d {
        return (false, false, false);
    }
    let nb = f.blocks.len();
    let mut seen = vec![false; nb];
    seen[d] = true;
    let mut work = Vec::new();
    for &p in preds {
        let pi = p as usize;
        if !seen[pi] {
            seen[pi] = true;
            work.push(pi);
        }
    }
    let mut any_reshape = false;
    let mut any_global = false;
    let mut any_slot = false;
    while let Some(u) = work.pop() {
        any_reshape |= blk_reshapes[u];
        any_global |= blk_stores_global[u];
        any_slot |= blk_stores_slot[u];
        if any_reshape && any_global && any_slot {
            break;
        }
        for &p in &f.blocks[u].preds {
            let pi = p as usize;
            if !seen[pi] {
                seen[pi] = true;
                work.push(pi);
            }
        }
    }
    (any_reshape, any_global, any_slot)
}

fn dom_gvn(f: &mut Func) {
    let nb = f.blocks.len();
    let idom = compute_idom(f);
    let mut children = vec![Vec::new(); nb];
    for b in 1..nb {
        let d = idom[b];
        if d < nb {
            children[d].push(b);
        }
    }
    let mut blk_reshapes = vec![false; nb];
    let mut blk_stores_global = vec![false; nb];
    let mut blk_stores_slot = vec![false; nb];
    for b in 0..nb {
        for &v in &f.blocks[b].insts {
            let op = &f.insts[v as usize].op;
            if op.may_reshape() {
                blk_reshapes[b] = true;
                blk_stores_global[b] = true;
                blk_stores_slot[b] = true;
                break;
            }
            match op {
                Op::StoreGlobal(..) => blk_stores_global[b] = true,
                Op::StoreSlot { .. } => blk_stores_slot[b] = true,
                _ => {}
            }
        }
    }

    let nv = f.insts.len();
    let mut subst: Vec<V> = (0..nv as V).collect();
    let mut dead = vec![false; nv];
    let mut any_changed = false;

    // Iterative DFS over the dominator tree.
    let mut stack: Vec<(usize, GvnEnv)> = vec![(0, GvnEnv::default())];
    while let Some((b, mut env)) = stack.pop() {
        for &v in &f.blocks[b].insts {
            let op = &f.insts[v as usize].op;
            let hit: Option<Option<V>> = match *op {
                Op::IsAtomOrPrim(a) => {
                    let a = resolve(&subst, a);
                    if let Some(&prev) = env.atoms.get(&a) {
                        Some(Some(prev))
                    } else {
                        env.atoms.insert(a, v);
                        None
                    }
                }
                Op::I32ToF64(a) => {
                    let a = resolve(&subst, a);
                    if let Some(&prev) = env.i2f.get(&a) {
                        Some(Some(prev))
                    } else {
                        env.i2f.insert(a, v);
                        None
                    }
                }
                Op::TaggedToF64(a) => {
                    let a = resolve(&subst, a);
                    if let Some(&prev) = env.t2f.get(&a) {
                        Some(Some(prev))
                    } else {
                        env.t2f.insert(a, v);
                        None
                    }
                }
                Op::TaggedToI32(a) => {
                    let a = resolve(&subst, a);
                    if let Some(&prev) = env.t2i.get(&a) {
                        Some(Some(prev))
                    } else {
                        env.t2i.insert(a, v);
                        None
                    }
                }
                Op::F64ToI32(a) => {
                    let a = resolve(&subst, a);
                    if let Some(&prev) = env.f2i.get(&a) {
                        Some(Some(prev))
                    } else {
                        env.f2i.insert(a, v);
                        None
                    }
                }
                Op::Box(a) => {
                    let a = resolve(&subst, a);
                    if let Some(&prev) = env.boxed.get(&a) {
                        Some(Some(prev))
                    } else {
                        env.boxed.insert(a, v);
                        None
                    }
                }
                Op::GuardFid { v: x, fid } => {
                    let x = resolve(&subst, x);
                    if env.fids.contains(&(x, fid)) {
                        Some(None)
                    } else {
                        env.fids.insert((x, fid));
                        None
                    }
                }
                Op::ShapeVals { obj, shape } => {
                    let obj = resolve(&subst, obj);
                    if let Some(&prev) = env.shapes.get(&(obj, shape)) {
                        Some(Some(prev))
                    } else {
                        env.shapes.insert((obj, shape), v);
                        None
                    }
                }
                Op::GuardConstObj { obj, oidx, ver } => {
                    let obj = resolve(&subst, obj);
                    if env.const_objs.contains(&(obj, oidx, ver)) {
                        Some(None)
                    } else {
                        env.const_objs.insert((obj, oidx, ver));
                        None
                    }
                }
                Op::GuardClass {
                    obj,
                    recv_shape,
                    class,
                    ver,
                    epoch,
                } => {
                    let obj = resolve(&subst, obj);
                    let key = (obj, recv_shape, class, ver, epoch);
                    if env.classes.contains(&key) {
                        Some(None)
                    } else {
                        env.classes.insert(key);
                        None
                    }
                }
                Op::ClassConst { bits, epoch } => {
                    if let Some(&prev) = env.class_consts.get(&(bits, epoch)) {
                        Some(Some(prev))
                    } else {
                        env.class_consts.insert((bits, epoch), v);
                        None
                    }
                }
                Op::MathGuard { recv, gidx, op: mop } => {
                    let key = (recv.map(|r| resolve(&subst, r)), gidx, mop as u8);
                    if env.math_guards.contains(&key) {
                        Some(None)
                    } else {
                        env.math_guards.insert(key);
                        None
                    }
                }
                Op::LoadGlobal(idx) => {
                    if let Some(&prev) = env.globals.get(&idx) {
                        Some(Some(prev))
                    } else {
                        env.globals.insert(idx, v);
                        None
                    }
                }
                Op::LoadGlobalOrUndef(idx) => {
                    if let Some(&prev) = env.globals_undef.get(&idx) {
                        Some(Some(prev))
                    } else {
                        env.globals_undef.insert(idx, v);
                        None
                    }
                }
                Op::LoadSlot { vals, slot } => {
                    let vals = resolve(&subst, vals);
                    if let Some(&prev) = env.slots.get(&(vals, slot)) {
                        Some(Some(prev))
                    } else {
                        env.slots.insert((vals, slot), v);
                        None
                    }
                }
                _ => {
                    if op.may_reshape() {
                        env.clear_reshape();
                    } else if matches!(op, Op::StoreGlobal(..)) {
                        env.clear_globals();
                    } else if matches!(op, Op::StoreSlot { .. }) {
                        env.clear_slots();
                    }
                    None
                }
            };
            if let Some(replacement) = hit {
                dead[v as usize] = true;
                if let Some(prev) = replacement {
                    subst[v as usize] = prev;
                }
                any_changed = true;
            }
        }
        // Push children in reverse so lower RPO indices visit first.
        let ch = &children[b];
        for &c in ch.iter().rev() {
            let mut child_env = env.clone();
            let (r, g, s) = between_effects(b, c, f, &blk_reshapes, &blk_stores_global, &blk_stores_slot);
            if r {
                child_env.clear_reshape();
            } else {
                if g {
                    child_env.clear_globals();
                }
                if s {
                    child_env.clear_slots();
                }
            }
            stack.push((c, child_env));
        }
    }

    if !any_changed {
        return;
    }
    for blk in &mut f.blocks {
        blk.insts.retain(|&v| !dead[v as usize]);
        match &mut blk.term {
            Term::Branch { cond, .. } => *cond = resolve(&subst, *cond),
            Term::Switch { val, .. } => *val = resolve(&subst, *val),
            Term::Return(v) => *v = resolve(&subst, *v),
            Term::Jump(_) | Term::Exit => {}
        }
        if let Some((_, regs)) = &mut blk.entry_state {
            for (_, u) in regs.iter_mut() {
                *u = resolve(&subst, *u);
            }
        }
    }
    for (vi, inst) in f.insts.iter_mut().enumerate() {
        if !dead[vi] {
            inst.op.map_operands(|u| resolve(&subst, u));
        }
    }
    for st in &mut f.states {
        for (_, u) in &mut st.regs {
            *u = resolve(&subst, *u);
        }
    }
}
