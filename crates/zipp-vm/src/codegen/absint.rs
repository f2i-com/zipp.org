// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

pub(crate) const IV_FULL: Iv = (-TWO_POW_53, TWO_POW_53);
pub(crate) const IV_I32: Iv = (i32::MIN as i64, i32::MAX as i64);
/// Sentinel bound for out-of-range mul products (keeps i64 math safe).
pub(crate) const IV_BIG: i64 = TWO_POW_54;

pub(crate) fn iv_join(a: Iv, b: Iv) -> Iv {
    (a.0.min(b.0), a.1.max(b.1))
}
pub(crate) fn iv_clamp(a: Iv) -> Iv {
    (a.0.max(-TWO_POW_53), a.1.min(TWO_POW_53))
}
pub(crate) fn iv_in_bounds(a: Iv) -> bool {
    a.0 >= -TWO_POW_53 && a.1 <= TWO_POW_53
}
pub(crate) fn iv_add(a: Iv, b: Iv) -> Iv {
    // Operands are clamped to ±2^53 (invariant), so sums stay well inside i64.
    (a.0 + b.0, a.1 + b.1)
}
pub(crate) fn iv_sub(a: Iv, b: Iv) -> Iv {
    (a.0 - b.1, a.1 - b.0)
}
pub(crate) fn iv_mul(a: Iv, b: Iv) -> Iv {
    let c = [
        (a.0 as i128) * (b.0 as i128),
        (a.0 as i128) * (b.1 as i128),
        (a.1 as i128) * (b.0 as i128),
        (a.1 as i128) * (b.1 as i128),
    ];
    let lo = *c.iter().min().unwrap();
    let hi = *c.iter().max().unwrap();
    (
        lo.clamp(-(IV_BIG as i128), IV_BIG as i128) as i64,
        hi.clamp(-(IV_BIG as i128), IV_BIG as i128) as i64,
    )
}

/// Abstract state at a program point: intervals for numeric regs/globals (a
/// missing key means "unknown" = `IV_FULL`), the `reg == global` copy facts used
/// to propagate branch refinements to the source global, and the most recent
/// compare (for refining at an immediately following conditional branch).
#[derive(Clone, PartialEq)]
pub(crate) struct AbsState {
    pub(crate) regs: FxHashMap<u16, Iv>,
    pub(crate) globs: FxHashMap<u32, Iv>,
    pub(crate) alias: FxHashMap<u16, u32>,
    pub(crate) cmp: Option<(u16, u16, u16, Cmp, usize)>, // (cond, a, b, op, ip)
}

impl AbsState {
    pub(crate) fn reg(&self, r: u16) -> Iv {
        self.regs.get(&r).copied().unwrap_or(IV_FULL)
    }
    pub(crate) fn glob(&self, g: u32) -> Iv {
        self.globs.get(&g).copied().unwrap_or(IV_FULL)
    }
    /// Pointwise join into `self`; returns true if `self` changed. `widen`
    /// pushes any growing bound straight to its 2^53 extreme (fast convergence).
    pub(crate) fn join_from(&mut self, other: &AbsState, widen: bool) -> bool {
        let mut changed = false;
        // A key missing on either side means FULL; FULL is absorbing, so keep
        // only keys present in BOTH (others drop to the implicit FULL).
        let keys: Vec<u16> = self.regs.keys().copied().collect();
        for r in keys {
            let a = self.regs[&r];
            let j = match other.regs.get(&r) {
                Some(&b) => iv_join(a, b),
                None => IV_FULL,
            };
            let j = if widen && j != a {
                (
                    if j.0 < a.0 { -TWO_POW_53 } else { j.0 },
                    if j.1 > a.1 { TWO_POW_53 } else { j.1 },
                )
            } else {
                j
            };
            if j != a {
                self.regs.insert(r, j);
                changed = true;
            }
        }
        let keys: Vec<u32> = self.globs.keys().copied().collect();
        for g in keys {
            let a = self.globs[&g];
            let j = match other.globs.get(&g) {
                Some(&b) => iv_join(a, b),
                None => IV_FULL,
            };
            let j = if widen && j != a {
                (
                    if j.0 < a.0 { -TWO_POW_53 } else { j.0 },
                    if j.1 > a.1 { TWO_POW_53 } else { j.1 },
                )
            } else {
                j
            };
            if j != a {
                self.globs.insert(g, j);
                changed = true;
            }
        }
        let before = self.alias.len();
        self.alias.retain(|r, g| other.alias.get(r) == Some(g));
        if self.alias.len() != before {
            changed = true;
        }
        if self.cmp != other.cmp && self.cmp.is_some() {
            self.cmp = None;
            changed = true;
        }
        changed
    }
    /// Narrow reg `r` (and, via the copy fact, its source global) to `iv`.
    pub(crate) fn refine_reg(&mut self, r: u16, iv: Iv) {
        let cur = self.reg(r);
        let n = (cur.0.max(iv.0), cur.1.min(iv.1));
        self.regs.insert(r, n);
        if let Some(&g) = self.alias.get(&r) {
            let cg = self.glob(g);
            self.globs.insert(g, (cg.0.max(iv.0), cg.1.min(iv.1)));
        }
    }
    /// Is any tracked interval empty (an infeasible path)?
    pub(crate) fn infeasible(&self) -> bool {
        self.regs.values().any(|&(lo, hi)| lo > hi) || self.globs.values().any(|&(lo, hi)| lo > hi)
    }
}

/// Refine `a <cmp> b == truth` into `st` (both operands, alias-propagated).
pub(crate) fn refine_cmp(st: &mut AbsState, a: u16, b: u16, cmp: Cmp, truth: bool) {
    let (ia, ib) = (st.reg(a), st.reg(b));
    // Normalise to a "less" relation: a < b / a <= b (swapping for Gt/Ge).
    let (l, r, il, ir, le, holds) = match (cmp, truth) {
        (Cmp::Lt, t) => (a, b, ia, ib, false, t),
        (Cmp::Le, t) => (a, b, ia, ib, true, t),
        (Cmp::Gt, t) => (b, a, ib, ia, false, t),
        (Cmp::Ge, t) => (b, a, ib, ia, true, t),
        (Cmp::Eq, true) | (Cmp::Ne, false) => {
            let m = (ia.0.max(ib.0), ia.1.min(ib.1));
            st.refine_reg(a, m);
            st.refine_reg(b, m);
            return;
        }
        (Cmp::Eq, false) | (Cmp::Ne, true) => return,
    };
    if holds {
        // l < r (or <=): l_hi ≤ r_hi (-1), r_lo ≥ l_lo (+1).
        let adj = if le { 0 } else { 1 };
        st.refine_reg(l, (i64::MIN, ir.1 - adj));
        st.refine_reg(r, (il.0 + adj, i64::MAX));
    } else {
        // !(l < r) ⇔ l ≥ r (or l > r for !(<=)).
        let adj = if le { 1 } else { 0 };
        st.refine_reg(l, (ir.0 + adj, i64::MAX));
        st.refine_reg(r, (i64::MIN, il.1 - adj));
    }
}

/// W10 (B123): the DV prover with STRICT-ENTRY seeding. `strict_globs` comes
/// in holding CANDIDATE global slots (loop-carried accumulators stored in the
/// region) and leaves holding the verified SURVIVORS: each survivor was
/// optimistically seeded `IV_I32` at entry, and the converged fixpoint's
/// header join stayed within i32 — the standard seed-and-check invariant
/// proof, valid PROVIDED the entry value really is i32, which the caller
/// enforces by marking the survivor's entry load strict (bail on wider — an
/// entry bail computes nothing, so it is always sound). A candidate whose
/// join escapes i32 is pruned and the fixpoint re-runs (the set shrinks
/// monotonically, so this terminates). Without this, a `|0`-truncated
/// accumulator like the DV swizzle's `bsum` enters at the ±2^53 entry-guard
/// interval and poisons every add on its chain.
pub(crate) fn analyze_int_guards_strict(
    proto: &FuncProto,
    s: usize,
    e: usize,
    entry: AbsState,
    dv: Option<&TaPinPlan>,
    strict_globs: &mut FxHashSet<u32>,
) -> FxHashSet<usize> {
    let n = e - s + 1;
    if n > 512 {
        strict_globs.clear();
        return FxHashSet::default();
    }
    let mut cands: FxHashSet<u32> = strict_globs.clone();
    loop {
        let mut seeded = entry.clone();
        for &g in &cands {
            seeded.globs.insert(g, IV_I32);
        }
        match analyze_run(proto, s, e, seeded, dv) {
            None => {
                if std::env::var_os("ZIPP_ABSINT_LOG").is_some() {
                    eprintln!("[absint] [{s},{e}] analyze_run=None (unsupported op or no convergence)");
                }
                strict_globs.clear();
                return FxHashSet::default();
            }
            Some((states, elide)) => {
                if std::env::var_os("ZIPP_ABSINT_LOG").is_some() {
                    let mut c: Vec<u32> = cands.iter().copied().collect();
                    c.sort_unstable();
                    eprintln!("[absint] [{s},{e}] cands={c:?} elided={}", elide.len());
                    if let Some(g) = std::env::var("ZIPP_ABSINT_GLOB").ok().and_then(|v| v.parse::<u32>().ok()) {
                        for (off, i) in proto.code[s..=e].iter().enumerate() {
                            let ip = s + off;
                            match states[off].as_ref() {
                                Some(st) => {
                                    let al: Vec<u16> = st.alias.iter().filter(|(_, &gg)| gg == g).map(|(&r, _)| r).collect();
                                    let ops: String = match *i {
                                        Instr::JumpIfNotLt { a, b, .. }
                                        | Instr::JumpIfNotLe { a, b, .. }
                                        | Instr::Mul { a, b, .. }
                                        | Instr::Add { a, b, .. } => {
                                            format!(" a={:?} b={:?}", st.reg(a), st.reg(b))
                                        }
                                        _ => String::new(),
                                    };
                                    eprintln!("[absint]     @{ip} g{g}={:?} alias={al:?} {i:?}{ops}", st.glob(g));
                                }
                                None => eprintln!("[absint]     @{ip} <unreached> {i:?}"),
                            }
                        }
                    }
                    for (off, i) in proto.code[s..=e].iter().enumerate() {
                        let ip = s + off;
                        if let Instr::Add { a, b, .. } = *i {
                            if !elide.contains(&ip) {
                                let st = states[off].as_ref();
                                eprintln!(
                                    "[absint]   guarded Add @{ip}: a={:?} b={:?} entry-globs={:?}",
                                    st.map(|st| st.reg(a)),
                                    st.map(|st| st.reg(b)),
                                    states[0].as_ref().map(|st| {
                                        let mut g: Vec<(u32, Iv)> =
                                            st.globs.iter().map(|(&k, &v)| (k, v)).collect();
                                        g.sort_unstable();
                                        g
                                    })
                                );
                            }
                        }
                    }
                }
                let bad: Vec<u32> = cands
                    .iter()
                    .copied()
                    .filter(|&g| {
                        states[0].as_ref().map_or(true, |st| {
                            let iv = st.glob(g);
                            iv.0 < i32::MIN as i64 || iv.1 > i32::MAX as i64
                        })
                    })
                    .collect();
                if bad.is_empty() {
                    *strict_globs = cands;
                    return elide;
                }
                for g in bad {
                    cands.remove(&g);
                }
                if cands.is_empty() {
                    strict_globs.clear();
                    return analyze_run(proto, s, e, entry, dv)
                        .map(|(_, el)| el)
                        .unwrap_or_default();
                }
            }
        }
    }
}

/// One full fixpoint + elide collection from a given entry state. `None` on
/// an op outside the (possibly DV-extended) subset or non-convergence.
#[allow(clippy::type_complexity)]
fn analyze_run(
    proto: &FuncProto,
    s: usize,
    e: usize,
    entry: AbsState,
    dv: Option<&TaPinPlan>,
) -> Option<(Vec<Option<AbsState>>, FxHashSet<usize>)> {
    let n = e - s + 1;
    // states[i] = abstract state BEFORE executing ip s+i.
    let mut states: Vec<Option<AbsState>> = vec![None; n];
    states[0] = Some(entry);

    // Transfer of one op. Returns (fallthrough_state, optional (target, state)).
    // `elide` (when Some) collects guard-elidable arithmetic ips on a final pass.
    #[allow(clippy::type_complexity)]
    pub(crate) fn step(
        proto: &FuncProto,
        ip: usize,
        st: &AbsState,
        elide: Option<&mut FxHashSet<usize>>,
        dv: Option<&TaPinPlan>,
    ) -> Option<(Option<AbsState>, Option<(usize, AbsState)>)> {
        let code = &proto.code;
        let mut out = st.clone();
        out.cmp = None;
        let arith = |out: &mut AbsState, dst: u16, iv: Iv, elide: Option<&mut FxHashSet<usize>>| {
            if iv_in_bounds(iv) {
                if let Some(set) = elide {
                    set.insert(ip);
                }
            }
            out.regs.insert(dst, iv_clamp(iv));
            out.alias.remove(&dst);
        };
        match code[ip] {
            Instr::LoadInt { dst, val } => {
                out.regs.insert(dst, (val as i64, val as i64));
                out.alias.remove(&dst);
            }
            Instr::LoadConst { dst, idx } => {
                let c = proto.constants[idx as usize];
                if !c.is_int() {
                    return None;
                }
                let v = (c.bits() as u32 as i32) as i64;
                out.regs.insert(dst, (v, v));
                out.alias.remove(&dst);
            }
            Instr::Move { dst, src } => {
                out.regs.insert(dst, st.reg(src));
                match st.alias.get(&src).copied() {
                    Some(g) => {
                        out.alias.insert(dst, g);
                    }
                    None => {
                        out.alias.remove(&dst);
                    }
                }
            }
            Instr::LoadGlobal { dst, idx } => {
                out.regs.insert(dst, st.glob(idx));
                out.alias.insert(dst, idx);
            }
            Instr::StoreGlobal { idx, src }
            | Instr::StoreGlobalStrict { idx, src }
            | Instr::StoreGlobalResolved { idx, src } => {
                out.globs.insert(idx, st.reg(src));
                out.alias.retain(|_, g| *g != idx);
                out.alias.insert(src, idx);
            }
            Instr::AddInt { dst, a, imm, .. } => {
                arith(
                    &mut out,
                    dst,
                    iv_add(st.reg(a), (imm as i64, imm as i64)),
                    elide,
                );
            }
            Instr::Add { dst, a, b } => arith(&mut out, dst, iv_add(st.reg(a), st.reg(b)), elide),
            Instr::Sub { dst, a, b } => arith(&mut out, dst, iv_sub(st.reg(a), st.reg(b)), elide),
            Instr::Mul { dst, a, b } => arith(&mut out, dst, iv_mul(st.reg(a), st.reg(b)), elide),
            Instr::Neg { dst, a } => {
                let (lo, hi) = st.reg(a);
                arith(&mut out, dst, (-hi, -lo), elide);
            }
            Instr::Mod { dst, .. } => {
                // |rem| < |b| ≤ 2^53; never guarded (see the Mod emitter).
                out.regs.insert(dst, (-(TWO_POW_53 - 1), TWO_POW_53 - 1));
                out.alias.remove(&dst);
            }
            Instr::Lt { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Lt, ip)),
            Instr::Le { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Le, ip)),
            Instr::Gt { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Gt, ip)),
            Instr::Ge { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Ge, ip)),
            Instr::Eq { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Eq, ip)),
            Instr::Ne { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Ne, ip)),
            Instr::Jump { target } => {
                return Some((None, Some((target as usize, out))));
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                let if_false = matches!(code[ip], Instr::JumpIfFalse { .. });
                let mut fall = out.clone();
                let mut jump = out;
                if let Some((c, a, b, op, cip)) = st.cmp {
                    if c == cond && cip + 1 == ip {
                        // fall-through executes when cond == !if_false… i.e. the
                        // branch is NOT taken: JumpIfFalse falls through on TRUE.
                        refine_cmp(&mut fall, a, b, op, if_false);
                        refine_cmp(&mut jump, a, b, op, !if_false);
                    }
                }
                return Some((Some(fall), Some((target as usize, jump))));
            }
            Instr::JumpIfNotLt { a, b, target } | Instr::JumpIfNotLe { a, b, target } => {
                let op = if matches!(code[ip], Instr::JumpIfNotLt { .. }) {
                    Cmp::Lt
                } else {
                    Cmp::Le
                };
                let mut fall = out.clone();
                let mut jump = out;
                refine_cmp(&mut fall, a, b, op, true);
                refine_cmp(&mut jump, a, b, op, false);
                return Some((Some(fall), Some((target as usize, jump))));
            }
            Instr::Return { .. } | Instr::ReturnUndefined => return Some((None, None)),
            // ── W10 DV-region arms (dv-retry plans only; see the fn doc) ──
            Instr::Bitwise { dst, op, .. } if dv.is_some() => {
                use crate::bytecode::BitwiseOp as B;
                let iv: Iv = if matches!(op, B::Ushr) {
                    (0, (1i64 << 32) - 1) // a u32
                } else {
                    IV_I32 // ToInt32 by definition
                };
                out.regs.insert(dst, iv);
                out.alias.remove(&dst);
            }
            Instr::MathOp {
                dst,
                op: MathFn::Imul,
                argc: 2,
                ..
            } if dv.is_some() => {
                out.regs.insert(dst, IV_I32); // ToInt32 by definition
                out.alias.remove(&dst);
            }
            Instr::CallMethod { dst, name, .. } if dv.is_some() => {
                let ta = dv.unwrap();
                let iv: Iv = match ta.access.get(&ip).map(|&j| ta.pins[j as usize].kind) {
                    Some(k) if k == DV_PIN_KIND => {
                        match proto
                            .string_constants
                            .get(name as usize)
                            .and_then(|s2| dv_get_kind(s2))
                        {
                            Some(0) => (-128, 127),
                            Some(1) => (0, 255),
                            Some(3) => (-32768, 32767),
                            Some(4) => (0, 65535),
                            Some(5) => IV_I32,
                            Some(6) => (0, (1i64 << 32) - 1),
                            _ => return None, // float kinds never reach an INT plan
                        }
                    }
                    // Flat-ASCII pinned charCodeAt: a byte.
                    Some(k) if k == STR_PIN_KIND => (0, 255),
                    _ => return None, // an unpinned call is outside the subset
                };
                out.regs.insert(dst, iv);
                out.alias.remove(&dst);
            }
            // A pinned int element read defines an i32; a pinned store defines
            // nothing. Unpinned index ops stay outside the subset.
            Instr::GetIndex { dst, .. } if dv.is_some() => {
                let ta = dv.unwrap();
                match ta.access.get(&ip).map(|&j| ta.pins[j as usize].kind) {
                    Some(k) if k == 5 || k == ARR_INT_PIN_KIND => {
                        out.regs.insert(dst, IV_I32);
                        out.alias.remove(&dst);
                    }
                    _ => return None,
                }
            }
            Instr::SetIndex { .. } if dv.is_some() => {
                if dv.unwrap().access.get(&ip).is_none() {
                    return None;
                }
            }
            // A pinned length read: non-negative, within the 2^53 range.
            Instr::GetProp { dst, .. } if dv.is_some() => {
                if dv.unwrap().access.get(&ip).is_none() {
                    return None;
                }
                out.regs.insert(dst, (0, TWO_POW_53));
                out.alias.remove(&dst);
            }
            _ => return None, // outside the modelled subset
        }
        Some((Some(out), None))
    }

    // Fixpoint with widening after a few passes; cap the pass count hard.
    let mut pass = 0usize;
    loop {
        pass += 1;
        if pass > 40 {
            return None; // no convergence — keep all guards
        }
        let widen = pass > 8;
        let mut changed = false;
        for ip in s..=e {
            let st = match &states[ip - s] {
                Some(st) if !st.infeasible() => st.clone(),
                _ => continue,
            };
            let (fall, jump) = match step(proto, ip, &st, None, dv) {
                Some(r) => r,
                None => return None,
            };
            let merge = |tip: usize, ns: AbsState, states: &mut Vec<Option<AbsState>>| {
                if tip < s || tip > e || ns.infeasible() {
                    return false; // exits the region (or a dead edge)
                }
                match &mut states[tip - s] {
                    Some(old) => old.join_from(&ns, widen),
                    slot @ None => {
                        *slot = Some(ns);
                        true
                    }
                }
            };
            if let Some(f) = fall {
                changed |= merge(ip + 1, f, &mut states);
            }
            if let Some((t, j)) = jump {
                changed |= merge(t, j, &mut states);
            }
        }
        if !changed {
            break;
        }
    }

    // Final pass over the stable states: collect provably-in-bounds arithmetic.
    let mut elide: FxHashSet<usize> = FxHashSet::default();
    for ip in s..=e {
        if let Some(st) = &states[ip - s] {
            if !st.infeasible() {
                let _ = step(proto, ip, st, Some(&mut elide), dv);
            }
        }
    }
    Some((states, elide))
}
