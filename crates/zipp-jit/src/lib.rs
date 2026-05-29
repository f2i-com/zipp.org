//! ZIPP native backend — a Cranelift JIT for the **integer subset** (PLAN.md
//! tier-0). Programs that only use `i64` (arithmetic, comparison, bitwise,
//! control flow, function calls, `print`) compile to native machine code;
//! anything using f64/arrays/strings/structs/builtins is reported ineligible so
//! the caller can fall back to the interpreter.
//!
//! Each ZIPP function becomes a Cranelift function: registers map to SSA
//! variables, jump targets to basic blocks, and calls to direct calls.

use std::collections::{BTreeSet, HashMap};

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, AbiParam, Block, InstBuilder, Value};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use zippc::ast::{BinOp, UnOp};
use zippc::ir::{Instr, Program};

/// Runtime hook for `print` from JIT'd code.
extern "C" fn zipp_print(x: i64) {
    println!("{x}");
}

/// Reason a program can't be JIT-compiled (integer subset only), or `None`.
pub fn ineligible_reason(prog: &Program) -> Option<&'static str> {
    for ins in &prog.code {
        let bad = match ins {
            Instr::FConst { .. } => "f64",
            Instr::SConst { .. } => "strings",
            Instr::Cast { .. } => "casts",
            Instr::ArrayLit { .. }
            | Instr::ArrayRepeat { .. }
            | Instr::Index { .. }
            | Instr::SetIndex { .. }
            | Instr::Len { .. } => "arrays",
            Instr::Builtin { .. } => "builtins",
            Instr::NewStruct { .. } | Instr::GetField { .. } | Instr::SetField { .. } => "structs",
            _ => continue,
        };
        return Some(bad);
    }
    None
}

fn var(r: u32) -> Variable {
    Variable::from_u32(r)
}

/// JIT-compile and run an eligible program's `main`, returning its i64 result.
pub fn run(prog: &Program) -> Result<i64, String> {
    if let Some(bad) = ineligible_reason(prog) {
        return Err(format!("--jit supports the integer subset only (program uses {bad})"));
    }

    let jit = JITBuilder::new(cranelift_module::default_libcall_names())
        .map_err(|e| format!("jit init failed: {e}"))?;
    let mut jit = jit;
    jit.symbol("zipp_print", zipp_print as *const u8);
    let mut module = JITModule::new(jit);

    let make_sig = |module: &JITModule, nparams: u32| {
        let mut sig = module.make_signature();
        for _ in 0..nparams {
            sig.params.push(AbiParam::new(types::I64));
        }
        sig.returns.push(AbiParam::new(types::I64));
        sig
    };

    // Declare every ZIPP function first so calls can reference them.
    let mut func_ids = Vec::with_capacity(prog.funcs.len());
    for (i, f) in prog.funcs.iter().enumerate() {
        let sig = make_sig(&module, f.nparams);
        let id = module
            .declare_function(&format!("zfn{i}"), Linkage::Local, &sig)
            .map_err(|e| format!("declare {}: {e}", f.name))?;
        func_ids.push(id);
    }
    // The `print` runtime import: (i64) -> ().
    let print_id = {
        let mut sig = module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        module
            .declare_function("zipp_print", Linkage::Import, &sig)
            .map_err(|e| format!("declare print: {e}"))?
    };

    // Define each function.
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    for (i, f) in prog.funcs.iter().enumerate() {
        let end = if i + 1 < prog.funcs.len() {
            prog.funcs[i + 1].entry
        } else {
            prog.code.len() as u32
        };
        ctx.func.signature = make_sig(&module, f.nparams);
        compile_function(&mut module, &mut ctx, &mut fctx, prog, i, end, &func_ids, print_id)?;
        module
            .define_function(func_ids[i], &mut ctx)
            .map_err(|e| format!("jit codegen failed for {}: {e:?}", f.name))?;
        module.clear_context(&mut ctx);
    }
    module
        .finalize_definitions()
        .map_err(|e| format!("finalize failed: {e}"))?;

    let main_ptr = module.get_finalized_function(func_ids[prog.main as usize]);
    // SAFETY: main has signature () -> i64 (the checker guarantees main takes no
    // params), and the code was just finalized by this module.
    let main_fn: extern "C" fn() -> i64 = unsafe { std::mem::transmute(main_ptr) };
    Ok(main_fn())
}

#[allow(clippy::too_many_arguments)]
fn compile_function(
    module: &mut JITModule,
    ctx: &mut cranelift_codegen::Context,
    fctx: &mut FunctionBuilderContext,
    prog: &Program,
    fi: usize,
    end: u32,
    func_ids: &[cranelift_module::FuncId],
    print_id: cranelift_module::FuncId,
) -> Result<(), String> {
    let f = &prog.funcs[fi];
    let entry_pc = f.entry;
    let mut builder = FunctionBuilder::new(&mut ctx.func, fctx);

    // Basic-block leaders: the entry, every branch target, and the instruction
    // after every branch.
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

    for r in 0..f.nregs {
        builder.declare_var(var(r), types::I64);
    }

    let entry_blk = blocks[&entry_pc];
    builder.append_block_params_for_function_params(entry_blk);
    builder.switch_to_block(entry_blk);
    let params: Vec<Value> = builder.block_params(entry_blk).to_vec();
    for (idx, pv) in params.iter().enumerate() {
        builder.def_var(var(idx as u32), *pv);
    }
    let zero = builder.ins().iconst(types::I64, 0);
    for r in f.nparams..f.nregs {
        builder.def_var(var(r), zero);
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
            // Unreachable code between a terminator and the next block leader
            // (e.g. the IR's fallthrough `return` after an explicit return).
            continue;
        }
        match &prog.code[pc as usize] {
            Instr::Const { dst, imm } => {
                let c = builder.ins().iconst(types::I64, *imm);
                builder.def_var(var(*dst), c);
            }
            Instr::Mov { dst, src } => {
                let s = builder.use_var(var(*src));
                builder.def_var(var(*dst), s);
            }
            Instr::Bin { op, dst, a, b } => {
                let av = builder.use_var(var(*a));
                let bv = builder.use_var(var(*b));
                let res = emit_bin(&mut builder, *op, av, bv);
                builder.def_var(var(*dst), res);
            }
            Instr::Unary { op, dst, a } => {
                let av = builder.use_var(var(*a));
                let res = match op {
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
                // brif goes to the first block when cond != 0.
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
                let p = module.declare_func_in_func(print_id, builder.func);
                let av = builder.use_var(var(*a));
                builder.ins().call(p, &[av]);
            }
            other => return Err(format!("internal: ineligible instr reached JIT: {other:?}")),
        }
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jit(src: &str) -> i64 {
        let prog = zippc::compile(src).expect("compile");
        run(&prog).expect("jit run")
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(jit("fn main(): i64 { return 7 * 6 + 3; }"), 45);
        assert_eq!(jit("fn main(): i64 { return (12 & 10) | (1 << 4); }"), 24);
    }

    #[test]
    fn recursion_matches_interpreter() {
        let src = "fn fib(n: i64): i64 { if (n < 2) { return n; } return fib(n-1) + fib(n-2); } \
                   fn main(): i64 { return fib(20); }";
        assert_eq!(jit(src), 6765);
    }

    #[test]
    fn loops_and_branches() {
        assert_eq!(
            jit("fn main(): i64 { let s = 0; let i = 0; while (i < 1000) { if (i % 2 == 0) { s += i; } i += 1; } return s; }"),
            249500
        );
    }

    #[test]
    fn rejects_non_integer_programs() {
        let f = zippc::compile("fn main(): f64 { return 1.5; }").unwrap();
        assert!(run(&f).is_err());
        let a = zippc::compile("fn main(): i64 { let x = [1, 2]; return x[0]; }").unwrap();
        assert!(run(&a).is_err());
    }
}

fn emit_bin(b: &mut FunctionBuilder, op: BinOp, x: Value, y: Value) -> Value {
    use BinOp::*;
    let cmp = |b: &mut FunctionBuilder, cc: IntCC| {
        let c = b.ins().icmp(cc, x, y);
        b.ins().uextend(types::I64, c)
    };
    match op {
        Add => b.ins().iadd(x, y),
        Sub => b.ins().isub(x, y),
        Mul => b.ins().imul(x, y),
        Div => b.ins().sdiv(x, y),
        Mod => b.ins().srem(x, y),
        BitAnd => b.ins().band(x, y),
        BitOr => b.ins().bor(x, y),
        BitXor => b.ins().bxor(x, y),
        Shl => b.ins().ishl(x, y),
        Shr => b.ins().sshr(x, y),
        Eq => cmp(b, IntCC::Equal),
        Ne => cmp(b, IntCC::NotEqual),
        Lt => cmp(b, IntCC::SignedLessThan),
        Le => cmp(b, IntCC::SignedLessThanOrEqual),
        Gt => cmp(b, IntCC::SignedGreaterThan),
        Ge => cmp(b, IntCC::SignedGreaterThanOrEqual),
        // && / || are lowered to branches by the IR, so these are unused; handle
        // anyway as eager 0/1 logic.
        And => {
            let xn = b.ins().icmp_imm(IntCC::NotEqual, x, 0);
            let yn = b.ins().icmp_imm(IntCC::NotEqual, y, 0);
            let xe = b.ins().uextend(types::I64, xn);
            let ye = b.ins().uextend(types::I64, yn);
            b.ins().band(xe, ye)
        }
        Or => {
            let xn = b.ins().icmp_imm(IntCC::NotEqual, x, 0);
            let yn = b.ins().icmp_imm(IntCC::NotEqual, y, 0);
            let xe = b.ins().uextend(types::I64, xn);
            let ye = b.ins().uextend(types::I64, yn);
            b.ins().bor(xe, ye)
        }
    }
}
