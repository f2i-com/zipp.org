// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Top-16 bits of the canonical bool tag (`0x7FFA`). The five tag patterns
/// 0x7FF9..=0x7FFD are: Int, Bool, Null, Undefined, Heap — only Int is a number.
pub(crate) const BOOL_TAG: u64 = INT_TAG + (1u64 << 48);

/// Can the loop region `[start, end]` be compiled in the double subset? Every op
/// in range must be numeric/control-flow with no closure op, and any `LoadConst`
/// must reference a numeric constant, a single-ASCII-char string, or (MEM path
/// only — `const_strs` is `Some`) a string constant pre-interned at compile time
/// whose bits the emitter embeds (`const_strs` maps constant index → bits).
pub(crate) fn region_can_compile(
    proto: &FuncProto,
    start: u32,
    end: u32,
    const_strs: Option<&FxHashMap<u32, u64>>,
) -> bool {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    if e <= s || e >= code.len() {
        return false;
    }
    // Under `ZIPP_JITDUMP` the scan runs to completion and reports EVERY op it
    // has no arm for, rather than stopping at the first. The first-only report
    // was actively misleading when prioritising admission work: it names one op,
    // that op gets admitted, and the region is still declined by the next one —
    // so a change that looked like it would unblock a region unblocked nothing.
    // Behaviour is unchanged when the flag is off (`dump` is false, and every
    // rejection returns immediately exactly as before).
    let dump = std::env::var_os("ZIPP_JITDUMP").is_some();
    let mut ok = true;
    macro_rules! reject {
        ($($arg:tt)*) => {{
            if dump {
                eprintln!($($arg)*);
                ok = false;
            } else {
                return false;
            }
        }};
    }
    // The back-edge must be an unconditional jump to the header (canonical
    // while/for shape). This guarantees no fall-through past `end`, so the only
    // out-of-region control transfers are explicit jump targets (loop exit /
    // break), which become exit stubs.
    match code[e] {
        Instr::Jump { target } if target == start => {}
        _ => return false,
    }
    for instr in &code[s..=e] {
        match *instr {
            Instr::LoadInt { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            // A `let`/`const` global write (TDZ-checked); inside a hot loop region the
            // binding is already initialized, so the JIT treats it like StoreGlobal.
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Div { .. }
            | Instr::Mod { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
            // Bitwise ops (`|`/`&`/`^`/`<<`/`>>`/`>>>`) — handled by the MEMORY
            // path (Int or exactly-integral-double operands; anything else
            // bails). The `(x + y) | 0` / `i & 7` idioms gate most real
            // object/method loops, so regions must admit them.
            | Instr::Bitwise { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::Jump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotLe { .. }
            // Heap property ops — handled by the MEMORY path via win64 helper
            // calls (the int/regalloc paths decline, so heap regions take the
            // mem path). A `Print`/etc. anywhere still rejects the region.
            // A strict-FORCED SetProp (a strict ClassTail region inside a
            // sloppy function) declines: the JIT slow path derives strictness
            // from the proto flag, which cannot see that region.
            | Instr::GetProp { .. }
            | Instr::SetProp { strict: false, .. }
            // Dense-array element read/write `a[i]` / `a[i]=v` — handled by the
            // MEMORY path via win64 helpers (the int/regalloc paths decline).
            | Instr::GetIndex { .. }
            | Instr::SetIndex { .. }
            // Read-modify-write key coercion (`o[k] += v`, `o[k]++`): a NUMBER
            // key on a non-nullish base is a plain move (the MEMORY path's
            // inline case); anything else bails to the interpreter.
            | Instr::ToPropKey { .. }
            // String concat (`s += …`) — handled by the MEMORY path via the
            // `jit_concat` / `jit_str_append` win64 helpers (the numeric
            // int/regalloc paths don't list them, so they decline → mem path).
            | Instr::StrConcat { .. }
            | Instr::StrAppendInPlace { .. }
            | Instr::Return { .. }
            | Instr::ReturnUndefined => {}
            // Method calls — handled by the MEMORY path. `arr.push(x)` /
            // `str.charCodeAt(i)` keep their dedicated win64 helpers; every
            // other `obj.m(…)` compiles to a `jit_call_method_ic` helper call
            // that consults the interpreter's per-site inline cache and
            // frame-calls the resolved plain user function (IC miss /
            // megamorphic / native callee → deopt to the interpreter at this
            // op; repeated deopts evict the region).
            Instr::CallMethod { .. } => {}
            // Plain calls `f(…)` — same protocol via `jit_call_ic`.
            Instr::Call { .. } => {}
            // Logical `!` — MEM path (Bool flips natively; anything else goes
            // through the `jit_truthy` helper).
            Instr::Not { .. } => {}
            // `Math.<op>(args…)` — MEM path. A 1-arg unary op (`abs`/`sqrt`/
            // `floor`/`sin`/…) loads its arg as a number (bails to the
            // interpreter — which runs ToNumber coercion — if not) and calls the
            // PURE `jit_math_unary` helper (the interpreter's exact `math_unary`,
            // so every JS quirk matches). A 2-arg op (`pow`/`atan2`/`imul`/
            // `min`/`max`/`hypot` with EXACTLY two args) uses `jit_math_two`.
            // Any other arity (variadic min/max/hypot, a 0-arg call) declines —
            // the interpreter handles it. The helpers run no user code and never
            // allocate (a non-numeric arg already bailed), so no pinned-pointer
            // re-fetch is needed.
            Instr::MathOp { op, argc, .. } => {
                // Exactly what `emit_math_op` implements — shared with Tier C's
                // check so the two admission lists cannot drift apart again.
                if !math_op_emittable(op, argc) {
                    reject!("[decline] MathOp arity {argc} op {op:?} at region [{start},{end}]");
                }
            }
            // `LoadBool` — materialise the boolean Value bits inline (a single
            // store; call-free, pure). Unblocks loops carrying a bool literal
            // (parser flags, `done=false`).
            Instr::LoadBool { .. } => {}
            // `undefined` / `null` as constants — one store of the canonical
            // bits each, the same shape as `LoadBool` above and call-free. Both
            // were simply absent, and the cost of that is not proportional to
            // how trivial they are: a single `LoadUndefined` declines the WHOLE
            // region, so map-set-heavy's largest loop ([39,110], 71 ops) ran
            // interpreted for want of three of them. The int/regalloc planners
            // still reject the region through their own catch-alls — these bits
            // are not an i64 or an f64 — so it takes the MEM path, exactly as
            // `LoadBool` does.
            Instr::LoadUndefined { .. } | Instr::LoadNull { .. } => {}
            // Fused `typeof a === "lit"` — MEM path via the PURE `jit_typeof_is`
            // helper (no alloc, no user code, total). The UNFUSED `TypeOf` is
            // still not admitted here: it allocates its result string, and after
            // this fusion the bare form is rare enough not to be worth the
            // refetch plumbing.
            Instr::TypeOfIs { .. } => {}
            // `Promise.resolve(x)` / `Number.is*(x)` at exactly one argument —
            // MEM path via `jit_static_fn`. `Promise.resolve` was async-
            // promise-chain's fill-loop's ONLY blocker (`a[j] =
            // Promise.resolve(j)` blacklisted the whole region, B38/B42); the
            // helper handles the non-heap-argument fast path (no user code, no
            // microtask) and deopts a heap argument to the interpreter's
            // identity/thenable protocol. Every other StaticFn keeps declining.
            Instr::StaticFn { op, argc, .. } => {
                use crate::bytecode::StaticFn as S;
                let ok = argc == 1
                    && matches!(
                        op,
                        S::PromiseResolve
                            | S::NumberIsInteger
                            | S::NumberIsNaN
                            | S::NumberIsFinite
                            | S::NumberIsSafeInteger
                    );
                if !ok {
                    reject!("[decline] StaticFn {op:?}/{argc} at region [{start},{end}]");
                }
            }
            // `CheckCoercible` — RequireObjectCoercible before a member access
            // (`objs[i&3].area()` emits one). MEM path: a null/undefined operand
            // bails to the interpreter (which throws the TypeError); any other
            // value is a pure no-op. Pure, call-free, no alloc — unblocks the
            // class-method-call loops (the GetIndex'd receiver is coerced before
            // the CallMethod).
            Instr::CheckCoercible { .. } => {}
            // Closure-cell / upvalue READS — MEM path via the pure `jit_cell_get`
            // / `jit_upval_get` helpers (a single heap LOAD of the cell's inner
            // Value; a TDZ cell → deopt sentinel → interpreter throws). Emitted
            // PER-OP (never hoisted across a Call/CallMethod), so a value an inner
            // closure mutated via a call in the SAME region is re-read on the next
            // execution. The helpers allocate nothing and run no user code, so no
            // pinned-pointer (r13/r14/TA) re-fetch is needed. Writes (`CellSet`,
            // `CellSetChecked`, `UpvalSet`) are NOT admitted — they keep declining.
            Instr::CellGet { .. } | Instr::UpvalGet { .. } => {}
            // Closure-cell / upvalue WRITES — same shape as the reads: one heap
            // store, no TDZ check (that is CellSetChecked, still declined), no
            // alloc, no user code. These used to decline, and one captured-local
            // assignment took the whole enclosing region down with it — they are
            // markdown-render's only region declines.
            Instr::CellSet { .. } | Instr::UpvalSet { .. } => {}
            // `+x` — a NUMBER passes straight through (the interpreter returns
            // the Value verbatim); anything else needs observable ToNumber
            // coercion and bails. It was simply absent from this list, which
            // declined sparse-array's for-in `keyFold` region outright.
            Instr::ToNum { .. } => {}
            // `obj["name" + i]` — the fused computed key. MEM path via
            // `jit_get_index_concat`, which handles only the own-DATA hit (no
            // alloc, no user code) and deopts otherwise.
            Instr::GetIndexConcat { .. } => {}
            // The fused computed-key WRITE and its evaluation-order shim.
            // Both MUST be admitted together with the fusion in
            // `compile/assign.rs`, or every loop that previously compiled its
            // `o["k" + i] = v` as Add+SetIndex would now DECLINE. ToConcatKey
            // is identity for primitives/strings (pure helper, deopts a real
            // coercion to the interpreter); SetIndexConcat handles the own
            // writable data-slot hit in place (scratch-formatted key, no
            // alloc, no version bump) and deopts a NEW key / exotic / string
            // key — exactly the cases the old Add+SetIndex pair also failed
            // to compile.
            Instr::ToConcatKey { .. } | Instr::SetIndexConcat { .. } => {}
            // `ForInLive` — the per-iteration for-in liveness check — MEM path via
            // the `jit_forin_live` helper (the shared `Vm::forin_live`; no getter
            // / Proxy trap fires, never re-enters the dispatch loop, so no GC safe
            // point — and it is GC-locked internally for belt-and-suspenders).
            // Emitted per-op (re-derives the live shape each execution). Lets
            // `for (k in obj)` loops over plain objects compile.
            Instr::ForInLive { .. } => {}
            // `HasProp` — the `in` operator — MEM path via the `jit_has_property`
            // helper (read-only `Vm::has_property_jit`, byte-identical to the
            // interpreter's `has_property_dyn` on a non-Proxy chain). Only a plain
            // `in` (`brand: false`) is admitted; the `#x in obj` ergonomic brand
            // check needs the private machinery → keeps declining. The helper runs
            // no user code and never allocates on the VM heap (a Proxy/exotic/
            // throwing case returns the deopt sentinel and the interpreter takes
            // over), so no r13/r14/TA refetch. Unblocks sparse-array's 8M
            // hole-aware `if (i in packed)` loops.
            Instr::HasProp { brand: false, .. } => {}
            Instr::HasProp { brand: true, .. } => {
                reject!("[decline] HasProp brand-check at region [{start},{end}]");
            }
            Instr::LoadConst { idx, .. } => {
                // Numeric constants run in the f64 region; a single-ASCII-char
                // string constant is resolvable to its interned slot (for
                // `s[i] === "x"` scans); a multi-char string constant is
                // accepted on the MEM path when its pre-interned bits are in
                // `const_strs`. Anything else rejects the region.
                //
                // BOTH string arms require `const_strs`, i.e. the MEM path. The
                // register paths (int/regalloc, which pass None) home values in
                // i64/f64 registers and have no way to hold a string: their
                // `emit_load_const` writes `v.bits()` straight into an xmm home,
                // and for a not-yet-interned constant those bits are the
                // `STRING_CONST_BIT | idx` SENTINEL, not a heap value. Admitting
                // a single-char literal there put that sentinel in a float
                // register, where it escaped on flush and was indexed as a heap
                // slot (`for (var s,i=0;i<20;i++) s="a"` aborted the process at
                // heap.rs), read as a NaN by arithmetic (so `1 < "2"` inside a
                // hot loop was always false), or flushed as a bogus value
                // (`typeof s` becoming "number"). MEM resolves it properly.
                match proto.constants.get(idx as usize) {
                    Some(c) if c.is_number() => {}
                    Some(&c)
                        if const_strs.is_some() && single_char_const_bits(proto, c).is_some() => {}
                    Some(_) if const_strs.is_some_and(|m| m.contains_key(&idx)) => {}
                    _ => {
                        reject!("[decline] non-region LoadConst at region [{start},{end}]");
                    }
                }
            }
            ref other => {
                reject!("[decline] {other:?} at region [{start},{end}]");
            }
        }
    }
    if !ok {
        return false;
    }
    // NOTE: helpers that can allocate (`StrConcat`/`StrAppendInPlace`) or run
    // user code (`Call`/`CallMethod`) USED to be forbidden alongside
    // GetProp/SetProp because the inline cache pins the heap version-array
    // pointer (r13) and the IC table pointer (r14), which an allocation /
    // a nested region compile can move. The memory path now RE-FETCHES those
    // pinned pointers after every such helper call instead (see
    // `emit_refetch_pinned`), so the mix is allowed.
    true
}

/// Max callee register count an inlined leaf may use (its body runs over a
/// scratch window carved above the caller frame; `jit_regs_fits` validates the
/// real window at host entry, so this compile-time cap only bounds the window).
pub(crate) const LEAF_MAX_REGS: u16 = 32;

/// Q4 leaf-call inlining eligibility: is `callee`'s body a leaf the region/Tier-C
/// emitter can inline over a scratch window? Returns the body ops to inline, or
/// `None` to decline (the Call keeps the per-call helper). v2 admits FORWARD
/// in-body branches (a converging diamond like `a && b && c`), dense-array reads,
/// `charCodeAt`, and comparisons — not just straight-line bodies.
///
/// Requirements (all NON-NEGOTIABLE for soundness — see `LeafInlinePlan`):
/// * Not a generator/async; reg_count ≤ 16; no rest/`arguments`; simple_params
///   (so arg binding is a plain positional copy, no defaults/destructuring).
/// * Body ops ⊂ a SAFE SUBSET of the region-admissible value/global ops, minus
///   anything that calls (`Call`/`CallMethod`/`Super*`), allocates on the VM
///   heap, reads a closure cell / upvalue (`Cell*`/`Upval*`), or touches the
///   `arguments`/heap property machinery. The subset below is exactly what the
///   inline emitter implements; any other op declines.
/// * Exactly ONE trailing `Return`/`ReturnUndefined`, reached by fall-through —
///   NO internal jump (straight-line; the inline emitter has no branch labels).
/// * NO deopt-capable op may appear AFTER an effect (`StoreGlobal*`): if an
///   inlined op bails, the interpreter re-runs the WHOLE call from the call ip,
///   so an effect that already ran would double-apply. (For v1 the only effect
///   admitted is `StoreGlobal*`; `SetProp`/`SetIndex` are NOT in the subset.)
pub fn callee_leaf_ok(callee: &FuncProto) -> Option<Vec<Instr>> {
    leaf_ok_impl(callee, false).map(|(body, _)| body)
}

/// Like `callee_leaf_ok`, but admits ONE `Call` in the body and reports its
/// index. Used for the nested (wrapper) inline: a one-line forwarder such as
///
/// ```text
/// function ri(n){ return (rnd() * n) | 0; }
/// ```
///
/// is not leaf-eligible — its body contains a `Call` — so a hot loop calling it
/// paid a real call per iteration even once `rnd` itself became inlinable. The
/// planner splices the inner callee's body in at that index behind its own
/// identity guard.
///
/// The admitted `Call` must precede any committed effect, so that a guard miss
/// can jump to the outer fallback (which re-runs the whole call) with nothing
/// applied yet. The body must also be branch-free: the splice renumbers ops, and
/// v1 does not remap branch targets.
pub fn callee_leaf_ok_one_call(callee: &FuncProto) -> Option<(Vec<Instr>, usize)> {
    let (body, call_at) = leaf_ok_impl(callee, true)?;
    Some((body, call_at?))
}

/// `ZIPP_NO_LEAF_GETPROP=1` drops `GetProp` back out of the leaf whitelist, so the
/// change can be A/B'd with `tools/bench.py --ab-env` against ONE binary — which
/// removes the fat-LTO code-layout confound that §2 warns about and that B70 had to
/// reason around.
fn leaf_getprop_enabled() -> bool {
    std::env::var_os("ZIPP_NO_LEAF_GETPROP").is_none()
}

fn leaf_ok_impl(callee: &FuncProto, allow_one_call: bool) -> Option<(Vec<Instr>, Option<usize>)> {
    if callee.is_generator || callee.is_async {
        return None;
    }
    if callee.rest_reg.is_some() || callee.arguments_reg.is_some() {
        return None;
    }
    if !callee.simple_params {
        return None;
    }
    // The inlined body runs over a scratch window carved above the caller frame;
    // `jit_regs_fits` validates `reg_window + callee_reg_count` at host entry (a
    // tight window → fallback), so a generous compile-time cap is sound. 32 covers
    // a CFG-ish leaf like `tokIs` (22 regs) plus headroom.
    if callee.reg_count > LEAF_MAX_REGS {
        return None;
    }
    let full = &callee.code;
    if full.is_empty() {
        return None;
    }
    // The body ends at the FIRST `Return`/`ReturnUndefined`: it is the UNIQUE exit
    // (forward in-body branches may skip ahead, but all converge here), and any op
    // after it is dead (the compiler appends a `ReturnUndefined` after an explicit
    // `Return`). Truncate there; `term == code.len()-1` is the single terminator.
    let term = full
        .iter()
        .position(|i| matches!(i, Instr::Return { .. } | Instr::ReturnUndefined))?;
    let code: Vec<Instr> = full[..=term].to_vec();
    // Every op except the terminator must be in the inline emitter's subset.
    // `seen_effect` enforces the side-effect-freedom-before-deopt ordering rule:
    // a deopt-capable op that bails re-runs the WHOLE call from the call ip, so no
    // such op may follow a committed effect (`StoreGlobal*`). The newly admitted
    // GetIndex / charCodeAt / Eq/Ne/Lt/Le/Gt/Ge are ALSO deopt-capable (they bail
    // via a helper deopt sentinel / a non-numeric operand), so they join that rule
    // — which keeps an effectful leaf like `mix` (a trailing `h = …`) sound while
    // admitting a pure CFG leaf like `tokIs`. Internal jumps must be FORWARD and
    // stay within the body (a back-edge = a loop, breaking deopt-idempotency and
    // the no-safepoint invariant).
    // An upvalue WRITE is admitted only in a BRANCH-FREE body, because the inline
    // emitter buffers it: `UpvalSet` emits nothing, later `UpvalGet`s of the same
    // index read the buffered register, and the cell is written ONCE after the
    // last op. That keeps a mid-body bail idempotent (nothing is committed yet,
    // so re-running the whole call is correct) — which is what lets a body like
    // mulberry32's `a = (a + K)|0; …imul…` inline at all, since its write comes
    // FIRST and is followed by deopt-capable arithmetic. Buffering is only valid
    // if the write is unconditional, hence no branches. Nothing can observe the
    // cell mid-body: the subset admits no calls.
    let branchy = code.iter().any(|i| {
        matches!(
            i,
            Instr::Jump { .. } | Instr::JumpIfFalse { .. } | Instr::JumpIfTrue { .. }
        )
    });
    let mut seen_effect = false;
    let mut call_at: Option<usize> = None;
    for (i, instr) in code.iter().enumerate() {
        let is_last = i == code.len() - 1;
        match *instr {
            // The single trailing return — IS the last op (by construction above).
            Instr::Return { .. } | Instr::ReturnUndefined => {
                debug_assert!(is_last);
            }
            _ if is_last => return None, // unreachable: last op is the terminator
            // ── deopt-capable value ops (may bail mid-body) ── forbidden AFTER
            // an effect (a bail would re-run the call and re-apply the effect).
            // The comparisons + dense-array read bail the same way (non-numeric
            // operand / a deopt sentinel from `jit_get_index`), so they join here.
            Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Div { .. }
            | Instr::Mod { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
            | Instr::Bitwise { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::GetIndex { .. }
            // An upvalue READ. Deopt-capable (a TDZ cell returns the deopt
            // sentinel), so it joins the effect-ordering rule. Reads only: an
            // upvalue WRITE is an effect that a mid-body bail would re-apply
            // when the call re-runs, and every remaining op after it would have
            // to be bail-free — `_ => return None` below still rejects
            // `UpvalSet`/`CellSet`/`StoreUpvalDyn`.
            | Instr::UpvalGet { .. } => {
                if seen_effect {
                    return None;
                }
            }
            // A NAMED property read, through the site-free `jit_get_prop_leaf`. Its
            // absence from this list is why a plain `f(o)` whose body reads `o.k`
            // was `(not leaf-eligible)`, so a hot loop calling it paid a full frame
            // call per iteration — 30.1ns against 7.0ns for the identical body
            // written as a METHOD, which the method inliner does inline (B73).
            //
            // Its own arm rather than joining the group above, because it carries an
            // env guard: `ZIPP_NO_LEAF_GETPROP=1` drops it back out so the change
            // can be measured with `--ab-env` on ONE binary. Deopt-capable (the
            // helper defers accessors, class chains and exotic receivers), so it
            // obeys the same effect-ordering rule.
            Instr::GetProp { .. } if leaf_getprop_enabled() => {
                if seen_effect {
                    return None;
                }
            }
            // ── the ONE nested call (wrapper inlining) ── admitted only for
            // `callee_leaf_ok_one_call`. Deopt-capable (the identity guard can
            // miss, and the spliced body can bail), so it obeys the effect rule;
            // branch-free because the splice renumbers ops without remapping
            // branch targets.
            Instr::Call { .. } if allow_one_call && !branchy && !seen_effect && call_at.is_none() => {
                call_at = Some(i);
            }
            // Buffered upvalue write — see `branchy` above. Deliberately does NOT
            // set `seen_effect`: nothing is committed to the cell until after the
            // body's last op, so deopt-capable ops may still follow it.
            Instr::UpvalSet { .. } => {
                if branchy || seen_effect {
                    return None;
                }
            }
            // `str.charCodeAt(i)` only — a read-only, alloc-free, no-user-code 1-arg
            // builtin (the inline emitter routes it through the `jit_char_code_at`
            // helper). Any other method call declines. Also deopt-capable.
            Instr::CallMethod { name, argc, .. } => {
                if argc != 1
                    || callee.string_constants.get(name as usize).map(|s| s.as_str())
                        != Some("charCodeAt")
                {
                    return None;
                }
                if seen_effect {
                    return None;
                }
            }
            // ── forward, in-body control flow ── a `Jump`/`JumpIf*` whose target
            // is strictly AHEAD of it and within the body (`> i && <= term`). A
            // backward edge would make the inlined body a loop over the caller
            // scratch window, breaking deopt-idempotency and the no-safepoint
            // invariant; an out-of-body target would escape the inline. Branches
            // do not bail, so the `seen_effect` rule does not gate them.
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. } => {
                let t = target as usize;
                if t <= i || t > term {
                    return None;
                }
            }
            // The inline emitter only implements the MathOp arities the region
            // path does (1-arg, or a fixed 2-arg op set).
            Instr::MathOp { op, argc, .. } => {
                if seen_effect {
                    return None;
                }
                let ok = match argc {
                    // See region_can_compile: `Math.imul(x)` (one arg) diverges
                    // (unary helper → NaN, interpreter → 0). Decline this leaf.
                    1 => !matches!(op, MathFn::Imul),
                    2 => matches!(
                        op,
                        MathFn::Pow
                            | MathFn::Atan2
                            | MathFn::Imul
                            | MathFn::Min
                            | MathFn::Max
                            | MathFn::Hypot
                    ),
                    _ => false,
                };
                if !ok {
                    return None;
                }
            }
            // ── pure, never-bail value/load ops ── safe anywhere.
            Instr::LoadInt { .. }
            | Instr::LoadConst { .. }
            | Instr::LoadBool { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. } => {}
            // ── the one admitted effect ── a global write. Must be the only
            // kind of effect; after it, no deopt-capable op may follow.
            Instr::StoreGlobal { .. }
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. } => {
                seen_effect = true;
            }
            // Anything else (calls, heap writes, closures, throw, …) declines —
            // the inline emitter doesn't implement it. Under `ZIPP_JITDECLINE=1`,
            // name the op: B74 found that a decline COUNT without the disqualifying
            // opcode makes the next whitelist candidate a guess, and guessing is
            // exactly what the flag exists to prevent.
            _ => {
                if std::env::var_os("ZIPP_JITDECLINE").is_some() {
                    eprintln!("[leaf-reject] {instr:?}");
                }
                return None;
            }
        }
    }
    // A `LoadConst` must be a NUMERIC constant (the inline emitter materialises
    // it as raw bits; a string/heap constant needs interning we don't do here).
    for instr in &code {
        if let Instr::LoadConst { idx, .. } = *instr {
            match callee.constants.get(idx as usize) {
                Some(c) if c.is_number() => {}
                _ => return None,
            }
        }
    }
    // `callee_leaf_ok_one_call` requires the call it was asked to find.
    if allow_one_call && call_at.is_none() {
        return None;
    }
    Some((code, call_at))
}

/// Detect a loop-invariant `g.length` to hoist out of a memory-path region: a
/// `GetProp{obj, name:"length"}` whose object is loaded from a global `g` that the
/// region never mutates (no `StoreGlobal(g)`, and no length-changing op anywhere —
/// `push`, `SetIndex`, `SetProp`). Then `g.length` is the same every iteration, so
/// it can be computed ONCE in the prologue rather than re-read (a helper call) per
/// iteration — the `for (i < s.length)` / `for (i < a.length)` idiom. Returns
/// `(get_ip, dst_reg, global_slot, name_idx)`, or `None` if no single such GetProp
/// qualifies (only the unique-GetProp case is hoisted, to keep it simple/safe).
pub(crate) fn hoistable_length(proto: &FuncProto, start: u32, end: u32) -> Option<(usize, u16, u32, u32)> {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    // The region must not change any container's length. A generic call
    // (`Call`, or any `CallMethod` other than the read-only `charCodeAt`)
    // can run ARBITRARY user code — which may mutate the container's length
    // or reassign the global holding it — so it rejects the hoist outright
    // (the per-iteration miss-helper read stays correct, just not hoisted).
    for instr in &code[s..=e] {
        match instr {
            Instr::SetIndex { .. } | Instr::SetProp { .. } | Instr::Call { .. } => return None,
            Instr::CallMethod { name, .. } => {
                if proto.string_constants.get(*name as usize).map(|s| s.as_str())
                    != Some("charCodeAt")
                {
                    return None;
                }
            }
            _ => {}
        }
    }
    // Exactly one `GetProp(_, "length")` in the region.
    let mut found: Option<(usize, u16, u16)> = None; // (ip, dst, obj)
    for ip in s..=e {
        if let Instr::GetProp { dst, obj, name } = code[ip] {
            if proto.string_constants.get(name as usize).map(|s| s.as_str()) == Some("length") {
                if found.is_some() {
                    return None; // more than one — bail
                }
                found = Some((ip, dst, obj));
            }
        }
    }
    let (get_ip, dst, obj) = found?;
    // The prologue writes `dst` DIRECTLY into the register file and then elides
    // the body `GetProp`, so the load must actually run on every pass. Without
    // this, `if (never) { n = arr.length; }` had `n` overwritten with the length
    // by a loop that never took the branch. Same requirement, and same argument,
    // as constant hoisting in `plan_region::runs_every_iteration`.
    if !runs_every_iteration(code, s, e, get_ip) {
        return None;
    }
    // `dst` must be written ONLY by this GetProp in the region.
    for ip in s..=e {
        if ip != get_ip && writes_reg(&code[ip]) == Some(dst) {
            return None;
        }
    }
    // `obj` must be defined in the region only by `LoadGlobal(g)` (same `g`), and
    // `g` never stored in the region.
    let mut g: Option<u32> = None;
    for ip in s..=e {
        match code[ip] {
            Instr::LoadGlobal { dst: ld, idx } if ld == obj => {
                if g.is_some() && g != Some(idx) {
                    return None; // obj loaded from two different globals
                }
                g = Some(idx);
            }
            Instr::StoreGlobal { idx, .. }
            | Instr::StoreGlobalStrict { idx, .. }
            | Instr::StoreGlobalResolved { idx, .. } => {
                if Some(idx) == g {
                    return None; // g mutated in the loop
                }
            }
            _ => {
                // `obj` defined by something other than LoadGlobal → not a global.
                if writes_reg(&code[ip]) == Some(obj) {
                    return None;
                }
            }
        }
    }
    let name_idx = match code[get_ip] {
        Instr::GetProp { name, .. } => name,
        _ => return None,
    };
    g.map(|g| (get_ip, dst, g, name_idx))
}

/// Addresses of the win64 heap helpers (vm.rs), the COMPILING function's id, and
/// the inline-cache base site index — threaded to the memory path so `GetProp`/
/// `SetProp` emit a call-free monomorphic inline cache (miss → helper).
#[derive(Clone, Copy)]
pub(crate) struct HeapHelpers {
    pub(crate) func_id: u32,
    pub(crate) get_prop_miss: usize,
    pub(crate) set_prop_miss: usize,
    /// Helper returning `vm.heap.versions_ptr()` (pinned in r13).
    pub(crate) versions_base: usize,
    /// Helper returning `vm.jit.ic_base_ptr()` (pinned in r14).
    pub(crate) ic_base: usize,
    /// Helper for a dense-array `GetIndex` (`a[i]`).
    pub(crate) get_index: usize,
    /// Helper for a dense-array `SetIndex` (`a[i] = v`).
    pub(crate) set_index: usize,
    /// Helper for `arr.push(x)`.
    pub(crate) array_push: usize,
    /// Helper for `str.charCodeAt(i)`.
    pub(crate) char_code_at: usize,
    /// Helper for `a + b` (`StrConcat`).
    pub(crate) concat: usize,
    /// Helper for in-place `a + b` (`StrAppendInPlace`).
    pub(crate) str_append: usize,
    /// Helper for a generic `obj.m(args…)` via the interpreter's per-site IC.
    pub(crate) call_method_ic: usize,
    /// Helper for a generic `f(args…)` via the interpreter's per-site IC.
    pub(crate) call_ic: usize,
    /// `PROP_VIA_IC` continuation for GetProp (accessor / class receiver).
    pub(crate) get_prop_slow: usize,
    /// `PROP_VIA_IC` continuation for SetProp.
    pub(crate) set_prop_slow: usize,
    /// Full `===` for non-interned heap operands (read-only, 0/1).
    pub(crate) strict_eq: usize,
    /// Full truthiness for non-Int/Bool conditions (read-only, 0/1).
    pub(crate) truthy: usize,
    /// TypedArray pin snapshot helper (see `HeapHelperAddrs::ta_snapshot`).
    pub(crate) ta_snapshot: usize,
    /// Uint8Clamped double-store helper (pure).
    pub(crate) ta_clamp_store: usize,
    /// Whitelisted DataView `get*` helper.
    pub(crate) dv_get: usize,
    /// Pure unary `Math.<op>` helper (MathFn code, f64 bits → f64 bits).
    pub(crate) math_unary: usize,
    /// Pure two-arg `Math.<op>` helper (MathFn code, f64 bits, f64 bits → f64 bits).
    pub(crate) math_two: usize,
    /// Pure `CellGet` helper (cell bits → inner Value bits / TDZ-deopt sentinel).
    pub(crate) cell_get: usize,
    /// `jit_str_index_of` intrinsic.
    pub(crate) str_index_of: usize,
    /// `jit_str_substring` intrinsic (substring/slice).
    pub(crate) str_substring: usize,
    /// `jit_coll_lookup` intrinsic (Map.get/has, Set.has).
    pub(crate) coll_lookup: usize,
    /// `UpvalGet` helper (upvalue idx → inner Value bits / TDZ-deopt sentinel).
    pub(crate) upval_get: usize,
    pub(crate) cell_set: usize,
    pub(crate) upval_set: usize,
    pub(crate) get_index_concat: usize,
    /// `ForInLive` helper (obj bits, key bits → Bool Value bits).
    pub(crate) forin_live: usize,
    /// `HasProp` (`in`) helper (key bits, obj bits → Bool Value bits / deopt).
    pub(crate) has_property: usize,
    /// Q4 leaf-inline entry headroom check (`jit_regs_fits`).
    pub(crate) regs_fits: usize,
    /// Tier C `TypeOf` helper (v bits → heap-string Value bits).
    pub(crate) typeof_str: usize,
    pub(crate) typeof_is: usize,
    pub(crate) static_fn: usize,
    pub(crate) to_concat_key: usize,
    pub(crate) set_index_concat: usize,
    /// Tier C `IsArray` helper (v bits → Bool bits / deopt sentinel).
    pub(crate) is_array: usize,
    /// Tier C `LenOf` helper (obj bits → length Value bits).
    pub(crate) len_of: usize,
    /// Tier C `ForInKeys` helper (obj bits → key-Array bits / deopt / threw).
    pub(crate) forin_keys: usize,
    /// First global inline-cache site id for this region; the k-th heap op uses
    /// `ic_base_idx + k`.
    pub(crate) ic_base_idx: u32,
}

/// Compile the loop region `[start, end]` (entered at `start`). Tries the
/// register-promoting path first (values live in xmm/gpr across the loop, no
/// per-op memory traffic — competitive with V8) and falls back to the simpler
/// memory-based path if the region's shape is outside the register allocator's
/// subset (e.g. it contains heap property ops). Returns `None` only if even the
/// fallback can't handle it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_region(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    heap: HeapHelpers,
    const_strs: &FxHashMap<u32, u64>,
    ta_plan: &TaPinPlan,
    leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
    method_plan: &FxHashMap<usize, MethodInlinePlan>,
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<JitFn> {
    // The register/SROA paths decline any region containing a Call/CallMethod, so
    // leaf inlining and method inlining (which apply only to those sites) are
    // reachable only via the memory path below.
    if let Some(f) = compile_region_regalloc(proto, start, end, globals_base_helper, ta_plan, heap.ta_snapshot, meter) {
        return Some(f);
    }
    compile_region_mem(proto, start, end, globals_base_helper, heap, const_strs, ta_plan, leaf_plan, method_plan, meter)
}

/// Compile a (rewritten, purely-numeric) field-promoted region via the integer or
/// double register path; returns `(code, is_int)`. Deliberately NOT the memory
/// path — the rewrite removed all heap ops, so if the register paths decline
/// (e.g. register pressure even with reuse), SROA is abandoned and the caller
/// falls back to the inline-cache mem path on the ORIGINAL bytecode.
pub(crate) fn compile_region_numeric(
    proto: &FuncProto,
    start: u32,
    end: u32,
    gh: usize,
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<(JitFn, bool)> {
    // SROA-rewritten code has no index ops, so an empty TA plan (no snapshot) is correct.
    if let Some(f) = compile_region_int(proto, start, end, gh, &TaPinPlan::default(), 0, meter) {
        return Some((f, true));
    }
    // SROA-rewritten code has no index ops, so an empty TA plan is correct here.
    compile_region_regalloc(proto, start, end, gh, &TaPinPlan::default(), 0, meter).map(|f| (f, false))
}

/// Clone `proto` and rewrite the region's heap ops to scratch field-globals so
/// the register paths can compile it: `GetProp(o.name) → LoadGlobal(dst, slot)`,
/// `SetProp(o.name, val) → StoreGlobal(slot, val)`, where `slot = pool_base + i`
/// and `i` is the field's index in `fp.fields`. The interpreter syncs each pool
/// slot ↔ the object's field around the native run (see `FieldSyncPlan`).
pub(crate) fn rewrite_for_field_promotion(
    proto: &FuncProto,
    start: u32,
    end: u32,
    fp: &FieldPromotePlan,
    pool_base: u32,
) -> FuncProto {
    let mut p = proto.clone();
    // Map a name-constant index to its pool slot BY FIELD STRING (fp.fields holds
    // one representative index per distinct field string).
    let slot_of = |name: u32| -> u32 {
        let s = &proto.string_constants[name as usize];
        let i = fp
            .fields
            .iter()
            .position(|&n| proto.string_constants[n as usize] == *s)
            .unwrap();
        pool_base + i as u32
    };
    for ip in start as usize..=end as usize {
        match p.code[ip] {
            Instr::GetProp { dst, name, .. } => {
                p.code[ip] = Instr::LoadGlobal { dst, idx: slot_of(name) };
            }
            Instr::SetProp { name, val, .. } => {
                p.code[ip] = Instr::StoreGlobal { idx: slot_of(name), src: val };
            }
            // The object-ref loads (`LoadGlobal o → r`) are now DEAD — their only
            // consumers (the heap ops above) no longer use `r`. Neutralise them to
            // `LoadInt 0` so the numeric path doesn't try to promote the object
            // global itself (a heap ref would fail its is-number entry guard, and
            // the whole region would bail). `r` stays dead/unread.
            Instr::LoadGlobal { dst, idx } if idx == fp.obj_global => {
                p.code[ip] = Instr::LoadInt { dst, val: 0 };
            }
            _ => {}
        }
    }
    p
}

