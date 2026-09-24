//! The parser: recursive descent for statements and patterns, precedence
//! climbing for binary operators, over the lexer's token vector, building
//! the arena [`Module`].
//!
//! It accepts what the RustPython 0.4 parser ZIPP used accepted (plus the
//! Python 3.12/3.13 syntax that parser lacked: PEP 701 f-strings and PEP 696
//! type parameter defaults), with the same node ranges, and rejects what it
//! rejected with the same message at the same offset: a syntax error is
//! reported at the first token that cannot continue a valid program, and the
//! grammar's semantic checks are reported where that LR parser reduced the
//! rule (see [`Follow`]).

use crate::ast::*;
use crate::intern::{Interner, Sym};
use crate::lexer::{self, Error, Lexed, Limits, Mode};
use crate::token::{Token, T};

mod compat;
mod expr;
mod fstring;
mod pattern;
mod stmt;

/// How deep expressions and blocks may nest before the parser stops with
/// an error rather than risk the native stack (about 2.4 KB per level).
const MAX_DEPTH: u32 = 1000;

/// A parse failure; `index` is the token it happened at.
pub(crate) struct Fail {
    pub error: Error,
    pub index: u32,
}

pub(crate) type PResult<T> = Result<T, Box<Fail>>;

/// Parse `source` (whose first byte is at offset `base`).
pub fn parse<'s>(
    source: &'s str,
    mode: Mode,
    base: u32,
    limits: Limits,
) -> Result<Module<'s>, Error> {
    let lexed = lexer::lex(source, mode, base, limits);
    parse_lexed(source, mode, base, lexed)
}

pub(crate) fn parse_lexed<'s>(
    source: &'s str,
    mode: Mode,
    base: u32,
    lexed: Lexed<'s>,
) -> Result<Module<'s>, Error> {
    let Lexed {
        mut tokens,
        names,
        errors,
    } = lexed;
    // The end of input is where the last token ends (the LR parser's last
    // location), not after trailing whitespace.
    let n = tokens.len();
    if tokens[n - 1].kind == T::EndOfFile {
        let end = if n > 1 { tokens[n - 2].end } else { 0 };
        tokens[n - 1].start = end;
        tokens[n - 1].end = end;
    }
    let mut parser = Parser::new(source, base, tokens, errors, names);
    match parser.parse_mod(mode) {
        Ok(body) => {
            parser.m.body = body;
            parser.m.end = parser.prev_end;
            let mut module = parser.m;
            module.line_starts = line_starts(source, base);
            Ok(module)
        }
        Err(fail) => Err(fail.error),
    }
}

fn line_starts(source: &str, base: u32) -> Vec<u32> {
    let mut starts = vec![base];
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => starts.push(base + i as u32 + 1),
            b'\r' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                starts.push(base + i as u32 + 1);
            }
            _ => {}
        }
        i += 1;
    }
    starts
}

/// Scratch stacks for the lists being built (a list's elements are pushed
/// here while its elements' own lists complete, then moved as one block).
#[derive(Default)]
struct Scratch {
    exprs: Vec<ExprId>,
    opt_exprs: Vec<Option<ExprId>>,
    stmts: Vec<StmtId>,
    pats: Vec<PatId>,
    syms: Vec<Sym>,
    cmp_ops: Vec<CmpOp>,
    keywords: Vec<Keyword>,
    comprehensions: Vec<Comprehension>,
    params: Vec<Param>,
    with_items: Vec<WithItem>,
    handlers: Vec<ExceptHandler>,
    aliases: Vec<Alias>,
    cases: Vec<MatchCase>,
    type_params: Vec<TypeParam>,
}

/// Every length a backtrack or an abandoned f-string restores.
#[derive(Clone, Copy)]
pub(crate) struct Mark {
    pos: usize,
    prev_end: u32,
    depth: u32,
    lens: [usize; 34],
}

pub(crate) struct Parser<'s> {
    src: &'s str,
    base: u32,
    toks: Vec<Token>,
    errors: Vec<Error>,
    pos: usize,
    /// End of the last consumed token.
    prev_end: u32,
    depth: u32,
    /// Token index of an unparenthesized `with` statement's first item.
    with_item_start: usize,
    s: Scratch,
    pub(crate) m: Module<'s>,
    /// Decoding buffer for the literal text of a string run.
    run_buf: String,
    /// How many f-strings enclose the current token.
    fstring_level: u32,
    /// The `\r\n` offsets of the outermost f-string being parsed, when it
    /// has any (see `fstring::shift`).
    crlf: Vec<u32>,
}

macro_rules! finish {
    ($name:ident, $field:ident, $table:ident, $t:ty) => {
        #[inline]
        pub(crate) fn $name(&mut self, base: usize) -> List<$t> {
            let start = self.m.$table.len();
            self.m.$table.extend(self.s.$field.drain(base..));
            List::new(start as u32, (self.m.$table.len() - start) as u32)
        }
    };
}

impl<'s> Parser<'s> {
    fn new(
        src: &'s str,
        base: u32,
        toks: Vec<Token>,
        errors: Vec<Error>,
        names: Interner<'s>,
    ) -> Self {
        let exprs = Vec::with_capacity(toks.len() / 2);
        Parser {
            src,
            base,
            toks,
            errors,
            pos: 0,
            prev_end: 0,
            depth: 0,
            with_item_start: usize::MAX,
            s: Scratch::default(),
            m: Module {
                source: src,
                body: List::default(),
                exprs,
                stmts: Vec::new(),
                patterns: Vec::new(),
                arguments: Vec::new(),
                expr_lists: Vec::new(),
                opt_expr_lists: Vec::new(),
                stmt_lists: Vec::new(),
                pat_lists: Vec::new(),
                sym_lists: Vec::new(),
                cmp_ops: Vec::new(),
                keywords: Vec::new(),
                comprehensions: Vec::new(),
                params: Vec::new(),
                with_items: Vec::new(),
                handlers: Vec::new(),
                aliases: Vec::new(),
                match_cases: Vec::new(),
                type_params: Vec::new(),
                names,
                str_data: String::new(),
                bytes_data: Vec::new(),
                line_starts: Vec::new(),
                base,
                end: 0,
            },
            run_buf: String::new(),
            fstring_level: 0,
            crlf: Vec::new(),
        }
    }

    finish!(finish_exprs, exprs, expr_lists, ExprId);
    finish!(finish_opt_exprs, opt_exprs, opt_expr_lists, Option<ExprId>);
    finish!(finish_stmts, stmts, stmt_lists, StmtId);
    finish!(finish_pats, pats, pat_lists, PatId);
    finish!(finish_syms, syms, sym_lists, Sym);
    finish!(finish_cmp_ops, cmp_ops, cmp_ops, CmpOp);
    finish!(finish_keywords, keywords, keywords, Keyword);
    finish!(
        finish_comprehensions,
        comprehensions,
        comprehensions,
        Comprehension
    );
    finish!(finish_with_items, with_items, with_items, WithItem);
    finish!(finish_handlers, handlers, handlers, ExceptHandler);
    finish!(finish_aliases, aliases, aliases, Alias);
    finish!(finish_cases, cases, match_cases, MatchCase);
    finish!(finish_type_params, type_params, type_params, TypeParam);

    #[inline]
    pub(crate) fn add_expr(&mut self, range: Range, kind: ExprKind) -> ExprId {
        let id = ExprId::from_index(self.m.exprs.len());
        self.m.exprs.push(Expr { range, kind });
        id
    }

    #[inline]
    pub(crate) fn add_stmt(&mut self, range: Range, kind: StmtKind) -> StmtId {
        let id = StmtId::from_index(self.m.stmts.len());
        self.m.stmts.push(Stmt { range, kind });
        id
    }

    #[inline]
    pub(crate) fn add_pat(&mut self, range: Range, kind: PatternKind) -> PatId {
        let id = PatId::from_index(self.m.patterns.len());
        self.m.patterns.push(Pattern { range, kind });
        id
    }

    #[inline]
    pub(crate) fn expr_range(&self, id: ExprId) -> Range {
        self.m.exprs[id.index()].range
    }

    #[inline]
    pub(crate) fn tok(&self) -> T {
        self.toks[self.pos].kind
    }

    #[inline]
    pub(crate) fn token(&self) -> Token {
        self.toks[self.pos]
    }

    #[inline]
    pub(crate) fn at(&self, kind: T) -> bool {
        self.toks[self.pos].kind == kind
    }

    /// The kind of the `n`th token after the current one.
    #[inline]
    pub(crate) fn peek(&self, n: usize) -> T {
        self.toks
            .get(self.pos + n)
            .map_or(T::EndOfFile, |tok| tok.kind)
    }

    /// The current token is the end of input or a lexical error.
    #[inline]
    pub(crate) fn sentinel(&self) -> bool {
        matches!(self.tok(), T::EndOfFile | T::Error)
    }

    #[inline]
    pub(crate) fn at_eof(&self) -> bool {
        self.at(T::EndOfFile)
    }

    #[inline]
    pub(crate) fn start(&self) -> u32 {
        self.toks[self.pos].start
    }

    #[inline]
    pub(crate) fn bump(&mut self) {
        debug_assert!(!self.sentinel());
        self.prev_end = self.toks[self.pos].end;
        self.pos += 1;
    }

    pub(crate) fn mark(&self) -> Mark {
        let s = &self.s;
        let m = &self.m;
        Mark {
            pos: self.pos,
            prev_end: self.prev_end,
            depth: self.depth,
            lens: [
                s.exprs.len(),
                s.opt_exprs.len(),
                s.stmts.len(),
                s.pats.len(),
                s.syms.len(),
                s.cmp_ops.len(),
                s.keywords.len(),
                s.comprehensions.len(),
                s.params.len(),
                s.with_items.len(),
                s.handlers.len(),
                s.aliases.len(),
                s.cases.len(),
                s.type_params.len(),
                m.exprs.len(),
                m.stmts.len(),
                m.patterns.len(),
                m.arguments.len(),
                m.expr_lists.len(),
                m.opt_expr_lists.len(),
                m.stmt_lists.len(),
                m.pat_lists.len(),
                m.sym_lists.len(),
                m.cmp_ops.len(),
                m.keywords.len(),
                m.comprehensions.len(),
                m.params.len(),
                m.with_items.len(),
                m.handlers.len(),
                m.aliases.len(),
                m.match_cases.len(),
                m.type_params.len(),
                m.str_data.len(),
                m.bytes_data.len(),
            ],
        }
    }

    /// Return to `mark`, dropping everything built since.
    pub(crate) fn rewind(&mut self, mark: &Mark) {
        self.pos = mark.pos;
        self.prev_end = mark.prev_end;
        self.depth = mark.depth;
        let l = &mark.lens;
        let s = &mut self.s;
        s.exprs.truncate(l[0]);
        s.opt_exprs.truncate(l[1]);
        s.stmts.truncate(l[2]);
        s.pats.truncate(l[3]);
        s.syms.truncate(l[4]);
        s.cmp_ops.truncate(l[5]);
        s.keywords.truncate(l[6]);
        s.comprehensions.truncate(l[7]);
        s.params.truncate(l[8]);
        s.with_items.truncate(l[9]);
        s.handlers.truncate(l[10]);
        s.aliases.truncate(l[11]);
        s.cases.truncate(l[12]);
        s.type_params.truncate(l[13]);
        let m = &mut self.m;
        m.exprs.truncate(l[14]);
        m.stmts.truncate(l[15]);
        m.patterns.truncate(l[16]);
        m.arguments.truncate(l[17]);
        m.expr_lists.truncate(l[18]);
        m.opt_expr_lists.truncate(l[19]);
        m.stmt_lists.truncate(l[20]);
        m.pat_lists.truncate(l[21]);
        m.sym_lists.truncate(l[22]);
        m.cmp_ops.truncate(l[23]);
        m.keywords.truncate(l[24]);
        m.comprehensions.truncate(l[25]);
        m.params.truncate(l[26]);
        m.with_items.truncate(l[27]);
        m.handlers.truncate(l[28]);
        m.aliases.truncate(l[29]);
        m.match_cases.truncate(l[30]);
        m.type_params.truncate(l[31]);
        m.str_data.truncate(l[32]);
        m.bytes_data.truncate(l[33]);
    }

    pub(crate) fn fail(&self, error: Error) -> Box<Fail> {
        Box::new(Fail {
            error,
            index: self.pos as u32,
        })
    }

    /// The error for a current token that cannot continue the program.
    #[cold]
    pub(crate) fn unexpected(&self) -> Box<Fail> {
        self.unexpected_expecting(false)
    }

    #[cold]
    pub(crate) fn unexpected_expecting(&self, indent: bool) -> Box<Fail> {
        let tok = self.token();
        match tok.kind {
            T::Error => self.fail(self.errors[tok.data as usize].clone()),
            // The LR parser's `UnrecognizedEof`: at the end of the last
            // token; an indentation error when only an indent could follow.
            T::EndOfFile => self.fail(Error::new(
                if indent {
                    lexer::msg::INDENTATION
                } else {
                    "Got unexpected EOF"
                },
                self.prev_end,
            )),
            T::Indent => self.fail(Error::new("unexpected indent", tok.start)),
            _ if indent => self.fail(Error::new("expected an indented block", tok.start)),
            _ => self.fail(Error::new(
                format!(
                    "invalid syntax. Got unexpected token {}",
                    self.display(self.pos)
                ),
                tok.start,
            )),
        }
    }

    /// A token as the classic parser's messages spelled it.
    fn display(&self, index: usize) -> String {
        let tok = self.toks[index];
        let text = &self.src[(tok.start - self.base) as usize..(tok.end - self.base) as usize];
        match tok.kind {
            T::Name => format!("'{}'", self.m.names.get(Sym(tok.data))),
            T::Int => format!(
                "'{}'",
                crate::numbers_display::int_decimal(text, tok.flags as u32)
            ),
            T::Float => format!("'{}'", crate::numbers::float(text)),
            T::Complex => format!("0j{}", crate::numbers::float(&text[..text.len() - 1])),
            T::String | T::FStringStart | T::FStringBroken => {
                let end = if tok.kind == T::FStringStart {
                    self.toks[tok.data as usize].end
                } else {
                    tok.end
                };
                let literal =
                    &self.src[(tok.start - self.base) as usize..(end - self.base) as usize];
                let kind = tok.str_kind();
                let quotes = if tok.triple() { 3 } else { 1 };
                let content = &literal[kind.prefix_len() as usize + quotes..literal.len() - quotes];
                let content = content.replace("\r\n", "\n").replace('\r', "\n");
                let q = if tok.triple() { "\"\"\"" } else { "\"" };
                format!("{}{q}{content}{q}", kind.display())
            }
            T::Newline => "Newline".to_owned(),
            T::Indent => "Indent".to_owned(),
            T::Dedent => "Dedent".to_owned(),
            T::EndOfFile => "EOF".to_owned(),
            _ => format!("'{text}'"),
        }
    }

    /// A check the grammar makes when it reduces a rule failed. The LR
    /// parser reduces once it has read the next token, and only if that
    /// token is in the rule's lookahead set; otherwise the token is the
    /// syntax error, and a lexical error there comes first.
    #[cold]
    pub(crate) fn reduce_error(&self, error: Error, follow: Follow) -> Box<Fail> {
        let reduces = match self.tok() {
            T::Error => return self.unexpected(),
            T::EndOfFile => follow.eof(),
            kind => follow.contains(kind),
        };
        if reduces {
            self.fail(error)
        } else {
            self.unexpected()
        }
    }

    #[inline]
    pub(crate) fn expect(&mut self, kind: T) -> PResult<()> {
        if self.at(kind) {
            self.bump();
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    #[inline]
    pub(crate) fn identifier(&mut self) -> PResult<Sym> {
        if self.at(T::Name) {
            let sym = Sym(self.toks[self.pos].data);
            self.bump();
            return Ok(sym);
        }
        Err(self.unexpected())
    }

    pub(crate) fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.fail(Error::new(
                "too many nested expressions or blocks",
                self.start(),
            )));
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn leave(&mut self) {
        self.depth -= 1;
    }

    fn parse_mod(&mut self, mode: Mode) -> PResult<List<StmtId>> {
        match mode {
            Mode::Module | Mode::Interactive => self.parse_program(),
            Mode::Expression => {
                let start = self.start();
                let (body, _) = self.parse_list(true)?;
                while self.at(T::Newline) {
                    self.bump();
                }
                if !self.at_eof() {
                    return Err(self.unexpected());
                }
                let range = self.expr_range(body);
                let _ = start;
                let stmt = self.add_stmt(range, StmtKind::Expr { value: body });
                let base = self.s.stmts.len();
                self.s.stmts.push(stmt);
                Ok(self.finish_stmts(base))
            }
        }
    }

    /// A string token's content (between its quotes) and where it starts.
    pub(crate) fn string_content(&self, tok: &Token, end: u32) -> (&'s str, u32) {
        let kind = tok.str_kind();
        let quotes = if tok.triple() { 3 } else { 1 };
        let start = tok.start + kind.prefix_len() + quotes;
        let text = &self.src[(start - self.base) as usize..(end - quotes - self.base) as usize];
        (text, start)
    }
}

/// The lookahead sets of the grammar rules whose actions can fail (LALR
/// lookahead sets merge contexts, so these are the tokens that can follow
/// the rule anywhere).
#[derive(Clone, Copy)]
pub(crate) enum Follow {
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

    fn contains(self, tok: T) -> bool {
        use T::*;
        let test = matches!(
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
        );
        match self {
            Follow::Test => test,
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
                    continues || tok == Else || test
                }
            }
            Follow::Parameters => matches!(tok, Rarrow | Colon),
            Follow::Checked => true,
            Follow::Pattern => matches!(tok, Comma | Rpar | Rsqb | Rbrace | Colon | If),
            Follow::ClosedPattern => {
                matches!(tok, Comma | Rpar | Rsqb | Rbrace | Colon | If | Vbar | As)
            }
            Follow::MappingKey => tok == Colon,
        }
    }
}

#[inline(always)]
pub(crate) fn range(start: u32, end: u32) -> Range {
    Range::new(start, end)
}

/// The end of a (non-empty) statement list.
pub(crate) fn body_end(m: &Module, body: List<StmtId>) -> u32 {
    m.list(body).last().map_or(0, |s| m.stmt(*s).range.end)
}
