/**
 * Scalar definitions shared by every backend. The WASM, WGSL and GLSL kernels
 * implement these same formulas; this file is their JavaScript statement.
 *
 * erf has no native implementation in JavaScript, WGSL, GLSL ES or freestanding
 * C, so GELU is defined with one approximation everywhere: an odd Taylor series
 * (seven terms) for |z| < 0.5, and Numerical Recipes' erfc Chebyshev fit
 * (fractional error < 1.2e-7) above it. GELU is the exact-erf form
 * x*cdf(x) = 0.5*x*(1 + erf(x/sqrt(2))), not the tanh approximation. cdf takes its lower
 * tail from erfc directly, so tiny negative-side values keep their precision
 * instead of cancelling in 1 + erf.
 */
function erfSeries(z) { // odd series, |z| < 0.5
  const t = z * z;
  return z * (1.1283791670955126 + t * (-0.37612638903183754 + t * (0.11283791670955126 + t * (-0.026866170645131252 +
    t * (0.005223977625442188 + t * (-0.0008548327023450852 + t * 0.00012055332981789664))))));
}
function erfcFit(a) { // a >= 0.5
  const t = 1 / (1 + 0.5 * a);
  return t * Math.exp(-a * a - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 +
    t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))));
}
export function erf(x) {
  const a = Math.abs(x);
  if (a < 0.5) return erfSeries(x);
  const r = a >= 10 ? 1 : 1 - erfcFit(a);
  return x < 0 ? -r : x > 0 ? r : x;
}
/** Standard normal CDF: 0.5*(1 + erf(x/sqrt(2))). */
export function cdf(x) {
  const z = x * Math.SQRT1_2;
  if (Math.abs(z) < 0.5) return 0.5 + 0.5 * erfSeries(z);
  if (z >= 10) return 1;
  if (z <= -10) return 0;
  const c = 0.5 * erfcFit(Math.abs(z));
  return z > 0 ? 1 - c : c;
}
export const gelu = x => x * cdf(x);
/** d/dx gelu(x) = cdf(x) + x * pdf(x), pdf being the standard normal density. */
export const geluGrad = x => cdf(x) + x * 0.3989422804014327 * Math.exp(-0.5 * x * x);
/** Overflow-free logistic function. */
export const sigmoid = x => x >= 0 ? 1 / (1 + Math.exp(-x)) : Math.exp(x) / (1 + Math.exp(x));
/** ReLU keeps NaN, so a diverged value reaches the finite-readback check on every backend. */
export const relu = x => (x > 0 || x !== x) ? x : 0;
