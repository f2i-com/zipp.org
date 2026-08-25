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

/// Mask covering the full top 16 bits — selects the tag pattern. A tagged
/// value's `bits & TAG_MASK` equals its `TAG_*` constant exactly.
const TAG_MASK: u64 = 0xFFFF_0000_0000_0000;

/// Lowest / highest top-16 pattern that denotes a tag. A value is a double iff
/// its top 16 bits fall outside this inclusive range.
const TAG_LO: u64 = 0x7FF9;
const TAG_HI: u64 = 0x7FFD;

/// A NaN-boxed JavaScript value. `Copy` and exactly 8 bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
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
    pub fn bits(self) -> u64 {
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
        } else {
            write!(f, "Double({})", f64::from_bits(self.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
