//! The LALRPOP-generated LR parser (`src/python.lalrpop` compiled into
//! `src/python.rs`) this crate used before the hand-written one in
//! `src/descent/`. It is compiled only with the test-only `reference-parser`
//! feature, for the differential test in `tests/differential.rs`: shipped
//! builds contain neither it nor `lalrpop-util`.

use crate::{
    ast,
    lexer::{LexResult, LexicalError, LexicalErrorType},
    parser::{ParseError, ParseErrorType},
    python,
    text_size::TextSize,
    token::Tok,
    Mode,
};
use lalrpop_util::ParseError as LalrpopError;
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Whether [`parse_tokens`] is running on this thread. The generated parser
/// parses f-string replacement fields through the public `Parse` API, which
/// consults this so they are parsed by the reference parser too.
pub(crate) fn active() -> bool {
    ACTIVE.with(Cell::get)
}

/// Parse a token stream with the reference parser, like
/// [`crate::parse_tokens`] (including every nested f-string expression).
pub fn parse_tokens(
    lxr: impl IntoIterator<Item = LexResult>,
    mode: Mode,
    source_path: &str,
) -> Result<ast::Mod, ParseError> {
    let previous = ACTIVE.with(|active| active.replace(true));
    let result = crate::parse_tokens(lxr, mode, source_path);
    ACTIVE.with(|active| active.set(previous));
    result
}

pub(crate) fn parse_lalrpop(
    lxr: impl IntoIterator<Item = LexResult>,
    mode: Mode,
    source_path: &str,
) -> Result<ast::Mod, ParseError> {
    let marker_token = (Tok::start_marker(mode), Default::default());
    let lexer = std::iter::once(Ok(marker_token)).chain(lxr);
    python::TopParser::new()
        .parse(lexer.map(|item| item.map(|(t, range)| (range.start(), t, range.end()))))
        .map_err(|e| parse_error_from_lalrpop(e, source_path))
}

// Convert `lalrpop_util::ParseError` to our internal type
fn parse_error_from_lalrpop(
    err: LalrpopError<TextSize, Tok, LexicalError>,
    source_path: &str,
) -> ParseError {
    let source_path = source_path.to_owned();

    match err {
        // TODO: Are there cases where this isn't an EOF?
        LalrpopError::InvalidToken { location } => ParseError {
            error: ParseErrorType::Eof,
            offset: location,
            source_path,
        },
        LalrpopError::ExtraToken { token } => ParseError {
            error: ParseErrorType::ExtraToken(token.1),
            offset: token.0,
            source_path,
        },
        LalrpopError::User { error } => ParseError {
            error: ParseErrorType::Lexical(error.error),
            offset: error.location,
            source_path,
        },
        LalrpopError::UnrecognizedToken { token, expected } => {
            // Hacky, but it's how CPython does it. See PyParser_AddToken,
            // in particular "Only one possible expected token" comment.
            let expected = (expected.len() == 1).then(|| expected[0].clone());
            ParseError {
                error: ParseErrorType::UnrecognizedToken(token.1, expected),
                offset: token.0,
                source_path,
            }
        }
        LalrpopError::UnrecognizedEof { location, expected } => {
            // This could be an initial indentation error that we should ignore
            let indent_error = expected == ["Indent"];
            if indent_error {
                ParseError {
                    error: ParseErrorType::Lexical(LexicalErrorType::IndentationError),
                    offset: location,
                    source_path,
                }
            } else {
                ParseError {
                    error: ParseErrorType::Eof,
                    offset: location,
                    source_path,
                }
            }
        }
    }
}
