// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Tier C eligibility: is every op of `proto` in the v1 whole-function mem-path
/// subset? Stricter than `region_can_compile` (no GetProp/SetProp/StrConcat/
/// MathOp/Bitwise/Cell/etc. yet — those are later increments). Rejects
/// generators/async, rest/`arguments` (materialized by call setup, not emitted
/// code), and any op the emitter below doesn't implement.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn mem_can_compile(proto: &FuncProto, const_strs: &FxHashMap<u32, u64>) -> bool {
    if proto.code.is_empty() {
        return false;
    }
    if proto.is_generator || proto.is_async {
        if std::env::var_os("ZIPP_JITLOG").is_some() { eprintln!("[tierC-reject] generator/async"); }
        return false;
    }
    // A rest parameter's array / the `arguments` object are built by the
    // interpreter's call setup, not by emitted code — the native entry would skip
    // them. Stay interpreted.
    if proto.rest_reg.is_some() || proto.arguments_reg.is_some() {
        if std::env::var_os("ZIPP_JITLOG").is_some() { eprintln!("[tierC-reject] rest/arguments"); }
        return false;
    }
    // Under `ZIPP_JITDUMP` the scan runs to completion and reports EVERY op this
    // tier has no arm for, instead of stopping at the first. Reporting only the
    // first is actively misleading when prioritising: admitting `UpvalGet` here
    // moved markdown-render's blacklist count by exactly zero, because the same
    // three functions were also using `UpvalSet`, `join` and `push` — which the
    // first-only report had never shown. Behaviour is unchanged when the flag is
    // off.
    let dump = std::env::var_os("ZIPP_JITDUMP").is_some();
    let mut ok = true;
    macro_rules! reject {
        ($($arg:tt)*) => {{
            if dump {
                eprintln!($($arg)*);
                ok = false;
            } else {
                if std::env::var_os("ZIPP_JITLOG").is_some() { eprintln!($($arg)*); }
                return false;
            }
        }};
    }
    for instr in &proto.code {
        match *instr {
            Instr::LoadInt { .. }
            | Instr::LoadBool { .. }
            | Instr::LoadNull { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. }
            | Instr::GetIndex { .. }
            | Instr::GetProp { .. }
            // `o.x = v` — the region's 8-way IC write, minus the setter-inline
            // prefix and the TA refetch (Tier C has neither). The IC site budget
            // already counted SetProp (`compile`'s `n_sites` filter and the
            // desync assertion at the end of this function both name it), so the
            // op was gated out one line short of working. It is
            // class-prototype-hot's ONLY blacklisted function. A strict-FORCED
            // write (strict ClassTail region in a sloppy function) declines:
            // the slow helper reads strictness off the proto flag.
            | Instr::SetProp { strict: false, .. }
            | Instr::AddInt { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::TypeOf { .. }
            | Instr::TypeOfIs { .. }
            | Instr::IsArray { .. }
            | Instr::LenOf { .. }
            | Instr::ForInKeys { .. }
            | Instr::ForInLive { .. }
            | Instr::Jump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotLe { .. }
            | Instr::Return { .. }
            | Instr::ReturnUndefined
            // Bitwise / `!` — self-contained register ops with the same
            // ToInt32-or-bail contract the region path uses. Their absence here
            // was the single most common Tier C rejection across the benches
            // (10 functions over three of them, tied with UpvalGet), and it is
            // silent: the whole function is blacklisted and INTERPRETED for the
            // rest of the run, so one `h ^= h << 13` costs the entire body.
            | Instr::Bitwise { .. }
            | Instr::Not { .. }
            // `undefined` as a constant — a single store of the canonical bits,
            // the exact shape of the `LoadNull` arm right above. It was in
            // NEITHER admission list, which is what kept map-set-heavy's
            // [39,110] region interpreted: three `LoadUndefined`s were its only
            // remaining blocker on the mem path.
            | Instr::LoadUndefined { .. }
            // `/` — `dbinop` handles it on the always-f64 path (JS `/` has no
            // integer form), the same call the region path makes.
            | Instr::Div { .. }
            // In-place string append — B56's local-accumulator rewrite now
            // plants these INSIDE function bodies (markdown's escapeHtml and
            // renderInline), so Tier C rejecting the op un-compiled functions
            // it previously compiled. Same helper protocol as the region arm;
            // allocates (grows the heap) ⇒ the emitter refetches r13/r14 when
            // the function pins them.
            | Instr::StrAppendInPlace { .. } => {}
            // NOT admitted here, though the emitters exist below and the REGION
            // path (Tier B) has carried them since B10.3: `CellGet`/`UpvalGet`/
            // `CellSet`/`UpvalSet`. Admitting them MEASURED SLOWER — see B50.
            //
            // Each is a win64 CALL per op (`jit_upval_get` resolves the closure
            // from `frames.last()`, then a heap get and a match), which is the
            // same work the interpreter's arm does inline and without an FFI
            // boundary. In a Tier B loop region the surrounding ops are compiled
            // and it nets out positive; in Tier C the shape is a SMALL function
            // whose body is mostly the upvalue access, so the call overhead plus
            // the native entry/exit is all there is, and it loses:
            //
            //   in-file control, ratio against a Tier-C-compiled control arm
            //   (so machine load cancels), median of 3 interleaved rounds:
            //     one UpvalGet        4.16 -> 6.00   (+44%)
            //     UpvalGet + UpvalSet 6.10 -> 6.71   (+10%)
            //   against, in the same probe:
            //     MathOp + Div        8.23 -> 5.67   (-31%)
            //     SetProp            11.42 -> 9.42   (-18%)
            //
            // Reaching a tier is not the same as being faster in it. Do not
            // re-admit these without a per-op probe of that shape.
            // `Math.<op>(…)` — the shared `emit_math_op`, gated by the same
            // predicate the region path uses.
            Instr::MathOp { op, argc, .. } => {
                if !math_op_emittable(op, argc) {
                    reject!("[tierC-reject] MathOp arity {argc} op {op:?}");
                }
            }
            // General plain call `f(args…)` — `this = undefined`.
            Instr::Call { .. } => {}
            // Method calls: the INTRINSIC set only — the builtins with
            // dedicated pure win64 helpers, i.e. the region path's whitelist
            // minus its pin-dependent fast paths. NOT the generic
            // `jit_call_method_ic` route: that helper deopts on every NATIVE
            // callee, so admitting an arbitrary method name would turn a
            // `join`/`toUpperCase`-calling function into per-call
            // native-entry + bail — strictly worse than staying interpreted
            // (B50's Upval lesson: reaching a tier is not being faster in it).
            Instr::CallMethod { name, argc, .. } => {
                let key = proto.string_constants.get(name as usize).map(|s| s.as_str());
                let ok = matches!(
                    (key, argc),
                    (Some("charCodeAt"), 1)
                        | (Some("indexOf"), 1)
                        | (Some("push"), 1)
                        | (Some("get"), 1)
                        | (Some("has"), 1)
                        | (Some("substring"), 2)
                        | (Some("slice"), 2)
                );
                if !ok {
                    reject!("[tierC-reject] CallMethod {key:?} argc={argc}");
                }
            }
            // Numeric / single-ASCII-char / pre-interned multi-char string
            // constants only (mirrors the region's LoadConst gate). const_strs
            // holds every multi-char string const interned by the caller.
            Instr::LoadConst { idx, .. } => match proto.constants.get(idx as usize) {
                Some(c) if c.is_number() => {}
                Some(&c) if single_char_const_bits(proto, c).is_some() => {}
                _ if const_strs.contains_key(&idx) => {}
                _ => {
                    reject!("[tierC-reject] LoadConst (non-numeric, non-interned string)");
                }
            },
            ref other => {
                reject!("[tierC-reject] op {other:?}");
            }
        }
    }
    ok
}

/// Compile the WHOLE body of `proto` to native code via the memory-path op
/// emitters (Tier C). `globals_base_helper` pins r12 = `vm.globals` base;
/// `heap` carries the win64 helper addresses (get_index/char_code_at/call_ic/
/// strict_eq/truthy). Returns a `JitFn` with the standard ABI, or `None` if the
/// body is ineligible. v1 uses NO inline caches / TA pins / inline plans, so
/// r13/r14 are saved-but-unused and no post-call re-fetch is emitted (the
/// globals pin r12 stays valid across calls — `self.globals` never reallocates).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn compile_proto_mem(
    proto: &FuncProto,
    func_id: u32,
    globals_base_helper: usize,
    heap: HeapHelpers,
    const_strs: &FxHashMap<u32, u64>,
    leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<JitFn> {
    if !mem_can_compile(proto, const_strs) {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let n = proto.code.len();
    // A label per ip; `labels[n]` is the fall-off-the-end (ReturnUndefined). All
    // jump targets are in-function, so they resolve directly (no exit stubs).
    let labels: Vec<_> = (0..=n).map(|_| ops.new_dynamic_label()).collect();
    // Shared epilogue: every Return / bail records [rsi] then jumps here.
    let epilogue = ops.new_dynamic_label();
    // ── Q4 leaf-call inlining (Tier C) ── inline a monomorphic plain-leaf callee
    // at a Call site over a scratch window carved above the whole-function frame.
    let do_leaf = !leaf_plan.is_empty();
    let max_scratch_top: u64 = leaf_plan
        .values()
        .map(|p| p.reg_window as u64 + p.callee_reg_count as u64)
        .max()
        .unwrap_or(0);
    // 32B shadow + 8B 5th-arg slot = 40; + a 16B leaf-headroom-flag slot when
    // inlining (keeps the frame's 16-alignment after the 6 pushes).
    let frame: i32 = 40 + if do_leaf { 16 } else { 0 };
    // Byte offset of the headroom flag (1 = the carved window fits → inline; 0 =
    // fall back to the per-call helper). MUST equal the prologue store offset.
    let leaf_flag_off = frame - 8;

    // r13 (heap versions base) and r14 (JIT IC table base) are READ by the GetProp
    // inline-cache probe AND the leaf-inline identity version guard. Pin + post-
    // call/alloc refetch them iff this function has a GetProp OR inlines a leaf.
    // INVARIANT (the refetch obligation): r13 moves on EVERY heap allocation
    // (versions Vec push), r14 on a nested region compile (during user code); so
    // EVERY op that allocates or runs user code (Call, Add-concat, TypeOf,
    // ForInKeys, ForInLive, GetProp-slow) MUST `emit_refetch_pinned` after
    // committing its result. fn11/12/13 have GetIndex-but-no-GetProp (has_prop=
    // false), so folding do_leaf in is REQUIRED — else the leaf version guard
    // (`[r13+rcx*4]`) reads an unpinned r13.
    let has_prop = proto
        .code
        .iter()
        .any(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }));
    let refetch_pinned = has_prop || do_leaf;
    let refetch = refetch_pinned.then_some((heap.versions_base, heap.ic_base));

    // ── prologue ── save callee-saved regs, stash inputs, pin r12 = globals base.
    // Mirrors `compile_region_mem` (6 pushes + frame) so the region emitters and
    // the shared epilogue work verbatim. r13/r14 are saved (win64 requires it)
    // and pinned only when the function reads them (has GetProp).
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; sub rsp, frame
        ; mov rbx, rcx                    // regs base
        ; mov rsi, rdx                    // bail_ip out-pointer
        ; mov rdi, r8                     // vm
        ; mov rcx, rdi                    // arg0 = vm
        ; mov rax, QWORD globals_base_helper as i64
        ; call rax
        ; mov r12, rax                    // pinned globals base pointer
    );
    if refetch_pinned {
        // Pin the heap version-array base (r13) and the IC table base (r14) —
        // copied from the region prologue. Read by the GetProp IC probe and the
        // leaf-inline identity version guard.
        dynasm!(ops
            ; mov rcx, rdi
            ; mov rax, QWORD heap.versions_base as i64
            ; call rax
            ; mov r13, rax
            ; mov rcx, rdi
            ; mov rax, QWORD heap.ic_base as i64
            ; call rax
            ; mov r14, rax
        );
    }
    // ── Q4 leaf-inline headroom check (once per entry) ── `jit_regs_fits` → 1 if
    // every carved scratch window lies inside the pinned register file. Each
    // inlined Call site reads the flag and falls back to the helper on 0. rbx is
    // callee-saved; rcx/rdx/r8 are volatile scratch here.
    if do_leaf {
        dynasm!(ops
            ; mov rcx, rdi                            // vm
            ; mov rdx, rbx                            // caller window base
            ; mov r8, QWORD max_scratch_top as i64    // highest scratch slot used
            ; mov rax, QWORD heap.regs_fits as i64
            ; call rax
            ; mov [rsp + leaf_flag_off], rax          // 1 = inline ok, 0 = helper
        );
    }

    // The k-th GetProp/SetProp uses inline-cache site `ic_site` (advanced in the
    // GetProp arm). Reserved contiguously by `Jit::compile` via reserve_ic_sites.
    let mut ic_site = heap.ic_base_idx;
    let int_hint = true; // v1 admits no double-constant feeds.
    // Step metering (a metered VM only) — a Tier C body can loop just as a Tier
    // A one can, so it needs the same charge. See codegen::meter.
    let blocks = crate::codegen::meter::block_map(meter, &proto.code, 0, n - 1);
    let mut meter_stubs: Vec<(dynasmrt::DynamicLabel, usize)> = Vec::new();
    for ip in 0..n {
        dynasm!(ops ; => labels[ip]);
        if let Some((m, bl)) = blocks.as_ref() {
            if let Some(&len) = bl.get(&ip) {
                let stub = ops.new_dynamic_label();
                crate::codegen::meter::emit_charge(&mut ops, m, len, stub);
                meter_stubs.push((stub, ip));
            }
        }
        // Each op gets its OWN dedicated bail label (records THIS ip); a guard
        // miss resumes the interpreter exactly here, side-effect-free.
        let bail = ops.new_dynamic_label();
        match proto.code[ip] {
            Instr::LoadInt { dst, val } => {
                let boxed = INT_TAG | (val as u32 as u64);
                dynasm!(ops
                    ; mov rax, QWORD boxed as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadBool { dst, val } => {
                let bits = BOOL_TAG | (val as u64);
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadNull { dst } => {
                dynasm!(ops
                    ; mov rax, QWORD Value::NULL.bits() as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadUndefined { dst } => {
                dynasm!(ops
                    ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadConst { dst, idx } => {
                // Numeric / single-ASCII-char (interned slot) / pre-interned
                // multi-char string (bits rooted in jit_const_strings). Mirrors
                // the region LoadConst arm. mem_can_compile gated the kinds.
                let c = proto.constants[idx as usize];
                let bits = single_char_const_bits(proto, c)
                    .or_else(|| const_strs.get(&idx).copied())
                    .unwrap_or_else(|| c.bits());
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::Move { dst, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadGlobal { dst, idx } => {
                dynasm!(ops
                    ; mov rax, [r12 + (idx as i32) * 8]
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::StoreGlobal { idx, src }
            | Instr::StoreGlobalStrict { idx, src }
            | Instr::StoreGlobalResolved { idx, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [r12 + (idx as i32) * 8], rax
                );
            }
            Instr::AddInt { dst, a, imm, .. } => {
                // Int fast path (the interpreter's `checked_add`), f64 fallback
                // on a non-Int operand or overflow. (Copied from the mem path.)
                let f64_path = ops.new_dynamic_label();
                let done_ai = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; jne => f64_path
                    ; add eax, imm
                    ; jo => f64_path
                );
                box_eax(&mut ops, dst);
                dynasm!(ops ; jmp => done_ai ; => f64_path);
                load_num_xmm(&mut ops, a, 0, bail);
                dynasm!(ops
                    ; mov eax, imm
                    ; cvtsi2sd xmm1, eax
                    ; addsd xmm0, xmm1
                );
                store_xmm(&mut ops, dst);
                dynasm!(ops ; => done_ai);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Bitwise { dst, a, b, op } => {
                // ToInt32 both operands (Int payloads or exactly-integral
                // i32-range doubles — see `load_toint32`; everything else
                // bails), then the 32-bit op. x86 32-bit shifts mask the count
                // to 5 bits — exactly JS's `& 31`. Results always fit i32
                // (boxed Int) except `>>>`, whose u32 result may exceed
                // i32::MAX and is then boxed as an (exact) double.
                use crate::bytecode::BitwiseOp as B;
                load_toint32(&mut ops, a, bail);
                dynasm!(ops ; mov r8d, eax);             // stash a
                load_toint32(&mut ops, b, bail);
                dynasm!(ops ; mov ecx, eax ; mov eax, r8d); // eax = a, ecx = b
                match op {
                    B::And => {
                        dynasm!(ops ; and eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Or => {
                        dynasm!(ops ; or eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Xor => {
                        dynasm!(ops ; xor eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Shl => {
                        dynasm!(ops ; shl eax, cl);
                        box_eax(&mut ops, dst);
                    }
                    B::Shr => {
                        dynasm!(ops ; sar eax, cl);
                        box_eax(&mut ops, dst);
                    }
                    B::Ushr => {
                        let as_dbl = ops.new_dynamic_label();
                        let done_u = ops.new_dynamic_label();
                        dynasm!(ops
                            ; shr eax, cl
                            ; test eax, eax
                            ; js => as_dbl                // u32 > i32::MAX → double
                        );
                        box_eax(&mut ops, dst);
                        dynasm!(ops
                            ; jmp => done_u
                            ; => as_dbl
                            ; mov eax, eax                // zero-extend u32 into rax
                            ; cvtsi2sd xmm0, rax          // exact (< 2^32)
                            ; movq rax, xmm0
                            ; mov [rbx + dreg(dst)], rax
                            ; => done_u
                        );
                    }
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Not { dst, a } => {
                // `!x`: a Bool flips its payload bit in place (the tag survives
                // the xor); anything else asks the read-only `jit_truthy`
                // helper (handles Int/double/heap incl. empty strings and
                // [[IsHTMLDDA]]) and flips its 0/1.
                let slow = ops.new_dynamic_label();
                let done_n = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, (INT_TAG_HI + 1) as i32   // Bool tag 0x7FFA
                    ; jne => slow
                    ; xor rax, 1                          // flip the payload bit
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => done_n
                    ; => slow
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // value bits
                    ; mov rax, QWORD heap.truthy as i64
                    ; call rax
                    ; xor rax, 1                          // !truthy
                    ; mov r8, QWORD BOOL_TAG as i64
                    ; or rax, r8
                    ; mov [rbx + dreg(dst)], rax
                    ; => done_n
                );
            }
            Instr::Sub { dst, a, b } => {
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Sub, int_hint)
            }
            Instr::Mul { dst, a, b } => {
                // `dbinop` excludes Mul from the int fast path (always f64), so no
                // overflow concern.
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Mul, int_hint)
            }
            Instr::Div { dst, a, b } => {
                // Always f64 — JS `/` has no integer form (mirrors the region arm
                // and the interpreter).
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Div, false)
            }
            Instr::MathOp { dst, op, arg_base, argc } => emit_math_op(
                &mut ops, ip, bail, epilogue, dst, op, arg_base, argc,
                heap.math_unary, heap.math_two,
            ),
            Instr::Add { dst, a, b } => {
                // Int+Int fast path, then f64, then the `jit_concat` fallback
                // (string concat / coercion — the interpreter's `add_values`),
                // which may allocate / run user code ⇒ refetch r13/r14 when
                // has_prop. (Copied from the region Add arm.)
                let slow = ops.new_dynamic_label();
                let f64_path = ops.new_dynamic_label();
                let done_a = ops.new_dynamic_label();
                if int_hint {
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(a)]
                        ; mov rcx, [rbx + dreg(b)]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => f64_path
                        ; mov r10, rcx
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => f64_path
                        ; add eax, ecx
                        ; jo => f64_path
                    );
                    box_eax(&mut ops, dst);
                    dynasm!(ops ; jmp => done_a);
                }
                dynasm!(ops ; => f64_path);
                load_num_xmm(&mut ops, a, 0, slow);
                load_num_xmm(&mut ops, b, 1, slow);
                dynasm!(ops ; addsd xmm0, xmm1);
                store_xmm(&mut ops, dst);
                dynasm!(ops
                    ; jmp => done_a
                    ; => slow
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]
                    ; mov r8, [rbx + dreg(b)]
                    ; mov rax, QWORD heap.concat as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                dynasm!(ops ; => done_a);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Lt { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Lt),
            Instr::Le { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Le),
            Instr::Gt { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Gt),
            Instr::Ge { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Ge),
            Instr::Eq { dst, a, b } => {
                region_poly_eq(&mut ops, ip, bail, epilogue, dst, a, b, false, heap.strict_eq)
            }
            Instr::Ne { dst, a, b } => {
                region_poly_eq(&mut ops, ip, bail, epilogue, dst, a, b, true, heap.strict_eq)
            }
            Instr::Jump { target } => {
                dynasm!(ops ; jmp => labels[target as usize]);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                // Int/Bool condition tests its payload directly; anything else
                // asks the read-only `jit_truthy` helper. (Copied from mem path.)
                let if_false = matches!(proto.code[ip], Instr::JumpIfFalse { .. });
                let t = labels[target as usize];
                let testit = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(cond)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32          // Int
                    ; je => testit
                    ; cmp r10d, (INT_TAG_HI + 1) as i32    // Bool
                    ; je => testit
                    ; mov rcx, rdi                         // vm
                    ; mov rdx, rax                         // value bits
                    ; mov rax, QWORD heap.truthy as i64
                    ; call rax                             // rax = 0/1
                    ; => testit
                    ; test eax, eax
                );
                if if_false {
                    dynasm!(ops ; jz => t);
                } else {
                    dynasm!(ops ; jnz => t);
                }
            }
            Instr::JumpIfNotLt { a, b, target } => {
                djump_if_not_cmp(&mut ops, ip, bail, epilogue, a, b, Cmp::Lt, labels[target as usize]);
            }
            Instr::JumpIfNotLe { a, b, target } => {
                djump_if_not_cmp(&mut ops, ip, bail, epilogue, a, b, Cmp::Le, labels[target as usize]);
            }
            Instr::GetIndex { dst, obj, key } => {
                // Generic element read `a[i]` via the win64 helper (dense arrays,
                // flat-ASCII strings, unpinned TypedArrays); `undefined` for
                // out-of-range, deopt sentinel for receivers/keys needing
                // interpreter semantics. No alloc / no user code → no re-fetch.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // array bits
                    ; mov r8, [rbx + dreg(key)]           // index bits
                    ; mov rax, QWORD heap.get_index as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::TypeOf { dst, a } => {
                // `typeof v` → a heap string (jit_typeof). Total (no deopt). The
                // downstream `=== "number"` compares by CONTENT (region_poly_eq
                // slow strict_eq), so a fresh alloc is correct. ALLOCATES ⇒
                // refetch r13/r14 after the store when has_prop.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // value bits
                    ; mov rax, QWORD heap.typeof_str as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
            }
            Instr::TypeOfIs { dst, a, code, neg } => {
                // Fused typeof compare — the region arm verbatim. PURE (no
                // alloc, no user code, total), so unlike `TypeOf` above it owes
                // no refetch.
                let code_neg = code as u32 | ((neg as u32) << 8);
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // value bits
                    ; mov r8d, code_neg as i32            // code | neg<<8
                    ; mov rax, QWORD heap.typeof_is as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax          // Bool Value bits
                );
            }
            Instr::IsArray { dst, a } => {
                // `Array.isArray(v)` → Bool bits; deopt sentinel for the rare
                // throwing case (revoked Proxy → interpreter re-executes + throws,
                // safe to redo — the check is side-effect-free). Pure, no refetch.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // value bits
                    ; mov rax, QWORD heap.is_array as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::LenOf { dst, obj } => {
                // For-in key-snapshot / array / string length. Pure, total — no
                // deopt, no alloc, no refetch.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // obj bits
                    ; mov rax, QWORD heap.len_of as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::ForInKeys { dst, obj } => {
                // Materialise the for-in key snapshot Array (jit_forin_keys).
                // ALLOCATES ⇒ refetch r13/r14 after the store when has_prop. A
                // Proxy trap / coercion throw → CALL_THREW → unwind (no redo).
                // Sentinel checks BEFORE the store (side-effect-free at bail).
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // obj bits
                    ; mov rax, QWORD heap.forin_keys as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::ForInLive { dst, obj, key } => {
                // Per-op for-in liveness (jit_forin_live → Vm::forin_live). Never
                // deopts. Can run a Proxy `has` trap (user code) ⇒ refetch r13/r14
                // after the store when has_prop. (Copied from the region arm.)
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // obj bits
                    ; mov r8, [rbx + dreg(key)]           // key bits
                    ; mov rax, QWORD heap.forin_live as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax          // Bool Value bits
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
            }
            Instr::GetProp { dst, obj, name } => {
                // 8-way inline cache (call-free on hit), then the miss helper,
                // then the PROP_VIA_IC slow path (accessor / class receiver — may
                // frame-call a getter ⇒ refetch r13/r14 after). Copied from the
                // region GetProp arm, minus the method-inline prefix + TA refetch
                // (Tier C has neither). r13/r14 are pinned in the prologue
                // (has_prop ⇒ refetch_pinned). See `IcEntry` for the layout.
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let probe = ops.new_dynamic_label();
                let next = ops.new_dynamic_label();
                let hit = ops.new_dynamic_label();
                let miss = ops.new_dynamic_label();
                let via_ic = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                let hop = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]          // receiver bits (probe-invariant)
                    ; lea r9, [r14 + off]                 // way 0 of this site
                    ; mov r8d, JIT_IC_WAYS as i32
                    ; => probe
                    ; cmp rax, [r9]                       // identity (empty 0 never matches)
                    ; jne => next
                    ; mov ecx, eax                        // recv heap idx (low 32)
                    ; mov edx, [r13 + rcx*4]              // live recv version
                    ; cmp edx, [r9 + 16]
                    ; jne => next
                    ; mov ecx, [r9 + 20]
                    ; shr ecx, 24                         // nhops (0 = own)
                    ; test ecx, ecx
                    ; jz => hit
                    ; lea r10, [r9 + 24]                  // hop cursor
                    ; => hop
                    ; mov edx, [r10]                      // hop heap idx
                    ; mov r11d, [r13 + rdx*4]             // live hop version
                    ; cmp r11d, [r10 + 4]
                    ; jne => next
                    ; add r10, 8
                    ; dec ecx
                    ; jnz => hop
                    ; => hit
                    ; mov rcx, [r9 + 8]                   // holder vals_ptr
                    ; mov edx, [r9 + 20]
                    ; and edx, 0x00FF_FFFF                // slot (low 24)
                    ; mov rax, [rcx + rdx*8]              // vals[slot] (CALL-FREE)
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => cont
                    ; => next
                    ; add r9, JIT_IC_STRIDE as i32
                    ; dec r8d
                    ; jnz => probe
                    ; jmp => miss
                    ; => miss
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits (rax survives the probe)
                    ; mov r8d, ic_site as i32             // site_idx
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov rax, QWORD heap.get_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD PROP_VIA_IC as i64
                    ; cmp rax, r10
                    ; je => via_ic
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => cont
                    // ── accessor / class receiver: the interpreter-IC slow helper
                    // resolves it (may frame-call a getter — user code).
                    ; => via_ic
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // caller window base
                    ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                    ; mov r9, QWORD (((name as u64) << 32) | obj as u64) as i64
                    ; mov rax, QWORD heap.get_prop_slow as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                // The miss/slow helpers may have allocated (versions Vec) or
                // frame-called a getter (nested compile) — re-derive r13/r14.
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                dynasm!(ops ; => cont);
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::SetProp { obj, name, val, strict: _ } => {
                // 8-way inline cache, CALL-FREE on hit. The region arm verbatim,
                // minus the setter-inline prefix and the TA refetch. Unlike
                // GetProp the helper only ever fills OWN ways here (identity +
                // receiver version fully guard an own writable data slot: any
                // redefinition / freeze / delete / proto change bumps the
                // version), so the probe has no hop chain to walk.
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let probe = ops.new_dynamic_label();
                let next = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]          // receiver bits
                    ; lea r9, [r14 + off]
                    ; mov r8d, JIT_IC_WAYS as i32
                    ; => probe
                    ; cmp rax, [r9]                       // identity
                    ; jne => next
                    ; mov ecx, eax                        // recv heap idx
                    ; mov edx, [r13 + rcx*4]              // live recv version
                    ; cmp edx, [r9 + 16]
                    ; jne => next
                    ; mov rcx, [r9 + 8]                   // vals_ptr
                    // Mask the hop count out of `slot_nhops` before indexing —
                    // see the region arm: an unmasked hop count would make this
                    // a wild STORE, not merely a wrong read.
                    ; mov edx, [r9 + 20]
                    ; and edx, 0x00FF_FFFF                // slot (low 24)
                    ; mov r10, [rbx + dreg(val)]          // val_bits
                    ; mov [rcx + rdx*8], r10              // vals[slot] = val (CALL-FREE)
                    ; jmp => cont
                    ; => next
                    ; add r9, JIT_IC_STRIDE as i32
                    ; dec r8d
                    ; jnz => probe
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits
                    ; mov r8, [rbx + dreg(val)]           // val_bits
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov QWORD [rsp + 32], ic_site as i32 // 5th arg: site_idx (stack)
                    ; mov rax, QWORD heap.set_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD PROP_VIA_IC as i64
                    ; cmp rax, r10
                    ; jne => cont
                    // ── setter / class receiver: interpreter-IC slow helper
                    // (may frame-call a setter — user code).
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // caller window base
                    ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                    ; mov r9, QWORD (((name as u64) << 32) | ((obj as u64) << 16) | val as u64) as i64
                    ; mov rax, QWORD heap.set_prop_slow as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                dynasm!(ops ; => cont);
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::CallMethod { dst, obj, name, arg_base, argc } => {
                // The intrinsic set (mem_can_compile gated) — the region path's
                // dedicated pure win64 helpers, minus its pin fast paths (Tier C
                // has no pins). Every helper here: receiver + arg bits in,
                // result bits out, deopt sentinel → bail; none runs user code,
                // and none moves the versions array (`push` grows the array's
                // own Vec) → no re-fetch anywhere in this arm.
                let key = proto.string_constants[name as usize].as_str();
                if argc == 2 {
                    // substring/slice: two args read from the contiguous window.
                    let is_slice = (key == "slice") as i32;
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                        ; lea r8, [rbx + dreg(arg_base)]      // &args[0..2]
                        ; mov r9d, is_slice
                        ; mov rax, QWORD heap.str_substring as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else if matches!(key, "get" | "has") {
                    // Map.get / Map.has / Set.has — the region arm verbatim:
                    // `has` retries as Set (op 2) before deopting; a wrong
                    // receiver kind deopts, so a same-named user method is
                    // unaffected (the interpreter runs it at this ip).
                    let opsel: i32 = if key == "get" { 0 } else { 1 };
                    let set_try = ops.new_dynamic_label();
                    let coll_done = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                        ; mov r8, [rbx + dreg(arg_base)]      // key bits
                        ; mov r9d, opsel
                        ; mov rax, QWORD heap.coll_lookup as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; jne => coll_done
                    );
                    if opsel == 1 {
                        dynasm!(ops
                            ; => set_try
                            ; mov rcx, rdi
                            ; mov rdx, [rbx + dreg(obj)]
                            ; mov r8, [rbx + dreg(arg_base)]
                            ; mov r9d, 2
                            ; mov rax, QWORD heap.coll_lookup as i64
                            ; call rax
                            ; mov r10, QWORD SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => bail
                        );
                    } else {
                        dynasm!(ops ; => set_try ; jmp => bail);
                    }
                    dynasm!(ops
                        ; => coll_done
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else {
                    // charCodeAt / indexOf / push — one-arg dedicated helpers.
                    let helper = match key {
                        "indexOf" => heap.str_index_of,
                        "push" => heap.array_push,
                        _ => heap.char_code_at,
                    };
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                        ; mov r8, [rbx + dreg(arg_base)]      // arg0 bits
                        ; mov rax, QWORD helper as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                }
            }
            Instr::Call { dst, callee, arg_base, argc } => {
                // General `f(args…)` (`this = undefined`) via the interpreter-IC
                // call helper. Packing: r9 = (callee<<16) | arg_base; argc on the
                // stack. The callee runs user code + allocates + can trigger a
                // nested compile ⇒ refetch r13/r14 after, when refetch_pinned (else
                // the next GetProp probe / leaf version guard reads a moved table).
                let packed_fip = ((func_id as u64) << 32) | ip as u64;
                let packed_args = ((callee as u64) << 16) | arg_base as u64;
                // Q4 leaf-call inlining: a monomorphic plain-leaf callee is inlined
                // with an identity guard; a guard miss / tight headroom falls through
                // to the SAME helper (a pure prefix). Tier C has no TA pins → no
                // ta_refetch.
                if let Some(lp) = leaf_plan.get(&ip) {
                    emit_inline_leaf_call(
                        &mut ops,
                        ip,
                        epilogue,
                        leaf_flag_off,
                        lp,
                        callee,
                        arg_base,
                        argc,
                        dst,
                        heap.math_unary,
                        heap.math_two,
                        // v2 body-op helpers (order matches the signature).
                        heap.get_index,
                        heap.char_code_at,
                        heap.strict_eq,
                        heap.truthy,
                        heap.call_ic,
                        packed_fip,
                        packed_args,
                        refetch,
                        None,
                    );
                } else {
                    emit_region_call_ic(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        heap.call_ic,
                        packed_fip,
                        packed_args,
                        argc,
                        dst,
                        refetch,
                        None,
                    );
                }
            }
            Instr::StrAppendInPlace { dst, a, b } => {
                // In-place `dst = a + b` (the linearity-proved accumulator
                // append). DEOPTS when the appended value needs real
                // ToPrimitive — the helper's purity gate runs BEFORE any
                // mutation, so the interpreter re-executes cleanly. Allocates
                // on the pure path ⇒ refetch r13/r14 when pinned.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // accumulator bits
                    ; mov r8, [rbx + dreg(b)]             // appended bits
                    ; mov rax, QWORD heap.str_append as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // needs ToPrimitive → interp
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Return { src } => {
                // Whole-function return: NO_BAIL + result Value (UNLIKE the region,
                // which records the ip and lets the interpreter perform the return).
                dynasm!(ops
                    ; mov DWORD [rsi], NO_BAIL as i32
                    ; mov rax, [rbx + dreg(src)]
                    ; jmp => epilogue
                );
            }
            Instr::ReturnUndefined => {
                dynasm!(ops
                    ; mov DWORD [rsi], NO_BAIL as i32
                    ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                    ; jmp => epilogue
                );
            }
            _ => return None, // mem_can_compile already filtered; defensive
        }
    }

    // Falling off the end behaves like ReturnUndefined.
    dynasm!(ops
        ; => labels[n]
        ; mov DWORD [rsi], NO_BAIL as i32
        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
        ; jmp => epilogue
    );

    // ── metering exits ── out of line, so the hot path is `sub` plus a
    // not-taken `jle`. Resuming at this ip is an ordinary bail.
    for (stub, ip) in meter_stubs {
        dynasm!(ops
            ; => stub
            ; mov DWORD [rsi], ip as i32
            ; xor rax, rax
            ; jmp => epilogue
        );
    }

    // ── epilogue ── restore and return; rax = result (or garbage on bail), [rsi]
    // = NO_BAIL or the resume ip. Mirrors `compile_region_mem`'s 6-pop epilogue.
    dynasm!(ops
        ; => epilogue
        ; add rsp, frame
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
    );

    // The IC-site cursor must have consumed exactly the sites `Jit::compile`
    // reserved (one per GetProp/SetProp). A mismatch ⇒ a GetProp's `[r14+off]`
    // probe reads past the reserved table (OOB / cross-site corruption).
    debug_assert_eq!(
        (ic_site - heap.ic_base_idx) as usize,
        proto.code.iter().filter(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. })).count(),
        "Tier C ic_site cursor desynced from reserved sites"
    );

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn { _buf: buf, entry: entry_ptr, self_binding: None })
}

