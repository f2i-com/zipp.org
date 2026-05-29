//! Register-machine interpreter (the v0 tier-0 runtime) with optional
//! execution-trace recording for the zk-STARK profile.
//!
//! Values are a tagged `i64`/`f64` union. The zk **provable** profile is
//! integer-only by design (PLAN.md §7 — floats break on-chain determinism), so
//! when trace recording is on, the first `f64` literal/cast returns an error
//! rather than producing an unprovable trace.

use crate::ast::{BinOp, Type, UnOp};
use crate::ir::{Instr, Program};

/// A runtime value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    I64(i64),
    F64(f64),
}

impl Value {
    fn is_zero(self) -> bool {
        match self {
            Value::I64(x) => x == 0,
            Value::F64(f) => f == 0.0,
        }
    }
    /// The i64 payload, or `None` for an `f64`.
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Value::I64(x) => Some(x),
            Value::F64(_) => None,
        }
    }
    /// i64 view for trace columns. Only reached for `I64` under trace recording
    /// (f64 is gated out), so this is exact there.
    fn trace_i64(self) -> i64 {
        match self {
            Value::I64(x) => x,
            Value::F64(f) => f as i64,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::I64(x) => write!(f, "{x}"),
            Value::F64(v) => write!(f, "{v}"),
        }
    }
}

/// Opcode class as seen by the (integer-only) zk trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Const,
    Add,
    Sub,
    Mul,
    Other,
}

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
    pub result: Value,
    pub output: Vec<Value>,
    pub trace: Vec<TraceStep>,
}

struct Frame {
    ret_pc: u32,
    base: usize,
    dst: u32,
}

const MAX_STEPS: u64 = 500_000_000;
pub const MAX_TRACE_STEPS: usize = 1 << 19;

const ZK_INT_ONLY: &str = "--prove is integer-only: this program uses f64 (the zk profile is integer-only by design)";

pub fn run(prog: &Program, record_trace: bool) -> Result<RunResult, String> {
    let main = &prog.funcs[prog.main as usize];
    let mut reg: Vec<Value> = vec![Value::I64(0); main.nregs as usize];
    let mut base: usize = 0;
    let mut pc: u32 = main.entry;
    let mut call_stack: Vec<Frame> = Vec::new();
    let mut output: Vec<Value> = Vec::new();
    let mut trace: Vec<TraceStep> = Vec::new();
    let mut clk: u64 = 0;

    macro_rules! rec {
        ($op:expr, $a:expr, $b:expr, $dst:expr, $imm:expr) => {
            if record_trace {
                if trace.len() >= MAX_TRACE_STEPS {
                    return Err(format!(
                        "trace exceeded {MAX_TRACE_STEPS} steps — use a smaller input for --prove"
                    ));
                }
                trace.push(TraceStep { clk, op: $op, a: $a, b: $b, dst: $dst, imm: $imm });
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
                reg[base + *dst as usize] = Value::I64(*imm);
                rec!(OpKind::Const, 0, 0, *imm, *imm);
            }
            Instr::FConst { dst, imm } => {
                if record_trace {
                    return Err(ZK_INT_ONLY.into());
                }
                reg[base + *dst as usize] = Value::F64(*imm);
            }
            Instr::Cast { dst, src, to } => {
                if record_trace && *to == Type::F64 {
                    return Err(ZK_INT_ONLY.into());
                }
                let v = reg[base + *src as usize];
                let res = cast(v, *to);
                reg[base + *dst as usize] = res;
                rec!(OpKind::Other, v.trace_i64(), 0, res.trace_i64(), 0);
            }
            Instr::Mov { dst, src } => {
                let v = reg[base + *src as usize];
                reg[base + *dst as usize] = v;
                rec!(OpKind::Other, v.trace_i64(), 0, v.trace_i64(), 0);
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
                rec!(kind, av.trace_i64(), bv.trace_i64(), res.trace_i64(), 0);
            }
            Instr::Unary { op, dst, a } => {
                let av = reg[base + *a as usize];
                let res = eval_un(*op, av)?;
                reg[base + *dst as usize] = res;
                rec!(OpKind::Other, av.trace_i64(), 0, res.trace_i64(), 0);
            }
            Instr::Jmp { target } => {
                rec!(OpKind::Other, 0, 0, 0, 0);
                pc = *target;
            }
            Instr::JmpIfZero { cond, target } => {
                let c = reg[base + *cond as usize];
                rec!(OpKind::Other, c.trace_i64(), 0, 0, 0);
                if c.is_zero() {
                    pc = *target;
                }
            }
            Instr::JmpIfNonZero { cond, target } => {
                let c = reg[base + *cond as usize];
                rec!(OpKind::Other, c.trace_i64(), 0, 0, 0);
                if !c.is_zero() {
                    pc = *target;
                }
            }
            Instr::Call { func, arg_base, argc, dst } => {
                let callee = &prog.funcs[*func as usize];
                let new_base = reg.len();
                reg.resize(new_base + callee.nregs as usize, Value::I64(0));
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
                rec!(OpKind::Other, v.trace_i64(), 0, v.trace_i64(), 0);
                reg.truncate(base);
                match call_stack.pop() {
                    None => return Ok(RunResult { result: v, output, trace }),
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
                rec!(OpKind::Other, v.trace_i64(), 0, v.trace_i64(), 0);
            }
        }
        clk += 1;
    }
}

fn cast(v: Value, to: Type) -> Value {
    match (v, to) {
        (Value::I64(x), Type::F64) => Value::F64(x as f64),
        (Value::F64(f), Type::I64) => Value::I64(f as i64),
        // identity (same-type cast); cast-to-bool is rejected by the checker.
        (other, _) => other,
    }
}

fn eval_un(op: UnOp, a: Value) -> Result<Value, String> {
    Ok(match (op, a) {
        (UnOp::Neg, Value::I64(x)) => Value::I64(x.wrapping_neg()),
        (UnOp::Neg, Value::F64(f)) => Value::F64(-f),
        (UnOp::Not, Value::I64(x)) => Value::I64((x == 0) as i64),
        (UnOp::BitNot, Value::I64(x)) => Value::I64(!x),
        _ => return Err(format!("runtime error: unary {op:?} on {a:?}")),
    })
}

fn eval_bin(op: BinOp, a: Value, b: Value) -> Result<Value, String> {
    match (a, b) {
        (Value::I64(x), Value::I64(y)) => eval_i64(op, x, y),
        (Value::F64(x), Value::F64(y)) => eval_f64(op, x, y),
        _ => Err("runtime error: mixed i64/f64 operands (use a cast)".into()),
    }
}

fn eval_i64(op: BinOp, a: i64, b: i64) -> Result<Value, String> {
    use BinOp::*;
    Ok(Value::I64(match op {
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
        BitAnd => a & b,
        BitOr => a | b,
        BitXor => a ^ b,
        Shl => a.wrapping_shl(b as u32),
        Shr => a.wrapping_shr(b as u32),
    }))
}

fn eval_f64(op: BinOp, a: f64, b: f64) -> Result<Value, String> {
    use BinOp::*;
    Ok(match op {
        Add => Value::F64(a + b),
        Sub => Value::F64(a - b),
        Mul => Value::F64(a * b),
        Div => Value::F64(a / b),
        Lt => Value::I64((a < b) as i64),
        Le => Value::I64((a <= b) as i64),
        Gt => Value::I64((a > b) as i64),
        Ge => Value::I64((a >= b) as i64),
        Eq => Value::I64((a == b) as i64),
        Ne => Value::I64((a != b) as i64),
        _ => return Err(format!("runtime error: operator {op:?} is not valid on f64")),
    })
}
