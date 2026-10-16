//! The interpreter's number operations the tensor kernels use: `Math.*`
//! (`vm::helpers_num2::math_unary`), float16 conversion and the typed-array
//! integer wrap (`vm::helpers_numeric::to_uint_modular`), as copies. The
//! kernels compile into the engine and, for the `torch` package, into a
//! separate WebAssembly module (zipp_torch.wasm) that cannot reach the
//! engine's code; both must compute exactly what the interpreted loop does.
//! `py_tensor::tests::jsmath_matches_the_interpreter` checks every function
//! here bit for bit against the original it copies.
#![allow(dead_code)]

/// The `Math` functions a kernel applies.
#[derive(Clone, Copy, Debug)]
pub enum M {
    Abs,
    Acos,
    Acosh,
    Asin,
    Asinh,
    Atan,
    Atanh,
    Ceil,
    Cos,
    Cosh,
    Exp,
    Expm1,
    Floor,
    Log,
    Log10,
    Log1p,
    Log2,
    Sin,
    Sinh,
    Sqrt,
    Tan,
    Tanh,
    Trunc,
}

impl M {
    pub const ALL: [M; 23] = [
        M::Abs,
        M::Acos,
        M::Acosh,
        M::Asin,
        M::Asinh,
        M::Atan,
        M::Atanh,
        M::Ceil,
        M::Cos,
        M::Cosh,
        M::Exp,
        M::Expm1,
        M::Floor,
        M::Log,
        M::Log10,
        M::Log1p,
        M::Log2,
        M::Sin,
        M::Sinh,
        M::Sqrt,
        M::Tan,
        M::Tanh,
        M::Trunc,
    ];
}

/// `Math[op](x)`.
pub fn math_unary(op: M, x: f64) -> f64 {
    match op {
        M::Abs => x.abs(),
        M::Floor => x.floor(),
        M::Ceil => x.ceil(),
        M::Trunc => x.trunc(),
        M::Sqrt => x.sqrt(),
        M::Exp => x.exp(),
        M::Log => x.ln(),
        M::Log2 => x.log2(),
        M::Log10 => x.log10(),
        M::Sin => x.sin(),
        M::Cos => x.cos(),
        M::Tan => x.tan(),
        M::Asin => x.asin(),
        M::Acos => x.acos(),
        M::Atan => x.atan(),
        M::Expm1 => x.exp_m1(),
        M::Log1p => x.ln_1p(),
        M::Sinh => x.sinh(),
        M::Cosh => x.cosh(),
        M::Tanh => x.tanh(),
        M::Asinh => x.asinh(),
        M::Acosh => acosh(x),
        M::Atanh => atanh(x),
    }
}

fn acosh(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x < 1.0 {
        return f64::NAN;
    }
    if x >= 268_435_456.0 {
        return x.ln() + std::f64::consts::LN_2;
    }
    if x > 2.0 {
        return (2.0 * x - 1.0 / (x + (x * x - 1.0).sqrt())).ln();
    }
    let t = x - 1.0;
    (t + (2.0 * t + t * t).sqrt()).ln_1p()
}

fn atanh(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let a = x.abs();
    if a > 1.0 {
        return f64::NAN;
    }
    if a == 1.0 {
        return f64::INFINITY.copysign(x);
    }
    let t = if a < 0.5 {
        0.5 * (2.0 * a + 2.0 * a * a / (1.0 - a)).ln_1p()
    } else {
        0.5 * ((a + a) / (1.0 - a)).ln_1p()
    };
    t.copysign(x)
}

/// An IEEE-754 binary16 bit pattern as an f64 (exact).
pub fn f16_bits_to_f64(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (bits >> 10) & 0x1f;
    let mant = (bits & 0x3ff) as f64;
    match exp {
        0 => sign * mant * 2f64.powi(-24),
        0x1f if mant == 0.0 => sign * f64::INFINITY,
        0x1f => f64::NAN,
        _ => sign * (1.0 + mant / 1024.0) * 2f64.powi((exp as i32) - 15),
    }
}

/// The nearest binary16 bit pattern to `f`, ties to even.
pub fn f64_to_f16_bits(f: f64) -> u16 {
    if f.is_nan() {
        return 0x7e00;
    }
    let sign: u16 = if f.is_sign_negative() { 0x8000 } else { 0 };
    let a = f.abs();
    if a.is_infinite() || a >= 65520.0 {
        return sign | 0x7c00;
    }
    if a == 0.0 {
        return sign;
    }
    let bits = a.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let m = bits & 0x000f_ffff_ffff_ffff;
    if e < -14 {
        let full = (1u64 << 52) | m;
        let shift = (28 - e) as u32;
        let rounded = round_shift_u64(full, shift);
        return sign | (rounded as u16);
    }
    let f16_exp = (e + 15) as u16;
    let rounded_mant = round_shift_u64(m, 42);
    if rounded_mant == 1024 {
        return sign | ((f16_exp + 1) << 10);
    }
    sign | (f16_exp << 10) | (rounded_mant as u16)
}

fn round_shift_u64(val: u64, shift: u32) -> u64 {
    if shift == 0 {
        return val;
    }
    if shift >= 64 {
        return 0;
    }
    let quotient = val >> shift;
    let remainder = val & ((1u64 << shift) - 1);
    let half = 1u64 << (shift - 1);
    if remainder > half || (remainder == half && (quotient & 1) == 1) {
        quotient + 1
    } else {
        quotient
    }
}

/// ToUint`bits` of `f` (the typed-array store of an integer element).
pub fn to_uint_modular(f: f64, bits: u32) -> u64 {
    if !f.is_finite() {
        return 0;
    }
    let m = 2f64.powi(bits as i32);
    f.trunc().rem_euclid(m) as u64
}
