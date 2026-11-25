//! The plans for `codegen::py_attr_inline`: which `PyGetAttr` / `PySetAttr`
//! sites of a Python body about to compile read or write layout-mode
//! instance storage, from the interpreter's per-site caches (`vm::py_attr`).
use super::*;
use crate::codegen::{
    PyAttrInline, PyCallEntryInline, PyCompilePlan, PyGlobalInline, PyIsInstanceInline,
    PyLeafInline, PyModGetInline, PyRegionExtras, PyStrmInline,
};

impl<'p> Vm<'p> {
    /// The out-of-line step of an inline attribute site
    /// (`codegen::emit_py_attr_site`): with `PY_ATTR_SETTLE` set (the class
    /// record's mirror was found cleared), first settle that mirror from the
    /// live map (a miss helper's repair, between instructions, so the map is
    /// settled; the records `ic_obj_ok` excludes keep theirs); then
    /// [`Vm::jit_py_op`].
    pub(crate) extern "win64" fn jit_py_attr_miss(vm: *mut core::ffi::c_void, regs: *const u64, packed: u64) -> u64 {
        let settle = packed & crate::codegen::PY_ATTR_SETTLE != 0;
        let packed = packed & !crate::codegen::PY_ATTR_SETTLE;
        if settle && !vm.is_null() {
            let v = unsafe { &mut *(vm as *mut Vm) };
            v.py_attr_settle_class(regs, packed);
        }
        Vm::jit_py_op(vm, regs, packed)
    }

    fn py_attr_settle_class(&mut self, regs: *const u64, packed: u64) {
        let func_id = (packed >> 32) as u32;
        let ip = packed as u32 as usize;
        if func_id as usize >= self.main_func_count.saturating_add(self.eval_funcs.len()) {
            return;
        }
        let proto = self.func(func_id as usize);
        let obj = match proto.code.get(ip) {
            Some(&Instr::PyGetAttr { obj, .. }) | Some(&Instr::PySetAttr { obj, .. }) => obj,
            _ => return,
        };
        // The window must be this VM's register file (as `jit_py_op` checks).
        let start = self.regs.as_ptr() as usize;
        let addr = regs as usize;
        if addr < start || (addr - start) % 8 != 0 {
            return;
        }
        let base = (addr - start) / 8;
        if base.saturating_add(proto.reg_count as usize) > self.regs.len() {
            return;
        }
        let o = self.get(base, obj);
        self.py_settle_class_of(o);
    }

    /// Settle the hot mirror of the record `o`'s class (see
    /// `jit_py_attr_miss`), when it is a record whose map is settled.
    pub(crate) fn py_settle_class_of(&mut self, o: Value) {
        let cls = if o.is_heap() && self.heap.is_str_like(o.heap_index()) {
            match self.py_rt.as_deref() {
                Some(p) => p.t_str,
                None => return,
            }
        } else {
            match self.py_inst_parts(o) {
                Some((cls, _)) => cls,
                None => return,
            }
        };
        if !cls.is_heap() || !self.ic_obj_ok(cls.heap_index()) {
            return;
        }
        if matches!(self.heap.get(cls.heap_index()), HeapObj::Object(m) if !m.is_ctor && m.shape() != crate::shape::DICT) {
            self.heap.refresh_mirror(cls.heap_index());
        }
    }

    /// Plan the attribute sites of `func_id` (every site of the body: an
    /// extended region reaches its out-of-line code too) and hand the plan
    /// to the JIT for the compile that follows. A site is planned only while
    /// its cache holds a layout-mode entry (or a simple `@property` getter/
    /// setter backed by one) whose class still has the entry's stamp; every
    /// planned fact is re-checked by the emitted guards.
    pub(crate) fn py_attr_inline_plan(&self, func_id: u32) {
        let Some(rt) = self.py_rt.as_deref() else {
            return;
        };
        let Some(ty) = rt.ty else {
            return;
        };
        if rt.inst_shape == crate::shape::DICT {
            return;
        }
        let Ok(ver_slot) = u32::try_from(ty.ver) else {
            return;
        };
        let proto = self.func(func_id as usize);
        let store_lane = self.heap.inline_store_lane_ok();
        let mut plan = rustc_hash::FxHashMap::default();
        let mut extras = PyRegionExtras {
            seq_shape: rt.seq_shape,
            inst_shape: rt.inst_shape,
            dict_shape: rt.dict_shape,
            dict_size_slot: rt.dict_slots[1] as u16,
            t_list_bits: rt.t_list.bits(),
            t_tuple_bits: rt.t_tuple.bits(),
            t_dict_bits: rt.t_dict.bits(),
            t_set_bits: rt.t_set.bits(),
            t_frozenset_bits: rt.t_frozenset.bits(),
            store_lane,
            dict_get_absent: rustc_hash::FxHashMap::default(),
            strm_sites: rustc_hash::FxHashMap::default(),
            leaf_sites: rustc_hash::FxHashMap::default(),
            new_fused_sites: rustc_hash::FxHashMap::default(),
            isinstance_sites: rustc_hash::FxHashMap::default(),
            global_sites: rustc_hash::FxHashMap::default(),
            mod_get_sites: rustc_hash::FxHashMap::default(),
            call_entry_sites: rustc_hash::FxHashMap::default(),
        };
        let plan_mod_or_class_val = |ip: usize, e: super::py_rt::PyIc, extras: &mut PyRegionExtras| -> bool {
            let Ok(ip_u32) = u32::try_from(ip) else {
                return false;
            };
            match e.kind {
                super::py_rt::ic::MOD_GET => {
                    let mod_shape = rt.mod_shape.get();
                    let g = Value::from_bits(e.a);
                    if mod_shape != crate::shape::DICT
                        && rt.t_module.is_heap()
                        && g.is_heap()
                        && matches!(self.heap.get(g.heap_index()), HeapObj::Map { .. })
                    {
                        let gi = g.heap_index();
                        if self.heap.version_of(gi) == e.hver {
                            if let (Some(val), Some(g_ver_off)) = (
                                self.py_map_at_bits(g, e.pos, e.b, e.d),
                                (gi as usize).checked_mul(4).and_then(|n| i32::try_from(n).ok()),
                            ) {
                                extras.mod_get_sites.insert(
                                    ip_u32,
                                    PyModGetInline::Module {
                                        mod_shape,
                                        t_module_bits: rt.t_module.bits(),
                                        globals_slot: rt.mod_globals_slot.get(),
                                        g_bits: e.a,
                                        g_ver_off,
                                        g_ver: e.hver,
                                        val_bits: val.bits(),
                                    },
                                );
                                return true;
                            }
                        }
                    }
                    false
                }
                super::py_rt::ic::TYPE_CALL_ATTR => {
                    let cls = Value::from_bits(e.a);
                    if cls.is_heap() && self.py_cls_stamp_ok(cls, e.hver, e.c) {
                        if let HeapObj::Object(m) = self.heap.get(cls.heap_index()) {
                            if m.shape() != crate::shape::DICT {
                                extras.mod_get_sites.insert(
                                    ip_u32,
                                    PyModGetInline::TypeCall {
                                        cls_bits: e.a,
                                        cls_shape: m.shape(),
                                        ver_slot,
                                        ver_bits: e.c,
                                        hver: e.hver,
                                        val_bits: e.b,
                                    },
                                );
                                return true;
                            }
                        }
                    }
                    false
                }
                super::py_rt::ic::CLASS_METHOD => {
                    let cls = Value::from_bits(e.a);
                    if cls.is_heap() && self.py_cls_stamp_ok(cls, e.hver, e.c) {
                        if let HeapObj::Object(m) = self.heap.get(cls.heap_index()) {
                            if m.shape() != crate::shape::DICT {
                                extras.mod_get_sites.insert(
                                    ip_u32,
                                    PyModGetInline::ClassVal {
                                        inst_shape: rt.inst_shape,
                                        cls_bits: e.a,
                                        cls_shape: m.shape(),
                                        ver_slot,
                                        ver_bits: e.c,
                                        hver: e.hver,
                                        layout: e.d,
                                        val_bits: e.b,
                                    },
                                );
                                return true;
                            }
                        }
                    }
                    false
                }
                super::py_rt::ic::GET_CLASS_L => {
                    let cls = Value::from_bits(e.a);
                    if cls.is_heap() && self.py_cls_stamp_ok(cls, e.hver, e.c) {
                        if let Some(val) = self.py_table_at(cls, ty.gv, e.b, e.pos) {
                            if !val.is_undefined() && val != Value::HOLE {
                                if let HeapObj::Object(m) = self.heap.get(cls.heap_index()) {
                                    if m.shape() != crate::shape::DICT {
                                        extras.mod_get_sites.insert(
                                            ip_u32,
                                            PyModGetInline::ClassVal {
                                                inst_shape: rt.inst_shape,
                                                cls_bits: e.a,
                                                cls_shape: m.shape(),
                                                ver_slot,
                                                ver_bits: e.c,
                                                hver: e.hver,
                                                layout: e.d,
                                                val_bits: val.bits(),
                                            },
                                        );
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                    false
                }
                _ => false,
            }
        };
        for (ip, instr) in proto.code.iter().enumerate() {
            let set = match instr {
                Instr::PyGetAttr { .. } => {
                    if !super::prof_py::on() {
                        let e = self.py_ic(func_id, ip);
                        if matches!(e.kind, super::py_rt::ic::MOD_GET | super::py_rt::ic::GET_CLASS_L)
                            && plan_mod_or_class_val(ip, e, &mut extras)
                        {
                            continue;
                        }
                    }
                    false
                }
                Instr::PyModGet { .. } if !super::prof_py::on() => {
                    let e = self.py_ic(func_id, ip);
                    if plan_mod_or_class_val(ip, e, &mut extras) {
                        continue;
                    }
                    if e.kind == super::py_rt::ic::ATTR_GET_L {
                        false
                    } else {
                        continue;
                    }
                }
                Instr::PyCallEntry { .. } if !super::prof_py::on() => {
                    let e = self.py_ic(func_id, ip);
                    let f = Value::from_bits(e.a);
                    if matches!(
                        e.kind,
                        super::py_rt::ic::CALL_ENTRY
                            | super::py_rt::ic::CALL_PLAIN..=super::py_rt::ic::CALL_REC
                            | super::py_rt::ic::CALL_EXC
                            | super::py_rt::ic::CALL_GEN
                    ) && f.is_heap()
                        && self.heap.version_of(f.heap_index()) == e.hver
                    {
                        if let HeapObj::Object(m) = self.heap.get(f.heap_index()) {
                            let pos = e.pos as usize;
                            if !m.is_ctor
                                && m.shape() != crate::shape::DICT
                                && pos < m.slot_len()
                                && !m.is_accessor_at(pos)
                            {
                                if let Ok(ip_u32) = u32::try_from(ip) {
                                    extras.call_entry_sites.insert(
                                        ip_u32,
                                        PyCallEntryInline {
                                            f_shape: m.shape(),
                                            pos: e.pos,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    continue;
                }
                Instr::PySetAttr { .. } => true,
                Instr::PyGlobal { .. } if !super::prof_py::on() => {
                    let e = self.py_ic(func_id, ip);
                    let g = Value::from_bits(e.a);
                    if (e.kind == super::py_rt::ic::GLOBAL || e.kind == super::py_rt::ic::BUILTIN)
                        && g.is_heap()
                        && matches!(self.heap.get(g.heap_index()), HeapObj::Map { .. })
                    {
                        let gi = g.heap_index();
                        let g_ver = self.heap.version_of(gi);
                        if g_ver == e.hver {
                            let resolved = if e.kind == super::py_rt::ic::GLOBAL {
                                self.py_map_at_bits(g, e.pos, e.b, e.d).map(|v| (v, -1i32, 0u32))
                            } else {
                                let absent = matches!(self.heap.get(gi), HeapObj::Map { keys, .. } if keys.len() as u64 == e.c);
                                let b = rt.builtins;
                                if absent && b.is_heap() {
                                    let bi = b.heap_index();
                                    let b_ver_off = (bi as usize).checked_mul(4).and_then(|n| i32::try_from(n).ok());
                                    b_ver_off.and_then(|bo| {
                                        self.py_map_at_bits(b, e.pos, e.b, e.d)
                                            .map(|v| (v, bo, self.heap.version_of(bi)))
                                    })
                                } else {
                                    None
                                }
                            };
                            if let (Some((val, b_ver_off, b_ver)), Ok(ip_u32), Some(g_ver_off)) = (
                                resolved,
                                u32::try_from(ip),
                                (gi as usize).checked_mul(4).and_then(|n| i32::try_from(n).ok()),
                            ) {
                                let seen = rt.func_shapes.get();
                                let mut func_shapes = [crate::shape::DICT; 2];
                                let mut n_sh = 0usize;
                                for s in seen {
                                    if s != crate::shape::DICT && n_sh < 2 {
                                        func_shapes[n_sh] = s;
                                        n_sh += 1;
                                    }
                                }
                                extras.global_sites.insert(
                                    ip_u32,
                                    PyGlobalInline {
                                        g_bits: e.a,
                                        g_ver_off,
                                        g_ver,
                                        b_ver_off,
                                        b_ver,
                                        rt_bits: rt.r.bits(),
                                        val_bits: val.bits(),
                                        func_shapes,
                                        func_globals_slot: rt.func_globals_slot.get(),
                                    },
                                );
                            }
                        }
                    }
                    continue;
                }
                Instr::PyIsInstance { .. } if !super::prof_py::on() => {
                    let e = self.py_ic(func_id, ip);
                    let t = Value::from_bits(e.a);
                    if e.kind == super::py_rt::ic::IS_TYPE && t.is_heap() {
                        let ti = t.heap_index();
                        if self.heap.version_of(ti) == e.hver {
                            if let (Ok(ip_u32), Some(t_ver_off)) = (
                                u32::try_from(ip),
                                (ti as usize).checked_mul(4).and_then(|n| i32::try_from(n).ok()),
                            ) {
                                let tb = e.a;
                                let (kind, recv_shape) = if tb == rt.istypes[0].bits() {
                                    (0u8, crate::shape::DICT)
                                } else if tb == rt.istypes[1].bits() {
                                    (1, crate::shape::DICT)
                                } else if tb == rt.istypes[2].bits() {
                                    (2, crate::shape::DICT)
                                } else if tb == rt.istypes[3].bits() {
                                    (3, crate::shape::DICT)
                                } else if tb == rt.istypes[4].bits() {
                                    (4, crate::shape::DICT)
                                } else if tb != rt.istypes[5].bits() {
                                    let rs = if tb == rt.t_list.bits() || tb == rt.t_tuple.bits() {
                                        rt.seq_shape
                                    } else if tb == rt.t_dict.bits() || tb == rt.t_set.bits() || tb == rt.t_frozenset.bits() {
                                        rt.dict_shape
                                    } else {
                                        rt.inst_shape
                                    };
                                    (5, rs)
                                } else {
                                    (u8::MAX, crate::shape::DICT)
                                };
                                if kind <= 4 || (kind == 5 && recv_shape != crate::shape::DICT) {
                                    extras.isinstance_sites.insert(
                                        ip_u32,
                                        PyIsInstanceInline {
                                            t_bits: tb,
                                            t_ver_off,
                                            hver: e.hver,
                                            kind,
                                            recv_shape,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    continue;
                }
                Instr::PyDictGet { .. } if !super::prof_py::on() => {
                    let e = self.py_ic(func_id, ip);
                    if e.kind == super::py_rt::ic::DICT_GET_ABSENT && e.d != 0 {
                        if let Ok(ip_u32) = u32::try_from(ip) {
                            extras.dict_get_absent.insert(ip_u32, e.d);
                        }
                    }
                    continue;
                }
                #[cfg(not(feature = "safe-sandbox"))]
                Instr::PyNew { dst, this_f, n, .. } if *n > 0 && !super::prof_py::on() => {
                    let n = *n as usize;
                    let matched_call = matches!(
                        proto.code.get(ip + 1),
                        Some(&Instr::PyCall { f, arg_base, argc, .. })
                            if f == *this_f && arg_base == *dst && argc as usize == n + 1
                    );
                    if matched_call && rt.inst_tmpl_hver != u32::MAX && rt.inst_tmpl.is_heap() {
                        let e = self.py_ic(func_id, ip);
                        let cls = Value::from_bits(e.a);
                        if e.kind == super::py_rt::ic::NEW && cls.is_heap() {
                            let ci = cls.heap_index();
                            if self.heap.version_of(ci) == e.hver {
                                let want = match rt.ctors.is_heap().then(|| self.heap.get(rt.ctors.heap_index())) {
                                    Some(HeapObj::Array(items)) => items.get(n).copied(),
                                    _ => None,
                                };
                                let (pos, islot) = (e.pos as usize, e.d as usize);
                                let ver_s = ty.ver;
                                if let (Some(want), HeapObj::Object(m)) = (want, self.heap.get(ci)) {
                                    if !m.is_ctor
                                        && pos < m.slot_len()
                                        && islot < m.slot_len()
                                        && ver_s < m.slot_len()
                                        && !m.is_accessor_at(pos)
                                        && !m.is_accessor_at(islot)
                                        && !m.is_accessor_at(ver_s)
                                        && want.is_heap()
                                        && m.val_at(pos).bits() == want.bits()
                                        && m.val_at(islot).bits() == e.b
                                    {
                                        let cls_ver = m.val_at(ver_s).bits();
                                        let init = Value::from_bits(e.b);
                                        let init_hver = e.c as u32;
                                        let init_entry_slot = (e.c >> 32) as u16;
                                        if init.is_heap() && self.heap.version_of(init.heap_index()) == init_hver {
                                            if let HeapObj::Object(im) = self.heap.get(init.heap_index()) {
                                                let s = init_entry_slot as usize;
                                                if !im.is_ctor && s < im.slot_len() && !im.is_accessor_at(s) {
                                                    let entry = im.val_at(s);
                                                    let init_fid = entry.is_heap().then(|| match self.heap.get(entry.heap_index()) {
                                                        HeapObj::Func(id) => Some(*id),
                                                        HeapObj::Closure { func, .. } => Some(*func),
                                                        _ => None,
                                                    }).flatten();
                                                    if let Some(init_fid) = init_fid {
                                                        if let Some(sp) = rt.store_plans.get(init_fid as usize).copied().flatten() {
                                                            let count = sp.count as usize;
                                                            let args_ok = count > 0
                                                                && count <= 8
                                                                && sp.arg_idx[..count].iter().all(|&a| a >= 1 && a as usize <= n);
                                                            if sp.cls_bits == e.a
                                                                && sp.cls_hver == e.hver
                                                                && sp.cls_ver == cls_ver
                                                                && self.func(init_fid as usize).param_count as usize == n + 1
                                                                && args_ok
                                                            {
                                                                let plan_idx = if rt.inst_plan_idx != u16::MAX {
                                                                    Some(rt.inst_plan_idx as usize)
                                                                } else {
                                                                    match self.heap.get(rt.inst_tmpl.heap_index()) {
                                                                        HeapObj::Object(tm) => match tm.prop_keys() {
                                                                            crate::heap::PropKeys::Planned { all, .. } => {
                                                                                rt.plans.iter().position(|k| all.is(k))
                                                                            }
                                                                            _ => None,
                                                                        },
                                                                        _ => None,
                                                                    }
                                                                };
                                                                if let Some(plan_idx) = plan_idx {
                                                                    if let (Ok(ip_u32), Ok(ctor_slot), Ok(init_slot), Ok(ver_slot16), Ok(plan_idx16)) = (
                                                                        u32::try_from(ip),
                                                                        u16::try_from(pos),
                                                                        u16::try_from(islot),
                                                                        u16::try_from(ver_s),
                                                                        u16::try_from(plan_idx),
                                                                    ) {
                                                                        let mut plans = rt.new_fused_plans.borrow_mut();
                                                                        if let Ok(pid) = u32::try_from(plans.len()) {
                                                                            plans.push(super::py_rt::NewFusedPlan {
                                                                                cls_bits: e.a,
                                                                                cls_hver: e.hver,
                                                                                cls_ver,
                                                                                ver_slot: ver_slot16,
                                                                                ctor_slot,
                                                                                want_bits: want.bits(),
                                                                                init_slot,
                                                                                init_bits: e.b,
                                                                                init_hver,
                                                                                init_entry_slot,
                                                                                entry_bits: entry.bits(),
                                                                                inst_tmpl: rt.inst_tmpl,
                                                                                inst_tmpl_hver: rt.inst_tmpl_hver,
                                                                                inst_shape: rt.inst_shape,
                                                                                plan_idx: plan_idx16,
                                                                                final_layout: sp.final_layout,
                                                                                count: sp.count,
                                                                                arg_idx: sp.arg_idx,
                                                                            });
                                                                            extras.new_fused_sites.insert(ip_u32, pid);
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }
                Instr::PyCall { argc, .. } if !super::prof_py::on() => {
                    let e = self.py_ic(func_id, ip);
                    let f = Value::from_bits(e.a);
                    if (1..=8).contains(argc) {
                        if let Some((strop_slot, op)) = self.py_strm_hit(&e, f) {
                            if !matches!(op, 52..=55) {
                                if let HeapObj::Object(m) = self.heap.get(f.heap_index()) {
                                    let f_shape = m.shape();
                                    let pos = e.pos as usize;
                                    if f_shape != crate::shape::DICT && pos < m.slot_len() {
                                        if let (Ok(ip_u32), Ok(strop_u32)) = (u32::try_from(ip), u32::try_from(strop_slot)) {
                                            extras.strm_sites.insert(
                                                ip_u32,
                                                PyStrmInline {
                                                    f_bits: e.a,
                                                    f_shape,
                                                    hver: e.hver,
                                                    entry_slot: e.pos,
                                                    entry_bits: m.val_at(pos).bits(),
                                                    strop_slot: strop_u32,
                                                    op,
                                                },
                                            );
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                    }
                    if e.kind == super::py_rt::ic::CALL_LEAF && e.c != 0 && f.is_heap() && *argc <= 12 {
                        let fid = e.c as u32;
                        let params = (e.c >> 48) as u16;
                        if *argc <= params {
                            if let Some(super::py_attr::py_leaf::LeafSlot::Yes(info)) = rt.leaf_bodies.get(fid as usize).copied() {
                                let fi = f.heap_index();
                                if self.heap.version_of(fi) == e.hver {
                                    if let HeapObj::Object(m) = self.heap.get(fi) {
                                        let f_shape = m.shape();
                                        let pos = e.pos as usize;
                                        if f_shape != crate::shape::DICT
                                            && pos < m.slot_len()
                                            && !m.is_accessor_at(pos)
                                            && m.val_at(pos).bits() == e.b
                                        {
                                            if let Ok(ip_u32) = u32::try_from(ip) {
                                                extras.leaf_sites.insert(
                                                    ip_u32,
                                                    PyLeafInline {
                                                        f_bits: e.a,
                                                        f_shape,
                                                        hver: e.hver,
                                                        entry_slot: e.pos,
                                                        entry_bits: e.b,
                                                        fid,
                                                        main_end: info.main_end,
                                                        reg_slots: info.reg_slots,
                                                        params: info.params,
                                                    },
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }
                Instr::PyMethod { .. } if crate::codegen::py_method_inline_enabled() => {
                    // A method found in the receiver's class's tables
                    // (`Vm::py_method_plan`), answered inline while the
                    // receiver, its class's stamp and its layout hold.
                    let e = self.py_ic(func_id, ip);
                    let Some((recv_shape, method, layout)) = self.py_method_plan(&e) else {
                        continue;
                    };
                    let cls_shape = match self.heap.get(Value::from_bits(e.a).heap_index()) {
                        HeapObj::Object(m) if m.shape() != crate::shape::DICT => m.shape(),
                        _ => continue,
                    };
                    let Ok(ip) = u32::try_from(ip) else {
                        continue;
                    };
                    plan.insert(
                        ip,
                        PyAttrInline {
                            set: false,
                            inst_shape: recv_shape,
                            cls_bits: e.a,
                            cls_shape,
                            ver_slot,
                            ver_bits: e.c,
                            hver: e.hver,
                            layout,
                            slot: 0,
                            store_lane: false,
                            method: method.bits(),
                        },
                    );
                    continue;
                }
                _ => continue,
            };
            let e = self.py_ic(func_id, ip);
            let want = if set { super::py_rt::ic::ATTR_SET_L } else { super::py_rt::ic::ATTR_GET_L };
            let (layout, slot) = if e.kind == want {
                (e.d, e.pos)
            } else if e.kind == super::py_rt::ic::ATTR_PROP {
                let cls = Value::from_bits(e.a);
                if !cls.is_heap() || !self.py_cls_stamp_ok(cls, e.hver, e.c) {
                    continue;
                }
                let Some(ls) = self.py_prop_inline_plan(cls, set, &e) else {
                    continue;
                };
                ls
            } else {
                continue;
            };
            let cls = Value::from_bits(e.a);
            if !cls.is_heap() || !self.py_cls_stamp_ok(cls, e.hver, e.c) {
                continue;
            }
            let cls_shape = match self.heap.get(cls.heap_index()) {
                HeapObj::Object(m) if m.shape() != crate::shape::DICT => m.shape(),
                _ => continue,
            };
            let Ok(ip) = u32::try_from(ip) else {
                continue;
            };
            plan.insert(
                ip,
                PyAttrInline {
                    set,
                    inst_shape: rt.inst_shape,
                    cls_bits: e.a,
                    cls_shape,
                    ver_slot,
                    ver_bits: e.c,
                    hver: e.hver,
                    layout,
                    slot,
                    store_lane,
                    method: 0,
                },
            );
        }
        if !plan.is_empty() && crate::jitlog_enabled() {
            eprintln!("[jit] PY attr plan fn{func_id}: {} inline site(s)", plan.len());
        }
        crate::codegen::set_py_attr_plan(
            &self.jit as *const crate::codegen::Jit as usize,
            func_id,
            PyCompilePlan { attrs: plan, extras },
        );
    }
}
