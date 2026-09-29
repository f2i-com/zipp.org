//! Lossless storage for Unicode property tables. Decode only when the regex
//! parser constructs its owned CodePointSet (which previously copied a slice).
//! No decoding or extra indirection is added to regex matching.
use crate::codepointset::Interval;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

pub(crate) struct PackedIntervals {
    pub(crate) bytes: &'static [u8],
    pub(crate) count: usize,
    // Identifier predicates need binary search for each group-name character.
    // Keep just those two tables directly indexed; packing is for expansion.
    pub(crate) direct: Option<&'static [Interval]>,
    #[cfg(test)]
    pub(crate) original: &'static [Interval],
}

impl PackedIntervals {
    #[cfg(feature = "prohibit-unsafe")]
    pub(crate) fn len(&self) -> usize {
        self.count
    }

    pub(crate) fn to_vec(&self) -> Vec<Interval> {
        if let Some(direct) = self.direct {
            return direct.to_vec();
        }
        let mut out = Vec::with_capacity(self.count);
        let mut bytes = self.bytes.iter().copied();
        let mut next = 0;
        for _ in 0..self.count {
            let first = next + read_varint(&mut bytes);
            let last = first + read_varint(&mut bytes);
            out.push(Interval::new(first, last));
            next = last + 1;
        }
        debug_assert!(bytes.next().is_none());
        out
    }

    pub(crate) fn contains_identifier(&self, cp: u32) -> bool {
        crate::codepointset::interval_contains(self.direct.expect("identifier table"), cp)
    }
}

fn read_varint(bytes: &mut impl Iterator<Item = u8>) -> u32 {
    let mut value = 0;
    let mut shift = 0;
    loop {
        // Only compile-time-validated internal tables reach this decoder.
        let byte = bytes.next().expect("truncated Unicode table");
        value |= u32::from(byte & 127) << shift;
        if byte < 128 {
            return value;
        }
        shift += 7;
    }
}

const fn varint_len(mut n: u32) -> usize {
    let mut len = 1;
    while n >= 128 {
        n >>= 7;
        len += 1;
    }
    len
}

pub(crate) const fn packed_len(intervals: &[Interval]) -> usize {
    let mut len = 0;
    let mut next = 0;
    let mut i = 0;
    while i < intervals.len() {
        let iv = &intervals[i];
        assert!(iv.first >= next && iv.last >= iv.first && iv.last <= 0x10ffff);
        len += varint_len(iv.first - next) + varint_len(iv.last - iv.first);
        next = iv.last + 1;
        i += 1;
    }
    len
}

pub(crate) const fn pack<const N: usize>(intervals: &[Interval]) -> [u8; N] {
    let mut out = [0; N];
    let mut pos = 0;
    let mut next = 0;
    let mut i = 0;
    while i < intervals.len() {
        let iv = &intervals[i];
        let values = [iv.first - next, iv.last - iv.first];
        let mut j = 0;
        while j < 2 {
            let mut n = values[j];
            while n >= 128 {
                out[pos] = (n as u8 & 127) | 128;
                pos += 1;
                n >>= 7;
            }
            out[pos] = n as u8;
            pos += 1;
            j += 1;
        }
        next = iv.last + 1;
        i += 1;
    }
    assert!(pos == N);
    out
}

// Keep the authoritative generated Interval literals readable. Rust evaluates
// the encoder at compile time; only the packed bytes enter shipped artifacts.
macro_rules! packed_intervals {
    ($($iv:expr),* $(,)?) => {{
        const INPUT: &[crate::codepointset::Interval] = &[$($iv),*];
        crate::packed_intervals::PackedIntervals {
            bytes: &crate::packed_intervals::pack::<{
                crate::packed_intervals::packed_len(INPUT)
            }>(INPUT),
            count: INPUT.len(),
            direct: None,
            #[cfg(test)]
            original: INPUT,
        }
    }};
}
pub(crate) use packed_intervals;

macro_rules! direct_intervals {
    ($($iv:expr),* $(,)?) => {{
        const INPUT: &[crate::codepointset::Interval] = &[$($iv),*];
        crate::packed_intervals::PackedIntervals {
            bytes: &[], count: INPUT.len(), direct: Some(INPUT),
            #[cfg(test)]
            original: INPUT,
        }
    }};
}
pub(crate) use direct_intervals;
