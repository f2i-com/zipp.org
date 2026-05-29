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
    /// A nullable struct reference, `T | null`. Runtime value is a struct pointer
    /// that may be null. Native tiers represent it as a pointer (0 = null).
    OptStruct(u32),
    /// Nullable heap references `str | null` / `T[] | null` — a pointer that may
    /// be null (0), like `OptStruct`; native tiers handle them directly.
    OptStr,
    OptArr(Elem),
    /// Nullable scalars `i64 | null` / `f64 | null` / `bool | null`. Scalars have
    /// no spare null sentinel, so these run on the interpreter (dynamically
    /// tagged); the native tiers fall back.
    OptI64,
    OptF64,
    OptBool,
    /// The type of the `null` literal — assignable to any nullable type.
    Null,
    /// A first-class function value `(P0, …) => R`, identified by its index in
    /// `Module::func_types`. The runtime value is a function index; the native
    /// tiers fall back (interpreter-only in v0).
    Func(u32),
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

    /// The non-null inner type of a nullable type (`T | null` → `T`), else None.
    pub fn opt_inner(self) -> Option<Type> {
        match self {
            Type::OptStruct(id) => Some(Type::Struct(id)),
            Type::OptStr => Some(Type::Str),
            Type::OptArr(e) => Some(Type::Array(e)),
            Type::OptI64 => Some(Type::I64),
            Type::OptF64 => Some(Type::F64),
            Type::OptBool => Some(Type::Bool),
            _ => None,
        }
    }

    /// True for a nullable type (`T | null`).
    pub fn is_opt(self) -> bool {
        self.opt_inner().is_some()
    }

    /// True for a nullable *heap* type (struct/str/array) — native tiers handle
    /// these as a possibly-null pointer (nullable scalars need boxing instead).
    pub fn is_opt_heap(self) -> bool {
        matches!(self, Type::OptStruct(_) | Type::OptStr | Type::OptArr(_))
    }

    /// The nullable wrapper of a scalar/heap type (`T` → `T | null`), if any.
    pub fn into_opt(self) -> Option<Type> {
        match self {
            Type::Struct(id) => Some(Type::OptStruct(id)),
            Type::Str => Some(Type::OptStr),
            Type::Array(e) => Some(Type::OptArr(e)),
            Type::I64 => Some(Type::OptI64),
            Type::F64 => Some(Type::OptF64),
            Type::Bool => Some(Type::OptBool),
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
    /// Conditional (ternary) `cond ? then : els`; both arms share a type.
    Cond { cond: Box<Expr>, then: Box<Expr>, els: Box<Expr> },
    /// The `null` literal.
    Null,
    /// Nullish coalescing `lhs ?? rhs` — `lhs` if non-null, else `rhs`.
    Coalesce { lhs: Box<Expr>, rhs: Box<Expr> },
    /// Optional field access `base?.field` — `null` if `base` is null, else
    /// `base.field`. Result is nullable (the field must be a struct in v0).
    OptField { base: Box<Expr>, field: String },
    /// Fused `base?.field ?? default` — the field's value, or `default` when
    /// `base` is null. Result is the (non-null) field type; works for any field.
    OptFieldOr { base: Box<Expr>, field: String, default: Box<Expr> },
    /// Optional method call `recv?.m(args)` — `null` if `recv` is null, else
    /// `name(recv, args)`. Result is nullable (the method must return a heap type).
    OptCall { recv: Box<Expr>, name: String, args: Vec<Expr> },
    /// Fused `recv?.m(args) ?? default` — the method result, or `default` when
    /// `recv` is null. Result is the (non-null) return type; works for any return.
    OptCallOr { recv: Box<Expr>, name: String, args: Vec<Expr>, default: Box<Expr> },
    /// A named function (or lifted lambda) used as a first-class value. Its type
    /// is the corresponding `Type::Func`.
    FuncRef(String),
    /// A closure: a lifted function plus the values it captures. The lifted
    /// function takes the captures as its leading parameters; `captures` are the
    /// expressions (evaluated in the enclosing scope) that supply them. Its type
    /// is the `Func` type of the *remaining* (explicit) parameters.
    MakeClosure { name: String, captures: Vec<Expr> },
    /// An indirect call through a function value `f(args)` (where `f` has a
    /// `Type::Func`). Distinguished from `Call` (a direct, statically-named call).
    CallValue { callee: Box<Expr>, args: Vec<Expr> },
    /// `arr.push(value)` — append to a growable array, returns the new length
    /// (`i64`). Mutates `arr`. Growable arrays are interpreter-tier in v0.
    Push { arr: Box<Expr>, value: Box<Expr> },
    /// `arr.pop()` — remove and return the last element (a runtime error if the
    /// array is empty). Result is the element type.
    Pop { arr: Box<Expr> },
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

/// A first-class function type `(params) => ret`, interned in `Module::func_types`
/// and referenced by index from `Type::Func`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncType {
    pub params: Vec<Type>,
    pub ret: Type,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub funcs: Vec<Func>,
    pub structs: Vec<StructDecl>,
    /// Interned function types (indexed by `Type::Func`). Empty unless the
    /// program uses first-class functions.
    pub func_types: Vec<FuncType>,
}
