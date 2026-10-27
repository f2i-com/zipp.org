//! TypeScript, read straight into the engine.
//!
//! There is no transpile step and no JavaScript text. TypeScript is a dialect
//! of the JavaScript grammar here: the JavaScript parser (`lang/js/parse`)
//! reads a `.ts` file with [`ParseOptions::typescript`] on, and at each place
//! the two grammars part it calls into this folder. What comes out is the
//! JavaScript syntax tree the same program written without types would give,
//! so the compiler, the VM and the JIT run TypeScript exactly as they run
//! JavaScript, at the same speed.
//!
//! - **Types are read and dropped as they are met** ([`types`]): annotations,
//!   type parameters and arguments, `as`/`satisfies`/`!`, `interface`, `type`,
//!   `declare`, overload signatures and abstract members. The type grammar is
//!   checked (a malformed type is a SyntaxError) but builds nothing.
//! - **The constructs with a run-time meaning are lowered to the tree tsc's
//!   output would parse to** ([`lower`]): `enum` (reverse-mapped, members
//!   constant-folded), `namespace` (merging IIFEs), constructor parameter
//!   properties, `import x = …` and `export = …`.
//! - **Imports used only as types are elided** ([`elide`]), as tsc does:
//!   `import { Props } from "./types"` must not ask `./types` for a binding
//!   that does not exist at run time.
//!
//! Semantics follow tsc with `target: ES2022` and its defaults: class fields
//! are defined (`useDefineForClassFields`), standard decorators, and enums
//! and namespaces as tsc emits them. Not supported: `.tsx` (the engine has no
//! JSX), and legacy `experimentalDecorators` semantics.

use crate::parse::ast::{Goal, Name, Program};
use crate::parse::parser::{PResult, ParseOptions, Parser};
use crate::parse::token::{Keyword, Punct, TokenKind};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;

mod class;
mod decl;
mod elide;
mod expr;
mod lower;
mod types;

#[cfg(test)]
mod tests;

use super::{Language, LanguageId};
use crate::embed::ScriptState;

/// The TypeScript entry of the language table.
pub(super) struct TypeScript;

impl Language for TypeScript {
    fn id(&self) -> LanguageId {
        LanguageId::TypeScript
    }

    fn names(&self) -> &'static [&'static str] {
        &["typescript", "ts"]
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "mts", "cts"]
    }

    fn shebang(&self, command: &str) -> bool {
        matches!(command, "ts-node" | "tsx" | "ts-node-esm")
    }

    fn compile(&self, source: &str) -> Result<ScriptState, String> {
        let ast = parse_script(source, true).map_err(|f| f.located(source, None))?;
        let program = crate::compile::compile_main_program(&ast, source)?;
        Ok(ScriptState::from_program(program))
    }

    fn compile_to_text(&self, source: &str) -> Result<String, String> {
        let ast = parse_auto(source).map_err(|f| f.located(source, None))?;
        let program = if ast.goal == Goal::Module {
            crate::compile::compile_main_module(&ast, source)?
        } else {
            crate::compile::compile_main_program(&ast, source)?
        };
        Ok(format!("{program:#?}"))
    }
}

/// What the TypeScript grammar keeps while it reads, on the parser
/// ([`Parser::ts_state`]). Empty and unused for JavaScript.
#[derive(Default)]
pub(crate) struct TsState {
    /// The function body most recently read was a signature with none: an
    /// overload, an abstract method or an ambient function. Read by the
    /// declaration that asked for it, straight after, which then drops it.
    pub(crate) bodiless: bool,
    /// A class member's modifiers included `static` (read by
    /// `parse_class_member_inner`, which would otherwise look for it).
    pub(crate) member_static: bool,
    /// The assignment expression about to be read is a conditional's true
    /// branch, where `(a): T => b` gives way to `(a) : …` when no second `:`
    /// follows (tsc's `allowReturnTypeInArrowFunction`).
    pub(crate) no_ret_arrow: bool,
    /// Conditional types are not allowed here: the check type of one is
    /// being read (`T extends infer U extends X ? …`).
    pub(crate) no_cond: bool,
    /// Constructor parameter properties (`constructor(private x: T)`) of
    /// the parameter list just read.
    pub(crate) param_props: Vec<Name>,
    /// The parameter properties of the function body just read, for the
    /// class whose constructor it is to declare as fields.
    pub(crate) ctor_props: Vec<Name>,
    /// The module's import bindings, by their interned name's address, and
    /// whether a value reference to each has been read.
    pub(crate) imports: FxHashMap<usize, bool>,
    /// An import came after a statement that may have referred to it, so
    /// references are counted from the finished tree instead.
    pub(crate) late_import: bool,
    /// Names declared at the top level only as types (`interface`, `type`),
    /// so `export { Props }` exports nothing.
    pub(crate) type_names: FxHashSet<Name>,
    /// Constant members of the enums read so far, for the initializers of
    /// later members and enums (`B = A << 1`, `X = Other.Y`).
    pub(crate) enums: FxHashMap<Name, Vec<(Name, lower::Const)>>,
    /// Where a `(` or `<` was tried as an arrow function and was not one, so
    /// it is not tried again (see `Parser::ts_arrow`).
    pub(crate) not_arrow: FxHashSet<u32>,
}

/// The address an interned identifier is known by: the lexer gives every
/// spelling of a name one allocation, so it identifies the name.
#[inline]
fn name_key(name: &Name) -> usize {
    std::sync::Arc::as_ptr(name) as *const u8 as usize
}

impl<'s> Parser<'s> {
    /// A value reference to `name` was read: an import it names is used.
    #[inline]
    pub(crate) fn ts_note_ref(&mut self, name: &Name) {
        if !self.ts_state.imports.is_empty() {
            if let Some(used) = self.ts_state.imports.get_mut(&name_key(name)) {
                *used = true;
            }
        }
    }

    /// The current token is the unescaped identifier `word` (a contextual
    /// keyword of TypeScript's: `type`, `namespace`, `readonly`, …).
    #[inline]
    pub(crate) fn ts_at(&self, word: &str) -> bool {
        matches!(&self.cur().kind,
            TokenKind::Ident { name, had_escape: false, private: false, .. } if &**name == word)
    }

    /// The current token's spelling, if it is an unescaped identifier or
    /// keyword.
    #[inline]
    pub(crate) fn ts_word(&self) -> Option<&str> {
        match &self.cur().kind {
            TokenKind::Ident {
                name,
                had_escape: false,
                private: false,
                ..
            } => Some(name),
            _ => None,
        }
    }

    /// [`Self::ts_word`], owned, for a `match` whose arms read on.
    pub(crate) fn ts_word_name(&self) -> Option<Name> {
        match &self.cur().kind {
            TokenKind::Ident {
                name,
                had_escape: false,
                private: false,
                ..
            } => Some(name.clone()),
            _ => None,
        }
    }

    /// Look at the token after the current one without moving: `f` sees the
    /// parser one token on, and everything is put back after.
    pub(crate) fn ts_peek<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> PResult<T> {
        let saved = self.peek_state();
        self.bump(false)?;
        let r = f(self);
        self.peek_restore(saved);
        Ok(r)
    }

    /// The next token is on the current line and is an identifier (or a
    /// keyword): what makes `type`, `interface`, `namespace` and the other
    /// contextual words declarations rather than names.
    pub(crate) fn ts_next_is_ident_on_line(&mut self) -> PResult<bool> {
        self.ts_peek(|p| {
            !p.cur().newline_before
                && matches!(p.cur().kind, TokenKind::Ident { private: false, .. })
        })
    }

    /// Consume a `>` that closes a type parameter or argument list, taking
    /// the first `>` of `>>`, `>=` and the like.
    pub(crate) fn ts_expect_gt(&mut self, regex_after: bool) -> PResult<()> {
        match self.cur().kind.as_punct() {
            Some(Punct::Gt) => {
                self.bump(regex_after)?;
                Ok(())
            }
            Some(Punct::Shr | Punct::UShr | Punct::GtEq | Punct::ShrEq | Punct::UShrEq) => {
                self.split_gt()
            }
            _ => Err(self.err_here("SyntaxError: expected '>'")),
        }
    }

    /// Run `f` with conditional types allowed (`allow`) or not, as tsc's
    /// `allowConditionalTypesAnd` / `disallowConditionalTypesAnd` do.
    pub(crate) fn ts_with_cond<T>(
        &mut self,
        allow: bool,
        f: impl FnOnce(&mut Self) -> PResult<T>,
    ) -> PResult<T> {
        let saved = std::mem::replace(&mut self.ts_state.no_cond, !allow);
        let r = f(self);
        self.ts_state.no_cond = saved;
        r
    }

    /// Whether the current token is an identifier usable as a name in a type
    /// or a declaration (not a reserved word other than the ones types use).
    pub(crate) fn ts_at_name(&self) -> bool {
        match &self.cur().kind {
            TokenKind::Ident {
                kw, private: false, ..
            } => {
                !kw.is_always_reserved()
                    || matches!(kw, Keyword::Void | Keyword::Null | Keyword::This)
            }
            _ => false,
        }
    }
}

/// Parse TypeScript `src` under `goal`.
fn parse_goal(
    src: &str,
    goal: Goal,
    allow_return: bool,
) -> Result<Program, crate::front::ParseFailure> {
    let opts = ParseOptions {
        goal,
        allow_return,
        typescript: true,
        ..if goal == Goal::Module {
            ParseOptions::module()
        } else {
            ParseOptions::script()
        }
    };
    crate::parse::stmt::parse(src, opts).map_err(crate::front::failure)
}

/// A TypeScript script (no `import`/`export`), CommonJS-shaped like the
/// engine's `.js` scripts when `allow_return`.
pub(crate) fn parse_script(
    src: &str,
    allow_return: bool,
) -> Result<Program, crate::front::ParseFailure> {
    parse_goal(src, Goal::Script, allow_return)
}

/// A TypeScript module.
pub(crate) fn parse_module(src: &str) -> Result<Program, crate::front::ParseFailure> {
    parse_goal(src, Goal::Module, false)
}

/// A TypeScript file whose goal is decided by what it holds: a module when
/// it imports or exports ([`looks_like_module`]), else a script, and the
/// other goal when the first does not parse.
pub(crate) fn parse_auto(src: &str) -> Result<Program, crate::front::ParseFailure> {
    let first = if looks_like_module(src) {
        Goal::Module
    } else {
        Goal::Script
    };
    match parse_goal(src, first, true) {
        Ok(p) => Ok(p),
        Err(e) => {
            let other = if first == Goal::Module {
                Goal::Script
            } else {
                Goal::Module
            };
            parse_goal(src, other, true).map_err(|_| e)
        }
    }
}

/// Whether `src` has an `import` or `export` declaration: a line that starts
/// with the word, followed by what only a declaration puts there. A guess
/// from the text, so it costs a scan rather than a parse; `parse_auto` tries
/// the other goal when it is wrong.
pub(crate) fn looks_like_module(src: &str) -> bool {
    let b = src.as_bytes();
    for at in memchr::memmem::find_iter(b, b"port") {
        if at < 2 {
            continue;
        }
        let word = at - 2;
        let is_import = &b[word..at] == b"im";
        let is_export = &b[word..at] == b"ex";
        if !is_import && !is_export {
            continue;
        }
        // Only blanks between the line's start and the word.
        let line_start = b[..word]
            .iter()
            .rposition(|&c| c == b'\n' || c == b'\r')
            .map_or(0, |i| i + 1);
        if !b[line_start..word].iter().all(|&c| c == b' ' || c == b'\t') {
            continue;
        }
        let next = b.get(at + 4).copied().unwrap_or(b'\n');
        if is_export {
            if matches!(next, b' ' | b'\t' | b'{' | b'*' | b'\n' | b'\r') {
                return true;
            }
        } else if matches!(next, b' ' | b'\t' | b'{' | b'*' | b'"' | b'\'') {
            return true;
        } else if next == b'.' && b[at + 5..].starts_with(b"meta") {
            return true;
        }
    }
    false
}

/// Whether `path` names a TypeScript file.
pub(crate) fn is_ts_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "ts" | "mts" | "cts"))
}

/// The module at `path`, read as TypeScript or JavaScript by its extension.
pub(crate) fn parse_module_file(path: &Path, code: &str) -> Result<Program, String> {
    if is_ts_path(path) {
        parse_module(code).map_err(|f| f.located(code, None))
    } else {
        crate::front::parse_module(code)
    }
}

/// A file a TypeScript import may mean when `raw` itself does not exist:
/// `./x` is `./x.ts` or `./x/index.ts`, and `./x.js` is `./x.ts` (tsc's
/// module resolution, which lets ESM TypeScript name its imports by the
/// JavaScript file they become).
pub(crate) fn resolve_candidates(raw: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let ext = raw.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "js" | "jsx" => out.push(raw.with_extension("ts")),
        "mjs" => out.push(raw.with_extension("mts")),
        "cjs" => out.push(raw.with_extension("cts")),
        "ts" | "mts" | "cts" => {}
        _ => {
            let s = raw.as_os_str();
            for add in [".ts", ".mts", ".js", ".mjs"] {
                let mut p = s.to_owned();
                p.push(add);
                out.push(p.into());
            }
            for index in ["index.ts", "index.js"] {
                out.push(raw.join(index));
            }
        }
    }
    out
}

/// Run the TypeScript file at `path`: a module (it imports or exports, or is
/// `.mts`) through the module loader, so the `.ts` files it imports load as
/// TypeScript too; otherwise as a script, like `zipp js`.
pub fn run_file(path: &Path) -> Result<crate::Outcome, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let module = match ext {
        "mts" => true,
        "cts" => false,
        _ => looks_like_module(&src),
    };
    let name = std::path::absolute(path).unwrap_or_else(|_| path.into());
    let name = name.to_string_lossy();
    if !module {
        match parse_script(&src, true) {
            Ok(ast) => return run_script(&src, ast, path, &name),
            // A script that does not parse may be a module the scan missed.
            Err(e) => {
                if parse_module(&src).is_err() {
                    return Err(e.located(&src, Some(&name)));
                }
            }
        }
    }
    crate::run_module_file(path, None)
}

fn run_script(src: &str, ast: Program, path: &Path, name: &str) -> Result<crate::Outcome, String> {
    let base_dir = path.parent().map(|p| {
        if p.as_os_str().is_empty() {
            Path::new(".").to_path_buf()
        } else {
            p.to_path_buf()
        }
    });
    let (program, lazy) = crate::compile_main_for_run(&ast, src)?;
    let outcome = crate::run_program(&program, lazy, base_dir, false, true, Some(name));
    // The process exits next; see `crate::run_then_exit`.
    std::mem::forget(program);
    std::mem::forget(ast);
    Ok(outcome)
}
