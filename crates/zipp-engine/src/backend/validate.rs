//! Post-compile bytecode validator.
//!
//! The register VM's hot-path dispatch relies on a handful of
//! invariants it does **not** check at runtime:
//!
//! 1. Every opcode byte decodes to a valid [`ROp`].
//! 2. Every instruction fits entirely within its instruction buffer
//!    — `ip + size <= len`.
//! 3. Each buffer ends in a terminator (`Halt` / `HaltValue` /
//!    `Return` / `ReturnUndef`).
//! 4. Every jump target lands on an instruction boundary.
//!
//! Today only `RCompiler` produces [`Bytecode`], so the invariants
//! hold by construction. This validator is defence in depth: a future
//! regression in the compiler that emits a truncated instruction or a
//! bad jump target becomes a clean `ZippError::Compile` at load
//! time instead of silent undefined behaviour at dispatch time.
//!
//! Runs once, when [`crate::engine::ZippEngine`] caches compiled
//! bytecode — cold path, budget-negligible.
//!
//! ## Recursion
//!
//! Function bodies compile to [`crate::object::CompiledFunctionObject`]s
//! whose `instructions` live in the parent bytecode's constants table.
//! Classes embed their constructor, methods, getters and setters the
//! same way. [`validate`] recurses through every such nested
//! instruction buffer so the invariants above hold for the entire
//! code graph a compilation produces, not just the top-level.
//!
//! ## What isn't validated
//!
//! Per-operand register / global index bounds **are** now checked
//! for the high-traffic opcodes (Move, arithmetic, comparisons,
//! Get/SetGlobal, Call, CallMethod, the various TestN*Jump fused
//! ops, etc.) — these are the ones whose runtime dispatch trusts
//! the operand without re-checking. Cache-slot operands are also
//! bounds-checked against `Bytecode::num_cache_slots`. Operands on
//! rarely-used opcodes still rely on the runtime panic path; adding
//! them is mechanical and tracked as future work but is less
//! pressing because those opcodes either don't appear in compiler
//! output today or already perform their own runtime bounds check.

use std::collections::HashSet;
use std::rc::Rc;

use crate::backend::bytecode::Bytecode;
use crate::object::{ClassObject, CompiledFunctionObject, Object};
use crate::rcode::ROp;

/// Validate a freshly-compiled [`Bytecode`] and every function body
/// reachable from its constants table. Returns a descriptive
/// `Err(String)` the engine surfaces as
/// [`crate::error::ZippError::Compile`].
pub fn validate(bc: &Bytecode) -> Result<(), String> {
    // Track every instructions `Rc` we've already walked. The compiler
    // doesn't emit self-referential function bodies but a future bug
    // could, and an infinite-loop validator would be a denial-of-
    // service channel all on its own.
    let mut visited: HashSet<*const Vec<u8>> = HashSet::new();
    let limits = OperandLimits {
        register_count: bc.register_count,
        num_cache_slots: bc.num_cache_slots,
    };
    validate_buffer_with_context(
        &bc.instructions,
        &bc.constants,
        &limits,
        "program",
        &mut visited,
    )?;
    validate_constants(&bc.constants, &mut visited)
}

/// A `Bytecode` that has been checked by [`validate`]. The wrapping
/// constructor is the only way to mint one outside of this crate, so
/// any public VM constructor that requires a `ValidatedBytecode`
/// cannot be tricked into running unchecked bytecode by an embedder.
///
/// Internal engine code that has *already* validated a buffer and
/// only needs the wrapper for the type-system contract uses
/// [`ValidatedBytecode::new_unchecked`], which is `pub(crate)` and
/// stays inside the validation boundary.
pub struct ValidatedBytecode(Bytecode);

impl ValidatedBytecode {
    /// Validate `bc` and, on success, take ownership of it as a
    /// `ValidatedBytecode`. This is the public entry point — every
    /// path that reaches a VM constructor must have gone through here
    /// (or [`Self::new_unchecked`] from inside the crate).
    pub fn new(bc: Bytecode) -> Result<Self, String> {
        validate(&bc)?;
        Ok(Self(bc))
    }

    /// Wrap a `Bytecode` without re-validating. Reserved for engine
    /// internals that have just called [`validate`] in the same
    /// function and only need the marker type to talk to VM
    /// constructors. Marked `pub(crate)` so external callers cannot
    /// reach for it as an "unsafe but convenient" escape hatch.
    pub(crate) fn new_unchecked(bc: Bytecode) -> Self {
        Self(bc)
    }

    /// Borrow the inner bytecode for read-only inspection (e.g. to
    /// peek at `register_count` before construction).
    pub fn as_bytecode(&self) -> &Bytecode {
        &self.0
    }

    /// Consume the wrapper and return the underlying bytecode. The
    /// caller takes responsibility for keeping any further use behind
    /// a `ValidatedBytecode` boundary.
    pub fn into_inner(self) -> Bytecode {
        self.0
    }
}

/// Per-buffer operand bounds. The compiler tracks these on
/// `Bytecode` and on each `CompiledFunctionObject`, so the validator
/// can reject a bytecode buffer whose register / cache-slot index
/// would index past the allocated frame at runtime.
#[derive(Clone, Copy)]
struct OperandLimits {
    register_count: u16,
    num_cache_slots: u16,
}

fn validate_buffer_with_context(
    inst: &[u8],
    constants: &[Object],
    limits: &OperandLimits,
    ctx: &str,
    _visited: &mut HashSet<*const Vec<u8>>,
) -> Result<(), String> {
    validate_single_buffer(inst, constants, limits).map_err(|e| format!("{ctx}: {e}"))
}

fn validate_single_buffer(
    inst: &[u8],
    constants: &[Object],
    limits: &OperandLimits,
) -> Result<(), String> {
    if inst.is_empty() {
        return Err("bytecode is empty (no terminator)".to_string());
    }

    // Walk forward, decoding each opcode and advancing by its declared
    // size. Any byte that doesn't decode, or any instruction that
    // would run off the end of the buffer, is an immediate error.
    // We also remember the last opcode we saw — that's the terminator
    // candidate (reading `inst.last()` would give an *operand* byte).
    let mut ip = 0usize;
    let mut boundaries: HashSet<usize> = HashSet::new();
    let mut last_op: Option<ROp> = None;
    while ip < inst.len() {
        boundaries.insert(ip);
        let byte = inst[ip];
        let op = ROp::from_byte(byte).ok_or_else(|| {
            format!("invalid opcode {byte:#04x} at ip {ip} (no such ROp variant)")
        })?;
        let size = instruction_size(inst, ip, op)?;
        if ip + size > inst.len() {
            return Err(format!(
                "truncated instruction at ip {ip}: opcode {op:?} needs {size} bytes, \
                 only {} available",
                inst.len() - ip
            ));
        }
        // Per-opcode operand sanity check for the ops where a bad
        // operand would cause a panic in the interpreter (index past
        // `constants`). This is defence in depth — the compiler
        // doesn't emit these today, but a future regression would
        // become a clean load-time error instead of a runtime panic.
        validate_operand_indices(inst, ip, op, constants)?;
        validate_register_operands(inst, ip, op, limits)?;
        last_op = Some(op);
        ip += size;
    }
    boundaries.insert(ip);

    // Terminator check. `ensure_terminated_instructions` appends a
    // fallback Halt when the VM loads bytecode, but validating here
    // means a future bug that skips that pass is caught at load.
    match last_op.expect("loop ran at least once on non-empty bytecode") {
        ROp::Halt | ROp::HaltValue | ROp::Return | ROp::ReturnUndef => {}
        other => {
            return Err(format!(
                "bytecode does not terminate with Halt/Return (last opcode: {other:?})"
            ));
        }
    }

    // Jump-target boundary check. Every direct jump must land on an
    // instruction boundary we just recorded. Catches compiler bugs
    // that patch the wrong site or compute a mid-instruction target.
    validate_jump_targets(inst, &boundaries)
}

/// Operand-bounds check for opcodes whose operands index into the
/// constants table.  A bad index would panic the host (`panic = abort`
/// in release strips the script context). Today the compiler is the
/// only producer and never emits a bad index — this is defence in
/// depth so a future compiler regression becomes a clean
/// `ZippError::Compile` at load time, not a runtime crash.
///
/// We only validate **constants-table indices** here. Register and
/// global indices are checked at the destination (`Heap::get`,
/// `globals.set_unchecked` is paired with a length check on each
/// global-table grow), so a stray register index can panic but
/// cannot escape memory safety.
fn validate_operand_indices(
    inst: &[u8],
    ip: usize,
    op: ROp,
    constants: &[Object],
) -> Result<(), String> {
    // Helper: read a u16 operand at `ip + offset` and return it as usize.
    // `instruction_size` already proved the instruction fits, so direct
    // indexing is safe.
    let read_u16 = |offset: usize| -> usize {
        u16::from_be_bytes([inst[ip + offset], inst[ip + offset + 1]]) as usize
    };
    let check_const = |const_idx: usize, label: &str| -> Result<(), String> {
        if const_idx >= constants.len() {
            return Err(format!(
                "{label} at ip {ip}: const_idx {const_idx} out of range \
                 (constants.len() = {})",
                constants.len()
            ));
        }
        Ok(())
    };

    match op {
        // [dst:2, const_idx:2]
        ROp::LoadConst => check_const(read_u16(3), "LoadConst")?,
        // [dst:2, const_idx:2, count:1, ...] — also requires the constant
        // be a CompiledFunction so MakeClosure can't grab a string.
        ROp::MakeClosure => {
            let const_idx = read_u16(3);
            check_const(const_idx, "MakeClosure")?;
            if !matches!(constants[const_idx], Object::CompiledFunction(_)) {
                return Err(format!(
                    "MakeClosure at ip {ip}: constant[{const_idx}] is not a CompiledFunction"
                ));
            }
        }
        // [dst:2, src:2, const_idx:2]
        ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst => {
            check_const(read_u16(5), "Reg+Const arith")?
        }
        // [r:2, const_idx:2, target:4]
        ROp::TestLtConstJump
        | ROp::TestLeConstJump
        | ROp::IncrementRegAndJump => check_const(read_u16(3), "Test*Const/IncJump")?,
        // [r:2, mod_const:2, cmp_const:2, target:4]
        ROp::ModRegConstStrictEqConstJump => {
            check_const(read_u16(3), "ModRegConst (mod)")?;
            check_const(read_u16(5), "ModRegConst (cmp)")?;
        }
        // [a:2, b:2, cmp_const:2, target:4]
        ROp::TestModRegStrictEqConstJump => check_const(read_u16(5), "TestModRegStrictEqConst")?,
        // GetProp / GetGlobalProp:  [dst:2, obj:2, prop_const:2, cache:2]
        //                                       ↑ ip+5
        // SetProp / SetGlobalProp:  [obj:2, prop_const:2, src:2, cache:2]
        //                                  ↑ ip+3
        // The two layouts are *not* the same — `dst` only appears on the
        // get path. Reading the wrong offset rejected legitimate bytecode
        // when this validator was first wired up.
        ROp::GetProp | ROp::GetGlobalProp => check_const(read_u16(5), "GetProp prop_const")?,
        ROp::SetProp | ROp::SetGlobalProp => check_const(read_u16(3), "SetProp prop_const")?,
        // [obj:2, prop_const:2, val_const:2, cache:2]
        ROp::AddConstToRegProp => {
            check_const(read_u16(3), "AddConstToRegProp prop_const")?;
            check_const(read_u16(5), "AddConstToRegProp val_const")?;
        }
        // [obj:2, s1p:2, s1c:2, s2p:2, s2c:2, dp:2, dc:2]
        ROp::AddRegPropsToRegProp => {
            check_const(read_u16(3), "AddRegPropsToRegProp s1_prop")?;
            check_const(read_u16(7), "AddRegPropsToRegProp s2_prop")?;
            check_const(read_u16(11), "AddRegPropsToRegProp dst_prop")?;
        }
        // [dst:2, base:2, nargs:1, prop_const:2, cache:2]
        ROp::CallMethod => check_const(read_u16(6), "CallMethod prop_const")?,
        // [hash:2, func:2, prop_const:2, kind:1]
        ROp::DefineAccessor => check_const(read_u16(5), "DefineAccessor prop_const")?,
        _ => {}
    }
    Ok(())
}

/// Per-opcode register / global / cache-slot bounds check.
///
/// All three operand classes are validated against the `Bytecode`'s
/// declared limits. Round-9 fixed the long-standing
/// `next_temp = base + …` undercount in the compiler so
/// `register_count` now reflects the true high-water mark; a register
/// operand that exceeds `limits.register_count` is therefore a compiler
/// regression and surfaces as a clean `ZippError::Compile` here.
fn validate_register_operands(
    inst: &[u8],
    ip: usize,
    op: ROp,
    limits: &OperandLimits,
) -> Result<(), String> {
    let read_u16 = |offset: usize| -> usize {
        u16::from_be_bytes([inst[ip + offset], inst[ip + offset + 1]]) as usize
    };
    let read_u8 = |offset: usize| -> usize { inst[ip + offset] as usize };
    let max_reg = limits.register_count as usize;
    let max_cache = limits.num_cache_slots as usize;
    let max_global = crate::vm::GLOBALS_SIZE;

    let check_reg = |idx: usize, label: &str| -> Result<(), String> {
        if idx >= max_reg {
            return Err(format!(
                "{label} at ip {ip}: register {idx} out of range (count = {max_reg})"
            ));
        }
        Ok(())
    };
    let check_global = |idx: usize, label: &str| -> Result<(), String> {
        if idx >= max_global {
            return Err(format!(
                "{label} at ip {ip}: global slot {idx} out of range (max = {max_global})"
            ));
        }
        Ok(())
    };
    let check_cache = |slot: usize, label: &str| -> Result<(), String> {
        if slot >= max_cache {
            return Err(format!(
                "{label} at ip {ip}: cache slot {slot} out of range (count = {max_cache})"
            ));
        }
        Ok(())
    };

    // Many opcodes encode a `base` register followed by `nargs` (a u8)
    // or a `count` (a u16) where the operand window spans
    // `[base, base + nargs)`. Verify the entire window stays in range.
    let check_reg_window = |base: usize, count: usize, label: &str| -> Result<(), String> {
        let end = base.saturating_add(count);
        if end > max_reg {
            return Err(format!(
                "{label} at ip {ip}: register window [{base}..{end}) \
                 exceeds count = {max_reg}"
            ));
        }
        Ok(())
    };

    match op {
        // [dst:2, global_idx:2]
        ROp::GetGlobal => {
            check_reg(read_u16(1), "GetGlobal dst")?;
            check_global(read_u16(3), "GetGlobal global")?;
        }
        // [global_idx:2, src:2]
        ROp::SetGlobal => {
            check_global(read_u16(1), "SetGlobal global")?;
            check_reg(read_u16(3), "SetGlobal src")?;
        }
        // [dst:2, base:2, nargs:1, prop_const:2, cache:2]
        ROp::CallMethod => {
            check_reg(read_u16(1), "CallMethod dst")?;
            check_reg_window(read_u16(3), read_u8(5) + 1, "CallMethod args")?;
            check_cache(read_u16(8), "CallMethod cache")?;
        }
        // [dst:2, obj:2, prop_const:2, cache:2]
        ROp::GetProp => {
            check_reg(read_u16(1), "GetProp dst")?;
            check_reg(read_u16(3), "GetProp obj")?;
            check_cache(read_u16(7), "GetProp cache")?;
        }
        // [dst:2, global_idx:2, prop_const:2, cache:2]
        ROp::GetGlobalProp => {
            check_reg(read_u16(1), "GetGlobalProp dst")?;
            check_global(read_u16(3), "GetGlobalProp global")?;
            check_cache(read_u16(7), "GetGlobalProp cache")?;
        }
        // [obj:2, prop_const:2, src:2, cache:2]
        ROp::SetProp => {
            check_reg(read_u16(1), "SetProp obj")?;
            check_reg(read_u16(5), "SetProp src")?;
            check_cache(read_u16(7), "SetProp cache")?;
        }
        // [global_idx:2, prop_const:2, src:2, cache:2]
        ROp::SetGlobalProp => {
            check_global(read_u16(1), "SetGlobalProp global")?;
            check_reg(read_u16(5), "SetGlobalProp src")?;
            check_cache(read_u16(7), "SetGlobalProp cache")?;
        }
        // [dst:2, src:2]
        ROp::Move
        | ROp::Neg
        | ROp::Not
        | ROp::UnaryPlus
        | ROp::Typeof
        | ROp::IsNullish
        | ROp::Await => {
            check_reg(read_u16(1), "unary dst")?;
            check_reg(read_u16(3), "unary src")?;
        }
        // [dst:2, idx:2] — idx is a constant slot, verified by validate_operand_indices
        ROp::LoadConst => check_reg(read_u16(1), "LoadConst dst")?,
        // [dst:2]
        ROp::LoadTrue
        | ROp::LoadFalse
        | ROp::LoadNull
        | ROp::LoadUndef
        | ROp::Super
        | ROp::ReturnUndef
        | ROp::NewTarget
        | ROp::ImportMeta => {
            // ReturnUndef has no dst; the others all encode `dst` at offset 1.
            if !matches!(op, ROp::ReturnUndef) {
                check_reg(read_u16(1), "Load* dst")?;
            }
        }
        // [src:2]
        ROp::Return | ROp::Throw => check_reg(read_u16(1), "Return/Throw src")?,
        // [dst:2, left:2, right:2]
        ROp::Add
        | ROp::Sub
        | ROp::Mul
        | ROp::Div
        | ROp::Mod
        | ROp::Pow
        | ROp::Equal
        | ROp::NotEqual
        | ROp::StrictEqual
        | ROp::StrictNotEqual
        | ROp::GreaterThan
        | ROp::GreaterOrEqual
        | ROp::LessThan
        | ROp::LessOrEqual
        | ROp::Instanceof
        | ROp::In
        | ROp::BitwiseAnd
        | ROp::BitwiseOr
        | ROp::BitwiseXor
        | ROp::LeftShift
        | ROp::RightShift
        | ROp::UnsignedRightShift
        | ROp::Index => {
            check_reg(read_u16(1), "binop dst")?;
            check_reg(read_u16(3), "binop left")?;
            check_reg(read_u16(5), "binop right")?;
        }
        // [obj:2, key:2, val:2]
        ROp::SetIndex => {
            check_reg(read_u16(1), "SetIndex obj")?;
            check_reg(read_u16(3), "SetIndex key")?;
            check_reg(read_u16(5), "SetIndex val")?;
        }
        // [dst:2, base:2, nargs:1] — base..base+nargs+1 is callee+args
        ROp::Call | ROp::New => {
            check_reg(read_u16(1), "Call dst")?;
            check_reg_window(read_u16(3), read_u8(5) + 1, "Call args")?;
        }
        // [dst:2, global_idx:2, base:2, nargs:1] — base..base+nargs+1
        ROp::CallGlobal => {
            check_reg(read_u16(1), "CallGlobal dst")?;
            check_global(read_u16(3), "CallGlobal global")?;
            check_reg_window(read_u16(5), read_u8(7) + 1, "CallGlobal args")?;
        }
        // [dst:2, func:2, args_arr:2]
        ROp::CallSpread | ROp::NewSpread => {
            check_reg(read_u16(1), "Spread dst")?;
            check_reg(read_u16(3), "Spread func")?;
            check_reg(read_u16(5), "Spread args")?;
        }
        // [dst:2, base:2, count:2] — base..base+count
        ROp::Array | ROp::Hash => {
            check_reg(read_u16(1), "Array/Hash dst")?;
            check_reg_window(read_u16(3), read_u16(5), "Array/Hash items")?;
        }
        _ => {}
    }
    Ok(())
}

/// Return the true byte length of the instruction starting at `ip`.
///
/// Most opcodes report a fixed size via [`ROp::size`]; the only
/// variable-size instruction is [`ROp::MakeClosure`] whose trailing
/// capture list is keyed on a `u8` count byte. We inline that rule
/// here so both the forward walker and the jump-target walker stay
/// in step.
fn instruction_size(inst: &[u8], ip: usize, op: ROp) -> Result<usize, String> {
    match op {
        ROp::MakeClosure => {
            if ip + 6 > inst.len() {
                return Err(format!(
                    "truncated MakeClosure at ip {ip}: needs at least 6 bytes"
                ));
            }
            Ok(6 + (inst[ip + 5] as usize) * 2)
        }
        _ => Ok(op.size()),
    }
}

fn validate_jump_targets(
    inst: &[u8],
    boundaries: &HashSet<usize>,
) -> Result<(), String> {
    let mut ip = 0;
    while ip < inst.len() {
        let op = ROp::from_byte(inst[ip]).expect("decoded once above");
        match op {
            ROp::Jump => check_target_u32(inst, ip + 1, boundaries, ip, "Jump")?,
            ROp::JumpIfNot | ROp::JumpIfTruthy => {
                // [cond:2, target:4]
                check_target_u32(inst, ip + 3, boundaries, ip, "JumpIfNot/Truthy")?;
            }
            ROp::IncrementRegAndJump
            | ROp::TestLtConstJump
            | ROp::TestLeConstJump
            | ROp::TestLtRegJump
            | ROp::TestLeRegJump => {
                // [r:2, x:2, target:4]
                check_target_u32(inst, ip + 5, boundaries, ip, "Test*Jump")?;
            }
            ROp::ModRegConstStrictEqConstJump | ROp::TestModRegStrictEqConstJump => {
                // [r:2, mod:2, cmp:2, target:4]
                check_target_u32(inst, ip + 7, boundaries, ip, "Mod*EqConstJump")?;
            }
            ROp::EnterTry => {
                // [catch_target:4, exc_reg:2]
                check_target_u32(inst, ip + 1, boundaries, ip, "EnterTry")?;
            }
            _ => {}
        }
        ip += instruction_size(inst, ip, op)?;
    }
    Ok(())
}

fn check_target_u32(
    inst: &[u8],
    offset: usize,
    boundaries: &HashSet<usize>,
    ip: usize,
    kind: &'static str,
) -> Result<(), String> {
    if offset + 4 > inst.len() {
        return Err(format!(
            "{kind} at ip {ip}: operand reads past end of bytecode"
        ));
    }
    let target = u32::from_be_bytes([
        inst[offset],
        inst[offset + 1],
        inst[offset + 2],
        inst[offset + 3],
    ]) as usize;
    if !boundaries.contains(&target) {
        return Err(format!(
            "{kind} at ip {ip}: target {target} not on an instruction boundary"
        ));
    }
    Ok(())
}

/// Recursively validate every function body reachable from `constants`.
///
/// `visited` tracks every `Rc<Vec<u8>>` instructions buffer we've
/// already walked (by raw pointer). A cycle would be a compiler bug,
/// but we refuse to loop on one rather than hang.
fn validate_constants(
    constants: &[Object],
    visited: &mut HashSet<*const Vec<u8>>,
) -> Result<(), String> {
    for (idx, obj) in constants.iter().enumerate() {
        validate_object(obj, idx, visited)?;
    }
    Ok(())
}

fn validate_object(
    obj: &Object,
    context_idx: usize,
    visited: &mut HashSet<*const Vec<u8>>,
) -> Result<(), String> {
    match obj {
        Object::CompiledFunction(func) => validate_compiled_function(func, context_idx, visited),
        Object::Class(class) => validate_class(class, context_idx, visited),
        _ => Ok(()),
    }
}

fn validate_compiled_function(
    func: &CompiledFunctionObject,
    context_idx: usize,
    visited: &mut HashSet<*const Vec<u8>>,
) -> Result<(), String> {
    // Guard against cycles / re-validation. Two pointer-equal `Rc`s
    // share an instruction buffer and we only need to walk it once.
    let ptr: *const Vec<u8> = Rc::as_ptr(&func.instructions);
    if !visited.insert(ptr) {
        return Ok(());
    }
    let label = format!("function at constant[{context_idx}]");
    let limits = OperandLimits {
        register_count: func.register_count,
        num_cache_slots: func.num_cache_slots,
    };
    validate_buffer_with_context(
        &func.instructions,
        &func.constants,
        &limits,
        &label,
        visited,
    )?;
    validate_constants(&func.constants, visited)
}

fn validate_class(
    class: &ClassObject,
    context_idx: usize,
    visited: &mut HashSet<*const Vec<u8>>,
) -> Result<(), String> {
    let label_base = format!("class {} at constant[{context_idx}]", class.name);
    if let Some(ctor) = &class.constructor {
        let label = format!("{label_base}::constructor");
        validate_named_function(ctor, &label, visited)?;
    }
    for (name, method) in &class.methods {
        let label = format!("{label_base}::{name}");
        validate_named_function(method, &label, visited)?;
    }
    for (name, method) in &class.static_methods {
        let label = format!("{label_base}::static {name}");
        validate_named_function(method, &label, visited)?;
    }
    for (name, getter) in &class.getters {
        let label = format!("{label_base}::get {name}");
        validate_named_function(getter, &label, visited)?;
    }
    for (name, setter) in &class.setters {
        let label = format!("{label_base}::set {name}");
        validate_named_function(setter, &label, visited)?;
    }
    Ok(())
}

fn validate_named_function(
    func: &CompiledFunctionObject,
    label: &str,
    visited: &mut HashSet<*const Vec<u8>>,
) -> Result<(), String> {
    let ptr: *const Vec<u8> = Rc::as_ptr(&func.instructions);
    if !visited.insert(ptr) {
        return Ok(());
    }
    let limits = OperandLimits {
        register_count: func.register_count,
        num_cache_slots: func.num_cache_slots,
    };
    validate_buffer_with_context(&func.instructions, &func.constants, &limits, label, visited)?;
    validate_constants(&func.constants, visited)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::VmCell;

    #[test]
    fn accepts_halt_only() {
        let bc = Bytecode::new(vec![ROp::Halt as u8], vec![], vec![]);
        assert!(validate(&bc).is_ok());
    }

    #[test]
    fn rejects_empty() {
        let bc = Bytecode::new(vec![], vec![], vec![]);
        assert!(validate(&bc).is_err());
    }

    #[test]
    fn rejects_invalid_opcode() {
        // 0xFF is outside ROp's variant range.
        let bc = Bytecode::new(vec![0xFF, ROp::Halt as u8], vec![], vec![]);
        let err = validate(&bc).unwrap_err();
        assert!(err.contains("invalid opcode"));
    }

    #[test]
    fn rejects_truncated_instruction() {
        // LoadConst needs [dst:2, idx:2] = 5 bytes total, but we
        // supply only the opcode plus one operand byte.
        let bc = Bytecode::new(vec![ROp::LoadConst as u8, 0x00], vec![], vec![]);
        let err = validate(&bc).unwrap_err();
        assert!(err.contains("truncated"));
    }

    #[test]
    fn rejects_missing_terminator() {
        // Move has size 5; build a [Move, Move] sequence that runs off
        // the end without ever emitting a terminator. `register_count`
        // is set to 2 so the new register-bounds check doesn't reject
        // the operands first — the terminator check is what we're
        // asserting on.
        let mut bc = Bytecode::new(
            vec![
                ROp::Move as u8, 0, 0, 0, 1, // Move r0 <- r1
                ROp::Move as u8, 0, 1, 0, 0, // Move r1 <- r0 (last — not a terminator)
            ],
            vec![],
            vec![],
        );
        bc.register_count = 2;
        let err = validate(&bc).unwrap_err();
        assert!(err.contains("does not terminate"));
    }

    #[test]
    fn rejects_jump_to_middle_of_instruction() {
        // Move is 5 bytes, so a Jump landing at offset 2 is mid-instruction.
        let mut inst = Vec::new();
        inst.extend_from_slice(&[ROp::Jump as u8, 0, 0, 0, 0x02]); // Jump to ip=2
        inst.extend_from_slice(&[
            ROp::Move as u8, 0, 0, 0, 1, ROp::Halt as u8,
        ]);
        let mut bc = Bytecode::new(inst, vec![], vec![]);
        bc.register_count = 2;
        let err = validate(&bc).unwrap_err();
        assert!(err.contains("not on an instruction boundary"));
    }

    fn stub_function(instructions: Vec<u8>, constants: Vec<Object>) -> CompiledFunctionObject {
        CompiledFunctionObject {
            instructions: Rc::new(instructions),
            constants: Rc::new(constants),
            num_locals: 0,
            num_parameters: 0,
            rest_parameter_index: None,
            takes_this: false,
            is_async: false,
            is_generator: false,
            num_cache_slots: 0,
            max_stack_depth: 0,
            register_count: 1,
            inline_cache: Rc::new(VmCell::new(Vec::new())),
            closure_captures: Vec::new(),
            captured_values: Vec::new(),
            properties: None,
        }
    }

    #[test]
    fn recurses_into_function_body() {
        // Parent has a valid Halt, but its constants table contains a
        // function whose body is broken. The parent must still fail.
        let bad_fn = stub_function(vec![0xFF /* invalid opcode */], vec![]);
        let bc = Bytecode::new(
            vec![ROp::Halt as u8],
            vec![Object::CompiledFunction(Box::new(bad_fn))],
            vec![],
        );
        let err = validate(&bc).unwrap_err();
        assert!(err.contains("function at constant[0]"));
        assert!(err.contains("invalid opcode"));
    }

    #[test]
    fn recurses_into_class_method() {
        let mut class = ClassObject {
            name: "Demo".to_string(),
            parent_chain: Vec::new(),
            constructor: None,
            methods: rustc_hash::FxHashMap::default(),
            static_methods: rustc_hash::FxHashMap::default(),
            getters: rustc_hash::FxHashMap::default(),
            setters: rustc_hash::FxHashMap::default(),
            super_methods: rustc_hash::FxHashMap::default(),
            super_getters: rustc_hash::FxHashMap::default(),
            super_setters: rustc_hash::FxHashMap::default(),
            super_constructor_chain: Vec::new(),
            field_initializers: Vec::new(),
            static_initializers: Vec::new(),
            static_fields: rustc_hash::FxHashMap::default(),
        };
        // Method body is truncated — missing HaltValue terminator.
        let bad = stub_function(vec![ROp::LoadConst as u8, 0, 0, 0, 0], vec![]);
        class.methods.insert("boom".to_string(), bad);

        let bc = Bytecode::new(
            vec![ROp::Halt as u8],
            vec![Object::Class(Box::new(class))],
            vec![],
        );
        let err = validate(&bc).unwrap_err();
        assert!(err.contains("Demo"));
        assert!(err.contains("boom"));
    }

    #[test]
    fn rejects_make_closure_with_bad_const_idx() {
        // MakeClosure [dst:2, const_idx:2, count:1] = 6 bytes.
        // const_idx = 99 but constants table is empty.
        let inst = vec![
            ROp::MakeClosure as u8,
            0, 0,       // dst = 0
            0, 99,      // const_idx = 99
            0,          // count = 0
            ROp::Halt as u8,
        ];
        let bc = Bytecode::new(inst, vec![], vec![]);
        let err = validate(&bc).unwrap_err();
        assert!(
            err.contains("const_idx") && err.contains("out of range"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_make_closure_non_function_constant() {
        // const_idx = 0 is in range, but constants[0] is a String.
        let inst = vec![
            ROp::MakeClosure as u8,
            0, 0,       // dst = 0
            0, 0,       // const_idx = 0
            0,          // count = 0
            ROp::Halt as u8,
        ];
        let bc = Bytecode::new(inst, vec![Object::String("nope".into())], vec![]);
        let err = validate(&bc).unwrap_err();
        assert!(
            err.contains("not a CompiledFunction"),
            "got: {err}"
        );
    }

    /// Round 9: a Move with src=5 against `register_count = 2` would
    /// previously have been silently tolerated (the runtime stack is
    /// pre-allocated to STACK_SIZE so an out-of-window read just sees
    /// the next stack slot). With the strict check enabled, any
    /// register operand past the declared count is a clean compile-
    /// time error.
    #[test]
    fn rejects_register_operand_past_register_count() {
        let mut bc = Bytecode::new(
            vec![
                ROp::Move as u8, 0, 0, 0, 5, // Move r0 <- r5
                ROp::Halt as u8,
            ],
            vec![],
            vec![],
        );
        bc.register_count = 2;
        let err = validate(&bc).unwrap_err();
        assert!(
            err.contains("register 5") && err.contains("count = 2"),
            "got: {err}"
        );
    }

    /// `Call dst, base, nargs` reads `nargs + 1` consecutive registers
    /// from `base` (the callee plus its args). A window that walks off
    /// the end must be rejected.
    #[test]
    fn rejects_call_window_past_register_count() {
        // Call dst=0 base=3 nargs=4 — reads regs 3..8 against a
        // register_count of 4.
        let mut bc = Bytecode::new(
            vec![
                ROp::Call as u8, 0, 0, 0, 3, 4, // Call r0 = r3(r4..r7)
                ROp::Halt as u8,
            ],
            vec![],
            vec![],
        );
        bc.register_count = 4;
        let err = validate(&bc).unwrap_err();
        assert!(
            err.contains("register window") && err.contains("count = 4"),
            "got: {err}"
        );
    }
}
