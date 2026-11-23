use crate::insn::MAX_CHAR_SET_LENGTH;
use core::fmt;
extern crate memchr;

/// Facilities for searching bytes.
pub trait ByteSearcher {
    /// Search for ourselves in a slice of bytes.
    /// The length of the slice is unspecified and may be 0.
    /// \return the next index of ourselves in the slice, or None.
    fn find_in(&self, rhs: &[u8]) -> Option<usize>;
}

impl ByteSearcher for [u8; 1] {
    #[inline(always)]
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr(self[0], rhs)
    }
}

impl ByteSearcher for [u8; 2] {
    #[inline(always)]
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr2(self[0], self[1], rhs)
    }
}

impl ByteSearcher for [u8; 3] {
    #[inline(always)]
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr3(self[0], self[1], self[2], rhs)
    }
}

impl ByteSearcher for memchr::memmem::Finder<'_> {
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        self.find(rhs)
    }
}

/// A ByteSet is any set of bytes.
pub trait ByteSet {
    /// \return whether the ByteSet contains the byte.
    fn contains(&self, b: u8) -> bool;
}

/// A ByteArraySet wraps a small array and uses linear equality.
#[derive(Copy, Clone, Debug)]
#[repr(align(4))]
pub struct ByteArraySet<ArraySet: SmallArraySet>(pub ArraySet);

/// Cover over contains() to avoid bumping into native contains call.
impl<ArraySet: SmallArraySet> ByteArraySet<ArraySet> {
    #[inline(always)]
    pub fn contains(self, b: u8) -> bool {
        self.0.contains(b)
    }
}

impl<ArraySet: SmallArraySet> ByteSearcher for ByteArraySet<ArraySet> {
    #[inline(always)]
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        self.0.find_in(rhs)
    }
}

/// A SmallArraySet is a set implemented as a small byte array.
pub trait SmallArraySet: Copy {
    fn contains(self, b: u8) -> bool;

    fn find_in(self, rhs: &[u8]) -> Option<usize>;
}

// Beware: Rust is cranky about loop unrolling.
// Do not try to be too clever here.
impl SmallArraySet for [u8; 2] {
    #[inline(always)]
    fn contains(self, b: u8) -> bool {
        b == self[0] || b == self[1]
    }

    #[inline(always)]
    fn find_in(self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr2(self[0], self[1], rhs)
    }
}
impl SmallArraySet for [u8; 3] {
    #[inline(always)]
    fn contains(self, b: u8) -> bool {
        b == self[0] || b == self[1] || b == self[2]
    }

    #[inline(always)]
    fn find_in(self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr3(self[0], self[1], self[2], rhs)
    }
}
impl SmallArraySet for [u8; 4] {
    #[inline(always)]
    fn contains(self, b: u8) -> bool {
        b == self[0] || b == self[1] || b == self[2] || b == self[3]
    }

    #[inline(always)]
    fn find_in(self, rhs: &[u8]) -> Option<usize> {
        #[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
        {
            if rhs.len() >= 16 {
                return unsafe { find_in_4_sse2(self, rhs) };
            }
        }
        for (idx, &byte) in rhs.iter().enumerate() {
            if self.contains(byte) {
                return Some(idx);
            }
        }
        None
    }
}

#[cfg(all(target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
#[inline]
unsafe fn find_in_4_sse2(needles: [u8; 4], rhs: &[u8]) -> Option<usize> {
    use core::arch::x86_64::*;
    unsafe {
        let n0 = _mm_set1_epi8(needles[0] as i8);
        let n1 = _mm_set1_epi8(needles[1] as i8);
        let n2 = _mm_set1_epi8(needles[2] as i8);
        let n3 = _mm_set1_epi8(needles[3] as i8);
        let ptr = rhs.as_ptr();
        let len = rhs.len();
        let mut off = 0usize;
        loop {
            let v = _mm_loadu_si128(ptr.add(off) as *const __m128i);
            let m0 = _mm_cmpeq_epi8(v, n0);
            let m1 = _mm_cmpeq_epi8(v, n1);
            let m2 = _mm_cmpeq_epi8(v, n2);
            let m3 = _mm_cmpeq_epi8(v, n3);
            let hit = _mm_or_si128(_mm_or_si128(m0, m1), _mm_or_si128(m2, m3));
            let mask = _mm_movemask_epi8(hit) as u32;
            if mask != 0 {
                return Some(off + mask.trailing_zeros() as usize);
            }
            off += 16;
            if off >= len {
                return None;
            }
            if off + 16 > len {
                off = len - 16;
            }
        }
    }
}

// CharSet helper. Avoid branching in the loop to get good unrolling.
#[allow(unused_parens)]
#[inline(always)]
pub fn charset_contains(set: &[u32; MAX_CHAR_SET_LENGTH], c: u32) -> bool {
    let mut result = false;
    for &v in set.iter() {
        result |= (v == c);
    }
    result
}

/// A helper function for formatting bitmaps, using - ranges.
fn format_bitmap<Func>(name: &str, f: &mut fmt::Formatter<'_>, contains: Func) -> fmt::Result
where
    Func: Fn(u8) -> bool,
{
    write!(f, "{}[", name)?;
    let mut idx = 0;
    let mut maybe_space = "";
    while idx <= 256 {
        // Compute the next value not contained.
        let mut end = idx;
        while end <= 256 && contains(end as u8) {
            end += 1;
        }
        match end - idx {
            0 => (),
            1 => write!(f, "{}{}", maybe_space, idx)?,
            _ => write!(f, "{}{}-{}", maybe_space, idx, end - 1)?,
        };
        if end > idx {
            maybe_space = " ";
        }
        idx = end + 1
    }
    write!(f, "]")?;
    Ok(())
}

/// A bitmap covering ASCII characters.
#[derive(Default, Copy, Clone)]
#[repr(align(4))]
pub struct AsciiBitmap(pub [u8; 16]);

impl AsciiBitmap {
    /// Set a byte val in this bitmap.
    #[inline(always)]
    pub fn set(&mut self, val: u8) {
        debug_assert!(val <= 127, "Value should be ASCII");
        self.0[(val >> 3) as usize] |= 1 << (val & 0x7);
    }
}

impl fmt::Debug for AsciiBitmap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_bitmap("AsciiBitmap", f, |v| self.contains(v))
    }
}

impl ByteSet for AsciiBitmap {
    /// \return whether this bitmap contains a given value.
    /// The value does NOT have to be ASCII.
    #[inline(always)]
    fn contains(&self, val: u8) -> bool {
        // Delicate tricks to avoid branches.
        // In general we want to compute the byte via /8, and then mask into the
        // byte. But if the value is not ASCII then the byte could be too large.
        // So mask off the MSB so that the byte is always in range.
        let byte = (val & 0x7F) >> 3;
        let bit = val & 0x7;

        // Now probe the bitmap. If our sign bit was set, we want the mask to be 0;
        // otherwise we want to set only the 'bit' offset.
        // Invert the sign bit and reuse it.
        let mask = ((val >> 7) ^ 1) << bit;

        // Probe the bitmap. We expect the compiler to elide the bounds check.
        (self.0[byte as usize] & mask) != 0
    }
}

/// A bitmap covering all bytes.
/// Stored as two 16-byte nibble lookup tables so SSSE3/AVX2 `pshufb` search
/// reads the tables directly with zero setup cost:
/// - `self.0[lo]` (`lo = val & 0xF`) holds bit `1 << ((val >> 4) & 7)` for `val < 128`.
/// - `self.0[16 + lo]` holds bit `1 << ((val >> 4) & 7)` for `val >= 128`.
#[derive(Default, Copy, Clone, PartialEq, Eq)]
#[repr(align(16))]
pub struct ByteBitmap([u8; 32]);

impl ByteBitmap {
    /// Construct from a sequence of bytes.
    pub fn new(bytes: &[u8]) -> ByteBitmap {
        let mut bb = ByteBitmap::default();
        for &b in bytes {
            bb.set(b)
        }
        bb
    }

    /// \return whether this bitmap contains a given byte val.
    #[inline(always)]
    pub fn contains(&self, val: u8) -> bool {
        let idx = ((val & 0x80) >> 3) | (val & 0x0F);
        let bit = (val >> 4) & 0x7;
        (self.0[idx as usize] & (1 << bit)) != 0
    }

    /// Set a bit in this bitmap.
    #[inline(always)]
    pub fn set(&mut self, val: u8) {
        let idx = ((val & 0x80) >> 3) | (val & 0x0F);
        let bit = (val >> 4) & 0x7;
        self.0[idx as usize] |= 1 << bit;
    }

    /// Set every byte in `lo..=hi`.
    pub fn set_range(&mut self, lo: u8, hi: u8) {
        debug_assert!(lo <= hi);
        for b in lo..=hi {
            self.set(b);
        }
    }

    /// Update ourselves from another bitmap, in place.
    pub fn bitor(&mut self, rhs: &ByteBitmap) {
        for idx in 0..self.0.len() {
            self.0[idx] |= rhs.0[idx];
        }
    }

    /// Invert our bits, in place.
    pub fn bitnot(&mut self) -> &mut Self {
        for val in self.0.iter_mut() {
            *val = !*val;
        }
        self
    }

    /// Count number of set bits.
    pub fn count_bits(&self) -> u32 {
        self.0.iter().map(|v| v.count_ones()).sum()
    }

    /// True if no non-ASCII byte (`0x80..=0xFF`) is set in this bitmap.
    #[cfg(all(feature = "std", target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
    #[inline(always)]
    fn is_ascii_only(&self) -> bool {
        let hi0 = u64::from_ne_bytes(self.0[16..24].try_into().unwrap());
        let hi1 = u64::from_ne_bytes(self.0[24..32].try_into().unwrap());
        (hi0 | hi1) == 0
    }

    /// Return ourselves as an array of a fixed length.
    /// Panics if the array is not large enough.
    #[allow(clippy::wrong_self_convention)]
    #[inline(always)]
    pub fn as_array<const N: usize>(&self) -> [u8; N] {
        let mut array = [0u8; N];
        let mut idx = 0;
        for byte in 0..=255 {
            if self.contains(byte) {
                array[idx] = byte;
                idx += 1;
            }
        }
        array
    }

    #[inline(always)]
    fn scalar_find_in(&self, bytes: &[u8]) -> Option<usize> {
        for (idx, &byte) in bytes.iter().enumerate() {
            if self.contains(byte) {
                return Some(idx);
            }
        }
        None
    }

    #[cfg(all(feature = "std", target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
    #[target_feature(enable = "ssse3")]
    unsafe fn find_in_ssse3(&self, bytes: &[u8]) -> Option<usize> {
        use core::arch::x86_64::*;
        const BIT_LO: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 0, 0, 0, 0, 0, 0, 0, 0];
        const BIT_HI: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 4, 8, 16, 32, 64, 128];
        unsafe {
            let tbl_lo = _mm_load_si128(self.0.as_ptr() as *const __m128i);
            let bit_lo = _mm_loadu_si128(BIT_LO.as_ptr() as *const __m128i);
            let mask_0f = _mm_set1_epi8(0x0F);
            let zero = _mm_setzero_si128();
            let ptr = bytes.as_ptr();
            let len = bytes.len();
            let mut off = 0usize;
            if self.is_ascii_only() {
                loop {
                    let v = _mm_loadu_si128(ptr.add(off) as *const __m128i);
                    let lo = _mm_and_si128(v, mask_0f);
                    let hi = _mm_and_si128(_mm_srli_epi16(v, 4), mask_0f);
                    let hit = _mm_and_si128(
                        _mm_shuffle_epi8(tbl_lo, lo),
                        _mm_shuffle_epi8(bit_lo, hi),
                    );
                    let mask = (!(_mm_movemask_epi8(_mm_cmpeq_epi8(hit, zero)) as u32)) & 0xFFFF;
                    if mask != 0 {
                        return Some(off + mask.trailing_zeros() as usize);
                    }
                    off += 16;
                    if off >= len {
                        return None;
                    }
                    if off + 16 > len {
                        off = len - 16;
                    }
                }
            } else {
                let tbl_hi = _mm_load_si128(self.0.as_ptr().add(16) as *const __m128i);
                let bit_hi = _mm_loadu_si128(BIT_HI.as_ptr() as *const __m128i);
                loop {
                    let v = _mm_loadu_si128(ptr.add(off) as *const __m128i);
                    let lo = _mm_and_si128(v, mask_0f);
                    let hi = _mm_and_si128(_mm_srli_epi16(v, 4), mask_0f);
                    let hit_lo = _mm_and_si128(
                        _mm_shuffle_epi8(tbl_lo, lo),
                        _mm_shuffle_epi8(bit_lo, hi),
                    );
                    let hit_hi = _mm_and_si128(
                        _mm_shuffle_epi8(tbl_hi, lo),
                        _mm_shuffle_epi8(bit_hi, hi),
                    );
                    let hit = _mm_or_si128(hit_lo, hit_hi);
                    let mask = (!(_mm_movemask_epi8(_mm_cmpeq_epi8(hit, zero)) as u32)) & 0xFFFF;
                    if mask != 0 {
                        return Some(off + mask.trailing_zeros() as usize);
                    }
                    off += 16;
                    if off >= len {
                        return None;
                    }
                    if off + 16 > len {
                        off = len - 16;
                    }
                }
            }
        }
    }

    #[cfg(all(feature = "std", target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
    #[target_feature(enable = "avx2")]
    unsafe fn find_in_avx2(&self, bytes: &[u8]) -> Option<usize> {
        use core::arch::x86_64::*;
        const BIT_LO: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 0, 0, 0, 0, 0, 0, 0, 0];
        const BIT_HI: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 4, 8, 16, 32, 64, 128];
        unsafe {
            let tbl_lo = _mm256_broadcastsi128_si256(_mm_load_si128(self.0.as_ptr() as *const __m128i));
            let bit_lo = _mm256_broadcastsi128_si256(_mm_loadu_si128(BIT_LO.as_ptr() as *const __m128i));
            let mask_0f = _mm256_set1_epi8(0x0F);
            let zero = _mm256_setzero_si256();
            let ptr = bytes.as_ptr();
            let len = bytes.len();
            let mut off = 0usize;
            if self.is_ascii_only() {
                loop {
                    let v = _mm256_loadu_si256(ptr.add(off) as *const __m256i);
                    let lo = _mm256_and_si256(v, mask_0f);
                    let hi = _mm256_and_si256(_mm256_srli_epi16(v, 4), mask_0f);
                    let hit = _mm256_and_si256(
                        _mm256_shuffle_epi8(tbl_lo, lo),
                        _mm256_shuffle_epi8(bit_lo, hi),
                    );
                    let mask = !(_mm256_movemask_epi8(_mm256_cmpeq_epi8(hit, zero)) as u32);
                    if mask != 0 {
                        return Some(off + mask.trailing_zeros() as usize);
                    }
                    off += 32;
                    if off >= len {
                        return None;
                    }
                    if off + 32 > len {
                        off = len - 32;
                    }
                }
            } else {
                let tbl_hi = _mm256_broadcastsi128_si256(_mm_load_si128(
                    self.0.as_ptr().add(16) as *const __m128i,
                ));
                let bit_hi =
                    _mm256_broadcastsi128_si256(_mm_loadu_si128(BIT_HI.as_ptr() as *const __m128i));
                loop {
                    let v = _mm256_loadu_si256(ptr.add(off) as *const __m256i);
                    let lo = _mm256_and_si256(v, mask_0f);
                    let hi = _mm256_and_si256(_mm256_srli_epi16(v, 4), mask_0f);
                    let hit_lo = _mm256_and_si256(
                        _mm256_shuffle_epi8(tbl_lo, lo),
                        _mm256_shuffle_epi8(bit_lo, hi),
                    );
                    let hit_hi = _mm256_and_si256(
                        _mm256_shuffle_epi8(tbl_hi, lo),
                        _mm256_shuffle_epi8(bit_hi, hi),
                    );
                    let hit = _mm256_or_si256(hit_lo, hit_hi);
                    let mask = !(_mm256_movemask_epi8(_mm256_cmpeq_epi8(hit, zero)) as u32);
                    if mask != 0 {
                        return Some(off + mask.trailing_zeros() as usize);
                    }
                    off += 32;
                    if off >= len {
                        return None;
                    }
                    if off + 32 > len {
                        off = len - 32;
                    }
                }
            }
        }
    }
}

#[cfg(all(feature = "std", target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
#[inline(always)]
fn bitmap_simd_level() -> u8 {
    use core::sync::atomic::{AtomicU8, Ordering};
    // 0 = uninit, 1 = scalar, 2 = ssse3, 3 = avx2
    static LEVEL: AtomicU8 = AtomicU8::new(0);
    match LEVEL.load(Ordering::Relaxed) {
        0 => {
            let lvl = if std::is_x86_feature_detected!("avx2") {
                3
            } else if std::is_x86_feature_detected!("ssse3") {
                2
            } else {
                1
            };
            LEVEL.store(lvl, Ordering::Relaxed);
            lvl
        }
        lvl => lvl,
    }
}

impl ByteSearcher for ByteBitmap {
    #[inline(always)]
    fn find_in(&self, bytes: &[u8]) -> Option<usize> {
        #[cfg(all(feature = "std", target_arch = "x86_64", not(feature = "prohibit-unsafe")))]
        {
            if bytes.len() >= 16 {
                let lvl = bitmap_simd_level();
                if lvl == 3 && bytes.len() >= 32 {
                    return unsafe { self.find_in_avx2(bytes) };
                }
                if lvl >= 2 {
                    return unsafe { self.find_in_ssse3(bytes) };
                }
            }
        }
        self.scalar_find_in(bytes)
    }
}

impl fmt::Debug for ByteBitmap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_bitmap("ByteBitmap", f, |v| self.contains(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bitmap(bytes: &[u8]) -> ByteBitmap {
        let mut bm = ByteBitmap::default();
        for &b in bytes {
            bm.set(b)
        }
        bm
    }

    #[test]
    fn set_range_sets_exactly_the_range() {
        for lo in 0..=255u8 {
            for hi in lo..=255u8 {
                let mut expected = ByteBitmap::default();
                for b in lo..=hi {
                    expected.set(b);
                }
                let mut bm = ByteBitmap::default();
                bm.set_range(lo, hi);
                assert_eq!(bm, expected, "{lo}..={hi}");
            }
        }
    }

    #[test]
    fn empty_search() {
        use crate::insn::StartPredicate;
        assert_eq!(StartPredicate::Arbitrary.find_in(&[1, 2, 3]), Some(0));
        assert_eq!(StartPredicate::Arbitrary.find_in(&[]), Some(0));
    }

    #[test]
    fn bitmap_search() {
        assert_eq!(make_bitmap(&[]).find_in(&[1, 2, 3]), None);
        assert_eq!(make_bitmap(&[]).bitnot().find_in(&[1, 2, 3]), Some(0));
        assert_eq!(make_bitmap(&[1]).bitnot().find_in(&[1, 2, 3]), Some(1));
        assert_eq!(make_bitmap(&[2]).bitnot().find_in(&[1, 2, 3]), Some(0));
        assert_eq!(
            make_bitmap(&[4, 5, 6, 7]).find_in(&[8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]),
            None
        );
        assert_eq!(
            make_bitmap(&[4, 5, 6, 7]).find_in(&[8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]),
            None
        );
        assert_eq!(
            make_bitmap(&[4, 5, 6, 7])
                .find_in(&[8, 9, 10, 11, 12, 13, 4, 14, 6, 15, 7, 16, 17, 18, 19, 20]),
            Some(6)
        );
    }

    #[test]
    fn literal_search() {
        assert_eq!([0, 1, 2, 3].find_in(&[4, 5, 6, 7]), None);
        assert_eq!([0, 1, 2, 3].find_in(&[]), None);
        // Exercise SSE2 16-byte loop + overlapping tail across all lengths 0..64.
        let needles = [b'a', b'e', b'i', b'o'];
        for len in 0..=64usize {
            let mut buf = vec![b'x'; len];
            assert_eq!(needles.find_in(&buf), None);
            for pos in 0..len {
                buf[pos] = needles[pos & 3];
                assert_eq!(needles.find_in(&buf), Some(pos), "len={len} pos={pos}");
                buf[pos] = b'x';
            }
        }
    }

    #[test]
    fn bitmap_simd_exhaustive() {
        // Test every single byte 0..=255 in its own bitmap across lengths 0..72.
        for target in 0..=255u8 {
            let bm = make_bitmap(&[target]);
            let fill = target.wrapping_add(1);
            for len in [0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65] {
                let mut buf = vec![fill; len];
                assert_eq!(bm.find_in(&buf), None, "target={target} len={len}");
                if len > 0 {
                    for &pos in &[0, len / 2, len - 1] {
                        buf[pos] = target;
                        assert_eq!(
                            bm.find_in(&buf),
                            Some(pos),
                            "target={target} len={len} pos={pos}"
                        );
                        buf[pos] = fill;
                    }
                }
            }
        }
        // Test multi-byte ASCII and inverted (non-ASCII) bitmaps across all lengths 0..70.
        let ascii_bm = make_bitmap(b"0123456789");
        let mut non_ascii_bm = make_bitmap(b"abc");
        non_ascii_bm.set(0x80);
        non_ascii_bm.set(0xC3);
        non_ascii_bm.set(0xFF);
        for (bm, hit, miss) in [(&ascii_bm, b'7', b'z'), (&non_ascii_bm, 0xC3u8, b'z')] {
            for len in 0..=70usize {
                let mut buf = vec![miss; len];
                assert_eq!(bm.find_in(&buf), None);
                for pos in 0..len {
                    buf[pos] = hit;
                    assert_eq!(bm.find_in(&buf), Some(pos), "hit={hit} len={len} pos={pos}");
                    buf[pos] = miss;
                }
            }
        }
    }
}
