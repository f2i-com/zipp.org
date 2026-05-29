//! Sound-subset type checker.
//!
//! Strict by design (no implicit coercions, `i64`/`bool` only) — the discipline
//! PLAN.md §2 relies on for speed and on-chain determinism. Variables are
//! lexically block-scoped with shadowing across scopes; `break`/`continue` are
//! only valid inside a loop.

use crate::ast::*;
use std::collections::HashMap;

struct Sig {
    params: Vec<Type>,
    ret: Type,
}

/// Lexical scope stack. Innermost scope is last.
struct Scope {
    frames: Vec<HashMap<String, Type>>,
}

impl Scope {
    fn new() -> Self {
        Self { frames: vec![HashMap::new()] }
    }
    fn enter(&mut self) {
        self.frames.push(HashMap::new());
    }
    fn exit(&mut self) {
        self.frames.pop();
    }
    /// Declare in the innermost scope; error on redeclaration *within the same scope*.
    fn declare(&mut self, name: &str, ty: Type) -> Result<(), String> {
        let top = self.frames.last_mut().expect("at least one scope");
        if top.contains_key(name) {
            return Err(format!("type error: '{name}' already declared in this scope"));
        }
        top.insert(name.to_string(), ty);
        Ok(())
    }
    fn lookup(&self, name: &str) -> Option<Type> {
        self.frames.iter().rev().find_map(|f| f.get(name).copied())
    }
}

pub fn check(m: &Module) -> Result<(), String> {
    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for f in &m.funcs {
        if sigs.contains_key(&f.name) {
            return Err(format!("type error: function '{}' redefined", f.name));
        }
        sigs.insert(
            f.name.clone(),
            Sig { params: f.params.iter().map(|p| p.ty).collect(), ret: f.ret },
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
    let mut scope = Scope::new();
    for p in &f.params {
        scope.declare(&p.name, p.ty).map_err(|_| {
            format!("type error: parameter '{}' declared twice in '{}'", p.name, f.name)
        })?;
    }
    for s in &f.body {
        check_stmt(s, &mut scope, sigs, f.ret, &f.name, 0)?;
    }
    Ok(())
}

fn check_stmt(
    s: &Stmt,
    scope: &mut Scope,
    sigs: &HashMap<String, Sig>,
    ret: Type,
    fname: &str,
    loop_depth: u32,
) -> Result<(), String> {
    check_stmt_kind(&s.kind, scope, sigs, ret, fname, loop_depth)
        .map_err(|e| with_line(e, s.line))
}

/// Append `[line N]` to a type error unless an inner statement already did
/// (so the most specific line wins).
fn with_line(e: String, line: u32) -> String {
    if e.contains("[line ") {
        e
    } else {
        format!("{e} [line {line}]")
    }
}

fn check_stmt_kind(
    s: &StmtKind,
    scope: &mut Scope,
    sigs: &HashMap<String, Sig>,
    ret: Type,
    fname: &str,
    loop_depth: u32,
) -> Result<(), String> {
    match s {
        StmtKind::Let { name, ty, value } => {
            let vt = type_of(value, scope, sigs)?;
            if let Some(ann) = ty {
                if *ann != vt {
                    return Err(format!(
                        "type error: `let {name}: {ann:?}` initialized with {vt:?}"
                    ));
                }
            }
            scope.declare(name, vt)
        }
        StmtKind::Assign { target, value } => {
            // `type_of` on the target validates it (an undeclared var or a
            // non-array index is an error) and gives the slot's type.
            let tt = type_of(target, scope, sigs)?;
            let vt = type_of(value, scope, sigs)?;
            if tt != vt {
                return Err(format!("type error: cannot assign {vt:?} to a target of type {tt:?}"));
            }
            Ok(())
        }
        StmtKind::Return(Some(e)) => {
            let t = type_of(e, scope, sigs)?;
            if t != ret {
                return Err(format!("type error: '{fname}' returns {ret:?} but found {t:?}"));
            }
            Ok(())
        }
        StmtKind::Return(None) => Err(format!(
            "type error: '{fname}' must return a {ret:?} value (bare `return` unsupported in v0)"
        )),
        StmtKind::If { cond, then_b, else_b } => {
            expect_type(cond, Type::Bool, scope, sigs, "if condition")?;
            check_block(then_b, scope, sigs, ret, fname, loop_depth)?;
            check_block(else_b, scope, sigs, ret, fname, loop_depth)
        }
        StmtKind::While { cond, body } => {
            expect_type(cond, Type::Bool, scope, sigs, "while condition")?;
            check_block(body, scope, sigs, ret, fname, loop_depth + 1)
        }
        StmtKind::For { init, cond, step, body } => {
            // The init binding is scoped to the loop.
            scope.enter();
            let r = (|| {
                if let Some(i) = init {
                    check_stmt(i, scope, sigs, ret, fname, loop_depth)?;
                }
                expect_type(cond, Type::Bool, scope, sigs, "for condition")?;
                if let Some(s) = step {
                    check_stmt(s, scope, sigs, ret, fname, loop_depth)?;
                }
                check_block(body, scope, sigs, ret, fname, loop_depth + 1)
            })();
            scope.exit();
            r
        }
        StmtKind::Break => {
            if loop_depth == 0 {
                return Err("type error: `break` outside of a loop".into());
            }
            Ok(())
        }
        StmtKind::Continue => {
            if loop_depth == 0 {
                return Err("type error: `continue` outside of a loop".into());
            }
            Ok(())
        }
        StmtKind::Print(e) => {
            let t = type_of(e, scope, sigs)?;
            if t != Type::I64 && t != Type::F64 && t != Type::Str {
                return Err(format!("type error: print expects a number or string, found {t:?}"));
            }
            Ok(())
        }
        StmtKind::ExprStmt(e) => {
            type_of(e, scope, sigs)?;
            Ok(())
        }
    }
}

fn check_block(
    stmts: &[Stmt],
    scope: &mut Scope,
    sigs: &HashMap<String, Sig>,
    ret: Type,
    fname: &str,
    loop_depth: u32,
) -> Result<(), String> {
    scope.enter();
    let r = (|| {
        for s in stmts {
            check_stmt(s, scope, sigs, ret, fname, loop_depth)?;
        }
        Ok(())
    })();
    scope.exit();
    r
}

fn expect_type(
    e: &Expr,
    want: Type,
    scope: &Scope,
    sigs: &HashMap<String, Sig>,
    what: &str,
) -> Result<(), String> {
    let got = type_of(e, scope, sigs)?;
    if got != want {
        return Err(format!("type error: {what} expects {want:?}, found {got:?}"));
    }
    Ok(())
}

fn type_of(e: &Expr, scope: &Scope, sigs: &HashMap<String, Sig>) -> Result<Type, String> {
    match e {
        Expr::Int(_) => Ok(Type::I64),
        Expr::Float(_) => Ok(Type::F64),
        Expr::Bool(_) => Ok(Type::Bool),
        Expr::Str(_) => Ok(Type::Str),
        Expr::Var(name) => scope
            .lookup(name)
            .ok_or_else(|| format!("type error: use of undeclared variable '{name}'")),
        Expr::Cast { to, e } => {
            let t = type_of(e, scope, sigs)?;
            if t == Type::I64 || t == Type::F64 {
                Ok(*to)
            } else {
                Err(format!("type error: cannot cast {t:?} to {to:?} (numbers only)"))
            }
        }
        Expr::Array(elems) => {
            // Parser guarantees at least one element.
            let first = type_of(&elems[0], scope, sigs)?;
            let elem = first
                .as_elem()
                .ok_or("type error: array elements must be scalar (no nested arrays in v0)")?;
            for e in &elems[1..] {
                let t = type_of(e, scope, sigs)?;
                if t != first {
                    return Err(format!(
                        "type error: array literal mixes {first:?} and {t:?}"
                    ));
                }
            }
            Ok(Type::Array(elem))
        }
        Expr::Repeat { value, count } => {
            let vt = type_of(value, scope, sigs)?;
            let elem = vt
                .as_elem()
                .ok_or("type error: array elements must be scalar (no nested arrays in v0)")?;
            if type_of(count, scope, sigs)? != Type::I64 {
                return Err("type error: repeat count must be i64".into());
            }
            Ok(Type::Array(elem))
        }
        Expr::Index { arr, index } => {
            let at = type_of(arr, scope, sigs)?;
            if type_of(index, scope, sigs)? != Type::I64 {
                return Err("type error: array index must be i64".into());
            }
            match at {
                Type::Array(elem) => Ok(elem.to_type()),
                _ => Err(format!("type error: cannot index a {at:?}")),
            }
        }
        Expr::Unary { op, e } => {
            let t = type_of(e, scope, sigs)?;
            match op {
                UnOp::Neg if t == Type::I64 || t == Type::F64 => Ok(t),
                UnOp::BitNot if t == Type::I64 => Ok(Type::I64),
                UnOp::Not if t == Type::Bool => Ok(Type::Bool),
                _ => Err(format!("type error: unary {op:?} on {t:?}")),
            }
        }
        Expr::Bin { op, l, r } => {
            let lt = type_of(l, scope, sigs)?;
            let rt = type_of(r, scope, sigs)?;
            use BinOp::*;
            // String operations: `+` concatenates, `==`/`!=` compare.
            if lt == Type::Str || rt == Type::Str {
                return match op {
                    Add if lt == Type::Str && rt == Type::Str => Ok(Type::Str),
                    Eq | Ne if lt == Type::Str && rt == Type::Str => Ok(Type::Bool),
                    _ => Err(format!("type error: operator {op:?} is not valid on strings")),
                };
            }
            let numeric = |t: Type| t == Type::I64 || t == Type::F64;
            match op {
                // +, -, *, / work on i64 OR f64 (operands must match — no implicit mixing).
                Add | Sub | Mul | Div => {
                    if lt == rt && numeric(lt) {
                        Ok(lt)
                    } else {
                        Err(format!("type error: arithmetic {op:?} on {lt:?} and {rt:?}"))
                    }
                }
                // %, bitwise and shifts are integer-only.
                Mod | BitAnd | BitOr | BitXor | Shl | Shr => {
                    if lt == Type::I64 && rt == Type::I64 {
                        Ok(Type::I64)
                    } else {
                        Err(format!("type error: integer op {op:?} on {lt:?} and {rt:?}"))
                    }
                }
                Lt | Le | Gt | Ge => {
                    if lt == rt && numeric(lt) {
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
            // Builtin: len(array) -> i64.
            if name == "len" {
                if args.len() != 1 {
                    return Err("type error: len expects 1 array argument".into());
                }
                return match type_of(&args[0], scope, sigs)? {
                    Type::Array(_) | Type::Str => Ok(Type::I64),
                    other => Err(format!(
                        "type error: len expects an array or string, found {other:?}"
                    )),
                };
            }
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
