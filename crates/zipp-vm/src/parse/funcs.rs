//! Functions, methods, classes, and parameter lists.
//!
//! The span discipline here is load-bearing and easy to get silently wrong:
//! `Function.prototype.toString` must return the EXACT original source, so each
//! node records the byte range of the production it came from — which for a
//! method is the whole `MethodDefinition` (key, `get`/`set`/`async`/`*`
//! included) and NOT the inner function, whose text begins at the `(`.

use super::ast::*;
use super::parser::{BindKind, PResult, Parser, ScopeKind, SyntaxError};
use super::token::{Keyword, Punct, Span, StrVal};

/// The spec's `PropName`, for the two names the class rules care about
/// (`"constructor"` and `"prototype"`). A computed key has no PropName, and a
/// numeric one can never spell either word, so both answer `None` — which is
/// exactly the exemption `class C { static ['prototype'](){} }` relies on.
fn prop_name(key: &PropKey) -> Option<&str> {
    match key {
        PropKey::Ident(n) => Some(n),
        PropKey::Str(StrVal::Utf8(s)) => Some(s),
        // A lone-surrogate key is not a name any of these rules match.
        PropKey::Str(StrVal::Utf16(_)) | PropKey::Num(_) => None,
        PropKey::Computed(_) | PropKey::Private(_) => None,
    }
}

impl<'s> Parser<'s> {
    /// A function after its `function` keyword has been consumed. `start` is the
    /// offset of `function` (or of `async`, for an async function) so the span
    /// covers the whole production. `is_expr` distinguishes a FunctionExpression
    /// from a FunctionDeclaration, which differ only in how the name is read.
    pub(crate) fn parse_function_rest(
        &mut self,
        is_async: bool,
        is_expr: bool,
        start: u32,
    ) -> PResult<Function> {
        let is_generator = self.eat(Punct::Star, false)?;
        // A function EXPRESSION's own name is in scope inside its body but not
        // outside; a declaration's name binds in the enclosing scope. The caller
        // handles the second, so the name is just read here.
        //
        // The two also differ in which `[Yield]/[Await]` the name is read under.
        // A FunctionDeclaration's is `BindingIdentifier[?Yield, ?Await]` — the
        // ENCLOSING context — while every expression form names its OWN:
        // `[~Yield, ~Await]` for `function` (15.2), `[+Yield, ~Await]` for
        // `function*` (15.5), and correspondingly for the async forms (15.8,
        // 15.9). So `function* g(){ (function yield(){}); }` is legal sloppy code
        // and `(function* yield(){})` is not, which the enclosing-context
        // approximation got backwards in both directions.
        let name = {
            let saved = (self.ctx.yield_, self.ctx.await_);
            if is_expr {
                self.ctx.yield_ = is_generator;
                // `await` is never an Identifier in Module code (13.1.1), so
                // there the relaxation does not apply.
                self.ctx.await_ = is_async || self.goal == Goal::Module;
            }
            let n = if self.is_binding_ident() { Some(self.binding_ident()) } else { None };
            (self.ctx.yield_, self.ctx.await_) = saved;
            match n {
                Some(r) => Some(r?.0),
                None => None,
            }
        };
        let (params, body) = self.parse_fn_tail(is_async, is_generator, false)?;
        Ok(Function { name, params, body, is_async, is_generator, span: Span::new(start, self.prev_end()) })
    }

    /// Parameters + body, with the context switched to the function's own.
    /// `unique` selects the spec's `UniqueFormalParameters` over plain
    /// `FormalParameters`: every METHOD (object literal or class, plain or
    /// generator or async, constructor included) takes the unique form, while a
    /// FunctionDeclaration/Expression -- generator and async ones too -- takes
    /// the permissive one and forbids duplicates only when strict or non-simple.
    fn parse_fn_tail(
        &mut self,
        is_async: bool,
        is_generator: bool,
        unique: bool,
    ) -> PResult<(Params, FnBody)> {
        let saved = self.ctx;
        let saved_labels = std::mem::take(&mut self.labels);
        // Parameters AND body are function code, hidden from any enclosing
        // cover region — see `Parser::enter_fn_code`.
        let fn_code = self.enter_fn_code();
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

        let params_at = self.cur().span.start;
        let params = self.parse_params()?;
        self.check_unique_params(&params, unique, params_at)?;
        let body = self.parse_fn_body_with_params(Some(&params))?;

        self.leave_fn_code(fn_code);
        self.ctx = saved;
        self.labels = saved_labels;
        self.check_use_strict_with_non_simple_params(&params, &body, params_at)?;
        Ok((params, body))
    }

    /// It is a Syntax Error if `FunctionBodyContainsUseStrict` is true and
    /// `IsSimpleParameterList` of the FormalParameters is false.
    ///
    /// The reason the spec forbids it: a non-simple parameter list is evaluated
    /// in its own scope with its own semantics, and a body directive would have
    /// to retroactively change how the parameters were already parsed — so
    /// `function f(a = 1) { "use strict"; }` is an error even though both halves
    /// are individually fine. Note this is about the DIRECTIVE, not about being
    /// strict: the same function inside already-strict code is legal, because
    /// there is no directive to apply retroactively.
    pub(crate) fn check_use_strict_with_non_simple_params(
        &self,
        params: &Params,
        body: &FnBody,
        pos: u32,
    ) -> PResult<()> {
        if !params.simple && body.directives.iter().any(|d| &*d.raw == "use strict") {
            return Err(SyntaxError::new(
                "SyntaxError: illegal 'use strict' directive in a function with a \
                 non-simple parameter list",
                pos,
            ));
        }
        Ok(())
    }

    /// 15.4.1: a getter's parameter list is empty (`get PropertyName ( )`) and a
    /// setter's is `PropertySetParameterList : FormalParameter` — exactly one,
    /// and a FormalParameter is a BindingElement, so it may carry a default but
    /// may NOT be a rest. Both are early errors, checked here for class members
    /// and object literals alike.
    pub(crate) fn check_accessor_arity(&self, is_get: bool, params: &Params) -> PResult<()> {
        if is_get {
            if !params.items.is_empty() {
                return Err(self.err_here("SyntaxError: getter must not have any formal parameters"));
            }
        } else if params.items.len() != 1 {
            return Err(
                self.err_here("SyntaxError: setter must have exactly one formal parameter")
            );
        } else if matches!(params.items[0], Pattern::Rest(_)) {
            return Err(self.err_here("SyntaxError: setter parameter may not be a rest parameter"));
        }
        Ok(())
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
        let (params, body) = self.parse_fn_tail(is_async, is_generator, true)?;
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

    /// 15.1.1: duplicate `BoundNames` in a parameter list are an error when the
    /// production is `UniqueFormalParameters` (every method and every arrow),
    /// when the list is not simple, or when the code is strict. `function
    /// f(a,a){}` stays legal in sloppy code -- and so, deliberately, do the
    /// GENERATOR and ASYNC declaration forms, which take plain
    /// `FormalParameters` despite looking like methods.
    ///
    /// A body-level `"use strict"` also triggers it, but that is not knowable
    /// here (the body has not been read yet) and a non-simple list already makes
    /// the directive itself an error, so the simple-list case is left to the
    /// compiler's own check.
    pub(crate) fn check_unique_params(
        &self,
        params: &Params,
        unique: bool,
        pos: u32,
    ) -> PResult<()> {
        if !(unique || !params.simple || self.ctx.strict) {
            return Ok(());
        }
        let mut names = Vec::new();
        for item in &params.items {
            super::stmt::collect_pattern_names(item, &mut names);
        }
        for i in 1..names.len() {
            if names[..i].contains(&names[i]) {
                return Err(SyntaxError::new(
                    format!(
                        "SyntaxError: duplicate parameter name '{}' not allowed in this context",
                        names[i]
                    ),
                    pos,
                ));
            }
        }
        Ok(())
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
                Pattern::Assign { left: Box::new(pat), right: Box::new(self.parse_assign_full()?) }
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

    /// The body, with the enclosing parameter list's `BoundNames` seeded into
    /// its scope as VAR-like bindings.
    ///
    /// That placement is the whole point: a parameter collides with the body's
    /// `LexicallyDeclaredNames` (`function f(a){ let a; }` is an error) but not
    /// with its `VarDeclaredNames` (`function f(a){ var a; }` is fine, and so is
    /// a body-level `function a(){}`). Recording them in `Scope::var` is exactly
    /// that asymmetry, and it is why they are recorded as var-like bindings.
    pub(crate) fn parse_fn_body_with_params(
        &mut self,
        params: Option<&Params>,
    ) -> PResult<FnBody> {
        self.expect(Punct::LBrace, true)?;
        let saved_strict = self.ctx.strict;
        let (directives, strict) = self.directive_prologue()?;
        if strict {
            self.ctx.strict = true;
        }
        self.scopes.push(ScopeKind::Function);
        if let Some(p) = params {
            let mut names = Vec::new();
            for item in &p.items {
                super::stmt::collect_pattern_names(item, &mut names);
            }
            for n in names {
                self.scopes.declare_param(&n);
            }
        }
        let mut stmts = Vec::new();
        while !self.at(Punct::RBrace) && !self.at_eof() {
            stmts.push(self.parse_stmt_list_item()?);
        }
        self.scopes.pop()?;
        self.expect(Punct::RBrace, false)?;
        let out = FnBody { directives, stmts };
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
                    Pattern::Assign { left: Box::new(p), right: Box::new(self.parse_assign_full()?) }
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
            // Whether the key would pass as a BindingIdentifier has to be
            // decided BEFORE it is consumed: a PropertyName may be any reserved
            // word (`{if: x}` is fine), so by the time the missing `:` reveals
            // this is shorthand the token that carried the keyword-ness is gone.
            let key_pos = self.cur().span.start;
            let key_bindable = self.is_binding_ident();
            let key = self.parse_prop_key()?;
            // Shorthand is "no colon", not "the key is an identifier" —
            // `{a: b}` has an identifier key and is NOT shorthand.
            let value = if self.eat(Punct::Colon, true)? {
                self.parse_binding_pattern()?
            } else {
                // Shorthand `{a}` / `{a = 1}`.
                let PropKey::Ident(n) = &key else {
                    return Err(self.err_here("SyntaxError: expected ':' in binding pattern"));
                };
                // `BindingProperty : SingleNameBinding` and
                // `SingleNameBinding : BindingIdentifier Initializer_opt` — the
                // shorthand form BINDS the name, so `var {if} = o` is an early
                // error, as is `var {await} = o` in a static block and
                // `var {eval} = o` under strict. The colon form escapes all of
                // this because its value is parsed by `parse_binding_pattern`.
                if !key_bindable {
                    return Err(SyntaxError::new(
                        format!("SyntaxError: '{n}' is not a valid binding name"),
                        key_pos,
                    ));
                }
                if self.ctx.strict && (&**n == "eval" || &**n == "arguments") {
                    return Err(SyntaxError::new(
                        format!("SyntaxError: '{n}' cannot be used as a binding in strict mode"),
                        key_pos,
                    ));
                }
                Pattern::Ident(n.clone())
            };
            let value = if self.eat(Punct::Eq, true)? {
                Pattern::Assign { left: Box::new(value), right: Box::new(self.parse_assign_full()?) }
            } else {
                value
            };
            props.push(PatternProp { key, value });
            if !self.eat(Punct::Comma, true)? {
                break;
            }
        }
        self.expect(Punct::RBrace, false)?;
        Ok(Pattern::Object { props, rest })
    }

    // ---- decorators --------------------------------------------------------

    /// `DecoratorList : DecoratorList_opt Decorator` — every `@…` at the current
    /// position, in SOURCE order (which is also their evaluation order).
    ///
    /// `Decorator` is deliberately NOT `LeftHandSideExpression`: the grammar
    /// admits only three shapes, so `@a[b]`, `@a.b(1).c` and `@a\`t\`` have no
    /// parse at all and must stay SyntaxErrors rather than becoming runtime
    /// TypeErrors.
    pub(crate) fn parse_decorators(&mut self) -> PResult<Vec<Expr>> {
        let mut out = Vec::new();
        while self.at(Punct::At) {
            self.bump_before_operand()?; // `@` — an operand follows
            out.push(self.parse_decorator()?);
        }
        Ok(out)
    }

    /// The DecoratorList for a ClassDeclaration about to be parsed: either one
    /// left pending by `@dec export …` or one written right here (`export @dec
    /// class …`). Returns the offset of its first `@` (so the class's
    /// [[SourceText]] can start there) or `None` when there are no decorators.
    pub(crate) fn take_class_decorators(&mut self) -> PResult<(Option<u32>, Vec<Expr>)> {
        if let Some((at, list)) = self.pending_class_decorators.take() {
            return Ok((Some(at), list));
        }
        if self.at(Punct::At) {
            let at = self.cur().span.start;
            return Ok((Some(at), self.parse_decorators()?));
        }
        Ok((None, Vec::new()))
    }

    /// One `Decorator`:
    ///   `@ DecoratorMemberExpression`         — `a`, `a.b`, `a.#p`, `a.b.c`
    ///   `@ DecoratorParenthesizedExpression`  — `( Expression )`
    ///   `@ DecoratorCallExpression`           — `a.b(args)`
    fn parse_decorator(&mut self) -> PResult<Expr> {
        if self.at(Punct::LParen) {
            // `@ ( Expression )` — the one form that admits arbitrary syntax.
            // Parsed as the full comma-Expression the production names, so
            // `@(a, b)` takes `b`; the `[In]` restriction of an enclosing `for`
            // head does not reach inside the parens.
            self.bump_before_operand()?;
            let saved_in = self.ctx.in_;
            self.ctx.in_ = true;
            let e = self.parse_expr()?;
            self.ctx.in_ = saved_in;
            self.expect(Punct::RParen, false)?;
            return Ok(e);
        }
        // DecoratorMemberExpression: an IdentifierReference followed by any
        // number of `.IdentifierName` / `.PrivateIdentifier` hops.
        let (name, _) = self.binding_ident_or_reference()?;
        let mut e = Expr::Ident(name);
        while self.at(Punct::Dot) {
            self.bump_after_operand()?;
            let prop = self.member_prop_public()?;
            e = Expr::Member(Box::new(Member { object: e, prop, optional: false }));
        }
        // …optionally ONE argument list, and nothing after it: `@a.b(1)` is a
        // DecoratorCallExpression, `@a(1).b` and `@a(1)(2)` are not.
        if self.at(Punct::LParen) {
            let args = self.parse_args_public()?;
            e = Expr::Call(Box::new(CallExpr { callee: e, args, optional: false }));
        }
        Ok(e)
    }

    // ---- classes -----------------------------------------------------------

    /// A class after its `class` keyword. A class body is ALWAYS strict, even
    /// inside sloppy code.
    pub(crate) fn parse_class_rest(&mut self, start: u32) -> PResult<Class> {
        self.parse_class_rest_dec(start, Vec::new())
    }

    /// `parse_class_rest` for a class that carried a DecoratorList before its
    /// `class` keyword (already parsed by the caller, since the list is what
    /// told the caller a class was coming).
    pub(crate) fn parse_class_rest_dec(
        &mut self,
        start: u32,
        decorators: Vec<Expr>,
    ) -> PResult<Class> {
        let saved_strict = self.ctx.strict;
        self.ctx.strict = true;

        let name = if self.is_binding_ident() { Some(self.binding_ident()?.0) } else { None };
        let superclass = if self.eat_kw(Keyword::Extends, true)? {
            let sup = self.parse_lhs_public()?;
            // `ClassHeritage : extends LeftHandSideExpression`. An ArrowFunction
            // is an AssignmentExpression and no LHS, so `class extends () => {}
            // {}` has no parse at all — but the LHS parser reaches one anyway,
            // because the `(` that starts it is a CoverParenthesizedExpression
            // until the `=>` arrives. Parenthesized (`extends (() => {})`) IS a
            // PrimaryExpression and stays legal (a runtime TypeError).
            if matches!(sup, Expr::Arrow(_)) && !self.take_parenthesized() {
                return Err(self.err_here(
                    "SyntaxError: an arrow function is not a valid class heritage expression",
                ));
            }
            Some(Box::new(sup))
        } else {
            None
        };
        let derived = superclass.is_some();

        self.expect(Punct::LBrace, true)?;
        // `[In]` restores inside the braces — see `parse_args`. Nothing a
        // ClassBody contains inherits it: a computed key is
        // `AssignmentExpression[+In]`, a field's is `Initializer[+In]`, and a
        // method body or static block is its own statement region. Without this
        // `for (var C = class { ['a' in o](){} };;)` died on the `in`, because a
        // `for` head parses with `[~In]` and the class never turned it back on.
        let saved_in = self.ctx.in_;
        self.ctx.in_ = true;
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
        let body_end = self.cur().span.start;
        self.expect(Punct::RBrace, false)?;
        self.ctx.in_ = saved_in;
        self.ctx.strict = saved_strict;
        Self::check_class_body(&body, body_end)?;
        Ok(Class {
            name,
            superclass,
            body,
            span: Span::new(start, self.prev_end()),
            decorators,
        })
    }

    /// §15.7.1 ClassBody early errors, plus §15.4.1's constructor rules. All of
    /// them read only the finished member list, so they run once here rather
    /// than being smeared across `parse_class_member`.
    ///
    /// The three that are easy to get subtly wrong:
    ///
    ///   * PropName is the STRING VALUE of the key, so `'prototype'` and
    ///     `prototype` are the same name. A computed key has no PropName and is
    ///     exempt — `class C { static ['prototype'](){} }` is legal.
    ///   * A private name may repeat exactly once, and only as a getter/setter
    ///     PAIR of matching staticness. Any other repeat — two getters, a getter
    ///     and a method, an instance/static pair — is an error.
    ///   * `#constructor` is banned as a ClassElementName even though
    ///     `constructor` is fine, and even though the two never collide.
    fn check_class_body(body: &[ClassMember], pos: u32) -> PResult<()> {
        let err = |msg: String| Err(SyntaxError::new(msg, pos));

        // PrivateBoundIdentifiers, in source order: (name, is_static, kind).
        let mut privates: Vec<(&str, bool, MethodKind)> = Vec::new();
        let mut ctor_count = 0usize;

        for m in body {
            let (key, is_static, kind, special) = match m {
                ClassMember::Method(cm) => (
                    &cm.key,
                    cm.is_static,
                    cm.kind,
                    // SpecialMethod: a getter, a setter, a generator or an async
                    // function. Only a plain method may be named `constructor`.
                    cm.kind == MethodKind::Get
                        || cm.kind == MethodKind::Set
                        || cm.func.is_generator
                        || cm.func.is_async,
                ),
                ClassMember::Field(cf) => (&cf.key, cf.is_static, MethodKind::Method, false),
                ClassMember::StaticBlock(_) => continue,
            };
            let is_field = matches!(m, ClassMember::Field(_));

            if let PropKey::Private(n) = key {
                if &**n == "constructor" {
                    return err(
                        "SyntaxError: '#constructor' is not a valid private name".into()
                    );
                }
                privates.push((n, is_static, if is_field { MethodKind::Method } else { kind }));
                continue;
            }

            let Some(name) = prop_name(key) else { continue };
            if is_static && name == "prototype" {
                return err(
                    "SyntaxError: a class may not have a static member named 'prototype'".into()
                );
            }
            if is_field {
                // A field named `constructor` is banned in both placements; a
                // STATIC field additionally may not be named `prototype` (caught
                // above, which covers methods too).
                if name == "constructor" {
                    return err(
                        "SyntaxError: a class field may not be named 'constructor'".into()
                    );
                }
                continue;
            }
            if !is_static && name == "constructor" {
                if special {
                    return err(
                        "SyntaxError: the class constructor may not be a getter, setter, \
                         generator or async method".into()
                    );
                }
                ctor_count += 1;
            }
        }

        if ctor_count > 1 {
            return err("SyntaxError: a class may only have one constructor".into());
        }

        for (i, (n, st, k)) in privates.iter().enumerate() {
            let mut others = privates
                .iter()
                .enumerate()
                .filter(|(j, (m, _, _))| *j != i && m == n)
                .map(|(_, e)| e);
            let Some((_, ost, ok)) = others.next() else { continue };
            // More than two entries, or a second entry that does not complete a
            // same-staticness accessor pair.
            let paired = others.next().is_none()
                && ost == st
                && matches!(
                    (k, ok),
                    (MethodKind::Get, MethodKind::Set) | (MethodKind::Set, MethodKind::Get)
                );
            if !paired {
                return err(format!(
                    "SyntaxError: private name '#{n}' has already been declared"
                ));
            }
        }
        Ok(())
    }

    /// ClassHeritage is a `LeftHandSideExpression`, so `extends a.b` binds the
    /// member, while `extends a + b` and `extends a => a` are syntax errors.
    /// Parsing it as an AssignmentExpression accepted both, and turned
    /// `class C extends () => {} {}` into a runtime TypeError instead.
    fn parse_lhs_public(&mut self) -> PResult<Expr> {
        self.parse_lhs()
    }

    fn parse_class_member(&mut self, derived: bool) -> PResult<ClassMember> {
        let dec_start = self.cur().span.start;
        // `ClassElement : DecoratorList_opt static_opt MethodDefinition` — the
        // list precedes `static`, and a ClassStaticBlock has no DecoratorList
        // production at all.
        let decorators = self.parse_decorators()?;
        // The member's [[SourceText]] starts at the `static`/name, not at the
        // decorators: `Function.prototype.toString` on a decorated method returns
        // the MethodDefinition, and `method_source`'s `static ` strip below
        // assumes the slice begins there.
        let start = self.cur().span.start;
        let mut m = self.parse_class_member_inner(derived, start)?;
        if !decorators.is_empty() {
            match &mut m {
                ClassMember::Method(cm) => {
                    // §15.7.1: `It is a Syntax Error if … ClassElement is
                    // DecoratorList MethodDefinition and PropName of
                    // MethodDefinition is "constructor"`. Decorating the
                    // constructor has no meaning — the class decorator is the
                    // hook for the whole class.
                    if cm.kind == MethodKind::Constructor {
                        return Err(SyntaxError::new(
                            "SyntaxError: a class constructor may not be decorated",
                            dec_start,
                        ));
                    }
                    cm.decorators = decorators;
                }
                ClassMember::Field(cf) => cf.decorators = decorators,
                ClassMember::StaticBlock(_) => {
                    return Err(SyntaxError::new(
                        "SyntaxError: a class static block may not be decorated",
                        dec_start,
                    ))
                }
            }
        }
        Ok(m)
    }

    fn parse_class_member_inner(
        &mut self,
        derived: bool,
        start: u32,
    ) -> PResult<ClassMember> {
        let is_static = if self.at_kw(Keyword::Static) {
            let save = self.save();
            self.bump_after_operand()?;
            // `static` may itself be the member NAME: a field
            // (`static = 1`, `static;`) or a method (`static() {}` —
            // fast-glob ships one). Only when something member-shaped
            // FOLLOWS is it the modifier.
            if self.at(Punct::Eq)
                || self.at(Punct::Semi)
                || self.at(Punct::RBrace)
                || self.at(Punct::LParen)
            {
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
            // A static block runs as a synthetic method (ClassStaticBlock-
            // DefinitionEvaluation builds one), so it is function code and
            // `new.target` is legal there — it evaluates to `undefined`. Without
            // this a class at SCRIPT top level, where nothing has turned the flag
            // on, rejected `static { v = new.target; }` outright.
            self.ctx.new_target = true;
            // `await` is RESERVED inside a ClassStaticBlock — both as an
            // expression and as a binding — even though the block is not async
            // and cannot await anything. Setting `await_` is what makes both
            // `await;` and `var await;` the SyntaxErrors they must be; without
            // it the first became a ReferenceError at runtime and the second was
            // silently accepted.
            self.ctx.await_ = true;
            // A static block is not a generator, so `yield` is an ordinary
            // identifier there unless the enclosing code is strict.
            self.ctx.yield_ = false;
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

        // `FieldDefinition : accessor [no LineTerminator here] ClassElementName
        // Initializer_opt` — an AUTO-ACCESSOR. `accessor` is contextual and may
        // be the member's own name, so it is the modifier only when a
        // ClassElementName follows it ON THE SAME LINE: `accessor = 1`,
        // `accessor;`, `accessor(){}` and `accessor \n $;` are all a member
        // NAMED `accessor` (the last by ASI, which is what
        // `field-definition-accessor-no-line-terminator` asserts).
        if self.at_kw(Keyword::Accessor) {
            let save = self.save();
            self.bump_after_operand()?;
            if !self.cur().newline_before && self.at_class_key_start() {
                return self.parse_auto_accessor(is_static, start);
            }
            self.restore(save);
        }

        // Accessors and modifiers, each of which may instead be a plain name.
        for (kw, kind) in [(Keyword::Get, MethodKind::Get), (Keyword::Set, MethodKind::Set)] {
            if self.at_kw(kw) {
                let save = self.save();
                self.bump_after_operand()?;
                if self.at_class_key_start() {
                    let key = self.parse_prop_key()?;
                    let func = self.parse_method_rest(false, false, start)?;
                    self.check_accessor_arity(kind == MethodKind::Get, &func.params)?;
                    return Ok(ClassMember::Method(ClassMethod {
                        key,
                        kind,
                        func: Box::new(func),
                        is_static,
                        decorators: Vec::new(),
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
            // PropName, not spelling: `class C { 'constructor'(){} }` defines the
            // constructor exactly as the bare identifier does, so `super()` is
            // legal in it and a second `constructor` beside it is a duplicate.
            let is_ctor = !is_static
                && !is_async
                && !is_generator
                && prop_name(&key) == Some("constructor");
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
                decorators: Vec::new(),
            }));
        }

        // A field. Its initializer runs in a context where `arguments` is an
        // early error and `return` is illegal.
        let value = if self.eat(Punct::Eq, true)? {
            let saved = self.ctx;
            self.ctx.in_field_init = true;
            self.ctx.return_ = false;
            self.ctx.super_prop = true;
            // Like a static block, an initializer is the body of a synthetic
            // method, so `new.target` is legal (and `undefined`) here too.
            self.ctx.new_target = true;
            self.field_init_await(is_static);
            let v = self.parse_assign_full()?;
            self.ctx = saved;
            Some(v)
        } else {
            None
        };
        self.semicolon()?;
        Ok(ClassMember::Field(ClassField { key, value, is_static, accessor: None, decorators: Vec::new() }))
    }

    /// The `[Await]` parameter for a FieldDefinition's Initializer.
    ///
    /// The production is `ClassElementName[?Yield, ?Await] Initializer[+In,
    /// ~Yield, ~Await]`, so an INSTANCE field's initializer does NOT inherit the
    /// enclosing function's `await`: inside `async function f(){ … }`,
    /// `class { x = await; }` is a plain IdentifierReference and must parse
    /// (staging/sm/fields/await-identifier-script.js), while `x = await 1` is
    /// still a SyntaxError because `await` is then just an identifier followed by
    /// a number.
    ///
    /// A STATIC field's initializer keeps `await` RESERVED — the same rule the
    /// ClassStaticBlock arm above implements, and unconditional: node rejects
    /// `class { static y = await; }` even in a plain sloppy Script.
    fn field_init_await(&mut self, is_static: bool) {
        self.ctx.await_ = is_static;
    }

    /// `accessor ClassElementName Initializer_opt`, with the `accessor` keyword
    /// already consumed. Produces ONE member: a private backing field plus the
    /// get/set pair that reads and writes it.
    fn parse_auto_accessor(&mut self, is_static: bool, start: u32) -> PResult<ClassMember> {
        let key = self.parse_prop_key()?;
        let value = if self.eat(Punct::Eq, true)? {
            let saved = self.ctx;
            self.ctx.in_field_init = true;
            self.ctx.return_ = false;
            self.ctx.super_prop = true;
            self.ctx.new_target = true;
            self.field_init_await(is_static);
            let v = self.parse_assign_full()?;
            self.ctx = saved;
            Some(v)
        } else {
            None
        };
        self.semicolon()?;
        let span = Span::new(start, self.prev_end());
        // `@` is not an identifier character, so no source can spell this name
        // and no user private can collide with it. The counter runs across the
        // whole parse, not per class, so `class B extends A` — where both
        // declare `accessor x` — gets two slots on the instance rather than one
        // shared one.
        self.accessor_seq += 1;
        let storage: Name = format!("#accessor@{}", self.accessor_seq).into_boxed_str();
        let slot = |p: &Name| -> Expr {
            Expr::Member(Box::new(Member {
                object: Expr::This,
                prop: MemberProp::Private(p.clone()),
                optional: false,
            }))
        };
        let body = |stmts: Vec<Stmt>| FnBody { directives: Vec::new(), stmts };
        let getter = Function {
            name: None,
            params: Params { items: Vec::new(), simple: true },
            body: body(vec![Stmt::Return(Some(slot(&storage)))]),
            is_async: false,
            is_generator: false,
            span,
        };
        let setter_param: Name = "value".into();
        let setter = Function {
            name: None,
            params: Params { items: vec![Pattern::Ident(setter_param.clone())], simple: true },
            body: body(vec![Stmt::Expr(Expr::Assign {
                op: AssignOp::Assign,
                target: Target::Member(Box::new(Member {
                    object: Expr::This,
                    prop: MemberProp::Private(storage.clone()),
                    optional: false,
                })),
                value: Box::new(Expr::Ident(setter_param)),
                covered: false,
            })]),
            is_async: false,
            is_generator: false,
            span,
        };
        Ok(ClassMember::Field(ClassField {
            key,
            value,
            is_static,
            accessor: Some(Box::new(AutoAccessor {
                storage,
                getter: Box::new(getter),
                setter: Box::new(setter),
            })),
            decorators: Vec::new(),
        }))
    }

    fn at_class_key_start(&self) -> bool {
        matches!(
            self.cur().kind,
            super::token::TokenKind::Ident { .. }
                | super::token::TokenKind::Str(_)
                | super::token::TokenKind::Num(_)
                // `NumericLiteral : DecimalBigIntegerLiteral` — see
                // `at_property_name_start`; `class C { get 5n(){} }` needs it too.
                | super::token::TokenKind::BigInt(_)
        ) || self.at(Punct::LBracket)
    }

    /// Bind a declaration's name in the current scope, applying the duplicate
    /// rules.
    pub(crate) fn declare(&mut self, name: &str, kind: BindKind, pos: u32) -> PResult<()> {
        match kind {
            BindKind::Var => self.scopes.declare_var(name, pos),
            // A FunctionDeclaration is VAR-scoped only where the spec's
            // TopLevelVarDeclaredNames applies — a Script, a FunctionBody or a
            // ClassStaticBlock. In a Block, a CaseBlock, a catch body, or at a
            // MODULE's top level it is a LEXICAL declaration of the current
            // scope.
            //
            // Routing it to `declare_var` everywhere made it walk outward to the
            // nearest var boundary and collide with lexical bindings it can
            // never actually reach: `let f; { function f(){} }` is legal (the
            // inner `f` is block-scoped, and Annex B simply skips the
            // var-hoisting when it would collide — which is exactly what the
            // `*-skip-early-err*` test family asserts), and it was rejected.
            BindKind::Function | BindKind::GenFunction
                if self.scopes.fn_decl_is_var_scoped() =>
            {
                self.scopes.declare_var(name, pos)
            }
            // Annex B.3.2.4/B.3.2.5: in a Block or CaseBlock, two
            // FunctionDeclarations of the same name are legal in SLOPPY code and
            // an error under strict. A generator/async declaration is not a
            // FunctionDeclaration, so it never opts in.
            BindKind::Function => {
                self.scopes.declare_lexical(name, kind, pos, !self.ctx.strict)
            }
            _ => self.scopes.declare_lexical(name, kind, pos, false),
        }
    }
}
