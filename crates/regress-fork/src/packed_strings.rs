//! Lossless storage for the Unicode properties of strings (`\p{RGI_Emoji}`
//! and the six sets it is made of).
//!
//! The generated tables are slices of slices: 7,906 fat pointers, 63 KB in a
//! 32-bit module, over 12,389 code points, and `RGI_Emoji` repeats the other
//! six sets entry for entry. Here the six sets share one pool of three-byte
//! code points and one length byte per string, both const-evaluated from the
//! generated literals, and `RGI_Emoji` is the whole pool. A set is decoded
//! only when the regex parser expands a property into its alternatives, which
//! previously copied each string out of the slice it was given.
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// The generated form of one property: its strings, each a slice of code points.
pub(crate) type Generated = &'static [&'static [u32]];

/// The number of strings in `sets`.
pub(crate) const fn count_strings(sets: &[Generated]) -> usize {
    let (mut total, mut i) = (0, 0);
    while i < sets.len() {
        total += sets[i].len();
        i += 1;
    }
    total
}

/// The number of code points in `sets`.
pub(crate) const fn count_code_points(sets: &[Generated]) -> usize {
    let (mut total, mut i) = (0, 0);
    while i < sets.len() {
        let mut j = 0;
        while j < sets[i].len() {
            total += sets[i][j].len();
            j += 1;
        }
        i += 1;
    }
    total
}

/// Each string's length in code points, set after set. `N` is
/// [`count_strings`] of `sets`.
pub(crate) const fn pack_lengths<const N: usize>(sets: &[Generated]) -> [u8; N] {
    let mut out = [0u8; N];
    let (mut at, mut i) = (0, 0);
    while i < sets.len() {
        let mut j = 0;
        while j < sets[i].len() {
            let length = sets[i][j].len();
            assert!(length > 0 && length <= u8::MAX as usize);
            out[at] = length as u8;
            at += 1;
            j += 1;
        }
        i += 1;
    }
    assert!(at == N);
    out
}

/// Every code point of `sets` as three little-endian bytes, string after
/// string. `N` is three times [`count_code_points`] of `sets`.
pub(crate) const fn pack_code_points<const N: usize>(sets: &[Generated]) -> [u8; N] {
    let mut out = [0u8; N];
    let (mut at, mut i) = (0, 0);
    while i < sets.len() {
        let mut j = 0;
        while j < sets[i].len() {
            let string = sets[i][j];
            let mut k = 0;
            while k < string.len() {
                let c = string[k];
                assert!(c <= 0x10FFFF);
                out[at] = c as u8;
                out[at + 1] = (c >> 8) as u8;
                out[at + 2] = (c >> 16) as u8;
                at += 3;
                k += 1;
            }
            j += 1;
        }
        i += 1;
    }
    assert!(at == N);
    out
}

/// Where each set starts, as (string index, code point index), with the end
/// of the last set as the final entry. `N` is one more than `sets.len()`.
pub(crate) const fn set_starts<const N: usize>(sets: &[Generated]) -> [(usize, usize); N] {
    assert!(N == sets.len() + 1);
    let mut out = [(0, 0); N];
    let (mut strings, mut code_points, mut i) = (0, 0, 0);
    while i < sets.len() {
        out[i] = (strings, code_points);
        let mut j = 0;
        while j < sets[i].len() {
            code_points += sets[i][j].len();
            j += 1;
        }
        strings += sets[i].len();
        i += 1;
    }
    out[i] = (strings, code_points);
    out
}

/// The strings of one property: a run of the pool.
#[derive(Clone, Copy)]
pub(crate) struct StringSet {
    lengths: &'static [u8],
    code_points: &'static [u8],
}

impl StringSet {
    /// The strings from `first` up to `last`, two entries of [`set_starts`].
    pub(crate) fn new(
        lengths: &'static [u8],
        code_points: &'static [u8],
        first: (usize, usize),
        last: (usize, usize),
    ) -> StringSet {
        StringSet {
            lengths: &lengths[first.0..last.0],
            code_points: &code_points[first.1 * 3..last.1 * 3],
        }
    }

    /// The number of strings.
    #[cfg(any(test, feature = "prohibit-unsafe"))]
    pub(crate) fn len(&self) -> usize {
        self.lengths.len()
    }

    /// The number of code points in all the strings.
    #[cfg(any(test, feature = "prohibit-unsafe"))]
    pub(crate) fn code_point_count(&self) -> usize {
        self.code_points.len() / 3
    }

    /// The strings, in the generated order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = PackedString> + use<> {
        let mut rest = self.code_points;
        self.lengths.iter().map(move |&length| {
            let (string, tail) = rest.split_at(usize::from(length) * 3);
            rest = tail;
            PackedString(string)
        })
    }
}

/// One string of a [`StringSet`].
#[derive(Clone, Copy)]
pub(crate) struct PackedString(&'static [u8]);

impl PackedString {
    /// Its code points.
    pub(crate) fn iter(&self) -> impl Iterator<Item = u32> + use<> {
        self.0
            .chunks_exact(3)
            .map(|c| u32::from(c[0]) | u32::from(c[1]) << 8 | u32::from(c[2]) << 16)
    }

    pub(crate) fn to_vec(self) -> Vec<u32> {
        self.iter().collect()
    }
}
