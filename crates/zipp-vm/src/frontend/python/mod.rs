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
use rustpython_parser::{ast, lexer, Mode, Parse, Tok};
use std::cell::RefCell;
use std::collections::BTreeSet;

mod emitter;
mod exprs;
mod stmts;
mod symtable;

use emitter::{Emitter, Unit, MAX_FUNCTIONS};

type R<T> = Result<T, String>;
const RUNTIME: &str = concat!(
    include_str!("runtime/core.js"),
    "\n",
    include_str!("runtime/types.js"),
    "\n",
    include_str!("runtime/builtins.js"),
    "\n",
    include_str!("runtime/stdlib.js"),
    "\n",
    include_str!("runtime/entry.js"),
);
const MAX_SOURCE: usize = 1024 * 1024;
const MAX_TOKENS: usize = 1 << 20;
const MAX_MODULES: usize = 256;
/// Modules the runtime provides itself; a project module of the same name
/// shadows it, as a script-directory module shadows the stdlib in CPython.
pub(super) const BUILTIN_MODULES: &[&str] = &[
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
    "_zipp_gpu",
];

/// Library modules written in Python and bundled with the frontend. One is
/// compiled into a program only when a module of the program imports it
/// (directly or through another bundled module), so a program that never
/// mentions it pays nothing.
pub(super) const BUNDLED_MODULES: &[(&str, &str)] = &[("zipp_gpu", include_str!("lib/zipp_gpu.py"))];

/// A single-file program: the source is the `main` module.
pub(super) fn compile(source: &str) -> R<Program> {
    compile_project("main", &[("main".to_owned(), source.to_owned())], false)
}

/// What the emitter knows about the whole project.
pub(super) struct Project<'a> {
    pub modules: BTreeSet<&'a str>,
    pub module_names: Vec<&'a str>,
}

/// A multi-module program. `modules` pairs each module name (the `.py` file's
/// stem) with its source; `entry` names the module whose top level runs.
/// The module names a suite imports anywhere in its statements.
fn imported_modules(suite: &[ast::Stmt], out: &mut BTreeSet<String>) {
    for stmt in suite {
        match stmt {
            ast::Stmt::Import(i) => {
                for alias in &i.names {
                    let head = alias.name.as_str().split('.').next().unwrap_or("");
                    out.insert(head.to_owned());
                }
            }
            ast::Stmt::ImportFrom(i) => {
                if let Some(module) = &i.module {
                    let head = module.as_str().split('.').next().unwrap_or("");
                    out.insert(head.to_owned());
                }
            }
            ast::Stmt::If(s) => {
                imported_modules(&s.body, out);
                imported_modules(&s.orelse, out);
            }
            ast::Stmt::For(s) => {
                imported_modules(&s.body, out);
                imported_modules(&s.orelse, out);
            }
            ast::Stmt::While(s) => {
                imported_modules(&s.body, out);
                imported_modules(&s.orelse, out);
            }
            ast::Stmt::With(s) => imported_modules(&s.body, out),
            ast::Stmt::Try(s) => {
                imported_modules(&s.body, out);
                for handler in &s.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    imported_modules(&h.body, out);
                }
                imported_modules(&s.orelse, out);
                imported_modules(&s.finalbody, out);
            }
            ast::Stmt::FunctionDef(s) => imported_modules(&s.body, out),
            ast::Stmt::ClassDef(s) => imported_modules(&s.body, out),
            ast::Stmt::Match(s) => {
                for case in &s.cases {
                    imported_modules(&case.body, out);
                }
            }
            _ => {}
        }
    }
}

/// `hosted` tells the runtime that an embedder will drain and answer host
/// requests (the wasm engine does); otherwise requests are settled locally.
pub(super) fn compile_project(
    entry: &str,
    modules: &[(String, String)],
    hosted: bool,
) -> R<Program> {
    if modules.is_empty() {
        return Err("Python project: no modules".into());
    }
    if modules.len() > MAX_MODULES {
        return Err(format!("Python project: more than {MAX_MODULES} modules"));
    }
    let mut names = BTreeSet::new();
    let mut module_names = Vec::new();
    for (name, _) in modules {
        if !is_module_name(name) {
            return Err(format!(
                "Python project: {name:?} is not a valid module name (letters, digits and _ only)"
            ));
        }
        if !names.insert(name.as_str()) {
            return Err(format!("Python project: duplicate module {name:?}"));
        }
        module_names.push(name.as_str());
    }
    if !names.contains(entry) {
        return Err(format!(
            "Python project: entry module {entry:?} is not in the project"
        ));
    }
    // Project modules first, then any bundled library module they import
    // (a project module of the same name shadows the bundled one).
    let mut sources: Vec<(String, String)> = modules.to_vec();
    let mut parsed = Vec::with_capacity(modules.len());
    let mut wanted = BTreeSet::new();
    let mut index = 0;
    while index < sources.len() {
        let (name, source) = &sources[index];
        let file = format!("{name}.py");
        preflight(&file, source)?;
        let source: &str = source.strip_prefix('\u{feff}').unwrap_or(source);
        let suite = ast::Suite::parse(source, &file).map_err(|e| {
            format!(
                "SyntaxError: {} ({})",
                e.error,
                position(&file, source, e.offset)
            )
        })?;
        let table = symtable::analyse(&suite, name).map_err(|e| format!("{file}: {e}"))?;
        imported_modules(&suite, &mut wanted);
        parsed.push((index, file, suite, table));
        for (bundled, text) in BUNDLED_MODULES {
            if wanted.contains(*bundled) && !sources.iter().any(|(n, _)| n == bundled) {
                sources.push(((*bundled).to_owned(), (*text).to_owned()));
            }
        }
        index += 1;
    }
    let sources = &sources;
    let mut names = names;
    let mut module_names = module_names;
    for (name, _) in sources.iter().skip(modules.len()) {
        names.insert(name.as_str());
        module_names.push(name.as_str());
    }
    let parsed: Vec<_> = parsed
        .into_iter()
        .map(|(index, file, suite, table)| {
            let (name, source) = &sources[index];
            let source: &str = source.strip_prefix('\u{feff}').unwrap_or(source);
            (name.as_str(), file, source, suite, table)
        })
        .collect();
    // Compile a fixed seed program. Its root initializes the private runtime and
    // then calls __zipp_py_entry. Replace only that empty function's prototype;
    // retain all seed globals and function IDs. Never relocate JS bytecode.
    let program = crate::compile_only(RUNTIME, false)?;
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
    let name_global = program.functions[entry_fn].name_global;
    let project = Project {
        modules: names,
        module_names,
    };
    let program = RefCell::new(program);
    // Each module's top level becomes a code object the runtime runs on first
    // import; the entry body registers them all, names the entry module, then
    // imports it.
    let mut inits = Vec::with_capacity(parsed.len());
    for (index, (name, file, source, suite, table)) in parsed.iter().enumerate() {
        if program.borrow().functions.len() >= MAX_FUNCTIONS {
            return Err("Python project: function count limit exceeded".into());
        }
        let unit = Unit {
            file,
            source,
            module_index: index as u32,
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
    for (name, file, func_id) in inits {
        let code = out.alloc()?;
        out.emit(Instr::MakeFunc { dst: code, func_id })?;
        let name_r = out.string(name)?;
        let file_r = out.string(file)?;
        out.helper("module", &[name_r, code, file_r])?;
    }
    let name = out.string(entry)?;
    out.helper("entry", &[name])?;
    let hosted_r = out.boolean(hosted)?;
    out.helper("hosted", &[hosted_r])?;
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
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    name.len() <= 64 && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Conservative compiler limits, additional to host execution limits.
fn preflight(file: &str, source: &str) -> R<()> {
    if source.len() > MAX_SOURCE {
        return Err(format!("Python: {file} exceeds 1 MiB"));
    }
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let (mut count, mut brackets, mut indent) = (0usize, 0usize, 0usize);
    for item in lexer::lex(source, Mode::Module) {
        let (token, _) = item.map_err(|e| {
            format!(
                "SyntaxError: {} ({})",
                e.error,
                position(file, source, e.location)
            )
        })?;
        count += 1;
        match token {
            Tok::Indent => indent += 1,
            Tok::Dedent => indent = indent.saturating_sub(1),
            Tok::Lpar | Tok::Lsqb | Tok::Lbrace => brackets += 1,
            Tok::Rpar | Tok::Rsqb | Tok::Rbrace => brackets = brackets.saturating_sub(1),
            _ => {}
        }
        if count > MAX_TOKENS || brackets > 200 || indent > 100 {
            return Err(format!(
                "Python: compiler complexity limit exceeded in {file}"
            ));
        }
    }
    Ok(())
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
