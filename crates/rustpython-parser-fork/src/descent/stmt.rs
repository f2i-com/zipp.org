//! Statements, blocks and the module body.

use super::{
    body_end,
    expr::{starts_expression, starts_test},
    optional_range, range, Follow, PResult, Parser,
};
use crate::{
    ast::{self, Ranged},
    context::set_context,
    function::validate_arguments,
    text_size::TextSize,
    token::Tok,
};

impl Parser<'_> {
    /// `Program`: statements and blank logical lines up to the end of input.
    pub(super) fn parse_program(&mut self) -> PResult<Vec<ast::Stmt>> {
        let mut body = Vec::new();
        loop {
            if self.at_eof() {
                return Ok(body);
            }
            if self.at(Tok::Newline) {
                self.bump();
                continue;
            }
            self.parse_statement(&mut body)?;
        }
    }

    /// One compound statement, or one line of simple statements.
    fn parse_statement(&mut self, out: &mut Vec<ast::Stmt>) -> PResult<()> {
        let stmt = match self.tok {
            Tok::If => self.parse_if()?,
            Tok::While => self.parse_while()?,
            Tok::For => self.parse_for(self.start(), false)?,
            Tok::Try => self.parse_try()?,
            Tok::With => self.parse_with(self.start(), false)?,
            Tok::Def => self.parse_function_def(Vec::new())?,
            Tok::Class => self.parse_class_def(Vec::new())?,
            Tok::At => self.parse_decorated()?,
            Tok::Match => self.parse_match()?,
            Tok::Async => {
                let start = self.start();
                self.bump();
                match self.tok {
                    Tok::Def => self.parse_function_def_at(start, true, Vec::new())?,
                    Tok::For => self.parse_for(start, true)?,
                    Tok::With => self.parse_with(start, true)?,
                    _ => return Err(self.unexpected()),
                }
            }
            _ => return self.parse_simple_line(out),
        };
        out.push(stmt);
        Ok(())
    }

    /// `Suite`: simple statements on the same line, or an indented block.
    pub(super) fn parse_suite(&mut self) -> PResult<Vec<ast::Stmt>> {
        let mut body = Vec::new();
        if !self.at(Tok::Newline) {
            self.parse_simple_line(&mut body)?;
            return Ok(body);
        }
        self.bump();
        if !self.at(Tok::Indent) {
            return Err(self.unexpected_expecting(Some("Indent")));
        }
        self.bump();
        self.enter()?;
        loop {
            self.parse_statement(&mut body)?;
            if self.at(Tok::Dedent) {
                self.bump();
                break;
            }
        }
        self.leave();
        Ok(body)
    }

    /// Small statements separated by `;`, then the end of the line.
    fn parse_simple_line(&mut self, out: &mut Vec<ast::Stmt>) -> PResult<()> {
        loop {
            out.push(self.parse_small_statement()?);
            if !self.at(Tok::Semi) {
                break;
            }
            self.bump();
            if self.at(Tok::Newline) {
                break;
            }
        }
        self.expect(Tok::Newline)
    }

    fn parse_small_statement(&mut self) -> PResult<ast::Stmt> {
        let start = self.start();
        Ok(match self.tok {
            Tok::Pass => {
                self.bump();
                ast::Stmt::Pass(ast::StmtPass {
                    range: range(start, self.prev_end),
                })
            }
            Tok::Break => {
                self.bump();
                ast::Stmt::Break(ast::StmtBreak {
                    range: range(start, self.prev_end),
                })
            }
            Tok::Continue => {
                self.bump();
                ast::Stmt::Continue(ast::StmtContinue {
                    range: range(start, self.prev_end),
                })
            }
            Tok::Return => {
                self.bump();
                let value = if starts_test(&self.tok) || self.at(Tok::Star) {
                    Some(Box::new(self.parse_list(true)?.0))
                } else {
                    None
                };
                ast::Stmt::Return(ast::StmtReturn {
                    value,
                    range: range(start, self.prev_end),
                })
            }
            Tok::Yield => {
                let value = self.parse_yield()?;
                ast::Stmt::Expr(ast::StmtExpr {
                    value: Box::new(value),
                    range: range(start, self.prev_end),
                })
            }
            Tok::Raise => {
                self.bump();
                let (exc, cause) = if starts_test(&self.tok) {
                    let exc = self.parse_test()?;
                    let cause = if self.at(Tok::From) {
                        self.bump();
                        Some(Box::new(self.parse_test()?))
                    } else {
                        None
                    };
                    (Some(Box::new(exc)), cause)
                } else {
                    (None, None)
                };
                ast::Stmt::Raise(ast::StmtRaise {
                    exc,
                    cause,
                    range: range(start, self.prev_end),
                })
            }
            Tok::Del => {
                self.bump();
                let mut targets = vec![self.parse_expression_or_star()?];
                while self.at(Tok::Comma) {
                    self.bump();
                    if !(starts_expression(&self.tok) || self.at(Tok::Star)) {
                        break;
                    }
                    targets.push(self.parse_expression_or_star()?);
                }
                ast::Stmt::Delete(ast::StmtDelete {
                    targets: targets
                        .into_iter()
                        .map(|expr| set_context(expr, ast::ExprContext::Del))
                        .collect(),
                    range: range(start, self.prev_end),
                })
            }
            Tok::Import => {
                self.bump();
                let mut names = vec![self.parse_import_alias(true)?];
                while self.at(Tok::Comma) {
                    self.bump();
                    names.push(self.parse_import_alias(true)?);
                }
                ast::Stmt::Import(ast::StmtImport {
                    names,
                    range: range(start, self.prev_end),
                })
            }
            Tok::From => self.parse_import_from(start)?,
            Tok::Global | Tok::Nonlocal => {
                let global = self.at(Tok::Global);
                self.bump();
                let mut names = vec![self.identifier()?];
                while self.at(Tok::Comma) {
                    self.bump();
                    names.push(self.identifier()?);
                }
                let range = range(start, self.prev_end);
                if global {
                    ast::Stmt::Global(ast::StmtGlobal { names, range })
                } else {
                    ast::Stmt::Nonlocal(ast::StmtNonlocal { names, range })
                }
            }
            Tok::Assert => {
                self.bump();
                let test = self.parse_test()?;
                let msg = if self.at(Tok::Comma) {
                    self.bump();
                    Some(Box::new(self.parse_test()?))
                } else {
                    None
                };
                ast::Stmt::Assert(ast::StmtAssert {
                    test: Box::new(test),
                    msg,
                    range: range(start, self.prev_end),
                })
            }
            Tok::Type => {
                self.bump();
                let name_start = self.start();
                let id = self.identifier()?;
                let name = ast::Expr::Name(ast::ExprName {
                    id,
                    ctx: ast::ExprContext::Store,
                    range: range(name_start, self.prev_end),
                });
                let type_params = if self.at(Tok::Lsqb) {
                    self.parse_type_params()?
                } else {
                    Vec::new()
                };
                self.expect(Tok::Equal)?;
                let value = self.parse_test()?;
                ast::Stmt::TypeAlias(ast::StmtTypeAlias {
                    name: Box::new(name),
                    type_params,
                    value: Box::new(value),
                    range: range(start, self.prev_end),
                })
            }
            _ => self.parse_expression_statement(start)?,
        })
    }

    /// `ExpressionOrStarExpression`.
    fn parse_expression_or_star(&mut self) -> PResult<ast::Expr> {
        if self.at(Tok::Star) {
            self.parse_star_expr()
        } else {
            self.parse_expression()
        }
    }

    /// An expression, an assignment, an augmented or annotated assignment.
    fn parse_expression_statement(&mut self, start: TextSize) -> PResult<ast::Stmt> {
        let (expr, single_test) = self.parse_list(true)?;
        let aug = match self.tok {
            Tok::Equal => {
                let mut values = Vec::new();
                while self.at(Tok::Equal) {
                    self.bump();
                    values.push(self.parse_list_or_yield()?);
                }
                let value = Box::new(values.pop().unwrap());
                let mut targets = Vec::with_capacity(values.len() + 1);
                targets.push(set_context(expr, ast::ExprContext::Store));
                for target in values {
                    targets.push(set_context(target, ast::ExprContext::Store));
                }
                return Ok(ast::Stmt::Assign(ast::StmtAssign {
                    targets,
                    value,
                    type_comment: None,
                    range: range(start, self.prev_end),
                }));
            }
            Tok::Colon if single_test => {
                self.bump();
                let annotation = self.parse_test()?;
                let value = if self.at(Tok::Equal) {
                    self.bump();
                    Some(Box::new(self.parse_list_or_yield()?))
                } else {
                    None
                };
                let simple = expr.is_name_expr();
                return Ok(ast::Stmt::AnnAssign(ast::StmtAnnAssign {
                    target: Box::new(set_context(expr, ast::ExprContext::Store)),
                    annotation: Box::new(annotation),
                    value,
                    simple,
                    range: range(start, self.prev_end),
                }));
            }
            Tok::PlusEqual => ast::Operator::Add,
            Tok::MinusEqual => ast::Operator::Sub,
            Tok::StarEqual => ast::Operator::Mult,
            Tok::AtEqual => ast::Operator::MatMult,
            Tok::SlashEqual => ast::Operator::Div,
            Tok::PercentEqual => ast::Operator::Mod,
            Tok::AmperEqual => ast::Operator::BitAnd,
            Tok::VbarEqual => ast::Operator::BitOr,
            Tok::CircumflexEqual => ast::Operator::BitXor,
            Tok::LeftShiftEqual => ast::Operator::LShift,
            Tok::RightShiftEqual => ast::Operator::RShift,
            Tok::DoubleStarEqual => ast::Operator::Pow,
            Tok::DoubleSlashEqual => ast::Operator::FloorDiv,
            _ => {
                return Ok(ast::Stmt::Expr(ast::StmtExpr {
                    value: Box::new(expr),
                    range: range(start, self.prev_end),
                }))
            }
        };
        self.bump();
        let value = self.parse_list_or_yield()?;
        Ok(ast::Stmt::AugAssign(ast::StmtAugAssign {
            target: Box::new(set_context(expr, ast::ExprContext::Store)),
            op: aug,
            value: Box::new(value),
            range: range(start, self.prev_end),
        }))
    }

    /// `name [as name]` for `import` (dotted names) and `from ... import`.
    fn parse_import_alias(&mut self, dotted: bool) -> PResult<ast::Alias> {
        let start = self.start();
        let name = if dotted {
            self.parse_dotted_name()?
        } else {
            self.identifier()?
        };
        let asname = if self.at(Tok::As) {
            self.bump();
            Some(self.identifier()?)
        } else {
            None
        };
        Ok(ast::Alias {
            name,
            asname,
            range: range(start, self.prev_end),
        })
    }

    fn parse_dotted_name(&mut self) -> PResult<ast::Identifier> {
        let first = self.identifier()?;
        if !self.at(Tok::Dot) {
            return Ok(first);
        }
        let mut name = String::from(first.as_str());
        while self.at(Tok::Dot) {
            self.bump();
            let part = self.identifier()?;
            name.push('.');
            name.push_str(part.as_str());
        }
        Ok(ast::Identifier::new(name))
    }

    fn parse_import_from(&mut self, start: TextSize) -> PResult<ast::Stmt> {
        self.bump();
        let mut level = 0u32;
        loop {
            match self.tok {
                Tok::Dot => level += 1,
                Tok::Ellipsis => level += 3,
                _ => break,
            }
            self.bump();
        }
        let module = if matches!(self.tok, Tok::Name { .. }) || level == 0 {
            Some(self.parse_dotted_name()?)
        } else {
            None
        };
        self.expect(Tok::Import)?;
        let names = match self.tok {
            Tok::Star => {
                let star = self.start();
                self.bump();
                vec![ast::Alias {
                    name: ast::Identifier::new("*"),
                    asname: None,
                    range: range(star, self.prev_end),
                }]
            }
            Tok::Lpar => {
                self.bump();
                let mut names = vec![self.parse_import_alias(false)?];
                while self.at(Tok::Comma) {
                    self.bump();
                    if self.at(Tok::Rpar) {
                        break;
                    }
                    names.push(self.parse_import_alias(false)?);
                }
                self.expect(Tok::Rpar)?;
                names
            }
            _ => {
                let mut names = vec![self.parse_import_alias(false)?];
                while self.at(Tok::Comma) {
                    self.bump();
                    names.push(self.parse_import_alias(false)?);
                }
                names
            }
        };
        Ok(ast::Stmt::ImportFrom(ast::StmtImportFrom {
            module,
            names,
            level: Some(ast::Int::new(level)),
            range: range(start, self.prev_end),
        }))
    }

    fn parse_if(&mut self) -> PResult<ast::Stmt> {
        let start = self.start();
        self.bump();
        let (test, _) = self.parse_named_test()?;
        self.expect(Tok::Colon)?;
        let body = self.parse_suite()?;
        let mut elifs = Vec::new();
        while self.at(Tok::Elif) {
            let elif_start = self.start();
            self.bump();
            let (test, _) = self.parse_named_test()?;
            self.expect(Tok::Colon)?;
            elifs.push((elif_start, test, self.parse_suite()?));
        }
        let mut orelse = self.parse_else()?;
        let end = orelse
            .last()
            .or_else(|| elifs.last().and_then(|elif| elif.2.last()))
            .or_else(|| body.last())
            .map(Ranged::end)
            .unwrap_or_default();
        for (elif_start, test, body) in elifs.into_iter().rev() {
            orelse = vec![ast::Stmt::If(ast::StmtIf {
                test: Box::new(test),
                body,
                orelse,
                range: range(elif_start, end),
            })];
        }
        Ok(ast::Stmt::If(ast::StmtIf {
            test: Box::new(test),
            body,
            orelse,
            range: range(start, end),
        }))
    }

    /// An optional `else: Suite`.
    fn parse_else(&mut self) -> PResult<Vec<ast::Stmt>> {
        if !self.at(Tok::Else) {
            return Ok(Vec::new());
        }
        self.bump();
        self.expect(Tok::Colon)?;
        self.parse_suite()
    }

    fn parse_while(&mut self) -> PResult<ast::Stmt> {
        let start = self.start();
        self.bump();
        let (test, _) = self.parse_named_test()?;
        self.expect(Tok::Colon)?;
        let body = self.parse_suite()?;
        let orelse = self.parse_else()?;
        let end = body_end(if orelse.is_empty() { &body } else { &orelse });
        Ok(ast::Stmt::While(ast::StmtWhile {
            test: Box::new(test),
            body,
            orelse,
            range: range(start, end),
        }))
    }

    /// `[async] for`; `start` is where `async` (or `for`) begins.
    fn parse_for(&mut self, start: TextSize, is_async: bool) -> PResult<ast::Stmt> {
        self.bump(); // `for`
        let (target, _) = self.parse_list(false)?;
        self.expect(Tok::In)?;
        let (iter, _) = self.parse_list(true)?;
        self.expect(Tok::Colon)?;
        let body = self.parse_suite()?;
        let orelse = self.parse_else()?;
        let end = body_end(if orelse.is_empty() { &body } else { &orelse });
        let target = Box::new(set_context(target, ast::ExprContext::Store));
        let iter = Box::new(iter);
        let range = range(start, end);
        Ok(if is_async {
            ast::Stmt::AsyncFor(ast::StmtAsyncFor {
                target,
                iter,
                body,
                orelse,
                type_comment: None,
                range,
            })
        } else {
            ast::Stmt::For(ast::StmtFor {
                target,
                iter,
                body,
                orelse,
                type_comment: None,
                range,
            })
        })
    }

    fn parse_try(&mut self) -> PResult<ast::Stmt> {
        let start = self.start();
        self.bump();
        self.expect(Tok::Colon)?;
        let body = self.parse_suite()?;
        if self.at(Tok::Finally) {
            self.bump();
            self.expect(Tok::Colon)?;
            let finalbody = self.parse_suite()?;
            let end = body_end(&finalbody);
            return Ok(ast::Stmt::Try(ast::StmtTry {
                body,
                handlers: Vec::new(),
                orelse: Vec::new(),
                finalbody,
                range: range(start, end),
            }));
        }
        if !self.at(Tok::Except) {
            return Err(self.unexpected());
        }
        let star = matches!(self.peek(1), Some(Tok::Star));
        let mut handlers = Vec::new();
        while self.at(Tok::Except) {
            let handler_start = self.start();
            self.bump();
            let type_ = if star {
                self.expect(Tok::Star)?;
                Some(self.parse_test()?)
            } else if starts_test(&self.tok) {
                Some(self.parse_test()?)
            } else {
                None
            };
            let name = if type_.is_some() && self.at(Tok::As) {
                self.bump();
                Some(self.identifier()?)
            } else {
                None
            };
            self.expect(Tok::Colon)?;
            let body = self.parse_suite()?;
            let end = body_end(&body);
            handlers.push(ast::ExceptHandler::ExceptHandler(
                ast::ExceptHandlerExceptHandler {
                    type_: type_.map(Box::new),
                    name,
                    body,
                    range: range(handler_start, end),
                },
            ));
        }
        let orelse = self.parse_else()?;
        let finalbody = if self.at(Tok::Finally) {
            self.bump();
            self.expect(Tok::Colon)?;
            self.parse_suite()?
        } else {
            Vec::new()
        };
        let end = finalbody
            .last()
            .or_else(|| orelse.last())
            .map(Ranged::end)
            .or_else(|| handlers.last().map(Ranged::end))
            .unwrap_or_default();
        let range = range(start, end);
        Ok(if star {
            ast::Stmt::TryStar(ast::StmtTryStar {
                body,
                handlers,
                orelse,
                finalbody,
                range,
            })
        } else {
            ast::Stmt::Try(ast::StmtTry {
                body,
                handlers,
                orelse,
                finalbody,
                range,
            })
        })
    }

    /// `[async] with`; `start` is where `async` (or `with`) begins.
    fn parse_with(&mut self, start: TextSize, is_async: bool) -> PResult<ast::Stmt> {
        self.bump(); // `with`
        let items = if self.at(Tok::Lpar) {
            self.parse_with_items_parenthesized()?
        } else {
            self.parse_with_items()?
        };
        self.expect(Tok::Colon)?;
        let body = self.parse_suite()?;
        let range = range(start, body_end(&body));
        Ok(if is_async {
            ast::StmtAsyncWith {
                items,
                body,
                type_comment: None,
                range,
            }
            .into()
        } else {
            ast::StmtWith {
                items,
                body,
                type_comment: None,
                range,
            }
            .into()
        })
    }

    /// `with` items that start with `(`: first as parenthesized items
    /// (`with (a as b, c):`), else as expressions (`with (a, b) as c:`);
    /// when both fail, the failure that got further is the LR parser's.
    fn parse_with_items_parenthesized(&mut self) -> PResult<Vec<ast::WithItem>> {
        let mark = self.mark();
        match self.try_parenthesized_with_items() {
            Ok(items) => {
                self.release(mark);
                Ok(items)
            }
            Err(first) => {
                self.rewind(mark);
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
    fn try_parenthesized_with_items(&mut self) -> PResult<Vec<ast::WithItem>> {
        self.bump(); // `(`
        let mut items = Vec::new();
        let mut spans = Vec::new();
        loop {
            let item_start = self.start();
            let item = self.parse_with_item()?;
            spans.push((item_start, self.prev_end, item.optional_vars.is_some()));
            items.push(item);
            if !self.at(Tok::Comma) {
                break;
            }
            self.bump();
            if self.at(Tok::Rpar) {
                break;
            }
        }
        self.expect(Tok::Rpar)?;
        if !self.at(Tok::Colon) {
            return Err(self.unexpected());
        }
        // The leading items without `as` are one `WithItemsNoAs` list, and
        // share its range.
        let plain = spans.iter().take_while(|span| !span.2).count();
        if plain > 0 {
            let shared = optional_range(spans[0].0, spans[plain - 1].1);
            for item in &mut items[..plain] {
                item.range = shared;
            }
        }
        Ok(items)
    }

    /// Comma-separated `Test ["as" Expression]` items, no trailing comma.
    fn parse_with_items(&mut self) -> PResult<Vec<ast::WithItem>> {
        self.with_item_start = self.index;
        let mut items = vec![self.parse_with_item()?];
        while self.at(Tok::Comma) {
            self.bump();
            items.push(self.parse_with_item()?);
        }
        Ok(items)
    }

    fn parse_with_item(&mut self) -> PResult<ast::WithItem> {
        let start = self.start();
        let context_expr = self.parse_test()?;
        let optional_vars = if self.at(Tok::As) {
            self.bump();
            let vars = self.parse_expression()?;
            Some(Box::new(set_context(vars, ast::ExprContext::Store)))
        } else {
            None
        };
        Ok(ast::WithItem {
            context_expr,
            optional_vars,
            range: optional_range(start, self.prev_end),
        })
    }

    fn parse_decorated(&mut self) -> PResult<ast::Stmt> {
        let mut decorators = Vec::new();
        while self.at(Tok::At) {
            self.bump();
            let (decorator, _) = self.parse_named_test()?;
            self.expect(Tok::Newline)?;
            decorators.push(decorator);
        }
        match self.tok {
            Tok::Def => self.parse_function_def(decorators),
            Tok::Class => self.parse_class_def(decorators),
            Tok::Async => {
                let start = self.start();
                self.bump();
                if !self.at(Tok::Def) {
                    return Err(self.unexpected());
                }
                self.parse_function_def_at(start, true, decorators)
            }
            _ => Err(self.unexpected()),
        }
    }

    fn parse_function_def(&mut self, decorator_list: Vec<ast::Expr>) -> PResult<ast::Stmt> {
        self.parse_function_def_at(self.start(), false, decorator_list)
    }

    /// `def` (current token); `start` is where `async` (or `def`) begins.
    fn parse_function_def_at(
        &mut self,
        start: TextSize,
        is_async: bool,
        decorator_list: Vec<ast::Expr>,
    ) -> PResult<ast::Stmt> {
        self.bump(); // `def`
        let name = self.identifier()?;
        let type_params = if self.at(Tok::Lsqb) {
            self.parse_type_params()?
        } else {
            Vec::new()
        };
        let params_start = self.start();
        self.expect(Tok::Lpar)?;
        let params = if self.at(Tok::Rpar) {
            None
        } else {
            Some(self.parse_parameter_list(true)?)
        };
        self.expect(Tok::Rpar)?;
        if let Some(params) = &params {
            validate_arguments(params)
                .map_err(|error| self.reduce_error(error, Follow::Parameters))?;
        }
        let args =
            Box::new(params.unwrap_or_else(|| {
                ast::Arguments::empty(optional_range(params_start, self.prev_end))
            }));
        let returns = if self.at(Tok::Rarrow) {
            self.bump();
            Some(Box::new(self.parse_test()?))
        } else {
            None
        };
        self.expect(Tok::Colon)?;
        let body = self.parse_suite()?;
        let range = range(start, body_end(&body));
        Ok(if is_async {
            ast::StmtAsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_comment: None,
                type_params,
                range,
            }
            .into()
        } else {
            ast::StmtFunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_comment: None,
                type_params,
                range,
            }
            .into()
        })
    }

    fn parse_class_def(&mut self, decorator_list: Vec<ast::Expr>) -> PResult<ast::Stmt> {
        let start = self.start();
        self.bump();
        let name = self.identifier()?;
        let type_params = if self.at(Tok::Lsqb) {
            self.parse_type_params()?
        } else {
            Vec::new()
        };
        let (bases, keywords) = if self.at(Tok::Lpar) {
            self.bump();
            let arguments = self.parse_call_args()?;
            self.bump(); // `)`
            (arguments.args, arguments.keywords)
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(Tok::Colon)?;
        let body = self.parse_suite()?;
        Ok(ast::Stmt::ClassDef(ast::StmtClassDef {
            name,
            bases,
            keywords,
            range: range(start, body_end(&body)),
            body,
            decorator_list,
            type_params,
        }))
    }

    /// `TypeParamList`: `[T, *Ts, **P, U: bound]`.
    fn parse_type_params(&mut self) -> PResult<Vec<ast::TypeParam>> {
        self.bump(); // `[`
        let mut params = Vec::new();
        loop {
            let start = self.start();
            let param = match self.tok {
                Tok::Star => {
                    self.bump();
                    let name = self.identifier()?;
                    ast::TypeParam::TypeVarTuple(ast::TypeParamTypeVarTuple {
                        name,
                        range: range(start, self.prev_end),
                    })
                }
                Tok::DoubleStar => {
                    self.bump();
                    let name = self.identifier()?;
                    ast::TypeParam::ParamSpec(ast::TypeParamParamSpec {
                        name,
                        range: range(start, self.prev_end),
                    })
                }
                _ => {
                    let name = self.identifier()?;
                    let bound = if self.at(Tok::Colon) {
                        self.bump();
                        Some(Box::new(self.parse_test()?))
                    } else {
                        None
                    };
                    ast::TypeParam::TypeVar(ast::TypeParamTypeVar {
                        name,
                        bound,
                        range: range(start, self.prev_end),
                    })
                }
            };
            params.push(param);
            if !self.at(Tok::Comma) {
                break;
            }
            self.bump();
            if self.at(Tok::Rsqb) {
                break;
            }
        }
        self.expect(Tok::Rsqb)?;
        Ok(params)
    }
}
