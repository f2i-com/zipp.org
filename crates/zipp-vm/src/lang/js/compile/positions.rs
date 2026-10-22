//! Source offsets to the `line:col` a stack frame reports (#78): 1-based,
//! with the column counted in UTF-16 code units, as V8 and browsers count it.

/// One source's line starts and non-ASCII characters, built once per compile
/// so that each call site's position costs two binary searches, however long
/// its line: a minified bundle is one line of megabytes.
pub(crate) struct SourceIndex {
    /// Byte offset of each line's first character; line 1 starts at 0.
    line_starts: Vec<u32>,
    /// For each non-ASCII character, its byte offset and the total by which
    /// the UTF-8 length of it and every earlier one exceeds their UTF-16
    /// length.
    wide: Vec<(u32, u32)>,
}

impl SourceIndex {
    pub(crate) fn new(src: &str) -> SourceIndex {
        let bytes = src.as_bytes();
        let mut line_starts = vec![0u32];
        let mut wide = Vec::new();
        let mut extra = 0u32;
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if b < 0x80 {
                // LineTerminator: LF, CR, and CR LF as one.
                if b == b'\n' || (b == b'\r' && bytes.get(i + 1) != Some(&b'\n')) {
                    line_starts.push(i as u32 + 1);
                }
                i += 1;
                continue;
            }
            // A lead byte: 2 and 3 byte forms are one UTF-16 unit, 4 bytes two.
            let (len, units): (u32, u32) = match b {
                0xF0..=0xFF => (4, 2),
                0xE0..=0xEF => (3, 1),
                _ => (2, 1),
            };
            extra += len - units;
            wide.push((i as u32, extra));
            // U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR end lines.
            if b == 0xE2 && bytes.get(i + 1) == Some(&0x80) && matches!(bytes.get(i + 2), Some(0xA8 | 0xA9)) {
                line_starts.push(i as u32 + 3);
            }
            i += len as usize;
        }
        SourceIndex { line_starts, wide }
    }

    /// The UTF-8 bytes beyond UTF-16 units that the characters before byte
    /// `off` add up to.
    fn extra_before(&self, off: u32) -> u32 {
        match self.wide.partition_point(|&(o, _)| o < off) {
            0 => 0,
            n => self.wide[n - 1].1,
        }
    }

    /// The `(line, column)` of byte offset `off`.
    pub(crate) fn line_col(&self, off: u32) -> (u32, u32) {
        let line = self.line_starts.partition_point(|&s| s <= off).max(1);
        let start = self.line_starts[line - 1];
        let units = (off - start) - (self.extra_before(off) - self.extra_before(start));
        (line as u32, units + 1)
    }
}

/// Where the host's own text sits in what the compiler was handed, so that
/// positions count from it: an embedding preamble ahead of an initial script
/// (`compile_script_with_preamble`), or the expression wrapper an embedder's
/// `eval_wrapped_in_context` puts around an expression. Positions in `guest`
/// are reported as the host's script would number them, with the host's own
/// offsets (Node's `vm.Script` `lineOffset` and `columnOffset`) added; a
/// position outside it is plumbing ([`CallSite::PLUMBING`],
/// [`CallSite::HIDDEN`]).
///
/// [`CallSite::PLUMBING`]: crate::bytecode::CallSite::PLUMBING
/// [`CallSite::HIDDEN`]: crate::bytecode::CallSite::HIDDEN
#[derive(Clone, Debug)]
pub(crate) struct SourceOrigin {
    /// Byte range of the host's text in the compiled source.
    pub(crate) guest: std::ops::Range<u32>,
    /// Whether text before `guest` is a preamble, whose frames a stack names
    /// `zipp:preamble`, rather than a wrapper whose frames it leaves out.
    /// Text after `guest` is always a wrapper's.
    pub(crate) preamble: bool,
    /// Added to every line in `guest`.
    pub(crate) line_offset: i32,
    /// Added to the columns of `guest`'s first line.
    pub(crate) column_offset: i32,
}

impl SourceOrigin {
    /// The `(line, col)` a call site at byte `off` records, `idx` being the
    /// whole compiled source's index and `(line, col)` its position there: a
    /// host position, a plumbing marker, or `(0, 0)` for a host position the
    /// offsets moved before line 1 or column 1, which a stack reports
    /// without a position.
    pub(crate) fn place(&self, idx: &SourceIndex, off: u32, line: u32, col: u32) -> (u32, u32) {
        use crate::bytecode::CallSite;
        if off < self.guest.start {
            return if self.preamble { (line | CallSite::PLUMBING, col) } else { (CallSite::HIDDEN, 0) };
        }
        if off >= self.guest.end {
            return (CallSite::HIDDEN, 0);
        }
        self.host_position(idx, line, col)
    }

    /// The host's `(line, col)` for whole-source position `(line, col)` in
    /// `guest`, or `(0, 0)` (see [`SourceOrigin::place`]).
    fn host_position(&self, idx: &SourceIndex, line: u32, col: u32) -> (u32, u32) {
        let (first_line, first_col) = idx.line_col(self.guest.start);
        let line = (line - first_line + 1) as i64;
        let (line, col) = if line == 1 {
            (line + self.line_offset as i64, (col - (first_col - 1)) as i64 + self.column_offset as i64)
        } else {
            (line + self.line_offset as i64, col as i64)
        };
        if line < 1 || col < 1 || line >= crate::bytecode::CallSite::PLUMBING as i64 {
            return (0, 0);
        }
        (line as u32, col as u32)
    }

    /// Where a syntax error at byte `pos` of `src`, the whole compiled
    /// source, sits in the host's text; an error at the end of the input is
    /// at the end of `guest`. `None` when the offsets moved it before line 1.
    pub(crate) fn locate(&self, src: &str, pos: u32) -> Option<(u32, u32)> {
        let idx = SourceIndex::new(src);
        let pos = pos.clamp(self.guest.start, self.guest.end.min(src.len() as u32));
        let (line, col) = idx.line_col(pos);
        match self.host_position(&idx, line, col) {
            (0, _) => None,
            located => Some(located),
        }
    }
}

/// `message` with the position of byte `pos` of `src` appended as a stack
/// frame: `SyntaxError: ...\n    at name:line:col`. `origin` maps it into the
/// host's text when the source was not the host's alone; without it the
/// position is `src`'s own.
pub(crate) fn locate_message(
    message: &str,
    src: &str,
    pos: u32,
    name: Option<&str>,
    origin: Option<&SourceOrigin>,
) -> String {
    let at = match origin {
        Some(origin) => origin.locate(src, pos),
        None => Some(SourceIndex::new(src).line_col(pos.min(src.len() as u32))),
    };
    match at {
        Some((line, col)) => format!("{message}\n    at {}:{line}:{col}", name.unwrap_or("<anonymous>")),
        None => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::SourceIndex;

    #[test]
    fn lines_and_utf16_columns() {
        let src = "ab\ncd\r\nef\rgh\u{2028}ij\n\u{e9}\u{1F600}x";
        let idx = SourceIndex::new(src);
        let at = |needle: &str| idx.line_col(src.find(needle).unwrap() as u32);
        assert_eq!(at("a"), (1, 1));
        assert_eq!(at("b"), (1, 2));
        assert_eq!(at("c"), (2, 1));
        assert_eq!(at("e"), (3, 1));
        assert_eq!(at("g"), (4, 1));
        assert_eq!(at("i"), (5, 1));
        // é is one UTF-16 unit, the emoji two: x is the fourth unit.
        assert_eq!(at("x"), (6, 4));
    }
}
