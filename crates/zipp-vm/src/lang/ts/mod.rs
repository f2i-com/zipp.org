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
//! and namespaces as tsc emits them. A project's `tsconfig.json` can ask for
//! assigned fields and for legacy `experimentalDecorators` (with
//! `emitDecoratorMetadata`), lowered as tsc lowers them ([`legacy`]). Not
//! supported: `.tsx` (the engine has no JSX).

use crate::parse::ast::{Expr, Goal, Name, Program};
use crate::parse::parser::{PResult, ParseOptions, Parser};
use crate::parse::token::{Keyword, Punct, TokenKind};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;

mod class;
pub(crate) mod config;
mod decl;
mod elide;
mod expr;
pub(crate) mod legacy;
mod lower;
mod types;

#[cfg(test)]
mod tests;

use super::{Language, LanguageId};
use crate::embed::ScriptState;
pub(crate) use config::TsConfig;

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
        let ast =
            parse_script(source, true, TsConfig::default()).map_err(|f| f.located(source, None))?;
        let program = crate::compile::compile_main_program(&ast, source)?;
        Ok(ScriptState::from_program(program))
    }

    fn compile_to_text(&self, source: &str) -> Result<String, String> {
        let ast = parse_auto(source, TsConfig::default()).map_err(|f| f.located(source, None))?;
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
    /// `useDefineForClassFields` (see `config.rs`).
    pub(crate) define_fields: bool,
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
    /// Constant members of the enums read so far, for the initializers of
    /// later members and enums (`B = A << 1`, `X = Other.Y`).
    pub(crate) enums: FxHashMap<Name, Vec<(Name, lower::Const)>>,
    /// The `const enum`s among them, whose constant members are inlined
    /// where they are read (`Kind.A` is `0`), as tsc inlines them.
    pub(crate) const_enums: FxHashSet<Name>,
    /// Where a `(` or `<` was tried as an arrow function and was not one, so
    /// it is not tried again (see `Parser::ts_arrow`).
    pub(crate) not_arrow: FxHashSet<u32>,
    /// `experimentalDecorators` and `emitDecoratorMetadata` (see
    /// `legacy.rs`); everything below is theirs.
    pub(crate) legacy_decorators: bool,
    pub(crate) decorator_metadata: bool,
    /// The parameter list just read: its parameters' decorators, `(index,
    /// decorators)`, and with metadata their types. As with `param_props`,
    /// the function body takes them, and leaves them as `fn_param_decs` and
    /// `fn_sig` (with its return type) for the class member reading it.
    pub(crate) param_decs: Vec<(u32, Vec<Expr>)>,
    pub(crate) param_types: Vec<legacy::Ser>,
    pub(crate) fn_param_decs: Vec<(u32, Vec<Expr>)>,
    pub(crate) fn_sig: Option<(Vec<legacy::Ser>, Option<legacy::Ser>)>,
    /// The type of the class field just read, with metadata.
    pub(crate) field_type: Option<legacy::Ser>,
    /// The class declarations being read, innermost last, and the one just
    /// finished, for its declaration to lower.
    pub(crate) legacy_classes: Vec<legacy::LegacyClass>,
    pub(crate) legacy_last: Option<legacy::LegacyClass>,
    /// The statement about to be read is an `export`'s declaration.
    pub(crate) legacy_export: bool,
    /// A class was lowered: the file needs the helpers.
    pub(crate) legacy_used: bool,
    /// Temporaries named so far (computed member names).
    pub(crate) legacy_temps: u32,
    /// The type aliases read so far, by the source range of their type.
    pub(crate) type_aliases: FxHashMap<Name, (u32, u32)>,
    /// Each import binding's module and the name it imports.
    pub(crate) import_from: FxHashMap<Name, (crate::parse::token::StrVal, String)>,
    /// The namespace imports metadata reads imported types through.
    pub(crate) meta_ns: Vec<(crate::parse::token::StrVal, Name)>,
}

/// The address an interned identifier is known by: the lexer gives every
/// spelling of a name one allocation, so it identifies the name.
#[inline]
fn name_key(name: &Name) -> usize {
    std::sync::Arc::as_ptr(name) as *const u8 as usize
}

impl<'s> Parser<'s> {
    /// A value reference to `name` was read: an import it names is used,
    /// unless a binding of the same name nearer the reference shadows it.
    #[inline]
    pub(crate) fn ts_note_ref(&mut self, name: &Name) {
        if self.ts_state.imports.is_empty() {
            return;
        }
        let key = name_key(name);
        if self.ts_state.imports.get(&key) == Some(&false) && !self.scopes.shadowed(name) {
            self.ts_state.imports.insert(key, true);
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
                self.split_first()
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
    config: TsConfig,
) -> Result<Program, crate::front::ParseFailure> {
    let opts = ParseOptions {
        goal,
        allow_return,
        typescript: true,
        ts_define_fields: config.define_fields,
        ts_legacy_decorators: config.legacy_decorators,
        ts_decorator_metadata: config.decorator_metadata,
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
    config: TsConfig,
) -> Result<Program, crate::front::ParseFailure> {
    parse_goal(src, Goal::Script, allow_return, config)
}

/// [`parse_script`] made strict whatever its own prologue says (an
/// embedder's preamble put before the guest, see `embed`), with tsc's
/// default settings.
pub(crate) fn parse_script_strict(
    src: &str,
    allow_return: bool,
    force_strict: bool,
) -> Result<Program, crate::front::ParseFailure> {
    let opts = ParseOptions {
        allow_return,
        force_strict,
        typescript: true,
        ..ParseOptions::script()
    };
    crate::parse::stmt::parse(src, opts).map_err(crate::front::failure)
}

/// A TypeScript module.
pub(crate) fn parse_module(
    src: &str,
    config: TsConfig,
) -> Result<Program, crate::front::ParseFailure> {
    parse_goal(src, Goal::Module, false, config)
}

/// A TypeScript file whose goal is decided by what it holds: a module when
/// it imports or exports ([`looks_like_module`]), else a script, and the
/// other goal when the first does not parse.
pub(crate) fn parse_auto(
    src: &str,
    config: TsConfig,
) -> Result<Program, crate::front::ParseFailure> {
    let first = if looks_like_module(src) {
        Goal::Module
    } else {
        Goal::Script
    };
    match parse_goal(src, first, true, config) {
        Ok(p) => Ok(p),
        Err(e) => {
            let other = if first == Goal::Module {
                Goal::Script
            } else {
                Goal::Module
            };
            parse_goal(src, other, true, config).map_err(|_| e)
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
/// A TypeScript module's syntax error names the file, as an entry's does.
pub(crate) fn parse_module_file(path: &Path, code: &str) -> Result<Program, String> {
    if is_ts_path(path) {
        parse_module(code, config::config_for(path)).map_err(|f| {
            #[cfg(not(feature = "wasm-no-fs-loader"))]
            let url = std::path::absolute(path).map(|p| crate::vm::file_url(&p)).ok();
            #[cfg(feature = "wasm-no-fs-loader")]
            let url: Option<String> = None;
            f.located(code, url.as_deref())
        })
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
    let config = config::config_for(path);
    if !module {
        match parse_script(&src, true, config) {
            Ok(ast) => return run_script(&src, ast, path, &name),
            // A script that does not parse may be a module the scan missed.
            Err(e) => {
                if parse_module(&src, config).is_err() {
                    return Err(e.located(&src, Some(&name)));
                }
            }
        }
    }
    run_module(path)
}

/// Run the TypeScript module at `path` through the module loader, which
/// parses and compiles it (and each `.ts` file it imports) once. Unlike
/// `crate::run_module_file` there is no separate pass over the entry first
/// to report its syntax errors: the loader's report names the file and
/// position the same way, and the pass cost a second parse and compile of
/// every entry.
#[cfg(not(feature = "wasm-no-fs-loader"))]
fn run_module(path: &Path) -> Result<crate::Outcome, String> {
    let host = crate::compile::compile_main_program(&crate::front::parse_auto("")?, "")?;
    let mut vm = crate::vm::Vm::new(&host);
    vm.set_module_base_dir(path.parent().map(|p| p.to_path_buf()));
    let result = vm.run().and_then(|_| vm.run_module_entry(path));
    if !matches!(&result, Err(thrown) if thrown.0.contains("top-level await is not supported")) {
        vm.report_unhandled_errors();
    }
    let outcome = crate::Outcome {
        output: std::mem::take(&mut vm.output),
        errput: std::mem::take(&mut vm.errput),
        error: result.err().map(|thrown| thrown.0),
    };
    // The process exits next; see `crate::run_then_exit`.
    std::mem::forget(vm);
    std::mem::forget(host);
    Ok(outcome)
}

#[cfg(feature = "wasm-no-fs-loader")]
fn run_module(_path: &Path) -> Result<crate::Outcome, String> {
    Err("filesystem module loader is disabled in this build".into())
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
