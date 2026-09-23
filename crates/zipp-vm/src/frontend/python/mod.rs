//! Python frontend: the RustPython AST lowered to native ZIPP register bytecode.
//!
//! The fixed runtime (`runtime/*.js`) is trusted JavaScript compiled by ZIPP
//! and implements Python's object model — classes, exceptions, the numeric
//! tower, str/list/dict/set, generators, modules — as JavaScript values;
//! guest Python is never translated to JavaScript source. The emitter lowers
//! control flow, calls and name access directly to VM instructions and
//! everything else to runtime helper calls. A program is a PROJECT: one
//! module per `.py` file plus the name of the entry module; `import`
//! resolves project modules and the built-in modules at compile time.
use crate::bytecode::{Instr, Program};
use crate::vm::prof::{self, Phase};
use rustpython_parser::lexer::{LexResult, LexicalError, LexicalErrorType};
use rustpython_parser::{ast, lexer, Mode, Parse, Tok};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

mod emitter;
mod exprs;
mod nesting;
mod stmts;
mod symtable;

use emitter::{Emitter, Unit, MAX_FUNCTIONS};

type R<T> = Result<T, String>;
const RUNTIME_BASE: &str = concat!(
    include_str!("runtime/core.js"),
    "\n",
    include_str!("runtime/types.js"),
    "\n",
    include_str!("runtime/builtins.js"),
    "\n",
    include_str!("runtime/stdlib.js"),
    "\n",
    include_str!("runtime/tensor.js"),
);
const RUNTIME_ENTRY: &str = include_str!("runtime/entry.js");
#[cfg(feature = "python-js-interop")]
const INTEROP_RUNTIME: &str = include_str!("runtime/javascript.js");
#[cfg(not(feature = "python-js-interop"))]
const INTEROP_RUNTIME: &str = "";
const MAX_SOURCE: usize = 1024 * 1024;
const MAX_TOKENS: usize = 1 << 20;
const MAX_MODULES: usize = 256;
/// Modules the runtime provides itself; a project module of the same name
/// shadows it, as a script-directory module shadows the stdlib in CPython.
pub(super) const BUILTIN_MODULES: &[&str] = &[
    #[cfg(feature = "python-js-interop")]
    "javascript",
    #[cfg(feature = "python-js-interop")]
    "js",
    "ui",
    "math",
    "random",
    "time",
    "sys",
    "json",
    "os",
    "string",
    "itertools",
    "functools",
    "collections",
    "copy",
    "re",
    "typing",
    "abc",
    "__future__",
    "operator",
    "dataclasses",
    "enum",
    "io",
    "builtins",
    "textwrap",
    "heapq",
    "bisect",
    "statistics",
    "fractions",
    "struct",
    "contextlib",
    "hashlib",
    "platform",
    "importlib",
    "_zipp_gpu",
    "_zipp_tensor",
];

/// Library modules written in Python and bundled with the frontend. One is
/// compiled into a program only when a module of the program imports it
/// (directly or through another bundled module), so a program that never
/// mentions it pays nothing.
pub(super) const BUNDLED_MODULES: &[(&str, &str)] = &[
    ("zipp_gpu", include_str!("lib/shared/zipp_gpu.py")),
    ("torch", include_str!("lib/torch.py")),
    ("torch._gpu", include_str!("lib/torch_gpu.py")),
    ("torch.nn", include_str!("lib/torch_nn.py")),
    (
        "torch.nn.functional",
        include_str!("lib/torch_nn_functional.py"),
    ),
    ("torch.nn.init", include_str!("lib/torch_nn_init.py")),
    ("torch.nn.utils", include_str!("lib/torch_nn_utils.py")),
    ("torch.nn.utils.rnn", include_str!("lib/torch_nn_utils_rnn.py")),
    ("torch.nn.parameter", include_str!("lib/torch_nn_parameter.py")),
    ("torch.optim", include_str!("lib/torch_optim.py")),
    ("torch.optim.optimizer", include_str!("lib/torch_optim_optimizer.py")),
    (
        "torch.optim.lr_scheduler",
        include_str!("lib/torch_optim_lr_scheduler.py"),
    ),
    ("torch.utils", include_str!("lib/torch_utils.py")),
    ("torch.utils.data", include_str!("lib/torch_utils_data.py")),
    ("torch.autograd", include_str!("lib/torch_autograd.py")),
    ("torch._utils", include_str!("lib/torch__utils.py")),
    ("pickle", include_str!("lib/pickle.py")),
    ("zipfile", include_str!("lib/zipfile.py")),
    ("pathlib", include_str!("lib/pathlib.py")),
    ("argparse", include_str!("lib/argparse.py")),
    ("inspect", include_str!("lib/inspect.py")),
    ("pytest", include_str!("lib/pytest.py")),
];

/// A single-file program: the source is the `main` module.
pub(super) fn compile(source: &str) -> R<Program> {
    compile_project("main", None, &[("main".to_owned(), source)], &[], &[], false)
}

/// Bounds on the virtual filesystem a program is compiled with.
const MAX_VFS_FILE: usize = 8 * 1024 * 1024;
const MAX_VFS_TOTAL: usize = 64 * 1024 * 1024;

/// The head (first segment) of a dotted module name.
fn module_head(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// What the emitter knows about the whole project.
pub(super) struct Project<'a> {
    pub modules: BTreeSet<&'a str>,
    pub module_names: Vec<&'a str>,
    /// The module run as the program: its `__name__` (and so the
    /// `__module__` of what it defines) is `__main__`.
    pub entry: &'a str,
}

/// A multi-module program. `modules` pairs each module name (the `.py` file's
/// stem) with its source; `entry` names the module whose top level runs.
/// Where a relative import could count from, given the module it appears in.
/// A module that is itself a package counts from itself and a plain module
/// counts from its parent. Discovery does not yet know which, so it offers both
/// and lets the module set decide; only names that exist are ever queued.
fn relative_bases(here: &str, level: u32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for strip in [level.saturating_sub(1), level] {
        let mut base = here.to_owned();
        let mut ok = true;
        for _ in 0..strip {
            match base.rfind('.') {
                Some(cut) => base.truncate(cut),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && !base.is_empty() && !out.contains(&base) {
            out.push(base);
        }
    }
    out
}

/// The module names a suite imports anywhere in its statements.
fn imported_modules(suite: &[ast::Stmt], here: &str, out: &mut BTreeSet<String>) {
    for stmt in suite {
        match stmt {
            ast::Stmt::Import(i) => {
                for alias in &i.names {
                    out.insert(alias.name.as_str().to_owned());
                }
            }
            ast::Stmt::ImportFrom(i) => {
                let level = i.level.map_or(0, |level| level.to_u32());
                let bases = if level == 0 {
                    vec![String::new()]
                } else {
                    relative_bases(here, level)
                };
                for base in bases {
                    let full = match (&i.module, base.is_empty()) {
                        (Some(module), true) => module.as_str().to_owned(),
                        (Some(module), false) => format!("{base}.{}", module.as_str()),
                        (None, true) => continue,
                        (None, false) => base.clone(),
                    };
                    out.insert(full.clone());
                    // `from pkg import sub` may name a submodule.
                    for alias in &i.names {
                        out.insert(format!("{full}.{}", alias.name.as_str()));
                    }
                }
            }
            ast::Stmt::If(s) => {
                // An `elif` ladder is walked iteratively.
                let mut arm = s;
                loop {
                    imported_modules(&arm.body, here, out);
                    match arm.orelse.as_slice() {
                        [ast::Stmt::If(next)] => arm = next,
                        orelse => {
                            imported_modules(orelse, here, out);
                            break;
                        }
                    }
                }
            }
            ast::Stmt::For(s) => {
                imported_modules(&s.body, here, out);
                imported_modules(&s.orelse, here, out);
            }
            ast::Stmt::While(s) => {
                imported_modules(&s.body, here, out);
                imported_modules(&s.orelse, here, out);
            }
            ast::Stmt::With(s) => imported_modules(&s.body, here, out),
            ast::Stmt::Try(s) => {
                imported_modules(&s.body, here, out);
                for handler in &s.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    imported_modules(&h.body, here, out);
                }
                imported_modules(&s.orelse, here, out);
                imported_modules(&s.finalbody, here, out);
            }
            ast::Stmt::FunctionDef(s) => imported_modules(&s.body, here, out),
            ast::Stmt::ClassDef(s) => imported_modules(&s.body, here, out),
            ast::Stmt::Match(s) => {
                for case in &s.cases {
                    imported_modules(&case.body, here, out);
                }
            }
            _ => {}
        }
    }
}

/// A project: `modules` are the candidate `.py` files by dotted module name
/// (`legacy.fast_memory` for `legacy/fast_memory.py`, `pkg` for
/// `pkg/__init__.py`); only the ones reachable from `entry` through imports
/// are compiled, plus the bundled library modules those import. `files` is
/// the virtual filesystem the program sees (paths relative to its root),
/// `argv` becomes `sys.argv[1:]`, and `hosted` tells the runtime that an
/// embedder will drain and answer host requests (the wasm engine does);
/// otherwise requests are settled locally. `entry_file` labels the entry
/// module (its `__file__`, `sys.argv[0]` and traceback frames) when it is
/// not the file its module name implies: a script run from a subfolder, or
/// one whose file name is not a module name (`my-script.py`, `tool`).
pub(super) fn compile_project<S: AsRef<str>>(
    entry: &str,
    entry_file: Option<&str>,
    modules: &[(String, S)],
    files: &[(String, Vec<u8>)],
    argv: &[String],
    hosted: bool,
) -> R<Program> {
    let _phase = prof::enter(Phase::PyFrontend);
    if modules.is_empty() {
        return Err("Python project: no modules".into());
    }
    if modules.len() > MAX_MODULES {
        return Err(format!("Python project: more than {MAX_MODULES} modules"));
    }
    let mut candidates: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, source) in modules {
        if !is_module_name(name) {
            return Err(format!(
                "Python project: {name:?} is not a valid module name (dotted identifiers only)"
            ));
        }
        if candidates.insert(name.as_str(), source.as_ref()).is_some() {
            return Err(format!("Python project: duplicate module {name:?}"));
        }
    }
    if !candidates.contains_key(entry) {
        return Err(format!(
            "Python project: entry module {entry:?} is not in the project"
        ));
    }
    let mut total = 0usize;
    for (path, bytes) in files {
        if bytes.len() > MAX_VFS_FILE {
            return Err(format!("Python project: file {path:?} exceeds 8 MiB"));
        }
        total += bytes.len();
        if total > MAX_VFS_TOTAL {
            return Err("Python project: files exceed 64 MiB in total".into());
        }
    }
    // Breadth-first from the entry: a module is compiled only when something
    // imports it, so an unrelated file with a syntax error costs nothing.
    // Bundled library modules come in by head name (`import torch.nn` brings
    // every `torch.*` module); a project module of the same name shadows them.
    // Sources are borrowed from `modules` or the bundled library, never
    // copied per module.
    let mut sources: Vec<(String, &str)> = Vec::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut parsed = Vec::new();
    let mut queue: Vec<String> = vec![entry.to_owned()];
    let mut queued: BTreeSet<String> = BTreeSet::new();
    queued.insert(entry.to_owned());
    // A `test_*.py` entry runs its tests through the bundled pytest, so that
    // module comes along even when the file never imports it.
    let entry_base = match entry_file {
        Some(file) => {
            let base = file.rsplit('/').next().unwrap_or(file);
            base.rsplit_once('.').map_or(base, |(stem, _)| stem)
        }
        None => entry.rsplit('.').next().unwrap_or(entry),
    };
    if entry_base.starts_with("test_") || entry_base.ends_with("_test") {
        queued.insert("pytest".to_owned());
        queue.push("pytest".to_owned());
    }
    while let Some(name) = queue.first().cloned() {
        queue.remove(0);
        let source: &str = match candidates.get(name.as_str()) {
            Some(s) => s,
            None => match BUNDLED_MODULES.iter().find(|(n, _)| *n == name) {
                Some((_, text)) => text,
                None => continue,
            },
        };
        let file = match entry_file {
            Some(label) if name == entry => label.to_owned(),
            _ => format!("{}.py", name.replace('.', "/")),
        };
        let suite = parse_module(&file, source)?;
        // Bound the tree's nesting before the recursive walks below.
        let text: &str = source.strip_prefix('\u{feff}').unwrap_or(source);
        let suite = nesting::check(suite, &file, text)?;
        let table = symtable::analyse(&suite, &name).map_err(|e| format!("{file}: {e}"))?;
        let mut wanted = BTreeSet::new();
        imported_modules(&suite, &name, &mut wanted);
        let index = sources.len();
        names.insert(name.clone());
        sources.push((name.clone(), source));
        parsed.push((index, file, suite, table));
        if sources.len() > MAX_MODULES {
            return Err(format!("Python project: more than {MAX_MODULES} modules"));
        }
        for imported in wanted {
            // `a.b.c` also needs the packages `a` and `a.b` when they exist.
            let mut prefix = String::new();
            for segment in imported.split('.') {
                if !prefix.is_empty() {
                    prefix.push('.');
                }
                prefix.push_str(segment);
                if candidates.contains_key(prefix.as_str()) && queued.insert(prefix.clone()) {
                    queue.push(prefix.clone());
                }
            }
            let head = module_head(&imported);
            if candidates.contains_key(head) {
                continue;
            }
            for (bundled, _) in BUNDLED_MODULES {
                if module_head(bundled) == head && queued.insert((*bundled).to_owned()) {
                    queue.push((*bundled).to_owned());
                }
            }
        }
    }
    let sources = &sources;
    let names: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let module_names: Vec<&str> = sources.iter().map(|(n, _)| n.as_str()).collect();
    let parsed: Vec<_> = parsed
        .into_iter()
        .map(|(index, file, suite, table)| {
            let (name, source) = &sources[index];
            let source: &str = source.strip_prefix('\u{feff}').unwrap_or(*source);
            let lines = emitter::line_starts(source);
            (name.as_str(), file, source, suite, table, lines)
        })
        .collect();
    // A fixed seed program. Its root initializes the private runtime and then
    // calls __zipp_py_entry. Replace only that empty function's prototype;
    // retain all seed globals and function IDs. Never relocate JS bytecode.
    let RuntimeSeed {
        program,
        entry_fn,
        rt_slot,
        line_slot,
    } = runtime_seed()?;
    let name_global = program.functions[entry_fn].name_global;
    let project = Project {
        modules: names,
        module_names,
        entry,
    };
    let program = RefCell::new(program);
    // Each module's top level becomes a code object the runtime runs on first
    // import; the entry body registers them all, names the entry module, then
    // imports it.
    let mut inits = Vec::with_capacity(parsed.len());
    for (index, (name, file, source, suite, table, lines)) in parsed.iter().enumerate() {
        if program.borrow().functions.len() >= MAX_FUNCTIONS {
            return Err("Python project: function count limit exceeded".into());
        }
        let unit = Unit {
            file,
            source,
            module_index: index as u32,
            lines,
        };
        let mut out = Emitter::new(
            &format!("<module {name}>"),
            unit,
            rt_slot,
            line_slot,
            table,
            0,
            &project,
            &program,
            "<module>".to_owned(),
        )?;
        out.future_annotations = suite.iter().any(|s| match s {
            ast::Stmt::ImportFrom(i) => {
                i.module
                    .as_ref()
                    .is_some_and(|m| m.as_str() == "__future__")
                    && i.names.iter().any(|a| a.name.as_str() == "annotations")
            }
            _ => false,
        });
        out.hoist_ints(&suite)?;
        out.frame_guard(1, |out| {
            out.suite(suite, 0)?;
            let value = out.none()?;
            out.emit(Instr::Return { src: value })?;
            Ok(())
        })?;
        let mut p = program.borrow_mut();
        let func_id = p.functions.len() as u32;
        p.functions.push(out.finish());
        inits.push((*name, file.as_str(), func_id));
    }
    // The entry: plain JS-shaped bytecode with no Python scope, built by hand.
    let entry_table = symtable::analyse(&[], "<entry>")?;
    let unit = Unit {
        file: "<entry>",
        source: "",
        module_index: 0,
        lines: &[0],
    };
    let mut out = Emitter::new(
        "__zipp_py_entry",
        unit,
        rt_slot,
        line_slot,
        &entry_table,
        0,
        &project,
        &program,
        "<entry>".to_owned(),
    )?;
    // Registers are reclaimed after every call: the entry's register count
    // must not grow with the number of modules or files.
    for (name, file, func_id) in inits {
        let mark = out.mark();
        let code = out.alloc()?;
        out.emit(Instr::MakeFunc { dst: code, func_id })?;
        let name_r = out.string(name)?;
        let file_r = out.string(file)?;
        out.helper("module", &[name_r, code, file_r])?;
        out.release(mark);
    }
    let name = out.string(entry)?;
    out.helper("entry", &[name])?;
    let hosted_r = out.boolean(hosted)?;
    out.helper("hosted", &[hosted_r])?;
    // The virtual filesystem: each file's bytes as a base64 string constant,
    // which the runtime decodes natively (`Uint8Array.fromBase64`) rather
    // than copying byte by byte in interpreted code. Paths and contents are
    // distinct constants, never searched for a duplicate.
    for (path, bytes) in files {
        let mark = out.mark();
        let path_r = out.unique_string(path.clone())?;
        let data_r = out.unique_string(base64(bytes))?;
        out.helper("vfs", &[path_r, data_r])?;
        out.release(mark);
    }
    let mut argv_regs = Vec::with_capacity(argv.len());
    for arg in argv {
        argv_regs.push(out.string(arg)?);
    }
    let argv_r = out.array(&argv_regs)?;
    out.helper("argv", &[argv_r])?;
    let name = out.string(entry)?;
    out.helper("runmain", &[name])?;
    let value = out.none()?;
    out.emit(Instr::Return { src: value })?;
    let mut proto = out.finish();
    proto.name_global = name_global;
    let mut program = program.into_inner();
    program.functions[entry_fn] = proto;
    Ok(program)
}

fn is_module_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 200 {
        return false;
    }
    name.split('.').all(|segment| {
        let mut chars = segment.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Parse one module under the conservative compiler limits (additional to
/// host execution limits). The limits are counted on the parser's own token
/// stream, so the source is lexed once: the counter ends the stream as soon
/// as a limit is crossed, before the parser sees a deeper token.
fn parse_module(file: &str, source: &str) -> R<Vec<ast::Stmt>> {
    if source.len() > MAX_SOURCE {
        return Err(format!("Python: {file} exceeds 1 MiB"));
    }
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let exceeded = Cell::new(false);
    let (mut count, mut brackets, mut indent) = (0usize, 0usize, 0usize);
    let tokens = lexer::lex(source, Mode::Module).map_while(|item: LexResult| {
        if exceeded.get() {
            return None;
        }
        if let Ok((token, range)) = &item {
            count += 1;
            match token {
                Tok::Indent => indent += 1,
                Tok::Dedent => indent = indent.saturating_sub(1),
                Tok::Lpar | Tok::Lsqb | Tok::Lbrace => brackets += 1,
                Tok::Rpar | Tok::Rsqb | Tok::Rbrace => brackets = brackets.saturating_sub(1),
                _ => {}
            }
            if count > MAX_TOKENS || brackets > 200 || indent > 100 {
                exceeded.set(true);
                return Some(Err(LexicalError::new(
                    LexicalErrorType::OtherError("compiler complexity limit".into()),
                    range.start(),
                )));
            }
        }
        Some(item)
    });
    let parsed = ast::Suite::parse_tokens(tokens, file);
    if exceeded.get() {
        return Err(format!(
            "Python: compiler complexity limit exceeded in {file}"
        ));
    }
    parsed.map_err(|e| {
        format!(
            "SyntaxError: {} ({})",
            e.error,
            position(file, source, e.offset)
        )
    })
}

/// The compiled runtime seed and the indices the frontend patches into it.
#[derive(Clone)]
struct RuntimeSeed {
    program: Program,
    entry_fn: usize,
    rt_slot: u32,
    line_slot: u32,
}

/// Whether [`runtime_seed`] keeps the compiled runtime for later programs.
static SEED_MEMO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// See [`super::set_python_runtime_memo`].
pub(super) fn set_runtime_memo(enabled: bool) {
    SEED_MEMO.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// The runtime seed, memoized per process and cloned per program. Parsing
/// and compiling ~420 KB of runtime JavaScript is most of a small program's
/// compile time; its output depends only on this build's features, the
/// process-latched compiler switches (read once from the environment) and
/// the script-goal latch, which keys the memo. A `Program` holds no heap
/// values (string constants are resolved per VM), so every program built
/// from one seed is independent. With the memo off (a process that compiles
/// one program) the seed is handed over without keeping a copy.
fn runtime_seed() -> R<RuntimeSeed> {
    use std::sync::OnceLock;
    static SEEDS: [OnceLock<Result<RuntimeSeed, String>>; 2] = [OnceLock::new(), OnceLock::new()];
    let seed = &SEEDS[crate::front::pure_script_goal() as usize];
    if let Some(seed) = seed.get() {
        return seed.clone();
    }
    if !SEED_MEMO.load(std::sync::atomic::Ordering::Relaxed) {
        return compile_runtime_seed();
    }
    seed.get_or_init(compile_runtime_seed).clone()
}

fn compile_runtime_seed() -> R<RuntimeSeed> {
    let runtime = format!("{RUNTIME_BASE}\n{INTEROP_RUNTIME}\n{RUNTIME_ENTRY}");
    let mut program = crate::compile_only(&runtime, false)?;
    // Only this program — the Python runtime every Python state is built
    // from — gets the native tensor loops (`vm::py_tensor`).
    program.python_natives = true;
    let entry_fn = program
        .functions
        .iter()
        .position(|f| f.name == "__zipp_py_entry")
        .ok_or("Python bootstrap entry not found; incompatible JS compiler")?;
    let global_slot = |name: &str| -> R<u32> {
        program
            .global_names
            .iter()
            .position(|n| n == name)
            .map(|i| i as u32)
            .ok_or_else(|| format!("Python bootstrap global {name} not found"))
    };
    let rt_slot = global_slot("__zipp_py")?;
    let line_slot = global_slot("__zipp_py_line")?;
    Ok(RuntimeSeed {
        program,
        entry_fn,
        rt_slot,
        line_slot,
    })
}

/// Standard padded base64, the form `Uint8Array.fromBase64` decodes.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= chunk.len() {
                TABLE[(n >> shift) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

/// `file:line:col` (1-based, columns in characters) of a byte offset in `source`.
pub(super) fn position(
    file: &str,
    source: &str,
    offset: rustpython_parser::text_size::TextSize,
) -> String {
    let offset = u32::from(offset) as usize;
    let prefix = source.get(..offset).unwrap_or(source);
    let line = prefix.bytes().filter(|c| *c == b'\n').count() + 1;
    let col = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("{file}:{line}:{col}")
}
