use std::collections::HashMap;
use std::mem;

use crate::rcode::ROp;

// NaN-boxing constants (same on all platforms — just u64 bit patterns)
const I32_SIG: u64 = 0x7FF9_0000_0000_0000;
const VAL_TRUE: u64 = 0x7FFA_0000_0000_0001;
const VAL_FALSE: u64 = 0x7FFA_0000_0000_0000;
#[allow(dead_code)]
const VAL_NULL: u64 = 0x7FFB_0000_0000_0000;
const VAL_UNDEFINED: u64 = 0x7FFC_0000_0000_0000;

pub const DJIT_THRESHOLD: u32 = 4;

fn read_u16(inst: &[u8], offset: usize) -> u16 {
    ((inst[offset] as u16) << 8) | (inst[offset + 1] as u16)
}

fn read_u32(inst: &[u8], offset: usize) -> u32 {
    ((inst[offset] as u32) << 24)
        | ((inst[offset + 1] as u32) << 16)
        | ((inst[offset + 2] as u32) << 8)
        | (inst[offset + 3] as u32)
}

fn read_u8(inst: &[u8], offset: usize) -> u8 {
    inst[offset]
}

fn const_i32(constants: &[crate::object::Object], idx: usize) -> Option<i32> {
    if idx >= constants.len() { return None; }
    match &constants[idx] {
        crate::object::Object::Integer(v) if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 => {
            Some(*v as i32)
        }
        _ => None,
    }
}

/// Check if a function can be compiled by the AArch64 JIT.
///
/// `Div` / `Mod` are deliberately excluded. On AArch64 `sdiv`
/// returns 0 on divide-by-zero (no trap), so the correctness
/// story is better than x86-64 — but the JIT still truncates
/// integer division (`5/2 → 2`) where the interpreter promotes
/// to f64 (`5/2 → 2.5`). Silently changing semantics once a
/// function gets JIT-compiled is a footgun, so for consistency
/// with the x86-64 decision, we reject it here too.
fn can_djit(instructions: &[u8], constants: &[crate::object::Object]) -> bool {
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => return false,
        };
        match op {
            ROp::LoadConst | ROp::LoadTrue | ROp::LoadFalse | ROp::LoadNull | ROp::LoadUndef
            | ROp::Move | ROp::GetGlobal | ROp::SetGlobal
            | ROp::Add | ROp::Sub | ROp::Mul
            | ROp::Equal | ROp::NotEqual | ROp::StrictEqual | ROp::StrictNotEqual
            | ROp::LessThan | ROp::LessOrEqual | ROp::GreaterThan | ROp::GreaterOrEqual
            | ROp::Neg | ROp::Not | ROp::IsNullish
            | ROp::Jump | ROp::JumpIfNot | ROp::JumpIfTruthy
            | ROp::Return | ROp::ReturnUndef | ROp::Halt | ROp::HaltValue
            | ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst
            | ROp::TestLtConstJump | ROp::TestLeConstJump
            | ROp::IncrementRegAndJump
            | ROp::TestLtRegJump | ROp::TestLeRegJump
            | ROp::ModRegConstStrictEqConstJump | ROp::TestModRegStrictEqConstJump
            | ROp::BitwiseAnd | ROp::BitwiseOr | ROp::BitwiseXor
            | ROp::LeftShift | ROp::RightShift | ROp::UnsignedRightShift
            | ROp::Call | ROp::CallGlobal | ROp::CallMethod | ROp::New
            | ROp::Array | ROp::SetIndex | ROp::Index
            | ROp::Hash | ROp::GetProp | ROp::SetProp
            => {}
            // Div / Mod intentionally rejected — see function doc comment.
            ROp::Div | ROp::Mod => return false,
            _ => return false,
        }
        if op == ROp::LoadConst {
            let idx = read_u16(instructions, ip + 3) as usize;
            if idx < constants.len() {
                match &constants[idx] {
                    crate::object::Object::Integer(_) => {}
                    crate::object::Object::CompiledFunction(_) => {}
                    crate::object::Object::BuiltinFunction(_) => {}
                    crate::object::Object::String(_) => {}
                    _ => return false,
                }
            }
        }
        ip += op.size();
    }
    true
}

/// See `crate::codegen::djit::x86_64::verify_truthiness_safe` for the
/// rationale and algorithm. Mirror the x86-64 logic — the bytecode is
/// arch-independent and the same JS truthiness semantics apply on
/// AArch64, where `JumpIfNot` / `JumpIfTruthy` / `Not` likewise emit
/// a single equality check against `VAL_TRUE`.
fn verify_truthiness_safe(instructions: &[u8]) -> bool {
    let mut is_target = vec![false; instructions.len() + 1];
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => return false,
        };
        match op {
            ROp::Jump => {
                let target = read_u32(instructions, ip + 1) as usize;
                if target <= instructions.len() { is_target[target] = true; }
            }
            ROp::JumpIfNot | ROp::JumpIfTruthy => {
                let target = read_u32(instructions, ip + 3) as usize;
                if target <= instructions.len() { is_target[target] = true; }
            }
            ROp::TestLtConstJump | ROp::TestLeConstJump => {
                let target = read_u32(instructions, ip + 5) as usize;
                if target <= instructions.len() { is_target[target] = true; }
            }
            ROp::TestLtRegJump | ROp::TestLeRegJump => {
                let target = read_u32(instructions, ip + 5) as usize;
                if target <= instructions.len() { is_target[target] = true; }
            }
            ROp::IncrementRegAndJump => {
                let target = read_u32(instructions, ip + 3) as usize;
                if target <= instructions.len() { is_target[target] = true; }
            }
            ROp::ModRegConstStrictEqConstJump => {
                let target = read_u32(instructions, ip + 7) as usize;
                if target <= instructions.len() { is_target[target] = true; }
            }
            ROp::TestModRegStrictEqConstJump => {
                let target = read_u32(instructions, ip + 7) as usize;
                if target <= instructions.len() { is_target[target] = true; }
            }
            _ => {}
        }
        ip += op.size();
    }

    let mut bool_regs: [bool; 256] = [false; 256];
    let mut ip = 0;
    while ip < instructions.len() {
        if is_target[ip] {
            for b in &mut bool_regs[..] { *b = false; }
        }
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => return false,
        };
        match op {
            ROp::JumpIfNot | ROp::JumpIfTruthy => {
                let cond = read_u16(instructions, ip + 1) as usize;
                if cond >= 256 || !bool_regs[cond] { return false; }
            }
            ROp::Not => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let src = read_u16(instructions, ip + 3) as usize;
                if src >= 256 || !bool_regs[src] { return false; }
                if dst < 256 { bool_regs[dst] = true; }
            }
            ROp::LoadTrue | ROp::LoadFalse => {
                let dst = read_u16(instructions, ip + 1) as usize;
                if dst < 256 { bool_regs[dst] = true; }
            }
            ROp::Equal | ROp::NotEqual | ROp::StrictEqual | ROp::StrictNotEqual
            | ROp::LessThan | ROp::LessOrEqual | ROp::GreaterThan | ROp::GreaterOrEqual
            | ROp::IsNullish => {
                let dst = read_u16(instructions, ip + 1) as usize;
                if dst < 256 { bool_regs[dst] = true; }
            }
            ROp::Move => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let src = read_u16(instructions, ip + 3) as usize;
                if dst < 256 {
                    bool_regs[dst] = src < 256 && bool_regs[src];
                }
            }
            ROp::LoadConst | ROp::GetGlobal | ROp::Add | ROp::Sub | ROp::Mul
            | ROp::Neg | ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst
            | ROp::BitwiseAnd | ROp::BitwiseOr | ROp::BitwiseXor
            | ROp::LeftShift | ROp::RightShift | ROp::UnsignedRightShift
            | ROp::LoadNull | ROp::LoadUndef
            | ROp::Call | ROp::CallGlobal | ROp::CallMethod | ROp::New
            | ROp::Array | ROp::Index | ROp::Hash | ROp::GetProp => {
                let dst = read_u16(instructions, ip + 1) as usize;
                if dst < 256 { bool_regs[dst] = false; }
            }
            _ => {}
        }
        ip += op.size();
    }
    true
}

fn count_call_opcodes(instructions: &[u8]) -> usize {
    let mut ip = 0;
    let mut count = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) { Some(op) => op, None => return count };
        if matches!(op, ROp::Call | ROp::CallGlobal | ROp::CallMethod | ROp::New) { count += 1; }
        ip += op.size();
    }
    count
}

fn intern_const(constants: &[crate::object::Object], idx: usize) -> Option<u32> {
    if idx >= constants.len() { return None; }
    match &constants[idx] {
        crate::object::Object::String(s) => Some(crate::intern::intern(s)),
        _ => None,
    }
}

/// Check if all Call/CallGlobal target self (for self-call optimization).
fn all_calls_are_self(instructions: &[u8], self_val: u64, globals_ptr: *const u64, globals_len: usize) -> (bool, Option<u16>) {
    let self_global_idx: Option<u16> = if !globals_ptr.is_null() {
        (0..globals_len).find(|&i| unsafe { *globals_ptr.add(i) } == self_val).map(|i| i as u16)
    } else { None };
    if self_global_idx.is_none() { return (false, None); }
    let expected_idx = self_global_idx.unwrap();
    let mut reg_global: [Option<u16>; 256] = [None; 256];
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) { Some(op) => op, None => return (false, None) };
        match op {
            ROp::GetGlobal => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let gidx = read_u16(instructions, ip + 3);
                if dst < 256 { reg_global[dst] = Some(gidx); }
            }
            ROp::Call => {
                let base = read_u16(instructions, ip + 3) as usize;
                match if base < 256 { reg_global[base] } else { None } {
                    Some(g) if g == expected_idx => {},
                    _ => return (false, None),
                }
            }
            ROp::CallGlobal => {
                if read_u16(instructions, ip + 3) != expected_idx { return (false, None); }
            }
            ROp::LoadConst | ROp::LoadTrue | ROp::LoadFalse | ROp::LoadNull | ROp::LoadUndef
            | ROp::Move | ROp::Add | ROp::Sub | ROp::Mul | ROp::Div | ROp::Mod
            | ROp::Neg | ROp::Not | ROp::Index
            | ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst => {
                let dst = read_u16(instructions, ip + 1) as usize;
                if dst < 256 { reg_global[dst] = None; }
            }
            _ => {}
        }
        ip += op.size();
    }
    (true, self_global_idx)
}

/// Find non-i32 registers (can't be pinned to 32-bit w-regs).
fn find_non_i32_registers(instructions: &[u8], constants: &[crate::object::Object]) -> [bool; 64] {
    let mut tainted = [false; 64];
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) { Some(op) => op, None => break };
        if op == ROp::LoadConst {
            let dst = read_u16(instructions, ip + 1) as usize;
            let cidx = read_u16(instructions, ip + 3) as usize;
            if dst < 64 && const_i32(constants, cidx).is_none() { tainted[dst] = true; }
        }
        if matches!(op, ROp::LoadTrue | ROp::LoadFalse | ROp::LoadNull | ROp::LoadUndef) {
            let dst = read_u16(instructions, ip + 1) as usize;
            if dst < 64 { tainted[dst] = true; }
        }
        if matches!(op, ROp::GreaterOrEqual | ROp::GreaterThan | ROp::LessThan | ROp::LessOrEqual
            | ROp::StrictEqual | ROp::StrictNotEqual | ROp::Equal | ROp::NotEqual) {
            let dst = read_u16(instructions, ip + 1) as usize;
            if dst < 64 { tainted[dst] = true; }
        }
        ip += op.size();
    }
    let mut changed = true;
    while changed {
        changed = false; ip = 0;
        while ip < instructions.len() {
            let op = match ROp::from_byte(instructions[ip]) { Some(op) => op, None => break };
            if op == ROp::Move {
                let (dst, src) = (read_u16(instructions, ip + 1) as usize, read_u16(instructions, ip + 3) as usize);
                if src < 64 && dst < 64 && tainted[src] && !tainted[dst] { tainted[dst] = true; changed = true; }
            }
            ip += op.size();
        }
    }
    tainted
}

/// Find up to 3 most-used i32-only VM registers for pinning to w24/w25/w26.
fn find_pinned_registers_3(instructions: &[u8], constants: &[crate::object::Object]) -> [Option<u16>; 3] {
    let non_i32 = find_non_i32_registers(instructions, constants);
    let mut freq: [u32; 64] = [0; 64];
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) { Some(op) => op, None => break };
        match op {
            ROp::Add | ROp::Sub | ROp::Mul | ROp::Mod
            | ROp::LessThan | ROp::LessOrEqual | ROp::GreaterThan | ROp::GreaterOrEqual
            | ROp::StrictEqual | ROp::StrictNotEqual | ROp::Equal | ROp::NotEqual => {
                let (dst, left, right) = (read_u16(instructions, ip + 1) as usize, read_u16(instructions, ip + 3) as usize, read_u16(instructions, ip + 5) as usize);
                if dst < 64 { freq[dst] += 1; } if left < 64 { freq[left] += 2; } if right < 64 { freq[right] += 2; }
            }
            ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst => {
                let (dst, src) = (read_u16(instructions, ip + 1) as usize, read_u16(instructions, ip + 3) as usize);
                if dst < 64 { freq[dst] += 1; } if src < 64 { freq[src] += 2; }
            }
            ROp::TestLtConstJump | ROp::TestLeConstJump | ROp::IncrementRegAndJump | ROp::ModRegConstStrictEqConstJump => {
                let r = read_u16(instructions, ip + 1) as usize;
                if r < 64 { freq[r] += 3; }
            }
            ROp::Move | ROp::Neg | ROp::Not => {
                let (dst, src) = (read_u16(instructions, ip + 1) as usize, read_u16(instructions, ip + 3) as usize);
                if dst < 64 { freq[dst] += 1; } if src < 64 { freq[src] += 1; }
            }
            _ => {}
        }
        ip += op.size();
    }
    for i in 0..64 { if non_i32[i] { freq[i] = 0; } }
    let mut best = [(0u16, 0u32); 3];
    for (i, &f) in freq.iter().enumerate() {
        if f > best[0].1 { best[2] = best[1]; best[1] = best[0]; best[0] = (i as u16, f); }
        else if f > best[1].1 { best[2] = best[1]; best[1] = (i as u16, f); }
        else if f > best[2].1 { best[2] = (i as u16, f); }
    }
    let mut result = [None; 3];
    for i in 0..3 { if best[i].1 >= 3 { result[i] = Some(best[i].0); } }
    result
}

fn needs_vm_ptr(instructions: &[u8]) -> bool {
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) { Some(op) => op, None => return false };
        match op {
            ROp::Call | ROp::CallGlobal | ROp::CallMethod | ROp::New
            | ROp::Array | ROp::SetIndex | ROp::Index
            | ROp::Hash | ROp::GetProp | ROp::SetProp
            | ROp::AddConstToRegProp | ROp::AddRegPropsToRegProp => return true,
            _ => {}
        }
        ip += op.size();
    }
    false
}

/// Emit helper call: load 64-bit address into x9 and blr.
fn emit_call_helper(ops: &mut dynasmrt::aarch64::Assembler, addr: u64) {
    dynasm!(ops
        ; movz x9, #((addr >> 48) as u32), LSL #48
        ; movk x9, #(((addr >> 32) & 0xFFFF) as u32), LSL #32
        ; movk x9, #(((addr >> 16) & 0xFFFF) as u32), LSL #16
        ; movk x9, #((addr & 0xFFFF) as u32)
        ; blr x9
    );
}

pub struct DjitFunction {
    buffer: ExecutableBuffer,
    has_calls: bool,
}

pub struct DynasmJit {
    compiled: HashMap<usize, DjitFunction>,
    call_counts: HashMap<usize, u32>,
    /// Function key that was just compiled mid-execution. Deferred until the
    /// next top-level run_register() to avoid using JIT code within the same
    /// recursive call chain that triggered compilation (which can corrupt state).
    deferred_key: Option<usize>,
}

impl Default for DynasmJit {
    fn default() -> Self {
        Self::new()
    }
}

impl DynasmJit {
    pub fn new() -> Self {
        Self {
            compiled: HashMap::new(),
            call_counts: HashMap::new(),
            deferred_key: None,
        }
    }

    /// Clear deferred key — called at the start of run_register() so
    /// JIT code compiled in a previous execution can be used.
    pub fn clear_deferred(&mut self) {
        self.deferred_key = None;
    }

    /// Mark a function as just-compiled so it won't be used mid-recursion.
    pub fn set_deferred(&mut self, func_key: usize) {
        self.deferred_key = Some(func_key);
    }

    pub fn record_call(&mut self, func_key: usize) -> bool {
        if self.compiled.contains_key(&func_key) { return false; }
        let count = self.call_counts.entry(func_key).or_insert(0);
        *count += 1;
        *count == DJIT_THRESHOLD
    }

    pub fn get_compiled(&self, func_key: usize) -> Option<&DjitFunction> {
        self.compiled.get(&func_key)
    }

    /// Get a raw function pointer for a compiled function.
    /// Returns None if the function was just compiled mid-recursion (deferred).
    pub fn get_fn_ptr(&self, func_key: usize) -> Option<*const u8> {
        if self.deferred_key == Some(func_key) { return None; }
        self.compiled.get(&func_key).map(|f| f.buffer.ptr(AssemblyOffset(0)))
    }

    pub fn has_calls(&self, func_key: usize) -> bool {
        self.compiled.get(&func_key).is_some_and(|f| f.has_calls)
    }

    /// Execute a compiled function (no calls variant).
    /// AArch64 AAPCS64: x0=regs, x1=consts, x2=globals
    ///
    /// # Safety
    /// `fn_ptr` must point to valid JIT-compiled code. All pointers must be valid.
    pub unsafe fn execute_ptr(
        fn_ptr: *const u8,
        regs: *mut u64,
        consts: *const u64,
        globals: *mut u64,
    ) -> u64 {
        let f: extern "C" fn(*mut u64, *const u64, *mut u64) -> u64 =
            mem::transmute(fn_ptr);
        f(regs, consts, globals)
    }

    /// Execute a compiled function with vm_ptr (calls variant).
    /// AArch64 AAPCS64: x0=regs, x1=consts, x2=globals, x3=vm_ptr
    ///
    /// # Safety
    /// `fn_ptr` must point to valid JIT-compiled code. All pointers must be valid.
    pub unsafe fn execute_ptr_with_vm(
        fn_ptr: *const u8,
        regs: *mut u64,
        consts: *const u64,
        globals: *mut u64,
        vm_ptr: *mut u8,
    ) -> u64 {
        let f: extern "C" fn(*mut u64, *const u64, *mut u64, *mut u8) -> u64 =
            mem::transmute(fn_ptr);
        f(regs, consts, globals, vm_ptr)
    }

    /// Compile a function to AArch64 machine code.
    pub fn try_compile(
        &mut self,
        func_key: usize,
        instructions: &[u8],
        constants: &[crate::object::Object],
        self_val: u64,
        reg_count: u16,
        layout: &crate::vm::JitLayout,
        globals_ptr: *const u64,
        globals_len: usize,
    ) -> bool {
        if !can_djit(instructions, constants) {
            return false;
        }
        if !verify_truthiness_safe(instructions) {
            return false;
        }
        if instructions.len() > 2000 {
            return false;
        }

        let num_calls = count_call_opcodes(instructions);
        if num_calls == 1 && instructions.len() < 50 {
            return false;
        }

        let has_calls = needs_vm_ptr(instructions);
        let reg_window = (reg_count as i32).max(1);

        // Phase 5: Self-call detection
        let (guaranteed_self, self_global_idx) =
            all_calls_are_self(instructions, self_val, globals_ptr, globals_len);

        // Phase 6: Register pinning (w24/w25/w26 for !has_calls compute functions)
        let pins = if !has_calls && instructions.len() >= 40 {
            find_pinned_registers_3(instructions, constants)
        } else {
            [None; 3]
        };
        let pin0 = pins[0].map(|p| p as i32); // VM reg → w24
        let pin1 = pins[1].map(|p| p as i32); // VM reg → w25
        let pin2 = pins[2].map(|p| p as i32); // VM reg → w26

        let mut ops = dynasmrt::aarch64::Assembler::new().unwrap();

        let mut labels: Vec<dynasmrt::DynamicLabel> = Vec::with_capacity(instructions.len() + 1);
        for _ in 0..=instructions.len() {
            labels.push(ops.new_dynamic_label());
        }

        let entry_label = ops.new_dynamic_label();
        let self_entry_label = ops.new_dynamic_label();

        // ── AArch64 AAPCS64 ABI ──
        // Entry: x0=regs, x1=consts, x2=globals, x3=vm_ptr
        // Callee-saved: x19-x28, x29(FP), x30(LR)
        //
        // has_calls layout:
        //   x19 = regs_ptr (persistent across helper calls)
        //   x20 = consts_ptr
        //   x21 = globals_ptr
        //   x22 = vm_ptr
        //   x23 = I32_SIG constant
        //   x9-x16 = scratch
        //
        // !has_calls layout:
        //   x0 = regs_ptr (stays in arg register)
        //   x1 = consts_ptr
        //   x2 = globals_ptr
        //   x23 = I32_SIG constant

        if has_calls {
            // External entry: save callee-saved registers, set up persistent pointers
            dynasm!(ops
                ; =>entry_label
                ; stp x19, x20, [sp, #-16]!
                ; stp x21, x22, [sp, #-16]!
                ; stp x23, x30, [sp, #-16]!
                ; mov x20, x1                   // consts_ptr
                ; mov x21, x2                   // globals_ptr
                ; mov x22, x3                   // vm_ptr
                ; movz x23, #0x7FF9, LSL #48    // I32_SIG
                ; mov x0, x0                    // regs_ptr stays in x0 for self_entry
                ; bl =>self_entry_label
                ; ldp x23, x30, [sp], #16
                ; ldp x21, x22, [sp], #16
                ; ldp x19, x20, [sp], #16
                ; ret
            );
            // Self entry: save x19 (regs), allocate frame
            dynasm!(ops
                ; =>self_entry_label
                ; stp x19, x30, [sp, #-16]!
                ; mov x19, x0                   // regs_ptr in x19
            );
        } else {
            // !has_calls: save x23 + LR, and pinned registers if used
            if pin0.is_some() || pin1.is_some() || pin2.is_some() {
                // Save x24-x26 (pinned) + x23 (I32_SIG) + x30 (LR).
                // Must keep sp 16-byte aligned → always push in pairs.
                dynasm!(ops
                    ; stp x24, x25, [sp, #-16]!
                    ; stp x26, x23, [sp, #-16]!
                    ; stp x30, xzr, [sp, #-16]!   // LR + padding (16-aligned)
                    ; movz x23, #0x7FF9, LSL #48
                );
                if let Some(p0) = pin0 { dynasm!(ops ; ldr w24, [x0, #(p0 * 8)]); }
                if let Some(p1) = pin1 { dynasm!(ops ; ldr w25, [x0, #(p1 * 8)]); }
                if let Some(p2) = pin2 { dynasm!(ops ; ldr w26, [x0, #(p2 * 8)]); }
            } else {
                dynasm!(ops
                    ; stp x23, x30, [sp, #-16]!
                    ; movz x23, #0x7FF9, LSL #48
                );
            }
        }

        // Register mapping: has_calls uses callee-saved x19-x21,
        // !has_calls uses argument registers x0-x2 directly.
        let rr: u32 = if has_calls { 19 } else { 0 }; // regs_ptr
        let cr: u32 = if has_calls { 20 } else { 1 }; // consts_ptr
        let gr: u32 = if has_calls { 21 } else { 2 }; // globals_ptr

        // ── Codegen loop ──
        let mut ip = 0;
        while ip < instructions.len() {
            let op = match ROp::from_byte(instructions[ip]) {
                Some(op) => op,
                None => return false,
            };
            // Emit label for this bytecode offset
            dynasm!(ops ; =>labels[ip]);

            match op {
                ROp::LoadConst => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let idx = read_u16(instructions, ip + 3) as u32;
                    // Load pre-converted constant from consts_ptr[idx]
                    dynasm!(ops
                        ; ldr x9, [X(cr), #(idx * 8)]
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::LoadTrue => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    dynasm!(ops
                        ; movz x9, #0x7FFA, LSL #48
                        ; orr x9, x9, #1
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::LoadFalse => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    dynasm!(ops
                        ; movz x9, #0x7FFA, LSL #48
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::LoadNull => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    dynasm!(ops
                        ; movz x9, #0x7FFB, LSL #48
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::LoadUndef => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    dynasm!(ops
                        ; movz x9, #0x7FFC, LSL #48
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::Move => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let src = read_u16(instructions, ip + 3) as u32;
                    dynasm!(ops
                        ; ldr x9, [X(rr), #(src * 8)]
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::GetGlobal => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let idx = read_u16(instructions, ip + 3);
                    // Skip loading self-reference when guaranteed_self
                    if self_global_idx != Some(idx) {
                        let idx = idx as u32;
                        dynasm!(ops
                            ; ldr x9, [X(gr), #(idx * 8)]
                            ; str x9, [X(rr), #(dst * 8)]
                        );
                    }
                }
                ROp::SetGlobal => {
                    let idx = read_u16(instructions, ip + 1) as u32;
                    let src = read_u16(instructions, ip + 3) as u32;
                    dynasm!(ops
                        ; ldr x9, [X(rr), #(src * 8)]
                        ; str x9, [X(gr), #(idx * 8)]
                    );
                }

                // ── Arithmetic (i32 fast path only) ──
                ROp::Add => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    // i32 fast path: adds w9, w10 → check overflow → re-box
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; adds w9, w9, w10
                        // On overflow (V flag), fall to slow path
                        // For now: no overflow handling (Phase 2 limitation)
                        ; orr x9, x9, x23    // re-box with I32_SIG
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::Sub => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; subs w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::Mul => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; mul w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::Div => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; sdiv w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::Mod => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    // AArch64: sdiv + msub for modulo
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; sdiv w11, w9, w10       // quotient
                        ; msub w9, w11, w10, w9   // remainder = a - q*b
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }

                // ── Comparisons ──
                ROp::LessThan | ROp::LessOrEqual | ROp::GreaterThan | ROp::GreaterOrEqual => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    // Using csel for branchless comparison
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; cmp w9, w10
                        ; movz x11, #0x7FFA, LSL #48       // VAL_FALSE
                        ; movz x12, #0x7FFA, LSL #48
                        ; orr x12, x12, #1                  // VAL_TRUE
                    );
                    match op {
                        ROp::LessThan =>      dynasm!(ops ; csel x9, x12, x11, lt),
                        ROp::LessOrEqual =>   dynasm!(ops ; csel x9, x12, x11, le),
                        ROp::GreaterThan =>   dynasm!(ops ; csel x9, x12, x11, gt),
                        ROp::GreaterOrEqual => dynasm!(ops ; csel x9, x12, x11, ge),
                        _ => unreachable!(),
                    }
                    dynasm!(ops ; str x9, [X(rr), #(dst * 8)]);
                }

                ROp::StrictEqual | ROp::Equal => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr x9, [X(rr), #(left * 8)]
                        ; ldr x10, [X(rr), #(right * 8)]
                        ; cmp x9, x10       // 64-bit compare for strict equality
                        ; movz x11, #0x7FFA, LSL #48
                        ; movz x12, #0x7FFA, LSL #48
                        ; orr x12, x12, #1
                        ; csel x9, x12, x11, eq
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::StrictNotEqual | ROp::NotEqual => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr x9, [X(rr), #(left * 8)]
                        ; ldr x10, [X(rr), #(right * 8)]
                        ; cmp x9, x10
                        ; movz x11, #0x7FFA, LSL #48
                        ; movz x12, #0x7FFA, LSL #48
                        ; orr x12, x12, #1
                        ; csel x9, x12, x11, ne
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }

                // ── Unary ──
                ROp::Neg => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let src = read_u16(instructions, ip + 3) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(src * 8)]
                        ; neg w9, w9
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::Not => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let src = read_u16(instructions, ip + 3) as u32;
                    // Check if value is VAL_TRUE → return FALSE, else TRUE
                    dynasm!(ops
                        ; ldr x9, [X(rr), #(src * 8)]
                        ; movz x10, #0x7FFA, LSL #48
                        ; orr x10, x10, #1            // VAL_TRUE
                        ; cmp x9, x10
                        ; movz x11, #0x7FFA, LSL #48  // FALSE
                        ; movz x12, #0x7FFA, LSL #48
                        ; orr x12, x12, #1             // TRUE
                        ; csel x9, x11, x12, eq        // was true → false, else → true
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::IsNullish => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let src = read_u16(instructions, ip + 3) as u32;
                    dynasm!(ops
                        ; ldr x9, [X(rr), #(src * 8)]
                        ; movz x10, #0x7FFB, LSL #48   // VAL_NULL
                        ; cmp x9, x10
                        ; b.eq >is_nullish
                        ; movz x10, #0x7FFC, LSL #48   // VAL_UNDEFINED
                        ; cmp x9, x10
                        ; b.eq >is_nullish
                        ; movz x9, #0x7FFA, LSL #48    // FALSE
                        ; b >nullish_done
                        ; is_nullish:
                        ; movz x9, #0x7FFA, LSL #48
                        ; orr x9, x9, #1               // TRUE
                        ; nullish_done:
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }

                // ── Control flow ──
                ROp::Jump => {
                    let target = read_u32(instructions, ip + 1) as usize;
                    dynasm!(ops ; b =>labels[target]);
                }
                ROp::JumpIfNot => {
                    let cond = read_u16(instructions, ip + 1) as u32;
                    let target = read_u32(instructions, ip + 3) as usize;
                    // Check if value is falsy (FALSE, NULL, UNDEFINED, 0)
                    // Simple: check if == VAL_TRUE
                    dynasm!(ops
                        ; ldr x9, [X(rr), #(cond * 8)]
                        ; movz x10, #0x7FFA, LSL #48
                        ; orr x10, x10, #1            // VAL_TRUE
                        ; cmp x9, x10
                        ; b.ne =>labels[target]        // not true → jump
                    );
                }
                ROp::JumpIfTruthy => {
                    let cond = read_u16(instructions, ip + 1) as u32;
                    let target = read_u32(instructions, ip + 3) as usize;
                    dynasm!(ops
                        ; ldr x9, [X(rr), #(cond * 8)]
                        ; movz x10, #0x7FFA, LSL #48
                        ; orr x10, x10, #1            // VAL_TRUE
                        ; cmp x9, x10
                        ; b.eq =>labels[target]
                    );
                }

                // ── Return ──
                ROp::Return | ROp::HaltValue => {
                    let src = read_u16(instructions, ip + 1) as u32;
                    if has_calls {
                        dynasm!(ops
                            ; ldr x0, [X(rr), #(src * 8)]
                            ; ldp x19, x30, [sp], #16
                            ; ret
                        );
                    } else if pin0.is_some() || pin1.is_some() || pin2.is_some() {
                        // Write back pinned registers BEFORE overwriting x0
                        if let Some(p0) = pin0 { dynasm!(ops ; orr x9, x24, x23 ; str x9, [x0, #(p0 * 8)]); }
                        if let Some(p1) = pin1 { dynasm!(ops ; orr x9, x25, x23 ; str x9, [x0, #(p1 * 8)]); }
                        if let Some(p2) = pin2 { dynasm!(ops ; orr x9, x26, x23 ; str x9, [x0, #(p2 * 8)]); }
                        // Now load return value (may come from pinned reg or memory)
                        if pin0 == Some(src as i32) {
                            dynasm!(ops ; mov w9, w24 ; orr x0, x9, x23);
                        } else if pin1 == Some(src as i32) {
                            dynasm!(ops ; mov w9, w25 ; orr x0, x9, x23);
                        } else if pin2 == Some(src as i32) {
                            dynasm!(ops ; mov w9, w26 ; orr x0, x9, x23);
                        } else {
                            dynasm!(ops ; ldr x0, [x0, #(src * 8)]); // x0 still valid as regs_ptr
                        }
                        // Restore callee-saved + ret (matching stp pairs in prologue)
                        dynasm!(ops
                            ; ldp x30, xzr, [sp], #16
                            ; ldp x26, x23, [sp], #16
                            ; ldp x24, x25, [sp], #16
                            ; ret
                        );
                    } else {
                        dynasm!(ops
                            ; ldr x0, [X(rr), #(src * 8)]
                            ; ldp x23, x30, [sp], #16
                            ; ret
                        );
                    }
                }
                ROp::ReturnUndef | ROp::Halt => {
                    if pin0.is_some() || pin1.is_some() || pin2.is_some() {
                        // Write back pinned registers before return
                        if let Some(p0) = pin0 { dynasm!(ops ; orr x9, x24, x23 ; str x9, [x0, #(p0 * 8)]); }
                        if let Some(p1) = pin1 { dynasm!(ops ; orr x9, x25, x23 ; str x9, [x0, #(p1 * 8)]); }
                        if let Some(p2) = pin2 { dynasm!(ops ; orr x9, x26, x23 ; str x9, [x0, #(p2 * 8)]); }
                        dynasm!(ops
                            ; movz x0, #0x7FFC, LSL #48
                            ; ldp x30, xzr, [sp], #16
                            ; ldp x26, x23, [sp], #16
                            ; ldp x24, x25, [sp], #16
                            ; ret
                        );
                    } else {
                        dynasm!(ops ; movz x0, #0x7FFC, LSL #48);
                        if has_calls {
                            dynasm!(ops ; ldp x19, x30, [sp], #16 ; ret);
                        } else {
                            dynasm!(ops ; ldp x23, x30, [sp], #16 ; ret);
                        }
                    }
                }

                // ── Bitwise ──
                ROp::BitwiseAnd => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; and w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::BitwiseOr => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; orr w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::BitwiseXor => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; eor w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::LeftShift => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; and w10, w10, #31
                        ; lsl w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::RightShift => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; and w10, w10, #31
                        ; asr w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }
                ROp::UnsignedRightShift => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let left = read_u16(instructions, ip + 3) as u32;
                    let right = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(left * 8)]
                        ; ldr w10, [X(rr), #(right * 8)]
                        ; and w10, w10, #31
                        ; lsr w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }

                // ── Fused ops ──
                ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let src = read_u16(instructions, ip + 3) as u32;
                    let cidx = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(src * 8)]
                        ; ldr w10, [X(cr), #(cidx * 8)]
                    );
                    match op {
                        ROp::AddRegConst => dynasm!(ops ; add w9, w9, w10),
                        ROp::SubRegConst => dynasm!(ops ; sub w9, w9, w10),
                        ROp::MulRegConst => dynasm!(ops ; mul w9, w9, w10),
                        _ => unreachable!(),
                    }
                    dynasm!(ops
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(dst * 8)]
                    );
                }

                ROp::TestLtConstJump | ROp::TestLeConstJump => {
                    let r = read_u16(instructions, ip + 1) as u32;
                    let cidx = read_u16(instructions, ip + 3) as u32;
                    let target = read_u32(instructions, ip + 5) as usize;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(r * 8)]
                        ; ldr w10, [X(cr), #(cidx * 8)]
                        ; cmp w9, w10
                    );
                    if op == ROp::TestLeConstJump {
                        // passes if r <= const → fail (jump) if r > const
                        dynasm!(ops ; b.gt =>labels[target]);
                    } else {
                        // passes if r < const → fail (jump) if r >= const
                        dynasm!(ops ; b.ge =>labels[target]);
                    }
                }
                ROp::TestLtRegJump | ROp::TestLeRegJump => {
                    let a = read_u16(instructions, ip + 1) as u32;
                    let b = read_u16(instructions, ip + 3) as u32;
                    let target = read_u32(instructions, ip + 5) as usize;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(a * 8)]
                        ; ldr w10, [X(rr), #(b * 8)]
                        ; cmp w9, w10
                    );
                    if op == ROp::TestLeRegJump {
                        dynasm!(ops ; b.gt =>labels[target]);
                    } else {
                        dynasm!(ops ; b.ge =>labels[target]);
                    }
                }

                ROp::IncrementRegAndJump => {
                    let r = read_u16(instructions, ip + 1) as u32;
                    let cidx = read_u16(instructions, ip + 3) as u32;
                    let target = read_u32(instructions, ip + 5) as usize;
                    dynasm!(ops
                        ; ldr w9, [X(rr), #(r * 8)]
                        ; ldr w10, [X(cr), #(cidx * 8)]
                        ; add w9, w9, w10
                        ; orr x9, x9, x23
                        ; str x9, [X(rr), #(r * 8)]
                        ; b =>labels[target]
                    );
                }

                ROp::ModRegConstStrictEqConstJump | ROp::TestModRegStrictEqConstJump => {
                    let r = read_u16(instructions, ip + 1) as u32;
                    let mod_c = read_u16(instructions, ip + 3) as u32;
                    let cmp_c = read_u16(instructions, ip + 5) as u32;
                    let target = read_u32(instructions, ip + 7) as usize;
                    let mod_src = if op == ROp::ModRegConstStrictEqConstJump {
                        // r is VM reg, mod_c and cmp_c are const indices
                        dynasm!(ops ; ldr w9, [X(rr), #(r * 8)]);
                        dynasm!(ops ; ldr w10, [X(cr), #(mod_c * 8)]);
                        0 // unused
                    } else {
                        // a=reg, b=reg, cmp_c=const
                        dynasm!(ops ; ldr w9, [X(rr), #(r * 8)]);
                        dynasm!(ops ; ldr w10, [X(rr), #(mod_c * 8)]);
                        0
                    };
                    let _ = mod_src;
                    // Compute modulo: sdiv + msub
                    dynasm!(ops
                        ; sdiv w11, w9, w10
                        ; msub w9, w11, w10, w9    // remainder
                    );
                    if op == ROp::ModRegConstStrictEqConstJump {
                        dynasm!(ops ; ldr w10, [X(cr), #(cmp_c * 8)]);
                    } else {
                        dynasm!(ops ; ldr w10, [X(cr), #(cmp_c * 8)]);
                    }
                    dynasm!(ops
                        ; cmp w9, w10
                        ; b.ne =>labels[target]    // not equal → jump (condition fails)
                    );
                    // If equal: fall through (condition passes)
                }

                // ── Call/CallGlobal via helper (has_calls only) ──
                ROp::Call => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let base = read_u16(instructions, ip + 3) as u32;
                    let nargs = read_u8(instructions, ip + 5) as u32;

                    if guaranteed_self {
                        // Direct self-call
                        dynasm!(ops
                            ; add x0, x19, #(reg_window * 8)
                        );
                        for i in 0..nargs {
                            dynasm!(ops
                                ; ldr x9, [x19, #((base + 1 + i) * 8)]
                                ; str x9, [x0, #((1 + i) * 8)]
                            );
                        }
                        dynasm!(ops
                            ; bl =>self_entry_label
                            ; str x0, [x19, #(dst * 8)]
                        );
                    } else {
                        let helper_addr = crate::vm::djit_call_helper as usize as u64;
                        dynasm!(ops
                            ; mov x0, x22
                            ; ldr x1, [x19, #(base * 8)]
                            ; add x2, x19, #((base + 1) * 8)
                            ; mov w3, nargs
                        );
                        Self::emit_call_helper(&mut ops, helper_addr);
                        dynasm!(ops ; str x0, [x19, #(dst * 8)]);
                    }
                }
                ROp::CallGlobal => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let global_idx = read_u16(instructions, ip + 3) as u32;
                    let base = read_u16(instructions, ip + 5) as u32;
                    let nargs = read_u8(instructions, ip + 7) as u32;

                    if guaranteed_self {
                        // Direct self-call: set up new register window + bl
                        dynasm!(ops
                            ; add x0, x19, #(reg_window * 8)  // new regs base
                        );
                        for i in 0..nargs {
                            dynasm!(ops
                                ; ldr x9, [x19, #((base + 1 + i) * 8)]
                                ; str x9, [x0, #((1 + i) * 8)]
                            );
                        }
                        dynasm!(ops
                            ; bl =>self_entry_label
                            ; str x0, [x19, #(dst * 8)]
                        );
                    } else {
                        let helper_addr = crate::vm::djit_call_helper as usize as u64;
                        dynasm!(ops
                            ; mov x0, x22
                            ; ldr x1, [x21, #(global_idx * 8)]
                            ; add x2, x19, #((base + 1) * 8)
                            ; mov w3, nargs
                        );
                        Self::emit_call_helper(&mut ops, helper_addr);
                        dynasm!(ops ; str x0, [x19, #(dst * 8)]);
                    }
                }
                ROp::CallMethod => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let base = read_u16(instructions, ip + 3) as u32;
                    let nargs_raw = read_u8(instructions, ip + 5);
                    let prop_c = read_u16(instructions, ip + 6) as u64;
                    let _cache = read_u16(instructions, ip + 8);
                    let packed: u64 = (nargs_raw as u64) | (prop_c << 8);
                    dynasm!(ops
                        ; mov x0, x22
                        ; ldr x1, [x19, #(base * 8)]
                        ; add x2, x19, #((base + 1) * 8)
                        ; movz x3, #((packed & 0xFFFF) as u32)
                        ; movk x3, #(((packed >> 16) & 0xFFFF) as u32), LSL #16
                    );
                    Self::emit_call_helper(&mut ops, crate::vm::djit_call_method_helper as usize as u64);
                    dynasm!(ops ; str x0, [x19, #(dst * 8)]);
                }
                ROp::New => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let base = read_u16(instructions, ip + 3) as u32;
                    let nargs = read_u8(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; mov x0, x22
                        ; ldr x1, [x19, #(base * 8)]
                        ; add x2, x19, #((base + 1) * 8)
                        ; mov w3, nargs
                    );
                    Self::emit_call_helper(&mut ops, crate::vm::djit_new_helper as usize as u64);
                    dynasm!(ops ; str x0, [x19, #(dst * 8)]);
                }
                ROp::Array => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let base = read_u16(instructions, ip + 3) as u32;
                    let count = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; mov x0, x22
                        ; add x1, x19, #(base * 8)
                        ; mov w2, count
                    );
                    Self::emit_call_helper(&mut ops, crate::vm::djit_array_helper as usize as u64);
                    dynasm!(ops ; str x0, [x19, #(dst * 8)]);
                }
                ROp::Index => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let obj = read_u16(instructions, ip + 3) as u32;
                    let key = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; mov x0, x22
                        ; ldr x1, [x19, #(obj * 8)]
                        ; ldr x2, [x19, #(key * 8)]
                        ; mov x3, xzr
                    );
                    Self::emit_call_helper(&mut ops, crate::vm::djit_get_index_helper as usize as u64);
                    dynasm!(ops ; str x0, [x19, #(dst * 8)]);
                }
                ROp::SetIndex => {
                    let obj_r = read_u16(instructions, ip + 1) as u32;
                    let key_r = read_u16(instructions, ip + 3) as u32;
                    let val_r = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; mov x0, x22
                        ; ldr x1, [x19, #(obj_r * 8)]
                        ; ldr x2, [x19, #(key_r * 8)]
                        ; ldr x3, [x19, #(val_r * 8)]
                    );
                    Self::emit_call_helper(&mut ops, crate::vm::djit_set_index_helper as usize as u64);
                    dynasm!(ops ; str x0, [x19, #(obj_r * 8)]);
                }
                ROp::Hash => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let base = read_u16(instructions, ip + 3) as u32;
                    let count = read_u16(instructions, ip + 5) as u32;
                    dynasm!(ops
                        ; mov x0, x22
                        ; add x1, x19, #(base * 8)
                        ; mov w2, count
                    );
                    Self::emit_call_helper(&mut ops, crate::vm::djit_hash_helper as usize as u64);
                    dynasm!(ops ; str x0, [x19, #(dst * 8)]);
                }
                ROp::GetProp => {
                    let dst = read_u16(instructions, ip + 1) as u32;
                    let obj_r = read_u16(instructions, ip + 3) as u32;
                    let prop_c = read_u16(instructions, ip + 5) as u32;
                    let cache = read_u16(instructions, ip + 7) as u32;
                    dynasm!(ops
                        ; mov x0, x22
                        ; ldr x1, [x19, #(obj_r * 8)]
                        ; mov w2, prop_c
                        ; mov w3, cache
                    );
                    Self::emit_call_helper(&mut ops, crate::vm::djit_get_prop_helper as usize as u64);
                    dynasm!(ops ; str x0, [x19, #(dst * 8)]);
                }
                ROp::SetProp => {
                    let obj_r = read_u16(instructions, ip + 1) as u32;
                    let prop_c = read_u16(instructions, ip + 3) as u32;
                    let src_r = read_u16(instructions, ip + 5) as u32;
                    let cache = read_u16(instructions, ip + 7) as u32;
                    dynasm!(ops
                        ; mov x0, x22
                        ; ldr x1, [x19, #(obj_r * 8)]
                        ; mov w2, prop_c
                        ; ldr x3, [x19, #(src_r * 8)]
                        ; mov w4, cache
                    );
                    Self::emit_call_helper(&mut ops, crate::vm::djit_set_prop_helper as usize as u64);
                }


                _ => {
                    // Unsupported opcode — should not reach here due to can_djit check
                    return false;
                }
            }

            ip += op.size();
        }

        // Emit trailing label
        dynasm!(ops ; =>labels[instructions.len()]);

        match ops.finalize() {
            Ok(buf) => {
                self.compiled.insert(func_key, DjitFunction { buffer: buf, has_calls });
                true
            }
            Err(_) => false,
        }
    }
}
