//! The arena AST: every node lives in a `Vec` of its [`Module`] and refers to
//! others by `u32` index; lists are index ranges into side tables; names are
//! interned [`Sym`]s; ranges are byte offsets (line numbers come from
//! [`Module::line_starts`]). A module is a handful of allocations however
//! large the program.
//!
//! The node set is Python's `ast` module's (with RustPython's `Arguments`
//! shape for parameters), and the ranges are the ones the RustPython 0.4
//! parser ZIPP used gave; [`crate::tree`] lays a module out in that shape.

use crate::intern::{Interner, Sym};
use std::marker::PhantomData;
use std::num::NonZeroU32;

/// A byte range of the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

impl Range {
    #[inline]
    pub fn new(start: u32, end: u32) -> Range {
        Range { start, end }
    }
}

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name(NonZeroU32);
        impl $name {
            #[inline]
            pub(crate) fn from_index(index: usize) -> Self {
                $name(NonZeroU32::new(index as u32 + 1).expect("arena overflow"))
            }
            #[inline]
            pub fn index(self) -> usize {
                self.0.get() as usize - 1
            }
        }
    };
}

id!(/// An expression.
    ExprId);
id!(/// A statement.
    StmtId);
id!(/// A pattern.
    PatId);
id!(/// Parameters (`Arguments`).
    ArgsId);

/// A list: a range of a side table.
pub struct List<T> {
    pub start: u32,
    pub len: u32,
    _t: PhantomData<T>,
}

impl<T> Clone for List<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for List<T> {}
impl<T> Default for List<T> {
    fn default() -> Self {
        List::new(0, 0)
    }
}
impl<T> std::fmt::Debug for List<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "List({}..+{})", self.start, self.len)
    }
}
impl<T> PartialEq for List<T> {
    fn eq(&self, other: &Self) -> bool {
        self.start == other.start && self.len == other.len
    }
}

impl<T> List<T> {
    #[inline]
    pub(crate) fn new(start: u32, len: u32) -> Self {
        List {
            start,
            len,
            _t: PhantomData,
        }
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }
    #[inline]
    fn range(&self) -> std::ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExprContext {
    Load,
    Store,
    Del,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Add,
    Sub,
    Mult,
    MatMult,
    Div,
    Mod,
    Pow,
    LShift,
    RShift,
    BitOr,
    BitXor,
    BitAnd,
    FloorDiv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Invert,
    Not,
    UAdd,
    USub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    Is,
    IsNot,
    In,
    NotIn,
}

/// `!s`, `!r`, `!a` or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conversion {
    None,
    Str,
    Repr,
    Ascii,
}

/// A decoded string in [`Module::str_data`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrRef {
    pub start: u32,
    pub len: u32,
}

/// Where a number literal's text is in the source (the node's range may
/// differ inside f-strings, see `parser::fstring`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub len: u32,
}

/// A constant. Numbers keep their literal's text, read with
/// [`Module::int_digits`] and friends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Constant {
    None,
    True,
    False,
    Ellipsis,
    /// An int literal: its text, whose digits are in `radix`.
    Int {
        radix: u8,
        text: Span,
    },
    Float {
        text: Span,
    },
    /// An imaginary literal (`2j`): the imaginary part is the text before `j`.
    Complex {
        text: Span,
    },
    /// A string; `u` when it (or its first part) has the `u` prefix.
    Str {
        value: StrRef,
        u: bool,
    },
    Bytes {
        value: StrRef,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExprKind {
    BoolOp {
        op: BoolOp,
        values: List<ExprId>,
    },
    NamedExpr {
        target: ExprId,
        value: ExprId,
    },
    BinOp {
        left: ExprId,
        op: Operator,
        right: ExprId,
    },
    UnaryOp {
        op: UnaryOp,
        operand: ExprId,
    },
    Lambda {
        args: ArgsId,
        body: ExprId,
    },
    IfExp {
        test: ExprId,
        body: ExprId,
        orelse: ExprId,
    },
    /// `keys[i]` is `None` for `**value`.
    Dict {
        keys: List<Option<ExprId>>,
        values: List<ExprId>,
    },
    Set {
        elts: List<ExprId>,
    },
    ListComp {
        elt: ExprId,
        generators: List<Comprehension>,
    },
    SetComp {
        elt: ExprId,
        generators: List<Comprehension>,
    },
    DictComp {
        key: ExprId,
        value: ExprId,
        generators: List<Comprehension>,
    },
    GeneratorExp {
        elt: ExprId,
        generators: List<Comprehension>,
    },
    Await {
        value: ExprId,
    },
    Yield {
        value: Option<ExprId>,
    },
    YieldFrom {
        value: ExprId,
    },
    Compare {
        left: ExprId,
        ops: List<CmpOp>,
        comparators: List<ExprId>,
    },
    Call {
        func: ExprId,
        args: List<ExprId>,
        keywords: List<Keyword>,
    },
    FormattedValue {
        value: ExprId,
        conversion: Conversion,
        format_spec: Option<ExprId>,
    },
    JoinedStr {
        values: List<ExprId>,
    },
    Constant(Constant),
    Attribute {
        value: ExprId,
        attr: Sym,
        ctx: ExprContext,
    },
    Subscript {
        value: ExprId,
        slice: ExprId,
        ctx: ExprContext,
    },
    Starred {
        value: ExprId,
        ctx: ExprContext,
    },
    Name {
        id: Sym,
        ctx: ExprContext,
    },
    List {
        elts: List<ExprId>,
        ctx: ExprContext,
    },
    Tuple {
        elts: List<ExprId>,
        ctx: ExprContext,
    },
    Slice {
        lower: Option<ExprId>,
        upper: Option<ExprId>,
        step: Option<ExprId>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Expr {
    pub range: Range,
    pub kind: ExprKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Keyword {
    pub range: Range,
    /// `None` for `**value`.
    pub arg: Option<Sym>,
    pub value: ExprId,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Comprehension {
    pub range: Range,
    pub target: ExprId,
    pub iter: ExprId,
    pub ifs: List<ExprId>,
    pub is_async: bool,
}

/// A parameter: its name, annotation and (not for `*`/`**` ones) default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Param {
    /// The name and annotation (not the default).
    pub range: Range,
    pub name: Sym,
    pub annotation: Option<ExprId>,
    pub default: Option<ExprId>,
}

/// A function's or lambda's parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arguments {
    pub range: Range,
    pub posonlyargs: List<Param>,
    pub args: List<Param>,
    pub vararg: Option<u32>,
    pub kwonlyargs: List<Param>,
    pub kwarg: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WithItem {
    pub range: Range,
    pub context_expr: ExprId,
    pub optional_vars: Option<ExprId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExceptHandler {
    pub range: Range,
    pub type_: Option<ExprId>,
    pub name: Option<Sym>,
    pub body: List<StmtId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Alias {
    pub range: Range,
    pub name: Sym,
    pub asname: Option<Sym>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchCase {
    pub range: Range,
    pub pattern: PatId,
    pub guard: Option<ExprId>,
    pub body: List<StmtId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TypeParamKind {
    TypeVar { bound: Option<ExprId> },
    ParamSpec,
    TypeVarTuple,
}

/// A PEP 695 type parameter, with its PEP 696 default (Python 3.13).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypeParam {
    pub range: Range,
    pub name: Sym,
    pub kind: TypeParamKind,
    pub default: Option<ExprId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StmtKind {
    FunctionDef {
        is_async: bool,
        name: Sym,
        args: ArgsId,
        body: List<StmtId>,
        decorator_list: List<ExprId>,
        returns: Option<ExprId>,
        type_params: List<TypeParam>,
    },
    ClassDef {
        name: Sym,
        bases: List<ExprId>,
        keywords: List<Keyword>,
        body: List<StmtId>,
        decorator_list: List<ExprId>,
        type_params: List<TypeParam>,
    },
    Return {
        value: Option<ExprId>,
    },
    Delete {
        targets: List<ExprId>,
    },
    Assign {
        targets: List<ExprId>,
        value: ExprId,
    },
    TypeAlias {
        name: ExprId,
        type_params: List<TypeParam>,
        value: ExprId,
    },
    AugAssign {
        target: ExprId,
        op: Operator,
        value: ExprId,
    },
    AnnAssign {
        target: ExprId,
        annotation: ExprId,
        value: Option<ExprId>,
        simple: bool,
    },
    For {
        is_async: bool,
        target: ExprId,
        iter: ExprId,
        body: List<StmtId>,
        orelse: List<StmtId>,
    },
    While {
        test: ExprId,
        body: List<StmtId>,
        orelse: List<StmtId>,
    },
    If {
        test: ExprId,
        body: List<StmtId>,
        orelse: List<StmtId>,
    },
    With {
        is_async: bool,
        items: List<WithItem>,
        body: List<StmtId>,
    },
    Match {
        subject: ExprId,
        cases: List<MatchCase>,
    },
    Raise {
        exc: Option<ExprId>,
        cause: Option<ExprId>,
    },
    /// `star`: `except*`.
    Try {
        star: bool,
        body: List<StmtId>,
        handlers: List<ExceptHandler>,
        orelse: List<StmtId>,
        finalbody: List<StmtId>,
    },
    Assert {
        test: ExprId,
        msg: Option<ExprId>,
    },
    Import {
        names: List<Alias>,
    },
    ImportFrom {
        module: Option<Sym>,
        names: List<Alias>,
        level: u32,
    },
    Global {
        names: List<Sym>,
    },
    Nonlocal {
        names: List<Sym>,
    },
    Expr {
        value: ExprId,
    },
    Pass,
    Break,
    Continue,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stmt {
    pub range: Range,
    pub kind: StmtKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PatternKind {
    MatchValue {
        value: ExprId,
    },
    /// `None`, `True` or `False`.
    MatchSingleton {
        value: Constant,
    },
    MatchSequence {
        patterns: List<PatId>,
    },
    MatchMapping {
        keys: List<ExprId>,
        patterns: List<PatId>,
        rest: Option<Sym>,
    },
    MatchClass {
        cls: ExprId,
        patterns: List<PatId>,
        kwd_attrs: List<Sym>,
        kwd_patterns: List<PatId>,
    },
    MatchStar {
        name: Option<Sym>,
    },
    MatchAs {
        pattern: Option<PatId>,
        name: Option<Sym>,
    },
    MatchOr {
        patterns: List<PatId>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pattern {
    pub range: Range,
    pub kind: PatternKind,
}

/// A parsed module (or expression: `body` is then one `Expr` statement).
pub struct Module<'s> {
    pub source: &'s str,
    pub body: List<StmtId>,
    pub exprs: Vec<Expr>,
    pub stmts: Vec<Stmt>,
    pub patterns: Vec<Pattern>,
    pub arguments: Vec<Arguments>,
    pub expr_lists: Vec<ExprId>,
    pub opt_expr_lists: Vec<Option<ExprId>>,
    pub stmt_lists: Vec<StmtId>,
    pub pat_lists: Vec<PatId>,
    pub sym_lists: Vec<Sym>,
    pub cmp_ops: Vec<CmpOp>,
    pub keywords: Vec<Keyword>,
    pub comprehensions: Vec<Comprehension>,
    pub params: Vec<Param>,
    pub with_items: Vec<WithItem>,
    pub handlers: Vec<ExceptHandler>,
    pub aliases: Vec<Alias>,
    pub match_cases: Vec<MatchCase>,
    pub type_params: Vec<TypeParam>,
    pub names: Interner<'s>,
    /// Decoded string contents.
    pub str_data: String,
    /// Decoded bytes contents.
    pub bytes_data: Vec<u8>,
    /// The byte offset each line starts at (line 1 at index 0).
    pub line_starts: Vec<u32>,
    /// The offset of `source`'s first byte.
    pub base: u32,
    /// Where the last token ends.
    pub end: u32,
}

/// Lists of the module's side tables.
pub trait ListOf<T> {
    fn items(&self, list: List<T>) -> &[T];
}

macro_rules! list_of {
    ($t:ty, $field:ident) => {
        impl ListOf<$t> for Module<'_> {
            #[inline]
            fn items(&self, list: List<$t>) -> &[$t] {
                &self.$field[list.range()]
            }
        }
    };
}

list_of!(ExprId, expr_lists);
list_of!(Option<ExprId>, opt_expr_lists);
list_of!(StmtId, stmt_lists);
list_of!(PatId, pat_lists);
list_of!(Sym, sym_lists);
list_of!(CmpOp, cmp_ops);
list_of!(Keyword, keywords);
list_of!(Comprehension, comprehensions);
list_of!(Param, params);
list_of!(WithItem, with_items);
list_of!(ExceptHandler, handlers);
list_of!(Alias, aliases);
list_of!(MatchCase, match_cases);
list_of!(TypeParam, type_params);

impl<'s> Module<'s> {
    #[inline]
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.index()]
    }
    #[inline]
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[id.index()]
    }
    #[inline]
    pub fn pattern(&self, id: PatId) -> &Pattern {
        &self.patterns[id.index()]
    }
    #[inline]
    pub fn arguments(&self, id: ArgsId) -> &Arguments {
        &self.arguments[id.index()]
    }
    #[inline]
    pub fn list<T>(&self, list: List<T>) -> &[T]
    where
        Self: ListOf<T>,
    {
        self.items(list)
    }
    #[inline]
    pub fn name(&self, sym: Sym) -> &str {
        self.names.get(sym)
    }
    #[inline]
    pub fn str(&self, value: StrRef) -> &str {
        &self.str_data[value.start as usize..(value.start + value.len) as usize]
    }
    #[inline]
    pub fn bytes(&self, value: StrRef) -> &[u8] {
        &self.bytes_data[value.start as usize..(value.start + value.len) as usize]
    }
    /// The source text of a range.
    pub fn text(&self, range: Range) -> &'s str {
        &self.source[(range.start - self.base) as usize..(range.end - self.base) as usize]
    }
    /// The 1-based line and 0-based byte column of an offset.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line = self.line_starts.partition_point(|&start| start <= offset);
        (line as u32, offset - self.line_starts[line - 1])
    }
    /// A number literal's text.
    pub fn number_text(&self, text: Span) -> &'s str {
        let start = (text.start - self.base) as usize;
        &self.source[start..start + text.len as usize]
    }
    /// An int literal's digits (underscores removed) and radix.
    pub fn int_digits(&self, radix: u8, text: Span) -> (String, u32) {
        let text = self.number_text(text);
        let digits = if radix == 10 { text } else { &text[2..] };
        (digits.chars().filter(|&c| c != '_').collect(), radix as u32)
    }
    /// A float literal's value.
    pub fn float_value(&self, text: Span) -> f64 {
        crate::numbers::float(self.number_text(text))
    }
    /// An imaginary literal's imaginary part.
    pub fn complex_value(&self, text: Span) -> f64 {
        let text = self.number_text(text);
        crate::numbers::float(&text[..text.len() - 1])
    }
}
