//! Statements, and the program entry point.

use super::ast::*;
use super::parser::{BindKind, PResult, ParseOptions, Parser, ScopeKind, SyntaxError};
use super::token::{Keyword, Punct, StrVal, TokenKind};

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

    /// `import` / `export`, legal only at a module's top level.
    ///
    /// `import` is also an EXPRESSION (`import(spec)`, `import.meta`), so the
    /// declaration form is only taken when the next token cannot continue one.
    fn parse_module_decl(&mut self) -> PResult<Option<Stmt>> {
        if self.goal != Goal::Module {
            return Ok(None);
        }
        if self.at_kw(Keyword::Import) {
            let save = self.save();
            let pos = self.cur().span.start;
            self.bump_after_operand()?;
            // `import(` and `import.` are expressions, not declarations.
            if self.at(Punct::LParen) || self.at(Punct::Dot) {
                self.restore(save);
                return Ok(None);
            }
            return Ok(Some(Stmt::Import(Box::new(self.parse_import_rest(pos)?))));
        }
        if self.at_kw(Keyword::Export) {
            let pos = self.cur().span.start;
            self.bump_after_operand()?;
            return Ok(Some(Stmt::Export(Box::new(self.parse_export_rest(pos)?))));
        }
        Ok(None)
    }

    fn parse_import_rest(&mut self, pos: u32) -> PResult<ImportDecl> {
        // `import "m"` — side effect only.
        if let TokenKind::Str(s) = self.cur().kind.clone() {
            self.bump_after_operand()?;
            let attributes = self.parse_import_attributes()?;
            self.semicolon()?;
            return Ok(ImportDecl {
                specifiers: Vec::new(),
                source: s,
                attributes,
                phase: ImportPhase::Evaluation,
            });
        }
        let mut specifiers = Vec::new();
        // `import d from`, `import d, {…} from`, `import d, * as ns from`
        if self.is_binding_ident() {
            let (n, p) = self.binding_ident()?;
            self.declare(&n.clone(), BindKind::Import, p)?;
            specifiers.push(ImportSpecifier::Default(n));
            if self.eat(Punct::Comma, false)? {
                self.parse_import_bindings(&mut specifiers)?;
            }
        } else {
            self.parse_import_bindings(&mut specifiers)?;
        }
        if !self.eat_kw(Keyword::From, false)? {
            return Err(SyntaxError::new("SyntaxError: expected 'from'", pos));
        }
        let TokenKind::Str(source) = self.cur().kind.clone() else {
            return Err(self.err_here("SyntaxError: expected a module specifier string"));
        };
        self.bump_after_operand()?;
        let attributes = self.parse_import_attributes()?;
        self.semicolon()?;
        Ok(ImportDecl { specifiers, source, attributes, phase: ImportPhase::Evaluation })
    }

    /// `* as ns` or `{ a, b as c }`.
    fn parse_import_bindings(&mut self, out: &mut Vec<ImportSpecifier>) -> PResult<()> {
        if self.eat(Punct::Star, false)? {
            if !self.eat_kw(Keyword::As, false)? {
                return Err(self.err_here("SyntaxError: expected 'as' after '*'"));
            }
            let (n, p) = self.binding_ident()?;
            self.declare(&n.clone(), BindKind::Import, p)?;
            out.push(ImportSpecifier::Namespace(n));
            return Ok(());
        }
        self.expect(Punct::LBrace, false)?;
        while !self.at(Punct::RBrace) {
            let imported = self.parse_module_export_name()?;
            let (local, lpos) = if self.eat_kw(Keyword::As, false)? {
                self.binding_ident()?
            } else {
                // Without `as`, the imported name must itself be a valid
                // binding, so a string name or a reserved word is an error.
                let ModuleExportName::Ident(n) = &imported else {
                    return Err(self.err_here("SyntaxError: a string module name requires 'as'"));
                };
                (n.clone(), self.cur().span.start)
            };
            self.declare(&local.clone(), BindKind::Import, lpos)?;
            out.push(ImportSpecifier::Named { imported, local });
            if !self.eat(Punct::Comma, false)? {
                break;
            }
        }
        self.expect(Punct::RBrace, false)?;
        Ok(())
    }

    /// `with { type: "json" }`. The legacy `assert` spelling is accepted the
    /// same way; the keyword itself is not recorded because nothing consumes it.
    fn parse_import_attributes(&mut self) -> PResult<Vec<ImportAttribute>> {
        let is_with = self.at_kw(Keyword::With);
        let is_assert =
            matches!(&self.cur().kind, TokenKind::Ident { name, .. } if name == "assert");
        if !is_with && !is_assert {
            return Ok(Vec::new());
        }
        // A LineTerminator before `assert` means ASI, not an attribute clause.
        if is_assert && self.cur().newline_before {
            return Ok(Vec::new());
        }
        self.bump_after_operand()?;
        self.expect(Punct::LBrace, false)?;
        let mut out = Vec::new();
        while !self.at(Punct::RBrace) {
            let key = match self.cur().kind.clone() {
                TokenKind::Str(s) => {
                    self.bump_after_operand()?;
                    s
                }
                TokenKind::Ident { .. } => StrVal::Utf8(self.ident_name()?.to_string()),
                _ => return Err(self.err_here("SyntaxError: expected an attribute key")),
            };
            self.expect(Punct::Colon, false)?;
            let TokenKind::Str(value) = self.cur().kind.clone() else {
                return Err(
                    self.err_here("SyntaxError: an import attribute value must be a string")
                );
            };
            self.bump_after_operand()?;
            out.push(ImportAttribute { key, value });
            if !self.eat(Punct::Comma, false)? {
                break;
            }
        }
        self.expect(Punct::RBrace, false)?;
        Ok(out)
    }

    fn parse_module_export_name(&mut self) -> PResult<ModuleExportName> {
        if let TokenKind::Str(s) = self.cur().kind.clone() {
            self.bump_after_operand()?;
            return Ok(ModuleExportName::Str(s));
        }
        // Reserved words are legal here: `export { default as d }`.
        Ok(ModuleExportName::Ident(self.ident_name()?))
    }

    fn parse_export_rest(&mut self, pos: u32) -> PResult<ExportDecl> {
        // `export * from "m"` / `export * as ns from "m"`
        if self.eat(Punct::Star, false)? {
            let alias = if self.eat_kw(Keyword::As, false)? {
                Some(self.parse_module_export_name()?)
            } else {
                None
            };
            if !self.eat_kw(Keyword::From, false)? {
                return Err(SyntaxError::new("SyntaxError: expected 'from'", pos));
            }
            let TokenKind::Str(source) = self.cur().kind.clone() else {
                return Err(self.err_here("SyntaxError: expected a module specifier string"));
            };
            self.bump_after_operand()?;
            let attributes = self.parse_import_attributes()?;
            self.semicolon()?;
            return Ok(ExportDecl::All { alias, source, attributes });
        }
        // `export default …`
        if self.at_kw(Keyword::Default) {
            let start = self.cur().span.start;
            self.bump_before_operand()?;
            if self.at_kw(Keyword::Function) {
                self.bump_after_operand()?;
                let f = self.parse_function_rest(false, start)?;
                return Ok(ExportDecl::Default(ExportDefault::Function(Box::new(f))));
            }
            if self.at_kw(Keyword::Async) {
                let save = self.save();
                self.bump_after_operand()?;
                if self.at_kw(Keyword::Function) && !self.cur().newline_before {
                    self.bump_after_operand()?;
                    let f = self.parse_function_rest(true, start)?;
                    return Ok(ExportDecl::Default(ExportDefault::Function(Box::new(f))));
                }
                self.restore(save);
            }
            if self.at_kw(Keyword::Class) {
                self.bump_after_operand()?;
                let c = self.parse_class_rest(start)?;
                return Ok(ExportDecl::Default(ExportDefault::Class(Box::new(c))));
            }
            let e = self.parse_assign_full()?;
            self.semicolon()?;
            return Ok(ExportDecl::Default(ExportDefault::Expr(e)));
        }
        // `export { a, b as c }` / `export { … } from "m"`
        if self.at(Punct::LBrace) {
            self.bump_after_operand()?;
            let mut specifiers = Vec::new();
            while !self.at(Punct::RBrace) {
                let local = self.parse_module_export_name()?;
                let exported = if self.eat_kw(Keyword::As, false)? {
                    self.parse_module_export_name()?
                } else {
                    local.clone()
                };
                specifiers.push(ExportSpecifier { local, exported });
                if !self.eat(Punct::Comma, false)? {
                    break;
                }
            }
            self.expect(Punct::RBrace, false)?;
            let source = if self.eat_kw(Keyword::From, false)? {
                let TokenKind::Str(s) = self.cur().kind.clone() else {
                    return Err(self.err_here("SyntaxError: expected a module specifier string"));
                };
                self.bump_after_operand()?;
                Some(s)
            } else {
                None
            };
            let attributes = self.parse_import_attributes()?;
            self.semicolon()?;
            return Ok(ExportDecl::Named { specifiers, source, attributes });
        }
        // `export var/let/const/function/class …` — the declaration binds
        // exactly as it would without the `export`.
        let decl = self.parse_stmt_list_item()?;
        Ok(ExportDecl::Decl(Box::new(decl)))
    }

    /// A StatementListItem: a Statement, or a declaration. Declarations are
    /// legal here and NOT in the Statement-only positions (an `if` body, a loop
    /// body), which is why the two entry points are distinct.
    pub(crate) fn parse_stmt_list_item(&mut self) -> PResult<Stmt> {
        if let Some(m) = self.parse_module_decl()? {
            return Ok(m);
        }
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
        // `using x = …` — explicit resource management. `using` is contextual:
        // it is a declaration only when followed, on the SAME line, by a plain
        // binding identifier (patterns are not allowed in using declarations),
        // so `using = 5` and a line-broken `using x` stay ordinary expressions.
        if matches!(&self.cur().kind, TokenKind::Ident { name, kw: Keyword::None, had_escape: false, private: false } if name == "using")
        {
            let save = self.save();
            self.bump_after_operand()?;
            if !self.cur().newline_before && self.is_binding_ident() {
                return Ok(Some(VarKind::Using));
            }
            self.restore(save);
        }
        // `await using x = …`, in contexts where `await` is a keyword.
        if self.ctx.await_ && self.at_kw(Keyword::Await) {
            let save = self.save();
            self.bump_after_operand()?;
            let is_using = !self.cur().newline_before
                && matches!(&self.cur().kind, TokenKind::Ident { name, kw: Keyword::None, had_escape: false, private: false } if name == "using");
            if is_using {
                self.bump_after_operand()?;
                if !self.cur().newline_before && self.is_binding_ident() {
                    return Ok(Some(VarKind::AwaitUsing));
                }
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
            let init = if self.eat(Punct::Eq, true)? { Some(self.parse_assign_full()?) } else { None };
            // `using` requires an initializer wherever it appears.
            if init.is_none() && matches!(kind, VarKind::Using | VarKind::AwaitUsing) {
                return Err(SyntaxError::new(
                    "SyntaxError: missing initializer in using declaration",
                    pos,
                ));
            }
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
                        Some(self.parse_expr_full()?)
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
                    let e = self.parse_expr_full()?;
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
                    let object = self.parse_expr_full()?;
                    self.expect(Punct::RParen, true)?;
                    let body = self.parse_stmt()?;
                    Ok(Stmt::With { object, body: Box::new(body) })
                }
<<<<<<< Updated upstream
                // `var` is a VariableStatement, which IS a Statement, so it is
                // legal as the unbraced body of `if`/`else`/`while`/`for`/`do`.
                // Only `let`/`const`/`class`/`function` are Declarations and
                // barred here.
                //
                // Falling through to the expression parser instead rejected
                // `if (x) var y = 1;` as "unexpected reserved word". Minifiers
                // emit that shape constantly — dropping the braces is one of
                // their standard size wins — so this took out every real-world
                // bundle, React's included.
=======
                // A VariableStatement IS a Statement — `if (c) var x = 1;` is
                // legal, and transpiled code produces it constantly. Only the
                // LEXICAL declarations are restricted to StatementListItem.
>>>>>>> Stashed changes
                Keyword::Var => {
                    self.bump_after_operand()?;
                    let d = self.parse_var_decl(VarKind::Var)?;
                    self.semicolon()?;
                    Ok(Stmt::VarDecl(d))
                }
<<<<<<< Updated upstream
                // A declaration is NOT a Statement, so it is an error here even
                // though it is fine as a StatementListItem.
=======
                // Annex B B.3.4: a function DECLARATION as a bare statement
                // body (`if (x) function f() {}`, `l: function f() {}`) is
                // web-compat sloppy-mode legality. Strict code keeps the error.
                Keyword::Function if !self.ctx.strict => {
                    self.bump_after_operand()?;
                    let f = self.parse_function_rest(false, start)?;
                    if let Some(n) = &f.name {
                        self.declare(&n.clone(), BindKind::Function, start)?;
                    }
                    Ok(Stmt::FnDecl(Box::new(f)))
                }
>>>>>>> Stashed changes
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
        // `foo:` — an identifier followed by a colon. Read PERMISSIVELY: a
        // label is not a binding, so `eval: …` is legal even in strict mode,
        // and a statement that merely STARTS with `arguments` (say
        // `arguments.length;`) must not be rejected while probing for one.
        if self.is_binding_ident() {
            let save = self.save();
            let tok = self.bump_after_operand()?;
            let TokenKind::Ident { name, .. } = tok.kind else { unreachable!() };
            let name = name.into_boxed_str();
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
        let e = self.parse_expr_full()?;
        self.semicolon()?;
        Ok(Stmt::Expr(e))
    }

    fn parse_if(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        self.expect(Punct::LParen, true)?;
        let test = self.parse_expr_full()?;
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
        let test = self.parse_expr_full()?;
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
        let test = self.parse_expr_full()?;
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
                let right = if of { self.parse_assign_full()? } else { self.parse_expr_full()? };
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
                let right = if of { self.parse_assign_full()? } else { self.parse_expr_full()? };
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
        let test = if self.at(Punct::Semi) { None } else { Some(self.parse_expr_full()?) };
        self.expect(Punct::Semi, true)?;
        let update = if self.at(Punct::RParen) { None } else { Some(self.parse_expr_full()?) };
        self.expect(Punct::RParen, true)?;
        let body = Box::new(self.in_loop(|p| p.parse_stmt())?);
        Ok(Stmt::For { init, test, update, body })
    }

    fn parse_switch(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        self.expect(Punct::LParen, true)?;
        let disc = self.parse_expr_full()?;
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
                Some(self.parse_expr_full()?)
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
