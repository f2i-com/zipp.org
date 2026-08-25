// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// True if every op of `proto` is in the kernel's pure-arithmetic subset (no
/// calls / globals / heap ops / branches / `%` / non-int `LoadConst`). Callers
/// additionally check `param_count`. Stricter than `can_compile` (no self-call)
/// because the kernel must be call-free (no `regs` realloc under the window).
pub(crate) fn can_kernel_body(proto: &FuncProto) -> bool {
    proto.code.iter().all(|instr| {
        matches!(
            instr,
            Instr::LoadInt { .. }
                | Instr::Move { .. }
                | Instr::AddInt { .. }
                | Instr::Add { .. }
                | Instr::Sub { .. }
                | Instr::Mul { .. }
                | Instr::Div { .. }
                | Instr::Mod { .. }
                | Instr::Lt { .. }
                | Instr::Le { .. }
                | Instr::Gt { .. }
                | Instr::Ge { .. }
                | Instr::Eq { .. }
                | Instr::Ne { .. }
                | Instr::Return { .. }
                | Instr::ReturnUndefined
        )
    })
}

/// f64 binop for a kernel body (`regs[dst] = regs[a] <op> regs[b]`). Reuses the
/// region's number load/store; a non-number operand jumps to `bail`. No
/// overflow concept — JS numbers are f64, so this never wraps or deopts.
pub(crate) fn kmap_dbinop(
    ops: &mut dynasmrt::x64::Assembler,
    bail: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    op: DOp,
) {
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    match op {
        DOp::Add => dynasm!(ops ; addsd xmm0, xmm1),
        DOp::Sub => dynasm!(ops ; subsd xmm0, xmm1),
        DOp::Mul => dynasm!(ops ; mulsd xmm0, xmm1),
        DOp::Div => dynasm!(ops ; divsd xmm0, xmm1),
    }
    store_xmm(ops, dst);
}

/// f64 remainder for a kernel body (JS `%`): `a - trunc(a/b)*b` (truncated
/// quotient, sign of the dividend — JS semantics). `% 0` and `Infinity % b`
/// yield NaN exactly as in JS (Inf/NaN propagate through trunc/mul/sub). Uses
/// `roundsd` (SSE4.1, universal on x86-64). A non-number operand jumps to `bail`.
pub(crate) fn kmap_dmod(
    ops: &mut dynasmrt::x64::Assembler,
    bail: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
) {
    load_num_xmm(ops, a, 0, bail); // xmm0 = a
    load_num_xmm(ops, b, 1, bail); // xmm1 = b
    dynasm!(ops
        ; movsd xmm2, xmm0
        ; divsd xmm2, xmm1           // a / b
        ; roundsd xmm2, xmm2, 0b11   // truncate toward zero
        ; mulsd xmm2, xmm1           // trunc(a/b) * b
        ; subsd xmm0, xmm2           // a - trunc(a/b)*b
    );
    store_xmm(ops, dst);
}

/// f64 ordered comparison → Bool for a kernel body. Mirrors the region `dcmp`
/// (NaN compares false for </<=/>/>=/==, true for !=). Non-number → `bail`.
pub(crate) fn kmap_dcmp(
    ops: &mut dynasmrt::x64::Assembler,
    bail: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    cmp: Cmp,
) {
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    match cmp {
        Cmp::Lt => dynasm!(ops ; ucomisd xmm1, xmm0 ; seta al),
        Cmp::Le => dynasm!(ops ; ucomisd xmm1, xmm0 ; setae al),
        Cmp::Gt => dynasm!(ops ; ucomisd xmm0, xmm1 ; seta al),
        Cmp::Ge => dynasm!(ops ; ucomisd xmm0, xmm1 ; setae al),
        Cmp::Eq => dynasm!(ops ; ucomisd xmm0, xmm1 ; sete al ; setnp cl ; and al, cl),
        Cmp::Ne => dynasm!(ops ; ucomisd xmm0, xmm1 ; setne al ; setp cl ; or al, cl),
    }
    dynasm!(ops
        ; movzx rax, al
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
    );
}

/// How a kernel callback body op classifies: a value op already emitted, or a
/// `Return` the kernel must turn into its own result-commit.
pub(crate) enum KBody {
    Plain,
    Ret(u16),
    RetUndef,
}

/// Emit ONE op of a kernel callback body (straight-line — `can_kernel_body`
/// rejects branches). The window base is pinned in `rbx`; operands load via the
/// region f64 helpers (handling int-tagged AND double), bailing to `bail` on a
/// non-number. Returns `None` for an unsupported op (reject the kernel).
pub(crate) fn emit_kernel_arith(
    ops: &mut dynasmrt::x64::Assembler,
    instr: &Instr,
    bail: dynasmrt::DynamicLabel,
) -> Option<KBody> {
    match *instr {
        Instr::LoadInt { dst, val } => {
            // A small integer constant; load_num_xmm will cvtsi2sd it on use.
            let boxed = INT_TAG | (val as u32 as u64);
            dynasm!(ops ; mov rax, QWORD boxed as i64 ; mov [rbx + dreg(dst)], rax);
            Some(KBody::Plain)
        }
        Instr::Move { dst, src } => {
            dynasm!(ops ; mov rax, [rbx + dreg(src)] ; mov [rbx + dreg(dst)], rax);
            Some(KBody::Plain)
        }
        Instr::AddInt { dst, a, imm, .. } => {
            load_num_xmm(ops, a, 0, bail);
            dynasm!(ops ; mov eax, imm ; cvtsi2sd xmm1, eax ; addsd xmm0, xmm1);
            store_xmm(ops, dst);
            Some(KBody::Plain)
        }
        Instr::Add { dst, a, b } => {
            kmap_dbinop(ops, bail, dst, a, b, DOp::Add);
            Some(KBody::Plain)
        }
        Instr::Sub { dst, a, b } => {
            kmap_dbinop(ops, bail, dst, a, b, DOp::Sub);
            Some(KBody::Plain)
        }
        Instr::Mul { dst, a, b } => {
            kmap_dbinop(ops, bail, dst, a, b, DOp::Mul);
            Some(KBody::Plain)
        }
        Instr::Div { dst, a, b } => {
            kmap_dbinop(ops, bail, dst, a, b, DOp::Div);
            Some(KBody::Plain)
        }
        Instr::Mod { dst, a, b } => {
            kmap_dmod(ops, bail, dst, a, b);
            Some(KBody::Plain)
        }
        Instr::Lt { dst, a, b } => {
            kmap_dcmp(ops, bail, dst, a, b, Cmp::Lt);
            Some(KBody::Plain)
        }
        Instr::Le { dst, a, b } => {
            kmap_dcmp(ops, bail, dst, a, b, Cmp::Le);
            Some(KBody::Plain)
        }
        Instr::Gt { dst, a, b } => {
            kmap_dcmp(ops, bail, dst, a, b, Cmp::Gt);
            Some(KBody::Plain)
        }
        Instr::Ge { dst, a, b } => {
            kmap_dcmp(ops, bail, dst, a, b, Cmp::Ge);
            Some(KBody::Plain)
        }
        Instr::Eq { dst, a, b } => {
            kmap_dcmp(ops, bail, dst, a, b, Cmp::Eq);
            Some(KBody::Plain)
        }
        Instr::Ne { dst, a, b } => {
            kmap_dcmp(ops, bail, dst, a, b, Cmp::Ne);
            Some(KBody::Plain)
        }
        Instr::Return { src } => Some(KBody::Ret(src)),
        Instr::ReturnUndefined => Some(KBody::RetUndef),
        _ => None,
    }
}

/// Compile a fused native `map` kernel for callback `proto` (see the module
/// comment above for the ABI and safety model). `None` if the callback isn't
/// kernel-eligible (the caller then uses the ordinary per-element path).
pub(crate) fn compile_map_kernel(proto: &FuncProto) -> Option<JitFn> {
    if proto.param_count == 0 || proto.param_count > 2 || !can_kernel_body(proto) {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let loop_top = ops.new_dynamic_label();
    let loop_continue = ops.new_dynamic_label();
    let loop_done = ops.new_dynamic_label();
    let kernel_bail = ops.new_dynamic_label();
    let epilogue = ops.new_dynamic_label();
    let want_index = proto.param_count >= 2;

    // ── prologue ── pin window=rbx, snapshot=r13, len=r14, out=r15, i=r12.
    // 5 callee-saved pushes (40B) from an 8-mod-16 entry ⇒ rsp 16-aligned; the
    // kernel makes NO calls, so it needs no shadow space (no `sub rsp`).
    dynasm!(ops
        ; push rbx
        ; push r12
        ; push r13
        ; push r14
        ; push r15
        ; mov rbx, rcx                          // window base (cb register frame)
        ; mov r13, rdx                          // snapshot ptr
        ; mov r14, r8                           // len
        ; mov r15, r9                           // out ptr
        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
        ; mov [rbx], rax                        // window[0] = this = undefined (once)
        ; xor r12, r12                          // i = 0
        ; => loop_top
        ; cmp r12, r14
        ; jae => loop_done                      // i >= len ⇒ done (unsigned)
        ; mov rax, [r13 + r12*8]
        ; mov [rbx + 8], rax                    // window[1] = snapshot[i] (element)
    );
    if want_index {
        // window[2] = Int(i). i < 2^31 (caller gates len ≤ i32::MAX), so the
        // low 32 bits are the exact non-negative payload.
        dynasm!(ops
            ; mov eax, r12d
            ; mov rcx, QWORD INT_TAG as i64
            ; or rax, rcx
            ; mov [rbx + 16], rax
        );
    }

    // ── callback body (straight-line); each Return stores out[i] and continues.
    let mut returned = false;
    for instr in &proto.code {
        match emit_kernel_arith(&mut ops, instr, kernel_bail)? {
            KBody::Plain => {}
            KBody::Ret(src) => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [r15 + r12*8], rax    // out[i] = result
                    ; jmp => loop_continue
                );
                returned = true;
                break;
            }
            KBody::RetUndef => {
                dynasm!(ops
                    ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                    ; mov [r15 + r12*8], rax
                    ; jmp => loop_continue
                );
                returned = true;
                break;
            }
        }
    }
    if !returned {
        // Falling off the end behaves like ReturnUndefined; falls into the step.
        dynasm!(ops
            ; mov rax, QWORD Value::UNDEFINED.bits() as i64
            ; mov [r15 + r12*8], rax
        );
    }
    dynasm!(ops
        ; => loop_continue
        ; inc r12
        ; jmp => loop_top
        ; => loop_done
        ; mov rax, r14                          // processed = len (ran to completion)
        ; jmp => epilogue
        ; => kernel_bail
        ; mov rax, r12                          // processed = i (the tail runs [i,len))
        ; jmp => epilogue
        ; => epilogue
        ; pop r15
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rbx
        ; ret
    );

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn {
        _buf: buf,
        entry: entry_ptr,
        self_binding: None,
    })
}

/// Compile a fused native `reduce` kernel for callback `proto` (2-param
/// `(acc, element)`; see the module comment for the ABI). The accumulator lives
/// in `r15` across iterations and is only committed at the callback's `Return`,
/// so a callback that mutates its `acc` param mid-body and then bails can't
/// corrupt it. `None` if ineligible. Index-using (3-param) reduces fall back to
/// the per-element path.
pub(crate) fn compile_reduce_kernel(proto: &FuncProto) -> Option<JitFn> {
    if proto.param_count != 2 || !can_kernel_body(proto) {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let loop_top = ops.new_dynamic_label();
    let loop_continue = ops.new_dynamic_label();
    let loop_done = ops.new_dynamic_label();
    let kernel_bail = ops.new_dynamic_label();
    let epilogue = ops.new_dynamic_label();

    // ── prologue ── window=rbx, snapshot=r13, count=r14, acc_ptr=rdi, i=r12,
    // acc bits=r15. 6 callee-saved pushes; no calls ⇒ no shadow space.
    dynasm!(ops
        ; push rbx
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; push r15
        ; mov rbx, rcx                          // window base
        ; mov r13, rdx                          // snapshot ptr (already shifted past any seed)
        ; mov r14, r8                           // count
        ; mov rdi, r9                           // acc_inout ptr
        ; mov r15, [rdi]                         // acc = seed bits
        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
        ; mov [rbx], rax                        // window[0] = this = undefined (once)
        ; xor r12, r12                          // i = 0
        ; => loop_top
        ; cmp r12, r14
        ; jae => loop_done
        ; mov [rbx + 8], r15                     // window[1] = acc (param 0)
        ; mov rax, [r13 + r12*8]
        ; mov [rbx + 16], rax                    // window[2] = element (param 1)
    );

    let mut returned = false;
    for instr in &proto.code {
        match emit_kernel_arith(&mut ops, instr, kernel_bail)? {
            KBody::Plain => {}
            KBody::Ret(src) => {
                dynasm!(ops ; mov rax, [rbx + dreg(src)] ; mov r15, rax ; jmp => loop_continue);
                returned = true;
                break;
            }
            KBody::RetUndef => {
                dynasm!(ops ; mov r15, QWORD Value::UNDEFINED.bits() as i64 ; jmp => loop_continue);
                returned = true;
                break;
            }
        }
    }
    if !returned {
        dynasm!(ops ; mov r15, QWORD Value::UNDEFINED.bits() as i64);
    }
    dynasm!(ops
        ; => loop_continue
        ; inc r12
        ; jmp => loop_top
        ; => loop_done
        ; mov [rdi], r15                         // write final acc
        ; mov rax, r14                           // processed = count (complete)
        ; jmp => epilogue
        ; => kernel_bail
        ; mov [rdi], r15                         // write acc-so-far (unchanged this elem)
        ; mov rax, r12                           // processed = i (tail reduces [i,count))
        ; jmp => epilogue
        ; => epilogue
        ; pop r15
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rbx
        ; ret
    );

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn {
        _buf: buf,
        entry: entry_ptr,
        self_binding: None,
    })
}

/// Compile a fused native `filter` kernel for predicate `proto`. The predicate
/// runs inline per element; when it returns `true` the ELEMENT is appended to a
/// compacted output. The result MUST be a Bool (a comparison) — a non-Bool
/// predicate result (e.g. a bare number used for truthiness) bails that element
/// to the interpreter tail. `None` if ineligible.
///
/// ABI: `fn(window, snapshot, len, out, out_count: *mut usize) -> usize`. Returns
/// the count SCANNED (`len` = complete, `< len` = bailed there); writes the
/// count KEPT to `*out_count`. `out` capacity must be ≥ len.
pub(crate) fn compile_filter_kernel(proto: &FuncProto) -> Option<JitFn> {
    if proto.param_count == 0 || proto.param_count > 2 || !can_kernel_body(proto) {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let loop_top = ops.new_dynamic_label();
    let loop_continue = ops.new_dynamic_label();
    let loop_done = ops.new_dynamic_label();
    let kernel_bail = ops.new_dynamic_label();
    let epilogue = ops.new_dynamic_label();
    let want_index = proto.param_count >= 2;

    // ── prologue ── window=rbx, snapshot=r13, len=r14, out=r15, i=r12,
    // out_idx(kept)=rdi. The 5th arg (out_count ptr) stays on the stack and is
    // read at the exits. 6 callee-saved pushes; no calls ⇒ no shadow space.
    // After 6 pushes the 5th win64 arg sits at [rsp + 48 + 8(ret) + 32(shadow)].
    dynasm!(ops
        ; push rbx
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; push r15
        ; mov rbx, rcx                          // window base
        ; mov r13, rdx                          // snapshot ptr
        ; mov r14, r8                           // len
        ; mov r15, r9                           // out ptr
        ; xor rdi, rdi                          // out_idx (kept) = 0
        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
        ; mov [rbx], rax                        // window[0] = this = undefined
        ; xor r12, r12                          // i = 0
        ; => loop_top
        ; cmp r12, r14
        ; jae => loop_done
        ; mov rax, [r13 + r12*8]
        ; mov [rbx + 8], rax                    // window[1] = element
    );
    if want_index {
        dynasm!(ops
            ; mov eax, r12d
            ; mov rcx, QWORD INT_TAG as i64
            ; or rax, rcx
            ; mov [rbx + 16], rax               // window[2] = Int(i)
        );
    }

    let mut returned = false;
    for instr in &proto.code {
        match emit_kernel_arith(&mut ops, instr, kernel_bail)? {
            KBody::Plain => {}
            KBody::Ret(src) => {
                // Predicate result must be a Bool (high16 == 0x7FFA); a non-Bool
                // (number used for truthiness, etc.) bails to the interpreter
                // tail, which evaluates JS truthiness correctly.
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; cmp ecx, (INT_TAG_HI + 1) as i32   // 0x7FFA bool tag
                    ; jne => kernel_bail
                    ; test eax, eax                      // Bool payload: 0=false, 1=true
                    ; jz => loop_continue                // false ⇒ drop
                    ; mov rax, [r13 + r12*8]             // keep: out[kept++] = element
                    ; mov [r15 + rdi*8], rax
                    ; inc rdi
                    ; jmp => loop_continue
                );
                returned = true;
                break;
            }
            KBody::RetUndef => {
                // undefined ⇒ falsy ⇒ drop the element.
                dynasm!(ops ; jmp => loop_continue);
                returned = true;
                break;
            }
        }
    }
    if !returned {
        // Falling off the end ⇒ undefined ⇒ falsy ⇒ drop (fall into the step).
    }
    dynasm!(ops
        ; => loop_continue
        ; inc r12
        ; jmp => loop_top
        ; => loop_done
        ; mov rcx, [rsp + 88]                   // out_count ptr (5th arg)
        ; mov [rcx], rdi                        // *out_count = kept
        ; mov rax, r14                          // scanned = len (complete)
        ; jmp => epilogue
        ; => kernel_bail
        ; mov rcx, [rsp + 88]
        ; mov [rcx], rdi                        // kept so far
        ; mov rax, r12                          // scanned = i (tail filters [i,len))
        ; jmp => epilogue
        ; => epilogue
        ; pop r15
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rbx
        ; ret
    );

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn {
        _buf: buf,
        entry: entry_ptr,
        self_binding: None,
    })
}

// ════════════════════════════════════════════════════════════════════════════
// OSR loop-region JIT (double / SSE2)
//
// Unlike the whole-function int JIT above, this compiles a HOT LOOP REGION —
// the bytecode range `[start, end]` where `end` is an unconditional back-edge
// `Jump { target: start }` — even when the enclosing function (e.g. the
// top-level script with its console.log) is NOT wholly compilable. It is entered
// mid-execution (on-stack replacement) at the loop header `start`.
//
// ## Why doubles, not ints
//
// Real JS numeric loops overflow i32 fast (a sum to 50M reaches ~1.25e15). JS
// numbers ARE f64, so the region computes every value in xmm registers via SSE2
// (`addsd`/`mulsd`/`ucomisd`). A value is loaded as f64 from its NaN-boxed form:
// an Int-tagged value (`0x7FF9…`) is `cvtsi2sd`'d; a real double is `movq`'d;
// anything else (bool/null/undef/heap/string) BAILS. Arithmetic results are
// stored back as raw f64 bits (a "double" `Value`). No overflow concept ⇒ the
// loop never deopts on magnitude.
//
// ## Exit model (simpler than the function JIT)
//
// A loop region has no return value. EVERY exit — a clean loop exit (a jump
// whose target leaves `[start,end]`), a `Return`, or a type-guard bail — just
// records "resume interpreting at ip X" into `[rsi]` and returns. The shared
// `(result, bail_ip)` ABI already carries this: `bail_ip` is the resume ip
// (result is ignored). The interpreter resumes there with regs+globals already
// consistent (every write went straight through to memory).
//
// ## Direct globals
//
// Top-level `let`s bind to `vm.globals`, which is allocated once and never
// reallocates. The prologue calls a helper to fetch `globals.as_mut_ptr()` once
// and pins it in callee-saved `r12`, so `LoadGlobal`/`StoreGlobal` are direct
// `mov [r12 + idx*8]` — no per-access helper call.
//
// ## Stack frame
//
// 4 pushes (rbx, rsi, rdi, r12) + `sub rsp, 40`. From the 8-mod-16 entry: after
// 4 pushes rsp ≡ 8, after sub 40 rsp ≡ 0 (mod 16) — aligned for the prologue
// helper call (and any future heap-op helper), with 32B of shadow space.
