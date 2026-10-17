//! Statements, blocks and the module body.

use super::expr::{starts_expression, starts_test};
use super::{body_end, range, Follow, PResult, Parser};
use crate::ast::*;
use crate::intern::Sym;
use crate::token::T;

impl Parser<'_> {
    /// `Program`: statements and blank logical lines up to the end of input.
    pub(super) fn parse_program(&mut self) -> PResult<List<StmtId>> {
        let base = self.s.stmts.len();
        loop {
            if self.at_eof() {
                return Ok(self.finish_stmts(base));
            }
            if self.at(T::Newline) {
                self.bump();
                continue;
            }
            self.parse_statement()?;
        }
    }

    /// One compound statement, or one line of simple statements, onto the
    /// statement stack.
    fn parse_statement(&mut self) -> PResult<()> {
        let stmt = match self.tok() {
            T::If => self.parse_if()?,
            T::While => self.parse_while()?,
            T::For => self.parse_for(self.start(), false)?,
            T::Try => self.parse_try()?,
            T::With => self.parse_with(self.start(), false)?,
            T::Def => self.parse_function_def_at(self.start(), false, List::default())?,
            T::Class => self.parse_class_def(List::default())?,
            T::At => self.parse_decorated()?,
            T::Match => self.parse_match()?,
            T::Async => {
                let start = self.start();
                self.bump();
                match self.tok() {
                    T::Def => self.parse_function_def_at(start, true, List::default())?,
                    T::For => self.parse_for(start, true)?,
                    T::With => self.parse_with(start, true)?,
                    _ => return Err(self.unexpected()),
                }
            }
            _ => return self.parse_simple_line(),
        };
        self.s.stmts.push(stmt);
        Ok(())
    }

    /// `Suite`: simple statements on the same line, or an indented block.
    pub(super) fn parse_suite(&mut self) -> PResult<List<StmtId>> {
        let base = self.s.stmts.len();
        if !self.at(T::Newline) {
            self.parse_simple_line()?;
            return Ok(self.finish_stmts(base));
        }
        self.bump();
        if !self.at(T::Indent) {
            return Err(self.unexpected_expecting(true));
        }
        self.bump();
        self.enter()?;
        loop {
            self.parse_statement()?;
            if self.at(T::Dedent) {
                self.bump();
                break;
            }
        }
        self.leave();
        Ok(self.finish_stmts(base))
    }

    /// Small statements separated by `;`, then the end of the line.
    fn parse_simple_line(&mut self) -> PResult<()> {
        loop {
            let stmt = self.parse_small_statement()?;
            self.s.stmts.push(stmt);
            if !self.at(T::Semi) {
                break;
            }
            self.bump();
            if self.at(T::Newline) {
                break;
            }
        }
        self.expect(T::Newline)
    }

    fn parse_small_statement(&mut self) -> PResult<StmtId> {
        let start = self.start();
        let kind = match self.tok() {
            T::Pass => {
                self.bump();
                StmtKind::Pass
            }
            T::Break => {
                self.bump();
                StmtKind::Break
            }
            T::Continue => {
                self.bump();
                StmtKind::Continue
            }
            T::Return => {
                self.bump();
                let value = if starts_test(self.tok()) || self.at(T::Star) {
                    Some(self.parse_list(true)?.0)
                } else {
                    None
                };
                StmtKind::Return { value }
            }
            T::Yield => {
                let value = self.parse_yield()?;
                StmtKind::Expr { value }
            }
            T::Raise => {
                self.bump();
                let (exc, cause) = if starts_test(self.tok()) {
                    let exc = self.parse_test()?;
                    let cause = if self.at(T::From) {
                        self.bump();
                        Some(self.parse_test()?)
                    } else {
                        None
                    };
                    (Some(exc), cause)
                } else {
                    (None, None)
                };
                StmtKind::Raise { exc, cause }
            }
            T::Del => {
                self.bump();
                let base = self.s.exprs.len();
                let target = self.parse_expression_or_star()?;
                self.s.exprs.push(target);
                while self.at(T::Comma) {
                    self.bump();
                    if !(starts_expression(self.tok()) || self.at(T::Star)) {
                        break;
                    }
                    let target = self.parse_expression_or_star()?;
                    self.s.exprs.push(target);
                }
                for i in base..self.s.exprs.len() {
                    let target = self.s.exprs[i];
                    self.set_context(target, ExprContext::Del);
                }
                StmtKind::Delete {
                    targets: self.finish_exprs(base),
                }
            }
            T::Import => {
                self.bump();
                let base = self.s.aliases.len();
                let alias = self.parse_import_alias(true)?;
                self.s.aliases.push(alias);
                while self.at(T::Comma) {
                    self.bump();
                    let alias = self.parse_import_alias(true)?;
                    self.s.aliases.push(alias);
                }
                StmtKind::Import {
                    names: self.finish_aliases(base),
                }
            }
            T::From => return self.parse_import_from(start),
            T::Global | T::Nonlocal => {
                let global = self.at(T::Global);
                self.bump();
                let base = self.s.syms.len();
                let name = self.identifier()?;
                self.s.syms.push(name);
                while self.at(T::Comma) {
                    self.bump();
                    let name = self.identifier()?;
                    self.s.syms.push(name);
                }
                let names = self.finish_syms(base);
                if global {
                    StmtKind::Global { names }
                } else {
                    StmtKind::Nonlocal { names }
                }
            }
            T::Assert => {
                self.bump();
                let test = self.parse_test()?;
                let msg = if self.at(T::Comma) {
                    self.bump();
                    Some(self.parse_test()?)
                } else {
                    None
                };
                StmtKind::Assert { test, msg }
            }
            T::Type => {
                self.bump();
                let name_start = self.start();
                let id = self.identifier()?;
                let name = self.add_expr(
                    range(name_start, self.prev_end),
                    ExprKind::Name {
                        id,
                        ctx: ExprContext::Store,
                    },
                );
                let type_params = if self.at(T::Lsqb) {
                    self.parse_type_params()?
                } else {
                    List::default()
                };
                self.expect(T::Equal)?;
                let value = self.parse_test()?;
                StmtKind::TypeAlias {
                    name,
                    type_params,
                    value,
                }
            }
            _ => return self.parse_expression_statement(start),
        };
        Ok(self.add_stmt(range(start, self.prev_end), kind))
    }

    fn parse_expression_or_star(&mut self) -> PResult<ExprId> {
        if self.at(T::Star) {
            self.parse_star_expr()
        } else {
            self.parse_expression()
        }
    }

    /// An expression, an assignment, an augmented or annotated assignment.
    fn parse_expression_statement(&mut self, start: u32) -> PResult<StmtId> {
        let (expr, single_test) = self.parse_list(true)?;
        let op = match self.tok() {
            T::Equal => {
                let base = self.s.exprs.len();
                self.s.exprs.push(expr);
                while self.at(T::Equal) {
                    self.bump();
                    let value = self.parse_list_or_yield()?;
                    self.s.exprs.push(value);
                }
                let value = self.s.exprs.pop().unwrap();
                for i in base..self.s.exprs.len() {
                    let target = self.s.exprs[i];
                    self.set_context(target, ExprContext::Store);
                }
                let targets = self.finish_exprs(base);
                return Ok(self.add_stmt(
                    range(start, self.prev_end),
                    StmtKind::Assign { targets, value },
                ));
            }
            T::Colon if single_test => {
                self.bump();
                let annotation = self.parse_test()?;
                let value = if self.at(T::Equal) {
                    self.bump();
                    Some(self.parse_list_or_yield()?)
                } else {
                    None
                };
                let simple = matches!(self.m.exprs[expr.index()].kind, ExprKind::Name { .. });
                self.set_context(expr, ExprContext::Store);
                return Ok(self.add_stmt(
                    range(start, self.prev_end),
                    StmtKind::AnnAssign {
                        target: expr,
                        annotation,
                        value,
                        simple,
                    },
                ));
            }
            T::PlusEqual => Operator::Add,
            T::MinusEqual => Operator::Sub,
            T::StarEqual => Operator::Mult,
            T::AtEqual => Operator::MatMult,
            T::SlashEqual => Operator::Div,
            T::PercentEqual => Operator::Mod,
            T::AmperEqual => Operator::BitAnd,
            T::VbarEqual => Operator::BitOr,
            T::CircumflexEqual => Operator::BitXor,
            T::LeftShiftEqual => Operator::LShift,
            T::RightShiftEqual => Operator::RShift,
            T::DoubleStarEqual => Operator::Pow,
            T::DoubleSlashEqual => Operator::FloorDiv,
            _ => {
                return Ok(
                    self.add_stmt(range(start, self.prev_end), StmtKind::Expr { value: expr })
                );
            }
        };
        self.bump();
        let value = self.parse_list_or_yield()?;
        self.set_context(expr, ExprContext::Store);
        Ok(self.add_stmt(
            range(start, self.prev_end),
            StmtKind::AugAssign {
                target: expr,
                op,
                value,
            },
        ))
    }

    /// `name [as name]` for `import` (dotted names) and `from ... import`.
    fn parse_import_alias(&mut self, dotted: bool) -> PResult<Alias> {
        let start = self.start();
        let name = if dotted {
            self.parse_dotted_name()?
        } else {
            self.identifier()?
        };
        let asname = if self.at(T::As) {
            self.bump();
            Some(self.identifier()?)
        } else {
            None
        };
        Ok(Alias {
            range: range(start, self.prev_end),
            name,
            asname,
        })
    }

    fn parse_dotted_name(&mut self) -> PResult<Sym> {
        let first = self.identifier()?;
        if !self.at(T::Dot) {
            return Ok(first);
        }
        let mut name = String::from(self.m.name(first));
        while self.at(T::Dot) {
            self.bump();
            let part = self.identifier()?;
            name.push('.');
            name.push_str(self.m.name(part));
        }
        Ok(self.intern_owned(name))
    }

    fn parse_import_from(&mut self, start: u32) -> PResult<StmtId> {
        self.bump();
        let mut level = 0u32;
        loop {
            match self.tok() {
                T::Dot => level += 1,
                T::Ellipsis => level += 3,
                _ => break,
            }
            self.bump();
        }
        let module = if self.at(T::Name) || level == 0 {
            Some(self.parse_dotted_name()?)
        } else {
            None
        };
        self.expect(T::Import)?;
        let base = self.s.aliases.len();
        match self.tok() {
            T::Star => {
                let star = self.start();
                self.bump();
                let name = self.intern_owned("*".to_owned());
                self.s.aliases.push(Alias {
                    range: range(star, self.prev_end),
                    name,
                    asname: None,
                });
            }
            T::Lpar => {
                self.bump();
                let alias = self.parse_import_alias(false)?;
                self.s.aliases.push(alias);
                while self.at(T::Comma) {
                    self.bump();
                    if self.at(T::Rpar) {
                        break;
                    }
                    let alias = self.parse_import_alias(false)?;
                    self.s.aliases.push(alias);
                }
                self.expect(T::Rpar)?;
            }
            _ => {
                let alias = self.parse_import_alias(false)?;
                self.s.aliases.push(alias);
                while self.at(T::Comma) {
                    self.bump();
                    let alias = self.parse_import_alias(false)?;
                    self.s.aliases.push(alias);
                }
            }
        }
        let names = self.finish_aliases(base);
        Ok(self.add_stmt(
            range(start, self.prev_end),
            StmtKind::ImportFrom {
                module,
                names,
                level,
            },
        ))
    }

    fn parse_if(&mut self) -> PResult<StmtId> {
        let start = self.start();
        self.bump();
        let (test, _) = self.parse_named_test()?;
        self.expect(T::Colon)?;
        let body = self.parse_suite()?;
        let mut elifs = Vec::new();
        while self.at(T::Elif) {
            let elif_start = self.start();
            self.bump();
            let (test, _) = self.parse_named_test()?;
            self.expect(T::Colon)?;
            let body = self.parse_suite()?;
            elifs.push((elif_start, test, body));
        }
        let mut orelse = self.parse_else()?;
        let end = if !orelse.is_empty() {
            body_end(&self.m, orelse)
        } else if let Some(last) = elifs.last() {
            body_end(&self.m, last.2)
        } else {
            body_end(&self.m, body)
        };
        for (elif_start, test, body) in elifs.into_iter().rev() {
            let stmt = self.add_stmt(range(elif_start, end), StmtKind::If { test, body, orelse });
            let base = self.s.stmts.len();
            self.s.stmts.push(stmt);
            orelse = self.finish_stmts(base);
        }
        Ok(self.add_stmt(range(start, end), StmtKind::If { test, body, orelse }))
    }

    /// An optional `else: Suite`.
    fn parse_else(&mut self) -> PResult<List<StmtId>> {
        if !self.at(T::Else) {
            return Ok(List::default());
        }
        self.bump();
        self.expect(T::Colon)?;
        self.parse_suite()
    }

    fn parse_while(&mut self) -> PResult<StmtId> {
        let start = self.start();
        self.bump();
        let (test, _) = self.parse_named_test()?;
        self.expect(T::Colon)?;
        let body = self.parse_suite()?;
        let orelse = self.parse_else()?;
        let end = body_end(&self.m, if orelse.is_empty() { body } else { orelse });
        Ok(self.add_stmt(range(start, end), StmtKind::While { test, body, orelse }))
    }

    /// `[async] for`; `start` is where `async` (or `for`) begins.
    fn parse_for(&mut self, start: u32, is_async: bool) -> PResult<StmtId> {
        self.bump(); // `for`
        let (target, _) = self.parse_list(false)?;
        self.expect(T::In)?;
        let (iter, _) = self.parse_list(true)?;
        self.expect(T::Colon)?;
        let body = self.parse_suite()?;
        let orelse = self.parse_else()?;
        let end = body_end(&self.m, if orelse.is_empty() { body } else { orelse });
        self.set_context(target, ExprContext::Store);
        Ok(self.add_stmt(
            range(start, end),
            StmtKind::For {
                is_async,
                target,
                iter,
                body,
                orelse,
            },
        ))
    }

    fn parse_try(&mut self) -> PResult<StmtId> {
        let start = self.start();
        self.bump();
        self.expect(T::Colon)?;
        let body = self.parse_suite()?;
        let hbase = self.s.handlers.len();
        if self.at(T::Finally) {
            self.bump();
            self.expect(T::Colon)?;
            let finalbody = self.parse_suite()?;
            let end = body_end(&self.m, finalbody);
            let handlers = self.finish_handlers(hbase);
            return Ok(self.add_stmt(
                range(start, end),
                StmtKind::Try {
                    star: false,
                    body,
                    handlers,
                    orelse: List::default(),
                    finalbody,
                },
            ));
        }
        if !self.at(T::Except) {
            return Err(self.unexpected());
        }
        let star = self.peek(1) == T::Star;
        while self.at(T::Except) {
            let handler_start = self.start();
            self.bump();
            let type_ = if star {
                self.expect(T::Star)?;
                Some(self.parse_test()?)
            } else if starts_test(self.tok()) {
                Some(self.parse_test()?)
            } else {
                None
            };
            let name = if type_.is_some() && self.at(T::As) {
                self.bump();
                Some(self.identifier()?)
            } else {
                None
            };
            self.expect(T::Colon)?;
            let body = self.parse_suite()?;
            let end = body_end(&self.m, body);
            self.s.handlers.push(ExceptHandler {
                range: range(handler_start, end),
                type_,
                name,
                body,
            });
        }
        let handlers = self.finish_handlers(hbase);
        let orelse = self.parse_else()?;
        let finalbody = if self.at(T::Finally) {
            self.bump();
            self.expect(T::Colon)?;
            self.parse_suite()?
        } else {
            List::default()
        };
        let end = if !finalbody.is_empty() {
            body_end(&self.m, finalbody)
        } else if !orelse.is_empty() {
            body_end(&self.m, orelse)
        } else {
            self.m.list(handlers).last().map_or(0, |h| h.range.end)
        };
        Ok(self.add_stmt(
            range(start, end),
            StmtKind::Try {
                star,
                body,
                handlers,
                orelse,
                finalbody,
            },
        ))
    }

    /// `[async] with`; `start` is where `async` (or `with`) begins.
    fn parse_with(&mut self, start: u32, is_async: bool) -> PResult<StmtId> {
        self.bump(); // `with`
        let items = if self.at(T::Lpar) {
            self.parse_with_items_parenthesized()?
        } else {
            self.parse_with_items()?
        };
        self.expect(T::Colon)?;
        let body = self.parse_suite()?;
        let end = body_end(&self.m, body);
        Ok(self.add_stmt(
            range(start, end),
            StmtKind::With {
                is_async,
                items,
                body,
            },
        ))
    }

    /// `with` items that start with `(`: first as parenthesized items
    /// (`with (a as b, c):`), else as expressions (`with (a, b) as c:`);
    /// when both fail, the failure that got further is the LR parser's.
    fn parse_with_items_parenthesized(&mut self) -> PResult<List<WithItem>> {
        let mark = self.mark();
        match self.try_parenthesized_with_items() {
            Ok(items) => Ok(items),
            Err(first) => {
                self.rewind(&mark);
                match self.parse_with_items() {
                    Ok(items) => Ok(items),
                    Err(second) if second.index > first.index => Err(second),
                    Err(_) => Err(first),
                }
            }
        }
    }

    /// `"(" items ","? ")"` followed by `:`, where each item is
    /// `Test ["as" Expression]`.
    fn try_parenthesized_with_items(&mut self) -> PResult<List<WithItem>> {
        self.bump(); // `(`
        let base = self.s.with_items.len();
        loop {
            let item = self.parse_with_item()?;
            self.s.with_items.push(item);
            if !self.at(T::Comma) {
                break;
            }
            self.bump();
            if self.at(T::Rpar) {
                break;
            }
        }
        self.expect(T::Rpar)?;
        if !self.at(T::Colon) {
            return Err(self.unexpected());
        }
        // The leading items without `as` are one `WithItemsNoAs` list, and
        // share its range.
        let items = &mut self.s.with_items[base..];
        let plain = items
            .iter()
            .take_while(|i| i.optional_vars.is_none())
            .count();
        if plain > 0 {
            let shared = range(items[0].range.start, items[plain - 1].range.end);
            for item in &mut items[..plain] {
                item.range = shared;
            }
        }
        Ok(self.finish_with_items(base))
    }

    /// Comma-separated `Test ["as" Expression]` items, no trailing comma.
    fn parse_with_items(&mut self) -> PResult<List<WithItem>> {
        self.with_item_start = self.pos;
        let base = self.s.with_items.len();
        let item = self.parse_with_item()?;
        self.s.with_items.push(item);
        while self.at(T::Comma) {
            self.bump();
            let item = self.parse_with_item()?;
            self.s.with_items.push(item);
        }
        Ok(self.finish_with_items(base))
    }

    fn parse_with_item(&mut self) -> PResult<WithItem> {
        let start = self.start();
        let context_expr = self.parse_test()?;
        let optional_vars = if self.at(T::As) {
            self.bump();
            let vars = self.parse_expression()?;
            self.set_context(vars, ExprContext::Store);
            Some(vars)
        } else {
            None
        };
        Ok(WithItem {
            range: range(start, self.prev_end),
            context_expr,
            optional_vars,
        })
    }

    fn parse_decorated(&mut self) -> PResult<StmtId> {
        let base = self.s.exprs.len();
        while self.at(T::At) {
            self.bump();
            let (decorator, _) = self.parse_named_test()?;
            self.expect(T::Newline)?;
            self.s.exprs.push(decorator);
        }
        let decorators = self.finish_exprs(base);
        match self.tok() {
            T::Def => self.parse_function_def_at(self.start(), false, decorators),
            T::Class => self.parse_class_def(decorators),
            T::Async => {
                let start = self.start();
                self.bump();
                if !self.at(T::Def) {
                    return Err(self.unexpected());
                }
                self.parse_function_def_at(start, true, decorators)
            }
            _ => Err(self.unexpected()),
        }
    }

    /// `def` (current token); `start` is where `async` (or `def`) begins.
    fn parse_function_def_at(
        &mut self,
        start: u32,
        is_async: bool,
        decorator_list: List<ExprId>,
    ) -> PResult<StmtId> {
        self.bump(); // `def`
        let name = self.identifier()?;
        let type_params = if self.at(T::Lsqb) {
            self.parse_type_params()?
        } else {
            List::default()
        };
        let params_start = self.start();
        self.expect(T::Lpar)?;
        let params = if self.at(T::Rpar) {
            None
        } else {
            Some(self.parse_parameter_list(true)?)
        };
        self.expect(T::Rpar)?;
        let args = match params {
            Some(args) => {
                if let Err(error) = self.validate_arguments(args) {
                    return Err(self.reduce_error(error, Follow::Parameters));
                }
                args
            }
            None => self.empty_arguments(range(params_start, self.prev_end)),
        };
        let returns = if self.at(T::Rarrow) {
            self.bump();
            Some(self.parse_test()?)
        } else {
            None
        };
        self.expect(T::Colon)?;
        let body = self.parse_suite()?;
        let end = body_end(&self.m, body);
        Ok(self.add_stmt(
            range(start, end),
            StmtKind::FunctionDef {
                is_async,
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
            },
        ))
    }

    fn parse_class_def(&mut self, decorator_list: List<ExprId>) -> PResult<StmtId> {
        let start = self.start();
        self.bump();
        let name = self.identifier()?;
        let type_params = if self.at(T::Lsqb) {
            self.parse_type_params()?
        } else {
            List::default()
        };
        let (bases, keywords) = if self.at(T::Lpar) {
            self.bump();
            let arguments = self.parse_call_args()?;
            self.bump(); // `)`
            arguments
        } else {
            (List::default(), List::default())
        };
        self.expect(T::Colon)?;
        let body = self.parse_suite()?;
        let end = body_end(&self.m, body);
        Ok(self.add_stmt(
            range(start, end),
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            },
        ))
    }

    /// `TypeParamList`: `[T, *Ts, **P, U: bound, V = default]`.
    fn parse_type_params(&mut self) -> PResult<List<TypeParam>> {
        self.bump(); // `[`
        let base = self.s.type_params.len();
        loop {
            let start = self.start();
            let (name, kind) = match self.tok() {
                T::Star => {
                    self.bump();
                    (self.identifier()?, TypeParamKind::TypeVarTuple)
                }
                T::DoubleStar => {
                    self.bump();
                    (self.identifier()?, TypeParamKind::ParamSpec)
                }
                _ => {
                    let name = self.identifier()?;
                    let bound = if self.at(T::Colon) {
                        self.bump();
                        Some(self.parse_test()?)
                    } else {
                        None
                    };
                    (name, TypeParamKind::TypeVar { bound })
                }
            };
            // PEP 696 (Python 3.13): a default.
            let default = if self.at(T::Equal) {
                self.bump();
                Some(if kind == TypeParamKind::TypeVarTuple && self.at(T::Star) {
                    self.parse_star_expr()?
                } else {
                    self.parse_test()?
                })
            } else {
                None
            };
            self.s.type_params.push(TypeParam {
                range: range(start, self.prev_end),
                name,
                kind,
                default,
            });
            if !self.at(T::Comma) {
                break;
            }
            self.bump();
            if self.at(T::Rsqb) {
                break;
            }
        }
        self.expect(T::Rsqb)?;
        Ok(self.finish_type_params(base))
    }
}
