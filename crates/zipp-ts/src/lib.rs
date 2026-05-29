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
//! rewritten accordingly). Generic functions **and classes** are
//! **monomorphized**: every use is specialized to concrete types (inferred or
//! explicit `<T>`) — a generic class instantiation becomes a fresh struct plus a
//! factory and methods — so the backends only ever see concrete code. Not yet:
//! closures, inheritance — and never the dynamic core (`any`, prototypes,
//! `eval`, exceptions, async), which is off-mission for an AOT/provable language.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

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
        structs: RefCell::new(HashMap::new()),
        struct_decls: RefCell::new(Vec::new()),
        fn_rets: RefCell::new(HashMap::new()),
        scope: RefCell::new(Vec::new()),
        ret_struct: Cell::new(None),
        generic_info: HashMap::new(),
        generic_class_info: HashMap::new(),
        fn_defaults: HashMap::new(),
        type_env: RefCell::new(Vec::new()),
        pending: RefCell::new(Vec::new()),
        pending_classes: RefCell::new(Vec::new()),
        done_insts: RefCell::new(HashSet::new()),
        enums: HashMap::new(),
        tmp: Cell::new(0),
    };
    lower.module(&ret.program)
}

struct Lower<'s> {
    src: &'s str,
    /// Interface/class/struct-instantiation name → struct index (in
    /// `Module::structs`). `RefCell` because generic-class instantiations add new
    /// structs on demand during lowering.
    structs: RefCell<HashMap<String, u32>>,
    /// Struct id → its decl. Parallel to `structs`; the module's `structs` vector
    /// is taken from here at the end. Grows as generic classes are instantiated.
    struct_decls: RefCell<Vec<z::StructDecl>>,
    /// Every callable's return type — top-level functions, class factories
    /// (`C__new`), methods (`C__m`), and generic instantiations. Lets a call
    /// (or method) resolve a receiver whose type comes from a call result.
    /// `RefCell` so generic instantiations can register their type at the call.
    fn_rets: RefCell<HashMap<String, z::Type>>,
    /// Per-function variable → type environment (drives `obj.method()` dispatch).
    scope: RefCell<Vec<HashMap<String, z::Type>>>,
    /// Struct id of the current function's return type (for `return {...}`).
    ret_struct: Cell<Option<u32>>,
    /// Generic function name → owned monomorphization metadata.
    generic_info: HashMap<String, GenericInfo>,
    /// Generic class name → owned monomorphization metadata.
    generic_class_info: HashMap<String, ClassGenericInfo>,
    /// Callable name (as emitted) → per-parameter default value, lowered once.
    /// A call site appends the missing trailing defaults.
    fn_defaults: HashMap<String, Vec<Option<z::Expr>>>,
    /// Active type-parameter bindings (a stack, for nested instantiation).
    type_env: RefCell<Vec<HashMap<String, z::Type>>>,
    /// Queue of generic function instantiations still to emit: (name, type args).
    pending: RefCell<Vec<(String, Vec<z::Type>)>>,
    /// Queue of generic class instantiations to flesh out: (name, type args, the
    /// already-allocated struct id whose fields/methods still need emitting).
    pending_classes: RefCell<Vec<(String, Vec<z::Type>, u32)>>,
    /// Mangled instantiation names already emitted or queued (dedup).
    done_insts: RefCell<HashSet<String>>,
    /// Enum: name → its members (numeric → i64, or string → str).
    enums: HashMap<String, EnumKind>,
    /// Counter for fresh temporary names (e.g. `for…of` desugaring).
    tmp: Cell<u32>,
}

/// Owned metadata to monomorphize a generic function without re-touching the AST.
struct GenericInfo {
    type_params: Vec<String>,
    /// Per type param: an argument index to infer it from (a parameter whose type
    /// is exactly `T`), or `None` if it must be supplied explicitly (`f<i64>(…)`).
    infer_from: Vec<Option<usize>>,
    /// The return type as a template, for the instantiation's `fn_rets` entry.
    ret: Option<TypeTpl>,
}

/// Owned metadata to monomorphize a generic class (`class C<T> { … }`).
struct ClassGenericInfo {
    type_params: Vec<String>,
    /// Per type param: a constructor-argument index to infer it from (a ctor
    /// param typed exactly `T`), or `None` (then `new C<…>()` is required).
    ctor_infer: Vec<Option<usize>>,
    /// Each method's `(name, return-type template)` — for the instantiation's
    /// `fn_rets` entries.
    methods: Vec<(String, Option<TypeTpl>)>,
}

/// A generic return/field type expressed against the type parameters.
enum TypeTpl {
    Concrete(z::Type),
    Param(usize),
}

/// An enum's members: numeric (i64-backed) or string (str-backed).
enum EnumKind {
    Int(HashMap<String, i64>),
    Str(HashMap<String, String>),
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
        // Pass 0: enums — numeric (auto-incrementing i64) or string (every member
        // a string literal). Registered so a type reference (`Color`) and
        // `Color.Member` resolve everywhere.
        for stmt in &program.body {
            if let Statement::TSEnumDeclaration(decl) = stmt {
                let name = decl.id.name.as_str().to_string();
                if self.enums.contains_key(&name) {
                    return Err(self.err(decl.span, format!("enum '{name}' redefined")));
                }
                // a string enum if any member is given a string literal
                let is_str = decl
                    .body
                    .members
                    .iter()
                    .any(|m| matches!(&m.initializer, Some(Expression::StringLiteral(_))));
                let kind = if is_str {
                    let mut members = HashMap::new();
                    for m in &decl.body.members {
                        let mname = self.enum_member_name(&m.id, m.span)?;
                        let val = match &m.initializer {
                            Some(Expression::StringLiteral(s)) => s.value.as_str().to_string(),
                            _ => {
                                return Err(self.err(
                                    m.span,
                                    "every member of a string enum needs a string value",
                                ))
                            }
                        };
                        members.insert(mname, val);
                    }
                    EnumKind::Str(members)
                } else {
                    let mut members = HashMap::new();
                    let mut next = 0i64;
                    for m in &decl.body.members {
                        let mname = self.enum_member_name(&m.id, m.span)?;
                        let val = match &m.initializer {
                            Some(e) => self.enum_value(e)?,
                            None => next,
                        };
                        members.insert(mname, val);
                        next = val + 1;
                    }
                    EnumKind::Int(members)
                };
                self.enums.insert(name, kind);
            }
        }

        // Pass 1a: register names. Non-generic interfaces/classes get a struct id
        // now (a placeholder filled in pass 1b); a generic class instead gets
        // owned metadata + a borrow of its AST to instantiate from. Source order,
        // so forward references resolve.
        let mut generic_classes: HashMap<String, &Class> = HashMap::new();
        for stmt in &program.body {
            match stmt {
                Statement::TSInterfaceDeclaration(i) => {
                    let name = i.id.name.as_str();
                    self.check_type_unique(name, i.span)?;
                    self.add_struct(z::StructDecl { name: name.to_string(), fields: Vec::new() });
                }
                Statement::ClassDeclaration(c) => {
                    let name = c
                        .id
                        .as_ref()
                        .ok_or_else(|| self.err(c.span, "classes must be named"))?
                        .name
                        .as_str();
                    self.check_type_unique(name, c.span)?;
                    if let Some(decl) = &c.type_parameters {
                        let info = self.class_generic_info_of(c, decl)?;
                        self.generic_class_info.insert(name.to_string(), info);
                        generic_classes.insert(name.to_string(), c);
                    } else {
                        self.add_struct(z::StructDecl {
                            name: name.to_string(),
                            fields: Vec::new(),
                        });
                    }
                }
                _ => {}
            }
        }

        // Pass 1b: fill in the non-generic struct bodies (placeholders from 1a).
        for stmt in &program.body {
            match stmt {
                Statement::TSInterfaceDeclaration(i) => {
                    let id = self.struct_id(i.id.name.as_str()).unwrap();
                    let decl = self.interface(i)?;
                    self.struct_decls.borrow_mut()[id as usize] = decl;
                }
                Statement::ClassDeclaration(c) if c.type_parameters.is_none() => {
                    let id = self.struct_id(c.id.as_ref().unwrap().name.as_str()).unwrap();
                    let decl = self.class_struct(c)?;
                    self.struct_decls.borrow_mut()[id as usize] = decl;
                }
                _ => {}
            }
        }

        // Pass 1c: collect the return type of every NON-generic callable up front
        // (generic instantiations register theirs at the call site). Methods can
        // then resolve the type of a call result for dispatch.
        for stmt in &program.body {
            match stmt {
                Statement::FunctionDeclaration(f) => {
                    if f.type_parameters.is_some() {
                        continue; // generic template — registered in pass 1d
                    }
                    if let Some(id) = &f.id {
                        let r = self.fn_ret_type(f)?;
                        self.fn_rets.borrow_mut().insert(id.name.as_str().to_string(), r);
                        let defs = self.lower_defaults(&f.params)?;
                        self.fn_defaults.insert(id.name.as_str().to_string(), defs);
                    }
                }
                Statement::ClassDeclaration(c) => {
                    if c.type_parameters.is_some() {
                        continue; // generic — registered per instantiation
                    }
                    let cname = c.id.as_ref().unwrap().name.as_str().to_string();
                    let cid = self.struct_id(&cname).unwrap();
                    self.fn_rets.borrow_mut().insert(format!("{cname}__new"), z::Type::Struct(cid));
                    // constructor defaults (factory `C__new` takes the ctor params)
                    if let Some(ctor) = c.body.body.iter().find_map(|el| match el {
                        ClassElement::MethodDefinition(m)
                            if matches!(m.kind, MethodDefinitionKind::Constructor) =>
                        {
                            Some(m)
                        }
                        _ => None,
                    }) {
                        let defs = self.lower_defaults(&ctor.value.params)?;
                        self.fn_defaults.insert(format!("{cname}__new"), defs);
                    }
                    for el in &c.body.body {
                        if let ClassElement::MethodDefinition(m) = el {
                            if matches!(m.kind, MethodDefinitionKind::Method) {
                                let mname = self.prop_name(&m.key, m.span)?;
                                let r = self.fn_ret_type(&m.value)?;
                                self.fn_rets.borrow_mut().insert(format!("{cname}__{mname}"), r);
                                // `this` is param 0 (always provided), then the rest
                                let mut defs = vec![None];
                                defs.extend(self.lower_defaults(&m.value.params)?);
                                self.fn_defaults.insert(format!("{cname}__{mname}"), defs);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Pass 1d: register generic function templates — owned metadata for
        // monomorphization, plus a borrow of the AST to instantiate from.
        let mut generics: HashMap<String, &Function> = HashMap::new();
        for stmt in &program.body {
            if let Statement::FunctionDeclaration(f) = stmt {
                let Some(decl) = &f.type_parameters else { continue };
                let name = f
                    .id
                    .as_ref()
                    .ok_or_else(|| self.err(f.span, "functions must be named"))?
                    .name
                    .as_str()
                    .to_string();
                if self.generic_info.contains_key(&name)
                    || self.fn_rets.borrow().contains_key(&name)
                {
                    return Err(self.err(f.span, format!("function '{name}' redefined")));
                }
                let info = self.generic_info_of(f, decl)?;
                self.generic_info.insert(name.clone(), info);
                generics.insert(name, f);
            }
        }

        // Pass 2: lower all bodies — non-generic functions, then class factories
        // and methods. Generic templates are lowered on demand (pass 3).
        let mut funcs = Vec::new();
        for stmt in &program.body {
            match stmt {
                Statement::FunctionDeclaration(f) => {
                    if f.type_parameters.is_none() {
                        funcs.push(self.func(f)?);
                    }
                }
                Statement::ClassDeclaration(c) => {
                    if c.type_parameters.is_none() {
                        self.lower_class(c, &mut funcs)?;
                    }
                }
                Statement::TSInterfaceDeclaration(_)
                | Statement::TSTypeAliasDeclaration(_)
                | Statement::TSEnumDeclaration(_)
                | Statement::EmptyStatement(_) => {}
                other => {
                    return Err(self.err(
                        span_of_stmt(other),
                        "only top-level functions, classes, interfaces, and enums are supported (v0)",
                    ))
                }
            }
        }

        // Pass 3: monomorphize. Instantiating a generic (function or class) may
        // request more, so drain both queues until empty.
        loop {
            if let Some((gname, type_args)) = self.pending.borrow_mut().pop() {
                let mangled = mangle_generic(&gname, &type_args);
                let f = generics[&gname];
                let type_params = self.generic_info[&gname].type_params.clone();
                self.push_type_env(&type_params, &type_args);
                let lowered = self.lower_func(f, mangled);
                self.type_env.borrow_mut().pop();
                funcs.push(lowered?);
                continue;
            }
            if let Some((cname, type_args, cid)) = self.pending_classes.borrow_mut().pop() {
                let c = generic_classes[&cname];
                let type_params = self.generic_class_info[&cname].type_params.clone();
                self.push_type_env(&type_params, &type_args);
                let r = self.instantiate_class_body(c, cid, &mut funcs);
                self.type_env.borrow_mut().pop();
                r?;
                continue;
            }
            break;
        }
        Ok(z::Module { funcs, structs: self.struct_decls.take() })
    }

    /// Precompute the owned monomorphization metadata for a generic function.
    fn generic_info_of(
        &self,
        f: &Function,
        decl: &TSTypeParameterDeclaration,
    ) -> LResult<GenericInfo> {
        let type_params: Vec<String> =
            decl.params.iter().map(|p| p.name.name.as_str().to_string()).collect();
        if type_params.is_empty() {
            return Err(self.err(f.span, "generic function has no type parameters"));
        }
        // A type param is inferable if some parameter's type is exactly `T`.
        let mut infer_from = vec![None; type_params.len()];
        for (pi, p) in f.params.items.iter().enumerate() {
            if let Some(ann) = &p.type_annotation {
                if let Some(tn) = bare_type_ref_name(&ann.type_annotation) {
                    if let Some(k) = type_params.iter().position(|t| t == tn) {
                        if infer_from[k].is_none() {
                            infer_from[k] = Some(pi);
                        }
                    }
                }
            }
        }
        let ret = match &f.return_type {
            Some(ann) => self.ret_tpl(&ann.type_annotation, &type_params),
            None => return Err(self.err(f.span, "functions need an explicit return type")),
        };
        Ok(GenericInfo { type_params, infer_from, ret })
    }

    /// A generic's return type as a template: a bare type param, else (if it has
    /// no type params) a concrete type, else `None` (instantiation result type
    /// stays unknown — fine, it just can't be a method receiver without help).
    fn ret_tpl(&self, t: &TSType, type_params: &[String]) -> Option<TypeTpl> {
        if let Some(tn) = bare_type_ref_name(t) {
            if let Some(k) = type_params.iter().position(|x| x == tn) {
                return Some(TypeTpl::Param(k));
            }
        }
        self.ty(t).ok().map(TypeTpl::Concrete)
    }

    /// An enum member's name (`Identifier` or string-literal key).
    fn enum_member_name(&self, id: &TSEnumMemberName, span: Span) -> LResult<String> {
        match id {
            TSEnumMemberName::Identifier(i) => Ok(i.name.as_str().to_string()),
            TSEnumMemberName::String(s) => Ok(s.value.as_str().to_string()),
            _ => Err(self.err(span, "computed enum member names aren't supported")),
        }
    }

    /// Evaluate a constant integer enum initializer (`= 5`, `= -1`).
    fn enum_value(&self, e: &Expression) -> LResult<i64> {
        match e {
            Expression::NumericLiteral(n) => {
                if n.value.fract() != 0.0 {
                    return Err(self.err(n.span, "enum value must be an integer"));
                }
                Ok(n.value as i64)
            }
            Expression::UnaryExpression(u)
                if matches!(u.operator, UnaryOperator::UnaryNegation) =>
            {
                Ok(-self.enum_value(&u.argument)?)
            }
            Expression::ParenthesizedExpression(p) => self.enum_value(&p.expression),
            other => Err(self.err(
                span_of_expr(other),
                "enum initializer must be an integer literal (string/computed enums aren't supported)",
            )),
        }
    }

    /// A fresh, collision-free temporary name (for desugaring).
    fn fresh(&self, tag: &str) -> String {
        let n = self.tmp.get();
        self.tmp.set(n + 1);
        format!("__z{tag}{n}")
    }

    /// Error if a type name is already taken (struct/interface/enum/generic class).
    fn check_type_unique(&self, name: &str, span: Span) -> LResult<()> {
        if self.struct_id(name).is_some()
            || self.generic_class_info.contains_key(name)
            || self.enums.contains_key(name)
        {
            return Err(self.err(span, format!("type '{name}' redefined")));
        }
        Ok(())
    }

    /// Push a fresh type-parameter → concrete-type scope.
    fn push_type_env(&self, params: &[String], args: &[z::Type]) {
        let mut env = HashMap::new();
        for (k, tp) in params.iter().enumerate() {
            if let Some(t) = args.get(k) {
                env.insert(tp.clone(), *t);
            }
        }
        self.type_env.borrow_mut().push(env);
    }

    /// Precompute owned monomorphization metadata for a generic class.
    fn class_generic_info_of(
        &self,
        c: &Class,
        decl: &TSTypeParameterDeclaration,
    ) -> LResult<ClassGenericInfo> {
        let type_params: Vec<String> =
            decl.params.iter().map(|p| p.name.name.as_str().to_string()).collect();
        if type_params.is_empty() {
            return Err(self.err(c.span, "generic class has no type parameters"));
        }
        // constructor-argument inference: a ctor param typed exactly `T`
        let mut ctor_infer = vec![None; type_params.len()];
        let ctor = c.body.body.iter().find_map(|el| match el {
            ClassElement::MethodDefinition(m)
                if matches!(m.kind, MethodDefinitionKind::Constructor) =>
            {
                Some(m)
            }
            _ => None,
        });
        if let Some(ctor) = ctor {
            for (pi, p) in ctor.value.params.items.iter().enumerate() {
                if let Some(ann) = &p.type_annotation {
                    if let Some(tn) = bare_type_ref_name(&ann.type_annotation) {
                        if let Some(k) = type_params.iter().position(|t| t == tn) {
                            if ctor_infer[k].is_none() {
                                ctor_infer[k] = Some(pi);
                            }
                        }
                    }
                }
            }
        }
        // each method's return-type template
        let mut methods = Vec::new();
        for el in &c.body.body {
            if let ClassElement::MethodDefinition(m) = el {
                if matches!(m.kind, MethodDefinitionKind::Method) {
                    let mname = self.prop_name(&m.key, m.span)?;
                    let ret = match &m.value.return_type {
                        Some(ann) => self.ret_tpl(&ann.type_annotation, &type_params),
                        None => None,
                    };
                    methods.push((mname, ret));
                }
            }
        }
        Ok(ClassGenericInfo { type_params, ctor_infer, methods })
    }

    /// Ensure a generic class is instantiated for `type_args`, returning the
    /// struct id. Allocates the struct immediately (fields filled in pass 3) and
    /// registers the factory/method return types for call-result typing.
    fn instantiate_generic_class(
        &self,
        name: &str,
        type_args: Vec<z::Type>,
        span: Span,
    ) -> LResult<u32> {
        let nparams = self.generic_class_info[name].type_params.len();
        if type_args.len() != nparams {
            return Err(self.err(
                span,
                format!("{name} expects {nparams} type argument(s), got {}", type_args.len()),
            ));
        }
        let mangled = mangle_generic(name, &type_args);
        if let Some(id) = self.struct_id(&mangled) {
            return Ok(id); // already instantiated
        }
        let id = self.add_struct(z::StructDecl { name: mangled.clone(), fields: Vec::new() });
        self.fn_rets.borrow_mut().insert(format!("{mangled}__new"), z::Type::Struct(id));
        for k in 0..self.generic_class_info[name].methods.len() {
            let (mname, ret) = &self.generic_class_info[name].methods[k];
            let rt = match ret {
                Some(TypeTpl::Concrete(t)) => Some(*t),
                Some(TypeTpl::Param(p)) => type_args.get(*p).copied(),
                None => None,
            };
            if let Some(rt) = rt {
                self.fn_rets.borrow_mut().insert(format!("{mangled}__{mname}"), rt);
            }
        }
        self.pending_classes.borrow_mut().push((name.to_string(), type_args, id));
        Ok(id)
    }

    /// Fill an instantiated class's struct fields and emit its factory/methods.
    fn instantiate_class_body(
        &self,
        c: &Class,
        cid: u32,
        funcs: &mut Vec<z::Func>,
    ) -> LResult<()> {
        let fields = self.class_fields(c)?;
        self.struct_decls.borrow_mut()[cid as usize].fields = fields;
        self.emit_class_fns(c, cid, funcs)
    }

    /// Resolve a `new C(…)` call's type arguments — explicit `new C<A>(…)`, else
    /// inferred from constructor arguments.
    fn class_type_args(
        &self,
        n: &NewExpression,
        name: &str,
        args: &[z::Expr],
    ) -> LResult<Vec<z::Type>> {
        let nparams = self.generic_class_info[name].type_params.len();
        if let Some(ta) = &n.type_arguments {
            if ta.params.len() != nparams {
                return Err(self.err(
                    n.span,
                    format!("{name} expects {nparams} type argument(s), got {}", ta.params.len()),
                ));
            }
            return ta.params.iter().map(|p| self.ty(p)).collect();
        }
        let mut out = Vec::with_capacity(nparams);
        for k in 0..nparams {
            let tp = &self.generic_class_info[name].type_params[k];
            let idx = self.generic_class_info[name].ctor_infer[k].ok_or_else(|| {
                self.err(n.span, format!("can't infer '{tp}' for {name}; write `new {name}<…>(…)`"))
            })?;
            let t = args.get(idx).and_then(|a| self.ztype_of(a)).ok_or_else(|| {
                self.err(n.span, format!("can't infer '{tp}' for {name}; write `new {name}<…>(…)`"))
            })?;
            out.push(t);
        }
        Ok(out)
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
            .struct_name(id)
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

    /// Lower a class's fields (under the active type env) to `(name, type)` pairs.
    fn class_fields(&self, c: &Class) -> LResult<Vec<(String, z::Type)>> {
        if c.super_class.is_some() {
            return Err(self.err(c.span, "class inheritance (`extends`) isn't supported"));
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
                ClassElement::MethodDefinition(_) => {} // lowered separately
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
        Ok(fields)
    }

    /// Lower a (non-generic) class's fields to a `StructDecl`.
    fn class_struct(&self, c: &Class) -> LResult<z::StructDecl> {
        let name = c.id.as_ref().unwrap().name.as_str().to_string();
        Ok(z::StructDecl { name, fields: self.class_fields(c)? })
    }

    /// Emit a class's factory + method functions into `funcs`, named off `cid`'s
    /// struct name (which is mangled for a generic instantiation).
    fn emit_class_fns(&self, c: &Class, cid: u32, funcs: &mut Vec<z::Func>) -> LResult<()> {
        let cname = self.struct_name(cid).unwrap();
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

    /// Emit a non-generic class's factory + methods (its struct already built).
    fn lower_class(&self, c: &Class, funcs: &mut Vec<z::Func>) -> LResult<()> {
        let cid = self.struct_id(c.id.as_ref().unwrap().name.as_str()).unwrap();
        self.emit_class_fns(c, cid, funcs)
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
            z::Type::Struct(id) | z::Type::OptStruct(id) => Some(id),
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

    /// A zero/empty default value for a class field type (the constructor usually
    /// overwrites it). Scalars → 0/false/"", arrays → empty; structs have no
    /// default (must be given an initializer).
    fn type_default(&self, ty: z::Type) -> Option<z::Expr> {
        Some(match ty {
            z::Type::I64 | z::Type::I32 | z::Type::U32 | z::Type::U64 => z::Expr::Int(0),
            z::Type::F64 => z::Expr::Float(0.0),
            z::Type::Bool => z::Expr::Bool(false),
            z::Type::Str => z::Expr::Str(String::new()),
            // an empty array `[elem; 0]` — the constructor typically reassigns it
            z::Type::Array(e) => {
                let elem = match e {
                    z::Elem::I64 => z::Expr::Int(0),
                    z::Elem::F64 => z::Expr::Float(0.0),
                    z::Elem::Bool => z::Expr::Bool(false),
                };
                z::Expr::Repeat { value: Box::new(elem), count: Box::new(z::Expr::Int(0)) }
            }
            // a nullable field defaults to `null`
            z::Type::OptStruct(_) => z::Expr::Null,
            z::Type::Struct(_) | z::Type::Null => return None,
        })
    }

    /// The declared name of a struct id.
    fn struct_name(&self, id: u32) -> Option<String> {
        self.struct_decls.borrow().get(id as usize).map(|d| d.name.clone())
    }

    /// Look up a struct id by name.
    fn struct_id(&self, name: &str) -> Option<u32> {
        self.structs.borrow().get(name).copied()
    }

    /// A struct field's type (for `obj.field` inference).
    fn struct_field_type(&self, id: u32, field: &str) -> Option<z::Type> {
        self.struct_decls
            .borrow()
            .get(id as usize)?
            .fields
            .iter()
            .find(|(n, _)| n == field)
            .map(|(_, t)| *t)
    }

    /// Register a new struct, returning its id.
    fn add_struct(&self, decl: z::StructDecl) -> u32 {
        let mut decls = self.struct_decls.borrow_mut();
        let id = decls.len() as u32;
        self.structs.borrow_mut().insert(decl.name.clone(), id);
        decls.push(decl);
        id
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
            z::Expr::Call { name, .. } => self.fn_rets.borrow().get(name).copied()?,
            z::Expr::StructLit { name, .. } => z::Type::Struct(self.struct_id(name)?),
            z::Expr::Field { base, field } => {
                let z::Type::Struct(id) = self.ztype_of(base)? else { return None };
                self.struct_field_type(id, field)?
            }
            z::Expr::Cond { then, .. } => self.ztype_of(then)?,
            z::Expr::Coalesce { rhs, .. } => self.ztype_of(rhs)?,
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
        self.lower_func(f, name)
    }

    /// Lower a function (or one generic instantiation) under whatever type
    /// environment is currently active, emitting it under `name`.
    fn lower_func(&self, f: &Function, name: String) -> LResult<z::Func> {
        self.push_scope();
        let mut params = Vec::new();
        for p in &f.params.items {
            let (pname, pty) = self.param(p)?;
            self.bind(&pname, pty);
            params.push(z::Param { name: pname, ty: pty });
        }
        let ret = self.fn_ret_type(f)?;
        self.ret_struct.set(match ret {
            z::Type::Struct(id) | z::Type::OptStruct(id) => Some(id),
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

    /// Lower each parameter's default value (`= …`). Defaults must be constant
    /// (they're evaluated at the call site), so a variable reference is rejected.
    fn lower_defaults(&self, params: &FormalParameters) -> LResult<Vec<Option<z::Expr>>> {
        let mut out = Vec::with_capacity(params.items.len());
        for p in &params.items {
            let d = match &p.initializer {
                Some(e) => {
                    let lowered = self.expr(e)?;
                    if expr_has_var(&lowered) {
                        return Err(self.err(
                            p.span,
                            "a default parameter value must be constant (it can't reference \
                             variables or other parameters)",
                        ));
                    }
                    Some(lowered)
                }
                None => None,
            };
            out.push(d);
        }
        Ok(out)
    }

    /// Append the missing trailing default arguments for a call to `name`.
    fn apply_defaults(&self, name: &str, args: &mut Vec<z::Expr>) {
        if let Some(defs) = self.fn_defaults.get(name) {
            while args.len() < defs.len() {
                match &defs[args.len()] {
                    Some(d) => args.push(d.clone()),
                    None => break, // a non-defaulted gap; the checker reports the arity error
                }
            }
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
                    other => {
                        // A generic type parameter currently in scope?
                        if let Some(t) =
                            self.type_env.borrow().iter().rev().find_map(|m| m.get(other).copied())
                        {
                            t
                        } else if let Some(k) = self.enums.get(other) {
                            match k {
                                EnumKind::Int(_) => z::Type::I64,
                                EnumKind::Str(_) => z::Type::Str,
                            }
                        } else if self.generic_class_info.contains_key(other) {
                            // `Box<i64>` → instantiate the generic class
                            let args = match &r.type_arguments {
                                Some(ta) => ta
                                    .params
                                    .iter()
                                    .map(|p| self.ty(p))
                                    .collect::<LResult<Vec<_>>>()?,
                                None => {
                                    return Err(self.err(
                                        r.span,
                                        format!("'{other}' is generic — write `{other}<…>`"),
                                    ))
                                }
                            };
                            z::Type::Struct(self.instantiate_generic_class(other, args, r.span)?)
                        } else if let Some(id) = self.struct_id(other) {
                            z::Type::Struct(id)
                        } else {
                            return Err(self.err(r.span, format!("unknown type '{other}'")));
                        }
                    }
                }
            }
            TSType::TSParenthesizedType(p) => self.ty(&p.type_annotation)?,
            // `T | null` / `T | undefined` → a nullable struct reference.
            TSType::TSUnionType(u) => {
                let non_null: Vec<&TSType> =
                    u.types.iter().filter(|t| !is_null_ts_type(t)).collect();
                if u.types.iter().any(is_null_ts_type) && non_null.len() == 1 {
                    match self.ty(non_null[0])? {
                        z::Type::Struct(id) => z::Type::OptStruct(id),
                        other => {
                            return Err(self.err(
                                u.span,
                                format!("only a struct type can be `… | null` in v0, not {other:?}"),
                            ))
                        }
                    }
                } else {
                    return Err(self.err(u.span, "only `T | null` unions are supported"));
                }
            }
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
            // `for (const x of arr) { … }` → an index loop over a hoisted array.
            Statement::ForOfStatement(fo) => {
                if fo.r#await {
                    return Err(self.err(fo.span, "`for await` isn't supported"));
                }
                let var = match &fo.left {
                    ForStatementLeft::VariableDeclaration(vd) => {
                        if vd.declarations.len() != 1 {
                            return Err(self.err(fo.span, "for…of needs exactly one binding"));
                        }
                        match &vd.declarations[0].id {
                            BindingPattern::BindingIdentifier(bi) => bi.name.as_str().to_string(),
                            _ => return Err(self.err(fo.span, "for…of binding must be a plain name")),
                        }
                    }
                    _ => return Err(self.err(fo.span, "for…of must bind with `let`/`const`")),
                };
                let arr = self.expr(&fo.right)?;
                let arr_tmp = self.fresh("arr");
                let idx_tmp = self.fresh("i");
                let line = self.line(fo.span);
                let var_eq = |t: &str| z::Expr::Var(t.to_string());
                // hoist the iterable once: `let __arr = <iterable>;`
                push(out, z::StmtKind::Let { name: arr_tmp.clone(), ty: None, value: arr }, fo.span);
                // loop body: `let x = __arr[__i]; <user body>`
                let mut body = vec![z::Stmt {
                    kind: z::StmtKind::Let {
                        name: var,
                        ty: None,
                        value: z::Expr::Index {
                            arr: Box::new(var_eq(&arr_tmp)),
                            index: Box::new(var_eq(&idx_tmp)),
                        },
                    },
                    line,
                }];
                body.extend(self.stmt_as_block(&fo.body)?);
                // `for (let __i = 0; __i < len(__arr); __i = __i + 1) { … }`
                let init = z::Stmt {
                    kind: z::StmtKind::Let {
                        name: idx_tmp.clone(),
                        ty: Some(z::Type::I64),
                        value: z::Expr::Int(0),
                    },
                    line,
                };
                let cond = z::Expr::Bin {
                    op: z::BinOp::Lt,
                    l: Box::new(var_eq(&idx_tmp)),
                    r: Box::new(z::Expr::Call { name: "len".into(), args: vec![var_eq(&arr_tmp)] }),
                };
                let step = z::Stmt {
                    kind: z::StmtKind::Assign {
                        target: var_eq(&idx_tmp),
                        value: z::Expr::Bin {
                            op: z::BinOp::Add,
                            l: Box::new(var_eq(&idx_tmp)),
                            r: Box::new(z::Expr::Int(1)),
                        },
                    },
                    line,
                };
                push(
                    out,
                    z::StmtKind::For {
                        init: Some(Box::new(init)),
                        cond,
                        step: Some(Box::new(step)),
                        body,
                    },
                    fo.span,
                );
            }
            // `switch (d) { case v: …; default: … }` → an if/else-if chain on a
            // hoisted discriminant. Sound subset: no fall-through (each non-empty
            // case ends with break/return/continue); empty cases stack onto the
            // next; a `break` only as the case terminator.
            Statement::SwitchStatement(sw) => {
                let disc = self.expr(&sw.discriminant)?;
                let sw_tmp = self.fresh("sw");
                push(out, z::StmtKind::Let { name: sw_tmp.clone(), ty: None, value: disc }, sw.span);
                let mut groups: Vec<(Vec<z::Expr>, Vec<z::Stmt>)> = Vec::new();
                let mut default_body: Vec<z::Stmt> = Vec::new();
                let mut pending: Vec<z::Expr> = Vec::new();
                for case in &sw.cases {
                    match &case.test {
                        None => {
                            default_body = self.switch_case_body(&case.consequent)?;
                            pending.clear(); // empty cases before default fall to it
                        }
                        Some(test) => {
                            let t = self.expr(test)?;
                            if case.consequent.is_empty() {
                                pending.push(t); // stacks onto the next non-empty case
                            } else {
                                let mut grp = std::mem::take(&mut pending);
                                grp.push(t);
                                groups.push((grp, self.switch_case_body(&case.consequent)?));
                            }
                        }
                    }
                }
                // fold into a right-nested chain (default = the innermost else)
                let line = self.line(sw.span);
                let mut else_b = default_body;
                for (grp, body) in groups.into_iter().rev() {
                    let cond = grp
                        .into_iter()
                        .map(|t| z::Expr::Bin {
                            op: z::BinOp::Eq,
                            l: Box::new(z::Expr::Var(sw_tmp.clone())),
                            r: Box::new(t),
                        })
                        .reduce(|a, b| z::Expr::Bin {
                            op: z::BinOp::Or,
                            l: Box::new(a),
                            r: Box::new(b),
                        })
                        .unwrap();
                    else_b = vec![z::Stmt {
                        kind: z::StmtKind::If { cond, then_b: body, else_b },
                        line,
                    }];
                }
                out.extend(else_b);
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
            let init = d
                .init
                .as_ref()
                .ok_or_else(|| self.err(d.span, "a `let`/`const` must be initialized"))?;
            match &d.id {
                BindingPattern::BindingIdentifier(bi) => {
                    let name = bi.name.as_str().to_string();
                    let ty = match &d.type_annotation {
                        Some(a) => Some(self.ty(&a.type_annotation)?),
                        None => None,
                    };
                    // `let p: Point = { ... }` — the annotation names the struct.
                    let value = match (&ty, init) {
                        (
                            Some(z::Type::Struct(id) | z::Type::OptStruct(id)),
                            Expression::ObjectExpression(obj),
                        ) => self.obj_struct_lit(obj, *id)?,
                        _ => self.expr(init)?,
                    };
                    // Track the variable's type (annotation, else inferred).
                    if let Some(t) = ty.or_else(|| self.ztype_of(&value)) {
                        self.bind(&name, t);
                    }
                    out.push(z::Stmt {
                        kind: z::StmtKind::Let { name, ty, value },
                        line: self.line(d.span),
                    });
                }
                BindingPattern::ArrayPattern(arr) => self.destructure_array(arr, init, out)?,
                BindingPattern::ObjectPattern(obj) => self.destructure_object(obj, init, out)?,
                _ => return Err(self.err(d.span, "this binding pattern isn't supported")),
            }
        }
        Ok(())
    }

    /// `const [a, b] = arr` → hoist `arr` once, then `let a = __d[0]; let b = __d[1]`.
    fn destructure_array(
        &self,
        arr: &ArrayPattern,
        init: &Expression,
        out: &mut Vec<z::Stmt>,
    ) -> LResult<()> {
        if arr.rest.is_some() {
            return Err(self.err(arr.span, "a rest element `...` in array destructuring isn't supported"));
        }
        let value = self.expr(init)?;
        let tmp = self.fresh("d");
        let line = self.line(arr.span);
        out.push(z::Stmt { kind: z::StmtKind::Let { name: tmp.clone(), ty: None, value }, line });
        for (i, el) in arr.elements.iter().enumerate() {
            let Some(pat) = el else { continue }; // hole, e.g. `[, b]`
            let name = match pat {
                BindingPattern::BindingIdentifier(bi) => bi.name.as_str().to_string(),
                _ => return Err(self.err(arr.span, "nested destructuring isn't supported")),
            };
            out.push(z::Stmt {
                kind: z::StmtKind::Let {
                    name,
                    ty: None,
                    value: z::Expr::Index {
                        arr: Box::new(z::Expr::Var(tmp.clone())),
                        index: Box::new(z::Expr::Int(i as i64)),
                    },
                },
                line,
            });
        }
        Ok(())
    }

    /// `const {x, y: z} = s` → hoist `s` once, then `let x = __d.x; let z = __d.y`.
    fn destructure_object(
        &self,
        obj: &ObjectPattern,
        init: &Expression,
        out: &mut Vec<z::Stmt>,
    ) -> LResult<()> {
        if obj.rest.is_some() {
            return Err(self.err(obj.span, "a rest element `...` in object destructuring isn't supported"));
        }
        let value = self.expr(init)?;
        let tmp = self.fresh("d");
        let line = self.line(obj.span);
        let tmp_ty = self.ztype_of(&value);
        out.push(z::Stmt { kind: z::StmtKind::Let { name: tmp.clone(), ty: None, value }, line });
        if let Some(t) = tmp_ty {
            self.bind(&tmp, t); // so `__d.field` resolves below
        }
        for p in &obj.properties {
            if p.computed {
                return Err(self.err(p.span, "computed keys in destructuring aren't supported"));
            }
            let field = self.prop_name(&p.key, p.span)?;
            let name = match &p.value {
                BindingPattern::BindingIdentifier(bi) => bi.name.as_str().to_string(),
                _ => return Err(self.err(p.span, "nested destructuring isn't supported")),
            };
            let value = z::Expr::Field { base: Box::new(z::Expr::Var(tmp.clone())), field };
            if let Some(t) = self.ztype_of(&value) {
                self.bind(&name, t);
            }
            out.push(z::Stmt { kind: z::StmtKind::Let { name, ty: None, value }, line });
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

    /// Lower one switch case's body: strip a trailing `break` (the terminator),
    /// reject fall-through and a `break` used anywhere but as the terminator.
    fn switch_case_body(&self, stmts: &[Statement]) -> LResult<Vec<z::Stmt>> {
        let trailing_break =
            matches!(stmts.last(), Some(Statement::BreakStatement(b)) if b.label.is_none());
        let body = if trailing_break { &stmts[..stmts.len() - 1] } else { stmts };
        if !trailing_break && !stmts.is_empty() && !stmt_diverges(stmts.last().unwrap()) {
            // otherwise the case would fall through to the next
            return Err(self.err(
                span_of_stmt(stmts.last().unwrap()),
                "switch case must end with `break`, `return`, or `continue` \
                 (fall-through isn't supported)",
            ));
        }
        for s in body {
            if stmt_has_switch_break(s) {
                return Err(self.err(
                    span_of_stmt(s),
                    "a `break` inside a switch case (other than the final one) isn't supported",
                ));
            }
        }
        let mut out = Vec::new();
        for s in body {
            self.stmt(s, &mut out)?;
        }
        Ok(out)
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
            Expression::NullLiteral(_) => z::Expr::Null,
            Expression::Identifier(id) => {
                // `undefined` is conflated with `null` in this subset.
                if id.name.as_str() == "undefined" {
                    z::Expr::Null
                } else {
                    z::Expr::Var(id.name.as_str().to_string())
                }
            }
            Expression::ParenthesizedExpression(p) => self.expr(&p.expression)?,
            Expression::BinaryExpression(b) => z::Expr::Bin {
                op: self.bin_op(b.operator, b.span)?,
                l: Box::new(self.expr(&b.left)?),
                r: Box::new(self.expr(&b.right)?),
            },
            Expression::LogicalExpression(l) => {
                let lhs = Box::new(self.expr(&l.left)?);
                let rhs = Box::new(self.expr(&l.right)?);
                match l.operator {
                    LogicalOperator::And => z::Expr::Bin { op: z::BinOp::And, l: lhs, r: rhs },
                    LogicalOperator::Or => z::Expr::Bin { op: z::BinOp::Or, l: lhs, r: rhs },
                    // `a ?? b` — nullish coalescing.
                    LogicalOperator::Coalesce => z::Expr::Coalesce { lhs, rhs },
                }
            }
            // `cond ? then : els`
            Expression::ConditionalExpression(c) => z::Expr::Cond {
                cond: Box::new(self.expr(&c.test)?),
                then: Box::new(self.expr(&c.consequent)?),
                els: Box::new(self.expr(&c.alternate)?),
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
                // `Color.Red` → the enum member's integer value.
                if let Expression::Identifier(obj) = &m.object {
                    if let Some(kind) = self.enums.get(obj.name.as_str()) {
                        let member = m.property.name.as_str();
                        let val = match kind {
                            EnumKind::Int(mem) => mem.get(member).map(|&v| z::Expr::Int(v)),
                            EnumKind::Str(mem) => mem.get(member).map(|s| z::Expr::Str(s.clone())),
                        };
                        return val.ok_or_else(|| {
                            self.err(
                                m.span,
                                format!("enum '{}' has no member '{member}'", obj.name.as_str()),
                            )
                        });
                    }
                }
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
                } else if let (
                    z::Type::Struct(id) | z::Type::OptStruct(id),
                    Expression::ObjectExpression(obj),
                ) = (to, &a.expression)
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
                let mut args = Vec::new();
                for a in &n.arguments {
                    let ex = a
                        .as_expression()
                        .ok_or_else(|| self.err(n.span, "spread arguments aren't supported"))?;
                    args.push(self.expr(ex)?);
                }
                // Generic class: resolve type args, instantiate, call its factory.
                let factory = if self.generic_class_info.contains_key(&cname) {
                    let type_args = self.class_type_args(n, &cname, &args)?;
                    let id = self.instantiate_generic_class(&cname, type_args, n.span)?;
                    format!("{}__new", self.struct_name(id).unwrap())
                } else {
                    let f = format!("{cname}__new");
                    if !self.fn_rets.borrow().contains_key(&f) {
                        return Err(
                            self.err(n.span, format!("`new {cname}(…)`: '{cname}' isn't a class")),
                        );
                    }
                    f
                };
                self.apply_defaults(&factory, &mut args);
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
            let mname = format!("{cname}__{method}");
            self.apply_defaults(&mname, &mut args);
            return Ok(z::Expr::Call { name: mname, args });
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
        // Generic call → monomorphize: resolve the type arguments (explicit or
        // inferred), queue the specialization, and call the mangled name.
        if self.generic_info.contains_key(&name) {
            let type_args = self.resolve_type_args(c, &name, &args)?;
            let mangled = mangle_generic(&name, &type_args);
            self.request_inst(&name, type_args, &mangled);
            return Ok(z::Expr::Call { name: mangled, args });
        }
        self.apply_defaults(&name, &mut args);
        Ok(z::Expr::Call { name, args })
    }

    /// Resolve a generic call's type arguments — explicit `f<A,B>(…)` if present,
    /// else inferred from arguments typed exactly `T`.
    fn resolve_type_args(
        &self,
        c: &CallExpression,
        name: &str,
        args: &[z::Expr],
    ) -> LResult<Vec<z::Type>> {
        let nparams = self.generic_info[name].type_params.len();
        if let Some(ta) = &c.type_arguments {
            if ta.params.len() != nparams {
                return Err(self.err(
                    c.span,
                    format!("{name} expects {nparams} type argument(s), got {}", ta.params.len()),
                ));
            }
            return ta.params.iter().map(|p| self.ty(p)).collect();
        }
        let mut out = Vec::with_capacity(nparams);
        for k in 0..nparams {
            let tp = &self.generic_info[name].type_params[k];
            let idx = self.generic_info[name].infer_from[k].ok_or_else(|| {
                self.err(
                    c.span,
                    format!("can't infer type argument '{tp}' for {name}; call it as {name}<…>(…)"),
                )
            })?;
            let t = args.get(idx).and_then(|a| self.ztype_of(a)).ok_or_else(|| {
                self.err(
                    c.span,
                    format!("can't infer type argument '{tp}' for {name}; call it as {name}<…>(…)"),
                )
            })?;
            out.push(t);
        }
        Ok(out)
    }

    /// Queue a generic instantiation (dedup by mangled name) and register its
    /// concrete return type so call results can be typed.
    fn request_inst(&self, name: &str, type_args: Vec<z::Type>, mangled: &str) {
        if let Some(ret) = self.concrete_ret(name, &type_args) {
            self.fn_rets.borrow_mut().insert(mangled.to_string(), ret);
        }
        if self.done_insts.borrow().contains(mangled) {
            return;
        }
        self.done_insts.borrow_mut().insert(mangled.to_string());
        self.pending.borrow_mut().push((name.to_string(), type_args));
    }

    /// A generic instantiation's concrete return type, if known.
    fn concrete_ret(&self, name: &str, type_args: &[z::Type]) -> Option<z::Type> {
        match &self.generic_info[name].ret {
            Some(TypeTpl::Concrete(t)) => Some(*t),
            Some(TypeTpl::Param(k)) => type_args.get(*k).copied(),
            None => None,
        }
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

/// Does this statement always leave the case (return/continue/break, or a block
/// or full if/else whose branches all do)? Used to reject fall-through.
fn stmt_diverges(s: &Statement) -> bool {
    match s {
        Statement::ReturnStatement(_)
        | Statement::ContinueStatement(_)
        | Statement::BreakStatement(_) => true,
        Statement::BlockStatement(b) => b.body.last().is_some_and(stmt_diverges),
        Statement::IfStatement(i) => {
            stmt_diverges(&i.consequent)
                && i.alternate.as_ref().is_some_and(|a| stmt_diverges(a))
        }
        _ => false,
    }
}

/// Does this statement contain a `break` that would target the enclosing switch?
/// (Descends into `if`/blocks but stops at loops/nested switches, which capture
/// their own `break`.)
fn stmt_has_switch_break(s: &Statement) -> bool {
    match s {
        Statement::BreakStatement(_) => true,
        Statement::IfStatement(i) => {
            stmt_has_switch_break(&i.consequent)
                || i.alternate.as_ref().is_some_and(|a| stmt_has_switch_break(a))
        }
        Statement::BlockStatement(b) => b.body.iter().any(stmt_has_switch_break),
        _ => false,
    }
}

/// Does a lowered expression reference any variable? Used to keep default
/// parameter values constant (they're cloned into call sites).
fn expr_has_var(e: &z::Expr) -> bool {
    match e {
        z::Expr::Var(_) => true,
        z::Expr::Cast { e, .. } | z::Expr::Unary { e, .. } => expr_has_var(e),
        z::Expr::Bin { l, r, .. } => expr_has_var(l) || expr_has_var(r),
        z::Expr::Cond { cond, then, els } => {
            expr_has_var(cond) || expr_has_var(then) || expr_has_var(els)
        }
        z::Expr::Call { args, .. } => args.iter().any(expr_has_var),
        z::Expr::Array(es) => es.iter().any(expr_has_var),
        z::Expr::Repeat { value, count } => expr_has_var(value) || expr_has_var(count),
        z::Expr::Index { arr, index } => expr_has_var(arr) || expr_has_var(index),
        z::Expr::Field { base, .. } => expr_has_var(base),
        z::Expr::StructLit { fields, .. } => fields.iter().any(|(_, e)| expr_has_var(e)),
        z::Expr::Coalesce { lhs, rhs } => expr_has_var(lhs) || expr_has_var(rhs),
        z::Expr::Int(_) | z::Expr::Float(_) | z::Expr::Bool(_) | z::Expr::Str(_) | z::Expr::Null => {
            false
        }
    }
}

/// Is this TS type the `null` or `undefined` keyword (a nullable union member)?
fn is_null_ts_type(t: &TSType) -> bool {
    matches!(t, TSType::TSNullKeyword(_) | TSType::TSUndefinedKeyword(_))
}

/// The name of a bare type reference (`T`), used to spot type parameters.
fn bare_type_ref_name<'a>(t: &'a TSType<'a>) -> Option<&'a str> {
    if let TSType::TSTypeReference(r) = t {
        if let TSTypeName::IdentifierReference(id) = &r.type_name {
            return Some(id.name.as_str());
        }
    }
    None
}

/// Mangle a generic instantiation to a unique, valid ZIPP function name.
fn mangle_generic(name: &str, args: &[z::Type]) -> String {
    let mut s = format!("{name}__G");
    for a in args {
        s.push('_');
        s.push_str(&mangle_type(*a));
    }
    s
}

fn mangle_type(t: z::Type) -> String {
    match t {
        z::Type::I64 => "i64".into(),
        z::Type::F64 => "f64".into(),
        z::Type::Bool => "bool".into(),
        z::Type::Str => "str".into(),
        z::Type::I32 => "i32".into(),
        z::Type::U32 => "u32".into(),
        z::Type::U64 => "u64".into(),
        z::Type::Array(e) => format!(
            "arr{}",
            match e {
                z::Elem::I64 => "i64",
                z::Elem::F64 => "f64",
                z::Elem::Bool => "bool",
            }
        ),
        z::Type::Struct(id) => format!("s{id}"),
        z::Type::OptStruct(id) => format!("opts{id}"),
        z::Type::Null => "null".into(),
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
    fn generics_monomorphized() {
        // identity at two types (inferred + explicit), and a 2-type-param generic
        // that itself instantiates another generic with one of its type params.
        let ts = "function id<T>(x: T): T { return x; } \
                  function pick<A, B>(a: A, b: B): A { return id<A>(a); } \
                  function main(): i64 { \
                    let x = id(7); \
                    let y = id<i64>(35); \
                    return pick<i64, bool>(x + y, true); }";
        assert_eq!(run_i64(ts), 42);
        // generic over a struct: the instantiation's result is typed (`c.v` works)
        let ts2 = "interface Box { v: i64; } \
                   function thru<T>(b: T): T { return b; } \
                   function main(): i64 { let b: Box = { v: 9 }; let c = thru<Box>(b); return c.v; }";
        assert_eq!(run_i64(ts2), 9);
        // generic recursion: the type param is threaded through each call
        let ts3 = "function rec<T>(x: T, n: i64): i64 { if (n <= 0) { return 0; } return 1 + rec<T>(x, n - 1); } \
                   function main(): i64 { return rec<bool>(true, 5); }";
        assert_eq!(run_i64(ts3), 5);
    }

    #[test]
    fn generic_classes() {
        // one generic class instantiated at two types (i64 and bool → two distinct
        // structs), explicit construction, method dispatch + field mutation
        let ts = "class Box<T> { \
                    value: T; \
                    constructor(v: T) { this.value = v; } \
                    get(): T { return this.value; } \
                    set(v: T): T { this.value = v; return v; } \
                  } \
                  function main(): i64 { \
                    let a = new Box<i64>(10); \
                    a.set(25); \
                    let b = new Box<bool>(true); \
                    if (b.get()) { return a.get() + 5; } \
                    return a.get(); }";
        assert_eq!(run_i64(ts), 30);
        // two type params + explicit args; a method returning one component
        let ts2 = "class Pair<A, B> { \
                     a: A; b: B; \
                     constructor(a: A, b: B) { this.a = a; this.b = b; } \
                     first(): A { return this.a; } \
                   } \
                   function main(): i64 { let p = new Pair<i64, bool>(42, true); return p.first(); }";
        assert_eq!(run_i64(ts2), 42);
        // a generic class as a typed parameter + constructor-arg inference
        let ts3 = "class Box<T> { v: T; constructor(v: T) { this.v = v; } get(): T { return this.v; } } \
                   function unwrap(b: Box<i64>): i64 { return b.get(); } \
                   function main(): i64 { return unwrap(new Box(99)); }";
        assert_eq!(run_i64(ts3), 99);
    }

    #[test]
    fn optionals() {
        // nullable param + `if (x !== null)` flow narrowing + null widening
        let ts = "interface User { age: i64; } \
                  function ageOr(u: User | null, fallback: i64): i64 { \
                    if (u !== null) { return u.age; } \
                    return fallback; \
                  } \
                  function main(): i64 { \
                    let a: User = { age: 30 }; \
                    let some: User | null = a; \
                    let none: User | null = null; \
                    return ageOr(some, -1) + ageOr(none, -1); }";
        assert_eq!(run_i64(ts), 29); // 30 + (-1)
        // `??` coalesces to a non-null default, then a field read
        let ts2 = "interface Box { v: i64; } \
                   function pick(b: Box | null): i64 { let def: Box = { v: 7 }; return (b ?? def).v; } \
                   function main(): i64 { let x: Box = { v: 100 }; return pick(x) + pick(null); }";
        assert_eq!(run_i64(ts2), 107); // 100 + 7
        // a self-referential nullable field (`next: Node | null`)
        let ts3 = "interface Node { val: i64; next: Node | null; } \
                   function main(): i64 { \
                     let tail: Node = { val: 2, next: null }; \
                     let head: Node = { val: 1, next: tail }; \
                     let n: Node | null = head.next; \
                     if (n !== null) { return head.val + n.val; } \
                     return head.val; }";
        assert_eq!(run_i64(ts3), 3); // 1 + 2
    }

    #[test]
    fn default_params() {
        // default on a top-level function — omitted then provided
        let ts = "function inc(x: i64, by: i64 = 1): i64 { return x + by; } \
                  function main(): i64 { return inc(10) + inc(10, 5); }";
        assert_eq!(run_i64(ts), 26); // 11 + 15
        // defaults on a constructor and a method
        let ts2 = "class Counter { \
                     n: i64; \
                     constructor(start: i64 = 100) { this.n = start; } \
                     add(d: i64 = 2): i64 { this.n = this.n + d; return this.n; } \
                   } \
                   function main(): i64 { \
                     let a = new Counter(); a.add(); \
                     let b = new Counter(0); b.add(40); \
                     return a.add() + b.n; }";
        assert_eq!(run_i64(ts2), 144); // a: 100→102→104 ; b: 0→40 ; 104 + 40
        // an enum member is a valid (constant) default
        let ts3 = "enum Mode { Off, On } \
                   function f(m: Mode = Mode.On): i64 { return m; } \
                   function main(): i64 { return f() + f(Mode.Off); }";
        assert_eq!(run_i64(ts3), 1); // 1 + 0
    }

    #[test]
    fn string_enums_and_destructuring() {
        // string enum: members are `str`; `E.M` is a string literal; concat works
        let ts = "enum Dir { North = \"N\", South = \"S\" } \
                  function main(): i64 { \
                    let d: Dir = Dir.North; \
                    let s: str = d + Dir.South; \
                    return len(s); }";
        assert_eq!(run_i64(ts), 2); // "NS"
        // array destructuring, including a hole
        let ts2 = "function main(): i64 { \
                     let xs: i64[] = [10, 20, 30]; \
                     let [a, , c] = xs; \
                     return a + c; }";
        assert_eq!(run_i64(ts2), 40); // 10 + 30
        // object destructuring from a struct: shorthand + renamed
        let ts3 = "interface P { x: i64; y: i64; } \
                   function main(): i64 { \
                     let p: P = { x: 5, y: 7 }; \
                     let { x, y: why } = p; \
                     return x * why; }";
        assert_eq!(run_i64(ts3), 35); // 5 * 7
    }

    #[test]
    fn ternary() {
        // nested ternary (right-associative) + use in arithmetic
        let ts = "function f(n: i64): i64 { return n < 0 ? 0 - n : n; } \
                  function grade(s: i64): i64 { return s >= 90 ? 1 : s >= 80 ? 2 : 3; } \
                  function main(): i64 { return f(-7) + grade(95) + grade(85) + grade(50); }";
        assert_eq!(run_i64(ts), 13); // 7 + 1 + 2 + 3
        // laziness: a fib written with `?:` only terminates if the untaken branch
        // (the recursive one) is NOT evaluated at the base case
        let ts2 = "function fib(n: i64): i64 { return n < 2 ? n : fib(n - 1) + fib(n - 2); } \
                   function main(): i64 { return fib(10); }";
        assert_eq!(run_i64(ts2), 55);
        // a ternary yielding a struct, then a field read (result type is tracked)
        let ts3 = "interface P { x: i64; } \
                   function pick(b: bool): P { let a: P = { x: 1 }; let c: P = { x: 9 }; return b ? a : c; } \
                   function main(): i64 { return pick(false).x; }";
        assert_eq!(run_i64(ts3), 9);
    }

    #[test]
    fn switch_statements() {
        // value cases + default + empty-case stacking + a returning case
        let ts = "function classify(n: i64): i64 { \
                    switch (n) { \
                      case 0: return 100; \
                      case 1: \
                      case 2: return 200; \
                      case 3: { let x: i64 = 5; return 300 + x; } \
                      default: return 999; \
                    } \
                  } \
                  function main(): i64 { \
                    return classify(0) + classify(1) + classify(2) + classify(3) + classify(7); }";
        // 100 + 200 + 200 + 305 + 999 = 1804
        assert_eq!(run_i64(ts), 1804);
        // break-terminated cases that mutate, plus a switch on an enum
        let ts2 = "enum Op { Add, Sub, Mul } \
                   function apply(op: Op, a: i64, b: i64): i64 { \
                     let r: i64 = 0; \
                     switch (op) { \
                       case Op.Add: r = a + b; break; \
                       case Op.Sub: r = a - b; break; \
                       case Op.Mul: r = a * b; break; \
                     } \
                     return r; \
                   } \
                   function main(): i64 { return apply(Op.Add, 3, 4) + apply(Op.Mul, 5, 6); }";
        assert_eq!(run_i64(ts2), 37); // 7 + 30
    }

    #[test]
    fn for_of_and_enums() {
        // for…of over an array, with `continue` and `break`
        let ts = "function main(): i64 { \
                    let xs: i64[] = [10, 20, 30, 40, 50]; \
                    let total: i64 = 0; \
                    for (const x of xs) { \
                      if (x == 30) { continue; } \
                      if (x == 50) { break; } \
                      total = total + x; \
                    } \
                    return total; }"; // 10 + 20 + 40 = 70
        assert_eq!(run_i64(ts), 70);
        // numeric enum: auto-increment + explicit value that auto-continues
        let ts2 = "enum Dir { North, East, South = 10, West } \
                   function main(): i64 { return Dir.North + Dir.East + Dir.South + Dir.West; }";
        assert_eq!(run_i64(ts2), 22); // 0 + 1 + 10 + 11
        // an enum as an i64-backed parameter type
        let ts3 = "enum Color { Red, Green, Blue } \
                   function val(c: Color): i64 { if (c == Color.Green) { return 99; } return 0; } \
                   function main(): i64 { return val(Color.Green); }";
        assert_eq!(run_i64(ts3), 99);
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
