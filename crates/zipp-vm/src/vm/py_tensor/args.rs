//! The arguments and answers of a native tensor kernel call, and the views
//! they carry: what the engine hands the kernels, in-process (`kernels`) or
//! across modules to the torch package's WebAssembly module (`wire`).
#![allow(dead_code)]

/// `all_finite`: answers a bool, or null when it declines.
pub(crate) const OP_ALL_FINITE: u32 = 11;

// TypedArray kinds (`native::TA_KINDS`) a tensor storage can be. An int8
// storage (Int8Array) reaches only the quantization kernels
// (`pt_view_q`); every other kernel declines it.
pub(crate) const KIND_I8: u8 = 0;
pub(crate) const KIND_U8: u8 = 1;
pub(crate) const KIND_U16: u8 = 4;
pub(crate) const KIND_F32: u8 = 7;
pub(crate) const KIND_F64: u8 = 8;
pub(crate) const KIND_F16: u8 = 11;

/// A typed-array view: its buffer's heap index, element kind, byte offset
/// and element count.
#[derive(Clone, Copy, Debug)]
pub(crate) struct View {
    pub(crate) buffer: u32,
    pub(crate) kind: u8,
    pub(crate) offset: usize,
    pub(crate) len: usize,
}

impl View {
    pub(crate) fn size(self) -> usize {
        match self.kind {
            KIND_I8 | KIND_U8 => 1,
            KIND_U16 | KIND_F16 => 2,
            KIND_F32 => 4,
            _ => 8,
        }
    }
}

/// One argument of a kernel call.
#[derive(Clone, Debug)]
pub(crate) enum Arg {
    /// A number (an int or a float).
    Num(f64),
    /// A live typed-array view (its kind is not yet checked).
    View(View),
    /// A JavaScript array: its items as indices, or `None` when one is not
    /// a non-negative safe integer.
    Ints(Option<Vec<usize>>),
    /// `true` / `false`.
    Bool(bool),
    Null,
    Undefined,
    /// A BigInt the value word holds, by its truthiness (the only use).
    SmallBigInt(bool),
    /// Anything else (a detached or out-of-range view included).
    Other,
}

impl Arg {
    pub(crate) fn num(&self) -> Option<f64> {
        match self {
            Arg::Num(x) => Some(*x),
            _ => None,
        }
    }
    pub(crate) fn is_number(&self) -> bool {
        matches!(self, Arg::Num(_))
    }
    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Arg::Null)
    }
    pub(crate) fn is_bool(&self) -> bool {
        matches!(self, Arg::Bool(_))
    }
    pub(crate) fn as_bool(&self) -> bool {
        matches!(self, Arg::Bool(true))
    }
    /// `Value::truthy_primitive`: the truthiness of a primitive, `None` for
    /// an object.
    pub(crate) fn truthy_primitive(&self) -> Option<bool> {
        match *self {
            Arg::Num(x) => Some(x != 0.0 && !x.is_nan()),
            Arg::Bool(b) | Arg::SmallBigInt(b) => Some(b),
            Arg::Null | Arg::Undefined => Some(false),
            Arg::View(_) | Arg::Ints(_) | Arg::Other => None,
        }
    }
}

/// What a kernel call answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// It wrote its outputs.
    Done,
    /// It declined; the runtime runs its own loop.
    Declined,
    /// `OP_ALL_FINITE`'s answer.
    Answer(bool),
    /// `OP_ALL_FINITE` declined.
    Null,
}

impl Outcome {
    pub(crate) fn ran(done: bool) -> Outcome {
        if done {
            Outcome::Done
        } else {
            Outcome::Declined
        }
    }
}

/// A non-negative safe-integer number, as an index.
pub(crate) fn as_index(v: &Arg) -> Option<usize> {
    let x = v.num()?;
    if !(0.0..=9007199254740991.0).contains(&x) || x.fract() != 0.0 {
        return None;
    }
    usize::try_from(x as u64).ok()
}
