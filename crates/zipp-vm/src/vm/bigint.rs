//! Arbitrary-precision BigInt support.
//!
//! Two-tier representation: `HeapObj::BigInt(i128)` is the FAST tier (every
//! value that fits i128 — virtually all real arithmetic stays here, allocation
//! free beyond the heap slot), `HeapObj::BigIntBig(Box<num_bigint::BigInt>)` is
//! the slow tier for values outside i128 range.
//!
//! CANONICAL-FORM INVARIANT: a `BigIntBig` never holds an i128-representable
//! value. Every constructor funnels through `BigVal::from_num` /
//! `Vm::make_bigint_val`, which demote a fitting result back to the fast tier.
//! Consequences relied on throughout the engine:
//!   * `BigInt(x) == BigInt(y)` / `BigIntBig(x) == BigIntBig(y)` compare by
//!     value; a `BigInt` and a `BigIntBig` are ALWAYS unequal (`===`, Map/Set
//!     keys, SameValue) — no cross-variant numeric comparison is ever needed.
//!   * Ordering a `Small` against a `Big` is decided by the Big's sign alone
//!     (its magnitude is beyond i128 by construction).
//!
//! Size cap: like V8, a result whose magnitude would exceed `MAX_BIGINT_BITS`
//! bits (huge `<<` counts, `**` with large exponents, `asUintN` with an absurd
//! width) throws `RangeError: Maximum BigInt size exceeded` instead of OOMing.

#![allow(unused_imports)]
use super::*;
use crate::heap::HeapObj;
use crate::value::Value;
use crate::vm::helpers_misc::BigOp;
use crate::vm::helpers_numeric::{bigint_as_intn, bigint_as_uintn, bigint_to_radix};
use num_bigint::{BigInt as NumBig, Sign};
use num_traits::{FromPrimitive, One, Signed, ToPrimitive, Zero};
use std::cmp::Ordering;

/// Maximum BigInt magnitude in bits. The ordinary engine matches V8's 2^30
/// limit; the hardened profile caps one value at roughly 128 KiB so a single
/// shift/power cannot overshoot a worker's memory ceiling by 128 MiB.
#[cfg(feature = "safe-sandbox")]
pub(crate) const MAX_BIGINT_BITS: u64 = 1 << 20;
#[cfg(not(feature = "safe-sandbox"))]
pub(crate) const MAX_BIGINT_BITS: u64 = 1 << 30;

pub(crate) const BIGINT_MIX_ERR: &str =
    "TypeError: Cannot mix BigInt and other types, use explicit conversions";
pub(crate) const BIGINT_SIZE_ERR: &str = "RangeError: Maximum BigInt size exceeded";

/// A BigInt mathematical value, detached from the heap, in canonical two-tier
/// form (see the module doc). This is what coercions (`ToBigInt`) and operators
/// pass around; `Vm::make_bigint_val` turns it back into a heap Value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BigVal {
    Small(i128),
    Big(NumBig),
}

impl BigVal {
    /// The single canonicalizing constructor: demotes to `Small` when fitting.
    pub(crate) fn from_num(n: NumBig) -> BigVal {
        match n.to_i128() {
            Some(v) => BigVal::Small(v),
            None => BigVal::Big(n),
        }
    }

    pub(crate) fn to_num(&self) -> NumBig {
        match self {
            BigVal::Small(n) => NumBig::from(*n),
            BigVal::Big(b) => b.clone(),
        }
    }

    pub(crate) fn is_zero(&self) -> bool {
        matches!(self, BigVal::Small(0))
    }

    pub(crate) fn is_negative(&self) -> bool {
        match self {
            BigVal::Small(n) => *n < 0,
            BigVal::Big(b) => b.sign() == Sign::Minus,
        }
    }

    /// Magnitude bit length (0 for 0).
    pub(crate) fn bits(&self) -> u64 {
        match self {
            BigVal::Small(n) => (128 - n.unsigned_abs().leading_zeros()) as u64,
            BigVal::Big(b) => b.bits(),
        }
    }

    /// Negation (canonical: `-(2^127)` demotes to `Small(i128::MIN)`,
    /// `-(i128::MIN)` promotes to `Big(2^127)`).
    pub(crate) fn neg(self) -> BigVal {
        match self {
            BigVal::Small(n) => match n.checked_neg() {
                Some(v) => BigVal::Small(v),
                None => BigVal::Big(-NumBig::from(n)),
            },
            BigVal::Big(b) => BigVal::from_num(-b),
        }
    }

    /// Low 64 bits, two's complement — NumericToRawBytes for
    /// BigInt64/BigUint64 array elements, DataView and Atomics operands.
    pub(crate) fn to_u64_wrap(&self) -> u64 {
        match self {
            BigVal::Small(n) => *n as u64,
            BigVal::Big(b) => {
                let low = b.iter_u64_digits().next().unwrap_or(0);
                if b.sign() == Sign::Minus {
                    low.wrapping_neg()
                } else {
                    low
                }
            }
        }
    }

    pub(crate) fn to_i64_wrap(&self) -> i64 {
        self.to_u64_wrap() as i64
    }

    /// SATURATING i128 — for domain range checks (Temporal epochNanoseconds):
    /// a value beyond i128 is certainly beyond any domain range, and the
    /// saturation preserves the sign and the hugeness the check observes.
    /// (±i128::MAX, not MIN, keeps negation involutive — matches the engine's
    /// historical saturating behavior.)
    pub(crate) fn to_i128_sat(&self) -> i128 {
        match self {
            BigVal::Small(n) => *n,
            BigVal::Big(b) => {
                if b.sign() == Sign::Minus {
                    -i128::MAX
                } else {
                    i128::MAX
                }
            }
        }
    }

    /// Total order by mathematical value. Canonical form makes the mixed cases
    /// sign-only decisions (a Big's magnitude is beyond every i128).
    pub(crate) fn cmp(&self, other: &BigVal) -> Ordering {
        match (self, other) {
            (BigVal::Small(a), BigVal::Small(b)) => a.cmp(b),
            (BigVal::Big(a), BigVal::Big(b)) => a.cmp(b),
            (BigVal::Small(_), BigVal::Big(b)) => {
                if b.sign() == Sign::Minus {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (BigVal::Big(a), BigVal::Small(_)) => {
                if a.sign() == Sign::Minus {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
        }
    }

    /// Exact ordering against an f64 by mathematical value (`None` for NaN).
    pub(crate) fn cmp_f64(&self, f: f64) -> Option<Ordering> {
        match self {
            BigVal::Small(n) => crate::vm::coerce::cmp_i128_f64(*n, f),
            BigVal::Big(b) => cmp_big_f64(b, f),
        }
    }

    /// String in the given radix (2..=36), lowercase digits.
    pub(crate) fn to_radix_string(&self, radix: u32) -> String {
        match self {
            BigVal::Small(n) => bigint_to_radix(*n, radix),
            BigVal::Big(b) => b.to_str_radix(radix),
        }
    }
}

/// Exact ordering of an out-of-i128 BigInt against an f64 by mathematical
/// value (`None` when `f` is NaN). The f64's integer part is exactly
/// representable as a BigInt, so the comparison is precise — no rounding
/// through `as f64`.
pub(crate) fn cmp_big_f64(x: &NumBig, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if f == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    let t = f.trunc();
    let bt = NumBig::from_f64(t)?; // exact: t is a finite integral f64
    Some(match x.cmp(&bt) {
        // x equals f's integer part: the fraction (sign) breaks the tie.
        Ordering::Equal => {
            if f > t {
                Ordering::Less
            } else if f < t {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        o => o,
    })
}

impl<'p> Vm<'p> {
    /// Allocate a BigInt Value from a `BigVal` (canonical by construction).
    pub(crate) fn make_bigint_val(&mut self, v: BigVal) -> Value {
        match v {
            BigVal::Small(n) => self.make_bigint(n),
            BigVal::Big(b) => Value::heap(self.heap.alloc(HeapObj::BigIntBig(Box::new(b)))),
        }
    }

    /// Allocate a BigInt Value from raw `num_bigint` math, demoting to the
    /// fast i128 representation when it fits (the canonical-form gate).
    pub(crate) fn make_bigint_num(&mut self, n: NumBig) -> Value {
        let v = BigVal::from_num(n);
        self.make_bigint_val(v)
    }

    /// The `BigVal` of a BigInt PRIMITIVE (either tier; clones a Big), else None.
    pub(crate) fn bigint_val(&self, v: Value) -> Option<BigVal> {
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::BigInt(n) => return Some(BigVal::Small(*n)),
                HeapObj::BigIntBig(b) => return Some(BigVal::Big((**b).clone())),
                _ => {}
            }
        }
        None
    }

    /// thisBigIntValue(v): a BigInt primitive OR a boxed BigInt wrapper
    /// (`Object(1n)`, a Boxed of kind 4). Backs BigInt.prototype methods.
    pub(crate) fn this_bigint_val(&self, v: Value) -> Option<BigVal> {
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::BigInt(n) => return Some(BigVal::Small(*n)),
                HeapObj::BigIntBig(b) => return Some(BigVal::Big((**b).clone())),
                HeapObj::Boxed { kind: 4, value } => return self.bigint_val(*value),
                _ => {}
            }
        }
        None
    }

    /// Whether `v` is a BigInt primitive (either tier; NOT a Boxed wrapper).
    pub(crate) fn is_bigint_prim(&self, v: Value) -> bool {
        v.is_heap()
            && matches!(
                self.heap.get(v.heap_index()),
                HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
            )
    }

    /// ToNumeric's ToPrimitive(number) step for an operand of a numeric
    /// operator: a true object (incl. a boxed wrapper) runs the observable
    /// protocol; primitives and the engine's unobservable shortcut types
    /// (Date / strings) pass through unchanged.
    pub(crate) fn numeric_prim(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap()
            && !matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Date(_)
                    | HeapObj::Str(_)
                    | HeapObj::Cons { .. }
                    | HeapObj::Symbol { .. }
                    | HeapObj::BigInt(_)
                    | HeapObj::BigIntBig(_)
            )
        {
            return self.to_primitive_number(v);
        }
        Ok(v)
    }

    /// ApplyStringOrNumericBinaryOperator for every numeric operator (`+`'s
    /// string case lives in `add_values`): ToNumeric each operand IN ORDER,
    /// then BigInt math when both are BigInts, Number math when neither is,
    /// and the spec's mixing TypeError otherwise. This runs ToPrimitive at
    /// most ONCE per operand (the caller must not re-coerce).
    pub(crate) fn numeric_binop(
        &mut self,
        op: BigOp,
        va: Value,
        vb: Value,
    ) -> Result<Value, Thrown> {
        // GC INVARIANT: `pa` (possibly a fresh primitive from va's valueOf) is
        // held in a Rust local — unrooted — while vb's coercion runs user code.
        let _gc = self.gc_lock_guard();
        let pa = self.numeric_prim(va)?;
        // ToNumeric(lval) COMPLETES (a Symbol is a TypeError) before rval's
        // observable coercion runs, per evaluation order.
        if pa.is_heap() && matches!(self.heap.get(pa.heap_index()), HeapObj::Symbol { .. }) {
            return Err(Thrown(
                "TypeError: Cannot convert a Symbol value to a number".into(),
            ));
        }
        let pb = self.numeric_prim(vb)?;
        let ab = self.bigint_val(pa);
        let bb = self.bigint_val(pb);
        match (ab, bb) {
            (Some(a), Some(b)) => self.bigint_op(op, a, b),
            (None, None) => self.number_binop(op, pa, pb),
            _ => Err(Thrown(BIGINT_MIX_ERR.into())),
        }
    }

    /// The Number side of `numeric_binop`: both operands are non-BigInt
    /// PRIMITIVES (ToNumber from here is unobservable).
    fn number_binop(&mut self, op: BigOp, pa: Value, pb: Value) -> Result<Value, Thrown> {
        use crate::vm::helpers_num2::{to_int32, to_uint32};
        let r = match op {
            BigOp::Add => Value::num(self.to_number(pa)? + self.to_number(pb)?),
            BigOp::Sub => Value::num(self.to_number(pa)? - self.to_number(pb)?),
            BigOp::Mul => Value::num(self.to_number(pa)? * self.to_number(pb)?),
            BigOp::Div => Value::num(self.to_number(pa)? / self.to_number(pb)?),
            BigOp::Mod => Value::num(self.to_number(pa)? % self.to_number(pb)?),
            BigOp::Pow => {
                let bb = self.to_number(pa)?;
                let ee = self.to_number(pb)?;
                // Spec: |base|==1 with a NaN/±Infinity exponent is NaN
                // (C/Rust powf returns 1 — a deliberate deviation).
                let p = if (bb == 1.0 || bb == -1.0) && (ee.is_nan() || ee.is_infinite()) {
                    f64::NAN
                } else {
                    bb.powf(ee)
                };
                Value::num(p)
            }
            BigOp::And => Value::int(to_int32(self.to_number(pa)?) & to_int32(self.to_number(pb)?)),
            BigOp::Or => Value::int(to_int32(self.to_number(pa)?) | to_int32(self.to_number(pb)?)),
            BigOp::Xor => Value::int(to_int32(self.to_number(pa)?) ^ to_int32(self.to_number(pb)?)),
            // Shift counts use the low 5 bits per the JS spec.
            BigOp::Shl => {
                let x = to_int32(self.to_number(pa)?);
                let s = to_uint32(self.to_number(pb)?) & 31;
                Value::int(x.wrapping_shl(s))
            }
            BigOp::Shr => {
                let x = to_int32(self.to_number(pa)?);
                let s = to_uint32(self.to_number(pb)?) & 31;
                Value::int(x >> s)
            }
            BigOp::Ushr => {
                let x = to_uint32(self.to_number(pa)?);
                let s = to_uint32(self.to_number(pb)?) & 31;
                let u = x >> s;
                // u32 may exceed i32::MAX — keep numeric range.
                if u <= i32::MAX as u32 {
                    Value::int(u as i32)
                } else {
                    Value::num(u as f64)
                }
            }
        };
        Ok(r)
    }

    /// A BigInt binary operation on two already-coerced BigInt values. The
    /// i128 fast path uses checked ops; ONLY a genuine overflow (or a Big
    /// operand) takes the num-bigint path, and every result re-canonicalizes.
    pub(crate) fn bigint_op(&mut self, op: BigOp, a: BigVal, b: BigVal) -> Result<Value, Thrown> {
        // `>>>` is not defined for BigInt regardless of values.
        if matches!(op, BigOp::Ushr) {
            return Err(Thrown(
                "TypeError: BigInts have no unsigned right shift, use >> instead".into(),
            ));
        }
        // Division/remainder by zero is a RangeError regardless of tier.
        if matches!(op, BigOp::Div | BigOp::Mod) && b.is_zero() {
            return Err(Thrown("RangeError: Division by zero".into()));
        }
        // Shifts and pow have their own count/size handling.
        match op {
            BigOp::Shl => return self.bigint_shift(a, b, true),
            BigOp::Shr => return self.bigint_shift(a, b, false),
            BigOp::Pow => return self.bigint_pow(a, b),
            _ => {}
        }
        // i128 fast path (no allocation while results stay in range).
        if let (BigVal::Small(x), BigVal::Small(y)) = (&a, &b) {
            let (x, y) = (*x, *y);
            let r = match op {
                BigOp::Add => x.checked_add(y),
                BigOp::Sub => x.checked_sub(y),
                BigOp::Mul => x.checked_mul(y),
                // checked_div/rem only fail for i128::MIN / -1 (→ big path).
                BigOp::Div => x.checked_div(y),
                BigOp::Mod => x.checked_rem(y),
                BigOp::And => Some(x & y),
                BigOp::Or => Some(x | y),
                BigOp::Xor => Some(x ^ y),
                BigOp::Pow | BigOp::Shl | BigOp::Shr | BigOp::Ushr => unreachable!(),
            };
            if let Some(r) = r {
                return Ok(self.make_bigint(r));
            }
        }
        // Big path: exact num-bigint math, result re-canonicalized.
        let (x, y) = (a.to_num(), b.to_num());
        let r = match op {
            BigOp::Add => x + y,
            BigOp::Sub => x - y,
            BigOp::Mul => {
                // Product bits ≈ bits(x)+bits(y): cap like V8 before allocating.
                if x.bits().saturating_add(y.bits()) > MAX_BIGINT_BITS + 1 {
                    return Err(Thrown(BIGINT_SIZE_ERR.into()));
                }
                x * y
            }
            // num-bigint `/` truncates toward zero and `%` takes the dividend's
            // sign — exactly the spec's BigInt::divide / BigInt::remainder.
            BigOp::Div => x / y,
            BigOp::Mod => x % y,
            BigOp::And => x & y,
            BigOp::Or => x | y,
            BigOp::Xor => x ^ y,
            BigOp::Pow | BigOp::Shl | BigOp::Shr | BigOp::Ushr => unreachable!(),
        };
        Ok(self.make_bigint_num(r))
    }

    /// `x << y` (`left`) / `x >> y` (`!left`) per BigInt::leftShift — a right
    /// shift is a left shift by the negated count; negative-direction shifts
    /// round toward -∞ (arithmetic shift). A count that would push the result
    /// past `MAX_BIGINT_BITS` throws RangeError (V8 parity) — but a shift that
    /// only DISCARDS bits (right direction) is computed for any count.
    fn bigint_shift(&mut self, a: BigVal, b: BigVal, left: bool) -> Result<Value, Thrown> {
        if a.is_zero() {
            return Ok(self.make_bigint(0));
        }
        // Effective LEFT-shift count. A Big count is astronomically large in
        // magnitude either way — only its direction matters (RangeError when
        // growing, all-bits-shifted-out when shrinking).
        let count: i128 = match &b {
            BigVal::Small(y) => {
                if left {
                    *y
                } else {
                    y.checked_neg().unwrap_or(i128::MAX)
                }
            }
            BigVal::Big(big) => {
                if (big.sign() != Sign::Minus) == left {
                    i128::MAX
                } else {
                    i128::MIN
                }
            }
        };
        if count >= 0 {
            // Growing: i128 fast path with a round-trip check, else num-bigint.
            if let BigVal::Small(n) = a {
                if count < 128 {
                    let r = n.wrapping_shl(count as u32);
                    if (r >> (count as u32)) == n {
                        return Ok(self.make_bigint(r));
                    }
                }
            }
            if a.bits().saturating_add(count.min(u64::MAX as i128) as u64) > MAX_BIGINT_BITS {
                return Err(Thrown(BIGINT_SIZE_ERR.into()));
            }
            Ok(self.make_bigint_num(a.to_num() << (count as usize)))
        } else {
            // Shrinking: arithmetic shift (rounds toward -∞ — both i128's `>>`
            // and num-bigint's `>>` do). Cap the count at bits+1 (beyond that
            // the result is already 0 or -1), keeping huge counts allocation-free.
            let s = count.unsigned_abs().min(a.bits() as u128 + 1) as u32;
            match a {
                BigVal::Small(n) => Ok(self.make_bigint(n >> s.min(127))),
                BigVal::Big(b) => Ok(self.make_bigint_num(b >> (s as usize))),
            }
        }
    }

    /// `x ** y` for BigInts: negative exponent → RangeError; 0/±1 bases short-
    /// circuit for ANY exponent; otherwise the result size is capped like V8.
    fn bigint_pow(&mut self, a: BigVal, b: BigVal) -> Result<Value, Thrown> {
        if b.is_negative() {
            return Err(Thrown("RangeError: Exponent must be non-negative".into()));
        }
        if b.is_zero() {
            return Ok(self.make_bigint(1));
        }
        // 0/±1 bases: |result| ≤ 1 for any exponent (so no size concerns).
        match &a {
            BigVal::Small(0) => return Ok(self.make_bigint(0)),
            BigVal::Small(1) => return Ok(self.make_bigint(1)),
            BigVal::Small(-1) => {
                let odd = match &b {
                    BigVal::Small(y) => y & 1 == 1,
                    BigVal::Big(big) => big.bit(0),
                };
                return Ok(self.make_bigint(if odd { -1 } else { 1 }));
            }
            _ => {}
        }
        // |base| ≥ 2 from here: result bits ≈ bits(a) · e.
        let e: u64 = match &b {
            BigVal::Small(y) => *y as u64,
            // |base| ≥ 2 with an exponent beyond i128 is astronomically big.
            BigVal::Big(_) => return Err(Thrown(BIGINT_SIZE_ERR.into())),
        };
        // i128 fast path for small results.
        if let BigVal::Small(x) = a {
            if e <= 127 {
                if let Some(r) = x.checked_pow(e as u32) {
                    return Ok(self.make_bigint(r));
                }
            }
        }
        if a.bits().saturating_mul(e) > MAX_BIGINT_BITS {
            return Err(Thrown(BIGINT_SIZE_ERR.into()));
        }
        // bits(a) ≥ 2 here, so e ≤ MAX_BIGINT_BITS/2 < u32::MAX.
        Ok(self.make_bigint_num(a.to_num().pow(e as u32)))
    }

    /// `BigInt.asIntN(bits, x)`: x wrapped into the signed bits-bit range.
    pub(crate) fn bigint_as_intn_val(&mut self, bits: u64, x: BigVal) -> Result<Value, Thrown> {
        if bits == 0 {
            return Ok(self.make_bigint(0));
        }
        if let BigVal::Small(n) = x {
            if bits <= 126 {
                return Ok(self.make_bigint(bigint_as_intn(bits as u32, n)));
            }
            if bits >= 128 {
                // Any i128 already fits a signed 128-bit (or wider) range.
                return Ok(self.make_bigint(n));
            }
        }
        let big = x.to_num();
        // Already in range (magnitude below 2^(bits-1)) — unchanged, no 2^bits
        // allocation (covers huge `bits` with any in-range value).
        if big.bits() < bits {
            return Ok(self.make_bigint_num(big));
        }
        if bits > MAX_BIGINT_BITS {
            return Err(Thrown(BIGINT_SIZE_ERR.into()));
        }
        let m = NumBig::one() << (bits as usize);
        let mut r = &big % &m;
        if r.sign() == Sign::Minus {
            r += &m;
        }
        if r >= (&m >> 1) {
            r -= m;
        }
        Ok(self.make_bigint_num(r))
    }

    /// `BigInt.asUintN(bits, x)`: x mod 2^bits (non-negative).
    pub(crate) fn bigint_as_uintn_val(&mut self, bits: u64, x: BigVal) -> Result<Value, Thrown> {
        if bits == 0 {
            return Ok(self.make_bigint(0));
        }
        if let BigVal::Small(n) = x {
            if bits <= 126 {
                return Ok(self.make_bigint(bigint_as_uintn(bits as u32, n)));
            }
            if n >= 0 {
                // bits ≥ 127 and 0 ≤ n < 2^127 ≤ 2^bits — unchanged.
                return Ok(self.make_bigint(n));
            }
        }
        let big = x.to_num();
        // Non-negative and already below 2^bits — unchanged, no allocation.
        if big.sign() != Sign::Minus && big.bits() <= bits {
            return Ok(self.make_bigint_num(big));
        }
        // A negative value maps to 2^bits + x: refuse absurd widths (V8 throws
        // for e.g. BigInt.asUintN(2**32, -1n) rather than building 2^(2^32)).
        if bits > MAX_BIGINT_BITS {
            return Err(Thrown(BIGINT_SIZE_ERR.into()));
        }
        let m = NumBig::one() << (bits as usize);
        let mut r = &big % &m;
        if r.sign() == Sign::Minus {
            r += &m;
        }
        Ok(self.make_bigint_num(r))
    }
}
