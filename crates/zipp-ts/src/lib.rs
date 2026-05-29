//! ZIPP **TypeScript frontend**: parse real `.ts` with `oxc`, then lower the
//! sound subset to ZIPP's own AST (`zippc::ast`). Everything downstream — the
//! sound checker, IR, GC, and all four execution targets (interp/jit/llvm/wasm)
//! plus the zk prover — is reused unchanged.
//!
//! `oxc` parses *all* TypeScript syntax; this module lowers the subset that maps
//! to ZIPP's sound, AOT-compilable core and rejects the rest with a clear,
//! line-numbered error. That's the AssemblyScript model: write real TS (tsc and
//! your editor type-check it), compile it to fast/provable/gas-metered code.
//!
//! Supported (v0): typed functions + recursion, `let`/`const` with annotations,
//! `if`/`while`/`for`, `break`/`continue`, return, the full operator set,
//! numeric casts (`i64(x)`, `u32(x)`, …), arrays (`T[]`, indexing, `.length`),
//! `console.log`/`print`, and the math builtins. Type mapping: `number`→f64,
//! `bigint`→i64, `boolean`→bool, `string`→str, and the ZIPP type names
//! `i64`/`i32`/`u32`/`u64`/`f64` usable directly. `interface`s and `class`es
//! become ZIPP structs (a class lowers to a `C__new` factory plus methods that
//! take `this` as their first parameter; `new`/`this`/`obj.method()` are
//! rewritten accordingly). Not yet: generics, closures, inheritance — and never
//! the dynamic core (`any`, prototypes, `eval`, exceptions, async), which is
//! off-mission for an AOT/provable language.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};
use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator};

use zippc::ast as z;

/// Parse TypeScript source and lower the sound subset to a ZIPP [`z::Module`].
pub fn compile_ts(src: &str) -> Result<z::Module, String> {
    let allocator = Allocator::default();
    let source_type = SourceType::ts();
    let ret = Parser::new(&allocator, src, source_type).parse();
    if !ret.errors.is_empty() {
        return Err(format!("typescript parse error: {}", ret.errors[0]));
    }
    let mut lower = Lower {
        src,
        structs: HashMap::new(),
        struct_fields: Vec::new(),
        fn_rets: HashMap::new(),
        scope: RefCell::new(Vec::new()),
        ret_struct: Cell::new(None),
    };
    lower.module(&ret.program)
}

struct Lower<'s> {
    src: &'s str,
    /// Interface/class name → struct index (in `Module::structs`).
    structs: HashMap<String, u32>,
    /// Parallel to `structs`: struct id → its `(field, type)` list. Used to infer
    /// `obj.field` types (and so resolve method-call receivers).
    struct_fields: Vec<Vec<(String, z::Type)>>,
    /// Every callable's return type — top-level functions, class factories
    /// (`C__new`), and methods (`C__m`). Lets a method call resolve a receiver
    /// expression whose type comes from a call result.
    fn_rets: HashMap<String, z::Type>,
    /// Per-function variable → type environment (drives `obj.method()` dispatch).
    scope: RefCell<Vec<HashMap<String, z::Type>>>,
    /// Struct id of the current function's return type (for `return {...}`).
    ret_struct: Cell<Option<u32>>,
}

type LResult<T> = Result<T, String>;

impl Lower<'_> {
    fn line(&self, span: Span) -> u32 {
        self.src.as_bytes()[..(span.start as usize).min(self.src.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count() as u32
            + 1
    }

    fn err(&self, span: Span, msg: impl std::fmt::Display) -> String {
        format!("typescript error [line {}]: {msg}", self.line(span))
    }

    fn module(&mut self, program: &Program) -> LResult<z::Module> {
        // Pass 1a: register every interface/class name → struct id (source order),
        // so type references and constructions resolve regardless of order.
        let mut next_id = 0u32;
        for stmt in &program.body {
            let (name, span) = match stmt {
                Statement::TSInterfaceDeclaration(i) => (i.id.name.as_str(), i.span),
                Statement::ClassDeclaration(c) => {
                    let id = c
                        .id
                        .as_ref()
                        .ok_or_else(|| self.err(c.span, "classes must be named"))?;
                    (id.name.as_str(), c.span)
                }
                _ => continue,
            };
            if self.structs.contains_key(name) {
                return Err(self.err(span, format!("type '{name}' redefined")));
            }
            self.structs.insert(name.to_string(), next_id);
            next_id += 1;
        }

        // Pass 1b: lower struct bodies (interface fields / class fields).
        let mut structs: Vec<z::StructDecl> = Vec::new();
        for stmt in &program.body {
            match stmt {
                Statement::TSInterfaceDeclaration(i) => structs.push(self.interface(i)?),
                Statement::ClassDeclaration(c) => structs.push(self.class_struct(c)?),
                _ => continue,
            }
        }
        self.struct_fields = structs.iter().map(|s| s.fields.clone()).collect();

        // Pass 1c: collect the return type of every callable up front, so method
        // bodies can resolve the type of a call result (for dispatch).
        for stmt in &program.body {
            match stmt {
                Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        let r = self.fn_ret_type(f)?;
                        self.fn_rets.insert(id.name.as_str().to_string(), r);
                    }
                }
                Statement::ClassDeclaration(c) => {
                    let cname = c.id.as_ref().unwrap().name.as_str().to_string();
                    let cid = self.structs[cname.as_str()];
                    self.fn_rets.insert(format!("{cname}__new"), z::Type::Struct(cid));
                    for el in &c.body.body {
                        if let ClassElement::MethodDefinition(m) = el {
                            if matches!(m.kind, MethodDefinitionKind::Method) {
                                let mname = self.prop_name(&m.key, m.span)?;
                                let r = self.fn_ret_type(&m.value)?;
                                self.fn_rets.insert(format!("{cname}__{mname}"), r);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Pass 2: lower all bodies — top-level functions, then class factories
        // and methods (emitted as ordinary ZIPP functions).
        let mut funcs = Vec::new();
        for stmt in &program.body {
            match stmt {
                Statement::FunctionDeclaration(f) => funcs.push(self.func(f)?),
                Statement::ClassDeclaration(c) => self.lower_class(c, &mut funcs)?,
                Statement::TSInterfaceDeclaration(_)
                | Statement::TSTypeAliasDeclaration(_)
                | Statement::EmptyStatement(_) => {}
                other => {
                    return Err(self.err(
                        span_of_stmt(other),
                        "only top-level functions, classes, and interfaces are supported (v0)",
                    ))
                }
            }
        }
        Ok(z::Module { funcs, structs })
    }

    fn interface(&self, decl: &TSInterfaceDeclaration) -> LResult<z::StructDecl> {
        let name = decl.id.name.as_str().to_string();
        let mut fields = Vec::new();
        for sig in &decl.body.body {
            match sig {
                TSSignature::TSPropertySignature(p) => {
                    let fname = match &p.key {
                        PropertyKey::StaticIdentifier(id) => id.name.as_str().to_string(),
                        _ => return Err(self.err(p.span, "interface field keys must be plain names")),
                    };
                    if p.optional {
                        return Err(self.err(p.span, "optional fields aren't supported yet"));
                    }
                    let ann = p
                        .type_annotation
                        .as_ref()
                        .ok_or_else(|| self.err(p.span, format!("field '{fname}' needs a type")))?;
                    fields.push((fname, self.ty(&ann.type_annotation)?));
                }
                _ => return Err(self.err(decl.span, "interface methods/index signatures aren't supported")),
            }
        }
        Ok(z::StructDecl { name, fields })
    }

    /// Lower an object literal `{ x: 1, y: 2 }` to a nominal `StructLit` for the
    /// struct identified by `id` (the ZIPP checker reorders/validates fields).
    fn obj_struct_lit(&self, obj: &ObjectExpression, id: u32) -> LResult<z::Expr> {
        let name = self
            .structs
            .iter()
            .find(|(_, &v)| v == id)
            .map(|(k, _)| k.clone())
            .ok_or_else(|| self.err(obj.span, "internal: unknown struct id"))?;
        let mut fields = Vec::new();
        for p in &obj.properties {
            match p {
                ObjectPropertyKind::ObjectProperty(op) => {
                    let fname = match &op.key {
                        PropertyKey::StaticIdentifier(k) => k.name.as_str().to_string(),
                        _ => return Err(self.err(op.span, "object keys must be plain names")),
                    };
                    fields.push((fname, self.expr(&op.value)?));
                }
                ObjectPropertyKind::SpreadProperty(_) => {
                    return Err(self.err(obj.span, "object spread isn't supported"))
                }
            }
        }
        Ok(z::Expr::StructLit { name, fields })
    }

    // ── classes ──────────────────────────────────────────────────────────────
    // A `class C` lowers to: a struct `C` (its fields), a factory function
    // `C__new(ctorParams): C` (build `this` from defaults, run the constructor
    // body, return it), and one free function `C__m(this: C, …)` per method.
    // `new C(a)` → `C__new(a)`, `obj.m(a)` → `C__m(obj, a)`, `this` → a local.

    /// A property/method key as a plain name (rejects computed/private keys).
    fn prop_name(&self, key: &PropertyKey, span: Span) -> LResult<String> {
        match key {
            PropertyKey::StaticIdentifier(id) => Ok(id.name.as_str().to_string()),
            PropertyKey::PrivateIdentifier(_) => {
                Err(self.err(span, "private members (`#x`) aren't supported"))
            }
            _ => Err(self.err(span, "computed member names aren't supported")),
        }
    }

    /// Lower a class's fields to a `StructDecl` (methods are lowered separately).
    fn class_struct(&self, c: &Class) -> LResult<z::StructDecl> {
        let name = c.id.as_ref().unwrap().name.as_str().to_string();
        if c.super_class.is_some() {
            return Err(self.err(c.span, "class inheritance (`extends`) isn't supported"));
        }
        if c.type_parameters.is_some() {
            return Err(self.err(c.span, "generic classes aren't supported yet"));
        }
        let mut fields = Vec::new();
        for el in &c.body.body {
            match el {
                ClassElement::PropertyDefinition(p) => {
                    if p.r#static {
                        return Err(self.err(p.span, "static fields aren't supported"));
                    }
                    if p.computed {
                        return Err(self.err(p.span, "computed field names aren't supported"));
                    }
                    let fname = self.prop_name(&p.key, p.span)?;
                    let ann = p.type_annotation.as_ref().ok_or_else(|| {
                        self.err(p.span, format!("field '{fname}' needs a type annotation"))
                    })?;
                    fields.push((fname, self.ty(&ann.type_annotation)?));
                }
                ClassElement::MethodDefinition(_) => {} // lowered in `lower_class`
                ClassElement::StaticBlock(s) => {
                    return Err(self.err(s.span, "static blocks aren't supported"))
                }
                ClassElement::AccessorProperty(a) => {
                    return Err(self.err(a.span, "accessor properties aren't supported"))
                }
                ClassElement::TSIndexSignature(i) => {
                    return Err(self.err(i.span, "index signatures aren't supported"))
                }
            }
        }
        Ok(z::StructDecl { name, fields })
    }

    /// Emit a class's factory + method functions into `funcs`.
    fn lower_class(&self, c: &Class, funcs: &mut Vec<z::Func>) -> LResult<()> {
        let cname = c.id.as_ref().unwrap().name.as_str().to_string();
        let cid = self.structs[cname.as_str()];
        funcs.push(self.class_factory(c, cid)?);
        for el in &c.body.body {
            if let ClassElement::MethodDefinition(m) = el {
                if m.r#static {
                    return Err(self.err(m.span, "static methods aren't supported"));
                }
                if m.computed {
                    return Err(self.err(m.span, "computed method names aren't supported"));
                }
                match m.kind {
                    MethodDefinitionKind::Method => funcs.push(self.class_method(&cname, cid, m)?),
                    MethodDefinitionKind::Constructor => {} // folded into the factory
                    MethodDefinitionKind::Get | MethodDefinitionKind::Set => {
                        return Err(self.err(m.span, "getters/setters aren't supported"))
                    }
                }
            }
        }
        Ok(())
    }

    /// Synthesize `C__new`: `let this: C = {defaults}; <ctor body>; return this;`.
    fn class_factory(&self, c: &Class, cid: u32) -> LResult<z::Func> {
        let cname = self.struct_name(cid).unwrap();
        let ctor = c.body.body.iter().find_map(|el| match el {
            ClassElement::MethodDefinition(m)
                if matches!(m.kind, MethodDefinitionKind::Constructor) =>
            {
                Some(m)
            }
            _ => None,
        });
        self.push_scope();
        let mut params = Vec::new();
        if let Some(ctor) = ctor {
            for p in &ctor.value.params.items {
                let (pname, pty) = self.param(p)?;
                self.bind(&pname, pty);
                params.push(z::Param { name: pname, ty: pty });
            }
        }
        self.bind("this", z::Type::Struct(cid));
        let line = self.line(c.span);
        let mut body = vec![z::Stmt {
            kind: z::StmtKind::Let {
                name: "this".into(),
                ty: Some(z::Type::Struct(cid)),
                value: self.class_default_lit(c, cid)?,
            },
            line,
        }];
        // A constructor body never returns an object literal, so disable that path.
        let prev = self.ret_struct.replace(None);
        if let Some(ctor) = ctor {
            if let Some(b) = &ctor.value.body {
                for s in &b.statements {
                    self.stmt(s, &mut body)?;
                }
            }
        }
        self.ret_struct.set(prev);
        body.push(z::Stmt {
            kind: z::StmtKind::Return(Some(z::Expr::Var("this".into()))),
            line,
        });
        self.pop_scope();
        Ok(z::Func { name: format!("{cname}__new"), params, ret: z::Type::Struct(cid), body })
    }

    /// The initial struct literal for `this`: each field's initializer, or a
    /// type default (the constructor typically overwrites these).
    fn class_default_lit(&self, c: &Class, cid: u32) -> LResult<z::Expr> {
        let name = self.struct_name(cid).unwrap();
        let mut fields = Vec::new();
        for el in &c.body.body {
            if let ClassElement::PropertyDefinition(p) = el {
                let fname = self.prop_name(&p.key, p.span)?;
                let ann = p.type_annotation.as_ref().ok_or_else(|| {
                    self.err(p.span, format!("field '{fname}' needs a type annotation"))
                })?;
                let fty = self.ty(&ann.type_annotation)?;
                let val = match &p.value {
                    Some(init) => self.expr(init)?,
                    None => self.type_default(fty).ok_or_else(|| {
                        self.err(
                            p.span,
                            format!(
                                "field '{fname}' needs an initializer (e.g. `{fname}: T = …`) — \
                                 its type has no default value"
                            ),
                        )
                    })?,
                };
                fields.push((fname, val));
            }
        }
        Ok(z::Expr::StructLit { name, fields })
    }

    /// Lower a method to `C__m(this: C, …params): ret { …body… }`.
    fn class_method(&self, cname: &str, cid: u32, m: &MethodDefinition) -> LResult<z::Func> {
        let mname = self.prop_name(&m.key, m.span)?;
        let f = &m.value;
        self.push_scope();
        self.bind("this", z::Type::Struct(cid));
        let mut params = vec![z::Param { name: "this".into(), ty: z::Type::Struct(cid) }];
        for p in &f.params.items {
            let (pname, pty) = self.param(p)?;
            self.bind(&pname, pty);
            params.push(z::Param { name: pname, ty: pty });
        }
        let ret = self.fn_ret_type(f)?;
        let prev = self.ret_struct.replace(match ret {
            z::Type::Struct(id) => Some(id),
            _ => None,
        });
        let body = match &f.body {
            Some(b) => self.block(&b.statements)?,
            None => return Err(self.err(m.span, "method has no body")),
        };
        self.ret_struct.set(prev);
        self.pop_scope();
        Ok(z::Func { name: format!("{cname}__{mname}"), params, ret, body })
    }

    /// A zero/empty default value for a class field type (scalars and strings).
    fn type_default(&self, ty: z::Type) -> Option<z::Expr> {
        Some(match ty {
            z::Type::I64 | z::Type::I32 | z::Type::U32 | z::Type::U64 => z::Expr::Int(0),
            z::Type::F64 => z::Expr::Float(0.0),
            z::Type::Bool => z::Expr::Bool(false),
            z::Type::Str => z::Expr::Str(String::new()),
            z::Type::Array(_) | z::Type::Struct(_) => return None,
        })
    }

    /// The declared name of a struct id (reverse of `self.structs`).
    fn struct_name(&self, id: u32) -> Option<String> {
        self.structs.iter().find(|(_, &v)| v == id).map(|(k, _)| k.clone())
    }

    /// Best-effort static type of a lowered expression — enough to resolve a
    /// method-call receiver to its class. `None` when unknown (then a method
    /// call errors asking for an annotation).
    fn ztype_of(&self, e: &z::Expr) -> Option<z::Type> {
        Some(match e {
            z::Expr::Int(_) => z::Type::I64,
            z::Expr::Float(_) => z::Type::F64,
            z::Expr::Bool(_) => z::Type::Bool,
            z::Expr::Str(_) => z::Type::Str,
            z::Expr::Var(n) => self.lookup(n)?,
            z::Expr::Cast { to, .. } => *to,
            z::Expr::Call { name, .. } => *self.fn_rets.get(name)?,
            z::Expr::StructLit { name, .. } => z::Type::Struct(*self.structs.get(name)?),
            z::Expr::Field { base, field } => {
                let z::Type::Struct(id) = self.ztype_of(base)? else { return None };
                let fields = self.struct_fields.get(id as usize)?;
                *fields.iter().find(|(n, _)| n == field).map(|(_, t)| t)?
            }
            _ => return None,
        })
    }

    fn push_scope(&self) {
        self.scope.borrow_mut().push(HashMap::new());
    }
    fn pop_scope(&self) {
        self.scope.borrow_mut().pop();
    }
    fn bind(&self, name: &str, ty: z::Type) {
        if let Some(top) = self.scope.borrow_mut().last_mut() {
            top.insert(name.to_string(), ty);
        }
    }
    fn lookup(&self, name: &str) -> Option<z::Type> {
        self.scope.borrow().iter().rev().find_map(|m| m.get(name).copied())
    }

    fn func(&self, f: &Function) -> LResult<z::Func> {
        let name = f
            .id
            .as_ref()
            .ok_or_else(|| self.err(f.span, "functions must be named"))?
            .name
            .as_str()
            .to_string();
        self.push_scope();
        let mut params = Vec::new();
        for p in &f.params.items {
            let (pname, pty) = self.param(p)?;
            self.bind(&pname, pty);
            params.push(z::Param { name: pname, ty: pty });
        }
        let ret = self.fn_ret_type(f)?;
        self.ret_struct.set(match ret {
            z::Type::Struct(id) => Some(id),
            _ => None,
        });
        let body = match &f.body {
            Some(b) => self.block(&b.statements)?,
            None => return Err(self.err(f.span, "function has no body")),
        };
        self.pop_scope();
        Ok(z::Func { name, params, ret, body })
    }

    /// Lower one formal parameter to `(name, type)`. Rejects destructuring and
    /// untyped params.
    fn param(&self, p: &FormalParameter) -> LResult<(String, z::Type)> {
        let pname = match &p.pattern {
            BindingPattern::BindingIdentifier(bi) => bi.name.as_str().to_string(),
            _ => return Err(self.err(p.span, "destructuring parameters aren't supported")),
        };
        let ann = p
            .type_annotation
            .as_ref()
            .ok_or_else(|| self.err(p.span, format!("parameter '{pname}' needs a type")))?;
        Ok((pname, self.ty(&ann.type_annotation)?))
    }

    /// A function's declared return type (required — ZIPP is explicitly typed).
    fn fn_ret_type(&self, f: &Function) -> LResult<z::Type> {
        match &f.return_type {
            Some(ann) => self.ty(&ann.type_annotation),
            None => Err(self.err(f.span, "functions need an explicit return type")),
        }
    }

    fn ty(&self, t: &TSType) -> LResult<z::Type> {
        Ok(match t {
            TSType::TSNumberKeyword(_) => z::Type::F64, // TS `number` is f64
            TSType::TSBigIntKeyword(_) => z::Type::I64,
            TSType::TSBooleanKeyword(_) => z::Type::Bool,
            TSType::TSStringKeyword(_) => z::Type::Str,
            TSType::TSArrayType(a) => {
                let elem = self.ty(&a.element_type)?;
                let e = elem.as_elem().ok_or_else(|| {
                    self.err(a.span, format!("array element type {elem:?} isn't a scalar"))
                })?;
                z::Type::Array(e)
            }
            TSType::TSTypeReference(r) => {
                let name = match &r.type_name {
                    TSTypeName::IdentifierReference(id) => id.name.as_str(),
                    _ => return Err(self.err(r.span, "qualified type names aren't supported")),
                };
                match name {
                    "i64" => z::Type::I64,
                    "i32" => z::Type::I32,
                    "u32" => z::Type::U32,
                    "u64" => z::Type::U64,
                    "f64" => z::Type::F64,
                    "bool" => z::Type::Bool,
                    "string" | "str" => z::Type::Str,
                    other => match self.structs.get(other) {
                        Some(&id) => z::Type::Struct(id),
                        None => return Err(self.err(r.span, format!("unknown type '{other}'"))),
                    },
                }
            }
            TSType::TSParenthesizedType(p) => self.ty(&p.type_annotation)?,
            other => return Err(self.err(span_of_type(other), "unsupported type")),
        })
    }

    fn block(&self, stmts: &[Statement]) -> LResult<Vec<z::Stmt>> {
        let mut out = Vec::new();
        for s in stmts {
            self.stmt(s, &mut out)?;
        }
        Ok(out)
    }

    /// Lower a statement, pushing one or more ZIPP statements.
    fn stmt(&self, s: &Statement, out: &mut Vec<z::Stmt>) -> LResult<()> {
        let push = |out: &mut Vec<z::Stmt>, kind, span| {
            out.push(z::Stmt { kind, line: self.line(span) })
        };
        match s {
            Statement::VariableDeclaration(v) => self.var_decl(v, out)?,
            Statement::ReturnStatement(r) => {
                let val = match (&r.argument, self.ret_struct.get()) {
                    // `return { ... }` from a struct-typed function.
                    (Some(Expression::ObjectExpression(obj)), Some(id)) => {
                        Some(self.obj_struct_lit(obj, id)?)
                    }
                    (Some(e), _) => Some(self.expr(e)?),
                    (None, _) => None,
                };
                push(out, z::StmtKind::Return(val), r.span);
            }
            Statement::IfStatement(i) => {
                let cond = self.expr(&i.test)?;
                let then_b = self.stmt_as_block(&i.consequent)?;
                let else_b = match &i.alternate {
                    Some(a) => self.stmt_as_block(a)?,
                    None => Vec::new(),
                };
                push(out, z::StmtKind::If { cond, then_b, else_b }, i.span);
            }
            Statement::WhileStatement(w) => {
                let cond = self.expr(&w.test)?;
                let body = self.stmt_as_block(&w.body)?;
                push(out, z::StmtKind::While { cond, body }, w.span);
            }
            Statement::ForStatement(f) => {
                let init = match &f.init {
                    Some(ForStatementInit::VariableDeclaration(v)) => {
                        let mut tmp = Vec::new();
                        self.var_decl(v, &mut tmp)?;
                        tmp.pop().map(Box::new)
                    }
                    Some(other) => {
                        let e = other
                            .as_expression()
                            .ok_or_else(|| self.err(f.span, "unsupported for-init"))?;
                        Some(Box::new(self.expr_stmt(e)?))
                    }
                    None => None,
                };
                let cond = match &f.test {
                    Some(e) => self.expr(e)?,
                    None => z::Expr::Bool(true),
                };
                let step = match &f.update {
                    Some(e) => Some(Box::new(self.expr_stmt(e)?)),
                    None => None,
                };
                let body = self.stmt_as_block(&f.body)?;
                push(out, z::StmtKind::For { init, cond, step, body }, f.span);
            }
            Statement::BreakStatement(b) => push(out, z::StmtKind::Break, b.span),
            Statement::ContinueStatement(c) => push(out, z::StmtKind::Continue, c.span),
            Statement::BlockStatement(b) => {
                // Flatten a bare block (v0 — no separate block scope).
                for s in &b.body {
                    self.stmt(s, out)?;
                }
            }
            Statement::EmptyStatement(_) => {}
            Statement::ExpressionStatement(e) => {
                let st = self.expr_stmt(&e.expression)?;
                out.push(st);
            }
            other => return Err(self.err(span_of_stmt(other), "unsupported statement")),
        }
        Ok(())
    }

    /// Lower a `let`/`const` (one or more declarators) into ZIPP `Let`s.
    fn var_decl(&self, v: &VariableDeclaration, out: &mut Vec<z::Stmt>) -> LResult<()> {
        for d in &v.declarations {
            let name = match &d.id {
                BindingPattern::BindingIdentifier(bi) => bi.name.as_str().to_string(),
                _ => return Err(self.err(d.span, "destructuring `let` isn't supported")),
            };
            let value = d
                .init
                .as_ref()
                .ok_or_else(|| self.err(d.span, format!("'{name}' must be initialized")))?;
            let ty = match &d.type_annotation {
                Some(a) => Some(self.ty(&a.type_annotation)?),
                None => None,
            };
            // `let p: Point = { ... }` — the annotation names the struct.
            let value = match (&ty, value) {
                (Some(z::Type::Struct(id)), Expression::ObjectExpression(obj)) => {
                    self.obj_struct_lit(obj, *id)?
                }
                _ => self.expr(value)?,
            };
            // Track the variable's type (annotation, else inferred) for dispatch.
            if let Some(t) = ty.or_else(|| self.ztype_of(&value)) {
                self.bind(&name, t);
            }
            out.push(z::Stmt {
                kind: z::StmtKind::Let { name, ty, value },
                line: self.line(d.span),
            });
        }
        Ok(())
    }

    /// Lower a `Statement` that may or may not be a block into a ZIPP block.
    fn stmt_as_block(&self, s: &Statement) -> LResult<Vec<z::Stmt>> {
        if let Statement::BlockStatement(b) = s {
            self.block(&b.body)
        } else {
            let mut out = Vec::new();
            self.stmt(s, &mut out)?;
            Ok(out)
        }
    }

    /// An expression used in statement position: assignment, `print`, or a bare
    /// expression.
    fn expr_stmt(&self, e: &Expression) -> LResult<z::Stmt> {
        let line = self.line(span_of_expr(e));
        let kind = match e {
            Expression::AssignmentExpression(a) => {
                let target = self.assign_target(&a.left)?;
                let value = self.expr(&a.right)?;
                let value = match assign_bin_op(a.operator) {
                    None => value, // plain `=`
                    Some(op) => z::Expr::Bin {
                        op,
                        l: Box::new(target.clone()),
                        r: Box::new(value),
                    },
                };
                z::StmtKind::Assign { target, value }
            }
            // `print(x)` / `console.log(x)` → ZIPP print.
            Expression::CallExpression(c) if self.is_print_call(c) => {
                let arg = c
                    .arguments
                    .first()
                    .and_then(|a| a.as_expression())
                    .ok_or_else(|| self.err(c.span, "print expects one argument"))?;
                z::StmtKind::Print(self.expr(arg)?)
            }
            _ => z::StmtKind::ExprStmt(self.expr(e)?),
        };
        Ok(z::Stmt { kind, line })
    }

    fn is_print_call(&self, c: &CallExpression) -> bool {
        match &c.callee {
            Expression::Identifier(id) => id.name.as_str() == "print",
            Expression::StaticMemberExpression(m) => {
                m.property.name.as_str() == "log"
                    && matches!(&m.object, Expression::Identifier(o) if o.name.as_str() == "console")
            }
            _ => false,
        }
    }

    fn assign_target(&self, t: &AssignmentTarget) -> LResult<z::Expr> {
        match t {
            AssignmentTarget::AssignmentTargetIdentifier(id) => {
                Ok(z::Expr::Var(id.name.as_str().to_string()))
            }
            AssignmentTarget::ComputedMemberExpression(m) => Ok(z::Expr::Index {
                arr: Box::new(self.expr(&m.object)?),
                index: Box::new(self.expr(&m.expression)?),
            }),
            // `obj.field = v`
            AssignmentTarget::StaticMemberExpression(m) => Ok(z::Expr::Field {
                base: Box::new(self.expr(&m.object)?),
                field: m.property.name.as_str().to_string(),
            }),
            _ => Err(self.err(span_of_assign_target(t), "unsupported assignment target")),
        }
    }

    fn expr(&self, e: &Expression) -> LResult<z::Expr> {
        Ok(match e {
            Expression::NumericLiteral(n) => {
                if numeric_is_float(n) {
                    z::Expr::Float(n.value)
                } else {
                    z::Expr::Int(n.value as i64)
                }
            }
            Expression::StringLiteral(s) => z::Expr::Str(s.value.as_str().to_string()),
            Expression::BooleanLiteral(b) => z::Expr::Bool(b.value),
            Expression::Identifier(id) => z::Expr::Var(id.name.as_str().to_string()),
            Expression::ParenthesizedExpression(p) => self.expr(&p.expression)?,
            Expression::BinaryExpression(b) => z::Expr::Bin {
                op: self.bin_op(b.operator, b.span)?,
                l: Box::new(self.expr(&b.left)?),
                r: Box::new(self.expr(&b.right)?),
            },
            Expression::LogicalExpression(l) => z::Expr::Bin {
                op: match l.operator {
                    LogicalOperator::And => z::BinOp::And,
                    LogicalOperator::Or => z::BinOp::Or,
                    LogicalOperator::Coalesce => {
                        return Err(self.err(l.span, "`??` is not supported"))
                    }
                },
                l: Box::new(self.expr(&l.left)?),
                r: Box::new(self.expr(&l.right)?),
            },
            Expression::UnaryExpression(u) => {
                let inner = self.expr(&u.argument)?;
                match u.operator {
                    UnaryOperator::UnaryNegation => z::Expr::Unary { op: z::UnOp::Neg, e: Box::new(inner) },
                    UnaryOperator::LogicalNot => z::Expr::Unary { op: z::UnOp::Not, e: Box::new(inner) },
                    UnaryOperator::BitwiseNot => z::Expr::Unary { op: z::UnOp::BitNot, e: Box::new(inner) },
                    UnaryOperator::UnaryPlus => inner, // no-op
                    _ => return Err(self.err(u.span, "unsupported unary operator")),
                }
            }
            Expression::CallExpression(c) => self.call(c)?,
            Expression::ArrayExpression(a) => {
                let mut elems = Vec::new();
                for el in &a.elements {
                    let ex = el
                        .as_expression()
                        .ok_or_else(|| self.err(a.span, "array holes/spreads aren't supported"))?;
                    elems.push(self.expr(ex)?);
                }
                z::Expr::Array(elems)
            }
            Expression::ComputedMemberExpression(m) => z::Expr::Index {
                arr: Box::new(self.expr(&m.object)?),
                index: Box::new(self.expr(&m.expression)?),
            },
            Expression::StaticMemberExpression(m) => {
                if m.property.name.as_str() == "length" {
                    // `arr.length` → len(arr)
                    z::Expr::Call { name: "len".into(), args: vec![self.expr(&m.object)?] }
                } else {
                    // `obj.field` → struct field access
                    z::Expr::Field {
                        base: Box::new(self.expr(&m.object)?),
                        field: m.property.name.as_str().to_string(),
                    }
                }
            }
            // `x as T`: numeric → runtime conversion; `{...} as Struct` → struct
            // literal; otherwise a no-op type assertion.
            Expression::TSAsExpression(a) => {
                let to = self.ty(&a.type_annotation)?;
                if to.is_numeric() {
                    z::Expr::Cast { to, e: Box::new(self.expr(&a.expression)?) }
                } else if let (z::Type::Struct(id), Expression::ObjectExpression(obj)) =
                    (to, &a.expression)
                {
                    self.obj_struct_lit(obj, id)?
                } else {
                    self.expr(&a.expression)?
                }
            }
            // `this` inside a method/constructor → the synthetic `this` binding.
            Expression::ThisExpression(_) => z::Expr::Var("this".into()),
            // `new C(args)` → the class factory `C__new(args)`.
            Expression::NewExpression(n) => {
                let cname = match &n.callee {
                    Expression::Identifier(id) => id.name.as_str().to_string(),
                    _ => return Err(self.err(n.span, "`new` requires a class name")),
                };
                let factory = format!("{cname}__new");
                if !self.fn_rets.contains_key(&factory) {
                    return Err(self.err(n.span, format!("`new {cname}(…)`: '{cname}' isn't a class")));
                }
                let mut args = Vec::new();
                for a in &n.arguments {
                    let ex = a
                        .as_expression()
                        .ok_or_else(|| self.err(n.span, "spread arguments aren't supported"))?;
                    args.push(self.expr(ex)?);
                }
                z::Expr::Call { name: factory, args }
            }
            Expression::ObjectExpression(o) => {
                return Err(self.err(
                    o.span,
                    "object literal needs a known type — annotate the `let`, write `{...} as T`, \
                     or return it from a struct-typed function",
                ))
            }
            other => return Err(self.err(span_of_expr(other), "unsupported expression")),
        })
    }

    /// A call is a method call (`obj.m(args)`), a numeric cast (`i64(x)`),
    /// `len`/a math builtin, or a user function call.
    fn call(&self, c: &CallExpression) -> LResult<z::Expr> {
        // Method call: `obj.m(args)` → `Class__m(obj, args)`. Resolve the
        // receiver's class via the type tracker.
        if let Expression::StaticMemberExpression(m) = &c.callee {
            let method = m.property.name.as_str();
            let recv = self.expr(&m.object)?;
            let cid = match self.ztype_of(&recv) {
                Some(z::Type::Struct(id)) => id,
                _ => {
                    return Err(self.err(
                        c.span,
                        format!(
                            "can't resolve method '.{method}()' — the receiver's type is unknown; \
                             annotate the variable or call it on a class instance"
                        ),
                    ))
                }
            };
            let cname = self
                .struct_name(cid)
                .ok_or_else(|| self.err(c.span, "internal: unknown struct id"))?;
            let mut args = vec![recv];
            for a in &c.arguments {
                let ex = a
                    .as_expression()
                    .ok_or_else(|| self.err(c.span, "spread arguments aren't supported"))?;
                args.push(self.expr(ex)?);
            }
            return Ok(z::Expr::Call { name: format!("{cname}__{method}"), args });
        }
        let name = match &c.callee {
            Expression::Identifier(id) => id.name.as_str().to_string(),
            _ => return Err(self.err(c.span, "only direct function calls are supported")),
        };
        let mut args = Vec::new();
        for a in &c.arguments {
            let ex = a
                .as_expression()
                .ok_or_else(|| self.err(c.span, "spread arguments aren't supported"))?;
            args.push(self.expr(ex)?);
        }
        // Numeric cast keywords used as calls.
        let cast = match name.as_str() {
            "i64" => Some(z::Type::I64),
            "i32" => Some(z::Type::I32),
            "u32" => Some(z::Type::U32),
            "u64" => Some(z::Type::U64),
            "f64" => Some(z::Type::F64),
            _ => None,
        };
        if let Some(to) = cast {
            if args.len() != 1 {
                return Err(self.err(c.span, format!("{name}(x) takes one argument")));
            }
            return Ok(z::Expr::Cast { to, e: Box::new(args.into_iter().next().unwrap()) });
        }
        Ok(z::Expr::Call { name, args })
    }

    fn bin_op(&self, op: BinaryOperator, span: Span) -> LResult<z::BinOp> {
        use BinaryOperator as B;
        Ok(match op {
            B::Addition => z::BinOp::Add,
            B::Subtraction => z::BinOp::Sub,
            B::Multiplication => z::BinOp::Mul,
            B::Division => z::BinOp::Div,
            B::Remainder => z::BinOp::Mod,
            B::Equality | B::StrictEquality => z::BinOp::Eq,
            B::Inequality | B::StrictInequality => z::BinOp::Ne,
            B::LessThan => z::BinOp::Lt,
            B::LessEqualThan => z::BinOp::Le,
            B::GreaterThan => z::BinOp::Gt,
            B::GreaterEqualThan => z::BinOp::Ge,
            B::BitwiseAnd => z::BinOp::BitAnd,
            B::BitwiseOR => z::BinOp::BitOr,
            B::BitwiseXOR => z::BinOp::BitXor,
            B::ShiftLeft => z::BinOp::Shl,
            B::ShiftRight => z::BinOp::Shr,
            _ => return Err(self.err(span, format!("unsupported operator {op:?}"))),
        })
    }
}

fn numeric_is_float(n: &NumericLiteral) -> bool {
    match &n.raw {
        Some(raw) => {
            let s = raw.as_str();
            if s.starts_with("0x") || s.starts_with("0X") || s.starts_with("0b") || s.starts_with("0o") {
                false
            } else {
                s.contains('.') || s.contains('e') || s.contains('E')
            }
        }
        None => n.value.fract() != 0.0,
    }
}

fn assign_bin_op(op: AssignmentOperator) -> Option<z::BinOp> {
    use AssignmentOperator as A;
    match op {
        A::Assign => None,
        A::Addition => Some(z::BinOp::Add),
        A::Subtraction => Some(z::BinOp::Sub),
        A::Multiplication => Some(z::BinOp::Mul),
        A::Division => Some(z::BinOp::Div),
        A::Remainder => Some(z::BinOp::Mod),
        A::BitwiseAnd => Some(z::BinOp::BitAnd),
        A::BitwiseOR => Some(z::BinOp::BitOr),
        A::BitwiseXOR => Some(z::BinOp::BitXor),
        A::ShiftLeft => Some(z::BinOp::Shl),
        A::ShiftRight => Some(z::BinOp::Shr),
        _ => None, // unsupported compound ops fall through as plain assign (checker will catch type errors)
    }
}

// Span accessors (oxc nodes implement GetSpan, but matching keeps deps minimal).
fn span_of_stmt(s: &Statement) -> Span {
    oxc_span::GetSpan::span(s)
}
fn span_of_expr(e: &Expression) -> Span {
    oxc_span::GetSpan::span(e)
}
fn span_of_type(t: &TSType) -> Span {
    oxc_span::GetSpan::span(t)
}
fn span_of_assign_target(t: &AssignmentTarget) -> Span {
    oxc_span::GetSpan::span(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_i64(ts: &str) -> i64 {
        let module = compile_ts(ts).expect("lower");
        let prog = zippc::compile_module(&module).expect("compile");
        match zippc::vm::run(&prog, false).expect("run").result {
            zippc::Value::I64(x) => x,
            other => panic!("expected i64, got {other:?}"),
        }
    }

    #[test]
    fn interfaces_and_structs() {
        // interface -> struct; typed-let construction; field read/write; struct param
        let ts = "interface Point { x: i64; y: i64; } \
                  function dist2(p: Point): i64 { return p.x * p.x + p.y * p.y; } \
                  function main(): i64 { \
                    let p: Point = { x: 3, y: 4 }; \
                    p.x = p.x + 1; \
                    return dist2(p); }";
        assert_eq!(run_i64(ts), 32);
        // `{...} as T` construction + a struct-returning function
        let ts2 = "interface V { a: i64; b: i64; } \
                   function mk(): V { return { a: 5, b: 6 }; } \
                   function main(): i64 { let v = mk(); return v.a + v.b; }";
        assert_eq!(run_i64(ts2), 11);
    }

    #[test]
    fn classes_with_methods() {
        // fields + constructor + methods + `this` + method dispatch + `new`
        let ts = "class Counter { \
                    n: i64; \
                    constructor(start: i64) { this.n = start; } \
                    bump(by: i64): i64 { this.n = this.n + by; return this.n; } \
                    get(): i64 { return this.n; } \
                  } \
                  function main(): i64 { \
                    let c = new Counter(10); \
                    c.bump(5); \
                    c.bump(7); \
                    return c.get(); }";
        assert_eq!(run_i64(ts), 22);
        // a method returning a fresh instance, then a chained-ish call
        let ts2 = "class Vec2 { \
                     x: i64; y: i64; \
                     constructor(x: i64, y: i64) { this.x = x; this.y = y; } \
                     add(o: Vec2): Vec2 { return new Vec2(this.x + o.x, this.y + o.y); } \
                     sum(): i64 { return this.x + this.y; } \
                   } \
                   function main(): i64 { \
                     let a = new Vec2(1, 2); \
                     let b = new Vec2(3, 4); \
                     let c = a.add(b); \
                     return c.sum(); }";
        assert_eq!(run_i64(ts2), 10);
        // a field initializer (no assignment in the constructor)
        let ts3 = "class Acc { \
                     total: i64 = 100; \
                     constructor() {} \
                     addn(k: i64): i64 { this.total = this.total + k; return this.total; } \
                   } \
                   function main(): i64 { let a = new Acc(); return a.addn(23); }";
        assert_eq!(run_i64(ts3), 123);
    }

    #[test]
    fn lowers_and_runs_fib() {
        let ts = "function fib(n: i64): i64 { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); } \
                  function main(): i64 { return fib(10); }";
        let module = compile_ts(ts).expect("lower");
        let prog = zippc::compile_module(&module).expect("compile");
        let r = zippc::vm::run(&prog, false).expect("run");
        assert_eq!(r.result, zippc::Value::I64(55));
    }
}
