//! The front end: source text → [`crate::parse::ast::Program`], via zipp's own
//! parser.
//!
//! Every place the engine turns text into a tree goes through one of these
//! four entry points, each named for the GRAMMAR it applies rather than the
//! file extension it usually serves:
//!
//! - [`parse_script`] — the CommonJS-shaped script: sloppy unless directed,
//!   top-level `return` legal (node wraps a file in a function, and real
//!   packages rely on it).
//! - [`parse_module`] — the Module goal: strict, top-level `await`,
//!   `import`/`export`.
//! - [`parse_auto`] — script first, module on failure. Replaces oxc's
//!   `SourceType::unambiguous()`: a file valid under both goals is a script,
//!   which is also what `unambiguous` chose.
//! - [`parse_eval`] — the true Script goal plus the context an eval call site
//!   supplies (strictness, `new.target`/`super` validity). Top-level `return`
//!   is a SyntaxError here, as the spec requires.

use crate::parse::ast::{Goal, Program};
use crate::parse::parser::ParseOptions;
use crate::parse::stmt::parse;

fn err_str(e: crate::parse::parser::SyntaxError) -> String {
    format!("SyntaxError: {}", e.msg.strip_prefix("SyntaxError: ").unwrap_or(&e.msg))
}

pub(crate) fn parse_script(src: &str) -> Result<Program, String> {
    parse(src, ParseOptions { allow_return: true, ..ParseOptions::script() }).map_err(err_str)
}

pub(crate) fn parse_module(src: &str) -> Result<Program, String> {
    parse(src, ParseOptions::module()).map_err(err_str)
}

pub(crate) fn parse_auto(src: &str) -> Result<Program, String> {
    match parse_script(src) {
        Ok(p) => Ok(p),
        Err(script_err) => match parse_module(src) {
            Ok(p) => Ok(p),
            // The script error, not the module one: a file that is neither is
            // overwhelmingly a broken script, and the script diagnostic points
            // at the right place.
            Err(_) => Err(script_err),
        },
    }
}

/// What an eval call site knows that the source text cannot say.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EvalFlags {
    /// The caller is strict code, which makes the eval'd program strict.
    pub force_strict: bool,
    /// `new.target` is legal (the eval is inside a function).
    pub allow_new_target: bool,
    /// `super.x` / `super()` are legal (the eval is inside a class member).
    /// Parsing is deliberately permissive here — the COMPILER enforces the
    /// exact method-kind rules, exactly as it did before the swap, so the
    /// parser only needs to not over-reject.
    pub allow_super: bool,
}

pub(crate) fn parse_eval(src: &str, flags: EvalFlags) -> Result<Program, String> {
    parse(
        src,
        ParseOptions {
            goal: Goal::EvalScript,
            force_strict: flags.force_strict,
            allow_return: false,
            allow_new_target: flags.allow_new_target,
            allow_super_property: flags.allow_super,
            allow_super_call: flags.allow_super,
            allow_await_expr: false,
            allow_yield_expr: false,
        },
    )
    .map_err(err_str)
}
