//! Register-based compiler.
//!
//! Emits register opcodes (ROp) instead of stack opcodes.
//! Locals ARE registers (0..num_locals-1). Temporaries are allocated above.
//! `compile_expression` returns a register index. `compile_expression_into`
//! writes into a specific register.
//!
//! The compiler is split across sibling files (`impl RCompiler` in each):
//!
//! * [`mod.rs`] (this file) — orchestration (`compile_program`),
//!   statements, expressions, loops, assignments, bytecode helpers.
//! * [`functions`] — `compile_function_literal` + `compile_class_literal`
//!   (function body / class member compilation).

mod assignment;
mod exceptions;
mod functions;
mod loops;

use rustc_hash::{FxHashMap, FxHashSet};
use std::rc::Rc;

use crate::intern::intern_str;

use crate::ast::ClassMember;
use crate::ast::{
    ArrayBindingItem, BindingPattern, BindingTarget, HashEntry, ObjectBindingItem,
};
use crate::ast::{Expression, Program, Statement, VariableKind};
use crate::bytecode::Bytecode;
use crate::object::{ClassObject, Object, RegExpObject};
use crate::rcode::{rmake, ROp};

const GLOBALS_SIZE: usize = 65_536;

pub struct RCompiler {
    instructions: Vec<u8>,
    constants: Vec<Object>,
    globals: FxHashMap<String, u16>,
    next_global: u16,
    /// Maps local variable names to register indices.
    locals: FxHashMap<String, u16>,
    class_defs: FxHashMap<String, ClassObject>,
    /// Number of registers allocated for named locals.
    num_locals: u16,
    /// Next temporary register index. Always >= num_locals.
    next_temp: u16,
    /// Maximum register index used (for register_count in bytecode).
    max_reg: u16,
    is_function_scope: bool,
    loop_stack: Vec<LoopContext>,
    temp_counter: usize,
    try_stack: Vec<TryContext>,
    // Constant deduplication maps
    constant_strings: FxHashMap<Rc<str>, u16>,
    constant_ints: FxHashMap<i64, u16>,
    constant_floats: FxHashMap<u64, u16>,
    // Inline cache slot counter
    next_cache_slot: u16,
    /// Names that are referenced inside nested function bodies.
    /// When Some, only these names get global slots + mirroring at top level.
    /// None means all locals get globals (used inside inner function scopes).
    captured_names: Option<FxHashSet<String>>,
    // Names declared with `const` — assignment to these is a compile error.
    const_bindings: FxHashSet<String>,
    /// Global slot indices that were freshly allocated because a parameter
    /// shadows a captured name from the outer scope (IIFE pattern).
    /// Inner closures created in this scope that reference these slots need
    /// `MakeClosure` to snapshot the values at creation time.
    param_shadow_slots: FxHashSet<u16>,
    /// Sticky overflow error. The compiler emits `u16` indices for
    /// every register / global / cache-slot / constant-table entry,
    /// so any one of those counters exceeding `u16::MAX + 1` would
    /// silently wrap in release mode (`overflow-checks = false` in
    /// the workspace profile) and emit bytecode that aliases the
    /// wrong slot. Each counter increment now goes through a checked
    /// helper that records the first overflow here; `compile_program*`
    /// checks the field and surfaces a clean `Err` instead of letting
    /// corrupt bytecode through to the unsafe VM dispatch.
    overflow_error: Option<String>,
}

pub(super) struct LoopContext {
    label: Option<String>,
    continue_target: usize,
    break_positions: Vec<usize>,
    continue_positions: Vec<usize>,
}

pub(super) struct TryContext {
    exception_temp: String,
    throw_jumps: Vec<usize>,
    /// When a try block has a finally, returns inside try/catch are deferred:
    /// the return value is stored in `return_temp` and control jumps to the
    /// finally block. After finally executes, if `return_flag_temp` is true,
    /// the actual return happens.
    has_finally: bool,
    return_temp: Option<String>,
    return_flag_temp: Option<String>,
    return_jumps: Vec<usize>,
}

impl Default for RCompiler {
    fn default() -> Self {
        Self::new()
    }
}

impl RCompiler {
    pub fn new() -> Self {
        Self {
            instructions: vec![],
            constants: vec![],
            globals: FxHashMap::default(),
            next_global: 0,
            locals: FxHashMap::default(),
            class_defs: FxHashMap::default(),
            num_locals: 0,
            next_temp: 0,
            max_reg: 0,
            is_function_scope: false,
            loop_stack: vec![],
            temp_counter: 0,
            try_stack: vec![],
            constant_strings: FxHashMap::default(),
            constant_ints: FxHashMap::default(),
            constant_floats: FxHashMap::default(),
            next_cache_slot: 0,
            captured_names: None,
            const_bindings: FxHashSet::default(),
            param_shadow_slots: FxHashSet::default(),
            overflow_error: None,
        }
    }

    /// Record a sticky compile-time overflow. The first message wins
    /// so the surfaced error points at the *root cause* counter
    /// rather than a downstream symptom.
    fn record_overflow(&mut self, msg: &str) {
        if self.overflow_error.is_none() {
            self.overflow_error = Some(msg.to_string());
        }
    }

    /// Saturating-add a u16 counter; records `msg` and returns the
    /// pre-increment value when overflow would occur. Pairs with
    /// `record_overflow` so overflow is surfaced via the public
    /// `compile_program*` `Result` instead of silently wrapping.
    #[inline]
    fn checked_inc_u16(&mut self, counter: u16, msg: &str) -> u16 {
        match counter.checked_add(1) {
            Some(v) => v,
            None => {
                self.record_overflow(msg);
                counter
            }
        }
    }

    /// Create a compiler pre-populated with an existing globals table.
    /// Used by `eval_in_context` to compile expressions that can access
    /// the script's global variables and functions.
    pub fn with_globals(globals: &FxHashMap<String, u16>) -> Self {
        let mut compiler = Self::new();
        for (name, &slot) in globals {
            compiler.globals.insert(name.clone(), slot);
        }
        // `m + 1` wraps to 0 if any slot is `u16::MAX`. Use
        // `checked_add` and surface as a sticky overflow so the
        // `compile_program*` caller sees a real error instead of
        // re-allocating slot 0 on the next `ensure_global_slot`.
        compiler.next_global = globals
            .values()
            .copied()
            .max()
            .map(|m| m.checked_add(1).unwrap_or_else(|| {
                compiler
                    .overflow_error
                    .get_or_insert_with(|| "global slot table is full".to_string());
                m
            }))
            .unwrap_or(0);
        compiler
    }

    fn new_function_scope(
        mut globals: FxHashMap<String, u16>,
        next_global: u16,
        parameters: &[String],
        captured_names: FxHashSet<String>,
    ) -> Self {
        let mut locals = FxHashMap::default();
        for (i, param) in parameters.iter().enumerate() {
            locals.insert(param.clone(), i as u16);
            // Always remove inherited globals for parameters. Two reasons:
            // 1. Captured params need fresh global slots (for closure capture)
            // 2. Uncaptured params must not be reloaded from parent globals
            //    by reload_locals_from_globals after function calls.
            globals.remove(param);
        }
        let num_locals = parameters.len() as u16;

        Self {
            instructions: vec![],
            constants: vec![],
            globals,
            next_global,
            locals,
            class_defs: FxHashMap::default(),
            num_locals,
            next_temp: num_locals,
            max_reg: if num_locals > 0 { num_locals - 1 } else { 0 },
            is_function_scope: true,
            loop_stack: vec![],
            temp_counter: 0,
            try_stack: vec![],
            constant_strings: FxHashMap::default(),
            constant_ints: FxHashMap::default(),
            constant_floats: FxHashMap::default(),
            next_cache_slot: 0,
            captured_names: Some(captured_names),
            const_bindings: FxHashSet::default(),
            param_shadow_slots: FxHashSet::default(),
            overflow_error: None,
        }
    }

    /// Returns true if a variable needs a global slot for inner function access.
    fn needs_global(&self, name: &str) -> bool {
        match &self.captured_names {
            Some(set) => set.contains(name),
            None => true, // inner function scopes always mirror
        }
    }

    // ── Register allocation ──────────────────────────────────────────────

    /// Allocate a temporary register.
    ///
    /// Saturates `next_temp` at `u16::MAX` and records a sticky
    /// overflow when more than 65 535 registers would be needed —
    /// `compile_program*` then surfaces the failure as a clean `Err`
    /// rather than letting an aliased register index escape.
    fn alloc_temp(&mut self) -> u16 {
        let r = self.next_temp;
        self.next_temp = self.checked_inc_u16(r, "register count exceeded u16::MAX");
        if r > self.max_reg {
            self.max_reg = r;
        }
        r
    }

    /// Position `next_temp` at `base + offset` for the next
    /// `alloc_temp` call. Used by argument-packing / array-literal /
    /// hash-literal emit code where each iteration of the loop must
    /// land its temp at a deterministic register so the emitted
    /// `Call` / `Array` / `Hash` opcode can read a contiguous window.
    /// Goes through `u16::try_from` so an overflow records the sticky
    /// compile error instead of wrapping to a register the call site
    /// would then alias.
    fn set_next_temp_to(&mut self, base: u16, offset: usize) {
        match u16::try_from(base as usize + offset) {
            Ok(v) => self.next_temp = v,
            Err(_) => {
                self.record_overflow("register count exceeded u16::MAX");
                // Saturate so any subsequent alloc_temp also flags
                // overflow and the compiler bails out rather than
                // emitting bytecode that references aliased registers.
                self.next_temp = u16::MAX;
            }
        }
    }

    /// Save temp state; call before compiling a sub-expression to scope temps.
    fn save_temps(&self) -> u16 {
        self.next_temp
    }

    /// Restore temp state; frees temps allocated after save point.
    fn restore_temps(&mut self, saved: u16) {
        self.next_temp = saved;
    }

    /// Ensure a named local has a register. Returns its register index.
    ///
    /// `num_locals` is checked-incremented; an overflow records the
    /// sticky compile error so subsequent allocations don't silently
    /// reuse register `0`.
    fn ensure_local(&mut self, name: &str) -> u16 {
        if let Some(&r) = self.locals.get(name) {
            return r;
        }
        let r = self.num_locals;
        self.locals.insert(name.to_string(), r);
        self.num_locals = self.checked_inc_u16(r, "local register count exceeded u16::MAX");
        // Keep next_temp above locals
        if self.next_temp < self.num_locals {
            self.next_temp = self.num_locals;
        }
        if r > self.max_reg {
            self.max_reg = r;
        }
        r
    }

    /// Ensure a binding slot exists (local in function scope, global otherwise).
    // Built-in global names that should always resolve as globals
    #[allow(dead_code)]
    const BUILTIN_GLOBALS: &'static [&'static str] = &[
        "Object", "Array", "Math", "JSON", "Symbol", "Reflect", "Proxy",
        "Number", "String", "Boolean", "RegExp", "Date", "Error", "TypeError",
        "RangeError", "SyntaxError", "ReferenceError", "Map", "Set", "WeakMap",
        "WeakSet", "Promise", "ArrayBuffer", "Int8Array", "Uint8Array",
        "Float32Array", "Float64Array", "DataView", "Intl",
    ];

    fn ensure_binding_slot(&mut self, name: &str) -> Result<BindingSlot, String> {
        if self.is_function_scope {
            Ok(BindingSlot::Local(self.ensure_local(name)))
        } else {
            Ok(BindingSlot::Global(self.ensure_global_slot(name)?))
        }
    }

    fn ensure_global_slot(&mut self, name: &str) -> Result<u16, String> {
        if let Some(&idx) = self.globals.get(name) {
            return Ok(idx);
        }
        if self.next_global as usize >= GLOBALS_SIZE {
            return Err("global symbol table overflow".to_string());
        }
        let idx = self.next_global;
        self.globals.insert(name.to_string(), idx);
        // `idx + 1` would wrap silently when `idx == u16::MAX` because
        // `GLOBALS_SIZE == u16::MAX + 1` — the `>=` check above passes
        // for `idx == 65 535` and the bare `+= 1` then wrapped to 0,
        // letting the next call hand out slot 0 again. `checked_add`
        // catches this; we surface a real error instead of corrupting
        // the globals table.
        self.next_global = idx
            .checked_add(1)
            .ok_or_else(|| "global slot counter overflow".to_string())?;
        Ok(idx)
    }

    // ── Top-level entry ──────────────────────────────────────────────────

    /// Compile for persistent execution: all top-level bindings are mirrored
    /// to global slots so they remain accessible after run_register() completes.
    pub fn compile_program_persistent(mut self, program: &Program) -> Result<Bytecode, String> {
        self.is_function_scope = true;
        // Scan for variables captured by nested closures, PLUS all top-level
        // declarations (needed for eval_in_context to read __result etc.).
        // This ensures program-scope vars get globals while function-internal
        // vars only get globals if actually captured by inner closures.
        let mut captured = scan_captured_names(&program.statements);
        // Add all top-level declarations so they're accessible via eval_in_context
        for stmt in &program.statements {
            match stmt {
                Statement::Let { name, .. } => { captured.insert(name.clone()); }
                Statement::LetPattern { pattern, .. } => {
                    Self::collect_pattern_names(pattern, &mut captured);
                }
                Statement::FunctionDecl { name, .. } => { captured.insert(name.clone()); }
                _ => {}
            }
        }
        // Also collect var declarations (hoisted to program scope)
        let mut var_names = Vec::new();
        Self::collect_var_names(&program.statements, &mut var_names);
        captured.extend(var_names);
        self.captured_names = Some(captured);
        self.compile_program_inner(program)
    }

    pub fn compile_program(mut self, program: &Program) -> Result<Bytecode, String> {
        // Use function scope at top level so all `let` bindings become register
        // locals. This enables fused opcodes (TestLtConstJump, AddRegConst, etc.)
        // Only mirror variables to globals that are actually referenced by nested
        // function bodies (captured_names scan). This avoids the overhead of
        // SetGlobal on every assignment and GetGlobal reload after every call.
        self.is_function_scope = true;
        self.captured_names = Some(scan_captured_names(&program.statements));
        self.compile_program_inner(program)
    }

    fn compile_program_inner(mut self, program: &Program) -> Result<Bytecode, String> {
        self.hoist_var_declarations(&program.statements)?;

        let mut last_reg = None;
        for stmt in program.statements.iter() {
            // Propagate compile errors instead of silently skipping. The
            // old behaviour masked real bugs (e.g. assignment to const
            // variables printed a stderr warning and otherwise succeeded).
            // If any call site still needs tolerant compilation, it should
            // catch the error at the `eval` / `compile_script` boundary.
            last_reg = self.compile_statement(stmt)?;
            // Free temps after each top-level statement
            self.next_temp = self.num_locals;
        }

        // If there's a last expression value, emit HaltValue; otherwise Halt.
        if let Some(r) = last_reg {
            self.emit(ROp::HaltValue, &[r]);
        } else {
            self.emit(ROp::Halt, &[]);
        }

        // Surface any sticky overflow recorded during emission. A
        // counter that overflowed mid-compile would otherwise feed
        // aliased indices into bytecode the unsafe VM dispatch trusts.
        if let Some(msg) = self.overflow_error.take() {
            return Err(msg);
        }

        let mut bytecode = Bytecode::with_cache_slots(
            self.instructions,
            self.constants,
            vec![],
            self.next_cache_slot,
            0,
            self.max_reg + 1,
        );
        bytecode.globals_table = self.globals.iter().map(|(k, &v)| (k.clone(), v)).collect();
        // Export the actual high-water mark so embedders that
        // create runtime globals don't collide with private slots
        // an inner closure already claimed for its captured names.
        bytecode.next_global_slot = self.next_global;
        Ok(bytecode)
    }

    // ── Statement compilation ────────────────────────────────────────────
    // Returns Some(reg) if the statement produced a value (expression stmt).

    fn compile_statement(&mut self, stmt: &Statement) -> Result<Option<u16>, String> {
        match stmt {
            Statement::Let { name, value, kind } => {
                if *kind == VariableKind::Const {
                    self.const_bindings.insert(name.clone());
                }
                let slot = self.ensure_binding_slot(name)?;
                match slot {
                    BindingSlot::Local(r) => {
                        self.compile_expression_into(value, r)?;
                        if self.is_function_scope && self.needs_global(name) {
                            let g = self.ensure_global_slot(name)?;
                            self.emit(ROp::SetGlobal, &[g, r]);
                        }
                    }
                    BindingSlot::Global(g) => {
                        let r = self.compile_expression(value)?;
                        self.emit(ROp::SetGlobal, &[g, r]);
                    }
                }
                Ok(None)
            }
            Statement::LetPattern {
                pattern,
                value,
                kind,
            } => {
                if *kind == VariableKind::Const {
                    Self::collect_pattern_names(pattern, &mut self.const_bindings);
                }
                // Pre-declare all binding names so their local registers are
                // allocated before the source temp, preventing ensure_local
                // from claiming the register holding the source object.
                self.pre_declare_pattern_locals(pattern);
                let src = self.compile_expression(value)?;
                self.assign_pattern(pattern, src)?;
                Ok(None)
            }
            Statement::Return { value } => {
                let r = self.compile_expression(value)?;
                // If inside a try-with-finally, defer the return
                if let Some(ctx) = self.try_stack.last() {
                    if ctx.has_finally {
                        if let (Some(ref rt), Some(ref rf)) = (ctx.return_temp.clone(), ctx.return_flag_temp.clone()) {
                            self.store_identifier(rt, r)?;
                            let true_r = self.alloc_temp();
                            self.emit(ROp::LoadTrue, &[true_r]);
                            self.store_identifier(rf, true_r)?;
                            let jmp = self.emit(ROp::Jump, &[9999]);
                            // Store the jump position on the try context
                            let ctx_mut = self.try_stack.last_mut().unwrap();
                            ctx_mut.return_jumps.push(jmp);
                            return Ok(None);
                        }
                    }
                }
                self.emit(ROp::Return, &[r]);
                Ok(None)
            }
            Statement::ReturnVoid => {
                // If inside a try-with-finally, defer the return
                if let Some(ctx) = self.try_stack.last() {
                    if ctx.has_finally {
                        if let (Some(ref rt), Some(ref rf)) = (ctx.return_temp.clone(), ctx.return_flag_temp.clone()) {
                            let undef_r = self.alloc_temp();
                            self.emit(ROp::LoadUndef, &[undef_r]);
                            self.store_identifier(rt, undef_r)?;
                            let true_r = self.alloc_temp();
                            self.emit(ROp::LoadTrue, &[true_r]);
                            self.store_identifier(rf, true_r)?;
                            let jmp = self.emit(ROp::Jump, &[9999]);
                            let ctx_mut = self.try_stack.last_mut().unwrap();
                            ctx_mut.return_jumps.push(jmp);
                            return Ok(None);
                        }
                    }
                }
                self.emit(ROp::ReturnUndef, &[]);
                Ok(None)
            }
            Statement::Expression(expr) => {
                let r = self.compile_expression(expr)?;
                Ok(Some(r))
            }
            Statement::Block(statements) => {
                let shadowed = self.enter_block_scope(statements);
                let mut last = None;
                for s in statements {
                    last = self.compile_statement(s)?;
                }
                self.exit_block_scope(shadowed);
                Ok(last)
            }
            Statement::MultiLet(statements) => {
                // Multi-declaration: let a = 1, b = 2; — no block scoping
                let mut last = None;
                for s in statements {
                    last = self.compile_statement(s)?;
                }
                Ok(last)
            }
            Statement::While { condition, body } => {
                self.compile_while_statement(condition, body, None)?;
                Ok(None)
            }
            Statement::For {
                init,
                condition,
                update,
                body,
            } => {
                self.compile_for_statement(
                    init.as_deref(),
                    condition.as_ref(),
                    update.as_ref(),
                    body,
                    None,
                )?;
                Ok(None)
            }
            Statement::ForOf {
                binding,
                iterable,
                body,
            } => {
                self.compile_for_of_statement(binding, iterable, body, None)?;
                Ok(None)
            }
            Statement::ForIn {
                var_name,
                iterable,
                body,
            } => {
                self.compile_for_in_statement(var_name, iterable, body, None)?;
                Ok(None)
            }
            Statement::FunctionDecl {
                name,
                parameters,
                body,
                is_async,
                is_generator,
            } => {
                let function_expr = Expression::Function {
                    parameters: parameters.clone(),
                    body: body.clone(),
                    is_async: *is_async,
                    is_generator: *is_generator,
                    is_arrow: false,
                };
                let r = self.compile_expression(&function_expr)?;
                self.store_binding(name, r)?;
                Ok(None)
            }
            Statement::ClassDecl {
                name,
                extends,
                members,
            } => {
                let class_name = name.as_deref().unwrap_or("");
                let extends_name = match extends.as_deref() {
                    Some(Expression::Identifier(s)) => Some(s.as_str()),
                    _ => None,
                };
                // Pre-register class name in globals so methods can reference
                // the class via the same global slot (e.g., static methods that
                // reference the class by name like `Counter.count`).
                // Only do this for non-function scope where store_binding uses
                // globals directly, OR ensure we also pre-register the local.
                if let Some(n) = name {
                    if self.is_function_scope {
                        // In function scope, store_binding will create a local AND
                        // mirror to global. Pre-register both so child compilers
                        // see the same slot for the class name.
                        self.ensure_local(n);
                        if self.needs_global(n) {
                            let _ = self.ensure_global_slot(n);
                        }
                    } else {
                        let _ = self.ensure_global_slot(n);
                    }
                }
                let class_obj = self.compile_class_literal(class_name, extends_name, members)?;
                if let Some(n) = name {
                    self.class_defs.insert(n.clone(), class_obj.clone());
                }
                let has_static_init = !class_obj.static_initializers.is_empty();
                let idx = self.add_constant(Object::Class(Box::new(class_obj)));
                let r = self.alloc_temp();
                self.emit(ROp::LoadConst, &[r, idx]);
                if has_static_init {
                    self.emit(ROp::InitClass, &[r]);
                }
                if let Some(n) = name {
                    self.store_binding(n, r)?;
                }
                Ok(None)
            }
            Statement::Throw { value } => {
                self.compile_throw_statement(value)?;
                Ok(None)
            }
            Statement::Try {
                try_block,
                catch_param,
                catch_block,
                finally_block,
            } => {
                let r = self.compile_try_statement(
                    try_block,
                    catch_param.as_deref(),
                    catch_block.as_deref(),
                    finally_block.as_deref(),
                )?;
                Ok(r)
            }
            Statement::Labeled { label, statement } => match statement.as_ref() {
                Statement::While { condition, body } => {
                    self.compile_while_statement(condition, body, Some(label.as_str()))?;
                    Ok(None)
                }
                Statement::DoWhile { body, condition } => {
                    self.compile_do_while_statement(body, condition, Some(label.as_str()))?;
                    Ok(None)
                }
                Statement::Switch {
                    discriminant,
                    cases,
                } => {
                    self.compile_switch_statement(discriminant, cases, Some(label.as_str()))?;
                    Ok(None)
                }
                Statement::For {
                    init,
                    condition,
                    update,
                    body,
                } => {
                    self.compile_for_statement(
                        init.as_deref(),
                        condition.as_ref(),
                        update.as_ref(),
                        body,
                        Some(label.as_str()),
                    )?;
                    Ok(None)
                }
                Statement::ForOf {
                    binding,
                    iterable,
                    body,
                } => {
                    self.compile_for_of_statement(binding, iterable, body, Some(label.as_str()))?;
                    Ok(None)
                }
                Statement::ForIn {
                    var_name,
                    iterable,
                    body,
                } => {
                    self.compile_for_in_statement(var_name, iterable, body, Some(label.as_str()))?;
                    Ok(None)
                }
                // Labeled block: `label: { ... break label; ... }`
                // Compile as a block with break support
                _ => {
                    self.loop_stack.push(LoopContext {
                        label: Some(label.clone()),
                        continue_target: self.instructions.len(),
                        break_positions: vec![],
                        continue_positions: vec![],
                    });
                    let _ = self.compile_statement(statement)?;
                    let after_block = self.instructions.len();
                    let ctx = self.loop_stack.pop().unwrap();
                    for pos in ctx.break_positions {
                        self.patch_jump(pos, after_block);
                    }
                    Ok(None)
                }
            },
            Statement::Break { label } => {
                let pos = self.emit(ROp::Jump, &[9999]);
                let loop_ctx = self.find_loop_ctx_mut(label.as_deref())?;
                loop_ctx.break_positions.push(pos);
                Ok(None)
            }
            Statement::Continue { label } => {
                let pos = self.emit(ROp::Jump, &[9999]);
                let loop_ctx = self.find_loop_ctx_mut(label.as_deref())?;
                loop_ctx.continue_positions.push(pos);
                Ok(None)
            }
            Statement::DoWhile { body, condition } => {
                self.compile_do_while_statement(body, condition, None)?;
                Ok(None)
            }
            Statement::Switch {
                discriminant,
                cases,
            } => {
                self.compile_switch_statement(discriminant, cases, None)?;
                Ok(None)
            }
            Statement::Debugger => {
                // No-op in sandboxed interpreter
                Ok(None)
            }
        }
    }

    /// Store a value register into the appropriate binding (local or global).
    fn store_binding(&mut self, name: &str, src: u16) -> Result<(), String> {
        if self.is_function_scope {
            let r = self.ensure_local(name);
            if r != src {
                self.emit(ROp::Move, &[r, src]);
            }
            // Mirror to global only if an inner function references this name
            if self.needs_global(name) {
                let g = self.ensure_global_slot(name)?;
                self.emit(ROp::SetGlobal, &[g, r]);
            }
        } else {
            let g = self.ensure_global_slot(name)?;
            self.emit(ROp::SetGlobal, &[g, src]);
        }
        Ok(())
    }

    // ── Expression compilation ───────────────────────────────────────────
    // Returns the register index holding the result.

    fn compile_expression(&mut self, expr: &Expression) -> Result<u16, String> {
        let dst = self.alloc_temp();
        self.compile_expression_into(expr, dst)?;
        Ok(dst)
    }

    /// Compile expression into a specific destination register.
    fn compile_expression_into(&mut self, expr: &Expression, dst: u16) -> Result<(), String> {
        match expr {
            Expression::Integer(v) => {
                let idx = self.add_constant_int(*v);
                self.emit(ROp::LoadConst, &[dst, idx]);
            }
            Expression::BigInt(v) => {
                let idx = self.add_constant(Object::BigInt(*v));
                self.emit(ROp::LoadConst, &[dst, idx]);
            }
            Expression::Float(v) => {
                let idx = self.add_constant_float(*v);
                self.emit(ROp::LoadConst, &[dst, idx]);
            }
            Expression::String(v) => {
                let idx = self.add_constant_string(Rc::from(v.as_str()));
                self.emit(ROp::LoadConst, &[dst, idx]);
            }
            Expression::RegExp { pattern, flags } => {
                let idx = self.add_constant(Object::RegExp(Box::new(RegExpObject {
                    pattern: pattern.clone(),
                    flags: flags.clone(),
                })));
                self.emit(ROp::LoadConst, &[dst, idx]);
            }
            Expression::Boolean(v) => {
                self.emit(
                    if *v { ROp::LoadTrue } else { ROp::LoadFalse },
                    &[dst],
                );
            }
            Expression::Null => {
                self.emit(ROp::LoadNull, &[dst]);
            }
            Expression::Identifier(name) => {
                self.load_identifier_into(name, dst)?;
            }
            Expression::This => {
                self.load_identifier_into("this", dst)?;
            }
            Expression::Super => {
                self.emit(ROp::Super, &[dst]);
            }
            Expression::NewTarget => {
                self.emit(ROp::NewTarget, &[dst]);
            }
            Expression::ImportMeta => {
                self.emit(ROp::ImportMeta, &[dst]);
            }
            Expression::Array(items) => {
                self.compile_array_into(items, dst)?;
            }
            Expression::Hash(pairs) => {
                self.compile_hash_into(pairs, dst)?;
            }
            Expression::Prefix { operator, right } => {
                let saved = self.save_temps();
                let src = self.compile_expression(right)?;
                match operator.as_str() {
                    "!" => self.emit(ROp::Not, &[dst, src]),
                    "-" => self.emit(ROp::Neg, &[dst, src]),
                    "+" => self.emit(ROp::UnaryPlus, &[dst, src]),
                    "~" => {
                        // ~x = x ^ -1
                        let neg1_idx = self.add_constant_int(-1);
                        let neg1 = self.alloc_temp();
                        self.emit(ROp::LoadConst, &[neg1, neg1_idx]);
                        self.emit(ROp::BitwiseXor, &[dst, src, neg1]);
                        0 // dummy
                    }
                    _ => return Err(format!("unsupported prefix operator {}", operator)),
                };
                self.restore_temps(saved);
            }
            Expression::Typeof { value } => {
                let saved = self.save_temps();
                if let Expression::Identifier(name) = &**value {
                    // typeof undeclaredVar must return "undefined" without throwing.
                    // Only compile the identifier if it's known to exist.
                    if self.locals.contains_key(name)
                        || self.globals.contains_key(name)
                        || Self::builtin_global_object(name).is_some()
                    {
                        let src = self.compile_expression(value)?;
                        self.emit(ROp::Typeof, &[dst, src]);
                    } else {
                        // Unknown identifier: typeof returns "undefined"
                        let undef = self.alloc_temp();
                        self.emit(ROp::LoadUndef, &[undef]);
                        self.emit(ROp::Typeof, &[dst, undef]);
                    }
                } else {
                    let src = self.compile_expression(value)?;
                    self.emit(ROp::Typeof, &[dst, src]);
                }
                self.restore_temps(saved);
            }
            Expression::Void { value } => {
                // Evaluate for side effects, then load undefined
                let saved = self.save_temps();
                let _ = self.compile_expression(value)?;
                self.restore_temps(saved);
                self.emit(ROp::LoadUndef, &[dst]);
            }
            Expression::Delete { value } => {
                self.compile_delete_into(value, dst)?;
            }
            Expression::Infix {
                left,
                operator,
                right,
            } => {
                if operator == "," {
                    // Evaluate left for side effects, result is right
                    let saved = self.save_temps();
                    let _ = self.compile_expression(left)?;
                    self.restore_temps(saved);
                    self.compile_expression_into(right, dst)?;
                    return Ok(());
                }
                if operator == "&&" || operator == "||" || operator == "??" {
                    self.compile_logical_into(left, operator, right, dst)?;
                    return Ok(());
                }
                // Fused: reg OP constant → single instruction (saves LoadConst + temp register)
                let fused_op = match operator.as_str() {
                    "+" => Some(ROp::AddRegConst),
                    "-" => Some(ROp::SubRegConst),
                    "*" => Some(ROp::MulRegConst),
                    _ => None,
                };
                if let Some(fused) = fused_op {
                    if let Some(const_idx) = self.try_numeric_const(right) {
                        let saved = self.save_temps();
                        let l = self.compile_expression(left)?;
                        self.emit(fused, &[dst, l, const_idx]);
                        self.restore_temps(saved);
                        return Ok(());
                    }
                    // Also check left-constant for truly commutative ops (Mul only).
                    // Note: Add/+ is NOT commutative in JS due to string concatenation
                    // (e.g., 5 + "3" = "53" but "3" + 5 = "35").
                    if fused == ROp::MulRegConst {
                        if let Some(const_idx) = self.try_numeric_const(left) {
                            let saved = self.save_temps();
                            let r = self.compile_expression(right)?;
                            self.emit(fused, &[dst, r, const_idx]);
                            self.restore_temps(saved);
                            return Ok(());
                        }
                    }
                }
                // ── Constant folding: evaluate compile-time constant expressions ──
                if let (Expression::Integer(a), Expression::Integer(b)) = (left.as_ref(), right.as_ref()) {
                    let folded: Option<i64> = match operator.as_str() {
                        "+" => a.checked_add(*b),
                        "-" => a.checked_sub(*b),
                        "*" => a.checked_mul(*b),
                        "/" if *b != 0 && *a % *b == 0 => Some(*a / *b),
                        "%" if *b != 0 => Some(*a % *b),
                        "&" => Some(*a & *b),
                        "|" => Some(*a | *b),
                        "^" => Some(*a ^ *b),
                        "<<" => Some(*a << (*b & 31)),
                        ">>" => Some(*a >> (*b & 31)),
                        _ => None,
                    };
                    if let Some(val) = folded {
                        let idx = self.add_constant_int(val);
                        self.emit(ROp::LoadConst, &[dst, idx]);
                        return Ok(());
                    }
                    // Boolean folds
                    let bool_folded: Option<bool> = match operator.as_str() {
                        "==" | "===" => Some(*a == *b),
                        "!=" | "!==" => Some(*a != *b),
                        ">" => Some(*a > *b),
                        "<" => Some(*a < *b),
                        ">=" => Some(*a >= *b),
                        "<=" => Some(*a <= *b),
                        _ => None,
                    };
                    if let Some(val) = bool_folded {
                        self.emit(if val { ROp::LoadTrue } else { ROp::LoadFalse }, &[dst]);
                        return Ok(());
                    }
                }
                let saved = self.save_temps();
                let l = self.compile_expression(left)?;
                let r = self.compile_expression(right)?;
                let op = match operator.as_str() {
                    "+" => ROp::Add,
                    "-" => ROp::Sub,
                    "*" => ROp::Mul,
                    "/" => ROp::Div,
                    "%" => ROp::Mod,
                    "**" => ROp::Pow,
                    "==" => ROp::Equal,
                    "!=" => ROp::NotEqual,
                    "===" => ROp::StrictEqual,
                    "!==" => ROp::StrictNotEqual,
                    ">" => ROp::GreaterThan,
                    "<" => ROp::LessThan,
                    ">=" => ROp::GreaterOrEqual,
                    "<=" => ROp::LessOrEqual,
                    "&" => ROp::BitwiseAnd,
                    "|" => ROp::BitwiseOr,
                    "^" => ROp::BitwiseXor,
                    "<<" => ROp::LeftShift,
                    ">>" => ROp::RightShift,
                    ">>>" => ROp::UnsignedRightShift,
                    "in" => ROp::In,
                    "instanceof" => ROp::Instanceof,
                    _ => return Err(format!("unsupported infix operator {}", operator)),
                };
                self.emit(op, &[dst, l, r]);
                self.restore_temps(saved);
            }
            Expression::If {
                condition,
                consequence,
                alternative,
            } => {
                self.compile_if_expr_into(condition, consequence, alternative.as_deref(), dst)?;
            }
            Expression::Function {
                parameters,
                body,
                is_async,
                is_generator,
                is_arrow,
            } => {
                let takes_this = !is_arrow;
                let func_obj = self.compile_function_literal(
                    parameters,
                    body,
                    takes_this,
                    *is_async,
                    *is_generator,
                )?;
                let captures = func_obj.closure_captures.clone();
                let idx = self.add_constant(Object::CompiledFunction(Box::new(func_obj)));
                if captures.is_empty() {
                    self.emit(ROp::LoadConst, &[dst, idx]);
                } else {
                    // Emit MakeClosure: [dst, const_idx, count, slot0, slot1, ...]
                    let mut operands = vec![dst, idx, captures.len() as u16];
                    operands.extend_from_slice(&captures);
                    self.emit(ROp::MakeClosure, &operands);
                }
            }
            Expression::Await { value } => {
                let src = self.compile_expression(value)?;
                self.emit(ROp::Await, &[dst, src]);
            }
            Expression::Yield { value, delegate: _ } => {
                let src = self.compile_expression(value)?;
                self.emit(ROp::Yield, &[dst, src]);
            }
            Expression::Sequence(exprs) => {
                // Evaluate all expressions, result of last goes into dst
                for (i, expr) in exprs.iter().enumerate() {
                    if i == exprs.len() - 1 {
                        self.compile_expression_into(expr, dst)?;
                    } else {
                        let tmp = self.compile_expression(expr)?;
                        let _ = tmp; // discard
                    }
                }
            }
            Expression::New { callee, arguments } => {
                self.compile_new_into(callee, arguments, dst)?;
            }
            Expression::Call {
                function,
                arguments,
            } => {
                self.compile_call_into(function, arguments, dst)?;
            }
            Expression::OptionalIndex { left, index } => {
                self.compile_optional_index_into(left, index, dst)?;
            }
            Expression::OptionalCall {
                function,
                arguments,
            } => {
                self.compile_optional_call_into(function, arguments, dst)?;
            }
            Expression::Assign {
                left,
                operator,
                right,
            } => {
                self.compile_assignment_into(left, operator, right, dst)?;
            }
            Expression::Update {
                target,
                operator,
                prefix,
            } => {
                let assign_op = match operator.as_str() {
                    "++" => "+=",
                    "--" => "-=",
                    _ => return Err(format!("unsupported update operator {}", operator)),
                };
                if *prefix {
                    self.compile_assignment_into(target, assign_op, &Expression::Integer(1), dst)?;
                } else {
                    // Post-fix: save old value, do assignment, return old value
                    let old = self.compile_expression(target)?;
                    if dst != old {
                        self.emit(ROp::Move, &[dst, old]);
                    }
                    let tmp = self.alloc_temp();
                    self.compile_assignment_into(target, assign_op, &Expression::Integer(1), tmp)?;
                }
            }
            Expression::Spread { .. } => {
                return Err("spread expression is only valid in array literals".to_string());
            }
            Expression::Class {
                name,
                extends,
                members,
            } => {
                let class_name = name.as_deref().unwrap_or("");
                let extends_name = match extends.as_deref() {
                    Some(Expression::Identifier(s)) => Some(s.as_str()),
                    _ => None,
                };
                let class_obj = self.compile_class_literal(class_name, extends_name, members)?;
                if let Some(n) = name {
                    self.class_defs.insert(n.clone(), class_obj.clone());
                }
                let has_static_init = !class_obj.static_initializers.is_empty();
                let idx = self.add_constant(Object::Class(Box::new(class_obj)));
                self.emit(ROp::LoadConst, &[dst, idx]);
                if has_static_init {
                    self.emit(ROp::InitClass, &[dst]);
                }
            }
            Expression::Index { left, index } => {
                self.compile_index_into(left, index, dst)?;
            }
        }
        Ok(())
    }

    // ── Helper: load identifier into register ────────────────────────────

    fn load_identifier_into(&mut self, name: &str, dst: u16) -> Result<(), String> {
        if let Some(&r) = self.locals.get(name) {
            // For captured variables (both local AND global), always read from
            // the global slot. This ensures cross-scope mutations are visible
            // without needing reload_locals_from_globals after every call.
            if let Some(&g) = self.globals.get(name) {
                self.emit(ROp::GetGlobal, &[dst, g]);
                return Ok(());
            }
            if r != dst {
                self.emit(ROp::Move, &[dst, r]);
            }
            return Ok(());
        }
        if let Some(&g) = self.globals.get(name) {
            self.emit(ROp::GetGlobal, &[dst, g]);
            return Ok(());
        }
        if let Some(builtin_obj) = Self::builtin_global_object(name) {
            let idx = self.add_constant(builtin_obj);
            self.emit(ROp::LoadConst, &[dst, idx]);
            return Ok(());
        }
        if self.is_function_scope {
            let g = self.ensure_global_slot(name)?;
            self.emit(ROp::GetGlobal, &[dst, g]);
            return Ok(());
        }
        Err(format!("undefined identifier {}", name))
    }

    // ── Array / Hash ─────────────────────────────────────────────────────

    fn compile_array_into(&mut self, items: &[Expression], dst: u16) -> Result<(), String> {
        let has_spread = items
            .iter()
            .any(|item| matches!(item, Expression::Spread { .. }));
        if has_spread {
            // Start with empty array, then append elements
            self.emit(ROp::Array, &[dst, 0, 0]);
            for item in items {
                match item {
                    Expression::Spread { value } => {
                        let v = self.compile_expression(value)?;
                        self.emit(ROp::AppendSpread, &[dst, v]);
                    }
                    _ => {
                        let v = self.compile_expression(item)?;
                        self.emit(ROp::AppendElement, &[dst, v]);
                    }
                }
            }
        } else {
            // Pack elements into contiguous registers
            let base = self.next_temp;
            for (i, item) in items.iter().enumerate() {
                self.set_next_temp_to(base, i);
                let r = self.alloc_temp();
                self.compile_expression_into(item, r)?;
            }
            self.set_next_temp_to(base, items.len());
            let count = match u16::try_from(items.len()) {
                Ok(c) => c,
                Err(_) => {
                    self.record_overflow("array literal element count exceeded u16::MAX");
                    return Ok(());
                }
            };
            self.emit(ROp::Array, &[dst, base, count]);
        }
        Ok(())
    }

    fn compile_hash_into(&mut self, pairs: &[HashEntry], dst: u16) -> Result<(), String> {
        // We need to compile method bodies first (before emitting register code)
        // because compile_function_literal modifies compiler state.

        // Collect info about what to emit for the Hash opcode
        enum KvSource<'a> {
            /// Regular key-value pair
            KeyValue {
                key: &'a Expression,
                value: &'a Expression,
            },
            /// Method: key + pre-compiled function constant index
            Method { key: &'a Expression, const_idx: u16 },
            /// Spread: sentinel key + spread expr
            Spread { expr: &'a Expression },
        }

        let mut kv_sources: Vec<KvSource> = Vec::new();
        // We also need to track getter/setter entries for the second pass
        let mut accessor_const_indices: Vec<(u16, u16, bool)> = Vec::new(); // (prop_const_idx, func_const_idx, is_setter)

        // First: pre-compile all methods, getters, setters (modifies self)
        for entry in pairs {
            match entry {
                HashEntry::KeyValue { key, value } => {
                    kv_sources.push(KvSource::KeyValue { key, value });
                }
                HashEntry::Method {
                    key,
                    parameters,
                    body,
                    is_async,
                    is_generator,
                } => {
                    let func_obj = self.compile_function_literal(
                        parameters,
                        body,
                        true,
                        *is_async,
                        *is_generator,
                    )?;
                    let func_idx = self.add_constant(Object::CompiledFunction(Box::new(func_obj)));
                    kv_sources.push(KvSource::Method {
                        key,
                        const_idx: func_idx,
                    });
                }
                HashEntry::Spread(expr) => {
                    kv_sources.push(KvSource::Spread { expr });
                }
                HashEntry::Getter { key, body } => {
                    let func_obj = self.compile_function_literal(&[], body, true, false, false)?;
                    let func_idx = self.add_constant(Object::CompiledFunction(Box::new(func_obj)));
                    let prop_name = match key {
                        Expression::String(s) => s.as_str(),
                        _ => return Err("computed getter keys not yet supported".to_string()),
                    };
                    let prop_idx = self.add_constant_string(Rc::from(prop_name));
                    accessor_const_indices.push((prop_idx, func_idx, false));
                }
                HashEntry::Setter {
                    key,
                    parameter,
                    body,
                } => {
                    let params = vec![parameter.clone()];
                    let func_obj =
                        self.compile_function_literal(&params, body, true, false, false)?;
                    let func_idx = self.add_constant(Object::CompiledFunction(Box::new(func_obj)));
                    let prop_name = match key {
                        Expression::String(s) => s.as_str(),
                        _ => return Err("computed setter keys not yet supported".to_string()),
                    };
                    let prop_idx = self.add_constant_string(Rc::from(prop_name));
                    accessor_const_indices.push((prop_idx, func_idx, true));
                }
            }
        }

        // Now emit register code for key-value pairs
        let rest_key = Expression::String("__fl_rest__".to_string());
        let base = self.next_temp;
        let num_kv = kv_sources.len();
        for (i, src) in kv_sources.iter().enumerate() {
            self.set_next_temp_to(base, i * 2);
            let kr = self.alloc_temp();
            self.set_next_temp_to(base, i * 2 + 1);
            let vr = self.alloc_temp();
            match src {
                KvSource::KeyValue { key, value } => {
                    self.compile_expression_into(key, kr)?;
                    self.compile_expression_into(value, vr)?;
                }
                KvSource::Method { key, const_idx } => {
                    self.compile_expression_into(key, kr)?;
                    self.emit(ROp::LoadConst, &[vr, *const_idx]);
                }
                KvSource::Spread { expr } => {
                    self.compile_expression_into(&rest_key, kr)?;
                    self.compile_expression_into(expr, vr)?;
                }
            }
        }
        self.set_next_temp_to(base, num_kv * 2);
        let count = match u16::try_from(num_kv * 2) {
            Ok(c) => c,
            Err(_) => {
                self.record_overflow("hash literal entry count exceeded u16::MAX");
                return Ok(());
            }
        };
        self.emit(ROp::Hash, &[dst, base, count]);

        // Second pass: emit DefineAccessor for getter/setter entries
        for (prop_idx, func_idx, is_setter) in &accessor_const_indices {
            let func_r = self.alloc_temp();
            self.emit(ROp::LoadConst, &[func_r, *func_idx]);
            let kind = if *is_setter { 1u16 } else { 0u16 };
            self.emit(
                ROp::DefineAccessor,
                &[dst, func_r, *prop_idx, kind],
            );
        }

        Ok(())
    }

    // ── Index access ─────────────────────────────────────────────────────

    fn compile_index_into(
        &mut self,
        left: &Expression,
        index: &Expression,
        dst: u16,
    ) -> Result<(), String> {
        // Named property with inline cache
        if let Expression::String(prop) = index {
            let obj_name: Option<&str> = match left {
                Expression::Identifier(name) => Some(name.as_str()),
                Expression::This => Some("this"),
                _ => None,
            };
            if let Some(name) = obj_name {
                // Local property access
                if let Some(&local_r) = self.locals.get(name) {
                    let const_idx = self.add_constant_string(Rc::from(prop.as_str()));
                    let cache_slot = self.next_cache_slot;
                    self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                    self.emit(
                        ROp::GetProp,
                        &[dst, local_r, const_idx, cache_slot],
                    );
                    return Ok(());
                }
                // Global property access
                if let Some(&global_idx) = self.globals.get(name) {
                    let const_idx = self.add_constant_string(Rc::from(prop.as_str()));
                    let cache_slot = self.next_cache_slot;
                    self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                    self.emit(
                        ROp::GetGlobalProp,
                        &[dst, global_idx, const_idx, cache_slot],
                    );
                    return Ok(());
                }
            }
            // General object property access
            let obj = self.compile_expression(left)?;
            let const_idx = self.add_constant_string(Rc::from(prop.as_str()));
            let cache_slot = self.next_cache_slot;
            self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
            self.emit(
                ROp::GetProp,
                &[dst, obj, const_idx, cache_slot],
            );
        } else {
            // Dynamic index
            let obj = self.compile_expression(left)?;
            let key = self.compile_expression(index)?;
            self.emit(ROp::Index, &[dst, obj, key]);
        }
        Ok(())
    }

    // ── Calls ────────────────────────────────────────────────────────────

    /// Check if a register is a named local (would be reloaded by reload_locals_from_globals).
    fn is_local_register(&self, reg: u16) -> bool {
        self.locals.values().any(|&r| r == reg)
    }

    /// Emit reload_locals_from_globals, then move `temp` → `dst` if they differ.
    /// Used after Call/New to protect the call result from being overwritten by reload.
    fn reload_and_move_result(&mut self, call_dst: u16, final_dst: u16) {
        // No reload needed: captured variables are always read from global slots
        // directly via GetGlobal in load_identifier_into.
        if call_dst != final_dst {
            self.emit(ROp::Move, &[final_dst, call_dst]);
        }
    }

    fn compile_call_into(
        &mut self,
        function: &Expression,
        arguments: &[Expression],
        dst: u16,
    ) -> Result<(), String> {
        // If dst is a local register, the reload after the call would overwrite it.
        // Use a temp register for the call result, reload, then move to dst.
        let call_dst = if self.is_local_register(dst) {
            self.alloc_temp()
        } else {
            dst
        };

        if self.arguments_have_spread(arguments) {
            let func = self.compile_expression(function)?;
            let args_arr = self.compile_spread_args_array(arguments)?;
            self.emit(
                ROp::CallSpread,
                &[call_dst, func, args_arr],
            );
            self.reload_and_move_result(call_dst, dst);
            return Ok(());
        }

        // Try fused OpCallGlobal
        if let Some(global_idx) = self.try_resolve_global_function(function) {
            let base = self.next_temp;
            // Reserve slot for callee (not actually used, but keeps base consistent)
            let _ = self.alloc_temp();
            for (i, arg) in arguments.iter().enumerate() {
                self.set_next_temp_to(base, 1 + i);
                let r = self.alloc_temp();
                self.compile_expression_into(arg, r)?;
            }
            self.set_next_temp_to(base, 1 + arguments.len());
            self.emit(
                ROp::CallGlobal,
                &[
                    call_dst,
                    global_idx,
                    base,
                    arguments.len() as u16,
                ],
            );
            self.reload_and_move_result(call_dst, dst);
            return Ok(());
        }

        // Try fused CallMethod for obj.method(args) pattern
        if let Expression::Index { left, index } = function {
            if let Expression::String(prop_name) = index.as_ref() {
                let base = self.next_temp;
                let obj_r = self.alloc_temp();
                self.compile_expression_into(left, obj_r)?;
                for (i, arg) in arguments.iter().enumerate() {
                    self.set_next_temp_to(base, 1 + i);
                    let r = self.alloc_temp();
                    self.compile_expression_into(arg, r)?;
                }
                self.set_next_temp_to(base, 1 + arguments.len());
                let const_idx = self.add_constant_string(Rc::from(prop_name.as_str()));
                let cache_slot = self.next_cache_slot;
                self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                self.emit(
                    ROp::CallMethod,
                    &[
                        call_dst,
                        base,
                        arguments.len() as u16,
                        const_idx,
                        cache_slot,
                    ],
                );
                self.reload_and_move_result(call_dst, dst);
                return Ok(());
            }
        }

        // Optional method call: obj?.method(args) — short-circuit to undefined when nullish
        if let Expression::OptionalIndex { left, index } = function {
            if let Expression::String(prop_name) = index.as_ref() {
                let base = self.next_temp;
                let obj_r = self.alloc_temp();
                self.compile_expression_into(left, obj_r)?;

                // Check nullish
                let nullish = self.alloc_temp();
                self.emit(ROp::IsNullish, &[nullish, obj_r]);
                let not_null_pos = self.emit(ROp::JumpIfNot, &[nullish, 9999]);
                self.emit(ROp::LoadUndef, &[dst]);
                let end_pos = self.emit(ROp::Jump, &[9999]);

                let call_pos = self.instructions.len();
                self.patch_jump(not_null_pos, call_pos);

                // Not nullish — do the method call
                for (i, arg) in arguments.iter().enumerate() {
                    self.set_next_temp_to(base, 1 + i);
                    let r = self.alloc_temp();
                    self.compile_expression_into(arg, r)?;
                }
                self.set_next_temp_to(base, 1 + arguments.len());
                let const_idx = self.add_constant_string(Rc::from(prop_name.as_str()));
                let cache_slot = self.next_cache_slot;
                self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
                self.emit(
                    ROp::CallMethod,
                    &[
                        call_dst,
                        base,
                        arguments.len() as u16,
                        const_idx,
                        cache_slot,
                    ],
                );
                self.reload_and_move_result(call_dst, dst);

                let end = self.instructions.len();
                self.patch_jump(end_pos, end);
                return Ok(());
            }
        }

        // General call: pack func + args in contiguous registers
        let base = self.next_temp;
        let func_r = self.alloc_temp();
        self.compile_expression_into(function, func_r)?;
        // Reset next_temp: func result is in base, internal temps are dead
        for (i, arg) in arguments.iter().enumerate() {
            self.set_next_temp_to(base, 1 + i);
            let r = self.alloc_temp();
            self.compile_expression_into(arg, r)?;
        }
        self.set_next_temp_to(base, 1 + arguments.len());
        self.emit(
            ROp::Call,
            &[call_dst, base, arguments.len() as u16],
        );
        self.reload_and_move_result(call_dst, dst);
        Ok(())
    }

    fn compile_new_into(
        &mut self,
        callee: &Expression,
        arguments: &[Expression],
        dst: u16,
    ) -> Result<(), String> {
        let call_dst = if self.is_local_register(dst) {
            self.alloc_temp()
        } else {
            dst
        };

        if self.arguments_have_spread(arguments) {
            let cls = self.compile_expression(callee)?;
            let args_arr = self.compile_spread_args_array(arguments)?;
            self.emit(
                ROp::NewSpread,
                &[call_dst, cls, args_arr],
            );
            self.reload_and_move_result(call_dst, dst);
            return Ok(());
        }
        let base = self.next_temp;
        let cls_r = self.alloc_temp();
        self.compile_expression_into(callee, cls_r)?;
        // Reset next_temp: callee result is in base, internal temps are dead
        for (i, arg) in arguments.iter().enumerate() {
            self.set_next_temp_to(base, 1 + i);
            let r = self.alloc_temp();
            self.compile_expression_into(arg, r)?;
        }
        self.set_next_temp_to(base, 1 + arguments.len());
        self.emit(
            ROp::New,
            &[call_dst, base, arguments.len() as u16],
        );
        self.reload_and_move_result(call_dst, dst);
        Ok(())
    }

    fn arguments_have_spread(&self, arguments: &[Expression]) -> bool {
        arguments
            .iter()
            .any(|arg| matches!(arg, Expression::Spread { .. }))
    }

    fn compile_spread_args_array(&mut self, arguments: &[Expression]) -> Result<u16, String> {
        let arr = self.alloc_temp();
        self.emit(ROp::Array, &[arr, 0, 0]);
        for arg in arguments {
            match arg {
                Expression::Spread { value } => {
                    let v = self.compile_expression(value)?;
                    self.emit(ROp::AppendSpread, &[arr, v]);
                }
                _ => {
                    let v = self.compile_expression(arg)?;
                    self.emit(ROp::AppendElement, &[arr, v]);
                }
            }
        }
        Ok(arr)
    }

    // ── Control flow ─────────────────────────────────────────────────────

    fn compile_if_expr_into(
        &mut self,
        condition: &Expression,
        consequence: &[Statement],
        alternative: Option<&[Statement]>,
        dst: u16,
    ) -> Result<(), String> {
        // Try fused condition opcodes before falling back to generic path.
        // These combine condition evaluation + conditional jump in one opcode,
        // eliminating 2 extra dispatches per branch check.
        let jump_pos = if let Some((reg, const_idx, is_le)) = self.try_fused_cmp_const(condition) {
            let op = if is_le {
                ROp::TestLeConstJump
            } else {
                ROp::TestLtConstJump
            };
            self.emit(op, &[reg, const_idx, 9999])
        } else if let Some((lr, rr, is_le)) = self.try_fused_cmp_reg(condition) {
            let op = if is_le {
                ROp::TestLeRegJump
            } else {
                ROp::TestLtRegJump
            };
            self.emit(op, &[lr, rr, 9999])
        } else if let Some((reg, mod_const, cmp_const)) = self.try_fused_mod_strict_eq(condition) {
            self.emit(
                ROp::ModRegConstStrictEqConstJump,
                &[reg, mod_const, cmp_const, 9999],
            )
        } else {
            let cond = self.compile_expression(condition)?;
            self.emit(ROp::JumpIfNot, &[cond, 9999])
        };

        // Consequence: compile statements with block scoping
        let shadowed = self.enter_block_scope(consequence);
        let mut last = None;
        for stmt in consequence {
            last = self.compile_statement(stmt)?;
        }
        self.exit_block_scope(shadowed);
        if let Some(r) = last {
            if r != dst {
                self.emit(ROp::Move, &[dst, r]);
            }
        }

        let jump_over = self.emit(ROp::Jump, &[9999]);
        let after_cons = self.instructions.len();
        self.patch_jump(jump_pos, after_cons);

        if let Some(alt_block) = alternative {
            let shadowed = self.enter_block_scope(alt_block);
            let mut last = None;
            for stmt in alt_block {
                last = self.compile_statement(stmt)?;
            }
            self.exit_block_scope(shadowed);
            if let Some(r) = last {
                if r != dst {
                    self.emit(ROp::Move, &[dst, r]);
                }
            }
        } else {
            self.emit(ROp::LoadNull, &[dst]);
        }

        let after_alt = self.instructions.len();
        self.patch_jump(jump_over, after_alt);
        Ok(())
    }


    // ── Logical operators ────────────────────────────────────────────────

    fn compile_logical_into(
        &mut self,
        left: &Expression,
        operator: &str,
        right: &Expression,
        dst: u16,
    ) -> Result<(), String> {
        let l = self.compile_expression(left)?;

        match operator {
            "||" => {
                // If left is truthy, result is left; otherwise evaluate right
                if l != dst {
                    self.emit(ROp::Move, &[dst, l]);
                }
                let end_pos = self.emit(ROp::JumpIfTruthy, &[dst, 9999]);
                self.compile_expression_into(right, dst)?;
                let end = self.instructions.len();
                self.patch_jump(end_pos, end);
            }
            "&&" => {
                // If left is falsy, result is left; otherwise evaluate right
                if l != dst {
                    self.emit(ROp::Move, &[dst, l]);
                }
                let end_pos = self.emit(ROp::JumpIfNot, &[dst, 9999]);
                self.compile_expression_into(right, dst)?;
                let end = self.instructions.len();
                self.patch_jump(end_pos, end);
            }
            "??" => {
                // If left is nullish, evaluate right; otherwise result is left
                if l != dst {
                    self.emit(ROp::Move, &[dst, l]);
                }
                let nullish = self.alloc_temp();
                self.emit(ROp::IsNullish, &[nullish, dst]);
                let use_left_pos = self.emit(ROp::JumpIfNot, &[nullish, 9999]);
                self.compile_expression_into(right, dst)?;
                let end = self.instructions.len();
                self.patch_jump(use_left_pos, end);
            }
            _ => return Err(format!("unsupported logical operator {}", operator)),
        }
        Ok(())
    }

    // ── Optional chaining ────────────────────────────────────────────────

    fn compile_optional_index_into(
        &mut self,
        left: &Expression,
        index: &Expression,
        dst: u16,
    ) -> Result<(), String> {
        let obj = self.compile_expression(left)?;
        let nullish = self.alloc_temp();
        self.emit(ROp::IsNullish, &[nullish, obj]);
        let not_null_pos = self.emit(ROp::JumpIfNot, &[nullish, 9999]);
        self.emit(ROp::LoadUndef, &[dst]);
        let end_pos = self.emit(ROp::Jump, &[9999]);

        let access_pos = self.instructions.len();
        self.patch_jump(not_null_pos, access_pos);

        // Do the index access from obj
        if let Expression::String(prop) = index {
            let const_idx = self.add_constant_string(Rc::from(prop.as_str()));
            let cache_slot = self.next_cache_slot;
            self.next_cache_slot = self.checked_inc_u16(self.next_cache_slot, "inline cache slot count exceeded u16::MAX");
            self.emit(
                ROp::GetProp,
                &[dst, obj, const_idx, cache_slot],
            );
        } else {
            let key = self.compile_expression(index)?;
            self.emit(ROp::Index, &[dst, obj, key]);
        }

        let end = self.instructions.len();
        self.patch_jump(end_pos, end);
        Ok(())
    }

    fn compile_optional_call_into(
        &mut self,
        function: &Expression,
        arguments: &[Expression],
        dst: u16,
    ) -> Result<(), String> {
        let call_dst = if self.is_local_register(dst) {
            self.alloc_temp()
        } else {
            dst
        };

        let func = self.compile_expression(function)?;
        let nullish = self.alloc_temp();
        self.emit(ROp::IsNullish, &[nullish, func]);
        let not_null_pos = self.emit(ROp::JumpIfNot, &[nullish, 9999]);
        // Null case: write undefined directly to final dst (not call_dst)
        // so the result is correct when we jump past the call+move path.
        self.emit(ROp::LoadUndef, &[dst]);
        let end_pos = self.emit(ROp::Jump, &[9999]);

        let call_pos = self.instructions.len();
        self.patch_jump(not_null_pos, call_pos);

        if self.arguments_have_spread(arguments) {
            let args_arr = self.compile_spread_args_array(arguments)?;
            self.emit(
                ROp::CallSpread,
                &[call_dst, func, args_arr],
            );
        } else {
            // We already have func in a register. Put it at base, args after.
            let base = func;
            let saved = self.save_temps();
            for (i, arg) in arguments.iter().enumerate() {
                self.set_next_temp_to(base, 1 + i);
                let r = self.alloc_temp();
                self.compile_expression_into(arg, r)?;
            }
            self.set_next_temp_to(base, 1 + arguments.len());
            self.emit(
                ROp::Call,
                &[call_dst, base, arguments.len() as u16],
            );
            self.restore_temps(saved);
        }
        self.reload_and_move_result(call_dst, dst);

        let end = self.instructions.len();
        self.patch_jump(end_pos, end);
        Ok(())
    }

    // ── Assignment ───────────────────────────────────────────────────────


    // ── Destructuring ────────────────────────────────────────────────────

    fn assign_pattern(&mut self, pattern: &BindingPattern, src: u16) -> Result<(), String> {
        match pattern {
            BindingPattern::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    match item {
                        ArrayBindingItem::Hole => continue,
                        ArrayBindingItem::Binding {
                            target,
                            default_value,
                        } => {
                            let key = self.alloc_temp();
                            let key_idx = self.add_constant_int(i as i64);
                            self.emit(ROp::LoadConst, &[key, key_idx]);
                            let val = self.alloc_temp();
                            self.emit(ROp::Index, &[val, src, key]);
                            let val = self.apply_default(val, default_value.as_ref())?;
                            self.assign_binding_target(target, val)?;
                        }
                        ArrayBindingItem::Rest { name } => {
                            let rest = self.alloc_temp();
                            self.emit(ROp::IteratorRest, &[rest, src, i as u16]);
                            self.store_identifier(name, rest)?;
                        }
                    }
                }
            }
            BindingPattern::Object(pairs) => {
                let excluded_keys: Vec<Expression> = pairs
                    .iter()
                    .filter(|p| !p.is_rest)
                    .map(|p| p.key.clone())
                    .collect();

                for ObjectBindingItem {
                    key,
                    target,
                    default_value,
                    is_rest,
                } in pairs
                {
                    if *is_rest {
                        let rest = self.compile_object_rest(src, &excluded_keys)?;
                        let BindingTarget::Identifier(name) = target else {
                            return Err("object rest target must be identifier".to_string());
                        };
                        self.store_identifier(name, rest)?;
                        continue;
                    }
                    let key_r = self.compile_expression(key)?;
                    let val = self.alloc_temp();
                    self.emit(ROp::Index, &[val, src, key_r]);
                    let val = self.apply_default(val, default_value.as_ref())?;
                    self.assign_binding_target(target, val)?;
                }
            }
        }
        Ok(())
    }

    fn assign_binding_target(&mut self, target: &BindingTarget, src: u16) -> Result<(), String> {
        match target {
            BindingTarget::Identifier(name) => self.store_identifier(name, src),
            BindingTarget::Pattern(pattern) => self.assign_pattern(pattern, src),
        }
    }

    /// Pre-declare all identifiers in a destructuring pattern as locals.
    /// Collects all binding names from a pattern into the given set.
    fn collect_pattern_names(pattern: &BindingPattern, out: &mut FxHashSet<String>) {
        match pattern {
            BindingPattern::Array(items) => {
                for item in items {
                    match item {
                        ArrayBindingItem::Binding { target, .. } => match target {
                            BindingTarget::Identifier(name) => {
                                out.insert(name.clone());
                            }
                            BindingTarget::Pattern(inner) => {
                                Self::collect_pattern_names(inner, out);
                            }
                        },
                        ArrayBindingItem::Rest { name } => {
                            out.insert(name.clone());
                        }
                        ArrayBindingItem::Hole => {}
                    }
                }
            }
            BindingPattern::Object(items) => {
                for item in items {
                    match &item.target {
                        BindingTarget::Identifier(name) => {
                            out.insert(name.clone());
                        }
                        BindingTarget::Pattern(inner) => {
                            Self::collect_pattern_names(inner, out);
                        }
                    }
                }
            }
        }
    }

    /// Scans statement list for `var` declarations and pre-allocates their
    /// binding slots with `undefined`. This implements JS var hoisting.
    fn hoist_var_declarations(&mut self, stmts: &[Statement]) -> Result<(), String> {
        let mut var_names = Vec::new();
        Self::collect_var_names(stmts, &mut var_names);
        for name in var_names {
            self.ensure_binding_slot(&name)?;
        }
        Ok(())
    }

    /// Recursively collects all `var`-declared names from a statement list.
    /// Does NOT descend into function bodies (var is function-scoped, not
    /// hoisted across function boundaries).
    /// Collect ALL local declaration names (let, const, var, function) recursively.
    /// Used to prevent child function globals from leaking to parent scope.
    #[allow(dead_code)]
    fn collect_all_local_names(stmts: &[Statement], out: &mut FxHashSet<String>) {
        for stmt in stmts {
            match stmt {
                Statement::Let { name, .. } => { out.insert(name.clone()); }
                Statement::LetPattern { pattern, .. } => {
                    Self::collect_pattern_names(pattern, out);
                }
                Statement::FunctionDecl { name, .. } => { out.insert(name.clone()); }
                Statement::MultiLet(stmts) => {
                    Self::collect_all_local_names(stmts, out);
                }
                Statement::Block(body) => {
                    Self::collect_all_local_names(body, out);
                }
                Statement::Expression(Expression::If { consequence, alternative, .. }) => {
                    Self::collect_all_local_names(consequence, out);
                    if let Some(alt) = alternative {
                        Self::collect_all_local_names(alt, out);
                    }
                }
                Statement::While { body, .. } | Statement::DoWhile { body, .. } => {
                    Self::collect_all_local_names(body, out);
                }
                Statement::For { init, body, .. } => {
                    if let Some(init_stmt) = init {
                        Self::collect_all_local_names(
                            std::slice::from_ref(init_stmt.as_ref()), out);
                    }
                    Self::collect_all_local_names(body, out);
                }
                Statement::ForIn { var_name, body, .. } => {
                    out.insert(var_name.clone());
                    Self::collect_all_local_names(body, out);
                }
                Statement::ForOf { binding, body, .. } => {
                    if let crate::ast::ForBinding::Identifier(name) = binding {
                        out.insert(name.clone());
                    }
                    Self::collect_all_local_names(body, out);
                }
                Statement::Try { try_block, catch_param, catch_block, finally_block, .. } => {
                    Self::collect_all_local_names(try_block, out);
                    if let Some(param) = catch_param { out.insert(param.clone()); }
                    if let Some(cb) = catch_block { Self::collect_all_local_names(cb, out); }
                    if let Some(fb) = finally_block { Self::collect_all_local_names(fb, out); }
                }
                Statement::Switch { cases, .. } => {
                    for case in cases {
                        Self::collect_all_local_names(&case.consequent, out);
                    }
                }
                Statement::Labeled { statement, .. } => {
                    Self::collect_all_local_names(
                        std::slice::from_ref(statement.as_ref()), out);
                }
                _ => {}
            }
        }
    }

    fn collect_var_names(stmts: &[Statement], out: &mut Vec<String>) {
        for stmt in stmts {
            match stmt {
                Statement::Let {
                    name,
                    kind: VariableKind::Var,
                    ..
                } => {
                    out.push(name.clone());
                }
                Statement::LetPattern {
                    pattern,
                    kind: VariableKind::Var,
                    ..
                } => {
                    let mut names = FxHashSet::default();
                    Self::collect_pattern_names(pattern, &mut names);
                    out.extend(names);
                }
                Statement::Block(body)
                | Statement::MultiLet(body)
                | Statement::While { body, .. }
                | Statement::DoWhile { body, .. } => {
                    Self::collect_var_names(body, out);
                }
                Statement::For { init, body, .. } => {
                    if let Some(init_stmt) = init {
                        Self::collect_var_names(std::slice::from_ref(init_stmt), out);
                    }
                    Self::collect_var_names(body, out);
                }
                Statement::ForOf { binding, body, .. } => {
                    if let crate::ast::ForBinding::Identifier(name) = binding {
                        out.push(name.clone());
                    }
                    Self::collect_var_names(body, out);
                }
                Statement::ForIn { var_name, body, .. } => {
                    out.push(var_name.clone());
                    Self::collect_var_names(body, out);
                }
                Statement::Labeled { statement, .. } => {
                    Self::collect_var_names(std::slice::from_ref(statement), out);
                }
                Statement::Try {
                    try_block,
                    catch_block,
                    finally_block,
                    ..
                } => {
                    Self::collect_var_names(try_block, out);
                    if let Some(cb) = catch_block {
                        Self::collect_var_names(cb, out);
                    }
                    if let Some(fb) = finally_block {
                        Self::collect_var_names(fb, out);
                    }
                }
                Statement::Switch { cases, .. } => {
                    for case in cases {
                        Self::collect_var_names(&case.consequent, out);
                    }
                }
                Statement::Expression(Expression::If {
                    consequence,
                    alternative,
                    ..
                }) => {
                    Self::collect_var_names(consequence, out);
                    if let Some(alt) = alternative {
                        Self::collect_var_names(alt, out);
                    }
                }
                _ => {}
            }
        }
    }

    /// Recursively collects let/const variable names from for-loop init positions.
    /// These need their inherited global slots removed so load_identifier_into
    /// reads the local register instead of the parent's stale global value.
    #[allow(dead_code)]
    fn collect_for_init_let_names(stmts: &[Statement], out: &mut Vec<String>) {
        for stmt in stmts {
            match stmt {
                Statement::For { init, body, .. } => {
                    if let Some(init_stmt) = init {
                        match init_stmt.as_ref() {
                            Statement::Let { name, kind, .. }
                                if *kind != VariableKind::Var =>
                            {
                                out.push(name.clone());
                            }
                            Statement::MultiLet(stmts) => {
                                for s in stmts {
                                    if let Statement::Let { name, kind, .. } = s {
                                        if *kind != VariableKind::Var {
                                            out.push(name.clone());
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // Recurse into for-body for nested for-loops
                    Self::collect_for_init_let_names(body, out);
                }
                // Recurse into other compound statements
                Statement::Block(body)
                | Statement::While { body, .. }
                | Statement::DoWhile { body, .. } => {
                    Self::collect_for_init_let_names(body, out);
                }
                Statement::Expression(Expression::If {
                    consequence,
                    alternative,
                    ..
                }) => {
                    Self::collect_for_init_let_names(consequence, out);
                    if let Some(alt) = alternative {
                        Self::collect_for_init_let_names(alt, out);
                    }
                }
                Statement::Try {
                    try_block,
                    catch_block,
                    finally_block,
                    ..
                } => {
                    Self::collect_for_init_let_names(try_block, out);
                    if let Some(cb) = catch_block {
                        Self::collect_for_init_let_names(cb, out);
                    }
                    if let Some(fb) = finally_block {
                        Self::collect_for_init_let_names(fb, out);
                    }
                }
                Statement::Switch { cases, .. } => {
                    for case in cases {
                        Self::collect_for_init_let_names(&case.consequent, out);
                    }
                }
                Statement::Labeled { statement, .. } => {
                    Self::collect_for_init_let_names(
                        std::slice::from_ref(statement.as_ref()),
                        out,
                    );
                }
                _ => {}
            }
        }
    }

    /// This ensures their registers are allocated before any temp registers,
    /// preventing ensure_local from claiming a temp that holds the source object.
    fn pre_declare_pattern_locals(&mut self, pattern: &BindingPattern) {
        if !self.is_function_scope {
            return;
        }
        match pattern {
            BindingPattern::Array(items) => {
                for item in items {
                    match item {
                        ArrayBindingItem::Binding { target, .. } => {
                            self.pre_declare_target_locals(target);
                        }
                        ArrayBindingItem::Rest { name } => {
                            self.ensure_local(name);
                        }
                        ArrayBindingItem::Hole => {}
                    }
                }
            }
            BindingPattern::Object(pairs) => {
                for pair in pairs {
                    self.pre_declare_target_locals(&pair.target);
                }
            }
        }
    }

    fn pre_declare_target_locals(&mut self, target: &BindingTarget) {
        match target {
            BindingTarget::Identifier(name) => {
                self.ensure_local(name);
            }
            BindingTarget::Pattern(pattern) => {
                self.pre_declare_pattern_locals(pattern);
            }
        }
    }

    fn apply_default(
        &mut self,
        val: u16,
        default_value: Option<&Expression>,
    ) -> Result<u16, String> {
        let Some(default_expr) = default_value else {
            return Ok(val);
        };
        // Check if val is undefined
        let typeof_r = self.alloc_temp();
        self.emit(ROp::Typeof, &[typeof_r, val]);
        let undef_str = self.add_constant_string(Rc::from("undefined"));
        let undef_r = self.alloc_temp();
        self.emit(ROp::LoadConst, &[undef_r, undef_str]);
        let cmp = self.alloc_temp();
        self.emit(ROp::Equal, &[cmp, typeof_r, undef_r]);
        let skip_pos = self.emit(ROp::JumpIfNot, &[cmp, 9999]);

        // val is undefined, use default
        self.compile_expression_into(default_expr, val)?;

        let end = self.instructions.len();
        self.patch_jump(skip_pos, end);
        Ok(val)
    }

    fn compile_object_rest(
        &mut self,
        src: u16,
        excluded_keys: &[Expression],
    ) -> Result<u16, String> {
        let keys_base = self.next_temp;
        for key in excluded_keys {
            let r = self.alloc_temp();
            self.compile_expression_into(key, r)?;
        }
        let dst = self.alloc_temp();
        self.emit(
            ROp::ObjectRest,
            &[
                dst,
                src,
                keys_base,
                excluded_keys.len() as u16,
            ],
        );
        Ok(dst)
    }

    fn destructure_array_assignment(
        &mut self,
        items: &[Expression],
        src: u16,
    ) -> Result<(), String> {
        for (i, item) in items.iter().enumerate() {
            match item {
                Expression::Spread { value } => {
                    if i + 1 != items.len() {
                        return Err("array rest element in assignment must be last".to_string());
                    }
                    if let Expression::Identifier(name) = &**value {
                        let rest = self.alloc_temp();
                        self.emit(ROp::IteratorRest, &[rest, src, i as u16]);
                        self.store_identifier(name, rest)?;
                    } else {
                        return Err("array rest assignment requires identifier target".to_string());
                    }
                }
                Expression::Identifier(name) => {
                    let key = self.alloc_temp();
                    let key_idx = self.add_constant_int(i as i64);
                    self.emit(ROp::LoadConst, &[key, key_idx]);
                    let val = self.alloc_temp();
                    self.emit(ROp::Index, &[val, src, key]);
                    self.store_identifier(name, val)?;
                }
                Expression::Assign {
                    left,
                    operator,
                    right,
                } if operator == "=" => {
                    let key = self.alloc_temp();
                    let key_idx = self.add_constant_int(i as i64);
                    self.emit(ROp::LoadConst, &[key, key_idx]);
                    let val = self.alloc_temp();
                    self.emit(ROp::Index, &[val, src, key]);
                    let val = self.apply_default(val, Some(right.as_ref()))?;
                    match &**left {
                        Expression::Identifier(name) => {
                            self.store_identifier(name, val)?;
                        }
                        Expression::Array(_) | Expression::Hash(_) => {
                            let tmp = self.alloc_temp();
                            if val != tmp {
                                self.emit(ROp::Move, &[tmp, val]);
                            }
                            self.compile_assignment_into(
                                left,
                                "=",
                                &Expression::Null, // placeholder, won't be used
                                tmp,
                            )?;
                        }
                        _ => {
                            return Err(
                                "array destructuring supports identifier or nested pattern targets"
                                    .to_string(),
                            );
                        }
                    }
                }
                Expression::Array(_) | Expression::Hash(_) => {
                    let key = self.alloc_temp();
                    let key_idx = self.add_constant_int(i as i64);
                    self.emit(ROp::LoadConst, &[key, key_idx]);
                    let val = self.alloc_temp();
                    self.emit(ROp::Index, &[val, src, key]);
                    // Store to temp, then destructure
                    let tmp_name = self.make_temp_name("arr_nested");
                    self.store_identifier(&tmp_name, val)?;
                    self.compile_assignment_into(
                        item,
                        "=",
                        &Expression::Identifier(tmp_name),
                        val,
                    )?;
                }
                _ => {
                    return Err(
                        "array destructuring assignment supports identifier targets".to_string()
                    );
                }
            }
        }
        Ok(())
    }

    fn destructure_object_assignment(
        &mut self,
        pairs: &[HashEntry],
        src: u16,
    ) -> Result<(), String> {
        let excluded_keys: Vec<Expression> = pairs
            .iter()
            .filter_map(|entry| match entry {
                HashEntry::Spread(_) => None,
                HashEntry::KeyValue { key, .. } => Some(key.clone()),
                HashEntry::Method { key, .. } => Some(key.clone()),
                HashEntry::Getter { key, .. } => Some(key.clone()),
                HashEntry::Setter { key, .. } => Some(key.clone()),
            })
            .collect();

        for entry in pairs {
            match entry {
                HashEntry::Spread(target_expr) => {
                    let name = match target_expr {
                        Expression::Identifier(name) => name,
                        _ => {
                            return Err(
                                "object rest destructuring requires identifier target".to_string()
                            )
                        }
                    };
                    let rest = self.compile_object_rest(src, &excluded_keys)?;
                    self.store_identifier(name, rest)?;
                }
                HashEntry::KeyValue {
                    key: key_expr,
                    value: target_expr,
                } => {
                    let key_r = self.compile_expression(key_expr)?;
                    let val = self.alloc_temp();
                    self.emit(ROp::Index, &[val, src, key_r]);

                    match target_expr {
                        Expression::Identifier(name) => {
                            self.store_identifier(name, val)?;
                        }
                        Expression::Assign {
                            left,
                            operator,
                            right,
                        } if operator == "=" => {
                            let val = self.apply_default(val, Some(right.as_ref()))?;
                            match &**left {
                                Expression::Identifier(name) => {
                                    self.store_identifier(name, val)?;
                                }
                                _ => {
                                    let tmp_name = self.make_temp_name("obj_nested");
                                    self.store_identifier(&tmp_name, val)?;
                                    let tmp = self.alloc_temp();
                                    self.compile_assignment_into(
                                        left,
                                        "=",
                                        &Expression::Identifier(tmp_name),
                                        tmp,
                                    )?;
                                }
                            }
                        }
                        Expression::Array(_) | Expression::Hash(_) => {
                            let tmp_name = self.make_temp_name("obj_nested");
                            self.store_identifier(&tmp_name, val)?;
                            let tmp = self.alloc_temp();
                            self.compile_assignment_into(
                                target_expr,
                                "=",
                                &Expression::Identifier(tmp_name),
                                tmp,
                            )?;
                        }
                        _ => {
                            return Err(
                                "object destructuring supports identifier targets".to_string()
                            );
                        }
                    }
                }
                HashEntry::Method { .. } | HashEntry::Getter { .. } | HashEntry::Setter { .. } => {
                    return Err(
                        "methods/getters/setters not valid in destructuring assignment".to_string(),
                    );
                }
            }
        }
        Ok(())
    }

    // ── Try/Throw ────────────────────────────────────────────────────────



    // ── Delete ───────────────────────────────────────────────────────────

    fn compile_delete_into(&mut self, value: &Expression, dst: u16) -> Result<(), String> {
        match value {
            Expression::Index { left, index } => {
                let obj = self.compile_expression(left)?;
                let key = self.compile_expression(index)?;
                self.emit(ROp::DeleteProp, &[dst, obj, key]);

                // Store mutated object back if it's an identifier
                if let Expression::Identifier(name) = &**left {
                    if let Some(&r) = self.locals.get(name.as_str()) {
                        self.emit(ROp::Move, &[r, obj]);
                    } else if let Some(&g) = self.globals.get(name.as_str()) {
                        self.emit(ROp::SetGlobal, &[g, obj]);
                    }
                }
                // Result is true
                self.emit(ROp::LoadTrue, &[dst]);
            }
            Expression::Identifier(_) => {
                self.emit(ROp::LoadFalse, &[dst]);
            }
            _ => {
                let _ = self.compile_expression(value)?;
                self.emit(ROp::LoadTrue, &[dst]);
            }
        }
        Ok(())
    }

    // ── Function / Class compilation ─────────────────────────────────────


    // ── Builtins (reused from stack compiler) ────────────────────────────

    fn builtin_global_object(name: &str) -> Option<Object> {
        // Delegate to the stack compiler's builtin_global_object
        crate::runtime::globals::builtin_global_object(name)
    }

    // ── Utilities ────────────────────────────────────────────────────────

    fn emit(&mut self, op: ROp, operands: &[u16]) -> usize {
        let pos = self.instructions.len();
        let u32_ops: Vec<u32> = operands.iter().map(|&v| v as u32).collect();
        self.instructions.extend(rmake(op, &u32_ops));
        pos
    }

    /// Emit a jump instruction with a usize target (no u16 truncation).
    fn emit_jump(&mut self, op: ROp, pre_operands: &[u16], target: usize) -> usize {
        let pos = self.instructions.len();
        let mut ops: Vec<u32> = pre_operands.iter().map(|&v| v as u32).collect();
        ops.push(target as u32);
        self.instructions.extend(rmake(op, &ops));
        pos
    }

    /// Patch a jump instruction's target. Target encoded as u32 big-endian
    /// to support functions > 65535 bytes.
    fn patch_jump(&mut self, op_pos: usize, target: usize) {
        let op_byte = self.instructions[op_pos];
        let op = ROp::from_byte(op_byte).expect("valid opcode");
        // Find the position of the jump target operand (u32 big-endian)
        let target_offset = match op {
            ROp::Jump => 1,         // [target:4] — offset 1
            ROp::JumpIfNot => 3,    // [cond:2, target:4] — offset 3
            ROp::JumpIfTruthy => 3, // [cond:2, target:4] — offset 3
            ROp::TestLtConstJump | ROp::TestLeConstJump | ROp::IncrementRegAndJump
            | ROp::TestLtRegJump | ROp::TestLeRegJump => 5,
            ROp::ModRegConstStrictEqConstJump | ROp::TestModRegStrictEqConstJump => 7,
            ROp::EnterTry => 1, // [catch_target:4] starts at offset 1
            _ => {
                debug_assert!(false, "patch_jump on non-jump opcode {:?}", op);
                return;
            }
        };
        let pos = op_pos + target_offset;
        let t = target as u32;
        self.instructions[pos] = ((t >> 24) & 0xff) as u8;
        self.instructions[pos + 1] = ((t >> 16) & 0xff) as u8;
        self.instructions[pos + 2] = ((t >> 8) & 0xff) as u8;
        self.instructions[pos + 3] = (t & 0xff) as u8;
    }

    /// Pop the current loop context and patch all break/continue jump targets.
    fn patch_loop_exits(&mut self, loop_end: usize) {
        if let Some(ctx) = self.loop_stack.pop() {
            for pos in ctx.break_positions {
                self.patch_jump(pos, loop_end);
            }
            for pos in ctx.continue_positions {
                self.patch_jump(pos, ctx.continue_target);
            }
        }
    }

    fn make_temp_name(&mut self, prefix: &str) -> String {
        let name = format!("__fl_{}_{}", prefix, self.temp_counter);
        self.temp_counter += 1;
        name
    }

    fn store_identifier(&mut self, name: &str, src: u16) -> Result<(), String> {
        if self.is_function_scope {
            let r = self.ensure_local(name);
            if r != src {
                self.emit(ROp::Move, &[r, src]);
            }
            self.mirror_local_to_global(name, r);
        } else {
            let g = self.ensure_global_slot(name)?;
            self.emit(ROp::SetGlobal, &[g, src]);
        }
        Ok(())
    }

    /// Prepare block scoping: remove let/const names from locals so they get
    /// fresh registers inside the block.  Returns the saved (name, old_reg)
    /// pairs that must be restored after the block.
    fn enter_block_scope(&mut self, stmts: &[Statement]) -> Vec<(String, Option<u16>, bool)> {
        let mut shadowed = Vec::new();
        for s in stmts {
            match s {
                Statement::Let { name, kind, .. } if *kind != VariableKind::Var => {
                    let old_reg = self.locals.remove(name);
                    let was_const = self.const_bindings.remove(name);
                    shadowed.push((name.clone(), old_reg, was_const));
                }
                Statement::LetPattern { pattern, kind, .. } if *kind != VariableKind::Var => {
                    let mut names = rustc_hash::FxHashSet::default();
                    Self::collect_pattern_names(pattern, &mut names);
                    for name in names {
                        let old_reg = self.locals.remove(&name);
                        let was_const = self.const_bindings.remove(&name);
                        shadowed.push((name, old_reg, was_const));
                    }
                }
                _ => {}
            }
        }
        shadowed
    }

    /// Restore bindings saved by `enter_block_scope`.
    fn exit_block_scope(&mut self, shadowed: Vec<(String, Option<u16>, bool)>) {
        for (name, old_reg, was_const) in shadowed {
            // Remove any const binding added inside the block
            self.const_bindings.remove(&name);
            // Restore the original register mapping
            if let Some(r) = old_reg {
                self.locals.insert(name.clone(), r);
            } else {
                self.locals.remove(&name);
            }
            // Restore const status if it was const before the block
            if was_const {
                self.const_bindings.insert(name);
            }
        }
    }

    fn ensure_binding_register(&mut self, name: &str) -> Result<u16, String> {
        if self.is_function_scope {
            Ok(self.ensure_local(name))
        } else {
            // For globals, use a temp register
            Ok(self.alloc_temp())
        }
    }

    fn write_binding(&mut self, name: &str, src: u16) -> Result<(), String> {
        if !self.is_function_scope {
            let g = self.ensure_global_slot(name)?;
            self.emit(ROp::SetGlobal, &[g, src]);
        }
        Ok(())
    }

    fn find_loop_ctx_mut(&mut self, label: Option<&str>) -> Result<&mut LoopContext, String> {
        match label {
            Some(target) => self
                .loop_stack
                .iter_mut()
                .rev()
                .find(|ctx| ctx.label.as_deref() == Some(target))
                .ok_or_else(|| format!("unknown loop label '{}'", target)),
            None => self
                .loop_stack
                .last_mut()
                .ok_or_else(|| "loop control outside loop".to_string()),
        }
    }

    // ── Fused opcode pattern detection ──────────────────────────────────

    /// Try to extract a numeric constant index from an expression.
    fn try_numeric_const(&mut self, expr: &Expression) -> Option<u16> {
        match expr {
            Expression::Integer(v) => Some(self.add_constant_int(*v)),
            Expression::Float(v) => Some(self.add_constant_float(*v)),
            _ => None,
        }
    }

    /// Try to detect `local < CONST` or `local <= CONST` pattern.
    /// Returns (register, const_index, is_le).
    fn try_fused_cmp_const(&mut self, expr: &Expression) -> Option<(u16, u16, bool)> {
        if let Expression::Infix {
            left,
            operator,
            right,
        } = expr
        {
            let is_lt = operator == "<";
            let is_le = operator == "<=";
            if !is_lt && !is_le {
                return None;
            }
            if let Expression::Identifier(name) = left.as_ref() {
                if let Some(&r) = self.locals.get(name) {
                    if let Some(const_idx) = self.try_numeric_const(right) {
                        return Some((r, const_idx, is_le));
                    }
                }
            }
        }
        None
    }

    /// Try to detect `local < local` or `local <= local` pattern for register-vs-register
    /// fused comparison+jump. Returns (left_reg, right_reg, is_le).
    fn try_fused_cmp_reg(&self, expr: &Expression) -> Option<(u16, u16, bool)> {
        if let Expression::Infix {
            left,
            operator,
            right,
        } = expr
        {
            let is_lt = operator == "<";
            let is_le = operator == "<=";
            if !is_lt && !is_le {
                return None;
            }
            if let Expression::Identifier(lname) = left.as_ref() {
                if let Some(&lr) = self.locals.get(lname) {
                    if let Expression::Identifier(rname) = right.as_ref() {
                        if let Some(&rr) = self.locals.get(rname) {
                            return Some((lr, rr, is_le));
                        }
                    }
                }
            }
        }
        None
    }

    /// Try to detect `(local % CONST_A) === CONST_B` pattern.
    /// Returns (register, mod_const_idx, cmp_const_idx).
    fn try_fused_mod_strict_eq(&mut self, expr: &Expression) -> Option<(u16, u16, u16)> {
        if let Expression::Infix {
            left,
            operator,
            right,
        } = expr
        {
            if operator != "===" {
                return None;
            }
            if let Expression::Infix {
                left: mod_left,
                operator: mod_op,
                right: mod_right,
            } = left.as_ref()
            {
                if mod_op != "%" {
                    return None;
                }
                if let Expression::Identifier(name) = mod_left.as_ref() {
                    if let Some(&r) = self.locals.get(name) {
                        if let Some(mod_const) = self.try_numeric_const(mod_right) {
                            if let Some(cmp_const) = self.try_numeric_const(right) {
                                return Some((r, mod_const, cmp_const));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Try to detect `local = local + CONST` or `local += CONST` pattern.
    /// Returns (register, const_index).
    fn try_fused_increment<'a>(&mut self, expr: &'a Expression) -> Option<(u16, u16, &'a str)> {
        if let Expression::Assign {
            left,
            operator,
            right,
        } = expr
        {
            if let Expression::Identifier(name) = left.as_ref() {
                if let Some(&r) = self.locals.get(name.as_str()) {
                    if operator == "+=" {
                        if let Some(const_idx) = self.try_numeric_const(right) {
                            return Some((r, const_idx, name.as_str()));
                        }
                    } else if operator == "=" {
                        if let Expression::Infix {
                            left: inner_left,
                            operator: inner_op,
                            right: inner_right,
                        } = right.as_ref()
                        {
                            if inner_op == "+" {
                                if let Expression::Identifier(inner_name) = inner_left.as_ref() {
                                    if inner_name == name {
                                        if let Some(const_idx) = self.try_numeric_const(inner_right)
                                        {
                                            return Some((r, const_idx, name.as_str()));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Compile a loop condition, trying fused opcodes first.
    /// Returns the position of the jump instruction to patch.

    /// If the local variable also has a global slot, emit SetGlobal to keep
    /// the global in sync. This is required for closure capture correctness:
    /// inner function scopes access outer variables via GetGlobal.
    fn mirror_local_to_global(&mut self, name: &str, reg: u16) {
        if self.needs_global(name) {
            if let Some(&g) = self.globals.get(name) {
                self.emit(ROp::SetGlobal, &[g, reg]);
            }
        }
    }

    /// After a function call returns, reload local registers from their global
    /// counterparts to handle the case where the callee modified a global.
    #[allow(dead_code)]
    fn reload_locals_from_globals(&mut self) {
        let pairs: Vec<(u16, u16)> = self
            .locals
            .iter()
            .filter_map(|(name, &r)| self.globals.get(name).map(|&g| (r, g)))
            .collect();
        for (r, g) in pairs {
            self.emit(ROp::GetGlobal, &[r, g]);
        }
    }

    fn try_resolve_global_function(&self, expr: &Expression) -> Option<u16> {
        if let Expression::Identifier(name) = expr {
            if self.locals.contains_key(name) {
                return None;
            }
            if let Some(&idx) = self.globals.get(name) {
                return Some(idx);
            }
        }
        None
    }

    fn add_constant(&mut self, obj: Object) -> u16 {
        self.constants.push(obj);
        let idx = self.constants.len() - 1;
        // `as u16` on `idx > u16::MAX` would silently truncate and
        // make every subsequent `LoadConst` alias the wrong constant.
        // The compiler doesn't currently emit anywhere near 65 535
        // constants, but a future regression (e.g. unbounded inline
        // string interning) would otherwise corrupt the program. The
        // `try_from` failure becomes a sticky compile error.
        match u16::try_from(idx) {
            Ok(v) => v,
            Err(_) => {
                self.record_overflow("constant table exceeded u16::MAX entries");
                u16::MAX
            }
        }
    }

    fn add_constant_string(&mut self, s: Rc<str>) -> u16 {
        if let Some(&idx) = self.constant_strings.get(&s) {
            return idx;
        }
        let interned = intern_str(&s);
        let idx = self.add_constant(Object::String(Rc::clone(&interned)));
        self.constant_strings.insert(interned, idx);
        idx
    }

    fn add_constant_int(&mut self, v: i64) -> u16 {
        if let Some(&idx) = self.constant_ints.get(&v) {
            return idx;
        }
        let idx = self.add_constant(Object::Integer(v));
        self.constant_ints.insert(v, idx);
        idx
    }

    fn add_constant_float(&mut self, v: f64) -> u16 {
        let bits = v.to_bits();
        if let Some(&idx) = self.constant_floats.get(&bits) {
            return idx;
        }
        let idx = self.add_constant(Object::Float(v));
        self.constant_floats.insert(bits, idx);
        idx
    }
}

enum BindingSlot {
    Local(u16),
    Global(u16),
}

// ── Captured-name scanner ─────────────────────────────────────────────────
// Walks top-level statements to find identifiers referenced inside nested
// function bodies. Only these names need global slots at top level.

pub(super) fn scan_captured_names(stmts: &[Statement]) -> FxHashSet<String> {
    let mut captured = FxHashSet::default();
    for stmt in stmts {
        scan_stmt_captures(stmt, &mut captured, false);
    }
    captured
}

/// Check if a function body uses `this` (directly, not inside nested non-arrow functions).
/// Uses the existing scan_expr_captures infrastructure with a sentinel set.
fn scan_body_uses_this(stmts: &[Statement]) -> bool {
    let mut out = FxHashSet::default();
    for stmt in stmts {
        scan_stmt_captures(stmt, &mut out, true);
        // Also check for Expression::This directly in statements
        scan_stmt_for_this(stmt, &mut out);
    }
    out.contains("this")
}

fn scan_stmt_for_this(stmt: &Statement, out: &mut FxHashSet<String>) {
    match stmt {
        Statement::Expression(expr) => scan_expr_for_this(expr, out),
        Statement::Let { value, .. } | Statement::LetPattern { value, .. } => {
            scan_expr_for_this(value, out)
        }
        Statement::Return { value } => scan_expr_for_this(value, out),
        Statement::Block(stmts) | Statement::MultiLet(stmts) => {
            for s in stmts {
                scan_stmt_for_this(s, out);
            }
        }
        _ => {
            // For other statement types, walk children
            // We rely on the fact that Expression::This in expression statements
            // is the most common case
        }
    }
}

fn scan_expr_for_this(expr: &Expression, out: &mut FxHashSet<String>) {
    match expr {
        Expression::This => {
            out.insert("this".to_string());
        }
        Expression::Function { is_arrow, body, .. } => {
            // Arrow functions inherit `this`, so keep scanning.
            // Regular functions have their own `this`, stop.
            if *is_arrow {
                for s in body {
                    scan_stmt_for_this(s, out);
                }
            }
        }
        Expression::Prefix { right, .. } => scan_expr_for_this(right, out),
        Expression::Infix { left, right, .. } | Expression::Assign { left, right, .. } => {
            scan_expr_for_this(left, out);
            scan_expr_for_this(right, out);
        }
        Expression::Index { left, index, .. } => {
            scan_expr_for_this(left, out);
            scan_expr_for_this(index, out);
        }
        Expression::Call { function, arguments, .. }
        | Expression::OptionalCall { function, arguments, .. } => {
            scan_expr_for_this(function, out);
            for arg in arguments {
                scan_expr_for_this(arg, out);
            }
        }
        Expression::Array(items) => {
            for item in items {
                scan_expr_for_this(item, out);
            }
        }
        Expression::If { condition, consequence, alternative } => {
            scan_expr_for_this(condition, out);
            for s in consequence {
                scan_stmt_for_this(s, out);
            }
            if let Some(alt) = alternative {
                for s in alt {
                    scan_stmt_for_this(s, out);
                }
            }
        }
        Expression::Spread { value } => scan_expr_for_this(value, out),
        Expression::Update { target, .. } => scan_expr_for_this(target, out),
        _ => {}
    }
}

fn scan_stmt_captures(stmt: &Statement, out: &mut FxHashSet<String>, in_func: bool) {
    match stmt {
        Statement::Let { value, .. } => {
            scan_expr_captures(value, out, in_func);
        }
        Statement::LetPattern { value, .. } => {
            scan_expr_captures(value, out, in_func);
        }
        Statement::Return { value } => {
            scan_expr_captures(value, out, in_func);
        }
        Statement::ReturnVoid => {}
        Statement::Expression(expr) => {
            scan_expr_captures(expr, out, in_func);
        }
        Statement::Block(stmts) | Statement::MultiLet(stmts) => {
            for s in stmts {
                scan_stmt_captures(s, out, in_func);
            }
        }
        Statement::While { condition, body } => {
            scan_expr_captures(condition, out, in_func);
            for s in body {
                scan_stmt_captures(s, out, in_func);
            }
        }
        Statement::For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(init) = init {
                scan_stmt_captures(init, out, in_func);
            }
            if let Some(cond) = condition {
                scan_expr_captures(cond, out, in_func);
            }
            if let Some(upd) = update {
                scan_expr_captures(upd, out, in_func);
            }
            for s in body {
                scan_stmt_captures(s, out, in_func);
            }
        }
        Statement::ForOf { iterable, body, .. } => {
            scan_expr_captures(iterable, out, in_func);
            for s in body {
                scan_stmt_captures(s, out, in_func);
            }
        }
        Statement::ForIn { iterable, body, .. } => {
            scan_expr_captures(iterable, out, in_func);
            for s in body {
                scan_stmt_captures(s, out, in_func);
            }
        }
        Statement::FunctionDecl { body, parameters, .. } => {
            let mut inner = FxHashSet::default();
            for s in body {
                scan_stmt_captures(s, &mut inner, true);
            }
            let mut own_decls = FxHashSet::default();
            for p in parameters {
                own_decls.insert(p.trim_start_matches("...").to_string());
            }
            let mut vn = Vec::new();
            RCompiler::collect_var_names(body, &mut vn);
            own_decls.extend(vn);
            for s in body {
                if let Statement::FunctionDecl { name, .. } = s {
                    own_decls.insert(name.clone());
                }
            }
            for name in &inner {
                if !own_decls.contains(name) { out.insert(name.clone()); }
            }
        }
        Statement::ClassDecl { members, .. } => {
            for member in members {
                match member {
                    ClassMember::Method(method) => {
                        for s in &method.body {
                            scan_stmt_captures(s, out, true);
                        }
                    }
                    ClassMember::Field { initializer, .. } => {
                        if let Some(init) = initializer {
                            scan_expr_captures(init, out, in_func);
                        }
                    }
                    ClassMember::StaticBlock { body } => {
                        for s in body {
                            scan_stmt_captures(s, out, true);
                        }
                    }
                }
            }
        }
        Statement::Throw { value } => {
            scan_expr_captures(value, out, in_func);
        }
        Statement::Try {
            try_block,
            catch_block,
            finally_block,
            ..
        } => {
            for s in try_block {
                scan_stmt_captures(s, out, in_func);
            }
            if let Some(cb) = catch_block {
                for s in cb {
                    scan_stmt_captures(s, out, in_func);
                }
            }
            if let Some(fb) = finally_block {
                for s in fb {
                    scan_stmt_captures(s, out, in_func);
                }
            }
        }
        Statement::Labeled { statement, .. } => {
            scan_stmt_captures(statement, out, in_func);
        }
        Statement::Break { .. } | Statement::Continue { .. } | Statement::Debugger => {}
        Statement::DoWhile { body, condition } => {
            for s in body {
                scan_stmt_captures(s, out, in_func);
            }
            scan_expr_captures(condition, out, in_func);
        }
        Statement::Switch {
            discriminant,
            cases,
        } => {
            scan_expr_captures(discriminant, out, in_func);
            for case in cases {
                if let Some(test) = &case.test {
                    scan_expr_captures(test, out, in_func);
                }
                for s in &case.consequent {
                    scan_stmt_captures(s, out, in_func);
                }
            }
        }
    }
}

fn scan_expr_captures(expr: &Expression, out: &mut FxHashSet<String>, in_func: bool) {
    match expr {
        Expression::Identifier(name) => {
            if in_func {
                out.insert(name.clone());
            }
        }
        Expression::Integer(_)
        | Expression::BigInt(_)
        | Expression::Float(_)
        | Expression::String(_)
        | Expression::Boolean(_)
        | Expression::Null
        | Expression::This
        | Expression::Super
        | Expression::NewTarget
        | Expression::ImportMeta
        | Expression::RegExp { .. } => {}
        Expression::Array(items) => {
            for item in items {
                scan_expr_captures(item, out, in_func);
            }
        }
        Expression::Hash(pairs) => {
            for entry in pairs {
                match entry {
                    HashEntry::KeyValue { key, value } => {
                        scan_expr_captures(key, out, in_func);
                        scan_expr_captures(value, out, in_func);
                    }
                    HashEntry::Method {
                        key,
                        parameters: _,
                        body,
                        ..
                    } => {
                        scan_expr_captures(key, out, in_func);
                        for s in body {
                            scan_stmt_captures(s, out, true);
                        }
                    }
                    HashEntry::Spread(expr) => {
                        scan_expr_captures(expr, out, in_func);
                    }
                    HashEntry::Getter { body, .. } | HashEntry::Setter { body, .. } => {
                        for s in body {
                            scan_stmt_captures(s, out, true);
                        }
                    }
                }
            }
        }
        Expression::Prefix { right, .. } => {
            scan_expr_captures(right, out, in_func);
        }
        Expression::Typeof { value }
        | Expression::Void { value }
        | Expression::Delete { value }
        | Expression::Await { value }
        | Expression::Spread { value } => {
            scan_expr_captures(value, out, in_func);
        }
        Expression::Yield { value, .. } => {
            scan_expr_captures(value, out, in_func);
        }
        Expression::Sequence(exprs) => {
            for e in exprs {
                scan_expr_captures(e, out, in_func);
            }
        }
        Expression::Infix { left, right, .. } => {
            scan_expr_captures(left, out, in_func);
            scan_expr_captures(right, out, in_func);
        }
        Expression::If {
            condition,
            consequence,
            alternative,
        } => {
            scan_expr_captures(condition, out, in_func);
            for s in consequence {
                scan_stmt_captures(s, out, in_func);
            }
            if let Some(alt) = alternative {
                for s in alt {
                    scan_stmt_captures(s, out, in_func);
                }
            }
        }
        Expression::Function { body, is_arrow, parameters, .. } => {
            if *is_arrow {
                if scan_body_uses_this(body) {
                    out.insert("this".to_string());
                }
            }
            // Scan body for identifiers used inside this function.
            // Subtract the function's own declarations (params + var + function decls)
            // since those shadow outer-scope variables of the same name.
            let mut inner = FxHashSet::default();
            for s in body {
                scan_stmt_captures(s, &mut inner, true);
            }
            let mut own_decls = FxHashSet::default();
            for p in parameters {
                own_decls.insert(p.trim_start_matches("...").to_string());
            }
            // Collect var-hoisted declarations (they're function-scoped)
            let mut vn = Vec::new();
            RCompiler::collect_var_names(body, &mut vn);
            own_decls.extend(vn);
            // Collect top-level function declarations
            for s in body {
                if let Statement::FunctionDecl { name, .. } = s {
                    own_decls.insert(name.clone());
                }
            }
            // Add captured names that aren't this function's own declarations
            for name in &inner {
                if !own_decls.contains(name) {
                    out.insert(name.clone());
                }
            }
        }
        Expression::Call {
            function,
            arguments,
        }
        | Expression::OptionalCall {
            function,
            arguments,
        } => {
            scan_expr_captures(function, out, in_func);
            for arg in arguments {
                scan_expr_captures(arg, out, in_func);
            }
        }
        Expression::New { callee, arguments } => {
            scan_expr_captures(callee, out, in_func);
            for arg in arguments {
                scan_expr_captures(arg, out, in_func);
            }
        }
        Expression::OptionalIndex { left, index } | Expression::Index { left, index } => {
            scan_expr_captures(left, out, in_func);
            scan_expr_captures(index, out, in_func);
        }
        Expression::Assign { left, right, .. } => {
            scan_expr_captures(left, out, in_func);
            scan_expr_captures(right, out, in_func);
        }
        Expression::Update { target, .. } => {
            scan_expr_captures(target, out, in_func);
        }
        Expression::Class {
            extends, members, ..
        } => {
            if let Some(ext) = extends {
                scan_expr_captures(ext, out, in_func);
            }
            for member in members {
                match member {
                    ClassMember::Method(method) => {
                        for s in &method.body {
                            scan_stmt_captures(s, out, true);
                        }
                    }
                    ClassMember::Field { initializer, .. } => {
                        if let Some(init) = initializer {
                            scan_expr_captures(init, out, in_func);
                        }
                    }
                    ClassMember::StaticBlock { body } => {
                        for s in body {
                            scan_stmt_captures(s, out, true);
                        }
                    }
                }
            }
        }
    }
}
