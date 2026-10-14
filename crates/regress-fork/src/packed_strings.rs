//! Lossless storage for the Unicode properties of strings (`\p{RGI_Emoji}`
//! and the six sets it is made of).
//!
//! The generated tables are slices of slices: 7,906 fat pointers, 63 KB in a
//! 32-bit module, over 12,389 code points, and `RGI_Emoji` repeats the other
//! six sets entry for entry. Here the six sets are one byte stream,
//! const-evaluated from the generated literals, and `RGI_Emoji` is the whole
//! stream. The strings of a set are sorted, and a string mostly differs from
//! the one before it in a code point or two (a skin tone, a gender), so each
//! is stored against the one before it ("front coding"): a header byte, the
//! length of the prefix the two share (high four bits) and the number of
//! code points after it (low four bits), then each of those code points as
//! the difference from the code point at the same place in the string before
//! (or, past that string's end, from the code point before it), zigzag-coded
//! in seven-bit groups. A set starts with [`SET_START`] and no string before.
//! The stream is 11.8 KB where three bytes a code point and a length byte a
//! string were 38 KB (compressed alone, 1.7 KB where they were 4.8). A set
//! is decoded only when the regex parser expands a property into its
//! alternatives, string after string, which is the only way it is read.
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// The generated form of one property: its strings, each a slice of code points.
pub(crate) type Generated = &'static [&'static [u32]];

/// The byte that starts a set: no header is 0xFF, since no string is longer
/// than fifteen code points.
const SET_START: u8 = 0xFF;

/// The length of the prefix `string` shares with `before`.
const fn shared_prefix(before: &[u32], string: &[u32]) -> usize {
    let mut k = 0;
    while k < before.len() && k < string.len() && before[k] == string[k] {
        k += 1;
    }
    k
}

/// What code point `j` of `string` is stored against.
const fn reference(before: &[u32], string: &[u32], j: usize) -> u32 {
    if j < before.len() {
        before[j]
    } else if j > 0 {
        string[j - 1]
    } else {
        0
    }
}

/// The zigzag code of `to - from`: small differences of either sign are
/// small numbers.
const fn zigzag(from: u32, to: u32) -> u32 {
    let d = to as i64 - from as i64;
    (if d >= 0 { d << 1 } else { ((-d) << 1) - 1 }) as u32
}

/// Encodes `sets` and returns the stream's length. `out` receives as much
/// of the stream as it holds (none, to measure), and `starts` the byte
/// offset of each set and of the end, as many as it holds.
const fn encode_into(sets: &[Generated], out: &mut [u8], starts: &mut [usize]) -> usize {
    let mut pos = 0;
    let mut i = 0;
    while i < sets.len() {
        if i < starts.len() {
            starts[i] = pos;
        }
        if pos < out.len() {
            out[pos] = SET_START;
        }
        pos += 1;
        let set = sets[i];
        let mut j = 0;
        while j < set.len() {
            let string = set[j];
            let before: &[u32] = if j > 0 { set[j - 1] } else { &[] };
            let k = shared_prefix(before, string);
            let n = string.len() - k;
            // The decoder keeps the string before in fifteen code points.
            assert!(string.len() <= 15 && (k << 4 | n) != SET_START as usize);
            if pos < out.len() {
                out[pos] = (k << 4 | n) as u8;
            }
            pos += 1;
            let mut c = k;
            while c < string.len() {
                assert!(string[c] <= 0x10FFFF);
                let mut z = zigzag(reference(before, string, c), string[c]);
                while z >= 128 {
                    if pos < out.len() {
                        out[pos] = (z as u8 & 127) | 128;
                    }
                    pos += 1;
                    z >>= 7;
                }
                if pos < out.len() {
                    out[pos] = z as u8;
                }
                pos += 1;
                c += 1;
            }
            j += 1;
        }
        i += 1;
    }
    if sets.len() < starts.len() {
        starts[sets.len()] = pos;
    }
    pos
}

/// The length of the stream of `sets`.
pub(crate) const fn encoded_len(sets: &[Generated]) -> usize {
    encode_into(sets, &mut [], &mut [])
}

/// The stream of `sets`. `N` is [`encoded_len`] of `sets`.
pub(crate) const fn encode<const N: usize>(sets: &[Generated]) -> [u8; N] {
    let mut out = [0u8; N];
    let len = encode_into(sets, &mut out, &mut []);
    assert!(len == N);
    out
}

/// Where each set starts, as (string index, code point index, byte offset in
/// the stream), with the end of the last set as the final entry. `N` is one
/// more than `sets.len()`.
pub(crate) const fn set_starts<const N: usize>(sets: &[Generated]) -> [(usize, usize, usize); N] {
    assert!(N == sets.len() + 1);
    let mut bytes = [0usize; N];
    encode_into(sets, &mut [], &mut bytes);
    let mut out = [(0, 0, 0); N];
    let (mut strings, mut code_points, mut i) = (0, 0, 0);
    while i < sets.len() {
        out[i] = (strings, code_points, bytes[i]);
        let mut j = 0;
        while j < sets[i].len() {
            code_points += sets[i][j].len();
            j += 1;
        }
        strings += sets[i].len();
        i += 1;
    }
    out[i] = (strings, code_points, bytes[i]);
    out
}

/// The strings of one property: a run of the sets.
#[derive(Clone, Copy)]
pub(crate) struct StringSet {
    bytes: &'static [u8],
    #[cfg(any(test, feature = "prohibit-unsafe"))]
    strings: usize,
    #[cfg(any(test, feature = "prohibit-unsafe"))]
    code_points: usize,
}

impl StringSet {
    /// The sets from `first` up to `last`, two entries of [`set_starts`].
    pub(crate) fn new(
        bytes: &'static [u8],
        first: (usize, usize, usize),
        last: (usize, usize, usize),
    ) -> StringSet {
        StringSet {
            bytes: bytes.get(first.2..last.2).unwrap_or_default(),
            #[cfg(any(test, feature = "prohibit-unsafe"))]
            strings: last.0 - first.0,
            #[cfg(any(test, feature = "prohibit-unsafe"))]
            code_points: last.1 - first.1,
        }
    }

    /// The number of strings.
    #[cfg(any(test, feature = "prohibit-unsafe"))]
    pub(crate) fn len(&self) -> usize {
        self.strings
    }

    /// The number of code points in all the strings.
    #[cfg(any(test, feature = "prohibit-unsafe"))]
    pub(crate) fn code_point_count(&self) -> usize {
        self.code_points
    }

    /// The strings, in the generated order.
    pub(crate) fn iter(&self) -> Strings {
        Strings {
            bytes: self.bytes,
            at: 0,
            before: PackedString {
                code_points: [0; 15],
                len: 0,
            },
        }
    }
}

/// The strings of a [`StringSet`], decoded one after another.
pub(crate) struct Strings {
    bytes: &'static [u8],
    at: usize,
    before: PackedString,
}

impl Iterator for Strings {
    type Item = PackedString;

    fn next(&mut self) -> Option<PackedString> {
        let mut header = *self.bytes.get(self.at)?;
        self.at += 1;
        if header == SET_START {
            self.before.len = 0;
            header = *self.bytes.get(self.at)?;
            self.at += 1;
        }
        let k = usize::from(header >> 4);
        let len = k + usize::from(header & 15);
        let before = self.before.code_points.get(..self.before.len)?;
        let mut string = self.before;
        string.len = len;
        let mut last = if k > 0 { *before.get(k - 1)? } else { 0 };
        for j in k..len {
            let (mut z, mut shift) = (0u32, 0u32);
            loop {
                let b = *self.bytes.get(self.at)?;
                self.at += 1;
                z |= u32::from(b & 127) << shift;
                if b < 128 {
                    break;
                }
                shift += 7;
                if shift > 21 {
                    return None;
                }
            }
            let d = (z >> 1) as i32 ^ -((z & 1) as i32);
            let from = before.get(j).copied().unwrap_or(last);
            last = from.wrapping_add_signed(d);
            *string.code_points.get_mut(j)? = last;
        }
        self.before = string;
        Some(string)
    }
}

/// One string of a [`StringSet`], held in place (no string is longer than
/// fifteen code points), so expanding a property allocates only what the
/// parser keeps.
#[derive(Clone, Copy)]
pub(crate) struct PackedString {
    code_points: [u32; 15],
    len: usize,
}

impl PackedString {
    /// Its code points.
    pub(crate) fn iter(&self) -> impl Iterator<Item = u32> + use<> {
        self.code_points.into_iter().take(self.len)
    }

    pub(crate) fn to_vec(self) -> Vec<u32> {
        self.code_points
            .get(..self.len)
            .unwrap_or_default()
            .to_vec()
    }
}
