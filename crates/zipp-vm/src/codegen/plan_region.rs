// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Decline this region, naming the reason under `ZIPP_JITDECLINE=1`. The planner
/// has ~25 exit points and `ZIPP_JITLOG` only reports `plan_region=None`, which
/// says a region missed the fastest tier but not what to fix. Diagnostic only —
/// the env lookup happens once per declined region-compile, never per iteration.
macro_rules! decline {
    ($reason:expr) => {{
        if std::env::var_os("ZIPP_JITDECLINE").is_some() {
            eprintln!("[decline-reason] {}", $reason);
        }
        return None;
    }};
}

/// Plan register homes for `[start, end]`, or `None` to decline (use mem path).
/// `ta_plan` (unboxed-region epic): the pinned-TypedArray plan, threaded so a
/// later increment can admit a pinned-Float64Array element GetIndex/SetIndex as a
/// VTy::Num xmm home. Not yet consulted — the blanket index/call/bitwise decline
/// below still applies (regions with those take the memory path).
pub(crate) fn plan_region(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
) -> Option<RegionPlan> {
    plan_region_cold(proto, start, end, ta_plan, admit_bitwise, &FxHashSet::default())
}

/// `cold`: ips the caller will emit as SIDE EXITS (B9) rather than as native
/// code. They are skipped by every analysis pass here — they never execute
/// natively, so they neither define a home's value nor constrain its type, and
/// letting them into the walks would only make the planner decline a region
/// whose hot path is perfectly typed.
pub(crate) fn plan_region_cold(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
    cold: &FxHashSet<usize>,
) -> Option<RegionPlan> {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    // ── unboxed-region epic: pinned TypedArray element access ──
    // A pinned GetIndex/SetIndex whose element kind matches the REGISTER PATH can run
    // unboxed: the double/regalloc path (admit_bitwise=false) hosts a kind-8 Float64
    // element as an f64 xmm home; the integer path (admit_bitwise=true) hosts a kind-5
    // Int32 element as a sign-extended i64 home. The two are mutually exclusive (an f64
    // can't be an i64 home, and vice-versa), so a region mixing kinds declines the
    // non-matching access to the memory path. The receiver `obj` (a heap TA) and index
    // `key` are NOT homed — the emitter reads the receiver via the pin's source and the
    // index via `key`'s home. Identify the admissible ops + their receiver regs so the
    // typing/homing passes below bypass the receivers.
    let pin_kind: u8 = if admit_bitwise { 5 } else { 8 };
    let pinned_elem = |ip: usize| -> bool {
        ta_plan
            .access
            .get(&ip)
            .map_or(false, |&j| ta_plan.pins[j as usize].kind == pin_kind)
    };
    // A pinned flat-ASCII STRING access (kind 254): `str.charCodeAt(i)` (CallMethod)
    // and `str.length` (GetProp), both on the int path only (admit_bitwise). The
    // receiver is read via the pin snapshot, never the register, so its reg is
    // excluded from typing/homing exactly like a pinned-element receiver.
    let pinned_str = |ip: usize| -> bool {
        admit_bitwise
            && ta_plan
                .access
                .get(&ip)
                .map_or(false, |&j| ta_plan.pins[j as usize].kind == STR_PIN_KIND)
    };
    let mut ta_recv_regs: FxHashSet<u16> = FxHashSet::default();
    {
        // Candidate receiver regs: the `obj` of every pinned index op, and the
        // `obj` of every pinned-STRING charCodeAt/length access.
        let mut recv: FxHashSet<u16> = FxHashSet::default();
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) { continue; }
            if pinned_elem(s + off) {
                if let Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. } = *instr {
                    recv.insert(obj);
                }
            }
            if pinned_str(s + off) {
                if let Instr::CallMethod { obj, .. } | Instr::GetProp { obj, .. } = *instr {
                    recv.insert(obj);
                }
            }
        }
        if !recv.is_empty() {
            // Each receiver reg must be defined by EXACTLY ONE LoadGlobal and used
            // ONLY as a pinned-index `obj` (else it would need a real home — decline
            // the whole region to the memory path, which handles it).
            let mut def_n: FxHashMap<u16, u32> = FxHashMap::default();
            let mut def_lg: FxHashSet<u16> = FxHashSet::default();
            for (off, instr) in code[s..=e].iter().enumerate() {
                if cold.contains(&(s + off)) { continue; }
                if let Some(d) = writes_reg(instr) {
                    *def_n.entry(d).or_insert(0) += 1;
                    if matches!(instr, Instr::LoadGlobal { .. }) {
                        def_lg.insert(d);
                    }
                }
            }
            let mut used_elsewhere: FxHashSet<u16> = FxHashSet::default();
            for (off, instr) in code[s..=e].iter().enumerate() {
                if cold.contains(&(s + off)) { continue; }
                // The receiver use AT a pinned access is exempt (read via the pin,
                // not the register). For a pinned-STRING access the obj-use is also
                // invisible to instr_uses today (CallMethod→vec![]; GetProp→[obj]),
                // so the CallMethod half is forward-defensive; the GetProp half is
                // the one that actually exempts a dual-use string receiver.
                let idx_obj = if pinned_elem(s + off) {
                    match *instr {
                        Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. } => Some(obj),
                        _ => None,
                    }
                } else if pinned_str(s + off) {
                    match *instr {
                        Instr::CallMethod { obj, .. } | Instr::GetProp { obj, .. } => Some(obj),
                        _ => None,
                    }
                } else {
                    None
                };
                for u in instr_uses(instr) {
                    if Some(u) != idx_obj && recv.contains(&u) {
                        used_elsewhere.insert(u);
                    }
                }
            }
            for &r in &recv {
                // Cleanly excludable when EITHER: (a) defined by exactly one
                // LoadGlobal and used only as a pinned obj (the global-receiver
                // case); OR (b) a live-in PARAM receiver — ZERO in-region defs and
                // used only as a pinned obj (`str` in fnv1a is reg1, the param,
                // read only via the STR pin). `def_n.get(&r).is_none()` (NOT
                // `!= Some(&1)`) is load-bearing: a multiply-defined reg must NOT
                // be admitted (it would need a real home).
                let clean_global =
                    def_n.get(&r) == Some(&1) && def_lg.contains(&r) && !used_elsewhere.contains(&r);
                let clean_param = def_n.get(&r).is_none() && !used_elsewhere.contains(&r);
                if clean_global || clean_param {
                    ta_recv_regs.insert(r);
                } else {
                    // A receiver register reused for other (numeric) values can't be
                    // cleanly excluded under the non-SSA register model → memory path.
                    // (Generalizing this needs SSA-like per-use disambiguation; it
                    // currently blocks e.g. the xorshift-fill `iv[i] = st|0` loop whose
                    // bytecode reuses the `iv` register for bitwise temps.)
                    return None;
                }
            }
        }
    }
    let mut ty: FxHashMap<u16, VTy> = FxHashMap::default();
    let mut first_seen: FxHashMap<u16, bool> = FxHashMap::default(); // reg → was first occurrence a def?
    let mut glob_first_read: FxHashMap<u32, bool> = FxHashMap::default(); // slot → first touch was a read?
    let mut reg_order: Vec<u16> = Vec::new();
    let mut glob_order: Vec<u32> = Vec::new();

    // Record a use (operand) of reg `r` with required type `req`.
    // Returns false on a type conflict (caller declines).
    let note_def = |r: u16, t: VTy, ty: &mut FxHashMap<u16, VTy>, first_seen: &mut FxHashMap<u16, bool>, reg_order: &mut Vec<u16>| -> bool {
        if let Some(prev) = ty.get(&r) {
            if *prev != t {
                return false;
            }
        } else {
            ty.insert(r, t);
            reg_order.push(r);
        }
        first_seen.entry(r).or_insert(true); // first occurrence is a def
        true
    };

    // Two passes are awkward with closures; do a single ordered pass collecting
    // type (from defs) and first-occurrence (def vs use). Operand type
    // requirements are validated in a second loop once types are known.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        // A call or a bitwise op can't be register-allocated (boxed Values / int32
        // lanes / arbitrary user code) — decline to the memory path. A dense-array
        // GetIndex/SetIndex likewise declines UNLESS it is a kind-8 (Float64) pinned
        // TypedArray access, which the f64 element fast path emits inline (the
        // element is a VTy::Num xmm home; receiver/index handled specially below).
        match *instr {
            Instr::Call { .. } => decline!("Call"),
            // A pinned-STRING charCodeAt is admitted on the int path (inlines to a
            // byte load, runs no user code, allocates nothing — the no-call
            // invariant that keeps BOOL_GPRS alive holds). Any other method call,
            // or a charCodeAt whose receiver isn't pinned, declines.
            Instr::CallMethod { .. } if !pinned_str(s + off) => decline!("CallMethod (receiver not a pinned string)"),
            // A Bitwise op declines UNLESS the caller (the INT path) admits it: its
            // i64 homes hold sign-extended integers, so the low 32 bits ARE ToInt32
            // and the op runs inline with no reload/rebox. The regalloc/double path
            // passes admit_bitwise=false (its homes are f64, not int32 lanes).
            Instr::Bitwise { .. } if !admit_bitwise => decline!("Bitwise on the double path"),
            Instr::GetIndex { .. } | Instr::SetIndex { .. } => {
                if !pinned_elem(s + off) {
                    decline!("GetIndex/SetIndex (element not a pinned TypedArray)");
                }
            }
            _ => {}
        }
        let (def, dty): (Option<u16>, VTy) = match *instr {
            // A pinned-f64 GetIndex loads an f64 element into a Num home.
            Instr::GetIndex { dst, .. } => (Some(dst), VTy::Num),
            Instr::LoadInt { dst, .. } => (Some(dst), VTy::Num),
            Instr::LoadConst { dst, .. } => (Some(dst), VTy::Num),
            Instr::LoadGlobal { dst, .. } => (Some(dst), VTy::Num),
            Instr::AddInt { dst, .. } => (Some(dst), VTy::Num),
            Instr::Neg { dst, .. } => (Some(dst), VTy::Num),
            Instr::Add { dst, .. }
            | Instr::Sub { dst, .. }
            | Instr::Mul { dst, .. }
            | Instr::Div { dst, .. }
            | Instr::Mod { dst, .. } => (Some(dst), VTy::Num),
            // A bitwise/shift result is always a number (a signed i32, or a u32
            // for `>>>` — both fit a Num i64 home).
            Instr::Bitwise { dst, .. } => (Some(dst), VTy::Num),
            Instr::Lt { dst, .. }
            | Instr::Le { dst, .. }
            | Instr::Gt { dst, .. }
            | Instr::Ge { dst, .. }
            | Instr::Eq { dst, .. }
            | Instr::Ne { dst, .. } => (Some(dst), VTy::Bool),
            // Pinned-STRING charCodeAt → a small int (0..65535); pinned-STRING
            // length → the snapshot units; both land in a Num i64 home.
            Instr::CallMethod { dst, .. } if pinned_str(s + off) => (Some(dst), VTy::Num),
            Instr::GetProp { dst, .. } if pinned_str(s + off) => (Some(dst), VTy::Num),
            // `Math.imul` → a signed i32 (Num). BLOCKER FIX: without this the carried
            // fnv1a accumulator (written ONLY by Imul) is never typed → the
            // used-but-undefined scan declines the whole region.
            Instr::MathOp { dst, op: MathFn::Imul, argc: 2, .. } => (Some(dst), VTy::Num),
            Instr::Move { dst, .. } => (Some(dst), VTy::Num), // refined below
            _ => (None, VTy::Num),
        };
        // Record operand first-occurrences (uses) BEFORE the def, so a reg used
        // and defined by the same op counts the use first (live-in).
        for u in instr_uses(instr) {
            first_seen.entry(u).or_insert(false); // first occurrence is a use ⇒ live-in
            if !ty.contains_key(&u) {
                // Type not yet known; tentatively untyped — refined when defined.
            }
        }
        // A TA-receiver reg is NOT typed/homed (sourced via the pin); skip its def.
        if let Some(d) = def.filter(|d| !ta_recv_regs.contains(d)) {
            // Move's dst type follows its src; default Num is corrected here.
            let t = if let Instr::Move { src, .. } = *instr {
                *ty.get(&src).unwrap_or(&VTy::Num)
            } else {
                dty
            };
            if !note_def(d, t, &mut ty, &mut first_seen, &mut reg_order) {
                decline!("type conflict on a reused register");
            }
        }
        // Globals: order + first-touch direction. A TA-receiver's LoadGlobal is
        // excluded (no numeric home; the element emitter reads it via the pin).
        match *instr {
            Instr::LoadGlobal { dst, .. } if ta_recv_regs.contains(&dst) => {}
            Instr::LoadGlobal { idx, .. } => {
                glob_first_read.entry(idx).or_insert(true);
                if !glob_order.contains(&idx) {
                    glob_order.push(idx);
                }
            }
            Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } => {
                glob_first_read.entry(idx).or_insert(false);
                if !glob_order.contains(&idx) {
                    glob_order.push(idx);
                }
            }
            _ => {}
        }
    }

    // A register used but never defined in the region is a read-only live-in —
    // most often a numeric FUNCTION PARAMETER, e.g. the `n` in
    // `function f(n){ for (var k=0;k<n;k++) ... }`.
    //
    // MEASURED 2026-07-25 — do not admit these BLANKETLY by typing them Num and
    // letting the entry guard sort it out. That is correct (emit_int_entry_load
    // bails unless the value is genuinely Int-tagged, so nothing can be misread)
    // but it is SLOWER: the INT path then accepts regions whose live-ins are
    // strings, doubles or objects, entry-bails on every OSR entry, and displaces
    // the MEM compile that was working. Suite geomean regressed 3.31x -> 3.45x,
    // worst on sparse-array (-18%) and async-promise-chain (-16%).
    //
    // What that note asked for, and what this is: admit ONLY registers used
    // exclusively as ARITHMETIC OPERANDS (`numeric_operand_uses`), so the guard
    // is backed by how the value is consumed rather than by hope. A live-in that
    // ever reaches a heap op, a `Move`, a `StoreGlobal`, an `Add` (which is also
    // string concat) or an `Eq`/`Ne` (which accept any type) still declines the
    // whole region, exactly as before. When the guard does fail, `entry_bail`
    // resumes at the loop header — an in-region ip — so it counts as a deopt and
    // the region self-evicts to the memory path after OSR_DEOPT_LIMIT tries.
    //
    // Worth 2.2x on the shape it unblocks: the same 20M-iteration loop ran 115ms
    // with a parameter bound (INT declined -> DOUBLE/MEM) vs 52ms with a literal
    // bound (INT compiled).
    //
    // TA-receiver regs are intentionally untyped (sourced via the pin) — skip them.
    let mut ro_live_in: Vec<u16> = Vec::new();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        for u in instr_uses(instr) {
            if !ta_recv_regs.contains(&u) && !ty.contains_key(&u) && !ro_live_in.contains(&u) {
                ro_live_in.push(u);
            }
        }
    }
    if !ro_live_in.is_empty() {
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) { continue; }
            let numeric = numeric_operand_uses(instr);
            for u in instr_uses(instr) {
                if ro_live_in.contains(&u) && !numeric.contains(&u) {
                    decline!("read-only live-in used where a number isn't required"); // 
                }
            }
        }
        // Entry-guarded Int, permanently homed (live-in ⇒ whole-region range).
        for &r in &ro_live_in {
            ty.insert(r, VTy::Num);
            first_seen.insert(r, false);
            reg_order.push(r);
        }
    }

    // Each pinned index op needs its index (and a SetIndex's stored value) in a
    // Num home: the emitter reads the index from `key`'s home and a SetIndex stores
    // `val`'s home. Decline if either isn't a number home.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        if pinned_elem(s + off) {
            let bad = match *instr {
                Instr::GetIndex { key, .. } => ty.get(&key) != Some(&VTy::Num),
                Instr::SetIndex { key, val, .. } => {
                    ty.get(&key) != Some(&VTy::Num) || ty.get(&val) != Some(&VTy::Num)
                }
                _ => false,
            };
            if bad {
                decline!("pinned index operand is not numeric");
            }
        }
    }

    // Validate operand type requirements now that types are known.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        match *instr {
            Instr::Add { a, b, .. }
            | Instr::Sub { a, b, .. }
            | Instr::Mul { a, b, .. }
            | Instr::Div { a, b, .. }
            | Instr::Mod { a, b, .. }
            | Instr::Lt { a, b, .. }
            | Instr::Le { a, b, .. }
            | Instr::Gt { a, b, .. }
            | Instr::Ge { a, b, .. }
            | Instr::Eq { a, b, .. }
            | Instr::Ne { a, b, .. }
            | Instr::JumpIfNotLt { a, b, .. }
            | Instr::JumpIfNotLe { a, b, .. }
            | Instr::Bitwise { a, b, .. } => {
                if ty.get(&a) == Some(&VTy::Bool) || ty.get(&b) == Some(&VTy::Bool) {
                    decline!("numeric op on a bool"); // outside the subset
                }
            }
            Instr::AddInt { a, .. } | Instr::Neg { a, .. } => {
                if ty.get(&a) == Some(&VTy::Bool) {
                    return None;
                }
            }
            Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => {
                // Only bool conditions are supported (the loop-guard shape).
                if ty.get(&cond) != Some(&VTy::Bool) {
                    decline!("branch condition is not a bool");
                }
            }
            _ => {}
        }
    }

    // Loop-invariant constant detection: a reg defined exactly once, by a
    // LoadInt/LoadConst, and not live-in, holds the same value every iteration —
    // materialise it once in the prologue and skip the body op.
    let mut def_count: FxHashMap<u16, u32> = FxHashMap::default();
    let mut const_def_ip: FxHashMap<u16, usize> = FxHashMap::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        match *instr {
            Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } => {
                *def_count.entry(dst).or_insert(0) += 1;
                const_def_ip.insert(dst, s + off);
            }
            _ => {
                if let Some(d) = writes_reg(instr) {
                    *def_count.entry(d).or_insert(0) += 1;
                }
            }
        }
    }
    // Registers that are actually USED as an operand somewhere in the region.
    // A defined-but-unused reg is DEAD (e.g. an object-ref load neutralised to
    // `LoadInt 0` by the field-promotion rewrite) — it must NOT be hoisted, or it
    // would consume a permanent xmm home for a value that's never read.
    let mut used: FxHashSet<u16> = FxHashSet::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        for u in instr_uses(instr) {
            used.insert(u);
        }
    }
    // ── pinned-STRING / Math.imul operand liveness ── instr_uses + writes_reg are
    // BLIND to CallMethod and MathOp operands/dsts (verified: both hit their `_`
    // arm). So the charCodeAt index reg, the Imul operand regs, and the charCodeAt/
    // Imul result-def ips are invisible — without this the index/operands would be
    // classed DEAD (their defining Move/Bitwise/LoadInt DCE'd → the inline op reads
    // an unmaterialised home / `xh` panics), and the home-reuse allocator would
    // free them at the wrong ip. Collect them LOCALLY (NOT by widening the shared
    // instr_uses/writes_reg, which SROA / f64-regalloc / leaf-inline depend on) and
    // feed both `used` (here) and the live-range touch loop (below). `(ip,reg,def)`.
    let mut str_imul_touch: Vec<(usize, u16, bool)> = Vec::new();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        let ip = s + off;
        match *instr {
            Instr::CallMethod { dst, arg_base, argc: 1, .. } if pinned_str(ip) => {
                str_imul_touch.push((ip, arg_base, false)); // index (use)
                str_imul_touch.push((ip, dst, true)); // charCodeAt result (def)
            }
            Instr::MathOp { dst, arg_base, op: MathFn::Imul, argc: 2, .. } => {
                str_imul_touch.push((ip, arg_base, false));
                str_imul_touch.push((ip, arg_base + 1, false));
                str_imul_touch.push((ip, dst, true)); // imul result (def)
            }
            _ => {}
        }
    }
    for &(_, r, is_def) in &str_imul_touch {
        if !is_def {
            used.insert(r);
        }
    }
    // ── dead-code elimination ── a register written in the region but never read
    // (not in `used`) is dead. Every int-region op is a pure value computation, so
    // its defining op produces a result nothing observes and can be skipped — and
    // the reg dropped from home allocation. Drop dead regs from `reg_order` so they
    // consume no xmm home and don't count toward the pool-overflow check (which can
    // flip the loop to the slower home-reuse path). `dead` excludes loop-carried
    // (live-in) regs — those are read across iterations even if not within one.
    // "Never read IN THE REGION" is not "dead": a dead reg gets no home, its
    // defining op is skipped and nothing is flushed, so the frame slot keeps
    // whatever the INTERPRETER last left there. If the register is read AFTER
    // the region, that is a silent wrong answer —
    //
    //     function f(){ for (var i=0;i<40;i++) { var q = i; } return q; }
    //
    // returned 7 (the value at the final pre-OSR iteration) instead of 39. The
    // declarator form is what exposes it: a plain `q = expr` also emits
    // `Move{dst:temp, src:q}` for the statement's value, which keeps `q` in
    // `used`, but `var q = expr` does not.
    //
    // There is no live-out analysis in codegen, so require the register to be
    // untouched by the REST OF THE FUNCTION as well. Conservative (a reg read
    // anywhere outside `[s, e]` is kept) and cheap — one scan of the proto.
    let read_outside: FxHashSet<u16> = code
        .iter()
        .enumerate()
        .filter(|(ip, _)| *ip < s || *ip > e)
        .flat_map(|(_, instr)| instr_uses(instr))
        .collect();
    let dead: FxHashSet<u16> = reg_order
        .iter()
        .copied()
        .filter(|r| {
            !used.contains(r) && first_seen.get(r) != Some(&false) && !read_outside.contains(r)
        })
        .collect();
    reg_order.retain(|r| !dead.contains(r));
    let mut hoist_ips: Vec<usize> = Vec::new();
    let mut hoisted: FxHashSet<u16> = FxHashSet::default();
    for (&r, &ip) in &const_def_ip {
        // `first_seen == true` only says the first OCCURRENCE is a def — it says
        // nothing about whether that def runs. Hoisting a constant whose def sits
        // on an untaken branch is wrong twice over: the prologue materialises it
        // (so the flush writes it over the register's real value) and the body op
        // is elided (so reads inside the region see the constant too). Require
        // the def to run on every pass. See `runs_every_iteration`.
        if def_count.get(&r) == Some(&1)
            && first_seen.get(&r) == Some(&true)
            && used.contains(&r)
            && runs_every_iteration(code, s, e, ip)
        {
            hoist_ips.push(ip);
            hoisted.insert(r);
        }
    }
    hoist_ips.sort_unstable();

    // Exact values of single-def integer-constant regs (hoisted or not): used by
    // the analysis entry state, the Mul strength reduction and the gpr mirrors.
    let mut const_vals: FxHashMap<u16, i64> = FxHashMap::default();
    for (&r, &ip) in &const_def_ip {
        if def_count.get(&r) != Some(&1) {
            continue;
        }
        match code[ip] {
            Instr::LoadInt { val, .. } => {
                const_vals.insert(r, val as i64);
            }
            Instr::LoadConst { idx, .. } => {
                if let Some(c) = proto.constants.get(idx as usize) {
                    if c.is_int() {
                        const_vals.insert(r, (c.bits() as u32 as i32) as i64);
                    }
                }
            }
            _ => {}
        }
    }

    // ── bool ↔ global decline ── globals are always homed as NUMBERS (an xmm,
    // flushed through `emit_int_box_from_home`), while a bool-typed reg is homed
    // in a gpr. Moving a value between the two would ask `xh()` for the xmm home
    // of a gpr-homed register, which is `unreachable!` — so `var b = i < 100;` at
    // top level inside any hot loop PANICKED the engine rather than declining.
    // There is no boxing path here, so decline the region to the memory tier.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        let bool_glob = match *instr {
            Instr::StoreGlobal { src, .. } | Instr::StoreGlobalStrict { src, .. } => src,
            Instr::LoadGlobal { dst, .. } => dst,
            _ => continue,
        };
        if ty.get(&bool_glob) == Some(&VTy::Bool) {
            decline!("bool value moved to/from a global (globals are numeric homes)");
        }
    }

    // ── home unification (copy coalescing) ── temps that only shuttle a global's
    // (or a live-in reg's) value share that value's home; the body copies vanish.
    //
    // DISABLED (soundness): an aliased reg shares another value's home, so its
    // home cannot be initialised from its own frame slot at entry — and an exit
    // taken before the alias's def then flushes the OTHER value into its slot.
    // That is a silent wrong answer, not a deopt: `for (i=0;i<8;i++) s = i;`
    // returned `i` (8) instead of 7, because the region is entered on the final
    // back-edge, runs zero body iterations, and flushes anyway. Re-enabling this
    // needs per-exit flush sets driven by a must-def dataflow (see PERF_ROADMAP).
    const UNIFY_HOMES: bool = false;
    let jump_targets = region_jump_targets(code, s, e);
    let (glob_alias, move_alias) = if UNIFY_HOMES {
        let g =
            unify_homes_with_globals(code, s, e, &ty, &first_seen, &dead, &hoisted, &jump_targets);
        let m = unify_move_homes(
            code, s, e, &ty, &first_seen, &dead, &hoisted, &jump_targets, &g,
        );
        (g, m)
    } else {
        (FxHashMap::default(), FxHashMap::default())
    };
    // Aliased regs don't consume an xmm home of their own.
    reg_order.retain(|r| !glob_alias.contains_key(r) && !move_alias.contains_key(r));

    // ── overflow-guard elision (INT path) ── interval analysis proves which
    // arithmetic results always stay inside [-2^53, 2^53].
    let mut elide_guard: FxHashSet<usize> = FxHashSet::default();
    let mut mul_shift: FxHashMap<usize, (u16, u8)> = FxHashMap::default();
    if region_is_int(proto, start, end, ta_plan) {
        let mut entry = AbsState {
            regs: FxHashMap::default(),
            globs: FxHashMap::default(),
            alias: FxHashMap::default(),
            cmp: None,
        };
        for (&r, &def_first) in &first_seen {
            if !def_first && ty.get(&r) == Some(&VTy::Num) {
                entry.regs.insert(r, IV_I32); // live-in reg: entry-guarded Int
            }
        }
        for (&g, &read_first) in &glob_first_read {
            if read_first {
                entry.globs.insert(g, IV_I32); // live-in global: entry-guarded Int
            }
        }
        for &r in &hoisted {
            if let Some(&v) = const_vals.get(&r) {
                entry.regs.insert(r, (v, v)); // materialised in the prologue
            }
        }
        elide_guard = analyze_int_guards(proto, s, e, entry);
        // Strength-reduce a guard-elided `Mul` by a constant power of two into a
        // left shift (`psllq`), skipping the imul gpr round-trip.
        for ip in s..=e {
            if !elide_guard.contains(&ip) {
                continue;
            }
            if let Instr::Mul { a, b, .. } = code[ip] {
                let (val_reg, k) = match (const_vals.get(&a), const_vals.get(&b)) {
                    (_, Some(&k)) => (a, k),
                    (Some(&k), _) => (b, k),
                    _ => continue,
                };
                if k >= 2 && (k as u64).is_power_of_two() {
                    mul_shift.insert(ip, (val_reg, (k as u64).trailing_zeros() as u8));
                }
            }
        }
    }

    // Per-register live range [first_ip, last_ip] within the region (for linear-
    // scan reuse). A live-in reg (used before defined) is loop-carried, so its
    // value spans the whole region [s, e]; otherwise it lives from its first
    // appearance to its last. Globals are loop-carried (whole region).
    let mut first_ip: FxHashMap<u16, usize> = FxHashMap::default();
    let mut last_ip: FxHashMap<u16, usize> = FxHashMap::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        let ip = s + off;
        let mut touch = |r: u16| {
            first_ip.entry(r).or_insert(ip);
            last_ip.insert(r, ip);
        };
        for u in instr_uses(instr) {
            touch(u);
        }
        if let Some(d) = writes_reg(instr) {
            touch(d);
        }
    }
    // Extend the live ranges for the charCodeAt-index / Imul-operand uses and the
    // charCodeAt/Imul result defs that instr_uses/writes_reg miss (S5 MAJOR): the
    // home-reuse allocator (active when n_numeric > POOL) would otherwise free/
    // reuse these homes at the wrong ip and clobber them. (fnv1a has few regs so
    // reuse is off and this is inert there, but it is required for a larger
    // STR-pinned region to be sound.)
    for &(ip, r, _is_def) in &str_imul_touch {
        first_ip.entry(r).or_insert(ip);
        let e = last_ip.entry(r).or_insert(ip);
        if ip > *e {
            *e = ip;
        }
    }
    let range = |r: u16| -> (usize, usize) {
        // Whole-region (permanent home) if loop-carried (live-in, used before
        // defined) OR a HOISTED constant — hoisted values are materialised once
        // in the prologue and read every iteration, so their home must never be
        // freed/reused mid-region (doing so clobbered them — a real bug).
        // A register READ OUTSIDE the region also keeps a permanent home. Sharing
        // one is only sound when clobbering the loser's frame slot is invisible,
        // and that is exactly the condition: `flush_exit` writes the shared home
        // to EVERY sharer's slot, so a sharer whose value still matters after the
        // region would come back holding an unrelated temp. Region-local liveness
        // does not imply the register is dead in the enclosing function — that is
        // what made blanket reuse unsound. Same `read_outside` set the dead-code
        // pass uses.
        if first_seen.get(&r) == Some(&false)
            || hoisted.contains(&r)
            || read_outside.contains(&r)
        {
            (s, e)
        } else {
            (first_ip[&r], last_ip[&r])
        }
    };
    // Registers eligible to SHARE an xmm: not loop-carried, not a hoisted
    // constant, and not read after the region. They need no entry load either —
    // an early exit flushing a stale value into their slot is unobservable.
    let shareable = |r: u16| -> bool {
        first_seen.get(&r) != Some(&false)
            && !hoisted.contains(&r)
            && !read_outside.contains(&r)
    };

    // The xmm home pool size. If one-home-per-numeric-value fits, use the simple
    // allocation (distinct home each — best ILP, what loop.js relies on). Only
    // when it would OVERFLOW do we linear-scan-reuse homes for non-overlapping
    // live ranges (lets bigger loops JIT, and is required for object SROA).
    const POOL: usize = (HOME_XMM_LAST - HOME_XMM_FIRST + 1) as usize;
    let n_numeric = reg_order.iter().filter(|r| ty[r] == VTy::Num).count() + glob_order.len();
    // DISABLED (soundness): linear-scan reuse gives two registers with
    // non-overlapping IN-REGION live ranges the same home, but `flush_exit`
    // writes that home to BOTH frame slots, so the sharer whose range already
    // ended is overwritten with an unrelated temp's value. Region-local liveness
    // does not imply the register is dead in the ENCLOSING FUNCTION — a loop
    // that assigns five locals early and then runs a long chain returned the
    // chain's temps in place of the locals. Reuse also defeats the entry-load
    // fix: `live_in_regs` then holds several `(reg, xmm)` pairs sharing one
    // `xmm`, so the prologue loads overwrite each other and only the last one
    // survives. Needs the same must-def / live-out analysis as UNIFY_HOMES; the
    // regions affected (>POOL numeric values) fall back to the memory tier.
    // Re-enabled, but only for `shareable` registers (see `range` above). The
    // blanket version was unsound: it shared homes on REGION-LOCAL liveness, so a
    // local assigned early in a big loop came back holding a later temp. Scoping
    // it to registers that are provably never read after the region removes the
    // failure mode and keeps what reuse is for — letting a loop with more live
    // numeric values than the 14-home pool reach the register tiers at all,
    // instead of declining to the memory path.
    let reuse = n_numeric > POOL;

    // ── allocate xmm/gpr homes ──
    let mut reg_home: FxHashMap<u16, Home> = FxHashMap::default();
    let mut glob_home: FxHashMap<u32, u8> = FxHashMap::default();
    let first_free_xmm: u8;
    if reuse {
        // Linear-scan: numeric values (regs + globals) by ascending range start,
        // reusing a home once a value's range ends. Loop-carried values (globals
        // and live-in regs) span [s, e] and so keep a permanent home.
        let mut intervals: Vec<(usize, usize, NumVal)> = Vec::new();
        for &r in &reg_order {
            if ty[&r] == VTy::Num {
                let (a, b) = range(r);
                intervals.push((a, b, NumVal::Reg(r)));
            }
        }
        for &gi in &glob_order {
            intervals.push((s, e, NumVal::Glob(gi)));
        }
        intervals.sort_by_key(|&(a, _, _)| a);
        let mut alloc = XmmAlloc::new();
        for (a, b, v) in intervals {
            let x = match alloc.alloc(a, b) {
                Some(x) => x,
                None => decline!("xmm pool exhausted even with home reuse"),
            };
            match v {
                NumVal::Reg(r) => {
                    reg_home.insert(r, Home::Xmm(x));
                }
                NumVal::Glob(gi) => {
                    glob_home.insert(gi, x);
                }
            }
        }
        first_free_xmm = alloc.next; // never-touched homes are free for constants
    } else {
        // One distinct home per numeric value (best ILP — what loop.js relies on).
        let mut next_xmm = HOME_XMM_FIRST;
        for &r in &reg_order {
            if ty[&r] == VTy::Num {
                if next_xmm > HOME_XMM_LAST {
                    decline!("xmm pool exhausted (registers)");
                }
                reg_home.insert(r, Home::Xmm(next_xmm));
                next_xmm += 1;
            }
        }
        for &gi in &glob_order {
            if next_xmm > HOME_XMM_LAST {
                decline!("xmm pool exhausted (globals)");
            }
            glob_home.insert(gi, next_xmm);
            next_xmm += 1;
        }
        first_free_xmm = next_xmm;
    }
    // Bools (both modes): gpr homes; a live-in bool is unsupported.
    let mut next_bool = 0usize;
    for &r in &reg_order {
        if ty[&r] == VTy::Bool {
            if first_seen.get(&r) == Some(&false) || next_bool >= BOOL_GPRS.len() {
                decline!("bool live-in, or bool gpr pool exhausted");
            }
            reg_home.insert(r, Home::Gpr(BOOL_GPRS[next_bool]));
            next_bool += 1;
        }
    }

    // ── spare-home constants ──
    // Distinct `AddInt` immediates get a permanent xmm const home when the pool
    // has room (saves a per-iteration materialise+convert in the loop body).
    let mut addint_imm_home: FxHashMap<i32, u8> = FxHashMap::default();
    {
        let mut imms: Vec<i32> = Vec::new();
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) { continue; }
            if let Instr::AddInt { imm, .. } = *instr {
                if !imms.contains(&imm) {
                    imms.push(imm);
                }
            }
        }
        let mut next = first_free_xmm;
        for imm in imms {
            if next > HOME_XMM_LAST {
                break;
            }
            addint_imm_home.insert(imm, next);
            next += 1;
        }
    }
    // Hoisted integer constants used as compare operands get a spare bool-gpr
    // mirror so int-path compares avoid a second `movq` from the xmm home.
    let mut gpr_const: FxHashMap<u16, (u8, i64)> = FxHashMap::default();
    {
        let mut cand: Vec<u16> = Vec::new();
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) { continue; }
            let (a, b) = match *instr {
                Instr::Lt { a, b, .. }
                | Instr::Le { a, b, .. }
                | Instr::Gt { a, b, .. }
                | Instr::Ge { a, b, .. }
                | Instr::Eq { a, b, .. }
                | Instr::Ne { a, b, .. }
                | Instr::JumpIfNotLt { a, b, .. }
                | Instr::JumpIfNotLe { a, b, .. } => (a, b),
                _ => continue,
            };
            for r in [a, b] {
                if hoisted.contains(&r) && const_vals.contains_key(&r) && !cand.contains(&r) {
                    cand.push(r);
                }
            }
        }
        let mut nb = next_bool;
        for r in cand {
            if nb >= BOOL_GPRS.len() {
                break;
            }
            gpr_const.insert(r, (BOOL_GPRS[nb], const_vals[&r]));
            nb += 1;
        }
    }

    // ── apply home unification ── aliased regs share their value's home; their
    // own slots are still flushed (from the shared home) on every exit.
    for (&r, &g) in &glob_alias {
        reg_home.insert(r, Home::Xmm(glob_home[&g]));
    }
    for (&r, &src) in &move_alias {
        let h = reg_home[&src];
        reg_home.insert(r, h);
    }

    // ── derived lists from the final homes (unified for both modes) ──
    // With reuse, several regs may share an xmm; flush_exit writes the shared
    // value to each reg's slot, which is sound (non-overlapping live ranges mean
    // the dead members are never read before being redefined).
    let mut num_regs = Vec::new();
    let mut bool_regs = Vec::new();
    let mut live_in_regs = Vec::new();
    let mut live_in_bools = Vec::new();
    for &r in &reg_order {
        match reg_home[&r] {
            Home::Xmm(x) => {
                num_regs.push((r, x));
                // Every flushed home is entry-loaded, not just the read-first
                // ones — see `RegionPlan::live_in_regs`. Two exceptions:
                // `hoisted` regs (the prologue materialises their constant right
                // after this, so a load would be dead), and `shareable` regs,
                // whose slot is never read after the region — flushing a stale
                // value into it is unobservable, and they may SHARE an xmm with
                // another register, which makes a per-register entry load
                // meaningless (the loads would overwrite each other).
                if !hoisted.contains(&r) && !shareable(r) {
                    live_in_regs.push((r, x));
                }
            }
            Home::Gpr(g) => {
                bool_regs.push((r, g));
                live_in_bools.push((r, g));
            }
        }
    }
    // Home-unified regs aren't in reg_order; they're flushed from the SHARED home
    // (never live-in: unification requires the first occurrence to be a def).
    for (&r, &g) in &glob_alias {
        num_regs.push((r, glob_home[&g]));
    }
    for (&r, _) in &move_alias {
        if let Home::Xmm(x) = reg_home[&r] {
            num_regs.push((r, x));
        }
    }
    let mut globs = Vec::new();
    let mut live_in_globs = Vec::new();
    for &gi in &glob_order {
        let x = glob_home[&gi];
        globs.push((gi, x));
        // Every flushed global is entry-loaded too. A def-first global used to
        // flush an uninitialised xmm, which surfaced as a raw f64 bit pattern
        // (`g = i * 2` in an 8-trip loop printed 4626604192193053000).
        live_in_globs.push((gi, x));
    }

    Some(RegionPlan {
        reg_home,
        glob_home,
        live_in_regs,
        live_in_bools,
        live_in_globs,
        num_regs,
        bool_regs,
        globs,
        hoist_ips,
        hoisted,
        dead,
        elide_guard,
        mul_shift,
        addint_imm_home,
        gpr_const,
        jump_targets,
        ta_recv_regs,
    })
}

/// Does the instruction at `d` run on EVERY pass through region `[s, e]`?
///
/// True when no branch in `[s, d)` can jump PAST `d` while staying inside the
/// region. Branches that LEAVE the region are deliberately allowed, including
/// the loop header's own exit test: OSR entry only happens after the interpreter
/// has already executed the loop `OSR_THRESHOLD` times, so a def that runs every
/// iteration has already written its value into the frame slot — the region
/// materialising the same constant into a home and flushing it back is then a
/// no-op. A def that can be SKIPPED has no such guarantee.
///
/// This is the cheap sound approximation of "the def dominates every exit". It
/// is what makes constant hoisting (and `hoistable_length`) safe without a full
/// dominator tree.
pub(crate) fn runs_every_iteration(code: &[Instr], s: usize, e: usize, d: usize) -> bool {
    for (ip, instr) in code.iter().enumerate().take(d).skip(s) {
        let target = match *instr {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target as usize,
            _ => continue,
        };
        let _ = ip;
        if target > d && target <= e {
            return false; // jumps over `d` and stays in the region
        }
    }
    true
}

/// The operand positions of `i` that REQUIRE a number, as opposed to positions
/// that accept any value. A read-only live-in is admitted as a numeric home only
/// when every one of its uses appears here, so the entry Int-guard is backed by
/// how the value is consumed.
///
/// Deliberately EXCLUDES: `Add` (also string concatenation), `Eq`/`Ne` (defined
/// on every type), `Move` and `StoreGlobal` (pure transfers that say nothing
/// about the value), `StrConcat`, and every heap-op receiver/key. Admitting
/// those is what made the blanket version of this a 3.31x -> 3.45x regression.
/// Relational ops are included: they are string-comparable in principle, but in
/// a region that already type-checked as numeric a `<` operand is a loop bound.
pub(crate) fn numeric_operand_uses(i: &Instr) -> Vec<u16> {
    match *i {
        Instr::Sub { a, b, .. }
        | Instr::Mul { a, b, .. }
        | Instr::Div { a, b, .. }
        | Instr::Mod { a, b, .. }
        | Instr::Bitwise { a, b, .. }
        | Instr::Lt { a, b, .. }
        | Instr::Le { a, b, .. }
        | Instr::Gt { a, b, .. }
        | Instr::Ge { a, b, .. }
        | Instr::JumpIfNotLt { a, b, .. }
        | Instr::JumpIfNotLe { a, b, .. } => vec![a, b],
        Instr::AddInt { a, .. } | Instr::Neg { a, .. } => vec![a],
        Instr::MathOp { arg_base, argc, .. } => (0..argc).map(|k| arg_base + k).collect(),
        _ => vec![],
    }
}

/// The VM registers an instruction reads (operands). Used for live-in analysis.
pub(crate) fn instr_uses(i: &Instr) -> Vec<u16> {
    match *i {
        Instr::Move { src, .. } => vec![src],
        Instr::StoreGlobal { src, .. } | Instr::StoreGlobalStrict { src, .. } => vec![src],
        Instr::AddInt { a, .. } | Instr::Neg { a, .. } => vec![a],
        Instr::Add { a, b, .. }
        | Instr::Sub { a, b, .. }
        | Instr::Mul { a, b, .. }
        | Instr::Div { a, b, .. }
        | Instr::Mod { a, b, .. }
        | Instr::StrConcat { a, b, .. }
        | Instr::StrAppendInPlace { a, b, .. }
        | Instr::Bitwise { a, b, .. }
        | Instr::Lt { a, b, .. }
        | Instr::Le { a, b, .. }
        | Instr::Gt { a, b, .. }
        | Instr::Ge { a, b, .. }
        | Instr::Eq { a, b, .. }
        | Instr::Ne { a, b, .. }
        | Instr::JumpIfNotLt { a, b, .. }
        | Instr::JumpIfNotLe { a, b, .. } => vec![a, b],
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => vec![cond],
        Instr::GetProp { obj, .. } => vec![obj],
        Instr::SetProp { obj, val, .. } => vec![obj, val],
        Instr::GetIndex { obj, key, .. } => vec![obj, key],
        Instr::SetIndex { obj, key, val } => vec![obj, key, val],
        Instr::GetIndexConcat { obj, key, .. } => vec![obj, key],
        Instr::SetIndexConcat { obj, key, val, .. } => vec![obj, key, val],
        Instr::DeleteIndexConcat { obj, key, .. } => vec![obj, key],
        Instr::Return { src } => vec![src],
        // MathOp / CallMethod read a CONTIGUOUS argument window starting at
        // `arg_base` (plus the receiver for a method call). Reporting no uses
        // let the home-unification passes treat those registers as dead and
        // alias over them — see the matching note in `writes_reg`.
        Instr::MathOp { arg_base, argc, .. } => {
            (0..argc).map(|k| arg_base + k).collect()
        }
        Instr::CallMethod { obj, arg_base, argc, .. } => {
            let mut v = vec![obj];
            v.extend((0..argc).map(|k| arg_base + k));
            v
        }
        _ => vec![],
    }
}

/// Plan for promoting a single stable object's fields to registers (SROA-lite,
/// the effect of V8's escape-analysis + scalar replacement): when EVERY
/// GetProp/SetProp in a region targets the SAME object — a global `obj_global`
/// loaded by `LoadGlobal` and never re-stored in the region, and whose ref reg
/// is used ONLY as the GetProp/SetProp receiver — its accessed fields can live in
/// registers for the loop body, synced to the heap object only at region
/// entry/exit, so the loop becomes register-only like V8.
#[allow(dead_code)] // wired into codegen in a following step
pub(crate) struct FieldPromotePlan {
    /// The global slot holding the promoted object.
    pub(crate) obj_global: u32,
    /// Distinct accessed field name-constant indices, in first-seen order. Each
    /// maps to a synthetic "field global" the heap ops are rewritten to use.
    pub(crate) fields: Vec<u32>,
    /// Heap index of the live promoted object at compile time (an identity guard
    /// at region entry: the global could be reassigned to a different object).
    pub(crate) obj_idx: u32,
    /// The object's heap version at compile time. A key add/remove/redefine,
    /// freeze, or `setPrototypeOf` bumps it, so a mismatch at region entry means
    /// the validated all-own-data-slot shape may no longer hold (a field could
    /// have become an accessor / non-writable / inherited) — bail to the
    /// interpreter. See memory: sroa-accessor-miscompile.
    pub(crate) obj_version: u32,
}

/// Detect whether `[start, end]` is field-promotable; see `FieldPromotePlan`.
/// `globals`/`heap` give the live runtime shape so we can reject a receiver
/// whose promoted fields aren't all OWN non-accessor data slots (an accessor /
/// inherited / non-writable field would diverge — see sroa-accessor-miscompile).
#[allow(dead_code)] // wired into codegen in a following step
pub(crate) fn plan_field_promotion(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals: &[Value],
    heap: &crate::heap::Heap,
) -> Option<FieldPromotePlan> {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    if !code[s..=e]
        .iter()
        .any(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }))
    {
        return None;
    }

    // Single-def map (for tracing an obj-ref reg to its LoadGlobal).
    let mut reg_def: FxHashMap<u16, usize> = FxHashMap::default();
    let mut reg_def_count: FxHashMap<u16, u32> = FxHashMap::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if let Some(d) = writes_reg(instr) {
            reg_def.insert(d, s + off);
            *reg_def_count.entry(d).or_insert(0) += 1;
        }
    }

    // Every heap-op receiver must be the SAME global object, loaded once.
    let mut obj_global: Option<u32> = None;
    let mut obj_ref_regs: FxHashSet<u16> = FxHashSet::default();
    let mut fields: Vec<u32> = Vec::new();
    for instr in &code[s..=e] {
        let (obj_reg, name) = match *instr {
            Instr::GetProp { obj, name, .. } => (obj, name),
            Instr::SetProp { obj, name, .. } => (obj, name),
            _ => continue,
        };
        let def_ip = *reg_def.get(&obj_reg)?; // must be defined in the region
        if reg_def_count.get(&obj_reg) != Some(&1) {
            return None; // multiple defs → can't trace
        }
        let g = match code[def_ip] {
            Instr::LoadGlobal { idx, .. } => idx,
            _ => return None, // receiver isn't a plain global load
        };
        match obj_global {
            None => obj_global = Some(g),
            Some(prev) if prev == g => {}
            Some(_) => return None, // two different objects at the site set
        }
        obj_ref_regs.insert(obj_reg);
        // Dedup by the field STRING, not the name-constant INDEX: the compiler
        // emits a distinct string-constant per occurrence, so `o.a` read and
        // `o.a` write have DIFFERENT name indices for the SAME field. Keying by
        // index would give them separate pool slots (the read wouldn't see the
        // write). Keep one representative index per distinct field string.
        let fname = &proto.string_constants[name as usize];
        // `length` is a SPECIAL property (an array's element count / a string's
        // length), not a plain stored slot. Scalar-replacing it diverges from the
        // interpreter — e.g. `arr.length = n` truncates the array, but a promoted
        // scalar would just track a dead pool slot. Decline; the inline-cache /
        // helper path handles `.length` correctly (read) and deopts the write.
        if fname == "length" {
            return None;
        }
        if !fields
            .iter()
            .any(|&n| proto.string_constants[n as usize] == *fname)
        {
            fields.push(name);
        }
    }
    let g = obj_global?;

    // The object ref must be stable (G not re-stored) and its ref reg must not
    // escape (used only as the GetProp/SetProp receiver, nowhere else).
    for instr in &code[s..=e] {
        if let Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } = *instr {
            if idx == g {
                return None;
            }
        }
        // EVERY load of the promoted object must feed a heap op only — if `g` is
        // also loaded into a register that is NOT a heap-op receiver, that ref
        // could escape (be stored, or used numerically), so the object isn't
        // provably confined to the rewritten accesses. Decline (→ inline cache).
        if let Instr::LoadGlobal { dst, idx } = *instr {
            if idx == g && !obj_ref_regs.contains(&dst) {
                return None;
            }
        }
        if matches!(instr, Instr::GetProp { .. } | Instr::SetProp { .. }) {
            continue;
        }
        for u in instr_uses(instr) {
            if obj_ref_regs.contains(&u) {
                return None; // ref reg used outside a heap op → object escapes
            }
        }
    }

    // ── runtime shape check ── SROA scalar-replaces each field with a pool slot,
    // bypassing any getter/setter AND the property's writability. It is sound
    // ONLY when the live global is a plain object whose every promoted field is
    // an OWN, non-accessor DATA slot (writable if the region stores to it). An
    // inherited field, an accessor (a class get/set or a defineProperty accessor),
    // or a non-writable store target would diverge from the interpreter — which
    // runs the accessor / honours non-writability each iteration while the scalar
    // pool would not. (Found by the accessor-inline audit 2026-06-14; class
    // get/set live on the PROTOTYPE so `m.pos` returns None and we decline.)
    let gv = *globals.get(g as usize)?;
    if !gv.is_heap() {
        return None;
    }
    let obj_idx = gv.heap_index();
    let m = match heap.get(obj_idx) {
        crate::heap::HeapObj::Object(m) => m,
        _ => return None, // arrays / typed-arrays / fns aren't plain-field SROA targets
    };
    for &name_idx in &fields {
        let fname = &proto.string_constants[name_idx as usize];
        // Writability is required only if the region STORES to this field.
        let need_writable = code[s..=e].iter().any(|instr| match *instr {
            Instr::SetProp { name, .. } => proto.string_constants[name as usize] == *fname,
            _ => false,
        });
        match m.pos(fname) {
            Some(slot) if !m.attrs[slot].accessor && (!need_writable || m.attrs[slot].writable) => {}
            _ => return None,
        }
    }
    Some(FieldPromotePlan { obj_global: g, fields, obj_idx, obj_version: heap.version_of(obj_idx) })
}

