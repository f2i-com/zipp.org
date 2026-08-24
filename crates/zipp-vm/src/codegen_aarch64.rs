//! Native AArch64 baseline JIT.
//!
//! This backend deliberately starts with the same correctness shape as ZIPP's
//! first x86-64 tier: it accepts only call-free integer bytecode, guards every
//! typed input, and returns the exact bytecode ip to the interpreter on a guard
//! miss or arithmetic result that cannot stay in the tagged-i32 representation.
//! This first landing compiles whole functions only. Hot numeric functions may
//! contain loops, but there is deliberately no separate ARM OSR/helper/regex
//! tier until those surfaces have native-ARM differential coverage.
//!
//! The mature x86-64 backend still owns the wider register-homed, helper-call,
//! IC, reducer, and cross-call tiers. Keeping this first ARM64 slice small makes
//! its fallback proof auditable and gives later tiers a working ABI and
//! executable-memory path to grow from.

#![cfg(all(feature = "jit", target_arch = "aarch64"))]

use std::mem;

use dynasmrt::{dynasm, AssemblyOffset, DynamicLabel, DynasmApi, DynasmLabelApi};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::bytecode::{FuncProto, Instr};

#[cfg(not(target_os = "windows"))]
use dynasmrt::ExecutableBuffer;
#[cfg(target_os = "windows")]
use windows_exec::ExecutableBuffer;

// dynasmrt 5.1's executable assembler synchronizes every non-macOS AArch64
// mapping with raw `mrs ctr_el0`/`dc cvau`/`ic ivau` instructions. Windows
// ARM64 does not permit that EL0 cache-maintenance path, so `commit()` raises
// STATUS_ILLEGAL_INSTRUCTION before generated code can run. Assemble into an
// ordinary Vec on Windows and use the supported Win32 W^X + cache-flush path.
#[cfg(not(target_os = "windows"))]
type ArmAssembler = dynasmrt::aarch64::Assembler;
#[cfg(target_os = "windows")]
type ArmAssembler = dynasmrt::VecAssembler<dynasmrt::aarch64::Aarch64Relocation>;

#[cfg(target_os = "windows")]
mod windows_exec {
    use std::io;
    use std::ptr::NonNull;

    use dynasmrt::AssemblyOffset;

    use core::ffi::c_void;

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_READWRITE: u32 = 0x04;
    const PAGE_EXECUTE_READ: u32 = 0x20;

    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualAlloc(
            address: *mut c_void,
            size: usize,
            allocation_type: u32,
            protect: u32,
        ) -> *mut c_void;
        fn VirtualProtect(
            address: *mut c_void,
            size: usize,
            new_protect: u32,
            old_protect: *mut u32,
        ) -> i32;
        fn FlushInstructionCache(
            process: *mut c_void,
            base_address: *const c_void,
            size: usize,
        ) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
    }

    /// An immutable Windows executable allocation. Construction is strictly
    /// RW -> RX; no writable/executable mapping exists at the same time.
    pub(super) struct ExecutableBuffer {
        ptr: NonNull<u8>,
        len: usize,
    }

    impl ExecutableBuffer {
        pub(super) fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
            if bytes.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot execute an empty ARM64 buffer",
                ));
            }

            let raw = unsafe {
                VirtualAlloc(
                    core::ptr::null_mut(),
                    bytes.len(),
                    MEM_RESERVE | MEM_COMMIT,
                    PAGE_READWRITE,
                )
            };
            let ptr = NonNull::new(raw.cast::<u8>()).ok_or_else(io::Error::last_os_error)?;

            unsafe {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.as_ptr(), bytes.len());
            }

            let mut old_protect = 0;
            if unsafe {
                VirtualProtect(
                    ptr.as_ptr().cast::<c_void>(),
                    bytes.len(),
                    PAGE_EXECUTE_READ,
                    &mut old_protect,
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                unsafe {
                    VirtualFree(ptr.as_ptr().cast::<c_void>(), 0, MEM_RELEASE);
                }
                return Err(error);
            }

            if unsafe {
                FlushInstructionCache(
                    GetCurrentProcess(),
                    ptr.as_ptr().cast::<c_void>(),
                    bytes.len(),
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                unsafe {
                    VirtualFree(ptr.as_ptr().cast::<c_void>(), 0, MEM_RELEASE);
                }
                return Err(error);
            }

            Ok(Self {
                ptr,
                len: bytes.len(),
            })
        }

        #[inline]
        pub(super) fn ptr(&self, offset: AssemblyOffset) -> *const u8 {
            assert!(offset.0 < self.len, "executable offset is out of bounds");
            unsafe { self.ptr.as_ptr().add(offset.0) }
        }

        #[inline]
        pub(super) fn len(&self) -> usize {
            self.len
        }
    }

    impl Drop for ExecutableBuffer {
        fn drop(&mut self) {
            unsafe {
                VirtualFree(self.ptr.as_ptr().cast::<c_void>(), 0, MEM_RELEASE);
            }
        }
    }
}

pub const JIT_THRESHOLD: u32 = 8;
pub const DEOPT_LIMIT: u32 = 64;
pub const NO_BAIL: u32 = u32::MAX;

pub const FN_COLD: u8 = 0;
pub const FN_COMPILED: u8 = 1;
pub const FN_DEAD: u8 = 2;

const INT_TAG_HI: u32 = 0x7ff9;
const BOOL_TAG_HI: u32 = 0x7ffa;
const UNDEFINED_TAG_HI: u32 = 0x7ffc;
/// AArch64 conditional/compare-and-branch instructions have a ±1 MiB range.
/// Bail stubs sit after the emitted body, so cap one compilation unit well below
/// that distance even for the widest accepted instruction expansion.
const MAX_COMPILED_OPS: usize = 4096;
/// Bound per-VM emitted code bytes even for a program containing many
/// individually-small hot functions. Page-rounded mapping overhead is bounded
/// separately by the retained-allocation count below. The sandbox disables
/// native codegen, but embedders should not need that fact to keep the baseline
/// cache finite.
const MAX_CODE_CACHE_BYTES: usize = 16 * 1024 * 1024;
/// Executable allocators round even tiny bodies to page-sized mappings. Bound
/// allocation count as well as emitted bytes so page overhead cannot turn many
/// tiny hot functions into an unexpectedly large native-code footprint.
const MAX_COMPILED_FUNCTIONS: usize = 4096;

#[inline]
pub(crate) const fn hole_absent_fast_enabled() -> bool {
    true
}

#[inline]
pub(crate) const fn hole_undef_enabled() -> bool {
    true
}

#[inline]
pub(crate) const fn forin_arr_own_enabled() -> bool {
    true
}

#[inline]
pub(crate) const fn forin_version_fast_enabled() -> bool {
    true
}

/// One immutable executable allocation. Generated code uses the platform C ABI:
/// `(regs: *mut u64, bail_ip: *mut u32, vm: *mut c_void) -> u64`.
/// The current ARM64 subset makes no calls and ignores `vm`; keeping the third
/// parameter preserves the native-entry contract used by the x86 tier.
pub struct JitFn {
    _buf: ExecutableBuffer,
    entry: *const u8,
    code_bytes: usize,
}

impl JitFn {
    /// # Safety
    ///
    /// `regs` must address the compiled function's full register window and the
    /// executable buffer must outlive this call.
    pub unsafe fn run(&self, regs: *mut u64, vm: *mut core::ffi::c_void) -> (u64, u32) {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Jit);
        let f: extern "C" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
            mem::transmute(self.entry);
        let mut bail = NO_BAIL;
        let result = f(regs, &mut bail, vm);
        (result, bail)
    }
}

#[derive(Default)]
pub struct Jit {
    threshold_override: u32,
    counts: FxHashMap<u32, u32>,
    compiled: FxHashMap<u32, JitFn>,
    blacklist: FxHashSet<u32>,
    fn_state: Vec<u8>,
    code_bytes: usize,
    deopts: FxHashMap<u32, u32>,
}

impl Jit {
    pub fn new() -> Self {
        let mut jit = Self::default();
        jit.threshold_override = std::env::var("ZIPP_JIT_THRESHOLD")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(0);
        jit
    }

    #[inline]
    fn function_threshold(&self) -> u32 {
        if self.threshold_override == 0 {
            JIT_THRESHOLD
        } else {
            self.threshold_override
        }
    }

    #[inline]
    pub fn get(&self, func_id: u32) -> Option<&JitFn> {
        self.compiled.get(&func_id)
    }

    #[inline]
    pub fn fn_state(&self, func_id: u32) -> u8 {
        self.fn_state
            .get(func_id as usize)
            .copied()
            .unwrap_or(FN_COLD)
    }

    fn set_fn_state(&mut self, func_id: u32, state: u8) {
        let i = func_id as usize;
        if self.fn_state.len() <= i {
            self.fn_state.resize(i + 1, FN_COLD);
        }
        self.fn_state[i] = state;
    }

    pub fn record_and_should_compile(&mut self, func_id: u32) -> bool {
        if self.compiled.contains_key(&func_id) || self.blacklist.contains(&func_id) {
            return false;
        }
        let threshold = self.function_threshold();
        let count = self.counts.entry(func_id).or_insert(0);
        *count = count.saturating_add(1);
        *count == threshold
    }

    pub fn compile(&mut self, func_id: u32, proto: &FuncProto) {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::JitCompile);
        match compile_function(proto) {
            Some(code)
                if self.compiled.len() < MAX_COMPILED_FUNCTIONS
                    && self.code_bytes.saturating_add(code.code_bytes) <= MAX_CODE_CACHE_BYTES =>
            {
                self.code_bytes += code.code_bytes;
                self.compiled.insert(func_id, code);
                self.set_fn_state(func_id, FN_COMPILED);
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!(
                        "[jit] ARM64 fn{func_id} compiled (call-free whole-function baseline, {} ops)",
                        proto.code.len()
                    );
                }
            }
            _ => {
                self.blacklist.insert(func_id);
                self.set_fn_state(func_id, FN_DEAD);
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] ARM64 fn{func_id} DECLINED (unsupported or code-cache limit)");
                }
            }
        }
    }

    /// Retire a chronically-polymorphic body after repeated guard/overflow
    /// exits. Occasional misses age back down after a clean native return.
    pub fn note_bail(&mut self, func_id: u32, bail: u32) {
        if bail == NO_BAIL {
            if let Some(n) = self.deopts.get_mut(&func_id) {
                *n = n.saturating_sub(1);
            }
            return;
        }
        let n = self.deopts.entry(func_id).or_insert(0);
        *n = n.saturating_add(1);
        if *n < DEOPT_LIMIT {
            return;
        }
        self.deopts.remove(&func_id);
        if let Some(code) = self.compiled.remove(&func_id) {
            self.code_bytes = self.code_bytes.saturating_sub(code.code_bytes);
        }
        self.blacklist.insert(func_id);
        self.set_fn_state(func_id, FN_DEAD);
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] ARM64 fn{func_id} EVICTED (chronic guard exits)");
        }
    }

    #[cfg(test)]
    pub(crate) fn compiled_count(&self) -> usize {
        self.compiled.len()
    }
}

fn compile_function(proto: &FuncProto) -> Option<JitFn> {
    if proto.code.is_empty() || proto.rest_reg.is_some() {
        return None;
    }
    compile_function_body(proto)
}

fn compile_function_body(proto: &FuncProto) -> Option<JitFn> {
    let start = 0usize;
    let end = proto.code.len().checked_sub(1)?;
    if proto.code.len() > MAX_COMPILED_OPS {
        return None;
    }
    // Scaled `ldr/str` immediates cover 0..32760 bytes. Keeping the first tier
    // inside that range avoids a hidden scratch-address path in every access.
    if proto.reg_count == 0 || proto.reg_count as usize > 4096 {
        return None;
    }
    if !matches!(
        proto.code[end],
        Instr::Return { .. } | Instr::ReturnUndefined | Instr::Jump { .. }
    ) {
        return None;
    }
    for instr in &proto.code {
        if !supported(instr, proto.reg_count, proto.code.len()) {
            return None;
        }
    }

    #[cfg(not(target_os = "windows"))]
    let mut ops = ArmAssembler::new().ok()?;
    // Base zero is correct for this call-free backend: every emitted label
    // relocation is relative within the same buffer. Revisit this if an ARM64
    // tier ever starts embedding external/absolute call targets.
    #[cfg(target_os = "windows")]
    let mut ops = ArmAssembler::new(0);
    let labels: Vec<DynamicLabel> = (start..=end).map(|_| ops.new_dynamic_label()).collect();
    let bails: Vec<DynamicLabel> = (start..=end).map(|_| ops.new_dynamic_label()).collect();

    // x9 = register-window base; x10 = bail/resume pointer. x11/w11 carries
    // the Int tag, x12 the boxed Int tag, x13 the boxed Bool tag, and w14 the
    // Bool tag discriminator. All are caller-saved under AAPCS64/Windows ARM64.
    dynasm!(ops
        ; .arch aarch64
        ; mov x9, x0
        ; mov x10, x1
        ; movz w11, INT_TAG_HI
        ; movz x12, INT_TAG_HI, LSL 48
        ; movz x13, BOOL_TAG_HI, LSL 48
        ; movz w14, BOOL_TAG_HI
    );

    for ip in start..=end {
        let label = labels[ip - start];
        let bail = bails[ip - start];
        dynasm!(ops ; .arch aarch64 ; =>label);
        emit_instr(&mut ops, &proto.code[ip], &labels, bail)?;
    }

    // Preflight requires a terminal op. Retain a fail-closed exact-ip bailout
    // in case malformed control flow ever reaches the physical end anyway.
    emit_resume(&mut ops, end as u32);

    for ip in start..=end {
        let bail = bails[ip - start];
        dynasm!(ops ; .arch aarch64 ; =>bail);
        emit_resume(&mut ops, ip as u32);
    }

    // dynasmrt's `finalize` panics when its implicit final commit discovers an
    // impossible relocation. Commit explicitly so any future branch-range
    // drift fails closed as a normal JIT decline instead.
    ops.commit().ok()?;
    #[cfg(not(target_os = "windows"))]
    let buf = ops.finalize().ok()?;
    #[cfg(target_os = "windows")]
    let buf = ExecutableBuffer::from_bytes(&ops.finalize().ok()?).ok()?;
    let entry = buf.ptr(AssemblyOffset(0));
    let code_bytes = buf.len();
    Some(JitFn {
        _buf: buf,
        entry,
        code_bytes,
    })
}

fn supported(instr: &Instr, reg_count: u16, code_len: usize) -> bool {
    let reg = |r: u16| r < reg_count;
    let target = |t: u32| (t as usize) < code_len;
    match *instr {
        Instr::LoadInt { dst, .. } | Instr::LoadUndefined { dst } | Instr::LoadBool { dst, .. } => {
            reg(dst)
        }
        Instr::Move { dst, src } => reg(dst) && reg(src),
        Instr::AddInt { dst, a, .. } | Instr::Neg { dst, a } => reg(dst) && reg(a),
        Instr::Add { dst, a, b }
        | Instr::Sub { dst, a, b }
        | Instr::Mul { dst, a, b }
        | Instr::Lt { dst, a, b }
        | Instr::Le { dst, a, b }
        | Instr::Gt { dst, a, b }
        | Instr::Ge { dst, a, b }
        | Instr::Eq { dst, a, b }
        | Instr::Ne { dst, a, b } => reg(dst) && reg(a) && reg(b),
        Instr::Jump { target: t } => target(t),
        Instr::JumpIfFalse { cond, target: t } | Instr::JumpIfTrue { cond, target: t } => {
            reg(cond) && target(t)
        }
        Instr::JumpIfNotLt { a, b, target: t } | Instr::JumpIfNotLe { a, b, target: t } => {
            reg(a) && reg(b) && target(t)
        }
        Instr::Return { src } => reg(src),
        Instr::ReturnUndefined => true,
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_instr(
    ops: &mut ArmAssembler,
    instr: &Instr,
    labels: &[DynamicLabel],
    bail: DynamicLabel,
) -> Option<()> {
    match *instr {
        Instr::LoadInt { dst, val } => {
            emit_u32(ops, 3, val as u32);
            dynasm!(ops ; .arch aarch64 ; orr x3, x3, x12);
            emit_store(ops, dst, 3);
        }
        Instr::LoadUndefined { dst } => {
            dynasm!(ops ; .arch aarch64 ; movz x3, UNDEFINED_TAG_HI, LSL 48);
            emit_store(ops, dst, 3);
        }
        Instr::LoadBool { dst, val } => {
            emit_u32(ops, 3, u32::from(val));
            dynasm!(ops ; .arch aarch64 ; orr x3, x3, x13);
            emit_store(ops, dst, 3);
        }
        Instr::Move { dst, src } => {
            emit_load_raw(ops, src, 3);
            emit_store(ops, dst, 3);
        }
        Instr::AddInt { dst, a, imm, .. } => {
            emit_load_int(ops, a, 3, bail);
            emit_u32(ops, 4, imm as u32);
            dynasm!(ops
                ; .arch aarch64
                ; adds w5, w3, w4
                ; b.vs =>bail
                ; orr x5, x5, x12
            );
            emit_store(ops, dst, 5);
        }
        Instr::Neg { dst, a } => {
            emit_load_int(ops, a, 3, bail);
            // `-0` must stay a double and i32::MIN cannot be negated as an Int.
            dynasm!(ops
                ; .arch aarch64
                ; cbz w3, =>bail
                ; negs w5, w3
                ; b.vs =>bail
                ; orr x5, x5, x12
            );
            emit_store(ops, dst, 5);
        }
        Instr::Add { dst, a, b } | Instr::Sub { dst, a, b } => {
            emit_load_int(ops, a, 3, bail);
            emit_load_int(ops, b, 4, bail);
            if matches!(instr, Instr::Add { .. }) {
                dynasm!(ops ; .arch aarch64 ; adds w5, w3, w4);
            } else {
                dynasm!(ops ; .arch aarch64 ; subs w5, w3, w4);
            }
            dynasm!(ops
                ; .arch aarch64
                ; b.vs =>bail
                ; orr x5, x5, x12
            );
            emit_store(ops, dst, 5);
        }
        Instr::Mul { dst, a, b } => {
            emit_load_int(ops, a, 3, bail);
            emit_load_int(ops, b, 4, bail);
            let nonzero = ops.new_dynamic_label();
            dynasm!(ops
                ; .arch aarch64
                ; smull x5, w3, w4
                ; sxtw x6, w5
                ; cmp x5, x6
                ; b.ne =>bail
                ; cbnz w5, =>nonzero
                ; eor w6, w3, w4
                // TB(N)Z reaches only +/-32 KiB. Keep that branch local, then
                // use the ordinary +/-128 MiB `b` for the per-ip bail stub,
                // which lives after the complete emitted function body.
                ; tbz w6, 31, =>nonzero
                ; b =>bail
                ; =>nonzero
                ; mov w5, w5
                ; orr x5, x5, x12
            );
            emit_store(ops, dst, 5);
        }
        Instr::Lt { dst, a, b }
        | Instr::Le { dst, a, b }
        | Instr::Gt { dst, a, b }
        | Instr::Ge { dst, a, b }
        | Instr::Eq { dst, a, b }
        | Instr::Ne { dst, a, b } => {
            emit_load_int(ops, a, 3, bail);
            emit_load_int(ops, b, 4, bail);
            dynasm!(ops ; .arch aarch64 ; cmp w3, w4);
            match instr {
                Instr::Lt { .. } => dynasm!(ops ; .arch aarch64 ; cset w5, lt),
                Instr::Le { .. } => dynasm!(ops ; .arch aarch64 ; cset w5, le),
                Instr::Gt { .. } => dynasm!(ops ; .arch aarch64 ; cset w5, gt),
                Instr::Ge { .. } => dynasm!(ops ; .arch aarch64 ; cset w5, ge),
                Instr::Eq { .. } => dynasm!(ops ; .arch aarch64 ; cset w5, eq),
                Instr::Ne { .. } => dynasm!(ops ; .arch aarch64 ; cset w5, ne),
                _ => unreachable!(),
            }
            dynasm!(ops ; .arch aarch64 ; orr x5, x5, x13);
            emit_store(ops, dst, 5);
        }
        Instr::Jump { target } => emit_jump(ops, target, labels),
        Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
            emit_load_bool(ops, cond, 3, bail);
            let jump_if_true = matches!(instr, Instr::JumpIfTrue { .. });
            emit_cond_jump(ops, target, labels, jump_if_true);
        }
        Instr::JumpIfNotLt { a, b, target } | Instr::JumpIfNotLe { a, b, target } => {
            emit_load_int(ops, a, 3, bail);
            emit_load_int(ops, b, 4, bail);
            dynasm!(ops ; .arch aarch64 ; cmp w3, w4);
            let not_le = matches!(instr, Instr::JumpIfNotLe { .. });
            emit_compare_exit(ops, target, labels, not_le);
        }
        Instr::Return { src } => {
            emit_load_raw(ops, src, 0);
            emit_no_bail(ops);
            dynasm!(ops ; .arch aarch64 ; ret);
        }
        Instr::ReturnUndefined => {
            dynasm!(ops ; .arch aarch64 ; movz x0, UNDEFINED_TAG_HI, LSL 48);
            emit_no_bail(ops);
            dynasm!(ops ; .arch aarch64 ; ret);
        }
        _ => return None,
    }
    Some(())
}

fn emit_load_raw(ops: &mut ArmAssembler, reg: u16, out: u8) {
    let off = u32::from(reg) * 8;
    dynasm!(ops ; .arch aarch64 ; ldr X(out), [x9, off]);
}

fn emit_store(ops: &mut ArmAssembler, reg: u16, src: u8) {
    let off = u32::from(reg) * 8;
    dynasm!(ops ; .arch aarch64 ; str X(src), [x9, off]);
}

fn emit_load_int(ops: &mut ArmAssembler, reg: u16, out: u8, bail: DynamicLabel) {
    emit_load_raw(ops, reg, out);
    dynasm!(ops
        ; .arch aarch64
        ; lsr x8, X(out), 48
        ; cmp w8, w11
        ; b.ne =>bail
        ; sxtw X(out), W(out)
    );
}

fn emit_load_bool(ops: &mut ArmAssembler, reg: u16, out: u8, bail: DynamicLabel) {
    emit_load_raw(ops, reg, out);
    dynasm!(ops
        ; .arch aarch64
        ; lsr x8, X(out), 48
        ; cmp w8, w14
        ; b.ne =>bail
        ; and w8, W(out), #1
        ; mov W(out), w8
    );
}

fn emit_u32(ops: &mut ArmAssembler, out: u8, value: u32) {
    let lo = value & 0xffff;
    let hi = value >> 16;
    dynasm!(ops
        ; .arch aarch64
        ; movz W(out), lo
        ; movk W(out), hi, LSL 16
    );
}

fn emit_no_bail(ops: &mut ArmAssembler) {
    dynasm!(ops ; .arch aarch64 ; movn w3, 0 ; str w3, [x10]);
}

fn emit_resume(ops: &mut ArmAssembler, ip: u32) {
    emit_u32(ops, 3, ip);
    dynasm!(ops
        ; .arch aarch64
        ; str w3, [x10]
        ; mov x0, xzr
        ; ret
    );
}

fn emit_jump(ops: &mut ArmAssembler, target: u32, labels: &[DynamicLabel]) {
    let label = labels[target as usize];
    dynasm!(ops ; .arch aarch64 ; b =>label);
}

fn emit_cond_jump(
    ops: &mut ArmAssembler,
    target: u32,
    labels: &[DynamicLabel],
    jump_if_true: bool,
) {
    let label = labels[target as usize];
    if jump_if_true {
        dynasm!(ops ; .arch aarch64 ; cbnz w3, =>label);
    } else {
        dynasm!(ops ; .arch aarch64 ; cbz w3, =>label);
    }
}

fn emit_compare_exit(ops: &mut ArmAssembler, target: u32, labels: &[DynamicLabel], not_le: bool) {
    let label = labels[target as usize];
    if not_le {
        dynasm!(ops ; .arch aarch64 ; b.gt =>label);
    } else {
        dynasm!(ops ; .arch aarch64 ; b.ge =>label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    fn proto(code: Vec<Instr>, reg_count: u16) -> FuncProto {
        FuncProto {
            name: "arm64-test".into(),
            code,
            reg_count,
            param_count: 0,
            length: 0,
            rest_reg: None,
            arguments_reg: None,
            is_generator: false,
            is_async: false,
            non_constructable: false,
            lexical_this: false,
            super_static: false,
            is_strict: false,
            simple_params: true,
            constants: Vec::new(),
            string_constants: Vec::new(),
            bigint_consts: Vec::new(),
            wtf8_consts: Vec::new(),
            name_global: None,
            upvalues: Vec::new(),
            eval_sites: Vec::new(),
            source: String::new(),
        }
    }

    #[test]
    fn native_loop_executes_and_returns_x0() {
        let p = proto(
            vec![
                Instr::LoadInt { dst: 1, val: 0 },
                Instr::LoadInt { dst: 2, val: 5 },
                Instr::JumpIfNotLt {
                    a: 1,
                    b: 2,
                    target: 5,
                },
                Instr::AddInt {
                    dst: 1,
                    a: 1,
                    imm: 1,
                    upd: false,
                },
                Instr::Jump { target: 2 },
                Instr::Return { src: 1 },
            ],
            3,
        );
        let f = compile_function(&p).expect("accepted ARM64 loop");
        let mut regs = vec![Value::UNDEFINED.bits(); 3];
        let (bits, bail) = unsafe { f.run(regs.as_mut_ptr(), core::ptr::null_mut()) };
        assert_eq!(bail, NO_BAIL);
        assert_eq!(Value::from_bits(bits), Value::int(5));
        assert_eq!(Value::from_bits(regs[1]), Value::int(5));
    }

    #[test]
    fn guard_and_overflow_bail_at_exact_ip_without_clobbering_dst() {
        let p = proto(
            vec![
                Instr::AddInt {
                    dst: 2,
                    a: 1,
                    imm: 1,
                    upd: false,
                },
                Instr::Return { src: 2 },
            ],
            3,
        );
        let f = compile_function(&p).expect("accepted add");
        for input in [Value::TRUE, Value::int(i32::MAX)] {
            let sentinel = Value::int(77);
            let mut regs = vec![Value::UNDEFINED.bits(), input.bits(), sentinel.bits()];
            let (_, bail) = unsafe { f.run(regs.as_mut_ptr(), core::ptr::null_mut()) };
            assert_eq!(bail, 0);
            assert_eq!(Value::from_bits(regs[2]), sentinel);
        }
    }

    #[test]
    fn negative_zero_multiply_bails_without_clobbering_dst() {
        let p = proto(
            vec![Instr::Mul { dst: 2, a: 0, b: 1 }, Instr::Return { src: 2 }],
            3,
        );
        let f = compile_function(&p).expect("accepted multiply");
        let sentinel = Value::int(77);
        let mut regs = vec![Value::int(-1).bits(), Value::int(0).bits(), sentinel.bits()];
        let (_, bail) = unsafe { f.run(regs.as_mut_ptr(), core::ptr::null_mut()) };
        assert_eq!(bail, 0);
        assert_eq!(Value::from_bits(regs[2]), sentinel);
    }

    #[test]
    fn far_negative_zero_bail_uses_near_test_bit_trampoline() {
        // Every bailout stub is emitted after the complete body. With the old
        // direct `tbnz =>bail`, this accepted body exceeded TBNZ's +/-32 KiB
        // relocation range and dynasmrt panicked while finalizing it.
        let mut code = vec![Instr::Mul { dst: 2, a: 0, b: 1 }; 768];
        code.push(Instr::Return { src: 2 });
        let p = proto(code, 3);
        let f = compile_function(&p).expect("large ARM64 multiply body compiles");
        assert!(
            f.code_bytes > 32 * 1024,
            "regression body must remain larger than TBNZ's reach"
        );

        let sentinel = Value::int(77);
        let mut regs = vec![Value::int(-1).bits(), Value::int(0).bits(), sentinel.bits()];
        let (_, bail) = unsafe { f.run(regs.as_mut_ptr(), core::ptr::null_mut()) };
        assert_eq!(bail, 0);
        assert_eq!(Value::from_bits(regs[2]), sentinel);
    }

    #[test]
    fn unsupported_and_oversized_functions_are_declined() {
        let unsupported = proto(
            vec![Instr::Mod { dst: 1, a: 1, b: 1 }, Instr::Return { src: 1 }],
            2,
        );
        assert!(compile_function(&unsupported).is_none());

        let mut code = vec![Instr::LoadInt { dst: 1, val: 0 }; MAX_COMPILED_OPS];
        code.push(Instr::Return { src: 1 });
        assert!(compile_function(&proto(code, 2)).is_none());
    }
}
