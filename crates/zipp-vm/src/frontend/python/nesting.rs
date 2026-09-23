//! Nesting bounds for a parsed module, enforced before any recursive walk.
//!
//! The parser builds arbitrarily deep trees without recursing (a million
//! unary minus signs fit in the source and token caps), but the symbol
//! table, the emitter and the AST's own `Drop` are recursive. [`check`] walks
//! the tree with an explicit stack and rejects one whose nesting would carry
//! those walks past [`NESTING_LIMIT`] levels; [`Suite`] then tears a deep tree
//! down iteratively, so neither a rejected nor an accepted program can
//! exhaust the native stack (or a wasm host's much smaller one).
//!
//! Chains the compiler walks iteratively cost no nesting here: the left
//! spine of an operator chain (`a + b + c ...`) and an `elif` ladder. So do
//! the clauses of one comprehension and the items of one `with`, which the
//! emitter lowers one inside the other: those count one level per clause.
use super::emitter::R;
use ast::Ranged;
use rustpython_parser::ast;

/// Levels of effective nesting the recursive walks may reach. The emitter's
/// own, stricter per-construct limit (`MAX_DEPTH`) still applies; this bound
/// covers every walk, including ones the emitter's accounting does not charge.
pub(super) const NESTING_LIMIT: usize = 512;
/// A tree deeper than this (counting every edge) is torn down iteratively.
const DEEP_TREE: usize = 256;

/// A parsed module that frees itself without recursion when it is deep.
pub(super) struct Suite {
    stmts: Vec<ast::Stmt>,
    deep: bool,
}

impl std::ops::Deref for Suite {
    type Target = [ast::Stmt];
    fn deref(&self) -> &[ast::Stmt] {
        &self.stmts
    }
}

impl Drop for Suite {
    fn drop(&mut self) {
        if self.deep {
            teardown(std::mem::take(&mut self.stmts));
        }
    }
}

enum Node<'a> {
    Stmt(&'a ast::Stmt),
    Expr(&'a ast::Expr),
    Pattern(&'a ast::Pattern),
}

/// Take ownership of a parsed module after checking its nesting. On error
/// the tree is still freed without recursion.
pub(super) fn check(stmts: Vec<ast::Stmt>, file: &str, source: &str) -> R<Suite> {
    let mut suite = Suite { stmts, deep: true };
    let mut stack: Vec<(Node, usize, usize)> = suite.stmts.iter().map(|s| (Node::Stmt(s), 1, 1)).collect();
    let mut deepest = 0usize;
    while let Some((node, depth, raw)) = stack.pop() {
        deepest = deepest.max(raw);
        if depth > NESTING_LIMIT {
            let at = match node {
                Node::Stmt(s) => s.range().start(),
                Node::Expr(e) => e.range().start(),
                Node::Pattern(p) => p.range().start(),
            };
            let at = super::position(file, source, at);
            return Err(format!("Python: {at}: expression nesting limit exceeded"));
        }
        children(node, depth, raw, &mut stack);
    }
    suite.deep = deepest > DEEP_TREE;
    Ok(suite)
}

/// The int literals (negated ones included) inside the loop bodies of
/// `stmts`, without entering nested function, class or lambda bodies, which
/// are their own code objects: the emitter loads each one once ahead of the
/// body instead of on every iteration. Order is first appearance.
pub(super) fn loop_int_literals(stmts: &[ast::Stmt]) -> Vec<i128> {
    let mut loops: Vec<&[ast::Stmt]> = Vec::new();
    let mut stack: Vec<(Node<'_>, usize, usize)> = stmts.iter().map(|s| (Node::Stmt(s), 0, 0)).collect();
    while let Some((node, depth, raw)) = stack.pop() {
        match node {
            Node::Stmt(ast::Stmt::FunctionDef(_) | ast::Stmt::AsyncFunctionDef(_) | ast::Stmt::ClassDef(_))
            | Node::Expr(ast::Expr::Lambda(_)) => continue,
            Node::Stmt(ast::Stmt::For(f)) => loops.push(&f.body),
            Node::Stmt(ast::Stmt::While(w)) => loops.push(&w.body),
            _ => {}
        }
        children(node, depth, raw, &mut stack);
    }
    let mut out = Vec::new();
    let mut stack: Vec<(Node<'_>, usize, usize)> = Vec::new();
    // An int `%` or `//` in a loop compares its result with zero.
    let mut floor_op = false;
    for body in loops {
        stack.extend(body.iter().map(|s| (Node::Stmt(s), 0, 0)));
    }
    let literal = |c: &ast::ExprConstant| match &c.value {
        ast::Constant::Int(i) => i.to_string().parse::<i128>().ok(),
        _ => None,
    };
    while let Some((node, depth, raw)) = stack.pop() {
        match node {
            Node::Stmt(ast::Stmt::FunctionDef(_) | ast::Stmt::AsyncFunctionDef(_) | ast::Stmt::ClassDef(_))
            | Node::Expr(ast::Expr::Lambda(_)) => continue,
            Node::Expr(ast::Expr::Constant(c)) => {
                if let Some(v) = literal(c) {
                    if !out.contains(&v) {
                        out.push(v);
                    }
                }
            }
            Node::Expr(ast::Expr::UnaryOp(u)) if matches!(u.op, ast::UnaryOp::USub) => {
                if let ast::Expr::Constant(c) = u.operand.as_ref() {
                    if let Some(v) = literal(c).and_then(i128::checked_neg) {
                        if !out.contains(&v) {
                            out.push(v);
                        }
                    }
                }
            }
            Node::Expr(ast::Expr::BinOp(b)) if matches!(b.op, ast::Operator::Mod | ast::Operator::FloorDiv) => floor_op = true,
            Node::Stmt(ast::Stmt::AugAssign(a)) if matches!(a.op, ast::Operator::Mod | ast::Operator::FloorDiv) => floor_op = true,
            _ => {}
        }
        children(node, depth, raw, &mut stack);
    }
    if floor_op && !out.contains(&0) {
        out.insert(0, 0);
    }
    out
}

/// Push `node`'s children with their effective and raw depths.
fn children<'a>(node: Node<'a>, depth: usize, raw: usize, stack: &mut Vec<(Node<'a>, usize, usize)>) {
    let next = depth + 1;
    let raw_next = raw + 1;
    let expr = |stack: &mut Vec<(Node<'a>, usize, usize)>, e: &'a ast::Expr, d: usize| {
        stack.push((Node::Expr(e), d, raw_next));
    };
    match node {
        Node::Stmt(s) => {
            let body = |stack: &mut Vec<(Node<'a>, usize, usize)>, b: &'a [ast::Stmt], d: usize| {
                stack.extend(b.iter().map(|s| (Node::Stmt(s), d, raw_next)));
            };
            match s {
                ast::Stmt::FunctionDef(f) => {
                    f.decorator_list.iter().for_each(|d| expr(stack, d, next));
                    arguments(&f.args, next, raw_next, stack);
                    if let Some(r) = &f.returns {
                        expr(stack, r, next);
                    }
                    type_params(&f.type_params, next, raw_next, stack);
                    body(stack, &f.body, next);
                }
                ast::Stmt::AsyncFunctionDef(f) => {
                    f.decorator_list.iter().for_each(|d| expr(stack, d, next));
                    arguments(&f.args, next, raw_next, stack);
                    if let Some(r) = &f.returns {
                        expr(stack, r, next);
                    }
                    type_params(&f.type_params, next, raw_next, stack);
                    body(stack, &f.body, next);
                }
                ast::Stmt::ClassDef(c) => {
                    c.decorator_list.iter().for_each(|d| expr(stack, d, next));
                    c.bases.iter().for_each(|b| expr(stack, b, next));
                    c.keywords.iter().for_each(|k| expr(stack, &k.value, next));
                    type_params(&c.type_params, next, raw_next, stack);
                    body(stack, &c.body, next);
                }
                ast::Stmt::Return(r) => {
                    if let Some(v) = &r.value {
                        expr(stack, v, next);
                    }
                }
                ast::Stmt::Delete(d) => d.targets.iter().for_each(|t| expr(stack, t, next)),
                ast::Stmt::Assign(a) => {
                    a.targets.iter().for_each(|t| expr(stack, t, next));
                    expr(stack, &a.value, next);
                }
                ast::Stmt::TypeAlias(a) => {
                    expr(stack, &a.name, next);
                    type_params(&a.type_params, next, raw_next, stack);
                    expr(stack, &a.value, next);
                }
                ast::Stmt::AugAssign(a) => {
                    expr(stack, &a.target, next);
                    expr(stack, &a.value, next);
                }
                ast::Stmt::AnnAssign(a) => {
                    expr(stack, &a.target, next);
                    expr(stack, &a.annotation, next);
                    if let Some(v) = &a.value {
                        expr(stack, v, next);
                    }
                }
                ast::Stmt::For(f) => {
                    expr(stack, &f.target, next);
                    expr(stack, &f.iter, next);
                    body(stack, &f.body, next);
                    body(stack, &f.orelse, next);
                }
                ast::Stmt::AsyncFor(f) => {
                    expr(stack, &f.target, next);
                    expr(stack, &f.iter, next);
                    body(stack, &f.body, next);
                    body(stack, &f.orelse, next);
                }
                ast::Stmt::While(w) => {
                    expr(stack, &w.test, next);
                    body(stack, &w.body, next);
                    body(stack, &w.orelse, next);
                }
                ast::Stmt::If(i) => {
                    expr(stack, &i.test, next);
                    body(stack, &i.body, next);
                    match i.orelse.as_slice() {
                        // An `elif` continues the ladder at the same level.
                        [elif @ ast::Stmt::If(_)] => stack.push((Node::Stmt(elif), depth, raw_next)),
                        orelse => body(stack, orelse, next),
                    }
                }
                ast::Stmt::With(w) => {
                    with_items(&w.items, next, raw_next, stack);
                    body(stack, &w.body, next + w.items.len());
                }
                ast::Stmt::AsyncWith(w) => {
                    with_items(&w.items, next, raw_next, stack);
                    body(stack, &w.body, next + w.items.len());
                }
                ast::Stmt::Match(m) => {
                    expr(stack, &m.subject, next);
                    for case in &m.cases {
                        stack.push((Node::Pattern(&case.pattern), next, raw_next));
                        if let Some(g) = &case.guard {
                            expr(stack, g, next);
                        }
                        body(stack, &case.body, next);
                    }
                }
                ast::Stmt::Raise(r) => {
                    for part in [&r.exc, &r.cause].into_iter().flatten() {
                        expr(stack, part, next);
                    }
                }
                ast::Stmt::Try(t) => {
                    body(stack, &t.body, next);
                    handlers(&t.handlers, next, raw_next, stack);
                    body(stack, &t.orelse, next);
                    body(stack, &t.finalbody, next);
                }
                ast::Stmt::TryStar(t) => {
                    body(stack, &t.body, next);
                    handlers(&t.handlers, next, raw_next, stack);
                    body(stack, &t.orelse, next);
                    body(stack, &t.finalbody, next);
                }
                ast::Stmt::Assert(a) => {
                    expr(stack, &a.test, next);
                    if let Some(m) = &a.msg {
                        expr(stack, m, next);
                    }
                }
                ast::Stmt::Expr(e) => expr(stack, &e.value, next),
                ast::Stmt::Import(_)
                | ast::Stmt::ImportFrom(_)
                | ast::Stmt::Global(_)
                | ast::Stmt::Nonlocal(_)
                | ast::Stmt::Pass(_)
                | ast::Stmt::Break(_)
                | ast::Stmt::Continue(_) => {}
            }
        }
        Node::Expr(e) => match e {
            ast::Expr::BoolOp(b) => b.values.iter().for_each(|v| expr(stack, v, next)),
            ast::Expr::NamedExpr(n) => {
                expr(stack, &n.target, next);
                expr(stack, &n.value, next);
            }
            ast::Expr::BinOp(b) => {
                // The left spine of a chain is walked iteratively.
                expr(stack, &b.left, depth);
                expr(stack, &b.right, next);
            }
            ast::Expr::UnaryOp(u) => expr(stack, &u.operand, next),
            ast::Expr::Lambda(l) => {
                arguments(&l.args, next, raw_next, stack);
                expr(stack, &l.body, next);
            }
            ast::Expr::IfExp(i) => {
                expr(stack, &i.test, next);
                expr(stack, &i.body, next);
                expr(stack, &i.orelse, next);
            }
            ast::Expr::Dict(d) => {
                d.keys.iter().flatten().for_each(|k| expr(stack, k, next));
                d.values.iter().for_each(|v| expr(stack, v, next));
            }
            ast::Expr::Set(s) => s.elts.iter().for_each(|v| expr(stack, v, next)),
            ast::Expr::ListComp(c) => {
                comprehension(&c.generators, next, raw_next, stack);
                expr(stack, &c.elt, next + c.generators.len());
            }
            ast::Expr::SetComp(c) => {
                comprehension(&c.generators, next, raw_next, stack);
                expr(stack, &c.elt, next + c.generators.len());
            }
            ast::Expr::DictComp(c) => {
                comprehension(&c.generators, next, raw_next, stack);
                expr(stack, &c.key, next + c.generators.len());
                expr(stack, &c.value, next + c.generators.len());
            }
            ast::Expr::GeneratorExp(c) => {
                comprehension(&c.generators, next, raw_next, stack);
                expr(stack, &c.elt, next + c.generators.len());
            }
            ast::Expr::Await(a) => expr(stack, &a.value, next),
            ast::Expr::Yield(y) => {
                if let Some(v) = &y.value {
                    expr(stack, v, next);
                }
            }
            ast::Expr::YieldFrom(y) => expr(stack, &y.value, next),
            ast::Expr::Compare(c) => {
                expr(stack, &c.left, next);
                c.comparators.iter().for_each(|v| expr(stack, v, next));
            }
            ast::Expr::Call(c) => {
                expr(stack, &c.func, next);
                c.args.iter().for_each(|v| expr(stack, v, next));
                c.keywords.iter().for_each(|k| expr(stack, &k.value, next));
            }
            ast::Expr::FormattedValue(f) => {
                expr(stack, &f.value, next);
                if let Some(s) = &f.format_spec {
                    expr(stack, s, next);
                }
            }
            ast::Expr::JoinedStr(j) => j.values.iter().for_each(|v| expr(stack, v, next)),
            ast::Expr::Attribute(a) => expr(stack, &a.value, next),
            ast::Expr::Subscript(s) => {
                expr(stack, &s.value, next);
                expr(stack, &s.slice, next);
            }
            ast::Expr::Starred(s) => expr(stack, &s.value, next),
            ast::Expr::List(l) => l.elts.iter().for_each(|v| expr(stack, v, next)),
            ast::Expr::Tuple(t) => t.elts.iter().for_each(|v| expr(stack, v, next)),
            ast::Expr::Slice(s) => {
                for part in [&s.lower, &s.upper, &s.step].into_iter().flatten() {
                    expr(stack, part, next);
                }
            }
            ast::Expr::Constant(_) | ast::Expr::Name(_) => {}
        },
        Node::Pattern(p) => {
            let pattern = |stack: &mut Vec<(Node<'a>, usize, usize)>, p: &'a ast::Pattern| {
                stack.push((Node::Pattern(p), next, raw_next));
            };
            match p {
                ast::Pattern::MatchValue(v) => expr(stack, &v.value, next),
                ast::Pattern::MatchSingleton(_) | ast::Pattern::MatchStar(_) => {}
                ast::Pattern::MatchSequence(q) => q.patterns.iter().for_each(|x| pattern(stack, x)),
                ast::Pattern::MatchMapping(m) => {
                    m.keys.iter().for_each(|k| expr(stack, k, next));
                    m.patterns.iter().for_each(|x| pattern(stack, x));
                }
                ast::Pattern::MatchClass(c) => {
                    expr(stack, &c.cls, next);
                    c.patterns.iter().for_each(|x| pattern(stack, x));
                    c.kwd_patterns.iter().for_each(|x| pattern(stack, x));
                }
                ast::Pattern::MatchAs(a) => {
                    if let Some(inner) = &a.pattern {
                        pattern(stack, inner);
                    }
                }
                ast::Pattern::MatchOr(o) => o.patterns.iter().for_each(|x| pattern(stack, x)),
            }
        }
    }
}

fn arguments<'a>(args: &'a ast::Arguments, depth: usize, raw: usize, stack: &mut Vec<(Node<'a>, usize, usize)>) {
    for a in args.posonlyargs.iter().chain(&args.args).chain(&args.kwonlyargs) {
        if let Some(d) = &a.default {
            stack.push((Node::Expr(d), depth, raw));
        }
        if let Some(ann) = &a.def.annotation {
            stack.push((Node::Expr(ann), depth, raw));
        }
    }
    for a in [&args.vararg, &args.kwarg].into_iter().flatten() {
        if let Some(ann) = &a.annotation {
            stack.push((Node::Expr(ann), depth, raw));
        }
    }
}

fn type_params<'a>(params: &'a [ast::TypeParam], depth: usize, raw: usize, stack: &mut Vec<(Node<'a>, usize, usize)>) {
    for p in params {
        if let ast::TypeParam::TypeVar(v) = p {
            if let Some(bound) = &v.bound {
                stack.push((Node::Expr(bound), depth, raw));
            }
        }
    }
}

fn with_items<'a>(items: &'a [ast::WithItem], depth: usize, raw: usize, stack: &mut Vec<(Node<'a>, usize, usize)>) {
    // Each item's statement is compiled inside the previous item's.
    for (i, item) in items.iter().enumerate() {
        stack.push((Node::Expr(&item.context_expr), depth + i, raw));
        if let Some(v) = &item.optional_vars {
            stack.push((Node::Expr(v), depth + i, raw));
        }
    }
}

fn handlers<'a>(handlers: &'a [ast::ExceptHandler], depth: usize, raw: usize, stack: &mut Vec<(Node<'a>, usize, usize)>) {
    for h in handlers {
        let ast::ExceptHandler::ExceptHandler(h) = h;
        if let Some(t) = &h.type_ {
            stack.push((Node::Expr(t), depth, raw));
        }
        stack.extend(h.body.iter().map(|s| (Node::Stmt(s), depth, raw)));
    }
}

fn comprehension<'a>(generators: &'a [ast::Comprehension], depth: usize, raw: usize, stack: &mut Vec<(Node<'a>, usize, usize)>) {
    // Each clause's loop is compiled inside the previous clause's.
    for (i, g) in generators.iter().enumerate() {
        stack.push((Node::Expr(&g.target), depth + i, raw));
        stack.push((Node::Expr(&g.iter), depth + i, raw));
        stack.extend(g.ifs.iter().map(|c| (Node::Expr(c), depth + i, raw)));
    }
}

enum Owned {
    Stmt(ast::Stmt),
    Expr(ast::Expr),
    Pattern(ast::Pattern),
}

fn placeholder() -> ast::Expr {
    ast::Expr::Constant(ast::ExprConstant {
        value: ast::Constant::None,
        kind: None,
        range: Default::default(),
    })
}

/// Free a tree with an explicit stack: every node's children are moved out
/// before the (then shallow) node is dropped.
fn teardown(stmts: Vec<ast::Stmt>) {
    let mut stack: Vec<Owned> = stmts.into_iter().map(Owned::Stmt).collect();
    let boxed = |stack: &mut Vec<Owned>, e: &mut Box<ast::Expr>| {
        stack.push(Owned::Expr(std::mem::replace(&mut **e, placeholder())));
    };
    let opt = |stack: &mut Vec<Owned>, e: &mut Option<Box<ast::Expr>>| {
        if let Some(e) = e.take() {
            stack.push(Owned::Expr(*e));
        }
    };
    let exprs = |stack: &mut Vec<Owned>, v: &mut Vec<ast::Expr>| {
        stack.extend(std::mem::take(v).into_iter().map(Owned::Expr));
    };
    let body = |stack: &mut Vec<Owned>, v: &mut Vec<ast::Stmt>| {
        stack.extend(std::mem::take(v).into_iter().map(Owned::Stmt));
    };
    let args = |stack: &mut Vec<Owned>, a: &mut ast::Arguments| {
        for a in a.posonlyargs.iter_mut().chain(&mut a.args).chain(&mut a.kwonlyargs) {
            if let Some(d) = a.default.take() {
                stack.push(Owned::Expr(*d));
            }
            if let Some(ann) = a.def.annotation.take() {
                stack.push(Owned::Expr(*ann));
            }
        }
        for a in [&mut a.vararg, &mut a.kwarg].into_iter().flatten() {
            if let Some(ann) = a.annotation.take() {
                stack.push(Owned::Expr(*ann));
            }
        }
    };
    let generators = |stack: &mut Vec<Owned>, gens: &mut Vec<ast::Comprehension>| {
        for g in std::mem::take(gens) {
            stack.push(Owned::Expr(g.target));
            stack.push(Owned::Expr(g.iter));
            stack.extend(g.ifs.into_iter().map(Owned::Expr));
        }
    };
    while let Some(node) = stack.pop() {
        match node {
            Owned::Stmt(mut s) => match &mut s {
                ast::Stmt::FunctionDef(f) => {
                    exprs(&mut stack, &mut f.decorator_list);
                    args(&mut stack, &mut f.args);
                    opt(&mut stack, &mut f.returns);
                    body(&mut stack, &mut f.body);
                }
                ast::Stmt::AsyncFunctionDef(f) => {
                    exprs(&mut stack, &mut f.decorator_list);
                    args(&mut stack, &mut f.args);
                    opt(&mut stack, &mut f.returns);
                    body(&mut stack, &mut f.body);
                }
                ast::Stmt::ClassDef(c) => {
                    exprs(&mut stack, &mut c.decorator_list);
                    exprs(&mut stack, &mut c.bases);
                    for k in std::mem::take(&mut c.keywords) {
                        stack.push(Owned::Expr(k.value));
                    }
                    body(&mut stack, &mut c.body);
                }
                ast::Stmt::Return(r) => opt(&mut stack, &mut r.value),
                ast::Stmt::Delete(d) => exprs(&mut stack, &mut d.targets),
                ast::Stmt::Assign(a) => {
                    exprs(&mut stack, &mut a.targets);
                    boxed(&mut stack, &mut a.value);
                }
                ast::Stmt::TypeAlias(a) => {
                    boxed(&mut stack, &mut a.name);
                    boxed(&mut stack, &mut a.value);
                }
                ast::Stmt::AugAssign(a) => {
                    boxed(&mut stack, &mut a.target);
                    boxed(&mut stack, &mut a.value);
                }
                ast::Stmt::AnnAssign(a) => {
                    boxed(&mut stack, &mut a.target);
                    boxed(&mut stack, &mut a.annotation);
                    opt(&mut stack, &mut a.value);
                }
                ast::Stmt::For(f) => {
                    boxed(&mut stack, &mut f.target);
                    boxed(&mut stack, &mut f.iter);
                    body(&mut stack, &mut f.body);
                    body(&mut stack, &mut f.orelse);
                }
                ast::Stmt::AsyncFor(f) => {
                    boxed(&mut stack, &mut f.target);
                    boxed(&mut stack, &mut f.iter);
                    body(&mut stack, &mut f.body);
                    body(&mut stack, &mut f.orelse);
                }
                ast::Stmt::While(w) => {
                    boxed(&mut stack, &mut w.test);
                    body(&mut stack, &mut w.body);
                    body(&mut stack, &mut w.orelse);
                }
                ast::Stmt::If(i) => {
                    boxed(&mut stack, &mut i.test);
                    body(&mut stack, &mut i.body);
                    body(&mut stack, &mut i.orelse);
                }
                ast::Stmt::With(w) => {
                    for item in std::mem::take(&mut w.items) {
                        stack.push(Owned::Expr(item.context_expr));
                        if let Some(v) = item.optional_vars {
                            stack.push(Owned::Expr(*v));
                        }
                    }
                    body(&mut stack, &mut w.body);
                }
                ast::Stmt::AsyncWith(w) => {
                    for item in std::mem::take(&mut w.items) {
                        stack.push(Owned::Expr(item.context_expr));
                        if let Some(v) = item.optional_vars {
                            stack.push(Owned::Expr(*v));
                        }
                    }
                    body(&mut stack, &mut w.body);
                }
                ast::Stmt::Match(m) => {
                    boxed(&mut stack, &mut m.subject);
                    for case in std::mem::take(&mut m.cases) {
                        stack.push(Owned::Pattern(case.pattern));
                        if let Some(g) = case.guard {
                            stack.push(Owned::Expr(*g));
                        }
                        stack.extend(case.body.into_iter().map(Owned::Stmt));
                    }
                }
                ast::Stmt::Raise(r) => {
                    opt(&mut stack, &mut r.exc);
                    opt(&mut stack, &mut r.cause);
                }
                ast::Stmt::Try(t) => {
                    body(&mut stack, &mut t.body);
                    for h in std::mem::take(&mut t.handlers) {
                        let ast::ExceptHandler::ExceptHandler(h) = h;
                        if let Some(ty) = h.type_ {
                            stack.push(Owned::Expr(*ty));
                        }
                        stack.extend(h.body.into_iter().map(Owned::Stmt));
                    }
                    body(&mut stack, &mut t.orelse);
                    body(&mut stack, &mut t.finalbody);
                }
                ast::Stmt::TryStar(t) => {
                    body(&mut stack, &mut t.body);
                    for h in std::mem::take(&mut t.handlers) {
                        let ast::ExceptHandler::ExceptHandler(h) = h;
                        if let Some(ty) = h.type_ {
                            stack.push(Owned::Expr(*ty));
                        }
                        stack.extend(h.body.into_iter().map(Owned::Stmt));
                    }
                    body(&mut stack, &mut t.orelse);
                    body(&mut stack, &mut t.finalbody);
                }
                ast::Stmt::Assert(a) => {
                    boxed(&mut stack, &mut a.test);
                    opt(&mut stack, &mut a.msg);
                }
                ast::Stmt::Expr(e) => boxed(&mut stack, &mut e.value),
                ast::Stmt::Import(_)
                | ast::Stmt::ImportFrom(_)
                | ast::Stmt::Global(_)
                | ast::Stmt::Nonlocal(_)
                | ast::Stmt::Pass(_)
                | ast::Stmt::Break(_)
                | ast::Stmt::Continue(_) => {}
            },
            Owned::Expr(mut e) => match &mut e {
                ast::Expr::BoolOp(b) => exprs(&mut stack, &mut b.values),
                ast::Expr::NamedExpr(n) => {
                    boxed(&mut stack, &mut n.target);
                    boxed(&mut stack, &mut n.value);
                }
                ast::Expr::BinOp(b) => {
                    boxed(&mut stack, &mut b.left);
                    boxed(&mut stack, &mut b.right);
                }
                ast::Expr::UnaryOp(u) => boxed(&mut stack, &mut u.operand),
                ast::Expr::Lambda(l) => {
                    args(&mut stack, &mut l.args);
                    boxed(&mut stack, &mut l.body);
                }
                ast::Expr::IfExp(i) => {
                    boxed(&mut stack, &mut i.test);
                    boxed(&mut stack, &mut i.body);
                    boxed(&mut stack, &mut i.orelse);
                }
                ast::Expr::Dict(d) => {
                    stack.extend(std::mem::take(&mut d.keys).into_iter().flatten().map(Owned::Expr));
                    exprs(&mut stack, &mut d.values);
                }
                ast::Expr::Set(s) => exprs(&mut stack, &mut s.elts),
                ast::Expr::ListComp(c) => {
                    generators(&mut stack, &mut c.generators);
                    boxed(&mut stack, &mut c.elt);
                }
                ast::Expr::SetComp(c) => {
                    generators(&mut stack, &mut c.generators);
                    boxed(&mut stack, &mut c.elt);
                }
                ast::Expr::DictComp(c) => {
                    generators(&mut stack, &mut c.generators);
                    boxed(&mut stack, &mut c.key);
                    boxed(&mut stack, &mut c.value);
                }
                ast::Expr::GeneratorExp(c) => {
                    generators(&mut stack, &mut c.generators);
                    boxed(&mut stack, &mut c.elt);
                }
                ast::Expr::Await(a) => boxed(&mut stack, &mut a.value),
                ast::Expr::Yield(y) => opt(&mut stack, &mut y.value),
                ast::Expr::YieldFrom(y) => boxed(&mut stack, &mut y.value),
                ast::Expr::Compare(c) => {
                    boxed(&mut stack, &mut c.left);
                    exprs(&mut stack, &mut c.comparators);
                }
                ast::Expr::Call(c) => {
                    boxed(&mut stack, &mut c.func);
                    exprs(&mut stack, &mut c.args);
                    for k in std::mem::take(&mut c.keywords) {
                        stack.push(Owned::Expr(k.value));
                    }
                }
                ast::Expr::FormattedValue(f) => {
                    boxed(&mut stack, &mut f.value);
                    opt(&mut stack, &mut f.format_spec);
                }
                ast::Expr::JoinedStr(j) => exprs(&mut stack, &mut j.values),
                ast::Expr::Attribute(a) => boxed(&mut stack, &mut a.value),
                ast::Expr::Subscript(s) => {
                    boxed(&mut stack, &mut s.value);
                    boxed(&mut stack, &mut s.slice);
                }
                ast::Expr::Starred(s) => boxed(&mut stack, &mut s.value),
                ast::Expr::List(l) => exprs(&mut stack, &mut l.elts),
                ast::Expr::Tuple(t) => exprs(&mut stack, &mut t.elts),
                ast::Expr::Slice(s) => {
                    opt(&mut stack, &mut s.lower);
                    opt(&mut stack, &mut s.upper);
                    opt(&mut stack, &mut s.step);
                }
                ast::Expr::Constant(_) | ast::Expr::Name(_) => {}
            },
            Owned::Pattern(mut p) => match &mut p {
                ast::Pattern::MatchValue(v) => boxed(&mut stack, &mut v.value),
                ast::Pattern::MatchSingleton(_) | ast::Pattern::MatchStar(_) => {}
                ast::Pattern::MatchSequence(q) => {
                    stack.extend(std::mem::take(&mut q.patterns).into_iter().map(Owned::Pattern));
                }
                ast::Pattern::MatchMapping(m) => {
                    exprs(&mut stack, &mut m.keys);
                    stack.extend(std::mem::take(&mut m.patterns).into_iter().map(Owned::Pattern));
                }
                ast::Pattern::MatchClass(c) => {
                    boxed(&mut stack, &mut c.cls);
                    stack.extend(std::mem::take(&mut c.patterns).into_iter().map(Owned::Pattern));
                    stack.extend(std::mem::take(&mut c.kwd_patterns).into_iter().map(Owned::Pattern));
                }
                ast::Pattern::MatchAs(a) => {
                    if let Some(inner) = a.pattern.take() {
                        stack.push(Owned::Pattern(*inner));
                    }
                }
                ast::Pattern::MatchOr(o) => {
                    stack.extend(std::mem::take(&mut o.patterns).into_iter().map(Owned::Pattern));
                }
            },
        }
    }
}
