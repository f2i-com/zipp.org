//! The error the classic f-string parser (the one ZIPP's front end used
//! before PEP 701) reported for an f-string literal. It scanned the text
//! between the quotes itself and parsed each replacement field's text as
//! `(text)`; this reproduces its checks, in its order, for f-strings the
//! PEP 701 parser rejects, so a program that failed before fails with the
//! same message at the same offset. It is never used on success.

use crate::lexer::{Error, Limits, Mode};
use crate::strings::{self, Chars, Sink};
use crate::token::Token;

struct Discard;

impl Sink for Discard {
    fn push(&mut self, _: char) {}
}

fn fstring_err(message: &str, pos: u32) -> Error {
    Error::new(format!("f-string: {message}"), pos)
}

/// The classic error for the f-string `tok` (ending at `end`), if that
/// parser rejected it.
pub(crate) fn fstring_error(src: &str, base: u32, tok: &Token, end: u32) -> Option<Error> {
    let kind = tok.str_kind();
    let quotes = if tok.triple() { 3 } else { 1 };
    let start = tok.start + kind.prefix_len() + quotes;
    let content = &src[(start - base) as usize..(end - quotes - base) as usize];
    let mut classic = Classic {
        chars: Chars::new(content, start),
        raw: kind.is_raw(),
    };
    classic.fstring(0).err()
}

struct Classic<'a> {
    chars: Chars<'a>,
    raw: bool,
}

impl Classic<'_> {
    fn pos(&self) -> u32 {
        self.chars.pos
    }

    fn fstring(&mut self, nested: u8) -> Result<(), Error> {
        if nested >= 2 {
            return Err(fstring_err("expressions nested too deeply", self.pos()));
        }
        while let Some(ch) = self.chars.peek() {
            match ch {
                '{' => {
                    self.chars.next();
                    if nested == 0 {
                        match self.chars.peek() {
                            Some('{') => {
                                self.chars.next();
                                continue;
                            }
                            None => return Err(fstring_err("expecting '}'", self.pos())),
                            _ => {}
                        }
                    }
                    self.formatted_value(nested)?;
                }
                '}' => {
                    if nested > 0 {
                        break;
                    }
                    self.chars.next();
                    if self.chars.peek() == Some('}') {
                        self.chars.next();
                    } else {
                        return Err(fstring_err("single '}' is not allowed", self.pos()));
                    }
                }
                '\\' if !self.raw => {
                    self.chars.next();
                    if !matches!(self.chars.peek(), Some('{' | '}')) {
                        strings::escape(&mut self.chars, false, &mut Discard)?;
                    }
                }
                _ => {
                    self.chars.next();
                }
            }
        }
        Ok(())
    }

    fn formatted_value(&mut self, nested: u8) -> Result<(), Error> {
        let mut expression = String::new();
        let mut delimiters = Vec::new();
        let mut self_documenting = false;
        let location = self.pos();
        while let Some(ch) = self.chars.next() {
            match ch {
                '!' | '=' | '>' | '<' if self.chars.peek() == Some('=') => {
                    expression.push(ch);
                    expression.push('=');
                    self.chars.next();
                }
                '!' if delimiters.is_empty() && self.chars.peek() != Some('=') => {
                    if expression.trim().is_empty() {
                        return Err(fstring_err("empty expression not allowed", self.pos()));
                    }
                    match self.chars.next() {
                        Some('s' | 'a' | 'r') => {}
                        Some(_) => {
                            return Err(fstring_err("invalid conversion character", self.pos()))
                        }
                        None => return Err(fstring_err("expecting '}'", self.pos())),
                    }
                    if !matches!(self.chars.peek(), Some('}' | ':')) {
                        return Err(fstring_err("expecting '}'", self.pos()));
                    }
                }
                '=' if self.chars.peek() != Some('=') && delimiters.is_empty() => {
                    self_documenting = true;
                }
                ':' if delimiters.is_empty() => self.spec(nested)?,
                '(' | '{' | '[' => {
                    expression.push(ch);
                    delimiters.push(ch);
                }
                ')' | ']' => {
                    let open = if ch == ')' { '(' } else { '[' };
                    match delimiters.pop() {
                        Some(c) if c == open => expression.push(ch),
                        Some(c) => {
                            return Err(fstring_err(
                                &format!(
                                    "closing parenthesis '{ch}' does not match opening parenthesis '{c}'"
                                ),
                                self.pos(),
                            ))
                        }
                        None => return Err(fstring_err(&format!("unmatched '{ch}'"), self.pos())),
                    }
                }
                '}' if !delimiters.is_empty() => match delimiters.pop() {
                    Some('{') => expression.push(ch),
                    Some(c) => {
                        return Err(fstring_err(
                            &format!(
                                "closing parenthesis '}}' does not match opening parenthesis '{c}'"
                            ),
                            self.pos(),
                        ))
                    }
                    None => {}
                },
                '}' => {
                    if expression.trim().is_empty() {
                        return Err(fstring_err("empty expression not allowed", self.pos()));
                    }
                    let text = format!("({expression})");
                    if let Err(error) =
                        crate::parser::parse(&text, Mode::Expression, location - 1, Limits::NONE)
                    {
                        return Err(fstring_err(&error.message, location));
                    }
                    return Ok(());
                }
                '"' | '\'' => {
                    expression.push(ch);
                    loop {
                        let Some(c) = self.chars.next() else {
                            return Err(fstring_err("unterminated string", self.pos()));
                        };
                        expression.push(c);
                        if c == ch {
                            break;
                        }
                    }
                }
                ' ' if self_documenting => {}
                '\\' => return Err(fstring_err("unterminated string", self.pos())),
                _ => {
                    if self_documenting {
                        return Err(fstring_err("expecting '}'", self.pos()));
                    }
                    expression.push(ch);
                }
            }
        }
        Err(fstring_err("expecting '}'", self.pos()))
    }

    fn spec(&mut self, nested: u8) -> Result<(), Error> {
        while let Some(next) = self.chars.peek() {
            match next {
                '{' => {
                    self.fstring(nested + 1)?;
                    continue;
                }
                '}' => break,
                _ => {}
            }
            self.chars.next();
        }
        Ok(())
    }
}
