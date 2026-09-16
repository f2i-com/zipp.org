//! Expression lowering for the Python emitter.
use super::emitter::{Emitter, LoopCtx, R};
use super::stmts::{binop_name, inplace_binop_name};
use super::symtable::{ScopeKind, SymKind};
use crate::bytecode::{Instr, Reg};
use rustpython_parser::ast;
use std::collections::{BTreeMap, BTreeSet};

/// Operator and comparison chains longer than this reuse their registers.
const LONG_CHAIN: usize = 16;
/// Display and argument lists longer than this are built in chunks of this
/// many elements, so a large literal needs a bounded number of registers.
const CHUNK: usize = 256;

/// What an operand is known to be from its syntax alone, so a fast path the
/// operand can never take is not emitted (and a guard it always passes is
/// skipped).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Known {
    /// An int literal: a BigInt.
    Int,
    /// A float literal: a JS number.
    Float,
    /// A str literal.
    Str,
    /// `None`, `True` or `False`: identity is value equality.
    Singleton,
    Unknown,
}

pub(super) fn known(e: &ast::Expr) -> Known {
    match e {
        ast::Expr::Constant(c) => match &c.value {
            ast::Constant::Int(_) => Known::Int,
            ast::Constant::Float(_) => Known::Float,
            ast::Constant::Str(_) => Known::Str,
            ast::Constant::None | ast::Constant::Bool(_) => Known::Singleton,
            _ => Known::Unknown,
        },
        // A negated number literal is folded to a constant.
        ast::Expr::UnaryOp(u) if matches!(u.op, ast::UnaryOp::USub) => match known(&u.operand) {
            k @ (Known::Int | Known::Float) => k,
            _ => Known::Unknown,
        },
        _ => Known::Unknown,
    }
}

/// The value of a small int literal (optionally negated), for the
/// immediate forms of `+` and `-`.
pub(super) fn small_int_literal(e: &ast::Expr) -> Option<i32> {
    match e {
        ast::Expr::Constant(c) => match &c.value {
            ast::Constant::Int(i) => i.to_string().parse::<i32>().ok(),
            _ => None,
        },
        ast::Expr::UnaryOp(u) if matches!(u.op, ast::UnaryOp::USub) => {
            small_int_literal(&u.operand).and_then(i32::checked_neg)
        }
        _ => None,
    }
}

impl<'a> Emitter<'a> {
    pub fn expr(&mut self, expr: &ast::Expr, depth: usize) -> R<Reg> {
        self.depth(expr, depth)?;
        match expr {
            ast::Expr::Name(n) => self.load_name(n.id.as_str()),
            ast::Expr::Constant(c) => self.constant(expr, &c.value),
            ast::Expr::BinOp(b) => self.binop_chain(b, depth),
            ast::Expr::UnaryOp(u) => {
                // `-<number literal>` is a constant, as CPython folds it.
                if let (ast::UnaryOp::USub, ast::Expr::Constant(c)) = (u.op, u.operand.as_ref()) {
                    match &c.value {
                        ast::Constant::Int(i) => return self.integer(&format!("-{i}")),
                        ast::Constant::Float(f) => return self.float(-*f),
                        _ => {}
                    }
                }
                let value = self.expr(&u.operand, depth + 1)?;
                match u.op {
                    ast::UnaryOp::Not => {
                        let t = self.truth(value)?;
                        let dst = self.alloc()?;
                        self.emit(Instr::Not { dst, a: t })?;
                        Ok(dst)
                    }
                    ast::UnaryOp::USub => self.unary(value, "neg"),
                    ast::UnaryOp::UAdd => {
                        let op = self.string("pos")?;
                        self.helper("unop", &[op, value])
                    }
                    ast::UnaryOp::Invert => self.unary(value, "invert"),
                }
            }
            ast::Expr::BoolOp(b) => {
                let dst = self.alloc()?;
                let mut jumps = Vec::new();
                for (i, value) in b.values.iter().enumerate() {
                    // Only `dst` outlives an operand.
                    let mark = self.mark();
                    // Operands after the first may not be evaluated.
                    if i > 0 {
                        self.speculative += 1;
                    }
                    let value = self.expr(value, depth + 1);
                    if i > 0 {
                        self.speculative -= 1;
                    }
                    let value = value?;
                    self.emit(Instr::Move { dst, src: value })?;
                    if i + 1 != b.values.len() {
                        let cond = self.truth(value)?;
                        let jump = match b.op {
                            ast::BoolOp::And => self.jump_if_false(cond)?,
                            ast::BoolOp::Or => self.jump_if_true(cond)?,
                        };
                        jumps.push(jump);
                    }
                    self.release(mark);
                }
                let end = self.here();
                for at in jumps {
                    self.patch(at, end)?;
                }
                Ok(dst)
            }
            ast::Expr::Compare(c) => {
                let dst = self.boolean(true)?;
                let mut jumps = Vec::new();
                let mut left = self.expr(&c.left, depth + 1)?;
                // A long chain carries the shared operand in one register and
                // reclaims each link's temporaries.
                let hold = if c.ops.len() > LONG_CHAIN {
                    let hold = self.alloc()?;
                    self.emit(Instr::Move { dst: hold, src: left })?;
                    left = hold;
                    Some(hold)
                } else {
                    None
                };
                let mut left_kind = known(&c.left);
                for (i, (op, right)) in c.ops.iter().zip(c.comparators.iter()).enumerate() {
                    let mark = self.mark();
                    if i > 0 {
                        self.speculative += 1;
                    }
                    let right_kind = known(right);
                    let right = self.expr(right, depth + 1);
                    if i > 0 {
                        self.speculative -= 1;
                    }
                    let right = right?;
                    let value = self.compare_once(op, left, left_kind, right, right_kind)?;
                    left_kind = right_kind;
                    self.emit(Instr::Move { dst, src: value })?;
                    if c.ops.len() > 1 {
                        let cond = self.truth(value)?;
                        jumps.push(self.jump_if_false(cond)?);
                    }
                    match hold {
                        Some(hold) => {
                            self.emit(Instr::Move { dst: hold, src: right })?;
                            self.release(mark);
                        }
                        None => left = right,
                    }
                }
                let end = self.here();
                for at in jumps {
                    self.patch(at, end)?;
                }
                Ok(dst)
            }
            ast::Expr::Call(c) => self.call(expr, c, depth + 1),
            ast::Expr::List(l) => {
                let arr = self.sequence_array(&l.elts, depth + 1)?;
                self.helper("list", &[arr])
            }
            ast::Expr::Tuple(t) => {
                let arr = self.sequence_array(&t.elts, depth + 1)?;
                self.helper("tuple", &[arr])
            }
            ast::Expr::Set(s) => {
                let arr = self.sequence_array(&s.elts, depth + 1)?;
                self.helper("set", &[arr])
            }
            ast::Expr::Dict(d) => {
                // `dictfill` and `dictmerge` update the dict in place, so
                // `result` stays put and each batch's registers are reclaimed.
                let result = self.helper("dict", &[])?;
                let mut keys = Vec::new();
                let mut values = Vec::new();
                let mark = self.mark();
                for (k, v) in d.keys.iter().zip(d.values.iter()) {
                    match k {
                        Some(k) => {
                            keys.push(self.expr(k, depth + 1)?);
                            values.push(self.expr(v, depth + 1)?);
                            if keys.len() == CHUNK {
                                let ka = self.array(&keys)?;
                                let va = self.array(&values)?;
                                self.helper("dictfill", &[result, ka, va])?;
                                keys.clear();
                                values.clear();
                                self.release(mark);
                            }
                        }
                        None => {
                            if !keys.is_empty() {
                                let ka = self.array(&keys)?;
                                let va = self.array(&values)?;
                                self.helper("dictfill", &[result, ka, va])?;
                                keys.clear();
                                values.clear();
                            }
                            let mapping = self.expr(v, depth + 1)?;
                            self.helper("dictmerge", &[result, mapping])?;
                            self.release(mark);
                        }
                    }
                }
                if !keys.is_empty() {
                    let ka = self.array(&keys)?;
                    let va = self.array(&values)?;
                    self.helper("dictfill", &[result, ka, va])?;
                }
                Ok(result)
            }
            ast::Expr::Subscript(s) => {
                let value = self.expr(&s.value, depth + 1)?;
                let index = self.expr(&s.slice, depth + 1)?;
                self.helper("getitem", &[value, index])
            }
            ast::Expr::Slice(s) => {
                let lo = match &s.lower {
                    Some(e) => self.expr(e, depth + 1)?,
                    None => self.none()?,
                };
                let hi = match &s.upper {
                    Some(e) => self.expr(e, depth + 1)?,
                    None => self.none()?,
                };
                let step = match &s.step {
                    Some(e) => self.expr(e, depth + 1)?,
                    None => self.none()?,
                };
                self.helper("slice", &[lo, hi, step])
            }
            ast::Expr::Attribute(a) => {
                let value = self.expr(&a.value, depth + 1)?;
                let name = self.string(a.attr.as_str())?;
                self.helper("getattr", &[value, name])
            }
            ast::Expr::IfExp(e) => {
                let otherwise = self.branch(&e.test, false, depth + 1)?;
                let dst = self.alloc()?;
                self.speculative += 1;
                let yes = self.expr(&e.body, depth + 1);
                self.speculative -= 1;
                let yes = yes?;
                self.emit(Instr::Move { dst, src: yes })?;
                let end = self.jump()?;
                let here = self.here();
                for j in otherwise {
                    self.patch(j, here)?;
                }
                self.speculative += 1;
                let no = self.expr(&e.orelse, depth + 1);
                self.speculative -= 1;
                let no = no?;
                self.emit(Instr::Move { dst, src: no })?;
                let here = self.here();
                self.patch(end, here)?;
                Ok(dst)
            }
            ast::Expr::NamedExpr(n) => {
                let value = self.expr(&n.value, depth + 1)?;
                let ast::Expr::Name(target) = n.target.as_ref() else {
                    return Err(self.error(expr, "walrus target must be a name"));
                };
                self.store_name(target.id.as_str(), value)?;
                Ok(value)
            }
            ast::Expr::Lambda(l) => {
                self.validate_args(expr, &l.args)?;
                let (defaults, kw_names, kw_values) = self.defaults_of(&l.args)?;
                let qualname = if self.kind() == ScopeKind::Module {
                    "<lambda>".to_owned()
                } else {
                    format!("{}.<locals>.<lambda>", self.qualname)
                };
                let body = l.body.as_ref();
                let simple = Emitter::simple_arity(&l.args);
                let (func_id, child) =
                    self.compile_child(expr, "<lambda>", qualname.clone(), |e| {
                        if let Some(count) = simple {
                            e.arity_guard(count)?;
                        }
                        let value = e.expr(body, depth + 1)?;
                        e.emit(Instr::Return { src: value })?;
                        Ok(())
                    })?;
                let none = self.none()?;
                self.make_function(
                    func_id,
                    child,
                    "<lambda>",
                    &qualname,
                    Some(&l.args),
                    defaults,
                    kw_names,
                    kw_values,
                    none,
                )
            }
            ast::Expr::ListComp(c) => {
                self.comprehension(expr, "<listcomp>", &c.generators, &[&c.elt], depth + 1)
            }
            ast::Expr::SetComp(c) => {
                self.comprehension(expr, "<setcomp>", &c.generators, &[&c.elt], depth + 1)
            }
            ast::Expr::DictComp(c) => self.comprehension(
                expr,
                "<dictcomp>",
                &c.generators,
                &[&c.key, &c.value],
                depth + 1,
            ),
            ast::Expr::GeneratorExp(c) => {
                self.comprehension(expr, "<genexpr>", &c.generators, &[&c.elt], depth + 1)
            }
            ast::Expr::JoinedStr(j) => self.fstring(&j.values, depth + 1),
            ast::Expr::FormattedValue(f) => {
                let part = self.fvalue(f, depth + 1)?;
                let arr = self.array(&[part])?;
                self.helper("strjoin", &[arr])
            }
            ast::Expr::Yield(y) => self.yield_value(expr, y, depth, true),
            ast::Expr::YieldFrom(y) => {
                if !self.proto.is_generator {
                    return Err(self.error(expr, "'yield from' outside function"));
                }
                let value = self.expr(&y.value, depth + 1)?;
                let iter = self.helper("iter", &[value])?;
                self.yield_from(expr, iter)
            }
            ast::Expr::Starred(_) => Err(self.error(
                expr,
                "starred expression is only valid in calls, literals and assignment",
            )),
            ast::Expr::Await(_) => Err(self.error(expr, "'await' is not supported")),
        }
    }

    /// `yield value`. When the expression's value is `used`, a resume by
    /// `next()` (which sends no value) reads as None.
    pub fn yield_value(&mut self, node: &ast::Expr, y: &ast::ExprYield, depth: usize, used: bool) -> R<Reg> {
        if !self.proto.is_generator {
            return Err(self.error(node, "'yield' outside function"));
        }
        let value = match &y.value {
            Some(v) => self.expr(v, depth + 1)?,
            None => self.none()?,
        };
        let dst = self.alloc()?;
        self.emit(Instr::Yield { dst, val: value })?;
        self.forget_line();
        if used {
            let sent = self.typeof_is(dst, "undefined")?;
            let skip = self.jump_if_false(sent)?;
            self.emit(Instr::LoadNull { dst })?;
            let here = self.here();
            self.patch(skip, here)?;
        }
        Ok(dst)
    }

    /// `yield from iter` (PEP 380): every value the subiterator produces is
    /// yielded, and whatever the delegating generator is resumed with goes
    /// back to the subiterator: a sent value through its `send`, a thrown
    /// exception through its `throw` (a GeneratorExit closes it instead), and
    /// a `close()` of the delegating generator (a return completion through
    /// the Yield) closes it. The runtime's `yfstep` performs one step and
    /// returns STOP with the subiterator's return value in `yfret`.
    fn yield_from(&mut self, node: &ast::Expr, iter: Reg) -> R<Reg> {
        let sent = self.none()?;
        let mode = self.small_int(0)?;
        let one = self.small_int(1)?;
        let (kind_reg, val_reg) = (self.alloc()?, self.alloc()?);
        let ereg = self.alloc()?;
        let stop = self.stop()?;
        let head = self.here();
        // The step can raise from the subiterator: name this line.
        self.forget_line();
        self.stamp_line(node)?;
        let item = self.helper("yfstep", &[iter, mode, sent])?;
        let done = self.alloc()?;
        self.emit(Instr::Eq {
            dst: done,
            a: item,
            b: stop,
        })?;
        let exit = self.jump_if_true(done)?;
        let fin = self.emit(Instr::PushFinally {
            target: 0,
            kind_reg,
            val_reg,
        })?;
        let catch = self.emit(Instr::PushHandler {
            catch_target: 0,
            catch_reg: ereg,
        })?;
        self.emit(Instr::Yield {
            dst: sent,
            val: item,
        })?;
        self.emit(Instr::PopHandler)?;
        self.emit(Instr::LoadInt {
            dst: kind_reg,
            val: 0,
        })?;
        self.emit(Instr::PopFinally)?;
        self.emit(Instr::LoadInt { dst: mode, val: 0 })?;
        self.emit(Instr::Jump { target: head })?;
        // Thrown in: forward the exception on the next step.
        let here = self.here();
        self.patch(catch, here)?;
        self.emit(Instr::LoadInt {
            dst: kind_reg,
            val: 0,
        })?;
        self.emit(Instr::PopFinally)?;
        self.emit(Instr::Move {
            dst: sent,
            src: ereg,
        })?;
        self.emit(Instr::LoadInt { dst: mode, val: 1 })?;
        self.emit(Instr::Jump { target: head })?;
        // Only a return completion (`close()`) reaches the finally.
        let here = self.here();
        self.patch(fin, here)?;
        let is_return = self.alloc()?;
        self.emit(Instr::Eq {
            dst: is_return,
            a: kind_reg,
            b: one,
        })?;
        let skip = self.jump_if_false(is_return)?;
        self.helper("yfclose", &[iter])?;
        let here = self.here();
        self.patch(skip, here)?;
        self.emit(Instr::EndFinally { kind_reg, val_reg })?;
        let here = self.here();
        self.patch(exit, here)?;
        self.forget_line();
        // Read straight after the step that returned STOP.
        self.prop(self.r_rt, "yfret")
    }

    /// `a op b op c ...`: a left-deep chain is lowered iteratively, leftmost
    /// operand first, every operand at the chain's own depth, so a flat
    /// chain's length costs neither native stack nor nesting budget. A long
    /// chain keeps its running value in one register and reclaims each
    /// link's temporaries.
    fn binop_chain(&mut self, top: &ast::ExprBinOp, depth: usize) -> R<Reg> {
        let mut links = vec![top];
        let mut left = top.left.as_ref();
        while let ast::Expr::BinOp(inner) = left {
            links.push(inner);
            left = inner.left.as_ref();
        }
        let first = self.expr(left, depth + 1)?;
        let mut kind = known(left);
        if links.len() <= LONG_CHAIN {
            let mut acc = first;
            for link in links.iter().rev() {
                acc = self.arith_operand(&link.op, acc, kind, &link.right, depth + 1, false)?;
                kind = Known::Unknown;
            }
            return Ok(acc);
        }
        let acc = self.alloc()?;
        self.emit(Instr::Move { dst: acc, src: first })?;
        for link in links.iter().rev() {
            let mark = self.mark();
            let value = self.arith_operand(&link.op, acc, kind, &link.right, depth + 1, false)?;
            kind = Known::Unknown;
            self.emit(Instr::Move { dst: acc, src: value })?;
            self.release(mark);
        }
        Ok(acc)
    }

    /// `a op <right>` with `right` still to evaluate: `+`/`-` of a small int
    /// literal takes the immediate form, which needs no register for it.
    /// Zero keeps the general form: `-0.0 - 0` is `-0.0` in Python, but the
    /// immediate form would add `+0` and give `0.0`.
    pub fn arith_operand(
        &mut self,
        op: &ast::Operator,
        a: Reg,
        ka: Known,
        right: &ast::Expr,
        depth: usize,
        inplace: bool,
    ) -> R<Reg> {
        if self.fast && ka != Known::Str {
            if let Some(k) = small_int_literal(right).filter(|k| *k != 0) {
                let imm = match op {
                    ast::Operator::Add => Some(k),
                    ast::Operator::Sub => k.checked_neg(),
                    _ => None,
                };
                if let Some(imm) = imm {
                    self.depth(right, depth)?;
                    return self.arith_imm(op, a, right, imm, inplace);
                }
            }
        }
        let b = self.expr(right, depth)?;
        self.arith_known(op, a, ka, b, known(right), inplace)
    }

    /// `a + k` / `a - k` for a small int literal `k`: one `AddInt` under an
    /// int-or-float guard. Its update form keeps a BigInt a BigInt and adds
    /// to a float as a float, exactly Python's int and float `+`; anything
    /// else (a bool, an object) loads the literal and asks the runtime.
    fn arith_imm(&mut self, op: &ast::Operator, a: Reg, literal: &ast::Expr, imm: i32, inplace: bool) -> R<Reg> {
        let dst = self.alloc()?;
        let is_int = self.typeof_is(a, "bigint")?;
        let add = self.jump_if_true(is_int)?;
        let is_float = self.typeof_is(a, "number")?;
        let slow = self.jump_if_false(is_float)?;
        let here = self.here();
        self.patch(add, here)?;
        self.emit(Instr::AddInt { dst, a, imm, upd: true })?;
        let end = self.jump()?;
        let here = self.here();
        self.patch(slow, here)?;
        let b = self.expr(literal, 0)?;
        let name = self.string(binop_name(op))?;
        let r = self.helper(if inplace { "iop" } else { "binop" }, &[name, a, b])?;
        self.emit(Instr::Move { dst, src: r })?;
        let here = self.here();
        self.patch(end, here)?;
        Ok(dst)
    }

    /// Jumps taken unless every `(value, typeof name)` pair holds.
    fn type_guard(&mut self, checks: &[(Reg, &str)]) -> R<Vec<usize>> {
        let mut fails = Vec::with_capacity(checks.len());
        for (value, name) in checks {
            let ok = self.typeof_is(*value, name)?;
            fails.push(self.jump_if_false(ok)?);
        }
        Ok(fails)
    }

    /// The fast-path tiers an operand pair can take, cheapest guard first:
    /// both ints (BigInts), both floats (JS numbers), both strs. A tier
    /// either operand's literal type rules out is not emitted, and a check a
    /// literal always passes is skipped.
    fn tiers(ka: Known, kb: Known, int: bool, float: bool, str: bool) -> Vec<&'static str> {
        let fits = |k: Known, t: Known| k == Known::Unknown || k == t;
        let mut out = Vec::new();
        if int && fits(ka, Known::Int) && fits(kb, Known::Int) {
            out.push("bigint");
        }
        if float && fits(ka, Known::Float) && fits(kb, Known::Float) {
            out.push("number");
        }
        if str && fits(ka, Known::Str) && fits(kb, Known::Str) {
            out.push("string");
        }
        out
    }

    /// Guard one tier: jumps to the next tier when `a` fails, and straight to
    /// the helper (returned separately) when `a` passes but `b` does not.
    fn tier_guard(&mut self, a: Reg, ka: Known, b: Reg, kb: Known, tier: &str) -> R<(Vec<usize>, Vec<usize>)> {
        let mut next = Vec::new();
        let mut slow = Vec::new();
        if ka == Known::Unknown {
            next = self.type_guard(&[(a, tier)])?;
        }
        if kb == Known::Unknown {
            slow = self.type_guard(&[(b, tier)])?;
        }
        Ok((next, slow))
    }

    /// `a op b` (or the in-place form). Int op int runs as VM instructions
    /// (JS BigInt arithmetic agrees with Python for `+ - * & | ^`; `//` and
    /// `%` are corrected from truncation to floor semantics inline); float op
    /// float for `+ - *` and `/` (behind a nonzero divisor) is the VM's IEEE
    /// arithmetic, which is Python's, signed zeros and NaN included; str + str
    /// concatenates below the runtime's text limit. Everything else, mixed
    /// int/float operands included, goes through the runtime's dispatch.
    pub fn arith_known(&mut self, op: &ast::Operator, a: Reg, ka: Known, b: Reg, kb: Known, inplace: bool) -> R<Reg> {
        let dst = self.alloc()?;
        let mut slow_jumps = Vec::new();
        let mut end_jumps = Vec::new();
        let (int, float, str) = match op {
            ast::Operator::Add => (true, true, true),
            ast::Operator::Sub | ast::Operator::Mult => (true, true, false),
            ast::Operator::Div => (false, true, false),
            ast::Operator::Mod
            | ast::Operator::FloorDiv
            | ast::Operator::BitAnd
            | ast::Operator::BitOr
            | ast::Operator::BitXor => (true, false, false),
            _ => (false, false, false),
        };
        let tiers = if self.fast {
            Self::tiers(ka, kb, int, float, str)
        } else {
            Vec::new()
        };
        let mut next: Vec<usize> = Vec::new();
        for tier in tiers {
            let here = self.here();
            for j in next.drain(..) {
                self.patch(j, here)?;
            }
            let (to_next, to_slow) = self.tier_guard(a, ka, b, kb, tier)?;
            next = to_next;
            slow_jumps.extend(to_slow);
            match (tier, op) {
                ("bigint", ast::Operator::Mod | ast::Operator::FloorDiv) => {
                    self.int_floor_op(op, dst, a, b, &mut slow_jumps, &mut end_jumps)?;
                    continue;
                }
                ("number", ast::Operator::Div) => {
                    // Division by either zero raises ZeroDivisionError.
                    let zero = self.float(0.0)?;
                    let is_zero = self.alloc()?;
                    self.emit(Instr::Eq { dst: is_zero, a: b, b: zero })?;
                    slow_jumps.push(self.jump_if_true(is_zero)?);
                    self.emit(Instr::Div { dst, a, b })?;
                }
                ("string", _) => {
                    // Past the runtime's text limit the helper raises MemoryError.
                    self.emit(Instr::Add { dst, a, b })?;
                    let len = self.prop(dst, "length")?;
                    let limit = self.prop(self.r_rt, "MAX_TEXT")?;
                    let ok = self.alloc()?;
                    self.emit(Instr::Le { dst: ok, a: len, b: limit })?;
                    slow_jumps.push(self.jump_if_false(ok)?);
                }
                (_, ast::Operator::Add) => {
                    self.emit(Instr::Add { dst, a, b })?;
                }
                (_, ast::Operator::Sub) => {
                    self.emit(Instr::Sub { dst, a, b })?;
                }
                (_, ast::Operator::Mult) => {
                    self.emit(Instr::Mul { dst, a, b })?;
                }
                (_, ast::Operator::BitAnd | ast::Operator::BitOr | ast::Operator::BitXor) => {
                    use crate::bytecode::BitwiseOp;
                    let op = match op {
                        ast::Operator::BitAnd => BitwiseOp::And,
                        ast::Operator::BitOr => BitwiseOp::Or,
                        _ => BitwiseOp::Xor,
                    };
                    self.emit(Instr::Bitwise { dst, a, b, op })?;
                }
                _ => return Err("Python emitter: no fast path for this operator".into()),
            }
            end_jumps.push(self.jump()?);
        }
        slow_jumps.extend(next);
        let here = self.here();
        for j in slow_jumps {
            self.patch(j, here)?;
        }
        // One runtime entry point per operator (`R.add`, `R.iadd`, ...), so
        // the primitive cases need no op-name dispatch.
        let helper = if inplace {
            inplace_binop_name(op)
        } else {
            binop_name(op)
        };
        let r = self.helper(helper, &[a, b])?;
        self.emit(Instr::Move { dst, src: r })?;
        let here = self.here();
        for j in end_jumps {
            self.patch(j, here)?;
        }
        Ok(dst)
    }

    /// Int `//` or `%` on two BigInts: the VM's truncating result, corrected
    /// to floor semantics when the remainder is non-zero and the signs
    /// differ. A zero divisor goes to the helper, which raises.
    fn int_floor_op(
        &mut self,
        op: &ast::Operator,
        dst: Reg,
        a: Reg,
        b: Reg,
        slow_jumps: &mut Vec<usize>,
        end_jumps: &mut Vec<usize>,
    ) -> R<()> {
        let zero = self.alloc()?;
        self.emit(Instr::LoadBigInt { dst: zero, value: 0 })?;
        let is_zero = self.alloc()?;
        self.emit(Instr::Eq { dst: is_zero, a: b, b: zero })?;
        slow_jumps.push(self.jump_if_true(is_zero)?);
        let r = self.alloc()?;
        self.emit(Instr::Mod { dst: r, a, b })?;
        if matches!(op, ast::Operator::Mod) {
            self.emit(Instr::Move { dst, src: r })?;
        } else {
            self.emit(Instr::Div { dst, a, b })?;
        }
        let nonzero = self.alloc()?;
        self.emit(Instr::Ne { dst: nonzero, a: r, b: zero })?;
        end_jumps.push(self.jump_if_false(nonzero)?);
        let sa = self.alloc()?;
        self.emit(Instr::Lt { dst: sa, a, b: zero })?;
        let sb = self.alloc()?;
        self.emit(Instr::Lt { dst: sb, a: b, b: zero })?;
        let same = self.alloc()?;
        self.emit(Instr::Eq { dst: same, a: sa, b: sb })?;
        end_jumps.push(self.jump_if_true(same)?);
        if matches!(op, ast::Operator::Mod) {
            self.emit(Instr::Add { dst, a: r, b })?;
        } else {
            let one = self.alloc()?;
            self.emit(Instr::LoadBigInt { dst: one, value: 1 })?;
            self.emit(Instr::Sub { dst, a: dst, b: one })?;
        }
        end_jumps.push(self.jump()?);
        Ok(())
    }

    /// `-x` and `~x`: an int negates or inverts as a BigInt, a float negates
    /// as a JS number (zero becoming -0.0); anything else asks the runtime.
    fn unary(&mut self, value: Reg, op: &str) -> R<Reg> {
        let dst = self.alloc()?;
        let mut end = Vec::new();
        let mut slow = Vec::new();
        if self.fast {
            let is_int = self.typeof_is(value, "bigint")?;
            let not_int = self.jump_if_false(is_int)?;
            if op == "neg" {
                self.emit(Instr::Neg { dst, a: value })?;
            } else {
                self.emit(Instr::BitNot { dst, a: value })?;
            }
            end.push(self.jump()?);
            let here = self.here();
            self.patch(not_int, here)?;
            if op == "neg" {
                slow.extend(self.type_guard(&[(value, "number")])?);
                self.emit(Instr::Neg { dst, a: value })?;
                end.push(self.jump()?);
            }
        }
        let here = self.here();
        for j in slow {
            self.patch(j, here)?;
        }
        let name = self.string(op)?;
        let r = self.helper("unop", &[name, value])?;
        self.emit(Instr::Move { dst, src: r })?;
        let here = self.here();
        for j in end {
            self.patch(j, here)?;
        }
        Ok(dst)
    }

    /// The VM comparison for `op` on two operands of one primitive type.
    fn compare_instr(op: &ast::CmpOp, dst: Reg, a: Reg, b: Reg) -> Option<Instr> {
        Some(match op {
            ast::CmpOp::Lt => Instr::Lt { dst, a, b },
            ast::CmpOp::LtE => Instr::Le { dst, a, b },
            ast::CmpOp::Gt => Instr::Gt { dst, a, b },
            ast::CmpOp::GtE => Instr::Ge { dst, a, b },
            ast::CmpOp::Eq => Instr::Eq { dst, a, b },
            ast::CmpOp::NotEq => Instr::Ne { dst, a, b },
            _ => return None,
        })
    }

    /// The tiers a comparison can run inline: ordering and equality of two
    /// ints or two floats (the VM's comparisons are exact for BigInts and
    /// IEEE for numbers, as Python's are), and equality of two strs. Str
    /// ordering stays in the runtime: JS orders UTF-16 units, Python code
    /// points.
    fn compare_tiers(&self, op: &ast::CmpOp, ka: Known, kb: Known) -> Vec<&'static str> {
        if !self.fast || Self::compare_instr(op, 0, 0, 0).is_none() {
            return Vec::new();
        }
        let equality = matches!(op, ast::CmpOp::Eq | ast::CmpOp::NotEq);
        Self::tiers(ka, kb, true, true, equality)
    }

    /// `a is b` / `a is not b` inline: identity is the VM's strict equality,
    /// except that two floats compare by `Object.is` in the runtime (NaN is
    /// itself, 0.0 is not -0.0), so a float left operand asks the runtime.
    /// Against a None/True/False literal no guard is needed at all.
    fn identity(&mut self, op: &ast::CmpOp, dst: Reg, a: Reg, ka: Known, b: Reg, kb: Known) -> R<Option<Vec<usize>>> {
        if !self.fast || !matches!(op, ast::CmpOp::Is | ast::CmpOp::IsNot) {
            return Ok(None);
        }
        let mut slow = Vec::new();
        if ka != Known::Singleton && kb != Known::Singleton {
            let is_float = self.typeof_is(a, "number")?;
            slow.push(self.jump_if_true(is_float)?);
        }
        if matches!(op, ast::CmpOp::Is) {
            self.emit(Instr::Eq { dst, a, b })?;
        } else {
            self.emit(Instr::Ne { dst, a, b })?;
        }
        Ok(Some(slow))
    }

    /// One comparison: inline tiers for ints, floats, strs and identity,
    /// everything else (and every guard miss) through the runtime.
    fn compare_once(&mut self, op: &ast::CmpOp, left: Reg, lk: Known, right: Reg, rk: Known) -> R<Reg> {
        let dst = self.alloc()?;
        let mut end = Vec::new();
        let mut slow_jumps = Vec::new();
        if let Some(slow) = self.identity(op, dst, left, lk, right, rk)? {
            if slow.is_empty() {
                return Ok(dst);
            }
            slow_jumps = slow;
            end.push(self.jump()?);
        }
        let mut next: Vec<usize> = Vec::new();
        for tier in self.compare_tiers(op, lk, rk) {
            let here = self.here();
            for j in next.drain(..) {
                self.patch(j, here)?;
            }
            let (to_next, to_slow) = self.tier_guard(left, lk, right, rk, tier)?;
            next = to_next;
            slow_jumps.extend(to_slow);
            let instr = Self::compare_instr(op, dst, left, right).expect("an inline comparison");
            self.emit(instr)?;
            end.push(self.jump()?);
        }
        slow_jumps.extend(next);
        let here = self.here();
        for j in slow_jumps {
            self.patch(j, here)?;
        }
        let r = self.helper(cmpop_name(op), &[left, right])?;
        self.emit(Instr::Move { dst, src: r })?;
        let here = self.here();
        for j in end {
            self.patch(j, here)?;
        }
        Ok(dst)
    }

    /// Compile `test` as a branch: the returned jumps are taken when its
    /// truth equals `when`, and execution falls through otherwise. A single
    /// comparison of two ints or two floats is one fused compare-and-branch
    /// (no boolean, no truth test); `and`/`or`/`not` short-circuit into the
    /// branch; a result known to be a bool skips the truth test.
    pub fn branch(&mut self, test: &ast::Expr, when: bool, depth: usize) -> R<Vec<usize>> {
        self.depth(test, depth)?;
        match test {
            ast::Expr::UnaryOp(u) if matches!(u.op, ast::UnaryOp::Not) && self.fast => {
                self.branch(&u.operand, !when, depth + 1)
            }
            ast::Expr::BoolOp(b) if self.fast => {
                // `and` stops at the first false operand, `or` at the first
                // true one; that stop is the branch's outcome when it matches
                // `when`, and otherwise skips the rest.
                let stops_on = matches!(b.op, ast::BoolOp::Or);
                let mut taken = Vec::new();
                let mut skips = Vec::new();
                let last = b.values.len() - 1;
                for (i, value) in b.values.iter().enumerate() {
                    if i > 0 {
                        self.speculative += 1;
                    }
                    let jumps = if i == last {
                        self.branch(value, when, depth + 1)
                    } else {
                        self.branch(value, stops_on, depth + 1)
                    };
                    if i > 0 {
                        self.speculative -= 1;
                    }
                    let jumps = jumps?;
                    if i == last || stops_on == when {
                        taken.extend(jumps);
                    } else {
                        skips.extend(jumps);
                    }
                }
                let here = self.here();
                for j in skips {
                    self.patch(j, here)?;
                }
                Ok(taken)
            }
            ast::Expr::Compare(c) if c.ops.len() == 1 && self.fast => {
                let op = &c.ops[0];
                let (lk, rk) = (known(&c.left), known(&c.comparators[0]));
                let left = self.expr(&c.left, depth + 1)?;
                let right = self.expr(&c.comparators[0], depth + 1)?;
                self.compare_branch(op, left, lk, right, rk, when)
            }
            _ => {
                let value = self.expr(test, depth)?;
                self.jump_on_truth(value, when)
            }
        }
    }

    /// Jumps taken when `value`'s truth equals `when`: a bool is its own
    /// truth, anything else asks the runtime.
    pub fn jump_on_truth(&mut self, value: Reg, when: bool) -> R<Vec<usize>> {
        let jump = |e: &mut Self, cond: Reg| if when { e.jump_if_true(cond) } else { e.jump_if_false(cond) };
        let is_bool = self.typeof_is(value, "boolean")?;
        let slow = self.jump_if_false(is_bool)?;
        let taken = jump(self, value)?;
        let over = self.jump()?;
        let here = self.here();
        self.patch(slow, here)?;
        let t = self.helper("truth", &[value])?;
        let taken2 = jump(self, t)?;
        let here = self.here();
        self.patch(over, here)?;
        Ok(vec![taken, taken2])
    }

    /// A single comparison as a branch (see [`Emitter::branch`]).
    fn compare_branch(&mut self, op: &ast::CmpOp, a: Reg, ka: Known, b: Reg, kb: Known, when: bool) -> R<Vec<usize>> {
        let mut taken = Vec::new();
        let mut done = Vec::new();
        let jump = |e: &mut Self, cond: Reg| if when { e.jump_if_true(cond) } else { e.jump_if_false(cond) };
        // Identity and membership results are always bools.
        if matches!(op, ast::CmpOp::Is | ast::CmpOp::IsNot | ast::CmpOp::In | ast::CmpOp::NotIn) {
            let value = self.compare_once(op, a, ka, b, kb)?;
            taken.push(jump(self, value)?);
            return Ok(taken);
        }
        let mut next: Vec<usize> = Vec::new();
        let mut slow = Vec::new();
        for tier in self.compare_tiers(op, ka, kb) {
            let here = self.here();
            for j in next.drain(..) {
                self.patch(j, here)?;
            }
            let (to_next, to_slow) = self.tier_guard(a, ka, b, kb, tier)?;
            next = to_next;
            slow.extend(to_slow);
            // Operands of one primitive type are never coerced, so a `>` may
            // swap its operands into the fused `<` form.
            let fused = match (op, when) {
                (ast::CmpOp::Lt, false) => Some(Instr::JumpIfNotLt { a, b, target: 0 }),
                (ast::CmpOp::LtE, false) => Some(Instr::JumpIfNotLe { a, b, target: 0 }),
                (ast::CmpOp::Gt, false) => Some(Instr::JumpIfNotLt { a: b, b: a, target: 0 }),
                (ast::CmpOp::GtE, false) => Some(Instr::JumpIfNotLe { a: b, b: a, target: 0 }),
                _ => None,
            };
            match fused {
                Some(instr) => taken.push(self.emit(instr)?),
                None => {
                    let cond = self.alloc()?;
                    let instr = Self::compare_instr(op, cond, a, b).expect("an inline comparison");
                    self.emit(instr)?;
                    taken.push(jump(self, cond)?);
                }
            }
            done.push(self.jump()?);
        }
        slow.extend(next);
        let here = self.here();
        for j in slow {
            self.patch(j, here)?;
        }
        let name = self.string(cmpop_name(op))?;
        let value = self.helper("richcmp", &[name, a, b])?;
        taken.extend(self.jump_on_truth(value, when)?);
        let here = self.here();
        for j in done {
            self.patch(j, here)?;
        }
        Ok(taken)
    }

    fn constant(&mut self, node: &ast::Expr, value: &ast::Constant) -> R<Reg> {
        match value {
            ast::Constant::None => self.none(),
            ast::Constant::Bool(b) => self.boolean(*b),
            ast::Constant::Str(s) => self.string(s),
            ast::Constant::Int(i) => self.integer(&i.to_string()),
            ast::Constant::Float(f) => self.float(*f),
            ast::Constant::Ellipsis => self.prop(self.r_rt, "ELLIPSIS"),
            ast::Constant::Bytes(b) => {
                // One Latin-1 string constant the runtime expands, however
                // long the literal.
                let text: String = b.iter().map(|byte| *byte as char).collect();
                let text = self.string(&text)?;
                self.helper("bytesconst", &[text])
            }
            ast::Constant::Tuple(items) => {
                let mut regs = Vec::with_capacity(items.len());
                for item in items {
                    regs.push(self.constant(node, item)?);
                }
                let arr = self.array(&regs)?;
                self.helper("tuple", &[arr])
            }
            ast::Constant::Complex { .. } => {
                Err(self.error(node, "complex numbers are not supported"))
            }
        }
    }

    /// A JS array of the elements, honouring `*iterable` splices. A long
    /// list is built a chunk at a time, reclaiming each chunk's registers.
    pub fn sequence_array(&mut self, elts: &[ast::Expr], depth: usize) -> R<Reg> {
        if !elts.iter().any(|e| matches!(e, ast::Expr::Starred(_))) {
            if elts.len() <= CHUNK {
                let mut regs = Vec::with_capacity(elts.len());
                for e in elts {
                    regs.push(self.expr(e, depth)?);
                }
                return self.array(&regs);
            }
            let arr = self.alloc()?;
            for (i, chunk) in elts.chunks(CHUNK).enumerate() {
                let mark = self.mark();
                let mut regs = Vec::with_capacity(chunk.len());
                for e in chunk {
                    regs.push(self.expr(e, depth)?);
                }
                let part = self.array(&regs)?;
                if i == 0 {
                    self.emit(Instr::Move { dst: arr, src: part })?;
                } else {
                    self.emit(Instr::ArrayAppend { arr, val: part, spread: true })?;
                }
                self.release(mark);
            }
            return Ok(arr);
        }
        // The helpers extend the array in place, so `arr` stays put.
        let arr = self.array(&[])?;
        for e in elts {
            let mark = self.mark();
            match e {
                ast::Expr::Starred(s) => {
                    let value = self.expr(&s.value, depth)?;
                    self.helper("extend", &[arr, value])?;
                }
                other => {
                    let value = self.expr(other, depth)?;
                    self.emit(Instr::ArrayAppend { arr, val: value, spread: false })?;
                }
            }
            self.release(mark);
        }
        Ok(arr)
    }

    fn fstring(&mut self, values: &[ast::Expr], depth: usize) -> R<Reg> {
        let mut parts = Vec::new();
        for v in values {
            match v {
                ast::Expr::Constant(c) => match &c.value {
                    ast::Constant::Str(s) => parts.push(self.string(s)?),
                    _ => return Err(self.error(v, "unexpected f-string part")),
                },
                ast::Expr::FormattedValue(f) => parts.push(self.fvalue(f, depth)?),
                other => parts.push(self.expr(other, depth)?),
            }
        }
        if !self.fast {
            let arr = self.array(&parts)?;
            return self.helper("strjoin", &[arr]);
        }
        // Every part is a str: concatenate inline, and let the helper raise
        // MemoryError past the runtime's text limit.
        let Some((&first, rest)) = parts.split_first() else {
            return self.string("");
        };
        if rest.is_empty() {
            return Ok(first);
        }
        let mut acc = first;
        for &part in rest {
            let dst = self.alloc()?;
            self.emit(Instr::Add { dst, a: acc, b: part })?;
            acc = dst;
        }
        let len = self.prop(acc, "length")?;
        let limit = self.prop(self.r_rt, "MAX_TEXT")?;
        let ok = self.alloc()?;
        self.emit(Instr::Le { dst: ok, a: len, b: limit })?;
        let fine = self.jump_if_true(ok)?;
        let arr = self.array(&parts)?;
        let joined = self.helper("strjoin", &[arr])?;
        self.emit(Instr::Move { dst: acc, src: joined })?;
        let here = self.here();
        self.patch(fine, here)?;
        Ok(acc)
    }
    fn fvalue(&mut self, f: &ast::ExprFormattedValue, depth: usize) -> R<Reg> {
        let value = self.expr(&f.value, depth)?;
        if self.fast && f.format_spec.is_none() && f.conversion.to_byte().is_none() {
            // `{x}` with no spec or conversion: a str is itself and an int
            // its decimal digits (JS ToString agrees); anything else formats
            // through the runtime.
            let dst = self.alloc()?;
            let is_str = self.typeof_is(value, "string")?;
            let not_str = self.jump_if_false(is_str)?;
            self.emit(Instr::Move { dst, src: value })?;
            let done = self.jump()?;
            let here = self.here();
            self.patch(not_str, here)?;
            let is_int = self.typeof_is(value, "bigint")?;
            let slow = self.jump_if_false(is_int)?;
            self.emit(Instr::ToStr { dst, a: value })?;
            let done2 = self.jump()?;
            let here = self.here();
            self.patch(slow, here)?;
            let spec = self.string("")?;
            let conv = self.small_int(0)?;
            let r = self.helper("fmt", &[value, spec, conv])?;
            self.emit(Instr::Move { dst, src: r })?;
            let here = self.here();
            self.patch(done, here)?;
            self.patch(done2, here)?;
            return Ok(dst);
        }
        let conv = self.small_int(f.conversion.to_byte().map(|b| b as i32).unwrap_or(0))?;
        let spec = match &f.format_spec {
            Some(spec) => match spec.as_ref() {
                ast::Expr::JoinedStr(j) => self.fstring(&j.values, depth)?,
                other => self.expr(other, depth)?,
            },
            None => self.string("")?,
        };
        self.helper("fmt", &[value, spec, conv])
    }

    // ---- calls -----------------------------------------------------------------------
    fn call(&mut self, node: &ast::Expr, c: &ast::ExprCall, depth: usize) -> R<Reg> {
        // Zero-argument `super()` needs the enclosing class cell and the first
        // parameter of the method it appears in.
        if let ast::Expr::Name(n) = c.func.as_ref() {
            if n.id.as_str() == "super" && c.args.is_empty() && c.keywords.is_empty() {
                if let (Some(cell), Some(first)) =
                    (self.cells.get("__class__").copied(), self.first_param_reg())
                {
                    let cls = self.cell_get(cell)?;
                    return self.helper("superof", &[cls, first]);
                }
                if self.cells.contains_key("__class__") {
                    return Err(self.error(
                        node,
                        "super(): the method's first parameter must be a plain local",
                    ));
                }
                return Err(self.error(node, "super(): no enclosing class"));
            }
        }
        if let Some(value) = self.intrinsic_call(c, depth)? {
            return Ok(value);
        }
        // Python's order: the callee (for `obj.name(...)`, the receiver and
        // then the attribute lookup), then the positional arguments, then
        // the keyword arguments.
        if let ast::Expr::Attribute(a) = c.func.as_ref() {
            let obj = self.expr(&a.value, depth)?;
            return self.method_call(obj, a.attr.as_str(), c, depth);
        }
        let f = self.expr(&c.func, depth)?;
        let args = self.sequence_array(&c.args, depth)?;
        if c.keywords.is_empty() {
            return self.call_positional(f, args);
        }
        let kwargs = self.keywords(&c.keywords, depth)?;
        self.call_value(f, args, kwargs)
    }

    /// `len(x)` and `isinstance(x, t)` through a global name: the callee is
    /// loaded and the arguments evaluated as for any call, and when the
    /// callee is the builtin itself (it may be shadowed or replaced) its
    /// implementation is called directly, with no argument array or
    /// binding step.
    fn intrinsic_call(&mut self, c: &ast::ExprCall, depth: usize) -> R<Option<Reg>> {
        let ast::Expr::Name(n) = c.func.as_ref() else {
            return Ok(None);
        };
        let name = n.id.as_str();
        let (builtin, direct) = match (name, c.args.len()) {
            ("len", 1) => ("BLEN", "len1"),
            ("isinstance", 2) => ("BISINSTANCE", "isinst"),
            _ => return Ok(None),
        };
        if !self.fast
            || !c.keywords.is_empty()
            || c.args.iter().any(|a| matches!(a, ast::Expr::Starred(_)))
            || self.sym_kind(name) != SymKind::Global
        {
            return Ok(None);
        }
        let f = self.load_name(name)?;
        let mut regs = Vec::with_capacity(c.args.len());
        for a in &c.args {
            regs.push(self.expr(a, depth)?);
        }
        let dst = self.alloc()?;
        let expected = self.prop(self.r_rt, builtin)?;
        let same = self.alloc()?;
        self.emit(Instr::Eq { dst: same, a: f, b: expected })?;
        let other = self.jump_if_false(same)?;
        let r = self.helper(direct, &regs)?;
        self.emit(Instr::Move { dst, src: r })?;
        let done = self.jump()?;
        let here = self.here();
        self.patch(other, here)?;
        let args = self.array(&regs)?;
        let r = self.call_positional(f, args)?;
        self.emit(Instr::Move { dst, src: r })?;
        let here = self.here();
        self.patch(done, here)?;
        Ok(Some(dst))
    }

    /// `obj.name(args)`. When no argument can observe evaluation order (a
    /// constant or a bound local), resolving the attribute after the
    /// arguments is indistinguishable, and one `callmethod` helper does both.
    /// Otherwise `mlookup` resolves the attribute first: a function found on
    /// the type comes back unbound (with `mself` set) and is called with the
    /// receiver prepended, so no bound method is allocated either way.
    fn method_call(&mut self, obj: Reg, name: &str, c: &ast::ExprCall, depth: usize) -> R<Reg> {
        let name = self.string(name)?;
        let transparent = c.args.iter().chain(c.keywords.iter().map(|k| &k.value)).all(|e| self.order_transparent(e));
        if transparent {
            let args = self.sequence_array(&c.args, depth)?;
            let kwargs = self.keywords(&c.keywords, depth)?;
            return self.helper("callmethod", &[obj, name, args, kwargs]);
        }
        let f = self.helper("mlookup", &[obj, name])?;
        // Read before anything else can run.
        let prepend = self.prop(self.r_rt, "mself")?;
        let plain = c.args.len() <= CHUNK && !c.args.iter().any(|e| matches!(e, ast::Expr::Starred(_)));
        let mut regs = Vec::new();
        let mut spread = None;
        if plain {
            for e in &c.args {
                regs.push(self.expr(e, depth)?);
            }
        } else {
            spread = Some(self.sequence_array(&c.args, depth)?);
        }
        let kwargs = if c.keywords.is_empty() {
            None
        } else {
            Some(self.keywords(&c.keywords, depth)?)
        };
        let args = self.alloc()?;
        let unbound = self.jump_if_false(prepend)?;
        match spread {
            None => {
                let mut with_self = Vec::with_capacity(regs.len() + 1);
                with_self.push(obj);
                with_self.extend_from_slice(&regs);
                self.array_into(args, &with_self)?;
            }
            Some(rest) => {
                self.array_into(args, &[obj])?;
                self.emit(Instr::ArrayAppend { arr: args, val: rest, spread: true })?;
            }
        }
        let join = self.jump()?;
        let here = self.here();
        self.patch(unbound, here)?;
        match spread {
            None => self.array_into(args, &regs)?,
            Some(rest) => {
                self.emit(Instr::Move { dst: args, src: rest })?;
            }
        }
        let here = self.here();
        self.patch(join, here)?;
        match kwargs {
            None => self.call_positional(f, args),
            Some(kwargs) => self.call_value(f, args, kwargs),
        }
    }

    /// An argument whose evaluation can neither run code nor raise, and
    /// whose value the attribute lookup cannot change, so it cannot tell
    /// whether the callee was resolved before it. A cell is not: a
    /// `__getattr__` or property run by the lookup may rebind or delete it
    /// through `nonlocal`.
    fn order_transparent(&self, e: &ast::Expr) -> bool {
        match e {
            ast::Expr::Constant(_) => true,
            ast::Expr::Name(n) => {
                let name = n.id.as_str();
                matches!(self.sym_kind(name), SymKind::Local | SymKind::Outer)
                    && self.definite.contains(name)
                    && !self.scope().deleted.contains(name)
            }
            _ => false,
        }
    }

    // ---- comprehensions -------------------------------------------------------------------
    fn comprehension(
        &mut self,
        node: &ast::Expr,
        kind: &str,
        generators: &[ast::Comprehension],
        elts: &[&ast::Expr],
        depth: usize,
    ) -> R<Reg> {
        let outer = self.expr(&generators[0].iter, depth)?;
        // With fast paths on, an exact list, tuple or range is passed as
        // itself for the comprehension to index or count in place.
        let outer_iter = self.helper(if self.fast { "seqiter" } else { "iter" }, &[outer])?;
        if let Some(scope) = self.inlinable(node, kind) {
            return self.inline_comprehension(scope, kind, generators, elts, outer_iter, depth);
        }
        let qualname = if self.kind() == ScopeKind::Module {
            kind.to_owned()
        } else {
            format!("{}.<locals>.{kind}", self.qualname)
        };
        let (func_id, child) = self.compile_child(node, kind, qualname.clone(), |e| {
            let acc = match kind {
                "<listcomp>" => Some(e.helper("list", &[])?),
                "<setcomp>" => Some(e.helper("set", &[])?),
                "<dictcomp>" => Some(e.helper("dict", &[])?),
                _ => None,
            };
            // A list accumulates by an inline push onto its (private) items.
            let items = match (kind, acc) {
                ("<listcomp>", Some(acc)) if e.fast => Some(e.prop(acc, "items")?),
                _ => None,
            };
            e.comp_loop(generators, 0, elts, acc, items, depth + 1)?;
            let result = match acc {
                Some(acc) => acc,
                None => e.none()?,
            };
            e.emit(Instr::Return { src: result })?;
            Ok(())
        })?;
        let empty = self.array(&[])?;
        let none = self.none()?;
        let f = self.make_function(
            func_id, child, kind, &qualname, None, empty, empty, empty, none,
        )?;
        let args = self.array(&[outer_iter])?;
        let null = self.none()?;
        self.call_value(f, args, null)
    }

    /// The scope of a comprehension the symbol table marked inline (see
    /// `Scope::inline`): it runs in the enclosing code (PEP 709), as CPython
    /// 3.12+ runs it, with no function object and no call. Its own names
    /// (the loop targets) get registers of their own, so they stay invisible
    /// outside; the enclosing code's locals it uses are that code's own
    /// registers, and names from further out are cells there already.
    fn inlinable(&self, node: &ast::Expr, kind: &str) -> Option<usize> {
        let range = ast::Ranged::range(node);
        let key = (u32::from(range.start()), u32::from(range.end()));
        let id = *self.table.by_offset.get(&key)?;
        let scope = &self.table.scopes[id];
        (scope.inline && scope.name == kind).then_some(id)
    }

    /// Lower an [`inlinable`](Emitter::inlinable) comprehension in place:
    /// the current emitter compiles its loops against the comprehension's
    /// scope, with the outermost iterable in `outer`.
    fn inline_comprehension(
        &mut self,
        scope: usize,
        kind: &str,
        generators: &[ast::Comprehension],
        elts: &[&ast::Expr],
        outer: Reg,
        depth: usize,
    ) -> R<Reg> {
        let acc = match kind {
            "<listcomp>" => self.helper("list", &[])?,
            "<setcomp>" => self.helper("set", &[])?,
            _ => self.helper("dict", &[])?,
        };
        let items = if kind == "<listcomp>" {
            Some(self.prop(acc, "items")?)
        } else {
            None
        };
        let saved_scope = std::mem::replace(&mut self.scope, scope);
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_definite = std::mem::take(&mut self.definite);
        // `yield` stays an error inside a comprehension.
        let saved_generator = std::mem::replace(&mut self.proto.is_generator, false);
        let parent = &self.table.scopes[saved_scope];
        let result = self.inline_comprehension_body(
            generators,
            elts,
            acc,
            items,
            outer,
            (&saved_locals, &saved_definite, &parent.deleted),
            depth,
        );
        self.proto.is_generator = saved_generator;
        self.definite = saved_definite;
        self.locals = saved_locals;
        self.scope = saved_scope;
        result?;
        Ok(acc)
    }
    fn inline_comprehension_body(
        &mut self,
        generators: &[ast::Comprehension],
        elts: &[&ast::Expr],
        acc: Reg,
        items: Option<Reg>,
        outer: Reg,
        enclosing: (&BTreeMap<String, Reg>, &BTreeSet<String>, &BTreeSet<String>),
        depth: usize,
    ) -> R<()> {
        let (locals, definite, deleted) = enclosing;
        let scope = self.scope();
        for (name, k) in &scope.symbols {
            if *k == SymKind::Outer {
                // The enclosing code's register, definitely bound here when
                // it is definitely bound there.
                let reg = *locals
                    .get(name)
                    .ok_or_else(|| format!("Python emitter: inline comprehension misses {name}"))?;
                self.locals.insert(name.clone(), reg);
                if definite.contains(name) && !deleted.contains(name) {
                    self.definite.insert(name.clone());
                }
                continue;
            }
            if *k != SymKind::Local {
                if *k == SymKind::Free && !self.cells.contains_key(name) {
                    return Err(format!("Python emitter: inline comprehension misses the cell {name}"));
                }
                continue;
            }
            let reg = self.alloc()?;
            if name == ".0" {
                self.emit(Instr::Move { dst: reg, src: outer })?;
                self.definite.insert(name.clone());
            } else {
                let unbound = self.unbound()?;
                self.emit(Instr::Move { dst: reg, src: unbound })?;
            }
            self.locals.insert(name.clone(), reg);
        }
        self.comp_loop(generators, 0, elts, Some(acc), items, depth + 1)
    }

    fn comp_loop(
        &mut self,
        generators: &[ast::Comprehension],
        index: usize,
        elts: &[&ast::Expr],
        acc: Option<Reg>,
        items: Option<Reg>,
        depth: usize,
    ) -> R<()> {
        let g = &generators[index];
        let stepper = if index == 0 {
            // The outermost iterable arrives as `.0`, already through
            // `seqiter` (or `iter`).
            let outer = self.load_name(".0")?;
            self.seq_stepper(outer, false)?
        } else {
            match self.range_call(&g.iter) {
                Some(args) => self.range_stepper(args, depth)?,
                None => {
                    let value = self.expr(&g.iter, depth)?;
                    self.seq_stepper(value, true)?
                }
            }
        };
        let head = self.here();
        let exits = self.loop_header(&stepper, &g.target, depth)?;
        let mut skips = Vec::new();
        for cond in &g.ifs {
            skips.extend(self.branch(cond, false, depth)?);
        }
        self.loops.push(LoopCtx {
            head,
            breaks: Vec::new(),
            continues: Vec::new(),
            handler_depth: self.handler_depth,
        });
        if index + 1 < generators.len() {
            self.comp_loop(generators, index + 1, elts, acc, items, depth)?;
        } else {
            match (acc, items) {
                (Some(acc), _) if elts.len() == 2 => {
                    let k = self.expr(elts[0], depth)?;
                    let v = self.expr(elts[1], depth)?;
                    self.helper("setitem", &[acc, k, v])?;
                }
                (_, Some(items)) => {
                    let v = self.expr(elts[0], depth)?;
                    self.emit(Instr::ArrayAppend {
                        arr: items,
                        val: v,
                        spread: false,
                    })?;
                }
                (Some(acc), None) => {
                    let v = self.expr(elts[0], depth)?;
                    self.helper("accumulate", &[acc, v])?;
                }
                (None, _) => {
                    let v = self.expr(elts[0], depth)?;
                    let dst = self.alloc()?;
                    self.emit(Instr::Yield { dst, val: v })?;
                }
            }
        }
        self.loops.pop();
        for s in skips {
            self.patch(s, head)?;
        }
        self.emit(Instr::Jump { target: head })?;
        let here = self.here();
        for exit in exits {
            self.patch(exit, here)?;
        }
        Ok(())
    }
}

pub(super) fn cmpop_name(op: &ast::CmpOp) -> &'static str {
    match op {
        ast::CmpOp::Eq => "eq",
        ast::CmpOp::NotEq => "ne",
        ast::CmpOp::Lt => "lt",
        ast::CmpOp::LtE => "le",
        ast::CmpOp::Gt => "gt",
        ast::CmpOp::GtE => "ge",
        ast::CmpOp::In => "in",
        ast::CmpOp::NotIn => "notin",
        ast::CmpOp::Is => "is",
        ast::CmpOp::IsNot => "isnot",
    }
}
