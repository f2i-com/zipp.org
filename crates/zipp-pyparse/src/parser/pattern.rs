//! `match` statements and their patterns.

use super::expr::starts_test;
use super::{body_end, range, Follow, PResult, Parser};
use crate::ast::*;
use crate::intern::{self, Sym};
use crate::lexer::Error;
use crate::token::T;

/// Tokens that can start a pattern.
fn starts_pattern(tok: T) -> bool {
    matches!(
        tok,
        T::Name
            | T::Int
            | T::Float
            | T::Complex
            | T::String
            | T::FStringStart
            | T::FStringBroken
            | T::Minus
            | T::None
            | T::True
            | T::False
            | T::Lpar
            | T::Lsqb
            | T::Lbrace
            | T::Star
    )
}

fn not_underscore(name: Sym) -> Option<Sym> {
    (name != intern::UNDERSCORE).then_some(name)
}

impl Parser<'_> {
    pub(super) fn parse_match(&mut self) -> PResult<StmtId> {
        let start = self.start();
        self.bump(); // `match`
        let base = self.s.exprs.len();
        let first = self.parse_elem()?.0;
        self.s.exprs.push(first);
        if self.at(T::Comma) {
            self.bump();
            while starts_test(self.tok()) || self.at(T::Star) {
                let subject = self.parse_elem()?.0;
                self.s.exprs.push(subject);
                if !self.at(T::Comma) {
                    break;
                }
                self.bump();
            }
        }
        self.expect(T::Colon)?;
        self.expect(T::Newline)?;
        if !self.at(T::Indent) {
            return Err(self.unexpected_expecting(true));
        }
        self.bump();
        let cbase = self.s.cases.len();
        loop {
            if !self.at(T::Case) {
                return Err(self.unexpected());
            }
            let case = self.parse_match_case()?;
            self.s.cases.push(case);
            if self.at(T::Dedent) {
                self.bump();
                break;
            }
        }
        let cases = self.finish_cases(cbase);
        let end = self
            .m
            .list(cases)
            .last()
            .map_or(0, |c| body_end(&self.m, c.body));
        // `match x, :` has the single subject `x`.
        let subject = if self.s.exprs.len() - base == 1 {
            self.s.exprs.pop().unwrap()
        } else {
            let elts = self.finish_exprs(base);
            self.add_expr(
                range(start, end),
                ExprKind::Tuple {
                    elts,
                    ctx: ExprContext::Load,
                },
            )
        };
        Ok(self.add_stmt(range(start, end), StmtKind::Match { subject, cases }))
    }

    fn parse_match_case(&mut self) -> PResult<MatchCase> {
        let start = self.start();
        self.bump(); // `case`
        let pattern = self.parse_patterns()?;
        let guard = if self.at(T::If) {
            self.bump();
            Some(self.parse_named_test()?.0)
        } else {
            None
        };
        self.expect(T::Colon)?;
        let body = self.parse_suite()?;
        Ok(MatchCase {
            range: range(start, body_end(&self.m, body)),
            pattern,
            guard,
            body,
        })
    }

    /// `Patterns`: a pattern, or an open sequence of them.
    fn parse_patterns(&mut self) -> PResult<PatId> {
        let start = self.start();
        let first = self.parse_pattern()?;
        if !self.at(T::Comma) {
            return Ok(first);
        }
        let base = self.s.pats.len();
        self.s.pats.push(first);
        while self.at(T::Comma) {
            self.bump();
            if !starts_pattern(self.tok()) {
                break;
            }
            let pattern = self.parse_pattern()?;
            self.s.pats.push(pattern);
        }
        let patterns = self.finish_pats(base);
        Ok(self.add_pat(
            range(start, self.prev_end),
            PatternKind::MatchSequence { patterns },
        ))
    }

    /// `Pattern`: an or-pattern with an optional `as name`.
    fn parse_pattern(&mut self) -> PResult<PatId> {
        self.enter()?;
        let result = self.parse_pattern_inner();
        self.leave();
        result
    }

    fn parse_pattern_inner(&mut self) -> PResult<PatId> {
        let start = self.start();
        let first = self.parse_closed_pattern()?;
        let pattern = if self.at(T::Vbar) {
            let base = self.s.pats.len();
            self.s.pats.push(first);
            while self.at(T::Vbar) {
                self.bump();
                let pattern = self.parse_closed_pattern()?;
                self.s.pats.push(pattern);
            }
            let patterns = self.finish_pats(base);
            self.add_pat(
                range(start, self.prev_end),
                PatternKind::MatchOr { patterns },
            )
        } else {
            first
        };
        if !self.at(T::As) {
            return Ok(pattern);
        }
        self.bump();
        let name = self.identifier()?;
        if name == intern::UNDERSCORE {
            return Err(self.reduce_error(
                Error::new("cannot use '_' as a target", start),
                Follow::Pattern,
            ));
        }
        Ok(self.add_pat(
            range(start, self.prev_end),
            PatternKind::MatchAs {
                pattern: Some(pattern),
                name: Some(name),
            },
        ))
    }

    fn parse_closed_pattern(&mut self) -> PResult<PatId> {
        let start = self.start();
        let singleton = |p: &mut Self, value: Constant| {
            p.bump();
            p.add_pat(
                range(start, p.prev_end),
                PatternKind::MatchSingleton { value },
            )
        };
        Ok(match self.tok() {
            T::None => singleton(self, Constant::None),
            T::True => singleton(self, Constant::True),
            T::False => singleton(self, Constant::False),
            T::Int | T::Float | T::Complex | T::Minus => {
                let value = self.parse_signed_number()?;
                self.add_pat(
                    range(start, self.prev_end),
                    PatternKind::MatchValue { value },
                )
            }
            T::String | T::FStringStart | T::FStringBroken => {
                let value = self.parse_strings(Follow::ClosedPattern)?;
                self.add_pat(
                    range(start, self.prev_end),
                    PatternKind::MatchValue { value },
                )
            }
            T::Star => {
                self.bump();
                let name = self.identifier()?;
                self.add_pat(
                    range(start, self.prev_end),
                    PatternKind::MatchStar {
                        name: not_underscore(name),
                    },
                )
            }
            T::Lpar => return self.parse_paren_pattern(start),
            T::Lsqb => {
                self.bump();
                let base = self.s.pats.len();
                while !self.at(T::Rsqb) {
                    let pattern = self.parse_pattern()?;
                    self.s.pats.push(pattern);
                    if !self.at(T::Comma) {
                        break;
                    }
                    self.bump();
                }
                self.expect(T::Rsqb)?;
                let patterns = self.finish_pats(base);
                self.add_pat(
                    range(start, self.prev_end),
                    PatternKind::MatchSequence { patterns },
                )
            }
            T::Lbrace => return self.parse_mapping_pattern(start),
            T::Name => {
                let id = self.identifier()?;
                let name_end = self.prev_end;
                if !self.at(T::Dot) && !self.at(T::Lpar) {
                    return Ok(self.add_pat(
                        range(start, name_end),
                        PatternKind::MatchAs {
                            pattern: None,
                            name: not_underscore(id),
                        },
                    ));
                }
                let mut value = self.add_expr(
                    range(start, name_end),
                    ExprKind::Name {
                        id,
                        ctx: ExprContext::Load,
                    },
                );
                while self.at(T::Dot) {
                    self.bump();
                    let attr = self.identifier()?;
                    value = self.add_expr(
                        range(start, self.prev_end),
                        ExprKind::Attribute {
                            value,
                            attr,
                            ctx: ExprContext::Load,
                        },
                    );
                }
                if self.at(T::Lpar) {
                    return self.parse_class_pattern(start, value);
                }
                self.add_pat(
                    range(start, self.prev_end),
                    PatternKind::MatchValue { value },
                )
            }
            _ => return Err(self.unexpected()),
        })
    }

    /// `ConstantExpr` or `AddOpExpr`: `[-]number [(+|-) number]`.
    fn parse_signed_number(&mut self) -> PResult<ExprId> {
        let start = self.start();
        let mut value = if self.at(T::Minus) {
            self.bump();
            let operand = self.parse_constant_atom()?;
            self.add_expr(
                range(start, self.prev_end),
                ExprKind::UnaryOp {
                    op: UnaryOp::USub,
                    operand,
                },
            )
        } else {
            self.parse_constant_atom()?
        };
        let op = match self.tok() {
            T::Plus => Operator::Add,
            T::Minus => Operator::Sub,
            _ => return Ok(value),
        };
        self.bump();
        let right = self.parse_constant_atom()?;
        value = self.add_expr(
            range(start, self.prev_end),
            ExprKind::BinOp {
                left: value,
                op,
                right,
            },
        );
        Ok(value)
    }

    /// `ConstantAtom`: an int, float or complex literal.
    fn parse_constant_atom(&mut self) -> PResult<ExprId> {
        let value = match self.tok() {
            T::Int | T::Float | T::Complex => self.number(),
            _ => return Err(self.unexpected()),
        };
        let start = self.start();
        self.bump();
        Ok(self.add_expr(range(start, self.prev_end), ExprKind::Constant(value)))
    }

    /// After `(`: a group, or a sequence pattern.
    fn parse_paren_pattern(&mut self, start: u32) -> PResult<PatId> {
        self.bump();
        if self.at(T::Rpar) {
            self.bump();
            return Ok(self.add_pat(
                range(start, self.prev_end),
                PatternKind::MatchSequence {
                    patterns: List::default(),
                },
            ));
        }
        let first = self.parse_pattern()?;
        if self.at(T::Rpar) {
            self.bump();
            return Ok(first);
        }
        if !self.at(T::Comma) {
            return Err(self.unexpected());
        }
        let base = self.s.pats.len();
        self.s.pats.push(first);
        while self.at(T::Comma) {
            self.bump();
            if self.at(T::Rpar) {
                break;
            }
            let pattern = self.parse_pattern()?;
            self.s.pats.push(pattern);
        }
        self.expect(T::Rpar)?;
        let patterns = self.finish_pats(base);
        Ok(self.add_pat(
            range(start, self.prev_end),
            PatternKind::MatchSequence { patterns },
        ))
    }

    /// After the class name: `(patterns, keyword=patterns)`.
    fn parse_class_pattern(&mut self, start: u32, cls: ExprId) -> PResult<PatId> {
        self.bump(); // `(`
        let pbase = self.s.pats.len();
        let abase = self.s.syms.len();
        // Keyword patterns wait here until the positional ones are finished.
        let mut kwd_patterns = Vec::new();
        while !self.at(T::Rpar) {
            let keyword =
                !kwd_patterns.is_empty() || (self.at(T::Name) && self.peek(1) == T::Equal);
            if keyword {
                let attr = self.identifier()?;
                self.s.syms.push(attr);
                self.expect(T::Equal)?;
                kwd_patterns.push(self.parse_pattern()?);
            } else {
                let pattern = self.parse_pattern()?;
                self.s.pats.push(pattern);
            }
            if !self.at(T::Comma) {
                break;
            }
            self.bump();
        }
        self.expect(T::Rpar)?;
        let patterns = self.finish_pats(pbase);
        let kwd_attrs = self.finish_syms(abase);
        let kbase = self.s.pats.len();
        self.s.pats.extend(kwd_patterns);
        let kwd_patterns = self.finish_pats(kbase);
        Ok(self.add_pat(
            range(start, self.prev_end),
            PatternKind::MatchClass {
                cls,
                patterns,
                kwd_attrs,
                kwd_patterns,
            },
        ))
    }

    /// After `{`: `{key: pattern, ..., **rest}`.
    fn parse_mapping_pattern(&mut self, start: u32) -> PResult<PatId> {
        self.bump();
        let kbase = self.s.exprs.len();
        let mut values = Vec::new();
        let mut rest = None;
        while !self.at(T::Rbrace) {
            if self.at(T::DoubleStar) {
                self.bump();
                rest = Some(self.identifier()?);
                if self.at(T::Comma) {
                    self.bump();
                }
                break;
            }
            let key = self.parse_mapping_key()?;
            self.s.exprs.push(key);
            self.expect(T::Colon)?;
            values.push(self.parse_pattern()?);
            if !self.at(T::Comma) {
                break;
            }
            self.bump();
        }
        self.expect(T::Rbrace)?;
        let keys = self.finish_exprs(kbase);
        let pbase = self.s.pats.len();
        self.s.pats.extend(values);
        let patterns = self.finish_pats(pbase);
        Ok(self.add_pat(
            range(start, self.prev_end),
            PatternKind::MatchMapping {
                keys,
                patterns,
                rest,
            },
        ))
    }

    /// `MappingKey`: a literal or a dotted name.
    fn parse_mapping_key(&mut self) -> PResult<ExprId> {
        let start = self.start();
        let constant = |p: &mut Self, value: Constant| {
            p.bump();
            p.add_expr(range(start, p.prev_end), ExprKind::Constant(value))
        };
        Ok(match self.tok() {
            T::Int | T::Float | T::Complex | T::Minus => return self.parse_signed_number(),
            T::String | T::FStringStart | T::FStringBroken => {
                return self.parse_strings(Follow::MappingKey)
            }
            T::None => constant(self, Constant::None),
            T::True => constant(self, Constant::True),
            T::False => constant(self, Constant::False),
            T::Name => {
                let id = self.identifier()?;
                let mut value = self.add_expr(
                    range(start, self.prev_end),
                    ExprKind::Name {
                        id,
                        ctx: ExprContext::Load,
                    },
                );
                // A bare name is not a key: at least one `.attr`.
                if !self.at(T::Dot) {
                    return Err(self.unexpected());
                }
                while self.at(T::Dot) {
                    self.bump();
                    let attr = self.identifier()?;
                    value = self.add_expr(
                        range(start, self.prev_end),
                        ExprKind::Attribute {
                            value,
                            attr,
                            ctx: ExprContext::Load,
                        },
                    );
                }
                value
            }
            _ => return Err(self.unexpected()),
        })
    }
}
