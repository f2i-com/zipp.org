//! Statements, and the program entry point.

use super::ast::*;
use super::parser::{BindKind, PResult, ParseOptions, Parser, ScopeKind, StmtPos, SyntaxError};
use super::token::{Keyword, Punct, StrVal, TokenKind};

/// Which duplicate rule a function DECLARATION's name plays by. Only a plain
/// `FunctionDeclaration` gets the Annex B B.3.2.4/B.3.2.5 carve-out; a generator
/// or async declaration is a different production and keeps the error.
fn fn_bind_kind(f: &Function) -> BindKind {
    if f.is_generator || f.is_async { BindKind::GenFunction } else { BindKind::Function }
}

/// The spec's `IsLabelledFunction`: a Statement that is a FunctionDeclaration
/// under one or more labels. Recurses, because `l1: l2: function f(){}` is one.
fn is_labelled_function(s: &Stmt) -> bool {
    match s {
        Stmt::FnDecl(_) => true,
        Stmt::Labeled { body, .. } => is_labelled_function(body),
        _ => false,
    }
}

/// Parse a complete program.
pub fn parse(src: &str, opts: ParseOptions) -> PResult<Program> {
    parse_exact(src, None, opts)
}

/// As [`parse`], plus the EXACT WTF-8 bytes `src` is a lossy view of — see
/// [`crate::parse::lexer::Lexer::set_exact_src`].
pub fn parse_exact(src: &str, exact: Option<&[u8]>, opts: ParseOptions) -> PResult<Program> {
    let goal = opts.goal;
    let mut p = Parser::new_exact(src, exact, opts)?;
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
        for (name, pos) in std::mem::take(&mut self.pending_export_locals) {
            if !self.scopes.module_binds(&name) {
                return Err(SyntaxError::new(
                    format!("SyntaxError: export '{name}' is not defined in the module"),
                    pos,
                ));
            }
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
        // `import`/`export` DECLARATIONS are ModuleItems, not StatementListItems,
        // so they are legal only at the module's own top level — `function f() {
        // export default null; }` is a SyntaxError. Declining here sends the
        // token to the expression parser, which rejects the reserved word (and
        // still accepts `import(…)` / `import.meta`, which ARE expressions).
        if !self.scopes.at_module_top_level() {
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
        let phase = self.import_phase_prefix()?;
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
        Ok(ImportDecl { specifiers, source, attributes, phase })
    }

    /// Record one ExportedName, rejecting a duplicate. §16.2.3.1: a module's
    /// ExportedNames must be unique — `export {x}; export {x};` is an error even
    /// though both name the same binding.
    fn record_export_name(&mut self, name: &ModuleExportName, pos: u32) -> PResult<()> {
        let text: Box<str> = match name {
            ModuleExportName::Ident(n) => n.clone(),
            ModuleExportName::Str(s) => s.to_lossy_string().into_boxed_str(),
        };
        if self.exported_names.iter().any(|n| *n == text) {
            return Err(SyntaxError::new(
                format!("SyntaxError: duplicate export name '{text}'"),
                pos,
            ));
        }
        self.exported_names.push(text);
        Ok(())
    }

    /// The current token is the UNESCAPED contextual keyword `want`.
    fn at_contextual(&self, want: &str) -> bool {
        matches!(&self.cur().kind,
            TokenKind::Ident { name, had_escape: false, private: false, .. } if name == want)
    }

    /// Consume a `defer` / `source` phase modifier if that is really what it is;
    /// consume nothing and return `Evaluation` otherwise.
    ///
    /// Both words are CONTEXTUAL and the ambiguity is real:
    ///
    /// ```text
    /// import defer from "m"          — a default binding NAMED `defer`
    /// import defer * as ns from "m"  — the DEFER phase (only a NameSpaceImport
    ///                                  may follow `defer`, so `*` settles it)
    /// import source from "m"         — a default binding NAMED `source`
    /// import source from from "m"    — the SOURCE phase, binding `from`
    /// ```
    ///
    /// so `source` needs two tokens of lookahead: it is the phase only when an
    /// ImportedBinding follows AND `from` follows that. This is the same bounded
    /// rewind `Parser::save`/`restore` already exists for.
    fn import_phase_prefix(&mut self) -> PResult<ImportPhase> {
        let defer = self.at_contextual("defer");
        if !defer && !self.at_contextual("source") {
            return Ok(ImportPhase::Evaluation);
        }
        let save = self.save();
        self.bump_after_operand()?;
        if defer {
            if self.at(Punct::Star) {
                return Ok(ImportPhase::Defer);
            }
        } else if self.is_binding_ident() {
            let after_binding = self.save();
            self.bump_after_operand()?;
            let at_from = self.at_kw(Keyword::From);
            self.restore(after_binding);
            if at_from {
                return Ok(ImportPhase::Source);
            }
        }
        self.restore(save);
        Ok(ImportPhase::Evaluation)
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
                let a = self.parse_module_export_name()?;
                self.record_export_name(&a, pos)?;
                Some(a)
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
            self.bump_before_operand()?;
            self.record_export_name(&ModuleExportName::Ident("default".into()), pos)?;
            // The [[SourceText]] of the exported declaration starts at ITS
            // first token (`function`, `async`, `class`) — `export default`
            // belongs to the export statement, not to the function whose
            // `toString` must reproduce the source.
            if self.at_kw(Keyword::Function) {
                let fstart = self.cur().span.start;
                self.bump_after_operand()?;
                let f = self.parse_function_rest(false, false, fstart)?;
                return Ok(ExportDecl::Default(ExportDefault::Function(Box::new(f))));
            }
            if self.at_kw(Keyword::Async) {
                let save = self.save();
                let fstart = self.cur().span.start;
                self.bump_after_operand()?;
                if self.at_kw(Keyword::Function) && !self.cur().newline_before {
                    self.bump_after_operand()?;
                    let f = self.parse_function_rest(true, false, fstart)?;
                    return Ok(ExportDecl::Default(ExportDefault::Function(Box::new(f))));
                }
                self.restore(save);
            }
            if self.at_kw(Keyword::Class) {
                let cstart = self.cur().span.start;
                self.bump_after_operand()?;
                let c = self.parse_class_rest(cstart)?;
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
                self.record_export_name(&exported, pos)?;
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
            // Without a `from`, each LOCAL name must resolve to a binding in
            // this module -- `export { unresolvable };` is an early error, not a
            // link-time one. The binding may still be declared further down the
            // file, so the names are queued and judged when the module ends.
            // WITH a `from` the local half is the FOREIGN module's export name
            // and binds nothing here, so it is exempt.
            if source.is_none() {
                for sp in &specifiers {
                    if let ModuleExportName::Ident(n) = &sp.local {
                        self.pending_export_locals.push((n.clone(), pos));
                    }
                }
            }
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
        let s = self.parse_stmt_list_item_inner()?;
        // The token after a StatementListItem opens a new statement, so a `/`
        // there is a REGEX — but the `}` that ended this item was scanned before
        // that was knowable (the same helper closes a function/class expression,
        // where the same `/` is division).
        self.reinterpret_slash_as_regex()?;
        Ok(s)
    }

    fn parse_stmt_list_item_inner(&mut self) -> PResult<Stmt> {
        // A StatementListItem is the one position where every declaration form is
        // legal, so it is also the position that CLEARS any restriction an
        // enclosing unbraced body left behind: `if (x) { function f(){} }` reaches
        // here through the Block, and the brace is what makes it a declaration
        // again rather than an Annex B clause.
        self.stmt_pos = StmtPos::ListItem;
        if let Some(m) = self.parse_module_decl()? {
            return Ok(m);
        }
        if self.at_kw(Keyword::Function) {
            let start = self.cur().span.start;
            self.bump_after_operand()?;
            let f = self.parse_function_rest(false, false, start)?;
            if let Some(n) = &f.name {
                let bk = fn_bind_kind(&f);
                self.declare(&n.clone(), bk, start)?;
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
                let f = self.parse_function_rest(true, false, start)?;
                if let Some(n) = &f.name {
                    self.declare(&n.clone(), BindKind::GenFunction, start)?;
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
            let at = self.cur().span.start;
            self.bump_after_operand()?;
            if !self.cur().newline_before && self.is_binding_ident() {
                self.check_using_position(at)?;
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
                let at = self.cur().span.start;
                self.bump_after_operand()?;
                if !self.cur().newline_before && self.is_binding_ident() {
                    self.check_using_position(at)?;
                    return Ok(Some(VarKind::AwaitUsing));
                }
            }
            self.restore(save);
        }
        Ok(None)
    }

    /// See `ScopeStack::using_decl_allowed`.
    fn check_using_position(&self, pos: u32) -> PResult<()> {
        if self.scopes.using_decl_allowed() {
            return Ok(());
        }
        Err(SyntaxError::new(
            "SyntaxError: a 'using' declaration may not appear here — it needs a \
             block to scope its disposal to",
            pos,
        ))
    }

    pub(crate) fn parse_var_decl(&mut self, kind: VarKind) -> PResult<VarDecl> {
        let mut decls = Vec::new();
        loop {
            let pos = self.cur().span.start;
            let id = self.parse_binding_pattern()?;
            // `using` binds resources by NAME so it can dispose them in reverse
            // declaration order; a pattern has no single name to record, so the
            // grammar takes a BindingIdentifier and nothing else. The FIRST
            // declarator is settled by the contextual-keyword lookahead
            // (`using [` is an element access, not a declaration) — it is the
            // second and later ones that reach here.
            if matches!(kind, VarKind::Using | VarKind::AwaitUsing)
                && !matches!(id, Pattern::Ident(_))
            {
                return Err(SyntaxError::new(
                    "SyntaxError: a 'using' declaration binds a plain identifier, \
                     not a destructuring pattern",
                    pos,
                ));
            }
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
        let s = self.parse_stmt_inner()?;
        // Same correction for an unbraced body (`if (c) { … } /re/.test(x)`) and
        // for the `)` of `do … while (…)`, after which §14.7.2 inserts the `;`.
        self.reinterpret_slash_as_regex()?;
        Ok(s)
    }

    fn parse_stmt_inner(&mut self) -> PResult<Stmt> {
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
                    let body = self.in_nested_body(|p| p.parse_stmt())?;
                    Ok(Stmt::With { object, body: Box::new(body) })
                }
                // `var` is a VariableStatement, which IS a Statement, so it is
                // legal as the unbraced body of `if`/`else`/`while`/`for`/`do`.
                // Only `let`/`const`/`class` are Declarations and barred here.
                //
                // Falling through to the expression parser instead rejected
                // `if (x) var y = 1;` as "unexpected reserved word". Minifiers
                // emit that shape constantly — dropping the braces is one of
                // their standard size wins — so this took out every real-world
                // bundle, React's included.
                Keyword::Var => {
                    self.bump_after_operand()?;
                    let d = self.parse_var_decl(VarKind::Var)?;
                    self.semicolon()?;
                    Ok(Stmt::VarDecl(d))
                }
                // Annex B B.3.4 / B.3.2: a function DECLARATION as a bare
                // statement body (`if (x) function f() {}`, `l: function f() {}`)
                // is web-compat sloppy-mode legality. Strict code keeps the
                // error, and so does every OTHER statement position — there is no
                // Annex B production for `while (x) function f(){}` or
                // `with (o) function f(){}`, which is what `StmtPos::Nested`
                // records. The generator/async forms are never covered: B.3.4
                // names `FunctionDeclaration`, and `function*` reaches here
                // through the same keyword, so the shape has to be re-checked
                // after parsing.
                Keyword::Function if !self.ctx.strict && self.stmt_pos != StmtPos::Nested => {
                    self.bump_after_operand()?;
                    let f = self.parse_function_rest(false, false, start)?;
                    if f.is_generator {
                        return Err(SyntaxError::new(
                            "SyntaxError: declaration not allowed in this position",
                            start,
                        ));
                    }
                    if let Some(n) = &f.name {
                        self.declare(&n.clone(), BindKind::Function, start)?;
                    }
                    Ok(Stmt::FnDecl(Box::new(f)))
                }
                // A declaration is NOT a Statement, so it is an error here even
                // though it is fine as a StatementListItem.
                Keyword::Function | Keyword::Class => Err(SyntaxError::new(
                    "SyntaxError: declaration not allowed in this position",
                    start,
                )),
                // `let [` has no reading at all in a Statement position: the
                // lexical declaration is not a Statement, and ExpressionStatement
                // excludes it by lookahead (14.5), which is why even the element
                // access `let[0] = 1` on a sloppy-mode variable named `let` is an
                // error here. Every other `let` — `let;`, `let.a`, `let(x)` — is
                // the ordinary identifier and falls through.
                Keyword::Let if self.let_bracket_ahead()? => Err(SyntaxError::new(
                    "SyntaxError: lexical declaration not allowed in this position",
                    start,
                )),
                // `async [no LineTerminator here] function` is in
                // ExpressionStatement's lookahead restriction, and there is no
                // Annex B carve-out for it, so in a Statement position it is an
                // error rather than an async function EXPRESSION. Reading it as
                // an expression made `for (;;) async function f(){}` a legal
                // infinite loop instead of a SyntaxError.
                Keyword::Async if self.async_function_ahead()? => Err(SyntaxError::new(
                    "SyntaxError: declaration not allowed in this position",
                    start,
                )),
                _ => self.parse_expr_or_labeled(),
            },
            _ => self.parse_expr_or_labeled(),
        }
    }

    /// Does `let [` start here? Peeks without consuming — see
    /// [`Self::async_function_ahead`]. A LineTerminator between the two does not
    /// break the sequence: ASI never applies to a token that could continue the
    /// statement, so `let \n [a] = 0` is still `let [`.
    fn let_bracket_ahead(&mut self) -> PResult<bool> {
        let save = self.save();
        self.bump_after_operand()?;
        let hit = self.at(Punct::LBracket);
        self.restore(save);
        Ok(hit)
    }

    /// Does `async [no LineTerminator here] function` start here? Peeks without
    /// consuming — the caller is deciding whether this Statement position is
    /// legal at all, not parsing it.
    fn async_function_ahead(&mut self) -> PResult<bool> {
        let save = self.save();
        self.bump_after_operand()?;
        let hit = self.at_kw(Keyword::Function) && !self.cur().newline_before;
        self.restore(save);
        Ok(hit)
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
                // The body inherits this label's position: a LabelledStatement at
                // statement-list level may wrap a FunctionDeclaration, and so may
                // one nested inside it (`l1: l2: function f(){}`).
                let body = self.parse_stmt()?;
                self.labels.pop();
                // §14.6.1 and §14.7.x: `IsLabelledFunction(Statement) is false`
                // for an if/else clause and for every loop body. So the two
                // Annex B allowances do not compose — `l: function f(){}` is
                // legal on its own and an error the moment it is the clause of an
                // `if`, which is why this is checked HERE (where the label is
                // known) rather than at the clause.
                if self.stmt_pos != StmtPos::ListItem && is_labelled_function(&body) {
                    return Err(SyntaxError::new(
                        "SyntaxError: a labelled function declaration is not allowed \
                         in this position",
                        start,
                    ));
                }
                return Ok(Stmt::Labeled { label: name, body: Box::new(body) });
            }
            self.restore(save);
        }
        let e = self.parse_expr_full()?;
        self.semicolon()?;
        Ok(Stmt::Expr(e))
    }

    /// Run `f` inside a Block scope, for an IfStatement clause (Annex B B.3.4).
    /// Pops even on error so the scope stack can never be left unbalanced.
    fn in_annexb_clause<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        self.scopes.push(ScopeKind::Block);
        let saved = std::mem::replace(&mut self.stmt_pos, StmtPos::IfClause);
        let r = f(self);
        self.stmt_pos = saved;
        let popped = self.scopes.pop();
        let v = r?;
        popped?;
        Ok(v)
    }

    /// Run `f` with the Statement position set to `Nested` — a loop body or a
    /// `with` body, where no declaration of any kind is a legal Statement.
    fn in_nested_body<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let saved = std::mem::replace(&mut self.stmt_pos, StmtPos::Nested);
        let r = f(self);
        self.stmt_pos = saved;
        r
    }

    fn parse_if(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        self.expect(Punct::LParen, true)?;
        let test = self.parse_expr_full()?;
        self.expect(Punct::RParen, true)?;
        // B.3.4 says a FunctionDeclaration in an IfStatement clause behaves as
        // if it were the sole StatementListItem of a BlockStatement, so its name
        // is BLOCK-scoped: `let f = 1; if (true) function f(){}` is legal.
        // Scoping the clause (rather than the declaration) is what the spec
        // actually says, and it keeps a LABELLED function — B.3.2, which is NOT
        // block-wrapped — colliding with an outer `let` as node does. A braced
        // clause just gets a redundant scope, and a `var` still hoists straight
        // through to its var boundary.
        let cons = Box::new(self.in_annexb_clause(|p| p.parse_stmt())?);
        let alt = if self.eat_kw(Keyword::Else, true)? {
            Some(Box::new(self.in_annexb_clause(|p| p.parse_stmt())?))
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
        let body = Box::new(self.in_loop(|p| p.in_nested_body(|p| p.parse_stmt()))?);
        Ok(Stmt::While { test, body })
    }

    fn parse_do_while(&mut self) -> PResult<Stmt> {
        self.bump_before_operand()?;
        let body = Box::new(self.in_loop(|p| p.in_nested_body(|p| p.parse_stmt()))?);
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
                // `for (using x of …)` disposes at the end of each iteration;
                // for-IN has no such form in the proposal.
                if !of && matches!(kind, VarKind::Using | VarKind::AwaitUsing) {
                    return Err(SyntaxError::new(
                        "SyntaxError: a 'using' declaration is not allowed in a \
                         for-in head",
                        pos,
                    ));
                }
                self.bump_before_operand()?;
                self.ctx.in_ = saved_in;
                let right = if of { self.parse_assign_full()? } else { self.parse_expr_full()? };
                self.expect(Punct::RParen, true)?;
                let body = Box::new(self.in_loop(|p| p.in_nested_body(|p| p.parse_stmt()))?);
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
                // Annex B B.3.5 (Initializers in ForIn Statement Heads):
                //   for ( var BindingIdentifier Initializer in Expression ) Statement
                // Sloppy code only, `var` only, ONE simple BindingIdentifier only,
                // and for-IN only — the sibling negative tests require
                // `for (a = 0 in {})`, `for (let a = 0 in {})`, `for (var [a] = 0 in
                // {})` and the strict-mode form to stay SyntaxErrors, and no such
                // production exists for for-of. Everything downstream (the B.3.6
                // "assign before the object expression" step, free-var analysis)
                // already handles a `ForTarget::Var` declarator carrying an init.
                if kind == VarKind::Var
                    && init.is_some()
                    && !self.ctx.strict
                    && matches!(pat, Pattern::Ident(_))
                    && self.at_kw(Keyword::In)
                {
                    self.bump_before_operand()?;
                    self.ctx.in_ = saved_in;
                    let right = self.parse_expr_full()?;
                    self.expect(Punct::RParen, true)?;
                    let body = Box::new(self.in_loop(|p| p.in_nested_body(|p| p.parse_stmt()))?);
                    let left =
                        ForTarget::Var(VarDecl { kind, decls: vec![Declarator { id: pat, init }] });
                    Stmt::ForIn { left, right, body }
                } else {
                    let mut decls = vec![Declarator { id: pat, init }];
                    while self.eat(Punct::Comma, true)? {
                        let p2 = self.cur().span.start;
                        let id = self.parse_binding_pattern()?;
                        self.declare_pattern(&id, kind, p2)?;
                        let init = if self.eat(Punct::Eq, true)? {
                            Some(self.parse_assign()?)
                        } else {
                            None
                        };
                        decls.push(Declarator { id, init });
                    }
                    self.ctx.in_ = saved_in;
                    self.expect(Punct::Semi, true)?;
                    self.parse_for_classic(Some(ForInit::Var(VarDecl { kind, decls })))?
                }
            }
        } else {
            let pos = self.cur().span.start;
            // A for-in/of head is a TARGET position — `for ({a = 1} of x);` is
            // legal — so a CoverInitializedName recorded while parsing it is
            // discharged once `in`/`of` proves the pattern reading. `parse_expr`
            // (not `_full`) is deliberate: finalizing here would raise the error
            // before that proof arrives. The classic-`for` arm below has no such
            // proof coming, so it raises. Stashing the enclosing region's error
            // first keeps an inner one from being mistaken for it.
            let outer_po = self.cover.pattern_only.take();
            // The restriction below is a LOOKAHEAD one, so it must be decided
            // from the token, not the parsed expression: an escaped
            // `async` is not the contextual `async` and stays legal.
            let head_bare_async = self.at_contextual("async");
            let e = self.parse_expr()?;
            if self.at_kw(Keyword::In) || self.at_kw(Keyword::Of) {
                let of = self.at_kw(Keyword::Of);
                // `for ( [lookahead ∉ { let [, async of }] LHS of …)`. Three
                // things narrow it, each with its own test262 file: it does NOT
                // apply to `for await (async of …)` (no ambiguity to resolve
                // there), nor to an escaped `async`, nor to a parenthesized
                // `(async)` — parens make the head's first token `(`, so
                // requiring the expression to be exactly `Expr::Ident("async")`
                // alongside the token check excludes that case too.
                if of
                    && !is_await
                    && head_bare_async
                    && matches!(&e, Expr::Ident(n) if &**n == "async")
                {
                    return Err(self
                        .err_here("SyntaxError: 'async' is not a valid for-of loop target"));
                }
                self.cover.pattern_only = outer_po;
                self.bump_before_operand()?;
                self.ctx.in_ = saved_in;
                let target = self.expr_to_target(e, true, pos)?;
                let right = if of { self.parse_assign_full()? } else { self.parse_expr_full()? };
                self.expect(Punct::RParen, true)?;
                let body = Box::new(self.in_loop(|p| p.in_nested_body(|p| p.parse_stmt()))?);
                let left = ForTarget::Target(target);
                if of {
                    Stmt::ForOf { left, right, body, is_await }
                } else {
                    Stmt::ForIn { left, right, body }
                }
            } else {
                // Not a for-in/of after all: the head was an ordinary
                // expression, so a pending CoverInitializedName is now final.
                if let Some(err) = self.cover.pattern_only.take() {
                    self.cover.pattern_only = outer_po;
                    return Err(err);
                }
                self.cover.pattern_only = outer_po;
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
        let body = Box::new(self.in_loop(|p| p.in_nested_body(|p| p.parse_stmt()))?);
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
