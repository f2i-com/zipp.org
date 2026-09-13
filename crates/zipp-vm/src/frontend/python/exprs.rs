//! Expression lowering for the Python emitter.
use super::emitter::{Emitter, LoopCtx, R};
use super::stmts::binop_name;
use super::symtable::ScopeKind;
use crate::bytecode::{Instr, Reg};
use rustpython_parser::ast;

impl<'a> Emitter<'a> {
    pub fn expr(&mut self, expr: &ast::Expr, depth: usize) -> R<Reg> {
        self.depth(expr, depth)?;
        match expr {
            ast::Expr::Name(n) => self.load_name(n.id.as_str()),
            ast::Expr::Constant(c) => self.constant(expr, &c.value),
            ast::Expr::BinOp(b) => {
                let a = self.expr(&b.left, depth + 1)?;
                let c = self.expr(&b.right, depth + 1)?;
                self.arith(&b.op, a, c, false)
            }
            ast::Expr::UnaryOp(u) => {
                let value = self.expr(&u.operand, depth + 1)?;
                match u.op {
                    ast::UnaryOp::Not => {
                        let t = self.truth(value)?;
                        let dst = self.alloc()?;
                        self.emit(Instr::Not { dst, a: t })?;
                        Ok(dst)
                    }
                    ast::UnaryOp::USub => {
                        let op = self.string("neg")?;
                        self.helper("unop", &[op, value])
                    }
                    ast::UnaryOp::UAdd => {
                        let op = self.string("pos")?;
                        self.helper("unop", &[op, value])
                    }
                    ast::UnaryOp::Invert => {
                        let op = self.string("invert")?;
                        self.helper("unop", &[op, value])
                    }
                }
            }
            ast::Expr::BoolOp(b) => {
                let dst = self.alloc()?;
                let mut jumps = Vec::new();
                for (i, value) in b.values.iter().enumerate() {
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
                for (i, (op, right)) in c.ops.iter().zip(c.comparators.iter()).enumerate() {
                    if i > 0 {
                        self.speculative += 1;
                    }
                    let right = self.expr(right, depth + 1);
                    if i > 0 {
                        self.speculative -= 1;
                    }
                    let right = right?;
                    let value = self.compare_once(op, left, right)?;
                    self.emit(Instr::Move { dst, src: value })?;
                    if c.ops.len() > 1 {
                        let cond = self.truth(value)?;
                        jumps.push(self.jump_if_false(cond)?);
                    }
                    left = right;
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
                let mut result = self.helper("dict", &[])?;
                let mut keys = Vec::new();
                let mut values = Vec::new();
                for (k, v) in d.keys.iter().zip(d.values.iter()) {
                    match k {
                        Some(k) => {
                            keys.push(self.expr(k, depth + 1)?);
                            values.push(self.expr(v, depth + 1)?);
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
                            result = self.helper("dictmerge", &[result, mapping])?;
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
                let value = self.expr(&e.test, depth + 1)?;
                let cond = self.truth(value)?;
                let dst = self.alloc()?;
                let otherwise = self.jump_if_false(cond)?;
                self.speculative += 1;
                let yes = self.expr(&e.body, depth + 1);
                self.speculative -= 1;
                let yes = yes?;
                self.emit(Instr::Move { dst, src: yes })?;
                let end = self.jump()?;
                let here = self.here();
                self.patch(otherwise, here)?;
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
            ast::Expr::Yield(y) => {
                if !self.proto.is_generator {
                    return Err(self.error(expr, "'yield' outside function"));
                }
                let value = match &y.value {
                    Some(v) => self.expr(v, depth + 1)?,
                    None => self.none()?,
                };
                let dst = self.alloc()?;
                self.emit(Instr::Yield { dst, val: value })?;
                self.forget_line();
                Ok(dst)
            }
            ast::Expr::YieldFrom(y) => {
                if !self.proto.is_generator {
                    return Err(self.error(expr, "'yield from' outside function"));
                }
                let value = self.expr(&y.value, depth + 1)?;
                let iter = self.helper("iter", &[value])?;
                let head = self.here();
                let item = self.helper("fornext", &[iter])?;
                let done = self.alloc()?;
                self.emit(Instr::Eq {
                    dst: done,
                    a: item,
                    b: self.r_stop,
                })?;
                let exit = self.jump_if_true(done)?;
                let sent = self.alloc()?;
                self.emit(Instr::Yield {
                    dst: sent,
                    val: item,
                })?;
                self.emit(Instr::Jump { target: head })?;
                let here = self.here();
                self.patch(exit, here)?;
                self.forget_line();
                self.helper("genreturned", &[iter])
            }
            ast::Expr::Starred(_) => Err(self.error(
                expr,
                "starred expression is only valid in calls, literals and assignment",
            )),
            ast::Expr::Await(_) => Err(self.error(expr, "'await' is not supported")),
        }
    }

    /// `a op b` (or the in-place form): int op int runs as VM instructions
    /// (JS BigInt arithmetic agrees with Python for + - *; `//` and `%` are
    /// corrected from truncation to floor semantics inline), everything else
    /// goes through the runtime's dispatch.
    pub fn arith(&mut self, op: &ast::Operator, a: Reg, b: Reg, inplace: bool) -> R<Reg> {
        let dst = self.alloc()?;
        let mut slow_jumps = Vec::new();
        let mut end_jumps = Vec::new();
        match op {
            ast::Operator::Add | ast::Operator::Sub | ast::Operator::Mult => {
                slow_jumps = self.both_ints(a, b)?;
                let instr = match op {
                    ast::Operator::Add => Instr::Add { dst, a, b },
                    ast::Operator::Sub => Instr::Sub { dst, a, b },
                    _ => Instr::Mul { dst, a, b },
                };
                self.emit(instr)?;
                end_jumps.push(self.jump()?);
            }
            ast::Operator::Mod | ast::Operator::FloorDiv => {
                slow_jumps = self.both_ints(a, b)?;
                let zero = self.alloc()?;
                self.emit(Instr::LoadBigInt { dst: zero, value: 0 })?;
                let is_zero = self.alloc()?;
                self.emit(Instr::Eq { dst: is_zero, a: b, b: zero })?;
                slow_jumps.push(self.jump_if_true(is_zero)?);
                // Truncating result, then the floor correction when the
                // remainder is non-zero and the signs differ.
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
            }
            _ => {}
        }
        let here = self.here();
        for j in slow_jumps {
            self.patch(j, here)?;
        }
        let name = self.string(binop_name(op))?;
        let r = self.helper(if inplace { "iop" } else { "binop" }, &[name, a, b])?;
        self.emit(Instr::Move { dst, src: r })?;
        let here = self.here();
        for j in end_jumps {
            self.patch(j, here)?;
        }
        Ok(dst)
    }

    /// One comparison: int-int ordering and equality as a VM instruction,
    /// everything else through the runtime.
    fn compare_once(&mut self, op: &ast::CmpOp, left: Reg, right: Reg) -> R<Reg> {
        let dst = self.alloc()?;
        let fast: Option<fn(Reg, Reg, Reg) -> Instr> = match op {
            ast::CmpOp::Lt => Some(|dst, a, b| Instr::Lt { dst, a, b }),
            ast::CmpOp::LtE => Some(|dst, a, b| Instr::Le { dst, a, b }),
            ast::CmpOp::Gt => Some(|dst, a, b| Instr::Gt { dst, a, b }),
            ast::CmpOp::GtE => Some(|dst, a, b| Instr::Ge { dst, a, b }),
            ast::CmpOp::Eq => Some(|dst, a, b| Instr::Eq { dst, a, b }),
            ast::CmpOp::NotEq => Some(|dst, a, b| Instr::Ne { dst, a, b }),
            _ => None,
        };
        let mut end = None;
        let mut slow_jumps = Vec::new();
        if let Some(make) = fast {
            slow_jumps = self.both_ints(left, right)?;
            self.emit(make(dst, left, right))?;
            end = Some(self.jump()?);
        }
        let here = self.here();
        for j in slow_jumps {
            self.patch(j, here)?;
        }
        let name = self.string(cmpop_name(op))?;
        let r = self.helper("richcmp", &[name, left, right])?;
        self.emit(Instr::Move { dst, src: r })?;
        if let Some(end) = end {
            let here = self.here();
            self.patch(end, here)?;
        }
        Ok(dst)
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
                let mut regs = Vec::with_capacity(b.len());
                for byte in b {
                    regs.push(self.small_int(*byte as i32)?);
                }
                let arr = self.array(&regs)?;
                self.helper("bytes", &[arr])
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

    /// A JS array of the elements, honouring `*iterable` splices.
    pub fn sequence_array(&mut self, elts: &[ast::Expr], depth: usize) -> R<Reg> {
        if !elts.iter().any(|e| matches!(e, ast::Expr::Starred(_))) {
            let mut regs = Vec::with_capacity(elts.len());
            for e in elts {
                regs.push(self.expr(e, depth)?);
            }
            return self.array(&regs);
        }
        let mut arr = self.array(&[])?;
        for e in elts {
            match e {
                ast::Expr::Starred(s) => {
                    let value = self.expr(&s.value, depth)?;
                    arr = self.helper("extend", &[arr, value])?;
                }
                other => {
                    let value = self.expr(other, depth)?;
                    arr = self.helper("append", &[arr, value])?;
                }
            }
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
        let arr = self.array(&parts)?;
        self.helper("strjoin", &[arr])
    }
    fn fvalue(&mut self, f: &ast::ExprFormattedValue, depth: usize) -> R<Reg> {
        let value = self.expr(&f.value, depth)?;
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
        let args = self.sequence_array(&c.args, depth)?;
        let kwargs = self.keywords(&c.keywords, depth)?;
        if let ast::Expr::Attribute(a) = c.func.as_ref() {
            let obj = self.expr(&a.value, depth)?;
            let name = self.string(a.attr.as_str())?;
            return self.helper("callmethod", &[obj, name, args, kwargs]);
        }
        let f = self.expr(&c.func, depth)?;
        if c.keywords.is_empty() {
            return self.call_positional(f, args);
        }
        self.call_value(f, args, kwargs)
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
        let outer_iter = self.helper("iter", &[outer])?;
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
            e.comp_loop(generators, 0, elts, acc, depth + 1)?;
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

    fn comp_loop(
        &mut self,
        generators: &[ast::Comprehension],
        index: usize,
        elts: &[&ast::Expr],
        acc: Option<Reg>,
        depth: usize,
    ) -> R<()> {
        let g = &generators[index];
        let iter = if index == 0 {
            // The outermost iterable arrives already iterated as `.0`.
            self.load_name(".0")?
        } else {
            let value = self.expr(&g.iter, depth)?;
            self.helper("iter", &[value])?
        };
        let head = self.here();
        let item = self.helper("fornext", &[iter])?;
        let done = self.alloc()?;
        self.emit(Instr::Eq {
            dst: done,
            a: item,
            b: self.r_stop,
        })?;
        let exit = self.jump_if_true(done)?;
        self.assign(&g.target, item, depth)?;
        let mut skips = Vec::new();
        for cond in &g.ifs {
            let value = self.expr(cond, depth)?;
            let t = self.truth(value)?;
            skips.push(self.jump_if_false(t)?);
        }
        self.loops.push(LoopCtx {
            head,
            breaks: Vec::new(),
            continues: Vec::new(),
            handler_depth: self.handler_depth,
        });
        if index + 1 < generators.len() {
            self.comp_loop(generators, index + 1, elts, acc, depth)?;
        } else {
            match acc {
                Some(acc) if elts.len() == 2 => {
                    let k = self.expr(elts[0], depth)?;
                    let v = self.expr(elts[1], depth)?;
                    self.helper("setitem", &[acc, k, v])?;
                }
                Some(acc) => {
                    let v = self.expr(elts[0], depth)?;
                    self.helper("accumulate", &[acc, v])?;
                }
                None => {
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
        self.patch(exit, here)?;
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
