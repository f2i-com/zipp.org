//! NaN-boxed dynamic value.
//!
//! A `Value` is a single `u64`. IEEE-754 doubles occupy their natural bit
//! pattern; every non-double payload hides inside the quiet-NaN space
//! (`0x7FF8_…`). This is the same representation strategy V8/JSC/SpiderMonkey
//! use, and it is the foundation of the "unboxed in registers" goal: an i32 or
//! a bool is a plain machine word the JIT can hold in a register and only
//! re-box at a boundary that needs the dynamic form.
//!
//! ## Encoding
//!
//! * **Double**: any `u64` whose bits are NOT in the quiet-NaN range. We store
//!   doubles by their raw bits. Real arithmetic NaN is canonicalised to one
//!   pattern so it never collides with a tagged payload.
//! * **Tagged**: top 16 bits = `0x7FFC..=0x7FFF` selects a tag; the low 48 bits
//!   carry the payload (an i32, a bool, or a heap index).
//!
//! Tagged values live in the high-16-bit patterns `0x7FF9..=0x7FFD`:
//!
//! | top16  | tag         | payload (low 48 bits)        |
//! |--------|-------------|------------------------------|
//! | 0x7FF9 | `Int`       | i32 (zero-extended)          |
//! | 0x7FFA | `Bool`      | 0 or 1                       |
//! | 0x7FFB | `Null`      | 0                            |
//! | 0x7FFB | small BigInt| bit 47 set; bits 0..47 an i47 |
//! | 0x7FFC | `Undefined` | 0                            |
//! | 0x7FFD | `Heap`      | u32 index into the heap      |
//!
//! These five patterns are all quiet-NaNs with a nonzero tag selector, so no
//! finite double or ±∞ ever lands there. Real arithmetic NaN is canonicalised
//! to `0x7FF8…` (tag selector 0), which decodes as a double — never a tag.
//!
//! Numbers that fit in i32 are stored as `Int` so integer arithmetic stays in
//! the integer domain (cheap, exact, and what hot loops/recursion use); any
//! other number is a `Double`.
//!
//! ## Small BigInts
//!
//! A BigInt in `[SMALL_BIGINT_MIN, SMALL_BIGINT_MAX]` (a signed 47-bit value,
//! about ±7.0e13) is an IMMEDIATE: the Null tag's pattern with payload bit 47
//! set and the value's two's complement in bits 0..47. `null` itself is the
//! Null tag with a zero payload, which never has bit 47 set, and `is_null` /
//! `is_nullish` compare the whole word, so the two cannot be confused. Living
//! inside an existing tag keeps every "is this a double" range check (here
//! and in the JIT's emitted code) exactly as it was: an immediate BigInt is a
//! tagged non-number, non-heap value, which is what it is.
//!
//! CANONICAL: a BigInt whose value is in that range is ALWAYS the immediate
//! (`Vm::make_bigint` is the one producer); `HeapObj::BigInt` holds only
//! values outside it. So two BigInts are equal iff their bits are equal when
//! either one is an immediate, and `===`, SameValue, Map/Set keys and hashing
//! by bits are exact for them.

/// Quiet-NaN base: sign=0, all exponent bits set, top mantissa bit set.
const QNAN: u64 = 0x7FF8_0000_0000_0000;
/// Shift to the tag field (bits 48..).
const TAG_SHIFT: u64 = 48;
/// Mask selecting the 48-bit payload.
const PAYLOAD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

// Full tagged values: `QNAN | (tag << 48) | payload`. The tag occupies the 3
// bits just above the quiet-NaN bit, giving the top-16 patterns documented
// above.
const TAG_INT: u64 = QNAN | (1 << TAG_SHIFT); // 0x7FF9…
const TAG_BOOL: u64 = QNAN | (2 << TAG_SHIFT); // 0x7FFA…
const TAG_NULL: u64 = QNAN | (3 << TAG_SHIFT); // 0x7FFB…
const TAG_UNDEFINED: u64 = QNAN | (4 << TAG_SHIFT); // 0x7FFC…
const TAG_HEAP: u64 = QNAN | (5 << TAG_SHIFT); // 0x7FFD…
/// A small BigInt: the Null tag with payload bit 47 set (see the module doc).
const TAG_SMALL_BIGINT: u64 = TAG_NULL | (1 << 47); // 0x7FFB_8…
/// The bits that select a small BigInt: the tag and the marker bit.
const SMALL_BIGINT_MASK: u64 = 0xFFFF_8000_0000_0000;
/// The small BigInt payload (its value's low 47 bits).
const SMALL_BIGINT_PAYLOAD: u64 = 0x0000_7FFF_FFFF_FFFF;
/// The range a small (immediate) BigInt covers: a signed 47-bit value.
pub const SMALL_BIGINT_MIN: i64 = -(1 << 46);
pub const SMALL_BIGINT_MAX: i64 = (1 << 46) - 1;

/// Mask covering the full top 16 bits — selects the tag pattern. A tagged
/// value's `bits & TAG_MASK` equals its `TAG_*` constant exactly.
const TAG_MASK: u64 = 0xFFFF_0000_0000_0000;

/// Lowest / highest top-16 pattern that denotes a tag. A value is a double iff
/// its top 16 bits fall outside this inclusive range.
const TAG_LO: u64 = 0x7FF9;
const TAG_HI: u64 = 0x7FFD;

/// A NaN-boxed JavaScript value. `Copy` and exactly 8 bytes.
///
/// `repr(transparent)`: the register file and every emitted-code window are
/// addressed as raw `u64` slots, and the B257 finalize helper reads a window
/// range as a `&[Value]` — the layout identity is a declared guarantee, not
/// an accident of a single-field struct.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Value(u64);

impl Value {
    pub const UNDEFINED: Value = Value(TAG_UNDEFINED);
    /// A sentinel for a global slot that was reserved (the name is referenced)
    /// but never declared/assigned — reading it throws ReferenceError (the JS
    /// "x is not defined"). Shares the UNDEFINED tag with a payload bit, so if it
    /// ever escapes a guard it degrades to undefined-ish rather than a wild NaN.
    /// `is_undefined()`/`is_nullish()` use exact bit compares, so this is distinct.
    pub const UNINITIALIZED: Value = Value(TAG_UNDEFINED | 1);
    /// An INTERNAL-ONLY sentinel for an array HOLE — an absent element of a sparse
    /// array (`[1,,3]`, a deleted index, a length-extended tail). Distinct from
    /// `undefined` so `HasProperty`/iteration can tell an absent index from a
    /// present `undefined` one. Like UNINITIALIZED it shares the UNDEFINED tag with
    /// a payload bit, so a stray escape degrades to undefined-ish; but it must NEVER
    /// reach user code — every read of an array slot maps a hole to `undefined` (or
    /// a prototype lookup). `is_undefined()` uses an exact bit compare, so a hole is
    /// not `undefined`.
    pub const HOLE: Value = Value(TAG_UNDEFINED | 2);
    pub const NULL: Value = Value(TAG_NULL);
    pub const TRUE: Value = Value(TAG_BOOL | 1);
    pub const FALSE: Value = Value(TAG_BOOL);

    #[inline(always)]
    pub fn from_bits(bits: u64) -> Value {
        Value(bits)
    }

    #[inline(always)]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[inline(always)]
    pub fn int(v: i32) -> Value {
        Value(TAG_INT | (v as u32 as u64))
    }

    #[inline(always)]
    pub fn bool(b: bool) -> Value {
        Value(TAG_BOOL | b as u64)
    }

    /// The immediate BigInt `v`, or `None` outside [`SMALL_BIGINT_MIN`,
    /// `SMALL_BIGINT_MAX`] (such a value is a heap BigInt). Only
    /// `Vm::make_bigint` should call this: it is what keeps BigInts canonical.
    #[inline(always)]
    pub fn small_bigint(v: i128) -> Option<Value> {
        if (SMALL_BIGINT_MIN as i128..=SMALL_BIGINT_MAX as i128).contains(&v) {
            Some(Value(TAG_SMALL_BIGINT | (v as i64 as u64 & SMALL_BIGINT_PAYLOAD)))
        } else {
            None
        }
    }

    /// Whether this is an immediate (small) BigInt.
    #[inline(always)]
    pub fn is_small_bigint(self) -> bool {
        (self.0 & SMALL_BIGINT_MASK) == TAG_SMALL_BIGINT
    }

    /// The value of an immediate BigInt (only valid when `is_small_bigint`).
    #[inline(always)]
    pub fn as_small_bigint(self) -> i64 {
        ((self.0 << 17) as i64) >> 17
    }

    /// The value of an immediate BigInt, `None` for anything else.
    #[inline(always)]
    pub fn small_bigint_val(self) -> Option<i64> {
        self.is_small_bigint().then(|| self.as_small_bigint())
    }

    #[inline(always)]
    pub fn heap(idx: u32) -> Value {
        Value(TAG_HEAP | idx as u64)
    }

    /// Box an `f64`, narrowing to `Int` when it is an exact i32. NaN is
    /// canonicalised so it never aliases a tagged payload.
    #[inline(always)]
    pub fn num(n: f64) -> Value {
        // Exact-integer narrowing: keeps hot integer code in the int domain.
        // EXCEPT negative zero — narrowing -0.0 to int 0 would lose its sign
        // (breaking `1/-0` === -Infinity, `Object.is(-0,+0)`, SameValue, etc.), so
        // -0.0 is kept as a double. (+0.0 still narrows: the `== 0.0` short-circuits
        // for non-zero integers, so this only costs a sign check at exactly zero.)
        if n.fract() == 0.0
            && n >= i32::MIN as f64
            && n <= i32::MAX as f64
            && !(n == 0.0 && n.is_sign_negative())
        {
            return Value::int(n as i32);
        }
        if n.is_nan() {
            return Value(QNAN); // canonical NaN double (no tag bits set)
        }
        Value(n.to_bits())
    }

    #[inline(always)]
    pub fn is_double(self) -> bool {
        // A double is any value whose top 16 bits are outside the tag range
        // [0x7FF9, 0x7FFD]. Finite doubles, ±∞, and canonical NaN (0x7FF8…)
        // all satisfy this; the five tag patterns do not.
        let hi = self.0 >> 48;
        !(TAG_LO..=TAG_HI).contains(&hi)
    }

    #[inline(always)]
    pub fn is_int(self) -> bool {
        (self.0 & TAG_MASK) == TAG_INT
    }

    #[inline(always)]
    pub fn is_bool(self) -> bool {
        (self.0 & TAG_MASK) == TAG_BOOL
    }

    #[inline(always)]
    pub fn is_heap(self) -> bool {
        (self.0 & TAG_MASK) == TAG_HEAP
    }

    #[inline(always)]
    pub fn is_null(self) -> bool {
        self.0 == TAG_NULL
    }

    #[inline(always)]
    pub fn is_undefined(self) -> bool {
        self.0 == TAG_UNDEFINED
    }

    /// The never-declared-global sentinel (see `UNINITIALIZED`).
    #[inline(always)]
    pub fn is_uninitialized(self) -> bool {
        self.0 == Value::UNINITIALIZED.0
    }

    /// The internal array-hole sentinel (see `HOLE`).
    #[inline(always)]
    pub fn is_hole(self) -> bool {
        self.0 == Value::HOLE.0
    }

    #[inline(always)]
    pub fn is_nullish(self) -> bool {
        self.0 == TAG_NULL || self.0 == TAG_UNDEFINED
    }

    /// Interpret as i32 (only valid when `is_int`).
    #[inline(always)]
    pub fn as_int(self) -> i32 {
        (self.0 & PAYLOAD_MASK) as u32 as i32
    }

    /// Interpret as bool (only valid when `is_bool`).
    #[inline(always)]
    pub fn as_bool(self) -> bool {
        (self.0 & 1) != 0
    }

    /// Heap index (only valid when `is_heap`).
    #[inline(always)]
    pub fn heap_index(self) -> u32 {
        (self.0 & PAYLOAD_MASK) as u32
    }

    /// Numeric view: `Int` widens to f64, `Double` reads its bits. Callers
    /// guard with `is_number` first; this returns NaN for non-numbers.
    #[inline(always)]
    pub fn as_f64(self) -> f64 {
        if self.is_int() {
            self.as_int() as f64
        } else if self.is_double() {
            f64::from_bits(self.0)
        } else {
            f64::NAN
        }
    }

    #[inline(always)]
    pub fn is_number(self) -> bool {
        self.is_int() || self.is_double()
    }

    /// JS truthiness for the primitive cases the VM handles inline. Heap
    /// values (strings/objects) are resolved by the VM, which knows the heap.
    #[inline(always)]
    pub fn truthy_primitive(self) -> Option<bool> {
        if self.is_int() {
            Some(self.as_int() != 0)
        } else if self.is_bool() {
            Some(self.as_bool())
        } else if self.is_nullish() {
            Some(false)
        } else if self.is_small_bigint() {
            Some(self.as_small_bigint() != 0)
        } else if self.is_double() {
            let d = f64::from_bits(self.0);
            Some(d != 0.0 && !d.is_nan())
        } else {
            None // heap — VM decides
        }
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_int() {
            write!(f, "Int({})", self.as_int())
        } else if self.is_bool() {
            write!(f, "Bool({})", self.as_bool())
        } else if self.is_null() {
            write!(f, "Null")
        } else if self.is_undefined() {
            write!(f, "Undefined")
        } else if self.is_heap() {
            write!(f, "Heap({})", self.heap_index())
        } else if self.is_small_bigint() {
            write!(f, "BigInt({})", self.as_small_bigint())
        } else {
            write!(f, "Double({})", f64::from_bits(self.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_bigint_roundtrip() {
        for v in [0i128, 1, -1, 7, -7, 1 << 40, -(1 << 40), SMALL_BIGINT_MIN as i128, SMALL_BIGINT_MAX as i128] {
            let x = Value::small_bigint(v).unwrap();
            assert!(x.is_small_bigint());
            assert_eq!(x.as_small_bigint() as i128, v);
            assert!(!x.is_null() && !x.is_nullish() && !x.is_undefined());
            assert!(!x.is_double() && !x.is_int() && !x.is_bool() && !x.is_heap() && !x.is_number());
            assert_eq!(x.truthy_primitive(), Some(v != 0));
        }
        assert!(Value::small_bigint(SMALL_BIGINT_MAX as i128 + 1).is_none());
        assert!(Value::small_bigint(SMALL_BIGINT_MIN as i128 - 1).is_none());
        assert!(!Value::NULL.is_small_bigint());
        assert!(!Value::num(f64::NAN).is_small_bigint());
        assert_ne!(Value::small_bigint(0).unwrap(), Value::NULL);
    }

    #[test]
    fn int_roundtrip() {
        for v in [0i32, 1, -1, 42, i32::MAX, i32::MIN, -123456] {
            let val = Value::int(v);
            assert!(val.is_int(), "{v} should be int");
            assert_eq!(val.as_int(), v);
            assert!(val.is_number());
            assert_eq!(val.as_f64(), v as f64);
        }
    }

    #[test]
    fn bool_roundtrip() {
        assert!(Value::TRUE.is_bool());
        assert!(Value::FALSE.is_bool());
        assert_eq!(Value::TRUE.as_bool(), true);
        assert_eq!(Value::FALSE.as_bool(), false);
        assert_eq!(Value::bool(true), Value::TRUE);
        assert_eq!(Value::bool(false), Value::FALSE);
    }

    #[test]
    fn null_undefined_distinct() {
        assert!(Value::NULL.is_null());
        assert!(Value::UNDEFINED.is_undefined());
        assert!(Value::NULL.is_nullish());
        assert!(Value::UNDEFINED.is_nullish());
        assert_ne!(Value::NULL, Value::UNDEFINED);
        assert!(!Value::NULL.is_int());
        assert!(!Value::NULL.is_double());
    }

    #[test]
    fn heap_roundtrip() {
        for idx in [0u32, 1, 1000, u32::MAX, 0x00FF_FF00] {
            let val = Value::heap(idx);
            assert!(val.is_heap(), "{idx} should be heap");
            assert_eq!(val.heap_index(), idx);
            assert!(!val.is_number());
        }
    }

    #[test]
    fn double_roundtrip() {
        for d in [
            0.5f64,
            -1.5,
            3.14159,
            1e300,
            -1e-300,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            let val = Value::num(d);
            assert!(val.is_double(), "{d} should be double");
            assert!(val.is_number());
            assert_eq!(val.as_f64(), d);
        }
    }

    #[test]
    fn num_narrows_integers_to_int() {
        let v = Value::num(42.0);
        assert!(v.is_int());
        assert_eq!(v.as_int(), 42);
        // Non-integer stays double.
        let v = Value::num(42.5);
        assert!(v.is_double());
        assert_eq!(v.as_f64(), 42.5);
    }

    #[test]
    fn nan_canonical_not_tagged() {
        let v = Value::num(f64::NAN);
        assert!(v.is_double());
        assert!(v.as_f64().is_nan());
        // Must not be mistaken for any tagged value.
        assert!(!v.is_int());
        assert!(!v.is_bool());
        assert!(!v.is_heap());
        assert!(!v.is_null());
        assert!(!v.is_undefined());
    }

    #[test]
    fn large_doubles_not_tagged_as_other() {
        // A double whose high bits happen to look NaN-ish must still decode as
        // a double and not as a tag.
        let v = Value::num(1.7976931348623157e308); // near f64::MAX
        assert!(v.is_double());
        assert!(!v.is_heap());
    }
}
