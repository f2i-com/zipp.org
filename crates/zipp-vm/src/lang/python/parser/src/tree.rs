//! The tree ZIPP's emitter walks: the arena AST ([`crate::ast`]) laid out as
//! borrowed nodes in a bump arena, in the shape (enum and struct names,
//! field names, node ranges) of the RustPython 0.4 AST the emitter was
//! written against. Children are `&'a` references, lists are [`Seq`]s of
//! nodes, names and string constants are `&'a str`; every node is `Copy`,
//! and the whole tree is freed at once with its [`Bump`].
//!
//! [`build`] lays a parsed module out (iteratively for expressions, so a
//! million-link chain builds on a small stack). A number literal keeps its
//! text; [`IntLit`] prints its decimal value.

use crate::ast::{self as a, ExprId, ExprKind, Module, PatternKind, StmtKind};
use crate::lexer::Error;
pub use bumpalo::Bump;
use std::fmt;

/// Byte offsets `start..end` in the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextRange {
    start: u32,
    end: u32,
}

impl TextRange {
    #[inline]
    pub fn new(start: u32, end: u32) -> TextRange {
        TextRange { start, end }
    }
    #[inline]
    pub fn start(&self) -> u32 {
        self.start
    }
    #[inline]
    pub fn end(&self) -> u32 {
        self.end
    }
}

/// A node with a source range.
pub trait Ranged {
    fn range(&self) -> TextRange;
    fn start(&self) -> u32 {
        self.range().start()
    }
    fn end(&self) -> u32 {
        self.range().end()
    }
}

impl<T: Ranged + ?Sized> Ranged for &T {
    #[inline]
    fn range(&self) -> TextRange {
        (**self).range()
    }
}

/// A list of nodes in the arena.
pub struct Seq<'a, T>(&'a [T]);

impl<T> Clone for Seq<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Seq<'_, T> {}

impl<T: fmt::Debug> fmt::Debug for Seq<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'a, T> Seq<'a, T> {
    pub const EMPTY: Seq<'a, T> = Seq(&[]);
    #[inline]
    pub fn as_slice(&self) -> &'a [T] {
        self.0
    }
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'a, T> {
        self.0.iter()
    }
}

impl<T> std::ops::Deref for Seq<'_, T> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        self.0
    }
}

impl<'a, T> IntoIterator for Seq<'a, T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, T> IntoIterator for &Seq<'a, T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// A name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identifier<'a>(&'a str);

impl<'a> Identifier<'a> {
    #[inline]
    pub fn as_str(&self) -> &'a str {
        self.0
    }
}

impl std::ops::Deref for Identifier<'_> {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.0
    }
}

impl AsRef<str> for Identifier<'_> {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl PartialEq<str> for Identifier<'_> {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Identifier<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for Identifier<'_> {
    fn eq(&self, other: &String) -> bool {
        self.0 == other
    }
}

impl fmt::Display for Identifier<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Identifier<'_>> for String {
    fn from(id: Identifier<'_>) -> String {
        id.0.to_owned()
    }
}

/// A str constant's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Str<'a>(&'a str);

impl<'a> Str<'a> {
    #[inline]
    pub fn as_str(&self) -> &'a str {
        self.0
    }
}

impl std::ops::Deref for Str<'_> {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.0
    }
}

impl AsRef<str> for Str<'_> {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl PartialEq<str> for Str<'_> {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Str<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl fmt::Display for Str<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A small unsigned int (an `import`'s relative level).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Int(u32);

impl Int {
    pub fn new(i: u32) -> Int {
        Int(i)
    }
    pub fn to_u32(&self) -> u32 {
        self.0
    }
    pub fn to_usize(&self) -> usize {
        self.0 as usize
    }
}

/// An int literal as written (`0x_ff`, `1_000`, `00`); displays as its
/// decimal value (any size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntLit<'a> {
    text: &'a str,
    radix: u8,
}

impl<'a> IntLit<'a> {
    /// The literal's text (prefix and underscores included).
    pub fn text(&self) -> &'a str {
        self.text
    }
    /// The value, when it fits a `u64`.
    pub fn to_u64(&self) -> Option<u64> {
        crate::numbers::small_int(self.text, self.radix)
    }
}

impl fmt::Display for IntLit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_u64() {
            Some(v) => v.fmt(f),
            None => f.write_str(&crate::numbers_display::int_decimal(
                self.text,
                self.radix as u32,
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Constant<'a> {
    None,
    Bool(bool),
    Str(Str<'a>),
    Bytes(&'a [u8]),
    Int(IntLit<'a>),
    /// Never built by the parser (kept for exhaustive matches).
    Tuple(Seq<'a, Constant<'a>>),
    Float(f64),
    Complex {
        real: f64,
        imag: f64,
    },
    Ellipsis,
}

impl PartialEq for Seq<'_, Constant<'_>> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExprContext {
    Load,
    Store,
    Del,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoolOp {
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Invert,
    Not,
    UAdd,
    USub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// An f-string replacement field's `!s`, `!r` or `!a`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConversionFlag {
    None,
    Str,
    Ascii,
    Repr,
}

impl ConversionFlag {
    pub fn to_byte(&self) -> Option<u8> {
        match self {
            ConversionFlag::None => None,
            ConversionFlag::Str => Some(b's'),
            ConversionFlag::Ascii => Some(b'a'),
            ConversionFlag::Repr => Some(b'r'),
        }
    }
    pub fn to_char(&self) -> Option<char> {
        Some(self.to_byte()? as char)
    }
}

/// Node structs: `Copy`, `Debug`, with a `range` field and [`Ranged`].
macro_rules! nodes {
    ($($(#[$doc:meta])* pub struct $name:ident<'a> { $(pub $field:ident: $ty:ty,)* })*) => {
        $(
            $(#[$doc])*
            #[derive(Clone, Copy, Debug)]
            pub struct $name<'a> {
                $(pub $field: $ty,)*
                pub range: TextRange,
            }
            impl Ranged for $name<'_> {
                #[inline]
                fn range(&self) -> TextRange {
                    self.range
                }
            }
        )*
    };
}

/// Node enums over node structs, with [`Ranged`] and `From` each struct.
macro_rules! node_enums {
    ($(pub enum $name:ident<'a> { $($variant:ident($ty:ident<'a>),)* })*) => {
        $(
            #[derive(Clone, Copy, Debug)]
            pub enum $name<'a> {
                $($variant($ty<'a>),)*
            }
            impl Ranged for $name<'_> {
                #[inline]
                fn range(&self) -> TextRange {
                    match self {
                        $($name::$variant(node) => node.range,)*
                    }
                }
            }
            $(
                impl<'a> From<$ty<'a>> for $name<'a> {
                    #[inline]
                    fn from(node: $ty<'a>) -> $name<'a> {
                        $name::$variant(node)
                    }
                }
            )*
        )*
    };
}

node_enums! {
    pub enum Expr<'a> {
        BoolOp(ExprBoolOp<'a>),
        NamedExpr(ExprNamedExpr<'a>),
        BinOp(ExprBinOp<'a>),
        UnaryOp(ExprUnaryOp<'a>),
        Lambda(ExprLambda<'a>),
        IfExp(ExprIfExp<'a>),
        Dict(ExprDict<'a>),
        Set(ExprSet<'a>),
        ListComp(ExprListComp<'a>),
        SetComp(ExprSetComp<'a>),
        DictComp(ExprDictComp<'a>),
        GeneratorExp(ExprGeneratorExp<'a>),
        Await(ExprAwait<'a>),
        Yield(ExprYield<'a>),
        YieldFrom(ExprYieldFrom<'a>),
        Compare(ExprCompare<'a>),
        Call(ExprCall<'a>),
        FormattedValue(ExprFormattedValue<'a>),
        JoinedStr(ExprJoinedStr<'a>),
        Constant(ExprConstant<'a>),
        Attribute(ExprAttribute<'a>),
        Subscript(ExprSubscript<'a>),
        Starred(ExprStarred<'a>),
        Name(ExprName<'a>),
        List(ExprList<'a>),
        Tuple(ExprTuple<'a>),
        Slice(ExprSlice<'a>),
    }

    pub enum Stmt<'a> {
        FunctionDef(StmtFunctionDef<'a>),
        AsyncFunctionDef(StmtAsyncFunctionDef<'a>),
        ClassDef(StmtClassDef<'a>),
        Return(StmtReturn<'a>),
        Delete(StmtDelete<'a>),
        Assign(StmtAssign<'a>),
        TypeAlias(StmtTypeAlias<'a>),
        AugAssign(StmtAugAssign<'a>),
        AnnAssign(StmtAnnAssign<'a>),
        For(StmtFor<'a>),
        AsyncFor(StmtAsyncFor<'a>),
        While(StmtWhile<'a>),
        If(StmtIf<'a>),
        With(StmtWith<'a>),
        AsyncWith(StmtAsyncWith<'a>),
        Match(StmtMatch<'a>),
        Raise(StmtRaise<'a>),
        Try(StmtTry<'a>),
        TryStar(StmtTryStar<'a>),
        Assert(StmtAssert<'a>),
        Import(StmtImport<'a>),
        ImportFrom(StmtImportFrom<'a>),
        Global(StmtGlobal<'a>),
        Nonlocal(StmtNonlocal<'a>),
        Expr(StmtExpr<'a>),
        Pass(StmtPass<'a>),
        Break(StmtBreak<'a>),
        Continue(StmtContinue<'a>),
    }

    pub enum Pattern<'a> {
        MatchValue(PatternMatchValue<'a>),
        MatchSingleton(PatternMatchSingleton<'a>),
        MatchSequence(PatternMatchSequence<'a>),
        MatchMapping(PatternMatchMapping<'a>),
        MatchClass(PatternMatchClass<'a>),
        MatchStar(PatternMatchStar<'a>),
        MatchAs(PatternMatchAs<'a>),
        MatchOr(PatternMatchOr<'a>),
    }

    pub enum ExceptHandler<'a> {
        ExceptHandler(ExceptHandlerExceptHandler<'a>),
    }

    pub enum TypeParam<'a> {
        TypeVar(TypeParamTypeVar<'a>),
        ParamSpec(TypeParamParamSpec<'a>),
        TypeVarTuple(TypeParamTypeVarTuple<'a>),
    }
}

impl<'a> AsRef<Expr<'a>> for Expr<'a> {
    #[inline]
    fn as_ref(&self) -> &Expr<'a> {
        self
    }
}

impl<'a> AsRef<Pattern<'a>> for Pattern<'a> {
    #[inline]
    fn as_ref(&self) -> &Pattern<'a> {
        self
    }
}

type E<'a> = &'a Expr<'a>;
type OptE<'a> = Option<&'a Expr<'a>>;

nodes! {
    pub struct ExprBoolOp<'a> { pub op: BoolOp, pub values: Seq<'a, Expr<'a>>, }
    pub struct ExprNamedExpr<'a> { pub target: E<'a>, pub value: E<'a>, }
    pub struct ExprBinOp<'a> { pub left: E<'a>, pub op: Operator, pub right: E<'a>, }
    pub struct ExprUnaryOp<'a> { pub op: UnaryOp, pub operand: E<'a>, }
    pub struct ExprLambda<'a> { pub args: &'a Arguments<'a>, pub body: E<'a>, }
    pub struct ExprIfExp<'a> { pub test: E<'a>, pub body: E<'a>, pub orelse: E<'a>, }
    pub struct ExprDict<'a> { pub keys: Seq<'a, Option<Expr<'a>>>, pub values: Seq<'a, Expr<'a>>, }
    pub struct ExprSet<'a> { pub elts: Seq<'a, Expr<'a>>, }
    pub struct ExprListComp<'a> { pub elt: E<'a>, pub generators: Seq<'a, Comprehension<'a>>, }
    pub struct ExprSetComp<'a> { pub elt: E<'a>, pub generators: Seq<'a, Comprehension<'a>>, }
    pub struct ExprDictComp<'a> { pub key: E<'a>, pub value: E<'a>, pub generators: Seq<'a, Comprehension<'a>>, }
    pub struct ExprGeneratorExp<'a> { pub elt: E<'a>, pub generators: Seq<'a, Comprehension<'a>>, }
    pub struct ExprAwait<'a> { pub value: E<'a>, }
    pub struct ExprYield<'a> { pub value: OptE<'a>, }
    pub struct ExprYieldFrom<'a> { pub value: E<'a>, }
    pub struct ExprCompare<'a> { pub left: E<'a>, pub ops: Seq<'a, CmpOp>, pub comparators: Seq<'a, Expr<'a>>, }
    pub struct ExprCall<'a> { pub func: E<'a>, pub args: Seq<'a, Expr<'a>>, pub keywords: Seq<'a, Keyword<'a>>, }
    pub struct ExprFormattedValue<'a> { pub value: E<'a>, pub conversion: ConversionFlag, pub format_spec: OptE<'a>, }
    pub struct ExprJoinedStr<'a> { pub values: Seq<'a, Expr<'a>>, }
    pub struct ExprConstant<'a> { pub value: Constant<'a>, pub kind: Option<&'a str>, }
    pub struct ExprAttribute<'a> { pub value: E<'a>, pub attr: Identifier<'a>, pub ctx: ExprContext, }
    pub struct ExprSubscript<'a> { pub value: E<'a>, pub slice: E<'a>, pub ctx: ExprContext, }
    pub struct ExprStarred<'a> { pub value: E<'a>, pub ctx: ExprContext, }
    pub struct ExprName<'a> { pub id: Identifier<'a>, pub ctx: ExprContext, }
    pub struct ExprList<'a> { pub elts: Seq<'a, Expr<'a>>, pub ctx: ExprContext, }
    pub struct ExprTuple<'a> { pub elts: Seq<'a, Expr<'a>>, pub ctx: ExprContext, }
    pub struct ExprSlice<'a> { pub lower: OptE<'a>, pub upper: OptE<'a>, pub step: OptE<'a>, }

    pub struct StmtFunctionDef<'a> {
        pub name: Identifier<'a>,
        pub args: &'a Arguments<'a>,
        pub body: Seq<'a, Stmt<'a>>,
        pub decorator_list: Seq<'a, Expr<'a>>,
        pub returns: OptE<'a>,
        pub type_params: Seq<'a, TypeParam<'a>>,
    }
    pub struct StmtAsyncFunctionDef<'a> {
        pub name: Identifier<'a>,
        pub args: &'a Arguments<'a>,
        pub body: Seq<'a, Stmt<'a>>,
        pub decorator_list: Seq<'a, Expr<'a>>,
        pub returns: OptE<'a>,
        pub type_params: Seq<'a, TypeParam<'a>>,
    }
    pub struct StmtClassDef<'a> {
        pub name: Identifier<'a>,
        pub bases: Seq<'a, Expr<'a>>,
        pub keywords: Seq<'a, Keyword<'a>>,
        pub body: Seq<'a, Stmt<'a>>,
        pub decorator_list: Seq<'a, Expr<'a>>,
        pub type_params: Seq<'a, TypeParam<'a>>,
    }
    pub struct StmtReturn<'a> { pub value: OptE<'a>, }
    pub struct StmtDelete<'a> { pub targets: Seq<'a, Expr<'a>>, }
    pub struct StmtAssign<'a> { pub targets: Seq<'a, Expr<'a>>, pub value: E<'a>, }
    pub struct StmtTypeAlias<'a> { pub name: E<'a>, pub type_params: Seq<'a, TypeParam<'a>>, pub value: E<'a>, }
    pub struct StmtAugAssign<'a> { pub target: E<'a>, pub op: Operator, pub value: E<'a>, }
    pub struct StmtAnnAssign<'a> { pub target: E<'a>, pub annotation: E<'a>, pub value: OptE<'a>, pub simple: bool, }
    pub struct StmtFor<'a> { pub target: E<'a>, pub iter: E<'a>, pub body: Seq<'a, Stmt<'a>>, pub orelse: Seq<'a, Stmt<'a>>, }
    pub struct StmtAsyncFor<'a> { pub target: E<'a>, pub iter: E<'a>, pub body: Seq<'a, Stmt<'a>>, pub orelse: Seq<'a, Stmt<'a>>, }
    pub struct StmtWhile<'a> { pub test: E<'a>, pub body: Seq<'a, Stmt<'a>>, pub orelse: Seq<'a, Stmt<'a>>, }
    pub struct StmtIf<'a> { pub test: E<'a>, pub body: Seq<'a, Stmt<'a>>, pub orelse: Seq<'a, Stmt<'a>>, }
    pub struct StmtWith<'a> { pub items: Seq<'a, WithItem<'a>>, pub body: Seq<'a, Stmt<'a>>, }
    pub struct StmtAsyncWith<'a> { pub items: Seq<'a, WithItem<'a>>, pub body: Seq<'a, Stmt<'a>>, }
    pub struct StmtMatch<'a> { pub subject: E<'a>, pub cases: Seq<'a, MatchCase<'a>>, }
    pub struct StmtRaise<'a> { pub exc: OptE<'a>, pub cause: OptE<'a>, }
    pub struct StmtTry<'a> {
        pub body: Seq<'a, Stmt<'a>>,
        pub handlers: Seq<'a, ExceptHandler<'a>>,
        pub orelse: Seq<'a, Stmt<'a>>,
        pub finalbody: Seq<'a, Stmt<'a>>,
    }
    pub struct StmtTryStar<'a> {
        pub body: Seq<'a, Stmt<'a>>,
        pub handlers: Seq<'a, ExceptHandler<'a>>,
        pub orelse: Seq<'a, Stmt<'a>>,
        pub finalbody: Seq<'a, Stmt<'a>>,
    }
    pub struct StmtAssert<'a> { pub test: E<'a>, pub msg: OptE<'a>, }
    pub struct StmtImport<'a> { pub names: Seq<'a, Alias<'a>>, }
    pub struct StmtImportFrom<'a> { pub module: Option<Identifier<'a>>, pub names: Seq<'a, Alias<'a>>, pub level: Option<Int>, }
    pub struct StmtGlobal<'a> { pub names: Seq<'a, Identifier<'a>>, }
    pub struct StmtNonlocal<'a> { pub names: Seq<'a, Identifier<'a>>, }
    pub struct StmtExpr<'a> { pub value: E<'a>, }
    pub struct StmtPass<'a> { pub _p: std::marker::PhantomData<&'a ()>, }
    pub struct StmtBreak<'a> { pub _p: std::marker::PhantomData<&'a ()>, }
    pub struct StmtContinue<'a> { pub _p: std::marker::PhantomData<&'a ()>, }

    pub struct PatternMatchValue<'a> { pub value: E<'a>, }
    pub struct PatternMatchSingleton<'a> { pub value: Constant<'a>, }
    pub struct PatternMatchSequence<'a> { pub patterns: Seq<'a, Pattern<'a>>, }
    pub struct PatternMatchMapping<'a> {
        pub keys: Seq<'a, Expr<'a>>,
        pub patterns: Seq<'a, Pattern<'a>>,
        pub rest: Option<Identifier<'a>>,
    }
    pub struct PatternMatchClass<'a> {
        pub cls: E<'a>,
        pub patterns: Seq<'a, Pattern<'a>>,
        pub kwd_attrs: Seq<'a, Identifier<'a>>,
        pub kwd_patterns: Seq<'a, Pattern<'a>>,
    }
    pub struct PatternMatchStar<'a> { pub name: Option<Identifier<'a>>, }
    pub struct PatternMatchAs<'a> { pub pattern: Option<&'a Pattern<'a>>, pub name: Option<Identifier<'a>>, }
    pub struct PatternMatchOr<'a> { pub patterns: Seq<'a, Pattern<'a>>, }

    pub struct ExceptHandlerExceptHandler<'a> { pub type_: OptE<'a>, pub name: Option<Identifier<'a>>, pub body: Seq<'a, Stmt<'a>>, }

    /// `default`: a PEP 696 default (`T = int`).
    pub struct TypeParamTypeVar<'a> { pub name: Identifier<'a>, pub bound: OptE<'a>, pub default: OptE<'a>, }
    pub struct TypeParamParamSpec<'a> { pub name: Identifier<'a>, pub default: OptE<'a>, }
    pub struct TypeParamTypeVarTuple<'a> { pub name: Identifier<'a>, pub default: OptE<'a>, }

    pub struct Arguments<'a> {
        pub posonlyargs: Seq<'a, ArgWithDefault<'a>>,
        pub args: Seq<'a, ArgWithDefault<'a>>,
        pub vararg: Option<&'a Arg<'a>>,
        pub kwonlyargs: Seq<'a, ArgWithDefault<'a>>,
        pub kwarg: Option<&'a Arg<'a>>,
    }
    pub struct ArgWithDefault<'a> { pub def: Arg<'a>, pub default: OptE<'a>, }
    pub struct Arg<'a> { pub arg: Identifier<'a>, pub annotation: OptE<'a>, }
    pub struct Keyword<'a> { pub arg: Option<Identifier<'a>>, pub value: Expr<'a>, }
    pub struct Alias<'a> { pub name: Identifier<'a>, pub asname: Option<Identifier<'a>>, }
    pub struct WithItem<'a> { pub context_expr: Expr<'a>, pub optional_vars: OptE<'a>, }
    pub struct MatchCase<'a> { pub pattern: Pattern<'a>, pub guard: OptE<'a>, pub body: Seq<'a, Stmt<'a>>, }
    pub struct Comprehension<'a> { pub target: Expr<'a>, pub iter: Expr<'a>, pub ifs: Seq<'a, Expr<'a>>, pub is_async: bool, }
}

mod build;
pub use build::build;
