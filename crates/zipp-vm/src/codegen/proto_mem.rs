// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Mark explicit returns whose bytecode lies in a structured `PushFinally`
/// protected interval. The compiler lays each protected body out strictly
/// between its push and the forward handler target; the target itself runs
/// after that handler has been popped. Nested intervals naturally raise the
/// depth above one.
///
/// This is deliberately a compile-time map: unprotected returns retain their
/// byte-identical `NO_BAIL` epilogue, while a protected return can hand its
/// exact instruction back to the interpreter to perform completion routing.
/// The difference sweep is linear even for hostile deeply-nested source.
fn tierc_protected_return_map(code: &[Instr]) -> Vec<bool> {
    let mut depth_delta = vec![0i32; code.len() + 1];
    for (push_ip, instr) in code.iter().enumerate() {
        let Instr::PushFinally { target, .. } = *instr else {
            continue;
        };
        let start = push_ip + 1;
        let end = (target as usize).min(code.len());
        if start < end {
            depth_delta[start] += 1;
            depth_delta[end] -= 1;
        }
    }

    let mut depth = 0i32;
    let mut protected = vec![false; code.len()];
    for (ip, instr) in code.iter().enumerate() {
        depth += depth_delta[ip];
        debug_assert!(depth >= 0, "unbalanced structured finally intervals");
        protected[ip] =
            depth != 0 && matches!(instr, Instr::Return { .. } | Instr::ReturnUndefined);
    }
    protected
}

env_off_switch!(
    /// Defer flat-ASCII builder metadata across a compiler-proved call-free
    /// Tier-C loop. `ZIPP_NO_STR_APPEND_CURSOR=1` restores one
    /// `jit_str_append_index_ascii` boundary crossing per appended character.
    fn str_append_cursor_enabled() = "ZIPP_NO_STR_APPEND_CURSOR"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TiercStrAppendCursorPlan {
    append_ip: usize,
    acc: u16,
    obj: u16,
    key: u16,
}

/// A cursor may carry the mutable builder only in its canonical accumulator
/// register. The local-accumulator lowering already proves its in-loop
/// statement-result copies dead, but cursor lifetime extends through the
/// function epilogue, where post-loop bytecode is otherwise unrestricted.
/// Re-run the compiler's whole-function dominating-write proof for every
/// non-self copy so no copied builder bits can reach a later read, including a
/// truthiness helper, another Move, a store, or Return.
fn tierc_str_append_cursor_moves_safe(code: &[Instr], header: usize, acc: u16) -> bool {
    let mut targets = vec![false; code.len()];
    for instr in code {
        if let Some(target) = bytecode_control_target(instr).map(|target| target as usize) {
            if target < targets.len() {
                targets[target] = true;
            }
        }
    }
    for (ip, instr) in code.iter().enumerate().skip(header) {
        if let Instr::Move { dst, src } = *instr {
            if src == acc
                && dst != acc
                && !crate::compile::write_dst_unobservable(code, &targets, dst, ip, None)
            {
                return false;
            }
        }
    }
    true
}

/// Once a cursor is active, native control may not leave the audited
/// `header..code.len()` span except through Return/fallthrough and the shared
/// epilogue. The selected unconditional loop back-edge is the sole permitted
/// backward edge; in particular a backward conditional must not jump into
/// unaudited bytecode while raw bytes are still unpublished.
fn tierc_str_append_cursor_flow_closed(code: &[Instr], header: usize, backedge: usize) -> bool {
    for (ip, instr) in code.iter().enumerate().skip(header) {
        let Some(target) = bytecode_control_target(instr).map(|target| target as usize) else {
            continue;
        };
        if target < header || target > code.len() {
            return false;
        }
        if target < ip
            && !(ip == backedge && target == header && matches!(instr, Instr::Jump { .. }))
        {
            return false;
        }
    }
    true
}

/// Admit one deferred ASCII append cursor only when the bytecode and the
/// already-selected Tier-C reductions prove its whole live span contains no
/// allocation, helper/user call, or observation of the builder's contents.
///
/// The sole nontrivial allowance is a B205 random-scale head: its covered
/// `Math.random()*k|0` source window contains a CallMethod in bytecode, but the
/// emitter replaces that entire window with guarded xorshift arithmetic. Any
/// guard miss exits through the shared epilogue before interpreter replay.
fn tierc_str_append_cursor_plan(
    proto: &FuncProto,
    random_fuse: &FxHashMap<usize, crate::codegen::RandomScaleFusePlan>,
    random_fuse_covered: &[bool],
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<TiercStrAppendCursorPlan> {
    if !str_append_cursor_enabled() || meter.is_some() || proto.code.is_empty() {
        return None;
    }
    let mut found = None;
    for (ip, instr) in proto.code.iter().enumerate() {
        if let Instr::StrAppendIndex {
            dst, a, obj, key, ..
        } = *instr
        {
            if found.is_some() || dst != a || obj == a || key == a {
                return None;
            }
            found = Some(TiercStrAppendCursorPlan {
                append_ip: ip,
                acc: a,
                obj,
                key,
            });
        }
    }
    let plan = found?;

    // The append must be inside one explicit back-edge. Starting the proof at
    // that header covers every later iteration after the cursor becomes live;
    // extending it to function end covers all loop exits before Return.
    let (header, backedge) = proto
        .code
        .iter()
        .enumerate()
        .skip(plan.append_ip + 1)
        .find_map(|(ip, instr)| match *instr {
            Instr::Jump { target }
                if (target as usize) <= plan.append_ip && plan.append_ip <= ip =>
            {
                Some((target as usize, ip))
            }
            _ => None,
        })?;

    if !tierc_str_append_cursor_flow_closed(&proto.code, header, backedge)
        || !tierc_str_append_cursor_moves_safe(&proto.code, header, plan.acc)
    {
        return None;
    }

    for ip in header..proto.code.len() {
        if random_fuse_covered.get(ip).copied().unwrap_or(false) {
            continue;
        }
        if random_fuse.contains_key(&ip)
            && random_fuse_covered.get(ip + 1).copied().unwrap_or(false)
        {
            continue;
        }
        let instr = &proto.code[ip];

        // `Move` only transports Value bits. The independent whole-function
        // check above proves every non-self accumulator copy is overwritten
        // before any read, so no copied builder alias can reach these uses.
        if instr_uses(instr).contains(&plan.acc)
            && !matches!(
                instr,
                Instr::StrAppendIndex { .. } | Instr::Move { .. } | Instr::Return { .. }
            )
        {
            return None;
        }
        let allowed_acc_write = matches!(instr, Instr::StrAppendIndex { .. })
            || matches!(instr, Instr::Move { dst, src } if dst == src);
        if writes_reg(instr) == Some(plan.acc) && !allowed_acc_write {
            return None;
        }

        // Closed call-free emission set. Every runtime type mismatch in these
        // arms jumps directly to the epilogue; none calls a helper before it.
        let pure = matches!(
            instr,
            Instr::LoadInt { .. }
                | Instr::LoadBool { .. }
                | Instr::LoadNull { .. }
                | Instr::LoadUndefined { .. }
                | Instr::LoadConst { .. }
                | Instr::LoadGlobal { .. }
                | Instr::Move { .. }
                | Instr::AddInt { .. }
                | Instr::Mul { .. }
                | Instr::Bitwise { .. }
                | Instr::Gt { .. }
                | Instr::Jump { .. }
                | Instr::JumpIfFalse { .. }
                | Instr::StrAppendIndex { .. }
                | Instr::Return { .. }
                | Instr::ReturnUndefined
        ) || matches!(instr, Instr::UpvalGet { .. }) && tierc_upval_inline_enabled();
        if !pure {
            return None;
        }
    }
    Some(plan)
}

#[cfg(test)]
mod str_append_cursor_proof_tests {
    use super::*;

    #[test]
    fn copied_accumulator_must_be_overwritten_before_any_read() {
        let observable = vec![
            Instr::Move { dst: 3, src: 2 },
            Instr::JumpIfFalse { cond: 3, target: 3 },
            Instr::Return { src: 2 },
            Instr::Return { src: 2 },
        ];
        assert!(!tierc_str_append_cursor_moves_safe(&observable, 0, 2));

        let overwritten = vec![
            Instr::Move { dst: 3, src: 2 },
            Instr::LoadInt { dst: 3, val: 1 },
            Instr::JumpIfFalse { cond: 3, target: 3 },
            Instr::Return { src: 2 },
        ];
        assert!(tierc_str_append_cursor_moves_safe(&overwritten, 0, 2));
    }

    #[test]
    fn conditional_backedge_cannot_escape_the_audited_span() {
        let closed = vec![
            Instr::LoadInt { dst: 1, val: 1 },
            Instr::JumpIfFalse { cond: 1, target: 3 },
            Instr::Jump { target: 0 },
            Instr::Return { src: 1 },
        ];
        assert!(tierc_str_append_cursor_flow_closed(&closed, 0, 2));

        let escaping = vec![
            Instr::LoadInt { dst: 1, val: 1 },
            Instr::JumpIfFalse { cond: 1, target: 0 },
            Instr::Jump { target: 1 },
            Instr::Return { src: 1 },
        ];
        assert!(!tierc_str_append_cursor_flow_closed(&escaping, 1, 2));
    }
}

/// Admit numeric remainder into Tier C.  The emitted prefix handles only
/// integer-valued Number operands; zero divisors, fractional values, BigInts,
/// and observable ToNumeric coercions decline before doing any work and resume
/// at the original bytecode.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_mod_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_MOD").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Admit RequireObjectCoercible into Tier C.  It is a no-op for every value
/// except null/undefined; those two exact bit patterns exit before effects so
/// the interpreter remains responsible for constructing and throwing the
/// TypeError.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_check_coercible_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_CHECK_COERCIBLE").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Admit the two non-callback Map mutations used by warm application code.
/// Generated code still proves the live receiver resolves to the main-realm
/// intrinsic before the helper performs any mutation.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
pub(crate) fn tierc_coll_mutate_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_COLL_MUTATE").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Admit guarded primitive-string `toUpperCase()` into Tier C.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_string_upper_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_STRING_UPPER").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Admit the dynamically-resolved zero-argument `random` method into Tier C.
/// This deliberately remains a `CallMethod`: the helper re-reads the live
/// property on every IC miss, so replacing `Math.random` is still observable.
/// The same-binary switch isolates the whole-function-entry benefit from the
/// arrow-local/string changes used by the NanoID workload.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_random_method_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_RANDOM_METHOD").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Native-to-native prefix for an admitted Tier-C `CallMethod`. The helper
/// resolves the live data property through the interpreter IC and declines to
/// the unchanged generic call helper unless the current callee has a Tier-C
/// entry. Kept separately switchable from admission for direct A/B evidence.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_method_crosscall_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_METHOD_CROSSCALL").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Bounded own-data `CallMethod` prefix: consume an already-filled guardable
/// `OwnData { shape, slot }` IC way without cloning/hashing the property name.
/// The runtime helper revalidates exact shape/slot/key/descriptor and re-reads
/// the live callee before delegating to the unchanged Tier-C cross-call.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_method_own_slot_direct_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_METHOD_OWN_SLOT_DIRECT").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

env_off_switch! {
    /// Inline a root-realm own-data method whose closed numeric body accesses
    /// directly-routable globals through the transactional typed lane.
    /// `ZIPP_NO_TIERC_METHOD_GLOBAL_INLINE=1` restores the unchanged live
    /// method-cross-call path for same-binary mechanism measurements.
    fn tierc_method_global_inline_enabled() = "ZIPP_NO_TIERC_METHOD_GLOBAL_INLINE"
}

env_off_switch! {
    /// Scalar-replace a fresh one-step object/array literal when every use is
    /// an exact projection in one return-ending Tier-C basic block. Metered and
    /// GC-stress VMs decline separately; `ZIPP_NO_TIERC_BLOCK_SROA=1` restores
    /// the ordinary allocation/property helpers for a same-binary A/B.
    fn tierc_block_sroa_enabled() = "ZIPP_NO_TIERC_BLOCK_SROA"
}

/// Admit unary numeric negation into Tier C. The emitted path flips the f64
/// sign bit (preserving JavaScript's `-(+0) === -0`) and side-effectlessly
/// declines non-numeric operands to the exact interpreter instruction.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_neg_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_NEG").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Admit a statically named property delete as a deliberately cold exact-ip
/// exit from Tier C.  The common path keeps the surrounding function native;
/// if execution reaches the delete, generated code performs no part of the
/// operation and resumes the interpreter at that bytecode.  This preserves
/// ToObject, configurability, strict-mode throwing and proxy semantics without
/// duplicating any of them in native code.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_cold_delete_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_COLD_DELETE").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Forward an immediately preceding captured read/write into the next read of
/// the same upvalue. There is no call, allocation, branch target, or other VM
/// action between the two bytecodes, so the source register and the cell must
/// still contain identical `Value` bits. This removes one win64 helper crossing
/// per hostile-pipeline activation while retaining an exact-ip TDZ guard.
/// `ZIPP_NO_TIERC_UPVAL_FORWARD=1` restores the helper call for A/B evidence.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_upval_forward_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_UPVAL_FORWARD").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Fuse the compiler's exact captured-counter lowering
/// `get; 1; add; 0; or; set` through one resolved-cell helper. Disabled for a
/// metered VM by the planner below: skipping five bytecodes must never reduce a
/// sandbox's instruction charge. `ZIPP_NO_TIERC_UPVAL_INC_I32=1` restores the
/// unfused sequence for a same-binary A/B.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_upval_inc_i32_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_UPVAL_INC_I32").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// B189: emit `UpvalGet` as three inline loads (activation upvalue base →
/// cell index → cell-value mirror) instead of the resolving helper call.
/// `ZIPP_NO_TIERC_UPVAL_INLINE=1` restores the helper for a same-binary A/B.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_upval_inline_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_UPVAL_INLINE").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Fuse a bounded straight-line chain of captured i32 xorshift assignments.
/// This is deliberately a bytecode-shape optimization, not a PRNG intrinsic:
/// every accepted step may use an arbitrary constant count and any of `<<`,
/// `>>`, or `>>>` in the generic expression `x ^= x SHIFT count`.
///
/// The planner declines under instruction metering because one helper replaces
/// several bytecodes. `ZIPP_NO_TIERC_UPVAL_XORSHIFT=1` keeps a same-binary
/// unfused route for performance and semantic differential testing.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_upval_xorshift_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_UPVAL_XORSHIFT").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Direct-slot counterpart used by module/top-level lexical bindings. It has a
/// distinct switch because a real workload may exercise the global shape while
/// a closure microbenchmark exercises only the captured-cell helper.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn tierc_global_xorshift_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_TIERC_GLOBAL_XORSHIFT").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
struct TiercUpvalIncI32 {
    idx: u16,
    old_dst: u16,
    one_dst: u16,
    add_dst: u16,
    zero_dst: u16,
    new_dst: u16,
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn tierc_upval_inc_i32_at(code: &[Instr], ip: usize) -> Option<TiercUpvalIncI32> {
    let [Instr::UpvalGet { dst: old_dst, idx }, Instr::LoadInt {
        dst: one_dst,
        val: 1,
    }, Instr::Add {
        dst: add_dst,
        a: add_a,
        b: add_b,
    }, Instr::LoadInt {
        dst: zero_dst,
        val: 0,
    }, Instr::Bitwise {
        dst: new_dst,
        a: or_a,
        b: or_b,
        op: crate::bytecode::BitwiseOp::Or,
    }, Instr::UpvalSet { idx: set_idx, src }] = code.get(ip..ip.checked_add(6)?)?
    else {
        return None;
    };
    if old_dst == one_dst
        || add_dst == zero_dst
        || !((*add_a == *old_dst && *add_b == *one_dst)
            || (*add_b == *old_dst && *add_a == *one_dst))
        || !((*or_a == *add_dst && *or_b == *zero_dst) || (*or_b == *add_dst && *or_a == *zero_dst))
        || set_idx != idx
        || src != new_dst
    {
        return None;
    }
    Some(TiercUpvalIncI32 {
        idx: *idx,
        old_dst: *old_dst,
        one_dst: *one_dst,
        add_dst: *add_dst,
        zero_dst: *zero_dst,
        new_dst: *new_dst,
    })
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const TIER_C_XORSHIFT_MAX_STEPS: usize = 8;

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
struct TiercUpvalXorShiftStep {
    first_dst: u16,
    second_dst: u16,
    amount_dst: u16,
    shifted_dst: u16,
    result_dst: u16,
    /// 0 = Shl, 1 = Shr, 2 = Ushr. Kept compact for the helper plan word.
    kind: u8,
    amount_value: i32,
    amount: u8,
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const EMPTY_TIER_C_XORSHIFT_STEP: TiercUpvalXorShiftStep = TiercUpvalXorShiftStep {
    first_dst: 0,
    second_dst: 0,
    amount_dst: 0,
    shifted_dst: 0,
    result_dst: 0,
    kind: 0,
    amount_value: 0,
    amount: 0,
};

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
struct TiercUpvalXorShift {
    idx: u16,
    count: u8,
    /// Low nibble is the count; each following seven-bit instruction is
    /// `(amount << 2) | kind`. This fits eight generic steps in one u64 arg.
    packed: u64,
    steps: [TiercUpvalXorShiftStep; TIER_C_XORSHIFT_MAX_STEPS],
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn tierc_upval_xorshift_step_at(
    code: &[Instr],
    ip: usize,
) -> Option<(u16, TiercUpvalXorShiftStep)> {
    use crate::bytecode::BitwiseOp as B;
    let [Instr::UpvalGet {
        dst: first_dst,
        idx,
    }, Instr::UpvalGet {
        dst: second_dst,
        idx: second_idx,
    }, Instr::LoadInt {
        dst: amount_dst,
        val: amount,
    }, Instr::Bitwise {
        dst: shifted_dst,
        a: shift_a,
        b: shift_b,
        op: shift_op,
    }, Instr::Bitwise {
        dst: result_dst,
        a: xor_a,
        b: xor_b,
        op: B::Xor,
    }, Instr::UpvalSet {
        idx: set_idx,
        src: set_src,
    }] = code.get(ip..ip.checked_add(6)?)?
    else {
        return None;
    };
    let kind = match shift_op {
        B::Shl => 0,
        B::Shr => 1,
        B::Ushr => 2,
        _ => return None,
    };
    // These exclusions prove that the generic algebraic value used by the
    // helper is also the value the register machine would read after its
    // preceding destination writes. Other aliases are harmless and are
    // materialized in exact bytecode order by the emitter.
    if second_idx != idx
        || set_idx != idx
        || set_src != result_dst
        || shift_a != second_dst
        || shift_b != amount_dst
        || !((*xor_a == *first_dst && *xor_b == *shifted_dst)
            || (*xor_b == *first_dst && *xor_a == *shifted_dst))
        || second_dst == amount_dst
        || first_dst == amount_dst
        || first_dst == shifted_dst
    {
        return None;
    }
    Some((
        *idx,
        TiercUpvalXorShiftStep {
            first_dst: *first_dst,
            second_dst: *second_dst,
            amount_dst: *amount_dst,
            shifted_dst: *shifted_dst,
            result_dst: *result_dst,
            kind,
            amount_value: *amount,
            amount: (*amount as u32 & 31) as u8,
        },
    ))
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn tierc_upval_xorshift_at(
    code: &[Instr],
    targeted: &[bool],
    ip: usize,
) -> Option<TiercUpvalXorShift> {
    let mut plan = TiercUpvalXorShift {
        idx: 0,
        count: 0,
        packed: 0,
        steps: [EMPTY_TIER_C_XORSHIFT_STEP; TIER_C_XORSHIFT_MAX_STEPS],
    };
    for slot in 0..TIER_C_XORSHIFT_MAX_STEPS {
        let at = ip.checked_add(slot.checked_mul(6)?)?;
        let Some((idx, step)) = tierc_upval_xorshift_step_at(code, at) else {
            break;
        };
        if slot != 0 && idx != plan.idx {
            break;
        }
        let end = at.checked_add(6)?;
        // The first label remains a legal entry into the fused plan. Every
        // other skipped label, including a later chained step's first read,
        // must be unreachable from an internal edge.
        let check_from = at + usize::from(slot == 0);
        if targeted.get(check_from..end)?.iter().any(|&target| target) {
            break;
        }
        if slot == 0 {
            plan.idx = idx;
        }
        plan.steps[slot] = step;
        plan.count += 1;
        plan.packed |= ((step.kind as u64) | ((step.amount as u64) << 2)) << (4 + slot * 7);
    }
    if plan.count == 0 {
        None
    } else {
        plan.packed |= plan.count as u64;
        Some(plan)
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
struct TiercGlobalXorShift {
    idx: u32,
    count: u8,
    steps: [TiercUpvalXorShiftStep; TIER_C_XORSHIFT_MAX_STEPS],
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn tierc_global_xorshift_step_at(
    code: &[Instr],
    ip: usize,
) -> Option<(u32, TiercUpvalXorShiftStep)> {
    use crate::bytecode::BitwiseOp as B;
    let [Instr::LoadGlobal {
        dst: first_dst,
        idx,
    }, Instr::LoadGlobal {
        dst: second_dst,
        idx: second_idx,
    }, Instr::LoadInt {
        dst: amount_dst,
        val: amount,
    }, Instr::Bitwise {
        dst: shifted_dst,
        a: shift_a,
        b: shift_b,
        op: shift_op,
    }, Instr::Bitwise {
        dst: result_dst,
        a: xor_a,
        b: xor_b,
        op: B::Xor,
    }, Instr::StoreGlobal {
        idx: set_idx,
        src: set_src,
    }
    | Instr::StoreGlobalStrict {
        idx: set_idx,
        src: set_src,
    }
    | Instr::StoreGlobalResolved {
        idx: set_idx,
        src: set_src,
    }] = code.get(ip..ip.checked_add(6)?)?
    else {
        return None;
    };
    let kind = match shift_op {
        B::Shl => 0,
        B::Shr => 1,
        B::Ushr => 2,
        _ => return None,
    };
    if second_idx != idx
        || set_idx != idx
        || set_src != result_dst
        || shift_a != second_dst
        || shift_b != amount_dst
        || !((*xor_a == *first_dst && *xor_b == *shifted_dst)
            || (*xor_b == *first_dst && *xor_a == *shifted_dst))
        || second_dst == amount_dst
        || first_dst == amount_dst
        || first_dst == shifted_dst
    {
        return None;
    }
    Some((
        *idx,
        TiercUpvalXorShiftStep {
            first_dst: *first_dst,
            second_dst: *second_dst,
            amount_dst: *amount_dst,
            shifted_dst: *shifted_dst,
            result_dst: *result_dst,
            kind,
            amount_value: *amount,
            amount: (*amount as u32 & 31) as u8,
        },
    ))
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn tierc_global_xorshift_at(
    code: &[Instr],
    targeted: &[bool],
    ip: usize,
) -> Option<TiercGlobalXorShift> {
    let mut plan = TiercGlobalXorShift {
        idx: 0,
        count: 0,
        steps: [EMPTY_TIER_C_XORSHIFT_STEP; TIER_C_XORSHIFT_MAX_STEPS],
    };
    for slot in 0..TIER_C_XORSHIFT_MAX_STEPS {
        let at = ip.checked_add(slot.checked_mul(6)?)?;
        let Some((idx, step)) = tierc_global_xorshift_step_at(code, at) else {
            break;
        };
        if slot != 0 && idx != plan.idx {
            break;
        }
        let end = at.checked_add(6)?;
        let check_from = at + usize::from(slot == 0);
        if targeted.get(check_from..end)?.iter().any(|&target| target) {
            break;
        }
        if slot == 0 {
            plan.idx = idx;
        }
        plan.steps[slot] = step;
        plan.count += 1;
    }
    (plan.count != 0).then_some(plan)
}

/// Starting with a tagged Int in RAX, reconstruct every destination written by
/// a recognized xorshift chain in exact bytecode order. The raw current i32 is
/// kept in R11D. The final boxed result remains in RAX for a direct global-slot
/// commit (the captured-cell helper has already committed its copy).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn emit_tierc_xorshift_registers(
    ops: &mut dynasmrt::x64::Assembler,
    steps: &[TiercUpvalXorShiftStep],
) {
    dynasm!(ops ; mov r11d, eax);
    for (step_no, &step) in steps.iter().enumerate() {
        if step_no == 0 {
            dynasm!(ops
                ; mov [rbx + dreg(step.first_dst)], rax
                ; mov [rbx + dreg(step.second_dst)], rax
            );
        } else {
            dynasm!(ops ; mov eax, r11d);
            box_eax(ops, step.first_dst);
            dynasm!(ops
                ; mov rax, [rbx + dreg(step.first_dst)]
                ; mov [rbx + dreg(step.second_dst)], rax
            );
        }
        let amount_bits = Value::int(step.amount_value).bits();
        dynasm!(ops
            ; mov rax, QWORD amount_bits as i64
            ; mov [rbx + dreg(step.amount_dst)], rax
            ; mov eax, r11d
            ; mov ecx, step.amount as i32
        );
        match step.kind {
            0 => dynasm!(ops ; shl eax, cl),
            1 => dynasm!(ops ; sar eax, cl),
            2 => dynasm!(ops ; shr eax, cl),
            _ => unreachable!("validated Tier-C xorshift kind"),
        }
        dynasm!(ops ; mov r10d, eax);
        if step.kind == 2 {
            let as_dbl = ops.new_dynamic_label();
            let done_u = ops.new_dynamic_label();
            dynasm!(ops
                ; test eax, eax
                ; js => as_dbl
            );
            box_eax(ops, step.shifted_dst);
            dynasm!(ops
                ; jmp => done_u
                ; => as_dbl
                ; mov eax, eax
                ; cvtsi2sd xmm0, rax
                ; movq rax, xmm0
                ; mov [rbx + dreg(step.shifted_dst)], rax
                ; => done_u
            );
        } else {
            box_eax(ops, step.shifted_dst);
        }
        dynasm!(ops
            ; xor r11d, r10d
            ; mov eax, r11d
        );
        box_eax(ops, step.result_dst);
    }
}

/// Tier C eligibility: is every op of `proto` in the v1 whole-function mem-path
/// subset? Stricter than `region_can_compile` (no GetProp/SetProp/StrConcat/
/// MathOp/Bitwise/Cell/etc. yet — those are later increments). Rejects
/// generators/async, rest/`arguments` (materialized by call setup, not emitted
/// code), and any op the emitter below doesn't implement.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn mem_can_compile(proto: &FuncProto, const_strs: &FxHashMap<u32, u64>) -> bool {
    if proto.code.is_empty() {
        return false;
    }
    if proto.is_generator || proto.is_async {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[tierC-reject] generator/async");
        }
        return false;
    }
    // A rest parameter's array / the `arguments` object are built by the
    // interpreter's call setup, not by emitted code — the native entry would skip
    // them. Stay interpreted.
    if proto.rest_reg.is_some() || proto.arguments_reg.is_some() {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[tierC-reject] rest/arguments");
        }
        return false;
    }
    // B50 established that crossing the helper boundary for a tiny closure
    // whose body is mostly one captured read/write is slower than interpreting
    // it. Keep this lane deliberately bounded to medium bodies: at least twelve
    // OTHER ops must be rescued from dispatch. The hostile closure body has 34;
    // the old one/two-op probes remain blacklisted.
    let upval_ops = proto
        .code
        .iter()
        .filter(|ins| matches!(ins, Instr::UpvalGet { .. } | Instr::UpvalSet { .. }))
        .count();
    let tierc_upval_ok = upval_ops != 0
        && crate::codegen::tierc_upval_enabled()
        && proto.code.len().saturating_sub(upval_ops)
            >= crate::codegen::tierc_upval_min_other_ops();
    // Under `ZIPP_JITDUMP` the scan runs to completion and reports EVERY op this
    // tier has no arm for, instead of stopping at the first. Reporting only the
    // first is actively misleading when prioritising: admitting `UpvalGet` here
    // moved markdown-render's blacklist count by exactly zero, because the same
    // three functions were also using `UpvalSet`, `join` and `push` — which the
    // first-only report had never shown. Behaviour is unchanged when the flag is
    // off.
    let dump = std::env::var_os("ZIPP_JITDUMP").is_some();
    let mut ok = true;
    macro_rules! reject {
        ($($arg:tt)*) => {{
            if dump {
                eprintln!($($arg)*);
                ok = false;
            } else {
                if std::env::var_os("ZIPP_JITLOG").is_some() { eprintln!($($arg)*); }
                return false;
            }
        }};
    }
    for (ip, instr) in proto.code.iter().enumerate() {
        match *instr {
            Instr::LoadInt { .. }
            | Instr::LoadBool { .. }
            | Instr::LoadNull { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. }
            | Instr::GetIndex { .. }
            | Instr::GetProp { .. }
            // `o.x = v` — the region's 8-way IC write, minus the setter-inline
            // prefix and the TA refetch (Tier C has neither). The IC site budget
            // already counted SetProp (`compile`'s `n_sites` filter and the
            // desync assertion at the end of this function both name it), so the
            // op was gated out one line short of working. It is
            // class-prototype-hot's ONLY blacklisted function. A strict-FORCED
            // write (strict ClassTail region in a sloppy function) declines:
            // the slow helper reads strictness off the proto flag.
            | Instr::SetProp { strict: false, .. }
            | Instr::AddInt { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::TypeOf { .. }
            | Instr::TypeOfIs { .. }
            | Instr::TypeOfSame { .. }
            | Instr::IsArray { .. }
            | Instr::LenOf { .. }
            | Instr::ForInKeys { .. }
            | Instr::ForInLive { .. }
            | Instr::Jump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotLe { .. }
            | Instr::Return { .. }
            | Instr::ReturnUndefined
            // Bitwise / `!` — self-contained register ops with the same
            // ToInt32-or-bail contract the region path uses. Their absence here
            // was the single most common Tier C rejection across the benches
            // (10 functions over three of them, tied with UpvalGet), and it is
            // silent: the whole function is blacklisted and INTERPRETED for the
            // rest of the run, so one `h ^= h << 13` costs the entire body.
            | Instr::Bitwise { .. }
            | Instr::Not { .. }
            // `undefined` as a constant — a single store of the canonical bits,
            // the exact shape of the `LoadNull` arm right above. It was in
            // NEITHER admission list, which is what kept map-set-heavy's
            // [39,110] region interpreted: three `LoadUndefined`s were its only
            // remaining blocker on the mem path.
            | Instr::LoadUndefined { .. }
            // `/` — `dbinop` handles it on the always-f64 path (JS `/` has no
            // integer form), the same call the region path makes.
            | Instr::Div { .. }
            // In-place string append — B56's local-accumulator rewrite now
            // plants these INSIDE function bodies (markdown's escapeHtml and
            // renderInline), so Tier C rejecting the op un-compiled functions
            // it previously compiled. Same helper protocol as the region arm;
            // allocates (grows the heap) ⇒ the emitter refetches r13/r14 when
            // the function pins them.
            | Instr::StrAppendInPlace { .. }
            | Instr::StrAppendIndex { .. }
            | Instr::AddRightPair { .. }
            | Instr::Pad2Concat { .. }
            | Instr::Pad2Conditional { .. }
            // W11 (B124) fused chain link — same helper protocol as the two
            // ops above (`jit_concat_chain`). Tier C NEEDS it: the fusion
            // plants chains INSIDE function bodies Tier C compiles today
            // (markdown-render's span()/block builders), so rejecting the op
            // would un-compile them — the exact B56 regression mode.
            | Instr::StrConcatChain { .. } => {}
            Instr::Mod { .. } => {
                if !tierc_mod_enabled() {
                    reject!("[tierC-reject] op Mod (disabled)");
                }
            }
            Instr::CheckCoercible { .. } => {
                if !tierc_check_coercible_enabled() {
                    reject!("[tierC-reject] op CheckCoercible (disabled)");
                }
            }
            // Static object literals: allocate a fresh ordinary object and
            // append only compiler-proved-new data properties.  The helpers
            // preserve current-realm provenance and decline exotic/malformed
            // inputs before mutation.  Kept independently ablatable because
            // each append crosses the helper ABI.
            Instr::NewObject { .. }
            | Instr::NewPlannedObject { .. }
            | Instr::AppendDataProp { .. }
            | Instr::FinalizeObject { .. } => {
                if !crate::vm::tierc_object_literal_enabled() {
                    reject!("[tierC-reject] object literal op (disabled)");
                }
            }
            Instr::NewArray { .. } => {
                if !crate::vm::tierc_new_array_enabled() {
                    reject!("[tierC-reject] op NewArray (disabled)");
                }
            }
            // Capture-free ordinary function literals and their method-home
            // side-table write.  Runtime helpers revalidate the immutable
            // MakeFunc site plus the exact active callee before any effect.
            Instr::MakeFunc { .. } | Instr::SetHomeObject { .. } => {
                if !crate::vm::tierc_makefunc_home_enabled() {
                    reject!("[tierC-reject] MakeFunc/SetHomeObject (disabled)");
                }
            }
            // Closure-CREATING ops: capture cells and real closures/arrows.
            // The helpers revalidate the immutable site against the exact
            // active callable and replicate the interpreter's lexical
            // inheritance (cells, `this`, `[[HomeObject]]`, `new.target`,
            // EvalScope, realm); every decline is a pure prefix. This is what
            // lets application-shaped render/handler code compile at all — a
            // React-style component allocates a handler arrow per item.
            Instr::MakeCell { .. }
            | Instr::MakeCellTdz { .. }
            | Instr::MakeCellFnName { .. }
            | Instr::MarkCellConst { .. }
            | Instr::MakeClosure { .. }
            | Instr::MakeArrow { .. }
            // Direct cell reads/writes in the CREATING function (a boxed local
            // is accessed through its cell once captured). Pure helpers; a TDZ
            // read/write declines so the interpreter throws its exact error.
            | Instr::CellGet { .. }
            | Instr::CellSet { .. }
            | Instr::CellSetChecked { .. } => {
                if !crate::vm::tierc_closure_make_enabled() {
                    reject!("[tierC-reject] closure-make op (disabled)");
                }
            }
            // Generic element write `a[i] = v` — the region path's exact
            // `jit_set_index` helper (dense store/grow, unpinned TypedArrays);
            // anything needing observable coercion or exotic receivers deopts.
            Instr::SetIndex { .. } => {
                if !crate::vm::tierc_closure_make_enabled() {
                    reject!("[tierC-reject] op SetIndex (disabled)");
                }
            }
            // `array[index](args…)` — the region path's guarded dense
            // computed-call helper; every miss is a pure prefix.
            Instr::CallMethodComputed { .. } => {
                if !(crate::vm::tierc_closure_make_enabled() && computed_call_dense_enabled()) {
                    reject!("[tierC-reject] op CallMethodComputed (disabled)");
                }
            }
            // `new Array(…)` — the interpreter arm's dense subset via a pure
            // helper; RangeError/sparse lengths decline to the interpreter.
            Instr::ArrayCtor { .. } => {
                if !crate::vm::tierc_closure_make_enabled() {
                    reject!("[tierC-reject] op ArrayCtor (disabled)");
                }
            }
            // Sync `for-of` machinery and its close-on-abrupt-exit finally
            // bracket. Every helper implements the interpreter arm's built-in
            // fast path (pristine dense-array iterator, no-op normal-completion
            // tails, frame handler pushes for the verified frame-backed
            // activation) and declines observable protocol steps purely.
            // Functions with handler ops never receive cross entries, so
            // frame-free activations cannot reach the bracket helpers.
            Instr::GetIterator { .. }
            | Instr::IterPrime { .. }
            | Instr::IterNext { .. }
            | Instr::PushFinally { .. }
            | Instr::PopFinally
            | Instr::EndFinally { .. }
            | Instr::IterCloseFinally { .. }
            | Instr::ObjectKeys { .. } => {
                if !crate::vm::tierc_iter_enabled() {
                    reject!("[tierC-reject] iterator/finally op (disabled)");
                }
            }
            // `key in obj` (never the `#x in obj` brand form): dense-present
            // fast path, then the full proxy-aware [[HasProperty]] with the
            // call-protocol throw wiring.
            Instr::HasProp { brand: false, .. } => {
                if !crate::vm::tierc_iter_enabled() {
                    reject!("[tierC-reject] op HasProp (disabled)");
                }
            }
            Instr::GlobalFn {
                op: crate::bytecode::GlobalFn::String,
                argc: 1,
                ..
            } => {
                if !crate::vm::tierc_int_string_enabled() {
                    reject!("[tierC-reject] GlobalFn String (disabled)");
                }
            }
            Instr::LooseEq { a, b, .. } => {
                // Profitability guard: admit only the compiler's adjacent
                // `LoadNull/Undefined; LooseEq` lowering (for `x == null`).
                // The helper rechecks the LIVE operands, so even an unusual
                // internal edge that skips the prefix still deopts safely.
                let null_prefix = ip.checked_sub(1).is_some_and(|previous| {
                    matches!(
                        proto.code[previous],
                        Instr::LoadNull { dst } | Instr::LoadUndefined { dst }
                            if dst == a || dst == b
                    )
                });
                if !crate::vm::tierc_loose_null_eq_enabled() || !null_prefix {
                    reject!("[tierC-reject] op LooseEq (not adjacent nullish comparison)");
                }
            }
            Instr::DeleteProp { .. } => {
                if !tierc_cold_delete_enabled() {
                    reject!("[tierC-reject] op DeleteProp (disabled)");
                }
            }
            Instr::Neg { .. } => {
                if !tierc_neg_enabled() {
                    reject!("[tierC-reject] op Neg (disabled)");
                }
            }
            // Captured accesses use Tier-C-specific helpers. A frame-free native
            // cross-call installs its live closure explicitly; using the region
            // helper here would read `frames.last()` (the CALLER) and silently
            // select the wrong cells. Writes preserve const / named-function /
            // TDZ semantics by declining before the store. The former blanket
            // rejection was right for tiny one-op closures (B50), but wrong for
            // medium bodies where blacklisting the other 30+ ops dominates.
            Instr::UpvalGet { .. } | Instr::UpvalSet { .. } if tierc_upval_ok => {}
            // `CellGet`/`CellSet` remain rejected: MakeCell functions are setup
            // heavy and need a separate profitability/activation design.
            //
            // `Math.<op>(…)` — the shared `emit_math_op`, gated by the same
            // predicate the region path uses.
            Instr::MathOp { op, argc, .. } => {
                if !math_op_emittable(op, argc) {
                    reject!("[tierC-reject] MathOp arity {argc} op {op:?}");
                }
            }
            // General plain call or an exact captured-callee call. The latter
            // carries its receiver explicitly and never re-resolves a member
            // name in native code.
            Instr::Call { .. } | Instr::CallWithThis { .. } | Instr::RegExpMethod { .. } => {}
            // Proper-tail-call PREFIX: the compiler always emits `TailCall`
            // immediately before an ordinary `Call`+`Return` of the same site
            // (compile/bindings.rs, compile/exprs.rs), and the interpreter's
            // arm is pure frame-reuse with that Call as its own fallback — so
            // Tier C admits it and emits only a depth guard (see the emitter
            // arm below): shallow tail calls run as ordinary native calls,
            // and a deep chain bails at the TailCall ip so the interpreter's
            // constant-stack reuse takes over. `TailCallWithThis` stays
            // rejected (with-bodies only).
            Instr::TailCall { .. } => {
                if !tierc_tailcall_enabled() {
                    reject!("[tierC-reject] op TailCall (disabled)");
                }
            }
            // Method calls: the INTRINSIC set only — the builtins with
            // dedicated pure win64 helpers, i.e. the region path's whitelist
            // minus its pin-dependent fast paths. NOT the generic
            // `jit_call_method_ic` route: that helper deopts on every NATIVE
            // callee, so admitting an arbitrary method name would turn a
            // `join`/`toUpperCase`-calling function into per-call
            // native-entry + bail — strictly worse than staying interpreted
            // (B50's Upval lesson: reaching a tier is not being faster in it).
            Instr::CallMethod { name, argc, .. } => {
                let key = proto.string_constants.get(name as usize).map(|s| s.as_str());
                let ok = matches!(
                    (key, argc),
                    (Some("charCodeAt"), 1)
                        | (Some("indexOf"), 1)
                        | (Some("push"), 1)
                        | (Some("get"), 1)
                        | (Some("has"), 1)
                        | (Some("substring"), 2)
                        | (Some("slice"), 2)
                ) || (argc == 0
                    && tierc_random_method_enabled()
                    && matches!(key, Some("random")))
                    || (tierc_coll_mutate_enabled()
                        && matches!((key, argc), (Some("set"), 2) | (Some("clear"), 0)))
                    || (argc == 0
                        && tierc_string_upper_enabled()
                        && matches!(key, Some("toUpperCase")))
                    || (argc == 1
                        && substring1_intrinsic_enabled()
                        && matches!(key, Some("substring") | Some("slice")))
                    // Every other name/arity: the general live-IC route (see
                    // the emitter's final CallMethod arm). A user-closure
                    // callee runs natively; natives/exotics deopt per call.
                    || crate::vm::tierc_closure_make_enabled();
                if !ok {
                    reject!("[tierC-reject] CallMethod {key:?} argc={argc}");
                }
            }
            // Numeric / single-ASCII-char / pre-interned multi-char string
            // constants only (mirrors the region's LoadConst gate). const_strs
            // holds every multi-char string const interned by the caller.
            Instr::LoadConst { idx, .. } => match proto.constants.get(idx as usize) {
                Some(c) if c.is_number() => {}
                Some(&c) if single_char_const_bits(proto, c).is_some() => {}
                _ if const_strs.contains_key(&idx) => {}
                _ => {
                    reject!("[tierC-reject] LoadConst (non-numeric, non-interned string)");
                }
            },
            ref other => {
                reject!("[tierC-reject] op {other:?}");
            }
        }
    }
    ok
}

/// W7: the callee-window registers a Tier-C body may READ BEFORE WRITING, as a
/// u64 bitmask (bit r = register r), or `u64::MAX` when the analysis declines
/// (reg_count > 64, or an op outside the closed Tier-C admission set). Wide
/// functions use [`cross_uninit_wide_mask`] instead. The
/// cross-call helper zeroes exactly these slots (plus missing arguments) when
/// it reuses an already-initialized window via `set_len` instead of the
/// zero-filling `resize`; every other register is proven DEF-BEFORE-USE on
/// every path from entry, so the stale bits it transiently holds are
/// unobservable. That proof carries to the interpreter too: Tier C is the
/// memory tier — every def stores to the window before the next op — so a
/// mid-body bail resumes with exactly the defs the bytecode executed, and the
/// remaining path from the resume ip is one of the paths the dataflow covered.
/// (GC-completeness is argued separately at the fill site: exposed slots hold
/// valid, possibly stale, `Value`s — never uninitialized bytes.)
///
/// The dataflow is a forward MUST-DEFINED analysis over the bytecode CFG:
/// state = bitset of definitely-written regs; meet = AND at joins; top = `!0`
/// for not-yet-reached ips; entry state = {reg 0 (`this`)} ∪ {1..=param_count}
/// (the helper zeroes missing arguments per call, making the param assumption
/// true for short calls). Iterated to fixpoint (states only lose bits —
/// terminates), then every operand READ whose bit is unset in its in-state
/// marks the register. The op set is CLOSED: `mem_can_compile` admits no
/// try/catch handler op, so a Tier-C body has NO in-frame exception edges, and
/// any op this table doesn't recognize declines the whole analysis. USES must
/// be exact (a missed use could leave a readable stale slot unzeroed); DEFS
/// may be under-approximated (only costs precision).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn cross_uninit_mask(proto: &FuncProto) -> u64 {
    // Delegates to the shared fixpoint with the CROSS ud table verbatim —
    // W11 (B124) extracted the core so the leaf-splice fill could reuse the
    // same may-read-before-write analysis with its own op table
    // (`splice_uninit_mask`); the cross path's masks are bit-identical to
    // the pre-refactor form (cross_call.rs pins this).
    uninit_mask_over(
        &proto.code,
        (proto.reg_count as usize).max(1),
        proto.param_count as usize,
        cross_ud,
    )
}

/// The owned, multi-word form of [`cross_uninit_mask`] for Tier-C callees with
/// more than 64 registers. `None` is fail-closed: the op/use table declined,
/// an operand was outside the declared register window, or the proto was not
/// wide. Each returned bit has the exact same may-read-before-write meaning as
/// its W7 counterpart; keeping the words owned by the JIT avoids embedding any
/// bytecode or heap pointers in cross-call metadata.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn cross_uninit_wide_mask(proto: &FuncProto) -> Option<Box<[u64]>> {
    let regs = (proto.reg_count as usize).max(1);
    if regs <= 64 {
        return None;
    }
    do_wide_mask_passes(&proto.code, regs, proto.param_count as usize, cross_ud)
}

/// W11 (B124): the leaf-SPLICE variant of the mask, over the plan's (possibly
/// nested-flattened) body. Differences from the cross table, each justified:
/// `Call` is a guard MARKER in a flat body (uses the callee reg only, defines
/// NOTHING — the dst is defined by the trailing `Move` `splice_nested_leaf`
/// inserts, and claiming the def here would be unsound if a layout ever put a
/// read between them); `Mod`/`Neg`/`UpvalGet` are ordinary defs the cross
/// table simply never needed; `UpvalSet` uses its src at its position (sound
/// for the deferred buffered commit because admission only allows it in
/// branch-free bodies). Unknown ops decline to full fill, as ever.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn splice_uninit_mask(code: &[Instr], reg_count: usize, param_count: usize) -> u64 {
    fn ud(i: &Instr) -> Option<(smallvec::Uses, Option<u16>)> {
        use smallvec::Uses;
        Some(match *i {
            Instr::Call { callee, .. } => (Uses::one(callee), None),
            Instr::Mod { dst, a, b } => (Uses::two(a, b), Some(dst)),
            Instr::Neg { dst, a } => (Uses::one(a), Some(dst)),
            Instr::UpvalGet { dst, .. } => (Uses::new(), Some(dst)),
            Instr::UpvalSet { src, .. } => (Uses::one(src), None),
            _ => return cross_ud(i),
        })
    }
    uninit_mask_over(code, reg_count, param_count, ud)
}

/// W11 (B124): the set of callee regs any body op DEFINES, for the arg-alias
/// proof (a param never defined may alias the caller's arg slot). `None` on
/// any op the splice table does not model — fail-closed, aliasing declines.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn splice_body_defs(code: &[Instr]) -> Option<u64> {
    fn ud(i: &Instr) -> Option<(smallvec::Uses, Option<u16>)> {
        use smallvec::Uses;
        Some(match *i {
            Instr::Call { callee, .. } => (Uses::one(callee), None),
            Instr::Mod { dst, a, b } => (Uses::two(a, b), Some(dst)),
            Instr::Neg { dst, a } => (Uses::one(a), Some(dst)),
            Instr::UpvalGet { dst, .. } => (Uses::new(), Some(dst)),
            Instr::UpvalSet { src, .. } => (Uses::one(src), None),
            _ => return cross_ud(i),
        })
    }
    let mut defs = 0u64;
    for i in code {
        let (_, d) = ud(i)?;
        if let Some(d) = d {
            if d as usize >= 64 {
                return None;
            }
            defs |= 1u64 << d;
        }
    }
    Some(defs)
}

/// The shared may-read-before-write fixpoint (see `cross_uninit_mask`'s doc
/// for the contract: USES exact, DEFS may under-approximate, unknown op ⇒
/// `u64::MAX` ⇒ full fill). `ud` supplies the per-op use/def table.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn uninit_mask_over(
    code: &[Instr],
    regs: usize,
    params: usize,
    ud: fn(&Instr) -> Option<(smallvec::Uses, Option<u16>)>,
) -> u64 {
    const DECLINE: u64 = u64::MAX;
    let n = code.len();
    if n == 0 || regs > 64 || 1 + params >= 64 {
        return DECLINE;
    }
    let _ = regs;
    do_mask_passes(code, params, ud)
}

/// The CROSS ud table — verbatim from the pre-W11 `cross_uninit_mask` (see
/// its doc for why USES must be exact and unlisted ops decline).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn cross_ud(i: &Instr) -> Option<(smallvec::Uses, Option<u16>)> {
    {
        use smallvec::Uses;
        let u0 = || Uses::new();
        let u1 = |a: u16| Uses::one(a);
        let u2 = |a: u16, b: u16| Uses::two(a, b);
        let u3 = |a: u16, b: u16, c: u16| Uses::three(a, b, c);
        Some(match *i {
            Instr::LoadConst { dst, .. }
            | Instr::LoadInt { dst, .. }
            | Instr::LoadUndefined { dst }
            | Instr::LoadNull { dst }
            | Instr::LoadBool { dst, .. }
            | Instr::LoadGlobal { dst, .. }
            | Instr::NewObject { dst, .. }
            | Instr::NewPlannedObject { dst, .. }
            | Instr::MakeFunc { dst, .. } => (u0(), Some(dst)),
            Instr::Move { dst, src } => (u1(src), Some(dst)),
            Instr::UpvalGet { dst, .. } => (u0(), Some(dst)),
            Instr::UpvalSet { src, .. } => (u1(src), None),
            Instr::StoreGlobal { src, .. }
            | Instr::StoreGlobalStrict { src, .. }
            | Instr::StoreGlobalResolved { src, .. } => (u1(src), None),
            Instr::Add { dst, a, b }
            | Instr::Sub { dst, a, b }
            | Instr::Mul { dst, a, b }
            | Instr::Div { dst, a, b }
            | Instr::Mod { dst, a, b }
            | Instr::Bitwise { dst, a, b, .. }
            | Instr::StrAppendInPlace { dst, a, b }
            // B208: same uses/def licence as Add — bytecode.rs declares the op
            // semantically identical for every operand pair. Its absence (a
            // W11/B124 staleness: the table was frozen pre-W11) failed closed
            // as a full window zero-fill on EVERY cross call into a callee
            // containing a fused chain (243k fills/run on router+react).
            | Instr::StrConcatChain { dst, a, b }
            | Instr::Lt { dst, a, b }
            | Instr::Le { dst, a, b }
            | Instr::Gt { dst, a, b }
            | Instr::Ge { dst, a, b }
            | Instr::Eq { dst, a, b }
            | Instr::Ne { dst, a, b } => (u2(a, b), Some(dst)),
            Instr::StrAppendIndex {
                dst, a, obj, key, ..
            } => (u3(a, obj, key), Some(dst)),
            Instr::AddRightPair { dst, a, b, c, .. } => (u3(a, b, c), Some(dst)),
            Instr::Pad2Concat { dst, src, .. } => (u1(src), Some(dst)),
            Instr::Pad2Conditional { dst, src } => (u1(src), Some(dst)),
            Instr::AddInt { dst, a, .. } => (u1(a), Some(dst)),
            Instr::Neg { dst, a }
            | Instr::Not { dst, a }
            | Instr::TypeOf { dst, a }
            | Instr::TypeOfIs { dst, a, .. } => (u1(a), Some(dst)),
            Instr::TypeOfSame { dst, a, b, .. } => (u2(a, b), Some(dst)),
            Instr::IsArray {
                dst,
                a,
                callee,
                this_v,
            } => (u3(a, callee, this_v), Some(dst)),
            Instr::LenOf { dst, obj } | Instr::ForInKeys { dst, obj } => (u1(obj), Some(dst)),
            Instr::ForInLive { dst, obj, key } => (u2(obj, key), Some(dst)),
            Instr::GetIndex { dst, obj, key } => (u2(obj, key), Some(dst)),
            Instr::GetProp { dst, obj, .. } => (u1(obj), Some(dst)),
            Instr::SetProp { obj, val, .. } => (u2(obj, val), None),
            Instr::AppendDataProp { obj, val, .. } => (u2(obj, val), None),
            Instr::SetHomeObject { method, home } => (u2(method, home), None),
            Instr::DeleteProp { dst, obj, .. } => (u1(obj), Some(dst)),
            Instr::NewArray {
                dst,
                arg_base,
                argc,
            } => (Uses::range(arg_base, argc), Some(dst)),
            Instr::FinalizeObject {
                dst,
                val_base,
                count,
                ..
            } => (Uses::range(val_base, count), Some(dst)),
            // Capture cells are read-modify-write on their register; closure
            // creation reads only named operands here (ParentLocal cell
            // sources were def'd by an earlier MakeCell* in the same body, so
            // entry-uninit analysis is unaffected — the instr_uses convention).
            Instr::MakeCell { reg } | Instr::MakeCellFnName { reg } => (u1(reg), Some(reg)),
            Instr::MakeCellTdz { reg } => (u0(), Some(reg)),
            Instr::MarkCellConst { reg } => (u1(reg), None),
            Instr::MakeClosure { dst, .. } => (u0(), Some(dst)),
            Instr::MakeArrow { dst, this_reg, .. } => (u1(this_reg), Some(dst)),
            Instr::CellGet { dst, cell } => (u1(cell), Some(dst)),
            Instr::CellSet { cell, src } | Instr::CellSetChecked { cell, src } => {
                (u2(cell, src), None)
            }
            Instr::SetIndex { obj, key, val } => (u3(obj, key, val), None),
            Instr::CallMethodComputed {
                dst,
                obj,
                key,
                arg_base,
                argc,
            } => (Uses::range(arg_base, argc).plus(obj).plus(key), Some(dst)),
            Instr::ArrayCtor {
                dst,
                callee,
                arg_base,
                argc,
                ..
            } => {
                let uses = match callee {
                    Some(reg) => Uses::range(arg_base, argc).plus(reg),
                    None => Uses::range(arg_base, argc),
                };
                (uses, Some(dst))
            }
            Instr::GetIterator { dst, src } => (u1(src), Some(dst)),
            Instr::IterPrime { dst, iter } => (u1(iter), Some(dst)),
            Instr::ObjectKeys {
                dst,
                obj,
                callee,
                this_v,
            } => (u3(obj, callee, this_v), Some(dst)),
            Instr::HasProp {
                dst,
                key,
                obj,
                brand: false,
            } => (u2(key, obj), Some(dst)),
            // The finally bracket manipulates frame state only; EndFinally and
            // IterCloseFinally read their completion registers on either path.
            Instr::PushFinally { .. } | Instr::PopFinally => (u0(), None),
            Instr::EndFinally { kind_reg, val_reg } => (u2(kind_reg, val_reg), None),
            Instr::IterCloseFinally { iter, kind_reg } => (u2(iter, kind_reg), None),
            // IterNext writes three registers, which does not fit the
            // single-def mask model; a decline only costs the zero-fill
            // fallback (and handler-op functions get no cross entry anyway).
            Instr::IterNext { .. } => return None,
            Instr::LooseEq { dst, a, b } => (u2(a, b), Some(dst)),
            Instr::MathOp {
                dst,
                callee,
                this_v,
                arg_base,
                argc,
                ..
            } => (
                if callee == crate::bytecode::NO_REG {
                    Uses::range(arg_base, argc)
                } else {
                    Uses::range(arg_base, argc).plus(callee).plus(this_v)
                },
                Some(dst),
            ),
            Instr::GlobalFn {
                dst,
                callee,
                arg_base,
                argc,
                ..
            } => (Uses::range(arg_base, argc).plus(callee), Some(dst)),
            Instr::Call {
                dst,
                callee,
                arg_base,
                argc,
            } => (Uses::range(arg_base, argc).plus(callee), Some(dst)),
            Instr::CallWithThis {
                dst,
                callee,
                this_v,
                arg_base,
                argc,
            }
            | Instr::RegExpMethod {
                dst,
                callee,
                this_v,
                arg_base,
                argc,
                ..
            } => (
                Uses::range(arg_base, argc).plus(callee).plus(this_v),
                Some(dst),
            ),
            // Frame-reuse prefix: the interpreter reads callee + args (USES
            // must be exact per the contract above); it defines nothing.
            Instr::TailCall {
                callee,
                arg_base,
                argc,
            } => (Uses::range(arg_base, argc).plus(callee), None),
            Instr::CallMethod {
                dst,
                obj,
                arg_base,
                argc,
                ..
            } => (Uses::range(arg_base, argc).plus(obj), Some(dst)),
            Instr::Jump { .. } | Instr::ReturnUndefined => (u0(), None),
            Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => (u1(cond), None),
            Instr::JumpIfNotLt { a, b, .. } | Instr::JumpIfNotLe { a, b, .. } => (u2(a, b), None),
            Instr::CheckCoercible { src } => (u1(src), None),
            Instr::Return { src } => (u1(src), None),
            _ => return None,
        })
    }
}

/// A tiny inline use-list (compile-time only; avoids per-op Vec churn).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
mod smallvec {
    pub(super) struct Uses {
        regs: [u16; 3],
        len: u8,
        /// Contiguous extra range `[base, base+count)` (call/math args).
        range: (u16, u16),
    }
    impl Uses {
        pub(super) fn new() -> Uses {
            Uses {
                regs: [0; 3],
                len: 0,
                range: (0, 0),
            }
        }
        pub(super) fn one(a: u16) -> Uses {
            Uses {
                regs: [a, 0, 0],
                len: 1,
                range: (0, 0),
            }
        }
        pub(super) fn two(a: u16, b: u16) -> Uses {
            Uses {
                regs: [a, b, 0],
                len: 2,
                range: (0, 0),
            }
        }
        pub(super) fn three(a: u16, b: u16, c: u16) -> Uses {
            Uses {
                regs: [a, b, c],
                len: 3,
                range: (0, 0),
            }
        }
        pub(super) fn range(base: u16, count: u16) -> Uses {
            Uses {
                regs: [0; 3],
                len: 0,
                range: (base, count),
            }
        }
        pub(super) fn plus(mut self, r: u16) -> Uses {
            self.regs[self.len as usize] = r;
            self.len += 1;
            self
        }
        pub(super) fn count(&self) -> usize {
            self.len as usize + self.range.1 as usize
        }
        /// Visit every referenced register. A malformed internal range that
        /// overflows `u16` is rejected instead of wrapping or panicking.
        pub(super) fn for_each(&self, mut f: impl FnMut(u16)) -> bool {
            for k in 0..self.len as usize {
                f(self.regs[k]);
            }
            let Some(end) = self.range.0.checked_add(self.range.1) else {
                return false;
            };
            for r in self.range.0..end {
                f(r);
            }
            true
        }
    }
}

/// Compile-time-only Tier-C scalar replacement. Instruction indices stay
/// unchanged: an allocation becomes a benign tagged zero and each proved
/// projection becomes a register move. Unlike region-local SROA this lane has
/// no deopt materializer, so its live range admits only non-bailing transfers.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Default)]
struct TiercBlockSroaPlan {
    alloc_dst: Vec<Option<u16>>,
    read_src: Vec<Option<u16>>,
    finalized_sites: usize,
    array_sites: usize,
    reads: usize,
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
impl TiercBlockSroaPlan {
    fn ensure_slots(&mut self, n: usize) {
        if self.alloc_dst.is_empty() {
            self.alloc_dst.resize(n, None);
            self.read_src.resize(n, None);
        }
    }

    fn is_empty(&self) -> bool {
        self.finalized_sites == 0 && self.array_sites == 0
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
enum TiercBlockAggregate<'a> {
    Array {
        arg_base: u16,
        argc: u16,
    },
    Finalized {
        plan: &'a crate::bytecode::StaticKeyPlan,
        val_base: u16,
    },
}

/// `reg` must still contain the bits copied by the erased allocation. A write
/// at the projection itself is conservatively rejected too; accepting the
/// self-move case is not worth widening this proof.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn tierc_block_sroa_source_unchanged(
    code: &[Instr],
    after: usize,
    through: usize,
    reg: u16,
) -> bool {
    code.get(after..=through).is_some_and(|slice| {
        slice
            .iter()
            .all(|instr| cross_ud(instr).is_some_and(|(_, dst)| dst != Some(reg)))
    })
}

/// Require the array index to be an immediate loaded after the allocation in
/// this same straight-line block. This deliberately does not propagate Moves
/// or constants across predecessors.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn tierc_block_sroa_reaching_int(code: &[Instr], after: usize, at: usize, reg: u16) -> Option<i32> {
    for instr in code.get(after..at)?.iter().rev() {
        let (_, dst) = cross_ud(instr)?;
        if dst == Some(reg) {
            return match *instr {
                Instr::LoadInt { val, .. } => Some(val),
                _ => None,
            };
        }
    }
    None
}

/// Prove one allocation and return its exact projection map. The 64-op cap is
/// both a profitability gate and an attacker-work bound; the public planner
/// also considers at most 16 aggregate sites per function.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn plan_tierc_block_sroa_site(
    proto: &FuncProto,
    targeted: &[bool],
    yield_heads: &[u32],
    alloc_ip: usize,
    dst: u16,
    aggregate: TiercBlockAggregate<'_>,
) -> Option<Vec<(usize, u16)>> {
    const MAX_BLOCK_OPS: usize = 64;

    let code = &proto.code;
    if dst >= proto.reg_count {
        return None;
    }
    let (source_base, source_end) = match &aggregate {
        TiercBlockAggregate::Array { arg_base, argc, .. } => {
            (*arg_base, arg_base.checked_add(*argc)?)
        }
        TiercBlockAggregate::Finalized { val_base, plan } => (
            *val_base,
            val_base.checked_add(u16::try_from(plan.len()).ok()?)?,
        ),
    };
    // The real instruction snapshots every source before defining dst. Our
    // filler writes dst first, so an overlapping source window is not safe.
    if (source_base..source_end).contains(&dst) {
        return None;
    }
    let scan_end = alloc_ip
        .checked_add(MAX_BLOCK_OPS)?
        .min(code.len().saturating_sub(1));
    let mut block_end = None;
    for ip in alloc_ip + 1..=scan_end {
        match code[ip] {
            Instr::Return { .. } | Instr::ReturnUndefined => {
                block_end = Some(ip);
                break;
            }
            Instr::Jump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotLe { .. } => return None,
            _ => {}
        }
    }
    let block_end = block_end?;
    // No side entry may skip the allocation or any alias construction. A
    // region-yield exit in the live range would likewise require materializing
    // the erased aggregate, which this deliberately tiny lane never does.
    if targeted.get(alloc_ip + 1..=block_end)?.iter().any(|&v| v)
        || yield_heads
            .iter()
            .any(|&head| (alloc_ip + 1..=block_end).contains(&(head as usize)))
    {
        return None;
    }

    let mut aliases = FxHashSet::default();
    aliases.insert(dst);
    let mut reads = Vec::new();
    for ip in alloc_ip + 1..=block_end {
        let instr = &code[ip];
        // Closure creation also reads the child proto's ParentLocal capture
        // registers, which no per-instruction use table can see. Compiler
        // bytecode boxes those sources explicitly, but a hand-built malformed
        // proto must not reach a deopt after this allocation was erased.
        if !aliases.is_empty()
            && matches!(instr, Instr::MakeClosure { .. } | Instr::MakeArrow { .. })
        {
            return None;
        }
        // Use the closed Tier-C table before inspecting operands. Besides
        // declining unknown ops, `Uses::for_each` rejects malformed range
        // operands whose `base + count` would overflow `u16`.
        let (uses, defined) = cross_ud(instr)?;
        if uses.count() > MAX_BLOCK_OPS {
            return None;
        }
        let mut invalid_use = false;
        let mut alias_uses = Vec::new();
        if !uses.for_each(|reg| {
            if reg >= proto.reg_count {
                invalid_use = true;
            } else if aliases.contains(&reg) {
                alias_uses.push(reg);
            }
        }) || invalid_use
        {
            return None;
        }
        let source_was_alias = matches!(*instr, Instr::Move { src, .. } if aliases.contains(&src));

        if !alias_uses.is_empty() {
            let projected = match (&aggregate, instr) {
                (
                    TiercBlockAggregate::Finalized { plan, val_base },
                    Instr::GetProp { obj, name, .. },
                ) if aliases.contains(obj) && alias_uses.iter().all(|&reg| reg == *obj) => {
                    let key = proto.string_constants.get(*name as usize)?;
                    let field = plan.keys().iter().position(|candidate| candidate == key)?;
                    val_base.checked_add(u16::try_from(field).ok()?)?
                }
                (
                    TiercBlockAggregate::Array { arg_base, argc },
                    Instr::GetIndex { obj, key, .. },
                ) if aliases.contains(obj)
                    && !aliases.contains(key)
                    && alias_uses.iter().all(|&reg| reg == *obj) =>
                {
                    let index = tierc_block_sroa_reaching_int(code, alloc_ip + 1, ip, *key)?;
                    if index < 0 || index >= *argc as i32 {
                        return None;
                    }
                    arg_base.checked_add(index as u16)?
                }
                (_, Instr::Move { src, .. }) if aliases.contains(src) => {
                    // Alias-only transfer; there is no projection at this ip.
                    u16::MAX
                }
                _ => return None,
            };
            if projected != u16::MAX {
                if projected >= proto.reg_count
                    || aliases.contains(&projected)
                    || !tierc_block_sroa_source_unchanged(code, alloc_ip + 1, ip, projected)
                {
                    return None;
                }
                reads.push((ip, projected));
            }
        }

        // cross_ud is the closed Tier-C one-def table. Unknown/multi-def ops
        // decline rather than risking a stale alias after a hidden write.
        if let Some(defined) = defined {
            if defined >= proto.reg_count {
                return None;
            }
            aliases.remove(&defined);
        }
        if source_was_alias {
            let Instr::Move { dst, .. } = *instr else {
                unreachable!()
            };
            aliases.insert(dst);
        }
    }
    if reads.is_empty() {
        return None;
    }

    // Between allocation and the final aggregate observation there may be no
    // helper, effect, guard, or control transfer. Thus generated code cannot
    // bail while an interpreter continuation would need the real allocation.
    let last_read = reads.last()?.0;
    for (offset, instr) in code.get(alloc_ip + 1..=last_read)?.iter().enumerate() {
        let ip = alloc_ip + 1 + offset;
        let pure = matches!(
            instr,
            Instr::LoadInt { .. }
                | Instr::LoadConst { .. }
                | Instr::LoadUndefined { .. }
                | Instr::LoadNull { .. }
                | Instr::LoadBool { .. }
                | Instr::Move { .. }
        ) || (reads.iter().any(|&(read_ip, _)| read_ip == ip)
            && matches!(instr, Instr::GetProp { .. } | Instr::GetIndex { .. }));
        if !pure {
            return None;
        }
    }
    Some(reads)
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn plan_tierc_block_sroa(
    proto: &FuncProto,
    targeted: &[bool],
    yield_heads: &[u32],
) -> TiercBlockSroaPlan {
    const MAX_SITES: usize = 16;

    let mut out = TiercBlockSroaPlan::default();
    if std::env::var_os("ZIPP_GC_STRESS").is_some()
        || !proto.eval_sites.is_empty()
        || proto.code.iter().any(|instr| {
            matches!(
                instr,
                Instr::PushFinally { .. }
                    | Instr::PopFinally
                    | Instr::EndFinally { .. }
                    | Instr::IterCloseFinally { .. }
            )
        })
    {
        return out;
    }

    let mut considered = 0usize;
    for (alloc_ip, instr) in proto.code.iter().enumerate() {
        let (dst, aggregate, finalized) = match *instr {
            Instr::NewArray {
                dst,
                arg_base,
                argc,
            } if argc != 0
                && arg_base
                    .checked_add(argc)
                    .is_some_and(|end| end <= proto.reg_count) =>
            {
                (dst, TiercBlockAggregate::Array { arg_base, argc }, false)
            }
            Instr::FinalizeObject {
                dst,
                plan,
                val_base,
                count,
            } if count != 0
                && val_base
                    .checked_add(count)
                    .is_some_and(|end| end <= proto.reg_count) =>
            {
                let Some(plan) = proto.static_key_plans.get(plan as usize) else {
                    continue;
                };
                if !plan.runtime_valid()
                    || plan.len() != count as usize
                    || plan.len() > crate::bytecode::FINALIZE_STAGE_SLOTS
                    || plan.has_element_key()
                {
                    continue;
                }
                (dst, TiercBlockAggregate::Finalized { plan, val_base }, true)
            }
            _ => continue,
        };
        considered += 1;
        if considered > MAX_SITES {
            break;
        }
        let Some(reads) =
            plan_tierc_block_sroa_site(proto, targeted, yield_heads, alloc_ip, dst, aggregate)
        else {
            continue;
        };
        if out.alloc_dst.get(alloc_ip).copied().flatten().is_some()
            || reads
                .iter()
                .any(|&(ip, _)| out.read_src.get(ip).copied().flatten().is_some())
        {
            continue;
        }
        out.ensure_slots(proto.code.len());
        out.alloc_dst[alloc_ip] = Some(dst);
        for (ip, src) in reads {
            out.read_src[ip] = Some(src);
            out.reads += 1;
        }
        if finalized {
            out.finalized_sites += 1;
        } else {
            out.array_sites += 1;
        }
    }
    out
}

#[cfg(all(test, feature = "jit", target_arch = "x86_64"))]
mod tierc_block_sroa_tests {
    use super::*;

    const ARRAY_SOURCE: &str = r#"
        function project(value) {
          const array = [value, 3];
          const alias = array;
          return (alias[0] + alias[1]) | 0;
        }
        project(7);
    "#;

    fn project_proto() -> FuncProto {
        let ast = crate::front::parse_script(ARRAY_SOURCE).expect("parse source");
        crate::compile::compile_program(&ast, ARRAY_SOURCE)
            .expect("compile source")
            .functions
            .into_iter()
            .find(|proto| proto.name == "project")
            .expect("project proto")
    }

    fn targets(proto: &FuncProto) -> Vec<bool> {
        let mut targets = vec![false; proto.code.len()];
        for instr in &proto.code {
            let target = match *instr {
                Instr::Jump { target }
                | Instr::JumpIfFalse { target, .. }
                | Instr::JumpIfTrue { target, .. }
                | Instr::JumpIfNotLt { target, .. }
                | Instr::JumpIfNotLe { target, .. } => Some(target as usize),
                _ => None,
            };
            if let Some(target) = target.filter(|&target| target < targets.len()) {
                targets[target] = true;
            }
        }
        targets
    }

    fn array_site(proto: &FuncProto) -> (usize, u16, u16) {
        proto
            .code
            .iter()
            .enumerate()
            .find_map(|(ip, instr)| match *instr {
                Instr::NewArray { dst, arg_base, .. } => Some((ip, dst, arg_base)),
                _ => None,
            })
            .expect("array site")
    }

    #[test]
    fn exact_array_plan_declines_side_entry() {
        let proto = project_proto();
        let mut targeted = targets(&proto);
        let read_ip = proto
            .code
            .iter()
            .position(|instr| matches!(instr, Instr::GetIndex { .. }))
            .expect("array read");
        let baseline = plan_tierc_block_sroa(&proto, &targeted, &[]);
        assert_eq!((baseline.array_sites, baseline.reads), (1, 2));

        // Model an edge from outside the block directly into its first read.
        // The production target map is purely structural, so setting this bit
        // exercises the same side-entry rejection without hand-authoring a
        // malformed jump target in otherwise compiler-generated bytecode.
        targeted[read_ip] = true;
        assert!(plan_tierc_block_sroa(&proto, &targeted, &[]).is_empty());
    }

    #[test]
    fn exact_array_plan_declines_escape_effect_and_source_overwrite() {
        let proto = project_proto();
        let (alloc_ip, dst, arg_base) = array_site(&proto);

        let mut escaped = proto.clone();
        escaped
            .code
            .insert(alloc_ip + 1, Instr::StoreGlobal { idx: 0, src: dst });
        assert!(plan_tierc_block_sroa(&escaped, &targets(&escaped), &[]).is_empty());

        let mut effected = proto.clone();
        effected.code.insert(
            alloc_ip + 1,
            Instr::StoreGlobal {
                idx: 0,
                src: arg_base,
            },
        );
        assert!(plan_tierc_block_sroa(&effected, &targets(&effected), &[]).is_empty());

        let mut overwritten = proto;
        overwritten.code.insert(
            alloc_ip + 1,
            Instr::LoadInt {
                dst: arg_base,
                val: 99,
            },
        );
        assert!(plan_tierc_block_sroa(&overwritten, &targets(&overwritten), &[]).is_empty());
    }

    #[test]
    fn exact_array_plan_declines_overflowing_use_window() {
        let mut proto = project_proto();
        let (alloc_ip, dst, _) = array_site(&proto);
        proto.code.insert(
            alloc_ip + 1,
            Instr::NewArray {
                dst,
                arg_base: u16::MAX,
                argc: 2,
            },
        );

        assert!(plan_tierc_block_sroa(&proto, &targets(&proto), &[]).is_empty());
    }

    #[test]
    fn exact_array_plan_declines_oversize_use_window() {
        let mut proto = project_proto();
        proto.reg_count = 128;
        let return_ip = proto
            .code
            .iter()
            .position(|instr| matches!(instr, Instr::Return { .. }))
            .expect("return site");
        proto.code.insert(
            return_ip,
            Instr::NewArray {
                dst: 100,
                arg_base: 32,
                argc: 65,
            },
        );

        assert!(plan_tierc_block_sroa(&proto, &targets(&proto), &[]).is_empty());
    }

    #[test]
    fn exact_array_plan_declines_hidden_closure_capture() {
        let mut proto = project_proto();
        let (_, dst, _) = array_site(&proto);
        let return_ip = proto
            .code
            .iter()
            .position(|instr| matches!(instr, Instr::Return { .. }))
            .expect("return site");
        proto.code.insert(
            return_ip,
            Instr::MakeClosure {
                dst,
                func_id: u32::MAX,
            },
        );

        assert!(plan_tierc_block_sroa(&proto, &targets(&proto), &[]).is_empty());
    }
}

/// The general Tier-C `CallMethod` route: try the own-slot direct inliner,
/// then the method cross-call, then the interpreter's live method IC — each
/// prefix pure, each fallback unchanged, `CALL_THREW` unwinding at this ip.
/// Extracted so the `random` intrinsic arm and every non-intrinsic name emit
/// the identical protocol.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[allow(clippy::too_many_arguments)]
fn emit_tierc_general_method(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    call_method_ic: usize,
    packed_fip: u64,
    packed_args: u64,
    argc: u16,
    dst: u16,
    arg_base: u16,
    refetch: Option<(usize, usize)>,
    own_slot_direct: bool,
) {
    // A metered region deliberately skips the native prefixes: their callee
    // entries have not yet proved byte-for-byte charge equivalence with the
    // interpreter CallMethod (`own_slot_direct` is pre-gated on the meter).
    let method_cross = tierc_method_crosscall_enabled();
    let method_done = ops.new_dynamic_label();
    if own_slot_direct {
        let direct_helper = crate::vm::jit_cross_own_method_call as usize;
        let direct_fallback = ops.new_dynamic_label();
        dynasm!(ops
            ; mov rcx, rdi                        // vm
            ; mov rdx, rbx                        // caller window base
            ; lea r8, [rbx + dreg(arg_base)]      // &args[0..argc]
            ; mov r9, QWORD packed_fip as i64
            ; mov rax, QWORD direct_helper as i64
            ; call rax
            ; mov r10, QWORD SELF_CALL_DEOPT as i64
            ; cmp rax, r10
            ; je => direct_fallback               // pure decline -> existing path
            ; mov r10, QWORD CALL_THREW as i64
            ; cmp rax, r10
            ; je => bail                          // committed throw
            ; mov [rbx + dreg(dst)], rax
        );
        if let Some((vb, icb)) = refetch {
            emit_refetch_pinned(ops, vb, Some(icb));
        }
        dynasm!(ops
            ; jmp => method_done
            ; => direct_fallback
        );
    }
    if method_cross {
        let method_cross_helper = crate::vm::jit_cross_method_call as usize;
        let method_fallback = ops.new_dynamic_label();
        dynasm!(ops
            ; mov rcx, rdi                        // vm
            ; mov rdx, rbx                        // caller window base
            ; lea r8, [rbx + dreg(arg_base)]      // &args[0..argc]
            ; mov r9, QWORD packed_fip as i64
            ; mov rax, QWORD method_cross_helper as i64
            ; call rax
            ; mov r10, QWORD SELF_CALL_DEOPT as i64
            ; cmp rax, r10
            ; je => method_fallback               // pure decline → generic helper
            ; mov r10, QWORD CALL_THREW as i64
            ; cmp rax, r10
            ; je => bail                          // committed throw
            ; mov [rbx + dreg(dst)], rax
        );
        if let Some((vb, icb)) = refetch {
            emit_refetch_pinned(ops, vb, Some(icb));
        }
        dynasm!(ops
            ; jmp => method_done
            ; => method_fallback
        );
    }
    emit_region_call_ic(
        ops,
        ip,
        bail,
        epilogue,
        call_method_ic,
        packed_fip,
        packed_args,
        argc,
        dst,
        refetch,
        None,
    );
    if own_slot_direct || method_cross {
        dynasm!(ops ; => method_done);
    }
}

/// Passes 1-3 of the mask analysis (see `cross_uninit_mask`'s doc).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn do_mask_passes(
    code: &[Instr],
    params: usize,
    ud: fn(&Instr) -> Option<(smallvec::Uses, Option<u16>)>,
) -> u64 {
    const DECLINE: u64 = u64::MAX;
    let n = code.len();
    // Pass 1: use/def per ip (decline on any unlisted op or out-of-range reg).
    let mut uses: Vec<smallvec::Uses> = Vec::with_capacity(n);
    let mut defs: Vec<Option<u16>> = Vec::with_capacity(n);
    for i in code.iter() {
        let Some((u, d)) = ud(i) else { return DECLINE };
        let mut bad = false;
        if !u.for_each(|r| bad |= r as usize >= 64) || bad || d.is_some_and(|r| r as usize >= 64) {
            return DECLINE; // defensive: reg_count said ≤ 64
        }
        uses.push(u);
        defs.push(d);
    }
    // Pass 2: fixpoint. `in_state[ip]` = regs definitely written on EVERY path
    // from entry to ip; `!0` = unreached (top).
    let entry_state: u64 = (1u64 << (1 + params)) - 1;
    let mut in_state = vec![u64::MAX; n];
    in_state[0] = entry_state;
    loop {
        let mut changed = false;
        for ip in 0..n {
            let s = in_state[ip];
            if s == u64::MAX {
                continue; // not (yet) reached
            }
            let out = s | defs[ip].map_or(0, |d| 1u64 << d);
            let (s1, s2) = match code[ip] {
                Instr::Jump { target } => (Some(target as usize), None),
                Instr::JumpIfFalse { target, .. }
                | Instr::JumpIfTrue { target, .. }
                | Instr::JumpIfNotLt { target, .. }
                | Instr::JumpIfNotLe { target, .. } => (Some(ip + 1), Some(target as usize)),
                Instr::Return { .. } | Instr::ReturnUndefined => (None, None),
                _ => (Some(ip + 1), None),
            };
            for t in [s1, s2].into_iter().flatten() {
                // `t == n` = falling off the end (ReturnUndefined) — no state.
                if t < n {
                    let m = in_state[t] & out;
                    if m != in_state[t] {
                        in_state[t] = m;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Pass 3: any READ of a not-definitely-written reg marks it.
    let mut mask = 0u64;
    for ip in 0..n {
        let s = in_state[ip];
        if s == u64::MAX {
            continue; // unreachable code never reads anything
        }
        if !uses[ip].for_each(|r| {
            if s & (1u64 << r) == 0 {
                mask |= 1u64 << r;
            }
        }) {
            return DECLINE;
        }
    }
    mask
}

/// Wide-register counterpart of [`do_mask_passes`]. `None` is the decline
/// sentinel; reached states are explicit `Option`s rather than an all-ones
/// bitset because an all-ones state is a legitimate value once the register
/// file spans multiple words. This runs once when a function is compiled, so
/// the small owned vectors keep the execution path simple without adding any
/// per-call allocation or pointer lifetime.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const WIDE_MASK_MAX_REGS: usize = 1_024;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const WIDE_MASK_MAX_WORDS: usize = WIDE_MASK_MAX_REGS.div_ceil(64);
/// Maximum backing storage for the reached-state bitsets alone. The fixed
/// tables are capped separately below; both checks run before allocation.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const WIDE_MASK_MAX_STATE_BYTES: usize = 2 * 1024 * 1024;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const WIDE_MASK_MAX_ANALYSIS_BYTES: usize = 8 * 1024 * 1024;
/// Operand visits cover both the validation and final marking passes.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const WIDE_MASK_MAX_OPERAND_VISITS: usize = 4 * 1024 * 1024;
/// A pass clones at most one state and meets it into at most two successors.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const WIDE_MASK_MAX_FIXPOINT_WORK: usize = 16 * 1024 * 1024;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const WIDE_MASK_MAX_FIXPOINT_PASSES: usize = 256;

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn do_wide_mask_passes(
    code: &[Instr],
    regs: usize,
    params: usize,
    ud: fn(&Instr) -> Option<(smallvec::Uses, Option<u16>)>,
) -> Option<Box<[u64]>> {
    let n = code.len();
    if n == 0 || regs <= 64 || regs > WIDE_MASK_MAX_REGS || params >= regs {
        return None;
    }
    let words = regs.div_ceil(64);
    if words > WIDE_MASK_MAX_WORDS {
        return None;
    }

    // Attacker-provided source can maximize both dimensions independently.
    // Budget the worst case BEFORE any O(code_len * words) allocation, and
    // decline to the ordinary full-fill path on overflow or excess. Include
    // the fixed Vec/use/def tables so a two-word function with enormous code
    // cannot hide most of its allocation outside the state-byte limit.
    let state_words = n.checked_mul(words)?;
    let state_bytes = state_words.checked_mul(std::mem::size_of::<u64>())?;
    if state_bytes > WIDE_MASK_MAX_STATE_BYTES {
        return None;
    }
    let fixed_row_bytes = std::mem::size_of::<Option<Vec<u64>>>()
        .checked_add(std::mem::size_of::<smallvec::Uses>())?
        .checked_add(std::mem::size_of::<Option<u16>>())?;
    let fixed_bytes = n.checked_mul(fixed_row_bytes)?;
    let analysis_bytes = state_bytes.checked_add(fixed_bytes)?;
    if analysis_bytes > WIDE_MASK_MAX_ANALYSIS_BYTES {
        return None;
    }
    let work_per_pass = state_words.checked_mul(3)?.checked_add(n)?;
    if work_per_pass == 0 || work_per_pass > WIDE_MASK_MAX_FIXPOINT_WORK {
        return None;
    }
    let pass_limit = WIDE_MASK_MAX_FIXPOINT_PASSES.min(WIDE_MASK_MAX_FIXPOINT_WORK / work_per_pass);
    if pass_limit == 0 {
        return None;
    }

    // Pass 1: the same closed use/def table as the <=64-register path, with
    // the proto's declared window as the bound instead of one machine word.
    let mut uses: Vec<smallvec::Uses> = Vec::with_capacity(n);
    let mut defs: Vec<Option<u16>> = Vec::with_capacity(n);
    let mut operand_visits = 0usize;
    for i in code {
        let (u, d) = ud(i)?;
        operand_visits = operand_visits.checked_add(u.count())?;
        if operand_visits.checked_mul(2)? > WIDE_MASK_MAX_OPERAND_VISITS {
            return None;
        }
        let mut bad = false;
        if !u.for_each(|r| bad |= r as usize >= regs)
            || bad
            || d.is_some_and(|r| r as usize >= regs)
        {
            return None;
        }
        uses.push(u);
        defs.push(d);
    }

    // Pass 2: forward MUST-DEFINED fixpoint. `None` means unreachable; a
    // reached successor receives a cloned state and subsequent predecessors
    // meet into it with word-wise AND.
    let mut entry = vec![0u64; words];
    for r in 0..=params {
        entry[r / 64] |= 1u64 << (r % 64);
    }
    let mut in_state: Vec<Option<Vec<u64>>> = vec![None; n];
    in_state[0] = Some(entry);
    let mut converged = false;
    for _ in 0..pass_limit {
        let mut changed = false;
        for ip in 0..n {
            let Some(mut out) = in_state[ip].clone() else {
                continue;
            };
            if let Some(d) = defs[ip] {
                let d = d as usize;
                out[d / 64] |= 1u64 << (d % 64);
            }
            let (s1, s2) = match code[ip] {
                Instr::Jump { target } => (Some(target as usize), None),
                Instr::JumpIfFalse { target, .. }
                | Instr::JumpIfTrue { target, .. }
                | Instr::JumpIfNotLt { target, .. }
                | Instr::JumpIfNotLe { target, .. } => (Some(ip + 1), Some(target as usize)),
                Instr::Return { .. } | Instr::ReturnUndefined => (None, None),
                _ => (Some(ip + 1), None),
            };
            for target in [s1, s2].into_iter().flatten().filter(|t| *t < n) {
                match &mut in_state[target] {
                    None => {
                        in_state[target] = Some(out.clone());
                        changed = true;
                    }
                    Some(prior) => {
                        for (slot, incoming) in prior.iter_mut().zip(&out) {
                            let meet = *slot & *incoming;
                            if meet != *slot {
                                *slot = meet;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
        if !changed {
            converged = true;
            break;
        }
    }
    if !converged {
        return None;
    }

    // Pass 3: mark each reachable read not definitely written on every path.
    let mut mask = vec![0u64; words];
    for ip in 0..n {
        let Some(state) = &in_state[ip] else {
            continue;
        };
        if !uses[ip].for_each(|r| {
            let r = r as usize;
            if state[r / 64] & (1u64 << (r % 64)) == 0 {
                mask[r / 64] |= 1u64 << (r % 64);
            }
        }) {
            return None;
        }
    }
    Some(mask.into_boxed_slice())
}

/// Compile the WHOLE body of `proto` to native code via the memory-path op
/// emitters (Tier C). `globals_base_helper` pins r12 = `vm.globals` base;
/// `heap` carries the win64 helper addresses (get_index/char_code_at/call_ic/
/// strict_eq/truthy). Returns a `JitFn` with the standard ABI, or `None` if the
/// body is ineligible. v1 uses NO inline caches / TA pins / inline plans, so
/// r13/r14 are saved-but-unused and no post-call re-fetch is emitted (the
/// globals pin r12 stays valid across calls — `self.globals` never reallocates).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn compile_proto_mem(
    proto: &FuncProto,
    func_id: u32,
    globals_base_helper: usize,
    heap: HeapHelpers,
    const_strs: &FxHashMap<u32, u64>,
    leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
    // Tier-C transactional own-method/global plans. Kept distinct from the
    // ordinary region method plans because these bodies execute without a
    // callee scratch window and buffer every global write until all guards pass.
    method_plan: &FxHashMap<usize, MethodInlinePlan>,
    // Tier-C cross-call plan (B83): `Call` ips that get the native→native
    // cross-call attempt (fallback: the unchanged `call_ic` helper).
    cross_plan: &CrossCallPlan,
    // Per-site accessor-arm emission flags (the SITE GATE), indexed by the
    // local site number — see `compile_region_mem`'s twin parameter.
    ic_emit: &[IcSiteEmit],
    meter: Option<crate::codegen::meter::Meter>,
    // B206: loop-head ips owned by live reg-homed regions; the body bails
    // unconditionally at each so those loops stay with their regions.
    yield_heads: &[u32],
    // B205: fused random-scale windows (window-start ip -> plan).
    random_fuse: &FxHashMap<usize, crate::codegen::RandomScaleFusePlan>,
    // B257: `MakeFunc` ips licensed for the plain lane (ip -> child id).
    plain_makefunc: &FxHashMap<usize, u32>,
) -> Option<JitFn> {
    if !mem_can_compile(proto, const_strs) {
        return None;
    }
    // These arms can exit before executing their bytecode (or their helper can
    // defensively decline). A metered basic block charges its full native
    // length at entry, so interpreter replay could charge one of those
    // bytecodes twice.
    // Sandboxed/metered VMs therefore keep the exact interpreter route.
    if meter.is_some()
        && proto.code.iter().any(|instr| match instr {
            Instr::DeleteProp { .. }
            | Instr::LooseEq { .. }
            | Instr::Mod { .. }
            | Instr::CheckCoercible { .. }
            | Instr::MakeFunc { .. }
            | Instr::SetHomeObject { .. }
            | Instr::GlobalFn {
                op: crate::bytecode::GlobalFn::String,
                argc: 1,
                ..
            } => true,
            Instr::CallMethod { name, argc, .. } => proto
                .string_constants
                .get(*name as usize)
                .is_some_and(|key| {
                    matches!(
                        (key.as_str(), *argc),
                        ("clear" | "toUpperCase", 0) | ("set", 2)
                    )
                }),
            _ => false,
        })
    {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let n = proto.code.len();
    let protected_returns = tierc_protected_return_map(&proto.code);
    let method_own_slot_direct = meter.is_none() && tierc_method_own_slot_direct_enabled();
    if method_own_slot_direct && std::env::var_os("ZIPP_JITLOG").is_some() {
        let sites = proto
            .code
            .iter()
            .filter(|instr| {
                matches!(
                    instr,
                    Instr::CallMethod { name, argc: 0, .. }
                        if proto.string_constants.get(*name as usize).is_some_and(|key| key == "random")
                )
            })
            .count();
        if sites != 0 {
            eprintln!("[jit] fn{func_id} Tier-C own-slot-direct method sites={sites}");
        }
    }
    // The only forwarded shape is bytecode-adjacent, and a targetable second
    // ip is excluded: an internal edge could otherwise enter the UpvalGet
    // without executing its textual predecessor. Tier C admits no handler ops,
    // so ordinary jump targets are the complete non-fallthrough entry set.
    let mut targeted = vec![false; n];
    for instr in &proto.code {
        let target = match *instr {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => Some(target as usize),
            _ => None,
        };
        if let Some(target) = target.filter(|&target| target < n) {
            targeted[target] = true;
        }
    }
    let block_sroa = if meter.is_none() && tierc_block_sroa_enabled() {
        plan_tierc_block_sroa(proto, &targeted, yield_heads)
    } else {
        TiercBlockSroaPlan::default()
    };
    if !block_sroa.is_empty() && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] fn{func_id} Tier-C block-SROA finalized={} arrays={} reads={}",
            block_sroa.finalized_sites, block_sroa.array_sites, block_sroa.reads
        );
    }
    let mut upval_forward = vec![None; n];
    if tierc_upval_forward_enabled() {
        for ip in 1..n {
            let Instr::UpvalGet { idx, .. } = proto.code[ip] else {
                continue;
            };
            if targeted[ip] {
                continue;
            }
            upval_forward[ip] = match proto.code[ip - 1] {
                Instr::UpvalGet { dst, idx: previous }
                | Instr::UpvalSet {
                    src: dst,
                    idx: previous,
                } if previous == idx => Some(dst),
                _ => None,
            };
        }
    }
    let n_upval_forwards = upval_forward.iter().flatten().count();
    if n_upval_forwards != 0 && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!("[jit] fn{func_id} Tier-C upval-forward sites={n_upval_forwards}");
    }
    let mut upval_inc = vec![None; n];
    let mut upval_inc_covered = vec![false; n];
    if meter.is_none() && tierc_upval_inc_i32_enabled() {
        for ip in 0..n.saturating_sub(5) {
            if upval_inc_covered[ip] || targeted[ip + 1..ip + 6].iter().any(|&target| target) {
                continue;
            }
            let Some(plan) = tierc_upval_inc_i32_at(&proto.code, ip) else {
                continue;
            };
            upval_inc[ip] = Some(plan);
            upval_inc_covered[ip + 1..ip + 6].fill(true);
        }
    }
    let n_upval_incs = upval_inc.iter().flatten().count();
    if n_upval_incs != 0 && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!("[jit] fn{func_id} Tier-C upval-inc-i32 sites={n_upval_incs}");
    }
    let mut upval_xorshift = vec![None; n];
    let mut upval_xorshift_covered = vec![false; n];
    if meter.is_none() && tierc_upval_xorshift_enabled() {
        for ip in 0..n {
            if upval_xorshift_covered[ip] {
                continue;
            }
            let Some(plan) = tierc_upval_xorshift_at(&proto.code, &targeted, ip) else {
                continue;
            };
            let end = ip + usize::from(plan.count) * 6;
            upval_xorshift[ip] = Some(plan);
            upval_xorshift_covered[ip + 1..end].fill(true);
        }
    }
    let n_upval_xorshift_chains = upval_xorshift.iter().flatten().count();
    let n_upval_xorshift_steps: usize = upval_xorshift
        .iter()
        .flatten()
        .map(|plan| usize::from(plan.count))
        .sum();
    if n_upval_xorshift_chains != 0 && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] fn{func_id} Tier-C upval-xorshift chains={n_upval_xorshift_chains} steps={n_upval_xorshift_steps}"
        );
    }
    let mut global_xorshift = vec![None; n];
    let mut global_xorshift_covered = vec![false; n];
    if meter.is_none() && tierc_global_xorshift_enabled() {
        for ip in 0..n {
            if global_xorshift_covered[ip] {
                continue;
            }
            let Some(plan) = tierc_global_xorshift_at(&proto.code, &targeted, ip) else {
                continue;
            };
            let end = ip + usize::from(plan.count) * 6;
            global_xorshift[ip] = Some(plan);
            global_xorshift_covered[ip + 1..end].fill(true);
        }
    }
    // B205: mark the five ops after each fused random-scale window start as
    // covered — the fused arm materializes every destination in bytecode
    // order, and the plan builder required them untargeted.
    let mut random_fuse_covered = vec![false; n];
    if meter.is_none() {
        for (&ip, fp) in random_fuse.iter() {
            let span = if fp.upval_alph.is_some() { 7 } else { 6 };
            if ip + span <= n
                && !targeted[ip + 1..ip + span].iter().any(|&t| t)
                && !global_xorshift_covered[ip..ip + span].iter().any(|&c| c)
            {
                random_fuse_covered[ip + 1..ip + span].fill(true);
            }
        }
    }
    let str_append_cursor =
        tierc_str_append_cursor_plan(proto, random_fuse, &random_fuse_covered, meter);
    if let Some(plan) = str_append_cursor {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] fn{func_id} Tier-C str-append-cursor ip={} acc=r{} source=r{} key=r{}",
                plan.append_ip, plan.acc, plan.obj, plan.key
            );
        }
    }
    let n_global_xorshift_chains = global_xorshift.iter().flatten().count();
    let n_global_xorshift_steps: usize = global_xorshift
        .iter()
        .flatten()
        .map(|plan| usize::from(plan.count))
        .sum();
    if n_global_xorshift_chains != 0 && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] fn{func_id} Tier-C global-xorshift chains={n_global_xorshift_chains} steps={n_global_xorshift_steps}"
        );
    }
    // A label per ip; `labels[n]` is the fall-off-the-end (ReturnUndefined). All
    // jump targets are in-function, so they resolve directly (no exit stubs).
    let labels: Vec<_> = (0..=n).map(|_| ops.new_dynamic_label()).collect();
    // Shared epilogue: every Return / bail records [rsi] then jumps here.
    let epilogue = ops.new_dynamic_label();
    // ── Q4 leaf-call inlining (Tier C) ── inline a monomorphic plain-leaf callee
    // at a Call site over a scratch window carved above the whole-function frame.
    let do_leaf = !leaf_plan.is_empty();
    let do_method = !method_plan.is_empty();
    let method_needs_headroom = method_plan.values().any(|p| p.win_top > p.reg_window);
    let max_scratch_top: u64 = leaf_plan
        .values()
        .map(|p| p.reg_window as u64 + p.callee_reg_count as u64)
        .chain(method_plan.values().map(|p| p.win_top as u64))
        .max()
        .unwrap_or(0);
    // 32B shadow + 8B 5th-arg slot = 40; + the B189b/B193 64B emitted-call scratch
    // when a Call site carries a cross3 plan (region_mem's layout: prior
    // activation 24B @ c3, window base|flags @ c3+24, result @ c3+32, bail
    // slot @ c3+40); + a 16B leaf-headroom-flag slot when inlining (48 and 16
    // both keep the frame's 16-alignment after the 6 pushes).
    let do_cross3 = meter.is_none() && cross_plan.values().any(|site| !site.cross3.is_empty());
    let cursor_off: i32 = 40;
    let c3_off: i32 = cursor_off
        + if str_append_cursor.is_some() {
            crate::vm::STR_APPEND_CURSOR_FRAME_BYTES
        } else {
            0
        };
    let frame: i32 =
        40 + if str_append_cursor.is_some() {
            crate::vm::STR_APPEND_CURSOR_FRAME_BYTES
        } else {
            0
        } + if do_cross3 { 64 } else { 0 }
            + if do_leaf || method_needs_headroom {
                16
            } else {
                0
            };
    let cursor_active_off = cursor_off + crate::vm::STR_APPEND_CURSOR_ACTIVE_OFF;
    let cursor_acc_bits_off = cursor_off + crate::vm::STR_APPEND_CURSOR_ACC_BITS_OFF;
    let cursor_source_bits_off = cursor_off + crate::vm::STR_APPEND_CURSOR_SOURCE_BITS_OFF;
    let cursor_source_version_off = cursor_off + crate::vm::STR_APPEND_CURSOR_SOURCE_VERSION_OFF;
    let cursor_source_ptr_off = cursor_off + crate::vm::STR_APPEND_CURSOR_SOURCE_PTR_OFF;
    let cursor_source_len_off = cursor_off + crate::vm::STR_APPEND_CURSOR_SOURCE_LEN_OFF;
    let cursor_out_ptr_off = cursor_off + crate::vm::STR_APPEND_CURSOR_OUT_PTR_OFF;
    let cursor_out_len_off = cursor_off + crate::vm::STR_APPEND_CURSOR_OUT_LEN_OFF;
    let cursor_out_capacity_off = cursor_off + crate::vm::STR_APPEND_CURSOR_OUT_CAPACITY_OFF;
    // Byte offset of the headroom flag (1 = the carved window fits → inline; 0 =
    // fall back to the per-call helper). MUST equal the prologue store offset.
    let leaf_flag_off = frame - 8;

    // r13 (heap versions base) and r14 (JIT IC table base) are READ by the GetProp
    // inline-cache probe and by exact-identity leaf guards. A same-prototype
    // guard calls its read-only VM helper instead, while a slot-generation guard
    // reads its own stable counter address; neither needs these two pins.
    // INVARIANT (the refetch obligation): r13 moves on EVERY heap allocation
    // (versions Vec push), r14 on a nested region compile (during user code); so
    // EVERY op that allocates or runs user code (Call, Add-concat, TypeOf,
    // ForInKeys, ForInLive, GetProp-slow) MUST `emit_refetch_pinned` after
    // committing its result. fn11/12/13 have GetIndex-but-no-GetProp (has_prop=
    // false), so folding do_leaf in is REQUIRED — else the leaf version guard
    // (`[r13+rcx*4]`) reads an unpinned r13.
    let has_prop = proto
        .code
        .iter()
        .any(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }));
    let precise_entry_pins = std::env::var_os("ZIPP_NO_TIERC_PRECISE_PINS").is_none();
    let leaf_needs_version_pins = leaf_plan
        .values()
        .any(|p| (p.same_proto_fid.is_none() && p.slot_guard.is_none()) || !p.nested.is_empty());
    // EVERY `MathOp` whose op was baked takes the direct guard, which reads
    // the callee's (and, for the BARE form, the receiver's) heap generation
    // through r13; the captured form always carried a `GetProp` that pinned
    // r13 anyway, the bare form carries no other reader.
    let has_direct_math_guard = heap.math_imul_guard.is_some()
        && (proto.code.iter().any(|i| matches!(i, Instr::MathOp { .. }))
            || leaf_plan
                .values()
                .any(|p| p.body.iter().any(|i| matches!(i, Instr::MathOp { .. }))));
    let refetch_pinned = has_prop
        || do_method
        || has_direct_math_guard
        || if precise_entry_pins {
            leaf_needs_version_pins
        } else {
            do_leaf
        };
    let refetch = refetch_pinned.then_some((heap.versions_base, heap.ic_base));

    // r12 is only read by direct global bytecodes, including a body spliced by
    // the leaf inliner. `globals` never reallocates, but obtaining its pointer
    // through an FFI helper on every whole-function entry is material for tiny
    // captureful callees. Keep the pre-change unconditional pin behind the
    // off-switch for a same-binary mechanism A/B.
    let direct_global = |i: &Instr| {
        matches!(
            i,
            Instr::LoadGlobal { .. }
                | Instr::LoadGlobalOrUndefined { .. }
                | Instr::StoreGlobal { .. }
                | Instr::StoreGlobalStrict { .. }
                | Instr::StoreGlobalResolved { .. }
                // A BARE MathOp reads the `Math` global slot in its guard.
                | Instr::MathOp {
                    callee: crate::bytecode::NO_REG,
                    ..
                }
        )
    };
    let needs_globals = !precise_entry_pins
        || proto.code.iter().any(direct_global)
        || leaf_plan.values().any(|p| p.body.iter().any(direct_global))
        || do_method;
    // The three pointers are already mirrored in VM-owned state and refreshed
    // at their sole growth sites. Loading them rdi-relative avoids three Win64
    // helper round-trips on every tiny Tier-C entry. The off-switch restores
    // the historical helper prologue for a same-binary process A/B.
    let direct_entry_bases = std::env::var_os("ZIPP_NO_TIERC_DIRECT_BASES").is_none();
    if precise_entry_pins && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] fn{func_id} Tier-C entry-pins globals={} version-ic={} headroom={} method-global={} bases={}",
            needs_globals as u8,
            refetch_pinned as u8,
            (do_leaf || method_needs_headroom) as u8,
            do_method as u8,
            if direct_entry_bases { "direct" } else { "helpers" },
        );
    }

    // ── prologue ── save callee-saved regs, stash inputs, pin r12 = globals base.
    // Mirrors `compile_region_mem` (6 pushes + frame) so the region emitters and
    // the shared epilogue work verbatim. r13/r14 are saved (win64 requires it)
    // and pinned only when the function reads them (has GetProp).
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; sub rsp, frame
        ; mov rbx, rcx                    // regs base
        ; mov rsi, rdx                    // bail_ip out-pointer
        ; mov rdi, r8                     // vm
    );
    if str_append_cursor.is_some() {
        dynasm!(ops
            // `jit_str_append_cursor_begin` forms `&mut StrAppendCursor`
            // before replacing it with `empty()`. Initialize every field to a
            // valid value first; zeroing only `active` would leave the typed
            // reference containing uninitialized integers/pointers (UB).
            ; mov QWORD [rsp + cursor_active_off], 0
            ; mov QWORD [rsp + cursor_acc_bits_off], 0
            ; mov QWORD [rsp + cursor_source_bits_off], 0
            ; mov QWORD [rsp + cursor_source_version_off], 0
            ; mov QWORD [rsp + cursor_source_ptr_off], 0
            ; mov QWORD [rsp + cursor_source_len_off], 0
            ; mov QWORD [rsp + cursor_out_ptr_off], 0
            ; mov QWORD [rsp + cursor_out_len_off], 0
            ; mov QWORD [rsp + cursor_out_capacity_off], 0
        );
    }
    if needs_globals {
        if direct_entry_bases {
            dynasm!(ops
                ; mov r12, [rdi + crate::vm::host_api::JIT_GLOBALS_RAW_OFFSET as i32]
            );
        } else {
            dynasm!(ops
                ; mov rcx, rdi                    // arg0 = vm
                ; mov rax, QWORD globals_base_helper as i64
                ; call rax
                ; mov r12, rax                    // pinned globals base pointer
            );
        }
    }
    if refetch_pinned {
        // Pin the heap version-array base (r13) and the IC table base (r14) —
        // copied from the region prologue. Read by the GetProp IC probe and the
        // leaf-inline identity version guard. Later allocation/user-code ops
        // retain their helper refetches: this shortcut is entry-only.
        if direct_entry_bases {
            dynasm!(ops
                ; mov r13, [rdi + crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32]
                ; mov r14, [rdi + crate::vm::host_api::JIT_IC_TABLE_RAW_OFFSET as i32]
            );
        } else {
            dynasm!(ops
                ; mov rcx, rdi
                ; mov rax, QWORD heap.versions_base as i64
                ; call rax
                ; mov r13, rax
                ; mov rcx, rdi
                ; mov rax, QWORD heap.ic_base as i64
                ; call rax
                ; mov r14, rax
            );
        }
    }
    // ── Q4 leaf-inline headroom check (once per entry) ── `jit_regs_fits` → 1 if
    // every carved scratch window lies inside the pinned register file. Each
    // inlined Call site reads the flag and falls back to the helper on 0. rbx is
    // callee-saved; rcx/rdx/r8 are volatile scratch here.
    if do_leaf || method_needs_headroom {
        dynasm!(ops
            ; mov rcx, rdi                            // vm
            ; mov rdx, rbx                            // caller window base
            ; mov r8, QWORD max_scratch_top as i64    // highest scratch slot used
            ; mov rax, QWORD heap.regs_fits as i64
            ; call rax
            ; mov [rsp + leaf_flag_off], rax          // 1 = inline ok, 0 = helper
        );
    }

    // The k-th GetProp/SetProp uses inline-cache site `ic_site` (advanced in the
    // GetProp arm). Reserved contiguously by `Jit::compile` via reserve_ic_sites.
    let mut ic_site = heap.ic_base_idx;
    let int_hint = true; // v1 admits no double-constant feeds.
                         // Step metering (a metered VM only) — a Tier C body can loop just as a Tier
                         // A one can, so it needs the same charge. See codegen::meter.
    let blocks = crate::codegen::meter::block_map(meter, &proto.code, 0, n - 1);
    let mut meter_stubs: Vec<(dynasmrt::DynamicLabel, usize)> = Vec::new();
    // B118 fused compare→branch (the region rule, Tier-C shape): `cmp {dst} ;
    // JumpIfTrue/False{cond: dst}` at the very next ip fuses (see
    // `emit_fused_cmp_branch_head`). Detection only — the JumpIf stays emitted
    // (it is a jump target of chained `||`/`&&` arms). Declined under step
    // metering, exactly as the region path: the fused branch would skip the
    // JumpIf block's charge. All targets are in-function here, so the taken
    // edge is `labels[target]` and the fallthrough is `labels[ip + 2]` — no
    // exit stubs.
    let cmp_branch_pair = |ip: usize, dst: u16| -> Option<(bool, u32)> {
        if !mem_cmp_fuse_enabled() || blocks.is_some() || ip + 1 >= n {
            return None;
        }
        match proto.code[ip + 1] {
            Instr::JumpIfFalse { cond, target } if cond == dst => Some((true, target)),
            Instr::JumpIfTrue { cond, target } if cond == dst => Some((false, target)),
            _ => None,
        }
    };
    for ip in 0..n {
        dynasm!(ops ; => labels[ip]);
        if let Some((m, bl)) = blocks.as_ref() {
            if let Some(&len) = bl.get(&ip) {
                let stub = ops.new_dynamic_label();
                crate::codegen::meter::emit_charge(&mut ops, m, len, stub);
                meter_stubs.push((stub, ip));
            }
        }
        // B206 yield-with-entry: this ip heads a loop a live REG-homed
        // region owns — exit to the interpreter here (its back-edge OSRs
        // straight into the region), so the whole-fn body serves calls and
        // the prologue while the region keeps the loop. Control falls into
        // the bail; the ops after it still emit (dead but label-linked).
        // Metered bodies never carry heads (the caller passes none): a
        // charge-then-bail would double-charge on replay.
        if !yield_heads.is_empty() && yield_heads.contains(&(ip as u32)) {
            dynasm!(ops
                ; mov DWORD [rsi], ip as i32
                ; jmp => epilogue
            );
        }
        // These bytecodes were materialized exactly by the fused captured-int
        // helper at their only predecessor. No internal jump may target them
        // (the plan checked `targeted`), and metered compilation never creates
        // a plan, so sharing their labels with the next live op is safe.
        if upval_inc_covered[ip] || upval_xorshift_covered[ip] || global_xorshift_covered[ip] {
            continue;
        }
        if random_fuse_covered[ip] {
            continue;
        }
        // B205: the fused `Math.random()*k|0` window. Guards then commit;
        // any miss resumes the interpreter at this ip having changed
        // nothing. Doubles are materialized EXACTLY (u32 -> f64 is exact;
        // u/2^32 and its *k stay under 2^53), and the Int result is the
        // integer identity floor(u*k/2^32) via one 64-bit mul.
        if let Some(fp) = random_fuse.get(&ip).copied() {
            if meter.is_none() && random_fuse_covered.get(ip + 1) == Some(&true) {
                let fb = ops.new_dynamic_label();
                let inv = (1.0f64 / 4294967296.0).to_bits();
                let kf = (fp.k as f64).to_bits();
                let math_idx = (fp.math_bits & 0xFFFF_FFFF) as i32;
                let random_idx = (fp.random_bits & 0xFFFF_FFFF) as i32;
                dynasm!(ops
                    // B207 (review): the lane raw-accesses the CALLEE's
                    // state global, which the caller's entry revalidation
                    // never scans — the route-epoch guard is what declines
                    // after any globalThis own-property redefinition (the
                    // same guard the cross3 lane carries).
                    ; cmp DWORD [rdi + crate::vm::host_api::JIT_GLOBAL_ROUTE_EPOCH_OFFSET as i32], 0
                    ; jne => fb
                    // Math binding by VALUE + heap VERSION (a recycled index
                    // bumps its version, so bits-equality can never match a
                    // different occupant — the review's ABA finding), its
                    // settled shape, then the random own-slot by VALUE +
                    // VERSION (the B193 shape->vals->value form, hardened).
                    ; mov rax, [r12 + (fp.math_slot as i32) * 8]
                    ; mov r10, QWORD fp.math_bits as i64
                    ; cmp rax, r10
                    ; jne => fb
                    ; cmp DWORD [r13 + math_idx * 4], fp.math_ver as i32
                    ; jne => fb
                    ; mov r10d, eax
                    ; mov r11, [rdi + crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32]
                    ; lea r11, [r11 + r10 * 8]
                    ; cmp DWORD [r11 + r10 * 8], fp.math_shape as i32
                    ; jne => fb
                    ; mov r11, [r11 + r10 * 8 + crate::vm::host_api::JIT_HOT_VALS_OFF as i32]
                    ; mov rax, [r11 + (fp.random_slot as i32) * 8]
                    ; mov r10, QWORD fp.random_bits as i64
                    ; cmp rax, r10
                    ; jne => fb
                    ; cmp DWORD [r13 + random_idx * 4], fp.random_ver as i32
                    ; jne => fb
                    // State slot: Int-tagged (the first call after the
                    // double-literal seed bails once and settles it).
                    ; mov rax, [r12 + (fp.state_slot as i32) * 8]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, 0x7FF9
                    ; jne => fb
                );
                // B205 stage 2: the captured alphabet's identity, through
                // the activation upvals and the cell-mirror authority; the
                // baked k is that immutable string's length.
                if let Some(ua) = fp.upval_alph {
                    let alph_idx = (ua.alph_bits & 0xFFFF_FFFF) as i32;
                    dynasm!(ops
                        ; mov rcx, [rdi + crate::vm::host_api::JIT_ACT_UPVALS_OFFSET as i32]
                        ; test rcx, rcx
                        ; jz => fb
                        ; mov ecx, [rcx + (ua.upval_idx as i32) * 4]
                        ; mov r11, [rdi + crate::vm::host_api::JIT_CELL_MIRROR_RAW_OFFSET as i32]
                        ; mov rcx, [r11 + rcx * 8]
                        ; mov r10, QWORD ua.alph_bits as i64
                        ; cmp rcx, r10
                        ; jne => fb
                        // B207: the alphabet string's version (ABA guard).
                        ; cmp DWORD [r13 + alph_idx * 4], ua.alph_ver as i32
                        ; jne => fb
                        ; mov [rbx + dreg(ua.dst_alph_b)], r10
                    );
                }
                for &(kind, amt) in fp.shifts.iter() {
                    dynasm!(ops ; mov ecx, eax);
                    match kind {
                        0 => dynasm!(ops ; shl ecx, amt as i8),
                        2 => dynasm!(ops ; shr ecx, amt as i8),
                        _ => dynasm!(ops ; sar ecx, amt as i8),
                    }
                    dynasm!(ops ; xor eax, ecx);
                }
                dynasm!(ops
                    // Commit the new state (int-box) — all guards passed.
                    ; mov ecx, eax
                    ; mov r10, QWORD INT_TAG as i64
                    ; or r10, rcx
                    ; mov [r12 + (fp.state_slot as i32) * 8], r10
                    // Result: floor(u * k / 2^32), u = state >>> 0.
                    ; mov ecx, eax
                    ; imul rcx, rcx, fp.k
                    ; shr rcx, 32
                    ; mov r10, QWORD INT_TAG as i64
                    ; or r10, rcx
                    ; mov [rbx + dreg(fp.dst_res)], r10
                    // Materialize the window's intermediates in order.
                    ; mov r10, QWORD fp.math_bits as i64
                    ; mov [rbx + dreg(fp.dst_math)], r10
                    ; mov ecx, eax
                    ; cvtsi2sd xmm0, rcx
                    ; mov r10, QWORD inv as i64
                    ; movq xmm1, r10
                    ; mulsd xmm0, xmm1
                    ; movq r10, xmm0
                    ; mov [rbx + dreg(fp.dst_random)], r10
                    ; mov r10, QWORD kf as i64
                    ; movq xmm1, r10
                    ; mulsd xmm0, xmm1
                    ; movq r10, xmm0
                    ; mov [rbx + dreg(fp.dst_prod)], r10
                    ; mov r10, QWORD (INT_TAG | (fp.k as u32 as u64)) as i64
                    ; mov [rbx + dreg(fp.dst_k)], r10
                    ; mov r10, QWORD INT_TAG as i64
                    ; mov [rbx + dreg(fp.dst_zero)], r10
                );
                emit_region_bail(&mut ops, ip, fb, epilogue);
                continue;
            }
        }
        if let Some(dst) = block_sroa.alloc_dst.get(ip).copied().flatten() {
            // Kill any stale heap root in the allocation register while
            // preserving the original instruction index and branch labels.
            dynasm!(ops
                ; mov rax, QWORD INT_TAG as i64
                ; mov [rbx + dreg(dst)], rax
            );
            continue;
        }
        if let Some(src) = block_sroa.read_src.get(ip).copied().flatten() {
            let (dst, prop_site) = match proto.code[ip] {
                Instr::GetProp { dst, .. } => (dst, true),
                Instr::GetIndex { dst, .. } => (dst, false),
                _ => return None,
            };
            dynasm!(ops
                ; mov rax, [rbx + dreg(src)]
                ; mov [rbx + dreg(dst)], rax
            );
            // Jit::compile reserved this original GetProp site even though the
            // projection no longer probes it. Keep every later site aligned.
            if prop_site {
                ic_site += 1;
            }
            continue;
        }
        // Each op gets its OWN dedicated bail label (records THIS ip); a guard
        // miss resumes the interpreter exactly here, side-effect-free.
        let bail = ops.new_dynamic_label();
        match proto.code[ip] {
            Instr::LoadInt { dst, val } => {
                let boxed = INT_TAG | (val as u32 as u64);
                dynasm!(ops
                    ; mov rax, QWORD boxed as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadBool { dst, val } => {
                let bits = BOOL_TAG | (val as u64);
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadNull { dst } => {
                dynasm!(ops
                    ; mov rax, QWORD Value::NULL.bits() as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadUndefined { dst } => {
                dynasm!(ops
                    ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadConst { dst, idx } => {
                // Numeric / single-ASCII-char (interned slot) / pre-interned
                // multi-char string (bits rooted in jit_const_strings). Mirrors
                // the region LoadConst arm. mem_can_compile gated the kinds.
                let c = proto.constants[idx as usize];
                let bits = single_char_const_bits(proto, c)
                    .or_else(|| const_strs.get(&idx).copied())
                    .unwrap_or_else(|| c.bits());
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::Move { dst, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::MakeFunc { dst, .. } => {
                // The helper re-reads this immutable site and validates the
                // active callable's exact function id before allocating.  A
                // decline is therefore a pure prefix and resumes MakeFunc;
                // any panic after allocation is fail-stop in the helper.
                let helper = crate::vm::jit_make_func as usize;
                let packed_fip = ((func_id as u64) << 32) | ip as u64;
                if let Some(&child) = plain_makefunc.get(&ip) {
                    // B257 plain lane: with the realm and eval-scope side
                    // tables both empty (two VM bytes) the full helper's
                    // answer is fixed — realm 0, no EvalScope — so a
                    // poll-and-allocate helper with the child id baked
                    // replaces the activation lookup. Either path returns
                    // the callable's bits or the deopt sentinel, so the
                    // join shares one check and one store.
                    use crate::vm::host_api::{
                        JIT_EVAL_SCOPE_NONEMPTY_OFFSET, JIT_OBJ_REALM_NONEMPTY_OFFSET,
                    };
                    let plain = crate::vm::jit_make_func_plain as usize;
                    let full = ops.new_dynamic_label();
                    let join = ops.new_dynamic_label();
                    dynasm!(ops
                        ; cmp BYTE [rdi + JIT_OBJ_REALM_NONEMPTY_OFFSET as i32], 0
                        ; jne => full
                        ; cmp BYTE [rdi + JIT_EVAL_SCOPE_NONEMPTY_OFFSET as i32], 0
                        ; jne => full
                        ; mov rcx, rdi
                        ; mov rdx, QWORD child as i64
                        ; mov rax, QWORD plain as i64
                        ; call rax
                        ; jmp => join
                        ; => full
                        ; mov rcx, rdi
                        ; mov rdx, QWORD packed_fip as i64
                        ; mov rax, QWORD helper as i64
                        ; call rax
                        ; => join
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                } else {
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, QWORD packed_fip as i64
                        ; mov rax, QWORD helper as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                }
                // The allocation can move all backing Vec storage even though
                // heap indices themselves are stable.
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::MakeCell { .. }
            | Instr::MakeCellTdz { .. }
            | Instr::MakeCellFnName { .. }
            | Instr::MarkCellConst { .. } => {
                // Capture-cell ops write their fresh cell back INTO the live
                // window through the validated pointer, so success needs no
                // destination store here. The helper re-reads the immutable
                // site and validates the activation before any effect.
                let helper = crate::vm::jit_make_cell as usize;
                let packed_fip = ((func_id as u64) << 32) | ip as u64;
                let window = proto.reg_count.max(1) as u64;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD packed_fip as i64
                    ; mov r9, QWORD window as i64
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::MakeClosure { dst, .. } | Instr::MakeArrow { dst, .. } => {
                // Real closures/arrows: the helper resolves capture cells from
                // the live window and the verified activation, and replicates
                // the interpreter's lexical this/home/new.target/EvalScope
                // inheritance before returning the fresh callable's bits.
                let helper = crate::vm::jit_make_closure as usize;
                let packed_fip = ((func_id as u64) << 32) | ip as u64;
                let window = proto.reg_count.max(1) as u64;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD packed_fip as i64
                    ; mov r9, QWORD window as i64
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::CellGet { dst, cell } => {
                // The region path's exact pure cell read; a TDZ read declines
                // to the interpreter's ReferenceError. No alloc → no refetch.
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(cell)]
                    ; mov rax, QWORD heap.cell_get as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::CellSet { cell, src } => {
                // The region path's unconditional cell store (its internal
                // nursery barrier is the single commit). No alloc.
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(cell)]
                    ; mov r8, [rbx + dreg(src)]
                    ; mov rax, QWORD heap.cell_set as i64
                    ; call rax
                );
            }
            Instr::CellSetChecked { cell, src } => {
                // TDZ-checked store: the decline precedes the write, so the
                // interpreter replays its exact ReferenceError.
                let helper = crate::vm::jit_cell_set_tdz_checked as usize;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(cell)]
                    ; mov r8, [rbx + dreg(src)]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::SetIndex { obj, key, val } => {
                // The region path's generic element write: dense store/grow and
                // unpinned-TypedArray number stores; observable coercion or
                // exotic receivers deopt. A grow REALLOCATES the dense Vec, so
                // pinned tables refetch exactly as after any allocating helper.
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(obj)]
                    ; mov r8, [rbx + dreg(key)]
                    ; mov r9, [rbx + dreg(val)]
                    ; mov rax, QWORD heap.set_index as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::CallMethodComputed {
                dst,
                obj,
                key,
                arg_base,
                argc,
            } => {
                // The region path's guarded dense `array[index](args…)`: only a
                // present own dense slot holding a plain Func/Closure frame-
                // calls; every miss is a pure prefix the interpreter replays.
                let packed_fip = ((func_id as u64) << 32) | ip as u64;
                let packed_args = ((obj as u64) << 32) | ((key as u64) << 16) | arg_base as u64;
                emit_region_call_ic(
                    &mut ops,
                    ip,
                    bail,
                    epilogue,
                    heap.call_method_computed_dense,
                    packed_fip,
                    packed_args,
                    argc,
                    dst,
                    refetch,
                    None,
                );
            }
            Instr::ArrayCtor {
                dst,
                callee,
                arg_base,
                argc,
                ..
            } => {
                // Dense `new Array(…)` subset; RangeError/sparse lengths and
                // malformed windows decline before any allocation.
                let helper = crate::vm::jit_array_ctor as usize;
                let mut packed = ((proto.reg_count.max(1) as u64) << 32)
                    | ((arg_base as u64) << 16)
                    | argc as u64;
                // Bit 63 says r9 is an observable captured callee. Never use a
                // JS Value as the sentinel: `Array = undefined` is a valid miss.
                if callee.is_some() {
                    packed |= 1u64 << 63;
                }
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD packed as i64
                );
                if let Some(callee) = callee {
                    dynasm!(ops ; mov r9, [rbx + dreg(callee)]);
                } else {
                    dynasm!(ops ; mov r9, QWORD Value::UNDEFINED.bits() as i64);
                }
                dynasm!(ops
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::GetIterator { dst, src } => {
                // Pristine dense-array identity only; every observable
                // protocol step declines. Pure → no refetch.
                let helper = crate::vm::jit_get_iterator as usize;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(src)]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::IterPrime { dst, iter } => {
                // Built-in iterables prime `undefined` with no observable get.
                let helper = crate::vm::jit_iter_prime as usize;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(iter)]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::IterNext {
                value_dst,
                done_dst,
                iter,
                idx,
                next,
            } => {
                // The REGION path's `jit_iter_next` verbatim: the dense-array
                // positional walk plus the intrinsic array/collection/regexp
                // iterator steps; everything else deopts before state moves.
                // The helper runs the loop safe point and can allocate, so
                // pinned tables re-derive afterwards.
                let packed = ((iter as u64) << 48)
                    | ((next as u64) << 32)
                    | ((value_dst as u64) << 16)
                    | done_dst as u64;
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // frame register window
                    ; mov r8, QWORD packed as i64         // iter/next/value/done regs
                    ; mov r9d, idx as i32                 // dense-Array cursor reg
                    ; mov rax, QWORD heap.iter_next as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // non-intrinsic → redo in interp
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                          // threw → unwind, NOT redo
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::PushFinally {
                target,
                kind_reg,
                val_reg,
            } => {
                // The region path's total handler-stack push. Tier-C bodies
                // with handler ops never receive cross entries, so the active
                // frame is always this activation's own.
                let packed = ((target as u64) << 32) | ((kind_reg as u64) << 16) | val_reg as u64;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, QWORD packed as i64
                    ; mov rax, QWORD heap.push_finally as i64
                    ; call rax
                );
            }
            Instr::PopFinally => {
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rax, QWORD heap.pop_finally as i64
                    ; call rax
                );
            }
            Instr::EndFinally { kind_reg, .. } => {
                // Only the NORMAL completion falls through natively; abrupt
                // completions decline into the interpreter's routing.
                let helper = crate::vm::jit_end_finally as usize;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(kind_reg)]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::IterCloseFinally { kind_reg, .. } => {
                // Normal/jump completions do not close; return/throw decline
                // (IteratorClose is observable user code).
                let helper = crate::vm::jit_iter_close_finally as usize;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(kind_reg)]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::HasProp {
                dst,
                key,
                obj,
                brand: false,
            } => {
                // The region path's read-only `jit_has_property` helper: the
                // full non-Proxy [[HasProperty]] walk, byte-identical to the
                // interpreter on the served chains; a Proxy anywhere, an
                // object key's observable ToString, or a non-object RHS
                // declines so the interpreter re-executes. PURE — no refetch.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(key)]         // key bits
                    ; mov r8, [rbx + dreg(obj)]          // obj bits
                    ; mov rax, QWORD heap.has_property as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::ObjectKeys {
                dst,
                obj,
                callee,
                this_v,
            } => {
                // Ordinary-object/array own-key snapshot (allocates; no user
                // code). Proxies and primitives decline purely.
                let helper = crate::vm::jit_object_keys as usize;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(obj)]
                    ; mov r8, [rbx + dreg(callee)]
                    ; mov r9, [rbx + dreg(this_v)]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::NewObject { dst, hint } => {
                // Allocation is a GC safe point.  The native frame's Values
                // live in vm.regs, and the helper reproduces realm_born before
                // returning the new object's bits.  A defensive decline is
                // still a pure prefix: the unreachable fresh allocation has
                // not escaped and the interpreter may replay NewObject.
                let helper = crate::vm::jit_new_object as usize;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov edx, hint as i32
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                // Heap::alloc can reallocate the version vector.  Functions
                // with property ICs or leaf guards pin r13/r14 and must refresh
                // them before the next emitted op.
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::NewPlannedObject { dst, plan } => {
                // The helper validates the unified FuncProto id and plan index
                // before its GC/allocation commit point. Packing uses the same
                // immutable-id convention as AppendDataProp.
                let helper = crate::vm::jit_new_planned_object as usize;
                let packed = ((func_id as u64) << 32) | plan as u64;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, QWORD packed as i64
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::NewArray {
                dst,
                arg_base,
                argc,
            } => {
                // Fixed-block literal only.  The helper validates that this
                // entire window lies inside vm.regs, collects before copying
                // the rooted Values, allocates, and applies current-realm
                // Array provenance.
                let helper = crate::vm::jit_new_array as usize;
                let packed =
                    ((proto.reg_count as u64) << 32) | ((arg_base as u64) << 16) | argc as u64;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD packed as i64
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::AppendDataProp { obj, name, val } => {
                // Pack the immutable unified function id with its static
                // name slot. The helper verifies exact Object kind before
                // its barrier and interpreter-equivalent push_data.
                let helper = crate::vm::jit_append_data_prop as usize;
                let packed_name = ((func_id as u64) << 32) | name as u64;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(obj)]
                    ; mov r8, QWORD packed_name as i64
                    ; mov r9, [rbx + dreg(val)]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::FinalizeObject {
                dst,
                plan,
                val_base,
                count,
            } => {
                // One helper call allocates AND fully populates the literal
                // from the staged register block — the values are live window
                // slots, so the helper's pre-allocation safe point sees them
                // as ordinary GC roots. Validation is a pure prefix; the
                // committed object is complete before the helper returns, so
                // no bail can ever observe partial initialization.
                // B182: bake the plan address and the shape fold when the
                // compile can resolve them — the fold over the plan's keys
                // with data bits is exactly what `finalize_shape` memoizes
                // (same fold, same thread-local transition tree), so the two
                // forms are observably identical; the baked helper keeps only
                // the checks a compile cannot freeze (window bounds, GC poll).
                let baked = if super::finalize_baked_enabled() {
                    proto
                        .static_key_plans
                        .get(plan as usize)
                        .filter(|pl| {
                            pl.runtime_valid()
                                && pl.len() == count as usize
                                && !pl.has_element_key()
                                && (count as usize) < crate::heap::PROP_INDEX_THRESHOLD
                        })
                        .map(|pl| {
                            let data = crate::shape::attr_bits(true, true, true, false);
                            let mut shape = crate::shape::EMPTY;
                            for key in pl.keys() {
                                shape = crate::shape::add(shape, key, data);
                            }
                            (pl as *const crate::bytecode::StaticKeyPlan as u64, shape)
                        })
                } else {
                    None
                };
                if let Some((plan_ptr, shape)) = baked {
                    let packed = ((shape as u64) << 32) | ((val_base as u64) << 16) | count as u64;
                    // B257: the thin helper validates only the slots it
                    // reads, so the window-bound the baked helper checks per
                    // call is proved here instead and the 5th (stack)
                    // argument is gone; the values go window -> slab in one
                    // copy. `ZIPP_NO_THIN_ALLOC` keeps the baked form.
                    let thin = crate::heap::thin_alloc_enabled()
                        && (val_base as usize + count as usize) <= proto.reg_count as usize;
                    if thin {
                        let helper = crate::vm::jit_finalize_object_thin as usize;
                        dynasm!(ops
                            ; mov rcx, rdi
                            ; mov rdx, rbx
                            ; mov r8, QWORD plan_ptr as i64
                            ; mov r9, QWORD packed as i64
                            ; mov rax, QWORD helper as i64
                            ; call rax
                            ; mov r10, QWORD SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => bail
                            ; mov [rbx + dreg(dst)], rax
                        );
                    } else {
                        let helper = crate::vm::jit_finalize_object_baked as usize;
                        dynasm!(ops
                            ; mov rcx, rdi
                            ; mov rdx, rbx
                            ; mov r8, QWORD plan_ptr as i64
                            ; mov r9, QWORD packed as i64
                            ; mov DWORD [rsp + 32], proto.reg_count as i32
                            ; mov rax, QWORD helper as i64
                            ; call rax
                            ; mov r10, QWORD SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => bail
                            ; mov [rbx + dreg(dst)], rax
                        );
                    }
                } else {
                    let helper = crate::vm::jit_finalize_object as usize;
                    let packed_plan = ((func_id as u64) << 32) | plan as u64;
                    let packed_window =
                        ((proto.reg_count as u64) << 32) | ((val_base as u64) << 16) | count as u64;
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, rbx
                        ; mov r8, QWORD packed_plan as i64
                        ; mov r9, QWORD packed_window as i64
                        ; mov rax, QWORD helper as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                }
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::SetHomeObject { method, home } => {
                // The interpreter deliberately ignores a non-heap method.
                // The helper duplicates that no-op, validates heap indices
                // before mutation, then publishes the barrier-backed home
                // edge.  A post-commit panic aborts instead of replaying it.
                let helper = crate::vm::jit_set_home_object as usize;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(method)]
                    ; mov r8, [rbx + dreg(home)]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::LooseEq { dst, a, b } => {
                let helper = crate::vm::jit_loose_null_eq as usize;
                if crate::vm::tierc_loose_null_inline_enabled() {
                    use crate::vm::host_api::{
                        JIT_HTMLDDA_IDX_OFFSET, JIT_HTMLDDA_SCALAR_ENABLED_OFFSET,
                    };
                    let a_nullish = ops.new_dynamic_label();
                    let b_nullish = ops.new_dynamic_label();
                    let check_other = ops.new_dynamic_label();
                    let equal = ops.new_dynamic_label();
                    let not_equal = ops.new_dynamic_label();
                    let heap_slow = ops.new_dynamic_label();
                    let done = ops.new_dynamic_label();
                    // Admission restricts this to an adjacent `x == null`, but
                    // recheck both live operands so an unusual internal entry
                    // still declines exactly like the helper. Once one is
                    // proven nullish, the other is equal iff it is nullish or
                    // the singleton [[IsHTMLDDA]] exotic. The scalar-disabled
                    // heap edge preserves the HashSet/counter ablation through
                    // the incumbent helper; all ordinary values are call-free.
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(a)]
                        ; mov r10, [rbx + dreg(b)]
                        ; mov r11, QWORD Value::NULL.bits() as i64
                        ; cmp rax, r11
                        ; je => a_nullish
                        ; mov r11, QWORD Value::UNDEFINED.bits() as i64
                        ; cmp rax, r11
                        ; je => a_nullish
                        ; mov r11, QWORD Value::NULL.bits() as i64
                        ; cmp r10, r11
                        ; je => b_nullish
                        ; mov r11, QWORD Value::UNDEFINED.bits() as i64
                        ; cmp r10, r11
                        ; je => b_nullish
                        ; jmp => bail
                        ; => a_nullish
                        ; mov rax, r10
                        ; jmp => check_other
                        ; => b_nullish
                        // rax already holds the other operand.
                        ; => check_other
                        ; mov r11, QWORD Value::NULL.bits() as i64
                        ; cmp rax, r11
                        ; je => equal
                        ; mov r11, QWORD Value::UNDEFINED.bits() as i64
                        ; cmp rax, r11
                        ; je => equal
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, TAG_HEAP_HI as i32
                        ; jne => not_equal
                        ; cmp BYTE [rdi + JIT_HTMLDDA_SCALAR_ENABLED_OFFSET as i32], 0
                        ; je => heap_slow
                        ; cmp eax, DWORD [rdi + JIT_HTMLDDA_IDX_OFFSET as i32]
                        ; je => equal
                        ; => not_equal
                        ; mov rax, QWORD Value::FALSE.bits() as i64
                        ; jmp => done
                        ; => equal
                        ; mov rax, QWORD Value::TRUE.bits() as i64
                        ; jmp => done
                        ; => heap_slow
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(a)]
                        ; mov r8, [rbx + dreg(b)]
                        ; mov rax, QWORD helper as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; => done
                        ; mov [rbx + dreg(dst)], rax
                    );
                } else {
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(a)]
                        ; mov r8, [rbx + dreg(b)]
                        ; mov rax, QWORD helper as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::DeleteProp { .. } => {
                // Intentional cold exit.  Do not even read the receiver here:
                // interpreter replay at this exact ip owns all observable
                // coercion/proxy/strict-delete behaviour and writes `dst`.
                dynasm!(ops ; jmp => bail);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::LoadGlobal { dst, idx } => {
                if let Some(plan) = global_xorshift[ip] {
                    // The normal Tier-C global route has already proved this
                    // is a live, direct slot. One Int guard at the first read
                    // replaces the twelve generic ToInt32 checks in a three-
                    // step chain. A miss is a pure exact-ip interpreter replay.
                    dynasm!(ops
                        ; mov rax, [r12 + (plan.idx as i32) * 8]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => bail
                    );
                    emit_tierc_xorshift_registers(&mut ops, &plan.steps[..usize::from(plan.count)]);
                    dynasm!(ops
                        ; mov [r12 + (plan.idx as i32) * 8], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                    let resume = ip + usize::from(plan.count) * 6;
                    dynasm!(ops ; jmp => labels[resume]);
                } else {
                    dynasm!(ops
                        ; mov rax, [r12 + (idx as i32) * 8]
                        ; mov [rbx + dreg(dst)], rax
                    );
                }
            }
            Instr::UpvalGet { dst, idx } => {
                if let Some(plan) = upval_inc[ip] {
                    // One cell resolution replaces the exact six-op captured
                    // counter sequence. Failure is a pure prefix and resumes at
                    // this UpvalGet. Success returns OLD Int bits; materialize
                    // every skipped destination in bytecode order so a later
                    // unrelated bail observes the same register file.
                    let add_overflow = ops.new_dynamic_label();
                    let add_done = ops.new_dynamic_label();
                    let inc_slow = ops.new_dynamic_label();
                    let inc_have_old = ops.new_dynamic_label();
                    let one = Value::int(1).bits();
                    let zero = Value::int(0).bits();
                    let overflow_sum = Value::num(i32::MAX as f64 + 1.0).bits();
                    // B201: the mirror is the cell authority, so the whole
                    // counter increment emits inline when the activation has
                    // an upvalue base, no const/fn-name cell exists, and the
                    // cell holds an Int — mirror load, low-32 wrap-inc,
                    // int-box, mirror store. Every other case takes the
                    // unchanged helper as a pure prefix. Both paths leave
                    // the OLD bits in rax for the shared materialization.
                    dynasm!(ops
                        ; mov rax, [rdi + crate::vm::host_api::JIT_ACT_UPVALS_OFFSET as i32]
                        ; test rax, rax
                        ; jz => inc_slow
                        ; mov ecx, [rax + (plan.idx as i32) * 4]
                        ; cmp BYTE [rdi + crate::vm::host_api::JIT_CONST_CELLS_NE_OFFSET as i32], 0
                        ; jne => inc_slow
                        ; cmp BYTE [rdi + crate::vm::host_api::JIT_FN_NAME_CELLS_NE_OFFSET as i32], 0
                        ; jne => inc_slow
                        ; mov r11, [rdi + crate::vm::host_api::JIT_CELL_MIRROR_RAW_OFFSET as i32]
                        ; mov rax, [r11 + rcx * 8]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, 0x7FF9
                        ; jne => inc_slow
                        ; lea edx, [eax + 1]
                        ; mov r10, QWORD Value::int(0).bits() as i64
                        ; or r10, rdx
                        ; mov [r11 + rcx * 8], r10
                        ; jmp => inc_have_old
                        ; => inc_slow
                        ; mov rcx, rdi
                        ; mov edx, plan.idx as i32
                        ; mov rax, QWORD heap.tierc_upval_inc_i32 as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; => inc_have_old
                        ; mov r11, rax
                        ; mov [rbx + dreg(plan.old_dst)], rax
                        ; mov rax, QWORD one as i64
                        ; mov [rbx + dreg(plan.one_dst)], rax
                        ; mov eax, r11d
                        ; cmp eax, i32::MAX
                        ; je => add_overflow
                        ; add eax, 1
                    );
                    box_eax(&mut ops, plan.add_dst);
                    dynasm!(ops
                        ; jmp => add_done
                        ; => add_overflow
                        ; mov rax, QWORD overflow_sum as i64
                        ; mov [rbx + dreg(plan.add_dst)], rax
                        ; => add_done
                        ; mov rax, QWORD zero as i64
                        ; mov [rbx + dreg(plan.zero_dst)], rax
                        ; mov eax, r11d
                        ; add eax, 1
                    );
                    box_eax(&mut ops, plan.new_dst);
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                    dynasm!(ops ; jmp => labels[ip + 6]);
                } else if let Some(plan) = upval_xorshift[ip] {
                    // Resolve the exact live cell once, require an Int before
                    // any mutation, and commit the bounded generic transform.
                    // The helper returns the old Int bits. The emitted straight
                    // line then reconstructs every skipped register write in
                    // bytecode order; no guard remains after the commit.
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov edx, plan.idx as i32
                        ; mov r8, QWORD plan.packed as i64
                        ; mov rax, QWORD crate::vm::jit_tierc_upval_xorshift_i32 as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                    );
                    emit_tierc_xorshift_registers(&mut ops, &plan.steps[..usize::from(plan.count)]);
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                    let resume = ip + usize::from(plan.count) * 6;
                    dynasm!(ops ; jmp => labels[resume]);
                } else if let Some(src) = upval_forward[ip] {
                    // The preceding helper successfully loaded/stored these
                    // exact bits. Guard the internal UNINITIALIZED sentinel so
                    // even malformed bytecode that stored it still replays this
                    // UpvalGet and throws instead of exposing the sentinel.
                    let uninit = Value::UNINITIALIZED.bits();
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(src)]
                        ; mov r10, QWORD uninit as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else if tierc_upval_inline_enabled() {
                    // B189 inline captured read — no helper round-trip. The
                    // activation caches the closure's upvalue base at entry
                    // (0 = none → bail), the cell index is one u32 load, and
                    // the cell-value mirror (write-through at `cell_set`, the
                    // codebase's single Cell-payload write) yields the value.
                    // The UNINITIALIZED sentinel travels through the mirror
                    // verbatim, so TDZ still replays at this exact ip and the
                    // interpreter supplies the ReferenceError.
                    let act_off = crate::vm::host_api::JIT_ACT_UPVALS_OFFSET as i32;
                    let mirror_off = crate::vm::host_api::JIT_CELL_MIRROR_RAW_OFFSET as i32;
                    let uninit = Value::UNINITIALIZED.bits();
                    dynasm!(ops
                        ; mov rax, [rdi + act_off]
                        ; test rax, rax
                        ; jz => bail
                        ; mov eax, [rax + (idx as i32) * 4]
                        ; mov r10, [rdi + mirror_off]
                        ; mov rax, [r10 + rax * 8]
                        ; mov r10, QWORD uninit as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else {
                    // Tier-C's activation may be frame-free (native cross-call),
                    // so this helper resolves the explicitly installed closure
                    // rather than the interpreter frame stack. TDZ/malformed →
                    // exact-ip bail and interpreter replay.
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov edx, idx as i32
                        ; mov rax, QWORD heap.tierc_upval_get as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                }
            }
            Instr::UpvalSet { idx, src } => {
                // Pure-prefix failure for immutable/TDZ cells; the interpreter
                // resumes at this op and supplies the exact PutValue semantics.
                // B201 stage 3: a NON-HEAP source stores inline through the
                // cell-mirror authority — activation upvals → cell index, the
                // sticky const/fn-name nonempty bytes decline, the old value's
                // UNINITIALIZED check keeps TDZ exact, and a heap-tagged src
                // takes the helper (it owns the write barrier). Both paths
                // leave nothing in registers the tail needs.
                let us_slow = ops.new_dynamic_label();
                let us_done = ops.new_dynamic_label();
                let uninit = Value::UNINITIALIZED.bits();
                dynasm!(ops
                    ; mov rax, [rdi + crate::vm::host_api::JIT_ACT_UPVALS_OFFSET as i32]
                    ; test rax, rax
                    ; jz => us_slow
                    ; mov ecx, [rax + (idx as i32) * 4]
                    ; cmp BYTE [rdi + crate::vm::host_api::JIT_CONST_CELLS_NE_OFFSET as i32], 0
                    ; jne => us_slow
                    ; cmp BYTE [rdi + crate::vm::host_api::JIT_FN_NAME_CELLS_NE_OFFSET as i32], 0
                    ; jne => us_slow
                    ; mov r8, [rbx + dreg(src)]
                    ; mov r10, r8
                    ; shr r10, 48
                    ; cmp r10d, 0x7FFD
                    ; je => us_slow
                    ; mov r11, [rdi + crate::vm::host_api::JIT_CELL_MIRROR_RAW_OFFSET as i32]
                    ; mov rax, [r11 + rcx * 8]
                    ; mov r10, QWORD uninit as i64
                    ; cmp rax, r10
                    ; je => us_slow
                    ; mov [r11 + rcx * 8], r8
                    ; jmp => us_done
                    ; => us_slow
                    ; mov rcx, rdi
                    ; mov edx, idx as i32
                    ; mov r8, [rbx + dreg(src)]
                    ; mov rax, QWORD heap.tierc_upval_set as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; => us_done
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::StoreGlobal { idx, src }
            | Instr::StoreGlobalStrict { idx, src }
            | Instr::StoreGlobalResolved { idx, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [r12 + (idx as i32) * 8], rax
                );
            }
            Instr::AddInt { dst, a, imm, .. } => {
                // Int fast path (the interpreter's `checked_add`), f64 fallback
                // on a non-Int operand or overflow. (Copied from the mem path.)
                let f64_path = ops.new_dynamic_label();
                let done_ai = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; jne => f64_path
                    ; add eax, imm
                    ; jo => f64_path
                );
                box_eax(&mut ops, dst);
                dynasm!(ops ; jmp => done_ai ; => f64_path);
                load_num_xmm(&mut ops, a, 0, bail);
                dynasm!(ops
                    ; mov eax, imm
                    ; cvtsi2sd xmm1, eax
                    ; addsd xmm0, xmm1
                );
                store_xmm(&mut ops, dst);
                dynasm!(ops ; => done_ai);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Neg { dst, a } => {
                // JS unary minus flips the IEEE-754 sign bit. Subtracting from
                // +0 would incorrectly turn `-(+0)` into +0, so share the exact
                // region-memory sequence and bail before effects on non-number.
                load_num_xmm(&mut ops, a, 1, bail);
                dynasm!(ops
                    ; mov rax, QWORD (1u64 << 63) as i64
                    ; movq xmm0, rax
                    ; xorpd xmm0, xmm1
                );
                store_xmm(&mut ops, dst);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Bitwise { dst, a, b, op } => {
                // ToInt32 both operands (Int payloads or exactly-integral
                // i32-range doubles — see `load_toint32`; everything else
                // bails), then the 32-bit op. x86 32-bit shifts mask the count
                // to 5 bits — exactly JS's `& 31`. Results always fit i32
                // (boxed Int) except `>>>`, whose u32 result may exceed
                // i32::MAX and is then boxed as an (exact) double.
                use crate::bytecode::BitwiseOp as B;
                load_toint32(&mut ops, a, bail);
                dynasm!(ops ; mov r8d, eax); // stash a
                load_toint32(&mut ops, b, bail);
                dynasm!(ops ; mov ecx, eax ; mov eax, r8d); // eax = a, ecx = b
                match op {
                    B::And => {
                        dynasm!(ops ; and eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Or => {
                        dynasm!(ops ; or eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Xor => {
                        dynasm!(ops ; xor eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Shl => {
                        dynasm!(ops ; shl eax, cl);
                        box_eax(&mut ops, dst);
                    }
                    B::Shr => {
                        dynasm!(ops ; sar eax, cl);
                        box_eax(&mut ops, dst);
                    }
                    B::Ushr => {
                        let as_dbl = ops.new_dynamic_label();
                        let done_u = ops.new_dynamic_label();
                        dynasm!(ops
                            ; shr eax, cl
                            ; test eax, eax
                            ; js => as_dbl                // u32 > i32::MAX → double
                        );
                        box_eax(&mut ops, dst);
                        dynasm!(ops
                            ; jmp => done_u
                            ; => as_dbl
                            ; mov eax, eax                // zero-extend u32 into rax
                            ; cvtsi2sd xmm0, rax          // exact (< 2^32)
                            ; movq rax, xmm0
                            ; mov [rbx + dreg(dst)], rax
                            ; => done_u
                        );
                    }
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Not { dst, a } => {
                // `!x`: a Bool flips its payload bit in place (the tag survives
                // the xor); anything else asks the read-only `jit_truthy`
                // helper (handles Int/double/heap incl. empty strings and
                // [[IsHTMLDDA]]) and flips its 0/1.
                let slow = ops.new_dynamic_label();
                let done_n = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, (INT_TAG_HI + 1) as i32   // Bool tag 0x7FFA
                    ; jne => slow
                    ; xor rax, 1                          // flip the payload bit
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => done_n
                    ; => slow
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // value bits
                    ; mov rax, QWORD heap.truthy as i64
                    ; call rax
                    ; xor rax, 1                          // !truthy
                    ; mov r8, QWORD BOOL_TAG as i64
                    ; or rax, r8
                    ; mov [rbx + dreg(dst)], rax
                    ; => done_n
                );
            }
            Instr::Sub { dst, a, b } => {
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Sub, int_hint)
            }
            Instr::Mul { dst, a, b } => {
                // `dbinop` excludes Mul from the int fast path (always f64), so no
                // overflow concern.
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Mul, int_hint)
            }
            Instr::Div { dst, a, b } => {
                // Always f64 — JS `/` has no integer form (mirrors the region arm
                // and the interpreter).
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Div, false)
            }
            Instr::Mod { dst, a, b } => {
                // Integer-valued Number remainder, copied from the MEM-region
                // path.  Every unsupported case exits at this exact ip before
                // observable coercion: the interpreter owns fractional/zero,
                // BigInt and object-ToNumeric semantics.
                load_num_xmm(&mut ops, a, 0, bail);
                load_num_xmm(&mut ops, b, 1, bail);
                let as_dbl = ops.new_dynamic_label();
                let mod_done = ops.new_dynamic_label();
                let rem_signed = ops.new_dynamic_label();
                dynasm!(ops
                    ; cvttsd2si rax, xmm0
                    ; cvttsd2si rcx, xmm1
                    ; test rcx, rcx
                    ; jz => bail
                    // Avoid the sole signed-idiv overflow (#DE): MIN / -1.
                    // Bailing for every divisor -1 also preserves ±0 exactly.
                    ; cmp rcx, -1
                    ; je => bail
                    ; cvtsi2sd xmm2, rax
                    ; ucomisd xmm2, xmm0
                    ; jp => bail
                    ; jne => bail
                    ; cvtsi2sd xmm2, rcx
                    ; ucomisd xmm2, xmm1
                    ; jp => bail
                    ; jne => bail
                    ; cqo
                    ; idiv rcx
                    // A zero remainder inherits the dividend's sign in JS.
                    ; test rdx, rdx
                    ; jnz => rem_signed
                    ; movq rax, xmm0
                    ; test rax, rax
                    ; js => bail
                    ; => rem_signed
                    ; movsxd r8, edx
                    ; cmp r8, rdx
                    ; jne => as_dbl
                    ; mov r8, QWORD INT_TAG as i64
                    ; mov eax, edx
                    ; or rax, r8
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => mod_done
                    ; => as_dbl
                    ; cvtsi2sd xmm0, rdx
                    ; movq rax, xmm0
                    ; mov [rbx + dreg(dst)], rax
                    ; => mod_done
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::CheckCoercible { src } => {
                // RequireObjectCoercible is a no-op except for null/undefined.
                // Exit before effects for those two values; the interpreter
                // constructs the exact TypeError at this bytecode.
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov r10, QWORD Value::NULL.bits() as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD Value::UNDEFINED.bits() as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::MathOp {
                dst,
                op,
                callee,
                this_v,
                arg_base,
                argc,
            } => emit_math_op(
                &mut ops,
                ip,
                bail,
                epilogue,
                dst,
                op,
                callee,
                this_v,
                arg_base,
                argc,
                heap.math_unary,
                heap.math_two,
                heap.math_imul_guard,
            ),
            Instr::GlobalFn {
                dst,
                op: crate::bytecode::GlobalFn::String,
                callee,
                arg_base,
                argc,
            } => {
                debug_assert_eq!(argc, 1);
                // The helper implements exactly primitive tagged-Int ToString.
                // Any other live value declines before effects and resumes the
                // interpreter at this GlobalFn bytecode, preserving Symbol and
                // observable object-coercion semantics.
                let helper = crate::vm::jit_int_string as usize;
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(arg_base)]
                    ; mov r8, [rbx + dreg(callee)]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    // 0..9 and 10..99 return permanently pinned single-char /
                    // pad2 strings without a safe point or allocation. Every
                    // allocated result has an index above the pinned prefix,
                    // so only that branch can have moved the version/IC bases.
                    let no_alloc = ops.new_dynamic_label();
                    dynasm!(ops
                        ; cmp eax, crate::heap::INTERN_PINNED_END as i32
                        ; jbe => no_alloc
                    );
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                    dynasm!(ops ; => no_alloc);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Add { dst, a, b } => {
                // Int+Int fast path, then f64, then the `jit_concat` fallback
                // (string concat / coercion — the interpreter's `add_values`),
                // which may allocate / run user code ⇒ refetch r13/r14 when
                // has_prop. (Copied from the region Add arm.)
                let slow = ops.new_dynamic_label();
                let f64_path = ops.new_dynamic_label();
                let done_a = ops.new_dynamic_label();
                if int_hint {
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(a)]
                        ; mov rcx, [rbx + dreg(b)]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => f64_path
                        ; mov r10, rcx
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => f64_path
                        ; add eax, ecx
                        ; jo => f64_path
                    );
                    box_eax(&mut ops, dst);
                    dynasm!(ops ; jmp => done_a);
                }
                dynasm!(ops ; => f64_path);
                load_num_xmm(&mut ops, a, 0, slow);
                load_num_xmm(&mut ops, b, 1, slow);
                dynasm!(ops ; addsd xmm0, xmm1);
                store_xmm(&mut ops, dst);
                dynasm!(ops
                    ; jmp => done_a
                    ; => slow
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]
                    ; mov r8, [rbx + dreg(b)]
                    ; mov rax, QWORD heap.concat as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                dynasm!(ops ; => done_a);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Lt { dst, a, b }
            | Instr::Le { dst, a, b }
            | Instr::Gt { dst, a, b }
            | Instr::Ge { dst, a, b }
            | Instr::Eq { dst, a, b }
            | Instr::Ne { dst, a, b } => {
                let cmp = match proto.code[ip] {
                    Instr::Lt { .. } => Cmp::Lt,
                    Instr::Le { .. } => Cmp::Le,
                    Instr::Gt { .. } => Cmp::Gt,
                    Instr::Ge { .. } => Cmp::Ge,
                    Instr::Eq { .. } => Cmp::Eq,
                    _ => Cmp::Ne,
                };
                // B118: Int+Int compare feeding the NEXT ip's JumpIf branches on
                // flags (bool still stored); non-Int falls through to the
                // unchanged generic sequence below.
                if let Some((iff, tgt)) = cmp_branch_pair(ip, dst) {
                    let t = labels[tgt as usize];
                    let ft = labels[ip + 2];
                    emit_fused_cmp_branch_head(&mut ops, dst, a, b, cmp, iff, t, ft);
                }
                match cmp {
                    Cmp::Eq => region_poly_eq(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        dst,
                        a,
                        b,
                        false,
                        heap.strict_eq,
                    ),
                    Cmp::Ne => region_poly_eq(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        dst,
                        a,
                        b,
                        true,
                        heap.strict_eq,
                    ),
                    _ => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, cmp),
                }
            }
            Instr::Jump { target } => {
                dynasm!(ops ; jmp => labels[target as usize]);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                // Int/Bool condition tests its payload directly; anything else
                // asks the read-only `jit_truthy` helper. (Copied from mem path.)
                let if_false = matches!(proto.code[ip], Instr::JumpIfFalse { .. });
                let t = labels[target as usize];
                let testit = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(cond)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32          // Int
                    ; je => testit
                    ; cmp r10d, (INT_TAG_HI + 1) as i32    // Bool
                    ; je => testit
                    ; mov rcx, rdi                         // vm
                    ; mov rdx, rax                         // value bits
                    ; mov rax, QWORD heap.truthy as i64
                    ; call rax                             // rax = 0/1
                    ; => testit
                    ; test eax, eax
                );
                if if_false {
                    dynasm!(ops ; jz => t);
                } else {
                    dynasm!(ops ; jnz => t);
                }
            }
            Instr::JumpIfNotLt { a, b, target } => {
                djump_if_not_cmp(
                    &mut ops,
                    ip,
                    bail,
                    epilogue,
                    a,
                    b,
                    Cmp::Lt,
                    labels[target as usize],
                );
            }
            Instr::JumpIfNotLe { a, b, target } => {
                djump_if_not_cmp(
                    &mut ops,
                    ip,
                    bail,
                    epilogue,
                    a,
                    b,
                    Cmp::Le,
                    labels[target as usize],
                );
            }
            Instr::GetIndex { dst, obj, key } => {
                // Generic element read `a[i]` via the win64 helper (dense arrays,
                // flat-ASCII strings, unpinned TypedArrays); `undefined` for
                // out-of-range, deopt sentinel for receivers/keys needing
                // interpreter semantics. No alloc / no user code → no re-fetch.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // array bits
                    ; mov r8, [rbx + dreg(key)]           // index bits
                    ; mov rax, QWORD heap.get_index as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::TypeOf { dst, a } => {
                // `typeof v` → a heap string (jit_typeof). Total (no deopt). The
                // downstream `=== "number"` compares by CONTENT (region_poly_eq
                // slow strict_eq), so a fresh alloc is correct. ALLOCATES ⇒
                // refetch r13/r14 after the store when has_prop.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // value bits
                    ; mov rax, QWORD heap.typeof_str as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
            }
            Instr::TypeOfIs { dst, a, code, neg } => {
                // Fused typeof compare — the region arm verbatim. PURE (no
                // alloc, no user code, total), so unlike `TypeOf` above it owes
                // no refetch.
                let code_neg = code as u32 | ((neg as u32) << 8);
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // value bits
                    ; mov r8d, code_neg as i32            // code | neg<<8
                    ; mov rax, QWORD heap.typeof_is as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax          // Bool Value bits
                );
            }
            Instr::TypeOfSame { dst, a, b, neg } => {
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(a)]
                    ; mov r8, [rbx + dreg(b)]
                    ; mov r9d, neg as i32
                    ; mov rax, QWORD heap.typeof_same as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::IsArray {
                dst,
                a,
                callee,
                this_v,
            } => {
                // `Array.isArray(v)` → Bool bits; deopt sentinel for the rare
                // throwing case (revoked Proxy → interpreter re-executes + throws,
                // safe to redo — the check is side-effect-free). Pure, no refetch.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // value bits
                    ; mov r8, [rbx + dreg(callee)]        // captured callee bits
                    ; mov r9, [rbx + dreg(this_v)]        // captured receiver bits
                    ; mov rax, QWORD heap.is_array as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::LenOf { dst, obj } => {
                // For-in key-snapshot / array / string length. Pure, total — no
                // deopt, no alloc, no refetch.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // obj bits
                    ; mov rax, QWORD heap.len_of as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::ForInKeys { dst, obj } => {
                // Materialise the for-in key snapshot Array (jit_forin_keys).
                // ALLOCATES ⇒ refetch r13/r14 after the store when has_prop. A
                // Proxy trap / coercion throw → CALL_THREW → unwind (no redo).
                // Sentinel checks BEFORE the store (side-effect-free at bail).
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // obj bits
                    ; mov rax, QWORD heap.forin_keys as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::ForInLive { dst, obj, key } => {
                // Per-op for-in liveness (jit_forin_live → Vm::forin_live). Never
                // deopts. Can run a Proxy `has` trap (user code) ⇒ refetch r13/r14
                // after the store when has_prop. (Copied from the region arm.)
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // obj bits
                    ; mov r8, [rbx + dreg(key)]           // key bits
                    ; mov rax, QWORD heap.forin_live as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax          // Bool Value bits
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
            }
            Instr::GetProp { dst, obj, name } => {
                // 8-way inline cache (call-free on hit), then the miss helper,
                // then the PROP_VIA_IC slow path (accessor / class receiver — may
                // frame-call a getter ⇒ refetch r13/r14 after). Copied from the
                // region GetProp arm, minus the method-inline prefix + TA refetch
                // (Tier C has neither). r13/r14 are pinned in the prologue
                // (has_prop ⇒ refetch_pinned). See `IcEntry` for the layout.
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                // The probe owns its own internal labels (`probe`/`next`/`hit`/`hop`);
                // only the two shared with the miss path survive here. `miss` went
                // with them -- it was reached solely by a `jmp` to the instruction
                // after it.
                let via_ic = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                // B114: accessor-way dispatch target (as the region arm),
                // SITE-GATED exactly as there.
                let site_emit = ic_emit
                    .get((ic_site - heap.ic_base_idx) as usize)
                    .copied()
                    .unwrap_or_default();
                let acc = site_emit.acc.then(|| ops.new_dynamic_label());
                emit_ic_probe(
                    &mut ops,
                    IcProbe::Get { dst },
                    obj,
                    off,
                    cont,
                    acc,
                    site_emit.direct_miss,
                );
                // ── B190a: quick `.length` prefix ── exactly the region arm's
                // (Str/Cons/dense-Array lengths are uncachable in the IC, so a
                // length read missed to the FULL property helper per read).
                if crate::codegen::quick_len_enabled()
                    && proto
                        .string_constants
                        .get(name as usize)
                        .is_some_and(|s| s == "length")
                {
                    let ql_miss = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, rax                    // obj_bits from the probe
                        ; mov rax, QWORD heap.quick_len as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => ql_miss
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => cont
                        ; => ql_miss
                        ; mov rax, [rbx + dreg(obj)]      // reload for the miss helper
                    );
                }
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits (rax survives the probe)
                    ; mov r8d, ic_site as i32             // site_idx
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov rax, QWORD heap.get_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD PROP_VIA_IC as i64
                    ; cmp rax, r10
                    ; je => via_ic
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => cont
                    // ── accessor / class receiver: the interpreter-IC slow helper
                    // resolves it (may frame-call a getter — user code).
                    ; => via_ic
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // caller window base
                    ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                    ; mov r9, QWORD (((name as u64) << 32) | obj as u64) as i64
                    ; mov rax, QWORD heap.get_prop_slow as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some(acc) = acc {
                    // ── accessor-WAY hit (B114): r9 = the matched way; the
                    // helper dispatches the getter directly (region arm, minus
                    // the TA refetch Tier C doesn't have).
                    let join = ops.new_dynamic_label();
                    dynasm!(ops
                        ; jmp => join
                        ; => acc
                        ; mov [rsp + 32], r9                  // 5th arg: way ptr
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, rbx                        // caller window base
                        ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                        ; mov r9, QWORD (((name as u64) << 32) | obj as u64) as i64
                        ; mov rax, QWORD heap.get_prop_acc as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                        ; => join
                    );
                }
                // The miss/slow helpers may have allocated (versions Vec) or
                // frame-called a getter (nested compile) — re-derive r13/r14.
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                dynasm!(ops ; => cont);
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::SetProp {
                obj,
                name,
                val,
                strict: _,
            } => {
                // 8-way inline cache, CALL-FREE on hit. The region arm verbatim,
                // minus the setter-inline prefix and the TA refetch. Unlike
                // GetProp the helper only ever fills OWN ways here (identity +
                // receiver version fully guard an own writable data slot: any
                // redefinition / freeze / delete / proto change bumps the
                // version), so the probe has no hop chain to walk.
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let cont = ops.new_dynamic_label();
                // B114: accessor-way dispatch target (as the region arm),
                // SITE-GATED exactly as there.
                let site_emit = ic_emit
                    .get((ic_site - heap.ic_base_idx) as usize)
                    .copied()
                    .unwrap_or_default();
                let acc = site_emit.acc.then(|| ops.new_dynamic_label());
                emit_ic_probe(
                    &mut ops,
                    IcProbe::Set { val },
                    obj,
                    off,
                    cont,
                    acc,
                    site_emit.direct_miss,
                );
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits
                    ; mov r8, [rbx + dreg(val)]           // val_bits
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov QWORD [rsp + 32], ic_site as i32 // 5th arg: site_idx (stack)
                    ; mov rax, QWORD heap.set_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD PROP_VIA_IC as i64
                    ; cmp rax, r10
                    ; jne => cont
                    // ── setter / class receiver: interpreter-IC slow helper
                    // (may frame-call a setter — user code).
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // caller window base
                    ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                    ; mov r9, QWORD (((name as u64) << 32) | ((obj as u64) << 16) | val as u64) as i64
                    ; mov rax, QWORD heap.set_prop_slow as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                if let Some(acc) = acc {
                    // ── accessor-WAY hit (B114): direct setter dispatch.
                    let join = ops.new_dynamic_label();
                    dynasm!(ops
                        ; jmp => join
                        ; => acc
                        ; mov [rsp + 32], r9                  // 5th arg: way ptr
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, rbx                        // caller window base
                        ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                        ; mov r9, QWORD (((name as u64) << 32) | ((obj as u64) << 16) | val as u64) as i64
                        ; mov rax, QWORD heap.set_prop_acc as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; => join
                    );
                }
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                dynasm!(ops ; => cont);
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::CallMethod {
                dst,
                obj,
                name,
                arg_base,
                argc,
            } => {
                // The intrinsic set (mem_can_compile gated) — the region path's
                // dedicated pure win64 helpers, minus its pin fast paths (Tier C
                // has no pins). Every helper here takes receiver + arg bits and
                // returns result bits or the deopt sentinel; none runs user code.
                // substring/slice does allocate its result and therefore
                // re-fetches the versions pointer explicitly below.
                let key = proto.string_constants[name as usize].as_str();
                let substring_arity_ok = argc == 2 || (argc == 1 && substring1_intrinsic_enabled());
                if key == "random" && argc == 0 && tierc_random_method_enabled() {
                    // Unlike arithmetic Math intrinsics, `Math.random()` must
                    // observe an own-property replacement. Route through the
                    // interpreter's live method IC and generic call protocol;
                    // a user function/native runs to completion, while an
                    // accessor/proxy/exotic receiver deopts before effects.
                    let packed_fip = ((func_id as u64) << 32) | ip as u64;
                    let packed_args =
                        ((name as u64) << 32) | ((obj as u64) << 16) | arg_base as u64;
                    if let Some(plan) = method_plan.get(&ip) {
                        // Exact own-data receiver/callee guards precede a
                        // closed typed schedule. Global writes stay buffered
                        // until every live tag/range/route/depth guard passes;
                        // the terminal stores cannot deopt or call a helper.
                        emit_inline_method_call(
                            &mut ops,
                            ip,
                            epilogue,
                            leaf_flag_off,
                            plan,
                            obj,
                            arg_base,
                            argc,
                            dst,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            refetch,
                            None,
                            None,
                        );
                    } else {
                        emit_tierc_general_method(
                            &mut ops,
                            ip,
                            bail,
                            epilogue,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            argc,
                            dst,
                            arg_base,
                            refetch,
                            method_own_slot_direct,
                        );
                    }
                } else if key == "toUpperCase" && argc == 0 && tierc_string_upper_enabled() {
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // primitive-string receiver bits
                        ; mov rax, QWORD heap.str_upper_case as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail                          // live prototype/realm mismatch
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // Case mapping allocates the result and can grow the heap's
                    // parallel versions table. It cannot run user code or grow
                    // the IC table, so only r13 needs re-deriving.
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, None);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else if tierc_coll_mutate_enabled()
                    && matches!((key, argc), ("set", 2) | ("clear", 0))
                {
                    let op = i32::from(key == "clear");
                    if argc == 2 {
                        dynasm!(ops
                            ; mov r8, [rbx + dreg(arg_base)]
                            ; mov r9, [rbx + dreg(arg_base + 1)]
                        );
                    } else {
                        dynasm!(ops
                            ; xor r8d, r8d
                            ; xor r9d, r9d
                        );
                    }
                    dynasm!(ops
                        ; mov QWORD [rsp + 32], op
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // Map receiver bits
                        ; mov rax, QWORD heap.coll_mutate as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail                          // own/proto/realm override
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else if substring_arity_ok && matches!(key, "substring" | "slice") {
                    // substring/slice: args read from the contiguous window;
                    // mode bit 1 tells the helper the end argument is absent.
                    let mode = (key == "slice") as i32 | (((argc == 1) as i32) << 1);
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                        ; lea r8, [rbx + dreg(arg_base)]      // &args[0..argc]
                        ; mov r9d, mode
                        ; mov rax, QWORD heap.str_substring as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // The result allocation can move the heap versions Vec.
                    // This helper cannot run user code or compile nested code,
                    // so only r13 (not r14) needs re-deriving.
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, None);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else if matches!(key, "get" | "has") {
                    // Map.get / Map.has / Set.has — the region arm verbatim:
                    // `has` retries as Set (op 2) before deopting; a wrong
                    // receiver kind deopts, so a same-named user method is
                    // unaffected (the interpreter runs it at this ip).
                    let opsel: i32 = if key == "get" { 0 } else { 1 };
                    let set_try = ops.new_dynamic_label();
                    let coll_done = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                        ; mov r8, [rbx + dreg(arg_base)]      // key bits
                        ; mov r9d, opsel
                        ; mov rax, QWORD heap.coll_lookup as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; jne => coll_done
                    );
                    if opsel == 1 {
                        dynasm!(ops
                            ; => set_try
                            ; mov rcx, rdi
                            ; mov rdx, [rbx + dreg(obj)]
                            ; mov r8, [rbx + dreg(arg_base)]
                            ; mov r9d, 2
                            ; mov rax, QWORD heap.coll_lookup as i64
                            ; call rax
                            ; mov r10, QWORD SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => bail
                        );
                    } else {
                        dynasm!(ops ; => set_try ; jmp => bail);
                    }
                    dynasm!(ops
                        ; => coll_done
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else if matches!(key, "charCodeAt" | "indexOf" | "push") && argc == 1 {
                    // charCodeAt / indexOf / push — one-arg dedicated helpers.
                    let helper = match key {
                        "indexOf" => heap.str_index_of,
                        "push" => heap.array_push,
                        _ => heap.char_code_at,
                    };
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                        ; mov r8, [rbx + dreg(arg_base)]      // arg0 bits
                        ; mov rax, QWORD helper as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else {
                    // Every remaining name/arity: the same live-IC route as
                    // `random` — a USER function/closure runs to completion
                    // through the method cross-call or generic helper; natives
                    // and accessor/exotic receivers deopt per call. Admission
                    // accepts these only under the closure-make latch: a hot
                    // application method (`getState`) is a user closure, and
                    // blacklisting the whole caller costs far more than a rare
                    // native-name bail.
                    let packed_fip = ((func_id as u64) << 32) | ip as u64;
                    let packed_args =
                        ((name as u64) << 32) | ((obj as u64) << 16) | arg_base as u64;
                    emit_tierc_general_method(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        heap.call_method_ic,
                        packed_fip,
                        packed_args,
                        argc,
                        dst,
                        arg_base,
                        refetch,
                        method_own_slot_direct,
                    );
                }
            }
            Instr::Call {
                dst,
                callee,
                arg_base,
                argc,
            } => {
                // General `f(args…)` (`this = undefined`) via the interpreter-IC
                // call helper. Packing: r9 = (callee<<16) | arg_base; argc on the
                // stack. The callee runs user code + allocates + can trigger a
                // nested compile ⇒ refetch r13/r14 after, when refetch_pinned (else
                // the next GetProp probe / leaf version guard reads a moved table).
                let packed_fip = ((func_id as u64) << 32) | ip as u64;
                let packed_args = ((callee as u64) << 16) | arg_base as u64;
                // ── B83 cross-call fast path ── a Call site whose live IC named
                // a plain user-function callee first tries the native→native
                // cross-call helper: it re-resolves the LIVE callee Value (read
                // from the register here, so a rebound global misses), and runs
                // a Tier-C-compiled callee directly over the contiguous window
                // above this frame — no `ic_call` probe, no `setup_call`, no
                // frame push, no nested `run_loop`. `SELF_CALL_DEOPT` (callee
                // not/never Tier-C, arrow, depth cap, stale global routes) falls
                // through to the unchanged `call_ic` helper — a pure prefix.
                // `CALL_THREW` bails so the interpreter unwinds (never re-runs).
                // Skipped when the site is leaf-inlined (strictly cheaper).
                let cross_site = cross_plan.get(&ip).copied();
                let cross = cross_site.is_some() && leaf_plan.get(&ip).is_none();
                let cross_done = ops.new_dynamic_label();
                if cross {
                    let site = cross_site.expect("cross site disappeared during emission");
                    // B189b: the fully-emitted lane first; every guard miss
                    // falls through to the unchanged helper block below (a
                    // pure prefix). Whole-fn callers run under a frame-free
                    // activation when cross-called themselves; the enter
                    // helper's root-stack duplication handles exactly that.
                    if do_cross3 {
                        for c3plan in site.cross3.iter() {
                            emit_cross3_call(
                                &mut ops,
                                c3plan,
                                callee,
                                arg_base,
                                dst,
                                proto.reg_count.max(1),
                                c3_off,
                                &heap,
                                bail,
                                cross_done,
                                refetch.is_some(),
                                None,
                            );
                        }
                    }
                    let same_proto2 = site.same_proto2;
                    let packed_cross: u64 = match same_proto2 {
                        Some(plan) => {
                            ((plan.fid as u64) << 32)
                                | ((proto.reg_count.max(1) as u64) << 16)
                                | u64::from(plan.callee_regs)
                        }
                        None => (argc as u64) | ((proto.reg_count.max(1) as u64) << 16),
                    };
                    let cross_helper = if same_proto2.is_some() {
                        heap.cross_call_same_proto2
                    } else {
                        heap.cross_call
                    };
                    let cross_fallback = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, rbx                        // caller window base
                        ; lea r8, [rbx + dreg(arg_base)]      // &args[0..argc]
                        ; mov r9, QWORD packed_cross as i64   // (caller_regs<<16)|argc
                        ; mov rax, [rbx + dreg(callee)]
                        ; mov [rsp + 32], rax                 // 5th arg: callee bits
                        ; mov rax, QWORD cross_helper as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => cross_fallback                // not eligible → call_ic
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail                          // threw → exit; unwind
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // The callee ran user code (alloc / nested compile possible):
                    // re-derive the pinned r13/r14, exactly as after call_ic.
                    if let Some((vb, icb)) = refetch {
                        emit_refetch_pinned(&mut ops, vb, Some(icb));
                    }
                    dynasm!(ops
                        ; jmp => cross_done
                        ; => cross_fallback
                    );
                }
                // Q4 leaf-call inlining: a monomorphic plain-leaf callee is inlined
                // with an identity guard; a guard miss / tight headroom falls through
                // to the SAME helper (a pure prefix). Tier C has no TA pins → no
                // ta_refetch.
                if let Some(lp) = leaf_plan.get(&ip) {
                    // Pair fusion skips caller bytecodes, so metered execution
                    // keeps the ordinary single-predicate path and its charges.
                    let span_pair_resume = if blocks.is_none() {
                        lp.span_code_unit_pred
                            .and_then(|p| p.pair)
                            .map(|p| labels[p.resume_ip as usize])
                    } else {
                        None
                    };
                    emit_inline_leaf_call(
                        &mut ops,
                        ip,
                        epilogue,
                        leaf_flag_off,
                        lp,
                        callee,
                        arg_base,
                        argc,
                        dst,
                        heap.math_unary,
                        heap.math_two,
                        heap.math_imul_guard,
                        // v2 body-op helpers (order matches the signature).
                        heap.get_index,
                        heap.char_code_at,
                        heap.strict_eq,
                        heap.truthy,
                        heap.call_ic,
                        packed_fip,
                        packed_args,
                        refetch,
                        None,
                        span_pair_resume,
                    );
                } else {
                    emit_region_call_ic(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        heap.call_ic,
                        packed_fip,
                        packed_args,
                        argc,
                        dst,
                        refetch,
                        None,
                    );
                }
                if cross {
                    dynasm!(ops ; => cross_done);
                }
            }
            Instr::RegExpMethod {
                dst,
                op,
                callee,
                this_v,
                arg_base,
                argc,
            } => {
                use crate::bytecode::RegExpMethod as R;

                let direct_on = match op {
                    R::Test | R::Exec => crate::vm::regexp_call_direct_enabled(),
                    R::MatchAll if argc >= 1 => crate::vm::string_regexp_call_direct_enabled(),
                    R::Replace if argc >= 2 => crate::vm::string_regexp_call_direct_enabled(),
                    _ => false,
                };
                let done = ops.new_dynamic_label();
                if direct_on {
                    let slow = ops.new_dynamic_label();
                    let direct_bail = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(this_v)]
                    );
                    match op {
                        R::Test | R::Exec => {
                            if argc == 0 {
                                dynasm!(ops ; mov r8, QWORD Value::UNDEFINED.bits() as i64);
                            } else {
                                dynasm!(ops ; mov r8, [rbx + dreg(arg_base)]);
                            }
                            dynasm!(ops
                                ; mov r9d, (op == R::Test) as i32
                                ; mov rax, QWORD heap.regexp_call_direct as i64
                            );
                        }
                        R::MatchAll | R::Replace => dynasm!(ops
                            ; lea r8, [rbx + dreg(arg_base)]
                            ; mov r9d, (op == R::Replace) as i32
                            ; mov rax, QWORD heap.string_regexp_call_direct as i64
                        ),
                    }
                    dynasm!(ops
                        ; mov r10, [rbx + dreg(callee)]
                        ; mov [rsp + 32], r10               // captured callee (5th arg)
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => slow
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => direct_bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    if let Some((vb, icb)) = refetch {
                        emit_refetch_pinned(&mut ops, vb, Some(icb));
                    }
                    emit_region_bail(&mut ops, ip, direct_bail, epilogue);
                    dynasm!(ops
                        ; jmp => done
                        ; => slow
                    );
                }

                let packed_fip = ((func_id as u64) << 32) | ip as u64;
                let packed_args =
                    ((this_v as u64) << 32) | ((callee as u64) << 16) | arg_base as u64;
                emit_region_call_ic(
                    &mut ops,
                    ip,
                    bail,
                    epilogue,
                    crate::vm::jit_call_with_this_ic as usize,
                    packed_fip,
                    packed_args,
                    argc,
                    dst,
                    refetch,
                    None,
                );
                dynasm!(ops ; => done);
            }
            Instr::CallWithThis {
                dst,
                callee,
                this_v,
                arg_base,
                argc,
            } => {
                // Exact captured-reference call. The helper reads both Value
                // operands from the rooted VM register window on every
                // execution; no name/shape-based target lookup is permitted.
                // Split builtin call: the captured-intrinsic lane first (a
                // bits-guarded direct helper; misses fall through) — see
                // `emit_captured_builtin_lane`. Tier C pins no arrays.
                let lane_done = captured_builtin_lane(proto, ip, callee, argc, &heap).map(
                    |(bits, helper, _)| {
                        emit_captured_builtin_lane(
                            &mut ops, callee, this_v, arg_base, dst, bits, helper, None,
                        )
                    },
                );
                let packed_fip = ((func_id as u64) << 32) | ip as u64;
                let packed_args =
                    ((this_v as u64) << 32) | ((callee as u64) << 16) | arg_base as u64;
                emit_region_call_ic(
                    &mut ops,
                    ip,
                    bail,
                    epilogue,
                    crate::vm::jit_call_with_this_ic as usize,
                    packed_fip,
                    packed_args,
                    argc,
                    dst,
                    refetch,
                    None,
                );
                if let Some(done) = lane_done {
                    dynasm!(ops ; => done);
                }
            }
            Instr::StrAppendInPlace { dst, a, b } => {
                // In-place `dst = a + b` (the linearity-proved accumulator
                // append). DEOPTS when the appended value needs real
                // ToPrimitive — the helper's purity gate runs BEFORE any
                // mutation, so the interpreter re-executes cleanly. Allocates
                // on the pure path ⇒ refetch r13/r14 when pinned.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // accumulator bits
                    ; mov r8, [rbx + dreg(b)]             // appended bits
                    ; mov rax, QWORD heap.str_append as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // needs ToPrimitive → interp
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::StrAppendIndex {
                dst, a, obj, key, ..
            } => {
                if str_append_cursor.is_some_and(|plan| plan.append_ip == ip) {
                    let begin = ops.new_dynamic_label();
                    let cursor_miss = ops.new_dynamic_label();
                    let done = ops.new_dynamic_label();
                    dynasm!(ops
                        // Once active, every field in the stack record is
                        // authoritative until the shared epilogue commits it.
                        ; cmp QWORD [rsp + cursor_active_off], 0
                        ; je => begin
                        // Builder identity: a surprising internal overwrite
                        // commits prior bytes, then replays this append.
                        ; mov rax, [rbx + dreg(a)]
                        ; cmp rax, [rsp + cursor_acc_bits_off]
                        ; jne => cursor_miss
                        // Immutable flat-ASCII source identity plus slot
                        // generation defeats heap-index reuse (ABA).
                        ; mov rax, [rbx + dreg(obj)]
                        ; cmp rax, [rsp + cursor_source_bits_off]
                        ; jne => cursor_miss
                        ; mov ecx, eax
                        ; mov r11, [rdi + crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32]
                        ; mov rdx, [rsp + cursor_source_version_off]
                        ; cmp DWORD [r11 + rcx * 4], edx
                        ; jne => cursor_miss
                        // The exact tagged-Int/in-range index proof the old
                        // helper repeated on every character.
                        ; mov rax, [rbx + dreg(key)]
                        ; mov rcx, rax
                        ; shr rcx, 48
                        ; cmp ecx, INT_TAG_HI as i32
                        ; jne => cursor_miss
                        ; test eax, eax
                        ; js => cursor_miss
                        ; mov ecx, eax
                        ; cmp rcx, [rsp + cursor_source_len_off]
                        ; jae => cursor_miss
                        ; mov r11, [rsp + cursor_source_ptr_off]
                        ; movzx r10d, BYTE [r11 + rcx]
                        // Never call Vec::push with deferred metadata: write
                        // one initialized byte only after the explicit spare-
                        // capacity guard, then advance the private cursor.
                        ; mov rax, [rsp + cursor_out_len_off]
                        ; cmp rax, [rsp + cursor_out_capacity_off]
                        ; jae => cursor_miss
                        ; mov r11, [rsp + cursor_out_ptr_off]
                        ; mov BYTE [r11 + rax], r10b
                        ; inc rax
                        ; mov [rsp + cursor_out_len_off], rax
                        ; mov rax, [rsp + cursor_acc_bits_off]
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => done
                        // First character: the helper performs the ordinary
                        // exact append, reserves, and publishes cursor fields.
                        ; => begin
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(a)]
                        ; mov r8, [rbx + dreg(obj)]
                        ; mov r9, [rbx + dreg(key)]
                        ; lea r10, [rsp + cursor_off]
                        ; mov [rsp + 32], r10
                        ; mov rax, QWORD crate::vm::jit_str_append_cursor_begin as usize as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // Begin may allocate the first builder. Preserve every
                    // existing Tier-C versions/IC pin obligation.
                    if let Some((vb, icb)) = refetch {
                        emit_refetch_pinned(&mut ops, vb, Some(icb));
                    }
                    dynasm!(ops
                        ; jmp => done
                        // Previous direct bytes are committed by epilogue;
                        // interpreter replay starts at this untouched append.
                        ; => cursor_miss
                        ; mov DWORD [rsi], ip as i32
                        ; jmp => epilogue
                        ; => done
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else {
                    // Pure ASCII prefix; the deopt sentinel means no mutation
                    // and re-executes the fused opcode's exact generic fallback.
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(a)]
                        ; mov r8, [rbx + dreg(obj)]
                        ; mov r9, [rbx + dreg(key)]
                        ; mov rax, QWORD heap.str_append_index as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // The first append may replace an interned seed with a
                    // freshly allocated mutable builder; preserve the usual
                    // post-allocation versions/IC pin invariant.
                    if let Some((vb, icb)) = refetch {
                        emit_refetch_pinned(&mut ops, vb, Some(icb));
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                }
            }
            Instr::AddRightPair {
                dst,
                a,
                b,
                c,
                in_place,
            } => {
                // Exact right-associated pair through the same helper as the
                // interpreter and region MEM path. It may allocate or run user
                // coercion code, so always refetch pinned state after a served
                // call; CALL_THREW is a committed unwind, never a redo.
                let pair_helper = if in_place {
                    crate::vm::jit_append_right_pair as usize
                } else {
                    crate::vm::jit_add_right_pair as usize
                };
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // outer left bits
                    ; mov r8, [rbx + dreg(b)]             // inner left bits
                    ; mov r9, [rbx + dreg(c)]             // inner right bits
                    ; mov rax, QWORD pair_helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // defensive; helper never redoes
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                          // pending_throw set
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Pad2Concat { dst, src, zero } => {
                // Mirror the MEM-region call-free tagged-Int hit. A miss is
                // still pristine and enters the shared exact helper; only the
                // miss path can allocate/run user code/throw and needs refetch.
                // ICSTATS deliberately uses the helper for exact counters.
                let slow = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                if !crate::vm::pad2_concat_stats_enabled() {
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(src)]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => slow
                        ; mov r10d, eax
                    );
                    if zero {
                        dynasm!(ops ; cmp r10d, 9 ; ja => slow);
                    } else {
                        dynasm!(ops
                            ; cmp r10d, 10
                            ; jb => slow
                            ; cmp r10d, 99
                            ; ja => slow
                        );
                    }
                    dynasm!(ops
                        ; add r10d, crate::heap::INTERN_PAD2_START as i32
                        ; mov rax, QWORD HEAP_TAG as i64
                        ; or rax, r10
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => done
                    );
                }
                dynasm!(ops
                    ; => slow
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(src)]          // RHS bits
                    ; mov r8d, zero as i32                // literal prefix selector
                    ; mov rax, QWORD crate::vm::jit_pad2_concat as usize as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // defensive; helper never redoes
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                          // pending_throw set
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
                dynasm!(ops ; => done);
            }
            Instr::Pad2Conditional { dst, src } => {
                // Whole pad2 conditional: direct canonical result for tagged
                // Int 0..99; pristine shared fallback otherwise. ICSTATS uses
                // the helper on hits so the mechanism census is exact.
                let slow = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                if !crate::vm::pad2_concat_stats_enabled() {
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(src)]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => slow
                        ; mov r10d, eax
                        ; cmp r10d, 99
                        ; ja => slow
                        ; add r10d, crate::heap::INTERN_PAD2_START as i32
                        ; mov rax, QWORD HEAP_TAG as i64
                        ; or rax, r10
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => done
                    );
                }
                dynasm!(ops
                    ; => slow
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(src)]
                    ; mov rax, QWORD crate::vm::jit_pad2_conditional as usize as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some((vb, icb)) = refetch {
                    emit_refetch_pinned(&mut ops, vb, Some(icb));
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
                dynasm!(ops ; => done);
            }
            Instr::StrConcatChain { dst, a, b } => {
                if crate::codegen::chain_fast_enabled() {
                    // Fused chain link via the single-dispatch fast sibling
                    // `jit_concat_chain_fast` (value-identical to
                    // `Vm::add_values_chain`, the interpreter's own entry);
                    // r9d carries the first-link capacity hint (0 = none).
                    // `result == old acc bits && old acc heap-tagged` proves
                    // the in-place arm ran (no alloc, no user code) and
                    // licenses skipping the r13/r14 refetch — the heap-tag
                    // test is load-bearing: a numeric accumulator can get its
                    // own bits back from the generic tail AFTER user coercion
                    // code ran. A throw returns CALL_THREW (pending_throw
                    // materialized) → bail = UNWIND, never a redo;
                    // SELF_CALL_DEOPT is never returned, the check is kept
                    // for uniformity with the siblings.
                    let hint = super::region_mem::chain_capacity_hint(
                        &proto.code,
                        ip,
                        a,
                        proto.code.len() - 1,
                    );
                    let next_leaf = if crate::heap::concat_suffix_memo_enabled() {
                        super::region_mem::chain_next_leaf(&proto.code, ip, a)
                    } else {
                        None
                    };
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(a)]            // acc bits
                        ; mov r8, [rbx + dreg(b)]             // leaf bits
                        ; mov r9d, hint as i32                // capacity hint
                    );
                    if let Some(next_b) = next_leaf {
                        let helper_ready = ops.new_dynamic_label();
                        dynasm!(ops
                            ; mov rax, QWORD crate::vm::jit_concat_chain_fast as usize as i64
                            ; mov r10, [rbx + dreg(next_b)]
                            ; shr r10, 48
                            ; cmp r10d, INT_TAG_HI as i32
                            ; jne => helper_ready
                            ; mov rax, QWORD crate::vm::jit_concat_chain_suffix_fast as usize as i64
                            ; => helper_ready
                        );
                    } else {
                        dynasm!(ops
                            ; mov rax, QWORD crate::vm::jit_concat_chain_fast as usize as i64
                        );
                    }
                    dynasm!(ops
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail                          // threw → unwind, NOT redo
                        ; mov r10, [rbx + dreg(a)]            // pre-call acc bits (dst not yet stored)
                        ; mov [rbx + dreg(dst)], rax
                    );
                    if let Some((vb, icb)) = refetch {
                        let refetch_lbl = ops.new_dynamic_label();
                        let skip = ops.new_dynamic_label();
                        dynasm!(ops
                            ; cmp rax, r10
                            ; jne => refetch_lbl
                            ; shr r10, 48
                            ; cmp r10d, TAG_HEAP_HI as i32
                            ; je => skip                      // in-place arm: no alloc, no user code
                            ; => refetch_lbl
                        );
                        emit_refetch_pinned(&mut ops, vb, Some(icb));
                        dynasm!(ops ; => skip);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else {
                    // W11 (B124) fused chain link (`jit_concat_chain` →
                    // `Vm::add_values_chain`, the interpreter's own entry).
                    // Allocates ⇒ refetch r13/r14 when pinned. CAN run user code
                    // (object RHS ToPrimitive via the `add_values` fallback), so
                    // a throw returns CALL_THREW (pending_throw materialized) →
                    // bail = UNWIND, never a redo that would re-run the side
                    // effects. SELF_CALL_DEOPT is never returned by this helper;
                    // the check is kept for uniformity with its siblings.
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(a)]            // acc bits
                        ; mov r8, [rbx + dreg(b)]             // leaf bits
                        ; mov rax, QWORD crate::vm::jit_concat_chain as usize as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail                          // threw → unwind, NOT redo
                        ; mov [rbx + dreg(dst)], rax
                    );
                    if let Some((vb, icb)) = refetch {
                        emit_refetch_pinned(&mut ops, vb, Some(icb));
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                }
            }
            Instr::Return { src } => {
                if protected_returns[ip] {
                    // A return completion must traverse every still-active
                    // for-of/user finally. Resume on the untouched Return so
                    // `route_through_finally` deposits and propagates it.
                    dynasm!(ops
                        ; mov DWORD [rsi], ip as i32
                        ; jmp => epilogue
                    );
                } else {
                    // Whole-function return: NO_BAIL + result Value (UNLIKE the region,
                    // which records the ip and lets the interpreter perform the return).
                    dynasm!(ops
                        ; mov DWORD [rsi], NO_BAIL as i32
                        ; mov rax, [rbx + dreg(src)]
                        ; jmp => epilogue
                    );
                }
            }
            Instr::ReturnUndefined => {
                if protected_returns[ip] {
                    dynasm!(ops
                        ; mov DWORD [rsi], ip as i32
                        ; jmp => epilogue
                    );
                } else {
                    dynasm!(ops
                        ; mov DWORD [rsi], NO_BAIL as i32
                        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                        ; jmp => epilogue
                    );
                }
            }
            // Proper-tail-call prefix to the Call+Return the compiler emits
            // right after it. Emitting nothing would be value-sound, but it
            // loses the interpreter's constant-stack frame reuse: past the
            // cross-call depth cap every tail hop's Call bails at ITS ip, the
            // resumed interpreter pushes a real frame per hop, and a strict
            // tail chain that completes today dies at MAX_FRAMES (measured at
            // exactly the 100k boundary). So guard the DEPTH: at the cap,
            // bail at THIS ip — the interpreter executes the TailCall itself
            // and frame reuse resumes (streak-bounded, the same catchable
            // RangeError contract as before admission). Below the cap the
            // guard is one load + one not-taken branch and the following
            // Call runs native.
            Instr::TailCall { .. } => {
                let depth_off = crate::vm::host_api::JIT_CALL_DEPTH_OFFSET as i32;
                dynasm!(ops
                    ; mov eax, [rdi + depth_off]
                    ; cmp eax, crate::vm::JIT_REGION_CALL_MAX as i32
                    ; jae => bail
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            _ => return None, // mem_can_compile already filtered; defensive
        }
    }

    // Falling off the end behaves like ReturnUndefined.
    dynasm!(ops
        ; => labels[n]
        ; mov DWORD [rsi], NO_BAIL as i32
        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
        ; jmp => epilogue
    );

    // ── metering exits ── out of line, so the hot path is `sub` plus a
    // not-taken `jle`. Resuming at this ip is an ordinary bail.
    for (stub, ip) in meter_stubs {
        dynasm!(ops
            ; => stub
            ; mov DWORD [rsi], ip as i32
            ; xor rax, rax
            ; jmp => epilogue
        );
    }

    // ── epilogue ── restore and return; rax = result (or garbage on bail), [rsi]
    // = NO_BAIL or the resume ip. Mirrors `compile_region_mem`'s 6-pop epilogue.
    dynasm!(ops ; => epilogue);
    if str_append_cursor.is_some() {
        dynasm!(ops
            // Preserve a clean whole-function result across the commit call;
            // on a bail rax is ignored, but the same sequence covers both.
            ; mov rcx, rdi
            ; lea rdx, [rsp + cursor_off]
            ; mov r8, rax
            ; mov r11, QWORD crate::vm::jit_str_append_cursor_commit as usize as i64
            ; call r11
        );
    }
    dynasm!(ops
        ; add rsp, frame
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
    );

    // The IC-site cursor must have consumed exactly the sites `Jit::compile`
    // reserved (one per GetProp/SetProp). A mismatch ⇒ a GetProp's `[r14+off]`
    // probe reads past the reserved table (OOB / cross-site corruption).
    debug_assert_eq!(
        (ic_site - heap.ic_base_idx) as usize,
        proto
            .code
            .iter()
            .filter(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }))
            .count(),
        "Tier C ic_site cursor desynced from reserved sites"
    );

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn {
        _buf: buf,
        entry: entry_ptr,
        self_binding: None,
    })
}
