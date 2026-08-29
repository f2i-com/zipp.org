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
    /// Address/value guards compared ONCE at region entry: one
    /// `global_gens[g]` generation per spliced callee slot.
    /// Sound because a flattened INT region runs no call, allocation, delete,
    /// or property write, so neither fact can change between entry and exit.
    pub(crate) guards: &'a [(u64, u32)],
    /// Expected VM global-route epoch when a flattened body performs raw
    /// global accesses. The emitter reads it relative to the live VM argument,
    /// so persistent code remains valid after an embedded `ScriptState` moves.
    pub(crate) route_epoch: Option<u32>,
    /// Read-only dense computed-call proofs run once before any region op.
    /// They validate exact receiver/element identities and ABA versions.
    pub(crate) computed_guards: &'a [DenseComputedEntryGuard],
    /// `(jit_regs_fits address, highest scratch slot used above rbx)`. The
    /// flattened body writes the callee window above the caller frame, and
    /// `flush_exit` stores every home back to `[rbx + dreg(r)]`; a `0` address
    /// means no carved window and no check.
    pub(crate) regs_fits: (usize, u64),
    /// First synthetic register above the caller's real frame. `None` for an
    /// ordinary region and the established Call splice; computed dispatch
    /// scratch is defs-before-use and is never observable at an interpreter
    /// resume, so integer emitters neither entry-load nor exit-flush it.
    pub(crate) scratch_base: Option<u16>,
}

impl IntEntry<'_> {
    #[inline]
    pub(crate) fn is_scratch(&self, reg: u16) -> bool {
        self.scratch_base.is_some_and(|base| reg >= base)
    }
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
    pub(crate) route_epoch: Option<u32>,
    pub(crate) computed_guards: Vec<DenseComputedEntryGuard>,
    pub(crate) regs_fits: (usize, u64),
    pub(crate) scratch_base: Option<u16>,
}

impl IntSplice {
    pub(crate) fn entry(&self) -> IntEntry<'_> {
        IntEntry {
            resume: &self.resume,
            guards: &self.guards,
            route_epoch: self.route_epoch,
            computed_guards: &self.computed_guards,
            regs_fits: self.regs_fits,
            scratch_base: self.scratch_base,
        }
    }
}

/// One native-entry validation bundle for a bounded dense computed-call site.
pub(crate) struct DenseComputedEntryGuard {
    pub(crate) recv_src: TaPinSrc,
    pub(crate) recv_bits: u64,
    pub(crate) recv_ver: u32,
    pub(crate) elements: Vec<(u64, u32)>,
    pub(crate) helper: usize,
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

/// The two bytecode shapes that preserve a computed method reference until
/// its arguments have been evaluated. New bytecode uses the capture-first
/// `GetIndex`/`CallWithThis` pair; the sealed legacy opcode remains accepted so
/// old/precompiled protos keep the same optimization contract.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ComputedCallSite {
    pub(crate) get_ip: Option<usize>,
    pub(crate) dst: u16,
    pub(crate) obj: u16,
    pub(crate) key: u16,
    pub(crate) callee: Option<u16>,
    pub(crate) arg_base: u16,
    pub(crate) argc: u16,
}

/// Recognize only the exact capture-first computed-call pair. The captured
/// callee, receiver, and key must survive argument evaluation unchanged, and
/// no control-flow edge may enter after the property read. These facts let the
/// computed splice omit both boxed operations while retaining the original
/// pair as its miss/replay target.
pub(crate) fn computed_call_site(
    proto: &FuncProto,
    region_start: usize,
    call_ip: usize,
) -> Option<ComputedCallSite> {
    match *proto.code.get(call_ip)? {
        Instr::CallMethodComputed {
            dst,
            obj,
            key,
            arg_base,
            argc,
        } => Some(ComputedCallSite {
            get_ip: None,
            dst,
            obj,
            key,
            callee: None,
            arg_base,
            argc,
        }),
        Instr::CallWithThis {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
        } => {
            let get_ip = (region_start..call_ip)
                .rev()
                .find(|&ip| writes_reg(&proto.code[ip]) == Some(callee))?;
            let Instr::GetIndex {
                dst: get_dst,
                obj,
                key,
            } = proto.code[get_ip]
            else {
                return None;
            };
            if get_dst != callee
                || obj != this_v
                || callee == this_v
                || callee == key
                || dst == callee
                || dst == this_v
                || dst == key
            {
                return None;
            }
            let arg_end = arg_base.checked_add(argc)?;
            if (arg_base..arg_end).contains(&callee)
                || (arg_base..arg_end).contains(&this_v)
                || (arg_base..arg_end).contains(&key)
            {
                return None;
            }
            if proto.code[get_ip + 1..call_ip]
                .iter()
                .any(|ins| writes_reg(ins).is_some_and(|r| r == callee || r == this_v || r == key))
            {
                return None;
            }
            if proto.code.iter().any(|ins| {
                bytecode_control_target(ins)
                    .is_some_and(|target| (get_ip + 1..=call_ip).contains(&(target as usize)))
            }) {
                return None;
            }
            Some(ComputedCallSite {
                get_ip: Some(get_ip),
                dst,
                obj,
                key,
                callee: Some(callee),
                arg_base,
                argc,
            })
        }
        _ => None,
    }
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
    computed_leaf_plan: &FxHashMap<usize, DenseComputedLeafPlan>,
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
    let computed: Vec<usize> = (s..=e)
        .filter(|&ip| computed_call_site(proto, s, ip).is_some())
        .collect();
    if !computed.is_empty() {
        // Keep the first lane deliberately bounded to one computed dispatch and
        // no ordinary Call splice. The transformed result is still generic over
        // every dense arm/key; larger mixed-call regions retain the existing MEM
        // helper until their cross-site liveness proof is equally explicit.
        if computed.len() == 1 && calls.is_empty() {
            return plan_int_computed_splice(
                proto,
                start,
                end,
                ta_plan,
                computed_leaf_plan,
                computed[0],
                regs_fits_helper,
                metered,
            );
        }
        decline!("[{start},{end}] mixed/multiple computed call sites");
    }
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
    let mut route_epoch: Option<u32> = None;
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
        span_is_replayable(proto, ta_plan, def_ip, c, s, e)?;
        prev_end = c;
        let win = win_top.max(lp.reg_window as u64);
        win_top = win + lp.callee_reg_count as u64;
        if win_top > MAX_SPLICE_WINDOW_TOP {
            decline!("@{c} scratch windows exceed the register-file headroom");
        }
        if !guards.contains(&guard) {
            guards.push(guard);
        }
        let body_has_direct_global = lp.body.iter().any(|ins| {
            matches!(
                ins,
                Instr::LoadGlobal { .. }
                    | Instr::LoadGlobalOrUndefined { .. }
                    | Instr::StoreGlobal { .. }
                    | Instr::StoreGlobalStrict { .. }
                    | Instr::StoreGlobalResolved { .. }
            )
        });
        if body_has_direct_global {
            // `emit_splice` deliberately discards the typed schedule and
            // flattens the leaf bytecode itself. Hoist the independent route
            // proof built for that exact body; coupling this to `typed_lane`
            // would incorrectly disable valid global-store leaves, because
            // their transactional typed schedule is intentionally narrower
            // than the established raw INT emitter.
            let Some(route_guard) = lp.direct_global_route_epoch else {
                decline!("@{c} direct globals lack a route-epoch guard");
            };
            if let Some(existing) = route_epoch {
                if existing != route_guard {
                    decline!("@{c} inconsistent route-epoch guards");
                }
            } else {
                route_epoch = Some(route_guard);
            }
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
    let mut string_constants = proto.string_constants.clone();
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
            emit_splice(
                site,
                &mut flat,
                &mut resume,
                &mut constants,
                &mut string_constants,
                r,
            )?;
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
    let mut captured_calls = FxHashMap::default();
    for (&ip, site) in &ta_plan.captured_calls {
        if !(s..=e).contains(&ip) {
            captured_calls.insert(ip, *site);
            continue;
        }
        let (Some(&get_off), Some(&call_off)) =
            (vip_of.get(&site.get_ip), vip_of.get(&site.call_ip))
        else {
            continue;
        };
        let mut mapped = *site;
        mapped.get_ip = base + get_off;
        mapped.call_ip = base + call_off;
        captured_calls.insert(mapped.call_ip, mapped);
    }
    let new_ta = TaPinPlan {
        pins: ta_plan.pins.clone(),
        access,
        captured_calls,
    };
    let mut sp = proto.clone();
    sp.reg_count = sp.reg_count.max(win_top as u16);
    sp.code = code;
    sp.constants = constants;
    sp.string_constants = string_constants;
    let admitted = int_unadmitted_ips(&sp, new_start, new_end, &new_ta, false);
    if admitted.as_ref().is_none_or(|v| !v.is_empty()) {
        if let Some(ips) = admitted {
            for ip in ips {
                log_decline(format_args!("flattened reject @{ip}: {:?}", sp.code[ip]));
            }
        }
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
        route_epoch,
        computed_guards: Vec::new(),
        regs_fits: (regs_fits_helper, win_top),
        scratch_base: None,
    })
}

/// Computed-call-only sibling of the ordinary Call splice. Every dense arm was
/// already proved a pure integer leaf from live state. This pass supplies the
/// remaining control proof: a bounded key dispatch, a replayable pre-call
/// prefix for every miss/bail, and entry guards that freeze the exact dense
/// receiver/elements for the duration of the call-free native loop.
#[allow(clippy::too_many_arguments)]
fn plan_int_computed_splice(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    computed_leaf_plan: &FxHashMap<usize, DenseComputedLeafPlan>,
    call_ip: usize,
    regs_fits_helper: usize,
    metered: bool,
) -> Option<IntSplice> {
    if !int_computed_leaf_enabled() {
        return None;
    }
    if metered {
        decline!("[{start},{end}] computed splice in metered VM");
    }
    let Some(cp) = computed_leaf_plan.get(&call_ip) else {
        decline!("@{call_ip} no dense computed leaf plan");
    };
    if cp.variants.is_empty() || cp.variants.len() > 4 {
        decline!("@{call_ip} computed arm count {}", cp.variants.len());
    }
    let Some(site) = computed_call_site(proto, start as usize, call_ip) else {
        return None;
    };
    let (dst, key, arg_base, argc) = (site.dst, site.key, site.arg_base, site.argc);
    let (s, e) = (start as usize, end as usize);
    let fallback = cp.fallback_ip as usize;
    if !(s..=call_ip).contains(&fallback) {
        decline!("@{call_ip} fallback @{fallback} outside pure region prefix");
    }
    span_is_replayable(proto, ta_plan, fallback, call_ip, s, e)?;

    // Two dispatch temporaries plus one SHARED callee window. Arms are mutually
    // exclusive and every success jumps to the common continuation, so no arm's
    // locals can be live into another; sharing avoids turning a small 3-way
    // helper table into dozens of artificial numeric registers.
    let key_const = proto.reg_count;
    let key_cmp = key_const.checked_add(1)?;
    let win = key_cmp.checked_add(1)?;
    let mut win_top = win as u64;
    let mut wins = Vec::with_capacity(cp.variants.len());
    let mut route_epoch = None;
    for lp in &cp.variants {
        if lp.upvals.len() != 0
            || !lp.nested.is_empty()
            || lp.typed_lane.is_none()
            || lp.param_count != argc
            || lp.uninit_mask != 0
            || !matches!(lp.body.last(), Some(Instr::Return { .. }))
        {
            decline!("@{call_ip} computed arm lost its pure typed proof");
        }
        if lp.body.iter().any(|ins| {
            matches!(
                ins,
                Instr::LoadGlobal { .. }
                    | Instr::LoadGlobalOrUndefined { .. }
                    | Instr::StoreGlobal { .. }
                    | Instr::StoreGlobalStrict { .. }
                    | Instr::StoreGlobalResolved { .. }
            )
        }) {
            let Some(epoch) = lp.direct_global_route_epoch else {
                decline!("@{call_ip} computed direct globals lack a route guard");
            };
            if route_epoch.is_some_and(|existing| existing != epoch) {
                decline!("@{call_ip} computed route guards disagree");
            }
            route_epoch = Some(epoch);
        }
        let arm_win = (win as u64).max(lp.reg_window as u64);
        win_top = win_top.max(arm_win.checked_add(lp.callee_reg_count as u64)?);
        if win_top > MAX_SPLICE_WINDOW_TOP {
            decline!("@{call_ip} computed windows exceed register headroom");
        }
        wins.push(arm_win as u16);
    }

    let mut code = proto.code.clone();
    let mut constants = proto.constants.clone();
    let mut string_constants = proto.string_constants.clone();
    let base = code.len();
    for ins in &mut code[s..=e] {
        *ins = Instr::ReturnUndefined;
    }
    let mut flat = Vec::new();
    let mut resume = Vec::new();
    let mut vip_of: FxHashMap<usize, usize> = FxHashMap::default();
    let mut access: FxHashMap<usize, u8> = FxHashMap::default();
    let mut fallback_fixup = None;

    for o in s..=e {
        vip_of.insert(o, flat.len());
        if cp.drop_obj_def.is_some_and(|d| d as usize == o) || site.get_ip == Some(o) {
            continue;
        }
        if o != call_ip {
            if let Some(&j) = ta_plan.access.get(&o) {
                access.insert(base + flat.len(), j);
            }
            flat.push(proto.code[o].clone());
            resume.push(o as u32);
            continue;
        }

        for (index, (lp, &win)) in cp.variants.iter().zip(&wins).enumerate() {
            flat.push(Instr::LoadInt {
                dst: key_const,
                val: index as i32,
            });
            resume.push(cp.fallback_ip);
            flat.push(Instr::Eq {
                dst: key_cmp,
                a: key,
                b: key_const,
            });
            resume.push(cp.fallback_ip);
            let miss_at = flat.len();
            flat.push(Instr::JumpIfFalse {
                cond: key_cmp,
                target: u32::MAX,
            });
            resume.push(cp.fallback_ip);

            let site = Site {
                call_ip,
                def_ip: fallback,
                dst,
                arg_base,
                win,
                plan: lp,
            };
            emit_splice(
                &site,
                &mut flat,
                &mut resume,
                &mut constants,
                &mut string_constants,
                cp.fallback_ip,
            )?;
            // Success rejoins the original continuation. The ordinary target
            // remapper below maps it to the synthetic copy of `call_ip + 1`.
            flat.push(Instr::Jump {
                target: call_ip as u32 + 1,
            });
            resume.push(cp.fallback_ip);
            let next = (base + flat.len()) as u32;
            let Instr::JumpIfFalse { target, .. } = &mut flat[miss_at] else {
                unreachable!()
            };
            *target = next;
        }
        // No bounded arm matched. Keep a sentinel through ordinary target
        // remapping, then patch it to the ORIGINAL replay prefix; otherwise an
        // in-range original ip would be mistaken for a synthetic branch target.
        fallback_fixup = Some(flat.len());
        flat.push(Instr::Jump { target: u32::MAX });
        resume.push(cp.fallback_ip);
    }
    resume.push(*resume.last()?);

    for ins in &mut flat {
        let target = match ins {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target,
            _ => continue,
        };
        if (s..=e).contains(&(*target as usize)) {
            *target = (base + vip_of[&(*target as usize)]) as u32;
        }
    }
    let fix = fallback_fixup?;
    let Instr::Jump { target } = &mut flat[fix] else {
        unreachable!()
    };
    *target = cp.fallback_ip;

    let new_start = base as u32;
    let new_end = (base + flat.len() - 1) as u32;
    code.extend(flat);
    for (&ip, &j) in &ta_plan.access {
        if !(s..=e).contains(&ip) {
            access.insert(ip, j);
        }
    }
    let mut captured_calls = FxHashMap::default();
    for (&ip, site) in &ta_plan.captured_calls {
        if !(s..=e).contains(&ip) {
            captured_calls.insert(ip, *site);
            continue;
        }
        let (Some(&get_off), Some(&call_off)) =
            (vip_of.get(&site.get_ip), vip_of.get(&site.call_ip))
        else {
            continue;
        };
        let mut mapped = *site;
        mapped.get_ip = base + get_off;
        mapped.call_ip = base + call_off;
        captured_calls.insert(mapped.call_ip, mapped);
    }
    let new_ta = TaPinPlan {
        pins: ta_plan.pins.clone(),
        access,
        captured_calls,
    };
    let mut sp = proto.clone();
    sp.reg_count = sp.reg_count.max(win_top as u16);
    sp.code = code;
    sp.constants = constants;
    sp.string_constants = string_constants;
    let admitted = int_unadmitted_ips(&sp, new_start, new_end, &new_ta, false);
    if admitted.as_ref().is_none_or(|v| !v.is_empty()) {
        if let Some(ips) = admitted {
            for ip in ips {
                log_decline(format_args!("computed reject @{ip}: {:?}", sp.code[ip]));
            }
        }
        decline!("[{start},{end}] computed flattened body is not INT-admissible");
    }

    let computed_guard = DenseComputedEntryGuard {
        recv_src: cp.recv_src,
        recv_bits: cp.recv_bits,
        recv_ver: cp.recv_ver,
        elements: cp
            .variants
            .iter()
            .map(|lp| (lp.callee_bits, lp.callee_ver))
            .collect(),
        helper: cp.guard_helper,
    };
    if std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] INT computed splice [{start},{end}] @{}: {} dense arms, fallback @{}, {} ops",
            call_ip,
            cp.variants.len(),
            cp.fallback_ip,
            new_end - new_start + 1
        );
    }
    Some(IntSplice {
        proto: sp,
        start: new_start,
        end: new_end,
        ta_plan: new_ta,
        resume,
        guards: Vec::new(),
        route_epoch,
        computed_guards: vec![computed_guard],
        regs_fits: (regs_fits_helper, win_top),
        scratch_base: Some(proto.reg_count),
    })
}

/// Append one call site's spliced body. `r` is the resume ip every op of it
/// carries (the site's `def_ip` — see [`Site::def_ip`]).
fn emit_splice(
    site: &Site,
    flat: &mut Vec<Instr>,
    resume: &mut Vec<u32>,
    constants: &mut Vec<Value>,
    string_constants: &mut Vec<String>,
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
        let Some(m) = map_body_instr(ins, &map, constants, string_constants, lp) else {
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
    string_constants: &mut Vec<String>,
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
        Instr::GetProp { dst, obj, name } => {
            let key = lp.string_constants.get(name as usize)?;
            let mapped_name = match string_constants
                .iter()
                .position(|candidate| candidate == key)
            {
                Some(index) => index as u32,
                None => {
                    string_constants.push(key.clone());
                    (string_constants.len() - 1) as u32
                }
            };
            Instr::GetProp {
                dst: m(dst),
                obj: m(obj),
                name: mapped_name,
            }
        }
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
        Instr::MathOp {
            dst,
            op: crate::bytecode::MathFn::Imul,
            callee,
            this_v,
            arg_base,
            argc: 2,
        } => Instr::MathOp {
            dst: m(dst),
            op: crate::bytecode::MathFn::Imul,
            callee: m(callee),
            this_v: m(this_v),
            arg_base: m(arg_base),
            argc: 2,
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
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
    )
}

/// B223: the ONE call form the replay allowlist can admit — `str.charCodeAt(i)`
/// on a pinned flat-ASCII string. The allowlist above is deliberately closed
/// because a replayed span must be effect-free, and a `CallMethod` is exactly
/// the shape that can run user code; but this one provably cannot. Three facts
/// compose, each checked in code rather than assumed:
///
/// 1. `Recv::Str` is produced ONLY for a `CallMethod` whose name constant is
///    literally `"charCodeAt"` (`jit_plans.rs`), so no other method reaches a
///    string pin;
/// 2. `STR_PIN_KIND` is assigned only when the RUNTIME receiver is a flat
///    ASCII `HeapObj::Str`, and the pin snapshot is re-guarded per execution —
///    a rope, a non-ASCII string or any other receiver deopts before the load;
/// 3. the INT emitter's string-pin arm inlines a byte load and, as
///    `plan_region`'s own admission comment puts it, "runs no user code,
///    allocates nothing".
///
/// So re-running it after a guard exit is a pure re-read of the same byte.
/// `pinned_arr_push` is deliberately NOT admitted here: it is the other
/// `CallMethod` the INT tier allows, and it COMMITS an append — the precise
/// hazard the allowlist's comment names. `ZIPP_NO_SPLICE_CCA=1` restores the
/// unconditional decline.
fn span_pinned_charcodeat(proto: &FuncProto, ta_plan: &TaPinPlan, ip: usize) -> bool {
    splice_charcodeat_enabled()
        && matches!(
            proto.code[ip],
            Instr::CallMethod { .. } | Instr::GetProp { .. } | Instr::CallWithThis { .. }
        )
        && ta_plan
            .access
            .get(&ip)
            .is_some_and(|&j| ta_plan.pins[j as usize].kind == crate::codegen::STR_PIN_KIND)
}

/// B223 latch: see `span_pinned_charcodeat`.
fn splice_charcodeat_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_SPLICE_CCA").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Every exit inside `[def_ip, call_ip]` resumes at `def_ip`, so the whole span
/// runs again in the interpreter. Prove that is the same as not having run it:
/// the span must be effect-free, branch-free, un-enterable from elsewhere, and
/// must not clobber a register a later op of the span reads.
fn span_is_replayable(
    proto: &FuncProto,
    ta_plan: &TaPinPlan,
    def_ip: usize,
    call_ip: usize,
    s: usize,
    e: usize,
) -> Option<()> {
    let mut written: FxHashSet<u16> = FxHashSet::default();
    let mut all_written: FxHashSet<u16> = FxHashSet::default();
    for ip in def_ip..call_ip {
        // Fail closed: this span is replayed from `def_ip` after ANY native
        // dispatch/guard exit.  It may therefore contain only operations whose
        // admitted INTEGER implementation cannot perform observable work.  In
        // particular, a pinned `arr.push(int)` is an admitted `CallMethod`; if
        // it sat here, a later computed-key miss would commit the push and then
        // replay it in the interpreter.  Keep calls, construction, printing,
        // appends, throws and every store-like opcode out by using a pure
        // allowlist instead of trying to enumerate today's effectful variants.
        if !matches!(
            proto.code[ip],
            Instr::LoadConst { .. }
                | Instr::LoadInt { .. }
                | Instr::Move { .. }
                | Instr::LoadGlobal { .. }
                | Instr::Add { .. }
                | Instr::Sub { .. }
                | Instr::Mul { .. }
                | Instr::Mod { .. }
                | Instr::AddInt { .. }
                | Instr::Neg { .. }
                | Instr::Bitwise { .. }
                | Instr::Not { .. }
                | Instr::Lt { .. }
                | Instr::Le { .. }
                | Instr::Gt { .. }
                | Instr::Ge { .. }
                | Instr::Eq { .. }
                | Instr::Ne { .. }
                | Instr::GetIndex { .. }
                | Instr::GetProp { .. }
        ) && !span_pinned_charcodeat(proto, ta_plan, ip)
        {
            decline!("@{call_ip} non-replayable op @{ip} in callee-def span");
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
        if let Some(t) = bytecode_control_target(ins) {
            if (def_ip + 1..=call_ip).contains(&(t as usize)) {
                decline!("@{call_ip} a jump enters the callee-def span at @{t}");
            }
        }
    }
    Some(())
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
            if let Some(t) = bytecode_control_target(&proto.code[ip]) {
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
        | Instr::NewPlannedObject { dst, .. }
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
        | Instr::TypeOfIs { dst, a, .. } => r(vec![a], Some(dst)),
        Instr::IsArray {
            dst,
            a,
            callee,
            this_v,
        } => r(vec![a, callee, this_v], Some(dst)),
        Instr::JsonParse {
            dst,
            a,
            callee,
            this_v,
        } => r(vec![a, callee, this_v], Some(dst)),
        Instr::JsonStringify {
            dst,
            val,
            space,
            callee,
            this_v,
        } => r(vec![val, space, callee, this_v], Some(dst)),
        Instr::HasProp { dst, key, obj, .. } => r(vec![key, obj], Some(dst)),
        Instr::InstanceOfDyn { dst, val, ctor } => r(vec![val, ctor], Some(dst)),
        Instr::ArrayRest { dst, src, .. } => r(vec![src], Some(dst)),
        Instr::ObjectSpread { target, src } => r(vec![target, src], None),
        Instr::RequireObject { val } => r(vec![val], None),
        Instr::IterClose { iter } | Instr::IterCloseQuiet { iter } => r(vec![iter], None),
        Instr::CallSpread { dst, callee, args } => r(vec![callee, args], Some(dst)),
        Instr::CallWithThisSpread {
            dst,
            callee,
            this_v,
            args,
        } => r(vec![callee, this_v, args], Some(dst)),
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
            callee,
            arg_base,
            argc,
            ..
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.extend(callee);
            r(v, Some(dst))
        }
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
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(callee);
            v.push(this_v);
            r(v, Some(dst))
        }
        Instr::GlobalFn {
            dst,
            callee,
            arg_base,
            argc,
            ..
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(callee);
            r(v, Some(dst))
        }
        Instr::NewArray {
            dst,
            arg_base,
            argc,
        } => r((0..argc).map(|k| arg_base + k).collect(), Some(dst)),
        Instr::StaticFn {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(callee);
            v.push(this_v);
            r(v, Some(dst))
        }
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
        Instr::CallWithThis {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(callee);
            v.push(this_v);
            r(v, Some(dst))
        }
        Instr::RegExpMethod {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(callee);
            v.push(this_v);
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
        Instr::CallMethodComputed {
            dst,
            obj,
            key,
            arg_base,
            argc,
        } => {
            let mut v: Vec<u16> = (0..argc).map(|k| arg_base + k).collect();
            v.push(obj);
            v.push(key);
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
    if let Some(epoch) = entry.route_epoch {
        let epoch_off = crate::vm::host_api::JIT_GLOBAL_ROUTE_EPOCH_OFFSET as i32;
        dynasm!(ops
            ; cmp DWORD [rdi + epoch_off], epoch as i32
            ; jne => entry_bail
        );
    }
    for &(addr, gen) in entry.guards {
        dynasm!(ops
            ; mov rax, QWORD addr as i64
            ; cmp DWORD [rax], gen as i32
            ; jne => entry_bail
        );
    }
    if !entry.computed_guards.is_empty() {
        // Win64 shadow/alignment exactly like `jit_regs_fits` above. These
        // helpers are read-only and execute before any native region op/home,
        // so every miss can take the header entry bail with no flush/replay
        // ambiguity.
        let pad: i32 = if has_shadow { 0 } else { 40 };
        let guard_fail = ops.new_dynamic_label();
        let guards_done = ops.new_dynamic_label();
        if pad != 0 {
            dynasm!(ops ; sub rsp, pad);
        }
        for guard in entry.computed_guards {
            match guard.recv_src {
                TaPinSrc::Global(g) => {
                    dynasm!(ops ; mov rdx, [r12 + (g as i32) * 8]);
                }
                TaPinSrc::Reg(r) => {
                    dynasm!(ops ; mov rdx, [rbx + dreg(r)]);
                }
            }
            dynasm!(ops
                ; mov rax, QWORD guard.recv_bits as i64
                ; cmp rdx, rax
                ; jne => guard_fail
            );
            for (index, &(callee_bits, callee_ver)) in guard.elements.iter().enumerate() {
                let packed = ((guard.recv_ver as u64) << 32) | index as u64;
                let expect = ((callee_ver as u64) << 1) | 1;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, QWORD guard.recv_bits as i64
                    ; mov r8, QWORD packed as i64
                    ; mov r9, QWORD callee_bits as i64
                    ; mov rax, QWORD guard.helper as i64
                    ; call rax
                    ; mov r10, QWORD expect as i64
                    ; cmp rax, r10
                    ; jne => guard_fail
                );
            }
        }
        if pad != 0 {
            dynasm!(ops ; add rsp, pad);
        }
        dynasm!(ops ; jmp => guards_done ; => guard_fail);
        if pad != 0 {
            dynasm!(ops ; add rsp, pad);
        }
        dynasm!(ops ; jmp => entry_bail ; => guards_done);
    }
}
