//! String and bytes literal contents: escapes decoded with the classic
//! rules, errors at the offsets the classic parser reported (it counted a
//! `\r\n` inside a literal as one character).

use crate::lexer::{msg, Error};
use crate::unicode_names;

/// The longest `\N{...}` name looked up.
const MAX_UNICODE_NAME: usize = 88;

/// A cursor over a literal's content: characters with `\r\n` and `\r` read
/// as `\n`, and a position that advances by the character read.
pub(crate) struct Chars<'a> {
    rest: &'a str,
    pub pos: u32,
}

impl<'a> Chars<'a> {
    pub fn new(content: &'a str, pos: u32) -> Self {
        Chars { rest: content, pos }
    }

    #[inline]
    pub fn peek(&self) -> Option<char> {
        let c = self.rest.chars().next()?;
        Some(if c == '\r' { '\n' } else { c })
    }

    #[inline]
    pub fn next(&mut self) -> Option<char> {
        let c = self.rest.chars().next()?;
        if c == '\r' {
            let skip = if self.rest.as_bytes().get(1) == Some(&b'\n') {
                2
            } else {
                1
            };
            self.rest = &self.rest[skip..];
            self.pos += 1;
            return Some('\n');
        }
        self.rest = &self.rest[c.len_utf8()..];
        self.pos += c.len_utf8() as u32;
        Some(c)
    }
}

/// Where decoded characters go.
pub(crate) trait Sink {
    fn push(&mut self, c: char);
}

impl Sink for String {
    #[inline]
    fn push(&mut self, c: char) {
        String::push(self, c)
    }
}

/// Bytes are the characters' low bytes, as the classic parser made them.
impl Sink for Vec<u8> {
    #[inline]
    fn push(&mut self, c: char) {
        Vec::push(self, c as u32 as u8)
    }
}

/// Decode the escape after a backslash (already read).
pub(crate) fn escape(chars: &mut Chars, bytes: bool, out: &mut impl Sink) -> Result<(), Error> {
    let Some(c) = chars.next() else {
        return Err(Error::new(msg::STRING, chars.pos));
    };
    let decoded = match c {
        '\\' => '\\',
        '\'' => '\'',
        '"' => '"',
        'a' => '\x07',
        'b' => '\x08',
        'f' => '\x0c',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'v' => '\x0b',
        '0'..='7' => {
            let mut value = c as u32 - '0' as u32;
            for _ in 0..2 {
                match chars.peek() {
                    Some(d @ '0'..='7') => {
                        chars.next();
                        value = value * 8 + (d as u32 - '0' as u32);
                    }
                    _ => break,
                }
            }
            char::from_u32(value).unwrap()
        }
        'x' => hex(chars, 2)?,
        'u' if !bytes => hex(chars, 4)?,
        'U' if !bytes => hex(chars, 8)?,
        'N' if !bytes => named(chars)?,
        '\n' => return Ok(()),
        c => {
            if bytes && !c.is_ascii() {
                return Err(Error::new(
                    "bytes can only contain ASCII literal characters",
                    chars.pos,
                ));
            }
            out.push('\\');
            c
        }
    };
    out.push(decoded);
    Ok(())
}

fn hex(chars: &mut Chars, digits: u32) -> Result<char, Error> {
    let error = Error::new(msg::UNICODE, chars.pos);
    let mut value: u32 = 0;
    for i in 1..=digits {
        match chars.next().and_then(|c| c.to_digit(16)) {
            Some(d) => value += d << ((digits - i) * 4),
            None => return Err(error),
        }
    }
    match value {
        0xD800..=0xDFFF => Ok(char::REPLACEMENT_CHARACTER),
        _ => char::from_u32(value).ok_or(error),
    }
}

fn named(chars: &mut Chars) -> Result<char, Error> {
    if chars.next() != Some('{') {
        return Err(Error::new(msg::STRING, chars.pos));
    }
    let start = chars.pos;
    let mut name = String::new();
    loop {
        match chars.next() {
            Some('}') => break,
            Some(c) => name.push(c),
            None => return Err(Error::new(msg::STRING, chars.pos)),
        }
    }
    if name.len() > MAX_UNICODE_NAME {
        return Err(Error::new(msg::UNICODE, chars.pos));
    }
    unicode_names::lookup(&name).ok_or_else(|| {
        Error::new(
            format!(
                "(unicode error) \\N{{{name}}}: unknown Unicode character name \
                 (this build knows common names only; spell the character or use \\u/\\U)"
            ),
            start,
        )
    })
}

/// A string literal's content (between the quotes).
pub(crate) fn decode_str(
    content: &str,
    pos: u32,
    raw: bool,
    out: &mut String,
) -> Result<(), Error> {
    if raw || !content.contains(['\\', '\r']) {
        if content.contains('\r') {
            let mut chars = Chars::new(content, pos);
            while let Some(c) = chars.next() {
                out.push(c);
            }
        } else {
            out.push_str(content);
        }
        return Ok(());
    }
    let mut chars = Chars::new(content, pos);
    while let Some(c) = chars.next() {
        if c == '\\' {
            escape(&mut chars, false, out)?;
        } else {
            out.push(c);
        }
    }
    Ok(())
}

/// A bytes literal's content.
pub(crate) fn decode_bytes(
    content: &str,
    pos: u32,
    raw: bool,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    let mut chars = Chars::new(content, pos);
    while let Some(c) = chars.next() {
        if c == '\\' && !raw {
            escape(&mut chars, true, out)?;
        } else if !c.is_ascii() {
            return Err(Error::new(
                "bytes can only contain ASCII literal characters",
                chars.pos,
            ));
        } else {
            out.push(c as u8);
        }
    }
    Ok(())
}

/// Literal text of an f-string (`spec`: of a format spec, where braces are
/// not doubled). A backslash before a brace is a literal backslash.
pub(crate) fn decode_fstring_middle(
    content: &str,
    pos: u32,
    raw: bool,
    spec: bool,
    out: &mut String,
) -> Result<(), Error> {
    let mut chars = Chars::new(content, pos);
    while let Some(c) = chars.next() {
        match c {
            '\\' if !raw => {
                if matches!(chars.peek(), Some('{' | '}') | None) {
                    out.push('\\');
                } else {
                    escape(&mut chars, false, out)?;
                }
            }
            '{' | '}' if !spec => {
                // Doubled: `{{` is `{`.
                chars.next();
                out.push(c);
            }
            c => out.push(c),
        }
    }
    Ok(())
}
