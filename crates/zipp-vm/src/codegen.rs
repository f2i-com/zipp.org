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

/// Number of times a loop back-edge must fire before the loop region is offered
/// to the OSR (on-stack-replacement) compiler. Low so hot loops promote fast.
pub const OSR_THRESHOLD: u32 = 8;

/// How many times a compiled region may "deopt" (a native run that resumes
/// INSIDE the region — a type guard bailed — rather than exiting cleanly) before
/// it is evicted and blacklisted. Prevents a livelock where the interpreter
/// re-enters native every back-edge only for it to bail at the same guard.
pub const OSR_DEOPT_LIMIT: u32 = 4;

/// Sentinel in `bail_ip` meaning the native code completed via `Return` (the
/// result is in the returned `u64`). Any other value is the ip to resume at.
pub const NO_BAIL: u32 = u32::MAX;

/// Tag bits for an `Int` value, matching `value.rs` (`0x7FF9 << 48`). A boxed
/// i32 is `INT_TAG | (i32 as u32 as u64)`.
const INT_TAG: u64 = 0x7FF9_0000_0000_0000;
/// Top-16 pattern that identifies an Int (the high 16 bits of `INT_TAG`).
const INT_TAG_HI: u32 = 0x7FF9;

/// NaN-box tag range for the polymorphic region `===` (mirror of value.rs):
/// a Value's high-16 in `[TAG_LO, TAG_HI]` is a tagged form (Int/Bool/Null/
/// Undefined/Heap); anything else is a double. `TAG_HEAP_HI` is the Heap tag.
const TAG_LO: u32 = 0x7FF9;
const TAG_HI: u32 = 0x7FFD;
const TAG_HEAP_HI: u32 = 0x7FFD;
/// Low 48 bits of a Value = a heap index (when the tag is Heap).
const PAYLOAD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// First non-interned heap index: indices `< 129` are the interned single-ASCII
/// chars (0..128) + the empty string (128); user objects start here. A heap
/// value `>=` this needs full `strict_eq` (the region bits-compare bails on it).
const USER_OBJ_START: i32 = 129;

/// Win64 addresses of the heap helpers (vm.rs), passed from the interpreter into
/// `Jit::compile_region`. The inline-cache base site index is assigned inside
/// `compile_region` (not here), then bundled into `HeapHelpers` for codegen.
#[derive(Clone, Copy)]
pub struct HeapHelperAddrs {
    pub get_prop_miss: usize,
    pub set_prop_miss: usize,
    pub versions_base: usize,
    pub ic_base: usize,
    /// Helper for a dense-array element read `a[i]` (`GetIndex`); returns the
    /// element bits, `undefined` for out-of-range, or the deopt sentinel.
    pub get_index: usize,
    /// Helper for a dense-array element write `a[i] = v` (`SetIndex`); returns 0
    /// on success (storing/growing) or the deopt sentinel.
    pub set_index: usize,
    /// Helper for `arr.push(x)` — returns the new length, or the deopt sentinel.
    pub array_push: usize,
    /// Helper for `str.charCodeAt(i)` — returns the char code / NaN, or deopt.
    pub char_code_at: usize,
}

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

/// A compiled loop region (OSR): native code for the bytecode range
/// `[start, end]`, entered at `start` (the loop header). `deopts` counts native
/// runs that bailed back inside the region; past `OSR_DEOPT_LIMIT` the region is
/// evicted. A native run returns the ip to resume interpreting at — a clean loop
/// exit (ip outside `[start,end]`) or a guard bail (ip inside it).
pub struct Region {
    code: JitFn,
    start: u32,
    end: u32,
    deopts: u32,
    /// True if compiled by the integer path. On eviction an int region falls
    /// back to the double path (rather than full-blacklisting the loop).
    is_int: bool,
    /// Set when this region was object-scalar-replaced (SROA): its GetProp/SetProp
    /// were rewritten to scratch field-globals. The interpreter must sync the
    /// object's fields ↔ the pool slots around each native run.
    field_plan: Option<FieldSyncPlan>,
}

/// How the interpreter syncs a scalar-replaced object around a native region run:
/// for each accessed field, the pool global slot holding it during the run.
#[derive(Clone)]
pub struct FieldSyncPlan {
    /// The global slot holding the promoted object.
    pub obj_global: u32,
    /// `(field name-constant index, pool global slot)` for each accessed field.
    pub fields: Vec<(u32, u32)>,
    /// The function id whose `string_constants` the name indices belong to.
    pub func_id: u32,
}

/// One monomorphic inline-cache slot for a JIT'd `GetProp`/`SetProp` site.
/// `repr(C)` with a fixed layout the native code indexes directly:
/// `obj_bits @0`, `vals_ptr @8`, `version @16`, `slot @20` (stride 24). A native
/// fast path checks `obj_bits` (object identity) AND the live object `version`
/// (read from the heap's parallel version array) against the cache; on a hit it
/// reads/writes `vals_ptr[slot]` with NO call (slots never move — keys are
/// append-only). `obj_bits == 0` means empty (no real object Value is 0).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IcEntry {
    obj_bits: u64,
    vals_ptr: u64,
    version: u32,
    slot: u32,
}

/// Per-function JIT state: call counts, compiled code, and a blacklist of
/// functions that aren't eligible (so we don't re-attempt them every tick).
/// The `region_*` maps mirror this for OSR loop regions, keyed by
/// `(func_id, loop_header_ip)`.
#[derive(Default)]
pub struct Jit {
    counts: FxHashMap<u32, u32>,
    compiled: FxHashMap<u32, JitFn>,
    blacklist: FxHashSet<u32>,
    regions: FxHashMap<(u32, u32), Region>,
    region_counts: FxHashMap<(u32, u32), u32>,
    region_blacklist: FxHashSet<(u32, u32)>,
    /// Loop headers where the INTEGER path was tried and deoptimised; the next
    /// compile for the key skips int and uses the double path instead.
    region_int_blacklist: FxHashSet<(u32, u32)>,
    /// Inline-cache slots for heap-op JIT sites, indexed by a global site id
    /// assigned at compile time. Grows only at compile time (never during a
    /// native run), so a base pointer fetched in a region prologue stays valid
    /// for that run; a `*_miss` helper only UPDATES an existing entry (no growth).
    ic_table: Vec<IcEntry>,
    /// One-entry cache of the most recent self-call target `(func_id, native
    /// entry)`. A self-recursive function (e.g. `fib`) always recurses into the
    /// SAME `func_id`, so this hits on every call and skips the `compiled`
    /// HashMap lookup that otherwise runs ~30M times for `fib(35)`. The cached
    /// entry pointer stays valid even if `compiled` rehashes: it points into the
    /// function's mmap'd `ExecutableBuffer`, which never moves, and a function's
    /// entry is immutable once compiled.
    self_cache: Option<(u32, *const u8)>,
    /// Compiled fused `map` kernels, keyed by callback `func_id`. `None` =
    /// tried and ineligible (so we don't recompile every `map` call). Keyed by
    /// `func_id` alone: a given callback proto has fixed param_count/body.
    map_kernels: FxHashMap<u32, Option<JitFn>>,
    /// Compiled fused `reduce` kernels, keyed by callback `func_id` (as above).
    reduce_kernels: FxHashMap<u32, Option<JitFn>>,
    /// Compiled fused `filter` kernels, keyed by predicate `func_id` (as above).
    filter_kernels: FxHashMap<u32, Option<JitFn>>,
}

impl Jit {
    pub fn new() -> Jit {
        Jit::default()
    }

    /// Look up compiled native code for `func_id`, if any.
    pub fn get(&self, func_id: u32) -> Option<&JitFn> {
        self.compiled.get(&func_id)
    }

    /// Native entry pointer for a SELF-CALL target, via a one-entry cache that
    /// the hot recursive path hits every time (skipping the `compiled` HashMap
    /// lookup). See `self_cache`. Returns `None` if `func_id` isn't compiled.
    #[inline]
    pub fn self_call_entry(&mut self, func_id: u32) -> Option<*const u8> {
        if let Some((id, entry)) = self.self_cache {
            if id == func_id {
                return Some(entry);
            }
        }
        let entry = self.compiled.get(&func_id)?.entry();
        self.self_cache = Some((func_id, entry));
        Some(entry)
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

    /// Native entry for the fused `map` kernel of callback `func_id`, compiling
    /// (and caching) it on first request. Returns `None` if the callback isn't
    /// kernel-eligible. The entry pointer is into the kernel's mmap'd buffer
    /// (stable; the cache `Option` owns the buffer for the VM's lifetime).
    pub fn map_kernel(&mut self, func_id: u32, proto: &FuncProto) -> Option<*const u8> {
        if let Some(slot) = self.map_kernels.get(&func_id) {
            return slot.as_ref().map(|f| f.entry());
        }
        let compiled = compile_map_kernel(proto);
        let entry = compiled.as_ref().map(|f| f.entry());
        self.map_kernels.insert(func_id, compiled);
        entry
    }

    /// Native entry for the fused `reduce` kernel of callback `func_id`,
    /// compiling and caching on first request. `None` if ineligible.
    pub fn reduce_kernel(&mut self, func_id: u32, proto: &FuncProto) -> Option<*const u8> {
        if let Some(slot) = self.reduce_kernels.get(&func_id) {
            return slot.as_ref().map(|f| f.entry());
        }
        let compiled = compile_reduce_kernel(proto);
        let entry = compiled.as_ref().map(|f| f.entry());
        self.reduce_kernels.insert(func_id, compiled);
        entry
    }

    /// Native entry for the fused `filter` kernel of predicate `func_id`,
    /// compiling and caching on first request. `None` if ineligible.
    pub fn filter_kernel(&mut self, func_id: u32, proto: &FuncProto) -> Option<*const u8> {
        if let Some(slot) = self.filter_kernels.get(&func_id) {
            return slot.as_ref().map(|f| f.entry());
        }
        let compiled = compile_filter_kernel(proto);
        let entry = compiled.as_ref().map(|f| f.entry());
        self.filter_kernels.insert(func_id, compiled);
        entry
    }

    // ── OSR loop regions ──

    /// Native code for the loop region of `func_id` whose header is `entry_ip`.
    pub fn get_region(&self, func_id: u32, entry_ip: u32) -> Option<&Region> {
        self.regions.get(&(func_id, entry_ip))
    }

    /// Count a back-edge to the loop headed at `entry_ip`. Returns `true` exactly
    /// once, when the count crosses `OSR_THRESHOLD` and the region is neither
    /// compiled nor blacklisted — the caller should then attempt `compile_region`.
    pub fn record_region(&mut self, func_id: u32, entry_ip: u32) -> bool {
        let key = (func_id, entry_ip);
        if self.regions.contains_key(&key) || self.region_blacklist.contains(&key) {
            return false;
        }
        let c = self.region_counts.entry(key).or_insert(0);
        *c += 1;
        *c == OSR_THRESHOLD
    }

    /// Attempt to compile the loop region `[start, end]` of `func_id` (entered at
    /// `start`). `globals_base_helper` is the address of the win64 helper that
    /// returns `vm.globals.as_mut_ptr()` (the region pins it for direct global
    /// access). On failure the region is blacklisted and never retried.
    #[allow(clippy::too_many_arguments)]
    pub fn compile_region(
        &mut self,
        func_id: u32,
        proto: &FuncProto,
        start: u32,
        end: u32,
        globals_base_helper: usize,
        heap_helpers: HeapHelperAddrs,
        field_pool_base: u32,
        field_pool_size: u32,
    ) {
        let key = (func_id, start);
        if self.regions.contains_key(&key) || self.region_blacklist.contains(&key) {
            return;
        }

        // ── object scalar-replacement (SROA) ── if the region's heap ops all
        // target one non-escaping global object, rewrite them to scratch
        // field-globals and compile the (now purely numeric) region — the loop
        // becomes register-only, like V8. Tried FIRST (beats the IC mem path).
        if !self.region_int_blacklist.contains(&key) {
            if let Some(fp) = plan_field_promotion(proto, start, end) {
                if (fp.fields.len() as u32) <= field_pool_size {
                    let sync_fields: Vec<(u32, u32)> = fp
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(i, &name)| (name, field_pool_base + i as u32))
                        .collect();
                    let rewritten = rewrite_for_field_promotion(proto, start, end, &fp, field_pool_base);
                    let compiled = compile_region_numeric(&rewritten, start, end, globals_base_helper);
                    if std::env::var_os("ZIPP_JITLOG").is_some() {
                        eprintln!(
                            "[jit] SROA region fn{func_id} [{start},{end}] fields={} -> {}",
                            fp.fields.len(),
                            if compiled.is_some() { "compiled" } else { "DECLINED (numeric path)" }
                        );
                    }
                    if let Some((code, is_int)) = compiled {
                        let plan = FieldSyncPlan { obj_global: fp.obj_global, fields: sync_fields, func_id };
                        self.regions.insert(
                            key,
                            Region { code, start, end, deopts: 0, is_int, field_plan: Some(plan) },
                        );
                        return;
                    }
                }
            }
        }

        // Prefer the integer path (i64/paddq — beats the double path on integer
        // loops) unless it already deoptimised for this loop. Fall back to the
        // double/memory path.
        if !self.region_int_blacklist.contains(&key) {
            if let Some(code) = compile_region_int(proto, start, end, globals_base_helper) {
                self.regions
                    .insert(key, Region { code, start, end, deopts: 0, is_int: true, field_plan: None });
                return;
            }
        }
        // Reserve one inline-cache slot per GetProp/SetProp in the region (only
        // heap regions take the mem path that uses them; numeric regions have 0).
        let n_sites = proto.code[start as usize..=end as usize]
            .iter()
            .filter(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }))
            .count();
        let ic_base_idx = self.reserve_ic_sites(n_sites);
        let helpers = HeapHelpers {
            func_id,
            get_prop_miss: heap_helpers.get_prop_miss,
            set_prop_miss: heap_helpers.set_prop_miss,
            versions_base: heap_helpers.versions_base,
            ic_base: heap_helpers.ic_base,
            get_index: heap_helpers.get_index,
            set_index: heap_helpers.set_index,
            array_push: heap_helpers.array_push,
            char_code_at: heap_helpers.char_code_at,
            ic_base_idx,
        };
        match compile_region(proto, start, end, globals_base_helper, helpers) {
            Some(code) => {
                self.regions
                    .insert(key, Region { code, start, end, deopts: 0, is_int: false, field_plan: None });
            }
            None => {
                self.region_blacklist.insert(key);
            }
        }
    }

    /// Record that a region run resumed at `resume_ip`. If that ip is inside the
    /// region (a deopt/bail) and the region has now deopted past the limit, evict
    /// and blacklist it (so the interpreter stops re-entering a guard that keeps
    /// failing). Returns whether the region remains installed.
    pub fn note_region_resume(&mut self, func_id: u32, entry_ip: u32, resume_ip: u32) {
        let key = (func_id, entry_ip);
        let (evict, retry) = if let Some(r) = self.regions.get_mut(&key) {
            if resume_ip >= r.start && resume_ip <= r.end {
                r.deopts += 1;
                // Retry on a SIMPLER path if this was an int region (value grew
                // past 2^53 → double handles it) or a SROA region (a field turned
                // non-numeric → the inline-cache mem path handles any type).
                (r.deopts >= OSR_DEOPT_LIMIT, r.is_int || r.field_plan.is_some())
            } else {
                (false, false)
            }
        } else {
            (false, false)
        };
        if evict {
            self.regions.remove(&key);
            if retry {
                // Don't blacklist the loop — let it recompile on a more general
                // path (region_int_blacklist also gates the SROA + int attempts).
                // Reset the back-edge counter so iterations re-trigger compile.
                self.region_int_blacklist.insert(key);
                self.region_counts.remove(&key);
            } else {
                self.region_blacklist.insert(key);
            }
        }
    }

    /// Reserve `n` fresh inline-cache slots (one per heap-op site in a region),
    /// returning the base global site id. The slots start empty (`obj_bits == 0`
    /// ⇒ always miss on first use).
    pub fn reserve_ic_sites(&mut self, n: usize) -> u32 {
        let base = self.ic_table.len() as u32;
        self.ic_table.resize(self.ic_table.len() + n, IcEntry::default());
        base
    }

    /// Base pointer of the inline-cache table (for a region prologue to pin).
    pub fn ic_base_ptr(&self) -> *const IcEntry {
        self.ic_table.as_ptr()
    }

    /// Fill inline-cache slot `site` after a miss (called by the `*_miss` helpers).
    /// Never grows the table (the slot was reserved at compile time), so a pinned
    /// base pointer in a running region stays valid.
    pub fn set_ic(&mut self, site: u32, obj_bits: u64, vals_ptr: u64, version: u32, slot: u32) {
        if let Some(e) = self.ic_table.get_mut(site as usize) {
            *e = IcEntry { obj_bits, vals_ptr, version, slot };
        }
    }
}

impl Region {
    /// Run this region's native code over the register window `regs` (vm pointer
    /// `vm`). Returns the ip to resume interpreting at. See `JitFn::run`.
    ///
    /// # Safety
    /// Same contract as [`JitFn::run`].
    pub unsafe fn run(&self, regs: *mut u64, vm: *mut core::ffi::c_void) -> u32 {
        let (_result, resume) = self.code.run(regs, vm);
        resume
    }

    /// The object scalar-replacement sync plan, if this region was field-promoted.
    /// The interpreter syncs the object's fields ↔ the pool globals around `run`.
    pub fn field_plan(&self) -> Option<&FieldSyncPlan> {
        self.field_plan.as_ref()
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
    // The function's own entry (offset 0). A self-recursive `Call` issues a
    // DIRECT native call here (same win64 ABI as `JitFn::run`), skipping the Rust
    // trampoline on the clean hot path. The recursion runs on the native stack,
    // bounded by an inline depth guard (see `emit_self_call`).
    let self_entry = ops.new_dynamic_label();

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
                // Fast path: a DIRECT native call to this function's own entry
                // with an inline depth guard — no Rust trampoline. Cold paths
                // (depth limit, or the callee bailed mid-body) route to the Rust
                // helper, which runs the activation on the interpreter WITH the
                // recursion depth held elevated (so re-entry can't livelock).
                emit_self_call(
                    &mut ops, ip, bail, self_entry, self_func_id, self_call_helper, dst, arg_base,
                    argc, proto.reg_count,
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
enum BinOp {
    Add,
    Sub,
    Mul,
    Mod,
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
        // Signed integer remainder (JS `%` on integers; truncated, sign of the
        // dividend = idiv's remainder). `% 0` is NaN (not an Int) → bail; bail on
        // divisor -1 too, which sidesteps the INT_MIN/-1 idiv #DE (and `% -1` is
        // always 0, so the interpreter handles that rare case correctly).
        // `cdq` sign-extends eax into edx:eax; `idiv r9d` puts the remainder in
        // edx, which we move into eax for `box_eax`. (Division `/` is NOT done
        // here — JS `/` is float division, e.g. 7/2 == 3.5, not an integer.)
        BinOp::Mod => dynasm!(ops
            ; test r9d, r9d
            ; jz => bail
            ; cmp r9d, -1
            ; je => bail
            ; cdq
            ; idiv r9d
            ; mov eax, edx
        ),
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
#[allow(clippy::too_many_arguments)]
fn emit_self_call(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    self_entry: dynasmrt::DynamicLabel,
    func_id: u32,
    helper: usize,
    dst: u16,
    arg_base: u16,
    argc: u16,
    reg_count: u16,
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

/// True if every op of `proto` is in the kernel's pure-arithmetic subset (no
/// calls / globals / heap ops / branches / `%` / non-int `LoadConst`). Callers
/// additionally check `param_count`. Stricter than `can_compile` (no self-call)
/// because the kernel must be call-free (no `regs` realloc under the window).
fn can_kernel_body(proto: &FuncProto) -> bool {
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
fn kmap_dbinop(
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
fn kmap_dmod(
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
fn kmap_dcmp(
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
enum KBody {
    Plain,
    Ret(u16),
    RetUndef,
}

/// Emit ONE op of a kernel callback body (straight-line — `can_kernel_body`
/// rejects branches). The window base is pinned in `rbx`; operands load via the
/// region f64 helpers (handling int-tagged AND double), bailing to `bail` on a
/// non-number. Returns `None` for an unsupported op (reject the kernel).
fn emit_kernel_arith(
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
        Instr::AddInt { dst, a, imm } => {
            load_num_xmm(ops, a, 0, bail);
            dynasm!(ops ; mov eax, imm ; cvtsi2sd xmm1, eax ; addsd xmm0, xmm1);
            store_xmm(ops, dst);
            Some(KBody::Plain)
        }
        Instr::Add { dst, a, b } => { kmap_dbinop(ops, bail, dst, a, b, DOp::Add); Some(KBody::Plain) }
        Instr::Sub { dst, a, b } => { kmap_dbinop(ops, bail, dst, a, b, DOp::Sub); Some(KBody::Plain) }
        Instr::Mul { dst, a, b } => { kmap_dbinop(ops, bail, dst, a, b, DOp::Mul); Some(KBody::Plain) }
        Instr::Div { dst, a, b } => { kmap_dbinop(ops, bail, dst, a, b, DOp::Div); Some(KBody::Plain) }
        Instr::Mod { dst, a, b } => { kmap_dmod(ops, bail, dst, a, b); Some(KBody::Plain) }
        Instr::Lt { dst, a, b } => { kmap_dcmp(ops, bail, dst, a, b, Cmp::Lt); Some(KBody::Plain) }
        Instr::Le { dst, a, b } => { kmap_dcmp(ops, bail, dst, a, b, Cmp::Le); Some(KBody::Plain) }
        Instr::Gt { dst, a, b } => { kmap_dcmp(ops, bail, dst, a, b, Cmp::Gt); Some(KBody::Plain) }
        Instr::Ge { dst, a, b } => { kmap_dcmp(ops, bail, dst, a, b, Cmp::Ge); Some(KBody::Plain) }
        Instr::Eq { dst, a, b } => { kmap_dcmp(ops, bail, dst, a, b, Cmp::Eq); Some(KBody::Plain) }
        Instr::Ne { dst, a, b } => { kmap_dcmp(ops, bail, dst, a, b, Cmp::Ne); Some(KBody::Plain) }
        Instr::Return { src } => Some(KBody::Ret(src)),
        Instr::ReturnUndefined => Some(KBody::RetUndef),
        _ => None,
    }
}

/// Compile a fused native `map` kernel for callback `proto` (see the module
/// comment above for the ABI and safety model). `None` if the callback isn't
/// kernel-eligible (the caller then uses the ordinary per-element path).
fn compile_map_kernel(proto: &FuncProto) -> Option<JitFn> {
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
    Some(JitFn { _buf: buf, entry: entry_ptr })
}

/// Compile a fused native `reduce` kernel for callback `proto` (2-param
/// `(acc, element)`; see the module comment for the ABI). The accumulator lives
/// in `r15` across iterations and is only committed at the callback's `Return`,
/// so a callback that mutates its `acc` param mid-body and then bails can't
/// corrupt it. `None` if ineligible. Index-using (3-param) reduces fall back to
/// the per-element path.
fn compile_reduce_kernel(proto: &FuncProto) -> Option<JitFn> {
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
    Some(JitFn { _buf: buf, entry: entry_ptr })
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
fn compile_filter_kernel(proto: &FuncProto) -> Option<JitFn> {
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
    Some(JitFn { _buf: buf, entry: entry_ptr })
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

/// Top-16 bits of the canonical bool tag (`0x7FFA`). The five tag patterns
/// 0x7FF9..=0x7FFD are: Int, Bool, Null, Undefined, Heap — only Int is a number.
const BOOL_TAG: u64 = INT_TAG + (1u64 << 48);

/// Can the loop region `[start, end]` be compiled in the double subset? Every op
/// in range must be numeric/control-flow with no call/heap/closure op, and any
/// `LoadConst` must reference a numeric constant.
fn region_can_compile(proto: &FuncProto, start: u32, end: u32) -> bool {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    if e <= s || e >= code.len() {
        return false;
    }
    // The back-edge must be an unconditional jump to the header (canonical
    // while/for shape). This guarantees no fall-through past `end`, so the only
    // out-of-region control transfers are explicit jump targets (loop exit /
    // break), which become exit stubs.
    match code[e] {
        Instr::Jump { target } if target == start => {}
        _ => return false,
    }
    for instr in &code[s..=e] {
        match *instr {
            Instr::LoadInt { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Div { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
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
            // Heap property ops — handled by the MEMORY path via win64 helper
            // calls (the int/regalloc paths decline, so heap regions take the
            // mem path). A `Print`/`Call`/etc. anywhere still rejects the region.
            | Instr::GetProp { .. }
            | Instr::SetProp { .. }
            // Dense-array element read/write `a[i]` / `a[i]=v` — handled by the
            // MEMORY path via win64 helpers (the int/regalloc paths decline).
            | Instr::GetIndex { .. }
            | Instr::SetIndex { .. }
            | Instr::Return { .. }
            | Instr::ReturnUndefined => {}
            // A whitelist of cheap, fixed-arity builtin method calls — handled by
            // the MEMORY path via a dedicated win64 helper (jit_array_push /
            // jit_char_code_at). Anything else (a user method, an unlisted builtin,
            // wrong arity) rejects the region.
            Instr::CallMethod { name, argc, .. } => {
                let key = proto.string_constants.get(name as usize).map(|s| s.as_str());
                if !(argc == 1 && matches!(key, Some("push") | Some("charCodeAt"))) {
                    return false;
                }
            }
            Instr::LoadConst { idx, .. } => {
                // Numeric constants run in the f64 region; a single-ASCII-char
                // string constant is resolvable to its interned slot (for
                // `s[i] === "x"` scans). Anything else (multi-char / non-ASCII
                // string, etc.) rejects the region.
                match proto.constants.get(idx as usize) {
                    Some(c) if c.is_number() => {}
                    Some(&c) if single_char_const_bits(proto, c).is_some() => {}
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

/// Addresses of the win64 heap helpers (vm.rs), the COMPILING function's id, and
/// the inline-cache base site index — threaded to the memory path so `GetProp`/
/// `SetProp` emit a call-free monomorphic inline cache (miss → helper).
#[derive(Clone, Copy)]
struct HeapHelpers {
    func_id: u32,
    get_prop_miss: usize,
    set_prop_miss: usize,
    /// Helper returning `vm.heap.versions_ptr()` (pinned in r13).
    versions_base: usize,
    /// Helper returning `vm.jit.ic_base_ptr()` (pinned in r14).
    ic_base: usize,
    /// Helper for a dense-array `GetIndex` (`a[i]`).
    get_index: usize,
    /// Helper for a dense-array `SetIndex` (`a[i] = v`).
    set_index: usize,
    /// Helper for `arr.push(x)`.
    array_push: usize,
    /// Helper for `str.charCodeAt(i)`.
    char_code_at: usize,
    /// First global inline-cache site id for this region; the k-th heap op uses
    /// `ic_base_idx + k`.
    ic_base_idx: u32,
}

/// Compile the loop region `[start, end]` (entered at `start`). Tries the
/// register-promoting path first (values live in xmm/gpr across the loop, no
/// per-op memory traffic — competitive with V8) and falls back to the simpler
/// memory-based path if the region's shape is outside the register allocator's
/// subset (e.g. it contains heap property ops). Returns `None` only if even the
/// fallback can't handle it.
fn compile_region(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    heap: HeapHelpers,
) -> Option<JitFn> {
    if let Some(f) = compile_region_regalloc(proto, start, end, globals_base_helper) {
        return Some(f);
    }
    compile_region_mem(proto, start, end, globals_base_helper, heap)
}

/// Compile a (rewritten, purely-numeric) field-promoted region via the integer or
/// double register path; returns `(code, is_int)`. Deliberately NOT the memory
/// path — the rewrite removed all heap ops, so if the register paths decline
/// (e.g. register pressure even with reuse), SROA is abandoned and the caller
/// falls back to the inline-cache mem path on the ORIGINAL bytecode.
fn compile_region_numeric(proto: &FuncProto, start: u32, end: u32, gh: usize) -> Option<(JitFn, bool)> {
    if let Some(f) = compile_region_int(proto, start, end, gh) {
        return Some((f, true));
    }
    compile_region_regalloc(proto, start, end, gh).map(|f| (f, false))
}

/// Clone `proto` and rewrite the region's heap ops to scratch field-globals so
/// the register paths can compile it: `GetProp(o.name) → LoadGlobal(dst, slot)`,
/// `SetProp(o.name, val) → StoreGlobal(slot, val)`, where `slot = pool_base + i`
/// and `i` is the field's index in `fp.fields`. The interpreter syncs each pool
/// slot ↔ the object's field around the native run (see `FieldSyncPlan`).
fn rewrite_for_field_promotion(
    proto: &FuncProto,
    start: u32,
    end: u32,
    fp: &FieldPromotePlan,
    pool_base: u32,
) -> FuncProto {
    let mut p = proto.clone();
    // Map a name-constant index to its pool slot BY FIELD STRING (fp.fields holds
    // one representative index per distinct field string).
    let slot_of = |name: u32| -> u32 {
        let s = &proto.string_constants[name as usize];
        let i = fp
            .fields
            .iter()
            .position(|&n| proto.string_constants[n as usize] == *s)
            .unwrap();
        pool_base + i as u32
    };
    for ip in start as usize..=end as usize {
        match p.code[ip] {
            Instr::GetProp { dst, name, .. } => {
                p.code[ip] = Instr::LoadGlobal { dst, idx: slot_of(name) };
            }
            Instr::SetProp { name, val, .. } => {
                p.code[ip] = Instr::StoreGlobal { idx: slot_of(name), src: val };
            }
            // The object-ref loads (`LoadGlobal o → r`) are now DEAD — their only
            // consumers (the heap ops above) no longer use `r`. Neutralise them to
            // `LoadInt 0` so the numeric path doesn't try to promote the object
            // global itself (a heap ref would fail its is-number entry guard, and
            // the whole region would bail). `r` stays dead/unread.
            Instr::LoadGlobal { dst, idx } if idx == fp.obj_global => {
                p.code[ip] = Instr::LoadInt { dst, val: 0 };
            }
            _ => {}
        }
    }
    p
}

/// Inferred type of a region value. The allocator places numbers in xmm
/// registers and booleans (compare results) in gprs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VTy {
    Num,
    Bool,
}

/// Where a region value lives for the duration of the loop.
#[derive(Clone, Copy)]
enum Home {
    Xmm(u8),
    Gpr(u8),
}

/// Register-allocation plan for a region: a fixed xmm/gpr home per VM register
/// and per global, computed by a type+liveness pass. `None` (decline) when the
/// region is outside the allocator's subset (too many live values, a type
/// conflict, an unsupported live-in, etc.) — the caller then uses the memory path.
struct RegionPlan {
    reg_home: FxHashMap<u16, Home>,
    glob_home: FxHashMap<u32, u8>, // global slot → xmm index
    /// Numeric registers that are read before written (must be loaded at entry).
    live_in_regs: Vec<(u16, u8)>, // (reg, xmm)
    /// Globals read before written (loaded + guarded at entry).
    live_in_globs: Vec<(u32, u8)>, // (slot, xmm)
    /// All numeric reg homes (flushed to the reg file on exit).
    num_regs: Vec<(u16, u8)>,
    /// All bool reg homes (boxed + flushed on exit).
    bool_regs: Vec<(u16, u8)>,
    /// All globals touched (flushed to globals memory on exit).
    globs: Vec<(u32, u8)>,
    /// Loop-invariant constants to materialise ONCE in the prologue: region ips
    /// of `LoadInt`/`LoadConst` whose dst is defined exactly once and never
    /// live-in. Their body occurrences are skipped (the home already holds them).
    hoist_ips: Vec<usize>,
    hoisted: FxHashSet<u16>,
    /// Registers DEFINED in the region but NEVER used as an operand (dead). All
    /// int-region ops are pure value computations (no side effects — heap/call ops
    /// decline the region), so a dead-dst op produces a value nothing reads and is
    /// skipped during body emission. The common source is object SROA, which
    /// neutralises the (now-unused) object-ref `LoadGlobal`s to `LoadInt 0`: that's
    /// ~7 dead ops/iteration in the object benchmark, and dropping them also frees
    /// their xmm homes (often taking the loop off the slower home-reuse path).
    dead: FxHashSet<u16>,
}

/// First xmm index usable as a value home (xmm0/xmm1 are scratch for the few ops
/// that need a temporary). xmm2..=xmm15 ⇒ 14 numeric homes.
const HOME_XMM_FIRST: u8 = 2;
const HOME_XMM_LAST: u8 = 15;
/// Gpr pool for boolean homes (r8..r11, all volatile; the region issues no calls
/// in its body so they survive). 4 simultaneous bools.
const BOOL_GPRS: [u8; 4] = [8, 9, 10, 11];

/// A numeric value being allocated an xmm home: a VM register or a global slot.
enum NumVal {
    Reg(u16),
    Glob(u32),
}

/// Tiny linear-scan xmm allocator: hands out home indices and reuses one once its
/// interval has ended. Intervals MUST be supplied in ascending start order. Used
/// only when one-home-per-value would overflow the pool (e.g. object SROA loops);
/// reusing a register can cost ILP, so simpler loops keep distinct homes.
struct XmmAlloc {
    next: u8,                 // next never-used xmm index
    active: Vec<(usize, u8)>, // (interval_end, xmm) currently live
    free: Vec<u8>,            // homes freed by expired intervals, available to reuse
}

impl XmmAlloc {
    fn new() -> XmmAlloc {
        XmmAlloc { next: HOME_XMM_FIRST, active: Vec::new(), free: Vec::new() }
    }

    /// Allocate a home for the interval `[start, end]`, or `None` if the pool is
    /// exhausted even after expiring intervals that ended before `start`.
    fn alloc(&mut self, start: usize, end: usize) -> Option<u8> {
        let mut i = 0;
        while i < self.active.len() {
            if self.active[i].0 < start {
                self.free.push(self.active[i].1);
                self.active.swap_remove(i);
            } else {
                i += 1;
            }
        }
        let x = if let Some(x) = self.free.pop() {
            x
        } else if self.next <= HOME_XMM_LAST {
            let x = self.next;
            self.next += 1;
            x
        } else {
            return None;
        };
        self.active.push((end, x));
        Some(x)
    }
}

/// Plan register homes for `[start, end]`, or `None` to decline (use mem path).
fn plan_region(proto: &FuncProto, start: u32, end: u32) -> Option<RegionPlan> {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    let mut ty: FxHashMap<u16, VTy> = FxHashMap::default();
    let mut first_seen: FxHashMap<u16, bool> = FxHashMap::default(); // reg → was first occurrence a def?
    let mut glob_first_read: FxHashMap<u32, bool> = FxHashMap::default(); // slot → first touch was a read?
    let mut reg_order: Vec<u16> = Vec::new();
    let mut glob_order: Vec<u32> = Vec::new();

    // Record a use (operand) of reg `r` with required type `req`.
    // Returns false on a type conflict (caller declines).
    let note_def = |r: u16, t: VTy, ty: &mut FxHashMap<u16, VTy>, first_seen: &mut FxHashMap<u16, bool>, reg_order: &mut Vec<u16>| -> bool {
        if let Some(prev) = ty.get(&r) {
            if *prev != t {
                return false;
            }
        } else {
            ty.insert(r, t);
            reg_order.push(r);
        }
        first_seen.entry(r).or_insert(true); // first occurrence is a def
        true
    };

    // Two passes are awkward with closures; do a single ordered pass collecting
    // type (from defs) and first-occurrence (def vs use). Operand type
    // requirements are validated in a second loop once types are known.
    for instr in &code[s..=e] {
        // A dense-array element access or a builtin method call can't be
        // register-allocated (their operands/result are boxed Values handled by a
        // helper, not known-type scalars) — decline so the region takes the memory
        // path that emits the helper call.
        if matches!(
            instr,
            Instr::GetIndex { .. } | Instr::SetIndex { .. } | Instr::CallMethod { .. }
        ) {
            return None;
        }
        let (def, dty): (Option<u16>, VTy) = match *instr {
            Instr::LoadInt { dst, .. } => (Some(dst), VTy::Num),
            Instr::LoadConst { dst, .. } => (Some(dst), VTy::Num),
            Instr::LoadGlobal { dst, .. } => (Some(dst), VTy::Num),
            Instr::AddInt { dst, .. } => (Some(dst), VTy::Num),
            Instr::Neg { dst, .. } => (Some(dst), VTy::Num),
            Instr::Add { dst, .. }
            | Instr::Sub { dst, .. }
            | Instr::Mul { dst, .. }
            | Instr::Div { dst, .. } => (Some(dst), VTy::Num),
            Instr::Lt { dst, .. }
            | Instr::Le { dst, .. }
            | Instr::Gt { dst, .. }
            | Instr::Ge { dst, .. }
            | Instr::Eq { dst, .. }
            | Instr::Ne { dst, .. } => (Some(dst), VTy::Bool),
            Instr::Move { dst, .. } => (Some(dst), VTy::Num), // refined below
            _ => (None, VTy::Num),
        };
        // Record operand first-occurrences (uses) BEFORE the def, so a reg used
        // and defined by the same op counts the use first (live-in).
        for u in instr_uses(instr) {
            first_seen.entry(u).or_insert(false); // first occurrence is a use ⇒ live-in
            if !ty.contains_key(&u) {
                // Type not yet known; tentatively untyped — refined when defined.
            }
        }
        if let Some(d) = def {
            // Move's dst type follows its src; default Num is corrected here.
            let t = if let Instr::Move { src, .. } = *instr {
                *ty.get(&src).unwrap_or(&VTy::Num)
            } else {
                dty
            };
            if !note_def(d, t, &mut ty, &mut first_seen, &mut reg_order) {
                return None;
            }
        }
        // Globals: order + first-touch direction.
        match *instr {
            Instr::LoadGlobal { idx, .. } => {
                glob_first_read.entry(idx).or_insert(true);
                if !glob_order.contains(&idx) {
                    glob_order.push(idx);
                }
            }
            Instr::StoreGlobal { idx, .. } => {
                glob_first_read.entry(idx).or_insert(false);
                if !glob_order.contains(&idx) {
                    glob_order.push(idx);
                }
            }
            _ => {}
        }
    }

    // A register used but never defined in the region is a read-only live-in.
    // Loading/typing it correctly (numeric vs bool) is fiddly, so decline and
    // let the memory path handle it (it reads everything from the reg file).
    for instr in &code[s..=e] {
        for u in instr_uses(instr) {
            if !ty.contains_key(&u) {
                return None;
            }
        }
    }

    // Validate operand type requirements now that types are known.
    for instr in &code[s..=e] {
        match *instr {
            Instr::Add { a, b, .. }
            | Instr::Sub { a, b, .. }
            | Instr::Mul { a, b, .. }
            | Instr::Div { a, b, .. }
            | Instr::Lt { a, b, .. }
            | Instr::Le { a, b, .. }
            | Instr::Gt { a, b, .. }
            | Instr::Ge { a, b, .. }
            | Instr::Eq { a, b, .. }
            | Instr::Ne { a, b, .. }
            | Instr::JumpIfNotLt { a, b, .. }
            | Instr::JumpIfNotLe { a, b, .. } => {
                if ty.get(&a) == Some(&VTy::Bool) || ty.get(&b) == Some(&VTy::Bool) {
                    return None; // numeric op on a bool — outside the subset
                }
            }
            Instr::AddInt { a, .. } | Instr::Neg { a, .. } => {
                if ty.get(&a) == Some(&VTy::Bool) {
                    return None;
                }
            }
            Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => {
                // Only bool conditions are supported (the loop-guard shape).
                if ty.get(&cond) != Some(&VTy::Bool) {
                    return None;
                }
            }
            _ => {}
        }
    }

    // Loop-invariant constant detection: a reg defined exactly once, by a
    // LoadInt/LoadConst, and not live-in, holds the same value every iteration —
    // materialise it once in the prologue and skip the body op.
    let mut def_count: FxHashMap<u16, u32> = FxHashMap::default();
    let mut const_def_ip: FxHashMap<u16, usize> = FxHashMap::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        match *instr {
            Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } => {
                *def_count.entry(dst).or_insert(0) += 1;
                const_def_ip.insert(dst, s + off);
            }
            _ => {
                if let Some(d) = writes_reg(instr) {
                    *def_count.entry(d).or_insert(0) += 1;
                }
            }
        }
    }
    // Registers that are actually USED as an operand somewhere in the region.
    // A defined-but-unused reg is DEAD (e.g. an object-ref load neutralised to
    // `LoadInt 0` by the field-promotion rewrite) — it must NOT be hoisted, or it
    // would consume a permanent xmm home for a value that's never read.
    let mut used: FxHashSet<u16> = FxHashSet::default();
    for instr in &code[s..=e] {
        for u in instr_uses(instr) {
            used.insert(u);
        }
    }
    // ── dead-code elimination ── a register written in the region but never read
    // (not in `used`) is dead. Every int-region op is a pure value computation, so
    // its defining op produces a result nothing observes and can be skipped — and
    // the reg dropped from home allocation. Drop dead regs from `reg_order` so they
    // consume no xmm home and don't count toward the pool-overflow check (which can
    // flip the loop to the slower home-reuse path). `dead` excludes loop-carried
    // (live-in) regs — those are read across iterations even if not within one.
    let dead: FxHashSet<u16> = reg_order
        .iter()
        .copied()
        .filter(|r| !used.contains(r) && first_seen.get(r) != Some(&false))
        .collect();
    reg_order.retain(|r| !dead.contains(r));
    let mut hoist_ips: Vec<usize> = Vec::new();
    let mut hoisted: FxHashSet<u16> = FxHashSet::default();
    for (&r, &ip) in &const_def_ip {
        if def_count.get(&r) == Some(&1) && first_seen.get(&r) == Some(&true) && used.contains(&r) {
            hoist_ips.push(ip);
            hoisted.insert(r);
        }
    }
    hoist_ips.sort_unstable();

    // Per-register live range [first_ip, last_ip] within the region (for linear-
    // scan reuse). A live-in reg (used before defined) is loop-carried, so its
    // value spans the whole region [s, e]; otherwise it lives from its first
    // appearance to its last. Globals are loop-carried (whole region).
    let mut first_ip: FxHashMap<u16, usize> = FxHashMap::default();
    let mut last_ip: FxHashMap<u16, usize> = FxHashMap::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        let ip = s + off;
        let mut touch = |r: u16| {
            first_ip.entry(r).or_insert(ip);
            last_ip.insert(r, ip);
        };
        for u in instr_uses(instr) {
            touch(u);
        }
        if let Some(d) = writes_reg(instr) {
            touch(d);
        }
    }
    let range = |r: u16| -> (usize, usize) {
        // Whole-region (permanent home) if loop-carried (live-in, used before
        // defined) OR a HOISTED constant — hoisted values are materialised once
        // in the prologue and read every iteration, so their home must never be
        // freed/reused mid-region (doing so clobbered them — a real bug).
        if first_seen.get(&r) == Some(&false) || hoisted.contains(&r) {
            (s, e)
        } else {
            (first_ip[&r], last_ip[&r])
        }
    };

    // The xmm home pool size. If one-home-per-numeric-value fits, use the simple
    // allocation (distinct home each — best ILP, what loop.js relies on). Only
    // when it would OVERFLOW do we linear-scan-reuse homes for non-overlapping
    // live ranges (lets bigger loops JIT, and is required for object SROA).
    const POOL: usize = (HOME_XMM_LAST - HOME_XMM_FIRST + 1) as usize;
    let n_numeric = reg_order.iter().filter(|r| ty[r] == VTy::Num).count() + glob_order.len();
    let reuse = n_numeric > POOL;

    // ── allocate xmm/gpr homes ──
    let mut reg_home: FxHashMap<u16, Home> = FxHashMap::default();
    let mut glob_home: FxHashMap<u32, u8> = FxHashMap::default();
    if reuse {
        // Linear-scan: numeric values (regs + globals) by ascending range start,
        // reusing a home once a value's range ends. Loop-carried values (globals
        // and live-in regs) span [s, e] and so keep a permanent home.
        let mut intervals: Vec<(usize, usize, NumVal)> = Vec::new();
        for &r in &reg_order {
            if ty[&r] == VTy::Num {
                let (a, b) = range(r);
                intervals.push((a, b, NumVal::Reg(r)));
            }
        }
        for &gi in &glob_order {
            intervals.push((s, e, NumVal::Glob(gi)));
        }
        intervals.sort_by_key(|&(a, _, _)| a);
        let mut alloc = XmmAlloc::new();
        for (a, b, v) in intervals {
            let x = alloc.alloc(a, b)?; // None ⇒ pool exhausted even with reuse
            match v {
                NumVal::Reg(r) => {
                    reg_home.insert(r, Home::Xmm(x));
                }
                NumVal::Glob(gi) => {
                    glob_home.insert(gi, x);
                }
            }
        }
    } else {
        // One distinct home per numeric value (best ILP — what loop.js relies on).
        let mut next_xmm = HOME_XMM_FIRST;
        for &r in &reg_order {
            if ty[&r] == VTy::Num {
                if next_xmm > HOME_XMM_LAST {
                    return None;
                }
                reg_home.insert(r, Home::Xmm(next_xmm));
                next_xmm += 1;
            }
        }
        for &gi in &glob_order {
            if next_xmm > HOME_XMM_LAST {
                return None;
            }
            glob_home.insert(gi, next_xmm);
            next_xmm += 1;
        }
    }
    // Bools (both modes): gpr homes; a live-in bool is unsupported.
    let mut next_bool = 0usize;
    for &r in &reg_order {
        if ty[&r] == VTy::Bool {
            if first_seen.get(&r) == Some(&false) || next_bool >= BOOL_GPRS.len() {
                return None;
            }
            reg_home.insert(r, Home::Gpr(BOOL_GPRS[next_bool]));
            next_bool += 1;
        }
    }

    // ── derived lists from the final homes (unified for both modes) ──
    // With reuse, several regs may share an xmm; flush_exit writes the shared
    // value to each reg's slot, which is sound (non-overlapping live ranges mean
    // the dead members are never read before being redefined).
    let mut num_regs = Vec::new();
    let mut bool_regs = Vec::new();
    let mut live_in_regs = Vec::new();
    for &r in &reg_order {
        match reg_home[&r] {
            Home::Xmm(x) => {
                num_regs.push((r, x));
                if first_seen.get(&r) == Some(&false) {
                    live_in_regs.push((r, x));
                }
            }
            Home::Gpr(g) => bool_regs.push((r, g)),
        }
    }
    let mut globs = Vec::new();
    let mut live_in_globs = Vec::new();
    for &gi in &glob_order {
        let x = glob_home[&gi];
        globs.push((gi, x));
        if glob_first_read.get(&gi) == Some(&true) {
            live_in_globs.push((gi, x));
        }
    }

    Some(RegionPlan {
        reg_home,
        glob_home,
        live_in_regs,
        live_in_globs,
        num_regs,
        bool_regs,
        globs,
        hoist_ips,
        hoisted,
        dead,
    })
}

/// The VM registers an instruction reads (operands). Used for live-in analysis.
fn instr_uses(i: &Instr) -> Vec<u16> {
    match *i {
        Instr::Move { src, .. } => vec![src],
        Instr::StoreGlobal { src, .. } => vec![src],
        Instr::AddInt { a, .. } | Instr::Neg { a, .. } => vec![a],
        Instr::Add { a, b, .. }
        | Instr::Sub { a, b, .. }
        | Instr::Mul { a, b, .. }
        | Instr::Div { a, b, .. }
        | Instr::Lt { a, b, .. }
        | Instr::Le { a, b, .. }
        | Instr::Gt { a, b, .. }
        | Instr::Ge { a, b, .. }
        | Instr::Eq { a, b, .. }
        | Instr::Ne { a, b, .. }
        | Instr::JumpIfNotLt { a, b, .. }
        | Instr::JumpIfNotLe { a, b, .. } => vec![a, b],
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => vec![cond],
        Instr::GetProp { obj, .. } => vec![obj],
        Instr::SetProp { obj, val, .. } => vec![obj, val],
        Instr::GetIndex { obj, key, .. } => vec![obj, key],
        Instr::SetIndex { obj, key, val } => vec![obj, key, val],
        Instr::Return { src } => vec![src],
        _ => vec![],
    }
}

/// Plan for promoting a single stable object's fields to registers (SROA-lite,
/// the effect of V8's escape-analysis + scalar replacement): when EVERY
/// GetProp/SetProp in a region targets the SAME object — a global `obj_global`
/// loaded by `LoadGlobal` and never re-stored in the region, and whose ref reg
/// is used ONLY as the GetProp/SetProp receiver — its accessed fields can live in
/// registers for the loop body, synced to the heap object only at region
/// entry/exit, so the loop becomes register-only like V8.
#[allow(dead_code)] // wired into codegen in a following step
struct FieldPromotePlan {
    /// The global slot holding the promoted object.
    obj_global: u32,
    /// Distinct accessed field name-constant indices, in first-seen order. Each
    /// maps to a synthetic "field global" the heap ops are rewritten to use.
    fields: Vec<u32>,
}

/// Detect whether `[start, end]` is field-promotable; see `FieldPromotePlan`.
#[allow(dead_code)] // wired into codegen in a following step
fn plan_field_promotion(proto: &FuncProto, start: u32, end: u32) -> Option<FieldPromotePlan> {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    if !code[s..=e]
        .iter()
        .any(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }))
    {
        return None;
    }

    // Single-def map (for tracing an obj-ref reg to its LoadGlobal).
    let mut reg_def: FxHashMap<u16, usize> = FxHashMap::default();
    let mut reg_def_count: FxHashMap<u16, u32> = FxHashMap::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if let Some(d) = writes_reg(instr) {
            reg_def.insert(d, s + off);
            *reg_def_count.entry(d).or_insert(0) += 1;
        }
    }

    // Every heap-op receiver must be the SAME global object, loaded once.
    let mut obj_global: Option<u32> = None;
    let mut obj_ref_regs: FxHashSet<u16> = FxHashSet::default();
    let mut fields: Vec<u32> = Vec::new();
    for instr in &code[s..=e] {
        let (obj_reg, name) = match *instr {
            Instr::GetProp { obj, name, .. } => (obj, name),
            Instr::SetProp { obj, name, .. } => (obj, name),
            _ => continue,
        };
        let def_ip = *reg_def.get(&obj_reg)?; // must be defined in the region
        if reg_def_count.get(&obj_reg) != Some(&1) {
            return None; // multiple defs → can't trace
        }
        let g = match code[def_ip] {
            Instr::LoadGlobal { idx, .. } => idx,
            _ => return None, // receiver isn't a plain global load
        };
        match obj_global {
            None => obj_global = Some(g),
            Some(prev) if prev == g => {}
            Some(_) => return None, // two different objects at the site set
        }
        obj_ref_regs.insert(obj_reg);
        // Dedup by the field STRING, not the name-constant INDEX: the compiler
        // emits a distinct string-constant per occurrence, so `o.a` read and
        // `o.a` write have DIFFERENT name indices for the SAME field. Keying by
        // index would give them separate pool slots (the read wouldn't see the
        // write). Keep one representative index per distinct field string.
        let fname = &proto.string_constants[name as usize];
        // `length` is a SPECIAL property (an array's element count / a string's
        // length), not a plain stored slot. Scalar-replacing it diverges from the
        // interpreter — e.g. `arr.length = n` truncates the array, but a promoted
        // scalar would just track a dead pool slot. Decline; the inline-cache /
        // helper path handles `.length` correctly (read) and deopts the write.
        if fname == "length" {
            return None;
        }
        if !fields
            .iter()
            .any(|&n| proto.string_constants[n as usize] == *fname)
        {
            fields.push(name);
        }
    }
    let g = obj_global?;

    // The object ref must be stable (G not re-stored) and its ref reg must not
    // escape (used only as the GetProp/SetProp receiver, nowhere else).
    for instr in &code[s..=e] {
        if let Instr::StoreGlobal { idx, .. } = *instr {
            if idx == g {
                return None;
            }
        }
        // EVERY load of the promoted object must feed a heap op only — if `g` is
        // also loaded into a register that is NOT a heap-op receiver, that ref
        // could escape (be stored, or used numerically), so the object isn't
        // provably confined to the rewritten accesses. Decline (→ inline cache).
        if let Instr::LoadGlobal { dst, idx } = *instr {
            if idx == g && !obj_ref_regs.contains(&dst) {
                return None;
            }
        }
        if matches!(instr, Instr::GetProp { .. } | Instr::SetProp { .. }) {
            continue;
        }
        for u in instr_uses(instr) {
            if obj_ref_regs.contains(&u) {
                return None; // ref reg used outside a heap op → object escapes
            }
        }
    }
    Some(FieldPromotePlan { obj_global: g, fields })
}

/// Register-promoting region codegen: each region value lives in a fixed xmm
/// (numbers) or gpr (booleans) home for the whole loop. Live-in values are
/// loaded + type-guarded ONCE at entry; the loop body is then pure register SSE
/// with NO per-op guards or memory traffic (this is what makes it competitive
/// with V8). All homes are flushed back to the reg file / globals on every exit.
fn compile_region_regalloc(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
) -> Option<JitFn> {
    if !region_can_compile(proto, start, end) {
        return None;
    }
    let plan = plan_region(proto, start, end)?;
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let (s, e) = (start as usize, end as usize);

    let in_region: Vec<_> = (s..=e).map(|_| ops.new_dynamic_label()).collect();
    let mut exit_stubs: FxHashMap<u32, dynasmrt::DynamicLabel> = FxHashMap::default();
    let flush_exit = ops.new_dynamic_label(); // flush homes, then restore + ret
    let entry_bail = ops.new_dynamic_label(); // entry guard failed: restore + ret, NO flush
    let lbl = |ip: u32, in_region: &[dynasmrt::DynamicLabel]| in_region[(ip - start) as usize];

    // ── prologue ── save callee-saved gprs, fetch globals base, save the
    // nonvolatile xmm6..15 (we may use them as homes), load live-in homes, jump
    // to the loop header. No call occurs after the globals-base fetch, so stack
    // alignment past that point is irrelevant and movdqu (unaligned) is fine.
    // r13/r14 are pushed (unused by the double path) to share the one restore
    // sequence with the int path, which uses them for guard constants.
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; mov rbx, rcx
        ; mov rsi, rdx
        ; mov rdi, r8
        ; sub rsp, 40                 // shadow space (32) + 8 pad ⇒ rsp 16-aligned
        ; mov rcx, rdi
        ; mov rax, QWORD globals_base_helper as i64
        ; call rax
        ; mov r12, rax
        ; add rsp, 40
        ; sub rsp, 160                // save area for xmm6..15 (10 × 16)
    );
    for k in 0..10u32 {
        let xi = 6 + k as u8;
        dynasm!(ops ; movdqu [rsp + (k as i32) * 16], Rx(xi));
    }
    // Load live-in globals (guarded) and live-in registers (guarded).
    for &(gi, x) in &plan.live_in_globs {
        dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]);
        emit_box_to_home(&mut ops, x, entry_bail);
    }
    for &(r, x) in &plan.live_in_regs {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_box_to_home(&mut ops, x, entry_bail);
    }
    // Hoisted loop-invariant constants: materialise once, here.
    for &hip in &plan.hoist_ips {
        emit_load_const(&mut ops, &plan, &proto.code[hip], proto);
    }
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // ── body ──
    for ip in s..=e {
        dynasm!(ops ; => lbl(ip as u32, &in_region));
        // A hoisted constant's home was filled in the prologue; the body op is a
        // no-op (fall through to the next ip, its label preserved for jumps).
        if let Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } = proto.code[ip] {
            if plan.hoisted.contains(&dst) {
                continue;
            }
        }
        // Dead-code elimination: skip a pure op whose result is never read (see
        // plan_region `dead`). Sound — every regalloc-region op is side-effect-free.
        if let Some(d) = writes_reg(&proto.code[ip]) {
            if plan.dead.contains(&d) {
                continue;
            }
        }
        match proto.code[ip] {
            Instr::LoadInt { .. } | Instr::LoadConst { .. } => {
                emit_load_const(&mut ops, &plan, &proto.code[ip], proto);
            }
            Instr::Move { dst, src } => match home(&plan, dst) {
                Home::Xmm(d) => {
                    let srx = xh(&plan, src);
                    dynasm!(ops ; movsd Rx(d), Rx(srx));
                }
                Home::Gpr(d) => {
                    let sg = gh(&plan, src);
                    dynasm!(ops ; mov Rq(d), Rq(sg));
                }
            },
            Instr::LoadGlobal { dst, idx } => {
                let d = xh(&plan, dst);
                let g = plan.glob_home[&idx];
                dynasm!(ops ; movsd Rx(d), Rx(g));
            }
            Instr::StoreGlobal { idx, src } => {
                let g = plan.glob_home[&idx];
                let srx = xh(&plan, src);
                dynasm!(ops ; movsd Rx(g), Rx(srx));
            }
            Instr::Add { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Add),
            Instr::Sub { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Sub),
            Instr::Mul { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Mul),
            Instr::Div { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Div),
            Instr::AddInt { dst, a, imm } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                dynasm!(ops
                    ; mov eax, imm
                    ; cvtsi2sd xmm0, eax
                );
                if d != ax {
                    dynasm!(ops ; movsd Rx(d), Rx(ax));
                }
                dynasm!(ops ; addsd Rx(d), xmm0);
            }
            Instr::Neg { dst, a } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                dynasm!(ops
                    ; xorps xmm0, xmm0
                    ; subsd xmm0, Rx(ax)
                    ; movsd Rx(d), xmm0
                );
            }
            Instr::Lt { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Lt),
            Instr::Le { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Le),
            Instr::Gt { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Gt),
            Instr::Ge { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Ge),
            Instr::Eq { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Eq),
            Instr::Ne { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Ne),
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } => {
                let c = gh(&plan, cond);
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; test Rq(c), Rq(c) ; jz => t);
            }
            Instr::JumpIfTrue { cond, target } => {
                let c = gh(&plan, cond);
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; test Rq(c), Rq(c) ; jnz => t);
            }
            Instr::JumpIfNotLt { a, b, target } => {
                let (ax, bx) = (xh(&plan, a), xh(&plan, b));
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; ucomisd Rx(bx), Rx(ax) ; jbe => t); // !(a<b)
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let (ax, bx) = (xh(&plan, a), xh(&plan, b));
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; ucomisd Rx(bx), Rx(ax) ; jb => t); // !(a<=b)
            }
            Instr::Return { .. } | Instr::ReturnUndefined => {
                dynasm!(ops ; mov DWORD [rsi], ip as i32 ; jmp => flush_exit);
            }
            _ => return None,
        }
    }

    // ── exit stubs ── set the resume ip, then flush+restore+ret.
    for (target, label) in &exit_stubs {
        dynasm!(ops
            ; => *label
            ; mov DWORD [rsi], *target as i32
            ; jmp => flush_exit
        );
    }

    // ── flush_exit ── write every home back to the reg file / globals (so the
    // interpreter resumes with consistent state), restore xmm6..15 + the stack,
    // and return. [rsi] already holds the resume ip.
    dynasm!(ops ; => flush_exit);
    for &(r, x) in &plan.num_regs {
        dynasm!(ops ; movq rax, Rx(x) ; mov [rbx + dreg(r)], rax);
    }
    for &(r, g) in &plan.bool_regs {
        // Box the 0/1 in the gpr into a Bool Value.
        dynasm!(ops
            ; mov rax, QWORD BOOL_TAG as i64
            ; or rax, Rq(g)
            ; mov [rbx + dreg(r)], rax
        );
    }
    for &(gi, x) in &plan.globs {
        dynasm!(ops ; movq rax, Rx(x) ; mov [r12 + (gi as i32) * 8], rax);
    }
    emit_region_restore(&mut ops);

    // ── entry_bail ── a live-in type guard failed; nothing was computed yet, so
    // restore (NO flush — reg file / globals are still consistent) and resume at
    // the header. [rsi] is set here to the loop header.
    dynasm!(ops
        ; => entry_bail
        ; mov DWORD [rsi], start as i32
    );
    emit_region_restore(&mut ops);

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn { _buf: buf, entry: entry_ptr })
}

/// `2^53` — the largest magnitude where consecutive integers are all exactly
/// representable in f64. Above it, JS `+`/`-` round, so an exact i64 result would
/// diverge: the int path bails to the interpreter when a result leaves
/// `[-2^53, 2^53]`. (Too large for a `cmp r64, imm32`, so it goes via a register.)
const TWO_POW_53: i64 = 9_007_199_254_740_992;
/// `2^54` — the unsigned upper bound for the shifted range check `(x + 2^53) ≤ 2^54`.
const TWO_POW_54: i64 = 18_014_398_509_481_984;

/// Can the loop region `[start, end]` run on the INTEGER path? Stricter than
/// `region_can_compile`: every op must be integer-valued (no Mul/Div/Mod — i64
/// multiply needs 128-bit and div/mod are fractional), and every `LoadConst`
/// must be an Int-tagged constant (a double constant would be misread as i64).
fn region_is_int(proto: &FuncProto, start: u32, end: u32) -> bool {
    if !region_can_compile(proto, start, end) {
        return false;
    }
    let (s, e) = (start as usize, end as usize);
    for instr in &proto.code[s..=e] {
        match *instr {
            Instr::LoadInt { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
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
            Instr::LoadConst { idx, .. } => {
                // Only Int-tagged constants; a double const can't be an i64 home.
                match proto.constants.get(idx as usize) {
                    Some(c) if c.is_int() => {}
                    _ => return false,
                }
            }
            _ => return false, // Div / Mod / anything else
        }
    }
    true
}

/// INTEGER region codegen: each numeric region value is stored as a raw i64 in
/// the low quadword of its xmm home; arithmetic uses `paddq`/`psubq` (~1-cycle
/// latency, vs `addsd`'s ~4), so the carried accumulator chain runs far faster
/// than the double path — the goal being to beat V8 on integer loops.
///
/// Correctness (mirrors JS f64 semantics exactly): every Int Value's i32 payload
/// is SIGN-EXTENDED to i64 on load. After every add/sub the result is checked
/// against `[-2^53, 2^53]`; if it leaves that range (where JS would round) the
/// region flushes all homes and bails to the interpreter at the NEXT ip — the
/// just-overflowed value flushed via `cvtsi2sd` equals JS's rounded result, so
/// resuming is sound. On exit each i64 home is boxed back to an Int Value (if it
/// fits i32) or a double (else, exact since |x| ≤ 2^53). All comparisons are
/// SIGNED. Live-ins are guarded Int-tagged at entry (bail otherwise, no flush).
fn compile_region_int(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
) -> Option<JitFn> {
    if !region_is_int(proto, start, end) {
        return None;
    }
    let plan = plan_region(proto, start, end)?;
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let (s, e) = (start as usize, end as usize);

    let in_region: Vec<_> = (s..=e).map(|_| ops.new_dynamic_label()).collect();
    let mut exit_stubs: FxHashMap<u32, dynasmrt::DynamicLabel> = FxHashMap::default();
    let flush_exit = ops.new_dynamic_label();
    let entry_bail = ops.new_dynamic_label();
    let lbl = |ip: u32, in_region: &[dynasmrt::DynamicLabel]| in_region[(ip - start) as usize];

    // ── prologue ── identical to the double path (save callee-saved, fetch
    // globals base, save xmm6..15) — only the live-in loads + body differ. r13/r14
    // additionally hold the 2^53/2^54 guard constants (pre-loaded once).
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; mov rbx, rcx
        ; mov rsi, rdx
        ; mov rdi, r8
        ; sub rsp, 40
        ; mov rcx, rdi
        ; mov rax, QWORD globals_base_helper as i64
        ; call rax
        ; mov r12, rax
        ; add rsp, 40
        ; mov r13, QWORD TWO_POW_53           // guard: + 2^53
        ; mov r14, QWORD TWO_POW_54           // guard: unsigned upper bound 2^54
        ; sub rsp, 160
    );
    for k in 0..10u32 {
        let xi = 6 + k as u8;
        dynasm!(ops ; movdqu [rsp + (k as i32) * 16], Rx(xi));
    }
    // Live-in globals/regs: guard Int-tagged, sign-extend payload, into the home.
    for &(gi, x) in &plan.live_in_globs {
        dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]);
        emit_int_entry_load(&mut ops, x, entry_bail);
    }
    for &(r, x) in &plan.live_in_regs {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_int_entry_load(&mut ops, x, entry_bail);
    }
    for &hip in &plan.hoist_ips {
        emit_int_const(&mut ops, &plan, &proto.code[hip], proto);
    }
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // ── body ──
    for ip in s..=e {
        dynasm!(ops ; => lbl(ip as u32, &in_region));
        if let Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } = proto.code[ip] {
            if plan.hoisted.contains(&dst) {
                continue;
            }
        }
        // Dead-code elimination: skip a pure value op whose result is never read
        // (a `dead` reg — see plan_region). All int-region ops are side-effect-free
        // (heap/calls decline the region), so this is sound. The label was already
        // emitted above so any jump still resolves. NOTE: jumps/stores/returns
        // aren't reg-defs, so `writes_reg` returns None for them — never skipped.
        if let Some(d) = writes_reg(&proto.code[ip]) {
            if plan.dead.contains(&d) {
                continue;
            }
        }
        match proto.code[ip] {
            Instr::LoadInt { .. } | Instr::LoadConst { .. } => {
                emit_int_const(&mut ops, &plan, &proto.code[ip], proto);
            }
            Instr::Move { dst, src } => match home(&plan, dst) {
                Home::Xmm(d) => {
                    let srx = xh(&plan, src);
                    dynasm!(ops ; movdqa Rx(d), Rx(srx));
                }
                Home::Gpr(d) => {
                    let sg = gh(&plan, src);
                    dynasm!(ops ; mov Rq(d), Rq(sg));
                }
            },
            Instr::LoadGlobal { dst, idx } => {
                let d = xh(&plan, dst);
                let g = plan.glob_home[&idx];
                dynasm!(ops ; movdqa Rx(d), Rx(g));
            }
            Instr::StoreGlobal { idx, src } => {
                let g = plan.glob_home[&idx];
                let srx = xh(&plan, src);
                dynasm!(ops ; movdqa Rx(g), Rx(srx));
            }
            Instr::Add { dst, a, b } => emit_ibin(&mut ops, &plan, ip, flush_exit, dst, a, b, true),
            Instr::Sub { dst, a, b } => emit_ibin(&mut ops, &plan, ip, flush_exit, dst, a, b, false),
            Instr::Mul { dst, a, b } => {
                // i64 multiply via imul (gpr). On i64 OVERFLOW (product ≥ 2^63)
                // the result wrapped → bail at THIS ip WITHOUT storing dst, so the
                // interpreter redoes it in f64 (reading the flushed operands). On a
                // representable-but-large product the 2^53 guard handles it (like
                // add): flush via cvtsi2sd (== JS's rounded product) + resume ip+1.
                let (d, ax, bx) = (xh(&plan, dst), xh(&plan, a), xh(&plan, b));
                let ovf = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                dynasm!(ops
                    ; movq rax, Rx(ax)
                    ; movq rcx, Rx(bx)
                    ; imul rax, rcx
                    ; jo => ovf            // i64 overflow → can't represent; redo in interp
                    ; movq Rx(d), rax
                    ; jmp => done
                    ; => ovf
                    ; mov DWORD [rsi], ip as i32 // resume at THIS op (dst not written)
                    ; jmp => flush_exit
                    ; => done
                );
                emit_i53_guard(&mut ops, d, ip, flush_exit);
            }
            Instr::AddInt { dst, a, imm } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                // Materialise the (sign-extended) immediate as i64 in xmm0.
                dynasm!(ops ; mov rax, QWORD imm as i64 ; movq xmm0, rax);
                if d != ax {
                    dynasm!(ops ; movdqa Rx(d), Rx(ax));
                }
                dynasm!(ops ; paddq Rx(d), xmm0);
                emit_i53_guard(&mut ops, d, ip, flush_exit);
            }
            Instr::Neg { dst, a } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                dynasm!(ops
                    ; pxor xmm0, xmm0
                    ; psubq xmm0, Rx(ax)
                    ; movdqa Rx(d), xmm0
                );
                emit_i53_guard(&mut ops, d, ip, flush_exit);
            }
            Instr::Lt { dst, a, b } => emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Lt),
            Instr::Le { dst, a, b } => emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Le),
            Instr::Gt { dst, a, b } => emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Gt),
            Instr::Ge { dst, a, b } => emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Ge),
            Instr::Eq { dst, a, b } => emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Eq),
            Instr::Ne { dst, a, b } => emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Ne),
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } => {
                let c = gh(&plan, cond);
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; test Rq(c), Rq(c) ; jz => t);
            }
            Instr::JumpIfTrue { cond, target } => {
                let c = gh(&plan, cond);
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; test Rq(c), Rq(c) ; jnz => t);
            }
            Instr::JumpIfNotLt { a, b, target } => {
                let (ax, bx) = (xh(&plan, a), xh(&plan, b));
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                // !(a<b) ⇔ a>=b (SIGNED).
                dynasm!(ops ; movq rax, Rx(ax) ; movq rcx, Rx(bx) ; cmp rax, rcx ; jge => t);
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let (ax, bx) = (xh(&plan, a), xh(&plan, b));
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                // !(a<=b) ⇔ a>b (SIGNED).
                dynasm!(ops ; movq rax, Rx(ax) ; movq rcx, Rx(bx) ; cmp rax, rcx ; jg => t);
            }
            Instr::Return { .. } | Instr::ReturnUndefined => {
                dynasm!(ops ; mov DWORD [rsi], ip as i32 ; jmp => flush_exit);
            }
            _ => return None,
        }
    }

    // ── exit stubs ──
    for (target, label) in &exit_stubs {
        dynasm!(ops ; => *label ; mov DWORD [rsi], *target as i32 ; jmp => flush_exit);
    }

    // ── flush_exit ── box each i64 home back to an Int/double Value and write it
    // to the reg file / globals, restore, return. [rsi] holds the resume ip.
    dynasm!(ops ; => flush_exit);
    for &(r, x) in &plan.num_regs {
        emit_int_box_from_home(&mut ops, x);
        dynasm!(ops ; mov [rbx + dreg(r)], rax);
    }
    for &(r, g) in &plan.bool_regs {
        dynasm!(ops ; mov rax, QWORD BOOL_TAG as i64 ; or rax, Rq(g) ; mov [rbx + dreg(r)], rax);
    }
    for &(gi, x) in &plan.globs {
        emit_int_box_from_home(&mut ops, x);
        dynasm!(ops ; mov [r12 + (gi as i32) * 8], rax);
    }
    emit_region_restore(&mut ops);

    // ── entry_bail ── a live-in wasn't Int-tagged; nothing computed, so restore
    // (NO flush) and resume at the header (interpreted).
    dynasm!(ops ; => entry_bail ; mov DWORD [rsi], start as i32);
    emit_region_restore(&mut ops);

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn { _buf: buf, entry: entry_ptr })
}

/// Entry load for the int path: the Value bits are in `rax`. Guard Int-tagged
/// (else `entry_bail`), SIGN-EXTEND the i32 payload to i64, store into the home.
fn emit_int_entry_load(ops: &mut dynasmrt::x64::Assembler, home: u8, entry_bail: dynasmrt::DynamicLabel) {
    dynasm!(ops
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => entry_bail        // not Int-tagged (double/bool/null/undef/heap)
        ; movsxd rax, eax          // sign-extend the i32 payload to i64
        ; movq Rx(home), rax
    );
}

/// Materialise an integer constant (`LoadInt`/`LoadConst`-Int) into its i64 home:
/// the FULL sign-extended i64 immediate, then `movq` (NOT cvtsi2sd — we want the
/// integer bit pattern, not its f64 form).
fn emit_int_const(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, instr: &Instr, proto: &FuncProto) {
    let (h, v) = match *instr {
        Instr::LoadInt { dst, val } => (xh(plan, dst), val as i64),
        Instr::LoadConst { dst, idx } => {
            let c = proto.constants[idx as usize];
            // region_is_int guaranteed c.is_int(); payload is the i32, sign-extend.
            (xh(plan, dst), (c.bits() as u32 as i32) as i64)
        }
        _ => unreachable!("emit_int_const on non-constant"),
    };
    dynasm!(ops ; mov rax, QWORD v ; movq Rx(h), rax);
}

/// Guard that the i64 in xmm home `h` is within `[-2^53, 2^53]` (signed); if not,
/// flush all homes and resume the interpreter at the NEXT ip (the overflowed
/// value flushes via cvtsi2sd to exactly JS's rounded result, so ip+1 is sound).
fn emit_i53_guard(ops: &mut dynasmrt::x64::Assembler, h: u8, ip: usize, flush_exit: dynasmrt::DynamicLabel) {
    // Range trick: x ∈ [-2^53, 2^53] ⟺ (x + 2^53) ≤ 2^54 as UNSIGNED (a value
    // below -2^53 wraps to a huge unsigned and fails too). The two constants are
    // pre-loaded once in the prologue (r13 = 2^53, r14 = 2^54) — avoiding two
    // 10-byte `movabs` per guard, which profiling showed dominated the loop.
    let ovf = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; movq rax, Rx(h)
        ; add rax, r13           // + 2^53 (no i64 overflow: |x| ≤ 2^54 here)
        ; cmp rax, r14           // 2^54
        ; jbe => done            // in range → continue
        ; => ovf
        ; mov DWORD [rsi], (ip + 1) as i32   // resume AFTER this op (result flushed)
        ; jmp => flush_exit
        ; => done
    );
}

/// `home[dst] = home[a] <±> home[b]` as i64 (paddq/psubq), with aliasing handled
/// and a 2^53 guard. `add = true` ⇒ paddq (commutative); else psubq.
#[allow(clippy::too_many_arguments)]
fn emit_ibin(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, ip: usize, flush_exit: dynasmrt::DynamicLabel, dst: u16, a: u16, b: u16, add: bool) {
    let (d, ax, bx) = (xh(plan, dst), xh(plan, a), xh(plan, b));
    if add {
        if d == ax {
            dynasm!(ops ; paddq Rx(d), Rx(bx));
        } else if d == bx {
            dynasm!(ops ; paddq Rx(d), Rx(ax)); // commutative
        } else {
            dynasm!(ops ; movdqa Rx(d), Rx(ax) ; paddq Rx(d), Rx(bx));
        }
    } else if d == ax {
        dynasm!(ops ; psubq Rx(d), Rx(bx));
    } else if d == bx {
        // dst == b (and ≠ a): use xmm0 to avoid clobbering b before reading it.
        dynasm!(ops ; movdqa xmm0, Rx(ax) ; psubq xmm0, Rx(bx) ; movdqa Rx(d), xmm0);
    } else {
        dynasm!(ops ; movdqa Rx(d), Rx(ax) ; psubq Rx(d), Rx(bx));
    }
    emit_i53_guard(ops, d, ip, flush_exit);
}

/// `bool_home[dst] = (home[a] <cmp> home[b])` as SIGNED i64 comparison.
fn emit_icmp(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, dst: u16, a: u16, b: u16, cmp: Cmp) {
    let (ax, bx) = (xh(plan, a), xh(plan, b));
    let d = gh(plan, dst);
    dynasm!(ops ; movq rax, Rx(ax) ; movq rcx, Rx(bx) ; cmp rax, rcx);
    match cmp {
        Cmp::Lt => dynasm!(ops ; setl al),
        Cmp::Le => dynasm!(ops ; setle al),
        Cmp::Gt => dynasm!(ops ; setg al),
        Cmp::Ge => dynasm!(ops ; setge al),
        Cmp::Eq => dynasm!(ops ; sete al),
        Cmp::Ne => dynasm!(ops ; setne al),
    }
    dynasm!(ops ; movzx Rq(d), al);
}

/// Box the i64 in xmm home `h` into a Value, leaving the bits in `rax`: Int-tag
/// if it fits i32 (low 32 masked in), else a double via `cvtsi2sd` (exact since
/// |x| ≤ 2^53, enforced by the per-op guard). Used by flush_exit.
fn emit_int_box_from_home(ops: &mut dynasmrt::x64::Assembler, h: u8) {
    let big = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; movq rax, Rx(h)
        ; cmp rax, 0x7FFFFFFF            // > i32::MAX ?
        ; jg => big
        ; cmp rax, -0x80000000           // < i32::MIN ?
        ; jl => big
        ; mov ecx, eax                   // low 32 (zero-extended into rcx)
        ; mov rdx, QWORD INT_TAG as i64
        ; or rdx, rcx
        ; mov rax, rdx                   // Int-tagged Value
        ; jmp => done
        ; => big
        ; cvtsi2sd xmm0, rax             // exact: |rax| ≤ 2^53
        ; movq rax, xmm0                 // double Value bits
        ; => done
    );
}

/// Restore xmm6..15 from the save area and the saved gprs, then `ret`.
fn emit_region_restore(ops: &mut dynasmrt::x64::Assembler) {
    for k in 0..10u32 {
        let xi = 6 + k as u8;
        dynasm!(ops ; movdqu Rx(xi), [rsp + (k as i32) * 16]);
    }
    dynasm!(ops
        ; add rsp, 160
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
    );
}

/// Materialise a numeric constant (a `LoadInt`/`LoadConst` op) into a value's
/// xmm home. Shared by the prologue (for hoisted loop-invariants) and the body.
fn emit_load_const(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, instr: &Instr, proto: &FuncProto) {
    match *instr {
        Instr::LoadInt { dst, val } => {
            let h = xh(plan, dst);
            dynasm!(ops ; mov eax, val ; cvtsi2sd Rx(h), eax);
        }
        Instr::LoadConst { dst, idx } => {
            let h = xh(plan, dst);
            let v = proto.constants[idx as usize];
            if v.is_int() {
                let payload = v.bits() as u32 as i32;
                dynasm!(ops ; mov eax, payload ; cvtsi2sd Rx(h), eax);
            } else {
                dynasm!(ops ; mov rax, QWORD v.bits() as i64 ; movq Rx(h), rax);
            }
        }
        _ => unreachable!("emit_load_const on non-constant op"),
    }
}

/// Guard that the Value bits already in `rax` are a number and load them into
/// xmm home `home` as f64 (Int → cvtsi2sd; double → movq); else jump to `bail`.
/// Used only at region entry for live-in values (the loop body is guard-free).
fn emit_box_to_home(ops: &mut dynasmrt::x64::Assembler, home: u8, bail: dynasmrt::DynamicLabel) {
    let int_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32      // 0x7FFA (bool tag)
        ; cmp r10d, 3                            // high16 ∈ [0x7FFA,0x7FFD] ⇒ not a number
        ; jbe => bail
        ; movq Rx(home), rax
        ; jmp => done
        ; => int_path
        ; cvtsi2sd Rx(home), eax
        ; => done
    );
}

/// The xmm home index of numeric register `r` (panics only on an allocator bug).
fn xh(plan: &RegionPlan, r: u16) -> u8 {
    match plan.reg_home[&r] {
        Home::Xmm(x) => x,
        Home::Gpr(_) => unreachable!("numeric use of a bool-homed register"),
    }
}
/// The gpr home index of bool register `r`.
fn gh(plan: &RegionPlan, r: u16) -> u8 {
    match plan.reg_home[&r] {
        Home::Gpr(g) => g,
        Home::Xmm(_) => unreachable!("bool use of a number-homed register"),
    }
}
fn home(plan: &RegionPlan, r: u16) -> Home {
    plan.reg_home[&r]
}

/// Emit a register-to-register f64 binop into the dst home, handling aliasing.
fn emit_dbin(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, dst: u16, a: u16, b: u16, op: DOp) {
    let (d, ax, bx) = (xh(plan, dst), xh(plan, a), xh(plan, b));
    let commutative = matches!(op, DOp::Add | DOp::Mul);
    // Arrange operands so the accumulator is `d`. For non-commutative ops where
    // d == b (and d != a), use xmm0 as a temp to avoid clobbering b.
    if d == ax {
        emit_dop(ops, d, bx, op);
    } else if d == bx {
        if commutative {
            emit_dop(ops, d, ax, op); // d holds b; d = b op a == a op b
        } else {
            dynasm!(ops ; movsd xmm0, Rx(ax));
            emit_dop_xmm0(ops, bx, op); // xmm0 = a op b
            dynasm!(ops ; movsd Rx(d), xmm0);
        }
    } else {
        dynasm!(ops ; movsd Rx(d), Rx(ax));
        emit_dop(ops, d, bx, op);
    }
}

/// `xmm[d] <op>= xmm[src]`.
fn emit_dop(ops: &mut dynasmrt::x64::Assembler, d: u8, src: u8, op: DOp) {
    match op {
        DOp::Add => dynasm!(ops ; addsd Rx(d), Rx(src)),
        DOp::Sub => dynasm!(ops ; subsd Rx(d), Rx(src)),
        DOp::Mul => dynasm!(ops ; mulsd Rx(d), Rx(src)),
        DOp::Div => dynasm!(ops ; divsd Rx(d), Rx(src)),
    }
}
/// `xmm0 <op>= xmm[src]`.
fn emit_dop_xmm0(ops: &mut dynasmrt::x64::Assembler, src: u8, op: DOp) {
    match op {
        DOp::Add => dynasm!(ops ; addsd xmm0, Rx(src)),
        DOp::Sub => dynasm!(ops ; subsd xmm0, Rx(src)),
        DOp::Mul => dynasm!(ops ; mulsd xmm0, Rx(src)),
        DOp::Div => dynasm!(ops ; divsd xmm0, Rx(src)),
    }
}

/// Emit `bool_home[dst] = (a <cmp> b)` using f64 ordered comparison.
fn emit_dcmp(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, dst: u16, a: u16, b: u16, cmp: Cmp) {
    let (ax, bx) = (xh(plan, a), xh(plan, b));
    let d = gh(plan, dst);
    match cmp {
        Cmp::Lt => dynasm!(ops ; ucomisd Rx(bx), Rx(ax) ; seta al),
        Cmp::Le => dynasm!(ops ; ucomisd Rx(bx), Rx(ax) ; setae al),
        Cmp::Gt => dynasm!(ops ; ucomisd Rx(ax), Rx(bx) ; seta al),
        Cmp::Ge => dynasm!(ops ; ucomisd Rx(ax), Rx(bx) ; setae al),
        Cmp::Eq => dynasm!(ops ; ucomisd Rx(ax), Rx(bx) ; sete al ; setnp cl ; and al, cl),
        Cmp::Ne => dynasm!(ops ; ucomisd Rx(ax), Rx(bx) ; setne al ; setp cl ; or al, cl),
    }
    dynasm!(ops ; movzx Rq(d), al);
}

/// Memory-based region codegen: every op loads operands from the register file
/// (with a type guard) and stores results back, globals via the pinned base
/// pointer. Correct and simple; ~4x faster than the interpreter but leaves
/// per-iteration memory traffic on the table (the register-promoting path above
/// removes it). Kept as the fallback for regions the allocator declines.
fn compile_region_mem(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    heap: HeapHelpers,
) -> Option<JitFn> {
    if !region_can_compile(proto, start, end) {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let (s, e) = (start as usize, end as usize);

    // One label per in-region ip (offset by `start`). Out-of-region jump targets
    // resolve to lazily-created exit stubs.
    let in_region: Vec<_> = (s..=e).map(|_| ops.new_dynamic_label()).collect();
    let mut exit_stubs: FxHashMap<u32, dynasmrt::DynamicLabel> = FxHashMap::default();
    let epilogue = ops.new_dynamic_label();
    let lbl = |ip: u32, in_region: &[dynasmrt::DynamicLabel]| in_region[(ip - start) as usize];

    // ── prologue ── save callee-saved, stash inputs, fetch globals base, jump to
    // the loop header (OSR entry).
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; sub rsp, 40                     // 32B shadow + an 8B 5th-arg slot ⇒ rsp 16-aligned
        ; mov rbx, rcx                    // regs base
        ; mov rsi, rdx                    // resume_ip out-pointer
        ; mov rdi, r8                     // vm
        ; mov rcx, rdi                    // arg0 = vm
        ; mov rax, QWORD globals_base_helper as i64
        ; call rax
        ; mov r12, rax                    // pinned globals base pointer
        ; mov rcx, rdi
        ; mov rax, QWORD heap.versions_base as i64
        ; call rax
        ; mov r13, rax                    // pinned heap version-array base (IC)
        ; mov rcx, rdi
        ; mov rax, QWORD heap.ic_base as i64
        ; call rax
        ; mov r14, rax                    // pinned inline-cache table base
        ; jmp => lbl(start, &in_region)
    );

    // The k-th GetProp/SetProp in the region uses inline-cache site `ic_site`.
    let mut ic_site = heap.ic_base_idx;
    for ip in s..=e {
        let ipl = lbl(ip as u32, &in_region);
        dynasm!(ops ; => ipl);
        let bail = ops.new_dynamic_label();
        match proto.code[ip] {
            Instr::LoadInt { dst, val } => {
                let boxed = INT_TAG | (val as u32 as u64);
                dynasm!(ops
                    ; mov rax, QWORD boxed as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadConst { dst, idx } => {
                // A single-ASCII-char string constant materialises as its
                // INTERNED slot (the same boxed Value `s[i]` yields), so a later
                // `=== "x"` is a bits compare; numeric/other consts use raw bits.
                let c = proto.constants[idx as usize];
                let bits = single_char_const_bits(proto, c).unwrap_or_else(|| c.bits());
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::Move { dst, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadGlobal { dst, idx } => {
                dynasm!(ops
                    ; mov rax, [r12 + (idx as i32) * 8]
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::StoreGlobal { idx, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [r12 + (idx as i32) * 8], rax
                );
            }
            Instr::Add { dst, a, b } => dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Add),
            Instr::Sub { dst, a, b } => dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Sub),
            Instr::Mul { dst, a, b } => dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Mul),
            Instr::Div { dst, a, b } => dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Div),
            Instr::AddInt { dst, a, imm } => {
                // a + imm in f64: load a, materialise imm as a double, addsd.
                load_num_xmm(&mut ops, a, 0, bail);
                dynasm!(ops
                    ; mov eax, imm
                    ; cvtsi2sd xmm1, eax
                    ; addsd xmm0, xmm1
                );
                store_xmm(&mut ops, dst);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Neg { dst, a } => {
                // Negate via 0.0 - a (keeps it in the f64 domain).
                load_num_xmm(&mut ops, a, 1, bail);
                dynasm!(ops
                    ; xorps xmm0, xmm0
                    ; subsd xmm0, xmm1
                );
                store_xmm(&mut ops, dst);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Lt { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Lt),
            Instr::Le { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Le),
            Instr::Gt { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Gt),
            Instr::Ge { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Ge),
            // `===` / `!==` are polymorphic: numeric operands compare as f64,
            // interned single-char strings / Int / Bool / Null / Undefined
            // compare by bits, non-interned heap operands bail to the interpreter.
            Instr::Eq { dst, a, b } => region_poly_eq(&mut ops, ip, bail, epilogue, dst, a, b, false),
            Instr::Ne { dst, a, b } => region_poly_eq(&mut ops, ip, bail, epilogue, dst, a, b, true),
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } => {
                guard_int_or_bool(&mut ops, cond, bail);
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops
                    ; mov eax, [rbx + dreg(cond)]
                    ; test eax, eax
                    ; jz => t
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::JumpIfTrue { cond, target } => {
                guard_int_or_bool(&mut ops, cond, bail);
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops
                    ; mov eax, [rbx + dreg(cond)]
                    ; test eax, eax
                    ; jnz => t
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::JumpIfNotLt { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                djump_if_not_cmp(&mut ops, ip, bail, epilogue, a, b, Cmp::Lt, t);
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                djump_if_not_cmp(&mut ops, ip, bail, epilogue, a, b, Cmp::Le, t);
            }
            Instr::GetProp { dst, obj, name } => {
                // ── monomorphic inline cache (CALL-FREE on hit) ──
                // Identity (obj_bits) + version match ⇒ read cached vals_ptr[slot]
                // directly. Miss ⇒ a lean helper that re-fills the cache. r14 =
                // IC table base, r13 = heap version-array base. IcEntry layout:
                // obj_bits@0, vals_ptr@8, version@16, slot@20 (stride 24).
                //
                // SAFETY (versions[heap_idx] read is in-bounds): the read at
                // `[r13 + heap_idx*4]` is reached ONLY after `jne miss` fails —
                // i.e. only when obj_bits == the CACHED obj_bits. `set_ic` only
                // ever caches a validated heap Object's bits, so a match implies
                // obj is that same valid object ⇒ heap_idx < heap.versions.len()
                // (== objs.len()). Likewise vals[slot]: slots are append-only so
                // a once-valid slot stays valid, and a version match proves vals
                // hasn't reallocated. (Verifier lenses that flagged an OOB here
                // analysed the read in isolation, missing the identity gate.)
                let o = (ic_site * 24) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let miss = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]          // obj_bits
                    ; cmp rax, [r14 + o]                  // cached obj_bits (identity)
                    ; jne => miss
                    ; mov ecx, eax                        // heap_idx = low 32 of obj_bits
                    ; mov edx, [r13 + rcx*4]              // live version
                    ; cmp edx, [r14 + o + 16]             // cached version
                    ; jne => miss
                    ; mov rcx, [r14 + o + 8]              // vals_ptr
                    ; mov edx, [r14 + o + 20]             // slot
                    ; mov rax, [rcx + rdx*8]              // vals[slot] (CALL-FREE)
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => cont
                    ; => miss
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits (still in rax)
                    ; mov r8d, ic_site as i32            // site_idx
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov rax, QWORD heap.get_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                    ; => cont
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::SetProp { obj, name, val } => {
                // ── monomorphic inline cache (CALL-FREE write on hit) ──
                let o = (ic_site * 24) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let miss = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]          // obj_bits
                    ; cmp rax, [r14 + o]                  // cached obj_bits
                    ; jne => miss
                    ; mov ecx, eax                        // heap_idx
                    ; mov edx, [r13 + rcx*4]              // live version
                    ; cmp edx, [r14 + o + 16]             // cached version
                    ; jne => miss
                    ; mov rcx, [r14 + o + 8]              // vals_ptr
                    ; mov edx, [r14 + o + 20]             // slot
                    ; mov r8, [rbx + dreg(val)]          // val_bits
                    ; mov [rcx + rdx*8], r8               // vals[slot] = val (CALL-FREE)
                    ; jmp => cont
                    ; => miss
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits
                    ; mov r8, [rbx + dreg(val)]          // val_bits
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov QWORD [rsp + 32], ic_site as i32 // 5th arg: site_idx (stack)
                    ; mov rax, QWORD heap.set_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; => cont
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::GetIndex { dst, obj, key } => {
                // Dense-array element read `a[i]` via a win64 helper. The helper
                // returns the element bits, `undefined` for out-of-range (a later
                // numeric op then guard-bails on it, matching the interpreter), or
                // the deopt sentinel for a non-array / non-int key. No inline
                // cache: arrays carry no shape, so the read is a direct
                // bounds-checked `vals[i]` inside the helper.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // array bits
                    ; mov r8, [rbx + dreg(key)]           // index bits
                    ; mov rax, QWORD heap.get_index as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::SetIndex { obj, key, val } => {
                // Dense-array element write `a[i] = v` via a win64 helper, which
                // stores in place or grows (matching the interpreter). Returns 0
                // (ok) or the deopt sentinel (non-array / negative / fractional /
                // non-numeric key → interpreter applies the no-op fallback).
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // array bits
                    ; mov r8, [rbx + dreg(key)]           // index bits
                    ; mov r9, [rbx + dreg(val)]           // value bits
                    ; mov rax, QWORD heap.set_index as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::CallMethod { dst, obj, name, arg_base, .. } => {
                // A whitelisted 1-arg builtin (`arr.push(x)` / `str.charCodeAt(i)`)
                // via a dedicated win64 helper: receiver + arg0 bits in, result
                // bits out, deopt sentinel → bail. region_can_compile gated this.
                let helper = match proto.string_constants[name as usize].as_str() {
                    "push" => heap.array_push,
                    "charCodeAt" => heap.char_code_at,
                    _ => return None, // defensive (gated)
                };
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                    ; mov r8, [rbx + dreg(arg_base)]      // arg0 bits
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Return { .. } | Instr::ReturnUndefined => {
                // Resume interpreting at this ip so the interpreter performs the
                // return (popping frames is its job, not the region's).
                dynasm!(ops
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => epilogue
                );
            }
            _ => return None, // region_can_compile already filtered; defensive
        }
    }

    // ── exit stubs ── one per distinct out-of-region jump target: record the
    // resume ip and jump to the shared epilogue.
    for (target, label) in &exit_stubs {
        dynasm!(ops
            ; => *label
            ; mov DWORD [rsi], *target as i32
            ; jmp => epilogue
        );
    }

    // ── epilogue ── restore and return; [rsi] already holds the resume ip.
    dynasm!(ops
        ; => epilogue
        ; add rsp, 40
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
    );

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn { _buf: buf, entry: entry_ptr })
}

/// Resolve a jump `target` to a label: an in-region ip uses its own label; an
/// out-of-region ip gets (or reuses) an exit stub label.
fn region_target(
    target: u32,
    start: u32,
    end: u32,
    in_region: &[dynasmrt::DynamicLabel],
    exit_stubs: &mut FxHashMap<u32, dynasmrt::DynamicLabel>,
    ops: &mut dynasmrt::x64::Assembler,
) -> dynasmrt::DynamicLabel {
    if target >= start && target <= end {
        in_region[(target - start) as usize]
    } else {
        *exit_stubs.entry(target).or_insert_with(|| ops.new_dynamic_label())
    }
}

#[derive(Clone, Copy)]
enum DOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// Load `regs[reg]` as an f64 into `xmm{which}` (0 or 1). Int-tagged → cvtsi2sd;
/// a real double → movq; bool/null/undef/heap → jump to `bail`.
fn load_num_xmm(ops: &mut dynasmrt::x64::Assembler, reg: u16, which: u8, bail: dynasmrt::DynamicLabel) {
    let int_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(reg)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32      // 0x7FFA (bool tag)
        ; cmp r10d, 3                            // high16 ∈ [0x7FFA, 0x7FFD] ⇒ not a number
        ; jbe => bail
        ; movq Rx(which), rax                    // double: raw f64 bits
        ; jmp => done
        ; => int_path
        ; cvtsi2sd Rx(which), eax                 // int: low-32 i32 payload
        ; => done
    );
}

/// Store `xmm0` (an f64 result) into `regs[dst]` as a double `Value`.
fn store_xmm(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    dynasm!(ops
        ; movq rax, xmm0
        ; mov [rbx + dreg(dst)], rax
    );
}

/// `regs[dst] = regs[a] <op> regs[b]` in f64. Guards both operands are numbers.
#[allow(clippy::too_many_arguments)]
fn dbinop(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
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
    emit_region_bail(ops, ip, bail, epilogue);
}

/// `regs[dst] = (regs[a] <cmp> regs[b]) as Bool` using f64 ordered comparison
/// (NaN compares false for </<=/>/>=/==, true for !=). Guards both are numbers.
#[allow(clippy::too_many_arguments)]
/// If `c` is a "pending string" constant (`Value::heap(STRING_CONST_BIT | si)`,
/// the form the compiler emits for a string literal) whose text is exactly ONE
/// ASCII byte, return that char's INTERNED Value bits (`Value::heap(byte)` —
/// single ASCII chars live at heap index == their byte; see `Heap::new`). This
/// lets the region materialise `"7"` as the same boxed value `s[i]` yields, so
/// `s[i] === "7"` is a bits compare. Returns `None` for numeric / multi-char /
/// non-ASCII / non-string constants (the region handles numbers; others decline).
fn single_char_const_bits(proto: &FuncProto, c: Value) -> Option<u64> {
    if !c.is_heap() {
        return None;
    }
    let raw = c.heap_index();
    if raw & crate::vm::STRING_CONST_BIT == 0 {
        return None; // a real heap value, not a pending string constant
    }
    let si = (raw & !crate::vm::STRING_CONST_BIT) as usize;
    let bytes = proto.string_constants.get(si)?.as_bytes();
    if bytes.len() == 1 && bytes[0] < 128 {
        Some(Value::heap(bytes[0] as u32).bits())
    } else {
        None
    }
}

/// Polymorphic strict `===` / `!==` (`ne` selects `!==`) for the region's MEMORY
/// path. Operand types are unknown at compile time, so the emitted code branches
/// at runtime:
///   1. EITHER operand is a DOUBLE (NaN-box high16 ∉ [TAG_LO, TAG_HI]) → the f64
///      numeric compare (identical to `dcmp` Eq/Ne) — keeps `0.5===0.5`,
///      `NaN!==NaN`, `0===-0` correct, and bails on a num-vs-non-num operand mix.
///   2. else EITHER operand is HEAP (high16 == 0x7FFD) with index ≥ 129 (a
///      multi-char string or a user object — NOT an interned single-char/empty
///      string) → BAIL to the interpreter (those need full `strict_eq`; raw bits
///      would wrongly distinguish equal-content strings).
///   3. else → 64-bit BITS equality. Exactly JS `===` for Int, Bool, Null,
///      Undefined, and interned single-char/empty strings (indices < 129). This
///      is the `s[i] === "7"` and `charCodeAt === 55` hot path (call-free).
#[allow(clippy::too_many_arguments)]
fn region_poly_eq(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    ne: bool,
) {
    let numeric = ops.new_dynamic_label();
    let a_not_heap = ops.new_dynamic_label();
    let do_bits = ops.new_dynamic_label();
    let store = ops.new_dynamic_label();
    // rax = a_bits, rcx = b_bits (kept live across the type checks).
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        // is a a double?  high16 = rax>>48; double ⇔ (high16 - TAG_LO) > (TAG_HI-TAG_LO)
        ; mov rdx, rax
        ; shr rdx, 48
        ; sub edx, TAG_LO as i32
        ; cmp edx, (TAG_HI - TAG_LO) as i32
        ; ja => numeric                       // a is a double (tag out of tagged range)
        // is b a double?
        ; mov rdx, rcx
        ; shr rdx, 48
        ; sub edx, TAG_LO as i32
        ; cmp edx, (TAG_HI - TAG_LO) as i32
        ; ja => numeric                       // b is a double
        // Neither is a double. Bail if EITHER is a heap value with index ≥ 129
        // (a non-interned string / object — needs full strict_eq).
        ; mov rdx, rax
        ; shr rdx, 48
        ; cmp edx, TAG_HEAP_HI as i32
        ; jne => a_not_heap
        ; mov rdx, rax
        ; mov r9, QWORD PAYLOAD_MASK as i64
        ; and rdx, r9
        ; cmp rdx, USER_OBJ_START as i32
        ; jae => bail                          // a is a non-interned heap value
        ; => a_not_heap
        ; mov rdx, rcx
        ; shr rdx, 48
        ; cmp edx, TAG_HEAP_HI as i32
        ; jne => do_bits
        ; mov rdx, rcx
        ; mov r9, QWORD PAYLOAD_MASK as i64
        ; and rdx, r9
        ; cmp rdx, USER_OBJ_START as i32
        ; jae => bail                          // b is a non-interned heap value
        ; jmp => do_bits
    );
    // ── numeric path (a or b is a double): f64 compare, identical to dcmp. ──
    dynasm!(ops ; => numeric);
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    if ne {
        dynasm!(ops ; ucomisd xmm0, xmm1 ; setne al ; setp cl ; or al, cl);
    } else {
        dynasm!(ops ; ucomisd xmm0, xmm1 ; sete al ; setnp cl ; and al, cl);
    }
    dynasm!(ops ; jmp => store);
    // ── bits path: result = (a_bits <op> b_bits) as Bool. ──
    dynasm!(ops
        ; => do_bits
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        ; cmp rax, rcx
    );
    if ne {
        dynasm!(ops ; setne al);
    } else {
        dynasm!(ops ; sete al);
    }
    dynasm!(ops
        ; => store
        ; movzx rax, al
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
    );
    emit_region_bail(ops, ip, bail, epilogue);
}

fn dcmp(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    cmp: Cmp,
) {
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    // Compute the boolean into al. ucomisd sets CF/ZF/PF; ordering tricks below
    // keep NaN-false semantics (see jump variant for the rationale).
    match cmp {
        Cmp::Lt => dynasm!(ops ; ucomisd xmm1, xmm0 ; seta al),   // a<b  ⇔ b>a ordered
        Cmp::Le => dynasm!(ops ; ucomisd xmm1, xmm0 ; setae al),  // a<=b ⇔ b>=a ordered
        Cmp::Gt => dynasm!(ops ; ucomisd xmm0, xmm1 ; seta al),   // a>b
        Cmp::Ge => dynasm!(ops ; ucomisd xmm0, xmm1 ; setae al),  // a>=b
        Cmp::Eq => dynasm!(ops
            ; ucomisd xmm0, xmm1
            ; sete al            // ZF=1 (equal OR unordered)
            ; setnp cl           // PF=0 (ordered)
            ; and al, cl         // equal AND ordered
        ),
        Cmp::Ne => dynasm!(ops
            ; ucomisd xmm0, xmm1
            ; setne al           // ZF=0 (a≠b)
            ; setp cl            // PF=1 (unordered)
            ; or al, cl          // a≠b OR NaN
        ),
    }
    dynasm!(ops
        ; movzx rax, al
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
    );
    emit_region_bail(ops, ip, bail, epilogue);
}

/// Fused `if !(regs[a] <cmp> regs[b]) goto target` in f64. Guards both numbers.
/// Only Lt/Le are emitted by the compiler (loop guards).
#[allow(clippy::too_many_arguments)]
fn djump_if_not_cmp(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    a: u16,
    b: u16,
    cmp: Cmp,
    target: dynasmrt::DynamicLabel,
) {
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    // Jump when the comparison is FALSE. ucomisd(b,a): CF=1 ⇔ b<a OR unordered.
    match cmp {
        // !(a<b): b<=a or NaN. ucomisd(b,a) then jbe (CF|ZF). NaN sets CF ⇒ jumps.
        Cmp::Lt => dynasm!(ops ; ucomisd xmm1, xmm0 ; jbe => target),
        // !(a<=b): b<a or NaN. ucomisd(b,a) then jb (CF). NaN sets CF ⇒ jumps.
        Cmp::Le => dynasm!(ops ; ucomisd xmm1, xmm0 ; jb => target),
        _ => {}
    }
    emit_region_bail(ops, ip, bail, epilogue);
}

/// Emit a region op's bail block: the success path skips it; the block records
/// the resume ip into `[rsi]` and jumps to the shared epilogue (which restores
/// the 4-push/40-byte frame). Unlike the function JIT no result is set — a
/// region's `run` ignores rax and reads only the resume ip.
fn emit_region_bail(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
) {
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; jmp => done
        ; => bail
        ; mov DWORD [rsi], ip as i32
        ; jmp => epilogue
        ; => done
    );
}
