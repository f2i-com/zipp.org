// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

env_off_switch! {
    /// `ZIPP_NO_POLYEQ_FAST=1` restores the pre-B88 `region_poly_eq`, where either
    /// operand being a double jumped to the numeric path unconditionally — and that
    /// path bails for a tagged non-number, so `x !== undefined` deopted the region
    /// on every iteration whenever `x` held a double. Read once per region compile.
    #[inline]
    fn poly_eq_fast_enabled() = "ZIPP_NO_POLYEQ_FAST"
}

env_off_switch! {
    /// `ZIPP_NO_TYPEOF_IS_INLINE=1` restores the helper-only `TypeOfIs` arm
    /// (`emit_typeof_is` calls `jit_typeof_is` for every value). Read once.
    #[inline]
    fn typeof_is_inline_enabled() = "ZIPP_NO_TYPEOF_IS_INLINE"
}

env_off_switch! {
    /// `ZIPP_NO_TYPEOF_SMALL_BIGINT=1`: `emit_typeof_is` hands a small
    /// (immediate) BigInt to `jit_typeof_is`, as before (the same-binary
    /// A/B for its inline answer).
    fn typeof_small_bigint_enabled() = "ZIPP_NO_TYPEOF_SMALL_BIGINT"
}

/// A baked literal's allocation without a call: take the next ready object of
/// its slab class (`Heap::ready`) and fill it, as `ObjMap::ready_fill` does.
/// There is no safe point, so no value moves and nothing needs refetching.
/// Falls through to the helper the caller emits next when the class has none
/// ready, and the helper's refill and poll are what make a loop of these
/// collect; the caller binds the returned label after that helper's tail.
/// `None`: the caller emits the helper alone.
pub(crate) fn emit_ready_literal(
    ops: &mut dynasmrt::x64::Assembler,
    proto: &crate::bytecode::FuncProto,
    plan: u16,
    shape: u32,
    val_base: u16,
    count: u16,
    dst: u16,
) -> Option<dynasmrt::DynamicLabel> {
    let plan = proto.static_key_plans.get(plan as usize)?;
    emit_ready_literal_of(ops, plan, shape, val_base, count, dst)
}

/// `emit_ready_literal` for the literal's plan itself (Tier O, which keeps
/// the plan's address rather than its function).
pub(crate) fn emit_ready_literal_of(
    ops: &mut dynasmrt::x64::Assembler,
    plan: &crate::bytecode::StaticKeyPlan,
    shape: u32,
    val_base: u16,
    count: u16,
    dst: u16,
) -> Option<dynasmrt::DynamicLabel> {
    if !crate::heap::ready_objects_enabled() {
        return None;
    }
    let class = crate::heap::val_slab_class(count as usize)? as usize;
    let handle = crate::bytecode::PlanHandle::borrowed_bits(plan);
    let (miss, done) = (ops.new_dynamic_label(), ops.new_dynamic_label());
    emit_ready_take(ops, class, miss);
    for i in 0..count {
        dynasm!(ops
            ; mov r8, [rbx + dreg(val_base + i)]
            ; mov [rcx + i as i32 * 8], r8
        );
    }
    emit_ready_fill(ops, handle, shape, count as i32, None, dst);
    dynasm!(ops
        ; jmp =>done
        ; =>miss
    );
    Some(done)
}

/// Take the next ready object of slab class `class`, or jump to `miss`:
/// rdx = its map, rcx = its value cell, rax = its slot.
fn emit_ready_take(ops: &mut dynasmrt::x64::Assembler, class: usize, miss: dynasmrt::DynamicLabel) {
    use crate::heap::{ReadyClass, ReadyObj};
    let next = (crate::vm::host_api::JIT_READY_OFFSET + class * std::mem::size_of::<ReadyClass>()) as i32;
    dynasm!(ops
        ; mov rax, [rdi + next]
        ; cmp rax, [rdi + next + 8]
        ; je =>miss
        ; lea rcx, [rax + std::mem::size_of::<ReadyObj>() as i32]
        ; mov [rdi + next], rcx
        ; mov rdx, [rax]
        ; mov rcx, [rax + 8]
        ; mov eax, [rax + 16]
    );
}

/// Finish the object [`emit_ready_take`] took, `n` values in its cell, with
/// the key list `handle` and `shape`, and put it in `dst`. `class`: the
/// instance's class, for its map and its hot record.
fn emit_ready_fill(
    ops: &mut dynasmrt::x64::Assembler,
    handle: u64,
    shape: u32,
    n: i32,
    class: Option<u32>,
    dst: u16,
) {
    let l = crate::heap::ready_layout();
    dynasm!(ops
        ; mov DWORD [rdx + l.shape], shape as i32
        ; mov r8, QWORD handle as i64
        ; mov [rdx + l.keys_all], r8
        ; mov QWORD [rdx + l.keys_len], n
        ; mov DWORD [rdx + l.attrs_len], n
        ; add QWORD [rdx + l.vals_owner_len], n
        ; mov r8, [rdi + crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32]
        ; mov rcx, rax
        ; shl rcx, 4
        ; mov DWORD [r8 + rcx], shape as i32
    );
    if let Some(class) = class {
        let (word, fid) = crate::heap::class_words(class);
        dynasm!(ops
            ; mov DWORD [r8 + rcx + crate::vm::host_api::JIT_HOT_FID_OFF as i32], fid as i32
            ; mov r8, QWORD word as i64
            ; mov [rdx + l.class], r8
        );
    }
    dynasm!(ops
        ; mov r8, QWORD crate::value::Value::heap(0).bits() as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
    );
}

/// `lane`'s instance in `dst`, or a fall through to the helper the caller
/// emits next, nothing done; the caller binds the returned label after it.
pub(crate) fn emit_new_lane(
    ops: &mut dynasmrt::x64::Assembler,
    lane: &NewLanePlan,
    callee: u16,
    arg_base: u16,
    dst: u16,
) -> dynasmrt::DynamicLabel {
    use crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET;
    let (miss, done) = (ops.new_dynamic_label(), ops.new_dynamic_label());
    dynasm!(ops
        ; mov rax, [rbx + dreg(callee)]
        ; mov r10, QWORD lane.callee_bits as i64
        ; cmp rax, r10
        ; jne =>miss
        ; mov ecx, eax
        ; mov r11, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
        ; cmp DWORD [r11 + rcx * 4], lane.callee_ver as i32
        ; jne =>miss
    );
    emit_new_guards(ops, lane, miss);
    emit_new_instance(ops, lane, arg_base, |r| r, dst, miss);
    dynasm!(ops
        ; jmp =>done
        ; =>miss
    );
    done
}

/// `lane`'s proof, or a jump to `miss`: nothing on the chain the proof read
/// has moved, and its prototype is one a collection has seen.
pub(crate) fn emit_new_guards(
    ops: &mut dynasmrt::x64::Assembler,
    lane: &NewLanePlan,
    miss: dynasmrt::DynamicLabel,
) {
    use crate::vm::host_api::{JIT_CLASS_PROTO_EPOCH_OFFSET, JIT_GEN_RAW_OFFSET, JIT_VERSIONS_RAW_OFFSET};
    dynasm!(ops
        ; mov r11, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
        ; cmp DWORD [rdi + JIT_CLASS_PROTO_EPOCH_OFFSET as i32], lane.epoch as i32
        ; jne =>miss
    );
    for &(slot, ver) in &lane.hops {
        dynasm!(ops
            ; cmp DWORD [r11 + slot as i32 * 4], ver as i32
            ; jne =>miss
        );
    }
    // A prototype no collection has seen must be logged for the next one
    // (`ProtoTable::fresh`): the helper's store does that, once.
    if let Some(proto) = lane.proto {
        dynasm!(ops
            ; mov r8, [rdi + JIT_GEN_RAW_OFFSET as i32]
            ; test BYTE [r8 + proto as i32], crate::heap::GEN_STATE as i8
            ; jz =>miss
        );
    }
}

/// Take a ready object for `lane` and fill it, or jump to `miss` with nothing
/// done: the finished instance in `dst`. `rg` maps a body register to the
/// caller's window.
pub(crate) fn emit_new_instance(
    ops: &mut dynasmrt::x64::Assembler,
    lane: &NewLanePlan,
    arg_base: u16,
    rg: impl Fn(u16) -> u16,
    dst: u16,
    miss: dynasmrt::DynamicLabel,
) {
    use crate::vm::host_api::{JIT_PROTO_COUNT_OFFSET, JIT_PROTO_LEN_OFFSET, JIT_PROTO_RAW_OFFSET};
    let n = lane.vals.len();
    let Some(class) = crate::heap::val_slab_class(n) else {
        unreachable!("the planner took a slab class's worth of stores")
    };
    emit_ready_take(ops, class as usize, miss);
    if let Some(proto) = lane.proto {
        dynasm!(ops
            ; cmp rax, [rdi + JIT_PROTO_LEN_OFFSET as i32]
            ; jae =>miss
            ; mov r8, [rdi + JIT_PROTO_RAW_OFFSET as i32]
            ; mov r9, QWORD crate::value::Value::heap(proto).bits() as i64
            ; mov [r8 + rax * 8], r9
            ; inc QWORD [rdi + JIT_PROTO_COUNT_OFFSET as i32]
        );
    }
    for (i, v) in lane.vals.iter().enumerate() {
        match *v {
            NewLaneVal::Arg(a) => dynasm!(ops ; mov r8, [rbx + dreg(arg_base + a)]),
            NewLaneVal::Bits(b) => dynasm!(ops ; mov r8, QWORD b as i64),
            NewLaneVal::Reg(r) => dynasm!(ops ; mov r8, [rbx + dreg(rg(r))]),
        }
        dynasm!(ops ; mov [rcx + i as i32 * 8], r8);
    }
    emit_ready_fill(ops, lane.keys, lane.shape, n as i32, lane.class, dst);
}

/// `TypeOfIs { dst, a, code, neg }` — `dst = (typeof a === TYPEOF_NAMES[code])
/// != neg` as Bool bits — shared by the Tier-C and region emitters.
///
/// A NaN-boxed value's tag decides five of the eight answers without the
/// heap: any double or Int is "number", the Bool tag is "boolean", the
/// Undefined tag is "undefined" (with any payload), the exact `null` is
/// "object", a small (immediate) BigInt (the Null tag with the marker bit)
/// is "bigint". Each of those is a compile-time constant for this
/// `code`/`neg`, so the arm is a tag test and a constant store. Only a heap
/// value — string, object, function, symbol, BigInt, the `[[IsHTMLDDA]]`
/// exotic, or a Cell/Proxy that unwraps to anything — and any other
/// Null-tagged value call `jit_typeof_is`,
/// which is where those rules live. PURE and total: no bail, no refetch,
/// clobbers only rax/rcx/rdx/r8 (the helper call's own set).
///
/// `rdi` = vm, `rbx` = register window, as in every arm of both emitters.
pub(crate) fn emit_typeof_is(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    a: u16,
    code: u8,
    neg: bool,
    helper: usize,
) {
    let code_neg = code as u32 | ((neg as u32) << 8);
    if !typeof_is_inline_enabled() {
        dynasm!(ops
            ; mov rcx, rdi                        // vm
            ; mov rdx, [rbx + dreg(a)]            // value bits
            ; mov r8d, code_neg as i32            // code | neg<<8
            ; mov rax, QWORD helper as i64
            ; call rax
            ; mov [rbx + dreg(dst)], rax          // Bool Value bits
        );
        return;
    }
    let lit = crate::bytecode::TYPEOF_NAMES.get(code as usize).copied();
    let answer = |name: &str| -> i64 {
        crate::value::Value::bool((lit == Some(name)) != neg).bits() as i64
    };
    let (res_number, res_boolean, res_undefined, res_object, res_bigint) = (
        answer("number"),
        answer("boolean"),
        answer("undefined"),
        answer("object"),
        answer("bigint"),
    );
    // A small (immediate) BigInt's top 17 bits: the Null tag and the
    // marker bit (`Value::is_small_bigint`).
    // (`-1` matches nothing: every such value goes to the helper.)
    let small_bigint_top = match crate::value::Value::small_bigint(0) {
        Some(v) if typeof_small_bigint_enabled() => (v.bits() >> 47) as i32,
        _ => -1,
    };
    let is_number = ops.new_dynamic_label();
    let is_boolean = ops.new_dynamic_label();
    let is_undefined = ops.new_dynamic_label();
    let not_null = ops.new_dynamic_label();
    let heap = ops.new_dynamic_label();
    let store = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov rdx, rax
        ; shr rdx, 48
        ; sub edx, TAG_LO as i32                 // 0 Int, 1 Bool, 2 Null, 3 Undefined, 4 Heap
        ; cmp edx, (TAG_HI - TAG_LO) as i32
        ; ja => is_number                        // outside the tag range: a double
        ; je => heap                             // == 4: heap value → helper
        ; test edx, edx
        ; jz => is_number                        // Int
        ; cmp edx, 1
        ; je => is_boolean
        ; cmp edx, 3
        ; je => is_undefined
        // Null tag. Exactly `null` is "object"; a small (immediate) BigInt
        // (value.rs: the Null tag with the marker bit) is "bigint" (every
        // Python int loop counter); anything else the helper names.
        ; mov r8, QWORD crate::value::Value::NULL.bits() as i64
        ; cmp rax, r8
        ; jne => not_null
        ; mov rax, QWORD res_object
        ; jmp => store
        ; => not_null
        ; mov r8, rax
        ; shr r8, 47
        ; cmp r8d, small_bigint_top
        ; jne => heap
        ; mov rax, QWORD res_bigint
        ; jmp => store
        ; => is_number
        ; mov rax, QWORD res_number
        ; jmp => store
        ; => is_boolean
        ; mov rax, QWORD res_boolean
        ; jmp => store
        ; => is_undefined
        ; mov rax, QWORD res_undefined
        ; jmp => store
        ; => heap
        ; mov rcx, rdi                            // vm
        ; mov rdx, rax                            // value bits
        ; mov r8d, code_neg as i32                // code | neg<<8
        ; mov rax, QWORD helper as i64
        ; call rax
        ; => store
        ; mov [rbx + dreg(dst)], rax              // Bool Value bits
    );
}

env_off_switch! {
    /// `ZIPP_NO_MEM_CMPJUMP=1` disables the memory-path fused compare→branch head
    /// (`emit_fused_cmp_branch_head`), restoring the unfused compare + JumpIf pair
    /// byte-identically. Read once per region compile.
    #[inline]
    fn mem_cmp_fuse_enabled() = "ZIPP_NO_MEM_CMPJUMP"
}

env_off_switch! {
    /// Fast Int/Int head for the memory-path fused relational jumps. The generic
    /// path below has to convert both numeric operands to f64 for mixed Int/double
    /// semantics; two tagged Ints can compare their signed i32 payloads directly.
    /// `ZIPP_NO_MEM_INT_CMPJUMP=1` restores the conversion-only emission.
    #[inline]
    fn mem_int_cmpjump_enabled() = "ZIPP_NO_MEM_INT_CMPJUMP"
}

/// Fused compare→branch fast head for the MEMORY path (B118): when a
/// `Lt/Le/Gt/Ge/Eq/Ne {dst,a,b}` is IMMEDIATELY followed by a
/// `JumpIfTrue/False{cond: dst}`, two Int-tagged operands compare and branch on
/// FLAGS — skipping the boxed-bool store→load round trip and the generic
/// JumpIf's tag dispatch. That serial `setcc → box → store → reload → test`
/// chain was the dominant per-character cost of the `parse-large-js` tokenize
/// loop (a charCodeAt scanner is Eq+JumpIf chains over Int char codes).
///
/// The boolean is STILL stored to `dst` before branching: a chained `a || b`
/// condition jumps straight to the JumpIf ip from an earlier arm (so the pair's
/// second op must stay emitted and reachable — the caller keeps it), and the
/// deopt contract wants the register file exact at every ip boundary.
///
/// Non-Int operands fall through to the UNCHANGED generic compare sequence the
/// caller emits right after this head (then into the still-emitted JumpIf).
/// No calls, no bails, no allocation — safe-point schedule unchanged.
/// Off switch: `ZIPP_NO_MEM_CMPJUMP=1` (`mem_cmp_fuse_enabled`).
pub(crate) fn emit_fused_cmp_branch_head(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    a: u16,
    b: u16,
    cmp: Cmp,
    if_false: bool,
    target: dynasmrt::DynamicLabel,
    fallthrough: dynasmrt::DynamicLabel,
) {
    let generic = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => generic                      // a not Int → generic pair
        ; mov r10, rcx
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => generic                      // b not Int → generic pair
        ; cmp eax, ecx                        // signed i32 payload compare
    );
    // setcc preserves flags; the branch below re-tests dl (the `or` boxing the
    // Bool clobbers flags in between).
    match cmp {
        Cmp::Eq => dynasm!(ops ; sete dl),
        Cmp::Ne => dynasm!(ops ; setne dl),
        Cmp::Lt => dynasm!(ops ; setl dl),
        Cmp::Le => dynasm!(ops ; setle dl),
        Cmp::Gt => dynasm!(ops ; setg dl),
        Cmp::Ge => dynasm!(ops ; setge dl),
    }
    dynasm!(ops
        ; movzx edx, dl
        ; mov r10, QWORD BOOL_TAG as i64
        ; or r10, rdx
        ; mov [rbx + dreg(dst)], r10          // dst gets the Bool Value regardless
        ; test dl, dl
    );
    if if_false {
        dynasm!(ops ; jz => target);
    } else {
        dynasm!(ops ; jnz => target);
    }
    dynasm!(ops
        ; jmp => fallthrough                  // not taken → skip the JumpIf ip
        ; => generic
    );
}

/// [`emit_fused_cmp_branch_head`] for `a CMP imm`, where the other operand
/// is a register the instruction just before set to the Int `imm`
/// (`LoadInt`) and no jump lands between the two: the register is not read
/// or tag-tested. A non-Int `a` falls to the same generic sequence.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_fused_cmp_imm_branch_head(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    a: u16,
    imm: i32,
    cmp: Cmp,
    if_false: bool,
    target: dynasmrt::DynamicLabel,
    fallthrough: dynasmrt::DynamicLabel,
) {
    let generic = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => generic                      // a not Int → generic pair
        ; cmp eax, imm                        // signed i32 payload compare
    );
    match cmp {
        Cmp::Eq => dynasm!(ops ; sete dl),
        Cmp::Ne => dynasm!(ops ; setne dl),
        Cmp::Lt => dynasm!(ops ; setl dl),
        Cmp::Le => dynasm!(ops ; setle dl),
        Cmp::Gt => dynasm!(ops ; setg dl),
        Cmp::Ge => dynasm!(ops ; setge dl),
    }
    dynasm!(ops
        ; movzx edx, dl
        ; mov r10, QWORD BOOL_TAG as i64
        ; or r10, rdx
        ; mov [rbx + dreg(dst)], r10          // dst gets the Bool Value regardless
        ; test dl, dl
    );
    if if_false {
        dynasm!(ops ; jz => target);
    } else {
        dynasm!(ops ; jnz => target);
    }
    dynasm!(ops
        ; jmp => fallthrough
        ; => generic
    );
}

env_off_switch! {
    /// `ZIPP_NO_CONST_CMP=1`: a fused compare against a register the
    /// previous `LoadInt` set reads and tag-tests that register again.
    #[inline]
    fn const_cmp_enabled() = "ZIPP_NO_CONST_CMP"
}

/// The byte boundary a compiled loop head starts on: 32, the fetch window.
/// A loop head left where it fell ran some tight loops 10-30% slower
/// (call_closure, arr_read, i32_rw, glob_loop) than the same code placed on a
/// boundary; over every scoreboard row, 32 measured 0.980 on the micro probes
/// and 0.995 on the workloads (16: 0.985 and 1.003), with a few loops that
/// happened to sit well unaligned losing (try_catch, dv_get). The padding is
/// at most a few long NOPs (`align_loop_head`), run once per loop entry when
/// the code before the head falls into it (the integer region's prologue
/// jumps over it). `ZIPP_JIT_LOOP_ALIGN=0` places heads where they fall (also
/// 16 or 64).
pub(crate) fn jit_loop_align() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static V: AtomicUsize = AtomicUsize::new(usize::MAX);
    let v = V.load(Ordering::Relaxed);
    if v != usize::MAX {
        return v;
    }
    let n = crate::env_var("ZIPP_JIT_LOOP_ALIGN")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| matches!(n, 0 | 16 | 32 | 64))
        .unwrap_or(32);
    V.store(n, Ordering::Relaxed);
    n
}

/// Pad `ops` to the next multiple of `align` bytes with at most two long
/// NOPs (the recommended 1- to 9-byte forms), so a loop head that is also
/// reached by falling through runs one or two NOP instructions, not up to
/// fifteen single-byte ones.
pub(crate) fn align_loop_head(ops: &mut dynasmrt::x64::Assembler, align: usize) {
    use dynasmrt::DynasmApi;
    const NOPS: [&[u8]; 10] = [
        &[],
        &[0x90],
        &[0x66, 0x90],
        &[0x0F, 0x1F, 0x00],
        &[0x0F, 0x1F, 0x40, 0x00],
        &[0x0F, 0x1F, 0x44, 0x00, 0x00],
        &[0x66, 0x0F, 0x1F, 0x44, 0x00, 0x00],
        &[0x0F, 0x1F, 0x80, 0x00, 0x00, 0x00, 0x00],
        &[0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
        &[0x66, 0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
    ];
    let off = ops.offset().0;
    let mut pad = (align - off % align) % align;
    while pad > 0 {
        let n = pad.min(9);
        for &b in NOPS[n] {
            ops.push(b);
        }
        pad -= n;
    }
}

/// The instructions of `code[s..=e]` some later instruction of the range
/// jumps back to (loop heads), by offset from `s`.
pub(crate) fn loop_heads(code: &[crate::bytecode::Instr], s: usize, e: usize) -> Vec<bool> {
    let mut heads = vec![false; e + 1 - s];
    for ip in s..=e {
        if let Some(t) = crate::codegen::fn_int::bytecode_control_target(&code[ip]) {
            let t = t as usize;
            if t >= s && t <= ip {
                heads[t - s] = true;
            }
        }
    }
    heads
}

/// Resolve a jump `target` to a label: an in-region ip uses its own label; an
/// out-of-region ip gets (or reuses) an exit stub label.
pub(crate) fn region_target(
    target: u32,
    start: u32,
    end: u32,
    in_region: &[dynasmrt::DynamicLabel],
    exit_stubs: &mut FxHashMap<u32, dynasmrt::DynamicLabel>,
    ops: &mut dynasmrt::x64::Assembler,
) -> dynasmrt::DynamicLabel {
    if target >= start && target <= end {
        in_region[(target - start) as usize]
    } else {
        *exit_stubs
            .entry(target)
            .or_insert_with(|| ops.new_dynamic_label())
    }
}

#[derive(Clone, Copy)]
pub(crate) enum DOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// Load `regs[reg]` as ToInt32 into `eax` for the region's `Bitwise` ops: an
/// Int-tagged payload directly, or a DOUBLE that is exactly integral in
/// (-2^63, 2^63) — for which ToInt32 is simply the low 32 bits of the i64
/// (modulo-2^32 wrap, signed), exactly what `(x + y) | 0` accumulators rely on
/// when the f64 sum crosses i32 range. Anything else — fractional / NaN / Inf /
/// |x| ≥ 2^63 (rare; ToInt32 still defined but not via i64) / bool / null /
/// undefined / heap — jumps to `bail` so the interpreter applies complete
/// ToInt32 semantics. Clobbers rax/r10/xmm0/xmm1.
pub(crate) fn load_toint32(
    ops: &mut dynasmrt::x64::Assembler,
    reg: u16,
    bail: dynasmrt::DynamicLabel,
) {
    let int_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(reg)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32      // 0x7FFA (bool tag)
        ; cmp r10d, 3                            // high16 ∈ [0x7FFA,0x7FFD] ⇒ not a number
        ; jbe => bail
        // A double. ToInt32 TRUNCATES toward zero, so a fractional value is
        // perfectly representable — this used to demand an exactly-integral
        // double and bail otherwise, which sent the single most common
        // truncation idiom in JS (`(x * k) | 0`) to the interpreter on every
        // iteration: 127ms vs 15ms per 3M ops, against node's 3ms.
        //
        // Truncate to i64 and take the low 32 bits. That IS ToInt32 for every
        // |x| < 2^63: truncation toward zero followed by modulo 2^32, which is
        // exactly what discarding the high half does — for fractional values
        // (3.7 -> 3) and for large ones alike (5e9 -> 705032704, 2^31 ->
        // -2147483648, 2^32 -> 0).
        //
        // The one case that must bail is `cvttsd2si` OVERFLOWING, which it
        // signals with the 0x8000_0000_0000_0000 indefinite: NaN, ±Inf, and
        // |x| >= 2^63. Their low 32 bits would be 0, which is right for NaN/±Inf
        // by luck but wrong for e.g. 1e21 (-559939584), so all three go to the
        // interpreter. A true -2^63 is folded in with them; harmless, it is the
        // same value the sentinel denotes.
        ; movq xmm0, rax
        ; cvttsd2si rax, xmm0                    // i64 trunc toward zero
        ; mov r10, QWORD i64::MIN
        ; cmp rax, r10
        ; je => bail                             // NaN / ±Inf / |x| >= 2^63
        ; jmp => done
        ; => int_path
        // eax already holds the i32 payload (low 32 of the boxed Value).
        ; => done
    );
}

/// Load `regs[reg]` as an f64 into `xmm{which}` (0 or 1). Int-tagged → cvtsi2sd;
/// a real double → movq; bool/null/undef/heap → jump to `bail`.
pub(crate) fn load_num_xmm(
    ops: &mut dynasmrt::x64::Assembler,
    reg: u16,
    which: u8,
    bail: dynasmrt::DynamicLabel,
) {
    let int_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(reg)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32      // 0x7FFA (bool tag)
        ; cmp r10d, 3                            // high16 ∈ [0x7FFA, 0x7FFD] ⇒ not a number
        ; jbe => bail
        ; movq Rx(which), rax                    // double: raw f64 bits
        ; jmp => done
        ; => int_path
        ; xorps Rx(which), Rx(which)             // break cvtsi2sd's false dep
        ; cvtsi2sd Rx(which), eax                 // int: low-32 i32 payload
        ; => done
    );
}

/// `a % b` for two Ints, a dividend of at least 0 and a divisor above 0: one
/// 32-bit `idiv`, and the remainder an Int (below the divisor, and never -0,
/// as the dividend is not negative). Any other pair goes to `slow`, the
/// arm's integer-valued double path, which owns the negative dividend's -0,
/// `% 0`, `% -1` and fractions. `k % 10` converted both operands to doubles
/// and back to check they were integers, a chain of conversions ahead of
/// the division on every iteration. Off under `ZIPP_NO_INT_MOD=1`.
pub(crate) fn emit_int_mod_fast(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    a: u16,
    b: u16,
    slow: dynasmrt::DynamicLabel,
    done: dynasmrt::DynamicLabel,
) {
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => slow
        ; mov r10, rcx
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => slow
        ; test eax, eax
        ; js => slow
        ; test ecx, ecx
        ; jle => slow
        ; cdq
        ; idiv ecx
        ; mov eax, edx
    );
    box_eax(ops, dst);
    dynasm!(ops ; jmp => done);
}

/// Store `xmm0` (an f64 result) into `regs[dst]` as a double `Value`.
pub(crate) fn store_xmm(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    dynasm!(ops
        ; movq rax, xmm0
        ; mov [rbx + dreg(dst)], rax
    );
}

/// Validate the exact callable reference consumed by a JIT `MathOp`.
///
/// The `Math.imul` specialization replaces only the allocation-free Rust
/// identity helper. Its source `LoadGlobal` and `GetProp` have already executed
/// at this point on every activation, as have all arguments. Exact receiver and
/// callee compares therefore preserve lookup/accessor/Proxy effects and
/// replacement during argument evaluation. The heap generation compare closes
/// the remaining GC slot-reuse ABA hole. Any mismatch bails at the MathOp, whose
/// interpreter arm calls the already-captured callee with the captured receiver.
pub(crate) fn emit_math_identity_guard(
    ops: &mut dynasmrt::x64::Assembler,
    op: MathFn,
    callee: u16,
    this_v: u16,
    bail: dynasmrt::DynamicLabel,
    imul_guard: Option<MathIntrinsicGuard>,
) {
    if callee == crate::bytecode::NO_REG {
        // BARE `MathOp`: `this_v` is the `LoadGlobal` index of `Math`. Validate
        // the LIVE global slot (r12 = globals base), the receiver's heap
        // generation (r13 = versions base; a structural change invalidates the
        // baked slot address), the live own slot value and the callable's
        // generation — the typed lane's `MathImulGuard`, generalised per op.
        let gidx = this_v as i32;
        if let Some(guard) = imul_guard {
            let (bits, ver, slot) = (
                guard.op_callee_bits[op as usize],
                guard.op_callee_ver[op as usize],
                guard.op_slot[op as usize],
            );
            if bits != 0 && slot != u32::MAX && this_v < crate::bytecode::BARE_MATH_BY_NAME {
                dynasm!(ops
                    ; mov rax, [r12 + gidx * 8]
                    ; mov r10, QWORD guard.receiver_bits as i64
                    ; cmp rax, r10
                    ; jne => bail
                    ; mov ecx, eax
                    ; cmp DWORD [r13 + rcx * 4], guard.receiver_ver as i32
                    ; jne => bail
                    ; mov rcx, QWORD guard.receiver_vals as i64
                    ; mov rax, [rcx + (slot as i32) * 8]
                    ; mov r10, QWORD bits as i64
                    ; cmp rax, r10
                    ; jne => bail
                    ; mov ecx, eax
                    ; cmp DWORD [r13 + rcx * 4], ver as i32
                    ; jne => bail
                );
                return;
            }
        }
        let identity = crate::vm::jit_math_bare_is_intrinsic as *const () as usize;
        dynasm!(ops
            ; mov rcx, rdi
            ; mov edx, op as i32
            ; mov r8d, gidx
            ; mov rax, QWORD identity as i64
            ; call rax
            ; test rax, rax
            ; jz => bail
        );
        return;
    }
    // Every op whose intrinsic callable was baked at compile time takes the
    // same three-compare guard `Math.imul` always had (the `imul` entry of the
    // table equals `callee_bits`/`callee_ver`); an op that was not a plain
    // intrinsic data property then keeps the helper-call guard below.
    if let Some(guard) = imul_guard {
        let (callee_bits, callee_ver) = (
            guard.op_callee_bits[op as usize],
            guard.op_callee_ver[op as usize],
        );
        if callee_bits != 0 {
            dynasm!(ops
                ; mov rax, [rbx + dreg(this_v)]
                ; mov r10, QWORD guard.receiver_bits as i64
                ; cmp rax, r10
                ; jne => bail
                ; mov rax, [rbx + dreg(callee)]
                ; mov r10, QWORD callee_bits as i64
                ; cmp rax, r10
                ; jne => bail
                // Exact heap bits make eax the validated live slot index.
                ; mov ecx, eax
                ; cmp DWORD [r13 + rcx * 4], callee_ver as i32
                ; jne => bail
            );
            return;
        }
    }

    let identity = crate::vm::jit_math_is_intrinsic as *const () as usize;
    dynasm!(ops
        ; mov rcx, rdi
        ; mov edx, op as i32
        ; mov r8, [rbx + dreg(callee)]
        ; mov r9, [rbx + dreg(this_v)]
        ; mov rax, QWORD identity as i64
        ; call rax
        ; test rax, rax
        ; jz => bail
    );
}

/// `regs[dst] = Math.<op>(args…)` for the mem paths. Operands are loaded as
/// numbers (Int/double); a non-numeric operand BAILS to the interpreter, which
/// runs the full ToNumber coercion (a user `valueOf`). So the helpers here never
/// run user code and never allocate — no r13/r14/TA refetch is owed. The result
/// is boxed by `emit_box_num`, which mirrors the interpreter's `Value::num(r)`
/// exactly (exact-int narrows, `-0`/NaN preserved).
///
/// SHARED by the region path (Tier B) and the whole-function path (Tier C).
/// It lived only in `region_mem.rs`, so Tier C rejected every `Math.*` call and
/// blacklisted the containing function for the whole run — which cost
/// markdown-render, parse-large-js and json-large one function each. Copying it
/// would have widened the divergence PERF_ROADMAP B43 already warns about
/// ("the emitter exists in TWO byte-identical copies — factor before editing"),
/// so it is factored here instead.
///
/// The caller must have gated the op/arity set with [`math_op_emittable`]: the
/// arms below assume it, exactly as the region admission check guarantees.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_math_op(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    op: MathFn,
    callee: u16,
    this_v: u16,
    arg_base: u16,
    argc: u16,
    math_unary: usize,
    math_two: usize,
    math_imul_guard: Option<MathIntrinsicGuard>,
) {
    emit_math_identity_guard(ops, op, callee, this_v, bail, math_imul_guard);
    if argc > 2 {
        // `Math.min(a, b, c)`: every argument a number, or the interpreter
        // runs ToNumber on them in order; the helper reads them from the frame.
        for k in 0..argc {
            load_num_xmm(ops, arg_base + k, 0, bail);
        }
        let many = crate::vm::jit_math_many as *const () as i64;
        dynasm!(ops
            ; mov ecx, op as i32
            ; lea rdx, [rbx + dreg(arg_base)]
            ; mov r8d, argc as i32
            ; mov rax, QWORD many
            ; call rax
            ; movq xmm0, rax
        );
        emit_box_num(ops, dst);
    } else if argc == 1
        && matches!(op, MathFn::Clz32)
        && emit_math_clz32(ops, arg_base, dst, bail, math_unary)
    {
    } else if argc == 1 {
        load_num_xmm(ops, arg_base, 0, bail);
        if !emit_math_unary_inline(ops, op) {
            dynasm!(ops
                ; movq rdx, xmm0                  // arg f64 bits (arg1)
                ; mov ecx, op as i32              // MathFn code (repr(u8), arg0)
                ; mov rax, QWORD math_unary as i64
                ; call rax
                ; movq xmm0, rax                  // result f64 bits
            );
        }
        emit_box_num(ops, dst);
    } else if matches!(op, MathFn::Imul) {
        // `Math.imul(a,b)` INLINE — a 32-bit signed multiply, no FFI: ToInt32
        // both operands (a non-int-coercible operand BAILS, so the interpreter
        // runs the full ToNumber incl. a user valueOf — matching the helper),
        // then `imul` (low 32 bits, signed) boxed as Int. The low 32 bits of the
        // product are identical whether the inputs were ToInt32 or ToUint32, so
        // this equals the interpreter's `math_two(Imul)` exactly.
        load_toint32(ops, arg_base, bail);
        dynasm!(ops ; mov r8d, eax);
        load_toint32(ops, arg_base + 1, bail);
        dynasm!(ops ; mov ecx, eax ; mov eax, r8d ; imul eax, ecx);
        box_eax(ops, dst);
    } else {
        // EXACTLY two args (the admission check gated the op set).
        load_num_xmm(ops, arg_base, 0, bail);
        load_num_xmm(ops, arg_base + 1, 1, bail);
        dynasm!(ops
            ; movq rdx, xmm0                  // arg0 f64 bits (arg1)
            ; movq r8, xmm1                   // arg1 f64 bits (arg2)
            ; mov ecx, op as i32              // MathFn code (arg0)
            ; mov rax, QWORD math_two as i64
            ; call rax
            ; movq xmm0, rax
        );
        emit_box_num(ops, dst);
    }
    emit_region_bail(ops, ip, bail, epilogue);
}

/// `Math.<op>(x)` with `x` in xmm0 and the result left there, for an op one
/// SSE instruction computes exactly as `helpers_num2::math_unary` does:
/// `sqrt` (`sqrtsd`, the correctly rounded square root `f64::sqrt` is),
/// `abs` (the sign bit cleared, as `f64::abs`) and, with SSE4.1, `floor`,
/// `ceil` and `trunc` (`roundsd`, exact for every input as the `f64`
/// methods are: ±0, ±∞ and NaN keep their class and sign). The caller boxes
/// the result as it boxes the helper's (`emit_box_num` canonicalises NaN).
/// False, having emitted nothing, for every other op or with
/// `ZIPP_NO_MATH_INLINE=1`; the caller then calls the helper.
pub(crate) fn emit_math_unary_inline(ops: &mut dynasmrt::x64::Assembler, op: MathFn) -> bool {
    if !crate::codegen::math_inline_enabled() {
        return false;
    }
    let sse41 = || std::arch::is_x86_feature_detected!("sse4.1");
    match op {
        MathFn::Sqrt => dynasm!(ops ; sqrtsd xmm0, xmm0),
        MathFn::Abs => dynasm!(ops
            ; movq rax, xmm0
            ; btr rax, 63
            ; movq xmm0, rax
        ),
        // roundsd's immediate: bit 3 suppresses the precision exception,
        // bits 0-1 pick the mode (1 down, 2 up, 3 toward zero).
        MathFn::Floor if sse41() => dynasm!(ops ; roundsd xmm0, xmm0, 9),
        MathFn::Ceil if sse41() => dynasm!(ops ; roundsd xmm0, xmm0, 10),
        MathFn::Trunc if sse41() => dynasm!(ops ; roundsd xmm0, xmm0, 11),
        // `x as f32 as f64`: both conversions round to nearest-even under the
        // default MXCSR, as Rust's casts do.
        MathFn::Fround => dynasm!(ops
            ; cvtsd2ss xmm0, xmm0
            ; cvtss2sd xmm0, xmm0
        ),
        // JS round: `f = floor(x)`, plus one when `x - f >= 0.5`, then x's
        // sign ORed in. `x - f` is exact wherever the answer depends on it,
        // so this avoids `floor(x + 0.5)`'s rounding of the sum (the double
        // below 0.5, odd values from 2^52 up). A nonzero result already has
        // x's sign; a zero one takes it, which is the spec's -0 for
        // [-0.5, -0]. ±Infinity gives NaN for `x - f` and keeps x; NaN stays
        // NaN, which the box canonicalises.
        MathFn::Round if sse41() => dynasm!(ops
            ; roundsd xmm1, xmm0, 9
            ; movapd xmm2, xmm0
            ; subsd xmm2, xmm1
            ; mov rax, QWORD 0x3FE0_0000_0000_0000u64 as i64
            ; movq xmm3, rax
            ; cmplesd xmm3, xmm2
            ; mov rax, QWORD 0x3FF0_0000_0000_0000u64 as i64
            ; movq xmm2, rax
            ; andpd xmm2, xmm3
            ; addsd xmm1, xmm2
            ; mov rax, QWORD i64::MIN
            ; movq xmm2, rax
            ; andpd xmm2, xmm0
            ; orpd xmm1, xmm2
            ; movapd xmm0, xmm1
        ),
        // NaN and ±0 return x; otherwise ±1.
        MathFn::Sign => {
            let (keep, pos) = (ops.new_dynamic_label(), ops.new_dynamic_label());
            dynasm!(ops
                ; xorpd xmm1, xmm1
                ; ucomisd xmm0, xmm1
                ; jp => keep
                ; je => keep
                ; ja => pos
                ; mov rax, QWORD 0xBFF0_0000_0000_0000u64 as i64
                ; movq xmm0, rax
                ; jmp => keep
                ; => pos
                ; mov rax, QWORD 0x3FF0_0000_0000_0000u64 as i64
                ; movq xmm0, rax
                ; => keep
            );
        }
        _ => return false,
    }
    true
}

/// `regs[dst] = Math.clz32(regs[arg])`: an Int argument's bits, or a double
/// truncated to i64 whose low half is ToUint32, counted in place and boxed as
/// an Int. A double `cvttsd2si` cannot represent (NaN, ±Infinity, beyond
/// 2^63) goes to the shared `math_unary` helper; a non-number bails, as
/// `load_num_xmm` does, so the interpreter runs ToNumber. False, having
/// emitted nothing, under `ZIPP_NO_MATH_INLINE=1`.
pub(crate) fn emit_math_clz32(
    ops: &mut dynasmrt::x64::Assembler,
    arg: u16,
    dst: u16,
    bail: dynasmrt::DynamicLabel,
    math_unary: usize,
) -> bool {
    if !crate::codegen::math_inline_enabled() {
        return false;
    }
    let (int_path, helper, done) = (
        ops.new_dynamic_label(),
        ops.new_dynamic_label(),
        ops.new_dynamic_label(),
    );
    dynasm!(ops
        ; mov rax, [rbx + dreg(arg)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32
        ; cmp r10d, 3
        ; jbe => bail
        ; movq xmm0, rax
        ; cvttsd2si rax, xmm0
        ; mov r10, QWORD i64::MIN
        ; cmp rax, r10
        ; je => helper
        ; => int_path
    );
    if std::arch::is_x86_feature_detected!("lzcnt") {
        dynasm!(ops ; lzcnt eax, eax);
    } else {
        let zero = ops.new_dynamic_label();
        dynasm!(ops
            ; mov ecx, eax
            ; mov eax, 32
            ; test ecx, ecx
            ; jz => zero
            ; bsr ecx, ecx
            ; mov eax, 31
            ; sub eax, ecx
            ; => zero
        );
    }
    box_eax(ops, dst);
    dynasm!(ops
        ; jmp => done
        ; => helper
        ; movq rdx, xmm0
        ; mov ecx, MathFn::Clz32 as i32
        ; mov rax, QWORD math_unary as i64
        ; call rax
        ; movq xmm0, rax
    );
    emit_box_num(ops, dst);
    dynasm!(ops ; => done);
    true
}

/// The op/arity subset `emit_math_op` implements. Shared by both mem paths'
/// admission checks so they cannot drift apart again.
///
/// `Math.imul(x)` (ONE arg) diverges and is excluded: the unary helper returns
/// NaN, but the interpreter coerces the missing 2nd arg to `to_uint32(NaN) == 0`
/// and yields 0. Every other unary op agrees at argc == 1.
pub(crate) fn math_op_emittable(op: MathFn, argc: u16) -> bool {
    match argc {
        1 => !matches!(op, MathFn::Imul),
        2 => matches!(
            op,
            MathFn::Pow | MathFn::Atan2 | MathFn::Imul | MathFn::Min | MathFn::Max | MathFn::Hypot
        ),
        3..=MATH_MANY_MAX => matches!(op, MathFn::Min | MathFn::Max | MathFn::Hypot),
        _ => false,
    }
}

/// The most arguments `Math.min`, `max` or `hypot` takes in compiled code: one
/// such call past it, or a zero-argument one, keeps its loop in the interpreter.
pub(crate) const MATH_MANY_MAX: u16 = 8;

/// `regs[dst] = regs[a] <op> regs[b]`. Add/Sub/Mul take an INT fast path when
/// both operands are Int-tagged (32-bit op + overflow check, result boxed Int —
/// exactly the interpreter's `checked_add/sub/mul` fast path), falling to the
/// f64 path on a non-Int operand or overflow. Keeping Int results Int matters
/// downstream: `(x+y)|0` accumulators and `a[i+1]` keys then take their cheap
/// Int paths instead of the double→int round-trip. Div is always f64 (JS `/`
/// has no integer form — mirrors the interpreter). Guards operands are numbers.
/// `wrap` (Add/Sub only) drops the overflow branch: the caller proved with
/// `trunc_only_arith_ips` that the result is observed only through ToInt32.
/// `fb` marks the function's arithmetic feedback on the double path (see
/// `ArithFb`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn dbinop(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    op: DOp,
    int_hint: bool,
    wrap: bool,
    fb: ArithFb,
) {
    let f64_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    // Mul takes it too unless `ZIPP_NO_INT_MUL=1`: an f64 product of two
    // Ints left `2 * i + 1` a double, so every element key, compare and add
    // downstream of it took its double path (an array store of such a key
    // its helper). A product that overflows i32 (hash mixing `h * 31`) pays
    // the `imul` and a predicted `jo` before the f64 path it took anyway.
    let int_ok = int_hint
        && (matches!(op, DOp::Add | DOp::Sub)
            || (matches!(op, DOp::Mul) && crate::codegen::int_mul_enabled()));
    // With `split`, a non-Int operand found by the int path's tag tests
    // goes on to the f64 operation with the operands those tests loaded,
    // where the f64 path used to load and test both again: a double product
    // or sum then costs what it did with no int path in front of it.
    // `ZIPP_NO_NUM_SPLIT=1` restores the reload.
    let split = int_ok && crate::codegen::num_split_enabled();
    let a_not_int = ops.new_dynamic_label();
    let b_not_int = ops.new_dynamic_label();
    let f64_op = ops.new_dynamic_label();
    // Two Ints whose result is no Int (an overflow, a -0 product) skip the
    // feedback check: they did not compute on a double, and marking the op
    // would give it the double-only form, whose results stay doubles where
    // this path gives Ints back.
    let (ovf, past_check) = match fb {
        ArithFb::None => (f64_path, f64_path),
        ArithFb::Notify { .. } => (ops.new_dynamic_label(), ops.new_dynamic_label()),
    };
    if int_ok {
        dynasm!(ops ; mov rax, [rbx + dreg(a)]);
        if !split {
            dynasm!(ops ; mov rcx, [rbx + dreg(b)]);
        }
        dynasm!(ops
            ; mov r10, rax
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
        );
        if split {
            dynasm!(ops ; jne => a_not_int ; mov rcx, [rbx + dreg(b)]);
        } else {
            dynasm!(ops ; jne => f64_path);
        }
        dynasm!(ops
            ; mov r10, rcx
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
        );
        if split {
            dynasm!(ops ; jne => b_not_int);
        } else {
            dynasm!(ops ; jne => f64_path);
        }
        match op {
            DOp::Add => dynasm!(ops ; add eax, ecx),
            DOp::Sub => dynasm!(ops ; sub eax, ecx),
            DOp::Mul => dynasm!(ops ; mov r10d, eax ; imul eax, ecx),
            DOp::Div => unreachable!(),
        }
        // A truncation-only result WRAPS (see `trunc_only_arith_ips`): the
        // i33 result's low 32 bits are exactly what every consumer's ToInt32
        // yields, so the overflow branch and the f64 redo are dead weight.
        if !(wrap && matches!(op, DOp::Add | DOp::Sub)) {
            dynasm!(ops ; jo => ovf);
        }
        if matches!(op, DOp::Mul) {
            // A zero product with a negative operand is -0, which no Int
            // encodes (the interpreter's rule): one operand is 0, so the
            // other's sign is the sign of their or.
            let nonzero = ops.new_dynamic_label();
            dynasm!(ops
                ; test eax, eax
                ; jnz => nonzero
                ; or r10d, ecx
                ; js => ovf
                ; => nonzero
            );
        }
        box_eax(ops, dst);
        dynasm!(ops ; jmp => done);
        if split {
            dynasm!(ops
                // `a` an Int, `b` not: `b` must be a double (high16 in
                // [0x7FFA, 0x7FFD] is a non-number tag, `load_num_xmm`'s rule).
                ; => b_not_int
                ; sub r10d, (INT_TAG_HI + 1) as i32
                ; cmp r10d, 3
                ; jbe => bail
                ; xorps xmm0, xmm0
                ; cvtsi2sd xmm0, eax
                ; movq xmm1, rcx
                ; jmp => f64_op
                // `a` not an Int: it must be a double; `b` loads as either.
                ; => a_not_int
                ; sub r10d, (INT_TAG_HI + 1) as i32
                ; cmp r10d, 3
                ; jbe => bail
                ; movq xmm0, rax
            );
            load_num_xmm(ops, b, 1, bail);
            dynasm!(ops ; jmp => f64_op);
        }
        if ovf != f64_path {
            dynasm!(ops ; => ovf);
            load_num_xmm(ops, a, 0, bail);
            load_num_xmm(ops, b, 1, bail);
            dynasm!(ops ; jmp => past_check);
        }
        // An overflow re-loads both operands from the register file, so the
        // clobbered eax (wrapped overflow value) is irrelevant.
        dynasm!(ops ; => f64_path);
    }
    let reload = ops.new_dynamic_label();
    dynasm!(ops ; => reload);
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    if split {
        dynasm!(ops ; => f64_op);
    }
    let first = emit_arith_fb_check(ops, fb);
    if past_check != f64_path {
        dynasm!(ops ; => past_check);
    }
    match op {
        DOp::Add => dynasm!(ops ; addsd xmm0, xmm1),
        DOp::Sub => dynasm!(ops ; subsd xmm0, xmm1),
        DOp::Mul => dynasm!(ops ; mulsd xmm0, xmm1),
        DOp::Div => dynasm!(ops ; divsd xmm0, xmm1),
    }
    store_xmm(ops, dst);
    if int_ok {
        dynasm!(ops ; => done);
    }
    emit_arith_fb_stub(ops, fb, first, reload);
    emit_region_bail(ops, ip, bail, epilogue);
}

/// Where compiled generic arithmetic records that it computed on a double
/// (`Jit::arith_fb`, which Tier C compiles double-only ops from).
#[derive(Clone, Copy)]
pub(crate) enum ArithFb {
    /// Records nothing.
    None,
    /// A Tier C body's op, which the double-only form would serve: the first
    /// time its byte at `addr` lacks [`ARITH_FB_DOUBLE`], `jit_arith_first_double`
    /// records it and evicts the body, so the next compile gives the op that
    /// form.
    Notify { addr: u64, func_id: u32, ip: u32 },
}

/// `ArithFb`'s record, on a double path with both operands loaded (xmm0,
/// xmm1): `Notify` tests its bit and returns the label the caller must place
/// `emit_arith_fb_stub` at.
pub(crate) fn emit_arith_fb_check(
    ops: &mut dynasmrt::x64::Assembler,
    fb: ArithFb,
) -> Option<dynasmrt::DynamicLabel> {
    let double = crate::codegen::ARITH_FB_DOUBLE as i8;
    match fb {
        ArithFb::None => None,
        ArithFb::Notify { addr, .. } => {
            let first = ops.new_dynamic_label();
            dynasm!(ops
                ; mov r11, QWORD addr as i64
                ; test BYTE [r11], double
                ; jz => first
            );
            Some(first)
        }
    }
}

/// The out-of-line half of `ArithFb::Notify`: `jit_arith_first_double`, then
/// back to `reload`, which loads the operands again (the call clobbered them)
/// and now passes the check.
pub(crate) fn emit_arith_fb_stub(
    ops: &mut dynasmrt::x64::Assembler,
    fb: ArithFb,
    first: Option<dynasmrt::DynamicLabel>,
    reload: dynasmrt::DynamicLabel,
) {
    let (Some(first), ArithFb::Notify { func_id, ip, .. }) = (first, fb) else {
        return;
    };
    let over = ops.new_dynamic_label();
    dynasm!(ops
        ; jmp => over
        ; => first
        ; mov rcx, rdi
        ; mov edx, func_id as i32
        ; mov r8d, ip as i32
        ; mov rax, QWORD crate::vm::jit_arith_first_double as *const () as i64
        ; call rax
        ; jmp => reload
        ; => over
    );
}

/// The `Add`/`Sub`/`AddInt` ips whose result is observed ONLY through ToInt32
/// truncation, so the boxed tiers' Int fast path may WRAP in i32 instead of
/// branching to the f64 overflow path. `(rotate(v) + 1013904223) | 0`: the sum
/// leaves i32 range on a quarter of the iterations, mispredicting `jo`, boxing
/// a double, and paying `cvttsd2si` in the `| 0` that folds it straight back.
///
/// WHY WRAPPING IS EXACT. The Int path runs only when both operands are
/// Int-tagged, so the true result is an integer of magnitude < 2^32 that the
/// generic path represents exactly (an Int, or an f64 after `jo`). Every
/// `Bitwise` operand position is ToInt32 / ToUint32 — a reduction mod 2^32 —
/// and the wrapped i32 IS that residue. A chain through `AddInt` holds too:
/// the generic path stays exact along it (|value| <= depth * 2^31, far below
/// 2^53) and the residue of an integer sum is the sum of the residues. Only
/// an integer IMMEDIATE may join a chain: a register operand can hold a
/// fraction or a string, and then the reader's f64 or concatenation path runs
/// on the wrapped value -- `(a + b + 0.5) | 0` truncates after wrapping where
/// ToInt32 truncates first, and `(a + b) + "5"` concatenates the wrong digits.
/// So `(a + b + c) | 0` admits only its last Add. `Mul` is never admitted:
/// its exact product rounds in f64, and ToInt32 of the rounded value is not
/// the wrapped product.
///
/// THE PROOF is a forward walk over the function's CFG from each producer:
/// every read of the destination register before its next definition, on
/// every path, must be a `Bitwise` operand or the operand of an `AddInt` that
/// is itself a PROVEN member. Reads come from `instr_uses`, the exhaustive operand table;
/// definitions from `writes_reg`, which may under-approximate — that only
/// extends a walk (a read of the redefined register then declines the
/// producer). Control edges are those of `bytecode_control_target`, and every
/// handler / finally target in the function is additionally a successor of
/// EVERY ip, over-approximating the exception edges. A deopt at any later ip
/// resumes the interpreter on this same CFG with the wrapped Int in the frame,
/// and it reads that register only where the walk allowed, through the same
/// ToInt32. Members are the LEAST fixpoint of "every arith reader is a
/// member", so a value can never circulate through members alone (`t = t + k`
/// per iteration, `| 0` after the loop): there the generic f64 accumulation
/// may round past 2^53 and the equivalence would break. Declined outright:
/// functions with a direct eval, `with` or eval-scope op (evaluated code reads
/// locals by name, invisibly to the operand table) and, when an `arguments`
/// object exists, parameter registers (a mapped `arguments[i]` aliases them).
///
/// All false under `ZIPP_NO_INT32_TRUNC_ADD=1` — the byte-identical old
/// emission, for a same-binary A/B.
pub(crate) fn trunc_only_arith_ips(proto: &FuncProto) -> Vec<bool> {
    trunc_only_arith_walk(proto, true)
}

/// [`trunc_only_arith_ips`], its walks bounded by the cuts or (`bounded`
/// false, for the test that the bound changes nothing) run to the end.
fn trunc_only_arith_walk(proto: &FuncProto, bounded: bool) -> Vec<bool> {
    let code = &proto.code;
    let n = code.len();
    let mut out = vec![false; n];
    // A Python body's fused instructions have `slow` edges this walk does
    // not follow; its arithmetic is not the JavaScript kind anyway.
    if n == 0 || !crate::codegen::int32_trunc_add_enabled() || crate::codegen::has_py_ops(code) {
        return out;
    }
    let by_name_scope = !proto.eval_sites.is_empty()
        || code.iter().any(|i| {
            matches!(
                i,
                Instr::DirectEval { .. }
                    | Instr::EvalScopeHas { .. }
                    | Instr::EvalScopeSet { .. }
                    | Instr::WithHas { .. }
                    | Instr::WithGet { .. }
                    | Instr::WithSet { .. }
            )
        });
    if by_name_scope {
        return out;
    }
    // `visited[q] == stamp`: reached by the current candidate's walk (a stamp
    // per walk, not a clear of the whole array per candidate). `limit`: the
    // walk's cut (see `cut_after` below); ips at or past it are not visited.
    fn push(q: usize, limit: usize, stack: &mut Vec<usize>, visited: &mut [u32], stamp: u32) {
        if q < limit && visited[q] != stamp {
            visited[q] = stamp;
            stack.push(q);
        }
    }
    let uses: Vec<Vec<u16>> = code.iter().map(instr_uses).collect();
    let defs: Vec<Option<u16>> = code.iter().map(writes_reg).collect();
    let handler_targets: Vec<usize> = code
        .iter()
        .filter_map(|i| match *i {
            Instr::PushHandler { catch_target, .. } => Some(catch_target as usize),
            Instr::PushFinally { target, .. } | Instr::JumpFinally { target, .. } => {
                Some(target as usize)
            }
            _ => None,
        })
        .filter(|&t| t < n)
        .collect();
    // A walk's bound. `cut_after[x]` is the first ip `c >= x` that no edge
    // crosses backwards: no jump at an ip past `c` lands at or before it, and
    // no handler target is at or before it (handlers are successors of every
    // ip). A path from past `c` therefore never comes back to `c` or before,
    // so a walk for a register last read at `x` stops at `c`: past it are no
    // reads. Without the bound every walk ran to the function's end, since
    // the compiler gives each statement fresh temporaries that are never
    // written again: quadratic, and most of a 3,000-line generated
    // function's run.
    let mut crossings = vec![0i32; n + 1];
    for (r, i) in code.iter().enumerate() {
        let s = match *i {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. }
            | Instr::PushFinally { target, .. }
            | Instr::JumpFinally { target, .. } => target as usize,
            Instr::PushHandler { catch_target, .. } => catch_target as usize,
            _ => continue,
        };
        // A jump at `r` back to `s` crosses every cut in `s..r`.
        if s < r {
            crossings[s] += 1;
            crossings[r] -= 1;
        }
    }
    for &h in &handler_targets {
        crossings[h] += 1;
        crossings[n] -= 1;
    }
    let mut cut_after = vec![n; n + 1];
    let mut open = 0i32;
    let mut crossed = vec![false; n];
    for x in 0..n {
        open += crossings[x];
        crossed[x] = open > 0;
    }
    for x in (0..n).rev() {
        cut_after[x] = if crossed[x] { cut_after[x + 1] } else { x };
    }
    let mut last_read: Vec<usize> = Vec::new();
    for (q, u) in uses.iter().enumerate() {
        for &r in u {
            let r = r as usize;
            if r >= last_read.len() {
                last_read.resize(r + 1, usize::MAX);
            }
            last_read[r] = q;
        }
    }
    // Per candidate: the arith readers its membership waits on; `None` once a
    // reader is neither truncating nor arithmetic (or the walk was declined).
    let mut deps: Vec<Option<Vec<usize>>> = vec![None; n];
    let mut visited = vec![0u32; n];
    let mut stamp = 0u32;
    let mut stack: Vec<usize> = Vec::new();
    for p in 0..n {
        let t = match code[p] {
            Instr::Add { dst, .. } | Instr::Sub { dst, .. } | Instr::AddInt { dst, .. } => dst,
            _ => continue,
        };
        if proto.arguments_reg.is_some() && (1..=proto.param_count).contains(&t) {
            continue;
        }
        // Never read anywhere: a member with no readers, as the walk finds.
        let limit = match last_read.get(t as usize) {
            _ if !bounded => n,
            Some(&hi) if hi != usize::MAX => (cut_after[hi] + 1).min(n),
            _ => 0,
        };
        stamp += 1;
        stack.clear();
        push(p + 1, limit, &mut stack, &mut visited, stamp);
        for &h in &handler_targets {
            push(h, limit, &mut stack, &mut visited, stamp);
        }
        let mut arith_readers: Vec<usize> = Vec::new();
        let mut ok = true;
        while let Some(q) = stack.pop() {
            let i = &code[q];
            if uses[q].contains(&t) {
                if matches!(i, Instr::AddInt { .. }) {
                    arith_readers.push(q);
                } else if !matches!(i, Instr::Bitwise { .. }) {
                    ok = false;
                    break;
                }
            }
            if defs[q] == Some(t) {
                continue; // redefined: this path ends here
            }
            match *i {
                Instr::Jump { target } => push(target as usize, limit, &mut stack, &mut visited, stamp),
                Instr::Return { .. } | Instr::ReturnUndefined => {}
                Instr::JumpIfFalse { target, .. }
                | Instr::JumpIfTrue { target, .. }
                | Instr::JumpIfNotLt { target, .. }
                | Instr::JumpIfNotLe { target, .. }
                | Instr::PushFinally { target, .. }
                | Instr::JumpFinally { target, .. } => {
                    push(target as usize, limit, &mut stack, &mut visited, stamp);
                    push(q + 1, limit, &mut stack, &mut visited, stamp);
                }
                Instr::PushHandler { catch_target, .. } => {
                    push(catch_target as usize, limit, &mut stack, &mut visited, stamp);
                    push(q + 1, limit, &mut stack, &mut visited, stamp);
                }
                _ => push(q + 1, limit, &mut stack, &mut visited, stamp),
            }
        }
        if ok {
            deps[p] = Some(arith_readers);
        }
    }
    // Least fixpoint: a candidate joins once every arith reader has.
    loop {
        let mut changed = false;
        for p in 0..n {
            if out[p] {
                continue;
            }
            if let Some(readers) = &deps[p] {
                if readers.iter().all(|&q| out[q]) {
                    out[p] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// `regs[dst] = (regs[a] <cmp> regs[b]) as Bool` using f64 ordered comparison
/// (NaN compares false for </<=/>/>=/==, true for !=). Guards both are numbers.
#[allow(clippy::too_many_arguments)]
/// If `c` is a "pending string" constant (`Value::heap(STRING_CONST_BIT | si)`,
/// the form the compiler emits for a string literal) whose text is exactly ONE
/// ASCII byte, return that char's INTERNED Value bits (`Value::heap(byte)` —
/// single ASCII chars live at heap index == their byte; see `Heap::new`). This
/// lets the region materialise `"7"` as the same boxed value `s[i]` yields, so
/// `s[i] === "7"` is a bits compare. Returns `None` for numeric / multi-char /
/// non-ASCII / non-string constants (the region handles numbers; others decline).
/// Element-kind id for a whitelisted DataView `get*` method name (the kinds the
/// `jit_dv_get` helper decodes without allocating). `None` for everything else
/// (set*, BigInt64/BigUint64 and Float16 getters stay on the generic path).
pub fn dv_get_kind(key: &str) -> Option<u8> {
    match key {
        "getInt8" => Some(0),
        "getUint8" => Some(1),
        "getInt16" => Some(3),
        "getUint16" => Some(4),
        "getInt32" => Some(5),
        "getUint32" => Some(6),
        "getFloat32" => Some(7),
        "getFloat64" => Some(8),
        _ => None,
    }
}

pub(crate) fn single_char_const_bits(proto: &FuncProto, c: Value) -> Option<u64> {
    if !c.is_heap() {
        return None;
    }
    let raw = c.heap_index();
    if raw & crate::vm::STRING_CONST_BIT == 0 {
        return None; // a real heap value, not a pending string constant
    }
    let si = (raw & !crate::vm::STRING_CONST_BIT) as usize;
    let bytes = proto.string_constants.get(si)?.as_bytes();
    if bytes.len() == 1 && bytes[0] < 128 {
        Some(Value::heap(bytes[0] as u32).bits())
    } else {
        None
    }
}

/// Polymorphic strict `===` / `!==` (`ne` selects `!==`) for the region's MEMORY
/// path. Operand types are unknown at compile time, so the emitted code branches
/// at runtime:
///   1. EITHER operand is a DOUBLE (NaN-box high16 ∉ [TAG_LO, TAG_HI]) → the f64
///      numeric compare (identical to `dcmp` Eq/Ne) — keeps `0.5===0.5`,
///      `NaN!==NaN`, `0===-0` correct, and bails on a num-vs-non-num operand mix.
///   2. else EITHER operand is HEAP (high16 == 0x7FFD) with index at/above
///      USER_OBJ_START (a dynamic string or user object — NOT an immutable
///      prefix string) → the read-only `jit_strict_eq` helper (full `strict_eq`
///      semantics: equal-content strings, BigInts, identity for objects) —
///      `line === "##"` scans stay native instead of deopting.
///   3. else → 64-bit BITS equality. Exactly JS `===` for Int, Bool, Null,
///      Undefined, and immutable prefix strings (indices < USER_OBJ_START). This
///      is the `s[i] === "7"` and `charCodeAt === 55` hot path (call-free).
#[allow(clippy::too_many_arguments)]
pub(crate) fn region_poly_eq(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    ne: bool,
    strict_eq_helper: usize,
) {
    let numeric = ops.new_dynamic_label();
    // B88: reached when exactly ONE operand is a double and the other is a
    // TAGGED non-number (undefined / null / bool / heap). `===` is false and
    // `!==` is true for every such pair, by definition — a Number is never
    // SameValueZero with a non-Number — so the answer is a constant and the
    // region does not have to leave.
    let definitely_ne = ops.new_dynamic_label();
    let a_is_dbl = ops.new_dynamic_label();
    let b_is_dbl = ops.new_dynamic_label();
    let do_bits = ops.new_dynamic_label();
    let store = ops.new_dynamic_label();
    let slow = ops.new_dynamic_label();
    let atoms = ops.new_dynamic_label();
    let after = ops.new_dynamic_label();
    // rax = a_bits, rcx = b_bits (kept live across the type checks).
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        // is a a double?  high16 = rax>>48; double ⇔ (high16 - TAG_LO) > (TAG_HI-TAG_LO)
        ; mov rdx, rax
        ; shr rdx, 48
        ; sub edx, TAG_LO as i32
        ; cmp edx, (TAG_HI - TAG_LO) as i32
        ; ja => a_is_dbl                      // a is a double (tag out of tagged range)
        // is b a double?
        ; mov rdx, rcx
        ; shr rdx, 48
        ; sub edx, TAG_LO as i32
        ; cmp edx, (TAG_HI - TAG_LO) as i32
        ; ja => b_is_dbl                      // b is a double, a is tagged
        // Neither is a double. Identical bits are `===` (the same Int, Bool,
        // null, undefined, or the same heap value). A heap value (string,
        // object, symbol, BigInt) against a tagged non-heap value is never
        // `===`, so differing bits answer it too: `x === null`, `o?.a`, `??`
        // with an object operand. Only two DIFFERENT heap values, one of
        // them outside the immutable prefix (a dynamic string: equal
        // content; a BigInt), need the full `strict_eq` helper.
        ; cmp rax, rcx
        ; je => do_bits
        ; mov rdx, rax
        ; shr rdx, 48
        ; cmp edx, TAG_HEAP_HI as i32
        ; jne => do_bits                       // a not heap: bits differ
        ; mov rdx, rcx
        ; shr rdx, 48
        ; cmp edx, TAG_HEAP_HI as i32
        ; jne => do_bits                       // b not heap: bits differ
        ; mov r9, QWORD PAYLOAD_MASK as i64
        ; mov rdx, rax
        ; and rdx, r9
        ; cmp rdx, USER_OBJ_START as i32
        ; jae => atoms                         // a: non-interned heap → atoms?
        ; mov rdx, rcx
        ; and rdx, r9
        ; cmp rdx, USER_OBJ_START as i32
        ; jae => atoms                         // b: non-interned heap → atoms?
        ; jmp => do_bits
        // Two different heap values, one outside the prefix. Both ATOMS
        // (`Heap::atoms`: the prefix's characters, each literal's shared
        // string) have different texts, so `===` is false without the
        // helper: `node.kind === "bin"` when it holds "num". Anything else,
        // or a slot past the bitmap, asks the helper.
        ; => atoms
        ; mov rdx, rax
        ; and rdx, r9                          // a's index
        ; mov r10, rcx
        ; and r10, r9                          // b's index
        ; mov r11, [rdi + crate::vm::host_api::JIT_ATOMS_WORDS_OFFSET as i32]
        ; mov r8, rdx
        ; shr r8, 6
        ; cmp r8, r11
        ; jae => slow
        ; mov r9, r10
        ; shr r9, 6
        ; cmp r9, r11
        ; jae => slow
        ; mov r11, [rdi + crate::vm::host_api::JIT_ATOMS_RAW_OFFSET as i32]
        ; mov r8, [r11 + r8 * 8]
        ; bt r8, rdx
        ; jnc => slow
        ; mov r9, [r11 + r9 * 8]
        ; bt r9, r10
        ; jnc => slow
        ; mov al, ne as i8                     // `!==` → true, `===` → false
        ; jmp => store
    );
    // ── one operand is a double ──────────────────────────────────────────────
    // The numeric path below calls `load_num_xmm` on BOTH operands, and that
    // bails for any tagged non-number. Jumping there on "a is a double" alone —
    // which is what this did — meant `x === undefined` / `!== null` / `!== "s"`
    // deopted the region on EVERY iteration whenever `x` held a double. Measured
    // in isolation: an Int-valued arm 9ms against a double-valued one 62ms
    // (6.9x, node 2ms for both), with 64 deopts and then eviction on the double
    // arm only — so the whole enclosing loop fell back to the interpreter
    // permanently. It is what killed `map-set-heavy`'s 400,000-iteration lookup
    // loop, where `m.get(k) !== undefined` reads a double because the map was
    // filled with `i * 2 + 1`.
    //
    // Only a NUMBER can compare equal to a double, so the other operand needs a
    // real f64 compare when it is a double or an Int, and has a constant answer
    // otherwise.
    if poly_eq_fast_enabled() {
        dynasm!(ops
            ; => a_is_dbl
            ; mov rdx, rcx
            ; shr rdx, 48
            ; sub edx, TAG_LO as i32
            ; cmp edx, (TAG_HI - TAG_LO) as i32
            ; ja => numeric                       // b is a double too
            ; mov rdx, rcx
            ; shr rdx, 48
            ; cmp edx, INT_TAG_HI as i32
            ; je => numeric                       // b is an Int: 1.0 === 1 is true
            ; jmp => definitely_ne                // b is undefined/null/bool/heap
            ; => b_is_dbl
            ; mov rdx, rax
            ; shr rdx, 48
            ; cmp edx, INT_TAG_HI as i32
            ; je => numeric                       // a is an Int
            ; jmp => definitely_ne                // a is undefined/null/bool/heap
            ; => definitely_ne
            ; mov al, ne as i8                    // `!==` → true, `===` → false
            ; jmp => store
        );
    } else {
        // `ZIPP_NO_POLYEQ_FAST=1`: the pre-B88 shape — either operand being a
        // double goes straight to the numeric path, which bails on a tagged
        // non-number. Kept so the change is A/B-able on ONE binary.
        dynasm!(ops ; => a_is_dbl ; jmp => numeric ; => b_is_dbl ; jmp => numeric ; => definitely_ne ; jmp => numeric);
    }
    // ── numeric path (both operands numeric): f64 compare, identical to dcmp. ──
    dynasm!(ops ; => numeric);
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    if ne {
        dynasm!(ops ; ucomisd xmm0, xmm1 ; setne al ; setp cl ; or al, cl);
    } else {
        dynasm!(ops ; ucomisd xmm0, xmm1 ; sete al ; setnp cl ; and al, cl);
    }
    dynasm!(ops ; jmp => store);
    // ── bits path: result = (a_bits <op> b_bits) as Bool. ──
    dynasm!(ops
        ; => do_bits
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        ; cmp rax, rcx
    );
    if ne {
        dynasm!(ops ; setne al);
    } else {
        dynasm!(ops ; sete al);
    }
    dynasm!(ops
        ; => store
        ; movzx rax, al
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
        ; jmp => after
        // ── slow path: full strict_eq via the read-only helper. ──
        ; => slow
        ; mov rcx, rdi                         // vm
        ; mov rdx, [rbx + dreg(a)]
        ; mov r8, [rbx + dreg(b)]
        ; mov rax, QWORD strict_eq_helper as i64
        ; call rax                             // rax = 0/1 (a === b)
    );
    if ne {
        dynasm!(ops ; xor rax, 1);
    }
    dynasm!(ops
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
        ; => after
    );
    emit_region_bail(ops, ip, bail, epilogue);
}

env_off_switch! {
    /// `ZIPP_NO_JIT_STR_CMP=1`: a relational op on anything but numbers bails
    /// (the same-binary A/B for `dcmp`'s string arm).
    fn jit_str_cmp_enabled() = "ZIPP_NO_JIT_STR_CMP"
}

/// Where `dcmp` and `djump_if_not_cmp` send a non-number: their string arm, or
/// `bail` when the caller's code around them cannot take a call.
fn not_number_label(
    ops: &mut dynasmrt::x64::Assembler,
    strings: bool,
    bail: dynasmrt::DynamicLabel,
) -> dynasmrt::DynamicLabel {
    if strings && jit_str_cmp_enabled() {
        ops.new_dynamic_label()
    } else {
        bail
    }
}

/// The end of a relational op: a jump over its cold arms, then those. Two
/// strings get their answer from `jit_str_relational` (0/1 in rax, which
/// `answer` takes); it reads the heap and allocates nothing on it, so there is
/// nothing to refetch. Anything else, with a user `valueOf` or a BigInt to
/// consider, goes to the interpreter from `bail`. `a < b` on strings used to
/// bail on every run, and a function comparing strings stayed interpreted, with
/// its caller resuming in the interpreter after each call.
#[allow(clippy::too_many_arguments)]
fn emit_relational_tail(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    not_number: dynasmrt::DynamicLabel,
    a: u16,
    b: u16,
    cmp: Cmp,
    answer: impl FnOnce(&mut dynasmrt::x64::Assembler),
) {
    if not_number == bail {
        emit_region_bail(ops, ip, bail, epilogue);
        return;
    }
    let done = ops.new_dynamic_label();
    let op = match cmp {
        Cmp::Lt => 0,
        Cmp::Le => 1,
        Cmp::Gt => 2,
        _ => 3,
    };
    dynasm!(ops
        ; jmp => done
        ; => not_number
        ; mov rcx, rdi                        // vm
        ; mov rdx, [rbx + dreg(a)]
        ; mov r8, [rbx + dreg(b)]
        ; mov r9d, op
        ; mov rax, QWORD crate::vm::jit_str_relational as *const () as i64
        ; call rax
        ; cmp eax, 2
        ; je => bail
    );
    answer(ops);
    dynasm!(ops
        ; jmp => done
        ; => bail
        ; mov DWORD [rsi], ip as i32
        ; jmp => epilogue
        ; => done
    );
}

/// `dst = regs[a] <cmp> regs[b]` for the four relational ops. `strings`: the
/// emitter around it can take a helper call in the cold arm.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dcmp(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    cmp: Cmp,
    strings: bool,
) {
    let not_number = not_number_label(ops, strings, bail);
    load_num_xmm(ops, a, 0, not_number);
    load_num_xmm(ops, b, 1, not_number);
    // Compute the boolean into al. ucomisd sets CF/ZF/PF; ordering tricks below
    // keep NaN-false semantics (see jump variant for the rationale).
    match cmp {
        Cmp::Lt => dynasm!(ops ; ucomisd xmm1, xmm0 ; seta al), // a<b  ⇔ b>a ordered
        Cmp::Le => dynasm!(ops ; ucomisd xmm1, xmm0 ; setae al), // a<=b ⇔ b>=a ordered
        Cmp::Gt => dynasm!(ops ; ucomisd xmm0, xmm1 ; seta al), // a>b
        _ => dynasm!(ops ; ucomisd xmm0, xmm1 ; setae al), // a>=b
    }
    dynasm!(ops
        ; movzx rax, al
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
    );
    emit_relational_tail(ops, ip, bail, epilogue, not_number, a, b, cmp, |ops| {
        dynasm!(ops
            ; mov r8, QWORD BOOL_TAG as i64
            ; or rax, r8
            ; mov [rbx + dreg(dst)], rax
        );
    });
}

/// Fused `if !(regs[a] <cmp> regs[b]) goto target` in f64. Guards both numbers.
/// Only Lt/Le are emitted by the compiler (loop guards).
#[allow(clippy::too_many_arguments)]
pub(crate) fn djump_if_not_cmp(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    a: u16,
    b: u16,
    cmp: Cmp,
    target: dynasmrt::DynamicLabel,
    strings: bool,
) {
    let generic = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    if mem_int_cmpjump_enabled() {
        // Mirror `emit_fused_cmp_branch_head`'s representation proof: an Int
        // has high16 == INT_TAG_HI and its low32 is the exact signed payload.
        // The not-taken arm jumps over the generic f64 conversion below; any
        // mixed/double/non-number input reaches that old path byte-for-byte.
        dynasm!(ops
            ; mov rax, [rbx + dreg(a)]
            ; mov rcx, [rbx + dreg(b)]
            ; mov r10, rax
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
            ; jne => generic
            ; mov r10, rcx
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
            ; jne => generic
            ; cmp eax, ecx                    // signed i32 payload comparison
        );
        match cmp {
            Cmp::Lt => dynasm!(ops ; jge => target), // !(a < b)
            Cmp::Le => dynasm!(ops ; jg => target),  // !(a <= b)
            _ => {}
        }
        dynasm!(ops
            ; jmp => done
            ; => generic
        );
    } else {
        // Keep the off-switch stream label-complete without emitting a branch
        // or any fast-head instructions.
        dynasm!(ops ; => generic);
    }
    let not_number = not_number_label(ops, strings, bail);
    load_num_xmm(ops, a, 0, not_number);
    load_num_xmm(ops, b, 1, not_number);
    // Jump when the comparison is FALSE. ucomisd(b,a): CF=1 ⇔ b<a OR unordered.
    match cmp {
        // !(a<b): b<=a or NaN. ucomisd(b,a) then jbe (CF|ZF). NaN sets CF ⇒ jumps.
        Cmp::Lt => dynasm!(ops ; ucomisd xmm1, xmm0 ; jbe => target),
        // !(a<=b): b<a or NaN. ucomisd(b,a) then jb (CF). NaN sets CF ⇒ jumps.
        Cmp::Le => dynasm!(ops ; ucomisd xmm1, xmm0 ; jb => target),
        _ => {}
    }
    dynasm!(ops ; => done);
    emit_relational_tail(ops, ip, bail, epilogue, not_number, a, b, cmp, |ops| {
        dynasm!(ops
            ; test eax, eax
            ; jz => target
        );
    });
}

/// `dst = obj[key]` for a present element of a dense Array at an int key,
/// read through the array mirror (`Heap::arr_mirror`) with no call: heap- and
/// int-tag checks, the mirror's bounds, the element's hole tag. Jumps to
/// `done` with `dst` written; anything else (a cleared entry has length 0)
/// falls through to the caller's helper path, which repairs the entry when
/// the store answers alone. An integral double key (`dist[to]` where `to`
/// came from `%`) is an index too. Clobbers rax, rcx, rdx, r9, r10, xmm0 and
/// xmm1, all of which the helper call clobbers anyway.
pub(crate) fn emit_arr_mirror_get(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    obj: u16,
    key: u16,
    done: dynasmrt::DynamicLabel,
) {
    let slow = ops.new_dynamic_label();
    let have_key = ops.new_dynamic_label();
    let arr_raw = crate::vm::host_api::JIT_ARR_MIRROR_RAW_OFFSET as i32;
    let arr_len = crate::vm::host_api::JIT_ARR_MIRROR_LEN_OFFSET as i32;
    dynasm!(ops
        ; mov rax, [rbx + dreg(obj)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, 0x7FFD                    // Value TAG_HEAP >> 48
        ; jne => slow
        ; mov rdx, [rbx + dreg(key)]
        ; mov r10, rdx
        ; shr r10, 48
        ; cmp r10d, 0x7FF9                    // Value TAG_INT >> 48
        ; je >int_key
        // A double with the sign clear (high word below the tags) whose
        // value is a whole number; NaN fails the compare, a negative or a
        // tagged value the range test.
        ; ja => slow
        ; movq xmm0, rdx
        ; cvttsd2si rdx, xmm0
        ; cvtsi2sd xmm1, rdx
        ; ucomisd xmm0, xmm1
        ; jne => slow
        ; jp => slow
        ; jmp => have_key
        ; int_key:
        ; mov edx, edx                        // a negative int is huge
        ; => have_key
        ; mov ecx, eax                        // heap idx (low 32)
        ; cmp ecx, [rdi + arr_len]
        ; jae => slow
        ; mov r9, [rdi + arr_raw]
        ; shl rcx, 4
        ; add r9, rcx                         // the slot's [base, len]
        ; cmp rdx, [r9 + 8]
        ; jae => slow
        ; mov r9, [r9]
        ; mov rax, [r9 + rdx*8]
        ; mov r10, QWORD ARR_HOLE_BITS as i64
        ; cmp rax, r10
        ; je => slow
        ; mov [rbx + dreg(dst)], rax
        ; jmp => done
        ; => slow
    );
}

/// `dst = key in obj` answered `true` for a present element of a dense Array
/// at a non-negative int key, read through the array mirror with no call (an
/// in-range element that is not a hole is an own property, whatever the
/// prototype chain holds). Jumps to `done` with `dst` written; a hole, an
/// index past the entry, a negative or non-int key, a cleared entry and any
/// other receiver fall through to the caller's helper, which proves an
/// absent answer and repairs the entry. Clobbers rax, rcx, rdx, r9 and r10.
pub(crate) fn emit_arr_mirror_has(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    obj: u16,
    key: u16,
    done: dynasmrt::DynamicLabel,
) {
    let slow = ops.new_dynamic_label();
    let arr_raw = crate::vm::host_api::JIT_ARR_MIRROR_RAW_OFFSET as i32;
    let arr_len = crate::vm::host_api::JIT_ARR_MIRROR_LEN_OFFSET as i32;
    dynasm!(ops
        ; mov rax, [rbx + dreg(obj)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, 0x7FFD                    // Value TAG_HEAP >> 48
        ; jne => slow
        ; mov rdx, [rbx + dreg(key)]
        ; mov r10, rdx
        ; shr r10, 48
        ; cmp r10d, 0x7FF9                    // Value TAG_INT >> 48
        ; jne => slow
        ; mov edx, edx                        // a negative int is huge
        ; mov ecx, eax                        // heap idx (low 32)
        ; cmp ecx, [rdi + arr_len]
        ; jae => slow
        ; mov r9, [rdi + arr_raw]
        ; shl rcx, 4
        ; add r9, rcx                         // the slot's [base, len]
        ; cmp rdx, [r9 + 8]
        ; jae => slow
        ; mov r9, [r9]
        ; mov rax, [r9 + rdx*8]
        ; mov r10, QWORD ARR_HOLE_BITS as i64
        ; cmp rax, r10
        ; je => slow
        ; mov r10, QWORD BOOL_TRUE_BITS as i64
        ; mov [rbx + dreg(dst)], r10
        ; jmp => done
        ; => slow
    );
}

/// `dst = obj.length` for a dense Array through its array mirror entry, with
/// no call: heap tag, the mirror's bounds, a live entry (a cleared one is
/// `[0, 0]`; a live one's base is never null, an empty Vec's included), and
/// the entry's length, boxed as an Int. A live entry is the dense store of an
/// Array with no element overlay, no arguments mapping and no virtual length
/// (`jit_get_index` repairs only those, and every write of a virtual length
/// clears the entry), so its length is the JS length. Jumps to `done` with
/// `dst` written; anything else falls through with rax, rcx, r9 and r10
/// clobbered, which the property probe after it reloads or the helpers it
/// leads to clobber anyway.
pub(crate) fn emit_arr_mirror_len(
    ops: &mut dynasmrt::x64::Assembler,
    obj: u16,
    dst: u16,
    done: dynasmrt::DynamicLabel,
) {
    let miss = ops.new_dynamic_label();
    let arr_raw = crate::vm::host_api::JIT_ARR_MIRROR_RAW_OFFSET as i32;
    let arr_len = crate::vm::host_api::JIT_ARR_MIRROR_LEN_OFFSET as i32;
    dynasm!(ops
        ; mov rax, [rbx + dreg(obj)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, 0x7FFD                    // Value TAG_HEAP >> 48
        ; jne => miss
        ; mov ecx, eax                        // heap idx (low 32)
        ; cmp ecx, [rdi + arr_len]
        ; jae => miss
        ; mov r9, [rdi + arr_raw]
        ; shl rcx, 4
        ; add r9, rcx                         // the slot's [base, len]
        ; cmp QWORD [r9], 0
        ; je => miss                          // no live entry
        ; mov rcx, [r9 + 8]
        ; cmp rcx, i32::MAX
        ; ja => miss                          // not an Int
        ; mov rax, QWORD Value::int(0).bits() as i64
        ; or rax, rcx
        ; mov [rbx + dreg(dst)], rax
        ; jmp => done
        ; => miss
    );
}

/// The nursery value barrier after a committed store of a heap value into an
/// object, as `jit_shape_set_barrier` runs it, called only when it can record
/// an edge. With the generation bytes kept (`gen_lane`: the nursery is on and
/// the GC oracle off), a young holder, or a value that is not young, needs
/// none in either barrier mode (`Heap::write_barrier_val`): only a young value
/// stored into an older holder calls. A long-lived array or object taking
/// long-lived values (a priority queue's swaps) called on every store.
/// Expects the holder's heap index in edx and the value's bits in r8, the
/// helper's arguments; jumps to `done` either way, clobbering rcx, r10 and
/// r11 besides the call's volatiles. `ZIPP_NO_BARRIER_FILTER=1` drops the
/// value's test (the identity-way store's holder test predates it).
pub(crate) fn emit_heap_store_barrier(
    ops: &mut dynasmrt::x64::Assembler,
    gen_lane: bool,
    done: dynasmrt::DynamicLabel,
) {
    if gen_lane {
        dynasm!(ops
            ; mov r11, [rdi + crate::vm::host_api::JIT_GEN_RAW_OFFSET as i32]
            ; test BYTE [r11 + rdx], crate::heap::JIT_GEN_STATE_MASK as i8
            ; jz => done                          // a young holder
        );
        if crate::codegen::barrier_filter_enabled() {
            dynasm!(ops
                ; mov r10d, r8d                   // the value's heap index
                ; test BYTE [r11 + r10], crate::heap::JIT_GEN_STATE_MASK as i8
                ; jnz => done                     // a value that is not young
            );
        }
    }
    dynasm!(ops
        ; mov rcx, rdi                            // vm
        ; mov rax, QWORD crate::vm::jit_shape_set_barrier as *const () as i64
        ; call rax
        ; jmp => done
    );
}

/// `obj[key] = val` overwriting a PRESENT element of a dense Array through its
/// array mirror entry, with no call but the nursery barrier's: heap and Int
/// tags, the mirror's bounds, a key below the live entry's length, and an
/// element that is not a hole; the store then commits and, for a heap value
/// only, runs the value-grain barrier (`jit_shape_set_barrier`, infallible).
/// The overwrite is `Heap::array_overwrite_present`'s: it moves no storage
/// and changes nothing that is present, so no snapshot or mirror moves, and
/// an own present element shadows any prototype setter. A live entry is the
/// dense store of an Array with no element overlay (frozen, sealed and
/// `defineProperty`'d arrays have one), no arguments mapping and no virtual
/// length. Jumps to `done`; anything else falls through with rax, rcx, rdx,
/// r8, r9, r10 and r11 clobbered, which the helper after it clobbers anyway.
pub(crate) fn emit_arr_mirror_set(
    ops: &mut dynasmrt::x64::Assembler,
    obj: u16,
    key: u16,
    val: u16,
    gen_lane: bool,
    done: dynasmrt::DynamicLabel,
) {
    let miss = ops.new_dynamic_label();
    let arr_raw = crate::vm::host_api::JIT_ARR_MIRROR_RAW_OFFSET as i32;
    let arr_len = crate::vm::host_api::JIT_ARR_MIRROR_LEN_OFFSET as i32;
    dynasm!(ops
        ; mov rax, [rbx + dreg(obj)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, 0x7FFD                    // Value TAG_HEAP >> 48
        ; jne => miss
        ; mov rdx, [rbx + dreg(key)]
        ; mov r10, rdx
        ; shr r10, 48
        ; cmp r10d, 0x7FF9                    // Value TAG_INT >> 48
        ; jne => miss
        ; mov edx, edx                        // a negative int is huge
        ; mov ecx, eax                        // heap idx (low 32)
        ; cmp ecx, [rdi + arr_len]
        ; jae => miss
        ; mov r9, [rdi + arr_raw]
        ; shl rcx, 4
        ; add r9, rcx                         // the slot's [base, len]
        ; cmp rdx, [r9 + 8]
        ; jae => miss
        ; mov r9, [r9]
        ; mov r10, QWORD ARR_HOLE_BITS as i64
        ; cmp [r9 + rdx * 8], r10
        ; je => miss                          // a hole: not an overwrite
        ; mov r10, [rbx + dreg(val)]
        ; mov [r9 + rdx * 8], r10             // COMMIT
        ; mov r11, r10
        ; shr r11, 48
        ; cmp r11d, 0x7FFD                    // a primitive: no barrier
        ; jne => done
        ; mov edx, eax                        // holder heap idx
        ; mov r8, r10                         // value bits
    );
    emit_heap_store_barrier(ops, gen_lane, done);
    dynasm!(ops ; => miss);
}

/// A for-of step (`IterNext`) over a dense Array, emitted: the helper's first
/// fast path (`jit_iter_next`'s positional walk) through the array mirror,
/// with no call. Taken only with no collection pending and none under
/// stress (the helper's step is the loop's safe point), an iterated Array
/// whose mirror entry is live (a dense store with no element overlay, no
/// arguments mapping and no virtual length, so its length is the store's),
/// and an Int cursor that is not negative. Below the length a present
/// element is the step's value, `done` false and the cursor one further; at
/// the length the step writes `done` true alone, as the helper's exhaustion
/// does. A hole, a dead entry and everything else fall through to the
/// helper with rax, rcx, rdx, r8, r9 and r10 clobbered.
pub(crate) fn emit_arr_mirror_iter_step(
    ops: &mut dynasmrt::x64::Assembler,
    value_dst: u16,
    done_dst: u16,
    iter: u16,
    idx: u16,
    done: dynasmrt::DynamicLabel,
) {
    use crate::vm::host_api::{
        JIT_ARR_MIRROR_LEN_OFFSET, JIT_ARR_MIRROR_RAW_OFFSET, JIT_GC_REQUESTED_OFFSET, JIT_GC_STRESS_OFFSET,
    };
    let miss = ops.new_dynamic_label();
    let exhausted = ops.new_dynamic_label();
    dynasm!(ops
        ; cmp BYTE [rdi + JIT_GC_REQUESTED_OFFSET as i32], 0
        ; jne => miss
        ; cmp BYTE [rdi + JIT_GC_STRESS_OFFSET as i32], 0
        ; jne => miss
        ; mov rax, [rbx + dreg(iter)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, 0x7FFD                    // Value TAG_HEAP >> 48
        ; jne => miss
        ; mov rdx, [rbx + dreg(idx)]
        ; mov r10, rdx
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => miss
        ; test edx, edx
        ; js => miss                          // a negative cursor: the helper's
        ; mov edx, edx
        ; mov ecx, eax                        // heap idx (low 32)
        ; cmp ecx, [rdi + JIT_ARR_MIRROR_LEN_OFFSET as i32]
        ; jae => miss
        ; mov r9, [rdi + JIT_ARR_MIRROR_RAW_OFFSET as i32]
        ; shl rcx, 4
        ; add r9, rcx                         // the slot's [base, len]
        ; mov r8, [r9]
        ; test r8, r8
        ; jz => miss                          // a dead entry ([0, 0])
        ; cmp rdx, [r9 + 8]
        ; jae => exhausted
        ; mov rax, [r8 + rdx * 8]
        ; mov r10, QWORD ARR_HOLE_BITS as i64
        ; cmp rax, r10
        ; je => miss                          // a hole reads the chain
        ; mov [rbx + dreg(value_dst)], rax
        ; mov r10, QWORD BOOL_TAG as i64
        ; mov [rbx + dreg(done_dst)], r10     // false
        ; inc edx
        ; mov r10, QWORD INT_TAG as i64
        ; or rdx, r10
        ; mov [rbx + dreg(idx)], rdx
        ; jmp => done
        ; => exhausted
        ; mov r10, QWORD (BOOL_TAG | 1) as i64
        ; mov [rbx + dreg(done_dst)], r10     // true; the value is left
        ; jmp => done
        ; => miss
    );
}

/// Emit a region op's bail block: the success path skips it; the block records
/// the resume ip into `[rsi]` and jumps to the shared epilogue (which restores
/// the 4-push/40-byte frame). Unlike the function JIT no result is set — a
/// region's `run` ignores rax and reads only the resume ip.
pub(crate) fn emit_region_bail(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
) {
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; jmp => done
        ; => bail
        ; mov DWORD [rsi], ip as i32
        ; jmp => epilogue
        ; => done
    );
}

#[cfg(test)]
mod int32_trunc_tests {
    use super::*;

    /// ECMA-262 ToInt32, written from the specification: truncate toward
    /// zero, reduce modulo 2^32, map into [-2^31, 2^31).
    fn spec_to_int32(x: f64) -> i32 {
        if !x.is_finite() {
            return 0;
        }
        let m = x.trunc().rem_euclid(4294967296.0);
        (if m >= 2147483648.0 {
            m - 4294967296.0
        } else {
            m
        }) as i32
    }

    /// ECMA-262 ToUint32 (the `>>> 0` view of the same residue).
    fn spec_to_uint32(x: f64) -> u32 {
        if !x.is_finite() {
            return 0;
        }
        x.trunc().rem_euclid(4294967296.0) as u32
    }

    /// The reference: the exact result on checked i64, then proven exactly
    /// representable as an f64 (what the generic path holds after `jo`).
    fn exact(a: i32, b: i32, sub: bool) -> f64 {
        let s = if sub {
            (a as i64).checked_sub(b as i64)
        } else {
            (a as i64).checked_add(b as i64)
        }
        .expect("an i32 pair never overflows i64");
        let d = s as f64;
        assert_eq!(d as i64, s, "an i33 result is exact in f64");
        d
    }

    fn check_pair(a: i32, b: i32) {
        let sum = exact(a, b, false);
        let diff = exact(a, b, true);
        assert_eq!(a.wrapping_add(b), spec_to_int32(sum), "({a} + {b}) | 0");
        assert_eq!(a.wrapping_sub(b), spec_to_int32(diff), "({a} - {b}) | 0");
        assert_eq!(
            a.wrapping_add(b) as u32,
            spec_to_uint32(sum),
            "({a} + {b}) >>> 0"
        );
        assert_eq!(
            a.wrapping_sub(b) as u32,
            spec_to_uint32(diff),
            "({a} - {b}) >>> 0"
        );
        assert_eq!(
            a.wrapping_add(b) & 0xffff,
            spec_to_int32(sum) & 0xffff,
            "({a} + {b}) & 65535"
        );
        // The chain rule: residues add, so wrapping an inner Add feeding an
        // outer Add feeding `| 0` is the outer exact result's ToInt32.
        let c = b ^ 0x5bd1_e995;
        let chain = exact(a, b, false) + c as f64;
        assert_eq!((chain as i64) as f64, chain, "an i34 chain is exact in f64");
        assert_eq!(
            a.wrapping_add(b).wrapping_add(c),
            spec_to_int32(chain),
            "({a} + {b} + {c}) | 0"
        );
    }

    /// `(a + b) | 0 == ToInt32(a + b)` for every i32 pair: the edge lattice
    /// exhaustively, then four million random pairs against the checked i64
    /// reference.
    #[test]
    fn wrapping_i32_add_sub_is_to_int32_of_the_exact_result() {
        let edges = [
            i32::MIN,
            i32::MIN + 1,
            -0x4000_0000,
            -1013904223,
            -0x8000,
            -2,
            -1,
            0,
            1,
            2,
            0x7fff,
            1013904223,
            0x4000_0000,
            i32::MAX - 1,
            i32::MAX,
        ];
        for &a in &edges {
            for &b in &edges {
                check_pair(a, b);
            }
        }
        // xorshift64*: deterministic, dependency-free.
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..4_000_000 {
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            let r = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
            check_pair(r as i32, (r >> 32) as i32);
        }
    }

    fn named_proto(src: &str, name: &str) -> FuncProto {
        let ast = crate::front::parse_script(src).expect("parse source");
        crate::compile::compile_program(&ast, src)
            .expect("compile source")
            .functions
            .into_iter()
            .find(|proto| proto.name == name)
            .unwrap_or_else(|| panic!("no function named {name}"))
    }

    fn arith_ips(proto: &FuncProto) -> Vec<usize> {
        (0..proto.code.len())
            .filter(|&ip| {
                matches!(
                    proto.code[ip],
                    Instr::Add { .. } | Instr::Sub { .. } | Instr::AddInt { .. }
                )
            })
            .collect()
    }

    fn member_ips(proto: &FuncProto) -> Vec<usize> {
        let m = trunc_only_arith_ips(proto);
        (0..proto.code.len()).filter(|&ip| m[ip]).collect()
    }

    /// The walks' cuts change no membership: every function of the bench
    /// corpus, plus loops, handlers and finally blocks around truncations,
    /// gets the same members bounded as run to the end.
    #[test]
    fn bounded_walks_match_unbounded() {
        fn js_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    js_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "js") {
                    out.push(p);
                }
            }
        }
        let mut sources = vec![r#"
            function loops(a, b) {
                let s = 0;
                for (let i = 0; i < a; i++) { s = (s + i * b) | 0; let t = s + 1; if (t & 1) continue; s = (t - b) | 0; }
                outer: for (let j = 0; j < b; j++) { for (let k = 0; k < j; k++) { if (k > 3) continue outer; s = (s + k) | 0; } }
                while (s > 0) { s = (s - 7) >>> 1; if (s === 3) break; }
                return (s + a) | 0;
            }
            function handlers(a, b) {
                let s = a + b;
                try { s = (s + 1) | 0; if (a) throw s; } catch (e) { s = (e + s) | 0; } finally { s = (s - b) | 0; }
                let u = a - b;
                try { return u | 0; } finally { s = (s + u) | 0; }
            }
            function late(a) { let r = 0; for (let i = 0; i < a; i++) { if (i > 2) r = (r + a) | 0; a = a + 1; } return r + a; }
            function switches(a, b) { let c = 0; switch (a & 3) { case 0: c = (a + b) | 0; break; case 1: c = a - b; break; default: c = (b + 1) & 255; } return c | 0; }
        "#
        .to_string()];
        // A long generated function of the eval-function bench's shape.
        let mut gen = String::from("function generated(x, y) { let a = x | 0, b = y | 0, c = 7, d = 0;\n");
        let mut seed: u32 = 31337;
        for _ in 0..400 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let k = 1 + (seed >> 8) % 97;
            gen.push_str(match (seed >> 4) % 8 {
                0 => "a = (a + Math.imul(b, 5)) | 0;\n",
                1 => "b = (b ^ (a >>> 3)) | 0;\n",
                2 => "if ((a & 3) === 1) { c = (c + 9) | 0; } else { d = (d - c) | 0; }\n",
                3 => "d = (d + (b & 11) - (c | 1)) | 0;\n",
                4 => "c = (c * 3 + a) & 65535;\n",
                5 => "switch (b & 3) { case 0: a = (a + 4) | 0; break; case 1: b = (b - 4) | 0; break; default: d = (d + 1) | 0; }\n",
                6 => "for (let i = 0; i < 3; i++) { a = (a + i * c) | 0; }\n",
                _ => "b = ((b << 1) | (a & 1)) | 0;\n",
            });
            if k == 1 {
                gen.push_str("try { a = (a + d) | 0; } finally { d = d + 1; }\n");
            }
        }
        gen.push_str("return (a ^ b ^ c ^ d) | 0; }\n");
        sources.push(gen);
        let mut files = Vec::new();
        js_files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench"), &mut files);
        files.sort();
        for f in &files {
            if let Ok(s) = std::fs::read_to_string(f) {
                sources.push(s);
            }
        }
        let (mut protos, mut compared) = (0usize, 0usize);
        for src in &sources {
            let Ok(ast) = crate::front::parse_script(src) else { continue };
            let Ok(prog) = crate::compile::compile_program(&ast, src) else { continue };
            compared += 1;
            for proto in &prog.functions {
                protos += 1;
                assert_eq!(
                    trunc_only_arith_walk(proto, true),
                    trunc_only_arith_walk(proto, false),
                    "function {:?}",
                    proto.name
                );
            }
        }
        assert!(compared > 20 && protos > 500, "{compared} sources, {protos} functions");
    }

    /// Every truncation idiom admits its producer, and a chain admits both.
    #[test]
    fn truncation_only_producers_are_members() {
        if crate::env_var_os("ZIPP_NO_INT32_TRUNC_ADD").is_some() {
            return; // the latch pins the analysis empty by design
        }
        let src = r#"
            function or0(a, b) { return (a + b) | 0; }
            function sub_ushr(a, b) { return (a - b) >>> 0; }
            function and_mask(a, b) { return (a + b) & 65535; }
            function shift_count(a, b) { return 1 << (a + b); }
            function addint(a) { return ((a | 0) + 1013904223) | 0; }
            function local_then_or(a, b) { let s = a + b; return s | 0; }
            function redefined(a, b) { return ((a + b) & 65535) + (a + b); }
        "#;
        for name in [
            "or0",
            "sub_ushr",
            "and_mask",
            "shift_count",
            "addint",
            "local_then_or",
        ] {
            let p = named_proto(src, name);
            assert!(!arith_ips(&p).is_empty(), "{name}: no arith op compiled");
            assert_eq!(
                member_ips(&p),
                arith_ips(&p),
                "{name}: every arith op is truncation-only: {:?}",
                p.code
            );
        }
        // `((a + b) & 65535) + (a + b)`: the FIRST Add feeds only `&` and is
        // then redefined by the second; the second feeds the exact outer Add.
        let p = named_proto(src, "redefined");
        let adds = arith_ips(&p);
        assert_eq!(adds.len(), 3, "{:?}", p.code);
        assert_eq!(member_ips(&p), vec![adds[0]], "{:?}", p.code);
        // `(a + b + c) | 0`: `c` may be a fraction or a string, so the inner
        // Add is observed exactly by the outer one; only the outer wraps. (A
        // literal operand is a `LoadInt` register too, and declines the same.)
        let p = named_proto("function chain(a, b, c) { return (a + b + c) | 0; }", "chain");
        let adds = arith_ips(&p);
        assert_eq!(adds.len(), 2, "{:?}", p.code);
        assert_eq!(member_ips(&p), vec![adds[1]], "{:?}", p.code);
    }

    /// A read that observes the exact value declines the producer: a second
    /// consumer, a loop-carried accumulator (least fixpoint), direct eval.
    #[test]
    fn observed_producers_are_not_members() {
        if crate::env_var_os("ZIPP_NO_INT32_TRUNC_ADD").is_some() {
            return;
        }
        let src = r#"
            function twice(a, b) { let s = a + b; return (s | 0) + s; }
            function carried(n, k) { let t = 0; for (let i = 0; i < n; i++) { t = t + k; } return t | 0; }
            function evaled(a, b) { eval("1"); return (a + b) | 0; }
            function returned(a, b) { return a + b; }
        "#;
        for name in ["twice", "carried", "evaled", "returned"] {
            let p = named_proto(src, name);
            assert!(!arith_ips(&p).is_empty(), "{name}: no arith op compiled");
            assert!(member_ips(&p).is_empty(), "{name}: {:?}", p.code);
        }
    }

    /// The exception edge is over-approximated: a handler that reads the
    /// producer's register declines it even though no ordinary path reaches
    /// the handler; a mapped `arguments` object declines parameter dsts.
    #[test]
    fn handler_edges_and_mapped_arguments_decline() {
        if crate::env_var_os("ZIPP_NO_INT32_TRUNC_ADD").is_some() {
            return;
        }
        let base = named_proto("function f(a, b) { return (a + b) | 0; }", "f");
        let (add_ip, add_dst) = base
            .code
            .iter()
            .enumerate()
            .find_map(|(ip, i)| match *i {
                Instr::Add { dst, .. } => Some((ip, dst)),
                _ => None,
            })
            .expect("an Add");
        assert!(trunc_only_arith_ips(&base)[add_ip]);
        // Trailing `ReturnUndefined` is unreachable by fallthrough (the
        // `Return` before it ends every path). Make it read the Add's dst and
        // reach it only through a handler edge.
        let mut handled = base.clone();
        let last = handled.code.len() - 1;
        assert!(matches!(handled.code[last], Instr::ReturnUndefined));
        handled.code[last] = Instr::Return { src: add_dst };
        assert!(
            trunc_only_arith_ips(&handled)[add_ip],
            "an unreachable exact read is no observer"
        );
        handled.code.insert(
            add_ip + 1,
            Instr::PushHandler {
                catch_target: (last + 1) as u32,
                catch_reg: add_dst + 1,
            },
        );
        assert!(
            !trunc_only_arith_ips(&handled)[add_ip],
            "the handler-reachable exact read declines: {:?}",
            handled.code
        );
        // Parameter register as dst with an arguments object present.
        let mut mapped = base.clone();
        let Instr::Add { a, b, .. } = mapped.code[add_ip] else {
            unreachable!()
        };
        mapped.code[add_ip] = Instr::Add { dst: 1, a, b };
        for i in &mut mapped.code[add_ip + 1..] {
            if let Instr::Bitwise { a, .. } = i {
                if *a == add_dst {
                    *a = 1;
                }
            }
        }
        assert!(trunc_only_arith_ips(&mapped)[add_ip], "{:?}", mapped.code);
        mapped.arguments_reg = Some(mapped.reg_count - 1);
        assert!(!trunc_only_arith_ips(&mapped)[add_ip], "{:?}", mapped.code);
    }
}
