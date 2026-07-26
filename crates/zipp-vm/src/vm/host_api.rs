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
        let mut push = |slot: u32, scope: SymbolScope, out: &mut Vec<Symbol>| {
            if let Some(name) = p.global_names.get(slot as usize) {
                if !name.is_empty() {
                    out.push(Symbol { name: name.clone(), index: slot, scope });
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
    pub(crate) fn host_get_slot(&mut self, index: u32) -> HostValue {
        let v = match self.globals.get(index as usize) {
            Some(v) => *v,
            None => return HostValue::Undefined,
        };
        if v.is_uninitialized() {
            return HostValue::Undefined;
        }
        let _g = self.gc_lock_guard();
        let mut seen: Vec<u32> = Vec::new();
        self.host_out(v, 0, &mut seen)
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
        let v = self.host_in(hv, 0);
        self.globals[index as usize] = v;
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
                Ok(self.host_out(v, 0, &mut seen))
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
    fn host_out(&mut self, v: Value, depth: usize, seen: &mut Vec<u32>) -> HostValue {
        if v.is_undefined() || v.is_uninitialized() {
            return HostValue::Undefined;
        }
        if v.is_null() {
            return HostValue::Null;
        }
        if v.is_bool() {
            return HostValue::Bool(v.as_bool());
        }
        if v.is_int() {
            return HostValue::Number(v.as_int() as f64);
        }
        if v.is_double() {
            return HostValue::Number(v.as_f64());
        }
        if !v.is_heap() {
            return HostValue::Opaque;
        }
        let idx = v.heap_index();
        if depth >= MAX_DEPTH || seen.contains(&idx) {
            return HostValue::Null;
        }

        // Classify and copy out of the heap in a short borrow, so the recursive
        // step below is free to allocate and mutate.
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
                let mut pairs = Vec::with_capacity(m.keys.len());
                for i in 0..m.keys.len() {
                    let a = &m.attrs[i];
                    // Accessors are not invoked: running user code in the middle
                    // of a marshal would let a getter mutate the graph being walked.
                    if !a.enumerable || a.accessor {
                        continue;
                    }
                    pairs.push((m.keys[i].clone(), m.vals[i]));
                }
                Shape::Object(pairs)
            }
            _ => Shape::Opaque,
        };

        match shape {
            Shape::Opaque => HostValue::Opaque,
            Shape::Str => match self.to_js_string(v) {
                Ok(s) => HostValue::String(s),
                Err(_) => HostValue::String(String::new()),
            },
            Shape::Array(items) => {
                seen.push(idx);
                let out = items
                    .into_iter()
                    .map(|it| {
                        if it.is_hole() {
                            HostValue::Undefined
                        } else {
                            self.host_out(it, depth + 1, seen)
                        }
                    })
                    .collect();
                seen.pop();
                HostValue::Array(out)
            }
            Shape::Object(pairs) => {
                seen.push(idx);
                let out = pairs
                    .into_iter()
                    .map(|(k, val)| (k, self.host_out(val, depth + 1, seen)))
                    .collect();
                seen.pop();
                HostValue::Object(out)
            }
        }
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
                let vals: Vec<Value> =
                    items.iter().map(|it| self.host_in(it, depth + 1)).collect();
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
