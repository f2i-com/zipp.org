//! Hand-written lexer for the ZIPP v0 subset.
//!
//! PLAN.md calls for reusing the oxc/SWC TS parser; for the v0 spike a small
//! self-contained lexer keeps the build dependency-free. Swapping in oxc is a
//! frontend-only change behind [`crate::parser`].

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // literals / identifiers
    Int(i64),
    Ident(String),
    // keywords
    Fn,
    Let,
    Return,
    If,
    Else,
    While,
    True,
    False,
    Print,
    TyI64,
    TyBool,
    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
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
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::Le);
                    i += 2;
                } else {
                    out.push(Tok::Lt);
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
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
                    return Err("lex error: '&' (did you mean '&&'?)".into());
                }
            }
            '|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    out.push(Tok::OrOr);
                    i += 2;
                } else {
                    return Err("lex error: '|' (did you mean '||'?)".into());
                }
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                let s = &src[start..i];
                let n: i64 = s
                    .parse()
                    .map_err(|_| format!("lex error: bad integer literal '{s}'"))?;
                out.push(Tok::Int(n));
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
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "print" => Tok::Print,
                    "i64" => Tok::TyI64,
                    "bool" => Tok::TyBool,
                    _ => Tok::Ident(s.to_string()),
                });
            }
            other => return Err(format!("lex error: unexpected character '{other}'")),
        }
    }
    Ok(out)
}
