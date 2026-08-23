// Splice-aware INT admission: flatten proven-splice leaf callees into a virtual
// region body so the integer emitters see arithmetic where the raw bytecode has
// a `Call`. The MEM path splices at emit time (one guard per call site, per
// execution); here the splice happens BEFORE `int_unadmitted_ips` and
// `plan_region` run, so the callee's ops get i64 homes like any other region op
// and the identity guard hoists to region entry.
#![allow(unused_imports)]
use super::*;

/// What the INT emitters need on top of a plain region plan when the body they
/// were handed is a FLATTENED one. All-empty (`Default`) is the unspliced case
/// and emits byte-for-byte what the pre-splice emitters did.
#[derive(Default)]
pub(crate) struct IntEntry<'a> {
    /// `resume[vip - start]` = the ORIGINAL ip the interpreter must resume at
    /// when the region exits BEFORE executing `vip`. One entry past the end, so
    /// the i53 guards' "resume AFTER this op" (`vip + 1`) is always in range.
    /// EMPTY ⇒ the code is the proto's own and `vip` IS the resume ip.
    pub(crate) resume: &'a [u32],
    /// One `(absolute address of `global_gens[g]`, baked generation)` per
    /// spliced callee slot, compared ONCE at region entry instead of per call.
    /// Sound because `slot_guard` keying admits only slots NO bytecode store
    /// can reach, and an INT region runs no call, no allocation and no property
    /// write — so nothing between entry and exit can bump the generation.
    pub(crate) guards: &'a [(u64, u32)],
    /// `(jit_regs_fits address, highest scratch slot used above rbx)`. The
    /// flattened body writes the callee window above the caller frame, and
    /// `flush_exit` stores every home back to `[rbx + dreg(r)]`; a `0` address
    /// means no carved window and no check.
    pub(crate) regs_fits: (usize, u64),
}

/// A flattened region: a synthetic proto whose `[start, end]` IS the spliced
/// body, plus everything the emitters need to run it (see [`IntEntry`]).
pub(crate) struct IntSplice {
    pub(crate) proto: FuncProto,
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) ta_plan: TaPinPlan,
    pub(crate) resume: Vec<u32>,
    pub(crate) guards: Vec<(u64, u32)>,
    pub(crate) regs_fits: (usize, u64),
}

impl IntSplice {
    pub(crate) fn entry(&self) -> IntEntry<'_> {
        IntEntry {
            resume: &self.resume,
            guards: &self.guards,
            regs_fits: self.regs_fits,
        }
    }
}

/// One spliced call site, after every admission check passed.
struct Site<'a> {
    call_ip: usize,
    /// The `LoadGlobal` that defines the callee register. DROPPED from the
    /// flattened body (its global holds a function, which has no i64 home);
    /// every exit inside `[def_ip, call_ip]` resumes HERE so the interpreter
    /// re-loads it before re-running the call.
    def_ip: usize,
    dst: u16,
    arg_base: u16,
    /// Base of THIS site's scratch window. Each site gets its own slice above
    /// the caller frame rather than sharing `LeafInlinePlan::reg_window`: the
    /// memory tier's sites are separate emissions, but here they are one
    /// straight-line body, and one shared window would give every callee local
    /// a live range spanning the whole region — 5 more permanent homes on the
    /// mix loop, which overflows the pool before any emitter runs.
    win: u16,
    plan: &'a LeafInlinePlan,
}

/// Cap on the highest scratch slot the flattened windows may reach. The entry
/// check makes an over-large window a deopt rather than a fault, but a region
/// that can never fit is better declined at compile time.
const MAX_SPLICE_WINDOW_TOP: u64 = 512;

fn log_decline(args: std::fmt::Arguments<'_>) {
    if std::env::var_os("ZIPP_JITDECLINE").is_some() {
        eprintln!("[int-splice] {args}");
    }
}

macro_rules! decline {
    ($($arg:tt)*) => {{
        log_decline(format_args!($($arg)*));
        return None;
    }};
}

/// Plan a flattened INT body for `[start, end]`, or `None` to compile the
/// region exactly as before.
///
/// The result is all-or-nothing per region: a `Call` that cannot be spliced
/// leaves an op the INT emitters have no arm for, so one un-flattenable site
/// declines the whole region back to the memory path.
pub(crate) fn plan_int_splice(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
    regs_fits_helper: usize,
    metered: bool,
) -> Option<IntSplice> {
    if !int_splice_enabled() {
        return None;
    }
    let (s, e) = (start as usize, end as usize);
    if e <= s || e >= proto.code.len() {
        return None;
    }
    let calls: Vec<usize> = (s..=e)
        .filter(|&ip| matches!(proto.code[ip], Instr::Call { .. }))
        .collect();
    if calls.is_empty() {
        return None; // nothing to flatten — the untouched path stays untouched
    }
    if metered {
        // A metered VM charges each basic block its exact instruction count.
        // The flattened body has a different count than the bytecode the
        // interpreter would run, so the step budget would diverge.
        decline!("[{start},{end}] metered VM");
    }

    // ── per-site admission ──
    let mut sites: Vec<Site> = Vec::with_capacity(calls.len());
    let mut win_top = 0u64; // highest scratch slot used, as a caller reg number
    let mut guards: Vec<(u64, u32)> = Vec::new();
    let mut prev_end = s; // spans must not overlap
    for &c in &calls {
        let Instr::Call {
            dst,
            callee,
            arg_base,
            argc,
        } = proto.code[c]
        else {
            unreachable!()
        };
        let Some(lp) = leaf_plan.get(&c) else {
            decline!("@{c} no leaf plan (not monomorphic / not inline-eligible)");
        };
        if !lp.upvals.is_empty() {
            // An upvalue read is `jit_cell_get`, an FFI call the INT emitters
            // have no arm for.
            decline!("@{c} callee reads upvalues");
        }
        if !lp.nested.is_empty() {
            decline!("@{c} nested (wrapper) splice");
        }
        if argc != lp.param_count {
            decline!("@{c} argc {argc} != param_count {}", lp.param_count);
        }
        if lp.uninit_mask != 0 {
            // A local read before it is written would read `undefined` (and reg
            // 0 is `this`); neither has an i64 home.
            decline!(
                "@{c} body reads an uninitialized local (mask {:#x})",
                lp.uninit_mask
            );
        }
        let Some(guard) = lp.slot_guard else {
            // Without the slot-generation proof there is nothing to hoist: the
            // per-execution bits+version guard reads the callee REGISTER, whose
            // defining LoadGlobal this flatten drops.
            decline!("@{c} no slot_guard (callee identity not hoistable)");
        };
        if lp.body.iter().any(|i| {
            matches!(
                i,
                Instr::Jump { .. }
                    | Instr::JumpIfFalse { .. }
                    | Instr::JumpIfTrue { .. }
                    | Instr::JumpIfNotLt { .. }
                    | Instr::JumpIfNotLe { .. }
            )
        }) {
            decline!("@{c} branchy body");
        }
        // The nearest def of the callee register must be the `LoadGlobal` the
        // slot guard was keyed on (re-derived here: the plan carries the baked
        // generation, not the ip).
        let mut def_ip = None;
        for j in (s..c).rev() {
            match splice_ud(&proto.code[j]) {
                None => decline!("@{c} unmodelled op @{j} in the callee-def scan"),
                Some((_, Some(d))) if d == callee => {
                    def_ip = Some(j);
                    break;
                }
                Some(_) => {}
            }
        }
        let Some(def_ip) = def_ip else {
            decline!("@{c} callee register r{callee} is a region live-in");
        };
        if !matches!(proto.code[def_ip], Instr::LoadGlobal { dst: d, .. } if d == callee) {
            decline!("@{c} callee def @{def_ip} is not a LoadGlobal");
        }
        if def_ip <= prev_end && !sites.is_empty() {
            decline!("@{c} callee-def span overlaps the previous site");
        }
        span_is_replayable(proto, def_ip, c, s, e)?;
        prev_end = c;
        let win = win_top.max(lp.reg_window as u64);
        win_top = win + lp.callee_reg_count as u64;
        if win_top > MAX_SPLICE_WINDOW_TOP {
            decline!("@{c} scratch windows exceed the register-file headroom");
        }
        if !guards.contains(&guard) {
            guards.push(guard);
        }
        sites.push(Site {
            call_ip: c,
            def_ip,
            dst,
            arg_base,
            win: win as u16,
            plan: lp,
        });
    }

    // ── liveness over the TRANSFORMED region ──
    // The flatten drops the callee-def `LoadGlobal` and (for a `ReturnUndefined`
    // callee) the `Call`'s dst write; both are only sound if the register is
    // dead from that point on. Exits inside a `[def_ip, call_ip]` span resume at
    // `def_ip`, where the interpreter re-runs both — so the span itself imposes
    // no liveness obligation.
    let reads_outside = reads_outside_region(proto, s, e)?;
    let live_out = region_live_out(proto, s, e, &sites, &reads_outside)?;
    for site in &sites {
        let Instr::LoadGlobal { dst: callee, .. } = proto.code[site.def_ip] else {
            unreachable!()
        };
        if live_out[site.def_ip - s].contains(&callee) {
            decline!(
                "@{} callee register r{callee} is live past its def",
                site.call_ip
            );
        }
        if matches!(site.plan.body.last(), Some(Instr::ReturnUndefined))
            && live_out[site.call_ip - s].contains(&site.dst)
        {
            decline!("@{} undefined result r{} is live", site.call_ip, site.dst);
        }
    }

    // ── build the flattened body ──
    // The original region is BLANKED in place and the flat body APPENDED, so
    // every ip outside `[start, end]` keeps its number: an exit stub's resume ip
    // and the enclosing function's register use-sets (`read_outside`) are read
    // straight out of this proto and must stay the interpreter's.
    let mut code = proto.code.clone();
    let mut constants = proto.constants.clone();
    let base = code.len();
    for ins in &mut code[s..=e] {
        *ins = Instr::ReturnUndefined;
    }
    let mut flat: Vec<Instr> = Vec::new();
    let mut resume: Vec<u32> = Vec::new();
    let mut vip_of: FxHashMap<usize, usize> = FxHashMap::default();
    let mut access: FxHashMap<usize, u8> = FxHashMap::default();
    let mut site_at: FxHashMap<usize, &Site> = FxHashMap::default();
    let mut span_resume: FxHashMap<usize, u32> = FxHashMap::default();
    for site in &sites {
        site_at.insert(site.call_ip, site);
        for ip in site.def_ip..=site.call_ip {
            span_resume.insert(ip, site.def_ip as u32);
        }
    }
    for o in s..=e {
        vip_of.insert(o, flat.len());
        let r = span_resume.get(&o).copied().unwrap_or(o as u32);
        if let Some(site) = site_at.get(&o) {
            emit_splice(site, &mut flat, &mut resume, &mut constants, r)?;
            continue;
        }
        if sites.iter().any(|st| st.def_ip == o) {
            continue; // the callee LoadGlobal — dropped, replayed on any exit
        }
        if let Some(&j) = ta_plan.access.get(&o) {
            access.insert(base + flat.len(), j);
        }
        flat.push(proto.code[o].clone());
        resume.push(r);
    }
    // One entry past the body: the i53 guards resume "after this op", and the
    // last op is the back edge, which has no guard — mirror it rather than
    // inventing an ip.
    resume.push(*resume.last()?);
    // Jump targets: inside the region they move with the body, outside they are
    // interpreter ips and must not.
    for ins in &mut flat {
        let t = match ins {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target,
            _ => continue,
        };
        if (s..=e).contains(&(*t as usize)) {
            *t = (base + vip_of[&(*t as usize)]) as u32;
        }
    }
    let new_start = base as u32;
    let new_end = (base + flat.len() - 1) as u32;
    code.extend(flat);
    // Pins keyed outside the region stay put; the blanked ops are never queried.
    for (&ip, &j) in &ta_plan.access {
        if !(s..=e).contains(&ip) {
            access.insert(ip, j);
        }
    }
    let new_ta = TaPinPlan {
        pins: ta_plan.pins.clone(),
        access,
    };
    let mut sp = proto.clone();
    sp.reg_count = sp.reg_count.max(win_top as u16);
    sp.code = code;
    sp.constants = constants;
    if !int_unadmitted_ips(&sp, new_start, new_end, &new_ta, false).is_some_and(|v| v.is_empty()) {
        decline!("[{start},{end}] flattened body is not INT-admissible");
    }
    if std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] INT splice [{start},{end}]: {} call(s) flattened, {} entry guard(s), {} ops",
            sites.len(),
            guards.len(),
            new_end - new_start + 1
        );
    }
    Some(IntSplice {
        proto: sp,
        start: new_start,
        end: new_end,
        ta_plan: new_ta,
        resume,
        guards,
        regs_fits: (regs_fits_helper, win_top),
    })
}

/// Append one call site's spliced body. `r` is the resume ip every op of it
/// carries (the site's `def_ip` — see [`Site::def_ip`]).
fn emit_splice(
    site: &Site,
    flat: &mut Vec<Instr>,
    resume: &mut Vec<u32>,
    constants: &mut Vec<Value>,
    r: u32,
) -> Option<()> {
    let lp = site.plan;
    let w = site.win;
    let n = lp.param_count;
    let alias = |i: u16| lp.alias_params >> i & 1 == 1;
    // Callee reg 0 is `this` and 1..=n the params; `uninit_mask == 0` proved no
    // other local is read before it is written, so nothing else needs seeding.
    let map = |reg: u16| -> u16 {
        if reg >= 1 && reg <= n && alias(reg - 1) {
            site.arg_base + (reg - 1)
        } else {
            w + reg
        }
    };
    for i in 0..n {
        if !alias(i) {
            flat.push(Instr::Move {
                dst: w + 1 + i,
                src: site.arg_base + i,
            });
            resume.push(r);
        }
    }
    let (body, term) = lp.body.split_at(lp.body.len() - 1);
    // Effect ordering: `callee_leaf_ok` proved no DEOPT-CAPABLE op follows a
    // `StoreGlobal`, but its notion of deopt-capable is the memory emitter's.
    // On the INT tier a `StoreGlobal` is only a home move — the global reaches
    // memory at `flush_exit` — so an exit after it would flush the store AND
    // resume at `def_ip`, re-running the whole call and applying it twice.
    // Re-check with THIS tier's exit set.
    let effect_at = body.iter().position(|i| {
        matches!(
            i,
            Instr::StoreGlobal { .. }
                | Instr::StoreGlobalStrict { .. }
                | Instr::StoreGlobalResolved { .. }
        )
    });
    if let Some(k) = effect_at {
        if body[k + 1..].iter().any(int_op_can_exit) {
            decline!("@{} body can exit after a StoreGlobal", site.call_ip);
        }
    }
    for ins in body {
        let Some(m) = map_body_instr(ins, &map, constants, lp) else {
            decline!("@{} body op {ins:?} is not flattenable", site.call_ip);
        };
        flat.push(m);
        resume.push(r);
    }
    match term[0] {
        // The dst is proved dead above; `undefined` has no i64 home, so the
        // write is simply not made.
        Instr::ReturnUndefined => {}
        Instr::Return { src } => {
            flat.push(Instr::Move {
                dst: site.dst,
                src: map(src),
            });
            resume.push(r);
        }
        _ => decline!("@{} body terminator is not a Return", site.call_ip),
    }
    Some(())
}

/// Rewrite one callee-body op into the caller's register space. Restricted to
/// the ops the INT emitters host; anything else declines rather than being
/// mis-shifted (a missed operand is a wrong-register read).
fn map_body_instr(
    i: &Instr,
    map: &impl Fn(u16) -> u16,
    constants: &mut Vec<Value>,
    lp: &LeafInlinePlan,
) -> Option<Instr> {
    let m = map;
    Some(match *i {
        Instr::LoadInt { dst, val } => Instr::LoadInt { dst: m(dst), val },
        Instr::LoadConst { dst, idx } => {
            // The body's pool index is the CALLEE's; the plan pre-resolved the
            // bits, so re-intern the value in the synthetic proto's pool.
            let v = Value::from_bits(*lp.consts.get(&idx)?);
            if !v.is_int() {
                return None; // a double constant cannot be an i64 home
            }
            constants.push(v);
            Instr::LoadConst {
                dst: m(dst),
                idx: (constants.len() - 1) as u32,
            }
        }
        Instr::Move { dst, src } => Instr::Move {
            dst: m(dst),
            src: m(src),
        },
        Instr::LoadGlobal { dst, idx } => Instr::LoadGlobal { dst: m(dst), idx },
        Instr::StoreGlobal { idx, src } => Instr::StoreGlobal { idx, src: m(src) },
        Instr::StoreGlobalStrict { idx, src } => Instr::StoreGlobalStrict { idx, src: m(src) },
        Instr::StoreGlobalResolved { idx, src } => Instr::StoreGlobalResolved { idx, src: m(src) },
        Instr::Add { dst, a, b } => Instr::Add {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::Sub { dst, a, b } => Instr::Sub {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::Mul { dst, a, b } => Instr::Mul {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::Mod { dst, a, b } => Instr::Mod {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::AddInt { dst, a, imm, upd } => Instr::AddInt {
            dst: m(dst),
            a: m(a),
            imm,
            upd,
        },
        Instr::Neg { dst, a } => Instr::Neg {
            dst: m(dst),
            a: m(a),
        },
        Instr::Bitwise { dst, a, b, op } => Instr::Bitwise {
            dst: m(dst),
            a: m(a),
            b: m(b),
            op,
        },
        Instr::Eq { dst, a, b } => Instr::Eq {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::Ne { dst, a, b } => Instr::Ne {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::Lt { dst, a, b } => Instr::Lt {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::Le { dst, a, b } => Instr::Le {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::Gt { dst, a, b } => Instr::Gt {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::Ge { dst, a, b } => Instr::Ge {
            dst: m(dst),
            a: m(a),
            b: m(b),
        },
        Instr::MathOp {
            dst,
            op: MathFn::Imul,
            arg_base,
            argc: 2,
        } => Instr::MathOp {
            dst: m(dst),
            op: MathFn::Imul,
            arg_base: m(arg_base),
            argc: 2,
        },
        _ => return None,
    })
}

/// Can this op take a SIDE EXIT on the integer tier? The arithmetic ops carry a
/// ±2^53 range guard and `Mod` a divide check; a pinned element/string access
/// guards its snapshot. Everything else in the flattenable subset is a home
/// move, an int32-lane op or a compare, none of which can leave the region.
fn int_op_can_exit(i: &Instr) -> bool {
    !matches!(
        i,
        Instr::Move { .. }
            | Instr::LoadInt { .. }
            | Instr::LoadConst { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. }
            | Instr::Bitwise { .. }
            | Instr::MathOp {
                op: MathFn::Imul,
                argc: 2,
                ..
            }
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
    )
}

/// Every exit inside `[def_ip, call_ip]` resumes at `def_ip`, so the whole span
/// runs again in the interpreter. Prove that is the same as not having run it:
/// the span must be effect-free, branch-free, un-enterable from elsewhere, and
/// must not clobber a register a later op of the span reads.
fn span_is_replayable(
    proto: &FuncProto,
    def_ip: usize,
    call_ip: usize,
    s: usize,
    e: usize,
) -> Option<()> {
    let mut written: FxHashSet<u16> = FxHashSet::default();
    let mut all_written: FxHashSet<u16> = FxHashSet::default();
    for ip in def_ip..call_ip {
        match proto.code[ip] {
            Instr::StoreGlobal { .. }
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. }
            | Instr::SetIndex { .. }
            | Instr::SetProp { .. } => {
                decline!("@{call_ip} callee-def span writes at @{ip}");
            }
            Instr::Jump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotLe { .. } => {
                decline!("@{call_ip} callee-def span branches at @{ip}");
            }
            _ => {}
        }
        if let Some((_, Some(d))) = splice_ud(&proto.code[ip]) {
            all_written.insert(d);
        }
    }
    for ip in def_ip..=call_ip {
        let Some((uses, def)) = splice_ud(&proto.code[ip]) else {
            decline!("@{call_ip} unmodelled op @{ip} in the callee-def span");
        };
        for u in uses {
            if !written.contains(&u) && all_written.contains(&u) {
                decline!("@{call_ip} span @{ip} reads r{u} across a span write");
            }
        }
        if let Some(d) = def {
            written.insert(d);
        }
    }
    // Control must not enter the span anywhere but at `def_ip`, or an
    // interpreter that jumped past the callee load would find it never ran.
    // Whole-function, not just the region: `slot_guard` keying already proved
    // this range gap-free, and re-deriving it here keeps the replay argument
    // self-contained rather than borrowed.
    let _ = (s, e);
    for ins in &proto.code {
        if let Some(t) = jump_target(ins) {
            if (def_ip + 1..=call_ip).contains(&(t as usize)) {
                decline!("@{call_ip} a jump enters the callee-def span at @{t}");
            }
        }
    }
    Some(())
}

fn jump_target(i: &Instr) -> Option<u32> {
    match *i {
        Instr::Jump { target }
        | Instr::JumpIfFalse { target, .. }
        | Instr::JumpIfTrue { target, .. }
        | Instr::JumpIfNotLt { target, .. }
        | Instr::JumpIfNotLe { target, .. } => Some(target),
        _ => None,
    }
}

/// Registers read ANYWHERE outside `[s, e]` in the enclosing function — the
/// live-out set every region exit inherits. `None` for an unmodelled op: a
/// missed read would make a live register look dead, which is exactly the
/// mistake this whole analysis exists to avoid.
fn reads_outside_region(proto: &FuncProto, s: usize, e: usize) -> Option<FxHashSet<u16>> {
    let mut set: FxHashSet<u16> = FxHashSet::default();
    for (ip, ins) in proto.code.iter().enumerate() {
        if (s..=e).contains(&ip) {
            continue;
        }
        let Some((uses, _)) = splice_ud(ins) else {
            log_decline(format_args!(
                "unmodelled op @{ip} outside the region: {ins:?}"
            ));
            return None;
        };
        set.extend(uses);
    }
    Some(set)
}

/// Backward liveness over the region, on the code as the SPLICE will leave it:
/// a spliced `Call` no longer reads its callee register, and no longer defines
/// its dst when the callee returns `undefined`; the callee-def `LoadGlobal` is
/// gone. Indexed by `ip - s`; entry `k` is the set live after `s + k` runs.
///
/// An out-of-region jump target contributes `reads_outside` (the conservative
/// set every exit stub hands back). A deopt exit needs no edge of its own: it
/// resumes at an ip the analysis already covers.
fn region_live_out(
    proto: &FuncProto,
    s: usize,
    e: usize,
    sites: &[Site],
    reads_outside: &FxHashSet<u16>,
) -> Option<Vec<FxHashSet<u16>>> {
    let n = e - s + 1;
    let mut ud: Vec<(Vec<u16>, Option<u16>)> = Vec::with_capacity(n);
    for ip in s..=e {
        let mut t = splice_ud(&proto.code[ip])?;
        if let Some(site) = sites.iter().find(|st| st.call_ip == ip) {
            let Instr::Call { arg_base, argc, .. } = proto.code[ip] else {
                unreachable!()
            };
            t.0 = (0..argc).map(|k| arg_base + k).collect();
            t.1 = match site.plan.body.last() {
                Some(Instr::ReturnUndefined) => None,
                _ => Some(site.dst),
            };
        } else if sites.iter().any(|st| st.def_ip == ip) {
            t = (Vec::new(), None);
        }
        ud.push(t);
    }
    let mut live_out: Vec<FxHashSet<u16>> = vec![FxHashSet::default(); n];
    let mut live_in: Vec<FxHashSet<u16>> = vec![FxHashSet::default(); n];
    // Monotone (sets only grow), bounded by the register file — iterate to a
    // fixpoint over the region's small CFG.
    loop {
        let mut changed = false;
        for k in (0..n).rev() {
            let ip = s + k;
            let mut out: FxHashSet<u16> = FxHashSet::default();
            let mut fallthrough = true;
            if let Some(t) = jump_target(&proto.code[ip]) {
                if matches!(proto.code[ip], Instr::Jump { .. }) {
                    fallthrough = false;
                }
                let t = t as usize;
                if (s..=e).contains(&t) {
                    out.extend(live_in[t - s].iter().copied());
                } else {
                    out.extend(reads_outside.iter().copied());
                }
            }
            if matches!(
                proto.code[ip],
                Instr::Return { .. } | Instr::ReturnUndefined
            ) {
                fallthrough = false;
            }
            if fallthrough {
                if k + 1 < n {
                    out.extend(live_in[k + 1].iter().copied());
                } else {
                    out.extend(reads_outside.iter().copied());
                }
            }
            let mut inn = out.clone();
            if let Some(d) = ud[k].1 {
                inn.remove(&d);
            }
            inn.extend(ud[k].0.iter().copied());
            if inn != live_in[k] || out != live_out[k] {
                changed = true;
                live_in[k] = inn;
                live_out[k] = out;
            }
        }
        if !changed {
            break;
        }
    }
    Some(live_out)
}

/// Use/def table for the liveness and reaching-def scans. USES must be EXACT —
/// a missed one makes a live register look dead — so every op this table does
/// not model returns `None` and declines the flatten.
fn splice_ud(i: &Instr) -> Option<(Vec<u16>, Option<u16>)> {
    let r = |v: Vec<u16>, d: Option<u16>| (v, d);
    Some(match *i {
        Instr::LoadConst { dst, .. }
        | Instr::LoadInt { dst, .. }
        | Instr::LoadUndefined { dst }
        | Instr::LoadNull { dst }
        | Instr::LoadHole { dst }
        | Instr::LoadBool { dst, .. }
        | Instr::NewObject { dst, .. }
        | Instr::MakeFunc { dst, .. }
        | Instr::Now { dst, .. }
        | Instr::LoadNewTarget { dst }
        | Instr::LoadCallee { dst }
        | Instr::LoadClassValue { dst, .. }
        | Instr::LoadGlobal { dst, .. }
        | Instr::LoadGlobalOrUndefined { dst, .. } => r(vec![], Some(dst)),
        Instr::Move { dst, src } => r(vec![src], Some(dst)),
        Instr::StoreGlobal { src, .. }
        | Instr::StoreGlobalStrict { src, .. }
        | Instr::StoreGlobalResolved { src, .. } => r(vec![src], None),
        Instr::Add { dst, a, b }
        | Instr::Sub { dst, a, b }
        | Instr::Mul { dst, a, b }
        | Instr::Div { dst, a, b }
        | Instr::Mod { dst, a, b }
        | Instr::Pow { dst, a, b }
        | Instr::Bitwise { dst, a, b, .. }
        | Instr::StrConcat { dst, a, b }
        | Instr::StrConcatChain { dst, a, b }
        | Instr::StrAppendInPlace { dst, a, b }
        | Instr::Lt { dst, a, b }
        | Instr::Le { dst, a, b }
        | Instr::Gt { dst, a, b }
        | Instr::Ge { dst, a, b }
        | Instr::Eq { dst, a, b }
        | Instr::Ne { dst, a, b }
        | Instr::LooseEq { dst, a, b }
        | Instr::LooseNe { dst, a, b } => r(vec![a, b], Some(dst)),
        Instr::StrAppendIndex {
            dst, a, obj, key, ..
        } => r(vec![a, obj, key], Some(dst)),
        Instr::AddRightPair { dst, a, b, c, .. } => r(vec![a, b, c], Some(dst)),
        Instr::Pad2Concat { dst, src, .. } => r(vec![src], Some(dst)),
        Instr::Pad2Conditional { dst, src } => r(vec![src], Some(dst)),
        Instr::AddInt { dst, a, .. }
        | Instr::Neg { dst, a }
        | Instr::Not { dst, a }
        | Instr::BitNot { dst, a }
        | Instr::ToNum { dst, a }
        | Instr::ToStr { dst, a }
        | Instr::TypeOf { dst, a }
        | Instr::TypeOfIs { dst, a, .. }
        | Instr::IsArray { dst, a } => r(vec![a], Some(dst)),
        Instr::JsonParse { dst, a } => r(vec![a], Some(dst)),
        Instr::JsonStringify { dst, val, space } => r(vec![val, space], Some(dst)),
        Instr::HasProp { dst, key, obj, .. } => r(vec![key, obj], Some(dst)),
        Instr::InstanceOfDyn { dst, val, ctor } => r(vec![val, ctor], Some(dst)),
        Instr::ArrayRest { dst, src, .. } => r(vec![src], Some(dst)),
        Instr::ObjectSpread { target, src } => r(vec![target, src], None),
        Instr::RequireObject { val } => r(vec![val], None),
        Instr::CheckIterable { src } => r(vec![src], None),
        Instr::IterClose { iter } | Instr::IterCloseQuiet { iter } => r(vec![iter], None),
        Instr::CallSpread { dst, callee, args } => r(vec![callee, args], Some(dst)),
        Instr::CallMethodSpread { dst, obj, args, .. } => r(vec![obj, args], Some(dst)),
        Instr::TailCall {
            callee,
            arg_base,
            argc,
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(callee);
            r(v, None)
        }
        Instr::ArrayCtor {
            dst,
            arg_base,
            argc,
        } => r((0..argc).map(|k| arg_base + k).collect(), Some(dst)),
        Instr::GetIndex { dst, obj, key } => r(vec![obj, key], Some(dst)),
        Instr::GetIndexConcat { dst, obj, key, .. } => r(vec![obj, key], Some(dst)),
        Instr::SetIndex { obj, key, val } => r(vec![obj, key, val], None),
        Instr::SetIndexConcat { obj, key, val, .. } => r(vec![obj, key, val], None),
        Instr::GetProp { dst, obj, .. } => r(vec![obj], Some(dst)),
        Instr::SetProp { obj, val, .. } => r(vec![obj, val], None),
        Instr::ToPropKey { dst, obj, src } => r(vec![obj, src], Some(dst)),
        Instr::LenOf { dst, obj } => r(vec![obj], Some(dst)),
        Instr::UpvalGet { dst, .. } => r(vec![], Some(dst)),
        Instr::UpvalSet { src, .. } => r(vec![src], None),
        Instr::MathOp {
            dst,
            arg_base,
            argc,
            ..
        }
        | Instr::GlobalFn {
            dst,
            arg_base,
            argc,
            ..
        }
        | Instr::StaticFn {
            dst,
            arg_base,
            argc,
            ..
        }
        | Instr::NewArray {
            dst,
            arg_base,
            argc,
        } => r((0..argc).map(|k| arg_base + k).collect(), Some(dst)),
        Instr::Call {
            dst,
            callee,
            arg_base,
            argc,
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(callee);
            r(v, Some(dst))
        }
        Instr::New {
            dst,
            callee,
            arg_base,
            argc,
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(callee);
            r(v, Some(dst))
        }
        Instr::CallMethod {
            dst,
            obj,
            arg_base,
            argc,
            ..
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(obj);
            r(v, Some(dst))
        }
        Instr::Print { arg_base, argc, .. } => r((0..argc).map(|k| arg_base + k).collect(), None),
        Instr::ArrayAppend { arr, val, .. } => r(vec![arr, val], None),
        Instr::Jump { .. } | Instr::ReturnUndefined | Instr::GenStart => r(vec![], None),
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => r(vec![cond], None),
        Instr::JumpIfNotLt { a, b, .. } | Instr::JumpIfNotLe { a, b, .. } => r(vec![a, b], None),
        Instr::Return { src } | Instr::Throw { src } | Instr::ThisCheck { src } => {
            r(vec![src], None)
        }
        _ => return None,
    })
}

/// The two region-entry checks a flattened body needs, emitted by BOTH integer
/// emitters (they share the frame contract this relies on: `rbx` is the caller
/// window base, `rdi` the vm pointer, no home loaded yet, and `entry_bail`
/// restores the established frame).
///
/// `has_shadow` says the frame already keeps 32 bytes of shadow at the bottom
/// with `rsp` 16-aligned — true whenever the region pins a view, and always on
/// the GPR tier. Otherwise a shadow frame is carved for the one call.
pub(crate) fn emit_int_splice_entry_guards(
    ops: &mut dynasmrt::x64::Assembler,
    entry: &IntEntry<'_>,
    has_shadow: bool,
    entry_bail: dynasmrt::DynamicLabel,
) {
    if entry.regs_fits.0 != 0 {
        let pad: i32 = if has_shadow { 0 } else { 40 };
        if pad != 0 {
            dynasm!(ops ; sub rsp, pad);
        }
        dynasm!(ops
            ; mov rcx, rdi                              // vm
            ; mov rdx, rbx                              // caller window base
            ; mov r8, QWORD entry.regs_fits.1 as i64    // highest scratch slot
            ; mov rax, QWORD entry.regs_fits.0 as i64
            ; call rax
        );
        if pad != 0 {
            dynasm!(ops ; add rsp, pad);
        }
        dynasm!(ops ; test rax, rax ; jz => entry_bail);
    }
    for &(addr, gen) in entry.guards {
        dynasm!(ops
            ; mov rax, QWORD addr as i64
            ; cmp DWORD [rax], gen as i32
            ; jne => entry_bail
        );
    }
}
