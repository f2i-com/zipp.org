//! Compile a function body's owned AST to [`Chunk`] bytecode — for the hot,
//! closure-free numeric/control-flow subset. Anything outside the subset makes
//! the whole compile bail (`Err(())`), and the caller keeps tree-walking that
//! function. Correctness rule: a compiled function's locals live in a flat VM
//! array that dies on return, so a body that could let an inner closure capture
//! an outer local would be wrong — therefore ANY nested function/arrow, `this`,
//! `arguments`, `super`, objects/arrays, member access, try/throw, for-in/of,
//! templates, spread, optional-chaining, or `??` bails.

use crate::ast::*;
use crate::bytecode::{Chunk, Op};
use crate::value::JsValue;

type R<T> = Result<T, ()>;

struct LoopCtx {
    /// Jump instruction indices to patch to the loop exit (from `break`).
    breaks: Vec<usize>,
    /// Jump instruction indices to patch to the loop's continue target.
    continues: Vec<usize>,
}

struct Compiler {
    code: Vec<Op>,
    consts: Vec<JsValue>,
    names: Vec<String>,
    /// Block scopes mapping a declared name to its local slot (innermost last).
    scopes: Vec<Vec<(String, u16)>>,
    nlocals: u16,
    loops: Vec<LoopCtx>,
}

/// Compile a function `def` to a [`Chunk`], or `Err(())` if any node is outside
/// the supported subset. `def.rest` (rest params) bails — handled by the caller.
pub fn compile_fn(def: &FuncDef) -> R<Chunk> {
    if def.rest.is_some() || def.uses_arguments {
        return Err(());
    }
    let mut c = Compiler {
        code: Vec::new(),
        consts: Vec::new(),
        names: Vec::new(),
        scopes: vec![Vec::new()],
        nlocals: 0,
        loops: Vec::new(),
    };
    // Params occupy the first slots, in order. A default value bails (keeps the
    // tree-walker's default-param semantics).
    for p in &def.params {
        if p.default.is_some() {
            return Err(());
        }
        c.declare(&p.name);
    }
    let nparams = def.params.len();
    c.block(&def.body)?;
    // Implicit `return undefined` at the end.
    c.code.push(Op::PushUndefined);
    c.code.push(Op::Return);
    Ok(Chunk {
        code: c.code,
        consts: c.consts,
        names: c.names,
        nlocals: c.nlocals as usize,
        nparams,
    })
}

impl Compiler {
    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    /// Declare `name` in the current block scope, returning its slot. Reuses an
    /// existing slot if the name is already declared in this same scope (var
    /// re-declaration / approximates function-scoped var).
    fn declare(&mut self, name: &str) -> u16 {
        let scope = self.scopes.last_mut().unwrap();
        if let Some((_, slot)) = scope.iter().find(|(n, _)| n == name) {
            return *slot;
        }
        let slot = self.nlocals;
        self.nlocals += 1;
        scope.push((name.to_string(), slot));
        slot
    }

    /// Resolve `name` to a local slot, searching inner scopes outward.
    fn resolve(&self, name: &str) -> Option<u16> {
        for scope in self.scopes.iter().rev() {
            if let Some((_, slot)) = scope.iter().rev().find(|(n, _)| n == name) {
                return Some(*slot);
            }
        }
        None
    }

    /// Intern a name in the chunk's name table (for free-variable load/store).
    fn name_idx(&mut self, name: &str) -> u32 {
        if let Some(i) = self.names.iter().position(|n| n == name) {
            return i as u32;
        }
        self.names.push(name.to_string());
        (self.names.len() - 1) as u32
    }

    fn const_idx(&mut self, v: JsValue) -> u32 {
        self.consts.push(v);
        (self.consts.len() - 1) as u32
    }

    // ───────────────────────── statements ─────────────────────────

    fn block(&mut self, stmts: &[Stmt]) -> R<()> {
        // A function declaration anywhere in the body would need hoisting + could
        // be captured — bail. (Cheap pre-scan; nested blocks are scanned when
        // entered.)
        self.scopes.push(Vec::new());
        let r = self.block_inner(stmts);
        self.scopes.pop();
        r
    }

    fn block_inner(&mut self, stmts: &[Stmt]) -> R<()> {
        for s in stmts {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> R<()> {
        match s {
            Stmt::Empty => Ok(()),
            Stmt::Expr(e) => {
                self.expr(e)?;
                self.code.push(Op::Pop);
                Ok(())
            }
            Stmt::Var { decls, .. } => {
                for (name, init) in decls {
                    match init {
                        Some(e) => {
                            self.expr(e)?;
                            let slot = self.declare(name);
                            self.code.push(Op::StoreLocal(slot));
                            self.code.push(Op::Pop);
                        }
                        None => {
                            self.declare(name);
                        }
                    }
                }
                Ok(())
            }
            Stmt::Block(stmts) => self.block(stmts),
            Stmt::If { cond, then, els } => {
                self.expr(cond)?;
                let jf = self.emit_jump_if_false();
                self.stmt(then)?;
                if let Some(e) = els {
                    let jend = self.emit_jump();
                    self.patch_to_here(jf);
                    self.stmt(e)?;
                    self.patch_to_here(jend);
                } else {
                    self.patch_to_here(jf);
                }
                Ok(())
            }
            Stmt::While { cond, body } => {
                let top = self.here();
                self.expr(cond)?;
                let jf = self.emit_jump_if_false();
                self.loops.push(LoopCtx { breaks: Vec::new(), continues: Vec::new() });
                self.stmt(body)?;
                let lc = self.loops.pop().unwrap();
                self.code.push(Op::Jump(top));
                self.patch_to_here(jf);
                let end = self.here();
                for b in lc.breaks {
                    self.patch(b, end);
                }
                for c in lc.continues {
                    self.patch(c, top); // continue re-tests the condition
                }
                Ok(())
            }
            Stmt::DoWhile { body, cond } => {
                let top = self.here();
                self.loops.push(LoopCtx { breaks: Vec::new(), continues: Vec::new() });
                self.stmt(body)?;
                let lc = self.loops.pop().unwrap();
                let cont = self.here();
                self.expr(cond)?;
                self.code.push(Op::JumpIfTrue(top));
                let end = self.here();
                for b in lc.breaks {
                    self.patch(b, end);
                }
                for c in lc.continues {
                    self.patch(c, cont);
                }
                Ok(())
            }
            Stmt::For { init, cond, step, body } => {
                // A for with no per-iteration closure capture is fine as one
                // flat scope (the tree-walker has the same per-iteration-let
                // limitation; compiled or not, behavior matches).
                self.scopes.push(Vec::new());
                let r = self.compile_for(init, cond, step, body);
                self.scopes.pop();
                r
            }
            Stmt::Return(e) => {
                match e {
                    Some(e) => self.expr(e)?,
                    None => self.code.push(Op::PushUndefined),
                }
                self.code.push(Op::Return);
                Ok(())
            }
            Stmt::Break => {
                let j = self.emit_jump();
                self.loops.last_mut().ok_or(())?.breaks.push(j as usize);
                Ok(())
            }
            Stmt::Continue => {
                let j = self.emit_jump();
                self.loops.last_mut().ok_or(())?.continues.push(j as usize);
                Ok(())
            }
            // Everything else (Func, Class, Throw, Try, ForOf, ForIn, Switch) bails.
            _ => Err(()),
        }
    }

    fn compile_for(&mut self, init: &Option<Box<Stmt>>, cond: &Option<Expr>, step: &Option<Expr>, body: &Stmt) -> R<()> {
        if let Some(i) = init {
            self.stmt(i)?;
        }
        let top = self.here();
        let jf = if let Some(c) = cond {
            self.expr(c)?;
            Some(self.emit_jump_if_false())
        } else {
            None
        };
        self.loops.push(LoopCtx { breaks: Vec::new(), continues: Vec::new() });
        self.stmt(body)?;
        let lc = self.loops.pop().unwrap();
        let cont = self.here();
        if let Some(st) = step {
            self.expr(st)?;
            self.code.push(Op::Pop);
        }
        self.code.push(Op::Jump(top));
        let end = self.here();
        if let Some(jf) = jf {
            self.patch(jf as usize, end);
        }
        for b in lc.breaks {
            self.patch(b, end);
        }
        for c in lc.continues {
            self.patch(c, cont);
        }
        Ok(())
    }

    // ───────────────────────── expressions ─────────────────────────

    fn expr(&mut self, e: &Expr) -> R<()> {
        match e {
            Expr::Num(n) => {
                let i = self.const_idx(JsValue::Num(*n));
                self.code.push(Op::PushConst(i));
            }
            Expr::Str(s) => {
                let i = self.const_idx(JsValue::Str(s.clone()));
                self.code.push(Op::PushConst(i));
            }
            Expr::Bool(b) => self.code.push(if *b { Op::PushTrue } else { Op::PushFalse }),
            Expr::Null => self.code.push(Op::PushNull),
            Expr::Undefined => self.code.push(Op::PushUndefined),
            Expr::Ident(name) => {
                if let Some(slot) = self.resolve(name) {
                    self.code.push(Op::LoadLocal(slot));
                } else {
                    let i = self.name_idx(name);
                    self.code.push(Op::LoadName(i));
                }
            }
            Expr::Binary { op, l, r } => {
                self.expr(l)?;
                self.expr(r)?;
                self.code.push(Op::Bin(*op));
            }
            Expr::Unary { op, arg } => {
                match op {
                    UnOp::Neg => {
                        self.expr(arg)?;
                        self.code.push(Op::Neg);
                    }
                    UnOp::Plus => {
                        self.expr(arg)?;
                        self.code.push(Op::Pos);
                    }
                    UnOp::Not => {
                        self.expr(arg)?;
                        self.code.push(Op::Not);
                    }
                    // BitNot/TypeOf/Void: bail (rare in hot numeric code).
                    _ => return Err(()),
                }
            }
            Expr::Logical { op, l, r } => match op {
                LogicalOp::And => {
                    self.expr(l)?;
                    self.code.push(Op::Dup);
                    let j = self.emit_jump_if_false();
                    self.code.push(Op::Pop);
                    self.expr(r)?;
                    self.patch_to_here(j);
                }
                LogicalOp::Or => {
                    self.expr(l)?;
                    self.code.push(Op::Dup);
                    let j = self.emit_jump_if_true();
                    self.code.push(Op::Pop);
                    self.expr(r)?;
                    self.patch_to_here(j);
                }
                LogicalOp::Nullish => return Err(()),
            },
            Expr::Cond { cond, then, els } => {
                self.expr(cond)?;
                let jf = self.emit_jump_if_false();
                self.expr(then)?;
                let jend = self.emit_jump();
                self.patch_to_here(jf);
                self.expr(els)?;
                self.patch_to_here(jend);
            }
            Expr::Assign { op, target, value } => {
                let name = match target.as_ref() {
                    Expr::Ident(n) => n,
                    _ => return Err(()), // member assignment bails
                };
                match op {
                    None => {
                        self.expr(value)?;
                    }
                    Some(o) => {
                        self.load_ident(name);
                        self.expr(value)?;
                        self.code.push(Op::Bin(*o));
                    }
                }
                self.store_ident(name); // peeks: leaves the assigned value
            }
            Expr::Update { op, prefix, arg } => {
                let name = match arg.as_ref() {
                    Expr::Ident(n) => n,
                    _ => return Err(()),
                };
                let one = self.const_idx(JsValue::Num(1.0));
                let binop = match op {
                    UpdateOp::Inc => BinOp::Add,
                    UpdateOp::Dec => BinOp::Sub,
                };
                if *prefix {
                    // ++x: x = ToNumber(x) + 1; yields new value.
                    self.load_ident(name);
                    self.code.push(Op::Pos); // ToNumber
                    self.code.push(Op::PushConst(one));
                    self.code.push(Op::Bin(binop));
                    self.store_ident(name);
                } else {
                    // x++: yields ToNumber(x) (old), stores old+1.
                    self.load_ident(name);
                    self.code.push(Op::Pos); // old as number, stays as the result
                    self.code.push(Op::Dup);
                    self.code.push(Op::PushConst(one));
                    self.code.push(Op::Bin(binop));
                    self.store_ident(name);
                    self.code.push(Op::Pop); // drop new, leaving old
                }
            }
            Expr::Call { callee, args } => {
                // Only simple calls (callee is any non-member expr). Member callee
                // (method call) needs `this` -> bail.
                if matches!(callee.as_ref(), Expr::Member { .. } | Expr::Super) {
                    return Err(());
                }
                if args.len() > u8::MAX as usize {
                    return Err(());
                }
                self.expr(callee)?;
                for a in args {
                    if matches!(a, Expr::Spread(_)) {
                        return Err(());
                    }
                    self.expr(a)?;
                }
                self.code.push(Op::Call(args.len() as u8));
            }
            Expr::Seq(exprs) => {
                for (i, e) in exprs.iter().enumerate() {
                    self.expr(e)?;
                    if i + 1 != exprs.len() {
                        self.code.push(Op::Pop);
                    }
                }
            }
            // Everything else bails: This, Template, Array, Object, New, Func,
            // Member, Super, Spread, OptChain, LogicalAssign.
            _ => return Err(()),
        }
        Ok(())
    }

    fn load_ident(&mut self, name: &str) {
        if let Some(slot) = self.resolve(name) {
            self.code.push(Op::LoadLocal(slot));
        } else {
            let i = self.name_idx(name);
            self.code.push(Op::LoadName(i));
        }
    }

    fn store_ident(&mut self, name: &str) {
        if let Some(slot) = self.resolve(name) {
            self.code.push(Op::StoreLocal(slot));
        } else {
            let i = self.name_idx(name);
            self.code.push(Op::StoreName(i));
        }
    }

    // ───────────────────────── jump helpers ─────────────────────────

    fn emit_jump(&mut self) -> usize {
        let at = self.code.len();
        self.code.push(Op::Jump(0));
        at
    }
    fn emit_jump_if_false(&mut self) -> usize {
        let at = self.code.len();
        self.code.push(Op::JumpIfFalse(0));
        at
    }
    fn emit_jump_if_true(&mut self) -> usize {
        let at = self.code.len();
        self.code.push(Op::JumpIfTrue(0));
        at
    }
    fn patch_to_here(&mut self, at: usize) {
        let target = self.here();
        self.patch(at, target);
    }
    fn patch(&mut self, at: usize, target: u32) {
        self.code[at] = match self.code[at] {
            Op::Jump(_) => Op::Jump(target),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(target),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(target),
            _ => unreachable!("patching a non-jump op"),
        };
    }
}
