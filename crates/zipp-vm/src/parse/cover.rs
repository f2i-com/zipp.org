//! Cover-grammar resolution.
//!
//! `( a, b )` is a parenthesized expression until a `=>` proves it was an
//! arrow's parameters, and the `=>` sits past the closing paren — so no bounded
//! lookahead decides it. The spec's answer, and this one: parse the permissive
//! superset ONCE, record the errors that are fatal in only one of the two
//! readings, then discharge the losing set when the ambiguity resolves.
//!
//! The conversions live here because both directions are needed and they are
//! not the same function:
//!
//! - [`Parser::expr_to_target`] — expression → assignment target
//!   (`[a.b] = x` is fine: a target may be a member access).
//! - [`Parser::expr_to_pattern`] — expression → BINDING pattern
//!   (`[a.b] = x` as a *declaration* is not: a binding must be a name).

use super::ast::*;
use super::parser::{PResult, Parser, SyntaxError};
use super::token::{Punct, Span};

impl<'s> Parser<'s> {
    /// `( … )` — a parenthesized expression, or arrow parameters.
    ///
    /// Returns `None` when this was an `async ( … )` that turned out to be an
    /// ordinary call, so the caller can re-parse it as one.
    pub(crate) fn parse_paren_or_arrow(
        &mut self,
        is_async: bool,
        start: u32,
    ) -> PResult<Option<Expr>> {
        let outer_cover = std::mem::take(&mut self.cover);
        let lparen = self.expect(Punct::LParen, true)?;

        let mut items: Vec<Expr> = Vec::new();
        let mut rest: Option<(Expr, u32)> = None;
        let mut trailing_comma = false;

        while !self.at(Punct::RParen) {
            if self.at(Punct::DotDotDot) {
                let pos = self.cur().span.start;
                self.bump_before_operand()?;
                // `...x` is legal only in a parameter list.
                self.cover_pattern_only(SyntaxError::new(
                    "SyntaxError: rest parameter is only valid in a parameter list",
                    pos,
                ));
                rest = Some((self.parse_assign()?, pos));
                if self.at(Punct::Comma) {
                    self.cover_pattern_only(SyntaxError::new(
                        "SyntaxError: rest parameter must be last",
                        pos,
                    ));
                }
                break;
            }
            items.push(self.parse_assign()?);
            if !self.eat(Punct::Comma, true)? {
                break;
            }
            if self.at(Punct::RParen) {
                trailing_comma = true;
            }
        }
        let rparen = self.expect(Punct::RParen, false)?;

        // `()` is only ever a parameter list.
        if items.is_empty() && rest.is_none() {
            self.cover_pattern_only(SyntaxError::new(
                "SyntaxError: unexpected token ')'",
                lparen.start,
            ));
        }
        if trailing_comma && rest.is_some() {
            self.cover_pattern_only(SyntaxError::new(
                "SyntaxError: a rest parameter may not have a trailing comma",
                rparen.start,
            ));
        }

        let is_arrow = self.at(Punct::Arrow) && !self.cur().newline_before;

        if is_arrow {
            self.bump_before_operand()?; // `=>`
            let inner = std::mem::replace(&mut self.cover, outer_cover);
            // Reading resolved: the expression-only errors are discharged, the
            // pattern-only ones were never errors here.
            if let Some(e) = inner.expr_only {
                return Err(e);
            }
            // `yield`/`await` USED AS EXPRESSIONS inside what turned out to be
            // parameters. Legal where they were parsed, illegal now — which is
            // exactly why the decision had to wait for the `=>`.
            if let Some(e) = inner.yield_await.into_iter().next() {
                return Err(e);
            }
            let mut params = Vec::with_capacity(items.len() + 1);
            for it in items {
                params.push(self.expr_to_pattern(it)?);
            }
            if let Some((r, _)) = rest {
                params.push(Pattern::Rest(Box::new(self.expr_to_pattern(r)?)));
            }
            let simple = params.iter().all(|p| matches!(p, Pattern::Ident(_)));
            return Ok(Some(self.finish_arrow(Params { items: params, simple }, is_async, start)?));
        }

        // Not an arrow. If this was `async ( … )`, it is a CALL to `async`.
        if is_async {
            self.cover = outer_cover;
            return Ok(None);
        }

        let inner = std::mem::replace(&mut self.cover, outer_cover);
        if let Some(e) = inner.pattern_only {
            return Err(e);
        }
        // The parenthesized errors that survive belong to the enclosing region.
        if let Some(e) = inner.expr_only {
            self.cover_expr_only(e);
        }
        for e in inner.yield_await {
            self.cover.yield_await.push(e);
        }

        if items.is_empty() {
            return Err(self.err_here("SyntaxError: unexpected token ')'"));
        }
        let mut it = items.into_iter();
        let first = it.next().unwrap();
        let e = match it.len() {
            0 => first,
            _ => {
                let mut v = vec![first];
                v.extend(it);
                Expr::Seq(v)
            }
        };
        // Remember the parentheses, which is the whole reason the AST has a
        // `covered` bit rather than a wrapper node: `(x) = f` is a valid
        // assignment but suppresses NamedEvaluation.
        self.mark_parenthesized();
        Ok(Some(e))
    }

    /// Parse an arrow's body and assemble it. The parameters are already known.
    pub(crate) fn finish_arrow(
        &mut self,
        params: Params,
        is_async: bool,
        start: u32,
    ) -> PResult<Expr> {
        let saved = self.ctx;
        // An arrow has no `this`, no `arguments`, and no `yield` of its own — it
        // inherits them. But `await` follows its OWN async-ness.
        self.ctx.await_ = is_async;
        self.ctx.in_ = true;
        let body = if self.at(Punct::LBrace) {
            ArrowBody::Block(self.parse_fn_body()?)
        } else {
            ArrowBody::Expr(Box::new(self.parse_assign()?))
        };
        self.ctx = saved;
        let span = Span::new(start, self.prev_end());
        Ok(Expr::Arrow(Box::new(Arrow { params, body, is_async, span })))
    }

    // ---- conversions -------------------------------------------------------

    /// Expression → assignment target.
    ///
    /// `simple_only` is set for the compound operators (`+=`, `**=`, …), which
    /// require a *simple* target — `[a] += x` is not a thing.
    pub(crate) fn expr_to_target(
        &mut self,
        e: Expr,
        allow_pattern: bool,
        pos: u32,
    ) -> PResult<Target> {
        Ok(match e {
            Expr::Ident(name) => {
                if self.ctx.strict && (&*name == "eval" || &*name == "arguments") {
                    return Err(SyntaxError::new(
                        format!("SyntaxError: cannot assign to '{name}' in strict mode"),
                        pos,
                    ));
                }
                Target::Ident { name, covered: self.take_parenthesized() }
            }
            Expr::Member(m) => Target::Member(m),
            // Annex B: a call is a SIMPLE assignment target in sloppy code. It
            // must PARSE and throw a ReferenceError at runtime — which is the
            // single case oxc's AST could not represent, and the reason the
            // engine used to rewrite source text and reparse.
            Expr::Call(c) => {
                if self.ctx.strict {
                    return Err(SyntaxError::new(
                        "SyntaxError: invalid assignment target",
                        pos,
                    ));
                }
                Target::Call(c)
            }
            Expr::Array(items) if allow_pattern => {
                let mut out = Vec::with_capacity(items.len());
                let n = items.len();
                for (i, item) in items.into_iter().enumerate() {
                    out.push(match item {
                        None => None,
                        Some(ArrayElem::Spread(inner)) => {
                            if i + 1 != n {
                                return Err(SyntaxError::new(
                                    "SyntaxError: rest element must be last",
                                    pos,
                                ));
                            }
                            Some(TargetElem {
                                target: self.expr_to_target(inner, true, pos)?,
                                default: None,
                                rest: true,
                            })
                        }
                        Some(ArrayElem::Expr(inner)) => {
                            let (target, default) = self.split_default(inner, pos)?;
                            Some(TargetElem { target, default, rest: false })
                        }
                    });
                }
                Target::Array(out)
            }
            Expr::Object(members) if allow_pattern => {
                let mut props = Vec::new();
                let mut rest = None;
                let n = members.len();
                for (i, m) in members.into_iter().enumerate() {
                    match m {
                        ObjectMember::Spread(inner) => {
                            if i + 1 != n {
                                return Err(SyntaxError::new(
                                    "SyntaxError: rest element must be last",
                                    pos,
                                ));
                            }
                            rest = Some(Box::new(self.expr_to_target(inner, false, pos)?));
                        }
                        ObjectMember::Prop { key, value, shorthand, init } => {
                            // A CoverInitializedName is legal here and only
                            // here — this is the conversion that makes
                            // `({a = 1} = {})` work.
                            let (target, default) = match init {
                                Some(d) => (self.expr_to_target(value, true, pos)?, Some(d)),
                                None => self.split_default(value, pos)?,
                            };
                            props.push(TargetProp { key, target, default, shorthand });
                        }
                        _ => {
                            return Err(SyntaxError::new(
                                "SyntaxError: invalid destructuring assignment target",
                                pos,
                            ))
                        }
                    }
                }
                Target::Object { props, rest }
            }
            _ => return Err(SyntaxError::new("SyntaxError: invalid assignment target", pos)),
        })
    }

    /// Split `a = 1` inside a destructuring position into target and default.
    fn split_default(&mut self, e: Expr, pos: u32) -> PResult<(Target, Option<Expr>)> {
        if let Expr::Assign { op: AssignOp::Assign, target, value } = e {
            return Ok((target, Some(*value)));
        }
        Ok((self.expr_to_target(e, true, pos)?, None))
    }

    /// Expression → BINDING pattern, for arrow parameters.
    ///
    /// Stricter than [`Self::expr_to_target`]: a binding introduces a name, so a
    /// member access or call is not allowed however valid it would be as an
    /// assignment target.
    pub(crate) fn expr_to_pattern(&mut self, e: Expr) -> PResult<Pattern> {
        Ok(match e {
            Expr::Ident(name) => {
                if self.ctx.strict && (&*name == "eval" || &*name == "arguments") {
                    return Err(self.err_here(format!(
                        "SyntaxError: '{name}' cannot be a parameter name in strict mode"
                    )));
                }
                Pattern::Ident(name)
            }
            Expr::Assign { op: AssignOp::Assign, target, value } => Pattern::Assign {
                left: Box::new(self.target_to_pattern(target)?),
                right: value,
            },
            Expr::Array(items) => {
                let n = items.len();
                let mut out = Vec::with_capacity(n);
                for (i, item) in items.into_iter().enumerate() {
                    out.push(match item {
                        None => None,
                        Some(ArrayElem::Spread(inner)) => {
                            if i + 1 != n {
                                return Err(
                                    self.err_here("SyntaxError: rest element must be last")
                                );
                            }
                            Some(PatternElem {
                                pat: Pattern::Rest(Box::new(self.expr_to_pattern(inner)?)),
                            })
                        }
                        Some(ArrayElem::Expr(inner)) => {
                            Some(PatternElem { pat: self.expr_to_pattern(inner)? })
                        }
                    });
                }
                Pattern::Array(out)
            }
            Expr::Object(members) => {
                let mut props = Vec::new();
                let mut rest = None;
                let n = members.len();
                for (i, m) in members.into_iter().enumerate() {
                    match m {
                        ObjectMember::Spread(inner) => {
                            if i + 1 != n {
                                return Err(
                                    self.err_here("SyntaxError: rest element must be last")
                                );
                            }
                            rest = Some(Box::new(self.expr_to_pattern(inner)?));
                        }
                        ObjectMember::Prop { key, value, shorthand, init } => {
                            let inner = self.expr_to_pattern(value)?;
                            let value = match init {
                                Some(d) => {
                                    Pattern::Assign { left: Box::new(inner), right: Box::new(d) }
                                }
                                None => inner,
                            };
                            props.push(PatternProp { key, value, shorthand });
                        }
                        _ => {
                            return Err(
                                self.err_here("SyntaxError: invalid destructuring parameter")
                            )
                        }
                    }
                }
                Pattern::Object { props, rest }
            }
            _ => return Err(self.err_here("SyntaxError: invalid parameter")),
        })
    }

    fn target_to_pattern(&mut self, t: Target) -> PResult<Pattern> {
        Ok(match t {
            Target::Ident { name, .. } => Pattern::Ident(name),
            Target::Array(items) => Pattern::Array(
                items
                    .into_iter()
                    .map(|i| match i {
                        None => Ok(None),
                        Some(e) => {
                            let inner = self.target_to_pattern(e.target)?;
                            let pat = match e.default {
                                Some(d) => Pattern::Assign {
                                    left: Box::new(inner),
                                    right: Box::new(d),
                                },
                                None => inner,
                            };
                            Ok(Some(PatternElem {
                                pat: if e.rest { Pattern::Rest(Box::new(pat)) } else { pat },
                            }))
                        }
                    })
                    .collect::<PResult<Vec<_>>>()?,
            ),
            Target::Object { props, rest } => {
                let mut out = Vec::with_capacity(props.len());
                for p in props {
                    let inner = self.target_to_pattern(p.target)?;
                    let value = match p.default {
                        Some(d) => Pattern::Assign { left: Box::new(inner), right: Box::new(d) },
                        None => inner,
                    };
                    out.push(PatternProp { key: p.key, value, shorthand: p.shorthand });
                }
                Pattern::Object {
                    props: out,
                    rest: match rest {
                        Some(r) => Some(Box::new(self.target_to_pattern(*r)?)),
                        None => None,
                    },
                }
            }
            Target::Member(_) | Target::Call(_) => {
                return Err(self.err_here("SyntaxError: invalid parameter"))
            }
        })
    }
}
