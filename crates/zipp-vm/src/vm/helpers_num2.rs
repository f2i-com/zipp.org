#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, ReactionPair, Reactions,
};
use crate::value::Value;

/// A single-argument `Math.<op>` computation, matching JS where it diverges
/// from Rust (`round` half-up; `sign` preserves ±0 and maps NaN→NaN). The
/// variadic/binary ops never reach here with the real call paths; they fall
/// back to operating on the one value provided.
pub(crate) fn math_unary(op: crate::bytecode::MathFn, x: f64) -> f64 {
    use crate::bytecode::MathFn as M;
    match op {
        M::Abs => x.abs(),
        M::Floor => x.floor(),
        M::Ceil => x.ceil(),
        M::Round => {
            // Spec Math.round: preserve NaN/±Infinity/±0; (0,0.5)→+0, [-0.5,0)→-0
            // (so 1/Math.round(-0.4) is -Infinity); else half-up via floor(x+0.5).
            // The explicit (0,0.5)/[-0.5,0) branches also fix the fp edge where
            // x+0.5 rounds up to 1 for the largest double below 0.5.
            if x.is_nan() || x.is_infinite() || x == 0.0 {
                x
            } else if x > 0.0 && x < 0.5 {
                0.0
            } else if x < 0.0 && x >= -0.5 {
                -0.0
            } else if x.abs() >= 4503599627370496.0 {
                // |x| >= 2^52: every double is already an integer, and x+0.5 would
                // round the magnitude UP to the next representable value, so
                // Math.round(-(2^53-1)) must return x unchanged.
                x
            } else {
                (x + 0.5).floor()
            }
        }
        M::Trunc => x.trunc(),
        M::Sign => {
            if x.is_nan() {
                f64::NAN
            } else if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                x
            }
        }
        M::Sqrt => x.sqrt(),
        M::Cbrt => x.cbrt(),
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
        // Math.clz32: leading zeros of ToUint32(x). Math.fround: round to f32.
        M::Clz32 => to_uint32(x).leading_zeros() as f64,
        M::Fround => x as f32 as f64,
        // Pow/Atan2/Imul/Min/Max/Hypot aren't unary; degrade gracefully.
        M::Min | M::Max => x,
        M::Hypot => x.abs(),
        M::Pow | M::Atan2 | M::Imul => f64::NAN,
    }
}

/// `Math.acosh(x)`.
///
/// Rust's `f64::acosh` is `ln(x + sqrt(x-1)*sqrt(x+1))`, whose argument is
/// `1 + tiny` for x just above 1 — the addition to 1 throws away every bit of
/// `tiny` below 2^-52, so `acosh(1.0000000000000013)` came out
/// 5.1619136520741884e-8 against the correct 5.1619136559035694e-8 (a relative
/// error near 1e-9, orders of magnitude past the 2-ULP the tests allow).
/// Reassociating around `ln_1p` keeps the small term small.
fn acosh(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x < 1.0 {
        return f64::NAN;
    }
    // Beyond 2^28, x*x overflows the useful range and acosh(x) → ln(2x).
    if x >= 268_435_456.0 {
        return x.ln() + std::f64::consts::LN_2;
    }
    if x > 2.0 {
        return (2.0 * x - 1.0 / (x + (x * x - 1.0).sqrt())).ln();
    }
    let t = x - 1.0; // exact for 1 ≤ x ≤ 2 (Sterbenz)
    (t + (2.0 * t + t * t).sqrt()).ln_1p()
}

/// `Math.atanh(x)`.
///
/// Rust's `f64::atanh` applies `0.5 * ln_1p(2x / (1 - x))` to the SIGNED
/// argument. For x near -1 that leaves `1 - x ≈ 2`, so the quotient lands a
/// few ULP from -1 and `ln_1p` is handed a value with almost no significant
/// bits left: `atanh(-0.9999999999999983)` came out -17.395445210310893 instead
/// of -17.36094877456742. Folding the sign FIRST puts the cancelling term in
/// the denominator, where `1 - |x|` is exact.
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
        // Split so that small |x| does not cancel inside the quotient:
        // 2a + 2a²/(1-a) == 2a/(1-a), but the leading term stays exact.
        0.5 * (2.0 * a + 2.0 * a * a / (1.0 - a)).ln_1p()
    } else {
        0.5 * ((a + a) / (1.0 - a)).ln_1p()
    };
    t.copysign(x)
}

/// `Math.f16round(x)`: round `x` to the nearest IEEE-754 binary16 (half) value
/// (round-ties-to-even), returned as an f64. NaN/±0/±Infinity pass through;
/// magnitudes at/above the round-to-infinity threshold (65520) become ±Infinity.
pub(crate) fn f16_round(x: f64) -> f64 {
    if !x.is_finite() {
        return x; // NaN, ±Infinity
    }
    if x == 0.0 {
        return x; // preserves ±0
    }
    let neg = x.is_sign_negative();
    let a = x.abs();
    // Largest finite half is 65504; the round-to-nearest-even tie point to infinity
    // is 65520 (midpoint between 65504 and 2^16).
    if a >= 65520.0 {
        return if neg { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    // Determine the binade exponent e with 2^e <= a < 2^(e+1) (robust to log2 noise).
    let mut e = a.log2().floor() as i32;
    while (e as f64).exp2() > a {
        e -= 1;
    }
    while ((e + 1) as f64).exp2() <= a {
        e += 1;
    }
    // Half has a 10-bit fraction → ulp = 2^(e-10) for normals; subnormals (e < -14)
    // share the fixed ulp 2^-24.
    let ulp_exp = if e < -14 { -24 } else { e - 10 };
    let ulp = (ulp_exp as f64).exp2();
    let rounded = (a / ulp).round_ties_even() * ulp;
    if neg {
        -rounded
    } else {
        rounded
    }
}

/// Decode an IEEE-754 binary16 (half) bit pattern to an f64 (exact — every half
/// value is representable as an f64). Drives `DataView.prototype.getFloat16`.
pub(crate) fn f16_bits_to_f64(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (bits >> 10) & 0x1f;
    let mant = (bits & 0x3ff) as f64;
    match exp {
        0 => sign * mant * 2f64.powi(-24),            // subnormal (±0 when mant == 0)
        0x1f if mant == 0.0 => sign * f64::INFINITY,  // ±Infinity
        0x1f => f64::NAN,                             // NaN
        _ => sign * (1.0 + mant / 1024.0) * 2f64.powi((exp as i32) - 15),
    }
}

/// Encode an f64 as the nearest IEEE-754 binary16 (half) bit pattern, rounding
/// ties to even (overflow → ±Infinity, NaN → a canonical quiet NaN). Drives
/// `DataView.prototype.setFloat16`.
pub(crate) fn f64_to_f16_bits(f: f64) -> u16 {
    if f.is_nan() {
        return 0x7e00;
    }
    let sign: u16 = if f.is_sign_negative() { 0x8000 } else { 0 };
    let a = f.abs();
    if a.is_infinite() || a >= 65520.0 {
        return sign | 0x7c00; // ±Infinity (65520 is the round-to-Inf tie point)
    }
    if a == 0.0 {
        return sign;
    }
    let bits = a.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i32 - 1023; // unbiased binary exponent
    let m = bits & 0x000f_ffff_ffff_ffff; // 52-bit trailing significand
    if e < -14 {
        // Subnormal half: round (a / 2^-24) to a nearest even integer mantissa.
        // a = (2^52 | m) * 2^(e-52); divide by 2^-24 → shift right by (28 - e).
        let full = (1u64 << 52) | m;
        let shift = (28 - e) as u32;
        let rounded = round_shift_u64(full, shift);
        // `rounded == 1024` naturally becomes the smallest NORMAL (exp 1, mant 0).
        return sign | (rounded as u16);
    }
    // Normal half: round the 52-bit significand to 10 bits (ties to even).
    let f16_exp = (e + 15) as u16; // 1..=30 here (a < 65520 ⇒ no exp-15 overflow)
    let rounded_mant = round_shift_u64(m, 42);
    if rounded_mant == 1024 {
        // Mantissa carried out → bump the exponent (never reaches Inf for a<65520).
        return sign | ((f16_exp + 1) << 10);
    }
    sign | (f16_exp << 10) | (rounded_mant as u16)
}

/// `val >> shift`, rounded to nearest with ties to even.
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

/// `Math.sumPrecise(numbers)`: the correctly-rounded sum of the finite f64s, via
/// Shewchuk's non-overlapping-partials algorithm. NaN if any input is NaN or if
/// both +∞ and −∞ appear; otherwise the lone infinity if present. The empty sum
/// (and an all-`-0` sum) is −0.
pub(crate) fn sum_precise(nums: &[f64]) -> f64 {
    let mut has_pos_inf = false;
    let mut has_neg_inf = false;
    let mut all_neg_zero = true;
    for &x in nums {
        if x.is_nan() {
            return f64::NAN;
        }
        if x.is_infinite() {
            if x > 0.0 {
                has_pos_inf = true;
            } else {
                has_neg_inf = true;
            }
        }
        // A value that is not negative zero makes the (possible) zero result +0.
        if !(x == 0.0 && x.is_sign_negative()) {
            all_neg_zero = false;
        }
    }
    if has_pos_inf && has_neg_inf {
        return f64::NAN;
    }
    if has_pos_inf {
        return f64::INFINITY;
    }
    if has_neg_inf {
        return f64::NEG_INFINITY;
    }
    // Exact fixed-point accumulation: every finite f64 is ±m·2^E with m < 2^53
    // and E ∈ [-1074, 971], so the exact sum of ANY number of them fits a
    // ~2100-bit signed fixed-point accumulator (bit p holds weight 2^(p−1074)).
    // Per-limb i128s defer carries (2^63 elements of headroom). The value is
    // rounded ONCE at the end (to nearest, ties to even), so the result is
    // correct even where intermediate f64 partial sums would overflow:
    // MAX + MAX − tiny must round DOWN to MAX_F64, not blow up to Infinity.
    const LIMBS: usize = 36; // 2304 bits ≥ top weight 2^1023 ⇒ bit 2097, + headroom
    let mut acc = [0i128; LIMBS];
    for &x in nums {
        if x == 0.0 {
            continue;
        }
        let bits = x.to_bits();
        let neg = bits >> 63 != 0;
        let exp_field = ((bits >> 52) & 0x7ff) as i32;
        let frac = bits & ((1u64 << 52) - 1);
        let (m, e) = if exp_field == 0 {
            (frac, -1074) // subnormal
        } else {
            (frac | (1u64 << 52), exp_field - 1075)
        };
        let pos = (e + 1074) as usize;
        let (idx, sh) = (pos / 64, pos % 64);
        let wide = (m as u128) << sh; // ≤ 116 bits — spans two limbs
        let (lo, hi) = (wide as u64, (wide >> 64) as u64);
        if neg {
            acc[idx] -= lo as i128;
            acc[idx + 1] -= hi as i128;
        } else {
            acc[idx] += lo as i128;
            acc[idx + 1] += hi as i128;
        }
    }
    // Carry-normalize into a base-2^64 magnitude + sign. A final carry of −1
    // means the whole number is negative — two's-complement it back.
    let mut limbs = [0u64; LIMBS];
    let mut carry: i128 = 0;
    for i in 0..LIMBS {
        let total = acc[i] + carry;
        let rem = (total & 0xFFFF_FFFF_FFFF_FFFF_i128) as u64; // total mod 2^64 ∈ [0, 2^64)
        carry = (total - rem as i128) >> 64;
        limbs[i] = rem;
    }
    let negative = carry < 0;
    if negative {
        let mut c = 1u64;
        for l in limbs.iter_mut() {
            let (s, o) = (!*l).overflowing_add(c);
            *l = s;
            c = u64::from(o);
        }
    }
    // Top set bit ⇒ the result's binade; round at its 53-bit ULP (clamped to
    // bit 0 — everything at or below 2^-1074 is exact in a subnormal result).
    let mut top: i64 = -1;
    for i in (0..LIMBS).rev() {
        if limbs[i] != 0 {
            top = (i as i64) * 64 + (63 - limbs[i].leading_zeros() as i64);
            break;
        }
    }
    if top < 0 {
        return if all_neg_zero { -0.0 } else { 0.0 };
    }
    let ulp = (top - 52).max(0); // bit position of the result's lowest kept bit
    let bit_at = |p: i64| (limbs[(p / 64) as usize] >> (p % 64)) & 1 == 1;
    // The 53 (or fewer) kept bits, as an integer.
    let q0 = {
        let (idx, sh) = ((ulp / 64) as usize, (ulp % 64) as u32);
        let lo = limbs[idx] >> sh;
        let hi = if sh > 0 && idx + 1 < LIMBS { limbs[idx + 1] << (64 - sh) } else { 0 };
        lo | hi
    };
    let round = ulp > 0 && bit_at(ulp - 1);
    let sticky = ulp > 1 && {
        let (idx, sh) = (((ulp - 1) / 64) as usize, ((ulp - 1) % 64) as u32);
        limbs[..idx].iter().any(|&l| l != 0) || (limbs[idx] & ((1u64 << sh) - 1)) != 0
    };
    let q = if round && (sticky || q0 & 1 == 1) { q0 + 1 } else { q0 };
    // Exact power of two for the scale; q·2^(ulp−1074) is exact (≤ 53
    // significant bits landing on representable weights), and a magnitude at
    // or beyond 2^1024 overflows to Infinity in the multiply per IEEE 754 —
    // which IS the correctly-rounded answer there.
    let k = (ulp - 1074) as i32;
    let scale = if k >= -1022 {
        if k <= 1023 {
            f64::from_bits(((k + 1023) as u64) << 52)
        } else {
            f64::INFINITY
        }
    } else {
        f64::from_bits(1u64 << (k + 1074)) // subnormal power of two
    };
    let mag = (q as f64) * scale;
    if negative {
        -mag
    } else {
        mag
    }
}

/// `Number.isInteger`: a number with no fractional part (no coercion).
pub(crate) fn num_is_integer(v: Value) -> bool {
    if v.is_int() {
        true
    } else if v.is_double() {
        let n = v.as_f64();
        n.is_finite() && n.fract() == 0.0
    } else {
        false
    }
}

/// `Number.isFinite`: a finite number (no coercion).
pub(crate) fn num_is_finite(v: Value) -> bool {
    v.is_int() || (v.is_double() && v.as_f64().is_finite())
}

/// `Number.isSafeInteger`: an integer within ±(2^53 − 1).
pub(crate) fn num_is_safe_integer(v: Value) -> bool {
    num_is_integer(v) && {
        let n = if v.is_int() { v.as_int() as f64 } else { v.as_f64() };
        n.abs() <= 9_007_199_254_740_991.0
    }
}

/// `Number.prototype.toString(radix)` for `radix` in 2..=36. Renders the integer
/// part in the given base (matching JS for whole numbers; a fractional part is
/// truncated — full fractional-radix rendering is out of the subset). NaN and
/// ±Infinity render via the canonical path (handled by the caller for radix 10).
// ── Date helpers (proleptic Gregorian, UTC; Howard Hinnant's algorithms) ──

/// Days since 1970-01-01 for (year, month 1..=12, day) — `day` may be out of
/// [1,31] and is carried linearly (so day 0 = the prior day), matching JS's
/// field normalization.
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// (year, month 1..=12, day) from days since 1970-01-01.
pub(crate) fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Break epoch ms into UTC parts: (year, month0, day, hour, min, sec, ms,
/// weekday 0=Sun..6=Sat). Uses floored division so negative ms work.
pub(crate) fn date_parts(ms: f64) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
    let total = ms.floor() as i64;
    let day = total.div_euclid(86_400_000);
    let rem = total.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(day);
    let h = rem / 3_600_000;
    let mi = (rem / 60_000) % 60;
    let s = (rem / 1000) % 60;
    let mss = rem % 1000;
    let wd = (day.rem_euclid(7) + 4) % 7; // 1970-01-01 was a Thursday (4)
    (y, m - 1, d, h, mi, s, mss, wd)
}

/// Epoch ms from UTC components (month0-based; out-of-range fields normalized
/// like JS). NOTE: the legacy 2-digit-year→19xx mapping is applied by the numeric
/// CONSTRUCTORS (`Date.UTC`, `new Date(y,m,…)`), NOT here — ISO string parsing
/// must take the year literally (year 1 = 1, not 1901).
pub(crate) fn ms_from_utc(y: i64, mo0: i64, d: i64, h: i64, mi: i64, s: i64, ms: i64) -> f64 {
    let year = y + mo0.div_euclid(12);
    let month = mo0.rem_euclid(12); // 0-based → 1-based below
    let days = days_from_civil(year, month + 1, d);
    days as f64 * 86_400_000.0
        + h as f64 * 3_600_000.0
        + mi as f64 * 60_000.0
        + s as f64 * 1000.0
        + ms as f64
}

/// The legacy 2-digit-year mapping for the Number-domain constructor path.
pub(crate) fn legacy_year_f64(y: f64) -> f64 {
    if (0.0..=99.0).contains(&y) {
        1900.0 + y
    } else {
        y
    }
}

/// Epoch ms from UTC components as the SPEC's MakeDay/MakeTime/MakeDate over
/// f64 NUMBERS — with the spec's exact IEEE-754 association, so huge component
/// values round/overflow exactly like `day × msPerDay + time` requires
/// (`Date.UTC(1970, 0, 213503982336, 0, 0, 0, -18446744073709552000)` is the
/// FINITE 34447360 — the two ~1.8e19 terms cancel only if each is rounded the
/// way the spec's operation order produces). Components must be ToInteger'd
/// (truncated) finite Numbers; a year too large for the i64 civil-day math is
/// NaN (it would TimeClip to NaN regardless).
pub(crate) fn ms_from_utc_f64(y: f64, mo0: f64, d: f64, h: f64, mi: f64, s: f64, ms: f64) -> f64 {
    // MakeDay: ym = y + floor(m / 12), month = m modulo 12.
    let ym = y + (mo0 / 12.0).floor();
    let mn = mo0.rem_euclid(12.0);
    if !ym.is_finite() || ym.abs() > 1.0e9 {
        return f64::NAN;
    }
    let day = days_from_civil(ym as i64, mn as i64 + 1, 1) as f64 + d - 1.0;
    // MakeTime: ((h·msPerHour + m·msPerMinute) + s·msPerSecond) + milli.
    let time = ((h * 3_600_000.0 + mi * 60_000.0) + s * 1_000.0) + ms;
    // MakeDate: day × msPerDay + time.
    day * 86_400_000.0 + time
}

/// JS TimeClip: NaN if non-finite or |t| > 8.64e15 (±100M days); else truncate
/// toward zero to an integer millisecond.
pub(crate) fn time_clip(n: f64) -> f64 {
    if !n.is_finite() || n.abs() > 8.64e15 {
        f64::NAN
    } else {
        // Spec step 3 is `ToInteger(time) + (+0)`: the `+ 0.0` exists to normalize a
        // truncated -0 to +0 (Rust's (-0.0).trunc() preserves the negative-zero bit).
        n.trunc() + 0.0
    }
}

/// `toISOString` form: `YYYY-MM-DDTHH:mm:ss.sssZ` (±YYYYYY outside 0..=9999).
pub(crate) fn date_to_iso(ms: f64) -> String {
    let (y, mo0, d, h, mi, s, mss, _) = date_parts(ms);
    if (0..=9999).contains(&y) {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo0 + 1, d, h, mi, s, mss)
    } else {
        let sign = if y < 0 { '-' } else { '+' };
        format!("{}{:06}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", sign, y.abs(), mo0 + 1, d, h, mi, s, mss)
    }
}

/// Abbreviated weekday names, `date_parts` weekday order (0 = Sunday).
const WEEKDAY: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
/// Abbreviated month names, 0-based (`date_parts` month0 order).
const MONTH: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A year zero-padded to at least 4 digits, a negative year sign-prefixed
/// (`-1` → `-0001`, `20` → `0020`, `2014` → `2014`) — the year field shared by the
/// `Date.prototype.to{,UTC,Date}String` forms.
fn fmt_year(y: i64) -> String {
    if y < 0 {
        format!("-{:04}", -y)
    } else {
        format!("{:04}", y)
    }
}

/// `Date.prototype.toDateString`: `"Thu Jan 01 1970"` (Weekday Mon DD YYYY, UTC).
pub(crate) fn date_to_date_string(ms: f64) -> String {
    let (y, mo0, d, _, _, _, _, wd) = date_parts(ms);
    format!("{} {} {:02} {}", WEEKDAY[wd as usize], MONTH[mo0 as usize], d, fmt_year(y))
}

/// `Date.prototype.toTimeString`: `"00:00:00 GMT+0000"`. The engine is UTC-only, so
/// the zone is always `GMT+0000` and the optional `(Zone Name)` suffix is omitted.
pub(crate) fn date_to_time_string(ms: f64) -> String {
    let (_, _, _, h, mi, s, _, _) = date_parts(ms);
    format!("{:02}:{:02}:{:02} GMT+0000", h, mi, s)
}

/// `Date.prototype.toString`: DateString + " " + TimeString + TimeZone, e.g.
/// `"Thu Jan 01 1970 00:00:00 GMT+0000"`. `"Invalid Date"` for a NaN time.
pub(crate) fn date_to_string(ms: f64) -> String {
    if ms.is_nan() {
        return "Invalid Date".to_string();
    }
    format!("{} {}", date_to_date_string(ms), date_to_time_string(ms))
}

/// `Date.prototype.toUTCString` (and its legacy `toGMTString` alias):
/// `"Thu, 01 Jan 1970 00:00:00 GMT"` (Weekday, DD Mon YYYY HH:MM:SS GMT).
pub(crate) fn date_to_utc_string(ms: f64) -> String {
    let (y, mo0, d, h, mi, s, _, wd) = date_parts(ms);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WEEKDAY[wd as usize], d, MONTH[mo0 as usize], fmt_year(y), h, mi, s
    )
}

/// Date-string parsing, in the two layers every engine has.
///
/// 21.4.3.2 `Date.parse` first tries the **Date Time String Format** of
/// 21.4.1.20 — the only shape the spec pins down — and, failing that, falls back
/// to an implementation-defined heuristic. Folding the two into one permissive
/// scan (what this used to be) makes the strict form accept things it must
/// reject: `1997-3-8T11:19:20`, `1997-03-08T1:1`, `1997-03-08T` and
/// `1997-03-08T11:19:10-07` are all NaN in V8 and SpiderMonkey precisely
/// because a `T` selects the strict grammar, while the SAME fields after a
/// SPACE reach the legacy parser and are accepted
/// (staging/sm/Date/non-iso.js).
///
/// `parse_iso_date_time` is the strict layer; `parse_legacy_date` is the
/// heuristic one, modelled on V8's `DateParser` (`src/date/dateparser*`) because
/// that is the behaviour test262's staging tests were written against.
pub(crate) fn parse_date(s: &str) -> f64 {
    let s = s.trim();
    if let Some(t) = parse_iso_date_time(s) {
        return time_clip(t);
    }
    time_clip(parse_legacy_date(s))
}

// ---- strict: the Date Time String Format (21.4.1.20) -----------------------

/// Read exactly `n` ASCII digits at `i`, advancing `i`.
fn take_digits(b: &[u8], i: &mut usize, n: usize) -> Option<i64> {
    if *i + n > b.len() {
        return None;
    }
    let mut v: i64 = 0;
    for k in 0..n {
        let c = b[*i + k];
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as i64;
    }
    *i += n;
    Some(v)
}

/// `YYYY-MM-DDTHH:mm:ss.sssZ` and its documented subsets, with every field
/// range-checked. Returns `None` (not NaN) when the string is not in this
/// format at all, so the caller can try the legacy parser; a string that IS in
/// this format but carries an out-of-range field returns `Some(NaN)` — it must
/// not fall through and be re-read by the looser grammar.
fn parse_iso_date_time(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0usize;
    // `YYYY`, or an expanded `±YYYYYY`. A negative zero year is not a year.
    let year = if b.first() == Some(&b'+') || b.first() == Some(&b'-') {
        let neg = b[0] == b'-';
        i = 1;
        let y = take_digits(b, &mut i, 6)?;
        if neg && y == 0 {
            return Some(f64::NAN);
        }
        if neg {
            -y
        } else {
            y
        }
    } else {
        take_digits(b, &mut i, 4)?
    };
    let mut month = 1i64;
    let mut day = 1i64;
    if b.get(i) == Some(&b'-') {
        i += 1;
        month = take_digits(b, &mut i, 2)?;
        if b.get(i) == Some(&b'-') {
            i += 1;
            day = take_digits(b, &mut i, 2)?;
        }
    }
    let (mut h, mut mi, mut sec, mut ms) = (0i64, 0i64, 0i64, 0i64);
    let mut has_time = false;
    // A lowercase `t`/`z` is not in the grammar, but every engine accepts it and
    // real-world data carries it, so the STRICT layer takes both spellings. The
    // legacy layer still rejects `t` outright, which is what keeps
    // `1997-3-8T11:19:20` (one-digit month, so not the strict format) NaN.
    if b.get(i) == Some(&b'T') || b.get(i) == Some(&b't') {
        has_time = true;
        i += 1;
        h = take_digits(b, &mut i, 2)?;
        if b.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        mi = take_digits(b, &mut i, 2)?;
        if b.get(i) == Some(&b':') {
            i += 1;
            sec = take_digits(b, &mut i, 2)?;
            if b.get(i) == Some(&b'.') {
                i += 1;
                // The grammar spells exactly three digits, but every engine
                // accepts any number of them (and `toJSON` round-trips are not
                // the only producers) — take the first three, ignore the rest.
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                let frac = &s[start..i];
                let mut scaled = String::with_capacity(3);
                scaled.push_str(&frac[..frac.len().min(3)]);
                while scaled.len() < 3 {
                    scaled.push('0');
                }
                ms = scaled.parse::<i64>().ok()?;
            }
        }
    }
    // `Z`, or `±HH:mm`. Absent: a date-only form is UTC and a date-time is local
    // time — identical here, since this engine's local time zone IS UTC
    // (`getTimezoneOffset` is 0 unconditionally).
    let mut offset_min = 0i64;
    let mut has_zone = false;
    if i < b.len() {
        has_zone = true;
        match b[i] {
            b'Z' | b'z' => {
                i += 1;
            }
            b'+' | b'-' => {
                let sign = if b[i] == b'-' { -1 } else { 1 };
                i += 1;
                let oh = take_digits(b, &mut i, 2)?;
                // `±HH:mm`, or the colon-less `±HHmm` every engine also takes.
                // An hours-ONLY `±HH` is not accepted here: `1997-03-08T11:19:10-07`
                // must be NaN (staging/sm/Date/non-iso.js), and only the legacy
                // layer — reached through a SPACE separator — reads that form.
                let om = if b.get(i) == Some(&b':') {
                    i += 1;
                    take_digits(b, &mut i, 2)?
                } else {
                    take_digits(b, &mut i, 2)?
                };
                if oh > 23 || om > 59 {
                    return Some(f64::NAN);
                }
                offset_min = sign * (oh * 60 + om);
            }
            _ => return None,
        }
    }
    if i != b.len() {
        return None;
    }
    // `DateTimeUTCOffset` only follows a `TimeSpec`, so a zone with no time
    // (`1970-01-01Z`) is not in the format.
    if has_zone && !has_time {
        return None;
    }
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || h > 24
        || mi > 59
        || sec > 59
        || (h == 24 && (mi != 0 || sec != 0 || ms != 0))
    {
        return Some(f64::NAN);
    }
    Some(ms_from_utc(year, month - 1, day, h, mi, sec, ms) - offset_min as f64 * 60_000.0)
}

// ---- heuristic: the legacy forms (V8's DateParser) -------------------------

fn is_day_num(n: i64) -> bool {
    (1..=31).contains(&n)
}

/// Everything the Date Time String Format does not cover: `toString` /
/// `toUTCString` output (`Thu Jan 01 1970 00:00:00 GMT+0000`), the
/// space-separated relaxed ISO forms (`1997-3-8 11:19:20`), and the US
/// `M/D/Y` + month-name orderings (`12/25/1995`, `may 1 1999`, `1 1999 may`).
///
/// Ordering and the two-digit-year window follow V8's `DayComposer::Write`:
/// three bare numbers are `Y/M/D` when the first cannot be a day-of-month and
/// `M/D/Y` otherwise; with a month NAME the first number is the day when it can
/// be one and the year otherwise; and a year of 0–49 means 20xx, 50–99 means
/// 19xx. staging/sm/Date/two-digit-years.js asserts exactly that, across
/// 100 × 12 × 31 dates plus 1000 written-month ones.
fn parse_legacy_date(s: &str) -> f64 {
    let b = s.as_bytes();
    let mut i = 0usize;
    // Up to three bare numbers (a fourth is a parse failure, which is what makes
    // `1997-03-08 11` — year, month, day, then a stray hour — NaN).
    let mut nums: Vec<i64> = Vec::new();
    let mut named_month: Option<i64> = None;
    let mut time: Option<(i64, i64, i64, i64)> = None;
    let mut pm: Option<bool> = None;
    let mut tz_utc = false;
    let mut offset_min: Option<i64> = None;

    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() || c == b',' {
            i += 1;
            continue;
        }
        // `(…)` is a comment — the `(Australian Eastern Daylight Time)` tail of
        // a `toString`. Nesting counts, as in V8.
        if c == b'(' {
            let mut depth = 0usize;
            while i < b.len() {
                if b[i] == b'(' {
                    depth += 1;
                } else if b[i] == b')' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let n: i64 = match s[start..i].parse() {
                Ok(v) => v,
                Err(_) => return f64::NAN, // absurdly long digit run
            };
            if b.get(i) == Some(&b':') {
                // A time: HH:mm[:ss[.sss]].
                if time.is_some() {
                    return f64::NAN;
                }
                i += 1;
                let m = match read_uint(b, &mut i) {
                    Some(v) => v,
                    None => return f64::NAN,
                };
                let mut sec = 0i64;
                let mut ms = 0i64;
                if b.get(i) == Some(&b':') {
                    i += 1;
                    sec = match read_uint(b, &mut i) {
                        Some(v) => v,
                        None => return f64::NAN,
                    };
                }
                if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                    i += 1;
                    let fs = i;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    let frac = &s[fs..i];
                    let mut scaled = String::with_capacity(3);
                    scaled.push_str(&frac[..frac.len().min(3)]);
                    while scaled.len() < 3 {
                        scaled.push('0');
                    }
                    ms = scaled.parse::<i64>().unwrap_or(0);
                }
                time = Some((n, m, sec, ms));
            } else {
                if nums.len() == 3 {
                    return f64::NAN;
                }
                nums.push(n);
                // `1997-3-8` / `5/1/1999`: the separator belongs to the date.
                if b.get(i) == Some(&b'-') || b.get(i) == Some(&b'/') {
                    i += 1;
                }
            }
            continue;
        }
        if c == b'+' || c == b'-' {
            let sign = if c == b'-' { -1 } else { 1 };
            // V8's rule: a sign is a time-zone offset once a time (or an
            // explicit UTC marker) has been seen, and otherwise the sign of a
            // date number — which is how `+001997-3-8 11:19:20` and this
            // engine's own `Sat Jan 01 -0001 …` both parse.
            if time.is_some() || tz_utc {
                i += 1;
                let oh = match read_uint(b, &mut i) {
                    Some(v) => v,
                    None => return f64::NAN,
                };
                if b.get(i) == Some(&b':') {
                    i += 1;
                    let om = match read_uint(b, &mut i) {
                        Some(v) => v,
                        None => return f64::NAN,
                    };
                    offset_min = Some(sign * (oh * 60 + om));
                } else if oh >= 100 {
                    // `-0700` — hours and minutes written without the colon.
                    offset_min = Some(sign * ((oh / 100) * 60 + oh % 100));
                } else {
                    // `-07` — hours only.
                    offset_min = Some(sign * oh * 60);
                }
                continue;
            }
            i += 1;
            let n = match read_uint(b, &mut i) {
                Some(v) => v,
                None => return f64::NAN,
            };
            if nums.len() == 3 {
                return f64::NAN;
            }
            nums.push(sign * n);
            if b.get(i) == Some(&b'-') || b.get(i) == Some(&b'/') {
                i += 1;
            }
            continue;
        }
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < b.len() && b[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word = &s[start..i];
            // V8 keys its keyword table on the first three letters and ignores
            // whatever follows, so `March` and `Mar` are the same token.
            let key: String = word.chars().take(3).flat_map(|c| c.to_lowercase()).collect();
            if let Some(m) = MONTH.iter().position(|x| x.eq_ignore_ascii_case(&key)) {
                if named_month.is_some() {
                    return f64::NAN;
                }
                named_month = Some(m as i64);
            } else if WEEKDAY.iter().any(|w| w.eq_ignore_ascii_case(&key)) {
                // Ignored — never validated against the date.
            } else if word.eq_ignore_ascii_case("gmt")
                || word.eq_ignore_ascii_case("ut")
                || word.eq_ignore_ascii_case("utc")
                || word.eq_ignore_ascii_case("z")
            {
                tz_utc = true;
                if offset_min.is_none() {
                    offset_min = Some(0);
                }
            } else if word.eq_ignore_ascii_case("am") {
                pm = Some(false);
            } else if word.eq_ignore_ascii_case("pm") {
                pm = Some(true);
            } else {
                return f64::NAN;
            }
            continue;
        }
        // Anything else (including a bare `T`, which selects the STRICT grammar
        // and so must not be readable here) is not a legacy date.
        return f64::NAN;
    }

    // DayComposer::Write.
    if nums.is_empty() {
        return f64::NAN;
    }
    while nums.len() < 3 {
        nums.push(1);
    }
    let (year, month, day) = match named_month {
        None => {
            if !is_day_num(nums[0]) {
                (nums[0], nums[1], nums[2]) // Y/M/D
            } else {
                (nums[2], nums[0], nums[1]) // M/D/Y
            }
        }
        Some(m) => {
            let (y, d) = if !is_day_num(nums[0]) { (nums[0], nums[1]) } else { (nums[1], nums[0]) };
            (y, m + 1, d)
        }
    };
    let year = match year {
        0..=49 => year + 2000,
        50..=99 => year + 1900,
        _ => year,
    };
    if !(1..=12).contains(&month) || !is_day_num(day) {
        return f64::NAN;
    }
    let (mut h, mi, sec, ms) = time.unwrap_or((0, 0, 0, 0));
    // A 12-hour clock, as V8's `TimeComposer::Write` spells it: the hour must be
    // at most 12, 12 folds to 0, and `pm` then adds 12. So `12:30 am` is 00:30,
    // `12:30 pm` is 12:30, `0:30 am` is 00:30 and `13:30 pm` is not a time.
    if let Some(is_pm) = pm {
        if h > 12 {
            return f64::NAN;
        }
        if h == 12 {
            h = 0;
        }
        if is_pm {
            h += 12;
        }
    }
    if h > 24 || mi > 59 || sec > 59 || (h == 24 && (mi != 0 || sec != 0 || ms != 0)) {
        return f64::NAN;
    }
    ms_from_utc(year, month - 1, day, h, mi, sec, ms)
        - offset_min.unwrap_or(0) as f64 * 60_000.0
}

/// Read a run of ASCII digits at `i` (at least one), advancing `i`.
fn read_uint(b: &[u8], i: &mut usize) -> Option<i64> {
    let start = *i;
    let mut v: i64 = 0;
    while *i < b.len() && b[*i].is_ascii_digit() {
        v = v.checked_mul(10)?.checked_add((b[*i] - b'0') as i64)?;
        *i += 1;
    }
    if *i == start {
        None
    } else {
        Some(v)
    }
}

/// `Number.prototype.toFixed(f)`. JS rounds half AWAY from zero — `(0.5).toFixed(0)`
/// is "1", `(2.5).toFixed(0)` is "3" — whereas Rust's `{:.*}` formatter rounds
/// half-to-even. We round the EXACT decimal of the f64 (not `x*10^f`, whose
/// product error would mis-round e.g. `0.15` whose true value is `0.14999…`):
/// format with guard digits to expose the exact value, then round the decimal
/// string half-up at `f` places. Huge magnitudes (≥1e21) defer to the default
/// rendering (JS switches to exponential there too).
pub(crate) fn to_fixed(n: f64, f: usize) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n.abs() >= 1e21 {
        // JS switches to exponential here — use the ECMAScript Number→String
        // (`fmt_f64`), NOT Rust's `{}` which prints the full decimal: e.g.
        // `(1e21).toFixed(2)` is "1e+21", not "1000000000000000000000".
        return fmt_f64(n);
    }
    // Step 10 is `If x < 0`, not "if the sign bit is set" — so `-0` carries no
    // sign (`(-0).toFixed(2)` is "0.00") while `-0.0001` still does ("-0.00").
    let neg = n < 0.0;
    // Exact decimal of |n| with 30 guard digits past `f`; the digit at index `f`
    // (first dropped) decides the rounding, and the formatter computes it exactly.
    let s = format!("{:.*}", f + 30, n.abs());
    let dot = s.find('.').unwrap();
    let int_part = &s[..dot];
    let frac = s[dot + 1..].as_bytes();
    let round_up = frac[f] >= b'5';
    // Digits we keep (integer + first `f` fractional), as a mutable byte buffer.
    let mut digits: Vec<u8> = int_part.bytes().chain(frac[..f].iter().copied()).collect();
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1'); // carried past the most-significant digit
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    // Place the decimal point `f` digits from the right.
    let mut out = String::from_utf8(digits).unwrap();
    if f > 0 {
        let point = out.len() - f;
        out.insert(point, '.');
    }
    if neg {
        out.insert(0, '-');
    }
    out
}

pub(crate) fn num_to_radix(n: f64, radix: u32) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let neg = n < 0.0;
    let mut int = n.abs().trunc() as u64;
    if int == 0 {
        return "0".into();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while int > 0 {
        buf.push(DIGITS[(int % radix as u64) as usize]);
        int /= radix as u64;
    }
    if neg {
        buf.push(b'-');
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

/// Normalize a Map key / Set element: `-0` becomes `+0` (SameValueZero treats
/// them equal, and iteration must yield `+0`). Everything else is unchanged.
pub(crate) fn normalize_zero(v: Value) -> Value {
    if v.is_double() && v.as_f64() == 0.0 {
        Value::num(0.0)
    } else {
        v
    }
}

/// JS `ToInt32`: truncate toward zero, take modulo 2^32, interpret as signed.
/// NaN/±Infinity → 0. Used by the bitwise operators.
pub(crate) fn to_int32(n: f64) -> i32 {
    to_uint32(n) as i32
}

/// JS `ToUint32`: truncate toward zero, take modulo 2^32 as an unsigned value.
pub(crate) fn to_uint32(n: f64) -> u32 {
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    // rem_euclid keeps the result in [0, 2^32); `as u32` then wraps exactly.
    let m = n.trunc().rem_euclid(4_294_967_296.0);
    m as u32
}

pub(crate) fn fmt_f64(n: f64) -> String {
    let mut out = String::new();
    fmt_f64_into(&mut out, n);
    out
}

/// `fmt_f64` appending into an existing buffer — the JSON.stringify fast path,
/// which otherwise paid a fresh `String` per serialized number. The allocating
/// wrapper above stays for every other caller.
pub(crate) fn fmt_f64_into(out: &mut String, n: f64) {
    if n.is_nan() {
        out.push_str("NaN");
        return;
    }
    if n.is_infinite() {
        out.push_str(if n > 0.0 { "Infinity" } else { "-Infinity" });
        return;
    }
    if n == 0.0 {
        out.push('0');
        return;
    }
    let neg = n < 0.0;
    let abs = n.abs();
    // Integer-valued doubles below the 1e21 exponential cutoff print without a
    // decimal point. Rust's `{}` is shortest-round-trip (matches JS Number→String,
    // e.g. 4660046610375530000 not …496) — NOT `n as i64`, which prints excess
    // digits the f64 can't distinguish and overflows for whole doubles > i64::MAX.
    if abs.fract() == 0.0 && abs < 1e21 {
        // Below 2^53 every integer is exactly representable and no two share an
        // f64, so the shortest round-trip form IS the exact decimal expansion —
        // integer formatting produces the identical string, and does it without
        // running the float shortest-round-trip search. Measured: an integral
        // double above i32 went 132ns -> 71ns per conversion.
        //
        // At or above 2^53 the f64 formatter is REQUIRED, not merely preferred:
        // integers there have gaps, so `as u64` would print excess digits the
        // f64 cannot distinguish (e.g. 4660046610375529984 where JS must say
        // 4660046610375530000).
        if neg {
            out.push('-');
        }
        if abs < 9_007_199_254_740_992.0 {
            // Digits written backward into a stack buffer (2^53-1 has 16),
            // then appended in one push — no intermediate String.
            let mut i = abs as u64;
            let mut buf = [0u8; 16];
            let mut p = buf.len();
            loop {
                p -= 1;
                buf[p] = b'0' + (i % 10) as u8;
                i /= 10;
                if i == 0 {
                    break;
                }
            }
            out.push_str(std::str::from_utf8(&buf[p..]).unwrap());
            return;
        }
        use std::fmt::Write;
        let _ = write!(out, "{abs}");
        return;
    }
    // General case: ECMAScript Number::toString (7.1.12.1). Extract the shortest
    // round-trip significant digits `s` (k of them) and the decimal point position
    // `n` such that the value is `s × 10^(n-k)`, via Rust's `{:e}` (also shortest
    // round-trip), then format with JS's exponential cutoffs (n > 21 or n ≤ -6).
    let sci = format!("{abs:e}"); // e.g. "1.2345e2", "1e21", "5e-1"
    let (mant, exp) = sci.split_once('e').expect("{:e} always has an exponent");
    let e: i32 = exp.parse().expect("valid exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let s = digits.as_str();
    let k = s.len() as i32;
    let np = e + 1; // decimal-point position (value ≈ 0.s × 10^np)
    if neg {
        out.push('-');
    }
    if k <= np && np <= 21 {
        // Integer: all digits, then (np-k) trailing zeros.
        out.push_str(s);
        out.extend(std::iter::repeat('0').take((np - k) as usize));
    } else if 0 < np && np <= 21 {
        // Point inside the digits.
        out.push_str(&s[..np as usize]);
        out.push('.');
        out.push_str(&s[np as usize..]);
    } else if -6 < np && np <= 0 {
        // Leading "0." then (-np) zeros then the digits.
        out.push_str("0.");
        out.extend(std::iter::repeat('0').take((-np) as usize));
        out.push_str(s);
    } else {
        // Exponential: first digit, optional ".rest", then e±(np-1).
        out.push_str(&s[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&s[1..]);
        }
        let e2 = np - 1;
        out.push('e');
        out.push(if e2 >= 0 { '+' } else { '-' });
        use std::fmt::Write;
        let _ = write!(out, "{}", e2.abs());
    }
}

#[cfg(test)]
mod fmt_f64_int_fast_path_tests {
    use super::fmt_f64;

    /// The integer fast path must be INDISTINGUISHABLE from the float formatter
    /// it replaces, including at the 2^53 boundary where it must hand back over.
    #[test]
    fn integer_fast_path_matches_float_formatter() {
        fn reference(n: f64) -> String {
            // What the code did before the fast path, for the integral branch.
            let neg = n < 0.0;
            let abs = n.abs();
            if neg { format!("-{abs}") } else { format!("{abs}") }
        }
        let mut vals: Vec<f64> = Vec::new();
        for i in 0..2000u64 {
            vals.push(i as f64);
        }
        for e in 0..53 {
            let b = (1u64 << e) as f64;
            for d in [-2.0, -1.0, 0.0, 1.0, 2.0] {
                let v = b + d;
                if v >= 0.0 && v.fract() == 0.0 {
                    vals.push(v);
                }
            }
        }
        // Straddle the hand-back point exactly.
        for k in 0..64u64 {
            vals.push(9_007_199_254_740_992.0 - k as f64);
            vals.push(9_007_199_254_740_992.0 + 2.0 * k as f64);
        }
        vals.push(4_294_967_295.0);
        vals.push(4_294_967_296.0);
        vals.push(1e20);
        for v in vals {
            for signed in [v, -v] {
                if signed.fract() != 0.0 || signed.abs() >= 1e21 {
                    continue;
                }
                assert_eq!(fmt_f64(signed), reference(signed), "mismatch at {signed}");
            }
        }
    }
}
