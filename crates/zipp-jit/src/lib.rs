//! ZIPP native backend — a Cranelift JIT for the **scalar subset plus 1-D
//! arrays** (`i64`/`f64`, casts, and `[T]` of scalars). Programs using
//! strings/structs/builtins fall back to the interpreter.
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
use zippc::ir::{FuncMeta, Instr, Program};

extern "C" fn zipp_print_i64(x: i64) {
    println!("{x}");
}
extern "C" fn zipp_print_f64(x: f64) {
    println!("{x}");
}

/// Allocate an array of `n` 8-byte slots, length-prefixed, zero-initialized.
/// Leaks (no GC in v0 — the interpreter's heap doesn't free either). Returns the
/// base pointer as an i64. Aborts on a negative/absurd length.
extern "C" fn zipp_alloc(n: i64) -> i64 {
    if n < 0 {
        eprintln!("zipp: array length cannot be negative ({n})");
        std::process::abort();
    }
    let total = (n as usize) + 1;
    let mut v: Vec<i64> = vec![0; total];
    v[0] = n;
    let ptr = v.as_mut_ptr();
    std::mem::forget(v); // leaked until process exit
    ptr as i64
}

/// `[value; n]` — allocate then fill. `val` is the raw 8-byte payload (f64
/// elements are passed bit-reinterpreted by the caller).
extern "C" fn zipp_array_repeat(n: i64, val: i64) -> i64 {
    let base = zipp_alloc(n) as *mut i64;
    // SAFETY: zipp_alloc gave us n+1 valid slots; we fill the n element slots.
    unsafe {
        for i in 0..n {
            *base.add(1 + i as usize) = val;
        }
    }
    base as i64
}

extern "C" fn zipp_oob(idx: i64, len: i64) {
    eprintln!("zipp: array index {idx} out of bounds (len {len})");
    std::process::abort();
}

// Strings: a pointer to an 8-aligned block `[ len:i64 | utf8 bytes… ]`, like an
// array but with packed bytes. Immutable. Literals are baked at compile time
// (`leak_str_blob`); concat allocates a fresh block. All leak (no GC v0).

/// Allocate an 8-aligned `[len|bytes]` string block, return its address.
fn make_str(bytes: &[u8]) -> i64 {
    let nslots = 1 + bytes.len().div_ceil(8);
    let mut v: Vec<i64> = vec![0; nslots];
    v[0] = bytes.len() as i64;
    // SAFETY: v has room for 8 + bytes.len() bytes after the length slot.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (v.as_mut_ptr() as *mut u8).add(8), bytes.len());
    }
    let p = v.as_ptr() as i64;
    std::mem::forget(v); // leaked until process exit
    p
}

/// Bytes slice of a string block (used by concat/eq/print).
unsafe fn str_bytes<'a>(s: i64) -> &'a [u8] {
    let len = *(s as *const i64) as usize;
    std::slice::from_raw_parts((s as *const u8).add(8), len)
}

/// Bake a literal into a leaked `[len|bytes]` block at JIT-compile time; the
/// address is embedded as a constant in the generated code.
fn leak_str_blob(s: &str) -> i64 {
    make_str(s.as_bytes())
}

extern "C" fn zipp_str_concat(a: i64, b: i64) -> i64 {
    // SAFETY: a, b are valid string blocks produced by make_str/leak_str_blob.
    let (sa, sb) = unsafe { (str_bytes(a), str_bytes(b)) };
    let mut bytes = Vec::with_capacity(sa.len() + sb.len());
    bytes.extend_from_slice(sa);
    bytes.extend_from_slice(sb);
    make_str(&bytes)
}

extern "C" fn zipp_str_eq(a: i64, b: i64) -> i64 {
    // SAFETY: as above.
    (unsafe { str_bytes(a) == str_bytes(b) }) as i64
}

extern "C" fn zipp_print_str(s: i64) {
    // SAFETY: as above.
    println!("{}", String::from_utf8_lossy(unsafe { str_bytes(s) }));
}

/// Runtime symbols the JIT calls into.
#[derive(Clone, Copy)]
struct RuntimeIds {
    print_i64: FuncId,
    print_f64: FuncId,
    print_str: FuncId,
    alloc: FuncId,
    array_repeat: FuncId,
    oob: FuncId,
    str_concat: FuncId,
    str_eq: FuncId,
}

/// Result of a JIT'd `main`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JitValue {
    I64(i64),
    F64(f64),
}

impl std::fmt::Display for JitValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JitValue::I64(x) => write!(f, "{x}"),
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

/// The JIT type of a register. Arrays are pointers (Cranelift type `i64`); the
/// bool carried by `Arr` records whether the *elements* are f64 (so loads/stores
/// know whether to bit-reinterpret).
#[derive(Clone, Copy, PartialEq)]
enum JTy {
    I64,
    F64,
    Arr(bool),
    Str,
}

impl JTy {
    fn clif(self) -> ClifType {
        if matches!(self, JTy::F64) {
            types::F64
        } else {
            types::I64 // i64, bool, and array/string pointers
        }
    }
    fn arr_f64(self) -> bool {
        matches!(self, JTy::Arr(true))
    }
}

fn jty_of(t: Type) -> JTy {
    match t {
        Type::F64 => JTy::F64,
        Type::Array(e) => JTy::Arr(matches!(e, Elem::F64)),
        Type::Str => JTy::Str,
        _ => JTy::I64,
    }
}

/// Cranelift type for a ZIPP type (bool / array / string pointers are i64).
fn clif_ty(t: Type) -> ClifType {
    jty_of(t).clif()
}

fn var(r: u32) -> Variable {
    Variable::from_u32(r)
}

/// Reason a program can't be JIT-compiled, or `None`. Only math builtins are
/// left unsupported (everything else — scalars, arrays, strings — compiles).
pub fn ineligible_reason(prog: &Program) -> Option<&'static str> {
    for ins in &prog.code {
        let bad = match ins {
            Instr::Builtin { .. } => "builtins",
            Instr::NewStruct { .. } | Instr::GetField { .. } | Instr::SetField { .. } => "structs",
            _ => continue,
        };
        return Some(bad);
    }
    None
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
    jit.symbol("zipp_print_i64", zipp_print_i64 as *const u8);
    jit.symbol("zipp_print_f64", zipp_print_f64 as *const u8);
    jit.symbol("zipp_print_str", zipp_print_str as *const u8);
    jit.symbol("zipp_alloc", zipp_alloc as *const u8);
    jit.symbol("zipp_array_repeat", zipp_array_repeat as *const u8);
    jit.symbol("zipp_oob", zipp_oob as *const u8);
    jit.symbol("zipp_str_concat", zipp_str_concat as *const u8);
    jit.symbol("zipp_str_eq", zipp_str_eq as *const u8);
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
        print_f64: import(&mut module, "zipp_print_f64", &[types::F64], None)?,
        print_str: import(&mut module, "zipp_print_str", &[types::I64], None)?,
        alloc: import(&mut module, "zipp_alloc", &[types::I64], Some(types::I64))?,
        array_repeat: import(&mut module, "zipp_array_repeat", &[types::I64, types::I64], Some(types::I64))?,
        oob: import(&mut module, "zipp_oob", &[types::I64, types::I64], None)?,
        str_concat: import(&mut module, "zipp_str_concat", &[types::I64, types::I64], Some(types::I64))?,
        str_eq: import(&mut module, "zipp_str_eq", &[types::I64, types::I64], Some(types::I64))?,
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

    let e0 = Instant::now();
    // SAFETY: main takes no params; the signature was just finalized.
    let value = unsafe {
        if main.ret == Type::F64 {
            let f: extern "C" fn() -> f64 = std::mem::transmute(ptr);
            JitValue::F64(f())
        } else {
            let f: extern "C" fn() -> i64 = std::mem::transmute(ptr);
            JitValue::I64(f())
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
            Instr::Mov { dst, src } => t[*dst as usize] = t[*src as usize],
            Instr::Bin { op, dst, a, .. } => {
                t[*dst as usize] = match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => t[*a as usize],
                    _ => JTy::I64, // comparisons (bool), mod/bitwise/shift
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
            builder.ins().iconst(types::I64, 0)
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
                let addr = builder.ins().iconst(types::I64, leak_str_blob(imm));
                builder.def_var(var(*dst), addr);
            }
            Instr::Cast { dst, src, to } => {
                let sval = builder.use_var(var(*src));
                let src_ty = rty[*src as usize].clif();
                let to_ty = clif_ty(*to);
                let res = if src_ty == types::I64 && to_ty == types::F64 {
                    builder.ins().fcvt_from_sint(types::F64, sval)
                } else if src_ty == types::F64 && to_ty == types::I64 {
                    builder.ins().fcvt_to_sint_sat(types::I64, sval)
                } else {
                    sval
                };
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
                    emit_bin(&mut builder, *op, av, bv, rty[*a as usize].clif())
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
                let id = match rty[*a as usize] {
                    JTy::Str => rt.print_str,
                    JTy::F64 => rt.print_f64,
                    _ => rt.print_i64,
                };
                let p = module.declare_func_in_func(id, builder.func);
                let av = builder.use_var(var(*a));
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
            other => return Err(format!("internal: ineligible instr reached JIT: {other:?}")),
        }
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

fn emit_bin(bld: &mut FunctionBuilder, op: BinOp, x: Value, y: Value, ty: ClifType) -> Value {
    use BinOp::*;
    if ty == types::F64 {
        let fc = |bld: &mut FunctionBuilder, cc: FloatCC| {
            let c = bld.ins().fcmp(cc, x, y);
            bld.ins().uextend(types::I64, c)
        };
        match op {
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
        }
    } else {
        let ic = |bld: &mut FunctionBuilder, cc: IntCC| {
            let c = bld.ins().icmp(cc, x, y);
            bld.ins().uextend(types::I64, c)
        };
        match op {
            Add => bld.ins().iadd(x, y),
            Sub => bld.ins().isub(x, y),
            Mul => bld.ins().imul(x, y),
            Div => bld.ins().sdiv(x, y),
            Mod => bld.ins().srem(x, y),
            BitAnd => bld.ins().band(x, y),
            BitOr => bld.ins().bor(x, y),
            BitXor => bld.ins().bxor(x, y),
            Shl => bld.ins().ishl(x, y),
            Shr => bld.ins().sshr(x, y),
            Eq => ic(bld, IntCC::Equal),
            Ne => ic(bld, IntCC::NotEqual),
            Lt => ic(bld, IntCC::SignedLessThan),
            Le => ic(bld, IntCC::SignedLessThanOrEqual),
            Gt => ic(bld, IntCC::SignedGreaterThan),
            Ge => ic(bld, IntCC::SignedGreaterThanOrEqual),
            And => {
                let xn = bld.ins().icmp_imm(IntCC::NotEqual, x, 0);
                let yn = bld.ins().icmp_imm(IntCC::NotEqual, y, 0);
                let xe = bld.ins().uextend(types::I64, xn);
                let ye = bld.ins().uextend(types::I64, yn);
                bld.ins().band(xe, ye)
            }
            Or => {
                let xn = bld.ins().icmp_imm(IntCC::NotEqual, x, 0);
                let yn = bld.ins().icmp_imm(IntCC::NotEqual, y, 0);
                let xe = bld.ins().uextend(types::I64, xn);
                let ye = bld.ins().uextend(types::I64, yn);
                bld.ins().bor(xe, ye)
            }
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
    fn rejects_structs() {
        // arrays + strings now JIT; structs still fall back (next).
        let st = zippc::compile(
            "struct P { x: i64 } fn main(): i64 { let p = P { x: 7 }; return p.x; }",
        )
        .unwrap();
        assert!(run(&st).is_err());
    }
}
