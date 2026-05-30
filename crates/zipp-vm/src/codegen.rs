//! Native x86-64 JIT (dynasm) for hot integer functions.
//!
//! This is the first JIT stage, built correctness-first. It compiles a
//! `FuncProto` to native code **only** when the whole function is expressible
//! as pure integer register computation with no calls (a "leaf int function"):
//! a hot numeric loop is the canonical case. Anything outside that set causes
//! the function to be REJECTED at compile time (it keeps running on the
//! interpreter), so a compiled function can never contain an op it doesn't
//! understand.
//!
//! ## Why this can't produce a wrong answer
//!
//! * Compile-time gating: `can_compile` walks the bytecode and refuses any op
//!   that isn't in the integer subset, or any `Call`. So the native code only
//!   ever runs ops it fully implements.
//! * Runtime type guard → bail: every arithmetic/compare op first checks that
//!   its operands are tagged `Int`. If not (a value became a double, string,
//!   etc.), the native code stops and returns a BAIL signal carrying the
//!   instruction index, and the interpreter resumes at exactly that ip with the
//!   register file already consistent (native code and interpreter share the
//!   same `regs` window). No silent fallthrough — the old engine's bug.
//! * Overflow → bail: integer add/sub/mul use the overflow flag; on overflow
//!   the op bails so the interpreter redoes it in the f64 domain. We NEVER
//!   truncate or wrap silently.
//!
//! ## ABI
//!
//! `extern "win64" fn(regs: *mut u64, bail_ip: *mut u32) -> u64`
//! * `rcx = regs` — pointer to this frame's register window (`Value` bits).
//! * `rdx = bail_ip` — out-param. Native writes `u32::MAX` here on a normal
//!   `Return` (and returns the result Value bits in rax), or the instruction
//!   index to resume at on a bail (rax is then ignored).
//!
//! Only `feature = "jit"` + `target_arch = "x86_64"` compiles this; other
//! configs fall back to the pure interpreter.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

use std::mem;

use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi, ExecutableBuffer};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::bytecode::{FuncProto, Instr};
use crate::value::Value;

/// Number of interpreter calls before a function is offered to the JIT.
pub const JIT_THRESHOLD: u32 = 8;

/// Sentinel in `bail_ip` meaning the native code completed via `Return` (the
/// result is in the returned `u64`). Any other value is the ip to resume at.
pub const NO_BAIL: u32 = u32::MAX;

/// Tag bits for an `Int` value, matching `value.rs` (`0x7FF9 << 48`). A boxed
/// i32 is `INT_TAG | (i32 as u32 as u64)`.
const INT_TAG: u64 = 0x7FF9_0000_0000_0000;
/// Top-16 pattern that identifies an Int (the high 16 bits of `INT_TAG`).
const INT_TAG_HI: u32 = 0x7FF9;

/// One compiled native function plus the buffer backing it.
pub struct JitFn {
    _buf: ExecutableBuffer,
    entry: *const u8,
}

impl JitFn {
    /// Raw native entry pointer (for self-recursive calls that re-enter the
    /// same code through the win64 trampoline).
    pub fn entry(&self) -> *const u8 {
        self.entry
    }

    /// Run the native code over `regs`. ABI: `(regs, bail_ip, vm) -> result`.
    /// Returns `(result_bits, bail_ip)`: `bail_ip == NO_BAIL` means a normal
    /// return with `result_bits`; otherwise the interpreter must resume at
    /// `bail_ip` (result_bits is meaningless).
    ///
    /// # Safety
    /// `regs` must point to at least the function's `reg_count` valid `Value`
    /// slots; `vm` must be a valid `*mut Vm`; the buffer outlives the call.
    pub unsafe fn run(&self, regs: *mut u64, vm: *mut core::ffi::c_void) -> (u64, u32) {
        let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
            mem::transmute(self.entry);
        let mut bail: u32 = NO_BAIL;
        let r = f(regs, &mut bail as *mut u32, vm);
        (r, bail)
    }
}

/// Per-function JIT state: call counts, compiled code, and a blacklist of
/// functions that aren't eligible (so we don't re-attempt them every tick).
#[derive(Default)]
pub struct Jit {
    counts: FxHashMap<u32, u32>,
    compiled: FxHashMap<u32, JitFn>,
    blacklist: FxHashSet<u32>,
}

impl Jit {
    pub fn new() -> Jit {
        Jit::default()
    }

    /// Look up compiled native code for `func_id`, if any.
    pub fn get(&self, func_id: u32) -> Option<&JitFn> {
        self.compiled.get(&func_id)
    }

    /// Record an interpreter entry into `func_id`. Returns `true` once it
    /// crosses the threshold and is neither compiled nor blacklisted — the
    /// caller should then attempt `compile`.
    pub fn record_and_should_compile(&mut self, func_id: u32) -> bool {
        if self.compiled.contains_key(&func_id) || self.blacklist.contains(&func_id) {
            return false;
        }
        let c = self.counts.entry(func_id).or_insert(0);
        *c += 1;
        *c == JIT_THRESHOLD
    }

    /// Attempt to compile `proto` (id `func_id`). On success it becomes
    /// available via `get`; on failure the id is blacklisted and never retried.
    /// `self_call_helper` is the address of the depth-guarded Rust trampoline
    /// the native code invokes for a self-recursive call (see vm.rs).
    pub fn compile(
        &mut self,
        func_id: u32,
        proto: &FuncProto,
        self_call_helper: usize,
        self_val_bits: u64,
    ) {
        if self.compiled.contains_key(&func_id) || self.blacklist.contains(&func_id) {
            return;
        }
        match compile_proto(proto, func_id, self_call_helper, self_val_bits) {
            Some(f) => {
                self.compiled.insert(func_id, f);
            }
            None => {
                self.blacklist.insert(func_id);
            }
        }
    }
}

/// Can this function be JIT-compiled in the current (leaf-int) subset? Rejects
/// any op outside the integer subset and any call/heap/closure/throw op.
///
/// `self_slot` is this function's own `name_global` (if it is a hoisted
/// top-level function). When present, the SELF-CALL pattern is allowed:
/// `LoadGlobal(self_slot) -> r` immediately followed by `Call{callee=r}`. That
/// lets a self-recursive integer function (fib) be compiled — the `LoadGlobal`
/// of the own slot is a no-op marker (its value is only the call target, which
/// the helper resolves), and the `Call` becomes a depth-guarded native recurse.
fn can_compile(proto: &FuncProto, self_slot: Option<u16>) -> bool {
    if proto.code.is_empty() {
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
            Instr::LoadGlobal { idx, .. } if Some(*idx as u16) == self_slot => {}
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
fn is_self_call(code: &[Instr], ip: usize, callee: u16, self_slot: Option<u16>) -> bool {
    let self_slot = match self_slot {
        Some(s) => s,
        None => return false,
    };
    for j in (0..ip).rev() {
        if let Some(w) = writes_reg(&code[j]) {
            if w == callee {
                return matches!(&code[j], Instr::LoadGlobal { idx, .. } if *idx as u16 == self_slot);
            }
        }
    }
    false
}

/// The destination register an instruction writes, if it writes exactly one.
fn writes_reg(i: &Instr) -> Option<u16> {
    match *i {
        Instr::LoadInt { dst, .. }
        | Instr::Move { dst, .. }
        | Instr::AddInt { dst, .. }
        | Instr::Add { dst, .. }
        | Instr::Sub { dst, .. }
        | Instr::Mul { dst, .. }
        | Instr::Lt { dst, .. }
        | Instr::Le { dst, .. }
        | Instr::Gt { dst, .. }
        | Instr::Ge { dst, .. }
        | Instr::Eq { dst, .. }
        | Instr::Ne { dst, .. }
        | Instr::LoadGlobal { dst, .. }
        | Instr::Call { dst, .. } => Some(dst),
        _ => None,
    }
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
fn compile_proto(
    proto: &FuncProto,
    self_func_id: u32,
    self_call_helper: usize,
    self_val_bits: u64,
) -> Option<JitFn> {
    let self_slot = proto.name_global;
    if !can_compile(proto, self_slot) {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;

    // A label per bytecode index so jumps resolve to the right native offset.
    // `labels[n]` is the fall-off-the-end label (treated as ReturnUndefined).
    let n = proto.code.len();
    let labels: Vec<_> = (0..=n).map(|_| ops.new_dynamic_label()).collect();
    // Shared epilogue: every Return/bail sets rax + [rsi] then jumps here, which
    // restores the stack frame and callee-saved regs before `ret`.
    let epilogue = ops.new_dynamic_label();

    // ── prologue ── save callee-saved regs, stash the 3 inputs, reserve shadow.
    // 3 pushes (24B) + sub 8 → 32B from a 16-aligned entry ⇒ 16-aligned, and
    // gives 32B shadow space below for helper calls (we sub 0x28 = 40 to also
    // hold the shadow region; 3 pushes + 40 = 64, 16-aligned).
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
        ; sub rsp, 32
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
            Instr::AddInt { dst, a, imm } => {
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
            Instr::LoadGlobal { dst, .. } => {
                // Only reached for `LoadGlobal(self_slot)` (can_compile gated).
                // Store the REAL self-function Value (embedded at compile time,
                // stable since hoisting). This matters when a self-`Call` deopts
                // to the interpreter: it resumes at the Call op and reads this
                // register as the callee, which must be the actual function.
                dynasm!(ops
                    ; mov rax, QWORD self_val_bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::Call { dst, arg_base, argc, .. } => {
                // Self-recursive call (can_compile verified callee == self_slot).
                // Marshal args, call the depth-guarded Rust helper, store result.
                emit_self_call(
                    &mut ops, ip, bail, self_func_id, self_call_helper, dst, arg_base, argc,
                );
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

    // ── epilogue ── undo the prologue and return (rax already holds the result
    // or 0-for-bail; [rsi] already holds NO_BAIL or the bail ip).
    dynasm!(ops
        ; => epilogue
        ; add rsp, 32
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
enum BinOp {
    Add,
    Sub,
    Mul,
}
#[derive(Clone, Copy)]
enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// Byte displacement of register `r` within the window (`[rcx + r*8]`).
#[inline]
fn dreg(r: u16) -> i32 {
    (r as i32) * 8
}

/// Emit this op's bail block at `bail`: the success path skips it; the block
/// records `ip` into `[rsi]` (bail_ip), then performs the FULL epilogue
/// (restore stack + callee-saved regs) and returns — a bare `ret` would leave
/// the prologue's pushes/`sub rsp` on the stack and corrupt the caller.
fn emit_bail(ops: &mut dynasmrt::x64::Assembler, ip: usize, bail: dynasmrt::DynamicLabel) {
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; jmp => done            // success path skips the bail block
        ; => bail
        ; mov DWORD [rsi], ip as i32
        ; xor rax, rax
        ; add rsp, 32
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
        ; => done
    );
}

/// Guard that `regs[r]` is tagged `Int`; on mismatch jump to `bail`. Reads the
/// high 16 bits and compares to `INT_TAG_HI`.
fn guard_int(ops: &mut dynasmrt::x64::Assembler, r: u16, bail: dynasmrt::DynamicLabel) {
    dynasm!(ops
        ; mov rax, [rbx + dreg(r)]
        ; shr rax, 48
        ; cmp eax, INT_TAG_HI as i32
        ; jne => bail
    );
}

/// Guard that `regs[r]` is Int OR Bool (both used as conditions). Int hi =
/// 0x7FF9, Bool hi = 0x7FFA. Accept either; else jump to `bail`.
fn guard_int_or_bool(ops: &mut dynasmrt::x64::Assembler, r: u16, bail: dynasmrt::DynamicLabel) {
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
fn box_eax(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    dynasm!(ops
        ; mov r8, QWORD INT_TAG as i64
        ; mov eax, eax            // zero-extend i32 payload into rax
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
    );
}

fn int_binop(
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
    }
    box_eax(ops, dst);
    emit_bail(ops, ip, bail);
}

/// `regs[dst] = (regs[a] <cmp> regs[b]) as Bool`. Guards both Int; bails else.
fn int_cmp(
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
fn jump_if_not_cmp(
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

/// DEOPT sentinel the self-call helper returns when it can't run the recursion
/// natively (depth limit, non-int arg, or callee not int-JIT'd). On seeing it,
/// the native code bails to the interpreter at this Call's ip so the call is
/// retried through the normal interpreter path. Chosen as a quiet-NaN tag value
/// no real `Value` produces (it is NOT a valid boxed Value).
pub const SELF_CALL_DEOPT: u64 = 0x7FFE_DEAD_BEEF_0000;

/// Emit a self-recursive call: `regs[dst] = self(regs[arg_base..arg_base+argc])`.
///
/// The args already sit contiguously in this frame's register window (the
/// compiler stages them there), so we pass `args_ptr = rbx + arg_base*8`
/// directly — no marshaling. Win64 call: rcx=vm, rdx=func_id, r8=args_ptr,
/// r9=argc. The helper (vm.rs `jit_self_call`) does the depth-guarded recursion
/// and returns the result Value bits, or `SELF_CALL_DEOPT` to bail. rbx/rsi/rdi
/// are callee-saved so they survive the call; 32B shadow space was reserved in
/// the prologue (rsp stays 16-aligned: prologue did 3 pushes + sub 40 = 64B).
fn emit_self_call(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    func_id: u32,
    helper: usize,
    dst: u16,
    arg_base: u16,
    argc: u16,
) {
    dynasm!(ops
        ; mov rcx, rdi                       // vm
        ; mov edx, func_id as i32            // func_id
        ; lea r8, [rbx + dreg(arg_base)]     // args_ptr (in the reg window)
        ; mov r9d, argc as i32               // argc
        ; mov rax, QWORD helper as i64
        ; call rax
        // rax = result bits OR SELF_CALL_DEOPT. Compare against the sentinel.
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => bail
        ; mov [rbx + dreg(dst)], rax
    );
    emit_bail(ops, ip, bail);
}
