//! Hidden classes.
//!
//! A **shape** names a key sequence: every object built by appending the same
//! property names, with the same attributes, in the same order shares one. That
//! makes "do these two objects lay their properties out identically?" a `u32`
//! compare instead of a per-object question.
//!
//! ## Why the engine needs it
//!
//! The inline caches key on object IDENTITY, so a site that sees more than a
//! handful of *instances* falls off a cliff even when every instance is the same
//! shape. Measured, reading one property from objects that differ only in how
//! many distinct instances the site has seen:
//!
//! ```text
//! receivers    1     8    12    16   1024
//! zipp      3.50  4.50  4.25 14.75  14.75 ns/read
//! node      1.25  0.75  0.75  0.75   0.75 ns/read
//! ```
//!
//! Flat for V8, a 3.5x step for us between 12 and 16. `json-large` walking
//! thousands of JSON objects that all share three or four shapes, and
//! `polymorphic-objects` reading 30,000 identically-built dictionaries, are
//! exactly the shape this punishes. A shape-keyed guard collapses all of them
//! onto one cache way.
//!
//! ## What this module deliberately does NOT do yet
//!
//! It does not change `ObjMap`'s layout. The keys, values and attributes stay in
//! the three parallel vectors they are in today, and the shape is maintained
//! ALONGSIDE them as a redundant summary. That is the whole point of landing it
//! this way round:
//!
//!   * the shape can be validated against the real data (a debug assertion
//!     compares `shape_len` to `keys.len()`, and the fill path resolves the slot
//!     through the shape and can be checked against `pos()`),
//!   * none of the ~49 sites that bake `vals.as_ptr()` into compiled code move,
//!   * and the measured win — the cliff above — is available immediately, while
//!     the allocation win from actually moving keys into the shape is worth far
//!     less (see `PERF_ROADMAP.md` B29/B37: removing allocations from these
//!     paths has measured ~0 four times).
//!
//! ## Transitions
//!
//! Shapes form a tree. The root is [`EMPTY`]; an edge adds one property. A
//! forward-transition cache on each node makes `{a:1, b:2}` reach the same shape
//! every time without hashing the whole sequence. Two objects sharing a shape
//! therefore share a key -> slot mapping BY CONSTRUCTION, which is what lets a
//! guard be a single integer compare.
//!
//! [`DICT`] is the escape hatch: an object whose layout stopped being describable
//! as "a sequence of appends" — a deleted key, a `defineProperty` that changed an
//! existing property's attributes, a `seal`/`freeze` — takes it, and shape-keyed
//! guards simply decline such objects. Correctness never depends on a shape being
//! present, only on it being *right* when it is.

use std::cell::RefCell;

/// The shape of an object whose layout is not describable by appends — it had a
/// key deleted, or an existing property's attributes redefined. Never equal to
/// another object's shape, so a guard against it can never match: `DICT == DICT`
/// is true numerically, which is why every guard site must reject this value
/// EXPLICITLY before comparing. [`ObjShape::guardable`] is the single place that
/// decision is made.
pub const DICT: u32 = 0;

/// The shape of an object with no own properties — `{}`, and the root of the
/// transition tree.
pub const EMPTY: u32 = 1;

/// Ceiling on the number of live shapes — and, more usefully, an automatic
/// opt-out for programs that shapes cannot help.
///
/// Shapes are permanent (the tree is never pruned, because a reissued id would
/// let a stale cache way match the wrong layout), so a bound is required
/// regardless. But the bound also does something better than prevent a leak: it
/// makes the mechanism SELF-TUNING. A program whose objects share layouts stays
/// far below it — `polymorphic-objects` builds 30,000 dictionaries and uses
/// 1,071 shapes — while a program whose keys are effectively unique blows
/// through it in one pass and every object thereafter is `DICT`, i.e. exactly
/// today's behaviour at the cost of one compare per append.
///
/// That second case is real, not hypothetical. `bench/real/json-large.js` builds
/// its tree with `obj[WORDS[ri(256)] + "_" + j]` — randomly-named keys — and so
/// wants 54,390 shapes for 18,604 objects. It can never benefit from a
/// shape-keyed guard, and before this cap it paid ~9% for the privilege of
/// finding that out on every append.
const SHAPE_MAX: usize = 4096;

/// Fan-out at which a node stops being scanned linearly and gains a hash index.
/// Most nodes never reach it; the ones that do are the roots of wide, data-shaped
/// trees, where the scan is what costs.
const FANOUT_INDEX_AT: usize = 8;

/// A 64-bit hash of one transition edge. FxHash-style: cheap, and only ever used
/// as a HINT — every hit is verified against the node's real key.
#[inline]
fn edge_hash(key: &str, bits: u8) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (bits as u64) << 56;
    for &b in key.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// One node of the transition tree: the shape reached by adding `key` to
/// `parent`.
struct Node {
    parent: u32,
    /// The property this edge adds. Empty for the root.
    key: Box<str>,
    /// Packed attribute bits of the added property — part of the shape's
    /// identity, because two objects whose `x` differs in enumerability do NOT
    /// have interchangeable layouts for a descriptor read.
    attr_bits: u8,
    /// Property count, which is also the slot index `key` occupies.
    len: u32,
    /// Forward transitions, so repeating a construction sequence finds the
    /// existing shape rather than minting one.
    ///
    /// A `Vec` scanned linearly, PLUS a hash index once the fan-out gets large.
    /// Both halves are answers to measurements:
    ///
    ///   * a `HashMap` alone needs an owned probe key (`Box::<str>::from(key)`
    ///     per lookup), which put a malloc on the property-append path;
    ///   * a `Vec` alone is fine for the straight-line case an object literal
    ///     produces, but real data is not straight lines — `json-large` builds a
    ///     tree with **max fan-out 313**, and scanning that per append cost it
    ///     ~9%.
    ///
    /// The index maps a hash of `(key, bits)` to a candidate node, VERIFIED
    /// against that node's real key so a collision is a miss rather than a wrong
    /// shape; a miss falls back to the scan, which is always authoritative.
    next: Vec<(Box<str>, u8, u32)>,
    index: Option<Box<std::collections::HashMap<u64, u32>>>,
}

struct Table {
    nodes: Vec<Node>,
}

impl Table {
    fn new() -> Table {
        // Index 0 is DICT and index 1 is EMPTY, so the ids above are usable as
        // plain constants.
        Table {
            nodes: vec![
                Node { parent: 0, key: "".into(), attr_bits: 0, len: 0, next: Vec::new(), index: None },
                Node { parent: 0, key: "".into(), attr_bits: 0, len: 0, next: Vec::new(), index: None },
            ],
        }
    }
}

thread_local! {
    /// The shape tree.
    ///
    /// Thread-local rather than a field on `Vm` because `ObjMap`'s mutators are
    /// `&mut self` with no access to the VM, and threading a table parameter
    /// through them would touch every call site — the exact churn this landing
    /// is designed to avoid. A shape is a pure description of a key sequence
    /// with no realm-specific content, so agents sharing a thread may share the
    /// tree safely; agents on different threads simply get their own.
    static TABLE: RefCell<Table> = RefCell::new(Table::new());
}

/// `ZIPP_NO_SHAPES=1` turns shape maintenance off — every object reports `DICT`
/// and every shape-keyed guard declines. Presence-checked, so `=0` also
/// disables. Exists so the standing gate can A/B the whole mechanism without a
/// rebuild, and so a regression can be bisected between "maintaining shapes" and
/// "the struct got bigger".
#[inline]
fn disabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static OFF: AtomicU8 = AtomicU8::new(2);
    match OFF.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_SHAPES").is_some() as u8;
            OFF.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Pack a property's descriptor flags into the byte that participates in shape
/// identity. The accessor's setter VALUE is deliberately absent: it is
/// per-object data, so two objects can share a shape while holding different
/// setters.
#[inline]
pub fn attr_bits(writable: bool, enumerable: bool, configurable: bool, accessor: bool) -> u8 {
    (writable as u8) | ((enumerable as u8) << 1) | ((configurable as u8) << 2) | ((accessor as u8) << 3)
}

/// The shape reached by appending `key` to `from`. [`DICT`] in, [`DICT`] out.
///
/// The caller must have established that `key` is ABSENT from `from` — every
/// append path in `ObjMap` probes with `pos()` first, and appending a duplicate
/// would produce a shape whose slot mapping disagrees with the object's.
pub fn add(from: u32, key: &str, bits: u8) -> u32 {
    if from == DICT || disabled() {
        return DICT;
    }
    TABLE.with(|t| {
        let mut t = t.borrow_mut();
        {
            let n = &t.nodes[from as usize];
            if let Some(ix) = &n.index {
                if let Some(&id) = ix.get(&edge_hash(key, bits)) {
                    // Verify: a hash collision must be a MISS, never a wrong
                    // shape. The scan below is the authority.
                    let c = &t.nodes[id as usize];
                    if c.attr_bits == bits && &*c.key == key {
                        return id;
                    }
                }
            }
            if let Some(&(_, _, id)) = n.next.iter().find(|(k, b, _)| *b == bits && &**k == key) {
                return id;
            }
        }
        if t.nodes.len() >= SHAPE_MAX {
            return DICT;
        }
        let len = t.nodes[from as usize].len + 1;
        let id = t.nodes.len() as u32;
        t.nodes.push(Node {
            parent: from,
            key: key.into(),
            attr_bits: bits,
            len,
            next: Vec::new(),
            index: None,
        });
        let h = edge_hash(key, bits);
        let n = &mut t.nodes[from as usize];
        n.next.push((key.into(), bits, id));
        match &mut n.index {
            Some(ix) => {
                ix.insert(h, id);
            }
            None if n.next.len() >= FANOUT_INDEX_AT => {
                let mut ix = std::collections::HashMap::with_capacity(n.next.len() * 2);
                for (k, b, nid) in &n.next {
                    ix.insert(edge_hash(k, *b), *nid);
                }
                n.index = Some(Box::new(ix));
            }
            None => {}
        }
        id
    })
}

/// Property count of `shape`. `0` for [`DICT`], which is also the honest answer
/// for "how many properties does this shape describe" — none, it describes none.
pub fn len(shape: u32) -> u32 {
    if shape == DICT {
        return 0;
    }
    TABLE.with(|t| t.borrow().nodes[shape as usize].len)
}

/// The slot `key` occupies in `shape`, or `None` if the shape does not carry it.
///
/// Walks the transition chain, so this is O(property count) — fine for an inline
/// cache FILL, which is the only caller. The hit path never comes here: it
/// compares the cached shape id and reuses the slot it recorded.
#[cfg(test)]
pub fn slot_of(shape: u32, key: &str) -> Option<u32> {
    if shape == DICT {
        return None;
    }
    TABLE.with(|t| {
        let t = t.borrow();
        let mut cur = shape;
        while cur != EMPTY && cur != DICT {
            let n = &t.nodes[cur as usize];
            if &*n.key == key {
                return Some(n.len - 1);
            }
            cur = n.parent;
        }
        None
    })
}

/// `(key, attr_bits)` per slot of `shape`, in insertion order.
///
/// The authoritative statement of what a shape CLAIMS about an object, used to
/// check it against what the object actually holds (`ObjMap::verify_shape`).
/// Walks the transition chain, so it is O(property count) and allocates — a
/// verifier, never a hot path.
pub fn describe(shape: u32) -> Vec<(Box<str>, u8)> {
    if shape == DICT {
        return Vec::new();
    }
    TABLE.with(|t| {
        let t = t.borrow();
        let mut out = Vec::new();
        let mut cur = shape;
        while cur != EMPTY && cur != DICT {
            let n = &t.nodes[cur as usize];
            out.push((n.key.clone(), n.attr_bits));
            cur = n.parent;
        }
        out.reverse();
        out
    })
}

/// Own property names of `shape`, in insertion order. Diagnostics and debug
/// assertions only — the hot paths read the object's own `keys`.
#[cfg(test)]
pub fn keys_of(shape: u32) -> Vec<String> {
    if shape == DICT {
        return Vec::new();
    }
    TABLE.with(|t| {
        let t = t.borrow();
        let mut out = Vec::new();
        let mut cur = shape;
        while cur != EMPTY && cur != DICT {
            let n = &t.nodes[cur as usize];
            out.push(n.key.to_string());
            cur = n.parent;
        }
        out.reverse();
        out
    })
}

/// Diagnostics for the transition tree: (nodes, max fan-out, total edges).
/// `ZIPP_SHAPESTATS=1` prints it at exit — fan-out is the number that decides
/// whether a linear scan of `next` is the right structure.
pub fn stats() -> (usize, usize, usize) {
    TABLE.with(|t| {
        let t = t.borrow();
        let mx = t.nodes.iter().map(|n| n.next.len()).max().unwrap_or(0);
        let tot = t.nodes.iter().map(|n| n.next.len()).sum();
        (t.nodes.len(), mx, tot)
    })
}

/// Number of shapes minted so far. Used by a test to prove that repeating a
/// construction sequence REUSES a shape rather than minting one per object —
/// which is the entire premise of the optimisation.
#[cfg(test)]
pub fn count() -> usize {
    TABLE.with(|t| t.borrow().nodes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: u8 = 0b1111; // a default data property: w+e+c, not accessor

    #[test]
    fn identical_construction_sequences_share_one_shape() {
        let a = add(add(EMPTY, "x", D), "y", D);
        let b = add(add(EMPTY, "x", D), "y", D);
        assert_eq!(a, b, "same keys in the same order must be the same shape");
        assert_eq!(len(a), 2);
        assert_eq!(keys_of(a), vec!["x", "y"]);
    }

    #[test]
    fn order_is_part_of_identity() {
        // `{x, y}` and `{y, x}` enumerate differently, so they cannot share a
        // slot mapping — a guard that conflated them would read the wrong value.
        let xy = add(add(EMPTY, "x", D), "y", D);
        let yx = add(add(EMPTY, "y", D), "x", D);
        assert_ne!(xy, yx);
        assert_eq!(slot_of(xy, "x"), Some(0));
        assert_eq!(slot_of(yx, "x"), Some(1));
    }

    #[test]
    fn attributes_are_part_of_identity() {
        let plain = add(EMPTY, "x", D);
        let non_enum = add(EMPTY, "x", attr_bits(true, false, true, false));
        assert_ne!(plain, non_enum, "enumerability is observable, so it is shape state");
    }

    #[test]
    fn slots_follow_insertion_order_and_missing_keys_answer_none() {
        let s = add(add(add(EMPTY, "a", D), "b", D), "c", D);
        assert_eq!(slot_of(s, "a"), Some(0));
        assert_eq!(slot_of(s, "b"), Some(1));
        assert_eq!(slot_of(s, "c"), Some(2));
        assert_eq!(slot_of(s, "nope"), None);
    }

    #[test]
    fn dict_is_absorbing_and_carries_nothing() {
        assert_eq!(add(DICT, "x", D), DICT);
        assert_eq!(len(DICT), 0);
        assert_eq!(slot_of(DICT, "x"), None);
    }

    #[test]
    fn repeating_a_sequence_does_not_mint_shapes() {
        // The premise of the whole optimisation: 1000 objects built the same way
        // share one shape, so a guard on it matches every one of them.
        let before = count();
        for _ in 0..1000 {
            add(add(add(EMPTY, "alpha", D), "beta", D), "gamma", D);
        }
        let minted = count() - before;
        assert!(minted <= 3, "expected at most 3 new nodes, minted {minted}");
    }
}
