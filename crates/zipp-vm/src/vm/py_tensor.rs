//! Native loops behind the Python runtime's `_zipp_tensor` kernels.
//!
//! The bundled `torch` stores tensors in JavaScript typed arrays and runs its
//! kernels as JavaScript (`frontend/python/runtime/tensor.js`). Python states
//! run those on the interpreter — always in the wasm build — where a float32
//! matmul costs ~50 ns per multiply-add. This module runs the heavy loops in
//! Rust instead, over the same typed arrays.
//!
//! # Reachability
//!
//! One native, [`native::PY_TENSOR`], bound by slot to the reserved global
//! `__zipp_py_native` only in a program the Python frontend built
//! (`Program::python_natives`). It is not in `builtin_globals` and is hidden
//! from `globalThis` reflection, so ordinary JavaScript cannot name it.
//! `tensor.js` calls it as `__zipp_py_native(op, ...)`.
//!
//! # Contract: same values, or decline
//!
//! Every kernel computes exactly what the JavaScript loop it replaces
//! computes: the same f64 operations in the same order, rounding to the
//! destination's element type exactly where a typed-array store would (once
//! on store for a local accumulator, on every `+=` into a typed array), and
//! the same `Math` functions (`helpers_num2::math_unary`, the interpreter's
//! own). A kernel returns `true` when it wrote its outputs and `false` when it
//! declined, leaving them untouched; the runtime then runs its own loop.
//! It declines on anything it does not recognise — an argument that is not a
//! Float32Array/Float64Array/Float16Array/Uint8Array/Uint16Array view (a
//! float16 storage is a Float16Array; a bfloat16 one is a Uint16Array of
//! bits, which the runtime only hands over to copy), a detached or out-of-range
//! view, an output sharing a buffer with an input, sizes that overflow — so
//! the JavaScript loop stays the reference for every odd case, errors
//! included.
//!
//! # Budget
//!
//! A kernel is priced at [`STEPS_PER_UNIT`] instruction steps per unit of
//! work (a multiply-add, an element) plus [`STEPS_BASE`], below what the
//! interpreted loop costs. It runs only if the whole price fits the budget
//! (`Vm::native_kernel_admits`) and is charged after it completes; otherwise
//! it declines and the interpreted loop spends the budget exactly as it did
//! before these kernels existed. It also declines while a trace is recorded,
//! and polls the host abort flag between blocks of work.
//!
//! # Where the kernels run
//!
//! The kernels (`kernels`, `fft`, `linalg`, `quant`, `sparse`, `jsmath`)
//! name nothing of the engine: they run over a `kernels::Host`. With the
//! torch package built in they run here, over the VM's heap. The WebAssembly
//! `python` artifact (`python-no-torch`) has no kernels: the torch package
//! brings the same source compiled into its own module (zipp_torch.wasm), and
//! each call goes there as bytes (`wire`) through the host function the host
//! registered (`python_packages::set_kernel_bridge`). Either way the same
//! code computes over the same bytes, with the same budget decisions.
mod args;
#[cfg(not(feature = "python-no-torch"))]
mod fft;
#[cfg(not(feature = "python-no-torch"))]
mod jsmath;
#[cfg(not(feature = "python-no-torch"))]
mod kernels;
#[cfg(not(feature = "python-no-torch"))]
mod linalg;
#[cfg(not(feature = "python-no-torch"))]
mod quant;
#[cfg(not(feature = "python-no-torch"))]
mod sparse;
#[cfg(feature = "python-no-torch")]
mod wire;

use super::{native, Thrown, Vm};
use crate::heap::HeapObj;
use crate::value::Value;
use args::{Arg, Outcome, View};

#[cfg(not(feature = "python-no-torch"))]
impl kernels::Host for Vm<'_> {
    fn bytes(&self, buffer: u32) -> Option<&[u8]> {
        match self.heap.get(buffer) {
            HeapObj::ArrayBuffer { data, detached } if !*detached => Some(data),
            _ => None,
        }
    }
    fn bytes_mut(&mut self, buffer: u32) -> Option<&mut [u8]> {
        match self.heap.get_mut(buffer) {
            HeapObj::ArrayBuffer { data, detached } if !*detached => Some(data),
            _ => None,
        }
    }
    fn admits(&self, cost: u64, transient: usize) -> bool {
        self.native_kernel_admits(cost, transient)
    }
    fn charge(&mut self, cost: u64) {
        self.charge_steps(i64::try_from(cost).unwrap_or(i64::MAX));
    }
    fn interrupted(&self) -> bool {
        self.native_kernel_interrupted()
    }
}

impl Vm<'_> {
    /// `__zipp_py_native(op, ...)`. `true`/`false` for a kernel that ran or
    /// declined; `OP_ALL_FINITE` answers `true`/`false`, or `null` when it
    /// declines.
    pub(crate) fn py_tensor(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let op = args.first().map(|&v| self.pt_arg(v)).as_ref().and_then(args::as_index);
        let Some(op) = op else {
            return Ok(Value::bool(false));
        };
        let op = u32::try_from(op).unwrap_or(0);
        let a: Vec<Arg> = args[1..].iter().map(|&v| self.pt_arg(v)).collect();
        #[cfg(not(feature = "python-no-torch"))]
        let outcome = kernels::run(self, op, &a);
        #[cfg(feature = "python-no-torch")]
        let outcome = self.pt_bridge(op, &a);
        Ok(match outcome {
            Outcome::Done => Value::bool(true),
            Outcome::Declined => Value::bool(false),
            Outcome::Answer(b) => Value::bool(b),
            Outcome::Null => Value::NULL,
        })
    }

    /// An argument as the kernels see it: a number, a live typed-array view
    /// (buffer, kind, byte offset, element count), a dense array's items as
    /// indices, or nothing they use.
    fn pt_arg(&self, v: Value) -> Arg {
        if v.is_number() {
            return Arg::Num(v.as_f64());
        }
        if v.is_bool() {
            return Arg::Bool(v.as_bool());
        }
        if v.is_null() {
            return Arg::Null;
        }
        if v.is_undefined() {
            return Arg::Undefined;
        }
        if !v.is_heap() {
            return Arg::Other;
        }
        let idx = v.heap_index();
        match self.heap.get(idx) {
            &HeapObj::TypedArray {
                buffer,
                kind,
                byte_offset,
                ..
            } => match self.ta_effective_len(idx) {
                Some(len) => Arg::View(View {
                    buffer,
                    kind,
                    offset: byte_offset,
                    len,
                }),
                None => Arg::Other,
            },
            HeapObj::Array(items) => {
                let mut out = Vec::new();
                if out.try_reserve_exact(items.len()).is_err() {
                    return Arg::Ints(None);
                }
                for &item in items.iter() {
                    match args::as_index(&if item.is_number() { Arg::Num(item.as_f64()) } else { Arg::Other }) {
                        Some(i) => out.push(i),
                        None => return Arg::Ints(None),
                    }
                }
                Arg::Ints(Some(out))
            }
            _ => Arg::Other,
        }
    }
}

#[cfg(feature = "python-no-torch")]
impl Vm<'_> {
    /// Array buffer `id`'s bytes, or `None` when it is detached.
    fn pt_bytes(&self, id: u32) -> Option<&[u8]> {
        match self.heap.get(id) {
            HeapObj::ArrayBuffer { data, detached } if !*detached => Some(data),
            _ => None,
        }
    }

    /// `op(a)` in the torch package's module (see the module docs): the
    /// request out through the host's kernel bridge, the written bytes and
    /// the charge back. Without a bridge, or when the call fails, the kernel
    /// declines and the runtime runs its own loop.
    fn pt_bridge(&mut self, op: u32, a: &[Arg]) -> Outcome {
        let declined = if op == args::OP_ALL_FINITE { Outcome::Null } else { Outcome::Declined };
        let Some(bridge) = crate::python_packages::kernel_bridge() else {
            return declined;
        };
        let budget = match self.native_kernel_budget() {
            None => wire::Budget {
                unlimited: true,
                open: true,
                heap_room: u64::MAX,
                steps_left: None,
            },
            Some((open, room, steps_left)) => wire::Budget {
                unlimited: false,
                open,
                heap_room: if room == usize::MAX { u64::MAX } else { room as u64 },
                steps_left,
            },
        };
        let request = wire::encode_request(op, a, budget, |id| self.pt_bytes(id));
        let Some(response) = bridge(&request) else {
            return declined;
        };
        let Some(response) = wire::decode_response(&response) else {
            return declined;
        };
        for (id, lo, bytes) in response.written {
            if let HeapObj::ArrayBuffer { data, detached: false } = self.heap.get_mut(id) {
                if let Some(dst) = lo.checked_add(bytes.len()).and_then(|hi| data.get_mut(lo..hi)) {
                    dst.copy_from_slice(bytes);
                }
            }
        }
        if response.charged > 0 {
            self.charge_steps(i64::try_from(response.charged).unwrap_or(i64::MAX));
        }
        response.outcome
    }
}

#[allow(dead_code)]
const _: () = assert!(native::PY_TENSOR != native::HOST_CALL);

#[cfg(all(test, not(feature = "python-no-torch")))]
mod tests {
    use super::jsmath;
    use crate::bytecode::MathFn;

    /// Values across every regime the functions branch on: signed zeros,
    /// subnormals, the float16 range edges, integers near 2^53, and a sweep.
    fn samples() -> Vec<f64> {
        let mut v = vec![
            0.0, -0.0, 0.5, -0.5, 1.0, -1.0, 2.0, 0.999999, -0.9999999999999983, 1e-300, -1e-300, 5e-324,
            6.1e-5, 6.0e-8, 65504.0, 65519.99, 65520.0, 1e300, 268_435_456.0, 4503599627370496.5,
            9007199254740993.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN, f64::MAX, f64::MIN_POSITIVE,
            std::f64::consts::PI, 255.5, 256.0, -1.5, 65535.7, 70000.0,
        ];
        let mut x = 1.0e-12f64;
        while x < 1.0e12 {
            v.push(x);
            v.push(-x);
            v.push(x * 1.37);
            x *= 1.618;
        }
        v
    }

    /// `jsmath` copies the interpreter's own functions so the torch
    /// package's WebAssembly module computes the same bits; any drift
    /// between a copy and its original fails here.
    #[test]
    fn jsmath_matches_the_interpreter() {
        use jsmath::M;
        let pairs = [
            (M::Abs, MathFn::Abs),
            (M::Acos, MathFn::Acos),
            (M::Acosh, MathFn::Acosh),
            (M::Asin, MathFn::Asin),
            (M::Asinh, MathFn::Asinh),
            (M::Atan, MathFn::Atan),
            (M::Atanh, MathFn::Atanh),
            (M::Ceil, MathFn::Ceil),
            (M::Cos, MathFn::Cos),
            (M::Cosh, MathFn::Cosh),
            (M::Exp, MathFn::Exp),
            (M::Expm1, MathFn::Expm1),
            (M::Floor, MathFn::Floor),
            (M::Log, MathFn::Log),
            (M::Log10, MathFn::Log10),
            (M::Log1p, MathFn::Log1p),
            (M::Log2, MathFn::Log2),
            (M::Sin, MathFn::Sin),
            (M::Sinh, MathFn::Sinh),
            (M::Sqrt, MathFn::Sqrt),
            (M::Tan, MathFn::Tan),
            (M::Tanh, MathFn::Tanh),
            (M::Trunc, MathFn::Trunc),
        ];
        assert_eq!(pairs.len(), M::ALL.len());
        for x in samples() {
            for (mine, theirs) in pairs {
                let (a, b) = (jsmath::math_unary(mine, x), super::super::helpers_num2::math_unary(theirs, x));
                assert!(a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()), "{mine:?}({x}): {a} vs {b}");
            }
            let h = jsmath::f64_to_f16_bits(x);
            assert_eq!(h, super::super::helpers_num2::f64_to_f16_bits(x), "f16({x})");
            let (a, b) = (jsmath::f16_bits_to_f64(h), super::super::helpers_num2::f16_bits_to_f64(h));
            assert!(a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()));
            for bits in [8, 16] {
                assert_eq!(jsmath::to_uint_modular(x, bits), super::super::helpers_numeric::to_uint_modular(x, bits));
            }
        }
        for h in 0..=u16::MAX {
            let (a, b) = (jsmath::f16_bits_to_f64(h), super::super::helpers_num2::f16_bits_to_f64(h));
            assert!(a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()), "{h:#x}");
        }
    }
}
