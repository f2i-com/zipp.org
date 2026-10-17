//! Tokens: a kind, flags, a byte range into the source and one word of data
//! (an interned name, or the index of an f-string's end token). No token
//! owns memory; number and string values are read from the source when the
//! parser needs them.

/// The kind of a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum T {
    Name,
    Int,
    Float,
    Complex,
    /// A string or bytes literal (not an f-string).
    String,
    /// `f"`, `rf'''`, ...: an f-string's prefix and opening quotes. `data` is
    /// the index of its `FStringEnd`.
    FStringStart,
    /// Literal text inside an f-string (or a format spec).
    FStringMiddle,
    FStringEnd,
    /// An f-string this lexer could not tokenize by PEP 701's rules but the
    /// classic string rules delimit: the parser reports its error. `data`
    /// indexes `Lexed::errors` (the PEP 701 error, the fallback).
    FStringBroken,
    /// The `{` opening a replacement field.
    FieldStart,
    /// The `}` closing a replacement field.
    FieldEnd,
    /// The `!` before a conversion character.
    Exclamation,
    /// The `:` opening a format spec.
    FormatSpec,
    Newline,
    Indent,
    Dedent,
    EndOfFile,
    /// A lexical error; `data` indexes `Lexed::errors`. Always last.
    Error,
    Lpar,
    Rpar,
    Lsqb,
    Rsqb,
    Colon,
    Comma,
    Semi,
    Plus,
    Minus,
    Star,
    Slash,
    Vbar,
    Amper,
    Less,
    Greater,
    Equal,
    Dot,
    Percent,
    Lbrace,
    Rbrace,
    EqEqual,
    NotEqual,
    LessEqual,
    GreaterEqual,
    Tilde,
    CircumFlex,
    LeftShift,
    RightShift,
    DoubleStar,
    DoubleStarEqual,
    PlusEqual,
    MinusEqual,
    StarEqual,
    SlashEqual,
    PercentEqual,
    AmperEqual,
    VbarEqual,
    CircumflexEqual,
    LeftShiftEqual,
    RightShiftEqual,
    DoubleSlash,
    DoubleSlashEqual,
    ColonEqual,
    At,
    AtEqual,
    Rarrow,
    Ellipsis,
    False,
    None,
    True,
    And,
    As,
    Assert,
    Async,
    Await,
    Break,
    Class,
    Continue,
    Def,
    Del,
    Elif,
    Else,
    Except,
    Finally,
    For,
    From,
    Global,
    If,
    Import,
    In,
    Is,
    Lambda,
    Nonlocal,
    Not,
    Or,
    Pass,
    Raise,
    Return,
    Try,
    While,
    Match,
    Type,
    Case,
    With,
    Yield,
}

/// A string literal's prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StrKind {
    Plain = 0,
    F = 1,
    Bytes = 2,
    Raw = 3,
    RawF = 4,
    RawBytes = 5,
    Unicode = 6,
}

impl StrKind {
    pub fn from_bits(bits: u8) -> StrKind {
        match bits & 7 {
            0 => StrKind::Plain,
            1 => StrKind::F,
            2 => StrKind::Bytes,
            3 => StrKind::Raw,
            4 => StrKind::RawF,
            5 => StrKind::RawBytes,
            _ => StrKind::Unicode,
        }
    }
    pub fn is_raw(self) -> bool {
        matches!(self, StrKind::Raw | StrKind::RawF | StrKind::RawBytes)
    }
    pub fn is_bytes(self) -> bool {
        matches!(self, StrKind::Bytes | StrKind::RawBytes)
    }
    pub fn is_f(self) -> bool {
        matches!(self, StrKind::F | StrKind::RawF)
    }
    pub fn prefix_len(self) -> u32 {
        match self {
            StrKind::Plain => 0,
            StrKind::RawF | StrKind::RawBytes => 2,
            _ => 1,
        }
    }
    /// How a syntax error names the prefix.
    pub fn display(self) -> &'static str {
        match self {
            StrKind::Plain => "",
            StrKind::F => "f",
            StrKind::Bytes => "b",
            StrKind::Raw => "r",
            StrKind::RawF => "rf",
            StrKind::RawBytes => "rb",
            StrKind::Unicode => "u",
        }
    }
}

/// `Token::flags` of strings: the prefix kind in bits 0-2, triple quotes in bit 3.
pub const TRIPLE: u8 = 8;

/// A token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: T,
    pub flags: u8,
    pub start: u32,
    pub end: u32,
    pub data: u32,
}

impl Token {
    pub fn str_kind(&self) -> StrKind {
        StrKind::from_bits(self.flags)
    }
    pub fn triple(&self) -> bool {
        self.flags & TRIPLE != 0
    }
}

/// The keyword spelled `word`.
pub fn keyword(word: &[u8]) -> Option<T> {
    Some(match word {
        b"False" => T::False,
        b"None" => T::None,
        b"True" => T::True,
        b"and" => T::And,
        b"as" => T::As,
        b"assert" => T::Assert,
        b"async" => T::Async,
        b"await" => T::Await,
        b"break" => T::Break,
        b"case" => T::Case,
        b"class" => T::Class,
        b"continue" => T::Continue,
        b"def" => T::Def,
        b"del" => T::Del,
        b"elif" => T::Elif,
        b"else" => T::Else,
        b"except" => T::Except,
        b"finally" => T::Finally,
        b"for" => T::For,
        b"from" => T::From,
        b"global" => T::Global,
        b"if" => T::If,
        b"import" => T::Import,
        b"in" => T::In,
        b"is" => T::Is,
        b"lambda" => T::Lambda,
        b"match" => T::Match,
        b"nonlocal" => T::Nonlocal,
        b"not" => T::Not,
        b"or" => T::Or,
        b"pass" => T::Pass,
        b"raise" => T::Raise,
        b"return" => T::Return,
        b"try" => T::Try,
        b"type" => T::Type,
        b"while" => T::While,
        b"with" => T::With,
        b"yield" => T::Yield,
        _ => return None,
    })
}
