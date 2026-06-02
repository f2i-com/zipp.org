#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
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
        M::Round => (x + 0.5).floor(),
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
        M::Acosh => x.acosh(),
        M::Atanh => x.atanh(),
        // Math.clz32: leading zeros of ToUint32(x). Math.fround: round to f32.
        M::Clz32 => to_uint32(x).leading_zeros() as f64,
        M::Fround => x as f32 as f64,
        // Pow/Atan2/Imul/Min/Max/Hypot aren't unary; degrade gracefully.
        M::Min | M::Max => x,
        M::Hypot => x.abs(),
        M::Pow | M::Atan2 | M::Imul => f64::NAN,
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

/// The legacy 2-digit-year mapping for the numeric Date constructors: 0..=99 →
/// 1900+y (so `Date.UTC(99,…)` is 1999). Years ≥100 (and negative) pass through.
pub(crate) fn legacy_year(y: i64) -> i64 {
    if (0..=99).contains(&y) {
        1900 + y
    } else {
        y
    }
}

/// JS TimeClip: NaN if non-finite or |t| > 8.64e15 (±100M days); else truncate
/// toward zero to an integer millisecond.
pub(crate) fn time_clip(n: f64) -> f64 {
    if !n.is_finite() || n.abs() > 8.64e15 {
        f64::NAN
    } else {
        n.trunc()
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

/// Parse the ISO-8601 subset JS accepts (`YYYY`, `YYYY-MM`, `YYYY-MM-DD`,
/// optionally `THH:mm[:ss[.sss]]` and a trailing `Z`). Treated as UTC. Returns
/// NaN if unrecognised.
pub(crate) fn parse_date(s: &str) -> f64 {
    let s = s.trim();
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let dp: Vec<&str> = date.split('-').collect();
    // A leading '-' (negative year) splits into an empty first field; reject.
    if dp.is_empty() || dp[0].is_empty() {
        return f64::NAN;
    }
    let parse = |x: &str| x.parse::<i64>().ok();
    let year = match parse(dp[0]) {
        Some(y) => y,
        None => return f64::NAN,
    };
    let mo = if dp.len() > 1 { match parse(dp[1]) { Some(v) => v, None => return f64::NAN } } else { 1 };
    let day = if dp.len() > 2 { match parse(dp[2]) { Some(v) => v, None => return f64::NAN } } else { 1 };
    let (mut h, mut mi, mut sec, mut msec) = (0i64, 0i64, 0i64, 0i64);
    if let Some(t) = time {
        let t = t.trim_end_matches('Z');
        // Drop a timezone offset (we treat everything as UTC).
        let t = t.split(['+']).next().unwrap_or(t);
        let (hms, frac) = match t.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (t, None),
        };
        let tp: Vec<&str> = hms.split(':').collect();
        if !tp.is_empty() {
            h = parse(tp[0]).unwrap_or(0);
        }
        if tp.len() > 1 {
            mi = parse(tp[1]).unwrap_or(0);
        }
        if tp.len() > 2 {
            sec = parse(tp[2]).unwrap_or(0);
        }
        if let Some(f) = frac {
            // First 3 digits = milliseconds.
            let f3: String = f.chars().take(3).chain(std::iter::repeat('0')).take(3).collect();
            msec = f3.parse::<i64>().unwrap_or(0);
        }
    }
    // mo here is 1-based from the string; ms_from_utc wants 0-based.
    ms_from_utc(year, mo - 1, day, h, mi, sec, msec)
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
        return format!("{n}");
    }
    let neg = n.is_sign_negative();
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
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n == 0.0 {
        return "0".into();
    }
    let neg = n < 0.0;
    let abs = n.abs();
    // Integer-valued doubles below the 1e21 exponential cutoff print without a
    // decimal point. Rust's `{}` is shortest-round-trip (matches JS Number→String,
    // e.g. 4660046610375530000 not …496) — NOT `n as i64`, which prints excess
    // digits the f64 can't distinguish and overflows for whole doubles > i64::MAX.
    if abs.fract() == 0.0 && abs < 1e21 {
        return if neg { format!("-{abs}") } else { format!("{abs}") };
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
    let body = if k <= np && np <= 21 {
        // Integer: all digits, then (np-k) trailing zeros.
        let mut r = String::from(s);
        r.extend(std::iter::repeat('0').take((np - k) as usize));
        r
    } else if 0 < np && np <= 21 {
        // Point inside the digits.
        format!("{}.{}", &s[..np as usize], &s[np as usize..])
    } else if -6 < np && np <= 0 {
        // Leading "0." then (-np) zeros then the digits.
        let mut r = String::from("0.");
        r.extend(std::iter::repeat('0').take((-np) as usize));
        r.push_str(s);
        r
    } else {
        // Exponential: first digit, optional ".rest", then e±(np-1).
        let mut m = String::from(&s[..1]);
        if k > 1 {
            m.push('.');
            m.push_str(&s[1..]);
        }
        let e2 = np - 1;
        format!("{m}e{}{}", if e2 >= 0 { '+' } else { '-' }, e2.abs())
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}
