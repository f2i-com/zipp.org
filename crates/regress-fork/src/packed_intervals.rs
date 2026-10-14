//! Lossless storage for Unicode property tables. Decode only when the regex
//! parser constructs its owned CodePointSet (which previously copied a slice).
//! No decoding or extra indirection is added to regex matching.
use crate::codepointset::{CodePointSet, Interval};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

pub(crate) struct PackedIntervals {
    pub(crate) bytes: &'static [u8],
    pub(crate) count: usize,
    // A table that is the union of others (a General_Category group), or the
    // complement of that union (Assigned), is stored as those tables: `bytes`
    // is empty and `count` is the number of intervals it has.
    pub(crate) union_of: &'static [&'static PackedIntervals],
    pub(crate) complement: bool,
    /// A derived table's intervals, built the first time a regex uses it:
    /// merging the parts costs more than decoding one packed table did, and
    /// a pattern such as `\p{L}` is compiled again for every new RegExp.
    #[cfg(feature = "std")]
    pub(crate) cache: Option<&'static std::sync::OnceLock<Vec<Interval>>>,
    #[cfg(test)]
    pub(crate) original: &'static [Interval],
}

impl PackedIntervals {
    #[cfg(feature = "prohibit-unsafe")]
    pub(crate) fn len(&self) -> usize {
        self.count
    }

    pub(crate) fn to_vec(&self) -> Vec<Interval> {
        if self.union_of.is_empty() {
            let mut out = Vec::with_capacity(self.count);
            self.decode_into(&mut out);
            return out;
        }
        #[cfg(feature = "std")]
        if let Some(cache) = self.cache {
            return cache.get_or_init(|| self.build_union()).clone();
        }
        self.build_union()
    }

    /// A derived table's intervals, built from its parts.
    fn build_union(&self) -> Vec<Interval> {
        // The parts are disjoint and each sorted: decode them all, sort once
        // and join touching neighbours, the form the generator writes (and
        // CodePointSet keeps).
        let mut all = Vec::new();
        for part in self.union_of {
            part.decode_into(&mut all);
        }
        all.sort_unstable_by_key(|iv| iv.first);
        let mut out: Vec<Interval> = Vec::with_capacity(all.len());
        for iv in all {
            match out.last_mut() {
                Some(last) if iv.first <= last.last + 1 => last.last = last.last.max(iv.last),
                _ => out.push(iv),
            }
        }
        if self.complement {
            out = CodePointSet::from_sorted_disjoint_intervals(out)
                .inverted()
                .intervals()
                .to_vec();
        }
        debug_assert_eq!(out.len(), self.count);
        out
    }

    /// Append this table's intervals (a derived table's are its parts').
    fn decode_into(&self, out: &mut Vec<Interval>) {
        if !self.union_of.is_empty() {
            out.extend(self.to_vec());
            return;
        }
        let mut bytes = self.bytes.iter().copied();
        let mut next = 0;
        for _ in 0..self.count {
            let first = next + read_varint(&mut bytes);
            let last = first + read_varint(&mut bytes);
            out.push(Interval::new(first, last));
            next = last + 1;
        }
        debug_assert!(bytes.next().is_none());
    }

    /// Whether `cp` is in this table: a binary search over its intervals,
    /// decoded into `cache` the first time. The identifier tables are asked
    /// once per character of an identifier (the engine's lexer) or a group
    /// name; packed like every other table, they decode once per process.
    #[cfg(feature = "std")]
    pub(crate) fn contains_identifier(
        &self,
        cache: &std::sync::OnceLock<Vec<Interval>>,
        cp: u32,
    ) -> bool {
        crate::codepointset::interval_contains(cache.get_or_init(|| self.to_vec()), cp)
    }

    /// Without `std` there is no cache: decode on every lookup.
    #[cfg(not(feature = "std"))]
    pub(crate) fn contains_identifier(&self, cp: u32) -> bool {
        crate::codepointset::interval_contains(&self.to_vec(), cp)
    }
}

fn read_varint(bytes: &mut impl Iterator<Item = u8>) -> u32 {
    // Compile-time validation bounds every gap and width to 0x10ffff, so a
    // value occupies at most three bytes. Avoid a variable-shift loop while
    // expanding Unicode properties; matching itself does not decode tables.
    let first = bytes.next().expect("truncated Unicode table");
    if first < 128 {
        return u32::from(first);
    }
    let second = bytes.next().expect("truncated Unicode table");
    let value = u32::from(first & 127) | (u32::from(second & 127) << 7);
    if second < 128 {
        return value;
    }
    let third = bytes.next().expect("truncated Unicode table");
    debug_assert!(third < 128);
    value | (u32::from(third) << 14)
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
            union_of: &[],
            complement: false,
            #[cfg(feature = "std")]
            cache: None,
            #[cfg(test)]
            original: INPUT,
        }
    }};
}
pub(crate) use packed_intervals;

// A table stored as the union of `parts` (complemented if `complement`); the
// literals stay for the interval count and the round-trip test, and only
// const evaluation reads them.
macro_rules! derived_intervals {
    ([$($part:expr),* $(,)?], $complement:expr, $($iv:expr),* $(,)?) => {{
        const INPUT: &[crate::codepointset::Interval] = &[$($iv),*];
        #[cfg(feature = "std")]
        static CACHE: std::sync::OnceLock<Vec<crate::codepointset::Interval>> =
            std::sync::OnceLock::new();
        crate::packed_intervals::PackedIntervals {
            bytes: &[],
            count: INPUT.len(),
            union_of: &[$($part),*],
            complement: $complement,
            #[cfg(feature = "std")]
            cache: Some(&CACHE),
            #[cfg(test)]
            original: INPUT,
        }
    }};
}
pub(crate) use derived_intervals;

#[cfg(test)]
mod tests {
    use super::read_varint;

    #[test]
    fn decode_width_boundaries_without_consuming_the_next_value() {
        let encoded = [0, 127, 128, 1, 255, 127, 128, 128, 1, 255, 255, 67, 1];
        let mut bytes = encoded.into_iter();
        for value in [0, 127, 128, 16383, 16384, 0x10ffff, 1] {
            assert_eq!(read_varint(&mut bytes), value);
        }
        assert!(bytes.next().is_none());
    }
}
