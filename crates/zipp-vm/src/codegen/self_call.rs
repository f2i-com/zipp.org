// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// DEOPT sentinel the self-call helper returns when it can't run the recursion
/// natively (depth limit, non-int arg, or callee not int-JIT'd). On seeing it,
/// the native code bails to the interpreter at this Call's ip so the call is
/// retried through the normal interpreter path. Chosen as a quiet-NaN tag value
/// no real `Value` produces (it is NOT a valid boxed Value).
pub const SELF_CALL_DEOPT: u64 = 0x7FFE_DEAD_BEEF_0000;

/// THREW sentinel the region call helpers (`jit_call_method_ic` / `jit_call_ic`)
/// return when the frame-called function THREW: the call's side effects already
/// happened and `pending_throw` is set, so the region must exit and the
/// interpreter must UNWIND (never re-execute the call op). Distinct from
/// `SELF_CALL_DEOPT`, which means "nothing happened yet — redo in the
/// interpreter". Like the deopt sentinel, a quiet-NaN pattern no boxed `Value`
/// produces.
pub const CALL_THREW: u64 = 0x7FFE_DEAD_BEEF_0001;

/// Completed allocation-free `SetIndexConcat` write. Unlike zero (the generic
/// helper-success result), this proves no VM heap allocation, GC safe point, or
/// user-code re-entry occurred, so a region may keep its pinned heap/IC bases
/// and TypedArray snapshots. Another impossible quiet-NaN Value pattern.
pub const CONCAT_SET_PURE: u64 = 0x7FFE_DEAD_BEEF_0003;

/// Sentinel the `*_prop_miss` helpers return when the access resolves to
/// something only the interpreter's per-site IC machinery can serve — an
/// ACCESSOR that must frame-call user code, or a CLASS-INSTANCE receiver
/// (method/getter/setter on the class chain). The region then calls the
/// `jit_get_prop_slow` / `jit_set_prop_slow` helper (which consults
/// `ic_get_prop`/`ic_set_prop` and frame-calls plain getters/setters), and
/// re-derives its pinned r13/r14 afterwards (user code may have run). Nothing
/// has happened yet when this is returned — the slow helper (or, after ITS
/// deopt, the interpreter) performs the access.
pub const PROP_VIA_IC: u64 = 0x7FFE_DEAD_BEEF_0002;

/// Emit a self-recursive call: `regs[dst] = self(regs[arg_base..arg_base+argc])`.
///
/// The args already sit contiguously in this frame's register window (the
/// compiler stages them there), so we pass `args_ptr = rbx + arg_base*8`
/// directly — no marshaling. Win64 call: rcx=vm, rdx=func_id, r8=args_ptr,
/// r9=argc. The helper (vm.rs `jit_self_call`) does the depth-guarded recursion
/// and returns the result Value bits, or `SELF_CALL_DEOPT` to bail. rbx/rsi/rdi
/// are callee-saved so they survive the call; 32B shadow space was reserved in
/// the prologue (rsp stays 16-aligned: prologue did 3 pushes + sub 40 = 64B).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_self_call(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    self_entry: dynasmrt::DynamicLabel,
    func_id: u32,
    helper: usize,
    dst: u16,
    callee: u16,
    arg_base: u16,
    argc: u16,
    reg_count: u16,
    self_val_bits: u64,
) {
    let depth_off = crate::vm::JIT_RECURSE_DEPTH_OFFSET as i32;
    let max = crate::vm::JIT_SELF_RECURSE_MAX_PUB as i32;
    let slow = ops.new_dynamic_label();
    let store = ops.new_dynamic_label();
    // Pack func_id (low 24b) + argc (high 8b) for the slow-path helper. Function
    // ids and arities are far below 2^24 / 2^8, so this is lossless.
    let packed = (func_id & 0x00FF_FFFF) | ((argc as u32) << 24);

    // ── FAST PATH ── depth guard, then a direct native call to our own entry.
    // The callee window is CONTIGUOUS at rbx + reg_count*8 (the register file is
    // pinned to a fixed capacity and never reallocates; get/set index it by raw
    // pointer, so writing past the live `len` is sound — the memory is allocated
    // and the callee defs every reg before reading). Depth bounds the native
    // stack; the depth limit (256) × reg_count is far below the reserved
    // capacity (max_window × MAX_FRAMES), so the window can't overflow the buffer.
    dynasm!(ops
        ; mov eax, [rdi + depth_off]
        ; cmp eax, max
        ; jae => slow                        // at the limit → Rust trampoline
        ; lea r11, [rbx + dreg(reg_count)]   // r11 = callee regs base
        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
        ; mov [r11], rax                     // callee reg0 = this = undefined
    );
    for i in 0..argc as i32 {
        dynasm!(ops
            ; mov rax, [rbx + dreg(arg_base) + i * 8]
            ; mov [r11 + (1 + i) * 8], rax   // callee reg(1+i) = arg i
        );
    }
    dynasm!(ops
        ; inc DWORD [rdi + depth_off]        // depth += 1
        ; mov rcx, r11                       // rcx = callee regs base
        ; lea rdx, [rsp + 32]                // rdx = callee's bail slot (our frame)
        ; mov DWORD [rsp + 32], NO_BAIL as i32
        ; mov r8, rdi                        // r8 = vm
        ; call => self_entry                 // win64 ABI; result bits → rax
        ; dec DWORD [rdi + depth_off]        // depth -= 1
        ; mov r10d, [rsp + 32]               // callee resume ip
        ; cmp r10d, NO_BAIL as i32
        ; je => store                        // clean native return → store result
        // The callee bailed mid-body. We must NOT unwind here (that would drop
        // depth to 0 and let the top-level interpreter re-enter native →
        // livelock). Instead fall through to the Rust finisher, which re-runs
        // this whole activation on the interpreter with depth held elevated.
    );

    // ── SLOW / FINISH PATH ── the Rust helper `jit_self_call_at` runs the
    // recursion (or the bailed continuation) with full bail-recovery + MAX_FRAMES
    // → RangeError handling, holding depth elevated so JIT re-entry can't
    // livelock. It needs the caller window base EXPLICITLY (rbx) since the fast
    // path tracks the window by raw pointer, not `regs.len()`. ABI: rcx=vm,
    // rdx=caller_base_ptr, r8=args_ptr, r9d=(func_id | argc<<24). rbx/rsi/rdi are
    // callee-saved across the call.
    dynasm!(ops
        ; => slow
        // Restore the callee register the hot path SKIPPED (the elided
        // `LoadGlobal(self_slot)`): every exit from here that reaches the
        // interpreter (`=> bail`) re-executes this Call op, which reads
        // `regs[callee]`. Writing it only on the cold path keeps the hot path
        // free of the per-call self-Value store.
        ; mov rax, QWORD self_val_bits as i64
        ; mov [rbx + dreg(callee)], rax
        ; mov rcx, rdi                       // vm
        ; mov rdx, rbx                        // caller window base
        ; lea r8, [rbx + dreg(arg_base)]     // args_ptr
        ; mov r9d, packed as i32             // func_id | argc<<24
        ; mov rax, QWORD helper as i64
        ; call rax
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => bail                         // helper deopted → redo in interp
        ; => store
        ; mov [rbx + dreg(dst)], rax
    );
    emit_bail(ops, ip, bail);
}

// ════════════════════════════════════════════════════════════════════════════
// Fused array-kernel JIT (map / reduce)
//
// `arr.map(cb)` / `arr.reduce(cb)` normally compile `cb` once (the leaf JIT)
// and call it per element through a reused register window
// (`invoke_cb_windowed`). Correct, and the per-element setup is call-free, but
// each element still pays a win64 call + the callback's prologue/epilogue — V8
// INLINES the callback body into the loop and pays zero. These kernels close
// that gap: a native loop iterates the array snapshot and inlines the callback
// body per element. No per-element call.
//
// SAFETY MODEL — same correctness-first stance as the leaf JIT:
// * `can_kernel_body` accepts ONLY pure-arithmetic callbacks (no calls,
//   globals, heap ops, branches, `%`, or non-int constants), so the kernel is
//   CALL-FREE and `self.regs`/the snapshot/the output buffer can't reallocate
//   under the live pointers during a run.
// * Arithmetic is f64/SSE (reusing the region's `load_num_xmm`/`store_xmm`), so
//   a kernel handles BOTH int-tagged and double elements — loop-built numeric
//   arrays hold doubles (the building loop ran in the SSE region). A non-number
//   operand jumps to the shared bail.
// * A bail does NOT abort the whole operation: the kernel returns the element
//   index `i` it bailed at, having committed results for `[0, i)`. The Rust
//   driver finishes `[i, len)` through the ordinary per-element path (which
//   handles strings/objects/etc. correctly). So a numeric array gets the full
//   inlined win, and a mixed array merely runs the tail the slow way — same
//   answer as node.
//
// map ABI:    `fn(window, snapshot, len, out) -> usize` (count written).
// reduce ABI: `fn(window, snapshot, count, acc_inout) -> usize`; `acc_inout`
//             is the seed in / the accumulated value out; `count` elements from
//             `snapshot` (the driver shifts the base past any seed element).
// `window` is the callback register frame: reg 0 = this, reg 1 = element (map)
// or accumulator (reduce), reg 2 = index (map) or element (reduce).

