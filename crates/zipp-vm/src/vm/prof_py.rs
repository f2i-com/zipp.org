//! `ZIPP_PY_PROF=1`: counters for the Python tier, printed at exit by the
//! CLI (`zipp_vm::py_prof_report`).
//!
//! * every fused Python instruction's hits and slow edges (and the sites
//!   whose slow edges are most taken);
//! * interpreted function entries by callee: the runtime's `R.*` / `rt.*`
//!   helpers by the name they are published under (`__zipp_py_bind` records
//!   those names), Python functions by their own;
//! * allocations by heap kind (records by their first keys), read off the
//!   collector's young log between steps (so an allocation the collector
//!   reclaimed before the next step is missed, and a pretenured large one is
//!   never logged).
//!
//! Off unless the variable is set: each hook is one relaxed load and a
//! branch. The fused steps run for the JIT too (`jit_py_op`), so their
//! counts hold with the JIT on; function entries are counted where the
//! interpreter pushes a frame (`setup_call`), so run with `ZIPP_PY_JIT=0` for
//! complete helper counts.

use super::*;
use crate::bytecode::Instr;
use crate::heap::HeapObj;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicU8;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[cfg(not(target_arch = "wasm32"))]
static ON: AtomicU8 = AtomicU8::new(2);

/// Whether `ZIPP_PY_PROF` is set (read once). Never in the WebAssembly
/// build, which has no environment to set it in and no exit report: there
/// the counting code is not compiled in.
#[cfg(target_arch = "wasm32")]
#[inline(always)]
pub(crate) fn on() -> bool {
    false
}

/// Whether `ZIPP_PY_PROF` is set (read once).
#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub(crate) fn on() -> bool {
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => init(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cold]
fn init() -> bool {
    let v = std::env::var_os("ZIPP_PY_PROF").is_some_and(|v| v != "0");
    ON.store(v as u8, Ordering::Relaxed);
    v
}

/// The fused instructions, in [`op_of`]'s numbering.
const OPS: [&str; 30] = [
    "PyArith", "PyAddImm", "PyCompare", "PyJumpCompare", "PyClassOf", "PyDictGet", "PyDictSet", "PyCallEntry",
    "PyGetItem", "PySetItem", "PyGlobal", "PyStrItem", "PyStrLen", "PyGetAttr", "PySetAttr", "PyIsInstance",
    "PyGenNext", "PyMethod", "PyModGet", "PyLen", "PyAttrFn", "PySeq", "PyRaise", "PyCaught", "PyClassAttr",
    "PyDictLookup", "PyUnpack", "PyMakeExc", "PyExcPop", "PyNew",
];

#[allow(clippy::declare_interior_mutable_const)]
const Z: AtomicU64 = AtomicU64::new(0);
static HIT: [AtomicU64; 30] = [Z; 30];
static SLOW: [AtomicU64; 30] = [Z; 30];

/// A fused instruction's number and slow edge.
fn op_of(i: &Instr) -> Option<(usize, u32)> {
    Some(match *i {
        Instr::PyArith { slow, .. } => (0, slow),
        Instr::PyAddImm { slow, .. } => (1, slow),
        Instr::PyCompare { slow, .. } => (2, slow),
        Instr::PyJumpCompare { slow, .. } => (3, slow),
        Instr::PyClassOf { slow, .. } => (4, slow),
        Instr::PyDictGet { slow, .. } => (5, slow),
        Instr::PyDictSet { slow, .. } => (6, slow),
        Instr::PyCallEntry { slow, .. } => (7, slow),
        Instr::PyGetItem { slow, .. } => (8, slow),
        Instr::PySetItem { slow, .. } => (9, slow),
        Instr::PyGlobal { slow, .. } => (10, slow),
        Instr::PyStrItem { slow, .. } => (11, slow),
        Instr::PyStrLen { slow, .. } => (12, slow),
        Instr::PyGetAttr { slow, .. } => (13, slow),
        Instr::PySetAttr { slow, .. } => (14, slow),
        Instr::PyIsInstance { slow, .. } => (15, slow),
        Instr::PyGenNext { slow, .. } => (16, slow),
        Instr::PyMethod { slow, .. } => (17, slow),
        Instr::PyModGet { slow, .. } => (18, slow),
        Instr::PyLen { slow, .. } => (19, slow),
        Instr::PyAttrFn { slow, .. } => (20, slow),
        Instr::PySeq { slow, .. } => (21, slow),
        Instr::PyRaise { slow, .. } => (22, slow),
        Instr::PyCaught { slow, .. } => (23, slow),
        Instr::PyClassAttr { slow, .. } => (24, slow),
        Instr::PyDictLookup { slow, .. } => (25, slow),
        Instr::PyUnpack { slow, .. } => (26, slow),
        Instr::PyMakeExc { slow, .. } => (27, slow),
        Instr::PyExcPop { slow, .. } => (28, slow),
        Instr::PyNew { slow, .. } => (29, slow),
        _ => return None,
    })
}

#[derive(Default)]
struct State {
    /// Slow edges by site `(func, ip)`: the op and the count.
    sites: rustc_hash::FxHashMap<(u32, u32), (usize, u64)>,
    /// Interpreted entries by function id.
    calls: rustc_hash::FxHashMap<u32, u64>,
    /// Function labels (published helper names override proto names).
    names: rustc_hash::FxHashMap<u32, String>,
    /// Allocations by kind label.
    allocs: rustc_hash::FxHashMap<String, u64>,
    /// The young log as last read: its length and last slot.
    young_len: usize,
    young_last: u32,
    /// Times the log was found reset (a collection ran between reads).
    young_resets: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

impl<'p> Vm<'p> {
    /// Count one fused step's outcome (`next` its next ip, or its throw).
    #[cold]
    #[inline(never)]
    pub(crate) fn py_prof_step(&mut self, func_id: u32, ip: usize, instr: &Instr, next: &Result<usize, Thrown>) {
        let Some((op, slow)) = op_of(instr) else {
            return;
        };
        let slowed = matches!(next, Ok(n) if *n == slow as usize);
        if slowed {
            SLOW[op].fetch_add(1, Ordering::Relaxed);
        } else {
            HIT[op].fetch_add(1, Ordering::Relaxed);
        }
        with_state(|st| {
            if slowed {
                st.sites.entry((func_id, ip as u32)).or_insert((op, 0)).1 += 1;
                self.py_prof_name(st, func_id);
            }
            self.py_prof_census(st);
        });
    }

    /// Count one interpreted function entry.
    #[cold]
    #[inline(never)]
    pub(crate) fn py_prof_call(&mut self, func_id: u32) {
        with_state(|st| {
            *st.calls.entry(func_id).or_insert(0) += 1;
            self.py_prof_name(st, func_id);
            self.py_prof_census(st);
        });
    }

    /// Record the name a runtime helper is published under.
    #[cold]
    pub(crate) fn py_prof_publish(&mut self, func_id: u32, label: String) {
        with_state(|st| {
            st.names.insert(func_id, label);
        });
    }

    fn py_prof_name(&self, st: &mut State, func_id: u32) {
        if st.names.contains_key(&func_id) {
            return;
        }
        let f = self.func(func_id as usize);
        let py = f.code.iter().any(|i| op_of(i).is_some());
        let base = if f.name.is_empty() { format!("<anon#{func_id}>") } else { f.name.clone() };
        st.names.insert(func_id, if py { format!("py {base}") } else { base });
    }

    /// Classify what the young log gained since the last read.
    fn py_prof_census(&self, st: &mut State) {
        let log = self.heap.young_log();
        let mut from = st.young_len;
        if from > log.len() || (from > 0 && log[from - 1] != st.young_last) {
            st.young_resets += 1;
            from = 0;
        }
        for &i in &log[from..] {
            let label = self.py_prof_kind(i);
            match st.allocs.get_mut(label.as_str()) {
                Some(n) => *n += 1,
                None => {
                    st.allocs.insert(label, 1);
                }
            }
        }
        st.young_len = log.len();
        st.young_last = log.last().copied().unwrap_or(0);
    }

    fn py_prof_kind(&self, i: u32) -> String {
        let k = match self.heap.get(i) {
            HeapObj::Object(m) => {
                let n = m.len().min(3);
                let keys: Vec<&str> = (0..n).map(|s| m.key_at(s)).collect();
                return format!("Object{{{}{}}}", keys.join(","), if m.len() > 3 { ",.." } else { "" });
            }
            HeapObj::Str(_) => "Str",
            HeapObj::Cons { .. } => "Cons",
            HeapObj::Array(_) => "Array",
            HeapObj::Map { .. } => "Map",
            HeapObj::Set(_) => "Set",
            HeapObj::BigInt(_) => "BigInt",
            HeapObj::BigIntBig(_) => "BigIntBig",
            HeapObj::Closure { .. } => "Closure",
            HeapObj::Cell => "Cell",
            HeapObj::Func(_) => "Func",
            HeapObj::Native(_) => "Native",
            HeapObj::NativeClosure { .. } => "NativeClosure",
            HeapObj::Bound { .. } => "Bound",
            HeapObj::Iterator { .. } => "Iterator",
            HeapObj::Generator { .. } => "Generator",
            HeapObj::TypedArray { .. } => "TypedArray",
            HeapObj::ArrayBuffer { .. } => "ArrayBuffer",
            _ => "other",
        };
        k.to_string()
    }
}

/// The report (empty when the variable is unset).
pub fn report() -> String {
    use std::fmt::Write;
    if !on() {
        return String::new();
    }
    let mut out = String::new();
    let mut ops: Vec<(usize, u64, u64)> = (0..OPS.len())
        .map(|i| (i, HIT[i].load(Ordering::Relaxed), SLOW[i].load(Ordering::Relaxed)))
        .filter(|&(_, h, s)| h + s > 0)
        .collect();
    ops.sort_by(|a, b| (b.1 + b.2).cmp(&(a.1 + a.2)));
    let _ = writeln!(out, "[pyprof] fused ops: op hits slow slow%");
    for (i, h, s) in ops {
        let _ = writeln!(out, "[pyprof] op {:<14} {:>12} {:>12} {:>6.1}%", OPS[i], h, s, s as f64 * 100.0 / (h + s) as f64);
    }
    with_state(|st| {
        let mut sites: Vec<_> = st.sites.iter().collect();
        sites.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        let _ = writeln!(out, "[pyprof] top slow-edge sites: count op function@ip");
        for ((f, ip), (op, n)) in sites.into_iter().take(25) {
            let name = st.names.get(f).map(String::as_str).unwrap_or("?");
            let _ = writeln!(out, "[pyprof] slow {:>12} {:<14} {}@{}", n, OPS[*op], name, ip);
        }
        let mut calls: Vec<_> = st.calls.iter().collect();
        calls.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = st.calls.values().sum();
        let _ = writeln!(out, "[pyprof] interpreted calls: {total} total; top callees");
        for (f, n) in calls.into_iter().take(30) {
            let name = st.names.get(f).map(String::as_str).unwrap_or("?");
            let _ = writeln!(out, "[pyprof] call {:>12} {}", n, name);
        }
        let mut allocs: Vec<_> = st.allocs.iter().collect();
        allocs.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = st.allocs.values().sum();
        let _ = writeln!(out, "[pyprof] allocations: {total} seen ({} log resets); by kind", st.young_resets);
        for (k, n) in allocs.into_iter().take(25) {
            let _ = writeln!(out, "[pyprof] alloc {:>12} {:>5.1}% {}", n, *n as f64 * 100.0 / total.max(1) as f64, k);
        }
    });
    out
}
