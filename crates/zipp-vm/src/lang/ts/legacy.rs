//! `experimentalDecorators`: TypeScript's legacy decorators, lowered to the
//! code tsc emits for them.
//!
//! With `experimentalDecorators` on in the file's `tsconfig.json` a decorator
//! is a function of the decorated thing, called once the class is defined. A
//! member's decorators are `__decorate([d], C.prototype, "m", null)` after
//! the class (`void 0` for a property's descriptor, the class itself as the
//! target of a static member), a parameter's `@p x` is `__param(i, p)` among
//! its method's, and a constructor's join the class's own, which run last,
//! their result replacing the class: `let C = class C { … }; …; C =
//! __decorate([d], C);`. References to the class inside its body read an
//! alias set first thing in the class and again with the decorated class
//! (`static { C_1 = this; }`, `C = C_1 = __decorate(…)`), as tsc's do. A
//! class with only member decorators stays a declaration.
//!
//! With `emitDecoratorMetadata`, `__metadata("design:type", T)`,
//! `"design:paramtypes"` and `"design:returntype"` entries follow each
//! decorated member's, and `"design:paramtypes"` the class's when it has a
//! constructor: the types serialized as tsc serializes them ([`classify`]),
//! a name the file declares resolved against its declaration so far, any
//! other in tsc's guarded form (`typeof T === "function" ? T : Object`).
//!
//! The helpers are tsc's (`__decorate` defers to `Reflect.decorate`, and
//! `__metadata` to `Reflect.metadata`, when a polyfill installs them),
//! declared once at the top of the file under names a program cannot spell.

use super::lower::{ident, member, str_expr, var_stmt};
use crate::parse::ast::*;
use crate::parse::parser::{PResult, ParseOptions, Parser};
use crate::parse::token::{Span, StrVal};

/// The helper names, which no source text can spell.
const DECORATE: &str = "%decorate";
const PARAM: &str = "%param";
const METADATA: &str = "%metadata";

/// A class declaration being read under `experimentalDecorators`, one per
/// nesting level (`TsState::legacy_classes`).
#[derive(Default)]
pub(crate) struct LegacyClass {
    /// The class's name, when its own decorators rename references to it.
    name: Option<Name>,
    /// What those references read.
    alias: Option<Name>,
    /// The scope depth of the class body: a binding of the name in a scope
    /// deeper than this shadows the class.
    depth: usize,
    /// A reference was renamed: the alias is declared and set.
    alias_used: bool,
    /// The constructor's parameter decorators and, with metadata, its
    /// parameter types.
    pub(crate) ctor_params: Vec<(u32, Vec<Expr>)>,
    pub(crate) ctor_types: Option<Vec<Expr>>,
    /// Each decorated member: static, key, has a descriptor (a method or an
    /// accessor), its decorators.
    members: Vec<(bool, Expr, bool, Vec<Expr>)>,
    /// The temporaries holding computed member names.
    temps: Vec<Name>,
}

impl LegacyClass {
    fn decorated(&self) -> bool {
        !self.members.is_empty() || !self.ctor_params.is_empty()
    }
}

/// How a lowered class is exported.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyExport {
    None,
    Named,
    Default,
}

/// A type as `emitDecoratorMetadata` serializes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ser {
    /// A global constructor: `String`, `Number`, `Object`, `Array`, …
    Ctor(&'static str),
    /// `void 0`: `void`, `undefined`, `null`, `never`.
    Void,
    /// A name the type refers to (`Foo`, `ns.Foo`), resolved when the
    /// metadata is built.
    Ref(Vec<String>),
}

fn call(callee: Expr, args: Vec<Expr>) -> Expr {
    Expr::Call(Box::new(CallExpr {
        callee,
        args: args.into_iter().map(Arg::Expr).collect(),
        optional: false,
        pos: 0,
    }))
}

fn array(items: Vec<Expr>) -> Expr {
    Expr::Array(
        items
            .into_iter()
            .map(|e| Some(ArrayElem::Expr(e)))
            .collect(),
        LiteralFlags::default(),
    )
}

fn void0() -> Expr {
    Expr::Unary {
        op: UnaryOp::Void,
        arg: Box::new(Expr::Num(0.0)),
    }
}

fn assign_name(name: &Name, value: Expr) -> Expr {
    Expr::Assign {
        op: AssignOp::Assign,
        target: Target::Ident {
            name: name.clone(),
            covered: false,
        },
        value: Box::new(value),
        covered: false,
    }
}

fn helper(name: &str) -> Expr {
    Expr::Ident(name.into())
}

impl<'s> Parser<'s> {
    /// A class declaration's body is about to be read: `decorated` when the
    /// class has decorators of its own, whose references to its name inside
    /// the body are then renamed.
    pub(crate) fn ts_legacy_enter(&mut self, name: Option<&Name>, decorated: bool) {
        let mut c = LegacyClass {
            depth: self.scopes.depth(),
            ..LegacyClass::default()
        };
        if decorated {
            if let Some(n) = name {
                c.name = Some(n.clone());
                c.alias = Some(format!("%{n}_1").into());
            }
        }
        self.ts_state.legacy_classes.push(c);
    }

    /// The class body just read: its record, for the declaration to lower.
    pub(crate) fn ts_legacy_exit(&mut self) {
        self.ts_state.legacy_last = self.ts_state.legacy_classes.pop();
    }

    /// An identifier reference to `name`: the alias it reads instead, when
    /// it names an enclosing decorated class that no nearer binding shadows.
    pub(crate) fn ts_class_alias(&mut self, name: &Name) -> Option<Name> {
        for c in self.ts_state.legacy_classes.iter_mut().rev() {
            if c.name.as_deref() == Some(&**name) {
                if self.scopes.binds_from(c.depth, name) {
                    return None;
                }
                c.alias_used = true;
                return c.alias.clone();
            }
        }
        None
    }

    /// A class member just read, with its decorators: a method's parameter
    /// decorators join them (`__param(i, d)`), or the class's for its
    /// constructor; with metadata, the member's types follow.
    pub(crate) fn ts_legacy_member(&mut self, m: &ClassMember, decorators: &mut Vec<Expr>) {
        let params = std::mem::take(&mut self.ts_state.fn_param_decs);
        let sig = std::mem::take(&mut self.ts_state.fn_sig);
        let field_type = std::mem::take(&mut self.ts_state.field_type);
        let metadata = self.ts_state.decorator_metadata;
        match m {
            ClassMember::Method(cm) if cm.kind == MethodKind::Constructor => {
                let types =
                    sig.map(|(p, _)| p.into_iter().map(|t| self.ts_ser_expr(&t, 0)).collect());
                if let Some(c) = self.ts_state.legacy_classes.last_mut() {
                    c.ctor_params = params;
                    c.ctor_types = types;
                }
            }
            ClassMember::Method(cm) => {
                if decorators.is_empty() && params.is_empty() {
                    return;
                }
                for (i, ds) in params {
                    for d in ds {
                        decorators.push(call(helper(PARAM), vec![Expr::Num(i as f64), d]));
                    }
                }
                if !metadata {
                    return;
                }
                let (ptypes, ret) = sig.unwrap_or_default();
                let meta = |k: &str, v: Expr| call(helper(METADATA), vec![str_expr(k), v]);
                match cm.kind {
                    MethodKind::Get => {
                        let t =
                            ret.map_or(Expr::Ident("Object".into()), |r| self.ts_ser_expr(&r, 0));
                        decorators.push(meta("design:type", t));
                        decorators.push(meta("design:paramtypes", array(Vec::new())));
                    }
                    MethodKind::Set => {
                        let p: Vec<Expr> = ptypes.iter().map(|t| self.ts_ser_expr(t, 0)).collect();
                        let t = p.first().cloned().unwrap_or(Expr::Ident("Object".into()));
                        decorators.push(meta("design:type", t));
                        decorators.push(meta("design:paramtypes", array(p)));
                    }
                    _ => {
                        let p = ptypes.iter().map(|t| self.ts_ser_expr(t, 0)).collect();
                        let r = ret.map_or_else(void0, |r| self.ts_ser_expr(&r, 0));
                        decorators.push(meta("design:type", Expr::Ident("Function".into())));
                        decorators.push(meta("design:paramtypes", array(p)));
                        decorators.push(meta("design:returntype", r));
                    }
                }
            }
            ClassMember::Field(cf) => {
                if decorators.is_empty() || !metadata {
                    return;
                }
                let t = match (&cf.accessor, field_type) {
                    (_, Some(t)) => self.ts_ser_expr(&t, 0),
                    (_, None) => Expr::Ident("Object".into()),
                };
                decorators.push(call(helper(METADATA), vec![str_expr("design:type"), t]));
            }
            ClassMember::StaticBlock(_) => {}
        }
    }

    /// The class body is read: move each member's decorators to the class's
    /// record (instance members first, then static ones, each in source
    /// order, as tsc decorates them), a computed name into a temporary.
    pub(crate) fn ts_legacy_strip(&mut self, body: &mut [ClassMember]) -> PResult<()> {
        let mut members = Vec::new();
        let mut temps = Vec::new();
        for m in body.iter_mut() {
            let (key, is_static, desc, decs) = match m {
                ClassMember::Method(cm) => (&mut cm.key, cm.is_static, true, &mut cm.decorators),
                ClassMember::Field(cf) => {
                    let desc = cf.accessor.is_some();
                    (&mut cf.key, cf.is_static, desc, &mut cf.decorators)
                }
                ClassMember::StaticBlock(_) => continue,
            };
            if decs.is_empty() {
                continue;
            }
            let decs = std::mem::take(decs);
            let k = match key {
                PropKey::Ident(n) => str_expr(n),
                PropKey::Str(s) => Expr::Str(s.clone()),
                PropKey::Num(n) => Expr::Num(*n),
                PropKey::Computed(e) => {
                    let t: Name = format!("%key{}", self.ts_state.legacy_temps).into();
                    self.ts_state.legacy_temps += 1;
                    let e = std::mem::replace(e, Expr::Null);
                    *key = PropKey::Computed(assign_name(&t, e));
                    temps.push(t.clone());
                    ident(&t)
                }
                PropKey::Private(_) => {
                    return Err(self.err_here("SyntaxError: Decorators are not valid here"));
                }
            };
            members.push((is_static, k, desc, decs));
        }
        if let Some(c) = self.ts_state.legacy_classes.last_mut() {
            c.members = members;
            c.temps = temps;
        }
        Ok(())
    }

    /// A class expression was read: under `experimentalDecorators` no
    /// decorator may be on it or its members.
    pub(crate) fn ts_legacy_expr_class(&mut self, class: &Class) -> PResult<()> {
        let last = self.ts_state.legacy_last.take();
        if !class.decorators.is_empty() || last.is_some_and(|c| c.decorated()) {
            return Err(self.err_here("SyntaxError: Decorators are not valid here"));
        }
        Ok(())
    }

    /// The statements a class declaration read under
    /// `experimentalDecorators` lowers to, ending with its export when it
    /// has one. A class with no decorators anywhere is the declaration
    /// itself.
    pub(crate) fn ts_legacy_lower(
        &mut self,
        mut class: Class,
        export: LegacyExport,
    ) -> PResult<Vec<Stmt>> {
        let frame = self.ts_state.legacy_last.take().unwrap_or_default();
        let mut class_decs = std::mem::take(&mut class.decorators);
        let whole = !class_decs.is_empty() || !frame.ctor_params.is_empty();
        let mut out = Vec::new();
        if !whole && frame.members.is_empty() {
            out.push(match export {
                LegacyExport::None => Stmt::ClassDecl(Box::new(class)),
                // The `export` around it is its caller's.
                LegacyExport::Named => Stmt::ClassDecl(Box::new(class)),
                LegacyExport::Default => Stmt::Export(Box::new(ExportDecl::Default(
                    ExportDefault::Class(Box::new(class)),
                ))),
            });
            return Ok(out);
        }
        self.ts_state.legacy_used = true;
        // An anonymous default export is tsc's `default_1` (numbered past a
        // name the file already binds).
        let name: Name = match class.name.clone() {
            Some(n) => n,
            None => {
                let mut i = 1;
                while self.scopes.module_binds(&format!("default_{i}")) {
                    i += 1;
                }
                format!("default_{i}").into()
            }
        };
        for t in &frame.temps {
            out.push(var_stmt(VarKind::Let, t, None));
        }
        let alias = frame.alias.clone().filter(|_| frame.alias_used);
        if whole {
            if let Some(a) = &alias {
                out.push(var_stmt(VarKind::Let, a, None));
                class.body.insert(
                    0,
                    ClassMember::StaticBlock(vec![Stmt::Expr(assign_name(a, Expr::This))]),
                );
            }
            out.push(var_stmt(
                VarKind::Let,
                &name,
                Some(Expr::Class(Box::new(class))),
            ));
        } else {
            out.push(Stmt::ClassDecl(Box::new(class)));
        }
        let proto: Name = "prototype".into();
        for pass_static in [false, true] {
            for (is_static, key, desc, decs) in frame.members.iter().filter(|m| m.0 == pass_static)
            {
                let target = if *is_static {
                    ident(&name)
                } else {
                    member(ident(&name), &proto)
                };
                let d = if *desc { Expr::Null } else { void0() };
                out.push(Stmt::Expr(call(
                    helper(DECORATE),
                    vec![array(decs.clone()), target, key.clone(), d],
                )));
            }
        }
        if whole {
            for (i, ds) in frame.ctor_params {
                for d in ds {
                    class_decs.push(call(helper(PARAM), vec![Expr::Num(i as f64), d]));
                }
            }
            if self.ts_state.decorator_metadata {
                if let Some(types) = frame.ctor_types {
                    class_decs.push(call(
                        helper(METADATA),
                        vec![str_expr("design:paramtypes"), array(types)],
                    ));
                }
            }
            let applied = call(helper(DECORATE), vec![array(class_decs), ident(&name)]);
            let value = match &alias {
                Some(a) => assign_name(a, applied),
                None => applied,
            };
            out.push(Stmt::Expr(assign_name(&name, value)));
        }
        match export {
            LegacyExport::None => {}
            LegacyExport::Named => out.push(Stmt::Export(Box::new(ExportDecl::Named {
                specifiers: vec![ExportSpecifier {
                    local: ModuleExportName::Ident(name.clone()),
                    exported: ModuleExportName::Ident(name.clone()),
                }],
                source: None,
                attributes: Vec::new(),
            }))),
            LegacyExport::Default => out.push(Stmt::Export(Box::new(ExportDecl::Default(
                ExportDefault::Expr(ident(&name)),
            )))),
        }
        Ok(out)
    }

    /// A class declaration (`export`ed: `export class …`) under
    /// `experimentalDecorators`: the statements it lowers to, the last
    /// returned for the statement list.
    pub(crate) fn ts_legacy_decl(&mut self, class: Class, exported: bool) -> PResult<Stmt> {
        let export = if exported {
            LegacyExport::Named
        } else {
            LegacyExport::None
        };
        let stmts = self.ts_legacy_lower(class, export)?;
        Ok(self.ts_emit(stmts))
    }

    /// The helpers, at the top of a file that used them.
    pub(crate) fn ts_legacy_helpers(&mut self, body: &mut Vec<Stmt>) {
        if !self.ts_state.legacy_used {
            return;
        }
        if self.goal != Goal::Module {
            self.ts_state.meta_ns.clear();
        }
        const SRC: &str = "\
var __decorate = function (decorators, target, key, desc) {
    var c = arguments.length, r = c < 3 ? target : desc === null ? desc = Object.getOwnPropertyDescriptor(target, key) : desc, d;
    if (typeof Reflect === \"object\" && typeof Reflect.decorate === \"function\") r = Reflect.decorate(decorators, target, key, desc);
    else for (var i = decorators.length - 1; i >= 0; i--) if (d = decorators[i]) r = (c < 3 ? d(r) : c > 3 ? d(target, key, r) : d(target, key)) || r;
    return c > 3 && r && Object.defineProperty(target, key, r), r;
};
var __param = function (paramIndex, decorator) {
    return function (target, key) { decorator(target, key, paramIndex); };
};
var __metadata = function (k, v) {
    if (typeof Reflect === \"object\" && typeof Reflect.metadata === \"function\") return Reflect.metadata(k, v);
};
";
        let Ok(program) = crate::parse::stmt::parse(SRC, ParseOptions::script()) else {
            return;
        };
        // Each helper function takes a span past the end of the file, where
        // no function of the file's starts (a deferred body is found by its
        // span), and whose source text is empty.
        let mut at = self.src_len() + 1;
        let mut place = |f: &mut Function| {
            f.span = Span::new(at, at + 1);
            at += 2;
        };
        let mut helpers = program.body;
        for (source, ns) in std::mem::take(&mut self.ts_state.meta_ns) {
            helpers.push(Stmt::Import(Box::new(ImportDecl {
                specifiers: vec![ImportSpecifier::Namespace(ns)],
                source,
                attributes: Vec::new(),
                phase: ImportPhase::Evaluation,
            })));
        }
        for (s, name) in helpers.iter_mut().zip([DECORATE, PARAM, METADATA]) {
            let Stmt::VarDecl(d) = s else { continue };
            let Some(decl) = d.decls.first_mut() else {
                continue;
            };
            decl.id = Pattern::Ident(name.into());
            if let Some(Expr::Function(f)) = &mut decl.init {
                place(f);
                if let Some(Stmt::Return(Some(Expr::Function(inner)))) = f.body.stmts.first_mut() {
                    place(inner);
                }
            }
        }
        body.splice(0..0, helpers);
    }

    /// The type annotation just read (`start..end` of the source), as
    /// metadata classifies it.
    pub(crate) fn ts_classify(&self, start: u32, end: u32) -> Ser {
        classify(self.src_text(start, end))
    }

    /// `ser` as the expression `emitDecoratorMetadata` writes for it.
    pub(crate) fn ts_ser_expr(&mut self, ser: &Ser, depth: u32) -> Expr {
        match ser {
            Ser::Ctor(c) => Expr::Ident((*c).into()),
            Ser::Void => void0(),
            Ser::Ref(path) => self.ts_ref_expr(path, depth),
        }
    }

    /// A type reference: a local alias's type, a local enum's member type,
    /// anything else in the guarded form. A name with no binding (an
    /// interface, a type-only import, a type parameter, a type declared
    /// later) reads as `Object` there, as tsc writes it. An imported name
    /// reads through a namespace import of its module made for the purpose:
    /// a name the module does not export (an interface) is `Object` rather
    /// than a link error, and the name's own import is elided as it would be.
    fn ts_ref_expr(&mut self, path: &[String], depth: u32) -> Expr {
        if let [single] = path {
            if let Some(&(start, end)) = self.ts_state.type_aliases.get(single.as_str()) {
                if depth < 8 {
                    let ser = classify(self.src_text(start, end));
                    return self.ts_ser_expr(&ser, depth + 1);
                }
                return Expr::Ident("Object".into());
            }
            if let Some(members) = self.ts_state.enums.get(single.as_str()) {
                let nums = members
                    .iter()
                    .all(|(_, c)| matches!(c, super::lower::Const::Num(_)));
                let strs = members
                    .iter()
                    .all(|(_, c)| matches!(c, super::lower::Const::Str(_)));
                let t = if nums {
                    "Number"
                } else if strs {
                    "String"
                } else {
                    "Object"
                };
                return Expr::Ident(t.into());
            }
        }
        let first: Name = path[0].as_str().into();
        let (base, guard_first) = match self.ts_state.import_from.get(&first).cloned() {
            Some((source, imported)) => {
                let ns = self.ts_meta_namespace(&source);
                let prop = Expr::Member(Box::new(Member {
                    object: ident(&ns),
                    prop: MemberProp::Computed(Box::new(str_expr(&imported))),
                    optional: false,
                }));
                (prop, false)
            }
            None => (ident(&first), true),
        };
        // `typeof a !== "undefined" && a.b && typeof a.b.C === "function" ?
        // a.b.C : Object`: a missing link reads as `Object` rather than
        // throwing.
        let typeof_is = |e: Expr, op: BinaryOp, s: &str| Expr::Binary {
            op,
            left: Box::new(Expr::Unary {
                op: UnaryOp::Typeof,
                arg: Box::new(e),
            }),
            right: Box::new(str_expr(s)),
        };
        let mut links = vec![base.clone()];
        let mut full = base;
        for n in &path[1..] {
            full = member(full, &Name::from(n.as_str()));
            links.push(full.clone());
        }
        let mut test = typeof_is(full.clone(), BinaryOp::StrictEq, "function");
        let n = links.len();
        for (i, link) in links.into_iter().take(n - 1).enumerate().rev() {
            let guard = if i == 0 && guard_first {
                typeof_is(link, BinaryOp::StrictNotEq, "undefined")
            } else {
                link
            };
            test = Expr::Logical {
                op: LogicalOp::And,
                left: Box::new(guard),
                right: Box::new(test),
            };
        }
        Expr::Cond {
            test: Box::new(test),
            cons: Box::new(full),
            alt: Box::new(Expr::Ident("Object".into())),
        }
    }

    /// The namespace binding metadata reads the module `source` through,
    /// declared with the helpers.
    fn ts_meta_namespace(&mut self, source: &StrVal) -> Name {
        let key = source.to_lossy_string();
        if let Some((_, ns)) = self
            .ts_state
            .meta_ns
            .iter()
            .find(|(s, _)| s.to_lossy_string() == key)
        {
            return ns.clone();
        }
        let ns: Name = format!("%meta{}", self.ts_state.meta_ns.len()).into();
        self.ts_state.meta_ns.push((source.clone(), ns.clone()));
        ns
    }
}

/// The token kinds the type classifier reads.
#[derive(Clone, Debug, PartialEq)]
enum T<'a> {
    Word(&'a str),
    Str,
    Num,
    BigInt,
    Template,
    P(char),
    Arrow,
}

fn lex(text: &str) -> Vec<T<'_>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c == b'"' || c == b'\'' || c == b'`' {
            let q = c;
            i += 1;
            let mut depth = 0u32;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if q == b'`' && b[i] == b'$' && b.get(i + 1) == Some(&b'{') {
                    depth += 1;
                    i += 2;
                    continue;
                }
                if q == b'`' && depth > 0 && b[i] == b'}' {
                    depth -= 1;
                    i += 1;
                    continue;
                }
                if b[i] == q && depth == 0 {
                    break;
                }
                i += 1;
            }
            i += 1;
            out.push(if q == b'`' { T::Template } else { T::Str });
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
            out.push(if b[i - 1] == b'n' && !text[s..i].starts_with("0x") {
                T::BigInt
            } else {
                T::Num
            });
        } else if c == b'_' || c == b'$' || c.is_ascii_alphabetic() || c >= 0x80 {
            let s = i;
            while i < b.len()
                && (b[i] == b'_' || b[i] == b'$' || b[i].is_ascii_alphanumeric() || b[i] >= 0x80)
            {
                i += 1;
            }
            out.push(T::Word(&text[s..i]));
        } else if c == b'=' && b.get(i + 1) == Some(&b'>') {
            out.push(T::Arrow);
            i += 2;
        } else {
            out.push(T::P(c as char));
            i += 1;
        }
    }
    out
}

/// How `emitDecoratorMetadata` serializes the type written as `text`.
/// Anything this does not recognize is `Object`, tsc's answer for most
/// type forms.
pub(crate) fn classify(text: &str) -> Ser {
    let toks = lex(text);
    let mut r = Reader { t: &toks, i: 0 };
    let s = r.union().unwrap_or(Ser::Ctor("Object"));
    if r.i < toks.len() {
        // A conditional type (`A extends B ? X : Y`) or something unread.
        return Ser::Ctor("Object");
    }
    s
}

/// What a union member serializes to, before the union combines them.
#[derive(Clone, PartialEq)]
enum Part {
    Ser(Ser),
    /// `null` / `undefined` / `never`, which a union skips.
    Skip,
}

struct Reader<'a, 'b> {
    t: &'b [T<'a>],
    i: usize,
}

impl<'a, 'b> Reader<'a, 'b> {
    fn peek(&self) -> Option<&T<'a>> {
        self.t.get(self.i)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&T::P(c)) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    /// Past a bracketed group whose opener was just read.
    fn skip_group(&mut self, open: char, close: char) -> Option<()> {
        let mut depth = 1;
        while let Some(t) = self.peek().cloned() {
            self.i += 1;
            match t {
                T::P(c) if c == open => depth += 1,
                T::P(c) if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(());
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn union(&mut self) -> Option<Ser> {
        self.eat('|');
        let mut parts = vec![self.inter()?];
        while self.eat('|') {
            parts.push(self.inter()?);
        }
        Some(combine(parts))
    }

    fn inter(&mut self) -> Option<Part> {
        self.eat('&');
        let mut parts = vec![self.postfix()?];
        while self.eat('&') {
            parts.push(self.postfix()?);
        }
        if parts.len() == 1 {
            return parts.pop();
        }
        Some(Part::Ser(combine(parts)))
    }

    fn postfix(&mut self) -> Option<Part> {
        let mut p = self.primary()?;
        while self.peek() == Some(&T::P('[')) {
            self.i += 1;
            if self.eat(']') {
                p = Part::Ser(Ser::Ctor("Array"));
            } else {
                self.skip_group('[', ']')?;
                p = Part::Ser(Ser::Ctor("Object"));
            }
        }
        Some(p)
    }

    fn primary(&mut self) -> Option<Part> {
        let t = self.peek()?.clone();
        self.i += 1;
        let s = match t {
            T::Str | T::Template => Ser::Ctor("String"),
            T::Num => Ser::Ctor("Number"),
            T::BigInt => Ser::Ctor("BigInt"),
            T::P('-') => {
                if matches!(self.peek(), Some(T::BigInt)) {
                    self.i += 1;
                    Ser::Ctor("BigInt")
                } else {
                    self.i += 1;
                    Ser::Ctor("Number")
                }
            }
            T::P('(') => {
                let start = self.i;
                self.skip_group('(', ')')?;
                if self.peek() == Some(&T::Arrow) {
                    self.i += 1;
                    self.union()?;
                    Ser::Ctor("Function")
                } else {
                    // A parenthesized type.
                    let inner = &self.t[start..self.i - 1];
                    let mut r = Reader { t: inner, i: 0 };
                    let s = r.union()?;
                    if s == Ser::Void {
                        return Some(Part::Skip);
                    }
                    s
                }
            }
            T::P('<') => {
                // A generic function type.
                self.skip_group('<', '>')?;
                if !self.eat('(') {
                    return None;
                }
                self.skip_group('(', ')')?;
                if self.peek() != Some(&T::Arrow) {
                    return None;
                }
                self.i += 1;
                self.union()?;
                Ser::Ctor("Function")
            }
            T::P('{') => {
                self.skip_group('{', '}')?;
                Ser::Ctor("Object")
            }
            T::P('[') => {
                self.skip_group('[', ']')?;
                Ser::Ctor("Array")
            }
            T::Word(w) => match w {
                "string" => Ser::Ctor("String"),
                "number" => Ser::Ctor("Number"),
                "boolean" | "true" | "false" => Ser::Ctor("Boolean"),
                "bigint" => Ser::Ctor("BigInt"),
                "symbol" => Ser::Ctor("Symbol"),
                "any" | "unknown" | "object" | "this" => Ser::Ctor("Object"),
                "void" => Ser::Void,
                "null" | "undefined" | "never" => {
                    return Some(Part::Skip);
                }
                "new" => {
                    self.rest_of_function()?;
                    Ser::Ctor("Function")
                }
                "abstract" if self.peek() == Some(&T::Word("new")) => {
                    self.i += 1;
                    self.rest_of_function()?;
                    Ser::Ctor("Function")
                }
                "readonly" => return self.postfix(),
                "unique" if self.peek() == Some(&T::Word("symbol")) => {
                    self.i += 1;
                    Ser::Ctor("Symbol")
                }
                "keyof" | "infer" => {
                    self.postfix()?;
                    Ser::Ctor("Object")
                }
                "typeof" => {
                    self.qualified()?;
                    if self.eat('<') {
                        self.skip_group('<', '>')?;
                    }
                    Ser::Ctor("Object")
                }
                "asserts" => {
                    while self.peek().is_some() {
                        self.i += 1;
                    }
                    Ser::Void
                }
                _ => {
                    // `x is T` (a type predicate) or a type reference.
                    if self.peek() == Some(&T::Word("is")) {
                        self.i += 1;
                        self.union()?;
                        Ser::Ctor("Boolean")
                    } else {
                        let mut path = vec![w.to_string()];
                        while self.eat('.') {
                            match self.peek() {
                                Some(T::Word(n)) => {
                                    path.push(n.to_string());
                                    self.i += 1;
                                }
                                _ => return None,
                            }
                        }
                        if self.eat('<') {
                            self.skip_group('<', '>')?;
                        }
                        Ser::Ref(path)
                    }
                }
            },
            _ => return None,
        };
        Some(Part::Ser(s))
    }

    /// `new (…) => T` / `new <T>(…) => T` after its `new`.
    fn rest_of_function(&mut self) -> Option<()> {
        if self.eat('<') {
            self.skip_group('<', '>')?;
        }
        if !self.eat('(') {
            return None;
        }
        self.skip_group('(', ')')?;
        if self.peek() != Some(&T::Arrow) {
            return None;
        }
        self.i += 1;
        self.union().map(|_| ())
    }

    fn qualified(&mut self) -> Option<()> {
        match self.peek() {
            Some(T::Word(_)) => self.i += 1,
            _ => return None,
        }
        while self.eat('.') {
            match self.peek() {
                Some(T::Word(_)) => self.i += 1,
                _ => return None,
            }
        }
        Some(())
    }
}

/// tsc's union serialization: `null`, `undefined` and `never` drop out;
/// what remains must serialize alike, else it is `Object`.
fn combine(parts: Vec<Part>) -> Ser {
    let mut out: Option<Ser> = None;
    for p in parts {
        let Part::Ser(s) = p else { continue };
        if s == Ser::Ctor("Object") {
            return s;
        }
        if s == Ser::Void {
            continue;
        }
        match &out {
            None => out = Some(s),
            Some(o) if *o == s => {}
            Some(_) => return Ser::Ctor("Object"),
        }
    }
    out.unwrap_or(Ser::Void)
}

#[cfg(test)]
mod tests {
    use super::{classify, Ser};

    #[test]
    fn types_serialize_as_tsc_serializes_them() {
        let ctor = |s| Ser::Ctor(s);
        let cases: &[(&str, Ser)] = &[
            ("string", ctor("String")),
            ("string | null", ctor("String")),
            ("\"a\" | \"b\"", ctor("String")),
            ("1 | 2", ctor("Number")),
            ("string | number", ctor("Object")),
            ("() => void", ctor("Function")),
            ("string[]", ctor("Array")),
            ("[number, string]", ctor("Array")),
            ("{ a: 1 }", ctor("Object")),
            ("any", ctor("Object")),
            ("unknown", ctor("Object")),
            ("boolean", ctor("Boolean")),
            ("bigint", ctor("BigInt")),
            ("symbol", ctor("Symbol")),
            ("object", ctor("Object")),
            ("keyof I", ctor("Object")),
            ("`x${string}`", ctor("String")),
            ("never", Ser::Void),
            ("typeof Local", ctor("Object")),
            ("(string)", ctor("String")),
            ("readonly string[]", ctor("Array")),
            ("true", ctor("Boolean")),
            ("null", Ser::Void),
            ("void", Ser::Void),
            ("x is string", ctor("Boolean")),
            ("this", ctor("Object")),
            ("new () => Foo", ctor("Function")),
            ("<T>(x: T) => T", ctor("Function")),
            ("Promise<number>", Ser::Ref(vec!["Promise".into()])),
            ("NS.Inner", Ser::Ref(vec!["NS".into(), "Inner".into()])),
            ("Local | undefined", Ser::Ref(vec!["Local".into()])),
            ("Local & I", ctor("Object")),
            ("A extends B ? C : D", ctor("Object")),
        ];
        for (text, want) in cases {
            assert_eq!(&classify(text), want, "{text}");
        }
    }
}
