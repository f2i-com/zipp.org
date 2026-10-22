//! The languages, and explicit, additive language selection. Existing
//! JavaScript APIs are unchanged.
//!
//! Zipp is one engine for several languages. The core (`bytecode`, `heap`,
//! `vm`, `codegen`) is the base language's, JavaScript's; each language
//! lives in `lang/<name>/` and is one entry of [`languages`]. A language
//! compiles its source to the shared bytecode, using the base instructions
//! where its semantics agree and adding its own where they differ. See
//! `docs/LANGUAGES.md` for the extension points and for adding a language.
//!
//! Python is an experimental, feature-gated subset, not CPython compatibility.
//! Detection is a caller-side policy and never a compiler fallback.
use crate::embed::{ScriptGoal, ScriptState};
use std::path::Path;
use std::str::FromStr;

mod js;
mod python;

/// A language the runtime runs: its names, how a file is recognised as its,
/// and its frontend. Every language implements it once, in `lang/<name>/`,
/// and is listed in [`languages`]; language detection, the `--lang` names
/// and single-file compiles go through that table.
pub trait Language: Sync {
    /// The identity a compiled source carries.
    fn id(&self) -> LanguageId;
    /// The names a host may select it by (`--lang`), lowercase; the first
    /// is its name in messages.
    fn names(&self) -> &'static [&'static str];
    /// File extensions that select it, lowercase, without the dot.
    fn extensions(&self) -> &'static [&'static str];
    /// Whether a `#!` line whose command is `command` (a basename, the
    /// command after `env` when the line uses it) selects it.
    fn shebang(&self, command: &str) -> bool;
    /// Compile a single-file program with the language's default options.
    /// A language this build leaves out is still listed, so a source is
    /// still recognised as its; compiling it is the error.
    fn compile(&self, source: &str) -> Result<ScriptState, String>;
    /// Developer-only inspection: the compiled program as text, with no
    /// guest execution.
    fn compile_to_text(&self, source: &str) -> Result<String, String>;
}

/// Every language, the base language first.
pub fn languages() -> &'static [&'static dyn Language] {
    &[&js::JavaScript, &python::Python]
}

/// The language `id` names.
pub fn language(id: LanguageId) -> &'static dyn Language {
    languages()
        .iter()
        .copied()
        .find(|l| l.id() == id)
        .expect("every LanguageId is in the table")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageId {
    JavaScript,
    Python,
}
impl FromStr for LanguageId {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let key = value.trim().to_ascii_lowercase();
        match languages().iter().find(|l| l.names().contains(&key.as_str())) {
            Some(l) => Ok(l.id()),
            None => Err(format!(
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
        Frontend::JavaScript { goal } => js::compile(source, goal)?,
        Frontend::Python {
            mode: PythonMode::Module,
        } => self::language(language).compile(source)?,
    };
    Ok(CompiledSource { language, state })
}
/// A Python state starts with the VM JIT off: an embedder's instruction
/// budget is charged per interpreted instruction, and compiled code meters by
/// basic block (it over-charges a block it leaves through a guard bail), so a
/// budgeted Python program could run out of steps where the interpreter would
/// not. A host that attaches no budget, such as the command line's `zipp py`,
/// turns it on with [`ScriptState::enable_vm_jit`]. `ZIPP_PY_JIT=1` in the
/// environment keeps it on from the start (the test lanes' JIT
/// configuration); `ZIPP_PY_JIT=0` keeps it off even when a host asks.
#[cfg(feature = "python")]
fn python_jit_default(state: &mut ScriptState) {
    // The JIT's Python tier is x86-64 only (see `ScriptState::enable_vm_jit`).
    if python_jit_env() != Some(true) || !cfg!(target_arch = "x86_64") {
        state.disable_vm_jit();
    }
}
/// `ZIPP_PY_JIT`: `Some(true)` for `1`, `Some(false)` for `0`, else `None`.
pub fn python_jit_env() -> Option<bool> {
    match std::env::var_os("ZIPP_PY_JIT")?.to_str()? {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
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
    compile_python_entry(entry, None, modules, files, argv, hosted)
}
/// [`compile_python_program`] for a script run by its file: `entry_file`
/// (root-relative, such as `tools/my-script.py` or an extensionless `tool`)
/// is the entry module's `__file__`, `sys.argv[0]` and traceback file,
/// whatever module name `entry` registers it under. Module sources are
/// borrowed, so a host that also loads the `.py` files into `files` can
/// point at those bytes instead of holding a second copy.
pub fn compile_python_script(
    entry: &str,
    entry_file: &str,
    modules: &[(String, &str)],
    files: &[(String, Vec<u8>)],
    argv: &[String],
    hosted: bool,
) -> Result<CompiledSource, String> {
    compile_python_entry(entry, Some(entry_file), modules, files, argv, hosted)
}
/// Whether Python compiles keep the compiled runtime for the next program in
/// the process. On by default: every program after the first clones it
/// instead of compiling ~420 KB of runtime JavaScript again (test suites,
/// the browser engine's re-runs, long-lived embedders). A process that
/// compiles exactly one program, such as a command-line run, turns it off
/// and so never copies a runtime it will not reuse.
pub fn set_python_runtime_memo(enabled: bool) {
    #[cfg(feature = "python")]
    python::set_runtime_memo(enabled);
    #[cfg(not(feature = "python"))]
    let _ = enabled;
}
/// Keep compiled Python code (the runtime every program starts from, and
/// library modules as they are imported) in `dir` between processes, or not
/// at all (`None`, the default). For a host that runs one program per
/// process, such as the `zipp` command: only the first process then compiles
/// the runtime (about 0.7 MB of JavaScript) and each library module. Entries
/// belong to the executable and settings that wrote them, see
/// `code_cache.rs` (one folder with [`crate::set_code_cache`]'s). Takes
/// effect if called before the first Python compile; ignored on `wasm32`.
pub fn set_python_runtime_cache(dir: Option<std::path::PathBuf>) {
    #[cfg(feature = "python")]
    python::set_runtime_cache(dir);
    #[cfg(not(feature = "python"))]
    let _ = dir;
}
/// Get Python ready ahead of the first program: compile the runtime every
/// Python program starts from (about 0.7 MB of JavaScript) into the
/// per-process memo ([`set_python_runtime_memo`], on by default), then run
/// an empty program once, so the code that initializes a Python state has
/// run before (on WebAssembly, the host's compiler optimizes it). A host
/// that expects to run Python calls it while idle so the first run does not
/// wait for either. Programs behave exactly as without it. A package added
/// later (`crate::python_packages`) makes the next Python compile rebuild
/// the runtime.
pub fn prewarm_python() -> Result<(), String> {
    #[cfg(feature = "python")]
    {
        python::prewarm()?;
        let mut compiled = compile_python_project("main", &[("main".to_owned(), "pass\n".to_owned())])?;
        compiled.state_mut().run_init().map(drop)
    }
    #[cfg(not(feature = "python"))]
    Err("Python support is not built; enable the `python` Cargo feature".into())
}
/// A file of a Python program's virtual filesystem: its bytes, or a reader
/// the host is asked for them with when the program first reads the file.
pub use crate::bytecode::PyFile as PythonFile;

/// [`compile_python_script`] with each file given as a [`PythonFile`], so a
/// host that runs a script inside a large folder can leave a file on disk
/// until the program opens it.
pub fn compile_python_script_files(
    entry: &str,
    entry_file: &str,
    modules: &[(String, &str)],
    files: Vec<(String, PythonFile)>,
    argv: &[String],
    hosted: bool,
) -> Result<CompiledSource, String> {
    compile_python_entry_files(entry, Some(entry_file), modules, files, argv, hosted)
}

fn compile_python_entry<S: AsRef<str>>(
    entry: &str,
    entry_file: Option<&str>,
    modules: &[(String, S)],
    files: &[(String, Vec<u8>)],
    argv: &[String],
    hosted: bool,
) -> Result<CompiledSource, String> {
    let files = files
        .iter()
        .map(|(path, bytes)| (path.clone(), PythonFile::Bytes(bytes.as_slice().into())))
        .collect();
    compile_python_entry_files(entry, entry_file, modules, files, argv, hosted)
}

fn compile_python_entry_files<S: AsRef<str>>(
    entry: &str,
    entry_file: Option<&str>,
    modules: &[(String, S)],
    files: Vec<(String, PythonFile)>,
    argv: &[String],
    hosted: bool,
) -> Result<CompiledSource, String> {
    #[cfg(feature = "python")]
    {
        let program = python::compile_project(entry, entry_file, modules, files, argv, hosted)?;
        let mut state = ScriptState::from_program(program);
        python_jit_default(&mut state);
        Ok(CompiledSource {
            language: LanguageId::Python,
            state,
        })
    }
    #[cfg(not(feature = "python"))]
    {
        let _ = (entry, entry_file, modules, files, argv, hosted);
        Err("Python support is not built; enable the `python` Cargo feature".into())
    }
}
/// Developer-only bytecode inspection, with no guest execution.
pub fn compile_source_to_text(source: &str, frontend: Frontend) -> Result<String, String> {
    match frontend {
        Frontend::JavaScript { goal } => js::compile_to_text(source, goal),
        Frontend::Python { .. } => language(LanguageId::Python).compile_to_text(source),
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
        let ext = ext.to_ascii_lowercase();
        if let Some(l) = languages().iter().find(|l| l.extensions().contains(&ext.as_str())) {
            return found(l.id(), DetectionReason::Extension);
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
            if let Some(l) = languages().iter().find(|l| l.shebang(&command)) {
                return found(l.id(), DetectionReason::Shebang);
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
    fn the_table_lists_each_language_once() {
        for id in [LanguageId::JavaScript, LanguageId::Python] {
            assert_eq!(languages().iter().filter(|l| l.id() == id).count(), 1);
            for name in language(id).names() {
                assert_eq!(name.parse::<LanguageId>().unwrap(), id, "{name}");
            }
        }
        let mut exts: Vec<&str> = languages().iter().flat_map(|l| l.extensions().iter().copied()).collect();
        let n = exts.len();
        exts.sort_unstable();
        exts.dedup();
        assert_eq!(exts.len(), n, "an extension selects two languages");
        assert_eq!(languages()[0].id(), LanguageId::JavaScript, "the base language first");
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
