//! ZIPP abstract syntax tree (v0 sound subset).

/// Scalar element type for arrays (keeps `Type` `Copy` — v0 arrays are 1-D).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elem {
    I64,
    F64,
    Bool,
}

impl Elem {
    pub fn to_type(self) -> Type {
        match self {
            Elem::I64 => Type::I64,
            Elem::F64 => Type::F64,
            Elem::Bool => Type::Bool,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    I64,
    F64,
    Bool,
    Str,
    Array(Elem),
}

impl Type {
    /// As a scalar element type (for use as an array element), if scalar.
    /// v0 arrays hold i64/f64/bool (not strings or nested arrays).
    pub fn as_elem(self) -> Option<Elem> {
        match self {
            Type::I64 => Some(Elem::I64),
            Type::F64 => Some(Elem::F64),
            Type::Bool => Some(Elem::Bool),
            Type::Str | Type::Array(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    // Bitwise / shift (integer-only — PLAN.md §5.4 native integers).
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Var(String),
    Cast { to: Type, e: Box<Expr> },
    Unary { op: UnOp, e: Box<Expr> },
    Bin { op: BinOp, l: Box<Expr>, r: Box<Expr> },
    Call { name: String, args: Vec<Expr> },
    /// Array literal `[a, b, c]`.
    Array(Vec<Expr>),
    /// Repeat literal `[value; count]`.
    Repeat { value: Box<Expr>, count: Box<Expr> },
    /// Indexing `arr[index]`.
    Index { arr: Box<Expr>, index: Box<Expr> },
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        ty: Option<Type>,
        value: Expr,
    },
    Assign {
        target: Expr, // lvalue: Var or Index
        value: Expr,
    },
    Return(Option<Expr>),
    If {
        cond: Expr,
        then_b: Vec<Stmt>,
        else_b: Vec<Stmt>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Print(Expr),
    ExprStmt(Expr),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Type,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub funcs: Vec<Func>,
}
