//! A pre-order walk over every node of a parsed program, for analyses that
//! need to see each expression, statement, pattern and function once (the
//! deferred-compile eligibility scan and its function index, `compile::lazy`).
//!
//! The callback sees a node before its children and returns how the walk goes
//! on ([`Flow`]): into the node's children, past them, or not at all, in which
//! case the walk reports `false` back up. Function, arrow and class nodes are
//! offered as themselves and then walked into like any other node.

use super::ast::*;

/// How the walk goes on after offering a node.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Into the node's children.
    Go,
    /// On past the node, its children unseen.
    Skip,
    /// Nowhere: the walk ends, reporting `false`.
    Stop,
}

impl From<bool> for Flow {
    /// `true` goes on into the node, `false` stops.
    fn from(go: bool) -> Flow {
        if go {
            Flow::Go
        } else {
            Flow::Stop
        }
    }
}

/// A node the walk offers to its callback.
#[derive(Clone, Copy)]
pub(crate) enum Node<'a> {
    Stmt(&'a Stmt),
    Expr(&'a Expr),
    Pattern(&'a Pattern),
    Target(&'a Target),
    PropKey(&'a PropKey),
    Function(&'a Function),
    Arrow(&'a Arrow),
    #[allow(dead_code)] // offered for completeness; no caller reads the class yet
    Class(&'a Class),
}

/// Walk `stmts` and everything under them.
pub(crate) fn stmts<'a>(stmts: &'a [Stmt], f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    stmts.iter().all(|s| stmt(s, f))
}

pub(crate) fn stmt<'a>(s: &'a Stmt, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    match f(Node::Stmt(s)) {
        Flow::Go => {}
        Flow::Skip => return true,
        Flow::Stop => return false,
    }
    match s {
        Stmt::Block(b) => stmts(b, f),
        Stmt::Empty | Stmt::Debugger | Stmt::Break(_) | Stmt::Continue(_) => true,
        Stmt::Expr(e) | Stmt::Throw(e) => expr(e, f),
        Stmt::If { test, cons, alt } => {
            expr(test, f) && stmt(cons, f) && alt.as_deref().is_none_or(|a| stmt(a, f))
        }
        Stmt::While { test, body } | Stmt::DoWhile { body, test } => expr(test, f) && stmt(body, f),
        Stmt::For { init, test, update, body } => {
            init.as_deref().is_none_or(|i| match i {
                ForInit::Var(d) => var_decl(d, f),
                ForInit::Expr(e) => expr(e, f),
            }) && test.as_ref().is_none_or(|e| expr(e, f))
                && update.as_ref().is_none_or(|e| expr(e, f))
                && stmt(body, f)
        }
        Stmt::ForIn { left, right, body } | Stmt::ForOf { left, right, body, .. } => {
            (match left {
                ForTarget::Var(d) => var_decl(d, f),
                ForTarget::Target(t) => target(t, f),
            }) && expr(right, f)
                && stmt(body, f)
        }
        Stmt::Switch { disc, cases } => {
            expr(disc, f)
                && cases
                    .iter()
                    .all(|c| c.test.as_ref().is_none_or(|t| expr(t, f)) && stmts(&c.body, f))
        }
        Stmt::Return(e) => e.as_ref().is_none_or(|e| expr(e, f)),
        Stmt::Try { block, handler, finalizer } => {
            stmts(block, f)
                && handler.as_ref().is_none_or(|h| {
                    h.param.as_ref().is_none_or(|p| pattern(p, f)) && stmts(&h.body, f)
                })
                && finalizer.as_ref().is_none_or(|b| stmts(b, f))
        }
        Stmt::Labeled { body, .. } => stmt(body, f),
        Stmt::With { object, body } => expr(object, f) && stmt(body, f),
        Stmt::VarDecl(d) => var_decl(d, f),
        Stmt::FnDecl(func) => function(func, f),
        Stmt::ClassDecl(c) => class(c, f),
        Stmt::Import(_) => true,
        Stmt::Export(e) => match &**e {
            ExportDecl::Named { .. } | ExportDecl::All { .. } => true,
            ExportDecl::Default(ExportDefault::Expr(e)) => expr(e, f),
            ExportDecl::Default(ExportDefault::Function(func)) => function(func, f),
            ExportDecl::Default(ExportDefault::Class(c)) => class(c, f),
            ExportDecl::Decl(s) => stmt(s, f),
        },
    }
}

fn var_decl<'a>(d: &'a VarDecl, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    d.decls
        .iter()
        .all(|x| pattern(&x.id, f) && x.init.as_ref().is_none_or(|e| expr(e, f)))
}

pub(crate) fn expr<'a>(e: &'a Expr, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    match f(Node::Expr(e)) {
        Flow::Go => {}
        Flow::Skip => return true,
        Flow::Stop => return false,
    }
    match e {
        Expr::Ident(_)
        | Expr::This
        | Expr::Super
        | Expr::Null
        | Expr::Bool(_)
        | Expr::Num(_)
        | Expr::BigInt(_)
        | Expr::Str(_)
        | Expr::Regex { .. }
        | Expr::NewTarget
        | Expr::ImportMeta => true,
        Expr::Array(items, _) => items.iter().all(|i| match i {
            None => true,
            Some(ArrayElem::Expr(e) | ArrayElem::Spread(e)) => expr(e, f),
        }),
        Expr::Object(members, _) => members.iter().all(|m| match m {
            ObjectMember::Prop { key, value, init, .. } => {
                prop_key(key, f) && expr(value, f) && init.as_ref().is_none_or(|e| expr(e, f))
            }
            ObjectMember::Method { key, func }
            | ObjectMember::Get { key, func }
            | ObjectMember::Set { key, func } => prop_key(key, f) && function(func, f),
            ObjectMember::Spread(e) => expr(e, f),
        }),
        Expr::Template(t) => t.exprs.iter().all(|e| expr(e, f)),
        Expr::TaggedTemplate { tag, quasi } => expr(tag, f) && quasi.exprs.iter().all(|e| expr(e, f)),
        Expr::Unary { arg, .. } | Expr::Await(arg) | Expr::Chain(arg) => expr(arg, f),
        Expr::Update { target: t, .. } => target(t, f),
        Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
            expr(left, f) && expr(right, f)
        }
        Expr::Assign { target: t, value, .. } => target(t, f) && expr(value, f),
        Expr::Cond { test, cons, alt } => expr(test, f) && expr(cons, f) && expr(alt, f),
        Expr::Call(c) => call(c, f),
        Expr::New { callee, args, .. } => expr(callee, f) && args_(args, f),
        Expr::Member(m) => member(m, f),
        Expr::Seq(es) => es.iter().all(|e| expr(e, f)),
        Expr::Arrow(a) => arrow(a, f),
        Expr::Function(func) => function(func, f),
        Expr::Class(c) => class(c, f),
        Expr::Yield { arg, .. } => arg.as_deref().is_none_or(|e| expr(e, f)),
        Expr::PrivateIn { object, .. } => expr(object, f),
        Expr::ImportCall { spec, options, .. } => {
            expr(spec, f) && options.as_deref().is_none_or(|e| expr(e, f))
        }
    }
}

fn call<'a>(c: &'a CallExpr, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    expr(&c.callee, f) && args_(&c.args, f)
}

fn args_<'a>(args: &'a [Arg], f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    args.iter().all(|a| match a {
        Arg::Expr(e) | Arg::Spread(e) => expr(e, f),
    })
}

fn member<'a>(m: &'a Member, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    expr(&m.object, f)
        && match &m.prop {
            MemberProp::Computed(e) => expr(e, f),
            MemberProp::Ident(_) | MemberProp::Private(_) => true,
        }
}

pub(crate) fn pattern<'a>(p: &'a Pattern, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    match f(Node::Pattern(p)) {
        Flow::Go => {}
        Flow::Skip => return true,
        Flow::Stop => return false,
    }
    match p {
        Pattern::Ident(_) => true,
        Pattern::Array(items) => items.iter().all(|i| i.as_ref().is_none_or(|e| pattern(&e.pat, f))),
        Pattern::Object { props, rest } => {
            props.iter().all(|p| prop_key(&p.key, f) && pattern(&p.value, f))
                && rest.as_deref().is_none_or(|r| pattern(r, f))
        }
        Pattern::Assign { left, right } => pattern(left, f) && expr(right, f),
        Pattern::Rest(p) => pattern(p, f),
    }
}

pub(crate) fn target<'a>(t: &'a Target, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    match f(Node::Target(t)) {
        Flow::Go => {}
        Flow::Skip => return true,
        Flow::Stop => return false,
    }
    match t {
        Target::Ident { .. } => true,
        Target::Member(m) => member(m, f),
        Target::Call(c) => call(c, f),
        Target::Array(items) => items.iter().all(|i| {
            i.as_ref()
                .is_none_or(|e| target(&e.target, f) && e.default.as_ref().is_none_or(|d| expr(d, f)))
        }),
        Target::Object { props, rest } => {
            props.iter().all(|p| {
                prop_key(&p.key, f)
                    && target(&p.target, f)
                    && p.default.as_ref().is_none_or(|d| expr(d, f))
            }) && rest.as_deref().is_none_or(|r| target(r, f))
        }
    }
}

fn prop_key<'a>(k: &'a PropKey, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    match f(Node::PropKey(k)) {
        Flow::Go => {}
        Flow::Skip => return true,
        Flow::Stop => return false,
    }
    match k {
        PropKey::Computed(e) => expr(e, f),
        PropKey::Ident(_) | PropKey::Str(_) | PropKey::Num(_) | PropKey::Private(_) => true,
    }
}

pub(crate) fn params<'a>(p: &'a Params, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    p.items.iter().all(|p| pattern(p, f))
}

pub(crate) fn function<'a>(func: &'a Function, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    match f(Node::Function(func)) {
        Flow::Go => params(&func.params, f) && stmts(&func.body.stmts, f),
        Flow::Skip => true,
        Flow::Stop => false,
    }
}

pub(crate) fn arrow<'a>(a: &'a Arrow, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    match f(Node::Arrow(a)) {
        Flow::Go => {}
        Flow::Skip => return true,
        Flow::Stop => return false,
    }
    params(&a.params, f)
        && match &a.body {
            ArrowBody::Expr(e) => expr(e, f),
            ArrowBody::Block(b) => stmts(&b.stmts, f),
        }
}

pub(crate) fn class<'a>(c: &'a Class, f: &mut impl FnMut(Node<'a>) -> Flow) -> bool {
    match f(Node::Class(c)) {
        Flow::Go => {}
        Flow::Skip => return true,
        Flow::Stop => return false,
    }
    c.decorators.iter().all(|d| expr(d, f))
        && c.superclass.as_deref().is_none_or(|e| expr(e, f))
        && c.body.iter().all(|m| match m {
            ClassMember::Method(m) => {
                m.decorators.iter().all(|d| expr(d, f)) && prop_key(&m.key, f) && function(&m.func, f)
            }
            ClassMember::Field(fd) => {
                fd.decorators.iter().all(|d| expr(d, f))
                    && prop_key(&fd.key, f)
                    && fd.value.as_ref().is_none_or(|e| expr(e, f))
                    && fd
                        .accessor
                        .as_deref()
                        .is_none_or(|a| function(&a.getter, f) && function(&a.setter, f))
            }
            ClassMember::StaticBlock(b) => stmts(b, f),
        })
}

#[cfg(test)]
mod visit_tests {
    use super::*;

    /// Every function and arrow is offered once, nested ones included; a
    /// skipped node's children are not offered, and a stop ends the walk.
    #[test]
    fn offers_every_function_skips_and_stops() {
        let prog = crate::front::parse_script(
            "function a(x = () => 1) { return function b() { var o = { m() {} }; class C { n() {} } }; }",
        )
        .expect("parses");
        let mut fns = 0;
        assert!(stmts(&prog.body, &mut |n| {
            if matches!(n, Node::Function(_) | Node::Arrow(_)) {
                fns += 1;
            }
            Flow::Go
        }));
        assert_eq!(fns, 5, "a, the default arrow, b, m and n");
        let mut names = Vec::new();
        assert!(stmts(&prog.body, &mut |n| match n {
            Node::Function(f) => {
                names.push(f.name.as_deref().unwrap_or("").to_string());
                if f.name.as_deref() == Some("b") {
                    Flow::Skip
                } else {
                    Flow::Go
                }
            }
            _ => Flow::Go,
        }));
        assert_eq!(names, ["a", "b"], "nothing inside b is offered");
        let mut seen = 0;
        assert!(!stmts(&prog.body, &mut |_| {
            seen += 1;
            (seen < 3).into()
        }));
        assert_eq!(seen, 3);
    }
}
