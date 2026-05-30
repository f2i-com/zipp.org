//! Register VM execution.
//!
//! This module hosts the [`VM`] struct — the execution context holding the
//! call stack, registers, heap arena, intern cache and JIT state — plus the
//! entry points used by the stack-based reference interpreter.
//!
//! The register-based dispatch loop lives in the [`rvm`] submodule and is
//! driven by the [`crate::backend::rcompiler`] bytecode compiler.

pub mod rvm;
mod builtins;
mod indexing;

use regex::RegexBuilder;
use serde_json::Value as JsonValue;
use std::borrow::Cow;
#[cfg(not(any(target_arch = "wasm32", target_arch = "riscv32")))]
use std::time::Duration;
#[cfg(not(any(target_arch = "wasm32", target_arch = "riscv32")))]
use std::time::Instant;
use std::{cell::UnsafeCell, rc::Rc};

use crate::bytecode::Bytecode;
use crate::config::ZippConfig;
use crate::object::{
    make_array, make_hash, unwrap_array, BuiltinFunction, BuiltinFunctionObject,
    CompiledFunctionObject, HashKey, HashObject, Object, PromiseObject, PromiseState,
    SuperRefObject,
};
use crate::value::{obj_into_val, val_inspect, val_to_obj, Heap, Value};

// ── Platform-safe time helpers ────────────────────────────────────────────
// On WASM, use js_sys for Date.now() and Math.random() for real values.
// On native, use std::time.
// On riscv32 (zkVM), time is not available — return deterministic values.

/// Get the current epoch time in milliseconds (platform-safe).
#[cfg(not(any(target_arch = "wasm32", target_arch = "riscv32")))]
pub(super) fn epoch_millis_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn epoch_millis_now() -> f64 {
    js_sys::Date::now()
}

#[cfg(target_arch = "riscv32")]
pub(super) fn epoch_millis_now() -> f64 {
    0.0
}

/// Generate a seed for the xorshift64 RNG (platform-safe).
#[cfg(not(any(target_arch = "wasm32", target_arch = "riscv32")))]
fn rng_seed_now() -> u64 {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x12345678_9abcdef0);
    if seed == 0 { 0x12345678_9abcdef0 } else { seed }
}

#[cfg(target_arch = "wasm32")]
fn rng_seed_now() -> u64 {
    // On WASM, use js_sys::Date::now() as seed for better entropy
    let seed = js_sys::Date::now() as u64;
    if seed == 0 { 0x12345678_9abcdef0 } else { seed }
}

// On riscv32 (zkVM), use a fixed seed — determinism is desired.
#[cfg(target_arch = "riscv32")]
fn rng_seed_now() -> u64 {
    0x12345678_9abcdef0
}

pub const STACK_SIZE: usize = 8192;
pub const GLOBALS_SIZE: usize = 65_536;
pub const MAX_FRAMES: usize = 1024;

/// Inline property cache: (shape_version, slot_index) pairs shared via Rc.
type InlineCacheRef = Option<Rc<crate::object::VmCell<Vec<(u32, u32)>>>>;
/// ZK execution trace step: (clock, pc, opcode, val_a, val_b, val_dst, const_val, aux).
pub type TraceStep = (u64, u64, u8, u64, u64, u64, u64, u64);

// Common error messages — defined once to avoid 14+ duplicate heap allocations.
pub(crate) const ERR_ARRAY_SIZE: &str = "Array size limit exceeded";
pub(crate) const ERR_STRING_LEN: &str = "String length limit exceeded";
pub(crate) const ERR_APPEND_TARGET: &str = "append target must be array";
pub(crate) const ERR_SPREAD_TARGET: &str = "spread target must be array";

// Pre-allocated typeof result strings — zero allocation on every typeof call.
thread_local! {
    static TYPEOF_UNDEFINED: Rc<str> = Rc::from("undefined");
    static TYPEOF_OBJECT: Rc<str> = Rc::from("object");
    static TYPEOF_BOOLEAN: Rc<str> = Rc::from("boolean");
    static TYPEOF_NUMBER: Rc<str> = Rc::from("number");
    static TYPEOF_STRING: Rc<str> = Rc::from("string");
    static TYPEOF_FUNCTION: Rc<str> = Rc::from("function");
}
pub const MAX_ARRAY_SIZE: usize = 1_000_000;
pub const SPARSE_ARRAY_THRESHOLD: usize = 1024;
pub const MAX_STRING_LENGTH: usize = 10_000_000;

/// Which ordering relation [`VM::compare_numeric`] should evaluate.
///
/// A tiny typed enum replaces a leftover `Opcode` reference from the
/// legacy stack-VM code, so the register VM no longer has to import the
/// stack VM's opcode table just to decide `<` vs `>`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NumericCmp {
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, Default)]
pub struct ExecutionQuota {
    pub instructions: u64,
    #[cfg(not(any(target_arch = "wasm32", target_arch = "riscv32")))]
    pub started_at: Option<Instant>,
    #[cfg(target_arch = "wasm32")]
    pub started_at_ms: Option<f64>,
}

#[derive(Debug)]
pub enum VMError {
    StackOverflow,
    StackUnderflow,
    InstructionOutOfBounds(usize),
    ExecutionTimeout(String),
    TypeError(String),
    InvalidOpcode(u8),
    /// Internal sentinel: a `yield` expression suspended the generator.
    /// Carries the yielded NaN-boxed Value.
    Yield(Value),
    /// JavaScript `throw` statement: carries the original thrown Value.
    Throw(Value),
}

#[derive(Clone)]
pub struct SharedGlobals {
    pub(crate) inner: Rc<UnsafeCell<Vec<Value>>>,
    /// One past the highest global index that has been written.
    /// GC only needs to scan 0..high_water_mark instead of all 65536 slots.
    high_water_mark: Rc<std::cell::Cell<usize>>,
    /// Dirty bitset: tracks which global slots were written during VM execution.
    /// Used by the WASM bridge to return only mutated indices, eliminating the
    /// need for deepEqual on unchanged state variables.
    dirty: Rc<UnsafeCell<Vec<u64>>>,
}

impl SharedGlobals {
    fn new() -> Self {
        // Pre-size both `globals` and `dirty` to their final
        // capacity so the `Vec`s never reallocate. Critical because
        // the register dispatch loop and the JIT both cache a raw
        // pointer to `globals.as_mut_ptr()`; a reallocation between
        // dispatch ticks would invalidate that pointer and a later
        // store would touch freed memory.
        //
        // The previous implementation lazy-grew via `resize(idx + 64,
        // …)` to save the up-front 512 KiB, but the growth path could
        // exceed `GLOBALS_SIZE` capacity when `idx` was near 65 535
        // (e.g. `65 535 + 64 = 65 599`), reallocating *despite* the
        // `with_capacity(GLOBALS_SIZE)` hint. Allocating the full
        // 524 288 B (`GLOBALS_SIZE * 8`) up front costs <1 ms on
        // modern hardware and removes the dangling-pointer hazard
        // entirely.
        let mut v = Vec::with_capacity(GLOBALS_SIZE);
        v.resize(GLOBALS_SIZE, Value::UNDEFINED);
        let mut dirty = Vec::with_capacity(GLOBALS_SIZE / 64);
        dirty.resize(GLOBALS_SIZE / 64, 0u64);
        Self {
            inner: Rc::new(UnsafeCell::new(v)),
            high_water_mark: Rc::new(std::cell::Cell::new(0)),
            dirty: Rc::new(UnsafeCell::new(dirty)),
        }
    }

    /// Returns one past the highest global index that has been written.
    /// GC and other scanning loops can use this to avoid iterating all 65536 slots.
    #[inline(always)]
    pub fn high_water_mark(&self) -> usize {
        self.high_water_mark.get()
    }

    /// Raw pointer to the globals data. Used by JIT-compiled code.
    #[cfg(feature = "djit")]
    #[inline(always)]
    pub(crate) fn raw_ptr(&self) -> *mut crate::value::Value {
        unsafe { (*self.inner.get()).as_mut_ptr() }
    }

    /// Global set — writes into the pre-allocated slot. The Vec is
    /// pre-sized to `GLOBALS_SIZE` in `SharedGlobals::new`, so any
    /// `idx < GLOBALS_SIZE` is always in-bounds. A `debug_assert`
    /// catches a future regression that re-introduced a lazy resize
    /// path. In release builds an out-of-range `idx` would have been
    /// rejected upstream by the compiler / validator.
    ///
    /// # Safety
    /// `idx` must be `< GLOBALS_SIZE`. The compiler enforces this for
    /// emitted bytecode (see `RCompiler::ensure_global_slot`) and
    /// the WASM bridge does the same range check.
    #[inline(always)]
    pub(crate) unsafe fn set_unchecked(&self, idx: usize, value: Value) {
        debug_assert!(idx < GLOBALS_SIZE, "global index {idx} out of range");
        let globals = &mut *self.inner.get();
        *globals.get_unchecked_mut(idx) = value;
        let next = idx + 1;
        if next > self.high_water_mark.get() {
            self.high_water_mark.set(next);
        }
        let dirty = &mut *self.dirty.get();
        *dirty.get_unchecked_mut(idx / 64) |= 1u64 << (idx % 64);
    }

    /// Global get — returns UNDEFINED for indices beyond
    /// `GLOBALS_SIZE`. The pre-allocated Vec means any in-range slot
    /// is initialised, so the explicit length check just guards
    /// callers that pass an out-of-range index by accident
    /// (e.g. embedder code that didn't go through the validator).
    #[inline(always)]
    pub(crate) unsafe fn get_unchecked(&self, idx: usize) -> Value {
        let globals = &*self.inner.get();
        if idx < globals.len() {
            *globals.get_unchecked(idx)
        } else {
            Value::UNDEFINED
        }
    }

    /// Check if a specific global slot is dirty.
    #[inline(always)]
    pub fn is_dirty(&self, idx: usize) -> bool {
        let dirty = unsafe { &*self.dirty.get() };
        let word = idx / 64;
        let bit = idx % 64;
        word < dirty.len() && (dirty[word] & (1u64 << bit)) != 0
    }

    /// Clear all dirty bits.
    #[inline]
    pub fn clear_dirty(&self) {
        let dirty = unsafe { &mut *self.dirty.get() };
        for word in dirty.iter_mut() {
            *word = 0;
        }
    }

    /// Roll the high-water mark back to `new_hwm` and clear every slot
    /// beyond it to `Value::UNDEFINED`. Used by
    /// [`crate::engine::ScriptState::restore_globals`] so a snapshot
    /// taken at high-water `N` can fully restore VM globals — including
    /// dropping any new globals the handler installed at slots `>= N`.
    /// The dirty bitset is also rewound so consumers don't see stale
    /// post-snapshot writes flagged after the restore.
    #[inline]
    pub fn truncate_to(&self, new_hwm: usize) {
        let cur = self.high_water_mark.get();
        if new_hwm >= cur {
            return;
        }
        let globals = unsafe { &mut *self.inner.get() };
        let upper = std::cmp::min(globals.len(), cur);
        for slot in &mut globals[new_hwm..upper] {
            *slot = Value::UNDEFINED;
        }
        self.high_water_mark.set(new_hwm);
        let dirty = unsafe { &mut *self.dirty.get() };
        let first_word = new_hwm / 64;
        if first_word < dirty.len() {
            // Mask within the first partial word so we keep dirty bits for
            // any slot still inside the snapshot range.
            let bit = (new_hwm % 64) as u32;
            if bit != 0 {
                let keep_mask = (1u64 << bit) - 1;
                dirty[first_word] &= keep_mask;
            } else {
                dirty[first_word] = 0;
            }
            for w in dirty[(first_word + 1)..].iter_mut() {
                *w = 0;
            }
        }
    }
}

/// Saved caller state for iterative register→register dispatch.
/// Pushed on Call, popped on Return — avoids recursive rdispatch_loop calls.
pub(crate) struct RCallFrame {
    pub(crate) ip: usize,
    pub(crate) inst_ptr: *const u8,
    pub(crate) inst_len: usize,
    pub(crate) constants_raw: *const Vec<Object>,
    pub(crate) constants_values_ptr: *const Value,
    pub(crate) constants_syms_ptr: *const u32,
    pub(crate) sp: usize,
    pub(crate) reg_base: usize,
    pub(crate) max_stack_depth: usize,
    pub(crate) inline_cache: Vec<(u32, u32)>,
    pub(crate) func_cache: *const crate::object::VmCell<Vec<(u32, u32)>>,
    pub(crate) num_cache_slots: u16,
    pub(crate) closure_saves: Vec<(u16, Value)>,
    pub(crate) dst_reg: usize,
    pub(crate) is_self_call: bool,
}

/// Saved state of the caller when entering a compiled function via OpCall.
/// Restored by OpReturn/OpReturnValue without recursing into a nested `run()`.
pub(crate) struct CallFrame {
    ip: usize,
    instructions: Rc<Vec<u8>>,
    pub(crate) constants: Rc<Vec<Object>>,
    pub(crate) locals: Vec<Object>,
    sp: usize,
    inline_cache: Vec<(u32, u32)>,
    max_stack_depth: usize,
    /// The function's persistent cache handle — written back on return.
    /// `None` for functions with no property accesses (num_cache_slots == 0).
    func_cache: InlineCacheRef,
    /// True if the called function is async (return value wrapped in Promise).
    is_async: bool,
}

pub struct VM {
    pub constants: Rc<Vec<Object>>,
    pub instructions: Rc<Vec<u8>>,
    /// NaN-boxed value stack. Each element is 8 bytes (Copy).
    pub stack: Vec<Value>,
    /// Current JIT register window pointer. Updated before JIT execution
    /// and after any helper that may resize the stack.
    pub(crate) jit_regs_ptr: *mut u64,
    pub sp: usize,
    pub ip: usize,
    /// Cached raw pointer to `instructions` data.
    /// Eliminates one indirection per bytecode read (Rc→Vec→data becomes ptr→data).
    /// SAFETY: Must be updated whenever `self.instructions` changes.
    pub(crate) inst_ptr: *const u8,
    /// Length of current instruction buffer (only used in debug_assert checks).
    pub(crate) inst_len: usize,
    pub globals: SharedGlobals,
    pub locals: Vec<Object>,
    pub config: ZippConfig,
    pub(crate) enforce_limits: bool,
    pub quota: ExecutionQuota,
    pub last_popped: Option<Value>,
    pub(crate) arg_buffer: Vec<Value>,
    pub(crate) string_concat_buf: String,
    pub(crate) locals_pool: Vec<Vec<Object>>,
    /// Inline property cache: indexed by cache_slot, stores (shape_version, pair_index).
    /// shape_version 0 means "uncached".
    pub(crate) inline_cache: Vec<(u32, u32)>,
    pub(crate) max_stack_depth: usize,
    /// Call-frame stack for non-recursive function dispatch.
    pub(crate) frames: Vec<CallFrame>,
    /// Iterative register→register call-frame stack. Avoids recursive rdispatch_loop.
    pub(crate) rframes: Vec<RCallFrame>,
    /// Heap for NaN-boxed Value objects (strings, arrays, hashes, etc.).
    pub heap: Heap,
    /// Number of registers used by the top-level program (register VM only).
    /// Register count for the top-level program. Exposed `pub` so
    /// out-of-crate consumers (tier-2 IR tests, external tooling)
    /// can introspect the compiled function shape — read-only from
    /// their perspective; the VM still owns writes via `reset_for_run`.
    pub register_count: u16,
    /// Number of actual arguments passed to the current function (for `arguments` object).
    pub(crate) last_call_nargs: u16,
    /// Raw pointer to pre-converted constants as NaN-boxed Values (register VM only).
    /// Points into the active `constants_values_cache` entry. Set by `preconvert_constants()`.
    pub(crate) constants_values_ptr: *const Value,
    /// Scratch buffer for building constants on cache miss.
    pub(crate) constants_values_buf: Vec<Value>,
    /// Cache of pre-converted constants keyed by `Rc::as_ptr` of the
    /// `Rc<Vec<Object>>` constants. Avoids repeated heap allocation for
    /// string/function constants across recursive calls to the same function.
    pub(crate) constants_values_cache: Vec<(usize, Vec<Value>)>,
    /// Raw pointer to current function's constants (register VM only).
    /// Avoids Rc::clone on every function call. Safe because the Rc in
    /// the CompiledFunctionObject keeps the data alive, and the heap is
    /// append-only during VM execution.
    pub(crate) constants_raw: *const Vec<Object>,
    /// Pre-interned symbol IDs for string constants (register VM only).
    /// `constants_syms_ptr[i]` is the interned symbol ID if constant `i` is a string, else 0.
    /// Eliminates `intern_rc()` hash lookups on property access slow paths.
    pub(crate) constants_syms_buf: Vec<u32>,
    pub(crate) constants_syms_ptr: *const u32,
    pub(crate) constants_syms_cache: Vec<(usize, Vec<u32>)>,
    /// Cached `typeof` result Values — lazily initialized on first use.
    /// Avoids allocating `Rc<str>` on every `typeof` call.
    pub(crate) typeof_undefined: Value,
    pub(crate) typeof_number: Value,
    pub(crate) typeof_string: Value,
    pub(crate) typeof_boolean: Value,
    pub(crate) typeof_function: Value,
    pub(crate) typeof_object: Value,
    pub(crate) typeof_symbol: Value,
    /// Cached method symbol IDs for fast-path dispatch (lazily initialized).
    /// u32::MAX = uninitialized. Symbol IDs are stable across VM lifetime.
    pub(crate) sym_push: u32,
    pub(crate) sym_pop: u32,
    pub(crate) sym_length: u32,
    pub(crate) sym_set: u32,
    pub(crate) sym_get: u32,
    pub(crate) sym_has: u32,
    pub(crate) sym_size: u32,
    pub(crate) sym_shift: u32,
    pub(crate) sym_unshift: u32,
    pub(crate) sym_splice: u32,
    pub(crate) sym_has_own_property: u32,
    pub(crate) sym_then: u32,
    pub(crate) sym_catch: u32,
    /// Fast path for `preconvert_constants`: remembers the last-used constants_raw
    /// key and its resolved pointers. Skips the linear cache scan when the same
    /// function is called repeatedly (e.g. `add()` called 1000× from a loop).
    pub(crate) last_preconvert_key: usize,
    pub(crate) last_preconvert_values_ptr: *const Value,
    pub(crate) last_preconvert_syms_ptr: *const u32,
    /// JIT property cache: cached hash object pointer to skip heap lookup + enum match.
    /// Heap baseline: number of objects that existed after initial compilation.
    /// Used by reset_for_rerun to truncate back to this point.
    pub(crate) fn_call_depth: u32,
    /// Stack of active try-catch handlers. Each entry has:
    /// (catch_ip, exception_reg, inst_ptr, inst_len, reg_base, rframes_depth)
    pub(crate) try_handlers: Vec<(usize, usize, *const u8, usize, usize, usize, *const Vec<Object>)>,
    pub(crate) heap_baseline: usize,
    /// On cache hit, the helper skips straight to values[slot].
    #[cfg(feature = "djit")]
    pub(crate) cached_hash_obj: u64,
    #[cfg(feature = "djit")]
    pub(crate) cached_hash_borrow: *mut u8, // raw pointer to HashObject data (via Rc → RcBox → T)
    /// Cached values.ptr + shape for ultra-fast inline property access.
    /// On hit: single comparison (shape) + direct array read. No pointer chasing.
    #[cfg(feature = "djit")]
    pub(crate) cached_values_ptr: *mut u64,
    #[cfg(feature = "djit")]
    pub(crate) cached_shape: u32,
    /// The `new.target` value for the current constructor call.
    /// Set in `execute_new_with_args_slice` before running the constructor,
    /// saved/restored across nested `new` calls.
    pub(crate) new_target: Value,
    /// Xorshift64 PRNG state for Math.random().
    pub(crate) rng_state: u64,
    /// Direct-mapped intern cache for Map key generation: (i32_value, left_bits) → sym_id.
    #[cfg(feature = "djit")]
    pub(crate) intern_cache: Vec<(u64, i32, u32)>,
    /// Direct-mapped value-bits → sym_id cache for the interpreter's
    /// CallMethod Map.set/get/has fast path. The Map bench's `__bench`
    /// function contains CallMethod and so is rejected by djit; it runs
    /// via the interpreter, where each Map op was paying a per-call
    /// `intern()` (FxHashMap) lookup. Inline-str values have deterministic
    /// NaN-box bits (same content → same bits), so a value_bits-keyed
    /// direct-map cache hits 100% on the second-and-subsequent runs of
    /// any bench/handler that re-uses the same keys.
    pub(crate) inline_sym_cache: Vec<(u64, u32)>,
    /// Direct-mapped sym_id → cached heap-Value cache for interpreter
    /// string materialization. Used when `"prefix" + i` produces a
    /// heap-stored canonical String (length 7+) — without this cache,
    /// every iteration alloc_fast's a fresh heap slot for the same
    /// content. With it, the second-and-later occurrences of any sym_id
    /// reuse the prior heap_idx, skipping the allocation. Slots store
    /// the full NaN-boxed Value bits (heap tag + idx) directly.
    pub(crate) interned_str_value_cache: Vec<(u32, u64)>,
    /// Cached Map object for fast repeated access (skip heap lookup + Box deref + Rc deref).
    #[cfg(feature = "djit")]
    pub(crate) cached_map_obj: u64,
    #[cfg(feature = "djit")]
    pub(crate) cached_map_entries: *mut u8,
    #[cfg(feature = "djit")]
    pub(crate) cached_map_indices: *mut u8,
    /// Direct data pointers: skip 2 dereferences per Map op
    #[cfg(feature = "djit")]
    pub(crate) cached_map_buckets_data: *const u16,  // FlatHashTable.buckets.ptr
    #[cfg(feature = "djit")]
    pub(crate) cached_map_entries_data: *const u8,   // entries Vec.ptr
    #[cfg(feature = "djit")]
    pub(crate) cached_map_mask: u32,                 // FlatHashTable.mask
    /// Pluggable localStorage backend (e.g. SQLite on native).
    pub local_storage: Option<Box<dyn crate::local_storage::LocalStorageBridge>>,
    /// Pluggable XDB database backend (e.g. SQLite on native).
    pub db: Option<Box<dyn crate::db_bridge::DbBridge>>,
    /// Pluggable 2D drawing backend (e.g. vello on native).
    pub draw: Option<Box<dyn crate::draw_bridge::DrawBridge>>,
    /// Pluggable CSS layout backend (e.g. taffy on native).
    pub layout: Option<Box<dyn crate::layout_bridge::LayoutBridge>>,
    /// Pluggable input/event state (e.g. winit on native).
    pub input: Option<Box<dyn crate::input_bridge::InputBridge>>,
    /// Pluggable HTTP backend (server-side).
    pub http: Option<Box<dyn crate::http_bridge::HttpBridge>>,
    /// Pluggable file system backend (server-side, scoped).
    pub fs: Option<Box<dyn crate::fs_bridge::FsBridge>>,
    /// Pluggable environment variable backend (server-side).
    pub env: Option<Box<dyn crate::env_bridge::EnvBridge>>,
    /// Event listeners registered via `window.addEventListener(type, handler)`.
    /// Maps event type (e.g. "keydown") to a list of handler Values (heap refs).
    pub event_listeners: std::collections::HashMap<String, Vec<Value>>,
    /// Pending async host calls queued by `softn.*` builtins.
    pub pending_host_calls: Vec<crate::host_bridge::PendingHostCall>,
    /// Microtask queue. Each entry is (callback, args) — drained at the
    /// end of the current top-level evaluation, before control returns
    /// to the host. Populated by `queueMicrotask(fn)` and by Promise
    /// `.then` chains on previously-pending promises.
    ///
    /// `VecDeque` over `Vec` because draining pulls from the front and
    /// `Vec::remove(0)` is O(n); a deeply queued tick (event chains,
    /// `Promise.then` fan-outs) used to be O(n²) in the queue length.
    pub microtask_queue: std::collections::VecDeque<(Value, Vec<Value>)>,
    /// Callbacks stored by `softn.*` builtins, keyed by host call ID.
    pub host_callbacks: std::collections::HashMap<u32, Value>,
    /// Auto-incrementing ID for host calls.
    pub next_host_call_id: u32,
    /// Counter for synchronous host calls per execution (DoS prevention).
    pub host_call_count: u32,
    /// ZK trace capture: when enabled, records (clk, pc, opcode, val_a, val_b, val_dst, const, aux)
    /// at each instruction for feeding into the STARK prover.
    pub trace_enabled: bool,
    pub trace_steps: Vec<TraceStep>,
    pub trace_clk: u64,
    /// Side-channel for errors raised by JIT helper callbacks. The
    /// helpers can't return `Result` (they cross the `extern "win64"`
    /// boundary into machine code), so before they had no way to
    /// signal an exception other than producing `Value::UNDEFINED`.
    /// That silently turned thrown JS errors, type errors, and
    /// timeouts into a normal `undefined` return — and the host saw
    /// the script "succeed" on a runaway script.
    ///
    /// Now: each helper that can fail stores the `VMError` here and
    /// returns the sentinel undefined; the dispatcher picks the
    /// error up via [`Self::take_jit_error`] after every JIT call
    /// and converts it back into a normal `Result::Err`.
    #[cfg(feature = "djit")]
    pub(crate) jit_error: Option<VMError>,
    /// dynasm-rs JIT compiler (behind `djit` feature).
    #[cfg(feature = "djit")]
    pub djit: crate::djit::DynasmJit,
    /// Tier-2 optimising JIT cache. Sits on top of tier-1; call
    /// dispatch consults it first, falling back to tier-1 and then
    /// the interpreter when no tier-2 code is available for the
    /// function. Gated on the same feature + arch set as the
    /// tier-2 emitter.
    #[cfg(all(feature = "djit", target_arch = "x86_64"))]
    pub tier2: crate::codegen::tier2::Tier2Jit,
    /// Soft-deopt signal from tier-2 emitted code. When a speculation
    /// guard fails, the deopt trampoline calls a runtime helper that
    /// sets this flag and returns. The VM dispatch site checks the
    /// flag after each tier-2 call: on set, it blacklists the
    /// offending function and retries the call through tier-1.
    #[cfg(all(feature = "djit", target_arch = "x86_64"))]
    pub deopt_pending: bool,
    /// Last-callee cache for `djit_call_helper`. When the same
    /// callee is called repeatedly (typical for tight inner loops),
    /// the metadata extraction (heap deref + function-shape match)
    /// and two HashMap lookups (`djit.has_calls` + `djit.get_fn_ptr`)
    /// can be skipped on cache hit. A single-entry cache covers the
    /// monomorphic case that dominates benchmark workloads; the
    /// reset value `callee_bits == 0` (not a valid Value) disables
    /// the cache.
    #[cfg(feature = "djit")]
    pub last_call_callee_bits: u64,
    #[cfg(feature = "djit")]
    pub last_call_fn_ptr: Option<*const u8>,
    #[cfg(feature = "djit")]
    pub last_call_has_calls: bool,
    #[cfg(feature = "djit")]
    pub last_call_instr: *const u8,
    #[cfg(feature = "djit")]
    pub last_call_instr_len: usize,
    #[cfg(feature = "djit")]
    pub last_call_consts_raw: *const Vec<crate::object::Object>,
    #[cfg(feature = "djit")]
    pub last_call_reg_count: u16,
    #[cfg(feature = "djit")]
    pub last_call_takes_this: bool,
    #[cfg(feature = "djit")]
    pub last_call_cache_slots: u16,
    #[cfg(feature = "djit")]
    pub last_call_cv_len: usize,
    #[cfg(feature = "djit")]
    pub last_call_cv_ptr: *const (u16, crate::runtime::value::Value),
    #[cfg(feature = "djit")]
    pub last_call_func_cache:
        *const crate::object::VmCell<Vec<(u32, u32)>>,
    /// Cached preconverted constants for the callee. On cache hit we
    /// pass `last_call_consts_values_ptr` directly to `execute_ptr`
    /// without touching `vm.constants_values_ptr`, avoiding both the
    /// `preconvert_constants` hashing and the caller-state save /
    /// restore pair. Safe for `has_calls == false` callees (whose
    /// native code never reads `vm.constants_*` via helpers); the
    /// `has_calls` path still goes through the full swap.
    #[cfg(feature = "djit")]
    pub last_call_consts_values_ptr: *const crate::runtime::value::Value,
    #[cfg(feature = "djit")]
    pub last_call_consts_syms_ptr: *const u32,
}

// ── JIT helper ABI ─────────────────────────────────────────────────────────
//
// The x86-64 JIT backend (`codegen/djit/x86_64.rs`) emits native calls that
// pass arguments in Windows x64 ABI registers (`rcx`, `rdx`, `r8`, `r9`) and
// reserves the 32-byte shadow space that calling convention requires. Every
// helper defined below is therefore declared `extern "win64"` so the Rust-
// generated callee agrees on register assignment *and* expected stack shape.
//
// `extern "win64"` is a valid ABI spec on any x86-64 Rust target, but the
// shadow-space contract means a sysv64-side caller (`extern "C"` on Linux /
// macOS) calling a `win64`-declared helper would miss the 32-byte reserve
// and clobber its own spill slots. Rather than maintain two parallel ABIs
// for the JIT, the `compile_error!` below refuses non-Windows x86-64 builds
// with the `djit` feature enabled — deployers who need the JIT on Linux or
// macOS should file an issue or rebuild djit's arg-passing around sysv64.
// (AArch64 has no such split — `extern "C"` there is AAPCS64 everywhere.)
#[cfg(all(feature = "djit", target_arch = "x86_64", not(target_os = "windows")))]
compile_error!(
    "djit JIT helpers use extern \"win64\" ABI which is Windows-only. \
     For Linux/macOS x86-64, change the djit feature to use extern \"C\" \
     (System V AMD64) and update djit.rs register assignments + shadow-space \
     reserves, or build with --no-default-features to disable the JIT."
);
#[cfg(all(feature = "djit", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
compile_error!("djit feature requires x86-64 or AArch64 target");

/// Batched property operations: execute multiple AddConstToRegProp + AddRegPropsToRegProp
/// on the same object in a single function call (eliminates per-access call overhead).
/// ops_ptr points to packed operations: [op_type:u8, data...] for each operation.
/// Format: type 0 = AddConst(sym:u32, cache:u32, val_bits:u64), type 1 = AddProps(s1_sym:u16, s2_sym:u16, dst_sym:u16, pad:u16)
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_batched_prop_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    ops_ptr: *const u64,   // packed operation descriptors
    num_ops: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let obj_val = Value::from_bits(obj_bits);
    let num = num_ops as usize;

    if !obj_val.is_heap() { return Value::UNDEFINED.bits(); }
    let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
    let hash_rc = match heap_obj {
        Object::Hash(h) => h,
        _ => return Value::UNDEFINED.bits(),
    };
    let hash = hash_rc.borrow_mut();

    for i in 0..num {
        let desc = *ops_ptr.add(i * 2);     // descriptor word
        let data = *ops_ptr.add(i * 2 + 1); // data word
        let op_type = (desc & 0xFF) as u8;

        if op_type == 0 {
            // AddConst: sym in bits 8-39, cache_slot in bits 40-55
            let sym = ((desc >> 8) & 0xFFFFFFFF) as u32;
            let cache_slot = ((desc >> 40) & 0xFFFF) as usize;
            let add_cv = Value::from_bits(data);

            let slot = if cache_slot < vm.inline_cache.len() {
                let (cs, co) = *vm.inline_cache.get_unchecked(cache_slot);
                if cs == hash.shape_version { co as usize }
                else {
                    match hash.str_slots.get(&sym) {
                        Some(&s) => { *vm.inline_cache.get_unchecked_mut(cache_slot) = (hash.shape_version, s as u32); s }
                        None => continue,
                    }
                }
            } else {
                match hash.str_slots.get(&sym) { Some(&s) => s, None => continue }
            };
            let prop_v = *hash.values.get_unchecked(slot);
            let result = if Value::both_i32(prop_v, add_cv) {
                let a = prop_v.as_i32_unchecked();
                let b = add_cv.as_i32_unchecked();
                match a.checked_add(b) {
                    Some(sum) => Value::from_i32(sum),
                    None => Value::from_f64(a as f64 + b as f64),
                }
            } else {
                Value::from_f64(prop_v.to_number() + add_cv.to_number())
            };
            *hash.values.get_unchecked_mut(slot) = result;
            hash.pairs_dirty = true;
        } else if op_type == 1 {
            // AddProps: s1_sym bits 8-23, s2_sym bits 24-39, dst_sym bits 40-55
            let s1_sym = ((desc >> 8) & 0xFFFF) as u32;
            let s2_sym = ((desc >> 24) & 0xFFFF) as u32;
            let dst_sym = ((desc >> 40) & 0xFFFF) as u32;
            let v1 = hash.get_by_sym(s1_sym).unwrap_or(Value::UNDEFINED);
            let v2 = hash.get_by_sym(s2_sym).unwrap_or(Value::UNDEFINED);
            let result = if Value::both_i32(v1, v2) {
                let a = v1.as_i32_unchecked();
                let b = v2.as_i32_unchecked();
                match a.checked_add(b) {
                    Some(sum) => Value::from_i32(sum),
                    None => Value::from_f64(a as f64 + b as f64),
                }
            } else {
                Value::from_f64(v1.to_number() + v2.to_number())
            };
            if let Some(&slot) = hash.str_slots.get(&dst_sym) {
                *hash.values.get_unchecked_mut(slot) = result;
                hash.pairs_dirty = true;
            }
        }
    }
    Value::UNDEFINED.bits()
}

/// Layout offsets for JIT inline property access (no helper call on cache hit).
#[cfg(feature = "djit")]
#[derive(Clone, Copy, Debug)]
pub struct JitLayout {
    pub stack_ptr_offset: usize,    // offset of stack Vec data pointer within VM
    pub jit_regs_ptr_offset: usize, // offset of jit_regs_ptr within VM
    pub ic_offset: usize,           // offset of inline_cache Vec within VM
    pub heap_offset: usize,         // offset of heap within VM
    pub object_size: usize,         // sizeof(Object)
    pub hash_discriminant: u8,      // enum tag for Object::Hash
    pub hash_rc_offset: usize,      // offset of Rc ptr within Object::Hash variant (after tag)
    pub rcbox_data_offset: usize,   // offset of T within RcBox<T> (after strong+weak)
    pub shape_version_offset: usize,// offset of shape_version within HashObject
    pub values_offset: usize,       // offset of values Vec within HashObject
    pub pairs_dirty_offset: usize,  // offset of pairs_dirty within HashObject
    pub cached_obj_offset: usize,   // offset of cached_hash_obj within VM
    pub cached_borrow_offset: usize,// offset of cached_hash_borrow within VM
    pub array_discriminant: u8,     // enum tag for Object::Array
    pub cached_values_offset: usize,  // offset of cached_values_ptr within VM
    pub cached_shape_offset: usize,   // offset of cached_shape within VM
    pub rope_discriminant: u8,        // enum tag for Object::StringRope
    pub rope_total_len_offset: usize, // offset of total_len within StringRopeNode in Object
    // Heap free_list offsets (for inline rope allocation)
    pub free_list_len_offset: usize,  // heap_offset + offset of free_list.len within Heap
    pub free_list_ptr_offset: usize,  // heap_offset + offset of free_list.ptr within Heap
    pub objects_len_offset: usize,    // heap_offset + offset of objects.len within Heap
    pub heap_tag: u64,                // QNAN | (TAG_HEAP << TAG_SHIFT) for from_heap encoding
    // Bump allocator offsets
    pub bump_next_offset: usize,      // heap_offset + offset of bump_next within Heap
    pub bump_end_offset: usize,       // heap_offset + offset of bump_end within Heap
    // Map intern cache offsets (for inline cache probe in JIT)
    pub intern_cache_ptr_offset: usize, // VM.intern_cache Vec.ptr
    pub cached_map_obj_offset: usize,   // VM.cached_map_obj
    pub cached_map_entries_offset: usize, // VM.cached_map_entries
    pub cached_map_indices_offset: usize, // VM.cached_map_indices
    pub cached_buckets_data_offset: usize, // VM.cached_map_buckets_data
    pub cached_entries_data_offset: usize, // VM.cached_map_entries_data
    pub cached_mask_offset: usize,         // VM.cached_map_mask
    // FlatHashTable layout offsets (for fully inline Map.get/set in JIT)
    pub ft_buckets_ptr_off: usize,  // offset of buckets Vec.ptr within FlatHashTable
    // (chain fields removed — FlatHashTable now uses open addressing)
    pub ft_mask_off: usize,         // offset of mask within FlatHashTable
    pub ft_count_off: usize,        // offset of count within FlatHashTable
    pub map_entry_size: usize,      // sizeof((HashKey, Value))
    pub map_entry_value_off: usize, // offset of Value within (HashKey, Value)
    pub hashkey_sym_disc: u8,       // discriminant byte for HashKey::Sym
    pub hashkey_sym_val_off: usize, // offset of u32 within HashKey::Sym variant
}

#[cfg(feature = "djit")]
pub fn jit_layout() -> JitLayout {
    use crate::object::{HashObject, Object, VmCell};
    use std::rc::Rc;

    // Verify Vec<T> internal layout assumptions used by JIT codegen.
    // JIT assumes: Vec.ptr at offset 8, Vec.len at offset 16 (cap at 0).
    // This is the Rust 1.80+ layout. If it changes, the JIT would silently break.
    {
        let probe: Vec<u64> = vec![0xDEAD_CAFE_u64];
        let base = &probe as *const Vec<u64> as *const u8;
        let data_ptr = probe.as_ptr() as usize;
        let ptr_at_8 = unsafe { *(base.add(8) as *const usize) };
        let len_at_16 = unsafe { *(base.add(16) as *const usize) };
        assert_eq!(ptr_at_8, data_ptr, "Vec layout changed: ptr not at offset 8");
        assert_eq!(len_at_16, 1, "Vec layout changed: len not at offset 16");
    }

    // Vec.ptr is at offset 8 within Vec (verified above)
    let stack_ptr_offset = std::mem::offset_of!(VM, stack) + 8;
    let ic_offset = std::mem::offset_of!(VM, inline_cache);
    let heap_offset = std::mem::offset_of!(VM, heap);
    let object_size = std::mem::size_of::<Object>();

    // Create a real Hash object to measure layout via pointer arithmetic
    let hash_obj = HashObject::default();
    let hash_rc = Rc::new(VmCell::new(hash_obj));
    let obj = Object::Hash(hash_rc.clone());

    // Hash discriminant (first byte of enum)
    let obj_ptr = &obj as *const Object as *const u8;
    let hash_discriminant = unsafe { *obj_ptr };

    // Rc pointer offset within the Object enum variant
    // For repr(Rust) enums with pointer-size data, the Rc is at offset 8
    // (1-byte discriminant + 7 bytes alignment padding)
    let rc_in_obj = {
        8usize
    };

    // RcBox layout: strong (8) + weak (8) + data at offset 16
    let rcbox_data_offset = std::mem::size_of::<usize>() * 2; // 16

    // HashObject field offsets
    let hash_ref = hash_rc.borrow();
    let hash_base = hash_ref as *const HashObject as usize;
    let shape_version_offset = &hash_ref.shape_version as *const u32 as usize - hash_base;
    let values_offset = &hash_ref.values as *const Vec<crate::value::Value> as usize - hash_base;
    let pairs_dirty_offset = &hash_ref.pairs_dirty as *const bool as usize - hash_base;

    let cached_obj_offset = std::mem::offset_of!(VM, cached_hash_obj);
    let cached_borrow_offset = std::mem::offset_of!(VM, cached_hash_borrow);

    // Array discriminant
    let arr_obj = Object::Array(Rc::new(VmCell::new(Vec::new())));
    let array_discriminant = unsafe { *(&arr_obj as *const Object as *const u8) };

    let jit_regs_ptr_offset = std::mem::offset_of!(VM, jit_regs_ptr);
    JitLayout {
        stack_ptr_offset,
        jit_regs_ptr_offset,
        ic_offset,
        heap_offset,
        object_size,
        hash_discriminant,
        hash_rc_offset: rc_in_obj,
        rcbox_data_offset,
        shape_version_offset,
        values_offset,
        pairs_dirty_offset,
        cached_obj_offset,
        cached_borrow_offset,
        array_discriminant,
        cached_values_offset: std::mem::offset_of!(VM, cached_values_ptr),
        cached_shape_offset: std::mem::offset_of!(VM, cached_shape),
        rope_discriminant: {
            let rope_obj = Object::StringRope(crate::object::StringRopeNode {
                left: crate::value::Value::UNDEFINED,
                right: crate::value::Value::UNDEFINED,
                total_len: 0,
            });
            unsafe { *(&rope_obj as *const Object as *const u8) }
        },
        rope_total_len_offset: {
            // StringRopeNode { left: Value(8), right: Value(8), total_len: usize(8) }
            // In Object enum with #[repr(u8)]: disc(1) + pad(7) + left(8) + right(8) + total_len(8)
            8 + 8 + 8 // = 24
        },
        // Heap field offsets — use offset_of! for correctness (Rust may reorder fields)
        free_list_len_offset: {
            let fl_off = std::mem::offset_of!(crate::value::Heap, free_list);
            heap_offset + fl_off + 16 // Vec<u32>.len at Vec offset 16
        },
        free_list_ptr_offset: {
            let fl_off = std::mem::offset_of!(crate::value::Heap, free_list);
            heap_offset + fl_off + 8 // Vec<u32>.ptr at Vec offset 8
        },
        objects_len_offset: {
            let obj_off = std::mem::offset_of!(crate::value::Heap, objects);
            heap_offset + obj_off + 16 // Vec<Object>.len at Vec offset 16
        },
        heap_tag: 0x7FFE_0000_0000_0000u64,          // QNAN | (TAG_HEAP << 48)
        bump_next_offset: heap_offset + std::mem::offset_of!(crate::value::Heap, bump_next),
        bump_end_offset: heap_offset + std::mem::offset_of!(crate::value::Heap, bump_end),
        intern_cache_ptr_offset: std::mem::offset_of!(VM, intern_cache) + 8, // Vec.ptr at offset 8
        cached_map_obj_offset: std::mem::offset_of!(VM, cached_map_obj),
        cached_map_entries_offset: std::mem::offset_of!(VM, cached_map_entries),
        cached_map_indices_offset: std::mem::offset_of!(VM, cached_map_indices),
        cached_buckets_data_offset: std::mem::offset_of!(VM, cached_map_buckets_data),
        cached_entries_data_offset: std::mem::offset_of!(VM, cached_map_entries_data),
        cached_mask_offset: std::mem::offset_of!(VM, cached_map_mask),
        ft_buckets_ptr_off: std::mem::offset_of!(crate::object::FlatHashTable, buckets) + 8,
        // (chain offsets removed — open addressing has no chain)
        ft_mask_off: std::mem::offset_of!(crate::object::FlatHashTable, mask),
        ft_count_off: std::mem::offset_of!(crate::object::FlatHashTable, count),
        map_entry_size: {
            std::mem::size_of::<(crate::object::HashKey, crate::value::Value)>()
        },
        map_entry_value_off: {
            let entry = (crate::object::HashKey::Null, crate::value::Value::NULL);
            let base = &entry as *const _ as usize;
            let val_ptr = &entry.1 as *const _ as usize;
            val_ptr - base
        },
        hashkey_sym_disc: {
            let key = crate::object::HashKey::Sym(0);
            unsafe { *(&key as *const _ as *const u8) }
        },
        hashkey_sym_val_off: {
            let key = crate::object::HashKey::Sym(42);
            // Find the offset of the u32 value within HashKey::Sym by scanning for 42
            let bytes = unsafe { std::slice::from_raw_parts(&key as *const _ as *const u8, std::mem::size_of::<crate::object::HashKey>()) };
            let mut off = 0;
            for (i, &_b) in bytes.iter().enumerate() {
                if i >= 1 && bytes[i..].starts_with(&42u32.to_ne_bytes()) {
                    off = i;
                    break;
                }
            }
            off
        },
    }
}

/// Call helper for dynasm JIT. Called from native JIT code when it encounters
/// a Call opcode. Dispatches the callee (JIT or interpreter) and returns the result.
///
/// Windows x64 ABI: rcx=vm_ptr, rdx=callee_bits, r8=args_ptr, r9=nargs
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_call_helper(
    vm_raw: *mut u8,
    callee_bits: u64,
    args_ptr: *const u64,
    nargs_u64: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let callee = Value::from_bits(callee_bits);
    let nargs = nargs_u64 as usize;

    // Monomorphic call-site cache: tight inner loops call the same
    // callee thousands of times. Skip the heap deref + match + two
    // `vm.djit.*` HashMap lookups (has_calls, get_fn_ptr) and the
    // preconvert-constants hash on hit.
    let (instr, instr_len, consts_raw, reg_count, takes_this,
         cv_ptr, cv_len, cache_slots, func_cache,
         cached_fn_ptr, cached_has_calls,
         cached_values_ptr, cached_syms_ptr);
    if callee_bits == vm.last_call_callee_bits && callee_bits != 0 {
        instr = vm.last_call_instr;
        instr_len = vm.last_call_instr_len;
        consts_raw = vm.last_call_consts_raw;
        reg_count = vm.last_call_reg_count as usize;
        takes_this = vm.last_call_takes_this;
        cv_ptr = vm.last_call_cv_ptr;
        cv_len = vm.last_call_cv_len;
        cache_slots = vm.last_call_cache_slots;
        func_cache = vm.last_call_func_cache;
        cached_fn_ptr = vm.last_call_fn_ptr;
        cached_has_calls = vm.last_call_has_calls;
        cached_values_ptr = vm.last_call_consts_values_ptr;
        cached_syms_ptr = vm.last_call_consts_syms_ptr;
    } else {
        if !callee.is_heap() {
            return Value::UNDEFINED.bits();
        }
        let idx = callee.heap_index();
        let extracted = {
            let obj = vm.heap.get(idx);
            match obj {
                Object::CompiledFunction(func) if func.register_count > 0 => Some((
                    func.instructions.as_ptr(),
                    func.instructions.len(),
                    &*func.constants as *const Vec<Object>,
                    func.register_count as usize,
                    func.takes_this,
                    func.captured_values.as_ptr(),
                    func.captured_values.len(),
                    func.num_cache_slots,
                    Rc::as_ptr(&func.inline_cache),
                )),
                _ => None,
            }
        };
        let Some((i, il, cr, rc, tt, cvp, cvl, cs, fc)) = extracted else {
            return Value::UNDEFINED.bits();
        };
        instr = i;
        instr_len = il;
        consts_raw = cr;
        reg_count = rc;
        takes_this = tt;
        cv_ptr = cvp;
        cv_len = cvl;
        cache_slots = cs;
        func_cache = fc;
        let key = instr as usize;
        cached_fn_ptr = vm.djit.get_fn_ptr(key);
        cached_has_calls = vm.djit.has_calls(key);
        // Preconvert callee's constants once, then cache the
        // resulting values_ptr / syms_ptr so subsequent calls skip
        // the hash + lookup on the preconvert-cache fast path.
        let saved_cr_for_preconv = vm.constants_raw;
        vm.constants_raw = consts_raw;
        vm.preconvert_constants();
        cached_values_ptr = vm.constants_values_ptr;
        cached_syms_ptr = vm.constants_syms_ptr;
        vm.constants_raw = saved_cr_for_preconv;
        // Populate cache. Using callee_bits as the match key makes
        // the fast path a single integer compare.
        vm.last_call_callee_bits = callee_bits;
        vm.last_call_fn_ptr = cached_fn_ptr;
        vm.last_call_has_calls = cached_has_calls;
        vm.last_call_instr = instr;
        vm.last_call_instr_len = instr_len;
        vm.last_call_consts_raw = consts_raw;
        vm.last_call_reg_count = reg_count as u16;
        vm.last_call_takes_this = takes_this;
        vm.last_call_cache_slots = cache_slots;
        vm.last_call_cv_len = cv_len;
        vm.last_call_cv_ptr = cv_ptr;
        vm.last_call_func_cache = func_cache;
        vm.last_call_consts_values_ptr = cached_values_ptr;
        vm.last_call_consts_syms_ptr = cached_syms_ptr;
    }

    let arg_offset = if takes_this { 1 } else { 0 };
    let reg_window = reg_count.max(1);
    let new_reg_base = vm.sp;
    let needed = new_reg_base + reg_window;

    if needed > STACK_SIZE {
        return Value::UNDEFINED.bits();
    }
    while vm.stack.len() < needed {
        vm.stack.push(Value::UNDEFINED);
    }

    // Compute the callee register-window pointer once; arg copy, zero
    // of tail regs, and the eventual `execute_ptr` all use it.
    let regs_base_ptr = vm.stack.as_mut_ptr().add(new_reg_base);

    // Set `this` to undefined for regular calls
    if takes_this {
        *regs_base_ptr = Value::UNDEFINED;
    }
    // Copy args
    for i in 0..nargs {
        *regs_base_ptr.add(arg_offset + i) = Value::from_bits(*args_ptr.add(i));
    }
    // Zero remaining registers
    for i in (nargs + arg_offset)..reg_window {
        *regs_base_ptr.add(i) = Value::UNDEFINED;
    }

    // Inject closure captures (stack-allocated to avoid heap alloc per call)
    let mut closure_buf: [(u16, Value); 8] = [(0, Value::UNDEFINED); 8];
    let cv_count = cv_len.min(8);
    #[allow(clippy::needless_range_loop)] // raw pointer indexing + parallel array
    for i in 0..cv_count {
        let (slot, val) = *cv_ptr.add(i);
        let old = vm.globals.get_unchecked(slot as usize);
        vm.globals.set_unchecked(slot as usize, val);
        closure_buf[i] = (slot, old);
    }

    // Advance sp past callee's register window for nested calls
    let saved_sp = vm.sp;
    vm.sp = new_reg_base + reg_window;

    // Swap in callee's per-function inline cache (mirrors interpreter Call path).
    // Without this, GetProp/SetProp in the callee use stale cache entries.
    // For cache_slots == 0 callees (pure-compute functions with no
    // property access), skip the swap entirely — they can't touch
    // the cache, so leaving the caller's cache visible is both safe
    // and avoids the Vec::take/replace pair on every call.
    let saved_ic = if cache_slots > 0 {
        let taken = (*func_cache).borrow_mut();
        if taken.is_empty() {
            std::mem::replace(&mut vm.inline_cache, vec![(0, 0); cache_slots as usize])
        } else {
            std::mem::replace(&mut vm.inline_cache, std::mem::take(&mut *taken))
        }
    } else {
        Vec::new()
    };

    // Check if callee is JIT'd → native call. The cached fn_ptr
    // (populated above on first call or matched cache) avoids the
    // HashMap lookup on repeated calls to the same callee.
    let result = if let Some(fn_ptr) = cached_fn_ptr {
        let has_calls = cached_has_calls;
        let r = if has_calls {
            // Helpers inside the callee may read `vm.constants_*`,
            // so we have to swap state and restore afterwards.
            let saved_cr = vm.constants_raw;
            let saved_cvp = vm.constants_values_ptr;
            let saved_csp = vm.constants_syms_ptr;
            vm.constants_raw = consts_raw;
            vm.constants_values_ptr = cached_values_ptr;
            vm.constants_syms_ptr = cached_syms_ptr;
            let r = crate::djit::DynasmJit::execute_ptr_with_vm(
                fn_ptr,
                regs_base_ptr as *mut u64,
                cached_values_ptr as *const u64,
                vm.globals.raw_ptr() as *mut u64,
                vm_raw,
            );
            vm.constants_raw = saved_cr;
            vm.constants_values_ptr = saved_cvp;
            vm.constants_syms_ptr = saved_csp;
            r
        } else {
            // Pure-compute callee — its emitted code never reads
            // `vm.constants_*` via helpers, so we pass the callee's
            // preconverted values pointer directly through rdx and
            // skip the caller-state swap/restore entirely.
            crate::djit::DynasmJit::execute_ptr(
                fn_ptr,
                regs_base_ptr as *mut u64,
                cached_values_ptr as *const u64,
                vm.globals.raw_ptr() as *mut u64,
            )
        };
        r
    } else {
        // Not JIT'd → run interpreter
        let saved_ip = vm.ip;
        let saved_inst_ptr = vm.inst_ptr;
        let saved_inst_len = vm.inst_len;
        let saved_cr = vm.constants_raw;
        let saved_cvp = vm.constants_values_ptr;
        let saved_csp = vm.constants_syms_ptr;
        let saved_max_depth = vm.max_stack_depth;

        vm.inst_ptr = instr;
        vm.inst_len = instr_len;
        vm.constants_raw = consts_raw;
        vm.preconvert_constants();
        // vm.sp already set above
        vm.ip = 0;

        let entry_depth = vm.rframes.len();
        let r = match vm.rdispatch_loop(entry_depth, new_reg_base) {
            Ok(()) => vm.last_popped.take().unwrap_or(Value::UNDEFINED).bits(),
            Err(e) => {
                // Stash the error so the dispatcher's post-JIT
                // `take_jit_error()` check converts it back to a real
                // `Result::Err`. Without this, a thrown JS error
                // would silently look like a successful `undefined`.
                vm.jit_error.get_or_insert(e);
                Value::UNDEFINED.bits()
            },
        };

        vm.ip = saved_ip;
        vm.inst_ptr = saved_inst_ptr;
        vm.inst_len = saved_inst_len;
        vm.constants_raw = saved_cr;
        vm.constants_values_ptr = saved_cvp;
        vm.constants_syms_ptr = saved_csp;
        vm.max_stack_depth = saved_max_depth;
        r
    };

    // Write back callee's inline cache and restore caller's. When
    // cache_slots == 0 there was no swap, so there's nothing to
    // restore — `saved_ic` is a dummy empty Vec we drop on the floor.
    if cache_slots > 0 {
        let our_cache = std::mem::replace(&mut vm.inline_cache, saved_ic);
        let fc = (*func_cache).borrow_mut();
        if fc.is_empty() { *fc = our_cache; }
    }

    // Invalidate cached hash/map — callee may have modified objects.
    // The JIT's post-call `mov QWORD [r15 + co_off], 0` also invalidates,
    // but we zero here too for the interpreter→JIT path. Pure-compute
    // callees (`has_calls == false`) can't allocate or mutate heap
    // objects, so the defensive zero is unnecessary for them.
    if cached_has_calls {
        vm.cached_hash_obj = 0;
        vm.cached_map_obj = 0;
    }

    // Restore sp
    vm.sp = saved_sp;

    // Restore closure captures
    #[allow(clippy::needless_range_loop)]
    for i in 0..cv_count {
        let (slot, old_val) = closure_buf[i];
        vm.globals.set_unchecked(slot as usize, old_val);
    }

    result
}

/// Constructor call: `new Ctor(args)`. Returns NaN-boxed result.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_new_helper(
    vm_raw: *mut u8,
    ctor_bits: u64,
    args_ptr: *const u64,
    nargs: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let nargs = nargs as usize;
    let ctor_val = Value::from_bits(ctor_bits);
    let ctor = val_to_obj(ctor_val, &vm.heap);
    let mut args = Vec::with_capacity(nargs);
    for i in 0..nargs {
        args.push(Value::from_bits(*args_ptr.add(i)));
    }
    let saved_ic = std::mem::take(&mut vm.inline_cache);
    let saved_cr = vm.constants_raw;
    let saved_cvp = vm.constants_values_ptr;
    let saved_csp = vm.constants_syms_ptr;
    let saved_ip = vm.ip;
    let saved_inst_ptr = vm.inst_ptr;
    let saved_inst_len = vm.inst_len;
    let saved_sp = vm.sp;
    let saved_max_depth = vm.max_stack_depth;
    let saved_nargs = vm.last_call_nargs;
    let r = match vm.execute_new_with_args_slice(ctor, &args) {
        Ok(()) => vm.pop_val().unwrap_or(Value::UNDEFINED).bits(),
        Err(e) => {
            // Stash the error so the dispatcher's post-JIT
            // `take_jit_error()` check converts it back to a real
            // `Result::Err`. Without this, a thrown JS error
            // would silently look like a successful `undefined`.
            vm.jit_error.get_or_insert(e);
            Value::UNDEFINED.bits()
        },
    };
    vm.inline_cache = saved_ic;
    vm.constants_raw = saved_cr;
    vm.constants_values_ptr = saved_cvp;
    vm.constants_syms_ptr = saved_csp;
    vm.ip = saved_ip;
    vm.inst_ptr = saved_inst_ptr;
    vm.inst_len = saved_inst_len;
    vm.sp = saved_sp;
    vm.max_stack_depth = saved_max_depth;
    vm.last_call_nargs = saved_nargs;
    vm.cached_hash_obj = 0;
    vm.cached_map_obj = 0;
    r
}

/// Method call: obj.method(args). Returns NaN-boxed result.
/// packed_info: nargs in low 8 bits, prop_const_idx in bits 8-23
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_call_method_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    args_ptr: *const u64,
    packed_info: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let nargs = (packed_info & 0xFF) as usize;
    let prop_idx = ((packed_info >> 8) & 0xFFFF) as usize;
    let obj_val = Value::from_bits(obj_bits);

    // Get property symbol directly from constants array
    let constants = &*vm.constants_raw;
    let prop_sym = if prop_idx < constants.len() {
        match &constants[prop_idx] {
            Object::String(s) => crate::intern::intern_rc(s),
            _ => *vm.constants_syms_ptr.add(prop_idx),
        }
    } else {
        *vm.constants_syms_ptr.add(prop_idx)
    };

    // ── Map fast path: use cached Map pointers if available ──
    if obj_bits == vm.cached_map_obj && !vm.cached_map_entries.is_null() {
        let entries = &mut *(vm.cached_map_entries as *mut Vec<(crate::object::HashKey, Value)>);
        let indices = &mut *(vm.cached_map_indices as *mut crate::object::FlatHashTable);
        if prop_sym == vm.sym_set && nargs >= 2 {
            let key = vm.hash_key_from_value(Value::from_bits(*args_ptr.add(0)));
            let value = Value::from_bits(*args_ptr.add(1));
            VM::map_insert_or_replace(entries, indices, key, value);
            return obj_val.bits();
        }
        if prop_sym == vm.sym_get && nargs >= 1 {
            let key = vm.hash_key_from_value(Value::from_bits(*args_ptr.add(0)));
            return VM::map_get(entries, indices, &key).unwrap_or(Value::UNDEFINED).bits();
        }
    }
    if obj_val.is_heap() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Map(map_obj) = heap_obj {
            if prop_sym == vm.sym_set && nargs >= 2 {
                let key = vm.hash_key_from_value(Value::from_bits(*args_ptr.add(0)));
                let value = Value::from_bits(*args_ptr.add(1));
                let entries = map_obj.entries.borrow_mut();
                let indices = map_obj.indices.borrow_mut();
                VM::map_insert_or_replace(entries, indices, key, value);
                return obj_val.bits(); // Map.set returns the map
            }
            if prop_sym == vm.sym_get && nargs >= 1 {
                let key = vm.hash_key_from_value(Value::from_bits(*args_ptr.add(0)));
                let result = VM::map_get(map_obj.entries.borrow(), map_obj.indices.borrow(), &key);
                return result.unwrap_or(Value::UNDEFINED).bits();
            }
        }
    }

    // ── Array fast path ──
    if obj_val.is_heap() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Array(arr_rc) = heap_obj {
            if prop_sym == vm.sym_push && nargs >= 1 {
                let items = arr_rc.borrow_mut();
                for i in 0..nargs {
                    items.push(Value::from_bits(*args_ptr.add(i)));
                }
                return Value::from_i64(items.len() as i64).bits();
            }
            if prop_sym == vm.sym_pop {
                let items = arr_rc.borrow_mut();
                return items.pop().unwrap_or(Value::UNDEFINED).bits();
            }
        }
    }

    // ── Generic method call fallback ──
    // Route through call_method_with_sym which uses the interpreter's full
    // method dispatch (handles Array.filter, .map, etc. correctly).
    let arg_start = vm.sp;
    let needed = arg_start + nargs;
    if vm.stack.len() < needed {
        vm.stack.resize(needed, Value::UNDEFINED);
    }
    for i in 0..nargs {
        unsafe { *vm.stack.get_unchecked_mut(arg_start + i) = Value::from_bits(*args_ptr.add(i)) };
    }
    // Save ALL VM state
    let saved_ic = std::mem::take(&mut vm.inline_cache);
    let saved_cr = vm.constants_raw;
    let saved_cvp = vm.constants_values_ptr;
    let saved_csp = vm.constants_syms_ptr;
    let saved_ip = vm.ip;
    let saved_inst_ptr = vm.inst_ptr;
    let saved_inst_len = vm.inst_len;
    let saved_sp = vm.sp;
    let saved_max_depth = vm.max_stack_depth;
    let saved_nargs = vm.last_call_nargs;
    let r = match vm.call_method_with_sym(obj_val, prop_sym, nargs, arg_start) {
        Ok(result) => result.bits(),
        Err(e) => {
            // Stash the error so the dispatcher's post-JIT
            // `take_jit_error()` check converts it back to a real
            // `Result::Err`. Without this, a thrown JS error
            // would silently look like a successful `undefined`.
            vm.jit_error.get_or_insert(e);
            Value::UNDEFINED.bits()
        },
    };
    // Restore ALL saved state
    vm.inline_cache = saved_ic;
    vm.constants_raw = saved_cr;
    vm.constants_values_ptr = saved_cvp;
    vm.constants_syms_ptr = saved_csp;
    vm.ip = saved_ip;
    vm.inst_ptr = saved_inst_ptr;
    vm.inst_len = saved_inst_len;
    vm.sp = saved_sp;
    vm.max_stack_depth = saved_max_depth;
    vm.last_call_nargs = saved_nargs;
    vm.cached_hash_obj = 0;
    vm.cached_map_obj = 0;
    r
}

/// Ultra-fast string rope append: creates a rope from left + right.
/// Skips is_number check. For rope + inline_str, computes length efficiently.
#[cfg(feature = "djit")]
#[inline(always)]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_rope_append_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let left = Value::from_bits(left_bits);
    let right = Value::from_bits(right_bits);

    // Fast path: if right is inline str (very common: + "a"), get length directly
    let right_len = if right.is_inline_str() {
        right.inline_str_len()
    } else {
        crate::object::string_val_len(right, &vm.heap)
    };

    // Fast path: if left is heap rope, read total_len from the Object directly
    let left_len = if left.is_heap() {
        let obj = &*vm.heap.objects.as_ptr().add(left.heap_index() as usize);
        match obj {
            Object::StringRope(r) => r.total_len,
            Object::String(s) => s.len(),
            _ => crate::object::string_val_len(left, &vm.heap),
        }
    } else if left.is_inline_str() {
        left.inline_str_len()
    } else {
        0
    };

    let total = left_len + right_len;
    if total <= 16 {
        return match vm.add_string_or_object(left, right) {
            Ok(v) => v.bits(),
            Err(e) => {
                // Stash the error so the dispatcher's post-JIT
                // `take_jit_error()` check converts it back to a real
                // `Result::Err`. Without this, a thrown JS error
                // would silently look like a successful `undefined`.
                vm.jit_error.get_or_insert(e);
                Value::UNDEFINED.bits()
            },
        };
    }
    // Direct rope creation (no type dispatch, no add_string_or_object)
    // Ensure bump region is ready for subsequent JIT-inlined allocations.
    // Pass `max_heap_objects` through so a tight sandbox limit caps the
    // bump region — the previous unbounded `ensure_bump_capacity(2048)`
    // could overshoot the configured ceiling between periodic
    // safepoints. With the budget-aware variant, the bump grows in
    // smaller increments as the limit approaches and stops growing once
    // it's reached, deferring the abort to the next periodic check.
    if vm.heap.bump_next >= vm.heap.bump_end {
        vm.heap
            .ensure_bump_capacity_within(2048, vm.config.max_heap_objects);
    }
    let rope = Object::StringRope(crate::object::StringRopeNode {
        left, right, total_len: total,
    });
    Value::from_heap(vm.heap.alloc_fast(rope)).bits()
}

/// Minimal rope allocation — caller already computed total_len.
/// Skips all type checks. Just allocate the rope node and return NaN-boxed value.
#[cfg(feature = "djit")]
#[inline(never)]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_rope_alloc_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    total_len: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let rope = Object::StringRope(crate::object::StringRopeNode {
        left: Value::from_bits(left_bits),
        right: Value::from_bits(right_bits),
        total_len: total_len as usize,
    });
    Value::from_heap(vm.heap.alloc_fast(rope)).bits()
}

/// Specialized string Add — skips numeric check, goes straight to rope path.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_string_add_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let left = Value::from_bits(left_bits);
    let right = Value::from_bits(right_bits);
    match vm.add_string_or_object(left, right) {
        Ok(v) => v.bits(),
        Err(e) => {
            // Stash the error so the dispatcher's post-JIT
            // `take_jit_error()` check converts it back to a real
            // `Result::Err`. Without this, a thrown JS error
            // would silently look like a successful `undefined`.
            vm.jit_error.get_or_insert(e);
            Value::UNDEFINED.bits()
        },
    }
}

/// Optimized Add helper for non-i32 cases: uses StringRope for string concat.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_add_generic_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let left = Value::from_bits(left_bits);
    let right = Value::from_bits(right_bits);
    // Numeric fast path
    if left.is_number() && right.is_number() {
        return Value::from_f64(left.to_number() + right.to_number()).bits();
    }
    // String rope fast path (matches interpreter's add_string_or_object)
    match vm.add_string_or_object(left, right) {
        Ok(v) => v.bits(),
        Err(e) => {
            // Stash the error so the dispatcher's post-JIT
            // `take_jit_error()` check converts it back to a real
            // `Result::Err`. Without this, a thrown JS error
            // would silently look like a successful `undefined`.
            vm.jit_error.get_or_insert(e);
            Value::UNDEFINED.bits()
        },
    }
}

/// Ultra-fast Map path: both intern cache AND Map pointer cache hit.
/// Tiny function body → fits in L1 i-cache for tight loops.
#[cfg(feature = "djit")]
#[inline(never)]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_map_fast_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    args_ptr: *const u64,
    packed_info: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let nargs = (packed_info & 0xFF) as usize;
    // Use pre-resolved symbol from bits[63:32] if available (set by JIT fused path),
    // otherwise fall back to constants_syms_ptr lookup (old packed format)
    let prop_sym = if (packed_info >> 32) != 0 {
        (packed_info >> 32) as u32
    } else {
        *vm.constants_syms_ptr.add(((packed_info >> 8) & 0xFFFF) as usize)
    };
    let str_left = Value::from_bits(*args_ptr.add(0));
    let i32_right = Value::from_bits(*args_ptr.add(1));

    if !i32_right.is_i32() {
        return djit_map_strcat_method_helper(vm_raw, obj_bits, args_ptr, packed_info);
    }

    let i32_val = i32_right.as_i32_unchecked();
    let left_bits = str_left.bits();
    let idx = (((left_bits as usize) ^ ((left_bits as usize) >> 5)) ^ (i32_val as usize)) & 2047;
    let (cl, ci, cs) = *vm.intern_cache.get_unchecked(idx);

    if cl != left_bits || ci != i32_val || cs == u32::MAX
       || obj_bits != vm.cached_map_obj || vm.cached_map_entries.is_null()
    {
        return djit_map_strcat_method_helper(vm_raw, obj_bits, args_ptr, packed_info);
    }

    // Double cache hit: direct Map operation
    // Use specialized get_sym for Sym keys (avoids PartialEq dispatch)
    let entries = &mut *(vm.cached_map_entries as *mut Vec<(crate::object::HashKey, Value)>);
    let indices = &mut *(vm.cached_map_indices as *mut crate::object::FlatHashTable);

    if prop_sym == vm.sym_get {
        return match indices.get_sym(entries, cs) {
            Some(i) => unsafe { entries.get_unchecked(i).1.bits() },
            None => Value::UNDEFINED.bits(),
        };
    }
    if prop_sym == vm.sym_set && nargs >= 3 {
        let value = Value::from_bits(*args_ptr.add(2));
        // Fast check-then-insert for Sym keys
        if let Some(i) = indices.get_sym(entries, cs) {
            unsafe { entries.get_unchecked_mut(i).1 = value; }
        } else {
            let idx = entries.len();
            entries.push((crate::object::HashKey::Sym(cs), value));
            indices.insert(entries, &crate::object::HashKey::Sym(cs), idx);
        }
        return Value::from_bits(obj_bits).bits();
    }
    Value::UNDEFINED.bits()
}

/// Ultra-minimal Map SET helper: caller already resolved sym_id via inline cache.
/// rcx=vm, rdx=sym_id, r8=value_bits, r9=unused
#[cfg(feature = "djit")]
#[inline(never)]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_map_sym_set_helper(
    vm_raw: *mut u8,
    sym_id: u64,
    value_bits: u64,
    _unused: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let cs = sym_id as u32;
    let entries = &mut *(vm.cached_map_entries as *mut Vec<(crate::object::HashKey, Value)>);
    let indices = &mut *(vm.cached_map_indices as *mut crate::object::FlatHashTable);
    let value = Value::from_bits(value_bits);
    let idx = entries.len();
    if let Some(i) = indices.get_or_insert_sym(entries, cs, idx) {
        entries.get_unchecked_mut(i).1 = value;
    } else {
        entries.push((crate::object::HashKey::Sym(cs), value));
    }
    // Refresh direct pointers only when reallocation actually occurred
    let new_eptr = entries.as_ptr() as *const u8;
    let new_bptr = indices.buckets.as_ptr();
    if new_eptr != vm.cached_map_entries_data || new_bptr != vm.cached_map_buckets_data {
        vm.cached_map_buckets_data = new_bptr;
        vm.cached_map_entries_data = new_eptr;
        vm.cached_map_mask = indices.mask;
    }
    vm.cached_map_obj
}

/// Fused Map.set with string+i32 key (full path with buffer+intern).
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_map_strcat_method_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    args_ptr: *const u64,   // [str_left, i32_right, ...extra_args]
    packed_info: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let nargs = (packed_info & 0xFF) as usize;
    let prop_sym = if (packed_info >> 32) != 0 {
        (packed_info >> 32) as u32
    } else {
        *vm.constants_syms_ptr.add(((packed_info >> 8) & 0xFFFF) as usize)
    };
    let obj_val = Value::from_bits(obj_bits);
    let str_left = Value::from_bits(*args_ptr.add(0));
    let i32_right = Value::from_bits(*args_ptr.add(1));

    // ── Ultra-fast cache check FIRST: skip buffer+intern entirely on hit ──
    if i32_right.is_i32() {
        let i32_val = i32_right.as_i32_unchecked();
        let left_bits = str_left.bits();
        const CACHE_SIZE: usize = 2048;
        // intern_cache is pre-allocated at VM construction
        let idx = (((left_bits as usize) ^ ((left_bits as usize) >> 5)) ^ (i32_val as usize)) & (CACHE_SIZE - 1);
        let (cl, ci, cs) = vm.intern_cache[idx];
        if cl == left_bits && ci == i32_val && cs != u32::MAX {
            // Cache HIT: skip buffer construction + intern entirely!
            let key = crate::object::HashKey::Sym(cs);

            // Ultra-fast Map access: use cached Map pointers if same object
            if obj_bits == vm.cached_map_obj && !vm.cached_map_entries.is_null() {
                let entries = &mut *(vm.cached_map_entries as *mut Vec<(crate::object::HashKey, Value)>);
                let indices = &mut *(vm.cached_map_indices as *mut crate::object::FlatHashTable);
                // Use get_sym fast path since cs is always a Sym key
                if prop_sym == vm.sym_get {
                    return match indices.get_sym(entries, cs) {
                        Some(i) => entries.get_unchecked(i).1.bits(),
                        None => Value::UNDEFINED.bits(),
                    };
                }
                if prop_sym == vm.sym_set && nargs >= 3 {
                    let value = Value::from_bits(*args_ptr.add(2));
                    if let Some(i) = indices.get_sym(entries, cs) {
                        entries.get_unchecked_mut(i).1 = value;
                    } else {
                        let idx = entries.len();
                        entries.push((key, value));
                        indices.insert(entries, &crate::object::HashKey::Sym(cs), idx);
                    }
                    // Refresh direct pointers after potential reallocation
                    vm.cached_map_buckets_data = indices.buckets.as_ptr();
                    vm.cached_map_entries_data = entries.as_ptr() as *const u8;
                    vm.cached_map_mask = indices.mask;
                    return obj_val.bits();
                }
                return Value::UNDEFINED.bits();
            }

            // Cold path: lookup Map + cache pointers
            if obj_val.is_heap() {
                let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
                if let Object::Map(map_obj) = heap_obj {
                    vm.cached_map_obj = obj_bits;
                    vm.cached_map_entries = map_obj.entries.borrow_mut() as *mut Vec<_> as *mut u8;
                    vm.cached_map_indices = map_obj.indices.borrow_mut() as *mut crate::object::FlatHashTable as *mut u8;
                    let entries = &mut *(vm.cached_map_entries as *mut Vec<(crate::object::HashKey, Value)>);
                    let indices = &mut *(vm.cached_map_indices as *mut crate::object::FlatHashTable);
                    // Cache direct data pointers (skip 2 dereferences per op)
                    vm.cached_map_buckets_data = indices.buckets.as_ptr();
                    vm.cached_map_entries_data = entries.as_ptr() as *const u8;
                    vm.cached_map_mask = indices.mask;
                    if prop_sym == vm.sym_get {
                        return match indices.get_sym(entries, cs) {
                            Some(i) => unsafe { entries.get_unchecked(i).1.bits() },
                            None => Value::UNDEFINED.bits(),
                        };
                    }
                    if prop_sym == vm.sym_set && nargs >= 3 {
                        let value = Value::from_bits(*args_ptr.add(2));
                        if let Some(i) = indices.get_sym(entries, cs) {
                            unsafe { entries.get_unchecked_mut(i).1 = value; }
                        } else {
                            let idx = entries.len();
                            entries.push((key, value));
                            indices.insert(entries, &crate::object::HashKey::Sym(cs), idx);
                        }
                        vm.cached_map_buckets_data = indices.buckets.as_ptr();
                        vm.cached_map_entries_data = entries.as_ptr() as *const u8;
                        vm.cached_map_mask = indices.mask;
                        return obj_val.bits();
                    }
                }
            }
            return Value::UNDEFINED.bits();
        }
    }

    // ── Cache miss: build buffer + intern ──
    let mut buf = [0u8; 32];
    let mut pos = 0usize;

    if str_left.is_inline_str() {
        let (b, len) = str_left.inline_str_buf();
        buf[pos..pos + len].copy_from_slice(&b[..len]);
        pos += len;
    } else if str_left.is_heap() {
        let obj = &*vm.heap.objects.as_ptr().add(str_left.heap_index() as usize);
        if let Object::String(s) = obj {
            let bytes = s.as_bytes();
            let n = bytes.len().min(buf.len() - pos);
            buf[pos..pos + n].copy_from_slice(&bytes[..n]);
            pos += n;
        }
    }

    if i32_right.is_i32() {
        let mut ibuf = itoa::Buffer::new();
        let s = ibuf.format(i32_right.as_i32_unchecked());
        let bytes = s.as_bytes();
        let n = bytes.len().min(buf.len() - pos);
        buf[pos..pos + n].copy_from_slice(&bytes[..n]);
        pos += n;
    } else {
        let concat_val = match vm.add_string_or_object(str_left, i32_right) {
            Ok(v) => v,
            Err(_) => return Value::UNDEFINED.bits(),
        };
        let key = vm.hash_key_from_value(concat_val);
        if obj_val.is_heap() {
            let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
            if let Object::Map(map_obj) = heap_obj {
                if prop_sym == vm.sym_set && nargs >= 3 {
                    let value = Value::from_bits(*args_ptr.add(2));
                    VM::map_insert_or_replace(
                        map_obj.entries.borrow_mut(), map_obj.indices.borrow_mut(), key, value);
                    return obj_val.bits();
                }
                if prop_sym == vm.sym_get {
                    return VM::map_get(map_obj.entries.borrow(), map_obj.indices.borrow(), &key)
                        .unwrap_or(Value::UNDEFINED).bits();
                }
            }
        }
        return Value::UNDEFINED.bits();
    }

    let i32_val = i32_right.as_i32_unchecked();
    let left_bits = str_left.bits();
    let sym = {
        const CACHE_SIZE: usize = 2048;
        // Cache already initialized above
        let idx = (((left_bits as usize) ^ ((left_bits as usize) >> 5)) ^ (i32_val as usize)) & (CACHE_SIZE - 1);
        let (cl, ci, cs) = vm.intern_cache[idx];
        if cl == left_bits && ci == i32_val && cs != u32::MAX {
            cs // Cache hit!
        } else {
            let key_str = std::str::from_utf8_unchecked(&buf[..pos]);
            let s = crate::intern::intern(key_str);
            vm.intern_cache[idx] = (left_bits, i32_val, s);
            s
        }
    };
    let key = crate::object::HashKey::Sym(sym);

    if obj_val.is_heap() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Map(map_obj) = heap_obj {
            if prop_sym == vm.sym_set && nargs >= 3 {
                let value = Value::from_bits(*args_ptr.add(2));
                VM::map_insert_or_replace(
                    map_obj.entries.borrow_mut(), map_obj.indices.borrow_mut(), key, value);
                return obj_val.bits();
            }
            if prop_sym == vm.sym_get {
                return VM::map_get(map_obj.entries.borrow(), map_obj.indices.borrow(), &key)
                    .unwrap_or(Value::UNDEFINED).bits();
            }
        }
    }
    Value::UNDEFINED.bits()
}

/// Create an array from NaN-boxed values. Returns NaN-boxed heap pointer.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_array_new_helper(
    vm_raw: *mut u8,
    items_ptr: *const u64,
    count: u64,
    _unused: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let count = count as usize;
    let mut items: Vec<Value> = Vec::with_capacity(count);
    for i in 0..count {
        items.push(Value::from_bits(*items_ptr.add(i)));
    }
    let arr = crate::object::make_array(items);
    obj_into_val(arr, &mut vm.heap).bits()
}

/// Set arr[key] = val. All params are NaN-boxed u64.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_set_index_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    key_bits: u64,
    val_bits: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let obj_val = Value::from_bits(obj_bits);
    let key_val = Value::from_bits(key_bits);
    let val_v = Value::from_bits(val_bits);

    if obj_val.is_heap() && key_val.is_i32() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Array(arr_rc) = heap_obj {
            let idx = key_val.as_i32_unchecked();
            if idx >= 0 {
                let i = idx as usize;
                let arr = arr_rc.borrow_mut();
                if i < arr.len() {
                    *arr.get_unchecked_mut(i) = val_v;
                } else if i <= MAX_ARRAY_SIZE {
                    if i > arr.len() + SPARSE_ARRAY_THRESHOLD {
                        // Sparse array index too far beyond current length
                        return Value::UNDEFINED.bits();
                    }
                    arr.resize(i + 1, Value::UNDEFINED);
                    *arr.get_unchecked_mut(i) = val_v;
                }
                return Value::UNDEFINED.bits();
            }
        }
    }
    // Slow path
    let obj = val_to_obj(obj_val, &vm.heap);
    let key = val_to_obj(key_val, &vm.heap);
    let val_obj = val_to_obj(val_v, &vm.heap);
    let _ = vm.execute_set_index(obj, key, val_obj);
    Value::UNDEFINED.bits()
}

/// Get arr[key]. Returns NaN-boxed result.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_get_index_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    key_bits: u64,
    _unused: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let obj_val = Value::from_bits(obj_bits);
    let key_val = Value::from_bits(key_bits);

    if obj_val.is_heap() && key_val.is_i32() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Array(arr_rc) = heap_obj {
            let idx = key_val.as_i32_unchecked();
            if idx >= 0 {
                let arr = arr_rc.borrow();
                let i = idx as usize;
                if i < arr.len() {
                    return (*arr.get_unchecked(i)).bits();
                }
                return Value::UNDEFINED.bits();
            }
        }
    }
    // Slow path
    let obj = val_to_obj(obj_val, &vm.heap);
    let key = val_to_obj(key_val, &vm.heap);
    match vm.execute_index_expression(obj, key) {
        Ok(()) => vm.pop_val().unwrap_or(Value::UNDEFINED).bits(),
        Err(e) => {
            // Stash the error so the dispatcher's post-JIT
            // `take_jit_error()` check converts it back to a real
            // `Result::Err`. Without this, a thrown JS error
            // would silently look like a successful `undefined`.
            vm.jit_error.get_or_insert(e);
            Value::UNDEFINED.bits()
        },
    }
}

/// Create a hash (object) from key-value pairs. Keys are constant indices (strings).
/// items_ptr points to NaN-boxed values: [val0, val1, val2, ...]
/// keys are looked up from constants via consts_ptr.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_hash_new_helper(
    vm_raw: *mut u8,
    items_ptr: *const u64,  // pointer to register values
    info_packed: u64,        // packed: count in low 32, const_base in high 32
    _consts_ptr: *const u64, // constants array (unused, kept for ABI compat)
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let count = (info_packed & 0xFFFFFFFF) as usize;
    let base = ((info_packed >> 32) & 0xFFFFFFFF) as usize;
    let num_pairs = count / 2;

    let mut hash = crate::object::HashObject::default();
    for i in 0..num_pairs {
        let key_val = Value::from_bits(*items_ptr.add(base + i * 2));
        let val_val = Value::from_bits(*items_ptr.add(base + i * 2 + 1));
        let key_obj = val_to_obj(key_val, &vm.heap);
        if let Object::String(s) = key_obj {
            let sym = crate::intern::intern(&s);
            hash.set_by_sym(sym, val_val);
        }
    }

    let obj = Object::Hash(std::rc::Rc::new(crate::object::VmCell::new(hash)));
    obj_into_val(obj, &mut vm.heap).bits()
}

/// Get named property with hash pointer cache + inline cache.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_get_prop_sym_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    sym_and_cache: u64,
    _unused: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let sym = (sym_and_cache & 0xFFFFFFFF) as u32;
    let cache_slot = (sym_and_cache >> 32) as usize;

    // Fast path: cached hash pointer
    if obj_bits == vm.cached_hash_obj && !vm.cached_hash_borrow.is_null() {
        let hash = &*(vm.cached_hash_borrow as *const crate::object::HashObject);
        if cache_slot < vm.inline_cache.len() {
            let (cs, co) = *vm.inline_cache.get_unchecked(cache_slot);
            if cs == hash.shape_version {
                return (*hash.values.get_unchecked(co as usize)).bits();
            }
        }
        if let Some(&slot) = hash.str_slots.get(&sym) {
            if cache_slot < vm.inline_cache.len() {
                *vm.inline_cache.get_unchecked_mut(cache_slot) = (hash.shape_version, slot as u32);
            }
            return (*hash.values.get_unchecked(slot)).bits();
        }
        return Value::UNDEFINED.bits();
    }

    // Cold path: full heap lookup
    let obj_val = Value::from_bits(obj_bits);
    if obj_val.is_heap() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Hash(hash_rc) = heap_obj {
            vm.cached_hash_obj = obj_bits;
            vm.cached_hash_borrow = hash_rc.borrow_mut() as *mut crate::object::HashObject as *mut u8;
            let hash = &*(vm.cached_hash_borrow as *const crate::object::HashObject);
            vm.cached_values_ptr = hash.values.as_ptr() as *mut u64;
            vm.cached_shape = hash.shape_version;
            if cache_slot < vm.inline_cache.len() {
                let (cs, co) = *vm.inline_cache.get_unchecked(cache_slot);
                if cs == hash.shape_version {
                    return (*hash.values.get_unchecked(co as usize)).bits();
                }
            }
            if let Some(&slot) = hash.str_slots.get(&sym) {
                if cache_slot < vm.inline_cache.len() {
                    *vm.inline_cache.get_unchecked_mut(cache_slot) = (hash.shape_version, slot as u32);
                }
                return (*hash.values.get_unchecked(slot)).bits();
            }
        }
    }
    Value::UNDEFINED.bits()
}

/// Set named property with inline cache.
/// sym_and_cache: sym in low 32, cache_slot in high 32
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_set_prop_sym_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    sym_and_cache: u64,
    val_bits: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let obj_val = Value::from_bits(obj_bits);
    let sym = (sym_and_cache & 0xFFFFFFFF) as u32;
    let cache_slot = (sym_and_cache >> 32) as usize;
    let val_v = Value::from_bits(val_bits);

    if obj_val.is_heap() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Hash(hash_rc) = heap_obj {
            let hash = hash_rc.borrow_mut();
            // Inline cache fast path
            let slot = if cache_slot < vm.inline_cache.len() {
                let (cached_shape, cached_offset) = *vm.inline_cache.get_unchecked(cache_slot);
                if cached_shape == hash.shape_version {
                    cached_offset as usize
                } else {
                    match hash.str_slots.get(&sym) {
                        Some(&s) => {
                            *vm.inline_cache.get_unchecked_mut(cache_slot) = (hash.shape_version, s as u32);
                            s
                        }
                        None => {
                            hash.set_by_sym(sym, val_v);
                            // Update cache AFTER adding new property (shape/values changed)
                            vm.cached_hash_obj = obj_bits;
                            vm.cached_hash_borrow = hash as *mut _ as *mut u8;
                            vm.cached_values_ptr = hash.values.as_mut_ptr() as *mut u64;
                            vm.cached_shape = hash.shape_version;
                            return Value::UNDEFINED.bits();
                        }
                    }
                }
            } else {
                match hash.str_slots.get(&sym) {
                    Some(&s) => s,
                    None => {
                        hash.set_by_sym(sym, val_v);
                        vm.cached_hash_obj = obj_bits;
                        vm.cached_hash_borrow = hash as *mut _ as *mut u8;
                        vm.cached_values_ptr = hash.values.as_mut_ptr() as *mut u64;
                        vm.cached_shape = hash.shape_version;
                        return Value::UNDEFINED.bits();
                    }
                }
            };
            *hash.values.get_unchecked_mut(slot) = val_v;
            hash.pairs_dirty = true;
            // Update JIT cached pointers
            vm.cached_hash_obj = obj_bits;
            vm.cached_hash_borrow = hash as *mut _ as *mut u8;
            vm.cached_values_ptr = hash.values.as_mut_ptr() as *mut u64;
            vm.cached_shape = hash.shape_version;
        }
    }
    Value::UNDEFINED.bits()
}

/// Get named property: obj.prop (fallback using NaN-boxed string constant).
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_get_prop_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    prop_const_bits: u64,  // NaN-boxed string constant
    _unused: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let obj_val = Value::from_bits(obj_bits);
    let prop_val = Value::from_bits(prop_const_bits);

    // Fast path: hash object with string key
    if obj_val.is_heap() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Hash(hash_rc) = heap_obj {
            if prop_val.is_heap() {
                let prop_obj = &*vm.heap.objects.as_ptr().add(prop_val.heap_index() as usize);
                if let Object::String(s) = prop_obj {
                    let sym = crate::intern::intern(s);
                    return hash_rc.borrow().get_by_sym(sym).unwrap_or(Value::UNDEFINED).bits();
                }
            } else if prop_val.is_inline_str() {
                let (buf, len) = prop_val.inline_str_buf();
                let s = std::str::from_utf8_unchecked(&buf[..len]);
                let sym = crate::intern::intern(s);
                return hash_rc.borrow().get_by_sym(sym).unwrap_or(Value::UNDEFINED).bits();
            }
        }
    }
    // Slow path
    let obj = val_to_obj(obj_val, &vm.heap);
    let key = val_to_obj(prop_val, &vm.heap);
    match vm.execute_index_expression(obj, key) {
        Ok(()) => vm.pop_val().unwrap_or(Value::UNDEFINED).bits(),
        Err(e) => {
            // Stash the error so the dispatcher's post-JIT
            // `take_jit_error()` check converts it back to a real
            // `Result::Err`. Without this, a thrown JS error
            // would silently look like a successful `undefined`.
            vm.jit_error.get_or_insert(e);
            Value::UNDEFINED.bits()
        },
    }
}

/// Set named property: obj.prop = val.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_set_prop_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    prop_const_bits: u64,
    val_bits: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let obj_val = Value::from_bits(obj_bits);
    let prop_val = Value::from_bits(prop_const_bits);
    let val_v = Value::from_bits(val_bits);

    if obj_val.is_heap() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Hash(hash_rc) = heap_obj {
            if prop_val.is_heap() {
                let prop_obj = &*vm.heap.objects.as_ptr().add(prop_val.heap_index() as usize);
                if let Object::String(s) = prop_obj {
                    let sym = crate::intern::intern(s);
                    hash_rc.borrow_mut().set_by_sym(sym, val_v);
                    return Value::UNDEFINED.bits();
                }
            } else if prop_val.is_inline_str() {
                let (buf, len) = prop_val.inline_str_buf();
                let s = std::str::from_utf8_unchecked(&buf[..len]);
                let sym = crate::intern::intern(s);
                hash_rc.borrow_mut().set_by_sym(sym, val_v);
                return Value::UNDEFINED.bits();
            }
        }
    }
    // Slow path
    let obj = val_to_obj(obj_val, &vm.heap);
    let key = val_to_obj(prop_val, &vm.heap);
    let val_obj = val_to_obj(val_v, &vm.heap);
    let _ = vm.execute_set_index(obj, key, val_obj);
    Value::UNDEFINED.bits()
}

/// Fused: obj.prop += const_val with inline cache + hash pointer cache.
/// sym_and_cache: sym in low 32 bits, cache_slot in high 32 bits
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_add_const_to_prop_helper(
    vm_raw: *mut u8,
    obj_bits: u64,
    sym_and_cache: u64,
    add_val: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut VM);
    let add_cv = Value::from_bits(add_val);
    let sym = (sym_and_cache & 0xFFFFFFFF) as u32;
    let cache_slot = (sym_and_cache >> 32) as usize;

    // Fast path: cached hash pointer (skip heap lookup + enum match)
    if obj_bits == vm.cached_hash_obj && !vm.cached_hash_borrow.is_null() {
        let hash = &mut *(vm.cached_hash_borrow as *mut crate::object::HashObject);
        let slot = if cache_slot < vm.inline_cache.len() {
            let (cs, co) = *vm.inline_cache.get_unchecked(cache_slot);
            if cs == hash.shape_version { co as usize }
            else {
                match hash.str_slots.get(&sym) {
                    Some(&s) => { *vm.inline_cache.get_unchecked_mut(cache_slot) = (hash.shape_version, s as u32); s }
                    None => return Value::UNDEFINED.bits(),
                }
            }
        } else {
            match hash.str_slots.get(&sym) { Some(&s) => s, None => return Value::UNDEFINED.bits() }
        };
        let prop_v = *hash.values.get_unchecked(slot);
        let result = if Value::both_i32(prop_v, add_cv) {
            let a = prop_v.as_i32_unchecked();
            let b = add_cv.as_i32_unchecked();
            match a.checked_add(b) {
                Some(sum) => Value::from_i32(sum),
                None => Value::from_f64(a as f64 + b as f64),
            }
        } else {
            Value::from_f64(prop_v.to_number() + add_cv.to_number())
        };
        *hash.values.get_unchecked_mut(slot) = result;
        hash.pairs_dirty = true;
        return Value::UNDEFINED.bits();
    }

    // Cold path: full lookup + cache update
    let obj_val = Value::from_bits(obj_bits);
    if obj_val.is_heap() {
        let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
        if let Object::Hash(hash_rc) = heap_obj {
            // Cache the hash pointer + values.ptr for future fast-path hits
            vm.cached_hash_obj = obj_bits;
            vm.cached_hash_borrow = hash_rc.borrow_mut() as *mut crate::object::HashObject as *mut u8;
            let hash = &mut *(vm.cached_hash_borrow as *mut crate::object::HashObject);
            vm.cached_values_ptr = hash.values.as_mut_ptr() as *mut u64;
            vm.cached_shape = hash.shape_version;
            // Inline cache fast path
            let slot = if cache_slot < vm.inline_cache.len() {
                let (cached_shape, cached_offset) = *vm.inline_cache.get_unchecked(cache_slot);
                if cached_shape == hash.shape_version {
                    cached_offset as usize
                } else {
                    // Cache miss: lookup and update
                    match hash.str_slots.get(&sym) {
                        Some(&s) => {
                            *vm.inline_cache.get_unchecked_mut(cache_slot) = (hash.shape_version, s as u32);
                            s
                        }
                        None => return Value::UNDEFINED.bits(),
                    }
                }
            } else {
                match hash.str_slots.get(&sym) {
                    Some(&s) => s,
                    None => return Value::UNDEFINED.bits(),
                }
            };
            let prop_v = *hash.values.get_unchecked(slot);
            let result = if Value::both_i32(prop_v, add_cv) {
                let a = prop_v.as_i32_unchecked();
                let b = add_cv.as_i32_unchecked();
                match a.checked_add(b) {
                    Some(sum) => Value::from_i32(sum),
                    None => Value::from_f64(a as f64 + b as f64),
                }
            } else {
                Value::from_f64(prop_v.to_number() + add_cv.to_number())
            };
            *hash.values.get_unchecked_mut(slot) = result;
            hash.pairs_dirty = true;
        }
    }
    Value::UNDEFINED.bits()
}

/// Fused: obj.dst = obj.s1 + obj.s2 using pre-interned symbols.
/// packed_syms: s1_sym in bits 0-15, s2_sym in bits 16-31, dst_sym in bits 32-47
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with a valid VM pointer and NaN-boxed arguments.
pub unsafe extern "win64" fn djit_add_props_to_prop_helper(
    _vm_raw: *mut u8,
    obj_bits: u64,
    _unused: *const u64,
    packed_syms: u64,
) -> u64 {
    let obj_val = Value::from_bits(obj_bits);
    let s1_sym = (packed_syms & 0xFFFF) as u32;
    let s2_sym = ((packed_syms >> 16) & 0xFFFF) as u32;
    let dst_sym = ((packed_syms >> 32) & 0xFFFF) as u32;

    if !obj_val.is_heap() { return Value::UNDEFINED.bits(); }
    let vm = &mut *(_vm_raw as *mut VM);
    let heap_obj = &*vm.heap.objects.as_ptr().add(obj_val.heap_index() as usize);
    let hash_rc = match heap_obj {
        Object::Hash(h) => h,
        _ => return Value::UNDEFINED.bits(),
    };

    let hash = hash_rc.borrow_mut();
    let v1 = hash.get_by_sym(s1_sym).unwrap_or(Value::UNDEFINED);
    let v2 = hash.get_by_sym(s2_sym).unwrap_or(Value::UNDEFINED);

    let result = if Value::both_i32(v1, v2) {
        let a = v1.as_i32_unchecked();
        let b = v2.as_i32_unchecked();
        match a.checked_add(b) {
            Some(sum) => Value::from_i32(sum),
            None => Value::from_f64(a as f64 + b as f64),
        }
    } else {
        Value::from_f64(v1.to_number() + v2.to_number())
    };

    if let Some(&slot) = hash.str_slots.get(&dst_sym) {
        *hash.values.get_unchecked_mut(slot) = result;
        hash.pairs_dirty = true;
    } else {
        hash.set_by_sym(dst_sym, result);
    }
    Value::UNDEFINED.bits()
}

/// Strict equality helper for JIT: handles string/object comparison.
/// Returns NaN-boxed TRUE or FALSE bits.
#[cfg(feature = "djit")]
/// # Safety
/// Called from JIT-generated code with NaN-boxed arguments.
pub unsafe extern "win64" fn djit_strict_eq_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let t = Value::TRUE.bits();
    let f = Value::FALSE.bits();
    // Fast path: bits equal (already checked in JIT, but handle NaN)
    if left_bits == right_bits {
        let lv = Value::from_bits(left_bits);
        if lv.is_f64() && lv.as_f64().is_nan() { return f; }
        return t;
    }
    let lv = Value::from_bits(left_bits);
    let rv = Value::from_bits(right_bits);
    if Value::both_i32(lv, rv) { return f; }
    if lv.is_number() && rv.is_number() {
        return if lv.to_number() == rv.to_number() { t } else { f };
    }
    let vm = &mut *(vm_raw as *mut VM);
    let result = vm.strict_equality_slow(lv, rv);
    if result { t } else { f }
}

impl VM {
    /// True if `byte` is one of the register VM's "this ends the block"
    /// terminators. Used by [`Self::ensure_terminated_instructions`] to
    /// decide whether a synthesised `ROp::Halt` needs to be appended so
    /// the dispatch loop can't run off the end of the bytecode.
    #[inline(always)]
    fn is_terminator_byte(byte: u8) -> bool {
        byte == crate::rcode::ROp::Return as u8
            || byte == crate::rcode::ROp::ReturnUndef as u8
            || byte == crate::rcode::ROp::Halt as u8
            || byte == crate::rcode::ROp::HaltValue as u8
    }

    pub fn ensure_terminated_instructions_pub(instructions: Vec<u8>) -> Vec<u8> {
        Self::ensure_terminated_instructions(instructions)
    }

    fn ensure_terminated_instructions(mut instructions: Vec<u8>) -> Vec<u8> {
        if instructions
            .last()
            .copied()
            .is_none_or(|b| !Self::is_terminator_byte(b))
        {
            instructions.push(crate::rcode::ROp::Halt as u8);
        }
        instructions
    }

    fn ensure_terminated_instructions_rc(instructions: Rc<Vec<u8>>) -> Rc<Vec<u8>> {
        if instructions
            .last()
            .copied()
            .is_some_and(Self::is_terminator_byte)
        {
            return instructions;
        }

        let mut owned = (*instructions).clone();
        owned.push(crate::rcode::ROp::Halt as u8);
        Rc::new(owned)
    }

    #[inline(always)]
    pub(crate) fn clone_object_fast(value: &Object) -> Object {
        match value {
            Object::Integer(v) => Object::Integer(*v),
            Object::Float(v) => Object::Float(*v),
            Object::Boolean(v) => Object::Boolean(*v),
            Object::Null => Object::Null,
            Object::Undefined => Object::Undefined,
            other => other.clone(),
        }
    }

    /// Create a VM from pre-shared (Rc-wrapped) bytecode data.
    /// Avoids deep-cloning instructions/constants on every creation.
    pub fn new_shared(
        instructions: Rc<Vec<u8>>,
        constants: Rc<Vec<Object>>,
        num_cache_slots: u16,
        max_stack_depth: u16,
        register_count: u16,
        config: ZippConfig,
    ) -> Self {
        let enforce_limits = config.requires_limit_checks();
        let inst_ptr = instructions.as_ptr();
        let inst_len = instructions.len();
        Self::new_inner(instructions, Rc::clone(&constants), inst_ptr, inst_len, num_cache_slots, max_stack_depth as usize, register_count, config, enforce_limits)
    }

    /// Construct a fresh VM from a validated bytecode.
    ///
    /// The [`crate::backend::validate::ValidatedBytecode`] argument is
    /// the type-system gate that prevents an embedder from running
    /// unchecked bytecode: the only way to mint one outside the crate
    /// is via `ValidatedBytecode::new`, which calls the validator
    /// first. Internal callers that work directly with
    /// `Rc<Vec<u8>> + Rc<Vec<Object>>` should use the `pub(crate)`
    /// helpers (e.g. `new_from_rc`) and have validated upstream.
    pub fn new(
        bytecode: crate::backend::validate::ValidatedBytecode,
        config: ZippConfig,
    ) -> Self {
        let bytecode = bytecode.into_inner();
        let enforce_limits = config.requires_limit_checks();
        let num_cache_slots = bytecode.num_cache_slots;
        let max_stack_depth = bytecode.max_stack_depth as usize;
        let register_count = bytecode.register_count;
        let instructions = Rc::new(Self::ensure_terminated_instructions(bytecode.instructions));
        let inst_ptr = instructions.as_ptr();
        let inst_len = instructions.len();
        Self::new_inner(instructions, Rc::new(bytecode.constants), inst_ptr, inst_len, num_cache_slots, max_stack_depth, register_count, config, enforce_limits)
    }

    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        instructions: Rc<Vec<u8>>,
        constants: Rc<Vec<Object>>,
        inst_ptr: *const u8,
        inst_len: usize,
        num_cache_slots: u16,
        max_stack_depth: usize,
        register_count: u16,
        config: ZippConfig,
        enforce_limits: bool,
    ) -> Self {
        crate::intern::ensure_init();
        let stack = Vec::with_capacity(STACK_SIZE);
        Self {
            constants,
            instructions,
            stack, jit_regs_ptr: std::ptr::null_mut(),
            sp: 0,
            ip: 0,
            inst_ptr,
            inst_len,
            globals: SharedGlobals::new(),
            locals: vec![],
            config,
            enforce_limits,
            quota: ExecutionQuota::default(),
            last_popped: None,
            arg_buffer: Vec::with_capacity(8),
            string_concat_buf: String::new(),
            locals_pool: Vec::with_capacity(8),
            inline_cache: vec![(0, 0); num_cache_slots as usize],
            max_stack_depth,
            frames: Vec::with_capacity(16),
            rframes: Vec::with_capacity(32),
            heap: Heap::new(),
            register_count,
            last_call_nargs: 0,
            constants_values_ptr: std::ptr::null(),
            constants_values_buf: Vec::new(),
            constants_values_cache: Vec::new(),
            constants_raw: std::ptr::null(),
            constants_syms_buf: Vec::new(),
            constants_syms_ptr: std::ptr::null(),
            constants_syms_cache: Vec::new(),
            typeof_undefined: Value::UNDEFINED,
            typeof_number: Value::UNDEFINED,
            typeof_string: Value::UNDEFINED,
            typeof_boolean: Value::UNDEFINED,
            typeof_function: Value::UNDEFINED,
            typeof_object: Value::UNDEFINED,
            typeof_symbol: Value::UNDEFINED,
            sym_push: crate::intern::intern("push"),
            sym_pop: crate::intern::intern("pop"),
            sym_length: crate::intern::intern("length"),
            sym_set: crate::intern::intern("set"),
            sym_get: crate::intern::intern("get"),
            sym_has: crate::intern::intern("has"),
            sym_size: crate::intern::intern("size"),
            sym_shift: crate::intern::intern("shift"),
            sym_unshift: crate::intern::intern("unshift"),
            sym_splice: crate::intern::intern("splice"),
            sym_has_own_property: crate::intern::intern("hasOwnProperty"),
            sym_then: crate::intern::intern("then"),
            sym_catch: crate::intern::intern("catch"),
            last_preconvert_key: 0,
            last_preconvert_values_ptr: std::ptr::null(),
            last_preconvert_syms_ptr: std::ptr::null(),
            new_target: Value::UNDEFINED,
            rng_state: rng_seed_now(),
            local_storage: None,
            db: None,
            draw: None,
            layout: None,
            input: None,
            http: None,
            fs: None,
            env: None,
            event_listeners: std::collections::HashMap::new(),
            pending_host_calls: Vec::new(),
            microtask_queue: std::collections::VecDeque::new(),
            host_callbacks: std::collections::HashMap::new(),
            next_host_call_id: 1,
            host_call_count: 0,
            trace_enabled: false,
            trace_steps: Vec::new(),
            trace_clk: 0,
            #[cfg(feature = "djit")]
            jit_error: None,
            #[cfg(feature = "djit")]
            djit: crate::djit::DynasmJit::new(),
            #[cfg(all(feature = "djit", target_arch = "x86_64"))]
            tier2: crate::codegen::tier2::Tier2Jit::new(),
            #[cfg(all(feature = "djit", target_arch = "x86_64"))]
            deopt_pending: false,
            #[cfg(feature = "djit")]
            last_call_callee_bits: 0,
            #[cfg(feature = "djit")]
            last_call_fn_ptr: None,
            #[cfg(feature = "djit")]
            last_call_has_calls: false,
            #[cfg(feature = "djit")]
            last_call_instr: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_instr_len: 0,
            #[cfg(feature = "djit")]
            last_call_consts_raw: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_reg_count: 0,
            #[cfg(feature = "djit")]
            last_call_takes_this: false,
            #[cfg(feature = "djit")]
            last_call_cache_slots: 0,
            #[cfg(feature = "djit")]
            last_call_cv_len: 0,
            #[cfg(feature = "djit")]
            last_call_cv_ptr: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_func_cache: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_consts_values_ptr: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_consts_syms_ptr: std::ptr::null(),
            fn_call_depth: 0,
            try_handlers: Vec::new(),
            heap_baseline: 0,
            #[cfg(feature = "djit")]
            cached_hash_obj: 0,
            #[cfg(feature = "djit")]
            cached_hash_borrow: std::ptr::null_mut(),
            #[cfg(feature = "djit")]
            cached_values_ptr: std::ptr::null_mut(),
            #[cfg(feature = "djit")]
            cached_shape: 0,
            #[cfg(feature = "djit")]
            intern_cache: Vec::new(), // lazy: allocated on first JIT use
            inline_sym_cache: Vec::new(), // lazy: allocated on first interpreter Map.set/get
            interned_str_value_cache: Vec::new(), // lazy: allocated on first canonical-str materialize
            #[cfg(feature = "djit")]
            cached_map_obj: 0,
            #[cfg(feature = "djit")]
            cached_map_entries: std::ptr::null_mut(),
            #[cfg(feature = "djit")]
            cached_map_indices: std::ptr::null_mut(),
            #[cfg(feature = "djit")]
            cached_map_buckets_data: std::ptr::null(),
            #[cfg(feature = "djit")]
            cached_map_entries_data: std::ptr::null(),
            #[cfg(feature = "djit")]
            cached_map_mask: 0,
        }
    }

    /// Internal-only VM constructor that accepts pre-`Rc`'d
    /// bytecode fields. Only the engine reaches for this — it has
    /// just called the validator on the source `Bytecode` and Rc-clones
    /// the instructions/constants for the per-process bytecode cache.
    /// Marked `pub(crate)` to prevent external callers from skipping
    /// validation by going directly through the `Rc` form.
    pub(crate) fn new_from_rc(
        instructions: Rc<Vec<u8>>,
        constants: Rc<Vec<Object>>,
        config: ZippConfig,
        initial_stack_capacity: usize,
        num_cache_slots: u16,
        max_stack_depth: u16,
    ) -> Self {
        Self::new_from_rc_with_globals(
            instructions,
            constants,
            config,
            initial_stack_capacity,
            SharedGlobals::new(),
            num_cache_slots,
            max_stack_depth,
        )
    }

    pub(crate) fn new_from_rc_with_globals(
        instructions: Rc<Vec<u8>>,
        constants: Rc<Vec<Object>>,
        config: ZippConfig,
        _initial_stack_capacity: usize,
        globals: SharedGlobals,
        num_cache_slots: u16,
        max_stack_depth: u16,
    ) -> Self {
        let enforce_limits = config.requires_limit_checks();
        let mut stack = Vec::with_capacity(STACK_SIZE);
        stack.reserve(STACK_SIZE);
        let instructions = Self::ensure_terminated_instructions_rc(instructions);
        let inst_ptr = instructions.as_ptr();
        let inst_len = instructions.len();
        Self {
            constants,
            instructions,
            stack, jit_regs_ptr: std::ptr::null_mut(),
            sp: 0,
            ip: 0,
            inst_ptr,
            inst_len,
            globals,
            locals: vec![],
            config,
            enforce_limits,
            quota: ExecutionQuota::default(),
            last_popped: None,
            arg_buffer: Vec::with_capacity(8),
            string_concat_buf: String::new(),
            locals_pool: Vec::with_capacity(8),
            inline_cache: vec![(0, 0); num_cache_slots as usize],
            max_stack_depth: max_stack_depth as usize,
            frames: Vec::with_capacity(16),
            rframes: Vec::with_capacity(32),
            heap: Heap::new(),
            register_count: 0,
            last_call_nargs: 0,
            constants_values_ptr: std::ptr::null(),
            constants_values_buf: Vec::new(),
            constants_values_cache: Vec::new(),
            constants_raw: std::ptr::null(),
            constants_syms_buf: Vec::new(),
            constants_syms_ptr: std::ptr::null(),
            constants_syms_cache: Vec::new(),
            typeof_undefined: Value::UNDEFINED,
            typeof_number: Value::UNDEFINED,
            typeof_string: Value::UNDEFINED,
            typeof_boolean: Value::UNDEFINED,
            typeof_function: Value::UNDEFINED,
            typeof_object: Value::UNDEFINED,
            typeof_symbol: Value::UNDEFINED,
            sym_push: crate::intern::intern("push"),
            sym_pop: crate::intern::intern("pop"),
            sym_length: crate::intern::intern("length"),
            sym_set: crate::intern::intern("set"),
            sym_get: crate::intern::intern("get"),
            sym_has: crate::intern::intern("has"),
            sym_size: crate::intern::intern("size"),
            sym_shift: crate::intern::intern("shift"),
            sym_unshift: crate::intern::intern("unshift"),
            sym_splice: crate::intern::intern("splice"),
            sym_has_own_property: crate::intern::intern("hasOwnProperty"),
            sym_then: crate::intern::intern("then"),
            sym_catch: crate::intern::intern("catch"),
            last_preconvert_key: 0,
            last_preconvert_values_ptr: std::ptr::null(),
            last_preconvert_syms_ptr: std::ptr::null(),
            new_target: Value::UNDEFINED,
            rng_state: rng_seed_now(),
            local_storage: None,
            db: None,
            draw: None,
            layout: None,
            input: None,
            http: None,
            fs: None,
            env: None,
            event_listeners: std::collections::HashMap::new(),
            pending_host_calls: Vec::new(),
            microtask_queue: std::collections::VecDeque::new(),
            host_callbacks: std::collections::HashMap::new(),
            next_host_call_id: 1,
            host_call_count: 0,
            trace_enabled: false,
            trace_steps: Vec::new(),
            trace_clk: 0,
            #[cfg(feature = "djit")]
            jit_error: None,
            #[cfg(feature = "djit")]
            djit: crate::djit::DynasmJit::new(),
            #[cfg(all(feature = "djit", target_arch = "x86_64"))]
            tier2: crate::codegen::tier2::Tier2Jit::new(),
            #[cfg(all(feature = "djit", target_arch = "x86_64"))]
            deopt_pending: false,
            #[cfg(feature = "djit")]
            last_call_callee_bits: 0,
            #[cfg(feature = "djit")]
            last_call_fn_ptr: None,
            #[cfg(feature = "djit")]
            last_call_has_calls: false,
            #[cfg(feature = "djit")]
            last_call_instr: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_instr_len: 0,
            #[cfg(feature = "djit")]
            last_call_consts_raw: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_reg_count: 0,
            #[cfg(feature = "djit")]
            last_call_takes_this: false,
            #[cfg(feature = "djit")]
            last_call_cache_slots: 0,
            #[cfg(feature = "djit")]
            last_call_cv_len: 0,
            #[cfg(feature = "djit")]
            last_call_cv_ptr: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_func_cache: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_consts_values_ptr: std::ptr::null(),
            #[cfg(feature = "djit")]
            last_call_consts_syms_ptr: std::ptr::null(),
            fn_call_depth: 0,
            try_handlers: Vec::new(),
            heap_baseline: 0,
            #[cfg(feature = "djit")]
            cached_hash_obj: 0,
            #[cfg(feature = "djit")]
            cached_hash_borrow: std::ptr::null_mut(),
            #[cfg(feature = "djit")]
            cached_values_ptr: std::ptr::null_mut(),
            #[cfg(feature = "djit")]
            cached_shape: 0,
            #[cfg(feature = "djit")]
            intern_cache: Vec::new(), // lazy: allocated on first JIT use
            inline_sym_cache: Vec::new(), // lazy: allocated on first interpreter Map.set/get
            interned_str_value_cache: Vec::new(), // lazy: allocated on first canonical-str materialize
            #[cfg(feature = "djit")]
            cached_map_obj: 0,
            #[cfg(feature = "djit")]
            cached_map_entries: std::ptr::null_mut(),
            #[cfg(feature = "djit")]
            cached_map_indices: std::ptr::null_mut(),
            #[cfg(feature = "djit")]
            cached_map_buckets_data: std::ptr::null(),
            #[cfg(feature = "djit")]
            cached_map_entries_data: std::ptr::null(),
            #[cfg(feature = "djit")]
            cached_map_mask: 0,
        }
    }

    /// Take any error a JIT helper stashed via the `jit_error`
    /// side-channel and clear the slot. The dispatcher calls this
    /// after every JIT execute_ptr return: `Some(err) => return
    /// Err(err)`, `None => continue`. See the field doc on
    /// [`Self::jit_error`] for why the channel exists.
    #[cfg(feature = "djit")]
    #[inline]
    pub(crate) fn take_jit_error(&mut self) -> Option<VMError> {
        self.jit_error.take()
    }

    /// Reset VM state for a new execution, reusing allocated buffers.
    /// Loads new bytecode and resets all execution state.
    /// Allocated buffers (stack, arg_buffer, locals_pool, frames) keep their
    /// capacity across calls. The Heap drops objects but keeps Vec capacity.
    pub fn reset_for_run(
        &mut self,
        instructions: Rc<Vec<u8>>,
        constants: Rc<Vec<Object>>,
        num_cache_slots: u16,
        max_stack_depth: u16,
        register_count: u16,
    ) {
        let instructions = Self::ensure_terminated_instructions_rc(instructions);
        // Detect whether the embedder is loading a fresh program or just
        // re-running the same bytecode buffer. The JIT cache is keyed by
        // raw bytecode-buffer pointer; reusing it across a *new* program
        // can dispatch to native code emitted for the previous one (the
        // freed Rc may have been replaced by a new allocation at the same
        // address). Re-running the *same* program is the safe, perf-
        // critical case (long-lived ScriptState event loops, the
        // `tier2_promotes_across_runs` test) — keep the cache then.
        let prev_inst_ptr = self.inst_ptr;
        let bytecode_changed = !std::ptr::eq(prev_inst_ptr, instructions.as_ptr());
        self.inst_ptr = instructions.as_ptr();
        self.inst_len = instructions.len();
        self.instructions = instructions;
        self.constants = constants;
        self.ip = 0;
        self.sp = 0;
        self.stack.clear();
        self.last_popped = None;
        self.locals.clear();
        self.arg_buffer.clear();
        // locals_pool: keep pooled vecs for reuse (don't clear)
        self.frames.clear();
        // Clear register-VM state left over from a prior run on a pooled VM.
        // Without this, a previous successful `try { throw ... } catch`
        // leaves no handler — but a previous partially-torn-down run could
        // leave stale entries that silently catch a freshly emitted
        // `throw`, making the next `eval("throw ...")` return the earlier
        // run's last value instead of the expected error.
        self.rframes.clear();
        self.try_handlers.clear();
        self.heap.reset();
        // Migrate local_objects in compiler-constructed hashes to VM heap
        for obj in self.constants.iter() {
            if let Object::Hash(hash_rc) = obj {
                unsafe { hash_rc.borrow_mut() }.migrate_local_objects(&mut self.heap);
            }
        }
        self.globals = SharedGlobals::new();
        self.quota = ExecutionQuota::default();
        self.inline_cache.clear();
        self.inline_cache.resize(num_cache_slots as usize, (0, 0));
        self.max_stack_depth = max_stack_depth as usize;
        self.register_count = register_count;
        self.constants_values_ptr = std::ptr::null();
        self.constants_values_buf.clear();
        self.constants_values_cache.clear();
        self.constants_raw = std::ptr::null();
        self.constants_syms_ptr = std::ptr::null();
        self.constants_syms_buf.clear();
        self.constants_syms_cache.clear();
        // Reset preconvert cache — old pointers are dangling after cache clear
        self.last_preconvert_key = 0;
        self.last_preconvert_values_ptr = std::ptr::null();
        self.last_preconvert_syms_ptr = std::ptr::null();
        // Reset typeof cache — heap was cleared, old Values are invalid
        self.typeof_undefined = Value::UNDEFINED;
        self.typeof_number = Value::UNDEFINED;
        self.typeof_string = Value::UNDEFINED;
        self.typeof_boolean = Value::UNDEFINED;
        self.typeof_function = Value::UNDEFINED;
        self.typeof_object = Value::UNDEFINED;
        // Drop host-side queues left over from a prior run on this pooled
        // VM. The heap was just cleared, so any heap-tagged Value sitting
        // in `event_listeners`, `microtask_queue`, or `host_callbacks`
        // now points into recycled storage and would either return the
        // wrong object or trip the heap bounds assert if invoked. The
        // counters are documented as per-execution; carrying them across
        // runs would let the second eval immediately trip a sync host
        // call rate-limit the first one half-filled.
        self.event_listeners.clear();
        self.pending_host_calls.clear();
        self.microtask_queue.clear();
        self.host_callbacks.clear();
        self.next_host_call_id = 0;
        self.host_call_count = 0;
        // JIT caches are keyed by the raw bytecode-buffer pointer of the
        // function being called. After a fresh program loads, the old
        // bytecode Rc is dropped and a future allocation can land at the
        // same address — so a stale cache entry would dispatch native
        // code emitted for the previous program. Clearing on bytecode
        // change keeps dispatch correct; we keep the cache when the
        // embedder is re-running the same bytecode (long-lived
        // ScriptState event loops, tier-2 promotion across runs).
        #[cfg(feature = "djit")]
        {
            self.jit_error = None;
            if bytecode_changed {
                self.djit = crate::djit::DynasmJit::new();
            }
        }
        #[cfg(all(feature = "djit", target_arch = "x86_64"))]
        {
            self.deopt_pending = false;
            if bytecode_changed {
                self.tier2 = crate::codegen::tier2::Tier2Jit::new();
            }
        }
        // Last-callee cache holds raw pointers into the previous
        // program's CompiledFunctionObject; only reset when bytecode
        // changed (otherwise the cache stays warm across re-runs).
        #[cfg(feature = "djit")]
        if bytecode_changed {
            self.last_call_callee_bits = 0;
            self.last_call_fn_ptr = None;
            self.last_call_has_calls = false;
            self.last_call_instr = std::ptr::null();
            self.last_call_instr_len = 0;
            self.last_call_consts_raw = std::ptr::null();
            self.last_call_reg_count = 0;
            self.last_call_takes_this = false;
            self.last_call_cache_slots = 0;
            self.last_call_cv_len = 0;
            self.last_call_cv_ptr = std::ptr::null();
            self.last_call_func_cache = std::ptr::null();
            self.last_call_consts_values_ptr = std::ptr::null();
            self.last_call_consts_syms_ptr = std::ptr::null();
        }
    }

    /// Push an Object onto the Value stack (converts Object → Value).
    #[inline(always)]
    pub fn push(&mut self, obj: Object) -> Result<(), VMError> {
        if self.sp >= STACK_SIZE {
            return Err(VMError::StackOverflow);
        }
        let val = obj_into_val(obj, &mut self.heap);
        unsafe {
            let ptr = self.stack.as_mut_ptr().add(self.sp);
            std::ptr::write(ptr, val);
            self.stack.set_len(self.sp + 1);
        }
        self.sp += 1;
        Ok(())
    }

    /// Push a Value directly (no conversion needed).
    #[inline(always)]
    pub fn push_val(&mut self, val: Value) -> Result<(), VMError> {
        if self.sp >= STACK_SIZE {
            return Err(VMError::StackOverflow);
        }
        unsafe {
            let ptr = self.stack.as_mut_ptr().add(self.sp);
            std::ptr::write(ptr, val);
            self.stack.set_len(self.sp + 1);
        }
        self.sp += 1;
        Ok(())
    }

    /// Push a Value without bounds checking. Caller must have verified stack capacity.
    #[inline(always)]
    unsafe fn push_unchecked(&mut self, val: Value) {
        debug_assert!(self.sp < STACK_SIZE, "push_unchecked: stack overflow");
        let ptr = self.stack.as_mut_ptr().add(self.sp);
        std::ptr::write(ptr, val);
        self.stack.set_len(self.sp + 1);
        self.sp += 1;
    }

    /// Pop a Value from the stack.
    #[inline(always)]
    pub fn pop_val(&mut self) -> Result<Value, VMError> {
        if self.sp == 0 {
            return Err(VMError::StackUnderflow);
        }
        self.sp -= 1;
        unsafe {
            let val = *self.stack.as_ptr().add(self.sp);
            self.stack.set_len(self.sp);
            Ok(val)
        }
    }

    /// Pop a Value and convert to Object.
    #[inline(always)]
    pub fn pop(&mut self) -> Result<Object, VMError> {
        let val = self.pop_val()?;
        Ok(val_to_obj(val, &self.heap))
    }


    /// Re-check wall time and the external abort flag from inside a
    /// callback-taking builtin (`Array.sort` comparator,
    /// `Array.map`/`filter`/`reduce` callbacks, `Promise.then` bodies,
    /// etc.). The main dispatch loop only tests these every ~64 K
    /// opcodes, so without this a hostile `sort` comparator that hangs
    /// in user code could burn through any `max_wall_time_ms` budget
    /// before control returned to the dispatch match. Two reads and two
    /// branches per call — budget-negligible even at 1 M iterations.
    #[inline]
    pub(crate) fn check_builtin_callback_limits(&mut self) -> Result<(), VMError> {
        if let Some(max_ms) = self.config.max_wall_time_ms {
            if self.wall_time_exceeded(max_ms) {
                return Err(VMError::ExecutionTimeout(format!(
                    "Exceeded {}ms wall time (inside builtin callback)",
                    max_ms
                )));
            }
        }
        if let Some(ref flag) = self.config.abort_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(VMError::ExecutionTimeout(
                    "Execution aborted by host (inside builtin callback)".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Platform-agnostic "has the VM burned more than `max_ms` milliseconds
    /// since wall-time tracking started?". Also lazily initialises the
    /// start timestamp on first call.
    ///
    /// On `wasm32` this reads `Date.now()`; on native `riscv32` (zkVM) it
    /// always returns `false` because time is non-deterministic; otherwise
    /// it uses a monotonic `Instant`.
    #[inline(always)]
    pub(crate) fn wall_time_exceeded(&mut self, max_ms: u64) -> bool {
        #[cfg(not(any(target_arch = "wasm32", target_arch = "riscv32")))]
        {
            if self.quota.started_at.is_none() {
                self.quota.started_at = Some(Instant::now());
            }
            match self.quota.started_at {
                Some(started) => started.elapsed().as_millis() as u64 >= max_ms,
                None => false,
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let now = epoch_millis_now();
            let started = *self.quota.started_at_ms.get_or_insert(now);
            now - started >= max_ms as f64
        }
        #[cfg(target_arch = "riscv32")]
        {
            let _ = max_ms;
            false
        }
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn check_execution_limits(&mut self) -> Result<(), VMError> {
        self.quota.instructions += 1;
        #[cfg(not(any(target_arch = "wasm32", target_arch = "riscv32")))]
        if self.quota.started_at.is_none() {
            self.quota.started_at = Some(Instant::now());
        }

        // Check external abort flag (set by host on outer timeout).
        // Checked every ~16K instructions (same cadence as other periodic checks)
        // to avoid atomic load overhead on every single instruction.
        if (self.quota.instructions & 0x3fff) == 0 {
            if let Some(ref flag) = self.config.abort_flag {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(VMError::ExecutionTimeout(
                        "Execution aborted by host (external timeout)".to_string()));
                }
            }
        }

        let max_instructions = self.config.max_instructions;
        let must_check_now = (self.quota.instructions & 0x3fff) == 0
            || max_instructions
                .map(|m| self.quota.instructions >= m)
                .unwrap_or(false);

        if !must_check_now {
            return Ok(());
        }

        if let Some(max) = max_instructions {
            if self.quota.instructions > max {
                return Err(VMError::ExecutionTimeout(format!(
                    "Execution exceeded maximum instruction count: {}",
                    max
                )));
            }
        }

        // Check heap limits to prevent OOM from unbounded allocation.
        // The Rust process aborts on OOM, killing ALL concurrent executions.
        if let Some(max_heap) = self.config.max_heap_objects {
            if self.heap.allocated_count() > max_heap {
                return Err(VMError::ExecutionTimeout(format!(
                    "Heap object limit exceeded: {} objects (limit: {})",
                    self.heap.allocated_count(), max_heap
                )));
            }
        }
        if let Some(max_bytes) = self.config.max_heap_bytes {
            let used = self.heap.estimated_memory_bytes();
            if used > max_bytes {
                return Err(VMError::ExecutionTimeout(format!(
                    "Heap memory limit exceeded: {}MB (limit: {}MB)",
                    used / (1024 * 1024), max_bytes / (1024 * 1024)
                )));
            }
        }

        // Per-thread string-interner cap. Unique symbols (property names,
        // hash keys, `obj['k' + i]` patterns) accumulate forever because
        // the interner has no eviction; without a ceiling a hostile
        // script can grow the thread-local table without ever tripping
        // `max_heap_bytes` (which doesn't account for interner state).
        let interned = crate::intern::interned_count();
        if interned > crate::intern::MAX_INTERNED_SYMBOLS {
            return Err(VMError::ExecutionTimeout(format!(
                "Interned-symbol limit exceeded: {} symbols (limit: {})",
                interned,
                crate::intern::MAX_INTERNED_SYMBOLS
            )));
        }

        #[cfg(not(any(target_arch = "wasm32", target_arch = "riscv32")))]
        if let Some(max_ms) = self.config.max_wall_time_ms {
            let elapsed = self
                .quota
                .started_at
                .map(|x| x.elapsed())
                .unwrap_or_else(|| Duration::from_millis(0));
            if elapsed > Duration::from_millis(max_ms) {
                return Err(VMError::ExecutionTimeout(format!(
                    "Execution exceeded maximum wall time: {}ms",
                    max_ms
                )));
            }
        }

        #[cfg(target_arch = "wasm32")]
        if let Some(max_ms) = self.config.max_wall_time_ms {
            let now = epoch_millis_now();
            let started = *self.quota.started_at_ms.get_or_insert(now);
            if now - started > max_ms as f64 {
                return Err(VMError::ExecutionTimeout(format!(
                    "Execution exceeded maximum wall time: {}ms",
                    max_ms
                )));
            }
        }
        Ok(())
    }

    /// Reset VM state for re-execution with the same bytecode.
    /// Preserves JIT caches for warm re-runs, resets heap and stack.
    /// Light reset for re-execution. Preserves heap, JIT caches, inline caches,
    /// and all bytecode pointers. Use for benchmarks where the same bytecode runs
    /// repeatedly — function instruction pointers remain stable for JIT keying.
    /// Set the heap baseline for truncation. Call after warmup/compilation
    /// to enable heap cleanup between benchmark runs.
    pub fn set_heap_baseline(&mut self) {
        self.heap_baseline = self.heap.objects.len();
    }

    pub fn reset_for_rerun(&mut self, _bytecode: &crate::bytecode::Bytecode) {
        self.ip = 0;
        self.sp = 0;
        self.stack.clear();
        self.frames.clear();
        self.rframes.clear();
        self.last_popped = None;
        // Keep constants cache between runs (same bytecode = same constants).
        // Only invalidate if heap truncation destroys referenced objects (below).
        self.inst_ptr = self.instructions.as_ptr();
        self.inst_len = self.instructions.len();

        // Truncate heap back to baseline if explicitly set (benchmark mode).
        // This reclaims runtime objects from the previous run while preserving
        // compiled functions and globals. Gives each run a clean, compact heap.
        if self.heap_baseline > 0 && self.heap.objects.len() > self.heap_baseline {
            self.heap.objects.truncate(self.heap_baseline);
            self.heap.free_list.clear();
            self.heap.rc_index_clear();
            // Constants cache: preconverted Values were allocated during warmup
            // (before set_heap_baseline), so they live BELOW the baseline and
            // survive truncation. Only clear if bump region overlaps baseline
            // (which would mean constants might have been bump-allocated above it).
            if self.heap.bump_end > 0
                && (self.heap.bump_end as usize) > self.heap_baseline
            {
                self.constants_values_cache.clear();
                self.last_preconvert_key = usize::MAX;
            }

            // Re-arm bump allocator within its ORIGINAL region only.
            // The bump region was created by ensure_bump_capacity(2048) at
            // [bump_end-2048 .. bump_end]. Never overlap with initial objects.
            if self.heap.bump_end > 0 {
                let original_start = self.heap.bump_end.saturating_sub(2048);
                // Only re-arm if the region is below baseline (safe to reuse)
                if (original_start as usize) < self.heap_baseline
                    && (self.heap.bump_end as usize) <= self.heap_baseline
                {
                    self.heap.bump_next = original_start;
                } else {
                    self.heap.bump_next = self.heap.bump_end; // exhausted
                }
            }

            // Invalidate cached pointers (truncated objects are gone)
            #[cfg(feature = "djit")]
            {
                self.cached_map_obj = 0;
                self.cached_map_entries = std::ptr::null_mut();
                self.cached_map_indices = std::ptr::null_mut();
                self.cached_map_buckets_data = std::ptr::null();
                self.cached_map_entries_data = std::ptr::null();
                self.cached_map_mask = 0;
                self.cached_hash_obj = 0;
                self.cached_hash_borrow = std::ptr::null_mut();
                self.cached_values_ptr = std::ptr::null_mut();
                self.cached_shape = 0;
            }
        }
    }

    /// Unwind call frames back to the given depth, restoring state.
    pub(crate) fn unwind_frames(&mut self, target_depth: usize) {
        while self.frames.len() > target_depth {
            self.restore_caller_frame();
        }
    }

    /// Pop one call frame and restore the caller's state. Returns whether
    /// the popped frame was async (needed to decide if the return value
    /// should be wrapped in a Promise).
    #[inline(never)]
    pub(crate) fn restore_caller_frame(&mut self) -> bool {
        let frame = self.frames.pop().unwrap();
        // Write the function's warm cache back to its persistent storage,
        // but only if the function actually uses inline caching.
        if let Some(ref func_cache) = frame.func_cache {
            *unsafe { func_cache.borrow_mut() } =
                std::mem::replace(&mut self.inline_cache, frame.inline_cache);
        }
        // Return used locals to pool for reuse.
        let mut used_locals = std::mem::replace(&mut self.locals, frame.locals);
        used_locals.clear();
        self.locals_pool.push(used_locals);
        self.instructions = frame.instructions;
        self.inst_ptr = self.instructions.as_ptr();
        self.inst_len = self.instructions.len();
        self.constants = frame.constants;
        self.ip = frame.ip;
        // The stack pointer should already be at the correct depth after
        // OpReturnValue pops the return value. Force sp to frame.sp to
        // handle any unbalanced stack from expression temporaries.
        self.sp = frame.sp;
        self.max_stack_depth = frame.max_stack_depth;
        frame.is_async
    }

    /// Value-native property get.  For HashObject receivers the value is
    /// returned directly from `hash.values` (which already stores `Value`)
    /// without any Object conversion or stack push/pop.
    #[inline(always)]
    pub(crate) fn get_property_val(
        &mut self,
        receiver_val: Value,
        prop_sym: u32,
        cache_slot: usize,
    ) -> Result<Value, VMError> {
        // Note: In standard JS, null/undefined property access throws TypeError.
        // Currently lenient (returns undefined) for webpack compatibility.
        // TODO: Enable this once webpack circular dep resolution is fixed.
        if receiver_val.is_null() || receiver_val.is_undefined() {
            return Ok(Value::UNDEFINED);
        }
        if receiver_val.is_heap() {
            let heap_obj = unsafe {
                &*self
                    .heap
                    .objects
                    .as_ptr()
                    .add(receiver_val.heap_index() as usize)
            };
            if let Object::Hash(hash_rc) = heap_obj {
                let hash = hash_rc.borrow();

                // Check for getter accessor before data property path.
                if hash.has_accessors() {
                    let prop_name = crate::intern::resolve(prop_sym);
                    if let Some(getter) = hash.get_getter(&prop_name) {
                        let getter_func = getter.clone();
                        let _ = hash; // end borrow before calling accessor
                        let (result, _) = self.execute_compiled_function_slice(
                            getter_func,
                            &[],
                            Some(receiver_val),
                        )?;
                        return Ok(result);
                    }
                }

                // Inline cache hit
                if cache_slot >= self.inline_cache.len() {
                    self.inline_cache.resize(cache_slot + 1, (0, 0));
                }
                let (cached_shape, cached_offset) =
                    unsafe { *self.inline_cache.get_unchecked(cache_slot) };
                if cached_shape == hash.shape_version {
                    let slot = cached_offset as usize;
                    debug_assert!(slot < hash.values.len());
                    let val = unsafe { *hash.values.get_unchecked(slot) };
                    // Return raw Value — preserves identity for === comparison.
                    // BoundMethod wrapping happens only in CallMethod path.
                    return Ok(val);
                }
                // Cache miss: symbol lookup + cache update
                if let Some(&pair_index) = hash.str_slots.get(&prop_sym) {
                    self.inline_cache[cache_slot] = (hash.shape_version, pair_index as u32);
                    let val = unsafe { hash.get_value_at_slot_unchecked(pair_index) };
                    return Ok(val);
                }
                return Ok(Value::UNDEFINED);
            }
            // Instance: read fields/methods directly from the heap.
            if matches!(heap_obj, Object::Instance(_)) {
                let heap_idx = receiver_val.heap_index() as usize;
                let prop_name = crate::intern::resolve(prop_sym);

                // Check getter first
                let getter = match &self.heap.objects[heap_idx] {
                    Object::Instance(inst) => inst.getters.get(&*prop_name).cloned(),
                    _ => None,
                };
                if let Some(getter_func) = getter {
                    let (result, _) = self.execute_compiled_function_slice(
                        getter_func,
                        &[],
                        Some(receiver_val),
                    )?;
                    return Ok(result);
                }

                // Read field or method
                return match &self.heap.objects[heap_idx] {
                    Object::Instance(inst) => {
                        if let Some(field_val) = inst.fields.get(&*prop_name) {
                            Ok(*field_val)
                        } else if let Some(method) = inst.methods.get(&*prop_name) {
                            let func_val = obj_into_val(
                                Object::CompiledFunction(Box::new(method.clone())),
                                &mut self.heap,
                            );
                            self.maybe_bind_method_val(func_val, receiver_val)
                        } else {
                            Ok(Value::UNDEFINED)
                        }
                    }
                    Object::Error(e) => {
                        let val: Option<Rc<str>> = match &*prop_name {
                            "message" => Some(e.message.clone()),
                            "name" => Some(e.name.clone()),
                            "stack" => Some(Rc::from("")),
                            _ => None,
                        };
                        if let Some(s) = val {
                            Ok(obj_into_val(Object::String(s), &mut self.heap))
                        } else {
                            Ok(Value::UNDEFINED)
                        }
                    }
                    _ => Ok(Value::UNDEFINED),
                };
            }
        }
        // CompiledFunction: check properties map, auto-create .prototype
        if receiver_val.is_heap() {
            let heap_idx = receiver_val.heap_index() as usize;
            let heap_obj = unsafe { &*self.heap.objects.as_ptr().add(heap_idx) };
            if let Object::CompiledFunction(func) = heap_obj {
                if let Some(ref props) = func.properties {
                    if let Some(&val) = props.get(&prop_sym) {
                        return Ok(val);
                    }
                }
                // Auto-create .prototype when first accessed (JS functions
                // have an implicit prototype property = {})
                let proto_sym = crate::intern::intern("prototype");
                if prop_sym == proto_sym {
                    let proto = make_hash(HashObject::default());
                    let proto_val = obj_into_val(proto, &mut self.heap);
                    let heap_obj = unsafe { &mut *self.heap.objects.as_mut_ptr().add(heap_idx) };
                    if let Object::CompiledFunction(func) = heap_obj {
                        func.properties
                            .get_or_insert_with(|| Box::new(rustc_hash::FxHashMap::default()))
                            .insert(proto_sym, proto_val);
                    }
                    return Ok(proto_val);
                }
                // Also auto-create "length" and "name" for functions
                let name = crate::intern::resolve(prop_sym);
                if &*name == "length" {
                    return Ok(Value::from_i32(func.num_parameters as i32));
                }
                if &*name == "name" {
                    return Ok(Value::UNDEFINED);
                }
            }
        }

        // Non-Hash/Instance/Function fallback: use existing stack-based path
        let obj = val_to_obj(receiver_val, &self.heap);
        let index_obj = Object::String(crate::intern::resolve(prop_sym));
        self.execute_index_expression(obj, index_obj)?;
        self.pop_val()
    }

    /// Value-native property set.  For HashObject receivers the value is
    /// written directly into `hash.values` (already `Vec<Value>`) without
    /// any Object conversion.  Returns `None` when mutated in-place (Hash),
    /// `Some(updated_receiver)` for non-Hash types needing store-back.
    #[inline(always)]
    pub(crate) fn set_property_val(
        &mut self,
        receiver_val: Value,
        prop_sym: u32,
        value: Value,
        cache_slot: usize,
    ) -> Result<Option<Value>, VMError> {
        if receiver_val.is_heap() {
            let heap_obj = unsafe {
                &*self
                    .heap
                    .objects
                    .as_ptr()
                    .add(receiver_val.heap_index() as usize)
            };
            if let Object::Hash(hash_rc) = heap_obj {
                // Check for setter accessor before data property path.
                {
                    let hash = hash_rc.borrow();
                    if hash.has_accessors() {
                        let prop_name = crate::intern::resolve(prop_sym);
                        if let Some(setter) = hash.get_setter(&prop_name) {
                            let setter_func = setter.clone();
                            let _ = hash; // end borrow before calling accessor
                            self.execute_compiled_function_slice(
                                setter_func,
                                std::slice::from_ref(&value),
                                Some(receiver_val),
                            )?;
                            return Ok(None);
                        }
                    }
                }

                let hash = unsafe { hash_rc.borrow_mut() };
                // Frozen check: silently ignore writes to frozen objects
                if hash.frozen {
                    return Ok(None);
                }
                // Inline cache hit
                if cache_slot >= self.inline_cache.len() {
                    self.inline_cache.resize(cache_slot + 1, (0, 0));
                }
                let (cached_shape, cached_offset) =
                    unsafe { *self.inline_cache.get_unchecked(cache_slot) };
                if cached_shape == hash.shape_version {
                    let slot = cached_offset as usize;
                    debug_assert!(slot < hash.values.len());
                    unsafe { hash.set_value_at_slot_unchecked(slot, value) };
                    return Ok(None);
                }
                // Cache miss: full insert + cache update
                hash.set_by_sym(prop_sym, value);
                if let Some(&slot) = hash.str_slots.get(&prop_sym) {
                    self.inline_cache[cache_slot] = (hash.shape_version, slot as u32);
                }
                return Ok(None);
            }
            // Instance: modify fields directly on the heap (in-place).
            if matches!(heap_obj, Object::Instance(_)) {
                let heap_idx = receiver_val.heap_index() as usize;
                let prop_name = crate::intern::resolve(prop_sym);

                // Check setter first
                let setter = match &self.heap.objects[heap_idx] {
                    Object::Instance(inst) => inst.setters.get(&*prop_name).cloned(),
                    _ => None,
                };
                if let Some(setter_func) = setter {
                    self.execute_compiled_function_slice(
                        setter_func,
                        std::slice::from_ref(&value),
                        Some(receiver_val),
                    )?;
                    return Ok(None);
                }

                if let Object::Instance(inst) = &mut self.heap.objects[heap_idx] {
                    inst.fields.insert(prop_name.to_string(), value);
                }
                return Ok(None);
            }
            // Class: modify static fields directly on the heap (in-place).
            if matches!(heap_obj, Object::Class(_)) {
                let heap_idx = receiver_val.heap_index() as usize;
                let prop_name = crate::intern::resolve(prop_sym);
                let val_obj = val_to_obj(value, &self.heap);
                if let Object::Class(class_obj) = &mut self.heap.objects[heap_idx] {
                    class_obj.static_fields.insert(prop_name.to_string(), val_obj);
                }
                return Ok(None);
            }
        }
        // CompiledFunction: store in properties map
        if receiver_val.is_heap() {
            let heap_obj = unsafe { &mut *self.heap.objects.as_mut_ptr().add(receiver_val.heap_index() as usize) };
            if let Object::CompiledFunction(func) = heap_obj {
                let props = func.properties.get_or_insert_with(|| Box::new(rustc_hash::FxHashMap::default()));
                props.insert(prop_sym, value);
                return Ok(None);
            }
        }
        // Non-Hash/Instance/Class/Function fallback
        let obj = val_to_obj(receiver_val, &self.heap);
        let val_obj = val_to_obj(value, &self.heap);
        self.execute_set_index(
            obj,
            Object::String(crate::intern::resolve(prop_sym)),
            val_obj,
        )?;
        let updated = self.pop_val()?;
        Ok(Some(updated))
    }

    /// Coerce a Value to its string representation for string concatenation.
    /// For Instance objects, calls `toString()` if defined.
    /// Returns the string, or falls back to `inspect()`.
    pub(crate) fn coerce_to_string_val(&mut self, val: Value) -> Result<Rc<str>, VMError> {
        if val.is_heap() {
            let heap_idx = val.heap_index() as usize;
            let heap_obj = unsafe { &*self.heap.objects.as_ptr().add(heap_idx) };
            if let Object::Instance(inst) = heap_obj {
                if let Some(to_str_func) = inst.methods.get("toString").cloned() {
                    let (result, _) = self.execute_compiled_function_slice(
                        to_str_func,
                        &[],
                        Some(val),
                    )?;
                    let result_obj = val_to_obj(result, &self.heap);
                    return Ok(Rc::from(result_obj.inspect()));
                }
                return Ok(Rc::from(format!("[Instance {}]", inst.class_name)));
            }
        }
        Ok(Rc::from(val_to_obj(val, &self.heap).inspect().as_str()))
    }

    /// If `val` is a heap CompiledFunction, wrap it as a BoundMethod with
    /// `receiver_val` as the receiver.  For non-function values (the common
    /// case for data properties), returns `val` unchanged.
    #[inline(always)]
    pub(crate) fn maybe_bind_method_val(
        &mut self,
        val: Value,
        receiver_val: Value,
    ) -> Result<Value, VMError> {
        if val.is_heap() {
            let heap_obj = unsafe { &*self.heap.objects.as_ptr().add(val.heap_index() as usize) };
            if let Object::CompiledFunction(func) = heap_obj {
                let bound = Object::BoundMethod(Box::new(crate::object::BoundMethodObject {
                    function: (**func).clone(),
                    receiver: Box::new(val_to_obj(receiver_val, &self.heap)),
                }));
                return Ok(obj_into_val(bound, &mut self.heap));
            }
        }
        Ok(val)
    }




    /// Like `set_property_fast_path` but discards the result value.
    /// Used by OpSetLocalPropertyPop / OpSetGlobalPropertyPop to avoid
    /// cloning the value just to push+pop it immediately.
    /// Returns `Some(updated_receiver)` for non-Hash types that need store-back,
    /// or `None` for Hash (mutated in-place).

    #[inline(always)]
    pub(crate) fn add_objects(&self, left: &Object, right: &Object) -> Result<Object, VMError> {
        match (left, right) {
            (Object::Integer(a), Object::Integer(b)) => Ok(Object::Integer(a + b)),
            (Object::Float(a), Object::Float(b)) => Ok(Object::Float(a + b)),
            (Object::Integer(a), Object::Float(b)) => Ok(Object::Float(*a as f64 + b)),
            (Object::Float(a), Object::Integer(b)) => Ok(Object::Float(a + *b as f64)),
            // BigInt arithmetic. Spec says BigInt + Number throws — we
            // promote here for ergonomic loops; tighten later if needed.
            (Object::BigInt(a), Object::BigInt(b)) => Ok(Object::BigInt(a.wrapping_add(*b))),
            (Object::BigInt(a), Object::Integer(b)) => {
                Ok(Object::BigInt(a.wrapping_add(*b as i128)))
            }
            (Object::Integer(a), Object::BigInt(b)) => {
                Ok(Object::BigInt((*a as i128).wrapping_add(*b)))
            }
            (Object::String(a), Object::String(b)) => {
                let mut s = String::with_capacity(a.len() + b.len());
                s.push_str(a);
                s.push_str(b);
                if s.len() > MAX_STRING_LENGTH {
                    return Err(VMError::TypeError(
                        ERR_STRING_LEN.to_string(),
                    ));
                }
                Ok(Object::String(s.into()))
            }
            (Object::String(a), b) => {
                let b_str: Cow<str> = match b {
                    Object::Integer(v) => {
                        let mut buf = itoa::Buffer::new();
                        Cow::Owned(buf.format(*v).to_string())
                    }
                    Object::Float(v) => {
                        let v = *v;
                        if v.is_finite() && v.fract() == 0.0 && v.abs() < i64::MAX as f64 {
                            let mut buf = itoa::Buffer::new();
                            Cow::Owned(buf.format(v as i64).to_string())
                        } else {
                            Cow::Owned(b.inspect())
                        }
                    }
                    Object::Array(items) => {
                        Cow::Owned(self.array_to_js_string(items.borrow()))
                    }
                    Object::Hash(_) | Object::Instance(_) => {
                        Cow::Borrowed("[object Object]")
                    }
                    _ => Cow::Owned(b.to_js_string()),
                };
                let mut s = String::with_capacity(a.len() + b_str.len());
                s.push_str(a);
                s.push_str(&b_str);
                if s.len() > MAX_STRING_LENGTH {
                    return Err(VMError::TypeError(
                        ERR_STRING_LEN.to_string(),
                    ));
                }
                Ok(Object::String(s.into()))
            }
            (a, Object::String(b)) => {
                let a_str: Cow<str> = match a {
                    Object::Integer(v) => {
                        let mut buf = itoa::Buffer::new();
                        Cow::Owned(buf.format(*v).to_string())
                    }
                    Object::Float(v) => {
                        let v = *v;
                        if v.is_finite() && v.fract() == 0.0 && v.abs() < i64::MAX as f64 {
                            let mut buf = itoa::Buffer::new();
                            Cow::Owned(buf.format(v as i64).to_string())
                        } else {
                            Cow::Owned(a.inspect())
                        }
                    }
                    Object::Array(items) => {
                        Cow::Owned(self.array_to_js_string(items.borrow()))
                    }
                    Object::Hash(_) | Object::Instance(_) => {
                        Cow::Borrowed("[object Object]")
                    }
                    _ => Cow::Owned(a.to_js_string()),
                };
                let mut s = String::with_capacity(a_str.len() + b.len());
                s.push_str(&a_str);
                s.push_str(b);
                if s.len() > MAX_STRING_LENGTH {
                    return Err(VMError::TypeError(
                        ERR_STRING_LEN.to_string(),
                    ));
                }
                Ok(Object::String(s.into()))
            }
            // JS behavior: coerce to numbers and add
            (a, b) => {
                let x = self.to_number(a)?;
                let y = self.to_number(b)?;
                Ok(Object::Float(x + y))
            }
        }
    }

    /// String concatenation using the VM's reusable scratch buffer.
    /// Only called from OpAdd where operands are owned (not borrowed from self).
    /// Avoids allocating a new String on every `+` — the buffer is cleared
    /// and reused, only the final `Rc<str>` is heap-allocated.

    #[inline(always)]
    pub(crate) fn mod_objects(&self, left: &Object, right: &Object) -> Result<Object, VMError> {
        let (a, b) = match (left, right) {
            (Object::Integer(a), Object::Integer(b)) => (*a as f64, *b as f64),
            (Object::Float(a), Object::Float(b)) => (*a, *b),
            (Object::Integer(a), Object::Float(b)) => (*a as f64, *b),
            (Object::Float(a), Object::Integer(b)) => (*a, *b as f64),
            (x, y) => (self.to_number(x)?, self.to_number(y)?),
        };
        Ok(Object::Float(a % b))
    }

    #[inline(always)]
    pub(crate) fn is_truthy(&self, value: &Object) -> bool {
        match value {
            Object::Boolean(v) => *v,
            Object::Null | Object::Undefined => false,
            Object::Integer(v) => *v != 0,
            Object::Float(v) => *v != 0.0 && !v.is_nan(),
            Object::String(s) => !s.is_empty(),
            _ => true,
        }
    }

    #[inline(always)]
    pub(crate) fn equals(&self, a: &Object, b: &Object) -> bool {
        match (a, b) {
            (Object::BigInt(x), Object::BigInt(y)) => x == y,
            (Object::BigInt(x), Object::Integer(y)) | (Object::Integer(y), Object::BigInt(x)) => {
                *x == *y as i128
            }
            (Object::Integer(x), Object::Integer(y)) => x == y,
            (Object::Float(x), Object::Float(y)) => x == y,
            (Object::Integer(x), Object::Float(y)) => (*x as f64) == *y,
            (Object::Float(x), Object::Integer(y)) => *x == (*y as f64),
            (Object::Boolean(x), Object::Boolean(y)) => x == y,
            (Object::String(x), Object::String(y)) => Rc::ptr_eq(x, y) || x == y,
            (Object::Null, Object::Null) => true,
            (Object::Null, Object::Undefined) | (Object::Undefined, Object::Null) => true,
            (Object::Undefined, Object::Undefined) => true,
            (Object::Boolean(v), other) => {
                let n = Object::Integer(if *v { 1 } else { 0 });
                self.equals(&n, other)
            }
            (other, Object::Boolean(v)) => {
                let n = Object::Integer(if *v { 1 } else { 0 });
                self.equals(other, &n)
            }
            (Object::String(s), Object::Integer(n)) => {
                let parsed = Self::js_string_to_number(s);
                !parsed.is_nan() && parsed == (*n as f64)
            }
            (Object::String(s), Object::Float(n)) => {
                let parsed = Self::js_string_to_number(s);
                !parsed.is_nan() && parsed == *n
            }
            (Object::Integer(n), Object::String(s)) => {
                let parsed = Self::js_string_to_number(s);
                !parsed.is_nan() && (*n as f64) == parsed
            }
            (Object::Float(n), Object::String(s)) => {
                let parsed = Self::js_string_to_number(s);
                !parsed.is_nan() && *n == parsed
            }
            (Object::Array(_), _)
            | (Object::Hash(_), _)
            | (_, Object::Array(_))
            | (_, Object::Hash(_)) => {
                let left = self.to_primitive_for_loose_eq(a);
                let right = self.to_primitive_for_loose_eq(b);
                match (left, right) {
                    (Some(l), Some(r)) => self.equals(&l, &r),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn to_primitive_for_loose_eq(&self, value: &Object) -> Option<Object> {
        match value {
            Object::Array(items) => {
                let borrowed = items.borrow();
                Some(Object::String(self.array_to_js_string(borrowed).into()))
            }
            Object::Hash(_) => Some(Object::String("[object Object]".to_string().into())),
            other => Some(other.clone()),
        }
    }

    fn array_to_js_string(&self, items: &[Value]) -> String {
        let mut parts = Vec::with_capacity(items.len());
        for item in items {
            let obj = val_to_obj(*item, &self.heap);
            let piece = match &obj {
                Object::Undefined | Object::Null => String::new(),
                Object::String(s) => s.to_string(),
                Object::Integer(v) => v.to_string(),
                Object::Float(v) => v.to_string(),
                Object::Boolean(v) => {
                    if *v {
                        "true".to_string()
                    } else {
                        "false".to_string()
                    }
                }
                Object::Array(nested) => self.array_to_js_string(nested.borrow()),
                Object::Hash(_) => "[object Object]".to_string(),
                other => other.inspect(),
            };
            parts.push(piece);
        }
        parts.join(",")
    }

    fn to_js_string(&self, value: &Object) -> String {
        match value {
            Object::String(s) => s.to_string(),
            Object::Integer(v) => v.to_string(),
            Object::Float(v) => v.to_string(),
            Object::Boolean(v) => {
                if *v {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
            Object::Null => "null".to_string(),
            Object::Undefined => "undefined".to_string(),
            Object::Array(items) => {
                let borrowed = items.borrow();
                self.array_to_js_string(borrowed)
            }
            Object::Hash(_) => "[object Object]".to_string(),
            other => other.inspect(),
        }
    }

    #[inline(always)]
    pub(crate) fn strict_equals(&self, a: &Object, b: &Object) -> bool {
        match (a, b) {
            (Object::Integer(x), Object::Integer(y)) => x == y,
            (Object::Float(x), Object::Float(y)) => x == y,
            (Object::Integer(x), Object::Float(y)) => (*x as f64) == *y,
            (Object::Float(x), Object::Integer(y)) => *x == (*y as f64),
            (Object::Array(xs), Object::Array(ys)) => {
                let xs = xs.borrow();
                let ys = ys.borrow();
                xs.len() == ys.len()
                    && xs.iter().zip(ys.iter()).all(|(x, y)| {
                        let xo = val_to_obj(*x, &self.heap);
                        let yo = val_to_obj(*y, &self.heap);
                        self.strict_equals(&xo, &yo)
                    })
            }
            (Object::Hash(xh), Object::Hash(yh)) => {
                let xh = unsafe { xh.borrow_mut() };
                let yh = unsafe { yh.borrow_mut() };
                if xh.pairs.len() != yh.pairs.len() {
                    return false;
                }
                xh.sync_pairs_if_dirty();
                yh.sync_pairs_if_dirty();
                xh.pairs.iter().all(|(k, xv)| {
                    yh.pairs
                        .get(k)
                        .map(|yv| {
                            let xo = val_to_obj(*xv, &self.heap);
                            let yo = val_to_obj(*yv, &self.heap);
                            self.strict_equals(&xo, &yo)
                        })
                        .unwrap_or(false)
                })
            }
            (Object::Boolean(x), Object::Boolean(y)) => x == y,
            (Object::String(x), Object::String(y)) => Rc::ptr_eq(x, y) || x == y,
            (Object::Null, Object::Null) => true,
            (Object::Undefined, Object::Undefined) => true,
            _ => false,
        }
    }

    fn same_value(&self, a: &Object, b: &Object) -> bool {
        match (a, b) {
            (Object::Integer(x), Object::Integer(y)) => x == y,
            (Object::Float(x), Object::Float(y)) => {
                if x.is_nan() && y.is_nan() {
                    true
                } else if *x == 0.0 && *y == 0.0 {
                    x.is_sign_negative() == y.is_sign_negative()
                } else {
                    x == y
                }
            }
            (Object::Integer(x), Object::Float(y)) | (Object::Float(y), Object::Integer(x)) => {
                let xf = *x as f64;
                if xf == 0.0 && *y == 0.0 {
                    !y.is_sign_negative()
                } else {
                    xf == *y
                }
            }
            (Object::Boolean(x), Object::Boolean(y)) => x == y,
            (Object::String(x), Object::String(y)) => x == y,
            (Object::Null, Object::Null) => true,
            (Object::Undefined, Object::Undefined) => true,
            _ => false,
        }
    }

    pub(crate) fn to_number(&self, value: &Object) -> Result<f64, VMError> {
        match value {
            Object::Integer(v) => Ok(*v as f64),
            Object::Float(v) => Ok(*v),
            Object::Boolean(v) => Ok(if *v { 1.0 } else { 0.0 }),
            Object::Null => Ok(0.0),
            Object::Undefined => Ok(f64::NAN),
            Object::BigInt(v) => Ok(*v as f64),
            Object::String(s) => Ok(Self::js_string_to_number(s)),
            Object::Array(items) => {
                let borrowed = items.borrow();
                Ok(Self::js_string_to_number(
                    &self.array_to_js_string(borrowed),
                ))
            }
            Object::Hash(_) => Ok(f64::NAN),
            // Match JavaScript behavior: Number(function) → NaN, Number(anything) → NaN
            _ => Ok(f64::NAN),
        }
    }

    /// Inverse of `days_to_ymd` — convert (year, month [1..12], day [1..31])
    /// to days since the Unix epoch (1970-01-01). Same Hinnant algorithm.
    /// Used by Date setters and `Date.UTC`.
    pub(crate) fn ymd_to_days(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = (y - era * 400) as i64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }

    /// Convert days since Unix epoch to (year, month, day).
    fn days_to_ymd(days: i64) -> (i64, i64, i64) {
        // Algorithm from Howard Hinnant's civil_from_days
        let z = days + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = (z - era * 146097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = if m <= 2 { y + 1 } else { y };
        (year, m as i64, d as i64)
    }

    /// Extract epoch ms from a Date method's receiver.
    ///
    /// The constructor binds receiver to the Date's backing Hash so
    /// setters can mutate the `__time_ms` field shared across all
    /// method invocations on the same Date instance. Legacy callers
    /// that bind receiver=Object::Float still work (reads only).
    fn extract_date_ms(receiver: &Option<Object>) -> f64 {
        match receiver {
            Some(Object::Hash(h)) => {
                let hb = h.borrow();
                if let Some(v) = hb.get_by_str("__time_ms") {
                    if v.is_f64() {
                        return v.as_f64();
                    }
                    if v.is_i32() {
                        return unsafe { v.as_i32_unchecked() } as f64;
                    }
                }
                epoch_millis_now()
            }
            Some(Object::Float(ms)) => *ms,
            Some(Object::Integer(ms)) => *ms as f64,
            _ => epoch_millis_now(),
        }
    }

    /// Write a new ms value back into a Date receiver's backing Hash.
    /// Returns the resulting ms (so setters can also return the new
    /// timestamp the way V8 does). No-op when the receiver isn't a
    /// hash-backed Date.
    pub(crate) fn store_date_ms(receiver: &Option<Object>, ms: f64) -> f64 {
        if let Some(Object::Hash(h)) = receiver {
            unsafe { h.borrow_mut() }.set_by_str(
                std::rc::Rc::from("__time_ms"),
                Value::from_f64(ms),
            );
        }
        ms
    }

    fn js_string_to_number(s: &str) -> f64 {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return 0.0;
        }

        if trimmed == "Infinity" || trimmed == "+Infinity" {
            return f64::INFINITY;
        }
        if trimmed == "-Infinity" {
            return f64::NEG_INFINITY;
        }

        let (sign, body) = if let Some(rest) = trimmed.strip_prefix('-') {
            (-1.0, rest)
        } else if let Some(rest) = trimmed.strip_prefix('+') {
            (1.0, rest)
        } else {
            (1.0, trimmed)
        };

        if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            if hex.is_empty() {
                return f64::NAN;
            }
            if let Ok(v) = i64::from_str_radix(hex, 16) {
                return sign * (v as f64);
            }
            return f64::NAN;
        }

        if let Some(bin) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            if bin.is_empty() {
                return f64::NAN;
            }
            if let Ok(v) = i64::from_str_radix(bin, 2) {
                return sign * (v as f64);
            }
            return f64::NAN;
        }

        if let Some(oct) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            if oct.is_empty() {
                return f64::NAN;
            }
            if let Ok(v) = i64::from_str_radix(oct, 8) {
                return sign * (v as f64);
            }
            return f64::NAN;
        }

        trimmed.parse::<f64>().unwrap_or(f64::NAN)
    }

    pub(crate) fn to_i32(&self, value: &Object) -> Result<i32, VMError> {
        let n = self.to_number(value)?;
        if n.is_nan() || n.is_infinite() {
            return Ok(0);
        }
        Ok((n as i64 as u32) as i32)
    }

    fn to_u32(&self, value: &Object) -> Result<u32, VMError> {
        let n = self.to_number(value)?;
        if n.is_nan() || n.is_infinite() {
            return Ok(0);
        }
        Ok(n as i64 as u32)
    }

    pub(crate) fn to_number_val(&self, val: Value) -> Result<f64, VMError> {
        if val.is_i32() {
            return Ok(unsafe { val.as_i32_unchecked() } as f64);
        }
        if val.is_f64() {
            return Ok(val.as_f64());
        }
        self.to_number(&val_to_obj(val, &self.heap))
    }

    /// Like to_number_val but calls valueOf() on Instance objects if available.
    pub(crate) fn coerce_to_number_val(&mut self, val: Value) -> Result<f64, VMError> {
        if val.is_i32() {
            return Ok(unsafe { val.as_i32_unchecked() } as f64);
        }
        if val.is_f64() {
            return Ok(val.as_f64());
        }
        if val.is_heap() {
            let heap_idx = val.heap_index() as usize;
            let heap_obj = unsafe { &*self.heap.objects.as_ptr().add(heap_idx) };
            if let Object::Instance(inst) = heap_obj {
                if let Some(value_of_func) = inst.methods.get("valueOf").cloned() {
                    let (result, _) = self.execute_compiled_function_slice(
                        value_of_func,
                        &[],
                        Some(val),
                    )?;
                    return self.to_number_val(result);
                }
            }
        }
        self.to_number(&val_to_obj(val, &self.heap))
    }

    /// Coerce an Instance to a primitive via valueOf()/toString() for the + operator.
    /// Returns the coerced Value (which may be a number or string).

    pub(crate) fn to_i32_val(&self, val: Value) -> Result<i32, VMError> {
        if val.is_i32() {
            return Ok(unsafe { val.as_i32_unchecked() });
        }
        self.to_i32(&val_to_obj(val, &self.heap))
    }

    fn to_u32_val(&self, val: Value) -> Result<u32, VMError> {
        if val.is_i32() {
            let i = unsafe { val.as_i32_unchecked() };
            return Ok(i as u32);
        }
        self.to_u32(&val_to_obj(val, &self.heap))
    }

    #[inline(always)]
    pub(crate) fn compare_numeric(
        &self,
        a: &Object,
        b: &Object,
        op: NumericCmp,
    ) -> Result<bool, VMError> {
        let (x, y) = match (a, b) {
            (Object::Integer(x), Object::Integer(y)) => (*x as f64, *y as f64),
            (Object::Float(x), Object::Float(y)) => (*x, *y),
            (Object::Integer(x), Object::Float(y)) => (*x as f64, *y),
            (Object::Float(x), Object::Integer(y)) => (*x, *y as f64),
            _ => (self.to_number(a)?, self.to_number(b)?),
        };

        Ok(match op {
            NumericCmp::Gt => x > y,
            NumericCmp::Lt => x < y,
            NumericCmp::Ge => x >= y,
            NumericCmp::Le => x <= y,
        })
    }

    pub(crate) fn hash_key_from_object(&self, obj: &Object) -> HashKey {
        match obj {
            Object::String(s) => HashKey::Sym(crate::intern::intern_rc(s)),
            Object::Integer(v) => HashKey::from_int(*v),
            Object::Float(v) => HashKey::from_float(*v),
            Object::Boolean(v) => HashKey::from_bool(*v),
            Object::Null => HashKey::Null,
            Object::Undefined => HashKey::Undefined,
            _ => HashKey::Other(Rc::from(obj.inspect())),
        }
    }

    /// Convert a NaN-boxed Value to a HashKey without full Object conversion.
    #[inline(always)]
    pub(crate) fn hash_key_from_value(&self, val: Value) -> HashKey {
        if val.is_i32() {
            return HashKey::from_int(unsafe { val.as_i32_unchecked() } as i64);
        }
        if val.is_inline_str() {
            // Hot path: inline strings are common Map keys
            let (buf, len) = val.inline_str_buf();
            let s = unsafe { std::str::from_utf8_unchecked(&buf[..len]) };
            return HashKey::Sym(crate::intern::intern(s));
        }
        if val.is_f64() {
            return HashKey::from_float(val.as_f64());
        }
        if val.is_bool() {
            return HashKey::from_bool(unsafe { val.as_bool_unchecked() });
        }
        if val.is_null() {
            return HashKey::Null;
        }
        if val.is_undefined() {
            return HashKey::Undefined;
        }
        if val.is_heap() {
            let obj = self.heap.get(val.heap_index());
            match obj {
                Object::String(s) => HashKey::Sym(crate::intern::intern_rc(s)),
                Object::StringRope(rope) => {
                    // Flatten to temp buffer + intern (immutable path)
                    if rope.total_len <= 64 {
                        let mut buf = [0u8; 64];
                        let len = Self::flatten_rope_to_buf(rope, &self.heap, &mut buf);
                        let s = unsafe { std::str::from_utf8_unchecked(&buf[..len]) };
                        HashKey::Sym(crate::intern::intern(s))
                    } else {
                        let flat = crate::object::flatten_rope(rope, &self.heap);
                        HashKey::Sym(crate::intern::intern(&flat))
                    }
                }
                Object::Symbol(id, _) => HashKey::Other(Rc::from(format!("@@sym:{}", id))),
                other => HashKey::Other(Rc::from(other.inspect())),
            }
        } else {
            HashKey::Undefined
        }
    }

    /// Materialize a stack-buffer-built short string as a heap `Value`,
    /// re-using a previously-cached heap slot when the same sym_id has
    /// been materialized before. The Map benchmark builds keys like
    /// `"key1234"` (7 bytes — too long for inline NaN-box) on every
    /// iteration; without the cache, each one alloc_fast's a fresh
    /// heap entry holding the same canonical `Rc<str>`, so the heap
    /// grows unboundedly and the bump allocator does work-per-iteration.
    /// With the cache, runs 2..N return the prior heap_idx in a single
    /// direct-map lookup.
    #[inline(always)]
    pub(crate) fn intern_short_str_value(
        &mut self,
        s: &str,
        interner_ptr: *mut crate::intern::Interner,
    ) -> Value {
        const CACHE_SIZE: usize = 4096;
        if self.interned_str_value_cache.is_empty() {
            self.interned_str_value_cache = vec![(u32::MAX, 0u64); CACHE_SIZE];
        }
        // Single intern call returns both id and canonical Rc — saves
        // the follow-up `resolve()` thread-local + Vec lookup when the
        // cache misses.
        let (sym, canonical_rc) =
            unsafe { crate::intern::intern_with_ptr_and_rc(interner_ptr, s) };
        let idx = (sym as usize) & (CACHE_SIZE - 1);
        let (cs, cv) = unsafe { *self.interned_str_value_cache.get_unchecked(idx) };
        if cs == sym {
            return Value::from_bits(cv);
        }
        // Miss: materialize once, then cache.
        let v = obj_into_val(Object::String(canonical_rc), &mut self.heap);
        unsafe {
            *self.interned_str_value_cache.get_unchecked_mut(idx) = (sym, v.bits());
        }
        v
    }

    /// Build a `HashKey` for an interpreter Map.set/get/has key.
    ///
    /// Hot path for Map benchmarks where keys are inline-boxed strings
    /// (e.g. "key0".."key999"). Inline-str values have deterministic
    /// NaN-box bits (same content → same bits), so a direct-mapped
    /// `value_bits → sym_id` cache hits 100% on the second-and-later
    /// occurrences of any key. Skips the FxHashMap probe inside
    /// `intern_with_ptr`. djit rejects functions containing
    /// `CallMethod`, so `__bench` for the Map benchmark runs through
    /// the interpreter — this is its dominant cost.
    ///
    /// Heap strings already benefit from `intern_rc`'s `ptr_ids`
    /// fast-path, so they stay on the original
    /// `hash_key_from_value_flatten` path (an experiment to extend the
    /// direct-map cache to them regressed by ~1 ms — the extra branch
    /// + match cost outweighed the marginal hashmap-vs-direct-map win).
    #[inline(always)]
    pub(crate) fn intern_inline_str_key(
        &mut self,
        val: Value,
        interner_ptr: *mut crate::intern::Interner,
    ) -> HashKey {
        if val.is_inline_str() {
            const CACHE_SIZE: usize = 2048;
            if self.inline_sym_cache.is_empty() {
                self.inline_sym_cache = vec![(0u64, u32::MAX); CACHE_SIZE];
            }
            let bits = val.bits();
            let idx = ((bits ^ (bits >> 13)) as usize) & (CACHE_SIZE - 1);
            let (cb, cs) = unsafe { *self.inline_sym_cache.get_unchecked(idx) };
            if cb == bits && cs != u32::MAX {
                return HashKey::Sym(cs);
            }
            let (buf, len) = val.inline_str_buf();
            let s = unsafe { std::str::from_utf8_unchecked(&buf[..len]) };
            let sym = unsafe { crate::intern::intern_with_ptr(interner_ptr, s) };
            unsafe { *self.inline_sym_cache.get_unchecked_mut(idx) = (bits, sym); }
            return HashKey::Sym(sym);
        }
        self.hash_key_from_value_flatten(val)
    }

    /// Like `hash_key_from_value` but eagerly flattens StringRope in-place so
    /// subsequent reads of the same heap slot find a flat String (O(1)).
    #[inline(always)]
    pub(crate) fn hash_key_from_value_flatten(&mut self, val: Value) -> HashKey {
        if !val.is_heap() {
            return self.hash_key_from_value(val);
        }
        let idx = val.heap_index();
        let obj = unsafe { &*self.heap.objects.as_ptr().add(idx as usize) };
        match obj {
            Object::String(s) => HashKey::Sym(crate::intern::intern_rc(s)),
            Object::StringRope(_) => {
                // Flatten in-place: replaces the rope with Object::String on the heap
                let rc = self.heap.flatten_rope_at(idx).unwrap();
                HashKey::Sym(crate::intern::intern_rc(&rc))
            }
            Object::Symbol(id, _) => HashKey::Other(Rc::from(format!("@@sym:{}", *id))),
            other => HashKey::Other(Rc::from(other.inspect())),
        }
    }

    /// Flatten a small rope to a stack buffer. Returns bytes written.
    /// Handles depth-1 ropes (the common case: "key" + "123").
    fn flatten_rope_to_buf(rope: &crate::object::StringRopeNode, heap: &Heap, buf: &mut [u8]) -> usize {
        let mut pos = 0;
        // Simple iterative flatten for small ropes (no work Vec allocation)
        let mut stack: [Value; 8] = [Value::UNDEFINED; 8];
        let mut top = 0;
        stack[top] = rope.left;
        top += 1;
        stack[top] = rope.right;
        top += 1;
        // Process left-to-right (left was pushed first, process it first)
        let mut i = 0;
        while i < top {
            let val = stack[i];
            i += 1;
            if val.is_inline_str() {
                let (b, len) = val.inline_str_buf();
                let end = (pos + len).min(buf.len());
                buf[pos..end].copy_from_slice(&b[..end - pos]);
                pos = end;
            } else if val.is_heap() {
                let obj = heap.get(val.heap_index());
                match obj {
                    Object::String(s) => {
                        let bytes = s.as_bytes();
                        let end = (pos + bytes.len()).min(buf.len());
                        buf[pos..end].copy_from_slice(&bytes[..end - pos]);
                        pos = end;
                    }
                    Object::StringRope(r) if top + 2 <= 8 => {
                        // Push right then left (so left is processed first on next iterations)
                        // But we're iterating forward, so push left first then right
                        stack[top] = r.left;
                        top += 1;
                        stack[top] = r.right;
                        top += 1;
                    }
                    Object::StringRope(r) => {
                        // Too deep, fall back to heap flatten
                        let flat = crate::object::flatten_rope(r, heap);
                        let bytes = flat.as_bytes();
                        let end = (pos + bytes.len()).min(buf.len());
                        buf[pos..end].copy_from_slice(&bytes[..end - pos]);
                        pos = end;
                    }
                    _ => {}
                }
            }
        }
        pos
    }

    pub(crate) fn object_from_hash_key(&self, key: &HashKey) -> Object {
        match key {
            HashKey::Sym(id) => Object::String(crate::intern::resolve(*id)),
            HashKey::Int(v) => Object::Integer(*v),
            HashKey::Float(bits) => Object::Float(f64::from_bits(*bits)),
            HashKey::Bool(v) => Object::Boolean(*v),
            HashKey::Null => Object::Null,
            HashKey::Undefined => Object::Undefined,
            HashKey::Other(s) => Object::String(s.clone()),
        }
    }

    #[inline(always)]
    pub(crate) fn map_insert_or_replace(
        entries: &mut Vec<(HashKey, Value)>,
        indices: &mut crate::object::FlatHashTable,
        key: HashKey,
        value: Value,
    ) {
        // Fast path: combined get_or_insert for Sym keys (single hash + probe)
        if let HashKey::Sym(id) = &key {
            let idx = entries.len();
            if let Some(existing) = indices.get_or_insert_sym(entries, *id, idx) {
                unsafe { entries.get_unchecked_mut(existing).1 = value; }
            } else {
                entries.push((key, value));
            }
            return;
        }
        let found = indices.get(entries, &key);
        if let Some(idx) = found {
            unsafe { entries.get_unchecked_mut(idx).1 = value; }
            return;
        }
        let idx = entries.len();
        entries.push((key.clone(), value));
        indices.insert(entries, &key, idx);
    }

    #[inline(always)]
    pub(crate) fn map_get(
        entries: &[(HashKey, Value)],
        indices: &crate::object::FlatHashTable,
        key: &HashKey,
    ) -> Option<Value> {
        // Fast path: use get_sym for Sym keys
        let found = if let HashKey::Sym(id) = key {
            indices.get_sym(entries, *id)
        } else {
            indices.get(entries, key)
        };
        found.map(|idx| unsafe { entries.get_unchecked(idx).1 })
    }

    pub(crate) fn map_contains(
        entries: &[(HashKey, Value)],
        indices: &crate::object::FlatHashTable,
        key: &HashKey,
    ) -> bool {
        indices.get(entries, key).is_some()
    }

    fn map_remove(
        entries: &mut Vec<(HashKey, Value)>,
        indices: &mut crate::object::FlatHashTable,
        key: &HashKey,
    ) -> Option<Value> {
        let idx = indices.get(entries, key)?;
        indices.remove(entries, idx);
        let removed = entries.remove(idx).1;
        // Rebuild the flat table after removal (entries shifted)
        indices.clear();
        for (i, (k, _)) in entries.iter().enumerate() {
            indices.insert(entries, k, i);
        }
        Some(removed)
    }

    fn set_insert_unique(
        entries: &mut Vec<HashKey>,
        indices: &mut rustc_hash::FxHashMap<HashKey, usize>,
        key: HashKey,
    ) {
        if indices.contains_key(&key) {
            return;
        }
        let idx = entries.len();
        entries.push(key.clone());
        indices.insert(key, idx);
    }

    fn set_contains(indices: &rustc_hash::FxHashMap<HashKey, usize>, key: &HashKey) -> bool {
        indices.contains_key(key)
    }

    fn set_remove(
        entries: &mut Vec<HashKey>,
        indices: &mut rustc_hash::FxHashMap<HashKey, usize>,
        key: &HashKey,
    ) -> bool {
        let Some(idx) = indices.remove(key) else {
            return false;
        };
        entries.remove(idx);
        for i in idx..entries.len() {
            if let Some(k) = entries.get(i) {
                indices.insert(k.clone(), i);
            }
        }
        true
    }

    pub(crate) fn same_value_zero(a: &Object, b: &Object) -> bool {
        match (a, b) {
            (Object::Float(x), Object::Float(y)) => (x.is_nan() && y.is_nan()) || (*x == *y),
            (Object::Integer(x), Object::Integer(y)) => x == y,
            (Object::Integer(x), Object::Float(y)) | (Object::Float(y), Object::Integer(x)) => {
                !y.is_nan() && (*x as f64 == *y)
            }
            (Object::String(x), Object::String(y)) => x == y,
            (Object::Boolean(x), Object::Boolean(y)) => x == y,
            (Object::Null, Object::Null) => true,
            (Object::Undefined, Object::Undefined) => true,
            _ => false,
        }
    }

    pub(crate) fn strict_equal(a: &Object, b: &Object) -> bool {
        match (a, b) {
            (Object::Float(x), Object::Float(y)) => !x.is_nan() && !y.is_nan() && (*x == *y),
            (Object::Integer(x), Object::Integer(y)) => x == y,
            (Object::Integer(x), Object::Float(y)) | (Object::Float(y), Object::Integer(x)) => {
                !y.is_nan() && (*x as f64 == *y)
            }
            (Object::String(x), Object::String(y)) => x == y,
            (Object::Boolean(x), Object::Boolean(y)) => x == y,
            (Object::Null, Object::Null) => true,
            (Object::Undefined, Object::Undefined) => true,
            // Reference identity for Rc-backed heap types (JS === semantics)
            (Object::Array(x), Object::Array(y)) => Rc::ptr_eq(x, y),
            (Object::Hash(x), Object::Hash(y)) => Rc::ptr_eq(x, y),
            (Object::Set(x), Object::Set(y)) => Rc::ptr_eq(&x.entries, &y.entries),
            (Object::Map(x), Object::Map(y)) => Rc::ptr_eq(&x.entries, &y.entries),
            _ => false,
        }
    }

    fn slice_bounds(start: i32, end: i32, len: i32) -> (i32, i32) {
        let norm = |idx: i32| {
            if idx < 0 {
                (len + idx).max(0)
            } else {
                idx.min(len)
            }
        };
        let s = norm(start);
        let e = norm(end);
        if e < s {
            (s, s)
        } else {
            (s, e)
        }
    }

    fn expand_js_replacement(
        template: &str,
        full_match: &str,
        captures: &[Option<String>],
        prefix: &str,
        suffix: &str,
    ) -> String {
        let chars: Vec<char> = template.chars().collect();
        let mut out = String::new();
        let mut i = 0usize;
        while i < chars.len() {
            if chars[i] != '$' {
                out.push(chars[i]);
                i += 1;
                continue;
            }

            if i + 1 >= chars.len() {
                out.push('$');
                i += 1;
                continue;
            }

            let next = chars[i + 1];
            match next {
                '$' => {
                    out.push('$');
                    i += 2;
                }
                '&' => {
                    out.push_str(full_match);
                    i += 2;
                }
                '`' => {
                    out.push_str(prefix);
                    i += 2;
                }
                '\'' => {
                    out.push_str(suffix);
                    i += 2;
                }
                '0'..='9' => {
                    if next == '0' {
                        out.push('$');
                        out.push('0');
                        i += 2;
                        continue;
                    }

                    let d1 = (next as u8 - b'0') as usize;
                    if i + 2 < chars.len() && chars[i + 2].is_ascii_digit() {
                        let d2 = (chars[i + 2] as u8 - b'0') as usize;
                        let idx2 = d1 * 10 + d2;
                        if idx2 > 0 && idx2 <= captures.len() {
                            if let Some(group) = &captures[idx2 - 1] {
                                out.push_str(group);
                            }
                            i += 3;
                            continue;
                        }
                    }

                    if d1 <= captures.len() {
                        if let Some(group) = &captures[d1 - 1] {
                            out.push_str(group);
                        }
                        i += 2;
                    } else {
                        out.push('$');
                        out.push(next);
                        i += 2;
                    }
                }
                _ => {
                    out.push('$');
                    out.push(next);
                    i += 2;
                }
            }
        }

        out
    }

    fn int_to_radix_string(value: i64, radix: u32) -> String {
        const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        if value == 0 {
            return "0".to_string();
        }

        let negative = value < 0;
        let mut n = value.unsigned_abs() as u128;
        let mut buf: Vec<char> = Vec::new();
        while n > 0 {
            let d = (n % radix as u128) as usize;
            buf.push(DIGITS[d] as char);
            n /= radix as u128;
        }
        if negative {
            buf.push('-');
        }
        buf.iter().rev().collect()
    }

    pub(crate) fn get_keys_array(&mut self, source: Object) -> Vec<Value> {
        match source {
            Object::Array(items) => {
                let items = items.borrow();
                let mut out = Vec::with_capacity(items.len());
                for i in 0..items.len() {
                    out.push(obj_into_val(
                        Object::String(i.to_string().into()),
                        &mut self.heap,
                    ));
                }
                out
            }
            Object::String(s) => {
                let mut out = Vec::with_capacity(Self::string_char_len(&s));
                for (i, _) in s.chars().enumerate() {
                    out.push(obj_into_val(
                        Object::String(i.to_string().into()),
                        &mut self.heap,
                    ));
                }
                out
            }
            Object::Hash(hash) => {
                let hash_b = hash.borrow();
                let mut out = Vec::with_capacity(hash_b.pairs.len());
                for key in self.ordered_hash_keys_js(hash_b) {
                    out.push(obj_into_val(
                        Object::String(key.display_key().into()),
                        &mut self.heap,
                    ));
                }
                // Also include getter-defined properties (from Object.defineProperty)
                if let Some(ref getters) = hash_b.getters {
                    for key in getters.keys() {
                        let key_val = obj_into_val(
                            Object::String(Rc::from(key.as_str())),
                            &mut self.heap,
                        );
                        if !out.iter().any(|v| v.bits() == key_val.bits()) {
                            out.push(key_val);
                        }
                    }
                }
                out
            }
            _ => vec![],
        }
    }

    pub(crate) fn ordered_hash_keys_js(&self, hash: &crate::object::HashObject) -> Vec<HashKey> {
        let mut numeric = Vec::<(u32, HashKey)>::new();
        let mut others = Vec::<HashKey>::new();

        for key in hash.ordered_keys_ref() {
            if let Some(v) = key.is_numeric_index() {
                numeric.push((v, key.clone()));
            } else {
                others.push(key.clone());
            }
        }

        numeric.sort_by_key(|(v, _)| *v);
        let mut out = Vec::with_capacity(numeric.len() + others.len());
        out.extend(numeric.into_iter().map(|(_, k)| k));
        out.extend(others);
        out
    }

    fn object_key_cow<'a>(&self, obj: &'a Object) -> Cow<'a, str> {
        match obj {
            Object::String(s) => Cow::Borrowed(s),
            Object::Integer(v) => Cow::Owned(v.to_string()),
            Object::Float(v) if v.fract() == 0.0 => Cow::Owned((*v as i64).to_string()),
            Object::Float(v) => Cow::Owned(v.to_string()),
            Object::Boolean(v) => Cow::Owned(v.to_string()),
            _ => Cow::Owned(obj.inspect()),
        }
    }

    fn object_to_array_index(obj: &Object) -> Option<usize> {
        match obj {
            Object::Integer(v) if *v >= 0 => Some(*v as usize),
            Object::Float(v) if v.is_finite() && v.fract() == 0.0 && *v >= 0.0 => Some(*v as usize),
            Object::String(s) => Self::parse_non_negative_usize(s),
            _ => None,
        }
    }

    fn numeric_array_index(obj: &Object) -> Option<usize> {
        match obj {
            Object::Integer(v) if *v >= 0 => Some(*v as usize),
            Object::Float(v) if v.is_finite() && v.fract() == 0.0 && *v >= 0.0 => Some(*v as usize),
            _ => None,
        }
    }

    #[inline(always)]
    fn parse_non_negative_usize(s: &str) -> Option<usize> {
        if s.is_empty() {
            return None;
        }
        let bytes = s.as_bytes();
        let mut out: usize = 0;
        for b in bytes {
            if !b.is_ascii_digit() {
                return None;
            }
            out = out.checked_mul(10)?;
            out = out.checked_add((b - b'0') as usize)?;
        }
        Some(out)
    }

    #[inline(always)]
    fn string_char_len(s: &str) -> usize {
        if s.is_ascii() {
            s.len()
        } else {
            s.chars().count()
        }
    }

    #[inline(always)]
    fn string_nth_char(s: &str, idx: usize) -> Option<char> {
        if s.is_ascii() {
            s.as_bytes().get(idx).map(|b| *b as char)
        } else {
            s.chars().nth(idx)
        }
    }

    pub(crate) fn op_in(&self, left: &Object, right: &Object) -> bool {
        match right {
            Object::Array(items) => {
                let items = items.borrow();
                match left {
                    Object::String(s) => {
                        if &**s == "length" {
                            return true;
                        }
                        if let Some(idx) = Self::parse_non_negative_usize(s) {
                            return idx < items.len();
                        }
                    }
                    _ => {
                        if let Some(idx) = Self::numeric_array_index(left) {
                            return idx < items.len();
                        }
                    }
                }
                false
            }
            Object::Hash(hash) => {
                let hash_b = hash.borrow();
                match left {
                    Object::String(s) => hash_b.contains_str(s),
                    _ => {
                        let key = self.hash_key_from_object(left);
                        hash_b.pairs.contains_key(&key)
                    }
                }
            }
            Object::Instance(instance) => {
                let k = self.object_key_cow(left);
                instance.fields.contains_key(k.as_ref())
                    || instance.methods.contains_key(k.as_ref())
                    || instance.getters.contains_key(k.as_ref())
                    || instance.setters.contains_key(k.as_ref())
            }
            Object::Class(class_obj) => {
                let k = self.object_key_cow(left);
                class_obj.methods.contains_key(k.as_ref())
                    || class_obj.static_methods.contains_key(k.as_ref())
                    || class_obj.static_fields.contains_key(k.as_ref())
                    || class_obj.getters.contains_key(k.as_ref())
                    || class_obj.setters.contains_key(k.as_ref())
                    || class_obj.super_methods.contains_key(k.as_ref())
                    || class_obj.super_getters.contains_key(k.as_ref())
                    || class_obj.super_setters.contains_key(k.as_ref())
            }
            Object::SuperRef(super_ref) => {
                let k = self.object_key_cow(left);
                super_ref.methods.contains_key(k.as_ref())
                    || super_ref.getters.contains_key(k.as_ref())
                    || super_ref.setters.contains_key(k.as_ref())
            }
            _ => false,
        }
    }

    pub(crate) fn op_instanceof(&self, left: &Object, right: &Object) -> bool {
        match (left, right) {
            (Object::Instance(inst), Object::Class(class_obj)) => {
                inst.class_name == class_obj.name
                    || inst.parent_chain.iter().any(|n| n == &class_obj.name)
            }
            (_, Object::Hash(hash)) if hash.borrow().contains_str("from") => {
                matches!(left, Object::Array(_))
            }
            (_, Object::Hash(hash)) if hash.borrow().contains_str("keys") => {
                matches!(
                    left,
                    Object::Hash(_) | Object::Array(_) | Object::Instance(_)
                )
            }
            _ => false,
        }
    }




    /// Stage call arguments from the stack into `arg_buffer` without popping a
    /// callee.  Used by `OpCallGlobal` where the function is read directly from
    /// the globals array instead of from the stack.

    pub(crate) fn call_value_slice(
        &mut self,
        callee: Value,
        args: &[Value],
    ) -> Result<Value, VMError> {
        // Peek heap to extract lightweight data without full val_to_obj clone
        if callee.is_heap() {
            let heap_obj = self.heap.get(callee.heap_index());
            match heap_obj {
                Object::BuiltinFunction(b) => {
                    // Clone just the fields, skip Box allocation
                    let builtin = BuiltinFunctionObject {
                        function: b.function.clone(),
                        receiver: b.receiver.clone(),
                    };
                    return self.execute_builtin_function_slice(builtin, args);
                }
                Object::CompiledFunction(f) => {
                    if f.is_generator {
                        let func = (**f).clone();
                        return Ok(self.create_generator(func, args.to_vec(), None));
                    }
                    // Clone CompiledFunctionObject without Box allocation
                    let func = (**f).clone();
                    let (result, _) = self.execute_compiled_function_slice(func, args, None)?;
                    return Ok(result);
                }
                // Hash with __call__ or __boundFn: invoke the wrapped function
                Object::Hash(hash_rc) => {
                    let bound_fn_sym = crate::intern::intern("__boundFn");
                    let bound_args_sym = crate::intern::intern("__boundArgs");
                    let hb = hash_rc.borrow();
                    if let Some(fn_val) = hb.get_by_sym(bound_fn_sym) {
                        // .bind() wrapper: prepend bound args, then call args
                        let bound_args = if let Some(arr_val) = hb.get_by_sym(bound_args_sym) {
                            let arr_obj = val_to_obj(arr_val, &self.heap);
                            match arr_obj {
                                Object::Array(a) => a.borrow().to_vec(),
                                _ => vec![],
                            }
                        } else {
                            vec![]
                        };
                        let fn_v = fn_val;
                        let _ = hb;
                        let mut all_args = bound_args;
                        all_args.extend_from_slice(args);
                        return self.call_value_slice(fn_v, &all_args);
                    }
                    let call_sym = crate::intern::intern("__call__");
                    if let Some(call_val) = hb.get_by_sym(call_sym) {
                        let cv = call_val;
                        let _ = hb;
                        return self.call_value_slice(cv, args);
                    }
                }
                _ => {}
            }
        }
        // Fall through for BoundMethod, SuperRef (need ownership for destructure)
        let callee_obj = val_to_obj(callee, &self.heap);
        match callee_obj {
            Object::BoundMethod(bound) => {
                // Check for .bind() pattern: receiver has __boundArgs
                let bound_args_sym = crate::intern::intern("__boundArgs");
                let prepend_args = match &*bound.receiver {
                    Object::Hash(h) => {
                        h.borrow().get_by_sym(bound_args_sym).and_then(|v| {
                            match val_to_obj(v, &self.heap) {
                                Object::Array(a) => Some(a.borrow().to_vec()),
                                _ => None,
                            }
                        })
                    }
                    _ => None,
                };
                if let Some(mut pre_args) = prepend_args {
                    // .bind() wrapper: prepend bound args, then call args
                    pre_args.extend_from_slice(args);
                    let (result, _) = self.execute_compiled_function_slice(
                        bound.function, &pre_args, None)?;
                    return Ok(result);
                }
                if bound.function.is_generator {
                    let receiver_val = obj_into_val(*bound.receiver, &mut self.heap);
                    return Ok(self.create_generator(
                        bound.function,
                        args.to_vec(),
                        Some(receiver_val),
                    ));
                }
                let receiver_val = obj_into_val(*bound.receiver, &mut self.heap);
                let (result, _) =
                    self.execute_compiled_function_slice(bound.function, args, Some(receiver_val))?;
                Ok(result)
            }
            Object::SuperRef(super_ref) => {
                let SuperRefObject {
                    mut receiver,
                    mut methods,
                    constructor_chain,
                    ..
                } = *super_ref;
                if let Some(ctor) = methods.remove("constructor") {
                    // Save original super info so we can restore after the parent
                    // constructor returns (needed for super.method() calls later).
                    let (saved_sm, saved_sg, saved_ss, saved_chain) =
                        if let Object::Instance(inst) = &*receiver {
                            (
                                inst.super_methods.clone(),
                                inst.super_getters.clone(),
                                inst.super_setters.clone(),
                                inst.super_constructor_chain.clone(),
                            )
                        } else {
                            (
                                rustc_hash::FxHashMap::default(),
                                rustc_hash::FxHashMap::default(),
                                rustc_hash::FxHashMap::default(),
                                vec![],
                            )
                        };

                    // Shift the super chain so that nested super() calls inside
                    // the parent constructor resolve to the next ancestor.
                    if let Object::Instance(inst) = &mut *receiver {
                        if let Some((next_methods, next_getters, next_setters)) =
                            constructor_chain.first()
                        {
                            inst.super_methods = next_methods.clone();
                            inst.super_getters = next_getters.clone();
                            inst.super_setters = next_setters.clone();
                            inst.super_constructor_chain =
                                constructor_chain[1..].to_vec();
                        } else {
                            inst.super_methods.clear();
                            inst.super_getters.clear();
                            inst.super_setters.clear();
                            inst.super_constructor_chain.clear();
                        }
                    }
                    let receiver_val = obj_into_val(*receiver, &mut self.heap);
                    let (result, receiver_after) =
                        self.execute_compiled_function_slice(ctor, args, Some(receiver_val))?;

                    // Restore original super info on the returned instance so that
                    // super.method() calls in the derived class work correctly.
                    let final_val = receiver_after.unwrap_or(result);
                    if final_val.is_heap() {
                        let heap_idx = final_val.heap_index() as usize;
                        if let Some(Object::Instance(inst)) =
                            self.heap.objects.get_mut(heap_idx)
                        {
                            inst.super_methods = saved_sm;
                            inst.super_getters = saved_sg;
                            inst.super_setters = saved_ss;
                            inst.super_constructor_chain = saved_chain;
                        }
                    }
                    Ok(final_val)
                } else {
                    Err(VMError::TypeError(
                        "super constructor not found".to_string(),
                    ))
                }
            }
            _other => {
                Ok(Value::UNDEFINED)
            }
        }
    }

    /// Returns the maximum number of positional arguments the callback will access.
    /// For compiled functions without rest params, this is num_parameters.
    /// For everything else, returns a conservative MAX to ensure all args are passed.
    fn callback_max_used_args(callback: &Object) -> usize {
        match callback {
            Object::CompiledFunction(f) if f.rest_parameter_index.is_none() => f.num_parameters,
            Object::BoundMethod(b) if b.function.rest_parameter_index.is_none() => {
                b.function.num_parameters
            }
            _ => usize::MAX,
        }
    }

    fn callback_max_used_args_val(callback: Value, heap: &Heap) -> usize {
        if callback.is_heap() {
            return Self::callback_max_used_args(heap.get(callback.heap_index()));
        }
        usize::MAX
    }

    fn call_value2(&mut self, callee: Value, a: Value, b: Value) -> Result<Value, VMError> {
        let args = [a, b];
        self.call_value_slice(callee, &args)
    }

    fn call_value3(
        &mut self,
        callee: Value,
        a: Value,
        b: Value,
        c: Value,
    ) -> Result<Value, VMError> {
        let args = [a, b, c];
        self.call_value_slice(callee, &args)
    }

    fn call_value4(
        &mut self,
        callee: Value,
        a: Value,
        b: Value,
        c: Value,
        d: Value,
    ) -> Result<Value, VMError> {
        let args = [a, b, c, d];
        self.call_value_slice(callee, &args)
    }

    fn format_to_precision(n: f64, precision: usize) -> String {
        if !n.is_finite() {
            return if n.is_nan() {
                "NaN".to_string()
            } else if n > 0.0 {
                "Infinity".to_string()
            } else {
                "-Infinity".to_string()
            };
        }
        if n == 0.0 {
            if precision <= 1 {
                return "0".to_string();
            }
            return format!("0.{}", "0".repeat(precision - 1));
        }
        let abs = n.abs();
        let exp = abs.log10().floor() as i32;
        // If exponent is within reasonable range, use fixed notation
        if exp >= 0 && (exp as usize) < precision {
            let decimal_places = precision - 1 - exp as usize;
            let formatted = format!("{:.*}", decimal_places, n);
            return formatted;
        }
        if (-4..0).contains(&exp) {
            let decimal_places = precision as i32 - 1 - exp;
            let formatted = format!("{:.*}", decimal_places as usize, n);
            return formatted;
        }
        // Use exponential notation
        let mantissa_digits = precision - 1;
        let formatted = format!("{:.*e}", mantissa_digits, n);
        // JavaScript uses e+N format
        if let Some(pos) = formatted.find('e') {
            let (mantissa, exp_part) = formatted.split_at(pos);
            let exp_str = &exp_part[1..];
            let exp_val: i32 = exp_str.parse().unwrap_or(0);
            if exp_val >= 0 {
                format!("{}e+{}", mantissa, exp_val)
            } else {
                format!("{}e{}", mantissa, exp_val)
            }
        } else {
            formatted
        }
    }

    fn deep_clone_value(&mut self, val: Value) -> Result<Value, VMError> {
        self.deep_clone_value_depth(val, 0)
    }

    /// Depth-bounded recursive clone. Without this, a cyclic object
    /// like `let a = {}; a.self = a; structuredClone(a)` recurses
    /// through `deep_clone_object` → `deep_clone_value` forever
    /// and stack-overflows the *host* process. `MAX_CLONE_DEPTH = 200`
    /// matches the `JSON.stringify` depth cap from R3.
    fn deep_clone_value_depth(&mut self, val: Value, depth: usize) -> Result<Value, VMError> {
        const MAX_CLONE_DEPTH: usize = 200;
        if depth > MAX_CLONE_DEPTH {
            return Err(VMError::TypeError(format!(
                "structuredClone: depth exceeded {} levels (possible cyclic structure)",
                MAX_CLONE_DEPTH
            )));
        }
        if !val.is_heap() {
            return Ok(val); // primitives are already cloned by value
        }
        let obj = val_to_obj(val, &self.heap);
        let cloned_obj = self.deep_clone_object_depth(obj, depth + 1)?;
        Ok(obj_into_val(cloned_obj, &mut self.heap))
    }

    fn deep_clone_object_depth(
        &mut self,
        obj: Object,
        depth: usize,
    ) -> Result<Object, VMError> {
        match obj {
            Object::Array(items) => {
                let borrowed = items.borrow();
                let mut new_items = Vec::with_capacity(borrowed.len());
                for &v in borrowed.iter() {
                    new_items.push(self.deep_clone_value_depth(v, depth)?);
                }
                Ok(make_array(new_items))
            }
            Object::Hash(hash) => {
                let h = hash.borrow();
                let mut new_hash = HashObject::default();
                for (k, &v) in h.pairs.iter() {
                    let cloned_v = self.deep_clone_value_depth(v, depth)?;
                    new_hash.insert_pair(k.clone(), cloned_v);
                }
                Ok(make_hash(new_hash))
            }
            // Primitives and other types: return as-is
            other => Ok(other),
        }
    }

    // ── Host call queue (softn.* bridge) ──────────────────────────────

    /// Upper bound on the combined UTF-8 byte length of
    /// [`PendingHostCall::args`]. A script can't otherwise back-pressure
    /// the host — `host.call(kind, args, cb)` succeeds synchronously and
    /// sits in the queue until the host drains it, so a hostile script
    /// could queue an unbounded number of huge-arg payloads and bloat
    /// the host's memory before a single tick of real work.
    ///
    /// 16 MiB matches `max_heap_bytes` default (64 MiB) to within a
    /// factor of four and is far larger than any realistic host-call
    /// payload.
    const MAX_HOST_CALL_ARGS_BYTES: usize = 16 * 1024 * 1024;

    /// Maximum number of host calls that can sit in the queue
    /// between host drains. A script running
    /// `for(;;) host.call("x", [], undefined)` would otherwise
    /// balloon the host's memory before wall-time caught it.
    const MAX_PENDING_HOST_CALLS: usize = 100_000;

    /// Queue an async host call and store the callback for later
    /// resolution. Fails when the combined `args` payload exceeds
    /// [`Self::MAX_HOST_CALL_ARGS_BYTES`] or when the queue itself
    /// exceeds [`Self::MAX_PENDING_HOST_CALLS`] — either way a
    /// hostile script can't grow the host's pending-call queue
    /// without bound.
    fn queue_host_call(
        &mut self,
        kind: &str,
        args: Vec<String>,
        callback: Value,
    ) -> Result<(), VMError> {
        let total: usize = args.iter().map(|s| s.len()).sum();
        if total > Self::MAX_HOST_CALL_ARGS_BYTES {
            return Err(VMError::ExecutionTimeout(format!(
                "Host-call argument payload {}B exceeds limit {}B",
                total,
                Self::MAX_HOST_CALL_ARGS_BYTES
            )));
        }
        if self.pending_host_calls.len() >= Self::MAX_PENDING_HOST_CALLS {
            return Err(VMError::ExecutionTimeout(format!(
                "Host-call queue length {} exceeds limit {}",
                self.pending_host_calls.len(),
                Self::MAX_PENDING_HOST_CALLS
            )));
        }
        let id = self.next_host_call_id;
        self.next_host_call_id += 1;
        if callback != Value::UNDEFINED {
            self.host_callbacks.insert(id, callback);
        }
        self.pending_host_calls.push(crate::host_bridge::PendingHostCall {
            id,
            kind: kind.to_string(),
            args,
        });
        Ok(())
    }

    // ── Bridge helper methods ────────────────────────────────────────

    /// Extract a `Vec<f64>` from a Value that should be an array.
    fn extract_f64_vec(&self, val: Option<Value>) -> Vec<f64> {
        if let Some(v) = val {
            if v.is_heap() {
                let obj = self.heap.get(v.heap_index());
                if let Object::Array(ref arr) = obj {
                    let items = arr.borrow();
                    return items.iter().map(|item| item.to_number()).collect();
                }
            }
        }
        Vec::new()
    }

    /// Extract a `Vec<u64>` from a Value that should be an array of numbers.
    fn extract_u64_vec(&self, val: Option<Value>) -> Vec<u64> {
        if let Some(v) = val {
            if v.is_heap() {
                let obj = self.heap.get(v.heap_index());
                if let Object::Array(ref arr) = obj {
                    let items = arr.borrow();
                    return items.iter().map(|item| item.to_number() as u64).collect();
                }
            }
        }
        Vec::new()
    }

    /// Extract a `LayoutStyle` from a Value that should be a JS object (hash).
    /// Properties map to the LayoutStyle struct fields.
    fn extract_layout_style(&self, val: Option<Value>) -> crate::layout_bridge::LayoutStyle {
        use crate::layout_bridge::*;
        let mut style = LayoutStyle::default();

        let hash = match val {
            Some(v) if v.is_heap() => {
                let obj = self.heap.get(v.heap_index());
                if let Object::Hash(ref h) = obj {
                    h.borrow()
                } else {
                    return style;
                }
            }
            _ => return style,
        };

        // Helper closures to read properties from the hash
        let get_str = |key: &str| -> Option<String> {
            hash.get_by_str(key).map(|v| val_inspect(v, &self.heap))
        };
        let get_f64 = |key: &str| -> Option<f64> { hash.get_by_str(key).map(|v| v.to_number()) };
        let get_f64_or = |key: &str, default: f64| -> f64 {
            hash.get_by_str(key)
                .map(|v| v.to_number())
                .unwrap_or(default)
        };

        // Display
        if let Some(d) = get_str("display") {
            style.display = match d.as_str() {
                "flex" => LayoutDisplay::Flex,
                "grid" => LayoutDisplay::Grid,
                "none" => LayoutDisplay::None,
                _ => LayoutDisplay::Flex,
            };
        }

        // Position
        if let Some(p) = get_str("position") {
            style.position = match p.as_str() {
                "relative" => LayoutPosition::Relative,
                "absolute" => LayoutPosition::Absolute,
                "fixed" => LayoutPosition::Fixed,
                "sticky" => LayoutPosition::Sticky,
                _ => LayoutPosition::Relative,
            };
        }

        // Overflow
        if let Some(o) = get_str("overflow") {
            style.overflow = match o.as_str() {
                "visible" => LayoutOverflow::Visible,
                "hidden" => LayoutOverflow::Hidden,
                "scroll" => LayoutOverflow::Scroll,
                _ => LayoutOverflow::Visible,
            };
        }

        // Flex container
        if let Some(fd) = get_str("flexDirection") {
            style.flex_direction = match fd.as_str() {
                "row" => FlexDirection::Row,
                "column" => FlexDirection::Column,
                "row-reverse" => FlexDirection::RowReverse,
                "column-reverse" => FlexDirection::ColumnReverse,
                _ => FlexDirection::Column,
            };
        }
        if let Some(fw) = get_str("flexWrap") {
            style.flex_wrap = match fw.as_str() {
                "nowrap" => FlexWrap::NoWrap,
                "wrap" => FlexWrap::Wrap,
                "wrap-reverse" => FlexWrap::WrapReverse,
                _ => FlexWrap::NoWrap,
            };
        }
        if let Some(jc) = get_str("justifyContent") {
            style.justify_content = match jc.as_str() {
                "flex-start" | "start" => JustifyContent::FlexStart,
                "flex-end" | "end" => JustifyContent::FlexEnd,
                "center" => JustifyContent::Center,
                "space-between" => JustifyContent::SpaceBetween,
                "space-around" => JustifyContent::SpaceAround,
                "space-evenly" => JustifyContent::SpaceEvenly,
                _ => JustifyContent::FlexStart,
            };
        }
        if let Some(ai) = get_str("alignItems") {
            style.align_items = match ai.as_str() {
                "flex-start" | "start" => AlignItems::FlexStart,
                "flex-end" | "end" => AlignItems::FlexEnd,
                "center" => AlignItems::Center,
                "baseline" => AlignItems::Baseline,
                "stretch" => AlignItems::Stretch,
                _ => AlignItems::Stretch,
            };
        }
        if let Some(ac) = get_str("alignContent") {
            style.align_content = match ac.as_str() {
                "flex-start" | "start" => AlignContent::FlexStart,
                "flex-end" | "end" => AlignContent::FlexEnd,
                "center" => AlignContent::Center,
                "stretch" => AlignContent::Stretch,
                "space-between" => AlignContent::SpaceBetween,
                "space-around" => AlignContent::SpaceAround,
                _ => AlignContent::Stretch,
            };
        }
        style.gap_row = get_f64_or("rowGap", 0.0);
        style.gap_column = get_f64_or("columnGap", 0.0);
        // shorthand: gap sets both
        if let Some(gap) = get_f64("gap") {
            if style.gap_row == 0.0 {
                style.gap_row = gap;
            }
            if style.gap_column == 0.0 {
                style.gap_column = gap;
            }
        }

        // Flex item
        style.flex_grow = get_f64_or("flexGrow", 0.0);
        style.flex_shrink = get_f64_or("flexShrink", 1.0);
        if let Some(fb) = hash.get_by_str("flexBasis") {
            style.flex_basis = self.parse_dimension(fb);
        }
        if let Some(als) = get_str("alignSelf") {
            style.align_self = match als.as_str() {
                "auto" => AlignSelf::Auto,
                "flex-start" | "start" => AlignSelf::FlexStart,
                "flex-end" | "end" => AlignSelf::FlexEnd,
                "center" => AlignSelf::Center,
                "baseline" => AlignSelf::Baseline,
                "stretch" => AlignSelf::Stretch,
                _ => AlignSelf::Auto,
            };
        }

        // Size
        if let Some(v) = hash.get_by_str("width") {
            style.width = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("height") {
            style.height = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("minWidth") {
            style.min_width = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("minHeight") {
            style.min_height = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("maxWidth") {
            style.max_width = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("maxHeight") {
            style.max_height = self.parse_dimension(v);
        }

        // Padding (number or [top, right, bottom, left])
        if let Some(v) = hash.get_by_str("padding") {
            style.padding = self.extract_spacing_f64(v);
        }
        if let Some(v) = hash.get_by_str("paddingTop") {
            style.padding[0] = v.to_number();
        }
        if let Some(v) = hash.get_by_str("paddingRight") {
            style.padding[1] = v.to_number();
        }
        if let Some(v) = hash.get_by_str("paddingBottom") {
            style.padding[2] = v.to_number();
        }
        if let Some(v) = hash.get_by_str("paddingLeft") {
            style.padding[3] = v.to_number();
        }

        // Margin (dimension or [top, right, bottom, left])
        if let Some(v) = hash.get_by_str("margin") {
            style.margin = self.extract_spacing_dim(v);
        }
        if let Some(v) = hash.get_by_str("marginTop") {
            style.margin[0] = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("marginRight") {
            style.margin[1] = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("marginBottom") {
            style.margin[2] = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("marginLeft") {
            style.margin[3] = self.parse_dimension(v);
        }

        // Border widths
        if let Some(v) = hash.get_by_str("borderWidth") {
            style.border = self.extract_spacing_f64(v);
        }
        if let Some(v) = hash.get_by_str("borderTopWidth") {
            style.border[0] = v.to_number();
        }
        if let Some(v) = hash.get_by_str("borderRightWidth") {
            style.border[1] = v.to_number();
        }
        if let Some(v) = hash.get_by_str("borderBottomWidth") {
            style.border[2] = v.to_number();
        }
        if let Some(v) = hash.get_by_str("borderLeftWidth") {
            style.border[3] = v.to_number();
        }

        // Inset (for absolute positioning)
        if let Some(v) = hash.get_by_str("top") {
            style.inset[0] = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("right") {
            style.inset[1] = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("bottom") {
            style.inset[2] = self.parse_dimension(v);
        }
        if let Some(v) = hash.get_by_str("left") {
            style.inset[3] = self.parse_dimension(v);
        }

        // Aspect ratio
        if let Some(v) = get_f64("aspectRatio") {
            style.aspect_ratio = Some(v);
        }

        // z-index
        if let Some(v) = get_f64("zIndex") {
            style.z_index = v as i32;
        }

        // order
        if let Some(v) = get_f64("order") {
            style.order = v as i32;
        }

        // Grid (basic) — grid_template_columns/rows as arrays of track strings
        // TODO: grid support can be expanded later

        style
    }

    /// Parse a Value as a Dimension: number → Points, "50%" → Percent, "auto" → Auto.
    fn parse_dimension(&self, val: Value) -> crate::layout_bridge::Dimension {
        use crate::layout_bridge::Dimension;
        if val.is_i32() || val.is_f64() {
            return Dimension::Points(val.to_number());
        }
        let s = val_inspect(val, &self.heap);
        if s == "auto" {
            Dimension::Auto
        } else if let Some(pct) = s.strip_suffix('%') {
            pct.parse::<f64>()
                .map(|v| Dimension::Percent(v / 100.0))
                .unwrap_or(Dimension::Auto)
        } else if let Ok(v) = s.parse::<f64>() {
            Dimension::Points(v)
        } else {
            Dimension::Auto
        }
    }

    /// Extract [top, right, bottom, left] f64 from a Value.
    /// If it's a single number, applies to all 4 sides.
    /// If it's an array, extracts up to 4 elements (CSS shorthand style).
    fn extract_spacing_f64(&self, val: Value) -> [f64; 4] {
        if val.is_i32() || val.is_f64() {
            let v = val.to_number();
            return [v, v, v, v];
        }
        if val.is_heap() {
            let obj = self.heap.get(val.heap_index());
            if let Object::Array(ref arr) = obj {
                let items = arr.borrow();
                return match items.len() {
                    0 => [0.0; 4],
                    1 => {
                        let v = items[0].to_number();
                        [v, v, v, v]
                    }
                    2 => {
                        let vert = items[0].to_number();
                        let horiz = items[1].to_number();
                        [vert, horiz, vert, horiz]
                    }
                    3 => {
                        let top = items[0].to_number();
                        let horiz = items[1].to_number();
                        let bottom = items[2].to_number();
                        [top, horiz, bottom, horiz]
                    }
                    _ => [
                        items[0].to_number(),
                        items[1].to_number(),
                        items[2].to_number(),
                        items[3].to_number(),
                    ],
                };
            }
        }
        [0.0; 4]
    }

    /// Extract [top, right, bottom, left] Dimension from a Value.
    fn extract_spacing_dim(&self, val: Value) -> [crate::layout_bridge::Dimension; 4] {
        use crate::layout_bridge::Dimension;
        if val.is_i32() || val.is_f64() {
            let d = Dimension::Points(val.to_number());
            return [d, d, d, d];
        }
        if val.is_heap() {
            let obj = self.heap.get(val.heap_index());
            if let Object::Array(ref arr) = obj {
                let items = arr.borrow();
                let parse = |i: usize| -> Dimension {
                    if let Some(&v) = items.get(i) {
                        self.parse_dimension(v)
                    } else {
                        Dimension::Auto
                    }
                };
                return match items.len() {
                    0 => [
                        Dimension::Auto,
                        Dimension::Auto,
                        Dimension::Auto,
                        Dimension::Auto,
                    ],
                    1 => {
                        let d = parse(0);
                        [d, d, d, d]
                    }
                    2 => {
                        let vert = parse(0);
                        let horiz = parse(1);
                        [vert, horiz, vert, horiz]
                    }
                    3 => {
                        let top = parse(0);
                        let horiz = parse(1);
                        let bottom = parse(2);
                        [top, horiz, bottom, horiz]
                    }
                    _ => [parse(0), parse(1), parse(2), parse(3)],
                };
            }
        }
        [
            Dimension::Auto,
            Dimension::Auto,
            Dimension::Auto,
            Dimension::Auto,
        ]
    }

    fn json_parse(&mut self, source: &str) -> Result<Object, VMError> {
        let parsed: JsonValue = serde_json::from_str(source)
            .map_err(|e| VMError::TypeError(format!("JSON.parse error: {}", e)))?;
        Ok(self.json_value_to_object(parsed))
    }

    fn object_to_json_value(&self, value: &Object) -> JsonValue {
        self.object_to_json_value_inner(value, 0)
    }

    /// Depth-bounded conversion. Recursion happens only on `Array` /
    /// `Hash`; every other variant is a leaf. A cyclic object like
    /// `let a = {}; a.self = a; JSON.stringify(a)` would otherwise
    /// recurse forever and crash the host process via Rust stack
    /// overflow. `MAX_JSON_DEPTH = 200` matches V8's practical
    /// nesting tolerance; on overflow we collapse to `null`, the
    /// same fallback the `_` arm already uses for non-JSON types.
    fn object_to_json_value_inner(&self, value: &Object, depth: usize) -> JsonValue {
        const MAX_JSON_DEPTH: usize = 200;
        if depth > MAX_JSON_DEPTH {
            return JsonValue::Null;
        }
        match value {
            Object::Null | Object::Undefined => JsonValue::Null,
            Object::Boolean(v) => JsonValue::Bool(*v),
            Object::Integer(v) => JsonValue::from(*v),
            Object::Float(v) => {
                serde_json::Number::from_f64(*v).map_or(JsonValue::Null, JsonValue::Number)
            }
            Object::String(s) => JsonValue::String(s.to_string()),
            Object::Array(items) => JsonValue::Array(
                items
                    .borrow()
                    .iter()
                    .map(|item| {
                        let obj = val_to_obj(*item, &self.heap);
                        self.object_to_json_value_inner(&obj, depth + 1)
                    })
                    .collect(),
            ),
            Object::Hash(hash) => {
                let hash_b = unsafe { hash.borrow_mut() };
                hash_b.sync_pairs_if_dirty();
                let mut map = serde_json::Map::new();
                for k in hash_b.ordered_keys_ref() {
                    let v = hash_b.pairs.get(&k).expect("hash key_order out of sync");
                    let obj = val_to_obj(*v, &self.heap);
                    let key = k.display_key();
                    map.insert(key, self.object_to_json_value_inner(&obj, depth + 1));
                }
                JsonValue::Object(map)
            }
            _ => JsonValue::Null,
        }
    }

    fn json_value_to_object(&mut self, value: JsonValue) -> Object {
        match value {
            JsonValue::Null => Object::Null,
            JsonValue::Bool(v) => Object::Boolean(v),
            JsonValue::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Object::Integer(i)
                } else {
                    Object::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            JsonValue::String(s) => Object::String(s.into()),
            JsonValue::Array(arr) => make_array(
                arr.into_iter()
                    .map(|v| {
                        let obj = self.json_value_to_object(v);
                        obj_into_val(obj, &mut self.heap)
                    })
                    .collect(),
            ),
            JsonValue::Object(obj) => {
                let mut hash = crate::object::HashObject::default();
                for (k, v) in obj {
                    let child = self.json_value_to_object(v);
                    let val = obj_into_val(child, &mut self.heap);
                    hash.insert_pair(HashKey::from_owned_string(k), val);
                }
                make_hash(hash)
            }
        }
    }

    /// Convert a DbRecord into a VM HashObject matching the web XDB format:
    /// `{ id, collection, data: { ...fields }, created_at, updated_at }`
    fn db_record_to_object(&mut self, record: crate::db_bridge::DbRecord) -> Object {
        let mut hash = crate::object::HashObject::default();
        let id_val = obj_into_val(Object::String(record.id.into()), &mut self.heap);
        hash.insert_pair(HashKey::from_string("id"), id_val);
        let coll_val = obj_into_val(Object::String(record.collection.into()), &mut self.heap);
        hash.insert_pair(HashKey::from_string("collection"), coll_val);
        let created_val = obj_into_val(Object::String(record.created_at.into()), &mut self.heap);
        hash.insert_pair(HashKey::from_string("created_at"), created_val);
        let updated_val = obj_into_val(Object::String(record.updated_at.into()), &mut self.heap);
        hash.insert_pair(HashKey::from_string("updated_at"), updated_val);
        // Parse data and wrap in a `.data` property (matching web XDB record format).
        // Prefer pre-parsed data_parsed (avoids redundant JSON string round-trip).
        let data_json_val = if let Some(parsed) = record.data_parsed {
            Some(parsed)
        } else if !record.data.is_empty() {
            serde_json::from_str::<JsonValue>(&record.data).ok()
        } else {
            None
        };
        if let Some(parsed) = data_json_val {
            let data_obj = self.json_value_to_object(parsed);
            let data_val = obj_into_val(data_obj, &mut self.heap);
            hash.insert_pair(HashKey::from_string("data"), data_val);
        } else {
            let empty = make_hash(crate::object::HashObject::default());
            let data_val = obj_into_val(empty, &mut self.heap);
            hash.insert_pair(HashKey::from_string("data"), data_val);
        }
        make_hash(hash)
    }

    fn build_regex(&self, pattern: &str, flags: &str) -> Result<regex::Regex, VMError> {
        // `RegexBuilder::build()` is synchronous and can spin for
        // ~O(pattern_len^2) on certain adversarial expansions like
        // `"a?".repeat(50_000) + "a".repeat(50_000)` — the `size_limit`
        // catches the compiled NFA/DFA size but not compile-time CPU.
        // Rejecting patterns above a conservative source-length ceiling
        // bounds that CPU before `build()` is even entered.
        const MAX_REGEX_PATTERN_LEN: usize = 4096;
        if pattern.len() > MAX_REGEX_PATTERN_LEN {
            return Err(VMError::TypeError(format!(
                "regex pattern length {} exceeds MAX_REGEX_PATTERN_LEN ({})",
                pattern.len(),
                MAX_REGEX_PATTERN_LEN
            )));
        }
        let mut builder = RegexBuilder::new(pattern);
        // Cap compiled regex size so a hostile pattern can't burn the host's
        // wall-time budget inside `build()` before the VM quota check can
        // fire. The `regex` crate defaults to 10 MiB / 2 MiB; bringing both
        // down to 1 MiB is still comfortably more than any realistic pattern
        // needs and fails fast on adversarial expansions.
        builder.size_limit(1 << 20);
        builder.dfa_size_limit(1 << 20);
        for flag in flags.chars() {
            match flag {
                'i' => {
                    builder.case_insensitive(true);
                }
                'm' => {
                    builder.multi_line(true);
                }
                's' => {
                    builder.dot_matches_new_line(true);
                }
                // 'g' (global) — matchAll/replaceAll wrap this elsewhere.
                // 'u' (unicode) — Rust `regex` is unicode-aware by default.
                // 'y' (sticky) — caller-side state, not a build-time switch.
                // 'v' (unicodeSets) — superset of 'u' for now; same engine.
                'u' | 'g' | 'y' | 'v' => {}
                _ => {
                    return Err(VMError::TypeError(format!(
                        "unsupported regex flag '{}'",
                        flag
                    )))
                }
            }
        }

        builder.build().map_err(|e| {
            // Friendlier diagnostic for the most common "JS works in V8"
            // patterns the Rust `regex` crate intentionally rejects.
            let msg = e.to_string();
            if msg.contains("look-around") {
                VMError::TypeError(
                    "regex lookahead/lookbehind not supported by this engine \
                     (regex crate uses linear-time matching)"
                        .to_string(),
                )
            } else {
                VMError::TypeError(format!("invalid regex: {}", e))
            }
        })
    }

    /// Push a call frame for a compiled function, set up the new function's
    /// state, and return. The caller should `continue` the dispatch loop.
    /// Args must be pre-staged in `self.arg_buffer` via `stage_call_args`.
    /// The return address (ip after OpCall) is saved in the frame.

    /// Like `push_call_frame`, but reads args directly from the VM stack
    /// instead of from `self.arg_buffer`. This eliminates the intermediate
    /// staging step (stack → arg_buffer → locals becomes stack → locals).
    /// Used by `OpCallGlobal` for non-async, non-rest-parameter calls.
    ///
    /// Takes the function by reference to avoid cloning the entire
    /// `CompiledFunctionObject` upfront. Only clones the `Rc` fields that
    /// actually need separate ownership (instructions, constants, and
    /// optionally inline_cache).

    /// Fast path for register→register function calls. Copies Values directly
    /// from caller registers into callee register window without any Object
    /// conversion. Returns the function result as a Value.
    #[allow(clippy::too_many_arguments)]
    ///
    /// `arg_stack_start` — absolute stack index of the first arg Value.
    /// `nargs` — number of arguments.
    /// `receiver_val` — optional `this` value (already a Value).
    ///
    /// # Safety
    /// - `instr_ptr` must point to valid bytecode of length `instr_len`.
    /// - `constants_raw` must point to a valid `Vec<Object>`.
    /// - `func_cache` must point to a valid `VmCell<Vec<(u32,u32)>>` on the heap.
    pub unsafe fn call_register_direct(
        &mut self,
        instr_ptr: *const u8,
        instr_len: usize,
        constants_raw: *const Vec<Object>,
        rest_parameter_index: Option<usize>,
        takes_this: bool,
        is_async: bool,
        num_cache_slots: u16,
        max_stack_depth: u16,
        register_count: u16,
        func_cache: *const crate::object::VmCell<Vec<(u32, u32)>>,
        arg_stack_start: usize,
        nargs: usize,
        receiver_val: Option<Value>,
    ) -> Result<Value, VMError> {
        // Set up register window (callee registers are above the caller's sp)
        let reg_base = self.sp;
        let reg_window = (register_count as usize).max(1);

        if reg_base + reg_window > STACK_SIZE {
            return Err(VMError::StackOverflow);
        }

        // ── Self-recursion fast path ─────────────────────────────────
        // When a function calls itself, inst_ptr/inst_len/constants/cache
        // are all identical — skip their save/restore entirely.
        // Also shares the inline cache with the recursive call for better
        // hit rates (the function's VmCell cache was already emptied on
        // the initial call entry).
        let is_self_call =
            instr_ptr == self.inst_ptr && !is_async && rest_parameter_index.is_none();
        if is_self_call {
            let saved_ip = self.ip;
            let saved_sp = self.sp;

            self.ip = 0;

            // Ensure stack fits
            let needed = reg_base + reg_window;
            if self.stack.len() < needed {
                self.stack.resize(needed, Value::UNDEFINED);
            }

            let arg_offset = if takes_this { 1 } else { 0 };
            if takes_this {
                unsafe {
                    *self.stack.get_unchecked_mut(reg_base) =
                        receiver_val.unwrap_or(Value::UNDEFINED)
                };
            }

            // Copy args directly via ptr::copy_nonoverlapping. Safe because
            // reg_base = self.sp >= arg_stack_start + nargs (non-overlapping).
            if nargs > 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.stack.as_ptr().add(arg_stack_start),
                        self.stack.as_mut_ptr().add(reg_base + arg_offset),
                        nargs,
                    );
                }
            }
            let first_uninit = nargs + arg_offset;
            for i in first_uninit..reg_window {
                unsafe { *self.stack.get_unchecked_mut(reg_base + i) = Value::UNDEFINED };
            }

            self.sp = reg_base + reg_window;
            let entry_depth = self.rframes.len();
            let run_result = self.rdispatch_loop(entry_depth, reg_base);
            let rv = self.last_popped.take().unwrap_or(Value::UNDEFINED);

            self.ip = saved_ip;
            self.sp = saved_sp;

            run_result?;
            return Ok(rv);
        }

        // ── Normal call path ─────────────────────────────────────────
        // Args stay in place at arg_stack_start — we'll copy directly to the
        // callee's register window after resize (non-overlapping regions).

        // Save current VM state — raw pointer swaps, zero Rc clones
        let saved_ip = self.ip;
        let saved_inst_ptr = self.inst_ptr;
        let saved_inst_len = self.inst_len;
        self.inst_ptr = instr_ptr;
        self.inst_len = instr_len;
        let saved_constants_raw = self.constants_raw;
        self.constants_raw = constants_raw;
        let saved_cv_ptr = self.constants_values_ptr;
        let saved_cs_ptr = self.constants_syms_ptr;
        self.preconvert_constants();
        // Register functions don't use self.locals — skip save/restore for perf.
        // The parent (rdispatch_loop) also doesn't use locals.
        let saved_sp = self.sp;
        // Skip last_popped save — in register→register calls, it's always None
        // (the result of the previous call was already stored in a register).
        let saved_max_stack_depth = self.max_stack_depth;
        let saved_inline_cache = if num_cache_slots > 0 {
            // SAFETY: func_cache points to a VmCell in a CompiledFunctionObject on the heap.
            // The heap is append-only so the pointer remains valid.
            let taken = std::mem::take(unsafe { &*func_cache }.borrow_mut());
            if taken.is_empty() && num_cache_slots > 0 {
                // Self-recursive call via normal path (e.g. async or rest-param
                // function): the VmCell was already emptied by an outer
                // activation. Allocate a fresh cache for this frame.
                std::mem::replace(
                    &mut self.inline_cache,
                    vec![(0, 0); num_cache_slots as usize],
                )
            } else {
                std::mem::replace(&mut self.inline_cache, taken)
            }
        } else {
            Vec::new()
        };

        self.ip = 0;
        self.max_stack_depth = max_stack_depth as usize;

        // Ensure stack fits register window.
        let needed = reg_base + reg_window;
        if self.stack.len() < needed {
            self.stack.resize(needed, Value::UNDEFINED);
        }

        let arg_offset = if takes_this { 1 } else { 0 };

        // Copy 'this' into register 0
        if takes_this {
            unsafe {
                *self.stack.get_unchecked_mut(reg_base) = receiver_val.unwrap_or(Value::UNDEFINED)
            };
        }

        // Copy positional args directly from caller's stack region to callee's
        // register window via ptr::copy_nonoverlapping. Safe because:
        // - reg_base = saved_sp >= arg_stack_start + nargs (non-overlapping)
        // - stack.resize() above ensures both regions are valid
        let positional_count = rest_parameter_index.map_or(nargs, |ri| nargs.min(ri));
        if positional_count > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.stack.as_ptr().add(arg_stack_start),
                    self.stack.as_mut_ptr().add(reg_base + arg_offset),
                    positional_count,
                );
            }
        }

        // Only init remaining registers to UNDEFINED (skip those already set by args/this)
        let first_uninit = positional_count + arg_offset;
        for i in first_uninit..reg_window {
            unsafe { *self.stack.get_unchecked_mut(reg_base + i) = Value::UNDEFINED };
        }

        // Handle rest parameter — read from original stack position
        if let Some(rest_i) = rest_parameter_index {
            let rest_reg = rest_i + arg_offset;
            let mut rest_values: Vec<Value> = Vec::with_capacity(nargs.saturating_sub(rest_i));
            for i in rest_i..nargs {
                rest_values.push(unsafe { *self.stack.get_unchecked(arg_stack_start + i) });
            }
            unsafe {
                *self.stack.get_unchecked_mut(reg_base + rest_reg) =
                    obj_into_val(make_array(rest_values), &mut self.heap)
            };
        }

        // Set sp past register window
        self.sp = reg_base + reg_window;

        // Execute register dispatch
        // IMPORTANT: use rframes (register frames), not frames (stack frames)
        let entry_depth = self.rframes.len();
        let run_result = self.rdispatch_loop(entry_depth, reg_base);

        // Extract return value directly as Value (no Object conversion!)
        let rv = self.last_popped.take().unwrap_or(Value::UNDEFINED);

        // Restore parent state — raw pointer swaps, zero Rc clones
        self.ip = saved_ip;
        self.inst_ptr = saved_inst_ptr;
        self.inst_len = saved_inst_len;
        self.constants_raw = saved_constants_raw;
        self.constants_values_ptr = saved_cv_ptr;
        self.constants_syms_ptr = saved_cs_ptr;
        self.max_stack_depth = saved_max_stack_depth;
        if num_cache_slots > 0 {
            // SAFETY: func_cache points to a VmCell in a CompiledFunctionObject on the heap.
            // The heap is append-only so the pointer remains valid.
            let our_cache = std::mem::replace(&mut self.inline_cache, saved_inline_cache);
            let fc = unsafe { &*func_cache }.borrow_mut();
            // Only write back if the VmCell is still empty (i.e. we're the
            // outermost activation that originally took the cache). For
            // inner self-recursive activations that allocated a fresh cache,
            // the VmCell was already restored by the outer activation — skip.
            if fc.is_empty() {
                *fc = our_cache;
            }
        }
        // Skip locals restore — register functions don't use locals.
        self.sp = saved_sp;

        // Handle async
        if is_async {
            let rv_obj = val_to_obj(rv, &self.heap);
            let promise = match run_result {
                Ok(()) => crate::object::new_fulfilled_promise(rv_obj),
                Err(err) => crate::object::new_rejected_promise(Object::Error(Box::new(
                    crate::object::ErrorObject {
                        name: Rc::from("Error"),
                        message: Rc::from(format!("{:?}", err)),
                    },
                ))),
            };
            return Ok(obj_into_val(promise, &mut self.heap));
        }

        run_result?;
        Ok(rv)
    }

    /// Transition a pending promise to `Fulfilled` or `Rejected`, then
    /// queue each registered then/catch handler onto the microtask queue
    /// with its companion promise as the target. No-op on an already-
    /// settled promise (matches the spec: resolve/reject only fire once).
    pub(crate) fn settle_promise(
        &mut self,
        prom: &std::rc::Rc<crate::object::VmCell<crate::object::PromiseObject>>,
        state: crate::object::PromiseState,
    ) -> Result<(), VMError> {
        // Snapshot the handler queues, then settle. Handlers run after
        // this call via `drain_microtasks`. We use a Rust-side closure
        // (wrapped as a BuiltinFunction with PromiseChainStep) to
        // orchestrate the handler invocation + chained-promise settle.
        let (then_chain, catch_chain, chained) = {
            let mut p = unsafe { prom.borrow_mut() };
            if !matches!(p.settled, crate::object::PromiseState::Pending) {
                return Ok(());
            }
            p.settled = state.clone();
            (
                std::mem::take(&mut p.then_chain),
                std::mem::take(&mut p.catch_chain),
                std::mem::take(&mut p.chained),
            )
        };
        // Re-borrow is dropped; now safe to queue microtasks.
        for i in 0..chained.len() {
            let handler = match &state {
                crate::object::PromiseState::Fulfilled(_) => then_chain[i],
                crate::object::PromiseState::Rejected(_) => catch_chain[i],
                crate::object::PromiseState::Pending => continue,
            };
            // Queue a small closure-like builtin: PromiseChainStep with
            // receiver = chained promise, args[0] = settlement value,
            // args[1] = handler (may be UNDEFINED).
            let (settle_val, settle_kind) = match &state {
                crate::object::PromiseState::Fulfilled(v) => {
                    (obj_into_val((**v).clone(), &mut self.heap), 0u8)
                }
                crate::object::PromiseState::Rejected(v) => {
                    (obj_into_val((**v).clone(), &mut self.heap), 1u8)
                }
                _ => unreachable!(),
            };
            // Encode the step directly — push a (Value, args) tuple where
            // Value is a step-dispatcher builtin and args carry the
            // context through.
            let step_bf = crate::object::Object::BuiltinFunction(Box::new(
                crate::object::BuiltinFunctionObject {
                    function: crate::object::BuiltinFunction::PromiseChainStep,
                    receiver: None,
                },
            ));
            let step_val = obj_into_val(step_bf, &mut self.heap);
            self.microtask_queue.push_back((
                step_val,
                vec![
                    chained[i],
                    settle_val,
                    Value::from_i64(settle_kind as i64),
                    handler,
                ],
            ));
        }
        Ok(())
    }

    /// Run every queued microtask to completion, FIFO. Each callback can
    /// queue more microtasks during its run; we keep draining until the
    /// queue is empty (matching V8's "drain microtasks before yielding
    /// to the host" semantics). A wall-time / instruction-count check is
    /// re-run after each callback so a malicious script that schedules
    /// itself recursively can't bypass execution limits.
    ///
    /// Returns the count of microtasks that ran. Errors propagate from
    /// the failing callback; subsequent microtasks are dropped (also
    /// matches V8: an unhandled rejection in one task doesn't stop
    /// later same-tick tasks, but for our embedding model bubbling up is
    /// the more useful default).
    pub fn drain_microtasks(&mut self) -> Result<usize, VMError> {
        let mut count = 0usize;
        // 64 K microtasks per drain ought to be plenty; cap so a buggy
        // script that re-queues itself can't DoS the host even if every
        // callback finishes inside 1 µs.
        const MAX_MICROTASKS_PER_DRAIN: usize = 65536;
        // FIFO: pull from the front (index 0). New microtasks queued by
        // a running callback append to the back, so they get picked up
        // on subsequent loop iterations of the same drain.
        loop {
            let (cb, args) = match self.microtask_queue.pop_front() {
                Some(entry) => entry,
                None => break,
            };
            count += 1;
            if count > MAX_MICROTASKS_PER_DRAIN {
                return Err(VMError::TypeError(
                    "microtask queue exceeded MAX_MICROTASKS_PER_DRAIN".to_string(),
                ));
            }
            self.check_builtin_callback_limits()?;
            self.call_value_slice(cb, &args)?;
        }
        Ok(count)
    }

    pub(crate) fn execute_compiled_function_slice(
        &mut self,
        func: CompiledFunctionObject,
        args: &[Value],
        receiver: Option<Value>,
    ) -> Result<(Value, Option<Value>), VMError> {
        self.fn_call_depth += 1;
        if self.fn_call_depth > 64 {
            self.fn_call_depth -= 1;
            return Err(VMError::StackOverflow);
        }
        let result = self.execute_compiled_function_slice_inner(func, args, receiver);
        self.fn_call_depth -= 1;
        result
    }

    fn execute_compiled_function_slice_inner(
        &mut self,
        func: CompiledFunctionObject,
        args: &[Value],
        receiver: Option<Value>,
    ) -> Result<(Value, Option<Value>), VMError> {
        let CompiledFunctionObject {
            instructions,
            constants,
            num_locals,
            num_parameters,
            rest_parameter_index,
            takes_this,
            is_async,
            is_generator: _,
            num_cache_slots,
            max_stack_depth,
            register_count,
            inline_cache: func_cache,
            closure_captures: _,
            captured_values, properties: _,
        } = func;

        let is_register = register_count > 0;

        // Inject captured closure values into globals (for closures created by MakeClosure)
        let mut closure_saves: Vec<(u16, Value)> = Vec::new();
        for &(slot, val) in &captured_values {
            let old = unsafe { self.globals.get_unchecked(slot as usize) };
            unsafe { self.globals.set_unchecked(slot as usize, val) };
            closure_saves.push((slot, old));
        }

        // Save current VM state
        let saved_ip = self.ip;
        let saved_inst_ptr = self.inst_ptr;
        let saved_inst_len = self.inst_len;
        let saved_instructions = std::mem::replace(&mut self.instructions, instructions);
        self.inst_ptr = self.instructions.as_ptr();
        self.inst_len = self.instructions.len();
        let saved_constants = std::mem::replace(&mut self.constants, constants);
        let saved_constants_raw = self.constants_raw;
        let saved_cv_ptr = self.constants_values_ptr;
        let saved_cs_ptr = self.constants_syms_ptr;
        if is_register {
            self.constants_raw = &*self.constants as *const Vec<Object>;
            self.preconvert_constants();
        }
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_sp = self.sp;
        let saved_last_popped = self.last_popped.take();
        let saved_max_stack_depth = self.max_stack_depth;
        let saved_inline_cache = if num_cache_slots > 0 {
            let mut taken = std::mem::take(unsafe { func_cache.borrow_mut() });
            // If the cache was already taken (recursive call), allocate fresh
            if taken.is_empty() {
                taken = vec![(0, 0); num_cache_slots as usize];
            }
            std::mem::replace(&mut self.inline_cache, taken)
        } else {
            Vec::new()
        };

        self.ip = 0;
        self.max_stack_depth = max_stack_depth as usize;
        let arg_offset = if takes_this { 1 } else { 0 };
        let rest_index = rest_parameter_index;

        let (run_result, return_value, receiver_after) = if is_register {
            // ── Register-based function ──────────────────────────────
            let reg_base = self.sp;
            // Ensure register window is large enough for both the function's
            // registers AND the argument slots (this + positional args + rest).
            let positional_count = rest_index.map_or(args.len(), |ri| args.len().min(ri));
            let arg_slots = positional_count + arg_offset;
            let rest_slots = rest_index.map_or(0, |ri| ri + arg_offset + 1);
            let reg_window = (register_count as usize).max(1).max(arg_slots).max(rest_slots);

            // Stack bounds check
            if reg_base + reg_window > STACK_SIZE {
                self.ip = saved_ip;
                self.instructions = saved_instructions;
                self.inst_ptr = saved_inst_ptr;
                self.inst_len = saved_inst_len;
                self.constants = saved_constants;
                self.constants_raw = saved_constants_raw;
                self.constants_values_ptr = saved_cv_ptr;
                self.constants_syms_ptr = saved_cs_ptr;
                self.max_stack_depth = saved_max_stack_depth;
                if num_cache_slots > 0 {
                    *unsafe { func_cache.borrow_mut() } =
                        std::mem::replace(&mut self.inline_cache, saved_inline_cache);
                }
                self.locals = saved_locals;
                self.last_popped = saved_last_popped;
                return Err(VMError::StackOverflow);
            }

            // Extend stack to fit register window
            while self.stack.len() < reg_base + reg_window {
                self.stack.push(Value::UNDEFINED);
            }

            // Initialize register window to UNDEFINED
            for i in 0..reg_window {
                self.stack[reg_base + i] = Value::UNDEFINED;
            }

            // Copy 'this' into register 0
            if takes_this {
                self.stack[reg_base] = receiver.unwrap_or(Value::UNDEFINED);
            }

            // Copy positional args into registers (window already sized to fit)
            for (i, arg) in args.iter().take(positional_count).enumerate() {
                self.stack[reg_base + i + arg_offset] = *arg;
            }

            // Handle rest parameter
            if let Some(rest_i) = rest_index {
                let rest_reg = rest_i + arg_offset;
                let rest_values: Vec<Value> = args.iter().skip(rest_i).copied().collect();
                self.stack[reg_base + rest_reg] =
                    obj_into_val(make_array(rest_values), &mut self.heap);
            }

            // No locals needed for register VM
            self.locals = Vec::new();

            // Set sp past register window for scratch use
            self.sp = reg_base + reg_window;
            self.last_call_nargs = args.len() as u16;

            // Execute register dispatch
            let entry_depth = self.rframes.len();
            let rr = self.rdispatch_loop(entry_depth, reg_base);

            // Extract results � return Value directly
            let rv = self.last_popped.take().unwrap_or(Value::UNDEFINED);
            let ra = if takes_this && reg_base < self.stack.len() {
                Some(self.stack[reg_base])
            } else {
                None
            };

            (rr, rv, ra)
        } else {
            // ── Stack-based function ─────────────────────────────────
            let init_local_count = num_parameters + arg_offset;
            let needed = num_locals.max(init_local_count);
            let mut new_locals = self
                .locals_pool
                .pop()
                .unwrap_or_else(|| Vec::with_capacity(needed));
            new_locals.resize(needed, Object::Undefined);

            if let Some(rest_i) = rest_parameter_index {
                let need = rest_i + arg_offset + 1;
                if new_locals.len() < need {
                    new_locals.resize(need, Object::Undefined);
                }
            }

            if takes_this {
                new_locals[0] = val_to_obj(receiver.unwrap_or(Value::UNDEFINED), &self.heap);
            }

            if let Some(rest_i) = rest_index {
                let rest_local = rest_i + arg_offset;
                let rest_values: Vec<Value> = args.iter().skip(rest_i).copied().collect();
                new_locals[rest_local] = make_array(rest_values);
            }

            let positional_count = rest_index.map_or(args.len(), |rest_i| args.len().min(rest_i));
            let positional_count =
                positional_count.min(new_locals.len().saturating_sub(arg_offset));
            for (i, arg) in args.iter().take(positional_count).enumerate() {
                let target = i + arg_offset;
                unsafe { *new_locals.get_unchecked_mut(target) = val_to_obj(*arg, &self.heap) };
            }

            self.locals = new_locals;

            if max_stack_depth > 0 && self.sp + max_stack_depth as usize > STACK_SIZE {
                self.ip = saved_ip;
                self.instructions = saved_instructions;
                self.inst_ptr = saved_inst_ptr;
                self.inst_len = saved_inst_len;
                self.constants = saved_constants;
                self.constants_raw = saved_constants_raw;
                self.max_stack_depth = saved_max_stack_depth;
                if num_cache_slots > 0 {
                    *unsafe { func_cache.borrow_mut() } =
                        std::mem::replace(&mut self.inline_cache, saved_inline_cache);
                }
                let mut used_locals = std::mem::replace(&mut self.locals, saved_locals);
                used_locals.clear();
                self.locals_pool.push(used_locals);
                self.stack.truncate(saved_sp);
                self.sp = saved_sp;
                self.last_popped = saved_last_popped;
                return Err(VMError::StackOverflow);
            }

            // Stack-based function dispatch was removed in 0.4.0 along
            // with the legacy stack compiler. Every function compiled
            // through `rcompiler` ships with `register_count > 0`, so
            // reaching this branch now means some caller synthesised a
            // `CompiledFunctionObject` with `register_count == 0` and
            // fed it to the VM — a bug we want surfaced, not silently
            // papered over.
            let rr: Result<(), VMError> = Err(VMError::TypeError(
                "stack-based function dispatch has been removed; \
                 CompiledFunctionObject.register_count must be > 0"
                    .to_string(),
            ));

            let rv = self.last_popped.take().unwrap_or(Value::UNDEFINED);
            let ra = if takes_this {
                if self.locals.is_empty() {
                    None
                } else {
                    let obj = std::mem::replace(&mut self.locals[0], Object::Undefined);
                    Some(obj_into_val(obj, &mut self.heap))
                }
            } else {
                None
            };

            (rr, rv, ra)
        };

        // Restore parent state
        self.ip = saved_ip;
        self.instructions = saved_instructions;
        // Restore inst_ptr/inst_len from saved values, NOT from self.instructions.
        // When the caller was entered via call_register_direct (register-based),
        // self.inst_ptr points to the caller's Rc<Vec<u8>> data (not self.instructions).
        // Restoring from self.instructions.as_ptr() would point to the wrong bytecode.
        self.inst_ptr = saved_inst_ptr;
        self.inst_len = saved_inst_len;
        self.constants = saved_constants;
        self.constants_raw = saved_constants_raw;
        self.constants_values_ptr = saved_cv_ptr;
        self.constants_syms_ptr = saved_cs_ptr;
        self.max_stack_depth = saved_max_stack_depth;
        if num_cache_slots > 0 {
            *unsafe { func_cache.borrow_mut() } =
                std::mem::replace(&mut self.inline_cache, saved_inline_cache);
        }
        let mut used_locals = std::mem::replace(&mut self.locals, saved_locals);
        used_locals.clear();
        self.locals_pool.push(used_locals);
        self.stack.truncate(saved_sp);
        self.sp = saved_sp;
        self.last_popped = saved_last_popped;

        // Restore captured closure values
        for &(slot, old_val) in &closure_saves {
            unsafe { self.globals.set_unchecked(slot as usize, old_val) };
        }

        // Handle async/errors
        if is_async {
            let return_obj = val_to_obj(return_value, &self.heap);
            let promise = match run_result {
                Ok(()) => crate::object::new_fulfilled_promise(return_obj),
                Err(err) => crate::object::new_rejected_promise(Object::Error(Box::new(
                    crate::object::ErrorObject {
                        name: Rc::from("Error"),
                        message: Rc::from(format!("{:?}", err)),
                    },
                ))),
            };
            let promise_val = obj_into_val(promise, &mut self.heap);
            return Ok((promise_val, receiver_after));
        }

        run_result?;
        Ok((return_value, receiver_after))
    }



    pub(crate) fn execute_new_with_args_slice(
        &mut self,
        callee: Object,
        args: &[Value],
    ) -> Result<(), VMError> {
        // Save and set new.target for the duration of this constructor call.
        let saved_new_target = self.new_target;

        match callee {
            Object::Class(class_obj) => {
                // Set new.target to the class itself (clone before destructuring).
                self.new_target = obj_into_val(Object::Class(class_obj.clone()), &mut self.heap);

                let crate::object::ClassObject {
                    name,
                    parent_chain,
                    constructor,
                    methods,
                    getters,
                    setters,
                    super_methods,
                    super_getters,
                    super_setters,
                    super_constructor_chain,
                    field_initializers,
                    ..
                } = *class_obj;

                let mut instance = crate::object::InstanceObject {
                    class_name: name,
                    parent_chain,
                    fields: rustc_hash::FxHashMap::default(),
                    methods,
                    getters,
                    setters,
                    super_methods,
                    super_getters,
                    super_setters,
                    super_constructor_chain,
                };

                // Run instance field initializers (parent fields first, then own)
                for (field_name, thunk) in &field_initializers {
                    let receiver_val =
                        obj_into_val(Object::Instance(Box::new(instance.clone())), &mut self.heap);
                    let (result, receiver_after) = self.execute_compiled_function_slice(
                        thunk.clone(),
                        &[],
                        Some(receiver_val),
                    )?;
                    // Update instance from receiver_after (in case thunk mutated this)
                    if let Some(ra) = receiver_after {
                        if let Object::Instance(updated) = val_to_obj(ra, &self.heap) {
                            instance = *updated;
                        }
                    }
                    instance.fields.insert(field_name.clone(), result);
                }

                if let Some(ctor) = constructor {
                    let receiver_val =
                        obj_into_val(Object::Instance(Box::new(instance.clone())), &mut self.heap);
                    let (_, receiver_after) =
                        self.execute_compiled_function_slice(ctor, args, Some(receiver_val))?;
                    if let Some(ra) = receiver_after {
                        if let Object::Instance(updated) = val_to_obj(ra, &self.heap) {
                            instance = *updated;
                        }
                    }
                }

                self.push(Object::Instance(Box::new(instance)))?;
                self.new_target = saved_new_target;
                Ok(())
            }
            Object::CompiledFunction(func) => {
                // new CompiledFunction(args): create object, copy prototype
                // methods, call constructor with it as `this`.
                let mut new_hash = HashObject::default();
                // Copy methods from Func.prototype to the new object
                if let Some(ref props) = func.properties {
                    let proto_sym = crate::intern::intern("prototype");
                    if let Some(proto_val) = props.get(&proto_sym) {
                        let proto_obj = val_to_obj(*proto_val, &self.heap);
                        if let Object::Hash(ph) = proto_obj {
                            let phb = ph.borrow();
                            let _proto_keys: usize = phb.pairs.len();
                            for (key, &val) in &phb.pairs {
                                if let crate::object::HashKey::Sym(s) = key {
                                    new_hash.set_by_sym(*s, val);
                                }
                            }
                        }
                    }
                }
                let new_obj = make_hash(new_hash);
                let receiver_val = obj_into_val(new_obj, &mut self.heap);
                self.new_target = receiver_val;
                let (result, receiver_after) = self.execute_compiled_function_slice(
                    *func, args, Some(receiver_val),
                )?;
                // Use receiver_after (modified `this`) if constructor didn't return an object
                let this_val = receiver_after.unwrap_or(receiver_val);
                let result_obj = val_to_obj(result, &self.heap);
                let final_val = match &result_obj {
                    Object::Hash(_) | Object::Instance(_) | Object::Array(_) => result,
                    _ => this_val
                };
                self.push_val(final_val)?;
                self.new_target = saved_new_target;
                Ok(())
            }
            Object::BoundMethod(bound) => {
                // new BoundMethod(args): same as CompiledFunction
                let new_obj = make_hash(HashObject::default());
                let receiver_val = obj_into_val(new_obj, &mut self.heap);
                self.new_target = receiver_val;
                let (result, _) = self.execute_compiled_function_slice(
                    bound.function, args, Some(receiver_val),
                )?;
                let result_obj = val_to_obj(result, &self.heap);
                let final_val = match &result_obj {
                    Object::Hash(_) | Object::Instance(_) | Object::Array(_) => result,
                    _ => receiver_val,
                };
                self.push_val(final_val)?;
                self.new_target = saved_new_target;
                Ok(())
            }
            Object::BuiltinFunction(builtin) => {
                self.new_target = saved_new_target;
                let out = self.execute_builtin_function_slice(*builtin, args)?;
                self.push_val(out)?;
                Ok(())
            }
            // Handle `new Date()` / `new Array()` / `new Promise(executor)`
            Object::Hash(ref hash) => {
                let h = hash.borrow();
                // `__construct` sentinel: when present, route the new-call
                // to the named BuiltinFunction. Used so callable global
                // namespaces (Promise's static-methods hash) can also be
                // invoked as `new Foo(...)` without a separate constructor
                // object.
                let construct_fn = h
                    .pairs
                    .iter()
                    .find_map(|(k, v)| match k {
                        HashKey::Sym(sym) if &*crate::intern::resolve(*sym) == "__construct" => {
                            Some(*v)
                        }
                        _ => None,
                    });
                let is_date = h.pairs.iter().any(|(k, _)| {
                    if let HashKey::Sym(sym) = k {
                        &*crate::intern::resolve(*sym) == "now"
                            && construct_fn.is_none()
                    } else { false }
                });
                let is_array = h.pairs.iter().any(|(k, _)| {
                    if let HashKey::Sym(sym) = k {
                        &*crate::intern::resolve(*sym) == "isArray"
                    } else { false }
                });
                let _ = h;
                if let Some(ctor_v) = construct_fn {
                    // Route through execute_builtin_function_slice so the
                    // synchronous-executor path in PromiseExecutorCtor sees
                    // the args verbatim.
                    let ctor_obj = val_to_obj(ctor_v, &self.heap);
                    if let Object::BuiltinFunction(b) = ctor_obj {
                        let out = self.execute_builtin_function_slice(*b, args)?;
                        self.push_val(out)?;
                        self.new_target = saved_new_target;
                        return Ok(());
                    }
                }
                if is_array {
                    // new Array() / new Array(n) / new Array(a, b, c)
                    let arr = if args.len() == 1 {
                        let arg = args[0];
                        let len = if arg.is_i32() {
                            (unsafe { arg.as_i32_unchecked() }) as usize
                        } else if arg.is_f64() {
                            arg.as_f64() as usize
                        } else {
                            // single non-numeric arg → array with that element
                            let items = vec![arg];
                            self.push(make_array(items))?;
                            self.new_target = saved_new_target;
                            return Ok(());
                        };
                        // `new Array(2**31)` would otherwise ask for
                        // a 16 GiB `vec![Value::UNDEFINED; len]` before
                        // a single push ran. Cap at MAX_ARRAY_SIZE —
                        // matches Array.from's length cap.
                        if len > MAX_ARRAY_SIZE {
                            return Err(VMError::TypeError(format!(
                                "Invalid array length: {} exceeds MAX_ARRAY_SIZE ({})",
                                len, MAX_ARRAY_SIZE
                            )));
                        }
                        let items = vec![Value::UNDEFINED; len];
                        make_array(items)
                    } else if args.is_empty() {
                        make_array(vec![])
                    } else {
                        make_array(args.to_vec())
                    };
                    self.push(arr)?;
                    self.new_target = saved_new_target;
                    Ok(())
                } else if is_date {
                    // Support `new Date()`, `new Date(ms)`, `new Date(string)`
                    let ms = if args.is_empty() {
                        epoch_millis_now()
                    } else {
                        let arg = args[0];
                        if arg.is_f64() {
                            arg.as_f64()
                        } else if arg.is_i32() {
                            (unsafe { arg.as_i32_unchecked() }) as f64
                        } else if arg.is_heap() {
                            match self.heap.get(arg.heap_index()) {
                                Object::String(s) => {
                                    // Very basic ISO 8601 parse
                                    s.parse::<f64>().unwrap_or_else(|_| epoch_millis_now())
                                }
                                _ => epoch_millis_now(),
                            }
                        } else {
                            epoch_millis_now()
                        }
                    };
                    // Build the hash, then bind it as the receiver for every
                    // method. Setters mutate `__time_ms` in this hash, so
                    // every method invoked on the same Date sees the same
                    // backing storage.
                    let mut date_hash = crate::object::HashObject::default();
                    date_hash.insert_pair(
                        HashKey::from_string("__time_ms"),
                        Value::from_f64(ms),
                    );
                    let date_obj = make_hash(date_hash);
                    let date_hash_rc = match &date_obj {
                        Object::Hash(rc) => rc.clone(),
                        _ => unreachable!(),
                    };
                    let receiver = Object::Hash(date_hash_rc.clone());

                    macro_rules! date_method {
                        ($name:expr, $func:ident) => {
                            let bf = Object::BuiltinFunction(Box::new(
                                crate::object::BuiltinFunctionObject {
                                    function: BuiltinFunction::$func,
                                    receiver: Some(receiver.clone()),
                                },
                            ));
                            let v = obj_into_val(bf, &mut self.heap);
                            unsafe { date_hash_rc.borrow_mut() }.insert_pair(
                                HashKey::from_string($name),
                                v,
                            );
                        }
                    }

                    date_method!("getTime", DateGetTime);
                    date_method!("getHours", DateGetHours);
                    date_method!("getMinutes", DateGetMinutes);
                    date_method!("getSeconds", DateGetSeconds);
                    date_method!("getMilliseconds", DateGetMilliseconds);
                    date_method!("getFullYear", DateGetFullYear);
                    date_method!("getMonth", DateGetMonth);
                    date_method!("getDate", DateGetDate);
                    date_method!("getDay", DateGetDay);
                    date_method!("toISOString", DateToISOString);
                    date_method!("toLocaleDateString", DateToLocaleDateString);
                    date_method!("toLocaleTimeString", DateToLocaleTimeString);
                    date_method!("toLocaleString", DateToLocaleString);
                    date_method!("toString", DateToString);
                    date_method!("valueOf", DateValueOf);
                    // ── Setters ──
                    date_method!("setTime", DateSetTime);
                    date_method!("setHours", DateSetHours);
                    date_method!("setMinutes", DateSetMinutes);
                    date_method!("setSeconds", DateSetSeconds);
                    date_method!("setMilliseconds", DateSetMilliseconds);
                    date_method!("setFullYear", DateSetFullYear);
                    date_method!("setMonth", DateSetMonth);
                    date_method!("setDate", DateSetDate);

                    self.push(date_obj)?;
                    self.new_target = saved_new_target;
                    Ok(())
                } else {
                    // Not Date/Array — just return the hash as-is (used as a
                    // constructor like new Set() / new Map() polyfill)
                    self.push(Object::Hash(hash.clone()))?;
                    self.new_target = saved_new_target;
                    Ok(())
                }
            }
            Object::Undefined | Object::Null => {
                // Graceful: new undefined() returns empty object
                self.push(make_hash(HashObject::default()))?;
                self.new_target = saved_new_target;
                Ok(())
            }
            other => {
                self.new_target = saved_new_target;
                Err(VMError::TypeError(format!(
                    "not a constructor: {:?}",
                    other.object_type()
                )))
            }
        }
    }


    pub(crate) fn execute_delete_property(
        &mut self,
        target: Object,
        key: Object,
    ) -> Result<(), VMError> {
        match target {
            Object::Hash(hash) => {
                let k = self.hash_key_from_object(&key);
                unsafe { hash.borrow_mut() }.remove_pair(&k);
                self.push(Object::Hash(hash))?;
                Ok(())
            }
            Object::Array(arr) => {
                let idx = match key {
                    Object::Integer(v) => v,
                    Object::Float(v) if v.fract() == 0.0 => v as i64,
                    _ => {
                        self.push(Object::Array(arr))?;
                        return Ok(());
                    }
                };
                if idx >= 0 {
                    let uidx = idx as usize;
                    let arr_ref = unsafe { arr.borrow_mut() };
                    if uidx < arr_ref.len() {
                        arr_ref[uidx] = Value::UNDEFINED;
                    }
                }
                self.push(Object::Array(arr))?;
                Ok(())
            }
            Object::Instance(mut instance) => {
                let prop = self.object_key_cow(&key);
                instance.fields.remove(prop.as_ref());
                self.push(Object::Instance(instance))?;
                Ok(())
            }
            _ => {
                self.push(target)?;
                Ok(())
            }
        }
    }

    // ── Generator support ───────────────────────────────────────────────

    /// Create a `{value, done}` iterator result hash object.
    fn make_iterator_result(&mut self, value: Value, done: bool) -> Value {
        let mut hash = crate::object::HashObject::with_capacity(2);
        let sym_value = crate::intern::intern("value");
        let sym_done = crate::intern::intern("done");
        hash.insert_pair(crate::object::HashKey::Sym(sym_value), value);
        hash.insert_pair(
            crate::object::HashKey::Sym(sym_done),
            Value::from_bool(done),
        );
        let obj = Object::Hash(Rc::new(crate::object::VmCell::new(hash)));
        obj_into_val(obj, &mut self.heap)
    }

    /// Create a GeneratorObject from a generator function and its arguments.
    /// Returns the generator as a NaN-boxed Value.
    pub(crate) fn create_generator(
        &mut self,
        func: CompiledFunctionObject,
        args: Vec<Value>,
        receiver: Option<Value>,
    ) -> Value {
        use crate::object::{GeneratorObject, GeneratorState, VmCell};
        let gen = GeneratorObject {
            function: func,
            locals: Vec::new(),
            saved_ip: 0,
            args,
            receiver,
            state: GeneratorState::Created,
        };
        let obj = Object::Generator(Rc::new(VmCell::new(gen)));
        obj_into_val(obj, &mut self.heap)
    }

    /// Execute a generator `.next(arg)` call.
    ///
    /// On the first call (`Created` state), the generator function is set up
    /// and executed until the first `yield` or `return`.
    ///
    /// On subsequent calls (`Suspended` state), the VM state is restored and
    /// the value passed to `.next()` is pushed onto the stack (as the result
    /// of the `yield` expression), then execution continues.
    ///
    /// Returns a `{value, done}` iterator result.
    pub(crate) fn execute_generator_next(
        &mut self,
        gen_rc: &Rc<crate::object::VmCell<crate::object::GeneratorObject>>,
        next_arg: Value,
    ) -> Result<Value, VMError> {
        use crate::object::GeneratorState;

        let state = gen_rc.borrow().state.clone();
        match state {
            GeneratorState::Completed => Ok(self.make_iterator_result(Value::UNDEFINED, true)),
            GeneratorState::Created => {
                // First call: set up the function and run until yield/return.
                let func = gen_rc.borrow().function.clone();
                let args = gen_rc.borrow().args.clone();
                let receiver = gen_rc.borrow().receiver;

                unsafe { gen_rc.borrow_mut() }.state = GeneratorState::Suspended;

                // Run the function body.  If it yields, we get Err(Yield(v)).
                let result = self.execute_generator_body(
                    gen_rc, func, &args, receiver, None, // no saved_ip — start from 0
                    None, // no resume value on first call
                );
                self.finalize_generator_result(gen_rc, result)
            }
            GeneratorState::Suspended => {
                // Resume: restore state and push the .next() argument.
                let func = gen_rc.borrow().function.clone();
                let saved_ip = gen_rc.borrow().saved_ip;
                let receiver = gen_rc.borrow().receiver;

                let result = self.execute_generator_body(
                    gen_rc,
                    func,
                    &[],
                    receiver,
                    Some(saved_ip),
                    Some(next_arg),
                );
                self.finalize_generator_result(gen_rc, result)
            }
        }
    }

    /// Execute a generator `.return(value)` call.
    /// Forces the generator to complete with the given value.
    fn execute_generator_return(
        &mut self,
        gen_rc: &Rc<crate::object::VmCell<crate::object::GeneratorObject>>,
        return_value: Value,
    ) -> Result<Value, VMError> {
        use crate::object::GeneratorState;

        let state = gen_rc.borrow().state.clone();
        match state {
            GeneratorState::Completed => Ok(self.make_iterator_result(return_value, true)),
            _ => {
                unsafe { gen_rc.borrow_mut() }.state = GeneratorState::Completed;
                Ok(self.make_iterator_result(return_value, true))
            }
        }
    }

    /// Execute the generator function body (either from the beginning or from
    /// a saved instruction pointer).
    ///
    /// This uses `execute_compiled_function_slice` with the full save/restore
    /// machinery. On `Yield`, the VM state (ip, locals) is saved into the
    /// GeneratorObject so it can be resumed later.
    fn execute_generator_body(
        &mut self,
        gen_rc: &Rc<crate::object::VmCell<crate::object::GeneratorObject>>,
        func: CompiledFunctionObject,
        args: &[Value],
        receiver: Option<Value>,
        resume_ip: Option<usize>,
        resume_value: Option<Value>,
    ) -> Result<Value, VMError> {
        // Destructure the function
        let CompiledFunctionObject {
            instructions,
            constants,
            num_locals,
            num_parameters,
            rest_parameter_index,
            takes_this,
            is_async: _,
            is_generator: _,
            num_cache_slots,
            max_stack_depth,
            register_count,
            inline_cache: func_cache,
            closure_captures: _,
            captured_values: _, properties: _,
        } = func;

        let is_register = register_count > 0;

        // Save current VM state
        let saved_ip = self.ip;
        let saved_inst_ptr = self.inst_ptr;
        let saved_inst_len = self.inst_len;
        let saved_instructions = std::mem::replace(&mut self.instructions, instructions);
        self.inst_ptr = self.instructions.as_ptr();
        self.inst_len = self.instructions.len();
        let saved_constants = std::mem::replace(&mut self.constants, constants);
        let saved_constants_raw = self.constants_raw;
        let saved_cv_ptr = self.constants_values_ptr;
        let saved_cs_ptr = self.constants_syms_ptr;
        if is_register {
            self.constants_raw = &*self.constants as *const Vec<Object>;
            self.preconvert_constants();
        }
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_sp = self.sp;
        let saved_last_popped = self.last_popped.take();
        let saved_max_stack_depth = self.max_stack_depth;
        let saved_inline_cache = if num_cache_slots > 0 {
            let mut taken = std::mem::take(unsafe { func_cache.borrow_mut() });
            // If the cache was already taken (recursive call), allocate fresh
            if taken.is_empty() {
                taken = vec![(0, 0); num_cache_slots as usize];
            }
            std::mem::replace(&mut self.inline_cache, taken)
        } else {
            Vec::new()
        };

        self.max_stack_depth = max_stack_depth as usize;
        let arg_offset = if takes_this { 1 } else { 0 };

        let (run_result, return_value) = if is_register {
            // ── Register-based generator ──────────────────────────────
            let reg_base = self.sp;
            let reg_window = (register_count as usize).max(1);

            if reg_base + reg_window > STACK_SIZE {
                // Restore on error
                self.ip = saved_ip;
                self.instructions = saved_instructions;
                self.inst_ptr = saved_inst_ptr;
                self.inst_len = saved_inst_len;
                self.constants = saved_constants;
                self.constants_raw = saved_constants_raw;
                self.constants_values_ptr = saved_cv_ptr;
                self.constants_syms_ptr = saved_cs_ptr;
                self.max_stack_depth = saved_max_stack_depth;
                if num_cache_slots > 0 {
                    *unsafe { func_cache.borrow_mut() } =
                        std::mem::replace(&mut self.inline_cache, saved_inline_cache);
                }
                self.locals = saved_locals;
                self.last_popped = saved_last_popped;
                return Err(VMError::StackOverflow);
            }

            while self.stack.len() < reg_base + reg_window {
                self.stack.push(Value::UNDEFINED);
            }

            if let Some(ip) = resume_ip {
                // Resuming: restore saved registers from GeneratorObject.locals
                self.ip = ip;
                {
                    let gen = gen_rc.borrow();
                    let saved_regs = &gen.locals;
                    for (i, obj) in saved_regs.iter().enumerate() {
                        if i < reg_window {
                            self.stack[reg_base + i] = obj_into_val(obj.clone(), &mut self.heap);
                        }
                    }
                }

                // Push the resume value (the arg to .next()) into the register
                // that was the dst of the ROp::Yield instruction.
                // saved_ip points past the 5-byte Yield instruction [opcode, dst_hi, dst_lo, src_hi, src_lo],
                // so the dst register is the big-endian u16 at instructions[ip-4..ip-2].
                if let Some(rv) = resume_value {
                    let dst_reg = ((self.instructions[ip - 4] as usize) << 8)
                        | (self.instructions[ip - 3] as usize);
                    self.stack[reg_base + dst_reg] = rv;
                }
            } else {
                // First call: initialize registers
                self.ip = 0;
                for i in 0..reg_window {
                    self.stack[reg_base + i] = Value::UNDEFINED;
                }
                if takes_this {
                    self.stack[reg_base] = receiver.unwrap_or(Value::UNDEFINED);
                }
                let rest_index = rest_parameter_index;
                let positional_count = rest_index.map_or(args.len(), |ri| args.len().min(ri));
                for (i, arg) in args.iter().take(positional_count).enumerate() {
                    self.stack[reg_base + i + arg_offset] = *arg;
                }
                if let Some(rest_i) = rest_index {
                    let rest_reg = rest_i + arg_offset;
                    let rest_values: Vec<Value> = args.iter().skip(rest_i).copied().collect();
                    self.stack[reg_base + rest_reg] =
                        obj_into_val(make_array(rest_values), &mut self.heap);
                }
            }

            self.locals = Vec::new();
            self.sp = reg_base + reg_window;

            let entry_depth = self.rframes.len();
            let rr = self.rdispatch_loop(entry_depth, reg_base);

            // Save register state if yielding
            if let Err(VMError::Yield(_)) = &rr {
                let mut regs = Vec::with_capacity(reg_window);
                for i in 0..reg_window {
                    regs.push(val_to_obj(self.stack[reg_base + i], &self.heap));
                }
                let gen = unsafe { gen_rc.borrow_mut() };
                gen.locals = regs;
                gen.saved_ip = self.ip;
            }

            let rv = self.last_popped.take().unwrap_or(Value::UNDEFINED);
            (rr, rv)
        } else {
            // ── Stack-based generator ─────────────────────────────────
            let init_local_count = num_parameters + arg_offset;
            let needed = num_locals.max(init_local_count);

            if let Some(ip) = resume_ip {
                // Resuming: restore locals from GeneratorObject
                self.ip = ip;
                {
                    let gen = gen_rc.borrow();
                    self.locals = gen.locals.clone();
                }

                // Push resume value onto the stack for OpYield to "receive"
                if let Some(rv) = resume_value {
                    unsafe { self.push_unchecked(rv) };
                }
            } else {
                // First call: set up locals
                self.ip = 0;
                let mut new_locals = self
                    .locals_pool
                    .pop()
                    .unwrap_or_else(|| Vec::with_capacity(needed));
                new_locals.resize(needed, Object::Undefined);

                if let Some(rest_i) = rest_parameter_index {
                    let need = rest_i + arg_offset + 1;
                    if new_locals.len() < need {
                        new_locals.resize(need, Object::Undefined);
                    }
                }

                if takes_this {
                    new_locals[0] = val_to_obj(receiver.unwrap_or(Value::UNDEFINED), &self.heap);
                }

                let rest_index = rest_parameter_index;
                if let Some(rest_i) = rest_index {
                    let rest_local = rest_i + arg_offset;
                    let rest_values: Vec<Value> = args.iter().skip(rest_i).copied().collect();
                    new_locals[rest_local] = make_array(rest_values);
                }

                let positional_count =
                    rest_parameter_index.map_or(args.len(), |rest_i| args.len().min(rest_i));
                let positional_count =
                    positional_count.min(new_locals.len().saturating_sub(arg_offset));
                for (i, arg) in args.iter().take(positional_count).enumerate() {
                    let target = i + arg_offset;
                    unsafe { *new_locals.get_unchecked_mut(target) = val_to_obj(*arg, &self.heap) };
                }

                self.locals = new_locals;
            }

            if max_stack_depth > 0 && self.sp + max_stack_depth as usize > STACK_SIZE {
                self.ip = saved_ip;
                self.instructions = saved_instructions;
                self.inst_ptr = saved_inst_ptr;
                self.inst_len = saved_inst_len;
                self.constants = saved_constants;
                self.constants_raw = saved_constants_raw;
                self.max_stack_depth = saved_max_stack_depth;
                if num_cache_slots > 0 {
                    *unsafe { func_cache.borrow_mut() } =
                        std::mem::replace(&mut self.inline_cache, saved_inline_cache);
                }
                let mut used_locals = std::mem::replace(&mut self.locals, saved_locals);
                used_locals.clear();
                self.locals_pool.push(used_locals);
                self.stack.truncate(saved_sp);
                self.sp = saved_sp;
                self.last_popped = saved_last_popped;
                return Err(VMError::StackOverflow);
            }

            // Legacy stack dispatch removed in 0.4.0 — same rationale
            // as the stack branch of execute_compiled_function_slice_inner.
            // Generators on the register path yield via ROp::Yield and
            // are resumed through run_register.
            let rr: Result<(), VMError> = Err(VMError::TypeError(
                "stack-based generator dispatch has been removed; \
                 register-based dispatch is the only supported path"
                    .to_string(),
            ));

            // Save locals if yielding
            if let Err(VMError::Yield(_)) = &rr {
                let gen = unsafe { gen_rc.borrow_mut() };
                gen.locals = self.locals.clone();
                gen.saved_ip = self.ip;
            }

            let rv = self.last_popped.take().unwrap_or(Value::UNDEFINED);
            (rr, rv)
        };

        // Restore parent VM state
        self.ip = saved_ip;
        self.instructions = saved_instructions;
        // Restore inst_ptr/inst_len from saved values (see execute_compiled_function_slice).
        self.inst_ptr = saved_inst_ptr;
        self.inst_len = saved_inst_len;
        self.constants = saved_constants;
        self.constants_raw = saved_constants_raw;
        self.constants_values_ptr = saved_cv_ptr;
        self.constants_syms_ptr = saved_cs_ptr;
        self.max_stack_depth = saved_max_stack_depth;
        if num_cache_slots > 0 {
            *unsafe { func_cache.borrow_mut() } =
                std::mem::replace(&mut self.inline_cache, saved_inline_cache);
        }
        let mut used_locals = std::mem::replace(&mut self.locals, saved_locals);
        used_locals.clear();
        self.locals_pool.push(used_locals);
        self.stack.truncate(saved_sp);
        self.sp = saved_sp;
        self.last_popped = saved_last_popped;

        match run_result {
            Err(VMError::Yield(yielded)) => Err(VMError::Yield(yielded)),
            Err(e) => Err(e),
            Ok(()) => Ok(return_value),
        }
    }

    /// Convert the result of `execute_generator_body` into an iterator result.
    /// On yield: `{value: yielded, done: false}`.
    /// On return/completion: `{value: returned, done: true}` + mark completed.
    fn finalize_generator_result(
        &mut self,
        gen_rc: &Rc<crate::object::VmCell<crate::object::GeneratorObject>>,
        result: Result<Value, VMError>,
    ) -> Result<Value, VMError> {
        use crate::object::GeneratorState;
        match result {
            Ok(return_value) => {
                // Normal completion (return or end of function)
                unsafe { gen_rc.borrow_mut() }.state = GeneratorState::Completed;
                Ok(self.make_iterator_result(return_value, true))
            }
            Err(VMError::Yield(yielded_val)) => {
                // Suspension: state already set to Suspended, ip/locals saved
                // by execute_generator_body
                Ok(self.make_iterator_result(yielded_val, false))
            }
            Err(e) => {
                unsafe { gen_rc.borrow_mut() }.state = GeneratorState::Completed;
                Err(e)
            }
        }
    }
}

// ── URI encoding helpers ──────────────────────────────────────────

/// Characters that `encodeURIComponent` does NOT encode.
const URI_COMPONENT_UNESCAPED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()";

/// Extra characters that `encodeURI` (but not `encodeURIComponent`) preserves.
const URI_EXTRA_UNESCAPED: &[u8] = b";,/?:@&=+$#";

pub(super) fn uri_encode(input: &str, is_full_uri: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        if URI_COMPONENT_UNESCAPED.contains(byte)
            || (is_full_uri && URI_EXTRA_UNESCAPED.contains(byte))
        {
            out.push(*byte as char);
        } else {
            // Percent-encode each byte of multi-byte UTF-8 chars too
            out.push('%');
            out.push(HEX_UPPER[(*byte >> 4) as usize] as char);
            out.push(HEX_UPPER[(*byte & 0x0F) as usize] as char);
        }
    }
    out
}

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

pub(super) fn uri_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

