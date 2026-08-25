// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn computed_drop_obj_is_observable(
    code: &[Instr],
    obj: u16,
    def_ip: usize,
    call_ip: usize,
    region_start: usize,
    region_end: usize,
) -> bool {
    let used_between = code[def_ip + 1..call_ip]
        .iter()
        .any(|ins| crate::codegen::instr_uses(ins).contains(&obj));
    // Conservative by design: even a later def could make a narrower proof
    // possible, but rejecting every post-call in-region use ensures the
    // object-valued definition is never erased while the native success path
    // can still observe its slot.
    let used_after = code[call_ip + 1..=region_end]
        .iter()
        .any(|ins| crate::codegen::instr_uses(ins).contains(&obj));
    let used_outside = code
        .iter()
        .enumerate()
        .filter(|(at, _)| *at < region_start || *at > region_end)
        .any(|(_, ins)| crate::codegen::instr_uses(ins).contains(&obj));
    used_between || used_after || used_outside
}

#[cfg(all(test, feature = "jit", target_arch = "x86_64"))]
mod computed_drop_liveness_tests {
    use super::*;

    #[test]
    fn post_call_receiver_use_prevents_dropping_its_definition() {
        let code = vec![
            Instr::LoadInt { dst: 1, val: 0 },
            Instr::Move { dst: 9, src: 2 },
            Instr::LoadInt { dst: 3, val: 0 },
            Instr::CallMethodComputed {
                dst: 4,
                obj: 9,
                key: 3,
                arg_base: 5,
                argc: 1,
            },
            Instr::Move { dst: 6, src: 9 },
            Instr::Jump { target: 1 },
        ];
        assert!(computed_drop_obj_is_observable(&code, 9, 1, 3, 1, 5));

        let mut no_post_use = code;
        no_post_use[4] = Instr::Move { dst: 6, src: 4 };
        assert!(!computed_drop_obj_is_observable(
            &no_post_use,
            9,
            1,
            3,
            1,
            5
        ));
    }
}

/// `ZIPP_JITDECLINE` names the constraint a nested-leaf splice failed on —
/// B75's survey showed a bare "Call" (and then a bare "wrapper's inner call not
/// inlinable") cannot choose between the possible generalisations.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn nested_reject(why: &str) {
    if std::env::var_os("ZIPP_JITDECLINE").is_some() {
        eprintln!("[nested-reject] {why}");
    }
}

/// `ZIPP_NO_PROTO_METHOD_INLINE=1` drops the B78 prototype-chain arm back out of
/// `build_method_shape`, leaving the class and own-slot arms untouched. Exists so
/// the change can be A/B'd with `tools/bench.py --ab-env` against ONE binary,
/// which removes the fat-LTO code-layout confound §2 warns about and that B77 was
/// reverted for. Cached, because this is read once per candidate receiver per
/// region compile. (`ZIPP_NO_METHOD_INLINE=1` remains the whole-mechanism switch;
/// it is no use for attribution, since it also kills the class and own-slot arms.)
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn proto_method_inline_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_PROTO_METHOD_INLINE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// W20 (M2): `ZIPP_NO_OWN_ACCESSOR_INLINE=1` drops the OWN-accessor arm back out
/// of `build_accessor_shape`, leaving the class arm exactly as it was -- i.e. OFF
/// reproduces pre-wave behaviour byte-identically, since the own-slot receivers
/// this admits are precisely the ones that used to `return None`. Exists so the
/// mechanism can be A/B'd with `tools/bench.py --ab-env` against ONE binary,
/// which removes the fat-LTO code-layout confound a two-binary A/B carries.
/// Memoized and read at PLAN time only (once per candidate receiver per region
/// compile), never on a hot path.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn own_accessor_inline_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_OWN_ACCESSOR_INLINE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Recognise one exact, effect-free scanner leaf. The match is intentionally
/// instruction-for-instruction: widening any operand relation without also
/// widening the helper's replay proof must fail closed to the generic leaf.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn span_code_unit_pred_plan(
    callee: &crate::bytecode::FuncProto,
    body: &[Instr],
    argc: u16,
) -> Option<crate::codegen::SpanCodeUnitPredPlan> {
    if !crate::codegen::span_code_unit_pred_enabled()
        || argc != 2
        || callee.param_count != 2
        || body.len() != 19
    {
        return None;
    }
    let load_global = |i: &Instr| match i {
        Instr::LoadGlobal { dst, idx } => Some((*dst, *idx)),
        _ => None,
    };
    let get_index = |i: &Instr| match i {
        Instr::GetIndex { dst, obj, key } => Some((*dst, *obj, *key)),
        _ => None,
    };
    let load_int = |i: &Instr| match i {
        Instr::LoadInt { dst, val } => Some((*dst, *val)),
        _ => None,
    };
    let eq = |i: &Instr| match i {
        Instr::Eq { dst, a, b } => Some((*dst, *a, *b)),
        _ => None,
    };
    let jump_false = |i: &Instr| match i {
        Instr::JumpIfFalse { cond, target } => Some((*cond, *target)),
        _ => None,
    };

    let (tags_obj, tags_g) = load_global(&body[0])?;
    let (kind, obj, index) = get_index(&body[1])?;
    let (four, 4) = load_int(&body[2])? else {
        return None;
    };
    let (first_flag, a, b) = eq(&body[3])?;
    let (cond, 12) = jump_false(&body[4])? else {
        return None;
    };
    if obj != tags_obj || index != 1 || a != kind || b != four || cond != first_flag {
        return None;
    }

    let (ends_obj, ends_g) = load_global(&body[5])?;
    let (end, obj, key) = get_index(&body[6])?;
    let (starts_obj, starts_g) = load_global(&body[7])?;
    let (start, obj2, key2) = get_index(&body[8])?;
    let (span, sub_a, sub_b) = match &body[9] {
        Instr::Sub { dst, a, b } => (*dst, *a, *b),
        _ => return None,
    };
    let (one, 1) = load_int(&body[10])? else {
        return None;
    };
    let (span_flag, eq_a, eq_b) = eq(&body[11])?;
    let (cond, 18) = jump_false(&body[12])? else {
        return None;
    };
    if obj != ends_obj
        || key != 1
        || obj2 != starts_obj
        || key2 != 1
        || sub_a != end
        || sub_b != start
        || eq_a != span
        || eq_b != one
        || cond != span_flag
    {
        return None;
    }

    let (source_obj, source_g) = load_global(&body[13])?;
    let (starts_obj2, starts_g2) = load_global(&body[14])?;
    let (start2, obj, key) = get_index(&body[15])?;
    let (unit, call_obj, name, arg_base, call_argc) = match &body[16] {
        Instr::CallMethod {
            dst,
            obj,
            name,
            arg_base,
            argc,
        } => (*dst, *obj, *name, *arg_base, *argc),
        _ => return None,
    };
    let (ret_flag, eq_a, eq_b) = eq(&body[17])?;
    let ret = match &body[18] {
        Instr::Return { src } => *src,
        _ => return None,
    };
    if starts_g2 != starts_g
        || starts_obj2 == starts_obj
        || obj != starts_obj2
        || key != 1
        || call_obj != source_obj
        || arg_base != start2
        || call_argc != 1
        || callee
            .string_constants
            .get(name as usize)
            .map(String::as_str)
            != Some("charCodeAt")
        || eq_a != unit
        || eq_b != 2
        || ret != ret_flag
    {
        return None;
    }

    let globals = [tags_g, ends_g, starts_g, source_g];
    if globals.iter().any(|&g| g > u16::MAX as u32) {
        return None;
    }
    let packed_globals = (globals[0] as u64)
        | ((globals[1] as u64) << 16)
        | ((globals[2] as u64) << 32)
        | ((globals[3] as u64) << 48);
    Some(crate::codegen::SpanCodeUnitPredPlan {
        packed_globals,
        helper: crate::vm::helpers_misc::jit_span_code_unit_pred as usize,
        pair: None,
    })
}

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
                | Instr::StrAppendIndex { dst, .. }
                | Instr::AddRightPair { dst, .. }
                | Instr::Pad2Concat { dst, .. }
                | Instr::Pad2Conditional { dst, .. }
                | Instr::StrConcatChain { dst, .. }
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
            | Instr::StoreGlobalResolved { idx, .. } = *ins
            {
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
            /// as: a flat-ASCII string (pin `units`), a dense Array (pin `len`),
            /// or a pristine TypedArray (a distinct length-only TA marker).
            /// All snapshots carry the exact length in the same slot.
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
                Instr::HasProp {
                    obj, brand: false, ..
                } => (obj, Recv::Ta),
                // A whitelisted DataView `get*` receiver pins the same way
                // (snapshot: data+byteOffset / byteLength).
                Instr::CallMethod {
                    obj, name, argc, ..
                } if (argc == 1 || argc == 2)
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
                Instr::CallMethod {
                    obj, name, argc, ..
                } if argc == 1
                    && proto
                        .string_constants
                        .get(name as usize)
                        .is_some_and(|k| k == "charCodeAt") =>
                {
                    (obj, Recv::Str)
                }
                // W20 M2: an `arr.push(x)` receiver pins as a dense Array on
                // exactly the same terms as `arr[i]` — the live receiver picks
                // the kind, and an all-Int one (`ARR_INT_PIN_KIND`) is what
                // lets the INTEGER tier host the append inline instead of
                // declining the whole region over one `CallMethod`. The pin is
                // ALSO what keeps the receiver register out of the numeric home
                // set (`ta_recv_regs`); without it a `LoadGlobal` of the array
                // would be typed Num and homed as an i64. Gated so
                // `ZIPP_NO_INT_PUSH=1` leaves every plan in the suite
                // bit-identical, this arm included.
                Instr::CallMethod {
                    obj, name, argc, ..
                } if crate::codegen::int_push_enabled()
                    && argc == 1
                    && proto
                        .string_constants
                        .get(name as usize)
                        .is_some_and(|k| k == "push") =>
                {
                    (obj, Recv::Ta)
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
                TaPinSrc::Global(g) => self
                    .globals
                    .get(g as usize)
                    .copied()
                    .unwrap_or(Value::UNDEFINED),
                TaPinSrc::Reg(r) => self.get(base, r),
            };
            if !live.is_heap() {
                continue;
            }
            let kind = match (self.heap.get(live.heap_index()), &recv) {
                (HeapObj::TypedArray { kind, .. }, Recv::Ta) if *kind < 9 => *kind,
                // TypedArray `length` is an INHERITED accessor, unlike Array's
                // own exotic `length`. A direct snapshot is legal only while
                // the live instance/prototype chain still resolves to the
                // pristine intrinsic getter. Recheck the same proof in
                // `jit_ta_snapshot` at every region entry so an override after
                // OSR planning becomes an exact-ip deopt, never a stale answer.
                //
                // A local RAB (fixed or length-tracking) is safe: the INT body
                // contains no arbitrary call, and the snapshot is re-derived on
                // every entry. A length-tracking GSAB can grow concurrently,
                // however, so its length cannot be cached even for one native
                // run and is declined both here and by the snapshot helper.
                (HeapObj::TypedArray { buffer, kind, .. }, Recv::Len) => {
                    let idx = live.heap_index();
                    let Some(marker) = crate::codegen::ta_len_pin_kind(*kind) else {
                        continue;
                    };
                    if !self.ta_length_is_intrinsic(idx)
                        || self.ta_effective_len(idx).is_none()
                        || (self.ta_tracking.contains(&idx) && self.shared_buffers.contains(buffer))
                    {
                        continue;
                    }
                    marker
                }
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
                        // Not all-Int, but possibly all-NUMBER (an array of
                        // doubles). Same bounded sample; offers the array to the
                        // DOUBLE tier, which unboxes the element into an f64 home.
                        // Without this middle kind the double tier either excludes
                        // arrays of doubles or admits arrays of OBJECTS and
                        // entry-bails forever (see `ARR_NUM_PIN_KIND`).
                        let all_num = items[..head].iter().all(|v| v.is_number())
                            && (n <= head || {
                                let step = (n - head).div_ceil(64).max(1);
                                items[head..].iter().step_by(step).all(|v| v.is_number())
                            });
                        if all_num {
                            crate::codegen::ARR_NUM_PIN_KIND
                        } else {
                            crate::codegen::ARR_PIN_KIND
                        }
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
            let slot = match plan
                .pins
                .iter()
                .position(|p| p.src == src && p.kind == kind)
            {
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

    /// Build the bounded dense-array plan consumed only by the INTEGER splice.
    /// This is intentionally narrower than the MEMORY computed-call helper:
    /// every covered own element must be an exact capture-free, branch-free,
    /// pure numeric leaf, and its typed lane must prove the caller arguments and
    /// result can stay in integer homes. Richer keys/callees retain the existing
    /// helper/interpreter path.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_dense_computed_leaf_plan(
        &self,
        func_id: u32,
        start: u32,
        end: u32,
        base: usize,
    ) -> rustc_hash::FxHashMap<usize, crate::codegen::DenseComputedLeafPlan> {
        use crate::codegen::{DenseComputedLeafPlan, LeafInlinePlan, TaPinSrc};
        use crate::heap::HeapObj;
        use rustc_hash::FxHashMap;

        const MAX_ARMS: usize = 4;
        let mut plans = FxHashMap::default();
        if !crate::codegen::int_computed_leaf_enabled()
            || !crate::codegen::int_splice_enabled()
            || !crate::codegen::typed_splice_enabled()
        {
            return plans;
        }
        #[cfg(feature = "instrument")]
        if self.jit.metered() {
            return plans;
        }

        // Exact subset `int_splice::map_body_instr` can flatten. Requiring every
        // operand (including destinations) to be nonzero proves the body never
        // observes or overwrites callee register 0 (`this`). That single rule
        // makes ordinary-method and lexical-arrow binding differences irrelevant
        // without pretending the receiver itself is `undefined`.
        fn pure_int_body(body: &[Instr]) -> bool {
            body.iter().all(|ins| match *ins {
                Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } => dst != 0,
                Instr::Move { dst, src } => dst != 0 && src != 0,
                Instr::Add { dst, a, b }
                | Instr::Sub { dst, a, b }
                | Instr::Mul { dst, a, b }
                | Instr::Mod { dst, a, b }
                | Instr::Bitwise { dst, a, b, .. }
                | Instr::Eq { dst, a, b }
                | Instr::Ne { dst, a, b }
                | Instr::Lt { dst, a, b }
                | Instr::Le { dst, a, b }
                | Instr::Gt { dst, a, b }
                | Instr::Ge { dst, a, b } => dst != 0 && a != 0 && b != 0,
                Instr::AddInt { dst, a, .. } | Instr::Neg { dst, a } => dst != 0 && a != 0,
                Instr::MathOp {
                    dst,
                    op: crate::bytecode::MathFn::Imul,
                    arg_base,
                    argc: 2,
                } => dst != 0 && arg_base != 0,
                Instr::Return { src } => src != 0,
                _ => false,
            })
        }

        let caller = self.func(func_id as usize);
        let (s, e) = (start as usize, end as usize);
        if e <= s || e >= caller.code.len() {
            return plans;
        }
        let log = std::env::var_os("ZIPP_JITLOG").is_some();
        let stored_globals: rustc_hash::FxHashSet<u32> = caller.code[s..=e]
            .iter()
            .filter_map(|ins| match *ins {
                Instr::StoreGlobal { idx, .. }
                | Instr::StoreGlobalStrict { idx, .. }
                | Instr::StoreGlobalResolved { idx, .. } => Some(idx),
                _ => None,
            })
            .collect();

        for ip in s..=e {
            let Instr::CallMethodComputed {
                obj,
                arg_base,
                argc,
                ..
            } = caller.code[ip]
            else {
                continue;
            };

            // Resolve the receiver copied into `obj` back to a source whose
            // Value is invariant for the whole native region. Every in-region
            // link must dominate the next one; a jump into its open gap would
            // permit an old frame-slot value and therefore declines.
            let mut reg = obj;
            let mut cursor = ip;
            let mut fallback_ip = ip;
            let mut drop_obj_def = None;
            let mut seen = rustc_hash::FxHashSet::default();
            let recv_src = 'source: loop {
                if !seen.insert(reg) {
                    break 'source None;
                }
                let nearest = (s..cursor)
                    .rev()
                    .find(|&j| crate::codegen::writes_reg(&caller.code[j]) == Some(reg));
                let Some(def_ip) = nearest else {
                    // A true live-in is usable only when no later region op can
                    // replace it before a subsequent loop iteration.
                    if caller.code[s..=e]
                        .iter()
                        .any(|ins| crate::codegen::writes_reg(ins) == Some(reg))
                    {
                        break 'source None;
                    }
                    break 'source Some(TaPinSrc::Reg(reg));
                };
                if reg == obj && cursor == ip {
                    drop_obj_def = Some(def_ip);
                }
                if caller.code.iter().any(|ins| {
                    slot_guard_jump_target(ins)
                        .is_some_and(|t| (def_ip + 1..=cursor).contains(&(t as usize)))
                }) {
                    break 'source None;
                }
                fallback_ip = fallback_ip.min(def_ip);
                match caller.code[def_ip] {
                    Instr::Move { dst, src } if dst == reg => {
                        reg = src;
                        cursor = def_ip;
                    }
                    Instr::LoadGlobal { dst, idx }
                        if dst == reg && !stored_globals.contains(&idx) =>
                    {
                        break 'source Some(TaPinSrc::Global(idx));
                    }
                    _ => break 'source None,
                }
            };
            let Some(recv_src) = recv_src else {
                if log {
                    eprintln!("[int-computed] fn{func_id}@{ip} DECLINE unstable receiver");
                }
                continue;
            };
            if let Some(def_ip) = drop_obj_def {
                // The success path omits this object-valued def (integer homes
                // cannot represent it). It must be a truly transient call temp:
                // no intervening or out-of-region read may observe the slot.
                if computed_drop_obj_is_observable(&caller.code, obj, def_ip, ip, s, e) {
                    if log {
                        eprintln!(
                            "[int-computed] fn{func_id}@{ip} DECLINE receiver temp is observable"
                        );
                    }
                    continue;
                }
            }
            let recv = match recv_src {
                TaPinSrc::Global(g) => self
                    .globals
                    .get(g as usize)
                    .copied()
                    .unwrap_or(Value::UNDEFINED),
                TaPinSrc::Reg(r) => self.get(base, r),
            };
            if !recv.is_heap()
                || self.arguments_objs.contains_key(&recv.heap_index())
                || self.array_elements_overlaid(recv.heap_index())
            {
                continue;
            }
            let items = match self.heap.get(recv.heap_index()) {
                HeapObj::Array(items) if !items.is_empty() && items.len() <= MAX_ARMS => items,
                _ => continue,
            };
            if items.iter().any(|v| v.is_hole()) {
                continue;
            }

            let mut variants = Vec::with_capacity(items.len());
            let mut failed = false;
            for (index, &callee_value) in items.iter().enumerate() {
                if self
                    .array_index_override(recv.heap_index(), index)
                    .is_some()
                {
                    failed = true;
                    break;
                }
                let Some((fid, _closure)) = self.ic_plain_fn(callee_value) else {
                    failed = true;
                    break;
                };
                // Dynamic/foreign program ids can share an integer fid with a
                // root function. Keep this first lane root-realm only.
                if self.get_function_realm(callee_value) != 0
                    || self.program.functions.get(fid as usize).is_none()
                {
                    failed = true;
                    break;
                }
                let callee = self.func(fid as usize);
                if !callee.upvalues.is_empty() || callee.param_count != argc {
                    failed = true;
                    break;
                }
                let Some((body, default_arg_mask)) =
                    crate::codegen::callee_leaf_ok_for_call(callee, argc)
                else {
                    failed = true;
                    break;
                };
                if default_arg_mask != 0
                    || !matches!(body.last(), Some(Instr::Return { .. }))
                    || !pure_int_body(&body)
                    || body.iter().any(|ins| match *ins {
                        Instr::LoadConst { idx, .. } => !callee
                            .constants
                            .get(idx as usize)
                            .is_some_and(|v| v.is_int()),
                        _ => false,
                    })
                {
                    failed = true;
                    break;
                }
                let mut consts = FxHashMap::default();
                for ins in &body {
                    if let Instr::LoadConst { idx, .. } = *ins {
                        consts.insert(idx, callee.constants[idx as usize].bits());
                    }
                }
                let uninit_mask = crate::codegen::splice_uninit_mask(
                    &body,
                    callee.reg_count as usize,
                    callee.param_count as usize,
                );
                if uninit_mask != 0 {
                    failed = true;
                    break;
                }
                let Some(defs) = crate::codegen::splice_body_defs(&body) else {
                    failed = true;
                    break;
                };
                let mut alias_params = 0u64;
                if crate::codegen::splice_alias_enabled() {
                    for p in 0..argc.min(63) {
                        if defs & (1u64 << (1 + p)) == 0 {
                            alias_params |= 1u64 << p;
                        }
                    }
                }
                let empty_upvals = FxHashMap::default();
                let empty_nested = FxHashMap::default();
                let typed_lane = match crate::codegen::build_typed_lane_guarded(
                    &body,
                    callee.param_count,
                    argc,
                    arg_base,
                    caller.reg_count,
                    callee.reg_count,
                    &empty_upvals,
                    &consts,
                    &empty_nested,
                    None,
                ) {
                    Ok(lane) => lane,
                    Err(reason) => {
                        if log {
                            eprintln!(
                                "[int-computed] fn{func_id}@{ip} arm {index} DECLINE typed lane ({reason})"
                            );
                        }
                        failed = true;
                        break;
                    }
                };
                variants.push(LeafInlinePlan {
                    callee_bits: callee_value.bits(),
                    callee_ver: self.heap.version_of(callee_value.heap_index()),
                    same_proto_fid: None,
                    same_proto_guard: crate::vm::helpers_misc::jit_leaf_same_func_proto as usize,
                    reg_window: caller.reg_count,
                    callee_reg_count: callee.reg_count,
                    this_bits: Value::UNDEFINED.bits(), // body provably never reads r0
                    param_count: callee.param_count,
                    default_arg_mask: 0,
                    body,
                    consts,
                    upvals: FxHashMap::default(),
                    cell_get: crate::vm::helpers_misc::jit_cell_get as usize,
                    cell_set: crate::vm::helpers_misc::jit_cell_set as usize,
                    prop_get: crate::vm::helpers_misc::jit_get_prop_leaf as usize,
                    callee_fid: fid,
                    nested: FxHashMap::default(),
                    uninit_mask,
                    alias_params,
                    slot_guard: None,
                    direct_global_route_epoch: None,
                    typed_lane: Some(typed_lane),
                    span_code_unit_pred: None,
                });
            }
            if failed || variants.len() != items.len() {
                continue;
            }
            if log {
                eprintln!(
                    "[int-computed] fn{func_id}@{ip} ELIGIBLE arms={} fallback=@{fallback_ip}",
                    variants.len()
                );
            }
            plans.insert(
                ip,
                DenseComputedLeafPlan {
                    recv_src,
                    recv_bits: recv.bits(),
                    recv_ver: self.heap.version_of(recv.heap_index()),
                    fallback_ip: fallback_ip as u32,
                    drop_obj_def: drop_obj_def.map(|v| v as u32),
                    variants,
                    guard_helper: crate::vm::helpers_misc::jit_dense_computed_leaf_guard as usize,
                },
            );
        }
        plans
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
    /// Tier C cross-call plan (B83): the `Call` ips worth emitting the native
    /// cross-call attempt at. The emitted attempt is CORRECT at any Call site
    /// (the helper re-resolves the live callee Value every call and deopts for
    /// anything but a Tier-C-compiled plain function); the plan exists only to
    /// avoid planting a useless extra helper round trip at sites whose live IC
    /// says the callee is a native/bound/exotic (or is still unfilled). Off
    /// switch: `ZIPP_NO_CROSSCALL=1` (empty plan ⇒ byte-identical Tier C code).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_cross_call_plan(&self, func_id: u32) -> crate::codegen::CrossCallPlan {
        let mut plan = crate::codegen::CrossCallPlan::default();
        if std::env::var_os("ZIPP_NO_CROSSCALL").is_some() {
            return plan;
        }
        let caller = self.func(func_id as usize);
        for (ip, instr) in caller.code.iter().enumerate() {
            let Instr::Call { argc, .. } = *instr else {
                continue;
            };
            // A filled plain-user-function way is the signal; the helper's own
            // live resolution is what correctness rests on. The bounded
            // polymorphic extension accepts only multiple identities sharing
            // one FuncProto (the rotating-closure shape).
            let (live, same_proto) = if let Some(live) = self.ic_call_mono(func_id, ip) {
                (Some(live), false)
            } else {
                (
                    crate::codegen::poly_crosscall_enabled()
                        .then(|| self.ic_call_same_proto(func_id, ip))
                        .flatten(),
                    true,
                )
            };
            let Some((_bits, _ver, fid, _closure)) = live else {
                continue;
            };
            let callee = self.func(fid as usize);
            if callee.is_generator
                || callee.is_async
                || callee.rest_reg.is_some()
                || callee.arguments_reg.is_some()
            {
                continue; // could never hold a cross entry / needs setup_call
            }
            let same_proto2 = (same_proto
                && crate::codegen::same_proto_cross2_enabled()
                && argc == 2
                && callee.lexical_this
                && callee.param_count == 2)
                .then_some(crate::codegen::SameProtoCross2Plan {
                    fid,
                    callee_regs: callee.reg_count.max(1),
                });
            if same_proto2.is_some() && std::env::var_os("ZIPP_JITLOG").is_some() {
                eprintln!(
                    "[cross] fn{func_id}@{ip} SAME-PROTO-ARROW2 fn{fid} callee_regs={}",
                    callee.reg_count.max(1)
                );
            }
            plan.insert(ip, same_proto2);
        }
        plan
    }

    /// Recognise the caller half of an adjacent
    /// `span_pred(i, a) || span_pred(i, b)` chain.  The callee recognizer has
    /// already proved the predicate body.  This proof additionally pins the
    /// two literal arguments, both live-global loads, both call ICs, and the
    /// exact shared branch.  A fast helper is effect-free; on any runtime shape
    /// miss the first call is replayed normally, so the skipped second load and
    /// call are never elided after an observable action.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn span_code_unit_pair_plan(
        &self,
        func_id: u32,
        caller: &crate::bytecode::FuncProto,
        range_start: usize,
        range_end: usize,
        call_ip: usize,
        callee_bits: u64,
        callee_ver: u32,
        callee_fid: u32,
    ) -> Option<crate::codegen::SpanCodeUnitPairPlan> {
        if !crate::codegen::span_code_unit_pair_enabled()
            || call_ip < range_start.checked_add(3)?
            || call_ip.checked_add(6)? > range_end
        {
            return None;
        }
        let code = &caller.code;
        let Instr::LoadGlobal {
            dst: first_callee_reg,
            idx: first_callee_global,
        } = &code[call_ip - 3]
        else {
            return None;
        };
        let Instr::LoadGlobal {
            dst: first_i_reg,
            idx: first_i_global,
        } = &code[call_ip - 2]
        else {
            return None;
        };
        let Instr::LoadInt {
            dst: first_ch_reg,
            val: first_ch,
        } = &code[call_ip - 1]
        else {
            return None;
        };
        let Instr::Call {
            dst: first_dst,
            callee: first_callee,
            arg_base: first_args,
            argc: first_argc,
        } = &code[call_ip]
        else {
            return None;
        };
        let Instr::JumpIfTrue {
            cond: first_cond,
            target: join,
        } = &code[call_ip + 1]
        else {
            return None;
        };
        let Instr::LoadGlobal {
            dst: second_callee_reg,
            idx: second_callee_global,
        } = &code[call_ip + 2]
        else {
            return None;
        };
        let Instr::LoadGlobal {
            dst: second_i_reg,
            idx: second_i_global,
        } = &code[call_ip + 3]
        else {
            return None;
        };
        let Instr::LoadInt {
            dst: second_ch_reg,
            val: second_ch,
        } = &code[call_ip + 4]
        else {
            return None;
        };
        let Instr::Call {
            dst: second_dst,
            callee: second_callee,
            arg_base: second_args,
            argc: second_argc,
        } = &code[call_ip + 5]
        else {
            return None;
        };
        let Instr::JumpIfFalse {
            cond: second_cond, ..
        } = &code[call_ip + 6]
        else {
            return None;
        };

        if *first_argc != 2
            || *second_argc != 2
            || *first_callee_reg != *first_callee
            || *first_i_reg != *first_args
            || first_args.checked_add(1) != Some(*first_ch_reg)
            || *first_cond != *first_dst
            || *join as usize != call_ip + 6
            || *second_callee_global != *first_callee_global
            || *second_i_global != *first_i_global
            || *second_callee_reg != *second_callee
            || *second_i_reg != *second_args
            || second_args.checked_add(1) != Some(*second_ch_reg)
            || *second_dst != *first_dst
            || *second_cond != *second_dst
            || !self.global_slot_directly_routable(*first_callee_global)
            || !self.global_slot_directly_routable(*first_i_global)
        {
            return None;
        }

        // The three setup destinations skipped on the fused route are compiler
        // temporaries.  Prove that syntactically instead of relying on today's
        // allocator convention: they must be distinct from the observable bool
        // result and never be read again anywhere after the shared branch.
        // `instr_uses` is exhaustive over the bytecode enum; scanning beyond
        // later redefinitions is intentionally conservative and fail-closed.
        let skipped = [*second_callee_reg, *second_i_reg, *second_ch_reg];
        if skipped.iter().any(|&r| r == *second_dst)
            || skipped[0] == skipped[1]
            || skipped[0] == skipped[2]
            || skipped[1] == skipped[2]
            || code[call_ip + 6..].iter().any(|instr| {
                crate::codegen::instr_uses(instr)
                    .iter()
                    .any(|r| skipped.contains(r))
            })
        {
            return None;
        }

        // No control edge may enter after either of the first call's literal
        // setup ops; otherwise the baked first code unit need not be live.
        if code
            .iter()
            .filter_map(slot_guard_jump_target)
            .any(|target| {
                let target = target as usize;
                target > call_ip - 3 && target <= call_ip
            })
        {
            return None;
        }

        // The second call must resolve to the identical live closure/version.
        // This guard is a compile-time witness only; the first emitted identity
        // guard is sufficient at runtime because the fast helper invokes no
        // user code between the two direct global reads.
        let (bits2, ver2, fid2, _) = self.ic_call_mono(func_id, call_ip + 5)?;
        if bits2 != callee_bits || ver2 != callee_ver || fid2 != callee_fid {
            return None;
        }

        let packed_units = (*first_ch as u32 as u64) | ((*second_ch as u32 as u64) << 32);
        Some(crate::codegen::SpanCodeUnitPairPlan {
            packed_units,
            resume_ip: (call_ip + 6) as u32,
            helper: crate::vm::helpers_misc::jit_span_code_unit_pair as usize,
        })
    }

    /// Resolve a call register whose nearest textual definition is an
    /// `UpvalGet` from the currently compiling activation. This is only a
    /// compile-time representative: the emitted same-proto guard validates the
    /// live register on every call, so a different control path/cell value can
    /// only miss to the normal helper, never run the selected body unchecked.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn leaf_upval_callee_representative(
        &self,
        func_id: u32,
        start: usize,
        call_ip: usize,
        callee_reg: u16,
    ) -> Option<(u64, u32, u32, u32)> {
        let caller = self.func(func_id as usize);
        let mut upval_idx = None;
        for at in (start..call_ip).rev() {
            match caller.code[at] {
                Instr::UpvalGet { dst, idx } if dst == callee_reg => {
                    upval_idx = Some(idx);
                    break;
                }
                _ if crate::codegen::writes_reg(&caller.code[at]) == Some(callee_reg) => {
                    return None;
                }
                _ => {}
            }
        }
        let idx = upval_idx?;
        let closure = self
            .frames
            .last()
            .filter(|frame| frame.func == func_id)
            .map(|frame| frame.closure)?;
        if closure == NO_CLOSURE || closure as usize >= self.heap.len() {
            return None;
        }
        let cell = match self.heap.get(closure) {
            crate::heap::HeapObj::Closure { upvalues, .. } => upvalues.get(idx as usize).copied(),
            _ => None,
        }?;
        if cell as usize >= self.heap.len() {
            return None;
        }
        let value = match self.heap.get(cell) {
            crate::heap::HeapObj::Cell(value) if !value.is_uninitialized() => *value,
            _ => return None,
        };
        if !value.is_heap() || value.heap_index() as usize >= self.heap.len() {
            return None;
        }
        let (fid, callee_closure) = self.ic_plain_fn(value)?;
        Some((
            value.bits(),
            self.heap.version_of(value.heap_index()),
            fid,
            callee_closure,
        ))
    }

    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_leaf_inline_plan(
        &self,
        func_id: u32,
        start: u32,
        end: u32,
    ) -> rustc_hash::FxHashMap<usize, crate::codegen::LeafInlinePlan> {
        use crate::codegen::{callee_leaf_ok_for_call, callee_leaf_ok_one_call, LeafInlinePlan};
        let mut plan = rustc_hash::FxHashMap::default();
        // A leaf splice executes callee bytecodes inside the caller's native
        // block, while the meter's block map contains only caller bytecodes.
        // Until a splice carries an exact callee charge map, decline every
        // leaf plan for an instrumented VM. This also keeps specialized
        // default-parameter prologues from becoming an uncharged path.
        #[cfg(feature = "instrument")]
        if self.jit.metered() {
            return plan;
        }
        let caller = self.func(func_id as usize);
        let reg_window = caller.reg_count;
        let log = std::env::var_os("ZIPP_JITLOG").is_some();
        for ip in start as usize..=end as usize {
            let Instr::Call {
                argc,
                arg_base,
                callee: callee_reg,
                ..
            } = caller.code[ip]
            else {
                continue;
            };
            // Prefer the exact monomorphic entry. A bounded extension accepts a
            // polymorphic site only when every filled way names the same
            // FuncProto; later gates require that proto to be capture-free and
            // non-arrow, and emitted code re-checks the LIVE callee's proto.
            let poly_on = std::env::var_os("ZIPP_NO_POLY_LEAF_INLINE").is_none();
            let upval_rep = poly_on
                .then(|| {
                    self.leaf_upval_callee_representative(func_id, start as usize, ip, callee_reg)
                })
                .flatten()
                .filter(|&(_, _, fid, _)| {
                    let proto = self.func(fid as usize);
                    !proto.lexical_this && proto.upvalues.is_empty()
                });
            let mono = self.ic_call_mono(func_id, ip);
            let (resolved, same_proto) = if let Some(dynamic) = upval_rep {
                (Some(dynamic), true)
            } else if let Some(exact) = mono {
                (Some(exact), false)
            } else if poly_on {
                (self.ic_call_same_proto(func_id, ip), true)
            } else {
                (None, false)
            };
            let Some((callee_bits, callee_ver, fid, closure)) = resolved else {
                if log {
                    eprintln!("[leaf] fn{func_id}@{ip} NOT-MONO (no single Callee IC way)");
                }
                continue;
            };
            let callee = self.func(fid as usize);
            if same_proto
                && (self.get_function_realm(Value::from_bits(callee_bits)) != 0
                    || self.program.functions.get(fid as usize).is_none())
            {
                // A sloppy ordinary call substitutes the CALLEE realm's global
                // object for undefined `this`. The splice bakes main-realm
                // `global_this`, so a child-realm representative is not an
                // interchangeable instance even when its FuncProto matches.
                // Eval/loader-only fids also stay on the real-call path because
                // the tiny runtime guard intentionally indexes the root program
                // table and fail-closes outside it.
                if log {
                    eprintln!(
                        "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE \
                         (same-proto representative is non-main-realm or dynamic)"
                    );
                }
                continue;
            }
            if same_proto && (callee.lexical_this || !callee.upvalues.is_empty()) {
                if log {
                    eprintln!(
                        "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE \
                         (same-proto leaf has captures or lexical this)"
                    );
                }
                continue;
            }
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
            // An exact-identity arrow is also bakeable: its Closure stores the
            // immutable lexical-this Value and the bits+slot-version guard pins
            // that exact closure instance. This is distinct from rotating-proto
            // cross-calls, which load each live closure's this dynamically.
            let this_bits = if callee.lexical_this {
                if closure == NO_CLOSURE {
                    continue;
                }
                match self.heap.get(closure) {
                    crate::heap::HeapObj::Closure { func, this_val, .. } if *func == fid => {
                        this_val.bits()
                    }
                    _ => continue,
                }
            } else if callee.is_strict {
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
            let mut nested_upvals: rustc_hash::FxHashMap<u16, u64> =
                rustc_hash::FxHashMap::default();
            let mut nested_consts: rustc_hash::FxHashMap<u32, u64> =
                rustc_hash::FxHashMap::default();
            let mut default_arg_mask = 0u64;
            let body = match callee_leaf_ok_for_call(callee, argc) {
                Some((b, mask)) => {
                    default_arg_mask = mask;
                    b
                }
                None => {
                    // Same-proto inlining deliberately stops at a direct leaf:
                    // a wrapper splice adds an independently identity-guarded
                    // inner call and has no rotating-identity evidence yet.
                    if same_proto {
                        if log {
                            eprintln!(
                                "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE \
                                 (same-proto target is not a direct leaf)"
                            );
                        }
                        continue;
                    }
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
            // A flattened wrapper owns no captures (enforced by
            // `splice_nested_leaf`), so the nested callee's biased body indices
            // exclusively address this returned map. Merge it before the write
            // preflight below; checking only the outer map would classify every
            // valid nested UpvalSet as "unresolved" and silently disable the
            // long-standing typed splice for `ri(n) { return rnd() * n | 0; }`.
            upvals.extend(nested_upvals);
            // An inline UpvalSet commits through `jit_cell_set`, intentionally
            // the unconditional declaring-scope primitive. Preflight every
            // written capture here so an immutable / TDZ PutValue stays on the
            // interpreter path instead of silently writing through it. The
            // exact-identity + heap-version guard pins these cell identities for
            // the lifetime of the plan; mutable contents are still loaded live.
            let mut invalid_upval_write = false;
            for instr in &body {
                let Instr::UpvalSet { idx, .. } = *instr else {
                    continue;
                };
                let Some(cell_bits) = upvals.get(&idx).copied() else {
                    invalid_upval_write = true;
                    break;
                };
                let cell = Value::from_bits(cell_bits).heap_index();
                if (!self.const_cells.is_empty() && self.const_cells.contains(&cell))
                    || (!self.fn_name_cells.is_empty() && self.fn_name_cells.contains(&cell))
                    || self.heap.cell_get(cell).is_uninitialized()
                {
                    invalid_upval_write = true;
                    break;
                }
            }
            if invalid_upval_write {
                if log {
                    eprintln!(
                        "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE \
                         (captured write is immutable, TDZ, or unresolved)"
                    );
                }
                continue;
            }
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
            // One shared predicate with the region and Tier A/C gates — see
            // `global_slot_directly_routable`. It also rejects a STORE to a slot the
            // global object now shadows with a real descriptor, which this copy of the
            // scan did not check.
            if !body.iter().all(|ins| match *ins {
                Instr::LoadGlobal { idx, .. } | Instr::LoadGlobalOrUndefined { idx, .. } => {
                    self.global_slot_directly_routable(idx)
                }
                Instr::StoreGlobal { idx, .. }
                | Instr::StoreGlobalStrict { idx, .. }
                | Instr::StoreGlobalResolved { idx, .. } => {
                    self.global_store_slot_directly_routable(idx)
                }
                _ => true,
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
            consts.extend(nested_consts);
            // W11 (B124): the may-read-before-write fill mask over the FINAL
            // (possibly nested-flattened) body — the emitter zero-fills only
            // these locals per execution. u64::MAX (switch off / unmodelled
            // op) keeps the full fill, byte-identical to pre-W11.
            let uninit_mask = if crate::codegen::splice_fill_enabled() {
                crate::codegen::splice_uninit_mask(
                    &body,
                    (callee.reg_count + extra_regs) as usize,
                    callee.param_count as usize,
                )
            } else {
                u64::MAX
            };
            // W11 (B124): params provably never written by any body op may
            // ALIAS the caller's arg slots (no copy). Fail-closed on unknown
            // defs; belt-and-braces: decline if a nested guard's callee reg
            // could itself be a param (the remap would misroute its read).
            let alias_params = if crate::codegen::splice_alias_enabled() {
                match crate::codegen::splice_body_defs(&body) {
                    Some(defs) if nested.values().all(|g| g.callee_reg > callee.param_count) => {
                        let mut m = 0u64;
                        let n_alias = (argc.min(callee.param_count) as u64).min(63);
                        for i in 0..n_alias {
                            if defs & (1u64 << (1 + i)) == 0 {
                                m |= 1u64 << i;
                            }
                        }
                        m
                    }
                    _ => 0,
                }
            } else {
                0
            };
            // W12: slot-generation guard — key the site to (addr of
            // global_gens[g], baked gen) when the callee register provably
            // holds global slot g's value at the call and every write to g
            // bumps the generation (conditions (a)-(g), `slot_guard_key`).
            // `None` keeps today's per-execution bits+version guard.
            let slot_guard = if same_proto || !crate::codegen::splice_slotgen_enabled() {
                None
            } else {
                match self.slot_guard_key(func_id, start as usize, ip, callee_bits, callee_ver) {
                    Ok((g, addr, gen)) => {
                        if log {
                            eprintln!("[leaf] fn{func_id}@{ip} slot_guard=g{g}@gen{gen}");
                        }
                        Some((addr, gen))
                    }
                    Err(reason) => {
                        if log {
                            eprintln!("[leaf] fn{func_id}@{ip} slot_guard=DECLINED({reason})");
                        }
                        None
                    }
                }
            };
            let body_has_direct_global = body.iter().any(|ins| {
                matches!(
                    ins,
                    Instr::LoadGlobal { .. }
                        | Instr::LoadGlobalOrUndefined { .. }
                        | Instr::StoreGlobal { .. }
                        | Instr::StoreGlobalStrict { .. }
                        | Instr::StoreGlobalResolved { .. }
                )
            });
            // A raw-global leaf body is shared by two emitters which do not
            // execute the same schedule: the typed lane carries its own guard,
            // while the generic MEM splice expands the bytecode directly and
            // the INT splice hoists that expansion into a region. Keep their
            // route authority independent of `typed_lane`: prove the exact
            // slots/realm here once and carry only the VM-relative epoch.
            //
            // Epoch zero is intentionally the sole admitted value. A later
            // delete/redefinition bumps it and both emitters fall back; even a
            // saturated epoch can therefore never equal this baked proof.
            // `StoreGlobalResolved` remains on the real-call path: its dynamic
            // binding resolution is not equivalent to an unconditional r12
            // store merely because its current slot happens to be routable.
            let direct_global_route_epoch = if body_has_direct_global {
                let callee_value = Value::from_bits(callee_bits);
                let nested_routes_are_root = nested.values().all(|guard| {
                    let value = Value::from_bits(guard.bits);
                    value.is_heap()
                        && self.get_function_realm(value) == 0
                        && self.ic_plain_fn(value).is_some_and(|(nested_fid, _)| {
                            self.jit_func_eligible(nested_fid)
                                && !self.closure_eval_scope.contains_key(&value.heap_index())
                        })
                });
                let route_is_exact = callee_value.is_heap()
                    && self.global_route_epoch == 0
                    && self.current_realm_id().is_none()
                    && self.get_function_realm(callee_value) == 0
                    && self.jit_func_eligible(fid)
                    && !self
                        .closure_eval_scope
                        .contains_key(&callee_value.heap_index())
                    && nested_routes_are_root
                    && body.iter().all(|ins| match *ins {
                        Instr::LoadGlobal { idx, .. }
                        | Instr::LoadGlobalOrUndefined { idx, .. } => {
                            self.global_slot_directly_routable(idx)
                        }
                        Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } => {
                            self.global_store_slot_directly_routable(idx)
                        }
                        Instr::StoreGlobalResolved { .. } => false,
                        _ => true,
                    });
                if !route_is_exact {
                    if log {
                        eprintln!(
                            "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE \
                             (direct-global route is not exact)"
                        );
                    }
                    continue;
                }
                Some(0)
            } else {
                None
            };
            // Typed splice lane: schedule a register-resident emission for a
            // proven-numeric body (fail-closed — any Err keeps the generic
            // boxed loop, byte-identical). Computed over the FINAL (possibly
            // nested-flattened) body with the merged upvals/consts/nested
            // maps, so the schedule sees exactly what the emitter would. A
            // raw global access additionally gets the VM-wide route-epoch
            // guard. Only epoch zero is admitted: after any observable global
            // delete/redefinition, saturation cannot make a stale baked epoch
            // look current and the real call remains authoritative.
            let typed_lane = if !crate::codegen::typed_splice_enabled() {
                None
            } else {
                match crate::codegen::build_typed_lane_guarded(
                    &body,
                    callee.param_count,
                    argc,
                    arg_base,
                    reg_window,
                    callee.reg_count + extra_regs,
                    &upvals,
                    &consts,
                    &nested,
                    direct_global_route_epoch,
                ) {
                    Ok(lane) => {
                        if log {
                            eprintln!(
                                "[leaf] fn{func_id}@{ip} TYPED-LANE (ops={} guards={})",
                                lane.n_ops, lane.n_guards
                            );
                        }
                        Some(lane)
                    }
                    Err(reason) => {
                        if log {
                            eprintln!("[leaf] fn{func_id}@{ip} typed-lane=DECLINED({reason})");
                        }
                        None
                    }
                }
            };
            if log && same_proto {
                eprintln!(
                    "[leaf] fn{func_id}@{ip} callee fn{fid} SAME-PROTO-INLINE \
                     (default_mask={default_arg_mask:#x})"
                );
            }
            let mut span_code_unit_pred = if same_proto {
                None
            } else {
                span_code_unit_pred_plan(callee, &body, argc)
            };
            if let Some(pred) = span_code_unit_pred.as_mut() {
                pred.pair = self.span_code_unit_pair_plan(
                    func_id,
                    caller,
                    start as usize,
                    end as usize,
                    ip,
                    callee_bits,
                    callee_ver,
                    fid,
                );
            }
            if log && span_code_unit_pred.is_some() {
                eprintln!("[leaf] fn{func_id}@{ip} callee fn{fid} SPAN-CODEUNIT-PRED");
                if span_code_unit_pred.is_some_and(|p| p.pair.is_some()) {
                    eprintln!("[leaf] fn{func_id}@{ip} callee fn{fid} SPAN-CODEUNIT-PAIR");
                }
            }
            if log {
                // W11 mechanism proof: the fill mask must come out ~0 on the
                // hot bodies (tokIs/mix) or the cut silently no-ops.
                eprintln!(
                    "[leaf] fn{func_id}@{ip} splice-lite uninit_mask={uninit_mask:#x} alias_params={alias_params:#x}"
                );
            }
            plan.insert(
                ip,
                LeafInlinePlan {
                    callee_bits,
                    callee_ver,
                    same_proto_fid: same_proto.then_some(fid),
                    same_proto_guard: crate::vm::helpers_misc::jit_leaf_same_func_proto as usize,
                    this_bits,
                    reg_window,
                    callee_reg_count: callee.reg_count + extra_regs,
                    param_count: callee.param_count,
                    default_arg_mask,
                    body,
                    consts,
                    nested,
                    upvals,
                    cell_get: crate::vm::helpers_misc::jit_cell_get as usize,
                    cell_set: crate::vm::helpers_misc::jit_cell_set as usize,
                    prop_get: crate::vm::helpers_misc::jit_get_prop_leaf as usize,
                    callee_fid: fid,
                    uninit_mask,
                    alias_params,
                    slot_guard,
                    direct_global_route_epoch,
                    typed_lane,
                    span_code_unit_pred,
                },
            );
        }
        plan
    }

    /// W12: prove the `Call` at `call_ip`'s callee register holds global slot
    /// g's value on every execution that reaches the call within the compiled
    /// range, and key the site to `(g, &global_gens[g] as u64, baked gen)`.
    /// `Err` carries the JITLOG decline reason. The conditions, each
    /// load-bearing:
    ///
    /// * (b) the NEAREST preceding def of the callee register — scanning
    ///   backwards from the call but never past `range_start` (a def outside
    ///   the compiled range is unknowable at OSR entry) — is exactly
    ///   `LoadGlobal { idx: g }` (NOT `LoadGlobalOrUndefined`, whose sentinel
    ///   rewrite diverges from the raw slot read), with no other def of that
    ///   register in between (nearest-def gives this by construction; any op
    ///   the def model cannot classify declines, fail-closed).
    /// * (c) no jump target anywhere in the proto lands in `(def_ip, call_ip]`
    ///   — the gap is straight-line, so the def dominates the call and the
    ///   register cannot be observed with any other value there.
    /// * (d) a real program slot: field-pool/eval-pool slots are synced by
    ///   Rust paths outside the bump audit (e.g. the SROA field sync).
    /// * (e) no bytecode store op anywhere targets g (fail-closed set, kept
    ///   current by the eval registration hook) — so every possible write to
    ///   `globals[g]` is an enumerated `bump_global_gen` caller.
    /// * (f) the slot is a live, plain, directly-routable binding (shared
    ///   predicate with every other JIT global gate).
    /// * (g) the LIVE slot holds the IC's callee NOW, same (bits, version) —
    ///   the baked generation then WITNESSES exactly that tuple: while
    ///   `gens[g]` is unchanged, `globals[g]` still holds F (every write
    ///   bumps), and a rooted, non-moving F keeps its index; a same-bits
    ///   reuse would require F to die first — impossible while rooted — so
    ///   the version re-check's ABA job transfers to rooting.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn slot_guard_key(
        &self,
        func_id: u32,
        range_start: usize,
        call_ip: usize,
        callee_bits: u64,
        callee_ver: u32,
    ) -> Result<(u32, u64, u32), &'static str> {
        let caller = self.func(func_id as usize);
        let Instr::Call { callee, .. } = caller.code[call_ip] else {
            return Err("not-a-call");
        };
        let mut def: Option<(usize, u32)> = None;
        for j in (range_start..call_ip).rev() {
            match slot_guard_def(&caller.code[j]) {
                None => return Err("unmodelled-op-in-scan"),
                Some(Some(d)) if d == callee => {
                    if let Instr::LoadGlobal { idx, .. } = caller.code[j] {
                        def = Some((j, idx));
                    }
                    break;
                }
                Some(_) => {}
            }
        }
        let Some((def_ip, g)) = def else {
            return Err("nearest-def-not-loadglobal");
        };
        for ins in &caller.code {
            if let Some(t) = slot_guard_jump_target(ins) {
                let t = t as usize;
                if t > def_ip && t <= call_ip {
                    return Err("jump-target-in-gap");
                }
            }
        }
        if g >= self.program.global_count {
            return Err("pool-slot");
        }
        if self.bytecode_stored_slots.contains(&g) {
            return Err("bytecode-stored");
        }
        if !self.global_slot_directly_routable(g) {
            return Err("not-directly-routable");
        }
        if self.globals[g as usize].bits() != callee_bits {
            return Err("live-slot-differs-from-ic");
        }
        if self
            .heap
            .version_of(Value::from_bits(callee_bits).heap_index())
            != callee_ver
        {
            return Err("callee-version-stale");
        }
        // `global_gens` owns a separately allocated, never-reallocated buffer,
        // so this element address remains stable even if an embedded Vm moves.
        // (d) bounds the index: gens was sized past global_count at boot.
        debug_assert!((g as usize) < self.global_gens.len());
        let addr = unsafe { self.global_gens.as_ptr().add(g as usize) } as u64;
        Ok((g, addr, self.global_gens[g as usize]))
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
        allow_global_methods: bool,
    ) -> rustc_hash::FxHashMap<usize, crate::codegen::MethodInlinePlan> {
        use crate::codegen::MethodInlinePlan;
        use crate::heap::HeapObj;
        const MAX_ARMS: usize = crate::codegen::JIT_IC_WAYS; // = 8
        let mut plan = rustc_hash::FxHashMap::default();
        if std::env::var_os("ZIPP_NO_METHOD_INLINE").is_some() {
            return plan; // kill-switch (live through all stages)
        }
        #[cfg(feature = "instrument")]
        if self.jit.metered() {
            // A splice executes callee bytecodes inside a caller block. Until
            // it carries an exact nested charge map, sandbox accounting keeps
            // the real CallMethod activation.
            return plan;
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
            // W19 (MI-LANE): a method site's `(arg_base, argc)` travels with the
            // arm so a lane can read formals from the caller's argument slots —
            // the SAME pair `emit_inline_method_call` binds from, taken from the
            // same instruction, so the two cannot disagree.
            let (obj, name, kind, arg_base, argc) = match caller.code[ip] {
                Instr::CallMethod {
                    obj,
                    name,
                    arg_base,
                    argc,
                    ..
                } => (obj, name, MiKind::Method, arg_base, argc),
                Instr::GetProp { obj, name, .. } => (obj, name, MiKind::Getter, 0, 0),
                Instr::SetProp { obj, name, .. } => (obj, name, MiKind::Setter, 0, 0),
                _ => continue,
            };
            let key = &caller.string_constants[name as usize];
            // ── enumerate candidate receivers ── the live exemplar at the obj reg
            // (last iteration's value) PLUS, when `obj` was `arr[idx]`, the array's
            // dense elements (the `objs[i&3]` polymorphic shape). Every candidate
            // is independently identity+version-guarded, so an extra/wrong guess
            // just yields a dead arm — never a correctness risk.
            let mut cand_bits: Vec<u64> = Vec::new();
            let push_cand = |v: Value, cands: &mut Vec<Value>, bits: &mut Vec<u64>| {
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
            if let Some(arr_reg) =
                Self::mi_last_getindex_array(&caller.code, start as usize, ip, obj)
            {
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
                    MiKind::Method => self.build_method_shape(
                        func_id,
                        ip,
                        recv,
                        key,
                        reg_window,
                        arg_base,
                        argc,
                        allow_global_methods,
                    ),
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
            plan.insert(
                ip,
                MethodInlinePlan {
                    reg_window,
                    win_top,
                    shapes,
                },
            );
        }
        plan
    }

    /// Tier-C's transactional own-method lane. Reuse the ordinary method
    /// planner, then retain only exact own-data arms whose body contains a
    /// direct global access and scheduled successfully. Every retained lane is
    /// register-only and therefore needs no scratch window.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_tierc_method_global_plan(
        &self,
        func_id: u32,
        start: u32,
        end: u32,
        base: usize,
    ) -> rustc_hash::FxHashMap<usize, crate::codegen::MethodInlinePlan> {
        let mut plan = self.build_method_inline_plan(func_id, start, end, base, true);
        plan.retain(|_, p| {
            p.shapes.retain(|s| {
                s.method_slot.is_some()
                    && s.typed_lane.is_some()
                    && s.body.iter().any(|i| {
                        matches!(
                            i,
                            Instr::LoadGlobal { .. }
                                | Instr::LoadGlobalOrUndefined { .. }
                                | Instr::StoreGlobal { .. }
                                | Instr::StoreGlobalStrict { .. }
                        )
                    })
            });
            if p.shapes.is_empty() {
                false
            } else {
                // A typed lane holds no value in a carved callee window.
                p.win_top = p.reg_window;
                true
            }
        });
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
            nested_reject("outer-has-upvalues");
            return None;
        }
        let (callee_reg, dst) = match outer_body[call_at] {
            Instr::Call { callee, dst, .. } => (callee, dst),
            _ => return None,
        };
        // Any argc is admitted (B76). The emitter zero-fills every register past
        // the OUTER window to undefined — which covers the inner's `this` (strict,
        // plain call ⇒ undefined) and any params the call leaves unfilled — so the
        // splice only has to SEED the params that are passed, with plain `Move`s
        // inserted after the guard marker. Pure ops: a later bail re-runs the whole
        // outer call with nothing committed, so deopt-idempotency is untouched.
        // The B75/B76 surveys showed `inner-call-has-args` was EVERY remaining
        // nested reject on the call-heavy rows (13 sites in parse-large-js alone).
        // Resolve the wrapper's own call site from ITS live IC.
        let Some((bits, ver, inner_fid, inner_closure)) = self.ic_call_mono(outer_fid, call_at)
        else {
            nested_reject("inner-call-site-not-monomorphic");
            return None;
        };
        let inner = self.func(inner_fid as usize);
        if inner.lexical_this || !inner.is_strict {
            nested_reject("inner-lexical-this-or-sloppy");
            return None;
        }
        let Some(inner_body) = callee_leaf_ok(inner) else {
            nested_reject("inner-not-leaf");
            return None;
        };
        // `splice_uninit_mask`, parameter aliasing and the typed-lane planner
        // encode register sets in u64. The historical 32+32 leaf caps made
        // this bound implicit; the guarded wider leaf cap needs it explicit.
        if outer.reg_count.checked_add(inner.reg_count)? > 64 {
            nested_reject("combined-register-window");
            return None;
        }
        if inner_body.iter().any(|i| {
            matches!(
                i,
                Instr::Jump { .. }
                    | Instr::JumpIfFalse { .. }
                    | Instr::JumpIfTrue { .. }
                    | Instr::JumpIfNotLt { .. }
                    | Instr::JumpIfNotLe { .. }
            )
        }) {
            nested_reject("inner-branchy");
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
                                                                    // Seed the inner's params from the outer call's arg registers. Params the
                                                                    // call does not fill stay at the emitter's undefined zero-fill.
        let (arg_base, argc) = match outer_body[call_at] {
            Instr::Call { arg_base, argc, .. } => (arg_base, argc),
            _ => return None,
        };
        for k in 0..argc.min(inner.param_count) {
            flat.push(Instr::Move {
                dst: off + 1 + k,
                src: arg_base + k,
            });
        }
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
        let guard = NestedGuard {
            callee_reg,
            bits,
            ver,
        };
        Some((flat, guard, inner.reg_count, upvals, consts))
    }

    /// The last `GetIndex{dst:obj_reg, obj:arr}` in `code[start..ip]` (the array a
    /// `arr[idx]` receiver came from), so the planner can bake an arm per array
    /// element. `None` if `obj_reg` was last produced by something else.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn mi_last_getindex_array(
        code: &[Instr],
        start: usize,
        ip: usize,
        obj_reg: u16,
    ) -> Option<u16> {
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
        // W19 (MI-LANE): the SITE's argument window. A lane reads a formal
        // parameter straight out of the caller's arg slot (`ParamLoad`), so it
        // needs the same `arg_base`/`argc` the emitter binds from.
        arg_base: u16,
        argc: u16,
        allow_global_methods: bool,
    ) -> Option<(crate::codegen::MethodInlineShape, u16)> {
        use crate::heap::HeapObj;
        use crate::vm::ic::Walked;
        let ridx = recv.heap_index();
        if !self.ic_obj_ok(ridx) {
            return None;
        }
        // Two receiver shapes inline. A class instance resolves the method
        // through its class; a PLAIN object resolves it to an own data property
        // holding a function — `{ m() {…} }`, the module/callback/vtable shape,
        // which is everywhere in real JavaScript and previously never inlined
        // (measured 21ns/call against 3.8ns for the same method on a class).
        let (recv_class, vals_ptr, shape_guardable) = match self.heap.get(ridx) {
            HeapObj::Object(m) if !m.is_ctor => {
                (m.class, m.vals.as_ptr() as u64, m.shape_guardable())
            }
            _ => return None,
        };
        // An own property named `key` shadows a CLASS method, so a class
        // receiver declines; for a plain receiver that own property IS the
        // method, and its slot is what gets guarded.
        let own_slot = match self.heap.get(ridx) {
            HeapObj::Object(m) => m.pos(key),
            _ => None,
        };
        let mut proto_method = None;
        let (fid, method_slot) = match (recv_class, own_slot) {
            (Some(c), None) => (self.ic_class_method_fid(func_id, ip, c)?, None),
            // ── B78: the method is INHERITED ── a plain object with neither a
            // class nor an own slot for `key`. `Object.create(proto)` and
            // `Ctor.prototype.m = fn` both land here, and both used to fall
            // through the `_` arm below to the per-call helper: 29.5ns/call at
            // ONE receiver, against 5.5ns for the same method on an ES class.
            //
            // Resolution is `ic_walk`, the interpreter's own side-effect-free
            // fill walk — so what gets baked is by construction what the
            // interpreter would resolve, and the exclusions it already makes
            // (exotic receivers, class links mid-chain, chains deeper than
            // IC_MAX_HOPS, accessors, `#`-names) are inherited rather than
            // re-derived here.
            (None, None) => {
                if !proto_method_inline_enabled() {
                    return None;
                }
                let (hops, slot) = match self.ic_walk(recv, key) {
                    Walked::ChainData { hops, slot, .. } => (hops, slot),
                    _ => return None, // accessor / chain miss / not cacheable
                };
                let n = hops.1 as usize;
                if n == 0 {
                    return None;
                }
                let holder = hops.0[n - 1].0;
                let (fv, hvals) = match self.heap.get(holder) {
                    HeapObj::Object(hm) => (hm.vals[slot as usize], hm.vals.as_ptr() as u64),
                    _ => return None,
                };
                if !fv.is_heap() {
                    return None;
                }
                // Same callee restrictions as the own-slot arm: a plain
                // capture-free function, never an arrow (whose `this` is
                // lexical and would be silently rebound to the receiver).
                let f = match self.heap.get(fv.heap_index()) {
                    HeapObj::Func(f) => *f,
                    HeapObj::Closure { func, upvalues, .. } if upvalues.is_empty() => *func,
                    _ => return None,
                };
                if self.func(f as usize).lexical_this {
                    return None;
                }
                proto_method = Some(crate::codegen::ProtoMethodGuard {
                    hops: hops.0[..n].to_vec(),
                    holder_vals_ptr: hvals,
                    holder_slot: slot,
                    fn_bits: fv.bits(),
                });
                (f, None)
            }
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
        };
        let callee = self.func(fid as usize);
        // Outer body admits `super.m()` (Stage 3); super targets do not.
        let body_len = Self::method_inline_body_ok(
            callee,
            true,
            false,
            allow_global_methods && method_slot.is_some(),
        )?;
        let body: Vec<Instr> = callee.code[..body_len].to_vec();
        let has_direct_global = body.iter().any(|i| {
            matches!(
                i,
                Instr::LoadGlobal { .. }
                    | Instr::LoadGlobalOrUndefined { .. }
                    | Instr::StoreGlobal { .. }
                    | Instr::StoreGlobalStrict { .. }
            )
        });
        if has_direct_global {
            // Narrow v1: an exact own-data method in the root/main realm, with
            // no inherited dynamic EvalScope. Its raw slots share the caller's
            // r12 table. Any other realm/environment must perform a real call.
            let (_, callee_bits) = method_slot?;
            let callee_v = Value::from_bits(callee_bits);
            if !shape_guardable
                || self.current_realm_id().is_some()
                || self.get_function_realm(callee_v) != 0
                // Main-program and loader-recorded ES-module protos are the
                // VM's exact immutable-JIT boundary. Ordinary eval/new Function
                // ids share the runtime table but are deliberately excluded.
                || !self.jit_func_eligible(fid)
                || self.closure_eval_scope.contains_key(&callee_v.heap_index())
                || self.global_route_epoch != 0
                || body.iter().any(|i| {
                    matches!(
                        i,
                        Instr::SuperMethod { .. }
                            | Instr::SuperGet { .. }
                            | Instr::SuperSet { .. }
                            | Instr::SuperBase { .. }
                    )
                })
            {
                return None;
            }
            if !body.iter().all(|i| match *i {
                Instr::LoadGlobal { idx, .. } | Instr::LoadGlobalOrUndefined { idx, .. } => {
                    self.global_slot_directly_routable(idx)
                }
                Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } => {
                    self.global_store_slot_directly_routable(idx)
                }
                _ => true,
            }) {
                return None;
            }
        }
        let field_slots = self.mi_bake_fields(ridx, &body, &callee.string_constants)?;
        let consts = Self::mi_bake_consts(&callee.constants, &body);
        // ── bake each `super.m()` in the body (Stage 3) ──
        let super_win = reg_window + callee.reg_count;
        let mut supers = rustc_hash::FxHashMap::default();
        let mut super_kinds: rustc_hash::FxHashMap<usize, rustc_hash::FxHashMap<u32, (u32, bool)>> =
            rustc_hash::FxHashMap::default();
        let mut max_super_regs = 0u16;
        for (bi, instr) in body.iter().enumerate() {
            if let Instr::SuperMethod {
                home_class_id,
                name: sname,
                argc: sargc,
                ..
            } = *instr
            {
                if sargc != 0 {
                    return None; // v1: 0-arg super only
                }
                let skey = &callee.string_constants[sname as usize];
                let sr = self.ic_super_method_baked(fid, bi, home_class_id, skey)?;
                let scallee = self.func(sr.fid as usize);
                // Super target must be inlinable AND have NO nested super (v1).
                let sblen = Self::method_inline_body_ok(scallee, false, false, false)?;
                let sbody: Vec<Instr> = scallee.code[..sblen].to_vec();
                let sfields = self.mi_bake_fields(ridx, &sbody, &scallee.string_constants)?;
                let sconsts = Self::mi_bake_consts(&scallee.constants, &sbody);
                if let Some(k) = self.mi_field_kinds(ridx, &sfields) {
                    super_kinds.insert(bi, k);
                }
                max_super_regs = max_super_regs.max(scallee.reg_count);
                supers.insert(
                    bi,
                    crate::codegen::SuperInline {
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
        let typed_lane = self.mi_plan_lane(
            func_id,
            ip,
            "method",
            ridx,
            &body,
            &supers,
            &super_kinds,
            &field_slots,
            &consts,
            vals_ptr,
            callee.reg_count,
            callee.param_count,
            argc,
            arg_base,
        );
        if has_direct_global && typed_lane.is_none() {
            // The boxed method emitter performs writes in source order and may
            // deopt later. Transactional global bodies are legal only when the
            // typed scheduler proved and buffered the entire closed body.
            return None;
        }
        Some((
            crate::codegen::MethodInlineShape {
                method_slot,
                proto_method,
                own_acc: None,
                recv_bits: recv.bits(),
                recv_ver,
                vals_ptr,
                field_slots,
                callee_reg_count: callee.reg_count,
                param_count: callee.param_count,
                body,
                consts,
                supers,
                typed_lane,
            },
            win_top,
        ))
    }

    /// W19 (MI-LANE): schedule this arm's body register-resident, or return
    /// `None` (with a JITLOG reason under `ZIPP_JITLOG`) to keep the boxed
    /// `emit_mi_body` emission byte-identical. Fail-closed at every step.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(clippy::too_many_arguments)]
    fn mi_plan_lane(
        &self,
        func_id: u32,
        ip: usize,
        kind: &str,
        ridx: u32,
        body: &[Instr],
        supers: &rustc_hash::FxHashMap<usize, crate::codegen::SuperInline>,
        super_kinds: &rustc_hash::FxHashMap<usize, rustc_hash::FxHashMap<u32, (u32, bool)>>,
        field_slots: &rustc_hash::FxHashMap<u32, u32>,
        consts: &rustc_hash::FxHashMap<u32, u64>,
        vals_ptr: u64,
        callee_reg_count: u16,
        param_count: u16,
        argc: u16,
        arg_base: u16,
    ) -> Option<crate::codegen::TypedLanePlan> {
        if !crate::codegen::mi_lane_enabled() {
            return None;
        }
        let log = std::env::var_os("ZIPP_JITLOG").is_some();
        let fields = match self.mi_field_kinds(ridx, field_slots) {
            Some(f) => f,
            None => {
                if log {
                    eprintln!("[mi] fn{func_id}@{ip} {kind} lane=DECLINED(field-not-number)");
                }
                return None;
            }
        };
        let has_direct_global = body.iter().any(|i| {
            matches!(
                i,
                Instr::LoadGlobal { .. }
                    | Instr::LoadGlobalOrUndefined { .. }
                    | Instr::StoreGlobal { .. }
                    | Instr::StoreGlobalStrict { .. }
            )
        });
        let global_route_guard = has_direct_global.then_some(self.global_route_epoch);
        match crate::codegen::build_mi_lane(
            body,
            supers,
            &fields,
            super_kinds,
            consts,
            vals_ptr,
            callee_reg_count,
            param_count,
            argc,
            arg_base,
            global_route_guard,
        ) {
            Ok(lane) => {
                if log {
                    eprintln!(
                        "[mi] fn{func_id}@{ip} {kind} LANE (ops={} guards={})",
                        lane.n_ops, lane.n_guards
                    );
                }
                Some(lane)
            }
            Err(reason) => {
                if log {
                    eprintln!("[mi] fn{func_id}@{ip} {kind} lane=DECLINED({reason})");
                }
                None
            }
        }
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
        let (recv_class, vals_ptr, own_slot) = match self.heap.get(ridx) {
            HeapObj::Object(m) if !m.is_ctor => (m.class, m.vals.as_ptr() as u64, m.pos(name)),
            _ => return None,
        };
        // Resolution splits on whether the receiver carries an OWN property of
        // this name.
        //
        //  * NONE -- the accessor is the receiver's CLASS's; the class resolvers
        //    hand back its fid. Unchanged, and still the only arm when a
        //    receiver has no own slot (G3b's "the recv-version guard catches a
        //    LATER own-add" reasoning is untouched).
        //  * SOME -- W20 (M2). An own DATA property SHADOWS a class accessor and
        //    still declines, exactly as G3b did. An own ACCESSOR *is* the
        //    accessor being read, and G3b declined that too, because an own
        //    accessor is an own property of that name: the twin of the defect
        //    B74/B78 fixed on `build_method_shape`. `Object.defineProperty(o,
        //    "v", {get})` is how most accessors in the wild are installed, and
        //    the identical getter body measured 3.8x (PGO) / 5.9x (stock)
        //    slower installed that way than on an ES class.
        let (fid, own_acc) = match own_slot {
            None => {
                let c = recv_class?;
                let fid = if is_setter {
                    self.ic_class_setter_fid(func_id, ip, c)?
                } else {
                    self.ic_class_getter_fid(func_id, ip, c)?
                };
                (fid, None)
            }
            Some(slot) => {
                if !own_accessor_inline_enabled() {
                    return None;
                }
                let m = match self.heap.get(ridx) {
                    HeapObj::Object(m) => m,
                    _ => return None,
                };
                let a = m.attr_at(slot);
                if !a.accessor {
                    // An own DATA property shadows the accessor -> G3b, unchanged.
                    return None;
                }
                // A getter lives in `vals[slot]`; a setter in `attrs[slot].setter`.
                // A getter-only accessor WRITTEN to (or a setter-only one read)
                // finds UNDEFINED here, `ic_plain_fn` rejects it, and the site
                // stays on the helper -- which throws in strict mode / answers
                // undefined exactly as the interpreter does.
                let (f, addr) = if is_setter {
                    (a.setter, std::ptr::addr_of!(m.attrs[slot].setter) as u64)
                } else {
                    (
                        m.val_at(slot),
                        vals_ptr + (slot * std::mem::size_of::<Value>()) as u64,
                    )
                };
                // A `defineProperty` accessor is an ARBITRARY function, unlike a
                // class accessor. `ic_plain_fn` is the same screen the
                // interpreter's own accessor path applies (rejects a
                // non-callable, a generator, an async fn); `method_inline_body_ok`
                // below rejects a body that reads an upvalue, `arguments` or a
                // rest parameter; and an ARROW is rejected here, because its
                // `this` is lexical and binding it to the receiver would be a
                // silent wrong answer (`build_method_shape` makes the same check
                // for the same reason).
                let (fid, _closure) = self.ic_plain_fn(f)?;
                if self.func(fid as usize).lexical_this {
                    return None;
                }
                // A class setter always has exactly one formal; a
                // `defineProperty` one need not. The emitter binds the incoming
                // value to window reg 1 unconditionally, so with 0 formals reg 1
                // is a LOCAL that must start undefined -- the same gate the
                // super-setter arm below applies, for the same reason.
                if is_setter && self.func(fid as usize).param_count != 1 {
                    return None;
                }
                (fid, Some((addr, f.bits())))
            }
        };
        let callee = self.func(fid as usize);
        // A GETTER body may read `super.v` (Stage 6) and a SETTER body may end
        // in `super.v = x` (Stage 7) — `method_inline_body_ok` holds the
        // per-op rules (the SuperSet is effectful, so last-op-only, and gated
        // on allow_setprop). A setter body may equally still end in its own
        // SetProp{obj:0} store (allow_setprop=is_setter).
        let body_len = Self::method_inline_body_ok(callee, true, is_setter, false)?;
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
        let mut super_kinds: rustc_hash::FxHashMap<usize, rustc_hash::FxHashMap<u32, (u32, bool)>> =
            rustc_hash::FxHashMap::default();
        let mut max_super_regs = 0u16;
        for (bi, instr) in body.iter().enumerate() {
            let (sr, sname, is_store) = match *instr {
                Instr::SuperGet {
                    home_class_id,
                    name: sname,
                    ..
                } => {
                    let skey = &callee.string_constants[sname as usize];
                    (
                        self.ic_super_getter_baked(fid, bi, home_class_id, skey)?,
                        sname,
                        false,
                    )
                }
                Instr::SuperSet {
                    home_class_id,
                    name: sname,
                    ..
                } => {
                    let skey = &callee.string_constants[sname as usize];
                    (
                        self.ic_super_setter_baked(fid, bi, home_class_id, skey)?,
                        sname,
                        true,
                    )
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
            let sblen = Self::method_inline_body_ok(scallee, false, is_store, false)?;
            let sbody: Vec<Instr> = scallee.code[..sblen].to_vec();
            let sfields = self.mi_bake_fields(ridx, &sbody, &scallee.string_constants)?;
            let sconsts = Self::mi_bake_consts(&scallee.constants, &sbody);
            if let Some(k) = self.mi_field_kinds(ridx, &sfields) {
                super_kinds.insert(bi, k);
            }
            max_super_regs = max_super_regs.max(scallee.reg_count);
            supers.insert(
                bi,
                crate::codegen::SuperInline {
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
        // W19 (MI-LANE): GETTERS only. A setter's body ends in the store the
        // v1 gate excludes (`build_mi_lane` would decline anyway), and a
        // setter's value is bound to sub-window reg 1 rather than read from an
        // argument slot — so `param_count > 0` must never reach `ParamLoad`
        // here, which reads `arg_base + i` with `arg_base = 0` at this site.
        let typed_lane = if is_setter || callee.param_count != 0 {
            None
        } else {
            self.mi_plan_lane(
                func_id,
                ip,
                "getter",
                ridx,
                &body,
                &supers,
                &super_kinds,
                &field_slots,
                &consts,
                vals_ptr,
                callee.reg_count,
                0,
                0,
                0,
            )
        };
        Some((
            crate::codegen::MethodInlineShape {
                method_slot: None,
                proto_method: None,
                own_acc,
                recv_bits: recv.bits(),
                recv_ver,
                vals_ptr,
                field_slots,
                callee_reg_count: callee.reg_count,
                param_count: callee.param_count,
                body,
                consts,
                supers,
                typed_lane,
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
                Instr::GetProp {
                    obj: 0,
                    name: fname,
                    ..
                } => (fname, false),
                Instr::SetProp {
                    obj: 0,
                    name: fname,
                    ..
                } => (fname, true),
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

    /// W19 (MI-LANE): pair each baked `this.<field>` slot with the live
    /// REPRESENTATION of the value it holds — `true` = Int-tagged, `false` = a
    /// boxed double. The lane emits the matching tag guard, so a slot that is
    /// re-typed after compile misses and re-runs the call through the helper
    /// (slower, never wrong).
    ///
    /// `None` if any admitted field holds a NON-number: the lane has no
    /// representation for it, and emitting a numeric guard that can never pass
    /// would turn every call at this arm into a guaranteed fallback.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_field_kinds(
        &self,
        ridx: u32,
        slots: &rustc_hash::FxHashMap<u32, u32>,
    ) -> Option<rustc_hash::FxHashMap<u32, (u32, bool)>> {
        let m = match self.heap.get(ridx) {
            crate::heap::HeapObj::Object(m) => m,
            _ => return None,
        };
        let mut out = rustc_hash::FxHashMap::default();
        for (&name, &slot) in slots.iter() {
            let v = *m.vals.get(slot as usize)?;
            if !v.is_number() {
                return None;
            }
            out.insert(name, (slot, v.is_int()));
        }
        Some(out)
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
                I::SuperMethod { arg_base, argc, .. } => (0..argc).any(|k| arg_base + k == dst),
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
        allow_globals: bool,
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
                I::LoadGlobal { .. } | I::LoadGlobalOrUndefined { .. } if allow_globals => {}
                I::StoreGlobal { .. } | I::StoreGlobalStrict { .. } if allow_globals => {}
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
        Instr::LoadConst { dst, idx } => Instr::LoadConst {
            dst: s(dst),
            idx: c(idx),
        },
        Instr::LoadBool { dst, val } => Instr::LoadBool { dst: s(dst), val },
        Instr::LoadUndefined { dst } => Instr::LoadUndefined { dst: s(dst) },
        Instr::Move { dst, src } => Instr::Move {
            dst: s(dst),
            src: s(src),
        },
        Instr::LoadGlobal { dst, idx } => Instr::LoadGlobal { dst: s(dst), idx },
        Instr::StoreGlobal { idx, src } => Instr::StoreGlobal { idx, src: s(src) },
        Instr::StoreGlobalStrict { idx, src } => Instr::StoreGlobalStrict { idx, src: s(src) },
        Instr::StoreGlobalResolved { idx, src } => Instr::StoreGlobalResolved { idx, src: s(src) },
        Instr::Add { dst, a, b } => Instr::Add {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::AddRightPair {
            dst,
            a,
            b,
            c: r,
            in_place,
        } => Instr::AddRightPair {
            dst: s(dst),
            a: s(a),
            b: s(b),
            c: s(r),
            in_place,
        },
        Instr::Pad2Concat { dst, src, zero } => Instr::Pad2Concat {
            dst: s(dst),
            src: s(src),
            zero,
        },
        Instr::Pad2Conditional { dst, src } => Instr::Pad2Conditional {
            dst: s(dst),
            src: s(src),
        },
        Instr::Sub { dst, a, b } => Instr::Sub {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Mul { dst, a, b } => Instr::Mul {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Div { dst, a, b } => Instr::Div {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Mod { dst, a, b } => Instr::Mod {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::AddInt { dst, a, imm, upd } => Instr::AddInt {
            dst: s(dst),
            a: s(a),
            imm,
            upd,
        },
        Instr::Neg { dst, a } => Instr::Neg {
            dst: s(dst),
            a: s(a),
        },
        Instr::Bitwise { dst, a, b, op } => Instr::Bitwise {
            dst: s(dst),
            a: s(a),
            b: s(b),
            op,
        },
        Instr::Eq { dst, a, b } => Instr::Eq {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Ne { dst, a, b } => Instr::Ne {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Lt { dst, a, b } => Instr::Lt {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Le { dst, a, b } => Instr::Le {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Gt { dst, a, b } => Instr::Gt {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Ge { dst, a, b } => Instr::Ge {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::GetIndex { dst, obj, key } => Instr::GetIndex {
            dst: s(dst),
            obj: s(obj),
            key: s(key),
        },
        Instr::CallMethod {
            dst,
            obj,
            name,
            arg_base,
            argc,
        } => Instr::CallMethod {
            dst: s(dst),
            obj: s(obj),
            name,
            arg_base: s(arg_base),
            argc,
        },
        Instr::MathOp {
            dst,
            op,
            arg_base,
            argc,
        } => Instr::MathOp {
            dst: s(dst),
            op,
            arg_base: s(arg_base),
            argc,
        },
        Instr::UpvalGet { dst, idx } => Instr::UpvalGet { dst: s(dst), idx },
        Instr::UpvalSet { idx, src } => Instr::UpvalSet { idx, src: s(src) },
        _ => return None,
    })
}

/// W12 slot-guard def model: what register does this op define? `Some(None)` =
/// provably none, `Some(Some(r))` = exactly r, `None` = unmodelled (the
/// reaching-def scan declines, fail-closed). USES need no modelling here —
/// only defs can break the "callee register still holds slot g's value"
/// chain; helper calls the ops make cannot write caller registers.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn slot_guard_def(i: &Instr) -> Option<Option<u16>> {
    Some(match *i {
        Instr::LoadConst { dst, .. }
        | Instr::LoadInt { dst, .. }
        | Instr::LoadBool { dst, .. }
        | Instr::LoadNull { dst }
        | Instr::LoadUndefined { dst }
        | Instr::LoadGlobal { dst, .. }
        | Instr::LoadGlobalOrUndefined { dst, .. }
        | Instr::Move { dst, .. }
        | Instr::Add { dst, .. }
        | Instr::AddInt { dst, .. }
        | Instr::Sub { dst, .. }
        | Instr::Mul { dst, .. }
        | Instr::Div { dst, .. }
        | Instr::Mod { dst, .. }
        | Instr::Neg { dst, .. }
        | Instr::Bitwise { dst, .. }
        | Instr::Not { dst, .. }
        | Instr::TypeOf { dst, .. }
        | Instr::TypeOfIs { dst, .. }
        | Instr::IsArray { dst, .. }
        | Instr::LenOf { dst, .. }
        | Instr::ForInKeys { dst, .. }
        | Instr::ForInLive { dst, .. }
        | Instr::GetIndex { dst, .. }
        | Instr::GetProp { dst, .. }
        | Instr::StrAppendInPlace { dst, .. }
        | Instr::StrAppendIndex { dst, .. }
        | Instr::AddRightPair { dst, .. }
        | Instr::Pad2Concat { dst, .. }
        | Instr::Pad2Conditional { dst, .. }
        | Instr::StrConcatChain { dst, .. }
        | Instr::Eq { dst, .. }
        | Instr::Ne { dst, .. }
        | Instr::Lt { dst, .. }
        | Instr::Le { dst, .. }
        | Instr::Gt { dst, .. }
        | Instr::Ge { dst, .. }
        | Instr::MathOp { dst, .. }
        | Instr::Call { dst, .. }
        | Instr::CallMethod { dst, .. }
        | Instr::UpvalGet { dst, .. }
        | Instr::CellGet { dst, .. } => Some(dst),
        Instr::StoreGlobal { .. }
        | Instr::StoreGlobalStrict { .. }
        | Instr::StoreGlobalResolved { .. }
        | Instr::SetProp { .. }
        | Instr::UpvalSet { .. }
        | Instr::CellSet { .. }
        | Instr::TailCall { .. }
        | Instr::Jump { .. }
        | Instr::JumpIfFalse { .. }
        | Instr::JumpIfTrue { .. }
        | Instr::JumpIfNotLt { .. }
        | Instr::JumpIfNotLe { .. }
        | Instr::Return { .. }
        | Instr::ReturnUndefined => None,
        _ => return None,
    })
}

/// W12 slot-guard control model: the jump target this op carries, if any.
/// These are the ONLY ops with instruction-index targets (`bytecode.rs`); a
/// target inside the def→call gap breaks the straight-line dominance proof.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn slot_guard_jump_target(i: &Instr) -> Option<u32> {
    match *i {
        Instr::Jump { target }
        | Instr::JumpIfFalse { target, .. }
        | Instr::JumpIfTrue { target, .. }
        | Instr::JumpIfNotLt { target, .. }
        | Instr::JumpIfNotLe { target, .. }
        | Instr::PushFinally { target, .. }
        | Instr::JumpFinally { target, .. } => Some(target),
        Instr::PushHandler { catch_target, .. } => Some(catch_target),
        _ => None,
    }
}
