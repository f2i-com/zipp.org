//! A hand-written front end for zipp: lexer, AST and parser.
//!
//! Replaces `oxc_parser`. The motivation is NOT speed — parsing is ~12% of the
//! cost of getting from source to bytecode, and oxc is fast and linear. It is
//! conformance and control:
//!
//! - **Early errors.** 2,214 of the engine's 2,907 test262 failures are
//!   static-semantics errors it cannot raise, because it pulls `oxc_parser`
//!   without `oxc_semantic`. Detecting them needs binding, strictness and
//!   context state that only the parser has as it goes.
//! - **An AST shaped for an engine.** oxc's `AssignmentTarget` cannot represent
//!   a `CallExpression`, so sloppy-mode `f() = 1` — which must parse and throw a
//!   *runtime* ReferenceError — currently forces a source-text rewrite and a
//!   full reparse (`crate::annexb_call_target_rewrite`), whose own comment
//!   admits it mishandles a strict function inside a sloppy script.
//! - **Exact source.** A string or regex may contain lone surrogates, which
//!   Rust's `String` cannot hold; the engine carries a parallel WTF-8 buffer
//!   (`compile/mod.rs` `exact_src`) to work around it. [`token::StrVal`] holds
//!   UTF-16 units when it has to, so that buffer can go away.
//!
//! Built incrementally behind the existing oxc path, and swapped in only when a
//! differential gate shows both front ends compile the corpus to identical
//! bytecode. Dead until then.

#![allow(dead_code)]

pub mod ast;
pub mod lexer;
pub mod token;

#[cfg(test)]
mod tests {
    use super::lexer::Lexer;
    use super::token::*;

    /// Lex to completion with `/` always treated as division, which is what the
    /// parser asks for in operand position.
    fn lex_all(src: &str) -> Vec<TokenKind> {
        let mut lx = Lexer::new(src);
        let mut out = Vec::new();
        loop {
            let t = lx.next_token(false).expect("lex error");
            if matches!(t.kind, TokenKind::Eof) {
                return out;
            }
            out.push(t.kind);
        }
    }

    fn names(src: &str) -> Vec<String> {
        lex_all(src)
            .into_iter()
            .filter_map(|k| match k {
                TokenKind::Ident { name, .. } => Some(name),
                _ => None,
            })
            .collect()
    }

    fn nums(src: &str) -> Vec<f64> {
        lex_all(src)
            .into_iter()
            .filter_map(|k| match k {
                TokenKind::Num(n) => Some(n.value),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn escaped_keyword_is_not_the_keyword() {
        // The whole point of tracking `had_escape`: these spell the identifier
        // `await`, not the keyword, and using one where a keyword is required
        // is a SyntaxError. Losing the bit silently accepts programs the spec
        // rejects — a family of test262 cases.
        let plain = lex_all("await");
        assert!(matches!(
            &plain[0],
            TokenKind::Ident { kw: Keyword::Await, had_escape: false, .. }
        ));

        let escaped = lex_all(r"\u0061wait");
        match &escaped[0] {
            TokenKind::Ident { name, kw, had_escape, .. } => {
                assert_eq!(name, "await", "the escape resolves into the name");
                assert_eq!(*kw, Keyword::None, "but it is NOT the keyword");
                assert!(had_escape);
            }
            other => panic!("got {other:?}"),
        }
        // Same for the braced form, mid-identifier.
        assert_eq!(names(r"cl\u{61}ss"), vec!["class"]);
        assert!(matches!(
            &lex_all(r"cl\u{61}ss")[0],
            TokenKind::Ident { kw: Keyword::None, .. }
        ));
    }

    #[test]
    fn lone_surrogates_survive() {
        // `String` cannot hold this; the engine's `exact_src` buffer exists
        // because of it.
        match &lex_all(r#" "\uD800" "#)[0] {
            TokenKind::Str(StrVal::Utf16(units)) => assert_eq!(units, &[0xD800]),
            other => panic!("expected raw UTF-16, got {other:?}"),
        }
        // A well-formed surrogate PAIR is ordinary text and stays cheap.
        match &lex_all(r#" "😀" "#)[0] {
            TokenKind::Str(StrVal::Utf8(s)) => assert_eq!(s, "😀"),
            other => panic!("expected UTF-8, got {other:?}"),
        }
        match &lex_all(r#" "\u{1F600}" "#)[0] {
            TokenKind::Str(StrVal::Utf8(s)) => assert_eq!(s, "😀"),
            other => panic!("expected UTF-8, got {other:?}"),
        }
    }

    #[test]
    fn string_escapes() {
        let k = lex_all(r#" "a\n\t\x41B\0z" "#);
        match &k[0] {
            TokenKind::Str(v) => assert_eq!(v.to_lossy_string(), "a\n\tAB\0z"),
            other => panic!("got {other:?}"),
        }
        // Line continuation produces nothing.
        match &lex_all("\"a\\\nb\"")[0] {
            TokenKind::Str(v) => assert_eq!(v.to_lossy_string(), "ab"),
            other => panic!("got {other:?}"),
        }
        // A raw newline terminates the literal — that is an error, not content.
        assert!(Lexer::new("\"a\nb\"").next_token(false).is_err());
    }

    #[test]
    fn slash_is_division_or_regex_only_the_parser_knows() {
        // Same bytes, both readings, decided by the caller.
        let mut div = Lexer::new("/re/g");
        assert!(matches!(div.next_token(false).unwrap().kind, TokenKind::Punct(Punct::Slash)));

        let mut rx = Lexer::new("/re/g");
        match rx.next_token(true).unwrap().kind {
            TokenKind::Regex { pattern, flags } => {
                assert_eq!(pattern.to_lossy_string(), "re");
                assert_eq!(flags, "g");
            }
            other => panic!("got {other:?}"),
        }
        // A `/` inside a character class does not close the literal.
        let mut cls = Lexer::new("/[/]/");
        match cls.next_token(true).unwrap().kind {
            TokenKind::Regex { pattern, .. } => assert_eq!(pattern.to_lossy_string(), "[/]"),
            other => panic!("got {other:?}"),
        }
        // Escapes are preserved verbatim: the regex engine compiles the source.
        let mut esc = Lexer::new(r"/a\/b\n/u");
        match esc.next_token(true).unwrap().kind {
            TokenKind::Regex { pattern, flags } => {
                assert_eq!(pattern.to_lossy_string(), r"a\/b\n");
                assert_eq!(flags, "u");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn templates_report_their_pieces() {
        // No substitution: one chunk that is both head and tail.
        match &lex_all("`abc`")[0] {
            TokenKind::Template { cooked, raw, head, tail } => {
                assert_eq!(cooked.as_ref().unwrap().to_lossy_string(), "abc");
                assert_eq!(raw, "abc");
                assert!(*head && *tail);
            }
            other => panic!("got {other:?}"),
        }
        // With a substitution the lexer stops at `${` and hands back control.
        let mut lx = Lexer::new("`a${x}b`");
        match lx.next_token(false).unwrap().kind {
            TokenKind::Template { head, tail, cooked, .. } => {
                assert!(head && !tail);
                assert_eq!(cooked.unwrap().to_lossy_string(), "a");
            }
            other => panic!("got {other:?}"),
        }
        assert_eq!(lx.next_token(false).unwrap().ident_name(), Some("x"));
        // The parser consumes the `}` by resuming the template AT it.
        let t = lx.read_template_continue().unwrap();
        match t.kind {
            TokenKind::Template { head, tail, cooked, .. } => {
                assert!(!head && tail);
                assert_eq!(cooked.unwrap().to_lossy_string(), "b");
            }
            other => panic!("got {other:?}"),
        }

        // An invalid escape is not fatal here: in a TAGGED template the cooked
        // value is `undefined` and only `raw` is required, so the lexer reports
        // it and lets the parser decide.
        match &lex_all(r"`\u{}`")[0] {
            TokenKind::Template { cooked, raw, .. } => {
                assert!(cooked.is_none(), "invalid escape => no cooked value");
                assert_eq!(raw, r"\u{}");
            }
            other => panic!("got {other:?}"),
        }
        // CRLF normalises to LF in both cooked and raw.
        match &lex_all("`a\r\nb`")[0] {
            TokenKind::Template { cooked, raw, .. } => {
                assert_eq!(cooked.as_ref().unwrap().to_lossy_string(), "a\nb");
                assert_eq!(raw, "a\nb");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn numeric_forms() {
        assert_eq!(nums("1 1.5 .5 1e3 1E-2"), vec![1.0, 1.5, 0.5, 1000.0, 0.01]);
        assert_eq!(nums("0x1f 0o17 0b1011"), vec![31.0, 15.0, 11.0]);
        // Separators.
        assert_eq!(nums("1_000_000 0x1_f"), vec![1000000.0, 31.0]);
        // ...but not leading, trailing, or doubled.
        for bad in ["1__0", "1_", "0x_1"] {
            assert!(Lexer::new(bad).next_token(false).is_err(), "{bad} should fail");
        }
        // The spelling is retained because strict mode rejects the legacy forms
        // and only the parser knows the strictness.
        match lex_all("0123")[0] {
            TokenKind::Num(NumLit { value, kind }) => {
                assert_eq!(value, 83.0);
                assert_eq!(kind, NumKind::LegacyOctal);
            }
            _ => panic!(),
        }
        match lex_all("08")[0] {
            TokenKind::Num(NumLit { value, kind }) => {
                assert_eq!(value, 8.0);
                assert_eq!(kind, NumKind::NonOctalDecimal);
            }
            _ => panic!(),
        }
        // A digit run that is not all-octal, with a fraction, is plain decimal.
        assert_eq!(nums("08.5"), vec![8.5]);
        // BigInt.
        assert!(matches!(&lex_all("123n")[0], TokenKind::BigInt(s) if s == "123"));
        assert!(matches!(&lex_all("0x10n")[0], TokenKind::BigInt(s) if s == "16"));
        // An identifier may not butt against a number.
        for bad in ["3in", "0x1g", "1e"] {
            assert!(Lexer::new(bad).next_token(false).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn newline_before_drives_asi() {
        let mut lx = Lexer::new("a\nb c");
        let a = lx.next_token(false).unwrap();
        let b = lx.next_token(false).unwrap();
        let c = lx.next_token(false).unwrap();
        assert!(!a.newline_before);
        assert!(b.newline_before, "a newline separates a from b");
        assert!(!c.newline_before, "only a space separates b from c");

        // A block comment containing a newline counts: `a = b /*\n*/ ++c`.
        let mut lx = Lexer::new("b /*\n*/ ++c");
        lx.next_token(false).unwrap();
        assert!(lx.next_token(false).unwrap().newline_before);
    }

    #[test]
    fn punctuator_maximal_munch_and_the_optional_chain_trap() {
        use Punct::*;
        assert_eq!(
            lex_all(">>>= >>> >> >= ** **= ??= ?? ..."),
            vec![
                TokenKind::Punct(UShrEq), TokenKind::Punct(UShr), TokenKind::Punct(Shr),
                TokenKind::Punct(GtEq), TokenKind::Punct(StarStar), TokenKind::Punct(StarStarEq),
                TokenKind::Punct(QuestionQuestionEq), TokenKind::Punct(QuestionQuestion),
                TokenKind::Punct(DotDotDot),
            ]
        );
        // `a?.b` is optional chaining...
        assert!(matches!(lex_all("a?.b")[1], TokenKind::Punct(QuestionDot)));
        // ...but `a?.5:b` is a conditional whose consequent is `.5`, so the
        // `?.` must NOT be taken when a digit follows.
        let k = lex_all("a?.5:b");
        assert!(matches!(k[1], TokenKind::Punct(Question)));
        assert!(matches!(k[2], TokenKind::Num(NumLit { value, .. }) if value == 0.5));
    }

    #[test]
    fn comments_including_the_annex_b_html_forms() {
        assert_eq!(names("a // comment\nb"), vec!["a", "b"]);
        assert_eq!(names("a /* c\n*/ b"), vec!["a", "b"]);
        assert_eq!(names("a <!-- comment\nb"), vec!["a", "b"]);
        // `-->` only opens a comment at the start of a line.
        assert_eq!(names("a\n--> comment\nb"), vec!["a", "b"]);
        assert!(Lexer::new("/* unterminated").next_token(false).is_err());
    }

    #[test]
    fn private_names_and_identifiers() {
        match &lex_all("#count")[0] {
            TokenKind::Ident { name, private, .. } => {
                assert_eq!(name, "count");
                assert!(*private);
            }
            other => panic!("got {other:?}"),
        }
        assert_eq!(names("$ _ a1 é ñame"), vec!["$", "_", "a1", "é", "ñame"]);
    }

    #[test]
    fn keyword_classification_matches_reservation_rules() {
        assert!(Keyword::classify("class").is_always_reserved());
        assert!(!Keyword::classify("let").is_always_reserved());
        assert!(Keyword::classify("let").is_strict_reserved());
        // Contextual keywords are reserved in neither sense.
        for w in ["of", "async", "get", "set", "from", "as"] {
            let k = Keyword::classify(w);
            assert_ne!(k, Keyword::None, "{w} should classify");
            assert!(!k.is_always_reserved() && !k.is_strict_reserved(), "{w} is contextual");
        }
        assert_eq!(Keyword::classify("notakeyword"), Keyword::None);
    }
}
