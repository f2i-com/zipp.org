/// Register-based opcode set.
///
/// Each instruction explicitly names source/destination registers (u16 indices)
/// within the current call frame's register window. Operand widths are 1 (u8,
/// small count or flag) or 2 (u16, register index / constant index / global
/// index / jump target / cache slot).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ROp {
    // ── Loads ─────────────────────────────────────────────────────────────
    /// Load constant into register.  [dst:2, const_idx:2]
    LoadConst,
    /// Load `true`.  [dst:2]
    LoadTrue,
    /// Load `false`.  [dst:2]
    LoadFalse,
    /// Load `null`.  [dst:2]
    LoadNull,
    /// Load `undefined`.  [dst:2]
    LoadUndef,

    // ── Register ops ──────────────────────────────────────────────────────
    /// Copy register.  [dst:2, src:2]
    Move,

    // ── Global access ─────────────────────────────────────────────────────
    /// Load global into register.  [dst:2, global_idx:2]
    GetGlobal,
    /// Store register into global.  [global_idx:2, src:2]
    SetGlobal,

    // ── Arithmetic ────────────────────────────────────────────────────────
    /// [dst:2, left:2, right:2]
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,

    // ── Comparison ────────────────────────────────────────────────────────
    /// [dst:2, left:2, right:2]
    Equal,
    NotEqual,
    StrictEqual,
    StrictNotEqual,
    GreaterThan,
    GreaterOrEqual,
    LessThan,
    LessOrEqual,
    Instanceof,
    In,

    // ── Bitwise ───────────────────────────────────────────────────────────
    /// [dst:2, left:2, right:2]
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
    LeftShift,
    RightShift,
    UnsignedRightShift,

    // ── Unary ─────────────────────────────────────────────────────────────
    /// [dst:2, src:2]
    Neg,
    Not,
    UnaryPlus,
    Typeof,
    IsNullish,

    // ── Control flow ──────────────────────────────────────────────────────
    /// Unconditional jump.  [target:2]
    Jump,
    /// Jump if register is falsy.  [cond:2, target:2]
    JumpIfNot,
    /// Jump if register is truthy.  [cond:2, target:2]
    JumpIfTruthy,

    // ── Function calls ────────────────────────────────────────────────────
    /// Call function in `base` register with args in base+1..base+nargs.
    /// Result goes into `dst`.  [dst:2, base:2, nargs:1]
    Call,
    /// Fused method call: obj.method(args).  Object in `base` register,
    /// args in base+1..base+nargs.  [dst:2, base:2, nargs:1, prop_const:2, cache:2]
    CallMethod,
    /// Call with spread args array.  [dst:2, func:2, args_arr:2]
    CallSpread,
    /// Return value from function.  [src:2]
    Return,
    /// Return undefined from function.  []
    ReturnUndef,

    // ── Constructors ──────────────────────────────────────────────────────
    /// `new` with args in base+1..base+nargs.  [dst:2, base:2, nargs:1]
    New,
    /// `new` with spread args.  [dst:2, class:2, args_arr:2]
    NewSpread,
    /// Load super reference.  [dst:2]
    Super,

    // ── Collections ───────────────────────────────────────────────────────
    /// Create array from contiguous regs base..base+count-1.  [dst:2, base:2, count:2]
    Array,
    /// Create hash from contiguous regs (key,val pairs).  [dst:2, base:2, count:2]
    Hash,
    /// Append element to array.  [arr:2, val:2]
    AppendElement,
    /// Spread-append iterable to array.  [arr:2, iterable:2]
    AppendSpread,

    // ── Property access ───────────────────────────────────────────────────
    /// Get named property (inline-cached).  [dst:2, obj:2, prop_const:2, cache:2]
    GetProp,
    /// Set named property (inline-cached). Keeps value.  [obj:2, prop_const:2, src:2, cache:2]
    SetProp,
    /// Get property on global object.  [dst:2, global_idx:2, prop_const:2, cache:2]
    GetGlobalProp,
    /// Set property on global object.  [global_idx:2, prop_const:2, src:2, cache:2]
    SetGlobalProp,

    // ── Index access ──────────────────────────────────────────────────────
    /// Dynamic index read.  [dst:2, obj:2, key:2]
    Index,
    /// Dynamic index write.  [obj:2, key:2, val:2]
    SetIndex,
    /// Delete property.  [dst:2, obj:2, key:2]
    DeleteProp,

    // ── Iterator / destructuring ──────────────────────────────────────────
    /// Slice array from index.  [dst:2, iterable:2, skip:2]
    IteratorRest,
    /// Get object keys as array.  [dst:2, obj:2]
    GetKeysIter,
    /// Object rest (exclude keys in keys_base..keys_base+count-1).
    /// [dst:2, obj:2, keys_base:2, count:2]
    ObjectRest,

    // ── Async ─────────────────────────────────────────────────────────────
    /// Await a value.  [dst:2, src:2]
    Await,

    // ── Error ─────────────────────────────────────────────────────────────
    /// Throw value.  [src:2]
    Throw,

    // ── Fused ─────────────────────────────────────────────────────────────
    /// Fused: load global, call with args.  [dst:2, global_idx:2, base:2, nargs:1]
    CallGlobal,
    /// Fused: regs[dst] = regs[src] + constants[idx].  [dst:2, src:2, const_idx:2]
    AddRegConst,
    /// Fused: regs[dst] = regs[src] - constants[idx].  [dst:2, src:2, const_idx:2]
    SubRegConst,
    /// Fused: regs[dst] = regs[src] * constants[idx].  [dst:2, src:2, const_idx:2]
    MulRegConst,
    /// Fused: if !(regs[r] < constants[idx]) jump target.  [r:2, const_idx:2, target:2]
    TestLtConstJump,
    /// Fused: if !(regs[r] <= constants[idx]) jump target.  [r:2, const_idx:2, target:2]
    TestLeConstJump,
    /// Fused: regs[r] += constants[idx]; jump target.  [r:2, const_idx:2, target:2]
    IncrementRegAndJump,
    /// Fused: if !((regs[r] % const_a) === const_b) jump target.
    /// [r:2, mod_const:2, cmp_const:2, target:2]
    ModRegConstStrictEqConstJump,
    /// Fused: obj.prop += const (with inline cache).
    /// [obj:2, prop_const:2, val_const:2, cache:2]
    AddConstToRegProp,
    /// Fused: obj.dst_prop = obj.src1_prop + obj.src2_prop (with inline cache).
    /// [obj:2, s1_prop:2, s1_cache:2, s2_prop:2, s2_cache:2, dst_prop:2, dst_cache:2]
    AddRegPropsToRegProp,

    /// Fused: if !(regs[a] < regs[b]) jump target.  [a:2, b:2, target:2]
    TestLtRegJump,
    /// Fused: if !(regs[a] <= regs[b]) jump target.  [a:2, b:2, target:2]
    TestLeRegJump,
    /// Fused: if !((regs[a] % regs[b]) === const[c]) jump target.  [a:2, b:2, c:2, target:2]
    TestModRegStrictEqConstJump,

    /// Define a getter or setter on a hash object.
    /// [hash:2, func:2, prop_const:2, kind:1] where kind 0 = getter, 1 = setter.
    DefineAccessor,

    /// Run static initializers on a class object (in-place).
    /// [dst:2] — reads class from dst register, runs static initializers, writes back.
    InitClass,

    /// Load `new.target` into register.  [dst:2]
    NewTarget,
    /// Load `import.meta` into register.  [dst:2]
    ImportMeta,
    /// Yield a value from a generator.  [dst:2, src:2]
    /// `src` is the yielded value; on resume, the value passed to `.next(v)`
    /// is written to `dst`.
    Yield,

    // ── Closures ──────────────────────────────────────────────────────────
    /// Create a closure by snapshotting captured global slots.
    /// [dst:2, const_idx:2, count:1]
    /// Followed by `count` pairs of [slot:2] — the global slot indices to capture.
    MakeClosure,

    // ── Arguments ─────────────────────────────────────────────────────────
    /// Build `arguments` array-like object from the function's actual args.
    /// [dst:2, arg_start:2, num_params:2]
    /// At runtime, reads registers [arg_start..arg_start+nargs] where nargs
    /// is the actual number of passed arguments (from the call frame).
    MakeArguments,

    // ── Exception handling ────────────────────────────────────────────────
    /// Push a catch handler. If any called function throws, jump to target.
    /// [catch_target:4, exception_dst:2]
    EnterTry,
    /// Pop the active catch handler.  []
    LeaveTry,

    // ── Halt ──────────────────────────────────────────────────────────────
    /// Halt execution (no result).  []
    Halt,
    /// Halt execution with result.  [src:2]
    HaltValue,
}

impl ROp {
    pub fn from_byte(value: u8) -> Option<Self> {
        if value <= ROp::HaltValue as u8 {
            Some(unsafe { std::mem::transmute::<u8, ROp>(value) })
        } else {
            None
        }
    }

    /// Total instruction size in bytes (opcode + operands).
    /// Uses a lookup table indexed by opcode byte for O(1) dispatch.
    #[inline(always)]
    pub fn size(self) -> usize {
        // SAFETY: ROp is #[repr(u8)] so all variants fit in 0..=max_variant.
        // Table is large enough for all variants; unknown bytes return 1 (safe advance).
        const SIZES: [u8; 256] = {
            let mut t = [1u8; 256];
            // [] — 0 operand bytes → size 1
            t[ROp::ReturnUndef as usize] = 1;
            t[ROp::Halt as usize] = 1;
            // [x:2] — 2 operand bytes → size 3
            t[ROp::LoadTrue as usize] = 3; t[ROp::LoadFalse as usize] = 3;
            t[ROp::LoadNull as usize] = 3; t[ROp::LoadUndef as usize] = 3;
            t[ROp::Super as usize] = 3; t[ROp::HaltValue as usize] = 3;
            t[ROp::Return as usize] = 3; t[ROp::Throw as usize] = 3;
            t[ROp::InitClass as usize] = 3; t[ROp::NewTarget as usize] = 3;
            t[ROp::ImportMeta as usize] = 3; t[ROp::Jump as usize] = 5;
            // [x:2, y:2] — 4 operand bytes → size 5
            t[ROp::Move as usize] = 5; t[ROp::Neg as usize] = 5; t[ROp::Not as usize] = 5;
            t[ROp::UnaryPlus as usize] = 5; t[ROp::Typeof as usize] = 5;
            t[ROp::IsNullish as usize] = 5; t[ROp::AppendElement as usize] = 5;
            t[ROp::AppendSpread as usize] = 5; t[ROp::GetKeysIter as usize] = 5;
            t[ROp::Await as usize] = 5; t[ROp::Yield as usize] = 5;
            t[ROp::LoadConst as usize] = 5; t[ROp::GetGlobal as usize] = 5;
            t[ROp::SetGlobal as usize] = 5; t[ROp::JumpIfNot as usize] = 7;
            t[ROp::JumpIfTruthy as usize] = 7;
            // [x:2, y:2, z:2] — 6 operand bytes → size 7
            t[ROp::Add as usize] = 7; t[ROp::Sub as usize] = 7; t[ROp::Mul as usize] = 7;
            t[ROp::Div as usize] = 7; t[ROp::Mod as usize] = 7; t[ROp::Pow as usize] = 7;
            t[ROp::Equal as usize] = 7; t[ROp::NotEqual as usize] = 7;
            t[ROp::StrictEqual as usize] = 7; t[ROp::StrictNotEqual as usize] = 7;
            t[ROp::GreaterThan as usize] = 7; t[ROp::GreaterOrEqual as usize] = 7;
            t[ROp::LessThan as usize] = 7; t[ROp::LessOrEqual as usize] = 7;
            t[ROp::Instanceof as usize] = 7; t[ROp::In as usize] = 7;
            t[ROp::BitwiseAnd as usize] = 7; t[ROp::BitwiseOr as usize] = 7;
            t[ROp::BitwiseXor as usize] = 7; t[ROp::LeftShift as usize] = 7;
            t[ROp::RightShift as usize] = 7; t[ROp::UnsignedRightShift as usize] = 7;
            t[ROp::CallSpread as usize] = 7; t[ROp::NewSpread as usize] = 7;
            t[ROp::Index as usize] = 7; t[ROp::SetIndex as usize] = 7;
            t[ROp::DeleteProp as usize] = 7; t[ROp::Array as usize] = 7;
            t[ROp::Hash as usize] = 7; t[ROp::IteratorRest as usize] = 7;
            t[ROp::AddRegConst as usize] = 7; t[ROp::SubRegConst as usize] = 7;
            t[ROp::MulRegConst as usize] = 7;
            t[ROp::TestLtConstJump as usize] = 9; t[ROp::TestLeConstJump as usize] = 9;
            t[ROp::IncrementRegAndJump as usize] = 9;
            t[ROp::TestLtRegJump as usize] = 9; t[ROp::TestLeRegJump as usize] = 9;
            // [x:2, y:2, z:1] — 5 operand bytes → size 6
            t[ROp::Call as usize] = 6; t[ROp::New as usize] = 6;
            t[ROp::MakeClosure as usize] = 6;
            // MakeArguments: [dst:2, arg_start:2, num_params:2] = 7 bytes
            t[ROp::MakeArguments as usize] = 7;
            // EnterTry: [catch_target:4, exception_dst:2] = 7 bytes
            t[ROp::EnterTry as usize] = 7;
            t[ROp::LeaveTry as usize] = 1;
            // [x:2, y:2, z:2, w:1] — 7 operand bytes → size 8
            t[ROp::CallGlobal as usize] = 8; t[ROp::DefineAccessor as usize] = 8;
            // [x:2, y:2, z:2, w:2] — 8 operand bytes → size 9
            t[ROp::ObjectRest as usize] = 9;
            t[ROp::ModRegConstStrictEqConstJump as usize] = 11;
            t[ROp::TestModRegStrictEqConstJump as usize] = 11;
            t[ROp::AddConstToRegProp as usize] = 9;
            t[ROp::GetProp as usize] = 9; t[ROp::SetProp as usize] = 9;
            t[ROp::GetGlobalProp as usize] = 9; t[ROp::SetGlobalProp as usize] = 9;
            // [x:2, y:2, z:1, w:2, v:2] — 9 operand bytes → size 10
            t[ROp::CallMethod as usize] = 10;
            // [obj:2, s1p:2, s1c:2, s2p:2, s2c:2, dp:2, dc:2] — 14 → size 15
            t[ROp::AddRegPropsToRegProp as usize] = 15;
            t
        };
        SIZES[self as u8 as usize] as usize
    }

    /// Total operand bytes (not including the opcode byte itself).
    #[inline(always)]
    pub fn operand_bytes(self) -> usize {
        self.size() - 1
    }
}

/// Encode a register-based instruction into a byte vector.
pub fn rmake(op: ROp, operands: &[u32]) -> Vec<u8> {
    use ROp::*;
    // Get operand widths for this opcode
    let widths: &[usize] = match op {
        LoadConst | GetGlobal => &[2, 2],
        SetGlobal => &[2, 2],
        LoadTrue | LoadFalse | LoadNull | LoadUndef | Super | HaltValue | InitClass | NewTarget
        | ImportMeta => &[2],
        Move | Neg | Not | UnaryPlus | Typeof | IsNullish | AppendElement | AppendSpread
        | GetKeysIter | Await | Yield => &[2, 2],
        Add | Sub | Mul | Div | Mod | Pow | Equal | NotEqual | StrictEqual | StrictNotEqual
        | GreaterThan | GreaterOrEqual | LessThan | LessOrEqual | Instanceof | In | BitwiseAnd
        | BitwiseOr | BitwiseXor | LeftShift | RightShift | UnsignedRightShift | CallSpread
        | NewSpread | Index | SetIndex | DeleteProp => &[2, 2, 2],
        Jump => &[4],
        JumpIfNot | JumpIfTruthy => &[2, 4],
        Return | Throw => &[2],
        ReturnUndef | Halt => &[],
        Call | New => &[2, 2, 1],
        Array | Hash | IteratorRest => &[2, 2, 2],
        GetProp => &[2, 2, 2, 2],
        SetProp => &[2, 2, 2, 2],
        GetGlobalProp => &[2, 2, 2, 2],
        SetGlobalProp => &[2, 2, 2, 2],
        ObjectRest => &[2, 2, 2, 2],
        CallGlobal => &[2, 2, 2, 1],
        CallMethod => &[2, 2, 1, 2, 2],
        AddRegConst | SubRegConst | MulRegConst => &[2, 2, 2],
        TestLtConstJump | TestLeConstJump | IncrementRegAndJump
        | TestLtRegJump | TestLeRegJump => &[2, 2, 4],
        ModRegConstStrictEqConstJump | TestModRegStrictEqConstJump => &[2, 2, 2, 4],
        AddConstToRegProp => &[2, 2, 2, 2],
        AddRegPropsToRegProp => &[2, 2, 2, 2, 2, 2, 2],
        DefineAccessor => &[2, 2, 2, 1],
        // Variable-length: [dst:2, const_idx:2, count:1, slot0:2, slot1:2, ...]
        // rmake handles this specially below
        MakeClosure => &[2, 2, 1],
        MakeArguments => &[2, 2, 2],
        EnterTry => &[4, 2],
        LeaveTry => &[],
    };

    let mut len = 1usize;
    for w in widths {
        len += w;
    }
    let mut out = vec![0u8; len];
    out[0] = op as u8;
    let mut offset = 1usize;
    for (i, operand) in operands.iter().enumerate() {
        let width = *widths.get(i).unwrap_or(&0);
        match width {
            4 => {
                // u32 big-endian (for jump targets)
                let v = *operand as u32;
                out[offset] = ((v >> 24) & 0xff) as u8;
                out[offset + 1] = ((v >> 16) & 0xff) as u8;
                out[offset + 2] = ((v >> 8) & 0xff) as u8;
                out[offset + 3] = (v & 0xff) as u8;
            }
            2 => {
                out[offset] = ((operand >> 8) & 0xff) as u8;
                out[offset + 1] = (operand & 0xff) as u8;
            }
            1 => {
                out[offset] = (operand & 0xff) as u8;
            }
            _ => {}
        }
        offset += width;
    }

    // Variable-length tail for MakeClosure: append slot indices as u16 big-endian
    if op == MakeClosure && operands.len() > 3 {
        for &slot in &operands[3..] {
            out.push(((slot >> 8) & 0xff) as u8);
            out.push((slot & 0xff) as u8);
        }
    }

    out
}
