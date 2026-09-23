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
//! Float32Array/Float64Array/Uint8Array view, a detached or out-of-range
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
use super::helpers_num2::math_unary;
use super::helpers_numeric::to_uint_modular;
use super::{native, Thrown, Vm};
use crate::bytecode::MathFn as M;
use crate::heap::HeapObj;
use crate::value::Value;

/// Instruction steps charged per multiply-add or element (the interpreted
/// loops take roughly 6-10 per unit).
const STEPS_PER_UNIT: u64 = 4;
/// Instruction steps charged per kernel call.
const STEPS_BASE: u64 = 16;
/// Units of work between two polls of the host abort flag.
const POLL_UNITS: usize = 1 << 16;

// Operation codes; `tensor.js` names the same numbers.
const OP_MATMUL: u32 = 1;
const OP_CONV2D: u32 = 2;
const OP_CONV2D_BACKWARD: u32 = 3;
const OP_CONV1D: u32 = 4;
const OP_CONV1D_BACKWARD: u32 = 5;
const OP_BINARY: u32 = 6;
const OP_UNARY: u32 = 7;
const OP_REDUCE: u32 = 8;
const OP_SOFTMAX: u32 = 9;
const OP_GATHER: u32 = 10;
const OP_ALL_FINITE: u32 = 11;
const OP_WHERE: u32 = 12;
const OP_MATMUL_NT: u32 = 13;
const OP_MAX_POOL2D: u32 = 14;
const OP_MAX_POOL2D_BACKWARD: u32 = 15;

// TypedArray kinds (`native::TA_KINDS`) a tensor storage can be.
const KIND_U8: u8 = 1;
const KIND_F32: u8 = 7;
const KIND_F64: u8 = 8;

/// A typed-array view: its buffer's heap index, element kind, byte offset
/// and element count.
#[derive(Clone, Copy)]
struct View {
    buffer: u32,
    kind: u8,
    offset: usize,
    len: usize,
}

impl View {
    fn size(self) -> usize {
        match self.kind {
            KIND_U8 => 1,
            KIND_F32 => 4,
            _ => 8,
        }
    }
}

/// `x` as a store into an element of `kind` leaves it: the typed-array
/// conversion (`helpers_numeric::ta_encode`), read back.
#[inline]
fn stored(kind: u8, x: f64) -> f64 {
    match kind {
        KIND_F32 => x as f32 as f64,
        KIND_U8 => to_uint_modular(x, 8) as f64,
        _ => x,
    }
}

/// JavaScript truthiness of a number.
#[inline]
fn truthy(x: f64) -> bool {
    x != 0.0 && !x.is_nan()
}

/// `Math.pow`, as the interpreter computes it (`vm::mathjson`).
#[inline]
fn js_pow(a: f64, b: f64) -> f64 {
    if (a == 1.0 || a == -1.0) && (b.is_nan() || b.is_infinite()) {
        f64::NAN
    } else {
        a.powf(b)
    }
}

/// `Math.max(x, y)` for non-NaN x, y: +0 wins a tie of zeros.
#[inline]
fn js_max(x: f64, y: f64) -> f64 {
    if x > y {
        x
    } else if y > x {
        y
    } else if x.is_sign_negative() {
        y
    } else {
        x
    }
}

/// `Math.min(x, y)` for non-NaN x, y: -0 wins a tie of zeros.
#[inline]
fn js_min(x: f64, y: f64) -> f64 {
    if x < y {
        x
    } else if y < x {
        y
    } else if x.is_sign_negative() {
        x
    } else {
        y
    }
}

// The standard normal CDF behind GELU: tensor.js's `erfSeries`, `erfcFit`
// and `cdf`, expression for expression.
#[inline]
fn erf_series(z: f64) -> f64 {
    let t = z * z;
    z * (1.1283791670955126
        + t * (-0.37612638903183754
            + t * (0.11283791670955126
                + t * (-0.026866170645131252
                    + t * (0.005223977625442188
                        + t * (-0.0008548327023450852 + t * 0.00012055332981789664))))))
}

#[inline]
fn erfc_fit(a: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.5 * a);
    t * math_unary(
        M::Exp,
        -a * a - 1.26551223
            + t * (1.00002368
                + t * (0.37409196
                    + t * (0.09678418
                        + t * (-0.18628806
                            + t * (0.27886807
                                + t * (-1.13520398
                                    + t * (1.48851587
                                        + t * (-0.82215223 + t * 0.17087277)))))))),
    )
}

#[inline]
fn cdf(x: f64) -> f64 {
    let z = x * 0.7071067811865476;
    if math_unary(M::Abs, z) < 0.5 {
        return 0.5 + 0.5 * erf_series(z);
    }
    if z >= 10.0 {
        return 1.0;
    }
    if z <= -10.0 {
        return 0.0;
    }
    let c = 0.5 * erfc_fit(math_unary(M::Abs, z));
    if z > 0.0 {
        1.0 - c
    } else {
        c
    }
}

/// One element of a binary op, as tensor.js's `BIN` table computes it.
/// `None`: the op has no native form.
#[inline]
fn binary_fn(op: u32) -> Option<fn(f64, f64) -> f64> {
    Some(match op {
        1 => |x, y| x + y,
        2 => |x, y| x - y,
        3 => |x, y| x * y,
        4 => |x, y| x / y,
        5 => js_pow,
        6 => |x: f64, y: f64| {
            if x.is_nan() || y.is_nan() {
                f64::NAN
            } else {
                js_max(x, y)
            }
        },
        7 => |x: f64, y: f64| {
            if x.is_nan() || y.is_nan() {
                f64::NAN
            } else {
                js_min(x, y)
            }
        },
        8 => |x, y| (x == y) as u8 as f64,
        9 => |x, y| (x != y) as u8 as f64,
        10 => |x, y| (x < y) as u8 as f64,
        11 => |x, y| (x <= y) as u8 as f64,
        12 => |x, y| (x > y) as u8 as f64,
        13 => |x, y| (x >= y) as u8 as f64,
        14 => |x, y| (truthy(x) && truthy(y)) as u8 as f64,
        15 => |x, y| (truthy(x) || truthy(y)) as u8 as f64,
        16 => |x, y| (truthy(x) ^ truthy(y)) as u8 as f64,
        17 => |x: f64, y: f64| math_unary(M::Floor, x / y),
        18 => |x: f64, y: f64| x - math_unary(M::Floor, x / y) * y,
        19 => |x: f64, y: f64| x.atan2(y),
        _ => return None,
    })
}

/// One element of a unary op, as tensor.js's `UN` table computes it
/// (`clamp` is handled by the caller). `None`: no native form.
#[inline]
fn unary_fn(op: u32) -> Option<fn(f64) -> f64> {
    Some(match op {
        1 => |x: f64| -x,
        2 => |x: f64| if x > 0.0 || x.is_nan() { x } else { 0.0 },
        3 => |x| math_unary(M::Exp, x),
        4 => |x| math_unary(M::Log, x),
        5 => |x| math_unary(M::Tanh, x),
        6 => |x: f64| 1.0 / (1.0 + math_unary(M::Exp, -x)),
        7 => |x| math_unary(M::Sqrt, x),
        8 => |x| x * x,
        9 => |x| math_unary(M::Abs, x),
        10 => |x| {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        },
        11 => |x: f64| x / (1.0 + math_unary(M::Exp, -x)),
        12 => |x| x * cdf(x),
        13 => |x: f64| cdf(x) + x * 0.3989422804014327 * math_unary(M::Exp, -0.5 * x * x),
        14 => |x| 1.0 / x,
        15 => |x| 1.0 / math_unary(M::Sqrt, x),
        16 => |x| math_unary(M::Log1p, x),
        17 => |x| math_unary(M::Expm1, x),
        18 => |x| {
            if x > 20.0 {
                x
            } else {
                math_unary(M::Log1p, math_unary(M::Exp, x))
            }
        },
        19 => |x| math_unary(M::Sin, x),
        20 => |x| math_unary(M::Cos, x),
        21 => |x| math_unary(M::Floor, x),
        22 => |x| math_unary(M::Ceil, x),
        23 => |x| math_unary(M::Trunc, x),
        24 => |x: f64| x.is_finite() as u8 as f64,
        25 => |x: f64| x.is_nan() as u8 as f64,
        26 => |x| (!truthy(x)) as u8 as f64,
        28 => |x| x - math_unary(M::Trunc, x),
        29 => |x| math_unary(M::Tan, x),
        30 => |x| math_unary(M::Atan, x),
        31 => |x| math_unary(M::Log2, x),
        32 => |x| math_unary(M::Log10, x),
        33 => |x: f64| x.is_infinite() as u8 as f64,
        34 => |x| js_pow(2.0, x),
        35 => |x| math_unary(M::Sinh, x),
        36 => |x| math_unary(M::Cosh, x),
        37 => |x| math_unary(M::Asin, x),
        38 => |x| math_unary(M::Acos, x),
        39 => |x| math_unary(M::Asinh, x),
        40 => |x| math_unary(M::Acosh, x),
        41 => |x| math_unary(M::Atanh, x),
        _ => return None,
    })
}

/// A zeroed `Vec` of `n`, or `None` when it cannot be allocated.
fn zeroed(n: usize) -> Option<Vec<f64>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).ok()?;
    v.resize(n, 0.0);
    Some(v)
}

/// Row-major strides of `shape`.
fn contiguous_strides(shape: &[usize]) -> Vec<usize> {
    let mut out = vec![0; shape.len()];
    let mut acc = 1usize;
    for d in (0..shape.len()).rev() {
        out[d] = acc;
        acc = acc.saturating_mul(shape[d]);
    }
    out
}

/// The largest offset a strided walk over `shape` reads, or `None` on
/// overflow. `shape` has no zero dims (the caller checks `numel > 0`).
fn max_offset(shape: &[usize], strides: &[usize], base: usize) -> Option<usize> {
    let mut hi = base;
    for (&n, &s) in shape.iter().zip(strides) {
        hi = hi.checked_add((n - 1).checked_mul(s)?)?;
    }
    Some(hi)
}

/// Visit every index of `shape` in row-major order, calling `f(flat, offs)`
/// with each operand's offset (per-operand strides, 0 broadcasting).
/// `strides[j]` is operand j's stride table. Returns false if interrupted.
fn walk<const N: usize>(
    shape: &[usize],
    strides: [&[usize]; N],
    mut poll: impl FnMut() -> bool,
    mut f: impl FnMut(usize, [usize; N]),
) -> bool {
    let rank = shape.len();
    let n: usize = shape.iter().product();
    if n == 0 {
        return true;
    }
    if rank == 0 {
        f(0, [0; N]);
        return true;
    }
    let last = rank - 1;
    let inner = shape[last];
    let mut idx = vec![0usize; rank];
    let mut offs = [0usize; N];
    let mut flat = 0usize;
    let mut since_poll = 0usize;
    loop {
        let mut o = offs;
        for _ in 0..inner {
            f(flat, o);
            flat += 1;
            for j in 0..N {
                o[j] += strides[j][last];
            }
        }
        since_poll += inner;
        if since_poll >= POLL_UNITS {
            since_poll = 0;
            if poll() {
                return false;
            }
        }
        // Advance the outer dims (everything but the last).
        let mut d = last;
        loop {
            if d == 0 {
                return true;
            }
            d -= 1;
            idx[d] += 1;
            for j in 0..N {
                offs[j] += strides[j][d];
            }
            if idx[d] < shape[d] {
                break;
            }
            for j in 0..N {
                offs[j] -= strides[j][d] * shape[d];
            }
            idx[d] = 0;
        }
    }
}

impl Vm<'_> {
    /// `__zipp_py_native(op, ...)`. `true`/`false` for a kernel that ran or
    /// declined; `OP_ALL_FINITE` answers `true`/`false`, or `null` when it
    /// declines.
    pub(crate) fn py_tensor(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let Some(op) = int_arg(args, 0) else {
            return Ok(Value::bool(false));
        };
        let op = u32::try_from(op).unwrap_or(0);
        let a = &args[1..];
        Ok(match op {
            OP_MATMUL => Value::bool(self.pt_matmul(a, false).is_some()),
            OP_MATMUL_NT => Value::bool(self.pt_matmul(a, true).is_some()),
            OP_MAX_POOL2D => Value::bool(self.pt_max_pool2d(a, false).is_some()),
            OP_MAX_POOL2D_BACKWARD => Value::bool(self.pt_max_pool2d(a, true).is_some()),
            OP_CONV2D => Value::bool(self.pt_conv2d(a, false).is_some()),
            OP_CONV2D_BACKWARD => Value::bool(self.pt_conv2d(a, true).is_some()),
            OP_CONV1D => Value::bool(self.pt_conv1d(a).is_some()),
            OP_CONV1D_BACKWARD => Value::bool(self.pt_conv1d_backward(a).is_some()),
            OP_BINARY => Value::bool(self.pt_binary(a).is_some()),
            OP_UNARY => Value::bool(self.pt_unary(a).is_some()),
            OP_REDUCE => Value::bool(self.pt_reduce(a).is_some()),
            OP_SOFTMAX => Value::bool(self.pt_softmax(a).is_some()),
            OP_GATHER => Value::bool(self.pt_gather(a).is_some()),
            OP_ALL_FINITE => match self.pt_all_finite(a) {
                Some(b) => Value::bool(b),
                None => Value::NULL,
            },
            OP_WHERE => Value::bool(self.pt_where(a).is_some()),
            _ => Value::bool(false),
        })
    }

    // ---- argument access ------------------------------------------------------------

    /// A storage view: a Float32Array, Float64Array or Uint8Array whose
    /// buffer is live and covers it.
    fn pt_view(&self, v: Value) -> Option<View> {
        if !v.is_heap() {
            return None;
        }
        let idx = v.heap_index();
        let HeapObj::TypedArray {
            buffer,
            kind,
            byte_offset,
            ..
        } = *self.heap.get(idx)
        else {
            return None;
        };
        if !matches!(kind, KIND_U8 | KIND_F32 | KIND_F64) {
            return None;
        }
        let len = self.ta_effective_len(idx)?;
        Some(View {
            buffer,
            kind,
            offset: byte_offset,
            len,
        })
    }

    /// Elements `start..start + count` of `v`, as f64.
    fn pt_read(&self, v: View, start: usize, count: usize) -> Option<Vec<f64>> {
        if start.checked_add(count)? > v.len {
            return None;
        }
        let size = v.size();
        let lo = v.offset.checked_add(start.checked_mul(size)?)?;
        let hi = lo.checked_add(count.checked_mul(size)?)?;
        let HeapObj::ArrayBuffer { data, detached } = self.heap.get(v.buffer) else {
            return None;
        };
        if *detached || hi > data.len() {
            return None;
        }
        let bytes = &data[lo..hi];
        let mut out = Vec::new();
        out.try_reserve_exact(count).ok()?;
        match v.kind {
            KIND_F32 => out.extend(bytes.chunks_exact(4).map(|c| {
                let mut b = [0u8; 4];
                b.copy_from_slice(c);
                f32::from_le_bytes(b) as f64
            })),
            KIND_F64 => out.extend(bytes.chunks_exact(8).map(|c| {
                let mut b = [0u8; 8];
                b.copy_from_slice(c);
                f64::from_le_bytes(b)
            })),
            _ => out.extend(bytes.iter().map(|&b| b as f64)),
        }
        Some(out)
    }

    /// The whole of `v`.
    fn pt_read_all(&self, v: View) -> Option<Vec<f64>> {
        self.pt_read(v, 0, v.len)
    }

    /// Store `values` into elements `start..` of `v`, each converted as a
    /// typed-array store converts it.
    fn pt_write(&mut self, v: View, start: usize, values: &[f64]) -> Option<()> {
        if start.checked_add(values.len())? > v.len {
            return None;
        }
        let size = v.size();
        let lo = v.offset.checked_add(start.checked_mul(size)?)?;
        let hi = lo.checked_add(values.len().checked_mul(size)?)?;
        let HeapObj::ArrayBuffer { data, detached } = self.heap.get_mut(v.buffer) else {
            return None;
        };
        if *detached || hi > data.len() {
            return None;
        }
        let bytes = &mut data[lo..hi];
        match v.kind {
            KIND_F32 => {
                for (c, &x) in bytes.chunks_exact_mut(4).zip(values) {
                    c.copy_from_slice(&(x as f32).to_le_bytes());
                }
            }
            KIND_F64 => {
                for (c, &x) in bytes.chunks_exact_mut(8).zip(values) {
                    c.copy_from_slice(&x.to_le_bytes());
                }
            }
            _ => {
                for (c, &x) in bytes.iter_mut().zip(values) {
                    *c = to_uint_modular(x, 8) as u8;
                }
            }
        }
        Some(())
    }

    /// A dense JavaScript array of non-negative safe integers.
    fn pt_ints(&self, v: Value) -> Option<Vec<usize>> {
        if !v.is_heap() {
            return None;
        }
        let HeapObj::Array(items) = self.heap.get(v.heap_index()) else {
            return None;
        };
        let mut out = Vec::new();
        out.try_reserve_exact(items.len()).ok()?;
        for &item in items.iter() {
            out.push(as_index(item)?);
        }
        Some(out)
    }

    /// Price `units` of work, or `None` when the budget cannot cover it or
    /// a heap ceiling has no room for `elems` f64 working values.
    fn pt_admit(&self, units: usize, elems: usize) -> Option<u64> {
        let cost = STEPS_BASE.saturating_add((units as u64).saturating_mul(STEPS_PER_UNIT));
        self.native_kernel_admits(cost, elems.saturating_mul(8)).then_some(cost)
    }

    fn pt_charge(&mut self, cost: u64) {
        self.charge_steps(i64::try_from(cost).unwrap_or(i64::MAX));
    }

    // ---- kernels ----------------------------------------------------------------------

    /// `(A, B, O, aOff, bOff, oOff, m, k, n)`: one [m,k] @ [k,n] product of a
    /// batch, tensor.js's `matmul` loop: i-k-j order over one f64 row, each
    /// output rounded once on store. `trans_b`: B is stored as its [n,k]
    /// transpose (the `transB` loop); element (i, j) still sums its k
    /// products in p order from 0 in an f64, so it is the same value.
    fn pt_matmul(&mut self, a: &[Value], trans_b: bool) -> Option<()> {
        let (va, vb, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?);
        if vo.buffer == va.buffer || vo.buffer == vb.buffer {
            return None;
        }
        let (ao, bo, oo) = (int_arg(a, 3)?, int_arg(a, 4)?, int_arg(a, 5)?);
        let (m, k, n) = (int_arg(a, 6)?, int_arg(a, 7)?, int_arg(a, 8)?);
        let (mk, kn, mn) = (m.checked_mul(k)?, k.checked_mul(n)?, m.checked_mul(n)?);
        let cost = self.pt_admit(mn.checked_mul(k)?.checked_add(mn)?, mk.saturating_add(kn).saturating_add(mn))?;
        let av = self.pt_read(va, ao, mk)?;
        let bv = self.pt_read(vb, bo, kn)?;
        let mut out = zeroed(mn)?;
        let rows_per_poll = (POLL_UNITS / k.max(1).saturating_mul(n).max(1)).max(1);
        for i in 0..m {
            if i % rows_per_poll == rows_per_poll - 1 && self.native_kernel_interrupted() {
                return None;
            }
            let row = &mut out[i * n..(i + 1) * n];
            let arow = &av[i * k..(i + 1) * k];
            if trans_b {
                for (j, r) in row.iter_mut().enumerate() {
                    let brow = &bv[j * k..(j + 1) * k];
                    let mut s = 0.0f64;
                    for (&x, &y) in arow.iter().zip(brow) {
                        s += x * y;
                    }
                    *r = s;
                }
                continue;
            }
            for (p, &x) in arow.iter().enumerate() {
                let brow = &bv[p * n..(p + 1) * n];
                for (r, &y) in row.iter_mut().zip(brow) {
                    *r += x * y;
                }
            }
        }
        self.pt_write(vo, oo, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// Forward `(X, W, bias|null, O, B, C, H, W, O, Cg, Kh, Kw, Sh, Sw, Ph,
    /// Pw, Dh, Dw, groups, Ho, Wo)` or backward `(X, W, G, GX, GW, GB, ...the
    /// same dims)`: tensor.js's `conv2d` walk. The forward sums in an f64
    /// local rounded on store; the backward accumulates into its outputs,
    /// rounding on every add as the typed-array `+=` does.
    fn pt_conv2d(&mut self, a: &[Value], backward: bool) -> Option<()> {
        let dims_at = if backward { 6 } else { 4 };
        let mut d = [0usize; 17];
        for (i, slot) in d.iter_mut().enumerate() {
            *slot = int_arg(a, dims_at + i)?;
        }
        let [nb, c, h, w, o, cg, kh, kw, sh, sw, ph, pw, dh, dw, groups, ho, wo] = d;
        if groups == 0 || o % groups != 0 || cg.checked_mul(groups)? != c {
            return None;
        }
        let per_group = o / groups;
        if per_group == 0 {
            return None;
        }
        let xn = nb.checked_mul(c)?.checked_mul(h)?.checked_mul(w)?;
        let wn = o.checked_mul(cg)?.checked_mul(kh)?.checked_mul(kw)?;
        let on = nb.checked_mul(o)?.checked_mul(ho)?.checked_mul(wo)?;
        let macs = on.checked_mul(cg)?.checked_mul(kh)?.checked_mul(kw)?;
        // Every index the walk forms must fit an isize: the padded
        // coordinates are signed.
        if xn > isize::MAX as usize / 2 || ho.checked_mul(sh)?.checked_add(kh.checked_mul(dh)?)? > isize::MAX as usize / 2 || wo.checked_mul(sw)?.checked_add(kw.checked_mul(dw)?)? > isize::MAX as usize / 2 {
            return None;
        }
        let vx = self.pt_view(arg(a, 0))?;
        let vw = self.pt_view(arg(a, 1))?;
        if vx.len != xn || vw.len != wn {
            return None;
        }
        let buffers = xn.saturating_add(wn).saturating_add(on).saturating_add(o);
        let cost = self.pt_admit(macs.checked_mul(if backward { 2 } else { 1 })?.checked_add(on)?, if backward { buffers.saturating_mul(2) } else { buffers })?;
        let xv = self.pt_read_all(vx)?;
        let wv = self.pt_read_all(vw)?;
        let (h, w, ph, pw) = (h as isize, w as isize, ph as isize, pw as isize);
        if backward {
            let vg = self.pt_view(arg(a, 2))?;
            let (vgx, vgw, vgb) = (self.pt_view(arg(a, 3))?, self.pt_view(arg(a, 4))?, self.pt_view(arg(a, 5))?);
            if vg.len != on || vgx.len != xn || vgw.len != wn || vgb.len != o {
                return None;
            }
            let outs = [vgx.buffer, vgw.buffer, vgb.buffer];
            if outs.iter().any(|&b| b == vx.buffer || b == vw.buffer || b == vg.buffer)
                || vgx.buffer == vgw.buffer
                || vgx.buffer == vgb.buffer
                || vgw.buffer == vgb.buffer
            {
                return None;
            }
            let gv_all = self.pt_read_all(vg)?;
            let (mut gx, mut gw, mut gb) = (zeroed(xn)?, zeroed(wn)?, zeroed(o)?);
            let (kx, kw_, kb) = (vgx.kind, vgw.kind, vgb.kind);
            let mut since = 0usize;
            for b in 0..nb {
                for oc in 0..o {
                    let first = (oc / per_group) * cg;
                    for hh in 0..ho {
                        for vv in 0..wo {
                            let oi = ((b * o + oc) * ho + hh) * wo + vv;
                            let gv = gv_all[oi];
                            gb[oc] = stored(kb, gb[oc] + gv);
                            for ci in 0..cg {
                                for a_ in 0..kh {
                                    let ih = (hh * sh) as isize - ph + (a_ * dh) as isize;
                                    if ih < 0 || ih >= h {
                                        continue;
                                    }
                                    for b_ in 0..kw {
                                        let iw = (vv * sw) as isize - pw + (b_ * dw) as isize;
                                        if iw < 0 || iw >= w {
                                            continue;
                                        }
                                        let xi = (((b * c + first + ci) as isize * h + ih) * w + iw) as usize;
                                        let wi = ((oc * cg + ci) * kh + a_) * kw + b_;
                                        gx[xi] = stored(kx, gx[xi] + gv * wv[wi]);
                                        gw[wi] = stored(kw_, gw[wi] + gv * xv[xi]);
                                    }
                                }
                            }
                            since += cg * kh * kw;
                            if since >= POLL_UNITS {
                                since = 0;
                                if self.native_kernel_interrupted() {
                                    return None;
                                }
                            }
                        }
                    }
                }
            }
            self.pt_write(vgx, 0, &gx)?;
            self.pt_write(vgw, 0, &gw)?;
            self.pt_write(vgb, 0, &gb)?;
        } else {
            let bias = arg(a, 2);
            let bv = if bias.is_null() {
                None
            } else {
                let vb = self.pt_view(bias)?;
                if vb.len != o {
                    return None;
                }
                Some(self.pt_read_all(vb)?)
            };
            let vo = self.pt_view(arg(a, 3))?;
            if vo.len != on || vo.buffer == vx.buffer || vo.buffer == vw.buffer {
                return None;
            }
            let mut out = zeroed(on)?;
            let mut since = 0usize;
            for b in 0..nb {
                for oc in 0..o {
                    let first = (oc / per_group) * cg;
                    for hh in 0..ho {
                        for vv in 0..wo {
                            let oi = ((b * o + oc) * ho + hh) * wo + vv;
                            let mut sum = match &bv {
                                Some(bv) => bv[oc],
                                None => 0.0,
                            };
                            for ci in 0..cg {
                                for a_ in 0..kh {
                                    let ih = (hh * sh) as isize - ph + (a_ * dh) as isize;
                                    if ih < 0 || ih >= h {
                                        continue;
                                    }
                                    for b_ in 0..kw {
                                        let iw = (vv * sw) as isize - pw + (b_ * dw) as isize;
                                        if iw < 0 || iw >= w {
                                            continue;
                                        }
                                        let xi = (((b * c + first + ci) as isize * h + ih) * w + iw) as usize;
                                        let wi = ((oc * cg + ci) * kh + a_) * kw + b_;
                                        sum += xv[xi] * wv[wi];
                                    }
                                }
                            }
                            out[oi] = sum;
                            since += cg * kh * kw;
                            if since >= POLL_UNITS {
                                since = 0;
                                if self.native_kernel_interrupted() {
                                    return None;
                                }
                            }
                        }
                    }
                }
            }
            self.pt_write(vo, 0, &out)?;
        }
        self.pt_charge(cost);
        Some(())
    }

    /// `(X, W, bias|null, O, B, C, L, Oc, K, Lo)`: tensor.js's `conv1d`.
    fn pt_conv1d(&mut self, a: &[Value]) -> Option<()> {
        let (nb, c, l, oc, k, lo) = (int_arg(a, 4)?, int_arg(a, 5)?, int_arg(a, 6)?, int_arg(a, 7)?, int_arg(a, 8)?, int_arg(a, 9)?);
        if lo.checked_add(k)? != l.checked_add(1)? {
            return None;
        }
        let (vx, vw, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 3))?);
        let xn = nb.checked_mul(c)?.checked_mul(l)?;
        let wn = oc.checked_mul(c)?.checked_mul(k)?;
        let on = nb.checked_mul(oc)?.checked_mul(lo)?;
        if vx.len != xn || vw.len != wn || vo.len != on || vo.buffer == vx.buffer || vo.buffer == vw.buffer {
            return None;
        }
        let cost = self.pt_admit(on.checked_mul(c)?.checked_mul(k)?.checked_add(on)?, xn.saturating_add(wn).saturating_add(on).saturating_add(oc))?;
        let bias = arg(a, 2);
        let bv = if bias.is_null() {
            None
        } else {
            let vb = self.pt_view(bias)?;
            if vb.len != oc {
                return None;
            }
            Some(self.pt_read_all(vb)?)
        };
        let xv = self.pt_read_all(vx)?;
        let wv = self.pt_read_all(vw)?;
        let mut out = zeroed(on)?;
        for b in 0..nb {
            if self.native_kernel_interrupted() {
                return None;
            }
            for o in 0..oc {
                let bias_v = match &bv {
                    Some(bv) => bv[o],
                    None => 0.0,
                };
                for t in 0..lo {
                    let mut s = bias_v;
                    for ci in 0..c {
                        let xb = (b * c + ci) * l + t;
                        let wb = (o * c + ci) * k;
                        for kk in 0..k {
                            s += xv[xb + kk] * wv[wb + kk];
                        }
                    }
                    out[(b * oc + o) * lo + t] = s;
                }
            }
        }
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(X, W, G, GX, GW, GB, B, C, L, Oc, K, Lo)`: tensor.js's
    /// `conv1dBackward`, rounding on every accumulation.
    fn pt_conv1d_backward(&mut self, a: &[Value]) -> Option<()> {
        let (nb, c, l, oc, k, lo) = (int_arg(a, 6)?, int_arg(a, 7)?, int_arg(a, 8)?, int_arg(a, 9)?, int_arg(a, 10)?, int_arg(a, 11)?);
        if lo.checked_add(k)? != l.checked_add(1)? {
            return None;
        }
        let (vx, vw, vg) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?);
        let (vgx, vgw, vgb) = (self.pt_view(arg(a, 3))?, self.pt_view(arg(a, 4))?, self.pt_view(arg(a, 5))?);
        let xn = nb.checked_mul(c)?.checked_mul(l)?;
        let wn = oc.checked_mul(c)?.checked_mul(k)?;
        let on = nb.checked_mul(oc)?.checked_mul(lo)?;
        if vx.len != xn || vw.len != wn || vg.len != on || vgx.len != xn || vgw.len != wn || vgb.len != oc {
            return None;
        }
        let outs = [vgx.buffer, vgw.buffer, vgb.buffer];
        if outs.iter().any(|&b| b == vx.buffer || b == vw.buffer || b == vg.buffer)
            || vgx.buffer == vgw.buffer
            || vgx.buffer == vgb.buffer
            || vgw.buffer == vgb.buffer
        {
            return None;
        }
        let cost = self.pt_admit(on.checked_mul(c)?.checked_mul(k)?.checked_mul(2)?.checked_add(on)?, xn.saturating_add(wn).saturating_add(on).saturating_mul(2).saturating_add(oc))?;
        let xv = self.pt_read_all(vx)?;
        let wv = self.pt_read_all(vw)?;
        let gv_all = self.pt_read_all(vg)?;
        let (mut gx, mut gw, mut gb) = (zeroed(xn)?, zeroed(wn)?, zeroed(oc)?);
        let (kx, kw, kb) = (vgx.kind, vgw.kind, vgb.kind);
        for b in 0..nb {
            if self.native_kernel_interrupted() {
                return None;
            }
            for o in 0..oc {
                for t in 0..lo {
                    let gv = gv_all[(b * oc + o) * lo + t];
                    if gv == 0.0 {
                        continue;
                    }
                    gb[o] = stored(kb, gb[o] + gv);
                    for ci in 0..c {
                        let xb = (b * c + ci) * l + t;
                        let wb = (o * c + ci) * k;
                        for kk in 0..k {
                            gx[xb + kk] = stored(kx, gx[xb + kk] + gv * wv[wb + kk]);
                            gw[wb + kk] = stored(kw, gw[wb + kk] + gv * xv[xb + kk]);
                        }
                    }
                }
            }
        }
        self.pt_write(vgx, 0, &gx)?;
        self.pt_write(vgw, 0, &gw)?;
        self.pt_write(vgb, 0, &gb)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(op, A, B, O, shape, stridesA, stridesB)`: `O[i] = op(A[.], B[.])`
    /// over the broadcast walk of `shape` (tensor.js's `binary`; every
    /// layout it special-cases visits the same pairs).
    fn pt_binary(&mut self, a: &[Value]) -> Option<()> {
        let f = binary_fn(u32::try_from(int_arg(a, 0)?).ok()?)?;
        let (va, vb, vo) = (self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?, self.pt_view(arg(a, 3))?);
        if vo.buffer == va.buffer || vo.buffer == vb.buffer {
            return None;
        }
        let shape = self.pt_ints(arg(a, 4))?;
        let (sa, sb) = (self.pt_ints(arg(a, 5))?, self.pt_ints(arg(a, 6))?);
        if sa.len() != shape.len() || sb.len() != shape.len() {
            return None;
        }
        let n = shape.iter().try_fold(1usize, |acc, &d| acc.checked_mul(d))?;
        if vo.len != n {
            return None;
        }
        let cost = self.pt_admit(n, n.saturating_add(va.len).saturating_add(vb.len))?;
        let mut out = zeroed(n)?;
        if n > 0 {
            if max_offset(&shape, &sa, 0)? >= va.len || max_offset(&shape, &sb, 0)? >= vb.len {
                return None;
            }
            let av = self.pt_read_all(va)?;
            let bv = self.pt_read_all(vb)?;
            let contiguous = contiguous_strides(&shape);
            if av.len() == n && bv.len() == n && sa == contiguous && sb == contiguous {
                for ((o, &x), &y) in out.iter_mut().zip(&av).zip(&bv) {
                    *o = f(x, y);
                }
            } else if sa == contiguous && bv.len() == 1 && sb.iter().all(|&s| s == 0) {
                let y = bv[0];
                for (o, &x) in out.iter_mut().zip(&av) {
                    *o = f(x, y);
                }
            } else if sb == contiguous && av.len() == 1 && sa.iter().all(|&s| s == 0) {
                let x = av[0];
                for (o, &y) in out.iter_mut().zip(&bv) {
                    *o = f(x, y);
                }
            } else {
                let done = walk(&shape, [&sa, &sb], || self.native_kernel_interrupted(), |i, [x, y]| {
                    out[i] = f(av[x], bv[y]);
                });
                if !done {
                    return None;
                }
            }
        }
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(op, A, O, lo, hi)`: `O[i] = op(A[i])` (tensor.js's `unary`); op 27
    /// is `clamp` to `[lo, hi]`.
    fn pt_unary(&mut self, a: &[Value]) -> Option<()> {
        let op = u32::try_from(int_arg(a, 0)?).ok()?;
        let (va, vo) = (self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?);
        if vo.buffer == va.buffer || vo.len != va.len {
            return None;
        }
        let cost = self.pt_admit(va.len, va.len)?;
        let mut v = self.pt_read_all(va)?;
        if op == 27 {
            let (lo, hi) = (num_arg(a, 3)?, num_arg(a, 4)?);
            for x in v.iter_mut() {
                *x = if *x < lo {
                    lo
                } else if *x > hi {
                    hi
                } else {
                    *x
                };
            }
        } else {
            let f = unary_fn(op)?;
            for x in v.iter_mut() {
                *x = f(*x);
            }
        }
        self.pt_write(vo, 0, &v)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(op, A, O, outer, block, inner, count, perStore)`: tensor.js's
    /// contiguous reduction — every output element takes its inputs in
    /// increasing flat order. `perStore`: the accumulator is the float32
    /// result itself, so every update rounds; otherwise it is a double array
    /// rounded once on store.
    fn pt_reduce(&mut self, a: &[Value]) -> Option<()> {
        let op = int_arg(a, 0)?;
        let (va, vo) = (self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?);
        if vo.buffer == va.buffer {
            return None;
        }
        let (outer, block, inner) = (int_arg(a, 3)?, int_arg(a, 4)?, int_arg(a, 5)?);
        let count = num_arg(a, 6)?;
        let per_store = arg(a, 7).truthy_primitive()?;
        let n_in = outer.checked_mul(block)?.checked_mul(inner)?;
        let n_out = outer.checked_mul(inner)?;
        if va.len != n_in || vo.len != n_out {
            return None;
        }
        let round = if per_store { vo.kind } else { KIND_F64 };
        let cost = self.pt_admit(n_in.checked_add(n_out)?, n_in.saturating_add(n_out.saturating_mul(2)))?;
        let av = self.pt_read_all(va)?;
        let init = match op {
            1 | 2 => 0.0,
            3 => 1.0,
            4 | 6 => f64::NEG_INFINITY,
            5 | 7 => f64::INFINITY,
            8 => 1.0,
            9 => 0.0,
            _ => return None,
        };
        let arg_op = op == 6 || op == 7;
        let mut acc = zeroed(n_out)?;
        let fill = stored(round, if arg_op { 0.0 } else { init });
        acc.iter_mut().for_each(|v| *v = fill);
        let mut best = if arg_op {
            let mut b = zeroed(n_out)?;
            b.iter_mut().for_each(|v| *v = init);
            b
        } else {
            Vec::new()
        };
        let mut i = 0usize;
        for x in 0..outer {
            if self.native_kernel_interrupted() {
                return None;
            }
            let base = x * inner;
            for r in 0..block {
                let src = &av[i..i + inner];
                i += inner;
                let dst = &mut acc[base..base + inner];
                match op {
                    1 | 2 => {
                        for (o, &v) in dst.iter_mut().zip(src) {
                            *o = stored(round, *o + v);
                        }
                    }
                    3 => {
                        for (o, &v) in dst.iter_mut().zip(src) {
                            *o = stored(round, *o * v);
                        }
                    }
                    4 => {
                        for (o, &v) in dst.iter_mut().zip(src) {
                            if v > *o || v.is_nan() {
                                *o = stored(round, v);
                            }
                        }
                    }
                    5 => {
                        for (o, &v) in dst.iter_mut().zip(src) {
                            if v < *o || v.is_nan() {
                                *o = stored(round, v);
                            }
                        }
                    }
                    6 | 7 => {
                        let bst = &mut best[base..base + inner];
                        for ((o, bb), &v) in dst.iter_mut().zip(bst.iter_mut()).zip(src) {
                            let b = *bb;
                            let better = if op == 6 { v > b } else { v < b };
                            if better || (v.is_nan() && !b.is_nan()) {
                                *bb = v;
                                *o = stored(round, r as f64);
                            }
                        }
                    }
                    8 => {
                        for (o, &v) in dst.iter_mut().zip(src) {
                            if !truthy(v) {
                                *o = 0.0;
                            }
                        }
                    }
                    _ => {
                        for (o, &v) in dst.iter_mut().zip(src) {
                            if truthy(v) {
                                *o = 1.0;
                            }
                        }
                    }
                }
            }
        }
        if op == 2 {
            for o in acc.iter_mut() {
                *o = stored(round, *o / count);
            }
        }
        self.pt_write(vo, 0, &acc)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(A, O, outer, n, inner, log)`: tensor.js's `softmax` along a dim.
    fn pt_softmax(&mut self, a: &[Value]) -> Option<()> {
        let (va, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?);
        if vo.buffer == va.buffer {
            return None;
        }
        let (outer, n, inner) = (int_arg(a, 2)?, int_arg(a, 3)?, int_arg(a, 4)?);
        let log = arg(a, 5).truthy_primitive()?;
        let total = outer.checked_mul(n)?.checked_mul(inner)?;
        if va.len != total || vo.len != total {
            return None;
        }
        let cost = self.pt_admit(total.checked_mul(3)?, total.saturating_mul(2))?;
        let av = self.pt_read_all(va)?;
        let mut out = zeroed(total)?;
        for x in 0..outer {
            if self.native_kernel_interrupted() {
                return None;
            }
            for r in 0..inner {
                let base = x * n * inner + r;
                let mut mx = f64::NEG_INFINITY;
                for i in 0..n {
                    let v = av[base + i * inner];
                    if v > mx {
                        mx = v;
                    }
                }
                let mut sum = 0.0;
                for i in 0..n {
                    sum += math_unary(M::Exp, av[base + i * inner] - mx);
                }
                let ls = math_unary(M::Log, sum);
                for i in 0..n {
                    let p = base + i * inner;
                    let z = av[p] - mx;
                    out[p] = if log { z - ls } else { math_unary(M::Exp, z) / sum };
                }
            }
        }
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(A, O, shape, strides, base)`: `O[o] = A[base + offset of o]`, the
    /// strided walk behind permute, expand and slice.
    fn pt_gather(&mut self, a: &[Value]) -> Option<()> {
        let (va, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?);
        if vo.buffer == va.buffer || vo.kind != va.kind {
            return None;
        }
        let shape = self.pt_ints(arg(a, 2))?;
        let st = self.pt_ints(arg(a, 3))?;
        let base = int_arg(a, 4)?;
        if st.len() != shape.len() {
            return None;
        }
        let n = shape.iter().try_fold(1usize, |acc, &d| acc.checked_mul(d))?;
        if vo.len != n {
            return None;
        }
        let cost = self.pt_admit(n, n.saturating_add(va.len))?;
        let mut out = zeroed(n)?;
        if n > 0 {
            if max_offset(&shape, &st, base)? >= va.len {
                return None;
            }
            let av = self.pt_read_all(va)?;
            let done = walk(&shape, [&st], || self.native_kernel_interrupted(), |i, [x]| {
                out[i] = av[base + x];
            });
            if !done {
                return None;
            }
        }
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(A)`: no NaN or infinity in a float storage.
    fn pt_all_finite(&mut self, a: &[Value]) -> Option<bool> {
        let va = self.pt_view(arg(a, 0))?;
        let cost = self.pt_admit(va.len, va.len)?;
        let av = self.pt_read_all(va)?;
        let finite = av.iter().all(|&x| x - x == 0.0);
        self.pt_charge(cost);
        Some(finite)
    }

    /// Forward `(X, O, I, NC, H, W, Kh, Kw, Sh, Sw, Ph, Pw, Dh, Dw, Ho, Wo)`
    /// or backward `(G, I, GX, ...the same dims)`: tensor.js's `maxPool2d`
    /// and `maxPool2dBackward`. The forward takes each window's values in
    /// row-major order (a padded position reads -Infinity) and keeps the
    /// max as the `max` reduction does (NaN wins, the last one) and its
    /// position as `argmax` does (the first maximum or the first NaN). The
    /// backward stores `g + 0` at each output's recorded position of a
    /// zeroed input-shaped gradient (positions outside the input dropped).
    fn pt_max_pool2d(&mut self, a: &[Value], backward: bool) -> Option<()> {
        let mut d = [0usize; 13];
        for (i, slot) in d.iter_mut().enumerate() {
            *slot = int_arg(a, 3 + i)?;
        }
        let [nc, h, w, kh, kw, sh, sw, ph, pw, dh, dw, ho, wo] = d;
        let k = kh.checked_mul(kw)?;
        let xn = nc.checked_mul(h)?.checked_mul(w)?;
        let on = nc.checked_mul(ho)?.checked_mul(wo)?;
        // Every coordinate the walk forms must fit an isize.
        let lim = isize::MAX as usize / 4;
        if xn > lim || ho.checked_mul(sh)?.checked_add(kh.checked_mul(dh)?)?.checked_add(ph)? > lim || wo.checked_mul(sw)?.checked_add(kw.checked_mul(dw)?)?.checked_add(pw)? > lim {
            return None;
        }
        let (v0, v1, v2) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?);
        if v0.buffer == v2.buffer || v1.buffer == v2.buffer || v0.buffer == v1.buffer {
            return None;
        }
        let (hi, wi, phi, pwi) = (h as isize, w as isize, ph as isize, pw as isize);
        if backward {
            let (vg, vi, vgx) = (v0, v1, v2);
            if vg.len != on || vi.len != on || vgx.len != xn {
                return None;
            }
            let cost = self.pt_admit(on.saturating_add(xn), on.saturating_mul(2).saturating_add(xn))?;
            let gv = self.pt_read_all(vg)?;
            let iv = self.pt_read_all(vi)?;
            let mut out = zeroed(xn)?;
            let mut o = 0usize;
            for p in 0..nc {
                if p % 64 == 63 && self.native_kernel_interrupted() {
                    return None;
                }
                let base = p * h * w;
                for i in 0..ho {
                    for j in 0..wo {
                        let r = iv[o];
                        if !(r >= 0.0 && r.fract() == 0.0 && r < k as f64) {
                            return None;
                        }
                        let r = r as usize;
                        let (ra, rb) = (r / kw, r % kw);
                        let y = (i * sh + ra * dh) as isize - phi;
                        let x = (j * sw + rb * dw) as isize - pwi;
                        if y >= 0 && y < hi && x >= 0 && x < wi {
                            out[base + y as usize * w + x as usize] = gv[o] + 0.0;
                        }
                        o += 1;
                    }
                }
            }
            self.pt_write(vgx, 0, &out)?;
            self.pt_charge(cost);
            return Some(());
        }
        let (vx, vo, vi) = (v0, v1, v2);
        if vx.len != xn || vo.len != on || vi.len != on || vi.kind != KIND_F64 {
            return None;
        }
        let cost = self.pt_admit(on.checked_mul(k)?.checked_add(on)?, xn.saturating_add(on.saturating_mul(2)))?;
        let xv = self.pt_read_all(vx)?;
        let mut vals = zeroed(on)?;
        let mut idx = zeroed(on)?;
        let mut o = 0usize;
        for p in 0..nc {
            if p % 64 == 63 && self.native_kernel_interrupted() {
                return None;
            }
            let base = p * h * w;
            for i in 0..ho {
                for j in 0..wo {
                    let (mut m, mut best, mut at) = (f64::NEG_INFINITY, f64::NEG_INFINITY, 0usize);
                    let mut r = 0usize;
                    for ra in 0..kh {
                        let y = (i * sh + ra * dh) as isize - phi;
                        for rb in 0..kw {
                            let x = (j * sw + rb * dw) as isize - pwi;
                            let v = if y >= 0 && y < hi && x >= 0 && x < wi { xv[base + y as usize * w + x as usize] } else { f64::NEG_INFINITY };
                            if v > m || v.is_nan() {
                                m = v;
                            }
                            if v > best || (v.is_nan() && !best.is_nan()) {
                                best = v;
                                at = r;
                            }
                            r += 1;
                        }
                    }
                    vals[o] = m;
                    idx[o] = at as f64;
                    o += 1;
                }
            }
        }
        self.pt_write(vo, 0, &vals)?;
        self.pt_write(vi, 0, &idx)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(C, A, B, O, shape, stridesC, stridesA, stridesB)`: tensor.js's
    /// `where`, `O[i] = C[.] ? A[.] : B[.]`.
    fn pt_where(&mut self, a: &[Value]) -> Option<()> {
        let (vc, va, vb, vo) = (
            self.pt_view(arg(a, 0))?,
            self.pt_view(arg(a, 1))?,
            self.pt_view(arg(a, 2))?,
            self.pt_view(arg(a, 3))?,
        );
        if vo.buffer == vc.buffer || vo.buffer == va.buffer || vo.buffer == vb.buffer {
            return None;
        }
        let shape = self.pt_ints(arg(a, 4))?;
        let (sc, sa, sb) = (self.pt_ints(arg(a, 5))?, self.pt_ints(arg(a, 6))?, self.pt_ints(arg(a, 7))?);
        if sc.len() != shape.len() || sa.len() != shape.len() || sb.len() != shape.len() {
            return None;
        }
        let n = shape.iter().try_fold(1usize, |acc, &d| acc.checked_mul(d))?;
        if vo.len != n {
            return None;
        }
        let cost = self.pt_admit(n, n.saturating_add(vc.len).saturating_add(va.len).saturating_add(vb.len))?;
        let mut out = zeroed(n)?;
        if n > 0 {
            if max_offset(&shape, &sc, 0)? >= vc.len || max_offset(&shape, &sa, 0)? >= va.len || max_offset(&shape, &sb, 0)? >= vb.len {
                return None;
            }
            let (cv, av, bv) = (self.pt_read_all(vc)?, self.pt_read_all(va)?, self.pt_read_all(vb)?);
            let done = walk(&shape, [&sc, &sa, &sb], || self.native_kernel_interrupted(), |i, [c, x, y]| {
                out[i] = if truthy(cv[c]) { av[x] } else { bv[y] };
            });
            if !done {
                return None;
            }
        }
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// A number argument.
fn num_arg(args: &[Value], i: usize) -> Option<f64> {
    let v = arg(args, i);
    v.is_number().then(|| v.as_f64())
}

/// A non-negative safe-integer number, as an index.
fn as_index(v: Value) -> Option<usize> {
    if !v.is_number() {
        return None;
    }
    let x = v.as_f64();
    if !(0.0..=9007199254740991.0).contains(&x) || x.fract() != 0.0 {
        return None;
    }
    usize::try_from(x as u64).ok()
}

fn int_arg(args: &[Value], i: usize) -> Option<usize> {
    as_index(arg(args, i))
}

#[allow(dead_code)]
const _: () = assert!(native::PY_TENSOR != native::HOST_CALL);
