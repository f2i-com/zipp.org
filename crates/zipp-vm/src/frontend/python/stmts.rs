//! Statement lowering for the Python emitter.
use super::emitter::{Emitter, LoopCtx, R};
use super::symtable::{ScopeKind, SymKind};
use crate::bytecode::{Instr, Reg};
use ast::Ranged;
use rustpython_parser::ast;

impl<'a> Emitter<'a> {
    pub fn suite(&mut self, suite: &[ast::Stmt], depth: usize) -> R<()> {
        for stmt in suite {
            let mark = self.mark();
            self.stmt(stmt, depth)?;
            self.release(mark);
        }
        Ok(())
    }

    /// A nested block: statements inside a branch, loop or handler. Locals
    /// bound inside are not definitely bound afterwards.
    fn block_suite(&mut self, suite: &[ast::Stmt], depth: usize) -> R<()> {
        self.control_depth += 1;
        let result = self.suite(suite, depth);
        self.control_depth -= 1;
        self.forget_line();
        result
    }

    pub fn stmt(&mut self, stmt: &ast::Stmt, depth: usize) -> R<()> {
        self.depth(stmt, depth)?;
        self.stamp_line(stmt)?;
        match stmt {
            ast::Stmt::Pass(_) | ast::Stmt::Global(_) | ast::Stmt::Nonlocal(_) => {}
            ast::Stmt::Expr(s) => {
                self.expr(&s.value, depth + 1)?;
            }
            ast::Stmt::Assign(s) => {
                let value = self.expr(&s.value, depth + 1)?;
                for target in &s.targets {
                    self.assign(target, value, depth + 1)?;
                }
            }
            ast::Stmt::AnnAssign(s) => {
                if let Some(value) = &s.value {
                    let value = self.expr(value, depth + 1)?;
                    self.assign(&s.target, value, depth + 1)?;
                }
                // Module and class bodies record `__annotations__`.
                if let (ast::Expr::Name(n), false) = (s.target.as_ref(), self.in_function()) {
                    let annotation = self.expr(&s.annotation, depth + 1)?;
                    let container = match self.r_ns {
                        Some(ns) => ns,
                        None => self.r_globals,
                    };
                    let name = self.string(n.id.as_str())?;
                    self.helper("annotate", &[container, name, annotation])?;
                }
            }
            ast::Stmt::AugAssign(s) => self.aug_assign(s, depth + 1)?,
            ast::Stmt::Return(s) => {
                if !self.in_function() {
                    return Err(self.error(stmt, "'return' outside function"));
                }
                let value = match s.value.as_ref() {
                    Some(expr) => self.expr(expr, depth + 1)?,
                    None => self.none()?,
                };
                self.emit(Instr::Return { src: value })?;
            }
            ast::Stmt::If(s) => {
                let value = self.expr(&s.test, depth + 1)?;
                let cond = self.truth(value)?;
                let jump = self.jump_if_false(cond)?;
                self.block_suite(&s.body, depth + 1)?;
                let end = self.jump()?;
                let here = self.here();
                self.patch(jump, here)?;
                self.block_suite(&s.orelse, depth + 1)?;
                let here = self.here();
                self.patch(end, here)?;
            }
            ast::Stmt::While(s) => {
                let head = self.here();
                self.forget_line();
                let value = self.expr(&s.test, depth + 1)?;
                let cond = self.truth(value)?;
                let exhausted = self.jump_if_false(cond)?;
                self.loops.push(LoopCtx {
                    head,
                    breaks: Vec::new(),
                    continues: Vec::new(),
                    handler_depth: self.handler_depth,
                });
                self.block_suite(&s.body, depth + 1)?;
                self.emit(Instr::Jump { target: head })?;
                self.finish_loop(exhausted, &s.orelse, depth + 1)?;
            }
            ast::Stmt::For(s) => {
                let value = self.expr(&s.iter, depth + 1)?;
                let iter = self.helper("iter", &[value])?;
                let head = self.here();
                self.forget_line();
                let item = self.helper("fornext", &[iter])?;
                let done = self.alloc()?;
                self.emit(Instr::Eq {
                    dst: done,
                    a: item,
                    b: self.r_stop,
                })?;
                let exhausted = self.jump_if_true(done)?;
                self.control_depth += 1;
                self.assign(&s.target, item, depth + 1)?;
                self.control_depth -= 1;
                self.loops.push(LoopCtx {
                    head,
                    breaks: Vec::new(),
                    continues: Vec::new(),
                    handler_depth: self.handler_depth,
                });
                self.block_suite(&s.body, depth + 1)?;
                self.emit(Instr::Jump { target: head })?;
                self.finish_loop(exhausted, &s.orelse, depth + 1)?;
            }
            ast::Stmt::Break(_) => {
                let Some(ctx) = self.loops.last() else {
                    return Err(self.error(stmt, "'break' outside loop"));
                };
                let floor = ctx.handler_depth;
                let jump = self.loop_jump(floor)?;
                self.loops.last_mut().unwrap().breaks.push(jump);
            }
            ast::Stmt::Continue(_) => {
                let Some(ctx) = self.loops.last() else {
                    return Err(self.error(stmt, "'continue' outside loop"));
                };
                let floor = ctx.handler_depth;
                let jump = self.loop_jump(floor)?;
                self.loops.last_mut().unwrap().continues.push(jump);
            }
            ast::Stmt::Assert(s) => {
                let value = self.expr(&s.test, depth + 1)?;
                let cond = self.truth(value)?;
                let passed = self.jump_if_true(cond)?;
                let msg = match &s.msg {
                    Some(expr) => self.expr(expr, depth + 1)?,
                    None => self.none()?,
                };
                self.helper("assertfail", &[msg])?;
                let here = self.here();
                self.patch(passed, here)?;
            }
            ast::Stmt::Delete(s) => {
                for target in &s.targets {
                    self.delete_target(target, depth + 1)?;
                }
            }
            ast::Stmt::Raise(s) => {
                let exc = match &s.exc {
                    Some(e) => self.expr(e, depth + 1)?,
                    None => self.none()?,
                };
                let cause = match &s.cause {
                    Some(c) => self.expr(c, depth + 1)?,
                    None => self.undefined()?,
                };
                self.helper("raise", &[exc, cause])?;
            }
            ast::Stmt::Try(s) => self.try_stmt(s, depth + 1)?,
            ast::Stmt::With(s) => self.with_stmt(&s.items, &s.body, depth + 1)?,
            ast::Stmt::FunctionDef(s) => {
                let value = self.function_def(
                    stmt,
                    s.name.as_str(),
                    &s.args,
                    &s.body,
                    &s.decorator_list,
                    depth + 1,
                )?;
                self.store_name(s.name.as_str(), value)?;
            }
            ast::Stmt::ClassDef(s) => {
                let value = self.class_def(stmt, s, depth + 1)?;
                self.store_name(s.name.as_str(), value)?;
            }
            ast::Stmt::Import(s) => {
                for alias in &s.names {
                    let module = self.import_module(stmt, alias.name.as_str())?;
                    // `import a.b as c` binds the leaf; `import a.b` binds `a`.
                    match &alias.asname {
                        Some(bound) => self.store_name(bound.as_str(), module)?,
                        None => {
                            let head = alias.name.as_str().split('.').next().unwrap_or("");
                            let value = if head == alias.name.as_str() {
                                module
                            } else {
                                let key = self.string(head)?;
                                self.helper("import", &[key, self.r_globals])?
                            };
                            self.store_name(head, value)?;
                        }
                    }
                }
            }
            ast::Stmt::ImportFrom(s) => {
                if s.level.is_some_and(|level| level.to_u32() > 0) {
                    return Err(self.error(stmt, "relative imports are not supported"));
                }
                let Some(name) = s.module.as_ref() else {
                    return Err(self.error(stmt, "relative imports are not supported"));
                };
                let module = self.import_module(stmt, name.as_str())?;
                for alias in &s.names {
                    if alias.name.as_str() == "*" {
                        if self.kind() != ScopeKind::Module {
                            return Err(self.error(stmt, "import * only allowed at module level"));
                        }
                        self.helper("importstar", &[module, self.r_globals])?;
                        continue;
                    }
                    let attr = self.string(alias.name.as_str())?;
                    let value = self.helper("importfrom", &[module, attr])?;
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.store_name(bound.as_str(), value)?;
                }
            }
            _ => return Err(self.error(stmt, "statement is not supported yet")),
        }
        Ok(())
    }

    fn loop_jump(&mut self, floor: usize) -> R<usize> {
        if self.handler_depth > floor {
            self.emit(Instr::JumpFinally {
                target: 0,
                floor: floor as u16,
            })
        } else {
            self.jump()
        }
    }

    fn finish_loop(&mut self, exhausted: usize, otherwise: &[ast::Stmt], depth: usize) -> R<()> {
        let context = self.loops.pop().ok_or("Python emitter: missing loop")?;
        for jump in context.continues {
            self.patch(jump, context.head)?;
        }
        let here = self.here();
        self.patch(exhausted, here)?;
        self.forget_line();
        self.block_suite(otherwise, depth)?;
        let end = self.here();
        for jump in context.breaks {
            self.patch(jump, end)?;
        }
        self.forget_line();
        Ok(())
    }

    // ---- assignment -------------------------------------------------------------
    pub fn assign(&mut self, target: &ast::Expr, value: Reg, depth: usize) -> R<()> {
        self.depth(target, depth)?;
        match target {
            ast::Expr::Name(n) => self.store_name(n.id.as_str(), value),
            ast::Expr::Tuple(t) => self.unpack_assign(&t.elts, value, depth),
            ast::Expr::List(t) => self.unpack_assign(&t.elts, value, depth),
            ast::Expr::Subscript(t) => {
                let obj = self.expr(&t.value, depth + 1)?;
                let index = self.expr(&t.slice, depth + 1)?;
                self.helper("setitem", &[obj, index, value])?;
                Ok(())
            }
            ast::Expr::Attribute(a) => {
                let obj = self.expr(&a.value, depth + 1)?;
                let name = self.string(a.attr.as_str())?;
                self.helper("setattr", &[obj, name, value])?;
                Ok(())
            }
            ast::Expr::Starred(_) => Err(self.error(
                target,
                "starred assignment target must be in a list or tuple",
            )),
            _ => Err(self.error(target, "cannot assign to this expression")),
        }
    }
    fn unpack_assign(&mut self, targets: &[ast::Expr], value: Reg, depth: usize) -> R<()> {
        let star = targets
            .iter()
            .position(|t| matches!(t, ast::Expr::Starred(_)));
        if targets
            .iter()
            .filter(|t| matches!(t, ast::Expr::Starred(_)))
            .count()
            > 1
        {
            return Err(self.error(&targets[0], "multiple starred expressions in assignment"));
        }
        let count = self.small_int(targets.len() as i32)?;
        let star_index = self.small_int(star.map(|i| i as i32).unwrap_or(-1))?;
        // The whole unpack is validated before any target is written.
        let items = self.helper("unpack", &[value, count, star_index])?;
        for (i, target) in targets.iter().enumerate() {
            let index = self.small_int(i as i32)?;
            let item = self.alloc()?;
            self.emit(Instr::GetIndex {
                dst: item,
                obj: items,
                key: index,
            })?;
            match target {
                ast::Expr::Starred(s) => self.assign(&s.value, item, depth + 1)?,
                other => self.assign(other, item, depth + 1)?,
            }
        }
        Ok(())
    }
    fn delete_target(&mut self, target: &ast::Expr, depth: usize) -> R<()> {
        match target {
            ast::Expr::Name(n) => self.delete_name(n.id.as_str()),
            ast::Expr::Subscript(t) => {
                let obj = self.expr(&t.value, depth + 1)?;
                let index = self.expr(&t.slice, depth + 1)?;
                self.helper("delitem", &[obj, index])?;
                Ok(())
            }
            ast::Expr::Attribute(a) => {
                let obj = self.expr(&a.value, depth + 1)?;
                let name = self.string(a.attr.as_str())?;
                self.helper("delattr", &[obj, name])?;
                Ok(())
            }
            ast::Expr::Tuple(t) => {
                for e in &t.elts {
                    self.delete_target(e, depth + 1)?;
                }
                Ok(())
            }
            ast::Expr::List(t) => {
                for e in &t.elts {
                    self.delete_target(e, depth + 1)?;
                }
                Ok(())
            }
            _ => Err(self.error(target, "cannot delete this expression")),
        }
    }
    fn aug_assign(&mut self, s: &ast::StmtAugAssign, depth: usize) -> R<()> {
        let op = self.string(binop_name(&s.op))?;
        match s.target.as_ref() {
            ast::Expr::Name(n) => {
                let left = self.load_name(n.id.as_str())?;
                let right = self.expr(&s.value, depth)?;
                let value = self.helper("iop", &[op, left, right])?;
                self.store_name(n.id.as_str(), value)
            }
            ast::Expr::Subscript(t) => {
                let obj = self.expr(&t.value, depth)?;
                let index = self.expr(&t.slice, depth)?;
                let left = self.helper("getitem", &[obj, index])?;
                let right = self.expr(&s.value, depth)?;
                let value = self.helper("iop", &[op, left, right])?;
                self.helper("setitem", &[obj, index, value])?;
                Ok(())
            }
            ast::Expr::Attribute(a) => {
                let obj = self.expr(&a.value, depth)?;
                let name = self.string(a.attr.as_str())?;
                let left = self.helper("getattr", &[obj, name])?;
                let right = self.expr(&s.value, depth)?;
                let value = self.helper("iop", &[op, left, right])?;
                self.helper("setattr", &[obj, name, value])?;
                Ok(())
            }
            other => Err(self.error(other, "illegal augmented assignment target")),
        }
    }

    // ---- try / with -----------------------------------------------------------------
    fn try_stmt(&mut self, s: &ast::StmtTry, depth: usize) -> R<()> {
        let has_finally = !s.finalbody.is_empty();
        let (kind_reg, val_reg) = (self.alloc()?, self.alloc()?);
        let fin_push = if has_finally {
            let at = self.emit(Instr::PushFinally {
                target: 0,
                kind_reg,
                val_reg,
            })?;
            self.handler_depth += 1;
            Some(at)
        } else {
            None
        };
        let mut to_finally: Vec<usize> = Vec::new();
        // The try body under a catch-all handler.
        let ereg = self.alloc()?;
        let push = self.emit(Instr::PushHandler {
            catch_target: 0,
            catch_reg: ereg,
        })?;
        self.handler_depth += 1;
        self.block_suite(&s.body, depth)?;
        self.emit(Instr::PopHandler)?;
        self.handler_depth -= 1;
        // `else` runs only when nothing was raised, outside the handler.
        self.block_suite(&s.orelse, depth)?;
        to_finally.push(self.leave_normally(has_finally, kind_reg)?);
        // Handlers.
        let catch_start = self.here();
        self.patch(push, catch_start)?;
        self.forget_line();
        let exc = self.helper("normexc", &[ereg])?;
        for handler in &s.handlers {
            let ast::ExceptHandler::ExceptHandler(h) = handler;
            self.stamp_line(handler)?;
            let next = match &h.type_ {
                Some(ty) => {
                    let spec = self.expr(ty, depth)?;
                    let matched = self.helper("excmatch", &[exc, spec])?;
                    Some(self.jump_if_false(matched)?)
                }
                None => None,
            };
            if let Some(name) = &h.name {
                self.control_depth += 1;
                self.store_name(name.as_str(), exc)?;
                self.control_depth -= 1;
            }
            self.helper("pushexc", &[exc])?;
            // The handler body runs under its own finally so the current-exception
            // stack is popped (and the `as` name unbound) on EVERY exit: normal
            // completion, `return`, `break`/`continue`, or a raise.
            let (k2, v2) = (self.alloc()?, self.alloc()?);
            let body_fin = self.emit(Instr::PushFinally {
                target: 0,
                kind_reg: k2,
                val_reg: v2,
            })?;
            self.handler_depth += 1;
            self.block_suite(&h.body, depth)?;
            self.emit(Instr::LoadInt { dst: k2, val: 0 })?;
            self.emit(Instr::PopFinally)?;
            self.handler_depth -= 1;
            let fin2 = self.here();
            self.patch(body_fin, fin2)?;
            self.forget_line();
            self.helper("popexc", &[])?;
            if let Some(name) = &h.name {
                self.delete_name(name.as_str())?;
            }
            self.emit(Instr::EndFinally {
                kind_reg: k2,
                val_reg: v2,
            })?;
            to_finally.push(self.leave_normally(has_finally, kind_reg)?);
            if let Some(next) = next {
                let here = self.here();
                self.patch(next, here)?;
            }
        }
        // No handler matched: re-raise (through the finally, if any).
        self.emit(Instr::Throw { src: ereg })?;
        if let Some(fin_push) = fin_push {
            self.handler_depth -= 1;
            let fin = self.here();
            self.patch(fin_push, fin)?;
            for j in &to_finally {
                self.patch(*j, fin)?;
            }
            self.forget_line();
            self.block_suite(&s.finalbody, depth)?;
            self.emit(Instr::EndFinally { kind_reg, val_reg })?;
        } else {
            let end = self.here();
            for j in &to_finally {
                self.patch(*j, end)?;
            }
        }
        self.forget_line();
        Ok(())
    }
    /// Normal completion of a protected region: record kind 0 and leave the
    /// finally handler (when there is one), then jump to the join point.
    fn leave_normally(&mut self, has_finally: bool, kind_reg: Reg) -> R<usize> {
        if has_finally {
            self.emit(Instr::LoadInt {
                dst: kind_reg,
                val: 0,
            })?;
            self.emit(Instr::PopFinally)?;
        }
        self.jump()
    }

    fn with_stmt(&mut self, items: &[ast::WithItem], body: &[ast::Stmt], depth: usize) -> R<()> {
        let Some((first, rest)) = items.split_first() else {
            return self.block_suite(body, depth);
        };
        let mgr = self.expr(&first.context_expr, depth)?;
        let pair = self.helper("withenter", &[mgr])?;
        let zero = self.small_int(0)?;
        let one = self.small_int(1)?;
        let exit = self.alloc()?;
        self.emit(Instr::GetIndex {
            dst: exit,
            obj: pair,
            key: zero,
        })?;
        let entered = self.alloc()?;
        self.emit(Instr::GetIndex {
            dst: entered,
            obj: pair,
            key: one,
        })?;
        if let Some(target) = &first.optional_vars {
            self.control_depth += 1;
            self.assign(target, entered, depth)?;
            self.control_depth -= 1;
        }
        let done = self.boolean(false)?;
        let (kind_reg, val_reg) = (self.alloc()?, self.alloc()?);
        let fin_push = self.emit(Instr::PushFinally {
            target: 0,
            kind_reg,
            val_reg,
        })?;
        self.handler_depth += 1;
        let ereg = self.alloc()?;
        let push = self.emit(Instr::PushHandler {
            catch_target: 0,
            catch_reg: ereg,
        })?;
        self.handler_depth += 1;
        self.control_depth += 1;
        if rest.is_empty() {
            self.suite(body, depth)?;
        } else {
            self.with_stmt(rest, body, depth)?;
        }
        self.control_depth -= 1;
        self.emit(Instr::PopHandler)?;
        self.handler_depth -= 1;
        let j1 = self.leave_normally(true, kind_reg)?;
        let catch = self.here();
        self.patch(push, catch)?;
        self.emit(Instr::LoadBool {
            dst: done,
            val: true,
        })?;
        // Calls __exit__(type, value, tb); rethrows `ereg` unless it returned true.
        self.helper("withexit", &[exit, ereg])?;
        let j2 = self.leave_normally(true, kind_reg)?;
        self.handler_depth -= 1;
        let fin = self.here();
        self.patch(fin_push, fin)?;
        self.patch(j1, fin)?;
        self.patch(j2, fin)?;
        let skip = self.jump_if_true(done)?;
        self.helper("withexitnormal", &[exit])?;
        let here = self.here();
        self.patch(skip, here)?;
        self.emit(Instr::EndFinally { kind_reg, val_reg })?;
        self.forget_line();
        Ok(())
    }

    // ---- def / class / import ------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    pub fn function_def(
        &mut self,
        node: &impl Ranged,
        name: &str,
        args: &ast::Arguments,
        body: &[ast::Stmt],
        decorators: &[ast::Expr],
        depth: usize,
    ) -> R<Reg> {
        self.validate_args(node, args)?;
        let mut decorator_regs = Vec::new();
        for d in decorators {
            decorator_regs.push(self.expr(d, depth)?);
        }
        let (defaults, kw_names, kw_values) = self.defaults_of(args)?;
        let qualname = if self.kind() == ScopeKind::Module {
            name.to_owned()
        } else if self.kind() == ScopeKind::Class {
            format!("{}.{}", self.qualname, name)
        } else {
            format!("{}.<locals>.{}", self.qualname, name)
        };
        let doc = match body.first() {
            Some(ast::Stmt::Expr(e)) => match e.value.as_ref() {
                ast::Expr::Constant(c) => match &c.value {
                    ast::Constant::Str(s) => self.string(s)?,
                    _ => self.none()?,
                },
                _ => self.none()?,
            },
            _ => self.none()?,
        };
        let (func_id, child) = self.compile_child(node, name, qualname.clone(), |e| {
            e.suite(body, depth)?;
            let none = e.none()?;
            e.emit(Instr::Return { src: none })?;
            Ok(())
        })?;
        let mut value = self.make_function(
            func_id,
            child,
            name,
            &qualname,
            Some(args),
            defaults,
            kw_names,
            kw_values,
            doc,
        )?;
        for d in decorator_regs.into_iter().rev() {
            let args = self.array(&[value])?;
            let null = self.none()?;
            value = self.call_value(d, args, null)?;
        }
        Ok(value)
    }

    fn class_def(&mut self, node: &ast::Stmt, s: &ast::StmtClassDef, depth: usize) -> R<Reg> {
        if !s.type_params.is_empty() {
            return Err(self.error(node, "class type parameters are not supported"));
        }
        let mut decorator_regs = Vec::new();
        for d in &s.decorator_list {
            decorator_regs.push(self.expr(d, depth)?);
        }
        let mut bases = Vec::new();
        for b in &s.bases {
            bases.push(self.expr(b, depth)?);
        }
        let bases = self.array(&bases)?;
        let kwargs = self.keywords(&s.keywords, depth)?;
        let qualname = if self.kind() == ScopeKind::Module {
            s.name.to_string()
        } else if self.kind() == ScopeKind::Class {
            format!("{}.{}", self.qualname, s.name)
        } else {
            format!("{}.<locals>.{}", self.qualname, s.name)
        };
        let ns = self.helper("newns", &[])?;
        let name_r = self.string(s.name.as_str())?;
        let qual_r = self.string(&qualname)?;
        let module_r = self.string(self.project.module_names[self.unit.module_index as usize])?;
        self.helper("nsinit", &[ns, name_r, qual_r, module_r])?;
        let (func_id, child) =
            self.compile_child(node, s.name.as_str(), qualname.clone(), |e| {
                if let Some(ast::Stmt::Expr(first)) = s.body.first() {
                    if let ast::Expr::Constant(c) = first.value.as_ref() {
                        if let ast::Constant::Str(doc) = &c.value {
                            let doc = e.string(doc)?;
                            e.store_name("__doc__", doc)?;
                        }
                    }
                }
                e.suite(&s.body, depth)?;
                // The body returns its `__class__` cell (or None) so the creator
                // can fill it in once the class object exists.
                let result = match e.cells.get("__class__") {
                    Some(cell) => *cell,
                    None => e.none()?,
                };
                e.emit(Instr::Return { src: result })?;
                Ok(())
            })?;
        let empty = self.array(&[])?;
        let none = self.none()?;
        let body_fn = self.make_function(
            func_id,
            child,
            s.name.as_str(),
            &qualname,
            None,
            empty,
            empty,
            empty,
            none,
        )?;
        let cls = self.helper("buildclass", &[body_fn, ns, name_r, bases, kwargs])?;
        let mut value = cls;
        for d in decorator_regs.into_iter().rev() {
            let args = self.array(&[value])?;
            let null = self.none()?;
            value = self.call_value(d, args, null)?;
        }
        Ok(value)
    }

    fn import_module(&mut self, stmt: &ast::Stmt, name: &str) -> R<Reg> {
        if !self.module_exists(name) {
            return Err(self.error(
                stmt,
                &format!(
                    "No module named '{name}' (project .py files and the built-in modules only)"
                ),
            ));
        }
        let key = self.string(name)?;
        self.helper("import", &[key, self.r_globals])
    }

    /// The keyword arguments of a call or class statement as a runtime
    /// keyword record, or None when there are none.
    pub fn keywords(&mut self, keywords: &[ast::Keyword], depth: usize) -> R<Reg> {
        if keywords.is_empty() {
            return self.none();
        }
        let mut record = self.helper("kwnew", &[])?;
        for k in keywords {
            let value = self.expr(&k.value, depth)?;
            record = match &k.arg {
                Some(name) => {
                    let name = self.string(name.as_str())?;
                    self.helper("kwset", &[record, name, value])?
                }
                None => self.helper("kwmerge", &[record, value])?,
            };
        }
        Ok(record)
    }

    pub fn first_param_reg(&self) -> Option<Reg> {
        let first = self.scope().params.first()?;
        match self.scope().symbols.get(first) {
            Some(SymKind::Cell) => None,
            _ => self.locals.get(first).copied(),
        }
    }
}

pub(super) fn binop_name(op: &ast::Operator) -> &'static str {
    match op {
        ast::Operator::Add => "add",
        ast::Operator::Sub => "sub",
        ast::Operator::Mult => "mul",
        ast::Operator::MatMult => "matmul",
        ast::Operator::Div => "truediv",
        ast::Operator::Mod => "mod",
        ast::Operator::Pow => "pow",
        ast::Operator::LShift => "lshift",
        ast::Operator::RShift => "rshift",
        ast::Operator::BitOr => "or",
        ast::Operator::BitXor => "xor",
        ast::Operator::BitAnd => "and",
        ast::Operator::FloorDiv => "floordiv",
    }
}
