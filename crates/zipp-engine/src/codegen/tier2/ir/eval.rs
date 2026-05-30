//! IR interpreter (phase 2).
//!
//! Executes an [`IrFunction`] directly, producing the same result the
//! register VM would for the corresponding bytecode. Used as:
//!
//! 1. **Semantic validation.** If translator + passes produced IR
//!    that disagrees with bytecode execution, this catches it
//!    immediately.
//! 2. **Differential fuzzing.** A later pass (phase 5 speculation)
//!    rewrites IR in ways that *must* preserve meaning; diff-testing
//!    the interpreter against tier 0 is the standing regression test.
//!
//! ## Scope
//!
//! Handles the same op subset the translator produces in phase 1:
//! arithmetic, comparisons, control flow, globals, constants.
//! Doesn't handle heap objects, calls, or speculation — those come
//! back when their respective phases land.
//!
//! ## What we don't do
//!
//! * No side effects on the real VM heap. Globals are a `Vec<u64>`
//!   owned by the evaluation session; callers pass one in.
//! * No deopt trampoline — `Terminator::Deopt` is evaluated as an
//!   error.
//! * No `CallRuntime` resolution — phase 2 rejects it. Phase 5 will
//!   wire in a callback for runtime helpers as speculation lands.

use std::collections::HashMap;

use super::types::{BlockId, IrFunction, IrOp, Terminator, ValueId};

/// A NaN-boxed-style value used by the interpreter. Mirrors the VM's
/// [`crate::value::Value`] semantics for the subset of types phase 2
/// handles; we don't import the real Value here so the IR interpreter
/// has no runtime surface — making it a clean differential oracle.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum EvalValue {
    Undef,
    Null,
    Bool(bool),
    I32(i32),
    F64(f64),
    /// A raw NaN-boxed bit pattern. Used when we don't know the type
    /// but must round-trip a value through an op chain.
    Raw(u64),
}

impl EvalValue {
    /// Truthiness for branch instructions, following JS ToBoolean
    /// rules over the subset we support.
    pub fn truthy(&self) -> bool {
        match self {
            EvalValue::Undef | EvalValue::Null => false,
            EvalValue::Bool(b) => *b,
            EvalValue::I32(n) => *n != 0,
            EvalValue::F64(f) => *f != 0.0 && !f.is_nan(),
            // Any non-null, non-false raw value is truthy — matches
            // how JS ToBoolean treats objects.
            EvalValue::Raw(_) => true,
        }
    }

    /// Coerce to an i32 for typed arithmetic. Used when the op says
    /// it expects i32 (typed ops) — if we see something else the
    /// caller has bugged the IR, not the interpreter; we surface
    /// `None` rather than silently lie.
    fn as_i32(&self) -> Option<i32> {
        match self {
            EvalValue::I32(v) => Some(*v),
            _ => None,
        }
    }

    /// Coerce to f64. Unlike [`Self::as_i32`] this *does* auto-
    /// convert integer values — useful for generic number ops that
    /// accept either.
    fn as_number(&self) -> Option<f64> {
        match self {
            EvalValue::I32(v) => Some(*v as f64),
            EvalValue::F64(f) => Some(*f),
            EvalValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            EvalValue::Null => Some(0.0),
            EvalValue::Undef => Some(f64::NAN),
            EvalValue::Raw(_) => None,
        }
    }

    /// Wrap an i32 result as the appropriate EvalValue. Used by all
    /// integer arithmetic helpers — if the result fits i32, we keep
    /// it typed; else we fall back to f64.
    fn from_num(f: f64) -> EvalValue {
        if f.is_finite() && f.fract() == 0.0 && f >= i32::MIN as f64 && f <= i32::MAX as f64 {
            EvalValue::I32(f as i32)
        } else {
            EvalValue::F64(f)
        }
    }
}

/// What can go wrong during interpretation.
#[derive(Clone, Debug)]
pub enum EvalError {
    /// An operand wasn't of the expected type. Indicates either a
    /// translator bug or an IR pass that broke type discipline.
    TypeMismatch {
        op: &'static str,
        detail: String,
    },
    /// An op this interpreter doesn't support. Phase 2 deliberately
    /// covers a subset; missing coverage falls through to this.
    Unsupported(&'static str),
    /// `Terminator::Deopt` fired. In tier-0 testing this always
    /// indicates a misapplied speculation — bubble up so the caller's
    /// differential-test harness can flag it.
    Deopt(u32),
    /// `Terminator::Unreachable` fired.
    Unreachable,
    /// A branch or jump whose target is an unknown block id.
    BadTarget(BlockId),
    /// An op referenced a ValueId that wasn't defined in this run.
    UndefinedValue(ValueId),
    /// Iteration count exceeded [`EvalSession::max_steps`]. Guards
    /// against runaway loops when the interpreter is used as a
    /// fuzzing oracle.
    StepLimit(usize),
    /// `CallRuntime` not supported in phase 2.
    CallRuntimeNotImplemented,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::TypeMismatch { op, detail } => {
                write!(f, "type mismatch in {op}: {detail}")
            }
            EvalError::Unsupported(s) => write!(f, "unsupported op for phase-2 interpreter: {s}"),
            EvalError::Deopt(id) => write!(f, "deopt triggered (id {id})"),
            EvalError::Unreachable => f.write_str("unreachable terminator hit"),
            EvalError::BadTarget(b) => write!(f, "branch to unknown block {b}"),
            EvalError::UndefinedValue(v) => write!(f, "reference to undefined {v}"),
            EvalError::StepLimit(n) => write!(f, "exceeded step limit ({n})"),
            EvalError::CallRuntimeNotImplemented => {
                f.write_str("CallRuntime not supported in phase-2 interpreter")
            }
        }
    }
}

impl std::error::Error for EvalError {}

/// A single execution of an [`IrFunction`]. Owns the value map and
/// the globals slice; callers can prime globals for a more realistic
/// execution context.
pub struct EvalSession<'a> {
    pub func: &'a IrFunction,
    pub globals: Vec<EvalValue>,
    /// Cap on total op evaluations. Guards against runaway loops —
    /// for differential tests this should be set generously
    /// (1_000_000 is plenty for any reasonable program).
    pub max_steps: usize,
}

impl<'a> EvalSession<'a> {
    pub fn new(func: &'a IrFunction, num_globals: usize) -> Self {
        Self {
            func,
            globals: vec![EvalValue::Undef; num_globals],
            max_steps: 1_000_000,
        }
    }

    /// Run the function with `entry_args` as the entry block's
    /// parameters. Returns the value the function eventually returns.
    pub fn run(&mut self, entry_args: Vec<EvalValue>) -> Result<EvalValue, EvalError> {
        let mut values: HashMap<ValueId, EvalValue> = HashMap::new();

        // Seed: bind entry block params from entry_args. Missing args
        // default to Undef, matching how the register VM initialises
        // an under-supplied call frame.
        let entry = self
            .func
            .blocks
            .first()
            .expect("ir function must have ≥ 1 block");
        for (i, (vid, _)) in entry.params.iter().enumerate() {
            let v = entry_args.get(i).copied().unwrap_or(EvalValue::Undef);
            values.insert(*vid, v);
        }

        let mut current = BlockId(0);
        let mut steps = 0usize;

        loop {
            let block = self
                .func
                .blocks
                .get(current.0 as usize)
                .ok_or(EvalError::BadTarget(current))?;

            // Straight-line ops.
            for (vid, op) in &block.ops {
                steps += 1;
                if steps > self.max_steps {
                    return Err(EvalError::StepLimit(self.max_steps));
                }
                let result = self.eval_op(op, &values)?;
                if let Some(v) = result {
                    values.insert(*vid, v);
                }
            }

            // Terminator.
            match &block.term {
                Terminator::Return(None) => return Ok(EvalValue::Undef),
                Terminator::Return(Some(v)) => {
                    return values
                        .get(v)
                        .copied()
                        .ok_or(EvalError::UndefinedValue(*v));
                }
                Terminator::Jump(next, args) => {
                    let next_block =
                        self.func.blocks.get(next.0 as usize).ok_or(EvalError::BadTarget(*next))?;
                    if args.len() != next_block.params.len() {
                        return Err(EvalError::TypeMismatch {
                            op: "Jump",
                            detail: format!(
                                "{} args but target {next} expects {}",
                                args.len(),
                                next_block.params.len()
                            ),
                        });
                    }
                    let arg_vals: Vec<EvalValue> = args
                        .iter()
                        .map(|v| values.get(v).copied().ok_or(EvalError::UndefinedValue(*v)))
                        .collect::<Result<_, _>>()?;
                    // Clear old values before binding new params so a
                    // loop that reuses a ValueId across iterations
                    // (which the translator shouldn't produce, but
                    // defensive) can't leak.
                    for (i, (vid, _)) in next_block.params.iter().enumerate() {
                        values.insert(*vid, arg_vals[i]);
                    }
                    current = *next;
                }
                Terminator::Branch {
                    cond,
                    then_block,
                    then_args,
                    else_block,
                    else_args,
                } => {
                    let cv = values.get(cond).copied().ok_or(EvalError::UndefinedValue(*cond))?;
                    let (target, args) = if cv.truthy() {
                        (*then_block, then_args)
                    } else {
                        (*else_block, else_args)
                    };
                    let target_block = self
                        .func
                        .blocks
                        .get(target.0 as usize)
                        .ok_or(EvalError::BadTarget(target))?;
                    if args.len() != target_block.params.len() {
                        return Err(EvalError::TypeMismatch {
                            op: "Branch",
                            detail: format!(
                                "{} args but target {target} expects {}",
                                args.len(),
                                target_block.params.len()
                            ),
                        });
                    }
                    let arg_vals: Vec<EvalValue> = args
                        .iter()
                        .map(|v| values.get(v).copied().ok_or(EvalError::UndefinedValue(*v)))
                        .collect::<Result<_, _>>()?;
                    for (i, (vid, _)) in target_block.params.iter().enumerate() {
                        values.insert(*vid, arg_vals[i]);
                    }
                    current = target;
                }
                Terminator::Deopt(d) => return Err(EvalError::Deopt(d.0)),
                Terminator::Unreachable => return Err(EvalError::Unreachable),
            }
        }
    }

    /// Evaluate a single op. Returns `Some(value)` for non-void ops,
    /// `None` for side-effectful void ops. Error if the op's operand
    /// types don't line up.
    fn eval_op(
        &mut self,
        op: &IrOp,
        values: &HashMap<ValueId, EvalValue>,
    ) -> Result<Option<EvalValue>, EvalError> {
        let get = |v: &ValueId| -> Result<EvalValue, EvalError> {
            values
                .get(v)
                .copied()
                .ok_or(EvalError::UndefinedValue(*v))
        };

        let result = match op {
            // ── Constants ──
            IrOp::ConstI32(v) => EvalValue::I32(*v),
            IrOp::ConstF64(bits) => EvalValue::F64(f64::from_bits(*bits)),
            IrOp::ConstBool(b) => EvalValue::Bool(*b),
            IrOp::ConstNull => EvalValue::Null,
            IrOp::ConstUndef => EvalValue::Undef,
            IrOp::ConstValue(bits) => EvalValue::Raw(*bits),

            IrOp::Copy(v) => get(v)?,

            // LoadReg only appears in the entry block before any
            // bytecode op has executed; phase-2 handles it by
            // returning Undef (equivalent to an uninitialised
            // register in the VM).
            IrOp::LoadReg(_) => EvalValue::Undef,

            // ── Typed i32 arithmetic (wrapping) ──
            IrOp::AddI32(a, b) => i32_binop(&get(a)?, &get(b)?, "AddI32", i32::wrapping_add)?,
            IrOp::SubI32(a, b) => i32_binop(&get(a)?, &get(b)?, "SubI32", i32::wrapping_sub)?,
            IrOp::MulI32(a, b) => i32_binop(&get(a)?, &get(b)?, "MulI32", i32::wrapping_mul)?,
            IrOp::NegI32(v) => i32_unop(&get(v)?, "NegI32", i32::wrapping_neg)?,

            // ── Typed f64 arithmetic ──
            IrOp::AddF64(a, b) => f64_binop(&get(a)?, &get(b)?, "AddF64", |x, y| x + y)?,
            IrOp::SubF64(a, b) => f64_binop(&get(a)?, &get(b)?, "SubF64", |x, y| x - y)?,
            IrOp::MulF64(a, b) => f64_binop(&get(a)?, &get(b)?, "MulF64", |x, y| x * y)?,
            IrOp::DivF64(a, b) => f64_binop(&get(a)?, &get(b)?, "DivF64", |x, y| x / y)?,
            IrOp::NegF64(v) => f64_unop(&get(v)?, "NegF64", |x| -x)?,

            // ── Generic arithmetic ──
            //
            // Matches the VM's behaviour: coerce both to number, add
            // as f64, narrow back to i32 if exact.
            IrOp::AddGeneric(a, b) => generic_number(&get(a)?, &get(b)?, "AddGeneric", |x, y| x + y)?,
            IrOp::SubGeneric(a, b) => generic_number(&get(a)?, &get(b)?, "SubGeneric", |x, y| x - y)?,
            IrOp::MulGeneric(a, b) => generic_number(&get(a)?, &get(b)?, "MulGeneric", |x, y| x * y)?,
            IrOp::DivGeneric(a, b) => generic_number(&get(a)?, &get(b)?, "DivGeneric", |x, y| x / y)?,
            IrOp::ModGeneric(a, b) => generic_number(&get(a)?, &get(b)?, "ModGeneric", |x, y| x % y)?,

            // ── Unary ──
            IrOp::NotBool(v) => match get(v)? {
                EvalValue::Bool(b) => EvalValue::Bool(!b),
                other => EvalValue::Bool(!other.truthy()),
            },

            // ── Comparisons (typed) ──
            IrOp::EqI32(a, b) => i32_cmp(&get(a)?, &get(b)?, "EqI32", |x, y| x == y)?,
            IrOp::NeI32(a, b) => i32_cmp(&get(a)?, &get(b)?, "NeI32", |x, y| x != y)?,
            IrOp::LtI32(a, b) => i32_cmp(&get(a)?, &get(b)?, "LtI32", |x, y| x < y)?,
            IrOp::LeI32(a, b) => i32_cmp(&get(a)?, &get(b)?, "LeI32", |x, y| x <= y)?,
            IrOp::GtI32(a, b) => i32_cmp(&get(a)?, &get(b)?, "GtI32", |x, y| x > y)?,
            IrOp::GeI32(a, b) => i32_cmp(&get(a)?, &get(b)?, "GeI32", |x, y| x >= y)?,

            // ── Comparisons (generic) ──
            IrOp::EqValue(a, b) | IrOp::LooseEqValue(a, b) => {
                // Strict and loose equality differ on undef/null and
                // cross-type coercion. For phase 2 — where we don't
                // have strings or objects — they're equivalent for
                // the supported type set.
                EvalValue::Bool(values_eq(&get(a)?, &get(b)?))
            }
            IrOp::NeValue(a, b) => EvalValue::Bool(!values_eq(&get(a)?, &get(b)?)),
            IrOp::LtValue(a, b) => value_cmp(&get(a)?, &get(b)?, |x, y| x < y)?,
            IrOp::LeValue(a, b) => value_cmp(&get(a)?, &get(b)?, |x, y| x <= y)?,

            // ── Globals ──
            IrOp::LoadGlobal(idx) => self
                .globals
                .get(*idx as usize)
                .copied()
                .unwrap_or(EvalValue::Undef),
            IrOp::StoreGlobal(idx, src) => {
                let v = get(src)?;
                if (*idx as usize) < self.globals.len() {
                    self.globals[*idx as usize] = v;
                } else {
                    // Auto-grow — the bytecode VM does similar.
                    self.globals.resize(*idx as usize + 1, EvalValue::Undef);
                    self.globals[*idx as usize] = v;
                }
                return Ok(None);
            }

            // ── Boxing / unboxing passes through as-is for phase 2 ──
            IrOp::BoxI32(v) | IrOp::BoxF64(v) | IrOp::BoxBool(v) => get(v)?,
            IrOp::UnboxI32(v) | IrOp::UnboxF64(v) | IrOp::UnboxBool(v) => get(v)?,

            // ── Speculative checks are pass-through in phase 2 ──
            // They'd normally deopt on type mismatch; without real
            // feedback we just trust the caller.
            IrOp::CheckI32(v, _)
            | IrOp::CheckF64(v, _)
            | IrOp::CheckHeap(v, _)
            | IrOp::CheckHeapShape(v, _, _)
            | IrOp::CheckFunctionIs(v, _, _) => get(v)?,

            // Checked arithmetic: honour the overflow check — if it
            // fires, return the deopt as an error.
            IrOp::CheckedAddI32(a, b, d) => match (get(a)?, get(b)?) {
                (EvalValue::I32(x), EvalValue::I32(y)) => match x.checked_add(y) {
                    Some(sum) => EvalValue::I32(sum),
                    None => return Err(EvalError::Deopt(d.0)),
                },
                _ => {
                    return Err(EvalError::TypeMismatch {
                        op: "CheckedAddI32",
                        detail: "non-i32 operand".into(),
                    })
                }
            },
            IrOp::CheckedSubI32(a, b, d) => match (get(a)?, get(b)?) {
                (EvalValue::I32(x), EvalValue::I32(y)) => match x.checked_sub(y) {
                    Some(v) => EvalValue::I32(v),
                    None => return Err(EvalError::Deopt(d.0)),
                },
                _ => {
                    return Err(EvalError::TypeMismatch {
                        op: "CheckedSubI32",
                        detail: "non-i32 operand".into(),
                    })
                }
            },
            IrOp::CheckedMulI32(a, b, d) => match (get(a)?, get(b)?) {
                (EvalValue::I32(x), EvalValue::I32(y)) => match x.checked_mul(y) {
                    Some(v) => EvalValue::I32(v),
                    None => return Err(EvalError::Deopt(d.0)),
                },
                _ => {
                    return Err(EvalError::TypeMismatch {
                        op: "CheckedMulI32",
                        detail: "non-i32 operand".into(),
                    })
                }
            },

            // Ops phase 2 deliberately doesn't handle.
            IrOp::CallRuntime(_, _) => return Err(EvalError::CallRuntimeNotImplemented),
            IrOp::CallValue(_, _) => return Err(EvalError::CallRuntimeNotImplemented),
            IrOp::MakeClosureNoCapture(_) => {
                return Err(EvalError::Unsupported("MakeClosureNoCapture"))
            }
            IrOp::LoadSlot(_, _, _) | IrOp::StoreSlot(_, _, _) => {
                return Err(EvalError::Unsupported("heap slots"))
            }
        };

        Ok(Some(result))
    }
}

// ─── Arithmetic helpers ─────────────────────────────────────────────────────

fn i32_binop(
    a: &EvalValue,
    b: &EvalValue,
    name: &'static str,
    f: fn(i32, i32) -> i32,
) -> Result<EvalValue, EvalError> {
    match (a.as_i32(), b.as_i32()) {
        (Some(x), Some(y)) => Ok(EvalValue::I32(f(x, y))),
        _ => Err(EvalError::TypeMismatch {
            op: name,
            detail: format!("{a:?}, {b:?}"),
        }),
    }
}

fn i32_unop(
    v: &EvalValue,
    name: &'static str,
    f: fn(i32) -> i32,
) -> Result<EvalValue, EvalError> {
    match v.as_i32() {
        Some(x) => Ok(EvalValue::I32(f(x))),
        None => Err(EvalError::TypeMismatch {
            op: name,
            detail: format!("{v:?}"),
        }),
    }
}

fn f64_binop(
    a: &EvalValue,
    b: &EvalValue,
    name: &'static str,
    f: fn(f64, f64) -> f64,
) -> Result<EvalValue, EvalError> {
    match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) => Ok(EvalValue::F64(f(x, y))),
        _ => Err(EvalError::TypeMismatch {
            op: name,
            detail: format!("{a:?}, {b:?}"),
        }),
    }
}

fn f64_unop(
    v: &EvalValue,
    name: &'static str,
    f: fn(f64) -> f64,
) -> Result<EvalValue, EvalError> {
    match v.as_number() {
        Some(x) => Ok(EvalValue::F64(f(x))),
        None => Err(EvalError::TypeMismatch {
            op: name,
            detail: format!("{v:?}"),
        }),
    }
}

fn generic_number(
    a: &EvalValue,
    b: &EvalValue,
    name: &'static str,
    f: fn(f64, f64) -> f64,
) -> Result<EvalValue, EvalError> {
    let x = a.as_number().ok_or_else(|| EvalError::TypeMismatch {
        op: name,
        detail: format!("{a:?}"),
    })?;
    let y = b.as_number().ok_or_else(|| EvalError::TypeMismatch {
        op: name,
        detail: format!("{b:?}"),
    })?;
    Ok(EvalValue::from_num(f(x, y)))
}

fn i32_cmp(
    a: &EvalValue,
    b: &EvalValue,
    name: &'static str,
    f: fn(i32, i32) -> bool,
) -> Result<EvalValue, EvalError> {
    match (a.as_i32(), b.as_i32()) {
        (Some(x), Some(y)) => Ok(EvalValue::Bool(f(x, y))),
        _ => Err(EvalError::TypeMismatch {
            op: name,
            detail: format!("{a:?}, {b:?}"),
        }),
    }
}

fn value_cmp(
    a: &EvalValue,
    b: &EvalValue,
    f: fn(f64, f64) -> bool,
) -> Result<EvalValue, EvalError> {
    match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) => Ok(EvalValue::Bool(f(x, y))),
        // Mixed-type comparisons involving NaN-boxed raw values
        // default to `false`, matching the VM's conservative handling.
        _ => Ok(EvalValue::Bool(false)),
    }
}

fn values_eq(a: &EvalValue, b: &EvalValue) -> bool {
    match (a, b) {
        (EvalValue::I32(x), EvalValue::I32(y)) => x == y,
        (EvalValue::F64(x), EvalValue::F64(y)) => x == y,
        (EvalValue::I32(x), EvalValue::F64(y)) | (EvalValue::F64(y), EvalValue::I32(x)) => {
            (*x as f64) == *y
        }
        (EvalValue::Bool(x), EvalValue::Bool(y)) => x == y,
        (EvalValue::Null, EvalValue::Null) => true,
        (EvalValue::Undef, EvalValue::Undef) => true,
        (EvalValue::Raw(x), EvalValue::Raw(y)) => x == y,
        _ => false,
    }
}
