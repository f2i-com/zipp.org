//! Python: an experimental subset, compiled to the shared bytecode.
//!
//! It overrides the base language where their semantics differ by emitting
//! instructions of its own (`Instr::Py*`: arithmetic and comparison,
//! attribute, item and global access, calls with Python's binding) and uses
//! the base instructions where they agree (constants, moves, jumps, plain
//! calls, returns, handlers). Its parts:
//!
//! - `parser/`: the lexer, syntax tree and parser, a crate of its own
//!   (`zipp-pyparse`) the compiler reads.
//! - `frontend/`: the compiler (symbol table, emitter, statements,
//!   expressions) and the compiled-code cache.
//! - `runtime/`: the runtime, written in JavaScript and compiled by the
//!   JavaScript frontend: the object model, the types, the builtins, the
//!   standard library.
//! - `lib/`: the bundled Python modules, torch's among them.
//! - `vm/`: the VM's side of the `Py*` instructions (`Vm::py_step`), the
//!   runtime tables and the tensor kernels. They are modules of
//!   `crate::vm`, whose private state they work on.
//! - `jit/`: the native tier of the `Py*` instructions, modules of
//!   `crate::codegen` and `crate::vm::engine` for the same reason.
//! - `packages.rs`: packages a host adds at run time
//!   (`crate::python_packages`).
//!
//! All of it is behind the `python` Cargo feature except this table entry,
//! so a build without Python still recognises a Python source and says why
//! it cannot run it.
use super::{Language, LanguageId};
use crate::embed::ScriptState;

#[cfg(feature = "python")]
mod frontend;
#[cfg(feature = "python")]
pub(super) use frontend::{compile, compile_project, prewarm, set_runtime_cache, set_runtime_memo};

/// What compiling Python says in a build without it.
#[cfg(not(feature = "python"))]
const NOT_BUILT: &str = "Python support is not built; enable the `python` Cargo feature";

/// The Python entry of the language table.
pub(super) struct Python;

impl Language for Python {
    fn id(&self) -> LanguageId {
        LanguageId::Python
    }

    fn names(&self) -> &'static [&'static str] {
        &["python", "py"]
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py", "pyw"]
    }

    /// `python` or `python3`, optionally with a version (`python3.12`).
    fn shebang(&self, command: &str) -> bool {
        command.strip_prefix("python").is_some_and(|s| {
            s.is_empty() || (s.starts_with('3') && s.chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
    }

    fn compile(&self, source: &str) -> Result<ScriptState, String> {
        #[cfg(feature = "python")]
        {
            let mut state = ScriptState::from_program(compile(source)?);
            super::python_jit_default(&mut state);
            Ok(state)
        }
        #[cfg(not(feature = "python"))]
        {
            let _ = source;
            Err(NOT_BUILT.into())
        }
    }

    fn compile_to_text(&self, source: &str) -> Result<String, String> {
        #[cfg(feature = "python")]
        {
            Ok(format!("{:#?}", compile(source)?))
        }
        #[cfg(not(feature = "python"))]
        {
            let _ = source;
            Err(NOT_BUILT.into())
        }
    }
}
