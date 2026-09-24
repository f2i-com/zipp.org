//! `codegen::py` in a build without the Python frontend: no program holds a
//! fused Python instruction there, so every question about one answers "none"
//! and the Python tier's code is not compiled at all. The signatures are
//! `py.rs`'s, so the tiers that consult them build unchanged; the emitters
//! are unreachable (each is called only for a body [`has_py_ops`] accepted).
#![allow(dead_code, unused_variables)]
use super::*;

#[inline(always)]
pub(crate) fn py_op_edges(i: &Instr) -> Option<(u32, Option<u32>)> {
    None
}

#[inline(always)]
pub(crate) fn py_op_regs(i: &Instr) -> Option<(Vec<u16>, Vec<u16>)> {
    None
}

#[inline(always)]
pub(crate) fn has_py_ops(code: &[Instr]) -> bool {
    false
}

#[inline(always)]
pub(crate) fn control_targets(i: &Instr) -> [Option<u32>; 2] {
    [bytecode_control_target(i), None]
}

#[inline(always)]
pub(crate) fn py_body_op(i: &Instr) -> bool {
    false
}

#[inline(always)]
pub(crate) fn small_bigint_bits(v: i128) -> Option<u64> {
    None
}

#[inline(always)]
pub(crate) fn py_tierc_enabled() -> bool {
    false
}

pub(crate) struct PyLabels {
    pub slow: dynasmrt::DynamicLabel,
    pub target: Option<dynasmrt::DynamicLabel>,
    pub bail: dynasmrt::DynamicLabel,
}

pub(crate) fn emit_py_load_bigint(
    ops: &mut dynasmrt::x64::Assembler,
    func_id: u32,
    ip: usize,
    dst: u16,
    bail: dynasmrt::DynamicLabel,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    unreachable!("no Python body in this build")
}

pub(crate) fn emit_py_jump_if_not(
    ops: &mut dynasmrt::x64::Assembler,
    a: u16,
    b: u16,
    le: bool,
    target: dynasmrt::DynamicLabel,
    bail: dynasmrt::DynamicLabel,
) {
    unreachable!("no Python body in this build")
}

pub(crate) fn emit_py_add_int(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    a: u16,
    imm: i32,
    upd: bool,
    bail: dynasmrt::DynamicLabel,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    unreachable!("no Python body in this build")
}

pub(crate) fn emit_py_op(
    ops: &mut dynasmrt::x64::Assembler,
    func_id: u32,
    ip: usize,
    instr: &Instr,
    labels: &PyLabels,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    unreachable!("no Python body in this build")
}

pub(crate) fn emit_py_array_append(
    ops: &mut dynasmrt::x64::Assembler,
    arr: u16,
    val: u16,
    bail: dynasmrt::DynamicLabel,
) {
    unreachable!("no Python body in this build")
}

pub(crate) fn emit_py_new_array(
    ops: &mut dynasmrt::x64::Assembler,
    reg_count: u16,
    dst: u16,
    arg_base: u16,
    argc: u16,
    bail: dynasmrt::DynamicLabel,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    unreachable!("no Python body in this build")
}

pub(crate) fn emit_py_load_const(
    ops: &mut dynasmrt::x64::Assembler,
    func_id: u32,
    dst: u16,
    idx: u32,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    unreachable!("no Python body in this build")
}

pub(crate) fn emit_py_push_handler(ops: &mut dynasmrt::x64::Assembler, catch_target: u32, catch_reg: u16) {
    unreachable!("no Python body in this build")
}

pub(crate) fn py_region_members(proto: &FuncProto, start: u32, end: u32) -> (u32, Vec<bool>) {
    (end, Vec::new())
}

impl Jit {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compile_py_region(
        &mut self,
        func_id: u32,
        proto: &FuncProto,
        start: u32,
        end: u32,
        globals_base_helper: usize,
        heap_helpers: HeapHelperAddrs,
        const_strs: &FxHashMap<u32, u64>,
        leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
        method_plan: &FxHashMap<usize, MethodInlinePlan>,
        cross_plan: &CrossCallPlan,
    ) {
        unreachable!("no Python body in this build")
    }
}
