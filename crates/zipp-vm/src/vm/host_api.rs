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
use crate::heap::{HeapObj, ObjMap};
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

/// VM-relative byte offsets of the heap's shape/vals mirror bases (B178).
/// The shape-way probes load the base pointers through the live VM argument
/// on EVERY access — the mirror vectors grow when helpers allocate, and
/// unlike the pinned `r13` versions base nothing re-derives these, so a
/// baked address would dangle after growth.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_HOT_MIRROR_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, hot_mirror_raw);
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
/// B189b mirror for the emitted call lane: a Closure occupant's captured
/// `this` bits (the upvalue base travels helper-side through
/// `jit_cross3_enter`, so it has no emitted offset).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_THIS_MIRROR_RAW_OFFSET: usize = core::mem::offset_of!(Vm<'static>, heap)
    + core::mem::offset_of!(crate::heap::Heap, this_mirror_raw);
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
pub(crate) const JIT_OBJ_REALM_NONEMPTY_OFFSET: usize = core::mem::offset_of!(Vm<'static>, obj_realm)
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

/// Default structural-conversion limits used at every host boundary.
///
/// A depth limit alone is not sufficient: a guest can build a tiny shared DAG
/// (`x = [x, x]` repeatedly) whose tree-shaped host representation expands
/// exponentially. These limits bound the representation itself, including
/// object keys and string payloads, before it is handed to an embedder.
pub const DEFAULT_HOST_VALUE_MAX_NODES: usize = 100_000;
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
        let HostValue::Object(pairs) = hv else {
            return self.host_in(hv, depth);
        };
        if depth >= MAX_DEPTH || !old.is_heap() {
            return self.host_in(hv, depth);
        }
        // Snapshot the old object's own data properties, then drop the borrow.
        let old_props: Vec<(String, Value)> = match self.heap.get(old.heap_index()) {
            HeapObj::Object(m) => (0..m.keys.len())
                .filter(|&i| !m.attr_at(i).accessor)
                .map(|i| (m.keys[i].clone(), m.val_at(i)))
                .collect(),
            _ => return self.host_in(hv, depth),
        };
        let find_old = |k: &str| old_props.iter().find(|(ok, _)| ok == k).map(|(_, v)| *v);

        let mut m = ObjMap::with_capacity(pairs.len().max(old_props.len()));
        for (k, val) in pairs {
            let prev = find_old(k);
            let v = match (val, prev) {
                // The host is not overwriting here — it is echoing back a value
                // it was never able to see. Keep what is really there.
                (HostValue::Null | HostValue::Undefined | HostValue::Opaque, Some(p))
                    if self.host_is_opaque(p) =>
                {
                    p
                }
                (_, Some(p)) => self.host_in_over(p, val, depth + 1),
                (_, None) => self.host_in(val, depth + 1),
            };
            m.set(k, v);
        }
        // Keys the host dropped entirely: preserve the opaque ones for the same
        // reason. Data the host omitted is treated as deliberately removed.
        for (k, p) in &old_props {
            if !pairs.iter().any(|(pk, _)| pk == k) && self.host_is_opaque(*p) {
                m.set(k, *p);
            }
        }
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
