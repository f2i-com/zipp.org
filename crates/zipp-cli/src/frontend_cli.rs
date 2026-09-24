//! Additive CLI commands. The established `js`, `mjs`, sandbox and other
//! commands continue through the original dispatch unchanged.
use std::collections::{HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use zipp_vm::embed::{ConsoleStream, ScriptGoal, ScriptState};
use zipp_vm::frontend::{
    compile_python_script, compile_source, detect_language, Frontend, LanguageId, PythonMode,
};

pub(super) fn try_run(args: &[String]) -> Option<Result<(), String>> {
    let first = args.first()?.as_str();
    if first != "py" && first != "run" && first != "--lang" && !first.starts_with("--lang=") {
        return None;
    }
    // One program per process: its compiled runtime is never reused.
    zipp_vm::frontend::set_python_runtime_memo(false);
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
    let mut bytecode = false;
    // `--no-gpu`: keep a torch/zipp_gpu program on the CPU graph evaluator
    // even when a GPU adapter is available (as `ZIPP_GPU=0` does).
    let mut gpu = true;
    let mut program_args: Vec<String> = Vec::new();
    while index < args.len() {
        let arg = &args[index];
        if options && arg == "--" {
            options = false;
            index += 1;
            continue;
        }
        if options && arg == "--bc" {
            bytecode = true;
            index += 1;
            continue;
        }
        if options && arg == "--no-gpu" {
            gpu = false;
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
        } else if options && arg.starts_with('-') && arg != "-" && filename.is_none() {
            return Err(format!("unknown frontend option {arg:?}"));
        } else if filename.is_none() {
            filename = Some(arg.clone());
            // Everything after the program is its `sys.argv[1:]`.
            program_args.extend(args[index + 1..].iter().cloned());
            break;
        }
        index += 1;
    }
    let filename = filename.ok_or(
        "usage: zipp py [--bc] [--no-gpu] FILE|DIR [ARGS...] | zipp run [--lang=python|javascript] [--no-gpu] FILE [ARGS...] | zipp --lang=python -",
    )?;
    let stdin = filename == "-";
    // A directory is a Python project rooted there with `main.py` as the
    // entry; a `.py` file runs as the entry of the project rooted at its
    // folder (so it can import its neighbours and read the files next to
    // it), unless bytecode inspection was asked for.
    if !stdin && Path::new(&filename).is_dir() {
        if explicit.is_some_and(|language| language != LanguageId::Python) {
            return Err("only Python projects can be run from a directory".into());
        }
        if bytecode {
            return Err("--bc takes a file, not a directory".into());
        }
        return run_project(Path::new(&filename), None, &program_args, gpu);
    }
    if !stdin
        && !bytecode
        && is_python_path(Path::new(&filename))
        && explicit.is_none_or(|language| language == LanguageId::Python)
    {
        let path = Path::new(&filename);
        let root = project_root_of(path);
        return run_project(&root, Some(path), &program_args, gpu);
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
        let module = path
            .and_then(Path::extension)
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("mjs"));
        if !bytecode {
            return super::run(&[if module { "mjs" } else { "js" }.to_owned(), filename]);
        }
        // `--bc` inspects and never runs: a script falls through to the
        // bytecode printer below, and a module, which that printer would parse
        // with the wrong grammar, is refused rather than executed.
        if module {
            return Err("--bc takes a script; a .mjs module cannot be inspected this way".into());
        }
    }
    // Any other Python file run by name (a shebang script, `zipp py tool`)
    // is the entry of its project exactly like a `.py` file.
    if detected.language == LanguageId::Python && !stdin && !bytecode {
        let path = Path::new(&filename);
        return run_project(&project_root_of(path), Some(path), &program_args, gpu);
    }
    let frontend = match detected.language {
        LanguageId::JavaScript => Frontend::JavaScript {
            goal: ScriptGoal::Compat,
        },
        LanguageId::Python => Frontend::Python {
            mode: PythonMode::Module,
        },
    };
    if bytecode {
        // Developer inspection: the compiled program, no execution.
        println!(
            "{}",
            zipp_vm::frontend::compile_source_to_text(&source, frontend)?
        );
        return Ok(());
    }
    let compiled = compile_source(&source, frontend)?;
    execute(compiled, gpu)
}

/// A Python program's graphs on the native GPU, when the build has it and
/// the run did not opt out.
fn install_gpu(state: &mut ScriptState, gpu: bool) {
    #[cfg(feature = "gpu")]
    super::gpu::install(state, gpu);
    #[cfg(not(feature = "gpu"))]
    let _ = (state, gpu);
}

/// A command-line Python run has no instruction budget, so it runs on the
/// VM JIT like `zipp js` (the embedding API's Python states start without it);
/// `ZIPP_PY_JIT=0` keeps it interpreted, `ZIPP_NOJIT=1` turns every JIT off.
/// The JIT's Python tier is x86-64 only: elsewhere Python stays interpreted.
fn enable_python_jit(state: &mut ScriptState) {
    if cfg!(target_arch = "x86_64") && zipp_vm::frontend::python_jit_env() != Some(false) {
        state.enable_vm_jit();
    }
}

fn execute(mut compiled: zipp_vm::frontend::CompiledSource, gpu: bool) -> Result<(), String> {
    let python = compiled.language() == LanguageId::Python;
    let state = compiled.state_mut();
    stream_console(state);
    if python {
        install_gpu(state, gpu);
        enable_python_jit(state);
    }
    let outcome = state.run_init();
    // A program read from standard input has no project folder to write to.
    if python {
        if let Ok(zipp_vm::embed::JsValue::String(listing)) =
            state.call_global("__zipp_py_vfs_changed", &[])
        {
            let paths = serde_json::from_str::<serde_json::Value>(&listing)
                .ok()
                .and_then(|v| v.get("changes").and_then(|c| c.as_array()).cloned())
                .unwrap_or_default();
            if !paths.is_empty() {
                let names: Vec<&str> = paths
                    .iter()
                    .filter_map(|c| c.get("path").and_then(|p| p.as_str()))
                    .take(5)
                    .collect();
                eprintln!(
                    "zipp: warning: {} file change(s) discarded, a program read from standard input has no project folder: {}",
                    paths.len(),
                    names.join(", ")
                );
            }
        }
    }
    // A Python traceback and its exit status come from the frontend, not from
    // the generic error path.
    if python {
        return python_exit(state, outcome.map(|_| ()));
    }
    outcome.map(|_| ())
}

/// A Python program that ended by `SystemExit` sets the process status the way
/// CPython does: an integer code becomes the status and nothing is printed; any
/// other code is printed to stderr and the status is 1. Every other failure
/// keeps the CLI's `zipp: <error>` report.
///
/// The status is the code's LOW 8 BITS, which is what `std::process::ExitCode`
/// can express and what a POSIX wait status carries. CPython on Windows passes
/// the full 32-bit value instead, so `sys.exit(258)` is 258 there and 2 here.
fn python_exit(
    state: &mut zipp_vm::embed::ScriptState,
    outcome: Result<(), String>,
) -> Result<(), String> {
    let status = match state.call_global("__zipp_py_exit_status", &[]) {
        Ok(zipp_vm::embed::JsValue::Number(code)) => (code as i64 & 0xff) as u8,
        Ok(zipp_vm::embed::JsValue::String(text)) => {
            eprintln!("{text}");
            1
        }
        _ => return outcome,
    };
    super::request_exit(status);
    Ok(())
}

/// Print console lines as the program produces them, so a long run shows
/// its progress and the two streams interleave as they were written. A
/// closed pipe drops the output rather than aborting the program.
fn stream_console(state: &mut ScriptState) {
    state.set_console_sink(Box::new(|stream, line| {
        let _ = match stream {
            ConsoleStream::Stdout => writeln!(io::stdout().lock(), "{line}"),
            ConsoleStream::Stderr => writeln!(io::stderr().lock(), "{line}"),
        };
    }));
}

/// The project a script belongs to: the working directory when the script
/// is inside it (as `python path/to/script.py` sees the tree from where it
/// is run: files open relative to it, and a `tests/test_x.py` imports the
/// modules of the folder it is run from), otherwise the script's own folder.
/// A filesystem root or the home folder is where scripts are run from, not a
/// project, so a script below one is rooted at its own folder. The script's
/// folder is always searched first for bare module names.
fn project_root_of(script: &Path) -> PathBuf {
    let own = script
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let own = std::fs::canonicalize(&own).unwrap_or(own);
    if let Ok(cwd) = std::env::current_dir() {
        let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
        if own.starts_with(&cwd) && !is_broad_folder(&cwd) {
            return cwd;
        }
    }
    own
}

/// A filesystem root or the user's home folder.
fn is_broad_folder(dir: &Path) -> bool {
    if dir.parent().is_none() {
        return true;
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    home.and_then(|home| std::fs::canonicalize(home).ok())
        .is_some_and(|home| home == dir)
}

/// Apply the same extension rules as language detection.
fn is_python_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("py") || s.eq_ignore_ascii_case("pyw"))
}

// Windows junctions and other reparse points must not bypass the symlink policy.
fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// A root-relative `/` path the project may name: no empty, `.` or `..`
/// segment, no absolute or prefixed component, no NUL, and none of the
/// separators the host filesystem would read differently (`\` everywhere,
/// and `:`, which is stream syntax, on Windows).
fn valid_relative(relative: &str) -> bool {
    !relative.is_empty()
        && !relative.contains(['\\', '\0'])
        && !(cfg!(windows) && relative.contains(':'))
        && relative
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(relative)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Refuse links in every existing component, including the final file.
/// The trusted CLI assumes another process is not replacing this tree mid-run.
fn checked_project_path(
    root: &Path,
    relative: &str,
    create_parents: bool,
) -> Result<PathBuf, String> {
    if !valid_relative(relative) {
        return Err(format!("invalid project path: {relative:?}"));
    }
    let parts: Vec<_> = relative.split('/').collect();
    let mut target = root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        target.push(part);
        let parent = i + 1 < parts.len();
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) => {
                if is_link(&metadata) {
                    return Err(format!(
                        "project path contains a symlink or reparse point: {}",
                        target.display()
                    ));
                }
                if parent && !metadata.is_dir() {
                    return Err(format!("not a project directory: {}", target.display()));
                }
                if !parent && !metadata.is_file() {
                    return Err(format!("not a regular project file: {}", target.display()));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if parent && create_parents {
                    std::fs::create_dir(&target)
                        .map_err(|e| format!("{}: {e}", target.display()))?;
                }
            }
            Err(error) => return Err(format!("{}: {error}", target.display())),
        }
    }
    Ok(target)
}

/// NTFS and APFS match names case-insensitively, so there two spellings of
/// one name are one file; elsewhere a path is its own key.
fn disk_key(relative: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        relative.to_lowercase()
    } else {
        relative.to_owned()
    }
}

/// One reported change that passed every check, ready to apply.
struct Change {
    relative: String,
    target: PathBuf,
    data: Option<Vec<u8>>,
}

/// Copy the program's file changes to disk. Every change is checked before
/// anything is written, and a change that fails is skipped while the rest
/// still apply; the skipped ones are returned as (path, reason) for the
/// caller to report, so a result is never partial without saying so. A
/// change may not touch a file that exists on disk but was never loaded
/// into the program (a skipped folder, a file over the size limits, an
/// unreadable one): the program saw it as missing, and writing would replace
/// contents it never read. `Err` is a malformed change report.
fn write_project_changes(
    root: &Path,
    listing: &str,
    loaded: &HashSet<String>,
) -> Result<Vec<(String, String)>, String> {
    let envelope: serde_json::Value =
        serde_json::from_str(listing).map_err(|e| format!("invalid VFS changes: {e}"))?;
    if envelope.get("version").and_then(|v| v.as_u64()) != Some(1) {
        return Err("unsupported VFS change version".into());
    }
    let reported = envelope
        .get("changes")
        .and_then(|v| v.as_array())
        .ok_or("missing VFS changes")?;
    let mut changes = Vec::with_capacity(reported.len());
    let mut refused: Vec<(String, String)> = Vec::new();
    for change in reported {
        let relative = change
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing VFS path")?;
        let deleted = change.get("deleted").and_then(|v| v.as_bool()) == Some(true);
        let data = change.get("base64").and_then(|v| v.as_str());
        if deleted == data.is_some() {
            return Err("VFS change needs exactly one of deletion or data".into());
        }
        let data = match data {
            Some(text) => Some(decode_base64(text).ok_or("invalid VFS base64")?),
            None => None,
        };
        let checked = checked_project_path(root, relative, false).and_then(|target| {
            match std::fs::symlink_metadata(&target) {
                Ok(_) if !loaded.contains(&disk_key(relative)) => Err(
                    "it exists on disk but was not loaded into the program (a skipped folder, over the size limits, or unreadable), so the program never saw its contents"
                        .to_owned(),
                ),
                _ => Ok(target),
            }
        });
        match checked {
            Ok(target) => changes.push(Change {
                relative: relative.to_owned(),
                target,
                data,
            }),
            Err(reason) => refused.push((relative.to_owned(), reason)),
        }
    }
    // Deletions first: a rename that only changes the case of a name is
    // reported as a write of the new spelling and a deletion of the old, and
    // on a case-insensitive disk both are the same file.
    let written: HashSet<String> = changes
        .iter()
        .filter(|c| c.data.is_some())
        .map(|c| disk_key(&c.relative))
        .collect();
    for change in changes.iter().filter(|c| c.data.is_none()) {
        let key = disk_key(&change.relative);
        if written.contains(&key) {
            // Keep the file and give it the new spelling; the writes below
            // replace its contents. When several spellings were written, the
            // last one is both the final content and the name it keeps.
            if let Some(new) = changes
                .iter()
                .rev()
                .find(|c| c.data.is_some() && disk_key(&c.relative) == key)
            {
                let _ = std::fs::rename(&change.target, &new.target);
            }
            continue;
        }
        match std::fs::remove_file(&change.target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => refused.push((change.relative.clone(), error.to_string())),
        }
    }
    for change in &changes {
        let Some(bytes) = &change.data else { continue };
        let result = checked_project_path(root, &change.relative, true)
            .and_then(|target| std::fs::write(&target, bytes).map_err(|e| e.to_string()));
        if let Err(reason) = result {
            refused.push((change.relative.clone(), reason));
        }
    }
    Ok(refused)
}

/// Folders never read into a project's virtual filesystem (nor any folder
/// whose name starts with a dot).
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "__pycache__",
    "node_modules",
    "target",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    "dist",
];
/// Per-file and total ceilings of the virtual filesystem (larger files are
/// left out with a note, not an error).
const VFS_FILE_LIMIT: u64 = 8 * 1024 * 1024;
const VFS_TOTAL_LIMIT: u64 = 64 * 1024 * 1024;
const MODULE_LIMIT: u64 = 1024 * 1024;
/// The frontend's ceiling on a project's candidate modules.
const MODULE_COUNT_LIMIT: usize = 256;
/// Directory entries a project walk visits before it stops, so a script run
/// from a large tree starts promptly; what lies beyond is left out.
const TREE_ENTRY_LIMIT: usize = 20_000;

/// What a walk of the project folder found.
#[derive(Default)]
struct Tree {
    /// Files as (root-relative `/` path, path, size): the first folder's
    /// files, then breadth-first from the root.
    files: Vec<(String, PathBuf, u64)>,
    /// Folders and entries that could not be listed or inspected.
    unreadable: Vec<String>,
    /// The walk stopped at `TREE_ENTRY_LIMIT`.
    truncated: bool,
}

fn relative_of(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Walk the files under `root` breadth-first, starting with `first` (the
/// script's folder, walked even when it is one the walk would skip), skipping
/// links, tool folders and dot-folders. A folder or entry that cannot be read
/// is noted and skipped, and the walk stops after `TREE_ENTRY_LIMIT` entries.
fn collect_tree(root: &Path, first: Option<&Path>) -> Tree {
    let mut tree = Tree::default();
    let mut queue: VecDeque<PathBuf> = first.map(Path::to_path_buf).into_iter().collect();
    queue.push_back(root.to_path_buf());
    let mut visited = 0usize;
    while let Some(dir) = queue.pop_front() {
        let mut entries: Vec<_> = match std::fs::read_dir(&dir) {
            Ok(entries) => entries.filter_map(Result::ok).collect(),
            Err(_) => {
                tree.unreadable.push(relative_of(root, &dir));
                continue;
            }
        };
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            visited += 1;
            if visited > TREE_ENTRY_LIMIT {
                tree.truncated = true;
                return tree;
            }
            let path = entry.path();
            // `DirEntry::metadata` does not follow links, and on Windows it
            // comes with the directory listing rather than a system call.
            let Ok(metadata) = entry.metadata() else {
                tree.unreadable.push(relative_of(root, &path));
                continue;
            };
            if is_link(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if first == Some(path.as_path())
                    || SKIPPED_DIRS.contains(&name.as_ref())
                    || name.starts_with('.')
                {
                    continue;
                }
                queue.push_back(path);
            } else if metadata.is_file() {
                tree.files
                    .push((relative_of(root, &path), path, metadata.len()));
            }
        }
    }
    tree
}

/// The dotted module name of a root-relative `.py` path.
fn module_name_of(relative: &str) -> Option<String> {
    let path = Path::new(relative);
    if !is_python_path(path) {
        return None;
    }
    let extension = path.extension()?.to_str()?;
    let stem = &relative[..relative.len() - extension.len() - 1];
    let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
    let name = stem.replace('/', ".");
    let valid = !name.is_empty()
        && name.split('.').all(|segment| {
            let mut chars = segment.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    valid.then_some(name)
}

/// A note about files left out of the project, naming the first few.
fn note_left_out(what: &str, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let shown: Vec<&str> = paths.iter().take(5).map(String::as_str).collect();
    let more = if paths.len() > shown.len() { ", ..." } else { "" };
    eprintln!(
        "zipp: {} {what}: {}{more}",
        paths.len(),
        shown.join(", ")
    );
}

/// Run the project rooted at `root`: every `.py` file in the tree is an
/// importable module, every file is readable through the virtual
/// filesystem, `script` (or `main.py`) is the entry, and files the program
/// writes are written back under `root` when it finishes. With `gpu`, its
/// `torch.compile` / `zipp_gpu` graphs run on a hardware GPU when one is
/// available, with unchanged semantics (see `gpu.rs`).
fn run_project(
    root: &Path,
    script: Option<&Path>,
    argv: &[String],
    gpu: bool,
) -> Result<(), String> {
    let canonical_root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let root = canonical_root.as_path();
    let script = match script {
        Some(script) => Some(
            std::fs::canonicalize(script).map_err(|e| format!("{}: {e}", script.display()))?,
        ),
        None => None,
    };
    let script_dir = script.as_deref().and_then(Path::parent);
    let mut tree = collect_tree(root, script_dir.filter(|dir| *dir != root));
    // The entry: the script itself, whatever its name, or the root's main.py.
    let script = match script {
        Some(script) => script,
        None => tree
            .files
            .iter()
            .find(|(relative, _, _)| module_name_of(relative).as_deref() == Some("main"))
            .map(|(_, path, _)| path.clone())
            .ok_or_else(|| format!("{}: a project needs a main.py entry", root.display()))?,
    };
    let entry_file = script
        .strip_prefix(root)
        .map(|_| relative_of(root, &script))
        .map_err(|_| format!("{}: not under {}", script.display(), root.display()))?;
    let entry_source = {
        let mut bytes = Vec::new();
        std::fs::File::open(&script)
            .and_then(|f| f.take(MODULE_LIMIT + 1).read_to_end(&mut bytes))
            .map_err(|e| format!("{entry_file}: {e}"))?;
        if bytes.len() as u64 > MODULE_LIMIT {
            return Err(format!("{entry_file}: exceeds the 1 MiB module limit"));
        }
        String::from_utf8(bytes).map_err(|_| format!("{entry_file}: not UTF-8 text"))?
    };
    // A file name that is not a module name (`my-script.py`, `tool`) runs as
    // `__main__`, which nothing else can import.
    let entry_base = entry_file.rsplit('/').next().unwrap_or(&entry_file);
    let entry = module_name_of(entry_base).unwrap_or_else(|| "__main__".to_owned());
    // The script's own folder, as a root-relative prefix ("" at the root).
    let script_dir_prefix = match entry_file.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    };
    // Load source before unrelated data, and the entry folder first. A large
    // checkout must not exhaust the VFS budget before its requested script.
    tree.files.sort_by_key(|(relative, _, _)| {
        let in_entry_folder = if script_dir_prefix.is_empty() {
            !relative.contains('/')
        } else {
            relative.starts_with(&script_dir_prefix)
        };
        (!in_entry_folder, !is_python_path(Path::new(relative)))
    });
    let mut files = Vec::new();
    let mut loaded = HashSet::new();
    // Indices into `files` of the other `.py` files that are UTF-8 text.
    let mut python_files: Vec<usize> = Vec::new();
    let mut total = 0u64;
    let mut over_limits = Vec::new();
    for (relative, path, size) in std::mem::take(&mut tree.files) {
        let is_py = is_python_path(Path::new(&relative));
        if !valid_relative(&relative) {
            tree.unreadable.push(relative);
            continue;
        }
        if size > VFS_FILE_LIMIT || (is_py && size > MODULE_LIMIT) || total + size > VFS_TOTAL_LIMIT
        {
            over_limits.push(relative);
            continue;
        }
        // A file that cannot be read (gone, locked, unreadable) is left out.
        let mut bytes = Vec::new();
        let readable = std::fs::File::open(&path)
            .and_then(|f| f.take(VFS_FILE_LIMIT + 1).read_to_end(&mut bytes))
            .is_ok();
        if !readable {
            tree.unreadable.push(relative);
            continue;
        }
        if bytes.len() as u64 > VFS_FILE_LIMIT
            || total + bytes.len() as u64 > VFS_TOTAL_LIMIT
            || (is_py && bytes.len() as u64 > MODULE_LIMIT)
        {
            over_limits.push(relative);
            continue;
        }
        total += bytes.len() as u64;
        if is_py && relative != entry_file && std::str::from_utf8(&bytes).is_ok() {
            python_files.push(files.len());
        }
        loaded.insert(disk_key(&relative));
        files.push((relative, bytes));
    }
    // Module names: the entry, then the script folder's files by their bare
    // names (the script's folder comes first, as `sys.path[0]` does), then
    // every file by its root-relative dotted name. A name already taken is
    // shadowed, not an error; the shadowed file stays readable. Sources are
    // borrowed from the loaded files, not held a second time.
    let mut modules: Vec<(String, &str)> = vec![(entry.clone(), entry_source.as_str())];
    let mut taken: HashSet<String> = HashSet::from([entry.clone()]);
    let mut unimportable: Vec<String> = Vec::new();
    let aliases = python_files.iter().filter_map(|&i| {
        let rest = files[i].0.strip_prefix(script_dir_prefix.as_str())?;
        Some((module_name_of(rest).filter(|_| !script_dir_prefix.is_empty())?, Some(i)))
    });
    let dotted = python_files
        .iter()
        .filter_map(|&i| Some((module_name_of(&files[i].0)?, Some(i))));
    // A nested entry is also importable by its dotted name, as before.
    let entry_dotted = module_name_of(&entry_file)
        .filter(|_| !script_dir_prefix.is_empty())
        .map(|name| (name, None));
    for (name, index) in aliases.chain(dotted).chain(entry_dotted) {
        if taken.contains(&name) {
            continue;
        }
        let relative = index.map_or(&entry_file, |i| &files[i].0);
        if modules.len() >= MODULE_COUNT_LIMIT {
            unimportable.push(relative.clone());
            continue;
        }
        let source = match index {
            // Checked to be UTF-8 when the file was loaded.
            Some(i) => std::str::from_utf8(&files[i].1).unwrap_or_default(),
            None => entry_source.as_str(),
        };
        taken.insert(name.clone());
        modules.push((name, source));
    }
    unimportable.sort();
    unimportable.dedup();
    if tree.truncated {
        eprintln!(
            "zipp: stopped reading {} after {TREE_ENTRY_LIMIT} entries; the files beyond are left out of the project (run the script from its own folder to load less)",
            root.display()
        );
    }
    note_left_out(
        "file(s) left out of the project (over the size limits; the program sees them as missing and cannot write to them)",
        &over_limits,
    );
    note_left_out(
        "unreadable file(s) or folder(s) left out of the project",
        &tree.unreadable,
    );
    note_left_out(
        &format!("Python file(s) past the {MODULE_COUNT_LIMIT}-module limit cannot be imported"),
        &unimportable,
    );
    let mut compiled =
        compile_python_script(&entry, &entry_file, &modules, &files, argv, false)?;
    drop(modules);
    drop(files);
    let state = compiled.state_mut();
    stream_console(state);
    install_gpu(state, gpu);
    enable_python_jit(state);
    let outcome = state.run_init();
    // Files the program wrote go back to disk, inside the root only. The
    // program's own failure (and its traceback) is the run's result even
    // when some of its changes could not be written, and it is reported
    // first.
    let refused = match state.call_global("__zipp_py_vfs_changed", &[]) {
        Ok(zipp_vm::embed::JsValue::String(listing)) => {
            write_project_changes(root, &listing, &loaded)?
        }
        _ => Vec::new(),
    };
    let lines: Vec<String> = refused
        .iter()
        .map(|(relative, reason)| format!("zipp: not written back: {relative}: {reason}"))
        .collect();
    let summary = format!(
        "{} of the program's file change(s) were not written back",
        refused.len()
    );
    // A SystemExit is the program choosing a process status, not a failure:
    // `python_exit` turns it into that status and prints nothing, and leaves a
    // real traceback to be reported as the run's error.
    match outcome {
        Err(error) if refused.is_empty() => python_exit(state, Err(error)),
        Err(error) => match python_exit(state, Err(error)) {
            // The program chose its status and keeps it, but a result is never
            // partial without saying so: the refusals are reported regardless.
            Ok(()) => {
                for line in lines {
                    eprintln!("{line}");
                }
                eprintln!("zipp: {summary}");
                Ok(())
            }
            Err(error) => Err(format!("{error}\n{}\nzipp: {summary}", lines.join("\n"))),
        },
        Ok(_) if refused.is_empty() => Ok(()),
        Ok(_) => {
            for line in lines {
                eprintln!("{line}");
            }
            Err(summary)
        }
    }
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(all(test, unix))]
mod containment_tests {
    use super::*;
    #[test]
    fn links_are_omitted_and_write_targets_are_refused() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!(
            "zipp-link-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir(base.join("fixture-target")).unwrap();
        std::fs::write(root.join("main.py"), "print(1)").unwrap();
        std::fs::write(base.join("fixture-target/data"), "fixture").unwrap();
        symlink(base.join("fixture-target"), root.join("linked-dir")).unwrap();
        symlink(base.join("fixture-target/data"), root.join("linked-file")).unwrap();
        symlink(&root, root.join("cycle")).unwrap();
        let tree = collect_tree(&root, None);
        assert_eq!(tree.files.len(), 1);
        for path in ["linked-dir/data", "linked-file", "cycle/new.txt"] {
            assert!(checked_project_path(&root, path, true).is_err());
            assert!(checked_project_path(&root, path, false).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(base.join("fixture-target/data")).unwrap(),
            "fixture"
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
