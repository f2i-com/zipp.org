//! The plans for `codegen::py_attr_inline`: which `PyGetAttr` / `PySetAttr`
//! sites of a Python body about to compile read or write layout-mode
//! instance storage, from the interpreter's per-site caches (`vm::py_attr`).
use super::*;
use crate::codegen::PyAttrInline;

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
        let Some((cls, _)) = self.py_inst_parts(o) else {
            return;
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
    /// its cache holds a layout-mode entry whose class still has the entry's
    /// stamp; every planned fact is re-checked by the emitted guards.
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
        for (ip, instr) in proto.code.iter().enumerate() {
            let set = match instr {
                Instr::PyGetAttr { .. } => false,
                Instr::PySetAttr { .. } => true,
                _ => continue,
            };
            let e = self.py_ic(func_id, ip);
            let want = if set { super::py_rt::ic::ATTR_SET_L } else { super::py_rt::ic::ATTR_GET_L };
            if e.kind != want {
                continue;
            }
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
                    layout: e.d,
                    slot: e.pos,
                    store_lane,
                },
            );
        }
        if !plan.is_empty() && std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] PY attr plan fn{func_id}: {} inline site(s)", plan.len());
        }
        crate::codegen::set_py_attr_plan(&self.jit as *const crate::codegen::Jit as usize, func_id, plan);
    }
}
