//! Expressions, call arguments, comprehensions and parameter lists.

use super::{optional_range, range, Follow, PResult, Parser};
use crate::{
    ast::{self, Ranged},
    context::set_context,
    function::{parse_args, validate_arguments, validate_pos_params, ArgumentList},
    string::parse_strings,
    text_size::TextSize,
    token::Tok,
};

// Binary levels, loosest first (`Or` .. `Term`); unary operators, `**`,
// `await` and the postfix forms bind tighter and are parsed directly.
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
    Lambda(TextSize, Option<ast::Arguments>),
    IfExp(TextSize, ast::Expr, ast::Expr),
}

/// Apply unary operators (outermost first) around `operand`.
fn wrap_unary(
    ops: Vec<(ast::UnaryOp, TextSize)>,
    mut operand: ast::Expr,
    end: TextSize,
) -> ast::Expr {
    for (op, location) in ops.into_iter().rev() {
        operand = ast::Expr::UnaryOp(ast::ExprUnaryOp {
            op,
            operand: Box::new(operand),
            range: range(location, end),
        });
    }
    operand
}

/// The level of a binary operator token, and the operator.
#[inline]
fn binary_op(tok: &Tok) -> Option<(u8, ast::Operator)> {
    use ast::Operator as O;
    Some(match tok {
        Tok::Vbar => (5, O::BitOr),
        Tok::CircumFlex => (6, O::BitXor),
        Tok::Amper => (7, O::BitAnd),
        Tok::LeftShift => (8, O::LShift),
        Tok::RightShift => (8, O::RShift),
        Tok::Plus => (9, O::Add),
        Tok::Minus => (9, O::Sub),
        Tok::Star => (10, O::Mult),
        Tok::Slash => (10, O::Div),
        Tok::DoubleSlash => (10, O::FloorDiv),
        Tok::Percent => (10, O::Mod),
        Tok::At => (10, O::MatMult),
        _ => return None,
    })
}

#[inline]
fn is_comparison(tok: &Tok) -> bool {
    matches!(
        tok,
        Tok::EqEqual
            | Tok::NotEqual
            | Tok::Less
            | Tok::LessEqual
            | Tok::Greater
            | Tok::GreaterEqual
            | Tok::In
            | Tok::Not
            | Tok::Is
    )
}

/// Tokens that can start `Expression` (a bitwise-or level expression).
#[inline]
pub(super) fn starts_expression(tok: &Tok) -> bool {
    matches!(
        tok,
        Tok::Name { .. }
            | Tok::Int { .. }
            | Tok::Float { .. }
            | Tok::Complex { .. }
            | Tok::String { .. }
            | Tok::Lpar
            | Tok::Lsqb
            | Tok::Lbrace
            | Tok::True
            | Tok::False
            | Tok::None
            | Tok::Ellipsis
            | Tok::Minus
            | Tok::Plus
            | Tok::Tilde
            | Tok::Await
    )
}

/// Tokens that can start `Test`.
#[inline]
pub(super) fn starts_test(tok: &Tok) -> bool {
    starts_expression(tok) || matches!(tok, Tok::Not | Tok::Lambda)
}

impl Parser<'_> {
    /// `Test<"all">`: a lambda, a conditional expression or an `or` test.
    pub(super) fn parse_test(&mut self) -> PResult<ast::Expr> {
        self.enter()?;
        let result = if self.at(Tok::Lambda) {
            self.parse_test_chain(None)
        } else {
            let start = self.start();
            match self.parse_binary(L_OR) {
                Ok(body) if self.at(Tok::If) => self.parse_test_chain(Some((start, body))),
                result => result,
            }
        };
        self.leave();
        result
    }

    /// A lambda, or a conditional expression (`first` is its body, parsed,
    /// with the `if` current). A lambda's body and a conditional
    /// expression's `else` branch are `Test`s again: those right-nested
    /// chains are read iteratively and folded afterwards (innermost first,
    /// as the grammar reduces them).
    #[inline(never)]
    fn parse_test_chain(&mut self, mut first: Option<(TextSize, ast::Expr)>) -> PResult<ast::Expr> {
        let mut pending = Vec::new();
        let mut expr = loop {
            let (start, body) = match first.take() {
                Some(first) => first,
                None => {
                    let start = self.start();
                    if self.at(Tok::Lambda) {
                        self.bump();
                        let params = if self.at(Tok::Colon) {
                            None
                        } else {
                            Some(self.parse_parameter_list(false)?)
                        };
                        self.expect(Tok::Colon)?;
                        pending.push(TestHead::Lambda(start, params));
                        continue;
                    }
                    (start, self.parse_binary(L_OR)?)
                }
            };
            if !self.at(Tok::If) {
                break body;
            }
            self.bump();
            let test = self.parse_binary(L_OR)?;
            self.expect(Tok::Else)?;
            pending.push(TestHead::IfExp(start, body, test));
        };
        let end = self.prev_end;
        while let Some(head) = pending.pop() {
            expr = match head {
                TestHead::Lambda(start, params) => {
                    if let Some(params) = &params {
                        validate_arguments(params)
                            .map_err(|error| self.reduce_error(error, Follow::Test))?;
                    }
                    let args =
                        params.unwrap_or_else(|| ast::Arguments::empty(optional_range(start, end)));
                    ast::Expr::Lambda(ast::ExprLambda {
                        args: Box::new(args),
                        body: Box::new(expr),
                        range: range(start, end),
                    })
                }
                TestHead::IfExp(start, body, test) => ast::Expr::IfExp(ast::ExprIfExp {
                    test: Box::new(test),
                    body: Box::new(body),
                    orelse: Box::new(expr),
                    range: range(start, end),
                }),
            };
        }
        Ok(expr)
    }

    /// `OrTest<"all">`.
    #[inline]
    pub(super) fn parse_or_test(&mut self) -> PResult<ast::Expr> {
        self.parse_binary(L_OR)
    }

    /// `Expression<"all">`: a bitwise-or level expression.
    #[inline]
    pub(super) fn parse_expression(&mut self) -> PResult<ast::Expr> {
        self.parse_binary(L_BOR)
    }

    /// Every binary level from `or` down to `*`, by precedence climbing.
    fn parse_binary(&mut self, min: u8) -> PResult<ast::Expr> {
        let start = self.start();
        let mut left = if min <= L_NOT && self.at(Tok::Not) {
            self.parse_not()?
        } else {
            self.parse_factor()?
        };
        loop {
            if let Some((level, op)) = binary_op(&self.tok) {
                if level < min {
                    break;
                }
                self.bump();
                let right = self.parse_binary(level + 1)?;
                left = ast::Expr::BinOp(ast::ExprBinOp {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                    range: range(start, self.prev_end),
                });
                continue;
            }
            if is_comparison(&self.tok) {
                if L_CMP < min {
                    break;
                }
                left = self.parse_comparison(start, left)?;
                continue;
            }
            let (level, op, tok) = match self.tok {
                Tok::And => (L_AND, ast::BoolOp::And, Tok::And),
                Tok::Or => (L_OR, ast::BoolOp::Or, Tok::Or),
                _ => break,
            };
            if level < min {
                break;
            }
            left = self.parse_bool_op(start, left, op, tok, level)?;
        }
        Ok(left)
    }

    /// `not` chains, folded iteratively.
    #[inline(never)]
    fn parse_not(&mut self) -> PResult<ast::Expr> {
        let mut nots = Vec::new();
        while self.at(Tok::Not) {
            nots.push(self.start());
            self.bump();
        }
        let mut operand = self.parse_binary(L_CMP)?;
        for location in nots.into_iter().rev() {
            operand = ast::Expr::UnaryOp(ast::ExprUnaryOp {
                op: ast::UnaryOp::Not,
                operand: Box::new(operand),
                range: range(location, self.prev_end),
            });
        }
        Ok(operand)
    }

    /// A comparison chain after its first operand.
    #[inline(never)]
    fn parse_comparison(&mut self, start: TextSize, left: ast::Expr) -> PResult<ast::Expr> {
        let mut ops = Vec::new();
        let mut comparators = Vec::new();
        while is_comparison(&self.tok) {
            let op = match self.bump() {
                Tok::EqEqual => ast::CmpOp::Eq,
                Tok::NotEqual => ast::CmpOp::NotEq,
                Tok::Less => ast::CmpOp::Lt,
                Tok::LessEqual => ast::CmpOp::LtE,
                Tok::Greater => ast::CmpOp::Gt,
                Tok::GreaterEqual => ast::CmpOp::GtE,
                Tok::In => ast::CmpOp::In,
                Tok::Not => {
                    self.expect(Tok::In)?;
                    ast::CmpOp::NotIn
                }
                _ => {
                    if self.at(Tok::Not) {
                        self.bump();
                        ast::CmpOp::IsNot
                    } else {
                        ast::CmpOp::Is
                    }
                }
            };
            ops.push(op);
            comparators.push(self.parse_binary(L_BOR)?);
        }
        Ok(ast::Expr::Compare(ast::ExprCompare {
            left: Box::new(left),
            ops,
            comparators,
            range: range(start, self.prev_end),
        }))
    }

    /// An `and` / `or` chain after its first operand.
    #[inline(never)]
    fn parse_bool_op(
        &mut self,
        start: TextSize,
        left: ast::Expr,
        op: ast::BoolOp,
        tok: Tok,
        level: u8,
    ) -> PResult<ast::Expr> {
        let mut values = vec![left];
        while self.tok == tok {
            self.bump();
            values.push(self.parse_binary(level + 1)?);
        }
        Ok(ast::Expr::BoolOp(ast::ExprBoolOp {
            op,
            values,
            range: range(start, self.prev_end),
        }))
    }

    /// `Factor`: unary `+ - ~`, then `Power`: `AtomExpr ("**" Factor)?`.
    /// Both chains (`- - x`, `a ** b ** c`) nest to the right; they are read
    /// iteratively and folded afterwards, so their length costs no stack.
    fn parse_factor(&mut self) -> PResult<ast::Expr> {
        if !matches!(self.tok, Tok::Plus | Tok::Minus | Tok::Tilde) {
            let start = self.start();
            let base = self.parse_await_expr()?;
            if !self.at(Tok::DoubleStar) {
                return Ok(base);
            }
            return self.parse_factor_chain(Vec::new(), start, base);
        }
        let ops = self.parse_unary_ops();
        let start = self.start();
        let base = self.parse_await_expr()?;
        if !self.at(Tok::DoubleStar) {
            return Ok(wrap_unary(ops, base, self.prev_end));
        }
        self.parse_factor_chain(ops, start, base)
    }

    /// The rest of a `**` chain, then the fold.
    #[inline(never)]
    fn parse_factor_chain(
        &mut self,
        ops: Vec<(ast::UnaryOp, TextSize)>,
        start: TextSize,
        base: ast::Expr,
    ) -> PResult<ast::Expr> {
        let mut chain = vec![(ops, start, base)];
        while self.at(Tok::DoubleStar) {
            self.bump();
            let ops = self.parse_unary_ops();
            let start = self.start();
            let base = self.parse_await_expr()?;
            chain.push((ops, start, base));
        }
        let end = self.prev_end;
        let (ops, _, base) = chain.pop().unwrap();
        let mut factor = wrap_unary(ops, base, end);
        while let Some((ops, start, base)) = chain.pop() {
            let power = ast::Expr::BinOp(ast::ExprBinOp {
                left: Box::new(base),
                op: ast::Operator::Pow,
                right: Box::new(factor),
                range: range(start, end),
            });
            factor = wrap_unary(ops, power, end);
        }
        Ok(factor)
    }

    /// Leading unary operators and where each starts.
    fn parse_unary_ops(&mut self) -> Vec<(ast::UnaryOp, TextSize)> {
        let mut ops = Vec::new();
        loop {
            let op = match self.tok {
                Tok::Plus => ast::UnaryOp::UAdd,
                Tok::Minus => ast::UnaryOp::USub,
                Tok::Tilde => ast::UnaryOp::Invert,
                _ => return ops,
            };
            ops.push((op, self.start()));
            self.bump();
        }
    }

    /// `AtomExpr`: an optional `await` before the postfix expression.
    fn parse_await_expr(&mut self) -> PResult<ast::Expr> {
        if !self.at(Tok::Await) {
            return self.parse_atom_expr();
        }
        let start = self.start();
        self.bump();
        let value = self.parse_atom_expr()?;
        Ok(ast::Expr::Await(ast::ExprAwait {
            value: Box::new(value),
            range: range(start, self.prev_end),
        }))
    }

    /// `AtomExpr2`: an atom followed by calls, subscripts and attributes.
    fn parse_atom_expr(&mut self) -> PResult<ast::Expr> {
        let start = self.start();
        let mut expr = self.parse_atom()?;
        loop {
            match self.tok {
                Tok::Lpar => {
                    self.bump();
                    let ArgumentList { args, keywords } = self.parse_call_args()?;
                    self.bump(); // `)`
                    expr = ast::Expr::Call(ast::ExprCall {
                        func: Box::new(expr),
                        args,
                        keywords,
                        range: range(start, self.prev_end),
                    });
                }
                Tok::Lsqb => {
                    self.bump();
                    let slice = self.parse_subscript_list()?;
                    self.expect(Tok::Rsqb)?;
                    expr = ast::Expr::Subscript(ast::ExprSubscript {
                        value: Box::new(expr),
                        slice: Box::new(slice),
                        ctx: ast::ExprContext::Load,
                        range: range(start, self.prev_end),
                    });
                }
                Tok::Dot => {
                    self.bump();
                    let attr = self.identifier()?;
                    expr = ast::Expr::Attribute(ast::ExprAttribute {
                        value: Box::new(expr),
                        attr,
                        ctx: ast::ExprContext::Load,
                        range: range(start, self.prev_end),
                    });
                }
                _ => return Ok(expr),
            }
        }
    }

    fn parse_atom(&mut self) -> PResult<ast::Expr> {
        let start = self.start();
        let follow = if self.index == self.with_item_start {
            Follow::WithItemAtom
        } else {
            Follow::Atom
        };
        let constant = |value: ast::Constant, end: TextSize| {
            Ok(ast::Expr::Constant(ast::ExprConstant {
                value,
                kind: None,
                range: range(start, end),
            }))
        };
        match self.tok {
            Tok::Name { .. } => {
                let id = self.identifier()?;
                Ok(ast::Expr::Name(ast::ExprName {
                    id,
                    ctx: ast::ExprContext::Load,
                    range: range(start, self.prev_end),
                }))
            }
            Tok::Int { .. } | Tok::Float { .. } | Tok::Complex { .. } => {
                let value = self.number();
                constant(value, self.prev_end)
            }
            Tok::String { .. } => self.parse_strings(follow),
            Tok::True => {
                self.bump();
                constant(true.into(), self.prev_end)
            }
            Tok::False => {
                self.bump();
                constant(false.into(), self.prev_end)
            }
            Tok::None => {
                self.bump();
                constant(ast::Constant::None, self.prev_end)
            }
            Tok::Ellipsis => {
                self.bump();
                constant(ast::Constant::Ellipsis, self.prev_end)
            }
            Tok::Lpar => self.parse_paren(start, follow),
            Tok::Lsqb => self.parse_list_display(start),
            Tok::Lbrace => self.parse_brace(start),
            _ => Err(self.unexpected()),
        }
    }

    /// Consume an int, float or complex token.
    pub(super) fn number(&mut self) -> ast::Constant {
        match self.bump() {
            Tok::Int { value } => ast::Constant::Int(value),
            Tok::Float { value } => ast::Constant::Float(value),
            Tok::Complex { real, imag } => ast::Constant::Complex { real, imag },
            _ => unreachable!("number() is only called at a number token"),
        }
    }

    /// One or more adjacent string tokens.
    #[inline(never)]
    pub(super) fn parse_strings(&mut self, follow: Follow) -> PResult<ast::Expr> {
        let mut values = Vec::new();
        while matches!(self.tok, Tok::String { .. }) {
            let token_range = self.range;
            if let Tok::String {
                value,
                kind,
                triple_quoted,
            } = self.bump()
            {
                values.push((
                    token_range.start(),
                    (value, kind, triple_quoted),
                    token_range.end(),
                ));
            }
        }
        parse_strings(values).map_err(|error| self.reduce_error(error, follow))
    }

    /// After `(`: a tuple, a parenthesized expression, a generator
    /// expression or a parenthesized `yield`.
    #[inline(never)]
    fn parse_paren(&mut self, start: TextSize, follow: Follow) -> PResult<ast::Expr> {
        self.bump();
        match self.tok {
            Tok::Rpar => {
                self.bump();
                return Ok(ast::Expr::Tuple(ast::ExprTuple {
                    elts: Vec::new(),
                    ctx: ast::ExprContext::Load,
                    range: range(start, self.prev_end),
                }));
            }
            Tok::Yield => {
                let expr = self.parse_yield()?;
                self.expect(Tok::Rpar)?;
                return Ok(expr);
            }
            Tok::DoubleStar => {
                let location = self.start();
                self.bump();
                self.parse_expression()?;
                self.expect(Tok::Rpar)?;
                return Err(self.reduce_error_other(
                    "cannot use double starred expression here",
                    location,
                    follow,
                ));
            }
            _ => {}
        }
        let (first, kind) = self.parse_elem()?;
        if kind != Kind::Star && matches!(self.tok, Tok::For | Tok::Async) {
            let generators = self.parse_comp_for()?;
            self.expect(Tok::Rpar)?;
            return Ok(ast::Expr::GeneratorExp(ast::ExprGeneratorExp {
                elt: Box::new(first),
                generators,
                range: range(start, self.prev_end),
            }));
        }
        if self.at(Tok::Rpar) {
            self.bump();
            if kind == Kind::Star {
                return Err(self.reduce_error_other(
                    "cannot use starred expression here",
                    first.start(),
                    follow,
                ));
            }
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.at(Tok::Comma) {
            self.bump();
            if self.at(Tok::Rpar) {
                break;
            }
            elts.push(self.parse_elem()?.0);
        }
        self.expect(Tok::Rpar)?;
        Ok(ast::Expr::Tuple(ast::ExprTuple {
            elts,
            ctx: ast::ExprContext::Load,
            range: range(start, self.prev_end),
        }))
    }

    /// After `[`: a list display or a list comprehension.
    #[inline(never)]
    fn parse_list_display(&mut self, start: TextSize) -> PResult<ast::Expr> {
        self.bump();
        if self.at(Tok::Rsqb) {
            self.bump();
            return Ok(ast::Expr::List(ast::ExprList {
                elts: Vec::new(),
                ctx: ast::ExprContext::Load,
                range: range(start, self.prev_end),
            }));
        }
        let (first, _) = self.parse_elem()?;
        if matches!(self.tok, Tok::For | Tok::Async) {
            let generators = self.parse_comp_for()?;
            self.expect(Tok::Rsqb)?;
            return Ok(ast::Expr::ListComp(ast::ExprListComp {
                elt: Box::new(first),
                generators,
                range: range(start, self.prev_end),
            }));
        }
        let mut elts = vec![first];
        while self.at(Tok::Comma) {
            self.bump();
            if self.at(Tok::Rsqb) {
                break;
            }
            elts.push(self.parse_elem()?.0);
        }
        self.expect(Tok::Rsqb)?;
        Ok(ast::Expr::List(ast::ExprList {
            elts,
            ctx: ast::ExprContext::Load,
            range: range(start, self.prev_end),
        }))
    }

    /// After `{`: a dict or set display or comprehension.
    #[inline(never)]
    fn parse_brace(&mut self, start: TextSize) -> PResult<ast::Expr> {
        self.bump();
        let mut keys = Vec::new();
        let mut values = Vec::new();
        match self.tok {
            Tok::Rbrace => {
                self.bump();
                return Ok(ast::Expr::Dict(ast::ExprDict {
                    keys,
                    values,
                    range: range(start, self.prev_end),
                }));
            }
            Tok::DoubleStar => {
                self.bump();
                keys.push(None);
                values.push(self.parse_expression()?);
            }
            _ => {
                let (first, kind) = self.parse_elem()?;
                if kind == Kind::Test && self.at(Tok::Colon) {
                    self.bump();
                    let value = self.parse_test()?;
                    if matches!(self.tok, Tok::For | Tok::Async) {
                        let generators = self.parse_comp_for()?;
                        self.expect(Tok::Rbrace)?;
                        return Ok(ast::Expr::DictComp(ast::ExprDictComp {
                            key: Box::new(first),
                            value: Box::new(value),
                            generators,
                            range: range(start, self.prev_end),
                        }));
                    }
                    keys.push(Some(first));
                    values.push(value);
                } else {
                    return self.parse_set(start, first, kind);
                }
            }
        }
        while self.at(Tok::Comma) {
            self.bump();
            if self.at(Tok::Rbrace) {
                break;
            }
            if self.at(Tok::DoubleStar) {
                self.bump();
                keys.push(None);
                values.push(self.parse_expression()?);
            } else {
                let key = self.parse_test()?;
                self.expect(Tok::Colon)?;
                keys.push(Some(key));
                values.push(self.parse_test()?);
            }
        }
        self.expect(Tok::Rbrace)?;
        Ok(ast::Expr::Dict(ast::ExprDict {
            keys,
            values,
            range: range(start, self.prev_end),
        }))
    }

    #[inline(never)]
    fn parse_set(&mut self, start: TextSize, first: ast::Expr, kind: Kind) -> PResult<ast::Expr> {
        if kind != Kind::Star && matches!(self.tok, Tok::For | Tok::Async) {
            let generators = self.parse_comp_for()?;
            self.expect(Tok::Rbrace)?;
            return Ok(ast::Expr::SetComp(ast::ExprSetComp {
                elt: Box::new(first),
                generators,
                range: range(start, self.prev_end),
            }));
        }
        let mut elts = vec![first];
        while self.at(Tok::Comma) {
            self.bump();
            if self.at(Tok::Rbrace) {
                break;
            }
            elts.push(self.parse_elem()?.0);
        }
        self.expect(Tok::Rbrace)?;
        Ok(ast::Expr::Set(ast::ExprSet {
            elts,
            range: range(start, self.prev_end),
        }))
    }

    /// `NamedExpressionTest`: `name := Test`, or a `Test`.
    pub(super) fn parse_named_test(&mut self) -> PResult<(ast::Expr, Kind)> {
        if matches!(self.tok, Tok::Name { .. }) && matches!(self.peek(1), Some(Tok::ColonEqual)) {
            let start = self.start();
            let id = self.identifier()?;
            let target_range = range(start, self.prev_end);
            self.bump(); // `:=`
            let value = self.parse_test()?;
            let end = value.end();
            return Ok((
                ast::Expr::NamedExpr(ast::ExprNamedExpr {
                    target: Box::new(ast::Expr::Name(ast::ExprName {
                        id,
                        ctx: ast::ExprContext::Store,
                        range: target_range,
                    })),
                    range: range(start, end),
                    value: Box::new(value),
                }),
                Kind::Named,
            ));
        }
        Ok((self.parse_test()?, Kind::Test))
    }

    /// `StarExpr`: `*` Expression.
    pub(super) fn parse_star_expr(&mut self) -> PResult<ast::Expr> {
        let start = self.start();
        self.bump();
        let value = self.parse_expression()?;
        Ok(ast::Expr::Starred(ast::ExprStarred {
            value: Box::new(value),
            ctx: ast::ExprContext::Load,
            range: range(start, self.prev_end),
        }))
    }

    /// `TestOrStarNamedExpr`.
    pub(super) fn parse_elem(&mut self) -> PResult<(ast::Expr, Kind)> {
        if self.at(Tok::Star) {
            return Ok((self.parse_star_expr()?, Kind::Star));
        }
        self.parse_named_test()
    }

    /// `GenericList<TestOrStarExpr>` (`test_level`, the `TestList`) or
    /// `GenericList<ExpressionOrStarExpression>` (the `ExpressionList`):
    /// one element, or a tuple of them with an optional trailing comma. The
    /// flag says whether the result is a single `Test` element.
    pub(super) fn parse_list(&mut self, test_level: bool) -> PResult<(ast::Expr, bool)> {
        let start = self.start();
        let (first, single_test) = self.parse_list_elem(test_level)?;
        if !self.at(Tok::Comma) {
            return Ok((first, single_test));
        }
        let mut elts = vec![first];
        while self.at(Tok::Comma) {
            self.bump();
            let more = if test_level {
                starts_test(&self.tok)
            } else {
                starts_expression(&self.tok)
            };
            if !more && !self.at(Tok::Star) {
                break;
            }
            elts.push(self.parse_list_elem(test_level)?.0);
        }
        Ok((
            ast::Expr::Tuple(ast::ExprTuple {
                elts,
                ctx: ast::ExprContext::Load,
                range: range(start, self.prev_end),
            }),
            false,
        ))
    }

    fn parse_list_elem(&mut self, test_level: bool) -> PResult<(ast::Expr, bool)> {
        if self.at(Tok::Star) {
            Ok((self.parse_star_expr()?, false))
        } else if test_level {
            Ok((self.parse_test()?, true))
        } else {
            Ok((self.parse_expression()?, false))
        }
    }

    /// `TestListOrYieldExpr`.
    pub(super) fn parse_list_or_yield(&mut self) -> PResult<ast::Expr> {
        if self.at(Tok::Yield) {
            self.parse_yield()
        } else {
            Ok(self.parse_list(true)?.0)
        }
    }

    /// `YieldExpr`: `yield [TestList]` or `yield from Test`.
    pub(super) fn parse_yield(&mut self) -> PResult<ast::Expr> {
        let start = self.start();
        self.bump();
        if self.at(Tok::From) {
            self.bump();
            let value = self.parse_test()?;
            return Ok(ast::Expr::YieldFrom(ast::ExprYieldFrom {
                value: Box::new(value),
                range: range(start, self.prev_end),
            }));
        }
        let value = if starts_test(&self.tok) || self.at(Tok::Star) {
            Some(Box::new(self.parse_list(true)?.0))
        } else {
            None
        };
        Ok(ast::Expr::Yield(ast::ExprYield {
            value,
            range: range(start, self.prev_end),
        }))
    }

    /// `ParameterList`, for a `def` (`typed`, closed by `)`) or a lambda
    /// (closed by `:`). The current token is not the closing one.
    pub(super) fn parse_parameter_list(&mut self, typed: bool) -> PResult<ast::Arguments> {
        let close = if typed { Tok::Rpar } else { Tok::Colon };
        let start = self.start();
        let mut posonlyargs = Vec::new();
        let mut args = Vec::new();
        let mut vararg = None;
        let mut kwonlyargs = Vec::new();
        let mut kwarg = None;
        let has_defs = matches!(self.tok, Tok::Name { .. });
        let mut bare_star = None;
        // Whether `*` or `**` may come next.
        let mut rest_allowed = !has_defs;
        if has_defs {
            args.push(self.parse_parameter_def(typed)?);
            let mut slash = false;
            while self.at(Tok::Comma) {
                match self.peek(1) {
                    Some(Tok::Name { .. }) => {
                        self.bump();
                        args.push(self.parse_parameter_def(typed)?);
                    }
                    Some(Tok::Slash) if !slash => {
                        self.bump();
                        self.bump();
                        slash = true;
                        posonlyargs = std::mem::take(&mut args);
                    }
                    _ => break,
                }
            }
            if self.at(Tok::Comma) && matches!(self.peek(1), Some(Tok::Star | Tok::DoubleStar)) {
                self.bump();
                rest_allowed = true;
            }
        }
        if rest_allowed && self.at(Tok::Star) {
            let star_location = self.start();
            self.bump();
            if matches!(self.tok, Tok::Name { .. }) {
                vararg = Some(Box::new(self.parse_star_parameter(typed)?));
            }
            let mut has_kwarg = false;
            while self.at(Tok::Comma) {
                match self.peek(1) {
                    Some(Tok::Name { .. }) => {
                        self.bump();
                        kwonlyargs.push(self.parse_parameter_def(typed)?);
                    }
                    Some(Tok::DoubleStar) => {
                        self.bump();
                        kwarg = self.parse_kwarg_parameter(typed)?;
                        has_kwarg = true;
                        break;
                    }
                    _ => break,
                }
            }
            if vararg.is_none() && kwonlyargs.is_empty() && !has_kwarg {
                bare_star = Some(star_location);
            }
        } else if rest_allowed && self.at(Tok::DoubleStar) {
            kwarg = self.parse_kwarg_parameter(typed)?;
        } else if !has_defs {
            return Err(self.unexpected());
        }
        if self.at(Tok::Comma) {
            self.bump();
        }
        if self.tok != close {
            return Err(self.unexpected());
        }
        // The grammar checks these when it reduces the list, at the closing
        // token; the bare `*` check is the inlined inner rule, so it is first.
        // (At the closing token, which is always in the lookahead set.)
        if let Some(location) = bare_star {
            return Err(self.reduce_error_other(
                "named arguments must follow bare *",
                location,
                Follow::Checked,
            ));
        }
        if has_defs {
            let defs = (posonlyargs, args);
            validate_pos_params(&defs)
                .map_err(|error| self.reduce_error(error, Follow::Checked))?;
            (posonlyargs, args) = defs;
        }
        Ok(ast::Arguments {
            posonlyargs,
            args,
            kwonlyargs,
            vararg,
            kwarg,
            range: optional_range(start, self.prev_end),
        })
    }

    /// `ParameterDef`: a parameter with an optional default.
    fn parse_parameter_def(&mut self, typed: bool) -> PResult<ast::ArgWithDefault> {
        let start = self.start();
        let arg = self.identifier()?;
        let annotation = if typed && self.at(Tok::Colon) {
            self.bump();
            Some(Box::new(self.parse_test()?))
        } else {
            None
        };
        let end = self.prev_end;
        let mut parameter = ast::ArgWithDefault {
            def: ast::Arg {
                arg,
                annotation,
                type_comment: None,
                range: range(start, end),
            },
            default: None,
            range: optional_range(start, end),
        };
        if self.at(Tok::Equal) {
            self.bump();
            parameter.default = Some(Box::new(self.parse_test()?));
        }
        Ok(parameter)
    }

    /// The name after `*`, with an annotation that may be starred.
    fn parse_star_parameter(&mut self, typed: bool) -> PResult<ast::Arg> {
        let start = self.start();
        let arg = self.identifier()?;
        let annotation = if typed && self.at(Tok::Colon) {
            self.bump();
            Some(Box::new(if self.at(Tok::Star) {
                self.parse_star_expr()?
            } else {
                self.parse_test()?
            }))
        } else {
            None
        };
        Ok(ast::Arg {
            arg,
            annotation,
            type_comment: None,
            range: range(start, self.prev_end),
        })
    }

    /// `KwargParameter`: `**` and an optional name.
    fn parse_kwarg_parameter(&mut self, typed: bool) -> PResult<Option<Box<ast::Arg>>> {
        self.bump(); // `**`
        if !matches!(self.tok, Tok::Name { .. }) {
            return Ok(None);
        }
        let start = self.start();
        let arg = self.identifier()?;
        let annotation = if typed && self.at(Tok::Colon) {
            self.bump();
            Some(Box::new(self.parse_test()?))
        } else {
            None
        };
        Ok(Some(Box::new(ast::Arg {
            arg,
            annotation,
            type_comment: None,
            range: range(start, self.prev_end),
        })))
    }

    /// `ArgumentList` after `(`, up to (not including) the `)`.
    #[inline(never)]
    pub(super) fn parse_call_args(&mut self) -> PResult<ArgumentList> {
        let mut func_args = Vec::new();
        while !self.at(Tok::Rpar) {
            let start = self.start();
            let keyword =
                matches!(self.tok, Tok::Name { .. }) && matches!(self.peek(1), Some(Tok::Equal));
            let arg = match self.tok {
                Tok::Star => {
                    self.bump();
                    let value = self.parse_test()?;
                    let expr = ast::Expr::Starred(ast::ExprStarred {
                        value: Box::new(value),
                        ctx: ast::ExprContext::Load,
                        range: range(start, self.prev_end),
                    });
                    (None, expr)
                }
                Tok::DoubleStar => {
                    self.bump();
                    let value = self.parse_test()?;
                    (Some((start, self.prev_end, None)), value)
                }
                Tok::Name { .. } if keyword => {
                    let name = self.identifier()?;
                    self.bump(); // `=`
                    let value = self.parse_test()?;
                    (Some((start, self.prev_end, Some(name))), value)
                }
                _ => {
                    let (elt, _) = self.parse_named_test()?;
                    if matches!(self.tok, Tok::For | Tok::Async) {
                        let generators = self.parse_comp_for()?;
                        let expr = ast::Expr::GeneratorExp(ast::ExprGeneratorExp {
                            elt: Box::new(elt),
                            generators,
                            range: range(start, self.prev_end),
                        });
                        (None, expr)
                    } else {
                        (None, elt)
                    }
                }
            };
            func_args.push(arg);
            if self.at(Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        if !self.at(Tok::Rpar) {
            return Err(self.unexpected());
        }
        // (At the `)`, always in the lookahead set.)
        parse_args(func_args).map_err(|error| self.reduce_error(error, Follow::Checked))
    }

    /// `SubscriptList` after `[`, up to (not including) the `]`.
    #[inline(never)]
    fn parse_subscript_list(&mut self) -> PResult<ast::Expr> {
        let start = self.start();
        let first = self.parse_subscript()?;
        if !self.at(Tok::Comma) {
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.at(Tok::Comma) {
            self.bump();
            if !(starts_test(&self.tok) || matches!(self.tok, Tok::Star | Tok::Colon)) {
                break;
            }
            elts.push(self.parse_subscript()?);
        }
        Ok(ast::Expr::Tuple(ast::ExprTuple {
            elts,
            ctx: ast::ExprContext::Load,
            range: range(start, self.prev_end),
        }))
    }

    /// `Subscript`: an element or a slice.
    fn parse_subscript(&mut self) -> PResult<ast::Expr> {
        let start = self.start();
        let lower = if self.at(Tok::Colon) {
            None
        } else {
            let (expr, kind) = self.parse_elem()?;
            if kind != Kind::Test || !self.at(Tok::Colon) {
                return Ok(expr);
            }
            Some(Box::new(expr))
        };
        self.bump(); // `:`
        let upper = if starts_test(&self.tok) {
            Some(Box::new(self.parse_test()?))
        } else {
            None
        };
        let step = if self.at(Tok::Colon) {
            self.bump();
            if starts_test(&self.tok) {
                Some(Box::new(self.parse_test()?))
            } else {
                None
            }
        } else {
            None
        };
        Ok(ast::Expr::Slice(ast::ExprSlice {
            lower,
            upper,
            step,
            range: range(start, self.prev_end),
        }))
    }

    /// `CompFor`: one or more `[async] for ... in ... [if ...]*` clauses.
    #[inline(never)]
    pub(super) fn parse_comp_for(&mut self) -> PResult<Vec<ast::Comprehension>> {
        let mut generators = Vec::new();
        while matches!(self.tok, Tok::For | Tok::Async) {
            let start = self.start();
            let is_async = self.at(Tok::Async);
            if is_async {
                self.bump();
            }
            self.expect(Tok::For)?;
            let (target, _) = self.parse_list(false)?;
            self.expect(Tok::In)?;
            let iter = self.parse_or_test()?;
            let mut ifs = Vec::new();
            while self.at(Tok::If) {
                self.bump();
                ifs.push(self.parse_or_test()?);
            }
            generators.push(ast::Comprehension {
                target: set_context(target, ast::ExprContext::Store),
                iter,
                ifs,
                is_async,
                range: optional_range(start, self.prev_end),
            });
        }
        Ok(generators)
    }
}
