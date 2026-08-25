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
//!   Rust's `String` cannot hold. [`token::StrVal`] holds UTF-16 units when it
//!   has to, and the lexer takes the WTF-8 original of an `eval` code string
//!   alongside the lossy `&str` view of it (`lexer::Lexer::set_exact_src`), so
//!   the compiler no longer carries a parallel source buffer of its own.
//!
//! This IS the engine's front end: every path from source text to bytecode
//! parses here (see `crate::front` for the four entry points). It was built
//! incrementally behind the oxc path and swapped in once a differential gate
//! showed both front ends producing identical ASTs across a multi-thousand-file
//! corpus; the bridge and oxc were then deleted.

pub mod ast;
pub mod cover;
pub mod expr;
pub mod funcs;
pub mod lexer;
pub(crate) mod limits;
pub mod parser;
pub mod stmt;
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

    /// `UnicodeIDStart`/`UnicodeIDContinue` are the real `ID_Start`/`ID_Continue`
    /// properties, NOT Rust's `is_alphabetic`/`is_alphanumeric`. Those disagree in
    /// BOTH directions, and each direction was a bug: the approximation rejected
    /// `Other_ID_Start` and the `Pc`/`Mn`/`Other_ID_Continue` classes, and accepted
    /// `Other_Alphabetic` marks, `No` digits and `Pattern_Syntax`.
    ///
    /// Worth 2131 test262 executions, because private names lex through the very
    /// same predicate — "expected a private name after '#'" was this bug wearing a
    /// different error message.
    #[test]
    fn identifier_chars_follow_id_start_and_id_continue() {
        // ACCEPTED: Other_ID_Start, and the continue-only classes.
        for c in ['\u{2118}', '\u{212E}', '\u{309B}', '\u{309C}'] {
            assert!(
                Lexer::id_start_for_test(c),
                "Other_ID_Start must start: {c:?}"
            );
        }
        for c in [
            '\u{0300}', '\u{0903}', '\u{203F}', '\u{2040}', '\u{FF3F}', '\u{0387}',
        ] {
            assert!(Lexer::id_part_for_test(c), "must continue: {c:?}");
        }
        // ZWNJ/ZWJ are added by the spec on top of ID_Continue.
        assert!(Lexer::id_part_for_test('\u{200C}'));
        assert!(Lexer::id_part_for_test('\u{200D}'));
        // U+00B7 was hand-special-cased before; the real table still admits it.
        assert!(Lexer::id_part_for_test('\u{00B7}'));

        // REJECTED: Alphabetic/alphanumeric but NOT ID_Start / ID_Continue.
        for c in ['\u{05B0}', '\u{0345}', '\u{0903}', '\u{2E2F}'] {
            assert!(!Lexer::id_start_for_test(c), "must not start: {c:?}");
        }
        assert!(
            !Lexer::id_part_for_test('\u{00B2}'),
            "No is not ID_Continue"
        );
        assert!(
            !Lexer::id_part_for_test('\u{2E2F}'),
            "Pattern_Syntax is excluded"
        );

        // And they really do lex, escaped and raw.
        assert_eq!(
            names(r"var \u2118 = 1;"),
            vec!["var".to_string(), "\u{2118}".to_string()]
        );
        assert_eq!(
            names("var a\u{0300} = 1;"),
            vec!["var".to_string(), "a\u{0300}".to_string()]
        );
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
            TokenKind::Ident {
                kw: Keyword::Await,
                had_escape: false,
                ..
            }
        ));

        let escaped = lex_all(r"\u0061wait");
        match &escaped[0] {
            TokenKind::Ident {
                name,
                kw,
                had_escape,
                ..
            } => {
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
            TokenKind::Ident {
                kw: Keyword::None,
                ..
            }
        ));
    }

    #[test]
    fn lone_surrogates_survive() {
        // `String` cannot hold this, which is what `StrVal::Utf16` is for.
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
        assert!(matches!(
            div.next_token(false).unwrap().kind,
            TokenKind::Punct(Punct::Slash)
        ));

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
            TokenKind::Template {
                cooked,
                raw,
                head,
                tail,
            } => {
                assert_eq!(cooked.as_ref().unwrap().to_lossy_string(), "abc");
                assert_eq!(raw, "abc");
                assert!(*head && *tail);
            }
            other => panic!("got {other:?}"),
        }
        // With a substitution the lexer stops at `${` and hands back control.
        let mut lx = Lexer::new("`a${x}b`");
        match lx.next_token(false).unwrap().kind {
            TokenKind::Template {
                head, tail, cooked, ..
            } => {
                assert!(head && !tail);
                assert_eq!(cooked.unwrap().to_lossy_string(), "a");
            }
            other => panic!("got {other:?}"),
        }
        assert_eq!(lx.next_token(false).unwrap().ident_name(), Some("x"));
        // The parser consumes the `}` by resuming the template AT it.
        let t = lx.read_template_continue().unwrap();
        match t.kind {
            TokenKind::Template {
                head, tail, cooked, ..
            } => {
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
            assert!(
                Lexer::new(bad).next_token(false).is_err(),
                "{bad} should fail"
            );
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
            assert!(
                Lexer::new(bad).next_token(false).is_err(),
                "{bad} should fail"
            );
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
                TokenKind::Punct(UShrEq),
                TokenKind::Punct(UShr),
                TokenKind::Punct(Shr),
                TokenKind::Punct(GtEq),
                TokenKind::Punct(StarStar),
                TokenKind::Punct(StarStarEq),
                TokenKind::Punct(QuestionQuestionEq),
                TokenKind::Punct(QuestionQuestion),
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
            assert!(
                !k.is_always_reserved() && !k.is_strict_reserved(),
                "{w} is contextual"
            );
        }
        assert_eq!(Keyword::classify("notakeyword"), Keyword::None);
    }
}

#[cfg(test)]
mod parser_tests {
    use super::parser::ParseOptions;
    use super::stmt::parse;

    fn ok(src: &str) -> String {
        match parse(src, ParseOptions::script()) {
            Ok(p) => format!("{p:?}"),
            Err(e) => panic!("failed to parse {src:?}: {} at {}", e.msg, e.pos),
        }
    }

    fn err(src: &str) -> String {
        match parse(src, ParseOptions::script()) {
            Ok(_) => panic!("expected {src:?} to be rejected"),
            Err(e) => e.msg,
        }
    }

    #[test]
    fn parses_the_ordinary_shapes() {
        for src in [
            "1 + 2 * 3;",
            "let x = 1, y = 2;",
            "const {a, b: [c] = []} = o;",
            "function f(a, b = 1, ...r) { return a; }",
            "class C extends B { #x = 1; static s = 2; get v() { return 1 } static { } }",
            "for (const k in o) {}",
            "for (const v of xs) {}",
            "try { } catch { } finally { }",
            "switch (x) { case 1: break; default: }",
            "a?.b?.[c]?.();",
            "label: for (;;) { break label; }",
            "x = a ? b : c;",
            "`a${1}b`;",
            "var re = /ab+c/gi;",
            "(a, b) => a + b;",
            "async (a) => await a;",
            "new Foo(1, 2).bar();",
            "o = { m() {}, get g() { return 1 }, [k]: v, ...rest };",
            "x **= 2 ** 3 ** 4;",
            "a ??= b;",
        ] {
            let _ = ok(src);
        }
    }

    #[test]
    fn cover_grammar_resolves_both_ways() {
        // `(a, b)` as a sequence expression, and as arrow parameters.
        assert!(ok("(a, b);").contains("Seq"));
        assert!(ok("(a, b) => a;").contains("Arrow"));
        // `{a = 1}` is a SyntaxError as a literal...
        assert!(err("({a = 1});").contains("shorthand"));
        // ...and legal as a destructuring target.
        assert!(ok("({a = 1} = {});").contains("Assign"));
        // `...rest` is only valid as a parameter list.
        assert!(err("(...a);").contains("rest"));
        assert!(ok("(...a) => a;").contains("Arrow"));
        // `async(1)` is a CALL, not an arrow.
        assert!(ok("async(1);").contains("Call"));
        assert!(ok("async (a) => a;").contains("Arrow"));
    }

    #[test]
    fn annex_b_call_assignment_target_parses() {
        // The case oxc's AST could not represent, which forced a source rewrite
        // and reparse. It must PARSE in sloppy code and throw at runtime.
        assert!(ok("f() = 1;").contains("Call"));
        // ...and be an early error in strict code.
        assert!(parse("'use strict'; f() = 1;", ParseOptions::script()).is_err());
    }

    #[test]
    fn early_errors_the_old_front_end_could_not_raise() {
        assert!(err("let x; let x;").contains("already been declared"));
        assert!(err("let y; var y;").contains("already been declared"));
        assert!(err("let a; { var a; }").contains("already been declared"));
        assert!(err("class C { constructor(){} constructor(){} }").contains("one constructor"));
        assert!(err("a: b: a: ;").contains("already been declared"));
        assert!(err("switch (x) { default: ; default: ; }").contains("default"));
        assert!(err("'use strict'; with (o) {}").contains("strict"));
        assert!(err("'use strict'; delete x;").contains("delete"));
        assert!(err("'use strict'; var eval;").contains("eval"));
        assert!(err("const c;").contains("missing initializer"));
        assert!(err("break;").contains("break"));
        assert!(err("continue;").contains("continue"));
        assert!(err("return 1;").contains("return"));
        assert!(err("a ?? b || c;").contains("mixed"));
        assert!(err("-a ** b;").contains("parentheses"));
    }

    #[test]
    fn statement_position_declarations() {
        // A VariableStatement IS a Statement: `if (c) var x = 1;` is legal and
        // transpiled code produces it constantly (`if (e) var r = new WeakMap()`
        // is verbatim Babel output). Lexical declarations stay restricted.
        let _ = ok("if (c) var x = 1;");
        let _ = ok("if (c) var x = 1; else var y = 2;");
        let _ = ok("while (c) var x = 1;");
        assert!(parse("if (c) let x = 1;", ParseOptions::script()).is_err());
        assert!(parse("if (c) const x = 1;", ParseOptions::script()).is_err());

        // Annex B B.3.4: a function declaration as an if-body is sloppy-legal.
        let _ = ok("if (c) function f() {}");
        let _ = ok("l: function f() {}");
        assert!(
            parse(
                "'use strict'; if (c) function f() {}",
                ParseOptions::script()
            )
            .is_err(),
            "strict keeps the error"
        );
        // A class is never legal there, in either mode.
        assert!(parse("if (c) class K {}", ParseOptions::script()).is_err());
    }

    #[test]
    fn asi_and_restricted_productions() {
        // A newline after `return` ends the statement.
        assert!(ok("function f() { return
1; }")
        .contains("Return"));
        // A newline before `++` means ASI, not a postfix operator.
        let _ = ok("a
++b;");
        // `async` followed by a newline is NOT an async arrow — the production
        // has a [no LineTerminator here] restriction — so this reads as a call
        // to `async` followed by `=>`, which is a SyntaxError. node agrees.
        assert!(parse(
            "async
() => {};",
            ParseOptions::script()
        )
        .is_err());
    }
}
