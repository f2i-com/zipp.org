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
            if let Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } = *ins {
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
                // `str.length` on the SAME receiver coalesces onto the charCodeAt
                // STR pin (the snapshot's `units` IS the length), so a `for (i <
                // str.length) str.charCodeAt(i)` loop guard resolves via the pin
                // instead of a GetProp IC — letting the whole loop run unboxed.
                Instr::GetProp { obj, name, .. }
                    if proto
                        .string_constants
                        .get(name as usize)
                        .is_some_and(|k| k == "length") =>
                {
                    (obj, Recv::Str)
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
                (HeapObj::Array(_), Recv::Ta)
                    if !self.arr_props.contains_key(&live.heap_index())
                        && !self.arguments_objs.contains_key(&live.heap_index()) =>
                {
                    crate::codegen::ARR_PIN_KIND
                }
                (HeapObj::DataView { .. }, Recv::Dv) => crate::codegen::DV_PIN_KIND,
                // Pin only a FLAT ASCII string — the inline byte load needs
                // byte i == UTF-16 unit i (a rope/non-ASCII string snapshots
                // zero and falls to the generic helper, so a wrong pin is safe;
                // we just skip pinning it here when it can't help).
                (HeapObj::Str(js), Recv::Str) if js.is_ascii() => crate::codegen::STR_PIN_KIND,
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
            // The inline emitter forces `this = undefined` for the callee window.
            // That is ONLY correct for a STRICT, NON-ARROW leaf (its `this` is
            // genuinely undefined when called as `f(args)`). An arrow captures
            // `this` lexically (stored in the Closure, NOT undefined); a sloppy
            // function substitutes the global object (OrdinaryCallBindThis). Both
            // would observe the wrong `this` if inlined — decline them.
            if callee.lexical_this || !callee.is_strict {
                if log {
                    eprintln!(
                        "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE \
                         (lexical_this={} strict={})",
                        callee.lexical_this, callee.is_strict
                    );
                }
                continue;
            }
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
            if shapes.is_empty() {
                continue;
            }
            if log {
                let k = match kind {
                    MiKind::Method => "method",
                    MiKind::Getter => "getter",
                    MiKind::Setter => "setter",
                };
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
        // Receiver must be a plain (non-ctor) class instance.
        let (recv_class, vals_ptr) = match self.heap.get(ridx) {
            HeapObj::Object(m) if !m.is_ctor => match m.class {
                Some(c) => (c, m.vals.as_ptr() as u64),
                None => return None,
            },
            _ => return None,
        };
        // G3b: an own property shadowing the method name → resolve to the own
        // prop, not the class method — decline this arm.
        if let HeapObj::Object(m) = self.heap.get(ridx) {
            if m.pos(key).is_some() {
                return None;
            }
        }
        let fid = self.ic_class_method_fid(func_id, ip, recv_class)?;
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
        // v1 accessors: NO super (allow_super=false); a setter body ends in a
        // SetProp{obj:0} store (allow_setprop=is_setter).
        let body_len = Self::method_inline_body_ok(callee, false, is_setter)?;
        let body: Vec<Instr> = callee.code[..body_len].to_vec();
        let field_slots = self.mi_bake_fields(ridx, &body, &callee.string_constants)?;
        let consts = Self::mi_bake_consts(&callee.constants, &body);
        let recv_ver = self.heap.version_of(ridx);
        Some((
            crate::codegen::MethodInlineShape {
                recv_bits: recv.bits(),
                recv_ver,
                vals_ptr,
                field_slots,
                callee_reg_count: callee.reg_count,
                param_count: callee.param_count,
                body,
                consts,
                supers: rustc_hash::FxHashMap::default(),
            },
            reg_window + callee.reg_count, // no super → window is just the body
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
