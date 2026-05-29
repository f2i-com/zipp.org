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
        tuple_ids: RefCell::new(HashSet::new()),
        tmp: Cell::new(0),
        func_types: RefCell::new(Vec::new()),
        lambdas: RefCell::new(Vec::new()),
        next_lambda: Cell::new(0),
        fn_value_types: RefCell::new(HashMap::new()),
        array_helpers: RefCell::new(HashSet::new()),
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
    /// Struct ids that are tuples (`[T0, T1, …]` lowers to a struct with fields
    /// "0","1",…). Distinguishes positional indexing/construction from structs.
    tuple_ids: RefCell<HashSet<u32>>,
    /// Counter for fresh temporary names (e.g. `for…of` desugaring).
    tmp: Cell<u32>,
    /// Interned first-class function types (→ `Module::func_types`), find-or-create
    /// by structural signature so equal types share a `Type::Func` id.
    func_types: RefCell<Vec<z::FuncType>>,
    /// Arrow lambdas lifted to top-level functions, drained into the module's
    /// `funcs` after lowering.
    lambdas: RefCell<Vec<z::Func>>,
    /// Counter for fresh lambda names (`__lambda0`, …).
    next_lambda: Cell<u32>,
    /// Callable name → its first-class function type (`Type::Func`). Lets a bare
    /// function reference and `ztype_of` resolve a function used as a value.
    fn_value_types: RefCell<HashMap<String, z::Type>>,
    /// Names of synthesized array-method helpers already generated (dedup, so a
    /// given `map`/`filter`/`reduce` at fixed element types is emitted once).
    array_helpers: RefCell<HashSet<String>>,
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
                        // Intern its first-class function type so the function can
                        // be used as a value (`let f = thatFn` / passing it along).
                        let ptys: Vec<z::Type> = f
                            .params
                            .items
                            .iter()
                            .map(|p| self.param(p).map(|(_, t)| t))
                            .collect::<LResult<_>>()?;
                        let fty = self.intern_func_type(ptys, r);
                        self.fn_value_types.borrow_mut().insert(id.name.as_str().to_string(), fty);
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
        // Append lifted arrow lambdas (created on demand while lowering bodies).
        funcs.extend(self.lambdas.take());
        Ok(z::Module {
            funcs,
            structs: self.struct_decls.take(),
            func_types: self.func_types.take(),
        })
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
            z::Type::OptStruct(_)
            | z::Type::OptStr
            | z::Type::OptArr(_)
            | z::Type::OptI64
            | z::Type::OptF64
            | z::Type::OptBool => z::Expr::Null,
            // structs and function values have no zero default (need an initializer)
            z::Type::Struct(_) | z::Type::Null | z::Type::Func(_) => return None,
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
            // An array literal's type comes from its first element.
            z::Expr::Array(es) => z::Type::Array(self.ztype_of(es.first()?)?.as_elem()?),
            // `arr.push(x)` is the new length; `arr.pop()` is the element type.
            z::Expr::Push { .. } => z::Type::I64,
            z::Expr::Pop { arr } => {
                let z::Type::Array(e) = self.ztype_of(arr)? else { return None };
                e.to_type()
            }
            // A string method's result type (so chaining like `s.slice(1).indexOf(...)` resolves).
            z::Expr::StrOp { op, .. } => op.result(),
            // Comparisons/logicals are bool; arithmetic/bitwise share the operand
            // type. Lets arrow return types infer from simple bodies.
            z::Expr::Bin { op, l, .. } => match op {
                z::BinOp::Eq | z::BinOp::Ne | z::BinOp::Lt | z::BinOp::Le | z::BinOp::Gt
                | z::BinOp::Ge | z::BinOp::And | z::BinOp::Or => z::Type::Bool,
                _ => self.ztype_of(l)?,
            },
            z::Expr::Unary { op, e } => match op {
                z::UnOp::Not => z::Type::Bool,
                _ => self.ztype_of(e)?,
            },
            // A function value / closure's type is the interned `Func` type
            // recorded for it (a closure's type is its explicit signature).
            z::Expr::FuncRef(name) | z::Expr::MakeClosure { name, .. } => {
                self.fn_value_types.borrow().get(name).copied()?
            }
            // An indirect call yields the function type's return type.
            z::Expr::CallValue { callee, .. } => {
                let z::Type::Func(id) = self.ztype_of(callee)? else { return None };
                self.func_types.borrow().get(id as usize)?.ret
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

    /// Find-or-create a first-class function type, returning `Type::Func(id)`.
    /// Structural dedup keeps one id per unique signature.
    fn intern_func_type(&self, params: Vec<z::Type>, ret: z::Type) -> z::Type {
        let ft = z::FuncType { params, ret };
        let mut tbl = self.func_types.borrow_mut();
        let id = tbl.iter().position(|t| *t == ft).unwrap_or_else(|| {
            tbl.push(ft);
            tbl.len() - 1
        });
        z::Type::Func(id as u32)
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

    /// Lower an arrow lambda `(p: T, …) => expr` to a lifted top-level function.
    /// Captured enclosing locals become the lifted function's LEADING parameters
    /// (keeping their names, so the body is unchanged); the arrow expression then
    /// lowers to a `MakeClosure` that snapshots those captures (a bare `FuncRef`
    /// when nothing is captured). v0: expression body, explicitly-typed params,
    /// capture-by-value.
    fn lower_arrow(&self, a: &ArrowFunctionExpression) -> LResult<z::Expr> {
        if a.type_parameters.is_some() {
            return Err(self.err(a.span, "generic arrow functions aren't supported (v0)"));
        }
        if !a.expression {
            return Err(self.err(
                a.span,
                "only expression-bodied arrows `(x: T) => expr` are supported (v0)",
            ));
        }
        let body_ast = match a.body.statements.first() {
            Some(Statement::ExpressionStatement(es)) => &es.expression,
            _ => return Err(self.err(a.span, "expected an expression-bodied arrow")),
        };
        self.push_scope();
        let mut explicit = Vec::with_capacity(a.params.items.len());
        let mut param_names = HashSet::new();
        for p in &a.params.items {
            let (pname, pty) = self.param(p)?;
            self.bind(&pname, pty);
            param_names.insert(pname.clone());
            explicit.push(z::Param { name: pname, ty: pty });
        }
        let body = self.expr(body_ast)?;
        let captures = self.collect_captures(&body, &param_names);
        let ret = match &a.return_type {
            Some(ann) => self.ty(&ann.type_annotation)?,
            None => self.ztype_of(&body).ok_or_else(|| {
                self.err(a.span, "can't infer the arrow's return type — annotate it `(x: T): R => …`")
            })?,
        };
        self.pop_scope();
        // Lifted signature: captures first, then the explicit params.
        let mut lifted_params: Vec<z::Param> =
            captures.iter().map(|(n, t)| z::Param { name: n.clone(), ty: *t }).collect();
        lifted_params.extend(explicit.iter().cloned());
        let id = self.next_lambda.get();
        self.next_lambda.set(id + 1);
        let name = format!("__lambda{id}");
        let line = self.line(a.span);
        // The closure's *value* type is the explicit signature (captures are bound).
        let fty = self.intern_func_type(explicit.iter().map(|p| p.ty).collect(), ret);
        self.lambdas.borrow_mut().push(z::Func {
            name: name.clone(),
            params: lifted_params,
            ret,
            body: vec![z::Stmt { kind: z::StmtKind::Return(Some(body)), line }],
        });
        self.fn_rets.borrow_mut().insert(name.clone(), ret);
        self.fn_value_types.borrow_mut().insert(name.clone(), fty);
        if captures.is_empty() {
            Ok(z::Expr::FuncRef(name))
        } else {
            let cap_exprs = captures.into_iter().map(|(n, _)| z::Expr::Var(n)).collect();
            Ok(z::Expr::MakeClosure { name, captures: cap_exprs })
        }
    }

    /// Captured enclosing locals referenced in `body` (deduped, first-appearance
    /// order), excluding the arrow's own `params`. Each is paired with its type
    /// from the enclosing scope. Capture is by value (snapshot at creation).
    fn collect_captures(
        &self,
        body: &z::Expr,
        params: &HashSet<String>,
    ) -> Vec<(String, z::Type)> {
        let mut all = Vec::new();
        collect_vars(body, &mut all);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for v in all {
            if params.contains(&v) || !seen.insert(v.clone()) {
                continue;
            }
            // A capture resolves to an enclosing local — not a parameter (the top
            // frame), not a global function/enum (no scope binding).
            if let Some(t) = self.enclosing_type(&v) {
                out.push((v, t));
            }
        }
        out
    }

    /// Type of `name` in the *enclosing* scope (every frame except the innermost,
    /// which holds the arrow's own parameters), if it's a local there.
    fn enclosing_type(&self, name: &str) -> Option<z::Type> {
        let scope = self.scope.borrow();
        if scope.len() < 2 {
            return None;
        }
        scope[..scope.len() - 1].iter().rev().find_map(|m| m.get(name).copied())
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
            TSType::TSTypeReference(r) => self.resolve_ref(r)?,
            TSType::TSParenthesizedType(p) => self.ty(&p.type_annotation)?,
            // `[T0, T1, …]` → a synthetic struct with positional fields "0","1",…
            TSType::TSTupleType(tt) => z::Type::Struct(self.tuple_type_id(tt)?),
            // `(p: P, …) => R` → a first-class function type.
            TSType::TSFunctionType(ft) => {
                let mut params = Vec::with_capacity(ft.params.items.len());
                for p in &ft.params.items {
                    params.push(self.param(p)?.1);
                }
                let ret = self.ty(&ft.return_type.type_annotation)?;
                self.intern_func_type(params, ret)
            }
            // `T | null` / `T | undefined` → a nullable type (struct, or a
            // nullable scalar i64/f64/bool).
            TSType::TSUnionType(u) => {
                let non_null: Vec<&TSType> =
                    u.types.iter().filter(|t| !is_null_ts_type(t)).collect();
                if u.types.iter().any(is_null_ts_type) && non_null.len() == 1 {
                    let inner = self.ty(non_null[0])?;
                    inner.into_opt().ok_or_else(|| {
                        self.err(
                            u.span,
                            format!("`{inner:?} | null` isn't supported (only struct/i64/f64/bool)"),
                        )
                    })?
                } else {
                    return Err(self.err(u.span, "only `T | null` unions are supported"));
                }
            }
            other => return Err(self.err(span_of_type(other), "unsupported type")),
        })
    }

    /// Resolve a named type reference (`i64`, a struct/enum/type-param, a generic
    /// class instantiation `Box<i64>`).
    fn resolve_ref(&self, r: &TSTypeReference) -> LResult<z::Type> {
        let name = match &r.type_name {
            TSTypeName::IdentifierReference(id) => id.name.as_str(),
            _ => return Err(self.err(r.span, "qualified type names aren't supported")),
        };
        Ok(match name {
            "i64" => z::Type::I64,
            "i32" => z::Type::I32,
            "u32" => z::Type::U32,
            "u64" => z::Type::U64,
            "f64" => z::Type::F64,
            "bool" => z::Type::Bool,
            "string" | "str" => z::Type::Str,
            other => {
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
                    let args = match &r.type_arguments {
                        Some(ta) => ta.params.iter().map(|p| self.ty(p)).collect::<LResult<_>>()?,
                        None => {
                            return Err(
                                self.err(r.span, format!("'{other}' is generic — write `{other}<…>`")),
                            )
                        }
                    };
                    z::Type::Struct(self.instantiate_generic_class(other, args, r.span)?)
                } else if let Some(id) = self.struct_id(other) {
                    z::Type::Struct(id)
                } else {
                    return Err(self.err(r.span, format!("unknown type '{other}'")));
                }
            }
        })
    }

    /// Resolve a tuple type to its synthetic struct id (interned per shape).
    fn tuple_type_id(&self, tt: &TSTupleType) -> LResult<u32> {
        let mut elems = Vec::with_capacity(tt.element_types.len());
        for el in &tt.element_types {
            elems.push(self.tuple_elem_ty(el)?);
        }
        // find-or-create a struct named for the element-type sequence
        let name = format!(
            "__tup{}",
            elems.iter().map(|t| format!("_{}", mangle_type(*t))).collect::<String>()
        );
        if let Some(id) = self.struct_id(&name) {
            return Ok(id);
        }
        let fields = elems.iter().enumerate().map(|(i, t)| (i.to_string(), *t)).collect();
        let id = self.add_struct(z::StructDecl { name, fields });
        self.tuple_ids.borrow_mut().insert(id);
        Ok(id)
    }

    /// The type of one tuple element.
    fn tuple_elem_ty(&self, el: &TSTupleElement) -> LResult<z::Type> {
        match el {
            TSTupleElement::TSNumberKeyword(_) => Ok(z::Type::F64),
            TSTupleElement::TSBigIntKeyword(_) => Ok(z::Type::I64),
            TSTupleElement::TSBooleanKeyword(_) => Ok(z::Type::Bool),
            TSTupleElement::TSStringKeyword(_) => Ok(z::Type::Str),
            TSTupleElement::TSTypeReference(r) => self.resolve_ref(r),
            TSTupleElement::TSArrayType(a) => {
                let elem = self.ty(&a.element_type)?;
                let e = elem
                    .as_elem()
                    .ok_or_else(|| self.err(a.span, "array element type isn't a scalar"))?;
                Ok(z::Type::Array(e))
            }
            TSTupleElement::TSTupleType(t) => Ok(z::Type::Struct(self.tuple_type_id(t)?)),
            TSTupleElement::TSParenthesizedType(p) => self.ty(&p.type_annotation),
            _ => Err(self.err(oxc_span::GetSpan::span(el), "unsupported tuple element type")),
        }
    }

    /// Lower an array literal `[a, b]` to a nominal tuple `StructLit`.
    fn arr_tuple_lit(&self, arr: &ArrayExpression, id: u32) -> LResult<z::Expr> {
        let name = self.struct_name(id).unwrap();
        let nfields = self.struct_decls.borrow()[id as usize].fields.len();
        if arr.elements.len() != nfields {
            return Err(self.err(
                arr.span,
                format!("tuple expects {nfields} elements, got {}", arr.elements.len()),
            ));
        }
        let mut fields = Vec::with_capacity(nfields);
        for (i, el) in arr.elements.iter().enumerate() {
            let ex = el
                .as_expression()
                .ok_or_else(|| self.err(arr.span, "tuple holes/spreads aren't supported"))?;
            fields.push((i.to_string(), self.expr(ex)?));
        }
        Ok(z::Expr::StructLit { name, fields })
    }

    /// True if `t` is a tuple type (a synthetic positional struct).
    fn is_tuple(&self, t: z::Type) -> bool {
        matches!(t, z::Type::Struct(id) if self.tuple_ids.borrow().contains(&id))
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
                    // `return [ ... ]` from a tuple-typed function.
                    (Some(Expression::ArrayExpression(arr)), Some(id))
                        if self.is_tuple(z::Type::Struct(id)) =>
                    {
                        Some(self.arr_tuple_lit(arr, id)?)
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
                        // `let t: [i64, str] = [a, b]` — array literal in tuple context
                        (Some(z::Type::Struct(id)), Expression::ArrayExpression(arr))
                            if self.is_tuple(z::Type::Struct(*id)) =>
                        {
                            self.arr_tuple_lit(arr, *id)?
                        }
                        // `let xs: T[] = []` — a typed empty (growable) array.
                        (Some(z::Type::Array(elem)), Expression::ArrayExpression(arr))
                            if arr.elements.is_empty() =>
                        {
                            z::Expr::Repeat {
                                value: Box::new(elem_default(*elem)),
                                count: Box::new(z::Expr::Int(0)),
                            }
                        }
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
        // a tuple destructures by positional field, an array by index
        let tuple = self.ztype_of(&value).is_some_and(|t| self.is_tuple(t));
        let tmp_ty = self.ztype_of(&value);
        let tmp = self.fresh("d");
        let line = self.line(arr.span);
        out.push(z::Stmt { kind: z::StmtKind::Let { name: tmp.clone(), ty: None, value }, line });
        if let Some(t) = tmp_ty {
            self.bind(&tmp, t);
        }
        for (i, el) in arr.elements.iter().enumerate() {
            let Some(pat) = el else { continue }; // hole, e.g. `[, b]`
            let name = match pat {
                BindingPattern::BindingIdentifier(bi) => bi.name.as_str().to_string(),
                _ => return Err(self.err(arr.span, "nested destructuring isn't supported")),
            };
            let access = if tuple {
                z::Expr::Field { base: Box::new(z::Expr::Var(tmp.clone())), field: i.to_string() }
            } else {
                z::Expr::Index {
                    arr: Box::new(z::Expr::Var(tmp.clone())),
                    index: Box::new(z::Expr::Int(i as i64)),
                }
            };
            if let Some(t) = self.ztype_of(&access) {
                self.bind(&name, t);
            }
            out.push(z::Stmt { kind: z::StmtKind::Let { name, ty: None, value: access }, line });
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

    /// Resolve `recv?.method(args)` to `(recv, mangled-method-name, args)` by
    /// dispatching on the receiver's (non-null) class.
    fn resolve_opt_call(
        &self,
        c: &CallExpression,
    ) -> LResult<(z::Expr, String, Vec<z::Expr>)> {
        let m = match &c.callee {
            Expression::StaticMemberExpression(m) if m.optional => m,
            _ => return Err(self.err(c.span, "only `recv?.method(…)` optional calls are supported")),
        };
        let method = m.property.name.as_str();
        let recv = self.expr(&m.object)?;
        let cid = match self.ztype_of(&recv).map(|t| t.opt_inner().unwrap_or(t)) {
            Some(z::Type::Struct(id)) => id,
            _ => {
                return Err(self.err(
                    c.span,
                    "can't resolve the receiver's class for `?.m()` — annotate it",
                ))
            }
        };
        let cname = self.struct_name(cid).ok_or_else(|| self.err(c.span, "internal: unknown struct"))?;
        let name = format!("{cname}__{method}");
        let mut args = Vec::new();
        for a in &c.arguments {
            let ex = a
                .as_expression()
                .ok_or_else(|| self.err(c.span, "spread arguments aren't supported"))?;
            args.push(self.expr(ex)?);
        }
        Ok((recv, name, args))
    }

    /// Lower a `.`/`?.` member access: optional `base?.field`, an enum member,
    /// `arr.length`, or a struct field read.
    fn member(&self, m: &StaticMemberExpression) -> LResult<z::Expr> {
        let field = m.property.name.as_str();
        if m.optional {
            return Ok(z::Expr::OptField {
                base: Box::new(self.expr(&m.object)?),
                field: field.to_string(),
            });
        }
        // `Color.Red` → the enum member's value.
        if let Expression::Identifier(obj) = &m.object {
            if let Some(kind) = self.enums.get(obj.name.as_str()) {
                let val = match kind {
                    EnumKind::Int(mem) => mem.get(field).map(|&v| z::Expr::Int(v)),
                    EnumKind::Str(mem) => mem.get(field).map(|s| z::Expr::Str(s.clone())),
                };
                return val.ok_or_else(|| {
                    self.err(m.span, format!("enum '{}' has no member '{field}'", obj.name.as_str()))
                });
            }
        }
        if field == "length" {
            Ok(z::Expr::Call { name: "len".into(), args: vec![self.expr(&m.object)?] })
        } else {
            Ok(z::Expr::Field { base: Box::new(self.expr(&m.object)?), field: field.to_string() })
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
                let n = id.name.as_str();
                // `undefined` is conflated with `null` in this subset.
                if n == "undefined" {
                    z::Expr::Null
                } else if self.lookup(n).is_none() && self.fn_value_types.borrow().contains_key(n) {
                    // A top-level function used as a value (not shadowed by a local).
                    z::Expr::FuncRef(n.to_string())
                } else {
                    z::Expr::Var(n.to_string())
                }
            }
            Expression::ArrowFunctionExpression(a) => self.lower_arrow(a)?,
            Expression::ParenthesizedExpression(p) => self.expr(&p.expression)?,
            Expression::BinaryExpression(b) => z::Expr::Bin {
                op: self.bin_op(b.operator, b.span)?,
                l: Box::new(self.expr(&b.left)?),
                r: Box::new(self.expr(&b.right)?),
            },
            Expression::LogicalExpression(l) => match l.operator {
                LogicalOperator::And => z::Expr::Bin {
                    op: z::BinOp::And,
                    l: Box::new(self.expr(&l.left)?),
                    r: Box::new(self.expr(&l.right)?),
                },
                LogicalOperator::Or => z::Expr::Bin {
                    op: z::BinOp::Or,
                    l: Box::new(self.expr(&l.left)?),
                    r: Box::new(self.expr(&l.right)?),
                },
                LogicalOperator::Coalesce => {
                    // `base?.field ?? default` / `recv?.m() ?? default` fuse to a
                    // type-safe default (works even for a scalar field/return).
                    if let Some(m) = optional_member(&l.left) {
                        z::Expr::OptFieldOr {
                            base: Box::new(self.expr(&m.object)?),
                            field: m.property.name.as_str().to_string(),
                            default: Box::new(self.expr(&l.right)?),
                        }
                    } else if let Some(call) = optional_call(&l.left) {
                        let (recv, name, args) = self.resolve_opt_call(call)?;
                        z::Expr::OptCallOr {
                            recv: Box::new(recv),
                            name,
                            args,
                            default: Box::new(self.expr(&l.right)?),
                        }
                    } else {
                        z::Expr::Coalesce {
                            lhs: Box::new(self.expr(&l.left)?),
                            rhs: Box::new(self.expr(&l.right)?),
                        }
                    }
                }
            },
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
                if a.elements.is_empty() {
                    return Err(self.err(
                        a.span,
                        "an empty array literal `[]` needs a type — annotate the binding \
                         (`let xs: i64[] = []`)",
                    ));
                }
                let mut elems = Vec::new();
                for el in &a.elements {
                    let ex = el
                        .as_expression()
                        .ok_or_else(|| self.err(a.span, "array holes/spreads aren't supported"))?;
                    elems.push(self.expr(ex)?);
                }
                z::Expr::Array(elems)
            }
            Expression::ComputedMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                // a tuple `t[0]` is a positional field access (constant index);
                // an array `a[i]` is a runtime index.
                if self.ztype_of(&obj).is_some_and(|t| self.is_tuple(t)) {
                    let idx = const_int_lit(&m.expression).ok_or_else(|| {
                        self.err(m.span, "a tuple index must be a constant integer")
                    })?;
                    z::Expr::Field { base: Box::new(obj), field: idx.to_string() }
                } else {
                    z::Expr::Index {
                        arr: Box::new(obj),
                        index: Box::new(self.expr(&m.expression)?),
                    }
                }
            }
            Expression::StaticMemberExpression(m) => self.member(m)?,
            // `a?.b` / `a?.m()` — optional chaining (the chain is wrapped here).
            Expression::ChainExpression(c) => match &c.expression {
                ChainElement::StaticMemberExpression(m) => self.member(m)?,
                ChainElement::CallExpression(call) => {
                    let (recv, name, args) = self.resolve_opt_call(call)?;
                    z::Expr::OptCall { recv: Box::new(recv), name, args }
                }
                _ => {
                    return Err(self.err(
                        c.span,
                        "only `a?.b` / `a?.m()` optional chaining is supported (not `?.[…]`)",
                    ))
                }
            },
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
                } else if let (z::Type::Struct(id), Expression::ArrayExpression(arr)) =
                    (to, &a.expression)
                {
                    if self.is_tuple(to) {
                        self.arr_tuple_lit(arr, id)? // `[a, b] as [i64, str]`
                    } else {
                        self.expr(&a.expression)?
                    }
                } else {
                    self.expr(&a.expression)?
                }
            }
            // `e!` non-null assertion — a no-op for lowering (ZIPP's own checker
            // tracks nullability); lets `arr.pop()!` satisfy tsc's `T | undefined`.
            Expression::TSNonNullExpression(n) => self.expr(&n.expression)?,
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

    /// Dispatch a builtin array method on an array receiver of element type `elem`.
    /// `push`/`pop` are primitives; the rest call a synthesized, per-element-type
    /// helper function (a plain loop) generated on demand. The helper's signature
    /// also enforces argument types (via the normal `Call` type-check).
    fn array_method(
        &self,
        recv: z::Expr,
        elem: z::Elem,
        method: &str,
        c: &CallExpression,
    ) -> LResult<z::Expr> {
        let mut args = self.lower_args(c)?;
        let tag = elem_tag(elem);
        match method {
            "push" => {
                if args.len() != 1 {
                    return Err(self.err(c.span, "push(x) takes one argument"));
                }
                Ok(z::Expr::Push { arr: Box::new(recv), value: Box::new(args.remove(0)) })
            }
            "pop" => {
                if !args.is_empty() {
                    return Err(self.err(c.span, "pop() takes no arguments"));
                }
                Ok(z::Expr::Pop { arr: Box::new(recv) })
            }
            // value scans → native (no push, no closure)
            "indexOf" | "includes" => {
                if args.len() != 1 {
                    return Err(self.err(c.span, format!("{method}(x) takes one argument")));
                }
                let includes = method == "includes";
                let ret = if includes { z::Type::Bool } else { z::Type::I64 };
                let name = self.ensure_helper(format!("__{method}_{tag}"), ret, |n| {
                    build_find_helper(n, elem, includes)
                });
                Ok(z::Expr::Call { name, args: prepend(recv, args) })
            }
            // in-place mutation → native, returns the receiver
            "reverse" => {
                if !args.is_empty() {
                    return Err(self.err(c.span, "reverse() takes no arguments"));
                }
                let name = self.ensure_helper(format!("__reverse_{tag}"), z::Type::Array(elem), |n| {
                    build_reverse_helper(n, elem)
                });
                Ok(z::Expr::Call { name, args: vec![recv] })
            }
            "fill" => {
                if args.is_empty() || args.len() > 3 {
                    return Err(self.err(c.span, "fill(value, start?, end?) takes 1 to 3 arguments"));
                }
                let n = args.len();
                let name = self.ensure_helper(format!("__fill{n}_{tag}"), z::Type::Array(elem), |nm| {
                    build_fill_helper(nm, elem, n)
                });
                Ok(z::Expr::Call { name, args: prepend(recv, args) })
            }
            // fresh-copy builders → interpreter-tier (push)
            "slice" => {
                if args.len() > 2 {
                    return Err(self.err(c.span, "slice(start?, end?) takes 0 to 2 arguments"));
                }
                let n = args.len();
                let name = self.ensure_helper(format!("__slice{n}_{tag}"), z::Type::Array(elem), |nm| {
                    build_slice_helper(nm, elem, n)
                });
                Ok(z::Expr::Call { name, args: prepend(recv, args) })
            }
            "concat" => {
                if args.len() != 1 {
                    return Err(self.err(c.span, "concat(other) takes one array argument"));
                }
                let name = self.ensure_helper(format!("__concat_{tag}"), z::Type::Array(elem), |n| {
                    build_concat_helper(n, elem)
                });
                Ok(z::Expr::Call { name, args: prepend(recv, args) })
            }
            "map" | "filter" | "reduce" | "some" | "every" | "findIndex" => {
                self.array_hof(recv, elem, method, args, c.span)
            }
            other => Err(self.err(
                c.span,
                format!(
                    "unknown array method '.{other}()' — supported: push, pop, indexOf, includes, \
                     reverse, fill, slice, concat, map, filter, reduce, some, every, findIndex"
                ),
            )),
        }
    }

    /// Higher-order (callback-taking) array methods. Resolves the callback's
    /// interned function type, validates its shape, ensures the matching helper
    /// exists, and emits `__method_<types>(arr, callback[, init])`.
    fn array_hof(
        &self,
        recv: z::Expr,
        elem: z::Elem,
        method: &str,
        args: Vec<z::Expr>,
        span: Span,
    ) -> LResult<z::Expr> {
        let cb = args.first().ok_or_else(|| {
            self.err(span, format!(".{method}() needs a callback function"))
        })?;
        let z::Type::Func(fid) = self.ztype_of(cb).ok_or_else(|| {
            self.err(span, format!(".{method}()'s callback type is unknown — pass an arrow or a named function"))
        })?
        else {
            return Err(self.err(span, format!(".{method}() expects a function argument")));
        };
        let ft = self.func_types.borrow()[fid as usize].clone();
        let t = elem.to_type();
        let f_ty = z::Type::Func(fid);
        let tag = elem_tag(elem);
        let (name, call_args) = match method {
            "map" => {
                if args.len() != 1 || ft.params != [t] {
                    return Err(self.err(span, "map((x: T) => U) takes one callback of one element"));
                }
                let u = ft.ret.as_elem().ok_or_else(|| {
                    self.err(span, "map's callback must return a scalar (array element type)")
                })?;
                let name = self.ensure_helper(
                    format!("__map_{tag}_{}", elem_tag(u)),
                    z::Type::Array(u),
                    |n| build_map_helper(n, elem, u, f_ty),
                );
                (name, prepend(recv, args))
            }
            "filter" => {
                if args.len() != 1 || ft.params != [t] || ft.ret != z::Type::Bool {
                    return Err(self.err(span, "filter((x: T) => bool) takes one boolean predicate"));
                }
                let name = self.ensure_helper(format!("__filter_{tag}"), z::Type::Array(elem), |n| {
                    build_filter_helper(n, elem, f_ty)
                });
                (name, prepend(recv, args))
            }
            "reduce" => {
                if args.len() != 2 {
                    return Err(self.err(span, "reduce((acc, x) => acc, init) takes a callback and an initial value"));
                }
                let u = ft.ret.as_elem().ok_or_else(|| {
                    self.err(span, "reduce's accumulator must be a scalar (i64/f64/bool)")
                })?;
                if ft.params != [ft.ret, t] {
                    return Err(self.err(span, "reduce's callback must be `(acc: U, x: T) => U`"));
                }
                let init_ty = self.ztype_of(&args[1]).ok_or_else(|| {
                    self.err(span, "reduce's initial value has an unknown type")
                })?;
                if init_ty != ft.ret {
                    return Err(self.err(span, format!("reduce's initial value must be {:?}", ft.ret)));
                }
                let name = self.ensure_helper(
                    format!("__reduce_{tag}_{}", elem_tag(u)),
                    u.to_type(),
                    |n| build_reduce_helper(n, elem, u, f_ty),
                );
                (name, prepend(recv, args))
            }
            // predicate scans: some/every → bool, findIndex → i64
            "some" | "every" | "findIndex" => {
                if args.len() != 1 || ft.params != [t] || ft.ret != z::Type::Bool {
                    return Err(self.err(
                        span,
                        format!(".{method}((x: T) => bool) takes one boolean predicate"),
                    ));
                }
                let ret = if method == "findIndex" { z::Type::I64 } else { z::Type::Bool };
                let m = method.to_string();
                let name = self.ensure_helper(format!("__{method}_{tag}"), ret, |n| {
                    build_predicate_helper(n, elem, f_ty, &m)
                });
                (name, prepend(recv, args))
            }
            _ => unreachable!(),
        };
        Ok(z::Expr::Call { name, args: call_args })
    }

    /// Generate (once) and register a synthesized array helper, returning its name.
    /// `build` is only run the first time a given name is seen.
    fn ensure_helper(
        &self,
        name: String,
        ret: z::Type,
        build: impl FnOnce(&str) -> z::Func,
    ) -> String {
        if self.array_helpers.borrow_mut().insert(name.clone()) {
            let func = build(&name);
            self.fn_rets.borrow_mut().insert(name.clone(), ret);
            self.lambdas.borrow_mut().push(func);
        }
        name
    }

    /// Dispatch a builtin string method on a string receiver. Most are runtime
    /// primitives (`StrOp`); `includes`/`startsWith` desugar to `indexOf`. The
    /// receiver is used exactly once (no double-evaluation).
    fn str_method(&self, recv: z::Expr, method: &str, c: &CallExpression) -> LResult<z::Expr> {
        use z::StrOpKind as K;
        let mut args = self.lower_args(c)?;
        let n = args.len();
        let span = c.span;
        // `recv.indexOf(needle) <cmp> 0` — the shared shape of includes/startsWith.
        let index_cmp = |recv: z::Expr, needle: z::Expr, op: z::BinOp| z::Expr::Bin {
            op,
            l: Box::new(z::Expr::StrOp { op: K::IndexOf, args: vec![recv, needle, z::Expr::Int(0)] }),
            r: Box::new(z::Expr::Int(0)),
        };
        match method {
            "charCodeAt" | "charAt" => {
                if n > 1 {
                    return Err(self.err(span, format!("{method}(index?) takes 0 or 1 arguments")));
                }
                let i = if n == 1 { args.remove(0) } else { z::Expr::Int(0) };
                let op = if method == "charCodeAt" { K::ByteAt } else { K::CharAt };
                Ok(z::Expr::StrOp { op, args: vec![recv, i] })
            }
            "slice" => match n {
                0 => Ok(z::Expr::StrOp { op: K::SliceFrom, args: vec![recv, z::Expr::Int(0)] }),
                1 => Ok(z::Expr::StrOp { op: K::SliceFrom, args: vec![recv, args.remove(0)] }),
                2 => {
                    let start = args.remove(0);
                    let end = args.remove(0);
                    Ok(z::Expr::StrOp { op: K::Slice, args: vec![recv, start, end] })
                }
                _ => Err(self.err(span, "slice(start?, end?) takes 0 to 2 arguments")),
            },
            "indexOf" => {
                if n == 0 || n > 2 {
                    return Err(self.err(span, "indexOf(needle, from?) takes 1 or 2 arguments"));
                }
                let needle = args.remove(0);
                let from = if n == 2 { args.remove(0) } else { z::Expr::Int(0) };
                Ok(z::Expr::StrOp { op: K::IndexOf, args: vec![recv, needle, from] })
            }
            "lastIndexOf" => {
                if n != 1 {
                    return Err(self.err(span, "lastIndexOf(needle) takes one argument"));
                }
                Ok(z::Expr::StrOp { op: K::LastIndexOf, args: vec![recv, args.remove(0)] })
            }
            "repeat" => {
                if n != 1 {
                    return Err(self.err(span, "repeat(count) takes one argument"));
                }
                Ok(z::Expr::StrOp { op: K::Repeat, args: vec![recv, args.remove(0)] })
            }
            "endsWith" => {
                if n != 1 {
                    return Err(self.err(span, "endsWith(suffix) takes one argument"));
                }
                Ok(z::Expr::StrOp { op: K::EndsWith, args: vec![recv, args.remove(0)] })
            }
            "includes" => {
                if n != 1 {
                    return Err(self.err(span, "includes(needle) takes one argument"));
                }
                Ok(index_cmp(recv, args.remove(0), z::BinOp::Ge))
            }
            "startsWith" => {
                if n != 1 {
                    return Err(self.err(span, "startsWith(prefix) takes one argument"));
                }
                Ok(index_cmp(recv, args.remove(0), z::BinOp::Eq))
            }
            other => Err(self.err(
                span,
                format!(
                    "unknown string method '.{other}()' — supported: charCodeAt, charAt, slice, \
                     indexOf, lastIndexOf, includes, startsWith, endsWith, repeat"
                ),
            )),
        }
    }

    /// Lower a call's positional arguments (rejecting spreads).
    fn lower_args(&self, c: &CallExpression) -> LResult<Vec<z::Expr>> {
        let mut args = Vec::with_capacity(c.arguments.len());
        for a in &c.arguments {
            let ex = a
                .as_expression()
                .ok_or_else(|| self.err(c.span, "spread arguments aren't supported"))?;
            args.push(self.expr(ex)?);
        }
        Ok(args)
    }

    /// A call is a method call (`obj.m(args)`), a numeric cast (`i64(x)`),
    /// `len`/a math builtin, an indirect call through a function value, or a
    /// user function call.
    fn call(&self, c: &CallExpression) -> LResult<z::Expr> {
        // Method call: `obj.m(args)` → `Class__m(obj, args)`. Resolve the
        // receiver's class via the type tracker.
        if let Expression::StaticMemberExpression(m) = &c.callee {
            let method = m.property.name.as_str();
            let recv = self.expr(&m.object)?;
            // Builtin array methods (push/pop/map/filter/reduce) on an array receiver.
            if let Some(z::Type::Array(elem)) = self.ztype_of(&recv) {
                return self.array_method(recv, elem, method, c);
            }
            // Builtin string methods on a string receiver.
            if self.ztype_of(&recv) == Some(z::Type::Str) {
                return self.str_method(recv, method, c);
            }
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
        // Indirect call on a non-identifier callee (a closure returned by a call,
        // a parenthesized/immediately-invoked arrow): lower it and dispatch when
        // it's function-typed.
        if !matches!(&c.callee, Expression::Identifier(_)) {
            let callee = self.expr(&c.callee)?;
            if matches!(self.ztype_of(&callee), Some(z::Type::Func(_))) {
                let args = self.lower_args(c)?;
                return Ok(z::Expr::CallValue { callee: Box::new(callee), args });
            }
            return Err(self.err(
                c.span,
                "this expression isn't callable (only functions and methods can be called)",
            ));
        }
        let name = match &c.callee {
            Expression::Identifier(id) => id.name.as_str().to_string(),
            _ => unreachable!("non-identifier callees handled above"),
        };
        let mut args = self.lower_args(c)?;
        // Indirect call through a function-valued local (a parameter or `let`
        // holding a function). Direct calls to named functions fall through.
        if matches!(self.lookup(&name), Some(z::Type::Func(_))) {
            return Ok(z::Expr::CallValue { callee: Box::new(z::Expr::Var(name)), args });
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

/// Collect every variable name referenced in a lowered expression (used by the
/// arrow-capture check).
fn collect_vars(e: &z::Expr, out: &mut Vec<String>) {
    use z::Expr::*;
    match e {
        Var(n) => out.push(n.clone()),
        Cast { e, .. } | Unary { e, .. } => collect_vars(e, out),
        Bin { l, r, .. } => {
            collect_vars(l, out);
            collect_vars(r, out);
        }
        Cond { cond, then, els } => {
            collect_vars(cond, out);
            collect_vars(then, out);
            collect_vars(els, out);
        }
        Call { args, .. } => args.iter().for_each(|a| collect_vars(a, out)),
        Array(es) => es.iter().for_each(|a| collect_vars(a, out)),
        Repeat { value, count } => {
            collect_vars(value, out);
            collect_vars(count, out);
        }
        Index { arr, index } => {
            collect_vars(arr, out);
            collect_vars(index, out);
        }
        Field { base, .. } | OptField { base, .. } => collect_vars(base, out),
        StructLit { fields, .. } => fields.iter().for_each(|(_, e)| collect_vars(e, out)),
        Coalesce { lhs, rhs } => {
            collect_vars(lhs, out);
            collect_vars(rhs, out);
        }
        OptFieldOr { base, default, .. } => {
            collect_vars(base, out);
            collect_vars(default, out);
        }
        OptCall { recv, args, .. } => {
            collect_vars(recv, out);
            args.iter().for_each(|a| collect_vars(a, out));
        }
        OptCallOr { recv, args, default, .. } => {
            collect_vars(recv, out);
            args.iter().for_each(|a| collect_vars(a, out));
            collect_vars(default, out);
        }
        CallValue { callee, args } => {
            collect_vars(callee, out);
            args.iter().for_each(|a| collect_vars(a, out));
        }
        // A closure's captures are expressions in the enclosing scope — their
        // variables count (this is how a nested arrow's captures propagate out).
        MakeClosure { captures, .. } => captures.iter().for_each(|c| collect_vars(c, out)),
        Push { arr, value } => {
            collect_vars(arr, out);
            collect_vars(value, out);
        }
        Pop { arr } => collect_vars(arr, out),
        StrOp { args, .. } => args.iter().for_each(|a| collect_vars(a, out)),
        FuncRef(_) | Int(_) | Float(_) | Bool(_) | Str(_) | Null => {}
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
        z::Expr::OptField { base, .. } => expr_has_var(base),
        z::Expr::OptFieldOr { base, default, .. } => {
            expr_has_var(base) || expr_has_var(default)
        }
        z::Expr::OptCall { recv, args, .. } => {
            expr_has_var(recv) || args.iter().any(expr_has_var)
        }
        z::Expr::OptCallOr { recv, args, default, .. } => {
            expr_has_var(recv) || args.iter().any(expr_has_var) || expr_has_var(default)
        }
        // A function value names a function, not a variable; an indirect call's
        // callee/args may; a closure's captures do.
        z::Expr::FuncRef(_) => false,
        z::Expr::CallValue { callee, args } => {
            expr_has_var(callee) || args.iter().any(expr_has_var)
        }
        z::Expr::MakeClosure { captures, .. } => captures.iter().any(expr_has_var),
        z::Expr::Push { arr, value } => expr_has_var(arr) || expr_has_var(value),
        z::Expr::Pop { arr } => expr_has_var(arr),
        z::Expr::StrOp { args, .. } => args.iter().any(expr_has_var),
        z::Expr::Int(_) | z::Expr::Float(_) | z::Expr::Bool(_) | z::Expr::Str(_) | z::Expr::Null => {
            false
        }
    }
}

/// If `e` is an optional member access `base?.field`, return that member.
fn optional_member<'e, 'a>(e: &'e Expression<'a>) -> Option<&'e StaticMemberExpression<'a>> {
    if let Expression::ChainExpression(c) = e {
        if let ChainElement::StaticMemberExpression(m) = &c.expression {
            if m.optional {
                return Some(m);
            }
        }
    }
    None
}

/// If `e` is an optional method call `recv?.method(args)`, return that call.
fn optional_call<'e, 'a>(e: &'e Expression<'a>) -> Option<&'e CallExpression<'a>> {
    if let Expression::ChainExpression(c) = e {
        if let ChainElement::CallExpression(call) = &c.expression {
            if matches!(&call.callee, Expression::StaticMemberExpression(m) if m.optional) {
                return Some(call);
            }
        }
    }
    None
}

/// A constant integer literal (for a tuple index), if `e` is one.
fn const_int_lit(e: &Expression) -> Option<i64> {
    match e {
        Expression::NumericLiteral(n) if n.value.fract() == 0.0 => Some(n.value as i64),
        Expression::ParenthesizedExpression(p) => const_int_lit(&p.expression),
        _ => None,
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

// --- Synthesized array-method helpers (plain ZIPP loops over a growable array) ---

fn stmt(kind: z::StmtKind) -> z::Stmt {
    z::Stmt { kind, line: 0 }
}
fn prepend(head: z::Expr, rest: Vec<z::Expr>) -> Vec<z::Expr> {
    let mut v = Vec::with_capacity(rest.len() + 1);
    v.push(head);
    v.extend(rest);
    v
}
fn elem_tag(e: z::Elem) -> &'static str {
    match e {
        z::Elem::I64 => "i64",
        z::Elem::F64 => "f64",
        z::Elem::Bool => "bool",
    }
}
fn elem_default(e: z::Elem) -> z::Expr {
    match e {
        z::Elem::I64 => z::Expr::Int(0),
        z::Elem::F64 => z::Expr::Float(0.0),
        z::Elem::Bool => z::Expr::Bool(false),
    }
}
/// `__i < len(arr)` — the shared loop condition.
fn lt_len() -> z::Expr {
    z::Expr::Bin {
        op: z::BinOp::Lt,
        l: Box::new(z::Expr::Var("__i".into())),
        r: Box::new(z::Expr::Call { name: "len".into(), args: vec![z::Expr::Var("arr".into())] }),
    }
}
/// `let __i = 0` and `__i = __i + 1` — the shared loop init/step.
fn loop_init() -> z::Stmt {
    stmt(z::StmtKind::Let { name: "__i".into(), ty: None, value: z::Expr::Int(0) })
}
fn loop_step() -> z::Stmt {
    stmt(z::StmtKind::Assign {
        target: z::Expr::Var("__i".into()),
        value: z::Expr::Bin {
            op: z::BinOp::Add,
            l: Box::new(z::Expr::Var("__i".into())),
            r: Box::new(z::Expr::Int(1)),
        },
    })
}
/// `arr[__i]`.
fn arr_at_i() -> z::Expr {
    z::Expr::Index {
        arr: Box::new(z::Expr::Var("arr".into())),
        index: Box::new(z::Expr::Var("__i".into())),
    }
}
/// `f(args)` — call the callback parameter.
fn call_f(args: Vec<z::Expr>) -> z::Expr {
    z::Expr::CallValue { callee: Box::new(z::Expr::Var("f".into())), args }
}

/// `function __map_T_U(arr: T[], f: (T)=>U): U[] { let __r: U[] = []; for (…) __r.push(f(arr[__i])); return __r; }`
fn build_map_helper(name: &str, t: z::Elem, u: z::Elem, f_ty: z::Type) -> z::Func {
    use z::{Expr, Param, StmtKind, Type};
    let body = vec![
        stmt(StmtKind::Let {
            name: "__r".into(),
            ty: Some(Type::Array(u)),
            value: Expr::Repeat { value: Box::new(elem_default(u)), count: Box::new(Expr::Int(0)) },
        }),
        stmt(StmtKind::For {
            init: Some(Box::new(loop_init())),
            cond: lt_len(),
            step: Some(Box::new(loop_step())),
            body: vec![stmt(StmtKind::ExprStmt(Expr::Push {
                arr: Box::new(Expr::Var("__r".into())),
                value: Box::new(call_f(vec![arr_at_i()])),
            }))],
        }),
        stmt(StmtKind::Return(Some(Expr::Var("__r".into())))),
    ];
    z::Func {
        name: name.into(),
        params: vec![
            Param { name: "arr".into(), ty: Type::Array(t) },
            Param { name: "f".into(), ty: f_ty },
        ],
        ret: Type::Array(u),
        body,
    }
}

/// `function __filter_T(arr: T[], f: (T)=>bool): T[] { … if (f(arr[__i])) __r.push(arr[__i]); … }`
fn build_filter_helper(name: &str, t: z::Elem, f_ty: z::Type) -> z::Func {
    use z::{Expr, Param, StmtKind, Type};
    let body = vec![
        stmt(StmtKind::Let {
            name: "__r".into(),
            ty: Some(Type::Array(t)),
            value: Expr::Repeat { value: Box::new(elem_default(t)), count: Box::new(Expr::Int(0)) },
        }),
        stmt(StmtKind::For {
            init: Some(Box::new(loop_init())),
            cond: lt_len(),
            step: Some(Box::new(loop_step())),
            body: vec![stmt(StmtKind::If {
                cond: call_f(vec![arr_at_i()]),
                then_b: vec![stmt(StmtKind::ExprStmt(Expr::Push {
                    arr: Box::new(Expr::Var("__r".into())),
                    value: Box::new(arr_at_i()),
                }))],
                else_b: vec![],
            })],
        }),
        stmt(StmtKind::Return(Some(Expr::Var("__r".into())))),
    ];
    z::Func {
        name: name.into(),
        params: vec![
            Param { name: "arr".into(), ty: Type::Array(t) },
            Param { name: "f".into(), ty: f_ty },
        ],
        ret: Type::Array(t),
        body,
    }
}

/// `function __reduce_T_U(arr: T[], f: (U,T)=>U, init: U): U { let __acc = init; for (…) __acc = f(__acc, arr[__i]); return __acc; }`
fn build_reduce_helper(name: &str, t: z::Elem, u: z::Elem, f_ty: z::Type) -> z::Func {
    use z::{Expr, Param, StmtKind, Type};
    let body = vec![
        stmt(StmtKind::Let {
            name: "__acc".into(),
            ty: Some(u.to_type()),
            value: Expr::Var("init".into()),
        }),
        stmt(StmtKind::For {
            init: Some(Box::new(loop_init())),
            cond: lt_len(),
            step: Some(Box::new(loop_step())),
            body: vec![stmt(StmtKind::Assign {
                target: Expr::Var("__acc".into()),
                value: call_f(vec![Expr::Var("__acc".into()), arr_at_i()]),
            })],
        }),
        stmt(StmtKind::Return(Some(Expr::Var("__acc".into())))),
    ];
    z::Func {
        name: name.into(),
        params: vec![
            Param { name: "arr".into(), ty: Type::Array(t) },
            Param { name: "f".into(), ty: f_ty },
            Param { name: "init".into(), ty: u.to_type() },
        ],
        ret: u.to_type(),
        body,
    }
}

// Compact AST constructors for the synthesized helpers above.
fn var(n: &str) -> z::Expr {
    z::Expr::Var(n.into())
}
fn int(i: i64) -> z::Expr {
    z::Expr::Int(i)
}
fn bin(op: z::BinOp, l: z::Expr, r: z::Expr) -> z::Expr {
    z::Expr::Bin { op, l: Box::new(l), r: Box::new(r) }
}
fn idx(arr: z::Expr, i: z::Expr) -> z::Expr {
    z::Expr::Index { arr: Box::new(arr), index: Box::new(i) }
}
fn len_of(arr: &str) -> z::Expr {
    z::Expr::Call { name: "len".into(), args: vec![var(arr)] }
}
fn ret(e: z::Expr) -> z::Stmt {
    stmt(z::StmtKind::Return(Some(e)))
}
fn let_(n: &str, val: z::Expr) -> z::Stmt {
    stmt(z::StmtKind::Let { name: n.into(), ty: None, value: val })
}
fn assign(target: z::Expr, val: z::Expr) -> z::Stmt {
    stmt(z::StmtKind::Assign { target, value: val })
}
fn inc(n: &str) -> z::Stmt {
    assign(var(n), bin(z::BinOp::Add, var(n), int(1)))
}
fn while_(cond: z::Expr, body: Vec<z::Stmt>) -> z::Stmt {
    stmt(z::StmtKind::While { cond, body })
}
fn if_then(cond: z::Expr, then_b: Vec<z::Stmt>) -> z::Stmt {
    stmt(z::StmtKind::If { cond, then_b, else_b: vec![] })
}
fn push_stmt(arr: &str, val: z::Expr) -> z::Stmt {
    stmt(z::StmtKind::ExprStmt(z::Expr::Push {
        arr: Box::new(var(arr)),
        value: Box::new(val),
    }))
}
/// `for (let __i = 0; __i < len(arr); __i = __i + 1) { body }`.
fn for_i(body: Vec<z::Stmt>) -> z::Stmt {
    stmt(z::StmtKind::For {
        init: Some(Box::new(loop_init())),
        cond: lt_len(),
        step: Some(Box::new(loop_step())),
        body,
    })
}
/// Clamp/normalize a slice index `src` against `__n` (TS semantics): a negative
/// index counts from the end, then clamp to `[0, __n]`.
fn norm_idx(src: z::Expr) -> z::Expr {
    z::Expr::Cond {
        cond: Box::new(bin(z::BinOp::Lt, src.clone(), int(0))),
        then: Box::new(z::Expr::Call {
            name: "max".into(),
            args: vec![bin(z::BinOp::Add, var("__n"), src.clone()), int(0)],
        }),
        els: Box::new(z::Expr::Call { name: "min".into(), args: vec![src, var("__n")] }),
    }
}
/// `arr[__i] (==|SameValueZero) x` — the element comparison for indexOf/includes.
/// `includes` on an `f64` array also matches NaN (`e !== e && x !== x`).
fn elem_eq(t: z::Elem, same_value_zero: bool) -> z::Expr {
    let plain = bin(z::BinOp::Eq, arr_at_i(), var("x"));
    if same_value_zero && t == z::Elem::F64 {
        bin(
            z::BinOp::Or,
            plain,
            bin(
                z::BinOp::And,
                bin(z::BinOp::Ne, arr_at_i(), arr_at_i()),
                bin(z::BinOp::Ne, var("x"), var("x")),
            ),
        )
    } else {
        plain
    }
}

/// `function __indexOf_T(arr: T[], x: T): i64` / `__includes_T(...): bool` —
/// a linear scan; `includes` uses SameValueZero so it matches NaN.
fn build_find_helper(name: &str, t: z::Elem, includes: bool) -> z::Func {
    use z::{Param, Type};
    let (matched, sentinel, ret_ty) = if includes {
        (z::Expr::Bool(true), z::Expr::Bool(false), Type::Bool)
    } else {
        (var("__i"), int(-1), Type::I64)
    };
    z::Func {
        name: name.into(),
        params: vec![
            Param { name: "arr".into(), ty: Type::Array(t) },
            Param { name: "x".into(), ty: t.to_type() },
        ],
        ret: ret_ty,
        body: vec![
            for_i(vec![if_then(elem_eq(t, includes), vec![ret(matched)])]),
            ret(sentinel),
        ],
    }
}

/// A callback array helper that scans with `pred` and returns on the first match:
/// `some` (→ true/false), `every` (→ false on first !pred / true), `findIndex`
/// (→ index / -1).
fn build_predicate_helper(name: &str, t: z::Elem, f_ty: z::Type, kind: &str) -> z::Func {
    use z::{Param, Type};
    let test = call_f(vec![arr_at_i()]);
    let (loop_body, tail, ret_ty) = match kind {
        "some" => (
            vec![if_then(test, vec![ret(z::Expr::Bool(true))])],
            ret(z::Expr::Bool(false)),
            Type::Bool,
        ),
        "every" => (
            vec![if_then(
                z::Expr::Unary { op: z::UnOp::Not, e: Box::new(test) },
                vec![ret(z::Expr::Bool(false))],
            )],
            ret(z::Expr::Bool(true)),
            Type::Bool,
        ),
        "findIndex" => (
            vec![if_then(test, vec![ret(var("__i"))])],
            ret(int(-1)),
            Type::I64,
        ),
        _ => unreachable!(),
    };
    z::Func {
        name: name.into(),
        params: vec![
            Param { name: "arr".into(), ty: Type::Array(t) },
            Param { name: "f".into(), ty: f_ty },
        ],
        ret: ret_ty,
        body: vec![for_i(loop_body), tail],
    }
}

/// `function __reverse_T(arr: T[]): T[]` — in-place two-pointer swap, returns the
/// (aliased) receiver.
fn build_reverse_helper(name: &str, t: z::Elem) -> z::Func {
    use z::{BinOp, Param, Type};
    z::Func {
        name: name.into(),
        params: vec![Param { name: "arr".into(), ty: Type::Array(t) }],
        ret: Type::Array(t),
        body: vec![
            let_("__i", int(0)),
            let_("__j", bin(BinOp::Sub, len_of("arr"), int(1))),
            while_(
                bin(BinOp::Lt, var("__i"), var("__j")),
                vec![
                    let_("__t", idx(var("arr"), var("__i"))),
                    assign(idx(var("arr"), var("__i")), idx(var("arr"), var("__j"))),
                    assign(idx(var("arr"), var("__j")), var("__t")),
                    inc("__i"),
                    assign(var("__j"), bin(BinOp::Sub, var("__j"), int(1))),
                ],
            ),
            ret(var("arr")),
        ],
    }
}

/// `function __fillN_T(arr: T[], value: T[, start[, end]]): T[]` — in-place fill
/// of `[start, end)` (TS clamping), returns the receiver. Native (no push).
fn build_fill_helper(name: &str, t: z::Elem, nargs: usize) -> z::Func {
    use z::{BinOp, Param, Type};
    let mut params = vec![
        Param { name: "arr".into(), ty: Type::Array(t) },
        Param { name: "value".into(), ty: t.to_type() },
    ];
    if nargs >= 2 {
        params.push(Param { name: "start".into(), ty: Type::I64 });
    }
    if nargs >= 3 {
        params.push(Param { name: "end".into(), ty: Type::I64 });
    }
    let s = if nargs >= 2 { norm_idx(var("start")) } else { int(0) };
    let e = if nargs >= 3 { norm_idx(var("end")) } else { var("__n") };
    z::Func {
        name: name.into(),
        params,
        ret: Type::Array(t),
        body: vec![
            let_("__n", len_of("arr")),
            let_("__i", s),
            let_("__e", e),
            while_(
                bin(BinOp::Lt, var("__i"), var("__e")),
                vec![assign(idx(var("arr"), var("__i")), var("value")), inc("__i")],
            ),
            ret(var("arr")),
        ],
    }
}

/// `function __sliceN_T(arr: T[][, start[, end]]): T[]` — a fresh copy of
/// `[start, end)` with TS clamping. Interpreter-tier (builds via push).
fn build_slice_helper(name: &str, t: z::Elem, nargs: usize) -> z::Func {
    use z::{BinOp, Expr, Param, Type};
    let mut params = vec![Param { name: "arr".into(), ty: Type::Array(t) }];
    if nargs >= 1 {
        params.push(Param { name: "start".into(), ty: Type::I64 });
    }
    if nargs >= 2 {
        params.push(Param { name: "end".into(), ty: Type::I64 });
    }
    let s = if nargs >= 1 { norm_idx(var("start")) } else { int(0) };
    let e = if nargs >= 2 { norm_idx(var("end")) } else { var("__n") };
    z::Func {
        name: name.into(),
        params,
        ret: Type::Array(t),
        body: vec![
            let_("__n", len_of("arr")),
            let_("__i", s),
            let_("__e", e),
            stmt(z::StmtKind::Let {
                name: "__r".into(),
                ty: Some(Type::Array(t)),
                value: Expr::Repeat { value: Box::new(elem_default(t)), count: Box::new(int(0)) },
            }),
            while_(
                bin(BinOp::Lt, var("__i"), var("__e")),
                vec![push_stmt("__r", idx(var("arr"), var("__i"))), inc("__i")],
            ),
            ret(var("__r")),
        ],
    }
}

/// `function __concat_T(arr: T[], other: T[]): T[]` — fresh array of both, in
/// order. Interpreter-tier (push).
fn build_concat_helper(name: &str, t: z::Elem) -> z::Func {
    use z::{BinOp, Expr, Param, Type};
    z::Func {
        name: name.into(),
        params: vec![
            Param { name: "arr".into(), ty: Type::Array(t) },
            Param { name: "other".into(), ty: Type::Array(t) },
        ],
        ret: Type::Array(t),
        body: vec![
            stmt(z::StmtKind::Let {
                name: "__r".into(),
                ty: Some(Type::Array(t)),
                value: Expr::Repeat { value: Box::new(elem_default(t)), count: Box::new(int(0)) },
            }),
            let_("__i", int(0)),
            while_(
                bin(BinOp::Lt, var("__i"), len_of("arr")),
                vec![push_stmt("__r", idx(var("arr"), var("__i"))), inc("__i")],
            ),
            let_("__j", int(0)),
            while_(
                bin(BinOp::Lt, var("__j"), len_of("other")),
                vec![push_stmt("__r", idx(var("other"), var("__j"))), inc("__j")],
            ),
            ret(var("__r")),
        ],
    }
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
        z::Type::OptStr => "optstr".into(),
        z::Type::OptArr(_) => "optarr".into(),
        z::Type::OptI64 => "opti64".into(),
        z::Type::OptF64 => "optf64".into(),
        z::Type::OptBool => "optbool".into(),
        z::Type::Null => "null".into(),
        z::Type::Func(id) => format!("fn{id}"),
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
    fn first_class_functions() {
        // named function as a value, arrow lambda, function-typed parameter,
        // and indirect calls — all on the interpreter tier.
        let ts = "function applyTwice(f: (n: i64) => i64, x: i64): i64 { return f(f(x)); } \
                  function inc(n: i64): i64 { return n + 1; } \
                  function main(): i64 { \
                    const g = inc; \
                    const double = (n: i64) => n * 2; \
                    return applyTwice(double, 5) + applyTwice(g, 10) + applyTwice(inc, 100); }";
        // double(double(5))=20 ; inc(inc(10))=12 ; inc(inc(100))=102 -> 134
        assert_eq!(run_i64(ts), 134);

        // the program is flagged interpreter-only (native tiers fall back)
        let m = compile_ts(ts).expect("lower");
        let prog = zippc::compile_module(&m).expect("compile");
        assert!(prog.uses_func_value);

        // capturing an enclosing local works now (covered in depth by
        // `closures_capture`).
        let cap = "function main(): i64 { let k = 3; const f = (n: i64) => n + k; return f(1); }";
        assert_eq!(run_i64(cap), 4);
    }

    #[test]
    fn growable_arrays_and_methods() {
        // push / pop / len
        let ts = "function main(): i64 { const xs: i64[] = []; \
                    xs.push(10); xs.push(20); xs.push(30); \
                    let last = xs.pop()!; return len(xs) * 100 + last; }";
        assert_eq!(run_i64(ts), 230); // len 2, last 30

        // map (arrow) + filter (named predicate) + reduce
        let ts2 = "function odd(n: i64): bool { return n % 2 === 1; } \
                   function main(): i64 { \
                     const xs: i64[] = []; for (let i = 1; i <= 4; i = i + 1) { xs.push(i); } \
                     const sq = xs.map((x: i64) => x * x); \
                     const odds = xs.filter(odd); \
                     const sum = sq.reduce((a: i64, b: i64) => a + b, 0); \
                     return sum * 10 + len(odds); }";
        // xs=[1,2,3,4]; sq=[1,4,9,16] → sum 30; odds=[1,3] → len 2  ⇒ 302
        assert_eq!(run_i64(ts2), 302);

        // reduce with a capturing closure
        let ts3 = "function main(): i64 { const xs: i64[] = []; \
                     xs.push(1); xs.push(2); xs.push(3); \
                     const k = 10; return xs.reduce((a: i64, x: i64) => a + x * k, 0); }";
        assert_eq!(run_i64(ts3), 60); // (1+2+3)*10

        // growable arrays flag the program interpreter-only
        let m = compile_ts(ts).expect("lower");
        let prog = zippc::compile_module(&m).expect("compile");
        assert!(prog.uses_growable);
    }

    #[test]
    fn calculator_demo() {
        // A compact recursive-descent calculator (the examples/calculator.ts shape)
        // — exercises classes/this, recursion, charCodeAt, and precedence/parens
        // end to end.
        let prelude = "class P { s: str; pos: i64; \
            constructor(s: str) { this.s = s; this.pos = 0; } \
            peek(): i64 { return this.pos < len(this.s) ? this.s.charCodeAt(this.pos) : -1; } \
            sp(): i64 { while (this.peek() === 32) { this.pos = this.pos + 1; } return this.pos; } \
            num(): i64 { let n = 0; while (true) { const c = this.peek(); \
              if (c < 48 || c > 57) { break; } n = n * 10 + (c - 48); this.pos = this.pos + 1; } return n; } \
            fac(): i64 { this.sp(); const c = this.peek(); \
              if (c === 40) { this.pos = this.pos + 1; const v = this.exp(); this.sp(); this.pos = this.pos + 1; return v; } \
              if (c === 45) { this.pos = this.pos + 1; return -this.fac(); } return this.num(); } \
            term(): i64 { let v = this.fac(); while (true) { this.sp(); const c = this.peek(); \
              if (c === 42) { this.pos = this.pos + 1; v = v * this.fac(); } \
              else if (c === 47) { this.pos = this.pos + 1; v = v / this.fac(); } else { break; } } return v; } \
            exp(): i64 { let v = this.term(); while (true) { this.sp(); const c = this.peek(); \
              if (c === 43) { this.pos = this.pos + 1; v = v + this.term(); } \
              else if (c === 45) { this.pos = this.pos + 1; v = v - this.term(); } else { break; } } return v; } } \
            function ev(s: str): i64 { const p = new P(s); return p.exp(); } ";
        let prog = |e: &str| format!("{prelude} function main(): i64 {{ return ev(\"{e}\"); }}");
        assert_eq!(run_i64(&prog("1 + 2 * 3")), 7); // precedence
        assert_eq!(run_i64(&prog("(1 + 2) * 3")), 9); // parens
        assert_eq!(run_i64(&prog("10 - 2 - 3")), 5); // left assoc
        assert_eq!(run_i64(&prog("100 / 7")), 14); // integer division
        assert_eq!(run_i64(&prog("-(3 + 4) + 10")), 3); // unary minus
        assert_eq!(run_i64(&prog("2 * -3 + 8")), 2);
        assert_eq!(run_i64(&prog("(2 + 3) * (4 + 5)")), 45);
    }

    #[test]
    fn string_methods() {
        // charCodeAt + out-of-range sentinel (-1, TS-divergent from NaN)
        assert_eq!(run_i64("function main(): i64 { return \"ABC\".charCodeAt(2); }"), 67);
        assert_eq!(run_i64("function main(): i64 { return \"ABC\".charCodeAt(3); }"), -1);
        assert_eq!(run_i64("function main(): i64 { return \"ABC\".charCodeAt(-1); }"), -1);

        // slice: positive / negative / clamp-empty / omitted-end length
        assert_eq!(run_i64("function main(): i64 { return \"hello\".slice(1, 3) === \"el\" ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { return \"hello\".slice(-1) === \"o\" ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { return \"hello\".slice(1, -1) === \"ell\" ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { return \"hello\".slice(3, 1) === \"\" ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { return len(\"hello\".slice(1)); }"), 4);

        // indexOf / lastIndexOf (empty-needle identities)
        assert_eq!(run_i64("function main(): i64 { return \"abab\".indexOf(\"ab\"); }"), 0);
        assert_eq!(run_i64("function main(): i64 { return \"abc\".indexOf(\"d\"); }"), -1);
        assert_eq!(run_i64("function main(): i64 { return \"abc\".indexOf(\"\"); }"), 0);
        assert_eq!(run_i64("function main(): i64 { return \"abab\".lastIndexOf(\"ab\"); }"), 2);

        // includes / startsWith / endsWith
        assert_eq!(run_i64("function main(): i64 { return \"abc\".includes(\"b\") ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { return \"abc\".includes(\"d\") ? 1 : 0; }"), 0);
        assert_eq!(run_i64("function main(): i64 { return \"abc\".startsWith(\"ab\") ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { return \"abc\".startsWith(\"b\") ? 1 : 0; }"), 0);
        assert_eq!(run_i64("function main(): i64 { return \"abc\".endsWith(\"bc\") ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { return \"abc\".endsWith(\"b\") ? 1 : 0; }"), 0);
        // HEADLINE: includes("b") is true but startsWith("b") is false
        assert_eq!(run_i64("function main(): i64 { return (\"abc\".includes(\"b\") && !\"abc\".startsWith(\"b\")) ? 1 : 0; }"), 1);

        // charAt: in-range char / out-of-range empty (contrast charCodeAt's -1)
        assert_eq!(run_i64("function main(): i64 { return \"abc\".charAt(0) === \"a\" ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { return \"abc\".charAt(3) === \"\" ? 1 : 0; }"), 1);

        // repeat
        assert_eq!(run_i64("function main(): i64 { return \"ab\".repeat(3) === \"ababab\" ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { return \"x\".repeat(0) === \"\" ? 1 : 0; }"), 1);

        // chaining + derivation cross-check (includes === indexOf >= 0)
        assert_eq!(run_i64("function main(): i64 { return \"hello\".slice(1).indexOf(\"l\"); }"), 1);

        // string methods stay NATIVE (no closure, no push, no nullable scalar)
        let m = compile_ts("function main(): i64 { return \"hello\".slice(1, 3).charCodeAt(0); }")
            .expect("lower");
        let prog = zippc::compile_module(&m).expect("compile");
        assert!(!prog.uses_growable && !prog.uses_func_value && !prog.uses_opt_scalar);
    }

    #[test]
    fn array_methods_extended() {
        // indexOf: first match / not found / final index
        assert_eq!(run_i64("function main(): i64 { const xs = [3,1,2,1]; return xs.indexOf(1); }"), 1);
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; return xs.indexOf(9); }"), -1);
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; return xs.indexOf(3); }"), 2);

        // includes
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; return xs.includes(2) ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; return xs.includes(9) ? 1 : 0; }"), 0);

        // NaN: SameValueZero in includes matches; strict-eq in indexOf does not
        let nan = "function nan(): f64 { return 0.0 / 0.0; } \
                   function main(): i64 { const xs: f64[] = [nan()]; \
                     const inc = xs.includes(nan()) ? 1 : 0; \
                     const idx = xs.indexOf(nan()); \
                     return inc * 100 + (idx + 1); }"; // includes→1, indexOf→-1 ⇒ 100
        assert_eq!(run_i64(nan), 100);

        // some / every empty identities
        assert_eq!(run_i64("function main(): i64 { const xs: i64[] = []; return xs.some((x: i64) => x > 0) ? 1 : 0; }"), 0);
        assert_eq!(run_i64("function main(): i64 { const xs: i64[] = []; return xs.every((x: i64) => x > 0) ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { const xs = [2,4,6]; return xs.every((x: i64) => x % 2 === 0) ? 1 : 0; }"), 1);
        assert_eq!(run_i64("function main(): i64 { const xs = [2,4,5]; return xs.every((x: i64) => x % 2 === 0) ? 1 : 0; }"), 0);

        // some short-circuits: a capturing closure records each visited element
        let sc = "function main(): i64 { const xs = [1,2,3,4]; const seen: i64[] = []; \
                    const found = xs.some((x: i64) => seen.push(x) > 0 && x === 2); \
                    return (found ? 1 : 0) * 100 + len(seen); }"; // stops at 2 ⇒ seen=[1,2] ⇒ 102
        assert_eq!(run_i64(sc), 102);

        // findIndex
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; return xs.findIndex((x: i64) => x > 1); }"), 1);
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; return xs.findIndex((x: i64) => x > 9); }"), -1);

        // reverse: in-place; involution
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; xs.reverse(); return xs[0] * 100 + xs[2]; }"), 301);
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; xs.reverse().reverse(); return xs[0]; }"), 1);

        // fill: range [start,end), in place
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3,4]; xs.fill(9,1,3); return xs[0]*1000+xs[1]*100+xs[2]*10+xs[3]; }"), 1994);

        // slice: to-end, negative, and COPY independence (fill on the copy)
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3,4]; const ys = xs.slice(2); return len(ys)*10 + ys[0]; }"), 23);
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3,4]; const ys = xs.slice(1,-1); return len(ys)*100 + ys[0]*10 + ys[1]; }"), 223);
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; const ys = xs.slice(0); ys.fill(0); return xs[0]+xs[1]+xs[2]; }"), 6);

        // concat: order + length
        assert_eq!(run_i64("function main(): i64 { const a = [1,2]; const b = [3,4]; const c = a.concat(b); return len(c)*1000 + c[0]*100 + c[2]*10 + c[3]; }"), 4134);

        // chaining across helper results
        assert_eq!(run_i64("function main(): i64 { return [1,2,3,4].filter((x: i64) => x % 2 === 0).map((x: i64) => x * 10).indexOf(40); }"), 1);

        // two DIFFERENT callbacks of the same type reuse the one helper
        // (structural func-type interning keeps it polymorphic, not baked to one fn)
        assert_eq!(run_i64("function main(): i64 { const xs = [1,2,3]; \
                    const a = xs.map((x: i64) => x + 1); const b = xs.map((x: i64) => x * 10); \
                    return a[2] * 100 + b[2]; }"), 430); // a=[2,3,4], b=[10,20,30]

        // indexOf / includes / reverse / fill use neither push nor a closure, so a
        // program built only from them stays native-eligible (no fallback).
        let native = "function main(): i64 { const xs = [3,1,2]; xs.reverse(); xs.fill(7,0,1); \
                      return xs.indexOf(7) + (xs.includes(3) ? 1 : 0); }";
        let m = compile_ts(native).expect("lower");
        let prog = zippc::compile_module(&m).expect("compile");
        assert!(
            !prog.uses_growable && !prog.uses_func_value,
            "indexOf/includes/reverse/fill must stay native-eligible"
        );
    }

    #[test]
    fn closures_capture() {
        // capture a parameter: adder(n) returns a closure remembering n
        let ts = "function adder(n: i64): (x: i64) => i64 { return (x: i64) => x + n; } \
                  function main(): i64 { const a = adder(10); const b = adder(100); \
                    return a(5) + b(5); }";
        assert_eq!(run_i64(ts), 120); // 15 + 105

        // capture a local
        let ts2 = "function main(): i64 { let base = 7; const f = (x: i64) => x * base; \
                    return f(6); }";
        assert_eq!(run_i64(ts2), 42);

        // nested / curried closures: transitive capture, called inline
        let ts3 = "function add3(a: i64): (b: i64) => (c: i64) => i64 { \
                     return (b: i64) => (c: i64) => a + b + c; } \
                   function main(): i64 { return add3(1)(2)(3); }";
        assert_eq!(run_i64(ts3), 6);

        // capture-by-value: reassigning after creation doesn't change the closure
        let ts4 = "function main(): i64 { let k = 1; const f = (x: i64) => x + k; \
                    k = 100; return f(0); }";
        assert_eq!(run_i64(ts4), 1); // snapshot k = 1, not 100
    }

    #[test]
    fn optional_method_calls() {
        // `a?.m() ?? default` — a scalar-returning method, fused
        let ts = "class Account { \
                    balance: i64; \
                    constructor(b: i64) { this.balance = b; } \
                    get(): i64 { return this.balance; } \
                  } \
                  function bal(a: Account | null): i64 { return a?.get() ?? -1; } \
                  function main(): i64 { let x = new Account(100); return bal(x) + bal(null); }";
        assert_eq!(run_i64(ts), 99); // 100 + (-1)
        // `a?.m()` returning a nullable struct → chain/narrow the result
        let ts2 = "class Node { \
                     v: i64; nxt: Node | null; \
                     constructor(v: i64, nxt: Node | null) { this.v = v; this.nxt = nxt; } \
                     next(): Node | null { return this.nxt; } \
                   } \
                   function main(): i64 { \
                     let tail = new Node(2, null); \
                     let head = new Node(1, tail); \
                     let h: Node | null = head; \
                     let n = h?.next(); \
                     return n?.v ?? -1; }";
        assert_eq!(run_i64(ts2), 2); // head.next() = tail, tail.v = 2
    }

    #[test]
    fn string_switch_and_early_narrowing() {
        // switch on a string discriminant (+ empty-case stacking)
        let ts = "function code(s: str): i64 { \
                    switch (s) { \
                      case \"red\": return 1; \
                      case \"green\": case \"lime\": return 2; \
                      default: return 0; \
                    } \
                  } \
                  function main(): i64 { return code(\"red\") + code(\"lime\") + code(\"blue\"); }";
        assert_eq!(run_i64(ts), 3); // 1 + 2 + 0
        // early-return narrowing: `if (x === null) return …;` then use x non-null
        let ts2 = "interface User { age: i64; } \
                   function ageOf(u: User | null): i64 { \
                     if (u === null) { return -1; } \
                     return u.age; \
                   } \
                   function main(): i64 { let a: User = { age: 40 }; return ageOf(a) + ageOf(null); }";
        assert_eq!(run_i64(ts2), 39); // 40 + (-1)
        // …also for a nullable scalar
        let ts3 = "function f(n: i64 | null): i64 { if (n === null) { return 0; } return n + 1; } \
                   function main(): i64 { return f(10) + f(null); }";
        assert_eq!(run_i64(ts3), 11); // 11 + 0
    }

    #[test]
    fn tuples() {
        // tuple return + destructuring (the multiple-return-values pattern)
        let ts = "function divmod(a: i64, b: i64): [i64, i64] { return [a / b, a % b]; } \
                  function main(): i64 { let [q, r] = divmod(17, 5); return q * 100 + r; }";
        assert_eq!(run_i64(ts), 302); // 3*100 + 2
        // heterogeneous tuple local + positional indexing
        let ts2 = "function main(): i64 { \
                     let t: [i64, str] = [42, \"hi\"]; \
                     return t[0] + len(t[1]); }";
        assert_eq!(run_i64(ts2), 44); // 42 + len("hi")
        // tuple as a parameter
        let ts3 = "function fst(p: [i64, bool]): i64 { return p[0]; } \
                   function main(): i64 { let p: [i64, bool] = [7, true]; return fst(p); }";
        assert_eq!(run_i64(ts3), 7);
    }

    #[test]
    fn nullable_str_and_array() {
        // str | null: `?? default`
        let ts = "function greet(name: str | null): str { return \"hi \" + (name ?? \"guest\"); } \
                  function main(): i64 { \
                    let a: str | null = \"Ada\"; \
                    let n: str | null = null; \
                    return len(greet(a)) + len(greet(n)); }";
        assert_eq!(run_i64(ts), 14); // "hi Ada"(6) + "hi guest"(8)
        // T[] | null: narrow then index
        let ts2 = "function sumOr(xs: i64[] | null): i64 { \
                     if (xs !== null) { return xs[0] + xs[1]; } \
                     return -1; \
                   } \
                   function main(): i64 { \
                     let some: i64[] | null = [10, 20]; \
                     let none: i64[] | null = null; \
                     return sumOr(some) + sumOr(none); }";
        assert_eq!(run_i64(ts2), 29); // 30 + (-1)
        // a nullable str struct field, read with `?? default`
        let ts3 = "interface User { nick: str | null; } \
                   function main(): i64 { let u: User = { nick: null }; return len(u.nick ?? \"anon\"); }";
        assert_eq!(run_i64(ts3), 4); // "anon"
    }

    #[test]
    fn nullable_scalars() {
        // nullable i64 param + narrowing + `?? default`
        let ts = "function orZero(n: i64 | null): i64 { \
                    if (n !== null) { return n; } \
                    return 0; \
                  } \
                  function main(): i64 { \
                    let some: i64 | null = 42; \
                    let none: i64 | null = null; \
                    return orZero(some) + orZero(none) + (none ?? 7); }";
        assert_eq!(run_i64(ts), 49); // 42 + 0 + 7
        // a nullable scalar struct field, read with `?? default`
        let ts2 = "interface Cfg { timeout: i64 | null; } \
                   function main(): i64 { \
                     let a: Cfg = { timeout: 30 }; \
                     let b: Cfg = { timeout: null }; \
                     return (a.timeout ?? 10) + (b.timeout ?? 10); }";
        assert_eq!(run_i64(ts2), 40); // 30 + 10
        // bool | null
        let ts3 = "function pick(flag: bool | null): i64 { return (flag ?? false) ? 1 : 0; } \
                   function main(): i64 { return pick(true) + pick(null); }";
        assert_eq!(run_i64(ts3), 1); // 1 + 0
    }

    #[test]
    fn optional_chaining() {
        // `a?.b ?? default` on a scalar field (the fused form)
        let ts = "interface User { age: i64; } \
                  function ageOf(u: User | null): i64 { return u?.age ?? -1; } \
                  function main(): i64 { \
                    let a: User = { age: 30 }; \
                    let some: User | null = a; \
                    let none: User | null = null; \
                    return ageOf(some) + ageOf(none); }";
        assert_eq!(run_i64(ts), 29); // 30 + (-1)
        // chained `?.` through a nullable struct field, ending in `?? default`
        let ts2 = "interface Addr { zip: i64; } \
                   interface User { addr: Addr | null; } \
                   function zipOf(u: User | null): i64 { return u?.addr?.zip ?? 0; } \
                   function main(): i64 { \
                     let home: Addr = { zip: 90210 }; \
                     let withAddr: User = { addr: home }; \
                     let noAddr: User = { addr: null }; \
                     let w: User | null = withAddr; \
                     return zipOf(w) + zipOf(noAddr) + zipOf(null); }";
        assert_eq!(run_i64(ts2), 90210); // 90210 + 0 + 0
        // bare `a?.b` on a struct field yields a nullable, then narrowed
        let ts3 = "interface Inner { v: i64; } \
                   interface Outer { inner: Inner | null; } \
                   function main(): i64 { \
                     let i: Inner = { v: 7 }; \
                     let o: Outer = { inner: i }; \
                     let oo: Outer | null = o; \
                     let r: Inner | null = oo?.inner; \
                     if (r !== null) { return r.v; } \
                     return -1; }";
        assert_eq!(run_i64(ts3), 7);
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
