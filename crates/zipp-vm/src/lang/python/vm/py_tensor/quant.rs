//! The native loops behind `_zipp_tensor`'s quantization kernels
//! (`q_quantize`, `q_dequantize`, `q_matmul`, `q_conv2d`, `q_requant`,
//! `q_fake_quant`): tensor.js's `qQuantize`, `qDequantize`, `qMatmul`,
//! `qConv2d`, `qRequant` and `qFakeQuant`, line for line. Each float32 step
//! of the JavaScript loop is a `Math.fround` of a double result; here it is
//! the same double operation followed by `as f32`, which rounds exactly as
//! `Math.fround` does. Integer rounding is half-to-even in both
//! (`round_ties_even`, the JavaScript `rintEven`). The integer
//! accumulators are doubles, exact below 2^53, as in the JavaScript loops.

/// A per-tensor parameter (one value for every channel) or per-channel
/// values.
pub(super) enum Params {
    One(f64),
    Many(Vec<f64>),
}

impl Params {
    #[inline]
    pub(super) fn get(&self, c: usize) -> f64 {
        match self {
            Params::One(x) => *x,
            Params::Many(v) => v[c],
        }
    }

    pub(super) fn fits(&self, n: usize) -> bool {
        match self {
            Params::One(_) => true,
            Params::Many(v) => v.len() == n,
        }
    }
}

#[inline]
fn fround(x: f64) -> f64 {
    x as f32 as f64
}

#[inline]
fn rint(x: f64) -> f64 {
    x.round_ties_even()
}

/// tensor.js's `fma32`: the float32 of the exact a * b + c for float32
/// values a, b, c. The product is exact in a double; the double sum's
/// rounding error (TwoSum) decides the one case its rounding to float32
/// can get wrong, a sum exactly halfway between two floats.
pub(super) fn fma32(a: f64, b: f64, c: f64) -> f64 {
    let p = a * b;
    let s = p + c;
    let bb = s - p;
    let err = (p - (s - bb)) + (c - bb);
    let r = fround(s);
    if err == 0.0 || r == s || !r.is_finite() {
        return r;
    }
    let rf = r as f32;
    let up = (s > r) == (r >= 0.0);
    let bits = rf.to_bits() as i32;
    let other = f32::from_bits((if up { bits.wrapping_add(1) } else { bits.wrapping_sub(1) }) as u32) as f64;
    if s - r != (other - r) / 2.0 {
        return r;
    }
    if (err > 0.0) == (other > r) {
        other
    } else {
        r
    }
}

/// `qQuantize`: x [outer, C, inner] to integers in [qmin, qmax]; `mode`
/// and `nan` as tensor.js describes them.
#[allow(clippy::too_many_arguments)]
pub(super) fn quantize(x: &[f64], scales: &Params, zps: &Params, out: &mut [f64], outer: usize, c_n: usize, inner: usize, qmin: f64, qmax: f64, nan: f64, mode: u32) -> Option<()> {
    if x.len() != outer.checked_mul(c_n)?.checked_mul(inner)? || out.len() != x.len() || !scales.fits(c_n) || !zps.fits(c_n) {
        return None;
    }
    let mut i = 0;
    for _ in 0..outer {
        for c in 0..c_n {
            let inv = fround(1.0 / fround(scales.get(c)));
            let z = zps.get(c);
            for _ in 0..inner {
                let v = x[i];
                let q = if v.is_nan() {
                    nan
                } else {
                    let mut q = rint(fround(v * inv));
                    if mode == 0 {
                        if q > 2147483520.0 {
                            q = 2147483520.0;
                        } else if q < -2147483648.0 {
                            q = -2147483648.0;
                        }
                        q += z;
                        if q > 2147483647.0 {
                            q -= 4294967296.0;
                        } else if q < -2147483648.0 {
                            q += 4294967296.0;
                        }
                    } else {
                        q += z;
                    }
                    q.clamp(qmin, qmax)
                };
                out[i] = q;
                i += 1;
            }
        }
    }
    Some(())
}

/// `qDequantize`: float(scale) * (q - zero_point), or with `dbl` the double
/// scale times it, each rounded once by the float32 store.
#[allow(clippy::too_many_arguments)]
pub(super) fn dequantize(q: &[f64], scales: &Params, zps: &Params, out: &mut [f64], outer: usize, c_n: usize, inner: usize, dbl: bool) -> Option<()> {
    if q.len() != outer.checked_mul(c_n)?.checked_mul(inner)? || out.len() != q.len() || !scales.fits(c_n) || !zps.fits(c_n) {
        return None;
    }
    let mut i = 0;
    for _ in 0..outer {
        for c in 0..c_n {
            let s = if dbl { scales.get(c) } else { fround(scales.get(c)) };
            let z = zps.get(c);
            for _ in 0..inner {
                out[i] = s * fround(q[i] - z);
                i += 1;
            }
        }
    }
    Some(())
}

/// `qFakeQuant`: the fake-quantized values (x's precision) and the mask.
#[allow(clippy::too_many_arguments)]
pub(super) fn fake_quant(x: &[f64], f32_input: bool, scales: &Params, zps: &Params, out: &mut [f64], mask: &mut [f64], outer: usize, c_n: usize, inner: usize, qmin: f64, qmax: f64) -> Option<()> {
    if x.len() != outer.checked_mul(c_n)?.checked_mul(inner)? || out.len() != x.len() || mask.len() != x.len() || !scales.fits(c_n) || !zps.fits(c_n) {
        return None;
    }
    let mut i = 0;
    for _ in 0..outer {
        for c in 0..c_n {
            let s = fround(scales.get(c));
            let inv = fround(1.0 / s);
            let z = zps.get(c);
            for _ in 0..inner {
                let p = x[i] * inv;
                let r = rint(if f32_input { fround(p) } else { p }) + z;
                mask[i] = if r >= qmin && r <= qmax { 1.0 } else { 0.0 };
                let cl = if r < qmin {
                    qmin
                } else if r > qmax {
                    qmax
                } else {
                    r
                };
                out[i] = s * (cl - z);
                i += 1;
            }
        }
    }
    Some(())
}

/// An integer zero point small enough for the i32 products below.
fn small_int(x: f64) -> Option<i32> {
    (x.fract() == 0.0 && x.abs() <= 255.0).then_some(x as i32)
}

/// The integers of an 8-bit storage minus a zero point (`None` for a value
/// that is not an 8-bit integer).
fn shifted(v: &[f64], zp: i32) -> Option<Vec<i32>> {
    let mut out = Vec::new();
    out.try_reserve_exact(v.len()).ok()?;
    for &x in v {
        if x.fract() != 0.0 || !(-128.0..=255.0).contains(&x) {
            return None;
        }
        out.push(x as i32 - zp);
    }
    Some(out)
}

/// The exact integer dot product: i32 products (each below 2^18 in
/// magnitude) summed in i32 over blocks that cannot overflow, the blocks in
/// i64. It is the double sum of the JavaScript loop, which is exact.
#[inline]
fn dot(a: &[i32], b: &[i32]) -> i64 {
    let mut total = 0i64;
    for (ca, cb) in a.chunks(4096).zip(b.chunks(4096)) {
        let mut s = 0i32;
        for (&x, &y) in ca.iter().zip(cb) {
            s += x * y;
        }
        total += s as i64;
    }
    total
}

/// `qMatmul`: acc[i, j] = sum_k (x[i, k] - x_zp) * (w[j, k] - w_zp[j]),
/// exact integers (the JavaScript loop's double sums).
#[allow(clippy::too_many_arguments)]
pub(super) fn matmul(x: &[f64], w: &[f64], wzp: &Params, out: &mut [f64], xzp: f64, m: usize, k: usize, n: usize, poll_units: usize, mut poll: impl FnMut() -> bool) -> Option<()> {
    if x.len() != m.checked_mul(k)? || w.len() != n.checked_mul(k)? || out.len() != m.checked_mul(n)? || !wzp.fits(n) {
        return None;
    }
    let xi = shifted(x, small_int(xzp)?)?;
    let mut wi = Vec::new();
    wi.try_reserve_exact(w.len()).ok()?;
    for j in 0..n {
        wi.extend(shifted(&w[j * k..(j + 1) * k], small_int(wzp.get(j))?)?);
    }
    let rows_per_poll = (poll_units / k.max(1).saturating_mul(n).max(1)).max(1);
    for i in 0..m {
        if i % rows_per_poll == rows_per_poll - 1 && poll() {
            return None;
        }
        let xr = &xi[i * k..(i + 1) * k];
        for j in 0..n {
            out[i * n + j] = dot(xr, &wi[j * k..(j + 1) * k]) as f64;
        }
    }
    Some(())
}

/// The dimensions of `qConv2d`: [B, C, H, W] input, [O, Cg, Kh, Kw] weight,
/// stride, padding, dilation, groups and the [Ho, Wo] output.
pub(super) struct ConvDims {
    pub b: usize,
    pub c: usize,
    pub h: usize,
    pub w: usize,
    pub o: usize,
    pub cg: usize,
    pub kh: usize,
    pub kw: usize,
    pub sh: usize,
    pub sw: usize,
    pub ph: usize,
    pub pw: usize,
    pub dh: usize,
    pub dw: usize,
    pub groups: usize,
    pub ho: usize,
    pub wo: usize,
}

/// `qConv2d`: the exact integer accumulators, padding reading x_zp (a zero
/// once the zero point is taken off). Each output pixel's input patch is
/// gathered once and dotted with every output channel of its group.
pub(super) fn conv2d(x: &[f64], wt: &[f64], wzp: &Params, out: &mut [f64], xzp: f64, d: &ConvDims, poll_units: usize, mut poll: impl FnMut() -> bool) -> Option<()> {
    let ConvDims { b, c, h, w, o, cg, kh, kw, sh, sw, ph, pw, dh, dw, groups, ho, wo } = *d;
    if groups == 0 || c != cg.checked_mul(groups)? || o % groups != 0 || !wzp.fits(o) {
        return None;
    }
    if x.len() != b.checked_mul(c)?.checked_mul(h)?.checked_mul(w)? || wt.len() != o.checked_mul(cg)?.checked_mul(kh)?.checked_mul(kw)? || out.len() != b.checked_mul(o)?.checked_mul(ho)?.checked_mul(wo)? {
        return None;
    }
    // The output size tensor.js computed from the same dimensions.
    let span_h = dh.checked_mul(kh.checked_sub(1)?)?.checked_add(1)?;
    let span_w = dw.checked_mul(kw.checked_sub(1)?)?.checked_add(1)?;
    if sh == 0 || sw == 0 || h + 2 * ph < span_h || w + 2 * pw < span_w || ho != (h + 2 * ph - span_h) / sh + 1 || wo != (w + 2 * pw - span_w) / sw + 1 {
        return None;
    }
    let xi = shifted(x, small_int(xzp)?)?;
    let plen = cg * kh * kw;
    let mut wi = Vec::new();
    wi.try_reserve_exact(wt.len()).ok()?;
    for oi in 0..o {
        wi.extend(shifted(&wt[oi * plen..(oi + 1) * plen], small_int(wzp.get(oi))?)?);
    }
    let per_group = o / groups;
    let mut patch = vec![0i32; plen];
    let row_units = wo.saturating_mul(plen).saturating_mul(per_group).max(1);
    let rows_per_poll = (poll_units / row_units).max(1);
    let mut rows = 0usize;
    for bi in 0..b {
        for g in 0..groups {
            let first = g * cg;
            for hi in 0..ho {
                rows += 1;
                if rows % rows_per_poll == 0 && poll() {
                    return None;
                }
                for vi in 0..wo {
                    let mut p = 0;
                    for ci in 0..cg {
                        let plane = (bi * c + first + ci) * h;
                        for khi in 0..kh {
                            let ih = (hi * sh + khi * dh) as isize - ph as isize;
                            let inside_h = ih >= 0 && ih < h as isize;
                            for kwi in 0..kw {
                                let iw = (vi * sw + kwi * dw) as isize - pw as isize;
                                patch[p] = if inside_h && iw >= 0 && iw < w as isize { xi[(plane + ih as usize) * w + iw as usize] } else { 0 };
                                p += 1;
                            }
                        }
                    }
                    for oi in g * per_group..(g + 1) * per_group {
                        out[((bi * o + oi) * ho + hi) * wo + vi] = dot(&patch, &wi[oi * plen..(oi + 1) * plen]) as f64;
                    }
                }
            }
        }
    }
    Some(())
}

/// `qRequant`: accumulators [outer, N, inner] to the output, mode 0
/// (fbgemm), 1 (oneDNN, the zero point added in float) or 2 (float output).
#[allow(clippy::too_many_arguments)]
pub(super) fn requant(acc: &[f64], bias: Option<&[f64]>, atw: &Params, mult: &Params, out: &mut [f64], outer: usize, n: usize, inner: usize, ozp: f64, lo: f64, hi: f64, mode: u32) -> Option<()> {
    if acc.len() != outer.checked_mul(n)?.checked_mul(inner)? || out.len() != acc.len() || !atw.fits(n) || !mult.fits(n) || bias.is_some_and(|b| b.len() != n) {
        return None;
    }
    let mut i = 0;
    for _ in 0..outer {
        for j in 0..n {
            let t = atw.get(j);
            let m = mult.get(j);
            let b = bias.map_or(0.0, |b| b[j]);
            let bt = if bias.is_some() { fround(b / t) } else { 0.0 };
            for _ in 0..inner {
                let a = fround(acc[i]);
                if mode == 2 {
                    out[i] = fma32(a, t, b);
                    i += 1;
                    continue;
                }
                let ab = fround(fround(a + bt) * m);
                let q = if mode == 0 { rint(ab) + ozp } else { rint(fround(ab + ozp)) };
                out[i] = q.clamp(lo, hi);
                i += 1;
            }
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fma32_is_the_fused_float32_operation() {
        // a * a + c is 1 + 2^-24, halfway between 1 and the next float:
        // the fused result rounds that tie to even.
        let a = (1.0f64 + 2f64.powi(-12)) as f32 as f64;
        let b = a;
        let c = -(2f64.powi(-11)) as f32 as f64;
        let exact = a * b + c;
        let fused = (a as f32).mul_add(b as f32, c as f32) as f64;
        assert_eq!(fma32(a, b, c), fused);
        assert_eq!(fma32(a, b, c), fround(exact));
        for &(x, y, z) in &[(3.0f64, 0.1f64 as f32 as f64, 1e-9f64 as f32 as f64), (-1.5, 2.25, 7.0), (1e20f64 as f32 as f64, 1e-20 as f32 as f64, -1.0)] {
            assert_eq!(fma32(x, y, z), (x as f32).mul_add(y as f32, z as f32) as f64);
        }
    }

    #[test]
    fn quantize_rounds_half_to_even_and_saturates_as_fbgemm() {
        let x = [0.25f64, 0.75, -0.25, f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
        let mut out = [0.0; 6];
        quantize(&x, &Params::One(0.5), &Params::One(-2.0), &mut out, 1, 1, 6, -128.0, 127.0, 127.0, 0).unwrap();
        // 0.5 -> 0, 1.5 -> 2, -0.5 -> -0; -inf wraps past int32 when the
        // negative zero point is added, as fbgemm's vector loop does.
        assert_eq!(out, [-2.0, 0.0, -2.0, 127.0, 127.0, 127.0]);
    }
}
