// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Inferred type of a region value. The allocator places numbers in xmm
/// registers and booleans (compare results) in gprs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum VTy {
    Num,
    Bool,
}

/// Where a region value lives for the duration of the loop.
#[derive(Clone, Copy)]
pub(crate) enum Home {
    Xmm(u8),
    Gpr(u8),
}

/// Register-allocation plan for a region: a fixed xmm/gpr home per VM register
/// and per global, computed by a type+liveness pass. `None` (decline) when the
/// region is outside the allocator's subset (too many live values, a type
/// conflict, an unsupported live-in, etc.) — the caller then uses the memory path.
pub(crate) struct RegionPlan {
    pub(crate) reg_home: FxHashMap<u16, Home>,
    pub(crate) glob_home: FxHashMap<u32, u8>, // global slot → xmm index
    /// Numeric registers loaded (and type-guarded) at entry. This is EVERY
    /// xmm-homed reg except the hoisted constants, not just the loop-carried
    /// ones: `flush_exit` writes every home in `num_regs` back to the reg file,
    /// and a region entered at the OSR back-edge can reach an exit BEFORE a
    /// def-first reg's def runs (loop condition already false, or a guard fails
    /// early, or the def sits on an untaken branch). Loading them all makes that
    /// flush write back the value the frame already held instead of whatever
    /// happened to be in the home. See `live_in_bools`.
    pub(crate) live_in_regs: Vec<(u16, u8)>, // (reg, xmm)
    /// Bool registers loaded (and Bool-guarded) at entry — all of them, for the
    /// same reason as `live_in_regs`. Bool homes are gprs the prologue does not
    /// otherwise initialise, so before this they flushed raw garbage.
    pub(crate) live_in_bools: Vec<(u16, u8)>, // (reg, gpr)
    /// Globals loaded (and guarded) at entry — every global in `globs`.
    pub(crate) live_in_globs: Vec<(u32, u8)>, // (slot, xmm)
    /// All numeric reg homes (flushed to the reg file on exit).
    pub(crate) num_regs: Vec<(u16, u8)>,
    /// All bool reg homes (boxed + flushed on exit).
    pub(crate) bool_regs: Vec<(u16, u8)>,
    /// All globals touched (flushed to globals memory on exit).
    pub(crate) globs: Vec<(u32, u8)>,
    /// Loop-invariant constants to materialise ONCE in the prologue: region ips
    /// of `LoadInt`/`LoadConst` whose dst is defined exactly once and never
    /// live-in. Their body occurrences are skipped (the home already holds them).
    pub(crate) hoist_ips: Vec<usize>,
    pub(crate) hoisted: FxHashSet<u16>,
    /// Registers DEFINED in the region but NEVER used as an operand (dead). All
    /// int-region ops are pure value computations (no side effects — heap/call ops
    /// decline the region), so a dead-dst op produces a value nothing reads and is
    /// skipped during body emission. The common source is object SROA, which
    /// neutralises the (now-unused) object-ref `LoadGlobal`s to `LoadInt 0`: that's
    /// ~7 dead ops/iteration in the object benchmark, and dropping them also frees
    /// their xmm homes (often taking the loop off the slower home-reuse path).
    pub(crate) dead: FxHashSet<u16>,
    /// Arithmetic region ips whose 2^53 overflow guard is PROVABLY unnecessary
    /// (interval analysis showed the result always lands in `[-2^53, 2^53]`, e.g.
    /// a loop counter bounded by the loop condition's constant). INT path only;
    /// for a `Mul` it also licenses dropping the i64-overflow `jo` check.
    pub(crate) elide_guard: FxHashSet<usize>,
    /// Guard-elided `Mul` ips whose one operand is a single-def constant power of
    /// two: `(value operand reg, shift)` — emitted as `psllq` instead of an
    /// imul gpr round-trip. INT path only (f64 keeps `mulsd`).
    pub(crate) mul_shift: FxHashMap<usize, (u16, u8)>,
    /// `AddInt` immediates hoisted into spare xmm const homes (filled once in the
    /// prologue; the int path stores the i64, the double path the f64 bits).
    pub(crate) addint_imm_home: FxHashMap<i32, u8>,
    /// Hoisted integer-constant registers mirrored in a spare (otherwise unused)
    /// bool gpr, so int-path compares read `cmp rax, Rq(g)` instead of a second
    /// `movq` from the constant's xmm home. `(gpr, value)` per const reg.
    pub(crate) gpr_const: FxHashMap<u16, (u8, i64)>,
    /// In-region jump-target ips (any branch lands there, incl. the loop header).
    /// Used to gate the compare+branch flag-fusion peephole: a branch that is
    /// itself a jump target can't rely on flags from the preceding compare.
    pub(crate) jump_targets: FxHashSet<usize>,
    /// Unboxed-region epic: receiver registers of pinned-Float64Array GetIndex/
    /// SetIndex (each defined by ONE LoadGlobal, used ONLY as such a receiver). They
    /// are NOT homed — the element-access emitter reads the live receiver via the
    /// pin's source for its identity guard — so their defining LoadGlobal body op is
    /// a no-op. Empty unless the f64-TA element fast path admitted this region.
    pub(crate) ta_recv_regs: FxHashSet<u16>,
}

/// First xmm index usable as a value home (xmm0/xmm1 are scratch for the few ops
/// that need a temporary). xmm2..=xmm15 ⇒ 14 numeric homes.
pub(crate) const HOME_XMM_FIRST: u8 = 2;
pub(crate) const HOME_XMM_LAST: u8 = 15;
/// Gpr pool for boolean homes (r8..r11, all volatile; the region issues no calls
/// in its body so they survive). 4 simultaneous bools.
pub(crate) const BOOL_GPRS: [u8; 4] = [8, 9, 10, 11];

/// A numeric value being allocated an xmm home: a VM register or a global slot.
pub(crate) enum NumVal {
    Reg(u16),
    Glob(u32),
}

/// Tiny linear-scan xmm allocator: hands out home indices and reuses one once its
/// interval has ended. Intervals MUST be supplied in ascending start order. Used
/// only when one-home-per-value would overflow the pool (e.g. object SROA loops);
/// reusing a register can cost ILP, so simpler loops keep distinct homes.
pub(crate) struct XmmAlloc {
    pub(crate) next: u8,                 // next never-used xmm index
    pub(crate) active: Vec<(usize, u8)>, // (interval_end, xmm) currently live
    pub(crate) free: Vec<u8>,            // homes freed by expired intervals, available to reuse
}

impl XmmAlloc {
    pub(crate) fn new() -> XmmAlloc {
        XmmAlloc { next: HOME_XMM_FIRST, active: Vec::new(), free: Vec::new() }
    }

    /// Allocate a home for the interval `[start, end]`, or `None` if the pool is
    /// exhausted even after expiring intervals that ended before `start`.
    pub(crate) fn alloc(&mut self, start: usize, end: usize) -> Option<u8> {
        let mut i = 0;
        while i < self.active.len() {
            if self.active[i].0 < start {
                self.free.push(self.active[i].1);
                self.active.swap_remove(i);
            } else {
                i += 1;
            }
        }
        let x = if let Some(x) = self.free.pop() {
            x
        } else if self.next <= HOME_XMM_LAST {
            let x = self.next;
            self.next += 1;
            x
        } else {
            return None;
        };
        self.active.push((end, x));
        Some(x)
    }
}

// ── region home unification (copy coalescing) ───────────────────────────────
//
// A region temp that only ever shuttles a global's value (`LoadGlobal r ← g` /
// `<arith> r; StoreGlobal g ← r` pairs) can share the GLOBAL's xmm home, which
// deletes the `movdqa`/`movaps` copies from the loop body (the same effect as
// V8's copy coalescing). Soundness hinges on the exit-flush: an aliased reg's
// slot is flushed FROM THE SHARED HOME, so it must be provable that wherever the
// interpreter can resume, it never reads the reg before re-executing a def —
// hence the dominance (no jump target into a def's use window) and no-store
// (the global isn't redefined inside the window) conditions below.

/// In-region jump-target ips of `[s, e]` (branch targets that stay inside the
/// region, plus the OSR entry header `s` which the prologue jumps to).
pub(crate) fn region_jump_targets(code: &[Instr], s: usize, e: usize) -> FxHashSet<usize> {
    let mut t: FxHashSet<usize> = FxHashSet::default();
    t.insert(s);
    for instr in &code[s..=e] {
        let target = match *instr {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target as usize,
            _ => continue,
        };
        if target >= s && target <= e {
            t.insert(target);
        }
    }
    t
}

/// Detect regs that can share a GLOBAL's home: every def of `r` is either
/// `LoadGlobal g` (same `g` for all defs) or an op immediately followed by
/// `StoreGlobal g ← r`; per def, the use window contains no other store to `g`
/// and no jump target. Returns `reg → global` for each unifiable reg.
#[allow(clippy::too_many_arguments)]
pub(crate) fn unify_homes_with_globals(
    code: &[Instr],
    s: usize,
    e: usize,
    ty: &FxHashMap<u16, VTy>,
    first_seen: &FxHashMap<u16, bool>,
    dead: &FxHashSet<u16>,
    hoisted: &FxHashSet<u16>,
    jump_targets: &FxHashSet<usize>,
) -> FxHashMap<u16, u32> {
    // defs / uses per reg, in ascending ip order. An operand read at a def ip
    // (e.g. `Add r = r + x`) is attributed to the PREVIOUS def's window.
    let mut defs: FxHashMap<u16, Vec<usize>> = FxHashMap::default();
    let mut uses: FxHashMap<u16, Vec<usize>> = FxHashMap::default();
    let mut g_stores: FxHashMap<u32, Vec<usize>> = FxHashMap::default();
    for ip in s..=e {
        for u in instr_uses(&code[ip]) {
            uses.entry(u).or_default().push(ip);
        }
        if let Some(d) = writes_reg(&code[ip]) {
            defs.entry(d).or_default().push(ip);
        }
        if let Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } = code[ip] {
            g_stores.entry(idx).or_default().push(ip);
        }
    }

    let mut alias: FxHashMap<u16, u32> = FxHashMap::default();
    'cand: for (&r, def_ips) in &defs {
        if ty.get(&r) != Some(&VTy::Num)
            || first_seen.get(&r) != Some(&true) // live-in regs keep their own home
            || dead.contains(&r)
            || hoisted.contains(&r) // hoisted consts materialise in the prologue
        {
            continue;
        }
        let use_ips = match uses.get(&r) {
            Some(u) if !u.is_empty() => u,
            _ => continue, // no uses: nothing to win
        };
        // All defs must agree on one global `g`.
        let mut g: Option<u32> = None;
        // The def ips whose form is `<arith>; StoreGlobal g ← r` (the store ip is
        // exempt from the window's "no store to g" rule — it WRITES r's value).
        let mut adj_store_ips: FxHashSet<usize> = FxHashSet::default();
        for &d in def_ips {
            let gd = match code[d] {
                Instr::LoadGlobal { idx, .. } => idx,
                _ => {
                    // Must be immediately followed by `StoreGlobal g ← r`, and a
                    // path must not be able to enter AT the store (jump target).
                    match code.get(d + 1) {
                        Some(&Instr::StoreGlobal { idx, src })
                        | Some(&Instr::StoreGlobalStrict { idx, src })
                            if src == r && d + 1 <= e && !jump_targets.contains(&(d + 1)) =>
                        {
                            adj_store_ips.insert(d + 1);
                            idx
                        }
                        _ => continue 'cand,
                    }
                }
            };
            match g {
                None => g = Some(gd),
                Some(prev) if prev == gd => {}
                Some(_) => continue 'cand,
            }
        }
        let g = match g {
            Some(g) => g,
            None => continue,
        };
        // Per-def window check: (d, u_last] must contain no foreign store to `g`
        // and no jump target. A use AT the next def ip (operand of the redefining
        // op) belongs to THIS window.
        for (k, &d) in def_ips.iter().enumerate() {
            let next_d = def_ips.get(k + 1).copied().unwrap_or(usize::MAX);
            let u_last = use_ips
                .iter()
                .copied()
                .filter(|&u| u > d && (u < next_d || u == next_d))
                .max();
            let u_last = match u_last {
                Some(u) => u,
                None => continue,
            };
            if let Some(stores) = g_stores.get(&g) {
                if stores
                    .iter()
                    .any(|&sip| sip > d && sip <= u_last && !adj_store_ips.contains(&sip))
                {
                    continue 'cand;
                }
            }
            if jump_targets.iter().any(|&t| t > d && t <= u_last) {
                continue 'cand;
            }
        }
        alias.insert(r, g);
    }
    alias
}

/// Detect `Move dst ← src` temps that can share a LIVE-IN reg's home (`src` is
/// loop-carried so its home spans the whole region in both allocation modes).
/// Conditions mirror `unify_homes_with_globals`: dst single-def, src not
/// redefined and no jump target inside dst's use window.
pub(crate) fn unify_move_homes(
    code: &[Instr],
    s: usize,
    e: usize,
    ty: &FxHashMap<u16, VTy>,
    first_seen: &FxHashMap<u16, bool>,
    dead: &FxHashSet<u16>,
    hoisted: &FxHashSet<u16>,
    jump_targets: &FxHashSet<usize>,
    glob_alias: &FxHashMap<u16, u32>,
) -> FxHashMap<u16, u16> {
    let mut defs: FxHashMap<u16, Vec<usize>> = FxHashMap::default();
    let mut uses: FxHashMap<u16, Vec<usize>> = FxHashMap::default();
    for ip in s..=e {
        for u in instr_uses(&code[ip]) {
            uses.entry(u).or_default().push(ip);
        }
        if let Some(d) = writes_reg(&code[ip]) {
            defs.entry(d).or_default().push(ip);
        }
    }
    let mut alias: FxHashMap<u16, u16> = FxHashMap::default();
    for ip in s..=e {
        let (dst, src) = match code[ip] {
            Instr::Move { dst, src } => (dst, src),
            _ => continue,
        };
        if ty.get(&dst) != Some(&VTy::Num)
            || ty.get(&src) != Some(&VTy::Num)
            || first_seen.get(&dst) != Some(&true)
            || dead.contains(&dst)
            || hoisted.contains(&dst)
            || glob_alias.contains_key(&dst) // already unified with a global
            || defs.get(&dst).map(|d| d.len()) != Some(1)
            // src must be live-in (whole-region home) and not itself re-homed.
            || first_seen.get(&src) != Some(&false)
            || glob_alias.contains_key(&src)
        {
            continue;
        }
        let u_last = match uses.get(&dst).and_then(|u| u.iter().copied().max()) {
            Some(u) => u,
            None => continue,
        };
        // src not redefined and no jump target in (ip, u_last].
        let src_redef = defs
            .get(&src)
            .map_or(false, |d| d.iter().any(|&di| di > ip && di <= u_last));
        if src_redef || jump_targets.iter().any(|&t| t > ip && t <= u_last) {
            continue;
        }
        alias.insert(dst, src);
    }
    alias
}

// ── int-region interval analysis (overflow-guard elision) ───────────────────
//
// Forward abstract interpretation over the region's small CFG with an interval
// domain on regs + globals. Live-in values are Int-tagged at entry (i32 range),
// constants are exact, guarded arithmetic clamps its result to [-2^53, 2^53]
// (the guard bails otherwise), and the loop-bound compare refines the counter's
// interval on the fall-through edge. Any arithmetic whose UNCLAMPED result is
// proven inside [-2^53, 2^53] keeps the invariant without a runtime check, so
// its guard is elided (and a Mul's i64-overflow `jo` with it).

pub(crate) type Iv = (i64, i64);
