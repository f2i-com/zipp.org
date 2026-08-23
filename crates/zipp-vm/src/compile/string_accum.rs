// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;
// Named explicitly so this file does not depend on how the parent module
// re-exports the AST (`use super::*` would supply it too; an explicit import
// shadows the glob rather than clashing with it).
use crate::parse::ast;

/// Compile a parsed program into bytecode.
/// Is constant Value `v` a (pending) string literal? String constants are encoded
/// as `Value::heap(STRING_CONST_BIT | si)` (see `add_string_const`).
pub(crate) fn is_string_const(v: Value) -> bool {
    v.is_heap() && (v.heap_index() & STRING_CONST_BIT) != 0
}

/// Does global slot `g`'s last write BEFORE `before` initialise it to a string
/// literal? Recognises the `s = "…"` shape the compiler emits as an adjacent
/// `LoadConst{dst:r}; StoreGlobal{idx:g, src:r}`.
pub(crate) fn global_inits_string(code: &[Instr], constants: &[Value], g: u32, before: usize) -> bool {
    let mut store_ip = None;
    for (ip, instr) in code.iter().enumerate().take(before) {
        if let Instr::StoreGlobal { idx, .. } = *instr {
            if idx == g {
                store_ip = Some(ip);
            }
        }
    }
    let sp = match store_ip {
        Some(s) if s >= 1 => s,
        _ => return false,
    };
    let src = match code[sp] {
        Instr::StoreGlobal { src, .. } => src,
        _ => return false,
    };
    match code[sp - 1] {
        Instr::LoadConst { dst, idx } if dst == src => {
            constants.get(idx as usize).copied().map(is_string_const).unwrap_or(false)
        }
        _ => false,
    }
}

/// Does instruction `i` reference register `r` in ANY of its register fields?
/// CONSERVATIVE: handles the simple op set a string-accumulator loop body is
/// restricted to (see `loop_inplace_safe`); any other variant returns `true`
/// (assume it touches `r`), so an unrecognised op can never let the buffer escape
/// unnoticed.
pub(crate) fn instr_touches(i: &Instr, r: Reg) -> bool {
    match *i {
        Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } | Instr::LoadGlobal { dst, .. } => {
            dst == r
        }
        Instr::Move { dst, src } => dst == r || src == r,
        Instr::StoreGlobal { src, .. }
        | Instr::StoreGlobalStrict { src, .. }
        | Instr::StoreGlobalResolved { src, .. } => src == r,
        Instr::AddInt { dst, a, .. } | Instr::Neg { dst, a } => dst == r || a == r,
        Instr::Add { dst, a, b }
        | Instr::Sub { dst, a, b }
        | Instr::Mul { dst, a, b }
        | Instr::Div { dst, a, b }
        | Instr::Mod { dst, a, b }
        | Instr::StrConcat { dst, a, b }
        | Instr::StrAppendInPlace { dst, a, b }
        | Instr::StrConcatChain { dst, a, b }
        | Instr::Lt { dst, a, b }
        | Instr::Le { dst, a, b }
        | Instr::Gt { dst, a, b }
        | Instr::Ge { dst, a, b }
        | Instr::Eq { dst, a, b }
        | Instr::Ne { dst, a, b } => dst == r || a == r || b == r,
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => cond == r,
        Instr::JumpIfNotLt { a, b, .. } | Instr::JumpIfNotLe { a, b, .. } => a == r || b == r,
        Instr::Jump { .. } => false,
        _ => true, // unrecognised op → assume it could reference `r`
    }
}

/// Is op `i` one of the simple, no-user-code, no-implicit-global-access ops a
/// linear string-accumulator loop body may contain? (Calls, heap property/index
/// ops, closures, etc. could read/alias the accumulator global, so they bar the
/// in-place rewrite.)
pub(crate) fn is_simple_loop_op(i: &Instr) -> bool {
    matches!(
        i,
        Instr::LoadInt { .. }
            | Instr::LoadConst { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. }
            | Instr::Add { .. }
            // W11 (B124) fused chain link — the same operator as `Add` (whose
            // presence here predates it); a fused chain in the body must not
            // bar the surrounding accumulator loop's rewrite.
            | Instr::StrConcatChain { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Div { .. }
            | Instr::Mod { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::Jump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotLe { .. }
    )
}

/// Can the string accumulator `g` (loaded at `load_ip`, appended at `k`, stored at
/// `store_ip`) in loop `[start, end]` be mutated IN PLACE soundly? PROVES the
/// accumulator's buffer is never aliased, so an in-place append is unobservable:
///  - top-level script function (runs once — a global accumulator must not persist
///    into a second call that would mutate a returned/escaped buffer);
///  - loop not nested in another loop (so post-loop code runs once, no re-entry);
///  - `g` is written ONLY by its init (before the loop) and this loop's store, read
///    ONLY by this loop's load (post-loop reads are fine — building is done);
///  - the body contains only simple ops (no calls/heap ops that could read `g`);
///  - the loaded value `a` and the result `dst` (the buffer's registers) are
///    touched ONLY by the three accumulator ops — never copied/stored elsewhere,
///    so the buffer can't leak into a second live reference.
#[allow(clippy::too_many_arguments)]
pub(crate) fn loop_inplace_safe(
    code: &[Instr],
    g: u32,
    start: usize,
    end: usize,
    is_top_level: bool,
    load_ip: usize,
    k: usize,
    store_ip: usize,
    a: Reg,
    dst: Reg,
) -> bool {
    if !is_top_level {
        return false;
    }
    // Not nested in another back-edge loop.
    for (jp, instr) in code.iter().enumerate() {
        if let Instr::Jump { target } = *instr {
            let t = target as usize;
            if t < jp && (t, jp) != (start, end) && t <= start && jp >= end {
                return false;
            }
        }
    }
    // `g` access discipline across the WHOLE function.
    let (mut st_before, mut st_in, mut st_after) = (0u32, 0u32, 0u32);
    let (mut ld_before, mut ld_in) = (0u32, 0u32);
    for (ip, instr) in code.iter().enumerate() {
        match *instr {
            Instr::StoreGlobal { idx, .. }
            | Instr::StoreGlobalStrict { idx, .. }
            | Instr::StoreGlobalResolved { idx, .. } if idx == g => {
                if ip < start {
                    st_before += 1
                } else if ip <= end {
                    st_in += 1
                } else {
                    st_after += 1
                }
            }
            Instr::LoadGlobal { idx, .. } if idx == g => {
                if ip < start {
                    ld_before += 1
                } else if ip <= end {
                    ld_in += 1
                }
            }
            _ => {}
        }
    }
    if st_before != 1 || st_in != 1 || st_after != 0 || ld_before != 0 || ld_in != 1 {
        return false;
    }
    // Only simple ops in the body, and the buffer's registers (`a`, `dst`) are
    // touched ONLY by the load/append/store — never leaking the buffer elsewhere.
    for ip in start..=end {
        // The candidate itself is the new two-Add op. Its primitive fast arm
        // runs no user code, while every coercing shape declines before
        // mutation to the exact fresh-result fallback. Keep it exempt here;
        // any *other* AddRightPair remains outside `is_simple_loop_op` and is
        // therefore the conservative unknown-op barrier.
        if ip != k && !is_simple_loop_op(&code[ip]) {
            return false;
        }
        if ip != load_ip && ip != k && ip != store_ip && (instr_touches(&code[ip], a) || instr_touches(&code[ip], dst)) {
            return false;
        }
    }
    true
}

/// Does instruction `i` READ register `r` — consume its VALUE through any
/// operand field? CONSERVATIVE: any variant not enumerated returns `true`.
/// Write-only `dst` fields deliberately do NOT count: a write to `r` is
/// handled by the callers' own rules, and counting it here would make the
/// dead-statement-value check below reject its own `Move` destination.
///
/// Every arm below was written against the variant's declaration in
/// `bytecode.rs` — the register fields, the whole register fields (an
/// `arg_base`/`argc` pair is a WINDOW of registers), and nothing but the
/// register fields (`name`/`idx` in `u32` positions are constant-pool or slot
/// indices, not registers). A missed READ field here would let a buffer alias
/// escape unnoticed, which is a wrong answer, so extend this list only with
/// the declaration in front of you.
pub(crate) fn accum_may_read(i: &Instr, r: Reg) -> bool {
    let in_args = |base: Reg, argc: u16| r >= base && (r as u32) < base as u32 + argc as u32;
    match *i {
        Instr::LoadInt { .. }
        | Instr::LoadConst { .. }
        | Instr::LoadGlobal { .. }
        | Instr::LoadGlobalOrUndefined { .. }
        | Instr::LoadGlobalOrUndefinedDyn { .. }
        | Instr::LoadBool { .. }
        | Instr::LoadUndefined { .. }
        | Instr::LoadNull { .. }
        | Instr::UpvalGet { .. }
        | Instr::Jump { .. }
        | Instr::ReturnUndefined => false,
        Instr::Move { src, .. } => src == r,
        Instr::StoreGlobal { src, .. }
        | Instr::StoreGlobalStrict { src, .. }
        | Instr::StoreGlobalResolved { src, .. }
        | Instr::UpvalSet { src, .. } => src == r,
        Instr::AddInt { a, .. }
        | Instr::Neg { a, .. }
        | Instr::Not { a, .. }
        | Instr::ToNum { a, .. }
        | Instr::ToStr { a, .. }
        | Instr::TypeOf { a, .. }
        | Instr::TypeOfIs { a, .. }
        | Instr::BitNot { a, .. } => a == r,
        Instr::Add { a, b, .. }
        | Instr::Sub { a, b, .. }
        | Instr::Mul { a, b, .. }
        | Instr::Div { a, b, .. }
        | Instr::Mod { a, b, .. }
        | Instr::StrConcat { a, b, .. }
        | Instr::StrAppendInPlace { a, b, .. }
        | Instr::StrConcatChain { a, b, .. }
        | Instr::Bitwise { a, b, .. }
        | Instr::Lt { a, b, .. }
        | Instr::Le { a, b, .. }
        | Instr::Gt { a, b, .. }
        | Instr::Ge { a, b, .. }
        | Instr::Eq { a, b, .. }
        | Instr::Ne { a, b, .. }
        | Instr::JumpIfNotLt { a, b, .. }
        | Instr::JumpIfNotLe { a, b, .. } => a == r || b == r,
        Instr::AddRightPair { a, b, c, .. } => a == r || b == r || c == r,
        Instr::Pad2Concat { src, .. } | Instr::Pad2Conditional { src, .. } => src == r,
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => cond == r,
        Instr::Return { src } | Instr::CheckCoercible { src } => src == r,
        Instr::CellGet { cell, .. } => cell == r,
        Instr::CellSet { cell, src } => cell == r || src == r,
        Instr::GetProp { obj, .. } => obj == r,
        Instr::SetProp { obj, val, .. } => obj == r || val == r,
        Instr::GetIndex { obj, key, .. } => obj == r || key == r,
        Instr::SetIndex { obj, key, val } => obj == r || key == r || val == r,
        Instr::GetIndexConcat { obj, key, .. } => obj == r || key == r,
        Instr::SetIndexConcat { obj, key, val, .. } => obj == r || key == r || val == r,
        Instr::ToConcatKey { src, .. } => src == r,
        Instr::Call { callee, arg_base, argc, .. } => callee == r || in_args(arg_base, argc),
        Instr::TailCall { callee, arg_base, argc } => callee == r || in_args(arg_base, argc),
        Instr::CallMethod { obj, arg_base, argc, .. } => obj == r || in_args(arg_base, argc),
        Instr::MathOp { arg_base, argc, .. } | Instr::StaticFn { arg_base, argc, .. } => {
            in_args(arg_base, argc)
        }
        _ => true, // unrecognised op → assume it reads `r`
    }
}

/// `instr_touches` over the WIDER op set the local-accumulator proof scans —
/// reads OR writes, conservative `true` for anything not enumerated. (The
/// original `instr_touches` stays as-is for the global proof's narrow body.)
pub(crate) fn accum_touches(i: &Instr, r: Reg) -> bool {
    if accum_may_read(i, r) {
        return true;
    }
    // Writes. Every enumerated variant's dst field, verified like the reads.
    match *i {
        Instr::LoadInt { dst, .. }
        | Instr::LoadConst { dst, .. }
        | Instr::LoadGlobal { dst, .. }
        | Instr::LoadGlobalOrUndefined { dst, .. }
        | Instr::LoadGlobalOrUndefinedDyn { dst, .. }
        | Instr::LoadBool { dst, .. }
        | Instr::LoadUndefined { dst }
        | Instr::LoadNull { dst }
        | Instr::Move { dst, .. }
        | Instr::AddInt { dst, .. }
        | Instr::Neg { dst, .. }
        | Instr::Not { dst, .. }
        | Instr::ToNum { dst, .. }
        | Instr::ToStr { dst, .. }
        | Instr::TypeOf { dst, .. }
        | Instr::TypeOfIs { dst, .. }
        | Instr::BitNot { dst, .. }
        | Instr::Add { dst, .. }
        | Instr::Sub { dst, .. }
        | Instr::Mul { dst, .. }
        | Instr::Div { dst, .. }
        | Instr::Mod { dst, .. }
        | Instr::StrConcat { dst, .. }
        | Instr::StrAppendInPlace { dst, .. }
        | Instr::StrConcatChain { dst, .. }
        | Instr::Bitwise { dst, .. }
        | Instr::Lt { dst, .. }
        | Instr::Le { dst, .. }
        | Instr::Gt { dst, .. }
        | Instr::Ge { dst, .. }
        | Instr::Eq { dst, .. }
        | Instr::Ne { dst, .. }
        | Instr::CellGet { dst, .. }
        | Instr::UpvalGet { dst, .. }
        | Instr::GetProp { dst, .. }
        | Instr::GetIndex { dst, .. }
        | Instr::GetIndexConcat { dst, .. }
        | Instr::Call { dst, .. }
        | Instr::CallMethod { dst, .. }
        | Instr::MathOp { dst, .. }
        | Instr::StaticFn { dst, .. }
        | Instr::ToConcatKey { dst, .. } => dst == r,
        Instr::AddRightPair { dst, .. }
        | Instr::Pad2Concat { dst, .. }
        | Instr::Pad2Conditional { dst, .. } => dst == r,
        Instr::StoreGlobal { .. }
        | Instr::StoreGlobalStrict { .. }
        | Instr::StoreGlobalResolved { .. }
        | Instr::UpvalSet { .. }
        | Instr::CellSet { .. }
        | Instr::SetProp { .. }
        | Instr::SetIndex { .. }
        | Instr::SetIndexConcat { .. }
        | Instr::TailCall { .. }
        | Instr::Return { .. }
        | Instr::ReturnUndefined
        | Instr::Jump { .. }
        | Instr::JumpIfFalse { .. }
        | Instr::JumpIfTrue { .. }
        | Instr::JumpIfNotLt { .. }
        | Instr::JumpIfNotLe { .. }
        | Instr::CheckCoercible { .. } => false,
        _ => true, // unrecognised op → assume it touches `r`
    }
}

/// Does instruction `i` WRITE register `r` — the write half of
/// `accum_touches`, `false` conservatively for anything not enumerated (a
/// missed WRITE here only under-covers a read and declines, never admits).
pub(crate) fn accum_writes(i: &Instr, r: Reg) -> bool {
    match *i {
        Instr::LoadInt { dst, .. }
        | Instr::LoadConst { dst, .. }
        | Instr::LoadGlobal { dst, .. }
        | Instr::LoadGlobalOrUndefined { dst, .. }
        | Instr::LoadGlobalOrUndefinedDyn { dst, .. }
        | Instr::LoadBool { dst, .. }
        | Instr::LoadUndefined { dst }
        | Instr::LoadNull { dst }
        | Instr::Move { dst, .. }
        | Instr::AddInt { dst, .. }
        | Instr::Neg { dst, .. }
        | Instr::Not { dst, .. }
        | Instr::ToNum { dst, .. }
        | Instr::ToStr { dst, .. }
        | Instr::TypeOf { dst, .. }
        | Instr::TypeOfIs { dst, .. }
        | Instr::BitNot { dst, .. }
        | Instr::Add { dst, .. }
        | Instr::Sub { dst, .. }
        | Instr::Mul { dst, .. }
        | Instr::Div { dst, .. }
        | Instr::Mod { dst, .. }
        | Instr::StrConcat { dst, .. }
        | Instr::StrAppendInPlace { dst, .. }
        | Instr::StrConcatChain { dst, .. }
        | Instr::Bitwise { dst, .. }
        | Instr::Lt { dst, .. }
        | Instr::Le { dst, .. }
        | Instr::Gt { dst, .. }
        | Instr::Ge { dst, .. }
        | Instr::Eq { dst, .. }
        | Instr::Ne { dst, .. }
        | Instr::CellGet { dst, .. }
        | Instr::UpvalGet { dst, .. }
        | Instr::GetProp { dst, .. }
        | Instr::GetIndex { dst, .. }
        | Instr::GetIndexConcat { dst, .. }
        | Instr::Call { dst, .. }
        | Instr::CallMethod { dst, .. }
        | Instr::MathOp { dst, .. }
        | Instr::StaticFn { dst, .. }
        | Instr::ToConcatKey { dst, .. } => dst == r,
        Instr::AddRightPair { dst, .. }
        | Instr::Pad2Concat { dst, .. }
        | Instr::Pad2Conditional { dst, .. } => dst == r,
        _ => false,
    }
}

/// Is `code[move_ip]`'s destination `t` UNOBSERVABLE — i.e. can no read of
/// `t` anywhere in the function see the value this `Move` stored? True when
/// every read of `t` is covered by a DOMINATING write found by scanning
/// straight-line code backwards from the read: the scan fails (conservatively)
/// at a jump target (control can enter with `t` from elsewhere), at our own
/// `move_ip`, or at ANY op outside the enumerated set — which includes every
/// `PushHandler`/`PushFinally`-style op carrying a hidden control target.
///
/// This exists because the register allocator REUSES statement-value slots:
/// the discarded value of one `out += x` may live in a register that a
/// different branch uses as a genuine scratch, so "never read anywhere" is
/// too blunt — the reuse always re-writes the register first, and this scan
/// proves exactly that.
fn move_dst_unobservable(code: &[Instr], targets: &[bool], t: Reg, move_ip: usize) -> bool {
    'reads: for (rp, instr) in code.iter().enumerate() {
        if rp == move_ip || !accum_may_read(instr, t) {
            continue;
        }
        let mut ip = rp;
        while ip > 0 {
            ip -= 1;
            if ip == move_ip {
                return false; // reached our Move with no covering write
            }
            if accum_writes(&code[ip], t) {
                continue 'reads; // covered on the only fall-through path
            }
            if targets[ip] {
                return false; // block boundary — another path can reach the read
            }
            match code[ip] {
                // An unconditional transfer means the read is only reachable
                // via a jump target we would have crossed — decline.
                Instr::Jump { .. } | Instr::Return { .. } | Instr::ReturnUndefined => {
                    return false;
                }
                _ => {}
            }
            if accum_touches(&code[ip], Reg::MAX) {
                // `Reg::MAX` is never a real register: `accum_touches` returns
                // true for it ONLY via the unrecognised-op arm, i.e. this op is
                // outside the enumerated set (it may carry a hidden control
                // target, e.g. PushHandler) — decline.
                return false;
            }
        }
        return false; // ran off the top of the function
    }
    true
}

/// Rewrite `out = out + b` inside a loop to `StrAppendInPlace` for a
/// FUNCTION-LOCAL accumulator register. The global proof
/// (`loop_inplace_safe`) restricts the body to call-free simple ops because a
/// call could read the accumulator GLOBAL by name; a local register cannot be
/// named by any other code, so calls in the body are admissible and the whole
/// burden moves to proving the REGISTER never leaks a second live reference
/// to the buffer while appends can still happen:
///
///  - `r` is not a parameter (sloppy `arguments` aliases parameter
///    registers invisibly), the function builds no `arguments`/rest object,
///    and is not a generator/async (caution, mirrors the inliner);
///  - the loop is not enclosed by another back-edge, and no jump from outside
///    the loop targets its interior — so once it exits, no append runs again
///    in this activation (a fresh call gets fresh registers);
///  - before/inside the loop, `r` is touched ONLY by: one `LoadConst` of a
///    string literal before the loop (the init — the first append copies off
///    the interned literal, so earlier escapes of the INIT value are safe),
///    the self-append `Add{dst:r, a:r}` sites themselves, and `Move`s of `r`
///    into a register that is NEVER READ anywhere in the function (the
///    discarded statement value of `out += x` — dead by construction);
///  - AFTER the loop anything goes: appends are unreachable, the buffer is
///    complete and behaves as an ordinary immutable string from then on.
///
/// A function whose accumulator is captured by a closure (or in a
/// direct-eval/`with` scope) never matches structurally: those locals compile
/// to cell operations, not a plain-register `Add{dst:r, a:r}` fed by a
/// `LoadConst` init.
pub(crate) fn rewrite_local_accumulators(f: &mut FuncProto) {
    if f.is_generator || f.is_async || f.arguments_reg.is_some() || f.rest_reg.is_some() {
        return;
    }
    let n = f.code.len();
    let param_top: Reg = f.param_count;
    let mut rewrites: Vec<usize> = Vec::new();
    for j in 0..n {
        let start = match f.code[j] {
            Instr::Jump { target } if (target as usize) < j => target as usize,
            _ => continue,
        };
        let end = j;
        // Not enclosed by another back-edge, and no OUTSIDE jump into the
        // interior (falling in at `start`, or a pre-loop jump TO `start`, is
        // the canonical entry and fine).
        let mut enclosed = false;
        for (jp, instr) in f.code.iter().enumerate() {
            let target = match *instr {
                Instr::Jump { target }
                | Instr::JumpIfFalse { target, .. }
                | Instr::JumpIfTrue { target, .. }
                | Instr::JumpIfNotLt { target, .. }
                | Instr::JumpIfNotLe { target, .. } => target as usize,
                _ => continue,
            };
            if target < jp && (target, jp) != (start, end) && target <= start && jp >= end {
                enclosed = true; // an enclosing back-edge
                break;
            }
            if (jp < start || jp > end) && target > start && target <= end {
                enclosed = true; // a jump into the interior from outside
                break;
            }
        }
        if enclosed {
            continue;
        }
        // Candidate accumulators: self-append `Add { dst: r, a: r }` in the
        // body, r above the parameter registers (reg 0 is `this`).
        let mut accs: Vec<Reg> = Vec::new();
        for k in start..=end {
            if let Instr::Add { dst, a, .. } = f.code[k] {
                if dst == a && dst > param_top && !accs.contains(&dst) {
                    accs.push(dst);
                }
            }
        }
        'acc: for r in accs {
            let appends: Vec<usize> = (start..=end)
                .filter(|&k| matches!(f.code[k], Instr::Add { dst, a, .. } if dst == r && a == r))
                .collect();
            // The init: the LAST pre-loop LoadConst of a string literal into
            // `r`. Any OTHER pre-loop touch of `r` (including an earlier init)
            // fails the scan below, so "last" is also "only".
            let init_ip = (0..start).rev().find(|&ip| {
                matches!(f.code[ip], Instr::LoadConst { dst, idx } if dst == r
                    && f.constants.get(idx as usize).copied().map(is_string_const).unwrap_or(false))
            });
            let Some(init_ip) = init_ip else { continue };
            // Jump targets of the whole function — block boundaries for the
            // dead-Move dominating-write scan. Ops outside the enumerated set
            // (PushHandler etc.) are handled by the scan's own barrier.
            let mut targets = vec![false; n];
            for instr in f.code.iter() {
                let t = match *instr {
                    Instr::Jump { target }
                    | Instr::JumpIfFalse { target, .. }
                    | Instr::JumpIfTrue { target, .. }
                    | Instr::JumpIfNotLt { target, .. }
                    | Instr::JumpIfNotLe { target, .. } => target as usize,
                    _ => continue,
                };
                if t < n {
                    targets[t] = true;
                }
            }
            for (ip, instr) in f.code.iter().enumerate() {
                if ip == init_ip || appends.contains(&ip) || ip > end {
                    continue; // the exempt sites; post-loop is unrestricted
                }
                // The discarded statement value of `out += x`: a Move of `r`
                // into a register no read can ever observe (the allocator
                // reuses these slots as scratch, so this is a dominating-write
                // proof, not a "never read" one).
                if ip >= start {
                    if let Instr::Move { dst, src } = f.code[ip] {
                        if src == r && dst != r && move_dst_unobservable(&f.code, &targets, dst, ip)
                        {
                            continue;
                        }
                    }
                }
                if accum_touches(&f.code[ip], r) {
                    continue 'acc;
                }
            }
            rewrites.extend(appends);
        }
    }
    for k in rewrites {
        if let Instr::Add { dst, a, b } = f.code[k] {
            f.code[k] = Instr::StrAppendInPlace { dst, a, b };
        }
    }
}

/// Rewrite a string-accumulator loop's `g = g + b` so it JITs. Always emits
/// `StrConcat` (a JIT routing hint, semantically identical to `Add`) when `g` is
/// initialised to a string literal; UPGRADES to `StrAppendInPlace` (mutates the
/// buffer, no per-element allocation) when `loop_inplace_safe` PROVES the
/// accumulator is never aliased. Numeric accumulators (`sum += x`) keep `Add` and
/// stay on the faster integer region. `is_top_level` is true for the script body.
pub(crate) fn rewrite_string_accumulators(f: &mut FuncProto, is_top_level: bool) {
    let n = f.code.len();
    // (ip, in_place)
    let mut rewrites: Vec<(usize, bool)> = Vec::new();
    for j in 0..n {
        let start = match f.code[j] {
            Instr::Jump { target } if (target as usize) < j => target as usize,
            _ => continue,
        };
        for k in start..=j {
            let (dst, a) = match f.code[k] {
                Instr::Add { dst, a, .. }
                | Instr::AddRightPair { dst, a, in_place: false, .. } => (dst, a),
                _ => continue,
            };
            // result `dst` stored back to some global `g` in the body
            let store_ip = (start..=j).find(|&m| {
                matches!(f.code[m], Instr::StoreGlobal { src, .. } | Instr::StoreGlobalStrict { src, .. } | Instr::StoreGlobalResolved { src, .. } if src == dst)
            });
            let (g, store_ip) = match store_ip {
                Some(m) => match f.code[m] {
                    Instr::StoreGlobal { idx, .. }
                    | Instr::StoreGlobalStrict { idx, .. }
                    | Instr::StoreGlobalResolved { idx, .. } => (idx, m),
                    _ => continue,
                },
                None => continue,
            };
            // operand `a` loaded from that same global `g` in the body
            let load_ip = (start..=j).find(|&m| {
                matches!(f.code[m], Instr::LoadGlobal { dst: ld, idx } if ld == a && idx == g)
            });
            let load_ip = match load_ip {
                Some(m) => m,
                None => continue,
            };
            if !global_inits_string(&f.code, &f.constants, g, start) {
                continue;
            }
            let in_place =
                loop_inplace_safe(&f.code, g, start, j, is_top_level, load_ip, k, store_ip, a, dst);
            rewrites.push((k, in_place));
        }
    }
    for (k, in_place) in rewrites {
        match f.code[k] {
            Instr::Add { dst, a, b } => {
                f.code[k] = if in_place {
                    Instr::StrAppendInPlace { dst, a, b }
                } else {
                    Instr::StrConcat { dst, a, b }
                };
            }
            Instr::AddRightPair { dst, a, b, c, in_place: false } if in_place => {
                f.code[k] = Instr::AddRightPair { dst, a, b, c, in_place: true };
            }
            Instr::AddRightPair { .. } => {
                // The generic fused op is already admitted by MEM/Tier C; no
                // StrConcat routing hint is needed when the alias proof declines.
            }
            _ => {}
        }
    }
}


/// The `type` import attribute of an import/export-from declaration's
/// `with { ... }` clause, if present.
///
/// NOTE: `ast::ImportDecl`/`ExportDecl` hold the attributes as a plain list, so
/// this takes the list rather than an optional clause node — there is no
/// `WithClause` wrapper to unwrap, and an absent clause is an empty slice. The
/// `with` vs (legacy) `assert` keyword is not recorded by `ImportAttribute`,
/// which changes nothing: this function ignored it before too.
///
/// A key or value holding a lone surrogate can never equal `"type"` and is never
/// a module type, so the lossy view is exact for both comparisons here.
pub(crate) fn with_clause_type(attrs: &[ast::ImportAttribute]) -> Option<String> {
    for a in attrs {
        if a.key.to_lossy_string() == "type" {
            return Some(a.value.to_lossy_string());
        }
    }
    None
}
