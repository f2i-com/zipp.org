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

use crate::bytecode::{
    BitwiseOp, ClassDef, FuncProto, InstanceCtor, Instr, Program, Reg, UpvalSource,
};
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
        classes: c.classes,
    })
}

struct Compiler {
    functions: Vec<FuncProto>,
    /// Global name → slot.
    globals: Vec<String>,
    /// Compiled class descriptors, indexed by the `MakeClass` class_id.
    classes: Vec<ClassDef>,
}

impl Compiler {
    fn new() -> Compiler {
        Compiler { functions: Vec::new(), globals: Vec::new(), classes: Vec::new() }
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

        // Pass 1: hoist top-level function (and class) declaration names to globals.
        for s in &prog.body {
            match s {
                ox::Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        self.global_slot(id.name.as_str());
                    }
                }
                ox::Statement::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        self.global_slot(id.name.as_str());
                    }
                }
                _ => {}
            }
        }

        // Compile the top-level body as function 0. The script has no enclosing
        // scope and binds everything to globals, so nothing it declares is a
        // captured cell.
        let top = self.compile_function_body(
            None,
            &[],
            None,
            None,
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
        rest: Option<&str>,
        params_ast: Option<&ox::FormalParameters>,
        body: &[ox::Statement],
        is_script: bool,
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
    ) -> R<FuncProto> {
        let mut fc = FnCompiler::new(self, params, rest, captured, enclosing);
        fc.is_script = is_script;

        // Apply default parameter values (`function f(x = expr)`) before the body:
        // for each defaulted param, `if (x === undefined) x = expr`.
        if let Some(pa) = params_ast {
            fc.emit_param_defaults(pa)?;
        }

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
            rest_reg: fc.rest_reg,
            constants: fc.constants,
            string_constants: fc.string_constants,
            name_global: None, // set by the caller for top-level declarations
            upvalues,
        })
    }

    /// Compile a class method or constructor. Like `compile_function_body` but
    /// (a) non-capturing (empty enclosing → free vars resolve to globals) and
    /// (b) it first emits instance-field initializers `this.field = expr` (only
    /// for the constructor; `fields` is empty for plain methods). `this` is reg 0.
    fn compile_class_fn(
        &mut self,
        name: &str,
        params: &[String],
        rest: Option<&str>,
        params_ast: Option<&ox::FormalParameters>,
        fields: &[(String, Option<&ox::Expression>)],
        body: &[ox::Statement],
    ) -> R<FuncProto> {
        let mut fc = FnCompiler::new(self, params, rest, HashSet::new(), Vec::new());
        if let Some(pa) = params_ast {
            fc.emit_param_defaults(pa)?;
        }
        // Instance field initializers: `this.field = expr` (this = reg 0).
        for (fname, finit) in fields {
            let save = fc.next_reg;
            let v = match finit {
                Some(e) => fc.expr(e)?,
                None => {
                    let t = fc.temp();
                    fc.emit(Instr::LoadUndefined { dst: t });
                    t
                }
            };
            let name_idx = fc.string_name(fname);
            fc.emit(Instr::SetProp { obj: 0, name: name_idx, val: v });
            fc.next_reg = save;
        }
        for s in body {
            if let ox::Statement::FunctionDeclaration(f) = s {
                if let Some(id) = &f.id {
                    fc.declare_local(id.name.as_str());
                }
            }
        }
        for s in body {
            fc.stmt(s)?;
        }
        fc.emit(Instr::ReturnUndefined);
        let upvalues: Vec<UpvalSource> = fc.upvalues.borrow().iter().map(|(_, s)| *s).collect();
        Ok(FuncProto {
            name: name.to_string(),
            code: fc.code,
            reg_count: fc.max_reg,
            param_count: params.len() as u16,
            rest_reg: fc.rest_reg,
            constants: fc.constants,
            string_constants: fc.string_constants,
            name_global: None,
            upvalues,
        })
    }

    /// Compile an arrow function body (expression- or block-bodied).
    fn compile_arrow_body(
        &mut self,
        params: &[String],
        rest: Option<&str>,
        a: &ox::ArrowFunctionExpression,
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
    ) -> R<FuncProto> {
        let mut fc = FnCompiler::new(self, params, rest, captured, enclosing);
        fc.emit_param_defaults(&a.params)?;
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
            rest_reg: fc.rest_reg,
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
        rest_reg: None,
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
    /// Register holding the rest-parameter array, if this function has one.
    rest_reg: Option<Reg>,
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
    /// Active optional-chain short-circuit targets: a stack (chains can nest),
    /// each entry collecting the ip of every `?.` nullish-bail jump in that chain.
    /// On exit the chain patches them to a "load undefined" block.
    chain_bails: Vec<Vec<u32>>,
    /// Enclosing loop contexts (innermost last) for `break`/`continue`: each
    /// collects the jump ips to patch to the loop's end (break) and continue
    /// point (continue).
    loop_ctx: Vec<LoopCtx>,
}

/// Pending `break`/`continue` jumps for one enclosing breakable construct. A
/// `switch` is a break target but NOT a continue target, so `continue` skips
/// switch frames to the innermost loop (`is_loop`).
struct LoopCtx {
    break_jumps: Vec<u32>,
    continue_jumps: Vec<u32>,
    is_loop: bool,
}

impl LoopCtx {
    fn loop_frame() -> LoopCtx {
        LoopCtx { break_jumps: Vec::new(), continue_jumps: Vec::new(), is_loop: true }
    }
    fn switch_frame() -> LoopCtx {
        LoopCtx { break_jumps: Vec::new(), continue_jumps: Vec::new(), is_loop: false }
    }
}

impl<'a> FnCompiler<'a> {
    fn new(
        cx: &'a mut Compiler,
        params: &[String],
        rest: Option<&str>,
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
            rest_reg: None,
            is_script: false,
            chain_bails: Vec::new(),
            loop_ctx: Vec::new(),
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
        // Rest parameter (`...rest`) takes the slot right after the fixed params;
        // the VM fills it with an array of the overflow args at call setup. As
        // with a fixed param, box it if a nested closure captures it.
        if let Some(name) = rest {
            let r = fc.alloc_reg();
            fc.rest_reg = Some(r);
            fc.scopes[0].push((name.to_string(), r));
            if fc.captured.contains(name) {
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
            S::BreakStatement(b) => {
                if b.label.is_some() {
                    return Err("labeled break is not in the zipp-vm subset yet".into());
                }
                let j = self.here();
                self.emit(Instr::Jump { target: 0 });
                match self.loop_ctx.last_mut() {
                    Some(ctx) => ctx.break_jumps.push(j),
                    None => return Err("`break` outside a loop is not supported".into()),
                }
            }
            S::ContinueStatement(c) => {
                if c.label.is_some() {
                    return Err("labeled continue is not in the zipp-vm subset yet".into());
                }
                let j = self.here();
                self.emit(Instr::Jump { target: 0 });
                // `continue` targets the innermost LOOP, skipping switch frames.
                match self.loop_ctx.iter_mut().rev().find(|c| c.is_loop) {
                    Some(ctx) => ctx.continue_jumps.push(j),
                    None => return Err("`continue` outside a loop is not supported".into()),
                }
            }
            S::SwitchStatement(s) => self.switch_stmt(s)?,
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
            S::ClassDeclaration(c) => self.class_decl(c)?,
            S::EmptyStatement(_) => {}
            _ => return Err("unsupported statement (not in the zipp-vm v1 subset yet)".into()),
        }
        Ok(())
    }

    fn var_decl(&mut self, d: &ox::VariableDeclaration) -> R<()> {
        for decl in &d.declarations {
            // Destructuring declaration (`let {a,b} = o`, `let [x,...r] = arr`):
            // declare every leaf binding, evaluate the initializer once into a
            // scratch register, then extract each target from it.
            if !matches!(decl.id, ox::BindingPattern::BindingIdentifier(_)) {
                let init = decl
                    .init
                    .as_ref()
                    .ok_or("a destructuring declaration requires an initializer")?;
                self.declare_pattern(&decl.id)?;
                let save = self.next_reg;
                let src = self.alloc_reg();
                let sv = self.expr_into(init, src)?;
                if sv != src {
                    self.emit(Instr::Move { dst: src, src: sv });
                }
                self.extract_pattern(&decl.id, src)?;
                self.next_reg = save; // reclaim the source + extraction temps
                continue;
            }
            let name = match &decl.id {
                ox::BindingPattern::BindingIdentifier(id) => id.name.as_str(),
                _ => unreachable!("handled above"),
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

    /// Phase 1 of a destructuring declaration: declare every leaf binding the
    /// pattern introduces, so they occupy stable (low) registers / global slots
    /// and any captured ones are boxed before extraction writes to them.
    fn declare_pattern(&mut self, pat: &ox::BindingPattern) -> R<()> {
        use ox::BindingPattern as P;
        match pat {
            P::BindingIdentifier(id) => {
                if self.is_script {
                    self.cx.global_slot(&id.name);
                } else {
                    self.declare_local(&id.name);
                }
                Ok(())
            }
            P::AssignmentPattern(ap) => self.declare_pattern(&ap.left),
            P::ObjectPattern(op) => {
                if op.rest.is_some() {
                    return Err("object rest in destructuring is not in the zipp-vm subset yet".into());
                }
                for prop in &op.properties {
                    self.declare_pattern(&prop.value)?;
                }
                Ok(())
            }
            P::ArrayPattern(arr) => {
                for el in arr.elements.iter().flatten() {
                    self.declare_pattern(el)?;
                }
                if let Some(rest) = &arr.rest {
                    self.declare_pattern(&rest.argument)?;
                }
                Ok(())
            }
        }
    }

    /// Phase 2: extract values from `src` (the initializer's value) into the
    /// already-declared bindings. Every temp this allocates sits above the
    /// declared locals, so callers reclaim them with a single `next_reg` reset.
    fn extract_pattern(&mut self, pat: &ox::BindingPattern, src: Reg) -> R<()> {
        use ox::BindingPattern as P;
        match pat {
            P::BindingIdentifier(id) => {
                let b = self.resolve(&id.name);
                self.store_binding(&b, src);
                Ok(())
            }
            // `target = default`: `src` is our scratch temp, so patch the default
            // into it in place when it came out undefined, then bind the target.
            P::AssignmentPattern(ap) => {
                self.apply_default_in_place(src, &ap.right)?;
                self.extract_pattern(&ap.left, src)
            }
            P::ObjectPattern(op) => {
                for prop in &op.properties {
                    let save = self.next_reg;
                    let val = self.alloc_reg();
                    self.extract_member(src, &prop.key, prop.computed, val)?;
                    self.extract_pattern(&prop.value, val)?;
                    self.next_reg = save;
                }
                Ok(())
            }
            P::ArrayPattern(arr) => {
                for (i, el) in arr.elements.iter().enumerate() {
                    if let Some(p) = el {
                        let save = self.next_reg;
                        let val = self.alloc_reg();
                        let idx = self.alloc_reg();
                        self.emit(Instr::LoadInt { dst: idx, val: i as i32 });
                        self.emit(Instr::GetIndex { dst: val, obj: src, key: idx });
                        self.extract_pattern(p, val)?;
                        self.next_reg = save;
                    }
                    // a hole (`[, x]`) binds nothing
                }
                if let Some(rest) = &arr.rest {
                    let save = self.next_reg;
                    let val = self.alloc_reg();
                    self.emit(Instr::ArrayRest { dst: val, src, start: arr.elements.len() as u32 });
                    self.extract_pattern(&rest.argument, val)?;
                    self.next_reg = save;
                }
                Ok(())
            }
        }
    }

    /// Read `obj[key]` into `dst` for a destructuring property. A static key
    /// (identifier / string / number) uses GetProp; a computed `[expr]` key is
    /// evaluated and read with GetIndex.
    fn extract_member(
        &mut self,
        obj: Reg,
        key: &ox::PropertyKey,
        computed: bool,
        dst: Reg,
    ) -> R<()> {
        if computed {
            let e = key
                .as_expression()
                .ok_or("unsupported computed destructuring key")?;
            let save = self.next_reg; // `dst` was allocated below this
            let k = self.expr(e)?;
            self.emit(Instr::GetIndex { dst, obj, key: k });
            self.next_reg = save; // reclaim the key-expression temps
            return Ok(());
        }
        let name = match key {
            ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
            ox::PropertyKey::StringLiteral(s) => s.value.to_string(),
            ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
            _ => return Err("unsupported destructuring property key".into()),
        };
        let nidx = self.string_name(&name);
        self.emit(Instr::GetProp { dst, obj, name: nidx });
        Ok(())
    }

    /// `if (reg === undefined) reg = default` — apply a destructuring/parameter
    /// default to a scratch register in place.
    fn apply_default_in_place(&mut self, reg: Reg, default: &ox::Expression) -> R<()> {
        let save = self.next_reg;
        let undef = self.alloc_reg();
        self.emit(Instr::LoadUndefined { dst: undef });
        let cond = self.alloc_reg();
        self.emit(Instr::Eq { dst: cond, a: reg, b: undef });
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 }); // skip default when defined
        let dv = self.expr_into(default, reg)?;
        if dv != reg {
            self.emit(Instr::Move { dst: reg, src: dv });
        }
        let end = self.here();
        self.patch_jump(jf, end);
        self.next_reg = save;
        Ok(())
    }

    fn func_decl(&mut self, f: &ox::Function) -> R<()> {
        let name = f.id.as_ref().map(|i| i.name.to_string());
        let (params, rest, body) = function_parts(f)?;
        let captured = capture::captured_locals(&with_rest(&params, &rest), body);
        let enclosing = self.child_enclosing();
        let mut proto = self.cx.compile_function_body(
            name.as_deref(),
            &params,
            rest.as_deref(),
            Some(&f.params),
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

    /// Compile a `class C { … }` declaration: build the method + constructor
    /// protos, register a ClassDef, and bind `C` to the materialized class value.
    fn class_decl(&mut self, class: &ox::Class) -> R<()> {
        let class_id = self.compile_class(class)?;
        let name = class.id.as_ref().map(|i| i.name.to_string());
        let Some(n) = name else { return Ok(()) };
        if self.is_script {
            let slot = self.cx.global_slot(&n) as u32;
            let tmp = self.temp();
            self.emit(Instr::MakeClass { dst: tmp, class_id });
            self.emit(Instr::StoreGlobal { idx: slot, src: tmp });
            self.next_reg -= 1;
        } else {
            let reg = self.declare_local(&n);
            if self.cell_regs.contains(&reg) {
                let tmp = self.temp();
                self.emit(Instr::MakeClass { dst: tmp, class_id });
                self.emit(Instr::CellSet { cell: reg, src: tmp });
                self.next_reg -= 1;
            } else {
                self.emit(Instr::MakeClass { dst: reg, class_id });
            }
        }
        Ok(())
    }

    /// Compile a class body into protos (methods get `this` at reg 0; the
    /// constructor proto runs instance-field initializers then the user ctor
    /// body) and register a ClassDef. Returns its class_id. Methods are compiled
    /// as non-capturing functions (free vars resolve to globals), so a class at
    /// module scope works fully; `extends`/`super`, static members, and
    /// get/set accessors are out of this subset.
    fn compile_class(&mut self, class: &ox::Class) -> R<u32> {
        if class.super_class.is_some() {
            return Err("class extends/super is not in the zipp-vm subset yet".into());
        }
        let cname = class.id.as_ref().map(|i| i.name.to_string()).unwrap_or_else(|| "<class>".into());
        let mut ctor_fn: Option<&ox::Function> = None;
        let mut methods: Vec<(String, &ox::Function)> = Vec::new();
        let mut getters: Vec<(String, &ox::Function)> = Vec::new();
        let mut fields: Vec<(String, Option<&ox::Expression>)> = Vec::new();
        for el in &class.body.body {
            match el {
                ox::ClassElement::MethodDefinition(m) => {
                    if m.r#static {
                        return Err("static class members are not in the zipp-vm subset yet".into());
                    }
                    match m.kind {
                        ox::MethodDefinitionKind::Constructor => ctor_fn = Some(&m.value),
                        ox::MethodDefinitionKind::Method => {
                            methods.push((class_key_name(&m.key)?, &m.value));
                        }
                        ox::MethodDefinitionKind::Get => {
                            getters.push((class_key_name(&m.key)?, &m.value));
                        }
                        ox::MethodDefinitionKind::Set => {
                            return Err("class set accessors are not in the zipp-vm subset yet".into());
                        }
                    }
                }
                ox::ClassElement::PropertyDefinition(p) => {
                    if p.r#static {
                        return Err("static class members are not in the zipp-vm subset yet".into());
                    }
                    fields.push((class_key_name(&p.key)?, p.value.as_ref()));
                }
                ox::ClassElement::StaticBlock(_) => {
                    return Err("static blocks are not in the zipp-vm subset yet".into());
                }
                _ => return Err("unsupported class member in the zipp-vm subset".into()),
            }
        }
        // Method protos.
        let mut method_defs: Vec<(String, u32)> = Vec::new();
        for (mname, func) in &methods {
            let (params, rest, body) = function_parts(func)?;
            let proto = self.cx.compile_class_fn(
                &format!("{cname}.{mname}"),
                &params,
                rest.as_deref(),
                Some(&*func.params),
                &[],
                body,
            )?;
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            method_defs.push((mname.clone(), fid));
        }
        // Getter protos (compiled identically to a no-arg method).
        let mut getter_defs: Vec<(String, u32)> = Vec::new();
        for (gname, func) in &getters {
            let (params, rest, body) = function_parts(func)?;
            let proto = self.cx.compile_class_fn(
                &format!("{cname}.get {gname}"),
                &params,
                rest.as_deref(),
                Some(&*func.params),
                &[],
                body,
            )?;
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            getter_defs.push((gname.clone(), fid));
        }
        // Constructor proto (only needed when there's a ctor or any field init).
        let ctor = if ctor_fn.is_some() || !fields.is_empty() {
            let (params, rest, body) = match ctor_fn {
                Some(f) => function_parts(f)?,
                None => (Vec::new(), None, &[][..]),
            };
            let params_ast = ctor_fn.map(|f| &*f.params);
            let proto = self.cx.compile_class_fn(
                &format!("{cname}.constructor"),
                &params,
                rest.as_deref(),
                params_ast,
                &fields,
                body,
            )?;
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            Some(fid)
        } else {
            None
        };
        let class_id = self.cx.classes.len() as u32;
        self.cx.classes.push(ClassDef {
            name: cname,
            ctor,
            methods: method_defs,
            getters: getter_defs,
        });
        Ok(class_id)
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
        let (params, rest, body) = function_parts(f)?;
        let captured = capture::captured_locals(&with_rest(&params, &rest), body);
        let enclosing = self.child_enclosing();
        let proto = self.cx.compile_function_body(
            name.as_deref(),
            &params,
            rest.as_deref(),
            Some(&f.params),
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
                ox::BindingPattern::AssignmentPattern(ap) => match &ap.left {
                    ox::BindingPattern::BindingIdentifier(id) => params.push(id.name.to_string()),
                    _ => return Err("destructuring parameters are not in the zipp-vm subset yet".into()),
                },
                _ => return Err("arrow parameter patterns not in the zipp-vm subset yet".into()),
            }
        }
        let rest = rest_name(&a.params)?;
        let captured = capture::captured_locals(&with_rest(&params, &rest), &a.body.statements);
        let enclosing = self.child_enclosing();
        let proto = self.cx.compile_arrow_body(&params, rest.as_deref(), a, captured, enclosing)?;
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
        self.loop_ctx.push(LoopCtx::loop_frame());
        self.stmt(&w.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        for c in ctx.continue_jumps {
            self.patch_jump(c, top); // continue → re-test
        }
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
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
        self.loop_ctx.push(LoopCtx::loop_frame());
        self.stmt(&f.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → run the update, then re-test
        }
        if let Some(update) = &f.update {
            self.expr(update)?;
        }
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        if let Some(j) = jf {
            self.patch_jump(j, end);
        }
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
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
        self.loop_ctx.push(LoopCtx::loop_frame());
        self.stmt(&d.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → re-evaluate the condition
        }
        let cond = self.expr(&d.test)?;
        // Loop back to top while the condition is truthy.
        self.emit(Instr::JumpIfTrue { cond, target: top });
        let end = self.here();
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        Ok(())
    }

    /// `switch (disc) { case A: …; default: …; … }`. Two passes: emit the
    /// `disc === testᵢ` comparison jumps in order (and remember `default`), then
    /// emit the case bodies consecutively so fall-through is natural; `break`
    /// (collected in a non-loop frame) jumps to the end.
    fn switch_stmt(&mut self, s: &ox::SwitchStatement) -> R<()> {
        self.push_scope();
        let disc = self.expr(&s.discriminant)?;

        // Pass 1: comparison jumps (strict `===`, like JS). `default` is recorded
        // and dispatched after the others fail.
        let mut case_jumps: Vec<(usize, u32)> = Vec::new();
        let mut default_index: Option<usize> = None;
        for (i, c) in s.cases.iter().enumerate() {
            match &c.test {
                Some(t) => {
                    let save = self.next_reg;
                    let tv = self.expr(t)?;
                    let cond = self.temp();
                    self.emit(Instr::Eq { dst: cond, a: disc, b: tv });
                    let j = self.here();
                    self.emit(Instr::JumpIfTrue { cond, target: 0 });
                    self.next_reg = save; // reclaim test/cond temps (disc survives)
                    case_jumps.push((i, j));
                }
                None => default_index = Some(i),
            }
        }
        // No case matched → default body (if present) else the end.
        let dispatch_default = self.here();
        self.emit(Instr::Jump { target: 0 });

        // Pass 2: case bodies, in source order (fall-through is natural).
        self.loop_ctx.push(LoopCtx::switch_frame());
        let mut body_start: Vec<u32> = Vec::with_capacity(s.cases.len());
        for c in &s.cases {
            body_start.push(self.here());
            for st in &c.consequent {
                self.stmt(st)?;
            }
        }
        let ctx = self.loop_ctx.pop().unwrap();
        let end = self.here();

        for (i, j) in case_jumps {
            self.patch_jump(j, body_start[i]);
        }
        let default_target = default_index.map(|i| body_start[i]).unwrap_or(end);
        self.patch_jump(dispatch_default, default_target);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        self.pop_scope();
        Ok(())
    }

    /// `for (const x of iter) body` — desugars to an index loop over an
    /// array/string: `let i=0; while (i < len(iter)) { x = iter[i]; body; i++ }`.
    /// (Generic iterables/iterators are not in the subset; arrays and strings
    /// cover the corpus and common code.)
    fn for_of_statement(&mut self, f: &ox::ForOfStatement) -> R<()> {
        self.push_scope();

        // A `for (let [a,b] of …)` / `for (let {x} of …)` head destructures each
        // element; a plain `for (let x of …)` binds it to one variable.
        let decl_pat = match &f.left {
            ox::ForStatementLeft::VariableDeclaration(d) => Some(&d.declarations[0].id),
            _ => None,
        };
        let pattern =
            decl_pat.filter(|p| !matches!(p, ox::BindingPattern::BindingIdentifier(_)));

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

        // The loop binding: either a destructuring pattern's leaves, or a single
        // (possibly cell-boxed) variable.
        let (var_reg, var_is_cell) = match pattern {
            Some(p) => {
                self.declare_pattern(p)?;
                (0, false)
            }
            None => {
                let var_name = for_left_name(&f.left)?;
                let r = self.declare_local(&var_name);
                (r, self.cell_regs.contains(&r))
            }
        };

        let top = self.here();
        // while (idx < len)
        let cond = self.temp();
        self.emit(Instr::Lt { dst: cond, a: idx_reg, b: len_reg });
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        self.next_reg -= 1; // reclaim cond temp

        // <binding> = iter[idx]
        if let Some(p) = pattern {
            let save = self.next_reg;
            let elem = self.alloc_reg();
            self.emit(Instr::GetIndex { dst: elem, obj: iter_reg, key: idx_reg });
            self.extract_pattern(p, elem)?;
            self.next_reg = save;
        } else if var_is_cell {
            let tmp = self.temp();
            self.emit(Instr::GetIndex { dst: tmp, obj: iter_reg, key: idx_reg });
            self.emit(Instr::CellSet { cell: var_reg, src: tmp });
            self.next_reg -= 1;
        } else {
            self.emit(Instr::GetIndex { dst: var_reg, obj: iter_reg, key: idx_reg });
        }

        self.loop_ctx.push(LoopCtx::loop_frame());
        self.stmt(&f.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → increment + re-test
        }
        self.emit(Instr::AddInt { dst: idx_reg, a: idx_reg, imm: 1 });
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
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

        self.loop_ctx.push(LoopCtx::loop_frame());
        self.stmt(&f.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → increment + re-test
        }
        self.emit(Instr::AddInt { dst: idx_reg, a: idx_reg, imm: 1 });
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
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
                    // `new Array(…)` / `new Object()` builtins (no real global).
                    if id.name == "Array" {
                        let (arg_base, argc) = self.eval_args_contiguous(&n.arguments)?;
                        self.emit(Instr::ArrayCtor { dst, arg_base, argc });
                        return Ok(dst);
                    }
                    if id.name == "Object" && n.arguments.is_empty() {
                        self.emit(Instr::NewObject { dst });
                        return Ok(dst);
                    }
                }
                // General `new C(args)`: evaluate the constructor value, then the
                // args (contiguous), and let the VM build the instance.
                let callee = self.expr(&n.callee)?;
                let (arg_base, argc) = self.eval_args_contiguous(&n.arguments)?;
                self.emit(Instr::New { dst, callee, arg_base, argc });
                Ok(dst)
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
            E::StaticMemberExpression(m) => self.static_member(m, dst),
            E::ComputedMemberExpression(m) => self.computed_member(m, dst),
            E::ChainExpression(ce) => self.chain_expr(ce, dst),
            _ => Err("unsupported expression (not in the zipp-vm v1 subset yet)".into()),
        }
    }

    fn static_member(&mut self, m: &ox::StaticMemberExpression, dst: Reg) -> R<Reg> {
        // Math constants (Math.PI, Math.E, …) — Math has no real global object.
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
        if m.optional {
            self.emit_optional_check(obj);
        }
        let name = self.string_name(m.property.name.as_str());
        self.emit(Instr::GetProp { dst, obj, name });
        Ok(dst)
    }

    fn computed_member(&mut self, m: &ox::ComputedMemberExpression, dst: Reg) -> R<Reg> {
        let obj = self.expr(&m.object)?;
        if m.optional {
            self.emit_optional_check(obj);
        }
        let key = self.expr(&m.expression)?;
        self.emit(Instr::GetIndex { dst, obj, key });
        Ok(dst)
    }

    /// `?.` short-circuit: if `obj` is null/undefined (loose `== null`), jump to
    /// the enclosing chain's "undefined" block, recorded for patching at chain
    /// exit. No-op outside a chain (an `optional` flag can only appear in one).
    fn emit_optional_check(&mut self, obj: Reg) {
        if self.chain_bails.is_empty() {
            return;
        }
        let save = self.next_reg;
        let nreg = self.alloc_reg();
        self.emit(Instr::LoadNull { dst: nreg });
        let cond = self.alloc_reg();
        self.emit(Instr::LooseEq { dst: cond, a: obj, b: nreg }); // true iff null OR undefined
        let jt = self.here();
        self.emit(Instr::JumpIfTrue { cond, target: 0 });
        self.chain_bails.last_mut().unwrap().push(jt);
        self.next_reg = save; // scratch temps dead after the check
    }

    /// Compile an optional chain `a?.b…`: open a short-circuit boundary, compile
    /// the chain element (its `?.` links record bail jumps), then route any
    /// short-circuit to a single `undefined` result.
    fn chain_expr(&mut self, ce: &ox::ChainExpression, dst: Reg) -> R<Reg> {
        self.chain_bails.push(Vec::new());
        let res = match &ce.expression {
            ox::ChainElement::StaticMemberExpression(m) => self.static_member(m, dst),
            ox::ChainElement::ComputedMemberExpression(m) => self.computed_member(m, dst),
            ox::ChainElement::CallExpression(c) => self.call(c, dst),
            _ => Err("this optional-chain form is not in the zipp-vm subset yet".into()),
        };
        let bails = self.chain_bails.pop().unwrap();
        let v = res?;
        if bails.is_empty() {
            return Ok(v);
        }
        if v != dst {
            self.emit(Instr::Move { dst, src: v });
        }
        let jmp = self.here();
        self.emit(Instr::Jump { target: 0 });
        let undef_at = self.here();
        self.emit(Instr::LoadUndefined { dst });
        let end = self.here();
        self.patch_jump(jmp, end);
        for b in bails {
            self.patch_jump(b, undef_at);
        }
        Ok(dst)
    }

    fn array_literal(&mut self, a: &ox::ArrayExpression, dst: Reg) -> R<Reg> {
        // With a `...spread` element the final length is dynamic, so build the
        // array incrementally via ArrayAppend instead of the fixed-block NewArray.
        if a.elements.iter().any(|e| matches!(e, ox::ArrayExpressionElement::SpreadElement(_))) {
            self.emit(Instr::NewArray { dst, arg_base: self.next_reg, argc: 0 }); // []
            for el in &a.elements {
                let save = self.next_reg;
                match el {
                    ox::ArrayExpressionElement::Elision(_) => {
                        let v = self.temp();
                        self.emit(Instr::LoadUndefined { dst: v });
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: false });
                    }
                    ox::ArrayExpressionElement::SpreadElement(s) => {
                        let v = self.expr(&s.argument)?;
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: true });
                    }
                    other => {
                        let e = other.as_expression().ok_or("unsupported array element")?;
                        let v = self.expr(e)?;
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: false });
                    }
                }
                self.next_reg = save;
            }
            return Ok(dst);
        }
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
                ox::ArrayExpressionElement::SpreadElement(_) => unreachable!("handled above"),
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
            let save = self.next_reg;
            match prop {
                ox::ObjectPropertyKind::ObjectProperty(p) => {
                    if p.computed {
                        // Computed key `{[expr]: v}` → SetIndex.
                        let ke = p.key.as_expression().ok_or("unsupported computed object key")?;
                        let key = self.expr(ke)?;
                        let v = self.expr(&p.value)?;
                        self.emit(Instr::SetIndex { obj: dst, key, val: v });
                    } else {
                        // Static identifier / string / number literal key.
                        let key = match &p.key {
                            ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                            ox::PropertyKey::StringLiteral(s) => s.value.to_string(),
                            ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                            _ => return Err("unsupported object key in the zipp-vm subset".into()),
                        };
                        let name = self.string_name(&key);
                        let v = self.expr(&p.value)?;
                        self.emit(Instr::SetProp { obj: dst, name, val: v });
                    }
                }
                ox::ObjectPropertyKind::SpreadProperty(s) => {
                    let src = self.expr(&s.argument)?;
                    self.emit(Instr::ObjectSpread { target: dst, src });
                }
            }
            self.next_reg = save; // reclaim this property's scratch temps
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
        // `x instanceof Ctor`: only built-in constructors are recognised (the
        // engine has no user prototype chain). Decided structurally in the VM.
        if matches!(b.operator, Op::Instanceof) {
            // A built-in constructor name → structural InstanceOf; anything else
            // (a user class value) → runtime InstanceOfDyn against its class link.
            if let ox::Expression::Identifier(id) = &b.right {
                if let Some(ctor) = InstanceCtor::from_name(&id.name) {
                    let val = self.expr(&b.left)?;
                    self.emit(Instr::InstanceOf { dst, val, ctor });
                    return Ok(dst);
                }
            }
            let val = self.expr(&b.left)?;
            let ctor = self.expr(&b.right)?;
            self.emit(Instr::InstanceOfDyn { dst, val, ctor });
            return Ok(dst);
        }
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
            Op::BitwiseAnd => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::And },
            Op::BitwiseOR => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Or },
            Op::BitwiseXOR => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Xor },
            Op::ShiftLeft => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Shl },
            Op::ShiftRight => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Shr },
            Op::ShiftRightZeroFill => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Ushr },
            Op::Exponential => Instr::Pow { dst, a, b: r },
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
            Op::Coalesce => {
                // `a ?? b`: keep `a` unless it is null/undefined. `== undefined`
                // (loose) is true for both null and undefined.
                let save = self.next_reg;
                let undef = self.alloc_reg();
                self.emit(Instr::LoadUndefined { dst: undef });
                let isnull = self.alloc_reg();
                self.emit(Instr::LooseEq { dst: isnull, a: dst, b: undef });
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: isnull, target: 0 }); // non-nullish → keep dst
                self.next_reg = save; // the nullish-test temps are dead now
                let b = self.expr_into(&l.right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
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
            Op::BitwiseNot => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::BitNot { dst, a });
                Ok(dst)
            }
            _ => Err("unsupported unary operator (zipp-vm v1)".into()),
        }
    }

    fn update(&mut self, u: &ox::UpdateExpression, dst: Reg) -> R<Reg> {
        let delta = match u.operator {
            ox::UpdateOperator::Increment => 1,
            ox::UpdateOperator::Decrement => -1,
        };
        // `obj.x++` / `arr[i]--` etc — read the member, yield old (postfix) or
        // new (prefix), write the incremented value back to the same slot.
        match &u.argument {
            ox::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                let cur = self.temp();
                self.emit(Instr::GetProp { dst: cur, obj, name });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: cur, imm: delta });
                self.emit(Instr::SetProp { obj, name, val: nw });
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { cur } });
                return Ok(dst);
            }
            ox::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                let cur = self.temp();
                self.emit(Instr::GetIndex { dst: cur, obj, key });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: cur, imm: delta });
                self.emit(Instr::SetIndex { obj, key, val: nw });
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { cur } });
                return Ok(dst);
            }
            _ => {}
        }
        // `x++` / `++x` / `x--` / `--x` on a simple identifier.
        let name = match &u.argument {
            ox::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => id.name.to_string(),
            _ => return Err("update on this target not in zipp-vm v1".into()),
        };
        let binding = self.resolve(&name);
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

    /// Emit default-value init for `x = default` parameters: `if (x === undefined)
    /// x = default`. Runs once at function entry, before the body. Param regs are
    /// already bound (captured ones boxed), so reads/writes go through resolve +
    /// load_binding/store_binding (handling plain locals and cells uniformly).
    fn emit_param_defaults(&mut self, params: &ox::FormalParameters) -> R<()> {
        for item in &params.items {
            // oxc stores a parameter default in `initializer` (the pattern stays a
            // plain BindingIdentifier), e.g. `function f(x = 5)`.
            let default = match &item.initializer {
                Some(d) => d,
                None => continue,
            };
            let name = match &item.pattern {
                ox::BindingPattern::BindingIdentifier(id) => id.name.to_string(),
                _ => continue, // destructuring patterns aren't in the subset
            };
            let b = self.resolve(&name);
            let save = self.next_reg;
            let prtmp = self.alloc_reg();
            let pr = self.load_binding(&b, prtmp);
            let undef = self.alloc_reg();
            self.emit(Instr::LoadUndefined { dst: undef });
            let cond = self.alloc_reg();
            self.emit(Instr::Eq { dst: cond, a: pr, b: undef });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond, target: 0 }); // skip when x !== undefined
            let dtmp = self.alloc_reg();
            let dv = self.expr_into(default, dtmp)?;
            self.store_binding(&b, dv);
            let end = self.here();
            self.patch_jump(jf, end);
            // The init temps are dead before the body; reclaim them (max_reg has
            // already captured the high-water) so body locals reuse the registers.
            self.next_reg = save;
        }
        Ok(())
    }

    /// Assign `src` to a destructuring-assignment target (existing binding or
    /// member, or a nested array/object pattern). Counterpart to `extract_pattern`
    /// for `=` targets that aren't declarations.
    fn assign_target(&mut self, target: &ox::AssignmentTarget, src: Reg) -> R<()> {
        use ox::AssignmentTarget as T;
        match target {
            T::AssignmentTargetIdentifier(id) => {
                let b = self.resolve(&id.name);
                self.store_binding(&b, src);
                Ok(())
            }
            T::StaticMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                self.emit(Instr::SetProp { obj, name, val: src });
                self.next_reg = save;
                Ok(())
            }
            T::ComputedMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                self.emit(Instr::SetIndex { obj, key, val: src });
                self.next_reg = save;
                Ok(())
            }
            T::ArrayAssignmentTarget(arr) => self.assign_array_target(arr, src),
            T::ObjectAssignmentTarget(o) => self.assign_object_target(o, src),
            _ => Err("unsupported destructuring-assignment target in the zipp-vm subset".into()),
        }
    }

    /// One element of a destructuring assignment, applying its `= default` first.
    fn assign_maybe_default(
        &mut self,
        m: &ox::AssignmentTargetMaybeDefault,
        val: Reg,
    ) -> R<()> {
        use ox::AssignmentTargetMaybeDefault as M;
        match m {
            M::AssignmentTargetWithDefault(d) => {
                self.apply_default_in_place(val, &d.init)?;
                self.assign_target(&d.binding, val)
            }
            M::AssignmentTargetIdentifier(id) => {
                let b = self.resolve(&id.name);
                self.store_binding(&b, val);
                Ok(())
            }
            M::StaticMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                self.emit(Instr::SetProp { obj, name, val });
                self.next_reg = save;
                Ok(())
            }
            M::ComputedMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                self.emit(Instr::SetIndex { obj, key, val });
                self.next_reg = save;
                Ok(())
            }
            M::ArrayAssignmentTarget(arr) => self.assign_array_target(arr, val),
            M::ObjectAssignmentTarget(o) => self.assign_object_target(o, val),
            _ => Err("unsupported destructuring-assignment element in the zipp-vm subset".into()),
        }
    }

    fn assign_array_target(&mut self, arr: &ox::ArrayAssignmentTarget, src: Reg) -> R<()> {
        for (i, el) in arr.elements.iter().enumerate() {
            if let Some(maybe) = el {
                let save = self.next_reg;
                let val = self.alloc_reg();
                let idx = self.alloc_reg();
                self.emit(Instr::LoadInt { dst: idx, val: i as i32 });
                self.emit(Instr::GetIndex { dst: val, obj: src, key: idx });
                self.assign_maybe_default(maybe, val)?;
                self.next_reg = save;
            }
        }
        if let Some(rest) = &arr.rest {
            let save = self.next_reg;
            let val = self.alloc_reg();
            self.emit(Instr::ArrayRest { dst: val, src, start: arr.elements.len() as u32 });
            self.assign_target(&rest.target, val)?;
            self.next_reg = save;
        }
        Ok(())
    }

    fn assign_object_target(&mut self, o: &ox::ObjectAssignmentTarget, src: Reg) -> R<()> {
        if o.rest.is_some() {
            return Err("object rest in destructuring assignment is not in the zipp-vm subset yet".into());
        }
        for prop in &o.properties {
            let save = self.next_reg;
            match prop {
                ox::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
                    // `({x} = o)` / `({x = d} = o)` — target is the identifier itself.
                    let val = self.alloc_reg();
                    let name = self.string_name(&p.binding.name);
                    self.emit(Instr::GetProp { dst: val, obj: src, name });
                    if let Some(init) = &p.init {
                        self.apply_default_in_place(val, init)?;
                    }
                    let b = self.resolve(&p.binding.name);
                    self.store_binding(&b, val);
                }
                ox::AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
                    // `({key: target} = o)`.
                    let val = self.alloc_reg();
                    self.extract_member(src, &p.name, p.computed, val)?;
                    self.assign_maybe_default(&p.binding, val)?;
                }
            }
            self.next_reg = save;
        }
        Ok(())
    }

    /// For a logical assignment (`||= &&= ??=`), emit the short-circuit test on
    /// `val` (which already holds the target's current value) and return the ip
    /// of the jump that, when taken, SKIPS the assignment (keeping `val`).
    fn emit_logical_skip(&mut self, op: ox::AssignmentOperator, val: Reg) -> u32 {
        use ox::AssignmentOperator as Op;
        match op {
            Op::LogicalOr => {
                let j = self.here();
                self.emit(Instr::JumpIfTrue { cond: val, target: 0 }); // truthy → skip
                j
            }
            Op::LogicalAnd => {
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: val, target: 0 }); // falsy → skip
                j
            }
            _ => {
                // ??= : skip when `val` is NOT null/undefined.
                let save = self.next_reg;
                let undef = self.alloc_reg();
                self.emit(Instr::LoadUndefined { dst: undef });
                let isnull = self.alloc_reg();
                self.emit(Instr::LooseEq { dst: isnull, a: val, b: undef });
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: isnull, target: 0 });
                self.next_reg = save;
                j
            }
        }
    }

    fn assign(&mut self, a: &ox::AssignmentExpression, dst: Reg) -> R<Reg> {
        use ox::AssignmentOperator as Op;
        let is_logical =
            matches!(a.operator, Op::LogicalOr | Op::LogicalAnd | Op::LogicalNullish);
        // Member-target assignment: `obj.x = v` / `arr[i] = v`. Only plain
        // `=` is supported for members in this subset.
        match &a.left {
            ox::AssignmentTarget::StaticMemberExpression(m) => {
                let obj = self.expr(&m.object)?; // evaluate the receiver once
                let name = self.string_name(m.property.name.as_str());
                if is_logical {
                    // `obj.x ??= v` etc: read current; skip the store on short-circuit.
                    self.emit(Instr::GetProp { dst, obj, name });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SetProp { obj, name, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SetProp { obj, name, val: dst });
                } else {
                    // Compound `obj.x op= v`: read obj.x, combine, write back.
                    let cur = self.temp();
                    self.emit(Instr::GetProp { dst: cur, obj, name });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SetProp { obj, name, val: dst });
                }
                return Ok(dst);
            }
            ox::AssignmentTarget::ComputedMemberExpression(m) => {
                let obj = self.expr(&m.object)?; // evaluate receiver + key once
                let key = self.expr(&m.expression)?;
                if is_logical {
                    self.emit(Instr::GetIndex { dst, obj, key });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SetIndex { obj, key, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SetIndex { obj, key, val: dst });
                } else {
                    let cur = self.temp();
                    self.emit(Instr::GetIndex { dst: cur, obj, key });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SetIndex { obj, key, val: dst });
                }
                return Ok(dst);
            }
            // Destructuring assignment to existing targets: `[a,b]=arr`, `({x}=o)`.
            ox::AssignmentTarget::ArrayAssignmentTarget(_)
            | ox::AssignmentTarget::ObjectAssignmentTarget(_) => {
                let src = self.expr_into(&a.right, dst)?;
                if src != dst {
                    self.emit(Instr::Move { dst, src });
                }
                self.assign_target(&a.left, dst)?;
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
            // Logical assignment: `x ||= y` / `x &&= y` / `x ??= y` only assign
            // `y` when the short-circuit condition holds (truthy-skip for ||=,
            // falsy-skip for &&=, non-nullish-skip for ??=).
            Op::LogicalOr | Op::LogicalAnd | Op::LogicalNullish => {
                let cur = self.load_binding(&binding, dst);
                if cur != dst {
                    self.emit(Instr::Move { dst, src: cur });
                }
                let j = self.emit_logical_skip(a.operator, dst);
                let v = self.expr_into(&a.right, dst)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.store_binding(&binding, dst);
                let end = self.here();
                self.patch_jump(j, end);
                Ok(dst)
            }
            // Arithmetic / bitwise compound assignment (`+= -= *= /= %= **= <<=
            // >>= >>>= |= ^= &=`).
            other => {
                if let Binding::Local(r) = binding {
                    // Plain local: compute in place.
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(other, r, r, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
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
                let instr = compound_assign_instr(other, dst, cur, rhs)
                    .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                self.emit(instr);
                self.store_binding(&binding, dst);
                Ok(dst)
            }
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
        // Optional call `f?.(args)`: evaluate the callee as a VALUE (its own `?.`
        // links short-circuit inside the chain), bail to undefined if it's
        // nullish, else call it. (Uses the general value-call, so `o?.m?.()` calls
        // with `this = undefined` — a rare edge.)
        if c.optional {
            let callee = self.expr(&c.callee)?;
            self.emit_optional_check(callee);
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::Call { dst, callee, arg_base, argc });
            return Ok(dst);
        }
        // Spread call: `f(...args)`, `obj.m(...args)`, `arr.push(...xs)`, etc.
        // Build the argument list as an array (spreading each `...x` element),
        // then dispatch via CallMethodSpread (method receiver) or CallSpread
        // (plain function value). Spread on a builtin like Math.max(...arr) that
        // isn't a method call is out of scope.
        if c.arguments.iter().any(|a| matches!(a, ox::Argument::SpreadElement(_))) {
            // `Math.max(...arr)` / `Math.min(...arr)` / `Math.hypot(...arr)` —
            // a variadic Math reduction over the spread array.
            if let ox::Expression::StaticMemberExpression(m) = &c.callee {
                if let ox::Expression::Identifier(obj) = &m.object {
                    if obj.name == "Math" {
                        if let Some(op) = crate::bytecode::MathFn::from_name(m.property.name.as_str()) {
                            let args_arr = self.build_spread_args(&c.arguments)?;
                            self.emit(Instr::MathSpread { dst, op, args: args_arr });
                            return Ok(dst);
                        }
                    }
                }
            }
            // Method call `obj.m(...)` — evaluate the receiver first so `this`
            // binds correctly, then build args, then CallMethodSpread.
            if let ox::Expression::StaticMemberExpression(m) = &c.callee {
                let obj = self.expr(&m.object)?;
                if m.optional {
                    self.emit_optional_check(obj);
                }
                let name = self.string_name(m.property.name.as_str());
                let args_arr = self.build_spread_args(&c.arguments)?;
                self.emit(Instr::CallMethodSpread { dst, obj, name, args: args_arr });
                return Ok(dst);
            }
            let callee = self.expr(&c.callee)?;
            let args_arr = self.build_spread_args(&c.arguments)?;
            self.emit(Instr::CallSpread { dst, callee, args: args_arr });
            return Ok(dst);
        }

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
            // Bare `Array(…)` / `Object()` behave like their `new` forms.
            if id.name == "Array" {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::ArrayCtor { dst, arg_base, argc });
                return Ok(dst);
            }
            if id.name == "Object" && c.arguments.is_empty() {
                self.emit(Instr::NewObject { dst });
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

        // `JSON.stringify(value, replacer, space)` → JsonStringify op (the
        // replacer arg is ignored; `space` controls indentation).
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "JSON" && m.property.name == "parse" && c.arguments.len() == 1 {
                    if let Some(ae) = c.arguments[0].as_expression() {
                        let a = self.expr(ae)?;
                        self.emit(Instr::JsonParse { dst, a });
                        return Ok(dst);
                    }
                }
                if obj.name == "JSON" && m.property.name == "stringify" && !c.arguments.is_empty() {
                    if let Some(ve) = c.arguments[0].as_expression() {
                        let val = self.expr(ve)?;
                        let space = if c.arguments.len() >= 3 {
                            match c.arguments[2].as_expression() {
                                Some(se) => self.expr(se)?,
                                None => {
                                    let r = self.temp();
                                    self.emit(Instr::LoadUndefined { dst: r });
                                    r
                                }
                            }
                        } else {
                            let r = self.temp();
                            self.emit(Instr::LoadUndefined { dst: r });
                            r
                        };
                        self.emit(Instr::JsonStringify { dst, val, space });
                        return Ok(dst);
                    }
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

        // Constructor-namespace static methods with a flat arg list:
        // Object.assign, Array.of, String.fromCharCode, Number.isInteger/… .
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if let Some(op) =
                    crate::bytecode::StaticFn::from_name(&obj.name, m.property.name.as_str())
                {
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::StaticFn { dst, op, arg_base, argc });
                    return Ok(dst);
                }
                // `Array.from(src[, mapFn])` — needs iteration + optional callback.
                if obj.name == "Array" && m.property.name == "from" && !c.arguments.is_empty() {
                    if let Some(se) = c.arguments[0].as_expression() {
                        let save = self.next_reg;
                        let src = self.expr(se)?;
                        let mapfn = self.temp();
                        match c.arguments.get(1).and_then(|a| a.as_expression()) {
                            Some(fe) => {
                                let f = self.expr_into(fe, mapfn)?;
                                if f != mapfn {
                                    self.emit(Instr::Move { dst: mapfn, src: f });
                                }
                            }
                            None => self.emit(Instr::LoadUndefined { dst: mapfn }),
                        }
                        self.emit(Instr::ArrayFrom { dst, src, mapfn });
                        self.next_reg = save.max(dst + 1);
                        return Ok(dst);
                    }
                }
            }
        }

        // Method call `obj.name(args…)` → CallMethod, binding `this` to obj.
        // (Computed-member calls `obj[k](…)` fall through to the generic path.)
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            let obj = self.expr(&m.object)?;
            if m.optional {
                self.emit_optional_check(obj); // `obj?.method()` — short-circuit if obj nullish
            }
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

    /// Build a call-argument list containing `...spread` into a fresh array and
    /// return its (live) register. Each plain arg is pushed as one element; each
    /// `...x` arg appends every element of `x` (an array, or a string's chars).
    /// Consumed by `CallSpread` / `CallMethodSpread`.
    fn build_spread_args(&mut self, args: &oxc_allocator::Vec<ox::Argument>) -> R<Reg> {
        let args_arr = self.temp();
        self.emit(Instr::NewArray { dst: args_arr, arg_base: self.next_reg, argc: 0 });
        for a in args {
            let save = self.next_reg;
            match a {
                ox::Argument::SpreadElement(s) => {
                    let v = self.expr(&s.argument)?;
                    self.emit(Instr::ArrayAppend { arr: args_arr, val: v, spread: true });
                }
                other => {
                    let e = other.as_expression().ok_or("unsupported spread-call argument")?;
                    let v = self.expr(e)?;
                    self.emit(Instr::ArrayAppend { arr: args_arr, val: v, spread: false });
                }
            }
            self.next_reg = save;
        }
        Ok(args_arr)
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

/// The instruction for an arithmetic/bitwise compound assignment (`dst = a <op>
/// b`). `None` for `=` and the logical-assignment operators (handled separately).
fn compound_assign_instr(op: ox::AssignmentOperator, dst: Reg, a: Reg, b: Reg) -> Option<Instr> {
    use ox::AssignmentOperator as Op;
    Some(match op {
        Op::Addition => Instr::Add { dst, a, b },
        Op::Subtraction => Instr::Sub { dst, a, b },
        Op::Multiplication => Instr::Mul { dst, a, b },
        Op::Division => Instr::Div { dst, a, b },
        Op::Remainder => Instr::Mod { dst, a, b },
        Op::Exponential => Instr::Pow { dst, a, b },
        Op::ShiftLeft => Instr::Bitwise { dst, a, b, op: BitwiseOp::Shl },
        Op::ShiftRight => Instr::Bitwise { dst, a, b, op: BitwiseOp::Shr },
        Op::ShiftRightZeroFill => Instr::Bitwise { dst, a, b, op: BitwiseOp::Ushr },
        Op::BitwiseOR => Instr::Bitwise { dst, a, b, op: BitwiseOp::Or },
        Op::BitwiseXOR => Instr::Bitwise { dst, a, b, op: BitwiseOp::Xor },
        Op::BitwiseAnd => Instr::Bitwise { dst, a, b, op: BitwiseOp::And },
        _ => return None,
    })
}

/// A class member's (non-computed) name. Computed `[expr]` and `#private` names
/// are out of the subset.
fn class_key_name(key: &ox::PropertyKey) -> R<String> {
    match key {
        ox::PropertyKey::StaticIdentifier(id) => Ok(id.name.to_string()),
        ox::PropertyKey::StringLiteral(s) => Ok(s.value.to_string()),
        ox::PropertyKey::NumericLiteral(n) => Ok(fmt_key_num(n.value)),
        _ => Err("computed or private class member names are not in the zipp-vm subset yet".into()),
    }
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

/// Extract (fixed param names, optional rest-param name, body statements) from
/// an oxc function.
fn function_parts<'a>(
    f: &'a ox::Function,
) -> R<(Vec<String>, Option<String>, &'a [ox::Statement<'a>])> {
    let mut params = Vec::new();
    for item in &f.params.items {
        match &item.pattern {
            ox::BindingPattern::BindingIdentifier(id) => params.push(id.name.to_string()),
            // `x = default` — bind the name here; the default is applied at the
            // function entry by compile_function_body's emit_param_defaults.
            ox::BindingPattern::AssignmentPattern(ap) => match &ap.left {
                ox::BindingPattern::BindingIdentifier(id) => params.push(id.name.to_string()),
                _ => return Err("destructuring parameters are not in the zipp-vm subset yet".into()),
            },
            _ => return Err("parameter patterns are not in the zipp-vm v1 subset yet".into()),
        }
    }
    let rest = rest_name(&f.params)?;
    let body = match &f.body {
        Some(b) => b.statements.as_slice(),
        None => &[],
    };
    Ok((params, rest, body))
}

/// All bindable parameter names (fixed params plus the rest name, if any) — the
/// set capture analysis must consider locals of this function.
fn with_rest(params: &[String], rest: &Option<String>) -> Vec<String> {
    let mut v = params.to_vec();
    if let Some(r) = rest {
        v.push(r.clone());
    }
    v
}

/// The rest-parameter name (`function f(...args)` → `Some("args")`), or `None`.
/// Only a plain identifier rest target is supported (no `...{a,b}`).
fn rest_name(params: &ox::FormalParameters) -> R<Option<String>> {
    match &params.rest {
        None => Ok(None),
        Some(r) => match &r.rest.argument {
            ox::BindingPattern::BindingIdentifier(id) => Ok(Some(id.name.to_string())),
            _ => Err("rest-parameter destructuring is not in the zipp-vm subset yet".into()),
        },
    }
}
