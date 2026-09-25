//! The Python runtime's registry, per VM (`Vm::py_rt`), and the fused
//! instructions' per-site caches.
//!
//! The runtime (`frontend/python/runtime/*.js`) ends its initialisation with
//! `__zipp_py_bind(R, pins, dict)` (entry.js), handing the engine its helper
//! object once. What the fused instructions (`vm::py_ops`, `vm::py_attr`)
//! need from it is read then and kept here: the builtin type objects and
//! sentinels `R` publishes, the runtime's template records and the shapes of
//! the records made like them (a list or tuple `{cls, items}`, an instance
//! `{cls, dict}`, an exception, a dict), and the slots of a class record's
//! fields (`makeType`'s literal). "Is this a list" is then one shape compare
//! and one bits compare. Every value kept is also pushed onto `pins` (an
//! array `R` holds), so none is collected while this registry names it.
//!
//! Shapes are the thread's (`shape::TABLE`): a VM runs on one thread for its
//! life, which is what makes keeping them per VM sound.
//!
//! ## Per-site caches (`PyIc`)
//!
//! Each function holding fused instructions gets, on first use, one entry
//! per instruction that caches (attribute, method, global and `isinstance`
//! sites). An entry records what the instruction's ordinary path found and
//! the facts that make it still true:
//!
//! * a class by its bits, its heap version (`Heap::version_of`: a key added
//!   to or removed from the record, or its slot swept and reused, changes
//!   it) and its `ver` (the runtime bumps it whenever its cache tables lose
//!   an entry, see core.js `makeType`), so "the class's table said `name` is
//!   a plain instance attribute" holds for as long as the three match;
//! * a `Map` entry by its position and its key there: the key's bits and a
//!   stamp (`Vm::py_key_stamp`: the site's rooted constant, else the key's
//!   slot version). A `Map`'s keys only append and a deleted entry's key
//!   becomes a hole, but a key that is not a rooted constant can die with
//!   its `Map` and its slot hold another `Map`'s key at the same position,
//!   so the bits alone do not name the entry;
//! * a name's absence from a `Map` by the Map's bits, heap version and key
//!   count (no key appended since the lookup that missed).
//!
//! Entries are not GC roots: every Value an entry names is compared by bits
//! against a live one before anything is read through it, or read afresh
//! from a live, guard-checked holder.

use super::*;
use crate::heap::HeapObj;
use crate::value::Value;
use std::cell::Cell;

/// Per-VM slot hints for runtime records read by name (see
/// [`Vm::py_hint_field`]); hints only: every use checks the key.
pub(crate) mod hint {
    pub const CTOR_INIT: usize = 0;
    pub const HIT_CLS: usize = 1;
    pub const HIT_F: usize = 2;
    pub const STROP: usize = 3;
    pub const LTMPL: usize = 4;
    pub const TTYPE: usize = 5;
    pub const IKIND: usize = 6;
    pub const IA: usize = 7;
    pub const II: usize = 8;
    pub const IB: usize = 9;
    pub const ISIZE: usize = 10;
    pub const IPICK: usize = 11;
    pub const ITMPL: usize = 12;
    pub const ITTYPE: usize = 13;
    pub const ISIZE_DICT: usize = 14;
    pub const IITEMS: usize = 15;
    pub const JNEXT: usize = 16;
    pub const MAP: usize = 17;
    pub const GS: usize = 18;
    pub const GLOBALS: usize = 19;
    pub const DEFAULTS: usize = 20;
    pub const COUNT: usize = 21;
}

/// Kinds of [`PyIc`] entries.
pub(super) mod ic {
    /// `PyGetAttr`: `a` class, `b` dict key bits, `d` its stamp (the key's
    /// slot version, or `KEY_ROOTED`: bits alone can name a later key, see
    /// `py_map_at`), `c` class `ver`, `pos`.
    pub const ATTR_GET: u8 = 1;
    /// `PySetAttr` replacing an entry: as [`ATTR_GET`].
    pub const ATTR_SET: u8 = 2;
    /// `PySetAttr` appending the name to a dict of `pos` entries.
    pub const ATTR_APPEND: u8 = 3;
    /// `PyGlobal` from the module globals: `a` the Map, `b` the key bits,
    /// `d` the key's stamp (as [`ATTR_GET`]), `pos`.
    pub const GLOBAL: u8 = 4;
    /// `PyGlobal` from the builtins: `a` the globals Map (absent there at
    /// `c` keys), `b` the builtins key bits, `d` its stamp, `pos` in the
    /// builtins.
    pub const BUILTIN: u8 = 5;
    /// `PyMethod` from a class's `gm`: `a` class, `b` the table's bits,
    /// `c` class `ver`, `pos` the table slot.
    pub const METHOD: u8 = 6;
    /// `PyClassAttr` from `gv`: as [`METHOD`].
    pub const CLASS_ATTR: u8 = 7;
    /// `PyAttrFn` from `gp` / `sp`: as [`METHOD`].
    pub const ATTR_FN: u8 = 8;
    /// `PyIsInstance`: `a` a class record found to be a type.
    pub const IS_TYPE: u8 = 9;
    /// `PyMethod` from a type's `gb` (a str's, or a dict-less builtin
    /// container's): as [`METHOD`].
    pub const BUILTIN_METHOD: u8 = 10;
    /// `PyNew`: slot hints only, `pos` the class's entry, `hver` the
    /// `__init__`'s.
    pub const NEW: u8 = 11;
    /// `PyCallEntry`: `a` the function record, `hver` its heap version,
    /// `pos` the entry's slot.
    pub const CALL_ENTRY: u8 = 12;
    /// `PyGetAttr` / `PyClassAttr` (`ga`) or `PySetAttr` (`sa`) where the
    /// class's table did not say `true` for the name: `a` class, `c` its
    /// `ver`, `b` the table's bits, `pos` the name's slot in it (`u32::MAX`
    /// when absent, then `d` the table's key count). The instruction takes
    /// its slow edge while that still holds.
    pub const NOT_PLAIN: u8 = 13;
    /// `PyGetAttr` from layout-mode storage (`vm::py_table::layout`): `a`
    /// class, `c` its `ver`, `d` the layout, `pos` the slot.
    pub const ATTR_GET_L: u8 = 14;
    /// `PySetAttr` replacing a value in layout-mode storage: as
    /// [`ATTR_GET_L`].
    pub const ATTR_SET_L: u8 = 15;
    /// `PySetAttr` appending to layout-mode storage of layout `d` holding
    /// `pos` values, making layout `b`.
    pub const ATTR_APPEND_L: u8 = 16;
    /// `PyGetAttr` answering a class's plain attribute (`gv`, `b` the
    /// table's bits, `pos` its slot) for an instance whose layout `d` lacks
    /// the name. (`METHOD` and `CLASS_ATTR` entries keep in `d` a layout
    /// proven to lack their name, 0 for none.)
    pub const GET_CLASS_L: u8 = 17;
}

/// One per-site cache entry (see the module comment); kind 0 is empty.
#[derive(Clone, Copy, Default)]
pub(super) struct PyIc {
    pub kind: u8,
    pub pos: u32,
    pub hver: u32,
    pub a: u64,
    pub b: u64,
    pub c: u64,
    pub d: u32,
}

/// A function's entries: the entry of each caching instruction by ip.
pub(super) struct FnIc {
    slot_of: Box<[u16]>,
    ents: Box<[PyIc]>,
}

/// The slots of a class record's fields (`makeType`'s literal).
#[derive(Clone, Copy)]
pub(super) struct TySlots {
    pub mro: usize,
    pub is_type: usize,
    pub ga: usize,
    pub sa: usize,
    pub gm: usize,
    pub gb: usize,
    pub gp: usize,
    pub sp: usize,
    pub gv: usize,
    pub ver: usize,
}

/// The registry.
pub(crate) struct PyRt {
    /// The runtime's helper object `R`.
    pub(super) r: Value,
    pub(super) t_list: Value,
    pub(super) t_tuple: Value,
    pub(super) t_dict: Value,
    pub(super) t_set: Value,
    pub(super) t_str: Value,
    pub(super) t_module: Value,
    pub(super) ebase: Value,
    /// `R.ISTYPES`: the types of None, bool, int, float and str, and bytes.
    pub(super) istypes: [Value; 6],
    /// `R.BUILTINS` (a Map), `R.EXCSTACK` and `R.CTORS` (Arrays).
    pub(super) builtins: Value,
    pub(super) excstack: Value,
    pub(super) ctors: Value,
    /// The template records (`R.SEQTMPL`, `R.EXCTMPL`, `R.INSTTMPL`).
    pub(super) seq_tmpl: Value,
    pub(super) exc_tmpl: Value,
    pub(super) inst_tmpl: Value,
    /// Shapes of records laid out as the templates (`shape::DICT`: none).
    pub(super) seq_shape: u32,
    pub(super) inst_shape: u32,
    pub(super) exc_shape: u32,
    /// A dict record's shape and its `map`, `size` slots.
    pub(super) dict_shape: u32,
    pub(super) dict_slots: [usize; 2],
    /// A class record's field slots (`None`: the literal's keys not found).
    pub(super) ty: Option<TySlots>,
    /// `R.mself`'s slot.
    pub(super) mself_slot: Option<usize>,
    /// The generator record shape `vm::py_gen` found plain.
    pub(super) gen_shape: Cell<u32>,
    pub(super) hints: [Cell<u16>; hint::COUNT],
    /// Interned attribute and global names (see `Vm::py_intern`).
    names: rustc_hash::FxHashMap<Box<[u8]>, Value>,
    ics: Vec<Option<Box<FnIc>>>,
    /// Frame tables by function id, read on first use (see [`PyFrame`]).
    frames: Vec<FrameSlot>,
    /// The `pins` array `R` holds, which keeps the values named here alive.
    pub(super) pins: Value,
    /// Instance attribute layouts (`vm::py_table::layout`).
    pub(crate) layouts: super::py_table::layout::Layouts,
}

/// The first bytes of the string constant a Python code object's frame
/// table is (the emitter's `FRAME_TABLE_TAG`, which must agree).
const FRAME_TABLE_TAG: &str = "\u{1}pyframe:";

/// A Python code object's frame table, which the emitter leaves as the
/// code object's last string constant (`Emitter::finish`): the frame guard
/// and the line of each ip.
pub(super) struct PyFrame {
    /// `(from, start, end, exception register)`: an exception leaving the
    /// frame from an ip in `from..` outside `start..end` enters the guard
    /// block at `start` (the handler the emitter's `frame_guard` lays out),
    /// the exception in the register.
    guard: Option<(u32, u32, u32, u16)>,
    /// The register the guard block and the handlers read the line from.
    line_reg: u16,
    /// `(ip, line)`: the encoded line from each ip on.
    lines: Box<[(u32, i32)]>,
}

#[derive(Default)]
enum FrameSlot {
    #[default]
    Unread,
    None,
    Some(Box<PyFrame>),
}

/// Where a Python code object's frame guard block starts (its main body
/// ends just before it, its out-of-line blocks follow it), from its frame
/// table; `None` for code without one.
#[cfg_attr(not(all(feature = "jit", target_arch = "x86_64")), allow(dead_code))]
pub(crate) fn py_frame_guard_start(proto: &crate::bytecode::FuncProto) -> Option<u32> {
    let body = proto.string_constants.last()?.strip_prefix(FRAME_TABLE_TAG)?;
    let head = body.split(';').next()?;
    let mut fields = head.split(',');
    fields.next()?.parse::<u32>().ok()?;
    fields.next()?.parse().ok()
}

impl PyFrame {
    #[inline(never)]
    fn parse(text: &str) -> Option<PyFrame> {
        let body = text.strip_prefix(FRAME_TABLE_TAG)?;
        let mut parts = body.split(';');
        let head: Vec<&str> = parts.next()?.split(',').collect();
        let (guard, line_reg) = match head.as_slice() {
            ["-", l] => (None, l.parse().ok()?),
            [f, s, e, x, l] => (Some((f.parse().ok()?, s.parse().ok()?, e.parse().ok()?, x.parse().ok()?)), l.parse().ok()?),
            _ => return None,
        };
        let (mut ip, mut line) = (0u32, 0i32);
        let mut lines = Vec::new();
        for entry in parts {
            let (a, b) = entry.split_once(',')?;
            ip = ip.checked_add(a.parse().ok()?)?;
            line = line.checked_add(b.parse().ok()?)?;
            lines.push((ip, line));
        }
        Some(PyFrame { guard, line_reg, lines: lines.into_boxed_slice() })
    }

    /// The encoded line of the instruction at `ip`.
    fn line(&self, ip: u32) -> Option<i32> {
        let n = self.lines.partition_point(|&(at, _)| at <= ip);
        n.checked_sub(1).map(|i| self.lines[i].1)
    }
}

/// The keys of a list or tuple record (`sequence`'s literal).
pub(super) const SEQ_KEYS: [&str; 2] = ["cls", "items"];
/// The keys of the record `makeExc` builds, in its literal's order.
pub(super) const EXC_KEYS: [&str; 9] = ["cls", "dict", "args", "cause", "context", "tbline", "suppress", "tbfn", "tbcl"];
/// An instance record's keys (the construction entries' literal).
pub(super) const INST_KEYS: [&str; 2] = ["cls", "dict"];

impl PyRt {
    fn new(r: Value) -> PyRt {
        PyRt {
            r,
            t_list: Value::UNDEFINED,
            t_tuple: Value::UNDEFINED,
            t_dict: Value::UNDEFINED,
            t_set: Value::UNDEFINED,
            t_str: Value::UNDEFINED,
            t_module: Value::UNDEFINED,
            ebase: Value::UNDEFINED,
            istypes: [Value::UNDEFINED; 6],
            builtins: Value::UNDEFINED,
            excstack: Value::UNDEFINED,
            ctors: Value::UNDEFINED,
            seq_tmpl: Value::UNDEFINED,
            exc_tmpl: Value::UNDEFINED,
            inst_tmpl: Value::UNDEFINED,
            seq_shape: crate::shape::DICT,
            inst_shape: crate::shape::DICT,
            exc_shape: crate::shape::DICT,
            dict_shape: crate::shape::DICT,
            dict_slots: [1, 2],
            ty: None,
            mself_slot: None,
            gen_shape: Cell::new(crate::shape::DICT),
            hints: std::array::from_fn(|_| Cell::new(0)),
            names: rustc_hash::FxHashMap::default(),
            ics: Vec::new(),
            frames: Vec::new(),
            pins: Value::UNDEFINED,
            layouts: Default::default(),
        }
    }
}

/// Whether two short keys are equal, compared inline (the record keys
/// checked at a known slot are a few bytes; a `memcmp` call costs more).
#[inline(always)]
pub(super) fn key_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| x == y)
}

/// Whether an instruction gets a [`PyIc`] entry.
fn caches(i: &crate::bytecode::Instr) -> bool {
    use crate::bytecode::Instr;
    matches!(
        i,
        Instr::PyGetAttr { .. }
            | Instr::PySetAttr { .. }
            | Instr::PyGlobal { .. }
            | Instr::PyMethod { .. }
            | Instr::PyClassAttr { .. }
            | Instr::PyAttrFn { .. }
            | Instr::PyIsInstance { .. }
            | Instr::PyNew { .. }
            | Instr::PyCallEntry { .. }
            | Instr::PyCall { .. }
    )
}

impl<'p> Vm<'p> {
    /// The frame table of function `func_id`, when it is a Python code
    /// object (read once per function).
    fn py_frame(&mut self, func_id: u32) -> Option<&PyFrame> {
        let f = func_id as usize;
        let proto = self.func(f);
        let p = self.py_rt.as_deref_mut()?;
        if p.frames.len() <= f {
            p.frames.resize_with(f + 1, FrameSlot::default);
        }
        if matches!(p.frames[f], FrameSlot::Unread) {
            p.frames[f] = match proto.string_constants.last().and_then(|t| PyFrame::parse(t)) {
                Some(frame) => FrameSlot::Some(Box::new(frame)),
                None => FrameSlot::None,
            };
        }
        match &p.frames[f] {
            FrameSlot::Some(frame) => Some(frame),
            _ => None,
        }
    }

    /// Whether frame `top` may be a Python code object's (the unwinder's
    /// quick test before [`Vm::py_unwind_frame`]: a function whose frame
    /// table was read and found absent is not).
    #[inline]
    pub(crate) fn py_unwind_candidate(&self, top: usize) -> bool {
        match self.py_rt.as_deref() {
            Some(p) => !matches!(p.frames.get(self.frames[top].func as usize), Some(FrameSlot::None)),
            None => false,
        }
    }

    /// The unwinder at the top frame (`vm::dispatch`'s `unwind_to_handler`),
    /// when it is a Python code object's: the ip the exception met it at is
    /// the frame's own (`exact`: it was raised by this frame's instruction)
    /// or the one before its resume point (a call it made). Entering one of
    /// the frame's handlers (`handler`), the line of that ip goes in the
    /// line register (`false`). Leaving it with no handler, the frame guard,
    /// when the ip is one it covers, is entered instead: the exception and
    /// the line in its registers, the frame resuming at the guard block
    /// (`true`).
    #[cold]
    #[inline(never)]
    pub(crate) fn py_unwind_frame(&mut self, top: usize, exact: bool, tv: Value, handler: bool) -> bool {
        let (func, base, ip) = {
            let f = &self.frames[top];
            (f.func, f.base, f.ip)
        };
        let at = if exact { ip } else { ip.saturating_sub(1) };
        let Ok(at) = u32::try_from(at) else {
            return false;
        };
        let Some(frame) = self.py_frame(func) else {
            return false;
        };
        let line = frame.line(at);
        let (line_reg, guard) = (frame.line_reg as usize, frame.guard);
        let write_line = |vm: &mut Self| {
            if let Some(l) = line {
                if base + line_reg < vm.regs.len() {
                    vm.regs[base + line_reg] = Value::int(l);
                }
            }
        };
        if handler {
            write_line(self);
            if let Some(l) = line {
                self.py_note_catch(tv, base, l);
            }
            return false;
        }
        let Some((from, start, end, ereg)) = guard else {
            return false;
        };
        if at < from || (start..end).contains(&at) || base + ereg as usize >= self.regs.len() {
            return false;
        }
        write_line(self);
        self.regs[base + ereg as usize] = tv;
        self.frames[top].ip = start as usize;
        true
    }

    /// An exception record `tv` meeting a handler of the Python frame at
    /// `base` at line `line`: that frame caught it, and CPython's traceback
    /// begins there (the frame and the line the exception passed). Recorded
    /// as the record's `tbfn` (the frame's function) and `tbcl` (the line)
    /// unless an earlier catch is still pending; `R.addframe` turns it into
    /// the frame's entry when the exception leaves that frame again (a bare
    /// `raise`, the end of a `finally` or `with`), and `rt.tracebackOf`
    /// lists it first while it is caught.
    fn py_note_catch(&mut self, tv: Value, base: usize, line: i32) {
        if !tv.is_heap() || base >= self.regs.len() {
            return;
        }
        let this = self.regs[base];
        if !this.is_heap() {
            return;
        }
        let exc_shape = self.py_rt.as_deref().map(|p| p.exc_shape);
        let idx = tv.heap_index();
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return;
        };
        if m.is_ctor {
            return;
        }
        let shape = m.shape();
        let (fn_slot, line_slot) = if shape != crate::shape::DICT && exc_shape == Some(shape) {
            (7, 8)
        } else {
            let own = |key: &str| m.pos(key).filter(|&s| !m.attr_at(s).accessor);
            match (own("tbfn"), own("tbcl")) {
                (Some(a), Some(b)) => (a, b),
                _ => return,
            }
        };
        if m.val_at(fn_slot) != Value::NULL {
            return;
        }
        self.heap.write_barrier_val(idx, this);
        if let HeapObj::Object(m) = self.heap.get_mut(idx) {
            m.set_val_at(fn_slot, this);
            m.set_val_at(line_slot, Value::int(line));
        }
    }

    /// `__zipp_py_bind(R, pins, dict)`: take the runtime's registry (`dict`
    /// a fresh dict record, for its layout). `undefined`.
    pub(crate) fn py_bind(&mut self, args: &[Value]) -> Value {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let (r, pins, dict) = (arg(0), arg(1), arg(2));
        if !self.py_plain_rec(r) || !pins.is_heap() || !matches!(self.heap.get(pins.heap_index()), HeapObj::Array(_)) {
            return Value::UNDEFINED;
        }
        if super::prof_py::on() {
            self.py_bind_publish(r);
        }
        let mut rt = PyRt::new(r);
        rt.pins = pins;
        let ri = r.heap_index();
        let own = |vm: &Self, key: &str| -> Value { vm.py_own_data(ri, key).unwrap_or(Value::UNDEFINED) };
        rt.t_list = own(self, "TLIST");
        rt.t_tuple = own(self, "TTUPLE");
        rt.t_dict = own(self, "TDICT");
        rt.t_set = own(self, "TSET");
        rt.t_str = own(self, "TSTR");
        rt.t_module = own(self, "TMODULE");
        rt.ebase = own(self, "EBASE");
        rt.builtins = own(self, "BUILTINS");
        rt.excstack = own(self, "EXCSTACK");
        rt.ctors = own(self, "CTORS");
        rt.seq_tmpl = own(self, "SEQTMPL");
        rt.exc_tmpl = own(self, "EXCTMPL");
        rt.inst_tmpl = own(self, "INSTTMPL");
        let istypes = own(self, "ISTYPES");
        if istypes.is_heap() && !self.array_js_len.contains_key(&istypes.heap_index()) {
            if let HeapObj::Array(a) = self.heap.get(istypes.heap_index()) {
                if a.len() == 6 {
                    for (i, &v) in a.iter().enumerate() {
                        rt.istypes[i] = v;
                    }
                }
            }
        }
        if let HeapObj::Object(m) = self.heap.get(ri) {
            rt.mself_slot = m.pos("mself").filter(|&s| {
                let a = m.attr_at(s);
                !a.accessor && a.writable
            });
        }
        rt.seq_shape = self.py_template_shape(rt.seq_tmpl, &SEQ_KEYS);
        rt.inst_shape = self.py_template_shape(rt.inst_tmpl, &INST_KEYS);
        rt.exc_shape = self.py_template_shape(rt.exc_tmpl, &EXC_KEYS);
        rt.dict_shape = self.py_template_shape(dict, &["cls", "map", "size"]);
        if rt.t_list.is_heap() {
            rt.ty = self.py_type_slots(rt.t_list.heap_index());
        }
        // The values above stay reachable through `pins` (held by `R`).
        let mut keep = vec![
            rt.t_list, rt.t_tuple, rt.t_dict, rt.t_set, rt.t_str, rt.t_module, rt.ebase, rt.builtins, rt.excstack,
            rt.ctors, rt.seq_tmpl, rt.exc_tmpl, rt.inst_tmpl,
        ];
        keep.extend_from_slice(&rt.istypes);
        let pi = pins.heap_index();
        for v in keep {
            if v.is_heap() {
                self.heap.write_barrier_val(pi, v);
                if let HeapObj::Array(a) = self.heap.get_mut(pi) {
                    a.push(v);
                }
            }
        }
        // Names interned before binding stay interned.
        if let Some(old) = self.py_rt.take() {
            rt.names = old.names;
            rt.layouts = old.layouts;
            // The layouts' keys stay alive through the new `pins`.
            for k in rt.layouts.keys() {
                self.heap.write_barrier_val(pi, k);
                if let HeapObj::Array(a) = self.heap.get_mut(pi) {
                    a.push(k);
                }
            }
        }
        self.py_rt = Some(Box::new(rt));
        Value::UNDEFINED
    }

    /// Whether `v` is a plain object record.
    #[inline]
    pub(super) fn py_plain_rec(&self, v: Value) -> bool {
        v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(m) if !m.is_ctor)
    }

    /// An own data property of the plain object `idx` (a scan of its keys).
    pub(super) fn py_own_data(&self, idx: u32, key: &str) -> Option<Value> {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let slot = m.pos(key)?;
        (!m.is_accessor_at(slot)).then(|| m.val_at(slot))
    }

    /// The shape of `tmpl` when its own properties are exactly `keys`, in
    /// order, all plain data properties; `shape::DICT` otherwise.
    fn py_template_shape(&self, tmpl: Value, keys: &[&str]) -> u32 {
        if !tmpl.is_heap() {
            return crate::shape::DICT;
        }
        let HeapObj::Object(m) = self.heap.get(tmpl.heap_index()) else {
            return crate::shape::DICT;
        };
        let plain = !m.is_ctor
            && m.shape_guardable()
            && m.len() == keys.len()
            && keys.iter().enumerate().all(|(i, k)| {
                let a = m.attr_at(i);
                m.key_at(i) == *k && a.writable && a.enumerable && a.configurable && !a.accessor
            });
        if plain {
            m.shape()
        } else {
            crate::shape::DICT
        }
    }

    /// A class record's field slots, from one made by `makeType`.
    fn py_type_slots(&self, t: u32) -> Option<TySlots> {
        let HeapObj::Object(m) = self.heap.get(t) else {
            return None;
        };
        let at = |k: &str| m.pos(k).filter(|&s| !m.is_accessor_at(s));
        Some(TySlots {
            mro: at("mro")?,
            is_type: at("isType")?,
            ga: at("ga")?,
            sa: at("sa")?,
            gm: at("gm")?,
            gb: at("gb")?,
            gp: at("gp")?,
            sp: at("sp")?,
            gv: at("gv")?,
            ver: at("ver")?,
        })
    }

    /// The registry, when the runtime bound one and `rt` is its `R`.
    #[inline]
    pub(super) fn py_rt_for(&self, rt: Value) -> Option<&PyRt> {
        let p = self.py_rt.as_deref()?;
        (p.r.bits() == rt.bits()).then_some(p)
    }

    /// An own data property of the plain record `idx` at the slot the VM's
    /// hint `h` remembers for `key`, else found (and remembered).
    #[inline]
    pub(super) fn py_hint_field(&self, h: usize, idx: u32, key: &str) -> Option<Value> {
        self.py_hint_slot(h, idx, key).map(|(v, _)| v)
    }

    /// [`Vm::py_hint_field`] with the slot.
    pub(super) fn py_hint_slot(&self, h: usize, idx: u32, key: &str) -> Option<(Value, usize)> {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let cell = self.py_rt.as_deref().map(|p| &p.hints[h]);
        let at = cell.map_or(usize::MAX, |c| c.get() as usize);
        let slot = if at < m.len() && key_eq(m.key_at(at), key) {
            at
        } else {
            let s = m.pos(key)?;
            if let (Some(c), Ok(s16)) = (cell, u16::try_from(s)) {
                c.set(s16);
            }
            s
        };
        (!m.is_accessor_at(slot)).then(|| (m.val_at(slot), slot))
    }

    /// An instance record's `cls` and `dict` (its first two slots when it
    /// has the instance template's shape; found by name otherwise, `dict`
    /// `undefined` when it has none).
    #[inline]
    pub(super) fn py_inst_parts(&self, o: Value) -> Option<(Value, Value)> {
        if !o.is_heap() {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(o.heap_index()) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let shape = m.shape();
        if shape != crate::shape::DICT && self.py_rt.as_deref().is_some_and(|p| p.inst_shape == shape) {
            return Some((m.val_at(0), m.val_at(1)));
        }
        let c = m.pos("cls").filter(|&s| !m.is_accessor_at(s))?;
        let d = m.pos("dict").filter(|&s| !m.is_accessor_at(s));
        Some((m.val_at(c), d.map_or(Value::UNDEFINED, |s| m.val_at(s))))
    }

    /// A record's own data `cls` (the first slot of a list, tuple, instance,
    /// exception or dict record of the usual layout).
    #[inline]
    pub(super) fn py_cls_of(&self, o: Value) -> Option<Value> {
        if !o.is_heap() {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(o.heap_index()) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let shape = m.shape();
        if shape != crate::shape::DICT {
            if let Some(p) = self.py_rt.as_deref() {
                if shape == p.inst_shape || shape == p.seq_shape || shape == p.exc_shape || shape == p.dict_shape {
                    return Some(m.val_at(0));
                }
            }
        }
        let c = m.pos("cls").filter(|&s| !m.is_accessor_at(s))?;
        Some(m.val_at(c))
    }

    /// A list or tuple record's `cls` and `items` (by name for a record of
    /// another layout).
    #[inline]
    pub(super) fn py_seq_parts(&self, o: Value) -> Option<(Value, Value)> {
        if !o.is_heap() {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(o.heap_index()) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let shape = m.shape();
        if shape != crate::shape::DICT && self.py_rt.as_deref().is_some_and(|p| p.seq_shape == shape) {
            return Some((m.val_at(0), m.val_at(1)));
        }
        let c = m.pos("cls").filter(|&s| !m.is_accessor_at(s))?;
        let i = m.pos("items").filter(|&s| !m.is_accessor_at(s))?;
        Some((m.val_at(c), m.val_at(i)))
    }

    /// A dict record's field (`which`: 0 `map`, 1 `size`) and its
    /// slot.
    #[inline]
    pub(super) fn py_dict_field(&self, idx: u32, which: usize) -> Option<(usize, Value)> {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let shape = m.shape();
        if shape != crate::shape::DICT {
            if let Some(p) = self.py_rt.as_deref() {
                if p.dict_shape == shape {
                    let s = p.dict_slots[which];
                    return Some((s, m.val_at(s)));
                }
            }
        }
        let key = ["map", "size"][which];
        let s = m.pos(key).filter(|&s| !m.is_accessor_at(s))?;
        Some((s, m.val_at(s)))
    }

    /// A class record's field `key` (`slot` its [`TySlots`] slot, checked;
    /// found by name in a record of another layout).
    #[inline]
    pub(super) fn py_ty_field(&self, c: u32, slot: usize, key: &str) -> Option<Value> {
        let HeapObj::Object(m) = self.heap.get(c) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let s = if slot < m.len() && key_eq(m.key_at(slot), key) { slot } else { m.pos(key)? };
        (!m.is_accessor_at(s)).then(|| m.val_at(s))
    }

    /// The class record `c`'s stamp `(heap version, ver bits)` for a cache
    /// entry: `c` a class record whose `ver` is at the literal's slot and
    /// holds a number.
    pub(super) fn py_cls_stamp(&self, c: Value) -> Option<(u32, u64)> {
        let ty = self.py_rt.as_deref()?.ty?;
        if !c.is_heap() {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(c.heap_index()) else {
            return None;
        };
        if m.is_ctor || ty.ver >= m.len() || m.key_at(ty.ver) != "ver" || m.is_accessor_at(ty.ver) {
            return None;
        }
        let v = m.val_at(ty.ver);
        if !v.is_number() || v.as_f64().is_nan() {
            return None;
        }
        Some((self.heap.version_of(c.heap_index()), v.bits()))
    }

    /// Whether the class `c` (whose bits matched an entry's) still has the
    /// entry's stamp.
    #[inline]
    pub(super) fn py_cls_stamp_ok(&self, c: Value, hver: u32, ver: u64) -> bool {
        let Some(ty) = self.py_rt.as_deref().and_then(|p| p.ty) else {
            return false;
        };
        let ci = c.heap_index();
        if self.heap.version_of(ci) != hver {
            return false;
        }
        // The same heap version: the record's keys are as they were when
        // the stamp was taken, `ver` at its slot.
        match self.heap.get(ci) {
            HeapObj::Object(m) => ty.ver < m.len() && m.val_at(ty.ver).bits() == ver,
            _ => false,
        }
    }

    /// This site's cache entry (kind 0 when it has none).
    #[inline]
    pub(super) fn py_ic(&self, func_id: u32, ip: usize) -> PyIc {
        let Some(p) = self.py_rt.as_deref() else {
            return PyIc::default();
        };
        let Some(Some(f)) = p.ics.get(func_id as usize) else {
            return PyIc::default();
        };
        match f.slot_of.get(ip) {
            Some(&s) if s != u16::MAX => f.ents[s as usize],
            _ => PyIc::default(),
        }
    }

    /// Record this site's cache entry.
    pub(super) fn py_ic_put(&mut self, func_id: u32, ip: usize, e: PyIc) {
        let code = &self.func(func_id as usize).code;
        let Some(p) = self.py_rt.as_deref_mut() else {
            return;
        };
        let f = func_id as usize;
        if p.ics.len() <= f {
            p.ics.resize_with(f + 1, || None);
        }
        let fic = p.ics[f].get_or_insert_with(|| {
            let mut slot_of = vec![u16::MAX; code.len()].into_boxed_slice();
            let mut n = 0usize;
            for (i, ins) in code.iter().enumerate() {
                if caches(ins) && n < u16::MAX as usize {
                    slot_of[i] = n as u16;
                    n += 1;
                }
            }
            Box::new(FnIc { slot_of, ents: vec![PyIc::default(); n].into_boxed_slice() })
        });
        if let Some(&s) = fic.slot_of.get(ip) {
            if s != u16::MAX {
                fic.ents[s as usize] = e;
            }
        }
    }

    /// The per-VM copy of a name: the first heap string made for `text` in
    /// this VM once the runtime bound itself, which every later request for
    /// the same text gets instead of `fresh`. Called by `vm::const_cache`
    /// for the string constants it memoizes, which that cache keeps alive
    /// for the VM's life: an attribute stored under a name by one function
    /// and read under it by another then compares by bits.
    pub(crate) fn py_intern(&mut self, text: &str, fresh: Value) -> Value {
        let Some(p) = self.py_rt.as_deref_mut() else {
            return fresh;
        };
        if text.len() > 64 {
            return fresh;
        }
        if let Some(&v) = p.names.get(text.as_bytes()) {
            return v;
        }
        if p.names.len() >= 1 << 16 {
            return fresh;
        }
        p.names.insert(text.as_bytes().into(), fresh);
        fresh
    }

    /// `ZIPP_PY_PROF`: name the runtime helpers by where `R` and `R.__rt`
    /// publish them.
    #[cold]
    fn py_bind_publish(&mut self, r: Value) {
        // `rt` first: a helper published on both is named by `R`.
        let mut tables = Vec::new();
        if let HeapObj::Object(m) = self.heap.get(r.heap_index()) {
            if let Some(s) = m.pos("__rt") {
                let v = m.val_at(s);
                if v.is_heap() {
                    tables.push(("rt", v.heap_index()));
                }
            }
        }
        tables.push(("R", r.heap_index()));
        let mut named = Vec::new();
        for (prefix, idx) in tables {
            let HeapObj::Object(m) = self.heap.get(idx) else {
                continue;
            };
            for (key, v, attr) in m.iter() {
                if attr.accessor || !v.is_heap() {
                    continue;
                }
                let f = match self.heap.get(v.heap_index()) {
                    HeapObj::Closure { func, .. } => *func,
                    HeapObj::Func(f) => *f,
                    _ => continue,
                };
                named.push((f, format!("{prefix}.{key}")));
            }
        }
        for (f, label) in named {
            self.py_prof_publish(f, label);
        }
    }
}
