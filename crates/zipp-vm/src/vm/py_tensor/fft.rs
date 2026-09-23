//! The native loop behind `_zipp_tensor.fft` (torch.fft): tensor.js's
//! `fftFactors`/`fftPlan`/`fftRec`/`fftRun`/`fft`, line for line. Every
//! arithmetic step is the JavaScript one in the same order (no fused
//! multiply-add, the interpreter's own cos/sin), so a native transform
//! stores exactly the bytes the JavaScript loop stores.
//!
//! A length whose prime factors are all at most 31 runs a mixed-radix
//! decimation-in-time Cooley-Tukey (radix 4 first, then 2, 3, 5 and the
//! odd primes to 31); any other length runs Bluestein's chirp-z algorithm
//! over a power-of-two convolution. Twiddles are cos/sin of 2*pi*k/N,
//! folded into the first octant, computed directly.
use super::super::helpers_num2::math_unary;
use crate::bytecode::MathFn as M;
use std::f64::consts::PI;

#[inline]
fn cos(x: f64) -> f64 {
    math_unary(M::Cos, x)
}

#[inline]
fn sin(x: f64) -> f64 {
    math_unary(M::Sin, x)
}

/// `twiddle`: cos and sin of 2*pi*k/n, the angle folded into the first
/// octant by the circle's symmetries (exact integer eighths of k).
fn twiddle(k: u128, n: u128) -> (f64, f64) {
    let big_t = 8 * n;
    let mut a = 8 * k;
    let (mut sc, mut ss, mut swap) = (1.0, 1.0, false);
    if 2 * a > big_t {
        a = big_t - a;
        ss = -1.0;
    }
    if 4 * a > big_t {
        a = big_t / 2 - a;
        sc = -1.0;
    }
    if 8 * a > big_t {
        a = big_t / 4 - a;
        swap = true;
    }
    let t = 2.0 * PI * a as f64 / big_t as f64;
    let (c, s) = (cos(t), sin(t));
    (sc * if swap { s } else { c }, ss * if swap { c } else { s })
}

/// `fftFactors`: 4s, then 2s, 3s, 5s and the odd primes to 31, or None
/// for a larger prime factor.
fn factors(n: usize) -> Option<Vec<usize>> {
    let mut f = Vec::new();
    let mut m = n;
    while m % 4 == 0 {
        f.push(4);
        m /= 4;
    }
    while m % 2 == 0 {
        f.push(2);
        m /= 2;
    }
    while m % 3 == 0 {
        f.push(3);
        m /= 3;
    }
    while m % 5 == 0 {
        f.push(5);
        m /= 5;
    }
    let mut q = 7;
    while q <= 31 {
        while m % q == 0 {
            f.push(q);
            m /= q;
        }
        q += 2;
    }
    (m == 1).then_some(f)
}

struct Blue {
    m: usize,
    sub: Box<Plan>,
    inv: Box<Plan>,
    wr: Vec<f64>,
    wi: Vec<f64>,
    br: Vec<f64>,
    bi: Vec<f64>,
    ar: Vec<f64>,
    ai: Vec<f64>,
    tr: Vec<f64>,
    ti: Vec<f64>,
}

/// `fftPlan`: a transform of length n in direction `sign` (-1 forward).
pub(super) struct Plan {
    n: usize,
    sign: f64,
    factors: Vec<usize>,
    cr: Vec<f64>,
    ci: Vec<f64>,
    blue: Option<Blue>,
    tmpr: [f64; 32],
    tmpi: [f64; 32],
    pr: [f64; 16],
    pi: [f64; 16],
    mr: [f64; 16],
    mi: [f64; 16],
}

impl Plan {
    pub(super) fn new(n: usize, sign: f64) -> Option<Plan> {
        if let Some(factors) = factors(n) {
            let mut cr = Vec::new();
            let mut ci = Vec::new();
            cr.try_reserve_exact(n).ok()?;
            ci.try_reserve_exact(n).ok()?;
            for k in 0..n {
                let (c, s) = twiddle(k as u128, n as u128);
                cr.push(c);
                ci.push(sign * s);
            }
            return Some(Plan {
                n,
                sign,
                factors,
                cr,
                ci,
                blue: None,
                tmpr: [0.0; 32],
                tmpi: [0.0; 32],
                pr: [0.0; 16],
                pi: [0.0; 16],
                mr: [0.0; 16],
                mi: [0.0; 16],
            });
        }
        let mut m = 1usize;
        while m < 2 * n - 1 {
            m = m.checked_mul(2)?;
        }
        let mut sub = Plan::new(m, -1.0)?;
        let inv = Plan::new(m, 1.0)?;
        let mut wr = zeros(n)?;
        let mut wi = zeros(n)?;
        let two_n = (n as u128) * 2;
        for j in 0..n {
            let (c, s) = twiddle((j as u128) * (j as u128) % two_n, two_n);
            wr[j] = c;
            wi[j] = sign * s;
        }
        let mut br = zeros(m)?;
        let mut bi = zeros(m)?;
        for j in 0..n {
            br[j] = wr[j];
            bi[j] = -wi[j];
            if j > 0 {
                br[m - j] = wr[j];
                bi[m - j] = -wi[j];
            }
        }
        let mut bhr = zeros(m)?;
        let mut bhi = zeros(m)?;
        sub.run(&br, &bi, &mut bhr, &mut bhi);
        Some(Plan {
            n,
            sign,
            factors: Vec::new(),
            cr: Vec::new(),
            ci: Vec::new(),
            blue: Some(Blue {
                m,
                sub: Box::new(sub),
                inv: Box::new(inv),
                wr,
                wi,
                br: bhr,
                bi: bhi,
                ar: zeros(m)?,
                ai: zeros(m)?,
                tr: zeros(m)?,
                ti: zeros(m)?,
            }),
            tmpr: [0.0; 32],
            tmpi: [0.0; 32],
            pr: [0.0; 16],
            pi: [0.0; 16],
            mr: [0.0; 16],
            mi: [0.0; 16],
        })
    }

    /// `fftRec`: y[oo + k] (k < len) = the DFT of x[io + j * stride].
    #[allow(clippy::too_many_arguments)]
    fn rec(&mut self, len: usize, xr: &[f64], xi: &[f64], io: usize, stride: usize, yr: &mut [f64], yi: &mut [f64], oo: usize, fi: usize) {
        if len == 1 {
            yr[oo] = xr[io];
            yi[oo] = xi[io];
            return;
        }
        let r = self.factors[fi];
        let m = len / r;
        let big_n = self.n;
        let step = big_n / len;
        for q in 0..r {
            self.rec(m, xr, xi, io + q * stride, stride * r, yr, yi, oo + q * m, fi + 1);
        }
        let (cr, ci) = (&self.cr, &self.ci);
        for k in 0..m {
            if r == 2 {
                let a = oo + k;
                let b = a + m;
                let mut br = yr[b];
                let mut bi = yi[b];
                if k != 0 {
                    let e = k * step;
                    let (wr, wi) = (cr[e], ci[e]);
                    let t = br * wr - bi * wi;
                    bi = br * wi + bi * wr;
                    br = t;
                }
                let (ar, ai) = (yr[a], yi[a]);
                yr[a] = ar + br;
                yi[a] = ai + bi;
                yr[b] = ar - br;
                yi[b] = ai - bi;
            } else if r == 4 {
                let i0 = oo + k;
                let i1 = i0 + m;
                let i2 = i1 + m;
                let i3 = i2 + m;
                let (mut x1r, mut x1i, mut x2r, mut x2i, mut x3r, mut x3i) = (yr[i1], yi[i1], yr[i2], yi[i2], yr[i3], yi[i3]);
                if k != 0 {
                    let mut e = k * step;
                    let (mut wr, mut wi) = (cr[e], ci[e]);
                    let mut t = x1r * wr - x1i * wi;
                    x1i = x1r * wi + x1i * wr;
                    x1r = t;
                    e = 2 * k * step;
                    wr = cr[e];
                    wi = ci[e];
                    t = x2r * wr - x2i * wi;
                    x2i = x2r * wi + x2i * wr;
                    x2r = t;
                    e = 3 * k * step;
                    wr = cr[e];
                    wi = ci[e];
                    t = x3r * wr - x3i * wi;
                    x3i = x3r * wi + x3i * wr;
                    x3r = t;
                }
                let (x0r, x0i) = (yr[i0], yi[i0]);
                let (s0r, s0i, d0r, d0i) = (x0r + x2r, x0i + x2i, x0r - x2r, x0i - x2i);
                let (s1r, s1i, d1r, d1i) = (x1r + x3r, x1i + x3i, x1r - x3r, x1i - x3i);
                let sg = self.sign;
                let (jr, ji) = if sg > 0.0 { (-d1i, d1r) } else { (d1i, -d1r) };
                yr[i0] = s0r + s1r;
                yi[i0] = s0i + s1i;
                yr[i1] = d0r + jr;
                yi[i1] = d0i + ji;
                yr[i2] = s0r - s1r;
                yi[i2] = s0i - s1i;
                yr[i3] = d0r - jr;
                yi[i3] = d0i - ji;
            } else {
                let (tr, ti) = (&mut self.tmpr, &mut self.tmpi);
                for q in 0..r {
                    let at = oo + q * m + k;
                    let mut xr0 = yr[at];
                    let mut xi0 = yi[at];
                    if q != 0 && k != 0 {
                        let e = q * k * step;
                        let (wr, wi) = (cr[e], ci[e]);
                        let t = xr0 * wr - xi0 * wi;
                        xi0 = xr0 * wi + xi0 * wr;
                        xr0 = t;
                    }
                    tr[q] = xr0;
                    ti[q] = xi0;
                }
                let sg = self.sign;
                if r > 5 {
                    let h = (r - 1) >> 1;
                    let big = big_n / r;
                    let (pr, pi, mr, mi) = (&mut self.pr, &mut self.pi, &mut self.mr, &mut self.mi);
                    let mut x0r = tr[0];
                    let mut x0i = ti[0];
                    for q in 1..=h {
                        pr[q] = tr[q] + tr[r - q];
                        pi[q] = ti[q] + ti[r - q];
                        mr[q] = tr[q] - tr[r - q];
                        mi[q] = ti[q] - ti[r - q];
                        x0r += pr[q];
                        x0i += pi[q];
                    }
                    yr[oo + k] = x0r;
                    yi[oo + k] = x0i;
                    for s in 1..=h {
                        let (mut ar, mut ai, mut br, mut bi) = (tr[0], ti[0], 0.0, 0.0);
                        for q in 1..=h {
                            let e = ((q * s) % r) * big;
                            let (wr, wi) = (cr[e], ci[e]);
                            ar += wr * pr[q];
                            ai += wr * pi[q];
                            br += wi * mr[q];
                            bi += wi * mi[q];
                        }
                        yr[oo + s * m + k] = ar - bi;
                        yi[oo + s * m + k] = ai + br;
                        yr[oo + (r - s) * m + k] = ar + bi;
                        yi[oo + (r - s) * m + k] = ai - br;
                    }
                } else if r == 3 {
                    let (t1r, t1i) = (tr[1] + tr[2], ti[1] + ti[2]);
                    let (t2r, t2i) = (tr[0] - 0.5 * t1r, ti[0] - 0.5 * t1i);
                    let (t3r, t3i) = (0.8660254037844386 * (tr[1] - tr[2]), 0.8660254037844386 * (ti[1] - ti[2]));
                    let (ur, ui) = if sg > 0.0 { (-t3i, t3r) } else { (t3i, -t3r) };
                    let i0 = oo + k;
                    let i1 = i0 + m;
                    let i2 = i1 + m;
                    yr[i0] = tr[0] + t1r;
                    yi[i0] = ti[0] + t1i;
                    yr[i1] = t2r + ur;
                    yi[i1] = t2i + ui;
                    yr[i2] = t2r - ur;
                    yi[i2] = t2i - ui;
                } else {
                    let (c1, c2, s1, s2) = (0.30901699437494745, -0.8090169943749475, 0.9510565162951535, 0.5877852522924731);
                    let (t1r, t1i, t2r, t2i) = (tr[1] + tr[4], ti[1] + ti[4], tr[2] + tr[3], ti[2] + ti[3]);
                    let (d1r, d1i, d2r, d2i) = (tr[1] - tr[4], ti[1] - ti[4], tr[2] - tr[3], ti[2] - ti[3]);
                    let (a1r, a1i) = (tr[0] + c1 * t1r + c2 * t2r, ti[0] + c1 * t1i + c2 * t2i);
                    let (a2r, a2i) = (tr[0] + c2 * t1r + c1 * t2r, ti[0] + c2 * t1i + c1 * t2i);
                    let (b1r, b1i) = (s1 * d1r + s2 * d2r, s1 * d1i + s2 * d2i);
                    let (b2r, b2i) = (s2 * d1r - s1 * d2r, s2 * d1i - s1 * d2i);
                    let (u1r, u1i) = if sg > 0.0 { (-b1i, b1r) } else { (b1i, -b1r) };
                    let (u2r, u2i) = if sg > 0.0 { (-b2i, b2r) } else { (b2i, -b2r) };
                    let i0 = oo + k;
                    let i1 = i0 + m;
                    let i2 = i1 + m;
                    let i3 = i2 + m;
                    let i4 = i3 + m;
                    yr[i0] = tr[0] + t1r + t2r;
                    yi[i0] = ti[0] + t1i + t2i;
                    yr[i1] = a1r + u1r;
                    yi[i1] = a1i + u1i;
                    yr[i4] = a1r - u1r;
                    yi[i4] = a1i - u1i;
                    yr[i2] = a2r + u2r;
                    yi[i2] = a2i + u2i;
                    yr[i3] = a2r - u2r;
                    yi[i3] = a2i - u2i;
                }
            }
        }
    }

    /// `fftRun`: the unnormalized transform of (xr, xi) into (yr, yi).
    pub(super) fn run(&mut self, xr: &[f64], xi: &[f64], yr: &mut [f64], yi: &mut [f64]) {
        let n = self.n;
        let Some(b) = self.blue.as_mut() else {
            self.rec(n, xr, xi, 0, 1, yr, yi, 0, 0);
            return;
        };
        let m = b.m;
        for j in 0..m {
            b.ar[j] = 0.0;
            b.ai[j] = 0.0;
        }
        for j in 0..n {
            let (a, c) = (xr[j], xi[j]);
            b.ar[j] = a * b.wr[j] - c * b.wi[j];
            b.ai[j] = a * b.wi[j] + c * b.wr[j];
        }
        b.sub.run(&b.ar, &b.ai, &mut b.tr, &mut b.ti);
        for j in 0..m {
            let (a, c, d, e) = (b.tr[j], b.ti[j], b.br[j], b.bi[j]);
            b.ar[j] = a * d - c * e;
            b.ai[j] = a * e + c * d;
        }
        b.inv.run(&b.ar, &b.ai, &mut b.tr, &mut b.ti);
        for k in 0..n {
            let a = b.tr[k] / m as f64;
            let c = b.ti[k] / m as f64;
            yr[k] = a * b.wr[k] - c * b.wi[k];
            yi[k] = a * b.wi[k] + c * b.wr[k];
        }
    }
}

/// Whether a length-n transform runs mixed radix (no Bluestein).
pub(super) fn smooth(n: usize) -> bool {
    factors(n).is_some()
}

/// The Bluestein convolution length of n: the power of two at least 2n-1.
fn blue_len(n: usize) -> usize {
    let mut m = 1usize;
    while m < 2 * n - 1 {
        m *= 2;
    }
    m
}

/// Work units of one length-n transform (for the budget).
pub(super) fn estimate_units(n: usize) -> u64 {
    match factors(n) {
        Some(f) => (n as u64).saturating_mul(f.iter().map(|&x| x as u64).sum::<u64>() + 1),
        None => {
            let m = blue_len(n);
            estimate_units(m).saturating_mul(2).saturating_add((m as u64).saturating_mul(4))
        }
    }
}

/// The work of building a length-n plan (its twiddles and, for
/// Bluestein, the transformed chirp).
pub(super) fn plan_units(n: usize) -> u64 {
    match factors(n) {
        Some(_) => (n as u64).saturating_mul(2),
        None => {
            let m = blue_len(n);
            estimate_units(m).saturating_add((m as u64).saturating_mul(8)).saturating_add(n as u64 * 2)
        }
    }
}

/// f64 working values a length-n plan and its row buffers hold.
pub(super) fn plan_elems(n: usize) -> usize {
    match factors(n) {
        Some(_) => n.saturating_mul(6),
        None => blue_len(n).saturating_mul(14).saturating_add(n.saturating_mul(6)),
    }
}

fn zeros(n: usize) -> Option<Vec<f64>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).ok()?;
    v.resize(n, 0.0);
    Some(v)
}

/// `fft`'s row loop: `rows` rows of `n_in` input elements (interleaved
/// pairs unless the input is real, modes 1 and 3) into `out`, returning
/// false when `poll` asks to stop between rows.
#[allow(clippy::too_many_arguments)]
pub(super) fn transform(plan: &mut Plan, a: &[f64], out: &mut [f64], rows: usize, n_in: usize, mode: u32, scale: f64, mut poll: impl FnMut() -> bool) -> bool {
    let n = plan.n;
    let half = (n >> 1) + 1;
    let n_out = if mode == 1 { half } else { n };
    let real_in = mode == 1 || mode == 3;
    let (mut xr, mut xi, mut yr, mut yi) = (vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    for row in 0..rows {
        if row % 16 == 15 && poll() {
            return false;
        }
        for j in 0..n {
            xr[j] = 0.0;
            xi[j] = 0.0;
        }
        if mode == 0 {
            let c = n_in.min(n);
            let base = 2 * row * n_in;
            for j in 0..c {
                xr[j] = a[base + 2 * j];
                xi[j] = a[base + 2 * j + 1];
            }
        } else if real_in {
            let c = n_in.min(n);
            let base = row * n_in;
            xr[..c].copy_from_slice(&a[base..base + c]);
        } else {
            let c = n_in.min(half);
            let base = 2 * row * n_in;
            for k in 0..c {
                let (re, im) = (a[base + 2 * k], a[base + 2 * k + 1]);
                if k == 0 || 2 * k == n {
                    xr[k] = re;
                    continue;
                }
                xr[k] = re;
                xi[k] = im;
                xr[n - k] = re;
                xi[n - k] = -im;
            }
        }
        plan.run(&xr, &xi, &mut yr, &mut yi);
        if real_in {
            yi[0] = 0.0;
            if n & 1 == 0 {
                yi[n >> 1] = 0.0;
            }
            if mode == 3 {
                for k in half..n {
                    yr[k] = yr[n - k];
                    yi[k] = -yi[n - k];
                }
            }
        }
        if mode == 2 {
            let base = row * n;
            for j in 0..n {
                out[base + j] = yr[j] * scale;
            }
        } else {
            let base = 2 * row * n_out;
            for k in 0..n_out {
                out[base + 2 * k] = yr[k] * scale;
                out[base + 2 * k + 1] = yi[k] * scale;
            }
        }
    }
    true
}
