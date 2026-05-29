//! Sound-subset type checker.
//!
//! This is intentionally strict (no implicit coercions, no `any`, unique names
//! per function) — the same discipline that PLAN.md §2 relies on for both
//! speed and on-chain determinism. v0 supports `i64` and `bool`.

use crate::ast::*;
use std::collections::HashMap;

struct Sig {
    params: Vec<Type>,
    ret: Type,
}

pub fn check(m: &Module) -> Result<(), String> {
    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for f in &m.funcs {
        if sigs.contains_key(&f.name) {
            return Err(format!("type error: function '{}' redefined", f.name));
        }
        sigs.insert(
            f.name.clone(),
            Sig {
                params: f.params.iter().map(|p| p.ty).collect(),
                ret: f.ret,
            },
        );
    }
    if !sigs.contains_key("main") {
        return Err("type error: program has no `main` function".into());
    }
    for f in &m.funcs {
        check_func(f, &sigs)?;
    }
    Ok(())
}

fn check_func(f: &Func, sigs: &HashMap<String, Sig>) -> Result<(), String> {
    let mut scope: HashMap<String, Type> = HashMap::new();
    for p in &f.params {
        if scope.insert(p.name.clone(), p.ty).is_some() {
            return Err(format!(
                "type error: parameter '{}' declared twice in '{}'",
                p.name, f.name
            ));
        }
    }
    check_block(&f.body, &mut scope, sigs, f.ret, &f.name)
}

fn check_block(
    stmts: &[Stmt],
    scope: &mut HashMap<String, Type>,
    sigs: &HashMap<String, Sig>,
    ret: Type,
    fname: &str,
) -> Result<(), String> {
    for s in stmts {
        check_stmt(s, scope, sigs, ret, fname)?;
    }
    Ok(())
}

fn check_stmt(
    s: &Stmt,
    scope: &mut HashMap<String, Type>,
    sigs: &HashMap<String, Sig>,
    ret: Type,
    fname: &str,
) -> Result<(), String> {
    match s {
        Stmt::Let { name, ty, value } => {
            let vt = type_of(value, scope, sigs)?;
            if let Some(ann) = ty {
                if *ann != vt {
                    return Err(format!(
                        "type error: `let {name}: {ann:?}` initialized with {vt:?}"
                    ));
                }
            }
            if scope.contains_key(name) {
                return Err(format!(
                    "type error: '{name}' already declared in '{fname}' (shadowing is not allowed in v0)"
                ));
            }
            scope.insert(name.clone(), vt);
            Ok(())
        }
        Stmt::Assign { name, value } => {
            let target = *scope
                .get(name)
                .ok_or_else(|| format!("type error: assignment to undeclared '{name}'"))?;
            let vt = type_of(value, scope, sigs)?;
            if target != vt {
                return Err(format!(
                    "type error: cannot assign {vt:?} to '{name}' of type {target:?}"
                ));
            }
            Ok(())
        }
        Stmt::Return(Some(e)) => {
            let t = type_of(e, scope, sigs)?;
            if t != ret {
                return Err(format!(
                    "type error: '{fname}' returns {ret:?} but found {t:?}"
                ));
            }
            Ok(())
        }
        Stmt::Return(None) => Err(format!(
            "type error: '{fname}' must return a {ret:?} value (bare `return` unsupported in v0)"
        )),
        Stmt::If { cond, then_b, else_b } => {
            expect_type(cond, Type::Bool, scope, sigs, "if condition")?;
            check_block(then_b, scope, sigs, ret, fname)?;
            check_block(else_b, scope, sigs, ret, fname)
        }
        Stmt::While { cond, body } => {
            expect_type(cond, Type::Bool, scope, sigs, "while condition")?;
            check_block(body, scope, sigs, ret, fname)
        }
        Stmt::Print(e) => {
            expect_type(e, Type::I64, scope, sigs, "print")?;
            Ok(())
        }
        Stmt::ExprStmt(e) => {
            type_of(e, scope, sigs)?;
            Ok(())
        }
    }
}

fn expect_type(
    e: &Expr,
    want: Type,
    scope: &HashMap<String, Type>,
    sigs: &HashMap<String, Sig>,
    what: &str,
) -> Result<(), String> {
    let got = type_of(e, scope, sigs)?;
    if got != want {
        return Err(format!("type error: {what} expects {want:?}, found {got:?}"));
    }
    Ok(())
}

fn type_of(
    e: &Expr,
    scope: &HashMap<String, Type>,
    sigs: &HashMap<String, Sig>,
) -> Result<Type, String> {
    match e {
        Expr::Int(_) => Ok(Type::I64),
        Expr::Bool(_) => Ok(Type::Bool),
        Expr::Var(name) => scope
            .get(name)
            .copied()
            .ok_or_else(|| format!("type error: use of undeclared variable '{name}'")),
        Expr::Unary { op, e } => {
            let t = type_of(e, scope, sigs)?;
            match op {
                UnOp::Neg if t == Type::I64 => Ok(Type::I64),
                UnOp::Not if t == Type::Bool => Ok(Type::Bool),
                _ => Err(format!("type error: unary {op:?} on {t:?}")),
            }
        }
        Expr::Bin { op, l, r } => {
            let lt = type_of(l, scope, sigs)?;
            let rt = type_of(r, scope, sigs)?;
            use BinOp::*;
            match op {
                Add | Sub | Mul | Div | Mod => {
                    if lt == Type::I64 && rt == Type::I64 {
                        Ok(Type::I64)
                    } else {
                        Err(format!("type error: arithmetic {op:?} on {lt:?} and {rt:?}"))
                    }
                }
                Lt | Le | Gt | Ge => {
                    if lt == Type::I64 && rt == Type::I64 {
                        Ok(Type::Bool)
                    } else {
                        Err(format!("type error: comparison {op:?} on {lt:?} and {rt:?}"))
                    }
                }
                Eq | Ne => {
                    if lt == rt {
                        Ok(Type::Bool)
                    } else {
                        Err(format!("type error: {op:?} on mismatched {lt:?} and {rt:?}"))
                    }
                }
                And | Or => {
                    if lt == Type::Bool && rt == Type::Bool {
                        Ok(Type::Bool)
                    } else {
                        Err(format!("type error: logical {op:?} on {lt:?} and {rt:?}"))
                    }
                }
            }
        }
        Expr::Call { name, args } => {
            let sig = sigs
                .get(name)
                .ok_or_else(|| format!("type error: call to unknown function '{name}'"))?;
            if args.len() != sig.params.len() {
                return Err(format!(
                    "type error: '{name}' expects {} args, got {}",
                    sig.params.len(),
                    args.len()
                ));
            }
            for (i, (a, pty)) in args.iter().zip(&sig.params).enumerate() {
                let at = type_of(a, scope, sigs)?;
                if at != *pty {
                    return Err(format!(
                        "type error: '{name}' arg {i} expects {pty:?}, found {at:?}"
                    ));
                }
            }
            Ok(sig.ret)
        }
    }
}
