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

use crate::bytecode::{FuncProto, Instr, MathFn};
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

/// Canonical NaN bits (`Value::num` canonicalises every NaN to this pattern so
/// raw TypedArray bytes can never alias a NaN-box tag). Mirror of value.rs QNAN.
const QNAN_BITS: u64 = 0x7FF8_0000_0000_0000;

/// Where a pinned TypedArray's live Value is RE-READ from at region entry and
/// after every user-code helper: a global slot (`g` never stored in the region)
/// or a frame register (never written in the region). The static choice is only
/// a HINT — every fast-path access re-checks object identity at runtime.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaPinSrc {
    Global(u32),
    Reg(u16),
}

/// One pinned TypedArray: its live-Value source and the element kind the inline
/// code was specialised for (the snapshot helper re-validates the kind).
/// `kind == DV_PIN_KIND` pins a DataView instead (snapshot: `base = data +
/// byteOffset`, `len = byteLength`) for the whitelisted `get*` methods.
#[derive(Clone, Copy)]
pub struct TaPin {
    pub src: TaPinSrc,
    pub kind: u8,
}

/// `TaPin::kind` marker for a pinned DataView receiver (not a TA element kind).
pub const DV_PIN_KIND: u8 = 255;

/// `TaPin::kind` marker for a pinned flat-ASCII STRING receiver (not a TA element
/// kind). Snapshot: `base = bytes.as_ptr()`, `len = units` (== byte len for ASCII),
/// for inlining `str.charCodeAt(i)` as a direct byte load. A non-ASCII / rope /
/// non-string snapshots all-zero (the per-access identity guard then misses and the
/// access takes the generic `jit_char_code_at` helper — full flatten/surrogate
/// semantics). ASCII-only is the correctness gate: byte i == UTF-16 unit i.
pub const STR_PIN_KIND: u8 = 254;

/// Compile-time plan for inline TypedArray element access in a memory-path
/// region: the pins (each gets a 32-byte stack snapshot slot `{obj_bits, base,
/// len}` filled by `jit_ta_snapshot`) and, per GetIndex/SetIndex ip, which pin
/// it should guard against. Built in dispatch.rs from LIVE VM state at OSR
/// compile time; empty for non-TA regions.
#[derive(Default)]
pub struct TaPinPlan {
    pub pins: Vec<TaPin>,
    pub access: FxHashMap<usize, u8>,
}

/// Q4 leaf-call inlining (v1). One inlinable monomorphic PLAIN-LEAF callee for a
/// `Call` site in a memory-path region. Built in dispatch.rs from LIVE VM state
/// (the resolved Callee IC entry) at OSR compile time; the emitted code guards
/// the callee register's identity against `callee_bits` and, on a hit, runs the
/// callee body INLINE (no frame push, no `setup_call`, no `run_loop`) over a
/// scratch register window carved at offset `reg_window` above the caller frame.
/// A guard MISS (callee reassigned / a different callee shape appears) falls
/// through to the UNCHANGED `emit_region_call_ic` helper — a pure prefix that
/// never deopts/evicts the region, so polymorphic / unknown callees degrade to
/// today's per-call path.
///
/// SOUNDNESS: the body is verified `callee_leaf_ok` — a straight-line sequence of
/// region-admissible value ops + global reads/writes, NO nested call / heap alloc
/// / closure-cell / upvalue op, exactly one trailing `Return`/`ReturnUndefined`,
/// and NO deopt-capable op after any effect (so an inlined-op bail — which
/// resumes the interpreter AT THE CALL IP and re-runs the whole call — can never
/// double-apply a `StoreGlobal`/`SetProp`). The body touches only the scratch
/// window (its own regs, offset by `reg_window`) and globals (`r12`); a pure leaf
/// runs no GC safepoint, so the carved window needs no zero-fill and the pinned
/// pointers (r12/r13/r14) stay valid across it.
pub struct LeafInlinePlan {
    /// Guard: the caller's callee register must hold exactly these Value bits.
    pub callee_bits: u64,
    /// Guard: the callee heap slot's live VERSION must still equal this baked
    /// value. Heap Value bits are pure `TAG_HEAP|idx`; the parallel `versions[]`
    /// array is the only ABA discriminator. A GC'd + reused slot keeps identical
    /// bits but bumps its version, so the bits-only guard would pass and run the
    /// STALE old callee body. The emitter checks `versions[idx] == callee_ver`
    /// AFTER the bits compare — restoring the exact `(bits, version)` tuple the
    /// interpreter's `ic_call` checks.
    pub callee_ver: u32,
    /// Offset (in registers) of the carved callee scratch window above the
    /// caller frame's window: callee reg `r` lives at caller reg `reg_window+r`.
    pub reg_window: u16,
    /// Callee register-window size (validated to fit the reserved capacity).
    pub callee_reg_count: u16,
    /// Callee formal parameter count (args 0..min(argc,param_count) are copied
    /// into scratch regs `reg_window+1 ..` ; reg `reg_window+0` is `this`).
    pub param_count: u16,
    /// The callee body ops to emit inline (with their OWN register numbers).
    pub body: Vec<Instr>,
    /// Resolved numeric-constant bits the body's `LoadConst` ops reference,
    /// keyed by the constant index (the callee's own constant pool index).
    pub consts: FxHashMap<u32, u64>,
}

impl LeafInlinePlan {
    /// Boxed bits for a body `LoadConst` constant (numeric — `callee_leaf_ok`
    /// rejected any non-numeric constant, and the planner pre-resolved them).
    fn const_bits(&self, idx: u32) -> u64 {
        self.consts.get(&idx).copied().unwrap_or(0)
    }
}

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
    /// Helper for `a + b` (`StrConcat`) — returns the result bits, or deopt.
    pub concat: usize,
    /// Helper for in-place `a + b` (`StrAppendInPlace`) — returns the result bits.
    pub str_append: usize,
    /// Helper for a generic `obj.m(args…)` (`CallMethod`) in a region: consults
    /// the interpreter's per-site inline cache and frame-calls the resolved
    /// plain user function to completion. Returns the result bits,
    /// `SELF_CALL_DEOPT` (IC miss / megamorphic / depth limit → re-execute the
    /// op in the interpreter), or `CALL_THREW` (exception pending → the region
    /// exits and the interpreter unwinds WITHOUT re-executing the call).
    pub call_method_ic: usize,
    /// Helper for a generic `f(args…)` (`Call`) in a region (same protocol).
    pub call_ic: usize,
    /// Helper for a `GetProp` the miss helper routed `PROP_VIA_IC` (accessor /
    /// class-instance receiver): interpreter-IC resolution + getter frame
    /// call. Returns the value bits / `SELF_CALL_DEOPT` / `CALL_THREW`.
    pub get_prop_slow: usize,
    /// The `SetProp` sibling of `get_prop_slow` (setter frame call; 0 = done).
    pub set_prop_slow: usize,
    /// Helper for a region `===`/`!==` whose operands are non-interned heap
    /// values: full `strict_eq` (read-only; returns 0/1, never deopts).
    pub strict_eq: usize,
    /// Helper for a region truthiness test on a non-Int/Bool value (`!x`,
    /// `JumpIfFalse/True` conditions): full `truthy` (read-only; 0/1).
    pub truthy: usize,
    /// Helper that (re)derives a pinned TypedArray's `{obj_bits, base, len}`
    /// snapshot into a region stack slot (no alloc, no user code).
    pub ta_snapshot: usize,
    /// Helper for a Uint8Clamped store of a DOUBLE value: round-half-even
    /// clamp + byte store (pure — no vm, no alloc).
    pub ta_clamp_store: usize,
    /// Helper for a whitelisted `dv.get*(pos[, le])` DataView read (no alloc,
    /// no user code; anything unusual returns the deopt sentinel).
    pub dv_get: usize,
    /// Helper for a UNARY `Math.<op>` over an already-numeric arg (pure: the
    /// region guards the arg numeric and passes its raw f64 bits). Returns the
    /// result's f64 bits. No vm, no alloc, no user code.
    pub math_unary: usize,
    /// Helper for a TWO-ARG `Math.<op>` (Pow/Atan2/Imul/Min/Max/Hypot) over
    /// already-numeric args (pure, same constraints). Returns f64 bits.
    pub math_two: usize,
    /// Helper for `CellGet` / `UpvalGet` reading a captured-local cell: a pure
    /// heap LOAD of the cell's inner Value (no alloc, no user code). Returns the
    /// inner Value bits, or `SELF_CALL_DEOPT` for a still-uninitialized (TDZ)
    /// cell so the region bails and the interpreter throws.
    pub cell_get: usize,
    /// Helper for `UpvalGet` (resolves the running closure's k-th upvalue cell,
    /// then loads it). Same purity/TDZ contract as `cell_get`.
    pub upval_get: usize,
    /// Helper for `ForInLive` (per-iteration for-in liveness). Delegates to the
    /// shared `Vm::forin_live`, so it matches the interpreter byte-for-byte.
    /// May allocate transiently (key re-derivation) — GC-guarded internally.
    /// Returns the BOOL Value bits.
    pub forin_live: usize,
    /// Helper for `HasProp` (the `in` operator, brand=false) over a non-Proxy
    /// chain: read-only `Vm::has_property_jit`. Returns the BOOL Value bits, or
    /// `SELF_CALL_DEOPT` when the op needs user code / a throw (interpreter only).
    pub has_property: usize,
    /// Helper for the Q4 leaf-inline ENTRY headroom check (`jit_regs_fits`):
    /// returns 1 when a carved callee scratch window fits the pinned register
    /// file. Called once per OSR entry when the region has any inlined call.
    pub regs_fits: usize,
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

/// Ways per JIT inline-cache site (matches the interpreter's `IC_WAYS`): a
/// region `GetProp`/`SetProp` probes up to this many entries natively before
/// falling to the miss helper, so polymorphic shape-cycling loops stay call-free.
pub const JIT_IC_WAYS: usize = 8;
/// Byte stride of one [`IcEntry`] (`size_of`, kept explicit for the emitter).
pub const JIT_IC_STRIDE: usize = 64;
/// Maximum proto-chain hops one [`IcEntry`] can guard (the interpreter's
/// `IC_MAX_HOPS` is 6; 5 fits the 64-byte entry and covers depth-5 chains —
/// the deepest the real-world benches walk).
pub const JIT_IC_MAX_HOPS: usize = 5;

/// One way of a JIT'd `GetProp`/`SetProp` site's inline cache. `repr(C)` with a
/// fixed layout the native code indexes directly: `obj_bits @0`, `vals_ptr @8`,
/// `version @16`, `slot|nhops<<24 @20`, then `nhops` pairs `(hop_idx @24+8k,
/// hop_ver @28+8k)` (stride 64). The native probe checks `obj_bits` (receiver
/// identity) AND the receiver's live `version` (read from the heap's parallel
/// version array — catches own key add/remove/redefine, freeze, and
/// setPrototypeOf, all of which bump it) — then, for a PROTO-CHAIN entry
/// (`nhops > 0`), the live version of each chain hop down to the holder (whose
/// `vals_ptr`/`slot` the hit reads); any chain mutation bumps a guarded hop.
/// Writes only ever fill OWN entries (`nhops == 0`). On a full match it
/// reads/writes `vals_ptr[slot]` with NO call (slots never move without a
/// version bump). `obj_bits == 0` means empty (no real object Value is 0).
/// `slot` is packed into 24 bits (an ObjMap with 16M+ keys is unreachable —
/// fills exceeding it are skipped defensively).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IcEntry {
    pub obj_bits: u64,
    pub vals_ptr: u64,
    pub version: u32,
    pub slot_nhops: u32,
    pub hops: [(u32, u32); JIT_IC_MAX_HOPS],
}

impl IcEntry {
    /// An OWN-property way (receiver == holder; no hop guards). `None` if the
    /// slot doesn't fit the 24-bit packing (never in practice).
    pub fn own(obj_bits: u64, vals_ptr: u64, version: u32, slot: u32) -> Option<IcEntry> {
        if slot > 0x00FF_FFFF {
            return None;
        }
        Some(IcEntry {
            obj_bits,
            vals_ptr,
            version,
            slot_nhops: slot,
            hops: [(0, 0); JIT_IC_MAX_HOPS],
        })
    }

    /// A PROTO-CHAIN way: the holder is `hops.len()` (1..=5) hops from the
    /// receiver; every hop is version-guarded.
    pub fn chain(
        obj_bits: u64,
        vals_ptr: u64,
        version: u32,
        slot: u32,
        hops: &[(u32, u32)],
    ) -> Option<IcEntry> {
        if slot > 0x00FF_FFFF || hops.is_empty() || hops.len() > JIT_IC_MAX_HOPS {
            return None;
        }
        let mut h = [(0u32, 0u32); JIT_IC_MAX_HOPS];
        h[..hops.len()].copy_from_slice(hops);
        Some(IcEntry {
            obj_bits,
            vals_ptr,
            version,
            slot_nhops: slot | ((hops.len() as u32) << 24),
            hops: h,
        })
    }
}

/// Dense function-tier states (see `Jit::fn_state`).
pub const FN_COLD: u8 = 0;
pub const FN_COMPILED: u8 = 1;
pub const FN_DEAD: u8 = 2;

/// Per-function JIT state: call counts, compiled code, and a blacklist of
/// functions that aren't eligible (so we don't re-attempt them every tick).
/// The `region_*` maps mirror this for OSR loop regions, keyed by
/// `(func_id, loop_header_ip)`.
#[derive(Default)]
pub struct Jit {
    counts: FxHashMap<u32, u32>,
    compiled: FxHashMap<u32, JitFn>,
    blacklist: FxHashSet<u32>,
    /// Dense per-func_id tier state ([`FN_COLD`]/[`FN_COMPILED`]/[`FN_DEAD`]),
    /// grown on demand. One array read on EVERY interpreted frame entry
    /// replaces the 2-3 hash probes (`compiled` miss + `blacklist` hit) that
    /// otherwise tax each call to a never-compiled function.
    fn_state: Vec<u8>,
    regions: FxHashMap<(u32, u32), Region>,
    region_counts: FxHashMap<(u32, u32), u32>,
    region_blacklist: FxHashSet<(u32, u32)>,
    /// Loop headers where the INTEGER path was tried and deoptimised; the next
    /// compile for the key skips int and uses the double path instead.
    region_int_blacklist: FxHashSet<(u32, u32)>,
    /// Inline-cache ways for heap-op JIT sites: site `k` owns the contiguous
    /// entries `[k*JIT_IC_WAYS, (k+1)*JIT_IC_WAYS)`. Grows only at compile time
    /// (never during a native run, EXCEPT through a region call helper — after
    /// which the region re-derives its pinned base pointer); a `*_miss` helper
    /// only UPDATES existing ways (no growth).
    ic_table: Vec<IcEntry>,
    /// Round-robin fill cursor per site (parallel to `ic_table` / JIT_IC_WAYS).
    ic_rot: Vec<u8>,
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
    /// Evicted regions parked here instead of being dropped. A region can be
    /// evicted REENTRANTLY (its `jit_call_*_ic` helper runs user code, which can
    /// loop back into the SAME region and deopt it past the limit) while an
    /// outer activation of that region is still executing on the native stack —
    /// dropping the `ExecutableBuffer` then would unmap code we're inside.
    /// Parked regions are freed only when the VM drops. Bounded: each loop key
    /// evicts at most twice (int retry, then blacklist).
    retired: Vec<Region>,
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

    /// Dense tier state of `func_id` — the frame-entry fast path.
    #[inline]
    pub fn fn_state(&self, func_id: u32) -> u8 {
        self.fn_state.get(func_id as usize).copied().unwrap_or(FN_COLD)
    }

    fn set_fn_state(&mut self, func_id: u32, s: u8) {
        let i = func_id as usize;
        if self.fn_state.len() <= i {
            self.fn_state.resize(i + 1, FN_COLD);
        }
        self.fn_state[i] = s;
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
                self.set_fn_state(func_id, FN_COMPILED);
            }
            None => {
                self.blacklist.insert(func_id);
                self.set_fn_state(func_id, FN_DEAD);
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
        // Called only after a `get_region` miss on the same back-edge, so
        // `regions` cannot contain the key — only the blacklist gates. This
        // keeps the per-back-edge cost of a permanently-rejected loop to one
        // hash probe (plus the counter bump until it is blacklisted).
        if self.region_blacklist.contains(&key) {
            return false;
        }
        let c = self.region_counts.entry(key).or_insert(0);
        *c += 1;
        *c == OSR_THRESHOLD
    }

    /// Permanently blacklist the region headed at `entry_ip` (the dispatch-side
    /// call-mix gate found it would lose to the interpreter — e.g. dominated by
    /// always-fallback native-callee sites).
    pub fn blacklist_region(&mut self, func_id: u32, entry_ip: u32) {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] region fn{func_id} [{entry_ip}] DECLINED (call-mix gate)");
        }
        self.region_blacklist.insert((func_id, entry_ip));
    }

    /// Undo the threshold trip reported by [`Jit::record_region`]: the caller
    /// found the region not YET safe to compile (an uninitialized global in
    /// the body) but expects it to become safe — re-arm so a later back-edge
    /// re-trips the threshold and re-checks.
    pub fn region_defer(&mut self, func_id: u32, entry_ip: u32) {
        if let Some(c) = self.region_counts.get_mut(&(func_id, entry_ip)) {
            *c -= 1;
        }
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
        const_strs: &FxHashMap<u32, u64>,
        ta_plan: &TaPinPlan,
        leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
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
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] INT region fn{func_id} [{start},{end}] compiled");
                }
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
            concat: heap_helpers.concat,
            str_append: heap_helpers.str_append,
            call_method_ic: heap_helpers.call_method_ic,
            call_ic: heap_helpers.call_ic,
            get_prop_slow: heap_helpers.get_prop_slow,
            set_prop_slow: heap_helpers.set_prop_slow,
            strict_eq: heap_helpers.strict_eq,
            truthy: heap_helpers.truthy,
            ta_snapshot: heap_helpers.ta_snapshot,
            ta_clamp_store: heap_helpers.ta_clamp_store,
            dv_get: heap_helpers.dv_get,
            math_unary: heap_helpers.math_unary,
            math_two: heap_helpers.math_two,
            cell_get: heap_helpers.cell_get,
            upval_get: heap_helpers.upval_get,
            forin_live: heap_helpers.forin_live,
            has_property: heap_helpers.has_property,
            regs_fits: heap_helpers.regs_fits,
            ic_base_idx,
        };
        match compile_region(proto, start, end, globals_base_helper, helpers, const_strs, ta_plan, leaf_plan) {
            Some(code) => {
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] DOUBLE/MEM region fn{func_id} [{start},{end}] compiled");
                }
                self.regions
                    .insert(key, Region { code, start, end, deopts: 0, is_int: false, field_plan: None });
            }
            None => {
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] region fn{func_id} [{start},{end}] DECLINED (blacklisted)");
                }
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
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] region fn{} [{}] deopt at ip {}", key.0, key.1, resume_ip);
                }
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
            if std::env::var_os("ZIPP_JITLOG").is_some() {
                eprintln!("[jit] region fn{} [{}] EVICTED (retry={retry})", key.0, key.1);
            }
            // Park, don't drop: an outer activation of this region may still be
            // running (a call helper re-entered the interpreter, which looped
            // back into the region and deopted it) — see `retired`.
            if let Some(r) = self.regions.remove(&key) {
                self.retired.push(r);
            }
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

    /// Reserve `n` fresh inline-cache sites (one per heap-op site in a region;
    /// `JIT_IC_WAYS` ways each), returning the base global site id. The ways
    /// start empty (`obj_bits == 0` ⇒ always miss on first use).
    pub fn reserve_ic_sites(&mut self, n: usize) -> u32 {
        let base = (self.ic_table.len() / JIT_IC_WAYS) as u32;
        self.ic_table
            .resize(self.ic_table.len() + n * JIT_IC_WAYS, IcEntry::default());
        self.ic_rot.resize(self.ic_rot.len() + n, 0);
        base
    }

    /// Base pointer of the inline-cache table (for a region prologue to pin).
    pub fn ic_base_ptr(&self) -> *const IcEntry {
        self.ic_table.as_ptr()
    }

    /// Fill one way of inline-cache site `site` after a miss (called by the
    /// `*_miss` helpers): an existing way for the same receiver identity is
    /// updated in place, else the next empty way, else round-robin eviction.
    /// Never grows the table (the site was reserved at compile time), so a
    /// pinned base pointer in a running region stays valid. `site == u32::MAX`
    /// (the hoisted-`.length` pseudo-site) is ignored.
    pub fn set_ic(&mut self, site: u32, e: IcEntry) {
        let base = (site as usize).wrapping_mul(JIT_IC_WAYS);
        let Some(ways) = self.ic_table.get_mut(base..base + JIT_IC_WAYS) else {
            return;
        };
        if let Some(w) = ways.iter_mut().find(|w| w.obj_bits == e.obj_bits) {
            *w = e;
            return;
        }
        if let Some(w) = ways.iter_mut().find(|w| w.obj_bits == 0) {
            *w = e;
            return;
        }
        let r = &mut self.ic_rot[site as usize];
        ways[*r as usize % JIT_IC_WAYS] = e;
        *r = r.wrapping_add(1);
    }
}

impl Region {
    /// Raw native entry pointer (into the region's mmap'd `ExecutableBuffer`,
    /// which never moves — stable across `regions`-map rehashes and, via
    /// `Jit::retired`, even across an eviction of a still-running region).
    /// The caller invokes it with the `JitFn::run` ABI; the returned bail slot
    /// is the ip to resume interpreting at.
    pub fn entry(&self) -> *const u8 {
        self.code.entry()
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

/// Is `reg`'s value at `ip` known to be an Int from a preceding int-producing op?
/// Finds the nearest backward writer of `reg`; it must be one of
/// `LoadInt`/`AddInt`/`Add`/`Sub`/`Mul`/`Mod` (each yields a boxed Int natively,
/// guarding its operands and bailing otherwise — so reaching `ip` natively proves
/// the value is an Int). SOUNDNESS: the writer must also DOMINATE the use along
/// the only entry path — i.e. no jump may land in `(writer_ip, ip]`, which would
/// let control reach `ip` bypassing the writer (possibly with a non-int value).
/// Conservative: returns false on any doubt. Lets the base-case-inline decision
/// skip a redundant int guard on `fib`'s `n-1` / `n-2` arguments.
fn arg_is_known_int(code: &[Instr], ip: usize, reg: u16) -> bool {
    let mut writer = None;
    for j in (0..ip).rev() {
        if let Some(w) = writes_reg(&code[j]) {
            if w == reg {
                writer = Some(j);
                break;
            }
        }
    }
    let w = match writer {
        Some(w) => w,
        None => return false,
    };
    let int_producing = matches!(
        &code[w],
        Instr::LoadInt { .. }
            | Instr::AddInt { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Mod { .. }
    );
    if !int_producing {
        return false;
    }
    // No branch may jump into (w, ip]: such an edge could reach `ip` without
    // executing the writer at `w`.
    for instr in code {
        let target = match *instr {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target as usize,
            _ => continue,
        };
        if target > w && target <= ip {
            return false;
        }
    }
    true
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
        | Instr::StrConcat { dst, .. }
        | Instr::StrAppendInPlace { dst, .. }
        | Instr::Bitwise { dst, .. }
        | Instr::Call { dst, .. } => Some(dst),
        _ => None,
    }
}

/// If this single-parameter function opens with a base case of the shape
/// `if (param <cmp> K) return param;` — i.e. it returns its argument UNCHANGED
/// for small inputs — report `(cmp, K)`. A self-call to such a function can then
/// inline the base case at the call site (`arg <cmp> K ? arg : recurse`),
/// eliminating the call + prologue/epilogue for every LEAF invocation (about
/// half of `fib`'s calls). The recognised shape is exactly what `fib` compiles
/// to (LoadInt K; compare param,K; JumpIfFalse; Move/Jump…; Return param); any
/// deviation returns `None`, so the optimization is opt-in and never wrong.
fn base_case_returns_arg(proto: &FuncProto) -> Option<(Cmp, i32)> {
    if proto.param_count != 1 {
        return None;
    }
    let code = &proto.code;
    if code.len() < 3 {
        return None;
    }
    // ip0: LoadInt{c, K}
    let (c, k) = match code[0] {
        Instr::LoadInt { dst, val } => (dst, val),
        _ => return None,
    };
    // ip1: compare param (reg 1) against c → t. The reported Cmp is the one whose
    // TRUE branch selects the base case.
    let (cmp, t) = match code[1] {
        Instr::Lt { dst, a: 1, b } if b == c => (Cmp::Lt, dst),
        Instr::Le { dst, a: 1, b } if b == c => (Cmp::Le, dst),
        Instr::Gt { dst, a: 1, b } if b == c => (Cmp::Gt, dst),
        Instr::Ge { dst, a: 1, b } if b == c => (Cmp::Ge, dst),
        _ => return None,
    };
    // ip2: JumpIfFalse{t, _} — when (param<cmp>K) is FALSE we leave for the
    // recursive body, so the base case is the FALL-THROUGH (ip3).
    match code[2] {
        Instr::JumpIfFalse { cond, .. } if cond == t => {}
        _ => return None,
    }
    // Base path from ip3: follow Move/Jump to a Return whose source traces back
    // to the param (reg 1). Bounded walk; any other op disqualifies.
    let mut ip = 3usize;
    let mut ret_reg: u16 = 1; // register currently holding the (copied) param
    for _ in 0..8 {
        match code.get(ip)? {
            Instr::Move { dst, src } => {
                if *src == ret_reg {
                    ret_reg = *dst;
                } else if *dst == ret_reg {
                    return None; // our tracked value was overwritten
                }
                ip += 1;
            }
            Instr::Jump { target } => ip = *target as usize,
            Instr::Return { src } => {
                return if *src == ret_reg { Some((cmp, k)) } else { None };
            }
            _ => return None,
        }
    }
    None
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
    // If the callee (== self for our self-calls) returns its argument unchanged
    // for small inputs (`fib`: `n<2 ? n`), inline that base case at each call
    // site so leaf invocations skip the call + prologue/epilogue entirely.
    let base_case = base_case_returns_arg(proto);
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
            Instr::AddInt { dst, a, imm, .. } => {
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
            Instr::LoadGlobal { .. } => {
                // `LoadGlobal(self_slot)` (can_compile gated) — only ever the
                // callee load of a self-`Call`. The native fast path calls the
                // entry DIRECTLY and never reads this register, so materialising
                // the self-Value here is dead work on every call (2 instr × ~96M
                // for fib(37)). SKIP it; `emit_self_call` lazily writes the callee
                // register on its cold (interpreter-bound) paths, where the
                // interpreter resume actually reads it. No-op here.
            }
            Instr::Call { dst, callee, arg_base, argc } => {
                // Self-recursive call (can_compile verified callee == self_slot).
                // Fast path: a DIRECT native call to this function's own entry
                // with an inline depth guard — no Rust trampoline. Cold paths
                // (depth limit, or the callee bailed mid-body) route to the Rust
                // helper / interpreter, which read `regs[callee]` — so they
                // restore it from `self_val_bits` first (the skipped LoadGlobal).
                //
                // BASE-CASE INLINING: when the callee returns its argument
                // unchanged for small inputs (`base_case`), test the guard here
                // and produce the result inline for the leaf case — no call. Only
                // for argc==1 (the recognised shape). A non-int arg or the
                // recursive case routes to the real call.
                match base_case {
                    Some((cmp, k)) if argc == 1 => {
                        let do_call = ops.new_dynamic_label();
                        let inline_base = ops.new_dynamic_label();
                        let after = ops.new_dynamic_label();
                        // Non-int arg → real call (which guards + bails correctly).
                        // Skip the guard when the arg provably came from an
                        // int-producing op (`fib`'s `n-1`/`n-2` from AddInt).
                        if !arg_is_known_int(&proto.code, ip, arg_base) {
                            guard_int(&mut ops, arg_base, do_call);
                        }
                        dynasm!(ops
                            ; mov eax, [rbx + dreg(arg_base)]   // arg payload (i32)
                            ; cmp eax, k
                        );
                        // Jump to the inline base case when `arg <cmp> k` is TRUE.
                        match cmp {
                            Cmp::Lt => dynasm!(ops ; jl => inline_base),
                            Cmp::Le => dynasm!(ops ; jle => inline_base),
                            Cmp::Gt => dynasm!(ops ; jg => inline_base),
                            Cmp::Ge => dynasm!(ops ; jge => inline_base),
                            Cmp::Eq | Cmp::Ne => unreachable!("base_case yields only Lt/Le/Gt/Ge"),
                        }
                        dynasm!(ops ; => do_call);
                        emit_self_call(
                            &mut ops, ip, bail, self_entry, self_func_id, self_call_helper, dst,
                            callee, arg_base, argc, proto.reg_count, self_val_bits,
                        );
                        dynasm!(ops
                            ; jmp => after
                            ; => inline_base
                            ; mov rax, [rbx + dreg(arg_base)]   // result = arg (base returns it)
                            ; mov [rbx + dreg(dst)], rax
                            ; => after
                        );
                    }
                    _ => {
                        emit_self_call(
                            &mut ops, ip, bail, self_entry, self_func_id, self_call_helper, dst,
                            callee, arg_base, argc, proto.reg_count, self_val_bits,
                        );
                    }
                }
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
#[derive(Clone, Copy, PartialEq, Eq)]
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

/// THREW sentinel the region call helpers (`jit_call_method_ic` / `jit_call_ic`)
/// return when the frame-called function THREW: the call's side effects already
/// happened and `pending_throw` is set, so the region must exit and the
/// interpreter must UNWIND (never re-execute the call op). Distinct from
/// `SELF_CALL_DEOPT`, which means "nothing happened yet — redo in the
/// interpreter". Like the deopt sentinel, a quiet-NaN pattern no boxed `Value`
/// produces.
pub const CALL_THREW: u64 = 0x7FFE_DEAD_BEEF_0001;

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
fn emit_self_call(
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
        Instr::AddInt { dst, a, imm, .. } => {
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
/// in range must be numeric/control-flow with no closure op, and any `LoadConst`
/// must reference a numeric constant, a single-ASCII-char string, or (MEM path
/// only — `const_strs` is `Some`) a string constant pre-interned at compile time
/// whose bits the emitter embeds (`const_strs` maps constant index → bits).
fn region_can_compile(
    proto: &FuncProto,
    start: u32,
    end: u32,
    const_strs: Option<&FxHashMap<u32, u64>>,
) -> bool {
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
            // A `let`/`const` global write (TDZ-checked); inside a hot loop region the
            // binding is already initialized, so the JIT treats it like StoreGlobal.
            | Instr::StoreGlobalStrict { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Div { .. }
            | Instr::Mod { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
            // Bitwise ops (`|`/`&`/`^`/`<<`/`>>`/`>>>`) — handled by the MEMORY
            // path (Int or exactly-integral-double operands; anything else
            // bails). The `(x + y) | 0` / `i & 7` idioms gate most real
            // object/method loops, so regions must admit them.
            | Instr::Bitwise { .. }
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
            // mem path). A `Print`/etc. anywhere still rejects the region.
            | Instr::GetProp { .. }
            | Instr::SetProp { .. }
            // Dense-array element read/write `a[i]` / `a[i]=v` — handled by the
            // MEMORY path via win64 helpers (the int/regalloc paths decline).
            | Instr::GetIndex { .. }
            | Instr::SetIndex { .. }
            // Read-modify-write key coercion (`o[k] += v`, `o[k]++`): a NUMBER
            // key on a non-nullish base is a plain move (the MEMORY path's
            // inline case); anything else bails to the interpreter.
            | Instr::ToPropKey { .. }
            // String concat (`s += …`) — handled by the MEMORY path via the
            // `jit_concat` / `jit_str_append` win64 helpers (the numeric
            // int/regalloc paths don't list them, so they decline → mem path).
            | Instr::StrConcat { .. }
            | Instr::StrAppendInPlace { .. }
            | Instr::Return { .. }
            | Instr::ReturnUndefined => {}
            // Method calls — handled by the MEMORY path. `arr.push(x)` /
            // `str.charCodeAt(i)` keep their dedicated win64 helpers; every
            // other `obj.m(…)` compiles to a `jit_call_method_ic` helper call
            // that consults the interpreter's per-site inline cache and
            // frame-calls the resolved plain user function (IC miss /
            // megamorphic / native callee → deopt to the interpreter at this
            // op; repeated deopts evict the region).
            Instr::CallMethod { .. } => {}
            // Plain calls `f(…)` — same protocol via `jit_call_ic`.
            Instr::Call { .. } => {}
            // Logical `!` — MEM path (Bool flips natively; anything else goes
            // through the `jit_truthy` helper).
            Instr::Not { .. } => {}
            // `Math.<op>(args…)` — MEM path. A 1-arg unary op (`abs`/`sqrt`/
            // `floor`/`sin`/…) loads its arg as a number (bails to the
            // interpreter — which runs ToNumber coercion — if not) and calls the
            // PURE `jit_math_unary` helper (the interpreter's exact `math_unary`,
            // so every JS quirk matches). A 2-arg op (`pow`/`atan2`/`imul`/
            // `min`/`max`/`hypot` with EXACTLY two args) uses `jit_math_two`.
            // Any other arity (variadic min/max/hypot, a 0-arg call) declines —
            // the interpreter handles it. The helpers run no user code and never
            // allocate (a non-numeric arg already bailed), so no pinned-pointer
            // re-fetch is needed.
            Instr::MathOp { op, argc, .. } => {
                let ok = match argc {
                    // `Math.imul(x)` (one arg) diverges: the unary helper returns
                    // NaN, but the interpreter coerces the missing 2nd arg to
                    // `to_uint32(NaN)==0` and yields 0. Decline so the interpreter
                    // runs it (every other unary op agrees at argc==1).
                    1 => !matches!(op, MathFn::Imul),
                    2 => matches!(
                        op,
                        MathFn::Pow
                            | MathFn::Atan2
                            | MathFn::Imul
                            | MathFn::Min
                            | MathFn::Max
                            | MathFn::Hypot
                    ),
                    _ => false,
                };
                if !ok {
                    if std::env::var_os("ZIPP_JITDUMP").is_some() {
                        eprintln!("[decline] MathOp arity {argc} op {op:?} at region [{start},{end}]");
                    }
                    return false;
                }
            }
            // `LoadBool` — materialise the boolean Value bits inline (a single
            // store; call-free, pure). Unblocks loops carrying a bool literal
            // (parser flags, `done=false`).
            Instr::LoadBool { .. } => {}
            // Closure-cell / upvalue READS — MEM path via the pure `jit_cell_get`
            // / `jit_upval_get` helpers (a single heap LOAD of the cell's inner
            // Value; a TDZ cell → deopt sentinel → interpreter throws). Emitted
            // PER-OP (never hoisted across a Call/CallMethod), so a value an inner
            // closure mutated via a call in the SAME region is re-read on the next
            // execution. The helpers allocate nothing and run no user code, so no
            // pinned-pointer (r13/r14/TA) re-fetch is needed. Writes (`CellSet`,
            // `CellSetChecked`, `UpvalSet`) are NOT admitted — they keep declining.
            Instr::CellGet { .. } | Instr::UpvalGet { .. } => {}
            // `ForInLive` — the per-iteration for-in liveness check — MEM path via
            // the `jit_forin_live` helper (the shared `Vm::forin_live`; no getter
            // / Proxy trap fires, never re-enters the dispatch loop, so no GC safe
            // point — and it is GC-locked internally for belt-and-suspenders).
            // Emitted per-op (re-derives the live shape each execution). Lets
            // `for (k in obj)` loops over plain objects compile.
            Instr::ForInLive { .. } => {}
            // `HasProp` — the `in` operator — MEM path via the `jit_has_property`
            // helper (read-only `Vm::has_property_jit`, byte-identical to the
            // interpreter's `has_property_dyn` on a non-Proxy chain). Only a plain
            // `in` (`brand: false`) is admitted; the `#x in obj` ergonomic brand
            // check needs the private machinery → keeps declining. The helper runs
            // no user code and never allocates on the VM heap (a Proxy/exotic/
            // throwing case returns the deopt sentinel and the interpreter takes
            // over), so no r13/r14/TA refetch. Unblocks sparse-array's 8M
            // hole-aware `if (i in packed)` loops.
            Instr::HasProp { brand: false, .. } => {}
            Instr::HasProp { brand: true, .. } => {
                if std::env::var_os("ZIPP_JITDUMP").is_some() {
                    eprintln!("[decline] HasProp brand-check at region [{start},{end}]");
                }
                return false;
            }
            Instr::LoadConst { idx, .. } => {
                // Numeric constants run in the f64 region; a single-ASCII-char
                // string constant is resolvable to its interned slot (for
                // `s[i] === "x"` scans); a multi-char string constant is
                // accepted on the MEM path when its pre-interned bits are in
                // `const_strs`. Anything else rejects the region.
                match proto.constants.get(idx as usize) {
                    Some(c) if c.is_number() => {}
                    Some(&c) if single_char_const_bits(proto, c).is_some() => {}
                    Some(_) if const_strs.is_some_and(|m| m.contains_key(&idx)) => {}
                    _ => {
                        if std::env::var_os("ZIPP_JITDUMP").is_some() {
                            eprintln!("[decline] non-region LoadConst at region [{start},{end}]");
                        }
                        return false;
                    }
                }
            }
            ref other => {
                if std::env::var_os("ZIPP_JITDUMP").is_some() {
                    eprintln!("[decline] {other:?} at region [{start},{end}]");
                }
                return false;
            }
        }
    }
    // NOTE: helpers that can allocate (`StrConcat`/`StrAppendInPlace`) or run
    // user code (`Call`/`CallMethod`) USED to be forbidden alongside
    // GetProp/SetProp because the inline cache pins the heap version-array
    // pointer (r13) and the IC table pointer (r14), which an allocation /
    // a nested region compile can move. The memory path now RE-FETCHES those
    // pinned pointers after every such helper call instead (see
    // `emit_refetch_pinned`), so the mix is allowed.
    true
}

/// Q4 v1 leaf-call inlining eligibility: is `callee`'s body a PLAIN LEAF the
/// region emitter can inline straight-line over a scratch window? Returns the
/// body ops to inline, or `None` to decline (the Call keeps the per-call helper).
///
/// Requirements (all NON-NEGOTIABLE for soundness — see `LeafInlinePlan`):
/// * Not a generator/async; reg_count ≤ 16; no rest/`arguments`; simple_params
///   (so arg binding is a plain positional copy, no defaults/destructuring).
/// * Body ops ⊂ a SAFE SUBSET of the region-admissible value/global ops, minus
///   anything that calls (`Call`/`CallMethod`/`Super*`), allocates on the VM
///   heap, reads a closure cell / upvalue (`Cell*`/`Upval*`), or touches the
///   `arguments`/heap property machinery. The subset below is exactly what the
///   inline emitter implements; any other op declines.
/// * Exactly ONE trailing `Return`/`ReturnUndefined`, reached by fall-through —
///   NO internal jump (straight-line; the inline emitter has no branch labels).
/// * NO deopt-capable op may appear AFTER an effect (`StoreGlobal*`): if an
///   inlined op bails, the interpreter re-runs the WHOLE call from the call ip,
///   so an effect that already ran would double-apply. (For v1 the only effect
///   admitted is `StoreGlobal*`; `SetProp`/`SetIndex` are NOT in the subset.)
pub fn callee_leaf_ok(callee: &FuncProto) -> Option<Vec<Instr>> {
    if callee.is_generator || callee.is_async {
        return None;
    }
    if callee.rest_reg.is_some() || callee.arguments_reg.is_some() {
        return None;
    }
    if !callee.simple_params {
        return None;
    }
    if callee.reg_count > 16 {
        return None;
    }
    let full = &callee.code;
    if full.is_empty() {
        return None;
    }
    // The straight-line body ends at the FIRST `Return`/`ReturnUndefined` (the
    // body must have no branch before it, so that return is the unique exit; any
    // op after it is dead — the compiler routinely appends a `ReturnUndefined`
    // after an explicit `Return`). Truncate the body there.
    let term = full
        .iter()
        .position(|i| matches!(i, Instr::Return { .. } | Instr::ReturnUndefined))?;
    let code: Vec<Instr> = full[..=term].to_vec();
    // Every op except the terminator must be a non-control-flow value/global op.
    // `seen_effect` enforces the side-effect-freedom-before-deopt ordering rule.
    let mut seen_effect = false;
    for (i, instr) in code.iter().enumerate() {
        let is_last = i == code.len() - 1;
        match *instr {
            // The single trailing return — IS the last op (by construction above).
            Instr::Return { .. } | Instr::ReturnUndefined => {
                debug_assert!(is_last);
            }
            _ if is_last => return None, // unreachable: last op is the terminator
            // ── deopt-capable value ops (may bail mid-body) ── forbidden AFTER
            // an effect (a bail would re-run the call and re-apply the effect).
            Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Div { .. }
            | Instr::Mod { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
            | Instr::Bitwise { .. } => {
                if seen_effect {
                    return None;
                }
            }
            // The inline emitter only implements the MathOp arities the region
            // path does (1-arg, or a fixed 2-arg op set).
            Instr::MathOp { op, argc, .. } => {
                if seen_effect {
                    return None;
                }
                let ok = match argc {
                    // See region_can_compile: `Math.imul(x)` (one arg) diverges
                    // (unary helper → NaN, interpreter → 0). Decline this leaf.
                    1 => !matches!(op, MathFn::Imul),
                    2 => matches!(
                        op,
                        MathFn::Pow
                            | MathFn::Atan2
                            | MathFn::Imul
                            | MathFn::Min
                            | MathFn::Max
                            | MathFn::Hypot
                    ),
                    _ => false,
                };
                if !ok {
                    return None;
                }
            }
            // ── pure, never-bail value/load ops ── safe anywhere.
            Instr::LoadInt { .. }
            | Instr::LoadConst { .. }
            | Instr::LoadBool { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. } => {}
            // ── the one admitted effect ── a global write. Must be the only
            // kind of effect; after it, no deopt-capable op may follow.
            Instr::StoreGlobal { .. } | Instr::StoreGlobalStrict { .. } => {
                seen_effect = true;
            }
            // Anything else (calls, heap props, branches, closures, throw, …)
            // declines — the inline emitter doesn't implement it.
            _ => return None,
        }
    }
    // A `LoadConst` must be a NUMERIC constant (the inline emitter materialises
    // it as raw bits; a string/heap constant needs interning we don't do here).
    for instr in &code {
        if let Instr::LoadConst { idx, .. } = *instr {
            match callee.constants.get(idx as usize) {
                Some(c) if c.is_number() => {}
                _ => return None,
            }
        }
    }
    Some(code)
}

/// Detect a loop-invariant `g.length` to hoist out of a memory-path region: a
/// `GetProp{obj, name:"length"}` whose object is loaded from a global `g` that the
/// region never mutates (no `StoreGlobal(g)`, and no length-changing op anywhere —
/// `push`, `SetIndex`, `SetProp`). Then `g.length` is the same every iteration, so
/// it can be computed ONCE in the prologue rather than re-read (a helper call) per
/// iteration — the `for (i < s.length)` / `for (i < a.length)` idiom. Returns
/// `(get_ip, dst_reg, global_slot, name_idx)`, or `None` if no single such GetProp
/// qualifies (only the unique-GetProp case is hoisted, to keep it simple/safe).
fn hoistable_length(proto: &FuncProto, start: u32, end: u32) -> Option<(usize, u16, u32, u32)> {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    // The region must not change any container's length. A generic call
    // (`Call`, or any `CallMethod` other than the read-only `charCodeAt`)
    // can run ARBITRARY user code — which may mutate the container's length
    // or reassign the global holding it — so it rejects the hoist outright
    // (the per-iteration miss-helper read stays correct, just not hoisted).
    for instr in &code[s..=e] {
        match instr {
            Instr::SetIndex { .. } | Instr::SetProp { .. } | Instr::Call { .. } => return None,
            Instr::CallMethod { name, .. } => {
                if proto.string_constants.get(*name as usize).map(|s| s.as_str())
                    != Some("charCodeAt")
                {
                    return None;
                }
            }
            _ => {}
        }
    }
    // Exactly one `GetProp(_, "length")` in the region.
    let mut found: Option<(usize, u16, u16)> = None; // (ip, dst, obj)
    for ip in s..=e {
        if let Instr::GetProp { dst, obj, name } = code[ip] {
            if proto.string_constants.get(name as usize).map(|s| s.as_str()) == Some("length") {
                if found.is_some() {
                    return None; // more than one — bail
                }
                found = Some((ip, dst, obj));
            }
        }
    }
    let (get_ip, dst, obj) = found?;
    // `dst` must be written ONLY by this GetProp in the region.
    for ip in s..=e {
        if ip != get_ip && writes_reg(&code[ip]) == Some(dst) {
            return None;
        }
    }
    // `obj` must be defined in the region only by `LoadGlobal(g)` (same `g`), and
    // `g` never stored in the region.
    let mut g: Option<u32> = None;
    for ip in s..=e {
        match code[ip] {
            Instr::LoadGlobal { dst: ld, idx } if ld == obj => {
                if g.is_some() && g != Some(idx) {
                    return None; // obj loaded from two different globals
                }
                g = Some(idx);
            }
            Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } => {
                if Some(idx) == g {
                    return None; // g mutated in the loop
                }
            }
            _ => {
                // `obj` defined by something other than LoadGlobal → not a global.
                if writes_reg(&code[ip]) == Some(obj) {
                    return None;
                }
            }
        }
    }
    let name_idx = match code[get_ip] {
        Instr::GetProp { name, .. } => name,
        _ => return None,
    };
    g.map(|g| (get_ip, dst, g, name_idx))
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
    /// Helper for `a + b` (`StrConcat`).
    concat: usize,
    /// Helper for in-place `a + b` (`StrAppendInPlace`).
    str_append: usize,
    /// Helper for a generic `obj.m(args…)` via the interpreter's per-site IC.
    call_method_ic: usize,
    /// Helper for a generic `f(args…)` via the interpreter's per-site IC.
    call_ic: usize,
    /// `PROP_VIA_IC` continuation for GetProp (accessor / class receiver).
    get_prop_slow: usize,
    /// `PROP_VIA_IC` continuation for SetProp.
    set_prop_slow: usize,
    /// Full `===` for non-interned heap operands (read-only, 0/1).
    strict_eq: usize,
    /// Full truthiness for non-Int/Bool conditions (read-only, 0/1).
    truthy: usize,
    /// TypedArray pin snapshot helper (see `HeapHelperAddrs::ta_snapshot`).
    ta_snapshot: usize,
    /// Uint8Clamped double-store helper (pure).
    ta_clamp_store: usize,
    /// Whitelisted DataView `get*` helper.
    dv_get: usize,
    /// Pure unary `Math.<op>` helper (MathFn code, f64 bits → f64 bits).
    math_unary: usize,
    /// Pure two-arg `Math.<op>` helper (MathFn code, f64 bits, f64 bits → f64 bits).
    math_two: usize,
    /// Pure `CellGet` helper (cell bits → inner Value bits / TDZ-deopt sentinel).
    cell_get: usize,
    /// `UpvalGet` helper (upvalue idx → inner Value bits / TDZ-deopt sentinel).
    upval_get: usize,
    /// `ForInLive` helper (obj bits, key bits → Bool Value bits).
    forin_live: usize,
    /// `HasProp` (`in`) helper (key bits, obj bits → Bool Value bits / deopt).
    has_property: usize,
    /// Q4 leaf-inline entry headroom check (`jit_regs_fits`).
    regs_fits: usize,
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
#[allow(clippy::too_many_arguments)]
fn compile_region(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    heap: HeapHelpers,
    const_strs: &FxHashMap<u32, u64>,
    ta_plan: &TaPinPlan,
    leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
) -> Option<JitFn> {
    // The register/SROA paths decline any region containing a Call, so leaf
    // inlining (which only applies to Call sites) is reachable only via the
    // memory path below.
    if let Some(f) = compile_region_regalloc(proto, start, end, globals_base_helper) {
        return Some(f);
    }
    compile_region_mem(proto, start, end, globals_base_helper, heap, const_strs, ta_plan, leaf_plan)
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
    /// Arithmetic region ips whose 2^53 overflow guard is PROVABLY unnecessary
    /// (interval analysis showed the result always lands in `[-2^53, 2^53]`, e.g.
    /// a loop counter bounded by the loop condition's constant). INT path only;
    /// for a `Mul` it also licenses dropping the i64-overflow `jo` check.
    elide_guard: FxHashSet<usize>,
    /// Guard-elided `Mul` ips whose one operand is a single-def constant power of
    /// two: `(value operand reg, shift)` — emitted as `psllq` instead of an
    /// imul gpr round-trip. INT path only (f64 keeps `mulsd`).
    mul_shift: FxHashMap<usize, (u16, u8)>,
    /// `AddInt` immediates hoisted into spare xmm const homes (filled once in the
    /// prologue; the int path stores the i64, the double path the f64 bits).
    addint_imm_home: FxHashMap<i32, u8>,
    /// Hoisted integer-constant registers mirrored in a spare (otherwise unused)
    /// bool gpr, so int-path compares read `cmp rax, Rq(g)` instead of a second
    /// `movq` from the constant's xmm home. `(gpr, value)` per const reg.
    gpr_const: FxHashMap<u16, (u8, i64)>,
    /// In-region jump-target ips (any branch lands there, incl. the loop header).
    /// Used to gate the compare+branch flag-fusion peephole: a branch that is
    /// itself a jump target can't rely on flags from the preceding compare.
    jump_targets: FxHashSet<usize>,
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

// ── region home unification (copy coalescing) ───────────────────────────────
//
// A region temp that only ever shuttles a global's value (`LoadGlobal r ← g` /
// `<arith> r; StoreGlobal g ← r` pairs) can share the GLOBAL's xmm home, which
// deletes the `movdqa`/`movaps` copies from the loop body (the same effect as
// V8's copy coalescing). Soundness hinges on the exit-flush: an aliased reg's
// slot is flushed FROM THE SHARED HOME, so it must be provable that wherever the
// interpreter can resume, it never reads the reg before re-executing a def —
// hence the dominance (no jump target into a def's use window) and no-store
// (the global isn't redefined inside the window) conditions below.

/// In-region jump-target ips of `[s, e]` (branch targets that stay inside the
/// region, plus the OSR entry header `s` which the prologue jumps to).
fn region_jump_targets(code: &[Instr], s: usize, e: usize) -> FxHashSet<usize> {
    let mut t: FxHashSet<usize> = FxHashSet::default();
    t.insert(s);
    for instr in &code[s..=e] {
        let target = match *instr {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target as usize,
            _ => continue,
        };
        if target >= s && target <= e {
            t.insert(target);
        }
    }
    t
}

/// Detect regs that can share a GLOBAL's home: every def of `r` is either
/// `LoadGlobal g` (same `g` for all defs) or an op immediately followed by
/// `StoreGlobal g ← r`; per def, the use window contains no other store to `g`
/// and no jump target. Returns `reg → global` for each unifiable reg.
#[allow(clippy::too_many_arguments)]
fn unify_homes_with_globals(
    code: &[Instr],
    s: usize,
    e: usize,
    ty: &FxHashMap<u16, VTy>,
    first_seen: &FxHashMap<u16, bool>,
    dead: &FxHashSet<u16>,
    hoisted: &FxHashSet<u16>,
    jump_targets: &FxHashSet<usize>,
) -> FxHashMap<u16, u32> {
    // defs / uses per reg, in ascending ip order. An operand read at a def ip
    // (e.g. `Add r = r + x`) is attributed to the PREVIOUS def's window.
    let mut defs: FxHashMap<u16, Vec<usize>> = FxHashMap::default();
    let mut uses: FxHashMap<u16, Vec<usize>> = FxHashMap::default();
    let mut g_stores: FxHashMap<u32, Vec<usize>> = FxHashMap::default();
    for ip in s..=e {
        for u in instr_uses(&code[ip]) {
            uses.entry(u).or_default().push(ip);
        }
        if let Some(d) = writes_reg(&code[ip]) {
            defs.entry(d).or_default().push(ip);
        }
        if let Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } = code[ip] {
            g_stores.entry(idx).or_default().push(ip);
        }
    }

    let mut alias: FxHashMap<u16, u32> = FxHashMap::default();
    'cand: for (&r, def_ips) in &defs {
        if ty.get(&r) != Some(&VTy::Num)
            || first_seen.get(&r) != Some(&true) // live-in regs keep their own home
            || dead.contains(&r)
            || hoisted.contains(&r) // hoisted consts materialise in the prologue
        {
            continue;
        }
        let use_ips = match uses.get(&r) {
            Some(u) if !u.is_empty() => u,
            _ => continue, // no uses: nothing to win
        };
        // All defs must agree on one global `g`.
        let mut g: Option<u32> = None;
        // The def ips whose form is `<arith>; StoreGlobal g ← r` (the store ip is
        // exempt from the window's "no store to g" rule — it WRITES r's value).
        let mut adj_store_ips: FxHashSet<usize> = FxHashSet::default();
        for &d in def_ips {
            let gd = match code[d] {
                Instr::LoadGlobal { idx, .. } => idx,
                _ => {
                    // Must be immediately followed by `StoreGlobal g ← r`, and a
                    // path must not be able to enter AT the store (jump target).
                    match code.get(d + 1) {
                        Some(&Instr::StoreGlobal { idx, src })
                        | Some(&Instr::StoreGlobalStrict { idx, src })
                            if src == r && d + 1 <= e && !jump_targets.contains(&(d + 1)) =>
                        {
                            adj_store_ips.insert(d + 1);
                            idx
                        }
                        _ => continue 'cand,
                    }
                }
            };
            match g {
                None => g = Some(gd),
                Some(prev) if prev == gd => {}
                Some(_) => continue 'cand,
            }
        }
        let g = match g {
            Some(g) => g,
            None => continue,
        };
        // Per-def window check: (d, u_last] must contain no foreign store to `g`
        // and no jump target. A use AT the next def ip (operand of the redefining
        // op) belongs to THIS window.
        for (k, &d) in def_ips.iter().enumerate() {
            let next_d = def_ips.get(k + 1).copied().unwrap_or(usize::MAX);
            let u_last = use_ips
                .iter()
                .copied()
                .filter(|&u| u > d && (u < next_d || u == next_d))
                .max();
            let u_last = match u_last {
                Some(u) => u,
                None => continue,
            };
            if let Some(stores) = g_stores.get(&g) {
                if stores
                    .iter()
                    .any(|&sip| sip > d && sip <= u_last && !adj_store_ips.contains(&sip))
                {
                    continue 'cand;
                }
            }
            if jump_targets.iter().any(|&t| t > d && t <= u_last) {
                continue 'cand;
            }
        }
        alias.insert(r, g);
    }
    alias
}

/// Detect `Move dst ← src` temps that can share a LIVE-IN reg's home (`src` is
/// loop-carried so its home spans the whole region in both allocation modes).
/// Conditions mirror `unify_homes_with_globals`: dst single-def, src not
/// redefined and no jump target inside dst's use window.
fn unify_move_homes(
    code: &[Instr],
    s: usize,
    e: usize,
    ty: &FxHashMap<u16, VTy>,
    first_seen: &FxHashMap<u16, bool>,
    dead: &FxHashSet<u16>,
    hoisted: &FxHashSet<u16>,
    jump_targets: &FxHashSet<usize>,
    glob_alias: &FxHashMap<u16, u32>,
) -> FxHashMap<u16, u16> {
    let mut defs: FxHashMap<u16, Vec<usize>> = FxHashMap::default();
    let mut uses: FxHashMap<u16, Vec<usize>> = FxHashMap::default();
    for ip in s..=e {
        for u in instr_uses(&code[ip]) {
            uses.entry(u).or_default().push(ip);
        }
        if let Some(d) = writes_reg(&code[ip]) {
            defs.entry(d).or_default().push(ip);
        }
    }
    let mut alias: FxHashMap<u16, u16> = FxHashMap::default();
    for ip in s..=e {
        let (dst, src) = match code[ip] {
            Instr::Move { dst, src } => (dst, src),
            _ => continue,
        };
        if ty.get(&dst) != Some(&VTy::Num)
            || ty.get(&src) != Some(&VTy::Num)
            || first_seen.get(&dst) != Some(&true)
            || dead.contains(&dst)
            || hoisted.contains(&dst)
            || glob_alias.contains_key(&dst) // already unified with a global
            || defs.get(&dst).map(|d| d.len()) != Some(1)
            // src must be live-in (whole-region home) and not itself re-homed.
            || first_seen.get(&src) != Some(&false)
            || glob_alias.contains_key(&src)
        {
            continue;
        }
        let u_last = match uses.get(&dst).and_then(|u| u.iter().copied().max()) {
            Some(u) => u,
            None => continue,
        };
        // src not redefined and no jump target in (ip, u_last].
        let src_redef = defs
            .get(&src)
            .map_or(false, |d| d.iter().any(|&di| di > ip && di <= u_last));
        if src_redef || jump_targets.iter().any(|&t| t > ip && t <= u_last) {
            continue;
        }
        alias.insert(dst, src);
    }
    alias
}

// ── int-region interval analysis (overflow-guard elision) ───────────────────
//
// Forward abstract interpretation over the region's small CFG with an interval
// domain on regs + globals. Live-in values are Int-tagged at entry (i32 range),
// constants are exact, guarded arithmetic clamps its result to [-2^53, 2^53]
// (the guard bails otherwise), and the loop-bound compare refines the counter's
// interval on the fall-through edge. Any arithmetic whose UNCLAMPED result is
// proven inside [-2^53, 2^53] keeps the invariant without a runtime check, so
// its guard is elided (and a Mul's i64-overflow `jo` with it).

type Iv = (i64, i64);
const IV_FULL: Iv = (-TWO_POW_53, TWO_POW_53);
const IV_I32: Iv = (i32::MIN as i64, i32::MAX as i64);
/// Sentinel bound for out-of-range mul products (keeps i64 math safe).
const IV_BIG: i64 = TWO_POW_54;

fn iv_join(a: Iv, b: Iv) -> Iv {
    (a.0.min(b.0), a.1.max(b.1))
}
fn iv_clamp(a: Iv) -> Iv {
    (a.0.max(-TWO_POW_53), a.1.min(TWO_POW_53))
}
fn iv_in_bounds(a: Iv) -> bool {
    a.0 >= -TWO_POW_53 && a.1 <= TWO_POW_53
}
fn iv_add(a: Iv, b: Iv) -> Iv {
    // Operands are clamped to ±2^53 (invariant), so sums stay well inside i64.
    (a.0 + b.0, a.1 + b.1)
}
fn iv_sub(a: Iv, b: Iv) -> Iv {
    (a.0 - b.1, a.1 - b.0)
}
fn iv_mul(a: Iv, b: Iv) -> Iv {
    let c = [
        (a.0 as i128) * (b.0 as i128),
        (a.0 as i128) * (b.1 as i128),
        (a.1 as i128) * (b.0 as i128),
        (a.1 as i128) * (b.1 as i128),
    ];
    let lo = *c.iter().min().unwrap();
    let hi = *c.iter().max().unwrap();
    (
        lo.clamp(-(IV_BIG as i128), IV_BIG as i128) as i64,
        hi.clamp(-(IV_BIG as i128), IV_BIG as i128) as i64,
    )
}

/// Abstract state at a program point: intervals for numeric regs/globals (a
/// missing key means "unknown" = `IV_FULL`), the `reg == global` copy facts used
/// to propagate branch refinements to the source global, and the most recent
/// compare (for refining at an immediately following conditional branch).
#[derive(Clone, PartialEq)]
struct AbsState {
    regs: FxHashMap<u16, Iv>,
    globs: FxHashMap<u32, Iv>,
    alias: FxHashMap<u16, u32>,
    cmp: Option<(u16, u16, u16, Cmp, usize)>, // (cond, a, b, op, ip)
}

impl AbsState {
    fn reg(&self, r: u16) -> Iv {
        self.regs.get(&r).copied().unwrap_or(IV_FULL)
    }
    fn glob(&self, g: u32) -> Iv {
        self.globs.get(&g).copied().unwrap_or(IV_FULL)
    }
    /// Pointwise join into `self`; returns true if `self` changed. `widen`
    /// pushes any growing bound straight to its 2^53 extreme (fast convergence).
    fn join_from(&mut self, other: &AbsState, widen: bool) -> bool {
        let mut changed = false;
        // A key missing on either side means FULL; FULL is absorbing, so keep
        // only keys present in BOTH (others drop to the implicit FULL).
        let keys: Vec<u16> = self.regs.keys().copied().collect();
        for r in keys {
            let a = self.regs[&r];
            let j = match other.regs.get(&r) {
                Some(&b) => iv_join(a, b),
                None => IV_FULL,
            };
            let j = if widen && j != a {
                (
                    if j.0 < a.0 { -TWO_POW_53 } else { j.0 },
                    if j.1 > a.1 { TWO_POW_53 } else { j.1 },
                )
            } else {
                j
            };
            if j != a {
                self.regs.insert(r, j);
                changed = true;
            }
        }
        let keys: Vec<u32> = self.globs.keys().copied().collect();
        for g in keys {
            let a = self.globs[&g];
            let j = match other.globs.get(&g) {
                Some(&b) => iv_join(a, b),
                None => IV_FULL,
            };
            let j = if widen && j != a {
                (
                    if j.0 < a.0 { -TWO_POW_53 } else { j.0 },
                    if j.1 > a.1 { TWO_POW_53 } else { j.1 },
                )
            } else {
                j
            };
            if j != a {
                self.globs.insert(g, j);
                changed = true;
            }
        }
        let before = self.alias.len();
        self.alias.retain(|r, g| other.alias.get(r) == Some(g));
        if self.alias.len() != before {
            changed = true;
        }
        if self.cmp != other.cmp && self.cmp.is_some() {
            self.cmp = None;
            changed = true;
        }
        changed
    }
    /// Narrow reg `r` (and, via the copy fact, its source global) to `iv`.
    fn refine_reg(&mut self, r: u16, iv: Iv) {
        let cur = self.reg(r);
        let n = (cur.0.max(iv.0), cur.1.min(iv.1));
        self.regs.insert(r, n);
        if let Some(&g) = self.alias.get(&r) {
            let cg = self.glob(g);
            self.globs.insert(g, (cg.0.max(iv.0), cg.1.min(iv.1)));
        }
    }
    /// Is any tracked interval empty (an infeasible path)?
    fn infeasible(&self) -> bool {
        self.regs.values().any(|&(lo, hi)| lo > hi) || self.globs.values().any(|&(lo, hi)| lo > hi)
    }
}

/// Refine `a <cmp> b == truth` into `st` (both operands, alias-propagated).
fn refine_cmp(st: &mut AbsState, a: u16, b: u16, cmp: Cmp, truth: bool) {
    let (ia, ib) = (st.reg(a), st.reg(b));
    // Normalise to a "less" relation: a < b / a <= b (swapping for Gt/Ge).
    let (l, r, il, ir, le, holds) = match (cmp, truth) {
        (Cmp::Lt, t) => (a, b, ia, ib, false, t),
        (Cmp::Le, t) => (a, b, ia, ib, true, t),
        (Cmp::Gt, t) => (b, a, ib, ia, false, t),
        (Cmp::Ge, t) => (b, a, ib, ia, true, t),
        (Cmp::Eq, true) | (Cmp::Ne, false) => {
            let m = (ia.0.max(ib.0), ia.1.min(ib.1));
            st.refine_reg(a, m);
            st.refine_reg(b, m);
            return;
        }
        (Cmp::Eq, false) | (Cmp::Ne, true) => return,
    };
    if holds {
        // l < r (or <=): l_hi ≤ r_hi (-1), r_lo ≥ l_lo (+1).
        let adj = if le { 0 } else { 1 };
        st.refine_reg(l, (i64::MIN, ir.1 - adj));
        st.refine_reg(r, (il.0 + adj, i64::MAX));
    } else {
        // !(l < r) ⇔ l ≥ r (or l > r for !(<=)).
        let adj = if le { 1 } else { 0 };
        st.refine_reg(l, (ir.0 + adj, i64::MAX));
        st.refine_reg(r, (i64::MIN, il.1 - adj));
    }
}

/// Run the interval analysis over the (int-eligible) region `[s, e]` and return
/// the set of arithmetic ips whose 2^53 guard can be elided. `entry` carries the
/// live-in i32 facts and hoisted-constant values. Returns an empty set on any
/// op outside the modelled subset or non-convergence (all guards kept).
fn analyze_int_guards(proto: &FuncProto, s: usize, e: usize, entry: AbsState) -> FxHashSet<usize> {
    let n = e - s + 1;
    if n > 512 {
        return FxHashSet::default();
    }
    // states[i] = abstract state BEFORE executing ip s+i.
    let mut states: Vec<Option<AbsState>> = vec![None; n];
    states[0] = Some(entry);

    // Transfer of one op. Returns (fallthrough_state, optional (target, state)).
    // `elide` (when Some) collects guard-elidable arithmetic ips on a final pass.
    #[allow(clippy::type_complexity)]
    fn step(
        proto: &FuncProto,
        ip: usize,
        st: &AbsState,
        elide: Option<&mut FxHashSet<usize>>,
    ) -> Option<(Option<AbsState>, Option<(usize, AbsState)>)> {
        let code = &proto.code;
        let mut out = st.clone();
        out.cmp = None;
        let mut arith = |out: &mut AbsState, dst: u16, iv: Iv, elide: Option<&mut FxHashSet<usize>>| {
            if iv_in_bounds(iv) {
                if let Some(set) = elide {
                    set.insert(ip);
                }
            }
            out.regs.insert(dst, iv_clamp(iv));
            out.alias.remove(&dst);
        };
        match code[ip] {
            Instr::LoadInt { dst, val } => {
                out.regs.insert(dst, (val as i64, val as i64));
                out.alias.remove(&dst);
            }
            Instr::LoadConst { dst, idx } => {
                let c = proto.constants[idx as usize];
                if !c.is_int() {
                    return None;
                }
                let v = (c.bits() as u32 as i32) as i64;
                out.regs.insert(dst, (v, v));
                out.alias.remove(&dst);
            }
            Instr::Move { dst, src } => {
                out.regs.insert(dst, st.reg(src));
                match st.alias.get(&src).copied() {
                    Some(g) => {
                        out.alias.insert(dst, g);
                    }
                    None => {
                        out.alias.remove(&dst);
                    }
                }
            }
            Instr::LoadGlobal { dst, idx } => {
                out.regs.insert(dst, st.glob(idx));
                out.alias.insert(dst, idx);
            }
            Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } => {
                out.globs.insert(idx, st.reg(src));
                out.alias.retain(|_, g| *g != idx);
                out.alias.insert(src, idx);
            }
            Instr::AddInt { dst, a, imm, .. } => {
                arith(&mut out, dst, iv_add(st.reg(a), (imm as i64, imm as i64)), elide);
            }
            Instr::Add { dst, a, b } => arith(&mut out, dst, iv_add(st.reg(a), st.reg(b)), elide),
            Instr::Sub { dst, a, b } => arith(&mut out, dst, iv_sub(st.reg(a), st.reg(b)), elide),
            Instr::Mul { dst, a, b } => arith(&mut out, dst, iv_mul(st.reg(a), st.reg(b)), elide),
            Instr::Neg { dst, a } => {
                let (lo, hi) = st.reg(a);
                arith(&mut out, dst, (-hi, -lo), elide);
            }
            Instr::Mod { dst, .. } => {
                // |rem| < |b| ≤ 2^53; never guarded (see the Mod emitter).
                out.regs.insert(dst, (-(TWO_POW_53 - 1), TWO_POW_53 - 1));
                out.alias.remove(&dst);
            }
            Instr::Lt { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Lt, ip)),
            Instr::Le { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Le, ip)),
            Instr::Gt { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Gt, ip)),
            Instr::Ge { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Ge, ip)),
            Instr::Eq { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Eq, ip)),
            Instr::Ne { dst, a, b } => out.cmp = Some((dst, a, b, Cmp::Ne, ip)),
            Instr::Jump { target } => {
                return Some((None, Some((target as usize, out))));
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                let if_false = matches!(code[ip], Instr::JumpIfFalse { .. });
                let mut fall = out.clone();
                let mut jump = out;
                if let Some((c, a, b, op, cip)) = st.cmp {
                    if c == cond && cip + 1 == ip {
                        // fall-through executes when cond == !if_false… i.e. the
                        // branch is NOT taken: JumpIfFalse falls through on TRUE.
                        refine_cmp(&mut fall, a, b, op, if_false);
                        refine_cmp(&mut jump, a, b, op, !if_false);
                    }
                }
                return Some((Some(fall), Some((target as usize, jump))));
            }
            Instr::JumpIfNotLt { a, b, target } | Instr::JumpIfNotLe { a, b, target } => {
                let op = if matches!(code[ip], Instr::JumpIfNotLt { .. }) { Cmp::Lt } else { Cmp::Le };
                let mut fall = out.clone();
                let mut jump = out;
                refine_cmp(&mut fall, a, b, op, true);
                refine_cmp(&mut jump, a, b, op, false);
                return Some((Some(fall), Some((target as usize, jump))));
            }
            Instr::Return { .. } | Instr::ReturnUndefined => return Some((None, None)),
            _ => return None, // outside the modelled subset
        }
        Some((Some(out), None))
    }

    // Fixpoint with widening after a few passes; cap the pass count hard.
    let mut pass = 0usize;
    loop {
        pass += 1;
        if pass > 40 {
            return FxHashSet::default(); // no convergence — keep all guards
        }
        let widen = pass > 8;
        let mut changed = false;
        for ip in s..=e {
            let st = match &states[ip - s] {
                Some(st) if !st.infeasible() => st.clone(),
                _ => continue,
            };
            let (fall, jump) = match step(proto, ip, &st, None) {
                Some(r) => r,
                None => return FxHashSet::default(),
            };
            let mut merge = |tip: usize, ns: AbsState, states: &mut Vec<Option<AbsState>>| {
                if tip < s || tip > e || ns.infeasible() {
                    return false; // exits the region (or a dead edge)
                }
                match &mut states[tip - s] {
                    Some(old) => old.join_from(&ns, widen),
                    slot @ None => {
                        *slot = Some(ns);
                        true
                    }
                }
            };
            if let Some(f) = fall {
                changed |= merge(ip + 1, f, &mut states);
            }
            if let Some((t, j)) = jump {
                changed |= merge(t, j, &mut states);
            }
        }
        if !changed {
            break;
        }
    }

    // Final pass over the stable states: collect provably-in-bounds arithmetic.
    let mut elide: FxHashSet<usize> = FxHashSet::default();
    for ip in s..=e {
        if let Some(st) = &states[ip - s] {
            if !st.infeasible() {
                let _ = step(proto, ip, st, Some(&mut elide));
            }
        }
    }
    elide
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
        // A dense-array element access, any call, or a bitwise op can't be
        // register-allocated (their operands/result are boxed Values handled by
        // a helper / int32 lanes, not the planner's f64-or-bool scalars — and a
        // call can run arbitrary user code) — decline so the region takes the
        // memory path that emits the helper call / inline int32 sequence.
        if matches!(
            instr,
            Instr::GetIndex { .. }
                | Instr::SetIndex { .. }
                | Instr::CallMethod { .. }
                | Instr::Call { .. }
                | Instr::Bitwise { .. }
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
            | Instr::Div { dst, .. }
            | Instr::Mod { dst, .. } => (Some(dst), VTy::Num),
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
            Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } => {
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
            | Instr::Mod { a, b, .. }
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

    // Exact values of single-def integer-constant regs (hoisted or not): used by
    // the analysis entry state, the Mul strength reduction and the gpr mirrors.
    let mut const_vals: FxHashMap<u16, i64> = FxHashMap::default();
    for (&r, &ip) in &const_def_ip {
        if def_count.get(&r) != Some(&1) {
            continue;
        }
        match code[ip] {
            Instr::LoadInt { val, .. } => {
                const_vals.insert(r, val as i64);
            }
            Instr::LoadConst { idx, .. } => {
                if let Some(c) = proto.constants.get(idx as usize) {
                    if c.is_int() {
                        const_vals.insert(r, (c.bits() as u32 as i32) as i64);
                    }
                }
            }
            _ => {}
        }
    }

    // ── home unification (copy coalescing) ── temps that only shuttle a global's
    // (or a live-in reg's) value share that value's home; the body copies vanish.
    let jump_targets = region_jump_targets(code, s, e);
    let glob_alias =
        unify_homes_with_globals(code, s, e, &ty, &first_seen, &dead, &hoisted, &jump_targets);
    let move_alias = unify_move_homes(
        code, s, e, &ty, &first_seen, &dead, &hoisted, &jump_targets, &glob_alias,
    );
    // Aliased regs don't consume an xmm home of their own.
    reg_order.retain(|r| !glob_alias.contains_key(r) && !move_alias.contains_key(r));

    // ── overflow-guard elision (INT path) ── interval analysis proves which
    // arithmetic results always stay inside [-2^53, 2^53].
    let mut elide_guard: FxHashSet<usize> = FxHashSet::default();
    let mut mul_shift: FxHashMap<usize, (u16, u8)> = FxHashMap::default();
    if region_is_int(proto, start, end) {
        let mut entry = AbsState {
            regs: FxHashMap::default(),
            globs: FxHashMap::default(),
            alias: FxHashMap::default(),
            cmp: None,
        };
        for (&r, &def_first) in &first_seen {
            if !def_first && ty.get(&r) == Some(&VTy::Num) {
                entry.regs.insert(r, IV_I32); // live-in reg: entry-guarded Int
            }
        }
        for (&g, &read_first) in &glob_first_read {
            if read_first {
                entry.globs.insert(g, IV_I32); // live-in global: entry-guarded Int
            }
        }
        for &r in &hoisted {
            if let Some(&v) = const_vals.get(&r) {
                entry.regs.insert(r, (v, v)); // materialised in the prologue
            }
        }
        elide_guard = analyze_int_guards(proto, s, e, entry);
        // Strength-reduce a guard-elided `Mul` by a constant power of two into a
        // left shift (`psllq`), skipping the imul gpr round-trip.
        for ip in s..=e {
            if !elide_guard.contains(&ip) {
                continue;
            }
            if let Instr::Mul { a, b, .. } = code[ip] {
                let (val_reg, k) = match (const_vals.get(&a), const_vals.get(&b)) {
                    (_, Some(&k)) => (a, k),
                    (Some(&k), _) => (b, k),
                    _ => continue,
                };
                if k >= 2 && (k as u64).is_power_of_two() {
                    mul_shift.insert(ip, (val_reg, (k as u64).trailing_zeros() as u8));
                }
            }
        }
    }

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
    let first_free_xmm: u8;
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
        first_free_xmm = alloc.next; // never-touched homes are free for constants
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
        first_free_xmm = next_xmm;
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

    // ── spare-home constants ──
    // Distinct `AddInt` immediates get a permanent xmm const home when the pool
    // has room (saves a per-iteration materialise+convert in the loop body).
    let mut addint_imm_home: FxHashMap<i32, u8> = FxHashMap::default();
    {
        let mut imms: Vec<i32> = Vec::new();
        for instr in &code[s..=e] {
            if let Instr::AddInt { imm, .. } = *instr {
                if !imms.contains(&imm) {
                    imms.push(imm);
                }
            }
        }
        let mut next = first_free_xmm;
        for imm in imms {
            if next > HOME_XMM_LAST {
                break;
            }
            addint_imm_home.insert(imm, next);
            next += 1;
        }
    }
    // Hoisted integer constants used as compare operands get a spare bool-gpr
    // mirror so int-path compares avoid a second `movq` from the xmm home.
    let mut gpr_const: FxHashMap<u16, (u8, i64)> = FxHashMap::default();
    {
        let mut cand: Vec<u16> = Vec::new();
        for instr in &code[s..=e] {
            let (a, b) = match *instr {
                Instr::Lt { a, b, .. }
                | Instr::Le { a, b, .. }
                | Instr::Gt { a, b, .. }
                | Instr::Ge { a, b, .. }
                | Instr::Eq { a, b, .. }
                | Instr::Ne { a, b, .. }
                | Instr::JumpIfNotLt { a, b, .. }
                | Instr::JumpIfNotLe { a, b, .. } => (a, b),
                _ => continue,
            };
            for r in [a, b] {
                if hoisted.contains(&r) && const_vals.contains_key(&r) && !cand.contains(&r) {
                    cand.push(r);
                }
            }
        }
        let mut nb = next_bool;
        for r in cand {
            if nb >= BOOL_GPRS.len() {
                break;
            }
            gpr_const.insert(r, (BOOL_GPRS[nb], const_vals[&r]));
            nb += 1;
        }
    }

    // ── apply home unification ── aliased regs share their value's home; their
    // own slots are still flushed (from the shared home) on every exit.
    for (&r, &g) in &glob_alias {
        reg_home.insert(r, Home::Xmm(glob_home[&g]));
    }
    for (&r, &src) in &move_alias {
        let h = reg_home[&src];
        reg_home.insert(r, h);
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
    // Home-unified regs aren't in reg_order; they're flushed from the SHARED home
    // (never live-in: unification requires the first occurrence to be a def).
    for (&r, &g) in &glob_alias {
        num_regs.push((r, glob_home[&g]));
    }
    for (&r, _) in &move_alias {
        if let Home::Xmm(x) = reg_home[&r] {
            num_regs.push((r, x));
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
        elide_guard,
        mul_shift,
        addint_imm_home,
        gpr_const,
        jump_targets,
    })
}

/// The VM registers an instruction reads (operands). Used for live-in analysis.
fn instr_uses(i: &Instr) -> Vec<u16> {
    match *i {
        Instr::Move { src, .. } => vec![src],
        Instr::StoreGlobal { src, .. } | Instr::StoreGlobalStrict { src, .. } => vec![src],
        Instr::AddInt { a, .. } | Instr::Neg { a, .. } => vec![a],
        Instr::Add { a, b, .. }
        | Instr::Sub { a, b, .. }
        | Instr::Mul { a, b, .. }
        | Instr::Div { a, b, .. }
        | Instr::Mod { a, b, .. }
        | Instr::StrConcat { a, b, .. }
        | Instr::StrAppendInPlace { a, b, .. }
        | Instr::Bitwise { a, b, .. }
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
        if let Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } = *instr {
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
    if !region_can_compile(proto, start, end, None) {
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
    // AddInt immediates as f64 const homes (an i32 converts to f64 exactly).
    {
        let mut imms: Vec<(i32, u8)> = plan.addint_imm_home.iter().map(|(&i, &h)| (i, h)).collect();
        imms.sort_unstable();
        for (imm, h) in imms {
            let bits = (imm as f64).to_bits();
            dynasm!(ops ; mov rax, QWORD bits as i64 ; movq Rx(h), rax);
        }
    }
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // ── body ──
    // Compare→branch flag fusion (ordered f64 compares only — Eq/Ne need the
    // parity fix-up, so they keep the boxed-bool `test` path).
    let mut flag_cmp: Option<(usize, u16, Cmp)> = None;
    // Redundant-copy tracker (see `LastCopy`).
    let mut lc: LastCopy = None;
    for ip in s..=e {
        dynasm!(ops ; => lbl(ip as u32, &in_region));
        if plan.jump_targets.contains(&ip) {
            lc = None; // control may arrive here with different home contents
        }
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
        let prev_flag = flag_cmp.take();
        match proto.code[ip] {
            Instr::LoadInt { .. } | Instr::LoadConst { .. } => {
                emit_load_const(&mut ops, &plan, &proto.code[ip], proto);
                if let Some(d) = writes_reg(&proto.code[ip]) {
                    copy_clobber(&mut lc, xh(&plan, d));
                }
            }
            // Register copies use movaps (a FULL-register copy): unlike
            // `movsd xmm, xmm`, it has no false dependency on the destination's
            // old value and is eliminated at rename — this keeps the loop's
            // carried dependency chains down to the actual addsd/mulsd.
            Instr::Move { dst, src } => match home(&plan, dst) {
                Home::Xmm(d) => {
                    let srx = xh(&plan, src);
                    if d != srx && !copy_is_noop(lc, d, srx) {
                        dynasm!(ops ; movaps Rx(d), Rx(srx));
                        copy_clobber(&mut lc, d);
                        lc = Some((d, srx));
                    } else {
                        flag_cmp = prev_flag; // nothing emitted; flags still live
                    }
                }
                Home::Gpr(d) => {
                    let sg = gh(&plan, src);
                    dynasm!(ops ; mov Rq(d), Rq(sg));
                }
            },
            Instr::LoadGlobal { dst, idx } => {
                let d = xh(&plan, dst);
                let g = plan.glob_home[&idx];
                if d != g && !copy_is_noop(lc, d, g) {
                    dynasm!(ops ; movaps Rx(d), Rx(g));
                    copy_clobber(&mut lc, d);
                    lc = Some((d, g));
                } else {
                    flag_cmp = prev_flag;
                }
            }
            Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } => {
                let g = plan.glob_home[&idx];
                let srx = xh(&plan, src);
                if g != srx && !copy_is_noop(lc, g, srx) {
                    dynasm!(ops ; movaps Rx(g), Rx(srx));
                    copy_clobber(&mut lc, g);
                    lc = Some((g, srx));
                } else {
                    flag_cmp = prev_flag;
                }
            }
            Instr::Add { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Add, &mut lc),
            Instr::Sub { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Sub, &mut lc),
            Instr::Mul { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Mul, &mut lc),
            Instr::Div { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Div, &mut lc),
            Instr::AddInt { dst, a, imm, .. } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                let skip_copy = d == ax || copy_is_noop(lc, d, ax);
                if let Some(&ch) = plan.addint_imm_home.get(&imm) {
                    // The immediate sits (as f64) in a prologue-filled const home.
                    if !skip_copy {
                        dynasm!(ops ; movaps Rx(d), Rx(ax));
                    }
                    dynasm!(ops ; addsd Rx(d), Rx(ch));
                } else {
                    // Materialise the immediate's f64 bits via a gpr: `movq` writes
                    // the full register (no cvtsi2sd false dependency on xmm0).
                    let bits = (imm as f64).to_bits();
                    dynasm!(ops ; mov rax, QWORD bits as i64 ; movq xmm0, rax);
                    if !skip_copy {
                        dynasm!(ops ; movaps Rx(d), Rx(ax));
                    }
                    dynasm!(ops ; addsd Rx(d), xmm0);
                }
                copy_clobber(&mut lc, d);
            }
            Instr::Neg { dst, a } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                dynasm!(ops
                    ; xorps xmm0, xmm0
                    ; subsd xmm0, Rx(ax)
                    ; movaps Rx(d), xmm0
                );
                copy_clobber(&mut lc, d);
            }
            Instr::Lt { dst, a, b } => {
                emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Lt);
                flag_cmp = Some((ip, dst, Cmp::Lt));
            }
            Instr::Le { dst, a, b } => {
                emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Le);
                flag_cmp = Some((ip, dst, Cmp::Le));
            }
            Instr::Gt { dst, a, b } => {
                emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Gt);
                flag_cmp = Some((ip, dst, Cmp::Gt));
            }
            Instr::Ge { dst, a, b } => {
                emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Ge);
                flag_cmp = Some((ip, dst, Cmp::Ge));
            }
            Instr::Eq { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Eq),
            Instr::Ne { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Ne),
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                let if_false = matches!(proto.code[ip], Instr::JumpIfFalse { .. });
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                // Flag fusion off the preceding ucomisd (ordered compares only).
                // emit_dcmp computed `Lt/Le` as `ucomisd b, a` (seta/setae) and
                // `Gt/Ge` as `ucomisd a, b` — the unsigned-style jcc below mirror
                // that operand order, and NaN (CF=ZF=PF=1) makes every ordered
                // comparison FALSE: the `if_false` jcc is then taken, exactly as
                // the interpreter's NaN comparison semantics demand.
                let fused = match prev_flag {
                    Some((cip, creg, op))
                        if creg == cond
                            && !(cip + 1..=ip).any(|p| plan.jump_targets.contains(&p)) =>
                    {
                        Some(op)
                    }
                    _ => None,
                };
                match fused {
                    Some(op) => match (op, if_false) {
                        (Cmp::Lt, true) => dynasm!(ops ; jbe => t),  // !(b > a)
                        (Cmp::Le, true) => dynasm!(ops ; jb => t),   // !(b >= a)
                        (Cmp::Gt, true) => dynasm!(ops ; jbe => t),  // !(a > b)
                        (Cmp::Ge, true) => dynasm!(ops ; jb => t),   // !(a >= b)
                        (Cmp::Lt, false) => dynasm!(ops ; ja => t),
                        (Cmp::Le, false) => dynasm!(ops ; jae => t),
                        (Cmp::Gt, false) => dynasm!(ops ; ja => t),
                        (Cmp::Ge, false) => dynasm!(ops ; jae => t),
                        _ => unreachable!("flag fusion records ordered compares only"),
                    },
                    None => {
                        let c = gh(&plan, cond);
                        if if_false {
                            dynasm!(ops ; test Rq(c), Rq(c) ; jz => t);
                        } else {
                            dynasm!(ops ; test Rq(c), Rq(c) ; jnz => t);
                        }
                    }
                }
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
/// `region_is_int`: every op must be integer-valued (no Div — fractional; `Mod`
/// IS allowed, via integer `idiv`), and every `LoadConst` must be an Int-tagged
/// constant (a double constant would be misread as i64).
fn region_is_int(proto: &FuncProto, start: u32, end: u32) -> bool {
    if !region_can_compile(proto, start, end, None) {
        return false;
    }
    let (s, e) = (start as usize, end as usize);
    for instr in &proto.code[s..=e] {
        match *instr {
            Instr::LoadInt { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            // `let`/`const` global write: inside a hot loop region the binding is
            // already initialized, so it's treated like StoreGlobal.
            | Instr::StoreGlobalStrict { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Mod { .. }
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
    // Spare-home constants: AddInt immediates as i64 xmm homes, and gpr mirrors
    // of hoisted compare constants (both filled once; the body reads them).
    {
        let mut imms: Vec<(i32, u8)> = plan.addint_imm_home.iter().map(|(&i, &h)| (i, h)).collect();
        imms.sort_unstable();
        for (imm, h) in imms {
            dynasm!(ops ; mov rax, QWORD imm as i64 ; movq Rx(h), rax);
        }
        let mut gcs: Vec<(u8, i64)> = plan.gpr_const.values().copied().collect();
        gcs.sort_unstable();
        for (g, v) in gcs {
            dynasm!(ops ; mov Rq(g), QWORD v);
        }
    }
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // ── body ──
    // Compare→branch flag fusion: the last EMITTED op being a compare leaves its
    // flags live for an immediately following conditional jump (no re-`test`).
    let mut flag_cmp: Option<(usize, u16, Cmp)> = None;
    // Redundant-copy tracker (see `LastCopy`).
    let mut lc: LastCopy = None;
    for ip in s..=e {
        dynasm!(ops ; => lbl(ip as u32, &in_region));
        if plan.jump_targets.contains(&ip) {
            lc = None; // control may arrive here with different home contents
        }
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
        let prev_flag = flag_cmp.take();
        match proto.code[ip] {
            Instr::LoadInt { .. } | Instr::LoadConst { .. } => {
                emit_int_const(&mut ops, &plan, &proto.code[ip], proto);
                if let Some(d) = writes_reg(&proto.code[ip]) {
                    copy_clobber(&mut lc, xh(&plan, d));
                }
            }
            Instr::Move { dst, src } => match home(&plan, dst) {
                Home::Xmm(d) => {
                    let srx = xh(&plan, src);
                    if d != srx && !copy_is_noop(lc, d, srx) {
                        dynasm!(ops ; movdqa Rx(d), Rx(srx));
                        copy_clobber(&mut lc, d);
                        lc = Some((d, srx));
                    } else {
                        flag_cmp = prev_flag; // nothing emitted; flags still live
                    }
                }
                Home::Gpr(d) => {
                    let sg = gh(&plan, src);
                    dynasm!(ops ; mov Rq(d), Rq(sg));
                }
            },
            Instr::LoadGlobal { dst, idx } => {
                let d = xh(&plan, dst);
                let g = plan.glob_home[&idx];
                if d != g && !copy_is_noop(lc, d, g) {
                    dynasm!(ops ; movdqa Rx(d), Rx(g));
                    copy_clobber(&mut lc, d);
                    lc = Some((d, g));
                } else {
                    flag_cmp = prev_flag;
                }
            }
            Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } => {
                let g = plan.glob_home[&idx];
                let srx = xh(&plan, src);
                if g != srx && !copy_is_noop(lc, g, srx) {
                    dynasm!(ops ; movdqa Rx(g), Rx(srx));
                    copy_clobber(&mut lc, g);
                    lc = Some((g, srx));
                } else {
                    flag_cmp = prev_flag;
                }
            }
            Instr::Add { dst, a, b } => {
                emit_ibin(&mut ops, &plan, ip, flush_exit, dst, a, b, true, &mut lc);
            }
            Instr::Sub { dst, a, b } => {
                emit_ibin(&mut ops, &plan, ip, flush_exit, dst, a, b, false, &mut lc);
            }
            Instr::Mul { dst, a, b } => {
                let d = xh(&plan, dst);
                copy_clobber(&mut lc, d);
                if let Some(&(val_reg, shift)) = plan.mul_shift.get(&ip) {
                    // Guard-elided multiply by a constant power of two: a left
                    // shift (logical == arithmetic for the proven-in-range i64).
                    let vx = xh(&plan, val_reg);
                    if d != vx {
                        dynasm!(ops ; movdqa Rx(d), Rx(vx));
                    }
                    dynasm!(ops ; psllq Rx(d), shift as i8);
                } else if plan.elide_guard.contains(&ip) {
                    // Result proven within ±2^53 ⇒ no i64 overflow possible and
                    // no 2^53 guard needed; bare imul through the gprs.
                    let (ax, bx) = (xh(&plan, a), xh(&plan, b));
                    dynasm!(ops
                        ; movq rax, Rx(ax)
                        ; movq rcx, Rx(bx)
                        ; imul rax, rcx
                        ; movq Rx(d), rax
                    );
                } else {
                    // i64 multiply via imul (gpr). On i64 OVERFLOW (product ≥ 2^63)
                    // the result wrapped → bail at THIS ip WITHOUT storing dst, so the
                    // interpreter redoes it in f64 (reading the flushed operands). On a
                    // representable-but-large product the 2^53 guard handles it (like
                    // add): flush via cvtsi2sd (== JS's rounded product) + resume ip+1.
                    let (ax, bx) = (xh(&plan, a), xh(&plan, b));
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
            }
            Instr::Mod { dst, a, b } => {
                // i64 remainder via idiv (gpr): `rem = a % b`, truncated toward
                // zero with the dividend's sign — exactly JS `%` for integer
                // operands (the region is all-int). `% 0` → NaN (not an Int) →
                // bail at THIS ip (the interpreter redoes it, yielding NaN). The
                // dividend is guaranteed |a| ≤ 2^53 (entry guard + per-op i53
                // guard) so it is never i64::MIN ⇒ idiv can't #DE; and
                // |rem| < |b| ≤ 2^53, so the result is always representable (no
                // i53 guard needed). rcx/rdx are scratch here (bool homes live in
                // r8..r11, never rcx/rdx).
                let (d, ax, bx) = (xh(&plan, dst), xh(&plan, a), xh(&plan, b));
                copy_clobber(&mut lc, d);
                let zbail = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                dynasm!(ops
                    ; movq rax, Rx(ax)
                    ; movq rcx, Rx(bx)
                    ; test rcx, rcx
                    ; jz => zbail              // % 0 → NaN → redo in interp
                    ; cqo                       // sign-extend rax into rdx:rax
                    ; idiv rcx                  // rdx = remainder, rax = quotient
                    ; movq Rx(d), rdx
                    ; jmp => done
                    ; => zbail
                    ; mov DWORD [rsi], ip as i32 // resume at THIS op (dst unwritten)
                    ; jmp => flush_exit
                    ; => done
                );
            }
            Instr::AddInt { dst, a, imm, .. } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                if imm == 0 {
                    // `a + 0` over i64 is the identity (`AddInt` is `Add`, and the
                    // region is integer-only — no -0.0 to preserve): a pure copy,
                    // never able to overflow.
                    if d != ax && !copy_is_noop(lc, d, ax) {
                        dynasm!(ops ; movdqa Rx(d), Rx(ax));
                        copy_clobber(&mut lc, d);
                        lc = Some((d, ax));
                    } else {
                        flag_cmp = prev_flag;
                    }
                } else {
                    let skip_copy = d == ax || copy_is_noop(lc, d, ax);
                    if let Some(&ch) = plan.addint_imm_home.get(&imm) {
                        // The immediate sits in a prologue-filled const home.
                        if !skip_copy {
                            dynasm!(ops ; movdqa Rx(d), Rx(ax));
                        }
                        dynasm!(ops ; paddq Rx(d), Rx(ch));
                    } else {
                        // Materialise the (sign-extended) immediate as i64 in xmm0.
                        dynasm!(ops ; mov rax, QWORD imm as i64 ; movq xmm0, rax);
                        if !skip_copy {
                            dynasm!(ops ; movdqa Rx(d), Rx(ax));
                        }
                        dynasm!(ops ; paddq Rx(d), xmm0);
                    }
                    copy_clobber(&mut lc, d);
                    if !plan.elide_guard.contains(&ip) {
                        emit_i53_guard(&mut ops, d, ip, flush_exit);
                    }
                }
            }
            Instr::Neg { dst, a } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                dynasm!(ops
                    ; pxor xmm0, xmm0
                    ; psubq xmm0, Rx(ax)
                    ; movdqa Rx(d), xmm0
                );
                copy_clobber(&mut lc, d);
                if !plan.elide_guard.contains(&ip) {
                    emit_i53_guard(&mut ops, d, ip, flush_exit);
                }
            }
            Instr::Lt { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Lt);
                flag_cmp = Some((ip, dst, Cmp::Lt));
            }
            Instr::Le { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Le);
                flag_cmp = Some((ip, dst, Cmp::Le));
            }
            Instr::Gt { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Gt);
                flag_cmp = Some((ip, dst, Cmp::Gt));
            }
            Instr::Ge { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Ge);
                flag_cmp = Some((ip, dst, Cmp::Ge));
            }
            Instr::Eq { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Eq);
                flag_cmp = Some((ip, dst, Cmp::Eq));
            }
            Instr::Ne { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Ne);
                flag_cmp = Some((ip, dst, Cmp::Ne));
            }
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                let if_false = matches!(proto.code[ip], Instr::JumpIfFalse { .. });
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                // Flag fusion: the integer compare that produced `cond` was the
                // last emitted op, so its flags directly drive this branch. The
                // setcc/movzx that boxed the bool home don't touch flags. Any ip
                // in (cmp_ip, ip] being a jump target would let a path arrive
                // here with foreign flags — bail out to the generic `test`.
                let fused = match prev_flag {
                    Some((cip, creg, op))
                        if creg == cond
                            && !(cip + 1..=ip).any(|p| plan.jump_targets.contains(&p)) =>
                    {
                        Some(op)
                    }
                    _ => None,
                };
                match fused {
                    Some(op) => {
                        // Jump when the comparison is false (JumpIfFalse) / true.
                        match (op, if_false) {
                            (Cmp::Lt, true) => dynasm!(ops ; jge => t),
                            (Cmp::Le, true) => dynasm!(ops ; jg => t),
                            (Cmp::Gt, true) => dynasm!(ops ; jle => t),
                            (Cmp::Ge, true) => dynasm!(ops ; jl => t),
                            (Cmp::Eq, true) => dynasm!(ops ; jne => t),
                            (Cmp::Ne, true) => dynasm!(ops ; je => t),
                            (Cmp::Lt, false) => dynasm!(ops ; jl => t),
                            (Cmp::Le, false) => dynasm!(ops ; jle => t),
                            (Cmp::Gt, false) => dynasm!(ops ; jg => t),
                            (Cmp::Ge, false) => dynasm!(ops ; jge => t),
                            (Cmp::Eq, false) => dynasm!(ops ; je => t),
                            (Cmp::Ne, false) => dynasm!(ops ; jne => t),
                        }
                    }
                    None => {
                        let c = gh(&plan, cond);
                        if if_false {
                            dynasm!(ops ; test Rq(c), Rq(c) ; jz => t);
                        } else {
                            dynasm!(ops ; test Rq(c), Rq(c) ; jnz => t);
                        }
                    }
                }
            }
            Instr::JumpIfNotLt { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                emit_icmp_flags(&mut ops, &plan, a, b);
                // !(a<b) ⇔ a>=b (SIGNED).
                dynasm!(ops ; jge => t);
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                emit_icmp_flags(&mut ops, &plan, a, b);
                // !(a<=b) ⇔ a>b (SIGNED).
                dynasm!(ops ; jg => t);
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

/// Tracker for the most recent register-to-register home copy along the linear
/// emission path: `Some((d, s))` means homes `d` and `s` currently hold the SAME
/// value, so a pending `mov* d2, s2` over the same pair (either order) is a
/// no-op and can be skipped — this typically deletes the `tmp ← g; g ← tmp + x`
/// round-trip from a loop's carried dependency chain. Reset at every jump-target
/// ip (control may arrive with different contents) and invalidated whenever
/// either home is rewritten.
type LastCopy = Option<(u8, u8)>;

/// Would `movdqa/movaps Rx(d), Rx(s)` be a no-op given the tracked copy?
#[inline]
fn copy_is_noop(lc: LastCopy, d: u8, s: u8) -> bool {
    lc == Some((d, s)) || lc == Some((s, d))
}

/// Invalidate the tracker after home `h` is rewritten.
#[inline]
fn copy_clobber(lc: &mut LastCopy, h: u8) {
    if let Some((a, b)) = *lc {
        if a == h || b == h {
            *lc = None;
        }
    }
}

/// `home[dst] = home[a] <±> home[b]` as i64 (paddq/psubq), with aliasing handled
/// and a 2^53 guard (skipped when the interval analysis proved the result is
/// always in range). `add = true` ⇒ paddq (commutative); else psubq.
#[allow(clippy::too_many_arguments)]
fn emit_ibin(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, ip: usize, flush_exit: dynasmrt::DynamicLabel, dst: u16, a: u16, b: u16, add: bool, lc: &mut LastCopy) {
    let (d, ax, bx) = (xh(plan, dst), xh(plan, a), xh(plan, b));
    if add {
        if d == ax || copy_is_noop(*lc, d, ax) {
            dynasm!(ops ; paddq Rx(d), Rx(bx));
        } else if d == bx || copy_is_noop(*lc, d, bx) {
            dynasm!(ops ; paddq Rx(d), Rx(ax)); // commutative
        } else {
            dynasm!(ops ; movdqa Rx(d), Rx(ax) ; paddq Rx(d), Rx(bx));
        }
    } else if d == ax || copy_is_noop(*lc, d, ax) {
        dynasm!(ops ; psubq Rx(d), Rx(bx));
    } else if d == bx {
        // dst == b (and ≠ a): use xmm0 to avoid clobbering b before reading it.
        dynasm!(ops ; movdqa xmm0, Rx(ax) ; psubq xmm0, Rx(bx) ; movdqa Rx(d), xmm0);
    } else {
        dynasm!(ops ; movdqa Rx(d), Rx(ax) ; psubq Rx(d), Rx(bx));
    }
    copy_clobber(lc, d);
    if !plan.elide_guard.contains(&ip) {
        emit_i53_guard(ops, d, ip, flush_exit);
    }
}

/// Set the integer flags for `home[a] <cmp> home[b]` (SIGNED). Reads `b` from
/// its prologue-filled gpr mirror when it is a hoisted constant (one `movq`
/// fewer in the loop body); symmetric for a constant `a`.
fn emit_icmp_flags(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, a: u16, b: u16) {
    if let Some(&(g, _)) = plan.gpr_const.get(&b) {
        let ax = xh(plan, a);
        dynasm!(ops ; movq rax, Rx(ax) ; cmp rax, Rq(g));
    } else if let Some(&(g, _)) = plan.gpr_const.get(&a) {
        let bx = xh(plan, b);
        dynasm!(ops ; movq rax, Rx(bx) ; cmp Rq(g), rax);
    } else {
        let (ax, bx) = (xh(plan, a), xh(plan, b));
        dynasm!(ops ; movq rax, Rx(ax) ; movq rcx, Rx(bx) ; cmp rax, rcx);
    }
}

/// `bool_home[dst] = (home[a] <cmp> home[b])` as SIGNED i64 comparison.
fn emit_icmp(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, dst: u16, a: u16, b: u16, cmp: Cmp) {
    let d = gh(plan, dst);
    emit_icmp_flags(ops, plan, a, b);
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
fn emit_dbin(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, dst: u16, a: u16, b: u16, op: DOp, lc: &mut LastCopy) {
    let (d, ax, bx) = (xh(plan, dst), xh(plan, a), xh(plan, b));
    let commutative = matches!(op, DOp::Add | DOp::Mul);
    // Arrange operands so the accumulator is `d`. For non-commutative ops where
    // d == b (and d != a), use xmm0 as a temp to avoid clobbering b.
    if d == ax || copy_is_noop(*lc, d, ax) {
        emit_dop(ops, d, bx, op);
    } else if d == bx || (commutative && copy_is_noop(*lc, d, bx)) {
        if commutative {
            emit_dop(ops, d, ax, op); // d holds b; d = b op a == a op b
        } else {
            // movaps: full-register copies (rename-eliminated, no false dep).
            dynasm!(ops ; movaps xmm0, Rx(ax));
            emit_dop_xmm0(ops, bx, op); // xmm0 = a op b
            dynasm!(ops ; movaps Rx(d), xmm0);
        }
    } else {
        dynasm!(ops ; movaps Rx(d), Rx(ax));
        emit_dop(ops, d, bx, op);
    }
    copy_clobber(lc, d);
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

/// Re-derive the region's pinned heap pointers after a helper call that can
/// move them: r13 (heap version-array base — `heap.alloc` pushes to the
/// parallel versions Vec, which can reallocate) and, when `ic_base` is given,
/// r14 (JIT IC-table base — a NESTED region compile triggered by user code the
/// helper ran grows `ic_table`). rbx (register file) and r12 (globals) are
/// pinned to capacity for the VM's lifetime and never need re-deriving.
/// Clobbers only volatile registers — emit AFTER storing the helper's result.
fn emit_refetch_pinned(
    ops: &mut dynasmrt::x64::Assembler,
    versions_base: usize,
    ic_base: Option<usize>,
) {
    dynasm!(ops
        ; mov rcx, rdi
        ; mov rax, QWORD versions_base as i64
        ; call rax
        ; mov r13, rax
    );
    if let Some(icb) = ic_base {
        dynasm!(ops
            ; mov rcx, rdi
            ; mov rax, QWORD icb as i64
            ; call rax
            ; mov r14, rax
        );
    }
}

/// Byte offset (from the post-prologue rsp) of pinned-TypedArray snapshot slot
/// `j`: the frame reserves 32 bytes per pin ABOVE the 32B shadow space + 8B
/// 5th-arg slot. Layout within a slot: `obj_bits @0`, `base @8`, `len @16`
/// (8 bytes pad — keeps rsp 16-aligned for helper calls).
fn ta_slot_off(j: usize) -> i32 {
    40 + 32 * j as i32
}

/// (Re)derive every pinned TypedArray snapshot: re-read the live Value from its
/// source (global slot / frame register) and call `jit_ta_snapshot`, which
/// re-validates kind/detach/resize and writes `{obj_bits, base, len}` into the
/// pin's stack slot (`{0,0,0}` when ineligible — the per-access identity guard
/// then never matches and the access takes the generic-helper fallback).
/// Emitted in the prologue and AFTER every helper that can run user code
/// (which may detach/resize a buffer or reassign the source) — the same
/// discipline as the r13/r14 re-fetch. Clobbers only volatile registers.
fn emit_refetch_ta(ops: &mut dynasmrt::x64::Assembler, snapshot_helper: usize, plan: &TaPinPlan) {
    for (j, pin) in plan.pins.iter().enumerate() {
        match pin.src {
            TaPinSrc::Global(g) => dynasm!(ops ; mov rdx, [r12 + (g as i32) * 8]),
            TaPinSrc::Reg(r) => dynasm!(ops ; mov rdx, [rbx + dreg(r)]),
        }
        dynasm!(ops
            ; mov rcx, rdi                      // vm
            ; mov r8d, pin.kind as i32          // expected element kind
            ; lea r9, [rsp + ta_slot_off(j)]    // out: snapshot slot
            ; mov rax, QWORD snapshot_helper as i64
            ; call rax
        );
    }
}

/// Materialise a TypedArray element index from `regs[key]` into `rcx` (i64):
/// an Int tag sign-extends its payload; a double must be exactly integral
/// (cvttsd2si round-trip — NaN/±Inf/huge yield the 0x8000… sentinel, which
/// fails the round-trip) or the op DEOPTS; any other tag deopts. A negative or
/// huge index survives here and is caught by the caller's unsigned bounds
/// check (len < 2^31, so any negative i64 compares above it).
/// Clobbers rcx/r10/xmm0/xmm1.
fn emit_ta_key(ops: &mut dynasmrt::x64::Assembler, key: u16, bail: dynasmrt::DynamicLabel) {
    let key_dbl = ops.new_dynamic_label();
    let key_ok = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rcx, [rbx + dreg(key)]
        ; mov r10, rcx
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => key_dbl
        ; movsxd rcx, ecx                       // Int payload (may be negative)
        ; jmp => key_ok
        ; => key_dbl
        ; sub r10d, (INT_TAG_HI + 1) as i32
        ; cmp r10d, 3                           // Bool/Null/Undefined/Heap → deopt
        ; jbe => bail
        ; movq xmm0, rcx
        ; cvttsd2si rcx, xmm0                   // i64 trunc (NaN/±Inf → sentinel)
        ; cvtsi2sd xmm1, rcx
        ; ucomisd xmm1, xmm0
        ; jne => bail                           // fractional / sentinel
        ; jp => bail                            // NaN
        ; => key_ok
    );
}

/// Box the u32 in `eax` into `regs[dst]`: Int when it fits i32 (mirrors
/// `Value::num`'s narrowing), else the exact double (the `>>>` boxing pattern).
fn emit_box_u32(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    let as_dbl = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; test eax, eax
        ; js => as_dbl
    );
    box_eax(ops, dst);
    dynasm!(ops
        ; jmp => done
        ; => as_dbl
        ; mov eax, eax                // zero-extend u32
        ; cvtsi2sd xmm0, rax          // exact (< 2^32)
        ; movq rax, xmm0
        ; mov [rbx + dreg(dst)], rax
        ; => done
    );
}

/// Box the double in `xmm0` into `regs[dst]` EXACTLY as `Value::num` does:
/// an exact-integer in [i32::MIN, i32::MAX] (but NOT -0.0) narrows to an Int
/// tag; NaN canonicalises to the QNAN double; everything else (incl. -0.0,
/// ±Inf, non-integral, out-of-range integers) stays the raw f64 bits. Used for
/// `MathOp` results, whose interpreter arm stores `Value::num(r)` — so a
/// `Math.floor(x)===3` downstream bits-compare against Int(3) matches.
/// Clobbers rax/rcx/r10/xmm1.
fn emit_box_num(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    let as_dbl = ops.new_dynamic_label();
    let store_int = ops.new_dynamic_label();
    let canon = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    // Every path computes the final boxed bits into `rax`, then falls through to
    // the single store at `done`.
    dynasm!(ops
        // Truncate-to-i32; cvttsd2si yields 0x8000_0000 for NaN / |x|>=2^31 /
        // ±Inf — those all fail the exact round-trip below, so they fall to the
        // double path. (i32::MIN itself round-trips and narrows correctly.)
        ; cvttsd2si ecx, xmm0
        ; xorps xmm1, xmm1
        ; cvtsi2sd xmm1, ecx               // back to f64 (exact for any i32)
        ; ucomisd xmm1, xmm0
        ; jp => as_dbl                     // NaN operand → not integral
        ; jne => as_dbl                    // non-integral / out-of-range → double
        // Integral and in i32 range. Reject -0.0 (its int form 0 loses the sign):
        // -0.0 narrows to ecx==0 but has the original sign bit set.
        ; test ecx, ecx
        ; jnz => store_int                 // non-zero int: narrows
        ; movq rax, xmm0                   // zero: inspect the original sign bit
        ; bt rax, 63
        ; jc => as_dbl                     // -0.0 → keep as double
        ; => store_int
        ; mov eax, ecx                     // zero-extend the i32 payload
        ; mov r10, QWORD INT_TAG as i64
        ; or rax, r10                      // rax = INT_TAG | (payload as u32)
        ; jmp => done
        ; => as_dbl
        ; ucomisd xmm0, xmm0
        ; jp => canon                      // NaN → canonical QNAN
        ; movq rax, xmm0                   // finite/±Inf/-0 → raw f64 bits
        ; jmp => done
        ; => canon
        ; mov rax, QWORD QNAN_BITS as i64
        ; => done
        ; mov [rbx + dreg(dst)], rax
    );
}

/// Box the double in `xmm0` into `regs[dst]`, CANONICALISING any NaN — raw
/// TypedArray/DataView bytes could otherwise alias a NaN-box tag (heap-index
/// forgery). Not int-narrowed (the f64 mem tier's established representation).
fn emit_box_f64_canon(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    let canon = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; ucomisd xmm0, xmm0
        ; jp => canon                 // NaN → canonical
        ; movq rax, xmm0
        ; jmp => done
        ; => canon
        ; mov rax, QWORD QNAN_BITS as i64
        ; => done
        ; mov [rbx + dreg(dst)], rax
    );
}

/// Emit a generic `CallMethod` / `Call` region op as a `jit_call_method_ic` /
/// `jit_call_ic` helper call (vm/helpers_misc.rs). The helper consults the
/// interpreter's per-site inline cache, frame-calls the resolved plain user
/// function to completion, and returns the result bits — or `SELF_CALL_DEOPT`
/// (nothing happened: IC miss / megamorphic / depth limit → the interpreter
/// re-executes this op) or `CALL_THREW` (the call ran and THREW:
/// `pending_throw` is set; the OSR caller unwinds instead of resuming). Both
/// sentinels bail at this ip. ABI: rcx=vm, rdx=caller window base (rbx),
/// r8=(func_id<<32)|ip, r9=op-specific packing, [rsp+32]=argc (5th arg).
/// After a successful call, `refetch` re-derives r13/r14 (only needed when the
/// region has GetProp/SetProp sites — the only r13/r14 consumers).
#[allow(clippy::too_many_arguments)]
fn emit_region_call_ic(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    helper: usize,
    packed_fip: u64,
    packed_args: u64,
    argc: u16,
    dst: u16,
    refetch: Option<(usize, usize)>,
    ta_refetch: Option<(usize, &TaPinPlan)>,
) {
    dynasm!(ops
        ; mov rcx, rdi                          // vm
        ; mov rdx, rbx                          // caller window base ptr
        ; mov r8, QWORD packed_fip as i64       // (func_id << 32) | ip
        ; mov r9, QWORD packed_args as i64      // name/callee/obj/arg_base packing
        ; mov DWORD [rsp + 32], argc as i32     // 5th arg: argc
        ; mov rax, QWORD helper as i64
        ; call rax
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => bail                            // IC miss/depth → redo in interp
        ; mov r10, QWORD CALL_THREW as i64
        ; cmp rax, r10
        ; je => bail                            // threw → exit; caller unwinds
        ; mov [rbx + dreg(dst)], rax
    );
    if let Some((vb, icb)) = refetch {
        emit_refetch_pinned(ops, vb, Some(icb));
    }
    // The call ran user code, which may have detached/resized a pinned
    // TypedArray's buffer (or reassigned its source) — re-derive the snapshots.
    if let Some((snap, plan)) = ta_refetch {
        emit_refetch_ta(ops, snap, plan);
    }
    emit_region_bail(ops, ip, bail, epilogue);
}

/// Q4 leaf-call inlining (v1): emit a guarded INLINE expansion of a monomorphic
/// plain-leaf callee for a region `Call` op, with a fallback to the unchanged
/// per-call helper. Emitted INSTEAD of `emit_region_call_ic` when a `LeafInlinePlan`
/// exists for this ip.
///
/// Shape:
/// ```text
///   ; guard: regs[callee] == callee_bits     ; miss → fallback
///   ; headroom flag == 0                      ; tight → fallback
///   ; regs[W+0] = undefined ; regs[W+1+i] = regs[arg_base+i]   (arg copy)
///   ; <inlined body over scratch window W>    ; any bail → resume at CALL IP
///   ; regs[dst] = <return value>
///   ; jmp done
/// fallback:
///   ; <emit_region_call_ic — the existing helper, a pure prefix>
/// done:
/// ```
/// SOUNDNESS: the guard miss and the headroom-tight case both fall to the helper
/// (a PURE PREFIX — never deopts/evicts the region). The body is straight-line
/// (`callee_leaf_ok`); any inlined-op bail records the CALL IP and exits to the
/// epilogue, so the interpreter re-runs the WHOLE call (the side-effect-freedom-
/// before-deopt rule guarantees no global write happened yet). The body touches
/// only the scratch window (regs `W..W+callee_reg_count`, inside the pinned,
/// headroom-checked register file) and globals (r12); it runs no GC safepoint,
/// allocates nothing, and calls nothing — so r12/r13/r14 and the TA pins stay
/// valid and need no re-fetch, and the scratch slots need no zero-fill (the body
/// writes every reg it reads — see `callee_leaf_ok`: a leaf reads only its params
/// (copied above), globals, and its own freshly-computed regs).
#[allow(clippy::too_many_arguments)]
fn emit_inline_leaf_call(
    ops: &mut dynasmrt::x64::Assembler,
    call_ip: usize,
    epilogue: dynasmrt::DynamicLabel,
    leaf_flag_off: i32,
    plan: &LeafInlinePlan,
    callee: u16,
    arg_base: u16,
    argc: u16,
    dst: u16,
    math_unary: usize,
    math_two: usize,
    // Fallback emission (the unchanged per-call helper).
    helper: usize,
    packed_fip: u64,
    packed_args: u64,
    refetch: Option<(usize, usize)>,
    ta_refetch: Option<(usize, &TaPinPlan)>,
) {
    let fallback = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    let w = plan.reg_window;
    // ── identity guard ── the callee register must hold EXACTLY the cached
    // function value. A miss (callee reassigned, or a 2nd shape appears at this
    // now-not-really-mono site) takes the helper — never evicts the region.
    dynasm!(ops
        ; mov rax, [rbx + dreg(callee)]
        ; mov r10, QWORD plan.callee_bits as i64
        ; cmp rax, r10
        ; jne => fallback
        // ── version guard ── heap Value bits are pure `TAG_HEAP|idx`; a GC'd +
        // reused callee slot keeps IDENTICAL bits but bumps its `versions[idx]`.
        // The bits compare alone would then PASS and run the STALE old callee
        // body. Re-check the live slot version against the baked one (exactly the
        // `(bits, version)` tuple `ic_call` checks) — a mismatch falls to the
        // helper, which re-resolves the call correctly. `rax` still holds the
        // callee bits; its low 32 bits are the heap index. r13 = pinned heap
        // version-array base (re-derived after any allocating helper because the
        // region inlines a call — see `refetch_pinned`). The read is in-bounds:
        // the index came from a live heap Value (the bits matched) and `versions`
        // never shrinks; staleness is caught by this very compare.
        ; mov ecx, eax                          // recv heap idx (low 32 of bits)
        ; mov edx, [r13 + rcx*4]                // live slot version
        ; cmp edx, DWORD plan.callee_ver as i32
        ; jne => fallback
        // ── headroom flag ── 0 ⇒ the scratch window might overflow the pinned
        // register file (near-MAX_FRAMES recursion) → take the helper.
        ; cmp QWORD [rsp + leaf_flag_off], 0
        ; je => fallback
        // ── arg binding ── reg 0 (callee `this`) = undefined; positional args
        // into W+1.. (a leaf with simple_params binds args positionally). Args
        // beyond `param_count`/`argc` are ignored by a leaf body (no
        // `arguments`); params beyond `argc` stay undefined (the slot is zeroed
        // here so a stale scratch value can't leak in).
        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
        ; mov [rbx + dreg(w)], rax
    );
    let n = argc.min(plan.param_count);
    for i in 0..plan.param_count {
        if i < n {
            dynasm!(ops
                ; mov rax, [rbx + dreg(arg_base + i)]
                ; mov [rbx + dreg(w + 1 + i)], rax
            );
        } else {
            dynasm!(ops
                ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                ; mov [rbx + dreg(w + 1 + i)], rax
            );
        }
    }
    // ── zero-fill the callee's LOCALS (regs past `this`+params) to undefined ──
    // exactly as `setup_call` resizes the whole callee window to UNDEFINED. The
    // leaf body may read a local before writing it (e.g. `var x; return a + x;`
    // reads the uninitialized `x`); without this, that read would pick up a
    // STALE Value left in the carved scratch window by a prior call's expansion.
    {
        let undef = Value::UNDEFINED.bits() as i64;
        let first_local = 1 + plan.param_count; // reg index past `this` + params
        if first_local < plan.callee_reg_count {
            dynasm!(ops ; mov rax, QWORD undef);
            for r in first_local..plan.callee_reg_count {
                dynasm!(ops ; mov [rbx + dreg(w + r)], rax);
            }
        }
    }
    // ── inline the body over the scratch window ── every register `r` maps to
    // `w + r`. Each op that can bail uses a FRESH bail label whose block resumes
    // the interpreter at the CALL IP (so the whole call re-runs cleanly).
    let rg = |r: u16| w + r; // scratch-window register mapping
    let mut ret_reg: Option<u16> = None;
    for instr in &plan.body {
        match *instr {
            Instr::LoadInt { dst: d, val } => {
                let boxed = INT_TAG | (val as u32 as u64);
                dynasm!(ops
                    ; mov rax, QWORD boxed as i64
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::LoadConst { dst: d, idx } => {
                // `callee_leaf_ok` already restricted these to numeric consts.
                let bits = plan.const_bits(idx);
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::LoadBool { dst: d, val } => {
                let bits = BOOL_TAG | (val as u64);
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::Move { dst: d, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(src))]
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::LoadGlobal { dst: d, idx } => {
                dynasm!(ops
                    ; mov rax, [r12 + (idx as i32) * 8]
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(src))]
                    ; mov [r12 + (idx as i32) * 8], rax
                );
            }
            Instr::Add { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Add, true)
            }
            Instr::Sub { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Sub, true)
            }
            Instr::Mul { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Mul, true)
            }
            Instr::Div { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Div, false)
            }
            Instr::Mod { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                let as_dbl = ops.new_dynamic_label();
                let mod_done = ops.new_dynamic_label();
                load_num_xmm(ops, rg(a), 0, bail);
                load_num_xmm(ops, rg(b), 1, bail);
                dynasm!(ops
                    ; cvttsd2si rax, xmm0
                    ; cvttsd2si rcx, xmm1
                    ; test rcx, rcx
                    ; jz => bail
                    ; cvtsi2sd xmm2, rax
                    ; ucomisd xmm2, xmm0
                    ; jne => bail
                    ; cvtsi2sd xmm2, rcx
                    ; ucomisd xmm2, xmm1
                    ; jne => bail
                    ; cqo
                    ; idiv rcx
                    ; movsxd r8, edx
                    ; cmp r8, rdx
                    ; jne => as_dbl
                    ; mov r8, QWORD INT_TAG as i64
                    ; mov eax, edx
                    ; or rax, r8
                    ; mov [rbx + dreg(rg(d))], rax
                    ; jmp => mod_done
                    ; => as_dbl
                    ; cvtsi2sd xmm0, rdx
                    ; movq rax, xmm0
                    ; mov [rbx + dreg(rg(d))], rax
                    ; => mod_done
                );
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::AddInt { dst: d, a, imm, .. } => {
                let bail = ops.new_dynamic_label();
                let f64_path = ops.new_dynamic_label();
                let done_ai = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(a))]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; jne => f64_path
                    ; add eax, imm
                    ; jo => f64_path
                );
                box_eax(ops, rg(d));
                dynasm!(ops ; jmp => done_ai ; => f64_path);
                load_num_xmm(ops, rg(a), 0, bail);
                dynasm!(ops
                    ; mov eax, imm
                    ; cvtsi2sd xmm1, eax
                    ; addsd xmm0, xmm1
                );
                store_xmm(ops, rg(d));
                dynasm!(ops ; => done_ai);
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::Neg { dst: d, a } => {
                let bail = ops.new_dynamic_label();
                load_num_xmm(ops, rg(a), 1, bail);
                dynasm!(ops
                    ; xorps xmm0, xmm0
                    ; subsd xmm0, xmm1
                );
                store_xmm(ops, rg(d));
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::Bitwise { dst: d, a, b, op } => {
                use crate::bytecode::BitwiseOp as B;
                let bail = ops.new_dynamic_label();
                load_toint32(ops, rg(a), bail);
                dynasm!(ops ; mov r8d, eax);
                load_toint32(ops, rg(b), bail);
                dynasm!(ops ; mov ecx, eax ; mov eax, r8d);
                match op {
                    B::And => { dynasm!(ops ; and eax, ecx); box_eax(ops, rg(d)); }
                    B::Or => { dynasm!(ops ; or eax, ecx); box_eax(ops, rg(d)); }
                    B::Xor => { dynasm!(ops ; xor eax, ecx); box_eax(ops, rg(d)); }
                    B::Shl => { dynasm!(ops ; shl eax, cl); box_eax(ops, rg(d)); }
                    B::Shr => { dynasm!(ops ; sar eax, cl); box_eax(ops, rg(d)); }
                    B::Ushr => {
                        let as_dbl = ops.new_dynamic_label();
                        let done_u = ops.new_dynamic_label();
                        dynasm!(ops
                            ; shr eax, cl
                            ; test eax, eax
                            ; js => as_dbl
                        );
                        box_eax(ops, rg(d));
                        dynasm!(ops
                            ; jmp => done_u
                            ; => as_dbl
                            ; mov eax, eax
                            ; cvtsi2sd xmm0, rax
                            ; movq rax, xmm0
                            ; mov [rbx + dreg(rg(d))], rax
                            ; => done_u
                        );
                    }
                }
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::MathOp { dst: d, op, arg_base: ab, argc: ac } => {
                let bail = ops.new_dynamic_label();
                if ac == 1 {
                    load_num_xmm(ops, rg(ab), 0, bail);
                    dynasm!(ops
                        ; movq rdx, xmm0
                        ; mov ecx, op as i32
                        ; mov rax, QWORD math_unary as i64
                        ; call rax
                        ; movq xmm0, rax
                    );
                    emit_box_num(ops, rg(d));
                } else {
                    load_num_xmm(ops, rg(ab), 0, bail);
                    load_num_xmm(ops, rg(ab + 1), 1, bail);
                    dynasm!(ops
                        ; movq rdx, xmm0
                        ; movq r8, xmm1
                        ; mov ecx, op as i32
                        ; mov rax, QWORD math_two as i64
                        ; call rax
                        ; movq xmm0, rax
                    );
                    emit_box_num(ops, rg(d));
                }
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::Return { src } => {
                ret_reg = Some(rg(src));
            }
            Instr::ReturnUndefined => {
                ret_reg = None;
            }
            // `callee_leaf_ok` guarantees the body contains only the ops above.
            ref other => unreachable!("inline leaf body op not admitted: {other:?}"),
        }
    }
    // ── store the return value into the caller's `dst` ──
    match ret_reg {
        Some(r) => dynasm!(ops
            ; mov rax, [rbx + dreg(r)]
            ; mov [rbx + dreg(dst)], rax
        ),
        None => dynasm!(ops
            ; mov rax, QWORD Value::UNDEFINED.bits() as i64
            ; mov [rbx + dreg(dst)], rax
        ),
    }
    dynasm!(ops
        ; jmp => done
        ; => fallback
    );
    // ── fallback ── the UNCHANGED per-call helper (a pure prefix). On a clean
    // return it writes `dst`; on its own deopt/throw sentinel it bails at this
    // ip via the bail label `emit_region_call_ic` creates internally.
    let helper_bail = ops.new_dynamic_label();
    emit_region_call_ic(
        ops, call_ip, helper_bail, epilogue, helper, packed_fip, packed_args, argc, dst, refetch,
        ta_refetch,
    );
    dynasm!(ops ; => done);
}

/// Memory-based region codegen: every op loads operands from the register file
/// (with a type guard) and stores results back, globals via the pinned base
/// pointer. Correct and simple; ~4x faster than the interpreter but leaves
/// per-iteration memory traffic on the table (the register-promoting path above
/// removes it). Kept as the fallback for regions the allocator declines.
#[allow(clippy::too_many_arguments)]
fn compile_region_mem(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    heap: HeapHelpers,
    const_strs: &FxHashMap<u32, u64>,
    ta_plan: &TaPinPlan,
    leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
) -> Option<JitFn> {
    if !region_can_compile(proto, start, end, Some(const_strs)) {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let (s, e) = (start as usize, end as usize);

    if std::env::var_os("ZIPP_JITDUMP").is_some() {
        for ip in s..=e {
            eprintln!("[dump] {ip}: {:?}", proto.code[ip]);
        }
    }

    // Does the region use the r13/r14 inline-cache pointers at all? Only
    // GetProp/SetProp read them; when absent, allocating/user-code helpers
    // skip the post-call re-fetch entirely.
    let has_prop = proto.code[s..=e]
        .iter()
        .any(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }));
    // ── Q4 leaf-call inlining ── the highest scratch slot any inlined callee
    // uses above the caller window (`reg_window + callee_reg_count`). Checked
    // ONCE at entry by `jit_regs_fits`; the result gates each inlined Call (a
    // tight-headroom run falls back to the per-call helper for every site).
    let do_leaf = !leaf_plan.is_empty();
    // The Q4 leaf-inline identity guard re-checks the callee slot's live version
    // (read from r13, the pinned heap version-array base) to defeat GC slot-reuse
    // ABA. r13 is pinned at the prologue, but any intervening ALLOCATING / user-
    // code helper (jit_concat, a fallback call, …) can reallocate the versions
    // Vec and leave r13 STALE. So whenever the region inlines a call, the version
    // base must be re-derived after such helpers too — exactly where a GetProp/
    // SetProp region re-derives it. Fold `do_leaf` into the refetch gate.
    let refetch_pinned = has_prop || do_leaf;
    let max_scratch_top: u64 = leaf_plan
        .values()
        .map(|p| p.reg_window as u64 + p.callee_reg_count as u64)
        .max()
        .unwrap_or(0);
    // Pinned-TypedArray snapshot slots: 32 bytes each, above the 32B shadow +
    // 8B 5th-arg slot. 32*n keeps the frame's 16-alignment. The leaf-inline
    // headroom flag adds one more 16B slot at the top of the frame.
    let n_ta = ta_plan.pins.len();
    let frame = 40 + 32 * n_ta as i32 + if do_leaf { 16 } else { 0 };
    // Byte offset (from post-prologue rsp) of the headroom flag slot (1 = the
    // scratch window fits → inline; 0 = fall back to the per-call helper).
    let leaf_flag_off = frame - 8;
    // Re-derive the pins after any helper that can run user code.
    let ta_refetch = (n_ta > 0).then_some((heap.ta_snapshot, ta_plan));
    // Registers fed by a DOUBLE constant (`x * 1.5`, `i * 2654435761`): their
    // arithmetic skips the Int+Int fast path (it would fail every iteration).
    // Pure perf heuristic — a multiply-defined reg merely keeps the check.
    let mut const_dbl_regs: FxHashSet<u16> = FxHashSet::default();
    for instr in &proto.code[s..=e] {
        if let Instr::LoadConst { dst, idx } = *instr {
            if proto.constants.get(idx as usize).is_some_and(|c| c.is_double()) {
                const_dbl_regs.insert(dst);
            }
        }
    }
    let int_hint = |a: u16, b: u16| !const_dbl_regs.contains(&a) && !const_dbl_regs.contains(&b);

    // One label per in-region ip (offset by `start`). Out-of-region jump targets
    // resolve to lazily-created exit stubs.
    let in_region: Vec<_> = (s..=e).map(|_| ops.new_dynamic_label()).collect();
    let mut exit_stubs: FxHashMap<u32, dynasmrt::DynamicLabel> = FxHashMap::default();
    let epilogue = ops.new_dynamic_label();
    // Resume in the interpreter at the loop header if the hoisted `.length`
    // compute deopts at entry (`g` isn't a string/array).
    let entry_len_bail = ops.new_dynamic_label();
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
        ; sub rsp, frame                  // 32B shadow + 8B 5th-arg slot + 32B/TA pin ⇒ rsp 16-aligned
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
    );
    // Pin each TypedArray's `{obj_bits, base, len}` snapshot (entry derivation).
    if let Some((snap, plan)) = ta_refetch {
        emit_refetch_ta(&mut ops, snap, plan);
    }
    // ── Q4 leaf-inline headroom check (once per entry) ── `jit_regs_fits(vm,
    // rbx, max_scratch_top)` → 1 if the carved scratch windows lie inside the
    // pinned register file (the common case). Stash the 0/1 in the flag slot;
    // each inlined Call site reads it and falls back to the helper on 0. rbx is
    // callee-saved (survives the call); rcx/rdx/r8 are volatile scratch here.
    if do_leaf {
        dynasm!(ops
            ; mov rcx, rdi                            // vm
            ; mov rdx, rbx                            // caller window base
            ; mov r8, QWORD max_scratch_top as i64    // highest scratch slot used
            ; mov rax, QWORD heap.regs_fits as i64
            ; call rax
            ; mov [rsp + leaf_flag_off], rax          // 1 = inline ok, 0 = helper
        );
    }

    // ── loop-invariant `g.length` hoist ── compute it ONCE here (reusing the
    // GetProp miss helper, which returns string/array `.length` directly) instead
    // of a helper call every iteration. The body skips the hoisted GetProp, so its
    // dst keeps this value. If `g` isn't a string/array at entry the helper deopts
    // → resume the loop in the interpreter (it recomputes `.length` correctly).
    let hoisted_len = hoistable_length(proto, start, end);
    if let Some((_get_ip, dst, g, name_idx)) = hoisted_len {
        let packed = ((heap.func_id as u64) << 32) | name_idx as u64;
        dynasm!(ops
            ; mov rdx, [r12 + (g as i32) * 8]     // obj bits = globals[g]
            ; mov rcx, rdi                         // vm
            // Pseudo-site: u32::MAX makes any fill a no-op (`set_ic` ignores
            // it). A real site id here could cross-pollute another site's ways
            // with a DIFFERENT KEY's slot (same receiver identity → wrong slot).
            ; mov r8d, -1                          // site_idx = u32::MAX

            ; mov r9, QWORD packed as i64
            ; mov rax, QWORD heap.get_prop_miss as i64
            ; call rax
            ; mov r10, QWORD SELF_CALL_DEOPT as i64
            ; cmp rax, r10
            ; je => entry_len_bail
            ; mov r10, QWORD PROP_VIA_IC as i64       // accessor `length` etc.
            ; cmp rax, r10
            ; je => entry_len_bail
            ; mov [rbx + dreg(dst)], rax
        );
    }
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // The k-th GetProp/SetProp in the region uses inline-cache site `ic_site`.
    let mut ic_site = heap.ic_base_idx;
    for ip in s..=e {
        // Skip the hoisted `.length` GetProp — its dst already holds the value
        // (computed once in the prologue). The label is still emitted so jumps
        // into this ip resolve; the op itself is elided.
        if let Some((get_ip, ..)) = hoisted_len {
            if ip == get_ip {
                dynasm!(ops ; => lbl(ip as u32, &in_region));
                continue;
            }
        }
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
                // `=== "x"` is a bits compare; a multi-char string constant uses
                // the bits interned at REGION-COMPILE time (rooted for the VM's
                // life in `jit_const_strings`); numeric consts use raw bits.
                let c = proto.constants[idx as usize];
                let bits = single_char_const_bits(proto, c)
                    .or_else(|| const_strs.get(&idx).copied())
                    .unwrap_or_else(|| c.bits());
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
            Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [r12 + (idx as i32) * 8], rax
                );
            }
            Instr::Add { dst, a, b } => {
                // Int+Int fast path (32-bit add + overflow check, Int result —
                // the interpreter's `checked_add`), then the numeric f64 path;
                // non-number operands (strings, objects) fall back to
                // `jit_concat` — the SAME `add_values` the interpreter's Add
                // runs (concat / numeric / coercion). The helper may allocate
                // or run user coercion code, so the pinned pointers are
                // re-derived when the region reads them.
                let slow = ops.new_dynamic_label();
                let f64_path = ops.new_dynamic_label();
                let done_a = ops.new_dynamic_label();
                if int_hint(a, b) {
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(a)]
                        ; mov rcx, [rbx + dreg(b)]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => f64_path
                        ; mov r10, rcx
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => f64_path
                        ; add eax, ecx
                        ; jo => f64_path          // overflow → f64 (reloads operands)
                    );
                    box_eax(&mut ops, dst);
                    dynasm!(ops ; jmp => done_a);
                }
                dynasm!(ops ; => f64_path);
                load_num_xmm(&mut ops, a, 0, slow);
                load_num_xmm(&mut ops, b, 1, slow);
                dynasm!(ops ; addsd xmm0, xmm1);
                store_xmm(&mut ops, dst);
                dynasm!(ops
                    ; jmp => done_a
                    ; => slow
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]
                    ; mov r8, [rbx + dreg(b)]
                    ; mov rax, QWORD heap.concat as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // IC-style redo (nothing ran)
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                          // threw (pending_throw set) → unwind, NOT redo
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                // `add_values` can run user coercion code (valueOf) — re-derive
                // the pinned TypedArray snapshots.
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                dynasm!(ops ; => done_a);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Sub { dst, a, b } => {
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Sub, int_hint(a, b))
            }
            Instr::Mul { dst, a, b } => {
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Mul, int_hint(a, b))
            }
            Instr::Div { dst, a, b } => {
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Div, false)
            }
            Instr::Mod { dst, a, b } => {
                // `a % b` for INTEGER-valued operands via i64 idiv (exact, and the
                // remainder takes the dividend's sign — JS `%` for integers).
                // Non-integer operands or `% 0` bail to the interpreter (true fmod
                // / NaN). xmm2/rax/rcx/rdx are scratch in this memory path.
                load_num_xmm(&mut ops, a, 0, bail); // xmm0 = a
                load_num_xmm(&mut ops, b, 1, bail); // xmm1 = b
                let as_dbl = ops.new_dynamic_label();
                let mod_done = ops.new_dynamic_label();
                dynasm!(ops
                    ; cvttsd2si rax, xmm0            // a → i64 (trunc toward 0)
                    ; cvttsd2si rcx, xmm1            // b → i64
                    ; test rcx, rcx
                    ; jz => bail                     // % 0 → NaN (interp)
                    ; cvtsi2sd xmm2, rax
                    ; ucomisd xmm2, xmm0
                    ; jne => bail                    // a not integer-valued → fmod
                    ; cvtsi2sd xmm2, rcx
                    ; ucomisd xmm2, xmm1
                    ; jne => bail                    // b not integer-valued → fmod
                    ; cqo                            // sign-extend rax into rdx:rax
                    ; idiv rcx                       // rdx = a % b (i64 remainder)
                    // Box the remainder as an Int Value when it fits i32 (it does
                    // for any |b| ≤ 2^31). Keeping it Int — not a double — means a
                    // downstream `s += (i%k)` concat hits the interned-digit fast
                    // path instead of allocating a string per element.
                    ; movsxd r8, edx
                    ; cmp r8, rdx
                    ; jne => as_dbl
                    ; mov r8, QWORD INT_TAG as i64
                    ; mov eax, edx                   // zero-extend i32 payload
                    ; or rax, r8
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => mod_done
                    ; => as_dbl
                    ; cvtsi2sd xmm0, rdx             // large remainder → double Value
                    ; movq rax, xmm0
                    ; mov [rbx + dreg(dst)], rax
                    ; => mod_done
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::AddInt { dst, a, imm, .. } => {
                // Int fast path (the interpreter's `checked_add` — keeps loop
                // counters Int so element-access keys stay on their cheap
                // path), f64 fallback otherwise / on overflow.
                let f64_path = ops.new_dynamic_label();
                let done_ai = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; jne => f64_path
                    ; add eax, imm
                    ; jo => f64_path
                );
                box_eax(&mut ops, dst);
                dynasm!(ops ; jmp => done_ai ; => f64_path);
                load_num_xmm(&mut ops, a, 0, bail);
                dynasm!(ops
                    ; mov eax, imm
                    ; cvtsi2sd xmm1, eax
                    ; addsd xmm0, xmm1
                );
                store_xmm(&mut ops, dst);
                dynasm!(ops ; => done_ai);
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
            Instr::Bitwise { dst, a, b, op } => {
                // ToInt32 both operands (Int payloads or exactly-integral
                // i32-range doubles — see `load_toint32`; everything else
                // bails), then the 32-bit op. x86 32-bit shifts mask the count
                // to 5 bits — exactly JS's `& 31`. Results always fit i32
                // (boxed Int) except `>>>`, whose u32 result may exceed
                // i32::MAX and is then boxed as an (exact) double.
                use crate::bytecode::BitwiseOp as B;
                load_toint32(&mut ops, a, bail);
                dynasm!(ops ; mov r8d, eax);             // stash a
                load_toint32(&mut ops, b, bail);
                dynasm!(ops ; mov ecx, eax ; mov eax, r8d); // eax = a, ecx = b
                match op {
                    B::And => {
                        dynasm!(ops ; and eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Or => {
                        dynasm!(ops ; or eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Xor => {
                        dynasm!(ops ; xor eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Shl => {
                        dynasm!(ops ; shl eax, cl);
                        box_eax(&mut ops, dst);
                    }
                    B::Shr => {
                        dynasm!(ops ; sar eax, cl);
                        box_eax(&mut ops, dst);
                    }
                    B::Ushr => {
                        let as_dbl = ops.new_dynamic_label();
                        let done_u = ops.new_dynamic_label();
                        dynasm!(ops
                            ; shr eax, cl
                            ; test eax, eax
                            ; js => as_dbl                // u32 > i32::MAX → double
                        );
                        box_eax(&mut ops, dst);
                        dynasm!(ops
                            ; jmp => done_u
                            ; => as_dbl
                            ; mov eax, eax                // zero-extend u32 into rax
                            ; cvtsi2sd xmm0, rax          // exact (< 2^32)
                            ; movq rax, xmm0
                            ; mov [rbx + dreg(dst)], rax
                            ; => done_u
                        );
                    }
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Not { dst, a } => {
                // `!x`: a Bool flips its payload bit in place (the tag survives
                // the xor); anything else asks the read-only `jit_truthy`
                // helper (handles Int/double/heap incl. empty strings and
                // [[IsHTMLDDA]]) and flips its 0/1.
                let slow = ops.new_dynamic_label();
                let done_n = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, (INT_TAG_HI + 1) as i32   // Bool tag 0x7FFA
                    ; jne => slow
                    ; xor rax, 1                          // flip the payload bit
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => done_n
                    ; => slow
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // value bits
                    ; mov rax, QWORD heap.truthy as i64
                    ; call rax
                    ; xor rax, 1                          // !truthy
                    ; mov r8, QWORD BOOL_TAG as i64
                    ; or rax, r8
                    ; mov [rbx + dreg(dst)], rax
                    ; => done_n
                );
            }
            Instr::LoadBool { dst, val } => {
                // Materialise the boolean Value bits (BOOL_TAG | 0/1) inline.
                let bits = BOOL_TAG | (val as u64);
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::MathOp { dst, op, arg_base, argc } => {
                // Pure `Math.<op>`. Operands are loaded as numbers (Int/double);
                // a non-numeric operand BAILS to the interpreter, which runs the
                // full ToNumber coercion (a user valueOf). So the helpers below
                // never run user code and never allocate — no r13/r14/TA refetch.
                // Result boxed via `emit_box_num` (mirrors the interpreter's
                // `Value::num(r)` exactly: exact-int narrows, -0/NaN preserved).
                if argc == 1 {
                    load_num_xmm(&mut ops, arg_base, 0, bail);
                    dynasm!(ops
                        ; movq rdx, xmm0                  // arg f64 bits (arg1)
                        ; mov ecx, op as i32              // MathFn code (repr(u8), arg0)
                        ; mov rax, QWORD heap.math_unary as i64
                        ; call rax
                        ; movq xmm0, rax                  // result f64 bits
                    );
                    emit_box_num(&mut ops, dst);
                } else {
                    // EXACTLY two args (region_can_compile gated the op set).
                    load_num_xmm(&mut ops, arg_base, 0, bail);
                    load_num_xmm(&mut ops, arg_base + 1, 1, bail);
                    dynasm!(ops
                        ; movq rdx, xmm0                  // arg0 f64 bits (arg1)
                        ; movq r8, xmm1                   // arg1 f64 bits (arg2)
                        ; mov ecx, op as i32              // MathFn code (arg0)
                        ; mov rax, QWORD heap.math_two as i64
                        ; call rax
                        ; movq xmm0, rax
                    );
                    emit_box_num(&mut ops, dst);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::CellGet { dst, cell } => {
                // Per-op captured-cell read (jit_cell_get). NEVER hoisted: a
                // Call/CallMethod earlier in the region may have run an inner
                // closure that mutated the cell, so the live value is re-read
                // here every execution. A TDZ cell returns SELF_CALL_DEOPT → bail
                // (the interpreter then throws the ReferenceError at this ip).
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(cell)]        // cell Value bits
                    ; mov rax, QWORD heap.cell_get as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // TDZ → interpreter throws
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::UpvalGet { dst, idx } => {
                // Per-op upvalue read (jit_upval_get resolves the running closure
                // from the TOP frame). Same no-hoist soundness as CellGet.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov edx, idx as i32                // upvalue index
                    ; mov rax, QWORD heap.upval_get as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // TDZ / malformed → interp
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::ForInLive { dst, obj, key } => {
                // Per-op for-in liveness check (jit_forin_live → Vm::forin_live).
                // Re-reads the live shape each execution. Stores the Bool Value
                // bits the helper returns (matches the interpreter's
                // `Value::bool(live)`). Never deopts. The helper does no VM-heap
                // alloc on the common path, but `key_of`/proto-walk could grow the
                // heap in principle — so when the region also has GetProp/SetProp
                // (the only r13/r14 consumers), re-derive those pinned pointers
                // afterward (the StrConcat discipline). It runs NO user code, so
                // the TypedArray snapshots are unaffected.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(obj)]         // obj bits
                    ; mov r8, [rbx + dreg(key)]          // key bits
                    ; mov rax, QWORD heap.forin_live as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax         // Bool Value bits
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
            }
            Instr::HasProp { dst, key, obj, brand: _ } => {
                // `key in obj` (region_can_compile admitted only brand=false).
                // The read-only `jit_has_property` helper returns the BOOL Value
                // bits, or SELF_CALL_DEOPT → bail (the interpreter re-executes the
                // op: throws on a non-object RHS, runs an object-key ToString, or
                // dispatches a Proxy `has` trap). PURE — no alloc, no user code,
                // so no r13/r14/TA refetch.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(key)]         // key bits (arg1)
                    ; mov r8, [rbx + dreg(obj)]          // obj bits (arg2)
                    ; mov rax, QWORD heap.has_property as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // proxy / coercion / throw → interp
                    ; mov [rbx + dreg(dst)], rax         // Bool Value bits
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Lt { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Lt),
            Instr::Le { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Le),
            Instr::Gt { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Gt),
            Instr::Ge { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Ge),
            // `===` / `!==` are polymorphic: numeric operands compare as f64,
            // interned single-char strings / Int / Bool / Null / Undefined
            // compare by bits, non-interned heap operands bail to the interpreter.
            Instr::Eq { dst, a, b } => {
                region_poly_eq(&mut ops, ip, bail, epilogue, dst, a, b, false, heap.strict_eq)
            }
            Instr::Ne { dst, a, b } => {
                region_poly_eq(&mut ops, ip, bail, epilogue, dst, a, b, true, heap.strict_eq)
            }
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                // An Int/Bool condition tests its payload directly; anything
                // else (double/heap/undefined/null) asks the read-only
                // `jit_truthy` helper — `while (obj)` / `if (!s)` loop shapes
                // stay native instead of deopting every iteration.
                let if_false = matches!(proto.code[ip], Instr::JumpIfFalse { .. });
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                let testit = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(cond)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32          // Int
                    ; je => testit
                    ; cmp r10d, (INT_TAG_HI + 1) as i32    // Bool
                    ; je => testit
                    ; mov rcx, rdi                         // vm
                    ; mov rdx, rax                         // value bits
                    ; mov rax, QWORD heap.truthy as i64
                    ; call rax                             // rax = 0/1
                    ; => testit
                    ; test eax, eax
                );
                if if_false {
                    dynasm!(ops ; jz => t);
                } else {
                    dynasm!(ops ; jnz => t);
                }
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
                // ── 8-way inline cache (CALL-FREE on hit) ── probe the site's
                // ways: receiver identity (obj_bits) + live receiver version,
                // then (for proto-chain ways) the live version of each guarded
                // hop; a full match reads the HOLDER's `vals_ptr[slot]`
                // directly. All ways miss ⇒ the helper re-fills one way. r14 =
                // IC table base, r13 = heap version-array base. See `IcEntry`
                // for the layout (stride 40, hops at +24/+32, u32::MAX = none).
                //
                // SAFETY (`[r13 + idx*4]` reads are in-bounds): the receiver
                // version is read only after the identity match against a
                // FILLED way, whose obj_bits the helper validated as a live
                // heap Object ⇒ heap_idx < versions.len() (which never
                // shrinks). Hop indices were likewise valid heap indices at
                // fill. Staleness is harmless for the LOADS (in-bounds) and
                // caught by the version compares before any vals deref.
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let probe = ops.new_dynamic_label();
                let next = ops.new_dynamic_label();
                let hit = ops.new_dynamic_label();
                let miss = ops.new_dynamic_label();
                let via_ic = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                let hop = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]          // receiver bits (probe-invariant)
                    ; lea r9, [r14 + off]                 // way 0 of this site
                    ; mov r8d, JIT_IC_WAYS as i32
                    ; => probe
                    ; cmp rax, [r9]                       // identity (empty 0 never matches)
                    ; jne => next
                    ; mov ecx, eax                        // recv heap idx (low 32)
                    ; mov edx, [r13 + rcx*4]              // live recv version
                    ; cmp edx, [r9 + 16]
                    ; jne => next
                    ; mov ecx, [r9 + 20]
                    ; shr ecx, 24                         // nhops (0 = own)
                    ; test ecx, ecx
                    ; jz => hit
                    ; lea r10, [r9 + 24]                  // hop cursor
                    ; => hop
                    ; mov edx, [r10]                      // hop heap idx
                    ; mov r11d, [r13 + rdx*4]             // live hop version
                    ; cmp r11d, [r10 + 4]
                    ; jne => next
                    ; add r10, 8
                    ; dec ecx
                    ; jnz => hop
                    ; => hit
                    ; mov rcx, [r9 + 8]                   // holder vals_ptr
                    ; mov edx, [r9 + 20]
                    ; and edx, 0x00FF_FFFF                // slot (low 24)
                    ; mov rax, [rcx + rdx*8]              // vals[slot] (CALL-FREE)
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => cont
                    ; => next
                    ; add r9, JIT_IC_STRIDE as i32
                    ; dec r8d
                    ; jnz => probe
                    ; jmp => miss
                    ; => miss
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits (rax survives the probe)
                    ; mov r8d, ic_site as i32             // site_idx
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov rax, QWORD heap.get_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD PROP_VIA_IC as i64
                    ; cmp rax, r10
                    ; je => via_ic
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => cont
                    // ── accessor / class receiver: the interpreter-IC slow
                    // helper resolves it (and may frame-call a getter — user
                    // code, so r13/r14 are re-derived afterwards).
                    ; => via_ic
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // caller window base
                    ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                    ; mov r9, QWORD (((name as u64) << 32) | obj as u64) as i64
                    ; mov rax, QWORD heap.get_prop_slow as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                // The slow helper may have frame-called user code (accessor) —
                // re-derive the pinned TypedArray snapshots too.
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                dynasm!(ops ; => cont);
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::SetProp { obj, name, val } => {
                // ── 8-way inline cache (CALL-FREE write on hit) ── like
                // GetProp, but the helper only ever fills OWN ways here
                // (identity + receiver version fully guard an own writable
                // data slot: any redefinition/freeze/delete/proto change bumps
                // the version), so the probe skips the hop checks.
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let probe = ops.new_dynamic_label();
                let next = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]          // receiver bits
                    ; lea r9, [r14 + off]
                    ; mov r8d, JIT_IC_WAYS as i32
                    ; => probe
                    ; cmp rax, [r9]                       // identity
                    ; jne => next
                    ; mov ecx, eax                        // recv heap idx
                    ; mov edx, [r13 + rcx*4]              // live recv version
                    ; cmp edx, [r9 + 16]
                    ; jne => next
                    ; mov rcx, [r9 + 8]                   // vals_ptr
                    ; mov edx, [r9 + 20]                  // slot
                    ; mov r10, [rbx + dreg(val)]          // val_bits
                    ; mov [rcx + rdx*8], r10              // vals[slot] = val (CALL-FREE)
                    ; jmp => cont
                    ; => next
                    ; add r9, JIT_IC_STRIDE as i32
                    ; dec r8d
                    ; jnz => probe
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits
                    ; mov r8, [rbx + dreg(val)]           // val_bits
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov QWORD [rsp + 32], ic_site as i32 // 5th arg: site_idx (stack)
                    ; mov rax, QWORD heap.set_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD PROP_VIA_IC as i64
                    ; cmp rax, r10
                    ; jne => cont
                    // ── setter / class receiver: interpreter-IC slow helper
                    // (may frame-call a setter — re-derive r13/r14 after).
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // caller window base
                    ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                    ; mov r9, QWORD (((name as u64) << 32) | ((obj as u64) << 16) | val as u64) as i64
                    ; mov rax, QWORD heap.set_prop_slow as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                // The slow helper may have frame-called user code (accessor) —
                // re-derive the pinned TypedArray snapshots too.
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                dynasm!(ops ; => cont);
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::ToPropKey { dst, obj, src } => {
                // `dst = ToPropertyKey(src)` for `o[k] op= v` / `o[k]++`: a
                // NUMBER key (Int or double) coerces to itself, so the op is a
                // move once the base is known non-nullish (the interpreter's
                // RequireObjectCoercible order). A nullish base (throw) or a
                // non-number key (observable toString/valueOf, or a heap
                // string/Symbol — rare in hot loops) bails to the interpreter.
                let tpk_ok = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]
                    ; shr rax, 48
                    ; cmp eax, (INT_TAG_HI + 2) as i32     // 0x7FFB Null
                    ; je => bail
                    ; cmp eax, (INT_TAG_HI + 3) as i32     // 0x7FFC Undefined
                    ; je => bail
                    ; mov rax, [rbx + dreg(src)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32          // Int key
                    ; je => tpk_ok
                    ; sub r10d, (INT_TAG_HI + 1) as i32
                    ; cmp r10d, 3                          // Bool/Null/Undef/Heap
                    ; jbe => bail                          //  → interpreter
                    ; => tpk_ok                            // double key
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::GetIndex { dst, obj, key } => {
                // ── pinned-TypedArray fast path ── when the OSR-time plan tied
                // this access to a pin: identity-guard the receiver against the
                // pin's snapshot, bounds-check against the snapshot len, then a
                // DIRECT machine load + dtype conversion (no call). Guard miss →
                // the generic helper below; OOB / non-integer key / invalidated
                // snapshot → DEOPT (the interpreter re-executes this op with
                // full semantics — OOB reads are rare in real code).
                let pinned = ta_plan
                    .access
                    .get(&ip)
                    .map(|&j| (j as usize, ta_plan.pins[j as usize].kind));
                let (ta_slow, ta_done) = (ops.new_dynamic_label(), ops.new_dynamic_label());
                if let Some((slot, kind)) = pinned {
                    let off = ta_slot_off(slot);
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(obj)]      // receiver bits
                        ; cmp rax, [rsp + off]            // identity vs snapshot
                        ; jne => ta_slow
                    );
                    emit_ta_key(&mut ops, key, bail);     // rcx = i64 index
                    dynasm!(ops
                        ; cmp rcx, [rsp + off + 16]       // unsigned: i < len?
                        ; jae => bail                     // OOB/negative → deopt
                        ; mov rdx, [rsp + off + 8]        // pinned data base
                    );
                    match kind {
                        0 => {
                            dynasm!(ops ; movsx eax, BYTE [rdx + rcx]);
                            box_eax(&mut ops, dst);
                        }
                        1 | 2 => {
                            dynasm!(ops ; movzx eax, BYTE [rdx + rcx]);
                            box_eax(&mut ops, dst);
                        }
                        3 => {
                            dynasm!(ops ; movsx eax, WORD [rdx + rcx * 2]);
                            box_eax(&mut ops, dst);
                        }
                        4 => {
                            dynasm!(ops ; movzx eax, WORD [rdx + rcx * 2]);
                            box_eax(&mut ops, dst);
                        }
                        5 => {
                            dynasm!(ops ; mov eax, [rdx + rcx * 4]);
                            box_eax(&mut ops, dst);
                        }
                        6 => {
                            // u32: Int when it fits i32 (mirrors Value::num),
                            // else the exact double (same as the `>>>` boxing).
                            dynasm!(ops ; mov eax, [rdx + rcx * 4]);
                            emit_box_u32(&mut ops, dst);
                        }
                        _ => {
                            // 7/8 (f32/f64): box the double, NaN-canonicalised.
                            if kind == 7 {
                                dynasm!(ops
                                    ; movss xmm0, [rdx + rcx * 4]
                                    ; cvtss2sd xmm0, xmm0
                                );
                            } else {
                                dynasm!(ops ; movsd xmm0, [rdx + rcx * 8]);
                            }
                            emit_box_f64_canon(&mut ops, dst);
                        }
                    }
                    dynasm!(ops ; jmp => ta_done ; => ta_slow);
                }
                // Generic element read `a[i]` via a win64 helper (dense arrays,
                // flat-ASCII strings, and unpinned TypedArrays). Returns the
                // element bits, `undefined` for out-of-range, or the deopt
                // sentinel for receivers/keys needing interpreter semantics.
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
                if pinned.is_some() {
                    dynasm!(ops ; => ta_done);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::SetIndex { obj, key, val } => {
                // ── pinned-TypedArray fast path ── mirror of GetIndex: identity
                // guard, integer key, bounds check, then a direct dtype-encoded
                // store. The VALUE must already be a number (Int or double) —
                // anything else deopts, because ToNumber coercion is observable
                // user code the interpreter must run. OOB stores deopt (the
                // interpreter performs the spec'd coerce-then-silent-no-op).
                let pinned = ta_plan
                    .access
                    .get(&ip)
                    .map(|&j| (j as usize, ta_plan.pins[j as usize].kind));
                let (ta_slow, ta_done) = (ops.new_dynamic_label(), ops.new_dynamic_label());
                if let Some((slot, kind)) = pinned {
                    let off = ta_slot_off(slot);
                    let val_int = ops.new_dynamic_label();
                    let sdone = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(obj)]      // receiver bits
                        ; cmp rax, [rsp + off]            // identity vs snapshot
                        ; jne => ta_slow
                    );
                    emit_ta_key(&mut ops, key, bail);     // rcx = i64 index
                    dynasm!(ops
                        ; cmp rcx, [rsp + off + 16]       // unsigned: i < len?
                        ; jae => bail                     // OOB store → deopt
                        ; mov rdx, [rsp + off + 8]        // pinned data base
                        ; mov rax, [rbx + dreg(val)]      // value bits
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; je => val_int
                        ; sub r10d, (INT_TAG_HI + 1) as i32
                        ; cmp r10d, 3                     // tagged non-number →
                        ; jbe => bail                     // observable coercion
                    );
                    // ── double value (raw f64 bits in rax) ──
                    match kind {
                        8 => dynasm!(ops ; mov [rdx + rcx * 8], rax),
                        7 => dynasm!(ops
                            ; movq xmm0, rax
                            ; cvtsd2ss xmm0, xmm0
                            ; movss [rdx + rcx * 4], xmm0
                        ),
                        2 => {
                            // Uint8Clamped: round-half-even clamp via the pure
                            // helper (stores the byte itself; clobbers only
                            // volatile regs, and the store is the op's end).
                            dynasm!(ops
                                ; lea rcx, [rdx + rcx]        // element address
                                ; mov rdx, rax                // f64 bits
                                ; mov rax, QWORD heap.ta_clamp_store as i64
                                ; call rax
                            );
                        }
                        _ => {
                            // Int dtypes: JS modular wrap = the low bits of the
                            // i64 truncation. NaN/±Inf/|x|≥2^63 hit the 0x8000…
                            // sentinel → deopt (interpreter wraps/zeroes).
                            dynasm!(ops
                                ; movq xmm0, rax
                                ; cvttsd2si r10, xmm0
                                ; mov r11, QWORD i64::MIN
                                ; cmp r10, r11
                                ; je => bail
                            );
                            match kind {
                                0 | 1 => dynasm!(ops ; mov [rdx + rcx], r10b),
                                3 | 4 => dynasm!(ops ; mov [rdx + rcx * 2], r10w),
                                _ => dynasm!(ops ; mov [rdx + rcx * 4], r10d),
                            }
                        }
                    }
                    dynasm!(ops ; jmp => sdone ; => val_int);
                    // ── Int value (i32 payload in eax) ──
                    match kind {
                        8 => dynasm!(ops
                            ; cvtsi2sd xmm0, eax
                            ; movsd [rdx + rcx * 8], xmm0
                        ),
                        7 => dynasm!(ops
                            ; cvtsi2ss xmm0, eax
                            ; movss [rdx + rcx * 4], xmm0
                        ),
                        2 => dynasm!(ops
                            // Integer clamp to [0,255] (no rounding needed).
                            ; xor r10d, r10d
                            ; test eax, eax
                            ; cmovs eax, r10d
                            ; mov r10d, 255
                            ; cmp eax, r10d
                            ; cmova eax, r10d
                            ; mov [rdx + rcx], al
                        ),
                        0 | 1 => dynasm!(ops ; mov [rdx + rcx], al),
                        3 | 4 => dynasm!(ops ; mov [rdx + rcx * 2], ax),
                        _ => dynasm!(ops ; mov [rdx + rcx * 4], eax),
                    }
                    dynasm!(ops ; => sdone ; jmp => ta_done ; => ta_slow);
                }
                // Generic element write `a[i] = v` via a win64 helper (dense
                // arrays — store/grow — and unpinned TypedArrays with number
                // values). Returns 0 (ok) or the deopt sentinel.
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
                if pinned.is_some() {
                    dynasm!(ops ; => ta_done);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::CallMethod { dst, obj, name, arg_base, argc } => {
                let key = proto.string_constants[name as usize].as_str();
                if (argc == 1 || argc == 2) && dv_get_kind(key).is_some() {
                    // Whitelisted DataView `get*(pos[, littleEndian])`.
                    // ── pinned-DataView fast path ── when the OSR plan pinned
                    // this receiver: identity guard, integral number pos,
                    // signed bounds check vs the pinned byteLength, then a
                    // direct (optionally byte-swapped) load. A double/heap
                    // littleEndian falls to the helper (full ToBoolean).
                    let kindid = dv_get_kind(key).unwrap();
                    let pinned = ta_plan
                        .access
                        .get(&ip)
                        .filter(|&&j| ta_plan.pins[j as usize].kind == DV_PIN_KIND)
                        .map(|&j| j as usize);
                    let (dv_slow, dv_done) =
                        (ops.new_dynamic_label(), ops.new_dynamic_label());
                    if let Some(slot) = pinned {
                        let off = ta_slot_off(slot);
                        let size = [1i32, 1, 1, 2, 2, 4, 4, 4, 8][kindid as usize];
                        dynasm!(ops
                            ; mov rax, [rbx + dreg(obj)]      // receiver bits
                            ; cmp rax, [rsp + off]            // identity vs snapshot
                            ; jne => dv_slow
                        );
                        emit_ta_key(&mut ops, arg_base, bail); // rcx = i64 pos
                        dynasm!(ops
                            ; test rcx, rcx
                            ; js => bail                      // negative → RangeError
                            ; mov r10, [rsp + off + 16]       // byteLength
                            ; sub r10, size
                            ; cmp rcx, r10                    // signed: pos > len-size
                            ; jg => bail                      //  (incl. len < size)
                            ; mov rdx, [rsp + off + 8]        // pinned data base
                        );
                        // littleEndian: only multi-byte kinds look at it. The
                        // inline path accepts Int/Bool/Null/Undefined (payload
                        // ≠ 0 ⇔ true — exactly ToBoolean for those tags);
                        // a double/heap flag falls to the helper.
                        let le_big = ops.new_dynamic_label();
                        let loaded = ops.new_dynamic_label();
                        if size > 1 {
                            if argc == 2 {
                                dynasm!(ops
                                    ; mov rax, [rbx + dreg(arg_base + 1)]
                                    ; mov r10, rax
                                    ; shr r10, 48
                                    ; sub r10d, INT_TAG_HI as i32
                                    ; cmp r10d, 3             // Int/Bool/Null/Undef
                                    ; ja => dv_slow           // double/heap → helper
                                    ; test eax, eax           // payload ≠ 0 ⇔ true
                                    ; jz => le_big            // falsy → big-endian
                                );
                            } else {
                                // Absent flag = undefined = big-endian.
                                dynasm!(ops ; jmp => le_big);
                            }
                        }
                        // ── little-endian load ──
                        match kindid {
                            0 => dynasm!(ops ; movsx eax, BYTE [rdx + rcx]),
                            1 => dynasm!(ops ; movzx eax, BYTE [rdx + rcx]),
                            3 => dynasm!(ops ; movsx eax, WORD [rdx + rcx]),
                            4 => dynasm!(ops ; movzx eax, WORD [rdx + rcx]),
                            5 | 6 => dynasm!(ops ; mov eax, [rdx + rcx]),
                            7 => dynasm!(ops ; movss xmm0, [rdx + rcx] ; cvtss2sd xmm0, xmm0),
                            _ => dynasm!(ops ; movsd xmm0, [rdx + rcx]),
                        }
                        if size > 1 {
                            dynasm!(ops ; jmp => loaded ; => le_big);
                            // ── big-endian load (byte-swapped) ──
                            match kindid {
                                3 => dynasm!(ops
                                    ; movzx eax, WORD [rdx + rcx]
                                    ; rol ax, 8
                                    ; movsx eax, ax
                                ),
                                4 => dynasm!(ops
                                    ; movzx eax, WORD [rdx + rcx]
                                    ; rol ax, 8
                                ),
                                5 | 6 => dynasm!(ops
                                    ; mov eax, [rdx + rcx]
                                    ; bswap eax
                                ),
                                7 => dynasm!(ops
                                    ; mov eax, [rdx + rcx]
                                    ; bswap eax
                                    ; movd xmm0, eax
                                    ; cvtss2sd xmm0, xmm0
                                ),
                                _ => dynasm!(ops
                                    ; mov rax, [rdx + rcx]
                                    ; bswap rax
                                    ; movq xmm0, rax
                                ),
                            }
                            dynasm!(ops ; => loaded);
                        }
                        match kindid {
                            6 => emit_box_u32(&mut ops, dst),
                            7 | 8 => emit_box_f64_canon(&mut ops, dst),
                            _ => box_eax(&mut ops, dst),
                        }
                        dynasm!(ops ; jmp => dv_done ; => dv_slow);
                    }
                    // Generic path: the dedicated win64 helper (receiver + pos
                    // + le bits in, element kind via the 5th-arg slot; result
                    // bits out, deopt sentinel → bail). No alloc, no user code
                    // — no re-fetch.
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                        ; mov r8, [rbx + dreg(arg_base)]      // pos bits
                    );
                    if argc == 2 {
                        dynasm!(ops ; mov r9, [rbx + dreg(arg_base + 1)]);
                    } else {
                        dynasm!(ops ; mov r9, QWORD Value::UNDEFINED.bits() as i64);
                    }
                    dynasm!(ops
                        ; mov QWORD [rsp + 32], kindid as i32 // 5th arg: kind
                        ; mov rax, QWORD heap.dv_get as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    if pinned.is_some() {
                        dynasm!(ops ; => dv_done);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else if argc == 1 && matches!(key, "push" | "charCodeAt") {
                    // The whitelisted 1-arg builtins keep their dedicated win64
                    // helpers: receiver + arg0 bits in, result bits out, deopt
                    // sentinel → bail. Neither allocates a heap OBJECT (push
                    // grows the array's own Vec; the versions array is
                    // untouched), so no pinned-pointer re-fetch is needed.
                    let helper = match key {
                        "push" => heap.array_push,
                        _ => heap.char_code_at,
                    };
                    // ── pinned-string charCodeAt fast path ── when the OSR plan
                    // pinned this receiver as a flat ASCII string (snapshot
                    // {obj_bits, bytes_ptr, units}): identity-guard the receiver,
                    // materialise the index, then a DIRECT byte load (byte i ==
                    // UTF-16 unit i for ASCII). Out of range → NaN (charCodeAt
                    // OOB semantics, == the helper's `unit_at None → NaN`). A
                    // guard miss / non-integral index / a re-snapshot that found
                    // the string non-ASCII (slot {0,0,0} → identity miss) falls
                    // through to the UNCHANGED generic helper below.
                    let str_pin = (key == "charCodeAt")
                        .then(|| ta_plan.access.get(&ip))
                        .flatten()
                        .filter(|&&j| ta_plan.pins[j as usize].kind == STR_PIN_KIND)
                        .map(|&j| j as usize);
                    let cc_done = ops.new_dynamic_label();
                    if let Some(slot) = str_pin {
                        let off = ta_slot_off(slot);
                        let cc_slow = ops.new_dynamic_label();
                        let cc_oob = ops.new_dynamic_label();
                        dynasm!(ops
                            ; mov rax, [rbx + dreg(obj)]      // receiver bits
                            ; cmp rax, [rsp + off]            // identity vs snapshot
                            ; jne => cc_slow                  // miss → generic helper
                        );
                        // Index → rcx (signed i64). Non-int/fractional/NaN bails to
                        // the interpreter — exactly the helper's deopt for those.
                        emit_ta_key(&mut ops, arg_base, bail);
                        dynasm!(ops
                            ; test rcx, rcx
                            ; js => cc_slow                   // negative → helper (array_index None → deopt)
                            ; mov r10, [rsp + off + 16]       // units (== ASCII byte len)
                            ; cmp rcx, r10
                            ; jae => cc_oob                   // i >= len → NaN
                            ; mov rdx, [rsp + off + 8]        // pinned bytes base
                            ; movzx eax, BYTE [rdx + rcx]     // ASCII code unit
                        );
                        box_eax(&mut ops, dst);
                        dynasm!(ops
                            ; jmp => cc_done
                            ; => cc_oob
                            ; mov rax, QWORD QNAN_BITS as i64 // charCodeAt OOB → NaN
                            ; mov [rbx + dreg(dst)], rax
                            ; jmp => cc_done
                            ; => cc_slow
                        );
                    }
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
                    if str_pin.is_some() {
                        dynasm!(ops ; => cc_done);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else {
                    // Generic `obj.m(args…)`: the interpreter-IC call helper
                    // (see `emit_region_call_ic`). Packing: r9 = (name<<32) |
                    // (obj<<16) | arg_base; argc via the stack.
                    let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                    let packed_args =
                        ((name as u64) << 32) | ((obj as u64) << 16) | arg_base as u64;
                    emit_region_call_ic(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        heap.call_method_ic,
                        packed_fip,
                        packed_args,
                        argc,
                        dst,
                        refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                        ta_refetch,
                    );
                }
            }
            Instr::Call { dst, callee, arg_base, argc } => {
                // Generic `f(args…)` with `this = undefined`: the interpreter-IC
                // call helper. Packing: r9 = (callee<<16) | arg_base.
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let packed_args = ((callee as u64) << 16) | arg_base as u64;
                // Q4 leaf-call inlining: a monomorphic plain-leaf callee at this
                // site is inlined with an identity guard; a guard miss / tight
                // headroom falls through to the SAME helper below (a pure prefix).
                if let Some(lp) = leaf_plan.get(&ip) {
                    emit_inline_leaf_call(
                        &mut ops,
                        ip,
                        epilogue,
                        leaf_flag_off,
                        lp,
                        callee,
                        arg_base,
                        argc,
                        dst,
                        heap.math_unary,
                        heap.math_two,
                        heap.call_ic,
                        packed_fip,
                        packed_args,
                        refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                        ta_refetch,
                    );
                } else {
                    emit_region_call_ic(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        heap.call_ic,
                        packed_fip,
                        packed_args,
                        argc,
                        dst,
                        refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                        ta_refetch,
                    );
                }
            }
            Instr::StrConcat { dst, a, b } => {
                // `dst = a + b` via the win64 `jit_concat` helper (rope concat or
                // numeric add). Same ABI as the method helpers: vm + two operand
                // bits in, result bits out, deopt sentinel → bail. The helper
                // ALLOCATES (a rope node grows the heap's parallel version
                // array, which may reallocate) — so when the region also has
                // GetProp/SetProp (the r13 users), re-derive r13 after the
                // call. It never runs user code, so the IC table (r14) is safe.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // a bits
                    ; mov r8, [rbx + dreg(b)]             // b bits
                    ; mov rax, QWORD heap.concat as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                          // threw (pending_throw set) → unwind, NOT redo
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::StrAppendInPlace { dst, a, b } => {
                // In-place `dst = a + b` via `jit_str_append` (mutates a's buffer
                // when uniquely owned — the emitter proved linearity). Never
                // deopts, but uses the same ABI; allocates/grows the heap, so
                // (like StrConcat) re-derive r13 when the region reads it.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // a (accumulator) bits
                    ; mov r8, [rbx + dreg(b)]             // b (appended) bits
                    ; mov rax, QWORD heap.str_append as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
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

    // Hoisted-`.length` deopt landing: resume the loop in the interpreter.
    if hoisted_len.is_some() {
        dynasm!(ops
            ; => entry_len_bail
            ; mov DWORD [rsi], start as i32
            ; jmp => epilogue
        );
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
        ; add rsp, frame
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

/// Load `regs[reg]` as ToInt32 into `eax` for the region's `Bitwise` ops: an
/// Int-tagged payload directly, or a DOUBLE that is exactly integral in
/// (-2^63, 2^63) — for which ToInt32 is simply the low 32 bits of the i64
/// (modulo-2^32 wrap, signed), exactly what `(x + y) | 0` accumulators rely on
/// when the f64 sum crosses i32 range. Anything else — fractional / NaN / Inf /
/// |x| ≥ 2^63 (rare; ToInt32 still defined but not via i64) / bool / null /
/// undefined / heap — jumps to `bail` so the interpreter applies complete
/// ToInt32 semantics. Clobbers rax/r10/xmm0/xmm1.
fn load_toint32(ops: &mut dynasmrt::x64::Assembler, reg: u16, bail: dynasmrt::DynamicLabel) {
    let int_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(reg)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32      // 0x7FFA (bool tag)
        ; cmp r10d, 3                            // high16 ∈ [0x7FFA,0x7FFD] ⇒ not a number
        ; jbe => bail
        // A double: accept an exact integral value; eax (the i64's low 32) IS
        // its ToInt32. NaN/±Inf/|x|≥2^63 fail the round-trip (cvttsd2si yields
        // the 0x8000… sentinel, which converts back to -2^63 ≠ x) → bail.
        ; movq xmm0, rax
        ; cvttsd2si rax, xmm0                    // i64 trunc (NaN/±Inf → 0x8000…)
        ; cvtsi2sd xmm1, rax
        ; ucomisd xmm1, xmm0
        ; jne => bail                            // fractional (or the NaN/Inf sentinel)
        ; jp => bail                             // NaN (unordered)
        ; jmp => done
        ; => int_path
        // eax already holds the i32 payload (low 32 of the boxed Value).
        ; => done
    );
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
        ; xorps Rx(which), Rx(which)             // break cvtsi2sd's false dep
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

/// `regs[dst] = regs[a] <op> regs[b]`. Add/Sub/Mul take an INT fast path when
/// both operands are Int-tagged (32-bit op + overflow check, result boxed Int —
/// exactly the interpreter's `checked_add/sub/mul` fast path), falling to the
/// f64 path on a non-Int operand or overflow. Keeping Int results Int matters
/// downstream: `(x+y)|0` accumulators and `a[i+1]` keys then take their cheap
/// Int paths instead of the double→int round-trip. Div is always f64 (JS `/`
/// has no integer form — mirrors the interpreter). Guards operands are numbers.
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
    int_hint: bool,
) {
    let f64_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    // Mul is EXCLUDED from the int fast path: hot integer multiplies (hash
    // mixing `i * 40503`) overflow i32 after a few thousand iterations and
    // would then pay the failed int attempt PLUS the f64 redo every time.
    let int_ok = int_hint && matches!(op, DOp::Add | DOp::Sub);
    if int_ok {
        dynasm!(ops
            ; mov rax, [rbx + dreg(a)]
            ; mov rcx, [rbx + dreg(b)]
            ; mov r10, rax
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
            ; jne => f64_path
            ; mov r10, rcx
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
            ; jne => f64_path
        );
        match op {
            DOp::Add => dynasm!(ops ; add eax, ecx ; jo => f64_path),
            DOp::Sub => dynasm!(ops ; sub eax, ecx ; jo => f64_path),
            DOp::Mul => dynasm!(ops ; imul eax, ecx ; jo => f64_path),
            DOp::Div => unreachable!(),
        }
        box_eax(ops, dst);
        // f64 fallback re-loads both operands from the register file, so the
        // clobbered eax (wrapped overflow value) is irrelevant.
        dynasm!(ops ; jmp => done ; => f64_path);
    }
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    match op {
        DOp::Add => dynasm!(ops ; addsd xmm0, xmm1),
        DOp::Sub => dynasm!(ops ; subsd xmm0, xmm1),
        DOp::Mul => dynasm!(ops ; mulsd xmm0, xmm1),
        DOp::Div => dynasm!(ops ; divsd xmm0, xmm1),
    }
    store_xmm(ops, dst);
    if int_ok {
        dynasm!(ops ; => done);
    }
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
/// Element-kind id for a whitelisted DataView `get*` method name (the kinds the
/// `jit_dv_get` helper decodes without allocating). `None` for everything else
/// (set*, BigInt64/BigUint64 and Float16 getters stay on the generic path).
pub fn dv_get_kind(key: &str) -> Option<u8> {
    match key {
        "getInt8" => Some(0),
        "getUint8" => Some(1),
        "getInt16" => Some(3),
        "getUint16" => Some(4),
        "getInt32" => Some(5),
        "getUint32" => Some(6),
        "getFloat32" => Some(7),
        "getFloat64" => Some(8),
        _ => None,
    }
}

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
///      string) → the read-only `jit_strict_eq` helper (full `strict_eq`
///      semantics: equal-content strings, BigInts, identity for objects) —
///      `line === "##"` scans stay native instead of deopting.
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
    strict_eq_helper: usize,
) {
    let numeric = ops.new_dynamic_label();
    let a_not_heap = ops.new_dynamic_label();
    let do_bits = ops.new_dynamic_label();
    let store = ops.new_dynamic_label();
    let slow = ops.new_dynamic_label();
    let after = ops.new_dynamic_label();
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
        ; jae => slow                          // a: non-interned heap → helper
        ; => a_not_heap
        ; mov rdx, rcx
        ; shr rdx, 48
        ; cmp edx, TAG_HEAP_HI as i32
        ; jne => do_bits
        ; mov rdx, rcx
        ; mov r9, QWORD PAYLOAD_MASK as i64
        ; and rdx, r9
        ; cmp rdx, USER_OBJ_START as i32
        ; jae => slow                          // b: non-interned heap → helper
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
        ; jmp => after
        // ── slow path: full strict_eq via the read-only helper. ──
        ; => slow
        ; mov rcx, rdi                         // vm
        ; mov rdx, [rbx + dreg(a)]
        ; mov r8, [rbx + dreg(b)]
        ; mov rax, QWORD strict_eq_helper as i64
        ; call rax                             // rax = 0/1 (a === b)
    );
    if ne {
        dynasm!(ops ; xor rax, 1);
    }
    dynasm!(ops
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
        ; => after
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
