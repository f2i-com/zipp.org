//! Bytecode IR + lowering from the checked AST.
//!
//! v0 lowers to a flat register-machine bytecode (a stand-in for ZHIR/ZMIR).
//! Registers are frame-relative and lexically scoped: entering a block snapshots
//! the register watermark, exiting restores it (so sibling blocks reuse
//! registers). `&&`/`||` are short-circuited; `break`/`continue` patch to the
//! enclosing loop. Jumps use absolute code offsets and are backpatched.

use crate::ast::{BinOp, Expr, Module, Stmt, StmtKind, StructDecl, Type, UnOp};
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
    Builtin { op: BuiltinOp, dst: u32, args: Vec<u32> },
    // Structs (heap-backed, like arrays; rejected by the integer-only zk profile).
    NewStruct { id: u32, dst: u32, fields: Vec<u32> },
    GetField { dst: u32, base: u32, field: String },
    SetField { base: u32, field: String, value: u32 },
    // Nullable struct references. `id` is the struct the null belongs to when
    // known from context (so the JIT can type a null-initialized local).
    ConstNull { dst: u32, id: Option<u32> },
}

/// Field names of a struct, in declaration order (indexed by struct id). The VM
/// uses this to resolve a field name to its slot.
#[derive(Debug, Clone)]
pub struct StructLayout {
    pub fields: Vec<String>,
    /// Field types, parallel to `fields` (the VM ignores these — it's
    /// dynamically tagged — but the native backends need them to type each slot).
    pub types: Vec<Type>,
}

/// Builtin math functions (recognized call names, not user functions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinOp {
    Abs,
    Min,
    Max,
    Pow,
    Sqrt,
    Floor,
    Ceil,
}

/// Map a call name to a math builtin, if it is one.
pub fn math_builtin(name: &str) -> Option<BuiltinOp> {
    Some(match name {
        "abs" => BuiltinOp::Abs,
        "min" => BuiltinOp::Min,
        "max" => BuiltinOp::Max,
        "pow" => BuiltinOp::Pow,
        "sqrt" => BuiltinOp::Sqrt,
        "floor" => BuiltinOp::Floor,
        "ceil" => BuiltinOp::Ceil,
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub struct FuncMeta {
    pub name: String,
    pub entry: u32,
    pub nregs: u32,
    pub nparams: u32,
    /// Return type and parameter types (used by the native JIT for signatures
    /// and per-register type inference).
    pub ret: Type,
    pub params: Vec<Type>,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub code: Vec<Instr>,
    pub funcs: Vec<FuncMeta>,
    pub main: u32,
    pub structs: Vec<StructLayout>,
}

impl Program {
    /// True if the program uses nullable struct references (`T | null`). The
    /// native tiers (jit/llvm/wasm) fall back to the interpreter for these (v0).
    pub fn uses_nullable(&self) -> bool {
        let opt = |t: &Type| matches!(t, Type::OptStruct(_) | Type::Null);
        self.code.iter().any(|i| matches!(i, Instr::ConstNull { .. }))
            || self.funcs.iter().any(|f| opt(&f.ret) || f.params.iter().any(opt))
            || self.structs.iter().any(|s| s.types.iter().any(opt))
    }
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
            structs: &m.structs,
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
            ret: f.ret,
            params: f.params.iter().map(|p| p.ty).collect(),
        });
    }

    let main = *func_index.get("main").expect("checker guarantees main exists");
    let structs = m
        .structs
        .iter()
        .map(|sd| StructLayout {
            fields: sd.fields.iter().map(|(n, _)| n.clone()).collect(),
            types: sd.fields.iter().map(|(_, t)| *t).collect(),
        })
        .collect();
    Ok(Program { code, funcs, main, structs })
}

struct LoopCtx {
    breaks: Vec<u32>,
    continues: Vec<u32>,
}

struct Gen<'a> {
    code: &'a mut Vec<Instr>,
    func_index: &'a HashMap<String, u32>,
    structs: &'a [StructDecl],
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
        self.scopes.pop().expect("scope underflow");
        // Registers are NOT reclaimed on scope exit: monotonic allocation gives
        // each register a single static type, which the native JIT relies on.
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
        match &s.kind {
            StmtKind::Let { name, ty, value } => {
                // A null-initialized nullable local carries its struct id, so the
                // JIT can type the register (and dereference it after narrowing).
                let v = match (value, ty) {
                    (Expr::Null, Some(Type::OptStruct(id))) => {
                        let r = self.alloc();
                        self.code.push(Instr::ConstNull { dst: r, id: Some(*id) });
                        r
                    }
                    _ => self.gen_expr(value)?,
                };
                let r = self.declare(name);
                self.code.push(Instr::Mov { dst: r, src: v });
                Ok(())
            }
            StmtKind::Assign { target, value } => match target {
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
                Expr::Field { base, field } => {
                    let b = self.gen_expr(base)?;
                    let v = self.gen_expr(value)?;
                    self.code.push(Instr::SetField { base: b, field: field.clone(), value: v });
                    Ok(())
                }
                _ => Err("ir error: invalid assignment target".into()),
            },
            StmtKind::Return(Some(e)) => {
                let v = self.gen_expr(e)?;
                self.code.push(Instr::Ret { src: v });
                Ok(())
            }
            StmtKind::Return(None) => Err("ir error: bare return unsupported".into()),
            StmtKind::If { cond, then_b, else_b } => {
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
            StmtKind::While { cond, body } => {
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
            StmtKind::For { init, cond, step, body } => {
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
            StmtKind::Break => {
                let idx = self.here();
                self.code.push(Instr::Jmp { target: 0 });
                self.loops
                    .last_mut()
                    .ok_or("ir error: break outside loop")?
                    .breaks
                    .push(idx);
                Ok(())
            }
            StmtKind::Continue => {
                let idx = self.here();
                self.code.push(Instr::Jmp { target: 0 });
                self.loops
                    .last_mut()
                    .ok_or("ir error: continue outside loop")?
                    .continues
                    .push(idx);
                Ok(())
            }
            StmtKind::Print(e) => {
                let v = self.gen_expr(e)?;
                self.code.push(Instr::Print { a: v });
                Ok(())
            }
            StmtKind::ExprStmt(e) => {
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
            Expr::StructLit { name, fields } => {
                // Evaluate provided fields, then arrange them in declaration order.
                let mut named: Vec<(String, u32)> = Vec::with_capacity(fields.len());
                for (fname, fexpr) in fields {
                    let r = self.gen_expr(fexpr)?;
                    named.push((fname.clone(), r));
                }
                let id = self
                    .structs
                    .iter()
                    .position(|s| &s.name == name)
                    .ok_or_else(|| format!("ir error: unknown struct '{name}'"))? as u32;
                let order: Vec<String> =
                    self.structs[id as usize].fields.iter().map(|(n, _)| n.clone()).collect();
                let mut ordered = Vec::with_capacity(order.len());
                for dn in &order {
                    let r = named
                        .iter()
                        .find(|(n, _)| n == dn)
                        .map(|(_, r)| *r)
                        .ok_or_else(|| format!("ir error: missing field '{dn}'"))?;
                    ordered.push(r);
                }
                let dst = self.alloc();
                self.code.push(Instr::NewStruct { id, dst, fields: ordered });
                Ok(dst)
            }
            Expr::Field { base, field } => {
                let b = self.gen_expr(base)?;
                let dst = self.alloc();
                self.code.push(Instr::GetField { dst, base: b, field: field.clone() });
                Ok(dst)
            }
            Expr::Null => {
                let dst = self.alloc();
                self.code.push(Instr::ConstNull { dst, id: None });
                Ok(dst)
            }
            // `lhs ?? rhs` — evaluate lhs once; if it's null, take rhs.
            Expr::Coalesce { lhs, rhs } => {
                let dst = self.alloc();
                let l = self.gen_expr(lhs)?;
                self.code.push(Instr::Mov { dst, src: l });
                let nullr = self.alloc();
                self.code.push(Instr::ConstNull { dst: nullr, id: None });
                let isnull = self.alloc();
                self.code.push(Instr::Bin { op: BinOp::Eq, dst: isnull, a: dst, b: nullr });
                let jz = self.here();
                self.code.push(Instr::JmpIfZero { cond: isnull, target: 0 }); // not null → keep lhs
                let rb = self.gen_expr(rhs)?;
                self.code.push(Instr::Mov { dst, src: rb });
                let end = self.here();
                self.patch(jz, end);
                Ok(dst)
            }
            // `base?.field` — null if base is null, else base.field.
            Expr::OptField { base, field } => {
                let dst = self.alloc();
                let b = self.gen_expr(base)?;
                self.code.push(Instr::ConstNull { dst, id: None }); // default: null
                let nullr = self.alloc();
                self.code.push(Instr::ConstNull { dst: nullr, id: None });
                let isnull = self.alloc();
                self.code.push(Instr::Bin { op: BinOp::Eq, dst: isnull, a: b, b: nullr });
                let jnz = self.here();
                self.code.push(Instr::JmpIfNonZero { cond: isnull, target: 0 }); // null → keep null
                let fv = self.alloc();
                self.code.push(Instr::GetField { dst: fv, base: b, field: field.clone() });
                self.code.push(Instr::Mov { dst, src: fv });
                let end = self.here();
                self.patch(jnz, end);
                Ok(dst)
            }
            // `base?.field ?? default` — base.field, or default when base is null.
            Expr::OptFieldOr { base, field, default } => {
                let dst = self.alloc();
                let b = self.gen_expr(base)?;
                let nullr = self.alloc();
                self.code.push(Instr::ConstNull { dst: nullr, id: None });
                let isnull = self.alloc();
                self.code.push(Instr::Bin { op: BinOp::Eq, dst: isnull, a: b, b: nullr });
                let jz = self.here();
                self.code.push(Instr::JmpIfZero { cond: isnull, target: 0 }); // non-null → load field
                let d = self.gen_expr(default)?;
                self.code.push(Instr::Mov { dst, src: d });
                let jmp = self.here();
                self.code.push(Instr::Jmp { target: 0 });
                let fld = self.here();
                self.patch(jz, fld);
                let fv = self.alloc();
                self.code.push(Instr::GetField { dst: fv, base: b, field: field.clone() });
                self.code.push(Instr::Mov { dst, src: fv });
                let end = self.here();
                self.patch(jmp, end);
                Ok(dst)
            }
            // Ternary `cond ? then : els` — branch, with both arms writing `dst`
            // (mirrors the short-circuit `&&`/`||` lowering above).
            Expr::Cond { cond, then, els } => {
                let dst = self.alloc();
                let c = self.gen_expr(cond)?;
                let jz = self.here();
                self.code.push(Instr::JmpIfZero { cond: c, target: 0 });
                let ta = self.gen_expr(then)?;
                self.code.push(Instr::Mov { dst, src: ta });
                let jend = self.here();
                self.code.push(Instr::Jmp { target: 0 });
                let else_start = self.here();
                self.patch(jz, else_start);
                let eb = self.gen_expr(els)?;
                self.code.push(Instr::Mov { dst, src: eb });
                let end = self.here();
                self.patch(jend, end);
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
            Expr::Call { name, args } if math_builtin(name).is_some() => {
                let op = math_builtin(name).unwrap();
                let regs = args
                    .iter()
                    .map(|a| self.gen_expr(a))
                    .collect::<Result<Vec<_>, _>>()?;
                let dst = self.alloc();
                self.code.push(Instr::Builtin { op, dst, args: regs });
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
