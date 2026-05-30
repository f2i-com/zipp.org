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
                if let ox::BindingPattern::BindingIdentifier(id) = &decl.id {
                    out.insert(id.name.to_string());
                }
            }
        }
        S::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
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
                    if let ox::BindingPattern::BindingIdentifier(id) = &decl.id {
                        out.insert(id.name.to_string());
                    }
                }
            }
            collect_bound_stmt(&f.body, out);
        }
        _ => {}
    }
}

// ── reference collection (descends into nested functions) ──

fn stmt_refs(s: &ox::Statement, out: &mut HashSet<String>) {
    use ox::Statement as S;
    match s {
        S::ExpressionStatement(e) => expr_refs(&e.expression, out),
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
            if let ox::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &u.argument {
                out.insert(id.name.to_string());
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
                if let Some(e) = arg.as_expression() {
                    expr_refs(e, out);
                }
            }
        }
        E::StaticMemberExpression(m) => expr_refs(&m.object, out),
        E::ComputedMemberExpression(m) => {
            expr_refs(&m.object, out);
            expr_refs(&m.expression, out);
        }
        E::ArrayExpression(a) => {
            for el in &a.elements {
                if let Some(e) = el.as_expression() {
                    expr_refs(e, out);
                }
            }
        }
        E::ObjectExpression(o) => {
            for prop in &o.properties {
                if let ox::ObjectPropertyKind::ObjectProperty(p) = prop {
                    expr_refs(&p.value, out);
                }
            }
        }
        E::FunctionExpression(f) => fn_node_free(&f.params, f.body.as_deref(), out),
        E::ArrowFunctionExpression(a) => arrow_free(a, out),
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
    let param_names = param_names(params);
    let stmts: &[ox::Statement] = match body {
        Some(b) => &b.statements,
        None => &[],
    };
    let inner = free_vars(&param_names, stmts);
    out.extend(inner);
}

fn arrow_free(a: &ox::ArrowFunctionExpression, out: &mut HashSet<String>) {
    let param_names = param_names(&a.params);
    let inner = free_vars(&param_names, &a.body.statements);
    out.extend(inner);
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
        S::DoWhileStatement(d) => {
            collect_nested_free(&d.body, out);
            collect_nested_free_expr(&d.test, out);
        }
        S::ForStatement(f) => {
            if let Some(t) = &f.test {
                collect_nested_free_expr(t, out);
            }
            if let Some(u) = &f.update {
                collect_nested_free_expr(u, out);
            }
            collect_nested_free(&f.body, out);
        }
        S::ReturnStatement(r) => {
            if let Some(a) = &r.argument {
                collect_nested_free_expr(a, out);
            }
        }
        S::FunctionDeclaration(f) => fn_node_free(&f.params, f.body.as_deref(), out),
        _ => {}
    }
}

fn collect_nested_free_expr(e: &ox::Expression, out: &mut HashSet<String>) {
    use ox::Expression as E;
    match e {
        E::FunctionExpression(f) => fn_node_free(&f.params, f.body.as_deref(), out),
        E::ArrowFunctionExpression(a) => arrow_free(a, out),
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
        E::StaticMemberExpression(m) => collect_nested_free_expr(&m.object, out),
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
                if let ox::ObjectPropertyKind::ObjectProperty(p) = prop {
                    collect_nested_free_expr(&p.value, out);
                }
            }
        }
        _ => {}
    }
}

fn param_names(p: &ox::FormalParameters) -> Vec<String> {
    p.items
        .iter()
        .filter_map(|item| match &item.pattern {
            ox::BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
            _ => None,
        })
        .collect()
}
