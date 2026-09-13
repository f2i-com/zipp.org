//! Additive CLI commands. The established `js`, `mjs`, sandbox and other
//! commands continue through the original dispatch unchanged.
use std::io::{self, Read};
use std::path::Path;
use zipp_vm::embed::ScriptGoal;
use zipp_vm::frontend::{
    compile_python_program, compile_source, detect_language, Frontend, LanguageId, PythonMode,
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
    let mut bytecode = false;
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
        "usage: zipp py [--bc] FILE|DIR [ARGS...] | zipp run [--lang=python|javascript] FILE [ARGS...] | zipp --lang=python -",
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
        return run_project(Path::new(&filename), None, &program_args);
    }
    if !stdin
        && !bytecode
        && is_python_path(Path::new(&filename))
        && explicit.is_none_or(|language| language == LanguageId::Python)
    {
        let path = Path::new(&filename);
        let root = project_root_of(path);
        return run_project(&root, Some(path), &program_args);
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
    if bytecode {
        // Developer inspection: the compiled program, no execution.
        println!(
            "{}",
            zipp_vm::frontend::compile_source_to_text(&source, frontend)?
        );
        return Ok(());
    }
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

/// The project a script belongs to: the working directory when the script
/// is inside it (as `python path/to/script.py` sees the tree from where it
/// is run: files open relative to it, and a `tests/test_x.py` imports the
/// modules of the folder it is run from), otherwise the script's own folder.
/// The script's folder is always searched first for bare module names.
fn project_root_of(script: &Path) -> std::path::PathBuf {
    let own = script
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let own = std::fs::canonicalize(&own).unwrap_or(own);
    if let Ok(cwd) = std::env::current_dir() {
        let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
        if own.starts_with(&cwd) {
            return cwd;
        }
    }
    own
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

/// Refuse links in every existing component, including the final file.
/// The trusted CLI assumes another process is not replacing this tree mid-run.
fn checked_project_path(
    root: &Path,
    relative: &str,
    create_parents: bool,
) -> Result<std::path::PathBuf, String> {
    if relative.is_empty()
        || relative.contains(['\\', ':', '\0'])
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(relative)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
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

fn write_project_changes(root: &Path, listing: &str) -> Result<(), String> {
    let envelope: serde_json::Value =
        serde_json::from_str(listing).map_err(|e| format!("invalid VFS changes: {e}"))?;
    if envelope.get("version").and_then(|v| v.as_u64()) != Some(1) {
        return Err("unsupported VFS change version".into());
    }
    let changes = envelope
        .get("changes")
        .and_then(|v| v.as_array())
        .ok_or("missing VFS changes")?;
    for change in changes {
        let relative = change
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing VFS path")?;
        let deleted = change.get("deleted").and_then(|v| v.as_bool()) == Some(true);
        let data = change.get("base64").and_then(|v| v.as_str());
        if deleted == data.is_some() {
            return Err("VFS change needs exactly one of deletion or data".into());
        }
        let target = checked_project_path(root, relative, !deleted)?;
        if deleted {
            match std::fs::remove_file(&target) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{}: {error}", target.display())),
            }
        } else {
            let bytes = decode_base64(data.unwrap()).ok_or("invalid VFS base64")?;
            std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    Ok(())
}

/// Folders never read into a project's virtual filesystem.
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

/// Every file under `root` (recursively, skipping tool folders), as
/// root-relative `/`-separated paths.
fn collect_tree(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, std::path::PathBuf)>,
) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if is_link(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            if SKIPPED_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            collect_tree(root, &path, out)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((relative, path));
        }
    }
    Ok(())
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

/// Run the project rooted at `root`: every `.py` file in the tree is an
/// importable module, every file is readable through the virtual
/// filesystem, `script` (or `main.py`) is the entry, and files the program
/// writes are written back under `root` when it finishes.
fn run_project(root: &Path, script: Option<&Path>, argv: &[String]) -> Result<(), String> {
    let canonical_root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let root = canonical_root.as_path();
    let mut tree = Vec::new();
    collect_tree(root, root, &mut tree)?;
    let entry = match script {
        Some(script) => {
            let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            let canonical_script =
                std::fs::canonicalize(script).unwrap_or_else(|_| script.to_path_buf());
            let relative = canonical_script
                .strip_prefix(&canonical_root)
                .map_err(|_| format!("{}: not under {}", script.display(), root.display()))?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            // The entry is known by its bare name (its folder comes first).
            let bare = relative.rsplit('/').next().unwrap_or(&relative).to_owned();
            module_name_of(&bare)
                .ok_or_else(|| format!("{}: not a valid module path", script.display()))?
        }
        None => "main".to_owned(),
    };
    // Bare names for the script's own folder when it is below the root.
    let script_dir_prefix = script.and_then(|s| {
        let dir = std::fs::canonicalize(s).ok()?.parent()?.to_path_buf();
        let root_c = std::fs::canonicalize(root).ok()?;
        let rel = dir.strip_prefix(&root_c).ok()?;
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        (!rel.is_empty()).then(|| rel + "/")
    });
    // Load source before unrelated data, and the entry folder first. A large
    // checkout must not exhaust the VFS budget before its requested script.
    tree.sort_by_key(|(relative, _)| {
        let in_entry_folder = script_dir_prefix
            .as_ref()
            .map_or(!relative.contains('/'), |prefix| {
                relative.starts_with(prefix)
            });
        (!in_entry_folder, !is_python_path(Path::new(relative)))
    });
    let mut modules = Vec::new();
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut skipped = Vec::new();
    for (relative, path) in tree {
        checked_project_path(root, &relative, false)?;
        let size = std::fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .len();
        let is_py = is_python_path(Path::new(&relative));
        if size > VFS_FILE_LIMIT || (is_py && size > MODULE_LIMIT) {
            skipped.push(relative);
            continue;
        }
        if total + size > VFS_TOTAL_LIMIT {
            skipped.push(relative);
            continue;
        }
        // A file that cannot be read (gone, locked, unreadable) is left out.
        let mut bytes = Vec::new();
        let readable = std::fs::File::open(&path)
            .and_then(|f| f.take(VFS_FILE_LIMIT + 1).read_to_end(&mut bytes))
            .is_ok();
        if !readable {
            if is_py {
                skipped.push(relative);
            }
            continue;
        }
        if bytes.len() as u64 > VFS_FILE_LIMIT
            || total + bytes.len() as u64 > VFS_TOTAL_LIMIT
            || (is_py && bytes.len() as u64 > MODULE_LIMIT)
        {
            skipped.push(relative);
            continue;
        }
        total += bytes.len() as u64;
        if is_py {
            if let (Some(name), Ok(source)) =
                (module_name_of(&relative), std::str::from_utf8(&bytes))
            {
                modules.push((name, source.to_owned()));
                if let Some(prefix) = &script_dir_prefix {
                    if let Some(bare) = relative
                        .strip_prefix(prefix.as_str())
                        .and_then(module_name_of)
                    {
                        // Nested package aliases are intentional: the script folder is first.
                        modules.push((bare, source.to_owned()));
                    }
                }
            }
        }
        files.push((relative, bytes));
    }
    if !modules.iter().any(|(name, _)| *name == entry) {
        return Err(match script {
            Some(_) => format!("{}: not a UTF-8 Python file", entry),
            None => format!("{}: a project needs a main.py entry", root.display()),
        });
    }
    if !skipped.is_empty() {
        eprintln!(
            "zipp: {} file(s) left out of the project (over the size limits): {}",
            skipped.len(),
            skipped
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let mut compiled = compile_python_program(&entry, &modules, &files, argv, false)?;
    let state = compiled.state_mut();
    let outcome = state.run_init();
    for line in state.take_output() {
        println!("{line}");
    }
    for line in state.take_errput() {
        eprintln!("{line}");
    }
    // Files the program wrote go back to disk, inside the root only.
    if let Ok(zipp_vm::embed::JsValue::String(listing)) =
        state.call_global("__zipp_py_vfs_changed", &[])
    {
        write_project_changes(root, &listing)?;
    }
    outcome.map(|_| ())
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
        let mut tree = Vec::new();
        collect_tree(&root, &root, &mut tree).unwrap();
        assert_eq!(tree.len(), 1);
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
