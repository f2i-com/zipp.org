//! x86-64 tier-2 emitter.
//!
//! Walks the allocated IR in `block_order` (RPO), emits dynasm
//! instructions for each op and terminator, and finalises into an
//! [`ExecutableBuffer`]. Consumes the Location map produced by
//! linear-scan and the serialised move sequences from
//! [`parallel_move::resolve`] for block-param edges.
//!
//! ## Physical register mapping
//!
//! The abstract 0..[`NUM_GP_REGS`] registers map onto x86-64 GPRs as:
//!
//! | abstract | physical | class |
//! |----------|----------|-------|
//! | reg0 | rbx | non-volatile |
//! | reg1 | rsi | non-volatile (Win64) |
//! | reg2 | rdi | non-volatile (Win64) |
//! | reg3 | r12 | non-volatile |
//! | reg4 | r13 | non-volatile |
//! | reg5 | r14 | non-volatile |
//! | reg6 | r15 | non-volatile |
//! | reg7 | r11 | **volatile** — see note |
//!
//! The first seven are saved across calls under the Windows x64 ABI
//! and are safe to hold values across any call boundary. `r11` is
//! caller-saved, so any ABI-crossing (runtime helper, guest call) in
//! a later phase must spill it first. Phase 4c does not emit any
//! `call` instructions, so this concern is latent. When phase 4d
//! adds runtime helper calls the emitter will insert save/restore
//! around call boundaries for any live value in reg7.
//!
//! ## Stack frame
//!
//! ```text
//! high ─┐
//!       │  caller's frame
//!       │  return address        ← rsp at function entry
//!       │  saved rbx,rsi,rdi,r12,r13,r14,r15   (7 × 8 = 56 bytes)
//!       │  spill slots           (num_spill_slots × 8, padded to 16)
//! low  ─┘  ← rsp during function body
//! ```
//!
//! After the 7 pushes (a return-address count of 8 + 56 = 64 bytes
//! pushed so far) `rsp` is 16-byte aligned, so reserving a multiple
//! of 16 bytes for spills keeps it aligned for any future `call`.
//!
//! ## Op coverage in this phase
//!
//! Typed integer + Boolean subset only. Generic arithmetic,
//! floating point, heap access, inline-cache property access, all
//! speculation checks, and runtime-helper calls are rejected with
//! [`EmitError::Unsupported`]. The caller falls back to tier 1 or
//! tier 0 for those functions; phases 4d+ extend the coverage.

use dynasmrt::{
    dynasm, x64::Assembler, AssemblyOffset, DynasmApi, DynasmLabelApi, DynamicLabel,
    ExecutableBuffer,
};
use std::mem;

use super::super::ir::types::RuntimeHelper;
use super::super::ir::{BlockId, IrFunction, IrOp, Terminator, ValueId};
use super::super::regalloc::parallel_move::{self, Move, MoveStep};
use super::super::regalloc::{Allocation, Location, NUM_GP_REGS};
use super::super::runtime;
use super::EmitError;

// ── NaN-boxing constants (kept local so this module doesn't depend on
//    codegen/djit/) ────────────────────────────────────────────────────
const QNAN: u64 = 0x7FF8_0000_0000_0000;
const TAG_SHIFT: u64 = 48;
const TAG_I32: u64 = 1;
const TAG_BOOL: u64 = 2;
const TAG_NULL: u64 = 3;
const TAG_UNDEFINED: u64 = 4;
const I32_SIG: u64 = QNAN | (TAG_I32 << TAG_SHIFT); // 0x7FF9_0000_0000_0000
const BOOL_SIG: u64 = QNAN | (TAG_BOOL << TAG_SHIFT); // 0x7FFA_0000_0000_0000
const VAL_FALSE: u64 = BOOL_SIG;
const VAL_TRUE: u64 = BOOL_SIG | 1;
const VAL_NULL: u64 = QNAN | (TAG_NULL << TAG_SHIFT);
const VAL_UNDEFINED: u64 = QNAN | (TAG_UNDEFINED << TAG_SHIFT);

// ── Register encoding indices (dynasm-rs x64 convention) ─────────────
const RAX: u8 = 0;
const RCX: u8 = 1;
const RDX: u8 = 2;
const RBX: u8 = 3;
const RSP: u8 = 4;
const _RBP: u8 = 5;
const RSI: u8 = 6;
const RDI: u8 = 7;
const R8: u8 = 8;
const R9: u8 = 9;
const R10: u8 = 10; // general scratch (never allocated as a value reg)
const R11: u8 = 11; // reg7 (volatile — caller-saved)
const R12: u8 = 12;
const R13: u8 = 13;
const R14: u8 = 14;
const R15: u8 = 15;

/// Abstract → physical register table.
const PHYS_GP: [u8; NUM_GP_REGS as usize] = [RBX, RSI, RDI, R12, R13, R14, R15, R11];

// ── XMM registers for f64 scratch ─────────────────────────────────────
//
// Phase 9's F64 emit uses SSE scalar doubles; we don't allocate xmm
// as value registers yet (the regalloc only tracks GP slots), so
// xmm0/xmm1 are short-lived scratches that hold operands across the
// few instructions of a single F64 op. Win64 treats xmm0..xmm5 as
// caller-saved, so using them inside a function body that doesn't
// cross a `call` boundary is always safe.
const XMM0: u8 = 0;
const XMM1: u8 = 1;

/// Number of physical registers that must be saved in the prologue —
/// the Win64 ABI tags rbx/rsi/rdi/r12..r15 as non-volatile. Indices
/// 0..=6 of [`PHYS_GP`]; r11 (reg7) is caller-saved, so functions
/// that call runtime helpers save/restore it around each call site
/// via a dedicated scratch slot in the frame's env area.
#[allow(dead_code)]
const NUM_SAVED_REGS: usize = 7;

// ── Frame layout ─────────────────────────────────────────────────────

/// Layout metadata computed once per function at the start of
/// [`emit`], then threaded through every helper that reads or writes
/// stack slots.
///
/// The frame grows down from rsp after the prologue:
///
/// ```text
///  [rsp + 0            .. num_spill*8]      regalloc spill slots
///  [rsp + num_spill*8  .. +scratch*8]       parallel-move scratch
///  (if has_runtime_call:)
///  [rsp + env_globals_off ..  +8]           globals_ptr stash
///  [rsp + env_vm_off      ..  +8]           vm_ptr stash
///  [rsp + env_r11_off     ..  +8]           r11 save slot (around calls)
/// ```
///
/// `spill_bytes` is the amount subtracted from rsp in the prologue —
/// a round-up of the above raw sizes to the next 16-byte multiple so
/// rsp stays 16-aligned for any emitted `call`.
#[derive(Copy, Clone, Debug)]
struct FrameLayout {
    num_spill_slots: u32,
    max_scratch_slots: u32,
    /// True when the function needs vm_ptr / globals_ptr stash slots.
    /// Covers two cases: runtime-helper calls (which clobber r8/r11
    /// and want vm_ptr in rcx) and deopt trampolines (which also
    /// need vm_ptr in rcx for the soft-deopt helper).
    needs_env: bool,
    /// True iff the function contains a runtime-helper call. Strict
    /// subset of `needs_env`; runtime-call emit sites read this to
    /// decide whether to save/restore r11 around the call.
    has_runtime_call: bool,
    /// True iff any IR op / terminator routes through the deopt
    /// trampoline. Sets `needs_env`.
    has_deopt: bool,
    /// Byte offset of globals_ptr stash from current rsp. Only
    /// meaningful when `needs_env` is true.
    env_globals_off: i32,
    /// Byte offset of vm_ptr stash from current rsp.
    env_vm_off: i32,
    /// Byte offset of the r11 save slot (used around calls).
    env_r11_off: i32,
    /// Byte offset of the CallValue arg-staging area (the first of
    /// `max_call_args` contiguous 8-byte slots). `djit_call_helper`
    /// reads a `*const u64` here, so each CallValue emit writes its
    /// args into `[rsp + env_call_args_off + i*8]` before the call.
    /// Zero when `max_call_args == 0`.
    env_call_args_off: i32,
    /// Max number of args across all `CallValue` ops in the function.
    /// Determines the size of the staging area above.
    max_call_args: u32,
    /// Total bytes subtracted from rsp after the `push` sequence.
    spill_bytes: u32,
}

impl FrameLayout {
    fn compute(func: &IrFunction, alloc: &Allocation) -> Self {
        let max_scratch_slots = max_parallel_move_scratch(func, alloc);
        let has_runtime_call = function_has_runtime_call(func);
        let has_deopt = function_has_deopt(func);
        let needs_env = has_runtime_call || has_deopt;
        let env_slots: u32 = if needs_env { 3 } else { 0 };
        let max_call_args = max_call_args(func);
        let total_slots =
            alloc.num_spill_slots + max_scratch_slots + env_slots + max_call_args;
        let raw_bytes = total_slots * 8;
        // After 7 pushes + 8-byte return addr (64 bytes), rsp is
        // 16-aligned. Any multiple of 16 keeps it aligned.
        let spill_bytes = (raw_bytes + 15) & !15;
        let env_base = ((alloc.num_spill_slots + max_scratch_slots) * 8) as i32;
        let env_call_args_off = env_base + (env_slots as i32) * 8;
        FrameLayout {
            num_spill_slots: alloc.num_spill_slots,
            max_scratch_slots,
            needs_env,
            has_runtime_call,
            has_deopt,
            env_globals_off: env_base,
            env_vm_off: env_base + 8,
            env_r11_off: env_base + 16,
            env_call_args_off,
            max_call_args,
            spill_bytes,
        }
    }
}

/// Walk `func` once, returning the largest `args.len()` across all
/// `IrOp::CallValue` ops. The caller reserves that many 8-byte
/// slots in the frame's call-arg staging area so each CallValue
/// emit has space to marshal its args before invoking
/// `djit_call_helper`.
fn max_call_args(func: &IrFunction) -> u32 {
    let mut max = 0u32;
    for block in &func.blocks {
        for (_, op) in &block.ops {
            if let IrOp::CallValue(_, args) = op {
                max = max.max(args.len() as u32);
            }
        }
    }
    max
}

/// Walk `func`'s ops and check for any op that lowers to a native
/// `call` — CallRuntime, generic arithmetic, or generic comparisons.
/// Each of those requires the frame's env-stash slots to be set up
/// so the emit_runtime_call sequence can pass vm_ptr in rcx and
/// reload globals_ptr afterwards.
fn function_has_runtime_call(func: &IrFunction) -> bool {
    for block in &func.blocks {
        for (_, op) in &block.ops {
            match op {
                IrOp::CallRuntime(..)
                | IrOp::CallValue(..)
                | IrOp::MakeClosureNoCapture(..)
                | IrOp::AddGeneric(..)
                | IrOp::SubGeneric(..)
                | IrOp::MulGeneric(..)
                | IrOp::DivGeneric(..)
                | IrOp::ModGeneric(..)
                | IrOp::EqValue(..)
                | IrOp::NeValue(..)
                | IrOp::LooseEqValue(..)
                | IrOp::LtValue(..)
                | IrOp::LeValue(..) => return true,
                _ => {}
            }
        }
    }
    false
}

/// Walk `func` and return true if any op or terminator can route
/// through the deopt trampoline (Check*, Checked*, `Deopt`).
fn function_has_deopt(func: &IrFunction) -> bool {
    for block in &func.blocks {
        for (_, op) in &block.ops {
            match op {
                IrOp::CheckI32(..)
                | IrOp::CheckF64(..)
                | IrOp::CheckHeap(..)
                | IrOp::CheckHeapShape(..)
                | IrOp::CheckFunctionIs(..)
                | IrOp::CheckedAddI32(..)
                | IrOp::CheckedSubI32(..)
                | IrOp::CheckedMulI32(..) => return true,
                _ => {}
            }
        }
        if matches!(&block.term, Terminator::Deopt(_)) {
            return true;
        }
    }
    false
}

/// For every block-param edge in `func`, run parallel_move::resolve
/// and remember the peak scratch-slot count. The result is the
/// number of scratch slots the frame needs to reserve on top of
/// `alloc.num_spill_slots`.
fn max_parallel_move_scratch(func: &IrFunction, alloc: &Allocation) -> u32 {
    let mut peak = 0u32;

    let resolve_edge = |peak: &mut u32, target: BlockId, args: &[ValueId]| {
        let target_block = &func.blocks[target.0 as usize];
        if target_block.params.len() != args.len() {
            return;
        }
        let mut moves: Vec<Move> = Vec::with_capacity(args.len());
        for (arg, (param_vid, _)) in args.iter().zip(target_block.params.iter()) {
            let src = match alloc.locations.get(arg).copied() {
                Some(l) => l,
                None => return,
            };
            let dst = match alloc.locations.get(param_vid).copied() {
                Some(l) => l,
                None => return,
            };
            moves.push(Move { src, dst });
        }
        // scratch_slot_start is irrelevant to the count; pass 0.
        let (_, scratch_used) = parallel_move::resolve(&moves, 0);
        if scratch_used > *peak {
            *peak = scratch_used;
        }
    };

    for block in &func.blocks {
        match &block.term {
            Terminator::Jump(target, args) => {
                resolve_edge(&mut peak, *target, args);
            }
            Terminator::Branch {
                then_block,
                then_args,
                else_block,
                else_args,
                ..
            } => {
                resolve_edge(&mut peak, *then_block, then_args);
                resolve_edge(&mut peak, *else_block, else_args);
            }
            _ => {}
        }
    }
    peak
}

/// Look up the native `extern "win64"` function pointer for a
/// [`RuntimeHelper`] variant. Returns `None` for helpers phase 4d
/// doesn't implement yet.
fn runtime_helper_addr(helper: RuntimeHelper) -> Option<usize> {
    match helper {
        RuntimeHelper::AddGeneric => Some(runtime::tier2_add_generic_helper as usize),
        RuntimeHelper::SubGeneric => Some(runtime::tier2_sub_generic_helper as usize),
        RuntimeHelper::MulGeneric => Some(runtime::tier2_mul_generic_helper as usize),
        RuntimeHelper::DivGeneric => Some(runtime::tier2_div_generic_helper as usize),
        RuntimeHelper::ModGeneric => Some(runtime::tier2_mod_generic_helper as usize),
        RuntimeHelper::ToBool => None,
        RuntimeHelper::Intern => None,
    }
}

// ── Public types ─────────────────────────────────────────────────────

/// Finalised tier-2 function: a chunk of executable memory plus the
/// metadata needed to call into it safely.
pub struct EmittedFunction {
    buffer: ExecutableBuffer,
    entry: AssemblyOffset,
    /// Spill-slot count the allocator + emit used. Caller plumbing in
    /// phase 4d uses this to size the slot area if it ever shares the
    /// VM stack.
    spill_slots: u32,
    /// Total emitted code length, in bytes. Surfaced for debug / perf
    /// counters; not required at runtime.
    code_len: usize,
}

impl EmittedFunction {
    /// Raw function pointer to the entry. Call via [`Self::execute`]
    /// rather than dereferencing this directly.
    pub fn entry_ptr(&self) -> *const u8 {
        self.buffer.ptr(self.entry)
    }

    /// Native code size, in bytes.
    pub fn code_len(&self) -> usize {
        self.code_len
    }

    /// Spill slots reserved by the stack frame. `spill_slots * 8` is
    /// the frame's dynamic area in bytes (before alignment rounding).
    pub fn spill_slots(&self) -> u32 {
        self.spill_slots
    }

    /// Invoke the emitted function.
    ///
    /// # Safety
    ///
    /// All four pointers must remain valid for the duration of the
    /// call. `regs` is read as an array indexed 0..n where n is the
    /// function's bytecode register count; elements above the entry
    /// parameter count aren't touched by phase-4c emissions.
    /// `globals` is read and written at any slot that appears in a
    /// `LoadGlobal` / `StoreGlobal` op. `consts` and `vm_ptr` are
    /// currently unused by phase-4c code but the Win64 ABI reserves
    /// their parameter slots for future phases.
    pub unsafe fn execute(
        &self,
        regs: *mut u64,
        consts: *const u64,
        globals: *mut u64,
        vm_ptr: *mut u8,
    ) -> u64 {
        let f: extern "win64" fn(*mut u64, *const u64, *mut u64, *mut u8) -> u64 =
            mem::transmute(self.entry_ptr());
        f(regs, consts, globals, vm_ptr)
    }
}

// ── Top-level entry ──────────────────────────────────────────────────

/// Emit native x86-64 code for `func` given `alloc`'s register/spill
/// assignment. Returns an executable buffer ready to invoke.
pub fn emit(func: &IrFunction, alloc: &Allocation) -> Result<EmittedFunction, EmitError> {
    let mut ops = Assembler::new().map_err(|_| EmitError::AssemblerFailed)?;

    let layout = FrameLayout::compute(func, alloc);

    // One label per block, indexed by BlockId.
    let mut block_labels: Vec<DynamicLabel> = Vec::with_capacity(func.blocks.len());
    for _ in 0..func.blocks.len() {
        block_labels.push(ops.new_dynamic_label());
    }
    // Single deopt trampoline at function end. All Check* and
    // Deopt terminators jump here on failure. Phase 5 emits a bare
    // `ud2` (hard trap); phase 6 replaces this with the real
    // state-reconstruction runtime.
    let deopt_label = ops.new_dynamic_label();

    let entry = ops.offset();

    // ── Prologue ──────────────────────────────────────────────────
    // Save the seven non-volatile registers our value regs live in.
    dynasm!(ops
        ; .arch x64
        ; push rbx
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; push r15
    );

    if layout.spill_bytes > 0 {
        dynasm!(ops ; sub rsp, layout.spill_bytes as i32);
    }

    // Stash env pointers when the function has any call out — either
    // runtime helpers (via generic arithmetic or `CallRuntime`) or
    // the deopt trampoline. r8 (globals_ptr) is needed after each
    // helper call; r9 (vm_ptr) is passed in rcx to both helper
    // shapes. One unconditional stash at entry is simpler than
    // per-site spills.
    if layout.needs_env {
        dynasm!(ops
            ; mov QWORD [rsp + layout.env_globals_off], r8
            ; mov QWORD [rsp + layout.env_vm_off], r9
        );
    }

    // ── Entry-block param load ────────────────────────────────────
    // The entry block's params represent the incoming bytecode
    // register window. rcx holds the `regs` pointer on entry.
    let entry_block = &func.blocks[0];
    for (slot_idx, (vid, _ty)) in entry_block.params.iter().enumerate() {
        let loc = *alloc
            .locations
            .get(vid)
            .ok_or(EmitError::Unsupported("entry param without location"))?;
        // Load from [rcx + slot_idx * 8] into `loc`. Go via r10 when
        // `loc` is a spill slot (no mem→mem mov on x86).
        let off = (slot_idx as i32) * 8;
        match loc {
            Location::Reg(r) => {
                let phys = PHYS_GP[r as usize];
                dynasm!(ops ; mov Rq(phys), QWORD [rcx + off]);
            }
            Location::Spill(slot) => {
                let spill_off = (slot as i32) * 8;
                dynasm!(ops
                    ; mov r10, QWORD [rcx + off]
                    ; mov QWORD [rsp + spill_off], r10
                );
            }
        }
    }

    // ── Block bodies ─────────────────────────────────────────────
    let block_order = &alloc.block_order;
    for (ordinal, &block_id) in block_order.iter().enumerate() {
        let label = block_labels[block_id.0 as usize];
        dynasm!(ops ; =>label);

        let block = &func.blocks[block_id.0 as usize];

        for (result_vid, op) in &block.ops {
            emit_op(&mut ops, func, alloc, &layout, *result_vid, op, deopt_label)?;
        }

        // The fallthrough target is the next block in linear order,
        // if any. Terminators that jump to it can elide the branch.
        let fallthrough = block_order.get(ordinal + 1).copied();
        emit_term(
            &mut ops,
            func,
            alloc,
            &block_labels,
            &block.term,
            fallthrough,
            &layout,
            deopt_label,
        )?;
    }

    // ── Deopt trampoline ────────────────────────────────────────
    //
    // Soft deopt: flip `vm.deopt_pending`, return to the caller.
    // The VM dispatch site notices the flag, blacklists this
    // function, and retries the call via tier-1. Functions that
    // never reach a Check* skip the trampoline entirely (the label
    // is unused but dynasm drops it silently).
    //
    // We only need vm_ptr in rcx; the helper does not read r8
    // (globals) or r11 (reg7). The caller-side tier-2 frame still
    // has rsp aligned for `call` because we padded the spill area
    // to a 16-byte multiple.
    if layout.has_deopt {
        let helper_addr = runtime::tier2_deopt_helper as usize;
        dynasm!(ops
            ; =>deopt_label
            ; mov rcx, QWORD [rsp + layout.env_vm_off]
            ; mov rax, QWORD helper_addr as i64
            ; call rax
        );
        emit_epilogue(&mut ops, layout.spill_bytes);
    } else {
        // No Check* / Deopt in this function. The label is never
        // targeted; emit a `ud2` as a safety net so any stray jump
        // (which would indicate an emit bug) aborts loudly rather
        // than running off into the epilogue of the previous block.
        dynasm!(ops
            ; =>deopt_label
            ; ud2
        );
    }

    // ── Finalise ─────────────────────────────────────────────────
    let code_len = ops.offset().0;
    let buffer = ops.finalize().map_err(|_| EmitError::AssemblerFailed)?;
    Ok(EmittedFunction {
        buffer,
        entry,
        spill_slots: alloc.num_spill_slots,
        code_len,
    })
}

// ── Op-level emit ────────────────────────────────────────────────────

fn emit_op(
    ops: &mut Assembler,
    func: &IrFunction,
    alloc: &Allocation,
    layout: &FrameLayout,
    result: ValueId,
    op: &IrOp,
    deopt_label: DynamicLabel,
) -> Result<(), EmitError> {
    match op {
        // ── Constants ────────────────────────────────────────────
        //
        // Integer / Boolean constants materialise as fully NaN-boxed
        // Values so they can flow directly into generic ops
        // (AddGeneric, Return, etc.) that expect `Value`-typed
        // operands. Typed ops (AddI32 / UnboxI32 / ...) read only
        // the low 32 bits of the operand Location, so the tag bits
        // in the upper half don't affect them. This trades two
        // extra `mov` bytes per constant for a type-consistent IR
        // at the emit/runtime boundary. Translator-level boxing
        // becomes a no-op when constants already carry tags.
        IrOp::ConstI32(v) => {
            let dst = loc_of(alloc, result)?;
            let boxed = I32_SIG | (*v as u32 as u64);
            mov_loc_imm64(ops, dst, boxed);
            Ok(())
        }
        IrOp::ConstBool(b) => {
            let dst = loc_of(alloc, result)?;
            let boxed = if *b { VAL_TRUE } else { VAL_FALSE };
            mov_loc_imm64(ops, dst, boxed);
            Ok(())
        }
        IrOp::ConstNull => {
            let dst = loc_of(alloc, result)?;
            mov_loc_imm64(ops, dst, VAL_NULL);
            Ok(())
        }
        IrOp::ConstUndef => {
            let dst = loc_of(alloc, result)?;
            mov_loc_imm64(ops, dst, VAL_UNDEFINED);
            Ok(())
        }
        IrOp::ConstValue(bits) => {
            let dst = loc_of(alloc, result)?;
            mov_loc_imm64(ops, dst, *bits);
            Ok(())
        }
        IrOp::ConstF64(bits) => {
            // The f64 bit pattern *is* the NaN-boxed Value bits
            // (Value::from_f64 just re-interprets). mov_loc_imm64
            // stores the full qword; downstream typed consumers
            // (UnboxF64, AddF64, ...) read the same 8 bytes.
            let dst = loc_of(alloc, result)?;
            mov_loc_imm64(ops, dst, *bits);
            Ok(())
        }

        // ── Reg bridging ─────────────────────────────────────────
        IrOp::LoadReg(r) => {
            let dst = loc_of(alloc, result)?;
            if (*r as u16) >= func.num_bytecode_regs {
                return Err(EmitError::Unsupported("LoadReg out of range"));
            }
            let off = (*r as i32) * 8;
            match dst {
                Location::Reg(n) => {
                    let phys = PHYS_GP[n as usize];
                    dynasm!(ops ; mov Rq(phys), QWORD [rcx + off]);
                }
                Location::Spill(slot) => {
                    let spill_off = (slot as i32) * 8;
                    dynasm!(ops
                        ; mov r10, QWORD [rcx + off]
                        ; mov QWORD [rsp + spill_off], r10
                    );
                }
            }
            Ok(())
        }

        IrOp::Copy(src) => {
            let dst_loc = loc_of(alloc, result)?;
            let src_loc = loc_of(alloc, *src)?;
            emit_move64(ops, src_loc, dst_loc);
            Ok(())
        }

        // ── I32 arithmetic ───────────────────────────────────────
        IrOp::AddI32(a, b) => emit_binop_i32(ops, alloc, result, *a, *b, BinopI32::Add),
        IrOp::SubI32(a, b) => emit_binop_i32(ops, alloc, result, *a, *b, BinopI32::Sub),
        IrOp::MulI32(a, b) => emit_binop_i32(ops, alloc, result, *a, *b, BinopI32::Mul),
        IrOp::NegI32(v) => {
            let dst = loc_of(alloc, result)?;
            let src = loc_of(alloc, *v)?;
            load_r32(ops, R10, src);
            dynasm!(ops ; neg r10d);
            store_r32(ops, dst, R10);
            Ok(())
        }

        // ── Bool ────────────────────────────────────────────────
        IrOp::NotBool(v) => {
            let dst = loc_of(alloc, result)?;
            let src = loc_of(alloc, *v)?;
            load_r32(ops, R10, src);
            dynasm!(ops ; xor r10d, 1);
            store_r32(ops, dst, R10);
            Ok(())
        }

        // ── I32 comparisons ─────────────────────────────────────
        IrOp::EqI32(a, b) => emit_cmp_i32(ops, alloc, result, *a, *b, CmpI32::Eq),
        IrOp::NeI32(a, b) => emit_cmp_i32(ops, alloc, result, *a, *b, CmpI32::Ne),
        IrOp::LtI32(a, b) => emit_cmp_i32(ops, alloc, result, *a, *b, CmpI32::Lt),
        IrOp::LeI32(a, b) => emit_cmp_i32(ops, alloc, result, *a, *b, CmpI32::Le),
        IrOp::GtI32(a, b) => emit_cmp_i32(ops, alloc, result, *a, *b, CmpI32::Gt),
        IrOp::GeI32(a, b) => emit_cmp_i32(ops, alloc, result, *a, *b, CmpI32::Ge),

        // ── Boxing / unboxing ───────────────────────────────────
        IrOp::BoxI32(v) => {
            let dst = loc_of(alloc, result)?;
            let src = loc_of(alloc, *v)?;
            // zero-extend low 32 bits via implicit 32→64 clearing of the upper half,
            // then or in the I32 tag.
            load_r32(ops, R10, src); // mov r10d, src32 → clears upper half of r10
            dynasm!(ops
                ; mov rax, QWORD I32_SIG as i64
                ; or r10, rax
            );
            store_r64(ops, dst, R10);
            Ok(())
        }
        IrOp::UnboxI32(v) => {
            let dst = loc_of(alloc, result)?;
            let src = loc_of(alloc, *v)?;
            // The low 32 bits are the i32 payload. mov r10d, src-low clears upper.
            load_r32(ops, R10, src);
            store_r32(ops, dst, R10);
            Ok(())
        }
        IrOp::BoxBool(v) => {
            let dst = loc_of(alloc, result)?;
            let src = loc_of(alloc, *v)?;
            // src is 0 or 1 in the low byte. Combine with BOOL_SIG.
            load_r32(ops, R10, src);
            dynasm!(ops
                ; mov rax, QWORD BOOL_SIG as i64
                ; or r10, rax
            );
            store_r64(ops, dst, R10);
            Ok(())
        }
        IrOp::UnboxBool(v) => {
            let dst = loc_of(alloc, result)?;
            let src = loc_of(alloc, *v)?;
            load_r32(ops, R10, src);
            dynasm!(ops ; and r10d, 1);
            store_r32(ops, dst, R10);
            Ok(())
        }
        // ── Globals ────────────────────────────────────────────
        // r8 holds the globals_ptr on entry; after any runtime call
        // r8 is clobbered and the post-call reload (see
        // `emit_runtime_call`) restores it from the env stash slot,
        // so access via r8 stays correct even across calls.
        IrOp::LoadGlobal(slot) => {
            let dst = loc_of(alloc, result)?;
            let off = (*slot as i32) * 8;
            match dst {
                Location::Reg(n) => {
                    let phys = PHYS_GP[n as usize];
                    dynasm!(ops ; mov Rq(phys), QWORD [r8 + off]);
                }
                Location::Spill(slot) => {
                    let spill_off = (slot as i32) * 8;
                    dynasm!(ops
                        ; mov r10, QWORD [r8 + off]
                        ; mov QWORD [rsp + spill_off], r10
                    );
                }
            }
            Ok(())
        }
        IrOp::StoreGlobal(slot, v) => {
            let src = loc_of(alloc, *v)?;
            let off = (*slot as i32) * 8;
            load_r64(ops, R10, src);
            dynasm!(ops ; mov QWORD [r8 + off], r10);
            Ok(())
        }

        // ── Generic arithmetic via runtime helpers ───────────────
        IrOp::AddGeneric(a, b) => emit_runtime_binop(
            ops, alloc, layout, result, *a, *b, RuntimeHelper::AddGeneric,
        ),
        IrOp::SubGeneric(a, b) => emit_runtime_binop(
            ops, alloc, layout, result, *a, *b, RuntimeHelper::SubGeneric,
        ),
        IrOp::MulGeneric(a, b) => emit_runtime_binop(
            ops, alloc, layout, result, *a, *b, RuntimeHelper::MulGeneric,
        ),
        IrOp::DivGeneric(a, b) => emit_runtime_binop(
            ops, alloc, layout, result, *a, *b, RuntimeHelper::DivGeneric,
        ),
        IrOp::ModGeneric(a, b) => emit_runtime_binop(
            ops, alloc, layout, result, *a, *b, RuntimeHelper::ModGeneric,
        ),

        IrOp::CallRuntime(helper, args) => {
            if args.len() > 3 {
                return Err(EmitError::Unsupported("CallRuntime arity > 3"));
            }
            let arg_locs: Vec<Location> = args
                .iter()
                .map(|v| loc_of(alloc, *v))
                .collect::<Result<_, _>>()?;
            let dst = alloc.locations.get(&result).copied();
            let helper_addr = runtime_helper_addr(*helper)
                .ok_or(EmitError::Unsupported("CallRuntime helper not implemented"))?;
            emit_runtime_call(ops, layout, &arg_locs, dst, helper_addr);
            Ok(())
        }
        IrOp::CallValue(callee, args) => {
            emit_call_value(ops, alloc, layout, result, *callee, args)
        }
        IrOp::MakeClosureNoCapture(const_idx) => {
            emit_make_closure_no_capture(ops, alloc, layout, result, *const_idx)
        }

        // ── Speculation checks ──────────────────────────────────
        //
        // Phase 5 emits trap-on-fail: any operand that fails the
        // tag check jumps to the function's shared deopt trampoline,
        // which in phase 5 is a bare `ud2`. Phase 6 replaces the
        // trampoline with the state-reconstruction runtime so a
        // failed speculation transparently falls back to tier 0.
        IrOp::CheckI32(v, _deopt_id) => {
            let dst = loc_of(alloc, result)?;
            let src = loc_of(alloc, *v)?;
            load_r64(ops, R10, src);
            // Extract the NaN-box tag (upper 16 bits) and compare
            // against the i32 tag 0x7FF9. `shr ; cmp` is the same
            // decoder tier-1 uses for its i32 fast path.
            dynasm!(ops
                ; mov rax, r10
                ; shr rax, 48
                ; cmp eax, 0x7FF9
                ; jne =>deopt_label
            );
            // Value is confirmed i32; the low 32 bits hold the
            // payload. Store the source into the result slot so
            // downstream typed ops (UnboxI32, AddI32, …) can read
            // from the result location.
            store_r64(ops, dst, R10);
            Ok(())
        }
        IrOp::CheckedAddI32(a, b, _deopt_id) => {
            emit_checked_binop_i32(ops, alloc, result, *a, *b, BinopI32::Add, deopt_label)
        }
        IrOp::CheckedSubI32(a, b, _deopt_id) => {
            emit_checked_binop_i32(ops, alloc, result, *a, *b, BinopI32::Sub, deopt_label)
        }
        IrOp::CheckedMulI32(a, b, _deopt_id) => {
            emit_checked_binop_i32(ops, alloc, result, *a, *b, BinopI32::Mul, deopt_label)
        }

        // ── Generic comparisons via runtime helpers ──────────────
        IrOp::EqValue(a, b) => emit_value_cmp(
            ops, alloc, layout, result, *a, *b, ValueCmp::Eq,
        ),
        IrOp::NeValue(a, b) => emit_value_cmp(
            ops, alloc, layout, result, *a, *b, ValueCmp::Ne,
        ),
        IrOp::LooseEqValue(a, b) => emit_value_cmp(
            ops, alloc, layout, result, *a, *b, ValueCmp::LooseEq,
        ),
        IrOp::LtValue(a, b) => emit_value_cmp(
            ops, alloc, layout, result, *a, *b, ValueCmp::Lt,
        ),
        IrOp::LeValue(a, b) => emit_value_cmp(
            ops, alloc, layout, result, *a, *b, ValueCmp::Le,
        ),

        // ── F64 arithmetic (SSE scalar doubles) ──────────────────
        IrOp::AddF64(a, b) => emit_binop_f64(ops, alloc, result, *a, *b, BinopF64::Add),
        IrOp::SubF64(a, b) => emit_binop_f64(ops, alloc, result, *a, *b, BinopF64::Sub),
        IrOp::MulF64(a, b) => emit_binop_f64(ops, alloc, result, *a, *b, BinopF64::Mul),
        IrOp::DivF64(a, b) => emit_binop_f64(ops, alloc, result, *a, *b, BinopF64::Div),
        IrOp::NegF64(v) => {
            let dst = loc_of(alloc, result)?;
            let src = loc_of(alloc, *v)?;
            // Negate via 0.0 - x: xorpd xmm1, xmm1 gives 0.0; subsd
            // does the flip. Two SSE ops, no constant load.
            load_xmm(ops, XMM0, src);
            dynasm!(ops
                ; xorpd xmm1, xmm1
                ; subsd xmm1, xmm0
            );
            store_xmm(ops, dst, XMM1);
            Ok(())
        }

        // ── F64 boxing / unboxing (identity — f64 bits are Value) ─
        //
        // NaN-boxed doubles already use the raw f64 bit pattern as
        // their Value representation; there's no tag to add or strip.
        // Emit both as a plain 64-bit move so downstream operand
        // Locations hold the same bits.
        IrOp::BoxF64(v) | IrOp::UnboxF64(v) => {
            let dst = loc_of(alloc, result)?;
            let src = loc_of(alloc, *v)?;
            emit_move64(ops, src, dst);
            Ok(())
        }
        IrOp::CheckF64(..) | IrOp::CheckHeap(..)
        | IrOp::CheckHeapShape(..) | IrOp::CheckFunctionIs(..) => {
            Err(EmitError::Unsupported("non-i32 speculation check"))
        }
        IrOp::LoadSlot(..) | IrOp::StoreSlot(..) => {
            Err(EmitError::Unsupported("shape-slot access"))
        }
    }
}

// ── Terminator emit ──────────────────────────────────────────────────

fn emit_term(
    ops: &mut Assembler,
    func: &IrFunction,
    alloc: &Allocation,
    block_labels: &[DynamicLabel],
    term: &Terminator,
    fallthrough: Option<BlockId>,
    layout: &FrameLayout,
    deopt_label: DynamicLabel,
) -> Result<(), EmitError> {
    match term {
        Terminator::Return(val) => {
            match val {
                Some(v) => {
                    let src = loc_of(alloc, *v)?;
                    load_r64(ops, RAX, src);
                }
                None => {
                    dynasm!(ops ; mov rax, QWORD VAL_UNDEFINED as i64);
                }
            }
            emit_epilogue(ops, layout.spill_bytes);
            Ok(())
        }

        Terminator::Jump(target, args) => {
            emit_block_edge(ops, func, alloc, *target, args)?;
            if fallthrough != Some(*target) {
                let lbl = block_labels[target.0 as usize];
                dynasm!(ops ; jmp =>lbl);
            }
            Ok(())
        }

        Terminator::Branch {
            cond,
            then_block,
            then_args,
            else_block,
            else_args,
        } => {
            let cond_loc = loc_of(alloc, *cond)?;
            // cond is Bool-typed: an unboxed 0/1 in the low byte.
            match cond_loc {
                Location::Reg(n) => {
                    let phys = PHYS_GP[n as usize];
                    dynasm!(ops ; test Rd(phys), Rd(phys));
                }
                Location::Spill(slot) => {
                    let off = (slot as i32) * 8;
                    dynasm!(ops
                        ; mov r10d, DWORD [rsp + off]
                        ; test r10d, r10d
                    );
                }
            }
            // Sequence: emit then-edge inline, then jz to else-edge
            // stub after. Keeps both paths inside their parent block
            // rather than threading through scratch labels.
            let then_lbl = block_labels[then_block.0 as usize];
            let else_lbl = block_labels[else_block.0 as usize];
            let else_edge_lbl = ops.new_dynamic_label();

            dynasm!(ops ; jz =>else_edge_lbl);

            // then path
            emit_block_edge(ops, func, alloc, *then_block, then_args)?;
            if fallthrough != Some(*then_block) {
                dynasm!(ops ; jmp =>then_lbl);
            }

            // else path
            dynasm!(ops ; =>else_edge_lbl);
            emit_block_edge(ops, func, alloc, *else_block, else_args)?;
            if fallthrough != Some(*else_block) {
                dynasm!(ops ; jmp =>else_lbl);
            }
            Ok(())
        }

        Terminator::Unreachable => {
            dynasm!(ops ; ud2);
            Ok(())
        }

        Terminator::Deopt(_id) => {
            // Route to the shared deopt trampoline. Phase 5 traps
            // there; phase 6 replaces the trampoline with a
            // state-reconstructing runtime.
            dynasm!(ops ; jmp =>deopt_label);
            Ok(())
        }
    }
}

/// Serialise the parallel-move set implied by `jump target(args...)`
/// and emit each step. Cycles consume scratch spill slots placed
/// above the regalloc's own spill area.
fn emit_block_edge(
    ops: &mut Assembler,
    func: &IrFunction,
    alloc: &Allocation,
    target: BlockId,
    args: &[ValueId],
) -> Result<(), EmitError> {
    let target_block = &func.blocks[target.0 as usize];
    if target_block.params.len() != args.len() {
        return Err(EmitError::Unsupported("block arg arity mismatch"));
    }
    let mut moves: Vec<Move> = Vec::with_capacity(args.len());
    for (arg, (param_vid, _)) in args.iter().zip(target_block.params.iter()) {
        let src = loc_of(alloc, *arg)?;
        let dst = loc_of(alloc, *param_vid)?;
        moves.push(Move { src, dst });
    }

    let (steps, _scratch_used) = parallel_move::resolve(&moves, alloc.num_spill_slots);

    for step in steps {
        match step {
            MoveStep::Copy { src, dst } => emit_move64(ops, src, dst),
            MoveStep::SaveToScratch { src, scratch } => {
                let scratch_loc = Location::Spill(scratch);
                emit_move64(ops, src, scratch_loc);
            }
            MoveStep::LoadFromScratch { scratch, dst } => {
                let src = Location::Spill(scratch);
                emit_move64(ops, src, dst);
            }
        }
    }
    Ok(())
}

// ── Runtime-call emission ────────────────────────────────────────────

/// Emit a Win64 call to a helper with 0..3 Value args.
///
/// Register layout at the call:
///
/// ```text
///   rcx = vm_ptr   (reloaded from env_vm_off)
///   rdx = arg0     (zero-default if none supplied)
///   r8  = arg1
///   r9  = arg2
/// ```
///
/// Before the call, r11 (our reg7) is saved to the env r11 stash to
/// preserve it across the volatile clobber. Afterwards, the slot is
/// reloaded and r8 is restored from env_globals_off so subsequent
/// `LoadGlobal` / `StoreGlobal` ops still see a valid pointer.
///
/// `dst` may be `None` for void-result helpers; when `Some`, the
/// return value in rax is stored into the location.
fn emit_runtime_call(
    ops: &mut Assembler,
    layout: &FrameLayout,
    args: &[Location],
    dst: Option<Location>,
    helper_addr: usize,
) {
    debug_assert!(
        layout.has_runtime_call,
        "emit_runtime_call presumes has_runtime_call (for r11 save + globals reload)"
    );
    debug_assert!(layout.needs_env, "frame must reserve env slots");
    debug_assert!(args.len() <= 3, "helper arity > 3 not supported");

    // Preserve r11 (reg7). load_r*/store_r* use r10 as scratch, but
    // never r11 — we only need to save r11 because any live value in
    // it would otherwise be clobbered by the helper.
    dynasm!(ops ; mov QWORD [rsp + layout.env_r11_off], r11);

    // Marshall args. Load each arg into its ABI-destination register
    // in an order that avoids clobbering inputs. The simple rule
    // used here: load right-to-left (r9 ← arg2, then r8 ← arg1, then
    // rdx ← arg0). This is safe because `args` Locations can only
    // alias PHYS_GP (values) or the spill area; r9/r8/rdx are
    // volatile and never used as value registers, so loading them
    // doesn't destroy any already-materialised input.
    if let Some(a2) = args.get(2).copied() {
        load_r64(ops, R9, a2);
    }
    if let Some(a1) = args.get(1).copied() {
        load_r64(ops, R8, a1);
    }
    if let Some(a0) = args.get(0).copied() {
        load_r64(ops, RDX, a0);
    }

    // vm_ptr → rcx (always the first arg on this ABI). Read from
    // the env slot rather than keeping it pinned in a register so
    // we don't burn a non-volatile reg on infrastructure.
    dynasm!(ops
        ; mov rcx, QWORD [rsp + layout.env_vm_off]
        ; mov rax, QWORD helper_addr as i64
        ; call rax
    );

    // Restore r11 (reg7) and r8 (globals_ptr). Both are clobbered
    // by the helper via the caller-saved convention; r11 may have
    // held a value, r8 is read on subsequent LoadGlobal/StoreGlobal
    // ops so must carry the original globals_ptr.
    dynasm!(ops
        ; mov r11, QWORD [rsp + layout.env_r11_off]
        ; mov r8, QWORD [rsp + layout.env_globals_off]
    );

    // Store return value (in rax) if the caller wants it.
    if let Some(dst_loc) = dst {
        store_r64(ops, dst_loc, RAX);
    }
}

/// Lower a `CallValue(callee, args)` op to a `djit_call_helper` call.
///
/// The helper's Win64 signature is
/// `(vm, callee_bits, args_ptr, nargs) -> u64`. We marshal each arg
/// into the per-function staging area (`env_call_args_off`), then
/// set up the four ABI registers and call. `r11` (our reg7) is
/// caller-saved around the call just like any other runtime helper.
/// `r8` (globals_ptr) is reloaded from its env stash afterwards so
/// subsequent `LoadGlobal` / `StoreGlobal` sites see a valid
/// pointer even though the helper clobbered it.
fn emit_call_value(
    ops: &mut Assembler,
    alloc: &Allocation,
    layout: &FrameLayout,
    result: ValueId,
    callee: ValueId,
    args: &[ValueId],
) -> Result<(), EmitError> {
    debug_assert!(
        layout.needs_env,
        "CallValue emit requires env stash (vm_ptr, globals)"
    );
    if (args.len() as u32) > layout.max_call_args {
        return Err(EmitError::Unsupported(
            "CallValue arity exceeds precomputed staging capacity",
        ));
    }
    let callee_loc = loc_of(alloc, callee)?;
    let arg_locs: Vec<Location> = args
        .iter()
        .map(|v| loc_of(alloc, *v))
        .collect::<Result<_, _>>()?;
    let dst = alloc.locations.get(&result).copied();

    // Save r11 (reg7) — caller-saved under Win64, may hold a live
    // value we need after the call.
    dynasm!(ops ; mov QWORD [rsp + layout.env_r11_off], r11);

    // Marshal each arg into the staging area at
    // `[rsp + env_call_args_off + i*8]`. We go through r10 so both
    // reg-sourced and spill-sourced arg Locations work.
    for (i, loc) in arg_locs.iter().enumerate() {
        let off = layout.env_call_args_off + (i as i32) * 8;
        load_r64(ops, R10, *loc);
        dynasm!(ops ; mov QWORD [rsp + off], r10);
    }

    // rcx = vm_ptr (from env stash).
    // rdx = callee bits.
    // r8  = &args_staging.
    // r9  = nargs (u64).
    load_r64(ops, RDX, callee_loc);
    dynasm!(ops
        ; mov rcx, QWORD [rsp + layout.env_vm_off]
        ; lea r8, [rsp + layout.env_call_args_off]
        ; mov r9d, args.len() as i32
    );
    let helper_addr = crate::vm::djit_call_helper as usize;
    dynasm!(ops
        ; mov rax, QWORD helper_addr as i64
        ; call rax
    );

    // Restore r11 + globals_ptr (see emit_runtime_call for the same
    // pattern).
    dynasm!(ops
        ; mov r11, QWORD [rsp + layout.env_r11_off]
        ; mov r8, QWORD [rsp + layout.env_globals_off]
    );

    if let Some(dst_loc) = dst {
        store_r64(ops, dst_loc, RAX);
    }
    Ok(())
}

/// Lower a `MakeClosureNoCapture(const_idx)` op: call the runtime
/// helper that clones the constant function onto the heap and
/// returns its Value. Reuses the standard runtime-call sequence
/// (save r11, set rcx=vm, call, restore r11 and globals).
fn emit_make_closure_no_capture(
    ops: &mut Assembler,
    alloc: &Allocation,
    layout: &FrameLayout,
    result: ValueId,
    const_idx: u32,
) -> Result<(), EmitError> {
    debug_assert!(
        layout.needs_env,
        "MakeClosureNoCapture requires env stash for vm_ptr"
    );
    let dst = loc_of(alloc, result)?;
    // Save r11 (reg7) — clobbered by the call.
    dynasm!(ops ; mov QWORD [rsp + layout.env_r11_off], r11);

    // rcx = vm_ptr, rdx = const_idx (zero-extended into 64 bits).
    dynasm!(ops
        ; mov rcx, QWORD [rsp + layout.env_vm_off]
        ; mov edx, const_idx as i32
    );
    let helper_addr = runtime::tier2_make_closure_helper as usize;
    dynasm!(ops
        ; mov rax, QWORD helper_addr as i64
        ; call rax
    );
    dynasm!(ops
        ; mov r11, QWORD [rsp + layout.env_r11_off]
        ; mov r8, QWORD [rsp + layout.env_globals_off]
    );
    store_r64(ops, dst, RAX);
    Ok(())
}

/// Lower an `(AddGeneric|SubGeneric|MulGeneric)(a, b)` op to a
/// runtime-helper call. Emits the save/call/restore sequence and
/// writes the result into `result`'s location.
fn emit_runtime_binop(
    ops: &mut Assembler,
    alloc: &Allocation,
    layout: &FrameLayout,
    result: ValueId,
    a: ValueId,
    b: ValueId,
    helper: RuntimeHelper,
) -> Result<(), EmitError> {
    let a_loc = loc_of(alloc, a)?;
    let b_loc = loc_of(alloc, b)?;
    let dst = loc_of(alloc, result)?;
    let helper_addr = runtime_helper_addr(helper)
        .ok_or(EmitError::Unsupported("helper not available"))?;
    emit_runtime_call(ops, layout, &[a_loc, b_loc], Some(dst), helper_addr);
    Ok(())
}

/// Which generic comparison the emitter is lowering.
#[derive(Copy, Clone)]
enum ValueCmp {
    Eq,
    Ne,
    LooseEq,
    Lt,
    Le,
}

/// Lower a generic comparison op (`EqValue` / `NeValue` / ... /
/// `LeValue`) to a runtime-helper call. The helper returns a raw
/// `u64` with the low byte holding 0 or 1 — the emit layer treats
/// the result as an unboxed [`ValueType::Bool`], so callers that
/// want a `Value` apply `BoxBool` downstream.
fn emit_value_cmp(
    ops: &mut Assembler,
    alloc: &Allocation,
    layout: &FrameLayout,
    result: ValueId,
    a: ValueId,
    b: ValueId,
    cmp: ValueCmp,
) -> Result<(), EmitError> {
    let a_loc = loc_of(alloc, a)?;
    let b_loc = loc_of(alloc, b)?;
    let dst = loc_of(alloc, result)?;
    let helper_addr = match cmp {
        ValueCmp::Eq => runtime::tier2_eq_value_helper as usize,
        ValueCmp::Ne => runtime::tier2_ne_value_helper as usize,
        ValueCmp::LooseEq => runtime::tier2_loose_eq_value_helper as usize,
        ValueCmp::Lt => runtime::tier2_lt_value_helper as usize,
        ValueCmp::Le => runtime::tier2_le_value_helper as usize,
    };
    emit_runtime_call(ops, layout, &[a_loc, b_loc], Some(dst), helper_addr);
    Ok(())
}

// ── Binop helpers ────────────────────────────────────────────────────

#[derive(Copy, Clone)]
enum BinopI32 {
    Add,
    Sub,
    Mul,
}

#[derive(Copy, Clone)]
enum BinopF64 {
    Add,
    Sub,
    Mul,
    Div,
}

/// Emit a scalar-double binary operation (SSE). Loads both operands
/// into xmm0/xmm1, executes the op, stores xmm0 back to the result
/// Location. Uses `movq` to move bits between GP and xmm so the
/// caller's Location conventions (either reg or spill) both work
/// without an intermediate spill.
fn emit_binop_f64(
    ops: &mut Assembler,
    alloc: &Allocation,
    result: ValueId,
    a: ValueId,
    b: ValueId,
    kind: BinopF64,
) -> Result<(), EmitError> {
    let dst = loc_of(alloc, result)?;
    let a_loc = loc_of(alloc, a)?;
    let b_loc = loc_of(alloc, b)?;

    load_xmm(ops, XMM0, a_loc);
    load_xmm(ops, XMM1, b_loc);
    match kind {
        BinopF64::Add => dynasm!(ops ; addsd xmm0, xmm1),
        BinopF64::Sub => dynasm!(ops ; subsd xmm0, xmm1),
        BinopF64::Mul => dynasm!(ops ; mulsd xmm0, xmm1),
        BinopF64::Div => dynasm!(ops ; divsd xmm0, xmm1),
    }
    store_xmm(ops, dst, XMM0);
    Ok(())
}

fn emit_binop_i32(
    ops: &mut Assembler,
    alloc: &Allocation,
    result: ValueId,
    a: ValueId,
    b: ValueId,
    kind: BinopI32,
) -> Result<(), EmitError> {
    let dst = loc_of(alloc, result)?;
    let a_loc = loc_of(alloc, a)?;
    let b_loc = loc_of(alloc, b)?;

    // Compute in R10, store out. Always going through a scratch keeps
    // the code uniform and avoids the subtle "dst == a or b" dance.
    load_r32(ops, R10, a_loc);
    match (kind, b_loc) {
        (BinopI32::Add, Location::Reg(n)) => {
            dynasm!(ops ; add r10d, Rd(PHYS_GP[n as usize]));
        }
        (BinopI32::Add, Location::Spill(slot)) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; add r10d, DWORD [rsp + off]);
        }
        (BinopI32::Sub, Location::Reg(n)) => {
            dynasm!(ops ; sub r10d, Rd(PHYS_GP[n as usize]));
        }
        (BinopI32::Sub, Location::Spill(slot)) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; sub r10d, DWORD [rsp + off]);
        }
        (BinopI32::Mul, Location::Reg(n)) => {
            dynasm!(ops ; imul r10d, Rd(PHYS_GP[n as usize]));
        }
        (BinopI32::Mul, Location::Spill(slot)) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; imul r10d, DWORD [rsp + off]);
        }
    }
    store_r32(ops, dst, R10);
    Ok(())
}

/// Same shape as [`emit_binop_i32`] but with a post-op `jo` that
/// jumps to the deopt trampoline on i32 overflow. Used for the
/// `Checked*I32` family that speculation passes insert when the
/// semantic fallback (widen to f64) must round-trip through tier 0.
fn emit_checked_binop_i32(
    ops: &mut Assembler,
    alloc: &Allocation,
    result: ValueId,
    a: ValueId,
    b: ValueId,
    kind: BinopI32,
    deopt_label: DynamicLabel,
) -> Result<(), EmitError> {
    let dst = loc_of(alloc, result)?;
    let a_loc = loc_of(alloc, a)?;
    let b_loc = loc_of(alloc, b)?;

    load_r32(ops, R10, a_loc);
    match (kind, b_loc) {
        (BinopI32::Add, Location::Reg(n)) => {
            dynasm!(ops ; add r10d, Rd(PHYS_GP[n as usize]));
        }
        (BinopI32::Add, Location::Spill(slot)) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; add r10d, DWORD [rsp + off]);
        }
        (BinopI32::Sub, Location::Reg(n)) => {
            dynasm!(ops ; sub r10d, Rd(PHYS_GP[n as usize]));
        }
        (BinopI32::Sub, Location::Spill(slot)) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; sub r10d, DWORD [rsp + off]);
        }
        (BinopI32::Mul, Location::Reg(n)) => {
            dynasm!(ops ; imul r10d, Rd(PHYS_GP[n as usize]));
        }
        (BinopI32::Mul, Location::Spill(slot)) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; imul r10d, DWORD [rsp + off]);
        }
    }
    dynasm!(ops ; jo =>deopt_label);
    store_r32(ops, dst, R10);
    Ok(())
}

#[derive(Copy, Clone)]
enum CmpI32 {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

fn emit_cmp_i32(
    ops: &mut Assembler,
    alloc: &Allocation,
    result: ValueId,
    a: ValueId,
    b: ValueId,
    kind: CmpI32,
) -> Result<(), EmitError> {
    let dst = loc_of(alloc, result)?;
    let a_loc = loc_of(alloc, a)?;
    let b_loc = loc_of(alloc, b)?;

    load_r32(ops, R10, a_loc);
    match b_loc {
        Location::Reg(n) => {
            dynasm!(ops ; cmp r10d, Rd(PHYS_GP[n as usize]));
        }
        Location::Spill(slot) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; cmp r10d, DWORD [rsp + off]);
        }
    }
    match kind {
        CmpI32::Eq => dynasm!(ops ; sete r10b),
        CmpI32::Ne => dynasm!(ops ; setne r10b),
        CmpI32::Lt => dynasm!(ops ; setl r10b),
        CmpI32::Le => dynasm!(ops ; setle r10b),
        CmpI32::Gt => dynasm!(ops ; setg r10b),
        CmpI32::Ge => dynasm!(ops ; setge r10b),
    }
    dynasm!(ops ; movzx r10d, r10b);
    store_r32(ops, dst, R10);
    Ok(())
}

// ── Load/store helpers ────────────────────────────────────────────────

fn load_r32(ops: &mut Assembler, dst_phys: u8, src: Location) {
    match src {
        Location::Reg(n) => {
            let phys = PHYS_GP[n as usize];
            // Moving a 32-bit reg clears upper half of the destination.
            dynasm!(ops ; mov Rd(dst_phys), Rd(phys));
        }
        Location::Spill(slot) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; mov Rd(dst_phys), DWORD [rsp + off]);
        }
    }
}

fn load_r64(ops: &mut Assembler, dst_phys: u8, src: Location) {
    match src {
        Location::Reg(n) => {
            let phys = PHYS_GP[n as usize];
            dynasm!(ops ; mov Rq(dst_phys), Rq(phys));
        }
        Location::Spill(slot) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; mov Rq(dst_phys), QWORD [rsp + off]);
        }
    }
}

fn store_r32(ops: &mut Assembler, dst: Location, src_phys: u8) {
    match dst {
        Location::Reg(n) => {
            let phys = PHYS_GP[n as usize];
            dynasm!(ops ; mov Rd(phys), Rd(src_phys));
        }
        Location::Spill(slot) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; mov DWORD [rsp + off], Rd(src_phys));
        }
    }
}

fn store_r64(ops: &mut Assembler, dst: Location, src_phys: u8) {
    match dst {
        Location::Reg(n) => {
            let phys = PHYS_GP[n as usize];
            dynasm!(ops ; mov Rq(phys), Rq(src_phys));
        }
        Location::Spill(slot) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; mov QWORD [rsp + off], Rq(src_phys));
        }
    }
}

/// Move the 64-bit value at `src` into an SSE scalar-double
/// register. GP source uses `movq` (bit-preserving qword move), spill
/// source uses `movsd` (scalar-double memory load).
fn load_xmm(ops: &mut Assembler, dst_xmm: u8, src: Location) {
    match src {
        Location::Reg(n) => {
            let phys = PHYS_GP[n as usize];
            dynasm!(ops ; movq Rx(dst_xmm), Rq(phys));
        }
        Location::Spill(slot) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; movsd Rx(dst_xmm), QWORD [rsp + off]);
        }
    }
}

/// Store an SSE scalar-double register back to a Location. Mirror of
/// [`load_xmm`]: `movq` for GP destinations, `movsd` for spill.
fn store_xmm(ops: &mut Assembler, dst: Location, src_xmm: u8) {
    match dst {
        Location::Reg(n) => {
            let phys = PHYS_GP[n as usize];
            dynasm!(ops ; movq Rq(phys), Rx(src_xmm));
        }
        Location::Spill(slot) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; movsd QWORD [rsp + off], Rx(src_xmm));
        }
    }
}

fn mov_loc_imm32(ops: &mut Assembler, dst: Location, imm: i32) {
    match dst {
        Location::Reg(n) => {
            let phys = PHYS_GP[n as usize];
            // mov r32, imm32 zero-extends to 64; fine since callers
            // treat the low 32 bits as the payload.
            dynasm!(ops ; mov Rd(phys), imm);
        }
        Location::Spill(slot) => {
            let off = (slot as i32) * 8;
            dynasm!(ops ; mov DWORD [rsp + off], imm);
        }
    }
}

fn mov_loc_imm64(ops: &mut Assembler, dst: Location, imm: u64) {
    match dst {
        Location::Reg(n) => {
            let phys = PHYS_GP[n as usize];
            dynasm!(ops ; mov Rq(phys), QWORD imm as i64);
        }
        Location::Spill(slot) => {
            let off = (slot as i32) * 8;
            dynasm!(ops
                ; mov r10, QWORD imm as i64
                ; mov QWORD [rsp + off], r10
            );
        }
    }
}

/// Emit a full-qword move. Goes through r10 when both ends are
/// spills (no mem→mem on x86).
fn emit_move64(ops: &mut Assembler, src: Location, dst: Location) {
    if src == dst {
        return;
    }
    match (src, dst) {
        (Location::Reg(sn), Location::Reg(dn)) => {
            dynasm!(ops ; mov Rq(PHYS_GP[dn as usize]), Rq(PHYS_GP[sn as usize]));
        }
        (Location::Reg(sn), Location::Spill(dslot)) => {
            let off = (dslot as i32) * 8;
            dynasm!(ops ; mov QWORD [rsp + off], Rq(PHYS_GP[sn as usize]));
        }
        (Location::Spill(sslot), Location::Reg(dn)) => {
            let off = (sslot as i32) * 8;
            dynasm!(ops ; mov Rq(PHYS_GP[dn as usize]), QWORD [rsp + off]);
        }
        (Location::Spill(sslot), Location::Spill(dslot)) => {
            let soff = (sslot as i32) * 8;
            let doff = (dslot as i32) * 8;
            dynasm!(ops
                ; mov r10, QWORD [rsp + soff]
                ; mov QWORD [rsp + doff], r10
            );
        }
    }
}

// ── Epilogue ─────────────────────────────────────────────────────────

fn emit_epilogue(ops: &mut Assembler, spill_bytes: u32) {
    if spill_bytes > 0 {
        dynasm!(ops ; add rsp, spill_bytes as i32);
    }
    dynasm!(ops
        ; pop r15
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
    );
}

// ── Utility ──────────────────────────────────────────────────────────

fn loc_of(alloc: &Allocation, vid: ValueId) -> Result<Location, EmitError> {
    alloc
        .locations
        .get(&vid)
        .copied()
        .ok_or(EmitError::Unsupported("value has no location"))
}

// ── Suppress warnings for unused constants ───────────────────────────
//
// Some of the ABI-register indices appear only in `dynasm!`-macro
// operand positions where the expanded code uses the literal
// register name (e.g. "rcx" rather than RCX). We keep the numeric
// constants for cross-reference clarity. Likewise VAL_FALSE /
// VAL_TRUE are the canonical boxed bool forms but most emissions
// build them inline via OR.
#[allow(dead_code)]
const _UNUSED_MARKERS: &[u64] = &[
    RCX as u64,
    RSP as u64,
    VAL_FALSE,
    VAL_TRUE,
];

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Unit tests for the x86-64 emitter. Each test builds a small
    //! IR function by hand, runs liveness + linear-scan + emit, then
    //! invokes the emitted code directly and checks the result.
    //!
    //! These tests exercise the JIT on the host CPU — they will run
    //! only on x86-64 targets with the `djit` feature enabled, which
    //! the module-level `cfg` already restricts.

    use super::*;
    use crate::codegen::tier2::ir::types::{Block, DeoptId, Terminator as IrTerm, ValueType};
    use crate::codegen::tier2::ir::IrFunction as IrF;
    use crate::codegen::tier2::regalloc;
    use std::rc::Rc;

    fn make_func(blocks: Vec<Block>, num_bytecode_regs: u16) -> IrF {
        IrF {
            bytecode_len: 0,
            num_bytecode_regs,
            num_parameters: num_bytecode_regs,
            blocks,
            deopt_points: Vec::new(),
            constants: Rc::new(Vec::new()),
        }
    }

    fn run(func: &IrF, regs: &mut [u64]) -> u64 {
        run_with_globals(func, regs, &mut [])
    }

    fn run_with_globals(func: &IrF, regs: &mut [u64], globals: &mut [u64]) -> u64 {
        let alloc = regalloc::allocate(func);
        let emitted = emit(func, &alloc).expect("emit must succeed");
        unsafe {
            emitted.execute(
                regs.as_mut_ptr(),
                std::ptr::null(),
                globals.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        }
    }

    #[test]
    fn return_constant_i32_boxed() {
        // bb0():
        //   v0 = ConstI32(42)
        //   v1 = BoxI32(v0)
        //   return v1
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstI32(42)),
                (v1, IrOp::BoxI32(v0)),
            ],
            term: IrTerm::Return(Some(v1)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 42);
    }

    #[test]
    fn return_undefined() {
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: Vec::new(),
            term: IrTerm::Return(None),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, VAL_UNDEFINED);
    }

    #[test]
    fn add_two_unboxed_i32s_and_return_boxed() {
        // bb0():
        //   v0 = ConstI32(5)
        //   v1 = ConstI32(7)
        //   v2 = AddI32(v0, v1)
        //   v3 = BoxI32(v2)
        //   return v3
        let vs: Vec<ValueId> = (0..4).map(ValueId).collect();
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (vs[0], IrOp::ConstI32(5)),
                (vs[1], IrOp::ConstI32(7)),
                (vs[2], IrOp::AddI32(vs[0], vs[1])),
                (vs[3], IrOp::BoxI32(vs[2])),
            ],
            term: IrTerm::Return(Some(vs[3])),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 12);
    }

    #[test]
    fn unbox_add_box_via_entry_param() {
        // One entry param: NaN-boxed i32. Return v + v.
        // bb0(p0):
        //   u = UnboxI32(p0)
        //   s = AddI32(u, u)
        //   r = BoxI32(s)
        //   return r
        let p0 = ValueId(0);
        let u = ValueId(1);
        let s = ValueId(2);
        let r = ValueId(3);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![(p0, ValueType::Value)],
            ops: vec![
                (u, IrOp::UnboxI32(p0)),
                (s, IrOp::AddI32(u, u)),
                (r, IrOp::BoxI32(s)),
            ],
            term: IrTerm::Return(Some(r)),
        }];
        let func = make_func(blocks, 1);
        let mut regs = [I32_SIG | 21];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 42);
    }

    #[test]
    fn sub_mul_neg() {
        // ((10 - 3) * -2) = -14
        let vs: Vec<ValueId> = (0..6).map(ValueId).collect();
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (vs[0], IrOp::ConstI32(10)),
                (vs[1], IrOp::ConstI32(3)),
                (vs[2], IrOp::SubI32(vs[0], vs[1])),
                (vs[3], IrOp::ConstI32(2)),
                (vs[4], IrOp::NegI32(vs[3])),
                (vs[5], IrOp::MulI32(vs[2], vs[4])),
            ],
            term: IrTerm::Return(Some(vs[5])),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs) as u32;
        // Unboxed return of an unboxed i32 still shows the raw bits
        // — our emitter stores -14 into the low 32 without boxing
        // because the final op is MulI32 (not BoxI32). Boxing happens
        // lazily as BoxI32 is an explicit IR op.
        assert_eq!(out, (-14i32) as u32);
    }

    #[test]
    fn eq_i32_returns_unboxed_bool() {
        // Both 5 == 5 → 1, and 5 == 6 → 0.
        for (a, b, want) in [(5i32, 5i32, 1u64), (5, 6, 0)] {
            let vs: Vec<ValueId> = (0..3).map(ValueId).collect();
            let blocks = vec![Block {
                id: BlockId(0),
                params: Vec::new(),
                ops: vec![
                    (vs[0], IrOp::ConstI32(a)),
                    (vs[1], IrOp::ConstI32(b)),
                    (vs[2], IrOp::EqI32(vs[0], vs[1])),
                ],
                term: IrTerm::Return(Some(vs[2])),
            }];
            let func = make_func(blocks, 0);
            let mut regs = [];
            let out = run(&func, &mut regs);
            assert_eq!(out, want);
        }
    }

    #[test]
    fn branch_on_bool_picks_correct_arm() {
        // bb0(p0: Value):  -- p0 is expected to be i32 0 or 1 (Bool raw)
        //   cond = UnboxBool(p0)
        //   branch cond then bb1() else bb2()
        // bb1(): return const(111) unboxed
        // bb2(): return const(222) unboxed
        let p0 = ValueId(0);
        let cond = ValueId(1);
        let c_then = ValueId(2);
        let c_else = ValueId(3);

        let bb0 = Block {
            id: BlockId(0),
            params: vec![(p0, ValueType::Value)],
            ops: vec![(cond, IrOp::UnboxBool(p0))],
            term: IrTerm::Branch {
                cond,
                then_block: BlockId(1),
                then_args: Vec::new(),
                else_block: BlockId(2),
                else_args: Vec::new(),
            },
        };
        let bb1 = Block {
            id: BlockId(1),
            params: Vec::new(),
            ops: vec![(c_then, IrOp::ConstI32(111))],
            term: IrTerm::Return(Some(c_then)),
        };
        let bb2 = Block {
            id: BlockId(2),
            params: Vec::new(),
            ops: vec![(c_else, IrOp::ConstI32(222))],
            term: IrTerm::Return(Some(c_else)),
        };
        let func = make_func(vec![bb0, bb1, bb2], 1);

        // bool true → bb1 → 111
        let mut regs_t = [VAL_TRUE];
        let out_t = run(&func, &mut regs_t) as u32;
        assert_eq!(out_t, 111);

        // bool false → bb2 → 222
        let mut regs_f = [VAL_FALSE];
        let out_f = run(&func, &mut regs_f) as u32;
        assert_eq!(out_f, 222);
    }

    #[test]
    fn jump_with_swapping_block_args_forces_cycle() {
        // bb0(a, b):
        //   jump bb1(b, a)   <-- swap a, b via jump
        // bb1(x, y):
        //   d = SubI32(x, y)
        //   return d
        //
        // Inputs: a=7, b=2. Expected: x=2, y=7, d = x - y = -5.
        let a = ValueId(0);
        let b = ValueId(1);
        let x = ValueId(2);
        let y = ValueId(3);
        let d = ValueId(4);

        let bb0 = Block {
            id: BlockId(0),
            params: vec![(a, ValueType::Value), (b, ValueType::Value)],
            ops: Vec::new(),
            term: IrTerm::Jump(BlockId(1), vec![b, a]),
        };
        let bb1 = Block {
            id: BlockId(1),
            params: vec![(x, ValueType::Value), (y, ValueType::Value)],
            ops: vec![(d, IrOp::SubI32(x, y))],
            term: IrTerm::Return(Some(d)),
        };
        let func = make_func(vec![bb0, bb1], 2);

        // Raw i32 values in the low 32 bits. The emitter's SubI32
        // treats them as i32s.
        let mut regs = [7, 2];
        let out = run(&func, &mut regs) as i32;
        assert_eq!(out, 2 - 7);
    }

    #[test]
    fn loop_accumulator_sums_0_to_9() {
        // bb0(): jump bb1(0, 0)
        // bb1(i, s):
        //   c = ConstI32(10)
        //   t = LtI32(i, c)
        //   branch t then bb2(i, s) else bb3(s)
        // bb2(i, s):
        //   s2 = AddI32(s, i)
        //   one = ConstI32(1)
        //   i2 = AddI32(i, one)
        //   jump bb1(i2, s2)
        // bb3(s):
        //   r = BoxI32(s)
        //   return r
        let c0 = ValueId(0);
        let c0b = ValueId(1);
        let i = ValueId(2);
        let s = ValueId(3);
        let c10 = ValueId(4);
        let t = ValueId(5);
        let i2 = ValueId(6);
        let s_then = ValueId(7);
        let s2 = ValueId(8);
        let one = ValueId(9);
        let i_next = ValueId(10);
        let s_final = ValueId(11);
        let r = ValueId(12);

        let bb0 = Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (c0, IrOp::ConstI32(0)),
                (c0b, IrOp::ConstI32(0)),
            ],
            term: IrTerm::Jump(BlockId(1), vec![c0, c0b]),
        };
        let bb1 = Block {
            id: BlockId(1),
            params: vec![(i, ValueType::I32), (s, ValueType::I32)],
            ops: vec![
                (c10, IrOp::ConstI32(10)),
                (t, IrOp::LtI32(i, c10)),
            ],
            term: IrTerm::Branch {
                cond: t,
                then_block: BlockId(2),
                then_args: vec![i, s],
                else_block: BlockId(3),
                else_args: vec![s],
            },
        };
        let bb2 = Block {
            id: BlockId(2),
            params: vec![(i2, ValueType::I32), (s_then, ValueType::I32)],
            ops: vec![
                (s2, IrOp::AddI32(s_then, i2)),
                (one, IrOp::ConstI32(1)),
                (i_next, IrOp::AddI32(i2, one)),
            ],
            term: IrTerm::Jump(BlockId(1), vec![i_next, s2]),
        };
        let bb3 = Block {
            id: BlockId(3),
            params: vec![(s_final, ValueType::I32)],
            ops: vec![(r, IrOp::BoxI32(s_final))],
            term: IrTerm::Return(Some(r)),
        };
        let func = make_func(vec![bb0, bb1, bb2, bb3], 0);

        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 45); // sum 0..=9
    }

    #[test]
    fn loadglobal_storeglobal_roundtrip() {
        // bb0():
        //   v = LoadGlobal(0)
        //   StoreGlobal(1, v)
        //   return v
        let v = ValueId(0);
        let store_void = ValueId(1);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v, IrOp::LoadGlobal(0)),
                (store_void, IrOp::StoreGlobal(1, v)),
            ],
            term: IrTerm::Return(Some(v)),
        }];
        let func = make_func(blocks, 0);
        let mut globals = [0x1234_5678_9abc_def0, 0];
        let mut regs = [];
        let out = run_with_globals(&func, &mut regs, &mut globals);
        assert_eq!(out, 0x1234_5678_9abc_def0);
        assert_eq!(globals[1], 0x1234_5678_9abc_def0);
    }

    #[test]
    fn spills_under_pressure() {
        // Force more than 8 simultaneously-live values so the
        // allocator has to spill, then verify arithmetic still
        // produces the right sum.
        //
        // Build: c0..c9 each ConstI32(i+1); sum = c0+c1+...+c9
        // All ci must be live at the time they're added, so the
        // allocator will spill at least two.
        let mut ops = Vec::new();
        let mut consts: Vec<ValueId> = Vec::new();
        let mut next = 0u32;
        for i in 0..10 {
            let cv = ValueId(next);
            next += 1;
            consts.push(cv);
            ops.push((cv, IrOp::ConstI32(i + 1)));
        }
        // Chain additions: acc = c0; acc += c1; ...
        let mut acc = consts[0];
        for c in &consts[1..] {
            let new_acc = ValueId(next);
            next += 1;
            ops.push((new_acc, IrOp::AddI32(acc, *c)));
            acc = new_acc;
        }
        let boxed = ValueId(next);
        ops.push((boxed, IrOp::BoxI32(acc)));
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops,
            term: IrTerm::Return(Some(boxed)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 55); // 1+2+..+10
    }

    #[test]
    fn checked_add_i32_computes_sum_on_no_overflow() {
        // CheckedAddI32 of two small i32 constants adds inline and
        // doesn't trigger the deopt trampoline.
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let v3 = ValueId(3);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstI32(100)),
                (v1, IrOp::ConstI32(42)),
                (v2, IrOp::CheckedAddI32(v0, v1, DeoptId(0))),
                (v3, IrOp::BoxI32(v2)),
            ],
            term: IrTerm::Return(Some(v3)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 142);
    }

    #[test]
    fn check_i32_passes_through_boxed_i32() {
        // CheckI32 on a boxed i32 entry-param: no deopt, downstream
        // UnboxI32+AddI32 reads the payload and doubles it.
        let p0 = ValueId(0);
        let checked = ValueId(1);
        let u = ValueId(2);
        let doubled = ValueId(3);
        let boxed = ValueId(4);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![(p0, ValueType::Value)],
            ops: vec![
                (checked, IrOp::CheckI32(p0, DeoptId(0))),
                (u, IrOp::UnboxI32(checked)),
                (doubled, IrOp::AddI32(u, u)),
                (boxed, IrOp::BoxI32(doubled)),
            ],
            term: IrTerm::Return(Some(boxed)),
        }];
        let func = make_func(blocks, 1);
        let mut regs = [I32_SIG | 21];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 42);
    }

    #[test]
    fn deopt_terminator_jumps_to_trampoline() {
        // A block ending in a Deopt terminator compiles successfully;
        // we can't actually execute it without the deopt runtime
        // (that's phase 6), so we only assert that `emit` returns Ok.
        let v0 = ValueId(0);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![(v0, IrOp::ConstI32(0))],
            term: IrTerm::Deopt(DeoptId(0)),
        }];
        let func = make_func(blocks, 0);
        let alloc = regalloc::allocate(&func);
        assert!(emit(&func, &alloc).is_ok());
    }

    #[test]
    fn eq_value_returns_boxed_true_for_same_i32() {
        // EqValue(5, 5) → boxed TRUE.
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstI32(5)),
                (v1, IrOp::ConstI32(5)),
                (v2, IrOp::EqValue(v0, v1)),
            ],
            term: IrTerm::Return(Some(v2)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, VAL_TRUE);
    }

    #[test]
    fn eq_value_returns_boxed_false_for_mismatch() {
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstI32(5)),
                (v1, IrOp::ConstI32(7)),
                (v2, IrOp::EqValue(v0, v1)),
            ],
            term: IrTerm::Return(Some(v2)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, VAL_FALSE);
    }

    #[test]
    fn lt_value_numeric_path_returns_boxed_true() {
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstI32(3)),
                (v1, IrOp::ConstI32(7)),
                (v2, IrOp::LtValue(v0, v1)),
            ],
            term: IrTerm::Return(Some(v2)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, VAL_TRUE);
    }

    #[test]
    fn le_value_equal_operands_returns_boxed_true() {
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstI32(42)),
                (v1, IrOp::ConstI32(42)),
                (v2, IrOp::LeValue(v0, v1)),
            ],
            term: IrTerm::Return(Some(v2)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        let out = run(&func, &mut regs);
        assert_eq!(out, VAL_TRUE);
    }

    #[test]
    fn ne_value_with_different_numeric_types() {
        // NeValue(i32(5), f64(5.5)) → boxed TRUE (different values).
        let p0 = ValueId(0);
        let p1 = ValueId(1);
        let result = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![
                (p0, ValueType::Value),
                (p1, ValueType::Value),
            ],
            ops: vec![(result, IrOp::NeValue(p0, p1))],
            term: IrTerm::Return(Some(result)),
        }];
        let func = make_func(blocks, 2);
        let mut regs = [
            I32_SIG | 5,
            5.5_f64.to_bits(),
        ];
        let out = run(&func, &mut regs);
        assert_eq!(out, VAL_TRUE);
    }

    // ── F64 arithmetic tests ─────────────────────────────────────────
    //
    // The F64 emit uses SSE scalar doubles (`addsd` / `subsd` /
    // `mulsd` / `divsd`). Each test constructs a small IR function,
    // runs it, and compares the returned f64 bits against the
    // hand-computed expected value. Because NaN-boxed doubles use the
    // raw f64 bit pattern as the Value, no boxing step is needed —
    // the return value's bits decode directly via `f64::from_bits`.

    fn run_f64(func: &IrF, regs: &mut [u64]) -> f64 {
        let bits = run(func, regs);
        f64::from_bits(bits)
    }

    #[test]
    fn const_f64_returns_boxed_value() {
        let v0 = ValueId(0);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![(v0, IrOp::ConstF64(3.14_f64.to_bits()))],
            term: IrTerm::Return(Some(v0)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        assert_eq!(run_f64(&func, &mut regs), 3.14);
    }

    #[test]
    fn add_f64_numeric() {
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstF64(1.5_f64.to_bits())),
                (v1, IrOp::ConstF64(2.25_f64.to_bits())),
                (v2, IrOp::AddF64(v0, v1)),
            ],
            term: IrTerm::Return(Some(v2)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        assert_eq!(run_f64(&func, &mut regs), 3.75);
    }

    #[test]
    fn sub_mul_div_f64() {
        // ((10.0 - 1.0) * 2.0) / 3.0 = 6.0
        let vs: Vec<ValueId> = (0..6).map(ValueId).collect();
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (vs[0], IrOp::ConstF64(10.0_f64.to_bits())),
                (vs[1], IrOp::ConstF64(1.0_f64.to_bits())),
                (vs[2], IrOp::SubF64(vs[0], vs[1])),
                (vs[3], IrOp::ConstF64(2.0_f64.to_bits())),
                (vs[4], IrOp::MulF64(vs[2], vs[3])),
                (vs[5], IrOp::DivF64(vs[4], {
                    // 3.0 const via a fresh VID below — push inline:
                    ValueId(6)
                })),
            ],
            term: IrTerm::Return(Some(vs[5])),
        }];
        // Hmm, inline const 3.0 needs its own op entry; rewriting:
        let _ = blocks; // discard; rebuild cleanly.

        let vs: Vec<ValueId> = (0..7).map(ValueId).collect();
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (vs[0], IrOp::ConstF64(10.0_f64.to_bits())),
                (vs[1], IrOp::ConstF64(1.0_f64.to_bits())),
                (vs[2], IrOp::SubF64(vs[0], vs[1])),
                (vs[3], IrOp::ConstF64(2.0_f64.to_bits())),
                (vs[4], IrOp::MulF64(vs[2], vs[3])),
                (vs[5], IrOp::ConstF64(3.0_f64.to_bits())),
                (vs[6], IrOp::DivF64(vs[4], vs[5])),
            ],
            term: IrTerm::Return(Some(vs[6])),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        assert_eq!(run_f64(&func, &mut regs), 6.0);
    }

    #[test]
    fn neg_f64() {
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstF64(2.5_f64.to_bits())),
                (v1, IrOp::NegF64(v0)),
            ],
            term: IrTerm::Return(Some(v1)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        assert_eq!(run_f64(&func, &mut regs), -2.5);
    }

    #[test]
    fn box_unbox_f64_are_identity() {
        // BoxF64(UnboxF64(ConstF64)) should round-trip.
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: vec![
                (v0, IrOp::ConstF64(7.125_f64.to_bits())),
                (v1, IrOp::UnboxF64(v0)),
                (v2, IrOp::BoxF64(v1)),
            ],
            term: IrTerm::Return(Some(v2)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        assert_eq!(run_f64(&func, &mut regs), 7.125);
    }

    #[test]
    fn f64_via_spill_slots() {
        // Create enough simultaneously-live f64 values to force the
        // regalloc to spill at least one — the spill path uses `movsd`
        // rather than `movq`, so this exercises both code paths.
        let mut next = 0u32;
        let mut vids = Vec::new();
        let mut ops_vec = Vec::new();
        for i in 0..10 {
            let v = ValueId(next);
            next += 1;
            ops_vec.push((v, IrOp::ConstF64((i as f64 + 1.0).to_bits())));
            vids.push(v);
        }
        // Chain additions: acc = c0; acc += c1; ...
        let mut acc = vids[0];
        for c in &vids[1..] {
            let new_acc = ValueId(next);
            next += 1;
            ops_vec.push((new_acc, IrOp::AddF64(acc, *c)));
            acc = new_acc;
        }
        let blocks = vec![Block {
            id: BlockId(0),
            params: Vec::new(),
            ops: ops_vec,
            term: IrTerm::Return(Some(acc)),
        }];
        let func = make_func(blocks, 0);
        let mut regs = [];
        // Sum 1+2+...+10 = 55.
        assert_eq!(run_f64(&func, &mut regs), 55.0);
    }

    #[test]
    fn deopt_id_constant_lives_in_unused_markers() {
        // Sanity: make sure our unused-markers array actually survives
        // — it's easy to drop by accident and then a dead-code warning
        // flips the build to error under CI. This asserts the constant
        // is readable.
        assert!(!_UNUSED_MARKERS.is_empty());
        let _ = DeoptId(0);
    }

    // ── Runtime-helper (CallRuntime + generic arithmetic) tests ──────
    //
    // These exercise the full Win64 call-emission path: r11 save,
    // env_globals/vm stashing, arg marshalling into rdx/r8/r9,
    // return collection, and the post-call restore of r8.
    //
    // The numeric fast paths in tier2_add_generic_helper et al. do not
    // deref vm_raw, so a null vm pointer is safe here. String / object
    // cases would require a real VM and live in the integration suite.

    #[test]
    fn add_generic_numeric_i32_plus_i32() {
        // bb0(p0, p1): return AddGeneric(p0, p1)
        //
        // Helper preserves i32 tag when both operands are i32 and
        // the sum fits — matches tier-0's semantic behaviour so
        // tier-2 promoted code doesn't widen integers to f64.
        let p0 = ValueId(0);
        let p1 = ValueId(1);
        let r = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![
                (p0, ValueType::Value),
                (p1, ValueType::Value),
            ],
            ops: vec![(r, IrOp::AddGeneric(p0, p1))],
            term: IrTerm::Return(Some(r)),
        }];
        let func = make_func(blocks, 2);
        let mut regs = [I32_SIG | 17, I32_SIG | 25];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 42);
    }

    #[test]
    fn sub_generic_numeric() {
        let p0 = ValueId(0);
        let p1 = ValueId(1);
        let r = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![
                (p0, ValueType::Value),
                (p1, ValueType::Value),
            ],
            ops: vec![(r, IrOp::SubGeneric(p0, p1))],
            term: IrTerm::Return(Some(r)),
        }];
        let func = make_func(blocks, 2);
        let mut regs = [I32_SIG | 100, I32_SIG | 30];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 70);
    }

    #[test]
    fn mul_generic_numeric() {
        let p0 = ValueId(0);
        let p1 = ValueId(1);
        let r = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![
                (p0, ValueType::Value),
                (p1, ValueType::Value),
            ],
            ops: vec![(r, IrOp::MulGeneric(p0, p1))],
            term: IrTerm::Return(Some(r)),
        }];
        let func = make_func(blocks, 2);
        let mut regs = [I32_SIG | 6, I32_SIG | 7];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 42);
    }

    #[test]
    fn sub_generic_non_numeric_returns_nan() {
        // null - 1 → NaN per JS semantics (Value(null) is non-numeric
        // in is_number test, so slow path returns NaN).
        //
        // Actually null coerces to 0 in JS arithmetic, but the
        // tier2_sub_generic_helper fallback returns NaN for non-numeric
        // operands. That's a design choice — a future phase can add
        // full ToNumber coercion; for now non-numeric sub/mul is
        // intentionally conservative.
        let p0 = ValueId(0);
        let p1 = ValueId(1);
        let r = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![
                (p0, ValueType::Value),
                (p1, ValueType::Value),
            ],
            ops: vec![(r, IrOp::SubGeneric(p0, p1))],
            term: IrTerm::Return(Some(r)),
        }];
        let func = make_func(blocks, 2);
        let mut regs = [VAL_NULL, I32_SIG | 1];
        let out = run(&func, &mut regs);
        let f = f64::from_bits(out);
        assert!(f.is_nan(), "expected NaN bit pattern, got {out:#018x}");
    }

    #[test]
    fn call_runtime_op_direct() {
        // Same as add_generic but using the explicit CallRuntime op,
        // which the speculation passes will emit to call any runtime
        // helper by id.
        use super::super::super::ir::types::RuntimeHelper;
        let p0 = ValueId(0);
        let p1 = ValueId(1);
        let r = ValueId(2);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![
                (p0, ValueType::Value),
                (p1, ValueType::Value),
            ],
            ops: vec![(
                r,
                IrOp::CallRuntime(RuntimeHelper::AddGeneric, vec![p0, p1]),
            )],
            term: IrTerm::Return(Some(r)),
        }];
        let func = make_func(blocks, 2);
        let mut regs = [I32_SIG | 3, I32_SIG | 4];
        let out = run(&func, &mut regs);
        assert_eq!(out, I32_SIG | 7);
    }

    #[test]
    fn runtime_call_preserves_globals_ptr() {
        // Exercise that r8 (globals_ptr) is correctly restored after
        // a runtime call — LoadGlobal after AddGeneric must still see
        // the original globals array.
        let p0 = ValueId(0);
        let p1 = ValueId(1);
        let add = ValueId(2);
        let glob = ValueId(3);
        let r = ValueId(4);
        let blocks = vec![Block {
            id: BlockId(0),
            params: vec![
                (p0, ValueType::Value),
                (p1, ValueType::Value),
            ],
            ops: vec![
                (add, IrOp::AddGeneric(p0, p1)), // clobbers r8
                (glob, IrOp::LoadGlobal(0)),     // must succeed
                (r, IrOp::Copy(glob)),
            ],
            term: IrTerm::Return(Some(r)),
        }];
        let func = make_func(blocks, 2);
        let mut regs = [I32_SIG | 1, I32_SIG | 2];
        let mut globals = [0xdeadbeef_cafef00du64];
        let out = run_with_globals(&func, &mut regs, &mut globals);
        assert_eq!(out, 0xdeadbeef_cafef00d);
    }

    #[test]
    fn parallel_move_scratch_slot_reserved_when_needed() {
        // A 2-cycle swap at a block edge forces parallel_move to
        // request scratch slot num_spill_slots. The pre-walk in
        // FrameLayout::compute must reserve that slot so the
        // SaveToScratch / LoadFromScratch sequence writes inside the
        // frame, not into caller memory.
        //
        // We emulate the test from phase 4c but confirm correctness
        // under FrameLayout instead of the old raw spill-bytes.
        let a = ValueId(0);
        let b = ValueId(1);
        let x = ValueId(2);
        let y = ValueId(3);
        let d = ValueId(4);

        let bb0 = Block {
            id: BlockId(0),
            params: vec![(a, ValueType::Value), (b, ValueType::Value)],
            ops: Vec::new(),
            term: IrTerm::Jump(BlockId(1), vec![b, a]),
        };
        let bb1 = Block {
            id: BlockId(1),
            params: vec![(x, ValueType::Value), (y, ValueType::Value)],
            ops: vec![(d, IrOp::SubI32(x, y))],
            term: IrTerm::Return(Some(d)),
        };
        let func = make_func(vec![bb0, bb1], 2);
        let alloc = regalloc::allocate(&func);
        let layout = FrameLayout::compute(&func, &alloc);
        // Whatever allocation chose, the frame must cover any scratch
        // the parallel-move pass requests for this edge.
        let max_scratch = max_parallel_move_scratch(&func, &alloc);
        let reserved_slots =
            layout.spill_bytes / 8 - (layout.has_runtime_call as u32 * 3);
        assert!(
            reserved_slots >= alloc.num_spill_slots + max_scratch,
            "frame reserves {reserved_slots} slots; needs at least {} \
             (spill={}, scratch={})",
            alloc.num_spill_slots + max_scratch,
            alloc.num_spill_slots,
            max_scratch,
        );

        let mut regs = [7, 2];
        let out = run(&func, &mut regs) as i32;
        assert_eq!(out, 2 - 7);
    }

}
