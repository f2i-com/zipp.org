//! The hand-written parser: recursive descent for statements, precedence
//! climbing for binary operators.
//!
//! It replaces the LALRPOP-generated LR parser (`src/python.lalrpop`, kept as
//! the grammar reference) and builds the same AST, with the same node ranges,
//! from the same token stream. Errors keep the LR parser's shape: a syntax
//! error is reported at the first token that cannot continue a valid program
//! (`UnrecognizedToken`, or `Eof` / an indentation error at the end of the
//! input), and the grammar's semantic checks (duplicate parameters, starred
//! expressions in parentheses, ...) raise the same `LexicalError`s at the same
//! locations. See `FORK.md` for the design and how it is validated against
//! the generated parser.
//!
//! The design follows the one Ruff moved to when it replaced its own LALRPOP
//! parser (MIT, <https://github.com/astral-sh/ruff>): a token source with a
//! small lookahead buffer and checkpoints, and one precedence-climbing routine
//! for every binary level. No code is copied from it.

use crate::{
    ast::{self, Ranged},
    lexer::{LexResult, LexicalError, LexicalErrorType},
    parser::{ParseError, ParseErrorType},
    text_size::{TextRange, TextSize},
    token::Tok,
    Mode,
};
use std::collections::VecDeque;

mod expr;
mod pattern;
mod stmt;

/// How deep expressions and blocks may nest. The parser recurses per nesting
/// level (a few frames each), so this bounds its stack use well inside the
/// 16 MiB WebAssembly stack; the generated LR parser kept its stack on the
/// heap. Real programs stay far below it (ZIPP's own frontend caps brackets at
/// 200 and indentation at 100).
const MAX_DEPTH: u32 = 1000;

/// A parse failure. `index` counts the tokens consumed before it, which is
/// how the one backtracking point (parenthesized `with` items) picks the
/// failure that got further, as the LR parser would have reported it.
pub(crate) struct Fail {
    error: ParseErrorType,
    offset: TextSize,
    index: u32,
}

type PResult<T> = Result<T, Box<Fail>>;

/// Parse a token stream (without the start marker) in `mode`.
pub(crate) fn parse_tokens(
    mut tokens: impl Iterator<Item = LexResult>,
    mode: Mode,
    source_path: &str,
) -> Result<ast::Mod, ParseError> {
    // One parser for every token source: it pulls through a `dyn` call (a
    // small cost next to lexing a token) instead of being compiled again
    // per iterator type.
    let mut parser = Parser::new(&mut tokens);
    parser.parse_mod(mode).map_err(|fail| ParseError {
        error: fail.error,
        offset: fail.offset,
        source_path: source_path.to_owned(),
    })
}

/// A saved token position for backtracking.
struct Mark {
    depth: u32,
    log_len: usize,
    prev_end: TextSize,
    index: u32,
}

struct Parser<'t> {
    tokens: &'t mut dyn Iterator<Item = LexResult>,
    /// The token stream has ended (or produced an error); it is not polled again.
    done: bool,
    /// The current token. At the end of the input, or at a lexical error,
    /// this is `Tok::EndOfFile` with `sentinel` set.
    tok: Tok,
    range: TextRange,
    sentinel: bool,
    /// The lexical error the current (sentinel) token stands for.
    lex_error: Option<LexicalError>,
    /// Tokens (or the lexical error) after the current one, already pulled.
    ahead: VecDeque<LexResult>,
    /// End of the last consumed token: what LALRPOP's `@R` is.
    prev_end: TextSize,
    /// Number of consumed tokens.
    index: u32,
    /// Consumed tokens, kept while a backtracking mark is active.
    log: Vec<(Tok, TextRange)>,
    marks: u32,
    depth: u32,
    /// Index of the first token of an unparenthesized `with` statement's
    /// first item, whose leading atom has its own lookahead set.
    with_item_start: u32,
}

impl<'t> Parser<'t> {
    fn new(tokens: &'t mut dyn Iterator<Item = LexResult>) -> Self {
        let mut parser = Parser {
            tokens,
            done: false,
            tok: Tok::EndOfFile,
            range: TextRange::default(),
            sentinel: true,
            lex_error: None,
            ahead: VecDeque::new(),
            prev_end: TextSize::default(),
            index: 0,
            log: Vec::new(),
            marks: 0,
            depth: 0,
            with_item_start: u32::MAX,
        };
        parser.advance();
        parser
    }

    /// Make the next token (or the end/error sentinel) current.
    fn advance(&mut self) {
        let next = match self.ahead.pop_front() {
            Some(next) => Some(next),
            None if self.done => None,
            None => {
                let next = self.tokens.next();
                if !matches!(next, Some(Ok(_))) {
                    self.done = true;
                }
                next
            }
        };
        match next {
            Some(Ok((tok, range))) => {
                self.tok = tok;
                self.range = range;
                self.sentinel = false;
            }
            Some(Err(error)) => {
                self.tok = Tok::EndOfFile;
                self.range = TextRange::empty(error.location);
                self.sentinel = true;
                self.lex_error = Some(error);
            }
            None => {
                self.tok = Tok::EndOfFile;
                self.range = TextRange::empty(self.prev_end);
                self.sentinel = true;
            }
        }
    }

    /// Consume the current token and return it.
    #[inline]
    fn bump(&mut self) -> Tok {
        debug_assert!(!self.sentinel);
        let range = self.range;
        self.prev_end = range.end();
        self.index += 1;
        let tok = std::mem::replace(&mut self.tok, Tok::EndOfFile);
        if self.marks > 0 {
            self.log.push((tok.clone(), range));
        }
        self.advance();
        tok
    }

    /// The `n`th token after the current one (`n >= 1`), if it is a token.
    fn peek(&mut self, n: usize) -> Option<&Tok> {
        while self.ahead.len() < n {
            if self.done {
                return None;
            }
            match self.tokens.next() {
                Some(item) => {
                    if item.is_err() {
                        self.done = true;
                    }
                    self.ahead.push_back(item);
                }
                None => {
                    self.done = true;
                    return None;
                }
            }
        }
        match self.ahead.get(n - 1) {
            Some(Ok((tok, _))) => Some(tok),
            _ => None,
        }
    }

    #[inline]
    fn start(&self) -> TextSize {
        self.range.start()
    }

    #[inline]
    fn at(&self, tok: Tok) -> bool {
        self.tok == tok
    }

    /// At the end of the input (not at a lexical error).
    fn at_eof(&self) -> bool {
        self.sentinel && self.lex_error.is_none()
    }

    fn mark(&mut self) -> Mark {
        self.marks += 1;
        Mark {
            depth: self.depth,
            log_len: self.log.len(),
            prev_end: self.prev_end,
            index: self.index,
        }
    }

    /// Keep what was parsed since `mark`.
    fn release(&mut self, _mark: Mark) {
        self.marks -= 1;
        if self.marks == 0 {
            self.log.clear();
        }
    }

    /// Return to `mark`: every token consumed since is replayed.
    fn rewind(&mut self, mark: Mark) {
        if self.sentinel {
            if let Some(error) = self.lex_error.take() {
                self.ahead.push_front(Err(error));
            }
        } else {
            let tok = std::mem::replace(&mut self.tok, Tok::EndOfFile);
            self.ahead.push_front(Ok((tok, self.range)));
        }
        for token in self.log.drain(mark.log_len..).rev() {
            self.ahead.push_front(Ok(token));
        }
        self.prev_end = mark.prev_end;
        self.index = mark.index;
        self.depth = mark.depth;
        self.marks -= 1;
        if self.marks == 0 {
            self.log.clear();
        }
        self.advance();
    }

    fn fail(&self, error: ParseErrorType, offset: TextSize) -> Box<Fail> {
        Box::new(Fail {
            error,
            offset,
            index: self.index,
        })
    }

    /// The error for a current token that cannot continue the program.
    #[cold]
    fn unexpected(&self) -> Box<Fail> {
        self.unexpected_expecting(None)
    }

    #[cold]
    fn unexpected_expecting(&self, expected: Option<&str>) -> Box<Fail> {
        if self.sentinel {
            if let Some(error) = &self.lex_error {
                return self.fail(ParseErrorType::Lexical(error.error.clone()), error.location);
            }
            // LALRPOP: `UnrecognizedEof` at the end of the last token; when
            // only an indent could follow, an indentation error.
            let error = if expected == Some("Indent") {
                ParseErrorType::Lexical(LexicalErrorType::IndentationError)
            } else {
                ParseErrorType::Eof
            };
            return self.fail(error, self.prev_end);
        }
        self.fail(
            ParseErrorType::UnrecognizedToken(self.tok.clone(), expected.map(str::to_owned)),
            self.range.start(),
        )
    }

    /// A check the grammar makes when it reduces a rule failed. The LR
    /// parser reduces once it has read the next token, and only if that
    /// token is in the rule's lookahead set (`follow`); otherwise the token
    /// is the syntax error, and a lexical error there comes first.
    #[cold]
    fn reduce_error(&self, error: LexicalError, follow: Follow) -> Box<Fail> {
        let reduces = if self.sentinel {
            if let Some(lex) = &self.lex_error {
                return self.fail(ParseErrorType::Lexical(lex.error.clone()), lex.location);
            }
            follow.eof()
        } else {
            follow.contains(&self.tok)
        };
        if reduces {
            self.fail(ParseErrorType::Lexical(error.error), error.location)
        } else {
            self.unexpected()
        }
    }

    #[cold]
    fn reduce_error_other(&self, message: &str, location: TextSize, follow: Follow) -> Box<Fail> {
        self.reduce_error(
            LexicalError::new(LexicalErrorType::OtherError(message.to_owned()), location),
            follow,
        )
    }

    #[inline]
    fn expect(&mut self, tok: Tok) -> PResult<()> {
        if self.tok == tok {
            self.bump();
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn identifier(&mut self) -> PResult<ast::Identifier> {
        if matches!(self.tok, Tok::Name { .. }) {
            if let Tok::Name { name } = self.bump() {
                return Ok(ast::Identifier::new(name));
            }
        }
        Err(self.unexpected())
    }

    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.fail(
                ParseErrorType::Lexical(LexicalErrorType::OtherError(
                    "too many nested expressions or blocks".to_owned(),
                )),
                self.start(),
            ));
        }
        Ok(())
    }

    #[inline]
    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn parse_mod(&mut self, mode: Mode) -> PResult<ast::Mod> {
        let start = TextSize::default();
        Ok(match mode {
            Mode::Module => {
                let body = self.parse_program()?;
                ast::ModModule {
                    body,
                    type_ignores: vec![],
                    range: optional_range(start, self.prev_end),
                }
                .into()
            }
            Mode::Interactive => {
                let body = self.parse_program()?;
                ast::ModInteractive {
                    body,
                    range: optional_range(start, self.prev_end),
                }
                .into()
            }
            Mode::Expression => {
                let (body, _) = self.parse_list(true)?;
                while self.at(Tok::Newline) {
                    self.bump();
                }
                if !self.at_eof() {
                    return Err(self.unexpected());
                }
                ast::ModExpression {
                    body: Box::new(body),
                    range: optional_range(start, self.prev_end),
                }
                .into()
            }
        })
    }
}

/// The lookahead sets of the grammar rules whose actions can fail (LALR
/// lookahead sets merge contexts, so these are the tokens that can follow the
/// rule anywhere).
#[derive(Clone, Copy)]
enum Follow {
    /// An `Atom` (strings, parenthesized forms).
    Atom,
    /// A `Test` (a lambda).
    Test,
    /// A function's `Parameters`.
    Parameters,
    /// A rule reduced at a token the caller has already checked.
    Checked,
    /// The `Atom` that starts the first item of a `with` statement (the
    /// grammar's `Test<"no-withitems">`): an item or an operator follows.
    WithItemAtom,
    /// A `Pattern`.
    Pattern,
    /// A literal (string) pattern.
    ClosedPattern,
    /// A (string) key of a mapping pattern.
    MappingKey,
}

impl Follow {
    fn eof(self) -> bool {
        matches!(self, Follow::Atom | Follow::Test | Follow::Checked)
    }

    fn contains(self, tok: &Tok) -> bool {
        use Tok::*;
        let test = || {
            matches!(
                tok,
                Rpar | Rsqb
                    | Rbrace
                    | Comma
                    | Colon
                    | Equal
                    | Semi
                    | Newline
                    | For
                    | Async
                    | As
                    | From
                    | PlusEqual
                    | MinusEqual
                    | StarEqual
                    | AtEqual
                    | SlashEqual
                    | PercentEqual
                    | AmperEqual
                    | VbarEqual
                    | CircumflexEqual
                    | LeftShiftEqual
                    | RightShiftEqual
                    | DoubleStarEqual
                    | DoubleSlashEqual
            )
        };
        match self {
            Follow::Test => test(),
            Follow::Atom | Follow::WithItemAtom => {
                let continues = matches!(
                    tok,
                    Lpar | Lsqb
                        | Dot
                        | DoubleStar
                        | Plus
                        | Minus
                        | Star
                        | Slash
                        | DoubleSlash
                        | Percent
                        | At
                        | Vbar
                        | CircumFlex
                        | Amper
                        | LeftShift
                        | RightShift
                        | EqEqual
                        | NotEqual
                        | Less
                        | LessEqual
                        | Greater
                        | GreaterEqual
                        | In
                        | Not
                        | Is
                        | And
                        | Or
                        | If
                );
                if let Follow::WithItemAtom = self {
                    continues || matches!(tok, Comma | Colon | As)
                } else {
                    continues || matches!(tok, Else) || test()
                }
            }
            Follow::Parameters => matches!(tok, Rarrow | Colon),
            Follow::Checked => true,
            Follow::Pattern => matches!(tok, Comma | Rpar | Rsqb | Rbrace | Colon | If),
            Follow::ClosedPattern => {
                matches!(tok, Comma | Rpar | Rsqb | Rbrace | Colon | If | Vbar | As)
            }
            Follow::MappingKey => matches!(tok, Colon),
        }
    }
}

#[inline(always)]
fn optional_range(start: TextSize, end: TextSize) -> ast::OptionalRange<TextRange> {
    ast::OptionalRange::<TextRange>::new(start, end)
}

#[inline(always)]
fn range(start: TextSize, end: TextSize) -> TextRange {
    TextRange::new(start, end)
}

/// The end of a (non-empty) statement list.
fn body_end(body: &[ast::Stmt]) -> TextSize {
    body.last().map(Ranged::end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use crate::{ast, Parse};

    /// Right-nested chains (and the postfix and operator chains) are parsed
    /// without recursing per link: ZIPP's frontend relies on a million-deep
    /// unary chain reaching its own nesting check instead of overflowing the
    /// native stack. The trees are leaked, since dropping them recurses.
    #[test]
    fn long_chains_parse_on_a_small_stack() {
        let cases = [
            ("x = ", "-", "1"),
            ("x = ", "~", "1"),
            ("x = ", "+", "1"),
            ("x = ", "not ", "1"),
            ("x = a", ".b", ""),
            ("x = f", "()", ""),
            ("x = f", "[0]", ""),
            ("x = ", "lambda: ", "1"),
            ("x = ", "lambda a, *b, c=1: ", "1"),
            ("x = ", "a if b else ", "c"),
            ("x = ", "a ** -", "1"),
            ("x = ", "await ", "1"),
            ("x = 1", " + 1", ""),
            ("x = 1", " < 1", ""),
            ("x = 1", " or 1", ""),
            ("x = ", "'s' ", ""),
        ];
        for (prefix, unit, suffix) in cases {
            let source = format!("{prefix}{}{suffix}\n", unit.repeat(100_000));
            let parsed = std::thread::Builder::new()
                .stack_size(256 << 10)
                .spawn(move || {
                    let result = ast::Suite::parse(&source, "<test>");
                    let ok = result.is_ok();
                    std::mem::forget(result);
                    ok
                })
                .unwrap()
                .join()
                .unwrap_or_else(|_| panic!("{unit:?} chain overflowed the stack"));
            // `await await` is not Python; everything else is.
            assert_eq!(parsed, unit != "await ", "{unit:?}");
        }
    }

    /// Nesting that does recurse (brackets, blocks, lambda defaults) is
    /// bounded, with an error instead of a stack overflow.
    #[test]
    fn deep_brackets_are_an_error_not_an_overflow() {
        for (open, close) in [
            ("(", ")"),
            ("[", "]"),
            ("{", "}"),
            ("f(", ")"),
            ("lambda a=", ": 0"),
        ] {
            let source = format!("x = {}1{}\n", open.repeat(5_000), close.repeat(5_000));
            let error = std::thread::Builder::new()
                .stack_size(64 << 20)
                .spawn(move || {
                    ast::Suite::parse(&source, "<test>")
                        .unwrap_err()
                        .to_string()
                })
                .unwrap()
                .join()
                .unwrap();
            assert!(error.contains("too many nested"), "{open}: {error}");
        }
    }
}
