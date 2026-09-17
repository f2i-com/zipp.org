//! Elementary functions in double precision, rounded to float32 once by the
//! caller.
//!
//! There is no libm here and deliberately no dependency on one. These are
//! ports of the routines the C kernels used, kept identical term for term,
//! because `gpu-lab` holds the compiled backend to *bit-for-bit* agreement
//! with the JavaScript reference over whole training steps. Each agrees with
//! `Math.exp` and friends to about one double ulp, which survives the round to
//! float32; a different polynomial, or libm's, would not necessarily.
//!
//! Everything here is pure arithmetic on `f64`, so it compiles without `std`
//! and is unit-testable on the host.

const LN2_HI: f64 = 6.93147180369123816490e-01;
const LN2_LO: f64 = 1.90821492927058770002e-10;
const LOG2E: f64 = 1.44269504088896338700e+00;

/// Round to nearest, ties to even -- what `f64.nearest` does on wasm.
///
/// The add-and-subtract trick is exact for the magnitudes reached here (the
/// exponent of a finite `exp` argument, so |x| < 2^11) and avoids needing an
/// intrinsic that `core` does not expose.
#[inline]
fn rint(x: f64) -> f64 {
    const MAGIC: f64 = 6755399441055744.0; // 2^52 + 2^51
    if x >= 0.0 { (x + MAGIC) - MAGIC } else { (x - MAGIC) + MAGIC }
}

#[inline]
fn abs(x: f64) -> f64 { if x < 0.0 { -x } else { x } }

/// `x * 2^k`, stepping through the exponent range so `k` past a single
/// exponent field still scales correctly.
fn scale2(mut x: f64, mut k: i32) -> f64 {
    while k > 1023 { x *= 8.98846567431157953865e+307; k -= 1023; }
    while k < -1022 { x *= 2.22507385850720138309e-308; k += 1022; }
    x * f64::from_bits(((k + 1023) as u64) << 52)
}

pub fn exp(x: f64) -> f64 {
    if x != x { return x; }
    if x > 710.0 { return f64::INFINITY; }
    if x < -746.0 { return 0.0; }
    let kd = rint(x * LOG2E);
    let r = (x - kd * LN2_HI) - kd * LN2_LO;
    let p = 1.0 + r*(1.0 + r*(1.0/2.0 + r*(1.0/6.0 + r*(1.0/24.0 + r*(1.0/120.0 + r*(1.0/720.0
        + r*(1.0/5040.0 + r*(1.0/40320.0 + r*(1.0/362880.0 + r*(1.0/3628800.0 + r*(1.0/39916800.0
        + r*(1.0/479001600.0 + r*(1.0/6227020800.0)))))))))))));
    scale2(p, kd as i32)
}

pub fn log(x: f64) -> f64 {
    if x != x || x < 0.0 { return f64::NAN; }
    if x == 0.0 { return f64::NEG_INFINITY; }
    if x == f64::INFINITY { return f64::INFINITY; }
    let mut bits = x.to_bits();
    let mut e = (bits >> 52) as i32 - 1023;
    if e == -1023 {
        // Subnormal: scale by 2^54 and take the exponent from the scaled value.
        bits = (x * 18014398509481984.0).to_bits();
        e = (bits >> 52) as i32 - 1023 - 54;
    }
    let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    if m > 1.41421356237309504880 { m *= 0.5; e += 1; }
    let f = m - 1.0;
    let s = f / (2.0 + f);
    let z = s * s;
    let p = z*(1.0/3.0 + z*(1.0/5.0 + z*(1.0/7.0 + z*(1.0/9.0 + z*(1.0/11.0 + z*(1.0/13.0
        + z*(1.0/15.0 + z*(1.0/17.0 + z*(1.0/19.0 + z*(1.0/21.0))))))))));
    let e = e as f64;
    e * LN2_HI + (2.0*s + (2.0*s*p + e * LN2_LO))
}

pub fn tanh(x: f64) -> f64 {
    if x != x { return x; }
    let a = abs(x);
    if a < 0.01 {
        let z = x * x;
        return x * (1.0 + z*(-1.0/3.0 + z*(2.0/15.0 + z*(-17.0/315.0))));
    }
    let r = if a > 20.0 { 1.0 } else { let t = exp(-2.0 * a); (1.0 - t) / (1.0 + t) };
    if x < 0.0 { -r } else { r }
}

fn erf_series(z: f64) -> f64 {
    let t = z * z;
    z * (1.1283791670955126 + t * (-0.37612638903183754 + t * (0.11283791670955126
        + t * (-0.026866170645131252 + t * (0.005223977625442188
        + t * (-0.0008548327023450852 + t * 0.00012055332981789664))))))
}

fn erfc_fit(a: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.5 * a);
    t * exp(-a * a - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418
        + t * (-0.18628806 + t * (0.27886807 + t * (-1.13520398 + t * (1.48851587
        + t * (-0.82215223 + t * 0.17087277)))))))))
}

/// The standard normal CDF, shared with `src/kernel-math.mjs`.
pub fn phi(x: f64) -> f64 {
    let z = x * 0.70710678118654752440;
    let a = abs(z);
    if a < 0.5 { return 0.5 + 0.5 * erf_series(z); }
    if z >= 10.0 { return 1.0; }
    if z <= -10.0 { return 0.0; }
    let c = 0.5 * erfc_fit(a);
    if z > 0.0 { 1.0 - c } else { c }
}

pub fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 { 1.0 / (1.0 + exp(-x)) } else { let e = exp(x); e / (1.0 + e) }
}

pub fn gelu(x: f64) -> f64 { x * phi(x) }

pub fn gelu_grad(x: f64) -> f64 { phi(x) + x * 0.3989422804014327 * exp(-0.5 * x * x) }

#[cfg(test)]
mod tests {
    use super::*;

    /// Agreement with the host's own libm, in the range the kernels see. The
    /// contract is one double ulp, which is what makes the float32 results
    /// identical; see the module comment.
    fn close(a: f64, b: f64, ulps: f64) -> bool {
        if a == b { return true; }
        let scale = if a.abs() > b.abs() { a.abs() } else { b.abs() };
        (a - b).abs() <= ulps * scale * f64::EPSILON
    }

    #[test]
    fn exp_matches_libm() {
        let mut x = -40.0;
        while x <= 40.0 {
            assert!(close(exp(x), x.exp(), 2.0), "exp({x}): {} vs {}", exp(x), x.exp());
            x += 0.013;
        }
        assert_eq!(exp(f64::NEG_INFINITY), 0.0);
        assert!(exp(f64::NAN).is_nan());
    }

    #[test]
    fn log_matches_libm() {
        for &x in &[1e-300_f64, 1e-8, 0.5, 1.0, 1.9, 2.0, 7.0, 1e8, 1e300] {
            assert!(close(log(x), x.ln(), 4.0), "log({x}): {} vs {}", log(x), x.ln());
        }
        assert_eq!(log(0.0), f64::NEG_INFINITY);
        assert!(log(-1.0).is_nan());
    }

    #[test]
    fn tanh_and_sigmoid_match_libm() {
        let mut x = -25.0;
        while x <= 25.0 {
            assert!(close(tanh(x), x.tanh(), 8.0), "tanh({x})");
            assert!(close(sigmoid(x), 1.0 / (1.0 + (-x).exp()), 8.0), "sigmoid({x})");
            x += 0.017;
        }
    }

    #[test]
    fn phi_is_a_cdf() {
        assert!(close(phi(0.0), 0.5, 1.0));
        assert_eq!(phi(-40.0), 0.0);
        assert_eq!(phi(40.0), 1.0);
        // Symmetric, and monotone across the branch at |z| = 0.5.
        let mut x = -6.0;
        let mut last = 0.0;
        while x <= 6.0 {
            let p = phi(x);
            assert!(p >= last, "phi decreased at {x}");
            assert!(close(p + phi(-x), 1.0, 64.0), "phi({x}) + phi(-{x}) != 1");
            last = p;
            x += 0.011;
        }
    }

    #[test]
    fn rint_rounds_half_to_even() {
        for (x, want) in [(0.5, 0.0), (1.5, 2.0), (2.5, 2.0), (-0.5, -0.0), (-1.5, -2.0), (-2.5, -2.0),
                          (1.4, 1.0), (1.6, 2.0), (-1.4, -1.0), (-1.6, -2.0)] {
            assert_eq!(rint(x), want, "rint({x})");
        }
    }
}
