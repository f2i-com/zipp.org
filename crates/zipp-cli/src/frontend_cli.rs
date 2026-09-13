//! Additive CLI commands. The established `js`, `mjs`, sandbox and other
//! commands continue through the original dispatch unchanged.
use std::io::{self, Read};
use std::path::Path;
use zipp_vm::embed::ScriptGoal;
use zipp_vm::frontend::{
    compile_python_project, compile_source, detect_language, Frontend, LanguageId, PythonMode,
};

pub(super) fn try_run(args: &[String]) -> Option<Result<(), String>> {
    let first = args.first()?.as_str();
    if first != "py" && first != "run" && first != "--lang" && !first.starts_with("--lang=") {
        return None;
    }
    Some(run(args))
}
fn run(args: &[String]) -> Result<(), String> {
    let first = args[0].as_str();
    let python_command = first == "py";
    let mut explicit = if python_command {
        Some(LanguageId::Python)
    } else {
        None
    };
    let mut filename: Option<String> = None;
    let mut index = if first == "py" || first == "run" {
        1
    } else {
        0
    };
    let mut options = true;
    while index < args.len() {
        let arg = &args[index];
        if options && arg == "--" {
            options = false;
            index += 1;
            continue;
        }
        let language = if options && arg == "--lang" {
            index += 1;
            Some(
                args.get(index)
                    .ok_or("--lang requires javascript or python")?
                    .as_str(),
            )
        } else if options {
            arg.strip_prefix("--lang=")
        } else {
            None
        };
        if let Some(language) = language {
            let language: LanguageId = language.parse()?;
            if explicit.is_some_and(|previous| previous != language) {
                return Err("conflicting language selections".into());
            }
            explicit = Some(language);
        } else if options && arg.starts_with('-') && arg != "-" {
            return Err(format!("unknown frontend option {arg:?}"));
        } else if filename.replace(arg.clone()).is_some() {
            return Err("expected one source filename, or - for standard input".into());
        }
        index += 1;
    }
    let filename = filename.ok_or(
        "usage: zipp py FILE|DIR | zipp run [--lang=python|javascript] FILE | zipp --lang=python -",
    )?;
    let stdin = filename == "-";
    // A directory is a Python project: every top-level `.py` file is a module
    // and `main.py` is the entry. Nothing is read recursively.
    if !stdin && Path::new(&filename).is_dir() {
        if explicit.is_some_and(|language| language != LanguageId::Python) {
            return Err("only Python projects can be run from a directory".into());
        }
        return run_project_dir(Path::new(&filename));
    }
    // Bound ingestion before allocating an unbounded stdin/file buffer. This is
    // not a sandbox switch. The old CLI commands retain their existing policy.
    const INPUT_LIMIT: u64 = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    if stdin {
        io::stdin()
            .lock()
            .take(INPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
    } else {
        std::fs::File::open(&filename)
            .map_err(|e| format!("{filename}: {e}"))?
            .take(INPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
    }
    if bytes.len() as u64 > INPUT_LIMIT {
        return Err("frontend input exceeds 16 MiB".into());
    }
    let source = String::from_utf8(bytes).map_err(|_| "source must be UTF-8")?;
    let path = if stdin {
        None
    } else {
        Some(Path::new(&filename))
    };
    let detected = detect_language(explicit, path, &source)?;
    if detected.language == LanguageId::JavaScript && !stdin {
        // `.mjs` retains the existing module loader rather than being silently
        // parsed as a script. Explicit Python selection wins over the extension.
        let command = if path
            .and_then(Path::extension)
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("mjs"))
        {
            "mjs"
        } else {
            "js"
        };
        return super::run(&[command.to_owned(), filename]);
    }
    let frontend = match detected.language {
        LanguageId::JavaScript => Frontend::JavaScript {
            goal: ScriptGoal::Compat,
        },
        LanguageId::Python => Frontend::Python {
            mode: PythonMode::Module,
        },
    };
    let compiled = compile_source(&source, frontend)?;
    execute(compiled)
}

fn execute(mut compiled: zipp_vm::frontend::CompiledSource) -> Result<(), String> {
    let state = compiled.state_mut();
    let outcome = state.run_init();
    for line in state.take_output() {
        println!("{line}");
    }
    for line in state.take_errput() {
        eprintln!("{line}");
    }
    outcome.map(|_| ())
}

fn run_project_dir(dir: &Path) -> Result<(), String> {
    const FILE_LIMIT: u64 = 1024 * 1024;
    let mut modules = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("py") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .take(FILE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > FILE_LIMIT {
            return Err(format!("{}: exceeds 1 MiB", path.display()));
        }
        let source =
            String::from_utf8(bytes).map_err(|_| format!("{}: not UTF-8", path.display()))?;
        modules.push((stem.to_owned(), source));
    }
    modules.sort();
    if modules.is_empty() {
        return Err(format!("{}: no .py files", dir.display()));
    }
    if !modules.iter().any(|(name, _)| name == "main") {
        return Err(format!(
            "{}: a project needs a main.py entry",
            dir.display()
        ));
    }
    execute(compile_python_project("main", &modules)?)
}
