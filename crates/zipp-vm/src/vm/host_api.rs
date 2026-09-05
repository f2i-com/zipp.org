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
pub(crate) const JIT_GEN_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, gen_raw);
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
    String(String),
    Array(Vec<HostValue>),
    /// A plain object, as its own enumerable data properties in insertion
    /// order. Accessors are not invoked and do not appear.
    Object(Vec<(String, HostValue)>),
    /// Something that cannot cross as data: a function, class, `Map`, `Set`,
    /// `Date`, `RegExp`, typed array, proxy, … Reading one yields `Opaque`;
    /// writing one is ignored.
    Opaque,
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
/// Nodes one fingerprint will walk before giving up and reporting "unknown".
/// Well above any real UI state, and far below anything that would make the
/// walk cost more than the copy it exists to avoid.
const FP_MAX_NODES: usize = 2_000_000;

#[inline]
fn fp_mix(h: &mut u64, x: u64) {
    *h ^= x;
    *h = h.wrapping_mul(0x100_0000_01b3);
    *h ^= *h >> 29;
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
        let Some(total) = self.used_string_bytes.checked_add(value.len()) else {
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
    /// `None` means "assume it changed" — returned when the graph is larger
    /// than this walk will traverse, so a pathological value degrades to the old
    /// always-copy behaviour rather than to a wrong answer.
    pub(crate) fn host_fingerprint_slot(&mut self, index: u32, seed: u64) -> Option<u64> {
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
        let mut nodes: usize = 0;
        if self.host_fp(v, 0, &mut seen, &mut h, &mut nodes) {
            Some(h)
        } else {
            None
        }
    }

    /// Hash `v` into `h`. False means the node budget ran out, which makes
    /// the whole fingerprint unusable rather than partial — a partial digest
    /// would be stable across a change in the part it never reached.
    fn host_fp(
        &mut self,
        v: Value,
        depth: usize,
        seen: &mut Vec<u32>,
        h: &mut u64,
        nodes: &mut usize,
    ) -> bool {
        *nodes += 1;
        if *nodes > FP_MAX_NODES {
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
            Str,
            Array(Vec<Value>),
            Object(Vec<(String, Value)>),
            Opaque,
        }
        let shape = match self.heap.get(idx) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => Shape::Str,
            HeapObj::Array(items) => Shape::Array(items.clone()),
            HeapObj::Object(m) => {
                let mut pairs = Vec::new();
                for i in 0..m.keys.len() {
                    let a = &m.attr_at(i);
                    // The same exclusion host_out makes: an accessor is never
                    // invoked, so it contributes nothing to the marshalled value
                    // and must contribute nothing to the digest either.
                    if !a.enumerable || a.accessor {
                        continue;
                    }
                    pairs.push((m.keys[i].clone(), m.val_at(i)));
                }
                Shape::Object(pairs)
            }
            _ => Shape::Opaque,
        };

        match shape {
            Shape::Opaque => {
                fp_mix(h, 9);
                true
            }
            Shape::Str => {
                fp_mix(h, 10);
                let s = self.to_js_string(v).unwrap_or_default();
                fp_mix(h, s.len() as u64);
                for chunk in s.as_bytes().chunks(8) {
                    let mut word = 0u64;
                    for (n, b) in chunk.iter().enumerate() {
                        word |= (*b as u64) << (n * 8);
                    }
                    fp_mix(h, word);
                }
                true
            }
            Shape::Array(items) => {
                fp_mix(h, 11);
                fp_mix(h, items.len() as u64);
                seen.push(idx);
                for it in items {
                    let ok = if it.is_hole() {
                        fp_mix(h, 12);
                        true
                    } else {
                        self.host_fp(it, depth + 1, seen, h, nodes)
                    };
                    if !ok {
                        seen.pop();
                        return false;
                    }
                }
                seen.pop();
                true
            }
            Shape::Object(pairs) => {
                fp_mix(h, 13);
                fp_mix(h, pairs.len() as u64);
                seen.push(idx);
                for (k, val) in pairs {
                    fp_mix(h, k.len() as u64);
                    for chunk in k.as_bytes().chunks(8) {
                        let mut word = 0u64;
                        for (n, b) in chunk.iter().enumerate() {
                            word |= (*b as u64) << (n * 8);
                        }
                        fp_mix(h, word);
                    }
                    if !self.host_fp(val, depth + 1, seen, h, nodes) {
                        seen.pop();
                        return false;
                    }
                }
                seen.pop();
                true
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
        let callee = match self.globals.get(index as usize) {
            Some(v) => *v,
            None => return Err(format!("zipp: no global in slot {index}")),
        };
        if !self.is_callable(callee) {
            return Err(format!("TypeError: global slot {index} is not a function"));
        }
        let argv: Vec<Value> = {
            let _g = self.gc_lock_guard();
            args.iter().map(|a| self.host_in(a, 0)).collect()
        };
        let res = self.call_value(callee, Value::UNDEFINED, &argv);
        // Drain regardless of outcome: a throw can still have queued jobs, and
        // leaving them parked would surface them at an arbitrary later call.
        self.drain_microtasks();
        match res {
            Ok(v) => {
                let _g = self.gc_lock_guard();
                let mut seen: Vec<u32> = Vec::new();
                let mut budget = HostValueBudget::default();
                self.host_out(v, 0, &mut seen, &mut budget)
            }
            Err(t) => Err(t.0),
        }
    }

    /// Run any pending microtasks. A host that resumed the script by writing
    /// globals (rather than calling a function) uses this to let promise
    /// continuations observe the write.
    pub(crate) fn host_pump(&mut self) {
        self.drain_microtasks();
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
                let s = self.to_js_string(v).unwrap_or_default();
                budget.charge_string(&s)?;
                Ok(HostValue::String(s))
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
        let find_old = |k: &str| {
            old_props
                .iter()
                .find(|(ok, _, _)| ok == k)
                .map(|(_, v, a)| (*v, *a))
        };

        let mut m = ObjMap::with_capacity(pairs.len().max(old_props.len()));
        for (k, val) in pairs {
            let prev = find_old(k);
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
        for (k, p, a) in &old_props {
            let host_sent_it = pairs.iter().any(|(pk, _)| pk == k);
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
    /// Call the guest function a global names, with numbers, for a number.
    /// Runs the guest re-entrantly inside the host call; the guest cannot make
    /// a nested host call while it does.
    fn call_global_numbers(&mut self, name: &str, args: &[f64]) -> Result<f64, String>;
}

/// The context-taking twin of [`crate::embed::HostCall`]: the same string
/// contract, plus the VM as [`HostCtx`] for the duration of the call.
pub type HostCallCtx =
    Box<dyn FnMut(&mut dyn HostCtx, &str, &[String]) -> Result<String, String>>;

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
}

impl<'p> HostCtx for Vm<'p> {
    fn typed_array_region(&mut self, name: &str) -> Result<(usize, usize, u8), String> {
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
        self.pinned_buffers.insert(buffer);
        Ok((ptr + byte_offset, length, kind))
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
