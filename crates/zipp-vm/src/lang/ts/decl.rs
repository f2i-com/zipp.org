//! Declarations and module items: the statements TypeScript adds.
//!
//! `interface`, `type` and `declare …` read as types and leave an empty
//! statement; `enum` and `namespace` lower to the IIFEs tsc emits; `import`
//! and `export` take their `type` forms and modifiers, `import x = …` and
//! `export = …`.
//!
//! A lowering of more than one statement leaves all but its last on the
//! statement list being gathered ([`Parser::ts_emit`]), so it reads as the
//! sequence tsc writes (`var E; (function (E) { … })(E || (E = {}));`).

use super::lower::{self, EnumMember};
use crate::parse::ast::*;
use crate::parse::parser::{BindKind, PResult, Parser, ScopeKind, SyntaxError};
use crate::parse::token::{Keyword, Punct, Span, TokenKind};

impl<'s> Parser<'s> {
    /// Emit `stmts` in order where one statement is expected: the earlier
    /// ones go on the statement list being read, and the last is returned
    /// for the caller to add.
    pub(crate) fn ts_emit(&mut self, mut stmts: Vec<Stmt>) -> Stmt {
        let last = stmts.pop().unwrap_or(Stmt::Empty);
        self.stmt_scratch.extend(stmts);
        last
    }

    /// A TypeScript declaration starting a StatementListItem, if one does.
    /// `None` leaves the token for the JavaScript statement parser (which an
    /// `abstract class` reaches with `abstract` consumed).
    pub(crate) fn ts_stmt(&mut self) -> PResult<Option<Stmt>> {
        let Some(word) = self.ts_word_name() else {
            return Ok(None);
        };
        match &*word {
            "interface" if self.ts_next_is_ident_on_line()? => {
                self.ts_interface()?;
                Ok(Some(Stmt::Empty))
            }
            "type" if self.ts_next_is_ident_on_line()? => {
                self.ts_type_alias()?;
                Ok(Some(Stmt::Empty))
            }
            "enum" => {
                let stmts = self.ts_enum(None)?;
                Ok(Some(self.ts_emit(stmts)))
            }
            "const" if self.ts_followed_by_word("enum") => {
                self.bump(false)?;
                let stmts = self.ts_const_enum(None)?;
                Ok(Some(self.ts_emit(stmts)))
            }
            "declare" if self.ts_next_is_ident_on_line()? => {
                self.bump(false)?;
                self.ts_declare()?;
                Ok(Some(Stmt::Empty))
            }
            "namespace" | "module" if self.ts_namespace_ahead()? => {
                let stmts = self.ts_namespace(None)?;
                Ok(Some(self.ts_emit(stmts)))
            }
            "abstract"
                if self.ts_peek(|p| p.at_kw(Keyword::Class) && !p.cur().newline_before)? =>
            {
                self.bump(false)?;
                Ok(None)
            }
            // `import x = …` in a script or a namespace (a module's own
            // `import` is read by `ts_module_decl`).
            "import"
                if (self.goal != Goal::Module || !self.scopes.at_module_top_level())
                    && self.ts_import_equals_ahead()? =>
            {
                let pos = self.cur().span.start;
                self.bump(false)?;
                let s = self.ts_import_equals(pos, false)?;
                Ok(Some(s))
            }
            _ => Ok(None),
        }
    }

    /// The source after the current token, past blanks, starts with the word
    /// `word`. A text check, not a token: `const` is read at every `const`
    /// declaration and a lookahead token there would be paid by every one.
    fn ts_followed_by_word(&self, word: &str) -> bool {
        let rest = self.rest_from(self.cur().span.end).trim_start();
        rest.starts_with(word)
            && !rest[word.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
    }

    /// `namespace N` / `module N` / `module "m"`, on one line.
    fn ts_namespace_ahead(&mut self) -> PResult<bool> {
        let is_module = self.ts_at("module");
        self.ts_peek(|p| {
            !p.cur().newline_before
                && (matches!(p.cur().kind, TokenKind::Ident { private: false, .. })
                    || (is_module && matches!(p.cur().kind, TokenKind::Str(_))))
        })
    }

    /// `import x =` (the `import` current).
    fn ts_import_equals_ahead(&mut self) -> PResult<bool> {
        let saved = self.peek_state();
        self.bump(false)?;
        if self.ts_at("type") {
            self.bump(false)?;
        }
        let r = self.is_binding_ident() && {
            self.bump(false)?;
            self.at(Punct::Eq)
        };
        self.peek_restore(saved);
        Ok(r)
    }

    /// `interface I<T> extends A, B { … }`, the `interface` current.
    fn ts_interface(&mut self) -> PResult<()> {
        self.bump(false)?;
        self.ident_name()?;
        if self.at(Punct::Lt) {
            self.ts_type_params()?;
        }
        if self.eat_kw(Keyword::Extends, false)? {
            loop {
                self.ts_type()?;
                if !self.eat(Punct::Comma, false)? {
                    break;
                }
            }
        }
        self.ts_type_literal()?;
        // The `}` was read as the end of a type; a statement follows.
        self.reinterpret_slash_as_regex()
    }

    /// `type T<P> = Type;`, the `type` current.
    fn ts_type_alias(&mut self) -> PResult<()> {
        self.bump(false)?;
        self.ident_name()?;
        if self.at(Punct::Lt) {
            self.ts_type_params()?;
        }
        self.expect(Punct::Eq, false)?;
        self.ts_type()?;
        self.semicolon()
    }

    /// An ambient declaration, after `declare`: read as types, emitting
    /// nothing. Its names are the host's, bound by nothing here.
    pub(crate) fn ts_declare(&mut self) -> PResult<()> {
        let Some(word) = self.ts_word() else {
            return Err(self.err_here("SyntaxError: declaration expected after 'declare'"));
        };
        match word {
            "const" | "let" | "var" | "using" => {
                self.bump(false)?;
                if self.ts_at("enum") {
                    return self.ts_skip_enum();
                }
                loop {
                    if self.at(Punct::LBracket) || self.at(Punct::LBrace) {
                        self.ts_skip_balanced()?;
                    } else {
                        self.ident_name()?;
                    }
                    self.ts_binding_suffix()?;
                    if self.at(Punct::Eq) {
                        self.bump(true)?;
                        self.parse_assign_full()?;
                    }
                    if !self.eat(Punct::Comma, false)? {
                        break;
                    }
                }
                self.semicolon()
            }
            "async" | "function" => {
                if word == "async" {
                    self.bump(false)?;
                }
                self.bump(false)?; // `function`
                let _ = self.eat(Punct::Star, false)?;
                self.ident_name()?;
                if self.at(Punct::Lt) {
                    self.ts_type_params()?;
                }
                self.ts_skip_params()?;
                if self.eat(Punct::Colon, false)? {
                    self.ts_return_type()?;
                }
                self.semicolon()
            }
            "abstract" | "class" => {
                if word == "abstract" {
                    self.bump(false)?;
                }
                self.bump(false)?; // `class`
                if self.ts_at_name() && !self.ts_at("extends") && !self.ts_at("implements") {
                    self.ident_name()?;
                }
                if self.at(Punct::Lt) {
                    self.ts_type_params()?;
                }
                while !self.at(Punct::LBrace) {
                    if self.at_eof() {
                        return Err(self.err_here("SyntaxError: unexpected end of input"));
                    }
                    if self.at(Punct::Lt) {
                        self.ts_type_args()?;
                    } else {
                        self.bump(false)?;
                    }
                }
                self.ts_skip_balanced()?;
                self.reinterpret_slash_as_regex()
            }
            "enum" => self.ts_skip_enum(),
            "namespace" | "module" | "global" => {
                if word != "global" {
                    self.bump(false)?;
                }
                // The name (dotted, or a module's string), up to the body.
                while !self.at(Punct::LBrace) {
                    if self.at(Punct::Semi)
                        || self.cur().newline_before
                            && !self.at(Punct::Dot)
                            && self.prev_is_name_end()
                    {
                        // `declare module "m";` — a shorthand ambient module.
                        return self.semicolon();
                    }
                    if self.at_eof() {
                        return Err(self.err_here("SyntaxError: unexpected end of input"));
                    }
                    self.bump(false)?;
                }
                self.ts_skip_balanced()?;
                self.reinterpret_slash_as_regex()
            }
            "type" => self.ts_type_alias(),
            "interface" => self.ts_interface(),
            _ => Err(self.err_here("SyntaxError: declaration expected after 'declare'")),
        }
    }

    /// Whether the token before the current one ended a name or string, so a
    /// line break here may end `declare module "m"`.
    fn prev_is_name_end(&self) -> bool {
        let prev = self.prev_end();
        prev > 0
            && self.rest_from(prev - 1).chars().next().is_some_and(|c| {
                c.is_alphanumeric() || c == '_' || c == '$' || c == '"' || c == '\''
            })
    }

    /// `enum E { … }` read and dropped (an ambient one), `enum` current.
    fn ts_skip_enum(&mut self) -> PResult<()> {
        self.bump(false)?;
        self.ident_name()?;
        if !self.at(Punct::LBrace) {
            return Err(self.err_here("SyntaxError: expected '{'"));
        }
        self.ts_skip_balanced()?;
        self.reinterpret_slash_as_regex()
    }

    /// The `var` or `let` a lowered enum or namespace declares its name
    /// with — none when the name is already declared in this scope, which
    /// is a declaration merging with it (`class C {}` + `namespace C {}`, two
    /// `enum E`s). `let` in a block, as tsc writes it; `var` elsewhere.
    fn ts_merge_decl(&mut self, name: &Name, pos: u32) -> PResult<Option<Stmt>> {
        self.scopes.ts_take_import(name);
        if self.scopes.declared_here(name) {
            return Ok(None);
        }
        let block = matches!(
            self.scopes.scopes.last().map(|s| s.kind),
            Some(ScopeKind::Block | ScopeKind::Switch | ScopeKind::Catch | ScopeKind::ForHead)
        );
        let kind = if block { VarKind::Let } else { VarKind::Var };
        self.declare(name, if block { BindKind::Let } else { BindKind::Var }, pos)?;
        Ok(Some(lower::var_stmt(kind, name, None)))
    }

    /// `const enum E { … }`, the `enum` current: lowered like any enum (so
    /// `E[0]` and a use before the declaration still work), and its constant
    /// members inlined from here on.
    fn ts_const_enum(&mut self, exported_in: Option<&Name>) -> PResult<Vec<Stmt>> {
        let name = match self.ts_peek(|p| p.ts_word_name())? {
            Some(n) => n,
            None => return self.ts_enum(exported_in),
        };
        let stmts = self.ts_enum(exported_in)?;
        self.ts_state.const_enums.insert(name);
        Ok(stmts)
    }

    /// `enum E { A, B = 2, C = "c" }`, the `enum` current, lowered.
    /// `exported_in` names the namespace it is exported from, if it is.
    pub(crate) fn ts_enum(&mut self, exported_in: Option<&Name>) -> PResult<Vec<Stmt>> {
        let start = self.cur().span.start;
        self.bump(false)?; // `enum`
        let (name, pos) = self.binding_ident()?;
        self.expect(Punct::LBrace, false)?;
        // The members are names in their initializers (`B = A | 2` means
        // the member `A`), hiding any import of the same name: a scope of
        // their own holds them while the body is read.
        self.scopes.push(ScopeKind::Function);
        let members = self.ts_enum_members();
        self.scopes.pop()?;
        let members = members?;
        self.expect(Punct::RBrace, false)?;
        let span = Span::new(start, self.prev_end());
        let (body, consts) = lower::enum_body(&name, members, &self.ts_state.enums);
        self.ts_state.enums.entry(name.clone()).or_default().extend(consts);
        let mut out = Vec::with_capacity(2);
        out.extend(self.ts_merge_decl(&name, pos)?);
        out.push(lower::iife(&name, body, lower::merge_arg(&name, exported_in), span));
        self.reinterpret_slash_as_regex()?;
        Ok(out)
    }

    /// An enum's members, up to its `}`.
    fn ts_enum_members(&mut self) -> PResult<Vec<EnumMember>> {
        let mut members = Vec::new();
        while !self.at(Punct::RBrace) {
            let member: Name = match self.cur().kind.clone() {
                TokenKind::Str(s) => {
                    self.bump(false)?;
                    s.to_lossy_string().into()
                }
                TokenKind::Ident { private: false, .. } => self.ident_name()?,
                TokenKind::Punct(Punct::LBracket) => {
                    self.bump(false)?;
                    let TokenKind::Str(s) = self.cur().kind.clone() else {
                        return Err(
                            self.err_here("SyntaxError: an enum member name must be a string")
                        );
                    };
                    self.bump(false)?;
                    self.expect(Punct::RBracket, false)?;
                    s.to_lossy_string().into()
                }
                _ => return Err(self.err_here("SyntaxError: enum member expected")),
            };
            let init = if self.eat(Punct::Eq, true)? {
                Some(self.with_in(|p| p.parse_assign_full())?)
            } else {
                None
            };
            self.scopes.declare_param(&member);
            members.push(EnumMember { name: member, init });
            if !self.eat(Punct::Comma, false)? {
                break;
            }
        }
        Ok(members)
    }

    /// `namespace A.B { … }` / `module A { … }`, the keyword current,
    /// lowered; a `declare`d or string-named (`module "m"`) one, and one with
    /// no run-time content, emits nothing.
    pub(crate) fn ts_namespace(&mut self, exported_in: Option<&Name>) -> PResult<Vec<Stmt>> {
        self.bump(false)?; // `namespace` / `module`
        if matches!(self.cur().kind, TokenKind::Str(_)) {
            // `module "m" { … }` outside `declare` is ambient all the same.
            self.bump(false)?;
            if self.at(Punct::LBrace) {
                self.ts_skip_balanced()?;
                self.reinterpret_slash_as_regex()?;
            } else {
                self.semicolon()?;
            }
            return Ok(Vec::new());
        }
        let mut names = vec![self.binding_ident()?];
        while self.eat(Punct::Dot, false)? {
            let pos = self.cur().span.start;
            names.push((self.ident_name()?, pos));
        }
        let end_name = names
            .last()
            .map(|(n, _)| n.clone())
            .expect("a namespace has a name");
        let body = self.ts_namespace_body(&end_name)?;
        let end = self.prev_end();
        self.reinterpret_slash_as_regex()?;
        // No run-time content (only types): nothing at all, as tsc emits.
        if body.iter().all(|s| matches!(s, Stmt::Empty)) {
            return Ok(Vec::new());
        }
        // Innermost first: each level's IIFE becomes its parent's body.
        let mut inner = body;
        for i in (1..names.len()).rev() {
            let (name, pos) = &names[i];
            let parent = &names[i - 1].0;
            inner = vec![
                lower::var_stmt(VarKind::Let, name, None),
                lower::iife(
                    name,
                    inner,
                    lower::merge_arg(name, Some(parent)),
                    Span::new(*pos, end),
                ),
            ];
        }
        let (name, pos) = &names[0];
        let mut out = Vec::with_capacity(2);
        out.extend(self.ts_merge_decl(name, *pos)?);
        out.push(lower::iife(
            name,
            inner,
            lower::merge_arg(name, exported_in),
            Span::new(*pos, end),
        ));
        Ok(out)
    }

    /// A namespace's `{ … }`, read as the body of the function it lowers to,
    /// its `export`s made assignments to `ns` (the namespace object).
    fn ts_namespace_body(&mut self, ns: &Name) -> PResult<Vec<Stmt>> {
        self.expect(Punct::LBrace, true)?;
        let saved = self.ctx;
        let saved_labels = std::mem::take(&mut self.labels);
        self.ctx.yield_ = false;
        self.ctx.await_ = false;
        self.ctx.return_ = false;
        self.ctx.in_loop = false;
        self.ctx.in_switch = false;
        self.ctx.in_ = true;
        self.scopes.push(ScopeKind::Function);
        let mark = self.stmt_scratch.len();
        while !self.at(Punct::RBrace) && !self.at_eof() {
            let r = if self.at_kw(Keyword::Export) {
                self.ts_namespace_export(ns)
            } else {
                self.parse_stmt_list_item()
            };
            match r {
                Ok(s) => self.stmt_scratch.push(s),
                Err(e) => {
                    self.stmt_scratch.truncate(mark);
                    return Err(e);
                }
            }
        }
        let body: Vec<Stmt> = self.stmt_scratch.drain(mark..).collect();
        self.scopes.pop()?;
        self.ctx = saved;
        self.labels = saved_labels;
        self.expect(Punct::RBrace, false)?;
        Ok(body)
    }

    /// `export …` inside namespace `ns`: the declaration, then its names
    /// made properties of `ns`.
    fn ts_namespace_export(&mut self, ns: &Name) -> PResult<Stmt> {
        self.bump(false)?; // `export`
        if let Some(word) = self.ts_word() {
            match word {
                "declare" => {
                    self.bump(false)?;
                    self.ts_declare()?;
                    return Ok(Stmt::Empty);
                }
                "interface" | "type" => {
                    return Ok(self.ts_stmt()?.unwrap_or(Stmt::Empty));
                }
                "enum" => {
                    let stmts = self.ts_enum(Some(ns))?;
                    return Ok(self.ts_emit(stmts));
                }
                "const" if self.ts_followed_by_word("enum") => {
                    self.bump(false)?;
                    let stmts = self.ts_const_enum(Some(ns))?;
                    return Ok(self.ts_emit(stmts));
                }
                "namespace" | "module" => {
                    let stmts = self.ts_namespace(Some(ns))?;
                    return Ok(self.ts_emit(stmts));
                }
                "import" => {
                    let pos = self.cur().span.start;
                    self.bump(false)?;
                    let s = self.ts_import_equals(pos, false)?;
                    let mut stmts = vec![s];
                    if let Some(Stmt::VarDecl(d)) = stmts.first() {
                        if let Some(Declarator {
                            id: Pattern::Ident(n),
                            ..
                        }) = d.decls.first()
                        {
                            let n = n.clone();
                            stmts.push(lower::assign_member_stmt(ns, &n, lower::ident(&n)));
                        }
                    }
                    return Ok(self.ts_emit(stmts));
                }
                "abstract" => {
                    self.bump(false)?;
                }
                _ => {}
            }
        }
        let at = self.cur().span.start;
        let decl = self.parse_stmt_list_item()?;
        let mut stmts = Vec::with_capacity(2);
        match &decl {
            Stmt::FnDecl(f) => {
                if let Some(n) = &f.name {
                    stmts.push(lower::assign_member_stmt(ns, n, lower::ident(n)));
                }
            }
            Stmt::ClassDecl(c) => {
                if let Some(n) = &c.name {
                    stmts.push(lower::assign_member_stmt(ns, n, lower::ident(n)));
                }
            }
            Stmt::VarDecl(d) => {
                let mut names = Vec::new();
                for dc in &d.decls {
                    crate::parse::stmt::collect_pattern_names(&dc.id, &mut names);
                }
                for n in names {
                    // A `const` is a value; a `let`/`var` stays live.
                    stmts.push(if d.kind == VarKind::Const {
                        lower::assign_member_stmt(ns, &n, lower::ident(&n))
                    } else {
                        lower::live_export(ns, &n, self.ts_name_pos(&n, at))
                    });
                }
            }
            Stmt::Empty => return Ok(Stmt::Empty),
            _ => {
                return Err(SyntaxError::new(
                    "SyntaxError: this export is not allowed in a namespace",
                    at,
                ))
            }
        }
        stmts.insert(0, decl);
        Ok(self.ts_emit(stmts))
    }

    /// Where binding `name` is spelled in the declaration starting at `from`
    /// (for [`lower::live_export`]'s spans).
    fn ts_name_pos(&self, name: &str, from: u32) -> u32 {
        let rest = self.rest_from(from);
        let mut search = 0;
        while let Some(i) = rest[search..].find(name) {
            let at = search + i;
            let before = rest[..at].chars().next_back();
            let after = rest[at + name.len()..].chars().next();
            let ident =
                |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$');
            if !ident(before) && !ident(after) {
                return from + at as u32;
            }
            search = at + name.len();
        }
        from
    }

    /// `import x = require("m")` or `import x = A.B.C`, after `import` (and
    /// `type`, which makes it read and drop), at `pos`. A `require` becomes
    /// the namespace import `import * as x from "m"` in a module (the engine
    /// has no CommonJS loader) and `var x = require("m")` in a script; a
    /// name becomes `var x = A.B.C`. `exported`: `export import x = …`.
    pub(crate) fn ts_import_equals(&mut self, pos: u32, exported: bool) -> PResult<Stmt> {
        let type_only = self.ts_at("type") && self.ts_peek(|p| p.is_binding_ident())?;
        if type_only {
            self.bump(false)?;
        }
        let (local, lpos) = self.binding_ident()?;
        self.expect(Punct::Eq, false)?;
        if self.ts_at("require") && self.ts_peek(|p| p.at(Punct::LParen))? {
            self.bump(false)?;
            self.bump(false)?; // `(`
            let TokenKind::Str(source) = self.cur().kind.clone() else {
                return Err(self.err_here("SyntaxError: string literal expected"));
            };
            self.bump(false)?;
            self.expect(Punct::RParen, false)?;
            self.semicolon()?;
            if type_only {
                return Ok(Stmt::Empty);
            }
            if self.goal == Goal::Module {
                self.declare(&local, BindKind::Import, lpos)?;
                if !exported {
                    self.ts_state.imports.insert(super::name_key(&local), false);
                }
                return Ok(Stmt::Import(Box::new(ImportDecl {
                    specifiers: vec![ImportSpecifier::Namespace(local)],
                    source,
                    attributes: Vec::new(),
                    phase: ImportPhase::Evaluation,
                })));
            }
            self.declare(&local, BindKind::Var, lpos)?;
            let call = Expr::Call(Box::new(CallExpr {
                callee: Expr::Ident("require".into()),
                args: vec![Arg::Expr(Expr::Str(source))],
                optional: false,
                pos,
            }));
            return Ok(lower::var_stmt(VarKind::Var, &local, Some(call)));
        }
        // An entity name: `A.B.C`.
        let (head, _) = self.binding_ident_or_reference()?;
        let mut value = Expr::Ident(head);
        while self.eat(Punct::Dot, false)? {
            let prop = self.ident_name()?;
            value = lower::member(value, &prop);
        }
        self.semicolon()?;
        if type_only {
            return Ok(Stmt::Empty);
        }
        self.declare(&local, BindKind::Var, lpos)?;
        Ok(lower::var_stmt(VarKind::Var, &local, Some(value)))
    }

    /// `import` / `export` at a module's top level, TypeScript's forms
    /// included. `None` is an `import(…)` or `import.meta` expression.
    pub(crate) fn ts_module_decl(&mut self) -> PResult<Option<Stmt>> {
        let pos = self.cur().span.start;
        if self.at_kw(Keyword::Import) {
            if self.ts_peek(|p| p.at(Punct::LParen) || p.at(Punct::Dot))? {
                return Ok(None);
            }
            if self
                .stmt_scratch
                .iter()
                .any(|s| !matches!(s, Stmt::Import(_) | Stmt::Empty))
            {
                self.ts_state.late_import = true;
            }
            self.bump(false)?;
            return self.ts_import_rest(pos).map(Some);
        }
        if self.at_kw(Keyword::Export) {
            self.bump(false)?;
            return self.ts_export_rest(pos).map(Some);
        }
        Ok(None)
    }

    /// An import declaration after `import`.
    fn ts_import_rest(&mut self, pos: u32) -> PResult<Stmt> {
        if matches!(self.cur().kind, TokenKind::Str(_)) {
            return Ok(Stmt::Import(Box::new(self.parse_import_rest(pos)?)));
        }
        if self.ts_import_equals_here()? {
            return self.ts_import_equals(pos, false);
        }
        // `import type …` — but `import type from "m"` and `import type, {…}`
        // bind a default import named `type`.
        let type_only = self.ts_at("type")
            && self.ts_peek(|p| {
                p.at(Punct::LBrace)
                    || p.at(Punct::Star)
                    || (p.is_binding_ident() && !p.ts_at("from"))
            })?;
        if type_only {
            self.bump(false)?;
            if self.at(Punct::Star) {
                self.bump(false)?;
                self.bump(false)?; // `as`
                self.bump(false)?; // the name
            } else {
                if !self.at(Punct::LBrace) {
                    self.bump(false)?;
                    let _ = self.eat(Punct::Comma, false)?;
                }
                if self.at(Punct::LBrace) {
                    self.ts_skip_balanced()?;
                }
            }
            if !self.eat_kw(Keyword::From, false)? {
                return Err(SyntaxError::new("SyntaxError: expected 'from'", pos));
            }
            self.bump(false)?; // the specifier
            self.parse_import_attributes()?;
            self.semicolon()?;
            return Ok(Stmt::Empty);
        }
        let phase = self.import_phase_prefix()?;
        let mut specifiers = Vec::new();
        let mut had_types = false;
        if self.is_binding_ident() {
            let (n, p) = self.binding_ident()?;
            self.ts_import_binding(n.clone(), p)?;
            specifiers.push(ImportSpecifier::Default(n));
            if self.eat(Punct::Comma, false)? {
                had_types |= self.ts_import_bindings(&mut specifiers)?;
            }
        } else {
            had_types |= self.ts_import_bindings(&mut specifiers)?;
        }
        if !self.eat_kw(Keyword::From, false)? {
            return Err(SyntaxError::new("SyntaxError: expected 'from'", pos));
        }
        let TokenKind::Str(source) = self.cur().kind.clone() else {
            return Err(self.err_here("SyntaxError: expected a module specifier string"));
        };
        self.bump_after_operand()?;
        let mut attributes = self.parse_import_attributes()?;
        self.semicolon()?;
        // `import { type A } from "m"` imports nothing at run time.
        if specifiers.is_empty() && had_types {
            return Ok(Stmt::Empty);
        }
        // tsc's `resolveJsonModule`: `import data from "./data.json"` is the
        // JSON module `with { type: "json" }` asks for.
        if attributes.is_empty() && source.to_lossy_string().ends_with(".json") {
            attributes.push(ImportAttribute {
                key: crate::parse::token::StrVal::Utf8("type".into()),
                value: crate::parse::token::StrVal::Utf8("json".into()),
            });
        }
        Ok(Stmt::Import(Box::new(ImportDecl {
            specifiers,
            source,
            attributes,
            phase,
        })))
    }

    /// At the binding after `import`: `x =` or `type x =`.
    fn ts_import_equals_here(&mut self) -> PResult<bool> {
        let saved = self.peek_state();
        if self.ts_at("type") {
            self.bump(false)?;
        }
        let r = self.is_binding_ident() && {
            self.bump(false)?;
            self.at(Punct::Eq)
        };
        self.peek_restore(saved);
        Ok(r)
    }

    /// Bind an import's local name, which elision watches for value uses.
    fn ts_import_binding(&mut self, local: Name, pos: u32) -> PResult<()> {
        self.declare(&local, BindKind::Import, pos)?;
        self.ts_state.imports.insert(super::name_key(&local), false);
        Ok(())
    }

    /// `* as ns` or `{ a, type B, c as d }`; whether a `type` one was dropped.
    fn ts_import_bindings(&mut self, out: &mut Vec<ImportSpecifier>) -> PResult<bool> {
        if self.eat(Punct::Star, false)? {
            if !self.eat_kw(Keyword::As, false)? {
                return Err(self.err_here("SyntaxError: expected 'as' after '*'"));
            }
            let (n, p) = self.binding_ident()?;
            self.ts_import_binding(n.clone(), p)?;
            out.push(ImportSpecifier::Namespace(n));
            return Ok(false);
        }
        self.expect(Punct::LBrace, false)?;
        let mut had_types = false;
        while !self.at(Punct::RBrace) {
            let type_only = self.ts_specifier_is_type()?;
            if type_only {
                self.bump(false)?;
                had_types = true;
            }
            let ipos = self.cur().span.start;
            let could_bind = self.is_binding_ident();
            let imported = self.parse_module_export_name()?;
            let (local, lpos) = if self.eat_kw(Keyword::As, false)? {
                self.binding_ident()?
            } else {
                let ModuleExportName::Ident(n) = &imported else {
                    return Err(self.err_here("SyntaxError: a string module name requires 'as'"));
                };
                if !could_bind || &**n == "eval" || &**n == "arguments" {
                    return Err(SyntaxError::new(
                        format!("SyntaxError: '{n}' cannot be used as an imported binding"),
                        ipos,
                    ));
                }
                (n.clone(), ipos)
            };
            if !type_only {
                self.ts_import_binding(local.clone(), lpos)?;
                out.push(ImportSpecifier::Named { imported, local });
            }
            if !self.eat(Punct::Comma, false)? {
                break;
            }
        }
        self.expect(Punct::RBrace, false)?;
        Ok(had_types)
    }

    /// At a specifier: is its `type` the modifier (`{ type A }`, `{ type A as
    /// B }`) rather than the name (`{ type }`, `{ type as t }`)?
    fn ts_specifier_is_type(&mut self) -> PResult<bool> {
        if !self.ts_at("type") {
            return Ok(false);
        }
        let saved = self.peek_state();
        self.bump(false)?;
        let r = if self.at(Punct::Comma) || self.at(Punct::RBrace) {
            false
        } else if self.at_kw(Keyword::As) {
            // `type as as x`, `type as as` are the modifier on `as`; `type as
            // x` renames an import named `type`.
            self.bump(false)?;
            self.at_kw(Keyword::As) || self.at(Punct::Comma) || self.at(Punct::RBrace)
        } else {
            true
        };
        self.peek_restore(saved);
        Ok(r)
    }

    /// An export declaration after `export`.
    fn ts_export_rest(&mut self, pos: u32) -> PResult<Stmt> {
        // `export = value;` — the module's one export, its default here.
        if self.at(Punct::Eq) {
            self.bump(true)?;
            let e = self.parse_assign_full()?;
            self.semicolon()?;
            self.record_export_name(&ModuleExportName::Ident("default".into()), pos)?;
            return Ok(Stmt::Export(Box::new(ExportDecl::Default(
                ExportDefault::Expr(e),
            ))));
        }
        if self.at(Punct::LBrace) {
            return self.ts_export_named(pos);
        }
        if let Some(word) = self.ts_word_name() {
            match &*word {
                // `export as namespace N;` (a UMD global, in declarations).
                "as" if self.ts_peek(|p| p.ts_at("namespace"))? => {
                    self.bump(false)?;
                    self.bump(false)?;
                    self.ident_name()?;
                    self.semicolon()?;
                    return Ok(Stmt::Empty);
                }
                "type" => {
                    let next_brace_or_star =
                        self.ts_peek(|p| p.at(Punct::LBrace) || p.at(Punct::Star))?;
                    if next_brace_or_star {
                        // `export type { A } [from "m"]`, `export type * from "m"`.
                        self.bump(false)?;
                        if self.at(Punct::Star) {
                            self.bump(false)?;
                            if self.eat_kw(Keyword::As, false)? {
                                self.bump(false)?;
                            }
                        } else {
                            self.ts_skip_balanced()?;
                        }
                        if self.eat_kw(Keyword::From, false)? {
                            self.bump(false)?;
                            self.parse_import_attributes()?;
                        }
                        self.semicolon()?;
                        return Ok(Stmt::Empty);
                    }
                    return Ok(self.ts_stmt()?.unwrap_or(Stmt::Empty));
                }
                "interface" => return Ok(self.ts_stmt()?.unwrap_or(Stmt::Empty)),
                "declare" => {
                    self.bump(false)?;
                    self.ts_declare()?;
                    return Ok(Stmt::Empty);
                }
                "import" => {
                    let ipos = self.cur().span.start;
                    self.bump(false)?;
                    let s = self.ts_import_equals(ipos, true)?;
                    return Ok(match s {
                        Stmt::VarDecl(d) => {
                            if let Some(Declarator {
                                id: Pattern::Ident(n),
                                ..
                            }) = d.decls.first()
                            {
                                self.record_export_name(&ModuleExportName::Ident(n.clone()), pos)?;
                            }
                            Stmt::Export(Box::new(ExportDecl::Decl(Box::new(Stmt::VarDecl(d)))))
                        }
                        // `export import x = require("m")`: the namespace,
                        // re-exported under `x`.
                        Stmt::Import(d) => {
                            if let Some(ImportSpecifier::Namespace(n)) = d.specifiers.first() {
                                let n = n.clone();
                                self.record_export_name(&ModuleExportName::Ident(n.clone()), pos)?;
                                self.ts_note_ref(&n);
                                let export = Stmt::Export(Box::new(ExportDecl::Named {
                                    specifiers: vec![ExportSpecifier {
                                        local: ModuleExportName::Ident(n.clone()),
                                        exported: ModuleExportName::Ident(n),
                                    }],
                                    source: None,
                                    attributes: Vec::new(),
                                }));
                                self.ts_emit(vec![Stmt::Import(d), export])
                            } else {
                                Stmt::Import(d)
                            }
                        }
                        other => other,
                    });
                }
                "enum" => {
                    let stmts = self.ts_enum(None)?;
                    return Ok(self.ts_export_lowered(stmts, pos)?);
                }
                "const" if self.ts_followed_by_word("enum") => {
                    self.bump(false)?;
                    let stmts = self.ts_const_enum(None)?;
                    return Ok(self.ts_export_lowered(stmts, pos)?);
                }
                "namespace" | "module" if self.ts_namespace_ahead()? => {
                    let stmts = self.ts_namespace(None)?;
                    return Ok(self.ts_export_lowered(stmts, pos)?);
                }
                "abstract" => {
                    self.bump(false)?;
                }
                "default" => return self.ts_export_default(pos),
                _ => {}
            }
        }
        let decl = self.parse_export_rest(pos)?;
        if let ExportDecl::Decl(d) = &decl {
            if matches!(**d, Stmt::Empty) {
                return Ok(Stmt::Empty);
            }
        }
        Ok(Stmt::Export(Box::new(decl)))
    }

    /// An exported enum or namespace: its `var` declaration is the export.
    fn ts_export_lowered(&mut self, stmts: Vec<Stmt>, pos: u32) -> PResult<Stmt> {
        let mut out = Vec::with_capacity(stmts.len());
        for s in stmts {
            if let Stmt::VarDecl(d) = &s {
                if let Some(Declarator {
                    id: Pattern::Ident(n),
                    ..
                }) = d.decls.first()
                {
                    self.record_export_name(&ModuleExportName::Ident(n.clone()), pos)?;
                }
                out.push(Stmt::Export(Box::new(ExportDecl::Decl(Box::new(s)))));
            } else {
                out.push(s);
            }
        }
        Ok(self.ts_emit(out))
    }

    /// `export default …`, the `default` current: TypeScript's forms
    /// (`abstract class`, `interface`, a function's overload signatures),
    /// else the JavaScript ones.
    fn ts_export_default(&mut self, pos: u32) -> PResult<Stmt> {
        let save = self.peek_state();
        self.bump(true)?; // `default`
        if self.ts_at("interface") && self.ts_next_is_ident_on_line()? {
            return Ok(self.ts_stmt()?.unwrap_or(Stmt::Empty));
        }
        if self.ts_at("abstract") && self.ts_peek(|p| p.at_kw(Keyword::Class))? {
            self.bump(false)?;
            self.peek_restore(save);
            // Re-read from `default`, the `abstract` now skipped over: the
            // JavaScript path takes `default class`.
            return self.ts_export_default_abstract(pos);
        }
        let is_fn = self.at_kw(Keyword::Function)
            || (self.at_kw(Keyword::Async)
                && self.ts_peek(|p| p.at_kw(Keyword::Function) && !p.cur().newline_before)?);
        if is_fn {
            let fstart = self.cur().span.start;
            let is_async = self.at_kw(Keyword::Async);
            if is_async {
                self.bump(false)?;
            }
            self.bump(false)?; // `function`
            let f = self.parse_function_rest(is_async, false, fstart)?;
            if std::mem::take(&mut self.ts_state.bodiless) {
                return Ok(Stmt::Empty);
            }
            self.record_export_name(&ModuleExportName::Ident("default".into()), pos)?;
            if let Some(n) = &f.name {
                let kind = if f.is_generator || f.is_async {
                    BindKind::GenFunction
                } else {
                    BindKind::Function
                };
                self.declare(&n.clone(), kind, fstart)?;
            }
            return Ok(Stmt::Export(Box::new(ExportDecl::Default(
                ExportDefault::Function(Box::new(f)),
            ))));
        }
        self.peek_restore(save);
        Ok(Stmt::Export(Box::new(self.parse_export_rest(pos)?)))
    }

    /// `export default abstract class …`: `default`, then `abstract`, then
    /// the class the JavaScript path reads.
    fn ts_export_default_abstract(&mut self, pos: u32) -> PResult<Stmt> {
        self.bump(true)?; // `default`
        self.bump(false)?; // `abstract`
        self.record_export_name(&ModuleExportName::Ident("default".into()), pos)?;
        let (cstart, decorators) = self.take_class_decorators()?;
        let cstart = cstart.unwrap_or_else(|| self.cur().span.start);
        self.bump(false)?; // `class`
        let c = self.parse_class_rest_dec(cstart, decorators)?;
        if let Some(n) = &c.name {
            self.declare(&n.clone(), BindKind::Class, cstart)?;
        }
        Ok(Stmt::Export(Box::new(ExportDecl::Default(
            ExportDefault::Class(Box::new(c)),
        ))))
    }

    /// `export { a, type B, c as d } [from "m"]`, the `{` current.
    fn ts_export_named(&mut self, pos: u32) -> PResult<Stmt> {
        self.bump(false)?;
        let mut specifiers = Vec::new();
        let mut had_types = false;
        while !self.at(Punct::RBrace) {
            let type_only = self.ts_specifier_is_type()?;
            if type_only {
                self.bump(false)?;
                had_types = true;
            }
            let local = self.parse_module_export_name()?;
            let exported = if self.eat_kw(Keyword::As, false)? {
                self.parse_module_export_name()?
            } else {
                local.clone()
            };
            if !type_only {
                specifiers.push(ExportSpecifier { local, exported });
            }
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
        if specifiers.is_empty() && had_types {
            return Ok(Stmt::Empty);
        }
        for sp in &specifiers {
            self.record_export_name(&sp.exported, pos)?;
        }
        if source.is_none() {
            for sp in &specifiers {
                match &sp.local {
                    ModuleExportName::Ident(n) => {
                        let n = n.clone();
                        self.ts_note_ref(&n);
                        self.pending_export_locals.push((n, pos));
                    }
                    ModuleExportName::Str(_) => {
                        return Err(SyntaxError::new(
                            "SyntaxError: a string export name requires a 'from' clause",
                            pos,
                        ));
                    }
                }
            }
        }
        Ok(Stmt::Export(Box::new(ExportDecl::Named {
            specifiers,
            source,
            attributes,
        })))
    }
}
