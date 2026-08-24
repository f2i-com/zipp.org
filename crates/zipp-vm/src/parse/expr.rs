//! Expressions.
//!
//! Precedence climbing for the binary operators, recursive descent for the
//! rest, and the spec's own cover-grammar technique for the three ambiguous
//! prefixes. No backtracking anywhere: the lexer is never rewound, and a token
//! is never scanned twice.
//!
//! ## The cover grammars
//!
//! **`( … )`** is a parenthesized expression until a `=>` proves it was an
//! arrow's parameter list, and the `=>` can be arbitrarily far away. So the
//! contents are parsed ONCE as the permissive superset — a comma-separated list
//! that also admits `...rest` and trailing commas — recording the constructs
//! that are legal in only one of the two readings. When the `)` is consumed the
//! next token settles it, and exactly one recorded error set is discharged.
//!
//! **`{ … }`** is an object literal until a `=` proves it a destructuring
//! target. `{a = 1}` is a `CoverInitializedName`: a SyntaxError as a literal,
//! legal as a target, so it parses into [`ObjectMember::Prop::init`] and the
//! error is recorded pending the decision.
//!
//! **`async`** is an identifier until what follows proves otherwise, and
//! `async (a, b)` is a call to a function named `async` unless a `=>` follows
//! the `)`. It reuses the `( … )` machinery with `is_async` set.

use super::ast::*;
use super::parser::{Ctx, PResult, Parser, SyntaxError};
use super::token::{Keyword, NumKind, Punct, StrVal, TokenKind};

/// Binding power of the binary operators, highest binds tightest. `**` is
/// right-associative and handled by the caller; everything here is left.
fn binary_prec(p: Punct, ctx: &Ctx) -> Option<(u8, BinaryOp)> {
    use BinaryOp as B;
    use Punct as P;
    let v = match p {
        P::PipePipe | P::AmpAmp | P::QuestionQuestion => return None, // logical: separate
        P::Pipe => (5, B::BitOr),
        P::Caret => (6, B::BitXor),
        P::Amp => (7, B::BitAnd),
        P::EqEq => (8, B::Eq),
        P::NotEq => (8, B::NotEq),
        P::EqEqEq => (8, B::StrictEq),
        P::NotEqEq => (8, B::StrictNotEq),
        P::Lt => (9, B::Lt),
        P::LtEq => (9, B::LtEq),
        P::Gt => (9, B::Gt),
        P::GtEq => (9, B::GtEq),
        P::Shl => (10, B::Shl),
        P::Shr => (10, B::Shr),
        P::UShr => (10, B::UShr),
        P::Plus => (11, B::Add),
        P::Minus => (11, B::Sub),
        P::Star => (12, B::Mul),
        P::Slash => (12, B::Div),
        P::Percent => (12, B::Rem),
        P::StarStar => (13, B::Exp),
        _ => return None,
    };
    let _ = ctx;
    Some(v)
}

fn assign_op(p: Punct) -> Option<AssignOp> {
    use AssignOp as A;
    Some(match p {
        Punct::Eq => A::Assign,
        Punct::PlusEq => A::Add,
        Punct::MinusEq => A::Sub,
        Punct::StarEq => A::Mul,
        Punct::SlashEq => A::Div,
        Punct::PercentEq => A::Rem,
        Punct::StarStarEq => A::Exp,
        Punct::ShlEq => A::Shl,
        Punct::ShrEq => A::Shr,
        Punct::UShrEq => A::UShr,
        Punct::AmpEq => A::BitAnd,
        Punct::PipeEq => A::BitOr,
        Punct::CaretEq => A::BitXor,
        Punct::AmpAmpEq => A::LogicalAnd,
        Punct::PipePipeEq => A::LogicalOr,
        Punct::QuestionQuestionEq => A::LogicalCoalesce,
        _ => return None,
    })
}

impl<'s> Parser<'s> {
    // ---- entry points ------------------------------------------------------

    /// [`Self::parse_assign`], finalized: the expression cannot become a
    /// destructuring pattern in any enclosing context, so a pending
    /// CoverInitializedName error (`{a = 1}` outside a target position) is
    /// raised HERE. Every full-expression position — statement expressions,
    /// conditions, arguments, computed keys, initializers — goes through one of
    /// these two wrappers; the plain variants are only for positions an
    /// enclosing conversion can still reinterpret.
    pub(crate) fn parse_assign_full(&mut self) -> PResult<Expr> {
        let outer = self.cover.pattern_only.take();
        let e = self.parse_assign()?;
        if let Some(err) = self.cover.pattern_only.take() {
            self.cover.pattern_only = outer;
            return Err(err);
        }
        self.cover.pattern_only = outer;
        Ok(e)
    }

    /// [`Self::parse_expr`], finalized — see [`Self::parse_assign_full`].
    pub(crate) fn parse_expr_full(&mut self) -> PResult<Expr> {
        let outer = self.cover.pattern_only.take();
        let e = self.parse_expr()?;
        if let Some(err) = self.cover.pattern_only.take() {
            self.cover.pattern_only = outer;
            return Err(err);
        }
        self.cover.pattern_only = outer;
        Ok(e)
    }

    /// `Expression` — the comma operator.
    pub(crate) fn parse_expr(&mut self) -> PResult<Expr> {
        let first = self.parse_assign()?;
        if !self.at(Punct::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(Punct::Comma, true)? {
            items.push(self.parse_assign()?);
        }
        Ok(Expr::Seq(items))
    }

    /// `AssignmentExpression`.
    pub(crate) fn parse_assign(&mut self) -> PResult<Expr> {
        // `yield` is an AssignmentExpression of its own, not an operand.
        if self.ctx.yield_ && self.at_kw(Keyword::Yield) {
            return self.parse_yield();
        }
        if self.at_kw(Keyword::Async) {
            if let Some(e) = self.try_async_prefixed()? {
                return Ok(e);
            }
        }

        let start = self.cur().span.start;
        // Stash the enclosing region's pattern-only error so an inner one
        // cannot be mistaken for it, and vice versa.
        let outer_po = self.cover.pattern_only.take();
        let lhs = self.parse_conditional()?;

        // `x => …`, detected AFTER the LHS parse: a lone identifier followed by
        // `=>` on the same line. Probing BEFORE the parse needed a save/restore
        // — a Token clone, String and all, for every identifier-initial
        // expression, which is the hottest path in the parser. The precedence
        // tower stops at `=>` (it is no operator), so the identifier arrives
        // here intact and the conversion is free.
        if self.at(Punct::Arrow) && !self.cur().newline_before {
            if let Expr::Ident(name) = &lhs {
                let name = name.clone();
                if self.ctx.strict && (&*name == "eval" || &*name == "arguments") {
                    return Err(SyntaxError::new(
                        format!("SyntaxError: '{name}' cannot be a parameter name in strict mode"),
                        start,
                    ));
                }
                self.cover.pattern_only = outer_po;
                self.bump_before_operand()?; // `=>`
                let params = Params { items: vec![Pattern::Ident(name)], simple: true };
                return self.finish_arrow(params, false, start);
            }
        }

        let Some(op) = self.cur().kind.as_punct().and_then(assign_op) else {
            // No `=` follows here — but the expression may STILL become a
            // pattern in an enclosing context (`({style = ''}) => …` reaches
            // this point for the object while the paren cover is undecided), so
            // the recorded error PROPAGATES rather than raising. It is raised
            // at expression finalization (`parse_assign_full`) or by the paren
            // cover's expression resolution — the points where no conversion
            // remains possible. Earliest error wins, and the outer one was
            // recorded first.
            self.cover.pattern_only = outer_po.or(self.cover.pattern_only.take());
            return Ok(lhs);
        };
        // It IS an assignment, so the target reading wins for the LHS and its
        // recorded pattern-only error was never an error.
        self.cover.pattern_only = outer_po;
        self.bump_before_operand()?;
        // Only `=` may target a destructuring pattern; `+=` and friends require
        // a simple target, which `expr_to_target` enforces by rejecting the
        // array/object forms for them.
        let target = self.expr_to_target(lhs, op == AssignOp::Assign, start)?;
        // The Annex-B allowance that lets a CallExpression be an assignment
        // target in sloppy code (`f() = 1` ⇒ runtime ReferenceError) predates
        // logical assignment, and 13.15.1 grants it no carve-out: `f() &&= 1`
        // is an early SyntaxError in both modes. `=` and `+=` keep the legacy.
        if matches!(target, Target::Call(_))
            && matches!(
                op,
                AssignOp::LogicalAnd | AssignOp::LogicalOr | AssignOp::LogicalCoalesce
            )
        {
            return Err(SyntaxError::new("SyntaxError: invalid assignment target", start));
        }
        let value = self.parse_assign()?;
        Ok(Expr::Assign { op, target, value: Box::new(value), covered: false })
    }

    fn parse_yield(&mut self) -> PResult<Expr> {
        self.bump_before_operand()?; // `yield`
        // `yield` and `yield *` are restricted productions: a LineTerminator
        // ends the expression, so `yield \n x` yields undefined.
        if self.cur().newline_before {
            return Ok(Expr::Yield { arg: None, delegate: false });
        }
        let delegate = self.eat(Punct::Star, true)?;
        // With no operand and no `*`, `yield` alone is legal wherever an
        // expression ends.
        if !delegate && self.at_expr_end() {
            return Ok(Expr::Yield { arg: None, delegate: false });
        }
        let arg = self.parse_assign()?;
        Ok(Expr::Yield { arg: Some(Box::new(arg)), delegate })
    }

    /// Does the current token terminate an expression? Used by the operand-less
    /// forms (`yield`, `return`, `break`).
    fn at_expr_end(&self) -> bool {
        self.at_eof()
            || self.at(Punct::Semi)
            || self.at(Punct::RBrace)
            || self.at(Punct::RParen)
            || self.at(Punct::RBracket)
            || self.at(Punct::Comma)
            || self.at(Punct::Colon)
    }


    /// Everything `async` can begin. Returns `None` if it is just an identifier.
    fn try_async_prefixed(&mut self) -> PResult<Option<Expr>> {
        let save = self.save();
        let start = self.cur().span.start;
        self.bump_after_operand()?; // `async`
        // A LineTerminator after `async` ends it: `async \n () => {}` is the
        // identifier `async` followed by a separate expression.
        if self.cur().newline_before {
            self.restore(save);
            return Ok(None);
        }
        // `async function` is NOT handled here: a function expression is a
        // PRIMARY, and returning it from assignment level bypassed the
        // member/call tail — `async function () {…}()` lost its call and died
        // on the orphaned parens. parse_primary owns it now.
        if self.at_kw(Keyword::Function) {
            self.restore(save);
            return Ok(None);
        }
        // `async x => …` — same permissive read as `try_ident_arrow`: the name
        // is only a binding once the `=>` is confirmed.
        if self.is_binding_ident() {
            let tok = self.bump_after_operand()?;
            let TokenKind::Ident { name, .. } = tok.kind else { unreachable!() };
            if self.at(Punct::Arrow) && !self.cur().newline_before {
                if self.ctx.strict && (name == "eval" || name == "arguments") {
                    return Err(SyntaxError::new(
                        format!("SyntaxError: '{name}' cannot be a parameter name in strict mode"),
                        start,
                    ));
                }
                // `AsyncArrowBindingIdentifier[Yield] : BindingIdentifier
                // [?Yield, +Await]` — `await` is reserved there whatever the
                // enclosing context, and by STRING VALUE, so the escaped
                // spelling `async await => 1` is the same error.
                if name == "await" {
                    return Err(SyntaxError::new(
                        "SyntaxError: 'await' is not a valid name in async function parameters",
                        start,
                    ));
                }
                self.bump_before_operand()?;
                let params =
                    Params { items: vec![Pattern::Ident(name.into_boxed_str())], simple: true };
                return Ok(Some(self.finish_arrow(params, true, start)?));
            }
            self.restore(save);
            return Ok(None);
        }
        // `async ( … )` — either an async arrow or a CALL to something named
        // `async`. The cover machinery decides, from what follows the `)`.
        if self.at(Punct::LParen) {
            if let Some(e) = self.parse_paren_or_arrow(true, start)? {
                return Ok(Some(e));
            }
            self.restore(save);
            return Ok(None);
        }
        self.restore(save);
        Ok(None)
    }

    // ---- conditional / binary ---------------------------------------------

    fn parse_conditional(&mut self) -> PResult<Expr> {
        let test = self.parse_nullish(0)?;
        if !self.at(Punct::Question) {
            return Ok(test);
        }
        self.bump_before_operand()?;
        // The branches are AssignmentExpressions with [In] restored — `for (a ?
        // b in c : d;;)` parses the `in`.
        let saved_in = self.ctx.in_;
        self.ctx.in_ = true;
        let cons = self.parse_assign()?;
        self.ctx.in_ = saved_in;
        self.expect(Punct::Colon, true)?;
        let alt = self.parse_assign()?;
        Ok(Expr::Cond { test: Box::new(test), cons: Box::new(cons), alt: Box::new(alt) })
    }

    /// ShortCircuitExpression: either a `??` chain or a `||`/`&&` chain — the
    /// grammar makes them ALTERNATIVES, so mixing them at one level without
    /// parentheses is a SyntaxError.
    ///
    /// The flag returned by the chain parsers says "a bare `||`/`&&` was
    /// consumed AT THIS LEVEL". Parenthesized logicals don't set it — the paren
    /// contents are parsed levels deeper — which is exactly the distinction the
    /// rule needs: `(a || b) ?? c` is legal, `a || b ?? c` is not. Testing the
    /// NEXT token (the previous implementation) got the parenthesized case
    /// wrong, because by then the parens were invisible.
    fn parse_nullish(&mut self, _min: u8) -> PResult<Expr> {
        let (mut left, saw_logical) = self.parse_or_chain()?;
        if !self.at(Punct::QuestionQuestion) {
            return Ok(left);
        }
        if saw_logical {
            return Err(self.err_here(
                "SyntaxError: '??' cannot be mixed with '||' or '&&' without parentheses",
            ));
        }
        while self.at(Punct::QuestionQuestion) {
            self.bump_before_operand()?;
            let (right, saw) = self.parse_or_chain()?;
            if saw {
                return Err(self.err_here(
                    "SyntaxError: '??' cannot be mixed with '||' or '&&' without parentheses",
                ));
            }
            left = Expr::Logical {
                op: LogicalOp::Coalesce,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_or_chain(&mut self) -> PResult<(Expr, bool)> {
        let (mut left, mut saw) = self.parse_and_chain()?;
        while self.at(Punct::PipePipe) {
            saw = true;
            self.bump_before_operand()?;
            let (right, _) = self.parse_and_chain()?;
            left =
                Expr::Logical { op: LogicalOp::Or, left: Box::new(left), right: Box::new(right) };
        }
        Ok((left, saw))
    }

    fn parse_and_chain(&mut self) -> PResult<(Expr, bool)> {
        let mut left = self.parse_binary(4)?;
        let mut saw = false;
        while self.at(Punct::AmpAmp) {
            saw = true;
            self.bump_before_operand()?;
            let right = self.parse_binary(4)?;
            left =
                Expr::Logical { op: LogicalOp::And, left: Box::new(left), right: Box::new(right) };
        }
        Ok((left, saw))
    }

    /// Precedence climbing. `min` is the lowest binding power this call accepts.
    fn parse_binary(&mut self, min: u8) -> PResult<Expr> {
        // `[+In] PrivateIdentifier in ShiftExpression` is a production of
        // RelationalExpression, NOT a PrimaryExpression: the private name is
        // legal only where a RelationalExpression is admissible (`min <= 9`,
        // relational's own precedence) and only with `[+In]`. Parsing it as a
        // primary let `typeof #f in o`, `-#f in o`, `1 + #f in o` and
        // `for (#f in o;;)` through.
        let mut left = if min <= 9 && self.ctx.in_ {
            match self.try_parse_private_in()? {
                Some(e) => e,
                None => self.parse_unary()?,
            }
        } else {
            self.parse_unary()?
        };
        loop {
            // `in` and `instanceof` are keywords at binary precedence, and `in`
            // is suppressed inside a `for` head.
            let (prec, op) = if self.at_kw(Keyword::In) && self.ctx.in_ {
                (9, BinaryOp::In)
            } else if self.at_kw(Keyword::Instanceof) {
                (9, BinaryOp::Instanceof)
            } else {
                match self.cur().kind.as_punct().and_then(|p| binary_prec(p, &self.ctx)) {
                    Some(v) => v,
                    None => break,
                }
            };
            if prec < min {
                break;
            }
            self.bump_before_operand()?;
            // `**` is RIGHT-associative, so its right operand accepts the same
            // precedence rather than one higher.
            let next_min = if op == BinaryOp::Exp { prec } else { prec + 1 };
            let right = self.parse_binary(next_min)?;
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
        }
        Ok(left)
    }

    /// `RelationalExpression : [+In] PrivateIdentifier in ShiftExpression`.
    ///
    /// Returns `None` when the current token is not a private name, so the
    /// caller falls through to the ordinary operand. The `in` is mandatory —
    /// a private name in any other expression position is an early error.
    fn try_parse_private_in(&mut self) -> PResult<Option<Expr>> {
        if !matches!(self.cur().kind, TokenKind::Ident { private: true, .. }) {
            return Ok(None);
        }
        let start = self.cur().span.start;
        let tok = self.bump_after_operand()?;
        let TokenKind::Ident { name, .. } = tok.kind else { unreachable!() };
        if !self.at_kw(Keyword::In) {
            return Err(SyntaxError::new("SyntaxError: unexpected private name", start));
        }
        self.bump_before_operand()?;
        // A ShiftExpression: `#f in #f in o` (RelationalExpression) and
        // `#f in () => {}` (AssignmentExpression) are both out of grammar. The
        // first falls out of `min = 10`; the second needs an explicit test
        // because parentheses leave no trace in the AST — comparing the arrow's
        // own span against the operand start distinguishes the illegal
        // `#f in () => {}` from the legal `#f in (() => {})`.
        let rhs_start = self.cur().span.start;
        let object = self.parse_binary(10)?;
        if let Expr::Arrow(a) = &object {
            if a.span.start == rhs_start {
                return Err(SyntaxError::new(
                    "SyntaxError: the right operand of a private-name 'in' must be a ShiftExpression",
                    rhs_start,
                ));
            }
        }
        Ok(Some(Expr::PrivateIn { name: name.into_boxed_str(), object: Box::new(object) }))
    }

    // ---- unary / update ----------------------------------------------------

    fn parse_unary(&mut self) -> PResult<Expr> {
        use Punct as P;
        let op = match self.cur().kind.as_punct() {
            Some(P::Minus) => Some(UnaryOp::Minus),
            Some(P::Plus) => Some(UnaryOp::Plus),
            Some(P::Bang) => Some(UnaryOp::Not),
            Some(P::Tilde) => Some(UnaryOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.bump_before_operand()?;
            let arg = self.parse_unary()?;
            // `-a ** b` is a SyntaxError: the operand of `**` may not be an
            // unparenthesized unary expression.
            if self.at(Punct::StarStar) {
                return Err(self.err_here(
                    "SyntaxError: unary operator before '**' requires parentheses",
                ));
            }
            return Ok(Expr::Unary { op, arg: Box::new(arg) });
        }
        for (kw, op) in [
            (Keyword::Typeof, UnaryOp::Typeof),
            (Keyword::Void, UnaryOp::Void),
            (Keyword::Delete, UnaryOp::Delete),
        ] {
            if self.at_kw(kw) {
                let pos = self.cur().span.start;
                self.bump_before_operand()?;
                let arg = self.parse_unary()?;
                if self.at(Punct::StarStar) {
                    return Err(self.err_here(
                        "SyntaxError: unary operator before '**' requires parentheses",
                    ));
                }
                if op == UnaryOp::Delete && self.ctx.strict {
                    // Strict mode: `delete x` on an unqualified identifier is an
                    // early error. A member expression is fine.
                    if matches!(arg, Expr::Ident(_)) {
                        return Err(SyntaxError::new(
                            "SyntaxError: 'delete' of an unqualified identifier in strict mode",
                            pos,
                        ));
                    }
                    // `delete this.#x` is always a SyntaxError.
                    if let Expr::Member(m) = &arg {
                        if matches!(m.prop, MemberProp::Private(_)) {
                            return Err(SyntaxError::new(
                                "SyntaxError: private fields cannot be deleted",
                                pos,
                            ));
                        }
                    }
                }
                return Ok(Expr::Unary { op, arg: Box::new(arg) });
            }
        }
        if self.ctx.await_ && self.at_kw(Keyword::Await) {
            let pos = self.cur().span.start;
            self.bump_before_operand()?;
            let arg = self.parse_unary()?;
            // `ExponentiationExpression : UpdateExpression ** …` — an
            // AwaitExpression is a UnaryExpression, so it is no more legal as an
            // unparenthesized `**` base than `-a ** b` is.
            if self.at(Punct::StarStar) {
                return Err(self.err_here(
                    "SyntaxError: unary operator before '**' requires parentheses",
                ));
            }
            // Legal here, but illegal in the arrow parameters this region might
            // still turn out to be — so record it rather than deciding now.
            self.cover
                .yield_await
                .push(SyntaxError::new("SyntaxError: 'await' in formal parameters", pos));
            return Ok(Expr::Await(Box::new(arg)));
        }
        // Prefix `++`/`--`.
        for (p, op) in [(Punct::PlusPlus, UpdateOp::Inc), (Punct::MinusMinus, UpdateOp::Dec)] {
            if self.at(p) {
                let pos = self.cur().span.start;
                self.bump_before_operand()?;
                let arg = self.parse_unary()?;
                let target = self.expr_to_target(arg, false, pos)?;
                return Ok(Expr::Update { op, prefix: true, target });
            }
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let start = self.cur().span.start;
        let e = self.parse_lhs()?;
        for (p, op) in [(Punct::PlusPlus, UpdateOp::Inc), (Punct::MinusMinus, UpdateOp::Dec)] {
            // Restricted production: a LineTerminator before `++` means ASI, not
            // a postfix operator.
            if self.at(p) && !self.cur().newline_before {
                self.bump_after_operand()?;
                let target = self.expr_to_target(e, false, start)?;
                return Ok(Expr::Update { op, prefix: false, target });
            }
        }
        Ok(e)
    }

    // ---- member / call chains ---------------------------------------------

    /// `LeftHandSideExpression`: member accesses, calls, `new`, and optional
    /// chains.
    pub(crate) fn parse_lhs(&mut self) -> PResult<Expr> {
        let lhs_start = self.cur().span.start;
        let mut e = if self.at_kw(Keyword::New) {
            self.parse_new()?
        } else {
            self.parse_primary()?
        };
        // `AssignmentExpression : ArrowFunction` — an ArrowFunction is NOT a
        // LeftHandSideExpression, so no `.`/`[`/`(`/template suffix may follow
        // one. Parentheses leave no trace in the AST, so the arrow's own span
        // start is what separates the legal `(() => {})['x']` (arrow starts
        // after the `(`) from the illegal `() => {} ['x']`.
        //
        // Accepting the suffix silently mis-parsed a class body: the two fields
        //     ["p"] = () => {}
        //     ["q"] = async () => {};
        // became `["p"] = (() => {})["q"] = async () => {}` — one field named
        // "p" holding the WRONG function and no "q" at all
        // (staging/sm/Function/function-name-computed-01.js).
        if matches!(&e, Expr::Arrow(a) if a.span.start == lhs_start) {
            return Ok(e);
        }
        // A CoverInitializedName (`{a = 1}`) and a duplicate `__proto__` are legal
        // only while the literal holding them may still be REFINED into a
        // destructuring pattern; both are recorded as deferred pattern-only errors
        // and discharged by the `=` that refines the literal. A `.`/`[`/`(`/
        // template suffix ends that possibility — the literal becomes the base of
        // a MemberExpression or CallExpression, which no refinement descends into
        // — so the deferred error can never be discharged and fires here.
        //
        // Without it `[{a = 0}.x] = []` was accepted: the enclosing array literal
        // IS refined, and its `=` dropped the inner object's error along with its
        // own (staging/sm/destructuring/bug1396261.js).
        if matches!(&e, Expr::Object(..) | Expr::Array(..))
            && (self.at(Punct::Dot)
                || self.at(Punct::QuestionDot)
                || self.at(Punct::LBracket)
                || self.at(Punct::LParen)
                || matches!(self.cur().kind, TokenKind::Template { .. }))
        {
            if let Some(err) = self.cover.pattern_only.take() {
                return Err(err);
            }
        }
        let mut saw_optional = false;
        loop {
            if self.at(Punct::Dot) {
                self.bump_after_operand()?;
                let prop = self.member_prop()?;
                e = Expr::Member(Box::new(Member { object: e, prop, optional: false }));
            } else if self.at(Punct::QuestionDot) {
                // `super?.x` / `super?.[x]` are early SyntaxErrors: `super` is
                // not a valid optional-chain base.
                if matches!(&e, Expr::Super) {
                    return Err(
                        self.err_here("SyntaxError: 'super' cannot start an optional chain")
                    );
                }
                saw_optional = true;
                self.bump_after_operand()?;
                if self.at(Punct::LParen) {
                    let args = self.parse_args()?;
                    e = Expr::Call(Box::new(CallExpr { callee: e, args, optional: true }));
                } else if self.at(Punct::LBracket) {
                    self.bump_before_operand()?;
                    let saved_in = self.ctx.in_;
                    self.ctx.in_ = true;
                    let idx = self.parse_expr_full()?;
                    self.ctx.in_ = saved_in;
                    self.expect(Punct::RBracket, false)?;
                    e = Expr::Member(Box::new(Member {
                        object: e,
                        prop: MemberProp::Computed(idx),
                        optional: true,
                    }));
                } else {
                    let prop = self.member_prop()?;
                    e = Expr::Member(Box::new(Member { object: e, prop, optional: true }));
                }
            } else if self.at(Punct::LBracket) {
                self.bump_before_operand()?;
                let saved_in = self.ctx.in_;
                self.ctx.in_ = true;
                let idx = self.parse_expr_full()?;
                self.ctx.in_ = saved_in;
                self.expect(Punct::RBracket, false)?;
                e = Expr::Member(Box::new(Member {
                    object: e,
                    prop: MemberProp::Computed(idx),
                    optional: false,
                }));
            } else if self.at(Punct::LParen) {
                let args = self.parse_args()?;
                e = Expr::Call(Box::new(CallExpr { callee: e, args, optional: false }));
            } else if matches!(self.cur().kind, TokenKind::Template { .. }) {
                // A tagged template. `` a?.b`x` `` is a SyntaxError: an optional
                // chain may not be tagged.
                if saw_optional {
                    return Err(self
                        .err_here("SyntaxError: tagged template in an optional chain"));
                }
                let quasi = self.parse_template()?;
                e = Expr::TaggedTemplate { tag: Box::new(e), quasi: Box::new(quasi) };
            } else {
                break;
            }
        }
        // Wrap the WHOLE chain once, so short-circuiting covers everything to
        // its right — `a?.b.c` skips `.c` too.
        if saw_optional {
            e = Expr::Chain(Box::new(e));
        }
        Ok(e)
    }

    /// `member_prop` / `parse_args` for the DECORATOR grammar, which lives in
    /// `parse/funcs.rs` (with the rest of the class parsing) and so cannot reach
    /// this module's private helpers.
    pub(crate) fn member_prop_public(&mut self) -> PResult<MemberProp> {
        self.member_prop()
    }

    pub(crate) fn parse_args_public(&mut self) -> PResult<Vec<Arg>> {
        self.parse_args()
    }

    fn member_prop(&mut self) -> PResult<MemberProp> {
        if let TokenKind::Ident { private: true, .. } = &self.cur().kind {
            let tok = self.bump_after_operand()?;
            let TokenKind::Ident { name, .. } = tok.kind else { unreachable!() };
            return Ok(MemberProp::Private(name.into_boxed_str()));
        }
        Ok(MemberProp::Ident(self.ident_name()?))
    }

    fn parse_new(&mut self) -> PResult<Expr> {
        let pos = self.cur().span.start;
        // `new` is followed by a MemberExpression — an OPERAND — so `new /re/`
        // is a regex literal (a runtime TypeError, not a syntax error).
        self.bump_before_operand()?; // `new`
        // `new.target`
        if self.at(Punct::Dot) {
            self.bump_after_operand()?;
            // `target` is a bolded terminal of `NewTarget : new . target`, so it
            // must appear exactly as written — `new.target` is a
            // SyntaxError. `ident_name()` returns the COOKED name, which cannot
            // tell the two apart, so the token is inspected directly.
            let unescaped_target = matches!(&self.cur().kind,
                TokenKind::Ident { name, had_escape: false, private: false, .. }
                    if &**name == "target");
            let name = self.ident_name()?;
            if &*name != "target" || !unescaped_target {
                return Err(SyntaxError::new("SyntaxError: expected 'new.target'", pos));
            }
            if !self.ctx.new_target {
                return Err(SyntaxError::new(
                    "SyntaxError: 'new.target' is only allowed inside a function",
                    pos,
                ));
            }
            return Ok(Expr::NewTarget);
        }
        // Does the callee start with the `import` keyword ITSELF? A
        // parenthesized expression leaves no trace in the AST, so testing the
        // parsed callee alone also rejected the legal `new (import(''))` —
        // there the ImportCall is a PrimaryExpression, hence a MemberExpression,
        // and the failure is a runtime TypeError.
        let bare_import = self.at_kw(Keyword::Import);
        // The callee is a MemberExpression — it takes member accesses but NOT
        // calls, so `new a.b()` applies `new` to `a.b`, not to `a.b()`.
        let mut callee =
            if self.at_kw(Keyword::New) { self.parse_new()? } else { self.parse_primary()? };
        // `new import(…)` / `new import.defer(…)`: an ImportCall is a
        // CallExpression, and `new` takes a MemberExpression, so it can never be
        // the callee. Checked BEFORE the member loop so `new import('m').p` is
        // caught too. (`new import.meta` stays legal — that is a MetaProperty,
        // a runtime TypeError rather than a SyntaxError.)
        if bare_import && matches!(callee, Expr::ImportCall { .. }) {
            return Err(SyntaxError::new(
                "SyntaxError: 'new' cannot be applied to an import call",
                pos,
            ));
        }
        loop {
            if self.at(Punct::Dot) {
                self.bump_after_operand()?;
                let prop = self.member_prop()?;
                callee = Expr::Member(Box::new(Member { object: callee, prop, optional: false }));
            } else if self.at(Punct::LBracket) {
                self.bump_before_operand()?;
                let idx = self.parse_expr()?;
                self.expect(Punct::RBracket, false)?;
                callee = Expr::Member(Box::new(Member {
                    object: callee,
                    prop: MemberProp::Computed(idx),
                    optional: false,
                }));
            } else if matches!(self.cur().kind, TokenKind::Template { .. }) {
                // `MemberExpression : MemberExpression TemplateLiteral` — a
                // tagged template is itself a MemberExpression, so ``new tag`t` ``
                // constructs the TAG'S RESULT, not the tag.
                let quasi = self.parse_template()?;
                callee = Expr::TaggedTemplate { tag: Box::new(callee), quasi: Box::new(quasi) };
            } else {
                break;
            }
        }
        // `new a?.b()` is a SyntaxError — an optional chain is not a valid
        // constructor.
        if self.at(Punct::QuestionDot) {
            return Err(self.err_here("SyntaxError: 'new' cannot be applied to an optional chain"));
        }
        let args = if self.at(Punct::LParen) { self.parse_args()? } else { Vec::new() };
        Ok(Expr::New { callee: Box::new(callee), args })
    }

    /// An ImportCall's argument list, from the `(` on.
    ///
    /// ```text
    /// ImportCall : import ( AssignmentExpression ,opt )
    ///            | import ( AssignmentExpression , AssignmentExpression ,opt )
    /// ```
    ///
    /// and the same two shapes after `import.defer` / `import.source`. This is
    /// NOT `Arguments`: the specifier is required and `...spread` is not allowed.
    /// Requiring the `(` here is also what keeps `typeof import.source`,
    /// `import.source.UNKNOWN` and `import.defer()` erroring as they must.
    fn parse_import_call(&mut self, phase: ImportPhase) -> PResult<Expr> {
        self.expect(Punct::LParen, true)?;
        // Both arguments are `AssignmentExpression[+In]` — see `parse_args`;
        // `for (p = import('./x.js', 'k' in {});;)` is legal.
        let saved_in = self.ctx.in_;
        self.ctx.in_ = true;
        let spec = self.parse_assign_full()?;
        let options = if self.eat(Punct::Comma, true)? && !self.at(Punct::RParen) {
            let o = self.parse_assign_full()?;
            let _ = self.eat(Punct::Comma, true)?;
            Some(Box::new(o))
        } else {
            None
        };
        self.ctx.in_ = saved_in;
        self.expect(Punct::RParen, false)?;
        Ok(Expr::ImportCall { spec: Box::new(spec), options, phase })
    }

    fn parse_args(&mut self) -> PResult<Vec<Arg>> {
        self.expect(Punct::LParen, true)?;
        // The `[In]` restriction applies only to the TOP level of a `for`
        // head; every bracketed subcontext restores it. `for (var c = f(1 in
        // {});;)` is legal.
        let saved_in = self.ctx.in_;
        self.ctx.in_ = true;
        let mut out = Vec::new();
        while !self.at(Punct::RParen) {
            if self.at(Punct::DotDotDot) {
                self.bump_before_operand()?;
                out.push(Arg::Spread(self.parse_assign_full()?));
            } else {
                out.push(Arg::Expr(self.parse_assign_full()?));
            }
            if !self.eat(Punct::Comma, true)? {
                break;
            }
        }
        self.ctx.in_ = saved_in;
        self.expect(Punct::RParen, false)?;
        Ok(out)
    }

    // ---- primary -----------------------------------------------------------

    fn parse_primary(&mut self) -> PResult<Expr> {
        // Clear the flag on entry so it describes only the primary about to be
        // parsed. Without this it leaks forward: `(a + b); x = 1` would mark
        // `x` as parenthesized because nothing consumed the flag in between.
        // The `(` branch re-sets it after its contents are parsed.
        self.parenthesized = false;
        let start = self.cur().span.start;
        // `ClassExpression : DecoratorList_opt class …` — a decorated class
        // EXPRESSION (`x = @dec class {}`, `f(@dec class C {})`). Nothing else in
        // PrimaryExpression starts with `@`, so this is unambiguous.
        if self.at(Punct::At) {
            let decorators = self.parse_decorators()?;
            if !self.at_kw(Keyword::Class) {
                return Err(self.err_here("SyntaxError: decorators must precede a class"));
            }
            self.bump_after_operand()?;
            let c = self.parse_class_rest_dec(start, decorators)?;
            return Ok(Expr::Class(Box::new(c)));
        }
        // Dispatch on a BORROW of the token; the taking arms consume it through
        // `bump`, which returns the old token, so payloads MOVE. The previous
        // shape cloned the whole TokenKind — String contents included — for
        // every primary, which was the hottest allocation in the parser.
        match &self.cur().kind {
            TokenKind::Num(n) => {
                let n = *n;
                // Legacy octal spellings are a SyntaxError in strict code, and
                // the SPELLING is only available here — the AST carries the
                // value alone.
                if self.ctx.strict
                    && matches!(n.kind, NumKind::LegacyOctal | NumKind::NonOctalDecimal)
                {
                    return Err(SyntaxError::new(
                        "SyntaxError: legacy octal literals are not allowed in strict mode",
                        start,
                    ));
                }
                self.bump_after_operand()?;
                Ok(Expr::Num(n.value))
            }
            TokenKind::BigInt(_) => {
                let tok = self.bump_after_operand()?;
                let TokenKind::BigInt(d) = tok.kind else { unreachable!() };
                Ok(Expr::BigInt(d.into_boxed_str()))
            }
            TokenKind::Str(_) => {
                self.check_legacy_escape()?;
                let tok = self.bump_after_operand()?;
                let TokenKind::Str(v) = tok.kind else { unreachable!() };
                Ok(Expr::Str(v))
            }
            TokenKind::Regex { .. } => {
                let start = self.cur().span.start;
                let tok = self.bump_after_operand()?;
                let TokenKind::Regex { pattern, flags } = tok.kind else { unreachable!() };
                // EARLY ERROR (12.9.5): the FlagText of a RegularExpressionLiteral
                // may hold no code point outside `dgimsuvy` and none of them
                // twice, and `u`/`v` select mutually exclusive grammars. This is
                // a SYNTAX error for the whole program — `/./G` must never run —
                // whereas the RegExp CONSTRUCTOR raises the same conditions as a
                // runtime SyntaxError from its own validation (`build_regexp`).
                let mut seen = String::with_capacity(flags.len());
                for c in flags.chars() {
                    if !"dgimsuvy".contains(c) {
                        return Err(SyntaxError::new(
                            format!("SyntaxError: invalid regular expression flag '{c}'"),
                            start,
                        ));
                    }
                    if seen.contains(c) {
                        return Err(SyntaxError::new(
                            format!("SyntaxError: duplicate regular expression flag '{c}'"),
                            start,
                        ));
                    }
                    seen.push(c);
                }
                if seen.contains('u') && seen.contains('v') {
                    return Err(SyntaxError::new(
                        "SyntaxError: the regular expression flags 'u' and 'v' are mutually exclusive",
                        start,
                    ));
                }
                // Canonical flag order, not source order: `/re/yu` reports its
                // flags as "uy" (the `flags` getter enumerates in a fixed
                // order, and `toString` goes through it), and the engine
                // stores what it reports.
                let canon: String = "dgimsuvy".chars().filter(|c| seen.contains(*c)).collect();
                Ok(Expr::Regex { pattern, flags: canon.into_boxed_str() })
            }
            TokenKind::Template { .. } => {
                let pos = self.cur().span.start;
                let t = self.parse_template()?;
                // An invalid escape makes a chunk's COOKED value undefined. That
                // is legal only in a TAGGED template, where the tag can still
                // read `.raw`; an untagged one needs the cooked value, so it is
                // an early error. The lexer already records this as
                // `cooked: None` — nothing was checking it on this path.
                if t.quasis.iter().any(|q| q.cooked.is_none()) {
                    return Err(SyntaxError::new(
                        "SyntaxError: invalid escape in an untagged template literal",
                        pos,
                    ));
                }
                Ok(Expr::Template(Box::new(t)))
            }
            TokenKind::Punct(Punct::LParen) => match self.parse_paren_or_arrow(false, start)? {
                Some(e) => Ok(e),
                None => Err(self.err_here("SyntaxError: unexpected token")),
            },
            TokenKind::Punct(Punct::LBracket) => self.parse_array_literal(),
            TokenKind::Punct(Punct::LBrace) => self.parse_object_literal(),
            TokenKind::Ident { private: true, .. } => {
                // A private name is never a PrimaryExpression — the only
                // expression production that admits one is
                // `RelationalExpression : [+In] PrivateIdentifier in
                // ShiftExpression`, handled in `parse_binary`. Reaching here
                // means the private name sits somewhere that production cannot
                // cover (`!#x`, `(#x) in o`, `typeof #x in o`, …).
                Err(SyntaxError::new("SyntaxError: unexpected private name", start))
            }
            TokenKind::Ident { kw, had_escape, .. } => {
                // Escaped spellings are identifiers, never keywords, so they
                // skip the keyword dispatch entirely.
                let kw = if *had_escape { Keyword::None } else { *kw };
                match kw {
                    Keyword::This => {
                        self.bump_after_operand()?;
                        Ok(Expr::This)
                    }
                    Keyword::Null => {
                        self.bump_after_operand()?;
                        Ok(Expr::Null)
                    }
                    Keyword::True => {
                        self.bump_after_operand()?;
                        Ok(Expr::Bool(true))
                    }
                    Keyword::False => {
                        self.bump_after_operand()?;
                        Ok(Expr::Bool(false))
                    }
                    Keyword::Function => {
                        self.bump_after_operand()?;
                        let f = self.parse_function_rest(false, true, start)?;
                        Ok(Expr::Function(Box::new(f)))
                    }
                    Keyword::Async => {
                        // `async function` as a primary, so the member/call
                        // tail applies (`async function () {…}()` is an IIFE).
                        // Anything else async-shaped was handled by
                        // parse_assign; a bare `async` here is an identifier.
                        let save = self.save();
                        self.bump_after_operand()?;
                        if self.at_kw(Keyword::Function) && !self.cur().newline_before {
                            self.bump_after_operand()?;
                            let f = self.parse_function_rest(true, true, start)?;
                            return Ok(Expr::Function(Box::new(f)));
                        }
                        self.restore(save);
                        let (name, _) = self.binding_ident_or_reference()?;
                        Ok(Expr::Ident(name))
                    }
                    Keyword::Class => {
                        self.bump_after_operand()?;
                        let c = self.parse_class_rest(start)?;
                        Ok(Expr::Class(Box::new(c)))
                    }
                    Keyword::Super => {
                        self.bump_after_operand()?;
                        if self.at(Punct::LParen) {
                            if !self.ctx.super_call {
                                return Err(SyntaxError::new(
                                    "SyntaxError: 'super()' is only valid in a derived constructor",
                                    start,
                                ));
                            }
                        } else if !self.ctx.super_prop {
                            return Err(SyntaxError::new(
                                "SyntaxError: 'super' property access is only valid in a method",
                                start,
                            ));
                        }
                        Ok(Expr::Super)
                    }
                    Keyword::Import => {
                        self.bump_after_operand()?;
                        if self.at(Punct::Dot) {
                            self.bump_after_operand()?;
                            // `import .` is followed by `meta`, `defer` or
                            // `source`. Matched by SPELLING: a fixed token of a
                            // production may not be written with an escape, so
                            // `import.meta` is an error, not `import.meta`.
                            // `None` = the meta-property; `Some(p)` = a phased
                            // ImportCall, which still requires its `(`.
                            let phase: Option<ImportPhase> = match &self.cur().kind {
                                TokenKind::Ident { name, had_escape: false, private: false, .. } => {
                                    match name.as_str() {
                                        "meta" => None,
                                        "defer" => Some(ImportPhase::Defer),
                                        "source" => Some(ImportPhase::Source),
                                        _ => {
                                            return Err(SyntaxError::new(
                                                "SyntaxError: expected 'import.meta'",
                                                start,
                                            ))
                                        }
                                    }
                                }
                                _ => {
                                    return Err(SyntaxError::new(
                                        "SyntaxError: expected 'import.meta'",
                                        start,
                                    ))
                                }
                            };
                            self.bump_after_operand()?;
                            let Some(phase) = phase else { return Ok(Expr::ImportMeta) };
                            return self.parse_import_call(phase);
                        }
                        self.parse_import_call(ImportPhase::Evaluation)
                    }
                    _ => {
                        let (name, _) = self.binding_ident_or_reference()?;
                        Ok(Expr::Ident(name))
                    }
                }
            }
            TokenKind::Eof => Err(self.err_here("SyntaxError: unexpected end of input")),
            TokenKind::Punct(p) => {
                let p = *p;
                Err(self.err_here(format!("SyntaxError: unexpected token {p:?}")))
            }
        }
    }

    /// `""` (LegacyOctalEscapeSequence) and `"\8"` (NonOctalDecimalEscape)
    /// are Annex B, and a Syntax Error in strict code. The LEXER records the
    /// spelling because it cannot know the strictness; this is where it is
    /// judged, once the current token is a string literal about to be used.
    pub(crate) fn check_legacy_escape(&self) -> PResult<()> {
        if self.ctx.strict && self.cur().legacy_escape {
            return Err(SyntaxError::new(
                "SyntaxError: legacy octal escape sequences are not allowed in strict mode",
                self.cur().span.start,
            ));
        }
        Ok(())
    }

    /// An IdentifierReference. Looser than a binding: `eval`/`arguments` may be
    /// READ in strict mode, only bound-to is an error.
    pub(crate) fn binding_ident_or_reference(&mut self) -> PResult<(Name, u32)> {
        let TokenKind::Ident { kw, had_escape, private: false, name } = &self.cur().kind else {
            return Err(self.err_here("SyntaxError: expected an identifier"));
        };
        // A ReservedWord is matched by STRING VALUE, so an escaped spelling is
        // still reserved here — `var = 1` is a SyntaxError. The lexer
        // reports `Keyword::None` for an escaped identifier (which is what stops
        // `if` being an `if` statement), so re-classify from the name;
        // skipping the checks for escaped spellings is what let every reserved
        // word through in every identifier position.
        let kw = if *had_escape { Keyword::classify(name) } else { *kw };
        if kw.is_always_reserved() {
            return Err(self.err_here("SyntaxError: unexpected reserved word"));
        }
        if self.ctx.strict && kw.is_strict_reserved() {
            return Err(self.err_here("SyntaxError: reserved word in strict mode"));
        }
        // `yield` and `await` are reserved by CONTEXT, not by strictness, and
        // reaching here means the token was not consumed as a YieldExpression or
        // an AwaitExpression — which for an unescaped spelling cannot happen, so
        // in practice this is the ESCAPED spelling. `yield` is still the
        // reserved word (ReservedWord matches on StringValue) but it is not the
        // KEYWORD, so it can be neither the operator nor an identifier: it is
        // simply an error, and without these two lines it silently became a
        // variable reference.
        // `ContainsArguments` — a class field initializer and a static block have
        // no `arguments` binding of their own and may not reach the enclosing
        // one, so the mere IDENTIFIER is an early error. `ctx.in_field_init` is
        // cleared by `parse_fn_tail` (a nested function has its own `arguments`)
        // and inherited by arrows (which do not), which is exactly the spec's
        // split. The compiler has the same check, but loses the flag on the
        // static-field, static-block and computed-key paths.
        if self.ctx.in_field_init && name == "arguments" {
            return Err(self.err_here(
                "SyntaxError: 'arguments' is not allowed in a class field initializer \
                 or static block",
            ));
        }
        if self.ctx.yield_ && kw == Keyword::Yield {
            return Err(self.err_here("SyntaxError: 'yield' is reserved inside a generator"));
        }
        // The [Await] parameter, plus 13.1.1's goal-level rule: in a Module
        // `await` is never an Identifier, not even inside a nested non-async
        // `function` (which clears `ctx.await_`).
        if (self.ctx.await_ || self.goal == Goal::Module) && kw == Keyword::Await {
            return Err(self.err_here(
                "SyntaxError: 'await' is reserved inside an async function or module",
            ));
        }
        let pos = self.cur().span.start;
        // Legal HERE (a sloppy Script's `await` is an ordinary name), but the
        // region may still turn out to be an async arrow's parameters, which
        // carry [+Await] — record and let the `=>` decide. See `Cover::
        // await_ident`.
        if kw == Keyword::Await {
            self.cover.await_ident.push(SyntaxError::new(
                "SyntaxError: 'await' is not a valid name in async function parameters",
                pos,
            ));
        }
        let tok = self.bump_after_operand()?;
        let TokenKind::Ident { name, .. } = tok.kind else { unreachable!() };
        Ok((name.into_boxed_str(), pos))
    }

    // ---- literals ----------------------------------------------------------

    fn parse_template(&mut self) -> PResult<TemplateLit> {
        let mut quasis = Vec::new();
        let mut exprs = Vec::new();
        let TokenKind::Template { cooked, raw, tail, .. } = self.cur().kind.clone() else {
            return Err(self.err_here("SyntaxError: expected a template literal"));
        };
        quasis.push(TemplateElement { cooked, raw: raw.into_boxed_str() });
        let mut done = tail;
        // A head chunk ending in `${` is followed by an Expression, so a `/`
        // there is a regex; a TemplateTail is an operand, so it is division.
        self.bump(!done)?;
        // A substitution is a full `Expression[+In]` — see `parse_args`;
        // `for (x = `${1 in {}}`;;)` is legal.
        let saved_in = self.ctx.in_;
        self.ctx.in_ = true;
        while !done {
            exprs.push(self.parse_expr_full()?);
            // The `}` that closes a substitution is not a punctuator here — the
            // lexer must resume template scanning AT it.
            if !self.at(Punct::RBrace) {
                return Err(self.err_here("SyntaxError: expected '}' in template literal"));
            }
            let tok = self.resume_template()?;
            let TokenKind::Template { cooked, raw, tail, .. } = tok.kind else {
                return Err(self.err_here("SyntaxError: malformed template literal"));
            };
            quasis.push(TemplateElement { cooked, raw: raw.into_boxed_str() });
            done = tail;
        }
        self.ctx.in_ = saved_in;
        Ok(TemplateLit { quasis, exprs })
    }

    fn parse_array_literal(&mut self) -> PResult<Expr> {
        self.expect(Punct::LBracket, true)?;
        // `[In]` restores inside the brackets — see `parse_args`.
        let saved_in = self.ctx.in_;
        self.ctx.in_ = true;
        let mut items: Vec<Option<ArrayElem>> = Vec::new();
        let mut rest_comma = false;
        loop {
            if self.at(Punct::RBracket) {
                break;
            }
            if self.at(Punct::Comma) {
                // A hole, not `undefined`.
                self.bump_before_operand()?;
                items.push(None);
                continue;
            }
            if self.at(Punct::DotDotDot) {
                let pos = self.cur().span.start;
                self.bump_before_operand()?;
                let _ = pos;
                // In a LITERAL a spread may sit anywhere (`[...a, b]` is
                // ordinary). The rest-must-be-last rule applies only to the
                // PATTERN reading, and `expr_to_target`/`expr_to_pattern`
                // enforce it during conversion — recording it here as a cover
                // error rejected valid literals.
                items.push(Some(ArrayElem::Spread(self.parse_assign()?)));
            } else {
                items.push(Some(ArrayElem::Expr(self.parse_assign()?)));
            }
            if !self.eat(Punct::Comma, true)? {
                break;
            }
            // `[...x,]` — a comma directly after the final spread, with the
            // bracket next. Recorded, not rejected: only the pattern reading
            // forbids it.
            if self.at(Punct::RBracket) && matches!(items.last(), Some(Some(ArrayElem::Spread(_)))) {
                rest_comma = true;
            }
        }
        self.ctx.in_ = saved_in;
        self.expect(Punct::RBracket, false)?;
        Ok(Expr::Array(items, LiteralFlags { rest_comma, parenthesized: false }))
    }

    fn parse_object_literal(&mut self) -> PResult<Expr> {
        self.expect(Punct::LBrace, true)?;
        // `[In]` restores inside the braces — see `parse_args`.
        let saved_in = self.ctx.in_;
        self.ctx.in_ = true;
        let mut members = Vec::new();
        let mut rest_comma = false;
        let mut proto_count = 0usize;
        let mut proto_pos = 0u32;
        while !self.at(Punct::RBrace) {
            if self.at(Punct::DotDotDot) {
                self.bump_before_operand()?;
                members.push(ObjectMember::Spread(self.parse_assign()?));
            } else {
                let (m, is_proto, pos) = self.parse_object_member()?;
                if is_proto {
                    proto_count += 1;
                    if proto_count == 2 {
                        proto_pos = pos;
                    }
                }
                members.push(m);
            }
            if !self.eat(Punct::Comma, true)? {
                break;
            }
            // `{...x,}` — see the Array case; legal only as a literal.
            if self.at(Punct::RBrace) && matches!(members.last(), Some(ObjectMember::Spread(_))) {
                rest_comma = true;
            }
        }
        self.ctx.in_ = saved_in;
        self.expect(Punct::RBrace, false)?;
        // Duplicate `__proto__` is an early error — but only for PLAIN
        // `__proto__: v` properties, not shorthand, methods or computed keys,
        // and not when the literal is a destructuring target.
        if proto_count > 1 {
            // An error only for an actual LITERAL — duplicate `__proto__` in a
            // destructuring target is legal — so it too fires on the
            // expression reading.
            self.cover_pattern_only(SyntaxError::new(
                "SyntaxError: duplicate __proto__ property in object literal",
                proto_pos,
            ));
        }
        Ok(Expr::Object(members, LiteralFlags { rest_comma, parenthesized: false }))
    }

    /// Returns the member, whether it is a plain `__proto__: v`, and its position.
    fn parse_object_member(&mut self) -> PResult<(ObjectMember, bool, u32)> {
        let start = self.cur().span.start;

        // `get x() {}` / `set x(v) {}` — but `get` alone is a property name.
        for (kw, is_get) in [(Keyword::Get, true), (Keyword::Set, false)] {
            if self.at_kw(kw) {
                let save = self.save();
                self.bump_after_operand()?;
                if self.at_property_name_start() {
                    let key = self.parse_prop_key()?;
                    let func = self.parse_method_rest(false, false, start)?;
                    self.check_accessor_arity(is_get, &func.params)?;
                    let m = if is_get {
                        ObjectMember::Get { key, func: Box::new(func) }
                    } else {
                        ObjectMember::Set { key, func: Box::new(func) }
                    };
                    return Ok((m, false, start));
                }
                self.restore(save);
                break;
            }
        }
        // `async m() {}`, `async *m() {}`
        if self.at_kw(Keyword::Async) {
            let save = self.save();
            self.bump_after_operand()?;
            if !self.cur().newline_before && (self.at_property_name_start() || self.at(Punct::Star))
            {
                let gen = self.eat(Punct::Star, false)?;
                let key = self.parse_prop_key()?;
                let func = self.parse_method_rest(true, gen, start)?;
                return Ok((ObjectMember::Method { key, func: Box::new(func) }, false, start));
            }
            self.restore(save);
        }
        // `*gen() {}`
        if self.at(Punct::Star) {
            self.bump_after_operand()?;
            let key = self.parse_prop_key()?;
            let func = self.parse_method_rest(false, true, start)?;
            return Ok((ObjectMember::Method { key, func: Box::new(func) }, false, start));
        }

        // A PropertyName is unrestricted — `{ break: 1 }`, `{ if: 1 }`,
        // `{ break: 1 }` are all fine, because a reserved word is still a
        // perfectly good property NAME. Shorthand re-reads the very same token
        // as an IdentifierReference, where it is not. `parse_prop_key` has
        // consumed the keyword classification by the time that is known, so
        // settle the question here, before the token is gone.
        let key_is_ident_ref = self.is_binding_ident();
        let key_pos = self.cur().span.start;
        let key = self.parse_prop_key()?;
        // `m() {}`
        if self.at(Punct::LParen) {
            let func = self.parse_method_rest(false, false, start)?;
            return Ok((ObjectMember::Method { key, func: Box::new(func) }, false, start));
        }
        // `k: v`
        if self.eat(Punct::Colon, true)? {
            let value = self.parse_assign()?;
            let is_proto = matches!(&key, PropKey::Ident(n) if &**n == "__proto__")
                || matches!(&key, PropKey::Str(s) if s.to_lossy_string() == "__proto__");
            return Ok((
                ObjectMember::Prop { key, value, shorthand: false, init: None },
                is_proto,
                start,
            ));
        }
        // Shorthand `{a}` or CoverInitializedName `{a = 1}`.
        let PropKey::Ident(name) = &key else {
            return Err(self.err_here("SyntaxError: expected ':' after property name"));
        };
        if !key_is_ident_ref {
            return Err(SyntaxError::new(
                format!("SyntaxError: '{name}' is not a valid shorthand property name"),
                key_pos,
            ));
        }
        let name = name.clone();
        // Shorthand re-reads the key as an IdentifierReference without going
        // through `binding_ident_or_reference`, so record `{await}` here too —
        // `async({await}) => {}` is the same early error as `async(await) =>
        // {}`.
        if &*name == "await" {
            self.cover.await_ident.push(SyntaxError::new(
                "SyntaxError: 'await' is not a valid name in async function parameters",
                key_pos,
            ));
        }
        let mut init = None;
        if self.at(Punct::Eq) {
            self.bump_before_operand()?;
            init = Some(self.parse_assign()?);
            // Legal ONLY if this literal turns out to be a destructuring
            // target, so it fires when the region resolves to an EXPRESSION —
            // in arrow params (`({a = 1}) => …`) it is a default and fine.
            self.cover_pattern_only(SyntaxError::new(
                "SyntaxError: invalid shorthand property initializer",
                start,
            ));
        }
        Ok((
            ObjectMember::Prop { key, value: Expr::Ident(name), shorthand: true, init },
            false,
            start,
        ))
    }

    fn at_property_name_start(&self) -> bool {
        matches!(
            self.cur().kind,
            // `NumericLiteral : DecimalBigIntegerLiteral` — a BigInt is a
            // LiteralPropertyName like any other number, so `{ get 5n(){} }`
            // has to reach the accessor path rather than falling through and
            // re-reading `get` as a shorthand property.
            TokenKind::Ident { .. } | TokenKind::Str(_) | TokenKind::Num(_) | TokenKind::BigInt(_)
        ) || self.at(Punct::LBracket)
    }

    pub(crate) fn parse_prop_key(&mut self) -> PResult<PropKey> {
        match self.cur().kind.clone() {
            TokenKind::Str(s) => {
                self.check_legacy_escape()?;
                self.bump_after_operand()?;
                Ok(PropKey::Str(s))
            }
            TokenKind::Num(n) => {
                self.bump_after_operand()?;
                Ok(PropKey::Num(n.value))
            }
            // A BigInt literal is a legal PropertyName: `{1n: "a"}` has the key
            // "1", because the key is `ToString(NumericValue)` and a BigInt's
            // string form carries no `n` suffix. `{1n: x}` and `{1: x}` are
            // therefore the SAME property, which is why this reuses the numeric
            // key rather than inventing a BigInt one.
            TokenKind::BigInt(digits) => {
                self.bump_after_operand()?;
                // …but a BigInt's ToString is EXACT, and f64 is not:
                // `{999999999999999999n: 1}` keys "999999999999999999", never
                // the rounded "1000000000000000000". 15 decimal digits is the
                // widest that always fits in an f64 (10^15 < 2^53), so anything
                // longer carries its own digits (the lexer has already
                // normalised `0x`/`0o`/`0b`/`_` to decimal).
                if digits.len() <= 15 {
                    let n: f64 = digits.parse().unwrap_or(f64::NAN);
                    Ok(PropKey::Num(n))
                } else {
                    Ok(PropKey::Str(StrVal::Utf8(digits)))
                }
            }
            TokenKind::Ident { private: true, name, .. } => {
                self.bump_after_operand()?;
                Ok(PropKey::Private(name.into_boxed_str()))
            }
            TokenKind::Punct(Punct::LBracket) => {
                self.bump_before_operand()?;
                let e = self.parse_assign()?;
                self.expect(Punct::RBracket, false)?;
                Ok(PropKey::Computed(e))
            }
            TokenKind::Ident { .. } => Ok(PropKey::Ident(self.ident_name()?)),
            _ => Err(self.err_here("SyntaxError: expected a property name")),
        }
    }
}
