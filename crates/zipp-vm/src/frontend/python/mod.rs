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
use crate::bytecode::{Instr, Program, PyLazy, PyLazyModule};
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
#[cfg(test)]
mod library_check;
#[cfg(test)]
mod minify_check;

use emitter::{Emitter, Unit, MAX_FUNCTIONS};

type R<T> = Result<T, String>;
/// A frontend source file as embedded: the comment-stripped copy `build.rs`
/// writes (every token keeps its line and column; see build/minify.rs).
macro_rules! pysrc {
    ($path:literal) => {
        include_str!(concat!(env!("OUT_DIR"), "/pysrc/", $path))
    };
}
const RUNTIME_BASE: &str = concat!(
    pysrc!("runtime/core.js"),
    "\n",
    pysrc!("runtime/types.js"),
    "\n",
    pysrc!("runtime/builtins.js"),
    "\n",
    pysrc!("runtime/stdlib.js"),
    "\n",
    pysrc!("runtime/tensor.js"),
);
const RUNTIME_ENTRY: &str = pysrc!("runtime/entry.js");
// The native CLI's synchronous GPU bridge (`_zipp_gpu.native`): inert
// unless the embedder answers `zipp.gpu.*` host calls. Only the native CLI
// enables it, so the WebAssembly and sandbox runtimes are unchanged.
#[cfg(feature = "python-native-gpu")]
const NATIVE_GPU_RUNTIME: &str = pysrc!("runtime/native_gpu.js");
#[cfg(not(feature = "python-native-gpu"))]
const NATIVE_GPU_RUNTIME: &str = "";
#[cfg(feature = "python-js-interop")]
const INTEROP_RUNTIME: &str = pysrc!("runtime/javascript.js");
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

/// A library module written in Python and bundled with the frontend
/// (`lib/modules.txt`). Every one a project does not shadow is registered
/// by name; one compiles only when it is first imported (`vm::py_lazy`), so
/// a program pays nothing for the modules it never imports. Its import
/// statements, read at build time (build/pyimports.rs), let a project's
/// imports be followed through it without compiling it.
pub(super) struct Bundled {
    pub name: &'static str,
    pub source: &'static str,
    /// Its import statements: `;` between statements, each `i|a.b,c`
    /// (`import a.b, c`) or `<level>|<module>|x,y` (`from ..m import x, y`).
    pub imports: &'static str,
    /// The file under `lib/`.
    #[cfg(test)]
    pub file: &'static str,
    /// The package it ships in: `base` or `torch`.
    #[cfg(test)]
    pub package: &'static str,
}

pub(super) const BUNDLED_MODULES: &[Bundled] = include!(concat!(env!("OUT_DIR"), "/bundled.rs"));

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

/// Queue what `wanted` names: project modules (and the packages above
/// them), and every library module of a head no project module shadows.
fn follow(
    wanted: &BTreeSet<String>,
    candidates: &BTreeMap<&str, &str>,
    queued: &mut BTreeSet<String>,
    queue: &mut Vec<String>,
    heads: &mut BTreeSet<String>,
) {
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
        let head = module_head(imported);
        if candidates.contains_key(head) || !heads.insert(head.to_owned()) {
            continue;
        }
        for bundled in BUNDLED_MODULES {
            if module_head(bundled.name) == head && queued.insert(bundled.name.to_owned()) {
                queue.push(bundled.name.to_owned());
            }
        }
    }
}

/// [`imported_modules`] of a bundled module, from its import statements
/// ([`Bundled::imports`]).
fn bundled_imports(imports: &str, here: &str, out: &mut BTreeSet<String>) {
    for stmt in imports.split(';').filter(|s| !s.is_empty()) {
        let mut fields = stmt.split('|');
        let kind = fields.next().unwrap_or("");
        if kind == "i" {
            for name in fields.next().unwrap_or("").split(',').filter(|n| !n.is_empty()) {
                out.insert(name.to_owned());
            }
            continue;
        }
        let level: u32 = kind.parse().unwrap_or(0);
        let module = fields.next().filter(|m| !m.is_empty());
        let names = fields.next().unwrap_or("");
        let bases = if level == 0 {
            vec![String::new()]
        } else {
            relative_bases(here, level)
        };
        for base in bases {
            let full = match (module, base.is_empty()) {
                (Some(module), true) => module.to_owned(),
                (Some(module), false) => format!("{base}.{module}"),
                (None, true) => continue,
                (None, false) => base.clone(),
            };
            out.insert(full.clone());
            for name in names.split(',').filter(|n| !n.is_empty()) {
                out.insert(format!("{full}.{name}"));
            }
        }
    }
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
    // every `torch.*` module) and are followed through their import lists
    // but not compiled here: every one the project does not shadow is
    // registered by name and compiles on first import (see `lazy_modules`).
    // Sources are borrowed from `modules`, never copied per module.
    let mut sources: Vec<(String, &str)> = Vec::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut parsed = Vec::new();
    let mut queue: Vec<String> = vec![entry.to_owned()];
    let mut queued: BTreeSet<String> = BTreeSet::new();
    // The heads whose library modules are queued already.
    let mut heads: BTreeSet<String> = BTreeSet::new();
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
            None => {
                // A library module is not compiled here, but what it imports
                // is followed exactly as if it were: a project module only it
                // imports is still the project's (as a script-directory
                // module shadows the stdlib for every importer in CPython).
                if let Some(module) = BUNDLED_MODULES.iter().find(|m| m.name == name) {
                    let mut wanted = BTreeSet::new();
                    bundled_imports(module.imports, &name, &mut wanted);
                    follow(&wanted, &candidates, &mut queued, &mut queue, &mut heads);
                }
                continue;
            }
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
        follow(&wanted, &candidates, &mut queued, &mut queue, &mut heads);
    }
    let sources = &sources;
    // Module indices (the traceback file index): the library modules first,
    // in their fixed order, so a library module's index, and with it its
    // compiled code, is the same whatever project imports it; then the
    // project's modules in the order found.
    let lazy = lazy_modules(&candidates);
    let mut names: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let mut module_names: Vec<&str> = lazy.iter().map(|(name, _)| *name).collect();
    module_names.extend(sources.iter().map(|(n, _)| n.as_str()));
    for (name, _) in &lazy {
        names.insert(name);
    }
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
            module_index: (lazy.len() + index) as u32,
            lines,
        };
        let func_id = emit_module(name, unit, suite, table, &project, &program, rt_slot, line_slot)?;
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
    //
    // The library modules first (module indices from 0), named in one call:
    // the runtime makes a module's record when it is first looked up and
    // compiles it when it is first imported (`__zipp_py_compile`). Their
    // files are `lazy_file`'s.
    if !lazy.is_empty() {
        let mark = out.mark();
        let names: Vec<&str> = lazy.iter().map(|(name, _)| *name).collect();
        let names_r = out.string(&names.join("\n"))?;
        let count = i32::try_from(names.len()).map_err(|_| "Python project: too many library modules")?;
        let count_r = out.small_int(count)?;
        out.helper("lazy", &[names_r, count_r])?;
        out.release(mark);
    }
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
    program.python_lazy = Some(Box::new(PyLazy {
        compile: compile_lazy,
        names: project.module_names.iter().map(|n| (*n).to_owned()).collect(),
        entry: entry.to_owned(),
        rt_slot,
        line_slot,
        modules: lazy
            .iter()
            .map(|&(name, source)| PyLazyModule {
                name: name.to_owned(),
                file: lazy_file(name),
                source,
                installed: std::sync::OnceLock::new(),
            })
            .collect(),
    }));
    Ok(program)
}

/// One module's top level as a code object appended to `program`: the
/// same code whether the module is compiled with the program or on first
/// import. Returns its function id.
#[allow(clippy::too_many_arguments)]
fn emit_module<'a>(
    name: &str,
    unit: Unit<'a>,
    suite: &[ast::Stmt],
    table: &'a symtable::SymTable,
    project: &'a Project<'a>,
    program: &'a RefCell<Program>,
    rt_slot: u32,
    line_slot: u32,
) -> R<u32> {
    let mut out = Emitter::new(
        &format!("<module {name}>"),
        unit,
        rt_slot,
        line_slot,
        table,
        0,
        project,
        program,
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
    out.hoist_ints(suite)?;
    out.frame_guard(1, |out| {
        out.suite(suite, 0)?;
        let value = out.none()?;
        out.emit(Instr::Return { src: value })?;
        Ok(())
    })?;
    let mut p = program.borrow_mut();
    let func_id = p.functions.len() as u32;
    p.functions.push(out.finish());
    Ok(func_id)
}

/// The library modules a project may import, compiled on first import: every
/// bundled module whose head the project does not shadow (a project module
/// `torch` hides all of `torch.*`, as a script-directory package hides an
/// installed one) and that the project does not define itself.
fn lazy_modules(candidates: &BTreeMap<&str, &str>) -> Vec<(&'static str, &'static str)> {
    BUNDLED_MODULES
        .iter()
        .filter(|m| !candidates.contains_key(module_head(m.name)) && !candidates.contains_key(m.name))
        .map(|m| (m.name, m.source))
        .collect()
}

/// The file a library module's tracebacks name: `torch/nn.py` (the runtime's
/// `R.lazy` derives the same).
fn lazy_file(name: &str) -> String {
    format!("{}.py", name.replace('.', "/"))
}

/// [`PyLazy::compile`]: module `k` of `lazy` (module index `k`), compiled
/// against the program's module names exactly as `compile_project` compiles
/// an eager module, with function ids from `base`. Compiled modules are kept
/// per process (when the runtime seed is, see [`set_runtime_memo`]), keyed on
/// everything their code depends on; each program gets its own copy.
fn compile_lazy(lazy: &PyLazy, k: usize, base: u32) -> R<Vec<crate::bytecode::FuncProto>> {
    use crate::bytecode::FuncProto;
    use std::collections::HashMap;
    use std::fmt::Write;
    use std::sync::{Arc, Mutex};
    static CACHE: Mutex<Option<HashMap<String, Arc<Vec<FuncProto>>>>> = Mutex::new(None);
    const CACHE_MAX: usize = 1024;
    let module = lazy.modules.get(k).ok_or("Python: no such library module")?;
    let module_index = k;
    let memo = SEED_MEMO.load(std::sync::atomic::Ordering::Relaxed);
    // The module's code depends on the program only through its module
    // index (`k`: library modules come first), the library's module names,
    // the seed's slots and the project modules that could answer one of its
    // imports: those sharing a head with a name it imports, or with itself
    // (`Emitter::module_exists`, `package_of`). So projects that do not
    // define such modules share one compile.
    let key = if memo {
        let mut heads = BTreeSet::new();
        if let Some(bundled) = BUNDLED_MODULES.iter().find(|m| m.name == module.name) {
            bundled_imports(bundled.imports, bundled.name, &mut heads);
        }
        let mut heads: BTreeSet<&str> = heads.iter().map(|m| module_head(m)).collect();
        heads.insert(module_head(&module.name));
        let mut relevant: Vec<&str> = lazy.names[lazy.modules.len()..]
            .iter()
            .map(String::as_str)
            .filter(|name| heads.contains(module_head(name)))
            .collect();
        relevant.sort_unstable();
        let mut key = format!(
            "{}|{module_index}|{}|{}|{}|",
            module.name,
            lazy.rt_slot,
            lazy.line_slot,
            crate::front::pure_script_goal()
        );
        for name in &lazy.names[..lazy.modules.len()] {
            let _ = write!(key, "{name},");
        }
        key.push('|');
        for name in relevant {
            let _ = write!(key, "{name},");
        }
        Some(key)
    } else {
        None
    };
    let cached = key
        .as_ref()
        .and_then(|key| CACHE.lock().ok().and_then(|c| c.as_ref().and_then(|c| c.get(key).cloned())));
    let functions = match cached {
        Some(functions) => functions,
        None => {
            let _phase = prof::enter(Phase::PyFrontend);
            let name = module.name.as_str();
            let file = module.file.as_str();
            let suite = parse_module(file, module.source)?;
            let source: &str = module.source.strip_prefix('\u{feff}').unwrap_or(module.source);
            let suite = nesting::check(suite, file, source)?;
            let table = symtable::analyse(&suite, name).map_err(|e| format!("{file}: {e}"))?;
            let lines = emitter::line_starts(source);
            let project = Project {
                modules: lazy.names.iter().map(String::as_str).collect(),
                module_names: lazy.names.iter().map(String::as_str).collect(),
                entry: &lazy.entry,
            };
            let mut fragment = crate::compile_only("", false)?;
            fragment.functions.clear();
            let fragment = RefCell::new(fragment);
            let unit = Unit {
                file,
                source,
                module_index: module_index as u32,
                lines: &lines,
            };
            emit_module(name, unit, &suite, &table, &project, &fragment, lazy.rt_slot, lazy.line_slot)?;
            let fragment = fragment.into_inner();
            relocatable(name, &fragment, lazy.rt_slot, lazy.line_slot)?;
            let functions = Arc::new(fragment.functions);
            if let Some(key) = key {
                if let Ok(mut cache) = CACHE.lock() {
                    let cache = cache.get_or_insert_with(HashMap::new);
                    if cache.len() >= CACHE_MAX {
                        cache.clear();
                    }
                    cache.insert(key, functions.clone());
                }
            }
            functions
        }
    };
    if (base as usize).saturating_add(functions.len()) > MAX_FUNCTIONS {
        return Err(format!("Python: importing {}: function count limit exceeded", module.name));
    }
    let mut functions = (*functions).clone();
    for f in &mut functions {
        for ins in &mut f.code {
            if let Instr::MakeFunc { func_id, .. } = ins {
                *func_id += base;
            }
        }
    }
    Ok(functions)
}

/// Whether a module compiled for installation (`compile_lazy`) is code that
/// only `MakeFunc` ids tie to its place in the program: the relocation
/// `compile_lazy` does. Anything else carrying a function, class or global
/// slot id (what `prepare_eval_program` remaps for `eval`) would run against
/// the wrong one, so it is refused loudly, never installed. A constraint on
/// the Python emitter, which today emits none of it.
fn relocatable(name: &str, fragment: &Program, rt_slot: u32, line_slot: u32) -> R<()> {
    if !fragment.classes.is_empty() {
        return Err(format!("Python: library module {name} defines class records compile-on-import cannot relocate"));
    }
    for f in &fragment.functions {
        let movable = f.name_global.is_none()
            && f.code.iter().all(|ins| match ins {
                Instr::LoadGlobal { idx, .. } => *idx == rt_slot || *idx == line_slot,
                Instr::MakeClosure { .. }
                | Instr::MakeArrow { .. }
                | Instr::ClassAddMember { .. }
                | Instr::MakeClass { .. }
                | Instr::LoadClassValue { .. }
                | Instr::LoadGlobalOrUndefined { .. }
                | Instr::StoreGlobal { .. }
                | Instr::StoreGlobalStrict { .. }
                | Instr::StoreGlobalResolved { .. }
                | Instr::LoadGlobalDyn { .. }
                | Instr::LoadGlobalOrUndefinedDyn { .. }
                | Instr::StoreGlobalDyn { .. }
                | Instr::EvalScopeHas { .. }
                | Instr::EvalScopeSet { .. }
                | Instr::DeleteGlobal { .. }
                | Instr::MathOp { .. }
                | Instr::LoadUpvalDyn { .. }
                | Instr::StoreUpvalDyn { .. }
                | Instr::SuperCtorFetch { .. }
                | Instr::SuperCtor { .. }
                | Instr::SuperCtorSpread { .. }
                | Instr::SuperBase { .. }
                | Instr::SuperMethod { .. }
                | Instr::SuperGet { .. }
                | Instr::SuperGetComputed { .. }
                | Instr::SuperMethodComputed { .. }
                | Instr::SuperMethodSpread { .. }
                | Instr::SuperMethodComputedSpread { .. }
                | Instr::SuperSet { .. }
                | Instr::SuperSetComputed { .. }
                | Instr::DirectEval { .. }
                | Instr::FieldInit { .. }
                | Instr::DecKey { .. }
                | Instr::DecElem { .. }
                | Instr::DecInits { .. }
                | Instr::DecField { .. } => false,
                _ => true,
            });
        if !movable {
            return Err(format!(
                "Python: library module {name} uses code compile-on-import cannot relocate (in {})",
                f.name
            ));
        }
    }
    Ok(())
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
    let runtime =
        format!("{RUNTIME_BASE}\n{NATIVE_GPU_RUNTIME}\n{INTEROP_RUNTIME}\n{RUNTIME_ENTRY}");
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
