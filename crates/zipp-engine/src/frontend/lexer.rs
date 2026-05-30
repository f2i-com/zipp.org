use crate::config::ZippConfig;
use crate::token::{lookup_ident, Token, TokenType};

#[derive(Clone)]
pub struct Lexer<'a> {
    input: &'a [u8],
    position: usize,
    read_position: usize,
    ch: u8,
    line: usize,
    column: usize,
    saw_newline: bool,
    last_token_type: Option<TokenType>,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str, _config: ZippConfig) -> Self {
        let mut lexer = Self {
            input: input.as_bytes(),
            position: 0,
            read_position: 0,
            ch: 0,
            line: 1,
            column: 0,
            saw_newline: false,
            last_token_type: None,
        };
        lexer.read_char();
        lexer
    }

    #[inline(always)]
    fn read_char(&mut self) {
        if self.read_position >= self.input.len() {
            self.ch = 0;
        } else {
            self.ch = self.input[self.read_position];
        }

        if self.ch == b'\n' {
            self.line += 1;
            self.column = 0;
        } else {
            self.column += 1;
        }

        self.position = self.read_position;
        self.read_position += 1;
    }

    #[inline(always)]
    fn peek_char(&self) -> u8 {
        if self.read_position >= self.input.len() {
            0
        } else {
            self.input[self.read_position]
        }
    }

    pub fn next_token(&mut self) -> Token {
        self.saw_newline = false;
        loop {
            self.skip_whitespace();
            let pos_before = self.position;
            self.skip_comments();
            if self.position == pos_before {
                break;
            }
        }
        self.skip_whitespace();

        let had_newline = self.saw_newline;
        let mut token = self.produce_token();
        token.had_newline_before = had_newline;
        if token.token_type != TokenType::Illegal {
            self.last_token_type = Some(token.token_type);
        }
        token
    }

    fn produce_token(&mut self) -> Token {
        let token_line = self.line;
        let token_column = self.column;

        if self.ch == 0 {
            return Token::new(TokenType::Eof, String::new(), token_line, token_column);
        }

        if self.ch == b'`' {
            return self.read_template_literal(token_line, token_column);
        }

        if self.ch == b'/' && self.should_start_regex_literal() {
            return self.read_regex_literal(token_line, token_column);
        }

        if self.ch == b'"' || self.ch == b'\'' {
            let quote = self.ch;
            let lit = self.read_quoted(quote);
            return Token::new(TokenType::String, lit, token_line, token_column);
        }

        if self.ch == b'.' && self.peek_char().is_ascii_digit() {
            let lit = self.read_number();
            return Token::new(TokenType::Float, lit, token_line, token_column);
        }

        if self.ch.is_ascii_digit() {
            let lit = self.read_number();
            // BigInt suffix: a trailing `n` after a pure-integer literal
            // (no fraction, no exponent). `1n`, `0xFFn`, `0b101n`.
            if self.ch == b'n'
                && !lit.contains('.')
                && !lit.contains('e')
                && !lit.contains('E')
            {
                self.read_char();
                return Token::new(TokenType::BigInt, lit, token_line, token_column);
            }
            let token_type = if lit.contains('.') || lit.contains('e') || lit.contains('E') {
                TokenType::Float
            } else {
                TokenType::Int
            };
            return Token::new(token_type, lit, token_line, token_column);
        }

        if is_letter(self.ch) {
            let lit = self.read_identifier();
            let t = lookup_ident(&lit);
            return Token::new(t, lit, token_line, token_column);
        }

        // Direct character match — replaces O(N) sorted_ops linear scan
        if let Some((tt, lit)) = self.match_operator() {
            return Token::new(tt, lit, token_line, token_column);
        }

        if self.ch == b'#' {
            self.read_char();
            return Token::new(TokenType::Hash, "#".to_string(), token_line, token_column);
        }

        let illegal = (self.ch as char).to_string();
        self.read_char();
        Token::new(TokenType::Illegal, illegal, token_line, token_column)
    }

    fn should_start_regex_literal(&self) -> bool {
        let peek = self.peek_char();
        // /= is DivideAssign, // is comment, /* is block comment
        if peek == b'=' || peek == b'/' || peek == b'*' {
            return false;
        }
        // Null byte means end of input
        if peek == 0 {
            return false;
        }
        // NOTE: Don't reject whitespace after / - regex like / $/ is valid!
        // Decision is based purely on the preceding token type.
        //
        // The two rules below are intentionally both "gates": a `/` becomes a
        // regex only when the previous token is known-to-open-an-expression
        // AND is not a token that typically expects a plain operand next
        // (binary arithmetic operators, postfix `++`/`--`, etc.). This
        // matches the reference parity lexer and is closer to how Acorn's
        // `q.exprAllowed` flag behaves in practice — plain arithmetic like
        // `a * b / c` stays as Slash instead of flipping `/c` into a regex.
        match self.last_token_type {
            None => true,
            Some(t) if Self::arithmetic_op_expects_operand(t) => false,
            Some(t) => !Self::token_can_end_expression(t),
        }
    }

    /// Binary / postfix operators that are always followed by an expression
    /// operand — after these, a `/` is division, not a regex. Treating them
    /// as "regex context" is technically permitted by the spec (`a * /x/`)
    /// but is virtually never the intended parse.
    fn arithmetic_op_expects_operand(token_type: TokenType) -> bool {
        matches!(
            token_type,
            TokenType::Plus
                | TokenType::Minus
                | TokenType::Asterisk
                | TokenType::Slash
                | TokenType::Percent
                | TokenType::Exponent
        )
    }

    fn token_can_end_expression(token_type: TokenType) -> bool {
        matches!(
            token_type,
            TokenType::Ident
                | TokenType::Int
                | TokenType::Float
                | TokenType::String
                | TokenType::Template
                | TokenType::Regex
                | TokenType::True
                | TokenType::False
                | TokenType::Null
                | TokenType::Undefined
                | TokenType::This
                | TokenType::Super
                | TokenType::RightParen
                | TokenType::RightBracket
                | TokenType::RightBrace
                | TokenType::Increment
                | TokenType::Decrement
        )
    }

    fn read_regex_literal(&mut self, line: usize, column: usize) -> Token {
        self.read_char();

        let mut pattern = String::new();
        let mut escaped = false;
        let mut in_char_class = false;

        while self.ch != 0 {
            if !escaped {
                if self.ch == b'[' {
                    in_char_class = true;
                } else if self.ch == b']' {
                    in_char_class = false;
                } else if self.ch == b'/' && !in_char_class {
                    break;
                }
            }

            if escaped {
                escaped = false;
            } else if self.ch == b'\\' {
                escaped = true;
            }

            pattern.push(self.ch as char);
            self.read_char();
        }

        if self.ch != b'/' {
            return Token::new(TokenType::Illegal, "/".to_string(), line, column);
        }

        self.read_char();
        let mut flags = String::new();
        while self.ch.is_ascii_alphabetic() {
            flags.push(self.ch as char);
            self.read_char();
        }

        let mut token = Token::new(TokenType::Regex, pattern, line, column);
        token.raw_literal = Some(flags);
        token
    }



    /// Fast O(1) operator matching via direct character dispatch.
    /// Replaces the O(N) sorted_ops linear scan + String clone.
    fn match_operator(&mut self) -> Option<(TokenType, String)> {
        let p1 = self.peek_char();
        let p2 = if self.read_position + 1 < self.input.len() {
            self.input[self.read_position + 1]
        } else { 0 };
        let p3 = if self.read_position + 2 < self.input.len() {
            self.input[self.read_position + 2]
        } else { 0 };

        let (tt, len) = match self.ch {
            b'+' => match p1 {
                b'+' => (TokenType::Increment, 2),
                b'=' => (TokenType::PlusAssign, 2),
                _ => (TokenType::Plus, 1),
            },
            b'-' => match p1 {
                b'-' => (TokenType::Decrement, 2),
                b'=' => (TokenType::MinusAssign, 2),
                _ => (TokenType::Minus, 1),
            },
            b'*' => match p1 {
                b'*' => if p2 == b'=' { (TokenType::ExponentAssign, 3) } else { (TokenType::Exponent, 2) },
                b'=' => (TokenType::MultiplyAssign, 2),
                _ => (TokenType::Asterisk, 1),
            },
            b'/' => match p1 {
                b'=' => (TokenType::DivideAssign, 2),
                _ => (TokenType::Slash, 1),
            },
            b'%' => match p1 {
                b'=' => (TokenType::PercentAssign, 2),
                _ => (TokenType::Percent, 1),
            },
            b'=' => match p1 {
                b'=' => if p2 == b'=' { (TokenType::StrictEqual, 3) } else { (TokenType::Equal, 2) },
                b'>' => (TokenType::Arrow, 2),
                _ => (TokenType::Assign, 1),
            },
            b'!' => match p1 {
                b'=' => if p2 == b'=' { (TokenType::StrictNotEqual, 3) } else { (TokenType::NotEqual, 2) },
                _ => (TokenType::Bang, 1),
            },
            b'<' => match p1 {
                b'<' => if p2 == b'=' { (TokenType::LeftShiftAssign, 3) } else { (TokenType::LeftShift, 2) },
                b'=' => (TokenType::LessThanOrEqual, 2),
                _ => (TokenType::LessThan, 1),
            },
            b'>' => match p1 {
                b'>' => match p2 {
                    b'>' => if p3 == b'=' { (TokenType::UnsignedRightShiftAssign, 4) } else { (TokenType::UnsignedRightShift, 3) },
                    b'=' => (TokenType::RightShiftAssign, 3),
                    _ => (TokenType::RightShift, 2),
                },
                b'=' => (TokenType::GreaterThanOrEqual, 2),
                _ => (TokenType::GreaterThan, 1),
            },
            b'&' => match p1 {
                b'&' => if p2 == b'=' { (TokenType::AndAssign, 3) } else { (TokenType::And, 2) },
                b'=' => (TokenType::BitwiseAndAssign, 2),
                _ => (TokenType::BitwiseAnd, 1),
            },
            b'|' => match p1 {
                b'|' => if p2 == b'=' { (TokenType::OrAssign, 3) } else { (TokenType::Or, 2) },
                b'=' => (TokenType::BitwiseOrAssign, 2),
                _ => (TokenType::BitwiseOr, 1),
            },
            b'^' => match p1 {
                b'=' => (TokenType::BitwiseXorAssign, 2),
                _ => (TokenType::BitwiseXor, 1),
            },
            b'~' => (TokenType::BitwiseNot, 1),
            b'?' => match p1 {
                b'?' => if p2 == b'=' { (TokenType::NullishAssign, 3) } else { (TokenType::NullishCoalescing, 2) },
                b'.' => (TokenType::OptionalChain, 2),
                _ => (TokenType::Question, 1),
            },
            b'.' => if p1 == b'.' && p2 == b'.' { (TokenType::Spread, 3) } else { (TokenType::Dot, 1) },
            b',' => (TokenType::Comma, 1),
            b';' => (TokenType::Semicolon, 1),
            b':' => (TokenType::Colon, 1),
            b'(' => (TokenType::LeftParen, 1),
            b')' => (TokenType::RightParen, 1),
            b'{' => (TokenType::LeftBrace, 1),
            b'}' => (TokenType::RightBrace, 1),
            b'[' => (TokenType::LeftBracket, 1),
            b']' => (TokenType::RightBracket, 1),
            _ => return None,
        };
        // Build literal from source slice (no intermediate Vec allocation)
        let start = self.position;
        for _ in 0..len {
            self.read_char();
        }
        let lit = unsafe { std::str::from_utf8_unchecked(&self.input[start..start + len]) }.to_owned();
        Some((tt, lit))
    }

    fn read_template_literal(&mut self, line: usize, column: usize) -> Token {
        self.read_char();
        let mut result = String::new();
        let mut raw = String::new();
        let mut interpolation_depth: i32 = 0;
        while self.ch != 0 {
            if self.ch == b'`' && interpolation_depth == 0 {
                break;
            }

            if self.ch == b'$' && self.peek_char() == b'{' {
                result.push('$');
                raw.push('$');
                self.read_char();
                result.push('{');
                raw.push('{');
                interpolation_depth += 1;
                self.read_char();
                continue;
            }

            if self.ch == b'}' && interpolation_depth > 0 {
                interpolation_depth -= 1;
                result.push('}');
                raw.push('}');
                self.read_char();
                continue;
            }

            if self.ch == b'\\' {
                raw.push('\\');
                self.read_char();
                raw.push(self.ch as char);
                match self.ch {
                    b'n' => result.push('\n'),
                    b'r' => result.push('\r'),
                    b't' => result.push('\t'),
                    b'`' => result.push('`'),
                    b'\\' => result.push('\\'),
                    b'$' => result.push('$'),
                    b'0' => result.push('\0'),
                    b'u' => {
                        let mut hex = String::with_capacity(4);
                        for _ in 0..4 {
                            self.read_char();
                            raw.push(self.ch as char);
                            if self.ch.is_ascii_hexdigit() {
                                hex.push(self.ch as char);
                            } else {
                                break;
                            }
                        }
                        if hex.len() == 4 {
                            if let Ok(code) = u32::from_str_radix(&hex, 16) {
                                if let Some(c) = char::from_u32(code) {
                                    result.push(c);
                                }
                            }
                        }
                    }
                    _ => {
                        result.push('\\');
                        result.push(self.ch as char);
                    }
                }
                self.read_char();
                continue;
            }
            result.push(self.ch as char);
            raw.push(self.ch as char);
            self.read_char();
        }
        if self.ch == b'`' {
            self.read_char();
        }
        let mut token = Token::new(TokenType::Template, result, line, column);
        token.raw_literal = Some(raw);
        token
    }

    fn read_quoted(&mut self, quote: u8) -> String {
        self.read_char();
        // Estimate string length from remaining input (avoid repeated String growth)
        let remaining = self.input.len().saturating_sub(self.position);
        let mut out = String::with_capacity(remaining.min(64));
        while self.ch != 0 && self.ch != quote {
            if self.ch == b'\\' {
                self.read_char();
                match self.ch {
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'"' => out.push('"'),
                    b'\'' => out.push('\''),
                    b'\\' => out.push('\\'),
                    b'0' => out.push('\0'),
                    b'u' => {
                        // \uXXXX Unicode escape — read 4 hex digits
                        let mut hex = String::with_capacity(4);
                        for _ in 0..4 {
                            self.read_char();
                            if self.ch.is_ascii_hexdigit() {
                                hex.push(self.ch as char);
                            } else {
                                break;
                            }
                        }
                        if hex.len() == 4 {
                            if let Ok(code) = u32::from_str_radix(&hex, 16) {
                                if let Some(c) = char::from_u32(code) {
                                    out.push(c);
                                }
                            }
                        }
                        // self.ch is on the last hex digit; the outer loop's
                        // read_char() will advance past it.
                    }
                    _ => {
                        out.push('\\');
                        out.push(self.ch as char);
                    }
                }
            } else if self.ch >= 0x80 {
                // Multi-byte UTF-8: collect all continuation bytes
                let mut bytes = vec![self.ch];
                let expected = if self.ch >= 0xF0 {
                    4
                } else if self.ch >= 0xE0 {
                    3
                } else {
                    2
                };
                for _ in 1..expected {
                    self.read_char();
                    if self.ch & 0xC0 == 0x80 {
                        bytes.push(self.ch);
                    } else {
                        break;
                    }
                }
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    out.push_str(s);
                }
            } else {
                out.push(self.ch as char);
            }
            self.read_char();
        }
        if self.ch == quote {
            self.read_char();
        }
        out
    }

    #[inline(always)]
    fn skip_whitespace(&mut self) {
        while self.ch == b' ' || self.ch == b'\t' || self.ch == b'\n' || self.ch == b'\r' {
            if self.ch == b'\n' || self.ch == b'\r' {
                self.saw_newline = true;
            }
            self.read_char();
        }
    }

    fn skip_comments(&mut self) {
        loop {
            if self.ch == b'/' && self.peek_char() == b'/' {
                self.read_char();
                self.read_char();
                while self.ch != 0 && self.ch != b'\n' {
                    self.read_char();
                }
                continue;
            }
            if self.ch == b'/' && self.peek_char() == b'*' {
                self.read_char();
                self.read_char();
                while self.ch != 0 {
                    if self.ch == b'*' && self.peek_char() == b'/' {
                        self.read_char();
                        self.read_char();
                        break;
                    }
                    self.read_char();
                }
                continue;
            }
            break;
        }
    }

    fn read_identifier(&mut self) -> String {
        let start = self.position;
        while is_letter(self.ch) || self.ch.is_ascii_digit() {
            self.read_char();
        }
        // Safety: identifiers are ASCII — skip intermediate Vec via str slice
        unsafe { std::str::from_utf8_unchecked(&self.input[start..self.position]) }.to_owned()
    }

    fn read_number(&mut self) -> String {
        if self.ch == b'0' {
            let next = self.peek_char();
            let radix = match next {
                b'x' | b'X' => Some(16),
                b'b' | b'B' => Some(2),
                b'o' | b'O' => Some(8),
                _ => None,
            };
            if let Some(radix) = radix {
                let mut out = String::from("0");
                self.read_char();
                out.push(self.ch as char);
                self.read_char();
                while Self::is_radix_digit(self.ch, radix)
                    || (self.ch == b'_' && Self::is_radix_digit(self.peek_char(), radix))
                {
                    if self.ch != b'_' {
                        out.push(self.ch as char);
                    }
                    self.read_char();
                }
                return out;
            }
        }

        // Fast path for simple decimal numbers (no underscores): slice-based, no push
        let start = self.position;
        let mut has_underscore = false;
        let mut seen_dot = self.ch == b'.';
        let mut seen_exp = false;
        if self.ch == b'.' { self.read_char(); }

        while self.ch.is_ascii_digit()
            || (!seen_dot && !seen_exp && self.ch == b'.')
            || (self.ch == b'_' && self.peek_char().is_ascii_digit())
            || (!seen_exp && (self.ch == b'e' || self.ch == b'E') && self.exponent_has_digits())
        {
            if self.ch == b'_' { has_underscore = true; }
            if self.ch == b'.' { seen_dot = true; }
            if self.ch == b'e' || self.ch == b'E' {
                seen_exp = true;
                self.read_char();
                if self.ch == b'+' || self.ch == b'-' { self.read_char(); }
                continue;
            }
            self.read_char();
        }

        if !has_underscore {
            // Fast path: str slice → owned String (skip intermediate Vec)
            unsafe { std::str::from_utf8_unchecked(&self.input[start..self.position]) }.to_owned()
        } else {
            // Slow path: filter underscores
            let slice = &self.input[start..self.position];
            let mut out = String::with_capacity(slice.len());
            for &b in slice {
                if b != b'_' { out.push(b as char); }
            }
            out
        }
    }

    fn is_radix_digit(ch: u8, radix: u32) -> bool {
        (ch as char).is_digit(radix)
    }

    fn exponent_has_digits(&self) -> bool {
        let mut i = self.read_position;
        if i >= self.input.len() {
            return false;
        }

        if self.input[i] == b'+' || self.input[i] == b'-' {
            i += 1;
        }

        i < self.input.len() && self.input[i].is_ascii_digit()
    }
}

fn is_letter(ch: u8) -> bool {
    ch == b'_' || ch == b'$' || ch.is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    use crate::config::ZippConfig;
    use crate::lexer::Lexer;
    use crate::token::TokenType;

    fn next_type(input: &str) -> TokenType {
        let mut lexer = Lexer::new(input, ZippConfig::default());
        lexer.next_token().token_type
    }

    #[test]
    fn tokenizes_basic_values() {
        let mut lexer = Lexer::new("42 3.14 \"hello\" 'x' `tmpl`", ZippConfig::default());
        assert_eq!(lexer.next_token().token_type, TokenType::Int);
        assert_eq!(lexer.next_token().token_type, TokenType::Float);
        assert_eq!(lexer.next_token().token_type, TokenType::String);
        assert_eq!(lexer.next_token().token_type, TokenType::String);
        assert_eq!(lexer.next_token().token_type, TokenType::Template);
    }

    #[test]
    fn tokenizes_keywords_and_ops() {
        assert_eq!(next_type("let"), TokenType::Let);
        assert_eq!(next_type("class"), TokenType::Class);
        assert_eq!(next_type("!=="), TokenType::StrictNotEqual);
        assert_eq!(next_type("..."), TokenType::Spread);
    }

    #[test]
    fn skips_comments_and_whitespace() {
        let mut lexer = Lexer::new("// a\n/* b */\n42", ZippConfig::default());
        let tok = lexer.next_token();
        assert_eq!(tok.token_type, TokenType::Int);
        assert_eq!(tok.literal, "42");
    }

    #[test]
    fn tokenizes_numeric_separators() {
        let mut lexer = Lexer::new("1_000_000 1_000.50", ZippConfig::default());
        let t1 = lexer.next_token();
        assert_eq!(t1.token_type, TokenType::Int);
        assert_eq!(t1.literal, "1000000");

        let t2 = lexer.next_token();
        assert_eq!(t2.token_type, TokenType::Float);
        assert_eq!(t2.literal, "1000.50");
    }

    #[test]
    fn tokenizes_scientific_notation() {
        let mut lexer = Lexer::new("1e3 2E-2 .5e+1 1_2e3", ZippConfig::default());

        let t1 = lexer.next_token();
        assert_eq!(t1.token_type, TokenType::Float);
        assert_eq!(t1.literal, "1e3");

        let t2 = lexer.next_token();
        assert_eq!(t2.token_type, TokenType::Float);
        assert_eq!(t2.literal, "2E-2");

        let t3 = lexer.next_token();
        assert_eq!(t3.token_type, TokenType::Float);
        assert_eq!(t3.literal, ".5e+1");

        let t4 = lexer.next_token();
        assert_eq!(t4.token_type, TokenType::Float);
        assert_eq!(t4.literal, "12e3");
    }

    #[test]
    fn tokenizes_radix_integer_literals() {
        let mut lexer = Lexer::new("0xff 0b101 0o10 0xFF_FF", ZippConfig::default());

        let t1 = lexer.next_token();
        assert_eq!(t1.token_type, TokenType::Int);
        assert_eq!(t1.literal, "0xff");

        let t2 = lexer.next_token();
        assert_eq!(t2.token_type, TokenType::Int);
        assert_eq!(t2.literal, "0b101");

        let t3 = lexer.next_token();
        assert_eq!(t3.token_type, TokenType::Int);
        assert_eq!(t3.literal, "0o10");

        let t4 = lexer.next_token();
        assert_eq!(t4.token_type, TokenType::Int);
        assert_eq!(t4.literal, "0xFFFF");
    }
}
