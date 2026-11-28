//! Inline attribute reads and writes on a Python instance's layout-mode
//! storage (stage S3, `vm::py_table::layout`), for the compiled Python
//! loops of `codegen::py`.
//!
//! `PyGetAttr` / `PySetAttr` sites whose interpreter cache (`vm::py_attr`)
//! holds a layout-mode entry when the loop compiles get a plan
//! ([`PyAttrInline`], built by `Vm::py_attr_inline_plan`): the class's bits
//! and stamp, the layout and the slot. The emitted code then follows the
//! layout module's JIT contract through the heap's hot mirror:
//!
//! 1. the object is a heap value whose mirror shape is the instance
//!    template's (`PyRt::inst_shape`), so `vals[0]` is its `cls` and
//!    `vals[1]` its `dict`;
//! 2. `cls` has the planned bits and still the planned stamp: its mirror
//!    shape is the planned one, its `ver` slot holds the planned bits and its
//!    heap version is the planned one (exactly `Vm::py_cls_stamp_ok`);
//! 3. `dict`'s mirror shape is `LAYOUT_BASE | layout`, so its mirror `vals`
//!    points at the layout's values;
//! 4. read (or write) `vals[slot]`.
//!
//! A write stores only when it needs no write barrier: the value is not a
//! heap reference, or the storage is young (the heap's generation bytes,
//! read only when `Heap::inline_store_lane_ok`, as the inline dense-array
//! store lane does). Any failed guard runs the instruction's ordinary step,
//! which is what the site compiled to before, so the answer is always the
//! interpreter's.
//!
//! A class record's hot mirror is cleared by every change to its keys (the
//! heap's `bump_version`), and only a repair re-settles it. So when step 2
//! finds the class's mirror cleared, the step runs through
//! `Vm::jit_py_attr_miss` with [`PY_ATTR_SETTLE`] set, which settles the
//! mirror first; every other failed guard (a stale plan included) runs the
//! plain step, so a site whose plan no longer holds costs only its guards.
use super::*;

/// One site's plan (see the module comment).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PyAttrInline {
    pub set: bool,
    /// The receiver's shape: `PyRt::inst_shape` (an attribute site, a user
    /// class's method), or a builtin container's record shape (a builtin
    /// method site).
    pub inst_shape: u32,
    pub cls_bits: u64,
    /// The class record's shape when planned.
    pub cls_shape: u32,
    /// The class record's `ver` slot and its bits (the stamp's `ver`).
    pub ver_slot: u32,
    pub ver_bits: u64,
    /// The class record's heap version (the stamp's `hver`).
    pub hver: u32,
    /// The storage's layout and the attribute's slot in it. For a method
    /// site: the layout proven to lack the method's name (a user class's
    /// instance), 0 for a dict-less builtin container.
    pub layout: u32,
    pub slot: u32,
    /// A store may use the generation bytes (see the module comment).
    pub store_lane: bool,
    /// A `PyMethod` site's answer: the method record the class's table held
    /// for the name when planned (0: an attribute site). See
    /// [`emit_py_method_inline`].
    pub method: u64,
}

/// Metadata about the Python runtime's sequence/instance shapes and planned
/// inline `PyDictGet` / `__zipp_py_strm` / `PyLeaf` / fused `PyNew` sites,
/// passed alongside the attribute plan for one region compile.
#[derive(Clone, Debug, Default)]
pub(crate) struct PyRegionExtras {
    pub seq_shape: u32,
    pub inst_shape: u32,
    pub dict_shape: u32,
    pub dict_size_slot: u16,
    pub t_list_bits: u64,
    pub t_tuple_bits: u64,
    pub t_dict_bits: u64,
    pub t_set_bits: u64,
    pub t_frozenset_bits: u64,
    pub store_lane: bool,
    pub dict_get_absent: FxHashMap<u32, u32>,
    pub strm_sites: FxHashMap<u32, PyStrmInline>,
    pub leaf_sites: FxHashMap<u32, PyLeafInline>,
    pub new_fused_sites: FxHashMap<u32, u32>,
    pub isinstance_sites: FxHashMap<u32, PyIsInstanceInline>,
    pub global_sites: FxHashMap<u32, PyGlobalInline>,
    pub mod_get_sites: FxHashMap<u32, PyModGetInline>,
    pub call_entry_sites: FxHashMap<u32, PyCallEntryInline>,
}

/// Plan for an inline-guarded `PyModGet` (or `PyGetAttr` for `GET_CLASS_L` /
/// `MOD_GET`): checks module/class/instance guards and stores the proven
/// `val_bits` directly with zero FFI.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PyModGetInline {
    Module {
        mod_shape: u32,
        t_module_bits: u64,
        globals_slot: u16,
        g_bits: u64,
        g_ver_off: i32,
        g_ver: u32,
        val_bits: u64,
    },
    TypeCall {
        cls_bits: u64,
        cls_shape: u32,
        ver_slot: u32,
        ver_bits: u64,
        hver: u32,
        val_bits: u64,
    },
    ClassVal {
        inst_shape: u32,
        cls_bits: u64,
        cls_shape: u32,
        ver_slot: u32,
        ver_bits: u64,
        hver: u32,
        layout: u32,
        val_bits: u64,
    },
}

/// Plan for an inline-guarded `PyCallEntry`: guards receiver `f` has `f_shape`
/// (polymorphic across all function records sharing that shape!), reads slot
/// `pos` afresh, verifies it is a `Func`/`Closure` (`shape == DICT && fid < FID_MIRROR_STR`),
/// and stores it to `dst` with zero FFI.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PyCallEntryInline {
    pub f_shape: u32,
    pub pos: u32,
}

/// Plan for an inline-guarded `PyGlobal` site (`ic::GLOBAL` or `ic::BUILTIN`):
/// guards `globals == g_bits` (or lazy function record `globals.globals == g_bits`),
/// `versions[g_idx] == g_ver` (and `rt == rt_bits` + `versions[b_idx] == b_ver` for
/// `BUILTIN`), then stores the proven `val_bits` directly with ZERO FFI.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PyGlobalInline {
    pub g_bits: u64,
    pub g_ver_off: i32,
    pub g_ver: u32,
    /// Non-negative byte offset (`b_idx * 4`) when `ic::BUILTIN`; `-1` for `ic::GLOBAL`.
    pub b_ver_off: i32,
    pub b_ver: u32,
    pub rt_bits: u64,
    pub val_bits: u64,
    /// Up to 2 function-record shapes that store `.globals` at `func_globals_slot`
    /// (lazy-globals calling convention). `shape::DICT` means unused.
    pub func_shapes: [u32; 2],
    pub func_globals_slot: u16,
}

/// Plan for an inline-guarded `PyIsInstance` site (`ic::IS_TYPE`): checks
/// `t == t_bits` + `versions[t_idx] == hver`, then settles builtin primitive
/// types (`NoneType`, `bool`, `int`, `float`, `str`) or exact heap class
/// records (`kind == 5`) without an FFI call.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PyIsInstanceInline {
    pub t_bits: u64,
    pub t_ver_off: i32,
    pub hver: u32,
    /// `0=NoneType`, `1=bool`, `2=int`, `3=float`, `4=str`, `5=exact heap class`.
    pub kind: u8,
    /// Expected receiver hot-mirror shape for `kind == 5`.
    pub recv_shape: u32,
}

/// Plan for an inline-guarded `__zipp_py_strm` `PyCall` site (`op == 11` gets
/// inline `sqrtsd`; other ops dispatch directly to a thin Rust helper without
/// `py_ic` / `py_strm_hit` / `regs` validation).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PyStrmInline {
    pub f_bits: u64,
    pub f_shape: u32,
    pub hver: u32,
    pub entry_slot: u32,
    pub entry_bits: u64,
    pub strop_slot: u32,
    pub op: i32,
}

/// Plan for an inline-guarded frameless Python leaf `PyCall` site (`ic::CALL_LEAF`):
/// skips `py_ic`, `regs` validation, `py_leaf_info`, and `refetch(ops)`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PyLeafInline {
    pub f_bits: u64,
    pub f_shape: u32,
    pub hver: u32,
    pub entry_slot: u32,
    pub entry_bits: u64,
    pub fid: u32,
    pub main_end: u16,
    pub reg_slots: u8,
    pub params: u8,
}

/// Combined plan for one `compile_py_region` call.
pub(crate) struct PyCompilePlan {
    pub attrs: FxHashMap<u32, PyAttrInline>,
    pub extras: PyRegionExtras,
}

thread_local! {
    /// The plan `Vm::py_attr_inline_plan` made for the compile that follows
    /// it: the planning JIT's address, the function and its sites. Kept out
    /// of `Jit` (and so out of `Vm`) on purpose; any compile takes it, and
    /// only the same JIT's compile of the same function uses it.
    static PENDING: std::cell::RefCell<Option<(usize, u32, PyCompilePlan)>> = const { std::cell::RefCell::new(None) };
    /// The plan of the body being compiled, for `emit_py_op` (set only for
    /// the duration of one `compile_py_region`).
    static PLAN: std::cell::RefCell<Option<PyCompilePlan>> = const { std::cell::RefCell::new(None) };
}

/// Hand the compile that follows the plan for `func_id` of the JIT at
/// `jit` (replacing any plan not taken).
pub(crate) fn set_py_attr_plan(jit: usize, func_id: u32, plan: PyCompilePlan) {
    let useful = !plan.attrs.is_empty()
        || plan.extras.seq_shape != crate::shape::DICT
        || plan.extras.inst_shape != crate::shape::DICT
        || plan.extras.dict_shape != crate::shape::DICT
        || !plan.extras.dict_get_absent.is_empty()
        || !plan.extras.strm_sites.is_empty()
        || !plan.extras.leaf_sites.is_empty()
        || !plan.extras.new_fused_sites.is_empty()
        || !plan.extras.isinstance_sites.is_empty()
        || !plan.extras.global_sites.is_empty()
        || !plan.extras.mod_get_sites.is_empty()
        || !plan.extras.call_entry_sites.is_empty();
    PENDING.with(|p| *p.borrow_mut() = useful.then_some((jit, func_id, plan)));
}

/// Take the pending plan: `Some` when it is for `func_id` of the JIT at
/// `jit`.
pub(crate) fn take_py_attr_plan(jit: usize, func_id: u32) -> Option<PyCompilePlan> {
    PENDING
        .with(|p| p.borrow_mut().take())
        .and_then(|(j, f, plan)| (j == jit && f == func_id).then_some(plan))
}

/// Run `f` with `plan` as the current body's plan.
pub(crate) fn with_py_attr_plan<R>(plan: Option<PyCompilePlan>, f: impl FnOnce() -> R) -> R {
    PLAN.with(|p| *p.borrow_mut() = plan);
    let r = f();
    PLAN.with(|p| *p.borrow_mut() = None);
    r
}

fn plan_at(ip: usize) -> Option<PyAttrInline> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| p.borrow().as_ref().and_then(|m| m.attrs.get(&ip).copied()))
}

pub(super) fn seq_plan() -> Option<(u32, u64, u64, bool)> {
    PLAN.with(|p| {
        p.borrow().as_ref().and_then(|m| {
            let e = &m.extras;
            (e.seq_shape != crate::shape::DICT).then_some((e.seq_shape, e.t_list_bits, e.t_tuple_bits, e.store_lane))
        })
    })
}

pub(super) fn dict_plan() -> Option<(u32, u16, u64, u64, u64)> {
    PLAN.with(|p| {
        p.borrow().as_ref().and_then(|m| {
            let e = &m.extras;
            (e.dict_shape != crate::shape::DICT).then_some((
                e.dict_shape,
                e.dict_size_slot,
                e.t_dict_bits,
                e.t_set_bits,
                e.t_frozenset_bits,
            ))
        })
    })
}

pub(super) fn inst_shape_plan() -> Option<u32> {
    PLAN.with(|p| {
        p.borrow().as_ref().and_then(|m| {
            let s = m.extras.inst_shape;
            (s != crate::shape::DICT).then_some(s)
        })
    })
}

pub(super) fn dict_get_absent_plan_at(ip: usize) -> Option<(u32, u32)> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| {
        p.borrow().as_ref().and_then(|m| {
            let inst_shape = m.extras.inst_shape;
            (inst_shape != crate::shape::DICT)
                .then(|| m.extras.dict_get_absent.get(&ip).copied().map(|l| (inst_shape, l)))
                .flatten()
        })
    })
}

pub(super) fn strm_plan_at(ip: usize) -> Option<PyStrmInline> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| p.borrow().as_ref().and_then(|m| m.extras.strm_sites.get(&ip).copied()))
}

pub(super) fn leaf_plan_at(ip: usize) -> Option<PyLeafInline> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| p.borrow().as_ref().and_then(|m| m.extras.leaf_sites.get(&ip).copied()))
}

pub(super) fn new_fused_plan_at(ip: usize) -> Option<u32> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| p.borrow().as_ref().and_then(|m| m.extras.new_fused_sites.get(&ip).copied()))
}

pub(super) fn isinstance_plan_at(ip: usize) -> Option<PyIsInstanceInline> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| p.borrow().as_ref().and_then(|m| m.extras.isinstance_sites.get(&ip).copied()))
}

pub(super) fn global_plan_at(ip: usize) -> Option<PyGlobalInline> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| p.borrow().as_ref().and_then(|m| m.extras.global_sites.get(&ip).copied()))
}

pub(super) fn mod_get_plan_at(ip: usize) -> Option<PyModGetInline> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| p.borrow().as_ref().and_then(|m| m.extras.mod_get_sites.get(&ip).copied()))
}

pub(super) fn call_entry_plan_at(ip: usize) -> Option<PyCallEntryInline> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| p.borrow().as_ref().and_then(|m| m.extras.call_entry_sites.get(&ip).copied()))
}

/// Emit the inline x86-64 fast path for `Instr::PyModGet` (and `PyGetAttr`
/// when `GET_CLASS_L` / `MOD_GET`): zero FFI and zero `refetch` on hit.
pub(super) fn emit_py_mod_get_inline(
    ops: &mut crate::codegen::X64Asm,
    ip: usize,
    dst: u16,
    obj: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{JIT_HOT_VALS_OFF, JIT_VERSIONS_RAW_OFFSET};
    let Some(p) = mod_get_plan_at(ip) else {
        return false;
    };
    let vals_off = JIT_HOT_VALS_OFF as i32;
    match p {
        PyModGetInline::Module {
            mod_shape,
            t_module_bits,
            globals_slot,
            g_bits,
            g_ver_off,
            g_ver,
            val_bits,
        } => {
            let gslot_off = (globals_slot as i32) * 8;
            dynasm!(ops ; mov rax, [rbx + dreg(obj)]);
            mirror_of_rax(ops, miss);
            dynasm!(ops
                ; cmp DWORD [r9 + r10 * 8], mod_shape as i32
                ; jne => miss
                ; mov r8, [r9 + r10 * 8 + vals_off]
                ; mov rax, [r8]
                ; mov rcx, QWORD t_module_bits as i64
                ; cmp rax, rcx
                ; jne => miss
                ; mov rax, [r8 + gslot_off]
                ; mov rcx, QWORD g_bits as i64
                ; cmp rax, rcx
                ; jne => miss
                ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
                ; cmp DWORD [rcx + g_ver_off], g_ver as i32
                ; jne => miss
                ; mov rax, QWORD val_bits as i64
                ; mov [rbx + dreg(dst)], rax
                ; jmp => done
            );
        }
        PyModGetInline::TypeCall {
            cls_bits,
            cls_shape,
            ver_slot,
            ver_bits,
            hver,
            val_bits,
        } => {
            let ver_off = (ver_slot as i32) * 8;
            dynasm!(ops
                ; mov rax, [rbx + dreg(obj)]
                ; mov rcx, QWORD cls_bits as i64
                ; cmp rax, rcx
                ; jne => miss
            );
            mirror_of_rax(ops, miss);
            dynasm!(ops
                ; cmp DWORD [r9 + r10 * 8], cls_shape as i32
                ; jne => miss
                ; mov rcx, [r9 + r10 * 8 + vals_off]
                ; mov rdx, [rcx + ver_off]
                ; mov rcx, QWORD ver_bits as i64
                ; cmp rdx, rcx
                ; jne => miss
                ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
                ; cmp DWORD [rcx + r10 * 4], hver as i32
                ; jne => miss
                ; mov rax, QWORD val_bits as i64
                ; mov [rbx + dreg(dst)], rax
                ; jmp => done
            );
        }
        PyModGetInline::ClassVal {
            inst_shape,
            cls_bits,
            cls_shape,
            ver_slot,
            ver_bits,
            hver,
            layout,
            val_bits,
        } => {
            let ver_off = (ver_slot as i32) * 8;
            dynasm!(ops ; mov rax, [rbx + dreg(obj)]);
            mirror_of_rax(ops, miss);
            dynasm!(ops
                ; cmp DWORD [r9 + r10 * 8], inst_shape as i32
                ; jne => miss
                ; mov r8, [r9 + r10 * 8 + vals_off]
                ; mov rax, [r8]
                ; mov rcx, QWORD cls_bits as i64
                ; cmp rax, rcx
                ; jne => miss
            );
            mirror_of_rax(ops, miss);
            dynasm!(ops
                ; cmp DWORD [r9 + r10 * 8], cls_shape as i32
                ; jne => miss
                ; mov rcx, [r9 + r10 * 8 + vals_off]
                ; mov rdx, [rcx + ver_off]
                ; mov rcx, QWORD ver_bits as i64
                ; cmp rdx, rcx
                ; jne => miss
                ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
                ; cmp DWORD [rcx + r10 * 4], hver as i32
                ; jne => miss
            );
            if layout != 0 {
                dynasm!(ops ; mov rax, [r8 + 8]);
                mirror_of_rax(ops, miss);
                dynasm!(ops
                    ; cmp DWORD [r9 + r10 * 8], layout_shape(layout) as i32
                    ; jne => miss
                );
            } else {
                let dict_ok = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [r8 + 8]
                    ; mov rcx, QWORD crate::value::Value::NULL.bits() as i64
                    ; cmp rax, rcx
                    ; je => dict_ok
                    ; mov rcx, QWORD crate::value::Value::UNDEFINED.bits() as i64
                    ; cmp rax, rcx
                    ; jne => miss
                    ; => dict_ok
                );
            }
            dynasm!(ops
                ; mov rax, QWORD val_bits as i64
                ; mov [rbx + dreg(dst)], rax
                ; jmp => done
            );
        }
    }
    true
}

/// Emit the inline x86-64 fast path for `Instr::PyCallEntry`: shape-guards `f`,
/// reads slot `pos` afresh, verifies it is a `Func`/`Closure` via `hot_mirror`,
/// and writes it to `dst` with zero FFI.
pub(super) fn emit_py_call_entry_inline(
    ops: &mut crate::codegen::X64Asm,
    ip: usize,
    dst: u16,
    f: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{JIT_HOT_FID_OFF, JIT_HOT_VALS_OFF};
    let Some(p) = call_entry_plan_at(ip) else {
        return false;
    };
    let pos_off = (p.pos as i32) * 8;
    dynasm!(ops ; mov rax, [rbx + dreg(f)]);
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], p.f_shape as i32
        ; jne => miss
        ; mov r8, [r9 + r10 * 8 + JIT_HOT_VALS_OFF as i32]
        ; mov rax, [r8 + pos_off]
    );
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], crate::shape::DICT as i32
        ; jne => miss
        ; cmp DWORD [r9 + r10 * 8 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_FUNC_MAX as i32
        ; jae => miss
        ; mov [rbx + dreg(dst)], rax
        ; jmp => done
    );
    true
}

/// Emit the inline x86-64 fast path for `Instr::PyGlobal` (`ic::GLOBAL` / `ic::BUILTIN`):
/// handles both direct `HeapObj::Map` and lazy-globals function records with zero FFI.
/// Jumps to `done` on hit; falls through to `miss` on any guard failure.
pub(super) fn emit_py_global_inline(
    ops: &mut crate::codegen::X64Asm,
    ip: usize,
    dst: u16,
    globals: u16,
    rt: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{JIT_HOT_VALS_OFF, JIT_VERSIONS_RAW_OFFSET};
    let Some(p) = global_plan_at(ip) else {
        return false;
    };
    let map_ok = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(globals)]
        ; mov rcx, QWORD p.g_bits as i64
        ; cmp rax, rcx
        ; je => map_ok
    );
    if p.func_shapes[0] != crate::shape::DICT {
        let shape_ok = ops.new_dynamic_label();
        let gslot_off = (p.func_globals_slot as i32) * 8;
        mirror_of_rax(ops, miss);
        dynasm!(ops
            ; mov edx, DWORD [r9 + r10 * 8]
            ; cmp edx, p.func_shapes[0] as i32
            ; je => shape_ok
        );
        if p.func_shapes[1] != crate::shape::DICT {
            dynasm!(ops
                ; cmp edx, p.func_shapes[1] as i32
                ; je => shape_ok
            );
        }
        dynasm!(ops
            ; jmp => miss
            ; => shape_ok
            ; mov r8, [r9 + r10 * 8 + JIT_HOT_VALS_OFF as i32]
            ; mov rax, [r8 + gslot_off]
            ; cmp rax, rcx
            ; jne => miss
        );
    } else {
        dynasm!(ops ; jmp => miss);
    }
    dynasm!(ops
        ; => map_ok
        ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
        ; cmp DWORD [rcx + p.g_ver_off], p.g_ver as i32
        ; jne => miss
    );
    if p.b_ver_off >= 0 {
        dynasm!(ops
            ; mov rdx, QWORD p.rt_bits as i64
            ; cmp [rbx + dreg(rt)], rdx
            ; jne => miss
            ; cmp DWORD [rcx + p.b_ver_off], p.b_ver as i32
            ; jne => miss
        );
    }
    dynasm!(ops
        ; mov rax, QWORD p.val_bits as i64
        ; mov [rbx + dreg(dst)], rax
        ; jmp => done
    );
    true
}

/// The mirror shape of layout-mode storage of layout `l`.
fn layout_shape(l: u32) -> u32 {
    crate::vm::py_table::layout::LAYOUT_BASE | l
}

/// Heap-tag and bounds-check the value in `rax`, leaving its heap index in
/// r10 and `&mirror[idx]` addressable as `[r9 + r10 * 8]` (the 16-byte
/// record: r9 = base + idx * 8); jump to `miss` otherwise. Clobbers r10, r9.
fn mirror_of_rax(ops: &mut crate::codegen::X64Asm, miss: dynasmrt::DynamicLabel) {
    use crate::vm::host_api::{JIT_HOT_MIRROR_LEN_OFFSET, JIT_HOT_MIRROR_RAW_OFFSET};
    dynasm!(ops
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, TAG_HEAP_HI as i32
        ; jne => miss
        ; mov r10d, eax
        ; cmp r10d, DWORD [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
        ; jae => miss
        ; mov r9, [rdi + JIT_HOT_MIRROR_RAW_OFFSET as i32]
        ; lea r9, [r9 + r10 * 8]
    );
}

/// The flag in the step's packed operand asking `Vm::jit_py_attr_miss` to
/// settle the class record's mirror first (see the module comment).
pub(crate) const PY_ATTR_SETTLE: u64 = 1 << 63;

/// The flag in `Vm::jit_py_method`'s `keys` operand asking it to settle the
/// receiver's class's mirror first (a method site's guard found it cleared).
pub(crate) const PY_METHOD_SETTLE: u64 = 1 << 63;

/// A planned site: the inline path, then its step (`miss`: the plain step;
/// `settle`: the step after settling the class's mirror), both through
/// `Vm::jit_py_attr_miss`, which runs the step exactly as `Vm::jit_py_op`
/// does. `false` (nothing emitted) when the body's plan lacks the site.
pub(crate) fn emit_py_attr_site(
    ops: &mut crate::codegen::X64Asm,
    func_id: u32,
    ip: usize,
    instr: &Instr,
    labels: &PyLabels,
    refetch: &mut dyn FnMut(&mut crate::codegen::X64Asm),
    done: dynasmrt::DynamicLabel,
) -> bool {
    let miss = ops.new_dynamic_label();
    let settle = ops.new_dynamic_label();
    if !emit_py_attr_inline(ops, ip, instr, miss, settle, done) {
        return false;
    }
    let packed = ((func_id as u64) << 32) | ip as u64;
    let helper = crate::vm::Vm::jit_py_attr_miss as *const () as usize;
    let call = ops.new_dynamic_label();
    let cont = ops.new_dynamic_label();
    dynasm!(ops
        ; => settle
        ; mov r8, QWORD (packed | PY_ATTR_SETTLE) as i64
        ; jmp => call
        ; => miss
        ; mov r8, QWORD packed as i64
        ; => call
        ; mov rcx, rdi
        ; mov rdx, rbx
        ; mov rax, QWORD helper as i64
        ; call rax
        ; mov [rsp + 32], rax
    );
    refetch(ops);
    dynasm!(ops
        ; mov rax, [rsp + 32]
        ; test eax, eax
        ; jz => cont
        ; cmp eax, PY_SLOW as i32
        ; je => labels.slow
        ; jmp => labels.bail
        ; => cont
    );
    true
}

env_off_switch! {
    /// `ZIPP_NO_PY_METHOD_INLINE=1` plans no `PyMethod` site (every one
    /// calls `Vm::jit_py_method`, as before): the same-binary A/B for
    /// [`emit_py_method_inline`].
    fn py_method_inline_enabled() = "ZIPP_NO_PY_METHOD_INLINE"
}

/// Emit the inline path of the `PyMethod` at `ip` when the body's plan has
/// one (`Vm::py_method_plan`): the receiver a record of the planned shape
/// (its `cls` then at `vals[0]`, a user class's instance's storage at
/// `vals[1]`) whose class has the planned bits and still the planned stamp
/// (as an attribute site checks it), and, for a user class's instance, whose
/// storage has the layout the plan proved to lack the name; then `dst` takes
/// the planned method, the one the class's table holds for as long as the
/// stamp holds. On success it jumps to `done`; a cleared class mirror jumps
/// to `settle`, any other failed guard to `miss`. `false` (nothing emitted)
/// without a method plan for the site.
pub(crate) fn emit_py_method_inline(
    ops: &mut crate::codegen::X64Asm,
    ip: usize,
    instr: &Instr,
    miss: dynasmrt::DynamicLabel,
    settle: dynasmrt::DynamicLabel,
    slow: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{JIT_HOT_FID_OFF, JIT_HOT_VALS_OFF, JIT_VERSIONS_RAW_OFFSET};
    let Some(p) = plan_at(ip) else {
        return false;
    };
    let Instr::PyMethod { dst, obj, .. } = *instr else {
        return false;
    };
    if p.method == 0 {
        return false;
    }
    let vals_off = JIT_HOT_VALS_OFF as i32;
    let ver_off = (p.ver_slot as i32) * 8;
    // 1. The receiver: either a primitive non-empty flat `str` (sentinel
    //    `shape::DICT`, checked via `FID_MIRROR_STR`) or a record of the
    //    planned shape (`r8 = vals`, `cls = vals[0]`).
    dynasm!(ops ; mov rax, [rbx + dreg(obj)]);
    mirror_of_rax(ops, miss);
    if p.inst_shape == crate::shape::DICT {
        dynasm!(ops
            ; cmp DWORD [r9 + r10 * 8 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_STR as i32
            ; jne => miss
            ; mov rax, QWORD p.cls_bits as i64
        );
    } else {
        dynasm!(ops
            ; cmp DWORD [r9 + r10 * 8], p.inst_shape as i32
            ; jne => miss
            ; mov r8, [r9 + r10 * 8 + vals_off]
            ; mov rax, [r8]
            ; mov rcx, QWORD p.cls_bits as i64
            ; cmp rax, rcx
            ; jne => miss
        );
    }
    // 2. The class: shape, `ver`, heap version (as an attribute site).
    mirror_of_rax(ops, miss);
    let cls_off = ops.new_dynamic_label();
    dynasm!(ops
        ; mov edx, DWORD [r9 + r10 * 8]
        ; cmp edx, p.cls_shape as i32
        ; jne => cls_off
        ; mov rcx, [r9 + r10 * 8 + vals_off]
        ; mov rdx, [rcx + ver_off]
        ; mov rcx, QWORD p.ver_bits as i64
        ; cmp rdx, rcx
        ; jne => miss
        ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
        ; cmp DWORD [rcx + r10 * 4], p.hver as i32
        ; jne => miss
    );
    // 3. A user class's instance: its storage lacks the name (the layout).
    if p.layout != 0 {
        dynasm!(ops ; mov rax, [r8 + 8]);
        mirror_of_rax(ops, miss);
        dynasm!(ops
            ; cmp DWORD [r9 + r10 * 8], layout_shape(p.layout) as i32
            ; jne => miss
        );
    }
    // 4. The method (or proven negative cache -> jump straight to `slow`).
    if p.method == crate::value::Value::HOLE.bits() {
        dynasm!(ops
            ; jmp => slow
            // The class's mirror is not the planned shape: settle it when cleared.
            ; => cls_off
            ; cmp edx, crate::shape::DICT as i32
            ; je => settle
            ; jmp => miss
        );
    } else {
        dynasm!(ops
            ; mov rax, QWORD p.method as i64
            ; mov [rbx + dreg(dst)], rax
            ; jmp => done
            // The class's mirror is not the planned shape: settle it when cleared.
            ; => cls_off
            ; cmp edx, crate::shape::DICT as i32
            ; je => settle
            ; jmp => miss
        );
    }
    true
}

/// Emit the inline path of the `PyGetAttr` / `PySetAttr` at `ip` when the
/// body's plan has one: on success it jumps to `done`; a cleared class
/// mirror jumps to `settle`, any other failed guard to `miss`. `false`
/// (nothing emitted) without a plan.
pub(super) fn emit_py_attr_inline(
    ops: &mut crate::codegen::X64Asm,
    ip: usize,
    instr: &Instr,
    miss: dynasmrt::DynamicLabel,
    settle: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{JIT_GEN_RAW_OFFSET, JIT_HOT_VALS_OFF, JIT_VERSIONS_RAW_OFFSET};
    let Some(p) = plan_at(ip) else {
        return false;
    };
    let (obj, dst, val) = match *instr {
        Instr::PyGetAttr { dst, obj, .. } | Instr::PyModGet { dst, obj, .. } if !p.set => (obj, Some(dst), None),
        Instr::PySetAttr { obj, val, .. } if p.set => (obj, None, Some(val)),
        _ => return false,
    };
    let vals_off = JIT_HOT_VALS_OFF as i32;
    let ver_off = (p.ver_slot as i32) * 8;
    let slot_off = (p.slot as i32) * 8;
    // 1. The instance: template shape; r8 = its vals.
    dynasm!(ops ; mov rax, [rbx + dreg(obj)]);
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], p.inst_shape as i32
        ; jne => miss
        ; mov r8, [r9 + r10 * 8 + vals_off]
        // 2. The class: bits, shape, `ver`, heap version.
        ; mov rax, [r8]
        ; mov rcx, QWORD p.cls_bits as i64
        ; cmp rax, rcx
        ; jne => miss
    );
    mirror_of_rax(ops, miss);
    let cls_off = ops.new_dynamic_label();
    dynasm!(ops
        ; mov edx, DWORD [r9 + r10 * 8]
        ; cmp edx, p.cls_shape as i32
        ; jne => cls_off
        ; mov rcx, [r9 + r10 * 8 + vals_off]
        ; mov rdx, [rcx + ver_off]
        ; mov rcx, QWORD p.ver_bits as i64
        ; cmp rdx, rcx
        ; jne => miss
        ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
        ; cmp DWORD [rcx + r10 * 4], p.hver as i32
        ; jne => miss
        // 3. The storage: layout mode of the planned layout; rcx = its vals.
        ; mov rax, [r8 + 8]
    );
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], layout_shape(p.layout) as i32
        ; jne => miss
        ; mov rcx, [r9 + r10 * 8 + vals_off]
    );
    // 4. The slot.
    if let Some(dst) = dst {
        dynasm!(ops
            ; mov rax, [rcx + slot_off]
            ; mov [rbx + dreg(dst)], rax
            ; jmp => done
        );
    } else if let Some(val) = val {
        let store = ops.new_dynamic_label();
        dynasm!(ops
            ; mov rax, [rbx + dreg(val)]
            ; mov rdx, rax
            ; shr rdx, 48
            ; cmp edx, TAG_HEAP_HI as i32
            ; jne => store
        );
        if p.store_lane {
            // A heap value into young storage: no barrier needed.
            dynasm!(ops
                ; mov rdx, [rdi + JIT_GEN_RAW_OFFSET as i32]
                ; test BYTE [rdx + r10], crate::heap::JIT_GEN_STATE_MASK as i8
                ; jnz => miss
            );
        } else {
            dynasm!(ops ; jmp => miss);
        }
        dynasm!(ops
            ; => store
            ; mov [rcx + slot_off], rax
            ; jmp => done
        );
    }
    // The class's mirror is not the planned shape: settle it when cleared.
    dynasm!(ops
        ; => cls_off
        ; cmp edx, crate::shape::DICT as i32
        ; je => settle
        ; jmp => miss
    );
    true
}

/// Inline `PyGetItem` for exact `list` and `tuple` records of `seq_shape`
/// indexed by an immediate (small) int (positive or negative), reading straight
/// through `hot_mirror` and `arr_mirror`. Jumps to `done` on hit; falls through
/// to `miss` for dicts, out-of-range indices, holes, or dead mirrors.
pub(super) fn emit_py_get_item_inline(
    ops: &mut crate::codegen::X64Asm,
    dst: u16,
    o: u16,
    k: u16,
    seq: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{
        JIT_ARR_MIRROR_LEN_OFFSET, JIT_ARR_MIRROR_RAW_OFFSET, JIT_HOT_VALS_OFF,
    };
    let Some((seq_shape, _, t_tuple, _)) = seq_plan() else {
        return false;
    };
    let cls_ok = ops.new_dynamic_label();
    let nonneg = ops.new_dynamic_label();
    let vals_off = JIT_HOT_VALS_OFF as i32;
    dynasm!(ops ; mov rax, [rbx + dreg(o)]);
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], seq_shape as i32
        ; jne => miss
        ; mov r8, [r9 + r10 * 8 + vals_off]
        ; mov rax, [r8]
        ; cmp rax, [rbx + dreg(seq)]
        ; je => cls_ok
        ; mov rcx, QWORD t_tuple as i64
        ; cmp rax, rcx
        ; jne => miss
        ; => cls_ok
        ; mov rax, [r8 + 8]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, TAG_HEAP_HI as i32
        ; jne => miss
        ; mov ecx, eax
        ; cmp ecx, DWORD [rdi + JIT_ARR_MIRROR_LEN_OFFSET as i32]
        ; jae => miss
        ; mov r9, [rdi + JIT_ARR_MIRROR_RAW_OFFSET as i32]
        ; shl rcx, 4
        ; add r9, rcx
        ; mov r8, [r9]
        ; test r8, r8
        ; jz => miss
    );
    // Load small BigInt index `k` into rdx (reg 2); clobbers only rdx, r10.
    py_load_small(ops, k, 2, miss);
    dynasm!(ops
        ; test rdx, rdx
        ; jns => nonneg
        ; add rdx, [r9 + 8]
        ; js => miss
        ; => nonneg
        ; cmp rdx, [r9 + 8]
        ; jae => miss
        ; mov rax, [r8 + rdx * 8]
        ; mov r10, QWORD crate::value::Value::HOLE.bits() as i64
        ; cmp rax, r10
        ; je => miss
        ; mov r10, QWORD crate::value::Value::UNDEFINED.bits() as i64
        ; cmp rax, r10
        ; je => miss
        ; mov [rbx + dreg(dst)], rax
        ; jmp => done
    );
    true
}

/// Inline `PySetItem` for exact mutable `list` records of `seq_shape` (never
/// tuples!) overwriting a present element at an in-bounds immediate int index.
pub(super) fn emit_py_set_item_inline(
    ops: &mut crate::codegen::X64Asm,
    o: u16,
    k: u16,
    v: u16,
    seq: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{
        JIT_ARR_MIRROR_LEN_OFFSET, JIT_ARR_MIRROR_RAW_OFFSET, JIT_HOT_VALS_OFF,
    };
    let Some((seq_shape, _, _, store_lane)) = seq_plan() else {
        return false;
    };
    let nonneg = ops.new_dynamic_label();
    let vals_off = JIT_HOT_VALS_OFF as i32;
    dynasm!(ops ; mov rax, [rbx + dreg(o)]);
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], seq_shape as i32
        ; jne => miss
        ; mov r8, [r9 + r10 * 8 + vals_off]
        ; mov rax, [r8]
        ; cmp rax, [rbx + dreg(seq)]
        ; jne => miss
        ; mov rax, [r8 + 8]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, TAG_HEAP_HI as i32
        ; jne => miss
        ; mov ecx, eax
        ; cmp ecx, DWORD [rdi + JIT_ARR_MIRROR_LEN_OFFSET as i32]
        ; jae => miss
        ; mov r9, [rdi + JIT_ARR_MIRROR_RAW_OFFSET as i32]
        ; shl rcx, 4
        ; add r9, rcx
        ; mov r8, [r9]
        ; test r8, r8
        ; jz => miss
    );
    // Load small BigInt index `k` into r11 (reg 11); preserves rax, r8, r9.
    py_load_small(ops, k, 11, miss);
    dynasm!(ops
        ; test r11, r11
        ; jns => nonneg
        ; add r11, [r9 + 8]
        ; js => miss
        ; => nonneg
        ; cmp r11, [r9 + 8]
        ; jae => miss
        ; mov r10, QWORD crate::value::Value::HOLE.bits() as i64
        ; cmp [r8 + r11 * 8], r10
        ; je => miss
        ; mov r10, [rbx + dreg(v)]
        ; mov [r8 + r11 * 8], r10
        ; mov r9, r10
        ; shr r9, 48
        ; cmp r9d, TAG_HEAP_HI as i32
        ; jne => done
        ; mov edx, eax
        ; mov r8, r10
    );
    emit_heap_store_barrier(ops, store_lane, done);
    true
}

/// Inline `PyUnpack` for exact `tuple` or `list` records of `seq_shape` whose
/// live `arr_mirror` length equals `n`.
pub(super) fn emit_py_unpack_inline(
    ops: &mut crate::codegen::X64Asm,
    dst: u16,
    v: u16,
    n: u32,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{
        JIT_ARR_MIRROR_LEN_OFFSET, JIT_ARR_MIRROR_RAW_OFFSET, JIT_HOT_VALS_OFF,
    };
    let Some((seq_shape, t_list, t_tuple, _)) = seq_plan() else {
        return false;
    };
    let cls_ok = ops.new_dynamic_label();
    let vals_off = JIT_HOT_VALS_OFF as i32;
    dynasm!(ops ; mov rax, [rbx + dreg(v)]);
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], seq_shape as i32
        ; jne => miss
        ; mov r8, [r9 + r10 * 8 + vals_off]
        ; mov rax, [r8]
        ; mov rcx, QWORD t_tuple as i64
        ; cmp rax, rcx
        ; je => cls_ok
        ; mov rcx, QWORD t_list as i64
        ; cmp rax, rcx
        ; jne => miss
        ; => cls_ok
        ; mov rax, [r8 + 8]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, TAG_HEAP_HI as i32
        ; jne => miss
        ; mov ecx, eax
        ; cmp ecx, DWORD [rdi + JIT_ARR_MIRROR_LEN_OFFSET as i32]
        ; jae => miss
        ; mov r9, [rdi + JIT_ARR_MIRROR_RAW_OFFSET as i32]
        ; shl rcx, 4
        ; add r9, rcx
        ; cmp QWORD [r9], 0
        ; je => miss
        ; mov r10, QWORD (n as u64) as i64
        ; cmp [r9 + 8], r10
        ; jne => miss
        ; mov [rbx + dreg(dst)], rax
        ; jmp => done
    );
    true
}

/// Inline `PyTruth` continuation for exact `list`/`tuple` (`seq_shape`) and
/// exact `dict`/`set`/`frozenset` (`dict_shape`). Expects `r9 = hot_mirror + idx*8`
/// and `r10 = idx` (from `PyTruth`'s `FID_MIRROR_STR` check). Zero FFI.
pub(super) fn emit_py_truth_container_inline(
    ops: &mut crate::codegen::X64Asm,
    store_true: dynasmrt::DynamicLabel,
    store_false: dynasmrt::DynamicLabel,
    miss: dynasmrt::DynamicLabel,
) {
    use crate::vm::host_api::{
        JIT_ARR_MIRROR_LEN_OFFSET, JIT_ARR_MIRROR_RAW_OFFSET, JIT_HOT_VALS_OFF,
    };
    let seq = seq_plan();
    let dict = dict_plan();
    if seq.is_none() && dict.is_none() {
        dynasm!(ops ; jmp => miss);
        return;
    }
    let vals_off = JIT_HOT_VALS_OFF as i32;
    dynasm!(ops
        ; mov edx, DWORD [r9 + r10 * 8]
        ; mov r8, [r9 + r10 * 8 + vals_off]
        ; test r8, r8
        ; jz => miss
    );
    if let Some((seq_shape, t_list, t_tuple, _)) = seq {
        let not_seq = ops.new_dynamic_label();
        let cls_ok = ops.new_dynamic_label();
        dynasm!(ops
            ; cmp edx, seq_shape as i32
            ; jne => not_seq
            ; mov rax, [r8]
            ; mov rcx, QWORD t_list as i64
            ; cmp rax, rcx
            ; je => cls_ok
            ; mov rcx, QWORD t_tuple as i64
            ; cmp rax, rcx
            ; jne => miss
            ; => cls_ok
            ; mov rax, [r8 + 8]
            ; mov r10, rax
            ; shr r10, 48
            ; cmp r10d, TAG_HEAP_HI as i32
            ; jne => miss
            ; mov ecx, eax
            ; cmp ecx, DWORD [rdi + JIT_ARR_MIRROR_LEN_OFFSET as i32]
            ; jae => miss
            ; mov r9, [rdi + JIT_ARR_MIRROR_RAW_OFFSET as i32]
            ; shl rcx, 4
            ; add r9, rcx
            ; cmp QWORD [r9], 0
            ; je => miss
            ; cmp QWORD [r9 + 8], 0
            ; je => store_false
            ; jmp => store_true
            ; => not_seq
        );
    }
    if let Some((dict_shape, size_slot, t_dict, t_set, t_frozenset)) = dict {
        let cls_ok = ops.new_dynamic_label();
        let size_off = (size_slot as i32) * 8;
        dynasm!(ops
            ; cmp edx, dict_shape as i32
            ; jne => miss
            ; mov rax, [r8]
            ; mov rcx, QWORD t_dict as i64
            ; cmp rax, rcx
            ; je => cls_ok
            ; mov rcx, QWORD t_set as i64
            ; cmp rax, rcx
            ; je => cls_ok
            ; mov rcx, QWORD t_frozenset as i64
            ; cmp rax, rcx
            ; jne => miss
            ; => cls_ok
            ; mov rax, [r8 + size_off]
            ; mov r10, rax
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
            ; jne => miss
            ; test eax, eax
            ; jz => store_false
            ; jmp => store_true
        );
    } else {
        dynasm!(ops ; jmp => miss);
    }
}

/// Inline `PyLen` for exact `list`/`tuple` (`seq_shape`) and exact
/// `dict`/`set`/`frozenset` (`dict_shape`).
pub(super) fn emit_py_len_inline(
    ops: &mut crate::codegen::X64Asm,
    dst: u16,
    v: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{
        JIT_ARR_MIRROR_LEN_OFFSET, JIT_ARR_MIRROR_RAW_OFFSET, JIT_HOT_VALS_OFF,
    };
    let seq = seq_plan();
    let dict = dict_plan();
    if seq.is_none() && dict.is_none() {
        return false;
    }
    let vals_off = JIT_HOT_VALS_OFF as i32;
    let store_len = ops.new_dynamic_label();
    dynasm!(ops ; mov rax, [rbx + dreg(v)]);
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; mov edx, DWORD [r9 + r10 * 8]
        ; mov r8, [r9 + r10 * 8 + vals_off]
        ; test r8, r8
        ; jz => miss
    );
    if let Some((seq_shape, t_list, t_tuple, _)) = seq {
        let not_seq = ops.new_dynamic_label();
        let cls_ok = ops.new_dynamic_label();
        dynasm!(ops
            ; cmp edx, seq_shape as i32
            ; jne => not_seq
            ; mov rax, [r8]
            ; mov rcx, QWORD t_list as i64
            ; cmp rax, rcx
            ; je => cls_ok
            ; mov rcx, QWORD t_tuple as i64
            ; cmp rax, rcx
            ; jne => miss
            ; => cls_ok
            ; mov rax, [r8 + 8]
            ; mov r10, rax
            ; shr r10, 48
            ; cmp r10d, TAG_HEAP_HI as i32
            ; jne => miss
            ; mov ecx, eax
            ; cmp ecx, DWORD [rdi + JIT_ARR_MIRROR_LEN_OFFSET as i32]
            ; jae => miss
            ; mov r9, [rdi + JIT_ARR_MIRROR_RAW_OFFSET as i32]
            ; shl rcx, 4
            ; add r9, rcx
            ; cmp QWORD [r9], 0
            ; je => miss
            ; mov r8, [r9 + 8]
            ; jmp => store_len
            ; => not_seq
        );
    }
    if let Some((dict_shape, size_slot, t_dict, t_set, t_frozenset)) = dict {
        let cls_ok = ops.new_dynamic_label();
        let size_off = (size_slot as i32) * 8;
        dynasm!(ops
            ; cmp edx, dict_shape as i32
            ; jne => miss
            ; mov rax, [r8]
            ; mov rcx, QWORD t_dict as i64
            ; cmp rax, rcx
            ; je => cls_ok
            ; mov rcx, QWORD t_set as i64
            ; cmp rax, rcx
            ; je => cls_ok
            ; mov rcx, QWORD t_frozenset as i64
            ; cmp rax, rcx
            ; jne => miss
            ; => cls_ok
            ; mov rax, [r8 + size_off]
            ; mov r10, rax
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
            ; jne => miss
            ; movsxd r8, eax
            ; test r8, r8
            ; js => miss
        );
    } else {
        dynasm!(ops ; jmp => miss);
    }
    dynasm!(ops ; => store_len);
    py_store_small(ops, dst, miss);
    dynasm!(ops ; jmp => done);
    true
}

/// Inline `PyClassOf` for a plain user-class instance (`inst_shape`), exact
/// sequence (`seq_shape`), or exact dict/set (`dict_shape`) whose `cls`
/// (`vals[0]`) is a plain non-ctor object record (settled mirror shape in
/// `1..LAYOUT_BASE` and `fid >= FID_MIRROR_CLASS`). Zero FFI, zero GC.
pub(super) fn emit_py_class_of_inline(
    ops: &mut crate::codegen::X64Asm,
    dst: u16,
    obj: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{JIT_HOT_FID_OFF, JIT_HOT_VALS_OFF};
    let inst = inst_shape_plan();
    let seq = seq_plan().map(|(s, ..)| s);
    let dict = dict_plan().map(|(s, ..)| s);
    if inst.is_none() && seq.is_none() && dict.is_none() {
        return false;
    }
    let shape_ok = ops.new_dynamic_label();
    let vals_off = JIT_HOT_VALS_OFF as i32;
    let fid_off = JIT_HOT_FID_OFF as i32;
    let max_obj_shape_m1 = (crate::vm::py_table::layout::LAYOUT_BASE - 1) as i32;
    dynasm!(ops ; mov rax, [rbx + dreg(obj)]);
    mirror_of_rax(ops, miss);
    dynasm!(ops ; mov ecx, DWORD [r9 + r10 * 8]);
    if let Some(s) = inst {
        dynasm!(ops ; cmp ecx, s as i32 ; je => shape_ok);
    }
    if let Some(s) = seq {
        dynasm!(ops ; cmp ecx, s as i32 ; je => shape_ok);
    }
    if let Some(s) = dict {
        dynasm!(ops ; cmp ecx, s as i32 ; je => shape_ok);
    }
    dynasm!(ops
        ; jmp => miss
        ; => shape_ok
        ; mov r8, [r9 + r10 * 8 + vals_off]
        ; test r8, r8
        ; jz => miss
        ; mov rax, [r8]
        ; mov rdx, rax
    );
    mirror_of_rax(ops, miss);
    dynasm!(ops
        // shape in 1..LAYOUT_BASE AND fid >= FID_MIRROR_CLASS proves plain non-ctor Object.
        ; mov ecx, DWORD [r9 + r10 * 8]
        ; dec ecx
        ; cmp ecx, max_obj_shape_m1
        ; jae => miss
        ; cmp DWORD [r9 + r10 * 8 + fid_off], crate::heap::FID_MIRROR_CLASS as i32
        ; jb => miss
        ; mov [rbx + dreg(dst)], rdx
        ; jmp => done
    );
    true
}

/// Inline `PyDictGet` for a site whose cache proved `layout` lacks `key`
/// (`ic::DICT_GET_ABSENT`): checks `mirror[obj].shape == inst_shape` and
/// `mirror[dict].shape == LAYOUT_BASE | layout`, then jumps directly to `done`
/// (when `absent`) or `slow` (when `!absent`). Zero FFI, zero GC, zero refetch.
pub(super) fn emit_py_dict_get_inline(
    ops: &mut crate::codegen::X64Asm,
    ip: usize,
    obj: u16,
    absent: bool,
    miss: dynasmrt::DynamicLabel,
    slow: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::JIT_HOT_VALS_OFF;
    let Some((inst_shape, layout)) = dict_get_absent_plan_at(ip) else {
        return false;
    };
    let vals_off = JIT_HOT_VALS_OFF as i32;
    dynasm!(ops ; mov rax, [rbx + dreg(obj)]);
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], inst_shape as i32
        ; jne => miss
        ; mov r8, [r9 + r10 * 8 + vals_off]
        ; mov rax, [r8 + 8]
    );
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], layout_shape(layout) as i32
        ; jne => miss
    );
    if absent {
        dynasm!(ops ; jmp => done);
    } else {
        dynasm!(ops ; jmp => slow);
    }
    true
}

/// Inline-guarded `PyCall` for a planned `__zipp_py_strm` site (`argc` in
/// `1..=8`): verifies the builtin record's identity, shape, version, entry,
/// and `strop == op`. For `op == 11, argc == 1` (`math.sqrt`), executes
/// `sqrtsd` completely inline; for all other ops, calls the thin direct helper
/// (`jit_py_strm_noalloc` without GC/refetch for non-allocating ops, else
/// `jit_py_strm_alloc`). Any guard miss or fallback returns to `miss` (`jit_py_call`).
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_py_strm_inline(
    ops: &mut crate::codegen::X64Asm,
    ip: usize,
    dst: u16,
    f: u16,
    arg_base: u16,
    argc: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
    refetch: &mut dyn FnMut(&mut crate::codegen::X64Asm),
) -> bool {
    use crate::vm::host_api::{JIT_HOT_VALS_OFF, JIT_VERSIONS_RAW_OFFSET};
    if argc == 0 || argc > 8 {
        return false;
    }
    let Some(p) = strm_plan_at(ip) else {
        return false;
    };
    let vals_off = JIT_HOT_VALS_OFF as i32;
    let entry_off = (p.entry_slot as i32) * 8;
    let strop_off = (p.strop_slot as i32) * 8;
    dynasm!(ops
        ; mov rax, [rbx + dreg(f)]
        ; mov rcx, QWORD p.f_bits as i64
        ; cmp rax, rcx
        ; jne => miss
    );
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], p.f_shape as i32
        ; jne => miss
        ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
        ; cmp DWORD [rcx + r10 * 4], p.hver as i32
        ; jne => miss
        ; mov r8, [r9 + r10 * 8 + vals_off]
        ; mov rax, [r8 + entry_off]
        ; mov rcx, QWORD p.entry_bits as i64
        ; cmp rax, rcx
        ; jne => miss
        ; mov rax, [r8 + strop_off]
        ; mov rcx, QWORD crate::value::Value::int(p.op).bits() as i64
        ; cmp rax, rcx
        ; jne => miss
    );
    if p.op == 11 && argc == 1 {
        py_load_num(ops, arg_base, 0, miss);
        dynasm!(ops
            ; xorps xmm2, xmm2
            ; ucomisd xmm0, xmm2
            ; jb => miss
            ; sqrtsd xmm0, xmm0
        );
        py_store_num(ops, dst);
        dynasm!(ops ; jmp => done);
        return true;
    }
    if p.op == 47 && argc == 2 {
        dynasm!(ops
            ; mov rax, [rbx + dreg(arg_base + 1)]
            ; mov [rbx + dreg(dst)], rax
            ; jmp => done
        );
        return true;
    }
    let noalloc = matches!(p.op, 6 | 10 | 11 | 12..=19 | 22..=25 | 28..=34 | 42 | 43 | 47);
    let packed = (p.op as u16 as u64) | ((argc as u64) << 16) | ((p.strop_slot as u64) << 32);
    let helper = if noalloc {
        crate::vm::Vm::jit_py_strm_noalloc as *const () as usize
    } else {
        crate::vm::Vm::jit_py_strm_alloc as *const () as usize
    };
    dynasm!(ops
        ; mov rcx, rdi
        ; mov rdx, QWORD p.f_bits as i64
        ; mov r8, QWORD packed as i64
        ; lea r9, [rbx + dreg(arg_base)]
        ; mov rax, QWORD helper as i64
        ; call rax
    );
    if noalloc {
        dynasm!(ops
            ; mov r10, QWORD PY_SLOW_BITS as i64
            ; cmp rax, r10
            ; je => miss
            ; mov [rbx + dreg(dst)], rax
            ; jmp => done
        );
    } else {
        dynasm!(ops ; mov [rsp + 32], rax);
        refetch(ops);
        dynasm!(ops
            ; mov rax, [rsp + 32]
            ; mov r10, QWORD PY_SLOW_BITS as i64
            ; cmp rax, r10
            ; je => miss
            ; mov [rbx + dreg(dst)], rax
            ; jmp => done
        );
    }
    true
}

/// Inline-guarded `PyCall` for a planned frameless Python leaf (`ic::CALL_LEAF`):
/// guards `f == f_bits`, `f_shape`, `hver`, and `entry == entry_bits`, then
/// invokes `Vm::jit_py_leaf_noalloc` directly (never allocates, so no `refetch`).
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_py_leaf_inline(
    ops: &mut crate::codegen::X64Asm,
    ip: usize,
    dst: u16,
    f: u16,
    arg_base: u16,
    argc: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{JIT_HOT_VALS_OFF, JIT_VERSIONS_RAW_OFFSET};
    if argc > 12 {
        return false;
    }
    let Some(p) = leaf_plan_at(ip) else {
        return false;
    };
    if argc > p.params as u16 {
        return false;
    }
    let vals_off = JIT_HOT_VALS_OFF as i32;
    let entry_off = (p.entry_slot as i32) * 8;
    let packed = (p.fid as u64)
        | ((p.main_end as u64) << 32)
        | ((p.reg_slots as u64) << 48)
        | (((argc as u64) & 0xF) << 56)
        | (((p.params as u64) & 0xF) << 60);
    dynasm!(ops
        ; mov rax, [rbx + dreg(f)]
        ; mov rcx, QWORD p.f_bits as i64
        ; cmp rax, rcx
        ; jne => miss
    );
    mirror_of_rax(ops, miss);
    dynasm!(ops
        ; cmp DWORD [r9 + r10 * 8], p.f_shape as i32
        ; jne => miss
        ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
        ; cmp DWORD [rcx + r10 * 4], p.hver as i32
        ; jne => miss
        ; mov r8, [r9 + r10 * 8 + vals_off]
        ; mov rax, [r8 + entry_off]
        ; mov rcx, QWORD p.entry_bits as i64
        ; cmp rax, rcx
        ; jne => miss
        ; mov rcx, rdi
        ; mov rdx, QWORD p.f_bits as i64
        ; mov r8, QWORD packed as i64
        ; lea r9, [rbx + dreg(arg_base)]
        ; mov rax, QWORD crate::vm::Vm::jit_py_leaf_noalloc as *const () as usize as i64
        ; call rax
        ; mov r10, QWORD PY_SLOW_BITS as i64
        ; cmp rax, r10
        ; je => miss
        ; mov [rbx + dreg(dst)], rax
        ; jmp => done
    );
    true
}

/// Inline-guarded `PyIsInstance`: checks `t == p.t_bits` and
/// `versions[t_idx] == p.hver`, then resolves builtin primitive types
/// (`NoneType`, `bool`, `int`, `float`, `str`) or exact heap class matches
/// (`kind == 5`) completely inline without an FFI call. Any unmatched or
/// subclass case jumps to `miss` (`jit_py_isinstance`).
pub(super) fn emit_py_isinstance_inline(
    ops: &mut crate::codegen::X64Asm,
    ip: usize,
    dst: u16,
    v: u16,
    t: u16,
    miss: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) -> bool {
    use crate::vm::host_api::{JIT_HOT_FID_OFF, JIT_HOT_MIRROR_LEN_OFFSET, JIT_HOT_MIRROR_RAW_OFFSET, JIT_HOT_VALS_OFF, JIT_VERSIONS_RAW_OFFSET};
    let Some(p) = isinstance_plan_at(ip) else {
        return false;
    };
    let ret_true = ops.new_dynamic_label();
    let ret_false = ops.new_dynamic_label();
    let check_heap = ops.new_dynamic_label();

    // Guard: `t` still equals the planned type object and its heap version is unchanged.
    dynasm!(ops
        ; mov rax, [rbx + dreg(t)]
        ; mov rcx, QWORD p.t_bits as i64
        ; cmp rax, rcx
        ; jne => miss
        ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
        ; cmp DWORD [rcx + p.t_ver_off], p.hver as i32
        ; jne => miss
        ; mov rax, [rbx + dreg(v)]
    );

    const BOOL_TAG_HI: i32 = 0x7FFA;
    const NULL_TAG_HI: i32 = 0x7FFB;
    const UNDEF_TAG_HI: i32 = 0x7FFC;

    match p.kind {
        // `NoneType`
        0 => {
            dynasm!(ops
                ; mov rcx, QWORD crate::value::Value::NULL.bits() as i64
                ; cmp rax, rcx
                ; je => ret_true
                ; mov rcx, rax
                ; shr rcx, 48
                ; cmp ecx, TAG_HEAP_HI as i32
                ; je => check_heap
                ; cmp ecx, UNDEF_TAG_HI
                ; je => miss
                ; jmp => ret_false
                ; => check_heap
                ; mov r10d, eax
                ; cmp r10d, DWORD [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
                ; jae => miss
                ; mov r9, [rdi + JIT_HOT_MIRROR_RAW_OFFSET as i32]
                ; lea r9, [r9 + r10 * 8]
                ; cmp DWORD [r9 + r10 * 8 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_STR as i32
                ; je => ret_false
                ; jmp => miss
            );
        }
        // `bool` (cannot be subclassed in Python)
        1 => {
            dynasm!(ops
                ; mov rcx, rax
                ; shr rcx, 48
                ; cmp ecx, BOOL_TAG_HI
                ; je => ret_true
                ; cmp ecx, TAG_HEAP_HI as i32
                ; je => check_heap
                ; cmp ecx, UNDEF_TAG_HI
                ; je => miss
                ; jmp => ret_false
                ; => check_heap
                ; mov r10d, eax
                ; cmp r10d, DWORD [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
                ; jae => miss
                ; mov r9, [rdi + JIT_HOT_MIRROR_RAW_OFFSET as i32]
                ; lea r9, [r9 + r10 * 8]
                ; cmp DWORD [r9 + r10 * 8 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_STR as i32
                ; je => ret_false
                ; jmp => miss
            );
        }
        // `int` (includes immediate small BigInts AND `bool`!)
        2 => {
            dynasm!(ops
                ; mov rcx, rax
                ; shr rcx, 47
                ; cmp ecx, SMALL_BIGINT_HI17
                ; je => ret_true
                ; mov rcx, rax
                ; shr rcx, 48
                ; cmp ecx, BOOL_TAG_HI
                ; je => ret_true
                ; cmp ecx, TAG_HEAP_HI as i32
                ; je => check_heap
                ; cmp ecx, UNDEF_TAG_HI
                ; je => miss
                // Remaining non-heap values: NULL (`0x7FFB` without bit 47), Int (`0x7FF9`), Double -> False!
                ; jmp => ret_false
                ; => check_heap
                ; mov r10d, eax
                ; cmp r10d, DWORD [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
                ; jae => miss
                ; mov r9, [rdi + JIT_HOT_MIRROR_RAW_OFFSET as i32]
                ; lea r9, [r9 + r10 * 8]
                ; cmp DWORD [r9 + r10 * 8 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_STR as i32
                ; je => ret_false
                ; jmp => miss
            );
        }
        // `float` (JS Int or Double)
        3 => {
            dynasm!(ops
                ; mov rcx, rax
                ; shr rcx, 48
                ; cmp ecx, TAG_HEAP_HI as i32
                ; je => check_heap
                ; cmp ecx, UNDEF_TAG_HI
                ; je => miss
                ; cmp ecx, BOOL_TAG_HI
                ; je => ret_false
                ; cmp ecx, NULL_TAG_HI
                ; je => ret_false
                ; jmp => ret_true
                ; => check_heap
                ; mov r10d, eax
                ; cmp r10d, DWORD [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
                ; jae => miss
                ; mov r9, [rdi + JIT_HOT_MIRROR_RAW_OFFSET as i32]
                ; lea r9, [r9 + r10 * 8]
                ; cmp DWORD [r9 + r10 * 8 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_STR as i32
                ; je => ret_false
                ; jmp => miss
            );
        }
        // `str` (non-empty flat `Str` via `FID_MIRROR_STR`; non-heap primitives are guaranteed False)
        4 => {
            dynasm!(ops
                ; mov rcx, rax
                ; shr rcx, 48
                ; cmp ecx, TAG_HEAP_HI as i32
                ; je => check_heap
                ; cmp ecx, UNDEF_TAG_HI
                ; je => miss
                ; jmp => ret_false
                ; => check_heap
                ; mov r10d, eax
                ; cmp r10d, DWORD [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
                ; jae => miss
                ; mov r9, [rdi + JIT_HOT_MIRROR_RAW_OFFSET as i32]
                ; lea r9, [r9 + r10 * 8]
                ; cmp DWORD [r9 + r10 * 8 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_STR as i32
                ; je => ret_true
                ; jmp => miss
            );
        }
        // Exact heap class match (`vals[0] == p.t_bits`)
        _ => {
            let _ = (ret_false, check_heap);
            mirror_of_rax(ops, miss);
            dynasm!(ops
                ; cmp DWORD [r9 + r10 * 8], p.recv_shape as i32
                ; jne => miss
                ; mov r8, [r9 + r10 * 8 + JIT_HOT_VALS_OFF as i32]
                ; mov rax, [r8]
                ; mov rcx, QWORD p.t_bits as i64
                ; cmp rax, rcx
                ; je => ret_true
                ; jmp => miss
            );
        }
    }

    dynasm!(ops
        ; => ret_true
        ; mov rax, QWORD crate::value::Value::TRUE.bits() as i64
        ; mov [rbx + dreg(dst)], rax
        ; jmp => done
    );
    if p.kind <= 4 {
        dynasm!(ops
            ; => ret_false
            ; mov rax, QWORD crate::value::Value::FALSE.bits() as i64
            ; mov [rbx + dreg(dst)], rax
            ; jmp => done
        );
    }
    true
}
