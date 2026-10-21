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
