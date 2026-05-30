//! An owned (lifetime-free) JavaScript AST.
//!
//! The oxc parser produces an arena-allocated AST tied to a `'a` lifetime, which
//! would infect every interpreter value (closures hold function bodies). We lower
//! it once (see [`crate::lower`]) into this owned tree so [`crate::value::JsValue`]
//! carries no lifetime — and so a future bytecode compiler has a stable input.
//!
//! This is the v0 subset (see `js-engine-direction` memory): enough to run real
//! JS — operators with coercion, control flow, functions/closures, objects,
//! arrays, `throw`/`try`, template literals. Prototypes/classes/`new`, modules,
//! generators, destructuring and `for-in`/`for-of` are deferred to later tiers.

use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Var,
    Let,
    Const,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    EqEq,
    NotEq,
    StrictEq,
    StrictNotEq,
    Lt,
    Le,
    Gt,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    In,
    InstanceOf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
    Nullish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
    TypeOf,
    Void,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOp {
    Inc,
    Dec,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr),
    /// `let a = 1, b;` — one or more (name, initializer) bindings.
    Var {
        kind: DeclKind,
        decls: Vec<(String, Option<Expr>)>,
    },
    Block(Vec<Stmt>),
    If {
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Box<Stmt>,
    },
    /// `for (x of iterable) …` — iterates array elements / string chars. `decl`
    /// is the binding's declaration kind, or `None` to assign an existing var.
    ForOf {
        decl: Option<DeclKind>,
        name: String,
        iterable: Expr,
        body: Box<Stmt>,
    },
    /// `for (k in obj) …` — iterates own enumerable keys.
    ForIn {
        decl: Option<DeclKind>,
        name: String,
        object: Expr,
        body: Box<Stmt>,
    },
    /// `switch (disc) { case … : … ; default: … }` — `None` test = `default`.
    /// Fall-through preserved (a `case` runs into the next until `break`).
    Switch {
        disc: Expr,
        cases: Vec<(Option<Expr>, Vec<Stmt>)>,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    /// A hoisted function declaration.
    Func(Rc<FuncDef>),
    /// A class declaration (lowered to a constructor + prototype/static methods).
    Class(Rc<ClassDef>),
    Throw(Expr),
    Try {
        block: Vec<Stmt>,
        /// `(binding, body)` — the binding is `None` for `catch { … }`.
        catch: Option<(Option<String>, Vec<Stmt>)>,
        finally: Option<Vec<Stmt>>,
    },
    Empty,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    Str(Rc<str>),
    Bool(bool),
    Null,
    Undefined,
    Ident(String),
    This,
    /// A template literal: `strings.len() == exprs.len() + 1`, interleaved.
    Template {
        strings: Vec<Rc<str>>,
        exprs: Vec<Expr>,
    },
    /// `[a, , c]` — holes are `None`.
    Array(Vec<Option<Expr>>),
    Object(Vec<Prop>),
    Unary {
        op: UnOp,
        arg: Box<Expr>,
    },
    /// `++x` / `x--`.
    Update {
        op: UpdateOp,
        prefix: bool,
        arg: Box<Expr>,
    },
    Binary {
        op: BinOp,
        l: Box<Expr>,
        r: Box<Expr>,
    },
    Logical {
        op: LogicalOp,
        l: Box<Expr>,
        r: Box<Expr>,
    },
    /// `target = value` (`op` is `None`) or a compound assign like `x += 1`.
    /// `target` is an `Ident` or `Member`.
    Assign {
        op: Option<BinOp>,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// `target &&= value` / `||=` / `??=` — guarded short-circuit assignment.
    LogicalAssign {
        op: LogicalOp,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// `obj.prop` (`computed = false`, `prop` is `Str`) or `obj[expr]`.
    Member {
        obj: Box<Expr>,
        prop: Box<Expr>,
        computed: bool,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `new Callee(args)`.
    New {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Cond {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Func(Rc<FuncDef>),
    /// The comma operator `(a, b, c)` — evaluates all, yields the last.
    Seq(Vec<Expr>),
    /// The `super` keyword — only valid as `super(args)` or `super.member`.
    Super,
    /// `...expr` in an argument or array-element position (spread).
    Spread(Box<Expr>),
    /// An optional chain `a?.b.c?.()` — a base expression plus a sequence of
    /// member/call steps, where a nullish value at any `?.` step short-circuits
    /// the whole chain to `undefined`. (Modelled as one flattened node so the
    /// short-circuit boundary is explicit and existing Member/Call stay simple.)
    OptChain { base: Box<Expr>, steps: Vec<ChainStep> },
}

/// One step of an [`Expr::OptChain`]. `optional` marks a `?.` step (short-circuit
/// if the current value is nullish).
#[derive(Debug, Clone)]
pub enum ChainStep {
    /// `.prop` / `?.prop` / `[expr]` / `?.[expr]`.
    Member { prop: Box<Expr>, computed: bool, optional: bool },
    /// `(args)` / `?.(args)`.
    Call { args: Vec<Expr>, optional: bool },
}

#[derive(Debug, Clone)]
pub enum Prop {
    KeyVal { key: PropKey, value: Expr },
    /// `{ ...expr }` — copy the source object's own enumerable properties.
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum PropKey {
    /// An identifier or string/number literal key, pre-stringified.
    Static(String),
    /// `{ [expr]: v }`.
    Computed(Expr),
}

/// A function parameter: a name plus an optional default value (`x = expr`).
/// (Destructuring parameters are a later tier.)
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub default: Option<Expr>,
}

#[derive(Debug)]
pub struct FuncDef {
    pub name: Option<String>,
    pub params: Vec<Param>,
    /// A rest parameter `...name`, if present (gathers the remaining args).
    pub rest: Option<String>,
    pub body: Vec<Stmt>,
    /// Arrow functions inherit `this` lexically; regular functions bind it per
    /// call. (An arrow with an expression body is lowered to `[Return(expr)]`.)
    pub is_arrow: bool,
}

/// A class lowered to: a constructor function (instance field initializers
/// prepended to its body), instance methods (→ `C.prototype`), and static
/// methods (→ `C`). (v0: no `extends`/`super`, getters/setters, private fields.)
#[derive(Debug)]
pub struct ClassDef {
    pub name: String,
    /// The `extends` expression (parent constructor), if any.
    pub superclass: Option<Expr>,
    pub ctor: Rc<FuncDef>,
    pub methods: Vec<(String, Rc<FuncDef>)>,
    pub statics: Vec<(String, Rc<FuncDef>)>,
}
