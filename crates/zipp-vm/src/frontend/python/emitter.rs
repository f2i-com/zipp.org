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
use std::collections::{BTreeMap, BTreeSet};

pub(super) type R<T> = Result<T, String>;
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
    // Persistent registers set up by the prologue.
    pub r_rt: Reg,
    pub r_stop: Reg,
    pub r_unb: Reg,
    pub r_globals: Reg,
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
    pub control_depth: usize,
    pub loops: Vec<LoopCtx>,
    pub handler_depth: usize,
    pub qualname: String,
    last_line: i32,
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
            r_stop: 0,
            r_unb: 0,
            r_globals: 0,
            r_ns: None,
            r_line: 0,
            locals: BTreeMap::new(),
            cells: BTreeMap::new(),
            definite: BTreeSet::new(),
            speculative: 0,
            control_depth: 0,
            loops: Vec::new(),
            handler_depth: 0,
            qualname,
            last_line: -1,
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

    fn prologue(&mut self) -> R<()> {
        self.r_rt = self.alloc()?;
        self.emit(Instr::LoadGlobal {
            dst: self.r_rt,
            idx: self.rt_slot,
        })?;
        self.r_stop = self.prop(self.r_rt, "STOP")?;
        self.r_unb = self.prop(self.r_rt, "UNBOUND")?;
        self.r_globals = self.prop(REG_FUNC, "globals")?;
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
                    let reg = self.alloc()?;
                    self.emit(Instr::Move {
                        dst: reg,
                        src: self.r_unb,
                    })?;
                    self.locals.insert(name.clone(), reg);
                }
                SymKind::Cell => {
                    let cell = self.helper("cell", &[self.r_unb])?;
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
        let offset = u32::from(node.range().start()) as usize;
        let prefix = self.unit.source.get(..offset).unwrap_or("");
        (prefix.bytes().filter(|c| *c == b'\n').count() + 1) as i32
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
        if let Some(index) = self.proto.string_constants.iter().position(|s| s == value) {
            return index as u32;
        }
        let index = self.proto.string_constants.len() as u32;
        self.proto.string_constants.push(value.to_owned());
        index
    }
    pub fn string(&mut self, value: &str) -> R<Reg> {
        let dst = self.alloc()?;
        let si = self.string_index(value);
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
    /// A runtime-internal count or index (a JS number, not a Python int).
    pub fn small_int(&mut self, val: i32) -> R<Reg> {
        let dst = self.alloc()?;
        self.emit(Instr::LoadInt { dst, val })?;
        Ok(dst)
    }
    pub fn float(&mut self, value: f64) -> R<Reg> {
        let dst = self.alloc()?;
        let idx = self.proto.constants.len() as u32;
        self.proto.constants.push(crate::value::Value::num(value));
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
        let (arg_base, argc) = self.arguments(values)?;
        let dst = self.alloc()?;
        self.emit(Instr::NewArray {
            dst,
            arg_base,
            argc,
        })?;
        Ok(dst)
    }
    /// `runtime.method(args...)`.
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
    /// Both operands are BigInts (Python ints): jump to the returned label
    /// otherwise. The caller patches it to the slow path.
    pub fn both_ints(&mut self, a: Reg, b: Reg) -> R<Vec<usize>> {
        let ta = self.typeof_is(a, "bigint")?;
        let j1 = self.jump_if_false(ta)?;
        let tb = self.typeof_is(b, "bigint")?;
        let j2 = self.jump_if_false(tb)?;
        Ok(vec![j1, j2])
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
        let cond = self.alloc()?;
        self.emit(Instr::Eq {
            dst: cond,
            a: value,
            b: self.r_unb,
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
    pub fn load_name(&mut self, name: &str) -> R<Reg> {
        match self.sym_kind(name) {
            SymKind::Local => {
                let reg = *self
                    .locals
                    .get(name)
                    .ok_or_else(|| format!("Python emitter: unreserved local {name}"))?;
                if !self.is_definite(name) {
                    self.check_bound(reg, name)?;
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
                let key = self.string(name)?;
                self.helper("nsload", &[ns, self.r_globals, key])
            }
            SymKind::Global => {
                let key = self.string(name)?;
                self.helper("gload", &[self.r_globals, key])
            }
        }
    }
    pub fn cell_reg(&self, name: &str) -> R<Reg> {
        self.cells
            .get(name)
            .copied()
            .ok_or_else(|| format!("Python emitter: no cell for {name}"))
    }
    pub fn store_name(&mut self, name: &str, value: Reg) -> R<()> {
        match self.sym_kind(name) {
            SymKind::Local => {
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
                let key = self.string(name)?;
                self.helper("gstore", &[self.r_globals, key, value])?;
            }
        }
        Ok(())
    }
    pub fn delete_name(&mut self, name: &str) -> R<()> {
        match self.sym_kind(name) {
            SymKind::Local => {
                let dst = *self
                    .locals
                    .get(name)
                    .ok_or_else(|| format!("Python emitter: unreserved local {name}"))?;
                self.emit(Instr::Move {
                    dst,
                    src: self.r_unb,
                })?;
                self.definite.remove(name);
            }
            SymKind::Cell | SymKind::Free => {
                let cell = self.cell_reg(name)?;
                self.cell_set(cell, self.r_unb)?;
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
                let key = self.string(name)?;
                self.helper("gdel", &[self.r_globals, key])?;
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
        let offset = u32::from(node.range().start());
        let scope_id = *self
            .table
            .by_offset
            .get(&offset)
            .ok_or_else(|| self.error(node, "internal: scope not analysed"))?;
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
        let module = self.string(self.project.module_names[self.unit.module_index as usize])?;
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
                self.r_globals,
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
    pub fn module_exists(&self, name: &str) -> bool {
        self.project.modules.contains(name) || Self::is_builtin_module(name)
    }
}
