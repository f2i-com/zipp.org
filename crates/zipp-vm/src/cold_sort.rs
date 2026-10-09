//! One shared sort for call sites that are not worth their own copy of it.
//!
//! Every `sort_by` / `sort_by_key` with its own closure, and every `sort()`
//! on its own element type, instantiates the standard library's whole sort:
//! about 5.7 KB of WebAssembly for a stable sort and 4.7 KB for an unstable
//! one. The JavaScript module carried 23 such copies, 143 KB, and most of
//! them order a handful of locale subtags or a table read once at start-up.
//!
//! The functions here sort a vector of POSITIONS through a `dyn` comparison,
//! so one instantiation serves every caller, then move the elements into that
//! order. The result is the stable order `slice::sort_by` gives: equal
//! elements keep their relative order because ties compare by position. The
//! work is still `O(n log n)` comparisons, which matters where the length is
//! the guest's to choose (the locale parser meters exactly that bound).
//!
//! Not for a hot path: every comparison is an indirect call and each sort
//! allocates. `Array.prototype.sort`, the TypedArray sorts, the collector's
//! pool sort and the integer-key orderings keep their own instantiations.

use core::cmp::Ordering;

/// Sort `order` (a permutation of positions) by `cmp`, ties by position.
#[inline(never)]
fn sort_positions(order: &mut [usize], cmp: &mut dyn FnMut(usize, usize) -> Ordering) {
    order.sort_unstable_by(|&a, &b| cmp(a, b).then(a.cmp(&b)));
}

/// `v.sort_by(cmp)`, stable, through the shared instantiation.
pub(crate) fn sort_by<T>(v: &mut Vec<T>, mut cmp: impl FnMut(&T, &T) -> Ordering) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut order: Vec<usize> = (0..n).collect();
    sort_positions(&mut order, &mut |a, b| cmp(&v[a], &v[b]));
    let mut slots: Vec<Option<T>> = v.drain(..).map(Some).collect();
    v.extend(order.into_iter().filter_map(|i| slots[i].take()));
}

/// `v.sort_by_key(key)`, stable, through the shared instantiation.
pub(crate) fn sort_by_key<T, K: Ord>(v: &mut Vec<T>, mut key: impl FnMut(&T) -> K) {
    sort_by(v, |a, b| key(a).cmp(&key(b)));
}

/// `v.sort()`, stable, through the shared instantiation.
pub(crate) fn sort<T: Ord>(v: &mut Vec<T>) {
    sort_by(v, T::cmp);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random values with many duplicates.
    fn values(n: usize, seed: u64) -> Vec<(u32, usize)> {
        let mut state = seed | 1;
        (0..n)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state % 7) as u32, i)
            })
            .collect()
    }

    #[test]
    fn agrees_with_the_standard_stable_sort() {
        for n in [0usize, 1, 2, 3, 7, 8, 31, 32, 33, 100, 1000] {
            for seed in 1..6u64 {
                let input = values(n, seed);
                let mut want = input.clone();
                want.sort_by(|a, b| a.0.cmp(&b.0));
                let mut by = input.clone();
                sort_by(&mut by, |a, b| a.0.cmp(&b.0));
                assert_eq!(by, want, "sort_by n={n} seed={seed}");
                let mut by_key = input.clone();
                sort_by_key(&mut by_key, |item| item.0);
                assert_eq!(by_key, want, "sort_by_key n={n} seed={seed}");
                let mut plain = input.clone();
                let mut plain_want = input;
                plain_want.sort();
                sort(&mut plain);
                assert_eq!(plain, plain_want, "sort n={n} seed={seed}");
            }
        }
    }

    #[test]
    fn keeps_every_element_exactly_once() {
        let mut v: Vec<String> = ["delta", "alpha", "charlie", "alpha", "bravo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        sort(&mut v);
        assert_eq!(v, ["alpha", "alpha", "bravo", "charlie", "delta"]);
        // Descending, and already ordered input.
        sort_by(&mut v, |a, b| b.cmp(a));
        assert_eq!(v, ["delta", "charlie", "bravo", "alpha", "alpha"]);
        sort_by(&mut v, |a, b| b.cmp(a));
        assert_eq!(v, ["delta", "charlie", "bravo", "alpha", "alpha"]);
    }
}
