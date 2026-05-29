//! Register-machine interpreter (the v0 tier-0 runtime) with optional
//! execution-trace recording for the zk-STARK profile.
//!
//! The trace is a flat stream of [`TraceStep`]s — one per executed instruction,
//! with operand *values* resolved. `zipp-zk` proves the arithmetic steps
//! (`Const`/`Add`/`Sub`/`Mul`) of that stream; control/`Other` steps are not yet
//! constrained (see `zipp-zk` docs for the v0 soundness boundary).

use crate::ast::{BinOp, UnOp};
use crate::ir::{Instr, Program};

/// Opcode class as seen by the zk trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Const,
    Add,
    Sub,
    Mul,
    Other,
}

/// One executed instruction, with resolved operand values.
#[derive(Debug, Clone, Copy)]
pub struct TraceStep {
    pub clk: u64,
    pub op: OpKind,
    pub a: i64,
    pub b: i64,
    pub dst: i64,
    pub imm: i64,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub result: i64,
    pub output: Vec<i64>,
    pub trace: Vec<TraceStep>,
}

struct Frame {
    ret_pc: u32,
    base: usize,
    dst: u32,
}

/// Runaway-loop guard for normal execution.
const MAX_STEPS: u64 = 500_000_000;
/// Cap on a recorded trace (proving cost scales with this). Picked so typical
/// demos pad to <= 2^19 rows and prove in well under a second.
pub const MAX_TRACE_STEPS: usize = 1 << 19;

pub fn run(prog: &Program, record_trace: bool) -> Result<RunResult, String> {
    let main = &prog.funcs[prog.main as usize];
    let mut reg: Vec<i64> = vec![0; main.nregs as usize];
    let mut base: usize = 0;
    let mut pc: u32 = main.entry;
    let mut call_stack: Vec<Frame> = Vec::new();
    let mut output: Vec<i64> = Vec::new();
    let mut trace: Vec<TraceStep> = Vec::new();
    let mut clk: u64 = 0;

    macro_rules! rec {
        ($op:expr, $a:expr, $b:expr, $dst:expr, $imm:expr) => {
            if record_trace {
                if trace.len() >= MAX_TRACE_STEPS {
                    return Err(format!(
                        "trace exceeded {} steps — use a smaller input for --prove",
                        MAX_TRACE_STEPS
                    ));
                }
                trace.push(TraceStep {
                    clk,
                    op: $op,
                    a: $a,
                    b: $b,
                    dst: $dst,
                    imm: $imm,
                });
            }
        };
    }

    loop {
        if clk >= MAX_STEPS {
            return Err("execution exceeded step limit (infinite loop?)".into());
        }
        let ins = prog
            .code
            .get(pc as usize)
            .ok_or_else(|| format!("vm error: pc {pc} out of bounds"))?;
        pc += 1;

        match ins {
            Instr::Const { dst, imm } => {
                reg[base + *dst as usize] = *imm;
                rec!(OpKind::Const, 0, 0, *imm, *imm);
            }
            Instr::Mov { dst, src } => {
                let v = reg[base + *src as usize];
                reg[base + *dst as usize] = v;
                rec!(OpKind::Other, v, 0, v, 0);
            }
            Instr::Bin { op, dst, a, b } => {
                let av = reg[base + *a as usize];
                let bv = reg[base + *b as usize];
                let res = eval_bin(*op, av, bv)?;
                reg[base + *dst as usize] = res;
                let kind = match op {
                    BinOp::Add => OpKind::Add,
                    BinOp::Sub => OpKind::Sub,
                    BinOp::Mul => OpKind::Mul,
                    _ => OpKind::Other,
                };
                rec!(kind, av, bv, res, 0);
            }
            Instr::Unary { op, dst, a } => {
                let av = reg[base + *a as usize];
                let res = match op {
                    UnOp::Neg => av.wrapping_neg(),
                    UnOp::Not => (av == 0) as i64,
                };
                reg[base + *dst as usize] = res;
                rec!(OpKind::Other, av, 0, res, 0);
            }
            Instr::Jmp { target } => {
                rec!(OpKind::Other, 0, 0, 0, 0);
                pc = *target;
            }
            Instr::JmpIfZero { cond, target } => {
                let c = reg[base + *cond as usize];
                rec!(OpKind::Other, c, 0, 0, 0);
                if c == 0 {
                    pc = *target;
                }
            }
            Instr::Call { func, arg_base, argc, dst } => {
                let callee = &prog.funcs[*func as usize];
                let new_base = reg.len();
                reg.resize(new_base + callee.nregs as usize, 0);
                for i in 0..*argc as usize {
                    reg[new_base + i] = reg[base + *arg_base as usize + i];
                }
                rec!(OpKind::Other, 0, 0, 0, 0);
                call_stack.push(Frame { ret_pc: pc, base, dst: *dst });
                base = new_base;
                pc = callee.entry;
            }
            Instr::Ret { src } => {
                let v = reg[base + *src as usize];
                rec!(OpKind::Other, v, 0, v, 0);
                reg.truncate(base);
                match call_stack.pop() {
                    None => {
                        return Ok(RunResult { result: v, output, trace });
                    }
                    Some(fr) => {
                        reg[fr.base + fr.dst as usize] = v;
                        base = fr.base;
                        pc = fr.ret_pc;
                    }
                }
            }
            Instr::Print { a } => {
                let v = reg[base + *a as usize];
                output.push(v);
                rec!(OpKind::Other, v, 0, v, 0);
            }
        }
        clk += 1;
    }
}

fn eval_bin(op: BinOp, a: i64, b: i64) -> Result<i64, String> {
    use BinOp::*;
    Ok(match op {
        Add => a.wrapping_add(b),
        Sub => a.wrapping_sub(b),
        Mul => a.wrapping_mul(b),
        Div => {
            if b == 0 {
                return Err("runtime error: division by zero".into());
            }
            a.wrapping_div(b)
        }
        Mod => {
            if b == 0 {
                return Err("runtime error: modulo by zero".into());
            }
            a.wrapping_rem(b)
        }
        Eq => (a == b) as i64,
        Ne => (a != b) as i64,
        Lt => (a < b) as i64,
        Le => (a <= b) as i64,
        Gt => (a > b) as i64,
        Ge => (a >= b) as i64,
        And => ((a != 0) && (b != 0)) as i64,
        Or => ((a != 0) || (b != 0)) as i64,
    })
}
