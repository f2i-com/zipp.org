//! Expressions, parameters and function bodies: where TypeScript's syntax
//! sits inside JavaScript's.
//!
//! Three prefixes are ambiguous until well past their start, and tsc decides
//! each by trying one reading and falling back: `(a): T => …` against a
//! parenthesized `(a)`, `<T>(x) => …` against the assertion `<T>x`, and
//! `f<T>(x)` against `f < T > (x)`. The same decisions are made here, in the
//! same way, with the parser's [`snapshot`](Parser::snapshot) as the
//! fallback point; tsc's lookahead rules (`isParenthesizedArrowFunction`,
//! `canFollowTypeArgumentsInExpression`) settle most cases from a token or
//! two, so the fallback is rarely taken.

use super::lower;
use crate::parse::ast::*;
use crate::parse::parser::{PResult, Parser, SyntaxError};
use crate::parse::token::{Keyword, Punct, TokenKind};

/// tsc's three-way answer to "is this an arrow function?".
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tri {
    True,
    False,
    Unknown,
}

impl<'s> Parser<'s> {
    /// At `(` or `<` where an AssignmentExpression starts: the arrow function
    /// it begins, if it begins one. `None` leaves the parser where it was,
    /// for the JavaScript reading. `no_ret` is set in a conditional's true
    /// branch (see `TsState::no_ret_arrow`).
    pub(crate) fn ts_arrow(
        &mut self,
        is_async: bool,
        start: u32,
        no_ret: bool,
    ) -> PResult<Option<Expr>> {
        let at = self.cur().span.start;
        let tri = if self.at(Punct::Lt) {
            self.ts_generic_arrow_tri()?
        } else {
            self.ts_paren_arrow_tri()?
        };
        match tri {
            Tri::False => Ok(None),
            Tri::True if !no_ret => self.ts_arrow_rest(is_async, start, false).map(Some),
            _ => {
                // Each `(` is tried once: a failed attempt is remembered, so
                // nested ambiguous parentheses cannot make the parse
                // exponential.
                if self.ts_state_not_arrow(at) {
                    return Ok(None);
                }
                let snap = self.snapshot();
                match self.ts_arrow_rest(is_async, start, no_ret) {
                    Ok(e) => Ok(Some(e)),
                    Err(_) => {
                        self.rewind(snap);
                        self.ts_mark_not_arrow(at);
                        Ok(None)
                    }
                }
            }
        }
    }

    fn ts_state_not_arrow(&self, at: u32) -> bool {
        self.ts_state.not_arrow.contains(&at)
    }

    fn ts_mark_not_arrow(&mut self, at: u32) {
        self.ts_state.not_arrow.insert(at);
    }

    /// At `<`: a generic arrow (`<T>(x: T) => x`) may start here only if a
    /// type parameter's name (or `const`) follows.
    fn ts_generic_arrow_tri(&mut self) -> PResult<Tri> {
        let r = self.ts_peek(|p| p.ts_at_name() || p.at_kw(Keyword::Const))?;
        Ok(if r { Tri::Unknown } else { Tri::False })
    }

    /// tsc's `isParenthesizedArrowFunctionExpressionWorker`, at `(`.
    fn ts_paren_arrow_tri(&mut self) -> PResult<Tri> {
        let saved = self.peek_state();
        let r = self.ts_paren_arrow_tri_inner();
        self.peek_restore(saved);
        Ok(r.unwrap_or(Tri::False))
    }

    fn ts_paren_arrow_tri_inner(&mut self) -> PResult<Tri> {
        self.bump(true)?; // `(`
        match self.cur().kind.as_punct() {
            Some(Punct::RParen) => {
                self.bump(false)?;
                return Ok(
                    if self.at(Punct::Arrow) || self.at(Punct::Colon) || self.at(Punct::LBrace) {
                        Tri::True
                    } else {
                        Tri::False
                    },
                );
            }
            Some(Punct::LBracket | Punct::LBrace) => return Ok(Tri::Unknown),
            Some(Punct::DotDotDot | Punct::At) => return Ok(Tri::True),
            _ => {}
        }
        if matches!(
            self.ts_word(),
            Some("public" | "private" | "protected" | "readonly" | "override")
        ) && self.ts_peek(|p| matches!(p.cur().kind, TokenKind::Ident { private: false, .. }))?
        {
            return Ok(Tri::True);
        }
        if !(self.is_binding_ident() || self.at_kw(Keyword::This)) {
            return Ok(Tri::False);
        }
        self.bump(false)?;
        Ok(match self.cur().kind.as_punct() {
            Some(Punct::Colon) => Tri::True,
            Some(Punct::Question) => {
                self.bump(false)?;
                if self.at(Punct::Colon)
                    || self.at(Punct::Comma)
                    || self.at(Punct::Eq)
                    || self.at(Punct::RParen)
                {
                    Tri::True
                } else {
                    Tri::False
                }
            }
            Some(Punct::Comma | Punct::Eq | Punct::RParen) => Tri::Unknown,
            _ => Tri::False,
        })
    }

    /// `<T>(params): R => body`, from the `<` or `(`.
    fn ts_arrow_rest(&mut self, is_async: bool, start: u32, no_ret: bool) -> PResult<Expr> {
        if self.at(Punct::Lt) {
            self.ts_type_params()?;
        }
        if !self.at(Punct::LParen) {
            return Err(self.err_here("SyntaxError: expected '('"));
        }
        // The parameters' default values are expressions of the enclosing
        // code, whose deferred cover records must not leak into it — and
        // `yield`/`await` expressions among them are errors in an arrow's
        // parameters, which the JavaScript cover grammar enforces the same way.
        let outer = std::mem::take(&mut self.cover);
        let params = self.ts_params();
        let inner = std::mem::replace(&mut self.cover, outer);
        let params = params?;
        self.ts_state.param_props.clear();
        self.ts_state.param_decs.clear();
        self.ts_state.param_types.clear();
        if let Some(e) = inner.yield_await.into_iter().next() {
            return Err(e);
        }
        if is_async {
            if let Some(e) = inner.await_ident.into_iter().next() {
                return Err(e);
            }
        } else {
            self.cover.await_ident.extend(inner.await_ident);
        }
        let mut had_ret = false;
        if self.at(Punct::Colon) {
            self.bump(false)?;
            self.ts_return_type()?;
            had_ret = true;
        }
        if !self.at(Punct::Arrow) || self.cur().newline_before {
            return Err(self.err_here("SyntaxError: expected '=>'"));
        }
        self.bump_before_operand()?;
        let e = self.finish_arrow(params, is_async, start)?;
        if no_ret && had_ret && !self.at(Punct::Colon) {
            return Err(self.err_here("SyntaxError: expected ':'"));
        }
        Ok(e)
    }

    /// A function's parameter list, TypeScript's way: `this: T` (dropped),
    /// `?`, `: T`, decorators (dropped: they are `experimentalDecorators`
    /// syntax) and parameter properties (`private x: T`, recorded in
    /// `TsState::param_props` for the constructor body).
    pub(crate) fn ts_params(&mut self) -> PResult<Params> {
        self.expect(Punct::LParen, true)?;
        let mut items = Vec::new();
        let mut props: Vec<Name> = Vec::new();
        let mut decs: Vec<(u32, Vec<Expr>)> = Vec::new();
        let mut types: Vec<super::legacy::Ser> = Vec::new();
        let legacy = self.ts_state.legacy_decorators;
        let metadata = self.ts_state.decorator_metadata;
        let mut first = true;
        while !self.at(Punct::RParen) {
            if self.at(Punct::At) {
                let d = self.parse_decorators()?;
                if legacy {
                    decs.push((items.len() as u32, d));
                }
            }
            let mut prop = false;
            while matches!(
                self.ts_word(),
                Some("public" | "private" | "protected" | "readonly" | "override")
            ) {
                let modifier = self.ts_peek(|p| {
                    matches!(p.cur().kind, TokenKind::Ident { private: false, .. })
                        || p.at(Punct::LBrace)
                        || p.at(Punct::LBracket)
                        || p.at(Punct::DotDotDot)
                })?;
                if !modifier {
                    break;
                }
                self.bump(false)?;
                prop = true;
            }
            // `this: T` types `this`; it is no parameter.
            if first && self.at_kw(Keyword::This) {
                self.bump(false)?;
                self.ts_annotation_opt()?;
                first = false;
                if !self.eat(Punct::Comma, true)? {
                    break;
                }
                continue;
            }
            first = false;
            if self.at(Punct::DotDotDot) {
                let pos = self.cur().span.start;
                self.bump_before_operand()?;
                let pat = self.parse_binding_pattern()?;
                let _ = self.eat(Punct::Question, false)?;
                let ann = self.ts_annotation_span()?;
                if metadata {
                    // A rest parameter's metadata is its element type.
                    let t = ann.map_or(super::legacy::Ser::Ctor("Object"), |(s, e)| {
                        let text = self.src_text(s, e).trim_end();
                        match text.strip_suffix("[]") {
                            Some(elem) => super::legacy::classify(elem),
                            None => super::legacy::Ser::Ctor("Object"),
                        }
                    });
                    types.push(t);
                }
                items.push(Pattern::Rest(Box::new(pat)));
                if self.at(Punct::Comma) {
                    return Err(SyntaxError::new(
                        "SyntaxError: rest parameter must be last",
                        pos,
                    ));
                }
                break;
            }
            let pat = self.parse_binding_pattern()?;
            let _ = self.eat(Punct::Question, false)?;
            let ann = self.ts_annotation_span()?;
            if metadata {
                types.push(ann.map_or(super::legacy::Ser::Ctor("Object"), |(s, e)| {
                    self.ts_classify(s, e)
                }));
            }
            if prop {
                if let Pattern::Ident(n) = &pat {
                    props.push(n.clone());
                }
            }
            items.push(if self.eat(Punct::Eq, true)? {
                Pattern::Assign {
                    left: Box::new(pat),
                    right: Box::new(self.parse_assign_full()?),
                }
            } else {
                pat
            });
            if !self.eat(Punct::Comma, true)? {
                break;
            }
        }
        self.expect(Punct::RParen, false)?;
        let simple = items.iter().all(|p| matches!(p, Pattern::Ident(_)));
        self.ts_state.param_props = props;
        if legacy {
            self.ts_state.param_decs = decs;
            self.ts_state.param_types = types;
        }
        Ok(Params { items, simple })
    }

    /// A function's return type and body. A signature without a body — an
    /// overload, an abstract method, an ambient function — gives an empty
    /// one and sets `TsState::bodiless` for the declaration to drop itself.
    pub(crate) fn ts_fn_body(&mut self, params: &Params) -> PResult<FnBody> {
        let props = std::mem::take(&mut self.ts_state.param_props);
        let decs = std::mem::take(&mut self.ts_state.param_decs);
        let types = std::mem::take(&mut self.ts_state.param_types);
        let mut ret = None;
        if self.at(Punct::Colon) {
            self.bump(false)?;
            let start = self.cur().span.start;
            self.ts_return_type()?;
            if self.ts_state.decorator_metadata {
                ret = Some(self.ts_classify(start, self.prev_end()));
            }
        }
        if !self.at(Punct::LBrace) {
            self.semicolon()?;
            self.ts_state.bodiless = true;
            return Ok(FnBody {
                directives: Vec::new(),
                stmts: Vec::new(),
            });
        }
        let mut body = self.parse_fn_body_with_params(Some(params))?;
        if !props.is_empty() {
            lower::param_props(&mut body, props.clone());
        }
        // Last, so it is this function's own for the class member reading it.
        self.ts_state.ctor_props = props;
        if self.ts_state.legacy_decorators {
            self.ts_state.fn_param_decs = decs;
            self.ts_state.fn_sig = self.ts_state.decorator_metadata.then_some((types, ret));
        }
        self.ts_state.bodiless = false;
        Ok(body)
    }

    /// After a declarator's name or pattern: `!` (definite assignment) and
    /// the type; whether there was a type.
    pub(crate) fn ts_binding_suffix(&mut self) -> PResult<bool> {
        if self.at(Punct::Bang) && !self.cur().newline_before {
            self.bump(false)?;
        }
        self.ts_annotation_opt()
    }

    /// `as` or `satisfies`, on the line of the expression it follows.
    #[inline]
    pub(crate) fn ts_at_as(&self) -> bool {
        !self.cur().newline_before && (self.at_kw(Keyword::As) || self.ts_at("satisfies"))
    }

    /// `as T`, `as const` or `satisfies T`, the operator current.
    pub(crate) fn ts_as_suffix(&mut self) -> PResult<()> {
        self.bump(false)?;
        if self.at_kw(Keyword::Const) {
            self.bump(false)?;
            return Ok(());
        }
        self.ts_type()
    }

    /// `<T>` of a type assertion (`<T>expr`), the `<` current; its operand
    /// is the caller's to read.
    pub(crate) fn ts_type_assertion_head(&mut self) -> PResult<()> {
        self.bump(false)?;
        self.ts_type()?;
        self.ts_expect_gt(true)
    }

    /// At `<` after a callee or `new` target: its type arguments, if they
    /// are (`f<T>(x)`, `` f<T>`t` ``, `f<T>` alone), consumed; otherwise
    /// nothing is, and the `<` is a less-than.
    pub(crate) fn ts_type_args_in_expr(&mut self) -> PResult<bool> {
        let snap = self.snapshot();
        match self.ts_expr_type_args() {
            Ok(true) => Ok(true),
            _ => {
                self.rewind(snap);
                Ok(false)
            }
        }
    }

    fn ts_expr_type_args(&mut self) -> PResult<bool> {
        // `<`, or the first of `<<` when the first type argument is a generic
        // function type (`f<<T>(x: T) => T>(g)`).
        if self.at(Punct::Shl) {
            self.split_first()?;
        } else {
            self.bump(false)?;
        }
        self.ts_with_cond(true, |p| {
            loop {
                p.ts_type()?;
                if !p.eat(Punct::Comma, false)? {
                    break;
                }
            }
            Ok(())
        })?;
        // The list closes on a `>` of its own: `a < b >= c` and `a < b >> c`
        // are comparisons and shifts.
        if !self.at(Punct::Gt) {
            return Ok(false);
        }
        self.bump(false)?;
        Ok(self.ts_can_follow_type_args())
    }

    /// tsc's `canFollowTypeArgumentsInExpression`.
    fn ts_can_follow_type_args(&self) -> bool {
        match &self.cur().kind {
            TokenKind::Punct(Punct::LParen) | TokenKind::Template { .. } => return true,
            TokenKind::Punct(Punct::Lt | Punct::Gt | Punct::Plus | Punct::Minus) => return false,
            _ => {}
        }
        self.cur().newline_before || self.ts_at_binary_operator() || !self.ts_at_expression_start()
    }

    fn ts_at_binary_operator(&self) -> bool {
        use Punct as P;
        if self.at_kw(Keyword::In)
            || self.at_kw(Keyword::Instanceof)
            || self.at_kw(Keyword::As)
            || self.ts_at("satisfies")
        {
            return true;
        }
        matches!(
            self.cur().kind.as_punct(),
            Some(
                P::Pipe
                    | P::Caret
                    | P::Amp
                    | P::EqEq
                    | P::NotEq
                    | P::EqEqEq
                    | P::NotEqEq
                    | P::LtEq
                    | P::GtEq
                    | P::Shl
                    | P::Shr
                    | P::UShr
                    | P::Star
                    | P::Slash
                    | P::Percent
                    | P::StarStar
                    | P::AmpAmp
                    | P::PipePipe
                    | P::QuestionQuestion
            )
        )
    }

    fn ts_at_expression_start(&self) -> bool {
        use Punct as P;
        match &self.cur().kind {
            TokenKind::Ident { .. }
            | TokenKind::Num(_)
            | TokenKind::BigInt(_)
            | TokenKind::Str(_)
            | TokenKind::Regex { .. }
            | TokenKind::Template { .. } => true,
            TokenKind::Punct(p) => matches!(
                p,
                P::LParen
                    | P::LBracket
                    | P::LBrace
                    | P::Slash
                    | P::SlashEq
                    | P::Plus
                    | P::Minus
                    | P::Tilde
                    | P::Bang
                    | P::PlusPlus
                    | P::MinusMinus
                    | P::Lt
                    | P::At
            ),
            TokenKind::Eof => false,
        }
    }

    /// `e`, just read as `E.A`: the constant value of member `A` when `E` is a
    /// `const enum` this file declared (and no nearer binding named `E`
    /// hides it).
    pub(crate) fn ts_inline_const_enum(&mut self, e: &mut Expr) {
        let Expr::Member(m) = &*e else { return };
        let (Expr::Ident(enum_name), MemberProp::Ident(member)) = (&m.object, &m.prop) else {
            return;
        };
        if !self.ts_state.const_enums.contains(enum_name) || self.scopes.shadowed(enum_name) {
            return;
        }
        let value = self.ts_state.enums.get(enum_name).and_then(|members| {
            members
                .iter()
                .rev()
                .find(|(k, _)| k == member)
                .map(|(_, v)| v.clone())
        });
        *e = match value {
            Some(lower::Const::Num(n)) => Expr::Num(n),
            Some(lower::Const::Str(s)) => Expr::Str(crate::parse::token::StrVal::Utf8(s)),
            None => return,
        };
    }
}
