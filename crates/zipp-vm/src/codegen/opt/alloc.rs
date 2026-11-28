//! Tier O's register allocation: linear scan over one live interval per
//! SSA value.
//!
//! Positions number the blocks' values in block order (reverse postorder),
//! two apart, with a block's phis at its start position and its terminator
//! (where its phi moves run) at its end. Liveness is SSA liveness over the
//! blocks (a phi's input is read at the end of its predecessor); a value's
//! interval is the hull of everywhere it is live, which covers a loop whole
//! once a value is live across its back edge. A frame state reads every
//! value it names at its guard. Values whose interval finds no free
//! register of their class live in a stack slot for their whole interval:
//! the interval that ends last is the one spilled.

use super::build::{Func, Op, Rep, V};

/// Where a value lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Loc {
    Gpr(u8),
    Xmm(u8),
    /// A stack slot, by index.
    Slot(u32),
    /// A read-only parameter (`Op::Param(r)`) read directly from `[rbx + 8*r]`
    /// without occupying a register or stack slot.
    Param(u16),
    /// A rematerialized compile-time constant (`ConstI32`/`ConstBool`/`ConstTagged`):
    /// consumes zero registers and zero stack slots.
    Imm,
}

/// The registers the allocator hands out. The emitter keeps rax, rcx, rdx,
/// r10 and r11 (and xmm0-xmm3) for its own sequences and spilled operands;
/// rbx, rsi and rdi hold the window, the bail word and the VM.
pub(crate) const GPRS: [u8; 7] = [8, 9, 12, 13, 14, 15, 5];
pub(crate) const XMMS: [u8; 12] = [4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

pub(crate) struct Alloc {
    pub(crate) loc: Vec<Loc>,
    /// Stack slots used.
    pub(crate) slots: u32,
}

fn is_xmm(r: Rep) -> bool {
    r == Rep::F64
}

pub(crate) fn allocate(f: &Func, d3_mode: bool) -> Alloc {
    let nv = f.insts.len();
    // ── positions ──
    let mut pos = vec![0u32; nv];
    let mut block_pos = Vec::with_capacity(f.blocks.len());
    let mut p = 0u32;
    for b in &f.blocks {
        let start = p;
        for &v in &b.insts {
            pos[v as usize] = if matches!(f.insts[v as usize].op, Op::Phi(_)) {
                start
            } else {
                p += 2;
                p
            };
        }
        p += 2;
        block_pos.push((start, p));
        p += 2;
    }
    // ── SSA liveness ──
    let nb = f.blocks.len();
    let words = nv.div_ceil(64).max(1);
    let mut live_in = vec![vec![0u64; words]; nb];
    let mut live_out = vec![vec![0u64; words]; nb];
    let mut live = vec![0u64; words];
    let set = |s: &mut [u64], v: V| s[v as usize / 64] |= 1 << (v % 64);
    let clr = |s: &mut [u64], v: V| s[v as usize / 64] &= !(1 << (v % 64));
    let mut changed = true;
    while changed {
        changed = false;
        for bi in (0..nb).rev() {
            let b = &f.blocks[bi];
            live.fill(0);
            for s in b.term.succs() {
                let sb = &f.blocks[s as usize];
                for (w, x) in live.iter_mut().zip(&live_in[s as usize]) {
                    *w |= x;
                }
                // This block's input to each of the successor's phis.
                let k = sb.preds.iter().position(|&q| q as usize == bi).expect("a successor lists its predecessor");
                for &phi in &sb.insts {
                    if let Op::Phi(ins) = &f.insts[phi as usize].op {
                        set(&mut live, ins[k]);
                    }
                }
            }
            if live != live_out[bi] {
                live_out[bi].copy_from_slice(&live);
                changed = true;
            }
            match b.term {
                super::build::Term::Branch { cond, .. } => set(&mut live, cond),
                super::build::Term::Switch { val, .. } => set(&mut live, val),
                super::build::Term::Return(v) => set(&mut live, v),
                super::build::Term::Jump(_) | super::build::Term::Exit => {}
            }
            for &v in b.insts.iter().rev() {
                clr(&mut live, v);
                let i = &f.insts[v as usize];
                if !matches!(i.op, Op::Phi(_)) {
                    i.op.for_each_operand(|u| set(&mut live, u));
                    if !d3_mode {
                        if let Some(s) = i.fs {
                            for &(_, u) in &f.states[s as usize].regs {
                                set(&mut live, u);
                            }
                        }
                    }
                }
            }
            if live != live_in[bi] {
                live_in[bi].copy_from_slice(&live);
                changed = true;
            }
        }
    }
    // ── intervals ── the hull of the value's def, its reads and the blocks
    // it is live into or out of.
    let mut start = pos.clone();
    let mut end = pos.clone();
    #[inline(always)]
    fn extend(v: usize, at: u32, start: &mut [u32], end: &mut [u32]) {
        start[v] = start[v].min(at);
        end[v] = end[v].max(at);
    }
    #[inline]
    fn extend_bitset(bits: &[u64], at: u32, start: &mut [u32], end: &mut [u32]) {
        for (wi, &w) in bits.iter().enumerate() {
            let mut m = w;
            let base = wi * 64;
            while m != 0 {
                let v = base + m.trailing_zeros() as usize;
                extend(v, at, start, end);
                m &= m - 1;
            }
        }
    }
    for (bi, b) in f.blocks.iter().enumerate() {
        let (bs, be) = block_pos[bi];
        extend_bitset(&live_in[bi], bs, &mut start, &mut end);
        extend_bitset(&live_out[bi], be, &mut start, &mut end);
        for &v in &b.insts {
            let i = &f.insts[v as usize];
            if !matches!(i.op, Op::Phi(_)) {
                let pv = pos[v as usize];
                i.op.for_each_operand(|u| extend(u as usize, pv, &mut start, &mut end));
                if !d3_mode {
                    if let Some(s) = i.fs {
                        // A state written after a native call is read after it.
                        let at = if i.op.state_after_call() { pv + 1 } else { pv };
                        for &(_, u) in &f.states[s as usize].regs {
                            extend(u as usize, at, &mut start, &mut end);
                        }
                    }
                }
            }
        }
        match b.term {
            super::build::Term::Branch { cond, .. } => extend(cond as usize, be, &mut start, &mut end),
            super::build::Term::Switch { val, .. } => extend(val as usize, be, &mut start, &mut end),
            super::build::Term::Return(v) => extend(v as usize, be, &mut start, &mut end),
            super::build::Term::Jump(_) | super::build::Term::Exit => {}
        }
    }
    // ── linear scan ── a value live across a call-out (a call position
    // strictly inside its interval) takes a callee-saved register, which
    // the native callee preserves, or a slot.
    let mut calls: Vec<u32> = (0..nv)
        .filter(|&v| f.insts[v].op.calls())
        .map(|v| pos[v])
        .collect();
    calls.sort_unstable();
    let crosses = |v: usize| {
        let s = start[v];
        let e = end[v];
        if s + 1 >= e {
            return false;
        }
        let k = calls.partition_point(|&c| c <= s);
        k < calls.len() && calls[k] < e
    };
    // ── loop depth & use weights ── estimate dynamic hotness per SSA value
    // from RPO back-edges (`s <= bi`) so inner-loop values stay in registers
    // while cold/single-use outer values spill first. Frame-state uses are
    // cold (only read on deopt/bail) and do not inflate hotness weights.
    let mut loop_depth = vec![0u8; nb];
    let mut in_loop = vec![false; nb];
    let mut work: Vec<usize> = Vec::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        for s in b.term.succs() {
            let h = s as usize;
            if h <= bi {
                in_loop.fill(false);
                in_loop[h] = true;
                if bi != h {
                    in_loop[bi] = true;
                    work.push(bi);
                    while let Some(u) = work.pop() {
                        for &p in &f.blocks[u].preds {
                            let pi = p as usize;
                            if pi >= h && !in_loop[pi] {
                                in_loop[pi] = true;
                                work.push(pi);
                            }
                        }
                    }
                }
                for (k, &m) in in_loop.iter().enumerate() {
                    if m {
                        loop_depth[k] = loop_depth[k].saturating_add(1);
                    }
                }
            }
        }
    }
    let blk_weight = |bi: usize| -> u32 { 10u32.pow(u32::from(loop_depth[bi].min(4))) };
    let mut weight = vec![0u32; nv];
    for (bi, b) in f.blocks.iter().enumerate() {
        let w = blk_weight(bi);
        for &v in &b.insts {
            let i = &f.insts[v as usize];
            weight[v as usize] = weight[v as usize].saturating_add(w);
            if let Op::Phi(ins) = &i.op {
                for (pi, &u) in ins.iter().enumerate() {
                    let pw = blk_weight(b.preds[pi] as usize);
                    weight[u as usize] = weight[u as usize].saturating_add(pw);
                }
            } else {
                i.op.for_each_operand(|u| {
                    weight[u as usize] = weight[u as usize].saturating_add(w);
                });
            }
        }
        match b.term {
            super::build::Term::Branch { cond, .. } => {
                weight[cond as usize] = weight[cond as usize].saturating_add(w);
            }
            super::build::Term::Switch { val, .. } => {
                weight[val as usize] = weight[val as usize].saturating_add(w);
            }
            super::build::Term::Return(v) => {
                weight[v as usize] = weight[v as usize].saturating_add(w);
            }
            super::build::Term::Jump(_) | super::build::Term::Exit => {}
        }
    }
    let mut d3_param_used = vec![false; nv];
    if d3_mode {
        for b in &f.blocks {
            for &v in &b.insts {
                let i = &f.insts[v as usize];
                if let Op::Phi(ins) = &i.op {
                    for &u in ins {
                        d3_param_used[u as usize] = true;
                    }
                } else {
                    i.op.for_each_operand(|u| d3_param_used[u as usize] = true);
                }
            }
            match b.term {
                super::build::Term::Branch { cond, .. } => d3_param_used[cond as usize] = true,
                super::build::Term::Switch { val, .. } => d3_param_used[val as usize] = true,
                super::build::Term::Return(v) => d3_param_used[v as usize] = true,
                super::build::Term::Jump(_) | super::build::Term::Exit => {}
            }
        }
    }
    let mut order: Vec<usize> = (0..nv)
        .filter(|&v| {
            f.insts[v].rep != Rep::Void
                && !f.insts[v].op.is_imm()
                && !(d3_mode && matches!(f.insts[v].op, Op::Param(_)) && !d3_param_used[v])
        })
        .collect();
    order.sort_by_key(|&v| (start[v], v));
    let mut loc = vec![Loc::Slot(u32::MAX); nv];
    for v in 0..nv {
        if f.insts[v].op.is_imm() {
            loc[v] = Loc::Imm;
        }
    }
    let mut slot_end: Vec<u32> = Vec::new();
    let readonly_param_reg = |v: usize| -> Option<u16> {
        if d3_mode {
            return None;
        }
        match f.insts[v].op {
            Op::Param(r) if f.readonly_param.get(r as usize).copied().unwrap_or(false) => Some(r),
            _ => None,
        }
    };
    let saved = |class: usize, r: u8| if class == 1 { r >= 6 } else { r != 8 && r != 9 };
    // Per class: the active intervals (value) and the free registers,
    // caller-saved and callee-saved.
    let mut active: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
    let mut free_vol: [Vec<u8>; 2] = [Vec::new(), Vec::new()];
    let mut free_sav: [Vec<u8>; 2] = [Vec::new(), Vec::new()];
    for (class, regs) in [&GPRS[..], &XMMS[..]].into_iter().enumerate() {
        for &r in regs.iter().rev() {
            if saved(class, r) {
                free_sav[class].push(r);
            } else {
                free_vol[class].push(r);
            }
        }
    }
    for &v in &order {
        // Low-weight read-only parameters (used at most once outside loops)
        // stay in `[rbx + 8*r]` without occupying a register or callee-saved push.
        if let Some(pr) = readonly_param_reg(v) {
            if weight[v] <= 2 {
                loc[v] = Loc::Param(pr);
                continue;
            }
        }
        let class = usize::from(is_xmm(f.insts[v].rep));
        // Expire the intervals that ended before this one starts.
        let mut k = 0;
        while k < active[class].len() {
            let a = active[class][k];
            if end[a] < start[v] {
                active[class].swap_remove(k);
                if let Loc::Gpr(r) | Loc::Xmm(r) = loc[a] {
                    if saved(class, r) {
                        free_sav[class].push(r);
                    } else {
                        free_vol[class].push(r);
                    }
                }
            } else {
                k += 1;
            }
        }
        let cross = crosses(v);
        let pick = if cross {
            free_sav[class].pop()
        } else {
            free_vol[class].pop().or_else(|| free_sav[class].pop())
        };
        if let Some(r) = pick {
            loc[v] = if class == 1 { Loc::Xmm(r) } else { Loc::Gpr(r) };
            active[class].push(v);
            continue;
        }
        // Spill the lowest-weight active interval (breaking ties by furthest
        // end) among those whose register `v` may take.
        let victim = active[class]
            .iter()
            .enumerate()
            .filter(|&(_, &a)| match loc[a] {
                Loc::Gpr(r) | Loc::Xmm(r) => !cross || saved(class, r),
                Loc::Slot(_) | Loc::Param(_) | Loc::Imm => false,
            })
            .max_by_key(|&(_, &a)| (std::cmp::Reverse(weight[a]), end[a]))
            .map(|(ki, &a)| (ki, a));
        // A spilled interval (`target`, which may be an earlier-started `victim`
        // moved retroactively out of a register) can only reuse a slot whose
        // previous occupant ended strictly before `start[target]`.
        let mut assign_spill = |target: usize, loc: &mut [Loc]| {
            if let Some(pr) = readonly_param_reg(target) {
                loc[target] = Loc::Param(pr);
            } else {
                let st = start[target];
                let en = end[target];
                let s = if let Some(idx) = slot_end.iter().position(|&e| e < st) {
                    slot_end[idx] = en;
                    idx as u32
                } else {
                    let idx = slot_end.len() as u32;
                    slot_end.push(en);
                    idx
                };
                loc[target] = Loc::Slot(s);
            }
        };
        match victim {
            Some((ki, victim))
                if weight[victim] < weight[v]
                    || (weight[victim] == weight[v] && end[victim] > end[v]) =>
            {
                loc[v] = loc[victim];
                assign_spill(victim, &mut loc);
                active[class][ki] = v;
            }
            _ => {
                assign_spill(v, &mut loc);
            }
        }
    }
    Alloc {
        loc,
        slots: slot_end.len() as u32,
    }
}
