//! `match` statements and their patterns.

use super::{body_end, expr::starts_test, optional_range, range, Follow, PResult, Parser};
use crate::{ast, text_size::TextSize, token::Tok};

/// Tokens that can start a pattern.
fn starts_pattern(tok: &Tok) -> bool {
    matches!(
        tok,
        Tok::Name { .. }
            | Tok::Int { .. }
            | Tok::Float { .. }
            | Tok::Complex { .. }
            | Tok::String { .. }
            | Tok::Minus
            | Tok::None
            | Tok::True
            | Tok::False
            | Tok::Lpar
            | Tok::Lsqb
            | Tok::Lbrace
            | Tok::Star
    )
}

impl Parser<'_> {
    pub(super) fn parse_match(&mut self) -> PResult<ast::Stmt> {
        let start = self.start();
        self.bump(); // `match`
        let mut subjects = vec![self.parse_elem()?.0];
        if self.at(Tok::Comma) {
            self.bump();
            while starts_test(&self.tok) || self.at(Tok::Star) {
                subjects.push(self.parse_elem()?.0);
                if !self.at(Tok::Comma) {
                    break;
                }
                self.bump();
            }
        }
        self.expect(Tok::Colon)?;
        self.expect(Tok::Newline)?;
        if !self.at(Tok::Indent) {
            return Err(self.unexpected_expecting(Some("Indent")));
        }
        self.bump();
        let mut cases = Vec::new();
        loop {
            if !self.at(Tok::Case) {
                return Err(self.unexpected());
            }
            cases.push(self.parse_match_case()?);
            if self.at(Tok::Dedent) {
                self.bump();
                break;
            }
        }
        let end = cases
            .last()
            .map_or(TextSize::default(), |case| body_end(&case.body));
        // `match x, :` has the single subject `x`.
        let subject = if subjects.len() == 1 {
            subjects.pop().unwrap()
        } else {
            ast::Expr::Tuple(ast::ExprTuple {
                elts: subjects,
                ctx: ast::ExprContext::Load,
                range: range(start, end),
            })
        };
        Ok(ast::Stmt::Match(ast::StmtMatch {
            subject: Box::new(subject),
            cases,
            range: range(start, end),
        }))
    }

    fn parse_match_case(&mut self) -> PResult<ast::MatchCase> {
        let start = self.start();
        self.bump(); // `case`
        let pattern = self.parse_patterns()?;
        let guard = if self.at(Tok::If) {
            self.bump();
            let (guard, _) = self.parse_named_test()?;
            Some(Box::new(guard))
        } else {
            None
        };
        self.expect(Tok::Colon)?;
        let body = self.parse_suite()?;
        Ok(ast::MatchCase {
            pattern,
            guard,
            range: optional_range(start, body_end(&body)),
            body,
        })
    }

    /// `Patterns`: a pattern, or an open sequence of them.
    fn parse_patterns(&mut self) -> PResult<ast::Pattern> {
        let start = self.start();
        let first = self.parse_pattern()?;
        if !self.at(Tok::Comma) {
            return Ok(first);
        }
        let mut patterns = vec![first];
        while self.at(Tok::Comma) {
            self.bump();
            if !starts_pattern(&self.tok) {
                break;
            }
            patterns.push(self.parse_pattern()?);
        }
        Ok(ast::Pattern::MatchSequence(ast::PatternMatchSequence {
            patterns,
            range: range(start, self.prev_end),
        }))
    }

    /// `Pattern`: an or-pattern with an optional `as name`.
    fn parse_pattern(&mut self) -> PResult<ast::Pattern> {
        self.enter()?;
        let result = self.parse_pattern_inner();
        self.leave();
        result
    }

    fn parse_pattern_inner(&mut self) -> PResult<ast::Pattern> {
        let start = self.start();
        let first = self.parse_closed_pattern()?;
        let pattern = if self.at(Tok::Vbar) {
            let mut patterns = vec![first];
            while self.at(Tok::Vbar) {
                self.bump();
                patterns.push(self.parse_closed_pattern()?);
            }
            ast::Pattern::MatchOr(ast::PatternMatchOr {
                patterns,
                range: range(start, self.prev_end),
            })
        } else {
            first
        };
        if !self.at(Tok::As) {
            return Ok(pattern);
        }
        self.bump();
        let name = self.identifier()?;
        if name.as_str() == "_" {
            return Err(self.reduce_error_other(
                "cannot use '_' as a target",
                start,
                Follow::Pattern,
            ));
        }
        Ok(ast::Pattern::MatchAs(ast::PatternMatchAs {
            pattern: Some(Box::new(pattern)),
            name: Some(name),
            range: range(start, self.prev_end),
        }))
    }

    fn parse_closed_pattern(&mut self) -> PResult<ast::Pattern> {
        let start = self.start();
        let singleton = |value: ast::Constant, end: TextSize| {
            Ok(ast::Pattern::MatchSingleton(ast::PatternMatchSingleton {
                value,
                range: range(start, end),
            }))
        };
        match self.tok {
            Tok::None => {
                self.bump();
                singleton(ast::Constant::None, self.prev_end)
            }
            Tok::True => {
                self.bump();
                singleton(true.into(), self.prev_end)
            }
            Tok::False => {
                self.bump();
                singleton(false.into(), self.prev_end)
            }
            Tok::Int { .. } | Tok::Float { .. } | Tok::Complex { .. } | Tok::Minus => {
                let value = self.parse_signed_number()?;
                Ok(ast::Pattern::MatchValue(ast::PatternMatchValue {
                    value: Box::new(value),
                    range: range(start, self.prev_end),
                }))
            }
            Tok::String { .. } => {
                let value = self.parse_strings(Follow::ClosedPattern)?;
                Ok(ast::Pattern::MatchValue(ast::PatternMatchValue {
                    value: Box::new(value),
                    range: range(start, self.prev_end),
                }))
            }
            Tok::Star => {
                self.bump();
                let name = self.identifier()?;
                Ok(ast::Pattern::MatchStar(ast::PatternMatchStar {
                    name: (name.as_str() != "_").then_some(name),
                    range: range(start, self.prev_end),
                }))
            }
            Tok::Lpar => self.parse_paren_pattern(start),
            Tok::Lsqb => {
                self.bump();
                let mut patterns = Vec::new();
                while !self.at(Tok::Rsqb) {
                    patterns.push(self.parse_pattern()?);
                    if !self.at(Tok::Comma) {
                        break;
                    }
                    self.bump();
                }
                self.expect(Tok::Rsqb)?;
                Ok(ast::Pattern::MatchSequence(ast::PatternMatchSequence {
                    patterns,
                    range: range(start, self.prev_end),
                }))
            }
            Tok::Lbrace => self.parse_mapping_pattern(start),
            Tok::Name { .. } => {
                let id = self.identifier()?;
                let name_end = self.prev_end;
                if !self.at(Tok::Dot) && !self.at(Tok::Lpar) {
                    return Ok(ast::Pattern::MatchAs(ast::PatternMatchAs {
                        pattern: None,
                        name: (id.as_str() != "_").then_some(id),
                        range: range(start, name_end),
                    }));
                }
                let mut value = ast::Expr::Name(ast::ExprName {
                    id,
                    ctx: ast::ExprContext::Load,
                    range: range(start, name_end),
                });
                while self.at(Tok::Dot) {
                    self.bump();
                    let attr = self.identifier()?;
                    value = ast::Expr::Attribute(ast::ExprAttribute {
                        value: Box::new(value),
                        attr,
                        ctx: ast::ExprContext::Load,
                        range: range(start, self.prev_end),
                    });
                }
                if self.at(Tok::Lpar) {
                    return self.parse_class_pattern(start, value);
                }
                Ok(ast::Pattern::MatchValue(ast::PatternMatchValue {
                    value: Box::new(value),
                    range: range(start, self.prev_end),
                }))
            }
            _ => Err(self.unexpected()),
        }
    }

    /// `ConstantExpr` or `AddOpExpr`: `[-]number [(+|-) number]`.
    fn parse_signed_number(&mut self) -> PResult<ast::Expr> {
        let start = self.start();
        let mut value = if self.at(Tok::Minus) {
            self.bump();
            let operand = self.parse_constant_atom()?;
            ast::Expr::UnaryOp(ast::ExprUnaryOp {
                op: ast::UnaryOp::USub,
                operand: Box::new(operand),
                range: range(start, self.prev_end),
            })
        } else {
            self.parse_constant_atom()?
        };
        let op = match self.tok {
            Tok::Plus => ast::Operator::Add,
            Tok::Minus => ast::Operator::Sub,
            _ => return Ok(value),
        };
        self.bump();
        let right = self.parse_constant_atom()?;
        value = ast::Expr::BinOp(ast::ExprBinOp {
            left: Box::new(value),
            op,
            right: Box::new(right),
            range: range(start, self.prev_end),
        });
        Ok(value)
    }

    /// `ConstantAtom`: an int, float or complex literal.
    fn parse_constant_atom(&mut self) -> PResult<ast::Expr> {
        if !matches!(
            self.tok,
            Tok::Int { .. } | Tok::Float { .. } | Tok::Complex { .. }
        ) {
            return Err(self.unexpected());
        }
        let start = self.start();
        let value = self.number();
        Ok(ast::Expr::Constant(ast::ExprConstant {
            value,
            kind: None,
            range: range(start, self.prev_end),
        }))
    }

    /// After `(`: a group, or a sequence pattern.
    fn parse_paren_pattern(&mut self, start: TextSize) -> PResult<ast::Pattern> {
        self.bump();
        if self.at(Tok::Rpar) {
            self.bump();
            return Ok(ast::Pattern::MatchSequence(ast::PatternMatchSequence {
                patterns: Vec::new(),
                range: range(start, self.prev_end),
            }));
        }
        let first = self.parse_pattern()?;
        if self.at(Tok::Rpar) {
            self.bump();
            return Ok(first);
        }
        if !self.at(Tok::Comma) {
            return Err(self.unexpected());
        }
        let mut patterns = vec![first];
        while self.at(Tok::Comma) {
            self.bump();
            if self.at(Tok::Rpar) {
                break;
            }
            patterns.push(self.parse_pattern()?);
        }
        self.expect(Tok::Rpar)?;
        Ok(ast::Pattern::MatchSequence(ast::PatternMatchSequence {
            patterns,
            range: range(start, self.prev_end),
        }))
    }

    /// After the class name: `(patterns, keyword=patterns)`.
    fn parse_class_pattern(&mut self, start: TextSize, cls: ast::Expr) -> PResult<ast::Pattern> {
        self.bump(); // `(`
        let mut patterns = Vec::new();
        let mut kwd_attrs = Vec::new();
        let mut kwd_patterns = Vec::new();
        while !self.at(Tok::Rpar) {
            let keyword = !kwd_attrs.is_empty()
                || (matches!(self.tok, Tok::Name { .. })
                    && matches!(self.peek(1), Some(Tok::Equal)));
            if keyword {
                kwd_attrs.push(self.identifier()?);
                self.expect(Tok::Equal)?;
                kwd_patterns.push(self.parse_pattern()?);
            } else {
                patterns.push(self.parse_pattern()?);
            }
            if !self.at(Tok::Comma) {
                break;
            }
            self.bump();
        }
        self.expect(Tok::Rpar)?;
        Ok(ast::Pattern::MatchClass(ast::PatternMatchClass {
            cls: Box::new(cls),
            patterns,
            kwd_attrs,
            kwd_patterns,
            range: range(start, self.prev_end),
        }))
    }

    /// After `{`: `{key: pattern, ..., **rest}`.
    fn parse_mapping_pattern(&mut self, start: TextSize) -> PResult<ast::Pattern> {
        self.bump();
        let mut keys = Vec::new();
        let mut patterns = Vec::new();
        let mut rest = None;
        while !self.at(Tok::Rbrace) {
            if self.at(Tok::DoubleStar) {
                self.bump();
                rest = Some(self.identifier()?);
                if self.at(Tok::Comma) {
                    self.bump();
                }
                break;
            }
            keys.push(self.parse_mapping_key()?);
            self.expect(Tok::Colon)?;
            patterns.push(self.parse_pattern()?);
            if !self.at(Tok::Comma) {
                break;
            }
            self.bump();
        }
        self.expect(Tok::Rbrace)?;
        Ok(ast::Pattern::MatchMapping(ast::PatternMatchMapping {
            keys,
            patterns,
            rest,
            range: range(start, self.prev_end),
        }))
    }

    /// `MappingKey`: a literal or a dotted name.
    fn parse_mapping_key(&mut self) -> PResult<ast::Expr> {
        let start = self.start();
        let constant = |value: ast::Constant, end: TextSize| {
            Ok(ast::Expr::Constant(ast::ExprConstant {
                value,
                kind: None,
                range: range(start, end),
            }))
        };
        match self.tok {
            Tok::Int { .. } | Tok::Float { .. } | Tok::Complex { .. } | Tok::Minus => {
                self.parse_signed_number()
            }
            Tok::String { .. } => self.parse_strings(Follow::MappingKey),
            Tok::None => {
                self.bump();
                constant(ast::Constant::None, self.prev_end)
            }
            Tok::True => {
                self.bump();
                constant(true.into(), self.prev_end)
            }
            Tok::False => {
                self.bump();
                constant(false.into(), self.prev_end)
            }
            Tok::Name { .. } => {
                let id = self.identifier()?;
                let mut value = ast::Expr::Name(ast::ExprName {
                    id,
                    ctx: ast::ExprContext::Load,
                    range: range(start, self.prev_end),
                });
                // A bare name is not a key: at least one `.attr`.
                if !self.at(Tok::Dot) {
                    return Err(self.unexpected());
                }
                while self.at(Tok::Dot) {
                    self.bump();
                    let attr = self.identifier()?;
                    value = ast::Expr::Attribute(ast::ExprAttribute {
                        value: Box::new(value),
                        attr,
                        ctx: ast::ExprContext::Load,
                        range: range(start, self.prev_end),
                    });
                }
                Ok(value)
            }
            _ => Err(self.unexpected()),
        }
    }
}
