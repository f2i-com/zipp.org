//! Hand-written lexer for the ZIPP v0 subset.
//!
//! PLAN.md calls for reusing the oxc/SWC TS parser; for the v0 spike a small
//! self-contained lexer keeps the build dependency-free. Swapping in oxc is a
//! frontend-only change behind [`crate::parser`].

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // literals / identifiers
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),
    // keywords
    Fn,
    Let,
    Return,
    If,
    Else,
    While,
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

pub fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            // whitespace
            ' ' | '\t' | '\r' | '\n' => {
                i += 1;
            }
            // line comments
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '{' => {
                out.push(Tok::LBrace);
                i += 1;
            }
            '}' => {
                out.push(Tok::RBrace);
                i += 1;
            }
            '[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            ']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            ':' => {
                out.push(Tok::Colon);
                i += 1;
            }
            ';' => {
                out.push(Tok::Semi);
                i += 1;
            }
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            '*' => {
                out.push(Tok::Star);
                i += 1;
            }
            '/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            '%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            '=' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::EqEq);
                    i += 2;
                } else {
                    out.push(Tok::Assign);
                    i += 1;
                }
            }
            '!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::NotEq);
                    i += 2;
                } else {
                    out.push(Tok::Bang);
                    i += 1;
                }
            }
            '<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'<' {
                    out.push(Tok::Shl);
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::Le);
                    i += 2;
                } else {
                    out.push(Tok::Lt);
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    out.push(Tok::Shr);
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::Ge);
                    i += 2;
                } else {
                    out.push(Tok::Gt);
                    i += 1;
                }
            }
            '&' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                    out.push(Tok::AndAnd);
                    i += 2;
                } else {
                    out.push(Tok::BitAnd);
                    i += 1;
                }
            }
            '|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    out.push(Tok::OrOr);
                    i += 2;
                } else {
                    out.push(Tok::BitOr);
                    i += 1;
                }
            }
            '^' => {
                out.push(Tok::BitXor);
                i += 1;
            }
            '~' => {
                out.push(Tok::BitNot);
                i += 1;
            }
            '"' => {
                i += 1; // opening quote
                let mut s = String::new();
                loop {
                    if i >= bytes.len() {
                        return Err("lex error: unterminated string literal".into());
                    }
                    let ch = bytes[i] as char;
                    if ch == '"' {
                        i += 1;
                        break;
                    }
                    if ch == '\\' {
                        i += 1;
                        if i >= bytes.len() {
                            return Err("lex error: unterminated escape in string".into());
                        }
                        s.push(match bytes[i] as char {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '"' => '"',
                            '\\' => '\\',
                            '0' => '\0',
                            other => return Err(format!("lex error: unknown escape '\\{other}'")),
                        });
                        i += 1;
                    } else {
                        s.push(ch);
                        i += 1;
                    }
                }
                out.push(Tok::Str(s));
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
                        .map_err(|_| format!("lex error: bad float literal '{s}'"))?;
                    out.push(Tok::Float(f));
                } else {
                    let s = &src[start..i];
                    let n: i64 = s
                        .parse()
                        .map_err(|_| format!("lex error: bad integer literal '{s}'"))?;
                    out.push(Tok::Int(n));
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
                out.push(match s {
                    "fn" => Tok::Fn,
                    "let" => Tok::Let,
                    "return" => Tok::Return,
                    "if" => Tok::If,
                    "else" => Tok::Else,
                    "while" => Tok::While,
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
            other => return Err(format!("lex error: unexpected character '{other}'")),
        }
    }
    Ok(out)
}
