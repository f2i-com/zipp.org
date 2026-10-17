//! Expressions, call arguments, comprehensions and parameter lists.

use super::{range, Follow, PResult, Parser};
use crate::ast::*;
use crate::intern::Sym;
use crate::lexer::Error;
use crate::token::T;
use rustc_hash::FxHashSet;

// Binary levels, loosest first; unary operators, `**`, `await` and the
// postfix forms bind tighter and are parsed directly.
const L_OR: u8 = 1;
const L_AND: u8 = 2;
const L_NOT: u8 = 3;
const L_CMP: u8 = 4;
pub(super) const L_BOR: u8 = 5;

/// What an element parsed as `TestOrStarNamedExpr` was, syntactically (a
/// parenthesized named expression is a `Test`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Test,
    Named,
    Star,
}

/// A `Test` waiting for its last part: a lambda's body, or the `else`
/// branch of `body if test else ...`.
enum TestHead {
    Lambda(u32, Option<ArgsId>),
    IfExp(u32, ExprId, ExprId),
}

#[inline]
fn binary_op(tok: T) -> Option<(u8, Operator)> {
    use Operator as O;
    Some(match tok {
        T::Vbar => (5, O::BitOr),
        T::CircumFlex => (6, O::BitXor),
        T::Amper => (7, O::BitAnd),
        T::LeftShift => (8, O::LShift),
        T::RightShift => (8, O::RShift),
        T::Plus => (9, O::Add),
        T::Minus => (9, O::Sub),
        T::Star => (10, O::Mult),
        T::Slash => (10, O::Div),
        T::DoubleSlash => (10, O::FloorDiv),
        T::Percent => (10, O::Mod),
        T::At => (10, O::MatMult),
        _ => return None,
    })
}

#[inline]
fn is_comparison(tok: T) -> bool {
    matches!(
        tok,
        T::EqEqual
            | T::NotEqual
            | T::Less
            | T::LessEqual
            | T::Greater
            | T::GreaterEqual
            | T::In
            | T::Not
            | T::Is
    )
}

/// Tokens that can start `Expression` (a bitwise-or level expression).
#[inline]
pub(super) fn starts_expression(tok: T) -> bool {
    matches!(
        tok,
        T::Name
            | T::Int
            | T::Float
            | T::Complex
            | T::String
            | T::FStringStart
            | T::FStringBroken
            | T::Lpar
            | T::Lsqb
            | T::Lbrace
            | T::True
            | T::False
            | T::None
            | T::Ellipsis
            | T::Minus
            | T::Plus
            | T::Tilde
            | T::Await
    )
}

/// Tokens that can start `Test`.
#[inline]
pub(super) fn starts_test(tok: T) -> bool {
    starts_expression(tok) || matches!(tok, T::Not | T::Lambda)
}

impl Parser<'_> {
    /// `Test<"all">`: a lambda, a conditional expression or an `or` test.
    pub(super) fn parse_test(&mut self) -> PResult<ExprId> {
        self.enter()?;
        let result = if self.at(T::Lambda) {
            self.parse_test_chain(None)
        } else {
            let start = self.start();
            match self.parse_binary(L_OR) {
                Ok(body) if self.at(T::If) => self.parse_test_chain(Some((start, body))),
                result => result,
            }
        };
        self.leave();
        result
    }

    /// Lambdas and conditional expressions nest to the right; the chain is
    /// read iteratively and folded afterwards (innermost first, as the
    /// grammar reduces them).
    #[inline(never)]
    fn parse_test_chain(&mut self, mut first: Option<(u32, ExprId)>) -> PResult<ExprId> {
        let mut pending = Vec::new();
        let mut expr = loop {
            let (start, body) = match first.take() {
                Some(first) => first,
                None => {
                    let start = self.start();
                    if self.at(T::Lambda) {
                        self.bump();
                        let params = if self.at(T::Colon) {
                            None
                        } else {
                            Some(self.parse_parameter_list(false)?)
                        };
                        self.expect(T::Colon)?;
                        pending.push(TestHead::Lambda(start, params));
                        continue;
                    }
                    (start, self.parse_binary(L_OR)?)
                }
            };
            if !self.at(T::If) {
                break body;
            }
            self.bump();
            let test = self.parse_binary(L_OR)?;
            self.expect(T::Else)?;
            pending.push(TestHead::IfExp(start, body, test));
        };
        let end = self.prev_end;
        while let Some(head) = pending.pop() {
            expr = match head {
                TestHead::Lambda(start, params) => {
                    let args = match params {
                        Some(args) => {
                            if let Err(error) = self.validate_arguments(args) {
                                return Err(self.reduce_error(error, Follow::Test));
                            }
                            args
                        }
                        None => self.empty_arguments(range(start, end)),
                    };
                    self.add_expr(range(start, end), ExprKind::Lambda { args, body: expr })
                }
                TestHead::IfExp(start, body, test) => self.add_expr(
                    range(start, end),
                    ExprKind::IfExp {
                        test,
                        body,
                        orelse: expr,
                    },
                ),
            };
        }
        Ok(expr)
    }

    /// The current number token's constant (not consumed).
    pub(super) fn number(&self) -> Constant {
        let tok = self.token();
        let text = Span {
            start: tok.start,
            len: tok.end - tok.start,
        };
        match tok.kind {
            T::Int => Constant::Int {
                radix: tok.flags,
                text,
            },
            T::Float => Constant::Float { text },
            _ => Constant::Complex { text },
        }
    }

    pub(super) fn empty_arguments(&mut self, range: Range) -> ArgsId {
        let id = ArgsId::from_index(self.m.arguments.len());
        self.m.arguments.push(Arguments {
            range,
            posonlyargs: List::default(),
            args: List::default(),
            vararg: None,
            kwonlyargs: List::default(),
            kwarg: None,
        });
        id
    }

    /// `OrTest<"all">`.
    #[inline]
    pub(super) fn parse_or_test(&mut self) -> PResult<ExprId> {
        self.parse_binary(L_OR)
    }

    /// `Expression<"all">`: a bitwise-or level expression.
    #[inline]
    pub(super) fn parse_expression(&mut self) -> PResult<ExprId> {
        self.parse_binary(L_BOR)
    }

    /// Every binary level from `or` down to `*`, by precedence climbing.
    fn parse_binary(&mut self, min: u8) -> PResult<ExprId> {
        let start = self.start();
        let mut left = if min <= L_NOT && self.at(T::Not) {
            self.parse_not()?
        } else {
            self.parse_factor()?
        };
        loop {
            let tok = self.tok();
            if let Some((level, op)) = binary_op(tok) {
                if level < min {
                    break;
                }
                self.bump();
                let right = self.parse_binary(level + 1)?;
                left = self.add_expr(
                    range(start, self.prev_end),
                    ExprKind::BinOp { left, op, right },
                );
                continue;
            }
            if is_comparison(tok) {
                if L_CMP < min {
                    break;
                }
                left = self.parse_comparison(start, left)?;
                continue;
            }
            let (level, op) = match tok {
                T::And => (L_AND, BoolOp::And),
                T::Or => (L_OR, BoolOp::Or),
                _ => break,
            };
            if level < min {
                break;
            }
            left = self.parse_bool_op(start, left, op, tok, level)?;
        }
        Ok(left)
    }

    #[inline(never)]
    fn parse_not(&mut self) -> PResult<ExprId> {
        let mut nots = Vec::new();
        while self.at(T::Not) {
            nots.push(self.start());
            self.bump();
        }
        let mut operand = self.parse_binary(L_CMP)?;
        for location in nots.into_iter().rev() {
            operand = self.add_expr(
                range(location, self.prev_end),
                ExprKind::UnaryOp {
                    op: UnaryOp::Not,
                    operand,
                },
            );
        }
        Ok(operand)
    }

    #[inline(never)]
    fn parse_comparison(&mut self, start: u32, left: ExprId) -> PResult<ExprId> {
        let ops_base = self.s.cmp_ops.len();
        let base = self.s.exprs.len();
        while is_comparison(self.tok()) {
            let tok = self.tok();
            self.bump();
            let op = match tok {
                T::EqEqual => CmpOp::Eq,
                T::NotEqual => CmpOp::NotEq,
                T::Less => CmpOp::Lt,
                T::LessEqual => CmpOp::LtE,
                T::Greater => CmpOp::Gt,
                T::GreaterEqual => CmpOp::GtE,
                T::In => CmpOp::In,
                T::Not => {
                    self.expect(T::In)?;
                    CmpOp::NotIn
                }
                _ => {
                    if self.at(T::Not) {
                        self.bump();
                        CmpOp::IsNot
                    } else {
                        CmpOp::Is
                    }
                }
            };
            self.s.cmp_ops.push(op);
            let right = self.parse_binary(L_BOR)?;
            self.s.exprs.push(right);
        }
        let ops = self.finish_cmp_ops(ops_base);
        let comparators = self.finish_exprs(base);
        Ok(self.add_expr(
            range(start, self.prev_end),
            ExprKind::Compare {
                left,
                ops,
                comparators,
            },
        ))
    }

    #[inline(never)]
    fn parse_bool_op(
        &mut self,
        start: u32,
        left: ExprId,
        op: BoolOp,
        tok: T,
        level: u8,
    ) -> PResult<ExprId> {
        let base = self.s.exprs.len();
        self.s.exprs.push(left);
        while self.at(tok) {
            self.bump();
            let value = self.parse_binary(level + 1)?;
            self.s.exprs.push(value);
        }
        let values = self.finish_exprs(base);
        Ok(self.add_expr(range(start, self.prev_end), ExprKind::BoolOp { op, values }))
    }

    /// `Factor`: unary `+ - ~`, then `Power`: `AtomExpr ("**" Factor)?`.
    /// Both nest to the right; they are read iteratively and folded.
    fn parse_factor(&mut self) -> PResult<ExprId> {
        if !matches!(self.tok(), T::Plus | T::Minus | T::Tilde) {
            let start = self.start();
            let base = self.parse_await_expr()?;
            if !self.at(T::DoubleStar) {
                return Ok(base);
            }
            return self.parse_factor_chain(Vec::new(), start, base);
        }
        let ops = self.parse_unary_ops();
        let start = self.start();
        let base = self.parse_await_expr()?;
        if !self.at(T::DoubleStar) {
            return Ok(self.wrap_unary(ops, base, self.prev_end));
        }
        self.parse_factor_chain(ops, start, base)
    }

    #[inline(never)]
    fn parse_factor_chain(
        &mut self,
        ops: Vec<(UnaryOp, u32)>,
        start: u32,
        base: ExprId,
    ) -> PResult<ExprId> {
        let mut chain = vec![(ops, start, base)];
        while self.at(T::DoubleStar) {
            self.bump();
            let ops = self.parse_unary_ops();
            let start = self.start();
            let base = self.parse_await_expr()?;
            chain.push((ops, start, base));
        }
        let end = self.prev_end;
        let (ops, _, base) = chain.pop().unwrap();
        let mut factor = self.wrap_unary(ops, base, end);
        while let Some((ops, start, base)) = chain.pop() {
            let power = self.add_expr(
                range(start, end),
                ExprKind::BinOp {
                    left: base,
                    op: Operator::Pow,
                    right: factor,
                },
            );
            factor = self.wrap_unary(ops, power, end);
        }
        Ok(factor)
    }

    fn wrap_unary(&mut self, ops: Vec<(UnaryOp, u32)>, mut operand: ExprId, end: u32) -> ExprId {
        for (op, location) in ops.into_iter().rev() {
            operand = self.add_expr(range(location, end), ExprKind::UnaryOp { op, operand });
        }
        operand
    }

    fn parse_unary_ops(&mut self) -> Vec<(UnaryOp, u32)> {
        let mut ops = Vec::new();
        loop {
            let op = match self.tok() {
                T::Plus => UnaryOp::UAdd,
                T::Minus => UnaryOp::USub,
                T::Tilde => UnaryOp::Invert,
                _ => return ops,
            };
            ops.push((op, self.start()));
            self.bump();
        }
    }

    /// `AtomExpr`: an optional `await` before the postfix expression.
    fn parse_await_expr(&mut self) -> PResult<ExprId> {
        if !self.at(T::Await) {
            return self.parse_atom_expr();
        }
        let start = self.start();
        self.bump();
        let value = self.parse_atom_expr()?;
        Ok(self.add_expr(range(start, self.prev_end), ExprKind::Await { value }))
    }

    /// `AtomExpr2`: an atom followed by calls, subscripts and attributes.
    fn parse_atom_expr(&mut self) -> PResult<ExprId> {
        let start = self.start();
        let mut expr = self.parse_atom()?;
        loop {
            match self.tok() {
                T::Lpar => {
                    self.bump();
                    let (args, keywords) = self.parse_call_args()?;
                    self.bump(); // `)`
                    expr = self.add_expr(
                        range(start, self.prev_end),
                        ExprKind::Call {
                            func: expr,
                            args,
                            keywords,
                        },
                    );
                }
                T::Lsqb => {
                    self.bump();
                    let slice = self.parse_subscript_list()?;
                    self.expect(T::Rsqb)?;
                    expr = self.add_expr(
                        range(start, self.prev_end),
                        ExprKind::Subscript {
                            value: expr,
                            slice,
                            ctx: ExprContext::Load,
                        },
                    );
                }
                T::Dot => {
                    self.bump();
                    let attr = self.identifier()?;
                    expr = self.add_expr(
                        range(start, self.prev_end),
                        ExprKind::Attribute {
                            value: expr,
                            attr,
                            ctx: ExprContext::Load,
                        },
                    );
                }
                _ => return Ok(expr),
            }
        }
    }

    fn parse_atom(&mut self) -> PResult<ExprId> {
        let start = self.start();
        let follow = if self.pos == self.with_item_start {
            Follow::WithItemAtom
        } else {
            Follow::Atom
        };
        let constant = |p: &mut Self, value: Constant| {
            p.bump();
            p.add_expr(range(start, p.prev_end), ExprKind::Constant(value))
        };
        Ok(match self.tok() {
            T::Name => {
                let id = self.identifier()?;
                self.add_expr(
                    range(start, self.prev_end),
                    ExprKind::Name {
                        id,
                        ctx: ExprContext::Load,
                    },
                )
            }
            T::Int | T::Float | T::Complex => {
                let value = self.number();
                constant(self, value)
            }
            T::String | T::FStringStart | T::FStringBroken => return self.parse_strings(follow),
            T::True => constant(self, Constant::True),
            T::False => constant(self, Constant::False),
            T::None => constant(self, Constant::None),
            T::Ellipsis => constant(self, Constant::Ellipsis),
            T::Lpar => return self.parse_paren(start, follow),
            T::Lsqb => return self.parse_list_display(start),
            T::Lbrace => return self.parse_brace(start),
            _ => return Err(self.unexpected()),
        })
    }

    /// After `(`: a tuple, a parenthesized expression, a generator
    /// expression or a parenthesized `yield`.
    #[inline(never)]
    fn parse_paren(&mut self, start: u32, follow: Follow) -> PResult<ExprId> {
        self.bump();
        match self.tok() {
            T::Rpar => {
                self.bump();
                let elts = List::default();
                return Ok(self.add_expr(
                    range(start, self.prev_end),
                    ExprKind::Tuple {
                        elts,
                        ctx: ExprContext::Load,
                    },
                ));
            }
            T::Yield => {
                let expr = self.parse_yield()?;
                self.expect(T::Rpar)?;
                return Ok(expr);
            }
            T::DoubleStar => {
                let location = self.start();
                self.bump();
                self.parse_expression()?;
                self.expect(T::Rpar)?;
                return Err(self.reduce_error(
                    Error::new("cannot use double starred expression here", location),
                    follow,
                ));
            }
            _ => {}
        }
        let (first, kind) = self.parse_elem()?;
        if kind != Kind::Star && matches!(self.tok(), T::For | T::Async) {
            let generators = self.parse_comp_for()?;
            self.expect(T::Rpar)?;
            return Ok(self.add_expr(
                range(start, self.prev_end),
                ExprKind::GeneratorExp {
                    elt: first,
                    generators,
                },
            ));
        }
        if self.at(T::Rpar) {
            self.bump();
            if kind == Kind::Star {
                let at = self.expr_range(first).start;
                return Err(
                    self.reduce_error(Error::new("cannot use starred expression here", at), follow)
                );
            }
            return Ok(first);
        }
        let base = self.s.exprs.len();
        self.s.exprs.push(first);
        while self.at(T::Comma) {
            self.bump();
            if self.at(T::Rpar) {
                break;
            }
            let elt = self.parse_elem()?.0;
            self.s.exprs.push(elt);
        }
        self.expect(T::Rpar)?;
        let elts = self.finish_exprs(base);
        Ok(self.add_expr(
            range(start, self.prev_end),
            ExprKind::Tuple {
                elts,
                ctx: ExprContext::Load,
            },
        ))
    }

    #[inline(never)]
    fn parse_list_display(&mut self, start: u32) -> PResult<ExprId> {
        self.bump();
        if self.at(T::Rsqb) {
            self.bump();
            return Ok(self.add_expr(
                range(start, self.prev_end),
                ExprKind::List {
                    elts: List::default(),
                    ctx: ExprContext::Load,
                },
            ));
        }
        let (first, _) = self.parse_elem()?;
        if matches!(self.tok(), T::For | T::Async) {
            let generators = self.parse_comp_for()?;
            self.expect(T::Rsqb)?;
            return Ok(self.add_expr(
                range(start, self.prev_end),
                ExprKind::ListComp {
                    elt: first,
                    generators,
                },
            ));
        }
        let base = self.s.exprs.len();
        self.s.exprs.push(first);
        while self.at(T::Comma) {
            self.bump();
            if self.at(T::Rsqb) {
                break;
            }
            let elt = self.parse_elem()?.0;
            self.s.exprs.push(elt);
        }
        self.expect(T::Rsqb)?;
        let elts = self.finish_exprs(base);
        Ok(self.add_expr(
            range(start, self.prev_end),
            ExprKind::List {
                elts,
                ctx: ExprContext::Load,
            },
        ))
    }

    #[inline(never)]
    fn parse_brace(&mut self, start: u32) -> PResult<ExprId> {
        self.bump();
        let kbase = self.s.opt_exprs.len();
        let vbase = self.s.exprs.len();
        match self.tok() {
            T::Rbrace => {
                self.bump();
                let keys = self.finish_opt_exprs(kbase);
                let values = self.finish_exprs(vbase);
                return Ok(
                    self.add_expr(range(start, self.prev_end), ExprKind::Dict { keys, values })
                );
            }
            T::DoubleStar => {
                self.bump();
                self.s.opt_exprs.push(None);
                let value = self.parse_expression()?;
                self.s.exprs.push(value);
            }
            _ => {
                let (first, kind) = self.parse_elem()?;
                if kind == Kind::Test && self.at(T::Colon) {
                    self.bump();
                    let value = self.parse_test()?;
                    if matches!(self.tok(), T::For | T::Async) {
                        let generators = self.parse_comp_for()?;
                        self.expect(T::Rbrace)?;
                        return Ok(self.add_expr(
                            range(start, self.prev_end),
                            ExprKind::DictComp {
                                key: first,
                                value,
                                generators,
                            },
                        ));
                    }
                    self.s.opt_exprs.push(Some(first));
                    self.s.exprs.push(value);
                } else {
                    return self.parse_set(start, first, kind);
                }
            }
        }
        while self.at(T::Comma) {
            self.bump();
            if self.at(T::Rbrace) {
                break;
            }
            if self.at(T::DoubleStar) {
                self.bump();
                self.s.opt_exprs.push(None);
                let value = self.parse_expression()?;
                self.s.exprs.push(value);
            } else {
                let key = self.parse_test()?;
                self.expect(T::Colon)?;
                self.s.opt_exprs.push(Some(key));
                let value = self.parse_test()?;
                self.s.exprs.push(value);
            }
        }
        self.expect(T::Rbrace)?;
        let keys = self.finish_opt_exprs(kbase);
        let values = self.finish_exprs(vbase);
        Ok(self.add_expr(range(start, self.prev_end), ExprKind::Dict { keys, values }))
    }

    #[inline(never)]
    fn parse_set(&mut self, start: u32, first: ExprId, kind: Kind) -> PResult<ExprId> {
        if kind != Kind::Star && matches!(self.tok(), T::For | T::Async) {
            let generators = self.parse_comp_for()?;
            self.expect(T::Rbrace)?;
            return Ok(self.add_expr(
                range(start, self.prev_end),
                ExprKind::SetComp {
                    elt: first,
                    generators,
                },
            ));
        }
        let base = self.s.exprs.len();
        self.s.exprs.push(first);
        while self.at(T::Comma) {
            self.bump();
            if self.at(T::Rbrace) {
                break;
            }
            let elt = self.parse_elem()?.0;
            self.s.exprs.push(elt);
        }
        self.expect(T::Rbrace)?;
        let elts = self.finish_exprs(base);
        Ok(self.add_expr(range(start, self.prev_end), ExprKind::Set { elts }))
    }

    /// `NamedExpressionTest`: `name := Test`, or a `Test`.
    pub(super) fn parse_named_test(&mut self) -> PResult<(ExprId, Kind)> {
        if self.at(T::Name) && self.peek(1) == T::ColonEqual {
            let start = self.start();
            let id = self.identifier()?;
            let target_range = range(start, self.prev_end);
            self.bump(); // `:=`
            let value = self.parse_test()?;
            let end = self.expr_range(value).end;
            let target = self.add_expr(
                target_range,
                ExprKind::Name {
                    id,
                    ctx: ExprContext::Store,
                },
            );
            return Ok((
                self.add_expr(range(start, end), ExprKind::NamedExpr { target, value }),
                Kind::Named,
            ));
        }
        Ok((self.parse_test()?, Kind::Test))
    }

    /// `StarExpr`: `*` Expression.
    pub(super) fn parse_star_expr(&mut self) -> PResult<ExprId> {
        let start = self.start();
        self.bump();
        let value = self.parse_expression()?;
        Ok(self.add_expr(
            range(start, self.prev_end),
            ExprKind::Starred {
                value,
                ctx: ExprContext::Load,
            },
        ))
    }

    /// `TestOrStarNamedExpr`.
    pub(super) fn parse_elem(&mut self) -> PResult<(ExprId, Kind)> {
        if self.at(T::Star) {
            return Ok((self.parse_star_expr()?, Kind::Star));
        }
        self.parse_named_test()
    }

    /// `GenericList<TestOrStarExpr>` (`test_level`) or
    /// `GenericList<ExpressionOrStarExpression>`: one element, or a tuple of
    /// them with an optional trailing comma. The flag says whether the
    /// result is a single `Test`.
    pub(super) fn parse_list(&mut self, test_level: bool) -> PResult<(ExprId, bool)> {
        let start = self.start();
        let (first, single_test) = self.parse_list_elem(test_level)?;
        if !self.at(T::Comma) {
            return Ok((first, single_test));
        }
        let base = self.s.exprs.len();
        self.s.exprs.push(first);
        while self.at(T::Comma) {
            self.bump();
            let tok = self.tok();
            let more = if test_level {
                starts_test(tok)
            } else {
                starts_expression(tok)
            };
            if !more && tok != T::Star {
                break;
            }
            let elt = self.parse_list_elem(test_level)?.0;
            self.s.exprs.push(elt);
        }
        let elts = self.finish_exprs(base);
        Ok((
            self.add_expr(
                range(start, self.prev_end),
                ExprKind::Tuple {
                    elts,
                    ctx: ExprContext::Load,
                },
            ),
            false,
        ))
    }

    fn parse_list_elem(&mut self, test_level: bool) -> PResult<(ExprId, bool)> {
        if self.at(T::Star) {
            Ok((self.parse_star_expr()?, false))
        } else if test_level {
            Ok((self.parse_test()?, true))
        } else {
            Ok((self.parse_expression()?, false))
        }
    }

    /// `TestListOrYieldExpr`.
    pub(super) fn parse_list_or_yield(&mut self) -> PResult<ExprId> {
        if self.at(T::Yield) {
            self.parse_yield()
        } else {
            Ok(self.parse_list(true)?.0)
        }
    }

    /// `YieldExpr`: `yield [TestList]` or `yield from Test`.
    pub(super) fn parse_yield(&mut self) -> PResult<ExprId> {
        let start = self.start();
        self.bump();
        if self.at(T::From) {
            self.bump();
            let value = self.parse_test()?;
            return Ok(self.add_expr(range(start, self.prev_end), ExprKind::YieldFrom { value }));
        }
        let value = if starts_test(self.tok()) || self.at(T::Star) {
            Some(self.parse_list(true)?.0)
        } else {
            None
        };
        Ok(self.add_expr(range(start, self.prev_end), ExprKind::Yield { value }))
    }

    /// `ParameterList`, for a `def` (`typed`, closed by `)`) or a lambda
    /// (closed by `:`). The current token is not the closing one.
    pub(super) fn parse_parameter_list(&mut self, typed: bool) -> PResult<ArgsId> {
        let close = if typed { T::Rpar } else { T::Colon };
        let start = self.start();
        let base = self.s.params.len();
        let mut posonly_len = 0usize;
        let mut args_start = base;
        let mut vararg = None;
        let mut kwonly_start = usize::MAX;
        let mut kwarg = None;
        let mut bare_star = None;
        let has_defs = self.at(T::Name);
        let mut rest_allowed = !has_defs;
        if has_defs {
            let param = self.parse_parameter_def(typed)?;
            self.s.params.push(param);
            let mut slash = false;
            while self.at(T::Comma) {
                match self.peek(1) {
                    T::Name => {
                        self.bump();
                        let param = self.parse_parameter_def(typed)?;
                        self.s.params.push(param);
                    }
                    T::Slash if !slash => {
                        self.bump();
                        self.bump();
                        slash = true;
                        posonly_len = self.s.params.len() - base;
                        args_start = self.s.params.len();
                    }
                    _ => break,
                }
            }
            if self.at(T::Comma) && matches!(self.peek(1), T::Star | T::DoubleStar) {
                self.bump();
                rest_allowed = true;
            }
        }
        let args_end = self.s.params.len();
        if rest_allowed && self.at(T::Star) {
            let star_location = self.start();
            self.bump();
            if self.at(T::Name) {
                vararg = Some(self.parse_star_parameter(typed)?);
            }
            kwonly_start = self.s.params.len();
            let mut has_kwarg = false;
            while self.at(T::Comma) {
                match self.peek(1) {
                    T::Name => {
                        self.bump();
                        let param = self.parse_parameter_def(typed)?;
                        self.s.params.push(param);
                    }
                    T::DoubleStar => {
                        self.bump();
                        kwarg = self.parse_kwarg_parameter(typed)?;
                        has_kwarg = true;
                        break;
                    }
                    _ => break,
                }
            }
            if vararg.is_none() && self.s.params.len() == kwonly_start && !has_kwarg {
                bare_star = Some(star_location);
            }
        } else if rest_allowed && self.at(T::DoubleStar) {
            kwarg = self.parse_kwarg_parameter(typed)?;
        } else if !has_defs {
            return Err(self.unexpected());
        }
        if self.at(T::Comma) {
            self.bump();
        }
        if !self.at(close) {
            return Err(self.unexpected());
        }
        // Checked when the list is reduced, at the closing token.
        if let Some(location) = bare_star {
            return Err(self.reduce_error(
                Error::new("named arguments must follow bare *", location),
                Follow::Checked,
            ));
        }
        if has_defs {
            // A parameter without a default after one with.
            let defs = &self.s.params[base..args_end];
            let first_invalid = defs
                .iter()
                .skip_while(|p| p.default.is_none())
                .find(|p| p.default.is_none());
            if let Some(invalid) = first_invalid {
                let at = invalid.range.start;
                return Err(self.reduce_error(
                    Error::new("non-default argument follows default argument", at),
                    Follow::Checked,
                ));
            }
        }
        // The side table: positional-only, positional, then the keyword-only
        // parameters, then `*args` and `**kwargs`.
        let table_start = self.m.params.len() as u32;
        let all: Vec<Param> = self.s.params.drain(base..).collect();
        let kwonly_from = if kwonly_start == usize::MAX {
            all.len()
        } else {
            kwonly_start - base
        };
        let posonly = List::new(table_start, posonly_len as u32);
        let args_len = (args_end - args_start) as u32;
        let args = List::new(table_start + (args_start - base) as u32, args_len);
        let kwonly = List::new(
            table_start + kwonly_from as u32,
            (all.len() - kwonly_from) as u32,
        );
        self.m.params.extend(all);
        let index_of = |p: Option<Param>, m: &mut Module| {
            p.map(|p| {
                m.params.push(p);
                m.params.len() as u32 - 1
            })
        };
        let vararg = index_of(vararg, &mut self.m);
        let kwarg = index_of(kwarg, &mut self.m);
        let id = ArgsId::from_index(self.m.arguments.len());
        self.m.arguments.push(Arguments {
            range: range(start, self.prev_end),
            posonlyargs: posonly,
            args,
            vararg,
            kwonlyargs: kwonly,
            kwarg,
        });
        Ok(id)
    }

    /// Duplicate parameter names (checked when the parameters' rule is
    /// reduced).
    pub(super) fn validate_arguments(&self, args: ArgsId) -> Result<(), Error> {
        let a = self.m.arguments(args);
        let m = &self.m;
        let mut seen = FxHashSet::default();
        let params = m
            .list(a.posonlyargs)
            .iter()
            .chain(m.list(a.args))
            .chain(m.list(a.kwonlyargs))
            .chain(a.vararg.map(|i| &m.params[i as usize]))
            .chain(a.kwarg.map(|i| &m.params[i as usize]));
        for param in params {
            if !seen.insert(param.name) {
                return Err(Error::new(
                    format!(
                        "duplicate argument '{}' in function definition",
                        m.name(param.name)
                    ),
                    param.range.start,
                ));
            }
        }
        Ok(())
    }

    /// `ParameterDef`: a parameter with an optional default.
    fn parse_parameter_def(&mut self, typed: bool) -> PResult<Param> {
        let start = self.start();
        let name = self.identifier()?;
        let annotation = if typed && self.at(T::Colon) {
            self.bump();
            Some(self.parse_test()?)
        } else {
            None
        };
        let mut param = Param {
            range: range(start, self.prev_end),
            name,
            annotation,
            default: None,
        };
        if self.at(T::Equal) {
            self.bump();
            param.default = Some(self.parse_test()?);
        }
        Ok(param)
    }

    /// The name after `*`, with an annotation that may be starred.
    fn parse_star_parameter(&mut self, typed: bool) -> PResult<Param> {
        let start = self.start();
        let name = self.identifier()?;
        let annotation = if typed && self.at(T::Colon) {
            self.bump();
            Some(if self.at(T::Star) {
                self.parse_star_expr()?
            } else {
                self.parse_test()?
            })
        } else {
            None
        };
        Ok(Param {
            range: range(start, self.prev_end),
            name,
            annotation,
            default: None,
        })
    }

    /// `KwargParameter`: `**` and an optional name.
    fn parse_kwarg_parameter(&mut self, typed: bool) -> PResult<Option<Param>> {
        self.bump(); // `**`
        if !self.at(T::Name) {
            return Ok(None);
        }
        let start = self.start();
        let name = self.identifier()?;
        let annotation = if typed && self.at(T::Colon) {
            self.bump();
            Some(self.parse_test()?)
        } else {
            None
        };
        Ok(Some(Param {
            range: range(start, self.prev_end),
            name,
            annotation,
            default: None,
        }))
    }

    /// `ArgumentList` after `(`, up to (not including) the `)`.
    pub(super) fn parse_call_args(&mut self) -> PResult<(List<ExprId>, List<Keyword>)> {
        let base = self.s.exprs.len();
        let kbase = self.s.keywords.len();
        // The first semantic error, reported once the list is complete.
        let mut error: Option<Error> = None;
        let mut double_starred = false;
        while !self.at(T::Rpar) {
            let start = self.start();
            let keyword = self.at(T::Name) && self.peek(1) == T::Equal;
            match self.tok() {
                T::Star => {
                    self.bump();
                    let value = self.parse_test()?;
                    let expr = self.add_expr(
                        range(start, self.prev_end),
                        ExprKind::Starred {
                            value,
                            ctx: ExprContext::Load,
                        },
                    );
                    self.positional(expr, &mut error, double_starred, kbase);
                }
                T::DoubleStar => {
                    self.bump();
                    let value = self.parse_test()?;
                    double_starred = true;
                    self.s.keywords.push(Keyword {
                        range: range(start, self.prev_end),
                        arg: None,
                        value,
                    });
                }
                T::Name if keyword => {
                    let name = self.identifier()?;
                    self.bump(); // `=`
                    let value = self.parse_test()?;
                    if error.is_none()
                        && self.s.keywords[kbase..].iter().any(|k| k.arg == Some(name))
                    {
                        error = Some(Error::new(
                            format!("keyword argument repeated: {}", self.m.name(name)),
                            start,
                        ));
                    }
                    self.s.keywords.push(Keyword {
                        range: range(start, self.prev_end),
                        arg: Some(name),
                        value,
                    });
                }
                _ => {
                    let (elt, _) = self.parse_named_test()?;
                    let expr = if matches!(self.tok(), T::For | T::Async) {
                        let generators = self.parse_comp_for()?;
                        self.add_expr(
                            range(start, self.prev_end),
                            ExprKind::GeneratorExp { elt, generators },
                        )
                    } else {
                        elt
                    };
                    self.positional(expr, &mut error, double_starred, kbase);
                }
            }
            if self.at(T::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        if !self.at(T::Rpar) {
            return Err(self.unexpected());
        }
        if let Some(error) = error {
            return Err(self.reduce_error(error, Follow::Checked));
        }
        let args = self.finish_exprs(base);
        let keywords = self.finish_keywords(kbase);
        Ok((args, keywords))
    }

    /// A positional argument: not after a keyword argument (unless starred),
    /// not after `**`.
    fn positional(
        &mut self,
        expr: ExprId,
        error: &mut Option<Error>,
        double_starred: bool,
        kbase: usize,
    ) {
        if error.is_none() {
            let starred = matches!(self.m.exprs[expr.index()].kind, ExprKind::Starred { .. });
            let at = self.expr_range(expr).start;
            if self.s.keywords.len() > kbase && !starred {
                *error = Some(Error::new(
                    "positional argument follows keyword argument",
                    at,
                ));
            } else if double_starred {
                *error = Some(Error::new(
                    "iterable argument unpacking follows keyword argument unpacking",
                    at,
                ));
            }
        }
        self.s.exprs.push(expr);
    }

    /// `SubscriptList` after `[`, up to (not including) the `]`.
    #[inline(never)]
    fn parse_subscript_list(&mut self) -> PResult<ExprId> {
        let start = self.start();
        let first = self.parse_subscript()?;
        if !self.at(T::Comma) {
            return Ok(first);
        }
        let base = self.s.exprs.len();
        self.s.exprs.push(first);
        while self.at(T::Comma) {
            self.bump();
            if !(starts_test(self.tok()) || matches!(self.tok(), T::Star | T::Colon)) {
                break;
            }
            let elt = self.parse_subscript()?;
            self.s.exprs.push(elt);
        }
        let elts = self.finish_exprs(base);
        Ok(self.add_expr(
            range(start, self.prev_end),
            ExprKind::Tuple {
                elts,
                ctx: ExprContext::Load,
            },
        ))
    }

    /// `Subscript`: an element or a slice.
    fn parse_subscript(&mut self) -> PResult<ExprId> {
        let start = self.start();
        let lower = if self.at(T::Colon) {
            None
        } else {
            let (expr, kind) = self.parse_elem()?;
            if kind != Kind::Test || !self.at(T::Colon) {
                return Ok(expr);
            }
            Some(expr)
        };
        self.bump(); // `:`
        let upper = if starts_test(self.tok()) {
            Some(self.parse_test()?)
        } else {
            None
        };
        let step = if self.at(T::Colon) {
            self.bump();
            if starts_test(self.tok()) {
                Some(self.parse_test()?)
            } else {
                None
            }
        } else {
            None
        };
        Ok(self.add_expr(
            range(start, self.prev_end),
            ExprKind::Slice { lower, upper, step },
        ))
    }

    /// `CompFor`: one or more `[async] for ... in ... [if ...]*` clauses.
    #[inline(never)]
    pub(super) fn parse_comp_for(&mut self) -> PResult<List<Comprehension>> {
        let base = self.s.comprehensions.len();
        while matches!(self.tok(), T::For | T::Async) {
            let start = self.start();
            let is_async = self.at(T::Async);
            if is_async {
                self.bump();
            }
            self.expect(T::For)?;
            let (target, _) = self.parse_list(false)?;
            self.expect(T::In)?;
            let iter = self.parse_or_test()?;
            let ibase = self.s.exprs.len();
            while self.at(T::If) {
                self.bump();
                let cond = self.parse_or_test()?;
                self.s.exprs.push(cond);
            }
            let ifs = self.finish_exprs(ibase);
            self.set_context(target, ExprContext::Store);
            self.s.comprehensions.push(Comprehension {
                range: range(start, self.prev_end),
                target,
                iter,
                ifs,
                is_async,
            });
        }
        Ok(self.finish_comprehensions(base))
    }

    /// Set an assignment target's context (and its elements').
    pub(super) fn set_context(&mut self, expr: ExprId, ctx: ExprContext) {
        let mut stack = vec![expr];
        while let Some(expr) = stack.pop() {
            match &mut self.m.exprs[expr.index()].kind {
                ExprKind::Name { ctx: c, .. }
                | ExprKind::Attribute { ctx: c, .. }
                | ExprKind::Subscript { ctx: c, .. } => *c = ctx,
                ExprKind::Starred { ctx: c, value } => {
                    *c = ctx;
                    stack.push(*value);
                }
                ExprKind::Tuple { ctx: c, elts } | ExprKind::List { ctx: c, elts } => {
                    *c = ctx;
                    let elts = *elts;
                    stack.extend_from_slice(self.m.list(elts));
                }
                _ => {}
            }
        }
    }

    /// Intern a name the parser built.
    pub(super) fn intern_owned(&mut self, name: String) -> Sym {
        self.m.names.intern_owned(name)
    }
}
