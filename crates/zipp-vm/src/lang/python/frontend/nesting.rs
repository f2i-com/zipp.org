//! Nesting bounds for a parsed module, enforced before any recursive walk.
//!
//! The parser builds arbitrarily deep trees without recursing (a million
//! unary minus signs fit in the source and token caps), but the symbol
//! table and the emitter are recursive. [`check`] walks the tree with an
//! explicit stack and rejects one whose nesting would carry those walks past
//! [`NESTING_LIMIT`] levels, so no program can exhaust the native stack (or a
//! wasm host's much smaller one). The tree lives in a bump arena and is
//! freed at once, whatever its depth.
//!
//! Chains the compiler walks iteratively cost no nesting here: the left
//! spine of an operator chain (`a + b + c ...`) and an `elif` ladder. So do
//! the clauses of one comprehension and the items of one `with`, which the
//! emitter lowers one inside the other: those count one level per clause.
use super::emitter::R;
use ast::Ranged;
use zipp_pyparse::tree as ast;

/// Levels of effective nesting the recursive walks may reach. The emitter's
/// own, stricter per-construct limit (`MAX_DEPTH`) still applies; this bound
/// covers every walk, including ones the emitter's accounting does not charge.
pub(super) const NESTING_LIMIT: usize = 512;

/// A parsed module whose nesting is within bounds.
pub(super) struct Suite<'a> {
    stmts: ast::Seq<'a, ast::Stmt<'a>>,
}

impl<'a> std::ops::Deref for Suite<'a> {
    type Target = [ast::Stmt<'a>];
    fn deref(&self) -> &[ast::Stmt<'a>] {
        &self.stmts
    }
}

enum Node<'a> {
    Stmt(&'a ast::Stmt<'a>),
    Expr(&'a ast::Expr<'a>),
    Pattern(&'a ast::Pattern<'a>),
}

/// Check a parsed module's nesting.
pub(super) fn check<'a>(stmts: ast::Seq<'a, ast::Stmt<'a>>, file: &str, source: &str) -> R<Suite<'a>> {
    let mut stack: Vec<(Node, usize, usize)> = stmts.iter().map(|s| (Node::Stmt(s), 1, 1)).collect();
    while let Some((node, depth, raw)) = stack.pop() {
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
    Ok(Suite { stmts })
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
        let default = match p {
            ast::TypeParam::TypeVar(v) => {
                if let Some(bound) = &v.bound {
                    stack.push((Node::Expr(bound), depth, raw));
                }
                &v.default
            }
            ast::TypeParam::ParamSpec(s) => &s.default,
            ast::TypeParam::TypeVarTuple(t) => &t.default,
        };
        if let Some(default) = default {
            stack.push((Node::Expr(default), depth, raw));
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
