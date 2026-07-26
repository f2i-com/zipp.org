//! Statements, and the program entry point.

use super::ast::*;
use super::parser::{BindKind, PResult, ParseOptions, Parser, ScopeKind, SyntaxError};
use super::token::{Keyword, Punct, TokenKind};

/// Parse a complete program.
pub fn parse(src: &str, opts: ParseOptions) -> PResult<Program> {
    let goal = opts.goal;
    let mut p = Parser::new(src, opts)?;
    p.parse_program(goal)
}

impl<'s> Parser<'s> {
    pub(crate) fn parse_program(&mut self, goal: Goal) -> PResult<Program> {
        let (directives, strict) = self.directive_prologue()?;
        if strict {
            self.ctx.strict = true;
        }
        self.scopes
            .push(if goal == Goal::Module { ScopeKind::Module } else { ScopeKind::Script });
        let mut body = Vec::new();
        while !self.at_eof() {
            body.push(self.parse_stmt_list_item()?);
        }
        self.scopes.pop()?;
        Ok(Program { goal, strict: self.ctx.strict, directives, body })
    }

    /// A StatementListItem: a Statement, or a declaration. Declarations are
    /// legal here and NOT in the Statement-only positions (an `if` body, a loop
    /// body), which is why the two entry points are distinct.
    pub(crate) fn parse_stmt_list_item(&mut self) -> PResult<Stmt> {
        if self.at_kw(Keyword::Function) {
            let start = self.cur().span.start;
            self.bump_after_operand()?;
            let f = self.parse_function_rest(false, start)?;
            if let Some(n) = &f.name {
                self.declare(&n.clone(), BindKind::Function, start)?;
            }
            return Ok(Stmt::FnDecl(Box::new(f)));
        }
        if self.at_kw(Keyword::Async) {
            let save = self.save();
            let start = self.cur().span.start;
            self.bump_after_operand()?;
            // `async function` only when no LineTerminator intervenes.
            if self.at_kw(Keyword::Function) && !self.cur().newline_before {
                self.bump_after_operand()?;
                let f = self.parse_function_rest(true, start)?;
                if let Some(n) = &f.name {
                    self.declare(&n.clone(), BindKind::Function, start)?;
                }
                return Ok(Stmt::FnDecl(Box::new(f)));
            }
            self.restore(save);
        }
        if self.at_kw(Keyword::Class) {
            let start = self.cur().span.start;
            self.bump_after_operand()?;
            let c = self.parse_class_rest(start)?;
            if let Some(n) = &c.name {
                self.declare(&n.clone(), BindKind::Class, start)?;
            }
            return Ok(Stmt::ClassDecl(Box::new(c)));
        }
        if let Some(kind) = self.at_var_decl_start()? {
            let d = self.parse_var_decl(kind)?;
            self.semicolon()?;
            return Ok(Stmt::VarDecl(d));
        }
        self.parse_stmt()
    }

    /// Which declaration keyword starts here, if any.
    ///
    /// `let` is the awkward one: it is a declaration only when followed by `[`,
    /// `{`, or an identifier. `let` alone, `let.a`, `let(x)` are the sloppy-mode
    /// IDENTIFIER `let`. `let [` is ALWAYS a declaration, even as a statement
    /// body, because the alternative reading is banned.
    fn at_var_decl_start(&mut self) -> PResult<Option<VarKind>> {
        if self.at_kw(Keyword::Var) {
            self.bump_after_operand()?;
            return Ok(Some(VarKind::Var));
        }
        if self.at_kw(Keyword::Const) {
            self.bump_after_operand()?;
            return Ok(Some(VarKind::Const));
        }
        if self.at_kw(Keyword::Let) {
            let save = self.save();
            self.bump_after_operand()?;
            let is_decl = self.at(Punct::LBracket)
                || self.at(Punct::LBrace)
                || self.is_binding_ident();
            if is_decl {
                return Ok(Some(VarKind::Let));
            }
            self.restore(save);
        }
        Ok(None)
    }

    pub(crate) fn parse_var_decl(&mut self, kind: VarKind) -> PResult<VarDecl> {
        let mut decls = Vec::new();
        loop {
            let pos = self.cur().span.start;
            let id = self.parse_binding_pattern()?;
            self.declare_pattern(&id, kind, pos)?;
            let init = if self.eat(Punct::Eq, true)? { Some(self.parse_assign()?) } else { None };
            // `const` without an initializer is an early error — except in a
            // for-in/of head, which parses through a different path.
            if init.is_none() && kind == VarKind::Const {
                return Err(SyntaxError::new(
                    "SyntaxError: missing initializer in const declaration",
                    pos,
                ));
            }
            // A destructuring declaration must be initialized.
            if init.is_none() && !matches!(id, Pattern::Ident(_)) {
                return Err(SyntaxError::new(
                    "SyntaxError: missing initializer in destructuring declaration",
                    pos,
                ));
            }
            decls.push(Declarator { id, init });
            if !self.eat(Punct::Comma, true)? {
                break;
            }
        }
        Ok(VarDecl { kind, decls })
    }

    /// Declare every name a pattern binds.
    pub(crate) fn declare_pattern(
        &mut self,
        pat: &Pattern,
        kind: VarKind,
        pos: u32,
    ) -> PResult<()> {
        let bk = match kind {
            VarKind::Var => BindKind::Var,
            VarKind::Const => BindKind::Const,
            _ => BindKind::Let,
        };
        let mut names = Vec::new();
        collect_pattern_names(pat, &mut names);
        for n in names {
            // `let let` is always an error, in either mode.
            if bk.is_lexical_kind() && &*n == "let" {
                return Err(SyntaxError::new(
                    "SyntaxError: 'let' is not a valid name for a lexical declaration",
                    pos,
                ));
            }
            self.declare(&n, bk, pos)?;
        }
        Ok(())
    }

    // ---- statements --------------------------------------------------------

    pub(crate) fn parse_stmt(&mut self) -> PResult<Stmt> {
        let start = self.cur().span.start;
        match self.cur().kind.clone() {
            TokenKind::Punct(Punct::LBrace) => {
                self.bump_before_operand()?;
                self.scopes.push(ScopeKind::Block);
                let mut body = Vec::new();
                while !self.at(Punct::RBrace) && !self.at_eof() {
                    body.push(self.parse_stmt_list_item()?);
                }
                self.scopes.pop()?;
                self.expect(Punct::RBrace, false)?;
                Ok(Stmt::Block(body))
            }
            TokenKind::Punct(Punct::Semi) => {
                self.bump_before_operand()?;
                Ok(Stmt::Empty)
            }
            TokenKind::Ident { kw, had_escape: false, private: false, .. } => match kw {
                Keyword::If => self.parse_if(),
                Keyword::While => self.parse_while(),
                Keyword::Do => self.parse_do_while(),
                Keyword::For => self.parse_for(),
                Keyword::Switch => self.parse_switch(),
                Keyword::Try => self.parse_try(),
                Keyword::Return => {
                    if !self.ctx.return_ {
                        return Err(SyntaxError::new(
                            "SyntaxError: 'return' outside of a function",
                            start,
                        ));
                    }
                    self.bump_before_operand()?;
                    // Restricted production: a LineTerminator ends it.
                    let arg = if self.at(Punct::Semi)
                        || self.at(Punct::RBrace)
                        || self.at_eof()
                        || self.cur().newline_before
                    {
                        None
                    } else {
                        Some(self.parse_expr()?)
                    };
                    self.semicolon()?;
                    Ok(Stmt::Return(arg))
                }
                Keyword::Throw => {
                    self.bump_before_operand()?;
                    if self.cur().newline_before {
                        return Err(self
                            .err_here("SyntaxError: no line break allowed after 'throw'"));
                    }
                    let e = self.parse_expr()?;
                    self.semicolon()?;
                    Ok(Stmt::Throw(e))
                }
                Keyword::Break | Keyword::Continue => {
                    let is_break = kw == Keyword::Break;
                    self.bump_before_operand()?;
                    let label = if self.is_binding_ident() && !self.cur().newline_before {
                        let (n, pos) = self.binding_ident()?;
                        if !self.labels.iter().any(|l| **l == *n) {
                            return Err(SyntaxError::new(
                                format!("SyntaxError: undefined label '{n}'"),
                                pos,
                            ));
                        }
                        Some(n)
                    } else {
                        if is_break && !self.ctx.in_loop && !self.ctx.in_switch {
                            return Err(SyntaxError::new(
                                "SyntaxError: illegal break statement",
                                start,
                            ));
                        }
                        if !is_break && !self.ctx.in_loop {
                            return Err(SyntaxError::new(
                                "SyntaxError: illegal continue statement",
                                start,
                            ));
                        }
                        None
                    };
                    self.semicolon()?;
                    Ok(if is_break { Stmt::Break(label) } else { Stmt::Continue(label) })
                }
                Keyword::Debugger => {
                    self.bump_before_operand()?;
                    self.semicolon()?;
                    Ok(Stmt::Debugger)
                }
                Keyword::With => {
                    if self.ctx.strict {
                        return Err(SyntaxError::new(
                            "SyntaxError: 'with' is not allowed in strict mode",
                            start,
                        ));
                    }
                    self.bump_before_operand()?;
                    self.expect(Punct::LParen, true)?;
                    let object = self.parse_expr()?;
                    self.expect(Punct::RParen, true)?;
                    let body = self.parse_stmt()?;
                    Ok(Stmt::With { object, body: Box::new(body) })
                }
                // A declaration is NOT a Statement, so it is an error here even
                // though it is fine as a StatementListItem.
                Keyword::Function | Keyword::Class => Err(SyntaxError::new(
                    "SyntaxError: declaration not allowed in this position",
                    start,
                )),
                _ => self.parse_expr_or_labeled(),
            },
            _ => self.parse_expr_or_labeled(),
        }
    }

    /// An expression statement, or a labelled statement.
    fn parse_expr_or_labeled(&mut self) -> PResult<Stmt> {
        let start = self.cur().span.start;
        // `foo:` — an identifier followed by a colon.
        if self.is_binding_ident() {
            let save = self.save();
            let (name, _) = self.binding_ident()?;
            if self.at(Punct::Colon) {
                self.bump_before_operand()?;
                if self.labels.iter().any(|l| **l == *name) {
                    return Err(SyntaxError::new(
                        format!("SyntaxError: label '{name}' has already been declared"),
                        start,
                    ));
                }
                self.labels.push(name.clone());
                let body = self.parse_stmt()?;
                self.labels.pop();
                return Ok(Stmt::Labeled { label: name, body: Box::new(body) });
            }
            self.restore(save);
        }
        let e = self.parse_expr()?;
        self.semicolon()?;
        Ok(Stmt::Expr(e))
    }

    fn parse_if(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        self.expect(Punct::LParen, true)?;
        let test = self.parse_expr()?;
        self.expect(Punct::RParen, true)?;
        let cons = Box::new(self.parse_stmt()?);
        let alt = if self.eat_kw(Keyword::Else, true)? {
            Some(Box::new(self.parse_stmt()?))
        } else {
            None
        };
        Ok(Stmt::If { test, cons, alt })
    }

    fn parse_while(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        self.expect(Punct::LParen, true)?;
        let test = self.parse_expr()?;
        self.expect(Punct::RParen, true)?;
        let body = Box::new(self.in_loop(|p| p.parse_stmt())?);
        Ok(Stmt::While { test, body })
    }

    fn parse_do_while(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        let body = Box::new(self.in_loop(|p| p.parse_stmt())?);
        if !self.eat_kw(Keyword::While, true)? {
            return Err(self.err_here("SyntaxError: expected 'while'"));
        }
        self.expect(Punct::LParen, true)?;
        let test = self.parse_expr()?;
        self.expect(Punct::RParen, false)?;
        // `do … while (…)` takes an optional semicolon, always.
        let _ = self.eat(Punct::Semi, true)?;
        Ok(Stmt::DoWhile { body, test })
    }

    fn parse_for(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        // `for await (… of …)`
        let is_await = if self.at_kw(Keyword::Await) && self.ctx.await_ {
            self.bump_before_operand()?;
            true
        } else {
            false
        };
        self.expect(Punct::LParen, true)?;
        self.scopes.push(ScopeKind::ForHead);

        // An empty init: `for (;;)`.
        if self.at(Punct::Semi) {
            self.bump_before_operand()?;
            let out = self.parse_for_classic(None)?;
            self.scopes.pop()?;
            return Ok(out);
        }

        // `[In]` is off in the head so a bare `in` reads as the for-in separator.
        let saved_in = self.ctx.in_;
        self.ctx.in_ = false;

        let out = if let Some(kind) = self.at_var_decl_start()? {
            let pos = self.cur().span.start;
            let pat = self.parse_binding_pattern()?;
            self.declare_pattern(&pat, kind, pos)?;
            if self.at_kw(Keyword::In) || self.at_kw(Keyword::Of) {
                let of = self.at_kw(Keyword::Of);
                self.bump_before_operand()?;
                self.ctx.in_ = saved_in;
                let right = if of { self.parse_assign()? } else { self.parse_expr()? };
                self.expect(Punct::RParen, true)?;
                let body = Box::new(self.in_loop(|p| p.parse_stmt())?);
                let left = ForTarget::Var(VarDecl {
                    kind,
                    decls: vec![Declarator { id: pat, init: None }],
                });
                if of {
                    Stmt::ForOf { left, right, body, is_await }
                } else {
                    Stmt::ForIn { left, right, body }
                }
            } else {
                // A classic head that began with a declaration.
                let init = if self.eat(Punct::Eq, true)? {
                    Some(self.parse_assign()?)
                } else if kind == VarKind::Const {
                    return Err(SyntaxError::new(
                        "SyntaxError: missing initializer in const declaration",
                        pos,
                    ));
                } else {
                    None
                };
                let mut decls = vec![Declarator { id: pat, init }];
                while self.eat(Punct::Comma, true)? {
                    let p2 = self.cur().span.start;
                    let id = self.parse_binding_pattern()?;
                    self.declare_pattern(&id, kind, p2)?;
                    let init =
                        if self.eat(Punct::Eq, true)? { Some(self.parse_assign()?) } else { None };
                    decls.push(Declarator { id, init });
                }
                self.ctx.in_ = saved_in;
                self.expect(Punct::Semi, true)?;
                self.parse_for_classic(Some(ForInit::Var(VarDecl { kind, decls })))?
            }
        } else {
            let pos = self.cur().span.start;
            let e = self.parse_expr()?;
            if self.at_kw(Keyword::In) || self.at_kw(Keyword::Of) {
                let of = self.at_kw(Keyword::Of);
                self.bump_before_operand()?;
                self.ctx.in_ = saved_in;
                let target = self.expr_to_target(e, true, pos)?;
                let right = if of { self.parse_assign()? } else { self.parse_expr()? };
                self.expect(Punct::RParen, true)?;
                let body = Box::new(self.in_loop(|p| p.parse_stmt())?);
                let left = ForTarget::Target(target);
                if of {
                    Stmt::ForOf { left, right, body, is_await }
                } else {
                    Stmt::ForIn { left, right, body }
                }
            } else {
                self.ctx.in_ = saved_in;
                self.expect(Punct::Semi, true)?;
                self.parse_for_classic(Some(ForInit::Expr(e)))?
            }
        };
        self.scopes.pop()?;
        Ok(out)
    }

    /// The `;;` form, with the first `;` already consumed.
    fn parse_for_classic(&mut self, init: Option<ForInit>) -> PResult<Stmt> {
        let test = if self.at(Punct::Semi) { None } else { Some(self.parse_expr()?) };
        self.expect(Punct::Semi, true)?;
        let update = if self.at(Punct::RParen) { None } else { Some(self.parse_expr()?) };
        self.expect(Punct::RParen, true)?;
        let body = Box::new(self.in_loop(|p| p.parse_stmt())?);
        Ok(Stmt::For { init, test, update, body })
    }

    fn parse_switch(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        self.expect(Punct::LParen, true)?;
        let disc = self.parse_expr()?;
        self.expect(Punct::RParen, true)?;
        self.expect(Punct::LBrace, true)?;
        // The whole CaseBlock is ONE scope, not one per clause — so
        // `case 1: let a; case 2: let a;` is a duplicate declaration.
        self.scopes.push(ScopeKind::Switch);
        let saved = self.ctx.in_switch;
        self.ctx.in_switch = true;
        let mut cases = Vec::new();
        let mut seen_default = false;
        while !self.at(Punct::RBrace) && !self.at_eof() {
            let test = if self.eat_kw(Keyword::Case, true)? {
                Some(self.parse_expr()?)
            } else if self.at_kw(Keyword::Default) {
                let pos = self.cur().span.start;
                self.bump_before_operand()?;
                if seen_default {
                    return Err(SyntaxError::new(
                        "SyntaxError: more than one default clause in switch statement",
                        pos,
                    ));
                }
                seen_default = true;
                None
            } else {
                return Err(self.err_here("SyntaxError: expected 'case' or 'default'"));
            };
            self.expect(Punct::Colon, true)?;
            let mut body = Vec::new();
            while !self.at(Punct::RBrace)
                && !self.at_kw(Keyword::Case)
                && !self.at_kw(Keyword::Default)
                && !self.at_eof()
            {
                body.push(self.parse_stmt_list_item()?);
            }
            cases.push(SwitchCase { test, body });
        }
        self.ctx.in_switch = saved;
        self.scopes.pop()?;
        self.expect(Punct::RBrace, false)?;
        Ok(Stmt::Switch { disc, cases })
    }

    fn parse_try(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        let block = self.parse_block_body()?;
        let handler = if self.eat_kw(Keyword::Catch, true)? {
            self.scopes.push(ScopeKind::Catch);
            // `catch {}` — the binding is optional.
            let param = if self.eat(Punct::LParen, true)? {
                let pos = self.cur().span.start;
                let p = self.parse_binding_pattern()?;
                // Annex B B.3.5 allows `catch (e) { var e }` only for a SIMPLE
                // parameter, so the scope records which shape it was.
                if let Pattern::Ident(n) = &p {
                    if let Some(sc) = self.scopes.scopes.last_mut() {
                        sc.simple_catch_param = Some(n.clone());
                    }
                }
                self.declare_pattern(&p, VarKind::Let, pos)?;
                self.expect(Punct::RParen, true)?;
                Some(p)
            } else {
                None
            };
            let body = self.parse_block_body()?;
            self.scopes.pop()?;
            Some(CatchClause { param, body })
        } else {
            None
        };
        let finalizer =
            if self.eat_kw(Keyword::Finally, true)? { Some(self.parse_block_body()?) } else { None };
        if handler.is_none() && finalizer.is_none() {
            return Err(self.err_here("SyntaxError: missing catch or finally after try"));
        }
        Ok(Stmt::Try { block, handler, finalizer })
    }

    fn parse_block_body(&mut self) -> PResult<Vec<Stmt>> {
        self.expect(Punct::LBrace, true)?;
        self.scopes.push(ScopeKind::Block);
        let mut out = Vec::new();
        while !self.at(Punct::RBrace) && !self.at_eof() {
            out.push(self.parse_stmt_list_item()?);
        }
        self.scopes.pop()?;
        self.expect(Punct::RBrace, false)?;
        Ok(out)
    }

    /// Run `f` with `in_loop` set, so `break`/`continue` validate.
    fn in_loop<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let saved = self.ctx.in_loop;
        self.ctx.in_loop = true;
        let r = f(self);
        self.ctx.in_loop = saved;
        r
    }
}

impl BindKind {
    pub(crate) fn is_lexical_kind(self) -> bool {
        matches!(self, BindKind::Let | BindKind::Const | BindKind::Class)
    }
}

/// Every name a binding pattern introduces.
pub(crate) fn collect_pattern_names(p: &Pattern, out: &mut Vec<Name>) {
    match p {
        Pattern::Ident(n) => out.push(n.clone()),
        Pattern::Assign { left, .. } => collect_pattern_names(left, out),
        Pattern::Rest(inner) => collect_pattern_names(inner, out),
        Pattern::Array(items) => {
            for it in items.iter().flatten() {
                collect_pattern_names(&it.pat, out);
            }
        }
        Pattern::Object { props, rest } => {
            for pr in props {
                collect_pattern_names(&pr.value, out);
            }
            if let Some(r) = rest {
                collect_pattern_names(r, out);
            }
        }
    }
}
