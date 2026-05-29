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
    /// A struct, identified by its index in `Module::structs`.
    Struct(u32),
    // Sized integers (v0: scalar only — not yet array elements). Reached via
    // casts (`i32(e)`, `u32(e)`, `u64(e)`); `i64` is the default integer type.
    I32,
    U32,
    U64,
}

impl Type {
    /// As a scalar element type (for use as an array element), if scalar.
    /// v0 arrays hold i64/f64/bool (not sized ints, strings, structs, nesting).
    pub fn as_elem(self) -> Option<Elem> {
        match self {
            Type::I64 => Some(Elem::I64),
            Type::F64 => Some(Elem::F64),
            Type::Bool => Some(Elem::Bool),
            _ => None,
        }
    }

    /// True for any integer type (i32/u32/i64/u64). `bool` is not counted.
    pub fn is_int(self) -> bool {
        matches!(self, Type::I32 | Type::U32 | Type::I64 | Type::U64)
    }

    /// True for any number (integer or f64).
    pub fn is_numeric(self) -> bool {
        self.is_int() || self == Type::F64
    }

    /// (bit width, signed) for an integer type.
    pub fn int_info(self) -> Option<(u8, bool)> {
        match self {
            Type::I32 => Some((32, true)),
            Type::U32 => Some((32, false)),
            Type::I64 => Some((64, true)),
            Type::U64 => Some((64, false)),
            _ => None,
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
    /// Struct literal `Name { field: value, ... }`.
    StructLit { name: String, fields: Vec<(String, Expr)> },
    /// Field access `base.field`.
    Field { base: Box<Expr>, field: String },
}

/// A statement plus the source line it starts on (for error messages).
#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
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
    For {
        init: Option<Box<Stmt>>,
        cond: Expr,
        step: Option<Box<Stmt>>,
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
pub struct StructDecl {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub funcs: Vec<Func>,
    pub structs: Vec<StructDecl>,
}
