// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Can this function be JIT-compiled in the current (leaf-int) subset? Rejects
/// any op outside the integer subset and any call/heap/closure/throw op.
///
/// `self_slot` is this function's own `name_global` (if it is a hoisted
/// top-level function). When present, the SELF-CALL pattern is allowed:
/// `LoadGlobal(self_slot) -> r` immediately followed by `Call{callee=r}`. That
/// lets a self-recursive integer function (fib) be compiled — the `LoadGlobal`
/// of the own slot is a no-op marker (its value is only the call target, which
/// the helper resolves), and the `Call` becomes a depth-guarded native recurse.
pub(crate) fn can_compile(proto: &FuncProto, self_slot: Option<u32>) -> bool {
    if proto.code.is_empty() {
        return false;
    }
    // A rest parameter's array is materialized by the interpreter's call setup,
    // not by emitted code; the native entry would skip it. Stay interpreted.
    if proto.rest_reg.is_some() {
        return false;
    }
    let code = &proto.code;
    for (ip, instr) in code.iter().enumerate() {
        match instr {
            Instr::LoadInt { .. }
            | Instr::Move { .. }
            | Instr::AddInt { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Mod { .. }
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
            | Instr::Return { .. }
            | Instr::ReturnUndefined => {}
            // `LoadGlobal(self_slot)` is allowed only as the immediately-
            // preceding callee load of a self `Call` (checked at the Call).
            Instr::LoadGlobal { idx, .. } if Some(*idx) == self_slot => {}
            // A self-call: callee must be loaded from self_slot by the prior op.
            Instr::Call { callee, .. } => {
                if !is_self_call(code, ip, *callee, self_slot) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Is the `Call` at `ip` (with callee register `callee`) a self-call — i.e. was
/// `callee` produced by a `LoadGlobal(self_slot)` earlier with no intervening
/// write to that register? Conservative: scans backward for the nearest writer.
pub(crate) fn is_self_call(code: &[Instr], ip: usize, callee: u16, self_slot: Option<u32>) -> bool {
    let self_slot = match self_slot {
        Some(s) => s,
        None => return false,
    };
    for j in (0..ip).rev() {
        if let Some(w) = writes_reg(&code[j]) {
            if w == callee {
                return matches!(&code[j], Instr::LoadGlobal { idx, .. } if *idx == self_slot);
            }
        }
    }
    false
}

/// Is `reg`'s value at `ip` known to be an Int from a preceding int-producing op?
/// Finds the nearest backward writer of `reg`; it must be one of
/// `LoadInt`/`AddInt`/`Add`/`Sub`/`Mul`/`Mod` (each yields a boxed Int natively,
/// guarding its operands and bailing otherwise — so reaching `ip` natively proves
/// the value is an Int). SOUNDNESS: the writer must also DOMINATE the use along
/// the only entry path — i.e. no jump may land in `(writer_ip, ip]`, which would
/// let control reach `ip` bypassing the writer (possibly with a non-int value).
/// Conservative: returns false on any doubt. Lets the base-case-inline decision
/// skip a redundant int guard on `fib`'s `n-1` / `n-2` arguments.
pub(crate) fn arg_is_known_int(code: &[Instr], ip: usize, reg: u16) -> bool {
    let mut writer = None;
    for j in (0..ip).rev() {
        if let Some(w) = writes_reg(&code[j]) {
            if w == reg {
                writer = Some(j);
                break;
            }
        }
    }
    let w = match writer {
        Some(w) => w,
        None => return false,
    };
    let int_producing = matches!(
        &code[w],
        Instr::LoadInt { .. }
            | Instr::AddInt { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Mod { .. }
    );
    if !int_producing {
        return false;
    }
    // No branch may jump into (w, ip]: such an edge could reach `ip` without
    // executing the writer at `w`.
    for instr in code {
        let target = match *instr {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target as usize,
            _ => continue,
        };
        if target > w && target <= ip {
            return false;
        }
    }
    true
}

/// The destination register an instruction writes, if it writes exactly one.
pub(crate) fn writes_reg(i: &Instr) -> Option<u16> {
    match *i {
        Instr::LoadInt { dst, .. }
        | Instr::LoadConst { dst, .. }
        | Instr::Move { dst, .. }
        | Instr::AddInt { dst, .. }
        | Instr::Add { dst, .. }
        | Instr::Sub { dst, .. }
        | Instr::Mul { dst, .. }
        | Instr::Div { dst, .. }
        | Instr::Mod { dst, .. }
        | Instr::Neg { dst, .. }
        | Instr::Lt { dst, .. }
        | Instr::Le { dst, .. }
        | Instr::Gt { dst, .. }
        | Instr::Ge { dst, .. }
        | Instr::Eq { dst, .. }
        | Instr::Ne { dst, .. }
        | Instr::LoadGlobal { dst, .. }
        | Instr::GetProp { dst, .. }
        // GetIndex defines `dst`: needed so the regalloc path's dead/hoist passes
        // treat a pinned-TypedArray element load as a normal def (unboxed-region
        // epic). Inert elsewhere — the int path never sees GetIndex (region_is_int
        // rejects it) and an SROA region with a GetIndex isn't field-promotable.
        | Instr::GetIndex { dst, .. }
        | Instr::GetIndexConcat { dst, .. }
        | Instr::DeleteIndexConcat { dst, .. }
        | Instr::StrConcat { dst, .. }
        | Instr::StrAppendInPlace { dst, .. }
        | Instr::Bitwise { dst, .. }
        | Instr::Call { dst, .. }
        // MathOp and CallMethod DEFINE their dst. They used to fall through the
        // `_` arm below and report "writes nothing", so a register defined by
        // `Math.imul(..)` or a pinned-string `charCodeAt(..)` looked
        // never-defined to the home-unification passes and got coalesced onto an
        // unrelated global's home — in practice the loop counter's. That
        // produced a dropped store, a loop counter that took the accumulator's
        // value (`i` ending at 61 instead of 20), an infinite loop when the
        // aliased counter could never reach its bound, and an `xh` panic when
        // the register had no home at all.
        | Instr::MathOp { dst, .. }
        | Instr::CallMethod { dst, .. } => Some(dst),
        // ToPropKey DEFINES its dst (for a numeric key it is the identity — the
        // regalloc tier emits it as a register copy). Absent from this list, the
        // dst looked never-defined, landed in `ro_live_in`, and one `x[i] *= v`
        // declined its whole loop to the boxed MEM tier — the only site in the
        // suite where "read-only live-in used where a number isn't required"
        // fired (typedarray-math's normalize phase).
        | Instr::ToPropKey { dst, .. } => Some(dst),
        _ => None,
    }
}

/// If this single-parameter function opens with a base case of the shape
/// `if (param <cmp> K) return param;` — i.e. it returns its argument UNCHANGED
/// for small inputs — report `(cmp, K)`. A self-call to such a function can then
/// inline the base case at the call site (`arg <cmp> K ? arg : recurse`),
/// eliminating the call + prologue/epilogue for every LEAF invocation (about
/// half of `fib`'s calls). The recognised shape is exactly what `fib` compiles
/// to (LoadInt K; compare param,K; JumpIfFalse; Move/Jump…; Return param); any
/// deviation returns `None`, so the optimization is opt-in and never wrong.
pub(crate) fn base_case_returns_arg(proto: &FuncProto) -> Option<(Cmp, i32)> {
    if proto.param_count != 1 {
        return None;
    }
    let code = &proto.code;
    if code.len() < 3 {
        return None;
    }
    // ip0: LoadInt{c, K}
    let (c, k) = match code[0] {
        Instr::LoadInt { dst, val } => (dst, val),
        _ => return None,
    };
    // ip1: compare param (reg 1) against c → t. The reported Cmp is the one whose
    // TRUE branch selects the base case.
    let (cmp, t) = match code[1] {
        Instr::Lt { dst, a: 1, b } if b == c => (Cmp::Lt, dst),
        Instr::Le { dst, a: 1, b } if b == c => (Cmp::Le, dst),
        Instr::Gt { dst, a: 1, b } if b == c => (Cmp::Gt, dst),
        Instr::Ge { dst, a: 1, b } if b == c => (Cmp::Ge, dst),
        _ => return None,
    };
    // ip2: JumpIfFalse{t, _} — when (param<cmp>K) is FALSE we leave for the
    // recursive body, so the base case is the FALL-THROUGH (ip3).
    match code[2] {
        Instr::JumpIfFalse { cond, .. } if cond == t => {}
        _ => return None,
    }
    // Base path from ip3: follow Move/Jump to a Return whose source traces back
    // to the param (reg 1). Bounded walk; any other op disqualifies.
    let mut ip = 3usize;
    let mut ret_reg: u16 = 1; // register currently holding the (copied) param
    for _ in 0..8 {
        match code.get(ip)? {
            Instr::Move { dst, src } => {
                if *src == ret_reg {
                    ret_reg = *dst;
                } else if *dst == ret_reg {
                    return None; // our tracked value was overwritten
                }
                ip += 1;
            }
            Instr::Jump { target } => ip = *target as usize,
            Instr::Return { src } => {
                return if *src == ret_reg { Some((cmp, k)) } else { None };
            }
            _ => return None,
        }
    }
    None
}

/// Win64 register plan (integer subset):
/// * `rcx` = regs base pointer (preserved across the body; we never clobber it
///   because we issue no calls).
/// * `rdx` = bail_ip out-pointer (preserved likewise).
/// * `rax`, `r8`, `r9`, `r10`, `r11` = scratch (all volatile under win64, and
///   we make no calls, so no save needed).
///
/// Because a self-call invokes a Rust helper (which clobbers the volatile
/// argument registers), the prologue moves the three inputs into NON-VOLATILE
/// (callee-saved) registers that survive any helper call:
/// * `rbx` = regs base pointer   (was rcx)
/// * `rsi` = bail_ip out-pointer (was rdx)
/// * `rdi` = vm pointer          (was r8)
/// A register `Value` lives at `[rbx + reg*8]`. We push/pop rbx/rsi/rdi and keep
/// the stack 16-byte aligned, reserving 32 bytes of shadow space for any helper
/// call (win64 requires the caller to provide it).
pub(crate) fn compile_proto(
    proto: &FuncProto,
    self_func_id: u32,
    self_call_helper: usize,
    self_val_bits: u64,
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<JitFn> {
    let self_slot = proto.name_global;
    if !can_compile(proto, self_slot) {
        return None;
    }
    // If the callee (== self for our self-calls) returns its argument unchanged
    // for small inputs (`fib`: `n<2 ? n`), inline that base case at each call
    // site so leaf invocations skip the call + prologue/epilogue entirely.
    let base_case = base_case_returns_arg(proto);
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;

    // A label per bytecode index so jumps resolve to the right native offset.
    // `labels[n]` is the fall-off-the-end label (treated as ReturnUndefined).
    let n = proto.code.len();
    let labels: Vec<_> = (0..=n).map(|_| ops.new_dynamic_label()).collect();
    // Shared epilogue: every Return/bail sets rax + [rsi] then jumps here, which
    // restores the stack frame and callee-saved regs before `ret`.
    let epilogue = ops.new_dynamic_label();
    // The function's own entry (offset 0). A self-recursive `Call` issues a
    // DIRECT native call here (same win64 ABI as `JitFn::run`), skipping the Rust
    // trampoline on the clean hot path. The recursion runs on the native stack,
    // bounded by an inline depth guard (see `emit_self_call`).
    let self_entry = ops.new_dynamic_label();
    // Step metering (a metered VM only). A whole-function body can contain a
    // loop — `can_compile` whitelists every branch with no direction
    // restriction — so without this a JIT'd function is an unbounded native
    // path. Charges land out of line, next to the epilogue.
    let blocks = crate::codegen::meter::block_map(meter, &proto.code, 0, n - 1);
    let mut meter_stubs: Vec<(dynasmrt::DynamicLabel, usize)> = Vec::new();

    // ── prologue ── save callee-saved regs, stash the 3 inputs, reserve shadow.
    // 3 pushes (24B) + sub 48 = 72B; +8 (return addr) = 80 ⇒ 16-aligned. The 48
    // gives 32B shadow for helper calls PLUS a 4B callee bail slot (at [rsp+32])
    // for the inline self-call.
    dynasm!(ops
        ; => self_entry
        ; push rbx
        ; push rsi
        ; push rdi
        ; sub rsp, 48
        ; mov rbx, rcx        // regs base
        ; mov rsi, rdx        // bail_ip ptr
        ; mov rdi, r8         // vm ptr
    );

    for (ip, instr) in proto.code.iter().enumerate() {
        let ipl = labels[ip];
        dynasm!(ops ; => ipl);
        // Each op that can bail gets its OWN dedicated bail label (records this
        // ip). Threading it explicitly — rather than dynasm `>bail` local labels
        // — guarantees a guard jumps to THIS op's bail, never a neighbour's
        // (which would resume the interpreter at the wrong ip: a silent bug).
        let bail = ops.new_dynamic_label();
        if let Some((m, bl)) = blocks.as_ref() {
            if let Some(&len) = bl.get(&ip) {
                let stub = ops.new_dynamic_label();
                crate::codegen::meter::emit_charge(&mut ops, m, len, stub);
                meter_stubs.push((stub, ip));
            }
        }
        match *instr {
            Instr::LoadInt { dst, val } => {
                let boxed = INT_TAG | (val as u32 as u64);
                dynasm!(ops
                    ; mov rax, QWORD boxed as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::Move { dst, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::AddInt { dst, a, imm, .. } => {
                guard_int(&mut ops, a, bail);
                dynasm!(ops
                    ; mov eax, [rbx + dreg(a)]    // low 32 bits = i32 payload
                    ; add eax, imm
                    ; jo => bail
                );
                box_eax(&mut ops, dst);
                emit_bail(&mut ops, ip, bail);
            }
            Instr::Add { dst, a, b } => int_binop(&mut ops, ip, bail, dst, a, b, BinOp::Add),
            Instr::Sub { dst, a, b } => int_binop(&mut ops, ip, bail, dst, a, b, BinOp::Sub),
            Instr::Mul { dst, a, b } => int_binop(&mut ops, ip, bail, dst, a, b, BinOp::Mul),
            Instr::Mod { dst, a, b } => int_binop(&mut ops, ip, bail, dst, a, b, BinOp::Mod),
            Instr::Lt { dst, a, b } => int_cmp(&mut ops, ip, bail, dst, a, b, Cmp::Lt),
            Instr::Le { dst, a, b } => int_cmp(&mut ops, ip, bail, dst, a, b, Cmp::Le),
            Instr::Gt { dst, a, b } => int_cmp(&mut ops, ip, bail, dst, a, b, Cmp::Gt),
            Instr::Ge { dst, a, b } => int_cmp(&mut ops, ip, bail, dst, a, b, Cmp::Ge),
            Instr::Eq { dst, a, b } => int_cmp(&mut ops, ip, bail, dst, a, b, Cmp::Eq),
            Instr::Ne { dst, a, b } => int_cmp(&mut ops, ip, bail, dst, a, b, Cmp::Ne),
            Instr::Jump { target } => {
                dynasm!(ops ; jmp => labels[target as usize]);
            }
            Instr::JumpIfFalse { cond, target } => {
                // The condition is a Bool (from a compare) or an Int. Falsy ⇔
                // payload low-32 == 0 (Int 0 or Bool false). Guard Int|Bool else
                // bail (e.g. a double/heap cond needs the interpreter's truthy).
                guard_int_or_bool(&mut ops, cond, bail);
                dynasm!(ops
                    ; mov eax, [rbx + dreg(cond)]
                    ; test eax, eax
                    ; jz => labels[target as usize]
                );
                emit_bail(&mut ops, ip, bail);
            }
            Instr::JumpIfTrue { cond, target } => {
                guard_int_or_bool(&mut ops, cond, bail);
                dynasm!(ops
                    ; mov eax, [rbx + dreg(cond)]
                    ; test eax, eax
                    ; jnz => labels[target as usize]
                );
                emit_bail(&mut ops, ip, bail);
            }
            Instr::JumpIfNotLt { a, b, target } => {
                jump_if_not_cmp(&mut ops, ip, bail, a, b, Cmp::Lt, labels[target as usize]);
            }
            Instr::JumpIfNotLe { a, b, target } => {
                jump_if_not_cmp(&mut ops, ip, bail, a, b, Cmp::Le, labels[target as usize]);
            }
            Instr::LoadGlobal { .. } => {
                // `LoadGlobal(self_slot)` (can_compile gated) — only ever the
                // callee load of a self-`Call`. The native fast path calls the
                // entry DIRECTLY and never reads this register, so materialising
                // the self-Value here is dead work on every call (2 instr × ~96M
                // for fib(37)). SKIP it; `emit_self_call` lazily writes the callee
                // register on its cold (interpreter-bound) paths, where the
                // interpreter resume actually reads it. No-op here.
            }
            Instr::Call { dst, callee, arg_base, argc } => {
                // Self-recursive call (can_compile verified callee == self_slot).
                // Fast path: a DIRECT native call to this function's own entry
                // with an inline depth guard — no Rust trampoline. Cold paths
                // (depth limit, or the callee bailed mid-body) route to the Rust
                // helper / interpreter, which read `regs[callee]` — so they
                // restore it from `self_val_bits` first (the skipped LoadGlobal).
                //
                // BASE-CASE INLINING: when the callee returns its argument
                // unchanged for small inputs (`base_case`), test the guard here
                // and produce the result inline for the leaf case — no call. Only
                // for argc==1 (the recognised shape). A non-int arg or the
                // recursive case routes to the real call.
                match base_case {
                    Some((cmp, k)) if argc == 1 => {
                        let do_call = ops.new_dynamic_label();
                        let inline_base = ops.new_dynamic_label();
                        let after = ops.new_dynamic_label();
                        // Non-int arg → real call (which guards + bails correctly).
                        // Skip the guard when the arg provably came from an
                        // int-producing op (`fib`'s `n-1`/`n-2` from AddInt).
                        if !arg_is_known_int(&proto.code, ip, arg_base) {
                            guard_int(&mut ops, arg_base, do_call);
                        }
                        dynasm!(ops
                            ; mov eax, [rbx + dreg(arg_base)]   // arg payload (i32)
                            ; cmp eax, k
                        );
                        // Jump to the inline base case when `arg <cmp> k` is TRUE.
                        match cmp {
                            Cmp::Lt => dynasm!(ops ; jl => inline_base),
                            Cmp::Le => dynasm!(ops ; jle => inline_base),
                            Cmp::Gt => dynasm!(ops ; jg => inline_base),
                            Cmp::Ge => dynasm!(ops ; jge => inline_base),
                            Cmp::Eq | Cmp::Ne => unreachable!("base_case yields only Lt/Le/Gt/Ge"),
                        }
                        dynasm!(ops ; => do_call);
                        emit_self_call(
                            &mut ops, ip, bail, self_entry, self_func_id, self_call_helper, dst,
                            callee, arg_base, argc, proto.reg_count, self_val_bits,
                        );
                        dynasm!(ops
                            ; jmp => after
                            ; => inline_base
                            ; mov rax, [rbx + dreg(arg_base)]   // result = arg (base returns it)
                            ; mov [rbx + dreg(dst)], rax
                            ; => after
                        );
                    }
                    _ => {
                        emit_self_call(
                            &mut ops, ip, bail, self_entry, self_func_id, self_call_helper, dst,
                            callee, arg_base, argc, proto.reg_count, self_val_bits,
                        );
                    }
                }
            }
            Instr::Return { src } => {
                dynasm!(ops
                    ; mov DWORD [rsi], NO_BAIL as i32   // bail_ip = NO_BAIL
                    ; mov rax, [rbx + dreg(src)]        // result = regs[src]
                    ; jmp => epilogue
                );
            }
            Instr::ReturnUndefined => {
                let undef = Value::UNDEFINED.bits();
                dynasm!(ops
                    ; mov DWORD [rsi], NO_BAIL as i32
                    ; mov rax, QWORD undef as i64
                    ; jmp => epilogue
                );
            }
            _ => return None, // can_compile already filtered; defensive
        }
    }
    // Falling off the end behaves like ReturnUndefined (jumps to epilogue).
    dynasm!(ops
        ; => labels[n]
        ; mov DWORD [rsi], NO_BAIL as i32
        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
        ; jmp => epilogue
    );

    // ── metering exits ── out of line so the hot path is just `sub` + a
    // not-taken `jle`. Resuming at this ip is exactly an ordinary guard bail:
    // the interpreter re-runs the block, charging it as it goes.
    for (stub, ip) in meter_stubs {
        dynasm!(ops
            ; => stub
            ; mov DWORD [rsi], ip as i32
            ; xor rax, rax
            ; jmp => epilogue
        );
    }

    // ── epilogue ── undo the prologue and return (rax already holds the result
    // or 0-for-bail; [rsi] already holds NO_BAIL or the bail ip).
    dynasm!(ops
        ; => epilogue
        ; add rsp, 48
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
    );

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn { _buf: buf, entry: entry_ptr })
}

#[derive(Clone, Copy)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Mod,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// Byte displacement of register `r` within the window (`[rcx + r*8]`).
#[inline]
pub(crate) fn dreg(r: u16) -> i32 {
    (r as i32) * 8
}

/// Emit this op's bail block at `bail`: the success path skips it; the block
/// records `ip` into `[rsi]` (bail_ip), then performs the FULL epilogue
/// (restore stack + callee-saved regs) and returns — a bare `ret` would leave
/// the prologue's pushes/`sub rsp` on the stack and corrupt the caller.
pub(crate) fn emit_bail(ops: &mut dynasmrt::x64::Assembler, ip: usize, bail: dynasmrt::DynamicLabel) {
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; jmp => done            // success path skips the bail block
        ; => bail
        ; mov DWORD [rsi], ip as i32
        ; xor rax, rax
        ; add rsp, 48
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
        ; => done
    );
}

/// Guard that `regs[r]` is tagged `Int`; on mismatch jump to `bail`. Reads the
/// high 16 bits and compares to `INT_TAG_HI`.
pub(crate) fn guard_int(ops: &mut dynasmrt::x64::Assembler, r: u16, bail: dynasmrt::DynamicLabel) {
    dynasm!(ops
        ; mov rax, [rbx + dreg(r)]
        ; shr rax, 48
        ; cmp eax, INT_TAG_HI as i32
        ; jne => bail
    );
}

/// Guard that `regs[r]` is Int OR Bool (both used as conditions). Int hi =
/// 0x7FF9, Bool hi = 0x7FFA. Accept either; else jump to `bail`.
pub(crate) fn guard_int_or_bool(ops: &mut dynasmrt::x64::Assembler, r: u16, bail: dynasmrt::DynamicLabel) {
    let ok = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(r)]
        ; shr rax, 48
        ; cmp eax, INT_TAG_HI as i32
        ; je => ok
        ; cmp eax, (INT_TAG_HI + 1) as i32   // Bool tag
        ; jne => bail
        ; => ok
    );
}

/// Box the i32 in `eax` into `regs[dst]` as an Int Value.
pub(crate) fn box_eax(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    dynasm!(ops
        ; mov r8, QWORD INT_TAG as i64
        ; mov eax, eax            // zero-extend i32 payload into rax
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
    );
}

pub(crate) fn int_binop(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    op: BinOp,
) {
    guard_int(ops, a, bail);
    guard_int(ops, b, bail);
    dynasm!(ops
        ; mov eax, [rbx + dreg(a)]
        ; mov r9d, [rbx + dreg(b)]
    );
    match op {
        BinOp::Add => dynasm!(ops ; add eax, r9d ; jo => bail),
        BinOp::Sub => dynasm!(ops ; sub eax, r9d ; jo => bail),
        BinOp::Mul => dynasm!(ops ; imul eax, r9d ; jo => bail),
        // Signed integer remainder (JS `%` on integers; truncated, sign of the
        // dividend = idiv's remainder). `% 0` is NaN (not an Int) → bail; bail on
        // divisor -1 too, which sidesteps the INT_MIN/-1 idiv #DE (and `% -1` is
        // always 0, so the interpreter handles that rare case correctly).
        // `cdq` sign-extends eax into edx:eax; `idiv r9d` puts the remainder in
        // edx, which we move into eax for `box_eax`. (Division `/` is NOT done
        // here — JS `/` is float division, e.g. 7/2 == 3.5, not an integer.)
        // A zero remainder from a NEGATIVE dividend is -0 in JS (`-20 % 5`),
        // which is not an Int — bail and let the interpreter make the double.
        BinOp::Mod => dynasm!(ops
            ; test r9d, r9d
            ; jz => bail
            ; cmp r9d, -1
            ; je => bail
            ; test eax, eax
            ; js => bail
            ; cdq
            ; idiv r9d
            ; mov eax, edx
        ),
    }
    box_eax(ops, dst);
    emit_bail(ops, ip, bail);
}

/// `regs[dst] = (regs[a] <cmp> regs[b]) as Bool`. Guards both Int; bails else.
pub(crate) fn int_cmp(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    cmp: Cmp,
) {
    guard_int(ops, a, bail);
    guard_int(ops, b, bail);
    let bool_tag = INT_TAG + (1u64 << 48); // 0x7FFA…
    dynasm!(ops
        ; mov eax, [rbx + dreg(a)]
        ; mov r9d, [rbx + dreg(b)]
        ; cmp eax, r9d
    );
    match cmp {
        Cmp::Lt => dynasm!(ops ; setl al),
        Cmp::Le => dynasm!(ops ; setle al),
        Cmp::Gt => dynasm!(ops ; setg al),
        Cmp::Ge => dynasm!(ops ; setge al),
        Cmp::Eq => dynasm!(ops ; sete al),
        Cmp::Ne => dynasm!(ops ; setne al),
    }
    dynasm!(ops
        ; movzx rax, al
        ; mov r8, QWORD bool_tag as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
    );
    emit_bail(ops, ip, bail);
}

/// Fused `if !(regs[a] <cmp> regs[b]) goto target`. Guards both Int; bails else.
pub(crate) fn jump_if_not_cmp(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    a: u16,
    b: u16,
    cmp: Cmp,
    target: dynasmrt::DynamicLabel,
) {
    guard_int(ops, a, bail);
    guard_int(ops, b, bail);
    dynasm!(ops
        ; mov eax, [rbx + dreg(a)]
        ; mov r9d, [rbx + dreg(b)]
        ; cmp eax, r9d
    );
    // Jump to target when the comparison is FALSE.
    match cmp {
        Cmp::Lt => dynasm!(ops ; jge => target), // !(a<b) ⇔ a>=b
        Cmp::Le => dynasm!(ops ; jg => target),   // !(a<=b) ⇔ a>b
        _ => {}
    }
    emit_bail(ops, ip, bail);
}

