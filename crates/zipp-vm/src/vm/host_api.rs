//! The rich-value half of the embedding API.
//!
//! [`crate::embed`] marshals primitives and renders everything else via
//! `ToString`, which is the right minimum for a host that wants a number back.
//! A host that keeps a UI in sync with the script needs more than that: it has
//! to read a global holding an array of objects, hand it to its own renderer,
//! and write it back afterwards. That is what this module adds — a structural
//! walk between the engine's `Value`/`HeapObj` graph and an owned
//! [`HostValue`] tree.
//!
//! Three deliberate limits, each because the alternative is worse:
//!
//! - **Only data crosses.** Functions, classes, `Map`/`Set`/`Date`/`RegExp`,
//!   typed arrays and proxies marshal to [`HostValue::Opaque`], never to a live
//!   reference — a `Value` is a heap INDEX whose meaning depends on this VM, so
//!   handing one out would be handing out a dangling reference the moment the
//!   collector moves. A host that wants a function's result should call it.
//! - **Writes skip opaque slots.** Setting a global that currently holds a
//!   function or class is a no-op rather than a clobber, so a host that reads
//!   its whole state, edits one field and writes it all back cannot destroy the
//!   script's own functions on the round trip.
//! - **Cycles become `Null` and depth is capped.** An object graph the host
//!   cannot represent must not become a hang or a stack overflow.
//!
//! Why not JSON, which would be far less code: `JSON.stringify` DROPS
//! function-valued properties and THROWS on a cycle, so it cannot express
//! either of the two rules above, and it would put a UTF-8 encode plus a
//! reparse on a path a host may run every frame.

use crate::bytecode::Program;
use crate::heap::{HeapObj, ObjMap, PropAttr};
use crate::value::Value;
use crate::vm::Vm;
use rustc_hash::FxHashMap;
use std::borrow::Cow;

/// Encode UTF-16 code units as WTF-8: pairs become one four-byte sequence,
/// a lone surrogate its own three-byte sequence (which is what makes the
/// result WTF-8 rather than UTF-8), everything else ordinary UTF-8.
pub(crate) fn utf16_to_wtf8(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len() * 3);
    let mut i = 0;
    while i < units.len() {
        let u = units[i] as u32;
        let cp = if (0xD800..0xDC00).contains(&u)
            && i + 1 < units.len()
            && (0xDC00..0xE000).contains(&(units[i + 1] as u32))
        {
            i += 1;
            0x10000 + ((u - 0xD800) << 10) + (units[i] as u32 - 0xDC00)
        } else {
            u
        };
        i += 1;
        match cp {
            0..=0x7F => out.push(cp as u8),
            0x80..=0x7FF => {
                out.push(0xC0 | (cp >> 6) as u8);
                out.push(0x80 | (cp & 0x3F) as u8);
            }
            0x800..=0xFFFF => {
                out.push(0xE0 | (cp >> 12) as u8);
                out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
                out.push(0x80 | (cp & 0x3F) as u8);
            }
            _ => {
                out.push(0xF0 | (cp >> 18) as u8);
                out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
                out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
                out.push(0x80 | (cp & 0x3F) as u8);
            }
        }
    }
    out
}

/// Old-object size above which a write-back merge indexes the old keys
/// instead of scanning them. Below it a scan over a handful of short keys is
/// cheaper than building a table.
const MERGE_INDEX_THRESHOLD: usize = 8;

#[cfg(test)]
mod merge_tests;

#[cfg(test)]
thread_local! {
    /// Key probes made by `host_in_over` merges on this thread — one per
    /// hash lookup or per compared key in the small-object scan. A test
    /// bound on this is what proves the merge is linear, independently of
    /// the machine's clock.
    pub(crate) static MERGE_KEY_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[inline]
fn merge_probe() {
    #[cfg(test)]
    MERGE_KEY_PROBES.with(|c| c.set(c.get() + 1));
}

/// Byte offset of `Vm::jit_call_depth`, for Tier C's `TailCall` depth guard —
/// the emitted code reads the counter as `[vm + off]` before the tail site's
/// `Call`, and bails to the interpreter's frame-reuse arm at the cap. The
/// field is private to `vm`, so the offset is computed here, inside the module
/// tree that can see it (the `JIT_RECURSE_DEPTH_OFFSET` precedent).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_CALL_DEPTH_OFFSET: usize = core::mem::offset_of!(Vm<'static>, jit_call_depth);

/// Byte offsets of VM-owned scalar epochs read by persistent native code.
///
/// `ScriptState` is movable, so compiled code must derive these addresses from
/// the live VM argument (`rdi`) on every entry. Baking `&vm.field` would leave a
/// dangling/stale address after an embedder moves the state between calls.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_GLOBAL_ROUTE_EPOCH_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, global_route_epoch);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_MI_CLASS_EPOCH_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, mi_class_epoch);
/// Exact `[[IsHTMLDDA]]` singleton mirror used by call-free loose-null
/// comparisons. The companion byte preserves `ZIPP_NO_HTMLDDA_SCALAR`'s
/// HashSet/counter ablation by routing heap operands back to the helper when
/// the scalar lane is disabled.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_HTMLDDA_IDX_OFFSET: usize = core::mem::offset_of!(Vm<'static>, htmldda_idx);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_HTMLDDA_SCALAR_ENABLED_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, htmldda_scalar_enabled);

/// VM-relative bases pinned by Tier-C whole-function entry code. These are
/// explicit mirrors rather than offsets into `Vec`: Rust does not expose a
/// stable `Vec` layout. Globals never grow after boot; the versions and IC
/// mirrors are refreshed at their sole growth sites.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_GLOBALS_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, globals_raw);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_VERSIONS_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, versions_raw);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_IC_TABLE_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, jit)
    + core::mem::offset_of!(crate::codegen::Jit, ic_table_raw);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const _: () = {
    assert!(JIT_GLOBALS_RAW_OFFSET % core::mem::align_of::<u64>() == 0);
    assert!(JIT_VERSIONS_RAW_OFFSET % core::mem::align_of::<u64>() == 0);
    assert!(JIT_IC_TABLE_RAW_OFFSET % core::mem::align_of::<u64>() == 0);
};

/// VM-relative byte offsets of the heap's shape/vals mirror bases (B178).
/// The shape-way probes load the base pointers through the live VM argument
/// on EVERY access — the mirror vectors grow when helpers allocate, and
/// unlike the pinned `r13` versions base nothing re-derives these, so a
/// baked address would dangle after growth.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_HOT_MIRROR_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, hot_mirror_raw);
/// Number of valid entries behind `JIT_HOT_MIRROR_RAW_OFFSET`.  A tagged heap
/// payload is still bounds-checked before emitted code indexes the mirror,
/// matching the defensive check made by the helper fallback.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_HOT_MIRROR_LEN_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, hot_mirror_len);
/// B195: the hot record's compile-checked layout — the emitted probes
/// address `base + idx*16` (one `lea` doubling the scale-8 index) and then
/// read the shape at +0, the fid at +4 and the vals base at +8.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_HOT_SHAPE_OFF: usize = 0;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_HOT_FID_OFF: usize = 4;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_HOT_VALS_OFF: usize = 8;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const _: () = {
    use crate::heap::HotMirror as H;
    assert!(core::mem::offset_of!(H, shape) == JIT_HOT_SHAPE_OFF);
    assert!(core::mem::offset_of!(H, fid) == JIT_HOT_FID_OFF);
    assert!(core::mem::offset_of!(H, vals) == JIT_HOT_VALS_OFF);
    assert!(core::mem::size_of::<H>() == 16);
    assert!(JIT_HOT_MIRROR_LEN_OFFSET % core::mem::align_of::<u32>() == 0);
};
/// VM-relative byte offset of the heap's cell-value mirror base (B189): same
/// derive-per-access rule as the mirrors above.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_CELL_MIRROR_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, cell_vals_mirror_raw);
/// B201: the sticky nonempty bytes gating the emitted inline cell ops.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_CONST_CELLS_NE_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, const_cells_nonempty);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_FN_NAME_CELLS_NE_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, fn_name_cells_nonempty);
/// VM-relative byte offset of the running Tier-C activation's cached upvalue
/// base pointer (0 = none). Set per native entry, restored per exit; the
/// emitted `UpvalGet` derives it from the live VM argument on every access.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_ACT_UPVALS_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, jit_tierc_activation)
        + core::mem::offset_of!(crate::vm::TiercActivationState, upvals_raw);

/// B189b: base of the whole Tier-C activation state (24 repr(C) bytes the
/// emitted call lane saves, installs and restores as three qwords), plus the
/// compile-checked layout contract it depends on.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_ACTIVATION_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, jit_tierc_activation);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const _: () = {
    use crate::vm::TiercActivationState as A;
    assert!(core::mem::offset_of!(A, active) == 0);
    assert!(core::mem::offset_of!(A, frame_free) == 1);
    assert!(core::mem::offset_of!(A, closure) == 4);
    assert!(core::mem::offset_of!(A, callee) == 8);
    assert!(core::mem::offset_of!(A, upvals_raw) == 16);
    assert!(core::mem::size_of::<A>() == 24);
};
/// B189b mirrors for the emitted call lane: a Closure occupant's captured
/// `this` bits and (B243, emitted again) its upvalue base, which the inline
/// activation install reads per call exactly as `jit_cross3_enter` did
/// through `upvals_mirror_of`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_THIS_MIRROR_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, this_mirror_raw);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_UPVALS_MIRROR_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, upvals_mirror_raw);

/// B244: VM-relative offset of the saturating dense-Array snapshot epoch.
/// Emitted code reads this scalar after a native cross call; equality with its
/// stack-cached copy licenses reuse of Array raw bases only when the value is
/// not `u64::MAX` (the permanently-dirty saturation state).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_ARRAY_SNAPSHOT_EPOCH_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, array_snapshot_epoch);
/// VM-relative offset of `Heap::gen_raw` (B264 inline dense store lane).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_GEN_RAW_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, heap) + core::mem::offset_of!(crate::heap::Heap, gen_raw);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const _: () = assert!(JIT_GEN_RAW_OFFSET % core::mem::align_of::<u64>() == 0);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const _: () = {
    assert!(core::mem::size_of::<u64>() == 8);
    assert!(JIT_ARRAY_SNAPSHOT_EPOCH_OFFSET % core::mem::align_of::<u64>() == 0);
};

/// B243: explicit register-file fields and its high-water mark, for the inline
/// window open/close. Native code reads the mirrored exposed allocation address
/// and writes only the logical length; it never depends on `Vec`'s private
/// layout or retains a Rust raw pointer across safe element references.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_REGS_PTR_OFFSET: usize = core::mem::offset_of!(Vm<'static>, regs)
    + core::mem::offset_of!(crate::vm::RegisterFile, ptr_mirror);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_REGS_LEN_OFFSET: usize = core::mem::offset_of!(Vm<'static>, regs)
    + core::mem::offset_of!(crate::vm::RegisterFile, logical_len);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_REGS_HW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, regs_hw);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const _: () = {
    use crate::vm::RegisterFile as R;
    assert!(core::mem::offset_of!(R, ptr_mirror) == 0);
    assert!(core::mem::offset_of!(R, logical_len) == core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(R, storage) == 2 * core::mem::size_of::<usize>());
    assert!(JIT_REGS_PTR_OFFSET % core::mem::align_of::<usize>() == 0);
    assert!(JIT_REGS_LEN_OFFSET % core::mem::align_of::<usize>() == 0);
};

/// B243: the activation root stack, scanned by the GC as `slots[..depth]`
/// and pushed/popped by the emitted lane as `rdi + SLOTS + depth*24` /
/// `inc`/`sub` on `depth`. Layout contract, compile-checked below.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_ROOT_DEPTH_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, jit_tierc_activation_stack)
        + core::mem::offset_of!(crate::vm::ActivationRootStack, depth);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_ROOT_SLOTS_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, jit_tierc_activation_stack)
        + core::mem::offset_of!(crate::vm::ActivationRootStack, slots);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const _: () = {
    use crate::vm::ActivationRootStack as R;
    assert!(core::mem::offset_of!(R, depth) == 0);
    assert!(core::mem::offset_of!(R, slots) == 8);
    assert!(core::mem::size_of::<R>() == 8 + 24 * crate::vm::TIER_C_ACTIVATION_ROOT_STACK_MAX);
    assert!(crate::vm::TIER_C_ACTIVATION_ROOT_STACK_MAX == 62);
};
/// B189b native GC-due guard: the emitted lane calls only when NO collection
/// is pending (`maybe_gc` would be a no-op); a pending request routes to the
/// helper, whose safe point runs it.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_GC_REQUESTED_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, gc_requested);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_GC_STRESS_OFFSET: usize = core::mem::offset_of!(Vm<'static>, gc_stress);
/// B199: raw base of the live cross-entry table, derived through the VM per
/// access (growth re-caches it). Records are 16 bytes: entry @+0 (0 = none),
/// mask_gen @+8 — the lane addresses `[base + fid*16 (+8)]` with the fid a
/// baked constant displacement.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_CROSS_TABLE_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, jit)
    + core::mem::offset_of!(crate::codegen::Jit, cross_table_raw);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const _: () = {
    use crate::codegen::CrossEntryRec as R;
    assert!(core::mem::offset_of!(R, entry) == 0);
    assert!(core::mem::offset_of!(R, mask_gen) == 8);
    assert!(core::mem::size_of::<R>() == 16);
};

/// VM-relative byte offsets of the three call-environment blocker bytes
/// (B189): each is a [`crate::vm::JitGuardedMap`]'s `nonempty_raw`. The
/// same-proto call lane requires all three to read 0 — a non-empty map means
/// realm transitions or eval scopes may apply to this callee, which only the
/// helper's full preflight can decide.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[allow(dead_code)] // consumed by the B189b emitted same-proto call lane
pub(crate) const JIT_OBJ_REALM_NONEMPTY_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, obj_realm)
        + core::mem::offset_of!(crate::vm::JitGuardedMap, nonempty_raw);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[allow(dead_code)] // consumed by the B189b emitted same-proto call lane
pub(crate) const JIT_EVAL_SCOPE_NONEMPTY_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, closure_eval_scope)
        + core::mem::offset_of!(crate::vm::JitGuardedMap, nonempty_raw);
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[allow(dead_code)] // consumed by the B189b emitted same-proto call lane
pub(crate) const JIT_REALM_GLOBALS_NONEMPTY_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, realm_global_objs)
        + core::mem::offset_of!(crate::vm::JitGuardedMap, nonempty_raw);

/// Whether a global slot holds a top-level function/class declaration or an
/// ordinary variable. Hosts use this to decide what is callable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolScope {
    /// A top-level `var`, `let` or `const`.
    Variable,
    /// A top-level `function` or `class` declaration.
    Function,
}

/// One top-level binding: its name, its stable global slot, and what kind of
/// declaration produced it.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub index: u32,
    pub scope: SymbolScope,
}

/// A JS value marshalled out of (or into) the VM as owned data.
///
/// Unlike [`crate::embed::JsValue`] this is a TREE: arrays and plain objects
/// cross with their contents, so a host can read structured state without
/// round-tripping it through JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum HostValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    /// A well-formed JavaScript string (every UTF-16 code unit paired), as
    /// UTF-8. Every string a host can build from Rust text lands here.
    String(String),
    /// A JavaScript string that is NOT well-formed UTF-16 — it holds a lone
    /// surrogate — as its exact code units, so nothing is replaced on the way
    /// out or back in (the 11 September 2026 audit's ZIPP-11). Produced only
    /// for such strings; a host that never makes one never sees this. Writing
    /// one recreates the exact string in the guest.
    Utf16(Vec<u16>),
    Array(Vec<HostValue>),
    /// A plain object, as its own enumerable data properties in insertion
    /// order. Accessors are not invoked and do not appear.
    Object(Vec<(String, HostValue)>),
    /// Something that cannot cross as data: a function, class, `Map`, `Set`,
    /// `Date`, `RegExp`, typed array, proxy, … Reading one yields `Opaque`;
    /// writing one is ignored.
    Opaque,
}

/// What a VM currently retains and has spent, as counters a host can read
/// between re-entries: the first stage of the 11 September 2026 audit's
/// ZIPP-06 (measure retained compiled code before attempting to reclaim it).
///
/// The dynamic-code figures are the recorder's lifetime counters: compilation
/// attempts, the source bytes they were charged, and the stable-address
/// function and class definitions retained by successful ones — which is
/// what `dispose()` does NOT reclaim within one WASM instance. They are exact
/// counts, not allocator bytes: the compiler's own allocations are not
/// introspectable, so a host compares these against the WASM instance's
/// linear-memory pages and its process memory separately, and recycles the
/// instance when the retained definitions have grown past what it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceUsage {
    /// Payload-aware resident guest heap estimate, in bytes.
    pub heap_bytes: usize,
    /// Bytecode instructions executed since limits were set (0 without a
    /// recorder).
    pub steps_used: u64,
    /// Dynamic compilations attempted (`eval`, `Function`, `ShadowRealm`,
    /// host eval), successful or not.
    pub dynamic_code_calls: usize,
    /// Source bytes those attempts were charged.
    pub dynamic_code_source_bytes: usize,
    /// Function definitions installed by successful dynamic compilations and
    /// retained for the VM's lifetime.
    pub retained_functions: usize,
    /// Class definitions likewise retained.
    pub retained_classes: usize,
    /// Console lines buffered and not yet taken.
    pub console_lines_buffered: usize,
    /// Console bytes charged over the VM's lifetime (never credited on take).
    pub console_bytes_lifetime: usize,
    /// ArrayBuffers pinned for the accelerator.
    pub pinned_buffers: usize,
    /// Functions in the compiled program itself (the preamble-plus-guest
    /// script), and its bytecode and retained source bytes: the allocation
    /// the `safe-sandbox` profile keeps for the WASM instance's lifetime
    /// rather than freeing on `dispose()`. Exact for what they count; the
    /// compiler's side tables are not included.
    pub program_functions: usize,
    pub program_bytecode_bytes: usize,
    pub program_source_bytes: usize,
}

/// How deep the walk will follow an object graph before giving up. Deep enough
/// for any UI state a host would sensibly hold, shallow enough that a pathological
/// graph cannot exhaust the native stack (this walk is natively recursive).
const MAX_DEPTH: usize = 64;

/// Digest of a global that is absent or never initialised.
const FP_ABSENT: u64 = 0x9e37_79b9_7f4a_7c15;
/// FNV-1a's offset basis, XORed with a per-engine key before the walk.
///
/// The mixer is a chain of bijections and therefore invertible: with a known
/// starting value an attacker can solve for input that lands the digest on any
/// chosen target, so equal digests would stop implying equal values for anyone
/// willing to compute it. Keying the start removes the ability to solve rather
/// than making it merely unlikely — see `ScriptState::set_fingerprint_seed`.
/// graph into a change in the digest; it is not a cryptographic commitment.
const FP_SEED: u64 = 0xcbf2_9ce4_8422_2325;
/// Work a fingerprint walk may do before answering "unknown" instead.
///
/// A node ceiling alone did not bound the walk: holes were free, strings
/// were hashed whole however long, an array's element vector was cloned
/// before its size was checked, and every slot in a batch started a fresh
/// allowance (the 11 September 2026 audit's ZIPP-04). This charges every
/// visited element — holes included — every property, and every key and
/// string byte, and a batch threads one budget through all of its slots so
/// the digest walk can never do more work than the read it stands in for
/// would be allowed to. The counters are exact and deterministic, so a test
/// can pin them.
#[derive(Debug, Clone)]
pub struct FingerprintBudget {
    max_nodes: usize,
    used_nodes: usize,
    max_string_bytes: usize,
    used_string_bytes: usize,
}

impl FingerprintBudget {
    pub fn new(max_nodes: usize, max_string_bytes: usize) -> Self {
        Self {
            max_nodes,
            used_nodes: 0,
            max_string_bytes,
            used_string_bytes: 0,
        }
    }

    /// Nodes (values, elements, holes, properties) visited so far.
    pub fn nodes_used(&self) -> usize {
        self.used_nodes
    }

    /// Key and string bytes hashed so far.
    pub fn string_bytes_used(&self) -> usize {
        self.used_string_bytes
    }

    /// Charge `n` nodes; `false` when that would cross the ceiling. Used
    /// both per visit and as a container's up-front size check, so an array
    /// larger than the remaining allowance is refused before it is walked.
    fn charge_nodes(&mut self, n: usize) -> bool {
        match self.used_nodes.checked_add(n) {
            Some(total) if total <= self.max_nodes => {
                self.used_nodes = total;
                true
            }
            _ => false,
        }
    }

    /// Whether `n` more nodes would fit, without charging them.
    fn nodes_fit(&self, n: usize) -> bool {
        n <= self.max_nodes - self.used_nodes
    }

    fn charge_string_bytes(&mut self, n: usize) -> bool {
        match self.used_string_bytes.checked_add(n) {
            Some(total) if total <= self.max_string_bytes => {
                self.used_string_bytes = total;
                true
            }
            _ => false,
        }
    }
}

impl Default for FingerprintBudget {
    /// The read's own limits, so digest and read agree by construction: a
    /// value the read could not marshal is one the digest reports unknown.
    fn default() -> Self {
        Self::new(
            DEFAULT_HOST_VALUE_MAX_NODES,
            DEFAULT_HOST_VALUE_MAX_STRING_BYTES,
        )
    }
}

#[inline]
fn fp_mix(h: &mut u64, x: u64) {
    *h ^= x;
    *h = h.wrapping_mul(0x100_0000_01b3);
    *h ^= *h >> 29;
}

/// Mix `bytes` eight at a time, little-endian, the last word zero-padded.
#[inline]
fn fp_mix_bytes(h: &mut u64, bytes: &[u8]) {
    for chunk in bytes.chunks(8) {
        let mut word = 0u64;
        for (n, b) in chunk.iter().enumerate() {
            word |= (*b as u64) << (n * 8);
        }
        fp_mix(h, word);
    }
}

/// Default structural-conversion limits used at every host boundary.
///
/// A depth limit alone is not sufficient: a guest can build a tiny shared DAG
/// (`x = [x, x]` repeatedly) whose tree-shaped host representation expands
/// exponentially. These limits bound the representation itself, including
/// object keys and string payloads, before it is handed to an embedder.
// Matched to the fingerprint walk budget. At 100,000 the digest would walk
// and answer for a value twenty times larger than the read could marshal, so
// a host could be told "unchanged" about something it was then unable to
// fetch. Equal budgets make the two agree by construction.
pub const DEFAULT_HOST_VALUE_MAX_NODES: usize = 2_000_000;
pub const DEFAULT_HOST_VALUE_MAX_STRING_BYTES: usize = 16 * 1024 * 1024;

/// Why a slot call did not produce a host value: the guest threw, or the
/// result exists but exceeds the conversion budget the caller supplied. A
/// caller that stages work across a boundary needs the distinction — a
/// conversion failure means "try less", a throw means "stop".
#[derive(Debug, Clone, PartialEq)]
pub enum HostCallError {
    /// The callee (or a microtask it scheduled) threw; the message.
    Thrown(String),
    /// The call succeeded but its result could not be represented within the
    /// budget; the limit that was crossed.
    Conversion(String),
}

impl HostCallError {
    pub fn into_message(self) -> String {
        match self {
            HostCallError::Thrown(m) | HostCallError::Conversion(m) => m,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostValueBudget {
    max_nodes: usize,
    used_nodes: usize,
    max_string_bytes: usize,
    used_string_bytes: usize,
}

impl HostValueBudget {
    pub fn new(max_nodes: usize, max_string_bytes: usize) -> Self {
        Self {
            max_nodes,
            used_nodes: 0,
            max_string_bytes,
            used_string_bytes: 0,
        }
    }

    pub fn charge_node(&mut self) -> Result<(), String> {
        if self.used_nodes >= self.max_nodes {
            return Err(format!(
                "RangeError: host value exceeds the conversion node limit ({})",
                self.max_nodes
            ));
        }
        self.used_nodes += 1;
        Ok(())
    }

    pub fn charge_string(&mut self, value: &str) -> Result<(), String> {
        self.charge_string_bytes(value.len())
    }

    /// [`Self::charge_string`] for text already counted in bytes (WTF-8 or
    /// UTF-16 units, each at least one byte).
    pub fn charge_string_bytes(&mut self, bytes: usize) -> Result<(), String> {
        let Some(total) = self.used_string_bytes.checked_add(bytes) else {
            return Err(self.string_limit_error());
        };
        if total > self.max_string_bytes {
            return Err(self.string_limit_error());
        }
        self.used_string_bytes = total;
        Ok(())
    }

    /// Check a lower bound before allocating a UTF-8 copy. UTF-8 always uses
    /// at least one byte per UTF-16 code unit, so rejection here has no false
    /// negatives; the exact byte count is charged after conversion.
    pub fn ensure_string_units(&self, units: usize) -> Result<(), String> {
        if units > self.max_string_bytes.saturating_sub(self.used_string_bytes) {
            return Err(self.string_limit_error());
        }
        Ok(())
    }

    /// Check a container's immediate children before cloning/reserving them.
    /// They are charged individually as the walk visits them.
    pub fn ensure_nodes(&self, additional: usize) -> Result<(), String> {
        if additional > self.max_nodes.saturating_sub(self.used_nodes) {
            return Err(format!(
                "RangeError: host value exceeds the conversion node limit ({})",
                self.max_nodes
            ));
        }
        Ok(())
    }

    fn string_limit_error(&self) -> String {
        format!(
            "RangeError: host value exceeds the conversion string limit ({} bytes)",
            self.max_string_bytes
        )
    }
}

#[cfg(all(test, feature = "jit", target_arch = "x86_64"))]
mod layout_tests {
    use super::*;
    use crate::value::Value;

    #[test]
    fn register_file_emitted_offsets_address_explicit_fields() {
        let mut regs = crate::vm::RegisterFile::from_vec(vec![Value::UNDEFINED; 3]);
        regs.truncate(1);
        let base = &mut regs as *mut crate::vm::RegisterFile as *mut u8;
        let ptr_off = core::mem::offset_of!(crate::vm::RegisterFile, ptr_mirror);
        let len_off = core::mem::offset_of!(crate::vm::RegisterFile, logical_len);
        // SAFETY: repr(C) field offsets above identify aligned fields of `regs`.
        assert_eq!(
            unsafe { *(base.add(ptr_off) as *const usize) },
            regs.as_ptr().expose_provenance()
        );
        assert_eq!(unsafe { *(base.add(len_off) as *const usize) }, regs.len());

        // This is the exact B243 operation: revive only within the initialized
        // high-water backing and observe it through the safe logical view.
        unsafe { core::ptr::write(base.add(len_off) as *mut usize, 3) };
        assert_eq!(regs.len(), 3);
        assert_eq!(regs.initialized_len(), 3);
    }
}

impl Default for HostValueBudget {
    fn default() -> Self {
        Self::new(
            DEFAULT_HOST_VALUE_MAX_NODES,
            DEFAULT_HOST_VALUE_MAX_STRING_BYTES,
        )
    }
}

impl<'p> Vm<'p> {
    /// The program's top-level bindings, in slot order.
    ///
    /// Only DECLARED slots are reported. `global_names` also carries every free
    /// identifier the program mentioned (`Math`, `JSON`, `undefined`, …) so the
    /// VM can pre-populate builtins, and a host that treated those as script
    /// state would try to sync the entire standard library.
    pub(crate) fn host_symbols(&self) -> Vec<Symbol> {
        let p: &Program = self.program;
        let mut out: Vec<Symbol> = Vec::new();
        let push = |slot: u32, scope: SymbolScope, out: &mut Vec<Symbol>| {
            if let Some(name) = p.global_names.get(slot as usize) {
                if !name.is_empty() {
                    out.push(Symbol {
                        name: name.clone(),
                        index: slot,
                        scope,
                    });
                }
            }
        };
        // Functions and classes first so a name declared both ways reports as
        // callable, then the ordinary variable bindings.
        for &s in &p.decl_globals {
            push(s, SymbolScope::Function, &mut out);
        }
        for &s in p.hoisted_globals.iter().chain(p.lexical_globals.iter()) {
            if p.decl_globals.contains(&s) {
                continue;
            }
            push(s, SymbolScope::Variable, &mut out);
        }
        out.sort_by_key(|s| s.index);
        out.dedup_by_key(|s| s.index);
        out
    }

    /// Read global slot `index` as owned data. Out-of-range and
    /// never-initialized slots read as `Undefined`, matching what the script
    /// itself would observe.
    pub(crate) fn host_get_slot(&mut self, index: u32) -> Result<HostValue, String> {
        let v = match self.globals.get(index as usize) {
            Some(v) => *v,
            None => return Ok(HostValue::Undefined),
        };
        if v.is_uninitialized() {
            return Ok(HostValue::Undefined);
        }
        let _g = self.gc_lock_guard();
        let mut seen: Vec<u32> = Vec::new();
        let mut budget = HostValueBudget::default();
        self.host_out(v, 0, &mut seen, &mut budget)
    }

    /// Mirror the recorder's heap ceiling into the heap, so the slot table
    /// grows gently instead of doubling past it (see
    /// `Heap::reserve_slot_growth`).
    #[cfg(feature = "instrument")]
    pub(crate) fn set_resident_ceiling(&mut self, bytes: usize) {
        self.heap.set_resident_ceiling(bytes);
    }

    /// Restore the instruction budget without touching any other limit.
    ///
    /// Only the step counter moves: `heap_limit`, `output_limit` and the
    /// sticky `exhaustion` are left exactly as they are, so a renewal can
    /// never resurrect an engine that has already spent a different budget.
    pub(crate) fn renew_step_budget(&mut self, max_steps: u64) -> bool {
        // `instr_rec` only exists under `instrument`, and this reached for it
        // unconditionally. The workspace build hides that — some other member
        // turns the feature on and Cargo unifies it — but `cargo build -p
        // zipp-vm`, and the `--no-default-features` pure interpreter this
        // crate's Cargo.toml advertises, both failed to compile.
        #[cfg(feature = "instrument")]
        {
            match self.instr_rec.as_mut() {
                Some(rec) if rec.exhaustion.is_none() => {
                    rec.set_step_limit(max_steps);
                    true
                }
                _ => false,
            }
        }
        // No instrument feature means no step budget was ever imposed, so there
        // is nothing to restore and nothing that could have been spent. True,
        // not false: the caller is asking whether it may keep going, and an
        // engine with no budget always may. Answering false would make a host
        // that checks the result stop dead in the one configuration that has no
        // reason to stop.
        #[cfg(not(feature = "instrument"))]
        {
            let _ = max_steps;
            true
        }
    }

    /// A digest of what global `index` would marshal to, without marshalling it.
    ///
    /// A host that mirrors globals pays for what they HOLD, not for how many
    /// changed: reading a 51 KB scene description out of the heap and rebuilding
    /// it as host values costs the same whether or not a byte of it moved. This
    /// walks the same graph `host_out` would, in the same order and with the
    /// same depth and cycle rules, and hashes it instead of allocating — so an
    /// unchanged digest means an unchanged marshalled value, and the host can
    /// skip the copy entirely.
    ///
    /// Deliberately NOT a write-generation counter. `global_gens` already
    /// exists and would be cheaper, but it moves only when the SLOT is assigned:
    /// `arr.push(x)` mutates the array a global points at without touching
    /// the slot, so a generation would report "unchanged" for a value that did
    /// change. A content digest cannot miss that, because it reads the content.
    ///
    /// `None` means "assume it changed" — returned when the graph needs more
    /// work than `budget` allows, so a pathological value degrades to the old
    /// always-copy behaviour rather than to a wrong answer. Equal digests are
    /// probabilistic evidence of equality, not structural proof; an unknown
    /// digest is never evidence of anything.
    pub(crate) fn host_fingerprint_slot(
        &mut self,
        index: u32,
        seed: u64,
        budget: &mut FingerprintBudget,
    ) -> Option<u64> {
        let v = match self.globals.get(index as usize) {
            Some(v) => *v,
            None => return Some(FP_ABSENT),
        };
        if v.is_uninitialized() {
            return Some(FP_ABSENT);
        }
        let _g = self.gc_lock_guard();
        let mut seen: Vec<u32> = Vec::new();
        let mut h: u64 = FP_SEED ^ seed;
        if self.host_fp(v, 0, &mut seen, &mut h, budget) {
            Some(h)
        } else {
            None
        }
    }

    /// Hash `v` into `h`. False means the budget ran out, which makes the
    /// whole fingerprint unusable rather than partial — a partial digest
    /// would be stable across a change in the part it never reached.
    ///
    /// Nothing is cloned out of the heap: containers are re-borrowed per
    /// element, so the walk's memory is its recursion and `seen`, and a
    /// container is refused before it is entered when it alone exceeds what
    /// the budget has left. No guest code runs (accessors are skipped, as
    /// `host_out` skips them), so the graph cannot change under the walk.
    fn host_fp(
        &mut self,
        v: Value,
        depth: usize,
        seen: &mut Vec<u32>,
        h: &mut u64,
        budget: &mut FingerprintBudget,
    ) -> bool {
        if !budget.charge_nodes(1) {
            return false;
        }
        if v.is_undefined() || v.is_uninitialized() {
            fp_mix(h, 1);
            return true;
        }
        if v.is_null() {
            fp_mix(h, 2);
            return true;
        }
        if v.is_bool() {
            fp_mix(h, if v.as_bool() { 3 } else { 4 });
            return true;
        }
        if v.is_int() {
            fp_mix(h, 5);
            fp_mix(h, v.as_int() as i64 as u64);
            return true;
        }
        if v.is_double() {
            fp_mix(h, 6);
            // The bit pattern, not the value: 0.0 and -0.0 are different
            // marshalled values and must be different digests.
            fp_mix(h, v.as_f64().to_bits());
            return true;
        }
        if !v.is_heap() {
            fp_mix(h, 7);
            return true;
        }
        let idx = v.heap_index();
        if depth >= MAX_DEPTH || seen.contains(&idx) {
            fp_mix(h, 8);
            return true;
        }

        enum Shape {
            Str { units: usize },
            Array { len: usize },
            Object { keys: usize, visible: usize },
            Opaque,
        }
        let shape = match self.heap.get(idx) {
            HeapObj::Str(s) => Shape::Str { units: s.units() },
            HeapObj::Cons { len, .. } => Shape::Str { units: *len },
            HeapObj::Array(items) => Shape::Array { len: items.len() },
            HeapObj::Object(m) => Shape::Object {
                keys: m.keys.len(),
                // The same exclusion host_out makes: an accessor is never
                // invoked, so it contributes nothing to the marshalled value
                // and must contribute nothing to the digest either.
                visible: (0..m.keys.len())
                    .filter(|&i| {
                        let a = m.attr_at(i);
                        a.enumerable && !a.accessor
                    })
                    .count(),
            },
            _ => Shape::Opaque,
        };

        match shape {
            Shape::Opaque => {
                fp_mix(h, 9);
                true
            }
            Shape::Str { units } => {
                // WTF-8 needs at least one byte per UTF-16 unit, so this
                // refuses without materializing; the exact byte count is
                // charged once the bytes exist. The EXACT bytes are hashed —
                // a lone surrogate is a different string from U+FFFD, and the
                // digest must say so.
                if !budget.charge_string_bytes(units) {
                    return false;
                }
                fp_mix(h, 10);
                let bytes = self.heap.str_wtf8_cow(idx).unwrap_or_default();
                if bytes.len() > units && !budget.charge_string_bytes(bytes.len() - units) {
                    return false;
                }
                fp_mix(h, bytes.len() as u64);
                fp_mix_bytes(h, &bytes);
                true
            }
            Shape::Array { len } => {
                // Every element, hole or not, is a node: refuse the whole
                // array before walking it when it cannot fit.
                if !budget.nodes_fit(len) {
                    return false;
                }
                fp_mix(h, 11);
                fp_mix(h, len as u64);
                seen.push(idx);
                let mut ok = true;
                for i in 0..len {
                    let it = match self.heap.get(idx) {
                        HeapObj::Array(items) => match items.get(i) {
                            Some(it) => *it,
                            None => break,
                        },
                        _ => break,
                    };
                    if it.is_hole() {
                        if !budget.charge_nodes(1) {
                            ok = false;
                            break;
                        }
                        fp_mix(h, 12);
                    } else if !self.host_fp(it, depth + 1, seen, h, budget) {
                        ok = false;
                        break;
                    }
                }
                seen.pop();
                ok
            }
            Shape::Object { keys, visible } => {
                if !budget.nodes_fit(visible) {
                    return false;
                }
                fp_mix(h, 13);
                fp_mix(h, visible as u64);
                seen.push(idx);
                let mut ok = true;
                for i in 0..keys {
                    // Hash the key inside the borrow — no clone — and carry
                    // only the value out for the recursive step.
                    let val = match self.heap.get(idx) {
                        HeapObj::Object(m) if i < m.keys.len() => {
                            let a = m.attr_at(i);
                            if !a.enumerable || a.accessor {
                                continue;
                            }
                            let key = m.keys[i].as_bytes();
                            if !budget.charge_string_bytes(key.len()) {
                                ok = false;
                                break;
                            }
                            fp_mix(h, key.len() as u64);
                            fp_mix_bytes(h, key);
                            m.val_at(i)
                        }
                        _ => break,
                    };
                    if !self.host_fp(val, depth + 1, seen, h, budget) {
                        ok = false;
                        break;
                    }
                }
                seen.pop();
                ok
            }
        }
    }

    /// Write global slot `index`. Returns `false` — leaving the slot untouched
    /// — when the slot currently holds something opaque, so a read/modify/write
    /// of the whole global set cannot overwrite the script's own functions with
    /// the `Opaque` placeholder they read back as.
    pub(crate) fn host_set_slot(&mut self, index: u32, hv: &HostValue) -> bool {
        let cur = match self.globals.get(index as usize) {
            Some(v) => *v,
            None => return false,
        };
        if matches!(hv, HostValue::Opaque) || self.host_is_opaque(cur) {
            return false;
        }
        let _g = self.gc_lock_guard();
        let v = self.host_in_over(cur, hv, 0);
        self.globals[index as usize] = v;
        self.bump_global_gen(index);
        true
    }

    /// Call the function in global slot `index`, then drain the microtask queue
    /// so promise callbacks the call scheduled have run before the host looks
    /// at the resulting state.
    ///
    /// Resolves the callee by SLOT, not by re-evaluating its name: the name
    /// path compiles a fresh program per call and interns it for the VM's
    /// lifetime, which a host calling a handler every frame cannot afford.
    pub(crate) fn host_call_slot(
        &mut self,
        index: u32,
        args: &[HostValue],
    ) -> Result<HostValue, String> {
        let mut budget = HostValueBudget::default();
        self.host_call_slot_bounded(index, args, &mut budget)
            .map_err(HostCallError::into_message)
    }

    /// [`Self::host_call_slot`] with the caller's own conversion budget for
    /// the RESULT, and a typed error so a throw and an over-budget result are
    /// distinguishable. The budget is charged only for what the result
    /// consumed; on a conversion failure the caller's copy is partially
    /// charged, so stage on a clone when a retry is intended.
    pub(crate) fn host_call_slot_bounded(
        &mut self,
        index: u32,
        args: &[HostValue],
        budget: &mut HostValueBudget,
    ) -> Result<HostValue, HostCallError> {
        let callee = match self.globals.get(index as usize) {
            Some(v) => *v,
            None => {
                return Err(HostCallError::Thrown(format!(
                    "zipp: no global in slot {index}"
                )))
            }
        };
        if !self.is_callable(callee) {
            return Err(HostCallError::Thrown(format!(
                "TypeError: global slot {index} is not a function"
            )));
        }
        let argv: Vec<Value> = {
            let _g = self.gc_lock_guard();
            args.iter().map(|a| self.host_in(a, 0)).collect()
        };
        let res = self.call_value(callee, Value::UNDEFINED, &argv);
        // Drain regardless of outcome: a throw can still have queued jobs, and
        // leaving them parked would surface them at an arbitrary later call.
        // The completion value is rooted across the drain (ZA-05).
        self.marshal_after_drain(res, budget)
    }

    /// Root `res`'s value, drain the microtask queue, then marshal the value
    /// under `budget`; the root is released on every exit.
    ///
    /// The callee's frame has already been popped, so between the call and
    /// the marshal the value is reachable from nothing the collector traces
    /// except `host_result_roots` — and the drain runs guest code and polls
    /// the collector after every job (the 11 September 2026 close audit's
    /// ZA-05). Rooting exactly this value keeps collection ON during the
    /// jobs: a long job still reclaims its own garbage.
    fn marshal_after_drain(
        &mut self,
        res: Result<Value, crate::vm::Thrown>,
        budget: &mut HostValueBudget,
    ) -> Result<HostValue, HostCallError> {
        let res = match res {
            Ok(v) => {
                if v.is_heap() {
                    self.host_result_roots.push(v);
                }
                Ok(v)
            }
            // The throw has reached the host: it is delivered as the message
            // below, and nothing outside the VM can catch the value. Clear it
            // BEFORE the drain, so neither a job nor the next entry sees it.
            Err(t) => Err(self.take_host_throw(t)),
        };
        self.drain_microtasks();
        match res {
            Ok(v) => {
                let out = {
                    let _g = self.gc_lock_guard();
                    let mut seen: Vec<u32> = Vec::new();
                    self.host_out(v, 0, &mut seen, budget)
                        .map_err(HostCallError::Conversion)
                };
                if v.is_heap() {
                    self.host_result_roots.pop();
                }
                out
            }
            Err(message) => Err(HostCallError::Thrown(message)),
        }
    }

    /// A throw that has propagated to the host boundary, as its message.
    ///
    /// `run_loop` leaves `pending_throw` set when a throw escapes it, so an
    /// ENCLOSING interpreter loop (a builtin's callback, a nested eval) can
    /// still catch the value. At the host boundary there is no enclosing
    /// loop: the host receives the message and the value is done. Leaving it
    /// set was a defect the ZA-05 regression surfaced — the next entry that
    /// reached a compiled-code exit or deopt check read the stale value as a
    /// throw in flight and failed with the PREVIOUS call's error (native JIT
    /// profiles; the WASM build runs no compiled tier). Every host entry
    /// that turns a `Thrown` into a `String` goes through here.
    pub(crate) fn take_host_throw(&mut self, t: crate::vm::Thrown) -> String {
        self.pending_throw = None;
        t.0
    }

    /// How many host completion values are currently rooted — zero between
    /// host entries. Test-only: pins that every exit releases its root.
    pub(crate) fn host_result_roots_len(&self) -> usize {
        self.host_result_roots.len()
    }

    /// Run any pending microtasks. A host that resumed the script by writing
    /// globals (rather than calling a function) uses this to let promise
    /// continuations observe the write.
    pub(crate) fn host_pump(&mut self) {
        self.drain_microtasks();
    }

    /// Evaluate `src` as indirect eval in the global scope and marshal the
    /// completion value as a structured [`HostValue`] under `budget` — the
    /// rich-value counterpart of the JSON projection the WASM
    /// `evalInContext` performs, with the same cycle, depth, opaque and
    /// budget rules as a slot read. Microtasks are drained afterwards, as
    /// [`Self::host_call_slot`] drains them. Each call compiles and retains a
    /// program; it is for one-off queries, never a per-frame path.
    pub(crate) fn host_eval_rich(
        &mut self,
        src: &str,
        budget: &mut HostValueBudget,
    ) -> Result<HostValue, HostCallError> {
        let res = self.do_eval(
            src,
            false,            // force_strict: inherit the source's own directive
            false,            // force_new_target_ok
            None,             // this_override: the realm global
            None,             // inherit_super
            false,            // ban_arguments
            false,            // direct
            Value::UNDEFINED, // caller_new_target
            None,             // caller_home_obj
            true,             // var_env_global: declarations persist
            None,             // param_collisions
            Vec::new(),       // lexical_collisions
            None,             // caller_scope
            None,             // eval_scope_idx
            None,             // exact_src
        );
        // Rooted across the drain, as a slot call's result is (ZA-05).
        self.marshal_after_drain(res, budget)
    }

    /// A snapshot of what this VM currently retains and has spent — see
    /// [`ResourceUsage`].
    pub(crate) fn host_resource_usage(&self) -> ResourceUsage {
        #[cfg(feature = "instrument")]
        let (steps_used, dynamic_code_calls, dynamic_code_source_bytes) = match &self.instr_rec {
            Some(rec) => {
                let (calls, bytes) = rec.dynamic_code_usage();
                (rec.steps_used(), calls, bytes)
            }
            None => (0, 0, 0),
        };
        #[cfg(not(feature = "instrument"))]
        let (steps_used, dynamic_code_calls, dynamic_code_source_bytes) = (0, 0, 0);
        #[cfg(feature = "instrument")]
        let console_bytes_lifetime = self.instr_rec.as_ref().map_or(0, |rec| rec.output_used);
        #[cfg(not(feature = "instrument"))]
        let console_bytes_lifetime = 0;
        ResourceUsage {
            heap_bytes: self.heap_bytes(),
            steps_used,
            dynamic_code_calls,
            dynamic_code_source_bytes,
            retained_functions: self.eval_funcs.len(),
            retained_classes: self.eval_classes.len(),
            console_lines_buffered: self.output.len() + self.errput.len(),
            console_bytes_lifetime,
            pinned_buffers: self.pinned_buffers.len(),
            program_functions: self.program.functions.len(),
            program_bytecode_bytes: self
                .program
                .functions
                .iter()
                .map(|f| f.code.len() * std::mem::size_of::<crate::bytecode::Instr>())
                .sum(),
            program_source_bytes: self.program.functions.iter().map(|f| f.source.len()).sum(),
        }
    }

    /// Does this value refuse to cross as data?
    fn host_is_opaque(&self, v: Value) -> bool {
        if !v.is_heap() {
            return false;
        }
        !matches!(
            self.heap.get(v.heap_index()),
            HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Array(_) | HeapObj::Object(_)
        )
    }

    /// `Value` → [`HostValue`]. `seen` carries the heap indices on the path from
    /// the root, so a back-edge becomes `Null` instead of recursing forever.
    fn host_out(
        &mut self,
        v: Value,
        depth: usize,
        seen: &mut Vec<u32>,
        budget: &mut HostValueBudget,
    ) -> Result<HostValue, String> {
        budget.charge_node()?;
        if v.is_undefined() || v.is_uninitialized() {
            return Ok(HostValue::Undefined);
        }
        if v.is_null() {
            return Ok(HostValue::Null);
        }
        if v.is_bool() {
            return Ok(HostValue::Bool(v.as_bool()));
        }
        if v.is_int() {
            return Ok(HostValue::Number(v.as_int() as f64));
        }
        if v.is_double() {
            return Ok(HostValue::Number(v.as_f64()));
        }
        if !v.is_heap() {
            return Ok(HostValue::Opaque);
        }
        let idx = v.heap_index();
        if depth >= MAX_DEPTH || seen.contains(&idx) {
            return Ok(HostValue::Null);
        }

        // Classify and copy out of the heap in a short borrow, so the recursive
        // step below is free to allocate and mutate.
        enum Shape {
            Str { units: usize },
            Array(Vec<Value>),
            Object(Vec<(String, Value)>),
            Opaque,
        }
        let shape = match self.heap.get(idx) {
            HeapObj::Str(s) => Shape::Str { units: s.units() },
            HeapObj::Cons { len, .. } => Shape::Str { units: *len },
            HeapObj::Array(items) => {
                budget.ensure_nodes(items.len())?;
                Shape::Array(items.clone())
            }
            HeapObj::Object(m) => {
                let count = (0..m.keys.len())
                    .filter(|&i| m.attr_at(i).enumerable && !m.attr_at(i).accessor)
                    .count();
                budget.ensure_nodes(count)?;
                let mut pairs = Vec::with_capacity(count);
                for i in 0..m.keys.len() {
                    let a = &m.attr_at(i);
                    // Accessors are not invoked: running user code in the middle
                    // of a marshal would let a getter mutate the graph being walked.
                    if !a.enumerable || a.accessor {
                        continue;
                    }
                    budget.charge_string(&m.keys[i])?;
                    pairs.push((m.keys[i].clone(), m.val_at(i)));
                }
                Shape::Object(pairs)
            }
            _ => Shape::Opaque,
        };

        match shape {
            Shape::Opaque => Ok(HostValue::Opaque),
            Shape::Str { units } => {
                budget.ensure_string_units(units)?;
                Ok(self.host_out_string(idx, budget)?)
            }
            Shape::Array(items) => {
                seen.push(idx);
                let out: Result<Vec<_>, _> = items
                    .into_iter()
                    .map(|it| {
                        if it.is_hole() {
                            budget.charge_node()?;
                            Ok(HostValue::Undefined)
                        } else {
                            self.host_out(it, depth + 1, seen, budget)
                        }
                    })
                    .collect();
                seen.pop();
                Ok(HostValue::Array(out?))
            }
            Shape::Object(pairs) => {
                seen.push(idx);
                let out: Result<Vec<_>, String> = pairs
                    .into_iter()
                    .map(|(k, val)| {
                        self.host_out(val, depth + 1, seen, budget)
                            .map(|value| (k, value))
                    })
                    .collect();
                seen.pop();
                Ok(HostValue::Object(out?))
            }
        }
    }

    /// The string at heap index `idx` as a host value: `String` when it is
    /// well-formed UTF-16 (its WTF-8 bytes are then valid UTF-8, taken
    /// without a copy through the lossy renderer), `Utf16` with the exact
    /// code units when it holds a lone surrogate. Charged by its byte count.
    fn host_out_string(
        &mut self,
        idx: u32,
        budget: &mut HostValueBudget,
    ) -> Result<HostValue, String> {
        let bytes = self
            .heap
            .str_wtf8_cow(idx)
            .map(Cow::into_owned)
            .unwrap_or_default();
        budget.charge_string_bytes(bytes.len())?;
        match String::from_utf8(bytes) {
            Ok(s) => Ok(HostValue::String(s)),
            Err(err) => Ok(HostValue::Utf16(
                crate::heap::wtf8_units_iter(err.as_bytes()).collect(),
            )),
        }
    }

    /// [`HostValue`] → `Value`, written OVER an existing value.
    ///
    /// The slot-level rule — a write may not replace something opaque with the
    /// placeholder it reads back as — has to hold one level deeper too, because
    /// a host that reads an object, spreads it, and writes it back sends every
    /// method it could not see back as `Null`. Without this, a single
    /// read-modify-write of an object carrying host functions silently strips
    /// them, and the failure only shows up the next time the script calls one.
    ///
    /// So: an incoming `Null`/`Undefined` for a key whose CURRENT value is
    /// opaque keeps the current value, and keys the host omitted entirely are
    /// preserved when they are opaque. An explicit non-null write always wins —
    /// this protects what the host could not express, never what it chose.
    fn host_in_over(&mut self, old: Value, hv: &HostValue, depth: usize) -> Value {
        // An ARRAY has to be walked element-wise for the same reason an object is
        // walked property-wise. Without this arm the whole preservation rule
        // stopped at the first array: everything below it was rebuilt from the
        // host's projection, so a function, or an instance, one element deep was
        // destroyed by a read-modify-write that changed nothing.
        //
        //     [ function () { ... } ]        -> host sees [null] -> element gone
        //     { list: [ { fn: ... } ] }      -> the array breaks the chain
        //
        // The object path already did this correctly, which is what made it look
        // like arrays were fine too.
        if let HostValue::Array(items) = hv {
            if depth < MAX_DEPTH && old.is_heap() {
                let old_items = match self.heap.get(old.heap_index()) {
                    HeapObj::Array(items) => Some(items.clone()),
                    _ => None,
                };
                if let Some(old_items) = old_items {
                    let mut vals: Vec<Value> = Vec::with_capacity(items.len());
                    for (i, it) in items.iter().enumerate() {
                        let prev = old_items.get(i).copied();
                        let v = match (it, prev) {
                            // The host is echoing back a value it could only ever
                            // see as Opaque. That is not an edit.
                            (
                                HostValue::Null | HostValue::Undefined | HostValue::Opaque,
                                Some(p),
                            ) if self.host_is_opaque(p) => p,
                            (_, Some(p)) => self.host_in_over(p, it, depth + 1),
                            (_, None) => self.host_in(it, depth + 1),
                        };
                        vals.push(v);
                    }
                    return Value::heap(self.heap.alloc(HeapObj::Array(vals)));
                }
            }
            return self.host_in(hv, depth);
        }
        let HostValue::Object(pairs) = hv else {
            return self.host_in(hv, depth);
        };
        if depth >= MAX_DEPTH || !old.is_heap() {
            return self.host_in(hv, depth);
        }
        // Snapshot the old object's own properties WITH their attributes — and
        // the class it is an instance of — then drop the borrow. Accessors are
        // kept in the snapshot rather than filtered out: the host never saw
        // them, so their absence from its echo says nothing.
        let (old_props, old_class): (Vec<(String, Value, PropAttr)>, Option<u32>) =
            match self.heap.get(old.heap_index()) {
                HeapObj::Object(m) => (
                    (0..m.keys.len())
                        .map(|i| (m.keys[i].clone(), m.val_at(i), m.attr_at(i)))
                        .collect(),
                    m.class,
                ),
                _ => return self.host_in(hv, depth),
            };
        // Matching each incoming key against the old properties by linear
        // scan, and then each old property against the incoming keys the same
        // way, made a merge of two similar N-key objects cost N² comparisons
        // (the 11 September 2026 audit's ZIPP-05). Index the old keys once —
        // a hash map past a small size, a scan below it where the map costs
        // more than it saves — and remember which old positions the host
        // sent, so the preservation pass below is a single walk.
        let old_index: Option<FxHashMap<&str, usize>> = if old_props.len() > MERGE_INDEX_THRESHOLD {
            let mut index =
                FxHashMap::with_capacity_and_hasher(old_props.len(), Default::default());
            for (i, (k, _, _)) in old_props.iter().enumerate() {
                // First occurrence wins, as the linear scan found the first.
                index.entry(k.as_str()).or_insert(i);
            }
            Some(index)
        } else {
            None
        };
        let find_old = |k: &str| -> Option<usize> {
            merge_probe();
            match &old_index {
                Some(index) => index.get(k).copied(),
                None => old_props.iter().position(|(ok, _, _)| {
                    merge_probe();
                    ok == k
                }),
            }
        };
        let mut sent = vec![false; old_props.len()];

        let mut m = ObjMap::with_capacity(pairs.len().max(old_props.len()));
        for (k, val) in pairs {
            let prev = find_old(k).map(|i| {
                sent[i] = true;
                (old_props[i].1, old_props[i].2)
            });
            // An accessor of the same name is handled by the preserve pass
            // below. What the host echoed back for this key is the GETTER'S
            // RESULT, and writing that in as a data property would replace the
            // accessor with a snapshot of one call to it.
            if matches!(prev, Some((_, a)) if a.accessor) {
                continue;
            }
            let v = match (val, prev) {
                // The host is not overwriting here — it is echoing back a value
                // it was never able to see. Keep what is really there.
                (HostValue::Null | HostValue::Undefined | HostValue::Opaque, Some((p, _)))
                    if self.host_is_opaque(p) =>
                {
                    p
                }
                (_, Some((p, _))) => self.host_in_over(p, val, depth + 1),
                (_, None) => self.host_in(val, depth + 1),
            };
            // Carry the property's own attributes. The host is supplying a
            // VALUE, not a descriptor, so a read-only property that the host
            // writes keeps its value change and stays read-only rather than
            // silently becoming writable.
            match prev {
                Some((_, a)) => {
                    m.define(k, v, a);
                }
                None => {
                    m.set(k, v);
                }
            }
        }
        // Keys the host did not send back. An enumerable data property is a
        // deliberate deletion — the host saw it and dropped it. Everything else
        // it never saw at all, and `host_out` is what decides that: it emits
        // only enumerable, non-accessor properties.
        //
        // That distinction was missing, and the absence of an invisible property
        // was read as intent to remove it. Mirroring the globals and writing
        // them straight back — what a host that tracks state does every tick —
        // deleted every non-enumerable property and every accessor:
        //
        //     new Error("boom").message  ->  undefined
        //     a get-only property        ->  undefined
        for (i, (k, p, a)) in old_props.iter().enumerate() {
            let host_sent_it = sent[i];
            let host_could_see_it = a.enumerable && !a.accessor;
            if host_sent_it && host_could_see_it {
                continue;
            }
            if !host_could_see_it || self.host_is_opaque(*p) {
                m.define(k, *p, *a);
            }
        }
        // An instance resolves its methods through its class, and this built a
        // fresh PLAIN object. Everything above is careful to keep what the host
        // could not represent — a function it echoed back as null, a key it
        // dropped entirely — and then the one thing that made the value an
        // instance was dropped anyway.
        //
        // The host does not have to do anything unusual to trigger it. Reading
        // the globals and writing them straight back, which is what a host that
        // mirrors state does every tick, was enough:
        //
        //     counter.next()  ->  1
        //     (host reads globals, writes the same values back)
        //     counter.next()  ->  TypeError: undefined is not a function
        //
        // Only `class` is carried. Seal/freeze state is deliberately not: this
        // merge has already applied the host's writes, so marking the result
        // frozen would describe an object that had just been written to.
        m.class = old_class;
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))))
    }

    /// [`HostValue`] → `Value`. Builds bottom-up; the caller holds a GC lock for
    /// the whole tree because the partially-built children live in Rust locals,
    /// which are not GC roots.
    fn host_in(&mut self, hv: &HostValue, depth: usize) -> Value {
        if depth >= MAX_DEPTH {
            return Value::NULL;
        }
        match hv {
            HostValue::Undefined | HostValue::Opaque => Value::UNDEFINED,
            HostValue::Null => Value::NULL,
            HostValue::Bool(b) => Value::bool(*b),
            HostValue::Number(n) => Value::num(*n),
            HostValue::String(s) => {
                let i = self.heap.alloc_str(s.clone());
                Value::heap(i)
            }
            HostValue::Utf16(units) => {
                let i = self
                    .heap
                    .alloc_js(crate::heap::JsStr::from_wtf8(utf16_to_wtf8(units)));
                Value::heap(i)
            }
            HostValue::Array(items) => {
                let vals: Vec<Value> = items.iter().map(|it| self.host_in(it, depth + 1)).collect();
                Value::heap(self.heap.alloc(HeapObj::Array(vals)))
            }
            HostValue::Object(pairs) => {
                let mut m = ObjMap::with_capacity(pairs.len());
                for (k, val) in pairs {
                    let v = self.host_in(val, depth + 1);
                    m.set(k, v);
                }
                Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))))
            }
        }
    }
}

/// What a context-taking host closure ([`HostCallCtx`]) may do with the VM
/// while a `__zippHostCall` is being served. Both operations name a global
/// of the running program; nothing else of the VM is reachable.
pub trait HostCtx {
    /// A guest typed array, by the name of the global holding it, as a region
    /// of this process's memory: the address of its first element, its element
    /// count, and its element kind (the index into the engine's kind table:
    /// 0 Int8, 1 Uint8, 2 Uint8Clamped, 3 Int16, 4 Uint16, 5 Int32, 6 Uint32,
    /// 7 Float32, 8 Float64). Pins the buffer: it stays alive and is never
    /// resized, transferred or detached, so a view the host builds over the
    /// region stays over those bytes. BigInt and length-tracking views are
    /// refused, as is a detached buffer.
    fn typed_array_region(&mut self, name: &str) -> Result<(usize, usize, u8), String>;
    /// [`Self::typed_array_region`] for several globals at once, as a single
    /// transaction: either every name resolves and every buffer is pinned, or
    /// nothing is pinned and the first failure is returned. A default
    /// implementation resolves one at a time and is NOT transactional; the
    /// engine's implementation is.
    fn typed_array_regions(&mut self, names: &[&str]) -> Result<Vec<(usize, usize, u8)>, String> {
        names
            .iter()
            .map(|name| self.typed_array_region(name))
            .collect()
    }
    /// Call the guest function a global names, with numbers, for a number.
    /// Runs the guest re-entrantly inside the host call; the guest cannot make
    /// a nested host call while it does.
    fn call_global_numbers(&mut self, name: &str, args: &[f64]) -> Result<f64, String>;
}

/// The context-taking twin of [`crate::embed::HostCall`]: the same string
/// contract, plus the VM as [`HostCtx`] for the duration of the call.
pub type HostCallCtx = Box<dyn FnMut(&mut dyn HostCtx, &str, &[String]) -> Result<String, String>>;

impl<'p> Vm<'p> {
    /// The slot of a top-level binding, by name.
    fn global_slot_by_name(&self, name: &str) -> Option<u32> {
        let p: &Program = self.program;
        p.global_names
            .iter()
            .position(|n| n == name)
            .map(|i| i as u32)
    }

    fn named_global(&self, name: &str) -> Result<Value, String> {
        let slot = self
            .global_slot_by_name(name)
            .ok_or_else(|| format!("ReferenceError: no global named {name:?}"))?;
        Ok(self
            .globals
            .get(slot as usize)
            .copied()
            .unwrap_or(Value::UNDEFINED))
    }

    /// Resolve the global binding `name` names, exactly as a bare identifier
    /// read at the top level would — and without compiling anything.
    ///
    /// `ScriptState::call_global` used to reach its callee by evaluating the
    /// name as a fresh program, so every name-based re-entry and every
    /// `has_global_function` probe went through the dynamic compiler: it
    /// spent the dynamic-code allowance, interned a program for the VM's
    /// lifetime, and contradicted the method's own "compiles nothing"
    /// contract (the 11 September 2026 audit's ZIPP-03). This is the lookup
    /// the `LoadGlobal` family performs, minus the activation-specific
    /// EvalScope (a host has no activation), in the same order:
    ///
    /// 1. a slot the main program or a later eval assigned the name, read
    ///    directly when live and unshadowed, otherwise through the same slow
    ///    path the interpreter takes (TDZ, a real own property of the global
    ///    object shadowing the slot, a deleted builtin);
    /// 2. an own property of the global object (eval-created vars,
    ///    `globalThis.x = v`), then its prototype chain (`HasBinding` is
    ///    `HasProperty` on the binding object, chain included);
    /// 3. the builtin table, which also serves the virtual value-properties
    ///    and respects `delete globalThis.X`.
    ///
    /// Nothing is cached: a binding may be reassigned between calls, so the
    /// current value is read every time. A prototype `has` trap can run guest
    /// code here, exactly as it can for a bare identifier read.
    pub(crate) fn host_resolve_global_by_name(&mut self, name: &str) -> Result<Value, String> {
        let slot = self
            .global_slot_by_name(name)
            .or_else(|| self.eval_global_map.get(name).copied());
        if let Some(idx) = slot {
            if let Some(v) = self.globals.get(idx as usize).copied() {
                if !v.is_uninitialized()
                    && !(self.global_route_epoch != 0 && self.global_real_own_route(idx))
                {
                    return Ok(v);
                }
                // Function 0 is the top-level script: the slow path names it
                // only when it throws, and a host-side lookup has no enclosing
                // function to name.
                return self.load_global_slow(idx, 0).map_err(|t| {
                    let message = self.take_host_throw(t);
                    message
                        .strip_suffix(" (in <anonymous>)")
                        .map(str::to_owned)
                        .unwrap_or(message)
                });
            }
        }
        if self.global_this != 0 {
            let has_own = matches!(
                self.heap.get(self.global_this),
                HeapObj::Object(m) if m.pos(name).is_some()
            );
            let gobj = Value::heap(self.global_this);
            if has_own {
                return self
                    .get_prop(gobj, name)
                    .map_err(|t| self.take_host_throw(t));
            }
            let proto = self.object_get_prototype_of(gobj);
            if proto.is_heap() {
                let inherited = self
                    .has_property_str_dyn(proto, name)
                    .map_err(|t| self.take_host_throw(t))?;
                if inherited {
                    return self
                        .get_prop(gobj, name)
                        .map_err(|t| self.take_host_throw(t));
                }
            }
        }
        if let Some(v) = self.global_by_name(name) {
            return Ok(v);
        }
        Err(format!("ReferenceError: {name} is not defined"))
    }
}

impl<'p> Vm<'p> {
    /// Resolve a typed-array global to its region WITHOUT pinning: the buffer
    /// index to pin, then the region. Splitting the pin off lets a batch
    /// validate every entry before it mutates pin state.
    fn resolve_typed_array_region(
        &mut self,
        name: &str,
    ) -> Result<(u32, (usize, usize, u8)), String> {
        let v = self.named_global(name)?;
        if !v.is_heap() {
            return Err(format!("TypeError: {name} is not a typed array"));
        }
        let ta = v.heap_index();
        let (buffer, kind, byte_offset, length) = match self.heap.get(ta) {
            crate::heap::HeapObj::TypedArray {
                buffer,
                kind,
                byte_offset,
                length,
            } => (*buffer, *kind, *byte_offset, *length),
            _ => return Err(format!("TypeError: {name} is not a typed array")),
        };
        if crate::vm::native::TA_KINDS[kind as usize].2 {
            return Err(format!("TypeError: {name} is a BigInt typed array"));
        }
        if self.ta_tracking.contains(&ta) {
            return Err(format!("TypeError: {name} is a length-tracking view"));
        }
        let size = crate::vm::native::TA_KINDS[kind as usize].1;
        let (ptr, buf_len) = match self.heap.get(buffer) {
            crate::heap::HeapObj::ArrayBuffer {
                data,
                detached: false,
            } => (data.as_ptr() as usize, data.len()),
            _ => return Err(format!("TypeError: {name} is over a detached buffer")),
        };
        let bytes = length
            .checked_mul(size)
            .ok_or_else(|| format!("RangeError: {name} is too long"))?;
        if byte_offset
            .checked_add(bytes)
            .is_none_or(|end| end > buf_len)
        {
            return Err(format!("RangeError: {name} is out of its buffer's bounds"));
        }
        Ok((buffer, (ptr + byte_offset, length, kind)))
    }
}

impl<'p> HostCtx for Vm<'p> {
    fn typed_array_region(&mut self, name: &str) -> Result<(usize, usize, u8), String> {
        let (buffer, region) = self.resolve_typed_array_region(name)?;
        self.pinned_buffers.insert(buffer);
        Ok(region)
    }

    /// Every name is resolved before any buffer is pinned, so a request whose
    /// later entry is invalid leaves pin state exactly as it was (the
    /// 11 September 2026 audit's ZIPP-08). A buffer already pinned by an
    /// earlier request stays pinned either way: pins are for the VM's
    /// lifetime and shared, never counted.
    fn typed_array_regions(&mut self, names: &[&str]) -> Result<Vec<(usize, usize, u8)>, String> {
        let mut resolved = Vec::with_capacity(names.len());
        for name in names {
            resolved.push(self.resolve_typed_array_region(name)?);
        }
        Ok(resolved
            .into_iter()
            .map(|(buffer, region)| {
                self.pinned_buffers.insert(buffer);
                region
            })
            .collect())
    }

    fn call_global_numbers(&mut self, name: &str, args: &[f64]) -> Result<f64, String> {
        let callee = self.named_global(name)?;
        if !self.is_callable(callee) {
            return Err(format!("TypeError: {name} is not a function"));
        }
        let argv: Vec<Value> = args.iter().map(|&a| Value::num(a)).collect();
        let v = self
            .call_value(callee, Value::UNDEFINED, &argv)
            .map_err(|t| t.0)?;
        self.to_number(v).map_err(|t| t.0)
    }
}
