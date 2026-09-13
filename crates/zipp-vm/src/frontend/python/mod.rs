//! Experimental Python-subset AST -> native ZIPP register bytecode.
//!
//! The fixed runtime bootstrap is trusted JavaScript compiled by ZIPP, but no
//! guest Python is translated into JavaScript source. Guest expressions/control
//! flow emit Instr directly. This intentionally avoids new opcodes or JIT edits.
//!
//! A program is a PROJECT: one or more modules, each a `.py` file's worth of
//! source, plus the name of the entry module. Every module-level binding lives
//! in the runtime's private name table under the key `"module.name"`, which the
//! emitter bakes into each load and store, so modules have distinct namespaces
//! without any change to the function-call ABI. `import`/`from ... import`
//! resolve only project modules and the built-in `ui` module, at compile time.
use crate::bytecode::{FuncProto, Instr, Program, Reg};
use ast::Ranged;
use rustpython_parser::{ast, lexer, Mode, Parse, Tok};
use std::collections::{BTreeMap, BTreeSet};

type R<T> = Result<T, String>;
const RUNTIME: &str = include_str!("runtime.js");
const MAX_SOURCE: usize = 64 * 1024;
const MAX_TOKENS: usize = 8192;
const MAX_DEPTH: usize = 64;
const MAX_FUNCTIONS: usize = 512;
const MAX_INSTRUCTIONS: usize = 65536;
const MAX_MODULES: usize = 64;
/// Modules the runtime provides itself; a project module of the same name
/// shadows it, as a script-directory module shadows the stdlib in CPython.
const BUILTIN_MODULES: &[&str] = &["ui"];

/// A single-file program: the source is the `main` module.
pub(super) fn compile(source: &str) -> R<Program> {
    compile_project("main", &[("main".to_owned(), source.to_owned())])
}

/// A multi-module program. `modules` pairs each module name (the `.py` file's
/// stem) with its source; `entry` names the module whose top level runs.
pub(super) fn compile_project(entry: &str, modules: &[(String, String)]) -> R<Program> {
    if modules.is_empty() {
        return Err("Python project: no modules".into());
    }
    if modules.len() > MAX_MODULES {
        return Err(format!("Python project: more than {MAX_MODULES} modules"));
    }
    let mut names = BTreeSet::new();
    for (name, _) in modules {
        if !is_module_name(name) {
            return Err(format!(
                "Python project: {name:?} is not a valid module name (letters, digits and _ only)"
            ));
        }
        if !names.insert(name.as_str()) {
            return Err(format!("Python project: duplicate module {name:?}"));
        }
    }
    if !names.contains(entry) {
        return Err(format!(
            "Python project: entry module {entry:?} is not in the project"
        ));
    }
    let mut parsed = Vec::with_capacity(modules.len());
    for (name, source) in modules {
        let file = format!("{name}.py");
        preflight(&file, source)?;
        let source = source.strip_prefix('\u{feff}').unwrap_or(source);
        let suite = ast::Suite::parse(source, &file).map_err(|e| {
            format!(
                "Python SyntaxError: {} at {}",
                e.error,
                position(&file, source, e.offset)
            )
        })?;
        parsed.push((name.as_str(), file, source, suite));
    }
    // Compile a fixed seed program. Its root initializes the private runtime and
    // then calls __zipp_py_entry. Replace only that empty function's prototype;
    // retain all seed globals and function IDs. Never relocate JS bytecode.
    let mut program = crate::compile_only(RUNTIME, false)?;
    let entry_fn = program
        .functions
        .iter()
        .position(|f| f.name == "__zipp_py_entry")
        .ok_or("Python bootstrap entry not found; incompatible JS compiler")?;
    let rt_slot = program
        .global_names
        .iter()
        .position(|n| n == "__zipp_py")
        .ok_or("Python bootstrap runtime slot not found")? as u32;
    let name_global = program.functions[entry_fn].name_global;
    let project = Project { modules: names };
    // Each module's top level becomes a parameterless function that the
    // runtime runs on first import; the entry body registers them all, names
    // the entry module, then imports it.
    let mut inits = Vec::with_capacity(parsed.len());
    for (name, file, source, suite) in &parsed {
        if program.functions.len() >= MAX_FUNCTIONS {
            return Err("Python project: function count limit exceeded".into());
        }
        let scope = Scope {
            module: name,
            file,
            in_function: false,
        };
        let mut out = Emitter::new(
            &format!("__zipp_py_mod_{name}"),
            source,
            rt_slot,
            scope,
            &project,
            &[],
            suite,
        )?;
        out.suite(suite, &mut program, 0)?;
        let value = out.none()?;
        out.emit(Instr::Return { src: value })?;
        let func_id = program.functions.len() as u32;
        program.functions.push(out.finish());
        inits.push((*name, func_id));
    }
    let scope = Scope {
        module: entry,
        file: "<entry>",
        in_function: false,
    };
    let mut out = Emitter::new("__zipp_py_entry", "", rt_slot, scope, &project, &[], &[])?;
    for (name, func_id) in inits {
        let code = out.alloc()?;
        out.emit(Instr::MakeFunc { dst: code, func_id })?;
        let name = out.string(name)?;
        out.helper("module", &[name, code])?;
    }
    let name = out.string(entry)?;
    out.helper("entry", &[name])?;
    let name = out.string(entry)?;
    out.helper("import", &[name])?;
    let value = out.none()?;
    out.emit(Instr::Return { src: value })?;
    let mut proto = out.finish();
    proto.name_global = name_global;
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

/// What the emitter knows about the whole project: the module names an
/// `import` may name.
struct Project<'a> {
    modules: BTreeSet<&'a str>,
}

/// Where code being emitted lives: which module (its namespace prefix), the
/// file name for diagnostics, and whether it is a function body.
#[derive(Clone, Copy)]
struct Scope<'a> {
    module: &'a str,
    file: &'a str,
    in_function: bool,
}

/// Conservative prototype compiler limits, additional to host execution limits.
/// They are not a substitute for fuzzing, Worker/process isolation or a review of
/// the third-party lexer/parser. In particular lexing happens before token caps.
fn preflight(file: &str, source: &str) -> R<()> {
    if source.len() > MAX_SOURCE {
        return Err(format!("Python subset: {file} exceeds 64 KiB"));
    }
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let (mut count, mut logical, mut brackets, mut indent) = (0usize, 0usize, 0usize, 0usize);
    for item in lexer::lex(source, Mode::Module) {
        let (token, _) = item.map_err(|e| {
            format!(
                "Python SyntaxError: {} at {}",
                e.error,
                position(file, source, e.location)
            )
        })?;
        count += 1;
        logical += 1;
        match token {
            Tok::Newline => logical = 0,
            Tok::Indent => indent += 1,
            Tok::Dedent => indent = indent.saturating_sub(1),
            Tok::Lpar | Tok::Lsqb | Tok::Lbrace => brackets += 1,
            Tok::Rpar | Tok::Rsqb | Tok::Rbrace => brackets = brackets.saturating_sub(1),
            _ => {}
        }
        if count > MAX_TOKENS || logical > 256 || brackets > 32 || indent > 32 {
            return Err(format!(
                "Python subset: compiler complexity limit exceeded in {file}"
            ));
        }
    }
    Ok(())
}

/// `file:line:col` (1-based, columns in characters) of a byte offset in `source`.
fn position(file: &str, source: &str, offset: rustpython_parser::text_size::TextSize) -> String {
    let offset = u32::from(offset) as usize;
    let prefix = source.get(..offset).unwrap_or(source);
    let line = prefix.bytes().filter(|c| *c == b'\n').count() + 1;
    let col = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("{file}:{line}:{col}")
}

struct LoopContext {
    head: u32,
    breaks: Vec<usize>,
}
struct Emitter<'a> {
    proto: FuncProto,
    source: &'a str,
    rt_slot: u32,
    next: usize,
    scope: Scope<'a>,
    project: &'a Project<'a>,
    locals: BTreeMap<String, Reg>,
    loops: Vec<LoopContext>,
}
impl<'a> Emitter<'a> {
    fn new(
        name: &str,
        source: &'a str,
        rt_slot: u32,
        scope: Scope<'a>,
        project: &'a Project<'a>,
        params: &[String],
        body: &[ast::Stmt],
    ) -> R<Self> {
        let mut proto = crate::compile::placeholder(name);
        proto.is_strict = true;
        proto.non_constructable = true;
        proto.simple_params = true;
        proto.source = source.to_owned();
        proto.param_count = u16::try_from(params.len()).map_err(|_| "too many parameters")?;
        proto.length = proto.param_count;
        let mut out = Self {
            proto,
            source,
            rt_slot,
            next: 1 + params.len(),
            scope,
            project,
            locals: BTreeMap::new(),
            loops: Vec::new(),
        };
        for (i, name) in params.iter().enumerate() {
            if out.locals.insert(name.clone(), (i + 1) as Reg).is_some() {
                return Err(format!("Python SyntaxError: duplicate argument {name:?}"));
            }
        }
        if scope.in_function {
            let mut assigned = BTreeSet::new();
            collect_locals(body, &mut assigned, 0)?;
            let mut uninitialized = Vec::new();
            for name in assigned {
                if !out.locals.contains_key(&name) {
                    let reg = out.alloc()?;
                    out.locals.insert(name, reg);
                    uninitialized.push(reg);
                }
            }
            if !uninitialized.is_empty() {
                let runtime = out.runtime()?;
                let unbound = out.prop(runtime, "unbound")?;
                for dst in uninitialized {
                    out.emit(Instr::Move { dst, src: unbound })?;
                }
            }
        }
        Ok(out)
    }
    fn finish(mut self) -> FuncProto {
        self.proto.reg_count = self.next as u16;
        self.proto
    }
    fn error(&self, node: &impl Ranged, message: &str) -> String {
        let at = position(self.scope.file, self.source, node.range().start());
        format!("Python subset {at}: {message}")
    }
    /// The private name-table key of a module-level binding in this scope's
    /// module: `"module.name"`.
    fn global_key(&self, name: &str) -> String {
        format!("{}.{}", self.scope.module, name)
    }
    fn depth(&self, node: &impl Ranged, depth: usize) -> R<()> {
        if depth > MAX_DEPTH {
            Err(self.error(node, "AST depth limit exceeded"))
        } else {
            Ok(())
        }
    }
    fn alloc(&mut self) -> R<Reg> {
        self.block(1)
    }
    fn block(&mut self, count: usize) -> R<Reg> {
        let count = count.max(1);
        let end = self.next.checked_add(count).ok_or("register overflow")?;
        if end >= u16::MAX as usize {
            return Err("Python subset: register limit exceeded".into());
        }
        let base = self.next as Reg;
        self.next = end;
        Ok(base)
    }
    fn emit(&mut self, instr: Instr) -> R<usize> {
        if self.proto.code.len() >= MAX_INSTRUCTIONS {
            return Err("Python subset: per-function bytecode limit exceeded".into());
        }
        let at = self.proto.code.len();
        self.proto.code.push(instr);
        Ok(at)
    }
    fn here(&self) -> u32 {
        self.proto.code.len() as u32
    }
    fn patch(&mut self, at: usize, target: u32) -> R<()> {
        match self.proto.code.get_mut(at) {
            Some(Instr::Jump { target: dst })
            | Some(Instr::JumpIfFalse { target: dst, .. })
            | Some(Instr::JumpIfTrue { target: dst, .. }) => {
                *dst = target;
                Ok(())
            }
            _ => Err("Python emitter: invalid jump patch".into()),
        }
    }
    fn string_index(&mut self, value: &str) -> u32 {
        if let Some(index) = self.proto.string_constants.iter().position(|s| s == value) {
            return index as u32;
        }
        let index = self.proto.string_constants.len() as u32;
        self.proto.string_constants.push(value.to_owned());
        index
    }
    fn string(&mut self, value: &str) -> R<Reg> {
        let dst = self.alloc()?;
        let si = self.string_index(value);
        // LoadConst addresses the VALUE constant pool, not string_constants.
        // Pending string Values carry STRING_CONST_BIT in their heap payload.
        let idx = self.proto.constants.len() as u32;
        self.proto
            .constants
            .push(crate::value::Value::heap(crate::vm::STRING_CONST_BIT | si));
        self.emit(Instr::LoadConst { dst, idx })?;
        Ok(dst)
    }
    fn integer(&mut self, text: &str) -> R<Reg> {
        if text.len() > 4096 {
            return Err("Python subset: integer literal too large".into());
        }
        let dst = self.alloc()?;
        if let Ok(value) = text.parse::<i128>() {
            self.emit(Instr::LoadBigInt { dst, value })?;
        } else {
            let value = num_bigint::BigInt::parse_bytes(text.as_bytes(), 10)
                .ok_or("Python emitter: invalid integer literal")?;
            let idx = self.proto.bigint_consts.len() as u32;
            self.proto.bigint_consts.push(value);
            self.emit(Instr::LoadBigIntBig { dst, idx })?;
        }
        Ok(dst)
    }
    fn none(&mut self) -> R<Reg> {
        let dst = self.alloc()?;
        self.emit(Instr::LoadNull { dst })?;
        Ok(dst)
    }
    fn boolean(&mut self, val: bool) -> R<Reg> {
        let dst = self.alloc()?;
        self.emit(Instr::LoadBool { dst, val })?;
        Ok(dst)
    }
    fn runtime(&mut self) -> R<Reg> {
        let dst = self.alloc()?;
        self.emit(Instr::LoadGlobal {
            dst,
            idx: self.rt_slot,
        })?;
        Ok(dst)
    }
    fn prop(&mut self, obj: Reg, key: &str) -> R<Reg> {
        let dst = self.alloc()?;
        let name = self.string_index(key);
        self.emit(Instr::GetProp { dst, obj, name })?;
        Ok(dst)
    }
    fn arguments(&mut self, args: &[Reg]) -> R<(Reg, u16)> {
        let argc = u16::try_from(args.len()).map_err(|_| "too many call arguments")?;
        let base = self.block(args.len())?;
        for (i, src) in args.iter().copied().enumerate() {
            self.emit(Instr::Move {
                dst: base + i as u16,
                src,
            })?;
        }
        Ok((base, argc))
    }
    fn raw_array(&mut self, values: &[Reg]) -> R<Reg> {
        let (arg_base, argc) = self.arguments(values)?;
        let dst = self.alloc()?;
        self.emit(Instr::NewArray {
            dst,
            arg_base,
            argc,
        })?;
        Ok(dst)
    }
    fn helper(&mut self, method: &str, args: &[Reg]) -> R<Reg> {
        let runtime = self.runtime()?;
        let callee = self.prop(runtime, method)?;
        let (arg_base, argc) = self.arguments(args)?;
        let dst = self.alloc()?;
        self.emit(Instr::Call {
            dst,
            callee,
            arg_base,
            argc,
        })?;
        Ok(dst)
    }
    fn load_name(&mut self, name: &str) -> R<Reg> {
        if let Some(reg) = self.locals.get(name).copied() {
            let bare = self.string(name)?;
            self.helper("local", &[reg, bare])
        } else {
            let key = self.string(&self.global_key(name))?;
            let bare = self.string(name)?;
            self.helper("load", &[key, bare])
        }
    }
    fn store_name(&mut self, name: &str, value: Reg) -> R<()> {
        if !self.scope.in_function {
            let key = self.string(&self.global_key(name))?;
            self.helper("store", &[key, value])?;
        } else {
            let dst = *self
                .locals
                .get(name)
                .ok_or("Python emitter: unreserved local")?;
            self.emit(Instr::Move { dst, src: value })?;
        }
        Ok(())
    }
    fn suite(&mut self, suite: &[ast::Stmt], program: &mut Program, depth: usize) -> R<()> {
        for stmt in suite {
            self.stmt(stmt, program, depth)?;
        }
        Ok(())
    }
    fn stmt(&mut self, stmt: &ast::Stmt, program: &mut Program, depth: usize) -> R<()> {
        self.depth(stmt, depth)?;
        match stmt {
            ast::Stmt::Pass(_) => {}
            ast::Stmt::Expr(s) => {
                self.expr(&s.value, depth + 1)?;
            }
            ast::Stmt::Assign(s) => {
                let value = self.expr(&s.value, depth + 1)?;
                for target in &s.targets {
                    self.assign(target, value, depth + 1)?;
                }
            }
            ast::Stmt::AugAssign(s) => {
                let ast::Expr::Name(name) = s.target.as_ref() else {
                    return Err(self.error(stmt, "augmented assignment currently requires a name"));
                };
                // *= is deliberately excluded until list in-place repetition is
                // implemented; falling back to binary * would break aliasing.
                let op = match s.op {
                    ast::Operator::Add => "iadd",
                    ast::Operator::Sub => "sub",
                    ast::Operator::FloorDiv => "floordiv",
                    ast::Operator::Mod => "mod",
                    _ => return Err(self.error(stmt, "unsupported augmented operator")),
                };
                let left = self.load_name(name.id.as_str())?;
                let right = self.expr(&s.value, depth + 1)?;
                let op = self.string(op)?;
                let value = self.helper("binary", &[op, left, right])?;
                self.store_name(name.id.as_str(), value)?;
            }
            ast::Stmt::Return(s) => {
                if !self.scope.in_function {
                    return Err(self.error(stmt, "return outside function"));
                }
                let value = match s.value.as_ref() {
                    Some(expr) => self.expr(expr, depth + 1)?,
                    None => self.none()?,
                };
                self.emit(Instr::Return { src: value })?;
            }
            ast::Stmt::If(s) => {
                let value = self.expr(&s.test, depth + 1)?;
                let cond = self.helper("truth", &[value])?;
                let jump = self.emit(Instr::JumpIfFalse { cond, target: 0 })?;
                self.suite(&s.body, program, depth + 1)?;
                let end = self.emit(Instr::Jump { target: 0 })?;
                self.patch(jump, self.here())?;
                self.suite(&s.orelse, program, depth + 1)?;
                self.patch(end, self.here())?;
            }
            ast::Stmt::While(s) => {
                let head = self.here();
                let value = self.expr(&s.test, depth + 1)?;
                let cond = self.helper("truth", &[value])?;
                let exhausted = self.emit(Instr::JumpIfFalse { cond, target: 0 })?;
                self.loops.push(LoopContext {
                    head,
                    breaks: Vec::new(),
                });
                self.suite(&s.body, program, depth + 1)?;
                self.emit(Instr::Jump { target: head })?;
                self.finish_loop(exhausted, &s.orelse, program, depth + 1)?;
            }
            ast::Stmt::For(s) => {
                let value = self.expr(&s.iter, depth + 1)?;
                let iter = self.helper("iter", &[value])?;
                let head = self.here();
                let item = self.helper("next", &[iter])?;
                let cond = self.prop(item, "done")?;
                let exhausted = self.emit(Instr::JumpIfTrue { cond, target: 0 })?;
                let value = self.prop(item, "value")?;
                self.assign(&s.target, value, depth + 1)?;
                self.loops.push(LoopContext {
                    head,
                    breaks: Vec::new(),
                });
                self.suite(&s.body, program, depth + 1)?;
                self.emit(Instr::Jump { target: head })?;
                self.finish_loop(exhausted, &s.orelse, program, depth + 1)?;
            }
            ast::Stmt::Break(_) => {
                if self.loops.is_empty() {
                    return Err(self.error(stmt, "break outside loop"));
                }
                let jump = self.emit(Instr::Jump { target: 0 })?;
                self.loops.last_mut().unwrap().breaks.push(jump);
            }
            ast::Stmt::Continue(_) => {
                let Some(context) = self.loops.last() else {
                    return Err(self.error(stmt, "continue outside loop"));
                };
                let target = context.head;
                self.emit(Instr::Jump { target })?;
            }
            ast::Stmt::Assert(s) => {
                let value = self.expr(&s.test, depth + 1)?;
                let cond = self.helper("truth", &[value])?;
                let passed = self.emit(Instr::JumpIfTrue { cond, target: 0 })?;
                let msg = match &s.msg {
                    Some(expr) => self.expr(expr, depth + 1)?,
                    None => self.string("")?,
                };
                self.helper("assertion", &[msg])?;
                self.patch(passed, self.here())?;
            }
            ast::Stmt::FunctionDef(s) => {
                if self.scope.in_function {
                    return Err(self.error(stmt, "nested functions/closures are not yet supported"));
                }
                if !s.decorator_list.is_empty() || s.returns.is_some() || !s.type_params.is_empty()
                {
                    return Err(self.error(
                        stmt,
                        "decorators, annotations and type parameters are not supported",
                    ));
                }
                if s.args.vararg.is_some()
                    || s.args.kwarg.is_some()
                    || !s.args.kwonlyargs.is_empty()
                {
                    return Err(
                        self.error(stmt, "only ordinary positional parameters are supported")
                    );
                }
                let mut params = Vec::new();
                for arg in s.args.posonlyargs.iter().chain(s.args.args.iter()) {
                    if arg.default.is_some() || arg.def.annotation.is_some() {
                        return Err(self
                            .error(stmt, "defaults and parameter annotations are not supported"));
                    }
                    params.push(arg.def.arg.to_string());
                }
                if program.functions.len() >= MAX_FUNCTIONS {
                    return Err(self.error(
                        stmt,
                        "function count limit exceeded (including runtime helpers)",
                    ));
                }
                let scope = Scope {
                    in_function: true,
                    ..self.scope
                };
                let mut child = Emitter::new(
                    s.name.as_str(),
                    self.source,
                    self.rt_slot,
                    scope,
                    self.project,
                    &params,
                    &s.body,
                )?;
                child.suite(&s.body, program, depth + 1)?;
                let none = child.none()?;
                child.emit(Instr::Return { src: none })?;
                let func_id = program.functions.len() as u32;
                program.functions.push(child.finish());
                let code = self.alloc()?;
                self.emit(Instr::MakeFunc { dst: code, func_id })?;
                let name = self.string(s.name.as_str())?;
                let argc = self.integer(&params.len().to_string())?;
                let value = self.helper("fn", &[code, name, argc])?;
                self.store_name(s.name.as_str(), value)?;
            }
            ast::Stmt::Import(s) => {
                for alias in &s.names {
                    let module = self.import_module(stmt, alias.name.as_str())?;
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.store_name(bound.as_str(), module)?;
                }
            }
            ast::Stmt::ImportFrom(s) => {
                if s.level.is_some_and(|level| level.to_u32() > 0) {
                    return Err(self.error(stmt, "relative imports are not supported"));
                }
                let Some(name) = s.module.as_ref() else {
                    return Err(self.error(stmt, "relative imports are not supported"));
                };
                let module = self.import_module(stmt, name.as_str())?;
                for alias in &s.names {
                    if alias.name.as_str() == "*" {
                        return Err(self.error(stmt, "`from module import *` is not supported"));
                    }
                    let attr = self.string(alias.name.as_str())?;
                    let value = self.helper("attr", &[module, attr])?;
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.store_name(bound.as_str(), value)?;
                }
            }
            _ => return Err(self.error(stmt, "statement is not supported by this Python subset")),
        }
        Ok(())
    }
    /// Resolve `name` against the project and the built-in modules at compile
    /// time, then emit the runtime import (which runs the module once).
    fn import_module(&mut self, stmt: &ast::Stmt, name: &str) -> R<Reg> {
        if self.scope.in_function {
            return Err(self.error(stmt, "imports inside functions are not supported"));
        }
        if name.contains('.') {
            return Err(self.error(stmt, "packages (dotted imports) are not supported"));
        }
        if !self.project.modules.contains(name) && !BUILTIN_MODULES.contains(&name) {
            return Err(self.error(
                stmt,
                &format!(
                    "No module named '{name}' (only project .py files and `ui` can be imported)"
                ),
            ));
        }
        let key = self.string(name)?;
        self.helper("import", &[key])
    }
    fn finish_loop(
        &mut self,
        exhausted: usize,
        otherwise: &[ast::Stmt],
        program: &mut Program,
        depth: usize,
    ) -> R<()> {
        let context = self.loops.pop().ok_or("Python emitter: missing loop")?;
        self.patch(exhausted, self.here())?;
        self.suite(otherwise, program, depth)?;
        let end = self.here();
        for jump in context.breaks {
            self.patch(jump, end)?;
        }
        Ok(())
    }
    fn assign(&mut self, target: &ast::Expr, value: Reg, depth: usize) -> R<()> {
        self.depth(target, depth)?;
        match target {
            ast::Expr::Name(n) => self.store_name(n.id.as_str(), value),
            ast::Expr::Tuple(t) => self.unpack_assign(&t.elts, value, depth),
            ast::Expr::List(t) => self.unpack_assign(&t.elts, value, depth),
            ast::Expr::Subscript(t) => {
                let obj = self.expr(&t.value, depth + 1)?;
                let index = self.expr(&t.slice, depth + 1)?;
                self.helper("setindex", &[obj, index, value])?;
                Ok(())
            }
            _ => Err(self.error(
                target,
                "unsupported assignment target (including starred targets)",
            )),
        }
    }
    fn unpack_assign(&mut self, targets: &[ast::Expr], value: Reg, depth: usize) -> R<()> {
        let count = self.integer(&targets.len().to_string())?;
        // Validate the full outer unpack before writing any target. This also
        // snapshots a/b before a,b = b,a, instead of sequential reassignment.
        let unpacked = self.helper("unpack", &[value, count])?;
        for (i, target) in targets.iter().enumerate() {
            let index = self.integer(&i.to_string())?;
            let item = self.helper("index", &[unpacked, index])?;
            self.assign(target, item, depth + 1)?;
        }
        Ok(())
    }
    fn expr(&mut self, expr: &ast::Expr, depth: usize) -> R<Reg> {
        self.depth(expr, depth)?;
        match expr {
            ast::Expr::Name(n) => self.load_name(n.id.as_str()),
            ast::Expr::Constant(c) => match &c.value {
                ast::Constant::None => self.none(),
                ast::Constant::Bool(b) => self.boolean(*b),
                ast::Constant::Str(s) => self.string(s),
                ast::Constant::Int(i) => self.integer(&i.to_string()),
                _ => Err(self.error(expr, "only int, str, bool and None literals are supported")),
            },
            ast::Expr::BinOp(b) => {
                let op = match b.op {
                    ast::Operator::Add => "add",
                    ast::Operator::Sub => "sub",
                    ast::Operator::Mult => "mul",
                    ast::Operator::FloorDiv => "floordiv",
                    ast::Operator::Mod => "mod",
                    _ => return Err(self.error(expr, "unsupported operator (including / and **)")),
                };
                let a = self.expr(&b.left, depth + 1)?;
                let b = self.expr(&b.right, depth + 1)?;
                let op = self.string(op)?;
                self.helper("binary", &[op, a, b])
            }
            ast::Expr::UnaryOp(u) => {
                let op = match u.op {
                    ast::UnaryOp::Not => "not",
                    ast::UnaryOp::USub => "neg",
                    ast::UnaryOp::UAdd => "pos",
                    _ => return Err(self.error(expr, "unsupported unary operator")),
                };
                let value = self.expr(&u.operand, depth + 1)?;
                let op = self.string(op)?;
                self.helper("unary", &[op, value])
            }
            ast::Expr::BoolOp(b) => {
                let dst = self.alloc()?;
                let mut jumps = Vec::new();
                for (i, value) in b.values.iter().enumerate() {
                    let value = self.expr(value, depth + 1)?;
                    self.emit(Instr::Move { dst, src: value })?;
                    if i + 1 != b.values.len() {
                        let cond = self.helper("truth", &[value])?;
                        let jump = match b.op {
                            ast::BoolOp::And => Instr::JumpIfFalse { cond, target: 0 },
                            ast::BoolOp::Or => Instr::JumpIfTrue { cond, target: 0 },
                        };
                        jumps.push(self.emit(jump)?);
                    }
                }
                let end = self.here();
                for at in jumps {
                    self.patch(at, end)?;
                }
                Ok(dst)
            }
            ast::Expr::Compare(c) => {
                let dst = self.boolean(true)?;
                let mut jumps = Vec::new();
                let mut left = self.expr(&c.left, depth + 1)?;
                for (op, right) in c.ops.iter().zip(c.comparators.iter()) {
                    let right = self.expr(right, depth + 1)?;
                    let op = match op {
                        ast::CmpOp::Eq => "eq",
                        ast::CmpOp::NotEq => "ne",
                        ast::CmpOp::Lt => "lt",
                        ast::CmpOp::LtE => "le",
                        ast::CmpOp::Gt => "gt",
                        ast::CmpOp::GtE => "ge",
                        ast::CmpOp::In => "in",
                        ast::CmpOp::NotIn => "notin",
                        ast::CmpOp::Is => "is",
                        ast::CmpOp::IsNot => "isnot",
                    };
                    let op = self.string(op)?;
                    let value = self.helper("compare", &[op, left, right])?;
                    self.emit(Instr::Move { dst, src: value })?;
                    jumps.push(self.emit(Instr::JumpIfFalse {
                        cond: dst,
                        target: 0,
                    })?);
                    left = right;
                }
                let end = self.here();
                for at in jumps {
                    self.patch(at, end)?;
                }
                Ok(dst)
            }
            ast::Expr::Call(c) => {
                if !c.keywords.is_empty() {
                    return Err(self.error(expr, "keyword arguments are not yet supported"));
                }
                // Python-to-Python calls are direct VM `Call`s, not a trip
                // through `Function.prototype.apply`: the callee's frame goes
                // on the VM's explicit frame stack, so deep Python recursion
                // is bounded (and catchable) the same way JavaScript's is
                // rather than consuming native stack per level. `callable`
                // validates the wrapper and arity, then hands back the code.
                let function = self.expr(&c.func, depth + 1)?;
                let mut args = Vec::new();
                for arg in &c.args {
                    args.push(self.expr(arg, depth + 1)?);
                }
                let count = self.integer(&args.len().to_string())?;
                let callee = self.helper("callable", &[function, count])?;
                let (arg_base, argc) = self.arguments(&args)?;
                let dst = self.alloc()?;
                self.emit(Instr::Call {
                    dst,
                    callee,
                    arg_base,
                    argc,
                })?;
                Ok(dst)
            }
            ast::Expr::List(l) => self.sequence("list", &l.elts, depth),
            ast::Expr::Tuple(t) => self.sequence("tuple", &t.elts, depth),
            ast::Expr::Subscript(s) => {
                let value = self.expr(&s.value, depth + 1)?;
                let index = self.expr(&s.slice, depth + 1)?;
                self.helper("index", &[value, index])
            }
            ast::Expr::Attribute(a) => {
                let value = self.expr(&a.value, depth + 1)?;
                let name = self.string(a.attr.as_str())?;
                self.helper("attr", &[value, name])
            }
            ast::Expr::IfExp(e) => {
                let value = self.expr(&e.test, depth + 1)?;
                let cond = self.helper("truth", &[value])?;
                let dst = self.alloc()?;
                let otherwise = self.emit(Instr::JumpIfFalse { cond, target: 0 })?;
                let yes = self.expr(&e.body, depth + 1)?;
                self.emit(Instr::Move { dst, src: yes })?;
                let end = self.emit(Instr::Jump { target: 0 })?;
                self.patch(otherwise, self.here())?;
                let no = self.expr(&e.orelse, depth + 1)?;
                self.emit(Instr::Move { dst, src: no })?;
                self.patch(end, self.here())?;
                Ok(dst)
            }
            _ => Err(self.error(expr, "expression is not supported by this Python subset")),
        }
    }
    fn sequence(&mut self, method: &str, values: &[ast::Expr], depth: usize) -> R<Reg> {
        let mut regs = Vec::new();
        for value in values {
            regs.push(self.expr(value, depth + 1)?);
        }
        let raw = self.raw_array(&regs)?;
        self.helper(method, &[raw])
    }
}

fn collect_target(target: &ast::Expr, names: &mut BTreeSet<String>, depth: usize) -> R<()> {
    if depth > MAX_DEPTH {
        return Err("Python subset: target depth exceeded".into());
    }
    match target {
        ast::Expr::Name(n) => {
            names.insert(n.id.to_string());
        }
        ast::Expr::Tuple(t) => {
            for item in &t.elts {
                collect_target(item, names, depth + 1)?;
            }
        }
        ast::Expr::List(l) => {
            for item in &l.elts {
                collect_target(item, names, depth + 1)?;
            }
        }
        // Attribute/index assignment does not make the base name a local.
        _ => {}
    }
    Ok(())
}
fn collect_locals(body: &[ast::Stmt], names: &mut BTreeSet<String>, depth: usize) -> R<()> {
    if depth > MAX_DEPTH {
        return Err("Python subset: scope depth exceeded".into());
    }
    for stmt in body {
        match stmt {
            ast::Stmt::Assign(s) => {
                for t in &s.targets {
                    collect_target(t, names, depth + 1)?;
                }
            }
            ast::Stmt::AugAssign(s) => collect_target(&s.target, names, depth + 1)?,
            ast::Stmt::For(s) => {
                collect_target(&s.target, names, depth + 1)?;
                collect_locals(&s.body, names, depth + 1)?;
                collect_locals(&s.orelse, names, depth + 1)?;
            }
            ast::Stmt::If(s) => {
                collect_locals(&s.body, names, depth + 1)?;
                collect_locals(&s.orelse, names, depth + 1)?;
            }
            ast::Stmt::While(s) => {
                collect_locals(&s.body, names, depth + 1)?;
                collect_locals(&s.orelse, names, depth + 1)?;
            }
            ast::Stmt::FunctionDef(s) => {
                names.insert(s.name.to_string());
            }
            _ => {}
        }
    }
    Ok(())
}
