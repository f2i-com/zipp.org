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
    /// Registers whose post-region textual uses are all dominated by a later
    /// post-region def. Their in-region value is therefore unobservable after
    /// any forward exit. Computed by the planner's fail-closed CFG/gap proof.
    pub(crate) outside_dead: FxHashSet<u16>,
    /// Arithmetic region ips whose 2^53 overflow guard is PROVABLY unnecessary
    /// (interval analysis showed the result always lands in `[-2^53, 2^53]`, e.g.
    /// a loop counter bounded by the loop condition's constant). INT path only;
    /// for a `Mul` it also licenses dropping the i64-overflow `jo` check.
    pub(crate) elide_guard: FxHashSet<usize>,
    /// W10 (B123): global slots whose entry load must be STRICT-i32 (bail on
    /// a wider value) because the DV guard prover assumed an i32 entry
    /// interval for them — licensed exactly like the W8 lazy strict set: an
    /// entry bail computes nothing, so bailing is always sound, and every
    /// in-region def of these globals provably writes i32 (`|0`-truncated
    /// accumulators). Empty for every non-DV plan.
    pub(crate) strict_entry_globs: FxHashSet<u32>,
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
    /// B94 live-range splitting. A VM register the bytecode compiler RECYCLED:
    /// it is a pinned-access receiver (an object) over one range and a numeric
    /// value over a disjoint one. `ta_recv_regs` cannot hold it — the numeric
    /// range needs a real home — and the one-def test rejected it, which is why
    /// the simplest `for (i…) { v = a[i]; s = s + v; }` declined and regalloc's
    /// pinned-element path was near-dead on real code (B93).
    ///
    /// It gets a NUMERIC home, and its memory slot `[rbx + dreg(r)]` is kept
    /// AUTHORITATIVE at all times: its `LoadGlobal` is emitted as a real store
    /// (not elided as a `ta_recv_regs` one is), and every numeric def writes the
    /// home THROUGH to memory. `flush_exit` therefore skips it entirely.
    ///
    /// Write-through costs two instructions per numeric def and buys the
    /// property that makes this reviewable: **every exit is correct without
    /// knowing which path reached it.** The alternative — flush variants picked
    /// by a per-ip validity dataflow — needs the exit stubs keyed by SOURCE
    /// rather than target, and cannot recover the source for a jump that leaves
    /// the region.
    ///
    /// Every member is independently proven by `split_home_provably_safe` and
    /// independently written through, so the set form is the same mechanism
    /// per register. Admission is still conservative: at most ONE split whose
    /// pinned accesses are element ops (B94's original case), because that is
    /// all that has ever been exercised there — but a receiver whose pinned
    /// accesses are all DataView `get*` CallMethods does not count against
    /// that limit (the DV swizzle loop recycles TWO receiver registers, one
    /// with the loop arithmetic and one with an Eq flag constant, and a
    /// one-split rule declined the whole region).
    pub(crate) split_recvs: FxHashSet<u16>,
    /// B97. Registers whose home is SHARED (linear-scan reuse) but which are read
    /// AFTER the region, so a stale flush into their frame slot would be visible.
    /// They are written THROUGH to `[rbx + dreg(r)]` at every def and skipped by
    /// `flush_exit`, which is the same trade B94 made for the split receiver:
    /// memory is authoritative, the home is a read cache.
    ///
    /// Before this they were forced to a PERMANENT whole-region home — the rule
    /// that made `class-prototype-hot` want 12 register homes plus 10 global homes
    /// against a pool of 14 (B96). The comment at `range()` states the exact
    /// hazard: "flush_exit writes the shared home to EVERY sharer's slot, so a
    /// sharer whose value still matters after the region would come back holding
    /// an unrelated temp." Write-through removes it — each slot receives its own
    /// value at its own def, before the home is reused.
    ///
    /// DISJOINT from `split_recvs` by construction (see the set's build in
    /// `plan_region`): a split receiver already has memory-authoritative
    /// slots from its own mechanism, and the two mechanisms disagree on
    /// exactly one ip class — the receiver `LoadGlobal` — so the overlap only
    /// ever created a way for an emitter to write the numeric home over the
    /// receiver object. Consumers still test both sets with OR; the exclusion
    /// is defence in depth for the next mechanism that adds a member.
    pub(crate) write_through: FxHashSet<u16>,
    /// Region ips of every split receiver's RECEIVER `LoadGlobal` (the one reading the
    /// pinned array's global slot). These emit a real memory store and are the
    /// only defs of the register that do NOT fill its numeric home — the same
    /// register is also loaded from other globals, and those are numeric defs.
    ///
    /// They are therefore the ONE ip class at which no write-through may be
    /// emitted, for a member of EITHER set: the store that belongs there is the
    /// receiver object the `LoadGlobal` arm just wrote, and the home holds the
    /// register's numeric half. Every emitter takes its def from
    /// `emit::wt_def_at`, which is where that exclusion lives.
    pub(crate) split_recv_lg: FxHashSet<usize>,
    /// Stored globals whose home interval is NARROWED to [first touch, last
    /// touch] instead of B96's forced whole-region [s, e] — so the home is
    /// reusable outside that window. Admission is a per-load dominance proof
    /// (every in-region `LoadGlobal` of the slot has a nearest preceding
    /// in-region store with no jump target in the gap — the `slot_guard_key`
    /// straight-line scan), which guarantees the home is store-filled before
    /// any read on every native path; a load not provably dominated keeps the
    /// permanent home (fail closed).
    ///
    /// The exit contract moves from flush to WRITE-THROUGH: each in-region
    /// `StoreGlobal` of a member also boxes the value into `[r12 + 8*slot]`,
    /// `flush_exit` skips the slot (at a mid-iteration exit the home may
    /// already belong to another value — flushing it would expose a stale
    /// slot to the interpreter, the B9-class failure), and there is no entry
    /// load (the home starts garbage; the dominance proof is what makes that
    /// unreadable). Memory therefore holds the last-stored value at EVERY
    /// exit — the same thing the interpreter would have — so no program-wide
    /// read-set scan is needed. Members are never read-first, so they are
    /// disjoint from `strict_entry_globs` by construction. Populated only for
    /// plans routed exclusively into the GPR emitter (`admit_dv` or
    /// `share_homes`); empty under `ZIPP_NO_GLOB_RANGE=1`.
    pub(crate) narrow_globs: FxHashSet<u32>,
    /// Glob-range SLOT-MATERIALIZED constants: single-def in-region
    /// `LoadInt`/Int-`LoadConst` registers (reg → payload) that carry NO home
    /// at all. Their uses read the immediate (they join the emitter's
    /// `hoist_c`), their defining op stores the compile-time-boxed Int to the
    /// register's frame slot — exactly when and what the interpreter's def
    /// would — and the exit flush never touches them, so the slot is
    /// interpreter-exact on EVERY path. This is the remat form for a const
    /// that IS read outside the region (`remat`/`hoisted` requires
    /// `!read_outside`, because an unconditional const flush could clobber a
    /// pre-region value some later code still reads); the def-site store
    /// costs two instructions per iteration where hoisting costs zero, so
    /// the cheaper form is preferred whenever it is sound. Requires the def
    /// to dominate every in-region use (no in-region jump target between
    /// def and use — a native use reads the immediate, and a path reaching
    /// it without the def would let the interpreter disagree). Populated
    /// only for GPR-emitter-only plans; empty under `ZIPP_NO_GLOB_RANGE=1`.
    pub(crate) slot_consts: FxHashMap<u16, i32>,
    /// DV endian-flag fusion: region ips of `Eq` ops ELIDED because their dst
    /// exists only to feed the immediately following pinned-DV `get*` call's
    /// littleEndian flag, while the SAME register is recycled as a numeric
    /// temp later in the body (`le === 1` written into the arg window, then
    /// the window reg reused by the `bsum` adds). Without the fusion that is
    /// a Bool def and a Num def on one register — a type conflict that
    /// declined the whole DV swizzle region. The call computes
    /// ToBoolean(a === b) inline from the two Num homes instead, and its
    /// deopt resumes AT the Eq ip so the interpreter recomputes the flag into
    /// the frame slot before re-running the call (the one-op re-execution
    /// window is pure). Guarded by `plan_region`'s fuse scan: the Eq must be
    /// adjacent, its Bool must be provably dead past the call (killed by a
    /// non-compare def before any use, branch or jump target intervenes, and
    /// the register never read outside the region).
    pub(crate) dv_flag_elide: FxHashSet<usize>,
    /// Pinned-DV call ip → the elided `Eq`'s `(a, b)` operand registers.
    pub(crate) dv_flag_fuse: FxHashMap<usize, (u16, u16)>,
    /// W7 guard hoisting: pin slots whose per-access IDENTITY compare is
    /// hoisted to REGION ENTRY. Sound only because `hoistable_pins` proved the
    /// region cannot invalidate the pin between entry and any access: the
    /// pin's SOURCE (global slot / frame register) has no in-region write, and
    /// every region op is on the closed no-user-code whitelist (nothing can
    /// run user code, allocate, GC, detach/resize a buffer or grow a Vec —
    /// which are the only ways identity, base or length can change). The
    /// snapshot is taken FROM the source in the same prologue, so
    /// `source == obj_bits` holds at entry by construction and the one check
    /// left is snapshot VALIDITY (`obj_bits != 0`); a miss takes `entry_bail`
    /// exactly like a failed live-in guard (no flush, resume at the header,
    /// counts as a deopt → chronic misses evict to the memory tier, whose
    /// per-access guards are untouched). Every OSR re-entry re-snapshots and
    /// re-checks, so a receiver reassigned BETWEEN entries revalidates by
    /// construction. Empty under `ZIPP_NO_GUARD_HOIST=1` (per-access guards
    /// restored byte-identically).
    pub(crate) hoist_pins: FxHashSet<u8>,
    /// W7: region ips of pinned-STRING `.length` GetProps hoisted to a
    /// prologue fill (sorted). Strings are immutable, so once the pin's
    /// identity is entry-guarded (`hoist_pins` holds its slot — a
    /// precondition) the snapshot `units` IS `str.length` for the whole run:
    /// the dst home is filled once from the snapshot and the body op is
    /// skipped, exactly like a hoisted constant (same single-def /
    /// def-first / runs-every-iteration conditions; the dst joins `hoisted`
    /// so it keeps a permanent home and no entry load).
    pub(crate) hoist_len_ips: Vec<usize>,
    /// W20 BOXREF — registers holding a BOXED HEAP VALUE inside a register-tier
    /// region. They get NO home at all: the value lives in the interpreter frame
    /// slot `[rbx + dreg(r)]`, which every def writes and no exit flushes, so the
    /// slot is AUTHORITATIVE on every path (the `emit_recv_slot_store` /
    /// `split_recvs` discipline, and the reason this needs neither a new `Home`
    /// variant nor a callee-saved gpr).
    ///
    /// A member is defined ONLY by an admitted dense-Array `GetIndex` whose
    /// elements are objects (`ARR_PIN_KIND`) and used ONLY as the receiver of an
    /// admitted `GetProp` (`getprop_ips`). That closed def/use shape is what makes
    /// the no-home choice sound and cheap: one store at the def, one load at the
    /// use, both L1, and nothing numeric ever touches the register.
    ///
    /// Empty under `ZIPP_NO_BOX_HOME=1`, and empty for every non-regalloc plan.
    pub(crate) box_regs: FxHashSet<u16>,
    /// W20 — region ips of `GetProp`s the REGALLOC emitter will serve with its own
    /// inline-cache probe (`emit_regalloc_ic_probe`) instead of declining the
    /// region to the memory tier. The dst is a `VTy::Num` home under a tag guard
    /// on the probe result. Empty unless BOXREF or the read-only-receiver arm
    /// admitted this region.
    pub(crate) getprop_ips: FxHashSet<usize>,
}

/// First xmm index usable as a value home (xmm0/xmm1 are scratch for the few ops
/// that need a temporary). xmm2..=xmm15 ⇒ 14 numeric homes.
pub(crate) const HOME_XMM_FIRST: u8 = 2;
pub(crate) const HOME_XMM_LAST: u8 = 15;
/// Gpr pool for boolean homes (r8..r11, all volatile; the region issues no calls
/// in its body so they survive). 4 simultaneous bools.
///
/// THE REGISTER CONTRACT. These four registers are PLANNER-OWNED for the whole
/// life of a compiled region. Between the prologue and any exit they may hold
///   * a `Bool` register home (every tier — `RegionPlan::bool_regs`), filled at
///     its in-region def or by `emit_bool_entry_load`, read back only by
///     `flush_exit`;
///   * a `gpr_const` mirror of a hoisted compare constant (the xmm INT tier),
///     filled ONCE in the prologue and read by `emit_icmp_flags` in the body;
///   * a numeric i64 home (the INT-GPR tier hands out whichever of these the
///     bools left free — see `gpr_home_map`).
/// NOTHING reloads any of them per iteration, so a body arm that scratches one
/// silently corrupts a live JS value until the region exits, across the
/// backedge, for every later iteration.
///
/// Therefore: **a region emitter's body may scratch rax/rcx/rdx and xmm0/xmm1
/// and nothing else** — never r8..r11, and never the pinned rbx/rsi/rdi/r12
/// (and r13/r14 where the i53 guard constants live). This is the invariant, and
/// it holds for entry code too: no helper reachable from a region prologue or
/// body scratches a `BOOL_GPRS` register any more, which is why the bool entry
/// loads no longer have to run last to be correct.
///
/// It has been violated three times, each time as a silent wrong answer:
/// W14 (the xmm INT tier's dense-Array tag check, `region_int.rs`), and W16's
/// two — the DOUBLE tier's `Bitwise` INT64_MIN sentinel (`regalloc.rs`) and the
/// `emit_box_to_home` tag check that the same tier's dense-Array read calls
/// (`emit.rs`). `tests/bool_home_clobber.rs` is the mechanical guard: it sweeps
/// one..four live bools across each tier so every one of these four registers
/// is occupied in turn while the body ops run.
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
        XmmAlloc {
            next: HOME_XMM_FIRST,
            active: Vec::new(),
            free: Vec::new(),
        }
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

/// Multi-segment home allocator for the glob-range mechanism: every value
/// brings a SET of disjoint live segments (a narrowed global or an unsplit
/// value brings one; a mixed-role temp brings one per def-range), and all of a
/// value's segments must land in ONE home — `reg_home`/`glob_home` are
/// per-value, and `flush_exit` reads one location per value. That same-home
/// constraint makes the graph non-interval (greedy start-order first-fit is no
/// longer optimal — measured one home over on the swizzle outer region), so
/// this searches: values in ascending first-segment start, lowest feasible
/// home first, with bounded backtracking, trying k homes from the sweep lower
/// bound upward. Segment overlap is inclusive at both ends — two touches at
/// the SAME ip always conflict, exactly like `XmmAlloc`'s strict `end < start`
/// freeing (an emitter arm may read an operand home after writing its dst's).
///
/// Returns one xmm home per value (in input order), or `None` when the values
/// do not fit the pool within the node budget — the caller then declines
/// exactly as an `XmmAlloc` exhaustion. Deterministic: fixed value order,
/// fixed home order, fixed budget.
pub(crate) fn alloc_value_homes(values: &[(Vec<(usize, usize)>, NumVal)]) -> Option<Vec<u8>> {
    const POOL: usize = (HOME_XMM_LAST - HOME_XMM_FIRST + 1) as usize;
    debug_assert!(values.windows(2).all(|w| w[0].0[0].0 <= w[1].0[0].0));
    // Lower bound: the max number of values simultaneously live at one ip.
    let mut events: Vec<(usize, i32)> = Vec::new();
    for (segs, _) in values {
        debug_assert!(segs.windows(2).all(|w| w[0].1 < w[1].0));
        for &(a, b) in segs {
            debug_assert!(a <= b);
            events.push((a, 1));
            events.push((b + 1, -1));
        }
    }
    events.sort_unstable();
    let (mut live, mut lb) = (0i32, 0i32);
    for &(_, d) in &events {
        live += d;
        lb = lb.max(live);
    }
    fn overlaps(segs: &[(usize, usize)], taken: &[(usize, usize)]) -> bool {
        segs.iter()
            .any(|&(a, b)| taken.iter().any(|&(c, d)| a <= d && c <= b))
    }
    // DFS over values in order, lowest feasible home first. All empty homes
    // are interchangeable, so only the LOWEST-indexed empty one is ever tried
    // (canonical form — any solution permutes into it), which is what keeps
    // the search from re-deriving the same partition k! times.
    fn fit(
        values: &[(Vec<(usize, usize)>, NumVal)],
        homes: &mut [Vec<(usize, usize)>],
        out: &mut Vec<usize>,
        budget: &mut u32,
    ) -> bool {
        let i = out.len();
        if i == values.len() {
            return true;
        }
        let mut seen_empty = false;
        for h in 0..homes.len() {
            if *budget == 0 {
                return false;
            }
            *budget -= 1;
            if homes[h].is_empty() {
                if seen_empty {
                    continue;
                }
                seen_empty = true;
            } else if overlaps(&values[i].0, &homes[h]) {
                continue;
            }
            let before = homes[h].len();
            homes[h].extend(values[i].0.iter().copied());
            out.push(h);
            if fit(values, homes, out, budget) {
                return true;
            }
            out.pop();
            homes[h].truncate(before);
        }
        false
    }
    for k in (lb.max(1) as usize)..=POOL {
        let mut homes: Vec<Vec<(usize, usize)>> = vec![Vec::new(); k];
        let mut choice: Vec<usize> = Vec::with_capacity(values.len());
        let mut budget = 200_000u32; // per pool size; spent ⇒ try a bigger pool
        if fit(values, &mut homes, &mut choice, &mut budget) {
            return Some(choice.iter().map(|&h| HOME_XMM_FIRST + h as u8).collect());
        }
    }
    None
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

/// B94. Prove that a candidate SPLIT receiver `r` never has a numeric operand
/// read its xmm home before a numeric def has filled it. Returns false when that
/// cannot be proved, and the caller then declines exactly as before.
///
/// The check is needed because `r` is deliberately NOT entry-loaded
/// (`live_in_regs` skips it: its memory slot holds the receiver OBJECT, so an
/// entry load would hand `emit_box_to_home` a non-number and bail on every OSR
/// entry). Its home therefore starts as GARBAGE, and a numeric use reaching it
/// first would silently compute a wrong answer -- the failure mode that must not
/// survive a green gate, so this is a whole-region veto, not a per-site
/// fallback.
///
/// The transfer function is per-instruction:
///   * `LoadGlobal r`  → home INVALID after (the object went to memory instead)
///   * any other def of `r` → home VALID after (a number was written to it)
///   * anything else   → unchanged
///
/// The meet is AND (a join is valid only if EVERY predecessor is valid), and
/// region entry is INVALID for the reason above.
///
/// Receiver uses need no condition: memory is authoritative for `r` throughout
/// (its `LoadGlobal` stores, every numeric def writes through, `flush_exit`
/// skips it), and every pinned access re-checks receiver identity anyway. The
/// `LoadGlobal` ip is NOT a numeric def, so nothing may write through there —
/// its own store is what makes memory current (`emit::wt_def_at`).
pub(crate) fn split_home_provably_safe(
    code: &[Instr],
    s: usize,
    e: usize,
    r: u16,
    cold: &FxHashSet<usize>,
    recv_lg_ips: &FxHashSet<usize>,
    recv_use_at: &dyn Fn(usize) -> Option<u16>,
) -> bool {
    let n = e - s + 1;
    // Successors of each region ip, as region-relative indices.
    let succ = |i: usize| -> Vec<usize> {
        let ip = s + i;
        let mut v = Vec::new();
        let (target, falls) = match code[ip] {
            Instr::Jump { target } => (Some(target as usize), false),
            Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => (Some(target as usize), true),
            _ => (None, true),
        };
        if let Some(t) = target {
            if t >= s && t <= e {
                v.push(t - s);
            }
        }
        if falls && ip < e {
            v.push(i + 1);
        }
        v
    };
    // valid_in[i]: does the home hold r's value on entry to region ip s+i?
    let mut valid_in = vec![false; n];
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        for t in succ(i) {
            preds[t].push(i);
        }
    }
    // Transfer: what a block leaves behind, given its input.
    let out_of = |i: usize, vin: bool| -> bool {
        if cold.contains(&(s + i)) {
            return vin;
        }
        // The RECEIVER LoadGlobal puts the object in memory and leaves the home
        // holding the previous iteration's number, which is no longer this
        // register's value: the home is invalid from here until a numeric def.
        // (Keying this on "any LoadGlobal of r" was wrong -- the same recycled
        // register is also loaded from OTHER globals, and those ARE numeric defs.)
        if recv_lg_ips.contains(&(s + i)) {
            return false;
        }
        match writes_reg(&code[s + i]) {
            Some(d) if d == r => true,
            _ => vin,
        }
    };
    // Fixed point. Entry (i == 0) is pinned INVALID; it has no home yet.
    for _ in 0..(n + 2) {
        let mut changed = false;
        for i in 0..n {
            let nv = if i == 0 || preds[i].is_empty() {
                false
            } else {
                preds[i].iter().all(|&p| out_of(p, valid_in[p]))
            };
            if nv != valid_in[i] {
                valid_in[i] = nv;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Veto if ANY use of r reads a home that is not provably live there. Every
    // use except the pinned-receiver one (which reads memory or the pin's global,
    // never the home) goes through the home, so the check covers `StoreGlobal`
    // and `Move` sources as well as arithmetic operands.
    for i in 0..n {
        let ip = s + i;
        if cold.contains(&ip) {
            continue;
        }
        let recv = recv_use_at(ip);
        for u in instr_uses(&code[ip]) {
            if u == r && Some(u) != recv && !valid_in[i] {
                return false;
            }
        }
    }
    true
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
        if let Instr::StoreGlobal { idx, .. }
        | Instr::StoreGlobalStrict { idx, .. }
        | Instr::StoreGlobalResolved { idx, .. } = code[ip]
        {
            g_stores.entry(idx).or_default().push(ip);
        }
    }

    let mut alias: FxHashMap<u16, u32> = FxHashMap::default();
    'cand: for (&r, def_ips) in &defs {
        if ty.get(&r) != Some(&VTy::Num)
            || first_seen.get(&r) != Some(&true) // live-in regs keep their own home
            || dead.contains(&r)
            || hoisted.contains(&r)
        // hoisted consts materialise in the prologue
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
                        | Some(&Instr::StoreGlobalResolved { idx, src })
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
