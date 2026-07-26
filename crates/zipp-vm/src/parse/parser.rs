//! The parser: recursive descent over [`super::lexer`], producing
//! [`super::ast`].
//!
//! ## Why hand-written
//!
//! Not for speed — parsing is ~12% of the cost of getting from source to
//! bytecode. For the ~2,200 static-semantics EARLY ERRORS the engine cannot
//! currently raise, every one of which needs binding, strictness or positional
//! state that only exists *while parsing*. A tree handed over after the fact
//! cannot reconstruct "was this the second `let x` in this scope".
//!
//! ## The three hard parts, and how they are handled here
//!
//! **1. Cover grammars.** JavaScript has three places where a prefix is
//! ambiguous for an unbounded distance, and no amount of lookahead fixes them:
//!
//! - `( a, b )` is either a parenthesized expression or arrow parameters, and
//!   the `=>` that decides it comes after the closing paren.
//! - `{ a = 1 }` is either an object literal (a SyntaxError, because
//!   `CoverInitializedName` is not a valid property) or a destructuring target
//!   (perfectly legal), decided by a later `=`.
//! - `async` is an identifier, a call target, or a function/arrow modifier.
//!
//! The technique is the spec's own: parse the permissive superset once, RECORD
//! rather than raise the errors that are fatal in one reading but not the
//! other, and discharge them when the ambiguity resolves. [`Cover`] holds those
//! deferred errors. Never backtrack, never re-lex.
//!
//! **2. ASI.** Driven entirely by [`Token::newline_before`], which the lexer
//! already computes correctly through block comments.
//!
//! **3. Scope tracking for early errors.** [`ScopeStack`] is the one structure
//! shared by the duplicate-declaration, parameter, for-head and export rules.
//! It is discarded when parsing finishes — the compiler builds its own.

use super::ast::*;
use super::lexer::{LexError, Lexer};
use super::token::{Keyword, Punct, Span, Token, TokenKind};

/// A syntax error, with the byte offset it was found at.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxError {
    pub msg: String,
    pub pos: u32,
}

impl SyntaxError {
    pub(crate) fn new(msg: impl Into<String>, pos: u32) -> SyntaxError {
        SyntaxError { msg: msg.into(), pos }
    }
}

impl From<LexError> for SyntaxError {
    fn from(e: LexError) -> SyntaxError {
        SyntaxError { msg: e.msg, pos: e.pos }
    }
}

pub type PResult<T> = Result<T, SyntaxError>;

/// Grammar parameters — the `[Yield]`, `[Await]`, `[In]`, `[Return]` subscripts
/// the spec threads through its productions, plus strictness.
///
/// Saved and restored around every construct that changes them, which is why
/// this is `Copy`: a function body resets `yield`/`await` from its own
/// generator/async flags rather than inheriting the enclosing ones, and an
/// arrow does the opposite (it inherits, because it has no binding of its own).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ctx {
    pub strict: bool,
    /// `yield` is a keyword here (inside a generator).
    pub yield_: bool,
    /// `await` is a keyword here (inside an async function, or a module's top
    /// level, where top-level await is legal).
    pub await_: bool,
    /// The `[In]` parameter: false only inside a `for` head, where a bare `in`
    /// would be mistaken for the `for-in` separator.
    pub in_: bool,
    /// `return` is legal (inside a function body, or `Goal::FunctionBody`).
    pub return_: bool,
    /// `new.target` is legal.
    pub new_target: bool,
    /// `super.x` is legal (a method).
    pub super_prop: bool,
    /// `super()` is legal (a derived constructor).
    pub super_call: bool,
    /// Inside a class field initializer or static block, where `arguments` is an
    /// early error.
    pub in_field_init: bool,
    /// Inside iteration/switch, for `break`/`continue` validity.
    pub in_loop: bool,
    pub in_switch: bool,
}

impl Default for Ctx {
    fn default() -> Ctx {
        Ctx {
            strict: false,
            yield_: false,
            await_: false,
            in_: true,
            return_: false,
            new_target: false,
            super_prop: false,
            super_call: false,
            in_field_init: false,
            in_loop: false,
            in_switch: false,
        }
    }
}

/// Errors deferred because the construct they occurred in is still ambiguous.
///
/// A cover region is parsed as the permissive superset of two productions. Some
/// things are fatal only if it turns out to be an expression (`...rest`,
/// `{a = 1}`), others only if it turns out to be a binding target (`a++`,
/// `new x`). Recording both and discharging the losing one when the `=>` or `=`
/// arrives is what avoids backtracking.
///
/// The FIRST of each kind wins, because the spec reports the earliest error.
#[derive(Debug, Clone, Default)]
pub(crate) struct Cover {
    /// Legal only as a binding/target: `...rest`, `{a = 1}`, a trailing comma
    /// after a rest element.
    pub pattern_only: Option<SyntaxError>,
    /// Legal only as an expression: `a++`, `new x`, a call, a literal.
    pub expr_only: Option<SyntaxError>,
    /// `yield`/`await` USED AS EXPRESSIONS inside the region. Legal where they
    /// stand, illegal in the arrow parameters the region may become — so this
    /// cannot be decided until the `=>` is seen.
    pub yield_await: Vec<SyntaxError>,
}

impl Cover {
    fn pattern_only(&mut self, e: SyntaxError) {
        if self.pattern_only.is_none() {
            self.pattern_only = Some(e);
        }
    }

    fn expr_only(&mut self, e: SyntaxError) {
        if self.expr_only.is_none() {
            self.expr_only = Some(e);
        }
    }
}

/// What kind of binding a name introduces. Decides which duplicate-declaration
/// rule applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindKind {
    Var,
    Let,
    Const,
    Class,
    Function,
    Param,
    CatchParam,
    Import,
}

impl BindKind {
    fn is_lexical(self) -> bool {
        matches!(self, BindKind::Let | BindKind::Const | BindKind::Class | BindKind::Import)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    Script,
    Module,
    Function,
    Block,
    /// A `switch`'s CaseBlock is ONE scope spanning every clause, not one per
    /// clause — `switch (x) { case 1: let a; case 2: let a; }` is a duplicate.
    Switch,
    Catch,
    /// A `for` head, so `for (let x of ...) { let x; }` is legal but
    /// `for (let x of ...) let x;` is not.
    ForHead,
    ClassStaticBlock,
}

#[derive(Debug)]
pub(crate) struct Scope {
    pub kind: ScopeKind,
    pub lex: Vec<(Box<str>, BindKind, u32)>,
    pub var: Vec<(Box<str>, u32)>,
    /// `var` stops here: Script, Module, Function, ClassStaticBlock.
    pub var_boundary: bool,
    /// Annex B B.3.5: `catch (e) { var e; }` is legal for a SIMPLE catch
    /// parameter only, so the exception has to know the parameter's shape.
    pub simple_catch_param: Option<Box<str>>,
}

/// The scope chain, owned by the parser and thrown away when it finishes.
///
/// Exists solely to raise early errors. The compiler builds its own scope
/// structures from the tree afterwards; duplicating them onto AST nodes would
/// be dead weight everywhere except here.
#[derive(Debug, Default)]
pub(crate) struct ScopeStack {
    pub scopes: Vec<Scope>,
}

impl ScopeStack {
    pub(crate) fn push(&mut self, kind: ScopeKind) {
        let var_boundary = matches!(
            kind,
            ScopeKind::Script | ScopeKind::Module | ScopeKind::Function | ScopeKind::ClassStaticBlock
        );
        self.scopes.push(Scope {
            kind,
            lex: Vec::new(),
            var: Vec::new(),
            var_boundary,
            simple_catch_param: None,
        });
    }

    /// Pop, propagating `var` names outward to the nearest var boundary — this
    /// is what makes `{ var x; }` a function-scoped declaration, and it is where
    /// `let x; { var x; }` is finally caught.
    pub(crate) fn pop(&mut self) -> PResult<()> {
        let Some(scope) = self.scopes.pop() else { return Ok(()) };
        if scope.var_boundary {
            return Ok(());
        }
        for (name, pos) in scope.var {
            self.declare_var(&name, pos)?;
        }
        Ok(())
    }

    /// `annexb_dup_ok`: Annex B.3.2.4/B.3.2.5 drop the duplicate-
    /// `LexicallyDeclaredNames` error in a Block or a CaseBlock when the source
    /// is NOT strict and BOTH entries come from FunctionDeclarations — which is
    /// what makes `{ function f(){} function f(){} }` legal in sloppy code and
    /// an error under `"use strict"`.
    pub(crate) fn declare_lexical(
        &mut self,
        name: &str,
        kind: BindKind,
        pos: u32,
        annexb_dup_ok: bool,
    ) -> PResult<()> {
        let Some(scope) = self.scopes.last_mut() else { return Ok(()) };
        let dup_lex = scope.lex.iter().any(|(n, k, _)| {
            &**n == name && !(annexb_dup_ok && *k == BindKind::Function)
        });
        if dup_lex || scope.var.iter().any(|(n, _)| &**n == name) {
            return Err(SyntaxError::new(
                format!("SyntaxError: Identifier '{name}' has already been declared"),
                pos,
            ));
        }
        scope.lex.push((name.into(), kind, pos));
        Ok(())
    }

    /// A FunctionDeclaration's name is VAR-scoped only at the top level of a
    /// Script, a FunctionBody or a ClassStaticBlock (the spec's
    /// `TopLevelVarDeclaredNames`). At a MODULE's top level it is lexical
    /// (`ModuleItemList` uses the plain `LexicallyDeclaredNames`), and inside a
    /// Block, a CaseBlock or a catch body it is block-scoped — so it must not be
    /// walked out to the nearest var boundary and checked against bindings it
    /// can never actually collide with.
    pub(crate) fn fn_decl_is_var_scoped(&self) -> bool {
        matches!(
            self.scopes.last().map(|s| s.kind),
            Some(ScopeKind::Script | ScopeKind::Function | ScopeKind::ClassStaticBlock)
        )
    }

    /// A `var` conflicts with any lexical binding between here and the nearest
    /// var boundary, inclusive.
    pub(crate) fn declare_var(&mut self, name: &str, pos: u32) -> PResult<()> {
        for scope in self.scopes.iter().rev() {
            if let Some(p) = &scope.simple_catch_param {
                // Annex B B.3.5 carve-out.
                if &**p == name {
                    break;
                }
            }
            if scope.lex.iter().any(|(n, _, _)| &**n == name) {
                return Err(SyntaxError::new(
                    format!("SyntaxError: Identifier '{name}' has already been declared"),
                    pos,
                ));
            }
            if scope.var_boundary {
                break;
            }
        }
        // Record at the nearest var boundary so a later lexical there conflicts.
        for scope in self.scopes.iter_mut().rev() {
            if scope.var_boundary {
                if !scope.var.iter().any(|(n, _)| &**n == name) {
                    scope.var.push((name.into(), pos));
                }
                return Ok(());
            }
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.var.push((name.into(), pos));
        }
        Ok(())
    }
}

/// Options the caller supplies. The goal is never sniffed: the engine has to
/// decide it once, explicitly, and a wrong guess changes strictness and whether
/// top-level `return` is legal.
#[derive(Debug, Clone)]
pub struct ParseOptions {
    pub goal: Goal,
    /// A direct eval from strict code, or `new Function` from a strict caller.
    pub force_strict: bool,
    /// Inherited from an eval call site — grammar parameters, not guesses.
    pub allow_return: bool,
    pub allow_new_target: bool,
    pub allow_super_property: bool,
    pub allow_super_call: bool,
    pub allow_await_expr: bool,
    pub allow_yield_expr: bool,
}

impl Default for ParseOptions {
    fn default() -> ParseOptions {
        ParseOptions {
            goal: Goal::Script,
            force_strict: false,
            allow_return: false,
            allow_new_target: false,
            allow_super_property: false,
            allow_super_call: false,
            allow_await_expr: false,
            allow_yield_expr: false,
        }
    }
}

impl ParseOptions {
    pub fn script() -> ParseOptions {
        ParseOptions::default()
    }

    pub fn module() -> ParseOptions {
        ParseOptions { goal: Goal::Module, force_strict: true, allow_await_expr: true, ..Default::default() }
    }
}

pub struct Parser<'s> {
    lx: Lexer<'s>,
    src: &'s str,
    /// The current token. There is always one; EOF is a token.
    tok: Token,
    /// Byte offset of the token BEFORE `tok`, for error positions that should
    /// point at what preceded rather than what follows.
    prev_end: u32,
    pub(crate) ctx: Ctx,
    pub(crate) scopes: ScopeStack,
    pub(crate) cover: Cover,
    pub(crate) opts: ParseOptions,
    /// Labels in scope, for duplicate-label and `break`/`continue` target
    /// checks. Reset at every function boundary.
    pub(crate) labels: Vec<Box<str>>,
    /// The expression just parsed was parenthesized. A one-slot flag rather
    /// than a wrapper node: parenthesization is observable in exactly two
    /// places, and both consume it immediately.
    pub(crate) parenthesized: bool,
    /// The grammar goal. `import`/`export` are declarations only in a module;
    /// in a script they are ordinary identifiers.
    pub(crate) goal: Goal,
}

impl<'s> Parser<'s> {
    pub fn new(src: &'s str, opts: ParseOptions) -> PResult<Parser<'s>> {
        let mut lx = Lexer::new(src);
        // A program starts in operand position, so a leading `/` is a regex.
        let tok = lx.next_token(true)?;
        let ctx = Ctx {
            strict: opts.force_strict || opts.goal == Goal::Module,
            await_: opts.allow_await_expr || opts.goal == Goal::Module,
            yield_: opts.allow_yield_expr,
            return_: opts.allow_return || opts.goal == Goal::FunctionBody,
            new_target: opts.allow_new_target,
            super_prop: opts.allow_super_property,
            super_call: opts.allow_super_call,
            ..Ctx::default()
        };
        let goal = opts.goal;
        Ok(Parser {
            lx,
            src,
            tok,
            prev_end: 0,
            ctx,
            scopes: ScopeStack::default(),
            cover: Cover::default(),
            opts,
            labels: Vec::new(),
            parenthesized: false,
            goal,
        })
    }

    // ---- token plumbing ----------------------------------------------------

    pub(crate) fn cur(&self) -> &Token {
        &self.tok
    }

    pub(crate) fn at_eof(&self) -> bool {
        matches!(self.tok.kind, TokenKind::Eof)
    }

    /// Advance. `regex_allowed` says whether a `/` starting the NEXT token
    /// begins a regex literal — only the parser knows, from the production it
    /// is in.
    pub(crate) fn bump(&mut self, regex_allowed: bool) -> PResult<Token> {
        self.prev_end = self.tok.span.end;
        let next = self.lx.next_token(regex_allowed)?;
        Ok(std::mem::replace(&mut self.tok, next))
    }

    /// Advance expecting an operand to follow (so `/` is a regex).
    pub(crate) fn bump_before_operand(&mut self) -> PResult<Token> {
        self.bump(true)
    }

    /// Advance expecting an operator to follow (so `/` is division).
    pub(crate) fn bump_after_operand(&mut self) -> PResult<Token> {
        self.bump(false)
    }

    /// Re-decide a `/` (or `/=`) that has already been lexed as an OPERATOR: at
    /// this point it starts a regular expression.
    ///
    /// `regex_allowed` must be supplied when a token is PRODUCED, but the `}`
    /// closing a function or class body is scanned by code shared between a
    /// DECLARATION (a statement — a regex may follow) and an EXPRESSION (an
    /// operand — a division may follow). That site cannot know which it is, so
    /// rather than thread the answer through every helper, the statement layer
    /// corrects the single offending token afterwards.
    ///
    /// Seeking to `prev_end` — the end of the `}` — rather than to the token's
    /// own start makes the lexer re-skip the intervening trivia, so
    /// `saw_newline`, which ASI reads, is recomputed rather than lost.
    pub(crate) fn reinterpret_slash_as_regex(&mut self) -> PResult<()> {
        if !(self.at(Punct::Slash) || self.at(Punct::SlashEq)) {
            return Ok(());
        }
        // Only a `}`, or the `)` of a `do … while (…)`, can end a statement and
        // still leave a `/` pending; after any other statement the expression
        // parser has already consumed the `/` as an operator. Guarding on the
        // preceding byte keeps this from ever re-reading a genuine division.
        let prev = self.prev_end as usize;
        if prev == 0 || !matches!(self.src.as_bytes()[prev - 1], b'}' | b')') {
            return Ok(());
        }
        self.lx.seek(self.prev_end);
        self.tok = self.lx.next_token(true)?;
        Ok(())
    }

    pub(crate) fn at(&self, p: Punct) -> bool {
        self.tok.is_punct(p)
    }

    pub(crate) fn at_kw(&self, k: Keyword) -> bool {
        self.tok.is_kw(k)
    }

    /// Consume `p` if present.
    pub(crate) fn eat(&mut self, p: Punct, regex_allowed: bool) -> PResult<bool> {
        if self.at(p) {
            self.bump(regex_allowed)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Consume `p` or fail.
    pub(crate) fn expect(&mut self, p: Punct, regex_allowed: bool) -> PResult<Span> {
        if !self.at(p) {
            return Err(self.err_here(format!("SyntaxError: expected {p:?}")));
        }
        Ok(self.bump(regex_allowed)?.span)
    }

    pub(crate) fn eat_kw(&mut self, k: Keyword, regex_allowed: bool) -> PResult<bool> {
        if self.at_kw(k) {
            self.bump(regex_allowed)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn err_here(&self, msg: impl Into<String>) -> SyntaxError {
        SyntaxError::new(msg, self.tok.span.start)
    }

    pub(crate) fn source(&self) -> &'s str {
        self.src
    }

    pub(crate) fn prev_end(&self) -> u32 {
        self.prev_end
    }

    /// A rewind point.
    ///
    /// Used ONLY for the bounded, single-token lookaheads the grammar genuinely
    /// needs — is `get` an accessor or a property name, is `async` a modifier or
    /// an identifier. The unbounded ambiguities are handled by the cover
    /// grammar, which never rewinds, so this can never walk back over an
    /// arbitrary amount of input.
    pub(crate) fn save(&self) -> (u32, Token) {
        (self.lx.pos(), self.tok.clone())
    }

    pub(crate) fn restore(&mut self, s: (u32, Token)) {
        self.lx.seek(s.0);
        self.tok = s.1;
    }

    /// Resume template scanning at the `}` that closes a substitution.
    ///
    /// The `}` is not a punctuator in this position — it is the start of the
    /// next template chunk — so the lexer has to be re-entered AT it rather
    /// than after it.
    pub(crate) fn resume_template(&mut self) -> PResult<Token> {
        self.lx.seek(self.tok.span.start);
        let t = self.lx.read_template_continue()?;
        // This bypasses `bump`, so it must maintain `prev_end` itself — an
        // arrow or function whose body ENDS in a template gets its span's end
        // from here, and a stale value truncated it by the tail chunk's length.
        self.prev_end = t.span.end;
        // A TemplateMiddle (`}…${`) is followed by an Expression — the spec's
        // InputElementRegExpOrTemplateTail position — so a `/` there is a regex.
        // After a TemplateTail the whole literal is an operand: division.
        let more = matches!(t.kind, TokenKind::Template { tail: false, .. });
        self.tok = self.lx.next_token(more)?;
        Ok(t)
    }

    pub(crate) fn cover_pattern_only(&mut self, e: SyntaxError) {
        if self.cover.pattern_only.is_none() {
            self.cover.pattern_only = Some(e);
        }
    }

    pub(crate) fn cover_expr_only(&mut self, e: SyntaxError) {
        if self.cover.expr_only.is_none() {
            self.cover.expr_only = Some(e);
        }
    }

    pub(crate) fn mark_parenthesized(&mut self) {
        self.parenthesized = true;
    }

    /// Consume the flag. Reading it clears it, so it can never leak onto a
    /// later, unparenthesized expression.
    pub(crate) fn take_parenthesized(&mut self) -> bool {
        std::mem::take(&mut self.parenthesized)
    }

    // ---- automatic semicolon insertion -------------------------------------

    /// Consume a statement terminator, inserting one where the spec allows.
    ///
    /// A semicolon is inserted when the offending token is on a new line, is
    /// `}`, or is EOF. That is the whole rule; the *restricted* productions
    /// (`return`/`throw`/`break`/`continue` operands, postfix `++`, `=>`) are
    /// handled where they occur, by testing `newline_before` directly.
    pub(crate) fn semicolon(&mut self) -> PResult<()> {
        if self.at(Punct::Semi) {
            self.bump_before_operand()?;
            return Ok(());
        }
        if self.at(Punct::RBrace) || self.at_eof() || self.tok.newline_before {
            return Ok(());
        }
        Err(self.err_here("SyntaxError: expected ';'"))
    }

    // ---- identifiers -------------------------------------------------------

    /// Is the current token usable as a BINDING identifier here?
    ///
    /// Three layers: always-reserved words never are; strict-mode reserved words
    /// are not in strict code; and `yield`/`await` are not where they are
    /// keywords. An escaped spelling (`await`) is an identifier, never a
    /// keyword — which is why the lexer records that.
    pub(crate) fn is_binding_ident(&self) -> bool {
        let TokenKind::Ident { kw, private, had_escape, name } = &self.tok.kind else {
            return false;
        };
        if *private {
            return false;
        }
        // A ReservedWord is matched by its STRING VALUE, so an escaped spelling
        // is still the reserved word for this purpose: `var var = 1` is a
        // SyntaxError even though `var` is not the `var` KEYWORD.
        //
        // The lexer deliberately reports `kw = Keyword::None` for an escaped
        // identifier — that is what stops `if (x)` being an `if` statement,
        // and it is asserted by a test. So the classification has to be redone
        // HERE from the name, or every check below is structurally unreachable
        // for exactly the inputs it exists to reject (which is what it was).
        let kw = if *had_escape { Keyword::classify(name) } else { *kw };
        if kw.is_always_reserved() {
            return false;
        }
        if self.ctx.strict && kw.is_strict_reserved() {
            return false;
        }
        if self.ctx.yield_ && kw == Keyword::Yield {
            return false;
        }
        if self.ctx.await_ && kw == Keyword::Await {
            return false;
        }
        true
    }

    /// Consume a binding identifier, applying the strict-mode `eval`/`arguments`
    /// rule.
    pub(crate) fn binding_ident(&mut self) -> PResult<(Name, u32)> {
        if !self.is_binding_ident() {
            return Err(self.err_here("SyntaxError: unexpected token, expected an identifier"));
        }
        let pos = self.tok.span.start;
        let tok = self.bump_after_operand()?;
        let TokenKind::Ident { name, .. } = tok.kind else { unreachable!() };
        if self.ctx.strict && (name == "eval" || name == "arguments") {
            return Err(SyntaxError::new(
                format!("SyntaxError: '{name}' cannot be used as a binding in strict mode"),
                pos,
            ));
        }
        Ok((name.into_boxed_str(), pos))
    }

    /// Consume any identifier-shaped token as a PROPERTY name, where reserved
    /// words are legal (`x.default`, `{ if: 1 }`).
    pub(crate) fn ident_name(&mut self) -> PResult<Name> {
        let TokenKind::Ident { private: false, .. } = &self.tok.kind else {
            return Err(self.err_here("SyntaxError: expected a property name"));
        };
        let tok = self.bump_after_operand()?;
        let TokenKind::Ident { name, .. } = tok.kind else { unreachable!() };
        Ok(name.into_boxed_str())
    }

    // ---- directive prologue ------------------------------------------------

    /// Parse a directive prologue, returning the directives and whether it
    /// turned strict mode on.
    ///
    /// The RAW text decides: `"use strict"` written with an escape is a normal
    /// string, not a directive. Comparing the cooked value would silently make
    /// a non-strict program strict.
    pub(crate) fn directive_prologue(&mut self) -> PResult<(Vec<Directive>, bool)> {
        let mut out = Vec::new();
        let mut strict = false;
        loop {
            let TokenKind::Str(_) = &self.tok.kind else { break };
            // A string is only a directive if the whole statement is just that
            // string — `"use strict" + x` is an expression.
            let start = self.tok.span.start as usize;
            let end = self.tok.span.end as usize;
            let raw = self.src.get(start..end).unwrap_or("");
            // Peek: the next token must end the statement.
            let save = self.tok.clone();
            let save_pos = self.lx.pos();
            let TokenKind::Str(value) = save.kind.clone() else { unreachable!() };
            self.bump_after_operand()?;
            let terminated = self.at(Punct::Semi)
                || self.at(Punct::RBrace)
                || self.at_eof()
                || self.tok.newline_before;
            if !terminated {
                // Not a directive after all; rewind so the expression parser
                // sees the string.
                self.lx.seek(save_pos);
                self.tok = save;
                break;
            }
            let inner = raw.get(1..raw.len().saturating_sub(1)).unwrap_or("");
            if inner == "use strict" {
                strict = true;
            }
            out.push(Directive { raw: inner.into(), value });
            let _ = self.eat(Punct::Semi, true)?;
        }
        Ok((out, strict))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) -> Parser<'_> {
        Parser::new(src, ParseOptions::script()).expect("lexes")
    }

    #[test]
    fn asi_inserts_only_where_the_spec_allows() {
        // A newline before the offending token allows insertion.
        let mut a = p("x\ny");
        a.bump_after_operand().unwrap();
        assert!(a.semicolon().is_ok(), "newline allows ASI");

        // `}` allows it.
        let mut b = p("x}");
        b.bump_after_operand().unwrap();
        assert!(b.semicolon().is_ok());

        // EOF allows it.
        let mut c = p("x");
        c.bump_after_operand().unwrap();
        assert!(c.semicolon().is_ok());

        // Same line, more tokens: no insertion, so this is an error.
        let mut d = p("x y");
        d.bump_after_operand().unwrap();
        assert!(d.semicolon().is_err(), "no ASI on the same line");
    }

    #[test]
    fn reserved_words_track_strictness_and_escapes() {
        // Always reserved.
        assert!(!p("class").is_binding_ident());
        assert!(!p("if").is_binding_ident());
        // Reserved only in strict mode.
        assert!(p("let").is_binding_ident(), "sloppy: `let` is a valid binding");
        let mut strict = p("let");
        strict.ctx.strict = true;
        assert!(!strict.is_binding_ident(), "strict: `let` is reserved");
        // `await` is contextual: a binding at script top level, a keyword in an
        // async function.
        assert!(p("await").is_binding_ident());
        let mut in_async = p("await");
        in_async.ctx.await_ = true;
        assert!(!in_async.is_binding_ident());
    }

    #[test]
    fn strict_mode_rejects_eval_and_arguments_as_bindings() {
        let mut sloppy = p("eval");
        assert!(sloppy.binding_ident().is_ok());
        let mut strict = p("arguments");
        strict.ctx.strict = true;
        assert!(strict.binding_ident().is_err());
    }

    #[test]
    fn directive_prologue_compares_raw_text() {
        let mut a = p(r#""use strict"; x"#);
        let (dirs, strict) = a.directive_prologue().unwrap();
        assert_eq!(dirs.len(), 1);
        assert!(strict);

        // Escaped: the cooked value is "use strict" but the RAW text is not, so
        // this does NOT enable strict mode. Comparing cooked values here is a
        // classic way to silently mis-compile a program.
        let esc = format!("\"use{}u0020strict\"; x", '\\');
        let mut b = p(&esc);
        let (dirs, strict) = b.directive_prologue().unwrap();
        assert_eq!(dirs.len(), 1, "still a directive");
        assert!(!strict, "but not THE directive");

        // Not a directive: the string is part of a larger expression.
        let mut c = p(r#""use strict" + x"#);
        let (dirs, strict) = c.directive_prologue().unwrap();
        assert!(dirs.is_empty() && !strict);
    }

    #[test]
    fn scope_stack_catches_the_duplicate_declaration_families() {
        let mut s = ScopeStack::default();
        s.push(ScopeKind::Script);
        // let x; let x;
        s.declare_lexical("x", BindKind::Let, 0, false).unwrap();
        assert!(s.declare_lexical("x", BindKind::Let, 5, false).is_err());
        // let y; var y;
        s.declare_lexical("y", BindKind::Let, 0, false).unwrap();
        assert!(s.declare_var("y", 5).is_err());
        // var z; var z; is fine.
        s.declare_var("z", 0).unwrap();
        assert!(s.declare_var("z", 5).is_ok());

        // A `var` inside a block still conflicts with an outer `let`, which is
        // what makes `let a; { var a; }` an error — and it is only caught when
        // the block pops and hoists its vars outward.
        let mut t = ScopeStack::default();
        t.push(ScopeKind::Script);
        t.declare_lexical("a", BindKind::Let, 0, false).unwrap();
        t.push(ScopeKind::Block);
        t.declare_var("a", 10).unwrap_err();
    }

    #[test]
    fn a_block_scope_does_not_leak_its_lexicals() {
        let mut s = ScopeStack::default();
        s.push(ScopeKind::Script);
        s.push(ScopeKind::Block);
        s.declare_lexical("b", BindKind::Let, 0, false).unwrap();
        s.pop().unwrap();
        // The inner `let b` is gone, so this is a fresh declaration.
        assert!(s.declare_lexical("b", BindKind::Let, 20, false).is_ok());
    }
}
