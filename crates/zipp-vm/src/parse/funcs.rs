//! Functions, methods, classes, and parameter lists.
//!
//! The span discipline here is load-bearing and easy to get silently wrong:
//! `Function.prototype.toString` must return the EXACT original source, so each
//! node records the byte range of the production it came from — which for a
//! method is the whole `MethodDefinition` (key, `get`/`set`/`async`/`*`
//! included) and NOT the inner function, whose text begins at the `(`.

use super::ast::*;
use super::parser::{BindKind, PResult, Parser, ScopeKind, SyntaxError};
use super::token::{Keyword, Punct, Span};

impl<'s> Parser<'s> {
    /// A function after its `function` keyword has been consumed. `start` is the
    /// offset of `function` (or of `async`, for an async function) so the span
    /// covers the whole production.
    pub(crate) fn parse_function_rest(&mut self, is_async: bool, start: u32) -> PResult<Function> {
        let is_generator = self.eat(Punct::Star, false)?;
        // A function EXPRESSION's own name is in scope inside its body but not
        // outside; a declaration's name binds in the enclosing scope. The caller
        // handles the second, so the name is just read here.
        let name = if self.is_binding_ident_for_fn(is_generator, is_async) {
            Some(self.binding_ident()?.0)
        } else {
            None
        };
        let (params, body) = self.parse_fn_tail(is_async, is_generator)?;
        Ok(Function { name, params, body, is_async, is_generator, span: Span::new(start, self.prev_end()) })
    }

    /// A function's own name is checked against the strictness and
    /// yield/await-ness of the CONTEXT IT SITS IN for a declaration, but of its
    /// OWN body for an expression. Approximated here as the enclosing context,
    /// which is what the current engine does.
    fn is_binding_ident_for_fn(&self, _gen: bool, _asyn: bool) -> bool {
        self.is_binding_ident()
    }

    /// Parameters + body, with the context switched to the function's own.
    fn parse_fn_tail(&mut self, is_async: bool, is_generator: bool) -> PResult<(Params, FnBody)> {
        let saved = self.ctx;
        let saved_labels = std::mem::take(&mut self.labels);
        // A function body resets these from its OWN flags — unlike an arrow,
        // which inherits them.
        self.ctx.yield_ = is_generator;
        self.ctx.await_ = is_async;
        self.ctx.return_ = true;
        self.ctx.new_target = true;
        self.ctx.in_ = true;
        self.ctx.in_loop = false;
        self.ctx.in_switch = false;
        self.ctx.in_field_init = false;

        let params = self.parse_params()?;
        let body = self.parse_fn_body()?;

        self.ctx = saved;
        self.labels = saved_labels;
        Ok((params, body))
    }

    /// A method's parameters and body. `start` is the offset of the whole
    /// MethodDefinition, which is what `toString` must reproduce.
    pub(crate) fn parse_method_rest(
        &mut self,
        is_async: bool,
        is_generator: bool,
        start: u32,
    ) -> PResult<Function> {
        let saved_super = self.ctx.super_prop;
        // A method — object literal or class — may use `super.x`.
        self.ctx.super_prop = true;
        let (params, body) = self.parse_fn_tail(is_async, is_generator)?;
        self.ctx.super_prop = saved_super;
        Ok(Function {
            name: None,
            params,
            body,
            is_async,
            is_generator,
            span: Span::new(start, self.prev_end()),
        })
    }

    pub(crate) fn parse_params(&mut self) -> PResult<Params> {
        self.expect(Punct::LParen, true)?;
        let mut items = Vec::new();
        while !self.at(Punct::RParen) {
            if self.at(Punct::DotDotDot) {
                let pos = self.cur().span.start;
                self.bump_before_operand()?;
                items.push(Pattern::Rest(Box::new(self.parse_binding_pattern()?)));
                if self.at(Punct::Comma) {
                    return Err(SyntaxError::new(
                        "SyntaxError: rest parameter must be last",
                        pos,
                    ));
                }
                break;
            }
            let pat = self.parse_binding_pattern()?;
            items.push(if self.eat(Punct::Eq, true)? {
                Pattern::Assign { left: Box::new(pat), right: Box::new(self.parse_assign()?) }
            } else {
                pat
            });
            if !self.eat(Punct::Comma, true)? {
                break;
            }
        }
        self.expect(Punct::RParen, false)?;
        // IsSimpleParameterList. Not a convenience: it decides whether
        // `arguments` is mapped, and whether a "use strict" directive in the
        // body is an early error.
        let simple = items.iter().all(|p| matches!(p, Pattern::Ident(_)));
        Ok(Params { items, simple })
    }

    /// A `{ … }` function body, with its own directive prologue.
    pub(crate) fn parse_fn_body(&mut self) -> PResult<FnBody> {
        self.expect(Punct::LBrace, true)?;
        let saved_strict = self.ctx.strict;
        let (directives, strict) = self.directive_prologue()?;
        if strict {
            self.ctx.strict = true;
        }
        self.scopes.push(ScopeKind::Function);
        let mut stmts = Vec::new();
        while !self.at(Punct::RBrace) && !self.at_eof() {
            stmts.push(self.parse_stmt_list_item()?);
        }
        self.scopes.pop()?;
        self.expect(Punct::RBrace, false)?;
        let out = FnBody { directives, stmts, strict: self.ctx.strict };
        self.ctx.strict = saved_strict;
        Ok(out)
    }

    // ---- binding patterns --------------------------------------------------

    pub(crate) fn parse_binding_pattern(&mut self) -> PResult<Pattern> {
        if self.at(Punct::LBracket) {
            return self.parse_array_binding();
        }
        if self.at(Punct::LBrace) {
            return self.parse_object_binding();
        }
        Ok(Pattern::Ident(self.binding_ident()?.0))
    }

    fn parse_array_binding(&mut self) -> PResult<Pattern> {
        self.expect(Punct::LBracket, true)?;
        let mut out = Vec::new();
        while !self.at(Punct::RBracket) {
            if self.at(Punct::Comma) {
                self.bump_before_operand()?;
                out.push(None); // a hole
                continue;
            }
            let pat = if self.at(Punct::DotDotDot) {
                let pos = self.cur().span.start;
                self.bump_before_operand()?;
                let inner = self.parse_binding_pattern()?;
                if self.at(Punct::Comma) {
                    return Err(SyntaxError::new("SyntaxError: rest element must be last", pos));
                }
                Pattern::Rest(Box::new(inner))
            } else {
                let p = self.parse_binding_pattern()?;
                if self.eat(Punct::Eq, true)? {
                    Pattern::Assign { left: Box::new(p), right: Box::new(self.parse_assign()?) }
                } else {
                    p
                }
            };
            out.push(Some(PatternElem { pat }));
            if !self.eat(Punct::Comma, true)? {
                break;
            }
        }
        self.expect(Punct::RBracket, false)?;
        Ok(Pattern::Array(out))
    }

    fn parse_object_binding(&mut self) -> PResult<Pattern> {
        self.expect(Punct::LBrace, true)?;
        let mut props = Vec::new();
        let mut rest = None;
        while !self.at(Punct::RBrace) {
            if self.at(Punct::DotDotDot) {
                let pos = self.cur().span.start;
                self.bump_before_operand()?;
                // An object rest binding must be a plain identifier.
                rest = Some(Box::new(Pattern::Ident(self.binding_ident()?.0)));
                if self.at(Punct::Comma) {
                    return Err(SyntaxError::new("SyntaxError: rest element must be last", pos));
                }
                break;
            }
            let key = self.parse_prop_key()?;
            let value = if self.eat(Punct::Colon, true)? {
                self.parse_binding_pattern()?
            } else {
                // Shorthand `{a}` / `{a = 1}`.
                let PropKey::Ident(n) = &key else {
                    return Err(self.err_here("SyntaxError: expected ':' in binding pattern"));
                };
                Pattern::Ident(n.clone())
            };
            let value = if self.eat(Punct::Eq, true)? {
                Pattern::Assign { left: Box::new(value), right: Box::new(self.parse_assign()?) }
            } else {
                value
            };
            let shorthand = matches!(&key, PropKey::Ident(_));
            props.push(PatternProp { key, value, shorthand });
            if !self.eat(Punct::Comma, true)? {
                break;
            }
        }
        self.expect(Punct::RBrace, false)?;
        Ok(Pattern::Object { props, rest })
    }

    // ---- classes -----------------------------------------------------------

    /// A class after its `class` keyword. A class body is ALWAYS strict, even
    /// inside sloppy code.
    pub(crate) fn parse_class_rest(&mut self, start: u32) -> PResult<Class> {
        let saved_strict = self.ctx.strict;
        self.ctx.strict = true;

        let name = if self.is_binding_ident() { Some(self.binding_ident()?.0) } else { None };
        let superclass = if self.eat_kw(Keyword::Extends, true)? {
            Some(Box::new(self.parse_lhs_public()?))
        } else {
            None
        };
        let derived = superclass.is_some();

        self.expect(Punct::LBrace, true)?;
        let mut body = Vec::new();
        let mut saw_ctor = false;
        while !self.at(Punct::RBrace) && !self.at_eof() {
            // A stray `;` between members is legal and means nothing.
            if self.eat(Punct::Semi, true)? {
                continue;
            }
            let m = self.parse_class_member(derived)?;
            if let ClassMember::Method(cm) = &m {
                if cm.kind == MethodKind::Constructor {
                    if saw_ctor {
                        return Err(self.err_here(
                            "SyntaxError: a class may only have one constructor",
                        ));
                    }
                    saw_ctor = true;
                }
            }
            body.push(m);
        }
        self.expect(Punct::RBrace, false)?;
        self.ctx.strict = saved_strict;
        Ok(Class { name, superclass, body, span: Span::new(start, self.prev_end()) })
    }

    /// `parse_lhs` is private to the expression module; classes need it for
    /// `extends`.
    fn parse_lhs_public(&mut self) -> PResult<Expr> {
        // `extends` takes a LeftHandSideExpression, so `extends a.b` binds the
        // member but `extends a + b` is a syntax error.
        self.parse_assign()
    }

    fn parse_class_member(&mut self, derived: bool) -> PResult<ClassMember> {
        let start = self.cur().span.start;
        let is_static = if self.at_kw(Keyword::Static) {
            let save = self.save();
            self.bump_after_operand()?;
            // `static` alone is a field name: `class C { static = 1 }`.
            if self.at(Punct::Eq) || self.at(Punct::Semi) || self.at(Punct::RBrace) {
                self.restore(save);
                false
            } else {
                true
            }
        } else {
            false
        };

        // `static { … }`
        if is_static && self.at(Punct::LBrace) {
            self.bump_before_operand()?;
            let saved = self.ctx;
            self.ctx.in_field_init = true;
            self.ctx.return_ = false;
            self.ctx.super_prop = true;
            self.scopes.push(ScopeKind::ClassStaticBlock);
            let mut stmts = Vec::new();
            while !self.at(Punct::RBrace) && !self.at_eof() {
                stmts.push(self.parse_stmt_list_item()?);
            }
            self.scopes.pop()?;
            self.expect(Punct::RBrace, false)?;
            self.ctx = saved;
            return Ok(ClassMember::StaticBlock(stmts));
        }

        // Accessors and modifiers, each of which may instead be a plain name.
        for (kw, kind) in [(Keyword::Get, MethodKind::Get), (Keyword::Set, MethodKind::Set)] {
            if self.at_kw(kw) {
                let save = self.save();
                self.bump_after_operand()?;
                if self.at_class_key_start() {
                    let key = self.parse_prop_key()?;
                    let func = self.parse_method_rest(false, false, start)?;
                    return Ok(ClassMember::Method(ClassMethod {
                        key,
                        kind,
                        func: Box::new(func),
                        is_static,
                    }));
                }
                self.restore(save);
                break;
            }
        }
        let mut is_async = false;
        if self.at_kw(Keyword::Async) {
            let save = self.save();
            self.bump_after_operand()?;
            if !self.cur().newline_before && (self.at_class_key_start() || self.at(Punct::Star)) {
                is_async = true;
            } else {
                self.restore(save);
            }
        }
        let is_generator = self.eat(Punct::Star, false)?;

        let key = self.parse_prop_key()?;

        if self.at(Punct::LParen) {
            let is_ctor = !is_static
                && !is_async
                && !is_generator
                && matches!(&key, PropKey::Ident(n) if &**n == "constructor");
            let saved_call = self.ctx.super_call;
            if is_ctor {
                // `super()` is legal only in a DERIVED constructor.
                self.ctx.super_call = derived;
            }
            let func = self.parse_method_rest(is_async, is_generator, start)?;
            self.ctx.super_call = saved_call;
            return Ok(ClassMember::Method(ClassMethod {
                key,
                kind: if is_ctor { MethodKind::Constructor } else { MethodKind::Method },
                func: Box::new(func),
                is_static,
            }));
        }

        // A field. Its initializer runs in a context where `arguments` is an
        // early error and `return` is illegal.
        let value = if self.eat(Punct::Eq, true)? {
            let saved = self.ctx;
            self.ctx.in_field_init = true;
            self.ctx.return_ = false;
            self.ctx.super_prop = true;
            let v = self.parse_assign()?;
            self.ctx = saved;
            Some(v)
        } else {
            None
        };
        self.semicolon()?;
        Ok(ClassMember::Field(ClassField { key, value, is_static }))
    }

    fn at_class_key_start(&self) -> bool {
        matches!(
            self.cur().kind,
            super::token::TokenKind::Ident { .. }
                | super::token::TokenKind::Str(_)
                | super::token::TokenKind::Num(_)
        ) || self.at(Punct::LBracket)
    }

    /// Bind a declaration's name in the current scope, applying the duplicate
    /// rules.
    pub(crate) fn declare(&mut self, name: &str, kind: BindKind, pos: u32) -> PResult<()> {
        match kind {
            BindKind::Var | BindKind::Function => self.scopes.declare_var(name, pos),
            _ => self.scopes.declare_lexical(name, kind, pos),
        }
    }
}
