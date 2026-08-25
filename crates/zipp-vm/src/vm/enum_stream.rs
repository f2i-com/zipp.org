//! Guarded fast-forwarding for invariant enumeration reductions.
//!
//! These recognizers sit on the interpreter's `ForInKeys` / `ObjectKeys`
//! instructions because the enclosing nested loops are deliberately not normal
//! OSR regions.  They accept only exact compiler bytecode shapes and ordinary
//! side-effect-free objects.  A failed runtime guard changes no JS state and the
//! original instruction executes unchanged.

use super::*;

#[derive(Clone, Copy)]
struct ForInSumPlan {
    source_global: u32,
    sum_global: u32,
    key_global: u32,
    i_global: u32,
    limit_global: u32,
    exit: usize,
}

#[derive(Clone, Copy)]
struct ForInCountPlan {
    source_global: u32,
    count_global: u32,
    key_global: u32,
    i_global: u32,
    limit_global: u32,
    exit: usize,
}

#[derive(Clone, Copy)]
struct SparseForInFoldPlan {
    source_global: u32,
    count_global: u32,
    fold_global: u32,
    key_global: u32,
    modulus: i32,
    exit: usize,
}

#[derive(Clone, Copy)]
struct ObjectKeysLenPlan {
    source_global: u32,
    sum_global: u32,
    i_global: u32,
    limit_global: u32,
    exit: usize,
}

#[derive(Clone, Copy)]
struct InPeriodicProbe {
    modulus: i32,
    scale: i32,
    offset: i32,
    counter_global: u32,
}

struct InProbePlan {
    source_global: u32,
    i_global: u32,
    limit_global: u32,
    probes: Vec<InPeriodicProbe>,
    exit: usize,
}

#[derive(Clone, Copy)]
enum ArrayCopyLenKind {
    Slice { start_reg: u16, end_reg: u16 },
    Concat { arg_reg: u16 },
}

#[derive(Clone, Copy)]
struct ArrayCopyLenPlan {
    kind: ArrayCopyLenKind,
    source_global: u32,
    sum_global: u32,
    i_global: u32,
    limit_global: u32,
    exit: usize,
}

#[inline]
fn enum_loop_reduce_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_ENUM_LOOP_REDUCE").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

#[inline]
fn enum_count_reduce_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_ENUM_COUNT_REDUCE").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

#[inline]
fn sparse_forin_fold_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_SPARSE_FORIN_FOLD").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

#[inline]
fn in_probe_reduce_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_IN_PROBE_REDUCE").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

#[inline]
fn array_copy_len_reduce_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_ARRAY_COPY_LEN_REDUCE").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

fn distinct(globals: &[u32]) -> bool {
    !globals
        .iter()
        .enumerate()
        .any(|(i, global)| globals[..i].contains(global))
}

/// Exact compiler shape for
///
/// `for (; i < limit; i++) for (key in source) count++;`
///
/// The recognizer intentionally includes both loop skeletons, the liveness
/// check and every global route. Any extra expression, coercion, mutation,
/// break/continue or observable store changes the bytecode and fails closed.
fn recognize_forin_count(proto: &crate::bytecode::FuncProto, ip: usize) -> Option<ForInCountPlan> {
    let start = ip.checked_sub(4)?;
    let end = start.checked_add(20)?;
    if end >= proto.code.len() || !matches!(proto.code[ip], Instr::ForInKeys { .. }) {
        return None;
    }
    let c = &proto.code;
    let (i_head, i_global) = match c[start] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (limit, limit_global) = match c[start + 1] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    if !matches!(c[start + 2], Instr::JumpIfNotLt { a, b, target }
        if a == i_head && b == limit && target as usize == start + 21)
    {
        return None;
    }
    let (source, source_global) = match c[start + 3] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let snapshot = match c[start + 4] {
        Instr::ForInKeys { dst, obj } if obj == source => dst,
        _ => return None,
    };
    let len = match c[start + 5] {
        Instr::LenOf { dst, obj } if obj == snapshot => dst,
        _ => return None,
    };
    let index = match c[start + 6] {
        Instr::LoadInt { dst, val } if val as usize == crate::bytecode::FORIN_SNAPSHOT_PREFIX => {
            dst
        }
        _ => return None,
    };
    if !matches!(c[start + 7], Instr::JumpIfNotLt { a, b, target }
        if a == index && b == len && target as usize == start + 17)
    {
        return None;
    }
    let key = match c[start + 8] {
        Instr::GetIndex { dst, obj, key } if obj == snapshot && key == index => dst,
        _ => return None,
    };
    let live = match c[start + 9] {
        Instr::ForInLive { dst, obj, key: k } if obj == snapshot && k == key => dst,
        _ => return None,
    };
    if !matches!(c[start + 10], Instr::JumpIfFalse { cond, target }
        if cond == live && target as usize == start + 15)
    {
        return None;
    }
    let key_global = match c[start + 11] {
        Instr::StoreGlobal { idx, src }
        | Instr::StoreGlobalStrict { idx, src }
        | Instr::StoreGlobalResolved { idx, src }
            if src == key =>
        {
            idx
        }
        _ => return None,
    };
    let (count, count_global) = match c[start + 12] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    if !matches!(c[start + 13], Instr::AddInt { dst, a, imm: 1, upd: true }
        if dst == count && a == count)
        || !matches!(c[start + 14], Instr::StoreGlobalResolved { idx, src }
            if idx == count_global && src == count)
        || !matches!(c[start + 15], Instr::AddInt { dst, a, imm: 1, upd: false }
            if dst == index && a == index)
        || !matches!(c[start + 16], Instr::Jump { target } if target as usize == start + 7)
    {
        return None;
    }
    let i_tail = match c[start + 17] {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    if !matches!(c[start + 18], Instr::AddInt { dst, a, imm: 1, upd: true }
        if dst == i_tail && a == i_tail)
        || !matches!(c[start + 19], Instr::StoreGlobalResolved { idx, src }
            if idx == i_global && src == i_tail)
        || !matches!(c[start + 20], Instr::Jump { target } if target as usize == start)
    {
        return None;
    }
    let globals = [
        source_global,
        count_global,
        key_global,
        i_global,
        limit_global,
    ];
    distinct(&globals).then_some(ForInCountPlan {
        source_global,
        count_global,
        key_global,
        i_global,
        limit_global,
        exit: start + 21,
    })
}

/// Exact compiler shape for a single sparse-array key/count/fold walk:
///
/// `for (key in source) { count++; fold = (fold + (+key) + source[key]) % M; }`
///
/// The full snapshot loop, liveness check, numeric coercion, source read and
/// modulo tail are part of the proof. Any mutation, call, alternate arithmetic
/// or extra statement changes the bytecode and declines before JS state changes.
fn recognize_sparse_forin_fold(
    proto: &crate::bytecode::FuncProto,
    ip: usize,
) -> Option<SparseForInFoldPlan> {
    let c = &proto.code;
    let start = ip.checked_sub(1)?;
    let (source, source_global) = match *c.get(start)? {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let snapshot = match *c.get(ip)? {
        Instr::ForInKeys { dst, obj } if obj == source => dst,
        _ => return None,
    };
    let len = match *c.get(ip + 1)? {
        Instr::LenOf { dst, obj } if obj == snapshot => dst,
        _ => return None,
    };
    let index = match *c.get(ip + 2)? {
        Instr::LoadInt { dst, val } if val as usize == crate::bytecode::FORIN_SNAPSHOT_PREFIX => {
            dst
        }
        _ => return None,
    };
    if !matches!(*c.get(ip + 3)?, Instr::JumpIfNotLt { a, b, target }
        if a == index && b == len && target as usize == ip + 24)
    {
        return None;
    }
    let key = match *c.get(ip + 4)? {
        Instr::GetIndex { dst, obj, key } if obj == snapshot && key == index => dst,
        _ => return None,
    };
    let live = match *c.get(ip + 5)? {
        Instr::ForInLive { dst, obj, key: k } if obj == snapshot && k == key => dst,
        _ => return None,
    };
    if !matches!(*c.get(ip + 6)?, Instr::JumpIfFalse { cond, target }
        if cond == live && target as usize == ip + 22)
    {
        return None;
    }
    let key_global = match *c.get(ip + 7)? {
        Instr::StoreGlobal { idx, src }
        | Instr::StoreGlobalStrict { idx, src }
        | Instr::StoreGlobalResolved { idx, src }
            if src == key =>
        {
            idx
        }
        _ => return None,
    };
    let (count, count_global) = match *c.get(ip + 8)? {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    if !matches!(*c.get(ip + 9)?, Instr::AddInt { dst, a, imm: 1, upd: true }
        if dst == count && a == count)
        || !matches!(*c.get(ip + 10)?,
            Instr::StoreGlobal { idx, src }
            | Instr::StoreGlobalStrict { idx, src }
            | Instr::StoreGlobalResolved { idx, src }
                if idx == count_global && src == count)
    {
        return None;
    }
    let (fold, fold_global) = match *c.get(ip + 11)? {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let key_again = match *c.get(ip + 12)? {
        Instr::LoadGlobal { dst, idx } if idx == key_global => dst,
        _ => return None,
    };
    let numeric_key = match *c.get(ip + 13)? {
        Instr::ToNum { dst, a } if a == key_again => dst,
        _ => return None,
    };
    let fold_key = match *c.get(ip + 14)? {
        Instr::Add { dst, a, b } if a == fold && b == numeric_key => dst,
        _ => return None,
    };
    let source_again = match *c.get(ip + 15)? {
        Instr::LoadGlobal { dst, idx } if idx == source_global => dst,
        _ => return None,
    };
    let key_third = match *c.get(ip + 16)? {
        Instr::LoadGlobal { dst, idx } if idx == key_global => dst,
        _ => return None,
    };
    let value = match *c.get(ip + 17)? {
        Instr::GetIndex { dst, obj, key } if obj == source_again && key == key_third => dst,
        _ => return None,
    };
    let total = match *c.get(ip + 18)? {
        Instr::Add { dst, a, b } if a == fold_key && b == value => dst,
        _ => return None,
    };
    let (mod_reg, modulus) = match *c.get(ip + 19)? {
        Instr::LoadInt { dst, val } if val != 0 => (dst, val),
        _ => return None,
    };
    let reduced = match *c.get(ip + 20)? {
        Instr::Mod { dst, a, b } if a == total && b == mod_reg => dst,
        _ => return None,
    };
    if !matches!(*c.get(ip + 21)?,
        Instr::StoreGlobal { idx, src }
        | Instr::StoreGlobalStrict { idx, src }
        | Instr::StoreGlobalResolved { idx, src }
            if idx == fold_global && src == reduced)
        || !matches!(*c.get(ip + 22)?, Instr::AddInt { dst, a, imm: 1, upd: false }
            if dst == index && a == index)
        || !matches!(*c.get(ip + 23)?, Instr::Jump { target }
            if target as usize == ip + 3)
        || !distinct(&[source_global, count_global, fold_global, key_global])
    {
        return None;
    }
    Some(SparseForInFoldPlan {
        source_global,
        count_global,
        fold_global,
        key_global,
        modulus,
        exit: ip + 24,
    })
}

/// Parse a pure periodic integer key based on the enclosing induction global:
/// `(i % M) * S + O` (the multiply/offset are optional), or `O + (i % M)`.
/// Returns `(next_ip, key_reg, modulus, scale, offset)`.
fn periodic_in_key(
    code: &[Instr],
    ip: usize,
    i_global: u32,
) -> Option<(usize, u16, i32, i32, i32)> {
    // Prefix form: O + (i % M).
    if let Some((offset_reg, offset)) = code.get(ip).and_then(|ins| match *ins {
        Instr::LoadInt { dst, val } => Some((dst, val)),
        _ => None,
    }) {
        let i_reg = match *code.get(ip + 1)? {
            Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
            _ => return None,
        };
        let (mod_reg, modulus) = match *code.get(ip + 2)? {
            Instr::LoadInt { dst, val } if val > 0 => (dst, val),
            _ => return None,
        };
        let rem = match *code.get(ip + 3)? {
            Instr::Mod { dst, a, b } if a == i_reg && b == mod_reg => dst,
            _ => return None,
        };
        let key = match *code.get(ip + 4)? {
            Instr::Add { dst, a, b }
                if (a == offset_reg && b == rem) || (a == rem && b == offset_reg) =>
            {
                dst
            }
            _ => return None,
        };
        return Some((ip + 5, key, modulus, 1, offset));
    }

    // Suffix form: (i % M) [* S] [+ O].
    let i_reg = match *code.get(ip)? {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    let (mod_reg, modulus) = match *code.get(ip + 1)? {
        Instr::LoadInt { dst, val } if val > 0 => (dst, val),
        _ => return None,
    };
    let mut key = match *code.get(ip + 2)? {
        Instr::Mod { dst, a, b } if a == i_reg && b == mod_reg => dst,
        _ => return None,
    };
    let mut next = ip + 3;
    let mut scale = 1i32;
    if let (
        Some(Instr::LoadInt {
            dst: scale_reg,
            val,
        }),
        Some(Instr::Mul { dst, a, b }),
    ) = (code.get(next), code.get(next + 1))
    {
        if (*a == key && *b == *scale_reg) || (*a == *scale_reg && *b == key) {
            key = *dst;
            scale = *val;
            next += 2;
        }
    }
    let mut offset = 0i32;
    if let Some(Instr::AddInt {
        dst,
        a,
        imm,
        upd: false,
    }) = code.get(next)
    {
        if *a == key {
            key = *dst;
            offset = *imm;
            next += 1;
        }
    }
    Some((next, key, modulus, scale, offset))
}

/// Recognize an arbitrary non-empty sequence of
/// `if (periodicIntegerKey in source) counter++` probes inside one compiler
/// induction loop. The first `HasProp` is the dispatch hook; accepting only it
/// prevents a late probe from fast-forwarding a partially executed iteration.
fn recognize_in_probe_reduce(
    proto: &crate::bytecode::FuncProto,
    first_has_ip: usize,
) -> Option<InProbePlan> {
    let c = &proto.code;
    // The compiler header is always two global loads + fused `<` branch. Scan
    // only the bounded pure-key prefix behind this first HasProp.
    let low = first_has_ip.saturating_sub(14);
    for start in low..first_has_ip {
        let (i_head, i_global) = match *c.get(start)? {
            Instr::LoadGlobal { dst, idx } => (dst, idx),
            _ => continue,
        };
        let (limit, limit_global) = match *c.get(start + 1)? {
            Instr::LoadGlobal { dst, idx } => (dst, idx),
            _ => continue,
        };
        let exit = match *c.get(start + 2)? {
            Instr::JumpIfNotLt { a, b, target } if a == i_head && b == limit => target as usize,
            _ => continue,
        };
        let mut pos = start + 3;
        let mut probes = Vec::new();
        let mut source_global = None;
        let mut first_seen = None;
        loop {
            // Outer-loop tail.
            if let (
                Some(Instr::LoadGlobal { dst: tail, idx }),
                Some(Instr::AddInt {
                    dst,
                    a,
                    imm: 1,
                    upd: true,
                }),
                Some(Instr::StoreGlobalResolved {
                    idx: store_idx,
                    src,
                }),
                Some(Instr::Jump { target }),
            ) = (c.get(pos), c.get(pos + 1), c.get(pos + 2), c.get(pos + 3))
            {
                if *idx == i_global
                    && *dst == *tail
                    && *a == *tail
                    && *store_idx == i_global
                    && *src == *tail
                    && *target as usize == start
                    && exit == pos + 4
                    && first_seen == Some(first_has_ip)
                    && !probes.is_empty()
                    && probes.len() <= 8
                {
                    let source_global = source_global?;
                    let mut globals = vec![source_global, i_global, limit_global];
                    globals.extend(probes.iter().map(|p: &InPeriodicProbe| p.counter_global));
                    if distinct(&globals) {
                        return Some(InProbePlan {
                            source_global,
                            i_global,
                            limit_global,
                            probes,
                            exit,
                        });
                    }
                }
                break;
            }

            let (after_key, key, modulus, scale, offset) = periodic_in_key(c, pos, i_global)?;
            let (obj, source) = match *c.get(after_key)? {
                Instr::LoadGlobal { dst, idx } => (dst, idx),
                _ => return None,
            };
            match source_global {
                Some(g) if g != source => return None,
                None => source_global = Some(source),
                _ => {}
            }
            let cond = match *c.get(after_key + 1)? {
                Instr::HasProp {
                    dst,
                    key: k,
                    obj: o,
                    brand: false,
                } if k == key && o == obj => dst,
                _ => return None,
            };
            let has_ip = after_key + 1;
            if first_seen.is_none() {
                first_seen = Some(has_ip);
            }
            if !matches!(*c.get(has_ip + 1)?, Instr::JumpIfFalse { cond: x, target }
                if x == cond && target as usize == has_ip + 5)
            {
                return None;
            }
            let (counter, counter_global) = match *c.get(has_ip + 2)? {
                Instr::LoadGlobal { dst, idx } => (dst, idx),
                _ => return None,
            };
            if !matches!(*c.get(has_ip + 3)?, Instr::AddInt { dst, a, imm: 1, upd: true }
                if dst == counter && a == counter)
                || !matches!(*c.get(has_ip + 4)?, Instr::StoreGlobalResolved { idx, src }
                    if idx == counter_global && src == counter)
            {
                return None;
            }
            probes.push(InPeriodicProbe {
                modulus,
                scale,
                offset,
                counter_global,
            });
            if probes.len() > 8 {
                return None;
            }
            pos = has_ip + 5;
        }
    }
    None
}

/// Exact compiler shapes for the two allocation-only reductions used by
/// `Array.prototype.slice(...).length` and `concat([literal...]).length`:
///
/// `for (; i < limit; i++) sum = (sum + source.<copy>(...).length) | 0;`
///
/// For concat the sole argument must be an array literal whose elements are
/// integer literals evaluated inside the loop.  Including the complete loop
/// skeleton is what makes it safe to skip every later argument evaluation and
/// result allocation after the first CallMethod reaches the interpreter.
fn recognize_array_copy_len(
    proto: &crate::bytecode::FuncProto,
    call_ip: usize,
) -> Option<ArrayCopyLenPlan> {
    use crate::bytecode::BitwiseOp;
    let c = &proto.code;
    let (call_dst, call_obj, name, call_arg_base, call_argc) = match *c.get(call_ip)? {
        Instr::CallMethod {
            dst,
            obj,
            name,
            arg_base,
            argc,
        } => (dst, obj, name, arg_base, argc),
        _ => return None,
    };
    let method = proto.string_constants.get(name as usize)?.as_str();

    let (start, kind, after_call) = match method {
        "slice" if call_argc == 2 => {
            let start = call_ip.checked_sub(7)?;
            let start_reg = match *c.get(start + 5)? {
                Instr::LoadInt { dst, .. } if dst == call_arg_base => dst,
                _ => return None,
            };
            let end_reg = match *c.get(start + 6)? {
                Instr::LoadInt { dst, .. } if dst == call_arg_base + 1 => dst,
                _ => return None,
            };
            (
                start,
                ArrayCopyLenKind::Slice { start_reg, end_reg },
                start + 8,
            )
        }
        "concat" if call_argc == 1 => {
            // The immediately preceding NewArray consumes a contiguous run of
            // LoadInt element expressions.  Nothing else is accepted: a getter,
            // call, spread, computed expression or hole changes this bytecode.
            let new_ip = call_ip.checked_sub(1)?;
            let (arg_reg, elem_base, elem_count) = match *c.get(new_ip)? {
                Instr::NewArray {
                    dst,
                    arg_base,
                    argc,
                } if dst == call_arg_base => (dst, arg_base, argc as usize),
                _ => return None,
            };
            if elem_count > 8 {
                return None;
            }
            let start = new_ip.checked_sub(5 + elem_count)?;
            for n in 0..elem_count {
                if !matches!(*c.get(start + 5 + n)?, Instr::LoadInt { dst, .. }
                    if dst == elem_base + n as u16)
                {
                    return None;
                }
            }
            (start, ArrayCopyLenKind::Concat { arg_reg }, call_ip + 1)
        }
        _ => return None,
    };

    let (i_head, i_global) = match *c.get(start)? {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (limit, limit_global) = match *c.get(start + 1)? {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (sum, sum_global) = match *c.get(start + 3)? {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (source, source_global) = match *c.get(start + 4)? {
        Instr::LoadGlobal { dst, idx } if dst == call_obj => (dst, idx),
        _ => return None,
    };
    let len = match *c.get(after_call)? {
        Instr::GetProp { dst, obj, name }
            if obj == call_dst
                && proto
                    .string_constants
                    .get(name as usize)
                    .is_some_and(|s| s == "length") =>
        {
            dst
        }
        _ => return None,
    };
    let added = match *c.get(after_call + 1)? {
        Instr::Add { dst, a, b } if a == sum && b == len => dst,
        _ => return None,
    };
    let zero = match *c.get(after_call + 2)? {
        Instr::LoadInt { dst, val: 0 } => dst,
        _ => return None,
    };
    let reduced = match *c.get(after_call + 3)? {
        Instr::Bitwise {
            dst,
            a,
            b,
            op: BitwiseOp::Or,
        } if a == added && b == zero => dst,
        _ => return None,
    };
    if !matches!(*c.get(after_call + 4)?,
        Instr::StoreGlobal { idx, src }
        | Instr::StoreGlobalStrict { idx, src }
        | Instr::StoreGlobalResolved { idx, src }
            if idx == sum_global && src == reduced)
    {
        return None;
    }
    let i_tail = match *c.get(after_call + 5)? {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    if !matches!(*c.get(after_call + 6)?, Instr::AddInt { dst, a, imm: 1, upd: true }
        if dst == i_tail && a == i_tail)
        || !matches!(*c.get(after_call + 7)?, Instr::StoreGlobalResolved { idx, src }
            if idx == i_global && src == i_tail)
        || !matches!(*c.get(after_call + 8)?, Instr::Jump { target }
            if target as usize == start)
    {
        return None;
    }
    let exit = after_call + 9;
    if !matches!(*c.get(start + 2)?, Instr::JumpIfNotLt { a, b, target }
        if a == i_head && b == limit && target as usize == exit)
        || !distinct(&[source_global, sum_global, i_global, limit_global])
    {
        return None;
    }
    let _ = source;
    Some(ArrayCopyLenPlan {
        kind,
        source_global,
        sum_global,
        i_global,
        limit_global,
        exit,
    })
}

fn recognize_forin_sum(proto: &crate::bytecode::FuncProto, ip: usize) -> Option<ForInSumPlan> {
    use crate::bytecode::BitwiseOp;
    let start = ip.checked_sub(4)?;
    let end = start.checked_add(25)?;
    if end >= proto.code.len() || !matches!(proto.code[ip], Instr::ForInKeys { .. }) {
        return None;
    }
    let c = &proto.code;
    let (i_head, i_global) = match c[start] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (limit, limit_global) = match c[start + 1] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    if !matches!(c[start + 2], Instr::JumpIfNotLt { a, b, target }
        if a == i_head && b == limit && target as usize == start + 26)
    {
        return None;
    }
    let (source, source_global) = match c[start + 3] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let snapshot = match c[start + 4] {
        Instr::ForInKeys { dst, obj } if obj == source => dst,
        _ => return None,
    };
    let len = match c[start + 5] {
        Instr::LenOf { dst, obj } if obj == snapshot => dst,
        _ => return None,
    };
    let index = match c[start + 6] {
        Instr::LoadInt { dst, val } if val as usize == crate::bytecode::FORIN_SNAPSHOT_PREFIX => {
            dst
        }
        _ => return None,
    };
    if !matches!(c[start + 7], Instr::JumpIfNotLt { a, b, target }
        if a == index && b == len && target as usize == start + 22)
    {
        return None;
    }
    let key = match c[start + 8] {
        Instr::GetIndex { dst, obj, key } if obj == snapshot && key == index => dst,
        _ => return None,
    };
    let live = match c[start + 9] {
        Instr::ForInLive { dst, obj, key: k } if obj == snapshot && k == key => dst,
        _ => return None,
    };
    if !matches!(c[start + 10], Instr::JumpIfFalse { cond, target }
        if cond == live && target as usize == start + 20)
    {
        return None;
    }
    let key_global = match c[start + 11] {
        Instr::StoreGlobal { idx, src }
        | Instr::StoreGlobalStrict { idx, src }
        | Instr::StoreGlobalResolved { idx, src }
            if src == key =>
        {
            idx
        }
        _ => return None,
    };
    let (sum, sum_global) = match c[start + 12] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let source_again = match c[start + 13] {
        Instr::LoadGlobal { dst, idx } if idx == source_global => dst,
        _ => return None,
    };
    let key_again = match c[start + 14] {
        Instr::LoadGlobal { dst, idx } if idx == key_global => dst,
        _ => return None,
    };
    let value = match c[start + 15] {
        Instr::GetIndex { dst, obj, key } if obj == source_again && key == key_again => dst,
        _ => return None,
    };
    let added = match c[start + 16] {
        Instr::Add { dst, a, b } if a == sum && b == value => dst,
        _ => return None,
    };
    let zero = match c[start + 17] {
        Instr::LoadInt { dst, val: 0 } => dst,
        _ => return None,
    };
    let reduced = match c[start + 18] {
        Instr::Bitwise {
            dst,
            a,
            b,
            op: BitwiseOp::Or,
        } if a == added && b == zero => dst,
        _ => return None,
    };
    if !matches!(c[start + 19], Instr::StoreGlobal { idx, src }
        | Instr::StoreGlobalStrict { idx, src }
        | Instr::StoreGlobalResolved { idx, src }
            if idx == sum_global && src == reduced)
        || !matches!(c[start + 20], Instr::AddInt { dst, a, imm: 1, upd: false }
            if dst == index && a == index)
        || !matches!(c[start + 21], Instr::Jump { target } if target as usize == start + 7)
    {
        return None;
    }
    let i_tail = match c[start + 22] {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    if !matches!(c[start + 23], Instr::AddInt { dst, a, imm: 1, upd: true }
        if dst == i_tail && a == i_tail)
        || !matches!(c[start + 24], Instr::StoreGlobalResolved { idx, src }
            if idx == i_global && src == i_tail)
        || !matches!(c[start + 25], Instr::Jump { target } if target as usize == start)
    {
        return None;
    }
    let globals = [
        source_global,
        sum_global,
        key_global,
        i_global,
        limit_global,
    ];
    distinct(&globals).then_some(ForInSumPlan {
        source_global,
        sum_global,
        key_global,
        i_global,
        limit_global,
        exit: start + 26,
    })
}

fn recognize_object_keys_len(
    proto: &crate::bytecode::FuncProto,
    ip: usize,
) -> Option<ObjectKeysLenPlan> {
    use crate::bytecode::BitwiseOp;
    let start = ip.checked_sub(5)?;
    let end = start.checked_add(14)?;
    if end >= proto.code.len() || !matches!(proto.code[ip], Instr::ObjectKeys { .. }) {
        return None;
    }
    let c = &proto.code;
    let (i_head, i_global) = match c[start] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (limit, limit_global) = match c[start + 1] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    if !matches!(c[start + 2], Instr::JumpIfNotLt { a, b, target }
        if a == i_head && b == limit && target as usize == start + 15)
    {
        return None;
    }
    let (sum, sum_global) = match c[start + 3] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (source, source_global) = match c[start + 4] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let keys = match c[start + 5] {
        Instr::ObjectKeys { dst, obj } if obj == source => dst,
        _ => return None,
    };
    let len = match c[start + 6] {
        Instr::GetProp { dst, obj, name }
            if obj == keys
                && proto
                    .string_constants
                    .get(name as usize)
                    .is_some_and(|s| s == "length") =>
        {
            dst
        }
        _ => return None,
    };
    let added = match c[start + 7] {
        Instr::Add { dst, a, b } if a == sum && b == len => dst,
        _ => return None,
    };
    let zero = match c[start + 8] {
        Instr::LoadInt { dst, val: 0 } => dst,
        _ => return None,
    };
    let reduced = match c[start + 9] {
        Instr::Bitwise {
            dst,
            a,
            b,
            op: BitwiseOp::Or,
        } if a == added && b == zero => dst,
        _ => return None,
    };
    if !matches!(c[start + 10], Instr::StoreGlobal { idx, src }
        | Instr::StoreGlobalStrict { idx, src }
        | Instr::StoreGlobalResolved { idx, src }
            if idx == sum_global && src == reduced)
    {
        return None;
    }
    let i_tail = match c[start + 11] {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    if !matches!(c[start + 12], Instr::AddInt { dst, a, imm: 1, upd: true }
        if dst == i_tail && a == i_tail)
        || !matches!(c[start + 13], Instr::StoreGlobalResolved { idx, src }
            if idx == i_global && src == i_tail)
        || !matches!(c[start + 14], Instr::Jump { target } if target as usize == start)
    {
        return None;
    }
    let globals = [source_global, sum_global, i_global, limit_global];
    distinct(&globals).then_some(ObjectKeysLenPlan {
        source_global,
        sum_global,
        i_global,
        limit_global,
        exit: start + 15,
    })
}

impl<'p> Vm<'p> {
    #[inline]
    fn enum_loop_observation_free(&self) -> bool {
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some() {
            return false;
        }
        true
    }

    fn enum_plain_object(&self, value: Value) -> Option<(u32, &ObjMap)> {
        if !value.is_heap() {
            return None;
        }
        let idx = value.heap_index();
        if (self.global_this != 0 && idx == self.global_this)
            || self.module_namespaces.contains_key(&idx)
            || self.realm_global_objs.contains_key(&idx)
            || self.arguments_objs.contains_key(&idx)
        {
            return None;
        }
        match self.heap.get(idx) {
            HeapObj::Object(map) if !map.is_ctor && map.class.is_none() => Some((idx, map)),
            _ => None,
        }
    }

    /// A real, main-realm Array whose copy operations can perform no observable
    /// property protocol: no own side-table properties/accessors, no virtual
    /// length, no custom prototype, and no indexed property anywhere on the
    /// default Array/Object prototype chain.
    fn array_copy_plain_len(&self, value: Value) -> Option<usize> {
        if !value.is_heap() {
            return None;
        }
        let idx = value.heap_index();
        let len = match self.heap.get(idx) {
            HeapObj::Array(items) => items.len(),
            _ => return None,
        };
        if self.arguments_objs.contains_key(&idx)
            || self.arr_props.contains_key(&idx)
            || self.array_js_len.contains_key(&idx)
            || self.proto_of.contains_key(&idx)
            || self.array_proto_has_index
        {
            return None;
        }
        Some(len)
    }

    /// Prove the remaining named Gets performed by slice/concat are the
    /// side-effect-free main-realm intrinsics: the method and constructor data
    /// slots on %Array.prototype%, the default @@species getter on %Array%, and
    /// (for concat) absence of @@isConcatSpreadable on both prototype anchors.
    fn array_copy_chain_pristine(&self, receiver: Value, method: &str) -> Option<usize> {
        let len = self.array_copy_plain_len(receiver)?;
        if self.arr_proto == 0
            || self.obj_proto == 0
            || self.array_ctor == 0
            || self.proto_of.contains_key(&self.arr_proto)
            || self.proto_of.contains_key(&self.obj_proto)
        {
            return None;
        }
        let ap = match self.heap.get(self.arr_proto) {
            HeapObj::Object(map) if map.class.is_none() => map,
            _ => return None,
        };
        let method_ok = ap.pos(method).is_some_and(|slot| {
            !ap.attrs[slot].accessor
                && ap.vals[slot].is_heap()
                && matches!(self.heap.get(ap.vals[slot].heap_index()), HeapObj::Native(id)
                    if native::proto_method(*id)
                        .is_some_and(|(name, kind, _)| name == method && kind == 0))
        });
        let constructor_ok = ap.pos("constructor").is_some_and(|slot| {
            !ap.attrs[slot].accessor && ap.vals[slot] == Value::heap(self.array_ctor)
        });
        if !method_ok || !constructor_ok {
            return None;
        }
        let op = match self.heap.get(self.obj_proto) {
            HeapObj::Object(map) if map.class.is_none() => map,
            _ => return None,
        };
        if method == "concat"
            && (ap.pos("@@isConcatSpreadable").is_some()
                || op.pos("@@isConcatSpreadable").is_some())
        {
            return None;
        }
        let ctor = match self.heap.get(self.array_ctor) {
            HeapObj::Object(map) if map.is_ctor => map,
            _ => return None,
        };
        let species_ok = ctor.pos("@@species").is_some_and(|slot| {
            ctor.attrs[slot].accessor
                && ctor.attrs[slot].setter == Value::UNDEFINED
                && ctor.vals[slot].is_heap()
                && matches!(self.heap.get(ctor.vals[slot].heap_index()),
                    HeapObj::Native(id) if *id == native::SPECIES_GET)
        });
        species_ok.then_some(len)
    }

    pub(crate) fn try_sparse_forin_fold_reduce(
        &mut self,
        func_id: u32,
        ip: usize,
    ) -> Option<usize> {
        if !enum_loop_reduce_enabled()
            || !sparse_forin_fold_enabled()
            || !self.enum_loop_observation_free()
        {
            return None;
        }
        let proto = self.program.functions.get(func_id as usize)?;
        let plan = recognize_sparse_forin_fold(proto, ip)?;
        let globals = [
            plan.source_global,
            plan.count_global,
            plan.fold_global,
            plan.key_global,
        ];
        if globals.iter().any(|&g| g as usize >= self.globals.len())
            || globals
                .iter()
                .any(|&g| !self.global_slot_directly_routable(g))
            || self.eval_const_globals.contains(&plan.count_global)
            || self.eval_const_globals.contains(&plan.fold_global)
            || self.eval_const_globals.contains(&plan.key_global)
        {
            return None;
        }
        let count = self.globals[plan.count_global as usize];
        let fold = self.globals[plan.fold_global as usize];
        if !count.is_int() || !fold.is_number() {
            return None;
        }
        let source = self.globals[plan.source_global as usize];
        if !source.is_heap() {
            return None;
        }
        let source_idx = source.heap_index();
        if !matches!(self.heap.get(source_idx), HeapObj::Array(_))
            || self.arguments_objs.contains_key(&source_idx)
            || self.regexp_result_props.contains_key(&source_idx)
            || self.proto_of.contains_key(&source_idx)
            || self.array_proto_has_index
            || self.arr_proto == 0
            || self.obj_proto == 0
            || self.proto_of.contains_key(&self.arr_proto)
            || self.proto_of.contains_key(&self.obj_proto)
        {
            return None;
        }
        let prototypes_barren = [self.arr_proto, self.obj_proto].into_iter().all(|idx| {
            matches!(self.heap.get(idx), HeapObj::Object(map) if map.class.is_none()
                && map.keys.iter().zip(&map.attrs)
                    .all(|(key, attr)| is_hidden_key(key) || !attr.enumerable))
        });
        if !prototypes_barren {
            return None;
        }

        // This lane targets genuinely sparse side-table arrays. Besides avoiding
        // overhead on ordinary small for-in loops, the threshold means a failed
        // recognition never scans a million-slot dense prefix for one property.
        let side = self.arr_props.get(&source_idx)?;
        if side.len() < 512 {
            return None;
        }
        let dense = match self.heap.get(source_idx) {
            HeapObj::Array(items) => items,
            _ => return None,
        };
        let dense_len = dense.len();
        let mut dense_entries: Vec<(u32, Value)> = Vec::new();
        for (index, &value) in dense.iter().enumerate() {
            if value.is_hole() {
                continue;
            }
            if !value.is_number() || index >= u32::MAX as usize {
                return None;
            }
            dense_entries.push((index as u32, value));
        }
        let mut sparse_entries: Vec<(u32, Value)> = Vec::with_capacity(side.len());
        for slot in 0..side.len() {
            let key = &side.keys[slot];
            if is_hidden_key(key) {
                continue;
            }
            let Some(index) = canonical_index_str(key).filter(|&n| n < u32::MAX as usize) else {
                // Even a non-enumerable named key can shadow an inherited key;
                // declining keeps the proof independent of that protocol.
                return None;
            };
            if index < dense_len {
                // A side-table descriptor below the materialized prefix is
                // authoritative over its Vec placeholder. Keep all such cases
                // on the existing descriptor-aware path.
                return None;
            }
            let attr = side.attrs[slot];
            if !attr.enumerable {
                continue;
            }
            let value = side.vals[slot];
            if attr.accessor || !value.is_number() {
                return None;
            }
            sparse_entries.push((index as u32, value));
        }
        sparse_entries.sort_unstable_by_key(|(index, _)| *index);
        let key_count = dense_entries.len().checked_add(sparse_entries.len())?;
        let final_count = (count.as_int() as i64).checked_add(key_count as i64)?;
        let final_count = i32::try_from(final_count).ok()?;

        let modulus = plan.modulus as f64;
        let mut final_fold = fold.as_f64();
        let mut last_index = None;
        for (index, value) in dense_entries.into_iter().chain(sparse_entries) {
            final_fold = ((final_fold + index as f64) + value.as_f64()) % modulus;
            last_index = Some(index);
        }
        let final_key = last_index.map(|index| self.alloc_str(index.to_string()));
        if let Some(key) = final_key {
            self.globals[plan.key_global as usize] = key;
        }
        self.globals[plan.count_global as usize] = Value::int(final_count);
        self.globals[plan.fold_global as usize] = Value::num(final_fold);
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] sparse-forin-fold committed {} keys", key_count);
        }
        Some(plan.exit)
    }

    pub(crate) fn try_array_copy_len_reduce(
        &mut self,
        func_id: u32,
        ip: usize,
        base: usize,
        receiver: Value,
        method: &str,
    ) -> Option<usize> {
        if !array_copy_len_reduce_enabled()
            || !self.enum_loop_observation_free()
            || !matches!(method, "slice" | "concat")
        {
            return None;
        }
        let proto = self.program.functions.get(func_id as usize)?;
        let plan = recognize_array_copy_len(proto, ip)?;
        let globals = [
            plan.source_global,
            plan.sum_global,
            plan.i_global,
            plan.limit_global,
        ];
        if globals.iter().any(|&g| g as usize >= self.globals.len())
            || globals
                .iter()
                .any(|&g| !self.global_slot_directly_routable(g))
            || self.eval_const_globals.contains(&plan.sum_global)
            || self.eval_const_globals.contains(&plan.i_global)
            || self.globals[plan.source_global as usize] != receiver
        {
            return None;
        }
        let induction = self.globals[plan.i_global as usize];
        let limit = self.globals[plan.limit_global as usize];
        let sum = self.globals[plan.sum_global as usize];
        if !induction.is_int()
            || induction.as_int() != 0
            || !limit.is_int()
            || limit.as_int() < 512
            || !sum.is_int()
        {
            return None;
        }
        let source_len = self.array_copy_chain_pristine(receiver, method)?;
        let result_len = match plan.kind {
            ArrayCopyLenKind::Slice { start_reg, end_reg } if method == "slice" => {
                let start = self.get(base, start_reg);
                let end = self.get(base, end_reg);
                if !start.is_int() || !end.is_int() || source_len > i32::MAX as usize {
                    return None;
                }
                let len = source_len as i64;
                let norm = |n: i32| {
                    if n < 0 {
                        (len + i64::from(n)).max(0)
                    } else {
                        i64::from(n).min(len)
                    }
                };
                let lo = norm(start.as_int());
                let hi = norm(end.as_int());
                usize::try_from((hi - lo).max(0)).ok()?
            }
            ArrayCopyLenKind::Concat { arg_reg } if method == "concat" => {
                let argument = self.get(base, arg_reg);
                let argument_len = self.array_copy_plain_len(argument)?;
                source_len.checked_add(argument_len)?
            }
            _ => return None,
        };
        let iterations = limit.as_int() as u32;
        let final_sum =
            (sum.as_int() as u32).wrapping_add((result_len as u32).wrapping_mul(iterations));
        self.globals[plan.sum_global as usize] = Value::int(final_sum as i32);
        self.globals[plan.i_global as usize] = limit;
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] array-copy-length committed {} {} iterations",
                method, iterations
            );
        }
        Some(plan.exit)
    }

    pub(crate) fn try_in_probe_reduce(&mut self, func_id: u32, ip: usize) -> Option<usize> {
        if !in_probe_reduce_enabled() || !self.enum_loop_observation_free() {
            return None;
        }
        let proto = self.program.functions.get(func_id as usize)?;
        let plan = recognize_in_probe_reduce(proto, ip)?;
        let mut globals = vec![plan.source_global, plan.i_global, plan.limit_global];
        globals.extend(plan.probes.iter().map(|p| p.counter_global));
        if globals.iter().any(|&g| g as usize >= self.globals.len())
            || globals
                .iter()
                .any(|&g| !self.global_slot_directly_routable(g))
            || self.eval_const_globals.contains(&plan.i_global)
            || plan
                .probes
                .iter()
                .any(|p| self.eval_const_globals.contains(&p.counter_global))
        {
            return None;
        }
        let induction = self.globals[plan.i_global as usize];
        let limit = self.globals[plan.limit_global as usize];
        if !induction.is_int()
            || induction.as_int() != 0
            || !limit.is_int()
            || limit.as_int() < 1024
        {
            return None;
        }
        let iterations = limit.as_int() as i64;
        let samples: i64 = plan
            .probes
            .iter()
            .map(|p| i64::from(p.modulus).min(iterations))
            .sum();
        if samples > 65_536 {
            return None;
        }

        let source = self.globals[plan.source_global as usize];
        if !source.is_heap() {
            return None;
        }
        let source_idx = source.heap_index();
        if !matches!(self.heap.get(source_idx), HeapObj::Array(_))
            || self.arguments_objs.contains_key(&source_idx)
            || self.proto_of.contains_key(&source_idx)
            || self.arr_proto == 0
            || self.obj_proto == 0
            || self.proto_of.contains_key(&self.arr_proto)
            || self.proto_of.contains_key(&self.obj_proto)
            || !matches!(self.heap.get(self.arr_proto), HeapObj::Object(m) if m.class.is_none())
            || !matches!(self.heap.get(self.obj_proto), HeapObj::Object(m) if m.class.is_none())
        {
            return None;
        }

        // Compute every result before committing any global. Presence is a pure
        // read for the guarded Array/default-chain shape; a later overflow or
        // unsupported arithmetic value therefore still fails with zero state
        // changed and lets the already-entered first HasProp execute normally.
        let mut results = Vec::with_capacity(plan.probes.len());
        for probe in &plan.probes {
            let current = self.globals[probe.counter_global as usize];
            if !current.is_int() {
                return None;
            }
            let modulus = i64::from(probe.modulus);
            let span = modulus.min(iterations);
            let mut hits = 0i64;
            for residue in 0..span {
                let key = residue
                    .checked_mul(i64::from(probe.scale))?
                    .checked_add(i64::from(probe.offset))?;
                let key = i32::try_from(key).ok()?;
                if self.has_property(source, Value::int(key)) {
                    // Count induction values in [0, iterations) with this
                    // residue, without materialising the full outer loop.
                    hits = hits.checked_add(1 + (iterations - 1 - residue) / modulus)?;
                }
            }
            let final_value = (current.as_int() as i64).checked_add(hits)?;
            results.push((probe.counter_global, i32::try_from(final_value).ok()?));
        }
        for (global, value) in results {
            self.globals[global as usize] = Value::int(value);
        }
        self.globals[plan.i_global as usize] = limit;
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] periodic-in-probes committed {} iterations / {} probes",
                iterations,
                plan.probes.len()
            );
        }
        Some(plan.exit)
    }

    pub(crate) fn try_forin_count_reduce(&mut self, func_id: u32, ip: usize) -> Option<usize> {
        if !enum_loop_reduce_enabled()
            || !enum_count_reduce_enabled()
            || !self.enum_loop_observation_free()
        {
            return None;
        }
        let proto = self.program.functions.get(func_id as usize)?;
        let plan = recognize_forin_count(proto, ip)?;
        let globals = [
            plan.source_global,
            plan.count_global,
            plan.key_global,
            plan.i_global,
            plan.limit_global,
        ];
        if globals.iter().any(|&g| g as usize >= self.globals.len())
            || globals
                .iter()
                .any(|&g| !self.global_slot_directly_routable(g))
            || self.eval_const_globals.contains(&plan.count_global)
            || self.eval_const_globals.contains(&plan.key_global)
            || self.eval_const_globals.contains(&plan.i_global)
        {
            return None;
        }
        let outer = self.globals[plan.i_global as usize];
        let limit = self.globals[plan.limit_global as usize];
        let count = self.globals[plan.count_global as usize];
        if !outer.is_int()
            || outer.as_int() != 0
            || !limit.is_int()
            || limit.as_int() < 1024
            || !count.is_int()
        {
            return None;
        }

        // Only the genuine default-chain Array shape is observation-free:
        // Array/Object prototype anchors are ordinary objects, and no Proxy or
        // child-realm/custom receiver link can participate in the snapshot.
        let source = self.globals[plan.source_global as usize];
        if !source.is_heap() {
            return None;
        }
        let source_idx = source.heap_index();
        if !matches!(self.heap.get(source_idx), HeapObj::Array(_))
            || self.arguments_objs.contains_key(&source_idx)
            || self.proto_of.contains_key(&source_idx)
            || self.arr_proto == 0
            || self.obj_proto == 0
            || self.proto_of.contains_key(&self.arr_proto)
            || self.proto_of.contains_key(&self.obj_proto)
            || !matches!(self.heap.get(self.arr_proto), HeapObj::Object(m) if m.class.is_none())
            || !matches!(self.heap.get(self.obj_proto), HeapObj::Object(m) if m.class.is_none())
        {
            return None;
        }

        // Build ONE real snapshot with the engine's authoritative ordering,
        // enumerability and prototype-shadowing logic. The exact recognized
        // body cannot mutate anything, so every remaining outer iteration
        // would produce the same live key sequence.
        let snapshot = self.for_in_keys(source).ok()?;
        let (per_iteration, last_key) = match self.heap.get(snapshot.heap_index()) {
            HeapObj::Array(items) if items.len() >= crate::bytecode::FORIN_SNAPSHOT_PREFIX => (
                items.len() - crate::bytecode::FORIN_SNAPSHOT_PREFIX,
                items
                    .last()
                    .copied()
                    .filter(|_| items.len() > crate::bytecode::FORIN_SNAPSHOT_PREFIX),
            ),
            _ => return None,
        };
        let iterations = limit.as_int() as i64;
        let delta = i64::try_from(per_iteration).ok()?.checked_mul(iterations)?;
        // The bytecode increments one at a time and promotes to Double at i32
        // overflow. Restrict the fused lane to the exact Int-tagged outcome;
        // huge counters retain the ordinary promotion/rounding path.
        let final_count = (count.as_int() as i64).checked_add(delta)?;
        let final_count = i32::try_from(final_count).ok()?;

        if let Some(key) = last_key {
            self.globals[plan.key_global as usize] = key;
        }
        self.globals[plan.count_global as usize] = Value::int(final_count);
        self.globals[plan.i_global as usize] = limit;
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] enum-forin-count committed {} x {} keys",
                iterations, per_iteration
            );
        }
        Some(plan.exit)
    }

    pub(crate) fn try_forin_sum_reduce(&mut self, func_id: u32, ip: usize) -> Option<usize> {
        if !enum_loop_reduce_enabled() || !self.enum_loop_observation_free() {
            return None;
        }
        let proto = self.program.functions.get(func_id as usize)?;
        let i_global = match proto.code.get(ip.checked_sub(4)?)? {
            Instr::LoadGlobal { idx, .. } => *idx,
            _ => return None,
        };
        let i = self.globals.get(i_global as usize).copied()?;
        if !i.is_int() || i.as_int() != 0 {
            return None;
        }
        let plan = recognize_forin_sum(proto, ip)?;
        let globals = [
            plan.source_global,
            plan.sum_global,
            plan.key_global,
            plan.i_global,
            plan.limit_global,
        ];
        if globals.iter().any(|&g| g as usize >= self.globals.len())
            || globals
                .iter()
                .any(|&g| !self.global_slot_directly_routable(g))
            || self.eval_const_globals.contains(&plan.sum_global)
            || self.eval_const_globals.contains(&plan.key_global)
            || self.eval_const_globals.contains(&plan.i_global)
        {
            return None;
        }
        let limit = self.globals[plan.limit_global as usize];
        let sum = self.globals[plan.sum_global as usize];
        if !limit.is_int() || limit.as_int() < 1024 || !sum.is_int() {
            return None;
        }
        let source = self.globals[plan.source_global as usize];
        let (source_idx, map) = self.enum_plain_object(source)?;
        // This narrow reducer handles the dominant default-prototype case.  A
        // custom chain stays ordinary so inherited keys/shadowing remain fully
        // observable through the existing path.
        if self.proto_of.contains_key(&source_idx) || self.obj_proto == 0 {
            return None;
        }
        let object_proto = match self.heap.get(self.obj_proto) {
            HeapObj::Object(map) => map,
            _ => return None,
        };
        if object_proto
            .keys
            .iter()
            .zip(&object_proto.attrs)
            .any(|(key, attr)| !is_hidden_key(key) && attr.enumerable)
            || self
                .proto_of
                .get(&self.obj_proto)
                .is_some_and(|proto| proto.is_heap())
        {
            return None;
        }
        let mut cycle = 0u32;
        let mut last_key = None;
        for slot in spec_key_order(&map.keys) {
            let key = &map.keys[slot];
            let attr = map.attrs[slot];
            if is_hidden_key(key) || !attr.enumerable {
                continue;
            }
            if attr.accessor || !map.vals[slot].is_int() {
                return None;
            }
            cycle = cycle.wrapping_add(map.vals[slot].as_int() as u32);
            last_key = Some(key.clone());
        }
        // Drop immutable map borrows before allocating the final key string.
        let final_key = last_key.map(|key| self.alloc_str(key));
        let remaining = limit.as_int() as u32;
        let result = (sum.as_int() as u32).wrapping_add(cycle.wrapping_mul(remaining));
        if let Some(key) = final_key {
            self.globals[plan.key_global as usize] = key;
        }
        self.globals[plan.sum_global as usize] = Value::int(result as i32);
        self.globals[plan.i_global as usize] = limit;
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] enum-forin-sum committed {} iterations", remaining);
        }
        Some(plan.exit)
    }

    pub(crate) fn try_object_keys_len_reduce(&mut self, func_id: u32, ip: usize) -> Option<usize> {
        if !enum_loop_reduce_enabled() || !self.enum_loop_observation_free() {
            return None;
        }
        let proto = self.program.functions.get(func_id as usize)?;
        let i_global = match proto.code.get(ip.checked_sub(5)?)? {
            Instr::LoadGlobal { idx, .. } => *idx,
            _ => return None,
        };
        let i = self.globals.get(i_global as usize).copied()?;
        if !i.is_int() || i.as_int() != 0 {
            return None;
        }
        let plan = recognize_object_keys_len(proto, ip)?;
        let globals = [
            plan.source_global,
            plan.sum_global,
            plan.i_global,
            plan.limit_global,
        ];
        if globals.iter().any(|&g| g as usize >= self.globals.len())
            || globals
                .iter()
                .any(|&g| !self.global_slot_directly_routable(g))
            || self.eval_const_globals.contains(&plan.sum_global)
            || self.eval_const_globals.contains(&plan.i_global)
        {
            return None;
        }
        let limit = self.globals[plan.limit_global as usize];
        let sum = self.globals[plan.sum_global as usize];
        if !limit.is_int() || limit.as_int() < 1024 || !sum.is_int() {
            return None;
        }
        let source = self.globals[plan.source_global as usize];
        let (_, map) = self.enum_plain_object(source)?;
        let count = map
            .keys
            .iter()
            .zip(&map.attrs)
            .filter(|(key, attr)| !is_hidden_key(key) && attr.enumerable)
            .count() as u32;
        let remaining = limit.as_int() as u32;
        let result = (sum.as_int() as u32).wrapping_add(count.wrapping_mul(remaining));
        self.globals[plan.sum_global as usize] = Value::int(result as i32);
        self.globals[plan.i_global as usize] = limit;
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] enum-object-keys committed {} iterations", remaining);
        }
        Some(plan.exit)
    }
}
