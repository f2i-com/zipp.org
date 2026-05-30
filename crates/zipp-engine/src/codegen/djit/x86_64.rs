use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi, AssemblyOffset, ExecutableBuffer};
use std::collections::HashMap;
use std::mem;

use crate::rcode::ROp;
use crate::vm::STACK_SIZE;

// NaN-boxing constants
const I32_SIG: u64 = 0x7FF9_0000_0000_0000;
const VAL_TRUE: u64 = 0x7FFA_0000_0000_0001;
const VAL_FALSE: u64 = 0x7FFA_0000_0000_0000;
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

/// Get an integer constant value at compile time for immediate embedding.
fn const_i32(constants: &[crate::object::Object], idx: usize) -> Option<i32> {
    if idx >= constants.len() { return None; }
    match &constants[idx] {
        crate::object::Object::Integer(v) if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 => {
            Some(*v as i32)
        }
        _ => None,
    }
}

/// Pre-intern a string constant at compile time, returning the symbol ID.
fn intern_const(constants: &[crate::object::Object], idx: usize) -> Option<u32> {
    if idx >= constants.len() { return None; }
    match &constants[idx] {
        crate::object::Object::String(s) => Some(crate::intern::intern(s)),
        _ => None,
    }
}

/// Check if a function can be compiled by the dynasm JIT.
///
/// `Div` / `Mod` are deliberately excluded: the x86_64 codegen emits
/// a raw `idiv r9d` with no divisor-zero or `(INT_MIN,-1)` guard, so
/// `a / 0` would raise a hardware `#DE` and terminate the host
/// process. The interpreter handles these cases correctly (falls
/// back to f64 semantics) so the JIT-inadmissible path gracefully
/// picks up the fix. Re-enabling requires emitting a runtime
/// `test r9d, r9d; jz &gt;slow` check plus an `(INT_MIN, -1)` guard
/// and threading into the slow helper.
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
            | ROp::AddConstToRegProp | ROp::AddRegPropsToRegProp
            => {}
            // Div / Mod intentionally rejected — see function doc comment.
            ROp::Div | ROp::Mod => return false,
            _ => return false,
        }
        // Verify LoadConst loads a supported constant type
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

/// JS truthiness is "any non-empty, non-zero, non-null value is true",
/// but the JIT's `JumpIfNot` / `JumpIfTruthy` / `Not` emit a single
/// `cmp rax, VAL_TRUE` — i.e. they assume the operand is a boolean.
/// That's safe when the operand was produced by a comparison (`<`,
/// `===`, …), `Not`, `IsNullish`, `LoadTrue` / `LoadFalse`, or `Move`d
/// from another known-bool register. For any other producer (numbers
/// from `Add`, strings from `LoadConst`, heap objects from `GetGlobal`,
/// …) the comparison gives the wrong answer — `if (1)` would route
/// through the falsy branch.
///
/// Rather than patch every conditional emit site to call a generic
/// truthiness helper (which would also need a `vm_ptr` we don't have
/// in `!has_calls` functions), this function refuses to JIT any
/// function where a conditional would consume a non-boolean register.
/// The interpreter handles those correctly. Bench / React hot paths
/// always test the result of a comparison, so the JIT still kicks in
/// for them.
///
/// The flow analysis is intra-procedural and conservative:
///   * On entry to any branch target (forward or backward), every
///     register's bool status is reset to "unknown / non-bool".
///   * Within a basic block, a write of a comparison/Not/IsNullish/
///     LoadTrue/LoadFalse marks the destination as bool, a `Move`
///     copies the source's bool flag, every other write clears it.
fn verify_truthiness_safe(instructions: &[u8]) -> bool {
    // Pass 1: collect every IP that's a jump target.
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

    // Pass 2: forward bool-flow check.
    // Cap at 256 regs — matches `all_calls_are_self`'s reg_global table
    // and is well above the compiler's typical register pressure for
    // anything we'd JIT-compile.
    let mut bool_regs: [bool; 256] = [false; 256];
    let mut ip = 0;
    while ip < instructions.len() {
        if is_target[ip] {
            // Conservative reset: a basic block reachable from elsewhere
            // can't assume any prior bool-tagging holds.
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
            // For ops with a destination register that doesn't produce a bool,
            // clear the dst's bool flag. The instruction layout for write-to-reg
            // ops is `dst:u16` at offset+1; ops without a dst write are no-ops.
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
            // No destination, or destination already known to be non-bool.
            _ => {}
        }
        ip += op.size();
    }
    true
}

/// Count only Call/CallGlobal (user function invocations, not helpers).
fn count_user_calls(instructions: &[u8]) -> usize {
    let mut ip = 0;
    let mut count = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => return count,
        };
        if matches!(op, ROp::Call | ROp::CallGlobal) { count += 1; }
        ip += op.size();
    }
    count
}

fn count_call_opcodes(instructions: &[u8]) -> usize {
    let mut ip = 0;
    let mut count = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => return count,
        };
        if matches!(op, ROp::Call | ROp::CallGlobal | ROp::CallMethod | ROp::New) {
            count += 1;
        }
        ip += op.size();
    }
    count
}

/// Check if all Call/CallGlobal ops are guaranteed self-calls.
/// `self_val` is the NaN-boxed value of the function being compiled.
/// `globals_ptr`/`globals_len` allow looking up which global slot holds self_val.
/// Returns (true, Some(global_idx)) if all calls target the same global that holds self_val.
fn all_calls_are_self(instructions: &[u8], self_val: u64, globals_ptr: *const u64, globals_len: usize) -> (bool, Option<u16>) {
    // First, find which global index holds self_val
    let self_global_idx: Option<u16> = if !globals_ptr.is_null() {
        let mut found = None;
        for i in 0..globals_len {
            if unsafe { *globals_ptr.add(i) } == self_val {
                found = Some(i as u16);
                break;
            }
        }
        found
    } else {
        None
    };

    if self_global_idx.is_none() {
        return (false, None);
    }
    let expected_idx = self_global_idx.unwrap();

    let mut reg_global: [Option<u16>; 256] = [None; 256];
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => return (false, None),
        };
        match op {
            ROp::GetGlobal => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let gidx = read_u16(instructions, ip + 3);
                if dst < 256 { reg_global[dst] = Some(gidx); }
            }
            ROp::Call => {
                let base = read_u16(instructions, ip + 3) as usize;
                let gidx = if base < 256 { reg_global[base] } else { None };
                match gidx {
                    Some(g) if g == expected_idx => {}
                    _ => return (false, None),
                }
            }
            ROp::CallGlobal => {
                let gidx = read_u16(instructions, ip + 3);
                if gidx != expected_idx {
                    return (false, None);
                }
            }
            // CallMethod and New are always non-self calls
            ROp::CallMethod | ROp::New => {
                return (false, None);
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

/// Identify registers that may ever hold non-i32 values (strings, booleans, etc).
/// These cannot be safely pinned to 32-bit CPU registers.
fn find_non_i32_registers(instructions: &[u8], constants: &[crate::object::Object]) -> [bool; 64] {
    let mut tainted = [false; 64];
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) { Some(op) => op, None => break };
        if op == ROp::LoadConst {
            let dst = read_u16(instructions, ip + 1) as usize;
            let cidx = read_u16(instructions, ip + 3) as usize;
            if dst < 64 && const_i32(constants, cidx).is_none() {
                tainted[dst] = true;
            }
        }
        // GetGlobal loads arbitrary values (strings, objects, etc.)
        if op == ROp::GetGlobal {
            let dst = read_u16(instructions, ip + 1) as usize;
            if dst < 64 { tainted[dst] = true; }
        }
        if matches!(op, ROp::LoadTrue | ROp::LoadFalse | ROp::LoadNull | ROp::LoadUndef) {
            let dst = read_u16(instructions, ip + 1) as usize;
            if dst < 64 { tainted[dst] = true; }
        }
        // Comparison ops store VAL_TRUE/VAL_FALSE (non-i32)
        if matches!(op, ROp::GreaterOrEqual | ROp::GreaterThan | ROp::LessThan | ROp::LessOrEqual
            | ROp::StrictEqual | ROp::StrictNotEqual | ROp::Equal | ROp::NotEqual) {
            let dst = read_u16(instructions, ip + 1) as usize;
            if dst < 64 { tainted[dst] = true; }
        }
        // Add result could be non-i32 if either input is tainted (string concat)
        if op == ROp::Add {
            let dst = read_u16(instructions, ip + 1) as usize;
            let left = read_u16(instructions, ip + 3) as usize;
            let right = read_u16(instructions, ip + 5) as usize;
            if dst < 64 && ((left < 64 && tainted[left]) || (right < 64 && tainted[right])) {
                tainted[dst] = true;
            }
        }
        ip += op.size();
    }
    // Propagate through Move chains
    let mut changed = true;
    while changed {
        changed = false;
        ip = 0;
        while ip < instructions.len() {
            let op = match ROp::from_byte(instructions[ip]) { Some(op) => op, None => break };
            if op == ROp::Move {
                let dst = read_u16(instructions, ip + 1) as usize;
                let src = read_u16(instructions, ip + 3) as usize;
                if src < 64 && dst < 64 && tainted[src] && !tainted[dst] {
                    tainted[dst] = true;
                    changed = true;
                }
            }
            ip += op.size();
        }
    }
    tainted
}

/// Find the 2 most frequently accessed VM registers for pinning to CPU registers.
/// Returns (pin0, pin1) or None if pinning isn't worthwhile.
fn find_pinned_registers(instructions: &[u8], constants: &[crate::object::Object]) -> Option<(u16, u16)> {
    // Exclude registers that may ever hold non-i32 values
    let non_i32_loaded = find_non_i32_registers(instructions, constants);
    let mut freq: [u32; 64] = [0; 64];
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => break,
        };
        // Count register reads/writes from common opcodes
        match op {
            ROp::Add | ROp::Sub | ROp::Mul | ROp::Mod
            | ROp::LessThan | ROp::LessOrEqual | ROp::GreaterThan | ROp::GreaterOrEqual
            | ROp::StrictEqual | ROp::StrictNotEqual | ROp::Equal | ROp::NotEqual => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let left = read_u16(instructions, ip + 3) as usize;
                let right = read_u16(instructions, ip + 5) as usize;
                if dst < 64 { freq[dst] += 1; }
                if left < 64 { freq[left] += 2; } // reads count more
                if right < 64 { freq[right] += 2; }
            }
            ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let src = read_u16(instructions, ip + 3) as usize;
                if dst < 64 { freq[dst] += 1; }
                if src < 64 { freq[src] += 2; }
            }
            ROp::TestLtConstJump | ROp::TestLeConstJump => {
                let r = read_u16(instructions, ip + 1) as usize;
                if r < 64 { freq[r] += 3; } // loop condition, extra weight
            }
            ROp::IncrementRegAndJump => {
                let r = read_u16(instructions, ip + 1) as usize;
                if r < 64 { freq[r] += 3; }
            }
            ROp::ModRegConstStrictEqConstJump => {
                let r = read_u16(instructions, ip + 1) as usize;
                if r < 64 { freq[r] += 3; }
            }
            ROp::Move => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let src = read_u16(instructions, ip + 3) as usize;
                if dst < 64 { freq[dst] += 1; }
                if src < 64 { freq[src] += 1; }
            }
            _ => {}
        }
        ip += op.size();
    }
    // Zero out frequency for registers that hold non-i32 values
    for i in 0..64 {
        if non_i32_loaded[i] { freq[i] = 0; }
    }
    // Find top 2
    let mut best = [(0u16, 0u32); 2];
    for (i, &f) in freq.iter().enumerate() {
        if f > best[0].1 {
            best[1] = best[0];
            best[0] = (i as u16, f);
        } else if f > best[1].1 {
            best[1] = (i as u16, f);
        }
    }
    if best[0].1 >= 4 && best[1].1 >= 4 {
        Some((best[0].0, best[1].0))
    } else {
        None
    }
}

/// Find up to 3 most frequently accessed VM registers for pinning.
/// Excludes registers that are ever loaded with non-i32 constants (strings, etc.)
/// since pinned registers are 32-bit and can't hold full NaN-boxed values.
fn find_pinned_registers_3(instructions: &[u8], constants: &[crate::object::Object]) -> [Option<u16>; 3] {
    let non_i32_loaded = find_non_i32_registers(instructions, constants);
    let mut freq: [u32; 64] = [0; 64];
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => break,
        };
        match op {
            ROp::Add | ROp::Sub | ROp::Mul | ROp::Mod
            | ROp::LessThan | ROp::LessOrEqual | ROp::GreaterThan | ROp::GreaterOrEqual
            | ROp::StrictEqual | ROp::StrictNotEqual | ROp::Equal | ROp::NotEqual => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let left = read_u16(instructions, ip + 3) as usize;
                let right = read_u16(instructions, ip + 5) as usize;
                if dst < 64 { freq[dst] += 1; }
                if left < 64 { freq[left] += 2; }
                if right < 64 { freq[right] += 2; }
            }
            ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let src = read_u16(instructions, ip + 3) as usize;
                if dst < 64 { freq[dst] += 1; }
                if src < 64 { freq[src] += 2; }
            }
            ROp::TestLtConstJump | ROp::TestLeConstJump
            | ROp::IncrementRegAndJump | ROp::ModRegConstStrictEqConstJump => {
                let r = read_u16(instructions, ip + 1) as usize;
                if r < 64 { freq[r] += 3; }
            }
            ROp::Move | ROp::Neg | ROp::Not => {
                let dst = read_u16(instructions, ip + 1) as usize;
                let src = read_u16(instructions, ip + 3) as usize;
                if dst < 64 { freq[dst] += 1; }
                if src < 64 { freq[src] += 1; }
            }
            ROp::Return | ROp::HaltValue => {
                let src = read_u16(instructions, ip + 1) as usize;
                if src < 64 { freq[src] += 1; }
            }
            _ => {}
        }
        ip += op.size();
    }
    // Zero out frequency for registers that hold non-i32 values
    for i in 0..64 {
        if non_i32_loaded[i] { freq[i] = 0; }
    }
    let mut result = [None; 3];
    let mut best = [(0u16, 0u32); 3];
    for (i, &f) in freq.iter().enumerate() {
        if f > best[0].1 { best[2] = best[1]; best[1] = best[0]; best[0] = (i as u16, f); }
        else if f > best[1].1 { best[2] = best[1]; best[1] = (i as u16, f); }
        else if f > best[2].1 { best[2] = (i as u16, f); }
    }
    for i in 0..3 {
        if best[i].1 >= 3 { result[i] = Some(best[i].0); }
    }
    result
}

/// Resolve property name to slot index from Hash opcode in bytecode.
/// Scans for Hash opcodes and maps property constants to their insertion order.
fn resolve_prop_slot(instructions: &[u8], constants: &[crate::object::Object], prop_sym: u32) -> Option<usize> {
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => break,
        };
        if op == ROp::Hash {
            let base = read_u16(instructions, ip + 3) as usize;
            let count = read_u16(instructions, ip + 5) as usize;
            let num_pairs = count / 2;
            // Keys are loaded into regs[base], regs[base+2], etc. via preceding LoadConst
            // Scan backwards to find which constants were loaded into those registers
            for (slot, pair) in (0..num_pairs).enumerate() {
                let key_reg = base + pair * 2;
                // Find the LoadConst that wrote to key_reg
                let mut scan = 0;
                while scan < ip {
                    if let Some(ROp::LoadConst) = ROp::from_byte(instructions[scan]) {
                        let dst = read_u16(instructions, scan + 1) as usize;
                        let cidx = read_u16(instructions, scan + 3) as usize;
                        if dst == key_reg {
                            if let Some(s) = intern_const(constants, cidx) {
                                if s == prop_sym {
                                    return Some(slot);
                                }
                            }
                        }
                    }
                    scan += ROp::from_byte(instructions[scan]).map_or(1, |o| o.size());
                }
            }
        }
        ip += op.size();
    }
    None
}

/// Check if a register holds a known string constant by scanning backwards.
/// Returns (string_length, Option<nan_boxed_bits>) for a known string constant.
fn known_str_const_info(instructions: &[u8], constants: &[crate::object::Object], reg: u16, before_ip: usize) -> Option<(usize, Option<u64>)> {
    let mut scan = 0;
    let mut result = None;
    while scan < before_ip {
        let op = match ROp::from_byte(instructions[scan]) { Some(op) => op, None => break };
        let size = op.size();
        if size >= 3 {
            let dst = read_u16(instructions, scan + 1);
            if dst == reg {
                if op == ROp::LoadConst {
                    let cidx = read_u16(instructions, scan + 3) as usize;
                    result = if cidx < constants.len() {
                        match &constants[cidx] {
                            crate::object::Object::String(s) => {
                                // Try to get NaN-boxed inline string bits
                                let bits = crate::value::Value::from_inline_str(s.as_bytes())
                                    .map(|v| v.bits());
                                Some((s.len(), bits))
                            },
                            _ => None,
                        }
                    } else { None };
                } else {
                    result = None;
                }
            }
        }
        scan += size;
    }
    result
}

/// Check if bytecode has any property ops with compile-time resolvable slots.
fn has_fixed_prop_slots(instructions: &[u8], constants: &[crate::object::Object]) -> bool {
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => break,
        };
        match op {
            ROp::AddConstToRegProp | ROp::GetProp | ROp::SetProp => {
                let prop_c = read_u16(instructions, ip + 3) as usize;
                if let Some(sym) = intern_const(constants, prop_c) {
                    if resolve_prop_slot(instructions, constants, sym).is_some() {
                        return true;
                    }
                }
            }
            _ => {}
        }
        ip += op.size();
    }
    false
}

/// Check if bytecode uses any heap operations that need vm_ptr.
fn needs_vm_ptr(instructions: &[u8]) -> bool {
    let mut ip = 0;
    while ip < instructions.len() {
        let op = match ROp::from_byte(instructions[ip]) {
            Some(op) => op,
            None => return false,
        };
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
        if self.compiled.contains_key(&func_key) {
            return false;
        }
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

    /// Does the compiled function use calls (needs 4th vm_ptr param)?
    pub fn has_calls(&self, func_key: usize) -> bool {
        self.compiled.get(&func_key).is_some_and(|f| f.has_calls)
    }

    /// Execute a compiled function from a raw pointer (no calls variant).
    /// Windows x64 ABI: rcx=regs, rdx=consts, r8=globals
    ///
    /// # Safety
    /// `fn_ptr` must point to valid JIT-compiled code. Register and constant
    /// pointers must be valid for the function's register window.
    pub unsafe fn execute_ptr(
        fn_ptr: *const u8,
        regs: *mut u64,
        consts: *const u64,
        globals: *mut u64,
    ) -> u64 {
        let f: extern "win64" fn(*mut u64, *const u64, *mut u64) -> u64 =
            mem::transmute(fn_ptr);
        f(regs, consts, globals)
    }

    /// Execute a compiled function with vm_ptr (calls variant).
    /// Windows x64 ABI: rcx=regs, rdx=consts, r8=globals, r9=vm_ptr
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
        let f: extern "win64" fn(*mut u64, *const u64, *mut u64, *mut u8) -> u64 =
            mem::transmute(fn_ptr);
        f(regs, consts, globals, vm_ptr)
    }

    /// Compile a function to x86-64 machine code.
    /// `self_val`: NaN-boxed value of this function (for self-call detection).
    /// `reg_count`: number of registers in this function's window.
    /// `layout`: struct offsets for inline property caching.
    #[allow(clippy::too_many_arguments)]
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
        let has_calls = needs_vm_ptr(instructions);

        // For !has_calls functions, Add/Sub/Mul only handle i32. Reject if
        // any arithmetic operand might hold non-i32 (strings from GetGlobal, etc.)
        if !has_calls {
            let non_i32 = find_non_i32_registers(instructions, constants);
            let mut ip2 = 0;
            while ip2 < instructions.len() {
                let op2 = match ROp::from_byte(instructions[ip2]) { Some(o) => o, None => break };
                if matches!(op2, ROp::Add | ROp::Sub | ROp::Mul | ROp::Div | ROp::Mod) {
                    let left = read_u16(instructions, ip2 + 3) as usize;
                    let right = read_u16(instructions, ip2 + 5) as usize;
                    if (left < 64 && non_i32[left]) || (right < 64 && non_i32[right]) {
                        return false;
                    }
                }
                ip2 += op2.size();
            }
        }
        // Reject non-self Call/CallGlobal.
        // Count CallMethod/New functions separately for bisection.
        if has_calls {
            let uc = count_user_calls(instructions);
            if uc > 0 {
                let (all_self, _) = all_calls_are_self(instructions, self_val, globals_ptr, globals_len);
                if !all_self { return false; }
            }
            // For functions with CallMethod/New: use bisect counter
            let has_cm_or_new = num_calls > uc;
            if has_cm_or_new {
                return false;
            }
            if instructions.len() > 150 { return false; }
        }
        if instructions.len() > 2000 {
            return false;
        }
        let reg_window = (reg_count as i32).max(1);
        // cached_obj_offset — used to invalidate cached_hash_obj after calls
        let co_off = layout.cached_obj_offset as i32;
        // Verify all calls target THIS function's global slot (not just any single target)
        let (guaranteed_self, self_global_idx) =
            all_calls_are_self(instructions, self_val, globals_ptr, globals_len);
        // Register pinning: for compute-only functions (no calls),
        // pin up to 3 most-used VM registers to esi/edi/r10d.
        // rbx holds I32_SIG constant (loaded once), freeing r10 for pinning.
        // Pin 2 regs (esi/edi) for small functions, 3 regs (+r10d) for larger loops
        let pins = if !has_calls && instructions.len() >= 40 {
            find_pinned_registers_3(instructions, constants)
        } else if !has_calls {
            let p = find_pinned_registers(instructions, constants);
            [p.map(|p| p.0), p.map(|p| p.1), None]
        } else {
            [None; 3]
        };
        let pin0 = pins[0].map(|p| p as i32); // VM reg → esi
        let pin1 = pins[1].map(|p| p as i32); // VM reg → edi
        let pin2 = pins[2].map(|p| p as i32); // VM reg → r10d (I32_SIG now in rbx)
        // For !has_calls: rbx = I32_SIG constant (avoids repeated 10-byte mov r10, imm64)
        let use_rbx_sig = !has_calls;
        // If all constants are i32, rdx (consts_ptr) is never needed after init → skip push/pop
        let rdx_free = use_rbx_sig && constants.iter().all(|c|
            matches!(c, crate::object::Object::Integer(v) if *v >= i32::MIN as i64 && *v <= i32::MAX as i64));
        // Cache cached_values_ptr in rdi for property-heavy functions (rdi is non-volatile on Win64)
        let use_rdi_cache = has_calls && has_fixed_prop_slots(instructions, constants);
        // Pin rbx to I32_SIG even in has_calls functions that have no Call/CallGlobal
        let use_rbx_in_calls = use_rdi_cache && num_calls == 0;
        // When true, rbx holds I32_SIG for the entire function body — skip redundant
        // 10-byte `mov rbx, QWORD I32_SIG` loads in generated code.
        let rbx_is_sig = use_rbx_sig || use_rbx_in_calls;
        let jrp_off = layout.jit_regs_ptr_offset as i32;

        let mut ops = dynasmrt::x64::Assembler::new().unwrap();

        // Create a label for every bytecode offset
        let mut labels: Vec<dynasmrt::DynamicLabel> = Vec::with_capacity(instructions.len() + 1);
        for _ in 0..=instructions.len() {
            labels.push(ops.new_dynamic_label());
        }

        // Windows x64 ABI: rcx=regs_ptr, rdx=consts_ptr, r8=globals_ptr, r9=vm_ptr
        // Scratch: rax, r9(non-call), r10, r11
        // Pinned (when !has_calls): esi = VM reg pin0, edi = VM reg pin1
        //
        // For functions that need vm_ptr (Call, Array, Index, SetIndex, GetProp, SetProp, Hash),
        // we save to nonvolatile registers:
        //   r12=regs, r13=consts, r14=globals, r15=vm_ptr
        // and allocate 32 bytes shadow space for helper calls.

        let entry_label = ops.new_dynamic_label();
        let self_entry_label = ops.new_dynamic_label();

        if has_calls {
            if use_rdi_cache {
                // External entry: 5 pushes (odd count → 16-aligned without extra sub)
                dynasm!(ops
                    ; =>entry_label
                    ; push rbx
                    ; push rdi
                    ; push r13
                    ; push r14
                    ; push r15
                    ; mov r13, rdx
                    ; mov r14, r8
                    ; mov r15, r9
                    ; call =>self_entry_label
                    ; pop r15
                    ; pop r14
                    ; pop r13
                    ; pop rdi
                    ; pop rbx
                    ; ret
                );
                // Self entry: save r12+rdi, sub 40 for shadow space + alignment
                // Entry rsp≡8(mod16), 2 pushes→≡8, sub 40→≡0 (mod16) ✓
                dynasm!(ops
                    ; =>self_entry_label
                    ; push r12
                    ; push rdi
                    ; sub rsp, 40
                    ; mov r12, rcx
                );
                if use_rbx_in_calls {
                    dynasm!(ops ; mov rbx, QWORD I32_SIG as i64);
                }
            } else {
                // External entry: 4 pushes + sub 8 for alignment
                dynasm!(ops
                    ; =>entry_label
                    ; push rbx
                    ; push r13
                    ; push r14
                    ; push r15
                    ; sub rsp, 8
                    ; mov r13, rdx
                    ; mov r14, r8
                    ; mov r15, r9
                    ; call =>self_entry_label
                    ; add rsp, 8
                    ; pop r15
                    ; pop r14
                    ; pop r13
                    ; pop rbx
                    ; ret
                );
                // Self entry: save r12, sub 32 for shadow space
                dynasm!(ops
                    ; =>self_entry_label
                    ; push r12
                    ; sub rsp, 32
                    ; mov r12, rcx
                );
            }
        }

        // Load pinned registers from register window at function entry.
        // Win64 ABI: rsi, rdi, rbx are non-volatile (callee-saved) — must push/pop.
        // r10 is volatile, no save needed.
        if use_rbx_sig {
            // Save all non-volatile pinned registers before use
            if pin0.is_some() { dynasm!(ops ; push rsi); }
            if pin1.is_some() { dynasm!(ops ; push rdi); }
            dynasm!(ops
                ; push rbx                          // save caller's rbx (callee-saved)
                ; mov rbx, QWORD I32_SIG as i64     // constant for re-boxing
            );
        }
        if let Some(p0) = pin0 { dynasm!(ops ; mov esi, [rcx + p0 * 8]); }
        if let Some(p1) = pin1 { dynasm!(ops ; mov edi, [rcx + p1 * 8]); }
        if let Some(p2) = pin2 { dynasm!(ops ; mov r10d, [rcx + p2 * 8]); }

        // Peephole: track Add results that will be consumed by a fused CallMethod
        let mut pending_strcat_fuse: Option<(i32, i32)> = None;
        // Track last verified obj_r for consecutive property ops (skip redundant checks)
        let mut last_verified_obj: Option<i32> = None;

        let mut ip = 0;
        while ip < instructions.len() {
            dynasm!(ops ; =>labels[ip]);

            let op = ROp::from_byte(instructions[ip]).unwrap();

            // Reset obj verification for non-consecutive property ops.
            // SetProp resets because it can add new properties (changing shape
            // and reallocating values Vec, invalidating cached_values_ptr).
            match op {
                ROp::AddConstToRegProp | ROp::AddRegPropsToRegProp | ROp::GetProp => {}
                _ => { last_verified_obj = None; }
            }

            match op {
                // LoadConst [dst:2, const_idx:2]
                ROp::LoadConst => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let idx = read_u16(instructions, ip + 3) as usize;
                    let imm = const_i32(constants, idx);
                    let idx_i32 = idx as i32;
                    // For pinned registers with known immediate, use mov reg, imm
                    if pin0 == Some(dst) {
                        if let Some(v) = imm { dynasm!(ops ; mov esi, v); }
                        else { dynasm!(ops ; mov esi, [rdx + idx_i32 * 8]); }
                    } else if pin1 == Some(dst) {
                        if let Some(v) = imm { dynasm!(ops ; mov edi, v); }
                        else { dynasm!(ops ; mov edi, [rdx + idx_i32 * 8]); }
                    } else if pin2 == Some(dst) {
                        if let Some(v) = imm { dynasm!(ops ; mov r10d, v); }
                        else { dynasm!(ops ; mov r10d, [rdx + idx_i32 * 8]); }
                    } else if let Some(v) = imm {
                        // Non-pinned: store NaN-boxed immediate directly
                        let boxed = I32_SIG | (v as u32 as u64);
                        dynasm!(ops
                            ; mov rax, QWORD boxed as i64
                            ; mov [rcx + dst * 8], rax
                        );
                    } else {
                        dynasm!(ops
                            ; mov rax, [rdx + idx_i32 * 8]
                            ; mov [rcx + dst * 8], rax
                        );
                    }
                }
                ROp::LoadTrue => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    dynasm!(ops
                        ; mov rax, QWORD VAL_TRUE as i64
                        ; mov [rcx + dst * 8], rax
                    );
                }
                ROp::LoadFalse => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    dynasm!(ops
                        ; mov rax, QWORD VAL_FALSE as i64
                        ; mov [rcx + dst * 8], rax
                    );
                }
                ROp::LoadNull => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    // Previously emitted VAL_UNDEFINED here — a typo
                    // that broke `x === null` checks in any function
                    // the JIT compiled. Identity-confusion bug.
                    dynasm!(ops
                        ; mov rax, QWORD VAL_NULL as i64
                        ; mov [rcx + dst * 8], rax
                    );
                }
                ROp::LoadUndef => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    dynasm!(ops
                        ; mov rax, QWORD VAL_UNDEFINED as i64
                        ; mov [rcx + dst * 8], rax
                    );
                }
                ROp::Move => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let src = read_u16(instructions, ip + 3) as i32;
                    // Pinned → pinned or memory
                    if pin0 == Some(src) {
                        if pin1 == Some(dst) { dynasm!(ops ; mov edi, esi); }
                        else if pin0 == Some(dst) { /* nop: same register */ }
                        else {
                            if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                            dynasm!(ops ; mov eax, esi ; or rax, rbx ; mov [rcx + dst * 8], rax);
                        }
                    } else if pin1 == Some(src) {
                        if pin0 == Some(dst) { dynasm!(ops ; mov esi, edi); }
                        else if pin1 == Some(dst) { /* nop */ }
                        else {
                            if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                            dynasm!(ops ; mov eax, edi ; or rax, rbx ; mov [rcx + dst * 8], rax);
                        }
                    } else if pin0 == Some(dst) {
                        dynasm!(ops ; mov esi, [rcx + src * 8]);
                    } else if pin1 == Some(dst) {
                        dynasm!(ops ; mov edi, [rcx + src * 8]);
                    } else if pin2 == Some(dst) {
                        if pin0 == Some(src) { dynasm!(ops ; mov r10d, esi); }
                        else if pin1 == Some(src) { dynasm!(ops ; mov r10d, edi); }
                        else { dynasm!(ops ; mov r10d, [rcx + src * 8]); }
                    } else {
                        dynasm!(ops ; mov rax, [rcx + src * 8] ; mov [rcx + dst * 8], rax);
                    }
                }
                ROp::GetGlobal => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let idx = read_u16(instructions, ip + 3);
                    // Skip loading function reference when guaranteed_self
                    // (the value is only used for Call's self-check, which is skipped)
                    if self_global_idx != Some(idx) {
                        let idx = idx as i32;
                        dynasm!(ops
                            ; mov rax, [r8 + idx * 8]
                            ; mov [rcx + dst * 8], rax
                        );
                    }
                }
                ROp::SetGlobal => {
                    let idx = read_u16(instructions, ip + 1) as i32;
                    let src = read_u16(instructions, ip + 3) as i32;
                    dynasm!(ops
                        ; mov rax, [rcx + src * 8]
                        ; mov [r8 + idx * 8], rax
                    );
                }

                // ── Arithmetic ──
                ROp::Add => {
                    let dst = read_u16(instructions, ip + 1);
                    let left = read_u16(instructions, ip + 3) as i32;
                    let right = read_u16(instructions, ip + 5) as i32;
                    let dst_i32 = dst as i32;

                    // Peephole: check if this Add result feeds a CallMethod within
                    // the next 2 instructions. If so, skip codegen and let CallMethod
                    // handle the fused string-concat + Map operation.
                    let mut fused = false;
                    if has_calls {
                        let mut scan = ip + 7; // after this Add
                        'scan: for _ in 0..2 {
                            if scan >= instructions.len() { break; }
                            if let Some(next_op) = ROp::from_byte(instructions[scan]) {
                                if next_op == ROp::CallMethod {
                                    let cm_base = read_u16(instructions, scan + 3);
                                    if cm_base + 1 == dst {
                                        pending_strcat_fuse = Some((left, right));
                                        fused = true;
                                        break 'scan;
                                    }
                                }
                                scan += next_op.size();
                            } else { break; }
                        }
                    }
                    if fused {
                        ip += op.size();
                        continue;
                    }

                    let dst = dst_i32;
                    if has_calls {
                        // Check if right operand is a known string constant
                        let right_str_info = known_str_const_info(
                            instructions, constants, right as u16, ip);
                        let right_str_len = right_str_info.map(|(len, _)| len);
                        let right_inline_bits = right_str_info.and_then(|(_, bits)| bits);
                        let done = ops.new_dynamic_label();

                        if let Some(str_len) = right_str_len {
                            // Right is a known string — skip i32 check entirely.
                            // Inline rope fast path: check left is heap rope,
                            // read total_len, call minimal alloc helper.
                            let heap_tag_hi = 0x7FFEi32;
                            let heap_ptr_off = layout.heap_offset as i32 + 8;
                            let obj_size = layout.object_size as i32;
                            let rope_disc = layout.rope_discriminant as i32;
                            let tl_off = layout.rope_total_len_offset as i32;
                            let str_len_i32 = str_len as i32;

                            dynasm!(ops
                                ; mov rax, [r12 + left * 8]
                                ; mov r10, rax
                                ; shr r10, 48
                                ; cmp r10d, heap_tag_hi
                                ; jne >rope_slow
                                ; mov r10d, eax
                                ; mov r11, [r15 + heap_ptr_off]
                            );
                            if obj_size == 32 {
                                dynasm!(ops ; shl r10, 5);
                            } else {
                                dynasm!(ops ; imul r10, r10, obj_size);
                            }
                            dynasm!(ops
                                ; add r11, r10
                                ; cmp BYTE [r11], rope_disc as i8
                                ; jne >rope_slow
                                ; mov r10, [r11 + tl_off]
                                ; add r10, str_len_i32
                                ; cmp r10, 16
                                ; jle >rope_slow
                            );
                            // Fully inline rope allocation — no helper call on fast path!
                            // r10 = total_len, left/right in registers
                            let bump_next_off = layout.bump_next_offset as i32;
                            let bump_end_off = layout.bump_end_offset as i32;
                            let fl_len_off = layout.free_list_len_offset as i32;
                            let fl_ptr_off = layout.free_list_ptr_offset as i32;
                            let obj_len_off = layout.objects_len_offset as i32;
                            let obj_cap_off = obj_len_off - 16;
                            let heap_tag_val = layout.heap_tag as i64;
                            dynasm!(ops
                                // Bump fast path: pre-allocated slots, just increment
                                ; mov r9d, [r15 + bump_next_off]
                                ; cmp r9d, [r15 + bump_end_off]
                                ; jge >rope_try_freelist
                                ; lea r11d, [r9d + 1]
                                ; mov [r15 + bump_next_off], r11d
                                ; jmp >rope_write
                                ; rope_try_freelist:
                                // Free list fallback
                                ; mov r11, [r15 + fl_len_off]
                                ; test r11, r11
                                ; jz >rope_alloc_push
                                ; dec r11
                                ; mov [r15 + fl_len_off], r11
                                ; mov r9, [r15 + fl_ptr_off]
                                ; mov r9d, [r9 + r11 * 4]
                                ; jmp >rope_write
                                ; rope_alloc_push:
                                ; mov r9, [r15 + obj_len_off]
                                ; cmp r9, [r15 + obj_cap_off]
                                ; jge >rope_slow
                                ; lea r11, [r9 + 1]
                                ; mov [r15 + obj_len_off], r11
                                ; rope_write:
                                // Write StringRopeNode at objects[r9d]
                                ; mov r11, [r15 + heap_ptr_off]  // objects.ptr
                            );
                            if obj_size == 32 {
                                dynasm!(ops
                                    ; mov rax, r9
                                    ; shl rax, 5
                                    ; add r11, rax
                                );
                            } else {
                                dynasm!(ops
                                    ; imul rax, r9, obj_size
                                    ; add r11, rax
                                );
                            }
                            dynasm!(ops
                                ; mov BYTE [r11], rope_disc as i8       // discriminant
                                ; mov rax, [r12 + left * 8]
                                ; mov [r11 + 8], rax                    // left
                            );
                            // Embed right value as immediate if it's an inline string
                            if let Some(bits) = right_inline_bits {
                                dynasm!(ops
                                    ; mov rax, QWORD bits as i64
                                    ; mov [r11 + 16], rax               // right (embedded constant)
                                );
                            } else {
                                dynasm!(ops
                                    ; mov rax, [r12 + right * 8]
                                    ; mov [r11 + 16], rax               // right (from register)
                                );
                            }
                            dynasm!(ops
                                ; mov [r11 + 24], r10                   // total_len
                                // Create NaN-boxed heap value: HEAP_TAG | idx
                                ; mov rax, QWORD heap_tag_val
                                ; or rax, r9
                                ; jmp =>done
                                ; rope_slow:
                            );
                            // Fallback: full rope_append_helper (for non-rope left, short strings, etc.)
                            let helper_addr = crate::vm::djit_rope_append_helper as usize as i64;
                            dynasm!(ops
                                ; mov rdx, [r12 + left * 8]
                                ; mov r8, [r12 + right * 8]
                                ; xor r9d, r9d
                                ; mov rcx, r15
                                ; mov rax, QWORD helper_addr
                                ; call rax
                                ; mov rcx, r12
                                ; mov rdx, r13
                                ; mov r8, r14
                                ; =>done
                                ; mov [rcx + dst * 8], rax
                            );
                        } else {
                            // Standard i32 fast path + rope slow path
                            let i32_sig_hi = (I32_SIG >> 48) as i32;
                            dynasm!(ops
                                ; mov rax, [rcx + left * 8]
                                ; mov r9, [rcx + right * 8]
                                ; mov r10, rax
                                ; shr r10, 48
                                ; cmp r10d, i32_sig_hi
                                ; jne >slow_add
                                ; mov r10, r9
                                ; shr r10, 48
                                ; cmp r10d, i32_sig_hi
                                ; jne >slow_add
                                // Both i32: fast path (add zero-extends, no mov eax,eax needed)
                                ; add eax, r9d
                            );
                            if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                            dynasm!(ops
                                ; or rax, rbx
                                ; jmp =>done
                                ; slow_add:
                            );
                            let helper_addr = crate::vm::djit_rope_append_helper as usize as i64;
                            dynasm!(ops
                                ; mov rdx, [r12 + left * 8]
                                ; mov r8, [r12 + right * 8]
                                ; xor r9d, r9d
                                ; mov rcx, r15
                                ; mov rax, QWORD helper_addr
                                ; call rax
                                ; mov rcx, r12
                                ; mov rdx, r13
                                ; mov r8, r14
                                ; =>done
                                ; mov [rcx + dst * 8], rax
                            );
                        }
                    } else {
                        // Pure i32 path — use pinned registers when available
                        // Special case: dst == left (common in accumulate patterns)
                        if pin0 == Some(dst) && pin0 == Some(left) {
                            if pin1 == Some(right) {
                                dynasm!(ops ; add esi, edi);
                            } else {
                                dynasm!(ops ; add esi, [rcx + right * 8]);
                            }
                        } else if pin1 == Some(dst) && pin1 == Some(left) {
                            if pin0 == Some(right) {
                                dynasm!(ops ; add edi, esi);
                            } else {
                                dynasm!(ops ; add edi, [rcx + right * 8]);
                            }
                        } else {
                            // Load left
                            if pin0 == Some(left) { dynasm!(ops ; mov eax, esi); }
                            else if pin1 == Some(left) { dynasm!(ops ; mov eax, edi); }
                            else { dynasm!(ops ; mov eax, [rcx + left * 8]); }
                            // Add right
                            if pin0 == Some(right) { dynasm!(ops ; add eax, esi); }
                            else if pin1 == Some(right) { dynasm!(ops ; add eax, edi); }
                            else { dynasm!(ops ; add eax, [rcx + right * 8]); }
                            // Store dst
                            if pin0 == Some(dst) { dynasm!(ops ; mov esi, eax); }
                            else if pin1 == Some(dst) { dynasm!(ops ; mov edi, eax); }
                            else {
                                if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                                dynasm!(ops ; or rax, rbx ; mov [rcx + dst * 8], rax);
                            }
                        }
                    }
                }
                ROp::Sub | ROp::Mul => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let left = read_u16(instructions, ip + 3) as i32;
                    let right = read_u16(instructions, ip + 5) as i32;
                    // Load left
                    if pin0 == Some(left) { dynasm!(ops ; mov eax, esi); }
                    else if pin1 == Some(left) { dynasm!(ops ; mov eax, edi); }
                    else { dynasm!(ops ; mov eax, [rcx + left * 8]); }
                    // Load right
                    if pin0 == Some(right) { dynasm!(ops ; mov r9d, esi); }
                    else if pin1 == Some(right) { dynasm!(ops ; mov r9d, edi); }
                    else { dynasm!(ops ; mov r9d, [rcx + right * 8]); }
                    match op {
                        ROp::Sub => dynasm!(ops ; sub eax, r9d),
                        ROp::Mul => dynasm!(ops ; imul eax, r9d),
                        _ => unreachable!(),
                    }
                    // Store dst
                    if pin0 == Some(dst) {
                        dynasm!(ops ; mov esi, eax);
                    } else if pin1 == Some(dst) {
                        dynasm!(ops ; mov edi, eax);
                    } else {
                        // sub/imul already zero-extend eax→rax; no mov eax,eax needed
                        if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                        dynasm!(ops ; or rax, rbx ; mov [rcx + dst * 8], rax);
                    }
                }

                ROp::Div => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let left = read_u16(instructions, ip + 3) as i32;
                    let right = read_u16(instructions, ip + 5) as i32;
                    if !rdx_free { dynasm!(ops ; push rdx); }
                    dynasm!(ops
                        ; mov eax, [rcx + left * 8]
                        ; cdq
                        ; mov r9d, [rcx + right * 8]
                        ; idiv r9d
                        // eax = quotient (Div), edx = remainder (Mod)
                    );
                    if !rdx_free { dynasm!(ops ; pop rdx); }
                    if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                    dynasm!(ops ; or rax, rbx ; mov [rcx + dst * 8], rax);
                }

                ROp::Mod => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let left = read_u16(instructions, ip + 3) as i32;
                    let right = read_u16(instructions, ip + 5) as i32;
                    if !rdx_free { dynasm!(ops ; push rdx); }
                    dynasm!(ops
                        ; mov eax, [rcx + left * 8]
                        ; cdq
                        ; mov r9d, [rcx + right * 8]
                        ; idiv r9d
                        ; mov eax, edx
                    );
                    if !rdx_free { dynasm!(ops ; pop rdx); }
                    if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                    dynasm!(ops ; or rax, rbx ; mov [rcx + dst * 8], rax);
                }

                // ── Fused Reg+Const ops ──
                ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let src = read_u16(instructions, ip + 3) as i32;
                    let cidx = read_u16(instructions, ip + 5) as i32;
                    let imm_c = const_i32(constants, cidx as usize);
                    // Pinned fast path: dst == src == pinned register
                    if pin0 == Some(dst) && pin0 == Some(src) && op == ROp::AddRegConst {
                        if let Some(v) = imm_c { dynasm!(ops ; add esi, v); }
                        else { dynasm!(ops ; add esi, [rdx + cidx * 8]); }
                        ip += op.size(); continue;
                    } else if pin1 == Some(dst) && pin1 == Some(src) && op == ROp::AddRegConst {
                        if let Some(v) = imm_c { dynasm!(ops ; add edi, v); }
                        else { dynasm!(ops ; add edi, [rdx + cidx * 8]); }
                        ip += op.size(); continue;
                    } else if pin2 == Some(dst) && pin2 == Some(src) && op == ROp::AddRegConst {
                        if let Some(v) = imm_c { dynasm!(ops ; add r10d, v); }
                        else { dynasm!(ops ; add r10d, [rdx + cidx * 8]); }
                        ip += op.size(); continue;
                    }
                    // Load source (from pinned or memory)
                    if pin0 == Some(src) {
                        dynasm!(ops ; mov eax, esi);
                    } else if pin1 == Some(src) {
                        dynasm!(ops ; mov eax, edi);
                    } else {
                        dynasm!(ops ; mov eax, [rcx + src * 8]);
                    }
                    dynasm!(ops ; mov r9d, [rdx + cidx * 8]);
                    match op {
                        ROp::AddRegConst => dynasm!(ops ; add eax, r9d),
                        ROp::SubRegConst => dynasm!(ops ; sub eax, r9d),
                        ROp::MulRegConst => dynasm!(ops ; imul eax, r9d),
                        _ => unreachable!(),
                    }
                    // Store to pinned or memory
                    if pin0 == Some(dst) {
                        dynasm!(ops ; mov esi, eax);
                        ip += op.size();
                        continue;
                    } else if pin1 == Some(dst) {
                        dynasm!(ops ; mov edi, eax);
                        ip += op.size();
                        continue;
                    }
                    if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                    dynasm!(ops ; or rax, rbx ; mov [rcx + dst * 8], rax);
                }

                // ── Comparisons ──
                ROp::LessThan | ROp::LessOrEqual | ROp::GreaterThan
                | ROp::GreaterOrEqual => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let left = read_u16(instructions, ip + 3) as i32;
                    let right = read_u16(instructions, ip + 5) as i32;
                    dynasm!(ops
                        ; mov eax, [rcx + left * 8]
                        ; cmp eax, [rcx + right * 8]
                    );
                    let true_label = ops.new_dynamic_label();
                    let done_label = ops.new_dynamic_label();
                    match op {
                        ROp::LessThan => dynasm!(ops ; jl =>true_label),
                        ROp::LessOrEqual => dynasm!(ops ; jle =>true_label),
                        ROp::GreaterThan => dynasm!(ops ; jg =>true_label),
                        ROp::GreaterOrEqual => dynasm!(ops ; jge =>true_label),
                        _ => unreachable!(),
                    }
                    dynasm!(ops
                        ; mov rax, QWORD VAL_FALSE as i64
                        ; jmp =>done_label
                        ; =>true_label
                        ; mov rax, QWORD VAL_TRUE as i64
                        ; =>done_label
                        ; mov [rcx + dst * 8], rax
                    );
                }
                ROp::StrictEqual | ROp::StrictNotEqual
                | ROp::Equal | ROp::NotEqual => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let left = read_u16(instructions, ip + 3) as i32;
                    let right = read_u16(instructions, ip + 5) as i32;
                    let is_neq = matches!(op, ROp::StrictNotEqual | ROp::NotEqual);
                    let true_label = ops.new_dynamic_label();
                    let done_label = ops.new_dynamic_label();
                    // Fast path: bitwise equal → true (for ===) or false (for !==)
                    dynasm!(ops
                        ; mov rax, [rcx + left * 8]
                        ; cmp rax, [rcx + right * 8]
                        ; je =>true_label
                    );
                    // Bits differ: call helper for string/object equality
                    if has_calls {
                        // has_calls path: r15=vm_ptr, r12=regs
                        dynasm!(ops
                            ; mov rcx, r15               // vm_raw
                            ; mov rdx, [r12 + left * 8]  // left_bits
                            ; mov r8, [r12 + right * 8]  // right_bits
                            ; xor r9, r9                 // unused
                            ; mov rax, QWORD crate::vm::djit_strict_eq_helper as i64
                            ; call rax
                            ; mov rcx, r12               // restore regs ptr
                        );
                    } else {
                        // no-calls path: rcx=regs, no vm_ptr available
                        // Fall back to bits-not-equal → false
                        dynasm!(ops ; mov rax, QWORD VAL_FALSE as i64);
                    }
                    if is_neq {
                        // For !==: invert the result (swap TRUE↔FALSE)
                        dynasm!(ops
                            ; xor rax, 1  // toggle low bit: TRUE→FALSE, FALSE→TRUE
                        );
                    }
                    dynasm!(ops
                        ; jmp =>done_label
                        ; =>true_label
                    );
                    if is_neq {
                        dynasm!(ops ; mov rax, QWORD VAL_FALSE as i64);
                    } else {
                        dynasm!(ops ; mov rax, QWORD VAL_TRUE as i64);
                    }
                    dynasm!(ops
                        ; =>done_label
                        ; mov [rcx + dst * 8], rax
                    );
                }

                ROp::Neg => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let src = read_u16(instructions, ip + 3) as i32;
                    dynasm!(ops
                        ; mov eax, [rcx + src * 8]
                        ; neg eax
                    );
                    if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                    dynasm!(ops ; or rax, rbx ; mov [rcx + dst * 8], rax);
                }

                ROp::Not => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let src = read_u16(instructions, ip + 3) as i32;
                    let done_not = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rax, [rcx + src * 8]
                        ; mov r9, QWORD VAL_TRUE as i64
                        ; cmp rax, r9
                        ; je >not_was_true
                        ; mov rax, QWORD VAL_TRUE as i64
                        ; jmp =>done_not
                        ; not_was_true:
                        ; mov rax, QWORD VAL_FALSE as i64
                        ; =>done_not
                        ; mov [rcx + dst * 8], rax
                    );
                }

                ROp::IsNullish => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let src = read_u16(instructions, ip + 3) as i32;
                    // null = 0x7FFB_0000_0000_0000, undefined = 0x7FFC_0000_0000_0000
                    // Both have bits[51:48] >= 3. Check: (val >> 48) >= 0x7FFB
                    dynasm!(ops
                        ; mov rax, [rcx + src * 8]
                        ; mov r9, QWORD VAL_NULL as i64
                        ; cmp rax, r9
                        ; je >is_nullish
                        ; mov r9, QWORD VAL_UNDEFINED as i64
                        ; cmp rax, r9
                        ; je >is_nullish
                        ; mov rax, QWORD VAL_FALSE as i64
                        ; jmp >nullish_done
                        ; is_nullish:
                        ; mov rax, QWORD VAL_TRUE as i64
                        ; nullish_done:
                        ; mov [rcx + dst * 8], rax
                    );
                }

                // ── Control flow ──
                ROp::Jump => {
                    let target = read_u32(instructions, ip + 1) as usize;
                    dynasm!(ops ; jmp =>labels[target]);
                }
                ROp::JumpIfNot => {
                    let cond = read_u16(instructions, ip + 1) as i32;
                    let target = read_u32(instructions, ip + 3) as usize;
                    dynasm!(ops
                        ; mov rax, [rcx + cond * 8]
                        ; mov r9, QWORD VAL_TRUE as i64
                        ; cmp rax, r9
                        ; jne =>labels[target]
                    );
                }
                ROp::JumpIfTruthy => {
                    let cond = read_u16(instructions, ip + 1) as i32;
                    let target = read_u32(instructions, ip + 3) as usize;
                    dynasm!(ops
                        ; mov rax, [rcx + cond * 8]
                        ; mov r9, QWORD VAL_TRUE as i64
                        ; cmp rax, r9
                        ; je =>labels[target]
                    );
                }

                // ── Fused comparison+jump ──
                ROp::TestLtConstJump | ROp::TestLeConstJump => {
                    let r = read_u16(instructions, ip + 1) as i32;
                    let cidx = read_u16(instructions, ip + 3) as usize;
                    let target = read_u32(instructions, ip + 5) as usize;
                    let imm = const_i32(constants, cidx);
                    let cidx_i32 = cidx as i32;
                    // Use pinned register + immediate constant when available
                    match (pin0 == Some(r), pin1 == Some(r), pin2 == Some(r), imm) {
                        (true, _, _, Some(v)) => dynasm!(ops ; cmp esi, v),
                        (_, true, _, Some(v)) => dynasm!(ops ; cmp edi, v),
                        (_, _, true, Some(v)) => dynasm!(ops ; cmp r10d, v),
                        (true, _, _, None) => dynasm!(ops ; cmp esi, [rdx + cidx_i32 * 8]),
                        (_, true, _, None) => dynasm!(ops ; cmp edi, [rdx + cidx_i32 * 8]),
                        (_, _, true, None) => dynasm!(ops ; cmp r10d, [rdx + cidx_i32 * 8]),
                        _ if imm.is_some() => {
                            let v = imm.unwrap();
                            dynasm!(ops ; mov eax, [rcx + r * 8] ; cmp eax, v);
                        }
                        _ => dynasm!(ops ; mov eax, [rcx + r * 8] ; cmp eax, [rdx + cidx_i32 * 8]),
                    }
                    if op == ROp::TestLtConstJump {
                        dynasm!(ops ; jge =>labels[target]);
                    } else {
                        dynasm!(ops ; jg =>labels[target]);
                    }
                }
                ROp::TestLtRegJump | ROp::TestLeRegJump => {
                    let a = read_u16(instructions, ip + 1) as i32;
                    let b = read_u16(instructions, ip + 3) as i32;
                    let target = read_u32(instructions, ip + 5) as usize;
                    dynasm!(ops
                        ; mov eax, [rcx + a * 8]
                        ; cmp eax, [rcx + b * 8]
                    );
                    if op == ROp::TestLtRegJump {
                        dynasm!(ops ; jge =>labels[target]);
                    } else {
                        dynasm!(ops ; jg =>labels[target]);
                    }
                }
                ROp::IncrementRegAndJump => {
                    let r = read_u16(instructions, ip + 1) as i32;
                    let cidx = read_u16(instructions, ip + 3) as usize;
                    let target = read_u32(instructions, ip + 5) as usize;
                    let imm = const_i32(constants, cidx);
                    let cidx_i32 = cidx as i32;
                    if pin0 == Some(r) {
                        if let Some(v) = imm {
                            dynasm!(ops ; add esi, v ; jmp =>labels[target]);
                        } else {
                            dynasm!(ops ; add esi, [rdx + cidx_i32 * 8] ; jmp =>labels[target]);
                        }
                    } else if pin1 == Some(r) {
                        if let Some(v) = imm {
                            dynasm!(ops ; add edi, v ; jmp =>labels[target]);
                        } else {
                            dynasm!(ops ; add edi, [rdx + cidx_i32 * 8] ; jmp =>labels[target]);
                        }
                    } else if pin2 == Some(r) {
                        if let Some(v) = imm {
                            dynasm!(ops ; add r10d, v ; jmp =>labels[target]);
                        } else {
                            dynasm!(ops ; add r10d, [rdx + cidx_i32 * 8] ; jmp =>labels[target]);
                        }
                    } else {
                        dynasm!(ops
                            ; mov eax, [rcx + r * 8]
                            ; add eax, [rdx + cidx_i32 * 8]
                        );
                        if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                        dynasm!(ops
                            ; mov r9d, eax
                            ; or r9, rbx
                            ; mov [rcx + r * 8], r9
                            ; jmp =>labels[target]
                        );
                    }
                }

                // ── Fused Mod+Compare+Jump ──
                ROp::ModRegConstStrictEqConstJump => {
                    let r = read_u16(instructions, ip + 1) as i32;
                    let mod_c_idx = read_u16(instructions, ip + 3) as usize;
                    let cmp_c_idx = read_u16(instructions, ip + 5) as usize;
                    let cmp_c = cmp_c_idx as i32;
                    let cmp_imm = const_i32(constants, cmp_c_idx);
                    let target = read_u32(instructions, ip + 7) as usize;

                    // Check if divisor is a small constant — use magic multiplication
                    let divisor = if mod_c_idx < constants.len() {
                        match &constants[mod_c_idx] {
                            crate::object::Object::Integer(v) if *v >= 2 && *v <= 7 => Some(*v as i32),
                            _ => None,
                        }
                    } else { None };

                    if let Some(d) = divisor {
                        // Magic constant modulo: avoids expensive idiv
                        // Compute n % d using: n - (n/d)*d
                        // n/d via multiply-high by magic constant
                        let magic: i64 = match d {
                            2 => 0, // special case: use AND
                            3 => 0x55555556,
                            5 => 0x33333334,
                            7 => 0x24924925,
                            _ => 0,
                        };
                        let cmp_c_i32 = cmp_c;
                        if d == 2 {
                            // x % 2: just test bit 0
                            if pin0 == Some(r) { dynasm!(ops ; mov eax, esi); }
                            else if pin1 == Some(r) { dynasm!(ops ; mov eax, edi); }
                            else if pin2 == Some(r) { dynasm!(ops ; mov eax, r10d); }
                            else { dynasm!(ops ; mov eax, [rcx + r * 8]); }
                            dynasm!(ops ; and eax, 1);
                            if let Some(cv) = cmp_imm {
                                dynasm!(ops ; cmp eax, cv ; jne =>labels[target]);
                            } else {
                                dynasm!(ops ; cmp eax, [rdx + cmp_c_i32 * 8] ; jne =>labels[target]);
                            }
                        } else if magic != 0 {
                            // Optimized modulo check using unsigned multiply trick
                            // For n % d == c: compute (n - c) * magic_unsigned, check < threshold
                            let cmp_val = cmp_imm.unwrap_or(0);

                            // Ultra-fast path for d=3: use unsigned multiply + compare
                            // n%3==c iff ((n-c) * 0xAAAAAAAB) < 0x55555556
                            if d == 3 && cmp_imm.is_some() {
                                if pin0 == Some(r) { dynasm!(ops ; mov eax, esi); }
                                else if pin1 == Some(r) { dynasm!(ops ; mov eax, edi); }
                                else if pin2 == Some(r) { dynasm!(ops ; mov eax, r10d); }
                                else { dynasm!(ops ; mov eax, [rcx + r * 8]); }
                                if cmp_val != 0 {
                                    dynasm!(ops ; sub eax, cmp_val);
                                }
                                dynasm!(ops
                                    ; imul eax, eax, 0x55555556i32  // (n-c) * (2^32/3 + 1)
                                    ; cmp eax, 0x55555556i32        // < 2^32/3 + 1 means divisible
                                    ; jae =>labels[target]          // NOT divisible → jump
                                );
                            } else if d == 5 && cmp_imm.is_some() {
                                // n%5==c iff ((n-c) * 0xCCCCCCCD) < 0x33333334
                                if pin0 == Some(r) { dynasm!(ops ; mov eax, esi); }
                                else if pin1 == Some(r) { dynasm!(ops ; mov eax, edi); }
                                else if pin2 == Some(r) { dynasm!(ops ; mov eax, r10d); }
                                else { dynasm!(ops ; mov eax, [rcx + r * 8]); }
                                if cmp_val != 0 {
                                    dynasm!(ops ; sub eax, cmp_val);
                                }
                                dynasm!(ops
                                    ; imul eax, eax, 0x33333334i32
                                    ; cmp eax, 0x33333334i32
                                    ; jae =>labels[target]
                                );
                            } else {
                                // General magic constant path
                                if let Some(cv) = cmp_imm {
                                    dynasm!(ops ; mov r11d, cv);
                                } else {
                                    dynasm!(ops ; mov r11d, [rdx + cmp_c_i32 * 8]);
                                }
                                if !rdx_free { dynasm!(ops ; push rdx); }
                                if pin0 == Some(r) { dynasm!(ops ; mov eax, esi); }
                                else if pin1 == Some(r) { dynasm!(ops ; mov eax, edi); }
                                else if pin2 == Some(r) { dynasm!(ops ; mov eax, r10d); }
                                else { dynasm!(ops ; mov eax, [rcx + r * 8]); }
                                dynasm!(ops
                                    ; mov r9d, eax
                                    ; mov edx, magic as i32
                                    ; imul edx
                                    ; mov eax, edx
                                    ; shr eax, 31
                                    ; add edx, eax
                                    ; imul eax, edx, d
                                    ; sub r9d, eax
                                    ; cmp r9d, r11d
                                );
                                if !rdx_free { dynasm!(ops ; pop rdx); }
                                dynasm!(ops ; jne =>labels[target]);
                            }
                        } else {
                            // Fallback to idiv
                            let mod_c = mod_c_idx as i32;
                            dynasm!(ops
                                ; mov r9d, [rdx + mod_c * 8]
                                ; mov r11d, [rdx + cmp_c_i32 * 8]
                                ; push rdx
                                ; mov eax, [rcx + r * 8]
                                ; cdq
                                ; idiv r9d
                                ; cmp edx, r11d
                                ; pop rdx
                                ; jne =>labels[target]
                            );
                        }
                    } else {
                        let mod_c = mod_c_idx as i32;
                        dynasm!(ops
                            ; mov r9d, [rdx + mod_c * 8]
                            ; mov r11d, [rdx + cmp_c * 8]
                            ; push rdx
                            ; mov eax, [rcx + r * 8]
                            ; cdq
                            ; idiv r9d
                            ; cmp edx, r11d
                            ; pop rdx
                            ; jne =>labels[target]
                        );
                    }
                }
                ROp::TestModRegStrictEqConstJump => {
                    let a = read_u16(instructions, ip + 1) as i32;
                    let b = read_u16(instructions, ip + 3) as i32;
                    let cmp_c = read_u16(instructions, ip + 5) as i32;
                    let target = read_u32(instructions, ip + 7) as usize;
                    dynasm!(ops
                        ; mov r9d, [rcx + b * 8]
                        ; mov r11d, [rdx + cmp_c * 8]
                        ; push rdx
                        ; mov eax, [rcx + a * 8]
                        ; cdq
                        ; idiv r9d
                        ; cmp edx, r11d
                        ; pop rdx
                        ; jne =>labels[target]
                    );
                }

                // ── Bitwise ops ──
                ROp::BitwiseAnd | ROp::BitwiseOr | ROp::BitwiseXor => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let left = read_u16(instructions, ip + 3) as i32;
                    let right = read_u16(instructions, ip + 5) as i32;
                    dynasm!(ops
                        ; mov eax, [rcx + left * 8]
                        ; mov r9d, [rcx + right * 8]
                    );
                    match op {
                        ROp::BitwiseAnd => dynasm!(ops ; and eax, r9d),
                        ROp::BitwiseOr => dynasm!(ops ; or eax, r9d),
                        ROp::BitwiseXor => dynasm!(ops ; xor eax, r9d),
                        _ => unreachable!(),
                    }
                    if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                    dynasm!(ops ; or rax, rbx ; mov [rcx + dst * 8], rax);
                }
                ROp::LeftShift | ROp::RightShift | ROp::UnsignedRightShift => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let left = read_u16(instructions, ip + 3) as i32;
                    let right = read_u16(instructions, ip + 5) as i32;
                    dynasm!(ops
                        ; mov eax, [rcx + left * 8]
                        ; push rcx
                        ; mov ecx, [rcx + right * 8]
                        ; and ecx, 31
                    );
                    match op {
                        ROp::LeftShift => dynasm!(ops ; shl eax, cl),
                        ROp::RightShift => dynasm!(ops ; sar eax, cl),
                        ROp::UnsignedRightShift => dynasm!(ops ; shr eax, cl),
                        _ => unreachable!(),
                    }
                    dynasm!(ops ; pop rcx);
                    if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                    dynasm!(ops ; or rax, rbx ; mov [rcx + dst * 8], rax);
                }

                // ── Function call ──
                ROp::Call => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let base = read_u16(instructions, ip + 3) as i32;
                    let nargs = read_u8(instructions, ip + 5) as i32;

                    // Stack bounds for self-call fast paths. Without
                    // this, JIT→JIT self-recursion advances `r12` by
                    // `reg_window*8` each level with no check — once it
                    // runs past `stack.capacity()`, writes corrupt
                    // adjacent heap memory before the native call
                    // stack eventually dies. The interpreter's
                    // `STACK_SIZE` check at the Call opcode only fires
                    // when the call goes through the interpreter.
                    let stack_ptr_off = layout.stack_ptr_offset as i32;
                    let stack_size_bytes = (STACK_SIZE * 8) as i32;
                    // Callee needs `reg_window` of its own past
                    // `r12 + reg_window*8` — that's the new frame.
                    let needed_bytes = reg_window.saturating_mul(2).saturating_mul(8);

                    if guaranteed_self {
                        let so_bail = ops.new_dynamic_label();
                        let so_done = ops.new_dynamic_label();
                        dynasm!(ops
                            ; mov r10, [r15 + stack_ptr_off]
                            ; add r10, stack_size_bytes
                            ; lea rax, [r12 + needed_bytes]
                            ; cmp rax, r10
                            ; ja =>so_bail
                            ; lea rcx, [r12 + reg_window * 8]
                        );
                        for i in 0..nargs {
                            dynasm!(ops
                                ; mov rax, [r12 + (base + 1 + i) * 8]
                                ; mov [rcx + (1 + i) * 8], rax
                            );
                        }
                        dynasm!(ops
                            ; call =>self_entry_label
                            ; mov rcx, r12                // restore regs ptr
                            ; mov [rcx + dst * 8], rax
                            ; jmp =>so_done
                            ; =>so_bail
                            ; mov rax, QWORD VAL_UNDEFINED as i64
                            ; mov [r12 + dst * 8], rax
                            ; =>so_done
                        );
                    } else {
                        let self_call_label = ops.new_dynamic_label();
                        let done_label = ops.new_dynamic_label();
                        let so_bail = ops.new_dynamic_label();
                        let self_val_i64 = self_val as i64;
                        let helper_addr = crate::vm::djit_call_helper as usize as i64;

                        dynasm!(ops
                            ; mov rax, [rcx + base * 8]
                            ; mov rbx, QWORD self_val_i64
                            ; cmp rax, rbx
                            ; je =>self_call_label
                            // Non-self path
                            ; mov rdx, [r12 + base * 8]
                            ; lea r8, [r12 + (base + 1) * 8]
                            ; mov r9d, nargs
                            ; mov rcx, r15
                            ; mov rax, QWORD helper_addr
                            ; call rax
                            ; mov r12, [r15 + jrp_off]
                            ; mov rcx, r12
                            ; mov rdx, r13
                            ; mov r8, r14
                            ; mov QWORD [r15 + co_off], 0
                            ; mov [rcx + dst * 8], rax
                            ; jmp =>done_label
                            // Self-call path with stack-bounds check
                            ; =>self_call_label
                            ; mov r10, [r15 + stack_ptr_off]
                            ; add r10, stack_size_bytes
                            ; lea rax, [r12 + needed_bytes]
                            ; cmp rax, r10
                            ; ja =>so_bail
                            ; lea rcx, [r12 + reg_window * 8]
                        );
                        for i in 0..nargs {
                            dynasm!(ops
                                ; mov rax, [r12 + (base + 1 + i) * 8]
                                ; mov [rcx + (1 + i) * 8], rax
                            );
                        }
                        dynasm!(ops
                            // Skip rdx/r8 setup: callee only uses rcx
                            ; call =>self_entry_label
                            ; mov rcx, r12
                            ; mov rdx, r13
                            ; mov r8, r14
                            ; mov [rcx + dst * 8], rax
                            ; jmp =>done_label
                            ; =>so_bail
                            ; mov rax, QWORD VAL_UNDEFINED as i64
                            ; mov [r12 + dst * 8], rax
                            ; =>done_label
                        );
                    }
                }

                // ── CallGlobal ── [dst:2, global_idx:2, base:2, nargs:1]
                ROp::CallGlobal => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let global_idx = read_u16(instructions, ip + 3) as i32;
                    let base = read_u16(instructions, ip + 5) as i32;
                    let nargs = read_u8(instructions, ip + 7) as i32;

                    // Stack bounds check — same rationale as ROp::Call.
                    let stack_ptr_off = layout.stack_ptr_offset as i32;
                    let stack_size_bytes = (STACK_SIZE * 8) as i32;
                    let needed_bytes = reg_window.saturating_mul(2).saturating_mul(8);

                    if guaranteed_self {
                        let so_bail = ops.new_dynamic_label();
                        let so_done = ops.new_dynamic_label();
                        dynasm!(ops
                            ; mov r10, [r15 + stack_ptr_off]
                            ; add r10, stack_size_bytes
                            ; lea rax, [r12 + needed_bytes]
                            ; cmp rax, r10
                            ; ja =>so_bail
                            ; lea rcx, [r12 + reg_window * 8]
                        );
                        for i in 0..nargs {
                            dynasm!(ops
                                ; mov rax, [r12 + (base + 1 + i) * 8]
                                ; mov [rcx + (1 + i) * 8], rax
                            );
                        }
                        dynasm!(ops
                            ; call =>self_entry_label
                            ; mov rcx, r12                // restore regs ptr
                            ; mov [rcx + dst * 8], rax
                            ; jmp =>so_done
                            ; =>so_bail
                            ; mov rax, QWORD VAL_UNDEFINED as i64
                            ; mov [r12 + dst * 8], rax
                            ; =>so_done
                        );
                    } else {
                        let self_call_label = ops.new_dynamic_label();
                        let done_label = ops.new_dynamic_label();
                        let so_bail = ops.new_dynamic_label();
                        let self_val_i64 = self_val as i64;
                        let helper_addr = crate::vm::djit_call_helper as usize as i64;

                        dynasm!(ops
                            ; mov rax, [r8 + global_idx * 8]
                            ; mov rbx, QWORD self_val_i64
                            ; cmp rax, rbx
                            ; je =>self_call_label
                            // Non-self path
                            ; mov rdx, [r14 + global_idx * 8]
                            ; lea r8, [r12 + (base + 1) * 8]
                            ; mov r9d, nargs
                            ; mov rcx, r15
                            ; mov rax, QWORD helper_addr
                            ; call rax
                            ; mov r12, [r15 + jrp_off]
                            ; mov rcx, r12
                            ; mov rdx, r13
                            ; mov r8, r14
                            ; mov QWORD [r15 + co_off], 0
                            ; mov [rcx + dst * 8], rax
                            ; jmp =>done_label
                            // Self-call path with stack-bounds check
                            ; =>self_call_label
                            ; mov r10, [r15 + stack_ptr_off]
                            ; add r10, stack_size_bytes
                            ; lea rax, [r12 + needed_bytes]
                            ; cmp rax, r10
                            ; ja =>so_bail
                            ; lea rcx, [r12 + reg_window * 8]
                        );
                        for i in 0..nargs {
                            dynasm!(ops
                                ; mov rax, [r12 + (base + 1 + i) * 8]
                                ; mov [rcx + (1 + i) * 8], rax
                            );
                        }
                        dynasm!(ops
                            // Skip rdx/r8 setup: callee only uses rcx
                            ; call =>self_entry_label
                            ; mov rcx, r12
                            ; mov rdx, r13
                            ; mov r8, r14
                            ; mov [rcx + dst * 8], rax
                            ; jmp =>done_label
                            ; =>so_bail
                            ; mov rax, QWORD VAL_UNDEFINED as i64
                            ; mov [r12 + dst * 8], rax
                            ; =>done_label
                        );
                    }
                }

                // ── New (constructor) ── [dst:2, base:2, nargs:1]
                ROp::New => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let base = read_u16(instructions, ip + 3) as i32;
                    let nargs = read_u8(instructions, ip + 5) as i32;

                    let helper_addr = crate::vm::djit_new_helper as usize as i64;
                    dynasm!(ops
                        ; mov rcx, r15
                        ; mov rdx, [r12 + base * 8]           // constructor
                        ; lea r8, [r12 + (base + 1) * 8]      // args
                        ; mov r9d, nargs
                        ; mov rax, QWORD helper_addr
                        ; call rax
                        ; mov r12, [r15 + jrp_off]
                        ; mov rcx, r12
                        ; mov rdx, r13
                        ; mov r8, r14
                        ; mov QWORD [r15 + co_off], 0
                        ; mov [rcx + dst * 8], rax
                    );
                }

                // ── CallMethod ── [dst:2, base:2, nargs:1, prop_const:2, cache:2]
                ROp::CallMethod => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let base = read_u16(instructions, ip + 3) as i32;
                    let nargs_raw = read_u8(instructions, ip + 5);
                    let prop_c = read_u16(instructions, ip + 6) as u64;

                    if let Some((str_reg, i32_reg)) = pending_strcat_fuse.take() {
                        // Fused path with INLINE intern cache probe.
                        // Avoids the full helper call when the intern cache hits.
                        let resolved_sym = intern_const(constants, prop_c as usize).unwrap_or(0);
                        let packed: u64 = (nargs_raw as u64 + 1) | ((resolved_sym as u64) << 32);
                        let sym_set_val = crate::intern::intern("set") as i32;
                        let sym_get_val = crate::intern::intern("get") as i32;
                        let ic_ptr_off = layout.intern_cache_ptr_offset as i32;
                        let _ic_len_off = ic_ptr_off + 8; // Vec.len is 8 bytes after Vec.ptr
                        let cmo_off = layout.cached_map_obj_offset as i32;

                        // NOTE: str_left/i32_right writes to register file are DEFERRED
                        // to the slow path. The inline fast path reads from original regs.

                        // Inline intern cache probe
                        let i32_sig_hi = (I32_SIG >> 48) as i32;
                        dynasm!(ops
                            // Check i32_right is i32
                            ; mov rax, [r12 + i32_reg * 8]
                            ; mov r10, rax
                            ; shr r10, 48
                            ; cmp r10d, i32_sig_hi
                            ; jne >fused_slow
                            // Extract i32 value
                            ; mov r10d, eax

                            // Compute hash: (left_bits ^ (left_bits >> 5)) ^ i32_val & 2047
                            ; mov r11, [r12 + str_reg * 8]  // left_bits
                            ; mov rax, r11
                            ; shr rax, 5
                            ; xor rax, r11
                            ; xor eax, r10d
                            ; and eax, 2047                  // cache index (2048-entry cache = 32KB, fits L1)

                            // Load cache entry: ptr + idx * 16
                            ; mov r9, [r15 + ic_ptr_off]     // intern_cache Vec ptr
                            ; shl eax, 4                     // idx * 16
                            ; add r9, rax                    // &cache[idx]

                            // Check cl == left_bits
                            ; cmp [r9], r11
                            ; jne >fused_slow
                            // Check ci == i32_val
                            ; cmp [r9 + 8], r10d
                            ; jne >fused_slow
                            // Load cs (sym_id)
                            ; mov r10d, [r9 + 12]
                            ; cmp r10d, -1                   // u32::MAX = not cached
                            ; je >fused_slow

                            // Check cached_map_obj matches and entries ptr non-null
                            ; mov rax, [r12 + base * 8]      // obj_bits
                            ; cmp rax, [r15 + cmo_off]
                            ; jne >fused_slow
                            ; cmp QWORD [r15 + layout.cached_map_entries_offset as i32], 0
                            ; je >fused_slow

                            // CACHE HIT! r10d = sym_id.
                        );

                        let cme_off = layout.cached_map_entries_offset as i32;
                        let cmi_off = layout.cached_map_indices_offset as i32;
                        let ft_bp_off = layout.ft_buckets_ptr_off as i32;
                        let ft_mask_off = layout.ft_mask_off as i32;
                        let entry_size = layout.map_entry_size as i32;
                        let entry_val_off = layout.map_entry_value_off as i32;
                        let _sym_helper_addr = crate::vm::djit_map_sym_set_helper as usize as i64;

                        if resolved_sym == sym_get_val as u32 {
                            // FULLY INLINE Map.get — zero function calls, zero indirections!
                            // Uses direct cached data pointers (skip FlatHashTable/Vec derefs).
                            let cbd_off = layout.cached_buckets_data_offset as i32;
                            let ced_off = layout.cached_entries_data_offset as i32;
                            let cmask_off = layout.cached_mask_offset as i32;
                            dynasm!(ops
                                ; imul eax, r10d, 0x9E3779B9u32 as i32
                                ; and eax, [r15 + cmask_off]         // hash & cached mask (1 load)
                                ; mov r9, [r15 + cbd_off]            // cached buckets data ptr (1 load)
                                ; movzx r9d, WORD [r9 + rax * 2]    // slot (1 load)
                                ; test r9d, r9d
                                ; jz >map_get_miss
                                ; dec r9d
                                ; mov r11, [r15 + ced_off]           // cached entries data ptr (1 load)
                            );
                            let hk_val_off_get = layout.hashkey_sym_val_off as i32;
                            if entry_size == 32 {
                                // 32 = power of 2: single shift
                                dynasm!(ops
                                    ; mov rax, r9
                                    ; shl rax, 5
                                    ; cmp DWORD [r11 + rax + hk_val_off_get], r10d
                                    ; jne >fused_slow
                                    ; mov rax, [r11 + rax + entry_val_off]
                                );
                            } else if entry_size == 40 {
                                dynasm!(ops
                                    ; lea rax, [r9 + r9 * 4]
                                    ; shl rax, 3
                                    ; cmp DWORD [r11 + rax + hk_val_off_get], r10d
                                    ; jne >fused_slow
                                    ; mov rax, [r11 + rax + entry_val_off]
                                );
                            } else {
                                dynasm!(ops
                                    ; imul rax, r9, entry_size
                                    ; cmp DWORD [r11 + rax + hk_val_off_get], r10d
                                    ; jne >fused_slow
                                    ; mov rax, [r11 + rax + entry_val_off]
                                );
                            }
                            dynasm!(ops
                                ; mov [rcx + dst * 8], rax
                                ; jmp >fused_done
                                ; map_get_miss:
                                // Empty bucket = key not in map → return undefined
                                ; mov rax, QWORD crate::value::Value::UNDEFINED.bits() as i64
                                ; mov [rcx + dst * 8], rax
                                ; jmp >fused_done
                            );
                        } else if resolved_sym == sym_set_val as u32 {
                            // FULLY INLINE Map.set for new-and-existing keys.
                            // Hot path: runs 2..N of a bench/handler loop all
                            // re-write existing keys — the first probe finds
                            // the same bucket that was populated on run 1, so
                            // we inline the value-update and skip the helper.
                            // Falls to helper only on: resize needed, entries
                            // full, or probe-1 mismatch + probe-2..4 collision.
                            let ft_cnt_off = layout.ft_count_off as i32;
                            let hk_disc = layout.hashkey_sym_disc as i32;
                            let hk_val_off = layout.hashkey_sym_val_off as i32;
                            let ced_off = layout.cached_entries_data_offset as i32;
                            let set_helper = crate::vm::djit_map_sym_set_helper as usize as i64;
                            dynasm!(ops
                                // r10d = sym_id. Hash it.
                                ; imul eax, r10d, 0x9E3779B9u32 as i32
                                ; mov r11, [r15 + cmi_off]       // &FlatHashTable
                                ; and eax, [r11 + ft_mask_off]   // hash & mask = bucket index
                                // Check load factor: count*4 < (mask+1)*3
                                ; mov r9d, [r11 + ft_cnt_off]    // count
                                ; shl r9d, 2                     // count * 4
                                ; mov edx, [r11 + ft_mask_off]   // mask (use edx as scratch)
                                ; inc edx                        // mask + 1
                                ; imul edx, edx, 3               // (mask+1) * 3
                                ; cmp r9d, edx
                                ; jge >iset_slow                 // need resize → helper
                                // Linear probe (4 unrolled). Probe 1 also
                                // checks sym-match (update fast path).
                                ; mov r9, [r11 + ft_bp_off]      // buckets.ptr
                                ; mov r11d, [r11 + ft_mask_off]  // mask
                                // Probe 1: check empty OR sym match
                                ; movzx r8d, WORD [r9 + rax * 2]
                                ; test r8d, r8d
                                ; jz >iset_found                 // empty → insert
                                ; dec r8d                        // 0-based entry idx
                                ; mov rdx, [r15 + ced_off]       // entries data ptr
                            );
                            if entry_size == 32 {
                                dynasm!(ops ; shl r8, 5);
                            } else if entry_size == 40 {
                                dynasm!(ops ; lea r8, [r8 + r8 * 4] ; shl r8, 3);
                            } else {
                                dynasm!(ops ; imul r8, r8, entry_size);
                            }
                            dynasm!(ops
                                ; cmp DWORD [rdx + r8 + hk_val_off], r10d
                                ; je >iset_update
                                // Probe 2: empty-only (collision)
                                ; lea eax, [eax + 1]
                                ; and eax, r11d
                                ; cmp WORD [r9 + rax * 2], 0
                                ; je >iset_found
                                ; lea eax, [eax + 1]
                                ; and eax, r11d
                                ; cmp WORD [r9 + rax * 2], 0
                                ; je >iset_found
                                ; lea eax, [eax + 1]
                                ; and eax, r11d
                                ; cmp WORD [r9 + rax * 2], 0
                                ; je >iset_found
                                ; jmp >iset_slow
                                ; iset_update:
                                // r8 = entry offset (already scaled), rdx = entries data ptr.
                                // Write new value and return the map object.
                                ; mov rax, [r12 + (base + 3) * 8]
                                ; mov [rdx + r8 + entry_val_off], rax
                                ; mov rax, [r12 + base * 8]
                                ; mov rcx, r12
                                ; mov rdx, r13
                                ; mov [rcx + dst * 8], rax
                                ; jmp >fused_done
                                ; iset_found:
                                // rax = empty bucket index, r9 = buckets.ptr
                                // Check entries capacity
                                ; mov r11, [r15 + cme_off]       // &Vec<entries>
                                ; mov edx, [r11 + 16]            // entries.len
                                ; cmp edx, [r11]                 // len < cap?
                                ; jge >iset_slow
                                // Write entry at entries.ptr + len * entry_size
                                ; push rax                       // save bucket index
                                ; push rdx                       // save entries.len
                                ; mov r11, [r11 + 8]             // entries.ptr (r11 reused)
                            );
                            if entry_size == 32 {
                                dynasm!(ops ; mov rax, rdx ; shl rax, 5);
                            } else if entry_size == 40 {
                                dynasm!(ops ; lea rax, [rdx + rdx * 4] ; shl rax, 3);
                            } else {
                                dynasm!(ops ; imul rax, rdx, entry_size);
                            }
                            dynasm!(ops
                                ; add r11, rax                   // &entries[len]
                                ; mov BYTE [r11], hk_disc as i8  // HashKey::Sym disc
                                ; mov DWORD [r11 + hk_val_off], r10d  // sym_id
                                ; mov rax, [r12 + (base + 3) * 8]
                                ; mov [r11 + entry_val_off], rax // value
                                // entries.len++
                                ; pop rdx                        // restore entries.len
                                ; mov r11, [r15 + cme_off]
                                ; lea eax, [edx + 1]
                                ; mov [r11 + 16], eax
                                // Update bucket: buckets[idx] = len + 1 (1-based)
                                ; pop rax                        // restore bucket index
                                ; lea edx, [edx + 1]            // 1-based entry index
                                ; mov WORD [r9 + rax * 2], dx
                                // Update FlatHashTable.count++
                                ; mov r11, [r15 + cmi_off]
                                ; inc DWORD [r11 + ft_cnt_off]
                                // Return map object
                                ; mov rax, [r12 + base * 8]
                                ; mov rcx, r12                   // restore rcx
                                ; mov rdx, r13                   // restore rdx
                                ; mov [rcx + dst * 8], rax
                                ; jmp >fused_done
                                ; iset_slow:
                                // r10d still has sym_id from cache hit
                                ; mov rcx, r15
                                ; mov edx, r10d
                                ; mov r8, [r12 + (base + 3) * 8]
                                ; xor r9d, r9d
                                ; mov rax, QWORD set_helper
                                ; call rax
                                ; mov rcx, r12
                                ; mov rdx, r13
                                ; mov r8, r14
                                ; mov [rcx + dst * 8], rax
                            );
                        } else {
                            dynasm!(ops
                                ; mov rax, QWORD crate::value::Value::UNDEFINED.bits() as i64
                                ; mov [rcx + dst * 8], rax
                            );
                        }
                        dynasm!(ops
                            ; jmp >fused_done
                            ; fused_slow:
                            // Deferred: write str_left/i32_right to register file for helper
                            ; mov rax, [r12 + str_reg * 8]
                            ; mov [r12 + (base + 1) * 8], rax
                            ; mov rax, [r12 + i32_reg * 8]
                            ; mov [r12 + (base + 2) * 8], rax
                        );

                        // Fallback: full fused helper
                        let fused_addr = crate::vm::djit_map_fast_helper as usize as i64;
                        dynasm!(ops
                            ; mov rcx, r15
                            ; mov rdx, [r12 + base * 8]
                            ; lea r8, [r12 + (base + 1) * 8]
                            ; mov r9, QWORD packed as i64
                            ; mov rax, QWORD fused_addr
                            ; call rax
                            ; mov rcx, r12
                            ; mov rdx, r13
                            ; mov r8, r14
                            ; mov [rcx + dst * 8], rax
                            ; fused_done:
                        );
                    } else {
                        // Normal CallMethod path
                        let packed: u64 = (nargs_raw as u64) | (prop_c << 8);
                        let helper_addr = crate::vm::djit_call_method_helper as usize as i64;
                        dynasm!(ops
                            ; mov rcx, r15
                            ; mov rdx, [r12 + base * 8]
                            ; lea r8, [r12 + (base + 1) * 8]
                            ; mov r9, QWORD packed as i64
                            ; mov rax, QWORD helper_addr
                            ; call rax
                            // Reload r12: helper may have resized vm.stack
                            ; mov r12, [r15 + jrp_off]
                            ; mov rcx, r12
                            ; mov rdx, r13
                            ; mov r8, r14
                            ; mov QWORD [r15 + co_off], 0
                            ; mov [rcx + dst * 8], rax
                        );
                    }
                }

                // ── Array creation ── [dst:2, base:2, count:2]
                ROp::Array => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let base = read_u16(instructions, ip + 3) as i32;
                    let count = read_u16(instructions, ip + 5) as i32;

                    let helper_addr = crate::vm::djit_array_new_helper as usize as i64;
                    dynasm!(ops
                        ; mov rcx, r15                        // vm_ptr
                        ; lea rdx, [r12 + base * 8]           // items_ptr
                        ; mov r8d, count                      // count
                        ; xor r9d, r9d                        // unused
                        ; mov rax, QWORD helper_addr
                        ; call rax
                        ; mov rcx, r12
                        ; mov rdx, r13
                        ; mov r8, r14
                        ; mov [rcx + dst * 8], rax
                    );
                }

                // ── Index (get) with inline i32-array fast path ──
                // Vec layout on Rust 1.92: [cap:0, ptr:8, len:16]
                ROp::Index => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let obj = read_u16(instructions, ip + 3) as i32;
                    let key = read_u16(instructions, ip + 5) as i32;

                    let helper_addr = crate::vm::djit_get_index_helper as usize as i64;
                    let heap_off = layout.heap_offset as i32;
                    let obj_size = layout.object_size as i32;
                    let arr_disc = layout.array_discriminant as i32;
                    let rc_off = layout.hash_rc_offset as i32;
                    let rcbox_data = layout.rcbox_data_offset as i32;
                    let i32_sig_hi = (I32_SIG >> 48) as i32;
                    let heap_tag_hi = 0x7FFEi32;

                    dynasm!(ops
                        // Check key is i32 and non-negative
                        ; mov rax, [r12 + key * 8]
                        ; mov r9, rax
                        ; shr rax, 48
                        ; cmp eax, i32_sig_hi
                        ; jne >idx_slow
                        ; mov r9d, r9d                        // zero-extend key i32
                        ; test r9d, r9d
                        ; js >idx_slow
                        // Check that obj is heap-tagged. Without this,
                        // `"ABCDEF"[0]` (inline string) would read the
                        // low 32 bits (the chars) as a heap index and
                        // scale by obj_size (32), reading ~35 GiB past
                        // the heap data pointer → SIGSEGV / host crash.
                        ; mov rax, [r12 + obj * 8]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, heap_tag_hi
                        ; jne >idx_slow
                        ; mov eax, eax                        // zero-extend heap_idx to rax
                        ; mov r11, [r15 + heap_off]           // objects Vec ptr (at Vec offset 8)
                    );
                    // Actually: heap is Heap { objects: Vec<Object>, ... }
                    // Vec layout: [cap:0, ptr:8, len:16]
                    // So objects.ptr is at heap_offset + 8
                    let heap_ptr_off = heap_off + 8; // Vec.ptr at offset 8
                    dynasm!(ops
                        ; mov r11, [r15 + heap_ptr_off]       // objects data ptr
                    );
                    if obj_size == 32 {
                        dynasm!(ops ; shl rax, 5);
                    } else {
                        let os = obj_size;
                        dynasm!(ops ; imul eax, eax, os);
                    }
                    dynasm!(ops
                        ; add r11, rax                        // &objects[heap_idx]
                        ; cmp BYTE [r11], arr_disc as i8      // check Array discriminant
                        ; jne >idx_slow
                        // Follow Rc → RcBox → VmCell<Vec<Value>>
                        ; mov r11, [r11 + rc_off]             // Rc ptr → RcBox
                        ; add r11, rcbox_data                 // → VmCell<Vec<Value>>
                        // Vec<Value> layout: [cap:0, ptr:8, len:16]
                        ; cmp r9d, [r11 + 16]                 // key < len?
                        ; jge >idx_slow                       // out of bounds → helper
                        // Direct access: vec.ptr[key]
                        ; mov r11, [r11 + 8]                  // Vec data ptr (offset 8!)
                        ; mov rax, [r11 + r9 * 8]             // arr[key]
                        ; jmp >idx_done
                        ; idx_slow:
                        ; mov rcx, r15
                        ; mov rdx, [r12 + obj * 8]
                        ; mov r8, [r12 + key * 8]
                        ; xor r9d, r9d
                        ; mov rax, QWORD helper_addr
                        ; call rax
                        ; mov rcx, r12
                        ; mov rdx, r13
                        ; mov r8, r14
                        ; idx_done:
                        ; mov [rcx + dst * 8], rax
                    );
                }

                // ── SetIndex with inline i32-array fast path ──
                ROp::SetIndex => {
                    let obj_r = read_u16(instructions, ip + 1) as i32;
                    let key_r = read_u16(instructions, ip + 3) as i32;
                    let val_r = read_u16(instructions, ip + 5) as i32;

                    let helper_addr = crate::vm::djit_set_index_helper as usize as i64;
                    let heap_off = layout.heap_offset as i32;
                    let heap_ptr_off = heap_off + 8; // Vec.ptr at offset 8
                    let obj_size = layout.object_size as i32;
                    let arr_disc = layout.array_discriminant as i32;
                    let rc_off = layout.hash_rc_offset as i32;
                    let rcbox_data = layout.rcbox_data_offset as i32;
                    let i32_sig_hi_si = (I32_SIG >> 48) as i32;
                    let heap_tag_hi_si = 0x7FFEi32;
                    dynasm!(ops
                        // Check key is i32 and non-negative
                        ; mov rax, [r12 + key_r * 8]
                        ; mov r9, rax
                        ; shr rax, 48
                        ; cmp eax, i32_sig_hi_si
                        ; jne >si_slow
                        ; mov r9d, r9d
                        ; test r9d, r9d
                        ; js >si_slow
                        // Check that obj is heap-tagged — without this
                        // a non-heap Value (inline string / number / bool)
                        // in obj register would be treated as a heap
                        // index and read unmapped memory.
                        ; mov rax, [r12 + obj_r * 8]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, heap_tag_hi_si
                        ; jne >si_slow
                        ; mov eax, eax                        // zero-extend heap_idx
                        ; mov r11, [r15 + heap_ptr_off]       // objects data ptr
                    );
                    if obj_size == 32 {
                        dynasm!(ops ; shl rax, 5);
                    } else {
                        let os = obj_size;
                        dynasm!(ops ; imul eax, eax, os);
                    }
                    dynasm!(ops
                        ; add r11, rax
                        ; cmp BYTE [r11], arr_disc as i8
                        ; jne >si_slow
                        ; mov r11, [r11 + rc_off]             // Rc → RcBox
                        ; add r11, rcbox_data                 // → Vec<Value>
                        ; cmp r9d, [r11 + 16]                 // key < len? (len at Vec+16)
                        ; jge >si_slow                        // out of bounds → helper (resize)
                        // Direct write: vec.ptr[key] = val
                        ; mov r11, [r11 + 8]                  // Vec data ptr (at Vec+8)
                        ; mov rax, [r12 + val_r * 8]
                        ; mov [r11 + r9 * 8], rax
                        ; jmp >si_done
                        ; si_slow:
                        ; mov rcx, r15
                        ; mov rdx, [r12 + obj_r * 8]
                        ; mov r8, [r12 + key_r * 8]
                        ; mov r9, [r12 + val_r * 8]
                        ; mov rax, QWORD helper_addr
                        ; call rax
                        ; mov rcx, r12
                        ; mov rdx, r13
                        ; mov r8, r14
                        ; si_done:
                    );
                }

                // ── Hash creation ── [dst:2, base:2, count:2]
                ROp::Hash => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let base = read_u16(instructions, ip + 3) as i32;
                    let count = read_u16(instructions, ip + 5) as i32;

                    let helper_addr = crate::vm::djit_hash_new_helper as usize as i64;
                    // Pack count and base into a single u64 for the info param
                    let info: u64 = (count as u64) | ((base as u64) << 32);
                    dynasm!(ops
                        ; mov rcx, r15                        // vm_ptr
                        ; mov rdx, r12                        // items_ptr (regs base)
                        ; mov r8, QWORD info as i64           // packed count|base (direct value)
                        ; mov r9, r13                         // consts_ptr
                        ; mov rax, QWORD helper_addr
                        ; call rax
                        ; mov rcx, r12
                        ; mov rdx, r13
                        ; mov r8, r14
                        ; mov [rcx + dst * 8], rax
                    );
                }

                // ── GetProp with inline cache ──
                ROp::GetProp => {
                    let dst = read_u16(instructions, ip + 1) as i32;
                    let obj = read_u16(instructions, ip + 3) as i32;
                    let prop_c = read_u16(instructions, ip + 5) as usize;
                    let cache_slot = read_u16(instructions, ip + 7) as i32;
                    let sym = intern_const(constants, prop_c).unwrap_or(0) as u64;
                    let packed = sym | ((cache_slot as u64) << 32);

                    let helper_addr = crate::vm::djit_get_prop_sym_helper as usize as i64;
                    let co_off = layout.cached_obj_offset as i32;
                    let cv_off = layout.cached_values_offset as i32;
                    let cs_off = layout.cached_shape_offset as i32;
                    let ic_ptr_off = (layout.ic_offset as i32) + 8;

                    let fixed_slot = resolve_prop_slot(instructions, constants, sym as u32);

                    if last_verified_obj != Some(obj) {
                        dynasm!(ops
                            ; mov rax, [r12 + obj * 8]
                            ; cmp rax, [r15 + co_off]
                            ; jne >gp_miss
                        );
                        if use_rdi_cache {
                            dynasm!(ops ; mov rdi, [r15 + cv_off]);
                        }
                    }
                    last_verified_obj = Some(obj);
                    if let Some(slot) = fixed_slot {
                        let slot_i32 = (slot * 8) as i32;
                        if use_rdi_cache {
                            dynasm!(ops ; mov rax, [rdi + slot_i32]);
                        } else {
                            dynasm!(ops
                                ; mov r11, [r15 + cv_off]
                                ; mov rax, [r11 + slot_i32]
                            );
                        }
                    } else {
                        dynasm!(ops
                            ; mov rax, [r15 + ic_ptr_off]
                            ; mov r11d, [rax + cache_slot * 8]
                            ; cmp r11d, [r15 + cs_off]
                            ; jne >gp_miss
                            ; mov eax, [rax + cache_slot * 8 + 4]
                        );
                        if use_rdi_cache {
                            dynasm!(ops ; mov rax, [rdi + rax * 8]);
                        } else {
                            dynasm!(ops
                                ; mov r11, [r15 + cv_off]
                                ; mov rax, [r11 + rax * 8]
                            );
                        }
                    }
                    dynasm!(ops
                        ; jmp >gp_done
                        ; gp_miss:
                        ; mov rcx, r15
                        ; mov rdx, [r12 + obj * 8]
                        ; mov r8, QWORD packed as i64
                        ; xor r9d, r9d
                        ; mov rax, QWORD helper_addr
                        ; call rax
                        ; mov rcx, r12
                        ; mov rdx, r13
                        ; mov r8, r14
                    );
                    if use_rdi_cache {
                        dynasm!(ops ; mov rdi, [r15 + cv_off]);
                    }
                    dynasm!(ops
                        ; gp_done:
                        ; mov [rcx + dst * 8], rax
                    );
                }

                // ── SetProp with inline cache ──
                ROp::SetProp => {
                    let obj_r = read_u16(instructions, ip + 1) as i32;
                    let prop_c = read_u16(instructions, ip + 3) as usize;
                    let src_r = read_u16(instructions, ip + 5) as i32;
                    let cache_slot = read_u16(instructions, ip + 7) as i32;
                    let sym = intern_const(constants, prop_c).unwrap_or(0) as u64;
                    let packed = sym | ((cache_slot as u64) << 32);

                    let helper_addr = crate::vm::djit_set_prop_sym_helper as usize as i64;
                    let co_off = layout.cached_obj_offset as i32;
                    let _cb_off = layout.cached_borrow_offset as i32;
                    let ic_ptr_off = (layout.ic_offset as i32) + 8;
                    let cv_off = layout.cached_values_offset as i32;
                    let cs_off = layout.cached_shape_offset as i32;
                    let _dirty_off = layout.pairs_dirty_offset as i32;

                    let fixed_slot = resolve_prop_slot(instructions, constants, sym as u32);

                    if last_verified_obj != Some(obj_r) {
                        dynasm!(ops
                            ; mov rax, [r12 + obj_r * 8]
                            ; cmp rax, [r15 + co_off]
                            ; jne >sp_miss
                        );
                        if use_rdi_cache {
                            dynasm!(ops ; mov rdi, [r15 + cv_off]);
                        }
                    }
                    last_verified_obj = Some(obj_r);
                    if let Some(slot) = fixed_slot {
                        // Compile-time resolved slot: direct write
                        let slot_i32 = (slot * 8) as i32;
                        if use_rdi_cache {
                            dynasm!(ops
                                ; mov r9, [r12 + src_r * 8]
                                ; mov [rdi + slot_i32], r9
                            );
                        } else {
                            dynasm!(ops
                                ; mov r11, [r15 + cv_off]
                                ; mov r9, [r12 + src_r * 8]
                                ; mov [r11 + slot_i32], r9
                            );
                        }
                    } else {
                        dynasm!(ops
                            ; mov rax, [r15 + ic_ptr_off]
                            ; mov r11d, [rax + cache_slot * 8]
                            ; cmp r11d, [r15 + cs_off]
                            ; jne >sp_miss
                            ; mov eax, [rax + cache_slot * 8 + 4]
                        );
                        if use_rdi_cache {
                            dynasm!(ops
                                ; mov r9, [r12 + src_r * 8]
                                ; mov [rdi + rax * 8], r9
                            );
                        } else {
                            dynasm!(ops
                                ; mov r11, [r15 + cv_off]
                                ; mov r9, [r12 + src_r * 8]
                                ; mov [r11 + rax * 8], r9
                            );
                        }
                    }
                    // Set pairs_dirty via helper's cached_hash_borrow pointer.
                    // Only safe when cached_hash_borrow is valid (after a miss that
                    // updates it). Skip for now — the helper already sets pairs_dirty.
                    // TODO: re-enable when cache validity is guaranteed
                    dynasm!(ops
                        ; jmp >sp_done
                        ; sp_miss:
                        ; mov rcx, r15
                        ; mov rdx, [r12 + obj_r * 8]
                        ; mov r8, QWORD packed as i64
                        ; mov r9, [r12 + src_r * 8]
                        ; mov rax, QWORD helper_addr
                        ; call rax
                        ; mov rcx, r12
                        ; mov rdx, r13
                        ; mov r8, r14
                    );
                    if use_rdi_cache {
                        dynasm!(ops ; mov rdi, [r15 + cv_off]);
                    }
                    dynasm!(ops ; sp_done:);
                }

                // ── Fused: obj.prop += const (INLINE CACHE) ──
                // [obj:2, prop_const:2, val_const:2, cache:2]
                ROp::AddConstToRegProp => {
                    let obj_r = read_u16(instructions, ip + 1) as i32;
                    let prop_c = read_u16(instructions, ip + 3) as usize;
                    let val_c = read_u16(instructions, ip + 5) as i32;
                    let cache_slot = read_u16(instructions, ip + 7) as i32;
                    let sym = intern_const(constants, prop_c).unwrap_or(0) as u64;
                    let packed = sym | ((cache_slot as u64) << 32);

                    let helper_addr = crate::vm::djit_add_const_to_prop_helper as usize as i64;
                    let co_off = layout.cached_obj_offset as i32;
                    let cv_off = layout.cached_values_offset as i32;
                    let cs_off = layout.cached_shape_offset as i32;
                    let _cb_off = layout.cached_borrow_offset as i32;
                    let ic_ptr_off = (layout.ic_offset as i32) + 8;
                    let _dirty_off = layout.pairs_dirty_offset as i32;
                    let val_c_imm = const_i32(constants, val_c as usize);

                    // Try to resolve slot index at compile time from Hash opcode
                    let fixed_slot = resolve_prop_slot(instructions, constants, sym as u32);

                    // Skip obj check if previous op already verified same obj_r
                    if last_verified_obj != Some(obj_r) {
                        dynasm!(ops
                            ; mov rax, [r12 + obj_r * 8]
                            ; cmp rax, [r15 + co_off]
                            ; jne >prop_miss
                        );
                        if use_rdi_cache {
                            dynasm!(ops ; mov rdi, [r15 + cv_off]);
                        }
                    }
                    last_verified_obj = Some(obj_r);
                    if let Some(slot) = fixed_slot {
                        // Compile-time resolved slot: skip IC lookup entirely!
                        let slot_i32 = (slot * 8) as i32;
                        if use_rdi_cache {
                            dynasm!(ops ; mov eax, [rdi + slot_i32]);
                        } else {
                            dynasm!(ops
                                ; mov r11, [r15 + cv_off]
                                ; mov eax, [r11 + slot_i32]
                            );
                        }
                    } else {
                        dynasm!(ops
                            ; mov rax, [r15 + ic_ptr_off]
                            ; mov r11d, [rax + cache_slot * 8]
                            ; cmp r11d, [r15 + cs_off]
                            ; jne >prop_miss
                            ; mov eax, [rax + cache_slot * 8 + 4]
                        );
                        if use_rdi_cache {
                            dynasm!(ops ; mov eax, [rdi + rax * 8]);
                        } else {
                            dynasm!(ops
                                ; mov r11, [r15 + cv_off]
                                ; mov eax, [r11 + rax * 8]
                            );
                        }
                    }
                    // Add constant value
                    if let Some(cv) = val_c_imm {
                        dynasm!(ops ; add eax, cv);
                    } else {
                        dynasm!(ops ; add eax, [r13 + val_c * 8]);
                    }
                    // Re-box and store (64-bit to avoid store-forwarding stalls)
                    if let Some(slot) = fixed_slot {
                        let slot_i32 = (slot * 8) as i32;
                        if use_rbx_in_calls {
                            dynasm!(ops
                                ; or rax, rbx
                                ; mov [rdi + slot_i32], rax
                            );
                        } else if use_rdi_cache {
                            if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                            dynasm!(ops ; or rax, rbx ; mov [rdi + slot_i32], rax);
                        } else {
                            if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                            dynasm!(ops
                                ; or rax, rbx
                                ; mov r11, [r15 + cv_off]
                                ; mov [r11 + slot_i32], rax
                            );
                        }
                    } else {
                        dynasm!(ops
                            ; mov r9d, eax
                            ; mov rax, [r15 + ic_ptr_off]
                            ; mov eax, [rax + cache_slot * 8 + 4]
                        );
                        if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                        dynasm!(ops ; or r9, rbx);
                        if use_rdi_cache {
                            dynasm!(ops ; mov [rdi + rax * 8], r9);
                        } else {
                            dynasm!(ops
                                ; mov r11, [r15 + cv_off]
                                ; mov [r11 + rax * 8], r9
                            );
                        }
                    }
                    // pairs_dirty set by helper on miss path
                    dynasm!(ops
                        ; jmp >prop_done
                        ; prop_miss:
                        ; mov rcx, r15
                        ; mov rdx, [r12 + obj_r * 8]
                        ; mov r8, QWORD packed as i64
                        ; mov r9, [r13 + val_c * 8]
                        ; mov rax, QWORD helper_addr
                        ; call rax
                        ; mov rcx, r12
                        ; mov rdx, r13
                        ; mov r8, r14
                    );
                    if use_rdi_cache {
                        dynasm!(ops ; mov rdi, [r15 + cv_off]);
                    }
                    dynasm!(ops ; prop_done:);
                }

                // ── Fused: obj.dst = obj.s1 + obj.s2 ──
                // [obj:2, s1_prop:2, s1_cache:2, s2_prop:2, s2_cache:2, dst_prop:2, dst_cache:2]
                ROp::AddRegPropsToRegProp => {
                    let obj_r = read_u16(instructions, ip + 1) as i32;
                    let s1_prop = read_u16(instructions, ip + 3) as usize;
                    let s1_cache = read_u16(instructions, ip + 5) as i32;
                    let s2_prop = read_u16(instructions, ip + 7) as usize;
                    let s2_cache = read_u16(instructions, ip + 9) as i32;
                    let dst_prop = read_u16(instructions, ip + 11) as usize;
                    let dst_cache = read_u16(instructions, ip + 13) as i32;
                    let s1_sym = intern_const(constants, s1_prop).unwrap_or(0) as u64;
                    let s2_sym = intern_const(constants, s2_prop).unwrap_or(0) as u64;
                    let dst_sym = intern_const(constants, dst_prop).unwrap_or(0) as u64;
                    let packed: u64 = s1_sym | (s2_sym << 16) | (dst_sym << 32);

                    let helper_addr = crate::vm::djit_add_props_to_prop_helper as usize as i64;
                    let co_off = layout.cached_obj_offset as i32;
                    let _cb_off = layout.cached_borrow_offset as i32;
                    let ic_ptr_off = (layout.ic_offset as i32) + 8;
                    let _dirty_off = layout.pairs_dirty_offset as i32;

                    let cv_off = layout.cached_values_offset as i32;
                    let cs_off = layout.cached_shape_offset as i32;
                    let s1_fixed = resolve_prop_slot(instructions, constants, s1_sym as u32);
                    let s2_fixed = resolve_prop_slot(instructions, constants, s2_sym as u32);
                    let dst_fixed = resolve_prop_slot(instructions, constants, dst_sym as u32);
                    if last_verified_obj != Some(obj_r) {
                        dynasm!(ops
                            ; mov rax, [r12 + obj_r * 8]
                            ; cmp rax, [r15 + co_off]
                            ; jne >arp_miss
                        );
                        if use_rdi_cache {
                            dynasm!(ops ; mov rdi, [r15 + cv_off]);
                        }
                    }
                    last_verified_obj = Some(obj_r);
                    if let (Some(s1s), Some(s2s), Some(ds)) = (s1_fixed, s2_fixed, dst_fixed) {
                        // All 3 slots resolved at compile time!
                        let s1_off = (s1s * 8) as i32;
                        let s2_off = (s2s * 8) as i32;
                        let ds_off = (ds * 8) as i32;
                        if use_rdi_cache && use_rbx_in_calls {
                            dynasm!(ops
                                ; mov eax, [rdi + s1_off]
                                ; add eax, [rdi + s2_off]
                                ; or rax, rbx
                                ; mov [rdi + ds_off], rax
                            );
                        } else if use_rdi_cache {
                            dynasm!(ops
                                ; mov eax, [rdi + s1_off]
                                ; add eax, [rdi + s2_off]
                            );
                            if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                            dynasm!(ops ; or rax, rbx ; mov [rdi + ds_off], rax);
                        } else {
                            dynasm!(ops
                                ; mov r11, [r15 + cv_off]
                                ; mov eax, [r11 + s1_off]
                                ; add eax, [r11 + s2_off]
                            );
                            if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                            dynasm!(ops ; or rax, rbx ; mov [r11 + ds_off], rax);
                        }
                    } else {
                        dynasm!(ops
                            ; mov rax, [r15 + ic_ptr_off]
                            ; mov r11d, [rax + s1_cache * 8]
                            ; cmp r11d, [r15 + cs_off]
                            ; jne >arp_miss
                        );
                        if use_rdi_cache {
                            dynasm!(ops
                                ; mov eax, [rax + s1_cache * 8 + 4]
                                ; mov eax, [rdi + rax * 8]
                                ; mov r9, [r15 + ic_ptr_off]
                                ; mov r9d, [r9 + s2_cache * 8 + 4]
                                ; add eax, [rdi + r9 * 8]
                            );
                            if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                            dynasm!(ops
                                ; or rax, rbx
                                ; mov r9, [r15 + ic_ptr_off]
                                ; mov r9d, [r9 + dst_cache * 8 + 4]
                                ; mov [rdi + r9 * 8], rax
                            );
                        } else {
                            dynasm!(ops
                                ; mov r11, [r15 + cv_off]
                                ; mov eax, [rax + s1_cache * 8 + 4]
                                ; mov eax, [r11 + rax * 8]
                                ; mov r9, [r15 + ic_ptr_off]
                                ; mov r9d, [r9 + s2_cache * 8 + 4]
                                ; add eax, [r11 + r9 * 8]
                            );
                            if !rbx_is_sig { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64); }
                            dynasm!(ops
                                ; or rax, rbx
                                ; mov r9, [r15 + ic_ptr_off]
                                ; mov r9d, [r9 + dst_cache * 8 + 4]
                                ; mov [r11 + r9 * 8], rax
                            );
                        }
                    }
                    // pairs_dirty set by helper on miss path
                    dynasm!(ops
                        ; jmp >arp_done
                        ; arp_miss:
                        ; mov rcx, r15
                        ; mov rdx, [r12 + obj_r * 8]
                        ; xor r8d, r8d
                        ; mov r9, QWORD packed as i64
                        ; mov rax, QWORD helper_addr
                        ; call rax
                        ; mov rcx, r12
                        ; mov rdx, r13
                        ; mov r8, r14
                    );
                    if use_rdi_cache {
                        dynasm!(ops ; mov rdi, [r15 + cv_off]);
                    }
                    dynasm!(ops ; arp_done:);
                }

                // ── Return ──
                ROp::Return | ROp::HaltValue => {
                    let src = read_u16(instructions, ip + 1) as i32;
                    // If returning a pinned register, re-box from CPU reg
                    if pin0 == Some(src) {
                        if rbx_is_sig { dynasm!(ops ; mov eax, esi ; or rax, rbx); }
                        else { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64 ; mov eax, esi ; or rax, rbx); }
                    } else if pin1 == Some(src) {
                        if rbx_is_sig { dynasm!(ops ; mov eax, edi ; or rax, rbx); }
                        else { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64 ; mov eax, edi ; or rax, rbx); }
                    } else if pin2 == Some(src) {
                        if rbx_is_sig { dynasm!(ops ; mov eax, r10d ; or rax, rbx); }
                        else { dynasm!(ops ; mov rbx, QWORD I32_SIG as i64 ; mov eax, r10d ; or rax, rbx); }
                    } else {
                        dynasm!(ops ; mov rax, [rcx + src * 8]);
                    }
                    if has_calls {
                        if use_rdi_cache {
                            dynasm!(ops ; add rsp, 40 ; pop rdi ; pop r12 ; ret);
                        } else {
                            dynasm!(ops ; add rsp, 32 ; pop r12 ; ret);
                        }
                    } else if use_rbx_sig {
                        // Restore non-volatile registers in reverse push order
                        dynasm!(ops ; pop rbx);
                        if pin1.is_some() { dynasm!(ops ; pop rdi); }
                        if pin0.is_some() { dynasm!(ops ; pop rsi); }
                        dynasm!(ops ; ret);
                    } else {
                        dynasm!(ops ; ret);
                    }
                }
                ROp::ReturnUndef | ROp::Halt => {
                    dynasm!(ops ; mov rax, QWORD VAL_UNDEFINED as i64);
                    if has_calls {
                        if use_rdi_cache {
                            dynasm!(ops ; add rsp, 40 ; pop rdi ; pop r12 ; ret);
                        } else {
                            dynasm!(ops ; add rsp, 32 ; pop r12 ; ret);
                        }
                    } else if use_rbx_sig {
                        dynasm!(ops ; pop rbx);
                        if pin1.is_some() { dynasm!(ops ; pop rdi); }
                        if pin0.is_some() { dynasm!(ops ; pop rsi); }
                        dynasm!(ops ; ret);
                    } else {
                        dynasm!(ops ; ret);
                    }
                }

                _ => return false,
            }

            ip += op.size();
        }

        // Safety return at end
        dynasm!(ops ; =>labels[instructions.len()]);
        dynasm!(ops ; mov rax, QWORD VAL_UNDEFINED as i64);
        if has_calls {
            if use_rdi_cache {
                dynasm!(ops ; add rsp, 40 ; pop rdi ; pop r12 ; ret);
            } else {
                dynasm!(ops ; add rsp, 32 ; pop r12 ; ret);
            }
        } else if use_rbx_sig {
            dynasm!(ops ; pop rbx ; ret);
        } else {
            dynasm!(ops ; ret);
        }

        match ops.finalize() {
            Ok(buf) => {
                self.compiled.insert(func_key, DjitFunction { buffer: buf, has_calls });
                true
            }
            Err(_) => false,
        }
    }
}
