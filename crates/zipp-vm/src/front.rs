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
use crate::parse::stmt::{parse, parse_exact};

fn err_str(e: crate::parse::parser::SyntaxError) -> String {
    format!("SyntaxError: {}", e.msg.strip_prefix("SyntaxError: ").unwrap_or(&e.msg))
}

/// Conformance mode: parse a top-level script under the PURE Script goal.
///
/// Off by default, because the two accommodations it disables are load-bearing
/// for real code: node wraps a `.js` file in a CommonJS function (so top-level
/// `return` is legal and packages ship it), and `zipp js` on an ESM-shaped
/// `.js` file falls back to the Module goal rather than failing. test262 tests
/// the grammar as specified — `ScriptBody : StatementList[~Return]`, and
/// `import`/`export` reachable only from the Module goal — so the runner turns
/// this on. Process-wide rather than a parameter so that `$262.evalScript` and
/// agent sources inherit it without threading a flag through every call site.
static PURE_SCRIPT_GOAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_pure_script_goal(on: bool) {
    PURE_SCRIPT_GOAL.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn pure_script_goal() -> bool {
    PURE_SCRIPT_GOAL.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn parse_script(src: &str) -> Result<Program, String> {
    let allow_return = !pure_script_goal();
    parse(src, ParseOptions { allow_return, ..ParseOptions::script() }).map_err(err_str)
}

pub(crate) fn parse_module(src: &str) -> Result<Program, String> {
    parse(src, ParseOptions::module()).map_err(err_str)
}

pub(crate) fn parse_auto(src: &str) -> Result<Program, String> {
    match parse_script(src) {
        Ok(p) => Ok(p),
        // Under the pure Script goal there is no second guess: a script that
        // fails to parse is a SyntaxError, not a module. This is what makes
        // `export default null;` at top level the early error the spec wants.
        Err(script_err) if pure_script_goal() => Err(script_err),
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

/// `exact` is the WTF-8 original of `src` when the code STRING held a lone
/// surrogate — `src` is then the lossy view of it, and only a regex literal
/// can carry such a code unit through to runtime (see
/// [`crate::parse::lexer::Lexer::set_exact_src`]).
pub(crate) fn parse_eval(
    src: &str,
    exact: Option<&[u8]>,
    flags: EvalFlags,
) -> Result<Program, String> {
    parse_exact(
        src,
        exact,
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
