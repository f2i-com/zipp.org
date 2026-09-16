/* Freestanding float32 kernels. No libc, Python runtime or external imports.
 *
 * Built with -msimd128 -ffp-contract=off: every product and sum rounds to
 * float32 separately, and each output accumulates in index order, so matrix
 * products and axis reductions match the JavaScript reference bit for bit.
 * There is no libm here: exp, log, tanh and erf are evaluated in double below
 * and rounded once, which agrees with Math.fround(Math.exp(x)) to ~1 ulp. */
#include <wasm_simd128.h>
#define EXPORT(name) __attribute__((export_name(#name)))
#define R __restrict

typedef union { double d; unsigned long long u; } bits64;
#define INF_D __builtin_inf()
#define NAN_D __builtin_nan("")
#define LN2_HI 6.93147180369123816490e-01
#define LN2_LO 1.90821492927058770002e-10

static double scale2(double x, int k) {
  bits64 v;
  while (k > 1023) { x *= 8.98846567431157953865e+307; k -= 1023; }
  while (k < -1022) { x *= 2.22507385850720138309e-308; k += 1022; }
  v.u = (unsigned long long)(k + 1023) << 52;
  return x * v.d;
}
static double exp_d(double x) {
  if (x != x) return x;
  if (x > 710.0) return INF_D;
  if (x < -746.0) return 0.0;
  double kd = __builtin_rint(x * 1.44269504088896338700e+00);
  double r = (x - kd * LN2_HI) - kd * LN2_LO;
  double p = 1.0 + r*(1.0 + r*(1.0/2 + r*(1.0/6 + r*(1.0/24 + r*(1.0/120 + r*(1.0/720 + r*(1.0/5040 + r*(1.0/40320 +
    r*(1.0/362880 + r*(1.0/3628800 + r*(1.0/39916800 + r*(1.0/479001600 + r*(1.0/6227020800.0)))))))))))));
  return scale2(p, (int)kd);
}
static double log_d(double x) {
  if (x != x || x < 0) return NAN_D;
  if (x == 0) return -INF_D;
  if (x == INF_D) return INF_D;
  bits64 v; v.d = x;
  int e = (int)(v.u >> 52) - 1023;
  if (e == -1023) { v.d = x * 18014398509481984.0; e = (int)(v.u >> 52) - 1023 - 54; }
  v.u = (v.u & 0x000fffffffffffffULL) | 0x3ff0000000000000ULL;
  double m = v.d;
  if (m > 1.41421356237309504880) { m *= 0.5; e++; }
  double f = m - 1.0, s = f / (2.0 + f), z = s * s;
  double p = z*(1.0/3 + z*(1.0/5 + z*(1.0/7 + z*(1.0/9 + z*(1.0/11 + z*(1.0/13 + z*(1.0/15 + z*(1.0/17 + z*(1.0/19 + z*(1.0/21))))))))));
  return e * LN2_HI + (2.0*s + (2.0*s*p + e * LN2_LO));
}
static double tanh_d(double x) {
  if (x != x) return x;
  double a = x < 0 ? -x : x, r;
  if (a < 0.01) { double z = x*x; return x * (1.0 + z*(-1.0/3 + z*(2.0/15 + z*(-17.0/315)))); }
  if (a > 20.0) r = 1.0; else { double t = exp_d(-2.0*a); r = (1.0 - t) / (1.0 + t); }
  return x < 0 ? -r : r;
}
/* The shared erf-based normal CDF (see src/kernel-math.mjs). */
static double erf_series(double z) {
  double t = z * z;
  return z * (1.1283791670955126 + t * (-0.37612638903183754 + t * (0.11283791670955126 + t * (-0.026866170645131252 +
    t * (0.005223977625442188 + t * (-0.0008548327023450852 + t * 0.00012055332981789664))))));
}
static double erfc_fit(double a) {
  double t = 1.0 / (1.0 + 0.5 * a);
  return t * exp_d(-a * a - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 +
    t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))));
}
static double phi_d(double x) {
  double z = x * 0.70710678118654752440, a = z < 0 ? -z : z, c;
  if (a < 0.5) return 0.5 + 0.5 * erf_series(z);
  if (z >= 10.0) return 1.0;
  if (z <= -10.0) return 0.0;
  c = 0.5 * erfc_fit(a);
  return z > 0 ? 1.0 - c : c;
}
static double sigmoid_d(double x) {
  if (x >= 0) return 1.0 / (1.0 + exp_d(-x));
  double e = exp_d(x); return e / (1.0 + e);
}
static double gelu_d(double x) { return x * phi_d(x); }
static double gelu_grad_d(double x) { return phi_d(x) + x * 0.3989422804014327 * exp_d(-0.5 * x * x); }

EXPORT(fill) void fill(float *R o, int n, float value) { for (int i = 0; i < n; i++) o[i] = value; }

/* op: 0 add, 1 sub, 2 mul, 3 div. mode: 0 equal shapes, 1 scalar a, 2 scalar b. */
#define BIN(OP) \
  if (mode == 0) { for (int i = 0; i < n; i++) o[i] = a[i] OP b[i]; } \
  else if (mode == 1) { float x = a[0]; for (int i = 0; i < n; i++) o[i] = x OP b[i]; } \
  else { float y = b[0]; for (int i = 0; i < n; i++) o[i] = a[i] OP y; }
EXPORT(binary) void binary(const float *R a, const float *R b, float *R o, int n, int mode, int op) {
  if (op == 0) { BIN(+) } else if (op == 1) { BIN(-) } else if (op == 2) { BIN(*) } else { BIN(/) }
}
/* NumPy broadcasting over four padded dimensions; a stride of 0 repeats an operand. */
#define STRIDED(OP) for (int x3 = 0; x3 < d3; x3++) o[i + x3] = pa[x3*a3] OP pb[x3*b3];
EXPORT(binary_strided) void binary_strided(const float *R a, const float *R b, float *R o, int op,
    int d0, int d1, int d2, int d3, int a0, int a1, int a2, int a3, int b0, int b1, int b2, int b3) {
  int i = 0;
  for (int x0 = 0; x0 < d0; x0++) for (int x1 = 0; x1 < d1; x1++) for (int x2 = 0; x2 < d2; x2++, i += d3) {
    const float *pa = a + x0*a0 + x1*a1 + x2*a2, *pb = b + x0*b0 + x1*b1 + x2*b2;
    if (op == 0) { STRIDED(+) } else if (op == 1) { STRIDED(-) } else if (op == 2) { STRIDED(*) } else { STRIDED(/) }
  }
}
/* Permutations as a strided gather. */
EXPORT(gather4) void gather4(const float *R a, float *R o, int d0, int d1, int d2, int d3, int s0, int s1, int s2, int s3) {
  int i = 0;
  for (int x0 = 0; x0 < d0; x0++) for (int x1 = 0; x1 < d1; x1++) for (int x2 = 0; x2 < d2; x2++) {
    const float *p = a + x0*s0 + x1*s1 + x2*s2;
    for (int x3 = 0; x3 < d3; x3++) o[i++] = p[x3*s3];
  }
}
/* op: 0 relu (NaN kept), 1 positive, 2 neg, 3 exp, 4 log, 5 sqrt, 6 tanh, 7 sigmoid, 8 gelu, 9 gelu_grad. */
EXPORT(unary) void unary(const float *R a, float *R o, int n, int op) {
  switch (op) {
    case 0: for (int i = 0; i < n; i++) { float x = a[i]; o[i] = (x > 0 || x != x) ? x : 0; } break;
    case 1: for (int i = 0; i < n; i++) o[i] = a[i] > 0 ? 1 : 0; break;
    case 2: for (int i = 0; i < n; i++) o[i] = -a[i]; break;
    case 3: for (int i = 0; i < n; i++) o[i] = (float)exp_d(a[i]); break;
    case 4: for (int i = 0; i < n; i++) o[i] = (float)log_d(a[i]); break;
    case 5: for (int i = 0; i < n; i++) o[i] = __builtin_sqrtf(a[i]); break;
    case 6: for (int i = 0; i < n; i++) o[i] = (float)tanh_d(a[i]); break;
    case 7: for (int i = 0; i < n; i++) o[i] = (float)sigmoid_d(a[i]); break;
    case 8: for (int i = 0; i < n; i++) o[i] = (float)gelu_d(a[i]); break;
    case 9: for (int i = 0; i < n; i++) o[i] = (float)gelu_grad_d(a[i]); break;
  }
}
EXPORT(pair_sum) void pair_sum(const float *R a, float *R o, int n) {
  for (int i = 0; i < (n + 1) / 2; i++) o[i] = a[2*i] + (2*i + 1 < n ? a[2*i+1] : 0);
}
/* Sum (or mean) over one axis of [outer, len, inner], accumulating in axis order. */
EXPORT(reduce_axis) void reduce_axis(const float *R a, float *R o, int outer, int len, int inner, int mean) {
  for (int p = 0; p < outer; p++) {
    float *out = o + p*inner;
    for (int i = 0; i < inner; i++) out[i] = 0;
    for (int j = 0; j < len; j++) { const float *row = a + (p*len + j)*inner; for (int i = 0; i < inner; i++) out[i] += row[i]; }
    if (mean) { float d = (float)len; for (int i = 0; i < inner; i++) out[i] /= d; }
  }
}
/* Row maximum (NaN-propagating, like Math.max) and the float32 sum of exp(x - max). */
static float row_stats(const float *x, int cols, float *sum) {
  float m = x[0], s = 0;
  for (int j = 1; j < cols; j++) { float v = x[j]; m = (v > m || v != v) ? v : m; }
  for (int j = 0; j < cols; j++) s += (float)exp_d(x[j] - m);
  *sum = s; return m;
}
EXPORT(softmax_rows) void softmax_rows(const float *R a, float *R o, int rows, int cols, int logmode) {
  for (int r = 0; r < rows; r++) {
    const float *x = a + r*cols; float *y = o + r*cols, s, m = row_stats(x, cols, &s);
    if (logmode) { float ls = (float)log_d(s); for (int j = 0; j < cols; j++) y[j] = (x[j] - m) - ls; }
    else for (int j = 0; j < cols; j++) y[j] = (float)exp_d(x[j] - m) / s;
  }
}
static int target(float t, int cols) { int c = (int)t; return c < 0 || c >= cols ? 0 : c; }
/* Per-row cross-entropy losses; the host reduces them pairwise and divides by N. */
EXPORT(ce_rows) void ce_rows(const float *R a, const float *R t, float *R o, int rows, int cols) {
  for (int r = 0; r < rows; r++) {
    const float *x = a + r*cols; float s, m = row_stats(x, cols, &s);
    o[r] = (float)log_d(s) - (x[target(t[r], cols)] - m);
  }
}
/* d(mean cross-entropy)/d(logits) = (softmax - onehot) / N. */
EXPORT(ce_grad) void ce_grad(const float *R a, const float *R t, float *R o, int rows, int cols) {
  float n = (float)rows;
  for (int r = 0; r < rows; r++) {
    const float *x = a + r*cols; float *y = o + r*cols, s, m = row_stats(x, cols, &s);
    int c = target(t[r], cols);
    for (int j = 0; j < cols; j++) y[j] = ((float)exp_d(x[j] - m) / s - (j == c ? 1.0f : 0.0f)) / n;
  }
}
/* C[m,n] = A[m,k] @ B[k,n], per batch. A 4x8 register block keeps eight SIMD
 * accumulators live across k; every output still sums its k products in order. */
static void matmul_panel(const float *R A, const float *R B, float *R C, int m, int k, int n) {
  int r = 0;
  for (; r + 4 <= m; r += 4) {
    int c = 0;
    for (; c + 8 <= n; c += 8) {
      v128_t c00 = wasm_f32x4_splat(0), c01 = c00, c10 = c00, c11 = c00, c20 = c00, c21 = c00, c30 = c00, c31 = c00;
      const float *a0 = A + r*k, *a1 = a0 + k, *a2 = a1 + k, *a3 = a2 + k;
      for (int j = 0; j < k; j++) {
        const float *b = B + j*n + c;
        v128_t b0 = wasm_v128_load(b), b1 = wasm_v128_load(b + 4), x;
        x = wasm_f32x4_splat(a0[j]); c00 = wasm_f32x4_add(c00, wasm_f32x4_mul(x, b0)); c01 = wasm_f32x4_add(c01, wasm_f32x4_mul(x, b1));
        x = wasm_f32x4_splat(a1[j]); c10 = wasm_f32x4_add(c10, wasm_f32x4_mul(x, b0)); c11 = wasm_f32x4_add(c11, wasm_f32x4_mul(x, b1));
        x = wasm_f32x4_splat(a2[j]); c20 = wasm_f32x4_add(c20, wasm_f32x4_mul(x, b0)); c21 = wasm_f32x4_add(c21, wasm_f32x4_mul(x, b1));
        x = wasm_f32x4_splat(a3[j]); c30 = wasm_f32x4_add(c30, wasm_f32x4_mul(x, b0)); c31 = wasm_f32x4_add(c31, wasm_f32x4_mul(x, b1));
      }
      float *o = C + r*n + c;
      wasm_v128_store(o, c00); wasm_v128_store(o + 4, c01); o += n;
      wasm_v128_store(o, c10); wasm_v128_store(o + 4, c11); o += n;
      wasm_v128_store(o, c20); wasm_v128_store(o + 4, c21); o += n;
      wasm_v128_store(o, c30); wasm_v128_store(o + 4, c31);
    }
    for (int rr = r; rr < r + 4; rr++) {
      float *o = C + rr*n;
      for (int cc = c; cc < n; cc++) o[cc] = 0;
      for (int j = 0; j < k; j++) { float x = A[rr*k + j]; const float *b = B + j*n; for (int cc = c; cc < n; cc++) o[cc] += x * b[cc]; }
    }
  }
  for (; r < m; r++) {
    float *o = C + r*n;
    for (int cc = 0; cc < n; cc++) o[cc] = 0;
    for (int j = 0; j < k; j++) { float x = A[r*k + j]; const float *b = B + j*n; for (int cc = 0; cc < n; cc++) o[cc] += x * b[cc]; }
  }
}
EXPORT(bmm) void bmm(const float *R a, const float *R b, float *R o, int batch, int m, int k, int n, int sa, int sb) {
  for (int t = 0; t < batch; t++) matmul_panel(a + t*sa, b + t*sb, o + t*m*n, m, k, n);
}
EXPORT(sgd_update) void sgd_update(const float *R p, const float *R d, float *R o, int n, float lr) {
  for (int i = 0; i < n; i++) o[i] = p[i] - lr * d[i];
}
/* w = 1 - dampening, rounded by the host. */
EXPORT(momentum_update) void momentum_update(const float *R buf, const float *R g, float *R o, int n, float mu, float w) {
  for (int i = 0; i < n; i++) o[i] = mu * buf[i] + w * g[i];
}
/* torch.lerp(m, g, w) with w = 1 - beta1, in the branch PyTorch selects for w. */
EXPORT(adam_m) void adam_m(const float *R m, const float *R g, float *R o, int n, float w) {
  if (w < 0.5f) { for (int i = 0; i < n; i++) o[i] = m[i] + w * (g[i] - m[i]); }
  else { float v = 1.0f - w; for (int i = 0; i < n; i++) o[i] = g[i] - (g[i] - m[i]) * v; }
}
EXPORT(adam_v) void adam_v(const float *R v, const float *R g, float *R o, int n, float beta, float w) {
  for (int i = 0; i < n; i++) o[i] = v[i] * beta + w * g[i] * g[i];
}
/* p - step_size * m / (sqrt(v) / sqrt(1 - beta2^t) + eps), PyTorch's Adam order. */
EXPORT(adam_update) void adam_update(const float *R p, const float *R m, const float *R v, float *R o, int n, float step, float bc, float eps) {
  for (int i = 0; i < n; i++) o[i] = p[i] - step * (m[i] / (__builtin_sqrtf(v[i]) / bc + eps));
}
EXPORT(life) void life(const float *R a, float *R o, int h, int w) {
  for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
    int count = 0;
    for (int dy = -1; dy <= 1; dy++) for (int dx = -1; dx <= 1; dx++)
      if (dx || dy) count += a[((y + dy + h) % h)*w + (x + dx + w) % w] > 0.5f;
    o[y*w + x] = (count == 3 || (count == 2 && a[y*w + x] > 0.5f)) ? 1 : 0;
  }
}
