//! Runs of adjacent string literals: plain strings and bytes, and PEP 701
//! f-strings (parsed from their tokens: literal text, replacement fields,
//! conversions, nested format specs, `=` debugging text).
//!
//! The nodes are shaped as the RustPython 0.4 parser shaped them: adjacent
//! constants of a run merge into one spanning the run; replacement fields and
//! format specs span their own literal; a replacement field that is an
//! unparenthesized tuple or generator expression spans from its `{` (that
//! parser parsed the field's text as `(text)`); inside the fields of a
//! literal with `\r\n` line ends, offsets count each `\r\n` as one byte, as
//! it did. Errors are reported when the run is complete, the first in source
//! order, as that parser's classic f-string parser reported them (see
//! `compat`).

use super::{compat, range, Follow, PResult, Parser};
use crate::ast::*;
use crate::lexer::{self, Error};
use crate::strings;
use crate::token::{StrKind, Token, T};

impl Parser<'_> {
    /// One or more adjacent string literals.
    #[inline(never)]
    pub(super) fn parse_strings(&mut self, follow: Follow) -> PResult<ExprId> {
        let initial_start = self.start();
        let initial_u = self.token().str_kind() == StrKind::Unicode;
        let (mut total, mut bytes, mut has_f) = (0usize, 0usize, false);
        let mut i = self.pos;
        loop {
            let tok = self.toks[i];
            match tok.kind {
                T::String => {
                    if tok.str_kind().is_bytes() {
                        bytes += 1;
                    }
                }
                T::FStringStart => {
                    has_f = true;
                    i = tok.data as usize;
                }
                T::FStringBroken => has_f = true,
                _ => break,
            }
            total += 1;
            i += 1;
        }
        let run_end = i;
        let whole = range(initial_start, self.toks[run_end - 1].end);
        if bytes > 0 && bytes < total {
            self.skip_to(run_end);
            return Err(self.reduce_error(
                Error::new("cannot mix bytes and nonbytes literals", initial_start),
                follow,
            ));
        }
        if bytes > 0 {
            let start = self.m.bytes_data.len();
            while self.pos < run_end {
                let tok = self.token();
                let (content, pos) = self.string_content(&tok, tok.end);
                let raw = tok.str_kind().is_raw();
                if let Err(error) = strings::decode_bytes(content, pos, raw, &mut self.m.bytes_data)
                {
                    self.skip_to(run_end);
                    return Err(self.reduce_error(error, follow));
                }
                self.bump();
            }
            let value = StrRef {
                start: start as u32,
                len: (self.m.bytes_data.len() - start) as u32,
            };
            return Ok(self.add_expr(whole, ExprKind::Constant(Constant::Bytes { value })));
        }
        if !has_f {
            let start = self.m.str_data.len();
            while self.pos < run_end {
                let tok = self.token();
                let (content, pos) = self.string_content(&tok, tok.end);
                let raw = tok.str_kind().is_raw();
                if let Err(error) = strings::decode_str(content, pos, raw, &mut self.m.str_data) {
                    self.skip_to(run_end);
                    return Err(self.reduce_error(error, follow));
                }
                self.bump();
            }
            let value = StrRef {
                start: start as u32,
                len: (self.m.str_data.len() - start) as u32,
            };
            return Ok(self.add_expr(
                whole,
                ExprKind::Constant(Constant::Str {
                    value,
                    u: initial_u,
                }),
            ));
        }
        let mark = self.mark();
        let mut run = Run {
            text: std::mem::take(&mut self.run_buf),
            open: false,
            range: whole,
            u: initial_u,
        };
        run.text.clear();
        let base = self.s.exprs.len();
        let mut error = None;
        while self.pos < run_end {
            let tok = self.token();
            match tok.kind {
                T::String => {
                    let (content, pos) = self.string_content(&tok, tok.end);
                    let raw = tok.str_kind().is_raw();
                    if let Err(e) = strings::decode_str(content, pos, raw, &mut run.text) {
                        error = Some(e);
                        break;
                    }
                    run.open = true;
                    self.bump();
                }
                T::FStringStart => match self.parse_fstring(&mut run) {
                    Ok(()) => {}
                    Err(FStringError::Reduced(e)) => {
                        error = Some(e);
                        break;
                    }
                    Err(FStringError::Lexical(e)) => {
                        self.run_buf = run.text;
                        self.rewind(&mark);
                        return Err(self.fail(e));
                    }
                },
                _ => {
                    error = Some(self.broken_fstring_error(&tok));
                    break;
                }
            }
        }
        if let Some(error) = error {
            self.run_buf = run.text;
            self.rewind(&mark);
            // The classic lexer failed on a later f-string of the run (one
            // this parser read by PEP 701's rules): that token, and so that
            // error, came before the run's reduction.
            let mut i = self.pos;
            while i < run_end {
                let tok = self.toks[i];
                if tok.kind == T::FStringStart {
                    if let Err(e) = self.classic_end(&tok) {
                        return Err(self.fail(e));
                    }
                    i = tok.data as usize;
                }
                i += 1;
            }
            self.skip_to(run_end);
            return Err(self.reduce_error(error, follow));
        }
        self.flush(&mut run);
        self.run_buf = run.text;
        let values = self.finish_exprs(base);
        Ok(self.add_expr(whole, ExprKind::JoinedStr { values }))
    }

    /// Consume tokens up to `end` (after an error in a string run).
    fn skip_to(&mut self, end: usize) {
        self.pos = end;
        self.prev_end = self.toks[end - 1].end;
    }

    /// The run's pending literal text, as one constant.
    fn flush(&mut self, run: &mut Run) {
        if !run.open {
            return;
        }
        let start = self.m.str_data.len();
        self.m.str_data.push_str(&run.text);
        run.text.clear();
        run.open = false;
        let value = StrRef {
            start: start as u32,
            len: (self.m.str_data.len() - start) as u32,
        };
        let constant = self.add_expr(
            run.range,
            ExprKind::Constant(Constant::Str { value, u: run.u }),
        );
        self.s.exprs.push(constant);
    }

    fn str_constant(&mut self, text: &str, at: Range) -> ExprId {
        let start = self.m.str_data.len();
        self.m.str_data.push_str(text);
        let value = StrRef {
            start: start as u32,
            len: text.len() as u32,
        };
        self.add_expr(at, ExprKind::Constant(Constant::Str { value, u: false }))
    }

    /// An f-string from its `FStringStart` through its `FStringEnd`.
    fn parse_fstring(&mut self, run: &mut Run) -> Result<(), FStringError> {
        let start_index = self.pos;
        let tok = self.token();
        let end_index = tok.data as usize;
        let literal = range(tok.start, self.toks[end_index].end);
        let outermost = self.fstring_level == 0;
        if outermost {
            self.crlf.clear();
            let text =
                &self.src[(literal.start - self.base) as usize..(literal.end - self.base) as usize];
            if text.contains("\r\n") {
                let base = literal.start;
                self.crlf
                    .extend(text.match_indices("\r\n").map(|(i, _)| base + i as u32));
            }
        }
        self.fstring_level += 1;
        let result = self.fstring_body(run, literal, tok.str_kind().is_raw());
        self.fstring_level -= 1;
        match result {
            Ok(()) => Ok(()),
            Err(fail) => Err(self.fstring_error(start_index, end_index, fail.error)),
        }
    }

    fn fstring_body(&mut self, run: &mut Run, literal: Range, raw: bool) -> PResult<()> {
        self.bump(); // `FStringStart`
        let mut text = String::new();
        loop {
            match self.tok() {
                T::FStringMiddle => {
                    let tok = self.token();
                    text.clear();
                    let content = self.token_text(&tok);
                    strings::decode_fstring_middle(content, tok.start, raw, false, &mut text)
                        .map_err(|e| self.fail(e))?;
                    if !text.is_empty() {
                        run.text.push_str(&text);
                        run.open = true;
                    }
                    self.bump();
                }
                T::FieldStart => {
                    let (value, debug) = self.parse_field(literal, raw, 0)?;
                    if let Some((text_start, _, after)) = debug {
                        let text = self.debug_text(text_start, after);
                        run.text.push_str(&text);
                        run.open = true;
                    }
                    self.flush(run);
                    self.s.exprs.push(value);
                }
                T::FStringEnd => {
                    self.bump();
                    return Ok(());
                }
                _ => return Err(self.unexpected()),
            }
        }
    }

    fn token_text(&self, tok: &Token) -> &str {
        self.src_range(tok.start, tok.end)
    }

    fn src_range(&self, start: u32, end: u32) -> &str {
        &self.src[(start - self.base) as usize..(end - self.base) as usize]
    }

    /// The `=` debugging text of `start..end` as CPython keeps it: the source
    /// with comments (between tokens) removed and line ends read as `\n`.
    fn debug_text(&self, start: u32, end: u32) -> String {
        let text = self.src_range(start, end);
        if !text.contains('#') {
            return newlines(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut at = start;
        let mut i = self.toks.partition_point(|t| t.start < start);
        while at < end {
            let next = self.toks.get(i).map_or(end, |t| t.start.clamp(at, end));
            // The gap before the next token: whitespace, line ends,
            // continuations and comments.
            let mut gap = self.src_range(at, next);
            while let Some(hash) = gap.find('#') {
                out.push_str(&gap[..hash]);
                gap = &gap[hash..];
                gap = &gap[gap.find(['\n', '\r']).unwrap_or(gap.len())..];
            }
            out.push_str(gap);
            if next >= end {
                break;
            }
            let token_end = self.toks[i].end.min(end);
            out.push_str(self.src_range(next, token_end));
            at = token_end;
            i += 1;
        }
        newlines(&out)
    }

    /// A replacement field: returns the `FormattedValue` and, for `=`
    /// debugging, where its text starts (after the `{`), the `=` offset and
    /// where the whitespace after the `=` ends.
    fn parse_field(
        &mut self,
        literal: Range,
        raw: bool,
        spec_depth: u32,
    ) -> PResult<(ExprId, Option<DebugText>)> {
        let lbrace = self.start();
        self.bump(); // `{`
        self.enter()?;
        let window = if self.fstring_level == 1 && !self.crlf.is_empty() {
            Some(self.node_counts())
        } else {
            None
        };
        let value = self.parse_field_expr(lbrace)?;
        if let Some(window) = window {
            self.shift(window);
        }
        let mut debug = None;
        if self.at(T::Equal) {
            let eq = self.start();
            self.bump();
            debug = Some((lbrace + 1, eq, self.start()));
        }
        let mut conversion = Conversion::None;
        if self.at(T::Exclamation) {
            let bang = self.token();
            self.bump();
            let tok = self.token();
            if tok.kind != T::Name || tok.start != bang.end {
                return Err(self.unexpected());
            }
            conversion = match self.token_text(&tok) {
                "s" => Conversion::Str,
                "r" => Conversion::Repr,
                "a" => Conversion::Ascii,
                _ => return Err(self.unexpected()),
            };
            self.bump();
        }
        let mut format_spec = None;
        if self.at(T::FormatSpec) {
            if spec_depth >= 2 {
                return Err(self.fail(Error::new(
                    "f-string: expressions nested too deeply",
                    self.start(),
                )));
            }
            self.bump();
            let base = self.s.exprs.len();
            let mut text = String::new();
            loop {
                match self.tok() {
                    T::FStringMiddle => {
                        let tok = self.token();
                        text.clear();
                        let content = self.token_text(&tok);
                        strings::decode_fstring_middle(content, tok.start, raw, true, &mut text)
                            .map_err(|e| self.fail(e))?;
                        if !text.is_empty() {
                            let constant = self.str_constant(&text, literal);
                            self.s.exprs.push(constant);
                        }
                        self.bump();
                    }
                    T::FieldStart => {
                        let (value, debug) = self.parse_field(literal, raw, spec_depth + 1)?;
                        if let Some((text_start, eq, after)) = debug {
                            let expression = self.debug_text(text_start, eq + 1);
                            let trailing = self.debug_text(eq + 1, after);
                            let first = self.str_constant(&expression, literal);
                            self.s.exprs.push(first);
                            let second = self.str_constant(&trailing, literal);
                            self.s.exprs.push(second);
                        }
                        self.s.exprs.push(value);
                    }
                    _ => break,
                }
            }
            let values = self.finish_exprs(base);
            format_spec = Some(self.add_expr(literal, ExprKind::JoinedStr { values }));
        }
        if !self.at(T::FieldEnd) {
            return Err(self.unexpected());
        }
        self.bump();
        self.leave();
        if debug.is_some() && conversion == Conversion::None && format_spec.is_none() {
            conversion = Conversion::Repr;
        }
        let fv = self.add_expr(
            literal,
            ExprKind::FormattedValue {
                value,
                conversion,
                format_spec,
            },
        );
        Ok((fv, debug))
    }

    /// A replacement field's expression: what may stand between parentheses
    /// (the classic parser parsed `(text)`), plus a lone starred expression.
    fn parse_field_expr(&mut self, lbrace: u32) -> PResult<ExprId> {
        match self.tok() {
            T::Yield => return self.parse_yield(),
            T::FieldEnd | T::FormatSpec | T::Exclamation | T::Equal | T::DoubleStar => {
                return Err(self.unexpected())
            }
            _ => {}
        }
        let (first, kind) = self.parse_elem()?;
        let quirk = |p: &Self| range(lbrace, p.start() + 1);
        if kind != super::expr::Kind::Star && matches!(self.tok(), T::For | T::Async) {
            let generators = self.parse_comp_for()?;
            let at = quirk(self);
            return Ok(self.add_expr(
                at,
                ExprKind::GeneratorExp {
                    elt: first,
                    generators,
                },
            ));
        }
        if !self.at(T::Comma) {
            return Ok(first);
        }
        let base = self.s.exprs.len();
        self.s.exprs.push(first);
        while self.at(T::Comma) {
            self.bump();
            if matches!(
                self.tok(),
                T::FieldEnd | T::FormatSpec | T::Exclamation | T::Equal
            ) {
                break;
            }
            let elt = self.parse_elem()?.0;
            self.s.exprs.push(elt);
        }
        let elts = self.finish_exprs(base);
        let at = quirk(self);
        Ok(self.add_expr(
            at,
            ExprKind::Tuple {
                elts,
                ctx: ExprContext::Load,
            },
        ))
    }

    /// The module's table lengths before a field's expression: everything
    /// the field builds lands after them (its lists are complete by then).
    fn node_counts(&self) -> [usize; 5] {
        [
            self.m.exprs.len(),
            self.m.keywords.len(),
            self.m.comprehensions.len(),
            self.m.params.len(),
            self.m.arguments.len(),
        ]
    }

    /// Count each `\r\n` of the outermost literal as one byte in the ranges
    /// of the nodes a field built (see the module docs).
    fn shift(&mut self, window: [usize; 5]) {
        let crlf = std::mem::take(&mut self.crlf);
        let map = |offset: u32| offset - crlf.partition_point(|&at| at < offset) as u32;
        let fix = |r: &mut Range| {
            r.start = map(r.start);
            r.end = map(r.end);
        };
        for expr in &mut self.m.exprs[window[0]..] {
            fix(&mut expr.range);
        }
        for keyword in &mut self.m.keywords[window[1]..] {
            fix(&mut keyword.range);
        }
        for comprehension in &mut self.m.comprehensions[window[2]..] {
            fix(&mut comprehension.range);
        }
        for param in &mut self.m.params[window[3]..] {
            fix(&mut param.range);
        }
        for arguments in &mut self.m.arguments[window[4]..] {
            fix(&mut arguments.range);
        }
        self.crlf = crlf;
    }

    /// The error an f-string that failed to parse reports: the classic
    /// f-string parser's, when the classic lexer delimited the same literal.
    /// Where the classic lexer ended the string `tok` starts, or its error.
    fn classic_end(&self, tok: &Token) -> Result<u32, Error> {
        let quote_at = (tok.start + tok.str_kind().prefix_len() - self.base) as usize;
        lexer::classic_end(self.src.as_bytes(), self.base, quote_at)
            .map(|(end, _)| self.base + end as u32)
    }

    /// The error an f-string that failed to parse reports: the classic
    /// lexer's, if it could not delimit the literal (reported where the
    /// token is); else the classic f-string parser's, when that lexer
    /// delimited the same literal; else this parser's.
    fn fstring_error(&self, start_index: usize, end_index: usize, error: Error) -> FStringError {
        let tok = self.toks[start_index];
        let end = self.toks[end_index].end;
        match self.classic_end(&tok) {
            Err(e) => FStringError::Lexical(e),
            Ok(classic_end) if classic_end == end => FStringError::Reduced(
                compat::fstring_error(self.src, self.base, &tok, end).unwrap_or(error),
            ),
            Ok(_) => FStringError::Reduced(error),
        }
    }

    fn broken_fstring_error(&self, tok: &Token) -> Error {
        compat::fstring_error(self.src, self.base, tok, tok.end)
            .unwrap_or_else(|| self.errors[tok.data as usize].clone())
    }
}

/// Where a field's `=` debugging text starts, the `=` offset and where the
/// whitespace after the `=` ends.
type DebugText = (u32, u32, u32);

/// Debugging text with its line ends read as `\n` (as all literal text).
fn newlines(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_owned()
    }
}

/// Why an f-string failed: an error the string rule reports when it is
/// reduced, or the classic lexer's error for the token itself.
enum FStringError {
    Reduced(Error),
    Lexical(Error),
}

/// The literal text of a string run not yet made a constant.
pub(super) struct Run {
    text: String,
    open: bool,
    range: Range,
    u: bool,
}
