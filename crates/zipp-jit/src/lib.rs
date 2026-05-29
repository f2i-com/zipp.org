//! ZIPP native backend — a Cranelift JIT covering the **whole language**:
//! scalars (`i64`/`f64`, casts), 1-D arrays, strings, structs, and math
//! builtins. Nothing falls back to the interpreter anymore.
//!
//! Registers are statically typed (the IR allocates them monotonically, so each
//! has one type); a forward pass infers each register's type, then every ZIPP
//! function is lowered to a Cranelift function — registers → SSA variables,
//! jump targets → blocks, calls → direct calls.
//!
//! Arrays are heap blocks `[ len:i64 | e0 | e1 | … ]` of 8-byte slots (f64
//! elements stored bit-reinterpreted), allocated by a tiny leak-on-alloc
//! runtime (`zipp_alloc`); no GC yet, matching the interpreter's heap. Indexing
//! is bounds-checked (one unsigned compare catches negative and ≥len), aborting
//! via `zipp_oob` — same semantics as the interpreter.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use cranelift_codegen::settings::Configurable;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, InstBuilder, MemFlags, TrapCode, Type as ClifType, Value,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};

use zippc::ast::{BinOp, Elem, Type, UnOp};
use zippc::ir::{BuiltinOp, FuncMeta, Instr, Program};

// The runtime itself (allocator + conservative GC + string/print/pow helpers)
// lives in the shared `zipp-rt` crate, so the JIT and the LLVM tier share one
// implementation. Here we just register `zipp-rt`'s `extern "C"` entry points as
// JIT symbols and call them.

/// Runtime symbols the JIT calls into.
#[derive(Clone, Copy)]
struct RuntimeIds {
    print_i64: FuncId,
    print_u64: FuncId,
    print_f64: FuncId,
    print_str: FuncId,
    alloc: FuncId,
    array_repeat: FuncId,
    oob: FuncId,
    str_concat: FuncId,
    str_eq: FuncId,
    ipow: FuncId,
}

/// Result of a JIT'd `main`. `U64` is separate so a u64 result prints unsigned
/// (i32/u32 main returns widen into `I64` with the correct value).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JitValue {
    I64(i64),
    U64(u64),
    F64(f64),
}

impl std::fmt::Display for JitValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JitValue::I64(x) => write!(f, "{x}"),
            JitValue::U64(x) => write!(f, "{x}"),
            JitValue::F64(v) => write!(f, "{v}"),
        }
    }
}

/// A JIT run, with compile and execution timed separately — the headline
/// "ran in" number folds both together, but the generated code's real speed is
/// `execute` alone.
#[derive(Debug, Clone, Copy)]
pub struct JitOutcome {
    pub value: JitValue,
    pub compile: Duration,
    pub execute: Duration,
}

/// The JIT type of a register. `I32`/`U32` are Cranelift `i32`; everything else
/// (incl. `i64`/`u64`, bool, and array/string/struct pointers) is `i64`. The
/// signedness distinguishes `sdiv`/`udiv`, `slt`/`ult`, `sext`/`uext`, etc.
#[derive(Clone, Copy, PartialEq)]
enum JTy {
    I64,
    I32,
    U32,
    U64,
    F64,
    Arr(bool),
    Str,
    Struct(u32),
}

impl JTy {
    fn clif(self) -> ClifType {
        match self {
            JTy::F64 => types::F64,
            JTy::I32 | JTy::U32 => types::I32,
            _ => types::I64, // i64, u64, bool, array/string/struct pointers
        }
    }
    fn is_int(self) -> bool {
        matches!(self, JTy::I64 | JTy::I32 | JTy::U32 | JTy::U64)
    }
    /// Among integers, whether the type is signed (i32/i64).
    fn signed(self) -> bool {
        matches!(self, JTy::I64 | JTy::I32)
    }
    fn arr_f64(self) -> bool {
        matches!(self, JTy::Arr(true))
    }
    fn struct_id(self) -> Option<u32> {
        match self {
            JTy::Struct(id) => Some(id),
            _ => None,
        }
    }
}

fn jty_of(t: Type) -> JTy {
    match t {
        Type::F64 => JTy::F64,
        Type::Array(e) => JTy::Arr(matches!(e, Elem::F64)),
        Type::Str => JTy::Str,
        Type::Struct(id) => JTy::Struct(id),
        // a nullable struct is a struct pointer that may be 0 (null)
        Type::OptStruct(id) => JTy::Struct(id),
        Type::I32 => JTy::I32,
        Type::U32 => JTy::U32,
        Type::U64 => JTy::U64,
        _ => JTy::I64, // i64, bool, null
    }
}

/// (slot index, field type) of a struct field by name.
fn struct_field(prog: &Program, id: u32, field: &str) -> Option<(usize, JTy)> {
    let layout = &prog.structs[id as usize];
    let slot = layout.fields.iter().position(|f| f == field)?;
    Some((slot, jty_of(layout.types[slot])))
}

/// Cranelift type for a ZIPP type (bool / array / string pointers are i64).
fn clif_ty(t: Type) -> ClifType {
    jty_of(t).clif()
}

fn var(r: u32) -> Variable {
    Variable::from_u32(r)
}

/// Reason a program can't be JIT-compiled, or `None`. The native backend now
/// covers the whole language, so this is always `None` (kept for the CLI's
/// fallback path and forward compatibility).
pub fn ineligible_reason(_prog: &Program) -> Option<&'static str> {
    None // the JIT compiles the whole language, including nullable structs
}

fn make_sig(module: &JITModule, params: &[Type], ret: Type) -> cranelift_codegen::ir::Signature {
    let mut sig = module.make_signature();
    for p in params {
        sig.params.push(AbiParam::new(clif_ty(*p)));
    }
    sig.returns.push(AbiParam::new(clif_ty(ret)));
    sig
}

/// JIT-compile and run an eligible program's `main`, timing compile vs execute.
pub fn run(prog: &Program) -> Result<JitOutcome, String> {
    run_with(prog, false)
}

/// As [`run`], but `fast_math` enables FMA contraction (fusing `a*b + c` into a
/// single rounded multiply-add). Faster on dense f64, but the results can
/// differ in the last bit from the strict-IEEE interpreter — so it's opt-in.
pub fn run_with(prog: &Program, fast_math: bool) -> Result<JitOutcome, String> {
    if let Some(bad) = ineligible_reason(prog) {
        return Err(format!("--jit supports the scalar subset only (program uses {bad})"));
    }
    let c0 = Instant::now();

    // Tier-0 but with the optimizer on: opt_level=none (the default) leaves
    // obvious redundancy (e.g. recomputed `x*x`) in the code; "speed" turns on
    // Cranelift's egraph mid-end (GVN/LICM-style rewrites). The verifier is a
    // codegen-correctness self-check (compile-time cost only) — we trust our
    // lowering (it's tested), so turn it off to shrink the compile half.
    let mut flags = cranelift_codegen::settings::builder();
    flags
        .set("opt_level", "speed")
        .map_err(|e| format!("opt flag: {e}"))?;
    flags
        .set("enable_verifier", "false")
        .map_err(|e| format!("verifier flag: {e}"))?;
    let isa = cranelift_native::builder()
        .map_err(|e| format!("host isa unavailable: {e}"))?
        .finish(cranelift_codegen::settings::Flags::new(flags))
        .map_err(|e| format!("isa finish: {e}"))?;
    let mut jit = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    jit.symbol("zipp_print_i64", zipp_rt::zipp_print_i64 as *const u8);
    jit.symbol("zipp_print_u64", zipp_rt::zipp_print_u64 as *const u8);
    jit.symbol("zipp_print_f64", zipp_rt::zipp_print_f64 as *const u8);
    jit.symbol("zipp_print_str", zipp_rt::zipp_print_str as *const u8);
    jit.symbol("zipp_alloc", zipp_rt::zipp_alloc as *const u8);
    jit.symbol("zipp_array_repeat", zipp_rt::zipp_array_repeat as *const u8);
    jit.symbol("zipp_oob", zipp_rt::zipp_oob as *const u8);
    jit.symbol("zipp_str_concat", zipp_rt::zipp_str_concat as *const u8);
    jit.symbol("zipp_str_eq", zipp_rt::zipp_str_eq as *const u8);
    jit.symbol("zipp_ipow", zipp_rt::zipp_ipow as *const u8);
    let mut module = JITModule::new(jit);

    let mut func_ids = Vec::with_capacity(prog.funcs.len());
    for (i, f) in prog.funcs.iter().enumerate() {
        let sig = make_sig(&module, &f.params, f.ret);
        let id = module
            .declare_function(&format!("zfn{i}"), Linkage::Local, &sig)
            .map_err(|e| format!("declare {}: {e}", f.name))?;
        func_ids.push(id);
    }
    let import = |module: &mut JITModule, name: &str, params: &[ClifType], ret: Option<ClifType>| {
        let mut sig = module.make_signature();
        for p in params {
            sig.params.push(AbiParam::new(*p));
        }
        if let Some(r) = ret {
            sig.returns.push(AbiParam::new(r));
        }
        module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| format!("declare {name}: {e}"))
    };
    let rt = RuntimeIds {
        print_i64: import(&mut module, "zipp_print_i64", &[types::I64], None)?,
        print_u64: import(&mut module, "zipp_print_u64", &[types::I64], None)?,
        print_f64: import(&mut module, "zipp_print_f64", &[types::F64], None)?,
        print_str: import(&mut module, "zipp_print_str", &[types::I64], None)?,
        alloc: import(&mut module, "zipp_alloc", &[types::I64], Some(types::I64))?,
        array_repeat: import(&mut module, "zipp_array_repeat", &[types::I64, types::I64], Some(types::I64))?,
        oob: import(&mut module, "zipp_oob", &[types::I64, types::I64], None)?,
        str_concat: import(&mut module, "zipp_str_concat", &[types::I64, types::I64], Some(types::I64))?,
        str_eq: import(&mut module, "zipp_str_eq", &[types::I64, types::I64], Some(types::I64))?,
        ipow: import(&mut module, "zipp_ipow", &[types::I64, types::I64], Some(types::I64))?,
    };

    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    for (i, f) in prog.funcs.iter().enumerate() {
        let end = if i + 1 < prog.funcs.len() {
            prog.funcs[i + 1].entry
        } else {
            prog.code.len() as u32
        };
        ctx.func.signature = make_sig(&module, &f.params, f.ret);
        compile_function(&mut module, &mut ctx, &mut fctx, prog, i, end, &func_ids, &rt, fast_math)?;
        module
            .define_function(func_ids[i], &mut ctx)
            .map_err(|e| format!("jit codegen failed for {}: {e:?}", f.name))?;
        module.clear_context(&mut ctx);
    }
    module
        .finalize_definitions()
        .map_err(|e| format!("finalize failed: {e}"))?;

    let main = &prog.funcs[prog.main as usize];
    let ptr = module.get_finalized_function(func_ids[prog.main as usize]);
    let compile = c0.elapsed();

    // Anchor the GC's conservative stack scan: this frame sits just above
    // `main`'s, so scanning [sp-at-collection, &anchor) covers every frame that
    // can hold a live heap handle while the program runs.
    let anchor: u8 = 0;
    zipp_rt::gc_set_stack_bottom(&anchor as *const u8 as usize);
    std::hint::black_box(&anchor);

    let e0 = Instant::now();
    // SAFETY: main takes no params; read the result at its native width.
    let value = unsafe {
        match jty_of(main.ret) {
            JTy::F64 => {
                let f: extern "C" fn() -> f64 = std::mem::transmute(ptr);
                JitValue::F64(f())
            }
            JTy::I32 => {
                let f: extern "C" fn() -> i32 = std::mem::transmute(ptr);
                JitValue::I64(f() as i64) // sign-extend
            }
            JTy::U32 => {
                let f: extern "C" fn() -> u32 = std::mem::transmute(ptr);
                JitValue::I64(f() as i64) // zero-extend (value)
            }
            JTy::U64 => {
                let f: extern "C" fn() -> u64 = std::mem::transmute(ptr);
                JitValue::U64(f())
            }
            _ => {
                let f: extern "C" fn() -> i64 = std::mem::transmute(ptr);
                JitValue::I64(f())
            }
        }
    };
    let execute = e0.elapsed();
    Ok(JitOutcome { value, compile, execute })
}

/// Forward pass: the [`JTy`] of every register in a function.
fn infer_reg_types(prog: &Program, f: &FuncMeta, end: u32) -> Vec<JTy> {
    let mut t = vec![JTy::I64; f.nregs as usize];
    for (i, pt) in f.params.iter().enumerate() {
        t[i] = jty_of(*pt);
    }
    let is_f64 = |t: &[JTy], r: u32| matches!(t[r as usize], JTy::F64);
    for pc in f.entry..end {
        match &prog.code[pc as usize] {
            Instr::Const { dst, .. } => t[*dst as usize] = JTy::I64,
            Instr::FConst { dst, .. } => t[*dst as usize] = JTy::F64,
            Instr::SConst { dst, .. } => t[*dst as usize] = JTy::Str,
            Instr::Cast { dst, to, .. } => t[*dst as usize] = jty_of(*to),
            Instr::Mov { dst, src } => {
                // A null (`i64 0`) Mov'd into a heap/struct register keeps that
                // register's type — so `let x: T|null = …; x = null` stays a struct.
                let s = t[*src as usize];
                let heap = matches!(t[*dst as usize], JTy::Struct(_) | JTy::Arr(_) | JTy::Str);
                if !(s == JTy::I64 && heap) {
                    t[*dst as usize] = s;
                }
            }
            Instr::Bin { op, dst, a, .. } => {
                use BinOp::*;
                t[*dst as usize] = match op {
                    // comparisons / logical → bool (i64 0/1)
                    Eq | Ne | Lt | Le | Gt | Ge | And | Or => JTy::I64,
                    // arithmetic / bitwise / shift → the operand type
                    _ => t[*a as usize],
                };
            }
            Instr::Unary { op, dst, a } => {
                t[*dst as usize] = match op {
                    UnOp::Neg => t[*a as usize],
                    _ => JTy::I64,
                };
            }
            Instr::Call { func, dst, .. } => {
                t[*dst as usize] = jty_of(prog.funcs[*func as usize].ret);
            }
            Instr::ArrayLit { dst, elems } => {
                t[*dst as usize] = JTy::Arr(elems.first().is_some_and(|e| is_f64(&t, *e)));
            }
            Instr::ArrayRepeat { dst, value, .. } => {
                t[*dst as usize] = JTy::Arr(is_f64(&t, *value));
            }
            Instr::Index { dst, arr, .. } => {
                t[*dst as usize] = if t[*arr as usize].arr_f64() {
                    JTy::F64
                } else {
                    JTy::I64
                };
            }
            Instr::Len { dst, .. } => t[*dst as usize] = JTy::I64,
            Instr::NewStruct { id, dst, .. } => t[*dst as usize] = JTy::Struct(*id),
            Instr::ConstNull { dst, id } => {
                t[*dst as usize] = match id {
                    Some(s) => JTy::Struct(*s), // a typed null (e.g. a nullable local)
                    None => JTy::I64,           // an untyped null (compared, not dereferenced)
                };
            }
            Instr::GetField { dst, base, field } => {
                t[*dst as usize] = t[*base as usize]
                    .struct_id()
                    .and_then(|id| struct_field(prog, id, field))
                    .map_or(JTy::I64, |(_, fty)| fty);
            }
            Instr::Builtin { op, dst, args } => {
                use BuiltinOp::*;
                t[*dst as usize] = match op {
                    Abs | Min | Max => t[args[0] as usize], // polymorphic in the operand
                    Pow => JTy::I64,
                    Sqrt | Floor | Ceil => JTy::F64,
                };
            }
            _ => {}
        }
    }
    t
}

/// A fused `p*q + r` (optionally negating p or r), replacing an adjacent
/// `mul` + `add`/`sub` pair.
#[derive(Clone, Copy)]
struct FmaPlan {
    p: u32,
    q: u32,
    r: u32,
    neg_p: bool,
    neg_r: bool,
}

/// Static read-count of every register over a function's instruction range.
fn use_counts(prog: &Program, f: &FuncMeta, end: u32) -> Vec<u32> {
    let mut u = vec![0u32; f.nregs as usize];
    let mut bump = |r: u32| u[r as usize] += 1;
    for pc in f.entry..end {
        match &prog.code[pc as usize] {
            Instr::Cast { src, .. } | Instr::Mov { src, .. } => bump(*src),
            Instr::Bin { a, b, .. } => {
                bump(*a);
                bump(*b);
            }
            Instr::Unary { a, .. } => bump(*a),
            Instr::JmpIfZero { cond, .. } | Instr::JmpIfNonZero { cond, .. } => bump(*cond),
            Instr::Call { arg_base, argc, .. } => {
                for k in 0..*argc {
                    bump(*arg_base + k);
                }
            }
            Instr::Ret { src } => bump(*src),
            Instr::Print { a } => bump(*a),
            _ => {}
        }
    }
    u
}

/// Plan FMA contraction: find each f64 `mul` whose result is used exactly once,
/// by the immediately-following `add`/`sub` in the same block. Returns the set
/// of mul pcs to drop and a map from each fused add/sub pc to its plan. Only
/// adjacent, single-use, same-block pairs are fused — that guarantees `p`/`q`
/// aren't redefined between the mul and the add, so the rewrite is sound.
fn plan_fma(
    prog: &Program,
    f: &FuncMeta,
    end: u32,
    rty: &[JTy],
    leaders: &BTreeSet<u32>,
) -> (std::collections::HashSet<u32>, HashMap<u32, FmaPlan>) {
    let uses = use_counts(prog, f, end);
    let mut skip = std::collections::HashSet::new();
    let mut plan = HashMap::new();
    for pc in f.entry..end {
        let Instr::Bin { op: BinOp::Mul, dst: m, a: p, b: q } = &prog.code[pc as usize] else {
            continue;
        };
        if rty[*m as usize].clif() != types::F64 || uses[*m as usize] != 1 {
            continue;
        }
        let next = pc + 1;
        if next >= end || leaders.contains(&next) {
            continue;
        }
        let Instr::Bin { op, dst: _, a, b } = &prog.code[next as usize] else {
            continue;
        };
        // p*q + r  /  p*q - r  /  r - p*q   (m on either side)
        let entry = match op {
            BinOp::Add if *a == *m => Some((*b, false, false)),
            BinOp::Add if *b == *m => Some((*a, false, false)),
            BinOp::Sub if *a == *m => Some((*b, false, true)), // m - r = p*q + (-r)
            BinOp::Sub if *b == *m => Some((*a, true, false)),  // r - m = (-p)*q + r
            _ => None,
        };
        if let Some((r, neg_p, neg_r)) = entry {
            skip.insert(pc);
            plan.insert(next, FmaPlan { p: *p, q: *q, r, neg_p, neg_r });
        }
    }
    (skip, plan)
}

/// Compute a bounds-checked element address `base + 8*(idx+1)`, aborting via
/// `zipp_oob` if `idx` is out of range (the unsigned compare also catches
/// negatives). Splits the current block into an out-of-bounds path and the
/// in-bounds continuation, and leaves the builder positioned in the latter.
fn checked_addr(
    module: &mut JITModule,
    builder: &mut FunctionBuilder,
    oob: FuncId,
    base: Value,
    idx: Value,
) -> Value {
    let flags = MemFlags::trusted();
    let len = builder.ins().load(types::I64, flags, base, 0);
    let inb = builder.ins().icmp(IntCC::UnsignedLessThan, idx, len);
    let ok = builder.create_block();
    let bad = builder.create_block();
    builder.ins().brif(inb, ok, &[], bad, &[]);
    builder.switch_to_block(bad);
    let f = module.declare_func_in_func(oob, builder.func);
    builder.ins().call(f, &[idx, len]);
    builder.ins().trap(TrapCode::HEAP_OUT_OF_BOUNDS); // zipp_oob aborts; unreachable
    builder.switch_to_block(ok);
    let i1 = builder.ins().iadd_imm(idx, 1);
    let off = builder.ins().imul_imm(i1, 8);
    builder.ins().iadd(base, off)
}

#[allow(clippy::too_many_arguments)]
fn compile_function(
    module: &mut JITModule,
    ctx: &mut cranelift_codegen::Context,
    fctx: &mut FunctionBuilderContext,
    prog: &Program,
    fi: usize,
    end: u32,
    func_ids: &[FuncId],
    rt: &RuntimeIds,
    fast_math: bool,
) -> Result<(), String> {
    let f = &prog.funcs[fi];
    let entry_pc = f.entry;
    let rty = infer_reg_types(prog, f, end);
    let mut builder = FunctionBuilder::new(&mut ctx.func, fctx);

    let mut leaders: BTreeSet<u32> = BTreeSet::new();
    leaders.insert(entry_pc);
    for pc in entry_pc..end {
        match &prog.code[pc as usize] {
            Instr::Jmp { target } => {
                leaders.insert(*target);
                if pc + 1 < end {
                    leaders.insert(pc + 1);
                }
            }
            Instr::JmpIfZero { target, .. } | Instr::JmpIfNonZero { target, .. } => {
                leaders.insert(*target);
                if pc + 1 < end {
                    leaders.insert(pc + 1);
                }
            }
            _ => {}
        }
    }
    let mut blocks: HashMap<u32, Block> = HashMap::new();
    for &pc in &leaders {
        blocks.insert(pc, builder.create_block());
    }

    let (fma_skip, fma_plan) = if fast_math {
        plan_fma(prog, f, end, &rty, &leaders)
    } else {
        (std::collections::HashSet::new(), HashMap::new())
    };
    if fast_math && std::env::var("ZIPP_JIT_DEBUG").is_ok() {
        eprintln!("[jit] fn {}: fused {} fma pair(s)", f.name, fma_plan.len());
    }

    for r in 0..f.nregs {
        builder.declare_var(var(r), rty[r as usize].clif());
    }

    let entry_blk = blocks[&entry_pc];
    builder.append_block_params_for_function_params(entry_blk);
    builder.switch_to_block(entry_blk);
    let params: Vec<Value> = builder.block_params(entry_blk).to_vec();
    for (idx, pv) in params.iter().enumerate() {
        builder.def_var(var(idx as u32), *pv);
    }
    for r in f.nparams..f.nregs {
        let z = if rty[r as usize].clif() == types::F64 {
            builder.ins().f64const(0.0)
        } else {
            builder.ins().iconst(rty[r as usize].clif(), 0) // i32 or i64 zero
        };
        builder.def_var(var(r), z);
    }

    let mut terminated = false;
    for pc in entry_pc..end {
        if pc != entry_pc && leaders.contains(&pc) {
            let nb = blocks[&pc];
            if !terminated {
                builder.ins().jump(nb, &[]);
            }
            builder.switch_to_block(nb);
            terminated = false;
        }
        if terminated {
            continue; // unreachable code between a terminator and the next leader
        }
        if fma_skip.contains(&pc) {
            continue; // mul folded into the following fma
        }
        match &prog.code[pc as usize] {
            Instr::Const { dst, imm } => {
                let c = builder.ins().iconst(types::I64, *imm);
                builder.def_var(var(*dst), c);
            }
            Instr::FConst { dst, imm } => {
                let c = builder.ins().f64const(*imm);
                builder.def_var(var(*dst), c);
            }
            Instr::SConst { dst, imm } => {
                // Bake the literal into a leaked [len|bytes] block now; embed its
                // address as a constant (valid for this JIT process's lifetime).
                let addr = builder.ins().iconst(types::I64, zipp_rt::leak_str_blob(imm));
                builder.def_var(var(*dst), addr);
            }
            Instr::Cast { dst, src, to } => {
                let sval = builder.use_var(var(*src));
                let res = emit_cast(&mut builder, rty[*src as usize], jty_of(*to), sval);
                builder.def_var(var(*dst), res);
            }
            Instr::Mov { dst, src } => {
                let s = builder.use_var(var(*src));
                builder.def_var(var(*dst), s);
            }
            Instr::Bin { op, dst, a, b } => {
                let res = if matches!(rty[*a as usize], JTy::Str) {
                    // String concat (+) / equality (==, !=) via the runtime.
                    let av = builder.use_var(var(*a));
                    let bv = builder.use_var(var(*b));
                    let id = if matches!(op, BinOp::Add) { rt.str_concat } else { rt.str_eq };
                    let f = module.declare_func_in_func(id, builder.func);
                    let call = builder.ins().call(f, &[av, bv]);
                    let r = builder.inst_results(call)[0];
                    if matches!(op, BinOp::Ne) {
                        builder.ins().bxor_imm(r, 1) // !eq
                    } else {
                        r
                    }
                } else if let Some(fp) = fma_plan.get(&pc) {
                    let mut pv = builder.use_var(var(fp.p));
                    let qv = builder.use_var(var(fp.q));
                    let mut rv = builder.use_var(var(fp.r));
                    if fp.neg_p {
                        pv = builder.ins().fneg(pv);
                    }
                    if fp.neg_r {
                        rv = builder.ins().fneg(rv);
                    }
                    builder.ins().fma(pv, qv, rv)
                } else {
                    let av = builder.use_var(var(*a));
                    let bv = builder.use_var(var(*b));
                    emit_bin(&mut builder, *op, av, bv, rty[*a as usize])
                };
                builder.def_var(var(*dst), res);
            }
            Instr::Unary { op, dst, a } => {
                let av = builder.use_var(var(*a));
                let res = match op {
                    UnOp::Neg if rty[*a as usize].clif() == types::F64 => builder.ins().fneg(av),
                    UnOp::Neg => builder.ins().ineg(av),
                    UnOp::BitNot => builder.ins().bnot(av),
                    UnOp::Not => {
                        let c = builder.ins().icmp_imm(IntCC::Equal, av, 0);
                        builder.ins().uextend(types::I64, c)
                    }
                };
                builder.def_var(var(*dst), res);
            }
            Instr::Jmp { target } => {
                builder.ins().jump(blocks[target], &[]);
                terminated = true;
            }
            Instr::JmpIfZero { cond, target } => {
                let c = builder.use_var(var(*cond));
                builder.ins().brif(c, blocks[&(pc + 1)], &[], blocks[target], &[]);
                terminated = true;
            }
            Instr::JmpIfNonZero { cond, target } => {
                let c = builder.use_var(var(*cond));
                builder.ins().brif(c, blocks[target], &[], blocks[&(pc + 1)], &[]);
                terminated = true;
            }
            Instr::Call { func, arg_base, argc, dst } => {
                let callee = module.declare_func_in_func(func_ids[*func as usize], builder.func);
                let args: Vec<Value> =
                    (0..*argc).map(|k| builder.use_var(var(*arg_base + k))).collect();
                let call = builder.ins().call(callee, &args);
                let r = builder.inst_results(call)[0];
                builder.def_var(var(*dst), r);
            }
            Instr::Ret { src } => {
                let s = builder.use_var(var(*src));
                builder.ins().return_(&[s]);
                terminated = true;
            }
            Instr::Print { a } => {
                let aj = rty[*a as usize];
                let mut av = builder.use_var(var(*a));
                // Widen i32/u32 to i64 (sign/zero extend) so the i64 printer is
                // correct; u64 needs the unsigned printer.
                if aj.clif() == types::I32 {
                    av = if aj.signed() {
                        builder.ins().sextend(types::I64, av)
                    } else {
                        builder.ins().uextend(types::I64, av)
                    };
                }
                let id = match aj {
                    JTy::Str => rt.print_str,
                    JTy::F64 => rt.print_f64,
                    JTy::U64 => rt.print_u64,
                    _ => rt.print_i64,
                };
                let p = module.declare_func_in_func(id, builder.func);
                builder.ins().call(p, &[av]);
            }
            Instr::ArrayLit { dst, elems } => {
                let n = builder.ins().iconst(types::I64, elems.len() as i64);
                let alloc = module.declare_func_in_func(rt.alloc, builder.func);
                let call = builder.ins().call(alloc, &[n]);
                let ptr = builder.inst_results(call)[0];
                let elem_f64 = rty[*dst as usize].arr_f64();
                let flags = MemFlags::trusted();
                for (i, e) in elems.iter().enumerate() {
                    let ev = builder.use_var(var(*e));
                    let raw = if elem_f64 {
                        builder.ins().bitcast(types::I64, MemFlags::new(), ev)
                    } else {
                        ev
                    };
                    builder.ins().store(flags, raw, ptr, 8 * (i as i32 + 1));
                }
                builder.def_var(var(*dst), ptr);
            }
            Instr::ArrayRepeat { dst, value, count } => {
                let cv = builder.use_var(var(*count));
                let vv = builder.use_var(var(*value));
                let raw = if rty[*dst as usize].arr_f64() {
                    builder.ins().bitcast(types::I64, MemFlags::new(), vv)
                } else {
                    vv
                };
                let rep = module.declare_func_in_func(rt.array_repeat, builder.func);
                let call = builder.ins().call(rep, &[cv, raw]);
                let ptr = builder.inst_results(call)[0];
                builder.def_var(var(*dst), ptr);
            }
            Instr::Index { dst, arr, idx } => {
                let base = builder.use_var(var(*arr));
                let i = builder.use_var(var(*idx));
                let addr = checked_addr(module, &mut builder, rt.oob, base, i);
                let raw = builder.ins().load(types::I64, MemFlags::trusted(), addr, 0);
                let val = if rty[*arr as usize].arr_f64() {
                    builder.ins().bitcast(types::F64, MemFlags::new(), raw)
                } else {
                    raw
                };
                builder.def_var(var(*dst), val);
            }
            Instr::SetIndex { arr, idx, value } => {
                let base = builder.use_var(var(*arr));
                let i = builder.use_var(var(*idx));
                let vv = builder.use_var(var(*value));
                let raw = if rty[*arr as usize].arr_f64() {
                    builder.ins().bitcast(types::I64, MemFlags::new(), vv)
                } else {
                    vv
                };
                let addr = checked_addr(module, &mut builder, rt.oob, base, i);
                builder.ins().store(MemFlags::trusted(), raw, addr, 0);
            }
            Instr::Len { dst, arr } => {
                let base = builder.use_var(var(*arr));
                let len = builder.ins().load(types::I64, MemFlags::trusted(), base, 0);
                builder.def_var(var(*dst), len);
            }
            Instr::NewStruct { id, dst, fields } => {
                // Reuse the array allocator: zipp_alloc(nfields) gives [n | slots];
                // fields go in the element slots (slot 0's len is unused here).
                let nfields = prog.structs[*id as usize].fields.len();
                let n = builder.ins().iconst(types::I64, nfields as i64);
                let alloc = module.declare_func_in_func(rt.alloc, builder.func);
                let call = builder.ins().call(alloc, &[n]);
                let ptr = builder.inst_results(call)[0];
                let flags = MemFlags::trusted();
                for (i, fr) in fields.iter().enumerate() {
                    let fv = builder.use_var(var(*fr));
                    let raw = if matches!(jty_of(prog.structs[*id as usize].types[i]), JTy::F64) {
                        builder.ins().bitcast(types::I64, MemFlags::new(), fv)
                    } else {
                        fv
                    };
                    builder.ins().store(flags, raw, ptr, 8 * (i as i32 + 1));
                }
                builder.def_var(var(*dst), ptr);
            }
            Instr::GetField { dst, base, field } => {
                let id = rty[*base as usize]
                    .struct_id()
                    .ok_or("internal: GetField on a non-struct register")?;
                let (slot, fty) =
                    struct_field(prog, id, field).ok_or("internal: unknown struct field")?;
                let base_v = builder.use_var(var(*base));
                let raw = builder.ins().load(types::I64, MemFlags::trusted(), base_v, 8 * (slot as i32 + 1));
                let val = if matches!(fty, JTy::F64) {
                    builder.ins().bitcast(types::F64, MemFlags::new(), raw)
                } else {
                    raw
                };
                builder.def_var(var(*dst), val);
            }
            Instr::SetField { base, field, value } => {
                let id = rty[*base as usize]
                    .struct_id()
                    .ok_or("internal: SetField on a non-struct register")?;
                let (slot, fty) =
                    struct_field(prog, id, field).ok_or("internal: unknown struct field")?;
                let base_v = builder.use_var(var(*base));
                let vv = builder.use_var(var(*value));
                let raw = if matches!(fty, JTy::F64) {
                    builder.ins().bitcast(types::I64, MemFlags::new(), vv)
                } else {
                    vv
                };
                builder.ins().store(MemFlags::trusted(), raw, base_v, 8 * (slot as i32 + 1));
            }
            Instr::ConstNull { dst, .. } => {
                // null is a 0 pointer (struct handles are i64 in Cranelift).
                let z = builder.ins().iconst(types::I64, 0);
                builder.def_var(var(*dst), z);
            }
            Instr::Builtin { op, dst, args } => {
                use BuiltinOp::*;
                let f64a = rty[args[0] as usize].clif() == types::F64;
                let res = match op {
                    Abs => {
                        let x = builder.use_var(var(args[0]));
                        if f64a { builder.ins().fabs(x) } else { builder.ins().iabs(x) }
                    }
                    Min => {
                        let (a, b) = (builder.use_var(var(args[0])), builder.use_var(var(args[1])));
                        if f64a { builder.ins().fmin(a, b) } else { builder.ins().smin(a, b) }
                    }
                    Max => {
                        let (a, b) = (builder.use_var(var(args[0])), builder.use_var(var(args[1])));
                        if f64a { builder.ins().fmax(a, b) } else { builder.ins().smax(a, b) }
                    }
                    Pow => {
                        let (a, b) = (builder.use_var(var(args[0])), builder.use_var(var(args[1])));
                        let f = module.declare_func_in_func(rt.ipow, builder.func);
                        let call = builder.ins().call(f, &[a, b]);
                        builder.inst_results(call)[0]
                    }
                    Sqrt => {
                        let x = builder.use_var(var(args[0]));
                        builder.ins().sqrt(x)
                    }
                    Floor => {
                        let x = builder.use_var(var(args[0]));
                        builder.ins().floor(x)
                    }
                    Ceil => {
                        let x = builder.use_var(var(args[0]));
                        builder.ins().ceil(x)
                    }
                };
                builder.def_var(var(*dst), res);
            }
        }
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

/// Numeric cast with Rust `as` semantics (truncate / sign- or zero-extend per
/// source signedness / saturating float↔int), mirroring the interpreter.
fn emit_cast(b: &mut FunctionBuilder, from: JTy, to: JTy, v: Value) -> Value {
    let (fc, tc) = (from.clif(), to.clif());
    if fc == types::F64 && to.is_int() {
        return if to.signed() {
            b.ins().fcvt_to_sint_sat(tc, v)
        } else {
            b.ins().fcvt_to_uint_sat(tc, v)
        };
    }
    if from.is_int() && tc == types::F64 {
        return if from.signed() {
            b.ins().fcvt_from_sint(types::F64, v)
        } else {
            b.ins().fcvt_from_uint(types::F64, v)
        };
    }
    if fc == tc {
        return v; // same clif width: signed↔unsigned reinterpret, or f64↔f64
    }
    if fc == types::I32 && tc == types::I64 {
        if from.signed() {
            b.ins().sextend(types::I64, v)
        } else {
            b.ins().uextend(types::I64, v)
        }
    } else {
        b.ins().ireduce(types::I32, v) // i64 → i32 (truncate low 32)
    }
}

fn emit_bin(bld: &mut FunctionBuilder, op: BinOp, x: Value, y: Value, ty: JTy) -> Value {
    use BinOp::*;
    if ty == JTy::F64 {
        let fc = |bld: &mut FunctionBuilder, cc: FloatCC| {
            let c = bld.ins().fcmp(cc, x, y);
            bld.ins().uextend(types::I64, c)
        };
        return match op {
            Add => bld.ins().fadd(x, y),
            Sub => bld.ins().fsub(x, y),
            Mul => bld.ins().fmul(x, y),
            Div => bld.ins().fdiv(x, y),
            Eq => fc(bld, FloatCC::Equal),
            Ne => fc(bld, FloatCC::NotEqual),
            Lt => fc(bld, FloatCC::LessThan),
            Le => fc(bld, FloatCC::LessThanOrEqual),
            Gt => fc(bld, FloatCC::GreaterThan),
            Ge => fc(bld, FloatCC::GreaterThanOrEqual),
            _ => bld.ins().iconst(types::I64, 0), // mod/bitwise/shift/&&/|| invalid on f64
        };
    }
    let signed = ty.signed();
    let ic = |bld: &mut FunctionBuilder, s: IntCC, u: IntCC| {
        let c = bld.ins().icmp(if signed { s } else { u }, x, y);
        bld.ins().uextend(types::I64, c)
    };
    use IntCC::*;
    match op {
        Add => bld.ins().iadd(x, y),
        Sub => bld.ins().isub(x, y),
        Mul => bld.ins().imul(x, y),
        Div => {
            if signed { bld.ins().sdiv(x, y) } else { bld.ins().udiv(x, y) }
        }
        Mod => {
            if signed { bld.ins().srem(x, y) } else { bld.ins().urem(x, y) }
        }
        BitAnd => bld.ins().band(x, y),
        BitOr => bld.ins().bor(x, y),
        BitXor => bld.ins().bxor(x, y),
        Shl => bld.ins().ishl(x, y), // Cranelift masks the shift mod width
        Shr => {
            if signed { bld.ins().sshr(x, y) } else { bld.ins().ushr(x, y) }
        }
        Eq => ic(bld, Equal, Equal),
        Ne => ic(bld, NotEqual, NotEqual),
        Lt => ic(bld, SignedLessThan, UnsignedLessThan),
        Le => ic(bld, SignedLessThanOrEqual, UnsignedLessThanOrEqual),
        Gt => ic(bld, SignedGreaterThan, UnsignedGreaterThan),
        Ge => ic(bld, SignedGreaterThanOrEqual, UnsignedGreaterThanOrEqual),
        // And/Or only ever see bool (i64 0/1) operands.
        And => {
            let xn = bld.ins().icmp_imm(NotEqual, x, 0);
            let yn = bld.ins().icmp_imm(NotEqual, y, 0);
            let xe = bld.ins().uextend(types::I64, xn);
            let ye = bld.ins().uextend(types::I64, yn);
            bld.ins().band(xe, ye)
        }
        Or => {
            let xn = bld.ins().icmp_imm(NotEqual, x, 0);
            let yn = bld.ins().icmp_imm(NotEqual, y, 0);
            let xe = bld.ins().uextend(types::I64, xn);
            let ye = bld.ins().uextend(types::I64, yn);
            bld.ins().bor(xe, ye)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jit_i64(src: &str) -> i64 {
        let prog = zippc::compile(src).expect("compile");
        match run(&prog).expect("jit run").value {
            JitValue::I64(x) => x,
            other => panic!("expected i64, got {other:?}"),
        }
    }

    /// Compile a TypeScript program (via the oxc frontend) and run it on the JIT.
    fn jit_ts_i64(ts: &str) -> i64 {
        let module = zipp_ts::compile_ts(ts).expect("lower ts");
        let prog = zippc::compile_module(&module).expect("compile");
        match run(&prog).expect("jit run").value {
            JitValue::I64(x) => x,
            other => panic!("expected i64, got {other:?}"),
        }
    }

    #[test]
    fn nullable_structs_run_native() {
        // a nullable param + `if (x !== null)` narrowing + null widening
        let ts = "interface User { age: i64; } \
                  function ageOr(u: User | null, fb: i64): i64 { \
                    if (u !== null) { return u.age; } return fb; \
                  } \
                  function main(): i64 { \
                    let a: User = { age: 30 }; \
                    let some: User | null = a; \
                    let none: User | null = null; \
                    return ageOr(some, -1) + ageOr(none, -1); }";
        assert_eq!(jit_ts_i64(ts), 29); // 30 + (-1)
        // `??` coalesce to a non-null default, then a field read
        let ts2 = "interface Box { v: i64; } \
                   function pick(b: Box | null): i64 { let d: Box = { v: 7 }; return (b ?? d).v; } \
                   function main(): i64 { let x: Box = { v: 100 }; return pick(x) + pick(null); }";
        assert_eq!(jit_ts_i64(ts2), 107); // 100 + 7
        // a null-initialized local, reassigned then narrowed (exercises the id +
        // non-downgrading Mov so the register stays a struct pointer)
        let ts3 = "interface N { v: i64; } \
                   function main(): i64 { \
                     let x: N | null = null; \
                     x = { v: 42 } as N; \
                     if (x !== null) { return x.v; } \
                     return -1; }";
        assert_eq!(jit_ts_i64(ts3), 42);
        // optional chaining `a?.b?.c ?? default`, native on the JIT
        let ts4 = "interface A { zip: i64; } interface U { a: A | null; } \
                   function z(u: U | null): i64 { return u?.a?.zip ?? -1; } \
                   function main(): i64 { let aa: A = { zip: 5 }; let u: U = { a: aa }; return z(u) + z(null); }";
        assert_eq!(jit_ts_i64(ts4), 4); // 5 + (-1)
    }
    fn jit_f64(src: &str) -> f64 {
        let prog = zippc::compile(src).expect("compile");
        match run(&prog).expect("jit run").value {
            JitValue::F64(x) => x,
            other => panic!("expected f64, got {other:?}"),
        }
    }

    #[test]
    fn integer_programs() {
        assert_eq!(jit_i64("fn main(): i64 { return 7 * 6 + 3; }"), 45);
        let fib = "fn fib(n: i64): i64 { if (n < 2) { return n; } return fib(n-1)+fib(n-2); } \
                   fn main(): i64 { return fib(20); }";
        assert_eq!(jit_i64(fib), 6765);
    }

    #[test]
    fn float_programs() {
        assert_eq!(jit_f64("fn main(): f64 { return 1.5 + 2.5 * 2.0; }"), 6.5);
        // i64 <-> f64 casts + a float loop
        let avg = "fn main(): f64 { let s = 0.0; let i = 1; \
                   while (i <= 4) { s = s + f64(i); i = i + 1; } return s / 4.0; }";
        assert_eq!(jit_f64(avg), 2.5);
    }

    #[test]
    fn fast_math_fma_is_correct() {
        // p*q + r and r - p*q with exact values: FMA must match the strict path.
        let prog = zippc::compile("fn main(): f64 { let a = 2.0; let b = 3.0; return a*b + 4.0; }")
            .expect("compile");
        let strict = match run_with(&prog, false).unwrap().value {
            JitValue::F64(x) => x,
            _ => unreachable!(),
        };
        let fused = match run_with(&prog, true).unwrap().value {
            JitValue::F64(x) => x,
            _ => unreachable!(),
        };
        assert_eq!(strict, 10.0);
        assert_eq!(fused, 10.0);
    }

    #[test]
    fn arrays_i64() {
        // literal, index read/write, len, a loop summing an array
        let sum = "fn main(): i64 { \
                     let a = [10, 20, 30, 40]; \
                     a[1] = a[1] + 5; \
                     let s = 0; let i = 0; \
                     while (i < len(a)) { s = s + a[i]; i = i + 1; } \
                     return s; }"; // 10 + 25 + 30 + 40 = 105
        assert_eq!(jit_i64(sum), 105);
    }

    #[test]
    fn arrays_f64_and_repeat() {
        // repeat-init f64 array, write, read back, average
        let avg = "fn main(): f64 { \
                     let a = [0.0; 4]; let i = 0; \
                     while (i < 4) { a[i] = f64(i) + 1.0; i = i + 1; } \
                     let s = 0.0; i = 0; \
                     while (i < len(a)) { s = s + a[i]; i = i + 1; } \
                     return s / f64(len(a)); }"; // (1+2+3+4)/4 = 2.5
        assert_eq!(jit_f64(avg), 2.5);
    }

    #[test]
    fn arrays_across_calls() {
        // array passed to and returned from a function (pointer ABI)
        let src = "fn fill(a: [i64], v: i64): [i64] { \
                       let i = 0; while (i < len(a)) { a[i] = v; i = i + 1; } return a; } \
                   fn main(): i64 { \
                       let a = [0, 0, 0]; a = fill(a, 7); return a[0] + a[2]; }"; // 14
        assert_eq!(jit_i64(src), 14);
    }

    #[test]
    fn strings() {
        // len (byte length), concat, equality (via branches), print
        assert_eq!(jit_i64("fn main(): i64 { let s = \"hello\"; return len(s); }"), 5);
        assert_eq!(jit_i64("fn main(): i64 { let s = \"foo\" + \"bar\"; return len(s); }"), 6);
        let eq = "fn main(): i64 { if (\"ab\" == \"ab\") { return 1; } return 0; }";
        assert_eq!(jit_i64(eq), 1);
        let ne = "fn main(): i64 { if (\"ab\" != \"cd\") { return 1; } return 0; }";
        assert_eq!(jit_i64(ne), 1);
    }

    #[test]
    fn structs() {
        // construct, field read/write, mixed i64/f64 fields, struct param
        let p = "struct P { x: i64, y: i64 } \
                 fn main(): i64 { let p = P { x: 3, y: 4 }; p.y = p.y + 10; return p.x + p.y; }";
        assert_eq!(jit_i64(p), 17);
        let m = "struct V { a: f64, n: i64 } \
                 fn main(): f64 { let v = V { a: 1.5, n: 2 }; v.a = v.a * f64(v.n); return v.a; }";
        assert_eq!(jit_f64(m), 3.0);
        let call = "struct P { x: i64, y: i64 } \
                    fn sm(p: P): i64 { return p.x + p.y; } \
                    fn main(): i64 { let p = P { x: 5, y: 6 }; return sm(p); }";
        assert_eq!(jit_i64(call), 11);
    }

    #[test]
    fn gc_keeps_live_data_under_pressure() {
        // Force frequent collections, then allocate ~200k short-lived arrays
        // while a live accumulator array must survive every collection. If the
        // GC freed a live object the result would be wrong (or it'd crash).
        zipp_rt::gc_set_threshold(64 * 1024);
        let src = "fn main(): i64 { \
                     let acc = [0, 0]; \
                     let i = 0; \
                     while (i < 200000) { \
                         let tmp = [i, i + 1, i + 2]; \
                         acc[0] = acc[0] + tmp[0]; \
                         acc[1] = acc[1] + 1; \
                         i = i + 1; \
                     } \
                     return acc[0] + acc[1]; }";
        // sum_{i=0}^{199999} i = 19999900000, plus acc[1] = 200000
        assert_eq!(jit_i64(src), 19999900000 + 200000);
        let (collections, _live) = zipp_rt::gc_stats();
        assert!(collections > 0, "a 64KB threshold should have forced collections");
    }

    #[test]
    fn sized_ints() {
        // u32 wrapping add (5e9 mod 2^32)
        assert_eq!(
            jit_i64("fn main(): i64 { let a = u32(4000000000); let b = u32(1000000000); return i64(a + b); }"),
            705032704
        );
        // UNSIGNED compare: as i32, 3e9 would be negative; as u32 it's > 1
        assert_eq!(
            jit_i64("fn main(): i64 { let a = u32(3000000000); if (a > u32(1)) { return 1; } return 0; }"),
            1
        );
        // unsigned division
        assert_eq!(
            jit_i64("fn main(): i64 { let a = u32(4000000000); return i64(a / u32(2)); }"),
            2000000000
        );
        // i32 signed overflow wraps to MIN
        assert_eq!(
            jit_i64("fn main(): i64 { let a = i32(2147483647); return i64(a + i32(1)); }"),
            -2147483648
        );
        // narrowing cast keeps the low 32 bits: (2^32 + 1) as u32 = 1
        assert_eq!(jit_i64("fn main(): i64 { return i64(u32(4294967297)); }"), 1);
        // u64 arithmetic
        assert_eq!(
            jit_i64("fn main(): i64 { let a = u64(10000000000); return i64(a * u64(2)); }"),
            20000000000
        );
        // u32 logical shift
        assert_eq!(
            jit_i64("fn main(): i64 { let a = u32(1); return i64(a << u32(31)); }"),
            2147483648
        );
    }

    #[test]
    fn builtins() {
        // the whole language compiles now — math builtins included
        assert_eq!(jit_i64("fn main(): i64 { return abs(0 - 5); }"), 5);
        assert_eq!(jit_i64("fn main(): i64 { return min(3, 7) + max(3, 7); }"), 10);
        assert_eq!(jit_i64("fn main(): i64 { return pow(2, 10); }"), 1024);
        assert_eq!(jit_f64("fn main(): f64 { return sqrt(16.0); }"), 4.0);
        assert_eq!(jit_f64("fn main(): f64 { return floor(3.7) + ceil(3.2); }"), 7.0);
        assert_eq!(jit_f64("fn main(): f64 { return abs(0.0 - 2.5); }"), 2.5);
    }
}
