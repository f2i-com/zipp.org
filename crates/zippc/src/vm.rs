//! Register-machine interpreter (the v0 tier-0 runtime) with optional
//! execution-trace recording for the zk-STARK profile.
//!
//! Values are a tagged `i64`/`f64` union. The zk **provable** profile is
//! integer-only by design (PLAN.md §7 — floats break on-chain determinism), so
//! when trace recording is on, the first `f64` literal/cast returns an error
//! rather than producing an unprovable trace.

use crate::ast::{BinOp, Type, UnOp};
use crate::ir::{BuiltinOp, Instr, Program};

/// A runtime value. `Arr`/`Str` are indices into VM heaps (arrays are reference
/// types — passing or assigning shares them; strings are immutable).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    I64(i64),
    F64(f64),
    Arr(usize),
    Str(usize),
    Struct { id: u32, ptr: usize },
    // Sized integers (reached via casts).
    I32(i32),
    U32(u32),
    U64(u64),
    /// A null struct reference (`T | null` whose value is absent).
    Null,
}

impl Value {
    fn is_zero(self) -> bool {
        match self {
            Value::I64(x) => x == 0,
            Value::I32(x) => x == 0,
            Value::U32(x) => x == 0,
            Value::U64(x) => x == 0,
            Value::F64(f) => f == 0.0,
            Value::Arr(_) | Value::Str(_) | Value::Struct { .. } | Value::Null => false,
        }
    }
    /// The i64 payload, or `None` for non-`i64` values. (Array index / repeat
    /// count are checked to be `i64`, and `--prove` rejects sized ints, so this
    /// is only ever asked of genuine `i64` values.)
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Value::I64(x) => Some(x),
            _ => None,
        }
    }
    /// i64 view for trace columns. Only reached for `I64` under trace recording
    /// (everything else is gated out), so this is exact there.
    fn trace_i64(self) -> i64 {
        match self {
            Value::I64(x) => x,
            Value::F64(f) => f as i64,
            Value::I32(x) => x as i64,
            Value::U32(x) => x as i64,
            Value::U64(x) => x as i64,
            Value::Arr(_) | Value::Str(_) | Value::Struct { .. } | Value::Null => 0,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::I64(x) => write!(f, "{x}"),
            Value::I32(x) => write!(f, "{x}"),
            Value::U32(x) => write!(f, "{x}"),
            Value::U64(x) => write!(f, "{x}"),
            Value::F64(v) => write!(f, "{v}"),
            Value::Arr(_) => write!(f, "[array]"),
            Value::Str(_) => write!(f, "[str]"),
            Value::Struct { .. } => write!(f, "[struct]"),
            Value::Null => write!(f, "null"),
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
    /// Lines emitted by `print`, already rendered to text.
    pub output: Vec<String>,
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
const ZK_NO_ARRAY: &str = "--prove does not support arrays yet (the zk profile is integer-only)";
const ZK_NO_STR: &str = "--prove does not support strings (the zk profile is integer-only)";
const ZK_NO_STRUCT: &str = "--prove does not support structs (the zk profile is integer-only)";

pub fn run(prog: &Program, record_trace: bool) -> Result<RunResult, String> {
    let main = &prog.funcs[prog.main as usize];
    let mut reg: Vec<Value> = vec![Value::I64(0); main.nregs as usize];
    let mut base: usize = 0;
    let mut pc: u32 = main.entry;
    let mut call_stack: Vec<Frame> = Vec::new();
    let mut output: Vec<String> = Vec::new();
    let mut heap: Vec<Vec<Value>> = Vec::new();
    let mut str_heap: Vec<String> = Vec::new();
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
            Instr::SConst { dst, imm } => {
                if record_trace {
                    return Err(ZK_NO_STR.into());
                }
                let i = str_heap.len();
                str_heap.push(imm.clone());
                reg[base + *dst as usize] = Value::Str(i);
            }
            Instr::Cast { dst, src, to } => {
                // The zk profile is i64-only: f64 and sized ints are gated out.
                if record_trace && matches!(to, Type::F64 | Type::I32 | Type::U32 | Type::U64) {
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
                let res = match (av, bv) {
                    // String concatenation / comparison (needs heap access).
                    (Value::Str(x), Value::Str(y)) => match op {
                        BinOp::Add => {
                            let s = format!("{}{}", str_heap[x], str_heap[y]);
                            let i = str_heap.len();
                            str_heap.push(s);
                            Value::Str(i)
                        }
                        BinOp::Eq => Value::I64((str_heap[x] == str_heap[y]) as i64),
                        BinOp::Ne => Value::I64((str_heap[x] != str_heap[y]) as i64),
                        _ => return Err(format!("runtime error: operator {op:?} not valid on strings")),
                    },
                    // Null comparisons: `x === null` / `x !== null` (a non-null
                    // struct is never equal to null; null equals only null).
                    (Value::Null, _) | (_, Value::Null)
                        if matches!(*op, BinOp::Eq | BinOp::Ne) =>
                    {
                        let eq = matches!((av, bv), (Value::Null, Value::Null));
                        Value::I64((if *op == BinOp::Eq { eq } else { !eq }) as i64)
                    }
                    _ => eval_bin(*op, av, bv)?,
                };
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
                output.push(render(v, &str_heap));
                rec!(OpKind::Other, v.trace_i64(), 0, v.trace_i64(), 0);
            }
            Instr::ArrayLit { dst, elems } => {
                if record_trace {
                    return Err(ZK_NO_ARRAY.into());
                }
                let vals: Vec<Value> = elems.iter().map(|r| reg[base + *r as usize]).collect();
                let h = heap.len();
                heap.push(vals);
                reg[base + *dst as usize] = Value::Arr(h);
            }
            Instr::ArrayRepeat { dst, value, count } => {
                if record_trace {
                    return Err(ZK_NO_ARRAY.into());
                }
                let v = reg[base + *value as usize];
                let n = reg[base + *count as usize]
                    .as_i64()
                    .ok_or("runtime error: array length must be i64")?;
                if n < 0 {
                    return Err("runtime error: array length cannot be negative".into());
                }
                let h = heap.len();
                heap.push(vec![v; n as usize]);
                reg[base + *dst as usize] = Value::Arr(h);
            }
            Instr::Index { dst, arr, idx } => {
                if record_trace {
                    return Err(ZK_NO_ARRAY.into());
                }
                let h = array_ref(reg[base + *arr as usize])?;
                let i = reg[base + *idx as usize]
                    .as_i64()
                    .ok_or("runtime error: index must be i64")?;
                let a = &heap[h];
                if i < 0 || i as usize >= a.len() {
                    return Err(format!("runtime error: index {i} out of bounds (len {})", a.len()));
                }
                reg[base + *dst as usize] = a[i as usize];
            }
            Instr::SetIndex { arr, idx, value } => {
                if record_trace {
                    return Err(ZK_NO_ARRAY.into());
                }
                let h = array_ref(reg[base + *arr as usize])?;
                let i = reg[base + *idx as usize]
                    .as_i64()
                    .ok_or("runtime error: index must be i64")?;
                let v = reg[base + *value as usize];
                let len = heap[h].len();
                if i < 0 || i as usize >= len {
                    return Err(format!("runtime error: index {i} out of bounds (len {len})"));
                }
                heap[h][i as usize] = v;
            }
            Instr::Len { dst, arr } => {
                let n = match reg[base + *arr as usize] {
                    Value::Arr(h) => heap[h].len(),
                    Value::Str(s) => str_heap[s].len(),
                    _ => return Err("runtime error: len expects an array or string".into()),
                };
                reg[base + *dst as usize] = Value::I64(n as i64);
            }
            Instr::Builtin { op, dst, args } => {
                let vals: Vec<Value> = args.iter().map(|r| reg[base + *r as usize]).collect();
                let res = eval_builtin(*op, &vals)?;
                reg[base + *dst as usize] = res;
                let a0 = vals.first().map(|v| v.trace_i64()).unwrap_or(0);
                let a1 = vals.get(1).map(|v| v.trace_i64()).unwrap_or(0);
                rec!(OpKind::Other, a0, a1, res.trace_i64(), 0);
            }
            Instr::NewStruct { id, dst, fields } => {
                if record_trace {
                    return Err(ZK_NO_STRUCT.into());
                }
                let vals: Vec<Value> = fields.iter().map(|r| reg[base + *r as usize]).collect();
                let ptr = heap.len();
                heap.push(vals);
                reg[base + *dst as usize] = Value::Struct { id: *id, ptr };
            }
            Instr::GetField { dst, base: obj, field } => {
                if record_trace {
                    return Err(ZK_NO_STRUCT.into());
                }
                let (id, ptr) = match reg[base + *obj as usize] {
                    Value::Struct { id, ptr } => (id, ptr),
                    Value::Null => return Err("runtime error: null reference (read of '.{field}')".replace("{field}", field)),
                    _ => return Err("runtime error: field access on a non-struct".into()),
                };
                let slot = prog.structs[id as usize]
                    .fields
                    .iter()
                    .position(|f| f == field)
                    .ok_or_else(|| format!("runtime error: no field '{field}'"))?;
                reg[base + *dst as usize] = heap[ptr][slot];
            }
            Instr::ConstNull { dst, .. } => {
                if record_trace {
                    return Err(ZK_NO_STRUCT.into());
                }
                reg[base + *dst as usize] = Value::Null;
            }
            Instr::SetField { base: obj, field, value } => {
                if record_trace {
                    return Err(ZK_NO_STRUCT.into());
                }
                let (id, ptr) = match reg[base + *obj as usize] {
                    Value::Struct { id, ptr } => (id, ptr),
                    Value::Null => return Err("runtime error: null reference (write to a field)".into()),
                    _ => return Err("runtime error: field assignment to a non-struct".into()),
                };
                let slot = prog.structs[id as usize]
                    .fields
                    .iter()
                    .position(|f| f == field)
                    .ok_or_else(|| format!("runtime error: no field '{field}'"))?;
                let v = reg[base + *value as usize];
                heap[ptr][slot] = v;
            }
        }
        clk += 1;
    }
}

/// Numeric cast, with Rust `as` semantics (truncate on narrowing, sign/zero
/// extend per source signedness on widening, saturating float→int). The native
/// backends mirror this exactly.
fn cast(v: Value, to: Type) -> Value {
    use Type as T;
    use Value as V;
    // A common i64 view (sign- or zero-extended per source signedness) and an
    // f64 view make the target conversions uniform.
    let as_i64 = match v {
        V::I64(x) => x,
        V::I32(x) => x as i64,
        V::U32(x) => x as i64,
        V::U64(x) => x as i64,
        V::F64(_) => 0, // handled per-target below
        _ => return v,  // non-numeric (checker prevents)
    };
    match to {
        T::F64 => V::F64(match v {
            V::F64(f) => f,
            V::I64(x) => x as f64,
            V::I32(x) => x as f64,
            V::U32(x) => x as f64,
            V::U64(x) => x as f64,
            _ => return v,
        }),
        T::I64 => V::I64(match v {
            V::F64(f) => f as i64,
            _ => as_i64,
        }),
        T::I32 => V::I32(match v {
            V::F64(f) => f as i32,
            _ => as_i64 as i32,
        }),
        T::U32 => V::U32(match v {
            V::F64(f) => f as u32,
            V::U64(x) => x as u32,
            _ => as_i64 as u32,
        }),
        T::U64 => V::U64(match v {
            V::F64(f) => f as u64,
            V::U64(x) => x,
            V::U32(x) => x as u64,
            // signed sources reinterpret their two's-complement bits as u64
            _ => as_i64 as u64,
        }),
        _ => v, // non-numeric target (checker prevents)
    }
}

fn array_ref(v: Value) -> Result<usize, String> {
    match v {
        Value::Arr(h) => Ok(h),
        _ => Err("runtime error: expected an array".into()),
    }
}

/// Resolve a value to printable text (needs the string heap for `Str`).
fn render(v: Value, strs: &[String]) -> String {
    match v {
        Value::I64(x) => x.to_string(),
        Value::I32(x) => x.to_string(),
        Value::U32(x) => x.to_string(),
        Value::U64(x) => x.to_string(),
        Value::F64(f) => f.to_string(),
        Value::Str(i) => strs[i].clone(),
        Value::Arr(_) => "[array]".to_string(),
        Value::Struct { .. } => "[struct]".to_string(),
        Value::Null => "null".to_string(),
    }
}

fn eval_builtin(op: BuiltinOp, args: &[Value]) -> Result<Value, String> {
    use BuiltinOp::*;
    Ok(match op {
        Abs => match args[0] {
            Value::I64(x) => Value::I64(x.wrapping_abs()),
            Value::F64(f) => Value::F64(f.abs()),
            _ => return Err("runtime error: abs expects a number".into()),
        },
        Min => match (args[0], args[1]) {
            (Value::I64(a), Value::I64(b)) => Value::I64(a.min(b)),
            (Value::F64(a), Value::F64(b)) => Value::F64(a.min(b)),
            _ => return Err("runtime error: min expects two numbers of the same type".into()),
        },
        Max => match (args[0], args[1]) {
            (Value::I64(a), Value::I64(b)) => Value::I64(a.max(b)),
            (Value::F64(a), Value::F64(b)) => Value::F64(a.max(b)),
            _ => return Err("runtime error: max expects two numbers of the same type".into()),
        },
        Pow => match (args[0], args[1]) {
            (Value::I64(base), Value::I64(exp)) => {
                if exp < 0 {
                    return Err("runtime error: pow exponent must be >= 0".into());
                }
                Value::I64(base.wrapping_pow(exp as u32))
            }
            _ => return Err("runtime error: pow expects two integers".into()),
        },
        Sqrt => match args[0] {
            Value::F64(f) => Value::F64(f.sqrt()),
            _ => return Err("runtime error: sqrt expects an f64".into()),
        },
        Floor => match args[0] {
            Value::F64(f) => Value::F64(f.floor()),
            _ => return Err("runtime error: floor expects an f64".into()),
        },
        Ceil => match args[0] {
            Value::F64(f) => Value::F64(f.ceil()),
            _ => return Err("runtime error: ceil expects an f64".into()),
        },
    })
}

fn eval_un(op: UnOp, a: Value) -> Result<Value, String> {
    Ok(match (op, a) {
        (UnOp::Neg, Value::I64(x)) => Value::I64(x.wrapping_neg()),
        (UnOp::Neg, Value::I32(x)) => Value::I32(x.wrapping_neg()),
        (UnOp::Neg, Value::U32(x)) => Value::U32(x.wrapping_neg()),
        (UnOp::Neg, Value::U64(x)) => Value::U64(x.wrapping_neg()),
        (UnOp::Neg, Value::F64(f)) => Value::F64(-f),
        (UnOp::Not, Value::I64(x)) => Value::I64((x == 0) as i64),
        (UnOp::BitNot, Value::I64(x)) => Value::I64(!x),
        (UnOp::BitNot, Value::I32(x)) => Value::I32(!x),
        (UnOp::BitNot, Value::U32(x)) => Value::U32(!x),
        (UnOp::BitNot, Value::U64(x)) => Value::U64(!x),
        _ => return Err(format!("runtime error: unary {op:?} on {a:?}")),
    })
}

fn eval_bin(op: BinOp, a: Value, b: Value) -> Result<Value, String> {
    match (a, b) {
        (Value::I64(x), Value::I64(y)) => eval_i64(op, x, y),
        (Value::I32(x), Value::I32(y)) => eval_i32(op, x, y),
        (Value::U32(x), Value::U32(y)) => eval_u32(op, x, y),
        (Value::U64(x), Value::U64(y)) => eval_u64(op, x, y),
        (Value::F64(x), Value::F64(y)) => eval_f64(op, x, y),
        _ => Err("runtime error: mismatched numeric operands (use a cast)".into()),
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

fn eval_i32(op: BinOp, a: i32, b: i32) -> Result<Value, String> {
    use BinOp::*;
    Ok(match op {
        Add => Value::I32(a.wrapping_add(b)),
        Sub => Value::I32(a.wrapping_sub(b)),
        Mul => Value::I32(a.wrapping_mul(b)),
        Div => {
            if b == 0 {
                return Err("runtime error: division by zero".into());
            }
            Value::I32(a.wrapping_div(b))
        }
        Mod => {
            if b == 0 {
                return Err("runtime error: modulo by zero".into());
            }
            Value::I32(a.wrapping_rem(b))
        }
        BitAnd => Value::I32(a & b),
        BitOr => Value::I32(a | b),
        BitXor => Value::I32(a ^ b),
        Shl => Value::I32(a.wrapping_shl(b as u32)),
        Shr => Value::I32(a.wrapping_shr(b as u32)),
        Eq => Value::I64((a == b) as i64),
        Ne => Value::I64((a != b) as i64),
        Lt => Value::I64((a < b) as i64),
        Le => Value::I64((a <= b) as i64),
        Gt => Value::I64((a > b) as i64),
        Ge => Value::I64((a >= b) as i64),
        And | Or => return Err(format!("runtime error: operator {op:?} is not valid on i32")),
    })
}

fn eval_u32(op: BinOp, a: u32, b: u32) -> Result<Value, String> {
    use BinOp::*;
    Ok(match op {
        Add => Value::U32(a.wrapping_add(b)),
        Sub => Value::U32(a.wrapping_sub(b)),
        Mul => Value::U32(a.wrapping_mul(b)),
        Div => {
            if b == 0 {
                return Err("runtime error: division by zero".into());
            }
            Value::U32(a / b)
        }
        Mod => {
            if b == 0 {
                return Err("runtime error: modulo by zero".into());
            }
            Value::U32(a % b)
        }
        BitAnd => Value::U32(a & b),
        BitOr => Value::U32(a | b),
        BitXor => Value::U32(a ^ b),
        Shl => Value::U32(a.wrapping_shl(b)),
        Shr => Value::U32(a.wrapping_shr(b)),
        Eq => Value::I64((a == b) as i64),
        Ne => Value::I64((a != b) as i64),
        Lt => Value::I64((a < b) as i64),
        Le => Value::I64((a <= b) as i64),
        Gt => Value::I64((a > b) as i64),
        Ge => Value::I64((a >= b) as i64),
        And | Or => return Err(format!("runtime error: operator {op:?} is not valid on u32")),
    })
}

fn eval_u64(op: BinOp, a: u64, b: u64) -> Result<Value, String> {
    use BinOp::*;
    Ok(match op {
        Add => Value::U64(a.wrapping_add(b)),
        Sub => Value::U64(a.wrapping_sub(b)),
        Mul => Value::U64(a.wrapping_mul(b)),
        Div => {
            if b == 0 {
                return Err("runtime error: division by zero".into());
            }
            Value::U64(a / b)
        }
        Mod => {
            if b == 0 {
                return Err("runtime error: modulo by zero".into());
            }
            Value::U64(a % b)
        }
        BitAnd => Value::U64(a & b),
        BitOr => Value::U64(a | b),
        BitXor => Value::U64(a ^ b),
        Shl => Value::U64(a.wrapping_shl(b as u32)),
        Shr => Value::U64(a.wrapping_shr(b as u32)),
        Eq => Value::I64((a == b) as i64),
        Ne => Value::I64((a != b) as i64),
        Lt => Value::I64((a < b) as i64),
        Le => Value::I64((a <= b) as i64),
        Gt => Value::I64((a > b) as i64),
        Ge => Value::I64((a >= b) as i64),
        And | Or => return Err(format!("runtime error: operator {op:?} is not valid on u64")),
    })
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
