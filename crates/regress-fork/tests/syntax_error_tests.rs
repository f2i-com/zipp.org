#![allow(clippy::uninlined_format_args)]

#[track_caller]
fn test_1_error(pattern: &str, expected_err: &str) {
    let res = regress::Regex::with_flags(pattern, "u");
    assert!(res.is_err(), "Pattern should not have parsed: {}", pattern);

    let err = res.err().unwrap().text;
    assert!(
        err.contains(expected_err),
        "Error text '{}' did not contain '{}' for pattern '{}'",
        err,
        expected_err,
        pattern
    );
}

#[test]
fn test_excessive_capture_groups() {
    let mut captures = String::from("s");
    let mut loops = String::from("s");
    for _ in 0..65536 {
        captures.push_str("(x)");
        loops.push_str("x{3,5}");
    }
    test_1_error(captures.as_str(), "Capture group count limit exceeded");
    test_1_error(loops.as_str(), "Loop count limit exceeded");
}

#[test]
fn test_syntax_errors() {
    test_1_error(r"*", "Invalid atom character");
    test_1_error(r"x**", "Invalid atom character");
    test_1_error(r"?", "Invalid atom character");
    test_1_error(r"{3,5}", "Invalid atom character");
    test_1_error(r"x{5,3}", "Invalid quantifier");

    test_1_error(r"]", "Invalid atom character");
    test_1_error(r"[abc", "Unbalanced bracket");

    test_1_error(r"(", "Unbalanced parenthesis");
    test_1_error(r"(?!", "Unbalanced parenthesis");
    test_1_error(r"abc)", "Unbalanced parenthesis");

    test_1_error(
        r"[z-a]",
        "Range values reversed, start char code is greater than end char code.",
    );

    // In unicode mode this is not allowed.
    test_1_error(r"[a-\s]", "Invalid character range");

    test_1_error("\\", "Incomplete escape");

    test_1_error("^*", "Quantifier not allowed here");
    test_1_error("${3}", "Quantifier not allowed here");
    test_1_error("(?=abc)*", "Quantifier not allowed here");
    test_1_error("(?!abc){3,}", "Quantifier not allowed here");

    test_1_error(r"\2(a)", "Invalid character escape");
    test_1_error("(?q:abc)", "Invalid group modifier");
}

#[cfg(feature = "prohibit-unsafe")]
#[test]
fn sandbox_rejects_repeated_unicode_property_expansion() {
    let repeated_character_property = r"\p{L}".repeat(512);
    let err = regress::Regex::with_flags(&repeated_character_property, "u")
        .expect_err("aggregate character-property expansion must be bounded");
    assert!(err.text.contains("property expansion exceeds"), "{err}");

    // One RGI_Emoji atom is 3,953 alternatives in the generated Unicode
    // table. The second must fail before its alternatives are copied into IR.
    let err = regress::Regex::with_flags(r"\p{RGI_Emoji}\p{RGI_Emoji}", "v")
        .expect_err("repeated string-property expansion must be bounded");
    assert!(err.text.contains("property expansion exceeds"), "{err}");
}

#[cfg(feature = "prohibit-unsafe")]
#[test]
fn resident_bytes_covers_unicode_expansion() {
    let simple = regress::Regex::new("abc").unwrap().resident_bytes();
    let character_property = regress::Regex::with_flags(r"\p{L}", "u")
        .unwrap()
        .resident_bytes();
    let string_property = regress::Regex::with_flags(r"\p{RGI_Emoji}", "v")
        .unwrap()
        .resident_bytes();

    eprintln!(
        "compiled resident bytes: simple={simple}, property={character_property}, RGI_Emoji={string_property}"
    );
    assert!(simple > 0);
    assert!(character_property > simple);
    assert!(string_property > character_property);
}
