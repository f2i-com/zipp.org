//! Statement lowering for the Python emitter.
use super::emitter::{Emitter, LoopCtx, R};
use super::exprs::{known, small_int_literal, Known};
use super::symtable::{ScopeKind, SymKind};
use crate::bytecode::{Instr, Reg};
use ast::Ranged;
use rustpython_parser::ast;

/// How a loop steps through its iterable (see [`Emitter::loop_header`]):
/// a fast mode chosen once, at loop entry, when the iterable allows it,
/// and otherwise the runtime iterator `iter`, ended by `stop`.
pub(super) struct Stepper {
    pub counted: Option<Counted>,
    pub indexed: Option<Indexed>,
    pub iter: Reg,
    pub stop: Reg,
}
/// Counting `cursor` towards `end` by `step` while `fast` holds.
pub(super) struct Counted {
    pub fast: Reg,
    pub cursor: Reg,
    pub end: Reg,
    pub step: Step,
}
/// Indexing the list or tuple `seq` while `fast` holds.
pub(super) struct Indexed {
    pub fast: Reg,
    pub seq: Reg,
    pub index: Reg,
}
#[derive(Clone, Copy)]
pub(super) enum Step {
    /// A literal step: its sign fixes the loop's direction.
    Imm(i32),
    /// A step known only at run time, with `positive` = step > 0.
    Dynamic { step: Reg, positive: Reg },
}

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
        // Bindings made inside the block are definite for the rest of the
        // block only; afterwards the block may not have run at all.
        let saved = self.definite.clone();
        self.control_depth += 1;
        let result = self.suite(suite, depth);
        self.control_depth -= 1;
        self.definite = saved;
        self.forget_line();
        result
    }

    pub fn stmt(&mut self, stmt: &ast::Stmt, depth: usize) -> R<()> {
        self.depth(stmt, depth)?;
        self.stamp_line(stmt)?;
        match stmt {
            ast::Stmt::Pass(_) | ast::Stmt::Global(_) | ast::Stmt::Nonlocal(_) => {}
            ast::Stmt::Expr(s) => match s.value.as_ref() {
                ast::Expr::Yield(y) => {
                    self.depth(s.value.as_ref(), depth + 1)?;
                    self.yield_value(&s.value, y, depth + 1, false)?;
                }
                value => {
                    self.expr(value, depth + 1)?;
                }
            },
            ast::Stmt::Assign(s) => {
                if let [target] = s.targets.as_slice() {
                    if self.literal_unpack(target, &s.value, depth + 1)? {
                        return Ok(());
                    }
                }
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
                    let annotation = self.annotation_value(&s.annotation, depth + 1)?;
                    let container = match self.r_ns {
                        Some(ns) => ns,
                        None => self.globals()?,
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
            ast::Stmt::Match(s) => self.match_stmt(s, depth)?,
            ast::Stmt::If(s) => self.if_chain(s, depth)?,
            ast::Stmt::While(s) => {
                let head = self.here();
                self.forget_line();
                let exhausted = self.branch(&s.test, false, depth + 1)?;
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
            ast::Stmt::For(s) => self.for_stmt(s, depth)?,
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
                    s.returns.as_deref(),
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
                                let globals = self.globals()?;
                                self.helper("import", &[key, globals])?
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
                        let globals = self.globals()?;
                        self.helper("importstar", &[module, globals])?;
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

    /// `if` with its `elif` ladder (each `elif` is an If alone in the
    /// previous orelse), lowered iteratively: every arm compiles at the
    /// statement's own depth. As in the nested form, a binding made by the
    /// first test is definite afterwards; a later arm's test runs only when
    /// the earlier tests failed, so its bindings are definite for the rest of
    /// the ladder but not after it.
    fn if_chain(&mut self, first: &ast::StmtIf, depth: usize) -> R<()> {
        let mut ends = Vec::new();
        let mut after = None;
        let mut arm = first;
        loop {
            let mark = self.mark();
            let skips = self.branch(&arm.test, false, depth + 1)?;
            self.release(mark);
            if after.is_none() {
                after = Some(self.definite.clone());
            }
            self.block_suite(&arm.body, depth + 1)?;
            let next = match arm.orelse.as_slice() {
                [ast::Stmt::If(next)] => Some(next),
                _ => None,
            };
            if !arm.orelse.is_empty() {
                ends.push(self.jump()?);
            }
            let here = self.here();
            for skip in skips {
                self.patch(skip, here)?;
            }
            match next {
                Some(next) => {
                    self.forget_line();
                    self.stamp_line(next)?;
                    arm = next;
                }
                None => {
                    self.forget_line();
                    self.block_suite(&arm.orelse, depth + 1)?;
                    break;
                }
            }
        }
        let here = self.here();
        for end in ends {
            self.patch(end, here)?;
        }
        if let Some(after) = after {
            self.definite = after;
        }
        self.forget_line();
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

    // ---- match statement ------------------------------------------------------------
    // Each case compiles its pattern to straight-line code that falls
    // through on success (with capture names stored along the way) and
    // jumps to the next case on failure. Captures are stored as they are
    // matched, so a failed case can leave some names bound, as in CPython.
    fn match_stmt(&mut self, s: &ast::StmtMatch, depth: usize) -> R<()> {
        let subject = self.expr(&s.subject, depth + 1)?;
        let mut ends = Vec::new();
        for case in &s.cases {
            self.forget_line();
            self.stamp_line(&case.pattern)?;
            let mut fails = Vec::new();
            self.speculative += 1;
            let matched = self.pattern(&case.pattern, subject, &mut fails, depth + 1);
            self.speculative -= 1;
            matched?;
            if let Some(guard) = &case.guard {
                let value = self.expr(guard, depth + 1)?;
                let cond = self.truth(value)?;
                fails.push(self.jump_if_false(cond)?);
            }
            self.block_suite(&case.body, depth + 1)?;
            ends.push(self.jump()?);
            let here = self.here();
            for jump in fails {
                self.patch(jump, here)?;
            }
        }
        let here = self.here();
        for jump in ends {
            self.patch(jump, here)?;
        }
        self.forget_line();
        Ok(())
    }

    fn pattern(
        &mut self,
        p: &ast::Pattern,
        subject: Reg,
        fails: &mut Vec<usize>,
        depth: usize,
    ) -> R<()> {
        self.depth(p, depth)?;
        match p {
            ast::Pattern::MatchValue(v) => {
                let value = self.expr(&v.value, depth + 1)?;
                let ok = self.helper("meq", &[subject, value])?;
                fails.push(self.jump_if_false(ok)?);
            }
            ast::Pattern::MatchSingleton(s) => {
                let constant = match &s.value {
                    ast::Constant::None => self.none()?,
                    ast::Constant::Bool(b) => self.boolean(*b)?,
                    _ => return Err(self.error(p, "unsupported singleton pattern")),
                };
                let ok = self.alloc()?;
                self.emit(Instr::Eq {
                    dst: ok,
                    a: subject,
                    b: constant,
                })?;
                fails.push(self.jump_if_false(ok)?);
            }
            ast::Pattern::MatchAs(a) => {
                if let Some(inner) = &a.pattern {
                    self.pattern(inner, subject, fails, depth + 1)?;
                }
                if let Some(name) = &a.name {
                    self.store_name(name.as_str(), subject)?;
                }
            }
            ast::Pattern::MatchStar(_) => {
                return Err(self.error(p, "star pattern outside a sequence pattern"));
            }
            ast::Pattern::MatchSequence(q) => {
                let star = q
                    .patterns
                    .iter()
                    .position(|x| matches!(x, ast::Pattern::MatchStar(_)));
                let count = self.small_int(q.patterns.len() as i32)?;
                let star_at = self.small_int(star.map(|i| i as i32).unwrap_or(-1))?;
                let items = self.helper("mseq", &[subject, count, star_at])?;
                self.fail_if_none(items, fails)?;
                for (i, sub) in q.patterns.iter().enumerate() {
                    let item = self.index_of(items, i)?;
                    match sub {
                        ast::Pattern::MatchStar(st) => {
                            if let Some(name) = &st.name {
                                self.store_name(name.as_str(), item)?;
                            }
                        }
                        _ => self.pattern(sub, item, fails, depth + 1)?,
                    }
                }
            }
            ast::Pattern::MatchMapping(m) => {
                let mut keys = Vec::new();
                for k in &m.keys {
                    keys.push(self.expr(k, depth + 1)?);
                }
                let keys = self.array(&keys)?;
                let values = self.helper("mmap", &[subject, keys])?;
                self.fail_if_none(values, fails)?;
                for (i, sub) in m.patterns.iter().enumerate() {
                    let item = self.index_of(values, i)?;
                    self.pattern(sub, item, fails, depth + 1)?;
                }
                if let Some(rest) = &m.rest {
                    let remaining = self.helper("mrest", &[subject, keys])?;
                    self.store_name(rest.as_str(), remaining)?;
                }
            }
            ast::Pattern::MatchClass(c) => {
                let cls = self.expr(&c.cls, depth + 1)?;
                let count = self.small_int(c.patterns.len() as i32)?;
                let values = self.helper("mcls", &[subject, cls, count])?;
                self.fail_if_none(values, fails)?;
                for (i, sub) in c.patterns.iter().enumerate() {
                    let item = self.index_of(values, i)?;
                    self.pattern(sub, item, fails, depth + 1)?;
                }
                for (name, sub) in c.kwd_attrs.iter().zip(&c.kwd_patterns) {
                    let key = self.string(name.as_str())?;
                    let value = self.helper("mattr", &[subject, key])?;
                    let unbound = self.unbound()?;
                    let missing = self.alloc()?;
                    self.emit(Instr::Eq {
                        dst: missing,
                        a: value,
                        b: unbound,
                    })?;
                    fails.push(self.jump_if_true(missing)?);
                    self.pattern(sub, value, fails, depth + 1)?;
                }
            }
            ast::Pattern::MatchOr(o) => {
                let mut done = Vec::new();
                let last = o.patterns.len().saturating_sub(1);
                for (i, alt) in o.patterns.iter().enumerate() {
                    if i == last {
                        self.pattern(alt, subject, fails, depth + 1)?;
                    } else {
                        let mut alt_fails = Vec::new();
                        self.pattern(alt, subject, &mut alt_fails, depth + 1)?;
                        done.push(self.jump()?);
                        let here = self.here();
                        for jump in alt_fails {
                            self.patch(jump, here)?;
                        }
                    }
                }
                let here = self.here();
                for jump in done {
                    self.patch(jump, here)?;
                }
            }
        }
        Ok(())
    }

    fn fail_if_none(&mut self, value: Reg, fails: &mut Vec<usize>) -> R<()> {
        let null = self.none()?;
        let is_null = self.alloc()?;
        self.emit(Instr::Eq {
            dst: is_null,
            a: value,
            b: null,
        })?;
        fails.push(self.jump_if_true(is_null)?);
        Ok(())
    }

    fn index_of(&mut self, array: Reg, index: usize) -> R<Reg> {
        let key = self.small_int(index as i32)?;
        let dst = self.alloc()?;
        self.emit(Instr::GetIndex {
            dst,
            obj: array,
            key,
        })?;
        Ok(dst)
    }

    /// `range(a[, b[, c]])` by name, with plain positional arguments: the
    /// callee value and the evaluated argument array, for the counted-loop
    /// fast path. `None` for any other iterable expression.
    pub fn range_call<'e>(&self, iter: &'e ast::Expr) -> Option<&'e [ast::Expr]> {
        let ast::Expr::Call(c) = iter else {
            return None;
        };
        let ast::Expr::Name(n) = c.func.as_ref() else {
            return None;
        };
        if !self.fast
            || n.id.as_str() != "range"
            || c.args.is_empty()
            || c.args.len() > 3
            || !c.keywords.is_empty()
            || c.args.iter().any(|a| matches!(a, ast::Expr::Starred(_)))
        {
            return None;
        }
        Some(&c.args)
    }

    /// `for target in iter: body [else: orelse]`.
    fn for_stmt(&mut self, s: &ast::StmtFor, depth: usize) -> R<()> {
        let stepper = match self.range_call(&s.iter) {
            Some(args) => self.range_stepper(args, depth + 1)?,
            None => {
                let value = self.expr(&s.iter, depth + 1)?;
                self.seq_stepper(value, true)?
            }
        };
        let head = self.here();
        self.forget_line();
        // The target is bound before every iteration of the body, but not
        // necessarily after the loop (the iterable may be empty).
        let saved = self.definite.clone();
        let exhausted = self.loop_header(&stepper, &s.target, depth + 1)?;
        self.loops.push(LoopCtx {
            head,
            breaks: Vec::new(),
            continues: Vec::new(),
            handler_depth: self.handler_depth,
        });
        self.block_suite(&s.body, depth + 1)?;
        self.emit(Instr::Jump { target: head })?;
        self.definite = saved;
        self.finish_loop(exhausted, &s.orelse, depth + 1)
    }

    /// `for x in range(...)` with plain positional arguments. When `range`
    /// is the builtin (checked at run time: it may be shadowed) and every
    /// argument is an int, the loop counts on BigInt registers taken straight
    /// from the arguments: no range object, no iterator, no helper call per
    /// step. Anything else (a shadowed `range`, a float, a bool or an
    /// `__index__` object, a zero step) calls `range` and iterates what it
    /// returns, which also raises what it raises.
    pub fn range_stepper(&mut self, args: &[ast::Expr], depth: usize) -> R<Stepper> {
        let f = self.load_name("range")?;
        let mut regs = Vec::with_capacity(args.len());
        for a in args {
            regs.push(self.expr(a, depth)?);
        }
        let fast = self.boolean(false)?;
        let (cursor, end) = (self.alloc()?, self.alloc()?);
        let iter = self.alloc()?;
        let step_literal = match args.get(2) {
            None => Some(1),
            Some(e) => small_int_literal(e).filter(|k| *k != 0),
        };
        let step = match step_literal {
            Some(k) => Step::Imm(k),
            None => Step::Dynamic {
                step: self.alloc()?,
                positive: self.alloc()?,
            },
        };
        let range = self.prop(self.r_rt, "TRANGE")?;
        let is_range = self.alloc()?;
        self.emit(Instr::Eq { dst: is_range, a: f, b: range })?;
        let mut generic = vec![self.jump_if_false(is_range)?];
        for (a, reg) in args.iter().zip(&regs) {
            if known(a) != Known::Int {
                let ok = self.typeof_is(*reg, "bigint")?;
                generic.push(self.jump_if_false(ok)?);
            }
        }
        match regs.as_slice() {
            [stop] => {
                self.emit(Instr::LoadBigInt { dst: cursor, value: 0 })?;
                self.emit(Instr::Move { dst: end, src: *stop })?;
            }
            [start, stop, ..] => {
                self.emit(Instr::Move { dst: cursor, src: *start })?;
                self.emit(Instr::Move { dst: end, src: *stop })?;
            }
            [] => return Err("Python emitter: range() without arguments".into()),
        }
        if let Step::Dynamic { step, positive } = step {
            let zero = self.alloc()?;
            self.emit(Instr::LoadBigInt { dst: zero, value: 0 })?;
            let is_zero = self.alloc()?;
            self.emit(Instr::Eq { dst: is_zero, a: regs[2], b: zero })?;
            generic.push(self.jump_if_true(is_zero)?);
            self.emit(Instr::Move { dst: step, src: regs[2] })?;
            self.emit(Instr::Lt { dst: positive, a: zero, b: step })?;
        }
        self.emit(Instr::LoadBool { dst: fast, val: true })?;
        let done = self.jump()?;
        let here = self.here();
        for j in generic {
            self.patch(j, here)?;
        }
        let arr = self.array(&regs)?;
        let null = self.none()?;
        let value = self.call_value(f, arr, null)?;
        let it = self.helper("iter", &[value])?;
        self.emit(Instr::Move { dst: iter, src: it })?;
        let here = self.here();
        self.patch(done, here)?;
        let stop = self.stop()?;
        Ok(Stepper {
            counted: Some(Counted { fast, cursor, end, step }),
            indexed: None,
            iter,
            stop,
        })
    }

    /// Iteration over an evaluated `value`. With fast paths on, an exact
    /// list or tuple is indexed in place (its current `items` and length
    /// re-read every step, so appends during the loop are seen as CPython's
    /// list iterator sees them) and a range object counts on its own
    /// fields; anything else steps through its iterator. `wrap` is false for
    /// a comprehension's outermost iterable, which arrives already passed
    /// through `seqiter` (an exact list, tuple or range stays itself).
    pub fn seq_stepper(&mut self, value: Reg, wrap: bool) -> R<Stepper> {
        if !self.fast {
            let iter = if wrap { self.helper("iter", &[value])? } else { value };
            let stop = self.stop()?;
            return Ok(Stepper {
                counted: None,
                indexed: None,
                iter,
                stop,
            });
        }
        let iter = if wrap { self.helper("seqiter", &[value])? } else { value };
        // `seqiter` returns an object: a list, tuple or range as itself, or
        // a runtime iterator record.
        let cls = self.prop(iter, "cls")?;
        let list = self.prop(self.r_rt, "TLIST")?;
        let is_seq = self.alloc()?;
        self.emit(Instr::Eq { dst: is_seq, a: cls, b: list })?;
        let tuple_check = self.jump_if_true(is_seq)?;
        let tuple = self.prop(self.r_rt, "TTUPLE")?;
        self.emit(Instr::Eq { dst: is_seq, a: cls, b: tuple })?;
        let here = self.here();
        self.patch(tuple_check, here)?;
        let index = self.small_int(0)?;
        let range = self.prop(self.r_rt, "TRANGE")?;
        let is_range = self.alloc()?;
        self.emit(Instr::Eq { dst: is_range, a: cls, b: range })?;
        // A range object counts from its own start, stop and step (never 0).
        let (cursor, end) = (self.alloc()?, self.alloc()?);
        let (step, positive) = (self.alloc()?, self.alloc()?);
        let not_range = self.jump_if_false(is_range)?;
        let start = self.prop(iter, "start")?;
        self.emit(Instr::Move { dst: cursor, src: start })?;
        let stop_v = self.prop(iter, "stop")?;
        self.emit(Instr::Move { dst: end, src: stop_v })?;
        let step_v = self.prop(iter, "step")?;
        self.emit(Instr::Move { dst: step, src: step_v })?;
        let zero = self.alloc()?;
        self.emit(Instr::LoadBigInt { dst: zero, value: 0 })?;
        self.emit(Instr::Lt { dst: positive, a: zero, b: step })?;
        let here = self.here();
        self.patch(not_range, here)?;
        let stop = self.stop()?;
        Ok(Stepper {
            counted: Some(Counted {
                fast: is_range,
                cursor,
                end,
                step: Step::Dynamic { step, positive },
            }),
            indexed: Some(Indexed { fast: is_seq, seq: iter, index }),
            iter,
            stop,
        })
    }

    /// The loop head: one step of whichever mode the stepper chose, binding
    /// `target` and falling through into the body, or jumping to the
    /// returned exits once the iterable is exhausted. The generic step comes
    /// first so the fast mode that closes the head falls straight into the
    /// body; a plain name target is bound in each mode directly.
    pub fn loop_header(&mut self, s: &Stepper, target: &ast::Expr, depth: usize) -> R<Vec<usize>> {
        let mut exits = Vec::new();
        let direct = matches!(target, ast::Expr::Name(_));
        let item = self.alloc()?;
        // Lists and tuples are the commoner fast mode: test them first.
        let mut to_mode = Vec::new();
        if let Some(ix) = &s.indexed {
            to_mode.push(self.jump_if_true(ix.fast)?);
        }
        if let Some(c) = &s.counted {
            to_mode.push(self.jump_if_true(c.fast)?);
        }
        let next = self.helper("fornext", &[s.iter])?;
        self.emit(Instr::Move { dst: item, src: next })?;
        let done = self.alloc()?;
        self.emit(Instr::Eq {
            dst: done,
            a: item,
            b: s.stop,
        })?;
        exits.push(self.jump_if_true(done)?);
        let modes = to_mode.len();
        let mut to_body = Vec::new();
        if direct {
            self.assign(target, item, depth)?;
        }
        if modes > 0 {
            to_body.push(self.jump()?);
        }
        let mut modes_left = modes;
        let mut to_mode = to_mode.into_iter();
        if let Some(ix) = &s.indexed {
            let here = self.here();
            self.patch(to_mode.next().expect("indexed mode jump"), here)?;
            modes_left -= 1;
            let items = self.prop(ix.seq, "items")?;
            let len = self.prop(items, "length")?;
            exits.push(self.emit(Instr::JumpIfNotLt { a: ix.index, b: len, target: 0 })?);
            let value = if direct { self.alloc()? } else { item };
            self.emit(Instr::GetIndex {
                dst: value,
                obj: items,
                key: ix.index,
            })?;
            self.emit(Instr::AddInt {
                dst: ix.index,
                a: ix.index,
                imm: 1,
                upd: false,
            })?;
            if direct {
                self.assign(target, value, depth)?;
            }
            if modes_left > 0 {
                to_body.push(self.jump()?);
            }
        }
        if let Some(c) = &s.counted {
            let here = self.here();
            self.patch(to_mode.next().expect("counted mode jump"), here)?;
            modes_left -= 1;
            match c.step {
                Step::Imm(k) if k > 0 => {
                    exits.push(self.emit(Instr::JumpIfNotLt { a: c.cursor, b: c.end, target: 0 })?);
                }
                Step::Imm(_) => {
                    exits.push(self.emit(Instr::JumpIfNotLt { a: c.end, b: c.cursor, target: 0 })?);
                }
                Step::Dynamic { positive, .. } => {
                    let down = self.jump_if_false(positive)?;
                    exits.push(self.emit(Instr::JumpIfNotLt { a: c.cursor, b: c.end, target: 0 })?);
                    let advance = self.jump()?;
                    let here = self.here();
                    self.patch(down, here)?;
                    exits.push(self.emit(Instr::JumpIfNotLt { a: c.end, b: c.cursor, target: 0 })?);
                    let here = self.here();
                    self.patch(advance, here)?;
                }
            }
            if direct {
                self.assign(target, c.cursor, depth)?;
            } else {
                self.emit(Instr::Move { dst: item, src: c.cursor })?;
            }
            match c.step {
                Step::Imm(imm) => {
                    self.emit(Instr::AddInt {
                        dst: c.cursor,
                        a: c.cursor,
                        imm,
                        upd: true,
                    })?;
                }
                Step::Dynamic { step, .. } => {
                    self.emit(Instr::Add {
                        dst: c.cursor,
                        a: c.cursor,
                        b: step,
                    })?;
                }
            }
            if modes_left > 0 {
                to_body.push(self.jump()?);
            }
        }
        let here = self.here();
        for j in to_body {
            self.patch(j, here)?;
        }
        if !direct {
            self.assign(target, item, depth)?;
        }
        Ok(exits)
    }

    fn finish_loop(&mut self, exhausted: Vec<usize>, otherwise: &[ast::Stmt], depth: usize) -> R<()> {
        let context = self.loops.pop().ok_or("Python emitter: missing loop")?;
        for jump in context.continues {
            self.patch(jump, context.head)?;
        }
        let here = self.here();
        for jump in exhausted {
            self.patch(jump, here)?;
        }
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
        let items = self.alloc()?;
        // An exact tuple or list of the right length is read in place.
        let mut join = None;
        if self.fast && star.is_none() {
            let mut slow = Vec::new();
            let is_obj = self.typeof_is(value, "object")?;
            slow.push(self.jump_if_false(is_obj)?);
            let null = self.none()?;
            let is_null = self.alloc()?;
            self.emit(Instr::Eq { dst: is_null, a: value, b: null })?;
            slow.push(self.jump_if_true(is_null)?);
            let cls = self.prop(value, "cls")?;
            let tuple = self.prop(self.r_rt, "TTUPLE")?;
            let exact = self.alloc()?;
            self.emit(Instr::Eq { dst: exact, a: cls, b: tuple })?;
            let is_tuple = self.jump_if_true(exact)?;
            let list = self.prop(self.r_rt, "TLIST")?;
            self.emit(Instr::Eq { dst: exact, a: cls, b: list })?;
            slow.push(self.jump_if_false(exact)?);
            let here = self.here();
            self.patch(is_tuple, here)?;
            let array = self.prop(value, "items")?;
            let len = self.prop(array, "length")?;
            let fits = self.alloc()?;
            self.emit(Instr::Eq { dst: fits, a: len, b: count })?;
            slow.push(self.jump_if_false(fits)?);
            self.emit(Instr::Move { dst: items, src: array })?;
            join = Some(self.jump()?);
            let here = self.here();
            for j in slow {
                self.patch(j, here)?;
            }
        }
        let star_index = self.small_int(star.map(|i| i as i32).unwrap_or(-1))?;
        // The whole unpack is validated before any target is written.
        let unpacked = self.helper("unpack", &[value, count, star_index])?;
        self.emit(Instr::Move { dst: items, src: unpacked })?;
        if let Some(join) = join {
            let here = self.here();
            self.patch(join, here)?;
        }
        // Every item is read before any target is written: a target may be
        // an element of the sequence being unpacked.
        let mut values = Vec::with_capacity(targets.len());
        for i in 0..targets.len() {
            let index = self.small_int(i as i32)?;
            let item = self.alloc()?;
            self.emit(Instr::GetIndex {
                dst: item,
                obj: items,
                key: index,
            })?;
            values.push(item);
        }
        for (target, item) in targets.iter().zip(values) {
            match target {
                ast::Expr::Starred(s) => self.assign(&s.value, item, depth + 1)?,
                other => self.assign(other, item, depth + 1)?,
            }
        }
        Ok(())
    }

    /// `a, b = x, y`: a literal right-hand side of the target's length,
    /// with no starred item on either side, needs no tuple and no unpack.
    /// Each item is copied into its own register before any target is
    /// written, so `a, b = b, a` swaps.
    fn literal_unpack(&mut self, target: &ast::Expr, value: &ast::Expr, depth: usize) -> R<bool> {
        let (ast::Expr::Tuple(ast::ExprTuple { elts: targets, .. })
        | ast::Expr::List(ast::ExprList { elts: targets, .. })) = target
        else {
            return Ok(false);
        };
        let (ast::Expr::Tuple(ast::ExprTuple { elts: items, .. })
        | ast::Expr::List(ast::ExprList { elts: items, .. })) = value
        else {
            return Ok(false);
        };
        let starred = |e: &ast::Expr| matches!(e, ast::Expr::Starred(_));
        if !self.fast
            || targets.len() != items.len()
            || targets.iter().any(starred)
            || items.iter().any(starred)
        {
            return Ok(false);
        }
        self.depth(value, depth)?;
        let mut regs = Vec::with_capacity(items.len());
        for item in items {
            let reg = self.expr(item, depth + 1)?;
            let copy = self.alloc()?;
            self.emit(Instr::Move { dst: copy, src: reg })?;
            regs.push(copy);
        }
        self.depth(target, depth)?;
        for (t, reg) in targets.iter().zip(regs) {
            self.assign(t, reg, depth + 1)?;
        }
        Ok(true)
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
        match s.target.as_ref() {
            ast::Expr::Name(n) => {
                let left = self.load_name(n.id.as_str())?;
                let value = self.arith_operand(&s.op, left, Known::Unknown, &s.value, depth, true)?;
                self.store_name(n.id.as_str(), value)
            }
            ast::Expr::Subscript(t) => {
                let obj = self.expr(&t.value, depth)?;
                let index = self.expr(&t.slice, depth)?;
                let left = self.helper("getitem", &[obj, index])?;
                let value = self.arith_operand(&s.op, left, Known::Unknown, &s.value, depth, true)?;
                self.helper("setitem", &[obj, index, value])?;
                Ok(())
            }
            ast::Expr::Attribute(a) => {
                let obj = self.expr(&a.value, depth)?;
                let name = self.string(a.attr.as_str())?;
                let left = self.helper("getattr", &[obj, name])?;
                let value = self.arith_operand(&s.op, left, Known::Unknown, &s.value, depth, true)?;
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
            self.finally_body(&s.finalbody, kind_reg, val_reg, depth)?;
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
    /// A `finally` body. Entered with an exception propagating (completion
    /// kind 2), that exception is the one being handled while the body runs,
    /// so an exception raised there gets it as `__context__`; the
    /// current-exception stack is popped again on every exit from the body.
    fn finally_body(&mut self, body: &[ast::Stmt], kind_reg: Reg, val_reg: Reg, depth: usize) -> R<()> {
        let pushed = self.boolean(false)?;
        let two = self.small_int(2)?;
        let throwing = self.alloc()?;
        self.emit(Instr::Eq {
            dst: throwing,
            a: kind_reg,
            b: two,
        })?;
        let skip = self.jump_if_false(throwing)?;
        let exc = self.helper("normexc", &[val_reg])?;
        self.helper("pushexc", &[exc])?;
        self.emit(Instr::LoadBool {
            dst: pushed,
            val: true,
        })?;
        let here = self.here();
        self.patch(skip, here)?;
        let (k2, v2) = (self.alloc()?, self.alloc()?);
        let body_fin = self.emit(Instr::PushFinally {
            target: 0,
            kind_reg: k2,
            val_reg: v2,
        })?;
        self.handler_depth += 1;
        self.block_suite(body, depth)?;
        self.emit(Instr::LoadInt { dst: k2, val: 0 })?;
        self.emit(Instr::PopFinally)?;
        self.handler_depth -= 1;
        let here = self.here();
        self.patch(body_fin, here)?;
        self.forget_line();
        let not_pushed = self.jump_if_false(pushed)?;
        self.helper("popexc", &[])?;
        let here = self.here();
        self.patch(not_pushed, here)?;
        self.emit(Instr::EndFinally {
            kind_reg: k2,
            val_reg: v2,
        })?;
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
        // The `as` target is bound inside the protected region: when binding
        // it fails, `__exit__` still sees the exception (and may suppress it,
        // leaving the target unbound afterwards).
        let saved = self.definite.clone();
        if let Some(target) = &first.optional_vars {
            self.assign(target, entered, depth)?;
        }
        if rest.is_empty() {
            self.block_suite(body, depth)?;
        } else {
            self.with_stmt(rest, body, depth)?;
        }
        self.definite = saved;
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
        returns: Option<&ast::Expr>,
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
        let simple = Emitter::simple_arity(args);
        let line = self.line_of(node);
        let (func_id, child) = self.compile_child(node, name, qualname.clone(), |e| {
            if let Some(count) = simple {
                e.arity_guard(count)?;
            }
            e.frame_guard(line, |e| {
                e.suite(body, depth)?;
                let none = e.none()?;
                e.emit(Instr::Return { src: none })?;
                Ok(())
            })
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
        // Annotations evaluate in the defining scope, into `__annotations__`.
        let mut ann_names = Vec::new();
        let mut ann_values = Vec::new();
        for a in args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
        {
            if let Some(ann) = &a.def.annotation {
                ann_names.push(self.string(a.def.arg.as_str())?);
                ann_values.push(self.annotation_value(ann, depth)?);
            }
        }
        for a in [&args.vararg, &args.kwarg].into_iter().flatten() {
            if let Some(ann) = &a.annotation {
                ann_names.push(self.string(a.arg.as_str())?);
                ann_values.push(self.annotation_value(ann, depth)?);
            }
        }
        if let Some(ret) = returns {
            ann_names.push(self.string("return")?);
            ann_values.push(self.annotation_value(ret, depth)?);
        }
        if !ann_names.is_empty() {
            let names = self.array(&ann_names)?;
            let values = self.array(&ann_values)?;
            self.helper("fannotate", &[value, names, values])?;
        }
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
        let module_r = self.string(self.module_name())?;
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
        let globals = self.globals()?;
        self.helper("import", &[key, globals])
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

/// The runtime entry point of an augmented assignment (`R.iadd`, ...).
pub(super) fn inplace_binop_name(op: &ast::Operator) -> &'static str {
    match op {
        ast::Operator::Add => "iadd",
        ast::Operator::Sub => "isub",
        ast::Operator::Mult => "imul",
        ast::Operator::MatMult => "imatmul",
        ast::Operator::Div => "itruediv",
        ast::Operator::Mod => "imod",
        ast::Operator::Pow => "ipow",
        ast::Operator::LShift => "ilshift",
        ast::Operator::RShift => "irshift",
        ast::Operator::BitOr => "ior",
        ast::Operator::BitXor => "ixor",
        ast::Operator::BitAnd => "iand",
        ast::Operator::FloorDiv => "ifloordiv",
    }
}
