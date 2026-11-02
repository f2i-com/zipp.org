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

pub(crate) fn allocate(f: &Func) -> Alloc {
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
    let set = |s: &mut [u64], v: V| s[v as usize / 64] |= 1 << (v % 64);
    let clr = |s: &mut [u64], v: V| s[v as usize / 64] &= !(1 << (v % 64));
    let get = |s: &[u64], v: usize| s[v / 64] >> (v % 64) & 1 != 0;
    // The values each block reads (operands, frame states, terminator) in
    // order, for the backward walk.
    let reads = |v: V| -> Vec<V> {
        let i = &f.insts[v as usize];
        let mut r = i.op.operands();
        if let Some(s) = i.fs {
            r.extend(f.states[s as usize].regs.iter().map(|&(_, x)| x));
        }
        r
    };
    let mut changed = true;
    while changed {
        changed = false;
        for bi in (0..nb).rev() {
            let b = &f.blocks[bi];
            let mut live = vec![0u64; words];
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
                live_out[bi] = live.clone();
                changed = true;
            }
            match b.term {
                super::build::Term::Branch { cond, .. } => set(&mut live, cond),
                super::build::Term::Return(v) => set(&mut live, v),
                super::build::Term::Jump(_) => {}
            }
            for &v in b.insts.iter().rev() {
                clr(&mut live, v);
                if !matches!(f.insts[v as usize].op, Op::Phi(_)) {
                    for u in reads(v) {
                        set(&mut live, u);
                    }
                }
            }
            if live != live_in[bi] {
                live_in[bi] = live;
                changed = true;
            }
        }
    }
    // ── intervals ── the hull of the value's def, its reads and the blocks
    // it is live into or out of.
    let mut start = pos.clone();
    let mut end = pos.clone();
    fn extend(v: usize, at: u32, start: &mut [u32], end: &mut [u32]) {
        start[v] = start[v].min(at);
        end[v] = end[v].max(at);
    }
    for (bi, b) in f.blocks.iter().enumerate() {
        let (bs, be) = block_pos[bi];
        for v in 0..nv {
            if get(&live_in[bi], v) {
                extend(v, bs, &mut start, &mut end);
            }
            if get(&live_out[bi], v) {
                extend(v, be, &mut start, &mut end);
            }
        }
        for &v in &b.insts {
            if !matches!(f.insts[v as usize].op, Op::Phi(_)) {
                for u in reads(v) {
                    extend(u as usize, pos[v as usize], &mut start, &mut end);
                }
            }
        }
        match b.term {
            super::build::Term::Branch { cond, .. } => extend(cond as usize, be, &mut start, &mut end),
            super::build::Term::Return(v) => extend(v as usize, be, &mut start, &mut end),
            super::build::Term::Jump(_) => {}
        }
    }
    // ── linear scan ──
    let mut order: Vec<usize> = (0..nv).collect();
    order.sort_by_key(|&v| (start[v], v));
    let mut loc = vec![Loc::Slot(u32::MAX); nv];
    let mut slots = 0u32;
    // Per class: the active intervals (value) and the free registers.
    let mut active: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
    let mut free: [Vec<u8>; 2] = [GPRS.iter().rev().copied().collect(), XMMS.iter().rev().copied().collect()];
    for &v in &order {
        let class = usize::from(is_xmm(f.insts[v].rep));
        // Expire the intervals that ended before this one starts.
        let mut k = 0;
        while k < active[class].len() {
            let a = active[class][k];
            if end[a] < start[v] {
                active[class].swap_remove(k);
                match loc[a] {
                    Loc::Gpr(r) | Loc::Xmm(r) => free[class].push(r),
                    Loc::Slot(_) => {}
                }
            } else {
                k += 1;
            }
        }
        if let Some(r) = free[class].pop() {
            loc[v] = if class == 1 { Loc::Xmm(r) } else { Loc::Gpr(r) };
            active[class].push(v);
            continue;
        }
        // Spill whichever ends last: the active one gives up its register.
        let (ki, &victim) = active[class]
            .iter()
            .enumerate()
            .max_by_key(|&(_, &a)| end[a])
            .expect("a full class has active intervals");
        if end[victim] > end[v] {
            loc[v] = loc[victim];
            loc[victim] = Loc::Slot(slots);
            slots += 1;
            active[class][ki] = v;
        } else {
            loc[v] = Loc::Slot(slots);
            slots += 1;
        }
    }
    Alloc { loc, slots }
}
