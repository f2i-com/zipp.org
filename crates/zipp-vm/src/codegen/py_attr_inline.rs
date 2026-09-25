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
    /// `PyRt::inst_shape`.
    pub inst_shape: u32,
    pub cls_bits: u64,
    /// The class record's shape when planned.
    pub cls_shape: u32,
    /// The class record's `ver` slot and its bits (the stamp's `ver`).
    pub ver_slot: u32,
    pub ver_bits: u64,
    /// The class record's heap version (the stamp's `hver`).
    pub hver: u32,
    /// The storage's layout and the attribute's slot in it.
    pub layout: u32,
    pub slot: u32,
    /// A store may use the generation bytes (see the module comment).
    pub store_lane: bool,
}

thread_local! {
    /// The plan `Vm::py_attr_inline_plan` made for the compile that follows
    /// it: the planning JIT's address, the function and its sites. Kept out
    /// of `Jit` (and so out of `Vm`) on purpose; any compile takes it, and
    /// only the same JIT's compile of the same function uses it.
    static PENDING: std::cell::RefCell<Option<(usize, u32, FxHashMap<u32, PyAttrInline>)>> = const { std::cell::RefCell::new(None) };
    /// The plan of the body being compiled, for `emit_py_op` (set only for
    /// the duration of one `compile_py_region`).
    static PLAN: std::cell::RefCell<Option<FxHashMap<u32, PyAttrInline>>> = const { std::cell::RefCell::new(None) };
}

/// Hand the compile that follows the plan for `func_id` of the JIT at
/// `jit` (replacing any plan not taken).
pub(crate) fn set_py_attr_plan(jit: usize, func_id: u32, plan: FxHashMap<u32, PyAttrInline>) {
    PENDING.with(|p| *p.borrow_mut() = (!plan.is_empty()).then_some((jit, func_id, plan)));
}

/// Take the pending plan: `Some` when it is for `func_id` of the JIT at
/// `jit`.
pub(crate) fn take_py_attr_plan(jit: usize, func_id: u32) -> Option<FxHashMap<u32, PyAttrInline>> {
    PENDING
        .with(|p| p.borrow_mut().take())
        .and_then(|(j, f, plan)| (j == jit && f == func_id).then_some(plan))
}

/// Run `f` with `plan` as the current body's plan.
pub(crate) fn with_py_attr_plan<R>(plan: Option<FxHashMap<u32, PyAttrInline>>, f: impl FnOnce() -> R) -> R {
    PLAN.with(|p| *p.borrow_mut() = plan);
    let r = f();
    PLAN.with(|p| *p.borrow_mut() = None);
    r
}

fn plan_at(ip: usize) -> Option<PyAttrInline> {
    let ip = u32::try_from(ip).ok()?;
    PLAN.with(|p| p.borrow().as_ref().and_then(|m| m.get(&ip).copied()))
}

/// The mirror shape of layout-mode storage of layout `l`.
fn layout_shape(l: u32) -> u32 {
    crate::vm::py_table::layout::LAYOUT_BASE | l
}

/// Heap-tag and bounds-check the value in `rax`, leaving its heap index in
/// r10 and `&mirror[idx]` addressable as `[r9 + r10 * 8]` (the 16-byte
/// record: r9 = base + idx * 8); jump to `miss` otherwise. Clobbers r10, r9.
fn mirror_of_rax(ops: &mut dynasmrt::x64::Assembler, miss: dynasmrt::DynamicLabel) {
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

/// A planned site: the inline path, then its step (`miss`: the plain step;
/// `settle`: the step after settling the class's mirror), both through
/// `Vm::jit_py_attr_miss`, which runs the step exactly as `Vm::jit_py_op`
/// does. `false` (nothing emitted) when the body's plan lacks the site.
pub(crate) fn emit_py_attr_site(
    ops: &mut dynasmrt::x64::Assembler,
    func_id: u32,
    ip: usize,
    instr: &Instr,
    labels: &PyLabels,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
    done: dynasmrt::DynamicLabel,
) -> bool {
    let miss = ops.new_dynamic_label();
    let settle = ops.new_dynamic_label();
    if !emit_py_attr_inline(ops, ip, instr, miss, settle, done) {
        return false;
    }
    let packed = ((func_id as u64) << 32) | ip as u64;
    let helper = crate::vm::Vm::jit_py_attr_miss as usize;
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

/// Emit the inline path of the `PyGetAttr` / `PySetAttr` at `ip` when the
/// body's plan has one: on success it jumps to `done`; a cleared class
/// mirror jumps to `settle`, any other failed guard to `miss`. `false`
/// (nothing emitted) without a plan.
fn emit_py_attr_inline(
    ops: &mut dynasmrt::x64::Assembler,
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
        Instr::PyGetAttr { dst, obj, .. } if !p.set => (obj, Some(dst), None),
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
