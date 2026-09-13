//! Explicit, additive language selection. Existing JavaScript APIs are unchanged.
//!
//! Python is an experimental, feature-gated subset, not CPython compatibility.
//! Detection is a caller-side policy and never a compiler fallback.
use crate::embed::{CompileOptions, ScriptGoal, ScriptState};
use std::path::Path;
use std::str::FromStr;

#[cfg(feature = "python")]
mod python;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageId {
    JavaScript,
    Python,
}
impl FromStr for LanguageId {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "js" | "javascript" => Ok(Self::JavaScript),
            "py" | "python" => Ok(Self::Python),
            _ => Err(format!(
                "unknown language {value:?}; expected javascript or python"
            )),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonMode {
    Module,
}
#[derive(Debug, Clone, Copy)]
pub enum Frontend {
    JavaScript { goal: ScriptGoal },
    Python { mode: PythonMode },
}
impl Frontend {
    pub fn language(self) -> LanguageId {
        match self {
            Self::JavaScript { .. } => LanguageId::JavaScript,
            Self::Python { .. } => LanguageId::Python,
        }
    }
}
/// Language identity belongs to the compiled-source handle. The bytecode ABI
/// remains unchanged in this prototype; per-FuncProto metadata is deferred.
pub struct CompiledSource {
    language: LanguageId,
    state: ScriptState,
}
impl CompiledSource {
    pub fn language(&self) -> LanguageId {
        self.language
    }
    pub fn state_mut(&mut self) -> &mut ScriptState {
        &mut self.state
    }
    pub fn into_state(self) -> ScriptState {
        self.state
    }
}
pub fn compile_source(source: &str, frontend: Frontend) -> Result<CompiledSource, String> {
    let language = frontend.language();
    let state = match frontend {
        Frontend::JavaScript { goal } => {
            crate::embed::compile_script_with_options(source, &CompileOptions { goal })?
        }
        Frontend::Python {
            mode: PythonMode::Module,
        } => {
            #[cfg(feature = "python")]
            {
                let program = python::compile(source)?;
                let mut state = ScriptState::from_program(program);
                // The Python emitter has not been validated against the JIT.
                // This affects this state only, never JavaScript engines.
                state.disable_vm_jit();
                state
            }
            #[cfg(not(feature = "python"))]
            {
                return Err(
                    "Python support is not built; enable the `python` Cargo feature".into(),
                );
            }
        }
    };
    Ok(CompiledSource { language, state })
}
/// Compile a multi-module Python project: `modules` pairs each module name
/// (the `.py` file's stem) with its source, and `entry` names the module whose
/// top level runs. Modules import one another by name; nothing outside the
/// project (and the built-in `ui` module) can be imported.
pub fn compile_python_project(
    entry: &str,
    modules: &[(String, String)],
) -> Result<CompiledSource, String> {
    compile_python_project_hosted(entry, modules, false)
}
/// `compile_python_project` for an embedder that drains the program's host
/// requests (`__zipp_py_take_host`) and answers them (`__zipp_py_deliver`):
/// with `hosted`, a request such as a GPU graph waits for the host instead
/// of being settled by the runtime's own reference implementation.
pub fn compile_python_project_hosted(
    entry: &str,
    modules: &[(String, String)],
    hosted: bool,
) -> Result<CompiledSource, String> {
    compile_python_program(entry, modules, &[], &[], hosted)
}
/// The general form: `modules` are candidate `.py` files by dotted module
/// name (only those reachable from `entry` through imports are compiled),
/// `files` is the program's virtual filesystem (root-relative paths and
/// bytes; 8 MiB per file, 64 MiB in total), and `argv` becomes
/// `sys.argv[1:]`.
pub fn compile_python_program(
    entry: &str,
    modules: &[(String, String)],
    files: &[(String, Vec<u8>)],
    argv: &[String],
    hosted: bool,
) -> Result<CompiledSource, String> {
    #[cfg(feature = "python")]
    {
        let program = python::compile_project(entry, modules, files, argv, hosted)?;
        let mut state = ScriptState::from_program(program);
        state.disable_vm_jit();
        Ok(CompiledSource {
            language: LanguageId::Python,
            state,
        })
    }
    #[cfg(not(feature = "python"))]
    {
        let _ = (entry, modules, files, argv, hosted);
        Err("Python support is not built; enable the `python` Cargo feature".into())
    }
}
/// Developer-only bytecode inspection, with no guest execution.
pub fn compile_source_to_text(source: &str, frontend: Frontend) -> Result<String, String> {
    match frontend {
        Frontend::JavaScript { goal } => {
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
        Frontend::Python { .. } => {
            #[cfg(feature = "python")]
            {
                Ok(format!("{:#?}", python::compile(source)?))
            }
            #[cfg(not(feature = "python"))]
            {
                Err("Python support is not built; enable the `python` Cargo feature".into())
            }
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionReason {
    Explicit,
    Extension,
    Shebang,
    Directive,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectedLanguage {
    pub language: LanguageId,
    pub reason: DetectionReason,
}
/// Explicit option > file extension > recognized shebang > initial directive.
/// No substring heuristics, parse-and-retry, or execution is performed.
pub fn detect_language(
    explicit: Option<LanguageId>,
    filename: Option<&Path>,
    source: &str,
) -> Result<DetectedLanguage, String> {
    let found = |language, reason| Ok(DetectedLanguage { language, reason });
    if let Some(language) = explicit {
        return found(language, DetectionReason::Explicit);
    }
    if let Some(ext) = filename.and_then(Path::extension).and_then(|s| s.to_str()) {
        let lang = match ext.to_ascii_lowercase().as_str() {
            "js" | "cjs" | "mjs" => Some(LanguageId::JavaScript),
            "py" | "pyw" => Some(LanguageId::Python),
            _ => None,
        };
        if let Some(lang) = lang {
            return found(lang, DetectionReason::Extension);
        }
    }
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    if let Some(line) = source.lines().next().and_then(|s| s.strip_prefix("#!")) {
        let words: Vec<_> = line.split_ascii_whitespace().collect();
        let base = |s: &str| s.rsplit('/').next().unwrap_or("").to_string();
        let command = match words.first() {
            Some(first) if base(first) == "env" => {
                // Deliberately accept only ordinary env COMMAND and env -S COMMAND.
                // Unknown env options are not guessed around.
                let index = if words.get(1) == Some(&"-S") { 2 } else { 1 };
                words.get(index).map(|s| base(s))
            }
            Some(first) => Some(base(first)),
            None => None,
        };
        if let Some(command) = command {
            let suffix = command.strip_prefix("python");
            let python = suffix.is_some_and(|s| {
                s.is_empty()
                    || (s.starts_with('3') && s.chars().all(|c| c.is_ascii_digit() || c == '.'))
            });
            if python {
                return found(LanguageId::Python, DetectionReason::Shebang);
            }
            if matches!(command.as_str(), "node" | "nodejs" | "qjs") {
                return found(LanguageId::JavaScript, DetectionReason::Shebang);
            }
        }
    }
    // Only a leading comment region is eligible. A string literal containing a
    // directive must not affect language selection.
    for line in source.lines().take(8) {
        let line = line.trim();
        if line.is_empty() || line.starts_with("#!") {
            continue;
        }
        let comment = line.strip_prefix('#').or_else(|| line.strip_prefix("//"));
        let Some(comment) = comment else {
            break;
        };
        if let Some(value) = comment.trim().strip_prefix("zipp:language=") {
            return found(value.parse()?, DetectionReason::Directive);
        }
    }
    Err("ambiguous source language; specify --lang=javascript or --lang=python".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ambiguous_is_not_javascript() {
        for source in ["x = 1", "print('hello')", "foo(bar)", ""] {
            assert!(detect_language(None, None, source).is_err());
        }
    }
    #[test]
    fn extension_and_explicit_priority() {
        let path = Path::new("app.py");
        assert_eq!(
            detect_language(None, Some(path), "").unwrap().language,
            LanguageId::Python
        );
        assert_eq!(
            detect_language(Some(LanguageId::JavaScript), Some(path), "")
                .unwrap()
                .reason,
            DetectionReason::Explicit
        );
    }
    #[test]
    fn shebangs_and_directives() {
        for source in [
            "#!/usr/bin/env python3\n",
            "#!/usr/bin/env -S python3 -u\n",
            "#!/usr/bin/python3.12\n",
            "# zipp:language=python\n",
        ] {
            assert_eq!(
                detect_language(None, None, source).unwrap().language,
                LanguageId::Python
            );
        }
        assert!(detect_language(None, None, "#!/usr/bin/python-malware\n").is_err());
        assert!(detect_language(None, None, "x = 1\n# zipp:language=python\n").is_err());
        assert!(detect_language(None, None, "# zipp:language=typo\n").is_err());
    }
    #[test]
    fn independent_javascript_goals() {
        assert!(compile_source(
            "return 1;",
            Frontend::JavaScript {
                goal: ScriptGoal::Pure
            }
        )
        .is_err());
        assert!(compile_source(
            "return 1;",
            Frontend::JavaScript {
                goal: ScriptGoal::Compat
            }
        )
        .is_ok());
    }
    #[cfg(not(feature = "python"))]
    #[test]
    fn unavailable_is_an_error_not_a_fallback() {
        assert!(compile_source(
            "pass",
            Frontend::Python {
                mode: PythonMode::Module
            }
        )
        .is_err());
    }
}
