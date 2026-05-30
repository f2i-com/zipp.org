//! Lower the oxc (arena-allocated, lifetime-bound) AST into our owned
//! lifetime-free [`crate::ast`]. Unsupported syntax returns a clear `Err` (the
//! v0 subset; coverage grows over time).

use std::rc::Rc;

use oxc_ast::ast as ox;

use crate::ast::*;

type R<T> = Result<T, String>;

pub fn lower_program(prog: &ox::Program) -> R<Vec<Stmt>> {
    prog.body.iter().map(stmt).collect()
}

fn block(stmts: &oxc_allocator::Vec<ox::Statement>) -> R<Vec<Stmt>> {
    stmts.iter().map(stmt).collect()
}

fn stmt(s: &ox::Statement) -> R<Stmt> {
    use ox::Statement as S;
    Ok(match s {
        S::ExpressionStatement(e) => Stmt::Expr(expr(&e.expression)?),
        S::VariableDeclaration(d) => var_decl(d)?,
        S::BlockStatement(b) => Stmt::Block(block(&b.body)?),
        S::IfStatement(i) => Stmt::If {
            cond: expr(&i.test)?,
            then: Box::new(stmt(&i.consequent)?),
            els: match &i.alternate {
                Some(a) => Some(Box::new(stmt(a)?)),
                None => None,
            },
        },
        S::WhileStatement(w) => Stmt::While {
            cond: expr(&w.test)?,
            body: Box::new(stmt(&w.body)?),
        },
        S::DoWhileStatement(d) => Stmt::DoWhile {
            body: Box::new(stmt(&d.body)?),
            cond: expr(&d.test)?,
        },
        S::ForStatement(f) => for_stmt(f)?,
        S::ForOfStatement(f) => {
            let (decl, name) = for_left(&f.left)?;
            Stmt::ForOf { decl, name, iterable: expr(&f.right)?, body: Box::new(stmt(&f.body)?) }
        }
        S::ForInStatement(f) => {
            let (decl, name) = for_left(&f.left)?;
            Stmt::ForIn { decl, name, object: expr(&f.right)?, body: Box::new(stmt(&f.body)?) }
        }
        S::SwitchStatement(sw) => {
            let disc = expr(&sw.discriminant)?;
            let mut cases = Vec::new();
            for c in &sw.cases {
                let test = match &c.test {
                    Some(e) => Some(expr(e)?),
                    None => None,
                };
                cases.push((test, c.consequent.iter().map(stmt).collect::<R<Vec<_>>>()?));
            }
            Stmt::Switch { disc, cases }
        }
        S::ReturnStatement(r) => Stmt::Return(match &r.argument {
            Some(a) => Some(expr(a)?),
            None => None,
        }),
        S::BreakStatement(_) => Stmt::Break,
        S::ContinueStatement(_) => Stmt::Continue,
        S::FunctionDeclaration(f) => Stmt::Func(Rc::new(func_def(f)?)),
        S::ClassDeclaration(c) => lower_class(c)?,
        S::ThrowStatement(t) => Stmt::Throw(expr(&t.argument)?),
        S::TryStatement(t) => try_stmt(t)?,
        S::EmptyStatement(_) => Stmt::Empty,
        _ => return Err("unsupported statement (not in the v0 JS engine yet)".into()),
    })
}

fn var_decl(d: &ox::VariableDeclaration) -> R<Stmt> {
    let kind = match d.kind {
        ox::VariableDeclarationKind::Var => DeclKind::Var,
        ox::VariableDeclarationKind::Const => DeclKind::Const,
        _ => DeclKind::Let,
    };
    let mut decls = Vec::new();
    for decl in &d.declarations {
        let name = binding_name(&decl.id)?;
        let init = match &decl.init {
            Some(e) => Some(expr(e)?),
            None => None,
        };
        decls.push((name, init));
    }
    Ok(Stmt::Var { kind, decls })
}

fn binding_name(b: &ox::BindingPattern) -> R<String> {
    match b {
        ox::BindingPattern::BindingIdentifier(id) => Ok(id.name.to_string()),
        _ => Err("destructuring/parameter patterns aren't in the v0 JS engine yet".into()),
    }
}

fn for_stmt(f: &ox::ForStatement) -> R<Stmt> {
    let init = match &f.init {
        None => None,
        Some(ox::ForStatementInit::VariableDeclaration(d)) => Some(Box::new(var_decl(d)?)),
        Some(other) => {
            let e = other
                .as_expression()
                .ok_or("unsupported for-loop initializer")?;
            Some(Box::new(Stmt::Expr(expr(e)?)))
        }
    };
    Ok(Stmt::For {
        init,
        cond: match &f.test {
            Some(e) => Some(expr(e)?),
            None => None,
        },
        step: match &f.update {
            Some(e) => Some(expr(e)?),
            None => None,
        },
        body: Box::new(stmt(&f.body)?),
    })
}

fn for_left(left: &ox::ForStatementLeft) -> R<(Option<DeclKind>, String)> {
    match left {
        ox::ForStatementLeft::VariableDeclaration(d) => {
            let kind = match d.kind {
                ox::VariableDeclarationKind::Var => DeclKind::Var,
                ox::VariableDeclarationKind::Const => DeclKind::Const,
                _ => DeclKind::Let,
            };
            Ok((Some(kind), binding_name(&d.declarations[0].id)?))
        }
        ox::ForStatementLeft::AssignmentTargetIdentifier(id) => Ok((None, id.name.to_string())),
        _ => Err("for-of/for-in needs a simple variable target (v0)".into()),
    }
}

fn try_stmt(t: &ox::TryStatement) -> R<Stmt> {
    let blk = block(&t.block.body)?;
    let catch = match &t.handler {
        None => None,
        Some(h) => {
            let binding = match &h.param {
                Some(p) => Some(binding_name(&p.pattern)?),
                None => None,
            };
            Some((binding, block(&h.body.body)?))
        }
    };
    let finally = match &t.finalizer {
        Some(b) => Some(block(&b.body)?),
        None => None,
    };
    Ok(Stmt::Try { block: blk, catch, finally })
}

// ───────────────────────── functions ─────────────────────────

fn one_param(item: &ox::FormalParameter) -> R<Param> {
    // In oxc, a default value lives in `FormalParameter.initializer` (NOT as an
    // `AssignmentPattern` inside `pattern`); `pattern` is the plain binding.
    let name = binding_name(&item.pattern)?;
    let default = match &item.initializer {
        Some(e) => Some(expr(e)?),
        None => None,
    };
    Ok(Param { name, default })
}

fn params_of(p: &ox::FormalParameters) -> R<(Vec<Param>, Option<String>)> {
    let params = p.items.iter().map(one_param).collect::<R<Vec<_>>>()?;
    let rest = match &p.rest {
        Some(r) => Some(binding_name(&r.rest.argument)?),
        None => None,
    };
    Ok((params, rest))
}

fn class_key_name(k: &ox::PropertyKey) -> R<String> {
    match k {
        ox::PropertyKey::StaticIdentifier(id) => Ok(id.name.to_string()),
        ox::PropertyKey::StringLiteral(s) => Ok(s.value.to_string()),
        ox::PropertyKey::NumericLiteral(n) => Ok(crate::value::num_to_string(n.value)),
        _ => Err("computed/private class member names aren't in the v0 JS engine yet".into()),
    }
}

fn lower_class(c: &ox::Class) -> R<Stmt> {
    let superclass = match &c.super_class {
        Some(e) => Some(expr(e)?),
        None => None,
    };
    let name = c
        .id
        .as_ref()
        .map(|i| i.name.to_string())
        .ok_or("anonymous class declarations aren't supported")?;
    let mut ctor: Option<FuncDef> = None;
    let mut methods: Vec<(String, Rc<FuncDef>)> = Vec::new();
    let mut statics: Vec<(String, Rc<FuncDef>)> = Vec::new();
    let mut field_inits: Vec<Stmt> = Vec::new();

    for el in &c.body.body {
        match el {
            ox::ClassElement::MethodDefinition(m) => {
                let fd = func_def(&m.value)?;
                match m.kind {
                    ox::MethodDefinitionKind::Constructor => ctor = Some(fd),
                    ox::MethodDefinitionKind::Method => {
                        let key = class_key_name(&m.key)?;
                        if m.r#static {
                            statics.push((key, Rc::new(fd)));
                        } else {
                            methods.push((key, Rc::new(fd)));
                        }
                    }
                    _ => return Err("class getters/setters aren't in the v0 JS engine yet".into()),
                }
            }
            ox::ClassElement::PropertyDefinition(p) if !p.r#static => {
                let key = class_key_name(&p.key)?;
                let init = match &p.value {
                    Some(e) => expr(e)?,
                    None => Expr::Undefined,
                };
                // `this.field = init;` — prepended to the constructor body.
                field_inits.push(Stmt::Expr(Expr::Assign {
                    op: None,
                    target: Box::new(Expr::Member {
                        obj: Box::new(Expr::This),
                        prop: Box::new(Expr::Str(key.into())),
                        computed: false,
                    }),
                    value: Box::new(init),
                }));
            }
            _ => {} // static fields / static blocks: deferred
        }
    }

    // Build the constructor: field initializers run first, then the explicit
    // constructor body (or just the field inits, if no constructor).
    let ctor = match ctor {
        Some(fd) => {
            let mut body = field_inits;
            body.extend(fd.body.iter().cloned());
            Rc::new(FuncDef {
                name: Some(name.clone()),
                params: fd.params,
                rest: fd.rest,
                body,
                is_arrow: false,
            })
        }
        None => {
            // A derived class with no explicit constructor gets the implicit
            // `constructor(...args) { super(...args); }`.
            let mut body = Vec::new();
            if c.super_class.is_some() {
                body.push(Stmt::Expr(Expr::Call {
                    callee: Box::new(Expr::Super),
                    args: vec![Expr::Spread(Box::new(Expr::Ident("arguments".into())))],
                }));
            }
            body.extend(field_inits);
            Rc::new(FuncDef {
                name: Some(name.clone()),
                params: Vec::new(),
                rest: None,
                body,
                is_arrow: false,
            })
        }
    };
    Ok(Stmt::Class(Rc::new(ClassDef { name, superclass, ctor, methods, statics })))
}

fn func_def(f: &ox::Function) -> R<FuncDef> {
    let body = match &f.body {
        Some(b) => fn_body(b)?,
        None => Vec::new(),
    };
    let (params, rest) = params_of(&f.params)?;
    Ok(FuncDef {
        name: f.id.as_ref().map(|i| i.name.to_string()),
        params,
        rest,
        body,
        is_arrow: false,
    })
}

fn fn_body(b: &ox::FunctionBody) -> R<Vec<Stmt>> {
    b.statements.iter().map(stmt).collect()
}

fn arrow_def(a: &ox::ArrowFunctionExpression) -> R<FuncDef> {
    // An expression-bodied arrow `(x) => e` is lowered to `{ return e; }`.
    let body = if a.expression {
        // oxc wraps the expression body as a single ExpressionStatement.
        let e = a
            .body
            .statements
            .first()
            .and_then(|s| match s {
                ox::Statement::ExpressionStatement(es) => Some(&es.expression),
                _ => None,
            })
            .ok_or("malformed arrow expression body")?;
        vec![Stmt::Return(Some(expr(e)?))]
    } else {
        fn_body(&a.body)?
    };
    let (params, rest) = params_of(&a.params)?;
    Ok(FuncDef { name: None, params, rest, body, is_arrow: true })
}

// ───────────────────────── expressions ─────────────────────────

fn expr(e: &ox::Expression) -> R<Expr> {
    use ox::Expression as E;
    Ok(match e {
        E::NumericLiteral(n) => Expr::Num(n.value),
        E::StringLiteral(s) => Expr::Str(s.value.as_str().into()),
        E::BooleanLiteral(b) => Expr::Bool(b.value),
        E::NullLiteral(_) => Expr::Null,
        E::Identifier(id) => {
            if id.name == "undefined" {
                Expr::Undefined
            } else {
                Expr::Ident(id.name.to_string())
            }
        }
        E::ThisExpression(_) => Expr::This,
        E::Super(_) => Expr::Super,
        E::TemplateLiteral(t) => template(t)?,
        E::ArrayExpression(a) => array_expr(a)?,
        E::ObjectExpression(o) => object_expr(o)?,
        E::ParenthesizedExpression(p) => expr(&p.expression)?,
        E::UnaryExpression(u) => unary(u)?,
        E::UpdateExpression(u) => update(u)?,
        E::BinaryExpression(b) => binary(b)?,
        E::LogicalExpression(l) => logical(l)?,
        E::AssignmentExpression(a) => assign(a)?,
        E::ConditionalExpression(c) => Expr::Cond {
            cond: Box::new(expr(&c.test)?),
            then: Box::new(expr(&c.consequent)?),
            els: Box::new(expr(&c.alternate)?),
        },
        E::CallExpression(c) => call(c)?,
        E::NewExpression(n) => Expr::New {
            callee: Box::new(expr(&n.callee)?),
            args: call_args(&n.arguments)?,
        },
        E::StaticMemberExpression(m) => Expr::Member {
            obj: Box::new(expr(&m.object)?),
            prop: Box::new(Expr::Str(m.property.name.as_str().into())),
            computed: false,
        },
        E::ComputedMemberExpression(m) => Expr::Member {
            obj: Box::new(expr(&m.object)?),
            prop: Box::new(expr(&m.expression)?),
            computed: true,
        },
        E::FunctionExpression(f) => Expr::Func(Rc::new(func_def(f)?)),
        E::ArrowFunctionExpression(a) => Expr::Func(Rc::new(arrow_def(a)?)),
        E::SequenceExpression(s) => {
            Expr::Seq(s.expressions.iter().map(expr).collect::<R<Vec<_>>>()?)
        }
        _ => return Err("unsupported expression (not in the v0 JS engine yet)".into()),
    })
}

fn template(t: &ox::TemplateLiteral) -> R<Expr> {
    let strings = t
        .quasis
        .iter()
        .map(|q| q.value.cooked.as_ref().map(|a| a.as_str()).unwrap_or("").into())
        .collect();
    let exprs = t.expressions.iter().map(expr).collect::<R<Vec<_>>>()?;
    Ok(Expr::Template { strings, exprs })
}

fn array_expr(a: &ox::ArrayExpression) -> R<Expr> {
    let mut out = Vec::with_capacity(a.elements.len());
    for el in &a.elements {
        match el {
            ox::ArrayExpressionElement::Elision(_) => out.push(None),
            ox::ArrayExpressionElement::SpreadElement(s) => {
                out.push(Some(Expr::Spread(Box::new(expr(&s.argument)?))))
            }
            other => {
                let e = other.as_expression().ok_or("bad array element")?;
                out.push(Some(expr(e)?));
            }
        }
    }
    Ok(Expr::Array(out))
}

fn object_expr(o: &ox::ObjectExpression) -> R<Expr> {
    let mut props = Vec::new();
    for p in &o.properties {
        match p {
            ox::ObjectPropertyKind::ObjectProperty(op) => {
                let key = prop_key(&op.key, op.computed)?;
                props.push(Prop::KeyVal { key, value: expr(&op.value)? });
            }
            ox::ObjectPropertyKind::SpreadProperty(_) => {
                return Err("object spread isn't in the v0 JS engine yet".into())
            }
        }
    }
    Ok(Expr::Object(props))
}

fn prop_key(k: &ox::PropertyKey, computed: bool) -> R<PropKey> {
    match k {
        ox::PropertyKey::StaticIdentifier(id) if !computed => Ok(PropKey::Static(id.name.to_string())),
        ox::PropertyKey::StringLiteral(s) if !computed => Ok(PropKey::Static(s.value.to_string())),
        ox::PropertyKey::NumericLiteral(n) if !computed => {
            Ok(PropKey::Static(crate::value::num_to_string(n.value)))
        }
        _ => {
            let e = k.as_expression().ok_or("unsupported object key")?;
            Ok(PropKey::Computed(expr(e)?))
        }
    }
}

fn unary(u: &ox::UnaryExpression) -> R<Expr> {
    use ox::UnaryOperator as Op;
    let op = match u.operator {
        Op::UnaryNegation => UnOp::Neg,
        Op::UnaryPlus => UnOp::Plus,
        Op::LogicalNot => UnOp::Not,
        Op::BitwiseNot => UnOp::BitNot,
        Op::Typeof => UnOp::TypeOf,
        Op::Void => UnOp::Void,
        Op::Delete => return Err("`delete` isn't in the v0 JS engine yet".into()),
    };
    Ok(Expr::Unary { op, arg: Box::new(expr(&u.argument)?) })
}

fn update(u: &ox::UpdateExpression) -> R<Expr> {
    let op = match u.operator {
        ox::UpdateOperator::Increment => UpdateOp::Inc,
        ox::UpdateOperator::Decrement => UpdateOp::Dec,
    };
    Ok(Expr::Update {
        op,
        prefix: u.prefix,
        arg: Box::new(simple_target(&u.argument)?),
    })
}

fn binary(b: &ox::BinaryExpression) -> R<Expr> {
    use ox::BinaryOperator as Op;
    let op = match b.operator {
        Op::Addition => BinOp::Add,
        Op::Subtraction => BinOp::Sub,
        Op::Multiplication => BinOp::Mul,
        Op::Division => BinOp::Div,
        Op::Remainder => BinOp::Mod,
        Op::Exponential => BinOp::Pow,
        Op::Equality => BinOp::EqEq,
        Op::Inequality => BinOp::NotEq,
        Op::StrictEquality => BinOp::StrictEq,
        Op::StrictInequality => BinOp::StrictNotEq,
        Op::LessThan => BinOp::Lt,
        Op::LessEqualThan => BinOp::Le,
        Op::GreaterThan => BinOp::Gt,
        Op::GreaterEqualThan => BinOp::Ge,
        Op::BitwiseAnd => BinOp::BitAnd,
        Op::BitwiseOR => BinOp::BitOr,
        Op::BitwiseXOR => BinOp::BitXor,
        Op::ShiftLeft => BinOp::Shl,
        Op::ShiftRight => BinOp::Shr,
        Op::ShiftRightZeroFill => BinOp::UShr,
        Op::In => BinOp::In,
        Op::Instanceof => BinOp::InstanceOf,
    };
    Ok(Expr::Binary {
        op,
        l: Box::new(expr(&b.left)?),
        r: Box::new(expr(&b.right)?),
    })
}

fn logical(l: &ox::LogicalExpression) -> R<Expr> {
    let op = match l.operator {
        ox::LogicalOperator::And => LogicalOp::And,
        ox::LogicalOperator::Or => LogicalOp::Or,
        ox::LogicalOperator::Coalesce => LogicalOp::Nullish,
    };
    Ok(Expr::Logical {
        op,
        l: Box::new(expr(&l.left)?),
        r: Box::new(expr(&l.right)?),
    })
}

fn assign(a: &ox::AssignmentExpression) -> R<Expr> {
    use ox::AssignmentOperator as Op;
    let target = Box::new(assign_target(&a.left)?);
    let value = Box::new(expr(&a.right)?);
    if let Some(op) = match a.operator {
        Op::LogicalAnd => Some(LogicalOp::And),
        Op::LogicalOr => Some(LogicalOp::Or),
        Op::LogicalNullish => Some(LogicalOp::Nullish),
        _ => None,
    } {
        return Ok(Expr::LogicalAssign { op, target, value });
    }
    let op = match a.operator {
        Op::Assign => None,
        Op::Addition => Some(BinOp::Add),
        Op::Subtraction => Some(BinOp::Sub),
        Op::Multiplication => Some(BinOp::Mul),
        Op::Division => Some(BinOp::Div),
        Op::Remainder => Some(BinOp::Mod),
        Op::Exponential => Some(BinOp::Pow),
        Op::BitwiseAnd => Some(BinOp::BitAnd),
        Op::BitwiseOR => Some(BinOp::BitOr),
        Op::BitwiseXOR => Some(BinOp::BitXor),
        Op::ShiftLeft => Some(BinOp::Shl),
        Op::ShiftRight => Some(BinOp::Shr),
        Op::ShiftRightZeroFill => Some(BinOp::UShr),
        _ => return Err("`&&=`/`||=`/`??=` aren't in the v0 JS engine yet".into()),
    };
    Ok(Expr::Assign { op, target, value })
}

fn assign_target(t: &ox::AssignmentTarget) -> R<Expr> {
    match t {
        ox::AssignmentTarget::AssignmentTargetIdentifier(id) => Ok(Expr::Ident(id.name.to_string())),
        _ => {
            let m = t
                .as_member_expression()
                .ok_or("destructuring assignment isn't in the v0 JS engine yet")?;
            member_to_expr(m)
        }
    }
}

fn simple_target(t: &ox::SimpleAssignmentTarget) -> R<Expr> {
    match t {
        ox::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
            Ok(Expr::Ident(id.name.to_string()))
        }
        _ => {
            let m = t.as_member_expression().ok_or("unsupported update/assignment target")?;
            member_to_expr(m)
        }
    }
}

fn member_to_expr(m: &ox::MemberExpression) -> R<Expr> {
    match m {
        ox::MemberExpression::StaticMemberExpression(s) => Ok(Expr::Member {
            obj: Box::new(expr(&s.object)?),
            prop: Box::new(Expr::Str(s.property.name.as_str().into())),
            computed: false,
        }),
        ox::MemberExpression::ComputedMemberExpression(c) => Ok(Expr::Member {
            obj: Box::new(expr(&c.object)?),
            prop: Box::new(expr(&c.expression)?),
            computed: true,
        }),
        ox::MemberExpression::PrivateFieldExpression(_) => {
            Err("private fields aren't in the v0 JS engine yet".into())
        }
    }
}

fn call_args(args: &oxc_allocator::Vec<ox::Argument>) -> R<Vec<Expr>> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        match a {
            ox::Argument::SpreadElement(s) => out.push(Expr::Spread(Box::new(expr(&s.argument)?))),
            other => out.push(expr(other.as_expression().ok_or("bad call argument")?)?),
        }
    }
    Ok(out)
}

fn call(c: &ox::CallExpression) -> R<Expr> {
    Ok(Expr::Call {
        callee: Box::new(expr(&c.callee)?),
        args: call_args(&c.arguments)?,
    })
}
