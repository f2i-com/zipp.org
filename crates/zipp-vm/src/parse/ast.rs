//! The AST.
//!
//! Derived from what the compiler actually consumes — every `ox::` reference in
//! `compile/` (12.5k lines) and `capture.rs` — rather than from what a general
//! JS tooling AST offers. Five decisions follow from serving an engine, and
//! each removes something the current arrangement pays for:
//!
//! **Owned, not arena-allocated.** No lifetime parameter, so the tree is `Send`
//! and can live in an `Arc`. `vm/agents.rs` has to hand a parsed program to a
//! worker thread and today RE-PARSES in-thread because `oxc_allocator` is not
//! `Send`; the module loader wants a `HashMap<PathBuf, Arc<..>>` cache it cannot
//! have for the same reason. If allocation ever profiles hot, the escape hatch
//! is `Box<Expr>` → an index into a flat `Vec<Expr>` — still owned, still
//! `Send`, and confined to this file. Not `bumpalo`: that reintroduces the
//! lifetime this exists to remove.
//!
//! **A call can be an assignment target.** Annex B makes
//! `AssignmentTargetType(CallExpression)` *simple* in sloppy code, so `f() = 1`
//! must parse and throw a ReferenceError at RUNTIME. An AST that cannot say
//! that forces the engine to rewrite source text and reparse
//! (`crate::annexb_call_target_rewrite`), a workaround whose own comment admits
//! it mishandles a strict function nested in a sloppy script. [`Target::Call`]
//! says it directly.
//!
//! **Strings are [`StrVal`], not `String`.** A literal may contain lone
//! surrogates. The engine currently carries a parallel WTF-8 buffer
//! (`compile/mod.rs` `exact_src`) purely to recover them.
//!
//! **No parenthesized-expression node.** Parenthesization is observable in
//! exactly two places — it makes an assignment target non-simple, and it
//! suppresses NamedEvaluation on `(x) = function(){}` — so it is a `bool` on
//! [`Target`], not a wrapper node that 13 sites would have to peel.
//!
//! **No scope or binding information on nodes.** The parser raises early errors
//! as it goes and discards its scope tree; the compiler builds its own
//! (`compile/scopes.rs`, `capture.rs`). Only the three facts `FuncProto` already
//! has fields for survive onto nodes: strictness, simple-parameter-list, and a
//! function's source span.

use super::token::{Span, StrVal};

/// An identifier. `Box<str>` rather than `String`: an AST holds a great many of
/// these and never grows one.
pub type Name = Box<str>;

// ============================================================================
// Program
// ============================================================================

/// What grammar the source is parsed under. Always supplied by the caller —
/// there is no "sniff it from the content" mode, because the engine has to make
/// that decision once, explicitly, for `annexb_call_target_rewrite` and the
/// test262 harness to behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goal {
    Script,
    Module,
    /// A direct or indirect `eval`. Differs from `Script` in that its
    /// completion value is observable.
    EvalScript,
    /// `new Function(...)`'s body — top-level `return` is legal here and only
    /// here.
    FunctionBody,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub goal: Goal,
    /// `"use strict"` was in effect for the whole program (directive prologue,
    /// module goal, or forced by a strict eval caller).
    pub strict: bool,
    pub directives: Vec<Directive>,
    pub body: Vec<Stmt>,
}

/// A directive-prologue entry. The RAW text matters, not the cooked value:
/// `"use strict"` is not a use-strict directive.
#[derive(Debug, Clone)]
pub struct Directive {
    pub raw: Box<str>,
    pub value: StrVal,
}

// ============================================================================
// Statements
// ============================================================================

#[derive(Debug, Clone)]
pub enum Stmt {
    Block(Vec<Stmt>),
    Empty,
    Expr(Expr),
    /// `if (test) cons else alt`
    If { test: Expr, cons: Box<Stmt>, alt: Option<Box<Stmt>> },
    While { test: Expr, body: Box<Stmt> },
    DoWhile { body: Box<Stmt>, test: Expr },
    For { init: Option<ForInit>, test: Option<Expr>, update: Option<Expr>, body: Box<Stmt> },
    ForIn { left: ForTarget, right: Expr, body: Box<Stmt> },
    /// `await` is set for `for await (… of …)`.
    ForOf { left: ForTarget, right: Expr, body: Box<Stmt>, is_await: bool },
    Switch { disc: Expr, cases: Vec<SwitchCase> },
    /// `test: None` is the `default:` clause.
    Return(Option<Expr>),
    Throw(Expr),
    Try { block: Vec<Stmt>, handler: Option<CatchClause>, finalizer: Option<Vec<Stmt>> },
    Break(Option<Name>),
    Continue(Option<Name>),
    Labeled { label: Name, body: Box<Stmt> },
    With { object: Expr, body: Box<Stmt> },
    Debugger,
    VarDecl(VarDecl),
    FnDecl(Box<Function>),
    ClassDecl(Box<Class>),
    Import(Box<ImportDecl>),
    Export(Box<ExportDecl>),
}

#[derive(Debug, Clone)]
pub struct SwitchCase {
    /// `None` for `default:`.
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct CatchClause {
    /// `catch {}` (optional binding) has `None`.
    pub param: Option<Pattern>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
    /// Explicit resource management: `using x = ...`.
    Using,
    AwaitUsing,
}

impl VarKind {
    pub fn is_lexical(self) -> bool {
        !matches!(self, VarKind::Var)
    }
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub kind: VarKind,
    pub decls: Vec<Declarator>,
}

#[derive(Debug, Clone)]
pub struct Declarator {
    pub id: Pattern,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone)]
pub enum ForInit {
    Var(VarDecl),
    Expr(Expr),
}

/// The left-hand side of a `for-in`/`for-of` head, which is either a fresh
/// declaration or an assignment to something that already exists.
#[derive(Debug, Clone)]
pub enum ForTarget {
    Var(VarDecl),
    Target(Target),
}

// ============================================================================
// Expressions
// ============================================================================

#[derive(Debug, Clone)]
pub enum Expr {
    Ident(Name),
    /// `this`
    This,
    /// `super` — only legal in positions the parser checks.
    Super,
    Null,
    Bool(bool),
    Num(f64),
    BigInt(Box<str>),
    Str(StrVal),
    Regex { pattern: StrVal, flags: Box<str> },
    /// A hole in `[1, , 3]` is `None`; a spread element is `Spread`.
    Array(Vec<Option<ArrayElem>>),
    Object(Vec<ObjectMember>),
    Template(Box<TemplateLit>),
    TaggedTemplate { tag: Box<Expr>, quasi: Box<TemplateLit> },
    Unary { op: UnaryOp, arg: Box<Expr> },
    Update { op: UpdateOp, prefix: bool, target: Target },
    Binary { op: BinaryOp, left: Box<Expr>, right: Box<Expr> },
    Logical { op: LogicalOp, left: Box<Expr>, right: Box<Expr> },
    Assign { op: AssignOp, target: Target, value: Box<Expr> },
    Cond { test: Box<Expr>, cons: Box<Expr>, alt: Box<Expr> },
    Call(Box<CallExpr>),
    New { callee: Box<Expr>, args: Vec<Arg> },
    Member(Box<Member>),
    /// An optional chain `a?.b.c` — the whole chain, so short-circuiting is
    /// scoped correctly. `Member`/`Call` nodes inside carry `optional`.
    Chain(Box<Expr>),
    Seq(Vec<Expr>),
    Arrow(Box<Arrow>),
    Function(Box<Function>),
    Class(Box<Class>),
    Yield { arg: Option<Box<Expr>>, delegate: bool },
    Await(Box<Expr>),
    /// `new.target`
    NewTarget,
    /// `import.meta`
    ImportMeta,
    /// `import(spec, options)` — a call, not a reference to `import`.
    ImportCall { spec: Box<Expr>, options: Option<Box<Expr>>, phase: ImportPhase },
}

#[derive(Debug, Clone)]
pub enum ArrayElem {
    Expr(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub struct CallExpr {
    pub callee: Expr,
    pub args: Vec<Arg>,
    /// `a?.()`
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub enum Arg {
    Expr(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub struct Member {
    pub object: Expr,
    pub prop: MemberProp,
    /// `a?.b`
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub enum MemberProp {
    /// `a.b`
    Ident(Name),
    /// `a[b]`
    Computed(Expr),
    /// `a.#b` — a private name, which is not a string key.
    Private(Name),
}

#[derive(Debug, Clone)]
pub struct TemplateLit {
    /// `quasis.len() == exprs.len() + 1`, always.
    pub quasis: Vec<TemplateElement>,
    pub exprs: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct TemplateElement {
    /// `None` when the chunk contains an invalid escape. Legal in a TAGGED
    /// template (the cooked value is `undefined`); a SyntaxError otherwise, so
    /// the distinction has to survive to whoever knows which this is.
    pub cooked: Option<StrVal>,
    pub raw: Box<str>,
}

// ============================================================================
// Assignment targets
// ============================================================================

/// The left-hand side of an assignment, or a `for-in`/`for-of` head.
///
/// Distinct from [`Pattern`], which is a *binding* form: `[a.b] = x` is a valid
/// assignment target but not a valid declaration.
#[derive(Debug, Clone)]
pub enum Target {
    Ident {
        name: Name,
        /// The target was parenthesized — `(x) = 1`. Observable twice: it makes
        /// the target non-simple for some early errors, and it suppresses
        /// NamedEvaluation, so `(x) = function(){}` leaves the function
        /// anonymous.
        covered: bool,
    },
    Member(Box<Member>),
    /// Annex B: `AssignmentTargetType(CallExpression)` is `simple` in SLOPPY
    /// code, so `f() = 1` parses and throws a ReferenceError when evaluated.
    /// The parser refuses to build this in strict code, so the compiler never
    /// has to re-check.
    Call(Box<CallExpr>),
    Array(Vec<Option<TargetElem>>),
    Object { props: Vec<TargetProp>, rest: Option<Box<Target>> },
}

#[derive(Debug, Clone)]
pub struct TargetElem {
    pub target: Target,
    /// `[a = 1] = x`
    pub default: Option<Expr>,
    /// `[...a] = x`
    pub rest: bool,
}

#[derive(Debug, Clone)]
pub struct TargetProp {
    pub key: PropKey,
    pub target: Target,
    pub default: Option<Expr>,
    /// `{a} = x` rather than `{a: a} = x`. Shorthand is observable: only a
    /// shorthand property may carry a default.
    pub shorthand: bool,
}

// ============================================================================
// Binding patterns
// ============================================================================

/// A *declaration* form: parameters, `var`/`let`/`const` ids, catch params.
#[derive(Debug, Clone)]
pub enum Pattern {
    Ident(Name),
    Array(Vec<Option<PatternElem>>),
    Object { props: Vec<PatternProp>, rest: Option<Box<Pattern>> },
    /// `x = 1` in a parameter or destructuring position.
    Assign { left: Box<Pattern>, right: Box<Expr> },
    /// `...x`
    Rest(Box<Pattern>),
}

#[derive(Debug, Clone)]
pub struct PatternElem {
    pub pat: Pattern,
}

#[derive(Debug, Clone)]
pub struct PatternProp {
    pub key: PropKey,
    pub value: Pattern,
    pub shorthand: bool,
}

// ============================================================================
// Property keys
// ============================================================================

#[derive(Debug, Clone)]
pub enum PropKey {
    Ident(Name),
    Str(StrVal),
    /// A numeric key. The canonical string form is what the object model uses,
    /// and it is not `format!("{}", n)` for every value, so the parser computes
    /// it once rather than leaving each consumer to get it wrong.
    Num(f64),
    Computed(Expr),
    /// `#x` in a class body.
    Private(Name),
}

// ============================================================================
// Object literals
// ============================================================================

#[derive(Debug, Clone)]
pub enum ObjectMember {
    /// `k: v`, or shorthand `k`.
    Prop { key: PropKey, value: Expr, shorthand: bool },
    /// `m() {}`
    Method { key: PropKey, func: Box<Function> },
    Get { key: PropKey, func: Box<Function> },
    Set { key: PropKey, func: Box<Function> },
    /// `...x`
    Spread(Expr),
}

// ============================================================================
// Functions and classes
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct Params {
    pub items: Vec<Pattern>,
    /// No defaults, no destructuring, no rest. Drives `arguments` mapping and a
    /// family of parameter early errors, and `FuncProto` already records it.
    pub simple: bool,
}

#[derive(Debug, Clone)]
pub struct Function {
    /// `None` for an anonymous function expression.
    pub name: Option<Name>,
    pub params: Params,
    pub body: FnBody,
    pub is_async: bool,
    pub is_generator: bool,
    /// Byte range of the whole function in the source — `Function.prototype
    /// .toString` must return the exact original text, so this is not
    /// reconstructible.
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FnBody {
    pub directives: Vec<Directive>,
    pub stmts: Vec<Stmt>,
    /// Strict because of its own prologue, or inherited.
    pub strict: bool,
}

#[derive(Debug, Clone)]
pub struct Arrow {
    pub params: Params,
    /// `x => x + 1` has an expression body; its value is the return value.
    pub body: ArrowBody,
    pub is_async: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ArrowBody {
    Expr(Box<Expr>),
    Block(FnBody),
}

#[derive(Debug, Clone)]
pub struct Class {
    pub name: Option<Name>,
    pub superclass: Option<Box<Expr>>,
    pub body: Vec<ClassMember>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ClassMember {
    Method(ClassMethod),
    Field(ClassField),
    /// `static { ... }`
    StaticBlock(Vec<Stmt>),
}

#[derive(Debug, Clone)]
pub struct ClassMethod {
    pub key: PropKey,
    pub kind: MethodKind,
    pub func: Box<Function>,
    pub is_static: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    Method,
    Constructor,
    Get,
    Set,
}

#[derive(Debug, Clone)]
pub struct ClassField {
    pub key: PropKey,
    pub value: Option<Expr>,
    pub is_static: bool,
}

// ============================================================================
// Modules
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportPhase {
    Evaluation,
    Source,
    Defer,
}

/// A module specifier name, which may be an arbitrary string:
/// `export { x as "a b" }` is legal.
#[derive(Debug, Clone)]
pub enum ModuleExportName {
    Ident(Name),
    Str(StrVal),
}

#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub specifiers: Vec<ImportSpecifier>,
    pub source: StrVal,
    /// `with { type: "json" }`
    pub attributes: Vec<ImportAttribute>,
    pub phase: ImportPhase,
}

#[derive(Debug, Clone)]
pub enum ImportSpecifier {
    /// `import d from "m"`
    Default(Name),
    /// `import * as ns from "m"`
    Namespace(Name),
    /// `import { a as b } from "m"`
    Named { imported: ModuleExportName, local: Name },
}

#[derive(Debug, Clone)]
pub struct ImportAttribute {
    pub key: StrVal,
    pub value: StrVal,
}

#[derive(Debug, Clone)]
pub enum ExportDecl {
    /// `export { a, b as c }` / `export { a } from "m"`
    Named {
        specifiers: Vec<ExportSpecifier>,
        source: Option<StrVal>,
        attributes: Vec<ImportAttribute>,
    },
    /// `export default <expr|function|class>`
    Default(ExportDefault),
    /// `export * from "m"` / `export * as ns from "m"`
    All { alias: Option<ModuleExportName>, source: StrVal, attributes: Vec<ImportAttribute> },
    /// `export var/let/const/function/class ...`
    Decl(Box<Stmt>),
}

#[derive(Debug, Clone)]
pub struct ExportSpecifier {
    pub local: ModuleExportName,
    pub exported: ModuleExportName,
}

#[derive(Debug, Clone)]
pub enum ExportDefault {
    Expr(Expr),
    Function(Box<Function>),
    Class(Box<Class>),
}

// ============================================================================
// Operators
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Minus,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOp {
    Inc,
    Dec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Rem, Exp,
    Eq, NotEq, StrictEq, StrictNotEq,
    Lt, LtEq, Gt, GtEq,
    Shl, Shr, UShr,
    BitAnd, BitOr, BitXor,
    In, Instanceof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
    /// `??`
    Coalesce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    /// `=`
    Assign,
    Add, Sub, Mul, Div, Rem, Exp,
    Shl, Shr, UShr,
    BitAnd, BitOr, BitXor,
    /// `&&=`, `||=`, `??=` — these SHORT-CIRCUIT, so they are not sugar for the
    /// arithmetic forms and the compiler must not treat them as such.
    LogicalAnd, LogicalOr, LogicalCoalesce,
}

impl AssignOp {
    /// The short-circuiting forms evaluate their right side conditionally.
    pub fn is_logical(self) -> bool {
        matches!(self, AssignOp::LogicalAnd | AssignOp::LogicalOr | AssignOp::LogicalCoalesce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The AST must be able to SAY the things the current one cannot. These are
    /// not parser tests — there is no parser yet — they pin the representability
    /// claims that justify replacing the AST at all.
    #[test]
    fn expresses_what_the_oxc_shape_could_not() {
        // 1. `f() = 1`. Annex B makes this a *simple* assignment target in
        //    sloppy code: it must parse, and throw a ReferenceError only when
        //    evaluated. Being unable to represent it is what forces
        //    `annexb_call_target_rewrite` to rewrite source text and reparse.
        let annexb = Expr::Assign {
            op: AssignOp::Assign,
            target: Target::Call(Box::new(CallExpr {
                callee: Expr::Ident("f".into()),
                args: vec![],
                optional: false,
            })),
            value: Box::new(Expr::Num(1.0)),
        };
        assert!(matches!(annexb, Expr::Assign { target: Target::Call(_), .. }));

        // 2. A lone surrogate in a literal. `String` cannot hold one, which is
        //    why the compiler carries a parallel WTF-8 buffer (`exact_src`).
        let lone = Expr::Str(StrVal::from_utf16(vec![0xD800]));
        match lone {
            Expr::Str(StrVal::Utf16(u)) => assert_eq!(u, vec![0xD800]),
            other => panic!("lone surrogate did not survive: {other:?}"),
        }

        // 3. Parenthesization, without a wrapper node. `(x) = f` is a valid
        //    assignment but suppresses NamedEvaluation, so the function stays
        //    anonymous — one bool rather than a node 13 sites must peel.
        let covered = Target::Ident { name: "x".into(), covered: true };
        assert!(matches!(covered, Target::Ident { covered: true, .. }));
    }

    /// Holes and spreads are different things and both are observable, so the
    /// array shape has to distinguish them from an ordinary element.
    #[test]
    fn array_holes_and_spreads_are_distinct() {
        // `[1, , ...r]`
        let a = Expr::Array(vec![
            Some(ArrayElem::Expr(Expr::Num(1.0))),
            None,
            Some(ArrayElem::Spread(Expr::Ident("r".into()))),
        ]);
        match a {
            Expr::Array(items) => {
                assert!(matches!(items[0], Some(ArrayElem::Expr(_))));
                assert!(items[1].is_none(), "a hole is not `undefined`");
                assert!(matches!(items[2], Some(ArrayElem::Spread(_))));
            }
            _ => unreachable!(),
        }
    }

    /// The short-circuiting assignments are not sugar for the arithmetic ones —
    /// `a ||= b` must not evaluate `b` when `a` is truthy.
    #[test]
    fn logical_assignment_is_marked_short_circuiting() {
        for op in [AssignOp::LogicalAnd, AssignOp::LogicalOr, AssignOp::LogicalCoalesce] {
            assert!(op.is_logical(), "{op:?} short-circuits");
        }
        for op in [AssignOp::Assign, AssignOp::Add, AssignOp::BitOr, AssignOp::Exp] {
            assert!(!op.is_logical(), "{op:?} does not");
        }
    }

    /// The tree has no lifetime, so it can cross a thread boundary and live in
    /// an `Arc`. `vm/agents.rs` re-parses in-thread today *because* an
    /// arena-allocated AST cannot; the module loader wants an `Arc` cache for
    /// the same reason.
    #[test]
    fn the_tree_is_send_and_shareable() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Program>();
        assert_sync::<Program>();
        assert_send::<std::sync::Arc<Program>>();

        let p = Program {
            goal: Goal::Script,
            strict: false,
            directives: vec![],
            body: vec![Stmt::Expr(Expr::Num(1.0))],
        };
        let shared = std::sync::Arc::new(p);
        let clone = std::sync::Arc::clone(&shared);
        let handle = std::thread::spawn(move || clone.body.len());
        assert_eq!(handle.join().unwrap(), 1);
    }
}
