//! The lexer: bytes in, a `Vec<Token>` out, with no allocation per token.
//!
//! Outside f-strings it produces the token stream (and the lexical errors,
//! at the same offsets) of the RustPython 0.4 lexer ZIPP used before, so a
//! program is accepted, or rejected with the same message, as it was.
//! F-strings follow PEP 701 (Python 3.12): nested quotes, multi-line
//! replacement fields, nested format specs and `=` debugging text are
//! tokens. An f-string that PEP 701's rules cannot tokenize but the classic
//! string rules delimit becomes one `FStringBroken` token, whose error the
//! parser reports as the classic f-string parser did (see `parser::compat`).

use crate::intern::{self, Interner, Sym};
use crate::token::{keyword, StrKind, Token, T, TRIPLE};
use unicode_ident::{is_xid_continue, is_xid_start};
use unicode_properties::{EmojiStatus, UnicodeEmoji};

/// How the source is parsed: as a module, or as one expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Module,
    Interactive,
    Expression,
}

/// Bounds a caller can put on a module before it is parsed. When one is
/// crossed the token stream ends in an error with `limit` set.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_tokens: usize,
    pub max_brackets: u32,
    pub max_indent: u32,
}

impl Limits {
    pub const NONE: Limits = Limits {
        max_tokens: usize::MAX,
        max_brackets: u32::MAX,
        max_indent: u32::MAX,
    };
}

/// An error: what a syntax error says, and where (a byte offset).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    pub offset: u32,
    /// A [`Limits`] bound was crossed (the message is not user-facing).
    pub limit: bool,
}

impl Error {
    pub(crate) fn new(message: impl Into<String>, offset: u32) -> Error {
        Error {
            message: message.into(),
            offset,
            limit: false,
        }
    }
}

pub(crate) mod msg {
    pub const EOF: &str = "unexpected EOF while parsing";
    pub const STRING: &str = "Got unexpected string";
    pub const UNICODE: &str = "Got unexpected unicode";
    pub const NESTING: &str = "Got unexpected nesting";
    pub const INDENTATION: &str = "unindent does not match any outer indentation level";
    pub const TAB: &str = "inconsistent use of tabs and spaces in indentation";
    pub const TABS_AFTER_SPACES: &str = "Tabs not allowed as part of indentation after spaces";
    pub const LINE_CONTINUATION: &str = "unexpected character after line continuation character";
    pub const EOL: &str = "EOL while scanning string literal";
    pub const FSTRING_NESTING: &str = "too many nested f-strings";
    pub fn unrecognized(c: char) -> String {
        format!("Got unexpected token {c}")
    }
}

/// A lexed source.
pub struct Lexed<'s> {
    pub tokens: Vec<Token>,
    pub names: Interner<'s>,
    /// The final error (referenced by the `Error` token) and the PEP 701
    /// errors of `FStringBroken` tokens.
    pub errors: Vec<Error>,
}

/// Lex `source` as a program whose first byte is at offset `base`.
pub fn lex(source: &str, mode: Mode, base: u32, limits: Limits) -> Lexed<'_> {
    lex_into(source, mode, base, limits, Interner::new())
}

pub(crate) fn lex_into<'s>(
    source: &'s str,
    mode: Mode,
    base: u32,
    limits: Limits,
    names: Interner<'s>,
) -> Lexed<'s> {
    let mut lexer = Lexer {
        src: source,
        b: source.as_bytes(),
        pos: 0,
        base,
        nesting: 0,
        at_bol: true,
        indents: vec![(0, 0)],
        toks: Vec::with_capacity(source.len() / 4 + 8),
        names,
        errors: Vec::new(),
        limits,
        top_level: 0,
        brackets: 0,
        indent_depth: 0,
        fstring_depth: 0,
    };
    lexer.run();
    let mut lexed = Lexed {
        tokens: lexer.toks,
        names: lexer.names,
        errors: lexer.errors,
    };
    soft_keywords(&mut lexed.tokens, mode);
    lexed
}

type LResult<T> = Result<T, Error>;

/// F-strings nest fewer levels than this (CPython's `MAXFSTRINGLEVEL`).
const MAX_FSTRING_NESTING: u32 = 150;

struct Lexer<'s> {
    src: &'s str,
    b: &'s [u8],
    pos: usize,
    base: u32,
    /// Open brackets (and replacement fields): newlines inside are not logical.
    nesting: u32,
    at_bol: bool,
    /// Indentation levels as (tabs, spaces), never empty.
    indents: Vec<(u32, u32)>,
    toks: Vec<Token>,
    names: Interner<'s>,
    errors: Vec<Error>,
    limits: Limits,
    /// Tokens counted against the limits (an f-string counts once).
    top_level: usize,
    /// Brackets outside f-strings, for the limits.
    brackets: u32,
    indent_depth: u32,
    /// F-strings open around the current position (PEP 701 nests them).
    fstring_depth: u32,
}

impl<'s> Lexer<'s> {
    #[inline]
    fn at(&self, i: usize) -> u8 {
        *self.b.get(i).unwrap_or(&0)
    }

    #[inline]
    fn peek(&self, n: usize) -> u8 {
        self.at(self.pos + n)
    }

    #[inline]
    fn eof(&self) -> bool {
        self.pos >= self.b.len()
    }

    #[inline]
    fn off(&self, pos: usize) -> u32 {
        self.base + pos as u32
    }

    #[inline]
    fn err(&self, message: impl Into<String>, pos: usize) -> Error {
        Error::new(message, self.off(pos))
    }

    #[inline]
    fn push(&mut self, kind: T, start: usize, end: usize) {
        self.push_data(kind, 0, start, end, 0);
    }

    #[inline]
    fn push_data(&mut self, kind: T, flags: u8, start: usize, end: usize, data: u32) {
        self.toks.push(Token {
            kind,
            flags,
            start: self.off(start),
            end: self.off(end),
            data,
        });
    }

    fn run(&mut self) {
        if self.src.starts_with('\u{feff}') {
            self.pos = 3;
        }
        loop {
            // One step of the classic lexer: tokens it queued in a step that
            // then failed were never returned, only the error.
            let step_start = self.toks.len();
            let step = self.step(step_start);
            if step.is_ok() {
                if let Err((at, error)) = self.count(step_start) {
                    self.toks.truncate(at);
                    self.fail(error);
                    return;
                }
            }
            match step {
                Ok(true) => {}
                Ok(false) => {
                    let end = self.off(self.b.len());
                    self.toks.push(Token {
                        kind: T::EndOfFile,
                        flags: 0,
                        start: end,
                        end,
                        data: 0,
                    });
                    return;
                }
                Err(error) => {
                    self.toks.truncate(step_start);
                    self.fail(error);
                    return;
                }
            }
        }
    }

    fn fail(&mut self, error: Error) {
        let offset = error.offset;
        self.errors.push(error);
        self.toks.push(Token {
            kind: T::Error,
            flags: 0,
            start: offset,
            end: offset,
            data: self.errors.len() as u32 - 1,
        });
    }

    /// Count the tokens of a step against the limits (after the fact: the
    /// token crossing a bound is replaced by the error).
    fn count(&mut self, from: usize) -> Result<(), (usize, Error)> {
        let limits = self.limits;
        let mut i = from;
        while i < self.toks.len() {
            let tok = self.toks[i];
            self.top_level += 1;
            match tok.kind {
                T::Indent => self.indent_depth += 1,
                T::Dedent => self.indent_depth = self.indent_depth.saturating_sub(1),
                T::Lpar | T::Lsqb | T::Lbrace => self.brackets += 1,
                T::Rpar | T::Rsqb | T::Rbrace => self.brackets = self.brackets.saturating_sub(1),
                _ => {}
            }
            if self.top_level > limits.max_tokens
                || self.brackets > limits.max_brackets
                || self.indent_depth > limits.max_indent
            {
                let error = Error {
                    message: "compiler complexity limit".to_owned(),
                    offset: tok.start,
                    limit: true,
                };
                return Err((i, error));
            }
            if tok.kind == T::FStringStart {
                i = tok.data as usize;
            }
            i += 1;
        }
        Ok(())
    }

    /// Returns `false` once the end of the input has been tokenized.
    fn step(&mut self, step_start: usize) -> LResult<bool> {
        loop {
            if self.at_bol {
                self.indentation()?;
            }
            if !self.consume()? {
                return Ok(false);
            }
            if self.toks.len() > step_start {
                return Ok(true);
            }
        }
    }

    fn indentation(&mut self) -> LResult<()> {
        let (mut tabs, mut spaces) = (0u32, 0u32);
        loop {
            match self.peek(0) {
                b' ' => {
                    self.pos += 1;
                    spaces += 1;
                }
                b'\t' => {
                    if spaces != 0 {
                        return Err(self.err(msg::TABS_AFTER_SPACES, self.pos));
                    }
                    self.pos += 1;
                    tabs += 1;
                }
                b'#' => {
                    self.skip_comment();
                    tabs = 0;
                    spaces = 0;
                }
                b'\x0C' => {
                    self.pos += 1;
                    tabs = 0;
                    spaces = 0;
                }
                b'\n' | b'\r' => {
                    self.skip_newline();
                    tabs = 0;
                    spaces = 0;
                }
                _ if self.eof() => {
                    tabs = 0;
                    spaces = 0;
                    break;
                }
                _ => {
                    self.at_bol = false;
                    break;
                }
            }
        }
        if self.nesting != 0 {
            return Ok(());
        }
        let pos = self.pos;
        let current = *self.indents.last().unwrap();
        match compare(tabs, spaces, current).ok_or_else(|| self.err(msg::TAB, pos))? {
            std::cmp::Ordering::Equal => {}
            std::cmp::Ordering::Greater => {
                self.indents.push((tabs, spaces));
                self.push(T::Indent, pos - (spaces + tabs) as usize, pos);
            }
            std::cmp::Ordering::Less => loop {
                let current = *self.indents.last().unwrap();
                match compare(tabs, spaces, current).ok_or_else(|| self.err(msg::TAB, pos))? {
                    std::cmp::Ordering::Less => {
                        self.indents.pop();
                        self.push(T::Dedent, pos, pos);
                    }
                    std::cmp::Ordering::Equal => break,
                    std::cmp::Ordering::Greater => {
                        return Err(self.err(msg::INDENTATION, pos));
                    }
                }
            },
        }
        Ok(())
    }

    fn skip_comment(&mut self) {
        while !self.eof() && !matches!(self.peek(0), b'\n' | b'\r') {
            self.pos += 1;
        }
    }

    /// Skip one newline (`\r\n` is one).
    #[inline]
    fn skip_newline(&mut self) {
        if self.peek(0) == b'\r' && self.peek(1) == b'\n' {
            self.pos += 2;
        } else {
            self.pos += 1;
        }
    }

    /// The character at `pos` and its length.
    fn char_at(&self, pos: usize) -> (char, usize) {
        let c = self.src[pos..].chars().next().unwrap_or('\0');
        (c, c.len_utf8())
    }

    /// One dispatch of the classic lexer. Returns `false` at the end.
    fn consume(&mut self) -> LResult<bool> {
        if self.eof() {
            let pos = self.pos;
            if self.nesting > 0 {
                return Err(self.err(msg::EOF, pos));
            }
            if !self.at_bol {
                self.at_bol = true;
                self.push(T::Newline, pos, pos);
            }
            while self.indents.len() > 1 {
                self.indents.pop();
                self.push(T::Dedent, pos, pos);
            }
            return Ok(false);
        }
        self.token()?;
        Ok(true)
    }

    /// Lex one token (or skip whitespace, a comment, a line continuation).
    fn token(&mut self) -> LResult<()> {
        let start = self.pos;
        let c = self.peek(0);
        macro_rules! op {
            ($len:expr, $kind:expr) => {{
                self.pos += $len;
                self.push($kind, start, self.pos);
            }};
        }
        match c {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => return self.identifier(),
            b'0'..=b'9' => return self.number(),
            b'#' => self.skip_comment(),
            b'"' | b'\'' => return self.string(start, StrKind::Plain),
            b'=' => {
                if self.peek(1) == b'=' {
                    op!(2, T::EqEqual)
                } else {
                    op!(1, T::Equal)
                }
            }
            b'+' => {
                if self.peek(1) == b'=' {
                    op!(2, T::PlusEqual)
                } else {
                    op!(1, T::Plus)
                }
            }
            b'*' => match (self.peek(1), self.peek(2)) {
                (b'=', _) => op!(2, T::StarEqual),
                (b'*', b'=') => op!(3, T::DoubleStarEqual),
                (b'*', _) => op!(2, T::DoubleStar),
                _ => op!(1, T::Star),
            },
            b'/' => match (self.peek(1), self.peek(2)) {
                (b'=', _) => op!(2, T::SlashEqual),
                (b'/', b'=') => op!(3, T::DoubleSlashEqual),
                (b'/', _) => op!(2, T::DoubleSlash),
                _ => op!(1, T::Slash),
            },
            b'%' => {
                if self.peek(1) == b'=' {
                    op!(2, T::PercentEqual)
                } else {
                    op!(1, T::Percent)
                }
            }
            b'|' => {
                if self.peek(1) == b'=' {
                    op!(2, T::VbarEqual)
                } else {
                    op!(1, T::Vbar)
                }
            }
            b'^' => {
                if self.peek(1) == b'=' {
                    op!(2, T::CircumflexEqual)
                } else {
                    op!(1, T::CircumFlex)
                }
            }
            b'&' => {
                if self.peek(1) == b'=' {
                    op!(2, T::AmperEqual)
                } else {
                    op!(1, T::Amper)
                }
            }
            b'-' => match self.peek(1) {
                b'=' => op!(2, T::MinusEqual),
                b'>' => op!(2, T::Rarrow),
                _ => op!(1, T::Minus),
            },
            b'@' => {
                if self.peek(1) == b'=' {
                    op!(2, T::AtEqual)
                } else {
                    op!(1, T::At)
                }
            }
            b'!' => {
                if self.peek(1) == b'=' {
                    op!(2, T::NotEqual)
                } else {
                    return Err(self.err(msg::unrecognized('!'), start));
                }
            }
            b'~' => op!(1, T::Tilde),
            b'(' => {
                op!(1, T::Lpar);
                self.nesting += 1;
            }
            b'[' => {
                op!(1, T::Lsqb);
                self.nesting += 1;
            }
            b'{' => {
                op!(1, T::Lbrace);
                self.nesting += 1;
            }
            b')' | b']' | b'}' => {
                let kind = match c {
                    b')' => T::Rpar,
                    b']' => T::Rsqb,
                    _ => T::Rbrace,
                };
                op!(1, kind);
                if self.nesting == 0 {
                    return Err(self.err(msg::NESTING, self.pos));
                }
                self.nesting -= 1;
            }
            b':' => {
                if self.peek(1) == b'=' {
                    op!(2, T::ColonEqual)
                } else {
                    op!(1, T::Colon)
                }
            }
            b';' => op!(1, T::Semi),
            b'<' => match (self.peek(1), self.peek(2)) {
                (b'<', b'=') => op!(3, T::LeftShiftEqual),
                (b'<', _) => op!(2, T::LeftShift),
                (b'=', _) => op!(2, T::LessEqual),
                _ => op!(1, T::Less),
            },
            b'>' => match (self.peek(1), self.peek(2)) {
                (b'>', b'=') => op!(3, T::RightShiftEqual),
                (b'>', _) => op!(2, T::RightShift),
                (b'=', _) => op!(2, T::GreaterEqual),
                _ => op!(1, T::Greater),
            },
            b',' => op!(1, T::Comma),
            b'.' => {
                if self.peek(1).is_ascii_digit() {
                    return self.number();
                }
                if self.peek(1) == b'.' && self.peek(2) == b'.' {
                    op!(3, T::Ellipsis)
                } else {
                    op!(1, T::Dot)
                }
            }
            b'\n' | b'\r' => {
                self.skip_newline();
                if self.nesting == 0 {
                    self.at_bol = true;
                    self.push(T::Newline, start, self.pos);
                }
            }
            b' ' | b'\t' | b'\x0C' => {
                self.pos += 1;
                while matches!(self.peek(0), b' ' | b'\t' | b'\x0C') {
                    self.pos += 1;
                }
            }
            b'\\' => {
                self.pos += 1;
                if matches!(self.peek(0), b'\n' | b'\r') {
                    self.skip_newline();
                } else {
                    return Err(self.err(msg::LINE_CONTINUATION, self.pos));
                }
                if self.eof() {
                    return Err(self.err(msg::EOF, self.pos));
                }
            }
            _ if c >= 0x80 => {
                let (ch, len) = self.char_at(start);
                if is_xid_start(ch) {
                    return self.identifier();
                }
                if is_emoji_presentation(ch) {
                    self.pos += len;
                    let sym = self.names.intern(&self.src[start..self.pos]);
                    self.push_data(T::Name, 0, start, self.pos, sym.0);
                } else {
                    self.pos += len;
                    return Err(self.err(msg::unrecognized(ch), self.pos));
                }
            }
            _ => {
                let (ch, len) = self.char_at(start);
                self.pos += len;
                return Err(self.err(msg::unrecognized(ch), self.pos));
            }
        }
        Ok(())
    }

    fn identifier(&mut self) -> LResult<()> {
        let start = self.pos;
        // A string prefix: `r'`, `rb"`, `f'''` ...
        let (c1, c2, c3) = (self.peek(0), self.peek(1), self.peek(2));
        if matches!(c2, b'"' | b'\'') {
            let kind = match c1 {
                b'r' | b'R' => Some(StrKind::Raw),
                b'f' | b'F' => Some(StrKind::F),
                b'u' | b'U' => Some(StrKind::Unicode),
                b'b' | b'B' => Some(StrKind::Bytes),
                _ => None,
            };
            if let Some(kind) = kind {
                return self.string(start, kind);
            }
        } else if matches!(c3, b'"' | b'\'') {
            let kind = match (c1 | 0x20, c2 | 0x20) {
                (b'r', b'f') | (b'f', b'r') => Some(StrKind::RawF),
                (b'r', b'b') | (b'b', b'r') => Some(StrKind::RawBytes),
                _ => None,
            };
            if let Some(kind) = kind {
                return self.string(start, kind);
            }
        }
        loop {
            let c = self.peek(0);
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.pos += 1;
            } else if c >= 0x80 {
                let (ch, len) = self.char_at(self.pos);
                if !is_xid_continue(ch) {
                    break;
                }
                self.pos += len;
            } else {
                break;
            }
        }
        let word = &self.src[start..self.pos];
        if let Some(kind) = keyword(word.as_bytes()) {
            self.push(kind, start, self.pos);
        } else {
            let sym = self.names.intern(word);
            self.push_data(T::Name, 0, start, self.pos, sym.0);
        }
        Ok(())
    }

    // Numbers: validated here with the classic lexer's rules and messages;
    // their values are read from the source when needed (see `numbers`).
    fn number(&mut self) -> LResult<()> {
        let start = self.pos;
        let radix = match (self.peek(0), self.peek(1) | 0x20) {
            (b'0', b'x') => 16,
            (b'0', b'o') => 8,
            (b'0', b'b') => 2,
            _ => 10,
        };
        if radix != 10 {
            self.pos += 2;
            if self.digits(radix) == 0 {
                return Err(self.err("ParseBigIntError { kind: Empty }", start));
            }
            self.push_data(T::Int, radix as u8, start, self.pos, 0);
            return Ok(());
        }
        let start_is_zero = self.peek(0) == b'0';
        let int_start = self.pos;
        self.digits(10);
        let int_end = self.pos;
        if self.peek(0) == b'.' || self.at_exponent() {
            if self.peek(0) == b'.' {
                if self.peek(1) == b'_' {
                    return Err(self.err("Invalid Syntax", self.pos));
                }
                self.pos += 1;
                self.digits(10);
            }
            if matches!(self.peek(0), b'e' | b'E') {
                if self.peek(1) == b'_' {
                    return Err(self.err("Invalid Syntax", self.pos));
                }
                self.pos += 1;
                if matches!(self.peek(0), b'-' | b'+') {
                    if self.peek(1) == b'_' {
                        return Err(self.err("Invalid Syntax", self.pos));
                    }
                    self.pos += 1;
                }
                if self.digits(10) == 0 {
                    return Err(self.err("Invalid decimal literal", self.pos));
                }
            }
            if matches!(self.peek(0), b'j' | b'J') {
                self.pos += 1;
                self.push(T::Complex, start, self.pos);
            } else {
                self.push(T::Float, start, self.pos);
            }
            return Ok(());
        }
        if matches!(self.peek(0), b'j' | b'J') {
            self.pos += 1;
            self.push(T::Complex, start, self.pos);
            return Ok(());
        }
        if start_is_zero
            && self.b[int_start..int_end]
                .iter()
                .any(|&b| (b'1'..=b'9').contains(&b))
        {
            return Err(self.err("Invalid Token", self.pos));
        }
        self.push_data(T::Int, 10, start, self.pos, 0);
        Ok(())
    }

    /// Digits of `radix`, single underscores between them. Returns the count.
    fn digits(&mut self, radix: u32) -> usize {
        let mut count = 0;
        loop {
            if is_digit(self.peek(0), radix) {
                self.pos += 1;
                count += 1;
            } else if self.peek(0) == b'_' && is_digit(self.peek(1), radix) {
                self.pos += 1;
            } else {
                return count;
            }
        }
    }

    fn at_exponent(&self) -> bool {
        matches!(self.peek(0), b'e' | b'E')
            && (self.peek(1).is_ascii_digit()
                || (matches!(self.peek(1), b'+' | b'-') && self.peek(2).is_ascii_digit()))
    }

    fn string(&mut self, start: usize, kind: StrKind) -> LResult<()> {
        let quote_at = start + kind.prefix_len() as usize;
        if kind.is_f() {
            return self.fstring(start, kind);
        }
        let (end, triple) = self.classic_string_end(quote_at)?;
        self.pos = end;
        let flags = kind as u8 | if triple { TRIPLE } else { 0 };
        self.push_data(T::String, flags, start, end, 0);
        Ok(())
    }

    /// Where a string whose quote is at `quote_at` ends by the classic rules
    /// (a backslash escapes the next character, the first matching quote
    /// closes), and whether it is triple-quoted; or the classic error.
    fn classic_string_end(&self, quote_at: usize) -> LResult<(usize, bool)> {
        classic_end(self.b, self.base, quote_at)
    }

    /// An f-string: PEP 701 tokens, or one `FStringBroken` token.
    fn fstring(&mut self, start: usize, kind: StrKind) -> LResult<()> {
        let quote_at = start + kind.prefix_len() as usize;
        // CPython's bound (the lexer and the parser recurse per level).
        if self.fstring_depth + 1 >= MAX_FSTRING_NESTING {
            return Err(self.err(msg::FSTRING_NESTING, quote_at));
        }
        let classic = self.classic_string_end(quote_at);
        let mark = self.toks.len();
        let nesting = self.nesting;
        self.fstring_depth += 1;
        let result = self.fstring_tokens(start, kind);
        self.fstring_depth -= 1;
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.message == msg::FSTRING_NESTING => Err(error),
            Err(error) => {
                self.toks.truncate(mark);
                self.nesting = nesting;
                let (end, triple) = classic?;
                self.pos = end;
                self.errors.push(error);
                let flags = kind as u8 | if triple { TRIPLE } else { 0 };
                self.push_data(
                    T::FStringBroken,
                    flags,
                    start,
                    end,
                    self.errors.len() as u32 - 1,
                );
                Ok(())
            }
        }
    }

    fn fstring_tokens(&mut self, start: usize, kind: StrKind) -> LResult<()> {
        let quote_at = start + kind.prefix_len() as usize;
        let quote = self.at(quote_at);
        let triple = self.at(quote_at + 1) == quote && self.at(quote_at + 2) == quote;
        self.pos = quote_at + if triple { 3 } else { 1 };
        let flags = kind as u8 | if triple { TRIPLE } else { 0 };
        let start_index = self.toks.len();
        self.push_data(T::FStringStart, flags, start, self.pos, 0);
        let q = Quote {
            quote,
            triple,
            raw: kind.is_raw(),
        };
        self.fstring_literal(q, false)?;
        let end_index = self.toks.len() as u32 - 1;
        self.toks[start_index].data = end_index;
        Ok(())
    }

    /// Literal text up to the closing quote (or, in a format spec, up to the
    /// `}` that closes the field, which is left current).
    fn fstring_literal(&mut self, q: Quote, spec: bool) -> LResult<()> {
        let mut text = self.pos;
        loop {
            if self.eof() {
                return Err(self.err("f-string: expecting '}'", self.pos));
            }
            let c = self.peek(0);
            if c == q.quote && (!q.triple || (self.peek(1) == q.quote && self.peek(2) == q.quote)) {
                if spec {
                    return Err(self.err("f-string: expecting '}'", self.pos));
                }
                self.middle(text);
                let end = self.pos + if q.triple { 3 } else { 1 };
                self.push(T::FStringEnd, self.pos, end);
                self.pos = end;
                return Ok(());
            }
            match c {
                b'\\' => {
                    let next = self.peek(1);
                    if q.raw {
                        self.pos += if next == q.quote || next == b'\\' {
                            2
                        } else {
                            1
                        };
                    } else if next == b'N' && self.peek(2) == b'{' {
                        match self.b[self.pos + 3..].iter().position(|&b| b == b'}') {
                            Some(i) => self.pos += 4 + i,
                            None => return Err(self.err("f-string: expecting '}'", self.pos)),
                        }
                    } else if next == b'{' || next == b'}' {
                        self.pos += 1;
                    } else if next == b'\r' && self.peek(2) == b'\n' {
                        self.pos += 3;
                    } else if next == 0 && self.pos + 1 >= self.b.len() {
                        self.pos += 1;
                    } else {
                        let (_, len) = self.char_at(self.pos + 1);
                        self.pos += 1 + len;
                    }
                }
                b'{' => {
                    if !spec && self.peek(1) == b'{' {
                        self.pos += 2;
                        continue;
                    }
                    self.middle(text);
                    self.field(q)?;
                    text = self.pos;
                }
                b'}' => {
                    if spec {
                        self.middle(text);
                        return Ok(());
                    }
                    if self.peek(1) == b'}' {
                        self.pos += 2;
                        continue;
                    }
                    return Err(self.err("f-string: single '}' is not allowed", self.pos));
                }
                b'\n' | b'\r' => {
                    if !q.triple {
                        return Err(self.err("f-string: unterminated string", self.pos));
                    }
                    self.skip_newline();
                }
                _ => self.pos += 1,
            }
        }
    }

    fn middle(&mut self, text: usize) {
        if self.pos > text {
            self.push(T::FStringMiddle, text, self.pos);
        }
    }

    /// A replacement field, from its `{` through its `}`.
    fn field(&mut self, q: Quote) -> LResult<()> {
        let open = self.pos;
        self.pos += 1;
        self.push(T::FieldStart, open, self.pos);
        let base = self.nesting;
        self.nesting += 1;
        loop {
            // Whitespace, comments and newlines (not logical: the field is
            // bracketed) produce no tokens. As in CPython, a comment runs to
            // the end of the line even in a single-quoted f-string (a quote
            // in it does not close the literal).
            if self.eof() {
                return Err(self.err("f-string: expecting '}'", self.pos));
            }
            if self.nesting == base + 1 {
                match self.peek(0) {
                    b'}' => {
                        self.push(T::FieldEnd, self.pos, self.pos + 1);
                        self.pos += 1;
                        self.nesting = base;
                        return Ok(());
                    }
                    b':' => {
                        self.push(T::FormatSpec, self.pos, self.pos + 1);
                        self.pos += 1;
                        self.fstring_literal(q, true)?;
                        // At the `}` closing the field.
                        self.push(T::FieldEnd, self.pos, self.pos + 1);
                        self.pos += 1;
                        self.nesting = base;
                        return Ok(());
                    }
                    b'!' if self.peek(1) != b'=' => {
                        self.push(T::Exclamation, self.pos, self.pos + 1);
                        self.pos += 1;
                        continue;
                    }
                    b')' | b']' => {
                        return Err(self.err("f-string: unmatched closing bracket", self.pos));
                    }
                    _ => {}
                }
            }
            self.token()?;
        }
    }
}

/// Where a string whose quote is at `quote_at` ends by the classic rules
/// (a backslash escapes the next character, the first matching quote
/// closes), and whether it is triple-quoted; or the classic error.
pub(crate) fn classic_end(b: &[u8], base: u32, quote_at: usize) -> LResult<(usize, bool)> {
    let at = |i: usize| *b.get(i).unwrap_or(&0);
    let err = |message: &str, pos: usize| Error::new(message, base + pos as u32);
    let quote = at(quote_at);
    let mut pos = quote_at + 1;
    let triple = at(pos) == quote && at(pos + 1) == quote;
    if triple {
        pos += 2;
    }
    let unterminated = |pos: usize| err(if triple { msg::EOF } else { msg::STRING }, pos);
    loop {
        if pos >= b.len() {
            return Err(unterminated(pos));
        }
        let c = b[pos];
        if c == b'\\' {
            pos += 1;
            if pos >= b.len() {
                return Err(unterminated(pos));
            }
            pos += if b[pos] == b'\r' && at(pos + 1) == b'\n' {
                2
            } else {
                1
            };
            continue;
        }
        if c == b'\n' || c == b'\r' {
            pos += if c == b'\r' && at(pos + 1) == b'\n' {
                2
            } else {
                1
            };
            if !triple {
                return Err(err(msg::EOL, pos));
            }
            continue;
        }
        pos += 1;
        if c == quote && (!triple || (at(pos) == quote && at(pos + 1) == quote)) {
            return Ok((if triple { pos + 2 } else { pos }, triple));
        }
    }
}

#[derive(Clone, Copy)]
struct Quote {
    quote: u8,
    triple: bool,
    raw: bool,
}

/// How an indentation compares with the current level: only a difference in
/// both tabs and spaces in the same direction is certain.
fn compare(tabs: u32, spaces: u32, (ctabs, cspaces): (u32, u32)) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering::*;
    match tabs.cmp(&ctabs) {
        Less => (spaces <= cspaces).then_some(Less),
        Greater => (spaces >= cspaces).then_some(Greater),
        Equal => Some(spaces.cmp(&cspaces)),
    }
}

#[inline]
fn is_digit(c: u8, radix: u32) -> bool {
    match radix {
        2 => matches!(c, b'0' | b'1'),
        8 => matches!(c, b'0'..=b'7'),
        10 => c.is_ascii_digit(),
        _ => c.is_ascii_hexdigit(),
    }
}

// RustPython's single-emoji identifier extension. Emoji=Yes alone also
// includes text-presentation characters, so test Emoji_Presentation.
fn is_emoji_presentation(c: char) -> bool {
    matches!(
        c.emoji_status(),
        EmojiStatus::EmojiPresentation
            | EmojiStatus::EmojiPresentationAndModifierBase
            | EmojiStatus::EmojiPresentationAndEmojiComponent
            | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
    )
}

/// `match`, `case` and `type` are keywords only where a statement starts
/// with them and the rest of the logical line fits; elsewhere they are names
/// (the classic soft-keyword pass, kept exactly). An f-string counts as one
/// token.
fn soft_keywords(tokens: &mut [Token], mode: Mode) {
    /// The token after `i`, an f-string counting as one.
    fn next(tokens: &[Token], i: usize) -> usize {
        if tokens[i].kind == T::FStringStart {
            tokens[i].data as usize + 1
        } else {
            i + 1
        }
    }
    fn to_name(tok: &mut Token) {
        let sym = match tok.kind {
            T::Match => intern::MATCH,
            T::Case => intern::CASE,
            T::Type => intern::TYPE,
            _ => return,
        };
        tok.kind = T::Name;
        tok.data = sym.0;
    }
    let mut start_of_line = matches!(mode, Mode::Module | Mode::Interactive);
    let n = tokens.len();
    let mut i = 0;
    while i < n {
        let kind = tokens[i].kind;
        let keyword = match kind {
            T::Match | T::Case => {
                let mut seen_colon = false;
                if start_of_line {
                    let mut nesting = 0i32;
                    let mut first = true;
                    let mut seen_lambda = false;
                    let mut j = next(tokens, i);
                    while j < n {
                        match tokens[j].kind {
                            T::Newline | T::Error | T::EndOfFile => break,
                            T::Lambda if nesting == 0 => seen_lambda = true,
                            T::Colon if nesting == 0 => {
                                if seen_lambda {
                                    seen_lambda = false;
                                } else if !first {
                                    seen_colon = true;
                                }
                            }
                            T::Lpar | T::Lsqb | T::Lbrace => nesting += 1,
                            T::Rpar | T::Rsqb | T::Rbrace => nesting -= 1,
                            _ => {}
                        }
                        first = false;
                        j = next(tokens, j);
                    }
                }
                seen_colon
            }
            T::Type => {
                let mut is_alias = false;
                let j = i + 1;
                if start_of_line
                    && j < n
                    && matches!(tokens[j].kind, T::Name | T::Type | T::Match | T::Case)
                {
                    let mut nesting = 0i32;
                    let mut k = next(tokens, j);
                    while k < n {
                        match tokens[k].kind {
                            T::Newline | T::Error | T::EndOfFile => break,
                            T::Equal if nesting == 0 => {
                                is_alias = true;
                                break;
                            }
                            T::Lsqb => nesting += 1,
                            T::Rsqb => nesting -= 1,
                            _ if nesting > 0 => {}
                            _ => break,
                        }
                        k = next(tokens, k);
                    }
                }
                is_alias
            }
            _ => true,
        };
        if !keyword {
            to_name(&mut tokens[i]);
        }
        if kind == T::FStringStart {
            // Inside an f-string (parsed as expressions) they are names.
            let end = tokens[i].data as usize;
            for tok in &mut tokens[i + 1..end] {
                to_name(tok);
            }
        }
        start_of_line = matches!(tokens[i].kind, T::Newline | T::Indent | T::Dedent);
        i = next(tokens, i);
    }
}

/// The name of an interned identifier token.
pub fn sym(token: &Token) -> Sym {
    Sym(token.data)
}

/// Where the classic lexer (every backslash escapes, the first matching
/// quote closes) ends the string literal starting at `start` (its prefix),
/// if it does. For tests comparing against that lexer.
#[doc(hidden)]
pub fn classic_literal_end(source: &str, start: usize) -> Option<usize> {
    let b = source.as_bytes();
    let mut quote_at = start;
    while quote_at < b.len() && !matches!(b[quote_at], b'"' | b'\'') {
        quote_at += 1;
    }
    classic_end(b, 0, quote_at).ok().map(|(end, _)| end)
}
