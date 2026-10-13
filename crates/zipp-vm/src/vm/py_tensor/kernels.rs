//! The native loops behind the Python runtime's `_zipp_tensor` kernels, as
//! code that runs over any [`Host`]: the engine itself (`vm::py_tensor`, the
//! `all` artifact and the native CLI), or the `torch` package's own
//! WebAssembly module (zipp_torch.wasm), which receives the arguments and
//! the bytes they view from the engine (see `wire`). The same source, the
//! same operations in the same order: the results are the same bits.
//!
//! Nothing here names the engine: arguments arrive as [`Arg`]s, typed-array
//! bytes through [`Host::bytes`] / [`Host::bytes_mut`], and the budget
//! through [`Host::admits`] / [`Host::charge`]. See `vm/py_tensor.rs` for
//! the contract (same values or decline) and the budget.
#![allow(clippy::too_many_arguments)]
use super::jsmath::{f16_bits_to_f64, f64_to_f16_bits, math_unary, to_uint_modular, M};
use super::args::*;
use super::{fft, linalg, quant, sparse};

/// What the kernels need from where they run.
pub(crate) trait Host {
    /// The whole of array buffer `buffer`, or `None` when it is detached.
    fn bytes(&self, buffer: u32) -> Option<&[u8]>;
    fn bytes_mut(&mut self, buffer: u32) -> Option<&mut [u8]>;
    /// Whether the budget covers `cost` steps and `transient` bytes of
    /// working memory (`Vm::native_kernel_admits`).
    fn admits(&self, cost: u64, transient: usize) -> bool;
    /// Charge `cost` steps for work done.
    fn charge(&mut self, cost: u64);
    /// A host abort request (`Vm::native_kernel_interrupted`).
    fn interrupted(&self) -> bool;
}

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
// OP_ALL_FINITE (11) is in `args`: the engine answers it when it has no kernels.
const OP_WHERE: u32 = 12;
const OP_MATMUL_NT: u32 = 13;
const OP_MAX_POOL2D: u32 = 14;
const OP_MAX_POOL2D_BACKWARD: u32 = 15;
const OP_FFT: u32 = 16;
const OP_LINALG: u32 = 17;
const OP_SPMM: u32 = 18;
const OP_SP_COALESCE: u32 = 19;
const OP_SP_KEYS: u32 = 20;
const OP_SP_SCATTER: u32 = 21;
const OP_SP_MERGE: u32 = 22;
const OP_INDEX_SELECT: u32 = 23;
const OP_Q_QUANTIZE: u32 = 24;
const OP_Q_DEQUANTIZE: u32 = 25;
const OP_Q_MATMUL: u32 = 26;
const OP_Q_CONV2D: u32 = 27;
const OP_Q_REQUANT: u32 = 28;
const OP_Q_FAKE: u32 = 29;
const OP_ARANGE: u32 = 30;
const OP_OPTIM: u32 = 31;
const OP_CELL: u32 = 32;
const OP_RNG: u32 = 33;

/// `x` as a store into an element of `kind` leaves it: the typed-array
/// conversion (`helpers_numeric::ta_encode`), read back.
#[inline]
fn stored(kind: u8, x: f64) -> f64 {
    match kind {
        KIND_F32 => x as f32 as f64,
        KIND_U8 => to_uint_modular(x, 8) as f64,
        KIND_U16 => to_uint_modular(x, 16) as f64,
        KIND_F16 => f16_bits_to_f64(f64_to_f16_bits(x)),
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

/// The output block a matmul keeps in registers: rows of A by columns of B.
const MM_ROWS: usize = 4;
const MM_COLS: usize = 4;

/// Outputs (i..i+4, j..j+4) of an [m, k] @ [k, n] product, each the sum of
/// its k products in p order from 0 in an f64 (the row loop's value): the
/// four A values and the B row chunk are read once per p for all sixteen.
fn mm_block(out: &mut [f64], av: &[f64], bv: &[f64], i: usize, j: usize, k: usize, n: usize) {
    let a0 = &av[i * k..(i + 1) * k];
    let a1 = &av[(i + 1) * k..(i + 2) * k];
    let a2 = &av[(i + 2) * k..(i + 3) * k];
    let a3 = &av[(i + 3) * k..(i + 4) * k];
    let (mut c0, mut c1, mut c2, mut c3) = ([0.0f64; MM_COLS], [0.0f64; MM_COLS], [0.0f64; MM_COLS], [0.0f64; MM_COLS]);
    for p in 0..k {
        let b = &bv[p * n + j..p * n + j + MM_COLS];
        let (x0, x1, x2, x3) = (a0[p], a1[p], a2[p], a3[p]);
        for c in 0..MM_COLS {
            c0[c] += x0 * b[c];
            c1[c] += x1 * b[c];
            c2[c] += x2 * b[c];
            c3[c] += x3 * b[c];
        }
    }
    out[i * n + j..i * n + j + MM_COLS].copy_from_slice(&c0);
    out[(i + 1) * n + j..(i + 1) * n + j + MM_COLS].copy_from_slice(&c1);
    out[(i + 2) * n + j..(i + 2) * n + j + MM_COLS].copy_from_slice(&c2);
    out[(i + 3) * n + j..(i + 3) * n + j + MM_COLS].copy_from_slice(&c3);
}

/// Columns `j0..` of one output row (`row`, starting at column j0, zeroed):
/// `row[j] += a[p] * b[p, j0 + j]` for p in order, the row loop.
fn mm_row_tail(row: &mut [f64], arow: &[f64], bv: &[f64], n: usize, j0: usize) {
    for (p, &x) in arow.iter().enumerate() {
        let brow = &bv[p * n + j0..(p + 1) * n];
        for (r, &y) in row.iter_mut().zip(brow) {
            *r += x * y;
        }
    }
}

/// One float32 / float64 element of a little-endian chunk, as f64.
#[inline(always)]
fn le32(c: &[u8]) -> f64 {
    f32::from_le_bytes(<[u8; 4]>::try_from(c).unwrap_or([0; 4])) as f64
}

#[inline(always)]
fn le64(c: &[u8]) -> f64 {
    f64::from_le_bytes(<[u8; 8]>::try_from(c).unwrap_or([0; 8]))
}

/// `out[i] = f(a[i], b[i])` over float32 (`kind` KIND_F32) or float64
/// little-endian bytes, each element read as its f64 value and the result
/// stored as `pt_write` stores it: the f64 loop over `pt_read` and
/// `pt_write`, without their two f64 copies. `b_one`: `b` is one element,
/// paired with every `a[i]`; `flip`: the pair is `f(b, a[i])`. (Chunks
/// read through fixed-size arrays, so the loops vectorize.)
#[inline(always)]
fn bytes_map2(kind: u8, a: &[u8], b: &[u8], out: &mut [u8], b_one: bool, flip: bool, f: impl Fn(f64, f64) -> f64) {
    if kind == KIND_F32 {
        if b_one {
            let y = le32(b.get(..4).unwrap_or(&[0; 4]));
            if flip {
                for (o, x) in out.chunks_exact_mut(4).zip(a.chunks_exact(4)) {
                    o.copy_from_slice(&(f(y, le32(x)) as f32).to_le_bytes());
                }
            } else {
                for (o, x) in out.chunks_exact_mut(4).zip(a.chunks_exact(4)) {
                    o.copy_from_slice(&(f(le32(x), y) as f32).to_le_bytes());
                }
            }
        } else {
            for (o, (x, y)) in out.chunks_exact_mut(4).zip(a.chunks_exact(4).zip(b.chunks_exact(4))) {
                o.copy_from_slice(&(f(le32(x), le32(y)) as f32).to_le_bytes());
            }
        }
    } else if b_one {
        let y = le64(b.get(..8).unwrap_or(&[0; 8]));
        if flip {
            for (o, x) in out.chunks_exact_mut(8).zip(a.chunks_exact(8)) {
                o.copy_from_slice(&f(y, le64(x)).to_le_bytes());
            }
        } else {
            for (o, x) in out.chunks_exact_mut(8).zip(a.chunks_exact(8)) {
                o.copy_from_slice(&f(le64(x), y).to_le_bytes());
            }
        }
    } else {
        for (o, (x, y)) in out.chunks_exact_mut(8).zip(a.chunks_exact(8).zip(b.chunks_exact(8))) {
            o.copy_from_slice(&f(le64(x), le64(y)).to_le_bytes());
        }
    }
}

/// `out[i] = f(a[i])` over float32/float64 bytes, as `bytes_map2`.
#[inline(always)]
fn bytes_map1(kind: u8, a: &[u8], out: &mut [u8], f: impl Fn(f64) -> f64) {
    if kind == KIND_F32 {
        for (o, x) in out.chunks_exact_mut(4).zip(a.chunks_exact(4)) {
            o.copy_from_slice(&(f(le32(x)) as f32).to_le_bytes());
        }
    } else {
        for (o, x) in out.chunks_exact_mut(8).zip(a.chunks_exact(8)) {
            o.copy_from_slice(&f(le64(x)).to_le_bytes());
        }
    }
}

/// A zeroed byte buffer of `n`, or `None` when it cannot be allocated.
fn zeroed_bytes(n: usize) -> Option<Vec<u8>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).ok()?;
    v.resize(n, 0);
    Some(v)
}

// ---- random draws: tensor.js's MT19937 and PyTorch's CPU transforms --------------------

/// 2**-24: a 24-bit uniform's scale.
const F24: f64 = 1.0 / 16_777_216.0;

/// tensor.js's generator state: 624 words and the position of the next.
struct Mt {
    w: [u32; 624],
    i: usize,
}

impl Mt {
    /// `mtTwist`: the 624 words regenerated in place, in order.
    fn twist(&mut self) {
        let m = &mut self.w;
        for k in 0..624 {
            let y = (m[k] & 0x8000_0000) | (m[(k + 1) % 624] & 0x7fff_ffff);
            m[k] = m[(k + 397) % 624] ^ (y >> 1) ^ if y & 1 != 0 { 0x9908_b0df } else { 0 };
        }
        self.i = 0;
    }

    /// `next32`: the next tempered word.
    fn next(&mut self) -> u32 {
        if self.i >= 624 {
            self.twist();
        }
        let mut y = self.w[self.i];
        self.i += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `nextDouble`: 53 bits, the first word high.
    fn next_double(&mut self) -> f64 {
        let (hi, lo) = (self.next(), self.next());
        ((hi & 0x1f_ffff) as f64 * 4_294_967_296.0 + lo as f64) / 9_007_199_254_740_992.0
    }
}

// avx_mathfun's float32 constants, rounded from their double values as
// tensor.js's `AVX` table rounds them (`Math.fround`).
const AVX_SQRTHF: f32 = 0.707_106_781_186_547_524_f64 as f32;
const AVX_MIN_NORM: f32 = 1.175_494_35e-38_f64 as f32;
const AVX_P: [f32; 9] = [
    7.037_683_629_2e-2_f64 as f32,
    -1.151_461_031_0e-1_f64 as f32,
    1.167_699_874_0e-1_f64 as f32,
    -1.242_014_084_6e-1_f64 as f32,
    1.424_932_278_7e-1_f64 as f32,
    -1.666_805_766_5e-1_f64 as f32,
    2.000_071_476_5e-1_f64 as f32,
    -2.499_999_399_3e-1_f64 as f32,
    3.333_333_117_4e-1_f64 as f32,
];
const AVX_Q1: f32 = -2.121_944_40e-4_f64 as f32;
const AVX_Q2: f32 = 0.693_359_375_f64 as f32;
const AVX_DP1: f32 = -0.785_156_25_f64 as f32;
const AVX_DP2: f32 = -2.418_756_484_985_351_562_5e-4_f64 as f32;
const AVX_DP3: f32 = -3.774_894_977_445_941_08e-8_f64 as f32;
const AVX_S0: f32 = -1.951_529_589_1e-4_f64 as f32;
const AVX_S1: f32 = 8.332_160_873_6e-3_f64 as f32;
const AVX_S2: f32 = -1.666_665_461_1e-1_f64 as f32;
const AVX_C0: f32 = 2.443_315_711_809_948e-5_f64 as f32;
const AVX_C1: f32 = -1.388_731_625_493_765e-3_f64 as f32;
const AVX_C2: f32 = 4.166_664_568_298_827e-2_f64 as f32;
const AVX_FOPI: f32 = 1.273_239_544_735_16_f64 as f32;
const AVX_TWO_PI: f32 = (2.0 * std::f64::consts::PI) as f32;

/// tensor.js's `avxLog`: log256_ps for one positive float32 lane. Each
/// float32 operation rounds as the JavaScript's `Math.fround` after the
/// same double operation does.
fn avx_log(x: f32) -> f32 {
    let x = if x < AVX_MIN_NORM { AVX_MIN_NORM } else { x };
    let u = x.to_bits();
    let e0 = (u >> 23) as i32 - 0x7f;
    let mut v = f32::from_bits((u & 0x807f_ffff) | 0x3f00_0000);
    let mut e = e0 as f32 + 1.0;
    let below = v < AVX_SQRTHF;
    let tmp = if below { v } else { 0.0 };
    v -= 1.0;
    e -= if below { 1.0 } else { 0.0 };
    v += tmp;
    let z = v * v;
    let mut y = AVX_P[0];
    for p in &AVX_P[1..] {
        y *= v;
        y += *p;
    }
    y *= v;
    y *= z;
    y += e * AVX_Q1;
    y -= z * 0.5;
    v += y;
    v + e * AVX_Q2
}

/// tensor.js's `avxSincos`: sincos256_ps for one float32 lane, (sin, cos).
fn avx_sincos(x: f32) -> (f32, f32) {
    let mut neg_sin = x < 0.0;
    let mut x = x.abs();
    let mut j = (x * AVX_FOPI).trunc() as i32;
    j = (j + 1) & !1;
    let y = j as f32;
    let (swap_sin, first_poly) = (j & 4 != 0, j & 2 == 0);
    x += y * AVX_DP1;
    x += y * AVX_DP2;
    x += y * AVX_DP3;
    let neg_cos = !(j - 2) & 4 != 0;
    neg_sin = neg_sin != swap_sin;
    let z = x * x;
    let mut yc = AVX_C0 * z;
    yc += AVX_C1;
    yc *= z;
    yc += AVX_C2;
    yc *= z;
    yc *= z;
    yc -= z * 0.5;
    yc += 1.0;
    let mut ys = AVX_S0 * z;
    ys += AVX_S1;
    ys *= z;
    ys += AVX_S2;
    ys *= z;
    ys *= x;
    ys += x;
    let (sin2, sin1) = if first_poly { (ys, 0.0) } else { (0.0, yc) };
    let s = sin1 + sin2;
    let c = (yc - sin1) + (ys - sin2);
    (if neg_sin { -s } else { s }, if neg_cos { -c } else { c })
}

/// tensor.js's `fmaF`: `a * b + c` for float32 values, rounded to float32
/// once (the exact product in a double; the sum's rounding error decides a
/// half-way case).
fn fma_f(a: f64, b: f64, c: f64) -> f64 {
    let fr = |x: f64| x as f32 as f64;
    let (p, t) = (a * b, a * b + c);
    let r = fr(t);
    let d = t - r;
    if d == 0.0 {
        return r;
    }
    let other = r + 2.0 * d;
    if fr(other) != other {
        return r;
    }
    let bb = t - p;
    let e = (p - (t - bb)) + (c - bb);
    if e == 0.0 || (e > 0.0) != (d > 0.0) {
        r
    } else {
        other
    }
}

/// tensor.js's `avxBlock`: 16 float32 uniforms (as doubles) to normals.
fn avx_block(d: &mut [f64], mean: f32, std: f32) {
    for j in 0..8 {
        let u1 = (1.0 - d[j]) as f32;
        let u2 = d[j + 8] as f32;
        let radius = (-2.0 * avx_log(u1)).sqrt();
        let (s, c) = avx_sincos(AVX_TWO_PI * u2);
        let (n1, n2) = (radius * c, radius * s);
        d[j] = fma_f(n1 as f64, std as f64, mean as f64);
        d[j + 8] = fma_f(n2 as f64, std as f64, mean as f64);
    }
}

/// tensor.js's `normalBlock` for float64 (no rounding between operations).
fn normal_block64(d: &mut [f64], mean: f64, std: f64) {
    for j in 0..8 {
        let (u1, u2) = (1.0 - d[j], d[j + 8]);
        let radius = (-2.0 * math_unary(M::Log, u1)).sqrt();
        let theta = 2.0 * std::f64::consts::PI * u2;
        d[j] = radius * math_unary(M::Cos, theta) * std + mean;
        d[j + 8] = radius * math_unary(M::Sin, theta) * std + mean;
    }
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

/// `__zipp_py_native(op, ...)`. `true`/`false` for a kernel that ran or
/// declined; `OP_ALL_FINITE` answers `true`/`false`, or `null` when it
/// declines.
pub(crate) fn run<H: Host + ?Sized>(h: &mut H, op: u32, a: &[Arg]) -> Outcome {
match op {
        OP_MATMUL => Outcome::ran(h.pt_matmul(a, false).is_some()),
        OP_MATMUL_NT => Outcome::ran(h.pt_matmul(a, true).is_some()),
        OP_MAX_POOL2D => Outcome::ran(h.pt_max_pool2d(a, false).is_some()),
        OP_MAX_POOL2D_BACKWARD => Outcome::ran(h.pt_max_pool2d(a, true).is_some()),
        OP_CONV2D => Outcome::ran(h.pt_conv2d(a, false).is_some()),
        OP_CONV2D_BACKWARD => Outcome::ran(h.pt_conv2d(a, true).is_some()),
        OP_CONV1D => Outcome::ran(h.pt_conv1d(a).is_some()),
        OP_CONV1D_BACKWARD => Outcome::ran(h.pt_conv1d_backward(a).is_some()),
        OP_BINARY => Outcome::ran(h.pt_binary(a).is_some()),
        OP_UNARY => Outcome::ran(h.pt_unary(a).is_some()),
        OP_REDUCE => Outcome::ran(h.pt_reduce(a).is_some()),
        OP_SOFTMAX => Outcome::ran(h.pt_softmax(a).is_some()),
        OP_GATHER => Outcome::ran(h.pt_gather(a).is_some()),
        OP_ALL_FINITE => match h.pt_all_finite(a) {
            Some(b) => Outcome::Answer(b),
            None => Outcome::Null,
        },
        OP_WHERE => Outcome::ran(h.pt_where(a).is_some()),
        OP_FFT => Outcome::ran(h.pt_fft(a).is_some()),
        OP_LINALG => Outcome::ran(h.pt_linalg(a).is_some()),
        OP_SPMM => Outcome::ran(h.pt_spmm(a).is_some()),
        OP_SP_COALESCE => Outcome::ran(h.pt_sp_coalesce(a).is_some()),
        OP_SP_KEYS => Outcome::ran(h.pt_sp_keys(a).is_some()),
        OP_SP_SCATTER => Outcome::ran(h.pt_sp_scatter(a).is_some()),
        OP_SP_MERGE => Outcome::ran(h.pt_sp_merge(a).is_some()),
        OP_INDEX_SELECT => Outcome::ran(h.pt_index_select(a).is_some()),
        OP_Q_QUANTIZE => Outcome::ran(h.pt_q_quantize(a).is_some()),
        OP_Q_DEQUANTIZE => Outcome::ran(h.pt_q_dequantize(a).is_some()),
        OP_Q_MATMUL => Outcome::ran(h.pt_q_matmul(a).is_some()),
        OP_Q_CONV2D => Outcome::ran(h.pt_q_conv2d(a).is_some()),
        OP_Q_REQUANT => Outcome::ran(h.pt_q_requant(a).is_some()),
        OP_Q_FAKE => Outcome::ran(h.pt_q_fake(a).is_some()),
        OP_ARANGE => Outcome::ran(h.pt_arange(a).is_some()),
        OP_OPTIM => Outcome::ran(h.pt_optim(a).is_some()),
        OP_CELL => Outcome::ran(h.pt_cell(a).is_some()),
        OP_RNG => Outcome::ran(h.pt_rng(a).is_some()),
        OP_LOCAL => Outcome::Answer(true),
        _ => Outcome::Declined,
}
}

// ---- argument access ------------------------------------------------------------

pub(crate) trait Kernels: Host {
    /// A storage view: a Float32Array, Float64Array, Float16Array,
    /// Uint8Array or Uint16Array whose buffer is live and covers it.
    fn pt_view(&self, v: &Arg) -> Option<View> {
        match v {
            Arg::View(view) if matches!(view.kind, KIND_U8 | KIND_U16 | KIND_F32 | KIND_F64 | KIND_F16) => Some(*view),
            _ => None,
        }
    }

    /// Elements `start..start + count` of `v`, as f64.
    fn pt_read(&self, v: View, start: usize, count: usize) -> Option<Vec<f64>> {
        if start.checked_add(count)? > v.len {
            return None;
        }
        let size = v.size();
        let lo = v.offset.checked_add(start.checked_mul(size)?)?;
        let hi = lo.checked_add(count.checked_mul(size)?)?;
        let data = self.bytes(v.buffer)?;
        if hi > data.len() {
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
            KIND_F16 => out.extend(bytes.chunks_exact(2).map(|c| f16_bits_to_f64(u16::from_le_bytes([c[0], c[1]])))),
            KIND_U16 => out.extend(bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as f64)),
            KIND_I8 => out.extend(bytes.iter().map(|&b| b as i8 as f64)),
            _ => out.extend(bytes.iter().map(|&b| b as f64)),
        }
        Some(out)
    }

    /// The bytes of `v`'s elements.
    fn pt_bytes(&self, v: View) -> Option<&[u8]> {
        let lo = v.offset;
        let hi = lo.checked_add(v.len.checked_mul(v.size())?)?;
        self.bytes(v.buffer)?.get(lo..hi)
    }

    /// Store `bytes` (already in `v`'s element format) as `v`'s elements.
    fn pt_write_bytes(&mut self, v: View, bytes: &[u8]) -> Option<()> {
        let lo = v.offset;
        let hi = lo.checked_add(v.len.checked_mul(v.size())?)?;
        if bytes.len() != hi - lo {
            return None;
        }
        self.bytes_mut(v.buffer)?.get_mut(lo..hi)?.copy_from_slice(bytes);
        Some(())
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
        let data = self.bytes_mut(v.buffer)?;
        if hi > data.len() {
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
            KIND_F16 => {
                for (c, &x) in bytes.chunks_exact_mut(2).zip(values) {
                    c.copy_from_slice(&f64_to_f16_bits(x).to_le_bytes());
                }
            }
            KIND_U16 => {
                for (c, &x) in bytes.chunks_exact_mut(2).zip(values) {
                    c.copy_from_slice(&(to_uint_modular(x, 16) as u16).to_le_bytes());
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
    fn pt_ints(&self, v: &Arg) -> Option<Vec<usize>> {
        match v {
            Arg::Ints(Some(items)) => Some(items.clone()),
            _ => None,
        }
    }

    /// Price `units` of work, or `None` when the budget cannot cover it or
    /// a heap ceiling has no room for `elems` f64 working values.
    fn pt_admit(&self, units: usize, elems: usize) -> Option<u64> {
        let cost = STEPS_BASE.saturating_add((units as u64).saturating_mul(STEPS_PER_UNIT));
        self.admits(cost, elems.saturating_mul(8)).then_some(cost)
    }

    fn pt_charge(&mut self, cost: u64) {
        self.charge(cost);
    }

    // ---- kernels ----------------------------------------------------------------------

    /// `(A, B, O, aOff, bOff, oOff, m, k, n)`: one [m,k] @ [k,n] product of a
    /// batch, tensor.js's `matmul` loop: i-k-j order over one f64 row, each
    /// output rounded once on store. `trans_b`: B is stored as its [n,k]
    /// transpose (the `transB` loop); element (i, j) still sums its k
    /// products in p order from 0 in an f64, so it is the same value.
    fn pt_matmul(&mut self, a: &[Arg], trans_b: bool) -> Option<()> {
        let (va, vb, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?);
        if vo.buffer == va.buffer || vo.buffer == vb.buffer {
            return None;
        }
        let (ao, bo, oo) = (int_arg(a, 3)?, int_arg(a, 4)?, int_arg(a, 5)?);
        let (m, k, n) = (int_arg(a, 6)?, int_arg(a, 7)?, int_arg(a, 8)?);
        let (mk, kn, mn) = (m.checked_mul(k)?, k.checked_mul(n)?, m.checked_mul(n)?);
        // (the transposed operand's [k, n] copy is working memory too)
        let cost = self.pt_admit(mn.checked_mul(k)?.checked_add(mn)?, mk.saturating_add(kn).saturating_add(mn).saturating_add(if trans_b { kn } else { 0 }))?;
        let av = self.pt_read(va, ao, mk)?;
        let bv = self.pt_read(vb, bo, kn)?;
        // B as [k, n] rows either way: the transposed operand's [n, k] rows
        // laid out once, so both forms run the same loops below (element
        // (i, j) is still the sum of its k products in p order).
        let bv = if trans_b {
            let mut t = zeroed(kn)?;
            for j in 0..n {
                for p in 0..k {
                    t[p * n + j] = bv[j * k + p];
                }
            }
            t
        } else {
            bv
        };
        let mut out = zeroed(mn)?;
        let rows_per_poll = (POLL_UNITS / k.max(1).saturating_mul(n).max(1)).max(1);
        let mut since_poll = 0usize;
        let mut i = 0;
        // Blocks of MM_ROWS x MM_COLS outputs, each output a sum in an f64
        // register that starts at 0 and adds its products in p order: what
        // the row loop computes for it, with B's row and A's values read
        // once per p for the whole block.
        // (Narrow products keep the row loop: a block's setup costs more
        // than it saves below two blocks a row.)
        while n >= 2 * MM_COLS && i + MM_ROWS <= m {
            since_poll += MM_ROWS;
            if since_poll >= rows_per_poll {
                since_poll = 0;
                if self.interrupted() {
                    return None;
                }
            }
            let mut j = 0;
            while j + MM_COLS <= n {
                mm_block(&mut out, &av, &bv, i, j, k, n);
                j += MM_COLS;
            }
            if j < n {
                for r in 0..MM_ROWS {
                    mm_row_tail(&mut out[(i + r) * n + j..(i + r + 1) * n], &av[(i + r) * k..(i + r + 1) * k], &bv, n, j);
                }
            }
            i += MM_ROWS;
        }
        while i < m {
            if self.interrupted() {
                return None;
            }
            mm_row_tail(&mut out[i * n..(i + 1) * n], &av[i * k..(i + 1) * k], &bv, n, 0);
            i += 1;
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
    fn pt_conv2d(&mut self, a: &[Arg], backward: bool) -> Option<()> {
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
                                if self.interrupted() {
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
                                if self.interrupted() {
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
    fn pt_conv1d(&mut self, a: &[Arg]) -> Option<()> {
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
            if self.interrupted() {
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
    fn pt_conv1d_backward(&mut self, a: &[Arg]) -> Option<()> {
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
            if self.interrupted() {
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
    fn pt_binary(&mut self, a: &[Arg]) -> Option<()> {
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
        // add/sub/mul/div of one float dtype over equal contiguous operands
        // or one of them a single element: the same pairs and the same f64
        // operation, read and stored straight from the bytes.
        let op = int_arg(a, 0)?;
        if n > 0 && (1..=4).contains(&op) && va.kind == vo.kind && vb.kind == vo.kind && matches!(vo.kind, KIND_F32 | KIND_F64) {
            let contiguous = contiguous_strides(&shape);
            let layout = if va.len == n && vb.len == n && sa == contiguous && sb == contiguous {
                Some((false, false))
            } else if va.len == n && sa == contiguous && vb.len == 1 && sb.iter().all(|&s| s == 0) {
                Some((true, false))
            } else if vb.len == n && sb == contiguous && va.len == 1 && sa.iter().all(|&s| s == 0) {
                Some((true, true))
            } else {
                None
            };
            if let Some((one, flip)) = layout {
                let mut out = zeroed_bytes(n.checked_mul(vo.size())?)?;
                {
                    let (x, y) = if flip { (self.pt_bytes(vb)?, self.pt_bytes(va)?) } else { (self.pt_bytes(va)?, self.pt_bytes(vb)?) };
                    let kd = vo.kind;
                    match op {
                        1 => bytes_map2(kd, x, y, &mut out, one, flip, |p, q| p + q),
                        2 => bytes_map2(kd, x, y, &mut out, one, flip, |p, q| p - q),
                        3 => bytes_map2(kd, x, y, &mut out, one, flip, |p, q| p * q),
                        _ => bytes_map2(kd, x, y, &mut out, one, flip, |p, q| p / q),
                    }
                }
                self.pt_write_bytes(vo, &out)?;
                self.pt_charge(cost);
                return Some(());
            }
        }
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
                let done = walk(&shape, [&sa, &sb], || self.interrupted(), |i, [x, y]| {
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

    /// `(O, start, step)`: tensor.js's `arange` loop for a float32 or
    /// float64 storage (the int64/int32 ones are float64 too): element i is
    /// `start + i * step`, stored as the typed array stores it.
    fn pt_arange(&mut self, a: &[Arg]) -> Option<()> {
        let vo = self.pt_view(arg(a, 0))?;
        if vo.kind != KIND_F32 && vo.kind != KIND_F64 {
            return None;
        }
        let (start, step) = (num_arg(a, 1)?, num_arg(a, 2)?);
        let n = vo.len;
        let cost = self.pt_admit(n, n)?;
        let mut out = Vec::new();
        out.try_reserve_exact(n).ok()?;
        for i in 0..n {
            out.push(start + i as f64 * step);
        }
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(kind, flags, P, G, M, V, W, M2, V2, W2, s0..s7)`: one SGD (kind 0)
    /// or Adam (kind 1) step of a float32/float64 parameter `P` from its
    /// gradient `G`: tensor.js's `optimStep` loop, element for element,
    /// every operation in its order and rounded where it rounds there. `P`
    /// is written in place; the state is read from `M`, `V`, `W` and the new
    /// state written to `M2`, `V2`, `W2` (each null where the update has
    /// none; `M` also on SGD's first step). The scalars arrive rounded to
    /// the dtype.
    fn pt_optim(&mut self, a: &[Arg]) -> Option<()> {
        let (kind, flags) = (int_arg(a, 0)?, int_arg(a, 1)?);
        let (vp, vg) = (self.pt_view(arg(a, 2))?, self.pt_view(arg(a, 3))?);
        if kind > 1 || !matches!(vp.kind, KIND_F32 | KIND_F64) || vg.kind != vp.kind || vg.len != vp.len {
            return None;
        }
        let adam = kind == 1;
        let (use_m, use_w) = (adam || flags & 4 != 0, adam && flags & 8 != 0);
        // A storage of the parameter's kind and length (`Some`), or null.
        let state = |i: usize| -> Option<Option<View>> {
            if arg(a, i).is_null() {
                return Some(None);
            }
            let v = self.pt_view(arg(a, i))?;
            (v.kind == vp.kind && v.len == vp.len).then_some(Some(v))
        };
        let (vm, vv, vw) = (state(4)?, state(5)?, state(6)?);
        let (vm2, vv2, vw2) = (state(7)?, state(8)?, state(9)?);
        // (SGD's old buffer is null on its first step)
        let m_ok = if adam { vm.is_some() } else { use_m || vm.is_none() };
        if !m_ok || vv.is_some() != adam || vw.is_some() != use_w || vm2.is_some() != use_m || vv2.is_some() != adam || vw2.is_some() != use_w {
            return None;
        }
        let first = use_m && !adam && vm.is_none();
        let views = [Some(vp), Some(vg), vm, vv, vw, vm2, vv2, vw2];
        for (i, x) in views.iter().enumerate() {
            for y in &views[i + 1..] {
                if let (Some(x), Some(y)) = (x, y) {
                    if x.buffer == y.buffer {
                        return None;
                    }
                }
            }
        }
        let mut s = [0.0f64; 8];
        for (k, v) in s.iter_mut().enumerate() {
            *v = num_arg(a, 10 + k)?;
        }
        let n = vp.len;
        let cost = self.pt_admit(n.saturating_mul(if adam { 4 } else { 2 }), n.saturating_mul(8))?;
        let mut p = self.pt_read_all(vp)?;
        let g = self.pt_read_all(vg)?;
        let read = |v: Option<View>| -> Option<Vec<f64>> {
            match v {
                Some(v) => self.pt_read_all(v),
                None => Some(Vec::new()),
            }
        };
        let (m, v, w) = (read(vm)?, read(vv)?, read(vw)?);
        let (mut m2, mut v2, mut w2) = (zeroed(if use_m { n } else { 0 })?, zeroed(if adam { n } else { 0 })?, zeroed(if use_w { n } else { 0 })?);
        let kd = vp.kind;
        let q = |x: f64| stored(kd, x);
        let neg = flags & 1 != 0;
        if !adam {
            let (lr, mom, damp1, wd) = (s[0], s[1], s[2], s[3]);
            let (decay, nest) = (flags & 2 != 0, flags & 16 != 0);
            for i in 0..n {
                let pv = p[i];
                let mut gv = if neg { -g[i] } else { g[i] };
                if decay {
                    gv = q(gv + q(pv * wd));
                }
                if use_m {
                    // (the first step's buffer is the gradient's clone)
                    let b = if first { gv } else { q(q(m[i] * mom) + q(gv * damp1)) };
                    m2[i] = b;
                    gv = if nest { q(gv + q(b * mom)) } else { b };
                }
                p[i] = pv - q(gv * lr);
            }
        } else {
            let (b1, ob1, b2, ob2, bc2, eps, ss, wd) = (s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]);
            let decay = flags & 2 != 0;
            let (coupled, decoupled) = (decay && flags & 4 == 0, decay && flags & 4 != 0);
            for i in 0..n {
                let pv = p[i];
                let mut gv = if neg { -g[i] } else { g[i] };
                let mut value = pv;
                if coupled {
                    gv = q(gv + q(pv * wd));
                } else if decoupled {
                    value = q(pv * wd);
                }
                let mv = q(q(m[i] * b1) + q(gv * ob1));
                let vq = q(q(v[i] * b2) + q(q(gv * gv) * ob2));
                m2[i] = mv;
                v2[i] = vq;
                let mut d = vq;
                if use_w {
                    let wv = w[i];
                    d = if wv.is_nan() || vq.is_nan() { f64::NAN } else { js_max(wv, vq) };
                    w2[i] = d;
                }
                p[i] = value - q(q(mv / q(q(q(math_unary(M::Sqrt, d)) / bc2) + eps)) * ss);
            }
        }
        self.pt_write(vp, 0, &p)?;
        for (view, vals) in [(vm2, &m2), (vv2, &v2), (vw2, &w2)] {
            if let Some(view) = view {
                self.pt_write(view, 0, vals)?;
            }
        }
        self.pt_charge(cost);
        Some(())
    }

    /// `(kind, H, in0..in3, out0..out3)`: one of tensor.js's fused
    /// recurrent-cell loops, element for element (each operation in its
    /// order, rounded where it rounds there; sigmoid `1 / (1 + exp(-x))`).
    /// Storages are one float32/float64 kind, `[B, k * H]` gate blocks or
    /// `[B, H]` states; an unused slot is null.
    /// - 0 `lstmCell`: (G, C) -> (c2, h2, A, T).
    /// - 1, 2 `lstmCellBw`: (D, A, X) -> (dG, dX); kind 1's dX may be null.
    /// - 3 `gruCell`: (GI, GH) -> (z, n, r).
    /// - 4, 5 `gruCellBw`: (D, X, Y, GH) -> (dGI, dGH); kind 4 has no Y.
    fn pt_cell(&mut self, a: &[Arg]) -> Option<()> {
        let (kind, h) = (int_arg(a, 0)?, int_arg(a, 1)?);
        if kind > 5 || h == 0 {
            return None;
        }
        let slot = |i: usize| -> Option<Option<View>> {
            if arg(a, i).is_null() {
                return Some(None);
            }
            self.pt_view(arg(a, i)).map(Some)
        };
        let ins = [slot(2)?, slot(3)?, slot(4)?, slot(5)?];
        let outs = [slot(6)?, slot(7)?, slot(8)?, slot(9)?];
        // The gate block width (4H or 3H) and the slot each kind reads its
        // batch from.
        let (k, gates) = match kind {
            0 => (4, ins[0]?),
            1 | 2 => (4, ins[1]?),
            3 => (3, ins[0]?),
            _ => (3, ins[3]?),
        };
        let kd = gates.kind;
        if !matches!(kd, KIND_F32 | KIND_F64) || gates.len % (k * h) != 0 {
            return None;
        }
        let b = gates.len / (k * h);
        let n = b * h;
        // Every slot's expected length (0: null); any other storage declines.
        let (want_in, want_out): ([usize; 4], [usize; 4]) = match kind {
            0 => ([4 * n, n, 0, 0], [n, n, 4 * n, n]),
            1 => ([n, 4 * n, n, 0], [4 * n, if outs[1].is_some() { n } else { 0 }, 0, 0]),
            2 => ([n, 4 * n, n, 0], [4 * n, n, 0, 0]),
            3 => ([3 * n, 3 * n, 0, 0], [n, n, n, 0]),
            4 => ([n, n, 0, 3 * n], [3 * n, 3 * n, 0, 0]),
            _ => ([n, n, n, 3 * n], [3 * n, 3 * n, 0, 0]),
        };
        let mut views = Vec::new();
        for (slots, want) in [(&ins, &want_in), (&outs, &want_out)] {
            for (v, &len) in slots.iter().zip(want.iter()) {
                match (v, len) {
                    (None, 0) => {}
                    (Some(v), len) if len > 0 && v.kind == kd && v.len == len => views.push(*v),
                    _ => return None,
                }
            }
        }
        for (i, x) in views.iter().enumerate() {
            if views[i + 1..].iter().any(|y| y.buffer == x.buffer) {
                return None;
            }
        }
        let cost = self.pt_admit(n.saturating_mul(4), n.saturating_mul(24))?;
        let read = |v: Option<View>| -> Option<Vec<f64>> {
            match v {
                Some(v) => self.pt_read_all(v),
                None => Some(Vec::new()),
            }
        };
        let (i0, i1, i2, i3) = (read(ins[0])?, read(ins[1])?, read(ins[2])?, read(ins[3])?);
        let mut o: [Vec<f64>; 4] = [zeroed(want_out[0])?, zeroed(want_out[1])?, zeroed(want_out[2])?, zeroed(want_out[3])?];
        let q = |x: f64| stored(kd, x);
        let sig = |x: f64| 1.0 / (1.0 + math_unary(M::Exp, -x));
        let tanh = |x: f64| math_unary(M::Tanh, x);
        for bi in 0..b {
            for j in 0..h {
                let kk = bi * h + j;
                match kind {
                    0 => {
                        let g0 = 4 * h * bi;
                        let sf = q(sig(i0[g0 + h + j]));
                        let t1 = q(sf * i1[kk]);
                        let si = q(sig(i0[g0 + j]));
                        let tg = q(tanh(i0[g0 + 2 * h + j]));
                        let c2 = q(t1 + q(si * tg));
                        let so = q(sig(i0[g0 + 3 * h + j]));
                        let tc = q(tanh(c2));
                        o[0][kk] = c2;
                        o[1][kk] = so * tc;
                        o[2][g0 + j] = si;
                        o[2][g0 + h + j] = sf;
                        o[2][g0 + 2 * h + j] = tg;
                        o[2][g0 + 3 * h + j] = so;
                        o[3][kk] = tc;
                    }
                    1 => {
                        let g0 = 4 * h * bi;
                        let g = i0[kk];
                        let (si, sf, tg) = (i1[g0 + j], i1[g0 + h + j], i1[g0 + 2 * h + j]);
                        let (dsf, dsi, dtg) = (q(g * i2[kk]), q(g * tg), q(g * si));
                        o[0][g0 + j] = q(dsi * q(si * q(1.0 - si)));
                        o[0][g0 + h + j] = q(dsf * q(sf * q(1.0 - sf)));
                        o[0][g0 + 2 * h + j] = q(dtg * q(1.0 - q(tg * tg)));
                        if !o[1].is_empty() {
                            o[1][kk] = g * sf;
                        }
                    }
                    2 => {
                        let g0 = 4 * h * bi;
                        let g = i0[kk];
                        let (so, tc) = (i1[g0 + 3 * h + j], i2[kk]);
                        let (dso, dtc) = (q(g * tc), q(g * so));
                        o[0][g0 + 3 * h + j] = q(dso * q(so * q(1.0 - so)));
                        o[1][kk] = q(dtc * q(1.0 - q(tc * tc)));
                    }
                    3 => {
                        let g0 = 3 * h * bi;
                        let r = q(sig(q(i0[g0 + j] + i1[g0 + j])));
                        o[0][kk] = sig(q(i0[g0 + h + j] + i1[g0 + h + j]));
                        o[1][kk] = tanh(q(i0[g0 + 2 * h + j] + q(r * i1[g0 + 2 * h + j])));
                        o[2][kk] = r;
                    }
                    4 => {
                        let g0 = 3 * h * bi;
                        let z = i1[kk];
                        let daz = q(i0[kk] * q(z * q(1.0 - z)));
                        o[0][g0 + h + j] = daz;
                        o[1][g0 + h + j] = daz;
                    }
                    _ => {
                        let g0 = 3 * h * bi;
                        let (r, nn) = (i1[kk], i2[kk]);
                        let dan = q(i0[kk] * q(1.0 - q(nn * nn)));
                        let dr = q(dan * i3[g0 + 2 * h + j]);
                        let dar = q(dr * q(r * q(1.0 - r)));
                        o[0][g0 + j] = dar;
                        o[1][g0 + j] = dar;
                        o[0][g0 + 2 * h + j] = dan;
                        o[1][g0 + 2 * h + j] = dan * r;
                    }
                }
            }
        }
        for (view, vals) in outs.iter().zip(o.iter()) {
            if let Some(view) = view {
                self.pt_write(*view, 0, vals)?;
            }
        }
        self.pt_charge(cost);
        Some(())
    }

    /// `(mode, MT, POS, O, mean, std)`: tensor.js's MT19937 draws into `O`,
    /// word for word and value for value: `MT` the generator's 624 state
    /// words (a Uint8Array over them), `POS` a one-element Float64Array
    /// holding its position (0..=624); both are advanced in place.
    /// - 0 `rand` float32: `(w & 0xffffff) * 2**-24` per word.
    /// - 1 `randDouble`: `nextDouble`, 53 bits from two words.
    /// - 2 `randn` float32, 16 or more: the AVX2 blocks (`avxBlock`).
    /// - 3 `randn` float64, 16 or more: `normalBlock` in double.
    fn pt_rng(&mut self, a: &[Arg]) -> Option<()> {
        let mode = int_arg(a, 0)?;
        let (vm, vp, vo) = (self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?, self.pt_view(arg(a, 3))?);
        let (mean, std) = (num_arg(a, 4)?, num_arg(a, 5)?);
        let n = vo.len;
        let want = match mode {
            0 | 2 => KIND_F32,
            1 | 3 => KIND_F64,
            _ => return None,
        };
        if vm.kind != KIND_U8 || vm.len != 624 * 4 || vp.kind != KIND_F64 || vp.len != 1 || vo.kind != want || (mode >= 2 && n < 16) {
            return None;
        }
        if vm.buffer == vp.buffer || vm.buffer == vo.buffer || vp.buffer == vo.buffer {
            return None;
        }
        let pos = self.pt_read(vp, 0, 1)?[0];
        if !(0.0..=624.0).contains(&pos) || pos.fract() != 0.0 {
            return None;
        }
        let normal = mode >= 2;
        let cost = self.pt_admit(n.saturating_mul(if normal { 16 } else { 2 }), n.saturating_add(624))?;
        let mut mt = Mt { w: [0u32; 624], i: pos as usize };
        {
            let bytes = self.bytes(vm.buffer)?.get(vm.offset..vm.offset.checked_add(624 * 4)?)?;
            for (w, c) in mt.w.iter_mut().zip(bytes.chunks_exact(4)) {
                *w = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            }
        }
        let mut d = zeroed(n)?;
        match mode {
            0 => d.iter_mut().for_each(|x| *x = (mt.next() & 0xffffff) as f64 * F24),
            1 => d.iter_mut().for_each(|x| *x = mt.next_double()),
            2 => {
                let (mn, sd) = (mean as f32, std as f32);
                d.iter_mut().for_each(|x| *x = (mt.next() & 0xffffff) as f64 * F24);
                let mut b = 0;
                while b + 16 <= n {
                    avx_block(&mut d[b..b + 16], mn, sd);
                    b += 16;
                }
                if n % 16 != 0 {
                    let tail = &mut d[n - 16..];
                    tail.iter_mut().for_each(|x| *x = (mt.next() & 0xffffff) as f64 * F24);
                    avx_block(tail, mn, sd);
                }
            }
            _ => {
                d.iter_mut().for_each(|x| *x = mt.next_double());
                let mut b = 0;
                while b + 16 <= n {
                    normal_block64(&mut d[b..b + 16], mean, std);
                    b += 16;
                }
                if n % 16 != 0 {
                    let tail = &mut d[n - 16..];
                    tail.iter_mut().for_each(|x| *x = mt.next_double());
                    normal_block64(tail, mean, std);
                }
            }
        }
        self.pt_write(vo, 0, &d)?;
        {
            let bytes = self.bytes_mut(vm.buffer)?.get_mut(vm.offset..vm.offset + 624 * 4)?;
            for (w, c) in mt.w.iter().zip(bytes.chunks_exact_mut(4)) {
                c.copy_from_slice(&w.to_le_bytes());
            }
        }
        self.pt_write(vp, 0, &[mt.i as f64])?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(op, A, O, lo, hi)`: `O[i] = op(A[i])` (tensor.js's `unary`); op 27
    /// is `clamp` to `[lo, hi]`.
    fn pt_unary(&mut self, a: &[Arg]) -> Option<()> {
        let op = u32::try_from(int_arg(a, 0)?).ok()?;
        let (va, vo) = (self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?);
        if vo.buffer == va.buffer || vo.len != va.len {
            return None;
        }
        let cost = self.pt_admit(va.len, va.len)?;
        // A float op on its own dtype: the same f64 function, read and
        // stored straight from the bytes.
        if va.kind == vo.kind && matches!(vo.kind, KIND_F32 | KIND_F64) && (op == 27 || unary_fn(op).is_some()) {
            let mut out = zeroed_bytes(va.len.checked_mul(vo.size())?)?;
            {
                let x = self.pt_bytes(va)?;
                let kd = vo.kind;
                match op {
                    27 => {
                        let (lo, hi) = (num_arg(a, 3)?, num_arg(a, 4)?);
                        bytes_map1(kd, x, &mut out, |x| if x < lo { lo } else if x > hi { hi } else { x });
                    }
                    1 => bytes_map1(kd, x, &mut out, |x| -x),
                    2 => bytes_map1(kd, x, &mut out, |x| if x > 0.0 || x.is_nan() { x } else { 0.0 }),
                    8 => bytes_map1(kd, x, &mut out, |x| x * x),
                    _ => bytes_map1(kd, x, &mut out, unary_fn(op)?),
                }
            }
            self.pt_write_bytes(vo, &out)?;
            self.pt_charge(cost);
            return Some(());
        }
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
    fn pt_reduce(&mut self, a: &[Arg]) -> Option<()> {
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
            if self.interrupted() {
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
    fn pt_softmax(&mut self, a: &[Arg]) -> Option<()> {
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
            if self.interrupted() {
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
    fn pt_gather(&mut self, a: &[Arg]) -> Option<()> {
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
            let done = walk(&shape, [&st], || self.interrupted(), |i, [x]| {
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
    fn pt_all_finite(&mut self, a: &[Arg]) -> Option<bool> {
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
    fn pt_max_pool2d(&mut self, a: &[Arg], backward: bool) -> Option<()> {
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
                if p % 64 == 63 && self.interrupted() {
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
            if p % 64 == 63 && self.interrupted() {
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

    /// `(A, O, rows, nIn, n, mode, inverse, scale)`: tensor.js's `fft`
    /// (see `fft`): each row's transform of length n, scaled, rounded to
    /// O's element type on store.
    fn pt_fft(&mut self, a: &[Arg]) -> Option<()> {
        let (va, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?);
        if va.buffer == vo.buffer || !matches!(va.kind, KIND_F32 | KIND_F64) || !matches!(vo.kind, KIND_F32 | KIND_F64) {
            return None;
        }
        let (rows, n_in, n) = (int_arg(a, 2)?, int_arg(a, 3)?, int_arg(a, 4)?);
        let mode = u32::try_from(int_arg(a, 5)?).ok()?;
        let inverse = num_arg(a, 6)? != 0.0;
        let scale = num_arg(a, 7)?;
        if mode > 3 || n == 0 || n as u64 > (1u64 << 40) {
            return None;
        }
        let half = (n >> 1) + 1;
        let n_out = if mode == 1 { half } else { n };
        let in_len = rows.checked_mul(n_in)?.checked_mul(if mode == 1 || mode == 3 { 1 } else { 2 })?;
        let out_len = rows.checked_mul(n_out)?.checked_mul(if mode == 2 { 1 } else { 2 })?;
        if va.len != in_len || vo.len != out_len {
            return None;
        }
        // Bluestein's chirp index j*j mod 2n is exact in the JavaScript
        // loop's doubles only while j*j stays below 2**53.
        if !fft::smooth(n) && n > 94_906_265 {
            return None;
        }
        let per_row = fft::estimate_units(n);
        let units = (rows as u64).saturating_mul(per_row).saturating_add(fft::plan_units(n));
        let cost = self.pt_admit(usize::try_from(units).ok()?, in_len.saturating_add(out_len).saturating_add(fft::plan_elems(n)))?;
        let mut plan = fft::Plan::new(n, if inverse { 1.0 } else { -1.0 })?;
        let av = self.pt_read_all(va)?;
        let mut out = zeroed(out_len)?;
        if !fft::transform(&mut plan, &av, &mut out, rows, n_in, mode, scale, || self.interrupted()) {
            return None;
        }
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(R, C, V, D, O, nnz, m, k, n)`: tensor.js's `spSpmm` (see
    /// `sparse::spmm`), the [m, n] product of a sparse matrix (row, column
    /// and value of each of nnz nonzeros) and the dense [k, n] D, each result
    /// summed in an f64 and rounded once on store into O. Declines a uint8
    /// or bool output (tensor.js stores those itself).
    fn pt_spmm(&mut self, a: &[Arg]) -> Option<()> {
        let (vr, vc, vv, vd, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?, self.pt_view(arg(a, 3))?, self.pt_view(arg(a, 4))?);
        if vr.kind != KIND_F64 || vc.kind != KIND_F64 || vo.kind == KIND_U8 || vo.kind == KIND_U16 {
            return None;
        }
        if [vr.buffer, vc.buffer, vv.buffer, vd.buffer].contains(&vo.buffer) {
            return None;
        }
        let (nnz, m, k, n) = (int_arg(a, 5)?, int_arg(a, 6)?, int_arg(a, 7)?, int_arg(a, 8)?);
        let (kn, mn) = (k.checked_mul(n)?, m.checked_mul(n)?);
        if vr.len != nnz || vc.len != nnz || vv.len != nnz || vd.len != kn || vo.len != mn || n == 0 {
            return None;
        }
        let cost = self.pt_admit(nnz.checked_mul(n)?.checked_add(mn)?, nnz.saturating_mul(3).saturating_add(kn).saturating_add(mn))?;
        let rows = self.pt_read_all(vr)?;
        let cols = self.pt_read_all(vc)?;
        let vals = self.pt_read_all(vv)?;
        let dense = self.pt_read_all(vd)?;
        let mut acc = zeroed(mn)?;
        sparse::spmm(&rows, &cols, &vals, &dense, &mut acc, m, k, n, POLL_UNITS, || self.interrupted())?;
        self.pt_write(vo, 0, &acc)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(K, V, F, O, G, n, block)`: tensor.js's `spCoalesce` (see
    /// `sparse::coalesce`) over n nonzeros' keys K and values V: the kept
    /// entries' first positions into F, their summed values into O (both
    /// sized for n), their count into G[0]. Declines the uint8/bool and
    /// bfloat16 storages, whose sums tensor.js makes itself.
    fn pt_sp_coalesce(&mut self, a: &[Arg]) -> Option<()> {
        let (vk, vv, vf, vo, vg) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?, self.pt_view(arg(a, 3))?, self.pt_view(arg(a, 4))?);
        if vk.kind != KIND_F64 || vf.kind != KIND_F64 || vg.kind != KIND_F64 || vv.kind != vo.kind || matches!(vo.kind, KIND_U8 | KIND_U16) {
            return None;
        }
        let outs = [vf.buffer, vo.buffer, vg.buffer];
        if outs.contains(&vk.buffer) || outs.contains(&vv.buffer) || vf.buffer == vo.buffer || vf.buffer == vg.buffer || vo.buffer == vg.buffer {
            return None;
        }
        let (n, block) = (int_arg(a, 5)?, int_arg(a, 6)?);
        let nb = n.checked_mul(block)?;
        if vk.len != n || vv.len != nb || vf.len != n || vo.len != nb || vg.len != 1 || block == 0 {
            return None;
        }
        let log = (usize::BITS - n.leading_zeros()) as usize;
        let cost = self.pt_admit(n.checked_mul(log.max(1))?.checked_add(nb)?, n.saturating_mul(3).saturating_add(nb.saturating_mul(2)))?;
        let keys = self.pt_read_all(vk)?;
        let vals = self.pt_read_all(vv)?;
        let mut first = zeroed(n)?;
        let mut out = zeroed(nb)?;
        let kind = vo.kind;
        let groups = sparse::coalesce(&keys, &vals, block, |x| stored(kind, x), &mut first, &mut out, POLL_UNITS, || self.interrupted())?;
        self.pt_write(vf, 0, &first[..groups])?;
        self.pt_write(vo, 0, &out[..groups * block])?;
        self.pt_write(vg, 0, &[groups as f64])?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(A, I, O, outer, size, inner)`: tensor.js's `indexSelect`, a copy:
    /// for each of `outer` slices of A ([outer, size, inner]) the `inner`
    /// elements at each index of I (a negative one counting from the end)
    /// in turn. The elements move as they are (A and O share an element
    /// type); an index out of range declines, for the JavaScript loop to
    /// report.
    fn pt_index_select(&mut self, a: &[Arg]) -> Option<()> {
        let (va, vi, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?);
        if vi.kind != KIND_F64 || va.kind != vo.kind || vo.buffer == va.buffer || vo.buffer == vi.buffer {
            return None;
        }
        let (outer, size, inner) = (int_arg(a, 3)?, int_arg(a, 4)?, int_arg(a, 5)?);
        let n = vi.len;
        if va.len != outer.checked_mul(size)?.checked_mul(inner)? || vo.len != outer.checked_mul(n)?.checked_mul(inner)? {
            return None;
        }
        let cost = self.pt_admit(vo.len.checked_add(n)?, n.saturating_add(vo.len))?;
        let idx = self.pt_read_all(vi)?;
        let mut at = Vec::new();
        at.try_reserve_exact(n).ok()?;
        for &j in &idx {
            let j = if j < 0.0 { j + size as f64 } else { j };
            if !(j >= 0.0) || j.fract() != 0.0 || j >= size as f64 {
                return None;
            }
            at.push(j as usize);
        }
        let es = va.size();
        let (a_hi, o_hi) = (va.offset.checked_add(va.len.checked_mul(es)?)?, vo.offset.checked_add(vo.len.checked_mul(es)?)?);
        // The selected rows gathered under the source's borrow (only the
        // output's bytes are held), then stored in one copy.
        let row = inner * es;
        let mut gathered = Vec::new();
        gathered.try_reserve_exact(o_hi - vo.offset).ok()?;
        match self.bytes(va.buffer) {
            Some(data) if a_hi <= data.len() => {
                let src = &data[va.offset..a_hi];
                for x in 0..outer {
                    for &j in &at {
                        let from = (x * size + j) * row;
                        gathered.extend_from_slice(&src[from..from + row]);
                    }
                }
            }
            _ => return None,
        }
        let data = self.bytes_mut(vo.buffer)?;
        if o_hi > data.len() || gathered.len() != o_hi - vo.offset {
            return None;
        }
        data[vo.offset..o_hi].copy_from_slice(&gathered);
        self.pt_charge(cost);
        Some(())
    }

    /// `(I, O, nnz, sizes)`: tensor.js's `spKeys` (see `sparse::keys`),
    /// the linear keys of nnz nonzeros' indices I into the zeroed O.
    fn pt_sp_keys(&mut self, a: &[Arg]) -> Option<()> {
        let (vi, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?);
        if vi.kind != KIND_F64 || vo.kind != KIND_F64 || vi.buffer == vo.buffer {
            return None;
        }
        let nnz = int_arg(a, 2)?;
        let sizes = self.pt_ints(arg(a, 3))?;
        let n_idx = sizes.len().checked_mul(nnz)?;
        if vi.len != n_idx || vo.len != nnz {
            return None;
        }
        let cost = self.pt_admit(n_idx.checked_add(nnz)?, n_idx.saturating_add(nnz))?;
        let indices = self.pt_read_all(vi)?;
        let mut out = zeroed(nnz)?;
        sparse::keys(&indices, &sizes, nnz, &mut out)?;
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(O, K, V, n, block)`: tensor.js's `spScatterAdd` for float32 and
    /// float64 storages: each nonzero's block of V added into O at key *
    /// block, in order, each sum rounded to O's element type. Only the
    /// blocks the keys name are read and written.
    fn pt_sp_scatter(&mut self, a: &[Arg]) -> Option<()> {
        let (vo, vk, vv) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?);
        if vk.kind != KIND_F64 || !matches!(vo.kind, KIND_F32 | KIND_F64) || vv.kind != vo.kind || vo.buffer == vk.buffer || vo.buffer == vv.buffer {
            return None;
        }
        let (n, block) = (int_arg(a, 3)?, int_arg(a, 4)?);
        let nb = n.checked_mul(block)?;
        if vk.len != n || vv.len != nb || block == 0 {
            return None;
        }
        let cost = self.pt_admit(nb, n.saturating_add(nb))?;
        let keys = self.pt_read_all(vk)?;
        let vals = self.pt_read_all(vv)?;
        // Every block first: an index out of range leaves O for the
        // JavaScript loop to report.
        let mut bases = Vec::new();
        bases.try_reserve_exact(n).ok()?;
        for &k in &keys {
            let base = sparse_index(k, block, vo.len)?;
            bases.push(base);
        }
        let size = vo.size();
        let data = self.bytes_mut(vo.buffer)?;
        let hi = vo.offset.checked_add(vo.len.checked_mul(size)?)?;
        if hi > data.len() {
            return None;
        }
        let bytes = &mut data[vo.offset..hi];
        for (e, &base) in bases.iter().enumerate() {
            let src = &vals[e * block..(e + 1) * block];
            if vo.kind == KIND_F32 {
                for (b, &x) in src.iter().enumerate() {
                    let at = (base + b) * 4;
                    let mut w = [0u8; 4];
                    w.copy_from_slice(&bytes[at..at + 4]);
                    let sum = (f32::from_le_bytes(w) as f64 + x) as f32;
                    bytes[at..at + 4].copy_from_slice(&sum.to_le_bytes());
                }
            } else {
                for (b, &x) in src.iter().enumerate() {
                    let at = (base + b) * 8;
                    let mut w = [0u8; 8];
                    w.copy_from_slice(&bytes[at..at + 8]);
                    let sum = f64::from_le_bytes(w) + x;
                    bytes[at..at + 8].copy_from_slice(&sum.to_le_bytes());
                }
            }
        }
        self.pt_charge(cost);
        Some(())
    }

    /// `(TK, TV, SK, SV, TAKE, O, G, tn, sn, block, alpha)`: tensor.js's
    /// `spMerge` (see `sparse::merge`) for float32/float64 values: the
    /// result entries' positions into TAKE, values into the zeroed O (both
    /// sized for tn + sn entries), their count into G[0].
    fn pt_sp_merge(&mut self, a: &[Arg]) -> Option<()> {
        let (tk, tv, sk, sv) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 1))?, self.pt_view(arg(a, 2))?, self.pt_view(arg(a, 3))?);
        let (vt, vo, vg) = (self.pt_view(arg(a, 4))?, self.pt_view(arg(a, 5))?, self.pt_view(arg(a, 6))?);
        if [tk.kind, sk.kind, vt.kind, vg.kind].iter().any(|&k| k != KIND_F64) || !matches!(vo.kind, KIND_F32 | KIND_F64) || tv.kind != vo.kind || sv.kind != vo.kind {
            return None;
        }
        let ins = [tk.buffer, tv.buffer, sk.buffer, sv.buffer];
        if ins.contains(&vt.buffer) || ins.contains(&vo.buffer) || ins.contains(&vg.buffer) || vt.buffer == vo.buffer || vt.buffer == vg.buffer || vo.buffer == vg.buffer {
            return None;
        }
        let (tn, sn, block) = (int_arg(a, 7)?, int_arg(a, 8)?, int_arg(a, 9)?);
        let alpha = num_arg(a, 10)?;
        let total = tn.checked_add(sn)?;
        let tb = total.checked_mul(block)?;
        if tk.len != tn || sk.len != sn || tv.len != tn.checked_mul(block)? || sv.len != sn.checked_mul(block)? || vt.len != total || vo.len != tb || vg.len != 1 || block == 0 {
            return None;
        }
        let cost = self.pt_admit(tb.checked_add(total)?, total.saturating_mul(2).saturating_add(tb.saturating_mul(2)))?;
        let (tkv, tvv, skv, svv) = (self.pt_read_all(tk)?, self.pt_read_all(tv)?, self.pt_read_all(sk)?, self.pt_read_all(sv)?);
        let mut take = zeroed(total)?;
        let mut out = zeroed(tb)?;
        let kind = vo.kind;
        let r = sparse::merge(&tkv, &tvv, &skv, &svv, block, alpha, |x| stored(kind, x), &mut take, &mut out)?;
        self.pt_write(vt, 0, &take[..r])?;
        self.pt_write(vo, 0, &out[..r * block])?;
        self.pt_write(vg, 0, &[r as f64])?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(op, nIn, nOut, ...inputs, ...outputs, ...dims)`: tensor.js's
    /// `linalg`, a batch of dense factorizations (`linalg`). Inputs are
    /// float32/float64 matrices, outputs float64 storages the runtime
    /// sized; the results agree with torch_linalg.py's Python algorithms
    /// (the fallback when this declines) to its documented tolerances.
    fn pt_linalg(&mut self, a: &[Arg]) -> Option<()> {
        let op = int_arg(a, 0)?;
        let (n_in, n_out) = (int_arg(a, 1)?, int_arg(a, 2)?);
        if n_in == 0 || n_in > 2 || n_out > 5 {
            return None;
        }
        let mut ins: Vec<View> = Vec::new();
        for i in 0..n_in {
            let v = self.pt_view(arg(a, 3 + i))?;
            if !matches!(v.kind, KIND_F32 | KIND_F64) {
                return None;
            }
            ins.push(v);
        }
        let mut outs: Vec<View> = Vec::new();
        for i in 0..n_out {
            let v = self.pt_view(arg(a, 3 + n_in + i))?;
            if v.kind != KIND_F64 || ins.iter().any(|x| x.buffer == v.buffer) || outs.iter().any(|x| x.buffer == v.buffer) {
                return None;
            }
            outs.push(v);
        }
        let dims_at = 3 + n_in + n_out;
        let mut dims = [0usize; 5];
        for (i, d) in dims.iter_mut().enumerate() {
            if arg(a, dims_at + i).is_number() {
                *d = int_arg(a, dims_at + i)?;
            }
        }
        let batch = dims[0];
        use linalg::Op;
        // Each matrix's work-bound op and dims, and the input and output sizes.
        let (bop, bm, bn, in_sizes, out_sizes): (Op, usize, usize, [usize; 2], [usize; 5]) = match op {
            1 => {
                let (m, n) = (dims[1], dims[2]);
                let mn = m.checked_mul(n)?;
                (Op::Lu, m, n, [mn, 0], [mn, m, m.min(n), 1, 1])
            }
            2 | 3 => {
                let (n, k) = (dims[1], dims[2]);
                let nk = n.checked_mul(k)?;
                (Op::Solve, n, k, [n.checked_mul(n)?, nk], [nk, if op == 2 { 1 } else { 0 }, 0, 0, 0])
            }
            4 => {
                let n = dims[1];
                (Op::Cholesky, n, n, [n.checked_mul(n)?, 0], [n.checked_mul(n)?, 1, 0, 0, 0])
            }
            5 => {
                let (m, n, qcols, rrows) = (dims[1], dims[2], dims[3], dims[4]);
                (Op::Qr, m, n, [m.checked_mul(n)?, 0], [m.checked_mul(qcols)?, rrows.checked_mul(n)?, 0, 0, 0])
            }
            6 => {
                let n = dims[1];
                (Op::Eigh, n, n, [n.checked_mul(n)?, 0], [n, n.checked_mul(n)?, 0, 0, 0])
            }
            7 => {
                let (m, n, full) = (dims[1], dims[2], dims[3] != 0);
                let k = m.min(n);
                let (uc, vr) = if full { (m, n) } else { (k, k) };
                (Op::Svd, m, n, [m.checked_mul(n)?, 0], [m.checked_mul(uc)?, k, vr.checked_mul(n)?, 0, 0])
            }
            8 => {
                let n = dims[1];
                let nn = n.checked_mul(n)?;
                (Op::Eig, n, n, [nn, 0], [n, n, nn, nn, 1])
            }
            _ => return None,
        };
        if ins.iter().enumerate().any(|(i, v)| batch.checked_mul(in_sizes[i]) != Some(v.len))
            || outs.iter().enumerate().any(|(i, v)| batch.checked_mul(out_sizes[i]) != Some(v.len))
        {
            return None;
        }
        // The price: each matrix's work bound. An SVD's is its sweeps plus a
        // basis completion only when a non-square full basis is asked for;
        // a rank-deficient input that needs more stops and declines below.
        let per = if op == 7 && !(dims[3] != 0 && bm != bn) {
            let k = bm.min(bn) as u64;
            let big = bm.max(bn) as u64;
            (k * k / 2 + 1)
                .saturating_mul(80)
                .saturating_mul(big.saturating_mul(4).saturating_add(k * 2))
                .saturating_add(big.saturating_pow(3).saturating_mul(4))
                .saturating_add(64)
        } else if op == 2 {
            // The factorization, then the solve.
            linalg::bound(Op::Lu, bm, bm).saturating_add(linalg::bound(bop, bm, bn))
        } else {
            linalg::bound(bop, bm, bn)
        };
        let units = per.saturating_mul(batch as u64);
        let elems = ins.iter().chain(outs.iter()).fold(0usize, |x, v| x.saturating_add(v.len)).saturating_mul(3);
        let cost = self.pt_admit(usize::try_from(units).ok()?, elems)?;
        let inputs: Vec<Vec<f64>> = ins.iter().map(|&v| self.pt_read_all(v)).collect::<Option<_>>()?;
        let mut results: Vec<Vec<f64>> = out_sizes[..n_out].iter().map(|&n| zeroed(batch.checked_mul(n)?)).collect::<Option<_>>()?;
        let spent = {
            let mut interrupted = || self.interrupted();
            let mut b = linalg::Budget::new(&mut interrupted);
            for t in 0..batch {
                let x = &inputs[0][t * in_sizes[0]..(t + 1) * in_sizes[0]];
                match op {
                    1 => {
                        let (m, n) = (dims[1], dims[2]);
                        let k = m.min(n);
                        let f = linalg::lu(x, m, n, &mut b)?;
                        results[0][t * m * n..(t + 1) * m * n].copy_from_slice(&f.lu);
                        for (i, &p) in f.perm.iter().enumerate() {
                            results[1][t * m + i] = p as f64;
                        }
                        for (i, &p) in f.pivots.iter().enumerate() {
                            results[2][t * k + i] = p as f64;
                        }
                        results[3][t] = f.sign;
                        results[4][t] = f.info as f64;
                    }
                    2 => {
                        let (n, k) = (dims[1], dims[2]);
                        let rhs = &inputs[1][t * n * k..(t + 1) * n * k];
                        let f = linalg::lu(x, n, n, &mut b)?;
                        let sol = linalg::lu_solve(&f.lu, &f.perm, n, rhs, k, &mut b)?;
                        results[0][t * n * k..(t + 1) * n * k].copy_from_slice(&sol);
                        results[1][t] = f.info as f64;
                    }
                    3 => {
                        let (n, k) = (dims[1], dims[2]);
                        let rhs = &inputs[1][t * n * k..(t + 1) * n * k];
                        let sol = linalg::tri_solve(x, n, rhs, k, dims[3] != 0, dims[4] != 0, &mut b)?;
                        results[0][t * n * k..(t + 1) * n * k].copy_from_slice(&sol);
                    }
                    4 => {
                        let n = dims[1];
                        let (l, info) = linalg::cholesky(x, n, &mut b)?;
                        results[0][t * n * n..(t + 1) * n * n].copy_from_slice(&l);
                        results[1][t] = info as f64;
                    }
                    5 => {
                        let (m, n, qcols, rrows) = (dims[1], dims[2], dims[3], dims[4]);
                        let (w, taus) = linalg::householder(x, m, n, &mut b)?;
                        for i in 0..rrows.min(m) {
                            for j in i..n {
                                results[1][t * rrows * n + i * n + j] = w[i * n + j];
                            }
                        }
                        if qcols > 0 {
                            let q = linalg::form_q(&w, &taus, m, n, qcols, &mut b)?;
                            results[0][t * m * qcols..(t + 1) * m * qcols].copy_from_slice(&q);
                        }
                    }
                    6 => {
                        let n = dims[1];
                        let lower = dims[2] != 0;
                        // The symmetric matrix of the named triangle.
                        let mut sym = zeroed(n * n)?;
                        for i in 0..n {
                            for j in 0..n {
                                let from_lower = j <= i;
                                let (r, c) = if from_lower == lower { (i, j) } else { (j, i) };
                                sym[i * n + j] = x[r * n + c];
                            }
                        }
                        let (w, v) = linalg::eigh(&sym, n, &mut b)?;
                        results[0][t * n..(t + 1) * n].copy_from_slice(&w);
                        results[1][t * n * n..(t + 1) * n * n].copy_from_slice(&v);
                    }
                    7 => {
                        let (m, n, full) = (dims[1], dims[2], dims[3] != 0);
                        let (u, sv, vh) = linalg::svd(x, m, n, full, &mut b)?;
                        let (su, ss, sh) = (out_sizes[0], out_sizes[1], out_sizes[2]);
                        if u.len() != su || sv.len() != ss || vh.len() != sh {
                            return None;
                        }
                        results[0][t * su..(t + 1) * su].copy_from_slice(&u);
                        results[1][t * ss..(t + 1) * ss].copy_from_slice(&sv);
                        results[2][t * sh..(t + 1) * sh].copy_from_slice(&vh);
                    }
                    _ => {
                        let n = dims[1];
                        match linalg::eig(x, n, &mut b)? {
                            Ok(e) => {
                                results[0][t * n..(t + 1) * n].copy_from_slice(&e.wr);
                                results[1][t * n..(t + 1) * n].copy_from_slice(&e.wi);
                                results[2][t * n * n..(t + 1) * n * n].copy_from_slice(&e.vr);
                                results[3][t * n * n..(t + 1) * n * n].copy_from_slice(&e.vi);
                                results[4][t] = 1.0;
                            }
                            Err(()) => results[4][t] = 0.0,
                        }
                    }
                }
                if b.spent() > units {
                    return None;
                }
            }
            b.spent()
        };
        for (v, r) in outs.iter().zip(&results) {
            self.pt_write(*v, 0, r)?;
        }
        // Charged for the work done, within the admitted price.
        let done = STEPS_BASE.saturating_add(spent.saturating_mul(STEPS_PER_UNIT));
        self.pt_charge(done.min(cost));
        Some(())
    }

    /// `(C, A, B, O, shape, stridesC, stridesA, stridesB)`: tensor.js's
    /// `where`, `O[i] = C[.] ? A[.] : B[.]`.
    // ---- quantization (see `quant`) ----------------------------------------------------

    /// `pt_view`, admitting an Int8Array too (a qint8 storage).
    fn pt_view_q(&self, v: &Arg) -> Option<View> {
        match v {
            Arg::View(view) if view.kind == KIND_I8 => Some(*view),
            _ => self.pt_view(v),
        }
    }

    /// A per-tensor parameter (a number) or per-channel values (a view).
    fn pt_params(&self, v: &Arg) -> Option<quant::Params> {
        if let Arg::Num(x) = v {
            return Some(quant::Params::One(*x));
        }
        let view = self.pt_view_q(v)?;
        Some(quant::Params::Many(self.pt_read_all(view)?))
    }

    /// `(X, S, Z, O, outer, C, inner, qmin, qmax, nan, mode)`: tensor.js's
    /// `qQuantize` (see `quant::quantize`) of a float32 X into O.
    fn pt_q_quantize(&mut self, a: &[Arg]) -> Option<()> {
        let (vx, vo) = (self.pt_view(arg(a, 0))?, self.pt_view_q(arg(a, 3))?);
        if vx.kind != KIND_F32 || !matches!(vo.kind, KIND_U8 | KIND_I8 | KIND_F64) || vo.buffer == vx.buffer {
            return None;
        }
        let (scales, zps) = (self.pt_params(arg(a, 1))?, self.pt_params(arg(a, 2))?);
        let (outer, c, inner) = (int_arg(a, 4)?, int_arg(a, 5)?, int_arg(a, 6)?);
        let (qmin, qmax, nan) = (num_arg(a, 7)?, num_arg(a, 8)?, num_arg(a, 9)?);
        let mode = u32::try_from(int_arg(a, 10)?).ok()?;
        if vx.len != vo.len || mode > 2 {
            return None;
        }
        let cost = self.pt_admit(vx.len, vx.len.saturating_mul(2))?;
        let x = self.pt_read_all(vx)?;
        let mut out = zeroed(vx.len)?;
        quant::quantize(&x, &scales, &zps, &mut out, outer, c, inner, qmin, qmax, nan, mode)?;
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(Q, S, Z, O, outer, C, inner, double)`: tensor.js's `qDequantize`
    /// into the float32 O.
    fn pt_q_dequantize(&mut self, a: &[Arg]) -> Option<()> {
        let (vq, vo) = (self.pt_view_q(arg(a, 0))?, self.pt_view(arg(a, 3))?);
        if !matches!(vq.kind, KIND_U8 | KIND_I8 | KIND_F64) || vo.kind != KIND_F32 || vo.buffer == vq.buffer || vq.len != vo.len {
            return None;
        }
        let (scales, zps) = (self.pt_params(arg(a, 1))?, self.pt_params(arg(a, 2))?);
        let (outer, c, inner) = (int_arg(a, 4)?, int_arg(a, 5)?, int_arg(a, 6)?);
        let dbl = arg(a, 7).is_bool() && arg(a, 7).as_bool();
        let cost = self.pt_admit(vq.len, vq.len.saturating_mul(2))?;
        let q = self.pt_read_all(vq)?;
        let mut out = zeroed(vq.len)?;
        quant::dequantize(&q, &scales, &zps, &mut out, outer, c, inner, dbl)?;
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(X, S, Z, O, M, outer, C, inner, qmin, qmax)`: tensor.js's
    /// `qFakeQuant` of a float32/float64 X into O (X's type) and the bool
    /// mask M.
    fn pt_q_fake(&mut self, a: &[Arg]) -> Option<()> {
        let (vx, vo, vm) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 3))?, self.pt_view(arg(a, 4))?);
        if !matches!(vx.kind, KIND_F32 | KIND_F64) || vo.kind != vx.kind || vm.kind != KIND_U8 || vx.len != vo.len || vx.len != vm.len {
            return None;
        }
        if vo.buffer == vx.buffer || vm.buffer == vx.buffer || vm.buffer == vo.buffer {
            return None;
        }
        let (scales, zps) = (self.pt_params(arg(a, 1))?, self.pt_params(arg(a, 2))?);
        let (outer, c, inner) = (int_arg(a, 5)?, int_arg(a, 6)?, int_arg(a, 7)?);
        let (qmin, qmax) = (num_arg(a, 8)?, num_arg(a, 9)?);
        let cost = self.pt_admit(vx.len, vx.len.saturating_mul(3))?;
        let x = self.pt_read_all(vx)?;
        let mut out = zeroed(vx.len)?;
        let mut mask = zeroed(vx.len)?;
        quant::fake_quant(&x, vx.kind == KIND_F32, &scales, &zps, &mut out, &mut mask, outer, c, inner, qmin, qmax)?;
        self.pt_write(vo, 0, &out)?;
        self.pt_write(vm, 0, &mask)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(X, W, Z, O, x_zp, M, K, N)`: tensor.js's `qMatmul`, the integer
    /// accumulators of a quantized [M, K] x [N, K]^T product into the
    /// float64 O.
    fn pt_q_matmul(&mut self, a: &[Arg]) -> Option<()> {
        let (vx, vw, vo) = (self.pt_view_q(arg(a, 0))?, self.pt_view_q(arg(a, 1))?, self.pt_view(arg(a, 3))?);
        if !matches!(vx.kind, KIND_U8 | KIND_I8) || !matches!(vw.kind, KIND_U8 | KIND_I8) || vo.kind != KIND_F64 {
            return None;
        }
        if vo.buffer == vx.buffer || vo.buffer == vw.buffer {
            return None;
        }
        let zps = self.pt_params(arg(a, 2))?;
        let xzp = num_arg(a, 4)?;
        let (m, k, n) = (int_arg(a, 5)?, int_arg(a, 6)?, int_arg(a, 7)?);
        let (mk, kn, mn) = (m.checked_mul(k)?, k.checked_mul(n)?, m.checked_mul(n)?);
        if vx.len != mk || vw.len != kn || vo.len != mn {
            return None;
        }
        let cost = self.pt_admit(mn.checked_mul(k)?.checked_add(mn)?, mk.saturating_add(kn).saturating_add(mn))?;
        let x = self.pt_read_all(vx)?;
        let w = self.pt_read_all(vw)?;
        let mut out = zeroed(mn)?;
        quant::matmul(&x, &w, &zps, &mut out, xzp, m, k, n, POLL_UNITS, || self.interrupted())?;
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(X, W, Z, O, x_zp, B, C, H, W, O, Cg, Kh, Kw, Sh, Sw, Ph, Pw, Dh, Dw,
    /// groups, Ho, Wo)`: tensor.js's `qConv2d` into the float64 O.
    fn pt_q_conv2d(&mut self, a: &[Arg]) -> Option<()> {
        let (vx, vw, vo) = (self.pt_view_q(arg(a, 0))?, self.pt_view_q(arg(a, 1))?, self.pt_view(arg(a, 3))?);
        if !matches!(vx.kind, KIND_U8 | KIND_I8) || !matches!(vw.kind, KIND_U8 | KIND_I8) || vo.kind != KIND_F64 {
            return None;
        }
        if vo.buffer == vx.buffer || vo.buffer == vw.buffer {
            return None;
        }
        let zps = self.pt_params(arg(a, 2))?;
        let xzp = num_arg(a, 4)?;
        let mut d = [0usize; 17];
        for (i, slot) in d.iter_mut().enumerate() {
            *slot = int_arg(a, 5 + i)?;
        }
        let [b, c, h, w, o, cg, kh, kw, sh, sw, ph, pw, dh, dw, groups, ho, wo] = d;
        let dims = quant::ConvDims { b, c, h, w, o, cg, kh, kw, sh, sw, ph, pw, dh, dw, groups, ho, wo };
        if vx.len != b.checked_mul(c)?.checked_mul(h)?.checked_mul(w)? || vw.len != o.checked_mul(cg)?.checked_mul(kh)?.checked_mul(kw)? {
            return None;
        }
        let n_out = b.checked_mul(o)?.checked_mul(ho)?.checked_mul(wo)?;
        if vo.len != n_out {
            return None;
        }
        let units = n_out.checked_mul(cg.checked_mul(kh)?.checked_mul(kw)?)?.checked_add(n_out)?;
        let cost = self.pt_admit(units, vx.len.saturating_add(vw.len).saturating_add(n_out))?;
        let x = self.pt_read_all(vx)?;
        let wt = self.pt_read_all(vw)?;
        let mut out = zeroed(n_out)?;
        quant::conv2d(&x, &wt, &zps, &mut out, xzp, &dims, POLL_UNITS, || self.interrupted())?;
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    /// `(A, bias|null, atw, mult, O, outer, N, inner, out_zp, lo, hi,
    /// mode)`: tensor.js's `qRequant` of float64 accumulators into the
    /// uint8 (modes 0, 1) or float32 (mode 2) O.
    fn pt_q_requant(&mut self, a: &[Arg]) -> Option<()> {
        let (vacc, vo) = (self.pt_view(arg(a, 0))?, self.pt_view(arg(a, 4))?);
        let mode = u32::try_from(int_arg(a, 11)?).ok()?;
        let want = if mode == 2 { KIND_F32 } else { KIND_U8 };
        if vacc.kind != KIND_F64 || vo.kind != want || mode > 2 || vacc.len != vo.len || vo.buffer == vacc.buffer {
            return None;
        }
        let bias = if arg(a, 1).is_null() {
            None
        } else {
            let vb = self.pt_view(arg(a, 1))?;
            if vb.buffer == vo.buffer {
                return None;
            }
            Some(self.pt_read_all(vb)?)
        };
        let (atw, mult) = (self.pt_params(arg(a, 2))?, self.pt_params(arg(a, 3))?);
        let (outer, n, inner) = (int_arg(a, 5)?, int_arg(a, 6)?, int_arg(a, 7)?);
        let (ozp, lo, hi) = (num_arg(a, 8)?, num_arg(a, 9)?, num_arg(a, 10)?);
        let cost = self.pt_admit(vacc.len, vacc.len.saturating_mul(2))?;
        let acc = self.pt_read_all(vacc)?;
        let mut out = zeroed(vacc.len)?;
        quant::requant(&acc, bias.as_deref(), &atw, &mult, &mut out, outer, n, inner, ozp, lo, hi, mode)?;
        self.pt_write(vo, 0, &out)?;
        self.pt_charge(cost);
        Some(())
    }

    fn pt_where(&mut self, a: &[Arg]) -> Option<()> {
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
            let done = walk(&shape, [&sc, &sa, &sb], || self.interrupted(), |i, [c, x, y]| {
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

impl<T: Host + ?Sized> Kernels for T {}

/// Argument `i`, or `Other` past the end.
fn arg(args: &[Arg], i: usize) -> &Arg {
    static OTHER: Arg = Arg::Other;
    args.get(i).unwrap_or(&OTHER)
}

/// A number argument.
fn num_arg(args: &[Arg], i: usize) -> Option<f64> {
    arg(args, i).num()
}

fn int_arg(args: &[Arg], i: usize) -> Option<usize> {
    as_index(arg(args, i))
}

/// The first element a sparse key names in a storage of `len` elements
/// holding blocks of `block`: key * block, when that block fits.
fn sparse_index(key: f64, block: usize, len: usize) -> Option<usize> {
    if !(0.0..=9007199254740991.0).contains(&key) || key.fract() != 0.0 {
        return None;
    }
    let base = usize::try_from(key as u64).ok()?.checked_mul(block)?;
    (base.checked_add(block)? <= len).then_some(base)
}

