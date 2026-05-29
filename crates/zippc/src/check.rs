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
    /// Set a name's type in the innermost frame (overwriting), shadowing any
    /// outer binding for the rest of this scope. Used for flow narrowing.
    fn set_local(&mut self, name: &str, ty: Type) {
        self.frames.last_mut().expect("at least one scope").insert(name.to_string(), ty);
    }
}

/// Does a statement always leave the enclosing block (return/break/continue, or
/// an `if`/`else` whose branches all do)? Used for early-return flow narrowing.
fn stmt_diverges(s: &Stmt) -> bool {
    match &s.kind {
        StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue => true,
        StmtKind::If { then_b, else_b, .. } => block_diverges(then_b) && block_diverges(else_b),
        _ => false,
    }
}

fn block_diverges(stmts: &[Stmt]) -> bool {
    stmts.last().is_some_and(stmt_diverges)
}

/// Checker context: function signatures + struct declarations.
struct Cx<'a> {
    sigs: HashMap<String, Sig>,
    structs: &'a [StructDecl],
    func_types: &'a [FuncType],
}

pub fn check(m: &Module) -> Result<(), String> {
    // Struct declarations: unique names, unique fields per struct.
    let mut seen_structs = std::collections::HashSet::new();
    for sd in &m.structs {
        if !seen_structs.insert(sd.name.as_str()) {
            return Err(format!("type error: struct '{}' redefined", sd.name));
        }
        let mut seen_fields = std::collections::HashSet::new();
        for (fname, fty) in &sd.fields {
            if !seen_fields.insert(fname.as_str()) {
                return Err(format!(
                    "type error: struct '{}' has a duplicate field '{}'",
                    sd.name, fname
                ));
            }
            // v0: sized integers aren't yet supported as struct fields (the
            // native backends would need width-aware field slots). Use i64.
            if matches!(fty, Type::I32 | Type::U32 | Type::U64) {
                return Err(format!(
                    "type error: struct '{}' field '{fname}': sized integers \
                     aren't supported as struct fields yet (use i64)",
                    sd.name
                ));
            }
        }
    }

    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for f in &m.funcs {
        if f.name == "len" || crate::ir::math_builtin(&f.name).is_some() {
            return Err(format!("type error: '{}' is a reserved builtin name", f.name));
        }
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
    let cx = Cx { sigs, structs: &m.structs, func_types: &m.func_types };
    for f in &m.funcs {
        check_func(f, &cx)?;
    }
    Ok(())
}

fn check_func(f: &Func, cx: &Cx) -> Result<(), String> {
    let mut scope = Scope::new();
    for p in &f.params {
        scope.declare(&p.name, p.ty).map_err(|_| {
            format!("type error: parameter '{}' declared twice in '{}'", p.name, f.name)
        })?;
    }
    // Check the body as a block (params stay in the outer frame) so flow
    // narrowing applies to the function's top-level statements too.
    check_block(&f.body, &mut scope, cx, f.ret, &f.name, 0)
}

fn check_stmt(
    s: &Stmt,
    scope: &mut Scope,
    cx: &Cx,
    ret: Type,
    fname: &str,
    loop_depth: u32,
) -> Result<(), String> {
    check_stmt_kind(&s.kind, scope, cx, ret, fname, loop_depth)
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
    cx: &Cx,
    ret: Type,
    fname: &str,
    loop_depth: u32,
) -> Result<(), String> {
    match s {
        StmtKind::Let { name, ty, value } => {
            let vt = type_of(value, scope, cx)?;
            // With an annotation, declare the annotated type (so `let x: T | null
            // = someT` keeps `x` nullable) and allow widening into it.
            let declared = match ty {
                Some(ann) => {
                    if !assignable(vt, *ann) {
                        return Err(format!(
                            "type error: `let {name}: {ann:?}` initialized with {vt:?}"
                        ));
                    }
                    *ann
                }
                None => vt,
            };
            scope.declare(name, declared)
        }
        StmtKind::Assign { target, value } => {
            // `type_of` on the target validates it (an undeclared var or a
            // non-array index is an error) and gives the slot's type.
            let tt = type_of(target, scope, cx)?;
            let vt = type_of(value, scope, cx)?;
            if !assignable(vt, tt) {
                return Err(format!("type error: cannot assign {vt:?} to a target of type {tt:?}"));
            }
            Ok(())
        }
        StmtKind::Return(Some(e)) => {
            let t = type_of(e, scope, cx)?;
            if !assignable(t, ret) {
                return Err(format!("type error: '{fname}' returns {ret:?} but found {t:?}"));
            }
            Ok(())
        }
        StmtKind::Return(None) => Err(format!(
            "type error: '{fname}' must return a {ret:?} value (bare `return` unsupported in v0)"
        )),
        StmtKind::If { cond, then_b, else_b } => {
            expect_type(cond, Type::Bool, scope, cx, "if condition")?;
            // Flow-narrow a `T | null` variable that the condition null-checks.
            let guard = null_guard(cond);
            let narrow = |scope: &mut Scope, want_then: bool| {
                if let Some((x, in_then)) = guard {
                    if in_then == want_then {
                        if let Some(inner) = scope.lookup(x).and_then(Type::opt_inner) {
                            let _ = scope.declare(x, inner); // fresh frame: can't fail
                        }
                    }
                }
            };
            scope.enter();
            narrow(scope, true);
            let r1 = check_block(then_b, scope, cx, ret, fname, loop_depth);
            scope.exit();
            r1?;
            scope.enter();
            narrow(scope, false);
            let r2 = check_block(else_b, scope, cx, ret, fname, loop_depth);
            scope.exit();
            r2
        }
        StmtKind::While { cond, body } => {
            expect_type(cond, Type::Bool, scope, cx, "while condition")?;
            check_block(body, scope, cx, ret, fname, loop_depth + 1)
        }
        StmtKind::For { init, cond, step, body } => {
            // The init binding is scoped to the loop.
            scope.enter();
            let r = (|| {
                if let Some(i) = init {
                    check_stmt(i, scope, cx, ret, fname, loop_depth)?;
                }
                expect_type(cond, Type::Bool, scope, cx, "for condition")?;
                if let Some(s) = step {
                    check_stmt(s, scope, cx, ret, fname, loop_depth)?;
                }
                check_block(body, scope, cx, ret, fname, loop_depth + 1)
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
            let t = type_of(e, scope, cx)?;
            if !t.is_numeric() && t != Type::Str {
                return Err(format!("type error: print expects a number or string, found {t:?}"));
            }
            Ok(())
        }
        StmtKind::ExprStmt(e) => {
            type_of(e, scope, cx)?;
            Ok(())
        }
    }
}

fn check_block(
    stmts: &[Stmt],
    scope: &mut Scope,
    cx: &Cx,
    ret: Type,
    fname: &str,
    loop_depth: u32,
) -> Result<(), String> {
    scope.enter();
    let r = (|| {
        for s in stmts {
            check_stmt(s, scope, cx, ret, fname, loop_depth)?;
            // Early-return narrowing: after `if (x === null) { <diverges> }` (or
            // `if (x !== null) {} else { <diverges> }`), `x` is non-null for the
            // rest of the block.
            if let StmtKind::If { cond, then_b, else_b } = &s.kind {
                if let Some((x, narrow_in_then)) = null_guard(cond) {
                    // the rest is non-null if the *null* branch always diverges
                    let rest_nonnull = if narrow_in_then {
                        block_diverges(else_b) // `x !== null`: else is the null branch
                    } else {
                        block_diverges(then_b) // `x === null`: then is the null branch
                    };
                    if rest_nonnull {
                        if let Some(inner) = scope.lookup(x).and_then(Type::opt_inner) {
                            scope.set_local(x, inner);
                        }
                    }
                }
            }
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
    cx: &Cx,
    what: &str,
) -> Result<(), String> {
    let got = type_of(e, scope, cx)?;
    if !assignable(got, want) {
        return Err(format!("type error: {what} expects {want:?}, found {got:?}"));
    }
    Ok(())
}

/// Is a value of type `from` assignable to a slot of type `to`? Identity, plus
/// widening `T` / `null` into `T | null`.
fn assignable(from: Type, to: Type) -> bool {
    from == to || (from == Type::Null && to.is_opt()) || to.opt_inner() == Some(from)
}

/// Validate `recv?.m(args)` (the receiver's non-null type is the method's `this`)
/// and return the method's (non-null) return type.
fn opt_call_ret(
    recv: &Expr,
    name: &str,
    args: &[Expr],
    scope: &Scope,
    cx: &Cx,
) -> Result<Type, String> {
    let sig = cx.sigs.get(name).ok_or_else(|| format!("type error: unknown method '{name}'"))?;
    let rt = type_of(recv, scope, cx)?;
    let recv_c = rt.opt_inner().unwrap_or(rt); // the non-null receiver type
    if sig.params.first() != Some(&recv_c) {
        return Err(format!("type error: optional method call on {rt:?}"));
    }
    if args.len() != sig.params.len() - 1 {
        return Err(format!(
            "type error: '{name}' expects {} argument(s), got {}",
            sig.params.len() - 1,
            args.len()
        ));
    }
    for (a, p) in args.iter().zip(&sig.params[1..]) {
        let at = type_of(a, scope, cx)?;
        if !assignable(at, *p) {
            return Err(format!("type error: method arg expects {p:?}, found {at:?}"));
        }
    }
    Ok(sig.ret)
}

/// A struct field's declared type, by struct id and field name.
fn struct_field_ty(cx: &Cx, id: u32, field: &str) -> Result<Type, String> {
    cx.structs[id as usize]
        .fields
        .iter()
        .find(|(n, _)| n == field)
        .map(|(_, t)| *t)
        .ok_or_else(|| {
            format!("type error: no field '{field}' on struct '{}'", cx.structs[id as usize].name)
        })
}

/// If `cond` is a null guard on a variable (`x === null` / `x !== null`), return
/// the variable name and whether it is non-null in the `then` branch.
fn null_guard(cond: &Expr) -> Option<(&str, bool)> {
    let Expr::Bin { op, l, r } = cond else { return None };
    let var = match (&**l, &**r) {
        (Expr::Var(x), Expr::Null) | (Expr::Null, Expr::Var(x)) => x.as_str(),
        _ => return None,
    };
    match op {
        BinOp::Ne => Some((var, true)),  // `x !== null` → x is non-null in `then`
        BinOp::Eq => Some((var, false)), // `x === null` → x is non-null in `else`
        _ => None,
    }
}

fn type_of(e: &Expr, scope: &Scope, cx: &Cx) -> Result<Type, String> {
    match e {
        Expr::Int(_) => Ok(Type::I64),
        Expr::Float(_) => Ok(Type::F64),
        Expr::Bool(_) => Ok(Type::Bool),
        Expr::Str(_) => Ok(Type::Str),
        Expr::Cond { cond, then, els } => {
            expect_type(cond, Type::Bool, scope, cx, "ternary condition")?;
            let tt = type_of(then, scope, cx)?;
            let et = type_of(els, scope, cx)?;
            if tt != et {
                return Err(format!(
                    "type error: ternary branches have different types ({tt:?} vs {et:?})"
                ));
            }
            Ok(tt)
        }
        Expr::Null => Ok(Type::Null),
        Expr::Coalesce { lhs, rhs } => {
            let lt = type_of(lhs, scope, cx)?;
            let rt = type_of(rhs, scope, cx)?;
            if lt == Type::Null {
                return Ok(rt); // `null ?? rhs`
            }
            match lt.opt_inner() {
                // `x ?? rhs`: a non-null rhs makes the whole thing non-null.
                Some(inner) if rt == inner || rt == lt => Ok(rt),
                Some(_) => Err(format!("type error: `??` right side {rt:?} doesn't match {lt:?}")),
                None => Err(format!("type error: `??` left side must be nullable, found {lt:?}")),
            }
        }
        Expr::OptField { base, field } => {
            let xid = match type_of(base, scope, cx)? {
                Type::OptStruct(id) | Type::Struct(id) => id,
                bt => {
                    return Err(format!("type error: optional access `?.{field}` on a {bt:?}"))
                }
            };
            // result is `field | null`; a heap field (struct/str/array) becomes
            // the matching nullable, a scalar field must be coalesced.
            let ft = struct_field_ty(cx, xid, field)?;
            let inner = ft.opt_inner().unwrap_or(ft);
            match inner {
                Type::Struct(_) | Type::Str | Type::Array(_) => Ok(inner.into_opt().unwrap()),
                _ => Err(format!(
                    "type error: optional access of the non-heap field '{field}' must be \
                     coalesced — write `…?.{field} ?? default`"
                )),
            }
        }
        Expr::OptFieldOr { base, field, default } => {
            let xid = match type_of(base, scope, cx)? {
                Type::OptStruct(id) | Type::Struct(id) => id,
                bt => {
                    return Err(format!("type error: optional access `?.{field}` on a {bt:?}"))
                }
            };
            // the field's value, defaulting to `default` when base is null → the
            // field's *non-null* type (so a nullable field coalesces away too).
            let ft = struct_field_ty(cx, xid, field)?;
            let result = ft.opt_inner().unwrap_or(ft);
            let dt = type_of(default, scope, cx)?;
            if !assignable(dt, result) {
                return Err(format!("type error: `?? default` has type {dt:?}, expected {result:?}"));
            }
            Ok(result)
        }
        Expr::OptCall { recv, name, args } => {
            // result is `ret | null`; a heap return becomes nullable (an already
            // nullable return doesn't double-wrap), a scalar return must be coalesced.
            let ret = opt_call_ret(recv, name, args, scope, cx)?;
            let inner = ret.opt_inner().unwrap_or(ret);
            match inner {
                Type::Struct(_) | Type::Str | Type::Array(_) => Ok(inner.into_opt().unwrap()),
                _ => Err(format!(
                    "type error: optional call returns a non-heap {ret:?} — coalesce it \
                     (`recv?.m(…) ?? default`)"
                )),
            }
        }
        Expr::OptCallOr { recv, name, args, default } => {
            let ret = opt_call_ret(recv, name, args, scope, cx)?;
            let result = ret.opt_inner().unwrap_or(ret); // the method's non-null result
            let dt = type_of(default, scope, cx)?;
            if !assignable(dt, result) {
                return Err(format!("type error: `?? default` has type {dt:?}, expected {result:?}"));
            }
            Ok(result)
        }
        Expr::Var(name) => scope
            .lookup(name)
            .ok_or_else(|| format!("type error: use of undeclared variable '{name}'")),
        Expr::Cast { to, e } => {
            let t = type_of(e, scope, cx)?;
            if t.is_numeric() && to.is_numeric() {
                Ok(*to)
            } else {
                Err(format!("type error: cannot cast {t:?} to {to:?} (numbers only)"))
            }
        }
        Expr::Array(elems) => {
            // Parser guarantees at least one element.
            let first = type_of(&elems[0], scope, cx)?;
            let elem = first
                .as_elem()
                .ok_or("type error: array elements must be scalar (no nested arrays in v0)")?;
            for e in &elems[1..] {
                let t = type_of(e, scope, cx)?;
                if t != first {
                    return Err(format!(
                        "type error: array literal mixes {first:?} and {t:?}"
                    ));
                }
            }
            Ok(Type::Array(elem))
        }
        Expr::Repeat { value, count } => {
            let vt = type_of(value, scope, cx)?;
            let elem = vt
                .as_elem()
                .ok_or("type error: array elements must be scalar (no nested arrays in v0)")?;
            if type_of(count, scope, cx)? != Type::I64 {
                return Err("type error: repeat count must be i64".into());
            }
            Ok(Type::Array(elem))
        }
        Expr::Index { arr, index } => {
            let at = type_of(arr, scope, cx)?;
            if type_of(index, scope, cx)? != Type::I64 {
                return Err("type error: array index must be i64".into());
            }
            match at {
                Type::Array(elem) => Ok(elem.to_type()),
                _ => Err(format!("type error: cannot index a {at:?}")),
            }
        }
        Expr::StructLit { name, fields } => {
            let id = cx
                .structs
                .iter()
                .position(|s| &s.name == name)
                .ok_or_else(|| format!("type error: unknown struct '{name}'"))?;
            let decl = &cx.structs[id];
            if fields.len() != decl.fields.len() {
                return Err(format!(
                    "type error: struct '{name}' expects {} fields, got {}",
                    decl.fields.len(),
                    fields.len()
                ));
            }
            for (fname, fexpr) in fields {
                let expected = decl
                    .fields
                    .iter()
                    .find(|(n, _)| n == fname)
                    .map(|(_, t)| *t)
                    .ok_or_else(|| format!("type error: struct '{name}' has no field '{fname}'"))?;
                let actual = type_of(fexpr, scope, cx)?;
                if !assignable(actual, expected) {
                    return Err(format!(
                        "type error: field '{name}.{fname}' expects {expected:?}, found {actual:?}"
                    ));
                }
            }
            Ok(Type::Struct(id as u32))
        }
        Expr::Field { base, field } => {
            let bt = type_of(base, scope, cx)?;
            match bt {
                Type::Struct(id) => {
                    let decl = &cx.structs[id as usize];
                    decl.fields
                        .iter()
                        .find(|(n, _)| n == field)
                        .map(|(_, t)| *t)
                        .ok_or_else(|| {
                            format!("type error: no field '{field}' on struct '{}'", decl.name)
                        })
                }
                Type::OptStruct(_) => Err(format!(
                    "type error: '{field}' accessed on a possibly-null value; narrow it with \
                     `if (x !== null) {{ … }}` or use `x ?? …`"
                )),
                _ => Err(format!("type error: cannot access field '{field}' on {bt:?}")),
            }
        }
        Expr::Unary { op, e } => {
            let t = type_of(e, scope, cx)?;
            match op {
                UnOp::Neg if t.is_numeric() => Ok(t),
                UnOp::BitNot if t.is_int() => Ok(t),
                UnOp::Not if t == Type::Bool => Ok(Type::Bool),
                _ => Err(format!("type error: unary {op:?} on {t:?}")),
            }
        }
        Expr::Bin { op, l, r } => {
            let lt = type_of(l, scope, cx)?;
            let rt = type_of(r, scope, cx)?;
            use BinOp::*;
            // Null comparison: `x === null` / `x !== null`.
            if lt == Type::Null || rt == Type::Null {
                let ok = |t: Type| t == Type::Null || t.is_opt();
                return match op {
                    Eq | Ne if ok(lt) && ok(rt) => Ok(Type::Bool),
                    _ => Err(format!(
                        "type error: only `==`/`!=` with `null` is allowed ({op:?} on {lt:?}, {rt:?})"
                    )),
                };
            }
            // String operations: `+` concatenates, `==`/`!=` compare.
            if lt == Type::Str || rt == Type::Str {
                return match op {
                    Add if lt == Type::Str && rt == Type::Str => Ok(Type::Str),
                    Eq | Ne if lt == Type::Str && rt == Type::Str => Ok(Type::Bool),
                    _ => Err(format!("type error: operator {op:?} is not valid on strings")),
                };
            }
            let numeric = |t: Type| t.is_numeric();
            match op {
                // +, -, *, / work on i64 OR f64 (operands must match — no implicit mixing).
                Add | Sub | Mul | Div => {
                    if lt == rt && numeric(lt) {
                        Ok(lt)
                    } else {
                        Err(format!("type error: arithmetic {op:?} on {lt:?} and {rt:?}"))
                    }
                }
                // %, bitwise and shifts are integer-only (operands same int type).
                Mod | BitAnd | BitOr | BitXor | Shl | Shr => {
                    if lt == rt && lt.is_int() {
                        Ok(lt)
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
                return match type_of(&args[0], scope, cx)? {
                    Type::Array(_) | Type::Str => Ok(Type::I64),
                    other => Err(format!(
                        "type error: len expects an array or string, found {other:?}"
                    )),
                };
            }
            // Builtin math: abs/min/max/pow (ints), sqrt/floor/ceil (floats).
            if crate::ir::math_builtin(name).is_some() {
                let argt: Vec<Type> = args
                    .iter()
                    .map(|a| type_of(a, scope, cx))
                    .collect::<Result<_, _>>()?;
                use Type::*;
                return match (name.as_str(), argt.as_slice()) {
                    ("abs", [t]) if *t == I64 || *t == F64 => Ok(*t),
                    ("min" | "max", [a, b]) if a == b && (*a == I64 || *a == F64) => Ok(*a),
                    ("pow", [I64, I64]) => Ok(I64),
                    ("sqrt" | "floor" | "ceil", [F64]) => Ok(F64),
                    _ => Err(format!("type error: invalid arguments to builtin '{name}'")),
                };
            }
            let sig = cx
                .sigs
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
                let at = type_of(a, scope, cx)?;
                if !assignable(at, *pty) {
                    return Err(format!(
                        "type error: '{name}' arg {i} expects {pty:?}, found {at:?}"
                    ));
                }
            }
            Ok(sig.ret)
        }
        // A function used as a value — its type is the matching `Func` type.
        Expr::FuncRef(name) => {
            let sig = cx
                .sigs
                .get(name)
                .ok_or_else(|| format!("type error: '{name}' is not a function"))?;
            let id = cx
                .func_types
                .iter()
                .position(|ft| ft.params == sig.params && ft.ret == sig.ret)
                .ok_or_else(|| format!("type error: no function type interned for '{name}'"))?;
            Ok(Type::Func(id as u32))
        }
        // A closure: drop the captured leading params to recover the function's
        // (explicit) value type, after checking each capture matches its param.
        Expr::MakeClosure { name, captures } => {
            let sig = cx
                .sigs
                .get(name)
                .ok_or_else(|| format!("type error: closure target '{name}' is not a function"))?;
            let params = sig.params.clone();
            let ret = sig.ret;
            if captures.len() > params.len() {
                return Err(format!(
                    "type error: closure '{name}' has {} captures but only {} parameters",
                    captures.len(),
                    params.len()
                ));
            }
            for (i, c) in captures.iter().enumerate() {
                let ct = type_of(c, scope, cx)?;
                if !assignable(ct, params[i]) {
                    return Err(format!(
                        "type error: closure capture {i} expects {:?}, found {ct:?}",
                        params[i]
                    ));
                }
            }
            let explicit = &params[captures.len()..];
            let id = cx
                .func_types
                .iter()
                .position(|ft| ft.params.as_slice() == explicit && ft.ret == ret)
                .ok_or_else(|| format!("type error: no function type interned for closure '{name}'"))?;
            Ok(Type::Func(id as u32))
        }
        // Indirect call: the callee must be a function value.
        Expr::CallValue { callee, args } => {
            let ct = type_of(callee, scope, cx)?;
            let Type::Func(id) = ct else {
                return Err(format!("type error: called a non-function value of type {ct:?}"));
            };
            let ft = &cx.func_types[id as usize];
            if args.len() != ft.params.len() {
                return Err(format!(
                    "type error: function value expects {} arg(s), got {}",
                    ft.params.len(),
                    args.len()
                ));
            }
            for (i, (a, pty)) in args.iter().zip(&ft.params).enumerate() {
                let at = type_of(a, scope, cx)?;
                if !assignable(at, *pty) {
                    return Err(format!(
                        "type error: call arg {i} expects {pty:?}, found {at:?}"
                    ));
                }
            }
            Ok(ft.ret)
        }
        // `arr.push(value)` — append; result is the new length.
        Expr::Push { arr, value } => {
            let at = type_of(arr, scope, cx)?;
            let Type::Array(e) = at else {
                return Err(format!("type error: push on a non-array {at:?}"));
            };
            let vt = type_of(value, scope, cx)?;
            if !assignable(vt, e.to_type()) {
                return Err(format!("type error: push expects {:?}, found {vt:?}", e.to_type()));
            }
            Ok(Type::I64)
        }
        // `arr.pop()` — remove the last element; result is the element type.
        Expr::Pop { arr } => {
            let at = type_of(arr, scope, cx)?;
            let Type::Array(e) = at else {
                return Err(format!("type error: pop on a non-array {at:?}"));
            };
            Ok(e.to_type())
        }
        // A native string method — `args[0]` is the receiver string.
        Expr::StrOp { op, args } => {
            use StrOpKind::*;
            if type_of(&args[0], scope, cx)? != Type::Str {
                return Err("type error: string method on a non-string".into());
            }
            let want: &[Type] = match op {
                ByteAt | SliceFrom | CharAt | Repeat => &[Type::I64],
                Slice => &[Type::I64, Type::I64],
                IndexOf => &[Type::Str, Type::I64],
                LastIndexOf | EndsWith => &[Type::Str],
            };
            let rest = &args[1..];
            if rest.len() != want.len() {
                return Err(format!(
                    "type error: string method {op:?} expects {} argument(s), got {}",
                    want.len(),
                    rest.len()
                ));
            }
            for (a, w) in rest.iter().zip(want) {
                let at = type_of(a, scope, cx)?;
                if !assignable(at, *w) {
                    return Err(format!("type error: string method arg expects {w:?}, found {at:?}"));
                }
            }
            Ok(op.result())
        }
    }
}
