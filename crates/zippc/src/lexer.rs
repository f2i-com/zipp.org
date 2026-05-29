//! Hand-written lexer for the ZIPP v0 subset.
//!
//! PLAN.md calls for reusing the oxc/SWC TS parser; for the v0 spike a small
//! self-contained lexer keeps the build dependency-free. Swapping in oxc is a
//! frontend-only change behind [`crate::parser`].
//!
//! Each token carries its 1-based source `line`/`col` so the parser and
//! checker can point at where an error is.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // literals / identifiers
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),
    // keywords
    Fn,
    Struct,
    Let,
    Return,
    If,
    Else,
    While,
    For,
    Break,
    Continue,
    True,
    False,
    Print,
    TyI64,
    TyF64,
    TyBool,
    TyStr,
    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Semi,
    Dot,
    Assign, // =
    // operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang, // !
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
    Shl,
    Shr,
}

/// A token plus its 1-based source position.
#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
}

/// Map a byte offset to a 1-based (line, col) using a precomputed line table.
fn to_pos(line_starts: &[usize], off: usize) -> (u32, u32) {
    let line = line_starts.partition_point(|&s| s <= off); // 1-based
    let col = off - line_starts[line - 1] + 1;
    (line as u32, col as u32)
}

pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    let bytes = src.as_bytes();

    // Byte offset where each line begins (line_starts[0] = 0).
    let mut line_starts = vec![0usize];
    for (k, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            line_starts.push(k + 1);
        }
    }
    let lerr = |msg: String, off: usize| -> String {
        let (l, c) = to_pos(&line_starts, off);
        format!("{msg} (line {l}:{c})")
    };

    let mut i = 0usize;
    let mut raw: Vec<(Tok, usize)> = Vec::new();
    // Offset of the current token's first byte; set at the top of each iteration.
    let mut tok_start: usize;

    // Push a token tagged with the offset of its first byte.
    macro_rules! emit {
        ($t:expr) => {
            raw.push(($t, tok_start))
        };
    }

    while i < bytes.len() {
        tok_start = i;
        let c = bytes[i] as char;
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                i += 1;
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '(' => {
                emit!(Tok::LParen);
                i += 1;
            }
            ')' => {
                emit!(Tok::RParen);
                i += 1;
            }
            '{' => {
                emit!(Tok::LBrace);
                i += 1;
            }
            '}' => {
                emit!(Tok::RBrace);
                i += 1;
            }
            '[' => {
                emit!(Tok::LBracket);
                i += 1;
            }
            ']' => {
                emit!(Tok::RBracket);
                i += 1;
            }
            ',' => {
                emit!(Tok::Comma);
                i += 1;
            }
            ':' => {
                emit!(Tok::Colon);
                i += 1;
            }
            ';' => {
                emit!(Tok::Semi);
                i += 1;
            }
            '.' => {
                emit!(Tok::Dot);
                i += 1;
            }
            '+' => {
                emit!(Tok::Plus);
                i += 1;
            }
            '-' => {
                emit!(Tok::Minus);
                i += 1;
            }
            '*' => {
                emit!(Tok::Star);
                i += 1;
            }
            '/' => {
                emit!(Tok::Slash);
                i += 1;
            }
            '%' => {
                emit!(Tok::Percent);
                i += 1;
            }
            '=' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    emit!(Tok::EqEq);
                    i += 2;
                } else {
                    emit!(Tok::Assign);
                    i += 1;
                }
            }
            '!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    emit!(Tok::NotEq);
                    i += 2;
                } else {
                    emit!(Tok::Bang);
                    i += 1;
                }
            }
            '<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'<' {
                    emit!(Tok::Shl);
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    emit!(Tok::Le);
                    i += 2;
                } else {
                    emit!(Tok::Lt);
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    emit!(Tok::Shr);
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    emit!(Tok::Ge);
                    i += 2;
                } else {
                    emit!(Tok::Gt);
                    i += 1;
                }
            }
            '&' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                    emit!(Tok::AndAnd);
                    i += 2;
                } else {
                    emit!(Tok::BitAnd);
                    i += 1;
                }
            }
            '|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    emit!(Tok::OrOr);
                    i += 2;
                } else {
                    emit!(Tok::BitOr);
                    i += 1;
                }
            }
            '^' => {
                emit!(Tok::BitXor);
                i += 1;
            }
            '~' => {
                emit!(Tok::BitNot);
                i += 1;
            }
            '"' => {
                i += 1; // opening quote
                let mut s = String::new();
                loop {
                    if i >= bytes.len() {
                        return Err(lerr("lex error: unterminated string literal".into(), tok_start));
                    }
                    let ch = bytes[i] as char;
                    if ch == '"' {
                        i += 1;
                        break;
                    }
                    if ch == '\\' {
                        i += 1;
                        if i >= bytes.len() {
                            return Err(lerr("lex error: unterminated escape in string".into(), tok_start));
                        }
                        s.push(match bytes[i] as char {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '"' => '"',
                            '\\' => '\\',
                            '0' => '\0',
                            other => return Err(lerr(format!("lex error: unknown escape '\\{other}'"), i)),
                        });
                        i += 1;
                    } else {
                        s.push(ch);
                        i += 1;
                    }
                }
                emit!(Tok::Str(s));
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                // Float literal: digits '.' digits  (e.g. 3.14, 2.0).
                let is_float = i + 1 < bytes.len()
                    && bytes[i] == b'.'
                    && (bytes[i + 1] as char).is_ascii_digit();
                if is_float {
                    i += 1; // consume '.'
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                    let s = &src[start..i];
                    let f: f64 = s
                        .parse()
                        .map_err(|_| lerr(format!("lex error: bad float literal '{s}'"), start))?;
                    emit!(Tok::Float(f));
                } else {
                    let s = &src[start..i];
                    let n: i64 = s
                        .parse()
                        .map_err(|_| lerr(format!("lex error: bad integer literal '{s}'"), start))?;
                    emit!(Tok::Int(n));
                }
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < bytes.len()
                    && ((bytes[i] as char).is_ascii_alphanumeric() || bytes[i] == b'_')
                {
                    i += 1;
                }
                let s = &src[start..i];
                emit!(match s {
                    "fn" => Tok::Fn,
                    "struct" => Tok::Struct,
                    "let" => Tok::Let,
                    "return" => Tok::Return,
                    "if" => Tok::If,
                    "else" => Tok::Else,
                    "while" => Tok::While,
                    "for" => Tok::For,
                    "break" => Tok::Break,
                    "continue" => Tok::Continue,
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "print" => Tok::Print,
                    "i64" => Tok::TyI64,
                    "f64" => Tok::TyF64,
                    "bool" => Tok::TyBool,
                    "str" => Tok::TyStr,
                    _ => Tok::Ident(s.to_string()),
                });
            }
            other => return Err(lerr(format!("lex error: unexpected character '{other}'"), tok_start)),
        }
    }

    Ok(raw
        .into_iter()
        .map(|(tok, off)| {
            let (line, col) = to_pos(&line_starts, off);
            Token { tok, line, col }
        })
        .collect())
}
