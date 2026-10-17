//! JavaScript, the base language. Its semantics are the base instruction
//! set's: every instruction of `Instr` outside a language's own section
//! means what ECMAScript says, and the VM, the GC and the native tiers are
//! built around them.
//!
//! The frontend's three parts are crate-level modules (`crate::parse`,
//! `crate::compile`, `crate::front`, declared in `lib.rs`), because the core
//! names them throughout: `eval`, `new Function`, the module loader and the
//! embedding API all compile JavaScript at run time.
use super::{Language, LanguageId};
use crate::embed::{CompileOptions, ScriptGoal, ScriptState};

/// The JavaScript entry of the language table.
pub(super) struct JavaScript;

impl Language for JavaScript {
    fn id(&self) -> LanguageId {
        LanguageId::JavaScript
    }

    fn names(&self) -> &'static [&'static str] {
        &["javascript", "js"]
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["js", "cjs", "mjs"]
    }

    fn shebang(&self, command: &str) -> bool {
        matches!(command, "node" | "nodejs" | "qjs")
    }

    fn compile(&self, source: &str) -> Result<ScriptState, String> {
        compile(source, ScriptGoal::Inherit)
    }

    fn compile_to_text(&self, source: &str) -> Result<String, String> {
        compile_to_text(source, ScriptGoal::Inherit)
    }
}

/// A script under `goal`.
pub(super) fn compile(source: &str, goal: ScriptGoal) -> Result<ScriptState, String> {
    crate::embed::compile_script_with_options(source, &CompileOptions { goal })
}

/// The compiled script under `goal`, as text.
pub(super) fn compile_to_text(source: &str, goal: ScriptGoal) -> Result<String, String> {
    // Do not ignore the per-call script goal when inspecting bytecode.
    let allow_return = match goal {
        ScriptGoal::Pure => false,
        ScriptGoal::Compat => true,
        ScriptGoal::Inherit => !crate::front::pure_script_goal(),
    };
    let ast = crate::front::parse_script_with(source, allow_return, false)?;
    let program = crate::compile::compile_main_program(&ast, source)?;
    Ok(format!("{program:#?}"))
}
