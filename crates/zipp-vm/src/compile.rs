//! Compile the oxc AST directly to register bytecode.
//!
//! Two passes over the source:
//!
//! 1. **Hoist**: every top-level `function f(...)` and `var/let f = function`
//!    name is assigned a global slot, so calls resolve regardless of textual
//!    order (matching JS function hoisting) and recursion works.
//! 2. **Emit**: each function body compiles to a `FuncProto`. Locals and
//!    parameters live in registers, tracked by a `Scope` (name → register).
//!    Expression results flow into caller-chosen destination registers, which
//!    is what lets a value stay in one place across a basic block.
//!
//! The supported subset (v1): numbers/strings/bools/null, `let`/`const`/`var`,
//! function declarations + calls + recursion, `if/else`, `while`, C-style
//! `for`, `return`, the arithmetic/comparison/logical operators, and
//! `console.log`. Anything else is a clear compile error — coverage grows over
//! time, the same way the old engine did.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use oxc_ast::ast as ox;

use crate::bytecode::{FuncProto, Instr, Program, Reg, UpvalSource};
use crate::capture;
use crate::value::Value;
use crate::vm::STRING_CONST_BIT;

type R<T> = Result<T, String>;

/// Shared, mutable upvalue list of a function: (name, where-the-cell-comes-from).
/// Shared via `Rc<RefCell>` so a deeply-nested function can append upvalues to
/// an ancestor (transitive capture): an intermediate function that only *passes
/// through* a variable still needs to capture it so its child can re-source it.
type UpvalList = Rc<RefCell<Vec<(String, UpvalSource)>>>;

/// A snapshot of an enclosing function's binding environment, used by a nested
/// function to resolve free variables to upvalues. The chain is ordered
/// outermost → innermost-parent; the last entry is the direct parent. The
/// `upvalues` handle is SHARED with the live ancestor `FnCompiler`, so resolving
/// a variable through this snapshot also records the upvalue on the ancestor.
#[derive(Clone)]
struct EnclosingFn {
    /// name → register holding that binding's CELL in the parent frame. Only
    /// captured bindings appear here (non-captured locals can't be upvalues).
    cell_locals: Vec<(String, Reg)>,
    /// The ancestor's own upvalue list (shared handle).
    upvalues: UpvalList,
}

/// Compile a parsed program into bytecode.
pub fn compile_program(prog: &ox::Program) -> R<Program> {
    let mut c = Compiler::new();
    c.compile(prog)?;
    Ok(Program {
        functions: c.functions,
        global_count: c.globals.len() as u32,
    })
}

struct Compiler {
    functions: Vec<FuncProto>,
    /// Global name → slot.
    globals: Vec<String>,
}

impl Compiler {
    fn new() -> Compiler {
        Compiler { functions: Vec::new(), globals: Vec::new() }
    }

    fn global_slot(&mut self, name: &str) -> u16 {
        if let Some(i) = self.globals.iter().position(|g| g == name) {
            return i as u16;
        }
        let i = self.globals.len() as u16;
        self.globals.push(name.to_string());
        i
    }

    fn compile(&mut self, prog: &ox::Program) -> R<()> {
        // Reserve function id 0 for the top-level script body; fill it last so
        // nested function ids are stable as we discover them.
        self.functions.push(placeholder("<script>"));

        // Pass 1: hoist top-level function declaration names to globals.
        for s in &prog.body {
            if let ox::Statement::FunctionDeclaration(f) = s {
                if let Some(id) = &f.id {
                    self.global_slot(id.name.as_str());
                }
            }
        }

        // Compile the top-level body as function 0. The script has no enclosing
        // scope and binds everything to globals, so nothing it declares is a
        // captured cell.
        let top = self.compile_function_body(
            None,
            &[],
            &prog.body,
            true,
            HashSet::new(),
            Vec::new(),
        )?;
        self.functions[0] = top;
        Ok(())
    }

    /// Compile a function (or the script top-level when `is_script`).
    /// `params` are parameter names; `body` are its statements.
    fn compile_function_body(
        &mut self,
        name: Option<&str>,
        params: &[String],
        body: &[ox::Statement],
        is_script: bool,
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
    ) -> R<FuncProto> {
        let mut fc = FnCompiler::new(self, params, captured, enclosing);
        fc.is_script = is_script;

        // Hoist function declarations in this body so calls resolve before the
        // textual definition. Top-level names become globals (the VM
        // materialises the function object at startup via `name_global`).
        // Nested names become locals, populated by a `MakeFunc` at the point
        // `func_decl` reaches them.
        for s in body {
            if let ox::Statement::FunctionDeclaration(f) = s {
                if let Some(id) = &f.id {
                    if is_script {
                        fc.cx.global_slot(id.name.as_str());
                    } else {
                        fc.declare_local(id.name.as_str());
                    }
                }
            }
        }

        for s in body {
            fc.stmt(s)?;
        }
        fc.emit(Instr::ReturnUndefined);

        let upvalues: Vec<UpvalSource> =
            fc.upvalues.borrow().iter().map(|(_, s)| *s).collect();
        Ok(FuncProto {
            name: name.unwrap_or("<script>").to_string(),
            code: fc.code,
            reg_count: fc.max_reg,
            param_count: params.len() as u16,
            constants: fc.constants,
            string_constants: fc.string_constants,
            name_global: None, // set by the caller for top-level declarations
            upvalues,
        })
    }

    /// Compile an arrow function body (expression- or block-bodied).
    fn compile_arrow_body(
        &mut self,
        params: &[String],
        a: &ox::ArrowFunctionExpression,
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
    ) -> R<FuncProto> {
        let mut fc = FnCompiler::new(self, params, captured, enclosing);
        if a.expression {
            // `x => expr`: the body is a single ExpressionStatement to return.
            let mut returned = false;
            for s in &a.body.statements {
                if let ox::Statement::ExpressionStatement(es) = s {
                    let r = fc.expr(&es.expression)?;
                    fc.emit(Instr::Return { src: r });
                    returned = true;
                }
            }
            if !returned {
                fc.emit(Instr::ReturnUndefined);
            }
        } else {
            // hoist nested function declarations (same as a normal body)
            for s in &a.body.statements {
                if let ox::Statement::FunctionDeclaration(f) = s {
                    if let Some(id) = &f.id {
                        fc.declare_local(id.name.as_str());
                    }
                }
            }
            for s in &a.body.statements {
                fc.stmt(s)?;
            }
            fc.emit(Instr::ReturnUndefined);
        }
        let upvalues: Vec<UpvalSource> =
            fc.upvalues.borrow().iter().map(|(_, s)| *s).collect();
        Ok(FuncProto {
            name: "<arrow>".to_string(),
            code: fc.code,
            reg_count: fc.max_reg,
            param_count: params.len() as u16,
            constants: fc.constants,
            string_constants: fc.string_constants,
            name_global: None,
            upvalues,
        })
    }
}

/// Resolve `name` to an `UpvalSource` for the INNERMOST function whose enclosing
/// chain is `chain` (outermost → direct parent). The direct parent is
/// `chain.last()`. If the variable is a cell-local of the direct parent, the
/// source is `ParentLocal`. Otherwise the parent must itself capture it as one
/// of its upvalues — found, or recursively created by capturing from the
/// grandparent — and the source is `ParentUpval(parent's upvalue index)`. This
/// is the standard Lua "find/create upvalue" walk, threading a capture through
/// every intermediate level. `None` if no enclosing function binds `name`.
fn capture_source(chain: &[EnclosingFn], name: &str) -> Option<UpvalSource> {
    let (parent, outer) = chain.split_last()?;
    // Direct parent holds the cell as a local.
    if let Some((_, reg)) = parent.cell_locals.iter().find(|(n, _)| n == name) {
        return Some(UpvalSource::ParentLocal(*reg));
    }
    // Parent already captured it as an upvalue.
    if let Some(i) = parent.upvalues.borrow().iter().position(|(n, _)| n == name) {
        return Some(UpvalSource::ParentUpval(i as u16));
    }
    // Otherwise make the parent capture it from ITS enclosing chain, then point
    // at the parent's freshly-added upvalue.
    let src_for_parent = capture_source(outer, name)?;
    let mut pups = parent.upvalues.borrow_mut();
    let idx = pups.len() as u16;
    pups.push((name.to_string(), src_for_parent));
    Some(UpvalSource::ParentUpval(idx))
}

fn placeholder(name: &str) -> FuncProto {
    FuncProto {
        name: name.to_string(),
        code: Vec::new(),
        reg_count: 0,
        param_count: 0,
        constants: Vec::new(),
        string_constants: Vec::new(),
        name_global: None,
        upvalues: Vec::new(),
    }
}

/// Per-function compilation state.
struct FnCompiler<'a> {
    cx: &'a mut Compiler,
    code: Vec<Instr>,
    constants: Vec<Value>,
    string_constants: Vec<String>,
    /// Lexical scope chain: each entry is (name, register).
    scopes: Vec<Vec<(String, Reg)>>,
    /// Next free register / high-water mark.
    next_reg: Reg,
    max_reg: Reg,
    /// True for the top-level script body: declarations (functions AND
    /// let/const/var) bind to globals rather than registers, so only genuinely
    /// nested functions ever capture.
    is_script: bool,
    /// This function's own bindings that some nested function captures; these
    /// are boxed into heap cells at declaration so the closure shares the slot.
    captured: HashSet<String>,
    /// Registers currently holding a CELL (a boxed captured binding), so reads/
    /// writes of them go through CellGet/CellSet.
    cell_regs: HashSet<Reg>,
    /// Upvalues this function captures, built lazily as free vars are resolved:
    /// (name, source-in-parent). Index in this Vec is the runtime upvalue index.
    /// Shared (`Rc<RefCell>`) so nested functions can append transitively.
    upvalues: UpvalList,
    /// Enclosing functions' binding snapshots (outermost → direct parent).
    enclosing: Vec<EnclosingFn>,
}

impl<'a> FnCompiler<'a> {
    fn new(
        cx: &'a mut Compiler,
        params: &[String],
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
    ) -> FnCompiler<'a> {
        let mut fc = FnCompiler {
            cx,
            code: Vec::new(),
            constants: Vec::new(),
            string_constants: Vec::new(),
            scopes: vec![Vec::new()],
            next_reg: 0,
            max_reg: 0,
            is_script: false,
            captured,
            cell_regs: HashSet::new(),
            upvalues: Rc::new(RefCell::new(Vec::new())),
            enclosing,
        };
        // Register 0 is reserved for `this` in every function (undefined for
        // plain calls, the receiver for method calls). Parameters follow at
        // registers 1.., in order. This uniform convention lets the call path
        // treat `this` as just another register slot.
        let _this_reg = fc.alloc_reg(); // reg 0 = this
        for p in params {
            let r = fc.alloc_reg();
            fc.scopes[0].push((p.clone(), r));
            // A captured parameter is boxed into a cell immediately so a nested
            // closure shares the live slot (mutations visible both ways).
            if fc.captured.contains(p) {
                fc.emit(Instr::MakeCell { reg: r });
                fc.cell_regs.insert(r);
            }
        }
        fc
    }

    /// Snapshot this function's environment for a nested function to capture
    /// from: cell-backed locals in scope, plus a SHARED handle to this
    /// function's upvalue list (so a grandchild can both read and transitively
    /// extend it via ParentUpval re-sourcing).
    fn snapshot(&self) -> EnclosingFn {
        let mut cell_locals = Vec::new();
        for scope in &self.scopes {
            for (name, reg) in scope {
                if self.cell_regs.contains(reg) {
                    cell_locals.push((name.clone(), *reg));
                }
            }
        }
        EnclosingFn { cell_locals, upvalues: self.upvalues.clone() }
    }

    /// Resolve a free variable to an upvalue index in THIS function, creating
    /// the upvalue on first use. `None` if not found in any enclosing function
    /// (then it's a global). Transitive: if the variable lives in an ancestor
    /// beyond the direct parent, every intermediate function captures it too.
    fn resolve_upvalue(&mut self, name: &str) -> Option<u16> {
        if let Some(i) = self.upvalues.borrow().iter().position(|(n, _)| n == name) {
            return Some(i as u16);
        }
        let src = capture_source(&self.enclosing, name)?;
        let mut ups = self.upvalues.borrow_mut();
        let idx = ups.len() as u16;
        ups.push((name.to_string(), src));
        Some(idx)
    }

    // ── register allocation ──
    fn alloc_reg(&mut self) -> Reg {
        let r = self.next_reg;
        self.next_reg += 1;
        if self.next_reg > self.max_reg {
            self.max_reg = self.next_reg;
        }
        r
    }
    /// A scratch register that the caller will stop using immediately; we still
    /// bump the high-water mark but let it be reused by resetting next_reg.
    fn temp(&mut self) -> Reg {
        self.alloc_reg()
    }

    fn emit(&mut self, i: Instr) {
        self.code.push(i);
    }
    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }
    fn pop_scope(&mut self) {
        let scope = self.scopes.pop().unwrap();
        // Free the registers the scope's locals used (block-local reuse).
        self.next_reg -= scope.len() as Reg;
    }

    fn declare_local(&mut self, name: &str) -> Reg {
        let r = self.alloc_reg();
        self.scopes.last_mut().unwrap().push((name.to_string(), r));
        // Box the local into a cell if a nested function captures it, so the
        // closure and this scope share one mutable slot.
        if self.captured.contains(name) {
            self.emit(Instr::MakeCell { reg: r });
            self.cell_regs.insert(r);
        }
        r
    }

    /// Like `declare_local` but never emits `MakeCell`. For bindings whose
    /// value is deposited into the register by the runtime (a `catch` param),
    /// where boxing must happen AFTER the value is present.
    fn declare_local_no_box(&mut self, name: &str) -> Reg {
        let r = self.alloc_reg();
        self.scopes.last_mut().unwrap().push((name.to_string(), r));
        r
    }

    /// Resolve a name to a local register (plain or cell-backed), an upvalue, or
    /// a global slot. Upvalue resolution lazily threads captures up the chain.
    fn resolve(&mut self, name: &str) -> Binding {
        for scope in self.scopes.iter().rev() {
            for (n, r) in scope.iter().rev() {
                if n == name {
                    return if self.cell_regs.contains(r) {
                        Binding::LocalCell(*r)
                    } else {
                        Binding::Local(*r)
                    };
                }
            }
        }
        // A free variable that resolves in an enclosing function is an upvalue.
        if let Some(idx) = self.resolve_upvalue(name) {
            return Binding::Upvalue(idx);
        }
        if let Some(i) = self.cx.globals.iter().position(|g| g == name) {
            return Binding::Global(i as u32);
        }
        // Unknown name → treat as a global (read yields undefined; matches JS
        // for declared-later globals; genuine ReferenceErrors are out of v1
        // scope). Reserve a slot so writes/reads are consistent.
        let slot = self.cx.global_slot(name);
        Binding::Global(slot as u32)
    }

    fn add_const(&mut self, v: Value) -> u32 {
        let i = self.constants.len() as u32;
        self.constants.push(v);
        i
    }
    /// Intern a string LITERAL and return its CONSTANT-POOL index (for
    /// `LoadConst`). The VM interns the pending-string Value on first load.
    fn add_string_const(&mut self, s: &str) -> u32 {
        let si = self.string_constants.len() as u32;
        self.string_constants.push(s.to_string());
        // Encode as a "pending string" heap Value the VM interns on first load.
        let v = Value::heap(STRING_CONST_BIT | si);
        self.add_const(v)
    }

    /// Intern a property/method NAME and return its `string_constants` INDEX —
    /// which is what `GetProp`/`SetProp`/`CallMethod` use to look the name up at
    /// runtime. This must NOT be `add_string_const`'s value: that returns the
    /// constant-POOL index, which diverges from the string_constants index as
    /// soon as any non-string constant is added (e.g. a numeric literal), making
    /// `string_constants[name]` go out of bounds (e.g. `(3.5).toFixed(2)`).
    fn string_name(&mut self, s: &str) -> u32 {
        let si = self.string_constants.len() as u32;
        self.string_constants.push(s.to_string());
        si
    }

    // ── statements ──
    fn stmt(&mut self, s: &ox::Statement) -> R<()> {
        use ox::Statement as S;
        match s {
            S::ExpressionStatement(e) => {
                let r = self.expr(&e.expression)?;
                let _ = r; // value discarded
            }
            S::VariableDeclaration(d) => self.var_decl(d)?,
            S::BlockStatement(b) => {
                self.push_scope();
                for st in &b.body {
                    self.stmt(st)?;
                }
                self.pop_scope();
            }
            S::IfStatement(i) => self.if_stmt(i)?,
            S::WhileStatement(w) => self.while_stmt(w)?,
            S::DoWhileStatement(d) => self.do_while_statement(d)?,
            S::ForStatement(f) => self.for_stmt(f)?,
            S::ForOfStatement(f) => self.for_of_statement(f)?,
            S::ForInStatement(f) => self.for_in_statement(f)?,
            S::ReturnStatement(r) => {
                if let Some(arg) = &r.argument {
                    let v = self.expr(arg)?;
                    self.emit(Instr::Return { src: v });
                } else {
                    self.emit(Instr::ReturnUndefined);
                }
            }
            S::FunctionDeclaration(f) => self.func_decl(f)?,
            S::ThrowStatement(t) => {
                let v = self.expr(&t.argument)?;
                self.emit(Instr::Throw { src: v });
            }
            S::TryStatement(t) => self.try_statement(t)?,
            S::EmptyStatement(_) => {}
            _ => return Err("unsupported statement (not in the zipp-vm v1 subset yet)".into()),
        }
        Ok(())
    }

    fn var_decl(&mut self, d: &ox::VariableDeclaration) -> R<()> {
        for decl in &d.declarations {
            let name = match &decl.id {
                ox::BindingPattern::BindingIdentifier(id) => id.name.as_str(),
                _ => return Err("destructuring is not in the zipp-vm v1 subset yet".into()),
            };

            // Top-level `let`/`const`/`var` bind to GLOBAL slots, so that a
            // nested function referencing them resolves via LoadGlobal (a
            // top-level binding is never an upvalue). This keeps the capture
            // machinery confined to genuinely nested scopes.
            if self.is_script {
                let slot = self.cx.global_slot(name) as u32;
                let tmp = self.temp();
                let v = if let Some(init) = &decl.init {
                    self.expr_into(init, tmp)?
                } else {
                    self.emit(Instr::LoadUndefined { dst: tmp });
                    tmp
                };
                self.emit(Instr::StoreGlobal { idx: slot, src: v });
                self.next_reg -= 1; // reclaim tmp
                continue;
            }

            // Allocate the local FIRST so `let x = x`-style self-reference and
            // ordinary declarations land in a stable register. declare_local
            // boxes the register into a cell if a nested function captures it.
            let reg = self.declare_local(name);
            let is_cell = self.cell_regs.contains(&reg);
            if let Some(init) = &decl.init {
                if is_cell {
                    // The init value must be written THROUGH the cell.
                    let tmp = self.temp();
                    let v = self.expr_into(init, tmp)?;
                    self.emit(Instr::CellSet { cell: reg, src: v });
                    self.next_reg -= 1; // reclaim tmp
                } else {
                    let v = self.expr_into(init, reg)?;
                    if v != reg {
                        self.emit(Instr::Move { dst: reg, src: v });
                    }
                }
            } else if !is_cell {
                self.emit(Instr::LoadUndefined { dst: reg });
            }
            // A captured local with no initializer keeps the cell's default
            // (undefined), set when MakeCell boxed the freshly-undefined reg.
        }
        Ok(())
    }

    fn func_decl(&mut self, f: &ox::Function) -> R<()> {
        let name = f.id.as_ref().map(|i| i.name.to_string());
        let (params, body) = function_parts(f)?;
        let captured = capture::captured_locals(&params, body);
        let enclosing = self.child_enclosing();
        let mut proto = self.cx.compile_function_body(
            name.as_deref(),
            &params,
            body,
            false,
            captured,
            enclosing,
        )?;
        let id = self.cx.functions.len() as u32;
        let has_upvalues = !proto.upvalues.is_empty();
        if self.is_script {
            // Top-level: bind the name to a global; the VM materialises the
            // function object at startup. A top-level function's free vars are
            // all globals, so it never captures — no MakeClosure needed.
            if let Some(n) = &name {
                let slot = self.cx.global_slot(n);
                proto.name_global = Some(slot);
            }
            self.cx.functions.push(proto);
        } else {
            // Nested: create the function object now into the local the hoisting
            // pre-pass reserved for this name. If it captures, build a closure;
            // otherwise a plain function object. The name's own binding may be a
            // plain register or a cell (when a sibling/inner function captures
            // this function name).
            self.cx.functions.push(proto);
            if let Some(n) = &name {
                match self.resolve(n) {
                    Binding::Local(reg) => self.emit_make_callable(reg, id, has_upvalues),
                    Binding::LocalCell(cell) => {
                        let tmp = self.temp();
                        self.emit_make_callable(tmp, id, has_upvalues);
                        self.emit(Instr::CellSet { cell, src: tmp });
                        self.next_reg -= 1;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// The enclosing-function chain to hand a function nested in THIS one: our
    /// own enclosing chain plus a snapshot of our current bindings.
    fn child_enclosing(&self) -> Vec<EnclosingFn> {
        let mut e = self.enclosing.clone();
        e.push(self.snapshot());
        e
    }

    /// Compile a function expression, returning `(func_id, has_upvalues)`. The
    /// name (if any) is not hoisted — the value is produced explicitly by a
    /// `MakeFunc`/`MakeClosure` at the use site.
    fn compile_func_expr(&mut self, name: Option<String>, f: &ox::Function) -> R<(u32, bool)> {
        let (params, body) = function_parts(f)?;
        let captured = capture::captured_locals(&params, body);
        let enclosing = self.child_enclosing();
        let proto = self.cx.compile_function_body(
            name.as_deref(),
            &params,
            body,
            false,
            captured,
            enclosing,
        )?;
        let has_upvalues = !proto.upvalues.is_empty();
        let id = self.cx.functions.len() as u32;
        self.cx.functions.push(proto);
        Ok((id, has_upvalues))
    }

    /// Compile an arrow function, returning `(func_id, has_upvalues)`. An
    /// expression-bodied arrow (`x => x + 1`) is a function whose single
    /// statement returns the expression.
    fn compile_arrow(&mut self, a: &ox::ArrowFunctionExpression) -> R<(u32, bool)> {
        let mut params = Vec::new();
        for item in &a.params.items {
            match &item.pattern {
                ox::BindingPattern::BindingIdentifier(id) => params.push(id.name.to_string()),
                _ => return Err("arrow parameter patterns not in the zipp-vm subset yet".into()),
            }
        }
        let captured = capture::captured_locals(&params, &a.body.statements);
        let enclosing = self.child_enclosing();
        let proto = self.cx.compile_arrow_body(&params, a, captured, enclosing)?;
        let has_upvalues = !proto.upvalues.is_empty();
        let id = self.cx.functions.len() as u32;
        self.cx.functions.push(proto);
        Ok((id, has_upvalues))
    }

    /// Emit `MakeClosure` if the just-compiled function captures upvalues, else
    /// `MakeFunc`.
    fn emit_make_callable(&mut self, dst: Reg, id: u32, has_upvalues: bool) {
        if has_upvalues {
            self.emit(Instr::MakeClosure { dst, func_id: id });
        } else {
            self.emit(Instr::MakeFunc { dst, func_id: id });
        }
    }

    fn if_stmt(&mut self, i: &ox::IfStatement) -> R<()> {
        let cond = self.expr(&i.test)?;
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 }); // patched
        self.stmt(&i.consequent)?;
        if let Some(alt) = &i.alternate {
            let jmp = self.here();
            self.emit(Instr::Jump { target: 0 }); // patched
            let else_start = self.here();
            self.patch_jump(jf, else_start);
            self.stmt(alt)?;
            let end = self.here();
            self.patch_jump(jmp, end);
        } else {
            let end = self.here();
            self.patch_jump(jf, end);
        }
        Ok(())
    }

    fn while_stmt(&mut self, w: &ox::WhileStatement) -> R<()> {
        let top = self.here();
        let cond = self.expr(&w.test)?;
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        self.stmt(&w.body)?;
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        Ok(())
    }

    fn for_stmt(&mut self, f: &ox::ForStatement) -> R<()> {
        self.push_scope();
        // init
        if let Some(init) = &f.init {
            match init {
                ox::ForStatementInit::VariableDeclaration(d) => self.var_decl(d)?,
                other => {
                    let e = other
                        .as_expression()
                        .ok_or("unsupported for-init")?;
                    self.expr(e)?;
                }
            }
        }
        let top = self.here();
        let jf = match &f.test {
            Some(t) => {
                let cond = self.expr(t)?;
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond, target: 0 });
                Some(j)
            }
            None => None,
        };
        self.stmt(&f.body)?;
        if let Some(update) = &f.update {
            self.expr(update)?;
        }
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        if let Some(j) = jf {
            self.patch_jump(j, end);
        }
        self.pop_scope();
        Ok(())
    }

    /// `try { … } catch (e) { … } finally { … }`.
    ///
    /// Codegen: `PushHandler(catch, e_reg)` ; try-body ; `PopHandler` ; jump
    /// past catch. The catch block lands the thrown value in `e_reg` and runs.
    /// A `finally` block is emitted inline on the normal-completion path after
    /// both try and catch (covers `try/finally` and `try/catch/finally` for code
    /// that completes or is caught locally). NOTE/LIMITATION: a `finally` does
    /// NOT yet run when an exception propagates THROUGH this frame uncaught, nor
    /// on `return` inside try — documented; full finally semantics is a later
    /// refinement.
    fn try_statement(&mut self, t: &ox::TryStatement) -> R<()> {
        let has_catch = t.handler.is_some();

        let push_at = if has_catch {
            let at = self.here();
            // catch_reg/target patched once known.
            self.emit(Instr::PushHandler { catch_target: 0, catch_reg: 0 });
            Some(at)
        } else {
            None
        };

        // Try block.
        self.push_scope();
        for s in &t.block.body {
            self.stmt(s)?;
        }
        self.pop_scope();

        if let (Some(push_at), Some(handler)) = (push_at, &t.handler) {
            // Normal completion of the try: pop the handler, skip the catch.
            self.emit(Instr::PopHandler);
            let skip = self.here();
            self.emit(Instr::Jump { target: 0 });

            // Catch block: the VM lands here with the thrown value already in
            // the catch register. Reserve that register WITHOUT auto-boxing
            // (the value is deposited by the unwind, not by a MakeCell), then
            // box it explicitly if a nested function captures the binding — at
            // which point the value is already present, so MakeCell wraps it.
            let catch_start = self.here();
            self.push_scope();
            let (e_reg, e_name) = match &handler.param {
                Some(p) => match &p.pattern {
                    ox::BindingPattern::BindingIdentifier(id) => {
                        (self.declare_local_no_box(id.name.as_str()), Some(id.name.to_string()))
                    }
                    _ => return Err("catch destructuring not in the zipp-vm subset yet".into()),
                },
                None => (self.declare_local_no_box("<catch.ignored>"), None),
            };
            if let Instr::PushHandler { catch_target, catch_reg } = &mut self.code[push_at as usize] {
                *catch_target = catch_start;
                *catch_reg = e_reg;
            }
            // If the catch binding is captured, box the now-present value.
            if let Some(n) = &e_name {
                if self.captured.contains(n) {
                    self.emit(Instr::MakeCell { reg: e_reg });
                    self.cell_regs.insert(e_reg);
                }
            }
            for s in &handler.body.body {
                self.stmt(s)?;
            }
            self.pop_scope();

            let after = self.here();
            self.patch_jump(skip, after);
        }

        // finally (normal-completion path only — see limitation note).
        if let Some(finalizer) = &t.finalizer {
            self.push_scope();
            for s in &finalizer.body {
                self.stmt(s)?;
            }
            self.pop_scope();
        }
        Ok(())
    }

    fn do_while_statement(&mut self, d: &ox::DoWhileStatement) -> R<()> {
        let top = self.here();
        self.stmt(&d.body)?;
        let cond = self.expr(&d.test)?;
        // Loop back to top while the condition is truthy.
        self.emit(Instr::JumpIfTrue { cond, target: top });
        Ok(())
    }

    /// `for (const x of iter) body` — desugars to an index loop over an
    /// array/string: `let i=0; while (i < len(iter)) { x = iter[i]; body; i++ }`.
    /// (Generic iterables/iterators are not in the subset; arrays and strings
    /// cover the corpus and common code.)
    fn for_of_statement(&mut self, f: &ox::ForOfStatement) -> R<()> {
        self.push_scope();
        let var_name = for_left_name(&f.left)?;

        // Evaluate the iterable into a stable scratch local.
        let iter_reg = self.declare_local("<forof.iter>");
        let v = self.expr_into(&f.right, iter_reg)?;
        if v != iter_reg {
            self.emit(Instr::Move { dst: iter_reg, src: v });
        }
        // Length and index counter.
        let len_reg = self.declare_local("<forof.len>");
        self.emit(Instr::LenOf { dst: len_reg, obj: iter_reg });
        let idx_reg = self.declare_local("<forof.idx>");
        self.emit(Instr::LoadInt { dst: idx_reg, val: 0 });

        // The loop variable binding (may be cell-boxed if captured).
        let var_reg = self.declare_local(&var_name);
        let var_is_cell = self.cell_regs.contains(&var_reg);

        let top = self.here();
        // while (idx < len)
        let cond = self.temp();
        self.emit(Instr::Lt { dst: cond, a: idx_reg, b: len_reg });
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        self.next_reg -= 1; // reclaim cond temp

        // var = iter[idx]
        if var_is_cell {
            let tmp = self.temp();
            self.emit(Instr::GetIndex { dst: tmp, obj: iter_reg, key: idx_reg });
            self.emit(Instr::CellSet { cell: var_reg, src: tmp });
            self.next_reg -= 1;
        } else {
            self.emit(Instr::GetIndex { dst: var_reg, obj: iter_reg, key: idx_reg });
        }

        self.stmt(&f.body)?;
        self.emit(Instr::AddInt { dst: idx_reg, a: idx_reg, imm: 1 });
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        self.pop_scope();
        Ok(())
    }

    /// `for (const k in obj) body` — iterate the object's own enumerable string
    /// keys (or an array's index strings), via the ObjectKeys op + an index loop.
    fn for_in_statement(&mut self, f: &ox::ForInStatement) -> R<()> {
        self.push_scope();
        let var_name = for_left_name(&f.left)?;

        let obj_reg = self.declare_local("<forin.obj>");
        let v = self.expr_into(&f.right, obj_reg)?;
        if v != obj_reg {
            self.emit(Instr::Move { dst: obj_reg, src: v });
        }
        let keys_reg = self.declare_local("<forin.keys>");
        self.emit(Instr::ObjectKeys { dst: keys_reg, obj: obj_reg });
        let len_reg = self.declare_local("<forin.len>");
        self.emit(Instr::LenOf { dst: len_reg, obj: keys_reg });
        let idx_reg = self.declare_local("<forin.idx>");
        self.emit(Instr::LoadInt { dst: idx_reg, val: 0 });

        let var_reg = self.declare_local(&var_name);
        let var_is_cell = self.cell_regs.contains(&var_reg);

        let top = self.here();
        let cond = self.temp();
        self.emit(Instr::Lt { dst: cond, a: idx_reg, b: len_reg });
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        self.next_reg -= 1;

        if var_is_cell {
            let tmp = self.temp();
            self.emit(Instr::GetIndex { dst: tmp, obj: keys_reg, key: idx_reg });
            self.emit(Instr::CellSet { cell: var_reg, src: tmp });
            self.next_reg -= 1;
        } else {
            self.emit(Instr::GetIndex { dst: var_reg, obj: keys_reg, key: idx_reg });
        }

        self.stmt(&f.body)?;
        self.emit(Instr::AddInt { dst: idx_reg, a: idx_reg, imm: 1 });
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        self.pop_scope();
        Ok(())
    }

    fn patch_jump(&mut self, at: u32, target: u32) {
        match &mut self.code[at as usize] {
            Instr::Jump { target: t }
            | Instr::JumpIfFalse { target: t, .. }
            | Instr::JumpIfTrue { target: t, .. } => *t = target,
            _ => panic!("patch_jump on non-jump"),
        }
    }

    // ── expressions ──
    /// Compile `e`, returning the register holding its value.
    fn expr(&mut self, e: &ox::Expression) -> R<Reg> {
        let dst = self.temp();
        self.expr_into(e, dst)
    }

    /// Compile `e` placing its value into `dst` (or another register it already
    /// occupies, which the caller may use directly). Returns the register that
    /// actually holds the result.
    fn expr_into(&mut self, e: &ox::Expression, dst: Reg) -> R<Reg> {
        use ox::Expression as E;
        match e {
            E::NumericLiteral(n) => {
                self.load_number(dst, n.value);
                Ok(dst)
            }
            E::StringLiteral(s) => {
                let idx = self.add_string_const(s.value.as_str());
                self.emit(Instr::LoadConst { dst, idx });
                Ok(dst)
            }
            E::TemplateLiteral(t) => {
                // Desugar `q0${e0}q1${e1}...qN` to string concatenation
                // q0 + e0 + q1 + e1 + ... + qN. q0 is loaded as a string, so every
                // `+` is a (rope) string concat that coerces each ${e} to a string.
                let q0 = t.quasis[0].value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
                let idx = self.add_string_const(q0);
                self.emit(Instr::LoadConst { dst, idx });
                for (i, e) in t.expressions.iter().enumerate() {
                    let r = self.expr(e)?;
                    self.emit(Instr::Add { dst, a: dst, b: r });
                    if let Some(qe) = t.quasis.get(i + 1) {
                        let q = qe.value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
                        if !q.is_empty() {
                            let qidx = self.add_string_const(q);
                            let qr = self.temp();
                            self.emit(Instr::LoadConst { dst: qr, idx: qidx });
                            self.emit(Instr::Add { dst, a: dst, b: qr });
                        }
                    }
                }
                Ok(dst)
            }
            E::BooleanLiteral(b) => {
                self.emit(Instr::LoadBool { dst, val: b.value });
                Ok(dst)
            }
            E::NullLiteral(_) => {
                self.emit(Instr::LoadNull { dst });
                Ok(dst)
            }
            E::Identifier(id) => {
                // Special global value identifiers that are not user bindings.
                match id.name.as_str() {
                    "undefined" => {
                        self.emit(Instr::LoadUndefined { dst });
                        return Ok(dst);
                    }
                    "NaN" => {
                        let idx = self.add_const(Value::num(f64::NAN));
                        self.emit(Instr::LoadConst { dst, idx });
                        return Ok(dst);
                    }
                    "Infinity" => {
                        let idx = self.add_const(Value::num(f64::INFINITY));
                        self.emit(Instr::LoadConst { dst, idx });
                        return Ok(dst);
                    }
                    _ => {}
                }
                match self.resolve(id.name.as_str()) {
                    Binding::Local(r) => Ok(r), // already in a register
                    Binding::LocalCell(cell) => {
                        self.emit(Instr::CellGet { dst, cell });
                        Ok(dst)
                    }
                    Binding::Upvalue(idx) => {
                        self.emit(Instr::UpvalGet { dst, idx });
                        Ok(dst)
                    }
                    Binding::Global(idx) => {
                        self.emit(Instr::LoadGlobal { dst, idx });
                        Ok(dst)
                    }
                }
            }
            E::ThisExpression(_) => {
                // `this` lives in register 0 of the current function.
                Ok(0)
            }
            E::ParenthesizedExpression(p) => self.expr_into(&p.expression, dst),
            E::BinaryExpression(b) => self.binary(b, dst),
            E::LogicalExpression(l) => self.logical(l, dst),
            E::UnaryExpression(u) => self.unary(u, dst),
            E::UpdateExpression(u) => self.update(u, dst),
            E::AssignmentExpression(a) => self.assign(a, dst),
            E::ConditionalExpression(c) => self.conditional(c, dst),
            E::CallExpression(c) => self.call(c, dst),
            E::NewExpression(n) => {
                // `new Error(msg)` / `new TypeError(msg)` / `new RangeError(msg)`
                // → a plain object {name, message}. Other constructors aren't in
                // the subset yet.
                if let ox::Expression::Identifier(id) = &n.callee {
                    if let Some(kind) = error_ctor(&id.name) {
                        return self.build_error(kind, n.arguments.first(), dst);
                    }
                }
                Err("`new` (non-Error constructor) is not in the zipp-vm subset yet".into())
            }
            E::FunctionExpression(f) => {
                let (id, has_up) =
                    self.compile_func_expr(f.id.as_ref().map(|i| i.name.to_string()), f)?;
                self.emit_make_callable(dst, id, has_up);
                Ok(dst)
            }
            E::ArrowFunctionExpression(a) => {
                let (id, has_up) = self.compile_arrow(a)?;
                self.emit_make_callable(dst, id, has_up);
                Ok(dst)
            }
            E::ArrayExpression(a) => self.array_literal(a, dst),
            E::ObjectExpression(o) => self.object_literal(o, dst),
            E::StaticMemberExpression(m) => {
                // Math constants (Math.PI, Math.E, …) — Math has no real global
                // object, so recognise the member-access shape.
                if let ox::Expression::Identifier(o) = &m.object {
                    if o.name == "Math" {
                        let c = match m.property.name.as_str() {
                            "PI" => Some(std::f64::consts::PI),
                            "E" => Some(std::f64::consts::E),
                            "LN2" => Some(std::f64::consts::LN_2),
                            "LN10" => Some(std::f64::consts::LN_10),
                            "LOG2E" => Some(std::f64::consts::LOG2_E),
                            "LOG10E" => Some(std::f64::consts::LOG10_E),
                            "SQRT2" => Some(std::f64::consts::SQRT_2),
                            "SQRT1_2" => Some(std::f64::consts::FRAC_1_SQRT_2),
                            _ => None,
                        };
                        if let Some(v) = c {
                            self.load_number(dst, v);
                            return Ok(dst);
                        }
                    }
                }
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                self.emit(Instr::GetProp { dst, obj, name });
                Ok(dst)
            }
            E::ComputedMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                self.emit(Instr::GetIndex { dst, obj, key });
                Ok(dst)
            }
            _ => Err("unsupported expression (not in the zipp-vm v1 subset yet)".into()),
        }
    }

    fn array_literal(&mut self, a: &ox::ArrayExpression, dst: Reg) -> R<Reg> {
        // Elements must occupy a contiguous register run for NewArray. Reserve
        // the block first (same contiguity discipline as call args) so an
        // element expression's scratch temps allocate above the block.
        let count = a.elements.len() as u16;
        let base = self.next_reg;
        for _ in &a.elements {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        for (i, el) in a.elements.iter().enumerate() {
            let slot = base + i as Reg;
            match el {
                ox::ArrayExpressionElement::Elision(_) => {
                    self.emit(Instr::LoadUndefined { dst: slot });
                }
                ox::ArrayExpressionElement::SpreadElement(_) => {
                    return Err("array spread is not in the zipp-vm subset yet".into());
                }
                other => {
                    let e = other.as_expression().ok_or("unsupported array element")?;
                    let v = self.expr_into(e, slot)?;
                    if v != slot {
                        self.emit(Instr::Move { dst: slot, src: v });
                    }
                }
            }
            self.next_reg = block_top;
        }
        self.emit(Instr::NewArray { dst, arg_base: base, argc: count });
        Ok(dst)
    }

    fn object_literal(&mut self, o: &ox::ObjectExpression, dst: Reg) -> R<Reg> {
        self.emit(Instr::NewObject { dst });
        for prop in &o.properties {
            match prop {
                ox::ObjectPropertyKind::ObjectProperty(p) => {
                    // Key: a plain identifier or string/number literal key.
                    let key = match &p.key {
                        ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                        ox::PropertyKey::StringLiteral(s) => s.value.to_string(),
                        ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                        _ => return Err("computed object keys not in the zipp-vm subset yet".into()),
                    };
                    let name = self.string_name(&key);
                    let v = self.expr(&p.value)?;
                    self.emit(Instr::SetProp { obj: dst, name, val: v });
                }
                ox::ObjectPropertyKind::SpreadProperty(_) => {
                    return Err("object spread is not in the zipp-vm subset yet".into());
                }
            }
        }
        Ok(dst)
    }

    fn load_number(&mut self, dst: Reg, n: f64) {
        if n.fract() == 0.0 && n >= i32::MIN as f64 && n <= i32::MAX as f64 {
            self.emit(Instr::LoadInt { dst, val: n as i32 });
        } else {
            let idx = self.add_const(Value::num(n));
            self.emit(Instr::LoadConst { dst, idx });
        }
    }

    fn binary(&mut self, b: &ox::BinaryExpression, dst: Reg) -> R<Reg> {
        use ox::BinaryOperator as Op;
        // `a - <int literal>` and `a + <int literal>` → AddInt fast path, but
        // ONLY when the left operand is provably numeric. `+` is overloaded for
        // string concatenation, so `'n=' + 42` must NOT take the integer path
        // (it would coerce the string to NaN). Subtraction is always numeric,
        // so it's always eligible; addition is eligible only when `left` is a
        // numeric literal or another arithmetic expression we just produced a
        // number from. When unsure, fall through to the generic `Add`, which
        // handles string concatenation correctly.
        if let ox::Expression::NumericLiteral(n) = &b.right {
            let imm_ok = n.value.fract() == 0.0
                && n.value >= i32::MIN as f64
                && n.value <= i32::MAX as f64;
            let eligible = matches!(b.operator, Op::Subtraction)
                || (matches!(b.operator, Op::Addition) && is_numeric_expr(&b.left));
            if imm_ok && eligible {
                let a = self.expr(&b.left)?;
                let mut imm = n.value as i32;
                if matches!(b.operator, Op::Subtraction) {
                    imm = -imm;
                }
                self.emit(Instr::AddInt { dst, a, imm });
                return Ok(dst);
            }
        }
        let a = self.expr(&b.left)?;
        let r = self.expr(&b.right)?;
        let instr = match b.operator {
            Op::Addition => Instr::Add { dst, a, b: r },
            Op::Subtraction => Instr::Sub { dst, a, b: r },
            Op::Multiplication => Instr::Mul { dst, a, b: r },
            Op::Division => Instr::Div { dst, a, b: r },
            Op::Remainder => Instr::Mod { dst, a, b: r },
            Op::LessThan => Instr::Lt { dst, a, b: r },
            Op::LessEqualThan => Instr::Le { dst, a, b: r },
            Op::GreaterThan => Instr::Gt { dst, a, b: r },
            Op::GreaterEqualThan => Instr::Ge { dst, a, b: r },
            Op::StrictEquality => Instr::Eq { dst, a, b: r },
            Op::StrictInequality => Instr::Ne { dst, a, b: r },
            Op::Equality => Instr::LooseEq { dst, a, b: r },
            Op::Inequality => Instr::LooseNe { dst, a, b: r },
            _ => return Err("unsupported binary operator (zipp-vm v1)".into()),
        };
        self.emit(instr);
        Ok(dst)
    }

    fn logical(&mut self, l: &ox::LogicalExpression, dst: Reg) -> R<Reg> {
        use ox::LogicalOperator as Op;
        // `a && b`: eval a into dst; if falsy, short-circuit; else eval b.
        let _a = self.expr_into(&l.left, dst)?;
        if _a != dst {
            self.emit(Instr::Move { dst, src: _a });
        }
        match l.operator {
            Op::And => {
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: dst, target: 0 });
                let b = self.expr_into(&l.right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
            Op::Or => {
                let j = self.here();
                self.emit(Instr::JumpIfTrue { cond: dst, target: 0 });
                let b = self.expr_into(&l.right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
            Op::Coalesce => return Err("?? is not in the zipp-vm v1 subset yet".into()),
        }
        Ok(dst)
    }

    fn unary(&mut self, u: &ox::UnaryExpression, dst: Reg) -> R<Reg> {
        use ox::UnaryOperator as Op;
        match u.operator {
            Op::UnaryNegation => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::Neg { dst, a });
                Ok(dst)
            }
            Op::UnaryPlus => {
                let a = self.expr_into(&u.argument, dst)?;
                Ok(a)
            }
            Op::LogicalNot => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::Not { dst, a });
                Ok(dst)
            }
            Op::Typeof => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::TypeOf { dst, a });
                Ok(dst)
            }
            _ => Err("unsupported unary operator (zipp-vm v1)".into()),
        }
    }

    fn update(&mut self, u: &ox::UpdateExpression, dst: Reg) -> R<Reg> {
        // `x++` / `++x` / `x--` / `--x` on a simple identifier.
        let name = match &u.argument {
            ox::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => id.name.to_string(),
            _ => return Err("update on non-identifier not in zipp-vm v1".into()),
        };
        let binding = self.resolve(&name);
        let delta = match u.operator {
            ox::UpdateOperator::Increment => 1,
            ox::UpdateOperator::Decrement => -1,
        };
        if let Binding::Local(r) = binding {
            // Plain register local: mutate in place.
            if u.prefix {
                self.emit(Instr::AddInt { dst: r, a: r, imm: delta });
                if r != dst {
                    self.emit(Instr::Move { dst, src: r });
                }
            } else {
                self.emit(Instr::Move { dst, src: r }); // yield old value
                self.emit(Instr::AddInt { dst: r, a: r, imm: delta });
            }
            return Ok(dst);
        }
        // Cell / upvalue / global: read into `dst`, compute, store back.
        let cur = self.load_binding(&binding, dst); // == dst
        if u.prefix {
            self.emit(Instr::AddInt { dst: cur, a: cur, imm: delta });
            self.store_binding(&binding, cur);
            Ok(dst) // dst holds the new value
        } else {
            // Keep the old value in `dst`; compute the new value in a temp.
            let tmp = self.temp();
            self.emit(Instr::AddInt { dst: tmp, a: cur, imm: delta });
            self.store_binding(&binding, tmp);
            self.next_reg -= 1; // reclaim tmp
            Ok(dst) // dst still holds the old value
        }
    }

    /// Emit a read of `binding` into `dst`; returns the register holding the
    /// value (the binding's own register for a plain Local, else `dst`).
    fn load_binding(&mut self, binding: &Binding, dst: Reg) -> Reg {
        match binding {
            Binding::Local(r) => *r,
            Binding::LocalCell(cell) => {
                self.emit(Instr::CellGet { dst, cell: *cell });
                dst
            }
            Binding::Upvalue(idx) => {
                self.emit(Instr::UpvalGet { dst, idx: *idx });
                dst
            }
            Binding::Global(idx) => {
                self.emit(Instr::LoadGlobal { dst, idx: *idx });
                dst
            }
        }
    }

    /// Emit a write of `src` to `binding`.
    fn store_binding(&mut self, b: &Binding, src: Reg) {
        match b {
            Binding::Local(r) => {
                if *r != src {
                    self.emit(Instr::Move { dst: *r, src });
                }
            }
            Binding::LocalCell(cell) => self.emit(Instr::CellSet { cell: *cell, src }),
            Binding::Upvalue(idx) => self.emit(Instr::UpvalSet { idx: *idx, src }),
            Binding::Global(idx) => self.emit(Instr::StoreGlobal { idx: *idx, src }),
        }
    }

    fn assign(&mut self, a: &ox::AssignmentExpression, dst: Reg) -> R<Reg> {
        use ox::AssignmentOperator as Op;
        // Member-target assignment: `obj.x = v` / `arr[i] = v`. Only plain
        // `=` is supported for members in this subset.
        match &a.left {
            ox::AssignmentTarget::StaticMemberExpression(m) => {
                if !matches!(a.operator, Op::Assign) {
                    return Err("compound assignment to a property not in the zipp-vm subset yet".into());
                }
                let obj = self.expr(&m.object)?;
                let val = self.expr_into(&a.right, dst)?;
                if val != dst {
                    self.emit(Instr::Move { dst, src: val });
                }
                let name = self.string_name(m.property.name.as_str());
                self.emit(Instr::SetProp { obj, name, val: dst });
                return Ok(dst);
            }
            ox::AssignmentTarget::ComputedMemberExpression(m) => {
                if !matches!(a.operator, Op::Assign) {
                    return Err("compound assignment to an index not in the zipp-vm subset yet".into());
                }
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                let val = self.expr_into(&a.right, dst)?;
                if val != dst {
                    self.emit(Instr::Move { dst, src: val });
                }
                self.emit(Instr::SetIndex { obj, key, val: dst });
                return Ok(dst);
            }
            _ => {}
        }
        let name = match &a.left {
            ox::AssignmentTarget::AssignmentTargetIdentifier(id) => id.name.to_string(),
            _ => return Err("assignment to non-identifier not in zipp-vm v1".into()),
        };
        let binding = self.resolve(&name);
        match a.operator {
            Op::Assign => {
                if let Binding::Local(r) = binding {
                    // Plain local: evaluate the RHS directly into its register.
                    let v = self.expr_into(&a.right, r)?;
                    if v != r {
                        self.emit(Instr::Move { dst: r, src: v });
                    }
                    if r != dst {
                        self.emit(Instr::Move { dst, src: r });
                    }
                } else {
                    // Cell / upvalue / global: evaluate into dst, then store.
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.store_binding(&binding, dst);
                }
                Ok(dst)
            }
            Op::Addition | Op::Subtraction | Op::Multiplication => {
                if let Binding::Local(r) = binding {
                    // Plain local: compute in place.
                    let rhs = self.expr(&a.right)?;
                    let instr = match a.operator {
                        Op::Addition => Instr::Add { dst: r, a: r, b: rhs },
                        Op::Subtraction => Instr::Sub { dst: r, a: r, b: rhs },
                        Op::Multiplication => Instr::Mul { dst: r, a: r, b: rhs },
                        _ => unreachable!(),
                    };
                    self.emit(instr);
                    if r != dst {
                        self.emit(Instr::Move { dst, src: r });
                    }
                    return Ok(dst);
                }
                // Cell / upvalue / global: load current → dst, compute → dst,
                // store back through the binding.
                let cur = self.load_binding(&binding, dst); // == dst
                let rhs = self.expr(&a.right)?;
                let instr = match a.operator {
                    Op::Addition => Instr::Add { dst, a: cur, b: rhs },
                    Op::Subtraction => Instr::Sub { dst, a: cur, b: rhs },
                    Op::Multiplication => Instr::Mul { dst, a: cur, b: rhs },
                    _ => unreachable!(),
                };
                self.emit(instr);
                self.store_binding(&binding, dst);
                Ok(dst)
            }
            _ => Err("unsupported assignment operator (zipp-vm v1)".into()),
        }
    }

    fn conditional(&mut self, c: &ox::ConditionalExpression, dst: Reg) -> R<Reg> {
        let cond = self.expr(&c.test)?;
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        let t = self.expr_into(&c.consequent, dst)?;
        if t != dst {
            self.emit(Instr::Move { dst, src: t });
        }
        let jmp = self.here();
        self.emit(Instr::Jump { target: 0 });
        let else_start = self.here();
        self.patch_jump(jf, else_start);
        let e = self.expr_into(&c.alternate, dst)?;
        if e != dst {
            self.emit(Instr::Move { dst, src: e });
        }
        let end = self.here();
        self.patch_jump(jmp, end);
        Ok(dst)
    }

    /// Build an Error-like object `{ name, message }` for `Error("msg")` (called
    /// either with `new` or bare). `arg` is the optional message argument.
    fn build_error(&mut self, kind: &str, arg: Option<&ox::Argument>, dst: Reg) -> R<Reg> {
        self.emit(Instr::NewObject { dst });
        // name = kind
        let name_const = self.add_string_const(kind);
        let tmp = self.temp();
        self.emit(Instr::LoadConst { dst: tmp, idx: name_const });
        let name_key = self.string_name("name");
        self.emit(Instr::SetProp { obj: dst, name: name_key, val: tmp });
        // message = arg (coerced to string at use; stored as-is)
        if let Some(a) = arg {
            if let Some(e) = a.as_expression() {
                let mv = self.expr_into(e, tmp)?;
                if mv != tmp {
                    self.emit(Instr::Move { dst: tmp, src: mv });
                }
            } else {
                self.emit(Instr::LoadUndefined { dst: tmp });
            }
        } else {
            let empty = self.add_string_const("");
            self.emit(Instr::LoadConst { dst: tmp, idx: empty });
        }
        let msg_key = self.string_name("message");
        self.emit(Instr::SetProp { obj: dst, name: msg_key, val: tmp });
        self.next_reg -= 1; // reclaim tmp
        Ok(dst)
    }

    fn call(&mut self, c: &ox::CallExpression, dst: Reg) -> R<Reg> {
        // Bare `Error("msg")` call (no `new`) → same Error object.
        if let ox::Expression::Identifier(id) = &c.callee {
            if let Some(kind) = error_ctor(&id.name) {
                return self.build_error(kind, c.arguments.first(), dst);
            }
        }
        // Number(x) / parseInt(s,radix) / parseFloat(s) → GlobalFn op.
        if let ox::Expression::Identifier(id) = &c.callee {
            if let Some(op) = crate::bytecode::GlobalFn::from_name(&id.name) {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::GlobalFn { dst, op, arg_base, argc });
                return Ok(dst);
            }
        }

        // console.log(...) → Print opcode.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "console"
                    && matches!(m.property.name.as_str(), "log" | "info" | "warn" | "error" | "debug")
                {
                    // node routes console.error / console.warn to stderr.
                    let to_stderr = matches!(m.property.name.as_str(), "error" | "warn");
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::Print { arg_base, argc, to_stderr });
                    self.emit(Instr::LoadUndefined { dst });
                    return Ok(dst);
                }
            }
        }

        // Clock idioms: `performance.now()` and `Date.now()` → Now opcode. They
        // have no real global object in the subset, so recognise the call shape.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                let epoch = match (obj.name.as_str(), m.property.name.as_str()) {
                    ("performance", "now") => Some(false),
                    ("Date", "now") => Some(true),
                    _ => None,
                };
                if let Some(epoch) = epoch {
                    self.emit(Instr::Now { dst, epoch });
                    return Ok(dst);
                }
            }
        }

        // `Array.isArray(x)` → IsArray op.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "Array" && m.property.name == "isArray" && c.arguments.len() == 1 {
                    if let Some(arg_expr) = c.arguments[0].as_expression() {
                        let a = self.expr(arg_expr)?;
                        self.emit(Instr::IsArray { dst, a });
                        return Ok(dst);
                    }
                }
            }
        }

        // `Object.keys/values/entries(o)` → dedicated ops (Object has no real
        // global object in the subset).
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "Object" && c.arguments.len() == 1 {
                    let mk = match m.property.name.as_str() {
                        "keys" => Some(0u8),
                        "values" => Some(1u8),
                        "entries" => Some(2u8),
                        _ => None,
                    };
                    if let Some(kind) = mk {
                        if let Some(arg_expr) = c.arguments[0].as_expression() {
                            let o = self.expr(arg_expr)?;
                            self.emit(match kind {
                                0 => Instr::ObjectKeys { dst, obj: o },
                                1 => Instr::ObjectValues { dst, obj: o },
                                _ => Instr::ObjectEntries { dst, obj: o },
                            });
                            return Ok(dst);
                        }
                    }
                }
            }
        }

        // `Math.<fn>(args…)` → MathOp. Math has no real global object in the
        // subset, so recognise the call shape (like console.log / Date.now).
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "Math" {
                    if let Some(op) = crate::bytecode::MathFn::from_name(m.property.name.as_str()) {
                        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                        self.emit(Instr::MathOp { dst, op, arg_base, argc });
                        return Ok(dst);
                    }
                }
            }
        }

        // Method call `obj.name(args…)` → CallMethod, binding `this` to obj.
        // (Computed-member calls `obj[k](…)` fall through to the generic path.)
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            let obj = self.expr(&m.object)?;
            let name = self.string_name(m.property.name.as_str());
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::CallMethod { dst, obj, name, arg_base, argc });
            return Ok(dst);
        }

        // General call: evaluate callee, then contiguous args.
        let callee = self.expr(&c.callee)?;
        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
        self.emit(Instr::Call { dst, callee, arg_base, argc });
        Ok(dst)
    }

    /// Evaluate call arguments into a contiguous run of registers and return
    /// (first register, count). The run MUST be contiguous because the
    /// `Call`/`Print`/`NewArray` opcodes address args as
    /// `[arg_base, arg_base+argc)`.
    ///
    /// Correctness subtlety: evaluating one argument may itself allocate scratch
    /// temps (e.g. `a[i]` evaluates `a` and `i` into temps). Those temps must
    /// NOT land inside the still-unfilled argument slots. So we reserve the
    /// whole block first (bumping `next_reg` past it), which forces per-arg
    /// temps to allocate ABOVE the block; we then reclaim them after each arg.
    fn eval_args_contiguous(
        &mut self,
        args: &oxc_allocator::Vec<ox::Argument>,
    ) -> R<(Reg, u16)> {
        let exprs: Vec<&ox::Expression> = args
            .iter()
            .map(|a| {
                a.as_expression()
                    .ok_or_else(|| "spread arguments are not in the zipp-vm subset yet".to_string())
            })
            .collect::<R<Vec<_>>>()?;
        let base = self.eval_contiguous(&exprs)?;
        Ok((base, exprs.len() as u16))
    }

    /// Evaluate `exprs` into the contiguous register block `[base, base+len)`,
    /// reclaiming each expression's scratch temps. Returns `base`.
    fn eval_contiguous(&mut self, exprs: &[&ox::Expression]) -> R<Reg> {
        let base = self.next_reg;
        // Reserve the block up front so arg-evaluation temps allocate above it.
        for _ in exprs {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        for (i, e) in exprs.iter().enumerate() {
            let slot = base + i as Reg;
            let v = self.expr_into(e, slot)?;
            if v != slot {
                self.emit(Instr::Move { dst: slot, src: v });
            }
            // Reclaim temps this argument used (everything above the block).
            self.next_reg = block_top;
        }
        Ok(base)
    }
}

enum Binding {
    /// Plain register-resident local (the fast path; no capture).
    Local(Reg),
    /// Local that has been boxed into a heap cell because a nested function
    /// captures it; the register holds the cell reference.
    LocalCell(Reg),
    /// A variable captured from an enclosing function: index into this
    /// function's upvalue list.
    Upvalue(u16),
    Global(u32),
}

/// Recognise the built-in Error constructor names the subset supports. Returns
/// the canonical `name` to store on the error object.
fn error_ctor(name: &str) -> Option<&'static str> {
    match name {
        "Error" => Some("Error"),
        "TypeError" => Some("TypeError"),
        "RangeError" => Some("RangeError"),
        "SyntaxError" => Some("SyntaxError"),
        _ => None,
    }
}

/// Extract the loop-variable name from a `for-of`/`for-in` left-hand side.
/// Supports `for (let/const/var x of …)` and `for (x of …)`.
fn for_left_name(left: &ox::ForStatementLeft) -> R<String> {
    match left {
        ox::ForStatementLeft::VariableDeclaration(d) => match &d.declarations[0].id {
            ox::BindingPattern::BindingIdentifier(id) => Ok(id.name.to_string()),
            _ => Err("for-of/for-in destructuring not in the zipp-vm subset yet".into()),
        },
        ox::ForStatementLeft::AssignmentTargetIdentifier(id) => Ok(id.name.to_string()),
        _ => Err("for-of/for-in needs a simple variable target".into()),
    }
}

/// Render a numeric object key the way JS does (`{0: 'a'}` has key `"0"`).
fn fmt_key_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Conservative static check: is this expression definitely a number? Used to
/// gate the `+ <int>` fast path (where `+` could otherwise mean string concat).
/// Only returns true for cases that cannot be strings.
fn is_numeric_expr(e: &ox::Expression) -> bool {
    use ox::Expression as E;
    match e {
        E::NumericLiteral(_) => true,
        E::ParenthesizedExpression(p) => is_numeric_expr(&p.expression),
        E::UnaryExpression(u) => matches!(
            u.operator,
            ox::UnaryOperator::UnaryNegation | ox::UnaryOperator::UnaryPlus
        ),
        E::BinaryExpression(b) => matches!(
            b.operator,
            ox::BinaryOperator::Subtraction
                | ox::BinaryOperator::Multiplication
                | ox::BinaryOperator::Division
                | ox::BinaryOperator::Remainder
        ),
        _ => false,
    }
}

/// Extract (param names, body statements) from an oxc function.
fn function_parts<'a>(f: &'a ox::Function) -> R<(Vec<String>, &'a [ox::Statement<'a>])> {
    let mut params = Vec::new();
    for item in &f.params.items {
        match &item.pattern {
            ox::BindingPattern::BindingIdentifier(id) => params.push(id.name.to_string()),
            _ => return Err("parameter patterns are not in the zipp-vm v1 subset yet".into()),
        }
    }
    if f.params.rest.is_some() {
        return Err("rest parameters are not in the zipp-vm v1 subset yet".into());
    }
    let body = match &f.body {
        Some(b) => b.statements.as_slice(),
        None => &[],
    };
    Ok((params, body))
}
