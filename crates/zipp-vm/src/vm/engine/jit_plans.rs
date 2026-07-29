// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// Build the TypedArray pin plan for the OSR region `[start, end]` from
    /// LIVE VM state (called right before `compile_region`, frame `base` on
    /// top): for each `GetIndex`/`SetIndex`, find the receiver's nearest
    /// preceding in-region writer — a `LoadGlobal g` with `g` never stored in
    /// the region pins via `Global(g)`; a receiver register never written in
    /// the region pins via `Reg(r)`; anything else is left to the generic
    /// helper. A source qualifies only if it holds a non-BigInt TypedArray
    /// RIGHT NOW (the emitted code is kind-specialised). The hint is purely an
    /// OPTIMISATION: every fast-path access re-checks receiver identity against
    /// the snapshot at runtime, and the snapshot helper re-validates kind /
    /// detach / bounds — a wrong or stale hint degrades to the helper path,
    /// never to a wrong answer.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_ta_pin_plan(
        &self,
        func_id: u32,
        start: u32,
        end: u32,
        base: usize,
    ) -> crate::codegen::TaPinPlan {
        use crate::codegen::{TaPin, TaPinPlan, TaPinSrc};
        // Conservative "does this instruction write register r" cover. An op
        // missing here only weakens the hint (see above) — never soundness.
        fn writes(i: &Instr, r: u16) -> bool {
            let dst = match *i {
                Instr::LoadInt { dst, .. }
                | Instr::LoadConst { dst, .. }
                | Instr::Move { dst, .. }
                | Instr::LoadGlobal { dst, .. }
                | Instr::LoadGlobalOrUndefined { dst, .. }
                | Instr::AddInt { dst, .. }
                | Instr::Add { dst, .. }
                | Instr::Sub { dst, .. }
                | Instr::Mul { dst, .. }
                | Instr::Div { dst, .. }
                | Instr::Mod { dst, .. }
                | Instr::Neg { dst, .. }
                | Instr::Not { dst, .. }
                | Instr::Bitwise { dst, .. }
                | Instr::Lt { dst, .. }
                | Instr::Le { dst, .. }
                | Instr::Gt { dst, .. }
                | Instr::Ge { dst, .. }
                | Instr::Eq { dst, .. }
                | Instr::Ne { dst, .. }
                | Instr::GetProp { dst, .. }
                | Instr::GetIndex { dst, .. }
                | Instr::HasProp { dst, .. }
                | Instr::StrConcat { dst, .. }
                | Instr::StrAppendInPlace { dst, .. }
                | Instr::Call { dst, .. }
                | Instr::CallMethod { dst, .. } => dst,
                _ => return false,
            };
            dst == r
        }
        let mut plan = TaPinPlan::default();
        let proto = self.func(func_id as usize);
        let (s, e) = (start as usize, end as usize);
        if e <= s || e >= proto.code.len() {
            return plan;
        }
        let mut stored_globals: rustc_hash::FxHashSet<u32> = rustc_hash::FxHashSet::default();
        for ins in &proto.code[s..=e] {
            if let Instr::StoreGlobal { idx, .. }
            | Instr::StoreGlobalStrict { idx, .. }
            | Instr::StoreGlobalResolved { idx, .. } = *ins {
                stored_globals.insert(idx);
            }
        }
        // Per-access pin selector: a TA-or-dense-Array element access (kind
        // taken from the LIVE receiver — a TypedArray's element kind, or
        // ARR_PIN_KIND for a dense Array), a DataView `get*`, or a flat-ASCII
        // `charCodeAt` string.
        enum Recv {
            Ta,
            Dv,
            Str,
            /// A `.length` read — resolves against whatever the receiver LIVES
            /// as: a flat-ASCII string (pin `units`) or a dense Array (pin
            /// `len`). Both snapshots already carry the length in the same slot.
            Len,
        }
        for aip in s..=e {
            let (obj, recv) = match proto.code[aip] {
                // `arr[i]` (GetIndex), `arr[i]=v` (SetIndex), and `i in arr`
                // (HasProp, brand=false) all pin their receiver the same way; the
                // LIVE heap object decides TA-kind vs ARR_PIN_KIND. SetIndex pins
                // only a TypedArray (its inline store path); a dense-Array store
                // is left to the generic helper (it can grow/realloc), but its
                // receiver is still observed here and resolves to ARR_PIN_KIND
                // only when the inline GetIndex/HasProp can use it.
                Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. } => (obj, Recv::Ta),
                Instr::HasProp { obj, brand: false, .. } => (obj, Recv::Ta),
                // A whitelisted DataView `get*` receiver pins the same way
                // (snapshot: data+byteOffset / byteLength).
                Instr::CallMethod { obj, name, argc, .. }
                    if (argc == 1 || argc == 2)
                        && proto
                            .string_constants
                            .get(name as usize)
                            .is_some_and(|k| crate::codegen::dv_get_kind(k).is_some()) =>
                {
                    (obj, Recv::Dv)
                }
                // A `str.charCodeAt(i)` receiver pins as a flat-ASCII string
                // (snapshot: bytes ptr + units), so the access inlines to a
                // direct byte load instead of the per-op `jit_char_code_at` call.
                Instr::CallMethod { obj, name, argc, .. }
                    if argc == 1
                        && proto
                            .string_constants
                            .get(name as usize)
                            .is_some_and(|k| k == "charCodeAt") =>
                {
                    (obj, Recv::Str)
                }
                // `.length` on the SAME receiver coalesces onto that receiver's pin
                // — the snapshot's third word IS the length for both pin families
                // (a string's `units`, a dense Array's `items.len()`). So the
                // guard of `for (i < str.length) str.charCodeAt(i)` AND of
                // `for (i < a.length) s += a[i]` each resolve via the pin instead
                // of a GetProp inline cache, which is what lets the whole loop
                // run unboxed on the integer tier rather than demoting to the
                // boxed memory path over one property read.
                Instr::GetProp { obj, name, .. }
                    if proto
                        .string_constants
                        .get(name as usize)
                        .is_some_and(|k| k == "length") =>
                {
                    (obj, Recv::Len)
                }
                _ => continue,
            };
            let writer = (s..aip).rev().find(|&wip| writes(&proto.code[wip], obj));
            let src = match writer.map(|wip| &proto.code[wip]) {
                Some(&Instr::LoadGlobal { idx, .. }) if !stored_globals.contains(&idx) => {
                    TaPinSrc::Global(idx)
                }
                Some(_) => continue,
                None => {
                    // Live-in receiver: pin only if NOTHING in the region
                    // writes it (so the prologue/refetch reg read stays the
                    // value the accesses see).
                    if proto.code[s..=e].iter().any(|i| writes(i, obj)) {
                        continue;
                    }
                    TaPinSrc::Reg(obj)
                }
            };
            let live = match src {
                TaPinSrc::Global(g) => {
                    self.globals.get(g as usize).copied().unwrap_or(Value::UNDEFINED)
                }
                TaPinSrc::Reg(r) => self.get(base, r),
            };
            if !live.is_heap() {
                continue;
            }
            let kind = match (self.heap.get(live.heap_index()), &recv) {
                (HeapObj::TypedArray { kind, .. }, Recv::Ta) if *kind < 9 => *kind,
                // A dense Array pins for inline `arr[i]` / `i in arr`. Decline
                // when it carries an `arr_props` overlay (defineProperty'd /
                // sparse-overlay index) or is a mapped-`arguments` object — both
                // need the interpreter's override-aware path, so a pin would be
                // wasted (the snapshot helper also declines at runtime → all-zero
                // → identity miss → generic helper, so this is an optimisation,
                // never a soundness gate).
                (HeapObj::Array(items), Recv::Ta | Recv::Len)
                    if !self.array_elements_overlaid(live.heap_index())
                        && !self.arguments_objs.contains_key(&live.heap_index()) =>
                {
                    // All-Int (sampled) ⇒ offer the array to the INTEGER tier, which
                    // unboxes `arr[i]` into an i64 home. Purely an admission hint —
                    // the emitted code re-checks every element's tag — so the sample
                    // may be bounded: a 200k-element array must not cost a full scan
                    // at every OSR compile. 64 from the front (the loop's first
                    // iterations, and where a mixed array usually reveals itself)
                    // plus a 64-point stride across the rest.
                    let n = items.len();
                    let head = n.min(64);
                    let all_int = items[..head].iter().all(|v| v.is_int())
                        && (n <= head || {
                            let step = (n - head).div_ceil(64).max(1);
                            items[head..].iter().step_by(step).all(|v| v.is_int())
                        });
                    if all_int {
                        crate::codegen::ARR_INT_PIN_KIND
                    } else {
                        crate::codegen::ARR_PIN_KIND
                    }
                }
                (HeapObj::DataView { .. }, Recv::Dv) => crate::codegen::DV_PIN_KIND,
                // Pin only a FLAT ASCII string — the inline byte load needs
                // byte i == UTF-16 unit i (a rope/non-ASCII string snapshots
                // zero and falls to the generic helper, so a wrong pin is safe;
                // we just skip pinning it here when it can't help).
                (HeapObj::Str(js), Recv::Str | Recv::Len) if js.is_ascii() => {
                    crate::codegen::STR_PIN_KIND
                }
                _ => continue,
            };
            let slot = match plan.pins.iter().position(|p| p.src == src && p.kind == kind) {
                Some(j) => j,
                None => {
                    if plan.pins.len() >= 8 {
                        continue; // slot budget — extra accesses use the helper
                    }
                    plan.pins.push(TaPin { src, kind });
                    plan.pins.len() - 1
                }
            };
            plan.access.insert(aip, slot as u8);
        }
        plan
    }

    /// Q4 v1: build the leaf-call inline plan for a memory-path region — the set
    /// of `Call` sites in `[start, end]` whose monomorphic cached callee is a
    /// PLAIN LEAF (`callee_leaf_ok`) the region emitter can inline straight-line.
    /// Each entry carries the callee's identity bits (the runtime guard), the
    /// scratch-window offset (the caller's `reg_count`), and the body to emit.
    /// A site not in the map keeps the per-call `jit_call_ic` helper.
    ///
    /// Resolution uses the LIVE per-site IC (`ic_call_mono`, read-only): the
    /// loop has executed `OSR_THRESHOLD` times by OSR-compile, so a hot
    /// monomorphic call already has its `Callee` way filled. A polymorphic /
    /// unfilled site simply isn't inlined.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_leaf_inline_plan(
        &self,
        func_id: u32,
        start: u32,
        end: u32,
    ) -> rustc_hash::FxHashMap<usize, crate::codegen::LeafInlinePlan> {
        use crate::codegen::{callee_leaf_ok, callee_leaf_ok_one_call, LeafInlinePlan};
        let mut plan = rustc_hash::FxHashMap::default();
        let caller = self.func(func_id as usize);
        let reg_window = caller.reg_count;
        let log = std::env::var_os("ZIPP_JITLOG").is_some();
        for ip in start as usize..=end as usize {
            let Instr::Call { argc, .. } = caller.code[ip] else {
                continue;
            };
            // Monomorphic plain-callee from the live IC (with the cached slot
            // version — the inline guard re-checks it to defeat GC slot-reuse ABA).
            let Some((callee_bits, callee_ver, fid, closure)) = self.ic_call_mono(func_id, ip)
            else {
                if log {
                    eprintln!("[leaf] fn{func_id}@{ip} NOT-MONO (no single Callee IC way)");
                }
                continue;
            };
            let callee = self.func(fid as usize);
            // A closure that captures upvalues is inlinable as long as its body
            // only READS them (`callee_leaf_ok` admits `UpvalGet` and nothing
            // else upvalue-shaped). Each cell is resolved HERE, from the exact
            // closure the identity guard pins — the inlined body has no frame, so
            // the frame-walking `jit_upval_get` would read the caller's closure.
            //
            // Before this, every captured-variable closure fell back to a real
            // call: `function mk(){ var u=3; return function(x){ return (x*u)|0; }; }`
            // ran 88ms/3M against 14ms for the identical closure with no capture,
            // which is the same 14ms a plain inlined leaf costs. The gate, not
            // closure dispatch, was the whole 6.3x.
            let mut upvals = rustc_hash::FxHashMap::default();
            if closure != NO_CLOSURE && !callee.upvalues.is_empty() {
                let cidx = Value::from_bits(callee_bits).heap_index();
                let n_up = match self.heap.get(cidx) {
                    crate::heap::HeapObj::Closure { upvalues, .. } => upvalues.len(),
                    // Not actually a closure object — the guard would fail anyway.
                    _ => 0,
                };
                if n_up < callee.upvalues.len() {
                    if log {
                        eprintln!(
                            "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE (closure has {n_up} \
                             upvalues, body expects {})",
                            callee.upvalues.len()
                        );
                    }
                    continue;
                }
                for i in 0..callee.upvalues.len() as u16 {
                    upvals.insert(i, Value::heap(self.closure_upvalue(cidx, i)).bits());
                }
            }
            // What `this` is in the callee window. The site is a plain `Call`,
            // so `thisArg` is undefined and `OrdinaryCallBindThis` decides:
            // undefined for a STRICT callee, the realm's global object for a
            // SLOPPY one. Both are compile-time constants, so both inline.
            //
            // An ARROW is the one shape that cannot: its `this` is captured
            // lexically and lives in the Closure, so it is neither of those two
            // values and there is nothing to bake.
            if callee.lexical_this {
                if log {
                    eprintln!(
                        "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE (arrow — \
                         lexical this)"
                    );
                }
                continue;
            }
            let this_bits = if callee.is_strict {
                Value::UNDEFINED.bits()
            } else if self.global_this != 0 {
                Value::heap(self.global_this).bits()
            } else {
                // No realm global yet (setup); nothing sound to bake.
                if log {
                    eprintln!("[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE (no realm global)");
                }
                continue;
            };
            // The carved scratch window must hold the callee's whole register
            // file; the headroom (vs MAX_FRAMES recursion) is checked at the
            // region entry by `jit_regs_fits`.
            // A plain leaf inlines directly. Otherwise try the WRAPPER shape: a
            // body whose only disqualifier is one `Call`, whose own callee is a
            // leaf. `function ri(n){ return (rnd()*n)|0; }` is the motivating case
            // — 3.75M calls per run of the log-scan benchmark, and inlining `rnd`
            // into `ri` did nothing on its own because the hot loop still called
            // `ri` for real.
            let mut nested = rustc_hash::FxHashMap::default();
            let mut extra_regs = 0u16;
            let mut nested_upvals: rustc_hash::FxHashMap<u16, u64> = rustc_hash::FxHashMap::default();
            let mut nested_consts: rustc_hash::FxHashMap<u32, u64> = rustc_hash::FxHashMap::default();
            let body = match callee_leaf_ok(callee) {
                Some(b) => b,
                None => {
                    let Some((outer, call_at)) = callee_leaf_ok_one_call(callee) else {
                        if log {
                            eprintln!("[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE (not leaf-eligible)");
                        }
                        continue;
                    };
                    match self.splice_nested_leaf(fid, callee, &outer, call_at) {
                        Some((flat, guard, inner_regs, inner_upvals, inner_consts)) => {
                            nested.insert(call_at, guard);
                            extra_regs = inner_regs;
                            nested_upvals = inner_upvals;
                            nested_consts = inner_consts;
                            if log {
                                eprintln!(
                                    "[leaf] fn{func_id}@{ip} callee fn{fid} NESTED-INLINE \
                                     (spliced at body ip {call_at}, +{inner_regs} regs)"
                                );
                            }
                            flat
                        }
                        None => {
                            if log {
                                eprintln!(
                                    "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE \
                                     (wrapper's inner call not inlinable)"
                                );
                            }
                            continue;
                        }
                    }
                }
            };
            // ── globals that are NOT slot bindings ──
            // An UNINITIALIZED slot is not an empty binding: a script
            // GlobalDeclarationInstantiation ($262.evalScript, and the test262
            // harness prelude) parks its var/function bindings as OWN PROPERTIES
            // of the global object and leaves the slot UNINITIALIZED by design,
            // so the interpreter's Load/StoreGlobal own-prop fallbacks govern
            // them. The inlined body emits LoadGlobal as a bare
            // `mov rax,[r12+idx*8]` (codegen/inline.rs) with no fallback, so it
            // reads the sentinel and the inlined callee sees `undefined`.
            //
            // That is a TIER DIVERGENCE — the failure mode this engine gates
            // hardest against. A harness function called from a loop worked for
            // the interpreted iterations and became "undefined is not a function"
            // the instant the leaf inline kicked in, always at the same
            // iteration, which reads like a scoping bug and is not one.
            if body.iter().any(|ins| {
                let g = match *ins {
                    Instr::LoadGlobal { idx, .. }
                    | Instr::LoadGlobalOrUndefined { idx, .. }
                    | Instr::StoreGlobal { idx, .. }
                    | Instr::StoreGlobalStrict { idx, .. }
                    | Instr::StoreGlobalResolved { idx, .. } => idx,
                    _ => return false,
                };
                self.globals.get(g as usize).is_some_and(|v| v.is_uninitialized())
            }) {
                if log {
                    eprintln!(
                        "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE \
                         (reads a global whose binding is an own property, not a slot)"
                    );
                }
                continue;
            }
            // Pre-resolve the numeric constants the body's `LoadConst` ops read
            // (callee_leaf_ok rejected any non-numeric constant).
            let mut consts = rustc_hash::FxHashMap::default();
            for instr in &body {
                if let Instr::LoadConst { idx, .. } = *instr {
                    if let Some(c) = callee.constants.get(idx as usize) {
                        consts.insert(idx, c.bits());
                    }
                }
            }
            if log {
                eprintln!(
                    "[leaf] fn{func_id}@{ip} callee fn{fid} INLINE-ELIGIBLE \
                     (argc={argc} callee_regs={} params={} body_ops={})",
                    callee.reg_count,
                    callee.param_count,
                    body.len()
                );
            }
            upvals.extend(nested_upvals);
            consts.extend(nested_consts);
            plan.insert(
                ip,
                LeafInlinePlan {
                    callee_bits,
                    callee_ver,
                    this_bits,
                    reg_window,
                    callee_reg_count: callee.reg_count + extra_regs,
                    param_count: callee.param_count,
                    body,
                    consts,
                    nested,
                    upvals,
                    cell_get: crate::vm::helpers_misc::jit_cell_get as usize,
                    cell_set: crate::vm::helpers_misc::jit_cell_set as usize,
                },
            );
        }
        plan
    }

    /// Q7 method-inline plan: in-region `CallMethod` sites whose LIVE receiver is
    /// a class instance with a trivial NO-`super` method body (own-`this` field
    /// reads + numeric arithmetic). Read-only — built (like the leaf plan) BEFORE
    /// the `&proto` borrow at the OSR-compile site. `base` is the caller frame
    /// base, used to read the live receiver exemplar from `self.regs` (the
    /// class-keyed IC doesn't record receiver instances). The emitted code guards
    /// the receiver identity+version and falls to the helper on ANY miss, so a
    /// stale/partial/wrong-shape plan is always safe (just slower). v1 is
    /// monomorphic per site (one exemplar receiver baked); other receivers /
    /// shapes miss to the helper. See [`crate::codegen::MethodInlinePlan`].
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_method_inline_plan(
        &self,
        func_id: u32,
        start: u32,
        end: u32,
        base: usize,
    ) -> rustc_hash::FxHashMap<usize, crate::codegen::MethodInlinePlan> {
        use crate::codegen::MethodInlinePlan;
        use crate::heap::HeapObj;
        const MAX_ARMS: usize = crate::codegen::JIT_IC_WAYS; // = 8
        let mut plan = rustc_hash::FxHashMap::default();
        if std::env::var_os("ZIPP_NO_METHOD_INLINE").is_some() {
            return plan; // kill-switch (live through all stages)
        }
        let log = std::env::var_os("ZIPP_JITLOG").is_some();
        let caller = self.func(func_id as usize);
        let reg_window = caller.reg_count;
        // The op at `ip` selects how the receiver's resolved member is inlined:
        // CallMethod -> class method; GetProp -> trivial class getter; SetProp ->
        // trivial class setter (Stage 5). All share receiver enumeration + the
        // guard tree; only the per-shape resolve/body/binding differ.
        #[derive(Clone, Copy)]
        enum MiKind {
            Method,
            Getter,
            Setter,
        }
        for ip in start as usize..=end as usize {
            let (obj, name, kind) = match caller.code[ip] {
                Instr::CallMethod { obj, name, .. } => (obj, name, MiKind::Method),
                Instr::GetProp { obj, name, .. } => (obj, name, MiKind::Getter),
                Instr::SetProp { obj, name, .. } => (obj, name, MiKind::Setter),
                _ => continue,
            };
            let key = &caller.string_constants[name as usize];
            // ── enumerate candidate receivers ── the live exemplar at the obj reg
            // (last iteration's value) PLUS, when `obj` was `arr[idx]`, the array's
            // dense elements (the `objs[i&3]` polymorphic shape). Every candidate
            // is independently identity+version-guarded, so an extra/wrong guess
            // just yields a dead arm — never a correctness risk.
            let mut cand_bits: Vec<u64> = Vec::new();
            let mut push_cand = |v: Value, cands: &mut Vec<Value>, bits: &mut Vec<u64>| {
                if v.is_heap() && !bits.contains(&v.bits()) && cands.len() < MAX_ARMS {
                    bits.push(v.bits());
                    cands.push(v);
                }
            };
            let mut cands: Vec<Value> = Vec::new();
            // PRIMARY source: receiver instances RECORDED at this site's Class*
            // IC fills during warmup — robust for `var o = arr[i]; o.m()` (where
            // `o` is loaded indirectly, defeating the obj-reg/array trace below).
            // Each is identity+version-guarded, so extras/stale are safe.
            if let Some(rset) = self.mi_recv.get(&(((func_id as u64) << 32) | ip as u64)) {
                for &b in rset {
                    push_cand(Value::from_bits(b), &mut cands, &mut cand_bits);
                }
            }
            // The live exemplar at the obj reg (always reliable — it's the op's
            // receiver, live at the op).
            if let Some(&v) = self.regs.get(base + obj as usize) {
                push_cand(v, &mut cands, &mut cand_bits);
            }
            // Best-effort: the dense elements of the array a `arr[idx]` receiver
            // came from (supplements recording; the temp may be reused).
            if let Some(arr_reg) = Self::mi_last_getindex_array(&caller.code, start as usize, ip, obj) {
                if let Some(&av) = self.regs.get(base + arr_reg as usize) {
                    if av.is_heap() {
                        if let HeapObj::Array(items) = self.heap.get(av.heap_index()) {
                            let snapshot: Vec<Value> = items.iter().copied().collect();
                            for el in snapshot {
                                push_cand(el, &mut cands, &mut cand_bits);
                            }
                        }
                    }
                }
            }
            // Build a guarded arm per candidate (any that declines is skipped).
            let n_cands = cands.len();
            let mut shapes = Vec::new();
            let mut win_top = 0u16;
            for recv in cands {
                let built = match kind {
                    MiKind::Method => self.build_method_shape(func_id, ip, recv, key, reg_window),
                    MiKind::Getter => {
                        self.build_accessor_shape(func_id, ip, recv, key, reg_window, false)
                    }
                    MiKind::Setter => {
                        self.build_accessor_shape(func_id, ip, recv, key, reg_window, true)
                    }
                };
                if let Some((shape, shape_top)) = built {
                    win_top = win_top.max(shape_top);
                    shapes.push(shape);
                }
            }
            let k = match kind {
                MiKind::Method => "method",
                MiKind::Getter => "getter",
                MiKind::Setter => "setter",
            };
            if shapes.is_empty() {
                // Say so. This `continue` used to be silent, which is how B59
                // stayed hidden for an unknown number of commits: every
                // super-using body declined `build_method_shape`, no INLINE line
                // appeared, and an absent line is indistinguishable from a site
                // that was never a candidate. `ZIPP_NO_METHOD_INLINE=1` was no
                // help either — killing a mechanism that is already declining
                // everywhere measures 0ms. A DECLINE line paired with the INLINE
                // line turns that 6x regression into one grep.
                if log && n_cands != 0 {
                    eprintln!(
                        "[mi] fn{func_id}@{ip} DECLINE {k} key={key} \
                         ({n_cands} candidate receivers, every arm declined)"
                    );
                }
                continue;
            }
            if log {
                eprintln!(
                    "[mi] fn{func_id}@{ip} INLINE {k} arms={} win_top={win_top}",
                    shapes.len()
                );
            }
            plan.insert(ip, MethodInlinePlan { reg_window, win_top, shapes });
        }
        plan
    }

    /// Splice a wrapper's single inner call into a FLAT body: the inner callee's
    /// ops replace the `Call`, with every inner register shifted above the outer
    /// window so the two never alias, and the inner `Return` rewritten as a `Move`
    /// into the call's `dst`.
    ///
    /// Returns `(flat_body, guard, inner_reg_count)`. `None` declines.
    ///
    /// v1 restrictions, all checked here: the inner call passes NO arguments (so
    /// there is no argument binding to emit mid-body — the prologue's zero-fill
    /// already leaves the whole inner window `undefined`, which is also the right
    /// `this` for the strict callees inlining admits); the wrapper captures no
    /// upvalues (the plan's `upvals` map is keyed by index, so two functions'
    /// upvalues would collide); and the inner body is branch-free (its ops are
    /// renumbered by the splice and v1 does not remap branch targets).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn splice_nested_leaf(
        &self,
        outer_fid: u32,
        outer: &crate::bytecode::FuncProto,
        outer_body: &[Instr],
        call_at: usize,
    ) -> Option<(
        Vec<Instr>,
        crate::codegen::NestedGuard,
        u16,
        rustc_hash::FxHashMap<u16, u64>,
        rustc_hash::FxHashMap<u32, u64>,
    )> {
        use crate::codegen::{callee_leaf_ok, NestedGuard};
        if !outer.upvalues.is_empty() {
            return None;
        }
        let (callee_reg, dst, argc) = match outer_body[call_at] {
            Instr::Call { callee, dst, argc, .. } => (callee, dst, argc),
            _ => return None,
        };
        if argc != 0 {
            return None;
        }
        // Resolve the wrapper's own call site from ITS live IC.
        let (bits, ver, inner_fid, inner_closure) = self.ic_call_mono(outer_fid, call_at)?;
        let inner = self.func(inner_fid as usize);
        if inner.lexical_this || !inner.is_strict {
            return None;
        }
        let inner_body = callee_leaf_ok(inner)?;
        if inner_body.iter().any(|i| {
            matches!(
                i,
                Instr::Jump { .. } | Instr::JumpIfFalse { .. } | Instr::JumpIfTrue { .. }
            )
        }) {
            return None;
        }
        // Shift every inner register above the outer window.
        let off = outer.reg_count;
        let ret_src = match inner_body.last()? {
            Instr::Return { src } => Some(*src + off),
            Instr::ReturnUndefined => None,
            _ => return None,
        };
        // The inner body's `LoadConst` indices address the INNER constant pool, so
        // bias them past the outer pool and bake the inner values under the biased
        // keys. Without this the emitter looked the index up in the WRONG pool,
        // got nothing, materialised zero bits and bailed — which is why a body
        // ending in `/ 4294967296` (a double constant) deopted 64 times and
        // evicted, while the same body ending in `/ 4` (a small int, emitted as
        // `LoadInt`) inlined cleanly.
        let const_off = outer.constants.len() as u32;
        let mut consts = rustc_hash::FxHashMap::default();
        let mut flat: Vec<Instr> = outer_body[..=call_at].to_vec(); // keep the Call as the guard marker
        for instr in &inner_body[..inner_body.len() - 1] {
            if let Instr::LoadConst { idx, .. } = *instr {
                let c = inner.constants.get(idx as usize)?;
                if !c.is_number() {
                    return None;
                }
                consts.insert(idx + const_off, c.bits());
            }
            flat.push(shift_leaf_regs(instr, off, const_off)?);
        }
        // The inner result becomes the wrapper's call destination.
        flat.push(match ret_src {
            Some(src) => Instr::Move { dst, src },
            None => Instr::LoadUndefined { dst },
        });
        flat.extend_from_slice(&outer_body[call_at + 1..]);
        // The spliced body's upvalue ops need the INNER closure's cells. Without
        // this they fell back to a zero cell, deopted on every iteration and the
        // call quietly re-ran in the interpreter — correct, but 10x slower than
        // the plain call it replaced. The outer wrapper is required to capture
        // nothing (checked above), so these indices own the map.
        let mut upvals = rustc_hash::FxHashMap::default();
        if !inner.upvalues.is_empty() {
            if inner_closure == NO_CLOSURE {
                return None;
            }
            let cidx = Value::from_bits(bits).heap_index();
            let n_up = match self.heap.get(cidx) {
                crate::heap::HeapObj::Closure { upvalues, .. } => upvalues.len(),
                _ => return None,
            };
            if n_up < inner.upvalues.len() {
                return None;
            }
            for i in 0..inner.upvalues.len() as u16 {
                upvals.insert(i, Value::heap(self.closure_upvalue(cidx, i)).bits());
            }
        }
        let guard = NestedGuard { callee_reg, bits, ver };
        Some((flat, guard, inner.reg_count, upvals, consts))
    }

    /// The last `GetIndex{dst:obj_reg, obj:arr}` in `code[start..ip]` (the array a
    /// `arr[idx]` receiver came from), so the planner can bake an arm per array
    /// element. `None` if `obj_reg` was last produced by something else.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn mi_last_getindex_array(code: &[Instr], start: usize, ip: usize, obj_reg: u16) -> Option<u16> {
        let mut arr = None;
        for instr in &code[start..ip] {
            if let Instr::GetIndex { dst, obj, .. } = *instr {
                if dst == obj_reg {
                    arr = Some(obj);
                }
            }
        }
        arr
    }

    /// Build one receiver arm for a `CallMethod` inline: validate `recv` is a
    /// plain class instance with no own-shadow of `key`, resolve its class method
    /// (+ any `super.m()`), and bake the per-receiver guards/slots. Returns the
    /// arm and its scratch-window top, or `None` to skip this receiver. Read-only.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_method_shape(
        &self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
        reg_window: u16,
    ) -> Option<(crate::codegen::MethodInlineShape, u16)> {
        use crate::heap::HeapObj;
        let ridx = recv.heap_index();
        if !self.ic_obj_ok(ridx) {
            return None;
        }
        // Two receiver shapes inline. A class instance resolves the method
        // through its class; a PLAIN object resolves it to an own data property
        // holding a function — `{ m() {…} }`, the module/callback/vtable shape,
        // which is everywhere in real JavaScript and previously never inlined
        // (measured 21ns/call against 3.8ns for the same method on a class).
        let (recv_class, vals_ptr) = match self.heap.get(ridx) {
            HeapObj::Object(m) if !m.is_ctor => (m.class, m.vals.as_ptr() as u64),
            _ => return None,
        };
        // An own property named `key` shadows a CLASS method, so a class
        // receiver declines; for a plain receiver that own property IS the
        // method, and its slot is what gets guarded.
        let own_slot = match self.heap.get(ridx) {
            HeapObj::Object(m) => m.pos(key),
            _ => None,
        };
        let (fid, method_slot) = match (recv_class, own_slot) {
            (Some(c), None) => (self.ic_class_method_fid(func_id, ip, c)?, None),
            (_, Some(slot)) => {
                // The own property must be a plain data slot (an accessor would
                // have to RUN, not be called) holding a closure with no
                // captures — an upvalue read has no frame to resolve against in
                // an inlined body.
                let (fv, is_data) = match self.heap.get(ridx) {
                    HeapObj::Object(m) => (m.vals[slot], !m.attrs[slot].accessor),
                    _ => return None,
                };
                if !is_data || !fv.is_heap() {
                    return None;
                }
                let f = match self.heap.get(fv.heap_index()) {
                    HeapObj::Func(f) => *f,
                    HeapObj::Closure { func, upvalues, .. } if upvalues.is_empty() => *func,
                    _ => return None,
                };
                // An ARROW must not be inlined here. `HeapObj::Closure` carries
                // the arrow's captured `this_val`, and this match drops it — the
                // spliced body would then run with reg 0 = the RECEIVER, which is
                // exactly the binding `lexical_this` exists to suppress. For
                // `function Maker(){ this.f=111; this.o={f:3, m:()=>this.f} }`
                // the inlined `o.m()` returned 3 where the interpreter and node
                // return 111 — a silent wrong answer at default thresholds, and
                // an ordinary shape (an arrow stored as an object method).
                //
                // `upvalues.is_empty()` does NOT screen them out: an arrow that
                // captures only `this` has no upvalues at all.
                if self.func(f as usize).lexical_this {
                    return None;
                }
                (f, Some((slot as u32, fv.bits())))
            }
            _ => return None,
        };
        let callee = self.func(fid as usize);
        // Outer body admits `super.m()` (Stage 3); super targets do not.
        let body_len = Self::method_inline_body_ok(callee, true, false)?;
        let body: Vec<Instr> = callee.code[..body_len].to_vec();
        let field_slots = self.mi_bake_fields(ridx, &body, &callee.string_constants)?;
        let consts = Self::mi_bake_consts(&callee.constants, &body);
        // ── bake each `super.m()` in the body (Stage 3) ──
        let super_win = reg_window + callee.reg_count;
        let mut supers = rustc_hash::FxHashMap::default();
        let mut max_super_regs = 0u16;
        for (bi, instr) in body.iter().enumerate() {
            if let Instr::SuperMethod { home_class_id, name: sname, argc: sargc, .. } = *instr {
                if sargc != 0 {
                    return None; // v1: 0-arg super only
                }
                let skey = &callee.string_constants[sname as usize];
                let sr = self.ic_super_method_baked(fid, bi, home_class_id, skey)?;
                let scallee = self.func(sr.fid as usize);
                // Super target must be inlinable AND have NO nested super (v1).
                let sblen = Self::method_inline_body_ok(scallee, false, false)?;
                let sbody: Vec<Instr> = scallee.code[..sblen].to_vec();
                let sfields = self.mi_bake_fields(ridx, &sbody, &scallee.string_constants)?;
                let sconsts = Self::mi_bake_consts(&scallee.constants, &sbody);
                max_super_regs = max_super_regs.max(scallee.reg_count);
                supers.insert(
                    bi,
                    crate::codegen::SuperInline {
                        // The VM `mi_class_epoch` scalar's address is stable for
                        // the run (Vm is not moved); bake a pointer + the value.
                        epoch_ptr: &self.mi_class_epoch as *const u32 as u64,
                        epoch_val: self.mi_class_epoch,
                        hops: sr.hops,
                        holder_vals_ptr: sr.holder_vals_ptr,
                        holder_slot: sr.holder_slot,
                        fn_bits: sr.fn_bits,
                        field_slots: sfields,
                        consts: sconsts,
                        body: sbody,
                        callee_reg_count: scallee.reg_count,
                        win_off: super_win,
                    },
                );
            }
        }
        let win_top = if supers.is_empty() {
            reg_window + callee.reg_count
        } else {
            super_win + max_super_regs
        };
        let recv_ver = self.heap.version_of(ridx);
        Some((
            crate::codegen::MethodInlineShape {
                method_slot,
                recv_bits: recv.bits(),
                recv_ver,
                vals_ptr,
                field_slots,
                callee_reg_count: callee.reg_count,
                param_count: callee.param_count,
                body,
                consts,
                supers,
            },
            win_top,
        ))
    }

    /// Build one receiver arm for an ACCESSOR (getter/setter) inline (Stage 5):
    /// validate `recv` is a plain class instance with no own-shadow of `name`,
    /// resolve its TRIVIAL class getter/setter, bake the per-receiver guards +
    /// field slot(s). v1: NO super (Tri/Hex super-accessors decline → helper).
    /// zipp resolves class accessors via the class id (prototype reassignment
    /// ignored — verified JIT==NOJIT, model limit), so the receiver identity +
    /// version guard alone matches the interpreter (like methods; no class-version
    /// guard, and class redefinition keeps the old instance's old accessor).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_accessor_shape(
        &self,
        func_id: u32,
        ip: usize,
        recv: Value,
        name: &str,
        reg_window: u16,
        is_setter: bool,
    ) -> Option<(crate::codegen::MethodInlineShape, u16)> {
        use crate::heap::HeapObj;
        let ridx = recv.heap_index();
        if !self.ic_obj_ok(ridx) {
            return None;
        }
        let (recv_class, vals_ptr) = match self.heap.get(ridx) {
            HeapObj::Object(m) if !m.is_ctor => match m.class {
                Some(c) => (c, m.vals.as_ptr() as u64),
                None => return None,
            },
            _ => return None,
        };
        // G3b: an own property named `name` shadows the accessor → decline (the
        // recv-version guard catches a LATER own-add).
        if let HeapObj::Object(m) = self.heap.get(ridx) {
            if m.pos(name).is_some() {
                return None;
            }
        }
        let fid = if is_setter {
            self.ic_class_setter_fid(func_id, ip, recv_class)?
        } else {
            self.ic_class_getter_fid(func_id, ip, recv_class)?
        };
        let callee = self.func(fid as usize);
        // A GETTER body may read `super.v` (Stage 6) and a SETTER body may end
        // in `super.v = x` (Stage 7) — `method_inline_body_ok` holds the
        // per-op rules (the SuperSet is effectful, so last-op-only, and gated
        // on allow_setprop). A setter body may equally still end in its own
        // SetProp{obj:0} store (allow_setprop=is_setter).
        let body_len = Self::method_inline_body_ok(callee, true, is_setter)?;
        let body: Vec<Instr> = callee.code[..body_len].to_vec();
        let field_slots = self.mi_bake_fields(ridx, &body, &callee.string_constants)?;
        let consts = Self::mi_bake_consts(&callee.constants, &body);
        // ── bake each `super.v` read / `super.v = x` write in the body ──
        // Identical in shape to `build_method_shape`'s `super.m()` loop: the
        // resolved parent accessor runs over a sub-window with the SAME
        // receiver, so `mi_bake_fields` resolves its `this.<field>` against the
        // same instance. The two directions differ ONLY in the resolver — a
        // getter lives in the holder's `vals[slot]`, a setter in
        // `attrs[slot].setter` — and each resolver bakes the address its own
        // re-check must read (see `ic_super_setter_baked`).
        let super_win = reg_window + callee.reg_count;
        let mut supers = rustc_hash::FxHashMap::default();
        let mut max_super_regs = 0u16;
        for (bi, instr) in body.iter().enumerate() {
            let (sr, sname, is_store) = match *instr {
                Instr::SuperGet { home_class_id, name: sname, .. } => {
                    let skey = &callee.string_constants[sname as usize];
                    (self.ic_super_getter_baked(fid, bi, home_class_id, skey)?, sname, false)
                }
                Instr::SuperSet { home_class_id, name: sname, .. } => {
                    let skey = &callee.string_constants[sname as usize];
                    (self.ic_super_setter_baked(fid, bi, home_class_id, skey)?, sname, true)
                }
                _ => continue,
            };
            let _ = sname;
            let scallee = self.func(sr.fid as usize);
            // The parent accessor must itself be inlinable with NO nested
            // super. A parent SETTER ends in its `this.<field> = x` store
            // (allow_setprop), a parent GETTER performs no store.
            //
            // A class-syntax setter always has exactly one formal, but a
            // `defineProperty`-installed one is an arbitrary function — and the
            // emitter binds the value to sub-window reg 1 unconditionally. With
            // 0 formals reg 1 is a LOCAL, which must start undefined, not hold
            // the value; require exactly the one-param shape instead of
            // special-casing it.
            if is_store && scallee.param_count != 1 {
                return None;
            }
            let sblen = Self::method_inline_body_ok(scallee, false, is_store)?;
            let sbody: Vec<Instr> = scallee.code[..sblen].to_vec();
            let sfields = self.mi_bake_fields(ridx, &sbody, &scallee.string_constants)?;
            let sconsts = Self::mi_bake_consts(&scallee.constants, &sbody);
            max_super_regs = max_super_regs.max(scallee.reg_count);
            supers.insert(
                bi,
                crate::codegen::SuperInline {
                    epoch_ptr: &self.mi_class_epoch as *const u32 as u64,
                    epoch_val: self.mi_class_epoch,
                    hops: sr.hops,
                    holder_vals_ptr: sr.holder_vals_ptr,
                    holder_slot: sr.holder_slot,
                    fn_bits: sr.fn_bits,
                    field_slots: sfields,
                    consts: sconsts,
                    body: sbody,
                    callee_reg_count: scallee.reg_count,
                    win_off: super_win,
                },
            );
        }
        let win_top = if supers.is_empty() {
            reg_window + callee.reg_count
        } else {
            super_win + max_super_regs
        };
        let recv_ver = self.heap.version_of(ridx);
        Some((
            crate::codegen::MethodInlineShape {
                method_slot: None,
                recv_bits: recv.bits(),
                recv_ver,
                vals_ptr,
                field_slots,
                callee_reg_count: callee.reg_count,
                param_count: callee.param_count,
                body,
                consts,
                supers,
            },
            win_top,
        ))
    }

    /// Resolve every `this.<field>` (GetProp/SetProp `obj:0`) in `body` to the
    /// live receiver's own DATA slot (a store also requires it be WRITABLE).
    /// `None` if any field is missing / an accessor / (store) non-writable / the
    /// receiver isn't a plain Object (decline the inline).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn mi_bake_fields(
        &self,
        ridx: u32,
        body: &[Instr],
        strconsts: &[String],
    ) -> Option<rustc_hash::FxHashMap<u32, u32>> {
        let m = match self.heap.get(ridx) {
            crate::heap::HeapObj::Object(m) => m,
            _ => return None,
        };
        let mut fs = rustc_hash::FxHashMap::default();
        for instr in body {
            // GetProp{obj:0} reads need a non-accessor slot; SetProp{obj:0} (a
            // setter's store) needs a non-accessor WRITABLE slot.
            let (fname, need_writable) = match *instr {
                Instr::GetProp { obj: 0, name: fname, .. } => (fname, false),
                Instr::SetProp { obj: 0, name: fname, .. } => (fname, true),
                _ => continue,
            };
            let fkey = &strconsts[fname as usize];
            match m.pos(fkey) {
                Some(s) if !m.attrs[s].accessor && (!need_writable || m.attrs[s].writable) => {
                    fs.insert(fname, s as u32);
                }
                _ => return None,
            }
        }
        Some(fs)
    }

    /// Pre-resolve the numeric-constant bits a body's `LoadConst` ops read.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn mi_bake_consts(
        consts: &[Value],
        body: &[Instr],
    ) -> rustc_hash::FxHashMap<u32, u64> {
        let mut c = rustc_hash::FxHashMap::default();
        for instr in body {
            if let Instr::LoadConst { idx, .. } = *instr {
                if let Some(v) = consts.get(idx as usize) {
                    c.insert(idx, v.bits());
                }
            }
        }
        c
    }

    /// Is the register a `SuperBase` at `at` writes unread by every LATER op in
    /// `body`, except as the `base` field of a `Super*` op?
    ///
    /// `emit_mi_body` drops `SuperBase` — the inlined `SuperMethod`/`SuperGet`/
    /// `SuperSet` arms resolve through their baked plan and never dereference
    /// `base`, so the capture has no inlined consumer. This is the proof
    /// obligation for that: it enumerates the reads of exactly the ops
    /// [`Self::method_inline_body_ok`] admits, with the `base` fields left out on
    /// purpose, and anything it does not recognise counts as a read. So growing
    /// that whitelist without revisiting this makes the body DECLINE, never
    /// silently read a register the emitter left stale.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_super_base_dst_dead(body: &[Instr], at: usize, dst: u16) -> bool {
        use crate::bytecode::Instr as I;
        for instr in &body[(at + 1).min(body.len())..] {
            let reads_dst = match *instr {
                I::LoadInt { .. } | I::LoadBool { .. } | I::LoadConst { .. } => false,
                I::Move { src, .. } => src == dst,
                I::GetProp { obj, .. } => obj == dst,
                I::Add { a, b, .. }
                | I::Sub { a, b, .. }
                | I::Mul { a, b, .. }
                | I::Div { a, b, .. }
                | I::Mod { a, b, .. }
                | I::Bitwise { a, b, .. } => a == dst || b == dst,
                I::AddInt { a, .. } | I::Neg { a, .. } => a == dst,
                // `base` is deliberately NOT compared: the inlined emission
                // ignores it. The argument window is a real read.
                I::SuperMethod { arg_base, argc, .. } => {
                    (0..argc).any(|k| arg_base + k == dst)
                }
                I::SuperGet { .. } => false,
                I::SuperSet { val, .. } => val == dst,
                // A second capture in the same body may reuse the temp; it writes,
                // and reads nothing.
                I::SuperBase { .. } => false,
                I::SetProp { obj, val, .. } => obj == dst || val == dst,
                I::Return { src } => src == dst,
                I::ReturnUndefined => false,
                _ => true,
            };
            if reads_dst {
                return false;
            }
        }
        true
    }

    /// Trivial-method body scan for the Q7 in-region emitter. Returns the body
    /// length (ops up to and incl. the first `Return`/`ReturnUndefined`), or
    /// `None` to decline. `allow_super` admits `SuperMethod` (the outer body); a
    /// super TARGET is scanned with `allow_super=false` (v1 has no nested super).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn method_inline_body_ok(
        p: &crate::bytecode::FuncProto,
        allow_super: bool,
        allow_setprop: bool,
    ) -> Option<usize> {
        use crate::bytecode::Instr as I;
        if p.is_generator || p.is_async {
            return None;
        }
        if p.rest_reg.is_some() || p.arguments_reg.is_some() {
            return None;
        }
        // Bound the scratch window (≤16, matching the leaf inliner's headroom).
        if p.reg_count > 16 {
            return None;
        }
        let code = &p.code;
        let term = code
            .iter()
            .position(|i| matches!(i, I::Return { .. } | I::ReturnUndefined))?;
        for (ix, instr) in code[..term].iter().enumerate() {
            match *instr {
                I::LoadInt { .. } | I::LoadBool { .. } | I::Move { .. } => {}
                I::LoadConst { idx, .. } => match p.constants.get(idx as usize) {
                    Some(c) if c.is_number() => {}
                    _ => return None,
                },
                I::GetProp { obj: 0, .. } => {}
                I::Add { .. }
                | I::Sub { .. }
                | I::Mul { .. }
                | I::Div { .. }
                | I::Mod { .. }
                | I::AddInt { .. }
                | I::Neg { .. }
                | I::Bitwise { .. } => {}
                // `super.m()` admitted only in the outer body (Stage 3); the
                // resolved super target is re-scanned with allow_super=false.
                I::SuperMethod { .. } if allow_super => {}
                // `GetSuperBase` — the base capture the compiler plants ahead of
                // every `super.m()` and `super.x = v`, because
                // MakeSuperPropertyReference must read the home object's
                // [[Prototype]] BEFORE the argument list / RHS runs. The inlined
                // Super* arms resolve through their BAKED plan (class epoch +
                // per-hop versions + a holder-slot re-read) and never read `base`,
                // so the register this writes is dead in an inlined body and
                // `emit_mi_body` drops the op. `mi_super_base_dst_dead` PROVES
                // that instead of assuming it: a future admitted op that reads the
                // register declines here rather than silently reading a stale slot.
                //
                // Its absence was a 6x regression. `SuperBase` arrived with the
                // MakeSuperPropertyReference ordering fix; the off-frame evaluator
                // (`method_body_inlinable_scan`) was taught the op and this scan
                // was not, so EVERY `super`-using method body declined here. That
                // took class-prototype-hot's 32M polymorphic `objs[i&3].area()`
                // calls off the native inline path and onto two nested frame calls
                // — 3.1ns -> 56ns per call, and the row from 1.27x to 7.99x.
                I::SuperBase { dst, .. }
                    if allow_super && Self::mi_super_base_dst_dead(&code[..term], ix, dst) => {}
                // `super.v` READ inside a class getter (Stage 6), under the same
                // rule: outer body only, and the resolved super getter is
                // re-scanned with the flag off so there is no nested super.
                I::SuperGet { .. } if allow_super => {}
                // `super.v = x` inside a class SETTER (Stage 7). Effectful — the
                // parent setter's body commits a store — so it obeys the same
                // last-op rule as the plain `this.<field> = val` store below: no
                // later op may bail after the effect commits. Gated on
                // allow_setprop too, so a GETTER body writing `super.v` (legal,
                // bizarre) stays on the helper.
                I::SuperSet { .. } if allow_super && allow_setprop && ix + 1 == term => {}
                // A setter's `this.<field> = val` store (Stage 5): the body's ONLY
                // effect, so it must be the LAST op before the terminator (no later
                // op can decline AFTER the store commits — the no-deopt-after-effect
                // rule). emit_mi_body handles `obj: 0` only.
                I::SetProp { obj: 0, .. } if allow_setprop && ix + 1 == term => {}
                // Rejects SuperGet/SuperSet/MathOp/GetIndex/non-last-SetProp/calls.
                _ => return None,
            }
        }
        Some(term + 1)
    }

    /// Would growing `self.regs` to `needed` slots exceed the pinned capacity?
    /// (Interpreter-only builds: never — there is no pinned native pointer to
    /// protect, so the Vec may grow/reallocate freely.)
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn regs_would_overflow(&self, needed: usize) -> bool {
        self.reg_capacity != 0 && needed > self.reg_capacity
    }

    /// The pinned register-file capacity (slots) for the Q4 leaf-inline headroom
    /// check in `jit_regs_fits`. The reserved capacity never changes after
    /// `reserve_jit_regs`, so a scratch window inside it can't trigger a realloc.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn reg_capacity_pub(&self) -> usize {
        self.reg_capacity
    }
    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    #[inline]
    pub(crate) fn regs_would_overflow(&self, _needed: usize) -> bool {
        false
    }

    /// Raise the initialized-slots high-water mark to `needed` after a frame
    /// window has been pushed. Only the native self-call path reads `regs_hw`
    /// (to expose an already-initialized window with `set_len` instead of a
    /// zero-filling `resize`), so on an interpreter-only build — no JIT feature,
    /// or any target that is not x86-64 — this is a no-op and the field does not
    /// exist at all.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn bump_regs_hw(&mut self, needed: usize) {
        if needed > self.regs_hw {
            self.regs_hw = needed;
        }
    }

    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    #[inline]
    pub(crate) fn bump_regs_hw(&mut self, _needed: usize) {}

}

/// Offset every REGISTER operand of a leaf-body instruction by `off`, so a
/// spliced inner body occupies a window above the wrapper's own registers and the
/// two can never alias. Returns `None` for anything outside the leaf subset —
/// that is a decline, never a silent mis-shift, because a missed operand would be
/// a wrong-register read.
///
/// Non-register fields are copied through, EXCEPT a `LoadConst` pool index,
/// which is biased by `const_off`: the spliced body's constants come from the
/// INNER function's pool, and the plan's single `consts` map is keyed by index,
/// so the two pools have to be given disjoint key ranges. Getting this wrong is
/// silent — the emitter falls back to zero bits, the op bails, and the call just
/// re-runs in the interpreter. `arg_base` IS a register (a window base).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn shift_leaf_regs(i: &Instr, off: u16, const_off: u32) -> Option<Instr> {
    let s = |r: u16| r + off;
    let c = |i: u32| i + const_off;
    Some(match *i {
        Instr::LoadInt { dst, val } => Instr::LoadInt { dst: s(dst), val },
        Instr::LoadConst { dst, idx } => Instr::LoadConst { dst: s(dst), idx: c(idx) },
        Instr::LoadBool { dst, val } => Instr::LoadBool { dst: s(dst), val },
        Instr::LoadUndefined { dst } => Instr::LoadUndefined { dst: s(dst) },
        Instr::Move { dst, src } => Instr::Move { dst: s(dst), src: s(src) },
        Instr::LoadGlobal { dst, idx } => Instr::LoadGlobal { dst: s(dst), idx },
        Instr::StoreGlobal { idx, src } => Instr::StoreGlobal { idx, src: s(src) },
        Instr::StoreGlobalStrict { idx, src } => Instr::StoreGlobalStrict { idx, src: s(src) },
        Instr::StoreGlobalResolved { idx, src } => Instr::StoreGlobalResolved { idx, src: s(src) },
        Instr::Add { dst, a, b } => Instr::Add { dst: s(dst), a: s(a), b: s(b) },
        Instr::Sub { dst, a, b } => Instr::Sub { dst: s(dst), a: s(a), b: s(b) },
        Instr::Mul { dst, a, b } => Instr::Mul { dst: s(dst), a: s(a), b: s(b) },
        Instr::Div { dst, a, b } => Instr::Div { dst: s(dst), a: s(a), b: s(b) },
        Instr::Mod { dst, a, b } => Instr::Mod { dst: s(dst), a: s(a), b: s(b) },
        Instr::AddInt { dst, a, imm, upd } => Instr::AddInt { dst: s(dst), a: s(a), imm, upd },
        Instr::Neg { dst, a } => Instr::Neg { dst: s(dst), a: s(a) },
        Instr::Bitwise { dst, a, b, op } => Instr::Bitwise { dst: s(dst), a: s(a), b: s(b), op },
        Instr::Eq { dst, a, b } => Instr::Eq { dst: s(dst), a: s(a), b: s(b) },
        Instr::Ne { dst, a, b } => Instr::Ne { dst: s(dst), a: s(a), b: s(b) },
        Instr::Lt { dst, a, b } => Instr::Lt { dst: s(dst), a: s(a), b: s(b) },
        Instr::Le { dst, a, b } => Instr::Le { dst: s(dst), a: s(a), b: s(b) },
        Instr::Gt { dst, a, b } => Instr::Gt { dst: s(dst), a: s(a), b: s(b) },
        Instr::Ge { dst, a, b } => Instr::Ge { dst: s(dst), a: s(a), b: s(b) },
        Instr::GetIndex { dst, obj, key } => Instr::GetIndex { dst: s(dst), obj: s(obj), key: s(key) },
        Instr::CallMethod { dst, obj, name, arg_base, argc } => {
            Instr::CallMethod { dst: s(dst), obj: s(obj), name, arg_base: s(arg_base), argc }
        }
        Instr::MathOp { dst, op, arg_base, argc } => {
            Instr::MathOp { dst: s(dst), op, arg_base: s(arg_base), argc }
        }
        Instr::UpvalGet { dst, idx } => Instr::UpvalGet { dst: s(dst), idx },
        Instr::UpvalSet { idx, src } => Instr::UpvalSet { idx, src: s(src) },
        _ => return None,
    })
}
