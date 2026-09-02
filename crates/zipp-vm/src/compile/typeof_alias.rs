//! The `typeof` ALIAS lane: `var t = typeof v; … if (t === "number") …`
//! reaches the fused `TypeOfIs` classifier even though the `typeof` result is
//! parked in a local first.
//!
//! The direct fusion (`typeof v === "lit"` → `TypeOfIs`, `exprs.rs`) needs the
//! `typeof` textually inside the comparison. A common real-world shape
//! classifies once and tests the name several times — `bench/real/json-large.js`'s
//! `walk` does exactly that — and every one of its `t === "number"` compiled
//! to `LoadConst` + a heap-string `Eq` (both operands non-interned strings, so
//! the `strict_eq` helper and a content compare), while the `TypeOf` paid the
//! classifier helper and the interned-name lookup.
//!
//! This lane records, at a DECLARATION statement `var|let|const t = typeof v`
//! whose two names are both plain register locals (no cell, no `with`
//! shadow, and in sloppy code no parameter — a mapped `arguments` object can
//! write those), the fact "t holds the typeof of v". While the fact holds,
//! `t === "lit"` (and `!==`/`==`/`!=`) compiles to `TypeOfIs { a: v, code }`,
//! which re-classifies `v` — identical to comparing the stored name as long
//! as `v` still holds the value it held at the declaration.
//!
//! A fact is a linear, compile-order fact made sound by three rules:
//!
//!  * KILL ON WRITE — every emitted instruction that may write `t` or `v`
//!    drops the fact (`typeof_alias_kills`; an op it does not enumerate is
//!    assumed to write everything). Plain locals are private to the frame: a
//!    closure or a direct eval would have boxed them into cells, which the
//!    record step refuses, so no write reaches them but this function's own
//!    instructions — and `emit` is the single door those go through.
//!  * SCOPE — a fact recorded inside a nested statement is dropped when that
//!    statement ends (a def inside one `if` arm does not dominate the code
//!    after the `if`). Within one statement list a later sibling is reached
//!    only through the declaration, so the fact survives to its siblings.
//!  * RE-ENTRY — entering a loop, `switch` or `try` clears every fact (a
//!    write later in a loop body reaches an earlier use on the next
//!    iteration; a `case` can be entered without running an earlier case's
//!    declaration, so each case body also starts clean; a handler runs from
//!    any point of its body), and a `for` head's init facts are cleared
//!    before its test/body/update, which run again without it.
//!
//! When every read of `t` became a `TypeOfIs` over `v`, the `TypeOf` that
//! produced `t` is dead: `typeof_alias_finish` rewrites it to `LoadUndefined`
//! (same register; no instruction in the function reads it by the bytecode
//! read table, and it is never a parameter, rest or `arguments` register).
//!
//! `ZIPP_NO_TYPEOF_ALIAS=1` restores the unfused bytecode bit-for-bit.
#![allow(unused_imports)]
use super::string_accum::accum_may_read;
use super::*;
use crate::parse::ast;

/// `ZIPP_NO_TYPEOF_ALIAS=1` disables the lane (compiler-side; read once).
#[inline]
pub(crate) fn typeof_alias_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_TYPEOF_ALIAS").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// One live fact: register `t` holds `typeof` of register `v`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypeofAlias {
    pub(crate) t: Reg,
    pub(crate) v: Reg,
    /// `typeof_alias_depth` inside the declaration's own `stmt` frame. The
    /// fact is dropped once the frame ENCLOSING that one ends — see
    /// `typeof_alias_stmt_exit`.
    pub(crate) depth: u32,
}

/// May emitting `i` write register `r`? The alias kill test. Every variant
/// enumerated here writes exactly its listed destination(s) or nothing; a
/// variant not enumerated is assumed to write ANY register (`true`), so an
/// unknown or future op can only lose the optimisation, never keep a stale
/// fact. Call-shaped ops write only `dst`: the callee's frame starts at the
/// argument window, which the compiler allocates above every live local.
pub(crate) fn typeof_alias_kills(i: &Instr, r: Reg) -> bool {
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
        | Instr::TypeOfSame { dst, .. }
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
        | Instr::CallWithThis { dst, .. }
        | Instr::RegExpMethod { dst, .. }
        | Instr::CallMethod { dst, .. }
        | Instr::MathOp { dst, .. }
        | Instr::StaticFn { dst, .. }
        | Instr::ToConcatKey { dst, .. }
        | Instr::AddRightPair { dst, .. }
        | Instr::Pad2Concat { dst, .. }
        | Instr::Pad2Conditional { dst, .. } => dst == r,
        Instr::StrAppendIndex { dst, scratch, .. } => dst == r || scratch == r,
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
        _ => true,
    }
}

/// Statements whose body can run again, or be entered at a point other than
/// its start, without the declarations above it running first.
fn typeof_alias_stmt_reenters(s: &ast::Stmt) -> bool {
    matches!(
        s,
        ast::Stmt::While { .. }
            | ast::Stmt::DoWhile { .. }
            | ast::Stmt::For { .. }
            | ast::Stmt::ForIn { .. }
            | ast::Stmt::ForOf { .. }
            | ast::Stmt::Switch { .. }
            | ast::Stmt::Try { .. }
    )
}

impl<'a> FnCompiler<'a> {
    /// `stmt` entry: one more frame; a re-entering statement starts clean.
    pub(crate) fn typeof_alias_stmt_enter(&mut self, s: &ast::Stmt) {
        self.typeof_alias_depth += 1;
        if !self.typeof_alias.is_empty() && typeof_alias_stmt_reenters(s) {
            self.typeof_alias.clear();
        }
    }

    /// `stmt` exit: a fact recorded at depth `d` (inside its declaration's
    /// frame) stays valid for the siblings of that declaration — compiled at
    /// the same depth `d` — and dies when the frame enclosing them ends, i.e.
    /// once the depth falls below `d - 1`.
    pub(crate) fn typeof_alias_stmt_exit(&mut self) {
        self.typeof_alias_depth = self.typeof_alias_depth.saturating_sub(1);
        if !self.typeof_alias.is_empty() {
            let keep = self.typeof_alias_depth + 1;
            self.typeof_alias.retain(|a| a.depth <= keep);
        }
    }

    /// Drop every fact (a `for` head after its init; each `switch` case body).
    pub(crate) fn typeof_alias_clear(&mut self) {
        self.typeof_alias.clear();
    }

    /// `emit` hook: an instruction that may write either register of a fact
    /// kills it. Only called while facts are live, so the cost is nil.
    pub(crate) fn typeof_alias_note_emit(&mut self, i: &Instr) {
        self.typeof_alias
            .retain(|a| !(typeof_alias_kills(i, a.t) || typeof_alias_kills(i, a.v)));
    }

    /// A declaration `name = init` has just been compiled into register
    /// `reg`. Record the fact when `init` is `typeof <local>` and the lowering
    /// was exactly `TypeOf { dst: reg, a: v }` (the last emitted instruction).
    pub(crate) fn typeof_alias_record(&mut self, name: &str, init: &ast::Expr, reg: Reg) {
        if !typeof_alias_enabled() || self.typeof_alias_depth == 0 {
            return;
        }
        let ast::Expr::Unary {
            op: ast::UnaryOp::Typeof,
            arg,
        } = init
        else {
            return;
        };
        let ast::Expr::Ident(vn) = &**arg else {
            return;
        };
        let vn: &str = vn;
        if vn == "arguments" || name == "arguments" {
            return;
        }
        if !self.with_objs_for(vn).is_empty() || !self.with_objs_for(name).is_empty() {
            return;
        }
        let Binding::Local(v) = self.resolve(vn) else {
            return;
        };
        if v == reg || self.cell_regs.contains(&reg) || self.cell_regs.contains(&v) {
            return;
        }
        if self.typeof_alias_mapped_arguments_hazard(reg, v) {
            return;
        }
        let Some(ip) = self.code.len().checked_sub(1) else {
            return;
        };
        if !matches!(self.code[ip], Instr::TypeOf { dst, a } if dst == reg && a == v) {
            return;
        }
        self.typeof_alias.push(TypeofAlias {
            t: reg,
            v,
            depth: self.typeof_alias_depth,
        });
        self.typeof_alias_defs.push((ip as u32, reg));
    }

    /// `this`, the fixed parameters, the rest parameter and the `arguments`
    /// register — the slots an `arguments` object may alias or that the
    /// call path fills.
    fn is_param_reg(&self, r: Reg) -> bool {
        r <= self.param_names.len() as Reg
            || Some(r) == self.rest_reg
            || Some(r) == self.arguments_reg
    }

    /// Sloppy code: a simple-parameter function's `arguments` object is
    /// MAPPED — `arguments[0] = x` writes the parameter's register without an
    /// instruction of this function doing so. That object exists only when
    /// the function mentions `arguments`: `uses_arguments` is set at entry for
    /// a nested arrow's reference (and a possible direct eval boxes every
    /// local, which the cell test refuses), and by `resolve` for each direct
    /// reference as it is compiled. Checked at the declaration AND at every
    /// use, so a reference textually between them is seen; a reference after
    /// the use runs after it (facts never survive a loop or a `switch` case).
    fn typeof_alias_mapped_arguments_hazard(&self, t: Reg, v: Reg) -> bool {
        !self.cx.in_strict && self.uses_arguments && (self.is_param_reg(t) || self.is_param_reg(v))
    }

    /// The register whose `typeof` local `name` holds, if a fact for it is
    /// live (and `name` still resolves to that plain local, unshadowed).
    pub(crate) fn typeof_alias_lookup(&mut self, name: &str) -> Option<Reg> {
        if self.typeof_alias.is_empty() || !typeof_alias_enabled() {
            return None;
        }
        if !self.with_objs_for(name).is_empty() {
            return None;
        }
        let Binding::Local(t) = self.resolve(name) else {
            return None;
        };
        let v = self.typeof_alias.iter().rev().find(|a| a.t == t).map(|a| a.v)?;
        if self.typeof_alias_mapped_arguments_hazard(t, v) {
            return None;
        }
        Some(v)
    }

    /// Function end: a recorded `TypeOf` whose destination no instruction
    /// reads any more is dead — every consumer became a `TypeOfIs` over the
    /// operand — and becomes a `LoadUndefined` of the same register (the
    /// register stays written, so nothing downstream sees a new shape).
    /// `accum_may_read` is conservative (an unenumerated op reads everything),
    /// and parameter-class registers are never touched (`arguments`).
    pub(crate) fn typeof_alias_finish(&mut self) {
        if self.typeof_alias_defs.is_empty() {
            return;
        }
        let defs = std::mem::take(&mut self.typeof_alias_defs);
        for (ip, t) in defs {
            if self.is_param_reg(t) {
                continue;
            }
            let Some(Instr::TypeOf { dst, .. }) = self.code.get(ip as usize) else {
                continue;
            };
            if *dst != t {
                continue;
            }
            if self.code.iter().any(|i| accum_may_read(i, t)) {
                continue;
            }
            self.code[ip as usize] = Instr::LoadUndefined { dst: t };
        }
    }
}
