//! The lexer.
//!
//! Byte-oriented with an ASCII fast path, since real JS is overwhelmingly
//! ASCII, falling back to `char` decoding only when a byte is >= 0x80.
//!
//! Three places JavaScript cannot be lexed without help from the parser, and
//! this lexer takes that help explicitly rather than guessing:
//!
//! 1. **`/` is either division or the start of a regex**, decidable only from
//!    the grammatical position — `a / b` versus `return /re/`. The parser knows
//!    which production it is in, so [`Lexer::next_token`] takes a
//!    `regex_allowed` flag. Guessing from the previous token (the usual hack)
//!    gets `a++ /b/g` and `if (x) /re/` wrong in opposite directions.
//!
//! 2. **Template literals nest arbitrarily.** `` `a${ `b${c}` }d` `` cannot be
//!    scanned in one pass, because the lexer has to hand control back at each
//!    `${` and be resumed at the matching `}`. The parser calls
//!    [`Lexer::read_template_continue`] when it has consumed that `}`.
//!
//! 3. **A `}` is either a block close or a template resume**, which is the same
//!    problem seen from the other side, and is resolved the same way.
//!
//! Errors are returned, never panicked: a syntax error is data.

use super::token::{Keyword, NumKind, NumLit, Punct, Span, StrVal, Token, TokenKind};

/// A lexical error, with the byte offset it was found at.
#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub msg: String,
    pub pos: u32,
}

impl LexError {
    fn new(msg: impl Into<String>, pos: usize) -> LexError {
        LexError { msg: msg.into(), pos: pos as u32 }
    }
}

type LResult<T> = Result<T, LexError>;

pub struct Lexer<'s> {
    src: &'s [u8],
    /// Current byte offset.
    pos: usize,
    /// A LineTerminator has been seen since the last token was produced.
    saw_newline: bool,
}

impl<'s> Lexer<'s> {
    pub fn new(src: &'s str) -> Lexer<'s> {
        let mut lx = Lexer { src: src.as_bytes(), pos: 0, saw_newline: false };
        // A leading BOM is whitespace, not part of the first token.
        if lx.src.starts_with(&[0xEF, 0xBB, 0xBF]) {
            lx.pos = 3;
        }
        // A hashbang (`#!...`) is a comment, but ONLY as the very first bytes —
        // anywhere else `#` starts a private name.
        if lx.src[lx.pos..].starts_with(b"#!") {
            lx.skip_to_line_end();
        }
        lx
    }

    pub fn pos(&self) -> u32 {
        self.pos as u32
    }

    /// Reposition the lexer, for the parser's cover-grammar re-scans.
    pub fn seek(&mut self, pos: u32) {
        self.pos = pos as usize;
        self.saw_newline = false;
    }

    // ---- byte helpers ------------------------------------------------------

    #[inline]
    fn peek(&self) -> u8 {
        *self.src.get(self.pos).unwrap_or(&0)
    }

    #[inline]
    fn peek_at(&self, n: usize) -> u8 {
        *self.src.get(self.pos + n).unwrap_or(&0)
    }

    #[inline]
    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    /// Decode the char at `pos` (which must be a UTF-8 boundary), and its width.
    fn char_at(&self, pos: usize) -> (char, usize) {
        let s = unsafe { std::str::from_utf8_unchecked(&self.src[pos..]) };
        match s.chars().next() {
            Some(c) => (c, c.len_utf8()),
            None => ('\0', 1),
        }
    }

    // ---- classification ----------------------------------------------------

    fn is_line_terminator(c: char) -> bool {
        matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
    }

    fn is_ws(c: char) -> bool {
        matches!(
            c,
            '\t' | '\u{0B}' | '\u{0C}' | ' ' | '\u{A0}' | '\u{FEFF}'
                | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
        )
    }

    /// IdentifierStart — `UnicodeIDStart`, which is the real `ID_Start` property.
    ///
    /// This used to approximate it with Rust's `char::is_alphabetic()`. That is a
    /// DIFFERENT Unicode property, and the two disagree in both directions:
    ///
    /// * too strict — `Other_ID_Start` (U+2118 SCRIPT CAPITAL P, U+212E, U+309B,
    ///   U+309C) is `ID_Start` but not Alphabetic, so `var ℘` was rejected;
    /// * too loose — `Other_Alphabetic` combining marks (U+05B0, U+0345, U+0903)
    ///   are Alphabetic but NOT `ID_Start` and were accepted, as was U+2E2F,
    ///   which is `Lm` but excluded from `ID_Start` by `Pattern_Syntax`.
    ///
    /// The ASCII arm stays explicit rather than deferring to the crate: its
    /// `ASCII_START` table excludes both `$` and `_`, so bailing out here first
    /// and calling the `_unicode` variant is both correct and one lookup cheaper
    /// on the hot path.
    fn is_id_start(c: char) -> bool {
        if c.is_ascii() {
            return c.is_ascii_alphabetic() || c == '$' || c == '_';
        }
        unicode_id_start::is_id_start_unicode(c)
    }

    /// IdentifierPart — `ID_Continue`, plus ZWNJ/ZWJ which the spec adds
    /// explicitly.
    ///
    /// `is_alphanumeric()` was wrong the same way: it missed the `Mn`/`Mc`
    /// combining marks (U+0300), the `Pc` connectors (U+203F, U+2040, U+FF3F)
    /// and `Other_ID_Continue` (U+0387, U+19DA), while over-accepting `No`
    /// (U+00B2 SUPERSCRIPT TWO). U+00B7 was hand-special-cased below precisely
    /// because the approximation could not express `Other_ID_Continue`; the real
    /// table covers it and the special case is gone.
    fn is_id_part(c: char) -> bool {
        if c.is_ascii() {
            return c.is_ascii_alphanumeric() || c == '$' || c == '_';
        }
        c == '\u{200C}' || c == '\u{200D}' || unicode_id_start::is_id_continue_unicode(c)
    }

    /// Test-only windows onto the identifier predicates, so the unit test can
    /// assert the REJECTED side too (a rejected char never reaches `names()`).
    #[cfg(test)]
    pub(crate) fn id_start_for_test(c: char) -> bool {
        Self::is_id_start(c)
    }

    #[cfg(test)]
    pub(crate) fn id_part_for_test(c: char) -> bool {
        Self::is_id_part(c)
    }

    // ---- trivia ------------------------------------------------------------

    /// Skip whitespace, line terminators and comments, recording whether any
    /// line terminator was crossed (which ASI and the no-LineTerminator-here
    /// restrictions are defined in terms of).
    fn skip_trivia(&mut self) -> LResult<()> {
        loop {
            if self.at_end() {
                return Ok(());
            }
            let b = self.peek();
            // ASCII fast path.
            if b < 0x80 {
                match b {
                    b' ' | b'\t' | 0x0B | 0x0C => {
                        self.pos += 1;
                        continue;
                    }
                    b'\n' => {
                        self.pos += 1;
                        self.saw_newline = true;
                        continue;
                    }
                    b'\r' => {
                        self.pos += 1;
                        if self.peek() == b'\n' {
                            self.pos += 1;
                        }
                        self.saw_newline = true;
                        continue;
                    }
                    b'/' => match self.peek_at(1) {
                        b'/' => {
                            self.pos += 2;
                            self.skip_to_line_end();
                            continue;
                        }
                        b'*' => {
                            self.skip_block_comment()?;
                            continue;
                        }
                        _ => return Ok(()),
                    },
                    // Annex B HTML-like comments. `<!--` is a line comment
                    // anywhere; `-->` only when it opens a line (i.e. only
                    // trivia precedes it on that line).
                    b'<' if self.peek_at(1) == b'!' && self.peek_at(2) == b'-' && self.peek_at(3) == b'-' => {
                        self.pos += 4;
                        self.skip_to_line_end();
                        continue;
                    }
                    b'-' if self.saw_newline
                        && self.peek_at(1) == b'-'
                        && self.peek_at(2) == b'>' =>
                    {
                        self.pos += 3;
                        self.skip_to_line_end();
                        continue;
                    }
                    _ => return Ok(()),
                }
            }
            let (c, w) = self.char_at(self.pos);
            if Self::is_line_terminator(c) {
                self.pos += w;
                self.saw_newline = true;
                continue;
            }
            if Self::is_ws(c) {
                self.pos += w;
                continue;
            }
            return Ok(());
        }
    }

    fn skip_to_line_end(&mut self) {
        while !self.at_end() {
            let b = self.peek();
            if b < 0x80 {
                if b == b'\n' || b == b'\r' {
                    return;
                }
                self.pos += 1;
            } else {
                let (c, w) = self.char_at(self.pos);
                if Self::is_line_terminator(c) {
                    return;
                }
                self.pos += w;
            }
        }
    }

    fn skip_block_comment(&mut self) -> LResult<()> {
        let start = self.pos;
        self.pos += 2;
        loop {
            if self.at_end() {
                return Err(LexError::new("unterminated comment", start));
            }
            let b = self.peek();
            if b == b'*' && self.peek_at(1) == b'/' {
                self.pos += 2;
                return Ok(());
            }
            if b < 0x80 {
                // A block comment containing a line terminator counts as one
                // for ASI — `a = b /*\n*/ ++c` inserts a semicolon.
                if b == b'\n' || b == b'\r' {
                    self.saw_newline = true;
                }
                self.pos += 1;
            } else {
                let (c, w) = self.char_at(self.pos);
                if Self::is_line_terminator(c) {
                    self.saw_newline = true;
                }
                self.pos += w;
            }
        }
    }

    // ---- the main entry point ---------------------------------------------

    /// Produce the next token. `regex_allowed` says whether a `/` here begins a
    /// regular expression literal (the parser knows; the lexer cannot).
    pub fn next_token(&mut self, regex_allowed: bool) -> LResult<Token> {
        self.saw_newline = false;
        self.skip_trivia()?;
        let newline_before = self.saw_newline;
        let start = self.pos;

        if self.at_end() {
            return Ok(Token {
                kind: TokenKind::Eof,
                span: Span::new(start as u32, start as u32),
                newline_before,
            });
        }

        let b = self.peek();
        let kind = match b {
            b'0'..=b'9' => self.read_number()?,
            b'.' if self.peek_at(1).is_ascii_digit() => self.read_number()?,
            b'"' | b'\'' => self.read_string(b)?,
            b'`' => self.read_template(true)?,
            b'/' if regex_allowed => self.read_regex()?,
            b'#' => {
                self.pos += 1;
                let (name, had_escape) = self.read_ident_name()?;
                if name.is_empty() {
                    return Err(LexError::new("expected a private name after '#'", start));
                }
                TokenKind::Ident { name, kw: Keyword::None, had_escape, private: true }
            }
            _ => {
                let (c, _) = if b < 0x80 { (b as char, 1) } else { self.char_at(self.pos) };
                if Self::is_id_start(c) || b == b'\\' {
                    let (name, had_escape) = self.read_ident_name()?;
                    let kw = if had_escape { Keyword::None } else { Keyword::classify(&name) };
                    TokenKind::Ident { name, kw, had_escape, private: false }
                } else {
                    TokenKind::Punct(self.read_punct()?)
                }
            }
        };

        Ok(Token { kind, span: Span::new(start as u32, self.pos as u32), newline_before })
    }

    // ---- identifiers -------------------------------------------------------

    /// Read an identifier, resolving `\uXXXX` / `\u{...}` escapes into the name
    /// and reporting whether any were used.
    ///
    /// The escaped form is a different token for keyword purposes — `await`
    /// spells the identifier `await`, not the keyword — so the flag has to
    /// survive to the parser.
    fn read_ident_name(&mut self) -> LResult<(String, bool)> {
        let mut out = String::new();
        let mut had_escape = false;
        let mut first = true;
        loop {
            if self.at_end() {
                break;
            }
            let b = self.peek();
            if b == b'\\' {
                let esc_at = self.pos;
                self.pos += 1;
                if self.peek() != b'u' {
                    return Err(LexError::new("expected 'u' in identifier escape", esc_at));
                }
                self.pos += 1;
                let cp = self.read_unicode_escape_value(esc_at)?;
                let c = char::from_u32(cp)
                    .ok_or_else(|| LexError::new("invalid code point in identifier", esc_at))?;
                let ok = if first { Self::is_id_start(c) } else { Self::is_id_part(c) };
                if !ok {
                    return Err(LexError::new(
                        "escape does not encode a valid identifier character",
                        esc_at,
                    ));
                }
                out.push(c);
                had_escape = true;
                first = false;
                continue;
            }
            let (c, w) = if b < 0x80 { (b as char, 1) } else { self.char_at(self.pos) };
            let ok = if first { Self::is_id_start(c) } else { Self::is_id_part(c) };
            if !ok {
                break;
            }
            out.push(c);
            self.pos += w;
            first = false;
        }
        Ok((out, had_escape))
    }

    /// The value of a `uXXXX` or `u{...}` escape body (the `u` already consumed).
    fn read_unicode_escape_value(&mut self, at: usize) -> LResult<u32> {
        if self.peek() == b'{' {
            self.pos += 1;
            let mut v: u32 = 0;
            let mut any = false;
            loop {
                let b = self.peek();
                if b == b'}' {
                    self.pos += 1;
                    break;
                }
                let d = hex_val(b)
                    .ok_or_else(|| LexError::new("invalid unicode escape", at))?;
                v = v.saturating_mul(16).saturating_add(d);
                if v > 0x10FFFF {
                    return Err(LexError::new("unicode escape out of range", at));
                }
                any = true;
                self.pos += 1;
            }
            if !any {
                return Err(LexError::new("empty unicode escape", at));
            }
            Ok(v)
        } else {
            let mut v: u32 = 0;
            for _ in 0..4 {
                let d = hex_val(self.peek())
                    .ok_or_else(|| LexError::new("invalid unicode escape", at))?;
                v = v * 16 + d;
                self.pos += 1;
            }
            Ok(v)
        }
    }

    // ---- strings -----------------------------------------------------------

    /// Read a string literal into UTF-16 units, so a lone surrogate escape
    /// survives instead of being replaced.
    fn read_string(&mut self, quote: u8) -> LResult<TokenKind> {
        let start = self.pos;
        self.pos += 1;
        let mut units: Vec<u16> = Vec::new();
        loop {
            if self.at_end() {
                return Err(LexError::new("unterminated string literal", start));
            }
            let b = self.peek();
            if b == quote {
                self.pos += 1;
                break;
            }
            if b == b'\n' || b == b'\r' {
                return Err(LexError::new("unterminated string literal", start));
            }
            if b == b'\\' {
                self.read_escape_into(&mut units, /* template */ false)?;
                continue;
            }
            if b < 0x80 {
                units.push(b as u16);
                self.pos += 1;
            } else {
                let (c, w) = self.char_at(self.pos);
                // U+2028/U+2029 ARE permitted directly in string literals
                // (ES2019 "JSON superset"), unlike \n and \r.
                let mut buf = [0u16; 2];
                units.extend_from_slice(c.encode_utf16(&mut buf));
                self.pos += w;
            }
        }
        Ok(TokenKind::Str(StrVal::from_utf16(units)))
    }

    /// Consume one `\`-escape and append its UTF-16 units.
    ///
    /// `Err` on a malformed escape. In a TEMPLATE that is not always fatal (a
    /// tagged template may contain invalid escapes and gets `undefined` for the
    /// cooked value), so the template reader catches it rather than propagating.
    fn read_escape_into(&mut self, out: &mut Vec<u16>, template: bool) -> LResult<()> {
        let at = self.pos;
        self.pos += 1; // the backslash
        if self.at_end() {
            return Err(LexError::new("unterminated escape", at));
        }
        let b = self.peek();
        match b {
            b'n' => { out.push(0x0A); self.pos += 1; }
            b't' => { out.push(0x09); self.pos += 1; }
            b'r' => { out.push(0x0D); self.pos += 1; }
            b'b' => { out.push(0x08); self.pos += 1; }
            b'f' => { out.push(0x0C); self.pos += 1; }
            b'v' => { out.push(0x0B); self.pos += 1; }
            b'0'..=b'7' => {
                // `\0` not followed by a digit is NUL and always legal. Any
                // other octal escape is Annex B legacy, illegal in strict code
                // and in templates; strictness lives in the parser, so the
                // spelling is preserved by returning the value and letting the
                // parser reject it via NumKind-style context. Templates are
                // unambiguous, so reject here.
                if b == b'0' && !self.peek_at(1).is_ascii_digit() {
                    out.push(0);
                    self.pos += 1;
                } else if template {
                    return Err(LexError::new("octal escape in template literal", at));
                } else {
                    let mut v: u32 = 0;
                    let max = if b <= b'3' { 3 } else { 2 };
                    let mut n = 0;
                    while n < max {
                        let d = self.peek();
                        if !(b'0'..=b'7').contains(&d) {
                            break;
                        }
                        v = v * 8 + (d - b'0') as u32;
                        self.pos += 1;
                        n += 1;
                    }
                    out.push(v as u16);
                }
            }
            b'8' | b'9' => {
                // Annex B: `\8`/`\9` are the literal characters, and illegal in
                // strict code or a template.
                if template {
                    return Err(LexError::new("invalid escape in template literal", at));
                }
                out.push(b as u16);
                self.pos += 1;
            }
            b'x' => {
                self.pos += 1;
                let hi = hex_val(self.peek())
                    .ok_or_else(|| LexError::new("invalid hex escape", at))?;
                self.pos += 1;
                let lo = hex_val(self.peek())
                    .ok_or_else(|| LexError::new("invalid hex escape", at))?;
                self.pos += 1;
                out.push((hi * 16 + lo) as u16);
            }
            b'u' => {
                self.pos += 1;
                let cp = self.read_unicode_escape_value(at)?;
                // Push as UTF-16. A value in the surrogate range is emitted as
                // that single unit — a lone surrogate, which is legal and
                // observable, and exactly what `String` could not hold.
                if cp > 0xFFFF {
                    let v = cp - 0x10000;
                    out.push(0xD800 + ((v >> 10) as u16));
                    out.push(0xDC00 + ((v & 0x3FF) as u16));
                } else {
                    out.push(cp as u16);
                }
            }
            b'\r' => {
                // LineContinuation: produces nothing. CRLF counts as one.
                self.pos += 1;
                if self.peek() == b'\n' {
                    self.pos += 1;
                }
            }
            b'\n' => {
                self.pos += 1;
            }
            _ => {
                let (c, w) = if b < 0x80 { (b as char, 1) } else { self.char_at(self.pos) };
                if Self::is_line_terminator(c) {
                    self.pos += w; // LineContinuation for U+2028/U+2029
                } else {
                    let mut buf = [0u16; 2];
                    out.extend_from_slice(c.encode_utf16(&mut buf));
                    self.pos += w;
                }
            }
        }
        Ok(())
    }

    // ---- templates ---------------------------------------------------------

    /// Read a template chunk starting at a backtick (`head`) or resuming after
    /// the `}` that closed a substitution.
    fn read_template(&mut self, head: bool) -> LResult<TokenKind> {
        let start = self.pos;
        self.pos += 1; // ` or }
        let raw_start = self.pos;
        let mut units: Vec<u16> = Vec::new();
        // An invalid escape is only fatal in an untagged template, which the
        // lexer cannot know, so record it and let the parser decide.
        let mut bad_escape = false;
        let tail;
        loop {
            if self.at_end() {
                return Err(LexError::new("unterminated template literal", start));
            }
            let b = self.peek();
            if b == b'`' {
                tail = true;
                break;
            }
            if b == b'$' && self.peek_at(1) == b'{' {
                tail = false;
                break;
            }
            if b == b'\\' {
                let save = self.pos;
                if self.read_escape_into(&mut units, true).is_err() {
                    bad_escape = true;
                    // Resynchronise: skip the backslash and one character, so
                    // scanning can still find the terminator.
                    self.pos = save + 1;
                    if !self.at_end() {
                        let (_, w) = self.char_at(self.pos);
                        self.pos += w;
                    }
                }
                continue;
            }
            // Raw text: CRLF and CR both normalise to LF in both cooked and raw.
            if b == b'\r' {
                self.pos += 1;
                if self.peek() == b'\n' {
                    self.pos += 1;
                }
                units.push(0x0A);
                continue;
            }
            if b < 0x80 {
                units.push(b as u16);
                self.pos += 1;
            } else {
                let (c, w) = self.char_at(self.pos);
                let mut buf = [0u16; 2];
                units.extend_from_slice(c.encode_utf16(&mut buf));
                self.pos += w;
            }
        }
        let raw_end = self.pos;
        // Consume the terminator: ` for a tail, ${ otherwise.
        self.pos += if tail { 1 } else { 2 };
        let raw = String::from_utf8_lossy(&self.src[raw_start..raw_end]).replace("\r\n", "\n").replace('\r', "\n");
        Ok(TokenKind::Template {
            cooked: if bad_escape { None } else { Some(StrVal::from_utf16(units)) },
            raw,
            head,
            tail,
        })
    }

    /// Resume a template after the parser has consumed the `}` that closes a
    /// substitution. `self.pos` must be ON that `}`.
    pub fn read_template_continue(&mut self) -> LResult<Token> {
        let start = self.pos;
        let kind = self.read_template(false)?;
        Ok(Token { kind, span: Span::new(start as u32, self.pos as u32), newline_before: false })
    }

    // ---- regex -------------------------------------------------------------

    /// Read a regex literal. The pattern is kept as written: the regex engine
    /// compiles the source, so interpreting escapes here would be wrong.
    fn read_regex(&mut self) -> LResult<TokenKind> {
        let start = self.pos;
        self.pos += 1; // opening /
        let body_start = self.pos;
        let mut in_class = false;
        loop {
            if self.at_end() {
                return Err(LexError::new("unterminated regular expression", start));
            }
            let b = self.peek();
            if b == b'\\' {
                self.pos += 1;
                if self.at_end() {
                    return Err(LexError::new("unterminated regular expression", start));
                }
                let (c, w) = if self.peek() < 0x80 {
                    (self.peek() as char, 1)
                } else {
                    self.char_at(self.pos)
                };
                if Self::is_line_terminator(c) {
                    return Err(LexError::new("unterminated regular expression", start));
                }
                self.pos += w;
                continue;
            }
            if b < 0x80 {
                match b {
                    b'[' => in_class = true,
                    b']' => in_class = false,
                    // A `/` inside a character class does not close the literal.
                    b'/' if !in_class => break,
                    b'\n' | b'\r' => {
                        return Err(LexError::new("unterminated regular expression", start))
                    }
                    _ => {}
                }
                self.pos += 1;
            } else {
                let (c, w) = self.char_at(self.pos);
                if Self::is_line_terminator(c) {
                    return Err(LexError::new("unterminated regular expression", start));
                }
                self.pos += w;
            }
        }
        let body_end = self.pos;
        self.pos += 1; // closing /
        // Flags are an IdentifierPart run; validating which letters are legal
        // (and rejecting duplicates) is the parser's job.
        let flags_start = self.pos;
        while !self.at_end() {
            let b = self.peek();
            let (c, w) = if b < 0x80 { (b as char, 1) } else { self.char_at(self.pos) };
            if !Self::is_id_part(c) {
                break;
            }
            self.pos += w;
        }
        let pattern_bytes = &self.src[body_start..body_end];
        let pattern = match std::str::from_utf8(pattern_bytes) {
            Ok(s) => StrVal::Utf8(s.to_string()),
            Err(_) => StrVal::Utf8(String::from_utf8_lossy(pattern_bytes).into_owned()),
        };
        let flags = String::from_utf8_lossy(&self.src[flags_start..self.pos]).into_owned();
        Ok(TokenKind::Regex { pattern, flags })
    }

    // ---- numbers -----------------------------------------------------------

    fn read_number(&mut self) -> LResult<TokenKind> {
        let start = self.pos;
        let mut kind = NumKind::Decimal;

        if self.peek() == b'0' {
            match self.peek_at(1) | 0x20 {
                b'x' => return self.read_radix(16, start),
                b'o' => return self.read_radix(8, start),
                b'b' => return self.read_radix(2, start),
                _ => {}
            }
            // Legacy forms: `0123` is octal (Annex B), `08`/`09` is decimal.
            let next = self.peek_at(1);
            if next.is_ascii_digit() {
                let mut p = self.pos + 1;
                let mut all_octal = true;
                while p < self.src.len() && self.src[p].is_ascii_digit() {
                    if self.src[p] >= b'8' {
                        all_octal = false;
                    }
                    p += 1;
                }
                // A `.` or exponent after the digits makes it an ordinary
                // decimal (`08.5`), not a legacy octal.
                let followed_by_dec = p < self.src.len() && (self.src[p] == b'.' || (self.src[p] | 0x20) == b'e');
                if all_octal && !followed_by_dec {
                    let text = std::str::from_utf8(&self.src[self.pos + 1..p]).unwrap_or("0");
                    let v = u64::from_str_radix(text, 8).unwrap_or(0) as f64;
                    self.pos = p;
                    self.reject_ident_after(start)?;
                    return Ok(TokenKind::Num(NumLit { value: v, kind: NumKind::LegacyOctal }));
                }
                kind = NumKind::NonOctalDecimal;
            }
        }

        // Decimal: digits [. digits] [(e|E) [+|-] digits]
        let mut text = String::new();
        self.read_digits_into(&mut text, 10)?;
        if self.peek() == b'.' {
            text.push('.');
            self.pos += 1;
            self.read_digits_into(&mut text, 10)?;
        }
        // BigInt: only a plain integer may carry the `n` suffix.
        if self.peek() == b'n' && !text.contains('.') && kind == NumKind::Decimal {
            self.pos += 1;
            self.reject_ident_after(start)?;
            return Ok(TokenKind::BigInt(text));
        }
        if (self.peek() | 0x20) == b'e' {
            let save = self.pos;
            self.pos += 1;
            let mut exp = String::new();
            if self.peek() == b'+' || self.peek() == b'-' {
                exp.push(self.peek() as char);
                self.pos += 1;
            }
            if !self.peek().is_ascii_digit() {
                // Not an exponent after all (`1e` / `1e+`): that is a syntax
                // error, since an identifier may not follow a numeric literal.
                self.pos = save;
                return Err(LexError::new("missing exponent digits", start));
            }
            let mut digits = String::new();
            self.read_digits_into(&mut digits, 10)?;
            text.push('e');
            text.push_str(&exp);
            text.push_str(&digits);
        }
        self.reject_ident_after(start)?;
        let value: f64 = text.parse().unwrap_or(f64::NAN);
        Ok(TokenKind::Num(NumLit { value, kind }))
    }

    fn read_radix(&mut self, radix: u32, start: usize) -> LResult<TokenKind> {
        self.pos += 2; // 0x / 0o / 0b
        let mut text = String::new();
        self.read_digits_into(&mut text, radix)?;
        if text.is_empty() {
            return Err(LexError::new("missing digits after radix prefix", start));
        }
        if self.peek() == b'n' {
            self.pos += 1;
            self.reject_ident_after(start)?;
            let v = u128::from_str_radix(&text, radix)
                .map(|v| v.to_string())
                .unwrap_or_else(|_| big_radix_to_decimal(&text, radix));
            return Ok(TokenKind::BigInt(v));
        }
        self.reject_ident_after(start)?;
        // Exact for values within f64's integer range; beyond that the
        // accumulate-in-f64 loop is what the spec's MV mandates anyway.
        let mut v = 0f64;
        for c in text.bytes() {
            v = v * radix as f64 + hex_val(c).unwrap_or(0) as f64;
        }
        Ok(TokenKind::Num(NumLit { value: v, kind: NumKind::Prefixed }))
    }

    /// Read digits of `radix`, honouring `_` separators. A separator may not
    /// lead, trail, or repeat.
    fn read_digits_into(&mut self, out: &mut String, radix: u32) -> LResult<()> {
        let mut last_was_sep = false;
        let mut any = false;
        loop {
            let b = self.peek();
            if b == b'_' {
                if !any || last_was_sep {
                    return Err(LexError::new("misplaced numeric separator", self.pos));
                }
                last_was_sep = true;
                self.pos += 1;
                continue;
            }
            let ok = match radix {
                16 => hex_val(b).is_some(),
                10 => b.is_ascii_digit(),
                8 => (b'0'..=b'7').contains(&b),
                2 => b == b'0' || b == b'1',
                _ => false,
            };
            if !ok {
                break;
            }
            out.push(b as char);
            any = true;
            last_was_sep = false;
            self.pos += 1;
        }
        if last_was_sep {
            return Err(LexError::new("trailing numeric separator", self.pos));
        }
        Ok(())
    }

    /// A numeric literal may not be immediately followed by an identifier start
    /// or a digit (`3in` / `0x1g`) — that is an early SyntaxError, not two
    /// tokens.
    fn reject_ident_after(&mut self, start: usize) -> LResult<()> {
        if self.at_end() {
            return Ok(());
        }
        let b = self.peek();
        let (c, _) = if b < 0x80 { (b as char, 1) } else { self.char_at(self.pos) };
        if Self::is_id_start(c) || c.is_ascii_digit() {
            return Err(LexError::new("identifier starts immediately after a number", start));
        }
        Ok(())
    }

    // ---- punctuators -------------------------------------------------------

    fn read_punct(&mut self) -> LResult<Punct> {
        use Punct::*;
        let start = self.pos;
        let b = self.peek();
        macro_rules! adv {
            ($n:expr, $p:expr) => {{
                self.pos += $n;
                return Ok($p);
            }};
        }
        match b {
            b'{' => adv!(1, LBrace),
            b'}' => adv!(1, RBrace),
            b'(' => adv!(1, LParen),
            b')' => adv!(1, RParen),
            b'[' => adv!(1, LBracket),
            b']' => adv!(1, RBracket),
            b';' => adv!(1, Semi),
            b',' => adv!(1, Comma),
            b':' => adv!(1, Colon),
            b'~' => adv!(1, Tilde),
            b'@' => adv!(1, At),
            b'.' => {
                if self.peek_at(1) == b'.' && self.peek_at(2) == b'.' {
                    adv!(3, DotDotDot)
                }
                adv!(1, Dot)
            }
            b'?' => match (self.peek_at(1), self.peek_at(2)) {
                // `?.` only when not followed by a digit: `a?.5:b` is a
                // conditional whose consequent is `.5`.
                (b'.', d) if !d.is_ascii_digit() => adv!(2, QuestionDot),
                (b'?', b'=') => adv!(3, QuestionQuestionEq),
                (b'?', _) => adv!(2, QuestionQuestion),
                _ => adv!(1, Question),
            },
            b'<' => match (self.peek_at(1), self.peek_at(2)) {
                (b'<', b'=') => adv!(3, ShlEq),
                (b'<', _) => adv!(2, Shl),
                (b'=', _) => adv!(2, LtEq),
                _ => adv!(1, Lt),
            },
            b'>' => match (self.peek_at(1), self.peek_at(2), self.peek_at(3)) {
                (b'>', b'>', b'=') => adv!(4, UShrEq),
                (b'>', b'>', _) => adv!(3, UShr),
                (b'>', b'=', _) => adv!(3, ShrEq),
                (b'>', _, _) => adv!(2, Shr),
                (b'=', _, _) => adv!(2, GtEq),
                _ => adv!(1, Gt),
            },
            b'=' => match (self.peek_at(1), self.peek_at(2)) {
                (b'=', b'=') => adv!(3, EqEqEq),
                (b'=', _) => adv!(2, EqEq),
                (b'>', _) => adv!(2, Arrow),
                _ => adv!(1, Eq),
            },
            b'!' => match (self.peek_at(1), self.peek_at(2)) {
                (b'=', b'=') => adv!(3, NotEqEq),
                (b'=', _) => adv!(2, NotEq),
                _ => adv!(1, Bang),
            },
            b'+' => match self.peek_at(1) {
                b'+' => adv!(2, PlusPlus),
                b'=' => adv!(2, PlusEq),
                _ => adv!(1, Plus),
            },
            b'-' => match self.peek_at(1) {
                b'-' => adv!(2, MinusMinus),
                b'=' => adv!(2, MinusEq),
                _ => adv!(1, Minus),
            },
            b'*' => match (self.peek_at(1), self.peek_at(2)) {
                (b'*', b'=') => adv!(3, StarStarEq),
                (b'*', _) => adv!(2, StarStar),
                (b'=', _) => adv!(2, StarEq),
                _ => adv!(1, Star),
            },
            b'/' => match self.peek_at(1) {
                b'=' => adv!(2, SlashEq),
                _ => adv!(1, Slash),
            },
            b'%' => match self.peek_at(1) {
                b'=' => adv!(2, PercentEq),
                _ => adv!(1, Percent),
            },
            b'&' => match (self.peek_at(1), self.peek_at(2)) {
                (b'&', b'=') => adv!(3, AmpAmpEq),
                (b'&', _) => adv!(2, AmpAmp),
                (b'=', _) => adv!(2, AmpEq),
                _ => adv!(1, Amp),
            },
            b'|' => match (self.peek_at(1), self.peek_at(2)) {
                (b'|', b'=') => adv!(3, PipePipeEq),
                (b'|', _) => adv!(2, PipePipe),
                (b'=', _) => adv!(2, PipeEq),
                _ => adv!(1, Pipe),
            },
            b'^' => match self.peek_at(1) {
                b'=' => adv!(2, CaretEq),
                _ => adv!(1, Caret),
            },
            _ => {
                let (c, _) = if b < 0x80 { (b as char, 1) } else { self.char_at(self.pos) };
                Err(LexError::new(format!("unexpected character {c:?}"), start))
            }
        }
    }
}

#[inline]
fn hex_val(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u32),
        b'a'..=b'f' => Some((b - b'a' + 10) as u32),
        b'A'..=b'F' => Some((b - b'A' + 10) as u32),
        _ => None,
    }
}

/// Decimal string for a radix literal too large for `u128` (`0x1ffff...n`).
/// Schoolbook accumulate — BigInt literals this size are vanishingly rare and
/// this runs once at parse time.
fn big_radix_to_decimal(digits: &str, radix: u32) -> String {
    let mut acc: Vec<u8> = vec![0]; // little-endian decimal digits
    for c in digits.bytes() {
        let d = hex_val(c).unwrap_or(0);
        let mut carry = d;
        for slot in acc.iter_mut() {
            let v = (*slot as u32) * radix + carry;
            *slot = (v % 10) as u8;
            carry = v / 10;
        }
        while carry > 0 {
            acc.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    while acc.len() > 1 && *acc.last().unwrap() == 0 {
        acc.pop();
    }
    acc.iter().rev().map(|d| (b'0' + d) as char).collect()
}
