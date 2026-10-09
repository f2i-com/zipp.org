//! The lexer decodes one character at a time from non-ASCII source. Under
//! `safe-sandbox` that decode is a checked one, and it used to check the whole
//! remainder of the source for every character.

use std::time::{Duration, Instant};

fn parses(source: &str) -> String {
    zipp_vm::parse_to_text(source, false)
        .unwrap_or_else(|error| panic!("valid script rejected: {source}: {error}"))
}

/// Characters of every UTF-8 width, each directly before one of every other
/// width and at the very end of the source: the four-byte window the decode
/// checks ends inside the next character, or at the end of the text.
#[test]
fn characters_of_every_width_decode_beside_each_other() {
    let widths = ["a", "\u{e9}", "\u{4e2d}", "\u{1F600}"];
    for first in widths {
        for second in widths {
            let text = format!("{first}{second}");
            for source in [
                format!("x = '{text}'"),
                format!("x = `{text}`"),
                format!("x = /{text}/u"),
                format!("x = 1 // {text}"),
                format!("x = 1 /* {text} */"),
            ] {
                let tree = parses(&source);
                if !source.contains("//") && !source.contains("/*") {
                    assert!(tree.contains(&text), "{source}: {tree}");
                }
            }
        }
    }
    // Identifiers: a letter of each width that is one.
    let tree = parses("var a\u{e9}\u{4e2d}\u{20000} = 1; a\u{e9}\u{4e2d}\u{20000}");
    assert!(tree.contains("a\u{e9}\u{4e2d}\u{20000}"), "{tree}");
    // A line separator and a no-break space are trivia, three and two bytes.
    parses("x\u{2028}=\u{a0}1\u{2029}");
    assert!(zipp_vm::parse_to_text("x = '\u{4e2d}", false).is_err());
}

/// 200,000 three-byte characters in one literal. Tokenizing them is linear;
/// when each decode checked the rest of the source it was 60 GB of checking.
#[test]
fn a_long_non_ascii_literal_tokenizes_in_linear_time() {
    let body = "\u{65e5}\u{672c}\u{8a9e}\u{306e}".repeat(50_000);
    let source = format!("const text = '{body}'; // {body}\n`{body}`");
    let started = Instant::now();
    let tree = parses(&source);
    let elapsed = started.elapsed();
    assert!(tree.len() > body.len());
    assert!(
        elapsed < Duration::from_secs(20),
        "tokenizing {} bytes took {elapsed:?}",
        source.len()
    );
}
