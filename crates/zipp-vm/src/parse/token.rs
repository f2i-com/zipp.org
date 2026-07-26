//! Tokens.
//!
//! Two things here exist because this lexer serves an ENGINE rather than a
//! tool, and both are conformance, not taste:
//!
//! - **Keywords are not their own token kind.** Nearly every JS "keyword" is
//!   contextual (`of`, `async`, `let`, `static`, `get`, `set`, `as`, `from`,
//!   `yield`, `await`), and which ones are reserved depends on strictness and on
//!   whether we are inside a generator or async function — none of which the
//!   lexer knows. So an identifier lexes as [`TokenKind::Ident`] carrying a
//!   [`Keyword`] classification, and the parser decides what it means.
//!
//! - **An identifier records whether it contained a unicode escape.** `await`
//!   is spelled with an escape, so it is NOT the keyword `await`, and using it
//!   where a keyword is required is a SyntaxError. Losing that bit silently
//!   accepts a family of programs the spec rejects.

use std::fmt;

/// A half-open byte range into the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }
}

/// A cooked string value.
///
/// JS strings are sequences of UTF-16 code units and may contain LONE
/// SURROGATES — `"\uD800"` is a legal, observable one-unit string. Rust's
/// `String` cannot hold one, which is why the engine currently carries a
/// parallel WTF-8 buffer (`compile/mod.rs` `exact_src`) just to recover escaped
/// regex source. Representing the rare case explicitly removes the need for it,
/// while the overwhelmingly common well-formed case stays a plain `String` and
/// costs nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrVal {
    /// Well-formed UTF-8; the common case.
    Utf8(String),
    /// Contains at least one lone surrogate, so it is only expressible as raw
    /// UTF-16 code units.
    Utf16(Vec<u16>),
}

impl StrVal {
    /// Build from UTF-16 units, choosing the cheap representation when possible.
    pub fn from_utf16(units: Vec<u16>) -> StrVal {
        match String::from_utf16(&units) {
            Ok(s) => StrVal::Utf8(s),
            Err(_) => StrVal::Utf16(units),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            StrVal::Utf8(s) => s.is_empty(),
            StrVal::Utf16(u) => u.is_empty(),
        }
    }

    /// The UTF-8 view, lossy for lone surrogates. For comparisons and for the
    /// many places that legitimately only handle well-formed text.
    pub fn to_lossy_string(&self) -> String {
        match self {
            StrVal::Utf8(s) => s.clone(),
            StrVal::Utf16(u) => String::from_utf16_lossy(u),
        }
    }
}

impl fmt::Display for StrVal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_lossy_string())
    }
}

/// A numeric literal's value plus how it was written, because the spelling is
/// observable: legacy octal (`0123`) and legacy octal-like decimals (`08`) are
/// SyntaxErrors in strict mode, and only the parser knows the strictness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumLit {
    pub value: f64,
    pub kind: NumKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumKind {
    /// `1`, `1.5`, `1e3`, `.5`
    Decimal,
    /// `0x1f`, `0o17`, `0b11` — the modern prefixed forms.
    Prefixed,
    /// `0123` — Annex B legacy octal. Strict-mode SyntaxError.
    LegacyOctal,
    /// `08`, `09` — a leading zero followed by a digit that is not octal, so it
    /// is decimal by Annex B. Also a strict-mode SyntaxError.
    NonOctalDecimal,
}

/// Which reserved-word-ish identifier this is, if any. Classification only —
/// whether it is actually reserved depends on context the parser owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    /// Not any known keyword.
    None,
    // Always reserved.
    Break, Case, Catch, Class, Const, Continue, Debugger, Default, Delete, Do,
    Else, Enum, Export, Extends, False, Finally, For, Function, If, Import, In,
    Instanceof, New, Null, Return, Super, Switch, This, Throw, True, Try,
    Typeof, Var, Void, While, With,
    // Reserved only in strict mode.
    Implements, Interface, Let, Package, Private, Protected, Public, Static, Yield,
    // Contextual: meaning depends entirely on position.
    Async, Await, As, From, Get, Of, Set, Target, Meta, Accessor,
}

impl Keyword {
    /// Classify an identifier spelling. Called once per identifier token, so it
    /// is a match on length then bytes rather than a hash lookup.
    pub fn classify(s: &str) -> Keyword {
        use Keyword::*;
        match s.len() {
            2 => match s {
                "do" => Do, "if" => If, "in" => In, "as" => As, "of" => Of,
                _ => None,
            },
            3 => match s {
                "for" => For, "new" => New, "try" => Try, "var" => Var,
                "let" => Let, "get" => Get, "set" => Set,
                _ => None,
            },
            4 => match s {
                "case" => Case, "else" => Else, "enum" => Enum, "from" => From,
                "meta" => Meta, "null" => Null, "this" => This, "true" => True,
                "void" => Void, "with" => With,
                _ => None,
            },
            5 => match s {
                "async" => Async, "await" => Await, "break" => Break,
                "catch" => Catch, "class" => Class, "const" => Const,
                "false" => False, "super" => Super, "throw" => Throw,
                "while" => While, "yield" => Yield,
                _ => None,
            },
            6 => match s {
                "delete" => Delete, "export" => Export, "import" => Import,
                "public" => Public, "return" => Return, "static" => Static,
                "switch" => Switch, "target" => Target, "typeof" => Typeof,
                _ => None,
            },
            7 => match s {
                "default" => Default, "extends" => Extends, "finally" => Finally,
                "package" => Package, "private" => Private,
                _ => None,
            },
            8 => match s {
                "continue" => Continue, "debugger" => Debugger, "function" => Function,
                "accessor" => Accessor,
                _ => None,
            },
            9 => match s {
                "interface" => Interface, "protected" => Protected,
                _ => None,
            },
            10 => match s {
                "implements" => Implements, "instanceof" => Instanceof,
                _ => None,
            },
            _ => None,
        }
    }

    /// Reserved in ALL contexts — never usable as a binding identifier.
    pub fn is_always_reserved(self) -> bool {
        use Keyword::*;
        matches!(
            self,
            Break | Case | Catch | Class | Const | Continue | Debugger | Default
                | Delete | Do | Else | Enum | Export | Extends | False | Finally
                | For | Function | If | Import | In | Instanceof | New | Null
                | Return | Super | Switch | This | Throw | True | Try | Typeof
                | Var | Void | While | With
        )
    }

    /// Reserved additionally in strict mode.
    pub fn is_strict_reserved(self) -> bool {
        use Keyword::*;
        matches!(
            self,
            Implements | Interface | Let | Package | Private | Protected | Public
                | Static | Yield
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// End of input.
    Eof,
    /// An identifier, private name, or keyword-shaped word.
    Ident {
        name: String,
        kw: Keyword,
        /// The spelling used a `\uXXXX` escape, so it cannot act as a keyword
        /// even when it reads like one (`await` is not `await`).
        had_escape: bool,
        /// `#name` — a class private name.
        private: bool,
    },
    Num(NumLit),
    BigInt(String),
    Str(StrVal),
    /// A regular expression literal: the pattern's exact source and its flags.
    /// The pattern is kept as written (escapes uninterpreted) because that is
    /// what the regex engine compiles.
    Regex { pattern: StrVal, flags: String },
    /// One piece of a template literal. `head` is true for the first chunk,
    /// `tail` for the last; a `\`...\`` with no substitutions is both.
    Template {
        /// The cooked value. `None` when the escape sequence is invalid, which
        /// is legal in a TAGGED template (the cooked value is `undefined`) and
        /// a SyntaxError otherwise — so the parser needs to know, not the lexer.
        cooked: Option<StrVal>,
        /// The raw source between the delimiters, for `String.raw`.
        raw: String,
        head: bool,
        tail: bool,
    },
    Punct(Punct),
}

/// Punctuators, including the compound-assignment forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Punct {
    LBrace, RBrace, LParen, RParen, LBracket, RBracket,
    Semi, Comma, Dot, DotDotDot, Colon, Question,
    /// `?.` — distinguished from `?` `.` because `a?.5:b` is a conditional.
    QuestionDot, QuestionQuestion, QuestionQuestionEq,
    Arrow,
    Lt, Gt, LtEq, GtEq,
    EqEq, NotEq, EqEqEq, NotEqEq,
    Plus, Minus, Star, Slash, Percent, StarStar,
    PlusPlus, MinusMinus,
    Shl, Shr, UShr,
    Amp, Pipe, Caret, Bang, Tilde,
    AmpAmp, PipePipe,
    Eq,
    PlusEq, MinusEq, StarEq, SlashEq, PercentEq, StarStarEq,
    ShlEq, ShrEq, UShrEq, AmpEq, PipeEq, CaretEq,
    AmpAmpEq, PipePipeEq,
    /// `@` — not JS, but reserved so decorator syntax fails with a clear error.
    At,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// A LineTerminator appeared between the previous token and this one.
    /// Automatic semicolon insertion is defined in terms of this, and so are
    /// the no-LineTerminator-here restrictions (`return`/`throw`/`break`/
    /// `continue` operands, `++`/`--` postfix, `=>`, `async` before a function).
    pub newline_before: bool,
}

impl TokenKind {
    /// The punctuator this token is, if any. Lets the expression parser look up
    /// a precedence without matching the whole enum.
    pub fn as_punct(&self) -> Option<Punct> {
        match self {
            TokenKind::Punct(p) => Some(*p),
            _ => None,
        }
    }
}

impl Token {
    pub fn is_punct(&self, p: Punct) -> bool {
        matches!(self.kind, TokenKind::Punct(x) if x == p)
    }

    /// The identifier's spelling, if this is one.
    pub fn ident_name(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Ident { name, .. } => Some(name),
            _ => None,
        }
    }

    /// This token is the given keyword AND was spelled without escapes, which
    /// is what "is this the keyword" actually means.
    pub fn is_kw(&self, k: Keyword) -> bool {
        matches!(
            &self.kind,
            TokenKind::Ident { kw, had_escape: false, private: false, .. } if *kw == k
        )
    }
}
