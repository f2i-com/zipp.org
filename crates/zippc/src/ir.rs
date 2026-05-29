//! Bytecode IR + lowering from the checked AST.
//!
//! v0 lowers straight to a flat register-machine bytecode (a stand-in for the
//! ZHIR/ZMIR pipeline in PLAN.md). Registers are frame-relative; each call
//! pushes a fresh register window. Jumps use absolute code offsets and are
//! backpatched.

use crate::ast::{BinOp, Func, Module, Stmt, UnOp};
use crate::ast::Expr;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Instr {
    Const { dst: u32, imm: i64 },
    Mov { dst: u32, src: u32 },
    Bin { op: BinOp, dst: u32, a: u32, b: u32 },
    Unary { op: UnOp, dst: u32, a: u32 },
    Jmp { target: u32 },
    JmpIfZero { cond: u32, target: u32 },
    Call { func: u32, arg_base: u32, argc: u32, dst: u32 },
    Ret { src: u32 },
    Print { a: u32 },
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
    // Pass 1: assign function indices.
    let mut func_index: HashMap<String, u32> = HashMap::new();
    for (i, f) in m.funcs.iter().enumerate() {
        func_index.insert(f.name.clone(), i as u32);
    }

    let mut code: Vec<Instr> = Vec::new();
    let mut funcs: Vec<FuncMeta> = Vec::with_capacity(m.funcs.len());

    // Pass 2: lower each function body into the shared code buffer.
    for f in &m.funcs {
        let entry = code.len() as u32;
        let mut g = Gen {
            code: &mut code,
            func_index: &func_index,
            regs: HashMap::new(),
            next_reg: 0,
        };
        for (i, p) in f.params.iter().enumerate() {
            g.regs.insert(p.name.clone(), i as u32);
        }
        g.next_reg = f.params.len() as u32;
        g.gen_block(&f.body)?;
        // Fallthrough return 0 so execution never runs off the end.
        let z = g.alloc();
        g.code.push(Instr::Const { dst: z, imm: 0 });
        g.code.push(Instr::Ret { src: z });
        let nregs = g.next_reg;
        funcs.push(FuncMeta {
            name: f.name.clone(),
            entry,
            nregs,
            nparams: f.params.len() as u32,
        });
    }

    let main = *func_index.get("main").expect("checker guarantees main exists");
    Ok(Program { code, funcs, main })
}

struct Gen<'a> {
    code: &'a mut Vec<Instr>,
    func_index: &'a HashMap<String, u32>,
    regs: HashMap<String, u32>,
    next_reg: u32,
}

impl<'a> Gen<'a> {
    fn alloc(&mut self) -> u32 {
        let r = self.next_reg;
        self.next_reg += 1;
        r
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
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
                let r = self.alloc();
                self.code.push(Instr::Mov { dst: r, src: v });
                self.regs.insert(name.clone(), r);
                Ok(())
            }
            Stmt::Assign { name, value } => {
                let v = self.gen_expr(value)?;
                let r = *self
                    .regs
                    .get(name)
                    .ok_or_else(|| format!("ir error: assign to unbound '{name}'"))?;
                self.code.push(Instr::Mov { dst: r, src: v });
                Ok(())
            }
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
                self.gen_block(then_b)?;
                let jend = self.here();
                self.code.push(Instr::Jmp { target: 0 });
                let else_start = self.here();
                self.patch_target(jz, else_start);
                self.gen_block(else_b)?;
                let end = self.here();
                self.patch_target(jend, end);
                Ok(())
            }
            Stmt::While { cond, body } => {
                let lstart = self.here();
                let c = self.gen_expr(cond)?;
                let jexit = self.here();
                self.code.push(Instr::JmpIfZero { cond: c, target: 0 });
                self.gen_block(body)?;
                self.code.push(Instr::Jmp { target: lstart });
                let end = self.here();
                self.patch_target(jexit, end);
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

    fn patch_target(&mut self, at: u32, target: u32) {
        match &mut self.code[at as usize] {
            Instr::Jmp { target: t } | Instr::JmpIfZero { target: t, .. } => *t = target,
            _ => unreachable!("patch_target on non-jump instruction"),
        }
    }

    fn gen_expr(&mut self, e: &Expr) -> Result<u32, String> {
        match e {
            Expr::Int(n) => {
                let r = self.alloc();
                self.code.push(Instr::Const { dst: r, imm: *n });
                Ok(r)
            }
            Expr::Bool(b) => {
                let r = self.alloc();
                self.code.push(Instr::Const { dst: r, imm: if *b { 1 } else { 0 } });
                Ok(r)
            }
            Expr::Var(name) => self
                .regs
                .get(name)
                .copied()
                .ok_or_else(|| format!("ir error: unbound variable '{name}'")),
            Expr::Unary { op, e } => {
                let a = self.gen_expr(e)?;
                let r = self.alloc();
                self.code.push(Instr::Unary { op: *op, dst: r, a });
                Ok(r)
            }
            Expr::Bin { op, l, r } => {
                let a = self.gen_expr(l)?;
                let b = self.gen_expr(r)?;
                let dst = self.alloc();
                self.code.push(Instr::Bin { op: *op, dst, a, b });
                Ok(dst)
            }
            Expr::Call { name, args } => {
                let func = *self
                    .func_index
                    .get(name)
                    .ok_or_else(|| format!("ir error: call to unknown function '{name}'"))?;
                let argc = args.len() as u32;
                // Reserve a contiguous argument block, then evaluate each arg
                // and move it into place (arg exprs may allocate temps above).
                let arg_base = self.next_reg;
                self.next_reg += argc;
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
