//! The syntax trees TypeScript's run-time constructs lower to: the trees of
//! the JavaScript tsc emits for them, built directly.
//!
//! A synthesized function needs a span of its own: the compiler finds a
//! deferred body by its function's span (`compile::lazy`), so two functions
//! must never share one. Each lowering below says which it uses.

use crate::parse::ast::*;
use crate::parse::token::{Span, StrVal};
use rustc_hash::FxHashMap;

/// An enum member's value when it is a compile-time constant.
#[derive(Debug, Clone)]
pub(crate) enum Const {
    Num(f64),
    Str(String),
}

pub(crate) fn ident(name: &Name) -> Expr {
    Expr::Ident(name.clone())
}

pub(crate) fn str_expr(s: &str) -> Expr {
    Expr::Str(StrVal::Utf8(s.to_owned()))
}

/// `object.prop`.
pub(crate) fn member(object: Expr, prop: &Name) -> Expr {
    Expr::Member(Box::new(Member {
        object,
        prop: MemberProp::Ident(prop.clone()),
        optional: false,
    }))
}

/// `object[key]`.
fn computed(object: Expr, key: Expr) -> Expr {
    Expr::Member(Box::new(Member {
        object,
        prop: MemberProp::Computed(Box::new(key)),
        optional: false,
    }))
}

fn assign(target: Target, value: Expr) -> Expr {
    Expr::Assign {
        op: AssignOp::Assign,
        target,
        value: Box::new(value),
        covered: false,
    }
}

fn target_of(e: Expr) -> Target {
    match e {
        Expr::Ident(name) => Target::Ident {
            name,
            covered: false,
        },
        Expr::Member(m) => Target::Member(m),
        _ => unreachable!("a lowering assigns to a name or a member"),
    }
}

/// `object.prop = value;`
pub(crate) fn assign_member_stmt(object: &Name, prop: &Name, value: Expr) -> Stmt {
    Stmt::Expr(assign(target_of(member(ident(object), prop)), value))
}

/// `var name;` / `let name;` / `var name = init;`.
pub(crate) fn var_stmt(kind: VarKind, name: &Name, init: Option<Expr>) -> Stmt {
    Stmt::VarDecl(VarDecl {
        kind,
        decls: vec![Declarator {
            id: Pattern::Ident(name.clone()),
            init,
        }],
    })
}

/// `this.x = x;` for each constructor parameter property, after the
/// constructor's `super(…)` call when it has one at its top level (as tsc
/// places them), else first.
pub(crate) fn param_props(body: &mut FnBody, props: Vec<Name>) {
    let at = after_super(&body.stmts);
    let assigns = props.into_iter().map(|n| {
        Stmt::Expr(assign(
            Target::Member(Box::new(Member {
                object: Expr::This,
                prop: MemberProp::Ident(n.clone()),
                optional: false,
            })),
            Expr::Ident(n),
        ))
    });
    body.stmts.splice(at..at, assigns);
}

/// Where a constructor's instance setup goes: after its top-level `super(…)`
/// statement when it has one (a derived class's `this` exists only then),
/// else first.
pub(crate) fn after_super(stmts: &[Stmt]) -> usize {
    stmts
        .iter()
        .position(|s| matches!(s, Stmt::Expr(Expr::Call(c)) if matches!(c.callee, Expr::Super)))
        .map_or(0, |i| i + 1)
}

/// `(function (param) { body })(arg);` — the IIFE an enum or namespace
/// lowers to. `span` must be unique to it (see the module docs).
pub(crate) fn iife(param: &Name, body: Vec<Stmt>, arg: Expr, span: Span) -> Stmt {
    let f = Function {
        name: None,
        params: Params {
            items: vec![Pattern::Ident(param.clone())],
            simple: true,
        },
        body: FnBody {
            directives: Vec::new(),
            stmts: body,
        },
        is_async: false,
        is_generator: false,
        span,
    };
    Stmt::Expr(Expr::Call(Box::new(CallExpr {
        callee: Expr::Function(Box::new(f)),
        args: vec![Arg::Expr(arg)],
        optional: false,
        pos: span.start,
    })))
}

/// The argument an enum's or namespace's IIFE is called with, which merges
/// it into an earlier declaration of the same name: `N || (N = {})` at the
/// top, `N = Outer.N || (Outer.N = {})` for one exported from (or dotted
/// inside) namespace `Outer`.
pub(crate) fn merge_arg(name: &Name, outer: Option<&Name>) -> Expr {
    let empty = || Expr::Object(Vec::new(), LiteralFlags::default());
    match outer {
        None => Expr::Logical {
            op: LogicalOp::Or,
            left: Box::new(ident(name)),
            right: Box::new(assign(target_of(ident(name)), empty())),
        },
        Some(outer) => {
            let slot = || member(ident(outer), name);
            assign(
                target_of(ident(name)),
                Expr::Logical {
                    op: LogicalOp::Or,
                    left: Box::new(slot()),
                    right: Box::new(assign(target_of(slot()), empty())),
                },
            )
        }
    }
}

/// `Object.defineProperty(ns, "name", { get: () => name, set: v => { name = v },
/// enumerable: true, configurable: true })`: a namespace's exported `let` or
/// `var`, kept live as tsc keeps it (it rewrites every use to `ns.name`).
/// The two arrows take the spans `(at, at + 1)` and `(at, at)`, where `at` is
/// the binding's name, which no real function can have.
pub(crate) fn live_export(ns: &Name, name: &Name, at: u32) -> Stmt {
    let v: Name = "%value".into();
    let getter = Expr::Arrow(Box::new(Arrow {
        params: Params::default(),
        body: ArrowBody::Expr(Box::new(ident(name))),
        is_async: false,
        span: Span::new(at, at + 1),
    }));
    let setter = Expr::Arrow(Box::new(Arrow {
        params: Params {
            items: vec![Pattern::Ident(v.clone())],
            simple: true,
        },
        body: ArrowBody::Block(FnBody {
            directives: Vec::new(),
            stmts: vec![Stmt::Expr(assign(target_of(ident(name)), ident(&v)))],
        }),
        is_async: false,
        span: Span::new(at, at),
    }));
    let prop = |key: &str, value: Expr| ObjectMember::Prop {
        key: PropKey::Ident(key.into()),
        value,
        shorthand: false,
        init: None,
    };
    let descriptor = Expr::Object(
        vec![
            prop("get", getter),
            prop("set", setter),
            prop("enumerable", Expr::Bool(true)),
            prop("configurable", Expr::Bool(true)),
        ],
        LiteralFlags::default(),
    );
    Stmt::Expr(Expr::Call(Box::new(CallExpr {
        callee: member(Expr::Ident("Object".into()), &"defineProperty".into()),
        args: vec![
            Arg::Expr(ident(ns)),
            Arg::Expr(str_expr(name)),
            Arg::Expr(descriptor),
        ],
        optional: false,
        pos: at,
    })))
}

/// One member of an enum being lowered: its name and initializer.
pub(crate) struct EnumMember {
    pub(crate) name: Name,
    pub(crate) init: Option<Expr>,
}

/// The statements of an enum's IIFE body (its parameter is `e`, the enum's
/// own name), and the constant members, for later enums to read.
///
/// Each member is `E[E["A"] = 0] = "A"` — a reverse mapping — unless its
/// value is a string (`E["A"] = "a"`). A constant initializer is folded as
/// tsc folds it, referring to earlier members (`B = A << 1`) and to members
/// of enums read before (`C = Other.X`); one that is not constant is
/// evaluated at run time with its references to members made `E.member`, and
/// a member without one is the previous member plus one.
pub(crate) fn enum_body(
    e: &Name,
    members: Vec<EnumMember>,
    known: &FxHashMap<Name, Vec<(Name, Const)>>,
) -> (Vec<Stmt>, Vec<(Name, Const)>) {
    let names: Vec<Name> = members.iter().map(|m| m.name.clone()).collect();
    let mut consts: Vec<(Name, Const)> = Vec::new();
    let mut out = Vec::with_capacity(members.len());
    // What the next member without an initializer is: a constant, or one
    // more than the named member at run time.
    let mut next: Result<f64, Option<Name>> = Ok(0.0);
    for m in members {
        let value: Result<Const, Expr> = match m.init {
            Some(init) => match fold(&init, e, &consts, known) {
                Some(c) => Ok(c),
                None => {
                    let mut init = init;
                    qualify(&mut init, e, &names);
                    Err(init)
                }
            },
            None => match &next {
                Ok(n) => Ok(Const::Num(*n)),
                Err(prev) => Err(match prev {
                    Some(prev) => Expr::Binary {
                        op: BinaryOp::Add,
                        left: Box::new(computed(ident(e), str_expr(prev))),
                        right: Box::new(Expr::Num(1.0)),
                    },
                    None => Expr::Num(f64::NAN),
                }),
            },
        };
        let key = str_expr(&m.name);
        let stmt = match value {
            Ok(Const::Str(s)) => {
                next = Err(None);
                consts.push((m.name.clone(), Const::Str(s.clone())));
                assign(target_of(computed(ident(e), key)), str_expr(&s))
            }
            Ok(Const::Num(n)) => {
                next = Ok(n + 1.0);
                consts.push((m.name.clone(), Const::Num(n)));
                reverse_mapped(e, key, Expr::Num(n), &m.name)
            }
            Err(v) => {
                next = Err(Some(m.name.clone()));
                reverse_mapped(e, key, v, &m.name)
            }
        };
        out.push(Stmt::Expr(stmt));
    }
    (out, consts)
}

/// `E[E["A"] = value] = "A"`.
fn reverse_mapped(e: &Name, key: Expr, value: Expr, name: &Name) -> Expr {
    let forward = assign(target_of(computed(ident(e), key)), value);
    assign(target_of(computed(ident(e), forward)), str_expr(name))
}

/// The constant value of an enum initializer, as tsc's
/// `evaluateEnumMember` computes it, or `None` when it is not constant.
fn fold(
    x: &Expr,
    e: &Name,
    consts: &[(Name, Const)],
    known: &FxHashMap<Name, Vec<(Name, Const)>>,
) -> Option<Const> {
    let num = |x: &Expr| match fold(x, e, consts, known)? {
        Const::Num(n) => Some(n),
        Const::Str(_) => None,
    };
    let lookup = |list: &[(Name, Const)], n: &str| {
        list.iter()
            .rev()
            .find(|(k, _)| &**k == n)
            .map(|(_, v)| v.clone())
    };
    Some(match x {
        Expr::Num(n) => Const::Num(*n),
        Expr::Str(StrVal::Utf8(s)) => Const::Str(s.clone()),
        Expr::Template(t) if t.exprs.is_empty() => {
            Const::Str(t.quasis[0].cooked.as_ref()?.to_lossy_string())
        }
        Expr::Ident(n) => match &**n {
            "Infinity" => Const::Num(f64::INFINITY),
            "NaN" => Const::Num(f64::NAN),
            _ => lookup(consts, n)?,
        },
        Expr::Member(m) => {
            let Expr::Ident(obj) = &m.object else {
                return None;
            };
            let prop = match &m.prop {
                MemberProp::Ident(p) => p.to_string(),
                MemberProp::Computed(k) => match &**k {
                    Expr::Str(StrVal::Utf8(s)) => s.clone(),
                    _ => return None,
                },
                MemberProp::Private(_) => return None,
            };
            if obj == e {
                lookup(consts, &prop)?
            } else {
                lookup(known.get(obj)?, &prop)?
            }
        }
        Expr::Unary { op, arg } => {
            let n = num(arg)?;
            Const::Num(match op {
                UnaryOp::Minus => -n,
                UnaryOp::Plus => n,
                UnaryOp::BitNot => !to_int32(n) as f64,
                _ => return None,
            })
        }
        Expr::Binary { op, left, right } => {
            if *op == BinaryOp::Add {
                match (
                    fold(left, e, consts, known)?,
                    fold(right, e, consts, known)?,
                ) {
                    (Const::Num(a), Const::Num(b)) => return Some(Const::Num(a + b)),
                    (a, b) => {
                        return Some(Const::Str(format!("{}{}", const_text(&a), const_text(&b))))
                    }
                }
            }
            let (a, b) = (num(left)?, num(right)?);
            Const::Num(match op {
                BinaryOp::Sub => a - b,
                BinaryOp::Mul => a * b,
                BinaryOp::Div => a / b,
                BinaryOp::Rem => a % b,
                BinaryOp::Exp => a.powf(b),
                BinaryOp::Shl => to_int32(a).wrapping_shl(to_uint32(b) & 31) as f64,
                BinaryOp::Shr => (to_int32(a) >> (to_uint32(b) & 31)) as f64,
                BinaryOp::UShr => (to_uint32(a) >> (to_uint32(b) & 31)) as f64,
                BinaryOp::BitAnd => (to_int32(a) & to_int32(b)) as f64,
                BinaryOp::BitOr => (to_int32(a) | to_int32(b)) as f64,
                BinaryOp::BitXor => (to_int32(a) ^ to_int32(b)) as f64,
                _ => return None,
            })
        }
        _ => return None,
    })
}

/// A constant as `+` concatenates it.
fn const_text(c: &Const) -> String {
    match c {
        Const::Str(s) => s.clone(),
        Const::Num(n) => crate::vm::helpers_num2::fmt_f64(*n),
    }
}

fn to_uint32(n: f64) -> u32 {
    if !n.is_finite() {
        return 0;
    }
    (n.trunc().rem_euclid(4294967296.0)) as u32
}

fn to_int32(n: f64) -> i32 {
    to_uint32(n) as i32
}

/// Make each reference in `x` to a member of enum `e` (any of `names`)
/// read it from the enum: inside the IIFE the members are properties, not
/// bindings. Nested functions and classes are left alone.
fn qualify(x: &mut Expr, e: &Name, names: &[Name]) {
    match x {
        Expr::Ident(n) => {
            if names.iter().any(|m| m == n) {
                let n = n.clone();
                *x = member(ident(e), &n);
            }
        }
        Expr::Member(m) => {
            qualify(&mut m.object, e, names);
            if let MemberProp::Computed(k) = &mut m.prop {
                qualify(k, e, names);
            }
        }
        Expr::Call(c) => {
            qualify(&mut c.callee, e, names);
            for a in &mut c.args {
                match a {
                    Arg::Expr(x) | Arg::Spread(x) => qualify(x, e, names),
                }
            }
        }
        Expr::New { callee, args, .. } => {
            qualify(callee, e, names);
            for a in args {
                match a {
                    Arg::Expr(x) | Arg::Spread(x) => qualify(x, e, names),
                }
            }
        }
        Expr::Unary { arg, .. } | Expr::Await(arg) => qualify(arg, e, names),
        Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
            qualify(left, e, names);
            qualify(right, e, names);
        }
        Expr::Cond { test, cons, alt } => {
            qualify(test, e, names);
            qualify(cons, e, names);
            qualify(alt, e, names);
        }
        Expr::Seq(items) => items.iter_mut().for_each(|x| qualify(x, e, names)),
        Expr::Chain(inner) => qualify(inner, e, names),
        Expr::Template(t) => t.exprs.iter_mut().for_each(|x| qualify(x, e, names)),
        Expr::TaggedTemplate { tag, quasi } => {
            qualify(tag, e, names);
            quasi.exprs.iter_mut().for_each(|x| qualify(x, e, names));
        }
        Expr::Array(items, _) => {
            for it in items.iter_mut().flatten() {
                match it {
                    ArrayElem::Expr(x) | ArrayElem::Spread(x) => qualify(x, e, names),
                }
            }
        }
        Expr::Object(members, _) => {
            for m in members {
                match m {
                    ObjectMember::Prop { value, .. } => qualify(value, e, names),
                    ObjectMember::Spread(x) => qualify(x, e, names),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}
