//! The Python emitter's core: registers, runtime helper calls, name access,
//! and the prologues of module, function, class-body and comprehension code.
//!
//! Every Python code object is one [`FuncProto`] with a fixed ABI: register 0
//! is the Python function OBJECT (`this`), register 1 is the array of bound
//! positional values the runtime's `bind` produced (parameters in signature
//! order, `*args` as a tuple and `**kwargs` as a dict at the end). A class body
//! receives its namespace as `args[0]`; a comprehension receives the outermost
//! iterator. Locals are registers, captured locals are cell objects (`{v}`)
//! in registers, free variables are cells read off `this.cells`, and globals go
//! through the module's dictionary held on `this.globals`.
use super::symtable::{Scope, ScopeKind, SymKind, SymTable};
use super::{Project, BUILTIN_MODULES};
use crate::bytecode::{FuncProto, Instr, Program, Reg, NO_NAME};
use ast::Ranged;
use rustpython_parser::ast;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(super) type R<T> = Result<T, String>;

/// Whether the emitter lowers operations to guarded inline fast paths (int
/// and float arithmetic, comparisons, identity tests, unpacking, counted and
/// indexed loops) ahead of the runtime helper each one falls back to.
/// `ZIPP_PY_NOFAST=1` (like `ZIPP_NOJIT`, any value) compiles every such
/// operation to its helper only, so the differential corpus can check both
/// lowerings. Read once per process.
pub(super) fn py_fast_paths() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_PY_NOFAST").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}
pub(super) const MAX_DEPTH: usize = 96;
pub(super) const MAX_FUNCTIONS: usize = 8192;
pub(super) const MAX_INSTRUCTIONS: usize = 1 << 20;
/// Registers 0 (`this`) and 1 (the bound-args array) are fixed by the ABI.
pub(super) const REG_FUNC: Reg = 0;
pub(super) const REG_ARGS: Reg = 1;

pub(super) struct LoopCtx {
    pub head: u32,
    pub breaks: Vec<usize>,
    pub continues: Vec<usize>,
    pub handler_depth: usize,
}

/// What a module contributes to diagnostics and line stamps.
#[derive(Clone, Copy)]
pub(super) struct Unit<'a> {
    pub file: &'a str,
    pub source: &'a str,
    pub module_index: u32,
    /// Byte offset of each line's start ([`line_starts`]), so a statement's
    /// line is a binary search rather than a count from the top of the file.
    pub lines: &'a [u32],
}

/// The byte offset where each line of `source` starts; the first is 0.
pub(super) fn line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0];
    starts.extend(
        source
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i as u32 + 1),
    );
    starts
}

pub(super) struct Emitter<'a> {
    pub proto: FuncProto,
    pub unit: Unit<'a>,
    pub rt_slot: u32,
    pub line_slot: u32,
    pub table: &'a SymTable,
    pub scope: usize,
    pub project: &'a Project<'a>,
    /// The program every compiled code object is appended to.
    pub program: &'a RefCell<Program>,
    pub next: usize,
    high: usize,
    // Persistent registers set up by the prologue. `UNBOUND` and the
    // globals are loaded there only when the scope's symbols show a use;
    // otherwise [`Emitter::unbound`] and [`Emitter::globals`] read them where
    // needed. The STOP sentinel is loaded ahead of each loop that tests it.
    pub r_rt: Reg,
    r_unb: Option<Reg>,
    r_globals: Option<Reg>,
    pub r_ns: Option<Reg>,
    pub r_line: Reg,
    /// Local (value) registers by name.
    pub locals: BTreeMap<String, Reg>,
    /// Cell object registers by name (both own cells and captured free cells).
    pub cells: BTreeMap<String, Reg>,
    /// Locals known to be bound on every path reaching the current point.
    /// Blocks save and restore it, so a binding made inside a branch, loop
    /// body or handler is definite only until that block ends.
    pub definite: BTreeSet<String>,
    /// Non-zero while compiling an operand that may not be evaluated (the
    /// right side of `and`/`or`, a conditional expression's arms): a walrus
    /// there must not count as a definite binding.
    pub speculative: usize,
    /// `from __future__ import annotations`: annotations stay strings.
    pub future_annotations: bool,
    pub control_depth: usize,
    pub loops: Vec<LoopCtx>,
    pub handler_depth: usize,
    pub qualname: String,
    last_line: i32,
    /// [`py_fast_paths`], read once per code object.
    pub fast: bool,
    /// Constant-pool indices already handed out, so every use of the same
    /// string or float shares one entry (and one string constant).
    string_ids: HashMap<String, u32>,
    string_consts: HashMap<u32, u32>,
    float_consts: HashMap<u64, u32>,
    /// Int literals loaded once ahead of the body ([`Emitter::hoist_ints`]),
    /// by value: [`Emitter::integer`] answers with the register.
    hoisted: HashMap<i128, Reg>,
}

impl<'a> Emitter<'a> {
    /// Create an emitter for the scope defined at `offset` (or the module scope
    /// when `offset` is `None`) and run its prologue.
    pub fn new(
        name: &str,
        unit: Unit<'a>,
        rt_slot: u32,
        line_slot: u32,
        table: &'a SymTable,
        scope: usize,
        project: &'a Project<'a>,
        program: &'a RefCell<Program>,
        qualname: String,
    ) -> R<Self> {
        let mut proto = crate::compile::placeholder(name);
        proto.is_strict = true;
        proto.non_constructable = true;
        proto.simple_params = true;
        proto.param_count = 1;
        proto.length = 1;
        proto.is_generator = table.scopes[scope].is_generator;
        let mut e = Emitter {
            proto,
            unit,
            rt_slot,
            line_slot,
            table,
            scope,
            project,
            program,
            next: 2,
            high: 2,
            r_rt: 0,
            r_unb: None,
            r_globals: None,
            r_ns: None,
            r_line: 0,
            locals: BTreeMap::new(),
            cells: BTreeMap::new(),
            definite: BTreeSet::new(),
            speculative: 0,
            future_annotations: false,
            control_depth: 0,
            loops: Vec::new(),
            handler_depth: 0,
            qualname,
            last_line: -1,
            fast: py_fast_paths(),
            string_ids: HashMap::new(),
            string_consts: HashMap::new(),
            float_consts: HashMap::new(),
            hoisted: HashMap::new(),
        };
        e.prologue()?;
        Ok(e)
    }

    pub fn scope(&self) -> &'a Scope {
        &self.table.scopes[self.scope]
    }
    pub fn kind(&self) -> ScopeKind {
        self.scope().kind
    }
    pub fn in_function(&self) -> bool {
        self.kind() == ScopeKind::Function
    }
    /// The `__module__` of what this module defines: `__main__` for the
    /// module run as the program, as its `__name__` is.
    pub fn module_name(&self) -> &'a str {
        let name = self.project.module_names[self.unit.module_index as usize];
        if name == self.project.entry {
            "__main__"
        } else {
            name
        }
    }

    fn prologue(&mut self) -> R<()> {
        self.r_rt = self.alloc()?;
        self.emit(Instr::LoadGlobal {
            dst: self.r_rt,
            idx: self.rt_slot,
        })?;
        let scope = self.scope();
        // Unbound-initialised locals and cells need UNBOUND right here, and
        // reads of captured cells test against it; global names, class
        // namespaces and nested code objects (which capture the globals)
        // need the module's dictionary.
        let unbound_use = |(name, kind): (&String, &SymKind)| match kind {
            SymKind::Local | SymKind::Cell => !scope.params.contains(name),
            SymKind::Free => true,
            _ => false,
        };
        if scope.symbols.iter().any(unbound_use) {
            self.r_unb = Some(self.prop(self.r_rt, "UNBOUND")?);
        }
        if self.kind() != ScopeKind::Function
            || !scope.children.is_empty()
            || scope
                .symbols
                .values()
                .any(|k| matches!(k, SymKind::Global | SymKind::ClassLocal))
        {
            self.r_globals = Some(self.prop(REG_FUNC, "globals")?);
        }
        self.r_line = self.alloc()?;
        if self.kind() == ScopeKind::Class {
            let zero = self.small_int(0)?;
            let ns = self.alloc()?;
            self.emit(Instr::GetIndex {
                dst: ns,
                obj: REG_ARGS,
                key: zero,
            })?;
            self.r_ns = Some(ns);
        }
        let scope = self.scope();
        // Parameters arrive bound, in signature order.
        for (i, name) in scope.params.iter().enumerate() {
            let idx = self.small_int(i as i32)?;
            let value = self.alloc()?;
            self.emit(Instr::GetIndex {
                dst: value,
                obj: REG_ARGS,
                key: idx,
            })?;
            match scope.symbols.get(name) {
                Some(SymKind::Cell) => {
                    let cell = self.helper("cell", &[value])?;
                    self.cells.insert(name.clone(), cell);
                }
                _ => {
                    self.locals.insert(name.clone(), value);
                }
            }
            self.definite.insert(name.clone());
        }
        // Other locals start unbound; own cells start holding the sentinel.
        for (name, kind) in &scope.symbols {
            if scope.params.contains(name) || name.starts_with('\u{0}') {
                continue;
            }
            match kind {
                SymKind::Local => {
                    let unbound = self.unbound()?;
                    let reg = self.alloc()?;
                    self.emit(Instr::Move {
                        dst: reg,
                        src: unbound,
                    })?;
                    self.locals.insert(name.clone(), reg);
                }
                SymKind::Cell => {
                    let unbound = self.unbound()?;
                    let cell = self.helper("cell", &[unbound])?;
                    self.cells.insert(name.clone(), cell);
                }
                _ => {}
            }
        }
        // Captured cells, in `free_order`, off the function object.
        if !scope.free_order.is_empty() {
            let cells = self.prop(REG_FUNC, "cells")?;
            for (i, name) in scope.free_order.iter().enumerate() {
                let idx = self.small_int(i as i32)?;
                let cell = self.alloc()?;
                self.emit(Instr::GetIndex {
                    dst: cell,
                    obj: cells,
                    key: idx,
                })?;
                self.cells.insert(name.clone(), cell);
            }
        }
        if self.proto.is_generator {
            self.emit(Instr::GenStart)?;
        }
        Ok(())
    }

    /// The UNBOUND sentinel: the prologue's register, or a read here.
    pub fn unbound(&mut self) -> R<Reg> {
        match self.r_unb {
            Some(reg) => Ok(reg),
            None => self.prop(self.r_rt, "UNBOUND"),
        }
    }
    /// The module's global dictionary: the prologue's register, or a read here.
    pub fn globals(&mut self) -> R<Reg> {
        match self.r_globals {
            Some(reg) => Ok(reg),
            None => self.prop(REG_FUNC, "globals"),
        }
    }
    /// The runtime's end-of-iteration sentinel, read ahead of a loop.
    pub fn stop(&mut self) -> R<Reg> {
        self.prop(self.r_rt, "STOP")
    }

    pub fn finish(mut self) -> FuncProto {
        self.proto.reg_count = (self.high.max(self.next)) as u16;
        self.proto
    }

    // ---- diagnostics ----------------------------------------------------------
    pub fn error(&self, node: &impl Ranged, message: &str) -> String {
        let at = super::position(self.unit.file, self.unit.source, node.range().start());
        format!("Python: {at}: {message}")
    }
    pub fn line_of(&self, node: &impl Ranged) -> i32 {
        let offset = u32::from(node.range().start());
        // Lines starting at or before the offset; the table always holds 0.
        self.unit.lines.partition_point(|&start| start <= offset).max(1) as i32
    }
    /// Stamp the current line into the runtime's line global (one `LoadInt`
    /// and one `StoreGlobal`; skipped when the line has not changed).
    pub fn stamp_line(&mut self, node: &impl Ranged) -> R<()> {
        let line = self.line_of(node);
        if line == self.last_line {
            return Ok(());
        }
        self.last_line = line;
        let val = self.unit.module_index as i32 * 1_000_000 + line;
        self.emit(Instr::LoadInt {
            dst: self.r_line,
            val,
        })?;
        self.emit(Instr::StoreGlobal {
            idx: self.line_slot,
            src: self.r_line,
        })?;
        Ok(())
    }
    /// After a jump target the last stamped line is unknown again.
    pub fn forget_line(&mut self) {
        self.last_line = -1;
    }
    pub fn depth(&self, node: &impl Ranged, depth: usize) -> R<()> {
        if depth > MAX_DEPTH {
            Err(self.error(node, "expression nesting limit exceeded"))
        } else {
            Ok(())
        }
    }

    // ---- registers ------------------------------------------------------------
    pub fn alloc(&mut self) -> R<Reg> {
        self.block(1)
    }
    pub fn block(&mut self, count: usize) -> R<Reg> {
        let count = count.max(1);
        let end = self.next.checked_add(count).ok_or("register overflow")?;
        if end >= u16::MAX as usize {
            return Err("Python: register limit exceeded in one function".into());
        }
        let base = self.next as Reg;
        self.next = end;
        self.high = self.high.max(end);
        Ok(base)
    }
    /// Statement-level temporaries are reclaimed by resetting to a mark.
    pub fn mark(&self) -> usize {
        self.next
    }
    pub fn release(&mut self, mark: usize) {
        self.next = mark;
    }

    // ---- instructions ---------------------------------------------------------
    pub fn emit(&mut self, instr: Instr) -> R<usize> {
        if self.proto.code.len() >= MAX_INSTRUCTIONS {
            return Err("Python: per-function bytecode limit exceeded".into());
        }
        let at = self.proto.code.len();
        self.proto.code.push(instr);
        Ok(at)
    }
    pub fn here(&self) -> u32 {
        self.proto.code.len() as u32
    }
    pub fn patch(&mut self, at: usize, target: u32) -> R<()> {
        match self.proto.code.get_mut(at) {
            Some(Instr::Jump { target: dst })
            | Some(Instr::JumpIfFalse { target: dst, .. })
            | Some(Instr::JumpIfTrue { target: dst, .. })
            | Some(Instr::JumpIfNotLt { target: dst, .. })
            | Some(Instr::JumpIfNotLe { target: dst, .. })
            | Some(Instr::JumpFinally { target: dst, .. })
            | Some(Instr::PushHandler {
                catch_target: dst, ..
            })
            | Some(Instr::PushFinally { target: dst, .. }) => {
                *dst = target;
                Ok(())
            }
            _ => Err("Python emitter: invalid jump patch".into()),
        }
    }
    pub fn jump(&mut self) -> R<usize> {
        self.emit(Instr::Jump { target: 0 })
    }
    pub fn jump_if_false(&mut self, cond: Reg) -> R<usize> {
        self.emit(Instr::JumpIfFalse { cond, target: 0 })
    }
    pub fn jump_if_true(&mut self, cond: Reg) -> R<usize> {
        self.emit(Instr::JumpIfTrue { cond, target: 0 })
    }

    // ---- constants --------------------------------------------------------------
    pub fn string_index(&mut self, value: &str) -> u32 {
        if let Some(&index) = self.string_ids.get(value) {
            return index;
        }
        let index = self.proto.string_constants.len() as u32;
        self.proto.string_constants.push(value.to_owned());
        self.string_ids.insert(value.to_owned(), index);
        index
    }
    pub fn string(&mut self, value: &str) -> R<Reg> {
        let dst = self.alloc()?;
        let si = self.string_index(value);
        let idx = match self.string_consts.get(&si) {
            Some(&idx) => idx,
            None => {
                let idx = self.proto.constants.len() as u32;
                self.proto
                    .constants
                    .push(crate::value::Value::heap(crate::vm::STRING_CONST_BIT | si));
                self.string_consts.insert(si, idx);
                idx
            }
        };
        self.emit(Instr::LoadConst { dst, idx })?;
        Ok(dst)
    }
    /// [`Self::string`] for a value known to be distinct from every other
    /// constant (the virtual filesystem's paths and contents): it is appended
    /// without searching the pool, which would be quadratic in the file count.
    pub fn unique_string(&mut self, value: String) -> R<Reg> {
        let dst = self.alloc()?;
        let si = self.proto.string_constants.len() as u32;
        self.proto.string_constants.push(value);
        let idx = self.proto.constants.len() as u32;
        self.proto
            .constants
            .push(crate::value::Value::heap(crate::vm::STRING_CONST_BIT | si));
        self.emit(Instr::LoadConst { dst, idx })?;
        Ok(dst)
    }
    /// A Python int (a BigInt in the runtime's value model).
    pub fn integer(&mut self, text: &str) -> R<Reg> {
        if text.len() > 4096 {
            return Err("Python: integer literal too large".into());
        }
        if let Ok(value) = text.parse::<i128>() {
            if let Some(&reg) = self.hoisted.get(&value) {
                return Ok(reg);
            }
        }
        let dst = self.alloc()?;
        if let Ok(value) = text.parse::<i128>() {
            self.emit(Instr::LoadBigInt { dst, value })?;
        } else {
            let value = num_bigint::BigInt::parse_bytes(text.as_bytes(), 10)
                .ok_or("Python: invalid integer literal")?;
            let idx = self.proto.bigint_consts.len() as u32;
            self.proto.bigint_consts.push(value);
            self.emit(Instr::LoadBigIntBig { dst, idx })?;
        }
        Ok(dst)
    }
    /// Load the int literals the body's loops use into registers of their
    /// own, ahead of the body (a fast path: off under `ZIPP_PY_NOFAST`).
    /// `LoadBigInt` allocates for every value outside the VM's interned
    /// range on every execution, so a literal in a loop otherwise costs an
    /// allocation per iteration; the interned ones are left where they are.
    /// A BigInt is immutable and every use reads the register, so sharing it
    /// changes nothing. Bounded, so a literal-heavy body keeps its register
    /// budget.
    pub fn hoist_ints(&mut self, stmts: &[ast::Stmt]) -> R<()> {
        if !self.fast {
            return Ok(());
        }
        const MAX_HOISTED: usize = 32;
        let interned = crate::heap::INTERN_BIGINT_MIN..=crate::heap::INTERN_BIGINT_MAX;
        for value in super::nesting::loop_int_literals(stmts) {
            if interned.contains(&value) || self.hoisted.contains_key(&value) {
                continue;
            }
            if self.hoisted.len() >= MAX_HOISTED {
                break;
            }
            let dst = self.alloc()?;
            self.emit(Instr::LoadBigInt { dst, value })?;
            self.hoisted.insert(value, dst);
        }
        Ok(())
    }
    /// A runtime-internal count or index (a JS number, not a Python int).
    pub fn small_int(&mut self, val: i32) -> R<Reg> {
        let dst = self.alloc()?;
        self.emit(Instr::LoadInt { dst, val })?;
        Ok(dst)
    }
    pub fn float(&mut self, value: f64) -> R<Reg> {
        let dst = self.alloc()?;
        // Keyed by bits: 0.0 and -0.0 (and each NaN payload) stay distinct.
        let idx = match self.float_consts.get(&value.to_bits()) {
            Some(&idx) => idx,
            None => {
                let idx = self.proto.constants.len() as u32;
                self.proto.constants.push(crate::value::Value::num(value));
                self.float_consts.insert(value.to_bits(), idx);
                idx
            }
        };
        self.emit(Instr::LoadConst { dst, idx })?;
        Ok(dst)
    }
    pub fn none(&mut self) -> R<Reg> {
        let dst = self.alloc()?;
        self.emit(Instr::LoadNull { dst })?;
        Ok(dst)
    }
    pub fn boolean(&mut self, val: bool) -> R<Reg> {
        let dst = self.alloc()?;
        self.emit(Instr::LoadBool { dst, val })?;
        Ok(dst)
    }
    pub fn undefined(&mut self) -> R<Reg> {
        let dst = self.alloc()?;
        self.emit(Instr::LoadUndefined { dst })?;
        Ok(dst)
    }

    // ---- objects and calls into the runtime -----------------------------------------
    pub fn prop(&mut self, obj: Reg, key: &str) -> R<Reg> {
        let dst = self.alloc()?;
        let name = self.string_index(key);
        self.emit(Instr::GetProp { dst, obj, name })?;
        Ok(dst)
    }
    pub fn set_prop(&mut self, obj: Reg, key: &str, val: Reg) -> R<()> {
        let name = self.string_index(key);
        self.emit(Instr::SetProp {
            obj,
            name,
            val,
            strict: false,
        })?;
        Ok(())
    }
    pub fn arguments(&mut self, args: &[Reg]) -> R<(Reg, u16)> {
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
    pub fn array(&mut self, values: &[Reg]) -> R<Reg> {
        let dst = self.alloc()?;
        self.array_into(dst, values)?;
        Ok(dst)
    }
    pub fn array_into(&mut self, dst: Reg, values: &[Reg]) -> R<()> {
        let (arg_base, argc) = self.arguments(values)?;
        self.emit(Instr::NewArray {
            dst,
            arg_base,
            argc,
        })?;
        Ok(())
    }
    /// `runtime.method(args...)`. A plain Get and Call: the interpreter's
    /// fused `CallMethod` measured slower on the runtime object than the pair
    /// (its receiver-kind probes run before the method cache).
    pub fn helper(&mut self, method: &str, args: &[Reg]) -> R<Reg> {
        let callee = self.prop(self.r_rt, method)?;
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
    /// An annotation's value: the expression, or its source text when the
    /// module imports `annotations` from `__future__` (PEP 563), so forward
    /// references and `X | None` never evaluate at definition time.
    pub fn annotation_value(&mut self, expr: &ast::Expr, depth: usize) -> R<Reg> {
        if self.future_annotations {
            let range = expr.range();
            let start = u32::from(range.start()) as usize;
            let end = u32::from(range.end()) as usize;
            let text = self.unit.source.get(start..end).unwrap_or("").to_owned();
            return self.string(&text);
        }
        self.expr(expr, depth)
    }

    /// A function or module body under a catch-all handler that records
    /// this frame (file, line, name) on any exception passing through, then
    /// rethrows: the traceback CPython prints, at the cost of one handler
    /// push per call. The line register starts at the definition line so a
    /// failure before the first statement (an argument-count error) still
    /// names the frame.
    pub fn frame_guard(
        &mut self,
        line: i32,
        body: impl FnOnce(&mut Emitter<'a>) -> R<()>,
    ) -> R<()> {
        self.emit(Instr::LoadInt {
            dst: self.r_line,
            val: self.unit.module_index as i32 * 1_000_000 + line,
        })?;
        let ereg = self.alloc()?;
        let push = self.emit(Instr::PushHandler {
            catch_target: 0,
            catch_reg: ereg,
        })?;
        self.handler_depth += 1;
        body(self)?;
        self.handler_depth -= 1;
        let here = self.here();
        self.patch(push, here)?;
        self.forget_line();
        let exc = self.helper("addframe", &[ereg, REG_FUNC, self.r_line])?;
        self.emit(Instr::Throw { src: exc })?;
        Ok(())
    }
    /// `call(f, args, kwargs)` through the runtime (one helper frame around
    /// the callee's own).
    pub fn call_value(&mut self, f: Reg, args: Reg, kwargs: Reg) -> R<Reg> {
        self.helper("callv", &[f, args, kwargs])
    }
    /// A call with positional arguments only. Callables that need no
    /// argument shuffling carry a `fast` entry point (plain functions with
    /// only positional parameters, fixed-arity builtins, classes) which is
    /// called directly; the callee validates the count. Anything else goes
    /// through `bind`.
    pub fn call_positional(&mut self, f: Reg, args: Reg) -> R<Reg> {
        let none = self.none()?;
        let not_none = self.alloc()?;
        self.emit(Instr::Ne {
            dst: not_none,
            a: f,
            b: none,
        })?;
        let slow_none = self.jump_if_false(not_none)?;
        let fast = self.prop(f, "fast")?;
        let is_fn = self.typeof_is(fast, "function")?;
        let slow = self.jump_if_false(is_fn)?;
        let (arg_base, argc) = self.arguments(&[args])?;
        let dst = self.alloc()?;
        self.emit(Instr::CallWithThis {
            dst,
            callee: fast,
            this_v: f,
            arg_base,
            argc,
            name: NO_NAME,
        })?;
        let end = self.jump()?;
        let here = self.here();
        self.patch(slow_none, here)?;
        self.patch(slow, here)?;
        let result = self.call_value(f, args, none)?;
        self.emit(Instr::Move { dst, src: result })?;
        let here = self.here();
        self.patch(end, here)?;
        Ok(dst)
    }
    /// The callee side of the direct call path: a function whose parameters
    /// are all plain positionals checks the argument count itself (the
    /// `bind` path also checks, so both entry points agree). Generators keep
    /// the checked entry only, so the error is raised at the call.
    pub fn arity_guard(&mut self, count: usize) -> R<()> {
        if self.proto.is_generator {
            return Ok(());
        }
        let len = self.prop(REG_ARGS, "length")?;
        let expected = self.small_int(count as i32)?;
        let ok = self.alloc()?;
        self.emit(Instr::Eq {
            dst: ok,
            a: len,
            b: expected,
        })?;
        let skip = self.jump_if_true(ok)?;
        self.helper("arity", &[REG_FUNC, REG_ARGS])?;
        let here = self.here();
        self.patch(skip, here)?;
        Ok(())
    }
    /// Only plain positional parameters: no defaults, `*args`, `**kwargs`
    /// or keyword-only names. Returns the parameter count.
    pub fn simple_arity(args: &ast::Arguments) -> Option<usize> {
        let plain = args.posonlyargs.iter().chain(&args.args);
        if args.vararg.is_some()
            || args.kwarg.is_some()
            || !args.kwonlyargs.is_empty()
            || plain.clone().any(|a| a.default.is_some())
        {
            return None;
        }
        Some(plain.count())
    }
    /// `dst = typeof value === <name>` as one fused instruction.
    pub fn typeof_is(&mut self, value: Reg, name: &str) -> R<Reg> {
        let code = crate::bytecode::TYPEOF_NAMES
            .iter()
            .position(|n| *n == name)
            .map(|i| i as u8)
            .unwrap_or(255);
        let dst = self.alloc()?;
        self.emit(Instr::TypeOfIs {
            dst,
            a: value,
            code,
            neg: false,
        })?;
        Ok(dst)
    }
    /// Python truth of `value`: a JS boolean is its own truth (the common
    /// result of a comparison); anything else asks the runtime.
    pub fn truth(&mut self, value: Reg) -> R<Reg> {
        let dst = self.alloc()?;
        let is_bool = self.typeof_is(value, "boolean")?;
        let slow = self.jump_if_false(is_bool)?;
        self.emit(Instr::Move { dst, src: value })?;
        let end = self.jump()?;
        let here = self.here();
        self.patch(slow, here)?;
        let r = self.helper("truth", &[value])?;
        self.emit(Instr::Move { dst, src: r })?;
        let here = self.here();
        self.patch(end, here)?;
        Ok(dst)
    }
    pub fn cell_get(&mut self, cell: Reg) -> R<Reg> {
        self.prop(cell, "v")
    }
    pub fn cell_set(&mut self, cell: Reg, value: Reg) -> R<()> {
        self.set_prop(cell, "v", value)
    }

    // ---- names ------------------------------------------------------------------
    pub fn sym_kind(&self, name: &str) -> SymKind {
        match self.scope().symbols.get(name) {
            Some(k) => *k,
            None => match self.kind() {
                ScopeKind::Class => SymKind::Global,
                _ => SymKind::Global,
            },
        }
    }
    fn check_bound(&mut self, value: Reg, name: &str) -> R<()> {
        let unbound = self.unbound()?;
        let cond = self.alloc()?;
        self.emit(Instr::Eq {
            dst: cond,
            a: value,
            b: unbound,
        })?;
        let skip = self.jump_if_false(cond)?;
        let key = self.string(name)?;
        self.helper("unboundlocal", &[key])?;
        let end = self.here();
        self.patch(skip, end)?;
        Ok(())
    }
    /// Whether a read of `name` here can skip the unbound check: bound on
    /// every path to this point and never the target of a `del`.
    fn is_definite(&self, name: &str) -> bool {
        self.definite.contains(name) && !self.scope().deleted.contains(name)
    }
    /// Whether a walrus anywhere in the code this one runs in can rebind
    /// the local `name` (an inline comprehension runs in its parent's code).
    fn walrus_rebinds(&self, name: &str) -> bool {
        let scope = self.scope();
        scope.walrus_targets.contains(name)
            || scope.inline
                && scope
                    .parent
                    .is_some_and(|p| self.table.scopes[p].walrus_targets.contains(name))
    }
    pub fn load_name(&mut self, name: &str) -> R<Reg> {
        match self.sym_kind(name) {
            SymKind::Local | SymKind::Outer => {
                let reg = *self
                    .locals
                    .get(name)
                    .ok_or_else(|| format!("Python emitter: unreserved local {name}"))?;
                if !self.is_definite(name) {
                    self.check_bound(reg, name)?;
                }
                if self.walrus_rebinds(name) {
                    // A later operand's `name := ...` must not change this one.
                    let copy = self.alloc()?;
                    self.emit(Instr::Move { dst: copy, src: reg })?;
                    return Ok(copy);
                }
                Ok(reg)
            }
            SymKind::Cell | SymKind::Free => {
                let cell = self.cell_reg(name)?;
                let value = self.cell_get(cell)?;
                if !self.is_definite(name) {
                    self.check_bound(value, name)?;
                }
                Ok(value)
            }
            SymKind::ClassLocal => {
                let ns = self
                    .r_ns
                    .ok_or("Python emitter: class local outside a class")?;
                let globals = self.globals()?;
                let key = self.string(name)?;
                self.helper("nsload", &[ns, globals, key])
            }
            SymKind::Global => {
                let globals = self.globals()?;
                let key = self.string(name)?;
                if !self.fast {
                    return self.helper("gload", &[globals, key]);
                }
                self.global_read(globals, key)
            }
        }
    }
    /// A global read with fast paths on: the module dictionary's `get`,
    /// then the builtins', as the VM's own Map method calls (no runtime
    /// frame), and the `gload` helper only when both miss (it raises the
    /// NameError, and answers `__builtins__`). `get` returns `undefined`
    /// for a missing key and never for a bound name, so the miss test is
    /// exact; the lookups are the same two the helper starts with, in the
    /// same order, so shadowing a builtin at module level is seen at once.
    fn global_read(&mut self, globals: Reg, key: Reg) -> R<Reg> {
        let dst = self.alloc()?;
        let get = self.string_index("get");
        let undefined = self.undefined()?;
        let found = self.alloc()?;
        let (arg_base, argc) = self.arguments(&[key])?;
        self.emit(Instr::CallMethod { dst, obj: globals, name: get, arg_base, argc })?;
        self.emit(Instr::Ne { dst: found, a: dst, b: undefined })?;
        let hit = self.jump_if_true(found)?;
        let builtins = self.prop(self.r_rt, "BUILTINS")?;
        let (arg_base, argc) = self.arguments(&[key])?;
        self.emit(Instr::CallMethod { dst, obj: builtins, name: get, arg_base, argc })?;
        self.emit(Instr::Ne { dst: found, a: dst, b: undefined })?;
        let hit2 = self.jump_if_true(found)?;
        let r = self.helper("gload", &[globals, key])?;
        self.emit(Instr::Move { dst, src: r })?;
        let here = self.here();
        self.patch(hit, here)?;
        self.patch(hit2, here)?;
        Ok(dst)
    }
    pub fn cell_reg(&self, name: &str) -> R<Reg> {
        self.cells
            .get(name)
            .copied()
            .ok_or_else(|| format!("Python emitter: no cell for {name}"))
    }
    pub fn store_name(&mut self, name: &str, value: Reg) -> R<()> {
        match self.sym_kind(name) {
            SymKind::Local | SymKind::Outer => {
                let dst = *self
                    .locals
                    .get(name)
                    .ok_or_else(|| format!("Python emitter: unreserved local {name}"))?;
                self.emit(Instr::Move { dst, src: value })?;
                if self.speculative == 0 {
                    self.definite.insert(name.to_owned());
                }
            }
            SymKind::Cell | SymKind::Free => {
                let cell = self.cell_reg(name)?;
                self.cell_set(cell, value)?;
                if self.speculative == 0 {
                    self.definite.insert(name.to_owned());
                }
            }
            SymKind::ClassLocal => {
                let ns = self
                    .r_ns
                    .ok_or("Python emitter: class local outside a class")?;
                let key = self.string(name)?;
                self.helper("nsstore", &[ns, key, value])?;
            }
            SymKind::Global => {
                let globals = self.globals()?;
                let key = self.string(name)?;
                self.helper("gstore", &[globals, key, value])?;
            }
        }
        Ok(())
    }
    pub fn delete_name(&mut self, name: &str) -> R<()> {
        match self.sym_kind(name) {
            SymKind::Local | SymKind::Outer => {
                let dst = *self
                    .locals
                    .get(name)
                    .ok_or_else(|| format!("Python emitter: unreserved local {name}"))?;
                let unbound = self.unbound()?;
                self.emit(Instr::Move { dst, src: unbound })?;
                self.definite.remove(name);
            }
            SymKind::Cell | SymKind::Free => {
                let cell = self.cell_reg(name)?;
                let unbound = self.unbound()?;
                self.cell_set(cell, unbound)?;
                self.definite.remove(name);
            }
            SymKind::ClassLocal => {
                let ns = self
                    .r_ns
                    .ok_or("Python emitter: class local outside a class")?;
                let key = self.string(name)?;
                self.helper("nsdel", &[ns, key])?;
            }
            SymKind::Global => {
                let globals = self.globals()?;
                let key = self.string(name)?;
                self.helper("gdel", &[globals, key])?;
            }
        }
        Ok(())
    }

    // ---- nested code objects ----------------------------------------------------------
    /// Compile the scope defined at `node` into a new proto; returns its id.
    pub fn compile_child(
        &mut self,
        node: &impl Ranged,
        name: &str,
        qualname: String,
        body: impl FnOnce(&mut Emitter<'a>) -> R<()>,
    ) -> R<(u32, &'a Scope)> {
        let range = node.range();
        let key = (u32::from(range.start()), u32::from(range.end()));
        let scope_id = *self
            .table
            .by_offset
            .get(&key)
            .ok_or_else(|| self.error(node, "internal: scope not analysed"))?;
        // A code object compiled against another node's scope would, for
        // instance, lower a generator body into a plain function.
        if self.table.scopes[scope_id].name != name {
            return Err(self.error(node, "internal: scope does not match its node"));
        }
        if self.program.borrow().functions.len() >= MAX_FUNCTIONS {
            return Err(self.error(node, "function count limit exceeded"));
        }
        let mut child = Emitter::new(
            name,
            self.unit,
            self.rt_slot,
            self.line_slot,
            self.table,
            scope_id,
            self.project,
            self.program,
            qualname,
        )?;
        child.future_annotations = self.future_annotations;
        body(&mut child)?;
        let mut program = self.program.borrow_mut();
        let func_id = program.functions.len() as u32;
        program.functions.push(child.finish());
        Ok((func_id, &self.table.scopes[scope_id]))
    }
    /// The cells array a child needs: for each of its free names, the current
    /// scope's own cell or its captured cell of the same name.
    pub fn cells_for(&mut self, child: &Scope) -> R<Reg> {
        let mut regs = Vec::with_capacity(child.free_order.len());
        for name in &child.free_order {
            let reg = match self.cells.get(name) {
                Some(r) => *r,
                None => {
                    return Err(format!(
                        "Python emitter: {} captures {name:?} but {} has no cell for it",
                        child.name,
                        self.scope().name
                    ))
                }
            };
            regs.push(reg);
        }
        self.array(&regs)
    }
    /// Build a function object over `func_id` for scope `child`.
    #[allow(clippy::too_many_arguments)]
    pub fn make_function(
        &mut self,
        func_id: u32,
        child: &Scope,
        name: &str,
        qualname: &str,
        args: Option<&ast::Arguments>,
        defaults: Reg,
        kwdefault_names: Reg,
        kwdefault_values: Reg,
        doc: Reg,
    ) -> R<Reg> {
        let code = self.alloc()?;
        self.emit(Instr::MakeFunc { dst: code, func_id })?;
        let name_r = self.string(name)?;
        let qual_r = self.string(qualname)?;
        let (posonly, positional, kwonly, varargs, varkw) = match args {
            Some(a) => (
                a.posonlyargs.len(),
                a.posonlyargs.len() + a.args.len(),
                a.kwonlyargs.len(),
                a.vararg.is_some(),
                a.kwarg.is_some(),
            ),
            None => (0, child.params.len(), 0, false, false),
        };
        let mut name_regs = Vec::new();
        for p in child.params.iter().take(positional + kwonly) {
            name_regs.push(self.string(p)?);
        }
        let argnames = self.array(&name_regs)?;
        let posonly_r = self.small_int(posonly as i32)?;
        let positional_r = self.small_int(positional as i32)?;
        let varargs_r = self.boolean(varargs)?;
        let varkw_r = self.boolean(varkw)?;
        let cells = self.cells_for(child)?;
        let isgen = self.boolean(child.is_generator)?;
        let module = self.string(self.module_name())?;
        let globals = self.globals()?;
        self.helper(
            "func",
            &[
                code,
                name_r,
                qual_r,
                argnames,
                posonly_r,
                positional_r,
                varargs_r,
                varkw_r,
                defaults,
                kwdefault_names,
                kwdefault_values,
                cells,
                globals,
                isgen,
                doc,
                module,
            ],
        )
    }
    /// Evaluate a signature's defaults in the CURRENT scope: positional
    /// defaults as an array (aligned to the LAST positional parameters) and
    /// keyword-only defaults as parallel name/value arrays.
    pub fn defaults_of(&mut self, args: &ast::Arguments) -> R<(Reg, Reg, Reg)> {
        let mut pos = Vec::new();
        for a in args.posonlyargs.iter().chain(&args.args) {
            if let Some(d) = &a.default {
                pos.push(self.expr(d, 0)?);
            }
        }
        let mut names = Vec::new();
        let mut values = Vec::new();
        for a in &args.kwonlyargs {
            if let Some(d) = &a.default {
                names.push(self.string(a.def.arg.as_str())?);
                values.push(self.expr(d, 0)?);
            }
        }
        let pos = self.array(&pos)?;
        let names = self.array(&names)?;
        let values = self.array(&values)?;
        Ok((pos, names, values))
    }
    pub fn validate_args(&self, node: &impl Ranged, args: &ast::Arguments) -> R<()> {
        let mut seen = BTreeSet::new();
        for a in args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
        {
            if !seen.insert(a.def.arg.as_str()) {
                return Err(self.error(node, &format!("duplicate argument '{}'", a.def.arg)));
            }
        }
        Ok(())
    }
    pub fn is_builtin_module(name: &str) -> bool {
        let head = name.split('.').next().unwrap_or(name);
        BUILTIN_MODULES.contains(&head)
    }
    /// A project module, a namespace package (a folder holding project
    /// modules but no `__init__.py`), or a built-in module.
    /// The module's own name, without the `__main__` alias the entry gets:
    /// relative imports resolve against where a module actually sits.
    pub fn raw_module_name(&self) -> &'a str {
        self.project.module_names[self.unit.module_index as usize]
    }

    /// The package a relative import counts from, following CPython: a module
    /// that has submodules is a package and counts from itself; anything else
    /// counts from its parent. `None` means there is nothing above it.
    pub fn package_of(&self, name: &str) -> Option<&'a str> {
        let prefix = format!("{name}.");
        if self.project.modules.iter().any(|m| m.starts_with(&prefix)) {
            return self.project.modules.iter().copied().find(|m| *m == name).or_else(|| {
                // A namespace package has submodules but no module of its own;
                // the name is still what a relative import counts from.
                self.project.modules.iter().copied().find(|m| m.starts_with(&prefix)).map(|m| &m[..name.len()])
            });
        }
        match name.rfind('.') {
            Some(cut) => self.project.modules.iter().copied().find(|m| *m == &name[..cut]).or_else(|| {
                self.project.modules.iter().copied().find(|m| m.starts_with(&name[..cut + 1])).map(|m| &m[..cut])
            }),
            None => None,
        }
    }

    pub fn module_exists(&self, name: &str) -> bool {
        if self.project.modules.contains(name) || Self::is_builtin_module(name) {
            return true;
        }
        let prefix = format!("{name}.");
        self.project.modules.iter().any(|m| m.starts_with(&prefix))
    }
}
