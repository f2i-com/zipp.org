//! The type grammar: every TypeScript type, read, checked and dropped.
//!
//! Nothing here builds a node. A type is consumed token by token by a
//! recursive descent that follows tsc's own (`parseType` and below), so a
//! type ends exactly where tsc's does — which is what decides where the
//! expression after an annotation begins — and a malformed one is a
//! SyntaxError rather than silently swallowed text.

use crate::parse::parser::{PResult, Parser};
use crate::parse::token::{Keyword, Punct, TokenKind};

impl<'s> Parser<'s> {
    /// `Type`.
    pub(crate) fn ts_type(&mut self) -> PResult<()> {
        self.with_syntax_recursion(|p| p.ts_type_inner())
    }

    fn ts_type_inner(&mut self) -> PResult<()> {
        if self.ts_at_function_type_start()? {
            return self.ts_function_type();
        }
        self.ts_union_type()?;
        // `Check extends Extends ? True : False`, where the `extends` is on
        // the line of the check type.
        if !self.ts_state.no_cond && !self.cur().newline_before && self.at_kw(Keyword::Extends) {
            self.bump(false)?;
            self.ts_with_cond(false, |p| p.ts_type())?;
            self.expect(Punct::Question, false)?;
            self.ts_with_cond(true, |p| p.ts_type())?;
            self.expect(Punct::Colon, false)?;
            self.ts_with_cond(true, |p| p.ts_type())?;
        }
        Ok(())
    }

    /// `: Type`, if a colon is next.
    pub(crate) fn ts_annotation_opt(&mut self) -> PResult<bool> {
        if self.at(Punct::Colon) {
            self.bump(false)?;
            self.ts_type()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// A return type: a type, or a type predicate (`x is T`, `this is T`,
    /// `asserts x`, `asserts x is T`).
    pub(crate) fn ts_return_type(&mut self) -> PResult<()> {
        if self.ts_at("asserts") {
            let predicate = self.ts_peek(|p| {
                !p.cur().newline_before
                    && matches!(p.cur().kind, TokenKind::Ident { private: false, .. })
            })?;
            if predicate {
                self.bump(false)?; // `asserts`
                self.bump(false)?; // the parameter, or `this`
                if self.ts_at("is") && !self.cur().newline_before {
                    self.bump(false)?;
                    self.ts_type()?;
                }
                return Ok(());
            }
        }
        self.ts_type()?;
        // `x is T`: the type just read was the parameter's name.
        if self.ts_at("is") && !self.cur().newline_before {
            self.bump(false)?;
            self.ts_type()?;
        }
        Ok(())
    }

    fn ts_union_type(&mut self) -> PResult<()> {
        let _ = self.eat(Punct::Pipe, false)?;
        self.ts_intersection_type()?;
        while self.at(Punct::Pipe) {
            self.bump(false)?;
            self.ts_intersection_type()?;
        }
        Ok(())
    }

    fn ts_intersection_type(&mut self) -> PResult<()> {
        let _ = self.eat(Punct::Amp, false)?;
        self.ts_type_operator()?;
        while self.at(Punct::Amp) {
            self.bump(false)?;
            self.ts_type_operator()?;
        }
        Ok(())
    }

    /// `keyof T`, `unique symbol`, `readonly T[]`, `infer U`, or a postfix
    /// type. A function type may stand here too (tsc reads it, then reports
    /// it must be parenthesized).
    fn ts_type_operator(&mut self) -> PResult<()> {
        if self.ts_at_function_type_start()? {
            return self.ts_function_type();
        }
        if let Some(w) = self.ts_word() {
            match w {
                "keyof" | "unique" | "readonly" => {
                    if self.ts_peek(|p| p.ts_at_type_start())? {
                        self.bump(false)?;
                        return self.with_syntax_recursion(|p| p.ts_type_operator());
                    }
                }
                "infer" => {
                    if self.ts_peek(|p| p.ts_at_name())? {
                        self.bump(false)?;
                        self.bump(false)?; // the type parameter
                        return self.ts_infer_constraint();
                    }
                }
                _ => {}
            }
        }
        self.ts_postfix_type()
    }

    /// `infer U extends C`: the constraint belongs to the `infer` unless it is
    /// really the `extends` of a conditional type (`T extends infer U extends
    /// C ? X : Y` in a position that allows one).
    fn ts_infer_constraint(&mut self) -> PResult<()> {
        if !self.at_kw(Keyword::Extends) {
            return Ok(());
        }
        let snap = self.snapshot();
        self.bump(false)?;
        let ok = self.ts_with_cond(false, |p| p.ts_type()).is_ok();
        if ok && (self.ts_state.no_cond || !self.at(Punct::Question)) {
            return Ok(());
        }
        self.rewind(snap);
        Ok(())
    }

    /// A primary type and its `[]` / `[K]` suffixes, which a line break ends.
    fn ts_postfix_type(&mut self) -> PResult<()> {
        self.ts_primary_type()?;
        while self.at(Punct::LBracket) && !self.cur().newline_before {
            self.bump(false)?;
            if !self.at(Punct::RBracket) {
                self.ts_with_cond(true, |p| p.ts_type())?;
            }
            self.expect(Punct::RBracket, false)?;
        }
        Ok(())
    }

    fn ts_primary_type(&mut self) -> PResult<()> {
        match &self.cur().kind {
            TokenKind::Ident {
                kw,
                had_escape,
                private: false,
                ..
            } => {
                let kw = if *had_escape { Keyword::None } else { *kw };
                match kw {
                    Keyword::Typeof => {
                        self.bump(false)?;
                        if self.at_kw(Keyword::Import) {
                            return self.ts_import_type();
                        }
                        self.ts_entity_name()?;
                        self.ts_type_args_on_line()
                    }
                    Keyword::Import => self.ts_import_type(),
                    Keyword::Void
                    | Keyword::Null
                    | Keyword::This
                    | Keyword::True
                    | Keyword::False => {
                        self.bump(false)?;
                        Ok(())
                    }
                    _ if kw.is_always_reserved() => {
                        Err(self.err_here("SyntaxError: type expected"))
                    }
                    _ => {
                        self.ts_entity_name()?;
                        self.ts_type_args_on_line()
                    }
                }
            }
            TokenKind::Str(_) | TokenKind::Num(_) | TokenKind::BigInt(_) => {
                self.bump(false)?;
                Ok(())
            }
            TokenKind::Template { .. } => self.ts_template_type(),
            TokenKind::Punct(Punct::Minus) => {
                self.bump(false)?;
                if !matches!(self.cur().kind, TokenKind::Num(_) | TokenKind::BigInt(_)) {
                    return Err(self.err_here("SyntaxError: a numeric literal type was expected"));
                }
                self.bump(false)?;
                Ok(())
            }
            TokenKind::Punct(Punct::LBrace) => {
                if self.ts_at_mapped_type()? {
                    self.ts_mapped_type()
                } else {
                    self.ts_type_literal()
                }
            }
            TokenKind::Punct(Punct::LBracket) => self.ts_tuple_type(),
            TokenKind::Punct(Punct::LParen) => {
                self.bump(false)?;
                self.ts_with_cond(true, |p| p.ts_type())?;
                self.expect(Punct::RParen, false)?;
                Ok(())
            }
            // JSDoc's `?` and `*` (unknown and any), which tsc reads too.
            TokenKind::Punct(Punct::Question | Punct::Star) => {
                self.bump(false)?;
                Ok(())
            }
            _ => Err(self.err_here("SyntaxError: type expected")),
        }
    }

    /// `A.B.C` — the dotted name of a type or a `typeof` query.
    fn ts_entity_name(&mut self) -> PResult<()> {
        if !matches!(self.cur().kind, TokenKind::Ident { private: false, .. }) {
            return Err(self.err_here("SyntaxError: type expected"));
        }
        self.bump(false)?;
        while self.at(Punct::Dot) {
            self.bump(false)?;
            if !matches!(self.cur().kind, TokenKind::Ident { .. }) {
                return Err(self.err_here("SyntaxError: identifier expected"));
            }
            self.bump(false)?;
        }
        Ok(())
    }

    /// Type arguments after a type's name, which must be on its line.
    fn ts_type_args_on_line(&mut self) -> PResult<()> {
        if self.at(Punct::Lt) && !self.cur().newline_before {
            self.ts_type_args()?;
        }
        Ok(())
    }

    /// `import("m").A.B<T>`, and `typeof import("m")`.
    fn ts_import_type(&mut self) -> PResult<()> {
        self.bump(false)?; // `import`
        self.expect(Punct::LParen, false)?;
        if !matches!(self.cur().kind, TokenKind::Str(_)) {
            return Err(self.err_here("SyntaxError: string literal expected"));
        }
        self.bump(false)?;
        if self.eat(Punct::Comma, false)? && !self.at(Punct::RParen) {
            // The import attributes object: `{ with: { "resolution-mode": … } }`.
            self.ts_skip_balanced()?;
            let _ = self.eat(Punct::Comma, false)?;
        }
        self.expect(Punct::RParen, false)?;
        while self.at(Punct::Dot) {
            self.bump(false)?;
            self.bump(false)?;
        }
        self.ts_type_args_on_line()
    }

    /// `` `a${T}b` ``.
    fn ts_template_type(&mut self) -> PResult<()> {
        let TokenKind::Template { tail, .. } = self.cur().kind else {
            unreachable!()
        };
        self.bump(false)?;
        let mut done = tail;
        while !done {
            self.ts_with_cond(true, |p| p.ts_type())?;
            if !self.at(Punct::RBrace) {
                return Err(self.err_here("SyntaxError: expected '}' in template literal type"));
            }
            let tok = self.resume_template()?;
            let TokenKind::Template { tail, .. } = tok.kind else {
                return Err(self.err_here("SyntaxError: malformed template literal type"));
            };
            done = tail;
        }
        Ok(())
    }

    /// `[A, B?, ...C[]]` and the labelled `[name: A, rest?: B]`.
    fn ts_tuple_type(&mut self) -> PResult<()> {
        self.bump(false)?; // `[`
        while !self.at(Punct::RBracket) {
            let _ = self.eat(Punct::DotDotDot, false)?;
            // `name:` / `name?:` — a label, which only an identifier followed
            // by one of those can be.
            if matches!(self.cur().kind, TokenKind::Ident { private: false, .. }) {
                let labelled = self.ts_peek(|p| p.at(Punct::Colon) || p.at(Punct::Question))?;
                if labelled {
                    let snap = self.snapshot();
                    self.bump(false)?;
                    let _ = self.eat(Punct::Question, false)?;
                    if self.at(Punct::Colon) {
                        self.bump(false)?;
                    } else {
                        // `[A?]`: an optional element, not a label.
                        self.rewind(snap);
                    }
                }
            }
            self.ts_with_cond(true, |p| p.ts_type())?;
            let _ = self.eat(Punct::Question, false)?;
            if !self.eat(Punct::Comma, false)? {
                break;
            }
        }
        self.expect(Punct::RBracket, false)?;
        Ok(())
    }

    /// `{ [K in T as N]?: V }`, with `+`/`-` on `readonly` and `?`.
    fn ts_at_mapped_type(&mut self) -> PResult<bool> {
        let saved = self.peek_state();
        self.bump(false)?; // `{`
        let mut r = false;
        if self.at(Punct::Plus) || self.at(Punct::Minus) {
            self.bump(false)?;
            r = self.ts_at("readonly");
        } else {
            if self.ts_at("readonly") {
                self.bump(false)?;
            }
            if self.at(Punct::LBracket) {
                self.bump(false)?;
                if self.ts_at_name() {
                    self.bump(false)?;
                    r = self.at_kw(Keyword::In);
                }
            }
        }
        self.peek_restore(saved);
        Ok(r)
    }

    fn ts_mapped_type(&mut self) -> PResult<()> {
        self.bump(false)?; // `{`
        if self.at(Punct::Plus) || self.at(Punct::Minus) {
            self.bump(false)?;
        }
        if self.ts_at("readonly") {
            self.bump(false)?;
        }
        self.expect(Punct::LBracket, false)?;
        self.bump(false)?; // the key's name
        if !self.eat_kw(Keyword::In, false)? {
            return Err(self.err_here("SyntaxError: expected 'in'"));
        }
        self.ts_with_cond(true, |p| p.ts_type())?;
        if self.eat_kw(Keyword::As, false)? {
            self.ts_with_cond(true, |p| p.ts_type())?;
        }
        self.expect(Punct::RBracket, false)?;
        if self.at(Punct::Plus) || self.at(Punct::Minus) {
            self.bump(false)?;
            self.expect(Punct::Question, false)?;
        } else {
            let _ = self.eat(Punct::Question, false)?;
        }
        if self.eat(Punct::Colon, false)? {
            self.ts_with_cond(true, |p| p.ts_type())?;
        }
        if !self.eat(Punct::Semi, false)? {
            let _ = self.eat(Punct::Comma, false)?;
        }
        self.expect(Punct::RBrace, false)?;
        Ok(())
    }

    /// `{ members }` — a type literal, or an interface's body.
    pub(crate) fn ts_type_literal(&mut self) -> PResult<()> {
        self.expect(Punct::LBrace, false)?;
        self.ts_with_cond(true, |p| {
            while !p.at(Punct::RBrace) && !p.at_eof() {
                p.with_syntax_recursion(|p| p.ts_type_member())?;
            }
            Ok(())
        })?;
        self.expect(Punct::RBrace, false)?;
        Ok(())
    }

    /// One member of a type literal or interface: a property, method, call,
    /// construct or index signature, or a `get`/`set` accessor.
    fn ts_type_member(&mut self) -> PResult<()> {
        if self.at(Punct::LParen) || self.at(Punct::Lt) {
            self.ts_signature_rest()?;
            return self.ts_type_member_end();
        }
        if self.at_kw(Keyword::New) && self.ts_peek(|p| p.at(Punct::LParen) || p.at(Punct::Lt))? {
            self.bump(false)?;
            self.ts_signature_rest()?;
            return self.ts_type_member_end();
        }
        // Modifiers, each of which is also a property name when what follows
        // cannot follow a modifier.
        while let Some(w) = self.ts_word() {
            if !matches!(
                w,
                "readonly"
                    | "public"
                    | "private"
                    | "protected"
                    | "static"
                    | "declare"
                    | "abstract"
                    | "override"
                    | "export"
                    | "get"
                    | "set"
            ) {
                break;
            }
            if !self.ts_peek(|p| p.ts_can_follow_modifier())? {
                break;
            }
            self.bump(false)?;
        }
        if self.at(Punct::LBracket) && self.ts_at_index_signature()? {
            self.ts_index_signature()?;
            return self.ts_type_member_end();
        }
        self.ts_member_name()?;
        let _ = self.eat(Punct::Question, false)?;
        if self.at(Punct::LParen) || self.at(Punct::Lt) {
            self.ts_signature_rest()?;
        } else {
            self.ts_annotation_opt()?;
        }
        self.ts_type_member_end()
    }

    /// A member ends at `;`, `,`, the closing `}`, or a line break.
    fn ts_type_member_end(&mut self) -> PResult<()> {
        if self.eat(Punct::Semi, false)? || self.eat(Punct::Comma, false)? {
            return Ok(());
        }
        if self.at(Punct::RBrace) || self.cur().newline_before {
            return Ok(());
        }
        Err(self.err_here("SyntaxError: expected ';'"))
    }

    /// What may follow a modifier word for it to be one: a member's name or
    /// the bracket of a computed name or index signature, on the same line.
    pub(crate) fn ts_can_follow_modifier(&self) -> bool {
        if self.cur().newline_before {
            return false;
        }
        matches!(
            self.cur().kind,
            TokenKind::Ident { .. } | TokenKind::Str(_) | TokenKind::Num(_) | TokenKind::BigInt(_)
        ) || self.at(Punct::LBracket)
            || self.at(Punct::LBrace)
            || self.at(Punct::Star)
            || self.at(Punct::DotDotDot)
    }

    /// A member's name: an identifier (any word), a string or number, or a
    /// computed `[expr]`.
    fn ts_member_name(&mut self) -> PResult<()> {
        match &self.cur().kind {
            TokenKind::Ident { .. }
            | TokenKind::Str(_)
            | TokenKind::Num(_)
            | TokenKind::BigInt(_) => {
                self.bump(false)?;
                Ok(())
            }
            TokenKind::Punct(Punct::LBracket) => {
                self.bump(true)?;
                self.with_in(|p| p.parse_assign_full())?;
                self.expect(Punct::RBracket, false)?;
                Ok(())
            }
            _ => Err(self.err_here("SyntaxError: property name expected")),
        }
    }

    /// At `[`: an index signature (`[key: string]: T`) rather than a
    /// computed name.
    pub(crate) fn ts_at_index_signature(&mut self) -> PResult<bool> {
        let saved = self.peek_state();
        self.bump(false)?; // `[`
        let r = if self.at(Punct::DotDotDot) || self.at(Punct::RBracket) {
            true
        } else if matches!(self.cur().kind, TokenKind::Ident { private: false, .. }) {
            self.bump(false)?;
            self.at(Punct::Colon) || self.at(Punct::Comma)
        } else {
            false
        };
        self.peek_restore(saved);
        Ok(r)
    }

    /// `[key: K]: V` (the `[` current), with no member separator.
    pub(crate) fn ts_index_signature(&mut self) -> PResult<()> {
        self.bump(false)?; // `[`
        while !self.at(Punct::RBracket) {
            let _ = self.eat(Punct::DotDotDot, false)?;
            self.bump(false)?; // the key's name
            self.ts_annotation_opt()?;
            if !self.eat(Punct::Comma, false)? {
                break;
            }
        }
        self.expect(Punct::RBracket, false)?;
        let _ = self.eat(Punct::Question, false)?;
        self.ts_annotation_opt()?;
        Ok(())
    }

    /// `<T>(params): R` of a call, construct or method signature.
    fn ts_signature_rest(&mut self) -> PResult<()> {
        if self.at(Punct::Lt) {
            self.ts_type_params()?;
        }
        self.ts_skip_params()?;
        if self.eat(Punct::Colon, false)? {
            self.ts_return_type()?;
        }
        Ok(())
    }

    /// `(a: A, b?: B, ...c: C[])` of a signature or a function type, whose
    /// parameters bind nothing.
    pub(crate) fn ts_skip_params(&mut self) -> PResult<()> {
        self.expect(Punct::LParen, false)?;
        while !self.at(Punct::RParen) {
            if self.at(Punct::At) {
                self.parse_decorators()?;
            }
            while matches!(
                self.ts_word(),
                Some("public" | "private" | "protected" | "readonly" | "override")
            ) && self.ts_peek(|p| {
                !p.at(Punct::Colon)
                    && !p.at(Punct::Comma)
                    && !p.at(Punct::RParen)
                    && !p.at(Punct::Question)
                    && !p.at(Punct::Eq)
            })? {
                self.bump(false)?;
            }
            let _ = self.eat(Punct::DotDotDot, false)?;
            if self.at(Punct::LBracket) || self.at(Punct::LBrace) {
                self.ts_skip_balanced()?;
            } else if matches!(self.cur().kind, TokenKind::Ident { private: false, .. }) {
                self.bump(false)?;
            } else {
                return Err(self.err_here("SyntaxError: parameter expected"));
            }
            let _ = self.eat(Punct::Question, false)?;
            self.ts_annotation_opt()?;
            if self.at(Punct::Eq) {
                self.bump(true)?;
                self.parse_assign_full()?;
            }
            if !self.eat(Punct::Comma, false)? {
                break;
            }
        }
        self.expect(Punct::RParen, false)?;
        Ok(())
    }

    /// Does a function or constructor type start here: `<T>(…) =>`,
    /// `(…) =>`, `new (…) =>`, `abstract new (…) =>`?
    fn ts_at_function_type_start(&mut self) -> PResult<bool> {
        match &self.cur().kind {
            TokenKind::Punct(Punct::Lt) => Ok(true),
            TokenKind::Punct(Punct::LParen) => self.ts_paren_starts_function_type(),
            TokenKind::Ident { .. } if self.at_kw(Keyword::New) => Ok(true),
            TokenKind::Ident { .. } if self.ts_at("abstract") => {
                self.ts_peek(|p| p.at_kw(Keyword::New))
            }
            _ => Ok(false),
        }
    }

    /// tsc's `isUnambiguouslyStartOfFunctionType`, at `(`.
    fn ts_paren_starts_function_type(&mut self) -> PResult<bool> {
        let saved = self.peek_state();
        let r = self.ts_paren_starts_function_type_inner();
        self.peek_restore(saved);
        Ok(r.unwrap_or(false))
    }

    fn ts_paren_starts_function_type_inner(&mut self) -> PResult<bool> {
        self.bump(false)?; // `(`
        if self.at(Punct::RParen) || self.at(Punct::DotDotDot) {
            return Ok(true);
        }
        // skipParameterStart
        while matches!(
            self.ts_word(),
            Some("public" | "private" | "protected" | "readonly")
        ) && self.ts_peek(|p| {
            matches!(p.cur().kind, TokenKind::Ident { .. })
                || p.at(Punct::LBrace)
                || p.at(Punct::LBracket)
        })? {
            self.bump(false)?;
        }
        if matches!(self.cur().kind, TokenKind::Ident { private: false, .. }) {
            self.bump(false)?;
        } else if self.at(Punct::LBracket) || self.at(Punct::LBrace) {
            self.ts_skip_balanced()?;
        } else {
            return Ok(false);
        }
        if self.at(Punct::Colon)
            || self.at(Punct::Comma)
            || self.at(Punct::Question)
            || self.at(Punct::Eq)
        {
            return Ok(true);
        }
        if self.at(Punct::RParen) {
            self.bump(false)?;
            return Ok(self.at(Punct::Arrow));
        }
        Ok(false)
    }

    /// `abstract new <T>(params) => R`, with `abstract`, `new` and the type
    /// parameters optional.
    fn ts_function_type(&mut self) -> PResult<()> {
        if self.ts_at("abstract") {
            self.bump(false)?;
        }
        if self.at_kw(Keyword::New) {
            self.bump(false)?;
        }
        if self.at(Punct::Lt) {
            self.ts_type_params()?;
        }
        self.ts_skip_params()?;
        self.expect(Punct::Arrow, false)?;
        // tsc's `parseReturnType` allows conditional types again: `A extends
        // <V>() => V extends B ? 1 : 2 ? X : Y` has one in the return type.
        self.ts_with_cond(true, |p| p.ts_return_type())
    }

    /// `<T extends C = D, const U, in out V>`.
    pub(crate) fn ts_type_params(&mut self) -> PResult<()> {
        self.bump(false)?; // `<`
        while !self.ts_at_gt() {
            while matches!(self.ts_word(), Some("const" | "in" | "out"))
                && self.ts_peek(|p| p.ts_at_name())?
            {
                self.bump(false)?;
            }
            if !self.ts_at_name() {
                return Err(self.err_here("SyntaxError: type parameter name expected"));
            }
            self.bump(false)?;
            if self.eat_kw(Keyword::Extends, false)? {
                self.ts_type()?;
            }
            if self.eat(Punct::Eq, false)? {
                self.ts_type()?;
            }
            if !self.eat(Punct::Comma, false)? {
                break;
            }
        }
        self.ts_expect_gt(false)
    }

    /// The current token begins with `>`.
    fn ts_at_gt(&self) -> bool {
        matches!(
            self.cur().kind.as_punct(),
            Some(Punct::Gt | Punct::Shr | Punct::UShr | Punct::GtEq | Punct::ShrEq | Punct::UShrEq)
        )
    }

    /// `<A, B>` in a type.
    pub(crate) fn ts_type_args(&mut self) -> PResult<()> {
        self.bump(false)?; // `<`
        self.ts_with_cond(true, |p| {
            while !p.ts_at_gt() {
                p.ts_type()?;
                if !p.eat(Punct::Comma, false)? {
                    break;
                }
            }
            Ok(())
        })?;
        self.ts_expect_gt(false)
    }

    /// Can a type start at the current token?
    pub(crate) fn ts_at_type_start(&self) -> bool {
        match &self.cur().kind {
            TokenKind::Ident {
                private: false, kw, ..
            } => {
                !kw.is_always_reserved()
                    || matches!(
                        kw,
                        Keyword::Void
                            | Keyword::Null
                            | Keyword::This
                            | Keyword::True
                            | Keyword::False
                            | Keyword::Typeof
                            | Keyword::New
                            | Keyword::Import
                    )
            }
            TokenKind::Str(_)
            | TokenKind::Num(_)
            | TokenKind::BigInt(_)
            | TokenKind::Template { .. } => true,
            TokenKind::Punct(p) => matches!(
                p,
                Punct::LBrace
                    | Punct::LBracket
                    | Punct::LParen
                    | Punct::Lt
                    | Punct::Minus
                    | Punct::Pipe
                    | Punct::Amp
                    | Punct::Question
                    | Punct::Star
            ),
            _ => false,
        }
    }

    /// Skip a bracketed run — `{…}`, `(…)` or `[…]`, whatever is inside, the
    /// current token being the opener — keeping template literals whole.
    /// For text that is only ever types or patterns (an ambient body, a
    /// destructured parameter in a signature), where no regex can occur.
    pub(crate) fn ts_skip_balanced(&mut self) -> PResult<()> {
        // `t` marks an open template substitution, whose `}` resumes it.
        let mut stack: Vec<u8> = Vec::with_capacity(8);
        loop {
            match &self.cur().kind {
                TokenKind::Eof => return Err(self.err_here("SyntaxError: unexpected end of input")),
                TokenKind::Punct(Punct::LBrace) => stack.push(b'{'),
                TokenKind::Punct(Punct::LParen) => stack.push(b'('),
                TokenKind::Punct(Punct::LBracket) => stack.push(b'['),
                TokenKind::Template { tail: false, .. } => stack.push(b't'),
                TokenKind::Punct(Punct::RBrace) => {
                    if stack.last() == Some(&b't') {
                        let tok = self.resume_template()?;
                        if matches!(tok.kind, TokenKind::Template { tail: true, .. }) {
                            stack.pop();
                        }
                        continue;
                    }
                    if stack.pop() != Some(b'{') {
                        return Err(self.unexpected_here());
                    }
                }
                TokenKind::Punct(Punct::RParen) => {
                    if stack.pop() != Some(b'(') {
                        return Err(self.unexpected_here());
                    }
                }
                TokenKind::Punct(Punct::RBracket) => {
                    if stack.pop() != Some(b'[') {
                        return Err(self.unexpected_here());
                    }
                }
                _ => {}
            }
            self.bump(false)?;
            if stack.is_empty() {
                return Ok(());
            }
        }
    }
}
