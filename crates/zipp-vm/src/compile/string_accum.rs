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
        if !is_simple_loop_op(&code[ip]) {
            return false;
        }
        if ip != load_ip && ip != k && ip != store_ip && (instr_touches(&code[ip], a) || instr_touches(&code[ip], dst)) {
            return false;
        }
    }
    true
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
                Instr::Add { dst, a, .. } => (dst, a),
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
        if let Instr::Add { dst, a, b } = f.code[k] {
            f.code[k] = if in_place {
                Instr::StrAppendInPlace { dst, a, b }
            } else {
                Instr::StrConcat { dst, a, b }
            };
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
