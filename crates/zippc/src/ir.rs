//! Bytecode IR + lowering from the checked AST.
//!
//! v0 lowers to a flat register-machine bytecode (a stand-in for ZHIR/ZMIR).
//! Registers are frame-relative and lexically scoped: entering a block snapshots
//! the register watermark, exiting restores it (so sibling blocks reuse
//! registers). `&&`/`||` are short-circuited; `break`/`continue` patch to the
//! enclosing loop. Jumps use absolute code offsets and are backpatched.

use crate::ast::{BinOp, Expr, Module, Stmt, Type, UnOp};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Instr {
    Const { dst: u32, imm: i64 },
    FConst { dst: u32, imm: f64 },
    SConst { dst: u32, imm: String },
    Cast { dst: u32, src: u32, to: Type },
    Mov { dst: u32, src: u32 },
    Bin { op: BinOp, dst: u32, a: u32, b: u32 },
    Unary { op: UnOp, dst: u32, a: u32 },
    Jmp { target: u32 },
    JmpIfZero { cond: u32, target: u32 },
    JmpIfNonZero { cond: u32, target: u32 },
    Call { func: u32, arg_base: u32, argc: u32, dst: u32 },
    Ret { src: u32 },
    Print { a: u32 },
    // Arrays (heap-backed; rejected by the integer-only zk profile).
    ArrayLit { dst: u32, elems: Vec<u32> },
    ArrayRepeat { dst: u32, value: u32, count: u32 },
    Index { dst: u32, arr: u32, idx: u32 },
    SetIndex { arr: u32, idx: u32, value: u32 },
    Len { dst: u32, arr: u32 },
}

#[derive(Debug, Clone)]
pub struct FuncMeta {
    pub name: String,
    pub entry: u32,
    pub nregs: u32,
    pub nparams: u32,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub code: Vec<Instr>,
    pub funcs: Vec<FuncMeta>,
    pub main: u32,
}

pub fn lower(m: &Module) -> Result<Program, String> {
    let mut func_index: HashMap<String, u32> = HashMap::new();
    for (i, f) in m.funcs.iter().enumerate() {
        func_index.insert(f.name.clone(), i as u32);
    }

    let mut code: Vec<Instr> = Vec::new();
    let mut funcs: Vec<FuncMeta> = Vec::with_capacity(m.funcs.len());

    for f in &m.funcs {
        let entry = code.len() as u32;
        let mut g = Gen {
            code: &mut code,
            func_index: &func_index,
            scopes: vec![(HashMap::new(), 0)],
            next_reg: 0,
            max_reg: 0,
            loops: Vec::new(),
        };
        for (i, p) in f.params.iter().enumerate() {
            g.scopes[0].0.insert(p.name.clone(), i as u32);
        }
        g.next_reg = f.params.len() as u32;
        g.max_reg = g.next_reg;
        g.gen_block(&f.body)?;
        // Fallthrough return of the zero value of the return type, so execution
        // never runs off the end.
        let z = g.alloc();
        if f.ret == Type::F64 {
            g.code.push(Instr::FConst { dst: z, imm: 0.0 });
        } else {
            g.code.push(Instr::Const { dst: z, imm: 0 });
        }
        g.code.push(Instr::Ret { src: z });
        funcs.push(FuncMeta {
            name: f.name.clone(),
            entry,
            nregs: g.max_reg,
            nparams: f.params.len() as u32,
        });
    }

    let main = *func_index.get("main").expect("checker guarantees main exists");
    Ok(Program { code, funcs, main })
}

struct LoopCtx {
    breaks: Vec<u32>,
    continues: Vec<u32>,
}

struct Gen<'a> {
    code: &'a mut Vec<Instr>,
    func_index: &'a HashMap<String, u32>,
    /// Scope stack: (name -> register, saved next_reg watermark on entry).
    scopes: Vec<(HashMap<String, u32>, u32)>,
    next_reg: u32,
    max_reg: u32,
    loops: Vec<LoopCtx>,
}

impl<'a> Gen<'a> {
    fn alloc(&mut self) -> u32 {
        let r = self.next_reg;
        self.next_reg += 1;
        if self.next_reg > self.max_reg {
            self.max_reg = self.next_reg;
        }
        r
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    fn enter(&mut self) {
        self.scopes.push((HashMap::new(), self.next_reg));
    }

    fn exit(&mut self) {
        let (_, saved) = self.scopes.pop().expect("scope underflow");
        self.next_reg = saved; // reclaim registers used inside the block
    }

    fn declare(&mut self, name: &str) -> u32 {
        let r = self.alloc();
        self.scopes.last_mut().unwrap().0.insert(name.to_string(), r);
        r
    }

    fn resolve(&self, name: &str) -> Result<u32, String> {
        self.scopes
            .iter()
            .rev()
            .find_map(|(m, _)| m.get(name).copied())
            .ok_or_else(|| format!("ir error: unbound variable '{name}'"))
    }

    fn scoped_block(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        self.enter();
        let r = self.gen_block(stmts);
        self.exit();
        r
    }

    fn gen_block(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        for s in stmts {
            self.gen_stmt(s)?;
        }
        Ok(())
    }

    fn gen_stmt(&mut self, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Let { name, value, .. } => {
                let v = self.gen_expr(value)?;
                let r = self.declare(name);
                self.code.push(Instr::Mov { dst: r, src: v });
                Ok(())
            }
            Stmt::Assign { target, value } => match target {
                Expr::Var(name) => {
                    let v = self.gen_expr(value)?;
                    let r = self.resolve(name)?;
                    self.code.push(Instr::Mov { dst: r, src: v });
                    Ok(())
                }
                Expr::Index { arr, index } => {
                    let a = self.gen_expr(arr)?;
                    let i = self.gen_expr(index)?;
                    let v = self.gen_expr(value)?;
                    self.code.push(Instr::SetIndex { arr: a, idx: i, value: v });
                    Ok(())
                }
                _ => Err("ir error: invalid assignment target".into()),
            },
            Stmt::Return(Some(e)) => {
                let v = self.gen_expr(e)?;
                self.code.push(Instr::Ret { src: v });
                Ok(())
            }
            Stmt::Return(None) => Err("ir error: bare return unsupported".into()),
            Stmt::If { cond, then_b, else_b } => {
                let c = self.gen_expr(cond)?;
                let jz = self.here();
                self.code.push(Instr::JmpIfZero { cond: c, target: 0 });
                self.scoped_block(then_b)?;
                let jend = self.here();
                self.code.push(Instr::Jmp { target: 0 });
                let else_start = self.here();
                self.patch(jz, else_start);
                self.scoped_block(else_b)?;
                let end = self.here();
                self.patch(jend, end);
                Ok(())
            }
            Stmt::While { cond, body } => {
                let lstart = self.here();
                let c = self.gen_expr(cond)?;
                let jexit = self.here();
                self.code.push(Instr::JmpIfZero { cond: c, target: 0 });
                self.loops.push(LoopCtx { breaks: Vec::new(), continues: Vec::new() });
                self.scoped_block(body)?;
                self.code.push(Instr::Jmp { target: lstart });
                let end = self.here();
                self.patch(jexit, end);
                let ctx = self.loops.pop().expect("loop ctx");
                for b in ctx.breaks {
                    self.patch(b, end);
                }
                for c in ctx.continues {
                    self.patch(c, lstart); // continue re-checks the condition
                }
                Ok(())
            }
            Stmt::For { init, cond, step, body } => {
                self.enter();
                if let Some(i) = init {
                    self.gen_stmt(i)?;
                }
                let lcond = self.here();
                let c = self.gen_expr(cond)?;
                let jexit = self.here();
                self.code.push(Instr::JmpIfZero { cond: c, target: 0 });
                self.loops.push(LoopCtx { breaks: Vec::new(), continues: Vec::new() });
                self.scoped_block(body)?;
                let lstep = self.here();
                if let Some(s) = step {
                    self.gen_stmt(s)?;
                }
                self.code.push(Instr::Jmp { target: lcond });
                let end = self.here();
                self.patch(jexit, end);
                let ctx = self.loops.pop().expect("loop ctx");
                for b in ctx.breaks {
                    self.patch(b, end);
                }
                for c in ctx.continues {
                    self.patch(c, lstep); // continue runs the step, then re-checks
                }
                self.exit();
                Ok(())
            }
            Stmt::Break => {
                let idx = self.here();
                self.code.push(Instr::Jmp { target: 0 });
                self.loops
                    .last_mut()
                    .ok_or("ir error: break outside loop")?
                    .breaks
                    .push(idx);
                Ok(())
            }
            Stmt::Continue => {
                let idx = self.here();
                self.code.push(Instr::Jmp { target: 0 });
                self.loops
                    .last_mut()
                    .ok_or("ir error: continue outside loop")?
                    .continues
                    .push(idx);
                Ok(())
            }
            Stmt::Print(e) => {
                let v = self.gen_expr(e)?;
                self.code.push(Instr::Print { a: v });
                Ok(())
            }
            Stmt::ExprStmt(e) => {
                self.gen_expr(e)?;
                Ok(())
            }
        }
    }

    fn patch(&mut self, at: u32, target: u32) {
        match &mut self.code[at as usize] {
            Instr::Jmp { target: t }
            | Instr::JmpIfZero { target: t, .. }
            | Instr::JmpIfNonZero { target: t, .. } => *t = target,
            _ => unreachable!("patch on non-jump instruction"),
        }
    }

    fn gen_expr(&mut self, e: &Expr) -> Result<u32, String> {
        match e {
            Expr::Int(n) => {
                let r = self.alloc();
                self.code.push(Instr::Const { dst: r, imm: *n });
                Ok(r)
            }
            Expr::Float(f) => {
                let r = self.alloc();
                self.code.push(Instr::FConst { dst: r, imm: *f });
                Ok(r)
            }
            Expr::Str(s) => {
                let r = self.alloc();
                self.code.push(Instr::SConst { dst: r, imm: s.clone() });
                Ok(r)
            }
            Expr::Bool(b) => {
                let r = self.alloc();
                self.code.push(Instr::Const { dst: r, imm: if *b { 1 } else { 0 } });
                Ok(r)
            }
            Expr::Var(name) => self.resolve(name),
            Expr::Cast { to, e } => {
                let src = self.gen_expr(e)?;
                let r = self.alloc();
                self.code.push(Instr::Cast { dst: r, src, to: *to });
                Ok(r)
            }
            Expr::Array(elems) => {
                let regs = elems
                    .iter()
                    .map(|e| self.gen_expr(e))
                    .collect::<Result<Vec<_>, _>>()?;
                let dst = self.alloc();
                self.code.push(Instr::ArrayLit { dst, elems: regs });
                Ok(dst)
            }
            Expr::Repeat { value, count } => {
                let v = self.gen_expr(value)?;
                let c = self.gen_expr(count)?;
                let dst = self.alloc();
                self.code.push(Instr::ArrayRepeat { dst, value: v, count: c });
                Ok(dst)
            }
            Expr::Index { arr, index } => {
                let a = self.gen_expr(arr)?;
                let i = self.gen_expr(index)?;
                let dst = self.alloc();
                self.code.push(Instr::Index { dst, arr: a, idx: i });
                Ok(dst)
            }
            Expr::Unary { op, e } => {
                let a = self.gen_expr(e)?;
                let r = self.alloc();
                self.code.push(Instr::Unary { op: *op, dst: r, a });
                Ok(r)
            }
            // Short-circuit logical operators lower to branches.
            Expr::Bin { op: BinOp::And, l, r } => {
                let dst = self.alloc();
                let la = self.gen_expr(l)?;
                self.code.push(Instr::Mov { dst, src: la });
                let jz = self.here();
                self.code.push(Instr::JmpIfZero { cond: dst, target: 0 });
                let rb = self.gen_expr(r)?;
                self.code.push(Instr::Mov { dst, src: rb });
                let end = self.here();
                self.patch(jz, end);
                Ok(dst)
            }
            Expr::Bin { op: BinOp::Or, l, r } => {
                let dst = self.alloc();
                let la = self.gen_expr(l)?;
                self.code.push(Instr::Mov { dst, src: la });
                let jnz = self.here();
                self.code.push(Instr::JmpIfNonZero { cond: dst, target: 0 });
                let rb = self.gen_expr(r)?;
                self.code.push(Instr::Mov { dst, src: rb });
                let end = self.here();
                self.patch(jnz, end);
                Ok(dst)
            }
            Expr::Bin { op, l, r } => {
                let a = self.gen_expr(l)?;
                let b = self.gen_expr(r)?;
                let dst = self.alloc();
                self.code.push(Instr::Bin { op: *op, dst, a, b });
                Ok(dst)
            }
            Expr::Call { name, args } if name == "len" => {
                let a = self.gen_expr(&args[0])?;
                let dst = self.alloc();
                self.code.push(Instr::Len { dst, arr: a });
                Ok(dst)
            }
            Expr::Call { name, args } => {
                let func = *self
                    .func_index
                    .get(name)
                    .ok_or_else(|| format!("ir error: call to unknown function '{name}'"))?;
                let argc = args.len() as u32;
                let arg_base = self.next_reg;
                // Reserve the contiguous argument window.
                for _ in 0..argc {
                    self.alloc();
                }
                for (i, a) in args.iter().enumerate() {
                    let v = self.gen_expr(a)?;
                    self.code.push(Instr::Mov { dst: arg_base + i as u32, src: v });
                }
                let dst = self.alloc();
                self.code.push(Instr::Call { func, arg_base, argc, dst });
                Ok(dst)
            }
        }
    }
}
