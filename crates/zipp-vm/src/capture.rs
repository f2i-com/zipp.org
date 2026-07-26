//! Free-variable analysis for closures.
//!
//! Two questions drive closure compilation, both answered by a pure walk of the
//! oxc AST (no register/compiler state):
//!
//! * **`free_vars(fn)`** — names a function references but does not bind itself,
//!   propagated up through nested functions. A name free in an inner function
//!   but not bound by a middle function stays free in the middle function too,
//!   so every level on the path between a capture and its definition lists it.
//!   This is what lets upvalue sourcing resolve one level at a time.
//!
//! * **`captured_locals(fn)`** — the subset of a function's OWN bindings
//!   (params + local declarations + nested function names) that appear free in
//!   some directly-nested function. Those bindings must be boxed into heap cells
//!   at declaration so the closure and the defining scope share one mutable
//!   slot. Bindings not captured stay in plain registers (the fast path).
//!
//! The walk covers the compiled subset; an unknown node simply contributes no
//! bindings/refs, which is conservative-safe (a missed capture would surface as
//! a failing test, not silent corruption, because resolution falls back to a
//! global lookup).

use std::collections::HashSet;

use oxc_ast::ast as ox;

/// Names a function body references but does not bind (propagated through nested
/// functions). `params` are the function's parameters.
pub fn free_vars(params: &[String], body: &[ox::Statement]) -> HashSet<String> {
    let mut refs = HashSet::new();
    let mut bound: HashSet<String> = params.iter().cloned().collect();
    collect_bound_in_body(body, &mut bound);
    for s in body {
        stmt_refs(s, &mut refs);
    }
    refs.retain(|n| !bound.contains(n));
    refs
}

/// Whether any parameter DEFAULT expression references `name` (free) — used
/// to detect a possible direct eval in the parameter scope.
pub fn params_reference(name: &str, params: &ox::FormalParameters) -> bool {
    let mut refs = HashSet::new();
    for item in &params.items {
        if let Some(init) = &item.initializer {
            expr_refs(init, &mut refs);
        }
        pattern_init_refs(&item.pattern, &mut refs);
    }
    if let Some(r) = &params.rest {
        pattern_init_refs(&r.rest.argument, &mut refs);
    }
    refs.contains(name)
}

/// Collect every name referenced by a DEFAULT-VALUE expression nested anywhere
/// inside a binding pattern (array/object destructuring element defaults).
fn pattern_init_refs(pat: &ox::BindingPattern, out: &mut HashSet<String>) {
    use ox::BindingPattern as P;
    match pat {
        P::BindingIdentifier(_) => {}
        P::AssignmentPattern(ap) => {
            expr_refs(&ap.right, out);
            pattern_init_refs(&ap.left, out);
        }
        P::ObjectPattern(op) => {
            for prop in &op.properties {
                // A computed key `{[expr]: v}` evaluates in the param scope —
                // an eval there introduces vars like an element default does.
                if prop.computed {
                    if let Some(ke) = prop.key.as_expression() {
                        expr_refs(ke, out);
                    }
                }
                pattern_init_refs(&prop.value, out);
            }
            if let Some(rest) = &op.rest {
                pattern_init_refs(&rest.argument, out);
            }
        }
        P::ArrayPattern(arr) => {
            for el in arr.elements.iter().flatten() {
                pattern_init_refs(el, out);
            }
            if let Some(rest) = &arr.rest {
                pattern_init_refs(&rest.argument, out);
            }
        }
    }
}

/// Names a parameter list's DEFAULT VALUES reference freely.
///
/// A default is evaluated in the callee's own scope but can close over the
/// ENCLOSING one, so `function outer(){ const G=1; function i(r=G){} }` makes
/// `G` a captured variable of `outer` just as surely as reading it in the body
/// would. `fn_node_free`/`arrow_free` used to scan only the body, so a name
/// referenced ONLY from a default was never boxed into a cell, the nested
/// function had nothing to capture, and the default threw "G is not defined" at
/// runtime. Minified bundles hit this constantly — `function J(r=G){...}` is a
/// standard shape for a defaulted config argument.
///
/// The parameter names themselves are treated as bound: a default referring to
/// an earlier parameter is not a capture of anything outer.
fn params_free(p: &ox::FormalParameters, bound: &[String], out: &mut HashSet<String>) {
    let mut refs = HashSet::new();
    for item in &p.items {
        if let Some(init) = &item.initializer {
            expr_refs(init, &mut refs);
        }
        pattern_init_refs(&item.pattern, &mut refs);
    }
    if let Some(r) = &p.rest {
        pattern_init_refs(&r.rest.argument, &mut refs);
    }
    for name in refs {
        if !bound.iter().any(|b| *b == name) {
            out.insert(name);
        }
    }
}

/// The function's own bindings that some directly-nested function captures.
pub fn captured_locals(params: &[String], body: &[ox::Statement]) -> HashSet<String> {
    let mut bound: HashSet<String> = params.iter().cloned().collect();
    collect_bound_in_body(body, &mut bound);

    // Union of free vars of each directly-nested function.
    let mut nested_free = HashSet::new();
    for s in body {
        collect_nested_free(s, &mut nested_free);
    }
    bound.intersection(&nested_free).cloned().collect()
}

/// True when a nested ARROW (transitively, with no intervening ordinary function
/// — ordinary functions bind their own `arguments`, see `fn_node_free`) references
/// `arguments`. The nearest enclosing ordinary function must then materialize and
/// box its `arguments` object so the arrow can capture it lexically as an upvalue.
pub fn nested_uses_arguments(body: &[ox::Statement]) -> bool {
    let mut nested_free = HashSet::new();
    for s in body {
        collect_nested_free(s, &mut nested_free);
    }
    nested_free.contains("arguments")
}

// ── bound-name collection (this scope only; does NOT descend into nested fns) ──

fn collect_bound_in_body(body: &[ox::Statement], out: &mut HashSet<String>) {
    for s in body {
        collect_bound_stmt(s, out);
    }
}

fn collect_bound_stmt(s: &ox::Statement, out: &mut HashSet<String>) {
    use ox::Statement as S;
    match s {
        S::VariableDeclaration(d) => {
            for decl in &d.declarations {
                collect_pattern_names(&decl.id, out);
            }
        }
        S::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                out.insert(id.name.to_string());
            }
        }
        // A class declaration is a lexical binding like `let`/`const`: a nested
        // closure that captures it must see it boxed (so a forward-materialised
        // function can hold its cell).
        S::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                out.insert(id.name.to_string());
            }
        }
        // Recurse into nested *statements* (blocks, loops, if) but NOT into
        // nested function bodies — those introduce their own scope.
        S::BlockStatement(b) => collect_bound_in_body(&b.body, out),
        S::IfStatement(i) => {
            collect_bound_stmt(&i.consequent, out);
            if let Some(a) = &i.alternate {
                collect_bound_stmt(a, out);
            }
        }
        S::WhileStatement(w) => collect_bound_stmt(&w.body, out),
        S::DoWhileStatement(d) => collect_bound_stmt(&d.body, out),
        S::ForStatement(f) => {
            if let Some(ox::ForStatementInit::VariableDeclaration(d)) = &f.init {
                for decl in &d.declarations {
                    collect_pattern_names(&decl.id, out);
                }
            }
            collect_bound_stmt(&f.body, out);
        }
        // for-of / for-in declare their loop variable too, so a closure that
        // captures it must see it boxed.
        S::ForOfStatement(f) => {
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                for decl in &d.declarations {
                    collect_pattern_names(&decl.id, out);
                }
            }
            collect_bound_stmt(&f.body, out);
        }
        S::ForInStatement(f) => {
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                for decl in &d.declarations {
                    collect_pattern_names(&decl.id, out);
                }
            }
            collect_bound_stmt(&f.body, out);
        }
        // Descend into try/switch/labeled bodies so a binding declared inside one
        // that a nested closure captures is detected (and boxed).
        S::TryStatement(t) => {
            collect_bound_in_body(&t.block.body, out);
            if let Some(h) = &t.handler {
                collect_bound_in_body(&h.body.body, out);
            }
            if let Some(f) = &t.finalizer {
                collect_bound_in_body(&f.body, out);
            }
        }
        S::SwitchStatement(sw) => {
            for case in &sw.cases {
                collect_bound_in_body(&case.consequent, out);
            }
        }
        S::LabeledStatement(l) => collect_bound_stmt(&l.body, out),
        // `var` declarations inside a `with` body hoist to the enclosing fn.
        S::WithStatement(w) => collect_bound_stmt(&w.body, out),
        _ => {}
    }
}

/// Insert every name a binding pattern introduces — recursing through object/
/// array destructuring, defaults (`= d`), and rest elements — so a destructured
/// local captured by a nested closure is detected and boxed.
pub(crate) fn collect_pattern_names(pat: &ox::BindingPattern, out: &mut HashSet<String>) {
    use ox::BindingPattern as P;
    match pat {
        P::BindingIdentifier(id) => {
            out.insert(id.name.to_string());
        }
        P::AssignmentPattern(ap) => collect_pattern_names(&ap.left, out),
        P::ObjectPattern(op) => {
            for prop in &op.properties {
                collect_pattern_names(&prop.value, out);
            }
            if let Some(rest) = &op.rest {
                collect_pattern_names(&rest.argument, out);
            }
        }
        P::ArrayPattern(arr) => {
            for el in arr.elements.iter().flatten() {
                collect_pattern_names(el, out);
            }
            if let Some(rest) = &arr.rest {
                collect_pattern_names(&rest.argument, out);
            }
        }
    }
}

// ── reference collection (descends into nested functions) ──

fn stmt_refs(s: &ox::Statement, out: &mut HashSet<String>) {
    use ox::Statement as S;
    match s {
        S::ExpressionStatement(e) => expr_refs(&e.expression, out),
        // The with OBJECT expression and every reference in the body count
        // (an outer local referenced only inside a with body must be captured).
        S::WithStatement(w) => {
            expr_refs(&w.object, out);
            stmt_refs(&w.body, out);
        }
        S::VariableDeclaration(d) => {
            for decl in &d.declarations {
                if let Some(init) = &decl.init {
                    expr_refs(init, out);
                }
            }
        }
        S::BlockStatement(b) => {
            for st in &b.body {
                stmt_refs(st, out);
            }
        }
        S::IfStatement(i) => {
            expr_refs(&i.test, out);
            stmt_refs(&i.consequent, out);
            if let Some(a) = &i.alternate {
                stmt_refs(a, out);
            }
        }
        S::WhileStatement(w) => {
            expr_refs(&w.test, out);
            stmt_refs(&w.body, out);
        }
        S::DoWhileStatement(d) => {
            stmt_refs(&d.body, out);
            expr_refs(&d.test, out);
        }
        S::ForStatement(f) => {
            if let Some(init) = &f.init {
                match init {
                    ox::ForStatementInit::VariableDeclaration(d) => {
                        for decl in &d.declarations {
                            if let Some(i) = &decl.init {
                                expr_refs(i, out);
                            }
                        }
                    }
                    other => {
                        if let Some(e) = other.as_expression() {
                            expr_refs(e, out);
                        }
                    }
                }
            }
            if let Some(t) = &f.test {
                expr_refs(t, out);
            }
            if let Some(u) = &f.update {
                expr_refs(u, out);
            }
            stmt_refs(&f.body, out);
        }
        S::ReturnStatement(r) => {
            if let Some(a) = &r.argument {
                expr_refs(a, out);
            }
        }
        S::FunctionDeclaration(f) => {
            // A nested function declaration contributes its OWN free vars
            // (minus what it binds), exactly like a function expression would.
            fn_node_free(&f.params, f.body.as_deref(), out);
        }
        S::ForOfStatement(f) => {
            expr_refs(&f.right, out);
            stmt_refs(&f.body, out);
        }
        S::ForInStatement(f) => {
            expr_refs(&f.right, out);
            stmt_refs(&f.body, out);
        }
        S::TryStatement(t) => {
            for st in &t.block.body {
                stmt_refs(st, out);
            }
            if let Some(h) = &t.handler {
                for st in &h.body.body {
                    stmt_refs(st, out);
                }
            }
            if let Some(f) = &t.finalizer {
                for st in &f.body {
                    stmt_refs(st, out);
                }
            }
        }
        S::SwitchStatement(sw) => {
            expr_refs(&sw.discriminant, out);
            for case in &sw.cases {
                if let Some(t) = &case.test {
                    expr_refs(t, out);
                }
                for st in &case.consequent {
                    stmt_refs(st, out);
                }
            }
        }
        S::ThrowStatement(t) => expr_refs(&t.argument, out),
        S::LabeledStatement(l) => stmt_refs(&l.body, out),
        S::ClassDeclaration(c) => class_free(c, out),
        _ => {}
    }
}

fn expr_refs(e: &ox::Expression, out: &mut HashSet<String>) {
    use ox::Expression as E;
    match e {
        E::Identifier(id) => {
            if id.name != "undefined" {
                out.insert(id.name.to_string());
            }
        }
        E::ParenthesizedExpression(p) => expr_refs(&p.expression, out),
        E::BinaryExpression(b) => {
            expr_refs(&b.left, out);
            expr_refs(&b.right, out);
        }
        E::LogicalExpression(l) => {
            expr_refs(&l.left, out);
            expr_refs(&l.right, out);
        }
        E::UnaryExpression(u) => expr_refs(&u.argument, out),
        E::UpdateExpression(u) => {
            match &u.argument {
                ox::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
                    out.insert(id.name.to_string());
                }
                // `o.x++` / `o.#x++` / `o[k]++`: the OBJECT (and key) are reads
                // an enclosing arrow/function must capture.
                ox::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                    expr_refs(&m.object, out)
                }
                ox::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                    expr_refs(&m.object, out);
                    expr_refs(&m.expression, out);
                }
                ox::SimpleAssignmentTarget::PrivateFieldExpression(p) => {
                    expr_refs(&p.object, out)
                }
                _ => {}
            }
        }
        E::AssignmentExpression(a) => {
            match &a.left {
                ox::AssignmentTarget::AssignmentTargetIdentifier(id) => {
                    out.insert(id.name.to_string());
                }
                ox::AssignmentTarget::StaticMemberExpression(m) => expr_refs(&m.object, out),
                ox::AssignmentTarget::ComputedMemberExpression(m) => {
                    expr_refs(&m.object, out);
                    expr_refs(&m.expression, out);
                }
                // `o.#x = v`: the object is a read to capture.
                ox::AssignmentTarget::PrivateFieldExpression(p) => {
                    expr_refs(&p.object, out)
                }
                _ => {}
            }
            expr_refs(&a.right, out);
        }
        E::ConditionalExpression(c) => {
            expr_refs(&c.test, out);
            expr_refs(&c.consequent, out);
            expr_refs(&c.alternate, out);
        }
        E::CallExpression(c) => {
            expr_refs(&c.callee, out);
            for arg in &c.arguments {
                if let Some(e) = arg_expr(arg) {
                    expr_refs(e, out);
                }
            }
        }
        E::NewExpression(n) => {
            expr_refs(&n.callee, out);
            for arg in &n.arguments {
                if let Some(e) = arg_expr(arg) {
                    expr_refs(e, out);
                }
            }
        }
        E::AwaitExpression(a) => expr_refs(&a.argument, out),
        E::YieldExpression(y) => {
            if let Some(a) = &y.argument {
                expr_refs(a, out);
            }
        }
        E::SequenceExpression(s) => {
            for e in &s.expressions {
                expr_refs(e, out);
            }
        }
        E::TemplateLiteral(t) => {
            for e in &t.expressions {
                expr_refs(e, out);
            }
        }
        E::TaggedTemplateExpression(t) => {
            expr_refs(&t.tag, out);
            for e in &t.quasi.expressions {
                expr_refs(e, out);
            }
        }
        E::StaticMemberExpression(m) => expr_refs(&m.object, out),
        E::PrivateFieldExpression(p) => expr_refs(&p.object, out),
        E::ComputedMemberExpression(m) => {
            expr_refs(&m.object, out);
            expr_refs(&m.expression, out);
        }
        E::ArrayExpression(a) => {
            for el in &a.elements {
                if let Some(e) = array_el_expr(el) {
                    expr_refs(e, out);
                }
            }
        }
        E::ObjectExpression(o) => {
            for prop in &o.properties {
                match prop {
                    ox::ObjectPropertyKind::ObjectProperty(p) => {
                        // A computed key `{[expr]: v}` references variables too —
                        // they must be captured, not just the value's.
                        if p.computed {
                            if let Some(ke) = p.key.as_expression() {
                                expr_refs(ke, out);
                            }
                        }
                        expr_refs(&p.value, out);
                    }
                    ox::ObjectPropertyKind::SpreadProperty(s) => expr_refs(&s.argument, out),
                }
            }
        }
        E::FunctionExpression(f) => fn_node_free(&f.params, f.body.as_deref(), out),
        E::ArrowFunctionExpression(a) => arrow_free(a, out),
        E::ClassExpression(c) => class_free(c, out),
        _ => {}
    }
}

/// Add a nested function's free variables to `out`. The nested function's own
/// bindings are subtracted, so only names it captures from further out remain.
fn fn_node_free(
    params: &ox::FormalParameters,
    body: Option<&ox::FunctionBody>,
    out: &mut HashSet<String>,
) {
    let mut param_names = param_names(params);
    // An ordinary function BINDS its own `arguments` (and `this`), so a reference
    // to `arguments` inside it is NOT free — it must not leak out as a capture of
    // the enclosing scope. (Arrows, handled by `arrow_free`, do not bind it.)
    param_names.push("arguments".to_string());
    let stmts: &[ox::Statement] = match body {
        Some(b) => &b.statements,
        None => &[],
    };
    let inner = free_vars(&param_names, stmts);
    out.extend(inner);
    params_free(params, &param_names, out);
}

fn arrow_free(a: &ox::ArrowFunctionExpression, out: &mut HashSet<String>) {
    let param_names = param_names(&a.params);
    let inner = free_vars(&param_names, &a.body.statements);
    out.extend(inner);
    params_free(&a.params, &param_names, out);
}

/// The operand expression of a call/new argument, including the spread case
/// (`f(...xs)`): `as_expression()` returns None for a SpreadElement, which would
/// drop a variable referenced ONLY inside a spread (so it wouldn't be captured).
fn arg_expr<'a>(a: &'a ox::Argument<'a>) -> Option<&'a ox::Expression<'a>> {
    match a {
        ox::Argument::SpreadElement(s) => Some(&s.argument),
        _ => a.as_expression(),
    }
}

/// The operand expression of an array element, including the spread (`[...xs]`).
fn array_el_expr<'a>(e: &'a ox::ArrayExpressionElement<'a>) -> Option<&'a ox::Expression<'a>> {
    match e {
        ox::ArrayExpressionElement::SpreadElement(s) => Some(&s.argument),
        _ => e.as_expression(),
    }
}

/// Names a class body references from OUTSIDE a method's own scope: each
/// method/getter/setter/constructor's free vars (minus its own params), plus
/// field initializers, computed keys, static blocks, and the `extends`
/// expression. The methods/ctor/field-inits are NESTED functions, so an
/// enclosing local they reference must be boxed — hence this feeds both
/// `captured_locals` (boxing) and `free_vars` (transitive capture). Over-
/// inclusion is harmless: it at most boxes an enclosing local used only
/// directly, which is transparent.
fn class_free(class: &ox::Class, out: &mut HashSet<String>) {
    if let Some(sc) = &class.super_class {
        expr_refs(sc, out);
    }
    for el in &class.body.body {
        match el {
            ox::ClassElement::MethodDefinition(m) => {
                fn_node_free(&m.value.params, m.value.body.as_deref(), out);
                if m.computed {
                    if let Some(k) = m.key.as_expression() {
                        expr_refs(k, out);
                    }
                }
            }
            ox::ClassElement::PropertyDefinition(p) => {
                if let Some(v) = &p.value {
                    expr_refs(v, out);
                }
                if p.computed {
                    if let Some(k) = p.key.as_expression() {
                        expr_refs(k, out);
                    }
                }
            }
            ox::ClassElement::StaticBlock(b) => {
                out.extend(free_vars(&[], &b.body));
            }
            _ => {}
        }
    }
}

/// Union of free vars of functions nested DIRECTLY in `s` (one level down).
fn collect_nested_free(s: &ox::Statement, out: &mut HashSet<String>) {
    use ox::Statement as S;
    match s {
        S::ExpressionStatement(e) => collect_nested_free_expr(&e.expression, out),
        S::VariableDeclaration(d) => {
            for decl in &d.declarations {
                if let Some(init) = &decl.init {
                    collect_nested_free_expr(init, out);
                }
            }
        }
        S::BlockStatement(b) => {
            for st in &b.body {
                collect_nested_free(st, out);
            }
        }
        S::IfStatement(i) => {
            collect_nested_free_expr(&i.test, out);
            collect_nested_free(&i.consequent, out);
            if let Some(a) = &i.alternate {
                collect_nested_free(a, out);
            }
        }
        S::WhileStatement(w) => {
            collect_nested_free_expr(&w.test, out);
            collect_nested_free(&w.body, out);
        }
        S::WithStatement(w) => {
            collect_nested_free_expr(&w.object, out);
            collect_nested_free(&w.body, out);
        }
        S::DoWhileStatement(d) => {
            collect_nested_free(&d.body, out);
            collect_nested_free_expr(&d.test, out);
        }
        S::ForStatement(f) => {
            // A closure in the INIT (`for (let i = 0, f = () => i; …)`)
            // captures head bindings too — scan declarator initializers.
            if let Some(init) = &f.init {
                match init {
                    ox::ForStatementInit::VariableDeclaration(d) => {
                        for decl in &d.declarations {
                            if let Some(i) = &decl.init {
                                collect_nested_free_expr(i, out);
                            }
                        }
                    }
                    other => {
                        if let Some(e) = other.as_expression() {
                            collect_nested_free_expr(e, out);
                        }
                    }
                }
            }
            if let Some(t) = &f.test {
                collect_nested_free_expr(t, out);
            }
            if let Some(u) = &f.update {
                collect_nested_free_expr(u, out);
            }
            collect_nested_free(&f.body, out);
        }
        S::ForOfStatement(f) => {
            collect_nested_free_expr(&f.right, out);
            collect_nested_free(&f.body, out);
        }
        S::ForInStatement(f) => {
            collect_nested_free_expr(&f.right, out);
            collect_nested_free(&f.body, out);
        }
        S::ReturnStatement(r) => {
            if let Some(a) = &r.argument {
                collect_nested_free_expr(a, out);
            }
        }
        S::FunctionDeclaration(f) => fn_node_free(&f.params, f.body.as_deref(), out),
        S::TryStatement(t) => {
            for st in &t.block.body {
                collect_nested_free(st, out);
            }
            if let Some(h) = &t.handler {
                for st in &h.body.body {
                    collect_nested_free(st, out);
                }
            }
            if let Some(f) = &t.finalizer {
                for st in &f.body {
                    collect_nested_free(st, out);
                }
            }
        }
        S::SwitchStatement(sw) => {
            collect_nested_free_expr(&sw.discriminant, out);
            for case in &sw.cases {
                if let Some(t) = &case.test {
                    collect_nested_free_expr(t, out);
                }
                for st in &case.consequent {
                    collect_nested_free(st, out);
                }
            }
        }
        S::ThrowStatement(t) => collect_nested_free_expr(&t.argument, out),
        S::LabeledStatement(l) => collect_nested_free(&l.body, out),
        S::ClassDeclaration(c) => class_free(c, out),
        _ => {}
    }
}

fn collect_nested_free_expr(e: &ox::Expression, out: &mut HashSet<String>) {
    use ox::Expression as E;
    match e {
        E::FunctionExpression(f) => fn_node_free(&f.params, f.body.as_deref(), out),
        E::ArrowFunctionExpression(a) => arrow_free(a, out),
        E::ClassExpression(c) => class_free(c, out),
        E::ParenthesizedExpression(p) => collect_nested_free_expr(&p.expression, out),
        E::BinaryExpression(b) => {
            collect_nested_free_expr(&b.left, out);
            collect_nested_free_expr(&b.right, out);
        }
        E::LogicalExpression(l) => {
            collect_nested_free_expr(&l.left, out);
            collect_nested_free_expr(&l.right, out);
        }
        E::UnaryExpression(u) => collect_nested_free_expr(&u.argument, out),
        E::AssignmentExpression(a) => collect_nested_free_expr(&a.right, out),
        E::ConditionalExpression(c) => {
            collect_nested_free_expr(&c.test, out);
            collect_nested_free_expr(&c.consequent, out);
            collect_nested_free_expr(&c.alternate, out);
        }
        E::CallExpression(c) => {
            collect_nested_free_expr(&c.callee, out);
            for arg in &c.arguments {
                if let Some(e) = arg.as_expression() {
                    collect_nested_free_expr(e, out);
                }
            }
        }
        E::NewExpression(n) => {
            collect_nested_free_expr(&n.callee, out);
            for arg in &n.arguments {
                if let Some(e) = arg.as_expression() {
                    collect_nested_free_expr(e, out);
                }
            }
        }
        E::AwaitExpression(a) => collect_nested_free_expr(&a.argument, out),
        E::YieldExpression(y) => {
            if let Some(a) = &y.argument {
                collect_nested_free_expr(a, out);
            }
        }
        E::SequenceExpression(s) => {
            for e in &s.expressions {
                collect_nested_free_expr(e, out);
            }
        }
        E::TemplateLiteral(t) => {
            for e in &t.expressions {
                collect_nested_free_expr(e, out);
            }
        }
        E::TaggedTemplateExpression(t) => {
            collect_nested_free_expr(&t.tag, out);
            for e in &t.quasi.expressions {
                collect_nested_free_expr(e, out);
            }
        }
        E::StaticMemberExpression(m) => collect_nested_free_expr(&m.object, out),
        E::PrivateFieldExpression(p) => collect_nested_free_expr(&p.object, out),
        E::ComputedMemberExpression(m) => {
            collect_nested_free_expr(&m.object, out);
            collect_nested_free_expr(&m.expression, out);
        }
        E::ArrayExpression(a) => {
            for el in &a.elements {
                if let Some(e) = el.as_expression() {
                    collect_nested_free_expr(e, out);
                }
            }
        }
        E::ObjectExpression(o) => {
            for prop in &o.properties {
                match prop {
                    ox::ObjectPropertyKind::ObjectProperty(p) => {
                        if p.computed {
                            if let Some(ke) = p.key.as_expression() {
                                collect_nested_free_expr(ke, out);
                            }
                        }
                        collect_nested_free_expr(&p.value, out);
                    }
                    ox::ObjectPropertyKind::SpreadProperty(s) => {
                        collect_nested_free_expr(&s.argument, out)
                    }
                }
            }
        }
        _ => {}
    }
}

fn param_names(p: &ox::FormalParameters) -> Vec<String> {
    // Every name a parameter list binds — plain identifiers, destructuring-pattern
    // leaves, and the rest parameter — so a nested function's own params shadow
    // outer bindings (they must NOT be reported as free / captured).
    let mut set = HashSet::new();
    for item in &p.items {
        collect_pattern_names(&item.pattern, &mut set);
    }
    if let Some(r) = &p.rest {
        collect_pattern_names(&r.rest.argument, &mut set);
    }
    set.into_iter().collect()
}
