//! Freestanding float32 kernels for Graph v2: no `std`, no allocator, no libc,
//! no imports. The host instantiates the module, reads `memory` and
//! `__heap_base`, and drives an arena itself; every entry point below takes
//! byte offsets into that memory.
//!
//! **The contract is bit-for-bit.** `gpu-lab` holds this backend to exact
//! equality with the JavaScript reference over whole training steps, not to a
//! tolerance. Two things keep that true and must survive any edit here:
//!
//!   * Rust never contracts `a * b + c` into an FMA -- each operation rounds
//!     on its own, which is what the reference does. (The C this replaced
//!     needed `-ffp-contract=off` to promise the same thing.)
//!   * Every output accumulates its terms in index order. SIMD therefore goes
//!     *across outputs*, never along the reduced axis: four columns at a time,
//!     four independent running sums, never a horizontal add.
//!
//! Elementary functions live in `math` and are evaluated in double, then
//! rounded once -- see that module for why they are hand-written rather than
//! taken from a libm.

#![cfg_attr(target_arch = "wasm32", no_std)]
#![allow(clippy::missing_safety_doc)]

pub mod math;

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! { core::arch::wasm32::unreachable() }

#[cfg(target_arch = "wasm32")]
mod kernels {
    use crate::math;
    use core::arch::wasm32::*;

    /// IEEE float32 square root. `core` exposes no scalar `sqrt`, but the SIMD
    /// one is the same instruction and exact either way.
    #[inline]
    fn sqrtf(x: f32) -> f32 { f32x4_extract_lane::<0>(f32x4_sqrt(f32x4_splat(x))) }

    #[inline]
    unsafe fn s<'a>(p: *const f32, n: i32) -> &'a [f32] { core::slice::from_raw_parts(p, n as usize) }
    #[inline]
    unsafe fn sm<'a>(p: *mut f32, n: i32) -> &'a mut [f32] { core::slice::from_raw_parts_mut(p, n as usize) }

    #[no_mangle]
    pub unsafe extern "C" fn fill(o: *mut f32, n: i32, value: f32) {
        for x in sm(o, n) { *x = value; }
    }

    /// `op`: 0 add, 1 sub, 2 mul, 3 div. `mode`: 0 equal shapes, 1 scalar a, 2 scalar b.
    #[no_mangle]
    pub unsafe extern "C" fn binary(a: *const f32, b: *const f32, o: *mut f32, n: i32, mode: i32, op: i32) {
        let o = sm(o, n);
        match mode {
            0 => { let (a, b) = (s(a, n), s(b, n));
                   for i in 0..o.len() { o[i] = apply(op, a[i], b[i]); } }
            1 => { let (x, b) = (*a, s(b, n));
                   for i in 0..o.len() { o[i] = apply(op, x, b[i]); } }
            _ => { let (a, y) = (s(a, n), *b);
                   for i in 0..o.len() { o[i] = apply(op, a[i], y); } }
        }
    }

    #[inline(always)]
    fn apply(op: i32, x: f32, y: f32) -> f32 {
        match op { 0 => x + y, 1 => x - y, 2 => x * y, _ => x / y }
    }

    /// NumPy broadcasting over four padded dimensions; a stride of 0 repeats an operand.
    #[no_mangle]
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn binary_strided(a: *const f32, b: *const f32, o: *mut f32, op: i32,
        d0: i32, d1: i32, d2: i32, d3: i32, a0: i32, a1: i32, a2: i32, a3: i32,
        b0: i32, b1: i32, b2: i32, b3: i32) {
        let mut i = 0isize;
        for x0 in 0..d0 { for x1 in 0..d1 { for x2 in 0..d2 {
            let pa = a.offset((x0*a0 + x1*a1 + x2*a2) as isize);
            let pb = b.offset((x0*b0 + x1*b1 + x2*b2) as isize);
            for x3 in 0..d3 {
                *o.offset(i + x3 as isize) =
                    apply(op, *pa.offset((x3*a3) as isize), *pb.offset((x3*b3) as isize));
            }
            i += d3 as isize;
        }}}
    }

    /// Permutations as a strided gather.
    #[no_mangle]
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn gather4(a: *const f32, o: *mut f32,
        d0: i32, d1: i32, d2: i32, d3: i32, s0: i32, s1: i32, s2: i32, s3: i32) {
        let mut i = 0isize;
        for x0 in 0..d0 { for x1 in 0..d1 { for x2 in 0..d2 {
            let p = a.offset((x0*s0 + x1*s1 + x2*s2) as isize);
            for x3 in 0..d3 { *o.offset(i) = *p.offset((x3*s3) as isize); i += 1; }
        }}}
    }

    /// `op`: 0 relu (NaN kept), 1 positive, 2 neg, 3 exp, 4 log, 5 sqrt,
    /// 6 tanh, 7 sigmoid, 8 gelu, 9 gelu_grad.
    #[no_mangle]
    pub unsafe extern "C" fn unary(a: *const f32, o: *mut f32, n: i32, op: i32) {
        let (a, o) = (s(a, n), sm(o, n));
        for i in 0..o.len() {
            let x = a[i];
            o[i] = match op {
                0 => if x > 0.0 || x != x { x } else { 0.0 },
                1 => if x > 0.0 { 1.0 } else { 0.0 },
                2 => -x,
                3 => math::exp(x as f64) as f32,
                4 => math::log(x as f64) as f32,
                5 => sqrtf(x),
                6 => math::tanh(x as f64) as f32,
                7 => math::sigmoid(x as f64) as f32,
                8 => math::gelu(x as f64) as f32,
                _ => math::gelu_grad(x as f64) as f32,
            };
        }
    }

    #[no_mangle]
    pub unsafe extern "C" fn pair_sum(a: *const f32, o: *mut f32, n: i32) {
        for i in 0..(n + 1) / 2 {
            let hi = if 2*i + 1 < n { *a.offset((2*i + 1) as isize) } else { 0.0 };
            *o.offset(i as isize) = *a.offset((2*i) as isize) + hi;
        }
    }

    /// Sum (or mean) over one axis of `[outer, len, inner]`, accumulating in axis order.
    #[no_mangle]
    pub unsafe extern "C" fn reduce_axis(a: *const f32, o: *mut f32, outer: i32, len: i32, inner: i32, mean: i32) {
        for p in 0..outer {
            let out = sm(o.offset((p*inner) as isize), inner);
            for x in out.iter_mut() { *x = 0.0; }
            for j in 0..len {
                let row = s(a.offset(((p*len + j)*inner) as isize), inner);
                for i in 0..out.len() { out[i] += row[i]; }
            }
            if mean != 0 { let d = len as f32; for x in out.iter_mut() { *x /= d; } }
        }
    }

    /// Row maximum (NaN-propagating, like `Math.max`) and the float32 sum of `exp(x - max)`.
    unsafe fn row_stats(x: &[f32]) -> (f32, f32) {
        let mut m = x[0];
        for &v in &x[1..] { if v > m || v != v { m = v; } }
        let mut sum = 0.0f32;
        for &v in x { sum += math::exp((v - m) as f64) as f32; }
        (m, sum)
    }

    #[no_mangle]
    pub unsafe extern "C" fn softmax_rows(a: *const f32, o: *mut f32, rows: i32, cols: i32, logmode: i32) {
        for r in 0..rows {
            let x = s(a.offset((r*cols) as isize), cols);
            let y = sm(o.offset((r*cols) as isize), cols);
            let (m, sum) = row_stats(x);
            if logmode != 0 {
                let ls = math::log(sum as f64) as f32;
                for j in 0..y.len() { y[j] = (x[j] - m) - ls; }
            } else {
                for j in 0..y.len() { y[j] = math::exp((x[j] - m) as f64) as f32 / sum; }
            }
        }
    }

    #[inline]
    fn target(t: f32, cols: i32) -> usize {
        let c = t as i32;
        if c < 0 || c >= cols { 0 } else { c as usize }
    }

    /// Per-row cross-entropy losses; the host reduces them pairwise and divides by N.
    #[no_mangle]
    pub unsafe extern "C" fn ce_rows(a: *const f32, t: *const f32, o: *mut f32, rows: i32, cols: i32) {
        for r in 0..rows {
            let x = s(a.offset((r*cols) as isize), cols);
            let (m, sum) = row_stats(x);
            *o.offset(r as isize) = math::log(sum as f64) as f32 - (x[target(*t.offset(r as isize), cols)] - m);
        }
    }

    /// `d(mean cross-entropy)/d(logits) = (softmax - onehot) / N`.
    #[no_mangle]
    pub unsafe extern "C" fn ce_grad(a: *const f32, t: *const f32, o: *mut f32, rows: i32, cols: i32) {
        let n = rows as f32;
        for r in 0..rows {
            let x = s(a.offset((r*cols) as isize), cols);
            let y = sm(o.offset((r*cols) as isize), cols);
            let (m, sum) = row_stats(x);
            let c = target(*t.offset(r as isize), cols);
            for j in 0..y.len() {
                let one = if j == c { 1.0 } else { 0.0 };
                y[j] = (math::exp((x[j] - m) as f64) as f32 / sum - one) / n;
            }
        }
    }

    /// `C[m,n] = A[m,k] @ B[k,n]`. A 4x8 register block keeps eight SIMD
    /// accumulators live across k; every output still sums its k products in
    /// order, because the vectors span columns rather than the reduced axis.
    unsafe fn matmul_panel(a: *const f32, b: *const f32, c: *mut f32, m: i32, k: i32, n: i32) {
        let (mi, ki, ni) = (m as isize, k as isize, n as isize);
        let mut r = 0isize;
        while r + 4 <= mi {
            let mut col = 0isize;
            while col + 8 <= ni {
                let z = f32x4_splat(0.0);
                let (mut c00, mut c01, mut c10, mut c11) = (z, z, z, z);
                let (mut c20, mut c21, mut c30, mut c31) = (z, z, z, z);
                let (a0, a1) = (a.offset(r*ki), a.offset((r + 1)*ki));
                let (a2, a3) = (a.offset((r + 2)*ki), a.offset((r + 3)*ki));
                for j in 0..ki {
                    let bp = b.offset(j*ni + col);
                    let b0 = v128_load(bp as *const v128);
                    let b1 = v128_load(bp.offset(4) as *const v128);
                    let x0 = f32x4_splat(*a0.offset(j));
                    c00 = f32x4_add(c00, f32x4_mul(x0, b0)); c01 = f32x4_add(c01, f32x4_mul(x0, b1));
                    let x1 = f32x4_splat(*a1.offset(j));
                    c10 = f32x4_add(c10, f32x4_mul(x1, b0)); c11 = f32x4_add(c11, f32x4_mul(x1, b1));
                    let x2 = f32x4_splat(*a2.offset(j));
                    c20 = f32x4_add(c20, f32x4_mul(x2, b0)); c21 = f32x4_add(c21, f32x4_mul(x2, b1));
                    let x3 = f32x4_splat(*a3.offset(j));
                    c30 = f32x4_add(c30, f32x4_mul(x3, b0)); c31 = f32x4_add(c31, f32x4_mul(x3, b1));
                }
                for (t, (lo, hi)) in [(c00, c01), (c10, c11), (c20, c21), (c30, c31)].into_iter().enumerate() {
                    let o = c.offset((r + t as isize)*ni + col);
                    v128_store(o as *mut v128, lo);
                    v128_store(o.offset(4) as *mut v128, hi);
                }
                col += 8;
            }
            for rr in r..r + 4 { matmul_tail(a, b, c, rr, col, ki, ni); }
            r += 4;
        }
        while r < mi { matmul_tail(a, b, c, r, 0, ki, ni); r += 1; }
    }

    /// The columns a register block did not cover, in the same ikj order.
    #[inline]
    unsafe fn matmul_tail(a: *const f32, b: *const f32, c: *mut f32, r: isize, from: isize, k: isize, n: isize) {
        let o = c.offset(r*n);
        for cc in from..n { *o.offset(cc) = 0.0; }
        for j in 0..k {
            let x = *a.offset(r*k + j);
            let bp = b.offset(j*n);
            for cc in from..n { *o.offset(cc) += x * *bp.offset(cc); }
        }
    }

    #[no_mangle]
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn bmm(a: *const f32, b: *const f32, o: *mut f32,
        batch: i32, m: i32, k: i32, n: i32, sa: i32, sb: i32) {
        for t in 0..batch as isize {
            matmul_panel(a.offset(t * sa as isize), b.offset(t * sb as isize),
                         o.offset(t * (m as isize) * (n as isize)), m, k, n);
        }
    }

    /// `C[m,n] = A[m,k] @ B[n,k]^T`: one contiguous row of B per output column,
    /// which is how a checkpoint stores a linear layer. Four columns at a time,
    /// four independent sums, each still in ascending k.
    unsafe fn matmul_t_panel(a: *const f32, b: *const f32, c: *mut f32, m: isize, k: isize, n: isize) {
        let mut col = 0isize;
        while col + 4 <= n {
            let (b0, b1) = (b.offset(col*k), b.offset((col + 1)*k));
            let (b2, b3) = (b.offset((col + 2)*k), b.offset((col + 3)*k));
            for r in 0..m {
                let ar = a.offset(r*k);
                let mut acc = f32x4_splat(0.0);
                for j in 0..k {
                    let w = f32x4(*b0.offset(j), *b1.offset(j), *b2.offset(j), *b3.offset(j));
                    acc = f32x4_add(acc, f32x4_mul(f32x4_splat(*ar.offset(j)), w));
                }
                v128_store(c.offset(r*n + col) as *mut v128, acc);
            }
            col += 4;
        }
        while col < n {
            let bc = b.offset(col*k);
            for r in 0..m {
                let ar = a.offset(r*k);
                let mut sum = 0.0f32;
                for j in 0..k { sum += *ar.offset(j) * *bc.offset(j); }
                *c.offset(r*n + col) = sum;
            }
            col += 1;
        }
    }

    #[no_mangle]
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn bmm_t(a: *const f32, b: *const f32, o: *mut f32,
        batch: i32, m: i32, k: i32, n: i32, sa: i32, sb: i32) {
        let (m, k, n) = (m as isize, k as isize, n as isize);
        for t in 0..batch as isize {
            matmul_t_panel(a.offset(t * sa as isize), b.offset(t * sb as isize), o.offset(t*m*n), m, k, n);
        }
    }

    /// `C[m,n] = A[m,k] @ W[n,k]^T` where W is block-quantized and stays that way.
    ///
    /// `dtype` is 0 for Q4_K and 1 for Q6_K; both pack 256 values to a block, so
    /// only the block's byte size and its decoder differ. Four output columns
    /// are decoded into `scratch` (4*k floats, supplied by the host arena) and
    /// every row of A reuses that decode, so the weight is read once per column
    /// block and never expanded anywhere. The decoder is `zipp_quants`, the
    /// same one the GGUF reader uses -- a second copy here would be a second
    /// thing to be wrong.
    #[no_mangle]
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn bmm_quant(a: *const f32, w: *const u8, o: *mut f32, scratch: *mut f32,
        batch: i32, m: i32, k: i32, n: i32, sa: i32, sb: i32, dtype: i32) {
        const BLOCK_SIZE: usize = 256; // Q4_K and Q6_K alike
        let bytes_per_block = if dtype == 0 { zipp_quants::q4_k::BYTES_PER_BLOCK }
                              else { zipp_quants::q6_k::BYTES_PER_BLOCK };
        let (mi, ki, ni) = (m as isize, k as isize, n as isize);
        let per = ki / BLOCK_SIZE as isize; // whole blocks per row, always exact
        let row_bytes = per * bytes_per_block as isize;
        for t in 0..batch as isize {
            let a = a.offset(t * sa as isize);
            let c = o.offset(t*mi*ni);
            let w = w.offset((t * sb as isize / BLOCK_SIZE as isize) * bytes_per_block as isize);
            let mut col = 0isize;
            while col < ni {
                let span = if ni - col >= 4 { 4 } else { ni - col };
                for i in 0..span {
                    let src = core::slice::from_raw_parts(w.offset((col + i) * row_bytes), row_bytes as usize);
                    let dst = core::slice::from_raw_parts_mut(scratch.offset(i * ki), ki as usize);
                    if dtype == 0 { zipp_quants::q4_k::dequantize(src, dst) }
                    else { zipp_quants::q6_k::dequantize(src, dst) }
                }
                // The decoded rows are exactly a [span, k] f32 weight, so the
                // transposed panel computes the rest.
                matmul_t_panel_into(a, scratch, c, mi, ki, ni, col, span);
                col += span;
            }
        }
    }

    /// `matmul_t_panel` over `span` decoded columns living contiguously at
    /// `b[0..span*k]`, writing them back at `col` of an `n`-wide output.
    unsafe fn matmul_t_panel_into(a: *const f32, b: *const f32, c: *mut f32,
        m: isize, k: isize, n: isize, col: isize, span: isize) {
        if span == 4 {
            let (b0, b1, b2, b3) = (b, b.offset(k), b.offset(2*k), b.offset(3*k));
            for r in 0..m {
                let ar = a.offset(r*k);
                let mut acc = f32x4_splat(0.0);
                for j in 0..k {
                    let w = f32x4(*b0.offset(j), *b1.offset(j), *b2.offset(j), *b3.offset(j));
                    acc = f32x4_add(acc, f32x4_mul(f32x4_splat(*ar.offset(j)), w));
                }
                v128_store(c.offset(r*n + col) as *mut v128, acc);
            }
        } else {
            for i in 0..span {
                let bc = b.offset(i*k);
                for r in 0..m {
                    let ar = a.offset(r*k);
                    let mut sum = 0.0f32;
                    for j in 0..k { sum += *ar.offset(j) * *bc.offset(j); }
                    *c.offset(r*n + col + i) = sum;
                }
            }
        }
    }

    #[no_mangle]
    pub unsafe extern "C" fn sgd_update(p: *const f32, d: *const f32, o: *mut f32, n: i32, lr: f32) {
        let (p, d, o) = (s(p, n), s(d, n), sm(o, n));
        for i in 0..o.len() { o[i] = p[i] - lr * d[i]; }
    }

    /// `w = 1 - dampening`, rounded by the host.
    #[no_mangle]
    pub unsafe extern "C" fn momentum_update(buf: *const f32, g: *const f32, o: *mut f32, n: i32, mu: f32, w: f32) {
        let (buf, g, o) = (s(buf, n), s(g, n), sm(o, n));
        for i in 0..o.len() { o[i] = mu * buf[i] + w * g[i]; }
    }

    /// `torch.lerp(m, g, w)` with `w = 1 - beta1`, in the branch PyTorch selects for w.
    #[no_mangle]
    pub unsafe extern "C" fn adam_m(m: *const f32, g: *const f32, o: *mut f32, n: i32, w: f32) {
        let (m, g, o) = (s(m, n), s(g, n), sm(o, n));
        if w < 0.5 {
            for i in 0..o.len() { o[i] = m[i] + w * (g[i] - m[i]); }
        } else {
            let v = 1.0 - w;
            for i in 0..o.len() { o[i] = g[i] - (g[i] - m[i]) * v; }
        }
    }

    #[no_mangle]
    pub unsafe extern "C" fn adam_v(v: *const f32, g: *const f32, o: *mut f32, n: i32, beta: f32, w: f32) {
        let (v, g, o) = (s(v, n), s(g, n), sm(o, n));
        for i in 0..o.len() { o[i] = v[i] * beta + w * g[i] * g[i]; }
    }

    /// `p - step_size * m / (sqrt(v) / sqrt(1 - beta2^t) + eps)`, PyTorch's Adam order.
    #[no_mangle]
    pub unsafe extern "C" fn adam_update(p: *const f32, m: *const f32, v: *const f32, o: *mut f32,
        n: i32, step: f32, bc: f32, eps: f32) {
        let (p, m, v, o) = (s(p, n), s(m, n), s(v, n), sm(o, n));
        for i in 0..o.len() { o[i] = p[i] - step * (m[i] / (sqrtf(v[i]) / bc + eps)); }
    }

    #[no_mangle]
    pub unsafe extern "C" fn life(a: *const f32, o: *mut f32, h: i32, w: i32) {
        for y in 0..h { for x in 0..w {
            let mut count = 0;
            for dy in -1..=1 { for dx in -1..=1 {
                if dx != 0 || dy != 0 {
                    let yy = (y + dy + h) % h;
                    let xx = (x + dx + w) % w;
                    if *a.offset((yy*w + xx) as isize) > 0.5 { count += 1; }
                }
            }}
            let alive = *a.offset((y*w + x) as isize) > 0.5;
            *o.offset((y*w + x) as isize) = if count == 3 || (count == 2 && alive) { 1.0 } else { 0.0 };
        }}
    }
}
