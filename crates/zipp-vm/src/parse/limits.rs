//! Hardened-profile validation for recursively-shaped syntax trees.
//!
//! The parser itself bounds recursive-descent calls, but several productions
//! (left-associated binary/member chains) are parsed with loops and still form
//! recursive AST spines. Capture analysis and the bytecode compiler walk those
//! trees recursively. Validate the completed tree with an explicit work stack
//! before any such consumer sees guest-controlled depth.

use super::ast::*;
use super::parser::PResult;
#[cfg(feature = "safe-sandbox")]
use super::parser::SyntaxError;

#[cfg(feature = "safe-sandbox")]
const MAX_SAFE_AST_NESTING: usize = 32;

pub(crate) fn validate_program_nesting(program: &Program) -> PResult<()> {
    #[cfg(not(feature = "safe-sandbox"))]
    {
        let _ = program;
        return Ok(());
    }

    #[cfg(feature = "safe-sandbox")]
    validate_program_nesting_safe(program)
}

#[cfg(feature = "safe-sandbox")]
#[derive(Clone, Copy)]
enum Work<'a> {
    Stmt(&'a Stmt, usize),
    Expr(&'a Expr, usize),
    Pattern(&'a Pattern, usize),
    Target(&'a Target, usize),
    Key(&'a PropKey, usize),
    Function(&'a Function, usize),
    Class(&'a Class, usize),
}

#[cfg(feature = "safe-sandbox")]
fn validate_program_nesting_safe(program: &Program) -> PResult<()> {
    let mut work = Vec::with_capacity(program.body.len().min(256));
    for stmt in &program.body {
        work.push(Work::Stmt(stmt, 1));
    }

    while let Some(item) = work.pop() {
        let depth = match item {
            Work::Stmt(_, d)
            | Work::Expr(_, d)
            | Work::Pattern(_, d)
            | Work::Target(_, d)
            | Work::Key(_, d)
            | Work::Function(_, d)
            | Work::Class(_, d) => d,
        };
        if depth > MAX_SAFE_AST_NESTING {
            return Err(SyntaxError::new(
                "SyntaxError: compiled syntax nesting exceeds the sandbox limit",
                0,
            ));
        }
        let child = depth + 1;

        match item {
            Work::Stmt(stmt, _) => match stmt {
                Stmt::Block(body) => push_stmts(&mut work, body, child),
                Stmt::Empty
                | Stmt::Break(_)
                | Stmt::Continue(_)
                | Stmt::Debugger
                | Stmt::Import(_) => {}
                Stmt::Expr(expr) | Stmt::Throw(expr) => work.push(Work::Expr(expr, child)),
                Stmt::If { test, cons, alt } => {
                    work.push(Work::Expr(test, child));
                    work.push(Work::Stmt(cons, child));
                    if let Some(alt) = alt {
                        work.push(Work::Stmt(alt, child));
                    }
                }
                Stmt::While { test, body } => {
                    work.push(Work::Expr(test, child));
                    work.push(Work::Stmt(body, child));
                }
                Stmt::DoWhile { body, test } => {
                    work.push(Work::Stmt(body, child));
                    work.push(Work::Expr(test, child));
                }
                Stmt::For {
                    init,
                    test,
                    update,
                    body,
                } => {
                    if let Some(init) = init {
                        match init {
                            ForInit::Var(var) => push_var(&mut work, var, child),
                            ForInit::Expr(expr) => work.push(Work::Expr(expr, child)),
                        }
                    }
                    if let Some(test) = test {
                        work.push(Work::Expr(test, child));
                    }
                    if let Some(update) = update {
                        work.push(Work::Expr(update, child));
                    }
                    work.push(Work::Stmt(body, child));
                }
                Stmt::ForIn { left, right, body }
                | Stmt::ForOf {
                    left, right, body, ..
                } => {
                    match left {
                        ForTarget::Var(var) => push_var(&mut work, var, child),
                        ForTarget::Target(target) => work.push(Work::Target(target, child)),
                    }
                    work.push(Work::Expr(right, child));
                    work.push(Work::Stmt(body, child));
                }
                Stmt::Switch { disc, cases } => {
                    work.push(Work::Expr(disc, child));
                    for case in cases {
                        if let Some(test) = &case.test {
                            work.push(Work::Expr(test, child));
                        }
                        push_stmts(&mut work, &case.body, child);
                    }
                }
                Stmt::Return(expr) => {
                    if let Some(expr) = expr {
                        work.push(Work::Expr(expr, child));
                    }
                }
                Stmt::Try {
                    block,
                    handler,
                    finalizer,
                } => {
                    push_stmts(&mut work, block, child);
                    if let Some(handler) = handler {
                        if let Some(param) = &handler.param {
                            work.push(Work::Pattern(param, child));
                        }
                        push_stmts(&mut work, &handler.body, child);
                    }
                    if let Some(finalizer) = finalizer {
                        push_stmts(&mut work, finalizer, child);
                    }
                }
                Stmt::Labeled { body, .. } => work.push(Work::Stmt(body, child)),
                Stmt::With { object, body } => {
                    work.push(Work::Expr(object, child));
                    work.push(Work::Stmt(body, child));
                }
                Stmt::VarDecl(var) => push_var(&mut work, var, child),
                Stmt::FnDecl(function) => work.push(Work::Function(function, child)),
                Stmt::ClassDecl(class) => work.push(Work::Class(class, child)),
                Stmt::Export(export) => match &**export {
                    ExportDecl::Named { .. } | ExportDecl::All { .. } => {}
                    ExportDecl::Decl(stmt) => work.push(Work::Stmt(stmt, child)),
                    ExportDecl::Default(default) => match default {
                        ExportDefault::Expr(expr) => work.push(Work::Expr(expr, child)),
                        ExportDefault::Function(function) => {
                            work.push(Work::Function(function, child))
                        }
                        ExportDefault::Class(class) => work.push(Work::Class(class, child)),
                    },
                },
            },
            Work::Expr(expr, _) => match expr {
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
                | Expr::ImportMeta => {}
                Expr::Array(items, _) => {
                    for item in items.iter().flatten() {
                        match item {
                            ArrayElem::Expr(expr) | ArrayElem::Spread(expr) => {
                                work.push(Work::Expr(expr, child))
                            }
                        }
                    }
                }
                Expr::Object(members, _) => {
                    for member in members {
                        match member {
                            ObjectMember::Prop {
                                key, value, init, ..
                            } => {
                                work.push(Work::Key(key, child));
                                work.push(Work::Expr(value, child));
                                if let Some(init) = init {
                                    work.push(Work::Expr(init, child));
                                }
                            }
                            ObjectMember::Method { key, func }
                            | ObjectMember::Get { key, func }
                            | ObjectMember::Set { key, func } => {
                                work.push(Work::Key(key, child));
                                work.push(Work::Function(func, child));
                            }
                            ObjectMember::Spread(expr) => work.push(Work::Expr(expr, child)),
                        }
                    }
                }
                Expr::Template(template) => push_exprs(&mut work, &template.exprs, child),
                Expr::TaggedTemplate { tag, quasi } => {
                    work.push(Work::Expr(tag, child));
                    push_exprs(&mut work, &quasi.exprs, child);
                }
                Expr::Unary { arg, .. }
                | Expr::Await(arg)
                | Expr::Chain(arg)
                | Expr::PrivateIn { object: arg, .. } => work.push(Work::Expr(arg, child)),
                Expr::Update { target, .. } => work.push(Work::Target(target, child)),
                Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
                    work.push(Work::Expr(left, child));
                    work.push(Work::Expr(right, child));
                }
                Expr::Assign { target, value, .. } => {
                    work.push(Work::Target(target, child));
                    work.push(Work::Expr(value, child));
                }
                Expr::Cond { test, cons, alt } => {
                    work.push(Work::Expr(test, child));
                    work.push(Work::Expr(cons, child));
                    work.push(Work::Expr(alt, child));
                }
                Expr::Call(call) => push_call(&mut work, call, child),
                Expr::New { callee, args } => {
                    work.push(Work::Expr(callee, child));
                    push_args(&mut work, args, child);
                }
                Expr::Member(member) => push_member(&mut work, member, child),
                Expr::Seq(exprs) => push_exprs(&mut work, exprs, child),
                Expr::Arrow(arrow) => {
                    push_patterns(&mut work, &arrow.params.items, child);
                    match &arrow.body {
                        ArrowBody::Expr(expr) => work.push(Work::Expr(expr, child)),
                        ArrowBody::Block(body) => push_stmts(&mut work, &body.stmts, child),
                    }
                }
                Expr::Function(function) => work.push(Work::Function(function, child)),
                Expr::Class(class) => work.push(Work::Class(class, child)),
                Expr::Yield { arg, .. } => {
                    if let Some(arg) = arg {
                        work.push(Work::Expr(arg, child));
                    }
                }
                Expr::ImportCall { spec, options, .. } => {
                    work.push(Work::Expr(spec, child));
                    if let Some(options) = options {
                        work.push(Work::Expr(options, child));
                    }
                }
            },
            Work::Pattern(pattern, _) => match pattern {
                Pattern::Ident(_) => {}
                Pattern::Array(items) => {
                    for item in items.iter().flatten() {
                        work.push(Work::Pattern(&item.pat, child));
                    }
                }
                Pattern::Object { props, rest } => {
                    for prop in props {
                        work.push(Work::Key(&prop.key, child));
                        work.push(Work::Pattern(&prop.value, child));
                    }
                    if let Some(rest) = rest {
                        work.push(Work::Pattern(rest, child));
                    }
                }
                Pattern::Assign { left, right } => {
                    work.push(Work::Pattern(left, child));
                    work.push(Work::Expr(right, child));
                }
                Pattern::Rest(pattern) => work.push(Work::Pattern(pattern, child)),
            },
            Work::Target(target, _) => match target {
                Target::Ident { .. } => {}
                Target::Member(member) => push_member(&mut work, member, child),
                Target::Call(call) => push_call(&mut work, call, child),
                Target::Array(items) => {
                    for item in items.iter().flatten() {
                        work.push(Work::Target(&item.target, child));
                        if let Some(default) = &item.default {
                            work.push(Work::Expr(default, child));
                        }
                    }
                }
                Target::Object { props, rest } => {
                    for prop in props {
                        work.push(Work::Key(&prop.key, child));
                        work.push(Work::Target(&prop.target, child));
                        if let Some(default) = &prop.default {
                            work.push(Work::Expr(default, child));
                        }
                    }
                    if let Some(rest) = rest {
                        work.push(Work::Target(rest, child));
                    }
                }
            },
            Work::Key(key, _) => {
                if let PropKey::Computed(expr) = key {
                    work.push(Work::Expr(expr, child));
                }
            }
            Work::Function(function, _) => {
                push_patterns(&mut work, &function.params.items, child);
                push_stmts(&mut work, &function.body.stmts, child);
            }
            Work::Class(class, _) => {
                if let Some(superclass) = &class.superclass {
                    work.push(Work::Expr(superclass, child));
                }
                push_exprs(&mut work, &class.decorators, child);
                for member in &class.body {
                    match member {
                        ClassMember::Method(method) => {
                            work.push(Work::Key(&method.key, child));
                            push_exprs(&mut work, &method.decorators, child);
                            work.push(Work::Function(&method.func, child));
                        }
                        ClassMember::Field(field) => {
                            work.push(Work::Key(&field.key, child));
                            if let Some(value) = &field.value {
                                work.push(Work::Expr(value, child));
                            }
                            push_exprs(&mut work, &field.decorators, child);
                            if let Some(accessor) = &field.accessor {
                                work.push(Work::Function(&accessor.getter, child));
                                work.push(Work::Function(&accessor.setter, child));
                            }
                        }
                        ClassMember::StaticBlock(body) => push_stmts(&mut work, body, child),
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(feature = "safe-sandbox")]
fn push_stmts<'a>(work: &mut Vec<Work<'a>>, stmts: &'a [Stmt], depth: usize) {
    for stmt in stmts {
        work.push(Work::Stmt(stmt, depth));
    }
}

#[cfg(feature = "safe-sandbox")]
fn push_exprs<'a>(work: &mut Vec<Work<'a>>, exprs: &'a [Expr], depth: usize) {
    for expr in exprs {
        work.push(Work::Expr(expr, depth));
    }
}

#[cfg(feature = "safe-sandbox")]
fn push_patterns<'a>(work: &mut Vec<Work<'a>>, patterns: &'a [Pattern], depth: usize) {
    for pattern in patterns {
        work.push(Work::Pattern(pattern, depth));
    }
}

#[cfg(feature = "safe-sandbox")]
fn push_var<'a>(work: &mut Vec<Work<'a>>, var: &'a VarDecl, depth: usize) {
    for decl in &var.decls {
        work.push(Work::Pattern(&decl.id, depth));
        if let Some(init) = &decl.init {
            work.push(Work::Expr(init, depth));
        }
    }
}

#[cfg(feature = "safe-sandbox")]
fn push_args<'a>(work: &mut Vec<Work<'a>>, args: &'a [Arg], depth: usize) {
    for arg in args {
        match arg {
            Arg::Expr(expr) | Arg::Spread(expr) => work.push(Work::Expr(expr, depth)),
        }
    }
}

#[cfg(feature = "safe-sandbox")]
fn push_call<'a>(work: &mut Vec<Work<'a>>, call: &'a CallExpr, depth: usize) {
    work.push(Work::Expr(&call.callee, depth));
    push_args(work, &call.args, depth);
}

#[cfg(feature = "safe-sandbox")]
fn push_member<'a>(work: &mut Vec<Work<'a>>, member: &'a Member, depth: usize) {
    work.push(Work::Expr(&member.object, depth));
    if let MemberProp::Computed(expr) = &member.prop {
        work.push(Work::Expr(expr, depth));
    }
}
